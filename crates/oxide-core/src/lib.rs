//! The Oxide engine.
//!
//! A single async task owns the conversation, the active harness, and the
//! provider, and exposes itself purely through an [`Op`] inbox and an [`Event`]
//! outbox. Any frontend — TUI, GUI, headless, RPC — is just a pair of channel
//! ends. This decoupling is what lets the same engine power both a terminal and
//! a desktop app, and lets behavior be swapped via harnesses at runtime.
//!
//! ```text
//!   frontend --Op-->  [ Engine task ]  --Event--> frontend
//!                          |
//!                  Harness (prompt+tools)
//!                          |
//!                  Provider (streaming)        ToolRouter --> sandbox (Fase 2)
//! ```

pub mod automation;
mod browser;
mod commands;
mod context;
pub mod db;
mod embed;
mod git_tools;
pub mod hooks;
mod index;
pub mod memory;
mod ptc;
mod sandbox;
mod store;
mod tools;
pub use tools::{Routed, ToolRouter};

use oxide_config::{Config, McpEnvVar, McpServerConfig};
use oxide_design::{
    build_design_token_contract, build_patch_instruction, extract_source_tokens,
    parse_design_markdown, review_design_selection, DesignPatchProposal, DesignReviewInput,
};

/// A shallow file-tree of the workspace, injected into the system prompt so the
/// agent sees the project's real structure from the first message (and doesn't
/// "forget the codebase" in a fresh tab or invent a standalone solution).
fn project_map(ws: &Path) -> String {
    const SKIP: &[&str] = &[
        ".git",
        "node_modules",
        "target",
        "dist",
        ".next",
        ".oxide",
        "vendor",
        "build",
        ".venv",
        "__pycache__",
        ".cache",
        "out",
        ".turbo",
        ".idea",
        ".vscode",
    ];
    fn walk(dir: &Path, prefix: &str, depth: usize, count: &mut usize, out: &mut String) {
        if depth > 2 || *count > 120 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<(bool, String, std::path::PathBuf)> = rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with('.') && name != ".env.example" && name != ".github" {
                    return None;
                }
                if SKIP.contains(&name.as_str()) {
                    return None;
                }
                let p = e.path();
                Some((p.is_dir(), name, p))
            })
            .collect();
        // Dirs first, then files; alphabetical within each.
        entries.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let per_dir = if depth == 0 { 40 } else { 16 };
        for (is_dir, name, p) in entries.into_iter().take(per_dir) {
            if *count > 120 {
                out.push_str(&format!("{prefix}…\n"));
                return;
            }
            *count += 1;
            if is_dir {
                out.push_str(&format!("{prefix}{name}/\n"));
                let child = format!("{prefix}  ");
                walk(&p, &child, depth + 1, count, out);
            } else {
                out.push_str(&format!("{prefix}{name}\n"));
            }
        }
    }
    let mut out = String::new();
    let mut count = 0;
    walk(ws, "", 0, &mut count, &mut out);
    out
}

/// Best-effort detect the project's stack so the agent builds with the right
/// framework instead of inventing a standalone solution.
fn detect_stack(ws: &Path) -> Option<String> {
    if let Ok(pkg) = std::fs::read_to_string(ws.join("package.json")) {
        let p = pkg.to_lowercase();
        let fw = if p.contains("\"next\"") {
            "Next.js (React)"
        } else if p.contains("nuxt") {
            "Nuxt (Vue)"
        } else if p.contains("@remix-run") {
            "Remix (React)"
        } else if p.contains("svelte") {
            "Svelte/SvelteKit"
        } else if p.contains("\"vue\"") {
            "Vue"
        } else if p.contains("\"react\"") {
            "React"
        } else if p.contains("\"vite\"") {
            "Vite"
        } else if p.contains("\"express\"") {
            "Node/Express"
        } else {
            "Node.js"
        };
        let extra = if p.contains("supabase") {
            " + Supabase"
        } else {
            ""
        };
        return Some(format!("{fw}{extra}"));
    }
    if ws.join("Cargo.toml").exists() {
        return Some("Rust (Cargo)".into());
    }
    if ws.join("go.mod").exists() {
        return Some("Go".into());
    }
    if ws.join("pyproject.toml").exists() || ws.join("requirements.txt").exists() {
        return Some("Python".into());
    }
    if ws.join("pom.xml").exists() || ws.join("build.gradle").exists() {
        return Some("Java/JVM".into());
    }
    if ws.join("Gemfile").exists() {
        return Some("Ruby".into());
    }
    None
}

/// A browser-like HTTP client for web tools.
fn web_client() -> Option<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36")
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .ok()
}

/// Decode the few HTML entities that matter for plain-text output.
fn html_decode(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

/// Strip HTML tags from a fragment, leaving text.
fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = (depth - 1).max(0),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Extract text inside `marker … > BODY <end>` from `hay`, tags stripped.
fn extract_between(hay: &str, marker: &str, end: &str) -> Option<String> {
    let i = hay.find(marker)?;
    let after = &hay[i + marker.len()..];
    let gt = after.find('>')?;
    let body = &after[gt + 1..];
    let e = body.find(end)?;
    let t = html_decode(&strip_tags(&body[..e]));
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

/// Web search: prefer Exa (hosted MCP, keyless, returns page content), fall
/// back to Brave HTML scraping.
async fn web_search(query: &str) -> (String, bool) {
    let q = query.trim();
    if q.is_empty() {
        return ("web_search: missing 'query'".into(), false);
    }
    // Exa connects via MCP (initialize + list_tools + call_tool = up to 3×30s).
    // Cap the whole thing so a slow/down server can't freeze the engine.
    let exa = tokio::time::timeout(std::time::Duration::from_secs(20), exa_search(q)).await;
    match exa {
        Ok(Ok(text)) if !text.trim().is_empty() => (text, true),
        _ => brave_search(q).await,
    }
}

/// Web search via Exa's public MCP endpoint (no API key).
async fn exa_search(query: &str) -> anyhow::Result<String> {
    let client = McpClient::connect_http("exa", "https://mcp.exa.ai/mcp").await?;
    let tool = client
        .list_tools()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|t| t.name)
        .find(|n| n.contains("web_search") || n.contains("search"))
        .unwrap_or_else(|| "web_search_exa".to_string());
    let (text, ok) = client
        .call_tool(
            &tool,
            &serde_json::json!({ "query": query, "numResults": 6 }),
        )
        .await?;
    if !ok {
        anyhow::bail!("exa error");
    }
    Ok(text.chars().take(9000).collect())
}

/// Web search via Brave HTML (fallback). Returns ranked `title / url / snippet`.
async fn brave_search(q: &str) -> (String, bool) {
    let Some(client) = web_client() else {
        return ("web_search: client error".into(), false);
    };
    let url = format!(
        "https://search.brave.com/search?q={}&source=web",
        q.replace(' ', "+")
    );
    let html = match client.get(&url).send().await {
        Ok(r) => match r.text().await {
            Ok(t) => t,
            Err(e) => return (format!("web_search: {e}"), false),
        },
        Err(e) => return (format!("web_search: {e}"), false),
    };
    let mut out = String::new();
    let mut n = 0;
    let mut rest = html.as_str();
    // Each organic web result carries `data-type="web"`.
    while let Some(i) = rest.find("data-type=\"web\"") {
        rest = &rest[i + 15..];
        let block_end = rest
            .find("data-type=\"")
            .unwrap_or(rest.len().min(6000))
            .min(6000);
        let block = &rest[..block_end];
        let url = block.find("href=\"https").and_then(|h| {
            let a = &block[h + 6..];
            a.find('"').map(|e| a[..e].to_string())
        });
        let Some(url) = url else { continue };
        if url.contains("search.brave.com") {
            continue;
        }
        let title = extract_between(block, "snippet-title", "</")
            .or_else(|| extract_between(block, "title=\"", "\""))
            .unwrap_or_else(|| url.clone());
        n += 1;
        out.push_str(&format!("{n}. {title}\n   {url}\n"));
        if let Some(desc) = extract_between(block, "snippet-description", "</") {
            out.push_str(&format!(
                "   {}\n",
                desc.chars().take(240).collect::<String>()
            ));
        }
        if n >= 8 {
            break;
        }
    }
    if out.is_empty() {
        return (format!("No results for '{q}'."), true);
    }
    (out, true)
}

/// Fetch a URL and return its readable text (scripts/styles/tags stripped).
async fn fetch_url(url: &str) -> (String, bool) {
    let u = url.trim();
    if !u.starts_with("http") {
        return ("fetch_url: url must start with http(s)".into(), false);
    }
    let Some(client) = web_client() else {
        return ("fetch_url: client error".into(), false);
    };
    let html = match client.get(u).send().await {
        Ok(r) => match r.text().await {
            Ok(t) => t,
            Err(e) => return (format!("fetch_url: {e}"), false),
        },
        Err(e) => return (format!("fetch_url: {e}"), false),
    };
    // Cap raw HTML before processing: large pages are expensive and the model
    // only needs the first ~200 KB to get the gist.
    let html = if html.len() > 200_000 {
        // Slice on a char boundary — a raw byte cut at 200_000 panics if it
        // lands inside a multi-byte UTF-8 sequence (non-ASCII page), which would
        // unwind the whole turn (and, unsupervised, the engine task) silently.
        let end = (0..=200_000)
            .rev()
            .find(|&i| html.is_char_boundary(i))
            .unwrap_or(0);
        &html[..end]
    } else {
        &html
    };
    // Drop <script>/<style> blocks with linear string scanning (not char-loop).
    let mut cleaned = String::with_capacity(html.len().min(200_000));
    let lower = html.to_ascii_lowercase();
    let mut pos = 0usize;
    while pos < html.len() {
        let rest = &lower[pos..];
        if let Some(rel) = rest.find("<script").or_else(|| rest.find("<style")) {
            let abs = pos + rel;
            // Flush everything before this tag.
            cleaned.push_str(&html[pos..abs]);
            let close = if lower[abs..].starts_with("<script") {
                "</script>"
            } else {
                "</style>"
            };
            pos = match lower[abs..].find(close) {
                Some(e) => abs + e + close.len(),
                None => html.len(), // unclosed tag — skip rest
            };
        } else {
            cleaned.push_str(&html[pos..]);
            break;
        }
    }
    let text = html_decode(&strip_tags(&cleaned));
    let capped: String = text.chars().take(15_000).collect();
    if capped.trim().is_empty() {
        return (format!("(no readable text at {u})"), true);
    }
    (capped, true)
}

/// Discover MCP servers configured in Codex (`~/.codex/config.toml`) and Claude
/// desktop / Claude Code (`mcpServers` JSON), so Oxide can reuse them.
pub fn discover_external_mcp() -> Vec<McpServerConfig> {
    let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    discover_external_mcp_for_workspace(&workspace)
}

/// Workspace-aware MCP discovery. This includes repo-scoped Codex config at
/// `.codex/config.toml` in addition to user/global config files.
pub fn discover_external_mcp_for_workspace(workspace: &Path) -> Vec<McpServerConfig> {
    // Cache for 60s — engines respawn on every tab switch and ~/.claude.json
    // can be megabytes; no need to re-read + re-parse it each time.
    static CACHE: std::sync::OnceLock<ExternalMcpCache> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let cache_key = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf())
        .display()
        .to_string();
    if let Ok(g) = cache.lock() {
        if let Some((t, v)) = g.get(&cache_key) {
            if t.elapsed() < std::time::Duration::from_secs(60) {
                return v.clone();
            }
        }
    }
    let mut out: Vec<McpServerConfig> = Vec::new();
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        return out;
    };
    let mut push = |server: McpServerConfig| {
        if server.name.trim().is_empty() || (server.command.is_empty() && server.url.is_empty()) {
            return;
        }
        if let Some(pos) = out.iter().position(|existing| existing.name == server.name) {
            out[pos] = server;
        } else {
            out.push(server);
        }
    };
    for (path, source) in [
        (
            home.join(".codex/config.toml"),
            "Codex user config".to_string(),
        ),
        (
            workspace.join(".codex/config.toml"),
            "Codex project config".to_string(),
        ),
    ] {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(v) = toml::from_str::<toml::Value>(&text) {
                collect_codex_mcp(&v, &source, &mut push);
            }
        }
    }
    // Claude desktop + Claude Code: mcpServers { NAME: { command, args, url } }
    for p in [
        home.join("Library/Application Support/Claude/claude_desktop_config.json"),
        home.join(".claude.json"),
    ] {
        if let Ok(text) = std::fs::read_to_string(&p) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                if p.ends_with(".claude.json") {
                    collect_claude_code_mcp(&v, workspace, &mut push);
                } else {
                    collect_json_mcp_servers(&v, "Claude Desktop", &mut push);
                }
            }
        }
    }
    if let Ok(mut g) = cache.lock() {
        g.insert(cache_key, (std::time::Instant::now(), out.clone()));
    }
    out
}

fn collect_codex_mcp(value: &toml::Value, source: &str, push: &mut impl FnMut(McpServerConfig)) {
    if let Some(tbl) = value.get("mcp_servers").and_then(|x| x.as_table()) {
        for (name, entry) in tbl {
            push(toml_mcp_server(name, entry, source));
        }
    }
    if let Some(plugins) = value.get("plugins").and_then(|x| x.as_table()) {
        for (plugin, cfg) in plugins {
            if let Some(servers) = cfg.get("mcp_servers").and_then(|x| x.as_table()) {
                for (name, entry) in servers {
                    push(toml_mcp_server(
                        name,
                        entry,
                        &format!("Codex plugin {plugin}"),
                    ));
                }
            }
        }
    }
}

fn toml_mcp_server(name: &str, entry: &toml::Value, source: &str) -> McpServerConfig {
    let s = |key: &str| {
        entry
            .get(key)
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string()
    };
    McpServerConfig {
        name: name.to_string(),
        command: s("command"),
        args: toml_string_array(entry.get("args")),
        url: s("url"),
        enabled: entry
            .get("enabled")
            .and_then(|x| x.as_bool())
            .unwrap_or(true),
        source: source.to_string(),
        external_ref: false,
        cwd: s("cwd"),
        env: toml_string_map(entry.get("env")),
        env_vars: toml_env_vars(entry.get("env_vars")),
        bearer_token_env_var: s("bearer_token_env_var"),
        http_headers: toml_string_map(entry.get("http_headers")),
        env_http_headers: toml_string_map(entry.get("env_http_headers")),
        startup_timeout_sec: toml_u64(entry.get("startup_timeout_sec")),
        tool_timeout_sec: toml_u64(entry.get("tool_timeout_sec")),
        enabled_tools: toml_string_array(entry.get("enabled_tools")),
        disabled_tools: toml_string_array(entry.get("disabled_tools")),
        required: entry
            .get("required")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
    }
}

fn collect_json_mcp_servers(
    value: &serde_json::Value,
    source: &str,
    push: &mut impl FnMut(McpServerConfig),
) {
    if let Some(obj) = value.get("mcpServers").and_then(|x| x.as_object()) {
        for (name, entry) in obj {
            push(json_mcp_server(name, entry, source));
        }
    }
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                collect_json_mcp_servers(item, source, push);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, item) in map {
                if key != "mcpServers" {
                    collect_json_mcp_servers(item, source, push);
                }
            }
        }
        _ => {}
    }
}

fn collect_claude_code_mcp(
    value: &serde_json::Value,
    workspace: &Path,
    push: &mut impl FnMut(McpServerConfig),
) {
    if let Some(obj) = value.get("mcpServers").and_then(|x| x.as_object()) {
        for (name, entry) in obj {
            push(json_mcp_server(name, entry, "Claude Code"));
        }
    }
    let workspace_key = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf())
        .display()
        .to_string();
    let Some(projects) = value.get("projects").and_then(|x| x.as_object()) else {
        return;
    };
    for (project_path, project_cfg) in projects {
        let project_key = std::path::PathBuf::from(project_path)
            .canonicalize()
            .unwrap_or_else(|_| std::path::PathBuf::from(project_path))
            .display()
            .to_string();
        if project_key == workspace_key {
            collect_json_mcp_servers(project_cfg, "Claude Code project", push);
        }
    }
}

fn json_mcp_server(name: &str, entry: &serde_json::Value, source: &str) -> McpServerConfig {
    let s = |key: &str| {
        entry
            .get(key)
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string()
    };
    McpServerConfig {
        name: name.to_string(),
        command: s("command"),
        args: json_string_array(entry.get("args")),
        url: s("url"),
        enabled: entry
            .get("enabled")
            .and_then(|x| x.as_bool())
            .unwrap_or(true),
        source: source.to_string(),
        external_ref: false,
        cwd: s("cwd"),
        env: json_string_map(entry.get("env")),
        env_vars: json_env_vars(entry.get("env_vars")),
        bearer_token_env_var: s("bearer_token_env_var"),
        http_headers: json_string_map(entry.get("http_headers")),
        env_http_headers: json_string_map(entry.get("env_http_headers")),
        startup_timeout_sec: json_u64(entry.get("startup_timeout_sec")),
        tool_timeout_sec: json_u64(entry.get("tool_timeout_sec")),
        enabled_tools: json_string_array(entry.get("enabled_tools")),
        disabled_tools: json_string_array(entry.get("disabled_tools")),
        required: entry
            .get("required")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
    }
}

fn toml_string_array(value: Option<&toml::Value>) -> Vec<String> {
    value
        .and_then(|x| x.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn json_string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(|x| x.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn toml_string_map(value: Option<&toml::Value>) -> BTreeMap<String, String> {
    value
        .and_then(|x| x.as_table())
        .map(|tbl| {
            tbl.iter()
                .filter_map(|(key, value)| value.as_str().map(|v| (key.clone(), v.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

fn json_string_map(value: Option<&serde_json::Value>) -> BTreeMap<String, String> {
    value
        .and_then(|x| x.as_object())
        .map(|tbl| {
            tbl.iter()
                .filter_map(|(key, value)| value.as_str().map(|v| (key.clone(), v.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

fn toml_env_vars(value: Option<&toml::Value>) -> Vec<McpEnvVar> {
    value
        .and_then(|x| x.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    if let Some(name) = item.as_str() {
                        Some(McpEnvVar::Name(name.to_string()))
                    } else {
                        item.as_table().and_then(|tbl| {
                            let name = tbl.get("name")?.as_str()?.to_string();
                            let source = tbl
                                .get("source")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            Some(McpEnvVar::Named { name, source })
                        })
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn json_env_vars(value: Option<&serde_json::Value>) -> Vec<McpEnvVar> {
    value
        .and_then(|x| x.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    if let Some(name) = item.as_str() {
                        Some(McpEnvVar::Name(name.to_string()))
                    } else {
                        item.as_object().and_then(|obj| {
                            let name = obj.get("name")?.as_str()?.to_string();
                            let source = obj
                                .get("source")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            Some(McpEnvVar::Named { name, source })
                        })
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn toml_u64(value: Option<&toml::Value>) -> Option<u64> {
    value
        .and_then(|x| x.as_integer())
        .and_then(|n| u64::try_from(n).ok())
        .filter(|n| *n > 0)
}

fn json_u64(value: Option<&serde_json::Value>) -> Option<u64> {
    value.and_then(|x| x.as_u64()).filter(|n| *n > 0)
}

fn duration_secs(value: Option<u64>, default_secs: u64) -> std::time::Duration {
    std::time::Duration::from_secs(value.filter(|secs| *secs > 0).unwrap_or(default_secs))
}

fn mcp_http_options(server: &McpServerConfig) -> HttpOptions {
    let has_auth_header = server
        .http_headers
        .keys()
        .chain(server.env_http_headers.keys())
        .any(|key| key.eq_ignore_ascii_case("authorization"));
    let bearer_token = if server.url.trim().is_empty()
        || !server.bearer_token_env_var.trim().is_empty()
        || has_auth_header
    {
        String::new()
    } else {
        mcp_keychain_bearer_token(&server.name, &server.url).unwrap_or_default()
    };
    HttpOptions {
        bearer_token,
        bearer_token_env_var: server.bearer_token_env_var.clone(),
        headers: server.http_headers.clone(),
        env_headers: server.env_http_headers.clone(),
        request_timeout: duration_secs(server.tool_timeout_sec, 30),
    }
}

fn mcp_keychain_bearer_token(server: &str, url: &str) -> Option<String> {
    if server.trim().is_empty() || url.trim().is_empty() {
        return None;
    }
    #[cfg(target_os = "macos")]
    {
        mcp_keychain_bearer_token_macos(server, url)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (server, url);
        None
    }
}

#[cfg(target_os = "macos")]
fn mcp_keychain_bearer_token_macos(server: &str, url: &str) -> Option<String> {
    mcp_keychain_bearer_token_macos_uncached(server, url)
}

#[cfg(target_os = "macos")]
fn mcp_keychain_bearer_token_macos_uncached(server: &str, url: &str) -> Option<String> {
    for account in keychain_candidate_accounts() {
        if let Some(secret) = keychain_password("Claude Code-credentials", &account) {
            if let Some(token) = claude_mcp_oauth_token_from_json(&secret, server) {
                return Some(token);
            }
        }
    }
    for account in keychain_accounts_for_service("Codex MCP Credentials", server) {
        if let Some(secret) = keychain_password("Codex MCP Credentials", &account) {
            if let Some(token) = codex_mcp_credential_token_from_json(&secret, server, url) {
                return Some(token);
            }
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn keychain_candidate_accounts() -> Vec<String> {
    let mut accounts: Vec<String> = Vec::new();
    if let Ok(user) = std::env::var("USER") {
        if !user.trim().is_empty() {
            accounts.push(user);
        }
    }
    if let Ok(output) = std::process::Command::new("whoami").output() {
        if output.status.success() {
            let user = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !user.is_empty() {
                accounts.push(user);
            }
        }
    }
    accounts.sort();
    accounts.dedup();
    accounts
}

#[cfg(target_os = "macos")]
fn keychain_password(service: &str, account: &str) -> Option<String> {
    let output = std::process::Command::new("security")
        .args(["find-generic-password", "-s", service, "-a", account, "-w"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let secret = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!secret.is_empty()).then_some(secret)
}

#[cfg(target_os = "macos")]
fn keychain_accounts_for_service(service: &str, server: &str) -> Vec<String> {
    let output = std::process::Command::new("security")
        .arg("dump-keychain")
        .output()
        .ok();
    let Some(output) = output.filter(|out| out.status.success()) else {
        return Vec::new();
    };
    let dump = String::from_utf8_lossy(&output.stdout);
    keychain_accounts_for_service_from_dump(&dump, service, server)
}

fn claude_mcp_oauth_token_from_json(text: &str, server: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let oauth = value.get("mcpOAuth").and_then(|v| v.as_object())?;
    let prefix = format!("{server}|");
    oauth.iter().find_map(|(key, entry)| {
        if key != server && !key.starts_with(&prefix) {
            return None;
        }
        entry
            .get("accessToken")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(str::to_string)
    })
}

fn codex_mcp_credential_token_from_json(text: &str, server: &str, url: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let server_matches = value
        .get("server_name")
        .and_then(|v| v.as_str())
        .map(|name| name == server)
        .unwrap_or(true);
    let url_matches = value
        .get("url")
        .and_then(|v| v.as_str())
        .map(|saved| saved.trim_end_matches('/') == url.trim_end_matches('/'))
        .unwrap_or(true);
    if !server_matches || !url_matches {
        return None;
    }
    value
        .get("token_response")
        .and_then(|v| v.get("access_token"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
}

fn keychain_accounts_for_service_from_dump(dump: &str, service: &str, server: &str) -> Vec<String> {
    let mut accounts = Vec::new();
    let mut current_account: Option<String> = None;
    let mut current_service: Option<String> = None;
    for line in dump.lines() {
        if let Some(account) = keychain_attribute_value(line, "acct") {
            current_account = Some(account);
        }
        if let Some(found_service) = keychain_attribute_value(line, "svce") {
            current_service = Some(found_service);
        }
        if current_service.as_deref() == Some(service) {
            if let Some(account) = current_account.as_ref() {
                if account == server || account.starts_with(&format!("{server}|")) {
                    accounts.push(account.clone());
                }
            }
            current_account = None;
            current_service = None;
        }
    }
    accounts.sort();
    accounts.dedup();
    accounts
}

fn keychain_attribute_value(line: &str, attr: &str) -> Option<String> {
    let marker = format!("\"{attr}\"<blob>=");
    let value = line.split(&marker).nth(1)?.trim();
    let quoted = value.strip_prefix('"')?;
    let end = quoted.find('"')?;
    Some(quoted[..end].to_string())
}

fn mcp_pool_key(server: &McpServerConfig) -> String {
    let mut key = vec![
        server.name.clone(),
        server.command.clone(),
        server.args.join("\u{1f}"),
        server.url.clone(),
        server.cwd.clone(),
        server.bearer_token_env_var.clone(),
        server
            .startup_timeout_sec
            .map(|v| v.to_string())
            .unwrap_or_default(),
        server
            .tool_timeout_sec
            .map(|v| v.to_string())
            .unwrap_or_default(),
    ];
    key.push(format!("{:?}", server.env));
    key.push(format!("{:?}", server.env_vars));
    key.push(format!("{:?}", server.http_headers));
    key.push(format!("{:?}", server.env_http_headers));
    key.push(server.enabled_tools.join("\u{1f}"));
    key.push(server.disabled_tools.join("\u{1f}"));
    key.join("\u{1e}")
}

/// hermes-style progressive tool disclosure: when the DEFERRABLE (MCP) tool
/// schemas would eat too much of the context on every request, they are
/// stripped from the model-visible array and replaced by three tiny bridge
/// tools. The full specs stay registered with the router, so a bridged call
/// still hits the normal approval/sandbox chokepoint.
/// hermes verify-on-stop: is this shell command a VERIFICATION (test / lint /
/// typecheck / build) whose passing exit is evidence the edits work?
fn is_verification_command(cmd: &str) -> bool {
    let c = cmd.to_ascii_lowercase();
    [
        "cargo test",
        "cargo check",
        "cargo clippy",
        "cargo build",
        "pytest",
        "npm test",
        "npm run test",
        "yarn test",
        "pnpm test",
        "go test",
        "go vet",
        "go build",
        "tsc",
        "eslint",
        "ruff",
        "mypy",
        "flake8",
        "vitest",
        "jest",
        "make test",
        "make check",
        "mix test",
        "rspec",
        "phpunit",
        "gradle test",
        "mvn test",
        "swift test",
    ]
    .iter()
    .any(|p| c.contains(p))
        || c.contains(" lint")
        || c.starts_with("lint")
}

/// Do these edited paths include CODE (vs docs/config prose)? Doc-only edits
/// must never trigger the verify-on-stop nudge.
fn edits_touch_code(paths: &[String]) -> bool {
    paths.iter().any(|p| {
        let lower = p.to_ascii_lowercase();
        !(lower.ends_with(".md")
            || lower.ends_with(".txt")
            || lower.ends_with(".rst")
            || lower.ends_with(".adoc")
            || lower.ends_with("license")
            || lower.ends_with(".license")
            || lower.ends_with(".svg")
            || lower.ends_with(".png"))
    })
}

/// The detached reviewer call behind [`Engine::maybe_self_review`]. Restricted
/// toolset — the model can ONLY `remember`/`save_skill`; its text output is
/// discarded, tool calls are applied directly to the memory store.
async fn self_review_task(
    provider_id: String,
    model: String,
    workspace: std::path::PathBuf,
    digest: String,
) {
    use oxide_providers::{Message, Role, StreamItem, TurnRequest};
    let provider = oxide_providers::build(&provider_id);
    let tools = vec![
        ToolSpec::new("remember", "Save ONE durable user preference or project fact.")
            .params(serde_json::json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]})),
        ToolSpec::new("save_skill", "Save ONE reusable multi-step procedure as markdown.")
            .params(serde_json::json!({"type":"object","properties":{
                "name":{"type":"string"},"content":{"type":"string"}
            },"required":["name","content"]})),
    ];
    let req = TurnRequest {
        model,
        reasoning_effort: "low".to_string(),
        temperature: 0.2,
        messages: vec![
            Message::new(
                Role::System,
                "You are Oxide's background self-improvement reviewer. Read the conversation \
                 digest. ONLY if it contains a durable user preference/project fact, call \
                 `remember`; ONLY if it contains a reusable multi-step procedure worth \
                 repeating in future sessions, call `save_skill` (concise markdown). At most \
                 2 calls total. Most digests contain NOTHING durable — then reply exactly \
                 'nothing'. Never echo the digest.",
            ),
            Message::new(Role::User, digest),
        ],
        tools,
        cwd: workspace.display().to_string(),
        conversation_id: String::new(),
        cli_resume: None,
        system_append: None,
        claude_agents: None,
    };
    let (tx, mut rx) = mpsc::channel(64);
    let worker = tokio::spawn(async move {
        let _ = provider.stream(req, tx).await;
    });
    let mem = memory::Memory::new(&workspace);
    let mut applied = 0u8;
    while let Some(item) = rx.recv().await {
        if applied >= 2 {
            continue; // drain silently; cap writes
        }
        if let StreamItem::ToolCall {
            name, arguments, ..
        } = item
        {
            match name.as_str() {
                "remember" => {
                    if let Some(t) = arguments["text"].as_str() {
                        let _ = mem.remember(t);
                        applied += 1;
                    }
                }
                "save_skill" => {
                    if let (Some(n), Some(c)) =
                        (arguments["name"].as_str(), arguments["content"].as_str())
                    {
                        let _ = mem.save_skill(n, c);
                        applied += 1;
                    }
                }
                _ => {}
            }
        }
    }
    let _ = worker.await;
    if applied > 0 {
        tracing::info!(applied, "self-review saved durable knowledge");
    }
}

fn deferrable_schema_chars(tools: &[ToolSpec]) -> usize {
    tools
        .iter()
        .map(|t| {
            t.name.len()
                + t.description.len()
                + serde_json::to_string(&t.parameters)
                    .map(|s| s.len())
                    .unwrap_or(0)
        })
        .sum()
}

fn tool_bridge_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec::new(
            "tool_search",
            "Find available EXTERNAL (MCP) tools by keyword. Their full schemas are deferred to keep context small — search first, then `tool_describe`, then `tool_call`.",
        )
        .params(serde_json::json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"]})),
        ToolSpec::new(
            "tool_describe",
            "Get the full JSON schema of a deferred external tool by exact name (from tool_search).",
        )
        .params(serde_json::json!({"type":"object","properties":{"name":{"type":"string"}},"required":["name"]})),
        ToolSpec::new(
            "tool_call",
            "Invoke a deferred external tool by exact name with its arguments object. Permissions apply exactly as if called directly.",
        )
        .mutating(true)
        .params(serde_json::json!({"type":"object","properties":{
            "name":{"type":"string"},
            "arguments":{"type":"object"}
        },"required":["name"]})),
    ]
}

fn tools_for_routing(mut tools: Vec<ToolSpec>, deferred_active: bool) -> Vec<ToolSpec> {
    if deferred_active {
        tools.extend(tool_bridge_specs());
    }
    tools
}

/// Keyword search over deferred tools (name > description weighting).
fn search_deferred(deferred: &[ToolSpec], query: &str) -> String {
    if deferred.is_empty() {
        return "tool_search: no tools are deferred right now — every available tool is already in your tool list.".to_string();
    }
    let words: Vec<String> = query
        .split_whitespace()
        .map(|w| w.to_ascii_lowercase())
        .filter(|w| !w.is_empty())
        .collect();
    let mut scored: Vec<(i32, &ToolSpec)> = deferred
        .iter()
        .map(|t| {
            let name = t.name.to_ascii_lowercase();
            let desc = t.description.to_ascii_lowercase();
            let score: i32 = words
                .iter()
                .map(|w| {
                    (if name.contains(w.as_str()) { 3 } else { 0 })
                        + (if desc.contains(w.as_str()) { 1 } else { 0 })
                })
                .sum();
            (score, t)
        })
        .filter(|(s, _)| *s > 0 || words.is_empty())
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name.cmp(&b.1.name)));
    if scored.is_empty() {
        return format!(
            "tool_search: nothing matched \"{query}\" among {} deferred tools.",
            deferred.len()
        );
    }
    let mut out = "Matching deferred tools (use tool_describe / tool_call):\n".to_string();
    for (_, t) in scored.iter().take(8) {
        let desc: String = t.description.chars().take(110).collect();
        out.push_str(&format!("- {} — {}\n", t.name, desc));
    }
    out
}

fn describe_deferred(deferred: &[ToolSpec], name: &str) -> String {
    match deferred.iter().find(|t| t.name == name) {
        Some(t) => format!(
            "{} — {}\nparameters schema:\n{}",
            t.name,
            t.description,
            serde_json::to_string_pretty(&t.parameters).unwrap_or_else(|_| "{}".into())
        ),
        None => format!("tool_describe: no deferred tool named '{name}' — use tool_search."),
    }
}

fn filter_mcp_tools(server: &McpServerConfig, tools: Vec<ToolSpec>) -> Vec<ToolSpec> {
    let prefix = format!("mcp__{}__", server.name);
    tools
        .into_iter()
        .filter(|tool| {
            let bare = tool.name.strip_prefix(&prefix).unwrap_or(&tool.name);
            server.tool_allowed(bare)
        })
        .collect()
}

fn resolve_external_mcp_reference(
    reference: &McpServerConfig,
    discovered: &[McpServerConfig],
) -> Option<McpServerConfig> {
    if !reference.external_ref {
        return Some(reference.clone());
    }
    let mut resolved = if reference.source.is_empty() {
        discovered
            .iter()
            .find(|server| server.name == reference.name)
    } else {
        discovered
            .iter()
            .find(|server| server.name == reference.name && server.source == reference.source)
    }?
    .clone();
    resolved.enabled = reference.enabled;
    resolved.required = reference.required;
    resolved.startup_timeout_sec = reference
        .startup_timeout_sec
        .or(resolved.startup_timeout_sec);
    resolved.tool_timeout_sec = reference.tool_timeout_sec.or(resolved.tool_timeout_sec);
    if !reference.enabled_tools.is_empty() {
        resolved.enabled_tools = reference.enabled_tools.clone();
    }
    if !reference.disabled_tools.is_empty() {
        resolved.disabled_tools = reference.disabled_tools.clone();
    }
    resolved.external_ref = false;
    Some(resolved)
}

use futures::{stream::FuturesUnordered, FutureExt, StreamExt};
use oxide_harness::{Harness, Registry, SkillRoute, ToolPolicyMode};
use oxide_mcp::{is_mcp_tool, server_of, HttpOptions, McpClient, StdioSpawnOptions};
use oxide_protocol::{
    ApprovalDecision, Event, Op, SubagentControlAction, ToolSpec, TurnId, UiSpec,
};
use oxide_providers::{Message, Provider, Role, StreamItem, TurnRequest};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use store::{CheckpointStore, SessionStore};
use tokio::sync::{mpsc, Barrier, Mutex};

const OP_QUEUE: usize = 64;
const EVENT_QUEUE: usize = 256;
const STREAM_QUEUE: usize = 256;
type ExternalMcpCache =
    std::sync::Mutex<HashMap<String, (std::time::Instant, Vec<McpServerConfig>)>>;

/// Fan-out depth for the multi-subscriber event bus. A subscriber that lags
/// further behind than this loses the gap and must resnapshot (Synara model);
/// the in-memory log keeps the full history for replay regardless.
const BROADCAST_CAP: usize = 4096;

/// A globally-sequenced engine event: `(seq, event)`. The `seq` is a monotonic
/// per-engine counter so any subscriber can order + dedup, and a late or
/// reconnecting one can ask for everything `after` a seq it already applied.
pub type SeqEvent = (u64, Event);

/// Multi-subscriber event fan-out + replay log for ONE engine run.
///
/// The primary `mpsc::Receiver` returned by [`spawn`] is unchanged (it drives the
/// main frontend). The bus is additive: it lets ADDITIONAL surfaces attach to the
/// SAME run — e.g. a TUI tab co-observing a GUI tab's turn — by taking a snapshot
/// (every event so far) plus a live tail, all keyed by a global monotonic `seq`.
/// This is the foundation for cross-surface continuity, reconnect/resnapshot, and
/// replay/audit (the Synara event-sourcing model, minus durability for now —
/// the log is in-memory; a file/SQLite backing can be added later).
pub struct EventBus {
    seq: AtomicU64,
    log: std::sync::Mutex<Vec<SeqEvent>>,
    tx: tokio::sync::broadcast::Sender<SeqEvent>,
}

impl EventBus {
    fn new() -> Arc<Self> {
        let (tx, _rx) = tokio::sync::broadcast::channel(BROADCAST_CAP);
        Arc::new(Self {
            seq: AtomicU64::new(0),
            log: std::sync::Mutex::new(Vec::new()),
            tx,
        })
    }

    /// Record one event in the replay log and fan it out live. Returns its `seq`.
    fn publish(&self, ev: &Event) -> u64 {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut log) = self.log.lock() {
            log.push((seq, ev.clone()));
        }
        // Err only when there are no live subscribers — harmless; the log still
        // has it for a future subscriber's snapshot.
        let _ = self.tx.send((seq, ev.clone()));
        seq
    }

    /// Attach a new subscriber: a snapshot of every event with `seq >= after`
    /// plus a live receiver for the tail. Subscribe to the live tail FIRST so no
    /// event can slip between the snapshot and the tail; a small snapshot/tail
    /// overlap is fine because the caller orders + dedups by `seq`.
    pub fn subscribe(
        &self,
        after: u64,
    ) -> (Vec<SeqEvent>, tokio::sync::broadcast::Receiver<SeqEvent>) {
        let rx = self.tx.subscribe();
        let snapshot = self
            .log
            .lock()
            .map(|log| log.iter().filter(|(s, _)| *s >= after).cloned().collect())
            .unwrap_or_default();
        (snapshot, rx)
    }
}

/// Cloneable handle a frontend uses to submit [`Op`]s into the engine.
#[derive(Clone)]
pub struct EngineHandle {
    op_tx: mpsc::Sender<Op>,
    bus: Arc<EventBus>,
}

impl EngineHandle {
    pub async fn submit(&self, op: Op) -> anyhow::Result<()> {
        self.op_tx
            .send(op)
            .await
            .map_err(|_| anyhow::anyhow!("engine task is gone"))?;
        Ok(())
    }

    /// Attach an ADDITIONAL surface to this engine's live event stream: a snapshot
    /// (everything emitted so far, from `after` exclusive of lower seqs) + a live
    /// tail. The primary [`spawn`] receiver keeps working independently.
    pub fn subscribe(
        &self,
        after: u64,
    ) -> (Vec<SeqEvent>, tokio::sync::broadcast::Receiver<SeqEvent>) {
        self.bus.subscribe(after)
    }

    /// The engine's event bus, for serving its stream to OTHER processes over a
    /// local socket — see [`serve_events`].
    pub fn bus(&self) -> Arc<EventBus> {
        self.bus.clone()
    }
}

/// Serve one engine's event stream to OTHER PROCESSES over a Unix domain socket.
///
/// Each connecting client receives a snapshot (every event so far) then the live
/// tail, as newline-delimited JSON `[seq, event]`, ordered by `seq`. This is the
/// cross-process half of the Synara model: a separate process (e.g. a TUI tab)
/// can co-observe a running GUI engine. Pair with [`subscribe_over_socket`].
///
/// Runs until the future is dropped or the socket errors. A lagged client is
/// disconnected so it reconnects and re-snapshots (no silent gaps).
pub async fn serve_events(path: PathBuf, bus: Arc<EventBus>) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;
    // Replace any stale socket from a prior run.
    let _ = std::fs::remove_file(&path);
    let listener = tokio::net::UnixListener::bind(&path)?;
    loop {
        let (mut stream, _) = listener.accept().await?;
        let (snapshot, mut rx) = bus.subscribe(0);
        tokio::spawn(async move {
            for ev in snapshot {
                let Ok(mut line) = serde_json::to_vec(&ev) else {
                    continue;
                };
                line.push(b'\n');
                if stream.write_all(&line).await.is_err() {
                    return;
                }
            }
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        let Ok(mut line) = serde_json::to_vec(&ev) else {
                            continue;
                        };
                        line.push(b'\n');
                        if stream.write_all(&line).await.is_err() {
                            return;
                        }
                    }
                    // Fell too far behind — drop so the client reconnects + resnapshots.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => return,
                    Err(_) => return,
                }
            }
        });
    }
}

/// Connect to a [`serve_events`] socket and receive its `(seq, Event)` stream.
/// Reconnects are the caller's job (re-call after the channel closes). Retries the
/// initial connect briefly so a just-started server isn't missed.
pub fn subscribe_over_socket(path: PathBuf) -> tokio::sync::mpsc::UnboundedReceiver<SeqEvent> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt;
        let mut stream = None;
        for _ in 0..25 {
            match tokio::net::UnixStream::connect(&path).await {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
        let Some(stream) = stream else { return };
        let mut lines = tokio::io::BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(ev) = serde_json::from_str::<SeqEvent>(&line) {
                if tx.send(ev).is_err() {
                    return;
                }
            }
        }
    });
    rx
}

fn registry_from_config(config: &Config) -> anyhow::Result<Registry> {
    let mut registry = Registry::with_builtins();
    let workspace = config.workspace.as_deref();
    for dir in oxide_harness::manifest_dirs(config.harness_dir.as_deref(), workspace) {
        if let Err(e) = registry.load_dir(&dir) {
            tracing::warn!(dir = %dir.display(), error = %e, "failed scanning harness dir");
        }
    }
    if registry.get(&config.harness).is_none() {
        anyhow::bail!(
            "configured harness '{}' not found (have: {:?})",
            config.harness,
            registry.ids()
        );
    }
    Ok(registry)
}

/// Start the engine task. Returns a handle to drive it and the event stream to
/// subscribe to. The engine runs until [`Op::Shutdown`] or all handles drop.
pub fn spawn(config: Config) -> anyhow::Result<(EngineHandle, mpsc::Receiver<Event>)> {
    let (op_tx, op_rx) = mpsc::channel(OP_QUEUE);
    let (event_tx, event_rx) = mpsc::channel(EVENT_QUEUE);
    let bus = EventBus::new();
    let registry = registry_from_config(&config)?;

    let workspace = config
        .workspace
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    // Resume reads the previous session *before* opening the new one.
    let mut history: Vec<Message> = Vec::new();
    // An explicit session id (tab/history) wins over generic "resume latest".
    let resume_id: Option<String> = config
        .resume_path
        .as_deref()
        .map(|p| p.display().to_string())
        .or_else(|| {
            if config.resume {
                SessionStore::latest(&workspace)
            } else {
                None
            }
        });
    if let Some(id) = &resume_id {
        if let Ok(msgs) = SessionStore::load(id) {
            history = msgs
                .into_iter()
                .filter(|m| model_history_role(&m.role))
                .map(|m| Message::new(role_from_str(&m.role), m.content))
                .collect();
            tracing::info!(count = history.len(), "resumed session {id}");
        }
    }

    let session_store = if config.persist {
        // Resuming attaches to the EXISTING session id; otherwise open a fresh
        // one (its row is created lazily on the first message).
        let attached = resume_id
            .as_deref()
            .and_then(|id| SessionStore::attach(id, &workspace).ok());
        match attached
            .map(Ok)
            .unwrap_or_else(|| SessionStore::open(&workspace))
        {
            Ok(s) => {
                // Stamp the runtime config (sidebar logo + exact replay mode);
                // applied now if the row exists, else carried to first append.
                s.set_runtime_config(
                    &config.provider,
                    &config.model,
                    &config.harness,
                    &config.reasoning_effort,
                );
                Some(s)
            }
            Err(e) => {
                tracing::warn!(error = %e, "session persistence disabled");
                None
            }
        }
    } else {
        None
    };

    let engine = Engine {
        config,
        registry,
        provider: oxide_providers::build("echo"),
        session: history,
        next_turn: 1,
        next_approval: Arc::new(AtomicU64::new(1)),
        session_approved: HashSet::new(),
        checkpoints: Arc::new(Mutex::new(CheckpointStore::load(&workspace))),
        workspace,
        session_store,
        subagent_parent: None,
        mcp_clients: Vec::new(),
        mcp_tools: Vec::new(),
        deferred_tools: Vec::new(),
        turns_since_review: 0,
        turn_verify_passed: false,
        pending_verify_cmds: std::collections::HashSet::new(),
        bg_done_tx: None,
        bg_spawn_tx: None,
        bg_tasks_running: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        bg_task_seq: 0,
        mcp_instructions: Vec::new(),
        required_mcp_unavailable: false,
        browser: None,
        ctx_window: None,
        read_files: std::collections::HashSet::new(),
        turn_edited: false,
        turn_edit_paths: Vec::new(),
        turn_reads: std::collections::HashSet::new(),
        last_tool_sig: String::new(),
        last_tool_reps: 0,
        turn_tool_signatures: std::collections::HashSet::new(),
        turn_todos: Vec::new(),
        user_interrupted: false,
        event_tx,
        bus: bus.clone(),
    };

    tokio::spawn(engine.run(op_rx));
    Ok((EngineHandle { op_tx, bus }, event_rx))
}

/// Stream idle limit per provider. CLI drivers (claude/codex) manage their OWN
/// timeouts (per-tool bash limits, API retries) and legitimately stay silent
/// for as long as a build/test runs — so DON'T impose a wall-clock idle cap on
/// them; the real signal is the child exiting, which closes the stream. The
/// 180s guard is only for HTTP/SSE providers, where a long silence = a real
/// stalled connection nothing else will recover.
fn idle_timeout_for(provider: &str) -> std::time::Duration {
    match provider {
        // Effectively "never" (a finite value avoids Instant overflow in
        // tokio's timer); the child exiting closes the stream and ends the round.
        "claude" | "claude_interactive" | "codex" => std::time::Duration::from_secs(30 * 24 * 3600),
        _ => std::time::Duration::from_secs(180),
    }
}

fn role_from_str(s: &str) -> Role {
    match s {
        "system" => Role::System,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        _ => Role::User,
    }
}

fn model_history_role(role: &str) -> bool {
    matches!(role, "system" | "user" | "assistant" | "tool")
}

/// Background jobs recorded in a session's rows, `(id, command, path)` —
/// last occurrence per id, kept only while the output file's mtime is within
/// `max_age`. These jobs carry no exit status (the process belongs to the CLI
/// driver), so a stale file is the only "it's over" signal.
/// ponytail: the mtime bound is the resurrection ceiling — a dismissed chip
/// comes back on reopen until the file goes stale; persist dismissals if that
/// ever annoys.
fn replayable_bg_jobs(
    rows: Vec<(String, String)>,
    max_age: std::time::Duration,
) -> Vec<(String, String, String)> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (role, content) in rows.into_iter().rev() {
        if role != "bg_job" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) else {
            continue;
        };
        let (Some(id), Some(command), Some(path)) =
            (v["id"].as_str(), v["command"].as_str(), v["path"].as_str())
        else {
            continue;
        };
        if !seen.insert(id.to_string()) {
            continue;
        }
        let fresh = std::fs::metadata(path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .map(|age| age < max_age)
            .unwrap_or(false);
        if fresh {
            out.push((id.to_string(), command.to_string(), path.to_string()));
        }
    }
    out
}

fn compact_chars(text: &str, limit: usize) -> String {
    let mut out: String = text.chars().take(limit).collect();
    if text.chars().count() > limit {
        out.push_str("\n…");
    }
    out
}

fn compact_json(value: &serde_json::Value, limit: usize) -> String {
    compact_chars(&value.to_string(), limit)
}

fn ui_props_tool_schema() -> serde_json::Value {
    serde_json::json!({
        "type":"object",
        "additionalProperties":false,
        "properties":{
            "title":{"type":"string","maxLength":4000},
            "text":{"type":"string","maxLength":4000},
            "label":{"type":"string","maxLength":4000},
            "value":{"type":"string","maxLength":4000},
            "caption":{"type":"string","maxLength":4000},
            "tone":{"type":"string","enum":["neutral","info","success","warning","danger"]},
            "language":{"type":"string","maxLength":40},
            "points":{"type":"array","maxItems":120,"items":{"type":"number"},"description":"chart: sparkline values, oldest first"},
            "options":{"type":"array","maxItems":12,"items":{"type":"string","maxLength":80},"description":"select: choices"},
            "placeholder":{"type":"string","maxLength":200},
            "columns":{
                "type":"array",
                "maxItems":12,
                "items":{
                    "type":"object",
                    "additionalProperties":false,
                    "properties":{
                        "key":{"type":"string","maxLength":120},
                        "label":{"type":"string","maxLength":120}
                    },
                    "required":["key","label"]
                }
            },
            "rows":{
                "type":"array",
                "maxItems":80,
                "items":{"type":"object"}
            },
            "action":{
                "type":"object",
                "additionalProperties":false,
                "properties":{
                    "name":{"type":"string","maxLength":120},
                    "label":{"type":"string","maxLength":120},
                    "payload":{}
                },
                "required":["name","label"]
            }
        }
    })
}

fn ui_node_tool_schema(depth: usize) -> serde_json::Value {
    let children = if depth == 0 {
        serde_json::json!({
            "type":"array",
            "maxItems":0,
            "description":"Maximum schema nesting reached; use shallower UI trees."
        })
    } else {
        serde_json::json!({
            "type":"array",
            "maxItems":24,
            "items": ui_node_tool_schema(depth - 1)
        })
    };
    serde_json::json!({
        "type":"object",
        "additionalProperties":false,
        "properties":{
            "id":{"type":"string","maxLength":120},
            "type":{"type":"string","enum":["stack","row","card","text","metric","table","code","alert","divider","action","chart","input","select"]},
            "props": ui_props_tool_schema(),
            "children": children
        },
        "required":["type"]
    })
}

fn ui_spec_tool_params() -> serde_json::Value {
    serde_json::json!({
        "type":"object",
        "additionalProperties":false,
        "properties":{
            "spec":{
                "type":"object",
                "additionalProperties":false,
                "description":"Rust-native UiSpec. Use table only with columns/rows, action only with props.action, chart with props.points, select with props.options; never emit HTML or JavaScript. Action buttons SUBMIT back to you: sibling input/select values are attached.",
                "properties":{
                    "title":{"type":"string","maxLength":4000},
                    "tone":{"type":"string","enum":["neutral","info","success","warning","danger"],"description":"optional theme tint for the whole card"},
                    "root": ui_node_tool_schema(4)
                },
                "required":["root"]
            }
        },
        "required":["spec"]
    })
}

fn design_selection_tool_schema() -> serde_json::Value {
    serde_json::json!({
        "type":"object",
        "additionalProperties":false,
        "properties":{
            "selector":{"type":"string","description":"Stable CSS selector or element handle from Design Workbench."},
            "component":{"type":"string","description":"Component hint, if known."},
            "source":{"type":"string","description":"Source file hint, if known."},
            "text":{"type":"string"},
            "html":{"type":"string"},
            "styles":{"type":"object","additionalProperties":{"type":"string"}}
        },
        "required":["selector"]
    })
}

fn design_edit_tool_schema() -> serde_json::Value {
    serde_json::json!({
        "type":"object",
        "additionalProperties":false,
        "properties":{
            "property":{"type":"string"},
            "old_value":{"type":"string"},
            "new_value":{"type":"string"}
        },
        "required":["property","new_value"]
    })
}

fn design_review_tool_params() -> serde_json::Value {
    serde_json::json!({
        "type":"object",
        "additionalProperties":false,
        "properties":{
            "selection": design_selection_tool_schema(),
            "edits":{"type":"array","items": design_edit_tool_schema()}
        },
        "required":["selection"]
    })
}

fn design_patch_tool_params() -> serde_json::Value {
    serde_json::json!({
        "type":"object",
        "additionalProperties":false,
        "properties":{
            "selection": design_selection_tool_schema(),
            "edits":{"type":"array","items": design_edit_tool_schema()},
            "instruction":{"type":"string","description":"Optional extra implementation instruction."}
        },
        "required":["selection","edits"]
    })
}

fn review_line_has_blocking_marker(line: &str) -> bool {
    let trimmed = line
        .trim_start_matches(|c: char| {
            c == '-' || c == '*' || c == ':' || c == ')' || c == '.' || c.is_ascii_digit()
        })
        .trim_start();
    let upper = trimmed.to_ascii_uppercase();
    [
        "GAP",
        "GAPS",
        "ISSUE",
        "ISSUES",
        "BUG",
        "BUGS",
        "REGRESSION",
        "REGRESSIONS",
        "FAIL",
        "FAILING",
        "MISSING",
    ]
    .iter()
    .any(|marker| {
        upper == *marker
            || upper.starts_with(&format!("{marker}:"))
            || upper.starts_with(&format!("{marker} -"))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolBudgetDecision {
    Continue,
    Extended { new_limit: usize },
    Stop,
}

#[derive(Debug, Clone)]
struct AdaptiveToolBudget {
    current_limit: usize,
    emergency_limit: usize,
    progress_checkpoint: usize,
}

impl AdaptiveToolBudget {
    fn new(configured: usize) -> Self {
        let current_limit = configured.clamp(1, 60);
        let emergency_limit = current_limit.saturating_mul(3).min(96);
        Self {
            current_limit,
            emergency_limit,
            progress_checkpoint: 0,
        }
    }

    fn after_tool_round(
        &mut self,
        rounds: usize,
        progress_score: usize,
        pending_todos: bool,
        edited_unverified: bool,
    ) -> ToolBudgetDecision {
        if rounds < self.current_limit {
            return ToolBudgetDecision::Continue;
        }
        if self.current_limit >= self.emergency_limit || progress_score <= self.progress_checkpoint
        {
            return ToolBudgetDecision::Stop;
        }

        let bonus = 8 + usize::from(pending_todos) * 4 + usize::from(edited_unverified) * 4;
        self.current_limit = self
            .current_limit
            .saturating_add(bonus)
            .min(self.emergency_limit);
        self.progress_checkpoint = progress_score;
        ToolBudgetDecision::Extended {
            new_limit: self.current_limit,
        }
    }
}

fn tool_progress_score(
    unique_tools: usize,
    unique_reads: usize,
    edited_paths: usize,
    completed_todos: usize,
    verified: bool,
) -> usize {
    unique_tools
        .saturating_add(unique_reads.saturating_mul(2))
        .saturating_add(edited_paths.saturating_mul(4))
        .saturating_add(completed_todos.saturating_mul(3))
        .saturating_add(usize::from(verified).saturating_mul(6))
}

fn review_passes_gate(review: &str) -> bool {
    let mut lines = review.trim_start().lines();
    let Some(first_line) = lines.next() else {
        return false;
    };
    if !first_line.trim().eq_ignore_ascii_case("DONE") {
        return false;
    }
    !lines.any(review_line_has_blocking_marker)
}

fn trigger_matches(user_text: &str, trigger: &str) -> bool {
    let trigger = trigger.trim().to_ascii_lowercase();
    if trigger.is_empty() {
        return false;
    }
    let text = user_text.to_ascii_lowercase();
    if trigger.contains(' ') {
        return text.contains(&trigger);
    }
    text.split(|c: char| !c.is_alphanumeric())
        .any(|token| token == trigger)
}

fn selected_skill_route(harness: &dyn Harness, user_text: &str) -> Option<SkillRoute> {
    harness.skill_routes().into_iter().find(|route| {
        route
            .triggers
            .iter()
            .any(|trigger| trigger_matches(user_text, trigger))
    })
}

fn render_skill_route(route: &SkillRoute) -> String {
    let mut out = format!(
        "\n\n# Harness workflow route: {}\n{}\n",
        route.id,
        route.instructions.trim()
    );
    if !route.template.is_empty() {
        out.push_str("\nSuggested workflow checklist:\n");
        for item in &route.template {
            out.push_str("- ");
            out.push_str(item.trim());
            out.push('\n');
        }
    }
    out
}

enum VerifyOutcome {
    Passed,
    Failed(String),
    Skipped,
}

struct Engine {
    config: Config,
    registry: Registry,
    provider: Box<dyn Provider>,
    /// Conversation history (system prompt is injected per-turn from the harness).
    session: Vec<Message>,
    next_turn: u64,
    next_approval: Arc<AtomicU64>,
    /// Tools approved for the whole session via ApproveForSession.
    session_approved: HashSet<String>,
    /// Root all tool filesystem/shell access is confined to.
    workspace: PathBuf,
    /// Append-only session log (None if persistence is off/unavailable).
    session_store: Option<SessionStore>,
    /// Id sesi induk saat engine ini adalah worker sub-agent (Synara model):
    /// transkrip anak dipersist sebagai sesi ber-parent_id, bukan dibuang.
    subagent_parent: Option<String>,
    /// Undo log for file-mutating tool calls.
    checkpoints: Arc<Mutex<CheckpointStore>>,
    /// Connected MCP servers (one per configured launcher).
    mcp_clients: Vec<std::sync::Arc<McpClient>>,
    /// Namespaced tool specs discovered from all MCP servers.
    mcp_tools: Vec<ToolSpec>,
    /// Tools stripped from the model-visible array this turn (schema bloat) —
    /// reachable via the tool_search/tool_describe/tool_call bridge.
    deferred_tools: Vec<ToolSpec>,
    /// Completed turns since the last background self-review fork.
    turns_since_review: u32,
    /// hermes verify-on-stop evidence: a verification command (test/lint/
    /// build) ran AND passed during the current turn.
    turn_verify_passed: bool,
    /// CLI-driver command ids whose command text classified as verification —
    /// resolved to evidence when their CommandFinished arrives ok.
    pending_verify_cmds: std::collections::HashSet<String>,
    /// Completion channel for background delegated subagents; results are
    /// drained ONLY while the engine is idle and re-enter as a fresh turn
    /// (hermes' completion-queue — never spliced into an in-flight turn).
    bg_done_tx: Option<mpsc::UnboundedSender<String>>,
    /// Dispatcher channel that actually spawns background delegations.
    bg_spawn_tx: Option<mpsc::UnboundedSender<BgDelegation>>,
    /// Live background subagents (capacity gate: max 3, reject not queue).
    bg_tasks_running: Arc<std::sync::atomic::AtomicUsize>,
    /// Monotonic handle counter for background delegations.
    bg_task_seq: u64,
    /// Server-level MCP instructions returned during initialize.
    mcp_instructions: Vec<(String, String)>,
    /// True when a configured required MCP server failed to connect.
    required_mcp_unavailable: bool,
    /// Lazily launched browser-automation session.
    browser: Option<browser::BrowserSession>,
    /// Model context window (tokens), reported by the provider; drives the
    /// compaction budget at 75% (opencode-style).
    ctx_window: Option<u64>,
    /// Files the model has read this session — `edit` requires a prior read.
    read_files: std::collections::HashSet<String>,
    /// Set when a write/edit succeeds this turn — drives the auto-verify pass.
    turn_edited: bool,
    /// Paths edited this turn — auto-verify only runs when a *relevant* code file
    /// was touched, so a README-only edit can't drag the agent into unrelated
    /// typecheck errors.
    turn_edit_paths: Vec<String>,
    /// Files already read THIS turn — re-reading the same file is intercepted to
    /// break the "read README, re-plan, read README again" loop.
    turn_reads: std::collections::HashSet<String>,
    /// Doom-loop guard: last tool call signature + consecutive repeat count.
    last_tool_sig: String,
    last_tool_reps: u8,
    /// Distinct tool calls and latest checklist, used by the adaptive turn budget.
    turn_tool_signatures: std::collections::HashSet<String>,
    turn_todos: Vec<(String, String)>,
    /// Set when the user interrupts a turn. The next turn prepends a notice so
    /// the model (esp. resumed CLI drivers, which carry their own todo/plan
    /// state) abandons the aborted work instead of finishing it over the new
    /// instruction. Reset once consumed.
    user_interrupted: bool,
    event_tx: mpsc::Sender<Event>,
    /// Multi-subscriber fan-out + replay log, mirrored by every `emit`.
    bus: Arc<EventBus>,
}

/// A background delegation, shipped from handle_tool_call to the dispatcher
/// task via channel. Indirection is deliberate: spawning the worker future
/// directly inside handle_tool_call creates a recursive Send-inference cycle
/// (the worker itself runs handle_tool_call).
struct BgDelegation {
    worker: Box<Engine>,
    system: String,
    task: String,
    worker_id: String,
    profile: WorkerProfile,
    handle: String,
    done_tx: mpsc::UnboundedSender<String>,
    counter: Arc<std::sync::atomic::AtomicUsize>,
    /// `/btw` side questions: deliver the answer straight to the frontend as
    /// a note (question included) instead of re-entering the model loop.
    notify: Option<mpsc::Sender<Event>>,
}

#[derive(Clone)]
struct WorkerProfile {
    id: String,
    provider: String,
    effort: String,
    instructions: String,
    toolset: WorkerToolset,
    max_steps: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkerToolset {
    Full,
    ReviewReadOnly,
    VerifyReadOnly,
    ExploreReadOnly,
}

impl WorkerToolset {
    fn label(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::ReviewReadOnly => "review-read-only",
            Self::VerifyReadOnly => "verify-read-only",
            Self::ExploreReadOnly => "explore-read-only",
        }
    }

    fn allowed_patterns(self) -> &'static [&'static str] {
        match self {
            Self::Full => &["*"],
            Self::ReviewReadOnly => &[
                "read_file",
                "search",
                "codebase_search",
                "fetch_url",
                "web_search",
                "design_read_system",
                "design_extract_tokens",
                "design_review",
                "design_propose_patch",
            ],
            Self::VerifyReadOnly => &[
                "read_file",
                "search",
                "codebase_search",
                "browser_read",
                "fetch_url",
                "design_read_system",
                "design_extract_tokens",
                "design_review",
            ],
            Self::ExploreReadOnly => &[
                "read_file",
                "search",
                "codebase_search",
                "browser_read",
                "fetch_url",
                "web_search",
                "design_read_system",
                "design_extract_tokens",
                "design_review",
                "design_propose_patch",
            ],
        }
    }

    fn allows(self, tool_name: &str) -> bool {
        self.allowed_patterns().iter().any(|allowed| {
            if *allowed == "*" {
                true
            } else if let Some(prefix) = allowed.strip_suffix('*') {
                tool_name.starts_with(prefix)
            } else {
                tool_name == *allowed
            }
        })
    }
}

struct SubagentAssignment {
    index: usize,
    task: String,
    system: String,
    worker_id: String,
    profile: WorkerProfile,
}

struct SubagentRunResult {
    index: usize,
    task: String,
    output: String,
    interrupted: bool,
    edited: bool,
    edit_paths: Vec<String>,
    read_files: HashSet<String>,
    session_approved: HashSet<String>,
}

const DEFAULT_SUBAGENT_OPERATING_MODE: &str = "\
Operate with high agency: think clearly, verify evidence, act proactively, \
question weak assumptions, and keep moving toward a concrete result. Treat \
blockers as things to diagnose, not excuses to stop. Prefer primary evidence \
from code, tests, logs, raw payloads, release assets, or UI behavior over \
speculation. Be constructively disagreeable when the plan or assumption is \
weak, while staying within safety, permission, sandbox, and tool policies. End \
with proof of what changed, what was validated, what remains risky, and the \
next concrete action.";

#[derive(Debug, Default, Deserialize)]
struct SelfImproveCapture {
    #[serde(default)]
    facts: Vec<String>,
    #[serde(default)]
    skills: Vec<SelfImproveSkill>,
}

#[derive(Debug, Default, Deserialize)]
struct SelfImproveSkill {
    #[serde(default)]
    name: String,
    #[serde(default)]
    content: String,
}

impl WorkerProfile {
    fn implementer(provider: &str, effort: &str) -> Self {
        Self {
            id: "implementer".to_string(),
            provider: provider.to_string(),
            effort: effort.to_string(),
            instructions: "You may inspect, edit, run commands, and verify. Keep changes scoped and finish the assigned implementation.".to_string(),
            toolset: WorkerToolset::Full,
            max_steps: 24,
        }
    }
}

fn worker_profile_system_block(profile: &WorkerProfile, exposed_tools: usize) -> String {
    let tool_policy = profile.toolset.label();
    format!(
        "# Sub-agent default operating mode\n{}\n\n# Sub-agent profile: {}\n{}\n\nToolset: {tool_policy}. Available tool policy: {} tool(s) exposed for this worker.",
        DEFAULT_SUBAGENT_OPERATING_MODE,
        profile.id,
        profile.instructions,
        exposed_tools
    )
}

fn subagent_profile_for(task: &str, provider: &str, effort: &str) -> WorkerProfile {
    let lower = task.to_ascii_lowercase();
    if lower.contains("test")
        || lower.contains("verify")
        || lower.contains("lint")
        || lower.contains("build")
    {
        WorkerProfile {
            id: "tester".to_string(),
            provider: provider.to_string(),
            effort: "low".to_string(),
            instructions: "You are the tester subagent. Run or inspect verification only. Do not edit files; report failures with exact commands and diagnostics.".to_string(),
            toolset: WorkerToolset::VerifyReadOnly,
            max_steps: 12,
        }
    } else if lower.contains("review")
        || lower.contains("audit")
        || lower.contains("risk")
        || lower.contains("risiko")
    {
        WorkerProfile {
            id: "reviewer".to_string(),
            provider: provider.to_string(),
            effort: "high".to_string(),
            instructions: "You are the reviewer subagent. Inspect for correctness, regressions, security, and test gaps. Do not edit files; return findings with file references.".to_string(),
            toolset: WorkerToolset::ReviewReadOnly,
            max_steps: 12,
        }
    } else if lower.contains("inspect")
        || lower.contains("explore")
        || lower.contains("find")
        || lower.contains("research")
    {
        WorkerProfile {
            id: "explorer".to_string(),
            provider: provider.to_string(),
            effort: "low".to_string(),
            instructions: "You are the explorer subagent. Locate relevant code, docs, and facts. Do not edit files; return a concise map of what matters.".to_string(),
            toolset: WorkerToolset::ExploreReadOnly,
            max_steps: 10,
        }
    } else {
        WorkerProfile::implementer(provider, effort)
    }
}

impl Engine {
    async fn emit(&self, ev: Event) {
        // Mirror to the multi-subscriber bus (seq + replay log) before handing the
        // event to the primary frontend channel.
        self.bus.publish(&ev);
        let _ = self.event_tx.send(ev).await;
    }

    async fn emit_audit(
        &self,
        turn: Option<TurnId>,
        kind: &str,
        title: impl Into<String>,
        detail: impl Into<String>,
        status: &str,
    ) {
        let title = compact_chars(&title.into(), 240);
        let detail = compact_chars(&detail.into(), 1600);
        let status = status.to_string();
        if turn.is_some() {
            if let Some(store) = &self.session_store {
                let body = serde_json::json!({
                    "kind": kind,
                    "title": title,
                    "detail": detail,
                    "status": status,
                    "turn": turn.map(|t| t.0),
                    "ts_ms": std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or_default(),
                });
                let _ = store.append("event", &body.to_string());
            }
        }
        self.emit(Event::AuditLog {
            turn,
            kind: kind.to_string(),
            title,
            detail,
            status,
        })
        .await;
    }

    async fn emit_tool_end(
        &self,
        turn: TurnId,
        call_id: String,
        tool: String,
        output: String,
        ok: bool,
    ) {
        let status = if ok { "done" } else { "failed" };
        self.emit_audit(
            Some(turn),
            "tool",
            format!("Tool finished · {tool}"),
            output.trim(),
            status,
        )
        .await;
        self.emit(Event::ToolCallEnd {
            turn,
            call_id,
            tool,
            output,
            ok,
        })
        .await;
    }

    fn next_request_id(&self) -> u64 {
        self.next_approval.fetch_add(1, Ordering::Relaxed)
    }

    async fn rewind_checkpoint(&self, checkpoint_id: u64) -> u64 {
        self.checkpoints.lock().await.rewind(checkpoint_id)
    }

    /// Background self-improvement (hermes' review loop, v1): every
    /// `SELF_REVIEW_INTERVAL` completed turns, fork a DETACHED reviewer call
    /// that replays a compact digest of the recent conversation with ONLY the
    /// `remember`/`save_skill` tools and applies what it saves. The main
    /// conversation is never touched; failures are silent (best-effort).
    fn maybe_self_review(&mut self) {
        const SELF_REVIEW_INTERVAL: u32 = 5;
        let cli_driver = matches!(
            self.config.provider.as_str(),
            "codex" | "claude" | "claude_interactive"
        );
        if cli_driver || self.config.provider == "echo" {
            return;
        }
        self.turns_since_review += 1;
        if self.turns_since_review < SELF_REVIEW_INTERVAL {
            return;
        }
        self.turns_since_review = 0;
        // Compact digest: the last few user/assistant exchanges.
        let digest: String = self
            .session
            .iter()
            .rev()
            .filter(|m| {
                matches!(
                    m.role,
                    oxide_providers::Role::User | oxide_providers::Role::Assistant
                )
            })
            .take(12)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|m| {
                let role = if matches!(m.role, oxide_providers::Role::User) {
                    "user"
                } else {
                    "assistant"
                };
                let text: String = m.content.chars().take(600).collect();
                format!("[{role}] {text}\n")
            })
            .collect();
        if digest.trim().is_empty() {
            return;
        }
        let provider_id = self.config.provider.clone();
        let model = self.config.model.clone();
        let workspace = self.workspace.clone();
        tokio::spawn(self_review_task(provider_id, model, workspace, digest));
    }

    /// Surface a session-db failure (if any) before the turn closes. The db
    /// layer deliberately degrades on errors (warn + empty result) so the app
    /// keeps working — but the user must KNOW the transcript isn't persisting,
    /// or messages vanish silently (disk full, locked db, …).
    async fn note_db_error_once(&self) {
        if let Some(e) = db::take_db_error() {
            self.emit(Event::Info {
                text: format!(
                    "\u{26a0} session database write failed: {e} — recent messages may not be persisted (check disk space)."
                ),
            })
            .await;
        }
    }

    /// Abort the warm persistent-claude child's in-flight generation (no-op for
    /// every other provider / the one-shot driver). Without this, a plain
    /// Interrupt only aborts OUR stream task — the warm child keeps generating
    /// the dead turn and its leftover output desyncs the next one.
    fn interrupt_persistent_claude(&self) {
        if self.config.provider == "claude" {
            let conv = self
                .session_store
                .as_ref()
                .map(|s| s.id.clone())
                .unwrap_or_default();
            let cwd = self.workspace.display().to_string();
            oxide_providers::claude_persistent_interrupt(&conv, &cwd);
        }
    }

    async fn snapshot_checkpoint(&self, path: &Path) -> u64 {
        self.checkpoints.lock().await.snapshot(path)
    }

    async fn snapshot_checkpoint_with(&self, path: &Path, prior: Option<Vec<u8>>) -> u64 {
        self.checkpoints.lock().await.snapshot_with(path, prior)
    }

    fn active_harness(&self) -> &dyn Harness {
        // Validated non-None at spawn and on every SetHarness.
        self.registry
            .get(&self.config.harness)
            .expect("active harness present")
    }

    /// Native harness tools plus every discovered MCP tool. This is what the
    /// model sees and what the [`ToolRouter`] gates — MCP tools flow through the
    /// same approval/sandbox chokepoint as built-ins.
    fn all_tools(&self) -> Vec<ToolSpec> {
        let harness = self.active_harness();
        let policy = harness.tool_policy();
        let mut tools = harness.tools();
        let declared = tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<std::collections::BTreeSet<_>>();
        tools.extend(self.mcp_tools.iter().cloned());
        // Hermes-style persistent memory + self-improvement tools.
        tools.push(
            ToolSpec::new("remember", "Save a durable fact to persistent memory for future sessions.")
                .mutating(true)
                .params(serde_json::json!({
                    "type": "object",
                    "properties": { "text": { "type": "string", "description": "The fact to remember." } },
                    "required": ["text"]
                })),
        );
        tools.push(
            ToolSpec::new("save_skill", "Capture a reusable procedure/skill you discovered for future tasks.")
                .mutating(true)
                .params(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "content": { "type": "string", "description": "Markdown describing the skill steps." }
                    },
                    "required": ["name", "content"]
                })),
        );
        // Browser automation (headless/visible) for background web testing.
        tools.push(ToolSpec::new("browser_open", "Ask the frontend to open or focus a browser/app target for the user. Use when the user needs to inspect a page visually.")
            .mutating(true)
            .params(serde_json::json!({"type":"object","properties":{"url":{"type":"string"},"note":{"type":"string"}},"required":["url"]})));
        tools.push(ToolSpec::new("browser_snapshot", "Ask the frontend to capture/refresh a visual snapshot of a browser/app target.")
            .mutating(true)
            .params(serde_json::json!({"type":"object","properties":{"url":{"type":"string"},"note":{"type":"string"}},"required":["url"]})));
        tools.push(ToolSpec::new("browser_navigate", "Open a URL in the automation browser; returns the page title and visible text.")
            .mutating(true)
            .params(serde_json::json!({"type":"object","properties":{"url":{"type":"string"}},"required":["url"]})));
        tools.push(
            ToolSpec::new(
                "browser_read",
                "Read the current page's visible text (innerText).",
            )
            .params(serde_json::json!({"type":"object","properties":{}})),
        );
        tools.push(ToolSpec::new("browser_click", "Click the first element matching a CSS selector.")
            .mutating(true)
            .params(serde_json::json!({"type":"object","properties":{"selector":{"type":"string"}},"required":["selector"]})));
        tools.push(ToolSpec::new("browser_type", "Type text into the element matching a CSS selector.")
            .mutating(true)
            .params(serde_json::json!({"type":"object","properties":{"selector":{"type":"string"},"text":{"type":"string"}},"required":["selector","text"]})));
        tools.push(
            ToolSpec::new(
                "browser_screenshot",
                "Capture a PNG screenshot of the current page to .oxide/screenshots.",
            )
            .mutating(true)
            .params(serde_json::json!({"type":"object","properties":{}})),
        );
        tools.push(ToolSpec::new("browser_eval", "Run JavaScript in the page and return the JSON result.")
            .mutating(true)
            .params(serde_json::json!({"type":"object","properties":{"script":{"type":"string"}},"required":["script"]})));
        tools.push(ToolSpec::new("ask_user", "Ask the user a question, optionally offering up to 4 short options to pick from. Use only when you genuinely need a decision before continuing.")
            .params(serde_json::json!({"type":"object","properties":{"question":{"type":"string"},"options":{"type":"array","items":{"type":"string"}}},"required":["question"]})));
        tools.push(ToolSpec::new("web_search", "Search the web (DuckDuckGo). Returns ranked results with title, URL, and snippet.")
            .params(serde_json::json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"]})));
        tools.push(ToolSpec::new("fetch_url", "Fetch a web page and return its readable text content (HTML stripped).")
            .params(serde_json::json!({"type":"object","properties":{"url":{"type":"string"}},"required":["url"]})));
        tools.push(ToolSpec::new("codebase_search", "Find code relevant to a natural-language query — fast indexed retrieval (TF-IDF + symbol-aware) across the workspace. Use this FIRST to locate where something is implemented when you don't know the file; prefer it over a broad `search`. Then read only the top file(s).")
            .params(serde_json::json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"]})));
        tools.push(ToolSpec::new("session_search", "Recall PAST Oxide conversations (other sessions). With `query`: find sessions that discussed a topic — returns a snippet, the messages around the match, and how each session began/ended. With `session_id`: read that session. With no args: list recent sessions. Use when the user references earlier work (\"like we did before\", \"the bug from last week\").")
            .params(serde_json::json!({"type":"object","properties":{
                "query":{"type":"string","description":"topic to find across past sessions"},
                "session_id":{"type":"string","description":"read this specific session instead of searching"}
            }})));
        tools.push(ToolSpec::new("delegate_task", "Delegate a SELF-CONTAINED subtask (research, review, exploration) to a background subagent and keep working. Returns a handle immediately; the result re-enters the conversation automatically once you are idle. Use for work that doesn't block your current step. Max 3 concurrent.")
            .params(serde_json::json!({"type":"object","properties":{
                "task":{"type":"string","description":"complete, standalone instructions for the subagent"},
                "profile":{"type":"string","enum":["explorer","reviewer","tester","implementer"],"description":"subagent specialization (default chosen from the task)"}
            },"required":["task"]})));
        tools.push(ToolSpec::new("execute_code", "Run a Python 3 script that calls tools PROGRAMMATICALLY — for loops/batches over many reads or searches without a model round-trip per call (e.g. read 30 files and count matches). Inside the script call oxide_call('read_file', {'path': ...}) or oxide_call('search', {'query': ...}); both are read-only and workspace-sandboxed. ONLY stdout returns (print what you need), capped at 50KB; max 50 tool calls; 5 minute timeout. Not for editing files or running shell commands — use edit/shell for that.")
            .params(serde_json::json!({"type":"object","properties":{
                "code":{"type":"string","description":"Python 3 source; oxide_call(name, args) is predefined"}
            },"required":["code"]})).mutating(true));
        tools.push(ToolSpec::new("todo_write", "Maintain a short task checklist for non-trivial multi-step work (>2 edits or multiple files/subsystems). Skip it for simple tasks. Call with the FULL list each time; keep exactly one task 'in_progress' and mark tasks 'completed' as you finish.")
            .params(serde_json::json!({
                "type":"object",
                "properties":{"todos":{"type":"array","items":{"type":"object","properties":{
                    "content":{"type":"string"},
                    "status":{"type":"string","enum":["pending","in_progress","completed"]}
                },"required":["content","status"]}}},
                "required":["todos"]
            })));
        tools.push(ToolSpec::new("render_ui_spec", "Render a constrained Rust-native UI artifact in the chat. Use for dashboards, metrics, tables, status panels, and summaries that are clearer as structured UI than markdown. The spec must use Oxide's fixed catalog: stack, row, card, text, metric, table, code, alert, divider, action, chart (sparkline via props.points), input, select (props.options). Action buttons submit BACK to you with any sibling input/select values filled in by the user.")
            .params(ui_spec_tool_params()));
        tools.push(ToolSpec::new("design_read_system", "Read and validate the workspace DESIGN.md contract. Returns parsed section completeness and a Rust-native token contract.")
            .params(serde_json::json!({"type":"object","properties":{"path":{"type":"string","description":"Optional design system path. Defaults to DESIGN.md."}}})));
        tools.push(ToolSpec::new("design_extract_tokens", "Extract a Rust-native design token contract from DESIGN.md/CSS text. Use before proposing visual changes so edits align with existing tokens.")
            .params(serde_json::json!({
                "type":"object",
                "additionalProperties":false,
                "properties":{
                    "content":{"type":"string","description":"DESIGN.md or CSS text to inspect."},
                    "source":{"type":"string","description":"Source label for token evidence."}
                },
                "required":["content"]
            })));
        tools.push(ToolSpec::new("design_snapshot", "Ask the frontend Design Workbench to capture or focus a visual target for element inspection.")
            .mutating(true)
            .params(serde_json::json!({
                "type":"object",
                "additionalProperties":false,
                "properties":{"url":{"type":"string"},"note":{"type":"string"}},
                "required":["url"]
            })));
        tools.push(ToolSpec::new("design_review", "Run deterministic checks on a selected Design Workbench element and pending edits. Emits a typed design review event.")
            .params(design_review_tool_params()));
        tools.push(ToolSpec::new("design_propose_patch", "Convert a selected Design Workbench element and edits into a typed patch proposal. This does not edit files; use it before source-code changes.")
            .params(design_patch_tool_params()));
        tools.extend(git_tools::specs());
        tools.retain(|tool| policy.allows(&tool.name, declared.contains(&tool.name)));
        let mut seen = std::collections::BTreeSet::new();
        tools.retain(|tool| seen.insert(tool.name.clone()));
        tools
    }

    fn tools_for_worker_profile(&self, profile: &WorkerProfile) -> Vec<ToolSpec> {
        let tools = self.all_tools();
        if profile.toolset == WorkerToolset::Full {
            return tools;
        }
        tools
            .into_iter()
            .filter(|tool| profile.toolset.allows(&tool.name))
            .collect()
    }

    /// Finish browser automation with the turn that owns it. This keeps one
    /// browser alive for multi-step navigate/read/click flows, then releases the
    /// Chromium process and temporary profile before the frontend sees Done.
    async fn close_browser(&mut self) {
        if let Some(session) = self.browser.take() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(8), session.close()).await;
        }
    }

    async fn finish_turn(&mut self, turn: TurnId) {
        self.close_browser().await;
        self.emit(Event::TurnFinished { turn }).await;
    }

    /// Ensure the browser session is launched; returns a ref or an error string.
    async fn ensure_browser(&mut self) -> Result<&browser::BrowserSession, String> {
        if self.browser.is_none() {
            match browser::BrowserSession::launch(self.config.browser_headless).await {
                Ok(s) => self.browser = Some(s),
                Err(e) => {
                    return Err(format!(
                        "browser launch failed: {e} (is a Chromium-based browser installed?)"
                    ))
                }
            }
        }
        Ok(self.browser.as_ref().unwrap())
    }

    /// Handle a `browser_*` tool. Returns Some((output, ok)) if it was one.
    async fn handle_browser_tool(
        &mut self,
        name: &str,
        args: &serde_json::Value,
    ) -> Option<(String, bool)> {
        if !matches!(
            name,
            "browser_navigate"
                | "browser_read"
                | "browser_click"
                | "browser_type"
                | "browser_screenshot"
                | "browser_eval"
        ) {
            return None;
        }
        let shots_dir = self.workspace.join(".oxide/screenshots");
        let sess = match self.ensure_browser().await {
            Ok(s) => s,
            Err(e) => return Some((e, false)),
        };
        let sa = |k: &str| args[k].as_str().unwrap_or("").to_string();
        // Each browser operation gets a hard outer cap so a stalled CDP call
        // (hung navigation, click on a removed element, etc.) can't freeze the
        // engine. The per-navigate timeout in browser.rs covers most hangs, but
        // screenshot and eval can also block on heavy pages.
        let op_future = async {
            match name {
                "browser_navigate" => sess.navigate(&sa("url")).await,
                "browser_read" => sess.read_text().await,
                "browser_click" => sess.click(&sa("selector")).await,
                "browser_type" => sess.type_text(&sa("selector"), &sa("text")).await,
                "browser_screenshot" => sess.screenshot(&shots_dir).await,
                "browser_eval" => sess.eval(&sa("script")).await,
                _ => Err(anyhow::anyhow!("unknown browser tool {name}")),
            }
        };
        let res = tokio::time::timeout(std::time::Duration::from_secs(45), op_future).await;
        Some(match res {
            Ok(Ok(out)) => (out, true),
            Ok(Err(e)) => (format!("browser error: {e}"), false),
            Err(_) => (
                "browser timeout after 45s — page may be slow or stalled".to_string(),
                false,
            ),
        })
    }

    /// Launch trusted/configured MCP servers and merge their tools. External
    /// Codex/Claude MCP servers are detected for the UI, but not auto-connected
    /// until the user adds/trusts them in Oxide's config.
    async fn connect_mcp_servers(&mut self) {
        // Process-wide pool of live MCP connections, keyed by server config.
        // Tab switches respawn the engine; without this every switch paid the
        // full reconnect (npx cold start) for every server.
        type Pool = std::collections::HashMap<
            String,
            (std::sync::Arc<McpClient>, Vec<oxide_protocol::ToolSpec>),
        >;
        static MCP_POOL: std::sync::OnceLock<tokio::sync::Mutex<Pool>> = std::sync::OnceLock::new();
        let pool = MCP_POOL.get_or_init(Default::default);

        self.required_mcp_unavailable = false;
        let configured = self.config.mcp_servers.clone();
        let discovered = discover_external_mcp_for_workspace(&self.workspace);
        let configured_names: HashSet<String> = configured
            .iter()
            .map(|server| server.name.clone())
            .collect();
        for ext in &discovered {
            if !configured_names.contains(&ext.name) {
                let source = if ext.source.is_empty() {
                    "external config"
                } else {
                    ext.source.as_str()
                };
                self.emit(Event::McpServerStatus {
                    name: ext.name.clone(),
                    status: "untrusted".to_string(),
                    tool_count: 0,
                    tools: Vec::new(),
                    detail: format!(
                        "detected from {source}; add/trust it in MCP settings to connect"
                    ),
                })
                .await;
            }
        }
        let mut servers = Vec::with_capacity(configured.len());
        for server in configured {
            if let Some(resolved) = resolve_external_mcp_reference(&server, &discovered) {
                servers.push(resolved);
            } else if server.enabled {
                if server.required {
                    self.required_mcp_unavailable = true;
                }
                let detail = format!(
                    "trusted external config from '{}' is no longer available",
                    if server.source.is_empty() {
                        "external config"
                    } else {
                        server.source.as_str()
                    }
                );
                self.emit(Event::McpServerStatus {
                    name: server.name.clone(),
                    status: "error".to_string(),
                    tool_count: 0,
                    tools: Vec::new(),
                    detail: detail.clone(),
                })
                .await;
                self.emit(Event::Error {
                    message: format!("mcp '{}' {detail}", server.name),
                })
                .await;
            }
        }
        let active_pool_keys: HashSet<String> = servers
            .iter()
            .filter(|server| server.enabled)
            .map(mcp_pool_key)
            .collect();
        pool.lock()
            .await
            .retain(|key, _| active_pool_keys.contains(key));
        // Connect all servers CONCURRENTLY with a hard per-server deadline —
        // sequential 60s connects to stale npx servers used to make the engine
        // ignore the first user message for minutes.
        let conn_futs = servers
            .iter()
            .filter(|s| s.enabled)
            .map(|srv| {
                let srv = srv.clone();
                async move {
                    let key = mcp_pool_key(&srv);
                    // Reuse a live pooled connection when it still answers.
                    if let Some((client, _cached_tools)) = pool.lock().await.get(&key).cloned() {
                        let refreshed = tokio::time::timeout(
                            std::time::Duration::from_secs(5),
                            client.list_tools(),
                        )
                        .await;
                        if let Ok(Ok(tools)) = refreshed {
                            let tools = filter_mcp_tools(&srv, tools);
                            return (srv, Ok((client, tools)));
                        }
                        pool.lock().await.remove(&key);
                    }
                    let fut = async {
                        let client = if !srv.url.is_empty() {
                            McpClient::connect_http_with(
                                &srv.name,
                                &srv.url,
                                mcp_http_options(&srv),
                            )
                            .await?
                        } else {
                            let cwd = if srv.cwd.trim().is_empty() {
                                None
                            } else {
                                Some(std::path::PathBuf::from(&srv.cwd))
                            };
                            McpClient::connect_stdio_with(
                                &srv.name,
                                &srv.command,
                                &srv.args,
                                StdioSpawnOptions {
                                    cwd,
                                    env: srv.env.clone(),
                                    env_vars: srv
                                        .env_vars
                                        .iter()
                                        .map(|env| env.name().to_string())
                                        .collect(),
                                    request_timeout: duration_secs(srv.tool_timeout_sec, 60),
                                },
                            )
                            .await?
                        };
                        let tools = filter_mcp_tools(&srv, client.list_tools().await?);
                        Ok::<_, anyhow::Error>((std::sync::Arc::new(client), tools))
                    };
                    let res =
                        match tokio::time::timeout(duration_secs(srv.startup_timeout_sec, 15), fut)
                            .await
                        {
                            Ok(r) => r,
                            Err(_) => Err(anyhow::anyhow!(
                                "timed out after {}s",
                                duration_secs(srv.startup_timeout_sec, 15).as_secs()
                            )),
                        };
                    if let Ok((client, tools)) = &res {
                        pool.lock()
                            .await
                            .insert(key, (client.clone(), tools.clone()));
                    }
                    (srv, res)
                }
            })
            .collect::<Vec<_>>();
        for (srv, connect) in futures::future::join_all(conn_futs).await {
            match connect.map(|(c, t)| {
                let instructions = c.instructions().to_string();
                self.mcp_clients.push(c);
                (t, instructions)
            }) {
                Ok((tools, instructions)) => {
                    let tool_names = tools.iter().map(|tool| tool.name.clone()).collect();
                    self.emit(Event::McpServerStatus {
                        name: srv.name.clone(),
                        status: "connected".to_string(),
                        tool_count: tools.len(),
                        tools: tool_names,
                        detail: "tools/list succeeded".to_string(),
                    })
                    .await;
                    self.emit(Event::Info {
                        text: format!("mcp '{}' connected: {} tool(s)", srv.name, tools.len()),
                    })
                    .await;
                    self.mcp_tools.extend(tools);
                    if !instructions.trim().is_empty() {
                        self.mcp_instructions.push((srv.name.clone(), instructions));
                    }
                }
                Err(e) => {
                    if srv.required {
                        self.required_mcp_unavailable = true;
                    }
                    self.emit(Event::McpServerStatus {
                        name: srv.name.clone(),
                        status: "error".to_string(),
                        tool_count: 0,
                        tools: Vec::new(),
                        detail: format!("connect failed: {e}"),
                    })
                    .await;
                    self.emit(Event::Error {
                        message: if srv.required {
                            format!("required mcp '{}' connect failed: {e}", srv.name)
                        } else {
                            format!("mcp '{}' connect failed: {e}", srv.name)
                        },
                    })
                    .await;
                }
            }
        }
    }

    /// Fire lifecycle hooks for `event`. Returns true if a `pre_tool` hook
    /// blocked (non-zero exit). Payload JSON is passed via `$OXIDE_HOOK_PAYLOAD`.
    async fn fire_hooks(&self, event: &str, matcher: &str, payload: serde_json::Value) -> bool {
        let hooks = hooks::Hooks::load(&self.workspace);
        self.fire_hook_commands(&hooks, event, matcher, payload)
            .await
    }

    async fn fire_hook_commands(
        &self,
        hooks: &hooks::Hooks,
        event: &str,
        matcher: &str,
        payload: serde_json::Value,
    ) -> bool {
        let mut blocked = false;
        let audit_turn = payload.get("turn").and_then(|v| v.as_u64()).map(TurnId);
        for hook in hooks.commands_for(event, matcher) {
            if !hook.status_message.is_empty() {
                self.emit(Event::Info {
                    text: hook.status_message.clone(),
                })
                .await;
            }
            if hook.background {
                let command = hook.command.clone();
                let workspace = self.workspace.clone();
                let payload = payload.clone();
                let event_name = event.to_string();
                let matcher = matcher.to_string();
                let timeout = hook.timeout;
                let event_tx = self.event_tx.clone();
                tokio::spawn(async move {
                    let ok = run_hook_command(
                        &workspace,
                        &command,
                        &event_name,
                        &matcher,
                        payload,
                        timeout,
                    )
                    .await;
                    let _ = event_tx
                        .send(Event::AuditLog {
                            turn: audit_turn,
                            kind: "hook".to_string(),
                            title: format!("Hook {event_name}"),
                            detail: command.clone(),
                            status: if ok {
                                "done".to_string()
                            } else {
                                "failed".to_string()
                            },
                        })
                        .await;
                    let _ = event_tx
                        .send(Event::HookFired {
                            hook: event_name,
                            command,
                            blocked: false,
                        })
                        .await;
                });
                self.emit_audit(
                    audit_turn,
                    "hook",
                    format!("Hook {event}"),
                    hook.command.clone(),
                    "background",
                )
                .await;
                self.emit(Event::HookFired {
                    hook: event.to_string(),
                    command: hook.command.clone(),
                    blocked: false,
                })
                .await;
                continue;
            }
            let fut = hook_command(
                &self.workspace,
                &hook.command,
                event,
                matcher,
                payload.clone(),
            );
            // A hook must never wedge the agent — bound it, then kill on drop.
            let status =
                tokio::time::timeout(std::time::Duration::from_secs(hook.timeout), fut).await;
            let ok = matches!(&status, Ok(Ok(o)) if o.status.success());
            let this_blocked = event == "pre_tool" && !ok;
            if this_blocked {
                blocked = true;
            }
            self.emit_audit(
                audit_turn,
                "hook",
                format!("Hook {event}"),
                hook.command.clone(),
                if this_blocked {
                    "blocked"
                } else if ok {
                    "done"
                } else {
                    "failed"
                },
            )
            .await;
            self.emit(Event::HookFired {
                hook: event.to_string(),
                command: hook.command.clone(),
                blocked: this_blocked,
            })
            .await;
        }
        blocked
    }

    async fn run_stop_lifecycle(&self, turn: TurnId, user_text: &str, interrupted: bool) {
        let hooks = hooks::Hooks::load(&self.workspace);
        let payload = serde_json::json!({
            "turn": turn.0,
            "interrupted": interrupted,
            "workspace": self.workspace.display().to_string(),
            "edited_paths": self.turn_edit_paths.clone(),
            "user_text": user_text,
        });
        self.run_auto_lint(&hooks, turn).await;
        self.write_turn_summary(&hooks, turn, user_text, interrupted)
            .await;
        self.fire_hook_commands(&hooks, "stop", "", payload).await;
    }

    async fn run_auto_lint(&self, hooks: &hooks::Hooks, turn: TurnId) {
        if !hooks.auto().lint || self.turn_edit_paths.is_empty() {
            return;
        }
        let command = if hooks.auto().lint_command.trim().is_empty() {
            match self.default_lint_command() {
                Some(command) => command,
                None => return,
            }
        } else {
            hooks.auto().lint_command.clone()
        };
        self.emit(Event::Info {
            text: format!("auto-lint: {command}"),
        })
        .await;
        self.emit_audit(
            Some(turn),
            "lint",
            "Auto lint started",
            command.clone(),
            "running",
        )
        .await;
        let fut = tokio::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&command)
            .current_dir(&self.workspace)
            .output();
        let out = match tokio::time::timeout(std::time::Duration::from_secs(180), fut).await {
            Ok(Ok(output)) => output,
            Ok(Err(err)) => {
                self.emit_audit(
                    Some(turn),
                    "lint",
                    "Auto lint failed",
                    err.to_string(),
                    "failed",
                )
                .await;
                self.emit(Event::Error {
                    message: format!("auto-lint spawn failed: {err}"),
                })
                .await;
                return;
            }
            Err(_) => {
                self.emit_audit(
                    Some(turn),
                    "lint",
                    "Auto lint timed out",
                    "after 180s",
                    "failed",
                )
                .await;
                self.emit(Event::Error {
                    message: "auto-lint timed out after 180s".into(),
                })
                .await;
                return;
            }
        };
        let mut text = String::from_utf8_lossy(&out.stdout).to_string();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        let text: String = text.trim().chars().take(4000).collect();
        if out.status.success() {
            self.emit_audit(Some(turn), "lint", "Auto lint passed", command, "done")
                .await;
            self.emit(Event::Info {
                text: "auto-lint passed".into(),
            })
            .await;
        } else {
            self.emit_audit(
                Some(turn),
                "lint",
                "Auto lint failed",
                text.clone(),
                "failed",
            )
            .await;
            self.emit(Event::Error {
                message: if text.is_empty() {
                    "auto-lint failed".into()
                } else {
                    format!("auto-lint failed:\n{text}")
                },
            })
            .await;
        }
    }

    fn default_lint_command(&self) -> Option<String> {
        let ws = &self.workspace;
        if ws.join("Cargo.toml").exists() {
            Some("cargo check --message-format short".into())
        } else if ws.join("package.json").exists() {
            Some("npm run lint --if-present".into())
        } else if ws.join("pyproject.toml").exists() || ws.join("requirements.txt").exists() {
            Some("ruff check .".into())
        } else {
            None
        }
    }

    async fn write_turn_summary(
        &self,
        hooks: &hooks::Hooks,
        turn: TurnId,
        user_text: &str,
        interrupted: bool,
    ) {
        if !hooks.auto().summarize {
            return;
        }
        let dir = self.workspace.join(".oxide/turn-summaries");
        if let Err(err) = std::fs::create_dir_all(&dir) {
            self.emit(Event::Error {
                message: format!("turn summary mkdir failed: {err}"),
            })
            .await;
            return;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let edited = if self.turn_edit_paths.is_empty() {
            "- none".to_string()
        } else {
            self.turn_edit_paths
                .iter()
                .map(|path| format!("- {path}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let body = format!(
            "# Turn {}\n\n- timestamp: {now}\n- interrupted: {interrupted}\n\n## Request\n{}\n\n## Edited Paths\n{edited}\n",
            turn.0,
            user_text.trim()
        );
        let path = dir.join(format!("turn-{}-{now}.md", turn.0));
        match std::fs::write(&path, body) {
            Ok(()) => {
                self.emit(Event::Info {
                    text: format!("turn summary saved: {}", path.display()),
                })
                .await
            }
            Err(err) => {
                self.emit(Event::Error {
                    message: format!("turn summary write failed: {err}"),
                })
                .await
            }
        }
    }

    async fn run_cli_self_improvement_bridge(
        &self,
        turn: TurnId,
        source_provider: &str,
        user_text: &str,
        assistant_text: &str,
        edited_paths: &[String],
    ) {
        if !matches!(source_provider, "codex" | "claude" | "claude_interactive")
            || edited_paths.is_empty()
            || assistant_text.trim().is_empty()
        {
            return;
        }

        self.emit_audit(
            Some(turn),
            "memory",
            "CLI self-improvement",
            format!(
                "Analyzing {} edited path(s) for durable learning",
                edited_paths.len()
            ),
            "running",
        )
        .await;

        let memory_store = memory::Memory::new(&self.workspace);
        let existing_memory = compact_chars(&memory_store.load_block(), 4000);
        let edited = edited_paths
            .iter()
            .take(20)
            .map(|path| format!("- {path}"))
            .collect::<Vec<_>>()
            .join("\n");
        let system = "\
You are Oxide's post-turn self-improvement bridge for native CLI providers.
Inspect one completed coding-agent turn and decide whether anything durable should be stored in project memory.
Return ONLY compact JSON with this exact shape:
{\"facts\":[\"...\"],\"skills\":[{\"name\":\"short-slug\",\"content\":\"# Title\\nReusable steps...\"}]}
Rules:
- Save only durable, non-obvious project quirks, user preferences, gotchas, or reusable procedures.
- Do not save temporary task status, release numbers, generic advice, or a plain list of edited files.
- Do not duplicate existing memory.
- Do not include secrets, tokens, keys, passwords, or private credentials.
- Use at most 3 facts and at most 1 skill.
- If nothing qualifies, return {\"facts\":[],\"skills\":[]}.";
        let user = format!(
            "Workspace: {}\nProvider: {source_provider}\n\nExisting memory:\n{}\n\nUser request:\n{}\n\nEdited paths:\n{}\n\nCLI final answer:\n{}",
            self.workspace.display(),
            if existing_memory.trim().is_empty() { "(none)" } else { existing_memory.trim() },
            compact_chars(user_text.trim(), 4000),
            edited,
            compact_chars(assistant_text.trim(), 6000),
        );
        let req = TurnRequest {
            model: String::new(),
            reasoning_effort: "low".to_string(),
            temperature: 0.0,
            messages: vec![
                Message::new(Role::System, system.to_string()),
                Message::new(Role::User, user),
            ],
            tools: Vec::new(),
            cwd: self.workspace.display().to_string(),
            conversation_id: self
                .session_store
                .as_ref()
                .map(|s| format!("{}:cli-self-improve:{}", s.id, turn.0))
                .unwrap_or_else(|| format!("cli-self-improve:{}", turn.0)),
            cli_resume: None,
            system_append: None,
            claude_agents: None,
        };

        let raw = match collect_provider_text_silent("chatgpt", req).await {
            Ok(text) if !text.trim().is_empty() => text,
            Ok(_) => {
                self.emit_audit(
                    Some(turn),
                    "memory",
                    "CLI self-improvement skipped",
                    "empty bridge response",
                    "skipped",
                )
                .await;
                return;
            }
            Err(err) => {
                self.emit_audit(
                    Some(turn),
                    "memory",
                    "CLI self-improvement unavailable",
                    err,
                    "skipped",
                )
                .await;
                return;
            }
        };

        let Some(capture) = parse_self_improvement_capture(&raw) else {
            self.emit_audit(
                Some(turn),
                "memory",
                "CLI self-improvement parse failed",
                compact_chars(raw.trim(), 1200),
                "failed",
            )
            .await;
            return;
        };

        let existing_lower = existing_memory.to_ascii_lowercase();
        let mut saved_facts = 0usize;
        let mut saved_skills = 0usize;
        let mut errors = Vec::new();
        for fact in capture.facts.into_iter().take(3) {
            let fact = clean_memory_fact(&fact);
            if fact.is_empty()
                || looks_like_secret(&fact)
                || existing_lower.contains(&fact.to_ascii_lowercase())
            {
                continue;
            }
            match memory_store.remember(&fact) {
                Ok(()) => saved_facts += 1,
                Err(err) => errors.push(format!("remember failed: {err}")),
            }
        }
        for skill in capture.skills.into_iter().take(1) {
            let name = skill.name.trim();
            let content = skill.content.trim();
            if name.is_empty() || content.len() < 20 || looks_like_secret(content) {
                continue;
            }
            match memory_store.save_skill(name, content) {
                Ok(()) => saved_skills += 1,
                Err(err) => errors.push(format!("save_skill failed: {err}")),
            }
        }

        if !errors.is_empty() {
            self.emit_audit(
                Some(turn),
                "memory",
                "CLI self-improvement save failed",
                errors.join("\n"),
                "failed",
            )
            .await;
        } else if saved_facts == 0 && saved_skills == 0 {
            self.emit_audit(
                Some(turn),
                "memory",
                "CLI self-improvement skipped",
                "no durable learning selected",
                "skipped",
            )
            .await;
        } else {
            self.emit_audit(
                Some(turn),
                "memory",
                "CLI self-improvement saved",
                format!("facts: {saved_facts}, skills: {saved_skills}"),
                "done",
            )
            .await;
        }
    }

    /// Dispatch a namespaced MCP tool call to the owning server.
    async fn mcp_call(&self, name: &str, args: &serde_json::Value) -> (String, bool) {
        let Some(server) = server_of(name) else {
            return (format!("malformed mcp tool name '{name}'"), false);
        };
        let Some(client) = self.mcp_clients.iter().find(|c| c.server() == server) else {
            return (format!("no connected mcp server '{server}'"), false);
        };
        match client.call_tool(name, args).await {
            Ok((out, ok)) => (out, ok),
            Err(e) => (format!("mcp call error: {e}"), false),
        }
    }

    async fn run(mut self, mut op_rx: mpsc::Receiver<Op>) {
        self.provider = oxide_providers::build(&self.config.provider);
        self.emit(Event::Ready {
            harness: self.config.harness.clone(),
        })
        .await;
        if let Some(store) = &self.session_store {
            // Tell the UI exactly which file this engine writes, so a tab binds
            // to its OWN transcript (newest-file guessing mixes tabs up).
            self.emit(Event::SessionPath {
                path: store.path_str(),
            })
            .await;
            let resumed = if self.session.is_empty() {
                String::new()
            } else {
                format!(" (resumed {} msgs)", self.session.len())
            };
            self.emit(Event::Info {
                text: format!("session {}{}", store.id, resumed),
            })
            .await;
            // Background jobs a previous run of this session left behind: their
            // processes outlive the app, so re-surface any whose output file is
            // still fresh — the frontend re-attaches its tailer off this event.
            for (command_id, command, path) in replayable_bg_jobs(
                db::load(&store.id),
                std::time::Duration::from_secs(6 * 3600),
            ) {
                self.emit(Event::BackgroundJob {
                    turn: TurnId(0),
                    command_id,
                    command,
                    path,
                })
                .await;
            }
        }
        self.connect_mcp_servers().await;

        // Skill curator (mechanical, at most once per 24h): stale skills move
        // to archive/ so the system-prompt index only carries live knowledge.
        {
            let ws = self.workspace.clone();
            tokio::task::spawn_blocking(move || {
                let moved = memory::Memory::new(&ws).curate();
                if moved > 0 {
                    tracing::info!(moved, "skill curator archived stale skills");
                }
            });
        }

        // Background-delegation completions re-enter HERE, between turns only.
        let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<String>();
        self.bg_done_tx = Some(bg_tx);
        // Dispatcher: receives delegation requests from handle_tool_call and
        // runs the worker — spawned OUT HERE so the worker future is never
        // nested inside handle_tool_call's own future (Send-inference cycle).
        let (spawn_tx, mut spawn_rx) = mpsc::unbounded_channel::<BgDelegation>();
        self.bg_spawn_tx = Some(spawn_tx);
        tokio::spawn(async move {
            while let Some(d) = spawn_rx.recv().await {
                tokio::spawn(async move {
                    let BgDelegation {
                        mut worker,
                        system,
                        task,
                        worker_id,
                        profile,
                        handle,
                        done_tx,
                        counter,
                        notify,
                    } = d;
                    // Dummy op channel: background subagents aren't steerable.
                    let (_op_tx, mut op_rx2) = mpsc::channel::<Op>(8);
                    let (out, interrupted) = worker
                        .stream_agentic_collect(
                            &system,
                            &task,
                            TurnId(0),
                            &worker_id,
                            profile,
                            &mut op_rx2,
                            None,
                        )
                        .await;
                    counter.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    let status = if interrupted {
                        "was interrupted"
                    } else {
                        "finished"
                    };
                    if let Some(events) = notify {
                        // /btw: the answer goes straight to the user as a note —
                        // never re-enters the model loop.
                        let _ = events
                            .send(Event::Info {
                                text: format!("\u{1f4ac} btw \u{b7} {task}\n\n{out}"),
                            })
                            .await;
                    } else {
                        let _ = done_tx.send(format!(
                            "<system-reminder>\nBackground subagent {handle} {status}.\nTask: {task}\nResult:\n{out}\n\nIncorporate this into the ongoing work; tell the user anything important from it.\n</system-reminder>"
                        ));
                    }
                });
            }
        });

        loop {
            let op = tokio::select! {
                op = op_rx.recv() => match op { Some(op) => op, None => break },
                Some(done) = bg_rx.recv() => Op::UserTurn { text: done },
            };
            {
                match op {
                    Op::UserTurn { text } => {
                        if self.maybe_side_question(&text) {
                            self.emit(Event::Info {
                                text: "\u{1f4ac} btw \u{b7} answering on the side\u{2026}".into(),
                            })
                            .await;
                            continue;
                        }
                        if self.maybe_best_of(&text) {
                            self.emit(Event::Info {
                                text: "\u{1f3c6} best-of panel running \u{2014} parallel candidates + judge\u{2026}".into(),
                            })
                            .await;
                            continue;
                        }
                        // Capture the id this turn will use (run_turn reads self.next_turn
                        // then increments) so a panic anywhere in run_turn's deep call tree
                        // still emits a matching TurnFinished. The engine task is spawned
                        // bare with no JoinHandle awaited, so an unguarded unwind would kill
                        // it silently: the spinner streams forever AND every later prompt on
                        // this tab is dead (op_rx dropped). catch_unwind keeps run() alive;
                        // the next turn self-heals via sanitize_tool_pairs.
                        let turn = TurnId(self.next_turn);
                        if std::panic::AssertUnwindSafe(self.run_turn(text, &mut op_rx))
                            .catch_unwind()
                            .await
                            .is_err()
                        {
                            self.emit(Event::Error {
                                message: "internal error during turn (session still alive — retry)"
                                    .into(),
                            })
                            .await;
                            self.note_db_error_once().await;
                            self.finish_turn(turn).await;
                        } else {
                            // hermes-style background self-improvement: every few
                            // turns a detached reviewer distills durable facts/
                            // skills from the conversation (engine-loop providers
                            // only — CLI agents run their own memory nudges).
                            self.maybe_self_review();
                        }
                    }
                    Op::SetHarness { id } => self.set_harness(id).await,
                    Op::ReloadHarnesses => self.reload_harnesses().await,
                    Op::Interrupt => {
                        // No turn in flight here; nothing to interrupt.
                        self.emit(Event::Info {
                            text: "nothing to interrupt".into(),
                        })
                        .await;
                    }
                    Op::SubagentControl { worker_id, .. } => {
                        self.emit(Event::Info {
                            text: format!("no live sub-agent worker '{worker_id}'"),
                        })
                        .await;
                    }
                    Op::ApprovalResponse { .. } => { /* handled inline during a turn */ }
                    Op::QuestionAnswer { .. } => { /* handled inline during a turn */ }
                    Op::Rewind { checkpoint_id } => {
                        let restored = self.rewind_checkpoint(checkpoint_id).await;
                        self.emit(Event::RewindDone {
                            id: checkpoint_id,
                            restored,
                        })
                        .await;
                    }
                    Op::SetHistory { msgs } => {
                        self.session = msgs
                            .iter()
                            .map(|(r, c)| Message::new(role_from_str(r), c.clone()))
                            .collect();
                        if let Some(store) = &self.session_store {
                            store.set_runtime_config(
                                &self.config.provider,
                                &self.config.model,
                                &self.config.harness,
                                &self.config.reasoning_effort,
                            );
                            let meta = format!("provider={}", self.config.provider);
                            let mut full: Vec<(String, String)> = vec![("meta".into(), meta)];
                            full.extend(msgs.iter().cloned());
                            let _ = store.rewrite(&full);
                        }
                        self.emit(Event::Info {
                            text: "history trimmed".into(),
                        })
                        .await;
                    }
                    Op::Shutdown => {
                        // Kill this conversation's warm persistent-claude child (if
                        // any) — the static registry outlives the engine otherwise
                        // (Synara's equivalent: query.close() on session stop).
                        if self.config.provider == "claude" {
                            let conv = self
                                .session_store
                                .as_ref()
                                .map(|s| s.id.clone())
                                .unwrap_or_default();
                            oxide_providers::claude_persistent_close(
                                &conv,
                                &self.workspace.display().to_string(),
                            );
                        }
                        break;
                    }
                }
            }
        }
        self.emit(Event::Shutdown).await;
    }

    async fn reload_harnesses(&mut self) {
        match registry_from_config(&self.config) {
            Ok(registry) => {
                let count = registry.ids().len();
                self.registry = registry;
                let harness = self.active_harness();
                self.emit(Event::Info {
                    text: format!(
                        "Harness registry reloaded · {count} available · active: {} ({}) · source: {}",
                        harness.display_name(),
                        harness.id(),
                        harness.source()
                    ),
                })
                .await;
            }
            Err(error) => {
                self.emit(Event::Error {
                    message: format!("failed to reload harnesses: {error:#}"),
                })
                .await;
            }
        }
    }

    async fn set_harness(&mut self, id: String) {
        if self.registry.get(&id).is_none() {
            self.emit(Event::Error {
                message: format!("unknown harness '{id}'"),
            })
            .await;
            return;
        }
        self.config.harness = id.clone();
        if let Some(store) = &self.session_store {
            store.set_runtime_config(
                &self.config.provider,
                &self.config.model,
                &self.config.harness,
                &self.config.reasoning_effort,
            );
        }
        self.emit(Event::HarnessChanged { id }).await;
    }

    /// Drive a single turn: build request from harness + history, stream the
    /// model, forward deltas as events, and remain interruptible.
    /// Keep the session under budget by *summarizing* the oldest turns into one
    /// brief (preserving goal/decisions/files/state) instead of dropping them,
    /// so the agent can continue with relevant context intact.
    /// Aggressively trim history after a context-overflow error (drop-based,
    /// keep only the last few messages, half the normal budget).
    async fn force_compact(&mut self, turn: TurnId) {
        let _ = turn;
        const KEEP_RECENT: usize = 4;
        if self.session.len() <= KEEP_RECENT + 1 {
            return;
        }
        self.prune_tool_outputs();
        let budget = (self.budget() / 2).max(20_000);
        let dropped = context::compact(&mut self.session, budget, KEEP_RECENT);
        if dropped > 0 {
            self.emit(Event::Compacted {
                dropped,
                tokens: context::estimate_tokens(&self.session),
            })
            .await;
        }
    }

    /// Compaction budget: 75% of the model's reported context window
    /// (opencode-style), falling back to the configured token cap.
    fn budget(&self) -> u64 {
        match self.ctx_window {
            Some(w) if w > 0 => ((w as f64 * 0.75) as u64).max(20_000),
            _ => self.config.max_context_tokens,
        }
    }

    /// Replace the oldest tool outputs with placeholders once recent tool output
    /// exceeds a protected window — preserves the conversation flow (opencode's
    /// `prune`). Returns true if anything was pruned.
    fn prune_tool_outputs(&mut self) -> bool {
        const PRUNE_PROTECT_TOKENS: u64 = 40_000;
        let mut acc = 0u64;
        let mut pruned = false;
        for m in self.session.iter_mut().rev() {
            if !matches!(m.role, Role::Tool) {
                continue;
            }
            let tok = (m.content.len() / 4) as u64;
            if acc >= PRUNE_PROTECT_TOKENS {
                if m.content != "[tool output pruned to save context]" {
                    m.content = "[tool output pruned to save context]".to_string();
                    pruned = true;
                }
            } else {
                acc += tok;
            }
        }
        pruned
    }

    async fn compact_session(&mut self, turn: TurnId) {
        let budget = self.budget();
        if context::estimate_tokens(&self.session) <= budget {
            return;
        }
        // 1. Prune old tool outputs first (cheap, preserves the dialogue).
        if self.prune_tool_outputs() && context::estimate_tokens(&self.session) <= budget {
            self.emit(Event::Compacted {
                dropped: 0,
                tokens: context::estimate_tokens(&self.session),
            })
            .await;
            return;
        }
        const KEEP_RECENT: usize = 8;
        if self.session.len() <= KEEP_RECENT + 1 {
            // Too short to summarize usefully — fall back to a hard trim.
            let dropped = context::compact(&mut self.session, budget, KEEP_RECENT);
            if dropped > 0 {
                self.emit(Event::Compacted {
                    dropped,
                    tokens: context::estimate_tokens(&self.session),
                })
                .await;
            }
            return;
        }
        let split = self.session.len() - KEEP_RECENT;
        let old: Vec<Message> = self.session.drain(0..split).collect();
        let blob = old
            .iter()
            .map(|m| format!("{:?}: {}", m.role, m.content))
            .collect::<Vec<_>>()
            .join("\n\n");
        // Compaction runs silently (no status pill) — like the SDK's automatic
        // compaction. The post-fact Event::Compacted note is enough.
        let provider = self.config.provider.clone();
        let effort = self.config.reasoning_effort.clone();
        let sys = "You compress conversation history. Summarize the earlier conversation below into a concise but COMPLETE brief that lets the assistant continue seamlessly. Preserve: the user's goal/task, decisions made, files created/edited (with paths), commands run and key results, current state, and open TODOs. Terse bullet points. Output only the summary.";
        let summary = self
            .stream_collect(&provider, sys, &blob, &effort, turn, false, true)
            .await;
        let summary = if summary.trim().is_empty() {
            format!(
                "(summary unavailable; {} earlier messages folded)",
                old.len()
            )
        } else {
            summary
        };
        self.session.insert(
            0,
            Message::new(
                Role::Assistant,
                format!("## Summary of earlier conversation\n{summary}"),
            ),
        );
        if let Some(store) = &self.session_store {
            let _ = store.append("summary", &summary);
        }
        self.emit(Event::Compacted {
            dropped: old.len() as u64,
            tokens: context::estimate_tokens(&self.session),
        })
        .await;
    }

    /// Run one provider stream to completion, emitting its output (as the answer
    /// or as reasoning) and returning the accumulated text. Used by the
    /// orchestration pipeline (front planner to backend implementer).
    #[allow(clippy::too_many_arguments)]
    async fn stream_collect(
        &self,
        provider_id: &str,
        system: &str,
        user: &str,
        effort: &str,
        turn: TurnId,
        as_reasoning: bool,
        silent: bool,
    ) -> String {
        let req = TurnRequest {
            model: String::new(), // let each provider/CLI pick its own default
            reasoning_effort: effort.to_string(),
            temperature: 0.2,
            messages: vec![
                Message::new(Role::System, system.to_string()),
                Message::new(Role::User, user.to_string()),
            ],
            tools: vec![],
            cwd: self.workspace.display().to_string(),
            conversation_id: self
                .session_store
                .as_ref()
                .map(|s| s.id.clone())
                .unwrap_or_default(),
            cli_resume: None,
            system_append: None,
            claude_agents: None,
        };
        let (tx, mut rx) = mpsc::channel::<StreamItem>(STREAM_QUEUE);
        let provider = oxide_providers::build(provider_id);
        let task = tokio::spawn(async move { provider.stream(req, tx).await });
        let mut out = String::new();
        // Idle-timeout like run_turn — a stalled provider must not wedge
        // compaction/orchestration forever (this path can't be interrupted).
        let idle = idle_timeout_for(provider_id);
        let mut timed_out = false;
        while let Some(item) = match tokio::time::timeout(idle, rx.recv()).await {
            Ok(it) => it,
            Err(_) => {
                timed_out = true;
                task.abort();
                None
            }
        } {
            match item {
                StreamItem::FileChanged(_) => {}
                // Sub-agent background jobs aren't surfaced individually.
                StreamItem::BackgroundJob { .. } => {}
                StreamItem::TextDelta(t) => {
                    out.push_str(&t);
                    if silent {
                        // collected silently (sub-agent)
                    } else if as_reasoning {
                        self.emit(Event::ReasoningDelta { turn, text: t }).await;
                    } else {
                        self.emit(Event::AgentMessageDelta { turn, text: t }).await;
                    }
                }
                StreamItem::ReasoningDelta(t) => {
                    if !silent {
                        self.emit(Event::ReasoningDelta { turn, text: t }).await;
                    }
                }
                StreamItem::ToolInputDelta {
                    id,
                    name,
                    delta,
                    accumulated,
                } => {
                    if !silent {
                        self.emit(Event::ToolCallDelta {
                            turn,
                            call_id: id,
                            tool: name,
                            delta,
                            accumulated,
                        })
                        .await;
                    }
                }
                StreamItem::Notice(text) => {
                    self.emit(Event::Info { text }).await;
                }
                StreamItem::Usage {
                    input,
                    output,
                    context_window,
                    cached_input,
                    reasoning_output,
                    cost_usd,
                } => {
                    self.emit(Event::TokensUsed {
                        turn,
                        input,
                        output,
                        cost_usd,
                        cached_input,
                        reasoning_output,
                    })
                    .await;
                    if let Some(limit) = context_window {
                        self.emit(Event::ContextWindow { limit }).await;
                    }
                }
                StreamItem::RateLimit {
                    plan,
                    primary_pct,
                    secondary_pct,
                    primary_reset_s,
                    secondary_reset_s,
                } => {
                    self.emit(Event::RateLimit {
                        plan,
                        primary_pct,
                        secondary_pct,
                        primary_reset_s,
                        secondary_reset_s,
                    })
                    .await;
                }
                StreamItem::CommandStarted { .. }
                | StreamItem::CommandOutput { .. }
                | StreamItem::CommandFinished { .. } => {}
                StreamItem::ToolCall { .. } => {}
                StreamItem::ReasoningItem(_) => {}
                // Sub-agent / silent collection — don't let its CLI session id
                // overwrite the main session's stored link.
                StreamItem::CliSession(_) => {}
                StreamItem::Done => break,
            }
        }
        if timed_out {
            self.emit(Event::Error {
                message: format!("{provider_id}: stream timed out"),
            })
            .await;
        } else if let Ok(Err(e)) = task.await {
            self.emit(Event::Error {
                message: e.to_string(),
            })
            .await;
        }
        out
    }

    /// Run a backend worker to completion with the same tool loop as the main
    /// agent, but keep its transcript out of the parent conversation. Used by
    /// orchestrated implementers and sub-agents.
    #[allow(clippy::too_many_arguments)]
    async fn stream_agentic_collect(
        &mut self,
        system: &str,
        user: &str,
        turn: TurnId,
        worker_id: &str,
        profile: WorkerProfile,
        op_rx: &mut mpsc::Receiver<Op>,
        start_barrier: Option<Arc<Barrier>>,
    ) -> (String, bool) {
        let saved_session = std::mem::replace(
            &mut self.session,
            vec![Message::new(Role::User, user.to_string())],
        );
        let saved_turn_reads = std::mem::take(&mut self.turn_reads);
        let saved_turn_edit_paths = std::mem::take(&mut self.turn_edit_paths);
        let saved_turn_edited = self.turn_edited;
        let saved_last_tool_sig = std::mem::take(&mut self.last_tool_sig);
        let saved_last_tool_reps = self.last_tool_reps;
        let saved_tool_signatures = std::mem::take(&mut self.turn_tool_signatures);
        let saved_todos = std::mem::take(&mut self.turn_todos);

        self.turn_edited = false;
        self.last_tool_reps = 0;

        let profile_id = profile.id.clone();
        self.fire_hooks(
            "subagent_start",
            &profile_id,
            serde_json::json!({ "turn": turn.0, "worker_id": worker_id, "profile": profile_id.clone(), "assignment": user }),
        )
        .await;
        self.emit(Event::SubagentStarted {
            turn,
            worker_id: worker_id.to_string(),
            profile: profile_id.clone(),
            task: user.to_string(),
        })
        .await;
        self.emit_audit(
            Some(turn),
            "subagent",
            format!("Subagent started · {profile_id}"),
            user,
            "running",
        )
        .await;
        if let Some(barrier) = start_barrier {
            barrier.wait().await;
        }

        let policy = self.active_harness().loop_policy();
        let model = policy.model.clone().unwrap_or_else(|| {
            let mut cfg = self.config.clone();
            cfg.provider = profile.provider.clone();
            cfg.effective_model()
        });
        let configured_steps = profile
            .max_steps
            .min(policy.max_steps as usize)
            .clamp(1, 24);
        let mut tool_budget = AdaptiveToolBudget::new(configured_steps);
        let tools = self.tools_for_worker_profile(&profile);
        let cli_driver = matches!(
            profile.provider.as_str(),
            "codex" | "claude" | "claude_interactive"
        );
        let worker_system = format!(
            "{system}\n\n{}",
            worker_profile_system_block(&profile, tools.len())
        );
        let cli_baseline = if cli_driver {
            git_baseline_tree(&self.workspace).await
        } else {
            None
        };
        let mut cli_changed: Vec<String> = Vec::new();
        let mut out = String::new();
        let mut interrupted = false;
        let mut step = 0usize;
        let mut overflow_retries = 0u8;
        let mut budget_stop_reminded = false;

        loop {
            context::sanitize_tool_pairs(&mut self.session);
            let mut msgs = vec![Message::new(Role::System, worker_system.clone())];
            msgs.extend(self.session.iter().cloned());
            let conversation_id = self
                .session_store
                .as_ref()
                .map(|s| format!("{}:{worker_id}", s.id))
                .unwrap_or_else(|| worker_id.to_string());
            let req = TurnRequest {
                model: model.clone(),
                reasoning_effort: profile.effort.clone(),
                temperature: policy.temperature,
                messages: msgs,
                tools: tools.clone(),
                cwd: self.workspace.display().to_string(),
                conversation_id,
                cli_resume: None,
                system_append: None,
                claude_agents: None,
            };

            let (stream_tx, mut stream_rx) = mpsc::channel::<StreamItem>(STREAM_QUEUE);
            let provider = oxide_providers::build(&profile.provider);
            let stream_task = tokio::spawn(async move { provider.stream(req, stream_tx).await });

            let mut round_text = String::new();
            let mut pending_reasoning: Option<serde_json::Value> = None;
            let mut did_tool = false;
            let mut steered = false;
            loop {
                tokio::select! {
                    item = stream_rx.recv() => {
                        match item {
                            Some(StreamItem::TextDelta(t)) => {
                                round_text.push_str(&t);
                                out.push_str(&t);
                            }
                            Some(StreamItem::ReasoningDelta(t)) => {
                                self.emit(Event::ReasoningDelta { turn, text: t }).await;
                            }
                            Some(StreamItem::ReasoningItem(v)) => {
                                pending_reasoning = Some(v);
                            }
                            Some(StreamItem::ToolInputDelta { id, name, delta, accumulated }) => {
                                self.emit(Event::ToolCallDelta {
                                    turn,
                                    call_id: id,
                                    tool: name,
                                    delta,
                                    accumulated,
                                }).await;
                            }
                            Some(StreamItem::ToolCall { id, name, arguments }) => {
                                did_tool = true;
                                let prose = std::mem::take(&mut round_text);
                                let mut msg = Message::with_tool_call(
                                    prose,
                                    oxide_providers::ToolCall { id: id.clone(), name: name.clone(), arguments: arguments.clone() },
                                );
                                msg.reasoning_item = pending_reasoning.take();
                                self.session.push(msg);
                                    if self.handle_tool_call(turn, name, arguments, id, Some(worker_id), op_rx).await {
                                    interrupted = true;
                                    break;
                                }
                            }
                            Some(StreamItem::FileChanged(path)) => {
                                if !cli_changed.contains(&path) {
                                    cli_changed.push(path.clone());
                                }
                                let (rel, diff) = cli_file_diff(&self.workspace, &path).await;
                                if !diff.trim().is_empty() {
                                    self.emit(Event::FileDiff { turn, path: rel, diff, checkpoint: 0 }).await;
                                }
                            }
                                Some(StreamItem::Notice(text)) => {
                                    self.emit(Event::Info { text }).await;
                                }
                                Some(StreamItem::CommandStarted { id, command, cwd, background }) => {
                                    self.emit(Event::CommandStarted {
                                        turn,
                                        command_id: id,
                                        worker_id: Some(worker_id.to_string()),
                                        command,
                                        cwd: if cwd.is_empty() { self.workspace.display().to_string() } else { cwd },
                                        background,
                                    }).await;
                                }
                                Some(StreamItem::BackgroundJob { id, command, path }) => {
                                    // Persisted so reopening this session re-surfaces the
                                    // job — its process outlives this app run.
                                    if let Some(store) = &self.session_store {
                                        let _ = store.append(
                                            "bg_job",
                                            &serde_json::json!({"id": &id, "command": &command, "path": &path}).to_string(),
                                        );
                                    }
                                    self.emit(Event::BackgroundJob { turn, command_id: id, command, path }).await;
                                }
                                Some(StreamItem::CommandOutput { id, stream, chunk }) => {
                                    self.emit(Event::CommandOutput {
                                        turn,
                                        command_id: id,
                                        worker_id: Some(worker_id.to_string()),
                                        stream,
                                        chunk,
                                    }).await;
                                }
                                Some(StreamItem::CommandFinished { id, ok, exit_code, duration_ms }) => {
                                    self.emit(Event::CommandFinished {
                                        turn,
                                        command_id: id,
                                        worker_id: Some(worker_id.to_string()),
                                        ok,
                                        exit_code,
                                        duration_ms,
                                    }).await;
                                }
                                Some(StreamItem::Usage { input, output, context_window, cached_input, reasoning_output, cost_usd }) => {
                                    self.emit(Event::TokensUsed { turn, input, output, cost_usd, cached_input, reasoning_output }).await;
                                if let Some(limit) = context_window {
                                    self.ctx_window = Some(limit);
                                    self.emit(Event::ContextWindow { limit }).await;
                                }
                            }
                            Some(StreamItem::RateLimit { plan, primary_pct, secondary_pct, primary_reset_s, secondary_reset_s }) => {
                                self.emit(Event::RateLimit { plan, primary_pct, secondary_pct, primary_reset_s, secondary_reset_s }).await;
                            }
                            Some(StreamItem::CliSession(_)) => {
                                // Worker CLI sessions are intentionally isolated from
                                // the parent chat's persisted CLI resume id.
                            }
                            Some(StreamItem::Done) | None => break,
                        }
                    }
                    op = op_rx.recv() => {
                        match op {
                            Some(Op::Interrupt) => { interrupted = true; break; }
                            Some(Op::Shutdown) => { interrupted = true; break; }
                            Some(Op::UserTurn { text }) => {
                                self.session.push(Message::new(Role::User, text.clone()));
                                self.emit(Event::Info { text: format!("Steering worker: {text}") }).await;
                                steered = true;
                            }
                            Some(Op::Rewind { checkpoint_id }) => {
                                let restored = self.rewind_checkpoint(checkpoint_id).await;
                                self.emit(Event::RewindDone { id: checkpoint_id, restored }).await;
                            }
                            Some(other) => {
                                self.emit(Event::Info { text: format!("queued op ignored by worker: {other:?}") }).await;
                            }
                            None => { interrupted = true; break; }
                        }
                    }
                }
            }

            let stream_err = if interrupted {
                stream_task.abort();
                None
            } else {
                stream_task
                    .await
                    .ok()
                    .and_then(|r| r.err())
                    .map(|e| e.to_string())
            };
            if let Some(err) = &stream_err {
                let low = err.to_lowercase();
                let overflow = low.contains("context")
                    || low.contains("exceeds")
                    || low.contains("too long")
                    || low.contains("maximum")
                    || (low.contains("token") && low.contains("limit"));
                if overflow && round_text.is_empty() && overflow_retries < 2 {
                    overflow_retries += 1;
                    self.force_compact(turn).await;
                    self.emit(Event::Info {
                        text: "worker context full — compacted, retrying".into(),
                    })
                    .await;
                    continue;
                }
                self.emit(Event::Error {
                    message: err.clone(),
                })
                .await;
            }
            if !round_text.is_empty() {
                let mut msg = Message::new(Role::Assistant, round_text);
                msg.reasoning_item = pending_reasoning.take();
                self.session.push(msg);
            }

            if did_tool {
                step += 1;
            }
            if interrupted {
                break;
            }
            if did_tool {
                let completed_todos = self
                    .turn_todos
                    .iter()
                    .filter(|(_, status)| status == "completed")
                    .count();
                let pending_todos = self
                    .turn_todos
                    .iter()
                    .any(|(_, status)| status != "completed");
                let progress = tool_progress_score(
                    self.turn_tool_signatures.len(),
                    self.turn_reads.len(),
                    self.turn_edit_paths.len(),
                    completed_todos,
                    self.turn_verify_passed,
                );
                match tool_budget.after_tool_round(
                    step,
                    progress,
                    pending_todos,
                    self.turn_edited && !self.turn_verify_passed,
                ) {
                    ToolBudgetDecision::Continue => {}
                    ToolBudgetDecision::Extended { new_limit } => {
                        budget_stop_reminded = false;
                        self.session.push(Message::new(
                            Role::User,
                            format!(
                                "<system-reminder>\nTool budget extended adaptively to {new_limit} rounds because measurable progress is continuing. Finish only the remaining assignment and verification; avoid repeated exploration.\n</system-reminder>"
                            ),
                        ));
                        continue;
                    }
                    ToolBudgetDecision::Stop if !budget_stop_reminded => {
                        budget_stop_reminded = true;
                        self.session.push(Message::new(
                            Role::User,
                            "<system-reminder>\nNo new measurable progress justified another tool-budget extension. Do not call more tools. Summarize completed work, verification, and remaining gaps.\n</system-reminder>",
                        ));
                        continue;
                    }
                    ToolBudgetDecision::Stop => break,
                }
            }
            if cli_driver && !steered {
                break;
            }
            if !did_tool && !steered {
                break;
            }
        }

        let changed: Vec<String> = std::mem::take(&mut cli_changed);
        for path in changed {
            let (rel, diff) = cli_file_diff(&self.workspace, &path).await;
            if !diff.trim().is_empty() {
                self.turn_edited = true;
                if !self.turn_edit_paths.iter().any(|path| path == &rel) {
                    self.turn_edit_paths.push(rel.clone());
                }
                let mut checkpoint = 0u64;
                if let Some(tree) = &cli_baseline {
                    let prior = tokio::process::Command::new("git")
                        .arg("-C")
                        .arg(&self.workspace)
                        .args(["cat-file", "-p", &format!("{tree}:{rel}")])
                        .output()
                        .await
                        .ok()
                        .filter(|o| o.status.success())
                        .map(|o| o.stdout);
                    let abs = self.workspace.join(&rel);
                    checkpoint = self.snapshot_checkpoint_with(&abs, prior).await;
                    self.emit(Event::CheckpointCreated {
                        turn,
                        id: checkpoint,
                        label: format!("worker edit {rel}"),
                    })
                    .await;
                }
                self.emit(Event::FileDiff {
                    turn,
                    path: rel,
                    diff,
                    checkpoint,
                })
                .await;
            } else {
                self.emit(Event::FileDiff {
                    turn,
                    path: rel,
                    diff: String::new(),
                    checkpoint: 0,
                })
                .await;
            }
        }

        let worker_edited = self.turn_edited;
        let worker_edit_paths = std::mem::take(&mut self.turn_edit_paths);
        let worker_edit_paths_for_bridge = worker_edit_paths.clone();
        // Synara model: transkrip anak dipersist sebagai sesi first-class
        // ber-parent_id (bukan dibuang) sebelum session induk dipulihkan.
        let child_transcript = std::mem::replace(&mut self.session, saved_session);
        let child_session = self
            .persist_subagent_session(worker_id, &profile_id, user, &child_transcript)
            .unwrap_or_default();
        self.turn_reads = saved_turn_reads;
        self.turn_edit_paths = saved_turn_edit_paths;
        self.turn_edit_paths.extend(worker_edit_paths);
        self.turn_edited = saved_turn_edited || worker_edited;
        self.last_tool_sig = saved_last_tool_sig;
        self.last_tool_reps = saved_last_tool_reps;
        self.turn_tool_signatures = saved_tool_signatures;
        self.turn_todos = saved_todos;

        if interrupted {
            self.emit(Event::Info {
                text: "worker interrupted".into(),
            })
            .await;
        }

        if cli_driver && !interrupted {
            self.run_cli_self_improvement_bridge(
                turn,
                &profile.provider,
                user,
                &out,
                &worker_edit_paths_for_bridge,
            )
            .await;
        }

        self.fire_hooks(
            "subagent_stop",
            &profile_id,
            serde_json::json!({ "turn": turn.0, "worker_id": worker_id, "profile": profile_id.clone(), "interrupted": interrupted }),
        )
        .await;
        let summary = compact_chars(out.trim(), 900);
        self.emit(Event::SubagentFinished {
            turn,
            worker_id: worker_id.to_string(),
            profile: profile_id.clone(),
            task: user.to_string(),
            summary: summary.clone(),
            ok: !interrupted,
            session: child_session,
        })
        .await;
        self.emit_audit(
            Some(turn),
            "subagent",
            format!("Subagent finished · {profile_id}"),
            summary,
            if interrupted { "interrupted" } else { "done" },
        )
        .await;

        (out, interrupted)
    }

    /// Cursor `/btw`: answer a SIDE question in a detached read-only worker
    /// while the main task keeps running untouched. The answer returns as a
    /// transcript note (never re-enters the model loop). True = consumed.
    fn maybe_side_question(&mut self, text: &str) -> bool {
        let Some(q) = text.trim().strip_prefix("/btw") else {
            return false;
        };
        let q = q.trim().to_string();
        if q.is_empty() {
            return true;
        }
        let (worker, done_tx, spawn_tx) = match (
            self.subagent_worker_engine(),
            self.bg_done_tx.clone(),
            self.bg_spawn_tx.clone(),
        ) {
            (Ok(w), Some(d), Some(sp)) => (w, d, sp),
            _ => return false,
        };
        self.bg_task_seq += 1;
        let handle = format!("btw-{}", self.bg_task_seq);
        let profile = subagent_profile_for(
            &format!("explorer: {q}"),
            &self.config.provider,
            &self.config.reasoning_effort,
        );
        let system = "You answer ONE side question from the user while the main agent keeps working. \
Answer directly and concisely (a few sentences; read files only when truly needed). Do NOT edit anything."
            .to_string();
        let counter = self.bg_tasks_running.clone();
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let sent = spawn_tx
            .send(BgDelegation {
                worker: Box::new(worker),
                system,
                task: q,
                worker_id: format!("bgtask-{handle}"),
                profile,
                handle,
                done_tx,
                counter: counter.clone(),
                notify: Some(self.event_tx.clone()),
            })
            .is_ok();
        if !sent {
            counter.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
        sent
    }

    /// Cursor `/best-of-n` (MoA-lite): run the same task through N parallel
    /// read-only workers with DIFFERENT lenses, then a judge picks the best
    /// answer and synthesizes. Result returns as a transcript note. Detached —
    /// spawned from the main loop (never inside handle_tool_call: Send-cycle).
    fn maybe_best_of(&mut self, text: &str) -> bool {
        let Some(rest) = text.trim().strip_prefix("/bestof") else {
            return false;
        };
        let rest = rest.trim();
        let (n, task) = match rest.split_once(char::is_whitespace) {
            Some((num, t)) if num.chars().all(|c| c.is_ascii_digit()) && !num.is_empty() => (
                num.parse::<usize>().unwrap_or(3).clamp(2, 4),
                t.trim().to_string(),
            ),
            _ => (3usize, rest.to_string()),
        };
        if task.is_empty() {
            return true;
        }
        const LENSES: [&str; 4] = [
            "simplest-correct-answer-first",
            "risk-and-edge-cases-first",
            "performance-and-scalability-first",
            "developer-experience-first",
        ];
        let mut workers = Vec::new();
        for lens in LENSES.iter().take(n) {
            match self.subagent_worker_engine() {
                Ok(w) => workers.push((w, lens.to_string())),
                Err(_) => return false,
            }
        }
        let Ok(judge) = self.subagent_worker_engine() else {
            return false;
        };
        let profile = subagent_profile_for(
            &format!("explorer: {task}"),
            &self.config.provider,
            &self.config.reasoning_effort,
        );
        let judge_profile = subagent_profile_for(
            &format!("reviewer: {task}"),
            &self.config.provider,
            &self.config.reasoning_effort,
        );
        let events = self.event_tx.clone();
        self.bg_task_seq += 1;
        let seq = self.bg_task_seq;
        let counter = self.bg_tasks_running.clone();
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        tokio::spawn(async move {
            let futs: Vec<_> = workers
                .into_iter()
                .enumerate()
                .map(|(i, (mut w, lens))| {
                    let task = task.clone();
                    let profile = profile.clone();
                    async move {
                        let system = format!(
                            "You are candidate {i} in a best-of-N panel. Approach the task with the lens: {lens}. \
Produce a compact, self-contained answer (read files only when needed; do NOT edit anything)."
                        );
                        let (_tx, mut rx) = mpsc::channel::<Op>(8);
                        let (out, _) = w
                            .stream_agentic_collect(
                                &system,
                                &task,
                                TurnId(0),
                                &format!("bestof-{seq}-{i}"),
                                profile,
                                &mut rx,
                                None,
                            )
                            .await;
                        (lens, out)
                    }
                })
                .collect();
            let results = futures::future::join_all(futs).await;
            let mut candidates = String::new();
            for (i, (lens, out)) in results.iter().enumerate() {
                candidates.push_str(&format!("\n--- Candidate {i} (lens: {lens}) ---\n{out}\n"));
            }
            let judge_task = format!(
                "Task given to all candidates:\n{task}\n{candidates}\n\nJudge: pick the BEST candidate (say which and why in 1-2 sentences), then present the winning answer, improved with the best ideas from the others."
            );
            let mut judge = judge;
            let (_tx, mut rx) = mpsc::channel::<Op>(8);
            let (verdict, _) = judge
                .stream_agentic_collect(
                    "You are the judge of a best-of-N panel. Be decisive and concise.",
                    &judge_task,
                    TurnId(0),
                    &format!("bestof-{seq}-judge"),
                    judge_profile,
                    &mut rx,
                    None,
                )
                .await;
            counter.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            let _ = events
                .send(Event::Info {
                    text: format!("\u{1f3c6} best-of-{n} \u{b7} {task}\n\n{verdict}"),
                })
                .await;
        });
        true
    }

    /// Persist transkrip worker sebagai sesi anak ber-`parent_id` (Synara
    /// model: sub-agent = thread first-class yang bisa dibuka seperti sesi
    /// biasa). None bila induk tidak menyimpan sesi atau transkrip kosong.
    fn persist_subagent_session(
        &self,
        worker_id: &str,
        profile_id: &str,
        task: &str,
        transcript: &[Message],
    ) -> Option<String> {
        let parent = self.subagent_parent.clone()?;
        if transcript.len() <= 1 {
            return None; // hanya prompt penugasan — tidak ada isi untuk dibuka
        }
        let store = SessionStore::open_child(&self.workspace, worker_id).ok()?;
        store.set_runtime_config(
            &self.config.provider,
            &self.config.model,
            &self.config.harness,
            &self.config.reasoning_effort,
        );
        for m in transcript {
            let role = match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };
            let _ = store.append(role, &m.content);
        }
        crate::db::set_parent(&store.id, &parent);
        crate::db::set_title(
            &store.id,
            &format!("{profile_id}: {}", compact_chars(task, 48)),
        );
        Some(store.id.clone())
    }

    fn subagent_worker_engine(&self) -> anyhow::Result<Self> {
        let mut config = self.config.clone();
        config.persist = false;
        let registry = registry_from_config(&config)?;
        Ok(Self {
            config,
            registry,
            provider: oxide_providers::build("echo"),
            session: Vec::new(),
            next_turn: self.next_turn,
            next_approval: Arc::clone(&self.next_approval),
            session_approved: self.session_approved.clone(),
            workspace: self.workspace.clone(),
            session_store: None,
            // Worker mengingat sesi induk agar transkripnya bisa dipersist
            // sebagai sesi anak (worker bersarang mewarisi induk terdekat).
            subagent_parent: self
                .session_store
                .as_ref()
                .map(|s| s.id.clone())
                .or_else(|| self.subagent_parent.clone()),
            checkpoints: Arc::clone(&self.checkpoints),
            mcp_clients: self.mcp_clients.clone(),
            mcp_tools: self.mcp_tools.clone(),
            deferred_tools: Vec::new(),
            turns_since_review: 0,
            turn_verify_passed: false,
            pending_verify_cmds: std::collections::HashSet::new(),
            bg_done_tx: None,
            bg_spawn_tx: None,
            bg_tasks_running: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            bg_task_seq: 0,
            mcp_instructions: self.mcp_instructions.clone(),
            required_mcp_unavailable: self.required_mcp_unavailable,
            browser: None,
            ctx_window: self.ctx_window,
            read_files: self.read_files.clone(),
            turn_edited: false,
            turn_edit_paths: Vec::new(),
            turn_reads: HashSet::new(),
            last_tool_sig: String::new(),
            last_tool_reps: 0,
            turn_tool_signatures: HashSet::new(),
            turn_todos: Vec::new(),
            user_interrupted: self.user_interrupted,
            event_tx: self.event_tx.clone(),
            // Subagent shares the parent's bus so its events land in the same
            // seq'd log/stream as the parent run.
            bus: self.bus.clone(),
        })
    }

    async fn run_subagents_parallel(
        &mut self,
        assignments: Vec<SubagentAssignment>,
        turn: TurnId,
        op_rx: &mut mpsc::Receiver<Op>,
    ) -> Vec<SubagentRunResult> {
        let mut results = Vec::with_capacity(assignments.len());
        let mut spawn_items = Vec::with_capacity(assignments.len());
        for assignment in assignments {
            match self.subagent_worker_engine() {
                Ok(worker) => {
                    let (op_tx, op_rx) = mpsc::channel(OP_QUEUE);
                    spawn_items.push((assignment, worker, op_tx, op_rx));
                }
                Err(err) => {
                    self.emit(Event::Error {
                        message: format!("sub-agent worker init failed: {err}"),
                    })
                    .await;
                    results.push(SubagentRunResult {
                        index: assignment.index,
                        task: assignment.task,
                        output: String::new(),
                        interrupted: true,
                        edited: false,
                        edit_paths: Vec::new(),
                        read_files: HashSet::new(),
                        session_approved: HashSet::new(),
                    });
                }
            }
        }

        if spawn_items.is_empty() {
            results.sort_by_key(|result| result.index);
            return results;
        }

        let start_barrier = Arc::new(Barrier::new(spawn_items.len()));
        let mut worker_ops = Vec::with_capacity(spawn_items.len());
        let mut handles = FuturesUnordered::new();
        for (assignment, mut worker, op_tx, mut worker_op_rx) in spawn_items {
            let barrier = Arc::clone(&start_barrier);
            let worker_id_for_control = assignment.worker_id.clone();
            let profile_for_control = assignment.profile.id.clone();
            worker_ops.push((worker_id_for_control, profile_for_control, op_tx));
            handles.push(tokio::spawn(async move {
                let SubagentAssignment {
                    index,
                    task,
                    system,
                    worker_id,
                    profile,
                } = assignment;
                let (output, interrupted) = worker
                    .stream_agentic_collect(
                        &system,
                        &task,
                        turn,
                        &worker_id,
                        profile,
                        &mut worker_op_rx,
                        Some(barrier),
                    )
                    .await;
                SubagentRunResult {
                    index,
                    task,
                    output,
                    interrupted,
                    edited: worker.turn_edited,
                    edit_paths: worker.turn_edit_paths,
                    read_files: worker.read_files,
                    session_approved: worker.session_approved,
                }
            }));
        }

        let mut parent_ops_open = true;
        while !handles.is_empty() {
            tokio::select! {
                joined = handles.next() => {
                    match joined {
                        Some(Ok(result)) => results.push(result),
                        Some(Err(err)) => {
                            self.emit(Event::Error {
                                message: format!("sub-agent worker task failed: {err}"),
                            }).await;
                        }
                        None => break,
                    }
                }
                op = op_rx.recv(), if parent_ops_open => {
                    match op {
                        Some(Op::SubagentControl { worker_id, action }) => {
                            let mut delivered = false;
                            for (live_worker_id, profile, worker_tx) in &worker_ops {
                                if live_worker_id != &worker_id {
                                    continue;
                                }
                                delivered = true;
                                let (status, detail, routed) = match action.clone() {
                                    SubagentControlAction::Interrupt => (
                                        "interrupt_requested",
                                        "operator interrupted this worker".to_string(),
                                        Op::Interrupt,
                                    ),
                                    SubagentControlAction::Steer { text } => (
                                        "steer_sent",
                                        compact_chars(&text, 300),
                                        Op::UserTurn { text },
                                    ),
                                };
                                let _ = worker_tx.send(routed).await;
                                self.emit(Event::SubagentStatus {
                                    turn,
                                    worker_id: live_worker_id.clone(),
                                    profile: profile.clone(),
                                    status: status.to_string(),
                                    detail,
                                })
                                .await;
                            }
                            if !delivered {
                                self.emit(Event::Error {
                                    message: format!("no live sub-agent worker '{worker_id}'"),
                                }).await;
                            }
                        }
                        Some(op) => {
                            for (_, _, worker_tx) in &worker_ops {
                                let _ = worker_tx.send(op.clone()).await;
                            }
                        }
                        None => {
                            parent_ops_open = false;
                            for (_, _, worker_tx) in &worker_ops {
                                let _ = worker_tx.send(Op::Interrupt).await;
                            }
                        }
                    }
                }
            }
        }

        results.sort_by_key(|result| result.index);
        for result in &results {
            self.turn_edited |= result.edited;
            for path in &result.edit_paths {
                if !self.turn_edit_paths.iter().any(|existing| existing == path) {
                    self.turn_edit_paths.push(path.clone());
                }
            }
            self.read_files.extend(result.read_files.iter().cloned());
            self.session_approved
                .extend(result.session_approved.iter().cloned());
        }
        results
    }

    async fn run_turn(&mut self, user_text: String, op_rx: &mut mpsc::Receiver<Op>) {
        let turn = TurnId(self.next_turn);
        self.next_turn += 1;
        self.emit(Event::TurnStarted { turn }).await;
        self.emit(Event::TurnStatus {
            turn,
            state: "working".into(),
            detail: String::new(),
        })
        .await;
        if self.required_mcp_unavailable {
            self.emit(Event::Error {
                message: "required MCP server unavailable; fix MCP settings or disable required before starting a turn".to_string(),
            })
            .await;
            self.note_db_error_once().await;
            self.finish_turn(turn).await;
            return;
        }

        // Expand `/slash` commands from .oxide/commands/*.md before running.
        let user_text = if user_text.trim_start().starts_with('/') {
            match commands::expand(&self.workspace, &user_text) {
                Some(expanded) => {
                    self.emit(Event::Info {
                        text: format!("▷ ran command {}", user_text.trim()),
                    })
                    .await;
                    expanded
                }
                None => {
                    self.emit(Event::Info {
                        text: format!("unknown command: {}", user_text.trim()),
                    })
                    .await;
                    user_text
                }
            }
        } else {
            user_text
        };

        // A turn the user interrupted leaves stale plan/todo state behind — for
        // resumed CLI drivers (codex/claude) it lives in their own session, for
        // API providers in the partial history. Tell the model to drop it and
        // treat this message as the sole current instruction.
        let user_text = if self.user_interrupted {
            self.user_interrupted = false;
            format!(
                "[The previous task was interrupted by the user before it finished. \
Abandon any unfinished plan or todo list from earlier — do not resume it. \
Treat the following as the only current, top-priority instruction:]\n\n{user_text}"
            )
        } else {
            user_text
        };

        self.session
            .push(Message::new(Role::User, user_text.clone()));
        if let Some(store) = &self.session_store {
            let _ = store.append("user", &user_text);
        }

        // Keep the running history under budget — summarize, don't just drop.
        self.compact_session(turn).await;

        // Progressive tool disclosure (hermes tool_search): defer MCP schemas
        // when they'd eat >~10% of the context window on EVERY request.
        let tools = {
            let full = self.all_tools();
            let (mcp, mut rest): (Vec<ToolSpec>, Vec<ToolSpec>) =
                full.into_iter().partition(|t| is_mcp_tool(&t.name));
            let budget = self
                .ctx_window
                .map(|c| (c as usize).saturating_mul(4) / 10)
                .unwrap_or(80_000);
            if deferrable_schema_chars(&mcp) > budget {
                self.deferred_tools = mcp;
                rest.extend(tool_bridge_specs());
                rest
            } else {
                self.deferred_tools.clear();
                rest.extend(mcp);
                rest
            }
        };
        let mem_block = memory::Memory::new(&self.workspace).load_block();
        let (
            policy,
            tool_policy,
            mut sys,
            mut cli_system_append,
            cli_claude_agents,
            harness_status,
            route,
        ) = {
            let harness = self.active_harness();
            let route = selected_skill_route(harness, &user_text);
            let tool_policy = harness.tool_policy();
            let status = format!(
                "Harness · {} ({}) · source: {} · tools: {} · policy: {:?}",
                harness.display_name(),
                harness.id(),
                harness.source(),
                tools.len(),
                tool_policy.mode
            );
            (
                harness.loop_policy(),
                tool_policy,
                harness.system_prompt(),
                harness.cli_system_append(),
                harness.claude_agents(),
                status,
                route,
            )
        };
        if let Some(route) = &route {
            let rendered = render_skill_route(route);
            sys.push_str(&rendered);
            let cli = cli_system_append.get_or_insert_with(String::new);
            cli.push_str(&rendered);
        }
        if tool_policy.mode == ToolPolicyMode::Allowlist || !tool_policy.deny.is_empty() {
            let allowed = tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let denied = tool_policy.deny.join(", ");
            let cli = cli_system_append.get_or_insert_with(String::new);
            cli.push_str("\n\n# Oxide harness tool policy\nUse only these allowed capabilities: ");
            cli.push_str(&allowed);
            if !denied.is_empty() {
                cli.push_str(". Explicitly forbidden: ");
                cli.push_str(&denied);
            }
            cli.push('.');
        }
        let route_status = route
            .as_ref()
            .map(|route| format!(" · route: {}", route.id))
            .unwrap_or_default();
        self.emit(Event::Info {
            text: format!("{harness_status}{route_status}"),
        })
        .await;
        // Mirror the user's language. The codex/chatgpt backend (and most models)
        // default to English without this; the user's prompt language should win.
        sys.push_str(
            "\n\n# Language\nAlways reply in the SAME language the user writes in \
             (e.g. user writes Indonesian, reply in Indonesian). Match the user's \
             language for every response; do not switch to English on your own.",
        );
        // Tell the agent exactly where it is working so it never wanders to $HOME.
        let stack = detect_stack(&self.workspace);
        sys.push_str(&format!(
            "\n\n# Working directory\nYou are operating in this EXISTING project: `{}`{}. \
             All shell commands run here (cwd) and relative paths resolve here.\n\
             - Build INSIDE this project, using its existing stack and conventions. Identify the stack first (read package.json / Cargo.toml / the framework config) and add code where it belongs — e.g. for Next.js create pages/components/route handlers in the right folders. NEVER hand-write a standalone `index.html` (or a generic from-scratch solution) when the project is a framework app — use the framework.\n\
             - Create new files ONLY inside this directory. Do NOT write outside it or invent a new sibling folder (e.g. `../something-new`, `/Volumes/...`). Even when asked for a 'separate' or 'standalone' page, put it inside this project unless the user gives an explicit absolute path elsewhere.\n\
             - Search, read, and edit inside this directory; do NOT scan $HOME or the whole filesystem.\n\
             - For long-running servers/watchers, start them detached with output redirected to a log, then poll the log/port. Do not block a shell tool forever.",
            self.workspace.display(),
            stack.map(|s| format!(" (detected stack: {s})")).unwrap_or_default()
        ));
        // Inject a shallow file-tree so the agent knows the real layout up front.
        let map = project_map(&self.workspace);
        if !map.trim().is_empty() {
            sys.push_str("\n\n# Project structure (shallow)\n```\n");
            sys.push_str(map.trim_end());
            sys.push_str(
                "\n```\nWork within this existing structure; place new code where it belongs.",
            );
        }
        // SOUL.md — persistent persona/identity (hermes-style), loaded first so
        // it frames everything. Lives in the workspace (or .oxide/) and can be
        // edited by the user — or by the agent for the root-level copy.
        for soul in [".oxide/SOUL.md", "SOUL.md"] {
            if let Ok(text) = std::fs::read_to_string(self.workspace.join(soul)) {
                let t = text.trim();
                if !t.is_empty() {
                    sys.push_str("\n\n# Persona (SOUL.md)\n");
                    sys.push_str(t);
                    break;
                }
            }
        }
        // Pinned project instructions (AGENTS.md / CLAUDE.md) — always resident,
        // never compacted away.
        if let Some(agents) = load_project_instructions(&self.workspace) {
            sys.push_str("\n\n# Project instructions (AGENTS.md)\n");
            sys.push_str(&agents);
        }
        sys.push_str(
            "\n\n# Diagrams\n\
When the user asks how something works, the architecture, a flow, a sequence, \
or relationships, include a Mermaid diagram in a ```mermaid fenced code block \
(flowchart/sequenceDiagram/stateDiagram-v2/classDiagram/erDiagram) — it renders \
as a visual in the chat. Keep it focused; add a short prose explanation too.\n\
\n# Task tracking\n\
For non-trivial work (multiple files, multiple tool steps, or anything that may take more than a few minutes), call `todo_write` early with a short checklist. Keep exactly one item `in_progress`, mark items `completed` as soon as they are done, and update the list when the plan changes.\n\
\n# Persistent memory & self-improvement\n\
             You have durable memory at .oxide/memory. Use the `remember` tool to store \
             important facts and `save_skill` to capture reusable procedures you discover. \
             Consult what you already know below before acting.",
        );
        if !mem_block.is_empty() {
            sys.push_str("\n\n");
            sys.push_str(&mem_block);
        }
        if !self.mcp_instructions.is_empty() {
            sys.push_str("\n\n# MCP server instructions\n");
            for (server, instructions) in &self.mcp_instructions {
                sys.push_str(&format!("\n## {server}\n{}\n", instructions.trim()));
            }
        }
        let mut assistant = String::new();
        let mut interrupted = false;

        // ── Orchestration pipeline (front planner to backend implementer) ──
        if self.config.orchestrate {
            let front = self.config.front_provider.clone();
            let backend = self.config.backend_provider.clone();
            let effort = self.config.reasoning_effort.clone();
            self.emit(Event::Info {
                text: format!("Planning · front: {front}"),
            })
            .await;
            let plan = self
                .stream_collect(
                    &front,
                    "You are the planner. Produce a clear, concise numbered plan to accomplish the user's request. Output only the plan — do not implement.",
                    &user_text,
                    &effort,
                    turn,
                    true,
                    false,
                )
                .await;

            if self.config.subagents {
                // ── Run the plan's numbered steps through tool-capable workers ──
                let subtasks: Vec<String> = plan
                    .lines()
                    .map(|l| l.trim())
                    .filter(|l| {
                        l.starts_with(|c: char| c.is_ascii_digit())
                            || l.starts_with('-')
                            || l.starts_with('*')
                    })
                    .map(|l| {
                        l.trim_start_matches(|c: char| {
                            c.is_ascii_digit() || matches!(c, '.' | ')' | '-' | '*' | ' ')
                        })
                        .to_string()
                    })
                    .filter(|l| !l.is_empty())
                    .take(6)
                    .collect();

                if subtasks.is_empty() {
                    // No clear steps — fall back to a single implementer.
                    let isys = format!(
                        "You are the implementer. Carry out this plan precisely.\n\nPLAN:\n{plan}"
                    );
                    let worker_sys = format!("{sys}\n\n# Orchestration role\n{isys}");
                    let worker_id = format!("orchestrate-implement-{}", turn.0);
                    let (out, was_interrupted) = self
                        .stream_agentic_collect(
                            &worker_sys,
                            &user_text,
                            turn,
                            &worker_id,
                            WorkerProfile::implementer(&backend, &effort),
                            op_rx,
                            None,
                        )
                        .await;
                    assistant = out;
                    interrupted |= was_interrupted;
                } else {
                    self.emit(Event::Info {
                        text: format!(
                            "Running {} tool-capable sub-agents · backend: {backend}",
                            subtasks.len()
                        ),
                    })
                    .await;
                    let assignments = subtasks
                        .iter()
                        .enumerate()
                        .map(|(i, st)| {
                            let profile = subagent_profile_for(st, &backend, &effort);
                            let profile_id = profile.id.clone();
                            let bsys = format!(
                                "{sys}\n\n# Sub-agent assignment\nYou are sub-agent {} ({profile_id}). Do EXACTLY this subtask and report what you did. Overall plan for context:\n{plan}",
                                i + 1
                            );
                            let worker_id =
                                format!("subagent-{}-{}-{}", turn.0, i + 1, profile_id);
                            SubagentAssignment {
                                index: i + 1,
                                task: st.clone(),
                                system: bsys,
                                worker_id,
                                profile,
                            }
                        })
                        .collect::<Vec<_>>();
                    let results = self.run_subagents_parallel(assignments, turn, op_rx).await;
                    interrupted |= results.iter().any(|result| result.interrupted);
                    for result in &results {
                        self.emit(Event::Info {
                            text: format!(
                                "Sub-agent {}: {}",
                                result.index,
                                result.task.chars().take(60).collect::<String>()
                            ),
                        })
                        .await;
                    }
                    let joined: String = results
                        .iter()
                        .map(|result| {
                            format!(
                                "### Sub-agent {} — {}\n{}",
                                result.index, result.task, result.output
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    if interrupted {
                        assistant = joined;
                    } else {
                        // Synthesize sub-agent outputs into the final answer.
                        self.emit(Event::Info {
                            text: format!("Synthesizing · front: {front}"),
                        })
                        .await;
                        let ssys = format!(
                            "You are the lead. Combine the sub-agent results into one coherent final answer for the user. Resolve overlaps, note anything incomplete.\n\nSUB-AGENT RESULTS:\n{joined}"
                        );
                        assistant = self
                            .stream_collect(&front, &ssys, &user_text, &effort, turn, false, false)
                            .await;
                    }
                }
            } else {
                self.emit(Event::Info {
                    text: format!("Implementing · backend: {backend}"),
                })
                .await;
                let isys = format!(
                    "You are the implementer. Carry out the following plan precisely to fulfil the user's request — do the actual work, edits and commands.\n\nPLAN:\n{plan}"
                );
                let worker_sys = format!("{sys}\n\n# Orchestration role\n{isys}");
                let worker_id = format!("orchestrate-implement-{}", turn.0);
                let (out, was_interrupted) = self
                    .stream_agentic_collect(
                        &worker_sys,
                        &user_text,
                        turn,
                        &worker_id,
                        WorkerProfile::implementer(&backend, &effort),
                        op_rx,
                        None,
                    )
                    .await;
                assistant = out;
                interrupted |= was_interrupted;
            }

            if interrupted {
                if !assistant.is_empty() {
                    if let Some(store) = &self.session_store {
                        let _ = store.append("assistant", &assistant);
                    }
                    self.session.push(Message::new(Role::Assistant, assistant));
                }
                self.emit(Event::Info {
                    text: "turn interrupted".into(),
                })
                .await;
                self.run_stop_lifecycle(turn, &user_text, true).await;
                self.note_db_error_once().await;
                self.finish_turn(turn).await;
                return;
            }

            // Auto-verify orchestrated workers too. The normal agentic path has
            // its own verification loop; without this, a subscription backend
            // could edit files and stop before proving the changes compile.
            if self.config.auto_verify && self.turn_edited {
                for verify_iter in 1..=2 {
                    let VerifyOutcome::Failed(report) = self.run_verify(turn).await else {
                        break;
                    };
                    self.turn_edited = false;
                    self.emit(Event::Info {
                        text: format!(
                            "Auto-verify failed · fixing {verify_iter}/2 · backend: {backend}"
                        ),
                    })
                    .await;
                    let header = format!("\n\n— Auto-verify fix {verify_iter} —\n");
                    self.emit(Event::AgentMessageDelta {
                        turn,
                        text: header.clone(),
                    })
                    .await;
                    assistant.push_str(&header);
                    let vsys = format!(
                        "{sys}\n\n# Verification fix\nA build/typecheck failed after the backend work. Fix the diagnostics below with tools, then summarize what changed. Do not just explain.\n\nPLAN:\n{plan}\n\nWORK SO FAR:\n{assistant}\n\nVERIFY FAILURE:\n{report}"
                    );
                    let worker_id = format!("orchestrate-verify-{}-{verify_iter}", turn.0);
                    let (fix, was_interrupted) = self
                        .stream_agentic_collect(
                            &vsys,
                            &user_text,
                            turn,
                            &worker_id,
                            WorkerProfile::implementer(&backend, &effort),
                            op_rx,
                            None,
                        )
                        .await;
                    assistant.push_str(&fix);
                    if was_interrupted {
                        interrupted = true;
                        break;
                    }
                    if !self.turn_edited {
                        break;
                    }
                }
            }

            if interrupted {
                self.emit(Event::Info {
                    text: "turn interrupted".into(),
                })
                .await;
                if !assistant.is_empty() {
                    if let Some(store) = &self.session_store {
                        let _ = store.append("assistant", &assistant);
                    }
                    self.session.push(Message::new(Role::Assistant, assistant));
                }
                self.run_stop_lifecycle(turn, &user_text, true).await;
                self.note_db_error_once().await;
                self.finish_turn(turn).await;
                return;
            }

            // ── Review + auto-fix loop (review, then re-implement if gaps) ──
            let max_iters: u32 = 3;
            let mut iter: u32 = 0;
            loop {
                self.emit(Event::Info {
                    text: format!("Reviewing · front: {front}"),
                })
                .await;
                let vsys = format!(
                    "You are the reviewer. Verify whether the implementation fulfils the user's request. On the FIRST line reply with exactly `DONE` if it is fully complete and correct, otherwise reply `GAPS` and then list the specific remaining gaps. Be concise.\n\nPLAN:\n{plan}\n\nRESULT SO FAR:\n{assistant}"
                );
                // Review shows in the thinking box (orchestrator's verification).
                let review = self
                    .stream_collect(&front, &vsys, &user_text, &effort, turn, true, false)
                    .await;
                let has_gaps = !review_passes_gate(&review);
                if !has_gaps {
                    self.emit(Event::Info {
                        text: "Review passed".to_string(),
                    })
                    .await;
                    break;
                }
                iter += 1;
                if iter >= max_iters {
                    self.emit(Event::Info {
                        text: format!("Gaps remain after {max_iters} fixes"),
                    })
                    .await;
                    let note = format!("\n\n— Remaining gaps —\n{}", review.trim());
                    self.emit(Event::AgentMessageDelta {
                        turn,
                        text: note.clone(),
                    })
                    .await;
                    assistant.push_str(&note);
                    break;
                }
                self.emit(Event::Info {
                    text: format!("Fixing gaps · iteration {iter} · backend: {backend}"),
                })
                .await;
                let header = format!("\n\n— Revision {iter} —\n");
                self.emit(Event::AgentMessageDelta {
                    turn,
                    text: header.clone(),
                })
                .await;
                assistant.push_str(&header);
                let fsys = format!(
                    "You are the implementer. Fix the gaps the reviewer found — make the actual edits/commands. Do not redo what already works.\n\nPLAN:\n{plan}\n\nGAPS TO FIX:\n{review}\n\nWORK SO FAR:\n{assistant}"
                );
                let worker_sys = format!("{sys}\n\n# Orchestration role\n{fsys}");
                let worker_id = format!("orchestrate-fix-{}-{iter}", turn.0);
                let (fix, was_interrupted) = self
                    .stream_agentic_collect(
                        &worker_sys,
                        &user_text,
                        turn,
                        &worker_id,
                        WorkerProfile::implementer(&backend, &effort),
                        op_rx,
                        None,
                    )
                    .await;
                assistant.push_str(&fix);
                if was_interrupted {
                    interrupted = true;
                    break;
                }
            }

            if interrupted {
                self.emit(Event::Info {
                    text: "turn interrupted".into(),
                })
                .await;
            }
            if !assistant.is_empty() {
                if let Some(store) = &self.session_store {
                    let _ = store.append("assistant", &assistant);
                }
                self.session.push(Message::new(Role::Assistant, assistant));
            }
            self.run_stop_lifecycle(turn, &user_text, interrupted).await;
            self.note_db_error_once().await;
            self.finish_turn(turn).await;
            return;
        }

        // ── Agentic loop: stream, run tool calls, re-request with results,
        //    until the model answers with no tool calls (or step budget runs out). ──
        let _ = &mut assistant; // (assistant is used by the orchestrate path above)
        let model = policy
            .model
            .clone()
            .unwrap_or_else(|| self.config.effective_model());
        let mut tool_budget = AdaptiveToolBudget::new(policy.max_steps as usize);
        let mut step = 0usize;
        let mut overflow_retries = 0u8;
        // Bounded re-requests for a round that died on a transient stream hiccup
        // (connection reset / 5xx / truncation) before producing anything.
        let mut transient_retries = 0u8;
        // CLI drivers (codex/claude) are self-agentic: they run their own tool
        // loop, so Oxide's nudge/wrap-up/auto-verify rounds would just respawn
        // the CLI with an out-of-context reminder as the whole prompt.
        let cli_driver = matches!(
            self.config.provider.as_str(),
            "codex" | "claude" | "claude_interactive"
        );
        let mut nudges = 0u8;
        let mut memory_nudged = false;
        let mut verify_evidence_nudged = false;
        // True once ANY tool call fires in this turn — used to decide whether
        // the nudge makes sense for API providers. For a pure text turn the
        // nudge produces a visible second reply rather than driving more work.
        let mut turn_had_tool = false;
        // Files a CLI driver reported changing — diffed + shown at turn end.
        let mut cli_changed: Vec<String> = Vec::new();
        // Git baseline tree of the workspace BEFORE the CLI runs, so its edits
        // can be reverted (the engine never sees the CLI's write moments).
        let cli_baseline: Option<String> = if cli_driver {
            git_baseline_tree(&self.workspace).await
        } else {
            None
        };
        let mut verifies = 0u8;
        let mut budget_stop_reminded = false;
        self.turn_edited = false;
        self.turn_reads.clear();
        self.turn_edit_paths.clear();
        self.turn_verify_passed = false;
        self.pending_verify_cmds.clear();
        self.last_tool_sig.clear();
        self.last_tool_reps = 0;
        self.turn_tool_signatures.clear();
        self.turn_todos.clear();
        loop {
            // Keep the running history under budget on EVERY request — long
            // agentic turns accumulate tool output and would otherwise overflow.
            self.compact_session(turn).await;
            // Compaction/interrupt/rewind can orphan a tool_call or tool_result;
            // strip any dangling pair so the provider request never 400s.
            context::sanitize_tool_pairs(&mut self.session);
            let mut msgs = vec![Message::new(Role::System, sys.clone())];
            msgs.extend(self.session.iter().cloned());
            let req = TurnRequest {
                model: model.clone(),
                reasoning_effort: self.config.reasoning_effort.clone(),
                temperature: policy.temperature,
                messages: msgs,
                tools: tools.clone(),
                cwd: self.workspace.display().to_string(),
                conversation_id: self
                    .session_store
                    .as_ref()
                    .map(|s| s.id.clone())
                    .unwrap_or_default(),
                // Seed the native CLI session id (if linked on a prior run) so a
                // resume after restart reattaches instead of starting fresh.
                cli_resume: self
                    .session_store
                    .as_ref()
                    .and_then(|s| db::cli_session(&s.id)),
                system_append: cli_system_append.clone(),
                claude_agents: cli_claude_agents.clone(),
            };

            let (stream_tx, mut stream_rx) = mpsc::channel::<StreamItem>(STREAM_QUEUE);
            let provider = oxide_providers::build(&self.config.provider);
            let stream_task = tokio::spawn(async move { provider.stream(req, stream_tx).await });

            let mut round_text = String::new();
            let mut pending_reasoning: Option<serde_json::Value> = None;
            let mut did_tool = false;
            let mut steered = false;
            // True only on a clean terminal stream signal (StreamItem::Done). A bare
            // channel close (None) leaves this false so a mid-stream cutoff can be
            // told apart from a real completion when deciding whether to retry.
            let mut saw_done = false;
            loop {
                tokio::select! {
                    item = stream_rx.recv() => {
                        // No engine-side idle cap: each provider manages its own
                        // timeout. HTTP/SSE clients have a read_timeout (a real
                        // stall closes the connection and the stream ends); CLI drivers
                        // (claude/codex) run their own tool timeouts and end the
                        // stream when the child exits. The user can always Interrupt.
                        match item {
                            Some(StreamItem::TextDelta(t)) => {
                                round_text.push_str(&t);
                                self.emit(Event::AgentMessageDelta { turn, text: t }).await;
                            }
                            Some(StreamItem::ReasoningDelta(t)) => {
                                self.emit(Event::ReasoningDelta { turn, text: t }).await;
                            }
                            Some(StreamItem::ReasoningItem(v)) => {
                                pending_reasoning = Some(v);
                            }
                            Some(StreamItem::ToolInputDelta { id, name, delta, accumulated }) => {
                                self.emit(Event::ToolCallDelta {
                                    turn,
                                    call_id: id,
                                    tool: name,
                                    delta,
                                    accumulated,
                                }).await;
                            }
                            Some(StreamItem::ToolCall { id, name, arguments }) => {
                                did_tool = true;
                                turn_had_tool = true;
                                // Record the assistant's tool call structurally so the model
                                // sees a real function_call/tool_use (with id) on replay — not
                                // flattened text. This is what stops the re-plan/re-read loop.
                                let prose = std::mem::take(&mut round_text);
                                let mut msg = Message::with_tool_call(
                                    prose,
                                    oxide_providers::ToolCall { id: id.clone(), name: name.clone(), arguments: arguments.clone() },
                                );
                                // Carry the model's encrypted reasoning so it keeps its train
                                // of thought instead of re-thinking every round.
                                msg.reasoning_item = pending_reasoning.take();
                                self.session.push(msg);
                                    if self.handle_tool_call(turn, name, arguments, id, None, op_rx).await {
                                    interrupted = true;
                                    break;
                                }
                            }
                            Some(StreamItem::FileChanged(path)) => {
                                if !cli_changed.contains(&path) {
                                    cli_changed.push(path.clone());
                                }
                                // Live counts: diff the file now (checkpoint 0 = not yet
                                // revertable) so the "Changing files" card shows +/- as edits
                                // land instead of staying blank until the turn settles. The
                                // turn-end pass re-emits with the real baseline + checkpoint.
                                let (rel, diff) = cli_file_diff(&self.workspace, &path).await;
                                if !diff.trim().is_empty() {
                                    self.emit(Event::FileDiff { turn, path: rel, diff, checkpoint: 0 }).await;
                                }
                            }
                                Some(StreamItem::Notice(text)) => {
                                    self.emit(Event::Info { text }).await;
                                }
                                Some(StreamItem::CommandStarted { id, command, cwd, background }) => {
                                    // Verification evidence: resolved at CommandFinished.
                                    if is_verification_command(&command) {
                                        self.pending_verify_cmds.insert(id.clone());
                                    }
                                    self.emit(Event::CommandStarted {
                                        turn,
                                        command_id: id.clone(),
                                        worker_id: None,
                                        command,
                                        cwd: if cwd.is_empty() { self.workspace.display().to_string() } else { cwd },
                                        background,
                                    }).await;
                                }
                                Some(StreamItem::BackgroundJob { id, command, path }) => {
                                    // Persisted so reopening this session re-surfaces the
                                    // job — its process outlives this app run.
                                    if let Some(store) = &self.session_store {
                                        let _ = store.append(
                                            "bg_job",
                                            &serde_json::json!({"id": &id, "command": &command, "path": &path}).to_string(),
                                        );
                                    }
                                    self.emit(Event::BackgroundJob { turn, command_id: id, command, path }).await;
                                }
                                Some(StreamItem::CommandOutput { id, stream, chunk }) => {
                                    self.emit(Event::CommandOutput {
                                        turn,
                                        command_id: id,
                                        worker_id: None,
                                        stream,
                                        chunk,
                                    }).await;
                                }
                                Some(StreamItem::CommandFinished { id, ok, exit_code, duration_ms }) => {
                                    if ok && self.pending_verify_cmds.remove(&id) {
                                        self.turn_verify_passed = true;
                                    }
                                    self.emit(Event::CommandFinished {
                                        turn,
                                        command_id: id,
                                        worker_id: None,
                                        ok,
                                        exit_code,
                                        duration_ms,
                                    }).await;
                                }
                                Some(StreamItem::Usage { input, output, context_window, cached_input, reasoning_output, cost_usd }) => {
                                    self.emit(Event::TokensUsed { turn, input, output, cost_usd, cached_input, reasoning_output }).await;
                                if let Some(limit) = context_window {
                                    self.ctx_window = Some(limit);
                                    self.emit(Event::ContextWindow { limit }).await;
                                }
                            }
                            Some(StreamItem::RateLimit { plan, primary_pct, secondary_pct, primary_reset_s, secondary_reset_s }) => {
                                self.emit(Event::RateLimit { plan, primary_pct, secondary_pct, primary_reset_s, secondary_reset_s }).await;
                            }
                            Some(StreamItem::CliSession(cli_id)) => {
                                // Persist the link so a resume after restart reattaches to
                                // this exact CLI session instead of starting a fresh one.
                                if let Some(store) = &self.session_store {
                                    db::set_cli_session(&store.id, &cli_id);
                                }
                            }
                            // Clean terminal signal vs. a bare channel close (cut-off):
                            // distinguished so the transient-retry path below only fires
                            // for a real interruption, never a normal completion.
                            Some(StreamItem::Done) => {
                                saw_done = true;
                                break;
                            }
                            None => break,
                        }
                    }
                    op = op_rx.recv() => {
                        match op {
                            Some(Op::Interrupt) => {
                                self.interrupt_persistent_claude();
                                interrupted = true; self.user_interrupted = true; break;
                            }
                            Some(Op::Shutdown) => {
                                self.interrupt_persistent_claude();
                                interrupted = true; break;
                            }
                            // Steering: a message sent mid-turn is injected into the
                            // conversation; the next agentic round picks it up.
                            Some(Op::UserTurn { text }) => {
                                if self.maybe_side_question(&text) {
                                    self.emit(Event::Info { text: "\u{1f4ac} btw \u{b7} answering on the side\u{2026}".to_string() }).await;
                                    continue;
                                }
                                if let Some(store) = &self.session_store {
                                    let _ = store.append("user", &text);
                                }
                                self.session.push(Message::new(Role::User, text.clone()));
                                self.emit(Event::Info { text: format!("Steering: {text}") }).await;
                                // Persistent claude driver: abort the in-flight
                                // generation NOW so the steer redirects mid-turn
                                // instead of waiting for the current answer. The
                                // steer text still flows via the next round
                                // (steered=true). No-op for every other provider /
                                // the one-shot driver.
                                self.interrupt_persistent_claude();
                                steered = true;
                            }
                            // Rewind works mid-turn too — restoring a checkpoint
                            // is independent of the stream in flight.
                            Some(Op::Rewind { checkpoint_id }) => {
                                let restored = self.rewind_checkpoint(checkpoint_id).await;
                                self.emit(Event::RewindDone { id: checkpoint_id, restored }).await;
                            }
                            Some(other) => {
                                self.emit(Event::Info { text: format!("queued op ignored mid-turn: {other:?}") }).await;
                            }
                            // Op channel closed (handle dropped — e.g. a pane was
                            // closed): abort the live stream instead of waiting for
                            // the model to finish on its own.
                            None => {
                                self.interrupt_persistent_claude();
                                interrupted = true; break;
                            }
                        }
                    }
                }
            }
            // Surface a provider error; on context-overflow, hard-compact + retry;
            // on a transient stream hiccup before any output, re-request the round.
            // `crashed` flags a panic in the provider task (JoinError) — terminal:
            // never compacted or retried (it would just panic again), only shown.
            let (stream_err, crashed) = if interrupted {
                stream_task.abort();
                (None, false)
            } else {
                match stream_task.await {
                    Ok(Ok(())) => (None, false),
                    Ok(Err(e)) => (Some(e.to_string()), false),
                    Err(join) if join.is_cancelled() => (None, false),
                    // A bare `.ok()` here used to swallow the JoinError, leaving
                    // stream_err=None — the turn then committed partial text and
                    // ended like a clean finish, hiding the crash from the user.
                    Err(join) => (Some(format!("provider stream task crashed: {join}")), true),
                }
            };
            if let Some(err) = &stream_err {
                let low = err.to_lowercase();
                let overflow = !crashed
                    && (low.contains("context")
                        || low.contains("exceeds")
                        || low.contains("too long")
                        || low.contains("maximum")
                        || (low.contains("token") && low.contains("limit")));
                if overflow && round_text.is_empty() && overflow_retries < 3 {
                    overflow_retries += 1;
                    self.force_compact(turn).await;
                    self.emit(Event::Info {
                        text: "context full — compacted, retrying".into(),
                    })
                    .await;
                    continue;
                }
                // Transient cutoff (connection reset / 5xx / truncation / stall) on a
                // round that produced NOTHING yet: re-request instead of ending the
                // turn — this is the "turn berhenti tiba-tiba" case. Gated hard on
                // `round_text.is_empty() && !did_tool && !saw_done` so a round that
                // already streamed text or ran a tool is never re-run (no duplicate
                // output / double tool calls). Allowlist transient classes only;
                // exclude hard provider stops (auth/quota) that won't self-heal.
                let transient = !overflow
                    && !crashed
                    && (low.contains("connection")
                        || low.contains("reset")
                        || low.contains("closed")
                        || low.contains("truncat")
                        || low.contains("before a completion")
                        || low.contains("stalled")
                        || low.contains("timed out")
                        || low.contains("timeout")
                        || low.contains("502")
                        || low.contains("503")
                        || low.contains("504")
                        || low.contains("temporarily")
                        || low.contains("overloaded"));
                let hard = low.contains("token expired")
                    || low.contains("rejected")
                    || low.contains("sign in")
                    || low.contains("log in")
                    || low.contains("re-authenticate")
                    || low.contains("wait for the plan reset")
                    || low.contains("rate limit reached");
                if transient
                    && !hard
                    && !saw_done
                    && round_text.is_empty()
                    && !did_tool
                    && transient_retries < 3
                {
                    transient_retries += 1;
                    let backoff =
                        std::time::Duration::from_millis(400u64 << (transient_retries - 1));
                    self.emit(Event::TurnStatus {
                        turn,
                        state: "retrying".into(),
                        detail: format!("{transient_retries}/3"),
                    })
                    .await;
                    self.emit(Event::Info {
                        text: format!("stream interrupted — retrying ({transient_retries}/3)"),
                    })
                    .await;
                    // Interruptible backoff: Stop/Shutdown wins immediately; a message
                    // sent during the wait is folded in as steering for the retry.
                    tokio::select! {
                        _ = tokio::time::sleep(backoff) => {}
                        op = op_rx.recv() => match op {
                            Some(Op::Interrupt) => { interrupted = true; self.user_interrupted = true; }
                            Some(Op::Shutdown) | None => { interrupted = true; }
                            Some(Op::UserTurn { text }) => {
                                if let Some(store) = &self.session_store {
                                    let _ = store.append("user", &text);
                                }
                                self.session.push(Message::new(Role::User, text));
                            }
                            Some(_) => {}
                        }
                    }
                    if !interrupted {
                        continue;
                    }
                }
                self.emit(Event::Error {
                    message: err.clone(),
                })
                .await;
            }
            if !round_text.is_empty() {
                assistant.push_str(&round_text);
                if let Some(store) = &self.session_store {
                    let _ = store.append("assistant", &round_text);
                }
                let mut msg = Message::new(Role::Assistant, round_text);
                msg.reasoning_item = pending_reasoning.take();
                self.session.push(msg);
            }
            if did_tool {
                step += 1;
            }
            if interrupted {
                break;
            }
            if did_tool {
                let completed_todos = self
                    .turn_todos
                    .iter()
                    .filter(|(_, status)| status == "completed")
                    .count();
                let pending_todos = self
                    .turn_todos
                    .iter()
                    .any(|(_, status)| status != "completed");
                let progress = tool_progress_score(
                    self.turn_tool_signatures.len(),
                    self.turn_reads.len(),
                    self.turn_edit_paths.len(),
                    completed_todos,
                    self.turn_verify_passed,
                );
                match tool_budget.after_tool_round(
                    step,
                    progress,
                    pending_todos,
                    self.turn_edited && !self.turn_verify_passed,
                ) {
                    ToolBudgetDecision::Continue => {}
                    ToolBudgetDecision::Extended { new_limit } => {
                        budget_stop_reminded = false;
                        self.emit(Event::Info {
                            text: format!(
                                "Tool budget extended adaptively to {new_limit} rounds because the turn is still making measurable progress"
                            ),
                        })
                        .await;
                        self.session.push(Message::new(
                            Role::User,
                            format!(
                                "<system-reminder>\nTool budget extended adaptively to {new_limit} rounds because measurable progress is continuing. Finish the remaining checklist and promised verification; avoid repeated exploration.\n</system-reminder>"
                            ),
                        ));
                        continue;
                    }
                    ToolBudgetDecision::Stop if !budget_stop_reminded => {
                        budget_stop_reminded = true;
                        self.session.push(Message::new(
                            Role::User,
                            "<system-reminder>\nNo new measurable progress justified another tool-budget extension. Do not call more tools. Reply with completed work, verification evidence, and genuinely unfinished items.\n</system-reminder>",
                        ));
                        continue;
                    }
                    ToolBudgetDecision::Stop => break,
                }
            }
            if cli_driver && !steered {
                // One CLI run per turn — it finished, we're done.
                break;
            }
            if !did_tool && !steered {
                // The model produced prose but took no action. If it likely owes
                // an edit, nudge it once to actually do the work before ending.
                // For non-CLI API providers skip the nudge when no tool was called
                // anywhere in this turn: the response is conversational, and a
                // nudge produces a second visible reply rather than driving action.
                if nudges < 1 && (cli_driver || turn_had_tool) {
                    nudges += 1;
                    self.session.push(Message::new(Role::User,
                        "<system-reminder>\nYou stopped, but the task may not be fully done. Check honestly:\n\
- Did you finish EVERY step you planned? If your plan said you'd run a typecheck/lint/tests, run them NOW with `shell` and fix what breaks — a single edit is rarely the whole task.\n\
- Are there other files/call-sites that need the same change for it to actually work?\n\
If yes, do it now with edit/write_file/shell — don't describe it. Only end when the task is genuinely COMPLETE and VERIFIED (you ran the check and saw it pass), or you're truly blocked and need a decision (then call ask_user).\n</system-reminder>"));
                    continue;
                }
                // Auto-verify: build/typecheck the edits and feed failures back
                // so the agent fixes them before finishing (Cursor-style).
                if self.config.auto_verify && self.turn_edited && verifies < 2 {
                    match self.run_verify(turn).await {
                        VerifyOutcome::Failed(report) => {
                            verifies += 1;
                            self.turn_edited = false;
                            self.session.push(Message::new(Role::User, format!(
                                "<system-reminder>\nA build/typecheck failed after your edits. Fix the errors below, \
then stop. Apply fixes with edit/write_file — do not just explain.\n\n{report}\n</system-reminder>"
                            )));
                            continue;
                        }
                        VerifyOutcome::Passed => self.turn_verify_passed = true,
                        VerifyOutcome::Skipped => {}
                    }
                }
                // hermes verify-on-stop: code was edited but NO verification
                // command ran and passed this turn — demand evidence (or an
                // explicit blocker) exactly once before letting the turn end.
                if !verify_evidence_nudged
                    && self.turn_edited
                    && !self.turn_verify_passed
                    && edits_touch_code(&self.turn_edit_paths)
                {
                    verify_evidence_nudged = true;
                    let hint = if self.config.verify_command.trim().is_empty() {
                        String::new()
                    } else {
                        format!(
                            " The project's verify command: `{}`.",
                            self.config.verify_command.trim()
                        )
                    };
                    self.session.push(Message::new(Role::User, format!(
                        "<system-reminder>\nYou edited code this turn but no verification command (tests/lint/typecheck) ran and passed. Run the appropriate check NOW with `shell` and fix what breaks.{hint} If verification is genuinely impossible here, state the blocker explicitly — then stop.\n</system-reminder>"
                    )));
                    continue;
                }
                // Self-improvement loop (hermes-style): after a substantial turn,
                // nudge ONCE to persist durable learnings before finishing.
                if !memory_nudged && step >= 8 && !self.turn_edit_paths.is_empty() {
                    memory_nudged = true;
                    self.session.push(Message::new(Role::User,
                        "<system-reminder>\nBefore finishing: did this task teach you anything durable and \
non-obvious (project quirks, gotchas, user preferences, a reusable multi-step procedure)? \
If yes, persist it NOW — `remember` for facts, `save_skill` for procedures. If nothing \
qualifies, just finish; do not save trivia.\n</system-reminder>"));
                    continue;
                }
                break;
            }
        }
        if interrupted {
            self.emit(Event::Info {
                text: "turn interrupted".into(),
            })
            .await;
        }
        // CLI drivers edit inside their own process — reconstruct the diffs from
        // git at turn end so the UI gets the same per-file cards + summary.
        let changed: Vec<String> = std::mem::take(&mut cli_changed);
        for path in changed {
            let (rel, diff) = cli_file_diff(&self.workspace, &path).await;
            if !diff.trim().is_empty() {
                self.turn_edited = true;
                if !self.turn_edit_paths.iter().any(|path| path == &rel) {
                    self.turn_edit_paths.push(rel.clone());
                }
                // Revertable: prior bytes come from the pre-turn git baseline
                // (absent there = new file, so revert deletes it).
                let mut checkpoint = 0u64;
                if let Some(tree) = &cli_baseline {
                    let prior = tokio::process::Command::new("git")
                        .arg("-C")
                        .arg(&self.workspace)
                        .args(["cat-file", "-p", &format!("{tree}:{rel}")])
                        .output()
                        .await
                        .ok()
                        .filter(|o| o.status.success())
                        .map(|o| o.stdout);
                    let abs = self.workspace.join(&rel);
                    checkpoint = self.snapshot_checkpoint_with(&abs, prior).await;
                    self.emit(Event::CheckpointCreated {
                        turn,
                        id: checkpoint,
                        label: format!("cli edit {rel}"),
                    })
                    .await;
                }
                self.emit(Event::FileDiff {
                    turn,
                    path: rel,
                    diff,
                    checkpoint,
                })
                .await;
            } else {
                // Touched but no textual change (identical write / already
                // committed). Emit an EMPTY FileDiff so the frontend can clear
                // its pending "editing…" row instead of spinning forever.
                self.emit(Event::FileDiff {
                    turn,
                    path: rel,
                    diff: String::new(),
                    checkpoint: 0,
                })
                .await;
            }
        }
        if cli_driver && !interrupted {
            self.run_cli_self_improvement_bridge(
                turn,
                &self.config.provider,
                &user_text,
                &assistant,
                &self.turn_edit_paths,
            )
            .await;
        }
        self.run_stop_lifecycle(turn, &user_text, interrupted).await;
        self.note_db_error_once().await;
        self.finish_turn(turn).await;

        // Context-aware follow-up suggestions, generated off-turn on the fast
        // lane. CLI drivers are skipped (a cold CLI spawn for 3 chips isn't
        // worth the cost) — the UI keeps its heuristic chips there.
        if !interrupted
            && !matches!(
                self.config.provider.as_str(),
                "codex" | "claude" | "claude_interactive" | "echo"
            )
        {
            let last_user = self
                .session
                .iter()
                .rev()
                .find(|m| m.role == Role::User && !m.content.starts_with("<system-reminder>"))
                .map(|m| m.content.chars().take(1200).collect::<String>())
                .unwrap_or_default();
            let last_reply = self
                .session
                .iter()
                .rev()
                .find(|m| m.role == Role::Assistant && !m.content.trim().is_empty())
                .map(|m| m.content.chars().take(1500).collect::<String>())
                .unwrap_or_default();
            let last_user_t = last_user.clone();
            let last_reply_t = last_reply.clone();
            if !last_reply.is_empty() {
                let provider_id = self.config.provider.clone();
                let model = {
                    let mut c = self.config.clone();
                    c.fast_mode = true;
                    c.effective_model()
                };
                let tx = self.event_tx.clone();
                tokio::spawn(async move {
                    let provider = oxide_providers::build(&provider_id);
                    let req = TurnRequest {
                        model,
                        reasoning_effort: "low".into(),
                        temperature: 0.7,
                        messages: vec![
                            Message::new(Role::System, "Suggest the user's 3 most likely NEXT prompts for this coding-agent conversation. Short imperative phrases (max 9 words each), in the user's language. Output ONLY the 3 prompts, one per line — no numbering, no quotes."),
                            Message::new(Role::User, format!("User asked:\n{last_user}\n\nAgent replied:\n{last_reply}")),
                        ],
                        tools: Vec::new(),
                        cwd: String::new(),
                        conversation_id: String::new(),
                        cli_resume: None,
                        system_append: None,
                        claude_agents: None,
                    };
                    let (stx, mut srx) = mpsc::channel::<StreamItem>(64);
                    let task = tokio::spawn(async move { provider.stream(req, stx).await });
                    let mut out = String::new();
                    while let Ok(Some(item)) =
                        tokio::time::timeout(std::time::Duration::from_secs(20), srx.recv()).await
                    {
                        match item {
                            StreamItem::TextDelta(t) => out.push_str(&t),
                            StreamItem::Done => break,
                            _ => {}
                        }
                    }
                    task.abort();
                    let items: Vec<String> = out
                        .lines()
                        .map(|l| {
                            l.trim()
                                .trim_start_matches(['-', '*', '•'])
                                .trim()
                                .to_string()
                        })
                        .filter(|l| !l.is_empty() && l.len() < 90)
                        .take(3)
                        .collect();
                    if !items.is_empty() {
                        let _ = tx.send(Event::Followups { items }).await;
                    }
                });
            }

            // Auto-title the chat from what it's about — once, while the title
            // is still the raw first line / "Chat" placeholder.
            let last_user = last_user_t;
            let last_reply = last_reply_t;
            let sid = self.session_store.as_ref().map(|s| s.id.clone());
            if let Some(sid) = sid {
                let cur = crate::db::title_of(&sid);
                let needs = cur.is_empty()
                    || cur.eq_ignore_ascii_case("chat")
                    || cur.starts_with("Context files")
                    || cur.starts_with('[')
                    || cur.starts_with('@');
                if needs && !last_reply.is_empty() {
                    let provider_id = self.config.provider.clone();
                    let model = {
                        let mut c = self.config.clone();
                        c.fast_mode = true;
                        c.effective_model()
                    };
                    let lu = last_user.clone();
                    let lr = last_reply.chars().take(800).collect::<String>();
                    tokio::spawn(async move {
                        let provider = oxide_providers::build(&provider_id);
                        let req = TurnRequest {
                            model,
                            reasoning_effort: "low".into(),
                            temperature: 0.4,
                            messages: vec![
                                Message::new(Role::System, "Give a SHORT title (3-6 words, the user's language) describing what this chat is about. Output ONLY the title — no quotes, no period, no prefix."),
                                Message::new(Role::User, format!("User asked:\n{lu}\n\nAgent replied:\n{lr}")),
                            ],
                            tools: Vec::new(),
                            cwd: String::new(),
                            conversation_id: String::new(),
                            cli_resume: None,
                            system_append: None,
                            claude_agents: None,
                        };
                        let (stx, mut srx) = mpsc::channel::<StreamItem>(64);
                        let task = tokio::spawn(async move { provider.stream(req, stx).await });
                        let mut out = String::new();
                        while let Ok(Some(item)) =
                            tokio::time::timeout(std::time::Duration::from_secs(15), srx.recv())
                                .await
                        {
                            match item {
                                StreamItem::TextDelta(t) => out.push_str(&t),
                                StreamItem::Done => break,
                                _ => {}
                            }
                        }
                        task.abort();
                        let title = out
                            .lines()
                            .find(|l| !l.trim().is_empty())
                            .unwrap_or("")
                            .trim()
                            .trim_matches(['"', '\'', '.', '*'])
                            .trim()
                            .to_string();
                        if !title.is_empty() {
                            crate::db::set_title(&sid, &title);
                        }
                    });
                }
            }
        }
    }

    /// Route one tool call through approval + sandbox and emit its result.
    /// Returns `true` if the turn was interrupted/shut down while waiting.
    /// Run the project's build/typecheck after edits and emit a durable audit
    /// row for every outcome so frontends can show verification evidence.
    async fn run_verify(&self, turn: TurnId) -> VerifyOutcome {
        let ws = &self.workspace;
        // Only verify when a relevant source file was edited. A docs/config-only
        // edit (e.g. README.md) must NOT trigger a project-wide typecheck that
        // surfaces pre-existing errors in unrelated files and drags the agent
        // off-task. Extension of any edited path drives the language choice.
        let ext = |p: &str| {
            std::path::Path::new(p)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase()
        };
        let edited: Vec<String> = self.turn_edit_paths.iter().map(|p| ext(p)).collect();
        let has = |exts: &[&str]| edited.iter().any(|e| exts.contains(&e.as_str()));
        let (prog, args): (String, Vec<String>) = if !self.config.verify_command.trim().is_empty() {
            (
                "sh".into(),
                vec!["-c".into(), self.config.verify_command.clone()],
            )
        } else if ws.join("Cargo.toml").exists() && has(&["rs"]) {
            (
                "cargo".into(),
                vec!["check".into(), "--message-format".into(), "short".into()],
            )
        } else if ws.join("tsconfig.json").exists() && has(&["ts", "tsx"]) {
            ("npx".into(), vec!["tsc".into(), "--noEmit".into()])
        } else if ws.join("package.json").exists()
            && has(&["ts", "tsx", "js", "jsx", "mjs", "cjs", "vue", "svelte"])
        {
            (
                "npm".into(),
                vec!["run".into(), "build".into(), "--if-present".into()],
            )
        } else if (ws.join("pyproject.toml").exists() || ws.join("requirements.txt").exists())
            && has(&["py"])
        {
            ("ruff".into(), vec!["check".into(), ".".into()])
        } else {
            self.emit_audit(
                Some(turn),
                "verify",
                "Verification skipped",
                "No matching verifier for the edited files",
                "skipped",
            )
            .await;
            return VerifyOutcome::Skipped;
        };
        let command = format!("{prog} {}", args.join(" "));
        self.emit_audit(
            Some(turn),
            "verify",
            "Verification started",
            command.clone(),
            "running",
        )
        .await;
        self.emit(Event::Info {
            text: format!("auto-verify: {command}"),
        })
        .await;
        let fut = tokio::process::Command::new(&prog)
            .args(&args)
            .current_dir(ws)
            .output();
        let out = match tokio::time::timeout(std::time::Duration::from_secs(180), fut).await {
            Ok(Ok(output)) => output,
            Ok(Err(err)) => {
                self.emit_audit(
                    Some(turn),
                    "verify",
                    "Verification could not start",
                    format!("{command}: {err}"),
                    "failed",
                )
                .await;
                return VerifyOutcome::Skipped;
            }
            Err(_) => {
                self.emit_audit(
                    Some(turn),
                    "verify",
                    "Verification timed out",
                    format!("{command} exceeded 180 seconds"),
                    "failed",
                )
                .await;
                return VerifyOutcome::Skipped;
            }
        };
        if out.status.success() {
            self.emit_audit(Some(turn), "verify", "Verification passed", command, "done")
                .await;
            return VerifyOutcome::Passed;
        }
        let mut s = String::from_utf8_lossy(&out.stdout).to_string();
        s.push_str(&String::from_utf8_lossy(&out.stderr));
        let s = s.trim();
        if s.is_empty() {
            let report = format!(
                "$ {command}\nexited with status {} and no diagnostics",
                out.status.code().unwrap_or(-1)
            );
            self.emit_audit(
                Some(turn),
                "verify",
                "Verification failed",
                report.clone(),
                "failed",
            )
            .await;
            return VerifyOutcome::Failed(report);
        }
        // Surface ONLY diagnostics that reference a file edited this turn — a
        // build failing on pre-existing errors elsewhere isn't this turn's job
        // (opencode does per-edited-file diagnostics, not project-wide chasing).
        let names: Vec<String> = self
            .turn_edit_paths
            .iter()
            .filter_map(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
            })
            .collect();
        if !names.is_empty() && self.config.verify_command.trim().is_empty() {
            let relevant: String = s
                .lines()
                .filter(|l| names.iter().any(|n| l.contains(n.as_str())))
                .collect::<Vec<_>>()
                .join("\n");
            if relevant.trim().is_empty() {
                self.emit_audit(
                    Some(turn),
                    "verify",
                    "Verification found unrelated failures",
                    command,
                    "skipped",
                )
                .await;
                return VerifyOutcome::Skipped;
            }
            let capped: String = relevant.chars().take(6000).collect();
            let report = format!("$ {command}\n{capped}");
            self.emit_audit(
                Some(turn),
                "verify",
                "Verification failed",
                report.clone(),
                "failed",
            )
            .await;
            return VerifyOutcome::Failed(report);
        }
        let capped: String = s.chars().take(6000).collect();
        let report = format!("$ {command}\n{capped}");
        self.emit_audit(
            Some(turn),
            "verify",
            "Verification failed",
            report.clone(),
            "failed",
        )
        .await;
        VerifyOutcome::Failed(report)
    }

    /// Validate an `edit` and compute the resulting full file content.
    fn compute_edit(&self, args: &serde_json::Value) -> Result<(String, String), String> {
        let path = args["path"]
            .as_str()
            .ok_or("edit: missing 'path'")?
            .to_string();
        let old = args["old_string"]
            .as_str()
            .ok_or("edit: missing 'old_string'")?;
        let new = args["new_string"].as_str().unwrap_or("");
        let replace_all = args["replace_all"].as_bool().unwrap_or(false);
        if old.is_empty() {
            return Err(
                "edit: 'old_string' is empty — use write_file to create a whole file.".into(),
            );
        }
        let abs = self.workspace.join(&path);
        if abs.exists() && !self.read_files.contains(&path) {
            return Err(format!(
                "edit: you must read_file '{path}' before editing it."
            ));
        }
        let content = std::fs::read_to_string(&abs).unwrap_or_default();
        let count = content.matches(old).count();
        if count == 0 {
            return Err(format!("edit: old_string not found in '{path}'. Read the file and copy the exact text (with whitespace)."));
        }
        if count > 1 && !replace_all {
            return Err(format!("edit: old_string appears {count} times in '{path}' — add surrounding lines to make it unique, or set replace_all=true."));
        }
        let new_content = if replace_all {
            content.replace(old, new)
        } else {
            content.replacen(old, new, 1)
        };
        Ok((path, new_content))
    }

    async fn handle_tool_call(
        &mut self,
        turn: TurnId,
        name: String,
        arguments: serde_json::Value,
        call_id: String,
        worker_id: Option<&str>,
        op_rx: &mut mpsc::Receiver<Op>,
    ) -> bool {
        // Unwrap the tool_search bridge invoker FIRST: approval routing keys on
        // the tool name, so the invocation must proceed as the REAL tool —
        // guardrails, summaries, and the UI all see the underlying call
        // (hermes' "routes through the normal dispatcher" rule). An unknown /
        // non-deferred inner name falls through to the `tool_call` error arm.
        let (name, mut arguments) = if name == "tool_call" {
            let inner = arguments["name"].as_str().unwrap_or("").to_string();
            let inner_args = arguments
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            if !inner.is_empty() && self.deferred_tools.iter().any(|t| t.name == inner) {
                (inner, inner_args)
            } else {
                (name, arguments)
            }
        } else {
            (name, arguments)
        };
        self.emit(Event::ToolCallBegin {
            turn,
            call_id: call_id.clone(),
            tool: name.clone(),
            args: arguments.clone(),
        })
        .await;
        self.emit_audit(
            Some(turn),
            "tool",
            format!("Tool started · {name}"),
            compact_json(&arguments, 800),
            "running",
        )
        .await;

        // ask_user: surface a question (with optional choices) and block for the answer.
        if name == "ask_user" {
            let request_id = self.next_request_id();
            let question = arguments["question"].as_str().unwrap_or("").to_string();
            let options = arguments["options"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            self.emit(Event::QuestionAsked {
                request_id,
                question,
                options,
            })
            .await;
            let answer = loop {
                match op_rx.recv().await {
                    Some(Op::QuestionAnswer {
                        request_id: rid,
                        answer,
                    }) if rid == request_id => break answer,
                    // A typed message while a question is pending IS the answer —
                    // frontends without a dedicated answer UI (TUI, panes) would
                    // otherwise deadlock here.
                    Some(Op::UserTurn { text }) => break text,
                    Some(Op::Interrupt) | Some(Op::Shutdown) | None => {
                        self.session.push(Message::tool_result(
                            "interrupted before answering",
                            &call_id,
                        ));
                        self.emit_tool_end(
                            turn,
                            call_id.clone(),
                            name,
                            "interrupted before answering".into(),
                            false,
                        )
                        .await;
                        return true;
                    }
                    Some(_) => {}
                }
            };
            self.session.push(Message::tool_result(
                format!("[ask_user answer] {answer}"),
                &call_id,
            ));
            self.emit_tool_end(turn, call_id.clone(), name, answer, true)
                .await;
            return false;
        }

        let hook_config = hooks::Hooks::load(&self.workspace);
        if hook_config.auto().guard_dangerous_shell {
            let guard_reason = match hooks::dangerous_tool_reason(&name, &arguments) {
                Some(reason) => Some(reason),
                None => hooks::dcg_tool_reason(&name, &arguments).await,
            };
            if let Some(reason) = guard_reason {
                self.session
                    .push(Message::tool_result(reason.clone(), &call_id));
                self.emit_audit(
                    Some(turn),
                    "guard",
                    "Dangerous command blocked",
                    reason.clone(),
                    "blocked",
                )
                .await;
                self.emit_tool_end(turn, call_id.clone(), name, reason, false)
                    .await;
                return false;
            }
        }

        let routing_tools = tools_for_routing(self.all_tools(), !self.deferred_tools.is_empty());
        let mut router = ToolRouter::new(
            self.config.approval_policy,
            self.config.sandbox,
            self.workspace.clone(),
            &routing_tools,
        );
        for t in &self.session_approved {
            router.approve_for_session(t);
        }

        // Gate on policy; request approval if needed.
        match router.route(&name) {
            Routed::Denied(reason) => {
                // Always pair the recorded tool call with a result — a dangling
                // function_call poisons every later request on paired providers.
                let output = format!("denied: {reason}");
                self.session
                    .push(Message::tool_result(output.clone(), &call_id));
                self.emit_tool_end(turn, call_id.clone(), name, output, false)
                    .await;
                return false;
            }
            Routed::Run => {
                // Run came from a prior "Always" — that consent lifts the
                // sandbox for this tool too.
                if router.is_session_approved(&name) {
                    router.set_approved(true);
                }
            }
            Routed::NeedsApproval => {
                let request_id = self.next_request_id();
                self.emit(Event::ApprovalRequested {
                    request_id,
                    tool: name.clone(),
                    summary: router.summarize(&name, &arguments),
                })
                .await;

                // Block the turn until the frontend answers (or interrupts).
                loop {
                    match op_rx.recv().await {
                        Some(Op::ApprovalResponse {
                            request_id: rid,
                            decision,
                        }) if rid == request_id => match decision {
                            ApprovalDecision::Reject => {
                                self.session
                                    .push(Message::tool_result("rejected by user", &call_id));
                                self.emit_tool_end(
                                    turn,
                                    call_id.clone(),
                                    name,
                                    "rejected by user".into(),
                                    false,
                                )
                                .await;
                                return false;
                            }
                            ApprovalDecision::ApproveForSession => {
                                self.session_approved.insert(name.clone());
                                router.set_approved(true);
                                break;
                            }
                            ApprovalDecision::Approve => {
                                router.set_approved(true);
                                break;
                            }
                        },
                        Some(Op::Interrupt) | Some(Op::Shutdown) | None => {
                            self.session.push(Message::tool_result(
                                "interrupted before approval",
                                &call_id,
                            ));
                            self.emit_tool_end(
                                turn,
                                call_id.clone(),
                                name,
                                "interrupted before approval".into(),
                                false,
                            )
                            .await;
                            return true;
                        }
                        Some(_) => {} // ignore unrelated ops while awaiting approval
                    }
                }
            }
        }

        // pre_tool hook — may block.
        if self.fire_hooks("pre_tool", &name, serde_json::json!({ "turn": turn.0, "tool": name.clone(), "args": arguments.clone() })).await {
            self.session.push(Message::tool_result("blocked by pre_tool hook", &call_id));
            self.emit_tool_end(turn, call_id.clone(), name, "blocked by pre_tool hook".into(), false).await;
            return false;
        }

        // Doom-loop guard (opencode-style): the SAME tool with byte-identical
        // input 3× in a row is never progress — stop executing it and force a
        // change of approach instead of burning the turn.
        {
            let sig = format!("{name}:{arguments}");
            self.turn_tool_signatures.insert(sig.clone());
            if sig == self.last_tool_sig {
                self.last_tool_reps += 1;
            } else {
                self.last_tool_sig = sig;
                self.last_tool_reps = 0;
            }
            if self.last_tool_reps >= 2 {
                let msg = format!(
                    "Loop detected: you've called `{name}` with identical input {} times in a row. \
The result will not change. STOP repeating this call — change your approach (different arguments, \
a different tool, or ask_user if you're blocked).",
                    self.last_tool_reps + 1
                );
                self.session
                    .push(Message::tool_result(msg.clone(), &call_id));
                self.emit_tool_end(turn, call_id.clone(), name, msg, false)
                    .await;
                return false;
            }
        }

        // Break the re-read loop: if the model reads a file it already read this
        // turn, don't re-read — its content is already in context. Push it to act.
        if name == "read_file" {
            if let Some(p) = arguments["path"].as_str() {
                if !self.turn_reads.insert(p.to_string()) {
                    let msg = format!(
                        "You already read `{p}` earlier this turn — its full content is in the conversation above. \
Do NOT read it again. Proceed now: make the edits with the edit/write_file tools."
                    );
                    self.session.push(Message::tool_result(
                        format!("[tool read_file]\n{msg}"),
                        &call_id,
                    ));
                    self.emit_tool_end(turn, call_id.clone(), name, msg, true)
                        .await;
                    return false;
                }
            }
        }

        // `edit` = surgical string replace; validate then handle it like a write.
        let mut edit_error: Option<String> = None;
        if name == "edit" {
            match self.compute_edit(&arguments) {
                Ok((path, new_content)) => {
                    arguments = serde_json::json!({ "path": path, "content": new_content });
                }
                Err(e) => edit_error = Some(e),
            }
        }
        let is_write = (name == "write_file" || name == "edit") && edit_error.is_none();

        // Snapshot the target before a write so the change can be rewound + diffed.
        let mut write_ctx: Option<(String, String, u64)> = None; // (path, prior, checkpoint)
        if is_write {
            if let Some(path) = arguments["path"].as_str() {
                let abs = self.workspace.join(path);
                let prior = std::fs::read_to_string(&abs).unwrap_or_default();
                let id = self.snapshot_checkpoint(&abs).await;
                self.emit(Event::CheckpointCreated {
                    turn,
                    id,
                    label: format!("write {path}"),
                })
                .await;
                write_ctx = Some((path.to_string(), prior, id));
            }
        }

        // Structured Git broker, then browser automation, memory, MCP, and native sandbox.
        let (output, ok) = if let Some(result) =
            git_tools::execute(&self.workspace, &name, &arguments).await
        {
            result
        } else if let Some(r) = self.handle_browser_tool(&name, &arguments).await {
            r
        } else if name == "remember" {
            let mem = memory::Memory::new(&self.workspace);
            match mem.remember(arguments["text"].as_str().unwrap_or("")) {
                Ok(()) => ("remembered".to_string(), true),
                Err(e) => (format!("memory error: {e}"), false),
            }
        } else if name == "save_skill" {
            let mem = memory::Memory::new(&self.workspace);
            let n = arguments["name"].as_str().unwrap_or("skill");
            let c = arguments["content"].as_str().unwrap_or("");
            match mem.save_skill(n, c) {
                Ok(()) => (format!("saved skill '{n}'"), true),
                Err(e) => (format!("memory error: {e}"), false),
            }
        } else if name == "todo_write" {
            let items: Vec<(String, String)> = arguments["todos"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|t| {
                            let c = t["content"].as_str()?.to_string();
                            let s =
                                normalize_todo_status(t["status"].as_str().unwrap_or("pending"));
                            Some((c, s))
                        })
                        .collect()
                })
                .unwrap_or_default();
            self.turn_todos = items.clone();
            self.emit(Event::Todos {
                items: items.clone(),
            })
            .await;
            let done = items.iter().filter(|(_, s)| s == "completed").count();
            (
                format!("todo list updated ({done}/{} done)", items.len()),
                true,
            )
        } else if name == "design_read_system" {
            let path = arguments["path"].as_str().unwrap_or("DESIGN.md");
            match sandbox::check_read(
                self.config.sandbox,
                &self.workspace,
                std::path::Path::new(path),
            ) {
                sandbox::PathCheck::Denied(why) => (why, false),
                sandbox::PathCheck::Ok(abs) => match std::fs::read_to_string(&abs) {
                    Ok(content) => {
                        let system = parse_design_markdown(&content);
                        let tokens = extract_source_tokens(&content, path);
                        let contract = build_design_token_contract(&tokens);
                        match serde_json::to_string_pretty(&serde_json::json!({
                            "system": system,
                            "token_contract": contract
                        })) {
                            Ok(payload) => (payload, true),
                            Err(e) => (
                                format!("design_read_system serialization error: {e}"),
                                false,
                            ),
                        }
                    }
                    Err(e) => (format!("design_read_system read error: {e}"), false),
                },
            }
        } else if name == "design_extract_tokens" {
            let content = arguments["content"].as_str().unwrap_or("");
            let source = arguments["source"].as_str().unwrap_or("inline-design");
            let tokens = extract_source_tokens(content, source);
            let contract = build_design_token_contract(&tokens);
            match serde_json::to_string_pretty(&contract) {
                Ok(payload) => (payload, true),
                Err(e) => (
                    format!("design_extract_tokens serialization error: {e}"),
                    false,
                ),
            }
        } else if name == "design_review" {
            match serde_json::from_value::<DesignReviewInput>(arguments.clone()) {
                Ok(input) => {
                    let review = review_design_selection(input);
                    let ok = review.ok;
                    self.emit(Event::DesignReviewCompleted {
                        turn,
                        review: Box::new(review.clone()),
                    })
                    .await;
                    match serde_json::to_string_pretty(&review) {
                        Ok(payload) => (payload, ok),
                        Err(e) => (format!("design_review serialization error: {e}"), false),
                    }
                }
                Err(e) => (format!("design_review parse error: {e}"), false),
            }
        } else if name == "design_propose_patch" {
            match serde_json::from_value::<DesignPatchProposal>(arguments.clone()) {
                Ok(mut proposal) => {
                    proposal.instruction = build_patch_instruction(&proposal);
                    self.emit(Event::DesignPatchProposed {
                        turn,
                        proposal: Box::new(proposal.clone()),
                    })
                    .await;
                    (proposal.instruction, true)
                }
                Err(e) => (format!("design_propose_patch parse error: {e}"), false),
            }
        } else if name == "design_snapshot" {
            let url = tool_arg_string(&arguments, "url");
            let note = tool_arg_string(&arguments, "note");
            self.emit(Event::DesignSnapshotRequested {
                turn,
                url: url.clone(),
                note,
            })
            .await;
            (format!("requested Design Workbench snapshot: {url}"), true)
        } else if name == "render_ui_spec" {
            match arguments.get("spec") {
                Some(value) => match serde_json::from_value::<UiSpec>(value.clone()) {
                    Ok(spec) => match spec.validate() {
                        Ok(()) => {
                            let title = spec
                                .title
                                .clone()
                                .or_else(|| spec.root.props.title.clone())
                                .unwrap_or_else(|| "Untitled UI".to_string());
                            if let Some(store) = &self.session_store {
                                if let Ok(payload) = serde_json::to_string(&spec) {
                                    let _ = store.append("ui_spec", &payload);
                                }
                            }
                            self.emit(Event::UiSpec {
                                turn,
                                spec: Box::new(spec),
                            })
                            .await;
                            (format!("rendered UI spec: {title}"), true)
                        }
                        Err(e) => (format!("render_ui_spec validation error: {e}"), false),
                    },
                    Err(e) => (format!("render_ui_spec parse error: {e}"), false),
                },
                None => ("render_ui_spec: missing 'spec'".to_string(), false),
            }
        } else if name == "shell" {
            let command = arguments["command"].as_str().unwrap_or("").to_string();
            let command_id = if call_id.trim().is_empty() {
                format!("shell-{}-{}", turn.0, self.next_request_id())
            } else {
                format!("shell-{}-{call_id}", turn.0)
            };
            self.emit(Event::CommandStarted {
                turn,
                command_id: command_id.clone(),
                worker_id: worker_id.map(str::to_string),
                command,
                cwd: self.workspace.display().to_string(),
                background: false,
            })
            .await;
            let started = std::time::Instant::now();
            let (output, ok) = router
                .exec_shell_streaming(
                    &arguments,
                    turn,
                    command_id.clone(),
                    worker_id.map(str::to_string),
                    self.event_tx.clone(),
                )
                .await;
            let exit_code = shell_exit_code(&output);
            self.emit(Event::CommandFinished {
                turn,
                command_id,
                worker_id: worker_id.map(str::to_string),
                ok,
                exit_code,
                duration_ms: started.elapsed().as_millis() as u64,
            })
            .await;
            (output, ok)
        } else if name == "web_search" {
            web_search(arguments["query"].as_str().unwrap_or("")).await
        } else if name == "fetch_url" {
            fetch_url(arguments["url"].as_str().unwrap_or("")).await
        } else if name == "codebase_search" {
            // Heavy synchronous walk + scoring — run off the async runtime so it
            // can never block the engine event loop (the "stuck" symptom), and
            // bound it so a huge repo can't hang the turn.
            let ws = self.workspace.clone();
            let q = arguments["query"].as_str().unwrap_or("").to_string();
            // Persistent incremental index (Augment-style): instant after the first
            // build, refreshes only changed files.
            let job = tokio::task::spawn_blocking(move || (index::search(&ws, &q), true));
            match tokio::time::timeout(std::time::Duration::from_secs(20), job).await {
                Ok(Ok(r)) => r,
                Ok(Err(_)) => ("codebase_search: internal error".into(), false),
                Err(_) => (
                    "codebase_search: timed out (repository too large). Narrow the query or use `search`/`read_file` on a specific path.".into(),
                    false,
                ),
            }
        } else if name == "delegate_task" {
            let task = arguments["task"].as_str().unwrap_or("").trim().to_string();
            if task.is_empty() {
                ("delegate_task: 'task' is required".to_string(), false)
            } else if self
                .bg_tasks_running
                .load(std::sync::atomic::Ordering::SeqCst)
                >= 3
            {
                (
                    "delegate_task: 3 background subagents already running — finish or wait for one before delegating more.".to_string(),
                    false,
                )
            } else {
                match (
                    self.subagent_worker_engine(),
                    self.bg_done_tx.clone(),
                    self.bg_spawn_tx.clone(),
                ) {
                    (Ok(worker), Some(done_tx), Some(spawn_tx)) => {
                        self.bg_task_seq += 1;
                        let handle = format!("bg-{}", self.bg_task_seq);
                        let profile = match arguments["profile"].as_str() {
                            Some(p) if !p.is_empty() => subagent_profile_for(
                                &format!("{p}: {task}"),
                                &self.config.provider,
                                &self.config.reasoning_effort,
                            ),
                            _ => subagent_profile_for(
                                &task,
                                &self.config.provider,
                                &self.config.reasoning_effort,
                            ),
                        };
                        let system = format!(
                            "You are a BACKGROUND subagent ({}). Complete exactly this task and report a compact, self-contained result — the parent agent reads it later with no other context.",
                            profile.id
                        );
                        let counter = self.bg_tasks_running.clone();
                        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let sent = spawn_tx
                            .send(BgDelegation {
                                worker: Box::new(worker),
                                system,
                                task: task.clone(),
                                worker_id: format!("bgtask-{handle}"),
                                profile,
                                handle: handle.clone(),
                                done_tx,
                                counter: counter.clone(),
                                notify: None,
                            })
                            .is_ok();
                        if sent {
                            (
                                format!("delegate_task: {handle} started in the background — its result will re-enter the conversation automatically."),
                                true,
                            )
                        } else {
                            counter.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                            ("delegate_task: dispatcher unavailable".to_string(), false)
                        }
                    }
                    (Err(e), _, _) => (format!("delegate_task: worker init failed: {e}"), false),
                    _ => (
                        "delegate_task: engine not ready for background delegation".to_string(),
                        false,
                    ),
                }
            }
        } else if name == "execute_code" {
            let code = arguments["code"].as_str().unwrap_or("").to_string();
            if code.trim().is_empty() {
                ("execute_code: 'code' is required".to_string(), false)
            } else {
                ptc::run(&self.workspace, self.config.sandbox, &code).await
            }
        } else if name == "session_search" {
            let q = arguments["query"].as_str().unwrap_or("").trim().to_string();
            let sid = arguments["session_id"]
                .as_str()
                .unwrap_or("")
                .trim()
                .to_string();
            let ws = self.workspace.clone();
            let cur = self
                .session_store
                .as_ref()
                .map(|s| s.id.clone())
                .unwrap_or_default();
            // DB calls are sync (worker-thread channel) — keep them off the
            // engine's async loop, and bound the whole recall.
            let job =
                tokio::task::spawn_blocking(move || db::session_recall_text(&ws, &q, &sid, &cur));
            match tokio::time::timeout(std::time::Duration::from_secs(10), job).await {
                Ok(Ok(text)) => (text, true),
                Ok(Err(_)) => ("session_search: internal error".into(), false),
                Err(_) => ("session_search: timed out".into(), false),
            }
        } else if name == "tool_search" {
            let q = arguments["query"].as_str().unwrap_or("");
            (crate::search_deferred(&self.deferred_tools, q), true)
        } else if name == "tool_describe" {
            let n = arguments["name"].as_str().unwrap_or("");
            (crate::describe_deferred(&self.deferred_tools, n), true)
        } else if name == "tool_call" {
            // Only reached when the unwrap above rejected the inner name.
            (
                "tool_call: unknown or non-deferred tool name — find it with `tool_search` first, then pass its exact name.".to_string(),
                false,
            )
        } else if name == "edit" {
            match &edit_error {
                Some(e) => (e.clone(), false),
                None => router.execute("write_file", &arguments).await,
            }
        } else if is_mcp_tool(&name) {
            self.mcp_call(&name, &arguments).await
        } else {
            router.execute(&name, &arguments).await
        };
        // hermes verify-on-stop evidence: a passing test/lint/build via the
        // engine's own shell tool proves the edits were verified this turn.
        if ok && name == "shell" {
            if let Some(c) = arguments.get("command").and_then(|v| v.as_str()) {
                if is_verification_command(c) {
                    self.turn_verify_passed = true;
                }
            }
        }
        // Remember reads so `edit` can require a prior read of the file.
        if ok && name == "read_file" {
            if let Some(p) = arguments.get("path").and_then(|v| v.as_str()) {
                self.read_files.insert(p.to_string());
            }
        }
        if ok {
            match name.as_str() {
                "browser_open" => {
                    self.emit(Event::BrowserTargetChanged {
                        turn,
                        url: tool_arg_string(&arguments, "url"),
                        note: tool_arg_string(&arguments, "note"),
                    })
                    .await;
                }
                "browser_snapshot" => {
                    self.emit(Event::BrowserSnapshotRequested {
                        turn,
                        url: tool_arg_string(&arguments, "url"),
                        note: tool_arg_string(&arguments, "note"),
                    })
                    .await;
                }
                _ => {}
            }
        }
        if ok && is_write {
            self.turn_edited = true;
            if let Some(p) = arguments["path"].as_str() {
                self.turn_edit_paths.push(p.to_string());
            }
            if let Some((path, prior, id)) = &write_ctx {
                self.emit(Event::PatchApplied {
                    turn,
                    path: path.clone(),
                })
                .await;
                let new = arguments["content"].as_str().unwrap_or("");
                let diff = unified_diff(prior, new, path);
                self.emit(Event::FileDiff {
                    turn,
                    path: path.clone(),
                    diff,
                    checkpoint: *id,
                })
                .await;
            }
        }
        // post_tool hook (informational).
        self.fire_hooks(
            "post_tool",
            &name,
            serde_json::json!({ "turn": turn.0, "tool": name.clone(), "ok": ok, "output": output.clone() }),
        )
        .await;
        // Feed the result back into the conversation so the agentic loop can
        // continue — cap huge outputs so one tool can't blow the context budget.
        let stored = if output.chars().count() > 40_000 {
            let head: String = output.chars().take(40_000).collect();
            format!("{head}\n… [output truncated]")
        } else {
            output.clone()
        };
        self.session.push(Message::tool_result(
            format!("[tool {name}]\n{stored}"),
            &call_id,
        ));
        self.emit_tool_end(turn, call_id.clone(), name, output, ok)
            .await;
        false
    }
}

async fn run_hook_command(
    workspace: &Path,
    command: &str,
    event: &str,
    matcher: &str,
    payload: serde_json::Value,
    timeout: u64,
) -> bool {
    matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(timeout),
            hook_command(workspace, command, event, matcher, payload)
        )
        .await,
        Ok(Ok(output)) if output.status.success()
    )
}

async fn hook_command(
    workspace: &Path,
    command: &str,
    event: &str,
    matcher: &str,
    payload: serde_json::Value,
) -> std::io::Result<std::process::Output> {
    tokio::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .current_dir(workspace)
        .env("OXIDE_HOOK_EVENT", event)
        .env("OXIDE_HOOK_MATCHER", matcher)
        .env("OXIDE_HOOK_PAYLOAD", payload.to_string())
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
}

/// Load pinned project instructions from AGENTS.md / CLAUDE.md (first found).
/// Snapshot the whole worktree as a git tree object WITHOUT touching the real
/// index or working tree (temp GIT_INDEX_FILE + add -A + write-tree). Returns
/// the tree sha — `git cat-file -p <sha>:<path>` then yields any file's
/// pre-turn bytes.
async fn git_baseline_tree(ws: &std::path::Path) -> Option<String> {
    // PID alone is NOT unique here: two conversations in one GUI process can
    // baseline the same workspace concurrently — sharing an index file lets
    // one call delete/overwrite the other's mid-`write-tree` (wrong tree =
    // wrong rewind bytes). A per-call nonce keeps each index private.
    static IDX_NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let idx = ws.join(format!(
        ".oxide/tmp-index-{}-{}",
        std::process::id(),
        IDX_NONCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::create_dir_all(ws.join(".oxide"));
    let add = tokio::process::Command::new("git")
        .arg("-C")
        .arg(ws)
        .env("GIT_INDEX_FILE", &idx)
        .args(["add", "-A", "."])
        .output()
        .await
        .ok()?;
    if !add.status.success() {
        let _ = std::fs::remove_file(&idx);
        return None;
    }
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(ws)
        .env("GIT_INDEX_FILE", &idx)
        .args(["write-tree"])
        .output()
        .await
        .ok();
    let _ = std::fs::remove_file(&idx);
    let out = out?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sha.is_empty() {
        None
    } else {
        Some(sha)
    }
}

fn load_project_instructions(workspace: &std::path::Path) -> Option<String> {
    let mut combined = String::new();
    // Single-file instructions — first match among the conventional names wins
    // (they're usually the same doc under different ecosystems' names).
    for name in [
        "AGENTS.md",
        "CLAUDE.md",
        ".oxide/AGENTS.md",
        ".cursorrules",
        ".windsurfrules",
    ] {
        if let Ok(text) = std::fs::read_to_string(workspace.join(name)) {
            let t = text.trim();
            if !t.is_empty() {
                combined.push_str(t);
                combined.push_str("\n\n");
                break;
            }
        }
    }
    // Rule directories (all files concatenated): Cursor `.cursor/rules/*.mdc`
    // and Oxide's own `.oxide/rules/*.md`.
    for (dir, ext) in [(".cursor/rules", "mdc"), (".oxide/rules", "md")] {
        if let Ok(rd) = std::fs::read_dir(workspace.join(dir)) {
            let mut paths: Vec<_> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|x| x.to_str()) == Some(ext))
                .collect();
            paths.sort();
            for p in paths {
                if let Ok(text) = std::fs::read_to_string(&p) {
                    let t = text.trim();
                    if !t.is_empty() {
                        combined.push_str(t);
                        combined.push_str("\n\n");
                    }
                }
            }
        }
    }
    let t = combined.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.chars().take(12000).collect())
    }
}

/// Unified diff between two file contents.
fn unified_diff(old: &str, new: &str, path: &str) -> String {
    similar::TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string()
}

/// Diff a CLI-edited file against the working tree's git baseline, returning
/// `(workspace-relative path, unified diff)`. New/untracked files render as
/// all-adds. Used both live (per edit, for streaming counts) and at turn end
/// (for the final revertable diff). The diff is capped so one huge file can't
/// blow the event payload.
async fn cli_file_diff(workspace: &std::path::Path, path: &str) -> (String, String) {
    let rel = std::path::Path::new(path)
        .strip_prefix(workspace)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.to_string());
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["diff", "--no-color", "--"])
        .arg(&rel)
        .output()
        .await;
    let mut diff = out
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    if diff.trim().is_empty() {
        if let Ok(text) = std::fs::read_to_string(workspace.join(&rel)) {
            let body: String = text.lines().take(400).map(|l| format!("+{l}\n")).collect();
            if !body.is_empty() {
                diff = format!("@@ new file @@\n{body}");
            }
        }
    }
    let diff: String = diff.chars().take(20_000).collect();
    (rel, diff)
}

async fn collect_provider_text_silent(
    provider_id: &str,
    req: TurnRequest,
) -> Result<String, String> {
    let (tx, mut rx) = mpsc::channel::<StreamItem>(STREAM_QUEUE);
    let provider = oxide_providers::build(provider_id);
    let task = tokio::spawn(async move { provider.stream(req, tx).await });
    let mut out = String::new();
    let mut timed_out = false;
    while let Some(item) =
        match tokio::time::timeout(std::time::Duration::from_secs(60), rx.recv()).await {
            Ok(item) => item,
            Err(_) => {
                timed_out = true;
                task.abort();
                None
            }
        }
    {
        match item {
            StreamItem::TextDelta(text) => out.push_str(&text),
            StreamItem::Done => break,
            StreamItem::FileChanged(_)
            | StreamItem::CommandStarted { .. }
            | StreamItem::CommandOutput { .. }
            | StreamItem::CommandFinished { .. }
            | StreamItem::BackgroundJob { .. }
            | StreamItem::ReasoningDelta(_)
            | StreamItem::ReasoningItem(_)
            | StreamItem::ToolInputDelta { .. }
            | StreamItem::ToolCall { .. }
            | StreamItem::Notice(_)
            | StreamItem::CliSession(_)
            | StreamItem::Usage { .. }
            | StreamItem::RateLimit { .. } => {}
        }
    }
    if timed_out {
        return Err(format!("{provider_id}: self-improvement bridge timed out"));
    }
    match task.await {
        Ok(Ok(())) => Ok(out),
        Ok(Err(err)) => Err(err.to_string()),
        Err(err) => Err(err.to_string()),
    }
}

fn parse_self_improvement_capture(text: &str) -> Option<SelfImproveCapture> {
    let trimmed = text.trim();
    if let Ok(capture) = serde_json::from_str::<SelfImproveCapture>(trimmed) {
        return Some(capture);
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<SelfImproveCapture>(&trimmed[start..=end]).ok()
}

fn clean_memory_fact(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn looks_like_secret(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("password=")
        || lower.contains("passwd=")
        || lower.contains("api_key=")
        || lower.contains("apikey=")
        || lower.contains("secret=")
        || lower.contains("token=")
        || lower.contains("bearer ")
        || lower.contains("sk-")
}

fn shell_exit_code(output: &str) -> Option<i32> {
    let line = output.lines().find(|line| line.starts_with("[exit "))?;
    let raw = line
        .trim_start_matches("[exit ")
        .split_whitespace()
        .next()?;
    raw.parse::<i32>().ok()
}

fn tool_arg_string(args: &serde_json::Value, key: &str) -> String {
    args[key].as_str().unwrap_or("").trim().to_string()
}

fn normalize_todo_status(status: &str) -> String {
    match status
        .trim()
        .to_ascii_lowercase()
        .replace('-', "_")
        .as_str()
    {
        "completed" | "complete" | "done" => "completed".to_string(),
        "in_progress" | "in progress" | "active" | "doing" => "in_progress".to_string(),
        _ => "pending".to_string(),
    }
}

#[cfg(test)]
mod map_test {
    use oxide_config::{McpEnvVar, McpServerConfig};
    use oxide_protocol::ToolSpec;

    #[tokio::test]
    async fn event_bus_snapshot_then_live_tail_by_seq() {
        use oxide_protocol::{Event, TurnId};
        let bus = super::EventBus::new();
        let s0 = bus.publish(&Event::Ready {
            harness: "h".into(),
        });
        let s1 = bus.publish(&Event::TurnStarted { turn: TurnId(1) });
        // A late subscriber gets BOTH prior events in the snapshot, ordered by seq.
        let (snap, mut rx) = bus.subscribe(0);
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].0, s0);
        assert_eq!(snap[1].0, s1);
        assert!(s1 > s0);
        // A subsequent event arrives on the live tail with the next seq.
        let s2 = bus.publish(&Event::TurnFinished { turn: TurnId(1) });
        let (rseq, _ev) = rx.recv().await.unwrap();
        assert_eq!(rseq, s2);
        // `after` filters the snapshot to only events newer than an applied seq.
        let (snap2, _rx2) = bus.subscribe(s1 + 1);
        assert_eq!(snap2.len(), 1);
        assert_eq!(snap2[0].0, s2);
    }

    #[tokio::test]
    async fn event_socket_bridges_to_a_separate_consumer() {
        use oxide_protocol::{Event, TurnId};
        let bus = super::EventBus::new();
        // Published BEFORE any client connects → must arrive via the snapshot.
        bus.publish(&Event::Ready {
            harness: "h".into(),
        });
        let path = std::env::temp_dir().join(format!("oxide-evsock-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        tokio::spawn(super::serve_events(path.clone(), bus.clone()));
        let mut rx = super::subscribe_over_socket(path.clone());
        // Snapshot delivers the pre-connect event.
        let (s0, _) = rx.recv().await.expect("snapshot event");
        // A live event after the client connected reaches it too, with a higher seq.
        bus.publish(&Event::TurnFinished { turn: TurnId(1) });
        let (s1, _) = rx.recv().await.expect("live event");
        assert!(s1 > s0);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn todo_status_variants_normalize_for_ui() {
        assert_eq!(super::normalize_todo_status("done"), "completed");
        assert_eq!(super::normalize_todo_status("in-progress"), "in_progress");
        assert_eq!(super::normalize_todo_status("blocked"), "pending");
    }

    #[test]
    fn bg_jobs_replay_last_row_per_id_and_only_fresh_files() {
        let fresh = std::env::temp_dir().join(format!("oxide-bgjob-{}.out", std::process::id()));
        std::fs::write(&fresh, "tick").unwrap();
        let fresh_s = fresh.display().to_string();
        let row = |id: &str, path: &str| {
            (
                "bg_job".to_string(),
                serde_json::json!({"id": id, "command": "watch ci", "path": path}).to_string(),
            )
        };
        let rows = vec![
            ("user".to_string(), "hi".to_string()),
            row("a", "/nonexistent/old.out"), // superseded by the later "a" row
            row("b", "/nonexistent/gone.out"), // file missing → dropped
            row("a", &fresh_s),
            ("bg_job".to_string(), "not-json".to_string()), // malformed → skipped
        ];
        let jobs = super::replayable_bg_jobs(rows, std::time::Duration::from_secs(3600));
        assert_eq!(
            jobs,
            vec![("a".to_string(), "watch ci".to_string(), fresh_s)]
        );
        std::fs::remove_file(&fresh).ok();
    }

    #[test]
    fn self_improvement_capture_parses_plain_and_fenced_json() {
        let plain = r#"{"facts":["Oxide CLI edits are reconstructed from git diff"],"skills":[]}"#;
        let parsed = super::parse_self_improvement_capture(plain).unwrap();
        assert_eq!(parsed.facts.len(), 1);

        let fenced = "```json\n{\"facts\":[],\"skills\":[{\"name\":\"release-check\",\"content\":\"# Release check\\nVerify assets before finishing.\"}]}\n```";
        let parsed = super::parse_self_improvement_capture(fenced).unwrap();
        assert_eq!(parsed.skills[0].name, "release-check");
    }

    #[test]
    fn map_shows_structure() {
        let ws = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let m = super::project_map(ws);
        assert!(
            m.contains("crates/") && m.contains("Cargo.toml"),
            "map:\n{m}"
        );
        eprintln!("--- project map sample ---\n{}", &m[..m.len().min(400)]);
    }

    #[test]
    fn codex_mcp_config_fields_are_preserved() {
        let value = toml::from_str::<toml::Value>(
            r#"
command = "npx"
args = ["-y", "pkg"]
enabled = false
cwd = "/tmp/project"
env = { STATIC_TOKEN = "abc" }
env_vars = ["LOCAL_TOKEN", { name = "REMOTE_TOKEN", source = "remote" }]
bearer_token_env_var = "HTTP_TOKEN"
http_headers = { "X-Team" = "oxide" }
env_http_headers = { "X-Secret" = "SECRET_ENV" }
startup_timeout_sec = 12
tool_timeout_sec = 34
enabled_tools = ["read"]
disabled_tools = ["write"]
required = true
"#,
        )
        .unwrap();

        let server = super::toml_mcp_server("context7", &value, "Codex project config");

        assert_eq!(server.name, "context7");
        assert_eq!(server.command, "npx");
        assert_eq!(server.args, vec!["-y", "pkg"]);
        assert!(!server.enabled);
        assert_eq!(server.cwd, "/tmp/project");
        assert_eq!(
            server.env.get("STATIC_TOKEN").map(String::as_str),
            Some("abc")
        );
        assert!(
            matches!(server.env_vars.first(), Some(McpEnvVar::Name(name)) if name == "LOCAL_TOKEN")
        );
        assert!(matches!(
            server.env_vars.get(1),
            Some(McpEnvVar::Named { name, source }) if name == "REMOTE_TOKEN" && source == "remote"
        ));
        assert_eq!(server.bearer_token_env_var, "HTTP_TOKEN");
        assert_eq!(
            server.http_headers.get("X-Team").map(String::as_str),
            Some("oxide")
        );
        assert_eq!(
            server.env_http_headers.get("X-Secret").map(String::as_str),
            Some("SECRET_ENV")
        );
        assert_eq!(server.startup_timeout_sec, Some(12));
        assert_eq!(server.tool_timeout_sec, Some(34));
        assert_eq!(server.enabled_tools, vec!["read"]);
        assert_eq!(server.disabled_tools, vec!["write"]);
        assert!(server.required);
    }

    #[test]
    fn claude_mcp_oauth_token_uses_matching_server_entry() {
        let json = r#"{
            "claudeAiOauth": {},
            "mcpOAuth": {
                "other|abc": { "accessToken": "wrong" },
                "supabase|59e4938976c99701": { "accessToken": "token-123", "refreshToken": "refresh-123" }
            }
        }"#;

        assert_eq!(
            super::claude_mcp_oauth_token_from_json(json, "supabase").as_deref(),
            Some("token-123")
        );
        assert_eq!(
            super::claude_mcp_oauth_token_from_json(json, "github"),
            None
        );
    }

    #[test]
    fn codex_mcp_credential_token_checks_server_and_url() {
        let json = r#"{
            "server_name": "supabase",
            "url": "https://mcp.supabase.com/mcp",
            "token_response": {
                "access_token": "token-456",
                "refresh_token": "refresh-456"
            }
        }"#;

        assert_eq!(
            super::codex_mcp_credential_token_from_json(
                json,
                "supabase",
                "https://mcp.supabase.com/mcp/"
            )
            .as_deref(),
            Some("token-456")
        );
        assert_eq!(
            super::codex_mcp_credential_token_from_json(json, "supabase", "https://example.com"),
            None
        );
    }

    #[test]
    fn keychain_dump_parser_finds_matching_service_accounts() {
        let dump = r#"
keychain: "/Users/me/Library/Keychains/login.keychain-db"
class: "genp"
attributes:
    "acct"<blob>="supabase|59e4938976c99701"
    "svce"<blob>="Codex MCP Credentials"
keychain: "/Users/me/Library/Keychains/login.keychain-db"
class: "genp"
attributes:
    "acct"<blob>="github|abc"
    "svce"<blob>="Codex MCP Credentials"
keychain: "/Users/me/Library/Keychains/login.keychain-db"
class: "genp"
attributes:
    "acct"<blob>="supabase|ignored"
    "svce"<blob>="Other Service"
"#;

        assert_eq!(
            super::keychain_accounts_for_service_from_dump(
                dump,
                "Codex MCP Credentials",
                "supabase"
            ),
            vec!["supabase|59e4938976c99701".to_string()]
        );
    }

    #[test]
    fn verification_command_classifier_and_code_filter() {
        assert!(crate::is_verification_command("cargo test -p oxide-core"));
        assert!(crate::is_verification_command(
            "export PATH=x && cargo clippy --workspace"
        ));
        assert!(crate::is_verification_command(
            "npm run test -- --watch=false"
        ));
        assert!(crate::is_verification_command("ruff check ."));
        assert!(!crate::is_verification_command("git status"));
        assert!(!crate::is_verification_command("ls -la"));
        assert!(crate::edits_touch_code(&[
            "src/lib.rs".into(),
            "README.md".into()
        ]));
        assert!(!crate::edits_touch_code(&[
            "README.md".into(),
            "docs/x.txt".into()
        ]));
    }

    #[test]
    fn tool_search_bridge_ranks_and_describes() {
        let deferred = vec![
            ToolSpec::new(
                "mcp__gh__create_issue",
                "Create a GitHub issue in a repository",
            )
            .params(serde_json::json!({"type":"object","properties":{"title":{"type":"string"}}})),
            ToolSpec::new("mcp__db__query", "Run a SQL query against the database"),
        ];
        let out = crate::search_deferred(&deferred, "github issue");
        assert!(out.contains("mcp__gh__create_issue"));
        // Name+desc hit outranks a non-matching tool (which is filtered out).
        assert!(!out.contains("mcp__db__query"));
        let d = crate::describe_deferred(&deferred, "mcp__gh__create_issue");
        assert!(d.contains("parameters schema"));
        assert!(d.contains("title"));
        assert!(crate::describe_deferred(&deferred, "nope").contains("no deferred tool"));
        assert!(crate::search_deferred(&[], "x").contains("no tools are deferred"));
        // Char accounting counts name + description + schema.
        assert!(crate::deferrable_schema_chars(&deferred) > 60);
    }

    #[test]
    fn progressive_bridge_tools_are_registered_with_the_router() {
        let tools = crate::tools_for_routing(Vec::new(), true);
        let router = crate::ToolRouter::new(
            oxide_protocol::ApprovalPolicy::OnRequest,
            oxide_protocol::SandboxPolicy::WorkspaceWrite,
            std::path::PathBuf::from("."),
            &tools,
        );

        assert!(matches!(router.route("tool_search"), crate::Routed::Run));
        assert!(matches!(router.route("tool_describe"), crate::Routed::Run));
        assert!(matches!(
            router.route("tool_call"),
            crate::Routed::NeedsApproval
        ));
    }

    #[test]
    fn external_mcp_reference_resolves_without_persisting_secrets() {
        let discovered = McpServerConfig {
            name: "github".into(),
            command: "npx".into(),
            source: "Codex user config".into(),
            env: std::collections::BTreeMap::from([("TOKEN".into(), "secret".into())]),
            ..McpServerConfig::default()
        };
        let reference = discovered.as_external_reference();

        let resolved = super::resolve_external_mcp_reference(&reference, &[discovered]).unwrap();

        assert_eq!(resolved.command, "npx");
        assert_eq!(
            resolved.env.get("TOKEN").map(String::as_str),
            Some("secret")
        );
        assert!(!resolved.external_ref);
        let persisted = toml::to_string(&reference).unwrap();
        assert!(!persisted.contains("secret"));
    }

    #[test]
    fn external_mcp_reference_never_falls_back_to_a_different_source() {
        let reference = McpServerConfig {
            name: "shared".into(),
            source: "Codex user config".into(),
            external_ref: true,
            ..McpServerConfig::default()
        };
        let replacement = McpServerConfig {
            name: "shared".into(),
            command: "malicious-replacement".into(),
            source: "Claude Desktop".into(),
            ..McpServerConfig::default()
        };

        assert!(super::resolve_external_mcp_reference(&reference, &[replacement]).is_none());
    }

    #[test]
    fn mcp_tool_filter_uses_bare_tool_names() {
        let server = McpServerConfig {
            name: "fs".to_string(),
            enabled_tools: vec!["read".to_string(), "write".to_string()],
            disabled_tools: vec!["write".to_string()],
            ..McpServerConfig::default()
        };
        let tools = vec![
            ToolSpec::new("mcp__fs__read", "read"),
            ToolSpec::new("mcp__fs__write", "write"),
            ToolSpec::new("mcp__fs__list", "list"),
        ];

        let filtered = super::filter_mcp_tools(&server, tools);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "mcp__fs__read");
    }

    #[test]
    fn subagent_profile_limits_reviewer_tools() {
        let profile = super::subagent_profile_for("review the diff for risks", "codex", "medium");

        assert_eq!(profile.id, "reviewer");
        assert_eq!(profile.toolset, super::WorkerToolset::ReviewReadOnly);
        assert!(profile.toolset.allows("read_file"));
        assert!(profile.toolset.allows("web_search"));
        assert!(!profile.toolset.allows("shell"));
        assert!(!profile.toolset.allows("edit"));
        assert!(!profile.toolset.allows("write_file"));
        assert_eq!(profile.effort, "high");
    }

    #[test]
    fn subagent_profile_limits_tester_to_read_only_tools() {
        let profile = super::subagent_profile_for("run tests and verify", "codex", "medium");

        assert_eq!(profile.id, "tester");
        assert_eq!(profile.toolset, super::WorkerToolset::VerifyReadOnly);
        assert!(profile.toolset.allows("read_file"));
        assert!(profile.toolset.allows("browser_read"));
        assert!(!profile.toolset.allows("shell"));
        assert!(!profile.toolset.allows("browser_navigate"));
        assert!(!profile.toolset.allows("browser_screenshot"));
        assert!(!profile.toolset.allows("edit"));
        assert!(!profile.toolset.allows("write_file"));
    }

    #[test]
    fn subagent_profile_limits_explorer_to_read_only_research_tools() {
        let profile = super::subagent_profile_for("explore provider catalog", "codex", "medium");

        assert_eq!(profile.id, "explorer");
        assert_eq!(profile.toolset, super::WorkerToolset::ExploreReadOnly);
        assert!(profile.toolset.allows("read_file"));
        assert!(profile.toolset.allows("codebase_search"));
        assert!(profile.toolset.allows("web_search"));
        assert!(!profile.toolset.allows("shell"));
        assert!(!profile.toolset.allows("edit"));
        assert!(!profile.toolset.allows("mcp__fs__write"));
    }

    #[test]
    fn review_gate_rejects_done_with_gap_markers() {
        assert!(super::review_passes_gate("DONE\nNo issues found."));
        assert!(!super::review_passes_gate(
            "DONE\nGAPS:\n- Missing regression test"
        ));
        assert!(!super::review_passes_gate(
            "DONE\nIssue: reviewer still found a race"
        ));
        assert!(!super::review_passes_gate("DONE but there are gaps"));
        assert!(!super::review_passes_gate("GAPS\n- incomplete"));
    }

    #[test]
    fn subagent_system_block_includes_default_high_agency_mode() {
        let profile = super::WorkerProfile::implementer("codex", "medium");
        let block = super::worker_profile_system_block(&profile, 8);

        assert!(block.contains("Sub-agent default operating mode"));
        assert!(block.contains("Operate with high agency"));
        assert!(block.contains("staying within safety, permission, sandbox, and tool policies"));
        assert!(block.contains("Sub-agent profile: implementer"));
        assert!(block.contains("8 tool(s) exposed"));
    }

    #[test]
    fn registry_loads_workspace_harnesses_by_default() {
        let root = std::env::temp_dir().join(format!(
            "oxide-workspace-harness-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let harness_dir = root.join("harnesses");
        std::fs::create_dir_all(&harness_dir).unwrap();
        std::fs::write(
            harness_dir.join("coding.toml"),
            r#"
id = "coding"
name = "Focused Coding"
system_prompt = "Test coding harness"

[[tools]]
name = "read_file"
description = "Read a file"
mutating = false
"#,
        )
        .unwrap();

        let config = oxide_config::Config {
            workspace: Some(root.clone()),
            harness: "coding".to_string(),
            ..oxide_config::Config::default()
        };

        let registry = super::registry_from_config(&config).unwrap();
        assert!(registry.get("coding").is_some());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn adaptive_tool_budget_extends_only_for_new_progress() {
        let mut budget = super::AdaptiveToolBudget::new(4);

        assert_eq!(
            budget.after_tool_round(3, 1, false, false),
            super::ToolBudgetDecision::Continue
        );
        assert_eq!(
            budget.after_tool_round(4, 2, false, false),
            super::ToolBudgetDecision::Extended { new_limit: 12 }
        );
        assert_eq!(
            budget.after_tool_round(12, 2, false, false),
            super::ToolBudgetDecision::Stop
        );
    }

    #[test]
    fn adaptive_tool_budget_rewards_pending_completion_work_but_keeps_ceiling() {
        let mut budget = super::AdaptiveToolBudget::new(24);

        assert_eq!(
            budget.after_tool_round(24, 5, true, true),
            super::ToolBudgetDecision::Extended { new_limit: 40 }
        );
        assert_eq!(budget.emergency_limit, 72);
    }

    #[test]
    fn harness_skill_routes_match_user_intent_without_short_false_positives() {
        let registry = oxide_harness::Registry::with_builtins();
        let harness = registry.get("default").unwrap();

        let frontend = super::selected_skill_route(harness, "audit animasi cursor di GUI").unwrap();
        assert_eq!(frontend.id, "frontend");
        assert!(super::render_skill_route(&frontend).contains("frontend workflow"));

        let review = super::selected_skill_route(harness, "review bug dan risiko").unwrap();
        assert_eq!(review.id, "review");

        assert!(super::selected_skill_route(harness, "write a migration guide").is_none());
    }
}
