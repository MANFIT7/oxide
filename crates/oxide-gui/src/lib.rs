//! Desktop GUI for Oxide — Codex-desktop style, fully functional.
//!
//! Beyond the chat (driven by the shared [`oxide_core`] engine) this GUI ships
//! working: a right file panel that opens and **edits + saves** files, a
//! **terminal** that runs shell commands in the workspace, an **Open folder**
//! picker, and a **Settings** modal that changes provider/model/permissions/
//! workspace and live-reconfigures the engine (persisted to `oxide.toml`).

mod board;
mod update;

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
        "chatgpt" | "codex" | "openai" => Some(svg_inner(SVG_OPENAI).replace("#000000", "#ececf0")),
        "claude" | "anthropic" => Some(svg_inner(SVG_CLAUDE)),
        "cursor" => Some(svg_inner(SVG_CURSOR)),
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

/// `(icon, verb, detail)` for a tool activity row, joined as "icon\tverb\tdetail".
fn activity_label(tool: &str, args: &serde_json::Value) -> String {
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let short = |t: &str| t.chars().take(90).collect::<String>();
    let (icon, verb, detail) = match tool {
        "shell" => ("terminal", "Run", short(s("command"))),
        "write_file" => ("edit", "Edit", s("path").to_string()),
        "read_file" => ("file", "Read", s("path").to_string()),
        "search" => ("search", "Search", s("pattern").to_string()),
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
    Submit(String),
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
        for m in &ms {
            if let Some(name) = m.strip_prefix("skill:") {
                let p = ws.join(".oxide/memory/skills").join(format!("{name}.md"));
                match std::fs::read_to_string(&p) {
                    Ok(c) => skills_block.push_str(&format!("\n## Skill: {name}\n{}\n", c.trim())),
                    Err(_) => skills_block.push_str(&format!("\n## Skill: {name} (not found)\n")),
                }
            } else {
                files.push(format!("@{m}"));
            }
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
    mentions.set(Vec::new());
    input.set(String::new());
    if !steer && *streaming.read() {
        // Default while running: queue — don't disturb the current turn.
        queue.write().push(text);
    } else {
        // Idle → new turn. Steer → inject into the running turn.
        let _ = engine.send(EngineCmd::Submit(text));
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
    let dir = ws.join(".oxide/commands");
    let mut v: Vec<(String, String)> = std::fs::read_dir(&dir)
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
        })
        .collect();
    v.sort();
    v
}

/// Combined `@` menu: skills first, then files/folders.
fn all_mention_items(ws: &Path, query: &str) -> Vec<String> {
    let mut v = skill_candidates(ws, query);
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

/// Recent non-empty sessions `(path, title, msg_count)`, newest first. Deletes
/// empty/0-byte session files as it scans (cleanup).
fn recent_sessions(ws: &Path) -> Vec<(PathBuf, String, usize)> {
    let dir = ws.join(".oxide/sessions");
    let mut items: Vec<(PathBuf, std::time::SystemTime, String, usize)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                continue;
            }
            let meta = e.metadata().ok();
            if meta.as_ref().map(|m| m.len()).unwrap_or(0) == 0 {
                let _ = std::fs::remove_file(&p);
                continue;
            }
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            let count = text.lines().filter(|l| !l.trim().is_empty()).count();
            if count == 0 {
                let _ = std::fs::remove_file(&p);
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
            let mtime = meta.and_then(|m| m.modified().ok()).unwrap_or(std::time::UNIX_EPOCH);
            items.push((p, mtime, title, count));
        }
    }
    items.sort_by(|a, b| b.1.cmp(&a.1));
    items.into_iter().take(15).map(|(p, _, t, c)| (p, t, c)).collect()
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
    let mut mcp_status = use_signal(std::collections::HashMap::<String, String>::new);
    let mut show_board = use_signal(|| false);
    let mut board = use_signal(board::Board::default);
    let mut new_card_title = use_signal(String::new);
    let mut past_sessions = use_signal(Vec::<(PathBuf, String, usize)>::new);
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
    let mut turn_edits = use_signal(Vec::<(String, u32, u32, u64)>::new);
    let mut edits_expanded = use_signal(|| false);
    let mut status = use_signal(String::new);

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
    use_effect(move || {
        let repo = { let r = cfg.read().github_repo.clone(); if r.trim().is_empty() { "MANFIT7/oxide".to_string() } else { r } };
        let url = cfg.read().update_url.clone();
        spawn(async move {
            if let Some(info) = update::check(&repo, &url).await {
                update_info.set(Some(info));
            }
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
            past_sessions.set(recent_sessions(&ws));
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
                        Some(EngineCmd::Submit(text)) => {
                            if let Some(h) = &handle {
                                messages.write().push(ChatMsg { author: Author::User, text: text.clone() });
                                messages.write().push(ChatMsg { author: Author::Agent, text: String::new() });
                                streaming.set(true);
                                let _ = h.submit(Op::UserTurn { text }).await;
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
                                if text.starts_with("session") {
                                    // internal noise — ignore
                                } else if text.starts_with(['🧭','⚙','🔍','🤖','🧩','🔁','✓','⚠']) {
                                    // pipeline stage → live animated status, not a chat note
                                    status.set(text);
                                } else {
                                    messages.write().push(ChatMsg { author: Author::Note, text });
                                }
                            }
                            Event::Error { message } => messages.write().push(ChatMsg { author: Author::Note, text: format!("error: {message}") }),
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
                                turn_edits.write().clear();
                                edits_expanded.set(false);
                                timeline.write().push(TimelineItem { title: format!("Turn {turn} started"), sub: String::new() });
                            }
                            Event::ApprovalRequested { request_id, tool, summary } => {
                                approvals.write().push((request_id, tool.clone(), summary.clone()));
                                timeline.write().push(TimelineItem { title: format!("Approval needed · {tool}"), sub: summary });
                            }
                            Event::ToolCallBegin { tool, args, .. } => {
                                timeline.write().push(TimelineItem { title: format!("⚙ {tool}"), sub: "running…".into() });
                                if tool != "ask_user" {
                                    messages.write().push(ChatMsg { author: Author::Activity { running: true, ok: true }, text: activity_label(&tool, &args) });
                                }
                            }
                            Event::ToolCallEnd { tool, ok, .. } => {
                                timeline.write().push(TimelineItem { title: format!("⚙ {tool}"), sub: if ok { "done".into() } else { "failed".into() } });
                                // Mark the most recent running activity row as finished.
                                let mut m = messages.write();
                                if let Some(c) = m.iter_mut().rev().find(|c| matches!(c.author, Author::Activity { running: true, .. })) {
                                    c.author = Author::Activity { running: false, ok };
                                }
                            }
                            Event::PatchApplied { path, .. } => {
                                timeline.write().push(TimelineItem { title: "✎ patched".into(), sub: path });
                                let v = *git_refresh.read();
                                git_refresh.set(v + 1); // trigger git-tab auto-refresh
                            }
                            Event::FileDiff { path, diff, checkpoint, .. } => {
                                let (adds, dels) = diff_counts(&diff);
                                turn_edits.write().push((path.clone(), adds, dels, checkpoint));
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
        div { class: "app",
            // ── Sidebar ────────────────────────────────────────────────
            aside { class: "sidebar",
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
                    button { class: "nav-item", Icon { name: "search" } span { "Search" } }
                    button { class: "nav-item", onclick: move |_| show_mcp.set(true),
                        Icon { name: "plugins" } span { "MCP" }
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
                        div { class: "project",
                            Icon { name: "folder" }
                            span { class: "project-name", "{project}" }
                            button { class: "project-add", title: "New chat", onclick: move |_| {
                                    show_board.set(false);
                                    let mut op = ui.open_path; op.set(None);
                                    let prov = cfg.read().provider.clone();
                                    let model = cfg.read().model.clone();
                                    let title = provider_title(&prov).to_string();
                                    new_agent_tab(tabs, active_tab, messages, cfg, engine, next_tab_id, &prov, &model, &title);
                                },
                                Icon { name: "plus" }
                            }
                        }
                        for (i, t) in tabs.read().iter().enumerate() {
                            {
                                let i = i;
                                let id = t.id;
                                let title = if t.title.is_empty() { "New chat".to_string() } else { t.title.clone() };
                                let is_active = i == *active_tab.read();
                                rsx! {
                                    div { key: "{id}", class: if is_active { "thread active" } else { "thread" },
                                        onclick: move |_| { show_board.set(false); switch_tab(tabs, active_tab, messages, cfg, engine, i); },
                                        "{title}"
                                    }
                                }
                            }
                        }
                        if !past_sessions.read().is_empty() {
                            div { class: "section-label recent-label", "Recent chats" }
                            for (path, title, count) in past_sessions.read().clone() {
                                {
                                    let path = path.clone();
                                    let title = title.clone();
                                    rsx! {
                                        div { class: "thread recent", title: "{count} messages",
                                            onclick: move |_| { show_board.set(false); open_session_tab(tabs, active_tab, messages, next_tab_id, cfg, path.clone(), title.clone()); },
                                            "{title}"
                                        }
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
                button { class: "settings-btn", onclick: move |_| show_settings.set(true),
                    Icon { name: "settings" } span { "Settings" }
                }
            }

            // ── Center column ──────────────────────────────────────────
            main { class: "main",
                if let Some(info) = update_info.read().clone() {
                    div { class: "update-banner",
                        span { class: "update-text",
                            "⬆ Update available · v{info.version}"
                            if !info.notes.is_empty() { span { class: "update-notes", " — {info.notes}" } }
                        }
                        div { class: "update-actions",
                            button { class: "update-btn", disabled: *updating.read(),
                                onclick: move |_| {
                                    updating.set(true);
                                    let info = info.clone();
                                    spawn(async move {
                                        match update::apply(&info).await {
                                            Ok(()) => update::restart(),
                                            Err(_) => updating.set(false),
                                        }
                                    });
                                },
                                if *updating.read() { "Updating…" } else { "Update & restart" }
                            }
                            button { class: "update-x", onclick: move |_| update_info.set(None), "✕" }
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
                        }
                    }
                }

                div { class: "center",
                    if *show_board.read() && cfg.read().workspace.is_some() {
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
                                       workspace: workspace.clone(), plan_mode, pursue_goal, goal_text, mentions, queue,
                                       on_settings: move |_| show_settings.set(true),
                                       on_open_folder: move |_| open_folder(cfg, ui, engine), on_pick_workspace: move |dir| apply_workspace(cfg, ui, engine, dir) }
                            div { class: "suggestions",
                                for s in suggestions.iter() {
                                    button { class: "suggestion",
                                        onclick: {
                                            let p = s.to_string();
                                            move |_| { let _ = engine.send(EngineCmd::Submit(p.clone())); }
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
                                                    rsx! {
                                                        details { class: "act-group", open: running,
                                                            summary { class: "act-group-head",
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
                                                                            DiffBody { diff }
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
                                        let more_txt = if expanded { "Show less".to_string() } else { format!("Show {} more files", n - 3) };
                                        rsx! {
                                            div { class: "edits-card",
                                                div { class: "edits-head",
                                                    span { class: "edits-ic", Icon { name: "list" } }
                                                    div { class: "edits-title-col",
                                                        span { class: "edits-title", "Edited {n} file{plural}" }
                                                        span { class: "edits-counts", span { class: "diff-adds", "+{total_add}" } " " span { class: "diff-dels", "−{total_del}" } }
                                                    }
                                                    button { class: "edits-undo", onclick: move |_| {
                                                        for (_, _, _, cp) in turn_edits.read().iter() { let _ = engine.send(EngineCmd::Rewind { id: *cp }); reverted.write().insert(*cp); }
                                                    }, "Undo ↺" }
                                                }
                                                for (path, a, d, _cp) in edits.iter().take(shown).cloned() {
                                                    div { class: "edits-row",
                                                        span { class: "edits-path", "{path}" }
                                                        span { class: "edits-rowcounts", span { class: "diff-adds", "+{a}" } " " span { class: "diff-dels", "−{d}" } }
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
                        div { class: "composer-dock",
                            Composer { input, streaming, engine, cfg, model_label, bypass,
                                       project: project.clone(), branch: branch.clone(),
                                       context_used: ctx_used, context_limit: ctx_limit,
                                       workspace: workspace.clone(), plan_mode, pursue_goal, goal_text, mentions, queue,
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
            if *show_files.read() && cfg.read().workspace.is_some() {
                aside { class: "files-panel",
                    div { class: "insp-tabs",
                        for (key, label) in [("files","Files"),("timeline","Timeline"),("sessions","Sessions"),("git","Git"),("memory","Memory"),("goal","Goal"),("browser","Browser"),("approvals","Approvals"),("checkpoints","Checkpoints"),("usage","Usage")] {
                            {
                                let active = *inspector_tab.read() == key;
                                let badge = match key {
                                    "approvals" => approvals.read().len(),
                                    "checkpoints" => checkpoints.read().len(),
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
        }
    }
}

#[component]
fn McpModal(cfg: Signal<Config>, engine: Coroutine<EngineCmd>, status: Signal<std::collections::HashMap<String, String>>, on_close: EventHandler<()>) -> Element {
    let mut name = use_signal(String::new);
    let mut command = use_signal(String::new);
    let mut args = use_signal(String::new);
    let servers = cfg.read().mcp_servers.clone();
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
                            list.push(oxide_config::McpServerConfig { name: n, command: cmd, args: a });
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

/// Native folder picker → switch workspace.
fn open_folder(cfg: Signal<Config>, ui: Ui, engine: Coroutine<EngineCmd>) {
    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
        apply_workspace(cfg, ui, engine, dir);
    }
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
                                if let Some(d) = rfd::FileDialog::new().pick_folder() { ws.set(d); }
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
    // `@mention` codebase picker.
    let mention = active_mention(&input.read());
    let mention_items: Vec<String> = match &mention {
        Some((_, q)) => all_mention_items(&workspace, q),
        None => Vec::new(),
    };
    let mention_at = mention.as_ref().map(|(a, _)| *a);
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
        div { class: "composer",
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
                        div { class: "menu-label", "Skills & files · ↑↓ Enter" }
                        for (i, path) in mention_items.iter().cloned().enumerate() {
                            {
                                let p_sel = path.clone();
                                let is_skill = path.starts_with("skill:");
                                let disp = path.strip_prefix("skill:").unwrap_or(&path).to_string();
                                let icon_name = if is_skill { "target" } else if path.ends_with('/') { "folder" } else { "file" };
                                let sel = i == msel;
                                rsx! {
                                    button {
                                        class: if sel { "menu-item sel" } else { "menu-item" },
                                        onmouseenter: move |_| mention_sel.set(i),
                                        onclick: move |_| { pick_mention(input, mentions, at, p_sel.clone()); mention_sel.set(0); },
                                        Icon { name: icon_name }
                                        span { class: "menu-name", "{disp}" }
                                        if is_skill { span { class: "menu-tag", "skill" } }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if !mentions.read().is_empty() {
                div { class: "chips",
                    for (i, m) in mentions.read().iter().cloned().enumerate() {
                        {
                            let is_skill = m.starts_with("skill:");
                            let disp = m.strip_prefix("skill:").unwrap_or(&m).to_string();
                            let icon_name = if is_skill { "target" } else if !m.contains('.') || m.ends_with('/') { "folder" } else { "file" };
                            rsx! {
                                span { class: if is_skill { "chip skill" } else { "chip" },
                                    Icon { name: icon_name }
                                    span { class: "chip-name", "{disp}" }
                                    button { class: "chip-x", onclick: move |_| {
                                        let mut v = mentions.read().clone();
                                        if i < v.len() { v.remove(i); }
                                        mentions.set(v);
                                    }, "✕" }
                                }
                            }
                        }
                    }
                }
            }
            textarea {
                class: "input",
                placeholder: if *streaming.read() { "Steer the agent (sent mid-run)…" } else { "Do anything" },
                value: "{input}",
                oninput: move |e| input.set(e.value()),
                onkeydown: move |e| {
                    // When the @mention popup is open, the keyboard drives it.
                    if let Some(at) = mention_at {
                        let items = all_mention_items(&ws_kd, &active_mention(&input.read()).map(|(_, q)| q).unwrap_or_default());
                        if !items.is_empty() {
                            match e.key() {
                                Key::ArrowDown => { e.prevent_default(); let n = items.len(); let s = (*mention_sel.read() + 1) % n; mention_sel.set(s); return; }
                                Key::ArrowUp => { e.prevent_default(); let n = items.len(); let c = *mention_sel.read(); mention_sel.set((c + n - 1) % n); return; }
                                Key::Enter => { e.prevent_default(); let s = (*mention_sel.read()).min(items.len() - 1); pick_mention(input, mentions, at, items[s].clone()); mention_sel.set(0); return; }
                                _ => {}
                            }
                        }
                    }
                    if e.key() == Key::Enter && !e.modifiers().shift() {
                        e.prevent_default();
                        submit_prompt(input, streaming, engine, plan_mode, pursue_goal, goal_text, mentions, queue, false, &ws_kd2);
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
                                        if let Some(file) = rfd::FileDialog::new().pick_file() {
                                            let mut cur = input.read().clone();
                                            if !cur.is_empty() && !cur.ends_with(' ') { cur.push(' '); }
                                            cur.push('@');
                                            cur.push_str(&file.display().to_string());
                                            cur.push(' ');
                                            input.set(cur);
                                        }
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
                            let mut c = cfg.read().clone();
                            let next = !c.fast_mode;
                            c.fast_mode = next;
                            if next {
                                if let Some(preset) = fast_model_for(&c.provider) {
                                    c.model = preset.model.to_string();
                                }
                                c.reasoning_effort = "low".to_string();
                            } else if c.reasoning_effort == "low" {
                                c.reasoning_effort = "medium".to_string();
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
                                                    let mut c = cfg.read().clone();
                                                    c.reasoning_effort = value.clone();
                                                    if value != "low" {
                                                        c.fast_mode = false;
                                                    }
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
                        button { class: "send steer", title: "Steer (inject into the running turn)", onclick: move |_| submit_prompt(input, streaming, engine, plan_mode, pursue_goal, goal_text, mentions, queue, true, &ws_steer), "↪" }
                        button { class: "send stop", title: "Stop", onclick: move |_| { let _ = engine.send(EngineCmd::Interrupt); }, "■" }
                    } else {
                        button { class: "send", onclick: move |_| submit_prompt(input, streaming, engine, plan_mode, pursue_goal, goal_text, mentions, queue, false, &ws_btn), "↑" }
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
        Author::User => rsx! { div { class: "row user", div { class: "bubble", "{text}" } } },
        Author::Agent => rsx! {
            div { class: "row agent",
                img { class: "avatar", src: logo_uri() }
                if text.is_empty() {
                    div { class: "typing", span {} span {} span {} }
                } else {
                    div { class: "agent-text", "{text}" }
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

#[component]
fn ActivityRow(text: String, running: bool, ok: bool) -> Element {
    let cls = if running { "activity-card running" } else if ok { "activity-card done" } else { "activity-card fail" };
    // text is "icon\tverb\tdetail"
    let mut parts = text.splitn(3, '\t');
    let icon = parts.next().unwrap_or("spark").to_string();
    let verb = parts.next().unwrap_or("").to_string();
    let detail = parts.next().unwrap_or("").to_string();
    rsx! {
        div { class: "row activity",
            div { class: "{cls}",
                span { class: "activity-tic", Icon { name: icon_static(&icon) } }
                if running {
                    span { class: "activity-spin" }
                } else if ok {
                    span { class: "activity-ic ok", "✓" }
                } else {
                    span { class: "activity-ic fail", "✕" }
                }
                span { class: "activity-verb", "{verb}" }
                if !detail.is_empty() { span { class: "activity-text", "{detail}" } }
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

#[component]
fn DiffBody(diff: String) -> Element {
    rsx! {
        pre { class: "diff-body",
            for line in diff.lines() {
                {
                    let cls = if line.starts_with("+++") || line.starts_with("---") {
                        "dl meta"
                    } else if line.starts_with("@@") {
                        "dl hunk"
                    } else if line.starts_with('+') {
                        "dl add"
                    } else if line.starts_with('-') {
                        "dl del"
                    } else {
                        "dl ctx"
                    };
                    let line = line.to_string();
                    rsx! { div { class: "{cls}", "{line}" } }
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
