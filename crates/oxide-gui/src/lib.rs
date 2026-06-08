//! Desktop GUI for Oxide — Codex-desktop style, fully functional.
//!
//! Beyond the chat (driven by the shared [`oxide_core`] engine) this GUI ships
//! working: a right file panel that opens and **edits + saves** files, a
//! **terminal** that runs shell commands in the workspace, an **Open folder**
//! picker, and a **Settings** modal that changes provider/model/permissions/
//! workspace and live-reconfigures the engine (persisted to `oxide.toml`).

mod board;
mod update;
mod preview_proxy;

use dioxus::desktop::{Config as DesktopConfig, WindowBuilder};
use dioxus::prelude::*;
use futures::StreamExt;
use oxide_config::Config;
use oxide_core::EngineHandle;
use oxide_protocol::{ApprovalDecision, ApprovalPolicy, Event, Op, SandboxPolicy};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const CSS: &str = include_str!("../assets/style.css");
const LOGO_BYTES: &[u8] = include_bytes!("../assets/logo.png");

fn logo_uri() -> &'static str {
    static URI: OnceLock<String> = OnceLock::new();
    URI.get_or_init(|| {
        use base64::Engine;
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(LOGO_BYTES)
        )
    })
    .as_str()
}

const XTERM_CSS: &str = include_str!("../assets/xterm/xterm.css");
const XTERM_JS: &str = include_str!("../assets/xterm/xterm.js");
const XTERM_FIT_JS: &str = include_str!("../assets/xterm/addon-fit.js");

// Brand logos for the provider picker (inline SVG).
const SVG_CLAUDE: &str = include_str!("../assets/providers/claude-icon.svg");
const SVG_OPENAI: &str = include_str!("../assets/providers/openai-icon.svg");
const SVG_CURSOR: &str = include_str!("../assets/providers/cursor.svg");
const SVG_MCP: &str = include_str!("../assets/providers/mcp-icon.svg");

/// SVG markup from `<svg` onward (drops the `<?xml?>` prolog) for inline use.
fn svg_inner(s: &str) -> String {
    match s.find("<svg") {
        Some(i) => s[i..].to_string(),
        None => s.to_string(),
    }
}

/// Brand logo markup for a provider, if it has one. The OpenAI mark is black,
/// so it is recolored for the dark UI.
fn provider_logo(provider: &str) -> Option<String> {
    match provider {
        "chatgpt" | "codex" | "openai" => Some(svg_inner(SVG_OPENAI).replace("#000000", "currentColor")),
        "claude" | "anthropic" => Some(svg_inner(SVG_CLAUDE)),
        "cursor" => Some(svg_inner(SVG_CURSOR)),
        "mcp" => Some(svg_inner(SVG_MCP).replace("#000000", "currentColor")),
        _ => None,
    }
}

struct ModelPreset {
    provider: &'static str,
    model: &'static str,
    provider_label: &'static str,
    label: &'static str,
    summary: &'static str,
    badge: &'static str,
    fast: bool,
}

/// Two current production-ready choices per implemented provider.
const MODEL_PRESETS: &[ModelPreset] = &[
    ModelPreset {
        provider: "chatgpt",
        model: "gpt-5.5",
        provider_label: "ChatGPT subscription",
        label: "GPT-5.5",
        summary: "Your ChatGPT Plus/Pro — no API key, no CLI",
        badge: "Subs",
        fast: false,
    },
    ModelPreset {
        provider: "codex",
        model: "gpt-5.5",
        provider_label: "Codex",
        label: "GPT-5.5",
        summary: "Best for complex coding agents",
        badge: "Smart",
        fast: false,
    },
    ModelPreset {
        provider: "codex",
        model: "gpt-5.4",
        provider_label: "Codex",
        label: "GPT-5.4",
        summary: "Faster frontier coding and subagents",
        badge: "Fast",
        fast: true,
    },
    ModelPreset {
        provider: "claude",
        model: "claude-opus-4-8",
        provider_label: "Claude Code",
        label: "Opus 4.8",
        summary: "Deep coding and agentic reasoning",
        badge: "Smart",
        fast: false,
    },
    ModelPreset {
        provider: "claude",
        model: "claude-sonnet-4-6",
        provider_label: "Claude Code",
        label: "Sonnet 4.6",
        summary: "Balanced speed and intelligence",
        badge: "Fast",
        fast: true,
    },
    ModelPreset {
        provider: "openai",
        model: "gpt-5.5",
        provider_label: "OpenAI API",
        label: "GPT-5.5",
        summary: "Best default for coding workflows",
        badge: "Smart",
        fast: false,
    },
    ModelPreset {
        provider: "openai",
        model: "gpt-5.4",
        provider_label: "OpenAI API",
        label: "GPT-5.4",
        summary: "Faster frontier coding lane",
        badge: "Fast",
        fast: true,
    },
    ModelPreset {
        provider: "anthropic",
        model: "claude-opus-4-8",
        provider_label: "Anthropic API",
        label: "Opus 4.8",
        summary: "Most capable Claude model",
        badge: "Smart",
        fast: false,
    },
    ModelPreset {
        provider: "anthropic",
        model: "claude-sonnet-4-6",
        provider_label: "Anthropic API",
        label: "Sonnet 4.6",
        summary: "Fast daily agent work",
        badge: "Fast",
        fast: true,
    },
];

struct EffortPreset {
    value: &'static str,
    label: &'static str,
    summary: &'static str,
}

const EFFORT_PRESETS: &[EffortPreset] = &[
    EffortPreset {
        value: "low",
        label: "Low",
        summary: "Best speed and token efficiency",
    },
    EffortPreset {
        value: "medium",
        label: "Medium",
        summary: "Balanced default for everyday work",
    },
    EffortPreset {
        value: "high",
        label: "High",
        summary: "More thorough planning and tool use",
    },
    EffortPreset {
        value: "xhigh",
        label: "Extra",
        summary: "Hardest long-running agent tasks",
    },
];

fn selected_model(provider: &str, model: &str) -> Option<&'static ModelPreset> {
    MODEL_PRESETS
        .iter()
        .find(|p| p.provider == provider && p.model == model)
}

fn fast_model_for(provider: &str) -> Option<&'static ModelPreset> {
    MODEL_PRESETS
        .iter()
        .find(|p| p.provider == provider && p.fast)
}

fn model_matches(preset: &ModelPreset, query: &str) -> bool {
    query.is_empty()
        || preset.provider.contains(query)
        || preset.model.contains(query)
        || preset.provider_label.to_ascii_lowercase().contains(query)
        || preset.label.to_ascii_lowercase().contains(query)
        || preset.summary.to_ascii_lowercase().contains(query)
        || preset.badge.to_ascii_lowercase().contains(query)
}

fn effort_label(value: &str) -> &'static str {
    EFFORT_PRESETS
        .iter()
        .find(|p| p.value == value)
        .map(|p| p.label)
        .unwrap_or("Medium")
}

fn workspace_of(config: &Config) -> PathBuf {
    if let Some(ws) = &config.workspace {
        return ws.clone();
    }
    // No folder chosen yet (welcome state). Avoid root "/" — fall back to HOME
    // so the file tree isn't the whole filesystem.
    match std::env::current_dir().ok() {
        Some(p) if p != PathBuf::from("/") => p,
        _ => std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")),
    }
}

fn project_name(ws: &std::path::Path) -> String {
    ws.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "workspace".to_string())
}

fn git_branch(ws: &std::path::Path) -> String {
    std::fs::read_to_string(ws.join(".git/HEAD"))
        .ok()
        .and_then(|h| {
            h.trim()
                .strip_prefix("ref: refs/heads/")
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "main".to_string())
}

/// Children of `dir` (dirs first, then files), skipping noisy `target`.
fn read_children(dir: &std::path::Path) -> Vec<(PathBuf, bool)> {
    let mut v: Vec<(PathBuf, bool)> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .map(|e| (e.path(), e.path().is_dir()))
            .filter(|(p, _)| p.file_name().map(|n| n != "target").unwrap_or(true))
            .collect(),
        Err(_) => Vec::new(),
    };
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.file_name().cmp(&b.0.file_name())));
    v
}

pub fn run(config: Config) -> anyhow::Result<()> {
    let window = WindowBuilder::new()
        .with_title("Oxide")
        .with_maximized(true)
        .with_inner_size(dioxus::desktop::tao::dpi::LogicalSize::new(1280.0, 820.0));
    LaunchBuilder::desktop()
        .with_cfg(DesktopConfig::new().with_window(window))
        .with_context(config)
        .launch(app);
    Ok(())
}

#[derive(Clone, PartialEq)]
enum Author {
    User,
    Agent,
    Note,
    /// A reviewable file diff: (path, checkpoint id to rewind).
    Diff(String, u64),
    /// A tool activity row (terminal/edit/read/…): (running, ok).
    Activity { running: bool, ok: bool },
}

/// Count added/removed lines in a unified diff (excludes the +++/--- headers).
fn diff_counts(diff: &str) -> (u32, u32) {
    let mut adds = 0;
    let mut dels = 0;
    for l in diff.lines() {
        if l.starts_with("+++") || l.starts_with("---") {
            continue;
        }
        if l.starts_with('+') {
            adds += 1;
        } else if l.starts_with('-') {
            dels += 1;
        }
    }
    (adds, dels)
}

/// Coarse, human status verb for the live pill (opencode-style).
fn status_verb(tool: &str) -> &'static str {
    match tool {
        "shell" => "Running commands",
        "search" | "codebase_search" => "Searching the codebase",
        "read_file" => "Reading files",
        "write_file" | "edit" => "Making edits",
        "remember" | "save_skill" => "Saving to memory",
        "web_search" | "fetch_url" => "Searching the web",
        "ask_user" => "Asking you",
        t if t.starts_with("browser_") => "Browsing",
        t if t.starts_with("mcp__") => "Using tools",
        _ => "Working",
    }
}

/// `(icon, verb, detail)` for a tool activity row, joined as "icon\tverb\tdetail".
fn activity_label(tool: &str, args: &serde_json::Value) -> String {
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let short = |t: &str| t.chars().take(90).collect::<String>();
    let (icon, verb, detail) = match tool {
        "shell" => ("terminal", "Run", short(s("command"))),
        "write_file" => ("edit", "Write", s("path").to_string()),
        "edit" => ("edit", "Edit", s("path").to_string()),
        "read_file" => ("file", "Read", s("path").to_string()),
        "search" => ("search", "Search", s("pattern").to_string()),
        "codebase_search" => ("search", "Find code", short(s("query"))),
        "web_search" => ("globe", "Search web", short(s("query"))),
        "fetch_url" => ("globe", "Fetch", s("url").to_string()),
        "browser_navigate" => ("globe", "Open", s("url").to_string()),
        t if t.starts_with("browser_") => ("globe", "Browser", t.trim_start_matches("browser_").to_string()),
        "remember" => ("brain", "Remember", String::new()),
        "save_skill" => ("brain", "Save skill", String::new()),
        other => ("spark", "Tool", other.to_string()),
    };
    format!("{icon}\t{verb}\t{detail}")
}

#[derive(Clone, PartialEq)]
struct ChatMsg {
    author: Author,
    text: String,
}

/// Commands sent into the engine coroutine.
enum EngineCmd {
    /// `engine` is the full prompt (with mention/skill/MCP context); `display`
    /// is the clean bubble text, optionally `"<space-joined mentions>\u{1}<body>"`.
    Submit { engine: String, display: String },
    Reconfigure(Config),
    /// Switch to another agent tab: reconfigure the engine to `0` and restore
    /// that tab's transcript `1` (without the message-clearing Reconfigure does).
    SwitchTab(Config, Vec<ChatMsg>),
    Approve { id: u64, decision: ApprovalDecision },
    Answer { id: u64, text: String },
    Rewind { id: u64 },
    Interrupt,
}

/// One agent session tab (its own provider + transcript) within a workspace.
#[derive(Clone, PartialEq)]
struct AgentTab {
    id: u64,
    title: String,
    provider: String,
    model: String,
    messages: Vec<ChatMsg>,
    /// "gui" = chat, "tui" = embedded terminal running a CLI.
    mode: String,
    /// CLI binary for tui mode (e.g. "codex", "claude").
    bin: String,
}

/// One row in the inspector Timeline.
#[derive(Clone, PartialEq)]
struct TimelineItem {
    title: String,
    sub: String,
}

/// Shared file/editor state, provided via context so tree nodes can reach it.
#[derive(Clone, Copy, PartialEq)]
struct Ui {
    workspace: Signal<PathBuf>,
    expanded: Signal<HashSet<PathBuf>>,
    open_path: Signal<Option<PathBuf>>,
    editor_text: Signal<String>,
    dirty: Signal<bool>,
}

/// Detect an in-progress `@mention` at the end of the input: returns the byte
/// index of the `@` and the query typed after it (no whitespace yet).
fn active_mention(text: &str) -> Option<(usize, String)> {
    let at = text.rfind('@')?;
    // The `@` must start a token (preceded by start-of-string or whitespace).
    if at > 0 {
        let prev = text[..at].chars().next_back().unwrap();
        if !prev.is_whitespace() {
            return None;
        }
    }
    let q = &text[at + 1..];
    if q.chars().any(|c| c.is_whitespace()) {
        return None;
    }
    Some((at, q.to_string()))
}

/// Walk the workspace for files/folders matching `query` (codebase `@` picker).
fn mention_candidates(ws: &Path, query: &str) -> Vec<String> {
    let q = query.to_ascii_lowercase();
    let mut out: Vec<(bool, String)> = Vec::new();
    let mut stack = vec![ws.to_path_buf()];
    let mut visited = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            visited += 1;
            if visited > 12000 {
                break;
            }
            let p = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if matches!(name.as_ref(), "target" | ".git" | "node_modules" | ".oxide") {
                continue;
            }
            let is_dir = p.is_dir();
            if let Ok(rel) = p.strip_prefix(ws) {
                let rels = rel.to_string_lossy().replace('\\', "/");
                if q.is_empty() || rels.to_ascii_lowercase().contains(&q) {
                    out.push((is_dir, rels));
                }
            }
            if is_dir {
                stack.push(p);
            }
            if out.len() > 400 {
                break;
            }
        }
    }
    // Dirs first, then shorter/shallower paths.
    out.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(a.1.matches('/').count().cmp(&b.1.matches('/').count()))
            .then(a.1.cmp(&b.1))
    });
    out.into_iter()
        .take(40)
        .map(|(d, s)| if d { format!("{s}/") } else { s })
        .collect()
}

/// ChatGPT subscription connection status from the codex OAuth file.
fn chatgpt_status() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let txt = std::fs::read_to_string(format!("{home}/.codex/auth.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
    v["tokens"]["access_token"].as_str()?;
    let mode = v["auth_mode"].as_str().unwrap_or("chatgpt");
    Some(format!("Connected · {mode}"))
}

/// Build the prompt (mode prefixes + mention/skill context) and submit it.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
/// JS: report the in-progress `@query` at the caret + whether the editor is empty.
const CE_QUERY_JS: &str = r#"
const el=document.getElementById('ce-input');
let q=null;
const sel=window.getSelection();
if(sel && sel.rangeCount){
  const r=sel.getRangeAt(0); const n=r.startContainer;
  if(n.nodeType===3){
    const t=n.textContent.slice(0,r.startOffset);
    const m=t.match(/(?:^|\s)@([^\s@]*)$/);
    if(m) q=m[1];
  }
}
const empty = !el || (el.textContent.replace(/ /g,'').trim()==='' && el.querySelectorAll('.ce-chip').length===0);
if (empty && el && el.innerHTML !== '') el.innerHTML = '';
return JSON.stringify({q, empty});
"#;

/// JS: serialize the editor into `{body, tokens}` for submission.
const CE_SERIALIZE_JS: &str = r#"
const el=document.getElementById('ce-input'); if(!el) return '{}';
let body=''; const tokens=[];
el.childNodes.forEach(n=>{
  if(n.nodeType===3) body+=n.textContent;
  else if(n.classList && n.classList.contains('ce-chip')){ tokens.push(n.dataset.token); body+='@'+(n.textContent||''); }
  else body+=(n.textContent||'');
});
return JSON.stringify({body: body.replace(/ /g,' ').trim(), tokens});
"#;

/// JS to replace the caret's `@query` with an inline chip span.
fn ce_insert_js(token: &str, label: &str) -> String {
    let token = serde_json::to_string(token).unwrap_or_else(|_| "\"\"".into());
    let label = serde_json::to_string(label).unwrap_or_else(|_| "\"\"".into());
    format!(
        r#"
const sel=window.getSelection(); if(!sel||!sel.rangeCount) return false;
const r=sel.getRangeAt(0); const n=r.startContainer;
if(n.nodeType!==3) return false;
const t=n.textContent; const off=r.startOffset;
const m=t.slice(0,off).match(/(?:^|\s)@([^\s@]*)$/);
if(!m) return false;
const start=off - m[1].length - 1;
const after=n.splitText(start);
after.textContent=after.textContent.slice(m[1].length+1);
const chip=document.createElement('span');
chip.className='ce-chip'; chip.setAttribute('contenteditable','false');
chip.dataset.token={token}; chip.textContent={label};
const sp=document.createTextNode(' ');
n.parentNode.insertBefore(chip, after);
n.parentNode.insertBefore(sp, after);
const nr=document.createRange(); nr.setStartAfter(sp); nr.collapse(true);
sel.removeAllRanges(); sel.addRange(nr);
const ed=document.getElementById('ce-input'); if(ed) ed.focus();
return true;
"#,
        token = token,
        label = label
    )
}

/// Split user text into `(is_mention, text)` segments — `@word` at a word
/// boundary becomes a mention pill.
/// Strip the prompt scaffolding the composer injects (context files, MCP/skill
/// blocks, plan/pursue tags, git context, picked-element, image notes) so a
/// persisted/resumed user message renders as just the human text + chips.
fn strip_scaffold(text: &str) -> String {
    const DROP_PREFIX: &[&str] = &[
        "Context files:", "Use these MCP servers", "- `", "## Skill:",
        "## Git context", "## Working git diff", "### status", "### recent commits",
        "### working diff", "(Use the `", "[Preview selection", "[Plan mode]",
        "[Pursue goal]", "(user attached", "- selector:", "- component:",
        "- source:", "- text:", "- html:", "Selected UI element",
    ];
    let mut keep = Vec::new();
    let mut in_diff_fence = false;
    for line in text.lines() {
        let l = line.trim_start();
        if in_diff_fence {
            if l.starts_with("```") { in_diff_fence = false; }
            continue; // drop the whole injected ```diff block
        }
        if l.starts_with("```diff") {
            in_diff_fence = true;
            continue;
        }
        if DROP_PREFIX.iter().any(|p| l.starts_with(p)) {
            continue;
        }
        keep.push(line);
    }
    keep.join("\n").trim().to_string()
}

fn user_segments(text: &str) -> Vec<(bool, String)> {
    let text = strip_scaffold(text);
    let text = text.as_str();
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<(bool, String)> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;
    while i < chars.len() {
        let at_word_start = i == 0 || chars[i - 1].is_whitespace();
        if chars[i] == '@' && at_word_start {
            let mut j = i + 1;
            let mut name = String::new();
            while j < chars.len() && !chars[j].is_whitespace() {
                name.push(chars[j]);
                j += 1;
            }
            if !name.is_empty() {
                if !buf.is_empty() {
                    out.push((false, std::mem::take(&mut buf)));
                }
                out.push((true, name));
                i = j;
                continue;
            }
        }
        buf.push(chars[i]);
        i += 1;
    }
    if !buf.is_empty() {
        out.push((false, buf));
    }
    out
}

/// Strip the `mcp:`/`skill:` prefix from a mention token for its chip label.
fn mention_label(token: &str) -> String {
    token
        .strip_prefix("mcp:")
        .or_else(|| token.strip_prefix("skill:"))
        .or_else(|| token.strip_prefix("ctx:"))
        .unwrap_or(token)
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(token)
        .to_string()
}

#[allow(clippy::too_many_arguments)]
fn submit_prompt(
    mut input: Signal<String>,
    streaming: Signal<bool>,
    engine: Coroutine<EngineCmd>,
    plan_mode: Signal<bool>,
    pursue_goal: Signal<bool>,
    goal_text: Signal<String>,
    mut mentions: Signal<Vec<String>>,
    mut queue: Signal<Vec<String>>,
    steer: bool,
    ws: &Path,
) {
    let raw = input.read().trim().to_string();
    if raw.is_empty() {
        return;
    }
    let mut text = String::new();
    if *plan_mode.read() {
        text.push_str("[Plan mode] Produce a clear, numbered plan first and do NOT modify anything yet — wait for approval.\n\n");
    }
    if *pursue_goal.read() {
        let g = goal_text.read().clone();
        if g.trim().is_empty() {
            text.push_str("[Pursue goal] Keep working autonomously until this is fully done.\n\n");
        } else {
            text.push_str(&format!("[Pursue goal] Keep working autonomously until this goal is fully done: {}\n\n", g.trim()));
        }
    }
    let ms = mentions.read().clone();
    if !ms.is_empty() {
        let mut files = Vec::new();
        let mut skills_block = String::new();
        let mut mcp_block = String::new();
        for m in &ms {
            if let Some(name) = m.strip_prefix("mcp:") {
                mcp_block.push_str(&format!("\n- `{name}` — call its tools via `mcp__{name}__*`"));
            } else if let Some(name) = m.strip_prefix("skill:") {
                let p = ws.join(".oxide/memory/skills").join(format!("{name}.md"));
                match std::fs::read_to_string(&p) {
                    Ok(c) => skills_block.push_str(&format!("\n## Skill: {name}\n{}\n", c.trim())),
                    Err(_) => skills_block.push_str(&format!("\n## Skill: {name} (not found)\n")),
                }
            } else {
                files.push(format!("@{m}"));
            }
        }
        if !mcp_block.is_empty() {
            text.push_str("Use these MCP servers for this task:");
            text.push_str(&mcp_block);
            text.push('\n');
        }
        if !files.is_empty() {
            text.push_str("Context files: ");
            text.push_str(&files.join(" "));
            text.push('\n');
        }
        if !skills_block.is_empty() {
            text.push_str(&skills_block);
        }
        text.push('\n');
    }
    text.push_str(&raw);
    // Clean bubble text: just what the user typed, with the picked mentions
    // shown as chips (encoded before a \u{1} marker) instead of inline context.
    let display = if ms.is_empty() {
        raw.clone()
    } else {
        format!("{}\u{1}{}", ms.join(" "), raw)
    };
    mentions.set(Vec::new());
    input.set(String::new());
    if !steer && *streaming.read() {
        // Default while running: queue — don't disturb the current turn.
        queue.write().push(text);
    } else {
        // Idle → new turn. Steer → inject into the running turn.
        let _ = engine.send(EngineCmd::Submit { engine: text, display });
    }
}

/// Serialize the contenteditable composer, build the prompt, and submit it.
#[allow(clippy::too_many_arguments)]
async fn submit_ce(
    streaming: Signal<bool>,
    engine: Coroutine<EngineCmd>,
    plan_mode: Signal<bool>,
    pursue_goal: Signal<bool>,
    goal_text: Signal<String>,
    mut queue: Signal<Vec<String>>,
    mut attachments: Signal<Vec<String>>,
    mut picked_element: Signal<Option<String>>,
    steer: bool,
    ws: PathBuf,
) {
    let json = dioxus::document::eval(CE_SERIALIZE_JS).join::<String>().await.unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap_or(serde_json::Value::Null);
    let body = v["body"].as_str().unwrap_or("").trim().to_string();
    let tokens: Vec<String> = v["tokens"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let n_imgs = attachments.read().len();
    let picked = picked_element.read().clone();
    if body.is_empty() && tokens.is_empty() && n_imgs == 0 && picked.is_none() {
        return;
    }
    // Built-in /review (Bugbot): review the working diff for bugs.
    if body.trim_start().starts_with("/review") {
        let _ = dioxus::document::eval("const e=document.getElementById('ce-input'); if(e) e.innerHTML='';").join::<bool>().await;
        let extra = body.trim_start().trim_start_matches("/review").trim();
        let diff = run_cmd(&ws, "git", &["diff"]).await;
        let diff: String = diff.chars().take(12000).collect();
        let prompt = format!(
            "Act as Bugbot. Review the current working changes for bugs, security issues, \
logic errors, and regressions. For each finding give: file:line, severity (high/med/low), \
why it's wrong, and the concrete fix. If the diff is clean, say so plainly.{}\n\n```diff\n{}\n```",
            if extra.is_empty() { String::new() } else { format!(" Extra focus: {extra}.") },
            diff
        );
        let _ = engine.send(EngineCmd::Submit { engine: prompt, display: "/review (Bugbot)".into() });
        return;
    }
    // Clear the editor immediately so a rapid second Enter can't double-submit
    // (the next serialize reads an empty body and returns).
    let _ = dioxus::document::eval("const e=document.getElementById('ce-input'); if(e) e.innerHTML='';")
        .join::<bool>()
        .await;
    let mut text = String::new();
    if *plan_mode.read() {
        text.push_str("[Plan mode] Produce a clear, numbered plan first and do NOT modify anything yet — wait for approval.\n\n");
    }
    if *pursue_goal.read() {
        let g = goal_text.read().clone();
        if g.trim().is_empty() {
            text.push_str("[Pursue goal] Keep working autonomously until this is fully done.\n\n");
        } else {
            text.push_str(&format!("[Pursue goal] Keep working autonomously until this goal is fully done: {}\n\n", g.trim()));
        }
    }
    let mut files = Vec::new();
    let mut skills_block = String::new();
    let mut mcp_block = String::new();
    let mut ctx_block = String::new();
    for tkn in &tokens {
        if let Some(name) = tkn.strip_prefix("mcp:") {
            mcp_block.push_str(&format!("\n- `{name}` — call its tools via `mcp__{name}__*`"));
        } else if let Some(name) = tkn.strip_prefix("skill:") {
            let p = ws.join(".oxide/memory/skills").join(format!("{name}.md"));
            match std::fs::read_to_string(&p) {
                Ok(c) => skills_block.push_str(&format!("\n## Skill: {name}\n{}\n", c.trim())),
                Err(_) => skills_block.push_str(&format!("\n## Skill: {name} (not found)\n")),
            }
        } else if let Some(kind) = tkn.strip_prefix("ctx:") {
            match kind {
                "git" => {
                    let st = run_cmd(&ws, "git", &["status", "--short", "--branch"]).await;
                    let df = run_cmd(&ws, "git", &["diff"]).await;
                    let lg = run_cmd(&ws, "git", &["log", "--oneline", "-10"]).await;
                    let df: String = df.chars().take(6000).collect();
                    ctx_block.push_str(&format!("\n## Git context\n### status\n{st}\n### recent commits\n{lg}\n### working diff\n```diff\n{df}\n```\n"));
                }
                "diff" => {
                    let df = run_cmd(&ws, "git", &["diff"]).await;
                    let df: String = df.chars().take(8000).collect();
                    ctx_block.push_str(&format!("\n## Working git diff\n```diff\n{df}\n```\n"));
                }
                "codebase" => ctx_block.push_str("\n(Use the `codebase_search` tool to find relevant code semantically before acting.)\n"),
                "web" => ctx_block.push_str("\n(Use the `web_search` tool to research this on the web.)\n"),
                _ => {}
            }
        } else {
            files.push(format!("@{tkn}"));
        }
    }
    if !ctx_block.is_empty() {
        text.push_str(&ctx_block);
        text.push('\n');
    }
    if !mcp_block.is_empty() {
        text.push_str("Use these MCP servers for this task:");
        text.push_str(&mcp_block);
        text.push('\n');
    }
    if !files.is_empty() {
        text.push_str("Context files: ");
        text.push_str(&files.join(" "));
        text.push('\n');
    }
    if !skills_block.is_empty() {
        text.push_str(&skills_block);
    }
    if !tokens.is_empty() {
        text.push('\n');
    }
    if n_imgs > 0 {
        text.push_str(&format!("\n(user attached {n_imgs} image{})", if n_imgs == 1 { "" } else { "s" }));
    }
    if let Some(p) = &picked {
        text.push_str(&format!("\n[Preview selection — change this element]\n{p}\n"));
        picked_element.set(None);
    }
    text.push_str(&body);
    let display = if n_imgs > 0 {
        format!("{body} [{n_imgs} image{}]", if n_imgs == 1 { "" } else { "s" })
    } else {
        body
    };
    attachments.write().clear();
    if !steer && *streaming.read() {
        queue.write().push(text);
    } else {
        let _ = engine.send(EngineCmd::Submit { engine: text, display });
    }
}

/// Saved skills matching `query`, returned as `skill:<name>` tokens.
fn skill_candidates(ws: &Path, query: &str) -> Vec<String> {
    let q = query.to_ascii_lowercase();
    let dir = ws.join(".oxide/memory/skills");
    let mut v: Vec<String> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("md"))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().to_string()))
        .filter(|n| q.is_empty() || n.to_ascii_lowercase().contains(&q))
        .map(|n| format!("skill:{n}"))
        .collect();
    v.sort();
    v.truncate(12);
    v
}

/// Parse `name:` / `description:` from a SKILL.md frontmatter block.
fn parse_skill_md(path: &Path) -> Option<(String, String)> {
    let text = std::fs::read_to_string(path).ok()?;
    let fm = text.strip_prefix("---").and_then(|r| r.find("\n---").map(|e| r[..e].to_string()));
    let fallback = path
        .parent()
        .and_then(|p| p.file_name())
        .or_else(|| path.file_stem())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let (mut name, mut desc) = (fallback, String::new());
    if let Some(fm) = fm {
        for l in fm.lines() {
            let l = l.trim();
            if let Some(v) = l.strip_prefix("name:") {
                name = v.trim().trim_matches('"').to_string();
            } else if let Some(v) = l.strip_prefix("description:") {
                desc = v.trim().trim_matches('"').to_string();
            }
        }
    }
    Some((name, desc))
}

/// Aggregate skills available from Oxide, Claude Code, and Codex.
fn discover_skills(ws: &Path) -> Vec<(&'static str, String, String)> {
    let mut out: Vec<(&'static str, String, String)> = Vec::new();

    // Oxide learned skills.
    if let Ok(rd) = std::fs::read_dir(ws.join(".oxide/memory/skills")) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("md") {
                let name = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                let desc = std::fs::read_to_string(&p)
                    .ok()
                    .and_then(|t| t.lines().find(|l| !l.trim().is_empty()).map(|l| l.trim().trim_start_matches('#').trim().to_string()))
                    .unwrap_or_default();
                out.push(("Oxide", name, desc));
            }
        }
    }

    let home = std::env::var("HOME").unwrap_or_default();

    // Claude Code skills: ~/.claude/skills/*/SKILL.md
    if let Ok(rd) = std::fs::read_dir(format!("{home}/.claude/skills")) {
        for e in rd.flatten() {
            let sk = e.path().join("SKILL.md");
            if sk.exists() {
                if let Some((n, d)) = parse_skill_md(&sk) {
                    out.push(("Claude Code", n, d));
                }
            }
        }
    }

    // Codex skills: walk ~/.codex/plugins for SKILL.md (bounded).
    let mut stack = vec![PathBuf::from(format!("{home}/.codex/plugins"))];
    let mut visited = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            visited += 1;
            if visited > 20000 || out.len() > 400 {
                break;
            }
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().and_then(|n| n.to_str()) == Some("SKILL.md") {
                if let Some((n, d)) = parse_skill_md(&p) {
                    out.push(("Codex", n, d));
                }
            }
        }
    }
    out
}

/// Launch all To-Do cards in parallel, each in its own git worktree.
fn run_board(mut board: Signal<board::Board>, cfg: Signal<Config>, root: PathBuf) {
    let todo: Vec<(u64, String, String)> = board
        .read()
        .cards
        .iter()
        .filter(|c| c.column == board::TODO)
        .map(|c| (c.id, c.title.clone(), c.desc.clone()))
        .collect();
    for (id, title, desc) in todo {
        {
            let mut b = board.write();
            if let Some(c) = b.cards.iter_mut().find(|c| c.id == id) {
                c.column = board::DOING.to_string();
            }
        }
        let base = cfg.read().clone();
        let root = root.clone();
        spawn(async move {
            let (result, branch) = board::run_card(base, title, desc, id, root.clone()).await;
            let snapshot = {
                let mut b = board.write();
                if let Some(c) = b.cards.iter_mut().find(|c| c.id == id) {
                    c.column = board::REVIEW.to_string();
                    c.result = result;
                    c.branch = branch;
                }
                b.clone()
            };
            snapshot.save(&root);
        });
    }
}

/// Set the permission mode (approval policy + sandbox) and reconfigure.
fn set_access_mode(
    mut cfg: Signal<Config>,
    engine: Coroutine<EngineCmd>,
    mut show_access: Signal<bool>,
    approval: ApprovalPolicy,
    sandbox: SandboxPolicy,
) {
    let mut c = cfg.read().clone();
    c.approval_policy = approval;
    c.sandbox = sandbox;
    cfg.set(c.clone());
    let _ = engine.send(EngineCmd::Reconfigure(c));
    show_access.set(false);
}

/// Available harness ids: builtins + manifests scanned from `dir`.
fn list_harnesses(dir: &Path) -> Vec<String> {
    let mut out = vec!["default".to_string(), "hermes".to_string()];
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("toml") {
                if let Ok(text) = std::fs::read_to_string(&p) {
                    if let Some(id) = text
                        .lines()
                        .find_map(|l| l.trim().strip_prefix("id ="))
                        .map(|v| v.trim().trim_matches('"').to_string())
                    {
                        if !out.contains(&id) {
                            out.push(id);
                        }
                    }
                }
            }
        }
    }
    out
}

/// Active `/slash` query (input starts with `/`, no space yet).
fn active_slash(text: &str) -> Option<String> {
    let t = text.trim_start();
    let rest = t.strip_prefix('/')?;
    if rest.contains(char::is_whitespace) {
        return None;
    }
    Some(rest.to_string())
}

/// Available slash commands `(name, description)` matching `query`.
fn slash_commands(ws: &Path, query: &str) -> Vec<(String, String)> {
    let q = query.to_ascii_lowercase();
    // Built-in commands handled by the composer itself.
    let builtins = [
        ("review", "Bugbot — review the working git diff for bugs"),
    ];
    let mut v: Vec<(String, String)> = builtins
        .iter()
        .filter(|(n, _)| q.is_empty() || n.contains(&q))
        .map(|(n, d)| (n.to_string(), d.to_string()))
        .collect();
    let dir = ws.join(".oxide/commands");
    v.extend(std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("md"))
        .filter_map(|p| {
            let name = p.file_stem()?.to_string_lossy().to_string();
            if !q.is_empty() && !name.to_ascii_lowercase().contains(&q) {
                return None;
            }
            let desc = std::fs::read_to_string(&p)
                .ok()
                .and_then(|t| {
                    t.strip_prefix("---")
                        .and_then(|r| r.find("\n---").map(|e| r[..e].to_string()))
                        .and_then(|fm| {
                            fm.lines().find_map(|l| {
                                l.trim().strip_prefix("description:").map(|d| d.trim().trim_matches('"').to_string())
                            })
                        })
                })
                .unwrap_or_default();
            Some((name, desc))
        }));
    v.sort();
    v.dedup();
    v
}

/// Combined `@` menu: skills first, then files/folders.
/// MCP servers (own + auto-imported) matching `query`, as `mcp:<server>` tokens.
fn mcp_candidates(query: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    if let Ok(cfg) = Config::load() {
        for s in cfg.mcp_servers {
            if s.enabled {
                names.push(s.name);
            }
        }
    }
    for s in oxide_core::discover_external_mcp() {
        if s.enabled {
            names.push(s.name);
        }
    }
    names.sort();
    names.dedup();
    let q = query.to_lowercase();
    names
        .into_iter()
        .filter(|n| q.is_empty() || n.to_lowercase().contains(&q))
        .take(8)
        .map(|n| format!("mcp:{n}"))
        .collect()
}

fn all_mention_items(ws: &Path, query: &str) -> Vec<String> {
    let q = query.to_ascii_lowercase();
    // Special context providers (Cursor-style @git / @web / @codebase).
    let mut v: Vec<String> = ["ctx:git", "ctx:diff", "ctx:codebase", "ctx:web"]
        .iter()
        .filter(|t| q.is_empty() || t.contains(&q))
        .map(|t| t.to_string())
        .collect();
    v.extend(mcp_candidates(query));
    v.extend(skill_candidates(ws, query));
    v.extend(mention_candidates(ws, query));
    v
}

/// List persisted sessions (id, message count, path) newest first.
fn list_sessions(ws: &Path) -> Vec<(String, usize, PathBuf)> {
    let dir = ws.join(".oxide/sessions");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("jsonl"))
        .collect();
    paths.sort();
    paths.reverse();
    paths
        .into_iter()
        .take(50)
        .map(|p| {
            let id = p
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let count = std::fs::read_to_string(&p)
                .map(|t| t.lines().filter(|l| !l.trim().is_empty()).count())
                .unwrap_or(0);
            (id, count, p)
        })
        .collect()
}

/// Delete a saved session file.
fn delete_session(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Move a saved session into `.oxide/sessions/archive/` (hidden from the list).
fn archive_session(path: &Path) {
    if let Some(dir) = path.parent() {
        let arch = dir.join("archive");
        let _ = std::fs::create_dir_all(&arch);
        if let Some(name) = path.file_name() {
            let _ = std::fs::rename(path, arch.join(name));
        }
    }
}

/// First user line of a session as its title.
fn session_title(path: &Path) -> String {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| {
            t.lines().find_map(|l| {
                let v: serde_json::Value = serde_json::from_str(l).ok()?;
                if v["role"].as_str()? == "user" {
                    Some(v["content"].as_str()?.lines().next().unwrap_or("").chars().take(38).collect::<String>())
                } else {
                    None
                }
            })
        })
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Chat".to_string())
}

/// Recent non-empty sessions `(path, title, msg_count)`, newest first. Deletes
/// empty/0-byte session files as it scans (cleanup).
fn recent_sessions(ws: &Path) -> Vec<(PathBuf, std::time::SystemTime, String)> {
    let dir = ws.join(".oxide/sessions");
    let mut items: Vec<(PathBuf, std::time::SystemTime, String)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                continue;
            }
            let meta = e.metadata().ok();
            // Don't delete a brand-new empty file — it's likely the active
            // session still being written (otherwise we'd resurrect the bug).
            let fresh = meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok())
                .map(|d| d.as_secs() < 90)
                .unwrap_or(false);
            if meta.as_ref().map(|m| m.len()).unwrap_or(0) == 0 {
                if !fresh {
                    let _ = std::fs::remove_file(&p);
                }
                continue;
            }
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            let count = text.lines().filter(|l| !l.trim().is_empty()).count();
            if count == 0 {
                if !fresh {
                    let _ = std::fs::remove_file(&p);
                }
                continue;
            }
            let title = text
                .lines()
                .find_map(|l| {
                    let v: serde_json::Value = serde_json::from_str(l).ok()?;
                    if v["role"].as_str()? == "user" {
                        Some(v["content"].as_str()?.lines().next().unwrap_or("").chars().take(38).collect::<String>())
                    } else {
                        None
                    }
                })
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| "Chat".to_string());
            let _ = count;
            let mtime = meta.and_then(|m| m.modified().ok()).unwrap_or(std::time::UNIX_EPOCH);
            items.push((p, mtime, title));
        }
    }
    items.sort_by(|a, b| b.1.cmp(&a.1));
    items.into_iter().take(30).collect()
}

/// Format a reset-after duration (seconds) like "in 5h" / "in 5d".
fn fmt_reset(secs: u64) -> String {
    if secs == 0 {
        "—".to_string()
    } else if secs < 3600 {
        format!("{}m", (secs / 60).max(1))
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// Short relative time like "5m", "3h", "2d", "1w".
fn relative_time(t: std::time::SystemTime) -> String {
    let secs = std::time::SystemTime::now().duration_since(t).map(|d| d.as_secs()).unwrap_or(0);
    if secs < 3600 {
        format!("{}m", (secs / 60).max(1))
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else if secs < 604_800 {
        format!("{}d", secs / 86_400)
    } else {
        format!("{}w", secs / 604_800)
    }
}

/// Group recent sessions by project: `(workspace, name, [(path, title, reltime)])`.
fn build_projects(current: &Path, recents: &[PathBuf]) -> Vec<(PathBuf, String, Vec<(PathBuf, String, String)>)> {
    let mut seen = HashSet::new();
    let mut wss: Vec<PathBuf> = Vec::new();
    for w in std::iter::once(current.to_path_buf()).chain(recents.iter().cloned()) {
        if w.exists() && seen.insert(w.clone()) {
            wss.push(w);
        }
    }
    let mut out = Vec::new();
    for ws in wss {
        let is_current = ws == current;
        // Only scan the ACTIVE project's sessions; recents show a clickable
        // header and load their chats when switched to. This avoids eagerly
        // touching every folder's `.oxide/sessions` (which on external/removable
        // volumes re-triggers the macOS "allow access" prompt every build).
        let items: Vec<(PathBuf, String, String)> = if is_current {
            recent_sessions(&ws).into_iter().map(|(p, m, t)| (p, t, relative_time(m))).collect()
        } else {
            Vec::new()
        };
        let name = project_name(&ws);
        out.push((ws, name, items));
    }
    out
}

/// Open a saved session transcript in a new tab (view).
fn open_session_tab(
    mut tabs: Signal<Vec<AgentTab>>,
    mut active_tab: Signal<usize>,
    mut messages: Signal<Vec<ChatMsg>>,
    mut next_id: Signal<u64>,
    cfg: Signal<Config>,
    path: PathBuf,
    title: String,
) {
    let loaded = load_session(&path);
    let cur = *active_tab.read();
    // If the current tab is an empty GUI chat, open the history IN PLACE instead
    // of spawning a new tab (user picked a chat from history with nothing typed).
    let cur_empty = {
        let t = tabs.read();
        messages.read().is_empty()
            && t.get(cur).map(|tab| tab.mode == "gui" && tab.messages.is_empty()).unwrap_or(false)
    };
    if cur_empty {
        if let Some(t) = tabs.write().get_mut(cur) {
            t.title = title;
            t.messages = loaded.clone();
        }
        messages.set(loaded);
        return;
    }
    if let Some(t) = tabs.write().get_mut(cur) {
        t.messages = messages.read().clone();
    }
    let id = *next_id.read();
    next_id.set(id + 1);
    let (provider, model) = { let c = cfg.read(); (c.provider.clone(), c.model.clone()) };
    tabs.write().push(AgentTab {
        id,
        title,
        provider,
        model,
        messages: loaded.clone(),
        mode: "gui".to_string(),
        bin: String::new(),
    });
    let idx = tabs.read().len() - 1;
    active_tab.set(idx);
    messages.set(loaded);
}

/// Load a session transcript into chat messages.
fn load_session(path: &Path) -> Vec<ChatMsg> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            let role = v["role"].as_str()?;
            let content = v["content"].as_str()?.to_string();
            let author = match role {
                "user" => Author::User,
                "assistant" => Author::Agent,
                _ => Author::Note,
            };
            Some(ChatMsg { author, text: content })
        })
        .collect()
}

/// Run a git subcommand in the workspace, returning stdout (stderr appended).
/// Detect localhost servers: listening TCP ports + the owning process name.
/// macOS/Linux via `lsof`. Returns `(port, "pid/command")` sorted, deduped.
async fn scan_ports() -> Vec<(u16, String)> {
    let out = match tokio::process::Command::new("lsof")
        .args(["-nP", "-iTCP", "-sTCP:LISTEN"])
        .output().await {
        Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
        Err(_) => return Vec::new(),
    };
    // macOS/media daemons that squat on localhost ports — never a dev server.
    const DENY: &[&str] = &[
        "spotify", "rapportd", "controlce", "sharingd", "identityser", "rapport",
        "cloudd", "apsd", "trustd", "nsurlsess", "airplay", "wifiagent", "music",
        "podcasts", "supercond", "remoted", "launchd", "deleted", "syncdefa",
    ];
    // Runtimes that *are* dev servers — these we always surface.
    const DEV: &[&str] = &[
        "node", "vite", "next", "bun", "deno", "python", "ruby", "php", "cargo",
        "rustc", "webpack", "esbuild", "turbo", "npm", "pnpm", "yarn", "rails",
        "flask", "uvicorn", "gunicorn", "caddy", "dotnet", "java", "air", "gin",
        "hugo", "jekyll", "astro", "remix", "nuxt", "ng", "serve", "http-ser",
    ];
    let mut found: std::collections::BTreeMap<u16, String> = std::collections::BTreeMap::new();
    for line in out.lines().skip(1) {
        let mut cols = line.split_whitespace();
        let cmd = cols.next().unwrap_or("").to_string();
        let lc = cmd.to_ascii_lowercase();
        if DENY.iter().any(|d| lc.starts_with(d)) { continue; }
        // NAME column holds e.g. "127.0.0.1:5173" or "*:3000".
        if let Some(addr) = line.split_whitespace().find(|c| c.contains(':') && (c.contains("127.0.0.1") || c.starts_with("*:") || c.contains("[::1]") || c.contains("localhost"))) {
            if let Some(p) = addr.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()) {
                if matches!(p, 22 | 53 | 88 | 445 | 631 | 5353 | 7000) { continue; }
                let is_dev = DEV.iter().any(|d| lc.starts_with(d));
                let common = matches!(p, 3000..=3009 | 4000 | 4200 | 4321 | 5000..=5005 | 5173..=5180 | 8000..=8090 | 8788 | 9000 | 1234 | 5500);
                // Keep only plausible dev servers: a known runtime, or a common
                // dev port. Drops random ephemeral daemons.
                if is_dev || common {
                    found.entry(p).or_insert(cmd.clone());
                }
            }
        }
    }
    found.into_iter().collect()
}

/// Run an arbitrary command in the workspace, returning stdout+stderr.
async fn run_cmd(ws: &Path, cmd: &str, args: &[&str]) -> String {
    match tokio::process::Command::new(cmd).args(args).current_dir(ws).output().await {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).to_string();
            let err = String::from_utf8_lossy(&o.stderr);
            if !err.trim().is_empty() { s.push('\n'); s.push_str(&err); }
            if s.trim().is_empty() { "(done)".to_string() } else { s }
        }
        Err(e) => format!("{cmd} error: {e} — is it installed?"),
    }
}

async fn git_run(ws: PathBuf, args: Vec<String>) -> String {
    match tokio::process::Command::new("git")
        .args(&args)
        .current_dir(&ws)
        .output()
        .await
    {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).to_string();
            let err = String::from_utf8_lossy(&o.stderr);
            if !err.trim().is_empty() {
                s.push_str(&err);
            }
            s
        }
        Err(e) => format!("git error: {e}"),
    }
}

/// Commit an `@mention` selection: strip the in-progress `@query` from the
/// input and add the path as a chip.
fn pick_mention(mut input: Signal<String>, mut mentions: Signal<Vec<String>>, at: usize, path: String) {
    let text = input.read().clone();
    let base = text.get(..at).unwrap_or("").trim_end().to_string();
    input.set(base);
    let clean = path.trim_end_matches('/').to_string();
    let mut m = mentions.read().clone();
    if !m.contains(&clean) {
        m.push(clean);
    }
    mentions.set(m);
}

fn open_file(mut ui: Ui, path: PathBuf) {
    match std::fs::read_to_string(&path) {
        Ok(content) => {
            ui.editor_text.set(content);
            ui.open_path.set(Some(path));
            ui.dirty.set(false);
        }
        Err(e) => {
            ui.editor_text.set(format!("// cannot open: {e}"));
            ui.open_path.set(Some(path));
            ui.dirty.set(false);
        }
    }
}

fn app() -> Element {
    let initial = use_context::<Config>();

    // Live, editable configuration.
    let cfg = use_signal(|| initial.clone());
    let ws0 = workspace_of(&initial);

    // Chat state.
    let mut messages = use_signal(Vec::<ChatMsg>::new);
    let input = use_signal(String::new);
    let mut context_limit = use_signal(|| None::<u64>);
    let mut streaming = use_signal(|| false);

    // Panels.
    let mut show_files = use_signal(|| true);
    let mut show_terminal = use_signal(|| false);
    let mut show_settings = use_signal(|| false);
    let mut show_skills = use_signal(|| false);
    let mut show_mcp = use_signal(|| false);
    let mut show_theme_menu = use_signal(|| false);
    let mut theme_menu_pos = use_signal(|| (12.0f64, 44.0f64));
    // ⌘K command palette.
    let mut show_palette = use_signal(|| false);
    let mut show_shortcuts = use_signal(|| false);
    let mut palette_query = use_signal(String::new);
    let mut palette_sel = use_signal(|| 0usize);
    let mut pinned = use_signal(|| false);
    let win = dioxus::desktop::use_window();
    let mut mcp_status = use_signal(std::collections::HashMap::<String, String>::new);
    // ChatGPT subscription usage: (plan, 5h %, weekly %, 5h reset s, weekly reset s).
    let mut usage_info = use_signal(|| None::<(String, u8, u8, String, String)>);
    // Tiling split-view (each pane its own live engine).
    let mut show_split = use_signal(|| false);
    let mut show_preview = use_signal(|| false);
    let mut preview_url = use_signal(String::new);
    let mut preview_ports = use_signal(Vec::<(u16, String)>::new);
    let mut picked_element = use_signal(|| Option::<String>::None);
    let split_panes = use_signal(|| vec![(0u64, "gui".to_string(), cfg.read().provider.clone(), cfg.read().model.clone())]);
    let split_layout = use_signal(|| Tile::Leaf(0));
    let split_next_id = use_signal(|| 1u64);
    let split_drag = use_signal(|| None::<u64>);
    let split_rects = use_signal(std::collections::HashMap::<u64, (f64, f64, f64, f64)>::new);
    let mut show_board = use_signal(|| false);
    let mut board = use_signal(board::Board::default);
    let mut new_card_title = use_signal(String::new);
    type ProjGroup = (PathBuf, String, Vec<(PathBuf, String, String)>);
    let mut projects_list = use_signal(Vec::<ProjGroup>::new);
    let mut session_menu = use_signal(|| None::<PathBuf>);
    let mut expanded_projects = use_signal(HashSet::<String>::new);
    // Agent tabs (multiple agent sessions in one workspace).
    let initial_provider = cfg.read().provider.clone();
    let initial_model = cfg.read().model.clone();
    let mut tabs = use_signal(|| {
        vec![AgentTab {
            id: 0,
            title: provider_title(&initial_provider).to_string(),
            provider: initial_provider,
            model: initial_model,
            messages: Vec::new(),
            mode: "gui".to_string(),
            bin: String::new(),
        }]
    });
    let mut active_tab = use_signal(|| 0usize);
    let mut renaming_tab = use_signal(|| None::<u64>);
    let mut rename_text = use_signal(String::new);
    let next_tab_id = use_signal(|| 1u64);
    let mut show_newtab = use_signal(|| false);

    // Composer modes (shared across both Composer instances).
    let plan_mode = use_signal(|| false);
    let pursue_goal = use_signal(|| false);
    let mentions = use_signal(Vec::<String>::new);

    // Inspector (right panel) state — ported from the desktop command center.
    let mut inspector_tab = use_signal(|| "files".to_string());
    let mut timeline = use_signal(Vec::<TimelineItem>::new);
    let mut approvals = use_signal(Vec::<(u64, String, String)>::new);
    let mut checkpoints = use_signal(Vec::<(u64, String)>::new);
    let mut usage = use_signal(|| (0u64, 0u64));
    // Git / Browser / Goal tab state.
    let mut git_status = use_signal(Vec::<String>::new);
    let mut git_busy = use_signal(String::new);
    let mut git_refresh = use_signal(|| 0u32);
    let mut git_diff = use_signal(String::new);
    let mut commit_msg = use_signal(String::new);
    let mut browser_url = use_signal(String::new);
    let mut browser_log = use_signal(Vec::<String>::new);
    let goal_text = use_signal(String::new);
    let mut memory_text = use_signal(String::new);
    let mut thinking = use_signal(String::new);
    let mut queue = use_signal(Vec::<String>::new);
    let mut questions = use_signal(Vec::<(u64, String, Vec<String>)>::new);
    let mut q_answer = use_signal(String::new);
    let mut reverted = use_signal(HashSet::<u64>::new);
    // Edits made this turn: (path, adds, dels, checkpoint).
    let mut turn_edits = use_signal(Vec::<(String, u32, u32, u64, String)>::new);
    let mut todos = use_signal(Vec::<(String, String)>::new);
    let mut edits_expanded = use_signal(|| false);
    // Per activity-group open state (keyed by first row index). Defaults to the
    // running state but, once the user toggles, their choice sticks across the
    // streaming re-renders that would otherwise force it back open.
    let mut act_open = use_signal(std::collections::HashMap::<usize, bool>::new);
    let mut status = use_signal(String::new);
    let mut turn_start = use_signal(|| None::<std::time::Instant>);

    // File/editor signals, shared with tree/editor via context.
    let ws_sig = use_signal(|| ws0.clone());
    let expanded = use_signal(HashSet::<PathBuf>::new);
    let open_path = use_signal(|| None::<PathBuf>);
    let editor_text = use_signal(String::new);
    let dirty = use_signal(|| false);
    let ui = use_context_provider(|| Ui {
        workspace: ws_sig,
        expanded,
        open_path,
        editor_text,
        dirty,
    });

    // OTA self-update.
    let mut update_info = use_signal(|| None::<update::UpdateInfo>);
    let mut updating = use_signal(|| false);
    let mut update_pct = use_signal(|| 0.0f32);
    use_effect(move || {
        let repo = { let r = cfg.read().github_repo.clone(); if r.trim().is_empty() { "MANFIT7/oxide".to_string() } else { r } };
        let url = cfg.read().update_url.clone();
        spawn(async move {
            if let Some(info) = update::check(&repo, &url).await {
                update_info.set(Some(info));
            }
        });
    });

    // Global keyboard shortcuts (⌘K command palette, Esc to close).
    use_future(move || async move {
        let mut eval = dioxus::document::eval(
            r#"
            if (!window.__oxkeys) {
              window.__oxkeys = 1;
              document.addEventListener('keydown', function(e){
                if ((e.metaKey || e.ctrlKey) && (e.key === 'k' || e.key === 'K')) { e.preventDefault(); dioxus.send('palette'); }
                else if ((e.metaKey || e.ctrlKey) && e.key === '/') { e.preventDefault(); dioxus.send('shortcuts'); }
                else if ((e.metaKey || e.ctrlKey) && (e.key === 'b' || e.key === 'B')) { e.preventDefault(); dioxus.send('files'); }
                else if (e.key === 'Escape') { dioxus.send('esc'); }
              }, true);
            }
            while (true) { await new Promise(r => setTimeout(r, 3600000)); }
            "#,
        );
        loop {
            match eval.recv::<String>().await {
                Ok(k) if k == "palette" => { let v = !*show_palette.read(); show_palette.set(v); palette_query.set(String::new()); palette_sel.set(0); }
                Ok(k) if k == "files" => { let v = !*show_files.read(); show_files.set(v); }
                Ok(k) if k == "shortcuts" => { let v = !*show_shortcuts.read(); show_shortcuts.set(v); }
                Ok(k) if k == "esc" => { show_palette.set(false); show_shortcuts.set(false); }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });

    // Disable the WebView's native right-click menu (Reload / Inspect Element).
    use_effect(move || {
        spawn(async move {
            let _ = dioxus::document::eval(
                "if(!window.__oxnoctx){window.__oxnoctx=1;document.addEventListener('contextmenu',function(e){e.preventDefault();},{capture:true});}",
            );
        });
    });

    // Auto-scroll the chat to the bottom as content streams in.
    use_effect(move || {
        let _ = messages.read(); // subscribe to any transcript change
        let _ = thinking.read().len();
        let _ = status.read().len();
        spawn(async move {
            let _ = dioxus::document::eval(
                "requestAnimationFrame(()=>{var s=document.querySelector('.scroll');if(s)s.scrollTop=s.scrollHeight;});",
            );
        });
    });

    // Load the kanban board + recent chat sessions for the active workspace.
    use_effect(move || {
        let ws = ui.workspace.read().clone();
        if cfg.read().workspace.is_some() {
            board.set(board::Board::load(&ws));
            projects_list.set(build_projects(&ws, &cfg.read().recent_workspaces));
            // Clean up orphaned pane worktrees from a previous run.
            let ws2 = ws.clone();
            spawn(async move {
                let _ = tokio::process::Command::new("git").arg("-C").arg(&ws2).args(["worktree", "prune"]).output().await;
            });
        }
    });

    // Auto-rename the active tab from its first user message.
    use_effect(move || {
        let snippet = messages
            .read()
            .iter()
            .find(|m| m.author == Author::User)
            .map(|m| m.text.clone());
        if let Some(text) = snippet {
            let cur = *active_tab.read();
            let mut tw = tabs.write();
            if let Some(t) = tw.get_mut(cur) {
                if t.title == provider_title(&t.provider) {
                    t.title = make_title(&text);
                }
            }
        }
    });

    // Auto-load git status when the Git tab is open or files change.
    use_effect(move || {
        let on_git = *inspector_tab.read() == "git";
        let _bump = *git_refresh.read();
        if on_git {
            let ws = ui.workspace.read().clone();
            spawn(async move {
                let s = git_run(ws, vec!["status".into(), "--short".into()]).await;
                git_status.set(s.lines().map(|l| l.to_string()).collect());
            });
        }
    });

    // Terminal.
    let mut term_lines = use_signal(Vec::<String>::new);
    let mut term_input = use_signal(String::new);

    // ── Engine coroutine (reconfigurable) ─────────────────────────────
    let engine = use_coroutine(move |mut rx: UnboundedReceiver<EngineCmd>| {
        let initial = initial.clone();
        async move {
            let (ev_tx, mut ev_rx) = tokio::sync::mpsc::channel::<Event>(256);
            let mut handle: Option<EngineHandle> = None;
            let mut forwarder: Option<tokio::task::JoinHandle<()>> = None;

            // Spawn helper expanded inline (avoids closure borrow issues).
            macro_rules! start_engine {
                ($conf:expr) => {{
                    if let Some(f) = forwarder.take() {
                        f.abort();
                    }
                    match oxide_core::spawn($conf) {
                        Ok((h, mut events)) => {
                            handle = Some(h);
                            let tx = ev_tx.clone();
                            forwarder = Some(tokio::spawn(async move {
                                while let Some(e) = events.recv().await {
                                    if tx.send(e).await.is_err() {
                                        break;
                                    }
                                }
                            }));
                        }
                        Err(e) => {
                            let _ = ev_tx
                                .send(Event::Error {
                                    message: format!("engine: {e}"),
                                })
                                .await;
                        }
                    }
                }};
            }

            start_engine!(initial);

            loop {
                tokio::select! {
                    cmd = rx.next() => match cmd {
                        Some(EngineCmd::Submit { engine: eng, display }) => {
                            if let Some(h) = &handle {
                                messages.write().push(ChatMsg { author: Author::User, text: display });
                                messages.write().push(ChatMsg { author: Author::Agent, text: String::new() });
                                streaming.set(true);
                                let _ = h.submit(Op::UserTurn { text: eng }).await;
                            }
                        }
                        Some(EngineCmd::Reconfigure(conf)) => {
                            // Persist the new config (provider/model/effort/fast/…) so it survives restart.
                            let ws = workspace_of(&conf);
                            if let Ok(s) = toml::to_string(&conf) {
                                let _ = std::fs::write(ws.join("oxide.toml"), &s);
                                // Also persist globally so the packaged app remembers across launches.
                                if let Some(home) = std::env::var_os("HOME") {
                                    let gdir = std::path::PathBuf::from(home).join(".config/oxide");
                                    let _ = std::fs::create_dir_all(&gdir);
                                    let _ = std::fs::write(gdir.join("config.toml"), &s);
                                }
                            }
                            messages.write().clear();
                            approvals.write().clear();
                            checkpoints.write().clear();
                            timeline.write().clear();
                            streaming.set(false);
                            start_engine!(conf);
                        }
                        Some(EngineCmd::SwitchTab(conf, tab_msgs)) => {
                            approvals.write().clear();
                            checkpoints.write().clear();
                            timeline.write().clear();
                            streaming.set(false);
                            start_engine!(conf);
                            *messages.write() = tab_msgs; // restore this tab's transcript
                        }
                        Some(EngineCmd::Answer { id, text }) => {
                            if let Some(h) = &handle {
                                let _ = h.submit(Op::QuestionAnswer { request_id: id, answer: text.clone() }).await;
                            }
                            questions.write().retain(|(qid, _, _)| *qid != id);
                            messages.write().push(ChatMsg { author: Author::User, text });
                        }
                        Some(EngineCmd::Approve { id, decision }) => {
                            if let Some(h) = &handle {
                                let _ = h.submit(Op::ApprovalResponse { request_id: id, decision }).await;
                            }
                            approvals.write().retain(|(aid, _, _)| *aid != id);
                        }
                        Some(EngineCmd::Rewind { id }) => {
                            if let Some(h) = &handle {
                                let _ = h.submit(Op::Rewind { checkpoint_id: id }).await;
                            }
                        }
                        Some(EngineCmd::Interrupt) => {
                            if let Some(h) = &handle {
                                let _ = h.submit(Op::Interrupt).await;
                            }
                            streaming.set(false);
                        }
                        None => break,
                    },
                    Some(ev) = ev_rx.recv() => {
                        match ev {
                            Event::AgentMessageDelta { text, .. } => {
                                let mut m = messages.write();
                                match m.last_mut() {
                                    // Append to the open agent bubble; but if tools/diffs came after it,
                                    // start a NEW bubble so the answer shows below them (not lost).
                                    Some(last) if last.author == Author::Agent => last.text.push_str(&text),
                                    _ => m.push(ChatMsg { author: Author::Agent, text }),
                                }
                            }
                            Event::ReasoningDelta { text, .. } => {
                                thinking.write().push_str(&text);
                            }
                            Event::Info { text } => {
                                if text.starts_with("session") || text.starts_with("mcp ") || text.starts_with("mcp '") {
                                    // internal/MCP noise — status shown in the MCP manager, not chat
                                } else if text.starts_with(['🧭','⚙','🔍','🤖','🧩','🔁','✓','⚠']) {
                                    // pipeline stage → live animated status, not a chat note
                                    status.set(text);
                                } else {
                                    messages.write().push(ChatMsg { author: Author::Note, text });
                                }
                            }
                            Event::Error { message } => {
                                // MCP connect errors surface on the manager dots, not the chat.
                                if !message.starts_with("mcp '") {
                                    messages.write().push(ChatMsg { author: Author::Note, text: format!("error: {message}") });
                                }
                            }
                            Event::ContextWindow { limit } => context_limit.set(Some(limit)),
                            Event::McpServerStatus { name, status, tool_count, detail, .. } => {
                                mcp_status.write().insert(name.clone(), format!("{status} · {tool_count} tool(s) · {detail}"));
                            }
                            // ── Inspector capture ──────────────────────────
                            Event::Ready { harness } => {
                                timeline.write().push(TimelineItem { title: "Engine ready".into(), sub: format!("Harness: {harness}") });
                            }
                            Event::TurnStarted { turn } => {
                                thinking.set(String::new());
                                status.set("Working…".to_string());
                                turn_start.set(Some(std::time::Instant::now()));
                                turn_edits.write().clear();
                                todos.write().clear();
                                edits_expanded.set(false);
                                timeline.write().push(TimelineItem { title: format!("Turn {turn} started"), sub: String::new() });
                            }
                            Event::ApprovalRequested { request_id, tool, summary } => {
                                approvals.write().push((request_id, tool.clone(), summary.clone()));
                                timeline.write().push(TimelineItem { title: format!("Approval needed · {tool}"), sub: summary });
                            }
                            Event::ToolCallBegin { tool, args, .. } => {
                                timeline.write().push(TimelineItem { title: format!("⚙ {tool}"), sub: "running…".into() });
                                status.set(status_verb(&tool).to_string());
                                if tool != "ask_user" {
                                    messages.write().push(ChatMsg { author: Author::Activity { running: true, ok: true }, text: activity_label(&tool, &args) });
                                }
                            }
                            Event::ToolCallEnd { tool, output, ok, .. } => {
                                timeline.write().push(TimelineItem { title: format!("⚙ {tool}"), sub: if ok { "done".into() } else { "failed".into() } });
                                // Mark the most recent running activity row as finished and
                                // attach its output (truncated) so the row can expand it.
                                let mut out = output.trim().to_string();
                                if out.chars().count() > 4000 {
                                    out = out.chars().take(4000).collect::<String>() + "\n… (truncated)";
                                }
                                let mut m = messages.write();
                                if let Some(c) = m.iter_mut().rev().find(|c| matches!(c.author, Author::Activity { running: true, .. })) {
                                    c.author = Author::Activity { running: false, ok };
                                    if !out.is_empty() {
                                        c.text.push('\t');
                                        c.text.push_str(&out);
                                    }
                                }
                            }
                            Event::Todos { items } => {
                                todos.set(items);
                            }
                            Event::PatchApplied { path, .. } => {
                                timeline.write().push(TimelineItem { title: "✎ patched".into(), sub: path });
                                let v = *git_refresh.read();
                                git_refresh.set(v + 1); // trigger git-tab auto-refresh
                            }
                            Event::FileDiff { path, diff, checkpoint, .. } => {
                                let (adds, dels) = diff_counts(&diff);
                                turn_edits.write().push((path.clone(), adds, dels, checkpoint, diff.clone()));
                                messages.write().push(ChatMsg { author: Author::Diff(path, checkpoint), text: diff });
                            }
                            Event::HookFired { hook, command, blocked } => {
                                timeline.write().push(TimelineItem {
                                    title: format!("🪝 {hook}{}", if blocked { " · blocked" } else { "" }),
                                    sub: command,
                                });
                            }
                            Event::QuestionAsked { request_id, question, options } => {
                                questions.write().push((request_id, question, options));
                            }
                            Event::RateLimit { plan, primary_pct, secondary_pct, primary_reset_s, secondary_reset_s } => {
                                let p_rem = 100u8.saturating_sub(primary_pct);
                                let s_rem = 100u8.saturating_sub(secondary_pct);
                                // Format reset times as a local clock (5h) / date (weekly), like Codex.
                                let js = format!(
                                    "const P={primary_reset_s},S={secondary_reset_s};const p=new Date(Date.now()+P*1000),s=new Date(Date.now()+S*1000);const t=d=>d.toLocaleTimeString([],{{hour:'numeric',minute:'2-digit'}});const dd=d=>d.toLocaleDateString([],{{month:'short',day:'numeric'}});return JSON.stringify({{p:t(p),s:dd(s)}});"
                                );
                                let labels = dioxus::document::eval(&js).join::<String>().await.unwrap_or_default();
                                let lv: serde_json::Value = serde_json::from_str(&labels).unwrap_or(serde_json::Value::Null);
                                let pl = lv["p"].as_str().unwrap_or("").to_string();
                                let sl = lv["s"].as_str().unwrap_or("").to_string();
                                usage_info.set(Some((plan, p_rem, s_rem, pl, sl)));
                            }
                            Event::CheckpointCreated { id, label, .. } => {
                                checkpoints.write().push((id, label.clone()));
                                timeline.write().push(TimelineItem { title: format!("⎌ checkpoint #{id}"), sub: label });
                            }
                            Event::RewindDone { id, restored } => {
                                timeline.write().push(TimelineItem { title: format!("⎌ rewound to #{id}"), sub: format!("{restored} file(s) restored") });
                            }
                            Event::TokensUsed { input, output, .. } => {
                                usage.set((input, output));
                            }
                            Event::Compacted { dropped, tokens } => {
                                timeline.write().push(TimelineItem { title: "∿ context compacted".into(), sub: format!("dropped {dropped} · ~{tokens} tok") });
                            }
                            Event::TurnFinished { .. } => {
                                streaming.set(false);
                                status.set(String::new());
                                // Settle any activity still showing a spinner so none stays
                                // "running" stuck at the bottom after the turn ends.
                                {
                                    let mut m = messages.write();
                                    for c in m.iter_mut() {
                                        if let Author::Activity { running, .. } = &mut c.author { *running = false; }
                                    }
                                }
                                if let Some(start) = turn_start.write().take() {
                                    let secs = start.elapsed().as_secs();
                                    let dur = if secs >= 60 { format!("{}m {}s", secs / 60, secs % 60) } else { format!("{secs}s") };
                                    messages.write().push(ChatMsg { author: Author::Note, text: format!("✓ Done · {dur}") });
                                }
                                // Submit the next queued message as a fresh turn.
                                let next = { let mut q = queue.write(); if q.is_empty() { None } else { Some(q.remove(0)) } };
                                if let Some(text) = next {
                                    if let Some(h) = &handle {
                                        messages.write().push(ChatMsg { author: Author::User, text: text.clone() });
                                        messages.write().push(ChatMsg { author: Author::Agent, text: String::new() });
                                        streaming.set(true);
                                        let _ = h.submit(Op::UserTurn { text }).await;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    });

    let workspace = ui.workspace.read().clone();
    let project = project_name(&workspace);
    let accent_style = {
        let a = cfg.read().accent_color.clone();
        if a.trim().is_empty() { String::new() } else { format!("--accent: {a}; --on-accent: #ffffff;") }
    };

    // Keyboard: ⌘1–9 jump to tab N, ⌘⇧] / ⌘⇧[ cycle tabs.
    use_future(move || async move {
        let mut eval = dioxus::document::eval(
            r#"
            if (!window.__oxtabkeys) {
              window.__oxtabkeys = 1;
              document.addEventListener('keydown', function(e){
                if (!(e.metaKey || e.ctrlKey)) return;
                if (e.shiftKey && (e.key === ']' || e.code === 'BracketRight')) { e.preventDefault(); dioxus.send('next'); }
                else if (e.shiftKey && (e.key === '[' || e.code === 'BracketLeft')) { e.preventDefault(); dioxus.send('prev'); }
                else if (!e.shiftKey && e.key >= '1' && e.key <= '9') { e.preventDefault(); dioxus.send('jump:' + e.key); }
              }, true);
            }
            while (true) { await new Promise(r => setTimeout(r, 3600000)); }
            "#,
        );
        loop {
            let msg = match eval.recv::<String>().await { Ok(m) => m, Err(_) => break };
            let n = tabs.read().len();
            if n == 0 { continue; }
            let cur = *active_tab.read();
            let target = if msg == "next" { (cur + 1) % n }
                else if msg == "prev" { (cur + n - 1) % n }
                else if let Some(d) = msg.strip_prefix("jump:") { d.parse::<usize>().ok().map(|x| x.saturating_sub(1)).filter(|&x| x < n).unwrap_or(cur) }
                else { cur };
            if target != cur {
                switch_tab(tabs, active_tab, messages, cfg, engine, target);
            }
        }
    });
    // Element picker: previewed page posts the selected element up to here.
    use_future(move || async move {
        let mut eval = dioxus::document::eval(
            r#"
            if (!window.__oxpick) {
              window.__oxpick = 1;
              window.addEventListener('message', function(e){
                if (e.data && e.data.type === 'oxide-element') { try { dioxus.send(JSON.stringify(e.data)); } catch(_){} }
              });
            }
            while (true) { await new Promise(r => setTimeout(r, 3600000)); }
            "#,
        );
        loop {
            let raw = match eval.recv::<String>().await { Ok(m) => m, Err(_) => break };
            let v: serde_json::Value = match serde_json::from_str(&raw) { Ok(v) => v, Err(_) => continue };
            let sel = v["selector"].as_str().unwrap_or("");
            let src = v["source"].as_str().unwrap_or("");
            let comp = v["component"].as_str().unwrap_or("");
            let text = v["text"].as_str().unwrap_or("");
            let html = v["html"].as_str().unwrap_or("");
            let mut ctx = String::from("Selected UI element to change:\n");
            ctx.push_str(&format!("- selector: {sel}\n"));
            if !comp.is_empty() { ctx.push_str(&format!("- component: <{comp}>\n")); }
            if !src.is_empty() { ctx.push_str(&format!("- source: {src}\n")); }
            if !text.is_empty() { ctx.push_str(&format!("- text: {text}\n")); }
            if !html.is_empty() { ctx.push_str(&format!("- html: {html}\n")); }
            picked_element.set(Some(ctx));
        }
    });
    // Active TUI tab (embedded terminal) info.
    let (active_is_tui, active_bin, active_tab_id) = {
        let t = tabs.read();
        match t.get(*active_tab.read()) {
            Some(tab) if tab.mode == "tui" => (true, tab.bin.clone(), tab.id),
            _ => (false, String::new(), 0),
        }
    };
    let branch = git_branch(&workspace);
    let active_cfg = cfg.read().clone();
    let provider = active_cfg.provider.clone();
    let model = active_cfg.model.clone();
    let bypass = matches!(active_cfg.approval_policy, ApprovalPolicy::Never);
    let model_name = selected_model(&provider, &model)
        .map(|p| p.label.to_string())
        .unwrap_or_else(|| {
            if model.is_empty() {
                provider.clone()
            } else {
                model.clone()
            }
        });
    // Effort is shown by its own pill — keep the model label clean.
    let model_label = match *context_limit.read() {
        Some(limit) => format!("{model_name} · {}k", limit / 1000),
        None => model_name.clone(),
    };
    let ctx_used = usage.read().0;
    let ctx_limit = context_limit.read().unwrap_or(0);
    // Show the hero until a real conversation starts (ignore stray notes).
    let is_empty = !messages
        .read()
        .iter()
        .any(|m| matches!(m.author, Author::User | Author::Agent));
    let editor_open = ui.open_path.read().is_some();

    let suggestions = [
        "Give me a high-level tour of this repository",
        "Find a likely bug and explain it",
        "Add a unit test for the most important function",
    ];

    // Run a terminal command in the workspace.
    let mut run_term = move || {
        let cmd = term_input.read().trim().to_string();
        if cmd.is_empty() {
            return;
        }
        term_input.set(String::new());
        term_lines.write().push(format!("$ {cmd}"));
        let ws = ui.workspace.read().clone();
        spawn(async move {
            let out = tokio::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(&cmd)
                .current_dir(&ws)
                .output()
                .await;
            let text = match out {
                Ok(o) => {
                    let mut s = String::from_utf8_lossy(&o.stdout).to_string();
                    s.push_str(&String::from_utf8_lossy(&o.stderr));
                    if s.trim().is_empty() {
                        format!("(exit {})", o.status.code().unwrap_or(-1))
                    } else {
                        s
                    }
                }
                Err(e) => format!("error: {e}"),
            };
            term_lines.write().push(text);
        });
    };

    rsx! {
        style { {CSS} }
        style { {XTERM_CSS} }
        div { class: "app", "data-theme": "{cfg.read().theme}", "data-density": "{cfg.read().density}", style: "{accent_style}",
            // ── Sidebar ────────────────────────────────────────────────
            aside { class: "sidebar",
                oncontextmenu: move |e: dioxus::prelude::MouseEvent| { e.prevent_default(); let c = e.client_coordinates(); theme_menu_pos.set((c.x, c.y)); session_menu.set(None); show_theme_menu.set(true); },
                if *show_theme_menu.read() {
                    div { class: "menu-backdrop", onclick: move |_| show_theme_menu.set(false) }
                    div { class: "theme-menu", style: "left: {theme_menu_pos.read().0}px; top: {theme_menu_pos.read().1}px;",
                        button { class: "menu-item", onclick: {
                            let win = win.clone();
                            move |_| { let v = !*pinned.read(); pinned.set(v); win.set_always_on_top(v); show_theme_menu.set(false); }
                        },
                            Icon { name: "target" } span { class: "menu-name", "Pin window" }
                            if *pinned.read() { span { class: "menu-check", "✓" } }
                        }
                        button { class: "menu-item", onclick: {
                            let win = win.clone();
                            let ws = workspace.clone();
                            move |_| {
                                show_theme_menu.set(false);
                                let theme = cfg.read().theme.clone();
                                let (mode, bin, provider, model) = {
                                    let t = tabs.read();
                                    match t.get(*active_tab.read()) {
                                        Some(tab) => (tab.mode.clone(), tab.bin.clone(), tab.provider.clone(), tab.model.clone()),
                                        None => ("gui".to_string(), String::new(), cfg.read().provider.clone(), cfg.read().model.clone()),
                                    }
                                };
                                let initial = messages.read().clone();
                                let dom = VirtualDom::new_with_props(PipWindow, PipWindowProps { workspace: ws.clone(), mode, provider, model, bin, theme, initial });
                                use dioxus::desktop::tao::dpi::LogicalSize;
                                let w = WindowBuilder::new()
                                    .with_title("Oxide — chat")
                                    .with_inner_size(LogicalSize::new(410.0, 620.0))
                                    .with_always_on_top(true);
                                win.new_window(dom, DesktopConfig::new().with_window(w));
                            }
                        },
                            Icon { name: "plugins" } span { class: "menu-name", "Picture in Picture" }
                        }
                        div { class: "plus-divider" }
                        div { class: "menu-label", "Theme" }
                        button { class: "menu-item", onclick: move |_| { set_theme(cfg, "light"); show_theme_menu.set(false); },
                            Icon { name: "spark" } span { class: "menu-name", "Light" }
                            if cfg.read().theme == "light" { span { class: "menu-check", "✓" } }
                        }
                        button { class: "menu-item", onclick: move |_| { set_theme(cfg, "dark"); show_theme_menu.set(false); },
                            Icon { name: "target" } span { class: "menu-name", "Dark" }
                            if cfg.read().theme == "dark" { span { class: "menu-check", "✓" } }
                        }
                        button { class: "menu-item", onclick: move |_| { set_theme(cfg, "system"); show_theme_menu.set(false); },
                            Icon { name: "settings" } span { class: "menu-name", "System" }
                            if cfg.read().theme == "system" { span { class: "menu-check", "✓" } }
                        }
                        div { class: "plus-divider" }
                        div { class: "menu-label", "Accent" }
                        div { class: "accent-swatches",
                            for c in ["", "#e0913a", "#7c91ff", "#3ad29f", "#e05d5d", "#c678dd", "#56b6c2"] {
                                {
                                    let c = c.to_string();
                                    let active = cfg.read().accent_color == c;
                                    rsx! {
                                        button {
                                            class: if active { "swatch active" } else { "swatch" },
                                            style: if c.is_empty() { "background: var(--surface-hi)".to_string() } else { format!("background: {c}") },
                                            title: if c.is_empty() { "Default" } else { "{c}" },
                                            onclick: move |_| { set_accent(cfg, &c); show_theme_menu.set(false); },
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                div { class: "brand",
                    img { class: "logo", src: logo_uri() }
                    span { class: "brand-name", "Oxide" }
                }
                nav { class: "nav",
                    button { class: "nav-item", onclick: move |_| {
                            // Reset to a fresh chat: clear transcript, close panels, reset the engine session.
                            show_board.set(false);
                            let mut op = ui.open_path; op.set(None);
                            messages.write().clear();
                            thinking.set(String::new());
                            status.set(String::new());
                            let cur = *active_tab.read();
                            if let Some(t) = tabs.write().get_mut(cur) { t.messages.clear(); t.title = provider_title(&t.provider).to_string(); }
                            let _ = engine.send(EngineCmd::Reconfigure(cfg.read().clone()));
                        },
                        Icon { name: "edit" } span { "New chat" }
                    }
                    button { class: "nav-item", onclick: move |_| { show_palette.set(true); palette_query.set(String::new()); palette_sel.set(0); },
                        Icon { name: "search" } span { "Search" }
                    }
                    button { class: "nav-item", onclick: move |_| show_mcp.set(true),
                        if let Some(l) = provider_logo("mcp") { span { class: "nav-logo", dangerous_inner_html: l } } else { Icon { name: "plugins" } }
                        span { "MCP" }
                    }
                    button { class: "nav-item", onclick: move |_| show_skills.set(true),
                        Icon { name: "target" } span { "Skills" }
                    }
                    button { class: if *show_board.read() { "nav-item on" } else { "nav-item" }, onclick: move |_| { let v = *show_board.read(); show_board.set(!v); },
                        Icon { name: "list" } span { "Board" }
                    }
                }
                div { class: "section-row",
                    span { class: "section-label", "Projects" }
                    button { class: "section-add", title: "Open folder", onclick: move |_| open_folder(cfg, ui, engine),
                        Icon { name: "plus" }
                    }
                }
                div { class: "projects",
                    if cfg.read().workspace.is_some() {
                        {
                            let pins: Vec<(PathBuf, String)> = cfg.read().pinned_sessions.iter()
                                .map(PathBuf::from)
                                .filter(|p| p.exists())
                                .map(|p| { let title = session_title(&p); (p, title) })
                                .collect();
                            if pins.is_empty() { rsx!{} } else {
                                rsx! {
                                    div { class: "section-label", "Pinned" }
                                    for (p, title) in pins {
                                        {
                                            let p_open = p.clone();
                                            let t_open = title.clone();
                                            let p_str = p.display().to_string();
                                            rsx! {
                                                div { class: "thread-anchor",
                                                    div { class: "row-actions",
                                                        button { class: "row-act-btn pinned", title: "Unpin", onclick: move |e: dioxus::prelude::MouseEvent| { e.stop_propagation(); toggle_pin(cfg, &p_str); }, "📌" }
                                                    }
                                                    div { class: "thread recent",
                                                        onclick: move |_| { show_board.set(false); open_session_tab(tabs, active_tab, messages, next_tab_id, cfg, p_open.clone(), t_open.clone()); },
                                                        span { class: "thread-title", title: "{title}", "{title}" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    div { class: "section-label", "Projects" }
                                }
                            }
                        }
                        for (pws, pname, sessions) in projects_list.read().clone() {
                            {
                                let is_current = pws == workspace;
                                let pname2 = pname.clone();
                                let expanded = expanded_projects.read().contains(&pname);
                                let shown = if expanded { sessions.len() } else { sessions.len().min(5) };
                                let total = sessions.len();
                                let ws_rebuild = workspace.clone();
                                let pws_switch = pws.clone();
                                rsx! {
                                    div { class: if is_current { "project current" } else { "project" },
                                        title: if is_current { "" } else { "Switch to this project" },
                                        onclick: move |_| { if !is_current { apply_workspace(cfg, ui, engine, pws_switch.clone()); } },
                                        Icon { name: "folder" }
                                        span { class: "project-name", "{pname}" }
                                        if is_current && *streaming.read() { span { class: "syn-spinner", style: "margin-left:6px" } }
                                        button { class: "project-add", title: "New chat here", onclick: move |e: dioxus::prelude::MouseEvent| {
                                                e.stop_propagation();
                                                show_board.set(false);
                                                if !is_current { apply_workspace(cfg, ui, engine, pws.clone()); }
                                                let mut op = ui.open_path; op.set(None);
                                                let prov = cfg.read().provider.clone();
                                                let model = cfg.read().model.clone();
                                                let title = provider_title(&prov).to_string();
                                                new_agent_tab(tabs, active_tab, messages, cfg, engine, next_tab_id, &prov, &model, &title);
                                            },
                                            Icon { name: "plus" }
                                        }
                                    }
                                    if is_current {
                                        for (i, t) in tabs.read().iter().enumerate() {
                                            {
                                                let i = i;
                                                let id = t.id;
                                                let ttl = if t.title.is_empty() { "New chat".to_string() } else { t.title.clone() };
                                                let is_active = i == *active_tab.read();
                                                let busy = is_active && *streaming.read();
                                                let prov = t.provider.clone();
                                                let logo = provider_logo(&prov);
                                                let editing = *renaming_tab.read() == Some(id);
                                                let ttl_dc = ttl.clone();
                                                rsx! {
                                                    div { key: "tab{id}", class: if is_active { "thread active" } else { "thread" },
                                                        onclick: move |_| { show_board.set(false); switch_tab(tabs, active_tab, messages, cfg, engine, i); },
                                                        ondoubleclick: move |_| { rename_text.set(ttl_dc.clone()); renaming_tab.set(Some(id)); },
                                                        if busy { span { class: "syn-spinner" } }
                                                        else if let Some(l) = logo { span { class: "tab-prov", dangerous_inner_html: l } }
                                                        if editing {
                                                            input { class: "rename-input", value: "{rename_text}", autofocus: true,
                                                                oninput: move |e| rename_text.set(e.value()),
                                                                onkeydown: move |e| {
                                                                    if e.key() == Key::Enter { e.prevent_default(); let n = rename_text.read().trim().to_string(); if !n.is_empty() { if let Some(t) = tabs.write().get_mut(i) { t.title = n; } } renaming_tab.set(None); }
                                                                    else if e.key() == Key::Escape { renaming_tab.set(None); }
                                                                },
                                                                onblur: move |_| { let n = rename_text.read().trim().to_string(); if !n.is_empty() { if let Some(t) = tabs.write().get_mut(i) { t.title = n; } } renaming_tab.set(None); },
                                                                onclick: move |e| e.stop_propagation(),
                                                            }
                                                        } else {
                                                            span { class: "thread-title", title: "{ttl}", "{ttl}" }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    for (path, title, reltime) in sessions.iter().take(shown).cloned() {
                                        {
                                            let p_open = path.clone();
                                            let p_dbl = path.clone();
                                            let p_del = path.clone();
                                            let p_arch = path.clone();
                                            let p_del2 = path.clone();
                                            let p_arch2 = path.clone();
                                            let t_open = title.clone();
                                            let menu_open = session_menu.read().as_ref() == Some(&path);
                                            let ws_d = ws_rebuild.clone();
                                            let ws_ar = ws_rebuild.clone();
                                            let ws_d2 = ws_rebuild.clone();
                                            let ws_ar2 = ws_rebuild.clone();
                                            let path_str = path.display().to_string();
                                            let is_pinned = cfg.read().pinned_sessions.iter().any(|p| p == &path_str);
                                            rsx! {
                                                div { class: "thread-anchor",
                                                    div { class: "row-actions",
                                                        button { class: if is_pinned { "row-act-btn pinned" } else { "row-act-btn" }, title: if is_pinned { "Unpin" } else { "Pin" },
                                                            onclick: move |e: dioxus::prelude::MouseEvent| { e.stop_propagation(); toggle_pin(cfg, &path_str); }, "📌" }
                                                        button { class: "row-act-btn", title: "Archive", onclick: move |e: dioxus::prelude::MouseEvent| { e.stop_propagation(); archive_session(&p_arch2); projects_list.set(build_projects(&ws_ar2, &cfg.read().recent_workspaces)); }, "⊟" }
                                                        button { class: "row-act-btn danger", title: "Delete", onclick: move |e: dioxus::prelude::MouseEvent| { e.stop_propagation(); delete_session(&p_del2); projects_list.set(build_projects(&ws_d2, &cfg.read().recent_workspaces)); }, "✕" }
                                                    }
                                                    div { class: "thread recent", title: "right-click / double-click for options",
                                                        onclick: move |_| { show_board.set(false); open_session_tab(tabs, active_tab, messages, next_tab_id, cfg, p_open.clone(), t_open.clone()); },
                                                        oncontextmenu: {
                                                            let p = p_dbl.clone();
                                                            move |e: dioxus::prelude::MouseEvent| { e.prevent_default(); e.stop_propagation(); show_theme_menu.set(false); session_menu.set(Some(p.clone())); }
                                                        },
                                                        ondoubleclick: move |_| { let cur = session_menu.read().clone(); session_menu.set(if cur.as_ref() == Some(&p_dbl) { None } else { Some(p_dbl.clone()) }); },
                                                        span { class: "thread-title", title: "{title}", "{title}" }
                                                        span { class: "thread-time", "{reltime}" }
                                                    }
                                                    if menu_open {
                                                        div { class: "menu-backdrop", onclick: move |_| session_menu.set(None) }
                                                        div { class: "thread-menu",
                                                            button { class: "menu-item", onclick: move |_| { archive_session(&p_arch); session_menu.set(None); projects_list.set(build_projects(&ws_ar, &cfg.read().recent_workspaces)); },
                                                                Icon { name: "folder" } span { class: "menu-name", "Archive" }
                                                            }
                                                            button { class: "menu-item danger", onclick: move |_| { delete_session(&p_del); session_menu.set(None); projects_list.set(build_projects(&ws_d, &cfg.read().recent_workspaces)); },
                                                                Icon { name: "trash" } span { class: "menu-name", "Delete" }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    if total > 5 {
                                        button { class: "show-more", onclick: move |_| {
                                            let mut e = expanded_projects.write();
                                            if e.contains(&pname2) { e.remove(&pname2); } else { e.insert(pname2.clone()); }
                                        }, if expanded { "Show less" } else { "Show more" } }
                                    }
                                }
                            }
                        }
                    } else {
                        button { class: "open-codebase", onclick: move |_| open_folder(cfg, ui, engine),
                            Icon { name: "folder" } span { "Open codebase" }
                        }
                    }
                }
                if let Some((plan, p, s, p_reset, s_reset)) = usage_info.read().clone() {
                    div { class: "usage-chip", title: "ChatGPT subscription — shared with Codex",
                        div { class: "usage-head", "Usage remaining" }
                        div { class: "usage-row",
                            span { class: "usage-k", "5h" }
                            span { class: "usage-bar", span { class: "usage-fill", style: "width:{p}%" } }
                            span { class: "usage-v", "{p}% · {p_reset}" }
                        }
                        div { class: "usage-row",
                            span { class: "usage-k", "wk" }
                            span { class: "usage-bar", span { class: "usage-fill", style: "width:{s}%" } }
                            span { class: "usage-v", "{s}% · {s_reset}" }
                        }
                        div { class: "usage-plan", "ChatGPT {plan}" }
                    }
                }
                button { class: "settings-btn", onclick: move |_| show_settings.set(true),
                    Icon { name: "settings" } span { "Settings" }
                }
            }

            // ── Center column ──────────────────────────────────────────
            main { class: "main",
                if let Some(info) = update_info.read().clone() {
                    {
                    let pct = (*update_pct.read() * 100.0) as u32;
                    rsx! {
                    div { class: "update-banner",
                        span { class: "update-text",
                            "⬆ Update available · v{info.version}"
                        }
                        if *updating.read() {
                            div { class: "update-progress",
                                div { class: "update-bar", style: "width: {pct}%" }
                            }
                        }
                        div { class: "update-actions",
                            button { class: "update-btn", disabled: *updating.read(),
                                onclick: move |_| {
                                    updating.set(true);
                                    update_pct.set(0.0);
                                    let info = info.clone();
                                    spawn(async move {
                                        match update::apply(&info, move |p| { let mut up = update_pct; up.set(p); }).await {
                                            Ok(()) => update::restart(),
                                            Err(_) => updating.set(false),
                                        }
                                    });
                                },
                                if *updating.read() { "Updating… {pct}%" } else { "Update & restart" }
                            }
                            button { class: "update-x", onclick: move |_| update_info.set(None), "✕" }
                        }
                    }
                    }
                    }
                }
                if cfg.read().workspace.is_some() {
                    div { class: "agent-tabs",
                        for (i, t) in tabs.read().iter().enumerate() {
                            {
                                let i = i;
                                let id = t.id;
                                let title = t.title.clone();
                                let logo = provider_logo(&t.provider);
                                let is_active = i == *active_tab.read();
                                let many = tabs.read().len() > 1;
                                rsx! {
                                    div { key: "{id}", class: if is_active { "agent-tab active" } else { "agent-tab" },
                                        onclick: move |_| switch_tab(tabs, active_tab, messages, cfg, engine, i),
                                        if let Some(l) = logo { span { class: "agent-tab-logo prov-logo", dangerous_inner_html: l } }
                                        span { class: "agent-tab-title", "{title}" }
                                        if many {
                                            button { class: "agent-tab-x", onclick: move |e| { e.stop_propagation(); close_tab(tabs, active_tab, messages, cfg, engine, i); }, "✕" }
                                        }
                                    }
                                }
                            }
                        }
                        div { class: "newtab-anchor",
                            button { class: "agent-tab-add", onclick: move |_| { let v = *show_newtab.read(); show_newtab.set(!v); },
                                Icon { name: "plus" } span { class: "chev", Icon { name: "chevron" } }
                            }
                            if *show_newtab.read() {
                                div { class: "menu-backdrop", onclick: move |_| show_newtab.set(false) }
                                div { class: "newtab-menu",
                                    div { class: "menu-label", "New agent · ⌘-click for TUI" }
                                    button { class: "menu-item", onclick: move |e| {
                                            show_newtab.set(false);
                                            let tui = e.modifiers().meta() || cfg.read().default_tab_mode == "tui";
                                            if tui { new_tui_tab(tabs, active_tab, messages, next_tab_id, "codex", "Codex"); }
                                            else { new_agent_tab(tabs, active_tab, messages, cfg, engine, next_tab_id, "codex", "", "Codex"); }
                                        },
                                        if let Some(l) = provider_logo("codex") { span { class: "agent-tab-logo prov-logo", dangerous_inner_html: l } }
                                        span { class: "menu-name", "Codex" }
                                    }
                                    button { class: "menu-item", onclick: move |e| {
                                            show_newtab.set(false);
                                            let tui = e.modifiers().meta() || cfg.read().default_tab_mode == "tui";
                                            if tui { new_tui_tab(tabs, active_tab, messages, next_tab_id, "claude", "Claude Code"); }
                                            else { new_agent_tab(tabs, active_tab, messages, cfg, engine, next_tab_id, "claude", "", "Claude Code"); }
                                        },
                                        if let Some(l) = provider_logo("claude") { span { class: "agent-tab-logo prov-logo", dangerous_inner_html: l } }
                                        span { class: "menu-name", "Claude Code" }
                                    }
                                }
                            }
                        }
                        div { class: "tab-actions",
                            button { class: "top-btn", onclick: move |_| open_folder(cfg, ui, engine),
                                Icon { name: "folder" } "Open folder"
                            }
                            button { class: if *show_files.read() { "top-btn on" } else { "top-btn" },
                                onclick: move |_| { let v = *show_files.read(); show_files.set(!v); }, Icon { name: "plugins" } "Files"
                            }
                            button { class: if *show_terminal.read() { "top-btn on" } else { "top-btn" },
                                onclick: move |_| { let v = *show_terminal.read(); show_terminal.set(!v); }, Icon { name: "terminal" } "Terminal"
                            }
                            button { class: if *show_split.read() { "top-btn on" } else { "top-btn" },
                                onclick: move |_| { let v = *show_split.read(); show_split.set(!v); }, Icon { name: "plugins" } "Split"
                            }
                            button { class: if *show_preview.read() { "top-btn on" } else { "top-btn" },
                                onclick: move |_| {
                                    let v = *show_preview.read(); show_preview.set(!v);
                                    if !v { spawn(async move { preview_ports.set(scan_ports().await); }); }
                                }, Icon { name: "browser" } "Preview"
                            }
                        }
                    }
                }

                div { class: if *show_preview.read() { "center with-preview" } else { "center" },
                    if *show_preview.read() {
                        div { class: "preview-panel",
                            div { class: "preview-bar",
                                input { class: "preview-addr", placeholder: "http://localhost:3000", value: "{preview_url}",
                                    oninput: move |e| preview_url.set(e.value()),
                                    onkeydown: move |e| if e.key() == Key::Enter {
                                        let mut u = preview_url.read().clone();
                                        if !u.is_empty() && !u.contains("://") { u = format!("http://{u}"); preview_url.set(u); }
                                    }
                                }
                                button { class: "preview-btn", title: "Rescan localhost ports", onclick: move |_| { spawn(async move { preview_ports.set(scan_ports().await); }); }, "⟳ Scan" }
                                button { class: "preview-btn pick", title: "Select an element to send to the composer", onclick: move |_| {
                                    spawn(async move { let _ = document::eval("document.querySelector('.preview-frame')?.contentWindow?.postMessage('oxide-pick-on','*')").await; });
                                }, "📍 Pick" }
                                button { class: "preview-btn", title: "Reload", onclick: move |_| { let u = preview_url.read().clone(); preview_url.set(String::new()); preview_url.set(u); }, "Reload" }
                                button { class: "preview-btn", title: "Open in system browser", onclick: move |_| { let u = preview_url.read().clone(); if !u.is_empty() { let _ = std::process::Command::new("open").arg(u).spawn(); } }, "↗" }
                                button { class: "term-x", onclick: move |_| show_preview.set(false), "✕" }
                            }
                            div { class: "preview-ports",
                                if preview_ports.read().is_empty() {
                                    span { class: "preview-hint", "No localhost servers detected. Start a dev server, then ⟳ Scan." }
                                }
                                for (port, cmd) in preview_ports.read().iter().cloned() {
                                    button { class: "port-chip", title: "{cmd}", onclick: move |_| {
                                        spawn(async move {
                                            preview_proxy::set_target(port);
                                            let pp = preview_proxy::ensure_proxy().await;
                                            if pp != 0 { preview_url.set(format!("http://127.0.0.1:{pp}/")); }
                                            else { preview_url.set(format!("http://localhost:{port}")); }
                                        });
                                    },
                                        span { class: "port-dot" } "localhost:{port}" span { class: "port-cmd", "{cmd}" }
                                    }
                                }
                            }
                            if preview_url.read().is_empty() {
                                div { class: "preview-empty", "Pick a detected server above, or type a URL. Build + run + see it without leaving Oxide." }
                            } else {
                                iframe { class: "preview-frame", src: "{preview_url}" }
                            }
                        }
                    }
                    if *show_split.read() && cfg.read().workspace.is_some() {
                        SplitView {
                            node: split_layout.read().clone(),
                            workspace: workspace.clone(),
                            panes: split_panes,
                            layout: split_layout,
                            next_id: split_next_id,
                            drag: split_drag,
                            rects: split_rects,
                            def_provider: cfg.read().provider.clone(),
                            def_model: cfg.read().model.clone(),
                        }
                    } else if *show_board.read() && cfg.read().workspace.is_some() {
                        div { class: "board",
                            div { class: "board-head",
                                h2 { "Board" }
                                div { class: "board-actions",
                                    input { class: "board-input", placeholder: "New task…", value: "{new_card_title}",
                                        oninput: move |e| new_card_title.set(e.value()),
                                        onkeydown: move |e| {
                                            if e.key() == Key::Enter {
                                                let t = new_card_title.read().trim().to_string();
                                                if !t.is_empty() {
                                                    board.write().add(t, String::new());
                                                    new_card_title.set(String::new());
                                                    let snap = board.read().clone(); snap.save(&workspace_of(&cfg.read()));
                                                }
                                            }
                                        }
                                    }
                                    button { class: "board-btn", onclick: move |_| { let _ = workspace_of(&cfg.read()); run_board(board, cfg, workspace_of(&cfg.read())); }, "▶ Run To-Do" }
                                    button { class: "board-btn ghost", onclick: move |_| {
                                            let root = workspace_of(&cfg.read());
                                            spawn(async move {
                                                let issues = board::import_github_issues(&root).await;
                                                let mut b = board.write();
                                                for (t, d) in issues { b.add(t, d); }
                                                let snap = b.clone(); drop(b); snap.save(&root);
                                            });
                                        }, "↓ GitHub issues" }
                                }
                            }
                            div { class: "board-cols four",
                                for (col, label) in [(board::TODO, "To Do"), (board::DOING, "In Progress"), (board::REVIEW, "Review"), (board::DONE, "Done")] {
                                    div { class: "board-col",
                                        div { class: "board-col-head", "{label}" }
                                        for card in board.read().cards.iter().filter(|c| c.column == col).cloned() {
                                            {
                                                let cid = card.id;
                                                let cbranch = card.branch.clone();
                                                rsx! {
                                                    div { class: if col == board::DOING { "board-card doing" } else { "board-card" },
                                                        div { class: "board-card-title", "{card.title}" }
                                                        if !card.result.is_empty() { div { class: "board-card-result", "{card.result}" } }
                                                        if !card.branch.is_empty() { div { class: "board-card-branch", "{card.branch}" } }
                                                        if col == board::REVIEW && !cbranch.is_empty() {
                                                            button { class: "board-merge", onclick: move |_| {
                                                                let root = workspace_of(&cfg.read());
                                                                let branch = cbranch.clone();
                                                                spawn(async move {
                                                                    let (ok, msg) = board::merge_branch(&root, &branch).await;
                                                                    let snap = {
                                                                        let mut b = board.write();
                                                                        if let Some(c) = b.cards.iter_mut().find(|c| c.id == cid) {
                                                                            if ok { c.column = board::DONE.to_string(); }
                                                                            c.result = format!("{}\n\n[merge] {msg}", c.result);
                                                                        }
                                                                        b.clone()
                                                                    };
                                                                    snap.save(&root);
                                                                });
                                                            }, "✓ Merge" }
                                                        }
                                                        button { class: "board-card-x", onclick: move |_| {
                                                            board.write().cards.retain(|c| c.id != cid);
                                                            let snap = board.read().clone(); snap.save(&workspace_of(&cfg.read()));
                                                        }, "✕" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else if active_is_tui {
                        TerminalView { key: "{active_tab_id}", id: active_tab_id, bin: active_bin.clone(), ws: workspace.display().to_string() }
                    } else if cfg.read().workspace.is_none() {
                        div { class: "hero welcome-screen",
                            img { class: "welcome-logo", src: logo_uri() }
                            h1 { class: "hero-title", "Welcome to Oxide" }
                            p { class: "welcome-sub", "Open a project folder to get started." }
                            button { class: "welcome-btn", onclick: move |_| open_folder(cfg, ui, engine), "Open folder" }
                            if !cfg.read().recent_workspaces.is_empty() {
                                div { class: "welcome-recent",
                                    div { class: "menu-label", "Recent" }
                                    for p in cfg.read().recent_workspaces.clone() {
                                        {
                                            let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| p.display().to_string());
                                            rsx! {
                                                button { class: "welcome-recent-item", onclick: move |_| apply_workspace(cfg, ui, engine, p.clone()),
                                                    Icon { name: "folder" } span { "{name}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else if editor_open {
                        Editor {}
                    } else if is_empty {
                        div { class: "hero",
                            h1 { class: "hero-title", "What should we build in {project}?" }
                            Composer { input, streaming, engine, cfg, model_label: model_label.clone(),
                                       bypass, project: project.clone(), branch: branch.clone(),
                                       context_used: ctx_used, context_limit: ctx_limit,
                                       workspace: workspace.clone(), plan_mode, pursue_goal, goal_text, mentions, queue, picked_element,
                                       on_settings: move |_| show_settings.set(true),
                                       on_open_folder: move |_| open_folder(cfg, ui, engine), on_pick_workspace: move |dir| apply_workspace(cfg, ui, engine, dir) }
                            div { class: "suggestions",
                                for s in suggestions.iter() {
                                    button { class: "suggestion",
                                        onclick: {
                                            let p = s.to_string();
                                            move |_| { let _ = engine.send(EngineCmd::Submit { engine: p.clone(), display: p.clone() }); }
                                        },
                                        Icon { name: "spark" } span { "{s}" }
                                    }
                                }
                            }
                        }
                    } else {
                        div { class: "scroll",
                            div { class: "col",
                                {
                                    // Group consecutive tool-activity rows so they collapse into one dropdown.
                                    let groups = {
                                        let msgs = messages.read();
                                        let mut g: Vec<(bool, Vec<usize>)> = Vec::new();
                                        for (i, m) in msgs.iter().enumerate() {
                                            if matches!(m.author, Author::Activity { .. }) {
                                                match g.last_mut() {
                                                    Some(last) if last.0 => last.1.push(i),
                                                    _ => g.push((true, vec![i])),
                                                }
                                            } else {
                                                g.push((false, vec![i]));
                                            }
                                        }
                                        g
                                    };
                                    rsx! {
                                        for (is_act, idxs) in groups.into_iter() {
                                            if is_act && idxs.len() > 2 {
                                                {
                                                    let rows: Vec<(String, bool, bool)> = idxs.iter().map(|&i| {
                                                        let m = &messages.read()[i];
                                                        if let Author::Activity { running, ok } = m.author { (m.text.clone(), running, ok) } else { (m.text.clone(), false, true) }
                                                    }).collect();
                                                    let running = rows.iter().any(|r| r.1);
                                                    let n = rows.len();
                                                    let done = rows.iter().filter(|r| !r.1).count();
                                                    let label = if running { format!("⚙ Working… {done}/{n}") } else { format!("⚙ {n} actions") };
                                                    let key = idxs[0];
                                                    let is_open = act_open.read().get(&key).copied().unwrap_or(running);
                                                    rsx! {
                                                        details { class: "act-group", open: is_open,
                                                            summary { class: "act-group-head",
                                                                onclick: move |e: dioxus::prelude::MouseEvent| {
                                                                    e.prevent_default();
                                                                    let cur = act_open.read().get(&key).copied().unwrap_or(running);
                                                                    act_open.write().insert(key, !cur);
                                                                },
                                                                span { class: "diff-caret", Icon { name: "chevron" } }
                                                                "{label}"
                                                            }
                                                            for (t, r, o) in rows { ActivityRow { text: t, running: r, ok: o } }
                                                        }
                                                    }
                                                }
                                            } else {
                                                for i in idxs {
                                                    {
                                                        let m = messages.read()[i].clone();
                                                        match &m.author {
                                                            Author::Diff(path, cp) => {
                                                                let path = path.clone();
                                                                let cp = *cp;
                                                                let diff = m.text.clone();
                                                                let (adds, dels) = diff_counts(&diff);
                                                                let is_reverted = reverted.read().contains(&cp);
                                                                rsx! {
                                                                    div { class: "row diffrow",
                                                                        details { class: "diff-card",
                                                                            summary { class: "diff-head",
                                                                                span { class: "diff-caret", Icon { name: "chevron" } }
                                                                                span { class: "diff-path", "{path}" }
                                                                                span { class: "diff-adds", "+{adds}" }
                                                                                span { class: "diff-dels", "−{dels}" }
                                                                                if is_reverted {
                                                                                    span { class: "diff-reverted", "✓ Reverted" }
                                                                                } else {
                                                                                    button { class: "diff-revert", onclick: move |e| { e.prevent_default(); let _ = engine.send(EngineCmd::Rewind { id: cp }); reverted.write().insert(cp); }, "Revert" }
                                                                                }
                                                                            }
                                                                            HunkedDiff { ws: workspace.clone(), path: path.clone(), diff }
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                            Author::User => {
                                                                let segs = user_segments(&m.text);
                                                                let copy = serde_json::to_string(&strip_scaffold(&m.text)).unwrap_or_default();
                                                                let idx = i;
                                                                rsx! {
                                                                    div { class: "row user",
                                                                        div { class: "bubble",
                                                                            for (is_m, s) in segs {
                                                                                if is_m { span { class: "inline-chip", "{s}" } } else { "{s}" }
                                                                            }
                                                                        }
                                                                        div { class: "msg-actions",
                                                                            button { class: "msg-act", title: "Copy message", onclick: move |_| { let c = copy.clone(); spawn(async move { let _ = document::eval(&format!("navigator.clipboard.writeText({c})")).await; }); }, "⧉" }
                                                                            button { class: "msg-act", title: "Restore checkpoint — revert files and chat back to here", onclick: move |_| {
                                                                                let floor = { let ms = messages.read(); ms.iter().skip(idx + 1).find_map(|mm| if let Author::Diff(_, cp) = mm.author { Some(cp) } else { None }) };
                                                                                if let Some(fl) = floor {
                                                                                    let ids: Vec<u64> = checkpoints.read().iter().map(|(id, _)| *id).filter(|id| *id >= fl).collect();
                                                                                    for id in ids.into_iter().rev() { let _ = engine.send(EngineCmd::Rewind { id }); reverted.write().insert(id); }
                                                                                }
                                                                                messages.write().truncate(idx + 1);
                                                                            }, "↩" }
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                            _ => rsx! { Message { author: m.author.clone(), text: m.text.clone() } }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                if !thinking.read().is_empty() {
                                    details { class: "thinking-box", open: *streaming.read(),
                                        summary { class: "thinking-sum", "💭 Thinking" }
                                        div { class: "thinking-body", "{thinking}" }
                                    }
                                }
                                if *streaming.read() && !status.read().is_empty() {
                                    div { class: "status-pill",
                                        span { class: "status-spinner" }
                                        span { class: "status-shimmer", "{status}" }
                                    }
                                }
                                if !queue.read().is_empty() {
                                    div { class: "queue-bar",
                                        span { class: "queue-label", "⧖ Queued ({queue.read().len()})" }
                                        for (qi, q) in queue.read().iter().enumerate() {
                                            {
                                                let qi = qi;
                                                let preview: String = q.lines().last().unwrap_or("").chars().take(48).collect();
                                                rsx! {
                                                    span { class: "queue-chip",
                                                        "{preview}"
                                                        button { class: "queue-x", onclick: move |_| { queue.write().remove(qi); }, "✕" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                for (qid, question, options) in questions.read().iter().cloned() {
                                    div { class: "question-card",
                                        div { class: "question-q", "❓ {question}" }
                                        div { class: "question-opts",
                                            for (oi, opt) in options.iter().enumerate() {
                                                {
                                                    let qid = qid;
                                                    let opt = opt.clone();
                                                    rsx! {
                                                        button { class: "question-opt", onclick: move |_| { let _ = engine.send(EngineCmd::Answer { id: qid, text: opt.clone() }); q_answer.set(String::new()); },
                                                            span { class: "question-num", "{oi + 1}" } "{opt}"
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        div { class: "question-free",
                                            input { class: "question-input", placeholder: "Or type your answer…", value: "{q_answer}",
                                                oninput: move |e| q_answer.set(e.value()),
                                                onkeydown: move |e| {
                                                    if e.key() == Key::Enter {
                                                        let a = q_answer.read().trim().to_string();
                                                        if !a.is_empty() { let _ = engine.send(EngineCmd::Answer { id: qid, text: a }); q_answer.set(String::new()); }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                for (id, tool, summary) in approvals.read().iter().cloned() {
                                    div { class: "approval-card",
                                        div { class: "approval-q", "Allow " span { class: "approval-tool", "{tool}" } "?" }
                                        if !summary.is_empty() { div { class: "approval-sum", "{summary}" } }
                                        div { class: "approval-actions",
                                            button { class: "approval-yes", onclick: move |_| { let _ = engine.send(EngineCmd::Approve { id, decision: ApprovalDecision::Approve }); }, "Approve" }
                                            button { class: "approval-always", onclick: move |_| { let _ = engine.send(EngineCmd::Approve { id, decision: ApprovalDecision::ApproveForSession }); }, "Always" }
                                            button { class: "approval-no", onclick: move |_| { let _ = engine.send(EngineCmd::Approve { id, decision: ApprovalDecision::Reject }); }, "Reject" }
                                        }
                                    }
                                }
                                if !turn_edits.read().is_empty() {
                                    {
                                        let edits = turn_edits.read().clone();
                                        let n = edits.len();
                                        let total_add: u32 = edits.iter().map(|e| e.1).sum();
                                        let total_del: u32 = edits.iter().map(|e| e.2).sum();
                                        let expanded = *edits_expanded.read();
                                        let shown = if expanded { n } else { n.min(3) };
                                        let plural = if n == 1 { "" } else { "s" };
                                        let more_txt = if expanded { "Show less".to_string() } else { format!("Show {} more files", n.saturating_sub(3)) };
                                        rsx! {
                                            div { class: "edits-card",
                                                div { class: "edits-head",
                                                    span { class: "edits-ic", Icon { name: "list" } }
                                                    div { class: "edits-title-col",
                                                        span { class: "edits-title", "Edited {n} file{plural}" }
                                                        span { class: "edits-counts", span { class: "diff-adds", "+{total_add}" } " " span { class: "diff-dels", "−{total_del}" } }
                                                    }
                                                    button { class: "edits-undo", onclick: move |_| {
                                                        for (_, _, _, cp, _) in turn_edits.read().iter() { let _ = engine.send(EngineCmd::Rewind { id: *cp }); reverted.write().insert(*cp); }
                                                    }, "Undo ↺" }
                                                }
                                                for (path, a, d, _cp, diff) in edits.iter().take(shown).cloned() {
                                                    details { class: "edits-row-d",
                                                        summary { class: "edits-row",
                                                            span { class: "edits-caret", Icon { name: "chevron" } }
                                                            span { class: "edits-path", "{path}" }
                                                            span { class: "edits-rowcounts", span { class: "diff-adds", "+{a}" } " " span { class: "diff-dels", "−{d}" } }
                                                        }
                                                        HunkedDiff { ws: workspace.clone(), path: path.clone(), diff }
                                                    }
                                                }
                                                if n > 3 {
                                                    button { class: "edits-more", onclick: move |_| { let v = *edits_expanded.read(); edits_expanded.set(!v); },
                                                        "{more_txt}"
                                                        Icon { name: "chevron" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if !todos.read().is_empty() {
                            {
                                let items = todos.read().clone();
                                let done = items.iter().filter(|(_, s)| s == "completed").count();
                                let n = items.len();
                                rsx! {
                                    div { class: "todo-card",
                                        div { class: "todo-head", span { class: "todo-ic", Icon { name: "list" } } "Tasks {done}/{n}" }
                                        for (content, st) in items {
                                            div { class: "todo-row {st}",
                                                span { class: "todo-box",
                                                    if st == "completed" { "✓" } else if st == "in_progress" { span { class: "syn-spinner" } } else { "" }
                                                }
                                                span { class: "todo-text", "{content}" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        div { class: "composer-dock",
                            Composer { input, streaming, engine, cfg, model_label, bypass,
                                       project: project.clone(), branch: branch.clone(),
                                       context_used: ctx_used, context_limit: ctx_limit,
                                       workspace: workspace.clone(), plan_mode, pursue_goal, goal_text, mentions, queue, picked_element,
                                       on_settings: move |_| show_settings.set(true),
                                       on_open_folder: move |_| open_folder(cfg, ui, engine), on_pick_workspace: move |dir| apply_workspace(cfg, ui, engine, dir) }
                        }
                    }
                }

                // Terminal dock
                if *show_terminal.read() {
                    div { class: "terminal",
                        div { class: "term-head",
                            span { "TERMINAL · {project}" }
                            div { class: "term-head-actions",
                                button { class: "term-x", onclick: move |_| term_lines.write().clear(), "clear" }
                                button { class: "term-x", onclick: move |_| show_terminal.set(false), "✕" }
                            }
                        }
                        div { class: "term-body",
                            for line in term_lines.read().iter() {
                                pre { class: "term-line", "{line}" }
                            }
                        }
                        div { class: "term-input-row",
                            span { class: "term-prompt", "$" }
                            input {
                                class: "term-input",
                                placeholder: "run a command…",
                                value: "{term_input}",
                                oninput: move |e| term_input.set(e.value()),
                                onkeydown: move |e| if e.key() == Key::Enter { run_term(); },
                            }
                        }
                    }
                }
            }

            // ── Right inspector (tabbed) ───────────────────────────────
            if *show_files.read() && !*show_preview.read() && cfg.read().workspace.is_some() {
                aside { class: "files-panel",
                    div { class: "insp-tabs",
                        for (key, label) in [("review","Review"),("files","Files"),("timeline","Timeline"),("sessions","Sessions"),("git","Git"),("memory","Memory"),("goal","Goal"),("browser","Browser"),("approvals","Approvals"),("checkpoints","Checkpoints"),("usage","Usage")] {
                            {
                                let active = *inspector_tab.read() == key;
                                let badge = match key {
                                    "approvals" => approvals.read().len(),
                                    "checkpoints" => checkpoints.read().len(),
                                    "review" => turn_edits.read().len(),
                                    _ => 0,
                                };
                                let k = key.to_string();
                                rsx! {
                                    button {
                                        class: if active { "insp-tab on" } else { "insp-tab" },
                                        onclick: move |_| inspector_tab.set(k.clone()),
                                        "{label}"
                                        if badge > 0 { span { class: "insp-badge", "{badge}" } }
                                    }
                                }
                            }
                        }
                        button { class: "term-x", onclick: move |_| show_files.set(false), "✕" }
                    }
                    div { class: "insp-body",
                        match inspector_tab.read().as_str() {
                            "review" => rsx! {
                                if turn_edits.read().is_empty() {
                                    div { class: "insp-empty", "No changes to review. Edits the agent makes appear here — accept to keep, reject to revert." }
                                } else {
                                    div { class: "review-head",
                                        span { class: "review-count", "{turn_edits.read().len()} changed file(s)" }
                                        button { class: "ed-close", onclick: move |_| {
                                            let edits = turn_edits.read().clone();
                                            for (_, _, _, cp, _) in edits.iter().rev() { let _ = engine.send(EngineCmd::Rewind { id: *cp }); }
                                            turn_edits.write().clear();
                                        }, "Reject all" }
                                    }
                                    for (idx, (path, adds, dels, cp, diff)) in turn_edits.read().clone().into_iter().enumerate() {
                                        div { class: "review-item",
                                            details { class: "review-diff-d",
                                                summary { class: "review-file",
                                                    span { class: "edits-caret", Icon { name: "chevron" } }
                                                    span { class: "review-path", "{path}" }
                                                    span { class: "diff-adds", "+{adds}" }
                                                    span { class: "diff-dels", "−{dels}" }
                                                }
                                                HunkedDiff { ws: workspace.clone(), path: path.clone(), diff }
                                            }
                                            div { class: "review-actions",
                                                button { class: "review-accept", title: "Keep this change", onclick: move |_| {
                                                    let mut v = turn_edits.write(); if idx < v.len() { v.remove(idx); }
                                                }, "Accept" }
                                                button { class: "review-reject", title: "Revert this change", onclick: move |_| {
                                                    let _ = engine.send(EngineCmd::Rewind { id: cp });
                                                    let mut v = turn_edits.write(); if idx < v.len() { v.remove(idx); }
                                                }, "Reject" }
                                            }
                                        }
                                    }
                                }
                            },
                            "files" => rsx! {
                                div { class: "tree",
                                    FileNode { path: workspace.clone(), depth: 0, is_root: true }
                                }
                            },
                            "approvals" => rsx! {
                                if approvals.read().is_empty() {
                                    div { class: "insp-empty", "No pending approvals. (Bypass is on — use --safe to require them.)" }
                                }
                                for (id, tool, summary) in approvals.read().iter().cloned() {
                                    div { class: "insp-card",
                                        div { class: "insp-card-title", "{tool}" }
                                        div { class: "insp-card-sub", "{summary}" }
                                        div { class: "insp-card-actions",
                                            button { class: "ed-save", onclick: move |_| { let _ = engine.send(EngineCmd::Approve { id, decision: ApprovalDecision::Approve }); }, "Approve" }
                                            button { class: "ed-close", onclick: move |_| { let _ = engine.send(EngineCmd::Approve { id, decision: ApprovalDecision::Reject }); }, "Reject" }
                                        }
                                    }
                                }
                            },
                            "checkpoints" => rsx! {
                                if checkpoints.read().is_empty() {
                                    div { class: "insp-empty", "No checkpoints yet. Each file write the agent makes is snapshotted here." }
                                }
                                for (id, label) in checkpoints.read().iter().cloned().rev() {
                                    div { class: "insp-card",
                                        div { class: "insp-card-title", "#{id} · {label}" }
                                        div { class: "insp-card-actions",
                                            button { class: "ed-close", onclick: move |_| { let _ = engine.send(EngineCmd::Rewind { id }); }, "Rewind to here" }
                                        }
                                    }
                                }
                            },
                            "sessions" => rsx! {
                                {
                                    let sessions = list_sessions(&workspace);
                                    rsx! {
                                        if sessions.is_empty() {
                                            div { class: "insp-empty", "No saved sessions yet. Conversations persist to .oxide/sessions." }
                                        }
                                        for (id, count, path) in sessions {
                                            div { class: "insp-card",
                                                div { class: "insp-card-title", "session {id}" }
                                                div { class: "insp-card-sub", "{count} message(s)" }
                                                div { class: "insp-card-actions",
                                                    button { class: "ed-save", onclick: move |_| {
                                                        let msgs = load_session(&path);
                                                        messages.set(msgs);
                                                    }, "Open transcript" }
                                                }
                                            }
                                        }
                                    }
                                }
                            },
                            "git" => {
                                let ws_refresh = workspace.clone();
                                let ws_commit = workspace.clone();
                                let ws_files = workspace.clone();
                                let ws_refresh2 = workspace.clone();
                                let ws_pr = workspace.clone();
                                rsx! {
                                    div { class: "git-bar",
                                        button { class: "ed-close", onclick: move |_| {
                                            let ws = ws_refresh.clone();
                                            spawn(async move {
                                                let s = git_run(ws, vec!["status".into(),"--short".into()]).await;
                                                git_status.set(s.lines().map(|l| l.to_string()).collect());
                                            });
                                        }, "Refresh" }
                                    }
                                    if git_status.read().is_empty() {
                                        div { class: "insp-empty", "Click Refresh to load git status." }
                                    }
                                    for line in git_status.read().iter().cloned() {
                                        {
                                            let file = line.get(3..).unwrap_or("").trim().to_string();
                                            let ch = line.get(..2).unwrap_or("").trim().chars().next().unwrap_or('?');
                                            let (badge_cls, badge) = match ch {
                                                'M' | 'R' => ("git-badge m", "M"),
                                                'A' => ("git-badge a", "A"),
                                                'D' => ("git-badge d", "D"),
                                                _ => ("git-badge u", "U"),
                                            };
                                            let ws = ws_files.clone();
                                            rsx! {
                                                button { class: "git-file", onclick: move |_| {
                                                    let ws = ws.clone();
                                                    let f = file.clone();
                                                    spawn(async move {
                                                        let d = git_run(ws, vec!["diff".into(), f]).await;
                                                        git_diff.set(d);
                                                    });
                                                },
                                                    span { class: "{badge_cls}", "{badge}" }
                                                    span { class: "git-name", "{file}" }
                                                }
                                            }
                                        }
                                    }
                                    if !git_diff.read().is_empty() {
                                        pre { class: "git-diff", "{git_diff}" }
                                    }
                                    div { class: "git-commit",
                                        input { class: "field-input", placeholder: "Commit message…",
                                            value: "{commit_msg}", oninput: move |e| commit_msg.set(e.value()) }
                                        button { class: "ed-save", onclick: move |_| {
                                            let ws = ws_commit.clone();
                                            let msg = commit_msg.read().clone();
                                            if !msg.trim().is_empty() {
                                                commit_msg.set(String::new());
                                                spawn(async move {
                                                    let _ = git_run(ws.clone(), vec!["add".into(),"-A".into()]).await;
                                                    let out = git_run(ws.clone(), vec!["commit".into(),"-m".into(), msg]).await;
                                                    let s = git_run(ws.clone(), vec!["status".into(),"--short".into()]).await;
                                                    git_status.set(s.lines().map(|l| l.to_string()).collect());
                                                    git_diff.set(out);
                                                });
                                            }
                                        }, "Commit" }
                                    }
                                    div { class: "git-actions",
                                        span { class: "git-branch-label", Icon { name: "git" } "{branch}" }
                                        button { class: "git-act", title: "Push to origin", onclick: {
                                            let ws = ws_refresh2.clone();
                                            move |_| {
                                                let ws = ws.clone();
                                                git_busy.set("Pushing…".into());
                                                spawn(async move {
                                                    let out = git_run(ws.clone(), vec!["push".into()]).await;
                                                    let out = if out.to_lowercase().contains("no upstream") || out.to_lowercase().contains("set-upstream") {
                                                        let b = git_branch(&ws);
                                                        git_run(ws.clone(), vec!["push".into(),"-u".into(),"origin".into(), b]).await
                                                    } else { out };
                                                    git_busy.set(String::new());
                                                    git_diff.set(format!("$ git push\n{out}"));
                                                });
                                            }
                                        }, "Push ↑" }
                                        button { class: "git-act", title: "Create a pull request (gh)", onclick: {
                                            let ws = ws_pr.clone();
                                            move |_| {
                                                let ws = ws.clone();
                                                git_busy.set("Creating PR…".into());
                                                spawn(async move {
                                                    let b = git_branch(&ws);
                                                    let _ = git_run(ws.clone(), vec!["push".into(),"-u".into(),"origin".into(), b]).await;
                                                    let out = run_cmd(&ws, "gh", &["pr","create","--fill"]).await;
                                                    git_busy.set(String::new());
                                                    git_diff.set(format!("$ gh pr create --fill\n{out}"));
                                                });
                                            }
                                        }, "Create PR" }
                                        if !git_busy.read().is_empty() { span { class: "git-busy", span { class: "syn-spinner" } "{git_busy}" } }
                                    }
                                }
                            },
                            "memory" => {
                                let mem_path = workspace.join(".oxide/memory/MEMORY.md");
                                let mem_save = mem_path.clone();
                                let skills_dir = workspace.join(".oxide/memory/skills");
                                let mut skills: Vec<String> = std::fs::read_dir(&skills_dir)
                                    .into_iter().flatten().flatten()
                                    .map(|e| e.path())
                                    .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("md"))
                                    .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().to_string()))
                                    .collect();
                                skills.sort();
                                rsx! {
                                    div { class: "git-bar",
                                        button { class: "ed-close", onclick: move |_| {
                                            memory_text.set(std::fs::read_to_string(&mem_path).unwrap_or_default());
                                        }, "Load" }
                                    }
                                    div { class: "insp-empty", "Durable facts the agent remembers across sessions (MEMORY.md). It also reads/writes this via the remember tool." }
                                    textarea { class: "goal-input", placeholder: "Nothing remembered yet — click Load, or the agent will fill this.",
                                        value: "{memory_text}", oninput: move |e| memory_text.set(e.value()) }
                                    button { class: "ed-save", onclick: move |_| {
                                        if let Some(parent) = mem_save.parent() { let _ = std::fs::create_dir_all(parent); }
                                        let _ = std::fs::write(&mem_save, memory_text.read().clone());
                                    }, "Save memory" }
                                    div { class: "menu-label", "Learned skills" }
                                    if skills.is_empty() {
                                        div { class: "insp-empty", "No skills yet. The agent saves reusable procedures via save_skill." }
                                    }
                                    for s in skills {
                                        div { class: "tl-item", div { class: "tl-title", "🛠 {s}" } }
                                    }
                                }
                            },
                            "goal" => rsx! {
                                {
                                    let mut goal_text = goal_text;
                                    let mut pursue = pursue_goal;
                                    let active = *pursue.read();
                                    rsx! {
                                        div { class: "insp-empty", "Set a goal — the agent keeps working toward it until done." }
                                        textarea { class: "goal-input", placeholder: "Describe the goal…",
                                            value: "{goal_text}", oninput: move |e| goal_text.set(e.value()) }
                                        button { class: if active { "ed-save" } else { "ed-close" },
                                            onclick: move |_| { let v = *pursue.read(); pursue.set(!v); },
                                            if active { "Goal active · click to stop" } else { "Activate goal" }
                                        }
                                    }
                                }
                            },
                            "browser" => rsx! {
                                div { class: "git-commit",
                                    input { class: "field-input", placeholder: "https://…",
                                        value: "{browser_url}", oninput: move |e| browser_url.set(e.value()) }
                                    button { class: "ed-save", onclick: move |_| {
                                        let url = browser_url.read().trim().to_string();
                                        if !url.is_empty() {
                                            browser_log.write().push(url.clone());
                                            spawn(async move {
                                                let _ = tokio::process::Command::new("open").arg(&url).output().await;
                                            });
                                        }
                                    }, "Open" }
                                }
                                if browser_log.read().is_empty() {
                                    div { class: "insp-empty", "Open a URL in your browser, or the agent's browser_open tool will log here." }
                                }
                                for u in browser_log.read().iter().cloned().rev() {
                                    div { class: "tl-item", div { class: "tl-title", "{u}" } }
                                }
                            },
                            "usage" => rsx! {
                                {
                                    let (tin, tout) = *usage.read();
                                    let limit = context_limit.read().unwrap_or(0);
                                    let pct = if limit > 0 { (tin as f64 / limit as f64 * 100.0).min(100.0) } else { 0.0 };
                                    rsx! {
                                        div { class: "usage-grid",
                                            div { class: "usage-stat", div { class: "usage-num", "{tin}" } div { class: "usage-lbl", "input tokens" } }
                                            div { class: "usage-stat", div { class: "usage-num", "{tout}" } div { class: "usage-lbl", "output tokens" } }
                                        }
                                        if limit > 0 {
                                            div { class: "usage-bar-wrap",
                                                div { class: "usage-bar-label", "context · {tin/1000}k / {limit/1000}k" }
                                                div { class: "usage-bar", div { class: "usage-bar-fill", style: "width: {pct}%" } }
                                            }
                                        }
                                    }
                                }
                            },
                            _ => rsx! {
                                if timeline.read().is_empty() {
                                    div { class: "insp-empty", "Activity will appear here as the agent works." }
                                }
                                for item in timeline.read().iter().cloned().rev() {
                                    div { class: "tl-item",
                                        div { class: "tl-title", "{item.title}" }
                                        if !item.sub.is_empty() { div { class: "tl-sub", "{item.sub}" } }
                                    }
                                }
                            },
                        }
                    }
                }
            }

            // ── Settings modal ─────────────────────────────────────────
            if *show_settings.read() {
                SettingsModal { cfg, ui, engine, on_close: move |_| show_settings.set(false) }
            }
            if *show_skills.read() {
                SkillsModal { workspace: workspace.clone(), on_close: move |_| show_skills.set(false) }
            }
            if *show_mcp.read() {
                McpModal { cfg, engine, status: mcp_status, on_close: move |_| show_mcp.set(false) }
            }
            if *show_shortcuts.read() {
                div { class: "modal-overlay", onclick: move |_| show_shortcuts.set(false),
                    div { class: "modal shortcuts-modal", onclick: move |e| e.stop_propagation(),
                        div { class: "modal-head", h2 { "Keyboard shortcuts" } button { class: "term-x", onclick: move |_| show_shortcuts.set(false), "✕" } }
                        div { class: "modal-body shortcuts-body",
                            for (k, d) in [
                                ("⌘K", "Command palette + chat search"),
                                ("⌘/", "This shortcuts sheet"),
                                ("⌘B", "Toggle Files inspector"),
                                ("⌘1–9", "Jump to agent tab N"),
                                ("⌘⇧]", "Next tab"),
                                ("⌘⇧[", "Previous tab"),
                                ("⌘↵", "Send message"),
                                ("⇧↵", "New line in composer"),
                                ("⇧⇥", "Toggle plan mode (in composer)"),
                                ("@", "Mention MCP / skill / file"),
                                ("Esc", "Close menus / overlays"),
                                ("Double-click tab", "Rename"),
                                ("Right-click chat", "Archive / Delete"),
                                ("Right-click sidebar", "Theme / Pin window / PiP"),
                            ] {
                                div { class: "shortcut-row",
                                    kbd { class: "shortcut-key", "{k}" }
                                    span { class: "shortcut-desc", "{d}" }
                                }
                            }
                        }
                    }
                }
            }
            if *show_palette.read() {
                {
                    let run = move |label: &str| {
                        show_palette.set(false);
                        match label {
                            "New chat" => {
                                show_board.set(false);
                                let prov = cfg.read().provider.clone();
                                let model = cfg.read().model.clone();
                                let title = provider_title(&prov).to_string();
                                new_agent_tab(tabs, active_tab, messages, cfg, engine, next_tab_id, &prov, &model, &title);
                            }
                            "Open folder…" => open_folder(cfg, ui, engine),
                            "Split view" => { let v = !*show_split.read(); show_split.set(v); }
                            "MCP servers" => show_mcp.set(true),
                            "Skills" => show_skills.set(true),
                            "Board" => { show_board.set(true); }
                            "Files panel" => { let v = !*show_files.read(); show_files.set(v); }
                            "Terminal" => { let v = !*show_terminal.read(); show_terminal.set(v); }
                            "Settings…" => show_settings.set(true),
                            "Theme: Light" => set_theme(cfg, "light"),
                            "Theme: Dark" => set_theme(cfg, "dark"),
                            "Theme: System" => set_theme(cfg, "system"),
                            "Toggle density" => toggle_density(cfg),
                            _ => {}
                        }
                    };
                    let actions: Vec<(&str, &str)> = vec![
                        ("plus", "New chat"), ("folder", "Open folder…"), ("plugins", "Split view"),
                        ("plugins", "MCP servers"), ("target", "Skills"), ("list", "Board"),
                        ("plugins", "Files panel"), ("terminal", "Terminal"), ("settings", "Settings…"),
                        ("spark", "Theme: Light"), ("target", "Theme: Dark"), ("settings", "Theme: System"),
                        ("list", "Toggle density"),
                    ];
                    let q = palette_query.read().to_lowercase();
                    let filtered: Vec<(&str, &str)> = actions.into_iter().filter(|(_, l)| q.is_empty() || l.to_lowercase().contains(&q)).collect();
                    let sel = if filtered.is_empty() { 0 } else { (*palette_sel.read()).min(filtered.len() - 1) };
                    let f2 = filtered.clone();
                    rsx! {
                        div { class: "modal-overlay palette-overlay", onclick: move |_| show_palette.set(false),
                            div { class: "palette", onclick: move |e| e.stop_propagation(),
                                input { class: "palette-input", autofocus: true, placeholder: "Type a command…", value: "{palette_query}",
                                    oninput: move |e| { palette_query.set(e.value()); palette_sel.set(0); },
                                    onkeydown: move |e| {
                                        let n = f2.len();
                                        if n == 0 { return; }
                                        match e.key() {
                                            Key::ArrowDown => { e.prevent_default(); palette_sel.set((sel + 1) % n); }
                                            Key::ArrowUp => { e.prevent_default(); palette_sel.set((sel + n - 1) % n); }
                                            Key::Enter => { e.prevent_default(); let mut run = run; run(f2[sel].1); }
                                            _ => {}
                                        }
                                    }
                                }
                                div { class: "palette-list",
                                    for (i, (icon, label)) in filtered.into_iter().enumerate() {
                                        button { class: if i == sel { "palette-item sel" } else { "palette-item" },
                                            onmouseenter: move |_| palette_sel.set(i),
                                            onclick: move |_| { let mut run = run; run(label); },
                                            Icon { name: icon } span { class: "palette-label", "{label}" }
                                        }
                                    }
                                    if !q.is_empty() {
                                        {
                                            let chats: Vec<(PathBuf, String)> = recent_sessions(&workspace).into_iter()
                                                .map(|(p, _, t)| (p, t))
                                                .filter(|(_, t)| t.to_lowercase().contains(&q))
                                                .take(8).collect();
                                            if chats.is_empty() { rsx!{} } else {
                                                rsx! {
                                                    div { class: "menu-label", style: "padding:8px 12px 4px", "Chats" }
                                                    for (p, title) in chats {
                                                        {
                                                            let p2 = p.clone();
                                                            let t2 = title.clone();
                                                            rsx! {
                                                                button { class: "palette-item",
                                                                    onclick: move |_| { show_palette.set(false); show_board.set(false); open_session_tab(tabs, active_tab, messages, next_tab_id, cfg, p2.clone(), t2.clone()); },
                                                                    Icon { name: "file" } span { class: "palette-label", "{title}" }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn McpModal(cfg: Signal<Config>, engine: Coroutine<EngineCmd>, status: Signal<std::collections::HashMap<String, String>>, on_close: EventHandler<()>) -> Element {
    let mut name = use_signal(String::new);
    let mut command = use_signal(String::new);
    let mut args = use_signal(String::new);
    let servers = cfg.read().mcp_servers.clone();
    let imported: Vec<oxide_config::McpServerConfig> = oxide_core::discover_external_mcp()
        .into_iter()
        .filter(|e| !servers.iter().any(|s| s.name == e.name))
        .collect();
    rsx! {
        div { class: "modal-overlay", onclick: move |_| on_close.call(()),
            div { class: "modal skills-modal", onclick: move |e| e.stop_propagation(),
                div { class: "modal-head",
                    h2 { "MCP servers" }
                    button { class: "term-x", onclick: move |_| on_close.call(()), "✕" }
                }
                div { class: "modal-body skills-body",
                    if servers.is_empty() {
                        div { class: "insp-empty", "No MCP servers. Add one below (e.g. npx @modelcontextprotocol/server-filesystem)." }
                    }
                    for (i, s) in servers.iter().enumerate() {
                        {
                            let i = i;
                            let st = status.read().get(&s.name).cloned();
                            let connected = st.as_deref().map(|x| x.starts_with("connected")).unwrap_or(false);
                            let cmdline = format!("{} {}", s.command, s.args.join(" "));
                            let servers2 = servers.clone();
                            rsx! {
                                div { class: "mcp-item",
                                    div { class: "mcp-top",
                                        span { class: if connected { "mcp-dot on" } else { "mcp-dot" } }
                                        span { class: "skill-name", "{s.name}" }
                                        button { class: "mcp-remove", onclick: move |_| {
                                            let mut list = servers2.clone(); list.remove(i);
                                            let mut c = cfg.read().clone(); c.mcp_servers = list; cfg.set(c.clone());
                                            let _ = engine.send(EngineCmd::Reconfigure(c));
                                        }, "Remove" }
                                    }
                                    div { class: "mcp-cmd", "{cmdline}" }
                                    if let Some(st) = st { div { class: "mcp-st", "{st}" } }
                                }
                            }
                        }
                    }
                    if !imported.is_empty() {
                        div { class: "mcp-section", "Imported from Codex / Claude" }
                        for s in imported.iter() {
                            {
                                let st = status.read().get(&s.name).cloned();
                                let connected = st.as_deref().map(|x| x.starts_with("connected")).unwrap_or(false);
                                let line = if s.url.is_empty() { format!("{} {}", s.command, s.args.join(" ")) } else { s.url.clone() };
                                let disabled = !s.enabled;
                                rsx! {
                                    div { class: "mcp-item",
                                        div { class: "mcp-top",
                                            span { class: if connected { "mcp-dot on" } else { "mcp-dot" } }
                                            span { class: "skill-name", "{s.name}" }
                                            span { class: "mcp-tag", if disabled { "disabled" } else if s.url.is_empty() { "imported" } else { "http" } }
                                        }
                                        div { class: "mcp-cmd", "{line}" }
                                        if let Some(st) = st { div { class: "mcp-st", "{st}" } }
                                    }
                                }
                            }
                        }
                    }
                    div { class: "mcp-section", "Add server" }
                    div { class: "mcp-add",
                        input { class: "field-input", placeholder: "name (e.g. fs)", value: "{name}", oninput: move |e| name.set(e.value()) }
                        input { class: "field-input", style: "margin-top:6px", placeholder: "command (e.g. npx)", value: "{command}", oninput: move |e| command.set(e.value()) }
                        input { class: "field-input", style: "margin-top:6px", placeholder: "args (space-separated)", value: "{args}", oninput: move |e| args.set(e.value()) }
                        button { class: "board-btn", style: "margin-top:8px", onclick: move |_| {
                            let n = name.read().trim().to_string();
                            let cmd = command.read().trim().to_string();
                            if n.is_empty() || cmd.is_empty() { return; }
                            let a: Vec<String> = args.read().split_whitespace().map(String::from).collect();
                            let mut list = cfg.read().mcp_servers.clone();
                            list.push(oxide_config::McpServerConfig { name: n, command: cmd, args: a, url: String::new(), enabled: true });
                            let mut c = cfg.read().clone(); c.mcp_servers = list; cfg.set(c.clone());
                            let _ = engine.send(EngineCmd::Reconfigure(c));
                            name.set(String::new()); command.set(String::new()); args.set(String::new());
                        }, "+ Add server" }
                    }
                }
            }
        }
    }
}

#[component]
fn SkillsModal(workspace: PathBuf, on_close: EventHandler<()>) -> Element {
    let mut query = use_signal(String::new);
    let skills = use_hook(|| discover_skills(&workspace));
    let q = query.read().to_ascii_lowercase();
    let filtered: Vec<(&'static str, String, String)> = skills
        .iter()
        .filter(|(src, n, d)| {
            q.is_empty()
                || n.to_ascii_lowercase().contains(&q)
                || d.to_ascii_lowercase().contains(&q)
                || src.to_ascii_lowercase().contains(&q)
        })
        .cloned()
        .collect();
    let total = skills.len();

    rsx! {
        div { class: "modal-overlay", onclick: move |_| on_close.call(()),
            div { class: "modal skills-modal", onclick: move |e| e.stop_propagation(),
                div { class: "modal-head",
                    h2 { "Skills · {total}" }
                    button { class: "term-x", onclick: move |_| on_close.call(()), "✕" }
                }
                div { class: "menu-search", style: "margin: 0 18px",
                    Icon { name: "search" }
                    input { class: "model-search", placeholder: "Search skills…",
                        value: "{query}", oninput: move |e| query.set(e.value()) }
                }
                div { class: "modal-body skills-body",
                    if filtered.is_empty() {
                        div { class: "insp-empty", "No skills found. Codex/Claude Code skills live in ~/.codex/plugins and ~/.claude/skills." }
                    }
                    for item in filtered.iter() {
                        {
                            let src = item.0;
                            let name = item.1.clone();
                            let desc = item.2.clone();
                            let src_cls = if src == "Codex" { "skill-src codex" } else if src == "Claude Code" { "skill-src claude" } else { "skill-src oxide" };
                            rsx! {
                                div { class: "skill-item",
                                    div { class: "skill-top",
                                        span { class: "skill-name", "{name}" }
                                        span { class: "{src_cls}", "{src}" }
                                    }
                                    if !desc.is_empty() { div { class: "skill-desc", "{desc}" } }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Switch to `dir`: update workspace, recent list, tree, and reconfigure.
fn apply_workspace(mut cfg: Signal<Config>, mut ui: Ui, engine: Coroutine<EngineCmd>, dir: PathBuf) {
    ui.workspace.set(dir.clone());
    ui.open_path.set(None);
    ui.expanded.set(HashSet::new());
    let mut c = cfg.read().clone();
    c.recent_workspaces.retain(|p| p != &dir);
    c.recent_workspaces.insert(0, dir.clone());
    c.recent_workspaces.truncate(8);
    c.workspace = Some(dir);
    cfg.set(c.clone());
    let _ = engine.send(EngineCmd::Reconfigure(c));
}

/// Switch the active agent tab: save the current transcript, load the target's.
fn switch_tab(
    mut tabs: Signal<Vec<AgentTab>>,
    mut active_tab: Signal<usize>,
    mut messages: Signal<Vec<ChatMsg>>,
    mut cfg: Signal<Config>,
    engine: Coroutine<EngineCmd>,
    idx: usize,
) {
    let cur = *active_tab.read();
    if cur == idx {
        return;
    }
    if let Some(t) = tabs.write().get_mut(cur) {
        t.messages = messages.read().clone();
    }
    let t = tabs.read()[idx].clone();
    active_tab.set(idx);
    let mut c = cfg.read().clone();
    c.provider = t.provider.clone();
    c.model = t.model.clone();
    cfg.set(c.clone());
    let _ = engine.send(EngineCmd::SwitchTab(c, t.messages.clone()));
}

/// Open a fresh agent tab for `provider` and make it active.
fn new_agent_tab(
    mut tabs: Signal<Vec<AgentTab>>,
    mut active_tab: Signal<usize>,
    mut messages: Signal<Vec<ChatMsg>>,
    mut cfg: Signal<Config>,
    engine: Coroutine<EngineCmd>,
    mut next_id: Signal<u64>,
    provider: &str,
    model: &str,
    title: &str,
) {
    let cur = *active_tab.read();
    if let Some(t) = tabs.write().get_mut(cur) {
        t.messages = messages.read().clone();
    }
    let id = *next_id.read();
    next_id.set(id + 1);
    tabs.write().push(AgentTab {
        id,
        title: title.to_string(),
        provider: provider.to_string(),
        model: model.to_string(),
        messages: Vec::new(),
        mode: "gui".to_string(),
        bin: String::new(),
    });
    let idx = tabs.read().len() - 1;
    active_tab.set(idx);
    let mut c = cfg.read().clone();
    c.provider = provider.to_string();
    c.model = model.to_string();
    cfg.set(c.clone());
    let _ = engine.send(EngineCmd::SwitchTab(c, Vec::new()));
}

/// Open an embedded-TUI tab running `bin` (codex/claude) in a PTY.
fn new_tui_tab(
    mut tabs: Signal<Vec<AgentTab>>,
    mut active_tab: Signal<usize>,
    mut messages: Signal<Vec<ChatMsg>>,
    mut next_id: Signal<u64>,
    bin: &str,
    title: &str,
) {
    let cur = *active_tab.read();
    if let Some(t) = tabs.write().get_mut(cur) {
        t.messages = messages.read().clone();
    }
    let id = *next_id.read();
    next_id.set(id + 1);
    tabs.write().push(AgentTab {
        id,
        title: format!("{title} (TUI)"),
        provider: bin.to_string(),
        model: String::new(),
        messages: Vec::new(),
        mode: "tui".to_string(),
        bin: bin.to_string(),
    });
    let idx = tabs.read().len() - 1;
    active_tab.set(idx);
}

/// Close an agent tab and switch to a neighbor.
fn close_tab(
    tabs: Signal<Vec<AgentTab>>,
    active_tab: Signal<usize>,
    messages: Signal<Vec<ChatMsg>>,
    cfg: Signal<Config>,
    engine: Coroutine<EngineCmd>,
    idx: usize,
) {
    let mut tabs_w = tabs;
    let len_before = tabs_w.read().len();
    if len_before <= 1 {
        return; // keep at least one tab
    }
    tabs_w.write().remove(idx);
    let len_after = tabs_w.read().len();
    let cur = *active_tab.read();
    let new_idx = if idx < cur || cur >= len_after {
        cur.saturating_sub(1)
    } else {
        cur
    }
    .min(len_after - 1);
    // Force a reload of the now-active tab (read borrows above are dropped here).
    let mut active = active_tab;
    active.set(usize::MAX);
    switch_tab(tabs_w, active, messages, cfg, engine, new_idx);
}

/// Short tab title from the first user message.
fn make_title(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let short: String = line.chars().take(32).collect();
    if line.chars().count() > 32 {
        format!("{}…", short.trim_end())
    } else {
        short
    }
}

/// Display title for a provider id.
fn provider_title(provider: &str) -> &'static str {
    match provider {
        "claude" => "Claude Code",
        "codex" => "Codex",
        "chatgpt" => "ChatGPT",
        "openai" => "OpenAI",
        "anthropic" => "Anthropic",
        _ => "Agent",
    }
}

/// Pin / unpin a session path and persist.
fn toggle_pin(mut cfg: Signal<Config>, path: &str) {
    let mut c = cfg.read().clone();
    if let Some(i) = c.pinned_sessions.iter().position(|p| p == path) {
        c.pinned_sessions.remove(i);
    } else {
        c.pinned_sessions.insert(0, path.to_string());
    }
    cfg.set(c.clone());
    if let Ok(s) = toml::to_string(&c) {
        let ws = workspace_of(&c);
        let _ = std::fs::write(ws.join("oxide.toml"), &s);
        if let Some(home) = std::env::var_os("HOME") {
            let d = std::path::PathBuf::from(home).join(".config/oxide");
            let _ = std::fs::create_dir_all(&d);
            let _ = std::fs::write(d.join("config.toml"), &s);
        }
    }
}

/// Toggle UI density (comfortable ↔ compact) and persist.
fn toggle_density(mut cfg: Signal<Config>) {
    let mut c = cfg.read().clone();
    c.density = if c.density == "compact" { "comfortable".to_string() } else { "compact".to_string() };
    cfg.set(c.clone());
    if let Ok(s) = toml::to_string(&c) {
        let ws = workspace_of(&c);
        let _ = std::fs::write(ws.join("oxide.toml"), &s);
        if let Some(home) = std::env::var_os("HOME") {
            let d = std::path::PathBuf::from(home).join(".config/oxide");
            let _ = std::fs::create_dir_all(&d);
            let _ = std::fs::write(d.join("config.toml"), &s);
        }
    }
}

/// Set the UI theme and persist (no engine reconfigure, so chat stays).
fn set_theme(mut cfg: Signal<Config>, theme: &str) {
    let mut c = cfg.read().clone();
    c.theme = theme.to_string();
    cfg.set(c.clone());
    if let Ok(s) = toml::to_string(&c) {
        let ws = workspace_of(&c);
        let _ = std::fs::write(ws.join("oxide.toml"), &s);
        if let Some(home) = std::env::var_os("HOME") {
            let d = std::path::PathBuf::from(home).join(".config/oxide");
            let _ = std::fs::create_dir_all(&d);
            let _ = std::fs::write(d.join("config.toml"), &s);
        }
    }
}

/// Set a custom accent color (empty = theme default) and persist.
fn set_accent(mut cfg: Signal<Config>, accent: &str) {
    let mut c = cfg.read().clone();
    c.accent_color = accent.to_string();
    cfg.set(c.clone());
    if let Ok(s) = toml::to_string(&c) {
        let ws = workspace_of(&c);
        let _ = std::fs::write(ws.join("oxide.toml"), &s);
        if let Some(home) = std::env::var_os("HOME") {
            let d = std::path::PathBuf::from(home).join(".config/oxide");
            let _ = std::fs::create_dir_all(&d);
            let _ = std::fs::write(d.join("config.toml"), &s);
        }
    }
}

/// Native folder picker → switch workspace.
fn open_folder(cfg: Signal<Config>, ui: Ui, engine: Coroutine<EngineCmd>) {
    // MUST use the async dialog: the blocking `FileDialog::pick_folder()` runs
    // an NSOpenPanel modal loop on the main thread, which deadlocks the webview
    // when invoked from inside a synchronous JS→native event dispatch.
    spawn(async move {
        if let Some(h) = rfd::AsyncFileDialog::new().pick_folder().await {
            apply_workspace(cfg, ui, engine, h.path().to_path_buf());
        }
    });
}

/// Local git branches (short names).
fn git_branches(ws: &Path) -> Vec<String> {
    std::process::Command::new("git")
        .args(["branch", "--format=%(refname:short)"])
        .current_dir(ws)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}

/// Git worktrees `(path, branch)`.
fn git_worktrees(ws: &Path) -> Vec<(PathBuf, String)> {
    let out = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(ws)
        .output();
    let Ok(out) = out else { return Vec::new() };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut res = Vec::new();
    let mut cur: Option<PathBuf> = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            cur = Some(PathBuf::from(p.trim()));
        } else if let Some(b) = line.strip_prefix("branch ") {
            let branch = b.trim().rsplit('/').next().unwrap_or("").to_string();
            if let Some(p) = cur.take() {
                res.push((p, branch));
            }
        } else if line.is_empty() {
            if let Some(p) = cur.take() {
                res.push((p, "detached".to_string()));
            }
        }
    }
    res
}

#[component]
fn FileNode(path: PathBuf, depth: usize, is_root: bool) -> Element {
    let ui = use_context::<Ui>();
    let is_dir = is_root || path.is_dir();
    let name = if is_root {
        project_name(&path)
    } else {
        path.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    };
    let expanded = is_root || ui.expanded.read().contains(&path);
    let pad = format!("padding-left: {}px", 8 + depth * 14);
    let is_open = ui.open_path.read().as_ref() == Some(&path);
    let node_cls = if is_open { "node open" } else { "node" };
    let icon_name = if is_dir { "folder" } else { "file" };
    let caret = if !is_dir {
        " "
    } else if expanded {
        "▾"
    } else {
        "▸"
    };

    let p2 = path.clone();
    let toggle = move |_| {
        if is_dir {
            let mut ex = ui.expanded;
            let mut set = ex.read().clone();
            if !set.remove(&p2) {
                set.insert(p2.clone());
            }
            ex.set(set);
        } else {
            open_file(ui, p2.clone());
        }
    };

    rsx! {
        div {
            class: "{node_cls}",
            style: "{pad}",
            onclick: toggle,
            span { class: "caret", "{caret}" }
            Icon { name: icon_name }
            span { class: "node-name", "{name}" }
        }
        if is_dir && expanded {
            for (child, _) in read_children(&path) {
                FileNode { path: child.clone(), depth: depth + 1, is_root: false }
            }
        }
    }
}

#[component]
fn Editor() -> Element {
    let mut ui = use_context::<Ui>();
    let path = ui.open_path.read().clone();
    let Some(path) = path else {
        return rsx! {};
    };
    let title = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let dirty = *ui.dirty.read();

    rsx! {
        div { class: "editor",
            div { class: "editor-head",
                span { class: "editor-title",
                    "{title}"
                    if dirty { span { class: "dot-dirty", "●" } }
                }
                div { class: "editor-actions",
                    button {
                        class: "ed-save",
                        onclick: move |_| {
                            let p = ui.open_path.read().clone();
                            if let Some(p) = p {
                                let text = ui.editor_text.read().clone();
                                let _ = std::fs::write(&p, text);
                                ui.dirty.set(false);
                            }
                        },
                        "Save"
                    }
                    button { class: "ed-close", onclick: move |_| ui.open_path.set(None), "Close" }
                }
            }
            textarea {
                class: "editor-area",
                spellcheck: false,
                value: "{ui.editor_text}",
                oninput: move |e| { ui.editor_text.set(e.value()); ui.dirty.set(true); },
            }
        }
    }
}

#[component]
fn SettingsModal(
    cfg: Signal<Config>,
    ui: Ui,
    engine: Coroutine<EngineCmd>,
    on_close: EventHandler<()>,
) -> Element {
    let base = cfg.read().clone();
    let mut provider = use_signal(|| base.provider.clone());
    let mut harness = use_signal(|| base.harness.clone());
    let harness_opts = {
        let dir = base.harness_dir.clone().unwrap_or_else(|| PathBuf::from("harnesses"));
        let dir = if dir.is_absolute() { dir } else { workspace_of(&base).join(dir) };
        list_harnesses(&dir)
    };
    let mut model = use_signal(|| base.model.clone());
    let mut effort = use_signal(|| base.reasoning_effort.clone());
    let mut fast = use_signal(|| base.fast_mode);
    let mut bypass = use_signal(|| matches!(base.approval_policy, ApprovalPolicy::Never));
    let mut ws = use_signal(|| workspace_of(&base));
    let mut orchestrate = use_signal(|| base.orchestrate);
    let mut front = use_signal(|| base.front_provider.clone());
    let mut backend = use_signal(|| base.backend_provider.clone());
    let mut subagents = use_signal(|| base.subagents);
    let mut upd_url = use_signal(|| base.update_url.clone());
    let mut gh_repo = use_signal(|| if base.github_repo.trim().is_empty() { "MANFIT7/oxide".to_string() } else { base.github_repo.clone() });
    let mut upd_status = use_signal(|| "Up to date".to_string());
    let mut tab_mode = use_signal(|| base.default_tab_mode.clone());
    let mut browser_headless = use_signal(|| base.browser_headless);

    let providers = ["chatgpt", "codex", "claude", "openai", "anthropic", "echo", "mock"];

    let mut save = move |_| {
        let mut c = cfg.read().clone();
        c.provider = provider.read().clone();
        c.harness = harness.read().clone();
        c.model = model.read().clone();
        c.reasoning_effort = effort.read().clone();
        c.fast_mode = *fast.read();
        c.orchestrate = *orchestrate.read();
        c.front_provider = front.read().clone();
        c.backend_provider = backend.read().clone();
        c.subagents = *subagents.read();
        c.update_url = upd_url.read().clone();
        c.github_repo = gh_repo.read().clone();
        c.default_tab_mode = tab_mode.read().clone();
        c.browser_headless = *browser_headless.read();
        c.approval_policy = if *bypass.read() {
            ApprovalPolicy::Never
        } else {
            ApprovalPolicy::OnRequest
        };
        if !*bypass.read() {
            c.sandbox = SandboxPolicy::WorkspaceWrite;
        }
        let chosen_ws = ws.read().clone();
        c.workspace = Some(chosen_ws.clone());
        // Persist to <workspace>/oxide.toml.
        if let Ok(s) = toml::to_string(&c) {
            let _ = std::fs::write(chosen_ws.join("oxide.toml"), s);
        }
        cfg.set(c.clone());
        let mut uiw = ui;
        uiw.workspace.set(chosen_ws);
        uiw.open_path.set(None);
        let _ = engine.send(EngineCmd::Reconfigure(c));
        on_close.call(());
    };

    let mut settings_tab = use_signal(|| "model".to_string());
    rsx! {
        div { class: "modal-overlay", onclick: move |_| on_close.call(()),
            div { class: "modal", onclick: move |e| e.stop_propagation(),
                div { class: "modal-head",
                    h2 { "Settings" }
                    button { class: "term-x", onclick: move |_| on_close.call(()), "✕" }
                }
                div { class: "settings-tabs",
                    for (key, label) in [("model", "Model"), ("access", "Access"), ("agents", "Agents"), ("updates", "Updates")] {
                        button { class: if settings_tab.read().as_str() == key { "settings-tab active" } else { "settings-tab" },
                            onclick: move |_| settings_tab.set(key.to_string()), "{label}" }
                    }
                }
                div { class: "modal-body",
                  if settings_tab.read().as_str() == "model" {
                    div { class: "field cgpt-field",
                        span { class: "field-label", "ChatGPT subscription (no API key)" }
                        div { class: "field-folder",
                            span { class: "folder-path",
                                {
                                    let s = chatgpt_status().unwrap_or_else(|| "Not connected".to_string());
                                    rsx! { "{s}" }
                                }
                            }
                            button { class: "ed-close", onclick: move |_| {
                                let _ = std::process::Command::new("codex").arg("login").spawn();
                            }, "Connect / Re-login" }
                        }
                        button { class: "ed-save", style: "margin-top:8px", onclick: move |_| {
                            provider.set("chatgpt".to_string());
                            model.set("gpt-5.5".to_string());
                            fast.set(false);
                        }, "Use ChatGPT subscription" }
                    }
                    label { class: "field",
                        span { class: "field-label", "Harness (coding behavior)" }
                        select { class: "field-input", value: "{harness}", onchange: move |e| harness.set(e.value()),
                            for h in harness_opts.iter() {
                                option { value: "{h}", selected: harness.read().as_str() == h.as_str(), "{h}" }
                            }
                        }
                    }
                    label { class: "field",
                        span { class: "field-label", "Provider" }
                        select {
                            class: "field-input",
                            value: "{provider}",
                            onchange: move |e| {
                                let next = e.value();
                                provider.set(next.clone());
                                if let Some(preset) = MODEL_PRESETS.iter().find(|p| p.provider == next) {
                                    model.set(preset.model.to_string());
                                    fast.set(preset.fast);
                                    effort.set(if preset.fast { "low".to_string() } else { "medium".to_string() });
                                }
                            },
                            for p in providers.iter() {
                                option { value: "{p}", selected: provider.read().as_str() == *p, "{p}" }
                            }
                        }
                    }
                    label { class: "field",
                        span { class: "field-label", "Model" }
                        select {
                            class: "field-input",
                            value: "{model}",
                            onchange: move |e| {
                                let next = e.value();
                                let is_fast = MODEL_PRESETS
                                    .iter()
                                    .find(|p| p.provider == provider.read().as_str() && p.model == next)
                                    .map(|p| p.fast)
                                    .unwrap_or(false);
                                model.set(next);
                                fast.set(is_fast);
                                if is_fast {
                                    effort.set("low".to_string());
                                }
                            },
                            for preset in MODEL_PRESETS.iter().filter(|p| p.provider == provider.read().as_str()) {
                                option {
                                    value: "{preset.model}",
                                    selected: model.read().as_str() == preset.model,
                                    "{preset.label} ({preset.model})"
                                }
                            }
                        }
                    }
                    label { class: "field",
                        span { class: "field-label", "Effort" }
                        select {
                            class: "field-input",
                            value: "{effort}",
                            onchange: move |e| {
                                let next = e.value();
                                if next != "low" {
                                    fast.set(false);
                                }
                                effort.set(next);
                            },
                            for preset in EFFORT_PRESETS.iter() {
                                option {
                                    value: "{preset.value}",
                                    selected: effort.read().as_str() == preset.value,
                                    "{preset.label} - {preset.summary}"
                                }
                            }
                        }
                    }
                    label { class: "field toggle-field",
                        input {
                            r#type: "checkbox",
                            checked: *fast.read(),
                            onchange: move |e| {
                                let enabled = e.checked();
                                fast.set(enabled);
                                if enabled {
                                    if let Some(preset) = fast_model_for(provider.read().as_str()) {
                                        model.set(preset.model.to_string());
                                    }
                                    effort.set("low".to_string());
                                }
                            }
                        }
                        span { class: "field-label", "Fast mode (fast model + low effort)" }
                    }
                  }
                  if settings_tab.read().as_str() == "access" {
                    label { class: "field",
                        span { class: "field-label", "Permissions" }
                        select {
                            class: "field-input",
                            onchange: move |e| bypass.set(e.value() == "full"),
                            option { value: "full", selected: *bypass.read(), "Full access (bypass)" }
                            option { value: "ask", selected: !*bypass.read(), "Ask first" }
                        }
                    }
                    div { class: "field",
                        span { class: "field-label", "Workspace folder" }
                        div { class: "field-folder",
                            span { class: "folder-path", "{ws.read().display()}" }
                            button { class: "ed-close", onclick: move |_| {
                                spawn(async move {
                                    if let Some(h) = rfd::AsyncFileDialog::new().pick_folder().await { ws.set(h.path().to_path_buf()); }
                                });
                            }, "Browse…" }
                        }
                    }
                  }
                  if settings_tab.read().as_str() == "agents" {
                    div { class: "field cgpt-field",
                        label { class: "toggle-field",
                            input { r#type: "checkbox", checked: *orchestrate.read(),
                                onchange: move |e| orchestrate.set(e.checked()) }
                            span { class: "field-label", "Orchestrate (front planner → backend implementer)" }
                        }
                        if *orchestrate.read() {
                            div { class: "orch-row",
                                div { class: "orch-col",
                                    span { class: "field-label", "Front (plan)" }
                                    select { class: "field-input", value: "{front}", onchange: move |e| front.set(e.value()),
                                        for p in providers.iter() { option { value: "{p}", selected: front.read().as_str() == *p, "{p}" } }
                                    }
                                }
                                div { class: "orch-col",
                                    span { class: "field-label", "Backend (do)" }
                                    select { class: "field-input", value: "{backend}", onchange: move |e| backend.set(e.value()),
                                        for p in providers.iter() { option { value: "{p}", selected: backend.read().as_str() == *p, "{p}" } }
                                    }
                                }
                            }
                            label { class: "toggle-field", style: "margin-top:10px",
                                input { r#type: "checkbox", checked: *subagents.read(),
                                    onchange: move |e| subagents.set(e.checked()) }
                                span { class: "field-label", "Sub-agents (fan plan out to parallel backends, then synthesize)" }
                            }
                        }
                    }
                    label { class: "field toggle-field",
                        input { r#type: "checkbox", checked: *browser_headless.read(),
                            onchange: move |e| browser_headless.set(e.checked()) }
                        span { class: "field-label", "Browser automation runs headless (background)" }
                    }
                    label { class: "field",
                        span { class: "field-label", "Default mode (new tabs / next launch)" }
                        select { class: "field-input", onchange: move |e| tab_mode.set(e.value()),
                            option { value: "gui", selected: tab_mode.read().as_str() == "gui", "GUI (chat)" }
                            option { value: "tui", selected: tab_mode.read().as_str() == "tui", "TUI (terminal)" }
                        }
                    }
                  }
                  if settings_tab.read().as_str() == "updates" {
                    div { class: "field",
                        span { class: "field-label", "Updates · current v{update::CURRENT}" }
                        div { class: "field-folder",
                            span { class: "folder-path", "{upd_status}" }
                            button { class: "ed-close", onclick: move |_| {
                                upd_status.set("Checking…".to_string());
                                let repo = gh_repo.read().clone();
                                let url = upd_url.read().clone();
                                spawn(async move {
                                    match update::check(&repo, &url).await {
                                        Some(info) => upd_status.set(format!("Update available · v{}", info.version)),
                                        None => upd_status.set("Up to date".to_string()),
                                    }
                                });
                            }, "Check for updates" }
                        }
                        span { class: "field-hint", "Checked automatically on startup; a banner appears when a newer release is available." }
                    }
                  }
                }
                div { class: "modal-foot",
                    button { class: "ed-close", onclick: move |_| on_close.call(()), "Cancel" }
                    button { class: "ed-save", onclick: move |e| save(e), "Save" }
                }
            }
        }
    }
}

#[component]
fn Composer(
    input: Signal<String>,
    streaming: Signal<bool>,
    engine: Coroutine<EngineCmd>,
    cfg: Signal<Config>,
    model_label: String,
    bypass: bool,
    project: String,
    branch: String,
    context_used: u64,
    context_limit: u64,
    workspace: PathBuf,
    plan_mode: Signal<bool>,
    pursue_goal: Signal<bool>,
    goal_text: Signal<String>,
    mentions: Signal<Vec<String>>,
    queue: Signal<Vec<String>>,
    mut picked_element: Signal<Option<String>>,
    on_settings: EventHandler<()>,
    on_open_folder: EventHandler<()>,
    on_pick_workspace: EventHandler<PathBuf>,
) -> Element {
    let mut show_proj = use_signal(|| false);
    let mut show_branch = use_signal(|| false);
    let recent = cfg.read().recent_workspaces.clone();
    let access_label = if bypass { "Full access" } else { "Ask first" };
    let mut plan_mode = plan_mode;
    let mut pursue_goal = pursue_goal;
    let mut mentions = mentions;
    let mut show_plus = use_signal(|| false);
    let mut show_access = use_signal(|| false);
    let mut mention_sel = use_signal(|| 0usize);
    // `@mention` picker driven by the contenteditable caret query.
    let mut mention_q = use_signal(|| None::<String>);
    let mut ce_empty = use_signal(|| true);
    // Pasted image attachments (data URLs), shown as preview cards.
    let mut attachments = use_signal(Vec::<String>::new);
    // Full-screen image preview (lightbox) when a thumbnail is clicked.
    let mut preview_img = use_signal(|| None::<String>);
    // Intercept image paste into the composer → attachment card (not inline).
    use_future(move || async move {
        let mut eval = dioxus::document::eval(
            r#"
            const el = document.getElementById('ce-input');
            if (el && !el.__oxpaste) {
              el.__oxpaste = true;
              el.addEventListener('paste', function(ev){
                const items = (ev.clipboardData || {}).items || [];
                for (const it of items) {
                  if (it.type && it.type.indexOf('image') === 0) {
                    ev.preventDefault();
                    const f = it.getAsFile();
                    const r = new FileReader();
                    r.onload = function(){ dioxus.send(r.result); };
                    r.readAsDataURL(f);
                  }
                }
              });
            }
            while (true) { await new Promise(r => setTimeout(r, 3600000)); }
            "#,
        );
        loop {
            match eval.recv::<String>().await {
                Ok(durl) => attachments.write().push(durl),
                Err(_) => break,
            }
        }
    });
    let mention_items: Vec<String> = match mention_q.read().as_ref() {
        Some(q) => all_mention_items(&workspace, q),
        None => Vec::new(),
    };
    let mention_at = if mention_q.read().is_some() { Some(0usize) } else { None };
    let msel = if mention_items.is_empty() {
        0
    } else {
        (*mention_sel.read()).min(mention_items.len() - 1)
    };
    // `/slash` command picker.
    let slash_items: Vec<(String, String)> = match active_slash(&input.read()) {
        Some(q) => slash_commands(&workspace, &q),
        None => Vec::new(),
    };
    let ws_kd = workspace.clone();
    // Context-usage ring (conic donut) shown in the composer toolbar.
    let ring_pct = if context_limit > 0 {
        (context_used as f64 / context_limit as f64 * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    let ring_style = format!(
        "background: conic-gradient(var(--accent) {p}%, #3a3a42 {p}% 100%)",
        p = ring_pct
    );
    let ring_num = format!("{}", ring_pct.round() as u64);
    let ring_title = if context_limit > 0 {
        format!(
            "{}% context used · {}k / {}k tokens",
            ring_pct.round() as u64,
            context_used / 1000,
            context_limit / 1000
        )
    } else {
        "context usage — send a message to populate".to_string()
    };
    let access_cls = if bypass {
        "pill access danger"
    } else {
        "pill access"
    };
    let mut input = input;
    let mut show_models = use_signal(|| false);
    let mut show_effort = use_signal(|| false);
    let mut model_query = use_signal(String::new);

    let cur_provider = cfg.read().provider.clone();
    let cur_model = cfg.read().model.clone();
    let cur_effort = cfg.read().reasoning_effort.clone();
    let fast_enabled = cfg.read().fast_mode;
    let pill_logo = provider_logo(&cur_provider);
    let query = model_query.read().trim().to_ascii_lowercase();
    let model_count = MODEL_PRESETS
        .iter()
        .filter(|preset| model_matches(preset, &query))
        .count();

    let ws_kd2 = workspace.clone();
    let ws_btn = workspace.clone();
    let ws_steer = workspace.clone();

    rsx! {
        div { class: if *streaming.read() { if cur_effort == "xhigh" { "composer working ultra" } else { "composer working" } } else { "composer" },
            if !slash_items.is_empty() {
                div { class: "mention-menu",
                    div { class: "menu-label", "Commands" }
                    for (name, desc) in slash_items.iter().cloned() {
                        {
                            let n = name.clone();
                            rsx! {
                                button { class: "menu-item",
                                    onclick: move |_| input.set(format!("/{n} ")),
                                    Icon { name: "spark" }
                                    span { class: "menu-name", "/{name}" }
                                    if !desc.is_empty() { span { class: "menu-meta", "{desc}" } }
                                }
                            }
                        }
                    }
                }
            }
            if let Some(at) = mention_at {
                if !mention_items.is_empty() {
                    div { class: "mention-menu",
                        for (i, path) in mention_items.iter().cloned().enumerate() {
                            {
                                let p_sel = path.clone();
                                let is_mcp = path.starts_with("mcp:");
                                let is_skill = path.starts_with("skill:");
                                let is_ctx = path.starts_with("ctx:");
                                let disp = path.strip_prefix("mcp:").or_else(|| path.strip_prefix("skill:")).or_else(|| path.strip_prefix("ctx:")).unwrap_or(&path).to_string();
                                let icon_name = if is_ctx { "branch" } else if is_mcp { "plugins" } else if is_skill { "target" } else if path.ends_with('/') { "folder" } else { "file" };
                                let grp = |p: &str| if p.starts_with("ctx:") { 0 } else if p.starts_with("mcp:") { 1 } else if p.starts_with("skill:") { 2 } else { 3 };
                                // Section header when the group changes.
                                let group = grp(&path);
                                let prev_group = if i == 0 { -1 } else { grp(&mention_items[i - 1]) };
                                let header = if group != prev_group {
                                    Some(match group { 0 => "Context", 1 => "MCP servers", 2 => "Skills", _ => "Files" })
                                } else { None };
                                let sel = i == msel;
                                rsx! {
                                    if let Some(h) = header { div { class: "menu-label", "{h}" } }
                                    button {
                                        class: if sel { "menu-item sel" } else { "menu-item" },
                                        onmouseenter: move |_| mention_sel.set(i),
                                        onclick: move |_| {
                                            let tok = p_sel.clone();
                                            let label = mention_label(&tok);
                                            spawn(async move { let _ = dioxus::document::eval(&ce_insert_js(&tok, &label)).join::<bool>().await; });
                                            mention_q.set(None);
                                            mention_sel.set(0);
                                            ce_empty.set(false);
                                        },
                                        if is_mcp { span { class: "nav-logo mcp-logo", dangerous_inner_html: provider_logo("mcp").unwrap_or_default() } }
                                        else { Icon { name: icon_name } }
                                        span { class: "menu-name", "{disp}" }
                                        if is_mcp { span { class: "menu-tag", "mcp" } }
                                        else if is_skill { span { class: "menu-tag", "skill" } }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if let Some(src) = preview_img.read().clone() {
                div { class: "img-lightbox", onclick: move |_| preview_img.set(None),
                    button { class: "img-lightbox-x", onclick: move |_| preview_img.set(None), "✕" }
                    img { class: "img-lightbox-img", src: "{src}", onclick: move |e| e.stop_propagation() }
                }
            }
            if !attachments.read().is_empty() {
                div { class: "attach-row",
                    for (i, src) in attachments.read().iter().cloned().enumerate() {
                        div { class: "attach-card",
                            img { src: "{src}", onclick: { let s = src.clone(); move |_| preview_img.set(Some(s.clone())) } }
                            button { class: "attach-x", onclick: move |_| { let mut v = attachments.write(); if i < v.len() { v.remove(i); } }, "✕" }
                        }
                    }
                }
            }
            if let Some(p) = picked_element.read().clone() {
                {
                    let label = p.lines().find_map(|l| l.strip_prefix("- selector: ")).unwrap_or("element").to_string();
                    rsx! {
                        div { class: "elem-chip", title: "{p}",
                            span { class: "elem-pin", "📍" }
                            span { class: "elem-sel", "{label}" }
                            span { class: "elem-note", "→ will be sent to change" }
                            button { class: "elem-x", onclick: move |_| picked_element.set(None), "✕" }
                        }
                    }
                }
            }
            div {
                class: "input ce-input",
                id: "ce-input",
                contenteditable: "true",
                "data-empty": "{ce_empty}",
                "data-ph": if *streaming.read() { "Steer the agent (sent mid-run)…" } else { "Do anything" },
                oninput: move |_| {
                    spawn(async move {
                        let j = dioxus::document::eval(CE_QUERY_JS).join::<String>().await.unwrap_or_default();
                        let v: serde_json::Value = serde_json::from_str(&j).unwrap_or(serde_json::Value::Null);
                        // Only write signals when the value actually changed —
                        // otherwise every keystroke re-renders and the caret jitters.
                        let new_q = v["q"].as_str().map(|s| s.to_string());
                        if *mention_q.read() != new_q {
                            mention_q.set(new_q);
                            mention_sel.set(0);
                        }
                        let new_empty = v["empty"].as_bool().unwrap_or(true);
                        if *ce_empty.read() != new_empty {
                            ce_empty.set(new_empty);
                        }
                    });
                },
                onkeydown: move |e| {
                    // When the @mention popup is open, the keyboard drives it.
                    let q = mention_q.read().clone();
                    if let Some(q) = q {
                        let items = all_mention_items(&ws_kd, &q);
                        if !items.is_empty() {
                            match e.key() {
                                Key::ArrowDown => { e.prevent_default(); let n = items.len(); let s = (*mention_sel.read() + 1) % n; mention_sel.set(s); return; }
                                Key::ArrowUp => { e.prevent_default(); let n = items.len(); let c = *mention_sel.read(); mention_sel.set((c + n - 1) % n); return; }
                                Key::Enter => {
                                    e.prevent_default();
                                    let s = (*mention_sel.read()).min(items.len() - 1);
                                    let tok = items[s].clone();
                                    let label = mention_label(&tok);
                                    spawn(async move { let _ = dioxus::document::eval(&ce_insert_js(&tok, &label)).join::<bool>().await; });
                                    mention_q.set(None);
                                    mention_sel.set(0);
                                    ce_empty.set(false);
                                    return;
                                }
                                Key::Escape => { e.prevent_default(); mention_q.set(None); return; }
                                _ => {}
                            }
                        }
                    }
                    if e.key() == Key::Enter && !e.modifiers().shift() {
                        e.prevent_default();
                        let ws = ws_kd2.clone();
                        spawn(async move { submit_ce(streaming, engine, plan_mode, pursue_goal, goal_text, queue, attachments, picked_element, false, ws).await; });
                    } else if e.key() == Key::Tab && e.modifiers().shift() {
                        e.prevent_default();
                        let v = *plan_mode.read();
                        plan_mode.set(!v);
                    }
                },
            }
            div { class: "toolbar",
                div { class: "toolbar-left",
                    div { class: "model-anchor",
                        button {
                            class: if *plan_mode.read() || *pursue_goal.read() { "round-btn on" } else { "round-btn" },
                            title: "More",
                            onclick: move |_| { let v = *show_plus.read(); show_plus.set(!v); },
                            "+"
                        }
                        if *show_plus.read() {
                            div { class: "menu-backdrop", onclick: move |_| show_plus.set(false) }
                            div { class: "plus-menu",
                                button { class: "plus-item",
                                    onclick: move |_| {
                                        show_plus.set(false);
                                        spawn(async move {
                                            if let Some(file) = rfd::AsyncFileDialog::new().pick_file().await {
                                                let tok = file.path().display().to_string();
                                                let label = mention_label(&tok);
                                                let js = format!(
                                                    "const ed=document.getElementById('ce-input'); if(ed){{ed.focus(); const c=document.createElement('span'); c.className='ce-chip'; c.setAttribute('contenteditable','false'); c.dataset.token={}; c.textContent={}; ed.appendChild(c); ed.appendChild(document.createTextNode(' '));}} return true;",
                                                    serde_json::to_string(&tok).unwrap(), serde_json::to_string(&label).unwrap()
                                                );
                                                let _ = dioxus::document::eval(&js).join::<bool>().await;
                                                ce_empty.set(false);
                                            }
                                        });
                                    },
                                    Icon { name: "paperclip" }
                                    span { class: "plus-name", "Add files & folders" }
                                }
                                div { class: "plus-divider" }
                                button { class: "plus-item",
                                    onclick: move |_| { let v = *plan_mode.read(); plan_mode.set(!v); },
                                    Icon { name: "list" }
                                    span { class: "plus-name", "Plan mode" }
                                    span { class: "plus-hint", "⇧⇥" }
                                    span { class: if *plan_mode.read() { "switch on" } else { "switch" }, span { class: "knob" } }
                                }
                                button { class: "plus-item",
                                    onclick: move |_| { let v = *pursue_goal.read(); pursue_goal.set(!v); },
                                    Icon { name: "target" }
                                    span { class: "plus-name", "Pursue goal" }
                                    span { class: if *pursue_goal.read() { "switch on" } else { "switch" }, span { class: "knob" } }
                                }
                                button { class: "plus-item",
                                    onclick: move |_| {
                                        let mut c = cfg.read().clone();
                                        c.orchestrate = !c.orchestrate;
                                        cfg.set(c.clone());
                                        let _ = engine.send(EngineCmd::Reconfigure(c));
                                    },
                                    Icon { name: "spark" }
                                    span { class: "plus-name", "Orchestrate" }
                                    span { class: "plus-hint", "plan→do→review" }
                                    span { class: if cfg.read().orchestrate { "switch on" } else { "switch" }, span { class: "knob" } }
                                }
                            }
                        }
                    }
                    if *plan_mode.read() {
                        span { class: "pill plan", Icon { name: "list" } "Plan" }
                    }
                    div { class: "access-anchor",
                        button { class: "{access_cls}", onclick: move |_| { let v = *show_access.read(); show_access.set(!v); },
                            Icon { name: "shield" } "{access_label}"
                        }
                        if *show_access.read() {
                            div { class: "menu-backdrop", onclick: move |_| show_access.set(false) }
                            {
                                let ap = cfg.read().approval_policy;
                                rsx! {
                                    div { class: "access-menu",
                                        div { class: "menu-label", "How should actions be approved?" }
                                        button { class: "menu-item", onclick: move |_| set_access_mode(cfg, engine, show_access, ApprovalPolicy::Always, SandboxPolicy::WorkspaceWrite),
                                            Icon { name: "shield" }
                                            span { class: "menu-copy", span { class: "menu-name", "Ask for approval" } span { class: "menu-meta", "Always ask before edits and network" } }
                                            if matches!(ap, ApprovalPolicy::Always) { span { class: "menu-check", "✓" } }
                                        }
                                        button { class: "menu-item", onclick: move |_| set_access_mode(cfg, engine, show_access, ApprovalPolicy::OnRequest, SandboxPolicy::WorkspaceWrite),
                                            Icon { name: "terminal" }
                                            span { class: "menu-copy", span { class: "menu-name", "Approve for me" } span { class: "menu-meta", "Auto-run safe; ask for risky actions" } }
                                            if matches!(ap, ApprovalPolicy::OnRequest) { span { class: "menu-check", "✓" } }
                                        }
                                        button { class: "menu-item", onclick: move |_| set_access_mode(cfg, engine, show_access, ApprovalPolicy::Never, SandboxPolicy::DangerFullAccess),
                                            Icon { name: "zap" }
                                            span { class: "menu-copy", span { class: "menu-name", "Full access" } span { class: "menu-meta", "Unrestricted files + network (yolo)" } }
                                            if matches!(ap, ApprovalPolicy::Never) { span { class: "menu-check", "✓" } }
                                        }
                                        div { class: "plus-divider" }
                                        button { class: "menu-item", onclick: move |_| { show_access.set(false); on_settings.call(()); },
                                            Icon { name: "settings" }
                                            span { class: "menu-copy", span { class: "menu-name", "Settings…" } span { class: "menu-meta", "Workspace, model, harness, more" } }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    button {
                        class: if fast_enabled { "pill fast on" } else { "pill fast" },
                        onclick: move |_| {
                            // Fast (speed) is independent of reasoning effort — you can
                            // run Fast + High together, like Codex/ChatGPT.
                            let mut c = cfg.read().clone();
                            c.fast_mode = !c.fast_mode;
                            if c.fast_mode {
                                if let Some(preset) = fast_model_for(&c.provider) {
                                    c.model = preset.model.to_string();
                                }
                            }
                            cfg.set(c.clone());
                            let _ = engine.send(EngineCmd::Reconfigure(c));
                        },
                        Icon { name: "zap" } "Fast"
                    }
                }
                div { class: "toolbar-right",
                    div { class: "model-anchor",
                        button {
                            class: "pill model",
                            onclick: move |_| {
                                let v = *show_models.read();
                                show_models.set(!v);
                                show_effort.set(false);
                            },
                            if let Some(svg) = pill_logo.clone() {
                                span { class: "prov-logo", dangerous_inner_html: svg }
                            } else {
                                Icon { name: "spark" }
                            }
                            "{model_label}"
                            span { class: "chev", Icon { name: "chevron" } }
                        }
                        if *show_models.read() {
                            div { class: "menu-backdrop", onclick: move |_| show_models.set(false) }
                            div { class: "model-menu",
                                div { class: "menu-search",
                                    Icon { name: "search" }
                                    input {
                                        class: "model-search",
                                        placeholder: "Search model, provider, smart, fast...",
                                        value: "{model_query}",
                                        oninput: move |e| model_query.set(e.value()),
                                    }
                                }
                                div { class: "menu-label", "Recommended models" }
                                if model_count == 0 {
                                    div { class: "menu-empty", "No matching model" }
                                }
                                for preset in MODEL_PRESETS.iter().filter(|preset| model_matches(preset, &query)) {
                                    {
                                        let selected = preset.provider == cur_provider && preset.model == cur_model;
                                        let logo = provider_logo(preset.provider);
                                        let prov = preset.provider.to_string();
                                        let model = preset.model.to_string();
                                        let is_fast = preset.fast;
                                        rsx! {
                                            button {
                                                class: if selected { "menu-item sel" } else { "menu-item" },
                                                onclick: move |_| {
                                                    // Keep the user's chosen effort + fast toggle on model switch.
                                                    let _ = is_fast;
                                                    let mut c = cfg.read().clone();
                                                    c.provider = prov.clone();
                                                    c.model = model.clone();
                                                    cfg.set(c.clone());
                                                    let _ = engine.send(EngineCmd::Reconfigure(c));
                                                    show_models.set(false);
                                                },
                                                if let Some(svg) = logo {
                                                    span { class: "prov-logo", dangerous_inner_html: svg }
                                                } else {
                                                    span { class: "prov-logo dot" }
                                                }
                                                span { class: "menu-copy",
                                                    span { class: "menu-name", "{preset.provider_label} · {preset.label}" }
                                                    span { class: "menu-meta", "{preset.model} · {preset.summary}" }
                                                }
                                                span { class: if preset.fast { "menu-badge fast" } else { "menu-badge" }, "{preset.badge}" }
                                                if selected { span { class: "menu-check", "✓" } }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    div { class: "model-anchor",
                        button {
                            class: "pill effort",
                            onclick: move |_| {
                                let v = *show_effort.read();
                                show_effort.set(!v);
                                show_models.set(false);
                            },
                            Icon { name: "brain" }
                            "{effort_label(&cur_effort)}"
                            span { class: "chev", Icon { name: "chevron" } }
                        }
                        if *show_effort.read() {
                            div { class: "menu-backdrop", onclick: move |_| show_effort.set(false) }
                            div { class: "effort-menu",
                                div { class: "menu-label", "Effort" }
                                for preset in EFFORT_PRESETS.iter() {
                                    {
                                        let selected = preset.value == cur_effort;
                                        let value = preset.value.to_string();
                                        rsx! {
                                            button {
                                                class: if selected { "menu-item sel" } else { "menu-item" },
                                                onclick: move |_| {
                                                    // Effort is independent of Fast — don't disable Fast here.
                                                    let mut c = cfg.read().clone();
                                                    c.reasoning_effort = value.clone();
                                                    cfg.set(c.clone());
                                                    let _ = engine.send(EngineCmd::Reconfigure(c));
                                                    show_effort.set(false);
                                                },
                                                Icon { name: "brain" }
                                                span { class: "menu-copy",
                                                    span { class: "menu-name", "{preset.label}" }
                                                    span { class: "menu-meta", "{preset.summary}" }
                                                }
                                                if selected { span { class: "menu-check", "✓" } }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    div { class: "usage-ring", title: "{ring_title}",
                        div { class: "ring", style: "{ring_style}",
                            div { class: "ring-hole", "{ring_num}" }
                        }
                    }
                    if *streaming.read() {
                        button { class: "send steer", title: "Steer (inject into the running turn)", onclick: move |_| { let ws = ws_steer.clone(); spawn(async move { submit_ce(streaming, engine, plan_mode, pursue_goal, goal_text, queue, attachments, picked_element, true, ws).await; }); }, "↪" }
                        button { class: "send stop", title: "Stop", onclick: move |_| { let _ = engine.send(EngineCmd::Interrupt); }, "■" }
                    } else {
                        button { class: "send", onclick: move |_| { let ws = ws_btn.clone(); spawn(async move { submit_ce(streaming, engine, plan_mode, pursue_goal, goal_text, queue, attachments, picked_element, false, ws).await; }); }, "↑" }
                    }
                }
            }
        }
        div { class: "selectors",
            div { class: "sel-anchor",
                button { class: "selector", onclick: move |_| { let v = *show_proj.read(); show_proj.set(!v); show_branch.set(false); },
                    Icon { name: "folder" } "{project}" span { class: "chev", Icon { name: "chevron" } }
                }
                if *show_proj.read() {
                    div { class: "menu-backdrop", onclick: move |_| show_proj.set(false) }
                    div { class: "sel-menu",
                        div { class: "menu-label", "Recent folders" }
                        if recent.is_empty() {
                            div { class: "insp-empty", "No recent folders yet." }
                        }
                        for p in recent.iter().cloned() {
                            {
                                let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| p.display().to_string());
                                let full = p.display().to_string();
                                rsx! {
                                    button { class: "menu-item", onclick: move |_| { on_pick_workspace.call(p.clone()); show_proj.set(false); },
                                        Icon { name: "folder" }
                                        span { class: "menu-copy", span { class: "menu-name", "{name}" } span { class: "menu-meta", "{full}" } }
                                    }
                                }
                            }
                        }
                        div { class: "plus-divider" }
                        button { class: "menu-item", onclick: move |_| { show_proj.set(false); on_open_folder.call(()); },
                            Icon { name: "plus" } span { class: "menu-name", "Open folder…" }
                        }
                    }
                }
            }
            div { class: "sel-anchor",
                button { class: "selector", onclick: move |_| { let v = *show_branch.read(); show_branch.set(!v); show_proj.set(false); },
                    Icon { name: "branch" } "{branch}" span { class: "chev", Icon { name: "chevron" } }
                }
                if *show_branch.read() {
                    div { class: "menu-backdrop", onclick: move |_| show_branch.set(false) }
                    {
                        let worktrees = git_worktrees(&workspace);
                        let branches = git_branches(&workspace);
                        let ws_branch = workspace.clone();
                        rsx! {
                            div { class: "sel-menu",
                                if !worktrees.is_empty() {
                                    div { class: "menu-label", "Worktrees" }
                                    for (wp, wb) in worktrees {
                                        {
                                            let name = wp.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                                            rsx! {
                                                button { class: "menu-item", onclick: move |_| { on_pick_workspace.call(wp.clone()); show_branch.set(false); },
                                                    Icon { name: "branch" }
                                                    span { class: "menu-copy", span { class: "menu-name", "{name}" } span { class: "menu-meta", "{wb}" } }
                                                }
                                            }
                                        }
                                    }
                                    div { class: "plus-divider" }
                                }
                                div { class: "menu-label", "Branches" }
                                for b in branches {
                                    {
                                        let bb = b.clone();
                                        let ws = ws_branch.clone();
                                        rsx! {
                                            button { class: "menu-item", onclick: move |_| {
                                                let _ = std::process::Command::new("git").args(["switch", &bb]).current_dir(&ws).output();
                                                show_branch.set(false);
                                            },
                                                Icon { name: "branch" } span { class: "menu-name", "{b}" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn Message(author: Author, text: String) -> Element {
    match author {
        Author::User => {
            let segs = user_segments(&text);
            let copy = serde_json::to_string(&text).unwrap_or_default();
            rsx! {
                div { class: "row user",
                    div { class: "bubble",
                        for (is_m, s) in segs {
                            if is_m { span { class: "inline-chip", "{s}" } } else { "{s}" }
                        }
                    }
                    button { class: "msg-copy", title: "Copy message", onclick: move |_| { let c = copy.clone(); spawn(async move { let _ = document::eval(&format!("navigator.clipboard.writeText({c})")).await; }); }, "⧉" }
                }
            }
        }
        Author::Agent => {
            let copy = serde_json::to_string(&text).unwrap_or_default();
            rsx! {
                div { class: "row agent",
                    img { class: "avatar", src: logo_uri() }
                    if text.is_empty() {
                        div { class: "typing", span {} span {} span {} }
                    } else {
                        div { class: "agent-text", "{text}" }
                        button { class: "msg-copy", title: "Copy message", onclick: move |_| { let c = copy.clone(); spawn(async move { let _ = document::eval(&format!("navigator.clipboard.writeText({c})")).await; }); }, "⧉" }
                    }
                }
            }
        },
        Author::Activity { running, ok } => rsx! { ActivityRow { text, running, ok } },
        Author::Diff(..) => rsx! {},
        Author::Note => {
            let is_cmd = text.starts_with('⌘') || text.starts_with('✎') || text.starts_with('🔎') || text.starts_with('⚙');
            if is_cmd {
                rsx! { div { class: "row tool", pre { class: "tool-card", "{text}" } } }
            } else {
                rsx! { div { class: "row note", div { class: "note-text", "{text}" } } }
            }
        }
    }
}

/// Embedded interactive terminal: runs `bin` in a PTY and bridges it to an
/// xterm.js instance in the webview via Dioxus eval.
#[component]
fn TerminalView(id: u64, bin: String, ws: String) -> Element {
    let host = format!("term-{id}");
    let host_js = host.clone();
    use_future(move || {
        let host = host_js.clone();
        let bin = bin.clone();
        let ws = ws.clone();
        async move {
            let setup = format!(
                r##"
                for (let i = 0; i < 300 && !window.Terminal; i++) {{ await new Promise(r => setTimeout(r, 20)); }}
                const el = document.getElementById("{host}");
                if (!el || !window.Terminal) return;
                el.innerHTML = "";
                const term = new window.Terminal({{ fontSize: 12.5, fontFamily: "'MesloLGS NF', 'JetBrainsMono Nerd Font', 'JetBrainsMono Nerd Font Mono', 'Hack Nerd Font', 'FiraCode Nerd Font', 'CaskaydiaCove Nerd Font', 'Symbols Nerd Font Mono', 'Symbols Nerd Font', ui-monospace, Menlo, monospace", cursorBlink: true, theme: {{ background: "#0e0e10", foreground: "#cdd0d6" }} }});
                let fit = null;
                try {{ fit = new window.FitAddon.FitAddon(); term.loadAddon(fit); }} catch (e) {{}}
                try {{ if (document.fonts && document.fonts.ready) await document.fonts.ready; }} catch (e) {{}}
                term.open(el);
                try {{ if (fit) fit.fit(); }} catch (e) {{}}
                term.focus();
                term.onData(d => dioxus.send(JSON.stringify({{ inp: d }})));
                const ro = new ResizeObserver(() => {{ try {{ if (fit) fit.fit(); dioxus.send(JSON.stringify({{ resize: [term.rows, term.cols] }})); }} catch (e) {{}} }});
                ro.observe(el);
                dioxus.send(JSON.stringify({{ resize: [term.rows, term.cols] }}));
                (async () => {{ while (true) {{ const m = await dioxus.recv(); if (typeof m === "string" && m.length) {{ term.write(Uint8Array.from(atob(m), c => c.charCodeAt(0))); }} }} }})();
            "##
            );
            // Inject the xterm runtime inline (asset!() isn't served under plain `cargo run`).
            let setup = format!("{XTERM_JS}\n;\n{XTERM_FIT_JS}\n;\n{setup}");
            let mut eval = dioxus::document::eval(&setup);

            let pty = portable_pty::native_pty_system();
            let pair = match pty.openpty(portable_pty::PtySize { rows: 32, cols: 110, pixel_width: 0, pixel_height: 0 }) {
                Ok(p) => p,
                Err(_) => return,
            };
            let mut cmd = portable_pty::CommandBuilder::new(&bin);
            // Launch the agent CLIs with permissions bypassed (yolo), like the rest of Oxide.
            match bin.as_str() {
                "codex" => cmd.arg("--dangerously-bypass-approvals-and-sandbox"),
                "claude" => cmd.arg("--dangerously-skip-permissions"),
                _ => {}
            }
            cmd.cwd(&ws);
            cmd.env("TERM", "xterm-256color");
            if let Ok(home) = std::env::var("HOME") {
                let path = std::env::var("PATH").unwrap_or_default();
                cmd.env("PATH", format!("{home}/.superconductor/bin:{home}/.local/bin:{home}/.bun/bin:/opt/homebrew/bin:/usr/local/bin:{path}"));
            }
            let mut child = match pair.slave.spawn_command(cmd) {
                Ok(c) => c,
                Err(_) => return,
            };
            drop(pair.slave);
            let mut reader = match pair.master.try_clone_reader() {
                Ok(r) => r,
                Err(_) => return,
            };
            let mut writer = match pair.master.take_writer() {
                Ok(w) => w,
                Err(_) => return,
            };
            let master = pair.master;

            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
            std::thread::spawn(move || {
                use std::io::Read;
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if tx.send(buf[..n].to_vec()).is_err() {
                                break;
                            }
                        }
                    }
                }
            });

            use base64::Engine;
            use std::io::Write;
            loop {
                tokio::select! {
                    bytes = rx.recv() => match bytes {
                        Some(bytes) => {
                            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                            if eval.send(serde_json::Value::String(b64)).is_err() { break; }
                        }
                        None => break,
                    },
                    msg = eval.recv::<String>() => match msg {
                        Ok(s) => {
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                                if let Some(inp) = v.get("inp").and_then(|x| x.as_str()) {
                                    let _ = writer.write_all(inp.as_bytes());
                                    let _ = writer.flush();
                                } else if let Some(rc) = v.get("resize").and_then(|x| x.as_array()) {
                                    let rows = rc.first().and_then(|x| x.as_u64()).unwrap_or(32) as u16;
                                    let cols = rc.get(1).and_then(|x| x.as_u64()).unwrap_or(110) as u16;
                                    let _ = master.resize(portable_pty::PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
                                }
                            }
                        }
                        Err(_) => break,
                    },
                }
            }
            let _ = child.kill();
        }
    });
    rsx! { div { id: "{host}", class: "xterm-host" } }
}

/// Commands into a ChatPane's own engine.
enum PaneCmd {
    Submit(String),
    Interrupt,
}

/// A tiling layout node: a leaf pane (by id) or a split of two nodes.
#[derive(Clone, PartialEq)]
enum Tile {
    Leaf(u64),
    Split { id: u64, vertical: bool, ratio: f64, a: Box<Tile>, b: Box<Tile> },
}

/// Replace leaf `target` with a split containing it plus a new leaf `new_pane`.
fn tile_split(node: &Tile, target: u64, vertical: bool, split_id: u64, new_pane: u64) -> Tile {
    match node {
        Tile::Leaf(id) if *id == target => Tile::Split {
            id: split_id,
            vertical,
            ratio: 0.5,
            a: Box::new(Tile::Leaf(*id)),
            b: Box::new(Tile::Leaf(new_pane)),
        },
        Tile::Leaf(id) => Tile::Leaf(*id),
        Tile::Split { id, vertical: v, ratio, a, b } => Tile::Split {
            id: *id,
            vertical: *v,
            ratio: *ratio,
            a: Box::new(tile_split(a, target, vertical, split_id, new_pane)),
            b: Box::new(tile_split(b, target, vertical, split_id, new_pane)),
        },
    }
}

/// Remove leaf `target`, collapsing its split. Returns None if the tree becomes empty.
fn tile_close(node: &Tile, target: u64) -> Option<Tile> {
    match node {
        Tile::Leaf(id) if *id == target => None,
        Tile::Leaf(id) => Some(Tile::Leaf(*id)),
        Tile::Split { id, vertical, ratio, a, b } => {
            match (tile_close(a, target), tile_close(b, target)) {
                (None, Some(x)) | (Some(x), None) => Some(x),
                (Some(a), Some(b)) => Some(Tile::Split {
                    id: *id,
                    vertical: *vertical,
                    ratio: *ratio,
                    a: Box::new(a),
                    b: Box::new(b),
                }),
                (None, None) => None,
            }
        }
    }
}

/// Set the ratio of split `split_id`.
fn tile_set_ratio(node: &Tile, split_id: u64, ratio: f64) -> Tile {
    match node {
        Tile::Leaf(id) => Tile::Leaf(*id),
        Tile::Split { id, vertical, ratio: r, a, b } => Tile::Split {
            id: *id,
            vertical: *vertical,
            ratio: if *id == split_id { ratio.clamp(0.12, 0.88) } else { *r },
            a: Box::new(tile_set_ratio(a, split_id, ratio)),
            b: Box::new(tile_set_ratio(b, split_id, ratio)),
        },
    }
}

/// Collect leaf ids in order.
fn tile_leaves(node: &Tile, out: &mut Vec<u64>) {
    match node {
        Tile::Leaf(id) => out.push(*id),
        Tile::Split { a, b, .. } => { tile_leaves(a, out); tile_leaves(b, out); }
    }
}

#[component]
fn ActivityRow(text: String, running: bool, ok: bool) -> Element {
    let cls = if running { "activity-card running" } else if ok { "activity-card done" } else { "activity-card fail" };
    // text is "icon\tverb\tdetail[\toutput]"
    let mut parts = text.splitn(4, '\t');
    let icon = parts.next().unwrap_or("spark").to_string();
    let verb = parts.next().unwrap_or("").to_string();
    let detail = parts.next().unwrap_or("").to_string();
    let output = parts.next().unwrap_or("").to_string();
    let lines = if output.is_empty() { 0 } else { output.lines().count() };
    rsx! {
        div { class: "row activity",
            if output.is_empty() {
                div { class: "{cls}",
                    span { class: "activity-tic", Icon { name: icon_static(&icon) } }
                    if running { span { class: "activity-spin" } }
                    else if ok { span { class: "activity-ic ok", "✓" } }
                    else { span { class: "activity-ic fail", "✕" } }
                    span { class: "activity-verb", "{verb}" }
                    if !detail.is_empty() { span { class: "activity-text", "{detail}" } }
                }
            } else {
                details { class: "{cls} has-out",
                    summary { class: "activity-sum",
                        span { class: "activity-tic", Icon { name: icon_static(&icon) } }
                        if ok { span { class: "activity-ic ok", "✓" } } else { span { class: "activity-ic fail", "✕" } }
                        span { class: "activity-verb", "{verb}" }
                        if !detail.is_empty() { span { class: "activity-text", "{detail}" } }
                        span { class: "activity-out-n", "{lines} lines" }
                        button { class: "copy-btn", title: "Copy output",
                            onclick: {
                                let out = output.clone();
                                move |e: dioxus::prelude::MouseEvent| {
                                    e.stop_propagation();
                                    let js = format!("navigator.clipboard.writeText({});", serde_json::to_string(&out).unwrap_or_default());
                                    let _ = dioxus::document::eval(&js);
                                }
                            },
                            "⧉"
                        }
                    }
                    pre { class: "activity-out", "{output}" }
                }
            }
        }
    }
}

/// Map a dynamic icon key to the static name the Icon component expects.
fn icon_static(key: &str) -> &'static str {
    match key {
        "terminal" => "terminal",
        "edit" => "edit",
        "file" => "file",
        "search" => "search",
        "globe" => "globe",
        "brain" => "brain",
        _ => "spark",
    }
}

/// Recursive tiling view: renders the layout tree as live `ChatPane`s with
/// draggable split dividers.
#[component]
fn SplitView(
    node: Tile,
    workspace: PathBuf,
    panes: Signal<Vec<(u64, String, String, String)>>,
    layout: Signal<Tile>,
    next_id: Signal<u64>,
    drag: Signal<Option<u64>>,
    rects: Signal<std::collections::HashMap<u64, (f64, f64, f64, f64)>>,
    def_provider: String,
    def_model: String,
) -> Element {
    match node {
        Tile::Leaf(pid) => {
            let (mode, target, model) = panes
                .read()
                .iter()
                .find(|p| p.0 == pid)
                .map(|p| (p.1.clone(), p.2.clone(), p.3.clone()))
                .unwrap_or_else(|| ("gui".to_string(), def_provider.clone(), def_model.clone()));
            let closable = {
                let mut l = Vec::new();
                tile_leaves(&layout.read(), &mut l);
                l.len() > 1
            };
            // New panes inherit the current pane's mode/target so a Claude tile
            // splits into more Claude tiles.
            let (im, it, imod) = (mode.clone(), target.clone(), model.clone());
            let ws_close = workspace.clone();
            rsx! {
                SplitLeaf {
                    pane_id: pid,
                    workspace: workspace.clone(),
                    mode: mode.clone(),
                    target: target.clone(),
                    model: model.clone(),
                    closable,
                    on_split: move |vertical: bool| {
                        let base = *next_id.read();
                        next_id.set(base + 2);
                        panes.write().push((base, im.clone(), it.clone(), imod.clone()));
                        let new_layout = tile_split(&layout.read(), pid, vertical, base + 1, base);
                        layout.set(new_layout);
                    },
                    on_close: move |_| {
                        let closed = tile_close(&layout.read(), pid);
                        if let Some(t) = closed {
                            layout.set(t);
                        }
                        panes.write().retain(|p| p.0 != pid);
                        if pid != 0 {
                            remove_pane_worktree(&ws_close, pid);
                        }
                    },
                    on_set_mode: move |(m, t): (String, String)| {
                        let mut ps = panes.write();
                        if let Some(p) = ps.iter_mut().find(|p| p.0 == pid) { p.1 = m; p.2 = t; }
                    },
                }
            }
        }
        Tile::Split { id, vertical, ratio, a, b } => {
            let na = *a;
            let nb = *b;
            let cls = if vertical { "split split-row" } else { "split split-col" };
            rsx! {
                div {
                    class: "{cls}",
                    onmounted: move |e| {
                        async move {
                            if let Ok(r) = e.get_client_rect().await {
                                rects.write().insert(id, (r.origin.x, r.origin.y, r.size.width, r.size.height));
                            }
                        }
                    },
                    onmousemove: move |e| {
                        if *drag.read() == Some(id) {
                            if let Some(&(x, y, w, h)) = rects.read().get(&id) {
                                let c = e.client_coordinates();
                                let ratio = if vertical { (c.x - x) / w.max(1.0) } else { (c.y - y) / h.max(1.0) };
                                let nl = tile_set_ratio(&layout.read(), id, ratio);
                                layout.set(nl);
                            }
                        }
                    },
                    onmouseup: move |_| drag.set(None),
                    div { class: "split-cell", style: "flex: {ratio}",
                        SplitView { node: na, workspace: workspace.clone(), panes, layout, next_id, drag, rects, def_provider: def_provider.clone(), def_model: def_model.clone() }
                    }
                    div { class: if vertical { "split-divider vert" } else { "split-divider horz" },
                        onmousedown: move |_| drag.set(Some(id)),
                    }
                    div { class: "split-cell", style: "flex: {1.0 - ratio}",
                        SplitView { node: nb, workspace: workspace.clone(), panes, layout, next_id, drag, rects, def_provider: def_provider.clone(), def_model: def_model.clone() }
                    }
                }
            }
        }
    }
}

/// Leaf wrapper: header (split/close/mode) + a GUI chat pane or an embedded TUI.
#[component]
fn SplitLeaf(
    pane_id: u64,
    workspace: PathBuf,
    mode: String,
    target: String,
    model: String,
    closable: bool,
    on_split: EventHandler<bool>,
    on_close: EventHandler<()>,
    on_set_mode: EventHandler<(String, String)>,
) -> Element {
    let mut show_menu = use_signal(|| false);
    let is_tui = mode == "tui";
    let label = if is_tui { format!("{target} · TUI") } else { target.clone() };
    rsx! {
        div { class: "pane", key: "pane{pane_id}-{mode}-{target}",
            div { class: "pane-head",
                div { class: "pane-mode-anchor",
                    button { class: "pane-label", title: "Change pane type", onclick: move |_| { let v = *show_menu.read(); show_menu.set(!v); },
                        if let Some(l) = provider_logo(&target) { span { class: "agent-tab-logo prov-logo", dangerous_inner_html: l } }
                        span { class: "pane-title", "{label}" }
                        span { class: "pane-caret", Icon { name: "chevron" } }
                    }
                    if *show_menu.read() {
                        div { class: "menu-backdrop", onclick: move |_| show_menu.set(false) }
                        div { class: "pane-mode-menu",
                            button { class: "menu-item", onclick: move |_| { show_menu.set(false); on_set_mode.call(("gui".into(), "chatgpt".into())); },
                                if let Some(l) = provider_logo("chatgpt") { span { class: "agent-tab-logo prov-logo", dangerous_inner_html: l } }
                                span { class: "menu-name", "GUI · ChatGPT" }
                            }
                            button { class: "menu-item", onclick: move |_| { show_menu.set(false); on_set_mode.call(("tui".into(), "codex".into())); },
                                if let Some(l) = provider_logo("codex") { span { class: "agent-tab-logo prov-logo", dangerous_inner_html: l } }
                                span { class: "menu-name", "TUI · Codex" }
                            }
                            button { class: "menu-item", onclick: move |_| { show_menu.set(false); on_set_mode.call(("tui".into(), "claude".into())); },
                                if let Some(l) = provider_logo("claude") { span { class: "agent-tab-logo prov-logo", dangerous_inner_html: l } }
                                span { class: "menu-name", "TUI · Claude Code" }
                            }
                        }
                    }
                }
                div { class: "pane-actions",
                    button { class: "pane-btn", title: "Split right", onclick: move |_| on_split.call(true), "⊞" }
                    button { class: "pane-btn", title: "Split down", onclick: move |_| on_split.call(false), "⊟" }
                    if closable {
                        button { class: "pane-btn", title: "Close pane", onclick: move |_| on_close.call(()), "✕" }
                    }
                }
            }
            if is_tui {
                TerminalView { id: pane_id, bin: target.clone(), ws: workspace.display().to_string() }
            } else {
                ChatPane { pane_id, workspace: workspace.clone(), provider: target.clone(), model: model.clone(), isolate: pane_id != 0 }
            }
        }
    }
}

/// Create (or reuse) an isolated git worktree for a split pane. Returns None if
/// `ws` isn't a git repo (caller then shares the main workspace).
async fn pane_worktree(ws: &Path, id: u64) -> Option<PathBuf> {
    let is_git = tokio::process::Command::new("git")
        .arg("-C").arg(ws).args(["rev-parse", "--is-inside-work-tree"])
        .output().await.ok().map(|o| o.status.success()).unwrap_or(false);
    if !is_git {
        return None;
    }
    let wt = ws.join(".oxide/worktrees").join(format!("pane-{id}"));
    if wt.exists() {
        return Some(wt);
    }
    let _ = std::fs::create_dir_all(ws.join(".oxide/worktrees"));
    let branch = format!("oxide/pane-{id}");
    let _ = tokio::process::Command::new("git")
        .arg("-C").arg(ws).args(["worktree", "add", "-B", &branch])
        .arg(&wt).arg("HEAD")
        .output().await;
    wt.exists().then_some(wt)
}

/// Remove a pane's worktree (best-effort) when the pane closes.
fn remove_pane_worktree(ws: &Path, id: u64) {
    let wt = ws.join(".oxide/worktrees").join(format!("pane-{id}"));
    if !wt.exists() {
        return;
    }
    let ws = ws.to_path_buf();
    spawn(async move {
        let _ = tokio::process::Command::new("git")
            .arg("-C").arg(&ws).args(["worktree", "remove", "--force"]).arg(&wt)
            .output().await;
        let _ = tokio::process::Command::new("git")
            .arg("-C").arg(&ws).args(["branch", "-D", &format!("oxide/pane-{id}")])
            .output().await;
    });
}

/// Picture-in-Picture: a separate always-on-top mini window holding one live
/// chat (its own engine). The main app stays open and full-size.
#[component]
fn PipWindow(
    workspace: PathBuf,
    mode: String,
    provider: String,
    model: String,
    bin: String,
    theme: String,
    initial: Vec<ChatMsg>,
) -> Element {
    rsx! {
        style { {CSS} }
        style { {XTERM_CSS} }
        div { class: "app pip-win", "data-theme": "{theme}",
            if mode == "tui" {
                TerminalView { id: 990_001, bin: bin.clone(), ws: workspace.display().to_string() }
            } else {
                ChatPane { pane_id: 990_001, workspace, provider, model, initial }
            }
        }
    }
}

/// A self-contained live chat pane: its own engine, transcript, and composer.
/// The surrounding header (split/close/mode) is provided by `SplitLeaf`.
#[component]
fn ChatPane(
    pane_id: u64,
    workspace: PathBuf,
    provider: String,
    model: String,
    #[props(default)] initial: Vec<ChatMsg>,
    #[props(default)] isolate: bool,
) -> Element {
    let mut messages = use_signal(move || initial.clone());
    let mut input = use_signal(String::new);
    let mut streaming = use_signal(|| false);
    let mut thinking = use_signal(String::new);
    let mut status = use_signal(String::new);

    let p0 = provider.clone();
    let m0 = model.clone();
    let w0 = workspace.clone();
    let pane = use_coroutine(move |mut rx: UnboundedReceiver<PaneCmd>| {
        let (p, m, w) = (p0.clone(), m0.clone(), w0.clone());
        async move {
            let (ev_tx, mut ev_rx) = tokio::sync::mpsc::channel::<Event>(256);
            let mut cfg = Config::load().unwrap_or_default();
            // Isolate non-primary panes in their own git worktree so parallel
            // agents never clobber each other's working tree.
            let ws_eff = if isolate {
                pane_worktree(&w, pane_id).await.unwrap_or_else(|| w.clone())
            } else {
                w.clone()
            };
            cfg.workspace = Some(ws_eff);
            cfg.provider = p;
            cfg.model = m;
            cfg.approval_policy = oxide_protocol::ApprovalPolicy::Never;
            cfg.persist = true;
            cfg.resume = false;
            cfg.orchestrate = false;
            cfg.subagents = false;
            let handle = match oxide_core::spawn(cfg) {
                Ok((h, mut events)) => {
                    let tx = ev_tx.clone();
                    tokio::spawn(async move {
                        while let Some(e) = events.recv().await {
                            if tx.send(e).await.is_err() {
                                break;
                            }
                        }
                    });
                    h
                }
                Err(_) => return,
            };
            loop {
                tokio::select! {
                    cmd = rx.next() => match cmd {
                        Some(PaneCmd::Submit(t)) => {
                            messages.write().push(ChatMsg { author: Author::User, text: t.clone() });
                            messages.write().push(ChatMsg { author: Author::Agent, text: String::new() });
                            streaming.set(true);
                            let _ = handle.submit(Op::UserTurn { text: t }).await;
                        }
                        Some(PaneCmd::Interrupt) => { let _ = handle.submit(Op::Interrupt).await; streaming.set(false); }
                        None => break,
                    },
                    ev = ev_rx.recv() => match ev {
                        Some(Event::AgentMessageDelta { text, .. }) => {
                            let mut mm = messages.write();
                            match mm.last_mut() {
                                Some(l) if l.author == Author::Agent => l.text.push_str(&text),
                                _ => mm.push(ChatMsg { author: Author::Agent, text }),
                            }
                        }
                        Some(Event::ReasoningDelta { text, .. }) => { thinking.write().push_str(&text); }
                        Some(Event::ToolCallBegin { tool, args, .. }) => {
                            if tool != "ask_user" {
                                messages.write().push(ChatMsg { author: Author::Activity { running: true, ok: true }, text: activity_label(&tool, &args) });
                            }
                        }
                        Some(Event::ToolCallEnd { output, ok, .. }) => {
                            let mut out = output.trim().to_string();
                            if out.chars().count() > 4000 { out = out.chars().take(4000).collect::<String>() + "\n… (truncated)"; }
                            let mut mm = messages.write();
                            if let Some(c) = mm.iter_mut().rev().find(|c| matches!(c.author, Author::Activity { running: true, .. })) {
                                c.author = Author::Activity { running: false, ok };
                                if !out.is_empty() { c.text.push('\t'); c.text.push_str(&out); }
                            }
                        }
                        Some(Event::FileDiff { path, diff, checkpoint, .. }) => { messages.write().push(ChatMsg { author: Author::Diff(path, checkpoint), text: diff }); }
                        Some(Event::TurnStarted { .. }) => { thinking.set(String::new()); status.set("Working…".to_string()); }
                        Some(Event::TurnFinished { .. }) => { streaming.set(false); status.set(String::new()); { let mut mm = messages.write(); for c in mm.iter_mut() { if let Author::Activity { running, .. } = &mut c.author { *running = false; } } } }
                        Some(Event::Info { text }) => { if text.starts_with(['🧭','⚙','🔍','🤖','🧩','🔁','✓','⚠']) { status.set(text); } }
                        Some(Event::Error { message }) => { messages.write().push(ChatMsg { author: Author::Note, text: format!("error: {message}") }); }
                        Some(Event::Shutdown) | None => break,
                        _ => {}
                    }
                }
            }
        }
    });

    rsx! {
        div { class: "pane-body",
            div { class: "pane-scroll",
                for msg in messages.read().iter() {
                    {
                        match &msg.author {
                            Author::Diff(path, _) => {
                                let path = path.clone();
                                let diff = msg.text.clone();
                                let (adds, dels) = diff_counts(&diff);
                                rsx! {
                                    div { class: "row diffrow",
                                        details { class: "diff-card",
                                            summary { class: "diff-head",
                                                span { class: "diff-caret", Icon { name: "chevron" } }
                                                span { class: "diff-path", "{path}" }
                                                span { class: "diff-adds", "+{adds}" }
                                                span { class: "diff-dels", "−{dels}" }
                                            }
                                            HunkedDiff { ws: workspace.clone(), path: path.clone(), diff }
                                        }
                                    }
                                }
                            }
                            _ => rsx! { Message { author: msg.author.clone(), text: msg.text.clone() } }
                        }
                    }
                }
                if !thinking.read().is_empty() {
                    details { class: "thinking-box", open: *streaming.read(),
                        summary { class: "thinking-sum", "💭 Thinking" }
                        div { class: "thinking-body", "{thinking}" }
                    }
                }
                if *streaming.read() && !status.read().is_empty() {
                    div { class: "status-pill", span { class: "status-spinner" } span { class: "status-shimmer", "{status}" } }
                }
            }
            div { class: "pane-composer",
                textarea { class: "input", placeholder: "Message…", value: "{input}",
                    oninput: move |e| input.set(e.value()),
                    onkeydown: move |e| if e.key() == Key::Enter && !e.modifiers().shift() {
                        e.prevent_default();
                        let t = input.read().trim().to_string();
                        if !t.is_empty() { input.set(String::new()); pane.send(PaneCmd::Submit(t)); }
                    }
                }
                if *streaming.read() {
                    button { class: "send stop", onclick: move |_| pane.send(PaneCmd::Interrupt), "■" }
                } else {
                    button { class: "send", onclick: move |_| {
                        let t = input.read().trim().to_string();
                        if !t.is_empty() { input.set(String::new()); pane.send(PaneCmd::Submit(t)); }
                    }, "↑" }
                }
            }
        }
    }
}

/// Split a unified diff into `(hunk_header, lines)` groups (drops the file
/// `---`/`+++` preamble; each group starts at an `@@` line).
fn split_hunks(diff: &str) -> Vec<(String, Vec<String>)> {
    let mut hunks: Vec<(String, Vec<String>)> = Vec::new();
    for line in diff.lines() {
        if line.starts_with("@@") {
            hunks.push((line.to_string(), Vec::new()));
        } else if line.starts_with("+++") || line.starts_with("---") {
            continue;
        } else if let Some(h) = hunks.last_mut() {
            h.1.push(line.to_string());
        }
    }
    hunks
}

/// Revert a single hunk by replacing its "new" block with its "old" block in
/// the current file. Best-effort: no-op if the block can't be located.
fn revert_hunk(ws: &Path, path: &str, lines: &[String]) -> bool {
    let mut old_block = String::new();
    let mut new_block = String::new();
    for l in lines {
        let (tag, rest) = l.split_at(l.char_indices().next().map(|(_, c)| c.len_utf8()).unwrap_or(0).min(l.len()));
        match tag {
            " " => { old_block.push_str(rest); old_block.push('\n'); new_block.push_str(rest); new_block.push('\n'); }
            "-" => { old_block.push_str(rest); old_block.push('\n'); }
            "+" => { new_block.push_str(rest); new_block.push('\n'); }
            _ => {}
        }
    }
    let file = ws.join(path);
    let Ok(content) = std::fs::read_to_string(&file) else { return false };
    // Match without forcing the trailing newline so end-of-file hunks work.
    let nb = new_block.trim_end_matches('\n');
    let ob = old_block.trim_end_matches('\n');
    if nb.is_empty() || !content.contains(nb) {
        return false;
    }
    let updated = content.replacen(nb, ob, 1);
    std::fs::write(&file, updated).is_ok()
}

#[component]
fn HunkedDiff(ws: PathBuf, path: String, diff: String) -> Element {
    let hunks = split_hunks(&diff);
    let mut reverted = use_signal(std::collections::HashSet::<usize>::new);
    rsx! {
        div { class: "hunked",
            for (hi, (header, lines)) in hunks.into_iter().enumerate() {
                {
                    let done = reverted.read().contains(&hi);
                    let ws2 = ws.clone();
                    let path2 = path.clone();
                    let lines2 = lines.clone();
                    rsx! {
                        div { class: if done { "hunk reverted" } else { "hunk" },
                            div { class: "hunk-head",
                                span { class: "hunk-hdr", "{header}" }
                                if done {
                                    span { class: "hunk-done", "reverted ↩" }
                                } else {
                                    button { class: "hunk-revert", title: "Undo just this hunk in the file",
                                        onclick: move |_| { if revert_hunk(&ws2, &path2, &lines2) { reverted.write().insert(hi); } }, "Revert hunk" }
                                }
                            }
                            pre { class: "diff-body",
                                for line in lines {
                                    {
                                        let cls = if line.starts_with('+') { "dl add" } else if line.starts_with('-') { "dl del" } else { "dl ctx" };
                                        rsx! { div { class: "{cls}", "{line}" } }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn Icon(name: &'static str) -> Element {
    let body = match name {
        "edit" => {
            rsx! { path { d: "M12 20h9" } path { d: "M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4z" } }
        }
        "search" => {
            rsx! { circle { cx: "11", cy: "11", r: "7" } line { x1: "21", y1: "21", x2: "16.65", y2: "16.65" } }
        }
        "plugins" => rsx! {
            rect { x: "3", y: "3", width: "7", height: "7", rx: "1" }
            rect { x: "14", y: "3", width: "7", height: "7", rx: "1" }
            rect { x: "3", y: "14", width: "7", height: "7", rx: "1" }
            rect { x: "14", y: "14", width: "7", height: "7", rx: "1" }
        },
        "terminal" => {
            rsx! { polyline { points: "4 17 10 11 4 5" } line { x1: "12", y1: "19", x2: "20", y2: "19" } }
        }
        "folder" => {
            rsx! { path { d: "M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z" } }
        }
        "file" => {
            rsx! { path { d: "M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" } polyline { points: "14 2 14 8 20 8" } }
        }
        "settings" => rsx! {
            circle { cx: "12", cy: "12", r: "3" }
            path { d: "M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" }
        },
        "spark" => {
            rsx! { path { d: "M12 2v6M12 16v6M2 12h6M16 12h6M5 5l4 4M15 15l4 4M19 5l-4 4M9 15l-4 4" } }
        }
        "shield" => rsx! { path { d: "M12 2l8 4v6c0 5-3.5 8-8 10-4.5-2-8-5-8-10V6z" } },
        "zap" => rsx! { polygon { points: "13 2 3 14 11 14 9 22 21 10 13 10 13 2" } },
        "chevron" => rsx! { polyline { points: "6 9 12 15 18 9" } },
        "plus" => rsx! { line { x1: "12", y1: "5", x2: "12", y2: "19" } line { x1: "5", y1: "12", x2: "19", y2: "12" } },
        "trash" => rsx! { polyline { points: "3 6 5 6 21 6" } path { d: "M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" } },
        "paperclip" => rsx! { path { d: "M21 12.5l-8.5 8.5a5 5 0 0 1-7-7l9-9a3.3 3.3 0 0 1 4.7 4.7l-9 9a1.7 1.7 0 0 1-2.4-2.4l8-8" } },
        "list" => rsx! {
            polyline { points: "3 6 4 7 6 5" }
            polyline { points: "3 12 4 13 6 11" }
            line { x1: "9", y1: "6", x2: "21", y2: "6" }
            line { x1: "9", y1: "12", x2: "21", y2: "12" }
            line { x1: "9", y1: "18", x2: "21", y2: "18" }
        },
        "target" => rsx! { circle { cx: "12", cy: "12", r: "9" } circle { cx: "12", cy: "12", r: "5" } circle { cx: "12", cy: "12", r: "1" } },
        "brain" => rsx! {
            line { x1: "5", y1: "19", x2: "5", y2: "14" }
            line { x1: "10", y1: "19", x2: "10", y2: "11" }
            line { x1: "15", y1: "19", x2: "15", y2: "8" }
            line { x1: "20", y1: "19", x2: "20", y2: "5" }
        },
        "mic" => {
            rsx! { rect { x: "9", y: "3", width: "6", height: "11", rx: "3" } path { d: "M5 11a7 7 0 0 0 14 0M12 18v3" } }
        }
        "laptop" => {
            rsx! { rect { x: "3", y: "4", width: "18", height: "12", rx: "2" } line { x1: "2", y1: "20", x2: "22", y2: "20" } }
        }
        "branch" => rsx! {
            circle { cx: "6", cy: "6", r: "2" } circle { cx: "6", cy: "18", r: "2" } circle { cx: "18", cy: "8", r: "2" }
            path { d: "M6 8v8M18 10a6 6 0 0 1-6 6H8" }
        },
        _ => rsx! { circle { cx: "12", cy: "12", r: "3" } },
    };
    rsx! {
        svg {
            view_box: "0 0 24 24", fill: "none", stroke: "currentColor",
            stroke_width: "1.9", stroke_linecap: "round", stroke_linejoin: "round",
            {body}
        }
    }
}
