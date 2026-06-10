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
mod embed;
mod index;
mod hooks;
mod memory;
mod sandbox;
mod store;
mod tools;
pub use tools::{Routed, ToolRouter};

use oxide_config::Config;
use oxide_config::McpServerConfig;

/// A shallow file-tree of the workspace, injected into the system prompt so the
/// agent sees the project's real structure from the first message (and doesn't
/// "forget the codebase" in a fresh tab or invent a standalone solution).
fn project_map(ws: &Path) -> String {
    const SKIP: &[&str] = &[
        ".git", "node_modules", "target", "dist", ".next", ".oxide", "vendor",
        "build", ".venv", "__pycache__", ".cache", "out", ".turbo", ".idea", ".vscode",
    ];
    fn walk(dir: &Path, prefix: &str, depth: usize, count: &mut usize, out: &mut String) {
        if depth > 2 || *count > 120 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
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
        let fw = if p.contains("\"next\"") { "Next.js (React)" }
            else if p.contains("nuxt") { "Nuxt (Vue)" }
            else if p.contains("@remix-run") { "Remix (React)" }
            else if p.contains("svelte") { "Svelte/SvelteKit" }
            else if p.contains("\"vue\"") { "Vue" }
            else if p.contains("\"react\"") { "React" }
            else if p.contains("\"vite\"") { "Vite" }
            else if p.contains("\"express\"") { "Node/Express" }
            else { "Node.js" };
        let extra = if p.contains("supabase") { " + Supabase" } else { "" };
        return Some(format!("{fw}{extra}"));
    }
    if ws.join("Cargo.toml").exists() { return Some("Rust (Cargo)".into()); }
    if ws.join("go.mod").exists() { return Some("Go".into()); }
    if ws.join("pyproject.toml").exists() || ws.join("requirements.txt").exists() { return Some("Python".into()); }
    if ws.join("pom.xml").exists() || ws.join("build.gradle").exists() { return Some("Java/JVM".into()); }
    if ws.join("Gemfile").exists() { return Some("Ruby".into()); }
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
    // Cache for 60s — engines respawn on every tab switch and ~/.claude.json
    // can be megabytes; no need to re-read + re-parse it each time.
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Option<(std::time::Instant, Vec<McpServerConfig>)>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Ok(g) = cache.lock() {
        if let Some((t, v)) = g.as_ref() {
            if t.elapsed() < std::time::Duration::from_secs(60) {
                return v.clone();
            }
        }
    }
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
    if let Ok(mut g) = cache.lock() {
        *g = Some((std::time::Instant::now(), out.clone()));
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
    // An explicit session file (tab/history) wins over generic "resume latest".
    if let Some(p) = config.resume_path.clone() {
        if let Ok(msgs) = SessionStore::load(&p) {
            history = msgs
                .into_iter()
                .filter(|m| m.role != "meta")
                .map(|m| Message::new(role_from_str(&m.role), m.content))
                .collect();
            tracing::info!(count = history.len(), "resumed session from {}", p.display());
        }
    } else if config.resume {
        if let Some(prev) = SessionStore::latest(&workspace) {
            if let Ok(msgs) = SessionStore::load(&prev) {
                history = msgs
                    .into_iter()
                    .filter(|m| m.role != "meta")
                    .map(|m| Message::new(role_from_str(&m.role), m.content))
                    .collect();
                tracing::info!(count = history.len(), "resumed prior session");
            }
        }
    }

    let session_store = if config.persist {
        // Resuming an existing session continues ITS file — no new file, no
        // duplicate meta line, one conversation in one transcript.
        let attached = config.resume_path.as_deref().and_then(|p| SessionStore::attach(p).ok());
        match attached.map(Ok).unwrap_or_else(|| SessionStore::open(&workspace)) {
            Ok(s) => {
                if config.resume_path.is_none() {
                    // Record which provider/model owns this session (sidebar logos).
                    let _ = s.append("meta", &format!("provider={}", config.provider));
                }
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
        turn_edited: false,
        turn_edit_paths: Vec::new(),
        turn_reads: std::collections::HashSet::new(),
        last_tool_sig: String::new(),
        last_tool_reps: 0,
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
    mcp_clients: Vec<std::sync::Arc<McpClient>>,
    /// Namespaced tool specs discovered from all MCP servers.
    mcp_tools: Vec<ToolSpec>,
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
    /// break the "read README → re-plan → read README again" loop.
    turn_reads: std::collections::HashSet<String>,
    /// Doom-loop guard: last tool call signature + consecutive repeat count.
    last_tool_sig: String,
    last_tool_reps: u8,
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
        tools.push(ToolSpec::new("codebase_search", "Find code relevant to a natural-language query — fast indexed retrieval (TF-IDF + symbol-aware) across the workspace. Use this FIRST to locate where something is implemented when you don't know the file; prefer it over a broad `search`. Then read only the top file(s).")
            .params(serde_json::json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"]})));
        tools.push(ToolSpec::new("todo_write", "Maintain a short task checklist for non-trivial multi-step work (>2 edits or multiple files/subsystems). Skip it for simple tasks. Call with the FULL list each time; keep exactly one task 'in_progress' and mark tasks 'completed' as you finish.")
            .params(serde_json::json!({
                "type":"object",
                "properties":{"todos":{"type":"array","items":{"type":"object","properties":{
                    "content":{"type":"string"},
                    "status":{"type":"string","enum":["pending","in_progress","completed"]}
                },"required":["content","status"]}}},
                "required":["todos"]
            })));
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
        // Process-wide pool of live MCP connections, keyed by server config.
        // Tab switches respawn the engine; without this every switch paid the
        // full reconnect (npx cold start) for every server.
        type Pool = std::collections::HashMap<String, (std::sync::Arc<McpClient>, Vec<oxide_protocol::ToolSpec>)>;
        static MCP_POOL: std::sync::OnceLock<tokio::sync::Mutex<Pool>> = std::sync::OnceLock::new();
        let pool = MCP_POOL.get_or_init(Default::default);

        let mut servers = self.config.mcp_servers.clone();
        // Auto-import MCP servers configured in Codex / Claude desktop so they
        // are available in Oxide without re-declaring them.
        for ext in discover_external_mcp() {
            if !servers.iter().any(|s| s.name == ext.name) {
                servers.push(ext);
            }
        }
        // Connect all servers CONCURRENTLY with a hard per-server deadline —
        // sequential 60s connects to stale npx servers used to make the engine
        // ignore the first user message for minutes.
        let conn_futs = servers
            .iter()
            .filter(|s| s.enabled)
            .map(|srv| {
                let srv = srv.clone();
                async move {
                    let key = format!("{}|{}|{}|{}", srv.name, srv.command, srv.args.join(" "), srv.url);
                    // Reuse a live pooled connection when it still answers.
                    if let Some((client, tools)) = pool.lock().await.get(&key).cloned() {
                        let alive = tokio::time::timeout(
                            std::time::Duration::from_secs(5),
                            client.list_tools(),
                        )
                        .await
                        .map(|r| r.is_ok())
                        .unwrap_or(false);
                        if alive {
                            return (srv, Ok((client, tools)));
                        }
                        pool.lock().await.remove(&key);
                    }
                    let fut = async {
                        let client = if !srv.url.is_empty() {
                            McpClient::connect_http(&srv.name, &srv.url).await?
                        } else {
                            McpClient::connect_stdio(&srv.name, &srv.command, &srv.args).await?
                        };
                        let tools = client.list_tools().await?;
                        Ok::<_, anyhow::Error>((std::sync::Arc::new(client), tools))
                    };
                    let res = match tokio::time::timeout(std::time::Duration::from_secs(15), fut).await {
                        Ok(r) => r,
                        Err(_) => Err(anyhow::anyhow!("timed out after 15s")),
                    };
                    if let Ok((client, tools)) = &res {
                        pool.lock().await.insert(key, (client.clone(), tools.clone()));
                    }
                    (srv, res)
                }
            })
            .collect::<Vec<_>>();
        for (srv, connect) in futures::future::join_all(conn_futs).await {
            match connect.map(|(c, t)| { self.mcp_clients.push(c); t }) {
                Ok(tools) => {
                    {
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
                    }
                }
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
            let fut = tokio::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(&self.workspace)
                .env("OXIDE_HOOK_EVENT", event)
                .env("OXIDE_HOOK_PAYLOAD", payload.to_string())
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output();
            // A hook must never wedge the agent — bound it (60s, then killed).
            let status = tokio::time::timeout(std::time::Duration::from_secs(60), fut).await;
            let ok = matches!(&status, Ok(Ok(o)) if o.status.success());
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
        self.emit(Event::Info { text: format!("compacting context ({} earlier messages)…", old.len()) }).await;
        let provider = self.config.provider.clone();
        let effort = self.config.reasoning_effort.clone();
        let sys = "You compress conversation history. Summarize the earlier conversation below into a concise but COMPLETE brief that lets the assistant continue seamlessly. Preserve: the user's goal/task, decisions made, files created/edited (with paths), commands run and key results, current state, and open TODOs. Terse bullet points. Output only the summary.";
        let summary = self.stream_collect(&provider, sys, &blob, &effort, turn, false, true).await;
        let summary = if summary.trim().is_empty() {
            format!("(summary unavailable; {} earlier messages folded)", old.len())
        } else {
            summary
        };
        self.session.insert(0, Message::new(Role::Assistant, format!("## Summary of earlier conversation\n{summary}")));
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
                Message::new(Role::System, system.to_string()),
                Message::new(Role::User, user.to_string()),
            ],
            tools: vec![],
            cwd: self.workspace.display().to_string(),
        };
        let (tx, mut rx) = mpsc::channel::<StreamItem>(STREAM_QUEUE);
        let provider = oxide_providers::build(provider_id);
        let task = tokio::spawn(async move { provider.stream(req, tx).await });
        let mut out = String::new();
        // Idle-timeout like run_turn — a stalled provider must not wedge
        // compaction/orchestration forever (this path can't be interrupted).
        while let Some(item) = match tokio::time::timeout(std::time::Duration::from_secs(180), rx.recv()).await {
            Ok(it) => it,
            Err(_) => { task.abort(); None }
        } {
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
                StreamItem::ReasoningItem(_) => {}
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

        self.session.push(Message::new(Role::User, user_text.clone()));
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
        let stack = detect_stack(&self.workspace);
        sys.push_str(&format!(
            "\n\n# Working directory\nYou are operating in this EXISTING project: `{}`{}. \
             All shell commands run here (cwd) and relative paths resolve here.\n\
             - Build INSIDE this project, using its existing stack and conventions. Identify the stack first (read package.json / Cargo.toml / the framework config) and add code where it belongs — e.g. for Next.js create pages/components/route handlers in the right folders. NEVER hand-write a standalone `index.html` (or a generic from-scratch solution) when the project is a framework app — use the framework.\n\
             - Create new files ONLY inside this directory. Do NOT write outside it or invent a new sibling folder (e.g. `../something-new`, `/Volumes/...`). Even when asked for a 'separate' or 'standalone' page, put it inside this project unless the user gives an explicit absolute path elsewhere.\n\
             - Search, read, and edit inside this directory; do NOT scan $HOME or the whole filesystem.",
            self.workspace.display(),
            stack.map(|s| format!(" (detected stack: {s})")).unwrap_or_default()
        ));
        // Inject a shallow file-tree so the agent knows the real layout up front.
        let map = project_map(&self.workspace);
        if !map.trim().is_empty() {
            sys.push_str("\n\n# Project structure (shallow)\n```\n");
            sys.push_str(map.trim_end());
            sys.push_str("\n```\nWork within this existing structure; place new code where it belongs.");
        }
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
                self.session.push(Message::new(Role::Assistant, assistant));
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
        // CLI drivers (codex/claude) are self-agentic: they run their own tool
        // loop, so Oxide's nudge/wrap-up/auto-verify rounds would just respawn
        // the CLI with an out-of-context reminder as the whole prompt.
        let cli_driver = matches!(self.config.provider.as_str(), "codex" | "claude");
        let mut nudges = 0u8;
        let mut verifies = 0u8;
        let mut wrapped_up = false;
        self.turn_edited = false;
        self.turn_reads.clear();
        self.turn_edit_paths.clear();
        self.last_tool_sig.clear();
        self.last_tool_reps = 0;
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
            };

            let (stream_tx, mut stream_rx) = mpsc::channel::<StreamItem>(STREAM_QUEUE);
            let provider = oxide_providers::build(&self.config.provider);
            let stream_task = tokio::spawn(async move { provider.stream(req, stream_tx).await });

            let mut round_text = String::new();
            let mut pending_reasoning: Option<serde_json::Value> = None;
            let mut did_tool = false;
            let mut steered = false;
            loop {
                tokio::select! {
                    res = tokio::time::timeout(std::time::Duration::from_secs(180), stream_rx.recv()) => {
                        // Idle-timeout: if the provider stalls mid-stream (HTTP
                        // open but no data), don't hang the turn forever — end the
                        // round so the user isn't stuck on a spinner.
                        let item = match res {
                            Ok(it) => it,
                            Err(_) => {
                                self.emit(Event::Info { text: "provider stream stalled (no data for 180s) — ending round".into() }).await;
                                stream_task.abort();
                                break;
                            }
                        };
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
                            Some(StreamItem::ToolCall { id, name, arguments }) => {
                                did_tool = true;
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
                                if self.handle_tool_call(turn, name, arguments, id, op_rx).await {
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
                                self.session.push(Message::new(Role::User, text.clone()));
                                self.emit(Event::Info { text: format!("↪ steering: {text}") }).await;
                                steered = true;
                            }
                            // Rewind works mid-turn too — restoring a checkpoint
                            // is independent of the stream in flight.
                            Some(Op::Rewind { checkpoint_id }) => {
                                let restored = self.checkpoints.rewind(checkpoint_id);
                                self.emit(Event::RewindDone { id: checkpoint_id, restored }).await;
                            }
                            Some(other) => {
                                self.emit(Event::Info { text: format!("queued op ignored mid-turn: {other:?}") }).await;
                            }
                            // Op channel closed (handle dropped — e.g. a pane was
                            // closed): abort the live stream instead of waiting for
                            // the model to finish on its own.
                            None => { interrupted = true; break; }
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
                let mut msg = Message::new(Role::Assistant, round_text);
                msg.reasoning_item = pending_reasoning.take();
                self.session.push(msg);
            }
            step += 1;
            if interrupted {
                break;
            }
            if step >= max_steps {
                // Force a text-only wrap-up instead of silently stopping
                // (opencode-style). One extra round so the model actually SEES
                // the reminder — pushing it and breaking sent nothing.
                if !wrapped_up {
                    wrapped_up = true;
                    self.session.push(Message::new(Role::User,
                        "<system-reminder>\nMaximum tool steps reached. Do NOT call any more tools. \
Reply with text only: summarize what you changed (with file paths and how you verified), and list \
any remaining tasks and the recommended next step.\n</system-reminder>"));
                    continue;
                }
                break;
            }
            if cli_driver && !steered {
                // One CLI run per turn — it finished, we're done.
                break;
            }
            if !did_tool && !steered {
                // The model produced prose but took no action. If it likely owes
                // an edit, nudge it once to actually do the work before ending.
                if nudges < 1 {
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
                    if let Some(report) = self.run_verify().await {
                        verifies += 1;
                        self.turn_edited = false;
                        self.session.push(Message::new(Role::User, format!(
                            "<system-reminder>\nA build/typecheck failed after your edits. Fix the errors below, \
then stop. Apply fixes with edit/write_file — do not just explain.\n\n{report}\n</system-reminder>"
                        )));
                        continue;
                    }
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
    /// Run the project's build/typecheck after edits. Returns `Some(report)`
    /// when it fails (so the agent can auto-fix), `None` when it passes, can't
    /// be detected, errors out, or times out (never blocks the turn).
    async fn run_verify(&self) -> Option<String> {
        let ws = &self.workspace;
        // Only verify when a relevant source file was edited. A docs/config-only
        // edit (e.g. README.md) must NOT trigger a project-wide typecheck that
        // surfaces pre-existing errors in unrelated files and drags the agent
        // off-task. Extension of any edited path drives the language choice.
        let ext = |p: &str| std::path::Path::new(p).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        let edited: Vec<String> = self.turn_edit_paths.iter().map(|p| ext(p)).collect();
        let has = |exts: &[&str]| edited.iter().any(|e| exts.contains(&e.as_str()));
        let (prog, args): (String, Vec<String>) = if !self.config.verify_command.trim().is_empty() {
            ("sh".into(), vec!["-c".into(), self.config.verify_command.clone()])
        } else if ws.join("Cargo.toml").exists() && has(&["rs"]) {
            ("cargo".into(), vec!["check".into(), "--message-format".into(), "short".into()])
        } else if ws.join("tsconfig.json").exists() && has(&["ts", "tsx"]) {
            ("npx".into(), vec!["tsc".into(), "--noEmit".into()])
        } else if ws.join("package.json").exists() && has(&["ts", "tsx", "js", "jsx", "mjs", "cjs", "vue", "svelte"]) {
            ("npm".into(), vec!["run".into(), "build".into(), "--if-present".into()])
        } else if (ws.join("pyproject.toml").exists() || ws.join("requirements.txt").exists()) && has(&["py"]) {
            ("ruff".into(), vec!["check".into(), ".".into()])
        } else {
            return None;
        };
        self.emit(Event::Info { text: format!("auto-verify: {prog} {}", args.join(" ")) }).await;
        let fut = tokio::process::Command::new(&prog)
            .args(&args)
            .current_dir(ws)
            .output();
        let out = match tokio::time::timeout(std::time::Duration::from_secs(180), fut).await {
            Ok(Ok(o)) => o,
            _ => return None, // spawn error / timeout → don't block
        };
        if out.status.success() {
            return None;
        }
        let mut s = String::from_utf8_lossy(&out.stdout).to_string();
        s.push_str(&String::from_utf8_lossy(&out.stderr));
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        // Surface ONLY diagnostics that reference a file edited this turn — a
        // build failing on pre-existing errors elsewhere isn't this turn's job
        // (opencode does per-edited-file diagnostics, not project-wide chasing).
        let names: Vec<String> = self.turn_edit_paths.iter()
            .filter_map(|p| std::path::Path::new(p).file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        if !names.is_empty() && self.config.verify_command.trim().is_empty() {
            let relevant: String = s.lines()
                .filter(|l| names.iter().any(|n| l.contains(n.as_str())))
                .collect::<Vec<_>>()
                .join("\n");
            if relevant.trim().is_empty() {
                return None; // failures are all in files we didn't touch
            }
            let capped: String = relevant.chars().take(6000).collect();
            return Some(format!("$ {} {}\n{capped}", prog, args.join(" ")));
        }
        let capped: String = s.chars().take(6000).collect();
        Some(format!("$ {} {}\n{capped}", prog, args.join(" ")))
    }

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
        call_id: String,
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
                    // A typed message while a question is pending IS the answer —
                    // frontends without a dedicated answer UI (TUI, panes) would
                    // otherwise deadlock here.
                    Some(Op::UserTurn { text }) => break text,
                    Some(Op::Interrupt) | Some(Op::Shutdown) | None => {
                        self.session.push(Message::tool_result("interrupted before answering", call_id));
                        return true;
                    }
                    Some(_) => {}
                }
            };
            self.session.push(Message::tool_result(format!("[ask_user answer] {answer}"), call_id));
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
                // Always pair the recorded tool call with a result — a dangling
                // function_call poisons every later request on paired providers.
                self.session.push(Message::tool_result(format!("denied: {reason}"), call_id));
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
                                self.session.push(Message::tool_result("rejected by user", call_id));
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
                        Some(Op::Interrupt) | Some(Op::Shutdown) | None => {
                            self.session.push(Message::tool_result("interrupted before approval", call_id));
                            return true;
                        }
                        Some(_) => {} // ignore unrelated ops while awaiting approval
                    }
                }
            }
        }

        // pre_tool hook — may block.
        if self.fire_hooks("pre_tool", serde_json::json!({ "tool": name.clone(), "args": arguments.clone() })).await {
            self.session.push(Message::tool_result("blocked by pre_tool hook", call_id));
            self.emit(Event::ToolCallEnd {
                turn,
                tool: name,
                output: "blocked by pre_tool hook".into(),
                ok: false,
            })
            .await;
            return false;
        }

        // Doom-loop guard (opencode-style): the SAME tool with byte-identical
        // input 3× in a row is never progress — stop executing it and force a
        // change of approach instead of burning the turn.
        {
            let sig = format!("{name}:{arguments}");
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
                self.session.push(Message::tool_result(msg.clone(), call_id));
                self.emit(Event::ToolCallEnd { turn, tool: name, output: msg, ok: false }).await;
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
                    self.session.push(Message::tool_result(format!("[tool read_file]\n{msg}"), call_id));
                    self.emit(Event::ToolCallEnd { turn, tool: name, output: msg, ok: true }).await;
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
        } else if name == "todo_write" {
            let items: Vec<(String, String)> = arguments["todos"].as_array()
                .map(|a| a.iter().filter_map(|t| {
                    let c = t["content"].as_str()?.to_string();
                    let s = t["status"].as_str().unwrap_or("pending").to_string();
                    Some((c, s))
                }).collect())
                .unwrap_or_default();
            self.emit(Event::Todos { items: items.clone() }).await;
            let done = items.iter().filter(|(_, s)| s == "completed").count();
            (format!("todo list updated ({done}/{} done)", items.len()), true)
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
            self.turn_edited = true;
            if let Some(p) = arguments["path"].as_str() {
                self.turn_edit_paths.push(p.to_string());
            }
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
        self.session.push(Message::tool_result(format!("[tool {name}]\n{stored}"), call_id));
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
    let mut combined = String::new();
    // Single-file instructions — first match among the conventional names wins
    // (they're usually the same doc under different ecosystems' names).
    for name in ["AGENTS.md", "CLAUDE.md", ".oxide/AGENTS.md", ".cursorrules", ".windsurfrules"] {
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
            let mut paths: Vec<_> = rd.flatten().map(|e| e.path())
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
    if t.is_empty() { None } else { Some(t.chars().take(12000).collect()) }
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

#[cfg(test)]
mod map_test {
    #[test]
    fn map_shows_structure() {
        let ws = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
        let m = super::project_map(ws);
        assert!(m.contains("crates/") && m.contains("Cargo.toml"), "map:\n{m}");
        eprintln!("--- project map sample ---\n{}", &m[..m.len().min(400)]);
    }
}
