//! The Oxide engine.
//!
//! A single async task owns the conversation, the active harness, and the
//! provider, and exposes itself purely through an [`Op`] inbox and an [`Event`]
//! outbox. Any frontend — TUI, GUI, headless, RPC — is just a pair of channel
//! ends. This decoupling is what lets the same engine power both a terminal and
//! a desktop app, and lets behavior be swapped via harnesses at runtime.
//!
//! ```text
//!   frontend ──Op──▶  [ Engine task ]  ──Event──▶ frontend
//!                          │
//!                  Harness (prompt+tools)
//!                          │
//!                  Provider (streaming)        ToolRouter ─▶ sandbox (Fase 2)
//! ```

mod browser;
mod commands;
mod context;
mod hooks;
mod memory;
mod sandbox;
mod store;
mod tools;
pub use tools::{Routed, ToolRouter};

use oxide_config::Config;
use oxide_config::McpServerConfig;

/// Recursively collect candidate source files (skips vendor/build dirs).
fn collect_code_files(root: &Path, out: &mut Vec<PathBuf>) {
    if out.len() > 1200 {
        return;
    }
    let skip = [".git", "target", "node_modules", ".oxide", "dist", "build", ".next", "vendor", ".venv", "__pycache__"];
    let Ok(rd) = std::fs::read_dir(root) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') && name != ".cursorrules" {
            continue;
        }
        if p.is_dir() {
            if !skip.contains(&name.as_str()) {
                collect_code_files(&p, out);
            }
        } else {
            let ext_ok = p.extension().and_then(|x| x.to_str()).map(|x| {
                matches!(x, "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "java" | "c" | "h" | "cpp" | "hpp" | "rb" | "php" | "swift" | "kt" | "cs" | "scala" | "sh" | "toml" | "md" | "css" | "html" | "vue" | "svelte" | "json" | "sql")
            }).unwrap_or(false);
            if ext_ok {
                out.push(p);
            }
        }
    }
}

/// Ranked codebase retrieval (no embeddings): scores ~50-line chunks by how
/// many query terms they contain, returns the top snippets with `path:line`.
fn codebase_search(ws: &Path, query: &str) -> (String, bool) {
    let terms: Vec<String> = query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| w.len() > 2)
        .map(String::from)
        .collect();
    if terms.is_empty() {
        return ("codebase_search: provide a query of 1+ words".into(), false);
    }
    let mut files = Vec::new();
    collect_code_files(ws, &mut files);
    let mut hits: Vec<(i64, String, usize, String)> = Vec::new();
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        if text.len() > 400_000 {
            continue;
        }
        let lines: Vec<&str> = text.lines().collect();
        let rel = f.strip_prefix(ws).unwrap_or(&f).display().to_string();
        let (win, step) = (50usize, 40usize);
        let mut start = 0;
        while start < lines.len() {
            let end = (start + win).min(lines.len());
            let chunk = lines[start..end].join("\n");
            let cl = chunk.to_lowercase();
            let mut distinct = 0i64;
            let mut total = 0i64;
            for t in &terms {
                let c = cl.matches(t.as_str()).count() as i64;
                if c > 0 {
                    distinct += 1;
                    total += c;
                }
            }
            if distinct > 0 {
                let score = distinct * 1000 + total;
                hits.push((score, rel.clone(), start + 1, chunk));
            }
            if end == lines.len() {
                break;
            }
            start += step;
        }
    }
    if hits.is_empty() {
        return (format!("No code found for: {}", terms.join(" ")), true);
    }
    hits.sort_by(|a, b| b.0.cmp(&a.0));
    hits.dedup_by(|a, b| a.1 == b.1 && a.2.abs_diff(b.2) < 20);
    let mut out = String::new();
    for (_, path, line, chunk) in hits.into_iter().take(8) {
        let snippet: String = chunk.lines().take(16).collect::<Vec<_>>().join("\n");
        out.push_str(&format!("── {path}:{line}\n{snippet}\n\n"));
    }
    (out.chars().take(9000).collect(), true)
}

/// A browser-like HTTP client for web tools.
fn web_client() -> Option<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36")
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .ok()
}

/// Decode `%XX` percent-escapes (UTF-8).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Decode the few HTML entities that matter for plain-text output.
fn html_decode(s: &str) -> String {
    s.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">")
        .replace("&quot;", "\"").replace("&#x27;", "'").replace("&#39;", "'").replace("&nbsp;", " ")
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

/// A DuckDuckGo redirect href (`...uddg=<enc>`) → the real URL.
fn decode_ddg(href: &str) -> String {
    if let Some(i) = href.find("uddg=") {
        let v = &href[i + 5..];
        let end = v.find('&').unwrap_or(v.len());
        return percent_decode(&v[..end]);
    }
    href.trim_start_matches("//").to_string()
}

/// Extract text inside `marker … > BODY <end>` from `hay`, tags stripped.
fn extract_between(hay: &str, marker: &str, end: &str) -> Option<String> {
    let i = hay.find(marker)?;
    let after = &hay[i + marker.len()..];
    let gt = after.find('>')?;
    let body = &after[gt + 1..];
    let e = body.find(end)?;
    let t = html_decode(&strip_tags(&body[..e]));
    if t.is_empty() { None } else { Some(t) }
}

/// Web search: prefer Exa (hosted MCP, keyless, returns page content), fall
/// back to Brave HTML scraping.
async fn web_search(query: &str) -> (String, bool) {
    let q = query.trim();
    if q.is_empty() {
        return ("web_search: missing 'query'".into(), false);
    }
    match exa_search(q).await {
        Ok(text) if !text.trim().is_empty() => (text, true),
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
        .call_tool(&tool, &serde_json::json!({ "query": query, "numResults": 6 }))
        .await?;
    if !ok {
        anyhow::bail!("exa error");
    }
    Ok(text.chars().take(9000).collect())
}

/// Web search via Brave HTML (fallback). Returns ranked `title / url / snippet`.
async fn brave_search(q: &str) -> (String, bool) {
    let Some(client) = web_client() else { return ("web_search: client error".into(), false) };
    let url = format!("https://search.brave.com/search?q={}&source=web", q.replace(' ', "+"));
    let html = match client.get(&url).send().await {
        Ok(r) => match r.text().await { Ok(t) => t, Err(e) => return (format!("web_search: {e}"), false) },
        Err(e) => return (format!("web_search: {e}"), false),
    };
    let mut out = String::new();
    let mut n = 0;
    let mut rest = html.as_str();
    // Each organic web result carries `data-type="web"`.
    while let Some(i) = rest.find("data-type=\"web\"") {
        rest = &rest[i + 15..];
        let block_end = rest.find("data-type=\"").unwrap_or(rest.len().min(6000)).min(6000);
        let block = &rest[..block_end];
        let url = block
            .find("href=\"https")
            .and_then(|h| { let a = &block[h + 6..]; a.find('"').map(|e| a[..e].to_string()) });
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
            out.push_str(&format!("   {}\n", desc.chars().take(240).collect::<String>()));
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
    let Some(client) = web_client() else { return ("fetch_url: client error".into(), false) };
    let html = match client.get(u).send().await {
        Ok(r) => match r.text().await { Ok(t) => t, Err(e) => return (format!("fetch_url: {e}"), false) },
        Err(e) => return (format!("fetch_url: {e}"), false),
    };
    // Drop <script>/<style> blocks, then strip tags.
    let mut cleaned = String::with_capacity(html.len());
    let lower = html.to_ascii_lowercase();
    let mut i = 0;
    while i < html.len() {
        let drop = ["<script", "<style"].iter().find_map(|tag| {
            if lower[i..].starts_with(tag) {
                let close = if *tag == "<script" { "</script>" } else { "</style>" };
                lower[i..].find(close).map(|e| i + e + close.len())
            } else {
                None
            }
        });
        if let Some(end) = drop {
            i = end;
        } else {
            cleaned.push(html[i..].chars().next().unwrap());
            i += html[i..].chars().next().unwrap().len_utf8();
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
    let mut out: Vec<McpServerConfig> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        return out;
    };
    let mut push = |name: String, command: String, args: Vec<String>, url: String, enabled: bool| {
        if (command.is_empty() && url.is_empty()) || !seen.insert(name.clone()) {
            return;
        }
        out.push(McpServerConfig { name, command, args, url, enabled });
    };
    // Codex: ~/.codex/config.toml -> [mcp_servers.NAME]
    if let Ok(text) = std::fs::read_to_string(home.join(".codex/config.toml")) {
        if let Ok(v) = toml::from_str::<toml::Value>(&text) {
            if let Some(tbl) = v.get("mcp_servers").and_then(|x| x.as_table()) {
                for (name, e) in tbl {
                    let s = |k: &str| e.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
                    let args = e
                        .get("args")
                        .and_then(|x| x.as_array())
                        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                        .unwrap_or_default();
                    let enabled = e.get("enabled").and_then(|x| x.as_bool()).unwrap_or(true);
                    push(name.clone(), s("command"), args, s("url"), enabled);
                }
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
                if let Some(obj) = v.get("mcpServers").and_then(|x| x.as_object()) {
                    for (name, e) in obj {
                        let s = |k: &str| e.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
                        let args = e
                            .get("args")
                            .and_then(|x| x.as_array())
                            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                            .unwrap_or_default();
                        push(name.clone(), s("command"), args, s("url"), true);
                    }
                }
            }
        }
    }
    out
}
use oxide_harness::{Harness, Registry};
use oxide_mcp::{is_mcp_tool, server_of, McpClient};
use oxide_protocol::{ApprovalDecision, Event, Op, ToolSpec, TurnId};
use oxide_providers::{Message, Provider, Role, StreamItem, TurnRequest};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use store::{CheckpointStore, SessionStore};
use tokio::sync::mpsc;

const OP_QUEUE: usize = 64;
const EVENT_QUEUE: usize = 256;
const STREAM_QUEUE: usize = 256;

/// Cloneable handle a frontend uses to submit [`Op`]s into the engine.
#[derive(Clone)]
pub struct EngineHandle {
    op_tx: mpsc::Sender<Op>,
}

impl EngineHandle {
    pub async fn submit(&self, op: Op) -> anyhow::Result<()> {
        self.op_tx
            .send(op)
            .await
            .map_err(|_| anyhow::anyhow!("engine task is gone"))?;
        Ok(())
    }
}

/// Start the engine task. Returns a handle to drive it and the event stream to
/// subscribe to. The engine runs until [`Op::Shutdown`] or all handles drop.
pub fn spawn(config: Config) -> anyhow::Result<(EngineHandle, mpsc::Receiver<Event>)> {
    let (op_tx, op_rx) = mpsc::channel(OP_QUEUE);
    let (event_tx, event_rx) = mpsc::channel(EVENT_QUEUE);

    let mut registry = Registry::with_builtins();
    if let Some(dir) = &config.harness_dir {
        if let Err(e) = registry.load_dir(dir) {
            tracing::warn!(error = %e, "failed scanning harness dir");
        }
    }
    if registry.get(&config.harness).is_none() {
        anyhow::bail!(
            "configured harness '{}' not found (have: {:?})",
            config.harness,
            registry.ids()
        );
    }

    let workspace = config
        .workspace
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    // Resume reads the previous session *before* opening the new one.
    let mut history: Vec<Message> = Vec::new();
    if config.resume {
        if let Some(prev) = SessionStore::latest(&workspace) {
            if let Ok(msgs) = SessionStore::load(&prev) {
                history = msgs
                    .into_iter()
                    .map(|m| Message {
                        role: role_from_str(&m.role),
                        content: m.content,
                    })
                    .collect();
                tracing::info!(count = history.len(), "resumed prior session");
            }
        }
    }

    let session_store = if config.persist {
        match SessionStore::open(&workspace) {
            Ok(s) => Some(s),
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
        next_approval: 1,
        session_approved: HashSet::new(),
        workspace,
        session_store,
        checkpoints: CheckpointStore::default(),
        mcp_clients: Vec::new(),
        mcp_tools: Vec::new(),
        browser: None,
        ctx_window: None,
        read_files: std::collections::HashSet::new(),
        event_tx,
    };

    tokio::spawn(engine.run(op_rx));
    Ok((EngineHandle { op_tx }, event_rx))
}

fn role_from_str(s: &str) -> Role {
    match s {
        "system" => Role::System,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        _ => Role::User,
    }
}

struct Engine {
    config: Config,
    registry: Registry,
    provider: Box<dyn Provider>,
    /// Conversation history (system prompt is injected per-turn from the harness).
    session: Vec<Message>,
    next_turn: u64,
    next_approval: u64,
    /// Tools approved for the whole session via ApproveForSession.
    session_approved: HashSet<String>,
    /// Root all tool filesystem/shell access is confined to.
    workspace: PathBuf,
    /// Append-only session log (None if persistence is off/unavailable).
    session_store: Option<SessionStore>,
    /// Undo log for file-mutating tool calls.
    checkpoints: CheckpointStore,
    /// Connected MCP servers (one per configured launcher).
    mcp_clients: Vec<McpClient>,
    /// Namespaced tool specs discovered from all MCP servers.
    mcp_tools: Vec<ToolSpec>,
    /// Lazily launched browser-automation session.
    browser: Option<browser::BrowserSession>,
    /// Model context window (tokens), reported by the provider; drives the
    /// compaction budget at 75% (opencode-style).
    ctx_window: Option<u64>,
    /// Files the model has read this session — `edit` requires a prior read.
    read_files: std::collections::HashSet<String>,
    event_tx: mpsc::Sender<Event>,
}

impl Engine {
    async fn emit(&self, ev: Event) {
        let _ = self.event_tx.send(ev).await;
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
        let mut tools = self.active_harness().tools();
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
        tools.push(ToolSpec::new("browser_navigate", "Open a URL in the automation browser; returns the page title and visible text.")
            .mutating(true)
            .params(serde_json::json!({"type":"object","properties":{"url":{"type":"string"}},"required":["url"]})));
        tools.push(ToolSpec::new("browser_read", "Read the current page's visible text (innerText).")
            .params(serde_json::json!({"type":"object","properties":{}})));
        tools.push(ToolSpec::new("browser_click", "Click the first element matching a CSS selector.")
            .mutating(true)
            .params(serde_json::json!({"type":"object","properties":{"selector":{"type":"string"}},"required":["selector"]})));
        tools.push(ToolSpec::new("browser_type", "Type text into the element matching a CSS selector.")
            .mutating(true)
            .params(serde_json::json!({"type":"object","properties":{"selector":{"type":"string"},"text":{"type":"string"}},"required":["selector","text"]})));
        tools.push(ToolSpec::new("browser_screenshot", "Capture a PNG screenshot of the current page to .oxide/screenshots.")
            .mutating(true)
            .params(serde_json::json!({"type":"object","properties":{}})));
        tools.push(ToolSpec::new("browser_eval", "Run JavaScript in the page and return the JSON result.")
            .mutating(true)
            .params(serde_json::json!({"type":"object","properties":{"script":{"type":"string"}},"required":["script"]})));
        tools.push(ToolSpec::new("ask_user", "Ask the user a question, optionally offering up to 4 short options to pick from. Use only when you genuinely need a decision before continuing.")
            .params(serde_json::json!({"type":"object","properties":{"question":{"type":"string"},"options":{"type":"array","items":{"type":"string"}}},"required":["question"]})));
        tools.push(ToolSpec::new("web_search", "Search the web (DuckDuckGo). Returns ranked results with title, URL, and snippet.")
            .params(serde_json::json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"]})));
        tools.push(ToolSpec::new("fetch_url", "Fetch a web page and return its readable text content (HTML stripped).")
            .params(serde_json::json!({"type":"object","properties":{"url":{"type":"string"}},"required":["url"]})));
        tools.push(ToolSpec::new("codebase_search", "Find code relevant to a natural-language query (ranked retrieval across the workspace). Use to locate where something is implemented.")
            .params(serde_json::json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"]})));
        tools
    }

    /// Ensure the browser session is launched; returns a ref or an error string.
    async fn ensure_browser(&mut self) -> Result<&browser::BrowserSession, String> {
        if self.browser.is_none() {
            match browser::BrowserSession::launch(self.config.browser_headless).await {
                Ok(s) => self.browser = Some(s),
                Err(e) => return Err(format!("browser launch failed: {e} (is a Chromium-based browser installed?)")),
            }
        }
        Ok(self.browser.as_ref().unwrap())
    }

    /// Handle a `browser_*` tool. Returns Some((output, ok)) if it was one.
    async fn handle_browser_tool(&mut self, name: &str, args: &serde_json::Value) -> Option<(String, bool)> {
        if !matches!(
            name,
            "browser_navigate" | "browser_read" | "browser_click" | "browser_type" | "browser_screenshot" | "browser_eval"
        ) {
            return None;
        }
        let shots_dir = self.workspace.join(".oxide/screenshots");
        let sess = match self.ensure_browser().await {
            Ok(s) => s,
            Err(e) => return Some((e, false)),
        };
        let sa = |k: &str| args[k].as_str().unwrap_or("").to_string();
        let res = match name {
            "browser_navigate" => sess.navigate(&sa("url")).await,
            "browser_read" => sess.read_text().await,
            "browser_click" => sess.click(&sa("selector")).await,
            "browser_type" => sess.type_text(&sa("selector"), &sa("text")).await,
            "browser_screenshot" => sess.screenshot(&shots_dir).await,
            "browser_eval" => sess.eval(&sa("script")).await,
            _ => return Some((format!("unknown browser tool {name}"), false)),
        };
        Some(match res {
            Ok(out) => (out, true),
            Err(e) => (format!("browser error: {e}"), false),
        })
    }

    /// Launch each configured MCP server and merge its tools. Failures are
    /// reported but never fatal — a missing server just means fewer tools.
    async fn connect_mcp_servers(&mut self) {
        let mut servers = self.config.mcp_servers.clone();
        // Auto-import MCP servers configured in Codex / Claude desktop so they
        // are available in Oxide without re-declaring them.
        for ext in discover_external_mcp() {
            if !servers.iter().any(|s| s.name == ext.name) {
                servers.push(ext);
            }
        }
        for srv in servers {
            if !srv.enabled {
                continue;
            }
            let connect = if !srv.url.is_empty() {
                McpClient::connect_http(&srv.name, &srv.url).await
            } else {
                McpClient::connect_stdio(&srv.name, &srv.command, &srv.args).await
            };
            match connect {
                Ok(client) => match client.list_tools().await {
                    Ok(tools) => {
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
                        self.mcp_clients.push(client);
                    }
                    Err(e) => {
                        self.emit(Event::McpServerStatus {
                            name: srv.name.clone(),
                            status: "error".to_string(),
                            tool_count: 0,
                            tools: Vec::new(),
                            detail: format!("tools/list failed: {e}"),
                        })
                        .await;
                        self.emit(Event::Error {
                            message: format!("mcp '{}' tools/list failed: {e}", srv.name),
                        })
                        .await;
                    }
                },
                Err(e) => {
                    self.emit(Event::McpServerStatus {
                        name: srv.name.clone(),
                        status: "error".to_string(),
                        tool_count: 0,
                        tools: Vec::new(),
                        detail: format!("connect failed: {e}"),
                    })
                    .await;
                    self.emit(Event::Error {
                        message: format!("mcp '{}' connect failed: {e}", srv.name),
                    })
                    .await;
                }
            }
        }
    }

    /// Fire lifecycle hooks for `event`. Returns true if a `pre_tool` hook
    /// blocked (non-zero exit). Payload JSON is passed via `$OXIDE_HOOK_PAYLOAD`.
    async fn fire_hooks(&self, event: &str, payload: serde_json::Value) -> bool {
        let hooks = hooks::Hooks::load(&self.workspace);
        let mut blocked = false;
        for cmd in hooks.commands(event) {
            let status = tokio::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(&self.workspace)
                .env("OXIDE_HOOK_EVENT", event)
                .env("OXIDE_HOOK_PAYLOAD", payload.to_string())
                .output()
                .await;
            let ok = status.map(|o| o.status.success()).unwrap_or(false);
            let this_blocked = event == "pre_tool" && !ok;
            if this_blocked {
                blocked = true;
            }
            self.emit(Event::HookFired {
                hook: event.to_string(),
                command: cmd.clone(),
                blocked: this_blocked,
            })
            .await;
        }
        blocked
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
            let resumed = if self.session.is_empty() {
                String::new()
            } else {
                format!(" (resumed {} msgs)", self.session.len())
            };
            self.emit(Event::Info {
                text: format!("session {}{}", store.id, resumed),
            })
            .await;
        }
        self.connect_mcp_servers().await;

        while let Some(op) = op_rx.recv().await {
            match op {
                Op::UserTurn { text } => self.run_turn(text, &mut op_rx).await,
                Op::SetHarness { id } => self.set_harness(id).await,
                Op::Interrupt => {
                    // No turn in flight here; nothing to interrupt.
                    self.emit(Event::Info {
                        text: "nothing to interrupt".into(),
                    })
                    .await;
                }
                Op::ApprovalResponse { .. } => { /* handled inline during a turn */ }
                Op::QuestionAnswer { .. } => { /* handled inline during a turn */ }
                Op::Rewind { checkpoint_id } => {
                    let restored = self.checkpoints.rewind(checkpoint_id);
                    self.emit(Event::RewindDone {
                        id: checkpoint_id,
                        restored,
                    })
                    .await;
                }
                Op::Shutdown => break,
            }
        }
        self.emit(Event::Shutdown).await;
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
            self.emit(Event::Compacted { dropped, tokens: context::estimate_tokens(&self.session) }).await;
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
            self.emit(Event::Compacted { dropped: 0, tokens: context::estimate_tokens(&self.session) }).await;
            return;
        }
        const KEEP_RECENT: usize = 8;
        if self.session.len() <= KEEP_RECENT + 1 {
            // Too short to summarize usefully — fall back to a hard trim.
            let dropped = context::compact(&mut self.session, budget, KEEP_RECENT);
            if dropped > 0 {
                self.emit(Event::Compacted { dropped, tokens: context::estimate_tokens(&self.session) }).await;
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
        let provider = self.config.provider.clone();
        let effort = self.config.reasoning_effort.clone();
        let sys = "You compress conversation history. Summarize the earlier conversation below into a concise but COMPLETE brief that lets the assistant continue seamlessly. Preserve: the user's goal/task, decisions made, files created/edited (with paths), commands run and key results, current state, and open TODOs. Terse bullet points. Output only the summary.";
        let summary = self.stream_collect(&provider, sys, &blob, &effort, turn, false, true).await;
        let summary = if summary.trim().is_empty() {
            format!("(summary unavailable; {} earlier messages folded)", old.len())
        } else {
            summary
        };
        self.session.insert(0, Message {
            role: Role::Assistant,
            content: format!("## Summary of earlier conversation\n{summary}"),
        });
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
    /// orchestration pipeline (front planner → backend implementer).
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
                Message { role: Role::System, content: system.to_string() },
                Message { role: Role::User, content: user.to_string() },
            ],
            tools: vec![],
        };
        let (tx, mut rx) = mpsc::channel::<StreamItem>(STREAM_QUEUE);
        let provider = oxide_providers::build(provider_id);
        let task = tokio::spawn(async move { provider.stream(req, tx).await });
        let mut out = String::new();
        while let Some(item) = rx.recv().await {
            match item {
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
                StreamItem::Notice(text) => {
                    self.emit(Event::Info { text }).await;
                }
                StreamItem::Usage { input, output, context_window } => {
                    self.emit(Event::TokensUsed { turn, input, output }).await;
                    if let Some(limit) = context_window {
                        self.emit(Event::ContextWindow { limit }).await;
                    }
                }
                StreamItem::RateLimit { plan, primary_pct, secondary_pct, primary_reset_s, secondary_reset_s } => {
                    self.emit(Event::RateLimit { plan, primary_pct, secondary_pct, primary_reset_s, secondary_reset_s }).await;
                }
                StreamItem::ToolCall { .. } => {}
                StreamItem::Done => break,
            }
        }
        task.abort();
        out
    }

    async fn run_turn(&mut self, user_text: String, op_rx: &mut mpsc::Receiver<Op>) {
        let turn = TurnId(self.next_turn);
        self.next_turn += 1;
        self.emit(Event::TurnStarted { turn }).await;

        // Expand `/slash` commands from .oxide/commands/*.md before running.
        let user_text = if user_text.trim_start().starts_with('/') {
            match commands::expand(&self.workspace, &user_text) {
                Some(expanded) => {
                    self.emit(Event::Info { text: format!("▷ ran command {}", user_text.trim()) }).await;
                    expanded
                }
                None => {
                    self.emit(Event::Info { text: format!("unknown command: {}", user_text.trim()) }).await;
                    user_text
                }
            }
        } else {
            user_text
        };

        self.session.push(Message {
            role: Role::User,
            content: user_text.clone(),
        });
        if let Some(store) = &self.session_store {
            let _ = store.append("user", &user_text);
        }

        // Keep the running history under budget — summarize, don't just drop.
        self.compact_session(turn).await;

        let tools = self.all_tools();
        let mem_block = memory::Memory::new(&self.workspace).load_block();
        let harness = self.active_harness();
        let policy = harness.loop_policy();
        let mut sys = harness.system_prompt();
        // Tell the agent exactly where it is working so it never wanders to $HOME.
        sys.push_str(&format!(
            "\n\n# Working directory\nYou are operating in this project: `{}`. \
             All shell commands run here (cwd) and relative paths resolve here. \
             Search, read, and edit only inside this directory unless the user explicitly asks otherwise — do NOT scan $HOME or the whole filesystem.",
            self.workspace.display()
        ));
        // Pinned project instructions (AGENTS.md / CLAUDE.md) — always resident,
        // never compacted away.
        if let Some(agents) = load_project_instructions(&self.workspace) {
            sys.push_str("\n\n# Project instructions (AGENTS.md)\n");
            sys.push_str(&agents);
        }
        sys.push_str(
            "\n\n# Persistent memory & self-improvement\n\
             You have durable memory at .oxide/memory. Use the `remember` tool to store \
             important facts and `save_skill` to capture reusable procedures you discover. \
             Consult what you already know below before acting.",
        );
        if !mem_block.is_empty() {
            sys.push_str("\n\n");
            sys.push_str(&mem_block);
        }
        let mut assistant = String::new();
        let mut interrupted = false;

        // ── Orchestration pipeline (front planner → backend implementer) ──
        if self.config.orchestrate {
            let front = self.config.front_provider.clone();
            let backend = self.config.backend_provider.clone();
            let effort = self.config.reasoning_effort.clone();
            self.emit(Event::Info { text: format!("🧭 Planning · front: {front}") }).await;
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
                // ── Fan out the plan's numbered steps to parallel sub-agents ──
                let subtasks: Vec<String> = plan
                    .lines()
                    .map(|l| l.trim())
                    .filter(|l| {
                        l.starts_with(|c: char| c.is_ascii_digit()) || l.starts_with('-') || l.starts_with('*')
                    })
                    .map(|l| l.trim_start_matches(|c: char| c.is_ascii_digit() || matches!(c, '.' | ')' | '-' | '*' | ' ')).to_string())
                    .filter(|l| !l.is_empty())
                    .take(6)
                    .collect();

                if subtasks.is_empty() {
                    // No clear steps — fall back to a single implementer.
                    let isys = format!("You are the implementer. Carry out this plan precisely.\n\nPLAN:\n{plan}");
                    assistant = self.stream_collect(&backend, &isys, &user_text, &effort, turn, false, false).await;
                } else {
                    self.emit(Event::Info {
                        text: format!("🤖 Spawning {} sub-agents · backend: {backend}", subtasks.len()),
                    })
                    .await;
                    let results = {
                        let this: &Self = &*self; // shared reborrow for concurrent sub-agents
                        let futures = subtasks.iter().enumerate().map(|(i, st)| {
                            let bsys = format!(
                                "You are sub-agent {}. Do EXACTLY this subtask and report what you did. Overall plan for context:\n{plan}",
                                i + 1
                            );
                            let st = st.clone();
                            let backend = backend.clone();
                            let effort = effort.clone();
                            async move {
                                let out = this.stream_collect(&backend, &bsys, &st, &effort, turn, false, true).await;
                                (i + 1, st, out)
                            }
                        });
                        futures::future::join_all(futures).await
                    };
                    for (i, st, _) in &results {
                        self.emit(Event::Info { text: format!("✓ sub-agent {i}: {}", st.chars().take(60).collect::<String>()) }).await;
                    }
                    // Synthesize sub-agent outputs into the final answer.
                    self.emit(Event::Info { text: format!("🧩 Synthesizing · front: {front}") }).await;
                    let joined: String = results
                        .iter()
                        .map(|(i, st, r)| format!("### Sub-agent {i} — {st}\n{r}"))
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    let ssys = format!(
                        "You are the lead. Combine the sub-agent results into one coherent final answer for the user. Resolve overlaps, note anything incomplete.\n\nSUB-AGENT RESULTS:\n{joined}"
                    );
                    assistant = self.stream_collect(&front, &ssys, &user_text, &effort, turn, false, false).await;
                }
            } else {
                self.emit(Event::Info { text: format!("⚙ Implementing · backend: {backend}") }).await;
                let isys = format!(
                    "You are the implementer. Carry out the following plan precisely to fulfil the user's request — do the actual work, edits and commands.\n\nPLAN:\n{plan}"
                );
                assistant = self.stream_collect(&backend, &isys, &user_text, &effort, turn, false, false).await;
            }

            // ── Review + auto-fix loop (review → if gaps, re-implement) ──
            let max_iters: u32 = 3;
            let mut iter: u32 = 0;
            loop {
                self.emit(Event::Info { text: format!("🔍 Reviewing · front: {front}") }).await;
                let vsys = format!(
                    "You are the reviewer. Verify whether the implementation fulfils the user's request. On the FIRST line reply with exactly `DONE` if it is fully complete and correct, otherwise reply `GAPS` and then list the specific remaining gaps. Be concise.\n\nPLAN:\n{plan}\n\nRESULT SO FAR:\n{assistant}"
                );
                // Review shows in the thinking box (orchestrator's verification).
                let review = self.stream_collect(&front, &vsys, &user_text, &effort, turn, true, false).await;
                let up = review.trim_start().to_ascii_uppercase();
                let has_gaps = up.starts_with("GAPS") || (up.contains("GAP") && !up.starts_with("DONE"));
                if !has_gaps {
                    self.emit(Event::Info { text: "✓ Review passed".to_string() }).await;
                    break;
                }
                iter += 1;
                if iter >= max_iters {
                    self.emit(Event::Info { text: format!("⚠ Gaps remain after {max_iters} fixes") }).await;
                    let note = format!("\n\n— ⚠ Remaining gaps —\n{}", review.trim());
                    self.emit(Event::AgentMessageDelta { turn, text: note.clone() }).await;
                    assistant.push_str(&note);
                    break;
                }
                self.emit(Event::Info { text: format!("🔁 Fixing gaps · iteration {iter} · backend: {backend}") }).await;
                let header = format!("\n\n— 🔁 Revision {iter} —\n");
                self.emit(Event::AgentMessageDelta { turn, text: header.clone() }).await;
                assistant.push_str(&header);
                let fsys = format!(
                    "You are the implementer. Fix the gaps the reviewer found — make the actual edits/commands. Do not redo what already works.\n\nPLAN:\n{plan}\n\nGAPS TO FIX:\n{review}\n\nWORK SO FAR:\n{assistant}"
                );
                let fix = self.stream_collect(&backend, &fsys, &user_text, &effort, turn, false, false).await;
                assistant.push_str(&fix);
            }

            if !assistant.is_empty() {
                if let Some(store) = &self.session_store {
                    let _ = store.append("assistant", &assistant);
                }
                self.session.push(Message { role: Role::Assistant, content: assistant });
            }
            self.fire_hooks("stop", serde_json::json!({})).await;
            self.emit(Event::TurnFinished { turn }).await;
            return;
        }

        // ── Agentic loop: stream → run tool calls → re-request with results,
        //    until the model answers with no tool calls (or step budget runs out). ──
        let _ = &mut assistant; // (assistant is used by the orchestrate path above)
        let model = policy
            .model
            .clone()
            .unwrap_or_else(|| self.config.effective_model());
        let max_steps = (policy.max_steps as usize).clamp(1, 60);
        let mut step = 0usize;
        let mut overflow_retries = 0u8;
        let mut nudges = 0u8;
        loop {
            // Keep the running history under budget on EVERY request — long
            // agentic turns accumulate tool output and would otherwise overflow.
            self.compact_session(turn).await;
            let mut msgs = vec![Message { role: Role::System, content: sys.clone() }];
            msgs.extend(self.session.iter().cloned());
            let req = TurnRequest {
                model: model.clone(),
                reasoning_effort: self.config.reasoning_effort.clone(),
                temperature: policy.temperature,
                messages: msgs,
                tools: tools.clone(),
            };

            let (stream_tx, mut stream_rx) = mpsc::channel::<StreamItem>(STREAM_QUEUE);
            let provider = oxide_providers::build(&self.config.provider);
            let stream_task = tokio::spawn(async move { provider.stream(req, stream_tx).await });

            let mut round_text = String::new();
            let mut did_tool = false;
            let mut steered = false;
            loop {
                tokio::select! {
                    item = stream_rx.recv() => {
                        match item {
                            Some(StreamItem::TextDelta(t)) => {
                                round_text.push_str(&t);
                                self.emit(Event::AgentMessageDelta { turn, text: t }).await;
                            }
                            Some(StreamItem::ReasoningDelta(t)) => {
                                self.emit(Event::ReasoningDelta { turn, text: t }).await;
                            }
                            Some(StreamItem::ToolCall { name, arguments }) => {
                                did_tool = true;
                                if self.handle_tool_call(turn, name, arguments, op_rx).await {
                                    interrupted = true;
                                    break;
                                }
                            }
                            Some(StreamItem::Notice(text)) => {
                                self.emit(Event::Info { text }).await;
                            }
                            Some(StreamItem::Usage { input, output, context_window }) => {
                                self.emit(Event::TokensUsed { turn, input, output }).await;
                                if let Some(limit) = context_window {
                                    self.ctx_window = Some(limit);
                                    self.emit(Event::ContextWindow { limit }).await;
                                }
                            }
                            Some(StreamItem::RateLimit { plan, primary_pct, secondary_pct, primary_reset_s, secondary_reset_s }) => {
                                self.emit(Event::RateLimit { plan, primary_pct, secondary_pct, primary_reset_s, secondary_reset_s }).await;
                            }
                            Some(StreamItem::Done) | None => break,
                        }
                    }
                    op = op_rx.recv() => {
                        match op {
                            Some(Op::Interrupt) => { interrupted = true; break; }
                            Some(Op::Shutdown) => { interrupted = true; break; }
                            // Steering: a message sent mid-turn is injected into the
                            // conversation; the next agentic round picks it up.
                            Some(Op::UserTurn { text }) => {
                                if let Some(store) = &self.session_store {
                                    let _ = store.append("user", &text);
                                }
                                self.session.push(Message { role: Role::User, content: text.clone() });
                                self.emit(Event::Info { text: format!("↪ steering: {text}") }).await;
                                steered = true;
                            }
                            Some(other) => {
                                self.emit(Event::Info { text: format!("queued op ignored mid-turn: {other:?}") }).await;
                            }
                            None => break,
                        }
                    }
                }
            }
            // Surface a provider error; on context-overflow, hard-compact + retry.
            let stream_err = if interrupted {
                stream_task.abort();
                None
            } else {
                stream_task.await.ok().and_then(|r| r.err()).map(|e| e.to_string())
            };
            if let Some(err) = &stream_err {
                let low = err.to_lowercase();
                let overflow = low.contains("context") || low.contains("exceeds") || low.contains("too long")
                    || low.contains("maximum") || (low.contains("token") && low.contains("limit"));
                if overflow && round_text.is_empty() && overflow_retries < 3 {
                    overflow_retries += 1;
                    self.force_compact(turn).await;
                    self.emit(Event::Info { text: "⚠ context full — compacted, retrying".into() }).await;
                    continue;
                }
                if round_text.is_empty() {
                    self.emit(Event::Error { message: err.clone() }).await;
                }
            }
            if !round_text.is_empty() {
                if let Some(store) = &self.session_store {
                    let _ = store.append("assistant", &round_text);
                }
                self.session.push(Message { role: Role::Assistant, content: round_text });
            }
            step += 1;
            if interrupted {
                break;
            }
            if step >= max_steps {
                // Force a text-only wrap-up instead of silently stopping (opencode-style).
                self.session.push(Message {
                    role: Role::User,
                    content: "<system-reminder>\nMaximum tool steps reached. Do NOT call any more tools. \
Reply with text only: summarize what you changed (with file paths and how you verified), and list \
any remaining tasks and the recommended next step.\n</system-reminder>".into(),
                });
                break;
            }
            if !did_tool && !steered {
                // The model produced prose but took no action. If it likely owes
                // an edit, nudge it once to actually do the work before ending.
                if nudges < 1 {
                    nudges += 1;
                    self.session.push(Message {
                        role: Role::User,
                        content: "<system-reminder>\nYou stopped without calling a tool. If the task requires \
changes, APPLY them now with the edit/write_file tools (then verify with shell). Do not just describe \
an edit — make it. Only end without acting if the task is genuinely complete and verified, or you are \
truly blocked and need a decision (then call ask_user).\n</system-reminder>".into(),
                    });
                    continue;
                }
                break;
            }
        }
        if interrupted {
            self.emit(Event::Info { text: "turn interrupted".into() }).await;
        }
        self.fire_hooks("stop", serde_json::json!({})).await;
        self.emit(Event::TurnFinished { turn }).await;
    }

    /// Route one tool call through approval + sandbox and emit its result.
    /// Returns `true` if the turn was interrupted/shut down while waiting.
    /// Validate an `edit` and compute the resulting full file content.
    fn compute_edit(&self, args: &serde_json::Value) -> Result<(String, String), String> {
        let path = args["path"].as_str().ok_or("edit: missing 'path'")?.to_string();
        let old = args["old_string"].as_str().ok_or("edit: missing 'old_string'")?;
        let new = args["new_string"].as_str().unwrap_or("");
        let replace_all = args["replace_all"].as_bool().unwrap_or(false);
        if old.is_empty() {
            return Err("edit: 'old_string' is empty — use write_file to create a whole file.".into());
        }
        let abs = self.workspace.join(&path);
        if abs.exists() && !self.read_files.contains(&path) {
            return Err(format!("edit: you must read_file '{path}' before editing it."));
        }
        let content = std::fs::read_to_string(&abs).unwrap_or_default();
        let count = content.matches(old).count();
        if count == 0 {
            return Err(format!("edit: old_string not found in '{path}'. Read the file and copy the exact text (with whitespace)."));
        }
        if count > 1 && !replace_all {
            return Err(format!("edit: old_string appears {count} times in '{path}' — add surrounding lines to make it unique, or set replace_all=true."));
        }
        let new_content = if replace_all { content.replace(old, new) } else { content.replacen(old, new, 1) };
        Ok((path, new_content))
    }

    async fn handle_tool_call(
        &mut self,
        turn: TurnId,
        name: String,
        mut arguments: serde_json::Value,
        op_rx: &mut mpsc::Receiver<Op>,
    ) -> bool {
        self.emit(Event::ToolCallBegin {
            turn,
            tool: name.clone(),
            args: arguments.clone(),
        })
        .await;

        // ask_user: surface a question (with optional choices) and block for the answer.
        if name == "ask_user" {
            let request_id = self.next_approval;
            self.next_approval += 1;
            let question = arguments["question"].as_str().unwrap_or("").to_string();
            let options = arguments["options"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>())
                .unwrap_or_default();
            self.emit(Event::QuestionAsked { request_id, question, options }).await;
            let answer = loop {
                match op_rx.recv().await {
                    Some(Op::QuestionAnswer { request_id: rid, answer }) if rid == request_id => break answer,
                    Some(Op::Interrupt) | Some(Op::Shutdown) | None => return true,
                    Some(_) => {}
                }
            };
            self.session.push(Message { role: Role::Tool, content: format!("[ask_user answer] {answer}") });
            self.emit(Event::ToolCallEnd { turn, tool: name, output: answer, ok: true }).await;
            return false;
        }

        let mut router = ToolRouter::new(
            self.config.approval_policy,
            self.config.sandbox,
            self.workspace.clone(),
            &self.all_tools(),
        );
        for t in &self.session_approved {
            router.approve_for_session(t);
        }

        // Gate on policy; request approval if needed.
        match router.route(&name) {
            Routed::Denied(reason) => {
                self.emit(Event::ToolCallEnd {
                    turn,
                    tool: name,
                    output: format!("denied: {reason}"),
                    ok: false,
                })
                .await;
                return false;
            }
            Routed::Run => {}
            Routed::NeedsApproval => {
                let request_id = self.next_approval;
                self.next_approval += 1;
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
                                self.emit(Event::ToolCallEnd {
                                    turn,
                                    tool: name,
                                    output: "rejected by user".into(),
                                    ok: false,
                                })
                                .await;
                                return false;
                            }
                            ApprovalDecision::ApproveForSession => {
                                self.session_approved.insert(name.clone());
                                break;
                            }
                            ApprovalDecision::Approve => break,
                        },
                        Some(Op::Interrupt) | Some(Op::Shutdown) | None => return true,
                        Some(_) => {} // ignore unrelated ops while awaiting approval
                    }
                }
            }
        }

        // pre_tool hook — may block.
        if self.fire_hooks("pre_tool", serde_json::json!({ "tool": name.clone(), "args": arguments.clone() })).await {
            self.emit(Event::ToolCallEnd {
                turn,
                tool: name,
                output: "blocked by pre_tool hook".into(),
                ok: false,
            })
            .await;
            return false;
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
                let id = self.checkpoints.snapshot(&abs);
                self.emit(Event::CheckpointCreated {
                    turn,
                    id,
                    label: format!("write {path}"),
                })
                .await;
                write_ctx = Some((path.to_string(), prior, id));
            }
        }

        // Browser automation, then memory tools, then MCP, then native sandbox.
        let (output, ok) = if let Some(r) = self.handle_browser_tool(&name, &arguments).await {
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
        } else if name == "web_search" {
            web_search(arguments["query"].as_str().unwrap_or("")).await
        } else if name == "fetch_url" {
            fetch_url(arguments["url"].as_str().unwrap_or("")).await
        } else if name == "codebase_search" {
            codebase_search(&self.workspace, arguments["query"].as_str().unwrap_or(""))
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
            if let Some((path, prior, id)) = &write_ctx {
                self.emit(Event::PatchApplied { turn, path: path.clone() }).await;
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
            serde_json::json!({ "tool": name.clone(), "ok": ok, "output": output.clone() }),
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
        self.session.push(Message {
            role: Role::Tool,
            content: format!("[tool {name}]\n{stored}"),
        });
        self.emit(Event::ToolCallEnd {
            turn,
            tool: name,
            output,
            ok,
        })
        .await;
        false
    }
}

/// Load pinned project instructions from AGENTS.md / CLAUDE.md (first found).
fn load_project_instructions(workspace: &std::path::Path) -> Option<String> {
    // Single-file instructions (first found): AGENTS.md / CLAUDE.md / Cursor rules.
    for name in ["AGENTS.md", "CLAUDE.md", ".oxide/AGENTS.md", ".cursorrules"] {
        if let Ok(text) = std::fs::read_to_string(workspace.join(name)) {
            let t = text.trim();
            if !t.is_empty() {
                let capped: String = t.chars().take(8000).collect();
                return Some(capped);
            }
        }
    }
    // Cursor's `.cursor/rules/*.mdc` rule files (concatenated).
    if let Ok(rd) = std::fs::read_dir(workspace.join(".cursor/rules")) {
        let mut combined = String::new();
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("mdc") {
                if let Ok(text) = std::fs::read_to_string(&p) {
                    combined.push_str(text.trim());
                    combined.push_str("\n\n");
                }
            }
        }
        let t = combined.trim();
        if !t.is_empty() {
            return Some(t.chars().take(8000).collect());
        }
    }
    None
}

/// Unified diff between two file contents.
fn unified_diff(old: &str, new: &str, path: &str) -> String {
    similar::TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string()
}

fn tool_arg_string(args: &serde_json::Value, key: &str) -> String {
    args[key].as_str().unwrap_or("").trim().to_string()
}
