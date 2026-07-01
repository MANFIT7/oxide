//! Desktop GUI for Oxide — Codex-desktop style, fully functional.
//!
//! Beyond the chat (driven by the shared [`oxide_core`] engine) this GUI ships
//! working: a right file panel that opens and **edits + saves** files, a
//! **terminal** that runs shell commands in the workspace, an **Open folder**
//! picker, and a **Settings** modal that changes provider/model/permissions/
//! workspace and live-reconfigures the engine (persisted to `oxide.toml`).

mod board;
mod hermes;
mod preview_proxy;
mod update;

use dioxus::desktop::{Config as DesktopConfig, WindowBuilder};
use dioxus::prelude::*;
use futures::StreamExt;
use oxide_config::Config;
use oxide_core::{automation, EngineHandle};
use oxide_protocol::{
    ApprovalDecision, ApprovalPolicy, DesignEdit, DesignPatchProposal, DesignSelection, Event, Op,
    SandboxPolicy, SubagentControlAction, UiNode, UiNodeKind, UiSpec, UiTone,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const CSS: &str = include_str!("../assets/style.css");
// Embedded terminal: wterm (DOM renderer + Zig/WASM VT core, Apache-2.0), bundled
// self-contained (WASM inlined as base64) via esbuild so it needs no network and
// no separate .wasm fetch. The IIFE exposes `var OxideWTerm`; TerminalView injects
// it once (guarded) and re-attaches it to `window` since dioxus wraps eval in an
// async fn. Replaces the old xterm.js embed and the oxide-term separate window.
const WTERM_JS: &str = include_str!("../assets/wterm.bundle.js");
const WTERM_CSS: &str = include_str!("../assets/wterm.css");
const MERMAID_JS: &[u8] = include_bytes!("../assets/vendor/mermaid.min.js");
const NERD_FONT: &[u8] = include_bytes!("../assets/fonts/JetBrainsMonoNerdFontMono-Regular.ttf");
const LOGO_BYTES: &[u8] = include_bytes!("../assets/logo.png");
const DONE_SOUND: &[u8] = include_bytes!("../../../sound/mixkit-software-interface-back-2575.wav");

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

// Brand logos for the provider picker (inline SVG).
const SVG_CLAUDE: &str = include_str!("../assets/providers/claude-icon.svg");
const SVG_OPENAI: &str = include_str!("../assets/providers/openai-icon.svg");
const SVG_CURSOR: &str = include_str!("../assets/providers/cursor.svg");
const SVG_MCP: &str = include_str!("../assets/providers/mcp-icon.svg");
const SVG_GITHUB: &str = include_str!("../assets/providers/github.svg");

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
        "chatgpt" | "codex" | "openai" => {
            Some(svg_inner(SVG_OPENAI).replace("#000000", "currentColor"))
        }
        "claude" | "claude_interactive" | "anthropic" => Some(svg_inner(SVG_CLAUDE)),
        "cursor" => Some(svg_inner(SVG_CURSOR)),
        "mcp" => Some(svg_inner(SVG_MCP).replace("#000000", "currentColor")),
        "github" => Some(svg_inner(SVG_GITHUB).replace("#181717", "currentColor")),
        _ => None,
    }
}

/// Resolve the `oxide-term` binary: bundled next to the app exe (inside
/// Oxide.app/Contents/MacOS), a dev build under the repo, then PATH.
fn oxide_term_bin() -> std::path::PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("oxide-term");
            if p.exists() {
                return p;
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        // oxide-term is a workspace member → built into the shared target/.
        for rel in ["target/release/oxide-term", "target/debug/oxide-term"] {
            let p = cwd.join(rel);
            if p.exists() {
                return p;
            }
        }
    }
    std::path::PathBuf::from("oxide-term")
}

/// Open the standalone native GPU terminal (oxide-term) in a separate wgpu/Metal
/// window running `cmd` (empty = $SHELL) in `cwd`. A GPU surface can't live in the
/// Dioxus webview, so it's a sibling native window. Returns false if it couldn't
/// spawn (binary absent).
fn spawn_oxide_term(cwd: &str, cmd: &[String]) -> bool {
    let mut c = std::process::Command::new(oxide_term_bin());
    if !cwd.is_empty() {
        c.arg("--cwd").arg(cwd);
    }
    for a in cmd {
        c.arg(a);
    }
    // Capture oxide-term's stderr to a log so a GPU/Metal init crash (which makes
    // the window silently fail to appear) is diagnosable instead of vanishing.
    if let Ok(f) = std::fs::File::create(std::env::temp_dir().join("oxide-term.log")) {
        c.stderr(std::process::Stdio::from(f));
    }
    c.spawn().is_ok()
}

/// Open a plain native GPU terminal ($SHELL) — the terminal-panel button.
fn launch_native_terminal() -> bool {
    spawn_oxide_term("", &[])
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
        model: "claude-fable-5",
        provider_label: "Claude Code",
        label: "Fable 5",
        summary: "Anthropic's newest frontier coding model",
        badge: "New",
        fast: false,
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
    // API-key providers (OpenAI/Anthropic) intentionally omitted — Oxide is a
    // GUI wrapper around the user's logged-in CLIs + ChatGPT subscription, with
    // no raw API-key entry (Synara-style).
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
    EffortPreset {
        value: "max",
        label: "Max",
        summary: "Deepest reasoning (Claude only)",
    },
];

/// Effort levels the selected provider actually supports — the menu groups to
/// each provider's own ceiling (GPT tops out at Extra, Claude reaches Max).
fn effort_levels(provider: &str) -> &'static [EffortPreset] {
    match provider {
        // Claude Code CLI + Anthropic API: low/medium/high/xhigh/max.
        "claude" | "claude_interactive" | "anthropic" => &EFFORT_PRESETS[0..5],
        // GPT family (codex/chatgpt/openai): low/medium/high/xhigh.
        "codex" | "chatgpt" | "openai" => &EFFORT_PRESETS[0..4],
        // Others: plain low/medium/high.
        _ => &EFFORT_PRESETS[0..3],
    }
}

/// Clamp an effort value to what the provider supports (nearest lower).
fn clamp_effort(provider: &str, effort: &str) -> String {
    let levels = effort_levels(provider);
    if levels.iter().any(|p| p.value == effort) {
        return effort.to_string();
    }
    // Too high for this provider — take its ceiling.
    levels
        .last()
        .map(|p| p.value.to_string())
        .unwrap_or_else(|| "medium".into())
}

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
        Some(p) if p.as_path() != Path::new("/") => p,
        _ => std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".")),
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
        .with_transparent(true)
        .with_inner_size(dioxus::desktop::tao::dpi::LogicalSize::new(1280.0, 820.0));
    LaunchBuilder::desktop()
        .with_cfg(
            DesktopConfig::new()
                .with_window(window)
                .with_background_color((0, 0, 0, 0)),
        )
        .with_context(config)
        .launch(app);
    Ok(())
}

#[derive(Clone, PartialEq)]
enum Author {
    User,
    Agent,
    Note,
    /// Rust-native structured UI artifact (json-render pattern without a JS runtime).
    UiSpec,
    /// A reviewable file diff: (path, checkpoint id to rewind).
    Diff(String, u64),
    /// A tool activity row (terminal/edit/read/…). `key` is the stable
    /// provider id (tool call_id / command_id) so streamed updates settle the
    /// exact row they belong to — found by id, never by Vec index, so inserts
    /// or reordering can't pair the wrong row. `None` for id-less notices.
    Activity {
        running: bool,
        ok: bool,
        key: Option<String>,
    },
}

const DONE_NOTE_PREFIX: char = '\u{2713}';
const DONE_NOTE_MARK: &str = "\u{2713} Done";

/// Newest-first index of the activity row carrying `key`. Replaces the old
/// `command_id/call_id` side maps, which went stale whenever a row was
/// inserted (e.g. above a trailing Done note) and then paired the wrong row.
fn activity_idx(msgs: &[ChatMsg], key: &str) -> Option<usize> {
    msgs.iter()
        .rposition(|m| matches!(&m.author, Author::Activity { key: Some(k), .. } if k == key))
}

/// Push an activity row into a backgrounded-tab buffer, keeping it above a
/// trailing Done note (the bg-loop equivalent of the `push_activity!` macro,
/// so a late row never lands below the turn's summary once merged into the view).
fn buf_push_activity(buf: &mut Vec<ChatMsg>, msg: ChatMsg) {
    if buf
        .last()
        .map(|x| matches!(x.author, Author::Note) && x.text.starts_with(DONE_NOTE_MARK))
        .unwrap_or(false)
    {
        let at = buf.len() - 1;
        buf.insert(at, msg);
    } else {
        buf.push(msg);
    }
}

fn looks_like_done_duration(part: &str) -> bool {
    let mut saw_unit = false;
    for token in part.split_whitespace() {
        let Some(unit) = token.chars().last() else {
            return false;
        };
        if !matches!(unit, 's' | 'm' | 'h') {
            return false;
        }
        let digits = &token[..token.len().saturating_sub(unit.len_utf8())];
        if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
            return false;
        }
        saw_unit = true;
    }
    saw_unit
}

fn done_note_display_parts(text: &str) -> (String, Vec<String>) {
    let clean = text.strip_prefix(DONE_NOTE_PREFIX).unwrap_or(text).trim();
    let mut parts: Vec<String> = clean
        .split(" · ")
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToString::to_string)
        .collect();
    if parts.is_empty() {
        return ("Done".to_string(), Vec::new());
    }
    if parts.len() > 1 && looks_like_done_duration(&parts[1]) {
        parts.remove(1);
    }
    let label = parts.remove(0);
    (label, parts)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActivityKind {
    Command,
    FileRead,
    FileChange,
    Search,
    Web,
    Memory,
    Generic,
}

impl ActivityKind {
    fn class_name(self) -> &'static str {
        match self {
            ActivityKind::Command => "command",
            ActivityKind::FileRead => "file-read",
            ActivityKind::FileChange => "file-change",
            ActivityKind::Search => "search",
            ActivityKind::Web => "web",
            ActivityKind::Memory => "memory",
            ActivityKind::Generic => "generic",
        }
    }
}

#[derive(Clone, PartialEq)]
struct ActivityView {
    icon: String,
    verb: String,
    detail: String,
    output: String,
    kind: ActivityKind,
}

#[derive(Clone, PartialEq)]
struct TranscriptGroup {
    activity: bool,
    indices: Vec<usize>,
    key: usize,
    live: bool,
}

#[derive(Clone, PartialEq)]
struct TranscriptTurn {
    groups: Vec<TranscriptGroup>,
    /// The turn's Done summary is pulled OUT of the row list so it always
    /// renders as the turn's last child — never below it (Synara's model: the
    /// summary is the last child of the message, not a reorderable sibling row).
    done_summary: Option<String>,
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

fn activity_kind(icon: &str, verb: &str, detail: &str) -> ActivityKind {
    let v = verb.to_ascii_lowercase();
    let d = detail.to_ascii_lowercase();
    match icon {
        "terminal" => ActivityKind::Command,
        "edit" => ActivityKind::FileChange,
        "eye" => ActivityKind::FileRead,
        "file" => ActivityKind::FileRead,
        "search" => ActivityKind::Search,
        "globe" => ActivityKind::Web,
        "brain" => ActivityKind::Memory,
        _ if v.contains("edit") || v.contains("write") || v.contains("patch") => {
            ActivityKind::FileChange
        }
        _ if v.contains("read")
            || d.ends_with(".rs")
            || d.ends_with(".ts")
            || d.ends_with(".tsx") =>
        {
            ActivityKind::FileRead
        }
        _ if v.contains("search") || v.contains("find") => ActivityKind::Search,
        _ if v.contains("web") || v.contains("fetch") || d.starts_with("http") => ActivityKind::Web,
        _ => ActivityKind::Generic,
    }
}

fn activity_view(text: &str) -> ActivityView {
    let mut parts = text.splitn(4, '\t');
    let icon = parts.next().unwrap_or("spark").to_string();
    let verb = parts.next().unwrap_or("").to_string();
    let detail = parts.next().unwrap_or("").to_string();
    let output = parts.next().unwrap_or("").to_string();
    let kind = activity_kind(&icon, &verb, &detail);
    ActivityView {
        icon,
        verb,
        detail,
        output,
        kind,
    }
}

fn build_transcript_turns(messages: &[ChatMsg]) -> Vec<TranscriptTurn> {
    let mut groups: Vec<TranscriptGroup> = Vec::new();
    let mut i = 0;
    while i < messages.len() {
        if matches!(messages[i].author, Author::Activity { .. }) {
            let start = i;
            let mut live = false;
            while i < messages.len() {
                match messages[i].author {
                    Author::Activity { running, .. } => {
                        live |= running;
                        i += 1;
                    }
                    _ => break,
                }
            }
            groups.push(TranscriptGroup {
                activity: true,
                indices: (start..i).collect(),
                key: start,
                live,
            });
        } else {
            groups.push(TranscriptGroup {
                activity: false,
                indices: vec![i],
                key: i,
                live: false,
            });
            i += 1;
        }
    }

    // Assemble groups into turns (split at user messages). A standalone
    // Done note is pulled OUT of the row list into `turn.done_summary`, so
    // the render can place it as the turn's LAST child — a late tool/activity row
    // that lands after it in the buffer can never render below it.
    let is_done_note =
        |m: &ChatMsg| matches!(m.author, Author::Note) && m.text.starts_with(DONE_NOTE_MARK);
    let mut turns: Vec<TranscriptTurn> = Vec::new();
    for group in groups {
        if !group.activity && group.indices.len() == 1 && is_done_note(&messages[group.indices[0]])
        {
            if let Some(turn) = turns.last_mut() {
                turn.done_summary = Some(messages[group.indices[0]].text.clone());
                continue;
            }
        }
        let starts_turn = group
            .indices
            .first()
            .map(|&idx| messages[idx].author == Author::User)
            .unwrap_or(false);
        if starts_turn || turns.is_empty() {
            turns.push(TranscriptTurn {
                groups: vec![group],
                done_summary: None,
            });
        } else if let Some(turn) = turns.last_mut() {
            turn.groups.push(group);
        }
    }
    turns
}

fn activity_group_display(rows: &[(String, bool, bool)]) -> (&'static str, String) {
    let n = rows.len();
    let running = rows.iter().any(|(_, running, _)| *running);
    let mut edits = 0;
    let mut commands = 0;
    let mut searches = 0;
    let mut web = 0;
    for (text, _, _) in rows {
        match activity_view(text).kind {
            ActivityKind::FileChange => edits += 1,
            ActivityKind::Command => commands += 1,
            ActivityKind::Search => searches += 1,
            ActivityKind::Web => web += 1,
            _ => {}
        }
    }

    if running && edits > 0 {
        (
            "edit",
            format!("Editing files… {n} action{}", if n == 1 { "" } else { "s" }),
        )
    } else if running {
        (
            "settings",
            format!("Working… {n} action{}", if n == 1 { "" } else { "s" }),
        )
    } else if edits > 0 && edits >= commands + searches + web {
        ("edit", format!("{edits} file changes · {n} actions"))
    } else if commands > 0 && commands >= searches + web {
        ("terminal", format!("{commands} commands · {n} actions"))
    } else if searches > 0 {
        ("search", format!("{searches} searches · {n} actions"))
    } else if web > 0 {
        ("browser", format!("{web} web actions · {n} actions"))
    } else {
        ("settings", format!("{n} actions"))
    }
}

/// Coalesce consecutive same-file edit rows into one. Three back-to-back
/// `Edit /path/main.rs` activity rows collapse to a single entry carrying a
/// repeat count, so the stream shows one animated `+/−` row instead of N
/// identical tool rows. Non-edit rows (and edits to a different file) pass
/// through with count 1. Returns `(text, running, ok, count)`.
fn coalesce_activity_rows(rows: Vec<(String, bool, bool)>) -> Vec<(String, bool, bool, usize)> {
    let mut out: Vec<(String, bool, bool, usize)> = Vec::with_capacity(rows.len());
    for (text, running, ok) in rows {
        let view = activity_view(&text);
        if matches!(view.kind, ActivityKind::FileChange) && !view.detail.is_empty() {
            if let Some(last) = out.last_mut() {
                let lview = activity_view(&last.0);
                if matches!(lview.kind, ActivityKind::FileChange) && lview.detail == view.detail {
                    last.1 |= running; // any still running keeps the row live
                    last.2 &= ok; // all must succeed for the row to read done
                    last.3 += 1;
                    continue;
                }
            }
        }
        out.push((text, running, ok, 1));
    }
    out
}

fn prefixed_icon_text(text: &str) -> Option<(&'static str, String)> {
    let trimmed = text.trim_start();
    let mut chars = trimmed.chars();
    let ch = chars.next()?;
    let icon = match ch {
        '\u{2318}' => "terminal",
        '\u{270e}' => "edit",
        '\u{1f50e}' => "search",
        '\u{2699}' => "settings",
        '\u{23f3}' | '\u{29d6}' => "clock",
        '\u{26a0}' => "alert",
        '\u{23f8}' => "pause",
        '\u{2753}' => "help",
        '\u{1f9ed}' => "target",
        '\u{1f916}' => "spark",
        '\u{1f9e9}' => "plugins",
        '\u{1f501}' => "refresh",
        '\u{1f9ea}' => "flask",
        '\u{1f310}' => "browser",
        '\u{1fa9d}' => "hook",
        '\u{1f4f8}' => "camera",
        '\u{1f6e0}' => "tool",
        '\u{1f4cd}' => "pin",
        '\u{2713}' => "check",
        _ => return None,
    };
    let label = chars.as_str().trim_start().to_string();
    Some((icon, label))
}

fn plain_status_icon_text(text: &str) -> Option<(&'static str, String)> {
    let trimmed = text.trim_start();
    let icon = if trimmed.starts_with("Planning") {
        "target"
    } else if trimmed.starts_with("Running") || trimmed.starts_with("Sub-agent") {
        "spark"
    } else if trimmed.starts_with("Synthesizing") {
        "plugins"
    } else if trimmed.starts_with("Implementing") || trimmed.starts_with("Preparing") {
        "settings"
    } else if trimmed.starts_with("Steering") {
        "corner-up-right"
    } else if trimmed.starts_with("Auto-verify") {
        "flask"
    } else if trimmed.starts_with("Reviewing") {
        "search"
    } else if trimmed.starts_with("Review passed") {
        "check"
    } else if trimmed.starts_with("Gaps remain")
        || trimmed.starts_with("context full")
        || trimmed.starts_with("worker context full")
    {
        "alert"
    } else if trimmed.starts_with("Fixing gaps") {
        "refresh"
    } else if trimmed.starts_with("Background") {
        "clock"
    } else {
        return None;
    };
    Some((icon, trimmed.to_string()))
}

fn icon_text(text: &str) -> Option<(&'static str, String)> {
    prefixed_icon_text(text).or_else(|| plain_status_icon_text(text))
}

fn is_stage_status(text: &str) -> bool {
    prefixed_icon_text(text).is_some() || plain_status_icon_text(text).is_some()
}

/// `(icon, verb, detail)` for a tool activity row, joined as "icon\tverb\tdetail".
fn activity_label(tool: &str, args: &serde_json::Value) -> String {
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let short = |t: &str| t.chars().take(90).collect::<String>();
    let (icon, verb, detail) = match tool {
        "shell" => ("terminal", "Run", short(s("command"))),
        "write_file" => ("edit", "Write", s("path").to_string()),
        "edit" => ("edit", "Edit", s("path").to_string()),
        "read_file" => ("eye", "Read", s("path").to_string()),
        "search" => ("search", "Search", short(s("query"))),
        "codebase_search" => ("search", "Find code", short(s("query"))),
        "web_search" => ("globe", "Search web", short(s("query"))),
        "fetch_url" => ("globe", "Fetch", s("url").to_string()),
        "browser_open" => ("globe", "Open", s("url").to_string()),
        "browser_navigate" => ("globe", "Open", s("url").to_string()),
        "browser_read" => ("globe", "Read page", String::new()),
        "browser_screenshot" => ("globe", "Screenshot", String::new()),
        "browser_eval" => ("globe", "Evaluate", short(s("script"))),
        "browser_click" => ("globe", "Click", s("selector").to_string()),
        "browser_type" => ("globe", "Type", s("selector").to_string()),
        "design_read_system" => ("palette", "Read design", s("path").to_string()),
        "design_extract_tokens" => ("palette", "Extract tokens", s("source").to_string()),
        "design_snapshot" => ("palette", "Design snapshot", s("url").to_string()),
        "design_review" => ("palette", "Review design", String::new()),
        "design_propose_patch" => ("palette", "Propose design patch", String::new()),
        "todo_write" => {
            let n = args
                .get("todos")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            (
                "brain",
                "Update todo",
                format!("{n} item{}", if n == 1 { "" } else { "s" }),
            )
        }
        "ask_user" => ("brain", "Ask user", short(s("question"))),
        "remember" => ("brain", "Remember", String::new()),
        "save_skill" => ("brain", "Save skill", String::new()),
        t if t.starts_with("mcp__") => {
            let rest = t.trim_start_matches("mcp__");
            let (server, name) = rest.split_once("__").unwrap_or(("", rest));
            ("spark", "MCP", format!("{name} · {server}"))
        }
        t if t.starts_with("browser_") => (
            "globe",
            "Browser",
            t.trim_start_matches("browser_").to_string(),
        ),
        other => ("spark", "Tool", other.to_string()),
    };
    format!("{icon}\t{verb}\t{detail}")
}

fn design_string(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

fn design_selection_from_value(value: &serde_json::Value) -> DesignSelection {
    let mut styles = std::collections::BTreeMap::new();
    if let Some(obj) = value.get("styles").and_then(|v| v.as_object()) {
        for (key, val) in obj {
            if let Some(s) = val.as_str() {
                styles.insert(key.clone(), s.to_string());
            }
        }
    }
    DesignSelection {
        selector: design_string(value, "selector"),
        component: design_string(value, "component"),
        source: design_string(value, "source"),
        text: design_string(value, "text"),
        html: design_string(value, "html"),
        styles,
    }
}

fn upsert_design_edit(
    edits: &mut Vec<(String, String, String)>,
    property: String,
    old_value: String,
    new_value: String,
) {
    if let Some(existing) = edits.iter_mut().find(|(prop, _, _)| *prop == property) {
        existing.2 = new_value;
    } else {
        edits.push((property, old_value, new_value));
    }
}

fn design_edit_values(edits: &[(String, String, String)]) -> Vec<DesignEdit> {
    edits
        .iter()
        .map(|(property, old_value, new_value)| DesignEdit {
            property: property.clone(),
            old_value: old_value.clone(),
            new_value: new_value.clone(),
        })
        .collect()
}

fn build_design_apply_prompt(
    selection: &DesignSelection,
    edits: &[DesignEdit],
    note: &str,
) -> String {
    let proposal = DesignPatchProposal {
        selection: selection.clone(),
        edits: edits.to_vec(),
        instruction: note.trim().to_string(),
    };
    let mut spec = String::from("Apply these Design Workbench edits to the SOURCE CODE.\n");
    spec.push_str("- Find the element in the codebase; prefer existing design tokens/classes over raw values.\n");
    spec.push_str("- Keep motion purposeful and add reduced-motion coverage for transform/scale/position animation.\n");
    spec.push_str("- After editing, verify the rendered UI or run the relevant build/check.\n\n");
    spec.push_str(&format!("- selector: {}\n", proposal.selection.selector));
    if !proposal.selection.component.is_empty() {
        spec.push_str(&format!(
            "- component: <{}>\n",
            proposal.selection.component
        ));
    }
    if !proposal.selection.source.is_empty() {
        spec.push_str(&format!("- source: {}\n", proposal.selection.source));
    }
    if !proposal.selection.text.is_empty() {
        spec.push_str(&format!("- text: {:?}\n", proposal.selection.text));
    }
    if !proposal.selection.html.is_empty() {
        spec.push_str(&format!("- html: {}\n", proposal.selection.html));
    }
    spec.push_str("- edits:\n");
    for edit in &proposal.edits {
        if edit.property == "text" {
            spec.push_str(&format!("  - text -> {:?}\n", edit.new_value));
        } else {
            spec.push_str(&format!(
                "  - {}: {} -> {}\n",
                edit.property, edit.old_value, edit.new_value
            ));
        }
    }
    if !proposal.instruction.is_empty() {
        spec.push_str("\nReview note:\n");
        spec.push_str(&proposal.instruction);
        spec.push('\n');
    }
    spec
}

fn tool_input_preview_label(tool: &str, accumulated: &str) -> String {
    let short = accumulated
        .trim()
        .replace(['\n', '\r', '\t'], " ")
        .chars()
        .take(140)
        .collect::<String>();
    let detail = if short.is_empty() {
        tool.to_string()
    } else {
        format!("{tool} · {short}")
    };
    format!("spark\tPreparing\t{detail}")
}

fn upsert_tool_input_preview(
    messages: &mut Vec<ChatMsg>,
    call_id: String,
    tool: String,
    accumulated: String,
) {
    if call_id.is_empty() {
        return;
    }
    let text = tool_input_preview_label(&tool, &accumulated);
    if let Some(idx) = activity_idx(messages, &call_id) {
        messages[idx].text = text;
        if let Author::Activity { running, ok, .. } = &mut messages[idx].author {
            *running = true;
            *ok = true;
        }
    } else {
        buf_push_activity(
            messages,
            ChatMsg {
                author: Author::Activity {
                    running: true,
                    ok: true,
                    key: Some(call_id),
                },
                text,
            },
        );
    }
}

fn command_activity_label(command: &str, background: bool) -> String {
    let short: String = command.trim().chars().take(140).collect();
    let verb = if background { "Background" } else { "Run" };
    format!("terminal\t{verb}\t{short}")
}

fn append_activity_output(text: &mut String, chunk: &str) {
    if chunk.is_empty() {
        return;
    }
    let mut parts = text.splitn(4, '\t');
    let icon = parts.next().unwrap_or("terminal");
    let verb = parts.next().unwrap_or("Run");
    let detail = parts.next().unwrap_or("");
    let mut output = parts.next().unwrap_or("").to_string();
    output.push_str(chunk);
    if output.chars().count() > 8000 {
        output = output
            .chars()
            .rev()
            .take(7000)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<String>();
        output.insert_str(0, "… (output truncated)\n");
    }
    *text = format!("{icon}\t{verb}\t{detail}\t{output}");
}

fn activity_has_output(text: &str) -> bool {
    text.splitn(4, '\t')
        .nth(3)
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

#[derive(Clone, PartialEq)]
struct ChatMsg {
    author: Author,
    text: String,
}

fn ui_spec_message(spec: UiSpec) -> ChatMsg {
    ChatMsg {
        author: Author::UiSpec,
        text: serde_json::to_string(&spec).unwrap_or_else(|_| "{}".to_string()),
    }
}

fn parse_ui_spec_message(text: &str) -> Result<UiSpec, String> {
    let spec: UiSpec =
        serde_json::from_str(text).map_err(|e| format!("ui spec parse error: {e}"))?;
    spec.validate()?;
    Ok(spec)
}

#[component]
fn SlotText(text: String, #[props(default)] reverse: bool) -> Element {
    let dir = if reverse { "down" } else { "up" };
    rsx! {
        span { class: "slot-text {dir}", aria_label: "{text}",
            for (i, ch) in text.chars().enumerate() {
                span {
                    class: "slot-char",
                    style: "--i:{i}",
                    aria_hidden: "true",
                    if ch == ' ' { "\u{00a0}" } else { "{ch}" }
                }
            }
        }
    }
}

/// Commands sent into the engine coroutine.
enum EngineCmd {
    /// `engine` is the full prompt (with mention/skill/MCP context); `display`
    /// is the clean bubble text.
    Submit {
        engine: String,
        display: String,
    },
    Reconfigure(Config),
    /// Activate tab `id`: swap the VIEW to its transcript/config. Engines are
    /// per-tab — the tab being left keeps its turn running in the background.
    SwitchTab {
        id: u64,
        conf: Config,
        msgs: Vec<ChatMsg>,
    },
    /// Stop and drop tab `id`'s engine (tab closed, or its session replaced).
    CloseTab(u64),
    Approve {
        id: u64,
        decision: ApprovalDecision,
    },
    Answer {
        id: u64,
        text: String,
    },
    Rewind {
        id: u64,
    },
    SetHistory(Vec<(String, String)>),
    SubagentControl {
        worker_id: String,
        action: SubagentControlAction,
    },
    Interrupt,
}

/// One agent session tab (its own provider + transcript) within a workspace.
#[derive(Clone, PartialEq)]
struct AgentTab {
    id: u64,
    title: String,
    provider: String,
    model: String,
    harness: String,
    reasoning_effort: String,
    messages: Vec<ChatMsg>,
    /// "gui" = chat, "tui" = embedded terminal running a CLI.
    mode: String,
    /// CLI binary for tui mode (e.g. "codex", "claude").
    bin: String,
    /// Session file backing this tab's model context (resume on switch).
    session: Option<PathBuf>,
    /// For a "tui" tab: the originating CLI session id to resume (so a TUI tab
    /// opened from a codex/claude chat continues it instead of starting fresh).
    resume: Option<String>,
}

const SESSION_RENDER_MESSAGE_LIMIT: usize = 20;

#[derive(Clone, PartialEq, Eq)]
enum TabStatus {
    Running,
    WaitingApproval,
    WaitingInput,
    Failed,
}

#[derive(Clone, PartialEq, Eq)]
struct DeletedSessionSpec {
    id: String,
    workspace: String,
    provider: String,
    title: String,
    pinned: bool,
    messages: Vec<(String, String)>,
}

#[derive(Clone, PartialEq, Eq)]
enum ToastAction {
    RestoreSessions(Vec<String>),
    RestoreDeletedSession(DeletedSessionSpec),
}

#[derive(Clone, PartialEq, Eq)]
struct ToastSpec {
    id: u64,
    kind: String,
    text: String,
    action_label: Option<String>,
    action: Option<ToastAction>,
}

#[derive(Clone, PartialEq, Eq)]
struct TextAttachment {
    name: String,
    rel_path: String,
    lines: usize,
    chars: usize,
}

#[derive(Clone, PartialEq, Eq)]
struct SessionListItem {
    id: String,
    title: String,
    count: usize,
    path: PathBuf,
    provider: String,
}

type ProjectGroup = (PathBuf, String, Vec<(PathBuf, String, String, String)>);
const PROJECT_SESSION_LIMIT: usize = 500;
const PROJECT_SESSION_PAGE_SIZE: usize = 5;

/// One row in the inspector Timeline.
#[derive(Clone, PartialEq)]
struct TimelineItem {
    title: String,
    sub: String,
}

#[derive(Clone, PartialEq)]
struct SubagentCard {
    worker_id: String,
    profile: String,
    task: String,
    summary: String,
    running: bool,
    ok: bool,
    logs: Vec<CommandLog>,
}

#[derive(Clone, PartialEq)]
struct CommandLog {
    command_id: String,
    command: String,
    output: String,
    running: bool,
    ok: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VisualFixtureMode {
    Streaming,
}

impl VisualFixtureMode {
    fn from_env() -> Option<Self> {
        match std::env::var("OXIDE_GUI_VISUAL_FIXTURE").ok().as_deref() {
            Some("streaming") => Some(Self::Streaming),
            _ => None,
        }
    }
}

fn visual_fixture_messages(mode: Option<VisualFixtureMode>) -> Vec<ChatMsg> {
    if !matches!(mode, Some(VisualFixtureMode::Streaming)) {
        return Vec::new();
    }
    vec![
        ChatMsg {
            author: Author::User,
            text: "Audit the Oxide GUI motion states and harden the harness parity pass."
                .to_string(),
        },
        ChatMsg {
            author: Author::Activity {
                running: true,
                ok: true,
                key: Some("visual-tool".to_string()),
            },
            text: tool_input_preview_label(
                "browser_search",
                "{\"query\":\"oxide gui visual qa cursor parity\"}",
            ),
        },
        ChatMsg {
            author: Author::Activity {
                running: true,
                ok: true,
                key: Some("visual-command".to_string()),
            },
            text: command_activity_label(
                "cargo test -p oxide-core gui_visual_fixture_screenshot",
                false,
            ),
        },
        ui_spec_message(visual_fixture_ui_spec()),
        ChatMsg {
            author: Author::Agent,
            text: String::new(),
        },
    ]
}

fn visual_fixture_ui_spec() -> UiSpec {
    UiSpec {
        title: Some("Cursor-grade Visual QA".to_string()),
        root: UiNode {
            id: Some("visual-qa-root".to_string()),
            kind: UiNodeKind::Card,
            props: oxide_protocol::UiProps {
                title: Some("Rust-native UI Spec".to_string()),
                caption: Some("Rendered by Dioxus from a typed Oxide protocol spec.".to_string()),
                ..Default::default()
            },
            children: vec![
                UiNode {
                    id: Some("metrics".to_string()),
                    kind: UiNodeKind::Row,
                    props: Default::default(),
                    children: vec![
                        UiNode {
                            id: Some("state".to_string()),
                            kind: UiNodeKind::Metric,
                            props: oxide_protocol::UiProps {
                                label: Some("Native state".to_string()),
                                value: Some("streaming".to_string()),
                                tone: Some(UiTone::Info),
                                ..Default::default()
                            },
                            children: Vec::new(),
                        },
                        UiNode {
                            id: Some("qa".to_string()),
                            kind: UiNodeKind::Metric,
                            props: oxide_protocol::UiProps {
                                label: Some("Visual QA".to_string()),
                                value: Some("seeded".to_string()),
                                tone: Some(UiTone::Success),
                                ..Default::default()
                            },
                            children: Vec::new(),
                        },
                    ],
                },
                UiNode {
                    id: Some("table".to_string()),
                    kind: UiNodeKind::Table,
                    props: oxide_protocol::UiProps {
                        columns: vec![
                            oxide_protocol::UiTableColumn {
                                key: "layer".to_string(),
                                label: "Layer".to_string(),
                            },
                            oxide_protocol::UiTableColumn {
                                key: "status".to_string(),
                                label: "Status".to_string(),
                            },
                        ],
                        rows: vec![
                            std::collections::BTreeMap::from([
                                ("layer".to_string(), serde_json::json!("Protocol")),
                                ("status".to_string(), serde_json::json!("typed")),
                            ]),
                            std::collections::BTreeMap::from([
                                ("layer".to_string(), serde_json::json!("GUI")),
                                ("status".to_string(), serde_json::json!("Dioxus")),
                            ]),
                        ],
                        ..Default::default()
                    },
                    children: Vec::new(),
                },
            ],
        },
    }
}

fn visual_fixture_thinking(mode: Option<VisualFixtureMode>) -> String {
    if matches!(mode, Some(VisualFixtureMode::Streaming)) {
        "Checking streamed tool arguments, reasoning placement, edit shimmer, and native window capture."
            .to_string()
    } else {
        String::new()
    }
}

fn visual_fixture_status(mode: Option<VisualFixtureMode>) -> String {
    if matches!(mode, Some(VisualFixtureMode::Streaming)) {
        "Running native GUI visual fixture".to_string()
    } else {
        String::new()
    }
}

fn visual_fixture_subagents(mode: Option<VisualFixtureMode>) -> Vec<SubagentCard> {
    if !matches!(mode, Some(VisualFixtureMode::Streaming)) {
        return Vec::new();
    }
    vec![SubagentCard {
        worker_id: "visual-motion-auditor".to_string(),
        profile: "reviewer".to_string(),
        task: "GUI motion parity".to_string(),
        summary: "Auditing waiting, reasoning, activity, and edit-review states.".to_string(),
        running: true,
        ok: true,
        logs: vec![CommandLog {
            command_id: "visual-check".to_string(),
            command: "python3 scripts/gui-visual-qa.py --runtime".to_string(),
            output: "Checking fixture selectors and PNG pixel sanity...".to_string(),
            running: true,
            ok: true,
        }],
    }]
}

fn visual_fixture_turn_edits(
    mode: Option<VisualFixtureMode>,
) -> Vec<(String, u32, u32, u64, String)> {
    if !matches!(mode, Some(VisualFixtureMode::Streaming)) {
        return Vec::new();
    }
    vec![
        (
            "crates/oxide-gui/src/lib.rs".to_string(),
            0,
            0,
            0,
            String::new(),
        ),
        (
            "scripts/gui-native-visual-smoke.py".to_string(),
            64,
            3,
            42,
            "@@ visual smoke @@\n+ capture native window region\n+ validate PNG pixels\n"
                .to_string(),
        ),
    ]
}

fn visual_fixture_todos(mode: Option<VisualFixtureMode>) -> Vec<(String, String)> {
    if !matches!(mode, Some(VisualFixtureMode::Streaming)) {
        return Vec::new();
    }
    vec![
        (
            "Verify native Dioxus screenshot artifact".to_string(),
            "in_progress".to_string(),
        ),
        (
            "Record remaining golden-diff follow-up".to_string(),
            "pending".to_string(),
        ),
    ]
}

#[derive(Clone, PartialEq)]
struct ReplayRow {
    role: String,
    title: String,
    detail: String,
}

#[derive(Clone, PartialEq)]
struct SessionReplay {
    path: PathBuf,
    title: String,
    rows: Vec<ReplayRow>,
    total: usize,
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

/// Walk the workspace for files/folders matching `query` (codebase `@` picker).
fn mention_candidates(ws: &Path, query: &str) -> Vec<String> {
    let q = query.to_ascii_lowercase();
    let mut out: Vec<(bool, String)> = Vec::new();
    let mut stack = vec![ws.to_path_buf()];
    let mut visited = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
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
// Leading "/query" (no space yet) -> drive the slash-command menu.
let slash=null;
if (el) {
  const sm = el.textContent.trim().match(/^\/([a-zA-Z0-9_-]*)$/);
  if (sm) slash = sm[1];
}
return JSON.stringify({q, empty, slash});
"#;

/// JS: serialize the editor into `{body, tokens}` for submission.
const CE_SERIALIZE_JS: &str = r#"
const el=document.getElementById('ce-input'); if(!el) return '{}';
let body=''; const tokens=[];
function walk(n){
  n.childNodes.forEach(c=>{
    if(c.nodeType===3) body+=c.textContent;
    else if(c.nodeName==='BR') body+='\n';
    else if(c.classList && c.classList.contains('ce-chip')){ tokens.push(c.dataset.token); body+='@'+(c.textContent||''); }
    else {
      if(body && !body.endsWith('\n') && (c.nodeName==='DIV'||c.nodeName==='P')) body+='\n';
      walk(c);
    }
  });
}
walk(el);
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

fn ce_insert_plain_text_js(text: &str) -> String {
    let text = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into());
    format!(
        r#"
const ed=document.getElementById('ce-input'); if(!ed) return false;
ed.focus();
document.execCommand('insertText', false, {text});
ed.dispatchEvent(new InputEvent('input',{{bubbles:true}}));
return true;
"#,
        text = text
    )
}

fn clipboard_write_js(text: &str) -> String {
    let text = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into());
    format!(
        r#"
const text = {text};
if (navigator.clipboard && navigator.clipboard.writeText) {{
  navigator.clipboard.writeText(text).catch(() => fallbackCopy(text));
  return true;
}}
return fallbackCopy(text);

function fallbackCopy(value) {{
const ta = document.createElement('textarea');
ta.value = value;
ta.setAttribute('readonly', '');
ta.style.position = 'fixed';
ta.style.top = '-9999px';
ta.style.left = '-9999px';
document.body.appendChild(ta);
ta.select();
try {{
  return document.execCommand('copy');
}} finally {{
  document.body.removeChild(ta);
}}
}}
"#,
        text = text
    )
}

fn copy_text_to_clipboard(text: String) {
    spawn(async move {
        let js = clipboard_write_js(&text);
        let _ = dioxus::document::eval(&js).join::<bool>().await;
    });
}

/// Split user text into `(is_mention, text)` segments — `@word` at a word
/// boundary becomes a mention pill.
/// Strip the prompt scaffolding the composer injects (context files, MCP/skill
/// blocks, plan/pursue tags, git context, picked-element, image notes) so a
/// persisted/resumed user message renders as just the human text + chips.
/// Write pasted-image data URLs to `<ws>/.oxide/attachments/` and return
/// `wsimg:<relpath>` markers (kept out of git via the .oxide ignore).
fn save_attachments(ws: &Path, atts: &[String]) -> Vec<String> {
    use base64::Engine;
    let dir = ws.join(".oxide/attachments");
    let _ = std::fs::create_dir_all(&dir);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut out = Vec::new();
    for (i, src) in atts.iter().enumerate() {
        // data:image/png;base64,XXXX
        let Some(comma) = src.find(',') else { continue };
        let meta = &src[..comma];
        let ext = if meta.contains("jpeg") || meta.contains("jpg") {
            "jpg"
        } else if meta.contains("gif") {
            "gif"
        } else if meta.contains("webp") {
            "webp"
        } else {
            "png"
        };
        let Ok(bytes) =
            base64::engine::general_purpose::STANDARD.decode(&src.as_bytes()[comma + 1..])
        else {
            continue;
        };
        let name = format!("att-{stamp}-{i}.{ext}");
        if std::fs::write(dir.join(&name), &bytes).is_ok() {
            out.push(format!("wsimg:.oxide/attachments/{name}"));
        }
    }
    out
}

fn save_pasted_text_attachment(ws: &Path, id: u64, text: &str) -> std::io::Result<TextAttachment> {
    let dir = ws.join(".oxide/attachments");
    std::fs::create_dir_all(&dir)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let disk_name = format!("pasted-text-{stamp}-{id}.txt");
    let path = dir.join(&disk_name);
    std::fs::write(&path, text.as_bytes())?;
    let name = if id == 1 {
        "Pasted text.txt".to_string()
    } else {
        format!("Pasted text {id}.txt")
    };
    let lines = text.lines().count().max(1);
    let chars = text.chars().count();
    Ok(TextAttachment {
        name,
        rel_path: format!(".oxide/attachments/{disk_name}"),
        lines,
        chars,
    })
}

fn text_attachment_name(rel_path: &str) -> String {
    Path::new(rel_path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Pasted text.txt".to_string())
}

/// Human token count: 272_000 to "272k", 1_000_000 to "1M".
fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        if n.is_multiple_of(1_000_000) {
            format!("{}M", n / 1_000_000)
        } else {
            format!("{:.1}M", n as f64 / 1_000_000.0)
        }
    } else {
        format!("{}k", n / 1000)
    }
}

/// Which subscription quota a provider draws from ("gpt", "claude", or "").
fn provider_family(p: &str) -> &'static str {
    match p {
        "chatgpt" | "codex" | "openai" => "gpt",
        "claude" | "claude_interactive" | "anthropic" => "claude",
        _ => "",
    }
}

fn strip_scaffold(text: &str) -> String {
    const DROP_PREFIX: &[&str] = &[
        "Context files:",
        "Use these MCP servers",
        "- `",
        "## Skill:",
        "## Git context",
        "## Working git diff",
        "### status",
        "### recent commits",
        "### working diff",
        "(Use the `",
        "[Preview selection",
        "[Plan mode]",
        "[Pursue goal]",
        "(user attached",
        "- selector:",
        "- component:",
        "- source:",
        "- text:",
        "- html:",
        "Selected UI element",
        "Run automation now",
        "Name:",
        "Kind:",
        "Schedule:",
        "Status:",
        "Automation prompt:",
        "## Automation request",
        "## Automation context",
    ];
    // Display messages may carry image data-URLs after a \u{2} separator —
    // those are render-only; never let them leak into copies/history/titles.
    let text = text.split('\u{2}').next().unwrap_or(text);
    let mut keep = Vec::new();
    let mut in_diff_fence = false;
    for line in text.lines() {
        let l = line.trim_start();
        if in_diff_fence {
            if l.starts_with("```") {
                in_diff_fence = false;
            }
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
    if let Some(rest) = token.strip_prefix("automation:") {
        if rest == "create" {
            return "Create automation".to_string();
        }
        return rest
            .split_once('|')
            .map(|(_, name)| name)
            .unwrap_or(rest)
            .trim_end_matches('/')
            .to_string();
    }
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
    mut text_attachments: Signal<Vec<TextAttachment>>,
    mut picked_element: Signal<Option<String>>,
    steer: bool,
    ws: PathBuf,
) {
    let json = dioxus::document::eval(CE_SERIALIZE_JS)
        .join::<String>()
        .await
        .unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap_or(serde_json::Value::Null);
    let body = v["body"].as_str().unwrap_or("").trim().to_string();
    let tokens: Vec<String> = v["tokens"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let n_imgs = attachments.read().len();
    let text_files = text_attachments.read().clone();
    let n_text_files = text_files.len();
    let picked = picked_element.read().clone();
    if body.is_empty() && tokens.is_empty() && n_imgs == 0 && n_text_files == 0 && picked.is_none()
    {
        return;
    }
    // Built-in /review (Bugbot): review the working diff for bugs.
    if body.trim_start().starts_with("/review") {
        let _ = dioxus::document::eval("const e=document.getElementById('ce-input'); if(e){ e.innerHTML=''; e.dispatchEvent(new InputEvent('input',{bubbles:true})); }").join::<bool>().await;
        let extra = body.trim_start().trim_start_matches("/review").trim();
        let diff = run_cmd(&ws, "git", &["diff"]).await;
        let diff: String = diff.chars().take(12000).collect();
        let prompt = format!(
            "Act as Bugbot. Review the current working changes for bugs, security issues, \
logic errors, and regressions. For each finding give: file:line, severity (high/med/low), \
why it's wrong, and the concrete fix. If the diff is clean, say so plainly.{}\n\n```diff\n{}\n```",
            if extra.is_empty() {
                String::new()
            } else {
                format!(" Extra focus: {extra}.")
            },
            diff
        );
        if *streaming.read() {
            queue.write().push(prompt);
        } else {
            engine.send(EngineCmd::Submit {
                engine: prompt,
                display: "/review (Bugbot)".into(),
            });
        }
        return;
    }
    // Clear the editor immediately so a rapid second Enter can't double-submit
    // (the next serialize reads an empty body and returns).
    let _ = dioxus::document::eval("const e=document.getElementById('ce-input'); if(e){ e.innerHTML=''; e.dispatchEvent(new InputEvent('input',{bubbles:true})); }")
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
            text.push_str(&format!(
                "[Pursue goal] Keep working autonomously until this goal is fully done: {}\n\n",
                g.trim()
            ));
        }
    }
    let mut files = Vec::new();
    let mut skills_block = String::new();
    let mut mcp_block = String::new();
    let mut ctx_block = String::new();
    for tkn in &tokens {
        if let Some(name) = tkn.strip_prefix("mcp:") {
            mcp_block.push_str(&format!(
                "\n- `{name}` — call its tools via `mcp__{name}__*`"
            ));
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
        } else if let Some(rest) = tkn.strip_prefix("automation:") {
            if rest == "create" {
                ctx_block.push_str(
                    "\n## Automation request\nThe user selected Create automation from the @ menu. Help them define a useful workspace automation. If enough details are present, create a `.oxide/automations/*.toml` automation spec with fields `id`, `name`, `kind = \"cron\"`, `status = \"ACTIVE\"`, `schedule`, `prompt`, and `created_ms`. Schedule formats: interval `FREQ=MINUTELY|HOURLY|DAILY;INTERVAL=N`; daily at a clock time `FREQ=DAILY;AT=09:00` (add `;TZ=+07:00` for a timezone offset, `;INTERVAL=N` for every N days); weekly on weekdays `FREQ=WEEKLY;BYDAY=MO,WE,FR;AT=09:00`; one-shot `ONCE=<unix_ms>`.\n",
                );
            } else {
                let (id, name) = rest.split_once('|').unwrap_or((rest, rest));
                ctx_block.push_str(&format!(
                    "\n## Automation context\nSelected automation: `{}` ({id}). Use this when the user asks to review, update, pause, or run that automation.\n",
                    name
                ));
            }
        } else {
            files.push(format!("@{tkn}"));
        }
    }
    if !ctx_block.is_empty() {
        text.push_str(&ctx_block);
        text.push('\n');
    }
    for att in &text_files {
        let path = ws.join(&att.rel_path);
        match std::fs::read_to_string(&path) {
            Ok(full) => {
                let full = full.trim_end();
                let chars = full.chars().count();
                let (body, note) = if chars > 24_000 {
                    let preview: String = full.chars().take(12_000).collect();
                    (
                        format!("{preview}\n\n… [attachment preview truncated at 12000 chars; full text is saved at {}]", att.rel_path),
                        "The full pasted text is saved on disk. Use `read_file` or `search` on this path if you need content beyond the preview."
                    )
                } else {
                    (
                        full.to_string(),
                        "The full pasted text is included below and also saved at this path.",
                    )
                };
                text.push_str(&format!(
                    "\n## Attached text file: {}\nPath: {}\n{}\n````text\n{}\n````\n",
                    att.name, att.rel_path, note, body
                ));
            }
            Err(err) => {
                text.push_str(&format!(
                    "\n## Attached text file: {}\nPath: {}\n[Could not read attachment: {err}]\n",
                    att.name, att.rel_path
                ));
            }
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
    if !tokens.is_empty() {
        text.push('\n');
    }
    if n_imgs > 0 {
        text.push_str(&format!("\n(user attached {n_imgs} image{} — image content is NOT visible to you; ask the user to describe it if needed)", if n_imgs == 1 { "" } else { "s" }));
    }
    if let Some(p) = &picked {
        text.push_str(&format!(
            "\n[Preview selection — change this element]\n{p}\n"
        ));
        picked_element.set(None);
    }
    text.push_str(&body);
    // Carry lightweight attachment refs after a \u{2} separator on BOTH the
    // model text and the display, so sent attachments survive session reload.
    let img_markers: Vec<String> = if n_imgs > 0 {
        save_attachments(&ws, &attachments.read())
    } else {
        Vec::new()
    };
    let text_markers: Vec<String> = text_files
        .iter()
        .map(|att| format!("wstxt:{}", att.rel_path))
        .collect();
    let marker_suffix: String = img_markers
        .iter()
        .chain(text_markers.iter())
        .map(|m| format!("\u{2}{m}"))
        .collect();
    if !marker_suffix.is_empty() {
        text.push_str(&marker_suffix); // persisted with the user turn; reload renders them
    }
    let display = if marker_suffix.is_empty() {
        body.clone()
    } else {
        format!("{body}{marker_suffix}")
    };
    attachments.write().clear();
    text_attachments.write().clear();
    if !steer && *streaming.read() {
        queue.write().push(text);
    } else {
        engine.send(EngineCmd::Submit {
            engine: text,
            display,
        });
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
    let fm = text
        .strip_prefix("---")
        .and_then(|r| r.find("\n---").map(|e| r[..e].to_string()));
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
                let name = p
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let desc = std::fs::read_to_string(&p)
                    .ok()
                    .and_then(|t| {
                        t.lines()
                            .find(|l| !l.trim().is_empty())
                            .map(|l| l.trim().trim_start_matches('#').trim().to_string())
                    })
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
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
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

async fn sync_board_issues_once(
    mut board: Signal<board::Board>,
    root: PathBuf,
    mut status: Signal<String>,
) {
    status.set("Syncing issues…".to_string());
    let fetched = board::fetch_issue_cards(&root).await;
    let (added, updated) = {
        let mut b = board.write();
        let result = b.upsert_issues(fetched.issues.clone());
        let snapshot = b.clone();
        drop(b);
        snapshot.save(&root);
        result
    };
    status.set(board::sync_summary(&fetched, added, updated));
}

fn sync_board_issues(
    board: Signal<board::Board>,
    root: PathBuf,
    status: Signal<String>,
    mut syncing: Signal<bool>,
) {
    if *syncing.read() {
        return;
    }
    syncing.set(true);
    spawn(async move {
        sync_board_issues_once(board, root, status).await;
        syncing.set(false);
    });
}

#[allow(clippy::too_many_arguments)]
fn run_automation_turn(
    workspace: PathBuf,
    spec: automation::AutomationSpec,
    trigger: &'static str,
    engine: Coroutine<EngineCmd>,
    streaming: Signal<bool>,
    mut queue: Signal<Vec<String>>,
    mut runs: Signal<Vec<automation::AutomationRunSpec>>,
    mut status: Signal<String>,
) {
    let run = automation::run_from_spec(&spec, trigger, "queued", automation::now_ms());
    match automation::write_run(&workspace, &run) {
        Ok(()) => {
            if let Ok(next_runs) = automation::read_runs(&workspace) {
                runs.set(next_runs);
            }
            let prompt = automation::build_run_prompt(&spec);
            let label = format!("Run automation: {}", spec.name);
            if *streaming.read() {
                queue.write().push(prompt);
                status.set(format!("Queued automation: {}", spec.name));
            } else {
                engine.send(EngineCmd::Submit {
                    engine: prompt,
                    display: label,
                });
                status.set(format!("Started automation: {}", spec.name));
            }
        }
        Err(err) => status.set(format!("Automation failed: {err}")),
    }
}

fn relative_ms(value: u64) -> String {
    let now = automation::now_ms();
    let secs = now.saturating_sub(value) / 1000;
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
    engine.send(EngineCmd::Reconfigure(c));
    show_access.set(false);
}

/// Available harness ids from the same registry path used by the engine.
fn list_harnesses(config: &Config) -> Vec<String> {
    let mut registry = oxide_harness::Registry::with_builtins();
    let workspace = Some(workspace_of(config));
    for dir in oxide_harness::manifest_dirs(config.harness_dir.as_deref(), workspace.as_deref()) {
        let _ = registry.load_dir(&dir);
    }
    registry.ids()
}

/// Available slash commands `(name, description)` matching `query`.
fn slash_commands(ws: &Path, query: &str) -> Vec<(String, String)> {
    let q = query.to_ascii_lowercase();
    // Built-in commands handled by the composer itself.
    let builtins = [("review", "Bugbot — review the working git diff for bugs")];
    let mut v: Vec<(String, String)> = builtins
        .iter()
        .filter(|(n, _)| q.is_empty() || n.contains(&q))
        .map(|(n, d)| (n.to_string(), d.to_string()))
        .collect();
    let dir = ws.join(".oxide/commands");
    v.extend(
        std::fs::read_dir(&dir)
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
                                    l.trim()
                                        .strip_prefix("description:")
                                        .map(|d| d.trim().trim_matches('"').to_string())
                                })
                            })
                    })
                    .unwrap_or_default();
                Some((name, desc))
            }),
    );
    v.sort();
    v.dedup();
    v
}

/// Combined `@` menu: skills first, then files/folders.
/// Trusted/configured MCP servers matching `query`, as `mcp:<server>` tokens.
fn mcp_candidates(ws: &Path, query: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    if let Ok(mut cfg) = Config::load() {
        let _ = cfg.overlay_file(&ws.join("oxide.toml"));
        for s in cfg.mcp_servers {
            if s.enabled {
                names.push(s.name);
            }
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

fn automation_candidates(ws: &Path, query: &str) -> Vec<String> {
    let q = query.to_ascii_lowercase();
    let mut out = Vec::new();
    if q.is_empty() || "automation".contains(&q) || "create automation".contains(&q) {
        out.push("automation:create".to_string());
    }
    if let Ok(specs) = automation::read_specs(ws) {
        for spec in specs {
            let hay = format!("{} {}", spec.id, spec.name).to_ascii_lowercase();
            if q.is_empty() || hay.contains(&q) {
                out.push(format!("automation:{}|{}", spec.id, spec.name));
            }
            if out.len() >= 10 {
                break;
            }
        }
    }
    out
}

fn all_mention_items(ws: &Path, query: &str) -> Vec<String> {
    let q = query.to_ascii_lowercase();
    // Special context providers (Cursor-style @git / @web / @codebase).
    let mut v: Vec<String> = ["ctx:git", "ctx:diff", "ctx:codebase", "ctx:web"]
        .iter()
        .filter(|t| q.is_empty() || t.contains(&q))
        .map(|t| t.to_string())
        .collect();
    v.extend(automation_candidates(ws, query));
    v.extend(mcp_candidates(ws, query));
    v.extend(skill_candidates(ws, query));
    v.extend(mention_candidates(ws, query));
    v
}

/// List persisted sessions from the global DB, matching the sidebar source.
fn list_sessions(ws: &Path) -> Vec<SessionListItem> {
    oxide_core::db::import_codex_desktop_threads_for_workspaces([ws.to_path_buf()], 300);
    oxide_core::db::import_workspace(ws);
    oxide_core::db::import_claude_sessions(ws);
    oxide_core::db::list(ws, 50)
        .into_iter()
        .map(|m| {
            let title = {
                let clean = strip_scaffold(&m.title);
                clean
                    .lines()
                    .find(|x| !x.trim().is_empty())
                    .unwrap_or("Chat")
                    .chars()
                    .take(52)
                    .collect::<String>()
            };
            let count = oxide_core::db::message_count(&m.id);
            SessionListItem {
                path: PathBuf::from(&m.id),
                id: m.id,
                title,
                count,
                provider: m.provider,
            }
        })
        .collect()
}

/// Delete a saved session file.
/// Session id carried in the PathBuf-typed handles the UI passes around.
fn sid(path: &Path) -> String {
    path.display().to_string()
}

fn delete_session(path: &Path) {
    oxide_core::db::delete(&sid(path));
}

fn archive_session(path: &Path) {
    oxide_core::db::archive(&sid(path));
}

fn capture_deleted_session(path: &Path) -> Option<DeletedSessionSpec> {
    let id = sid(path);
    let meta = oxide_core::db::meta(&id)?;
    let messages = oxide_core::db::load(&id);
    if messages.is_empty() {
        return None;
    }
    Some(DeletedSessionSpec {
        id,
        workspace: meta.workspace,
        provider: meta.provider,
        title: meta.title,
        pinned: meta.pinned,
        messages,
    })
}

fn restore_deleted_session(spec: &DeletedSessionSpec) {
    let workspace = PathBuf::from(&spec.workspace);
    oxide_core::db::rewrite(&spec.id, &workspace, &spec.provider, &spec.messages);
    if !spec.title.trim().is_empty() {
        oxide_core::db::set_title(&spec.id, &spec.title);
    }
    oxide_core::db::set_pinned(&spec.id, spec.pinned);
    oxide_core::db::restore(&spec.id);
}

/// Recent non-empty sessions `(path, title, msg_count)`, newest first. Deletes
/// empty/0-byte session files as it scans (cleanup).
fn recent_sessions(ws: &Path) -> Vec<(PathBuf, std::time::SystemTime, String, String)> {
    // Import legacy JSONL + Claude Code TUI transcripts, then query the global db.
    oxide_core::db::import_workspace(ws);
    oxide_core::db::import_claude_sessions(ws);
    db_recent_sessions(ws, 30)
}

/// Recent sessions from the global DB only. Used for inactive `/Volumes/...`
/// projects so their chat rows stay visible without touching the volume.
fn db_recent_sessions(
    ws: &Path,
    limit: usize,
) -> Vec<(PathBuf, std::time::SystemTime, String, String)> {
    oxide_core::db::list(ws, limit)
        .into_iter()
        .map(|m| {
            let t = std::time::UNIX_EPOCH
                + std::time::Duration::from_millis(m.updated_ms.max(0) as u64);
            let title = {
                let clean = strip_scaffold(&m.title);
                clean
                    .lines()
                    .find(|x| !x.trim().is_empty())
                    .unwrap_or("Chat")
                    .chars()
                    .take(38)
                    .collect::<String>()
            };
            (PathBuf::from(m.id), t, title, m.provider)
        })
        .collect()
}

/// Short relative time like "5m", "3h", "2d", "1w".
fn relative_time(t: std::time::SystemTime) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(t)
        .map(|d| d.as_secs())
        .unwrap_or(0);
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

#[cfg(target_os = "macos")]
fn is_macos_volume_path(path: &Path) -> bool {
    use std::path::Component;

    let mut components = path.components();
    matches!(components.next(), Some(Component::RootDir))
        && matches!(components.next(), Some(Component::Normal(name)) if name == "Volumes")
}

#[cfg(not(target_os = "macos"))]
fn is_macos_volume_path(_path: &Path) -> bool {
    false
}

fn should_defer_recent_workspace_scan(current: &Path, workspace: &Path) -> bool {
    if !is_macos_volume_path(workspace) {
        return false;
    }
    if !current.as_os_str().is_empty() && workspace == current {
        return false;
    }
    std::env::var_os("OXIDE_SCAN_RECENT_VOLUMES").is_none()
}

/// Group recent sessions by project: `(workspace, name, [(path, title, reltime)])`.
fn build_projects(current: &Path, recents: &[PathBuf]) -> Vec<ProjectGroup> {
    let mut seen = HashSet::new();
    let mut wss: Vec<(PathBuf, bool)> = Vec::new();
    let opened_by_oxide: Vec<PathBuf> = oxide_core::db::workspaces_opened_by_oxide()
        .into_iter()
        .map(PathBuf::from)
        .collect();
    let mut known_workspaces: HashSet<String> = HashSet::new();
    for w in std::iter::once(current.to_path_buf())
        .chain(recents.iter().cloned())
        .chain(opened_by_oxide.iter().cloned())
    {
        if !w.as_os_str().is_empty() {
            known_workspaces.insert(w.display().to_string());
        }
    }
    let import_workspaces: Vec<PathBuf> = known_workspaces.iter().map(PathBuf::from).collect();
    oxide_core::db::import_codex_desktop_threads_for_workspaces(import_workspaces, 500);
    // STABLE order: db recency first (only changes when you actually chat, not
    // when you click to switch), then the current workspace + recents as a
    // fallback so a brand-new project still appears. Clicking never reorders.
    let db_ws: Vec<PathBuf> = oxide_core::db::workspaces()
        .into_iter()
        .filter(|w| known_workspaces.contains(w))
        .map(PathBuf::from)
        .collect();
    for w in db_ws
        .into_iter()
        .chain(std::iter::once(current.to_path_buf()))
        .chain(recents.iter().cloned())
    {
        if w.as_os_str().is_empty() || !seen.insert(w.clone()) {
            continue;
        }
        let deferred = should_defer_recent_workspace_scan(current, &w);
        if deferred || w.exists() {
            wss.push((w, deferred));
        }
    }
    let mut out = Vec::new();
    for (ws, deferred) in wss {
        // Group each project's OWN chats under it (synara-style), so a chat
        // always appears under the folder it belongs to — not just the active
        // one. These are user-opened folders, so access is already granted.
        let sessions = if deferred {
            db_recent_sessions(&ws, PROJECT_SESSION_LIMIT)
        } else {
            recent_sessions(&ws)
                .into_iter()
                .take(PROJECT_SESSION_LIMIT)
                .collect()
        };
        let items: Vec<(PathBuf, String, String, String)> = sessions
            .into_iter()
            .map(|(p, m, t, prov)| (p, t, relative_time(m), prov))
            .collect();
        let name = project_name(&ws);
        out.push((ws, name, items));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_workspace_scan_is_not_deferred() {
        let workspace = Path::new("/Volumes/Data/oxide");

        assert!(!should_defer_recent_workspace_scan(workspace, workspace));
    }

    #[test]
    fn inactive_macos_volume_scan_is_deferred_by_default() {
        let current = Path::new("/Users/example/project");
        let recent = Path::new("/Volumes/Data/oxide");
        let expected =
            cfg!(target_os = "macos") && std::env::var_os("OXIDE_SCAN_RECENT_VOLUMES").is_none();

        assert_eq!(
            should_defer_recent_workspace_scan(current, recent),
            expected
        );
    }

    #[test]
    fn done_note_display_strips_duration_and_raw_check() {
        let text = format!("{DONE_NOTE_MARK} · 3m 1s · 2 file(s) +4 −1");
        let (label, meta) = done_note_display_parts(&text);

        assert_eq!(label, "Done");
        assert_eq!(meta, vec!["2 file(s) +4 −1"]);
    }

    #[test]
    fn done_note_display_keeps_non_duration_meta() {
        let text = format!("{DONE_NOTE_MARK} · 2 file(s) +4 −1");
        let (label, meta) = done_note_display_parts(&text);

        assert_eq!(label, "Done");
        assert_eq!(meta, vec!["2 file(s) +4 −1"]);
    }

    fn act(text: &str) -> ChatMsg {
        ChatMsg {
            author: Author::Activity {
                running: false,
                ok: true,
                key: None,
            },
            text: text.into(),
        }
    }
    fn note(text: &str) -> ChatMsg {
        ChatMsg {
            author: Author::Note,
            text: text.into(),
        }
    }

    #[test]
    fn done_summary_extracted_so_trailing_activity_stays_above_it() {
        // Buffer order with the Done note BEFORE trailing activity rows (the bug:
        // CLI tool events surfaced after TurnFinished landed below the footer).
        let done = format!("{DONE_NOTE_MARK} · 1m");
        let msgs = vec![
            ChatMsg {
                author: Author::User,
                text: "go".into(),
            },
            ChatMsg {
                author: Author::Agent,
                text: "working".into(),
            },
            note(&done),
            act("terminal\tBash\tgit status"),
            act("eye\tRead\tlib.rs"),
        ];
        let turns = build_transcript_turns(&msgs);
        assert_eq!(turns.len(), 1);
        // The Done note is pulled out of the row groups into `done_summary`
        // (the render then places it as the turn's last child), so NO group is
        // the Done note and the trailing activity group is the last group.
        assert_eq!(turns[0].done_summary.as_deref(), Some(done.as_str()));
        assert!(turns[0].groups.iter().all(|g| !g
            .indices
            .iter()
            .any(|&i| msgs[i].text.starts_with(DONE_NOTE_MARK))));
        assert!(turns[0].groups.last().unwrap().activity);
    }
}

/// Push a toast (kind: "ok" | "err" | "info") that auto-dismisses after 4s.
fn push_toast(mut toasts: Signal<Vec<ToastSpec>>, mut seq: Signal<u64>, kind: &str, text: &str) {
    let id = *seq.peek() + 1;
    seq.set(id);
    toasts.write().push(ToastSpec {
        id,
        kind: kind.to_string(),
        text: text.to_string(),
        action_label: None,
        action: None,
    });
    spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        toasts.write().retain(|t| t.id != id);
    });
}

fn push_action_toast(
    mut toasts: Signal<Vec<ToastSpec>>,
    mut seq: Signal<u64>,
    kind: &str,
    text: &str,
    action_label: &str,
    action: ToastAction,
) {
    let id = *seq.peek() + 1;
    seq.set(id);
    toasts.write().push(ToastSpec {
        id,
        kind: kind.to_string(),
        text: text.to_string(),
        action_label: Some(action_label.to_string()),
        action: Some(action),
    });
    spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(8)).await;
        toasts.write().retain(|t| t.id != id);
    });
}

/// Play the turn-done chime. `force` = always play (a background tab finished
/// while you're looking elsewhere). When false (the foreground turn you're
/// watching finished) the chime is suppressed if the window is focused — no
/// point dinging while you're already staring at the result. Volume is
/// user-configurable; simultaneous chimes overlap via a cloned element.
fn play_notification_sound(cfg: Signal<Config>, force: bool) {
    let c = cfg.peek();
    if !c.notification_sound {
        return;
    }
    let vol = c.notification_volume.clamp(0.0, 1.0);
    drop(c);
    spawn(async move {
        let js = format!(
            r#"
            try {{
              if (!{force} && document.hasFocus()) return true;
              const url = '/notify-sound/done.wav';
              const base = window.__oxideDoneAudio || new Audio(url);
              window.__oxideDoneAudio = base;
              // Overlap simultaneous chimes: if the shared element is mid-play,
              // ring a throwaway clone so neither one is cut short.
              const a = (!base.paused && base.currentTime > 0) ? base.cloneNode() : base;
              a.volume = {vol};
              a.currentTime = 0;
              const p = a.play();
              if (p && p.catch) p.catch(() => {{}});
            }} catch (_) {{}}
            return true;
        "#
        );
        let _ = dioxus::document::eval(&js).join::<bool>().await;
    });
}

fn flash_restored_sessions(mut restored_sessions: Signal<HashSet<String>>, ids: Vec<String>) {
    if ids.is_empty() {
        return;
    }
    {
        let mut set = restored_sessions.write();
        for id in &ids {
            set.insert(id.clone());
        }
    }
    spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(1600)).await;
        let mut set = restored_sessions.write();
        for id in ids {
            set.remove(&id);
        }
    });
}

fn tab_status_class(status: &TabStatus) -> &'static str {
    match status {
        TabStatus::Running => "running",
        TabStatus::WaitingApproval => "approval",
        TabStatus::WaitingInput => "input",
        TabStatus::Failed => "failed",
    }
}

fn tab_status_label(status: &TabStatus) -> &'static str {
    match status {
        TabStatus::Running => "run",
        TabStatus::WaitingApproval => "approve",
        TabStatus::WaitingInput => "input",
        TabStatus::Failed => "error",
    }
}

fn refresh_projects_list(mut projects_list: Signal<Vec<ProjectGroup>>, cfg: Signal<Config>) {
    let c = cfg.peek().clone();
    spawn(async move {
        let groups = tokio::task::spawn_blocking(move || {
            build_projects(&workspace_of(&c), &c.recent_workspaces)
        })
        .await
        .unwrap_or_default();
        projects_list.set(groups);
    });
}

fn active_tab_id(tabs: Signal<Vec<AgentTab>>, active_tab: Signal<usize>) -> Option<u64> {
    tabs.peek().get(*active_tab.peek()).map(|t| t.id)
}

fn select_env_tab(
    mut env_tab: Signal<String>,
    mut show_env: Signal<bool>,
    mut env_tab_by_tab: Signal<HashMap<u64, String>>,
    tabs: Signal<Vec<AgentTab>>,
    active_tab: Signal<usize>,
    tab: &str,
    toggle: bool,
) {
    if toggle && *show_env.peek() && env_tab.peek().as_str() == tab {
        show_env.set(false);
        return;
    }
    let next = tab.to_string();
    env_tab.set(next.clone());
    if let Some(id) = active_tab_id(tabs, active_tab) {
        env_tab_by_tab.write().insert(id, next);
    }
    show_env.set(true);
}

fn queue_preview(text: &str) -> String {
    let clean = strip_scaffold(text);
    if clean.starts_with("Act as Bugbot.") {
        return "/review (Bugbot)".to_string();
    }
    clean
        .lines()
        .map(str::trim)
        .find(|line| {
            !line.is_empty()
                && !line.starts_with("## ")
                && !line.starts_with("Context files:")
                && !line.starts_with("[Plan mode]")
                && !line.starts_with("[Pursue goal]")
        })
        .unwrap_or("queued prompt")
        .chars()
        .take(54)
        .collect()
}

/// Stem of the active tab's session file (per-thread storage key).
fn thread_stem(tabs: &Signal<Vec<AgentTab>>, active_tab: &Signal<usize>) -> String {
    let cur = *active_tab.peek();
    tabs.peek()
        .get(cur)
        .and_then(|t| {
            t.session
                .as_ref()
                .and_then(|p| p.file_stem().map(|x| x.to_string_lossy().to_string()))
        })
        .unwrap_or_else(|| "default".into())
}

fn thread_json_load<T: serde::de::DeserializeOwned + Default>(
    ws: &Path,
    dir: &str,
    stem: &str,
) -> T {
    std::fs::read_to_string(ws.join(format!(".oxide/{dir}/{stem}.json")))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn thread_json_save<T: serde::Serialize>(ws: &Path, dir: &str, stem: &str, v: &T) {
    let d = ws.join(format!(".oxide/{dir}"));
    let _ = std::fs::create_dir_all(&d);
    if let Ok(t) = serde_json::to_string(v) {
        let _ = std::fs::write(d.join(format!("{stem}.json")), t);
    }
}

/// Smooth-scroll the transcript to message index `i` and flash it.
fn jump_to_msg(i: usize) {
    spawn(async move {
        let _ = dioxus::document::eval(&format!(
            "const el=document.getElementById('msg-{i}'); if(el){{ el.scrollIntoView({{behavior:'smooth',block:'center'}}); el.classList.add('flash'); setTimeout(()=>el.classList.remove('flash'),1200); }}"
        )).await;
    });
}

/// Jump the chat scroll to the bottom after the next render tick.
fn scroll_chat_bottom() {
    spawn(async move {
        let _ = dioxus::document::eval(
            "for (const delay of [0, 40, 140]) setTimeout(()=>requestAnimationFrame(()=>{const s=document.querySelector('.scroll'); if(s) s.scrollTo({top:s.scrollHeight, behavior:'auto'});}),delay);",
        )
        .await;
    });
}

/// Keep the transcript pinned only when the user was already reading the tail.
fn scroll_chat_bottom_if_sticky() {
    spawn(async move {
        let _ = dioxus::document::eval(
            // Coalesce per-event follow-scrolls into one rAF (the __oxstickQ latch):
            // a streaming turn calls this on every delta, and stacking a scroll per
            // event on top of the MutationObserver snap is what makes the tail jitter.
            "if(window.__oxstickQ)return;window.__oxstickQ=1;requestAnimationFrame(()=>{window.__oxstickQ=0;const s=document.querySelector('.scroll'); if(!s) return; const d=s.scrollHeight-s.scrollTop-s.clientHeight; if(window.__oxstick!==false || d < 180) s.scrollTop=s.scrollHeight;});",
        )
        .await;
    });
}

/// Open a saved session transcript in a new tab (view).
#[allow(clippy::too_many_arguments)]
fn open_session_tab(
    mut tabs: Signal<Vec<AgentTab>>,
    mut active_tab: Signal<usize>,
    messages: Signal<Vec<ChatMsg>>,
    mut next_id: Signal<u64>,
    mut cfg: Signal<Config>,
    mut ui: Ui,
    engine: Coroutine<EngineCmd>,
    busy_tabs: Signal<HashSet<u64>>,
    path: PathBuf,
    title: String,
) {
    let loaded = load_session(&path);
    let session_runtime = |meta: Option<&oxide_core::db::SessionMeta>, base: &Config| {
        let provider = meta
            .map(|m| m.provider.clone())
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| base.provider.clone());
        let model = meta
            .map(|m| m.model.clone())
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| base.model.clone());
        let harness = meta
            .map(|m| m.harness.clone())
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| base.harness.clone());
        let effort = meta
            .map(|m| m.reasoning_effort.clone())
            .filter(|e| !e.is_empty())
            .unwrap_or_else(|| base.reasoning_effort.clone());
        (provider, model, harness, effort)
    };
    // If the current tab is mid-turn, NEVER replace it (that would kill its
    // engine and abort the running task). Open the session in a NEW tab instead
    // so folder A keeps working while you go look at folder B — synara-style.
    {
        let cur = *active_tab.peek();
        let cur_busy = tabs
            .peek()
            .get(cur)
            .map(|t| busy_tabs.peek().contains(&t.id))
            .unwrap_or(false);
        if cur_busy {
            // Save the live transcript into the busy tab before leaving it.
            if let Some(t) = tabs.write().get_mut(cur) {
                t.messages = messages.peek().clone();
            }
            let meta = oxide_core::db::meta(&sid(&path));
            let base_cfg = cfg.peek().clone();
            let (prov, model, harness, effort) = session_runtime(meta.as_ref(), &base_cfg);
            let id = *next_id.peek();
            next_id.set(id + 1);
            tabs.write().push(AgentTab {
                id,
                title: title.clone(),
                provider: prov.clone(),
                model: model.clone(),
                harness: harness.clone(),
                reasoning_effort: effort.clone(),
                messages: loaded.clone(),
                mode: "gui".into(),
                bin: String::new(),
                session: Some(path.clone()),
                resume: None,
            });
            let idx = tabs.peek().len() - 1;
            active_tab.set(idx);
            let mut c = base_cfg;
            c.provider = prov;
            c.model = model;
            c.harness = harness;
            c.reasoning_effort = effort;
            if let Some(ws) = oxide_core::db::meta(&sid(&path))
                .map(|m| PathBuf::from(m.workspace))
                .filter(|w| !w.as_os_str().is_empty())
            {
                ui.workspace.set(ws.clone());
                c.recent_workspaces.retain(|p| p != &ws);
                c.recent_workspaces.insert(0, ws.clone());
                c.recent_workspaces.truncate(8);
                c.workspace = Some(ws);
            }
            c.resume_path = Some(path);
            cfg.set(c.clone());
            engine.send(EngineCmd::SwitchTab {
                id,
                conf: c,
                msgs: loaded,
            });
            scroll_chat_bottom();
            return;
        }
    }
    let cur = *active_tab.read();
    // One metadata fetch for both the workspace and the provider (was two).
    let meta = oxide_core::db::meta(&sid(&path));
    // A session file lives at <workspace>/.oxide/sessions/<id>.jsonl — the
    // chat MUST run in that workspace, or the engine (in another folder)
    // appends this conversation into the wrong project.
    let session_ws = meta
        .as_ref()
        .map(|m| PathBuf::from(&m.workspace))
        .filter(|w| !w.as_os_str().is_empty());
    let mut c = cfg.read().clone();
    // Adopt the session's own runtime mode (provider/model/harness/effort), not
    // whatever the composer was last set to.
    let (sess_provider, sess_model, sess_harness, sess_effort) = session_runtime(meta.as_ref(), &c);
    c.provider = sess_provider.clone();
    c.model = sess_model.clone();
    c.harness = sess_harness.clone();
    c.reasoning_effort = sess_effort.clone();
    if let Some(t) = tabs.write().get_mut(cur) {
        t.provider = sess_provider;
        t.model = sess_model;
        t.harness = sess_harness;
        t.reasoning_effort = sess_effort;
    }
    if let Some(ws) = session_ws {
        if c.workspace.as_deref() != Some(ws.as_path()) {
            ui.workspace.set(ws.clone());
            ui.open_path.set(None);
            ui.expanded.set(HashSet::new());
            c.recent_workspaces.retain(|p| p != &ws);
            c.recent_workspaces.insert(0, ws.clone());
            c.recent_workspaces.truncate(8);
            c.workspace = Some(ws);
        }
    }
    // Open in the CURRENT tab (synara-style) — a sidebar click navigates, it
    // doesn't multiply tabs. New tabs come from the + button.
    let tab_id = tabs.read().get(cur).map(|t| t.id).unwrap_or(0);
    if let Some(t) = tabs.write().get_mut(cur) {
        t.title = title;
        t.messages = loaded.clone();
        t.session = Some(path.clone());
    }
    c.resume_path = Some(path);
    cfg.set(c.clone());
    // The tab's CONTENT changed (different session) — its old engine, if any,
    // holds the old history. Drop it; a fresh one resumes this session lazily.
    engine.send(EngineCmd::CloseTab(tab_id));
    engine.send(EngineCmd::SwitchTab {
        id: tab_id,
        conf: c,
        msgs: loaded,
    });
    scroll_chat_bottom();
}

/// Load a session transcript into chat messages.
fn load_session(path: &Path) -> Vec<ChatMsg> {
    let mut rows = oxide_core::db::load(&sid(path));
    // Long / repeatedly-compacted transcripts are expensive to paint (markdown +
    // syntax highlight per message). Show only the tail — the engine still
    // resumes the FULL history on continue, so nothing is lost from the model.
    let total = rows.len();
    let trimmed = total > SESSION_RENDER_MESSAGE_LIMIT;
    if trimmed {
        rows = rows.split_off(total - SESSION_RENDER_MESSAGE_LIMIT);
    }
    let mut out: Vec<ChatMsg> = rows
        .into_iter()
        .filter_map(|(role, content)| {
            if !matches!(role.as_str(), "user" | "assistant" | "summary" | "ui_spec") {
                return None;
            }
            let author = match role.as_str() {
                "user" => Author::User,
                "assistant" => Author::Agent,
                "ui_spec" => Author::UiSpec,
                _ => Author::Note,
            };
            Some(ChatMsg {
                author,
                text: content,
            })
        })
        .collect();
    if trimmed {
        let hidden = total - SESSION_RENDER_MESSAGE_LIMIT;
        out.insert(0, ChatMsg { author: Author::Note, text: format!("… {hidden} earlier messages hidden (long session) — the agent still resumes the full context") });
    }
    out
}

fn replay_role_label(role: &str) -> &'static str {
    match role {
        "user" => "User",
        "assistant" => "Assistant",
        "summary" => "Summary",
        "event" => "Audit",
        "ui_spec" => "UI",
        "meta" => "Meta",
        "tool" => "Tool",
        "system" => "System",
        _ => "Row",
    }
}

fn parse_replay_row(role: String, content: String) -> ReplayRow {
    if role == "event" {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
            let kind = v["kind"].as_str().unwrap_or("event");
            let title = v["title"].as_str().unwrap_or(kind);
            let status = v["status"].as_str().unwrap_or("");
            let detail = v["detail"].as_str().unwrap_or("");
            let title = if status.is_empty() {
                format!("{} · {}", replay_role_label(&role), title)
            } else {
                format!("{} · {} · {}", replay_role_label(&role), status, title)
            };
            return ReplayRow {
                role,
                title,
                detail: detail.chars().take(600).collect(),
            };
        }
    }
    if role == "ui_spec" {
        if let Ok(spec) = parse_ui_spec_message(&content) {
            let title = spec
                .title
                .as_deref()
                .or(spec.root.props.title.as_deref())
                .unwrap_or("Untitled UI");
            return ReplayRow {
                role,
                title: format!("UI · {title}"),
                detail: "Rust-native structured artifact".to_string(),
            };
        }
    }
    let mut lines = content.lines();
    let first = lines.find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    ReplayRow {
        role: role.clone(),
        title: format!(
            "{} · {}",
            replay_role_label(&role),
            first.chars().take(80).collect::<String>()
        ),
        detail: content.chars().take(600).collect(),
    }
}

fn load_session_replay(path: &Path, title: String) -> SessionReplay {
    let rows = oxide_core::db::load(&sid(path));
    let total = rows.len();
    let start = total.saturating_sub(80);
    let replay_rows = rows
        .into_iter()
        .skip(start)
        .map(|(role, content)| parse_replay_row(role, content))
        .collect();
    SessionReplay {
        path: path.to_path_buf(),
        title,
        rows: replay_rows,
        total,
    }
}

/// Run a git subcommand in the workspace, returning stdout (stderr appended).
/// Detect localhost servers: listening TCP ports + the owning process name.
/// macOS/Linux via `lsof`. Returns `(port, "pid/command")` sorted, deduped.
/// Running listener processes with pids (for the Environment card dropdown's
/// kill buttons). Same filtering as scan_ports.
/// Claude subscription usage via the OAuth usage endpoint (token from the
/// Keychain item Claude Code writes). Returns (plan, 5h_remaining%, weekly_remaining%).
/// Best-effort; None when not logged in or the call fails. The token is never
/// logged or persisted.
async fn fetch_claude_usage() -> Option<(String, u8, u8)> {
    // Read the OAuth blob from the login Keychain.
    let kc = tokio::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "Claude Code-credentials",
            "-w",
        ])
        .output()
        .await
        .ok()?;
    if !kc.status.success() {
        return None;
    }
    let blob: serde_json::Value = serde_json::from_slice(&kc.stdout).ok()?;
    let oauth = &blob["claudeAiOauth"];
    let token = oauth["accessToken"].as_str()?;
    if token.is_empty() {
        return None;
    }
    let plan = oauth["subscriptionType"]
        .as_str()
        .unwrap_or("claude")
        .to_string();

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .ok()?;
    let resp = client
        .get("https://api.anthropic.com/api/oauth/usage")
        .header("Authorization", format!("Bearer {token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: serde_json::Value = resp.json().await.ok()?;
    let rem = |x: &serde_json::Value| -> u8 {
        let u = x["utilization"].as_f64().unwrap_or(0.0);
        (100.0 - u).clamp(0.0, 100.0).round() as u8
    };
    Some((plan, rem(&v["five_hour"]), rem(&v["seven_day"])))
}

async fn scan_procs() -> Vec<(u16, String, u32)> {
    let out = match tokio::process::Command::new("lsof")
        .args(["-nP", "-iTCP", "-sTCP:LISTEN"])
        .output()
        .await
    {
        Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
        Err(_) => return Vec::new(),
    };
    const DENY: &[&str] = &[
        "spotify",
        "rapportd",
        "controlce",
        "sharingd",
        "identityser",
        "rapport",
        "cloudd",
        "apsd",
        "trustd",
        "nsurlsess",
        "airplay",
        "wifiagent",
        "music",
        "podcasts",
        "supercond",
        "remoted",
        "launchd",
        "deleted",
        "syncdefa",
        "agent-",
    ];
    let mut found: std::collections::BTreeMap<u16, (String, u32)> =
        std::collections::BTreeMap::new();
    for line in out.lines().skip(1) {
        let mut cols = line.split_whitespace();
        let cmd = cols.next().unwrap_or("").to_string();
        let pid: u32 = cols.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        let lc = cmd.to_ascii_lowercase();
        if DENY.iter().any(|d| lc.starts_with(d)) {
            continue;
        }
        if let Some(addr) = line.split_whitespace().find(|c| {
            c.contains(':')
                && (c.contains("127.0.0.1")
                    || c.starts_with("*:")
                    || c.contains("[::1]")
                    || c.contains("localhost"))
        }) {
            if let Some(p) = addr.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()) {
                if pid > 0 {
                    found.entry(p).or_insert((cmd, pid));
                }
            }
        }
    }
    found
        .into_iter()
        .map(|(port, (name, pid))| (port, name, pid))
        .collect()
}

async fn scan_ports() -> Vec<(u16, String)> {
    let out = match tokio::process::Command::new("lsof")
        .args(["-nP", "-iTCP", "-sTCP:LISTEN"])
        .output()
        .await
    {
        Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
        Err(_) => return Vec::new(),
    };
    // macOS/media daemons that squat on localhost ports — never a dev server.
    const DENY: &[&str] = &[
        "spotify",
        "rapportd",
        "controlce",
        "sharingd",
        "identityser",
        "rapport",
        "cloudd",
        "apsd",
        "trustd",
        "nsurlsess",
        "airplay",
        "wifiagent",
        "music",
        "podcasts",
        "supercond",
        "remoted",
        "launchd",
        "deleted",
        "syncdefa",
        "agent-",
    ];
    // Runtimes that *are* dev servers — these we always surface.
    const DEV: &[&str] = &[
        "node", "vite", "next", "bun", "deno", "python", "ruby", "php", "cargo", "rustc",
        "webpack", "esbuild", "turbo", "npm", "pnpm", "yarn", "rails", "flask", "uvicorn",
        "gunicorn", "caddy", "dotnet", "java", "air", "gin", "hugo", "jekyll", "astro", "remix",
        "nuxt", "ng", "serve", "http-ser",
    ];
    let mut found: std::collections::BTreeMap<u16, String> = std::collections::BTreeMap::new();
    for line in out.lines().skip(1) {
        let mut cols = line.split_whitespace();
        let cmd = cols.next().unwrap_or("").to_string();
        let lc = cmd.to_ascii_lowercase();
        if DENY.iter().any(|d| lc.starts_with(d)) {
            continue;
        }
        // NAME column holds e.g. "127.0.0.1:5173" or "*:3000".
        if let Some(addr) = line.split_whitespace().find(|c| {
            c.contains(':')
                && (c.contains("127.0.0.1")
                    || c.starts_with("*:")
                    || c.contains("[::1]")
                    || c.contains("localhost"))
        }) {
            if let Some(p) = addr.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()) {
                if matches!(p, 22 | 53 | 88 | 445 | 631 | 5353 | 7000) {
                    continue;
                }
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

/// Repo-wide working diff: `(path, adds, dels, diff)` per changed file.
async fn load_changed_files(ws: &Path) -> Vec<(String, u32, u32, String)> {
    let num = run_cmd(ws, "git", &["diff", "--numstat"]).await;
    let mut out = Vec::new();
    for line in num.lines().take(40) {
        let mut it = line.split_whitespace();
        let (Some(a), Some(d), Some(path)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        let adds = a.parse().unwrap_or(0);
        let dels = d.parse().unwrap_or(0);
        let diff = run_cmd(ws, "git", &["diff", "--", path]).await;
        out.push((
            path.to_string(),
            adds,
            dels,
            diff.chars().take(20000).collect(),
        ));
    }
    out
}

/// Run an arbitrary command in the workspace, returning stdout+stderr.
async fn run_cmd(ws: &Path, cmd: &str, args: &[&str]) -> String {
    match tokio::process::Command::new(cmd)
        .args(args)
        .current_dir(ws)
        .output()
        .await
    {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).to_string();
            let err = String::from_utf8_lossy(&o.stderr);
            if !err.trim().is_empty() {
                s.push('\n');
                s.push_str(&err);
            }
            if s.trim().is_empty() {
                "(done)".to_string()
            } else {
                s
            }
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

fn open_file(mut ui: Ui, path: PathBuf) {
    // PDF/image: previewed via the asset handler, not slurped as text.
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if matches!(
        ext.as_str(),
        "pdf" | "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp"
    ) {
        ui.editor_text.set(String::new());
        ui.open_path.set(Some(path));
        ui.dirty.set(false);
        return;
    }
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

async fn hermes_diff_context(ws: &Path) -> String {
    let status = run_cmd(ws, "git", &["status", "--short", "--branch"]).await;
    let diff = run_cmd(ws, "git", &["diff"]).await;
    let rules = std::fs::read_to_string(ws.join("AGENTS.md"))
        .or_else(|_| std::fs::read_to_string(ws.join("agents.md")))
        .unwrap_or_default();
    let diff: String = diff.chars().take(16_000).collect();
    let rules: String = rules.chars().take(6_000).collect();
    format!(
        "## Git status\n```text\n{}\n```\n\n## Working git diff\n```diff\n{}\n```\n\n## Project rules\n{}",
        status.trim(),
        diff.trim(),
        if rules.trim().is_empty() {
            "(no AGENTS.md found)"
        } else {
            rules.trim()
        }
    )
}

fn submit_hermes_prompt(
    mut cfg: Signal<Config>,
    engine: Coroutine<EngineCmd>,
    streaming: Signal<bool>,
    mut status: Signal<String>,
    prompt: String,
    display: String,
) {
    if *streaming.read() {
        status.set("Finish or stop the current turn before starting Hermes".to_string());
        return;
    }
    let mut next = cfg.read().clone();
    next.harness = "hermes".to_string();
    cfg.set(next.clone());
    engine.send(EngineCmd::Reconfigure(next));
    engine.send(EngineCmd::Submit {
        engine: prompt,
        display,
    });
    status.set("Hermes evolve started".to_string());
}

fn app() -> Element {
    let initial = use_context::<Config>();
    let visual_fixture = VisualFixtureMode::from_env();

    // Live, editable configuration.
    let cfg = use_signal(|| initial.clone());
    let ws0 = workspace_of(&initial);

    // Chat state.
    let mut messages = use_signal(move || visual_fixture_messages(visual_fixture));
    let mut context_limit = use_signal(|| None::<u64>);
    let mut streaming =
        use_signal(move || matches!(visual_fixture, Some(VisualFixtureMode::Streaming)));

    // Panels.
    // Environment pane (right): one tabbed home for Files/Terminals/Preview/Diffs.
    let mut show_env = use_signal(|| false);
    let mut env_tab = use_signal(|| "files".to_string());
    let env_tab_by_tab = use_signal(HashMap::<u64, String>::new);
    // Environment card: running-process dropdown (port, name, pid).
    let mut procs_list = use_signal(Vec::<(u16, String, u32)>::new);
    // Environment card menus + per-thread extras.
    let mut git_menu = use_signal(|| false);
    let mut branch_menu = use_signal(|| false);
    let mut branches_list = use_signal(Vec::<String>::new);
    // Pinned messages + markers, per thread: (msg index, snippet, done) /
    // (msg index, snippet, color, done).
    let mut pinned_msgs = use_signal(Vec::<(usize, String, bool)>::new);
    let mut markers = use_signal(Vec::<(usize, String, u8, bool)>::new);
    let mut pins_open = use_signal(|| true);
    let mut marks_open = use_signal(|| true);
    let mut note_open = use_signal(|| false);
    let mut note_text = use_signal(String::new);
    let mut recap_open = use_signal(|| false);
    let mut recap_text = use_signal(String::new);
    // Multi-terminal model: (id, title, lines).
    let mut terms = use_signal(|| vec![(1u64, "zsh 1".to_string(), Vec::<String>::new())]);
    let mut term_sel = use_signal(|| 0usize);
    let mut term_seq = use_signal(|| 1u64);
    let mut show_settings = use_signal(|| false);
    let mut settings_initial_tab = use_signal(|| "model".to_string());
    let mut show_skills = use_signal(|| false);
    let mut show_mcp = use_signal(|| false);
    let mut show_theme_menu = use_signal(|| false);
    let mut theme_menu_pos = use_signal(|| (12.0f64, 44.0f64));
    // Cmd-K command palette.
    let mut show_palette = use_signal(|| false);
    let mut show_shortcuts = use_signal(|| false);
    // Cursor-style icon rail: sidebar collapses to a thin strip.
    let mut sidebar_collapsed = use_signal(|| false);
    // Resizable side panels: (which: 1=left sidebar, 2=right inspector, start_x, start_w).
    let mut panel_drag = use_signal(|| None::<(u8, f64, f64)>);
    // Width (px) of the Environment panel (drag id 3) — persisted.
    let mut rpanel_w = use_signal(|| cfg.peek().env_width);
    // Height (px) of the bottom terminal panel (drag id 4, vertical).
    let mut term_h = use_signal(|| 240.0f64);
    let mut sidebar_w = use_signal(|| cfg.peek().sidebar_width);
    let mut insp_w = use_signal(|| cfg.peek().inspector_width);
    let mut palette_query = use_signal(String::new);
    let mut palette_sel = use_signal(|| 0usize);
    let mut pinned = use_signal(|| false);
    let win = dioxus::desktop::use_window();
    let mut mcp_status = use_signal(std::collections::HashMap::<String, String>::new);
    // ChatGPT subscription usage: (plan, 5h %, weekly %, 5h reset s, weekly reset s).
    // (family "gpt"/"claude", plan, 5h-remaining %, weekly-remaining %, 5h reset, weekly reset).
    // Family-tagged so the card never shows one provider's quota while another is active.
    let mut usage_info = use_signal(|| None::<(String, String, u8, u8, String, String)>);
    // Tiling split-view (each pane its own live engine).
    let mut show_split = use_signal(|| false);
    // Right-hand Changes panel (Cursor-style): repo-wide diff + commit/PR.
    let mut changed_files = use_signal(Vec::<(String, u32, u32, String)>::new);
    let mut preview_url = use_signal(String::new);
    let mut preview_ports = use_signal(Vec::<(u16, String)>::new);
    let mut picked_element = use_signal(|| Option::<String>::None);
    // Design Mode (Cursor 3.0): selected element + live style edits.
    let mut design_mode = use_signal(|| false);
    let mut design_sel = use_signal(|| Option::<serde_json::Value>::None);
    let mut design_edits = use_signal(Vec::<(String, String, String)>::new);
    let mut design_note = use_signal(String::new);
    let split_panes = use_signal(|| {
        vec![(
            0u64,
            "gui".to_string(),
            cfg.read().provider.clone(),
            cfg.read().model.clone(),
        )]
    });
    let split_layout = use_signal(|| Tile::Leaf(0));
    let split_next_id = use_signal(|| 1u64);
    let split_drag = use_signal(|| None::<u64>);
    let split_rects = use_signal(std::collections::HashMap::<u64, (f64, f64, f64, f64)>::new);
    let mut show_board = use_signal(|| false);
    let mut board = use_signal(board::Board::default);
    let board_sync_status = use_signal(|| "Issue sync idle".to_string());
    let mut board_syncing = use_signal(|| false);
    let mut new_card_title = use_signal(String::new);
    let mut automations = use_signal(Vec::<automation::AutomationSpec>::new);
    let mut automation_runs = use_signal(Vec::<automation::AutomationRunSpec>::new);
    let automation_name = use_signal(|| automation::DEFAULT_NAME.to_string());
    let automation_schedule = use_signal(|| automation::DEFAULT_SCHEDULE.to_string());
    let automation_prompt = use_signal(|| automation::DEFAULT_PROMPT.to_string());
    let automation_status = use_signal(|| "Automations idle".to_string());
    let automation_confirm_delete = use_signal(|| None::<String>);
    let hermes_ws = ws0.clone();
    let hermes_profiles = use_signal(move || hermes::read_profiles(&hermes_ws).unwrap_or_default());
    let hermes_profile_name = use_signal(|| "Hermes lane".to_string());
    let hermes_goal =
        use_signal(|| "Improve Oxide with local-first Hermes-grade agent workflows".to_string());
    let hermes_validation =
        use_signal(|| "cargo check -p oxide-gui && cargo test -p oxide-core".to_string());
    let hermes_review_prompt = use_signal(|| {
        "DONE only if the change is complete, scoped, validated, and local-first; otherwise list GAPS with exact fixes.".to_string()
    });
    let hermes_status = use_signal(|| "Hermes idle".to_string());
    let hermes_confirm_delete = use_signal(|| None::<String>);
    let mut projects_list = use_signal(Vec::<ProjectGroup>::new);
    let mut session_menu = use_signal(|| None::<PathBuf>);
    // Per-project visible session count. Default is 5; Show more reveals
    // another page so long histories expand gradually.
    let mut project_session_pages = use_signal(HashMap::<String, usize>::new);
    // Projects whose chat list is collapsed (click the caret on the header).
    let mut collapsed_projects = use_signal(HashSet::<String>::new);
    // Bump to force the sidebar (pins/projects) to re-read the session db.
    let mut sessions_refresh = use_signal(|| 0u64);
    let mut confirm_archive_workspace = use_signal(|| None::<String>);
    let restored_sessions = use_signal(HashSet::<String>::new);
    // Tab currently animating closed.
    let mut closing_tab = use_signal(|| None::<u64>);
    // Suggested follow-up prompts shown above the composer after a turn.
    let mut followups = use_signal(Vec::<String>::new);
    let mut queue = use_signal(Vec::<String>::new);
    // Toast notifications (bottom-right stack, auto-dismiss).
    let toasts = use_signal(Vec::<ToastSpec>::new);
    let toast_seq = use_signal(|| 0u64);
    use_future(move || async move {
        procs_list.set(scan_procs().await);
        preview_ports.set(scan_ports().await);
    });
    use_future(move || async move {
        let mut last_root = PathBuf::new();
        let mut last_sync: Option<std::time::Instant> = None;
        loop {
            let root = cfg.peek().workspace.clone();
            let open = *show_board.peek();
            if open {
                if let Some(root) = root {
                    let switched = root != last_root;
                    let stale = last_sync
                        .map(|t| t.elapsed() >= std::time::Duration::from_secs(300))
                        .unwrap_or(true);
                    if (switched || stale) && !*board_syncing.peek() {
                        last_root = root.clone();
                        board_syncing.set(true);
                        sync_board_issues_once(board, root, board_sync_status).await;
                        board_syncing.set(false);
                        last_sync = Some(std::time::Instant::now());
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });
    // Agent tabs (multiple agent sessions in one workspace).
    let initial_provider = cfg.read().provider.clone();
    let initial_model = cfg.read().model.clone();
    let initial_harness = cfg.read().harness.clone();
    let initial_effort = cfg.read().reasoning_effort.clone();
    let mut tabs = use_signal(|| {
        vec![AgentTab {
            id: 0,
            title: provider_title(&initial_provider).to_string(),
            provider: initial_provider,
            model: initial_model,
            harness: initial_harness,
            reasoning_effort: initial_effort,
            messages: visual_fixture_messages(visual_fixture),
            mode: "gui".to_string(),
            bin: String::new(),
            session: None,
            resume: None,
        }]
    });
    let active_tab = use_signal(|| 0usize);
    let mut renaming_tab = use_signal(|| None::<u64>);
    let mut rename_text = use_signal(String::new);
    let next_tab_id = use_signal(|| 1u64);
    let mut show_newtab = use_signal(|| false);

    // Composer modes (shared across both Composer instances).
    let plan_mode = use_signal(|| false);
    let pursue_goal = use_signal(|| false);

    // Inspector (right panel) state — ported from the desktop command center.
    let mut inspector_tab = use_signal(|| "files".to_string());
    let mut timeline = use_signal(Vec::<TimelineItem>::new);
    let mut subagent_cards = use_signal(move || visual_fixture_subagents(visual_fixture));
    let mut session_replay = use_signal(|| None::<SessionReplay>);
    let mut approvals = use_signal(Vec::<(u64, String, String)>::new);
    let mut checkpoints = use_signal(Vec::<(u64, String)>::new);
    // (input, output, cached_input) tokens for the latest turn.
    let mut usage = use_signal(|| (0u64, 0u64, 0u64));
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
    let mut thinking = use_signal(move || visual_fixture_thinking(visual_fixture));
    // Background tasks the CLI agent started ("running in background") — their
    // result won't stream back, so we surface what they are as persistent chips.
    let mut bg_jobs = use_signal(Vec::<String>::new);
    // Tab ids whose engine is mid-turn. Engines are per-tab: leaving a tab no
    // longer kills its turn — this drives the sidebar spinners + view state.
    let mut busy_tabs = use_signal(HashSet::<u64>::new);
    let mut tab_statuses = use_signal(HashMap::<u64, TabStatus>::new);
    let mut questions = use_signal(Vec::<(u64, String, Vec<String>)>::new);
    let mut q_answer = use_signal(String::new);
    let mut reverted = use_signal(HashSet::<u64>::new);
    // Checkpoints the user explicitly Kept (Cursor-style Accept) — clears the
    // review affordance on that row so reviewed edits read as resolved.
    let mut accepted = use_signal(HashSet::<u64>::new);
    // Edits made this turn: (path, adds, dels, checkpoint).
    let mut turn_edits = use_signal(move || visual_fixture_turn_edits(visual_fixture));
    let mut todos = use_signal(move || visual_fixture_todos(visual_fixture));
    let mut edits_expanded = use_signal(|| false);
    let mut edits_undone = use_signal(|| false);
    // Two-click confirm for the destructive restore-checkpoint hover button.
    let mut confirm_restore = use_signal(|| None::<usize>);
    // Full-screen preview for an image attached to a sent message.
    let mut chat_img = use_signal(|| None::<String>);
    // Long user prompts render clamped (the sticky header would otherwise eat
    // half the viewport); indices here are user-expanded.
    let mut expanded_user = use_signal(HashSet::<usize>::new);
    // User override for the thinking-box open state (None = follow streaming).
    let mut think_open = use_signal(|| None::<bool>);
    // Per activity-group open state (keyed by first row index). Defaults to the
    // running state but, once the user toggles, their choice sticks across the
    // streaming re-renders that would otherwise force it back open.
    let mut act_open = use_signal(std::collections::HashMap::<usize, bool>::new);
    // Tool/command activity rows are paired to their streamed updates by a stable
    // key stored ON the row (see `activity_idx`), not by a side index map — so a
    // row inserted above the "Done" note can't shift another row's pairing.
    let mut status = use_signal(move || visual_fixture_status(visual_fixture));
    let mut turn_start = use_signal(move || {
        if matches!(visual_fixture, Some(VisualFixtureMode::Streaming)) {
            Some(std::time::Instant::now())
        } else {
            None
        }
    });
    // Seconds since the turn started (ticks while streaming, shown in the pill).
    let mut elapsed_s = use_signal(|| 0u64);
    use_future(move || async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            if let Some(t) = *turn_start.read() {
                elapsed_s.set(t.elapsed().as_secs());
            }
        }
    });

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
        let repo = {
            let r = cfg.read().github_repo.clone();
            if r.trim().is_empty() {
                "MANFIT7/oxide".to_string()
            } else {
                r
            }
        };
        let url = cfg.read().update_url.clone();
        spawn(async move {
            if let Some(info) = update::check(&repo, &url).await {
                update_info.set(Some(info));
            }
        });
    });

    // Warm the syntect syntax set off-thread so the first code block in a reply
    // doesn't stall the UI mid-stream.
    use_hook(|| {
        std::thread::spawn(|| {
            let _ = highlight_code("", "txt");
        });
    });

    // Global keyboard shortcuts (Cmd-K command palette, Esc to close).
    use_future(move || async move {
        let mut eval = dioxus::document::eval(
            r#"
            if (!window.__oxkeys) {
              window.__oxkeys = 1;
              document.addEventListener('keydown', function(e){
                // Don't hijack shortcuts while a terminal/TUI has focus.
                if (e.target && e.target.closest && e.target.closest('.xterm, .terminal, .term-host, .wterm, .wterm-host')) return;
                if ((e.metaKey || e.ctrlKey) && (e.key === 'k' || e.key === 'K')) { e.preventDefault(); dioxus.send('palette'); }
                else if ((e.metaKey || e.ctrlKey) && e.key === '/') { e.preventDefault(); dioxus.send('shortcuts'); }
                else if ((e.metaKey || e.ctrlKey) && (e.key === 'b' || e.key === 'B')) { e.preventDefault(); dioxus.send('files'); }
                // Cmd-L toggles composer focus. Bind Cmd only (not Ctrl) so
                // non-macOS Ctrl+L stays free for clearing the terminal.
                else if (e.metaKey && (e.key === 'l' || e.key === 'L')) {
                  e.preventDefault();
                  const el = document.getElementById('ce-input');
                  if (el) {
                    if (document.activeElement === el) { el.blur(); }
                    else {
                      el.focus();
                      const r = document.createRange(); r.selectNodeContents(el); r.collapse(false);
                      const sel = window.getSelection(); sel.removeAllRanges(); sel.addRange(r);
                    }
                  }
                }
                else if (e.key === 'Escape') { dioxus.send('esc'); }
              }, true);
            }
            while (true) { await new Promise(r => setTimeout(r, 3600000)); }
            "#,
        );
        loop {
            match eval.recv::<String>().await {
                Ok(k) if k == "palette" => {
                    let v = !*show_palette.read();
                    show_palette.set(v);
                    palette_query.set(String::new());
                    palette_sel.set(0);
                }
                Ok(k) if k == "files" => select_env_tab(
                    env_tab,
                    show_env,
                    env_tab_by_tab,
                    tabs,
                    active_tab,
                    "files",
                    true,
                ),
                Ok(k) if k == "shortcuts" => {
                    let v = !*show_shortcuts.read();
                    show_shortcuts.set(v);
                }
                Ok(k) if k == "esc" => {
                    show_palette.set(false);
                    show_shortcuts.set(false);
                    chat_img.set(None);
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });

    // Disable the WebView's native right-click menu (Reload / Inspect Element).
    use_effect(move || {
        spawn(async move {
            let _ = dioxus::document::eval(
                "if(!window.__oxnoctx){window.__oxnoctx=1;document.addEventListener('contextmenu',function(e){e.preventDefault();},{capture:true});}\
                 if(!window.__oxzoom){window.__oxzoom=1;var z=parseFloat(localStorage.getItem('oxzoom')||'1')||1;document.documentElement.style.zoom=z;\
                 document.addEventListener('keydown',function(e){if(!(e.metaKey||e.ctrlKey))return;var k=e.key;\
                 if(k==='='||k==='+'){z=Math.min(2.5,z+0.1);}else if(k==='-'){z=Math.max(0.5,z-0.1);}else if(k==='0'){z=1;}else return;\
                 e.preventDefault();document.documentElement.style.zoom=z;localStorage.setItem('oxzoom',String(z));},true);}",
            );
        });
    });

    // Auto-scroll the chat to the bottom as content streams in — but only when
    // the user is already near the bottom, so reading scrollback isn't yanked.
    // Serve the bundled mermaid lib from the app's OWN origin (custom-scheme
    // origins block remote/module script loads — same-origin is allowed).
    // macOS liquid-glass: apply window vibrancy once. Only the "ara" theme
    // reveals it (transparent app bg + glass sidebar); solid themes cover it.
    use_hook(|| {
        #[cfg(target_os = "macos")]
        {
            use window_vibrancy::{apply_vibrancy, NSVisualEffectMaterial, NSVisualEffectState};
            let win = dioxus::desktop::window();
            let _ = apply_vibrancy(
                &win.window,
                NSVisualEffectMaterial::Sidebar,
                Some(NSVisualEffectState::Active),
                None,
            );
        }
    });

    // Serve workspace-local images so the agent's ![](path) screenshots render.
    {
        let ws_sig = ui.workspace;
        dioxus::desktop::use_asset_handler("wsimg", move |req, responder| {
            let rel = req.uri().path().trim_start_matches("/wsimg/").to_string();
            let rel = percent_decode(&rel);
            let ws = ws_sig.peek().clone();
            let path = if rel.starts_with('/') {
                std::path::PathBuf::from(&rel)
            } else {
                ws.join(&rel)
            };
            // Confine to the workspace; refuse traversal outside it.
            let ok = path
                .canonicalize()
                .ok()
                .map(|c| c.starts_with(&ws) || rel.starts_with('/'))
                .unwrap_or(false);
            let body = if ok {
                std::fs::read(&path).unwrap_or_default()
            } else {
                Vec::new()
            };
            let ct = match path.extension().and_then(|e| e.to_str()) {
                Some("png") => "image/png",
                Some("jpg") | Some("jpeg") => "image/jpeg",
                Some("gif") => "image/gif",
                Some("svg") => "image/svg+xml",
                Some("webp") => "image/webp",
                Some("pdf") => "application/pdf",
                _ => "application/octet-stream",
            };
            let resp = dioxus::desktop::wry::http::Response::builder()
                .header("Content-Type", ct)
                .body(std::borrow::Cow::from(body))
                .unwrap();
            responder.respond(resp);
        });
    }
    dioxus::desktop::use_asset_handler("nerdfont", move |_req, responder| {
        let resp = dioxus::desktop::wry::http::Response::builder()
            .header("Content-Type", "font/ttf")
            .body(std::borrow::Cow::from(NERD_FONT.to_vec()))
            .unwrap();
        responder.respond(resp);
    });
    dioxus::desktop::use_asset_handler("notify-sound", move |_req, responder| {
        let resp = dioxus::desktop::wry::http::Response::builder()
            .header("Content-Type", "audio/wav")
            .header("Cache-Control", "public, max-age=31536000")
            .body(std::borrow::Cow::from(DONE_SOUND.to_vec()))
            .unwrap();
        responder.respond(resp);
    });
    dioxus::desktop::use_asset_handler("mermaidjs", move |_req, responder| {
        let body = MERMAID_JS.to_vec();
        let resp = dioxus::desktop::wry::http::Response::builder()
            .header("Content-Type", "text/javascript")
            .header("Access-Control-Allow-Origin", "*")
            .body(std::borrow::Cow::from(body))
            .unwrap();
        responder.respond(resp);
    });

    use_future(move || async move {
        let js = r#"
            if (!window.__oxideSoundBoot) {
              window.__oxideSoundBoot = true;
              const ensure = () => {
                const audio = window.__oxideDoneAudio || new Audio('/notify-sound/done.wav');
                window.__oxideDoneAudio = audio;
                audio.preload = 'auto';
                audio.volume = 0.48;
                audio.load();
                return audio;
              };
              ensure();
              const unlock = () => {
                try {
                  const audio = ensure();
                  audio.muted = true;
                  const p = audio.play();
                  if (p && p.then) {
                    p.then(() => {
                      audio.pause();
                      audio.currentTime = 0;
                      audio.muted = false;
                    }).catch(() => { audio.muted = false; });
                  } else {
                    audio.muted = false;
                  }
                } catch (_) {}
              };
              document.addEventListener('pointerdown', unlock, { once: true, capture: true });
              document.addEventListener('keydown', unlock, { once: true, capture: true });
            }
            return true;
        "#;
        let _ = dioxus::document::eval(js).join::<bool>().await;
    });

    // Load mermaid (v11, bundled) from the same-origin asset handler.
    use_future(move || async move {
        let theme = if cfg.peek().theme != "light" {
            "dark"
        } else {
            "default"
        };
        let js = format!(
            r#"
            (function(){{
              if (window.__oxmermaid) return;
              window.__oxmermaid = 1;
              const boot = () => {{
                const M = window.mermaid; if (!M) return;
                try {{ M.initialize({{startOnLoad:false,theme:'{theme}',securityLevel:'loose',fontFamily:'inherit'}}); }} catch(e){{}}
                const run = () => {{
                  document.querySelectorAll('.mermaid:not([data-ox-done])').forEach((el)=>{{
                    const src=(el.textContent||'').trim(); if(!src) return;
                    el.setAttribute('data-ox-done','1');
                    const id='oxmmd-'+(window.__oxmc=(window.__oxmc||0)+1);
                    M.render(id,src).then(r=>{{el.innerHTML=r.svg;}}).catch(()=>{{el.removeAttribute('data-ox-done');el.classList.add('mermaid-err');}});
                  }});
                }};
                run();
                new MutationObserver(run).observe(document.body,{{childList:true,subtree:true}});
              }};
              const s=document.createElement('script');
              s.src='/mermaidjs/mermaid.min.js';
              s.onload=boot;
              s.onerror=()=>{{ window.__oxmermaid=0; }};
              document.head.appendChild(s);
            }})();
            while (true) {{ await new Promise(r => setTimeout(r, 3600000)); }}
            "#
        );
        let _ = dioxus::document::eval(&js).recv::<String>().await;
    });

    // Autoscroll via ONE persistent MutationObserver (installed once) instead of
    // a JS eval round-trip per streaming delta.
    use_future(move || async move {
        let _ = dioxus::document::eval(
            r#"
            if (!window.__oxscroll) {
              window.__oxscroll = 1;
              let cur = null, inner = null;
              const rebind = () => {
                const s = document.querySelector('.scroll');
                if (s === cur) return;
                cur = s;
                if (inner) { inner.disconnect(); inner = null; }
                if (!s) return;
                let ignoreScroll = false;
                const bottomDistance = () => Math.max(0, s.scrollHeight - s.scrollTop - s.clientHeight);
                const hasSelection = () => {
                  const sel = window.getSelection && window.getSelection();
                  return !!sel && String(sel).length > 0;
                };
                const typingTarget = () => {
                  const el = document.activeElement;
                  if (!el || !s.contains(el)) return false;
                  const tag = String(el.tagName || '').toLowerCase();
                  return tag === 'input' || tag === 'textarea' || el.isContentEditable;
                };
                const upd = () => {
                  const d = bottomDistance();
                  window.__oxstick = d < 160;
                  const b = s.querySelector('.jump-bottom');
                  if (b) b.classList.toggle('show', d > 300);
                };
                // Coalesce the snap into ONE rAF per frame. A streaming turn fires a
                // characterData mutation per token; snapping + reading layout on each
                // forces a reflow per token = visible tail jitter. Batching to one
                // snap-per-frame keeps the tail glued smoothly (Synara's "snap in
                // layout timing" lesson, adapted to the MutationObserver model).
                let stickQueued = false;
                const stick = () => {
                  if (stickQueued) return;
                  stickQueued = true;
                  requestAnimationFrame(() => {
                    stickQueued = false;
                    if (window.__oxstick !== false && !hasSelection() && !typingTarget()) {
                      ignoreScroll = true;
                      s.scrollTop = s.scrollHeight;
                      requestAnimationFrame(() => { ignoreScroll = false; });
                    }
                    // Keep the LIVE reasoning panel pinned to its latest line as it
                    // streams (Cursor-style), but only when already near the bottom
                    // so a manual scroll-up to re-read isn't yanked back down.
                    const tb = s.querySelector('.thinking-box[open] .thinking-body');
                    if (tb && window.__oxstick !== false && !hasSelection()) {
                      const dd = tb.scrollHeight - tb.scrollTop - tb.clientHeight;
                      if (dd < 60) tb.scrollTop = tb.scrollHeight;
                    }
                    upd();
                  });
                };
                s.addEventListener('scroll', () => {
                  if (ignoreScroll) return;
                  upd();
                }, { passive: true });
                // Wheel/touch/key scrolls are the user's intent to inspect history.
                // Do not let incoming streaming tokens pull them back down until
                // they return near the bottom or press the jump button.
                const userScrollIntent = () => {
                  if (bottomDistance() > 80) window.__oxstick = false;
                };
                s.addEventListener('wheel', userScrollIntent, { passive: true });
                s.addEventListener('touchmove', userScrollIntent, { passive: true });
                s.addEventListener('keydown', (ev) => {
                  if (['PageUp', 'ArrowUp', 'Home', ' '].includes(ev.key)) userScrollIntent();
                }, { passive: true });
                inner = new MutationObserver(stick);
                inner.observe(s, { childList: true, subtree: true, characterData: true });
                // Fresh transcript mount (app start, welcome to chat): start at the bottom.
                s.scrollTop = s.scrollHeight;
                upd();
              };
              // Watch the whole document subtree so .scroll being remounted
              // (empty<->transcript, editor toggle, tab switch) re-binds the observer.
              new MutationObserver(rebind).observe(document.body, { childList: true, subtree: true });
              rebind();
            }
            while (true) { await new Promise(r => setTimeout(r, 3600000)); }
            "#,
        ).recv::<String>().await;
    });

    // Per-thread notepad + recap: reload when the active tab's session changes.
    use_effect(move || {
        let cur = *active_tab.read();
        let sess = tabs.read().get(cur).and_then(|t| t.session.clone());
        let ws = ui.workspace.peek().clone();
        let stem = sess
            .as_ref()
            .and_then(|p| p.file_stem().map(|x| x.to_string_lossy().to_string()))
            .unwrap_or_else(|| "default".into());
        let note =
            std::fs::read_to_string(ws.join(format!(".oxide/notes/{stem}.md"))).unwrap_or_default();
        note_text.set(note);
        pinned_msgs.set(thread_json_load(&ws, "pins", &stem));
        markers.set(thread_json_load(&ws, "markers", &stem));
        // Recap = last compaction summary recorded in the session file.
        let recap = sess
            .map(|p| {
                oxide_core::db::load(&sid(&p))
                    .into_iter()
                    .rfind(|(role, _)| role == "summary")
                    .map(|(_, content)| content)
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        recap_text.set(recap);
    });

    // Poll Claude subscription usage (CLI/API providers don't stream it).
    // Fetches immediately when the provider becomes claude/anthropic (no 120s
    // wait after a switch), then refreshes every 120s while it stays claude.
    use_future(move || async move {
        let mut last: Option<std::time::Instant> = None;
        let mut last_prov = String::new();
        loop {
            let prov = cfg.peek().provider.clone();
            let is_claude = matches!(prov.as_str(), "claude" | "claude_interactive" | "anthropic");
            let switched = prov != last_prov;
            last_prov = prov;
            if is_claude
                && (switched
                    || last.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(120)))
            {
                if let Some((plan, r5, rw)) = fetch_claude_usage().await {
                    usage_info.set(Some((
                        "claude".into(),
                        plan,
                        r5,
                        rw,
                        String::new(),
                        String::new(),
                    )));
                }
                last = Some(std::time::Instant::now());
            }
            tokio::time::sleep(std::time::Duration::from_secs(8)).await;
        }
    });

    // Keep the Environment card's change counts fresh per workspace.
    use_effect(move || {
        let ws = ui.workspace.read().clone();
        if cfg.read().workspace.is_some() {
            spawn(async move {
                changed_files.set(load_changed_files(&ws).await);
            });
        }
    });

    // Load the kanban board + recent chat sessions for the active workspace.
    use_effect(move || {
        let ws = ui.workspace.read().clone();
        if cfg.read().workspace.is_none() {
            // Welcome state: still show every known project folder + its chats
            // so picking up where you left off is one click.
            let recents = cfg.read().recent_workspaces.clone();
            if !recents.is_empty() {
                projects_list.set(build_projects(std::path::Path::new(""), &recents));
            }
            automations.set(Vec::new());
            automation_runs.set(Vec::new());
        }
        let _ = sessions_refresh.read();
        if cfg.read().workspace.is_some() {
            // Off the UI thread — sqlite queries + fs checks per workspace, plus
            // the board JSON read. Doing these inline added a hitch to each switch.
            {
                let ws2 = ws.clone();
                let recents = cfg.read().recent_workspaces.clone();
                let mut pl = projects_list;
                let mut board = board;
                spawn(async move {
                    let ws3 = ws2.clone();
                    let (groups, bd) = tokio::task::spawn_blocking(move || {
                        (build_projects(&ws2, &recents), board::Board::load(&ws3))
                    })
                    .await
                    .unwrap_or_default();
                    board.set(bd);
                    pl.set(groups);
                });
            }
            {
                let ws2 = ws.clone();
                let mut autos = automations;
                let mut runs = automation_runs;
                let mut auto_status = automation_status;
                spawn(async move {
                    let loaded = tokio::task::spawn_blocking(move || {
                        let specs = automation::read_specs(&ws2)?;
                        let runs = automation::read_runs(&ws2)?;
                        Ok::<_, anyhow::Error>((specs, runs))
                    })
                    .await;
                    match loaded {
                        Ok(Ok((specs, run_specs))) => {
                            autos.set(specs);
                            runs.set(run_specs);
                            auto_status.set("Automations idle".to_string());
                        }
                        Ok(Err(err)) => auto_status.set(format!("Automation load failed: {err}")),
                        Err(err) => auto_status.set(format!("Automation load failed: {err}")),
                    }
                });
            }
            // Clean up orphaned pane worktrees from a previous run — ONCE per
            // workspace per session, not on every tab/folder switch (the git
            // subprocesses it spawns were adding a hitch to each switch).
            let ws2 = ws.clone();
            let do_prune = {
                use std::sync::{Mutex, OnceLock};
                static DONE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
                DONE.get_or_init(Default::default)
                    .lock()
                    .unwrap()
                    .insert(ws2.display().to_string())
            };
            if do_prune {
                spawn(async move {
                    if let Ok(rd) = std::fs::read_dir(ws2.join(".oxide/worktrees")) {
                        for e in rd.flatten() {
                            let p = e.path();
                            if p.file_name()
                                .and_then(|n| n.to_str())
                                .map(|n| n.starts_with("pane-"))
                                .unwrap_or(false)
                            {
                                let _ = tokio::process::Command::new("git")
                                    .arg("-C")
                                    .arg(&ws2)
                                    .args(["worktree", "remove", "--force"])
                                    .arg(&p)
                                    .output()
                                    .await;
                                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                                    let _ = tokio::process::Command::new("git")
                                        .arg("-C")
                                        .arg(&ws2)
                                        .args(["branch", "-D", &format!("oxide/{name}")])
                                        .output()
                                        .await;
                                }
                            }
                        }
                    }
                    let _ = tokio::process::Command::new("git")
                        .arg("-C")
                        .arg(&ws2)
                        .args(["worktree", "prune"])
                        .output()
                        .await;
                });
            }
        }
    });

    // Catch-all: whenever streaming stops (turn done, error, interrupt, or a
    // dead engine), settle any activity row still showing a spinner. This is
    // the single place that covers every termination path, so no "bash"/"edit"
    // spinner lingers after the turn ends.
    use_effect(move || {
        if !*streaming.read() {
            let has_running = messages
                .peek()
                .iter()
                .any(|c| matches!(c.author, Author::Activity { running: true, .. }));
            if has_running {
                for c in messages.write().iter_mut() {
                    if let Author::Activity { running, .. } = &mut c.author {
                        *running = false;
                    }
                }
            }
            // Drop any "editing…" pending row (empty diff, no checkpoint) that
            // never resolved — its spinner must not outlive the turn.
            if turn_edits.peek().iter().any(|e| e.4.is_empty() && e.3 == 0) {
                turn_edits.write().retain(|e| !(e.4.is_empty() && e.3 == 0));
            }
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
            // Peek first — only take a write lock when the title really changes,
            // so this doesn't dirty `tabs` on every streaming delta.
            let needs = tabs
                .peek()
                .get(cur)
                .map(|t| t.title == provider_title(&t.provider))
                .unwrap_or(false);
            if needs {
                let new_title = make_title(&text);
                let mut sess = None;
                if let Some(t) = tabs.write().get_mut(cur) {
                    t.title = new_title.clone();
                    sess = t.session.clone();
                }
                if let Some(s) = sess {
                    oxide_core::db::set_title(&sid(&s), &new_title);
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
    let mut term_input = use_signal(String::new);

    // ── Engine coroutine (reconfigurable) ─────────────────────────────
    let engine = use_coroutine(move |mut rx: UnboundedReceiver<EngineCmd>| {
        let initial = initial.clone();
        async move {
            // One engine PER TAB. Events are tagged (tab id, generation) so a
            // single loop serves all engines without cross-tab bleed; a stale
            // generation (engine replaced) is simply dropped.
            // UNBOUNDED on purpose: a bounded channel here back-propagates into
            // core. If the Dioxus render thread falls behind (long answer =
            // superlinear whole-message markdown re-render), a bounded forwarder
            // would block on send, stop draining core's event_rx, fill core's
            // EVENT_QUEUE, and park `emit().await` mid-turn — which also strands
            // the op_rx arm so Stop goes dead. Unbounded keeps the forwarder
            // always-draining so the engine never stalls and Interrupt stays live;
            // queue memory is bounded by the answer length (tiny per-delta).
            let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel::<(u64, u64, Event)>();
            let mut handles: std::collections::HashMap<u64, EngineHandle> =
                std::collections::HashMap::new();
            let mut fwds: std::collections::HashMap<u64, tokio::task::JoinHandle<()>> =
                std::collections::HashMap::new();
            let mut gens: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
            let mut gen_seq: u64 = 0;
            // Approvals/questions that arrived while their tab was backgrounded —
            // replayed into the view when the user returns to that tab.
            // Background-tab transcripts accumulate HERE (a plain map), not in the
            // `tabs` signal — so a backgrounded turn's token stream doesn't dirty
            // the UI signal (and re-schedule the sidebar) on every delta. Merged
            // back into the tab when the user switches to it.
            let mut bg_buffers: std::collections::HashMap<u64, Vec<ChatMsg>> =
                std::collections::HashMap::new();
            let mut parked_appr: std::collections::HashMap<u64, Vec<(u64, String, String)>> =
                std::collections::HashMap::new();
            let mut parked_q: std::collections::HashMap<u64, Vec<(u64, String, Vec<String>)>> =
                std::collections::HashMap::new();

            // (Re)spawn the engine for one tab. Replaces any previous engine of
            // that tab (its turn is interrupted — same-tab reconfigure semantics).
            macro_rules! spawn_tab_engine {
                ($tid:expr, $conf:expr) => {{
                    let tid: u64 = $tid;
                    if let Some(h) = handles.remove(&tid) {
                        let _ = h.submit(Op::Interrupt).await;
                    }
                    if let Some(f) = fwds.remove(&tid) {
                        f.abort();
                    }
                    gen_seq += 1;
                    let g = gen_seq;
                    gens.insert(tid, g);
                    match oxide_core::spawn($conf) {
                        Ok((h, mut events)) => {
                            handles.insert(tid, h);
                            let tx = ev_tx.clone();
                            fwds.insert(
                                tid,
                                tokio::spawn(async move {
                                    while let Some(e) = events.recv().await {
                                        // Sync send (unbounded): never blocks, so this
                                        // task always drains core's event_rx immediately.
                                        if tx.send((tid, g, e)).is_err() {
                                            break;
                                        }
                                    }
                                }),
                            );
                        }
                        Err(e) => {
                            let _ = ev_tx.send((
                                tid,
                                g,
                                Event::Error {
                                    message: format!("engine: {e}"),
                                },
                            ));
                        }
                    }
                }};
            }

            // The id of the tab currently shown — events from other tabs go to
            // their buffered transcripts instead of the live view.
            macro_rules! active_id {
                () => {
                    tabs.peek().get(*active_tab.peek()).map(|t| t.id)
                };
            }

            let first_id = tabs.peek().first().map(|t| t.id).unwrap_or(0);
            spawn_tab_engine!(first_id, initial);
            // The tab the VIEW is bound to. Updated when the SwitchTab command is
            // PROCESSED (not when the click lands) so events racing in between are
            // routed to the right transcript, never the outgoing one.
            let mut view_tab: u64 = first_id;
            let mut cur_ws = workspace_of(&{ cfg.peek().clone() });
            // Streaming text coalescing: appending the live agent bubble re-runs
            // markdown+syntax-highlight on the WHOLE message per token, which janks
            // on fast streams. Buffer deltas and paint at ~30fps instead (modern
            // streaming-UI practice).
            let mut agent_buf = String::new();
            let mut last_paint = std::time::Instant::now();
            // True between a turn's Done note and the next TurnStarted. A late
            // activity arriving in this window is inserted ABOVE the Done note so
            // it never dangles below the summary.
            let mut turn_done = false;
            // Insert an activity row, keeping it above a trailing Done note.
            macro_rules! push_activity {
                ($msg:expr) => {{
                    let mut m = messages.write();
                    if turn_done
                        && m.last()
                            .map(|x| {
                                matches!(x.author, Author::Note)
                                    && x.text.starts_with(DONE_NOTE_MARK)
                            })
                            .unwrap_or(false)
                    {
                        let at = m.len() - 1;
                        m.insert(at, $msg);
                        at
                    } else {
                        let at = m.len();
                        m.push($msg);
                        at
                    }
                }};
            }
            macro_rules! flush_agent {
                () => {{
                    if !agent_buf.is_empty() {
                        let chunk = std::mem::take(&mut agent_buf);
                        let mut m = messages.write();
                        match m.last_mut() {
                            Some(last) if last.author == Author::Agent => {
                                last.text.push_str(&chunk)
                            }
                            _ => m.push(ChatMsg {
                                author: Author::Agent,
                                text: chunk,
                            }),
                        }
                        last_paint = std::time::Instant::now();
                    }
                }};
            }

            loop {
                tokio::select! {
                    cmd = rx.next() => {
                      // Land buffered streaming text before any view change.
                      flush_agent!();
                      match cmd {
                        Some(EngineCmd::Submit { engine: eng, display }) => {
                            followups.write().clear();
                            let aid = active_id!().unwrap_or(0);
                            if !handles.contains_key(&aid) {
                                // Lazy spawn: a fresh tab / reopened session gets its
                                // engine on first send (resuming its own transcript).
                                let mut conf = cfg.peek().clone();
                                conf.resume_path = tabs.peek().iter().find(|t| t.id == aid).and_then(|t| t.session.clone());
                                spawn_tab_engine!(aid, conf);
                            }
                            if let Some(h) = handles.get(&aid) {
                                messages.write().push(ChatMsg { author: Author::User, text: display });
                                messages.write().push(ChatMsg { author: Author::Agent, text: String::new() });
                                scroll_chat_bottom();
                                streaming.set(true);
                                // Reset the elapsed clock at send, not just at TurnStarted —
                                // otherwise the status pill flashes the PREVIOUS turn's seconds
                                // in the gap before the engine's TurnStarted arrives.
                                turn_start.set(Some(std::time::Instant::now()));
                                elapsed_s.set(0);
                                busy_tabs.write().insert(aid);
                                tab_statuses.write().insert(aid, TabStatus::Running);
                                let _ = h.submit(Op::UserTurn { text: eng }).await;
                            } else {
                                // Engine failed to start — don't eat the message silently.
                                messages.write().push(ChatMsg { author: Author::User, text: display });
                                messages.write().push(ChatMsg { author: Author::Note, text: format!("{} engine not running — check provider/settings, or switch model to restart it", '\u{26a0}') });
                                scroll_chat_bottom();
                            }
                        }
                        Some(EngineCmd::Reconfigure(conf)) => {
                            // Effort must fit the (possibly new) provider's range.
                            let mut conf = conf;
                            conf.reasoning_effort = clamp_effort(&conf.provider, &conf.reasoning_effort);
                            // Provider the active tab had BEFORE this reconfigure — used to
                            // drop stale usage when the quota source changes (e.g. ChatGPT to Claude).
                            let prev_provider = tabs
                                .peek()
                                .get(*active_tab.peek())
                                .map(|t| t.provider.clone())
                                .unwrap_or_default();
                            // Persist the new config (provider/model/effort/fast/…) so it survives restart.
                            // resume_path is a RUNTIME session pointer, not a setting — persisting
                            // it makes a later launch (possibly in another folder) silently
                            // continue an old session instead of starting clean.
                            let ws = workspace_of(&conf);
                            let mut persist = conf.clone();
                            persist.resume_path = None;
                            persist.resume = false;
                            if let Ok(s) = toml::to_string(&persist) {
                                let _ = std::fs::write(ws.join("oxide.toml"), &s);
                                // Also persist globally so the packaged app remembers across launches.
                                if let Some(home) = std::env::var_os("HOME") {
                                    let gdir = std::path::PathBuf::from(home).join(".config/oxide");
                                    let _ = std::fs::create_dir_all(&gdir);
                                    let _ = std::fs::write(gdir.join("config.toml"), &s);
                                }
                            }
                            // Only wipe the transcript when switching PROJECT — a
                            // model/effort/fast/access change must not erase the chat.
                            let same_ws = ws == cur_ws;
                            cur_ws = ws.clone();
                            let kept = if same_ws { messages.peek().clone() } else { Vec::new() };
                            // Same workspace = same conversation: continue THIS tab's
                            // own session file (bound via Event::SessionPath), so a
                            // model/effort change doesn't mint a new file or attach to
                            // another tab's transcript.
                            if same_ws {
                                let cur = *active_tab.peek();
                                conf.resume_path = tabs.peek().get(cur).and_then(|t| t.session.clone());
                            } else {
                                // New project = clean slate. A stale resume id from the
                                // previous folder must never leak into this engine, or the
                                // new chat continues (and appends to) another folder's session.
                                conf.resume_path = None;
                                conf.resume = false;
                                if let Some(t) = tabs.write().get_mut(*active_tab.peek()) {
                                    t.session = None;
                                }
                            }
                            // Keep the active tab's provider/logo/title in sync with
                            // the picker — switching ChatGPT to Claude must restyle the tab.
                            {
                                let cur = *active_tab.peek();
                                let mut tw = tabs.write();
                                if let Some(t) = tw.get_mut(cur) {
                                    t.harness = conf.harness.clone();
                                    t.reasoning_effort = conf.reasoning_effort.clone();
                                    if t.mode == "gui" && t.provider != conf.provider {
                                        let was_default = t.title == provider_title(&t.provider);
                                        t.provider = conf.provider.clone();
                                        t.model = conf.model.clone();
                                        if was_default {
                                            t.title = provider_title(&conf.provider).to_string();
                                        }
                                    } else if t.mode == "gui" {
                                        t.model = conf.model.clone();
                                    }
                                }
                            }
                            // Usage card belongs to one provider's quota. When the source
                            // family changes, drop the old value so the card never shows the
                            // previous provider's numbers (the claude poll / chatgpt RateLimit
                            // event repopulates it for the new provider).
                            if provider_family(&prev_provider) != provider_family(&conf.provider) {
                                usage_info.set(None);
                            }
                            approvals.write().clear();
                            checkpoints.write().clear();
                            timeline.write().clear();
                            streaming.set(false);
                            let aid = active_id!().unwrap_or(0);
                            busy_tabs.write().remove(&aid);
                            tab_statuses.write().remove(&aid);
                            // Stale events from the replaced engine are dropped by the
                            // generation guard — no channel drain needed.
                            spawn_tab_engine!(aid, conf);
                            messages.set(kept);
                        }
                        Some(EngineCmd::SwitchTab { id, conf, msgs }) => {
                            // VIEW switch only — the tab being left keeps its engine
                            // (and any in-flight turn) running in the background.
                            // Save the outgoing view first — deltas may have landed after
                            // the caller's click-time snapshot. If it's still streaming,
                            // hand its trailing agent bubble to the bg buffer so further
                            // tokens continue it there (no signal write per token).
                            if view_tab != id {
                                let mut snap = messages.peek().clone();
                                // Settle any spinner in the outgoing view: once this tab
                                // is backgrounded its tool's ToolCallEnd routes to the bg
                                // buffer (which has no End handler) and never reaches this
                                // saved snapshot — so a running:true row would spin forever
                                // until the tab is reopened. Mark them done at switch-out.
                                for c in snap.iter_mut() {
                                    if let Author::Activity { running, .. } = &mut c.author { *running = false; }
                                }
                                if busy_tabs.peek().contains(&view_tab) {
                                    let seed = if matches!(snap.last().map(|m| &m.author), Some(Author::Agent)) {
                                        snap.pop()
                                    } else { None };
                                    if let Some(s) = seed {
                                        bg_buffers.entry(view_tab).or_default().insert(0, s);
                                    }
                                }
                                if let Some(t) = tabs.write().iter_mut().find(|t| t.id == view_tab) {
                                    t.messages = snap;
                                }
                            }
                            view_tab = id;
                            if let Some(saved_env_tab) = env_tab_by_tab.peek().get(&id).cloned() {
                                env_tab.set(saved_env_tab);
                            }
                            cur_ws = workspace_of(&conf);
                            approvals.write().clear();
                            checkpoints.write().clear();
                            timeline.write().clear();
                            queue.write().clear();
                            questions.write().clear();
                            followups.write().clear();
                            thinking.set(String::new());
                            bg_jobs.write().clear();
                            // These are GLOBAL signals, not per-tab — clear them on tab
                            // switch too, else the previous tab's subagent cards
                            // linger on the newly-viewed tab ("stuck, won't disappear").
                            subagent_cards.write().clear();
                            // The tab's snapshot + anything that streamed while it was
                            // backgrounded (drained from the bg buffer in one write).
                            let mut cur_msgs = tabs.peek().iter().find(|t| t.id == id).map(|t| t.messages.clone()).unwrap_or(msgs);
                            if let Some(buf) = bg_buffers.remove(&id) {
                                cur_msgs.extend(buf);
                            }
                            if let Some(t) = tabs.write().iter_mut().find(|t| t.id == id) { t.messages = cur_msgs.clone(); }
                            *messages.write() = cur_msgs; // this tab's transcript
                            let busy = busy_tabs.peek().contains(&id);
                            streaming.set(busy);
                            status.set(if busy { "Working…".to_string() } else { String::new() });
                            turn_start.set(None);
                            elapsed_s.set(0);
                            // Replay approvals/questions that arrived while backgrounded.
                            if let Some(v) = parked_appr.remove(&id) {
                                approvals.write().extend(v);
                            }
                            if let Some(v) = parked_q.remove(&id) {
                                questions.write().extend(v);
                            }
                        }
                        Some(EngineCmd::CloseTab(id)) => {
                            if let Some(h) = handles.remove(&id) {
                                let _ = h.submit(Op::Interrupt).await;
                            }
                            if let Some(f) = fwds.remove(&id) {
                                f.abort();
                            }
                            gens.remove(&id);
                            parked_appr.remove(&id);
                            parked_q.remove(&id);
                            bg_buffers.remove(&id);
                            busy_tabs.write().remove(&id);
                            tab_statuses.write().remove(&id);
                        }
                        Some(EngineCmd::Answer { id, text }) => {
                            if let Some(tid) = active_id!() {
                                if let Some(h) = handles.get(&tid) {
                                    busy_tabs.write().insert(tid);
                                    tab_statuses.write().insert(tid, TabStatus::Running);
                                    let _ = h.submit(Op::QuestionAnswer { request_id: id, answer: text.clone() }).await;
                                }
                            }
                            questions.write().retain(|(qid, _, _)| *qid != id);
                            messages.write().push(ChatMsg { author: Author::User, text });
                            scroll_chat_bottom();
                        }
                        Some(EngineCmd::Approve { id, decision }) => {
                            if let Some(tid) = active_id!() {
                                if let Some(h) = handles.get(&tid) {
                                    busy_tabs.write().insert(tid);
                                    tab_statuses.write().insert(tid, TabStatus::Running);
                                    let _ = h.submit(Op::ApprovalResponse { request_id: id, decision }).await;
                                }
                            }
                            approvals.write().retain(|(aid, _, _)| *aid != id);
                        }
                        Some(EngineCmd::Rewind { id }) => {
                            if let Some(h) = active_id!().and_then(|t| handles.get(&t)) {
                                let _ = h.submit(Op::Rewind { checkpoint_id: id }).await;
                            }
                        }
                        Some(EngineCmd::SubagentControl { worker_id, action }) => {
                            if let Some(h) = active_id!().and_then(|t| handles.get(&t)) {
                                let _ = h
                                    .submit(Op::SubagentControl { worker_id, action })
                                    .await;
                            }
                        }
                        Some(EngineCmd::SetHistory(msgs)) => {
                            if let Some(h) = active_id!().and_then(|t| handles.get(&t)) {
                                let _ = h.submit(Op::SetHistory { msgs }).await;
                            }
                        }
                        Some(EngineCmd::Interrupt) => {
                            let aid = active_id!();
                            if let Some(h) = aid.and_then(|t| handles.get(&t)) {
                                let _ = h.submit(Op::Interrupt).await;
                            }
                            if let Some(t) = aid {
                                busy_tabs.write().remove(&t);
                                tab_statuses.write().remove(&t);
                            }
                            streaming.set(false);
                            status.set(String::new());
                            todos.write().clear();
                            subagent_cards.write().clear();
                        }
                        None => break,
                      }
                    },
                    // Idle-flush: the delta handler only paints when >=33ms passed
                    // since the last paint OR the buffer grew past 800B, so the final
                    // sub-33ms tail of a burst sits in agent_buf until the NEXT event.
                    // If the provider then pauses (bursty/slow stream, end-of-text gap
                    // before TurnFinished), nothing wakes the loop and the buffered tail
                    // stays invisible — chrome (spinner/shimmer/elapsed) keeps moving so
                    // it reads as "frozen then jumps". This arm bounds tail latency to
                    // ~50ms. The `if !agent_buf.is_empty()` guard is REQUIRED: select!
                    // only builds the sleep future when the precondition is true, so an
                    // empty buffer means no timer (no busy-spin); flush_agent! empties
                    // the buffer via mem::take so the arm self-disables after one fire.
                    // Anchored to last_paint (not loop-idle) so a chatty background tab
                    // can't starve the foreground tail.
                    _ = tokio::time::sleep(
                        std::time::Duration::from_millis(50)
                            .saturating_sub(last_paint.elapsed()),
                    ), if !agent_buf.is_empty() => {
                        flush_agent!();
                    },
                    Some((ev_tid, ev_gen, ev)) = ev_rx.recv() => {
                        // Drop events from a replaced engine (stale generation).
                        if gens.get(&ev_tid).copied() != Some(ev_gen) {
                            continue;
                        }
                        // Per-tab busy bookkeeping (drives sidebar spinners).
                        match &ev {
                            Event::TurnStarted { .. } => {
                                busy_tabs.write().insert(ev_tid);
                                tab_statuses.write().insert(ev_tid, TabStatus::Running);
                            }
                            Event::ApprovalRequested { .. } => {
                                tab_statuses.write().insert(ev_tid, TabStatus::WaitingApproval);
                            }
                            Event::QuestionAsked { .. } => {
                                tab_statuses.write().insert(ev_tid, TabStatus::WaitingInput);
                            }
                            Event::TurnFinished { .. } => {
                                busy_tabs.write().remove(&ev_tid);
                                tab_statuses.write().remove(&ev_tid);
                            }
                            Event::Error { message } => {
                                busy_tabs.write().remove(&ev_tid);
                                if message.starts_with("mcp '") {
                                    tab_statuses.write().remove(&ev_tid);
                                } else {
                                    tab_statuses.write().insert(ev_tid, TabStatus::Failed);
                                }
                            }
                            _ => {}
                        }
                        // Events from a BACKGROUND tab go to its buffered transcript;
                        // only the view-bound tab writes to the live view below.
                        if ev_tid != view_tab {
                            // Append to the plain bg buffer — NO `tabs` signal write, so a
                            // backgrounded tab's token stream never re-schedules the UI.
                            let buf = bg_buffers.entry(ev_tid).or_default();
                            match ev {
                                Event::AgentMessageDelta { text, .. } => {
                                    match buf.last_mut() {
                                        Some(l) if l.author == Author::Agent => l.text.push_str(&text),
                                        _ => buf.push(ChatMsg { author: Author::Agent, text }),
                                    }
                                }
                                Event::Info { text } => {
                                    if text.starts_with("session") || text.starts_with("mcp ") || text.starts_with("mcp '") {
                                        // noise
                                        } else if let Some(label) = text.strip_prefix('\u{2699}').or_else(|| text.strip_prefix('\u{23f3}')) {
                                            let label = label.trim().to_string();
                                            // Suppress the redundant "Running …" notice when a
                                            // command row is already live in this tab's buffer.
                                            let cmd_live = buf.iter().any(|m| matches!(m.author, Author::Activity { running: true, key: Some(_), .. }));
                                            if label.starts_with("Running ") && cmd_live {
                                                continue;
                                            }
                                            let (verb, detail) = label.split_once(' ').unwrap_or((label.as_str(), ""));
                                        let row = format!("spark\t{verb}\t{detail}");
                                        if buf.last().map(|m| m.author == Author::Agent && m.text.is_empty()).unwrap_or(false) {
                                            buf.pop();
                                        }
                                        buf_push_activity(buf, ChatMsg { author: Author::Activity { running: false, ok: true, key: None }, text: row });
                                    } else if is_stage_status(&text) {
                                        // live stage info — meaningless once backgrounded
                                    } else {
                                        buf.push(ChatMsg { author: Author::Note, text });
                                    }
                                }
                                Event::FileDiff { path, diff, checkpoint, .. } => {
                                    buf.push(ChatMsg { author: Author::Diff(path, checkpoint), text: diff });
                                }
                                Event::UiSpec { spec, .. } => {
                                    buf.push(ui_spec_message(*spec));
                                }
                                    Event::ToolCallDelta { call_id, tool, accumulated, .. } => {
                                        upsert_tool_input_preview(buf, call_id, tool, accumulated);
                                    }
                                    Event::ToolCallBegin { call_id, tool, args, .. } => {
                                        if tool != "ask_user" && tool != "shell" {
                                            let text = activity_label(&tool, &args);
                                            if let Some(idx) = activity_idx(buf, &call_id) {
                                                buf[idx].text = text;
                                                if let Author::Activity { running, ok, .. } = &mut buf[idx].author {
                                                    *running = true;
                                                    *ok = true;
                                                }
                                            } else {
                                                buf_push_activity(buf, ChatMsg {
                                                    author: Author::Activity { running: true, ok: true, key: Some(call_id) },
                                                    text,
                                                });
                                            }
                                    }
                                }
                                    Event::ToolCallEnd { call_id, output, ok, .. } => {
                                        let mut out = output.trim().to_string();
                                        if out.chars().count() > 4000 {
                                            out = out.chars().take(4000).collect::<String>() + "\n… (truncated)";
                                        }
                                    if let Some(idx) = activity_idx(buf, &call_id) {
                                        if let Author::Activity { running, ok: o, .. } = &mut buf[idx].author {
                                            *running = false;
                                            *o = ok;
                                        }
                                        if !out.is_empty() {
                                            buf[idx].text.push('\t');
                                            buf[idx].text.push_str(&out);
                                            }
                                        }
                                    }
                                    Event::CommandStarted { command_id, command, background, .. } => {
                                        buf_push_activity(buf, ChatMsg {
                                            author: Author::Activity { running: true, ok: true, key: Some(command_id) },
                                            text: command_activity_label(&command, background),
                                        });
                                    }
                                    Event::CommandOutput { command_id, chunk, .. } => {
                                        if let Some(idx) = activity_idx(buf, &command_id) {
                                            append_activity_output(&mut buf[idx].text, &chunk);
                                        } else {
                                            let mut text = command_activity_label(&command_id, false);
                                            append_activity_output(&mut text, &chunk);
                                            buf_push_activity(buf, ChatMsg { author: Author::Activity { running: true, ok: true, key: Some(command_id) }, text });
                                        }
                                    }
                                    Event::CommandFinished { command_id, ok, exit_code, .. } => {
                                        if let Some(idx) = activity_idx(buf, &command_id) {
                                            {
                                                let row = &mut buf[idx];
                                                if let Author::Activity { running, ok: o, .. } = &mut row.author {
                                                    *running = false;
                                                    *o = ok;
                                                }
                                                if let Some(code) = exit_code {
                                                    append_activity_output(&mut row.text, &format!("\n[exit {code}]\n"));
                                                }
                                            }
                                        }
                                    }
                                    Event::SessionPath { path } => {
                                    // Session binding is rare (once) — keep it on the tab itself.
                                    let pb = std::path::PathBuf::from(&path);
                                    if let Some(t) = tabs.write().iter_mut().find(|t| t.id == ev_tid) {
                                        t.session = Some(pb);
                                    }
                                }
                                Event::ApprovalRequested { request_id, tool, summary } => {
                                    parked_appr.entry(ev_tid).or_default().push((request_id, tool.clone(), summary));
                                    buf.push(ChatMsg { author: Author::Note, text: format!("Waiting for approval ({tool}) - open this tab to respond") });
                                }
                                Event::QuestionAsked { request_id, question, options } => {
                                    parked_q.entry(ev_tid).or_default().push((request_id, question.clone(), options));
                                    buf.push(ChatMsg { author: Author::Note, text: format!("Question: {question} - open this tab to answer") });
                                }
                                Event::TurnFinished { .. } => {
                                    if buf.last().map(|m| m.author == Author::Agent && m.text.is_empty()).unwrap_or(false) {
                                        buf.pop();
                                    }
                                    for c in buf.iter_mut() {
                                        if let Author::Activity { running, .. } = &mut c.author { *running = false; }
                                    }
                                    buf.push(ChatMsg { author: Author::Note, text: DONE_NOTE_MARK.into() });
                                    let title = tabs.peek().iter().find(|t| t.id == ev_tid).map(|t| t.title.clone()).unwrap_or_default();
                                    if !title.is_empty() {
                                        push_toast(toasts, toast_seq, "ok", &format!("{title} — finished"));
                                    }
                                    // Background tab done — you're looking elsewhere, always chime.
                                    play_notification_sound(cfg, true);
                                }
                                Event::Error { message } if !message.starts_with("mcp '") => {
                                    buf.push(ChatMsg { author: Author::Note, text: format!("error: {message}") });
                                    push_toast(toasts, toast_seq, "err", &message.chars().take(120).collect::<String>());
                                }
                                _ => {}
                            }
                            continue;
                        }
                        // Land any buffered streaming text before a structural event
                        // (tool/diff/finish) so transcript order is preserved.
                        if !matches!(ev, Event::AgentMessageDelta { .. }) {
                            flush_agent!();
                        }
                        match ev {
                            Event::AgentMessageDelta { text, .. } => {
                                if status.peek().as_str() != "Writing…" {
                                    status.set("Writing…".to_string());
                                }
                                agent_buf.push_str(&text);
                                // Paint at ~30fps or when a sizable chunk has built up.
                                if last_paint.elapsed() >= std::time::Duration::from_millis(33)
                                    || agent_buf.len() > 800
                                {
                                    flush_agent!();
                                }
                            }
                            Event::ReasoningDelta { text, .. } => {
                                thinking.write().push_str(&text);
                                if status.peek().as_str() != "Thinking…" {
                                    status.set("Thinking…".to_string());
                                }
                            }
                            Event::Info { text } => {
                                if text.starts_with("session") || text.starts_with("mcp ") || text.starts_with("mcp '") {
                                    // internal/MCP noise — status shown in the MCP manager, not chat
                                } else if text.starts_with('\u{23f3}') {
                                    // Background task the agent started. Surface it as a
                                    // persistent chip + activity row so the user sees what's
                                    // running (its result won't stream back this turn).
                                    let label = text.trim_start_matches('\u{23f3}').trim().to_string();
                                    if !label.is_empty() && !bg_jobs.read().contains(&label) {
                                        bg_jobs.write().push(label.clone());
                                    }
                                    status.set(format!("Background · {label}"));
                                    let (verb, detail) = label.split_once(' ').unwrap_or((label.as_str(), ""));
                                    let row = format!("terminal\tBackground {verb}\t{detail}");
                                    {
                                        let mut mw = messages.write();
                                        if mw.last().map(|m| m.author == Author::Agent && m.text.is_empty()).unwrap_or(false) {
                                            mw.pop();
                                        }
                                    }
                                    // Route through push_activity! so it lands above a trailing
                                    // "Done" note like every other activity row (never below it).
                                    let running = verb.eq_ignore_ascii_case("running");
                                    push_activity!(ChatMsg { author: Author::Activity { running, ok: true, key: None }, text: row });
                                    } else if text.starts_with('\u{2699}') {
                                        // CLI-driver tool activity: live shimmer + an activity
                                        // trail row in the chat (synara-style).
                                        let mut label = text.trim_start_matches('\u{2699}').trim().to_string();
                                        // Suppress the redundant "Running …" notice when a command
                                        // row is already live (CommandStarted created one).
                                        let cmd_live = messages.read().iter().any(|m| matches!(m.author, Author::Activity { running: true, key: Some(_), .. }));
                                        if label.starts_with("Running ") && cmd_live {
                                            status.set(label);
                                            continue;
                                        }
                                        // "mcp__server__tool …" to "tool · server (MCP)".
                                    if let Some(rest) = label.strip_prefix("mcp__") {
                                        let (srv, tail) = rest.split_once("__").unwrap_or(("", rest));
                                        let (tool, args) = tail.split_once(' ').unwrap_or((tail, ""));
                                        label = format!("{tool} {srv} (MCP){}{args}", if args.is_empty() { "" } else { " " });
                                    }
                                    status.set(label.clone());
                                    // ActivityRow parses "icon\tverb\tdetail".
                                    let (verb, detail) = label.split_once(' ').unwrap_or((label.as_str(), ""));
                                    let icon = if detail.contains("(MCP)") {
                                        "plugins"
                                    } else {
                                        match verb.to_ascii_lowercase().as_str() {
                                            "bash" | "shell" | "running" => "terminal",
                                            "read" => "eye",
                                            "create" | "write" | "edit" | "editing" | "multiedit" | "notebookedit" => "edit",
                                            "grep" | "glob" | "search" | "searching" | "websearch" => "search",
                                            "task" | "agent" => "spark",
                                            _ => "spark",
                                        }
                                    };
                                    let row = format!("{icon}\t{verb}\t{detail}");
                                    {
                                        let mut mw = messages.write();
                                        // The Submit placeholder bubble stays empty while a CLI
                                        // runs tools — pop it so typing dots don't sit stuck
                                        // above the activity trail (deltas reopen a bubble).
                                        if mw.last().map(|m| m.author == Author::Agent && m.text.is_empty()).unwrap_or(false) {
                                            mw.pop();
                                        }
                                    }
                                    push_activity!(ChatMsg { author: Author::Activity { running: false, ok: true, key: None }, text: row });
                                    // CLI edits compute their real diff at turn end — show the
                                    // file in the "Edited files" card NOW as a pending row
                                    // (synara-style live), replaced by the diff when it lands.
                                    if matches!(verb.to_ascii_lowercase().as_str(), "edit" | "editing" | "write" | "multiedit" | "notebookedit") && !detail.is_empty() {
                                        let p = detail.split_whitespace().next().unwrap_or("").to_string();
                                        if !p.is_empty() && !turn_edits.peek().iter().any(|e| e.0 == p) {
                                            turn_edits.write().push((p, 0, 0, 0, String::new()));
                                        }
                                    }
                                } else if is_stage_status(&text) {
                                    // pipeline stage becomes live animated status, not a chat note
                                    status.set(text);
                                } else {
                                    messages.write().push(ChatMsg { author: Author::Note, text });
                                }
                            }
                            Event::Error { message } => {
                                // MCP connect errors surface on the manager dots, not the chat.
                                if !message.starts_with("mcp '") {
                                    push_toast(toasts, toast_seq, "err", &message.chars().take(120).collect::<String>());
                                    messages.write().push(ChatMsg { author: Author::Note, text: format!("error: {message}") });
                                    // A turn-level error means no TurnFinished may come —
                                    // unstick the composer and clear per-turn progress cards
                                    // so stale todo spinners don't survive the failed turn.
                                    streaming.set(false);
                                    status.set(String::new());
                                    todos.write().clear();
                                    subagent_cards.write().clear();
                                    turn_edits.write().clear();
                                    {
                                        let mut m = messages.write();
                                        for c in m.iter_mut() {
                                            if let Author::Activity { running, .. } = &mut c.author {
                                                *running = false;
                                            }
                                        }
                                    }
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
                            Event::Followups { items } => {
                                followups.set(items.into_iter().take(3).collect());
                            }
                            Event::SessionPath { path } => {
                                // Bind the active tab to the EXACT transcript this
                                // engine writes — never guess via newest-file (mixes tabs).
                                let cur = *active_tab.peek();
                                let pb = std::path::PathBuf::from(&path);
                                let mut persist: Option<(String, String)> = None;
                                if let Some(t) = tabs.write().get_mut(cur) {
                                    t.session = Some(pb);
                                    // Now the session row exists — save a non-generic tab title
                                    // to the DB so a later reload shows it (not "Chat").
                                    if t.title != provider_title(&t.provider) && !t.title.is_empty() {
                                        persist = Some((path.clone(), t.title.clone()));
                                    }
                                }
                                if let Some((id, title)) = persist {
                                    oxide_core::db::set_title(&id, &title);
                                }
                            }
                            Event::TurnStarted { turn } => {
                                turn_done = false;
                                thinking.set(String::new());
                                status.set("Working…".to_string());
                                turn_start.set(Some(std::time::Instant::now()));
                                elapsed_s.set(0);
                                turn_edits.write().clear();
                                bg_jobs.write().clear();
                                todos.write().clear();
                                    subagent_cards.write().clear();
                                    edits_expanded.set(false);
                                edits_undone.set(false);
                                accepted.write().clear();
                                think_open.set(None);
                                timeline.write().push(TimelineItem { title: format!("Turn {turn} started"), sub: String::new() });
                            }
                            Event::AuditLog { kind, title, detail, status, .. } => {
                                let sub = if detail.trim().is_empty() {
                                    status.clone()
                                } else {
                                    format!("{status} · {detail}")
                                };
                                timeline.write().push(TimelineItem {
                                    title: format!("Audit · {kind} · {title}"),
                                    sub,
                                });
                            }
                            Event::UiSpec { spec, .. } => {
                                messages.write().push(ui_spec_message(*spec));
                                scroll_chat_bottom_if_sticky();
                            }
                            Event::SubagentStarted { worker_id, profile, task, .. } => {
                                subagent_cards.write().push(SubagentCard {
                                    worker_id,
                                    profile: profile.clone(),
                                    task: task.clone(),
                                        summary: String::new(),
                                        running: true,
                                        ok: true,
                                        logs: Vec::new(),
                                    });
                                timeline.write().push(TimelineItem {
                                    title: format!("Subagent · {profile}"),
                                    sub: task,
                                });
                                scroll_chat_bottom_if_sticky();
                            }
                            Event::SubagentStatus { worker_id, profile, status, detail, .. } => {
                                {
                                    let mut cards = subagent_cards.write();
                                    if let Some(card) = cards.iter_mut().find(|c| c.worker_id == worker_id) {
                                        card.summary = format!("{status}: {detail}");
                                    } else {
                                        cards.push(SubagentCard {
                                            worker_id,
                                            profile: profile.clone(),
                                            task: status.clone(),
                                            summary: detail.clone(),
                                            running: true,
                                            ok: true,
                                            logs: Vec::new(),
                                        });
                                    }
                                }
                                timeline.write().push(TimelineItem {
                                    title: format!("Subagent {status} · {profile}"),
                                    sub: detail,
                                });
                                scroll_chat_bottom_if_sticky();
                            }
                            Event::SubagentFinished { worker_id, profile, task, summary, ok, .. } => {
                                {
                                    let mut cards = subagent_cards.write();
                                    if let Some(card) = cards.iter_mut().find(|c| c.worker_id == worker_id) {
                                        card.summary = summary.clone();
                                        card.running = false;
                                        card.ok = ok;
                                    } else {
                                        cards.push(SubagentCard {
                                            worker_id,
                                            profile: profile.clone(),
                                            task: task.clone(),
                                                summary: summary.clone(),
                                                running: false,
                                                ok,
                                                logs: Vec::new(),
                                            });
                                    }
                                }
                                timeline.write().push(TimelineItem {
                                    title: format!("Subagent {} · {profile}", if ok { "done" } else { "stopped" }),
                                    sub: if summary.trim().is_empty() { task } else { summary },
                                });
                                scroll_chat_bottom_if_sticky();
                            }
                            Event::ApprovalRequested { request_id, tool, summary } => {
                                approvals.write().push((request_id, tool.clone(), summary.clone()));
                                timeline.write().push(TimelineItem { title: format!("Approval needed · {tool}"), sub: summary });
                            }
                            Event::ToolCallDelta { call_id, tool, accumulated, .. } => {
                                status.set(format!("Preparing {tool} input…"));
                                let mut m = messages.write();
                                upsert_tool_input_preview(&mut m, call_id, tool, accumulated);
                                scroll_chat_bottom_if_sticky();
                            }
                            Event::ToolCallBegin { call_id, tool, args, .. } => {
                                timeline.write().push(TimelineItem { title: format!("Tool · {tool}"), sub: "running…".into() });
                                // Live shimmer shows WHAT it's doing ("Reading src/lib.rs…"),
                                // not just a generic verb.
                                status.set(activity_label(&tool, &args));
                                    if tool != "ask_user" && tool != "shell" {
                                        let text = activity_label(&tool, &args);
                                        let idx = activity_idx(&messages.read(), &call_id);
                                        if let Some(idx) = idx {
                                            let mut m = messages.write();
                                            if let Some(c) = m.get_mut(idx) {
                                                c.text = text;
                                                if let Author::Activity { running, ok, .. } = &mut c.author {
                                                    *running = true;
                                                    *ok = true;
                                                }
                                            }
                                        } else {
                                            push_activity!(ChatMsg { author: Author::Activity { running: true, ok: true, key: Some(call_id) }, text });
                                        }
                                    }
                            }
                                Event::ToolCallEnd { call_id, tool, output, ok, .. } => {
                                    timeline.write().push(TimelineItem { title: format!("Tool · {tool}"), sub: if ok { "done".into() } else { "failed".into() } });
                                    // Settle the exact row this call opened — found by its key
                                    // (call_id), never by index — and attach its output. A missing
                                    // row (shell/ask_user, or merged from a backgrounded tab) is a
                                    // no-op; the turn-end sweep settles anything still running.
                                    let mut out = output.trim().to_string();
                                    if out.chars().count() > 4000 {
                                        out = out.chars().take(4000).collect::<String>() + "\n… (truncated)";
                                    }
                                    let idx = activity_idx(&messages.read(), &call_id);
                                    if let Some(idx) = idx {
                                        let mut m = messages.write();
                                        if let Some(c) = m.get_mut(idx) {
                                            if let Author::Activity { running, ok: o, .. } = &mut c.author { *running = false; *o = ok; }
                                            if !(out.is_empty() || tool == "shell" && activity_has_output(&c.text)) {
                                                c.text.push('\t');
                                                c.text.push_str(&out);
                                            }
                                        }
                                    }
                                }
                                Event::CommandStarted { command_id, worker_id, command, cwd, background, .. } => {
                                    timeline.write().push(TimelineItem {
                                        title: if background { "Background command".into() } else { "Command".into() },
                                        sub: format!("{command} · {cwd}"),
                                    });
                                    if let Some(worker_id) = worker_id {
                                        let mut cards = subagent_cards.write();
                                        if let Some(card) = cards.iter_mut().find(|c| c.worker_id == worker_id) {
                                            card.logs.push(CommandLog {
                                                command_id,
                                                command,
                                                output: String::new(),
                                                running: true,
                                                ok: true,
                                            });
                                        }
                                    } else {
                                        if background && !bg_jobs.read().iter().any(|j| j == &command) {
                                            bg_jobs.write().push(command.clone());
                                        }
                                        status.set(if background { format!("Background · {command}") } else { format!("Running · {command}") });
                                        // Insert above any trailing Done note (CLI drivers
                                        // like claude can surface a command row after the turn's
                                        // text + Done landed) so it never renders below the footer.
                                        push_activity!(ChatMsg {
                                            author: Author::Activity { running: true, ok: true, key: Some(command_id) },
                                            text: command_activity_label(&command, background),
                                        });
                                    }
                                    scroll_chat_bottom_if_sticky();
                                }
                                Event::CommandOutput { command_id, worker_id, stream, chunk, .. } => {
                                    if let Some(worker_id) = worker_id {
                                        let mut cards = subagent_cards.write();
                                        if let Some(card) = cards.iter_mut().find(|c| c.worker_id == worker_id) {
                                            if let Some(log) = card.logs.iter_mut().find(|log| log.command_id == command_id) {
                                                if stream == "stderr" && !chunk.trim().is_empty() {
                                                    log.output.push_str("[stderr] ");
                                                }
                                                log.output.push_str(&chunk);
                                                if log.output.chars().count() > 7000 {
                                                    log.output = log.output.chars().rev().take(6000).collect::<Vec<_>>().into_iter().rev().collect();
                                                    log.output.insert_str(0, "… (output truncated)\n");
                                                }
                                            }
                                        }
                                    } else if let Some(idx) = { let g = messages.read(); activity_idx(&g, &command_id) } {
                                        if let Some(row) = messages.write().get_mut(idx) {
                                            let chunk = if stream == "stderr" && !chunk.trim().is_empty() { format!("[stderr] {chunk}") } else { chunk };
                                            append_activity_output(&mut row.text, &chunk);
                                        }
                                    }
                                }
                                Event::CommandFinished { command_id, worker_id, ok, exit_code, duration_ms, .. } => {
                                    let footer = match exit_code {
                                        Some(code) => format!("\n[exit {code} · {}ms]\n", duration_ms),
                                        None => format!("\n[done · {}ms]\n", duration_ms),
                                    };
                                    if let Some(worker_id) = worker_id {
                                        let mut cards = subagent_cards.write();
                                        if let Some(card) = cards.iter_mut().find(|c| c.worker_id == worker_id) {
                                            if let Some(log) = card.logs.iter_mut().find(|log| log.command_id == command_id) {
                                                log.running = false;
                                                log.ok = ok;
                                                log.output.push_str(&footer);
                                            }
                                        }
                                    } else if let Some(idx) = { let g = messages.read(); activity_idx(&g, &command_id) } {
                                        if let Some(row) = messages.write().get_mut(idx) {
                                            if let Author::Activity { running, ok: o, .. } = &mut row.author { *running = false; *o = ok; }
                                            append_activity_output(&mut row.text, &footer);
                                        }
                                    }
                                    scroll_chat_bottom_if_sticky();
                                }
                                Event::Todos { items } => {
                                    todos.set(items);
                                    scroll_chat_bottom_if_sticky();
                                }
                            Event::PatchApplied { path, .. } => {
                                timeline.write().push(TimelineItem { title: "Patched".into(), sub: path });
                                let v = *git_refresh.read();
                                git_refresh.set(v + 1); // trigger git-tab auto-refresh
                            }
                            Event::FileDiff { path, diff, checkpoint, .. } => {
                                let base = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
                                let nb = base(&path);
                                if diff.trim().is_empty() {
                                    // Touched but no textual change, so drop the pending
                                    // "editing…" row so its spinner doesn't linger.
                                    turn_edits.write().retain(|e| !(e.4.is_empty() && e.3 == 0 && base(&e.0) == nb));
                                } else {
                                    let (adds, dels) = diff_counts(&diff);
                                    // Upsert: replace the provisional "editing…" row with the
                                    // real diff. The pending path may be ABSOLUTE (claude's tool
                                    // input) while this one is workspace-relative (git diff), so
                                    // match exact first, then any pending row by file name.
                                    {
                                        let real = (path.clone(), adds, dels, checkpoint, diff.clone());
                                        let mut te = turn_edits.write();
                                        if let Some(e) = te.iter_mut().find(|e| e.0 == path) {
                                            *e = real;
                                        } else if let Some(e) = te.iter_mut().find(|e| e.4.is_empty() && e.3 == 0 && base(&e.0) == nb) {
                                            *e = real;
                                        } else {
                                            te.push(real);
                                        }
                                    }
                                    messages.write().push(ChatMsg { author: Author::Diff(path, checkpoint), text: diff });
                                }
                            }
                            Event::HookFired { hook, command, blocked } => {
                                timeline.write().push(TimelineItem {
                                    title: format!("Hook · {hook}{}", if blocked { " · blocked" } else { "" }),
                                    sub: command,
                                });
                            }
                            Event::BrowserTargetChanged { url, note, .. } => {
                                timeline.write().push(TimelineItem { title: format!("Browser open · {url}"), sub: note });
                            }
                            Event::BrowserSnapshotRequested { url, note, .. } => {
                                timeline.write().push(TimelineItem { title: format!("Browser snapshot · {url}"), sub: note });
                            }
                            Event::DesignSnapshotRequested { url, note, .. } => {
                                if !url.trim().is_empty() {
                                    preview_url.set(url.clone());
                                    show_env.set(true);
                                    env_tab.set("preview".to_string());
                                    design_mode.set(true);
                                    spawn(async move { let _ = document::eval("document.querySelector('.preview-frame')?.contentWindow?.postMessage('oxide-design-on','*')").await; });
                                }
                                timeline.write().push(TimelineItem { title: format!("Design snapshot {url}"), sub: note });
                            }
                            Event::DesignPatchProposed { proposal, .. } => {
                                timeline.write().push(TimelineItem {
                                    title: format!("Design patch · {}", proposal.selection.selector),
                                    sub: format!("{} pending edit(s)", proposal.edits.len()),
                                });
                            }
                            Event::DesignReviewCompleted { review, .. } => {
                                timeline.write().push(TimelineItem {
                                    title: format!("Design review · score {}", review.score),
                                    sub: format!("ok={} · {} finding(s)", review.ok, review.findings.len()),
                                });
                            }
                            Event::QuestionAsked { request_id, question, options } => {
                                questions.write().push((request_id, question, options));
                            }
                            Event::RateLimit { plan, primary_pct, secondary_pct, primary_reset_s, secondary_reset_s } => {
                                let p_rem = 100u8.saturating_sub(primary_pct);
                                let s_rem = 100u8.saturating_sub(secondary_pct);
                                timeline.write().push(TimelineItem {
                                    title: "ChatGPT subscription usage".into(),
                                    sub: format!("5h {p_rem}% left · weekly {s_rem}% left"),
                                });
                                // Format reset times as a local clock (5h) / date (weekly), like Codex.
                                let js = format!(
                                    "const P={primary_reset_s},S={secondary_reset_s};const p=new Date(Date.now()+P*1000),s=new Date(Date.now()+S*1000);const t=d=>d.toLocaleTimeString([],{{hour:'numeric',minute:'2-digit'}});const dd=d=>d.toLocaleDateString([],{{month:'short',day:'numeric'}});return JSON.stringify({{p:t(p),s:dd(s)}});"
                                );
                                // Do NOT await the webview inline — a stalled eval here
                                // blocks the whole engine-event loop (frozen stream).
                                let mut usage_info = usage_info;
                                spawn(async move {
                                    let labels = dioxus::document::eval(&js).join::<String>().await.unwrap_or_default();
                                    let lv: serde_json::Value = serde_json::from_str(&labels).unwrap_or(serde_json::Value::Null);
                                    let pl = lv["p"].as_str().unwrap_or("").to_string();
                                    let sl = lv["s"].as_str().unwrap_or("").to_string();
                                    usage_info.set(Some(("gpt".into(), plan, p_rem, s_rem, pl, sl)));
                                });
                            }
                            Event::CheckpointCreated { id, label, .. } => {
                                checkpoints.write().push((id, label.clone()));
                                timeline.write().push(TimelineItem { title: format!("⎌ checkpoint #{id}"), sub: label });
                            }
                            Event::RewindDone { id, restored } => {
                                timeline.write().push(TimelineItem { title: format!("⎌ rewound to #{id}"), sub: format!("{restored} file(s) restored") });
                                // Confirm in the transcript too — the timeline tab is rarely open.
                                messages.write().push(ChatMsg { author: Author::Note, text: format!("⎌ Restored {restored} file(s) (checkpoint #{id})") });
                            }
                            Event::TokensUsed { input, output, cached_input, .. } => {
                                usage.set((input, output, cached_input));
                            }
                            Event::TurnStatus { state: s, detail, .. } => {
                                // Authoritative pushed working-state. The visible new
                                // signal is "retrying" — show it instead of an apparent
                                // freeze while the engine re-requests after a transient cut.
                                if s == "retrying" {
                                    status.set(if detail.is_empty() {
                                        "Retrying…".to_string()
                                    } else {
                                        format!("Retrying… ({detail})")
                                    });
                                }
                            }
                            Event::Compacted { dropped, tokens } => {
                                timeline.write().push(TimelineItem { title: "∿ context compacted".into(), sub: format!("dropped {dropped} · ~{tokens} tok") });
                            }
                            Event::TurnFinished { .. } => {
                                streaming.set(false);
                                status.set(String::new());
                                // Todo/task cards are turn-scoped progress, not durable transcript
                                // content. Clear them at finish so an interrupted/aborted model turn
                                // cannot leave a perpetual "Tasks 0/N" spinner in the composer area.
                                todos.write().clear();
                                // Drop any "editing…" rows that never got a real diff (e.g.
                                // a read that was mislabeled, or a path that didn't match) so
                                // no spinner lingers after the turn is done.
                                turn_edits.write().retain(|e| !(e.4.is_empty() && e.3 == 0));
                                // Subagent cards are turn-scoped too: if a SubagentFinished
                                // never arrived (worker died, or a card was synthesized from a
                                // status), settle them so the "Agents" badge + running counter
                                // don't keep spinning/over-reporting after the turn ends.
                                for c in subagent_cards.write().iter_mut() {
                                    c.running = false;
                                }
                                {
                                    let mut mw = messages.write();
                                    if mw.last().map(|m| m.author == Author::Agent && m.text.is_empty()).unwrap_or(false) {
                                        mw.pop();
                                    }
                                    // Persist the thinking as a collapsed "Thought for Ns"
                                    // row above the reply (Cursor Glass style).
                                    let th = thinking.peek().clone();
                                    if !th.trim().is_empty() {
                                        let secs = turn_start.peek().as_ref().map(|t| t.elapsed().as_secs()).unwrap_or(0).max(1);
                                        let row = ChatMsg { author: Author::Note, text: format!("§thought\t{secs}\t{th}") };
                                        if let Some(pos) = mw.iter().rposition(|m| m.author == Author::Agent && !m.text.is_empty()) {
                                            mw.insert(pos, row);
                                        } else {
                                            mw.push(row);
                                        }
                                    }
                                }
                                thinking.set(String::new());
                                // Cheap contextual follow-up suggestions (Claude Code-style).
                                {
                                    let had_error = messages.peek().iter().rev().take(6)
                                        .any(|m| m.author == Author::Note && m.text.starts_with("error:"));
                                    let edited = !turn_edits.peek().is_empty();
                                    let mut f: Vec<String> = Vec::new();
                                    if had_error {
                                        f.push("Fix the error above".into());
                                    }
                                    if edited {
                                        f.push("Review the changes you just made and fix any issues".into());
                                        f.push("Run the relevant build/tests and fix failures".into());
                                        f.push("Commit these changes with a clear message".into());
                                    }
                                    // Prose-only turns get no generic filler — chips stay
                                    // hidden unless the model generates real ones.
                                    f.truncate(3);
                                    followups.set(f);
                                }
                                sessions_refresh.set(sessions_refresh() + 1);
                                {
                                    let mut sr = sessions_refresh;
                                    spawn(async move {
                                        tokio::time::sleep(std::time::Duration::from_millis(2600)).await;
                                        sr.set(sr() + 1);
                                    });
                                }
                                // New/updated sessions show up right away
                                // (fs walk off the event thread).
                                {
                                    let c = cfg.peek().clone();
                                    let mut pl = projects_list;
                                    spawn(async move {
                                        let groups = tokio::task::spawn_blocking(move || {
                                            build_projects(&workspace_of(&c), &c.recent_workspaces)
                                        }).await.unwrap_or_default();
                                        pl.set(groups);
                                    });
                                }
                                // Settle any activity still showing a spinner so none stays
                                // "running" stuck at the bottom after the turn ends.
                                {
                                    let mut m = messages.write();
                                    for c in m.iter_mut() {
                                        if let Author::Activity { running, .. } = &mut c.author { *running = false; }
                                    }
                                }
                                // Background tasks the agent kicked off won't stream their
                                // result back this turn - tell the user plainly so the
                                // "I'll let you know when done" never silently dangles.
                                if !bg_jobs.peek().is_empty() {
                                    let jobs = bg_jobs.peek().join(", ");
                                    messages.write().push(ChatMsg { author: Author::Note, text: format!("Background task(s) still running: {jobs} - the result won't return automatically. Ask the agent to check the output, or check the Environment / Local Servers panel.") });
                                }
                                if let Some(start) = turn_start.write().take() {
                                    let secs = start.elapsed().as_secs();
                                    let dur = if secs >= 60 { format!("{}m {}s", secs / 60, secs % 60) } else { format!("{secs}s") };
                                    // Cursor-style turn summary: duration + change totals.
                                    let (nf, ta, td) = {
                                        let e = turn_edits.read();
                                        (e.len(), e.iter().map(|x| x.1).sum::<u32>(), e.iter().map(|x| x.2).sum::<u32>())
                                    };
                                    let sum = if nf > 0 { format!("{DONE_NOTE_MARK} · {dur} · {nf} file(s) +{ta} −{td}") } else { format!("{DONE_NOTE_MARK} · {dur}") };
                                    messages.write().push(ChatMsg { author: Author::Note, text: sum });
                                }
                                // From here, late activities slot above the Done note.
                                turn_done = true;
                                // Submit the next queued message as a fresh turn.
                                let next = { let mut q = queue.write(); if q.is_empty() { None } else { Some(q.remove(0)) } };
                                if next.is_none() {
                                    // Foreground turn done — only chime if the window isn't
                                    // focused (you stepped away); no ding while you're watching.
                                    play_notification_sound(cfg, false);
                                }
                                if let Some(text) = next {
                                    if let Some(h) = handles.get(&ev_tid) {
                                        followups.write().clear();
                                        messages.write().push(ChatMsg { author: Author::User, text: text.clone() });
                                        messages.write().push(ChatMsg { author: Author::Agent, text: String::new() });
                                        scroll_chat_bottom();
                                        streaming.set(true);
                                        // See the send-site above: zero the clock now so the
                                        // pill doesn't flash the prior turn's elapsed seconds.
                                        turn_start.set(Some(std::time::Instant::now()));
                                        elapsed_s.set(0);
                                        busy_tabs.write().insert(ev_tid);
                                        tab_statuses.write().insert(ev_tid, TabStatus::Running);
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
        if a.trim().is_empty() {
            String::new()
        } else {
            format!("--accent: {a}; --on-accent: #ffffff;")
        }
    };

    // Keyboard: Cmd-1 through Cmd-9 jump to tab N, Cmd-Shift-] / Cmd-Shift-[ cycle tabs.
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
            let msg = match eval.recv::<String>().await {
                Ok(m) => m,
                Err(_) => break,
            };
            let n = tabs.read().len();
            if n == 0 {
                continue;
            }
            let cur = *active_tab.read();
            let target = if msg == "next" {
                (cur + 1) % n
            } else if msg == "prev" {
                (cur + n - 1) % n
            } else if let Some(d) = msg.strip_prefix("jump:") {
                d.parse::<usize>()
                    .ok()
                    .map(|x| x.saturating_sub(1))
                    .filter(|&x| x < n)
                    .unwrap_or(cur)
            } else {
                cur
            };
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
            let raw = match eval.recv::<String>().await {
                Ok(m) => m,
                Err(_) => break,
            };
            let v: serde_json::Value = match serde_json::from_str(&raw) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if *design_mode.read() {
                design_sel.set(Some(v));
                design_edits.set(Vec::new());
                design_note.set(String::new());
                continue;
            }
            let sel = v["selector"].as_str().unwrap_or("");
            let src = v["source"].as_str().unwrap_or("");
            let comp = v["component"].as_str().unwrap_or("");
            let text = v["text"].as_str().unwrap_or("");
            let html = v["html"].as_str().unwrap_or("");
            let mut ctx = String::from("Selected UI element to change:\n");
            ctx.push_str(&format!("- selector: {sel}\n"));
            if !comp.is_empty() {
                ctx.push_str(&format!("- component: <{comp}>\n"));
            }
            if !src.is_empty() {
                ctx.push_str(&format!("- source: {src}\n"));
            }
            if !text.is_empty() {
                ctx.push_str(&format!("- text: {text}\n"));
            }
            if !html.is_empty() {
                ctx.push_str(&format!("- html: {html}\n"));
            }
            picked_element.set(Some(ctx));
        }
    });
    // Active TUI tab (embedded terminal) info.
    let (active_is_tui, active_tab_id) = {
        let t = tabs.read();
        match t.get(*active_tab.read()) {
            Some(tab) if tab.mode == "tui" => (true, tab.id),
            _ => (false, 0),
        }
    };
    let branch = git_branch(&workspace);
    let ws_changes = workspace.clone();
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
    use_future(move || async move {
        // Crash recovery once: a run left queued/running by a previous app session
        // (closed mid-run) is reconciled to "interrupted" so it never shows as a
        // perpetual ghost and the scheduler starts clean.
        if let Some(root) = cfg.peek().workspace.clone() {
            if let Ok(n) = automation::reconcile_orphaned_runs(&root) {
                if n > 0 {
                    if let Ok(next) = automation::read_runs(&root) {
                        automation_runs.set(next);
                    }
                }
            }
        }
        loop {
            let root = cfg.peek().workspace.clone();
            let mut delay_ms = 30_000u64;
            if let Some(root) = root {
                let now = automation::now_ms();
                let specs = automations.peek().clone();
                let runs_snapshot = automation_runs.peek().clone();
                // Fire EVERY due automation (each run_automation_turn records its
                // run, so the next tick won't re-fire it; a busy engine queues them).
                for spec in automation::due_automations(&specs, &runs_snapshot, now) {
                    run_automation_turn(
                        root.clone(),
                        spec.clone(),
                        "scheduled",
                        engine,
                        streaming,
                        queue,
                        automation_runs,
                        automation_status,
                    );
                }
                // Sleep until the soonest upcoming automation (clamped to [10s,30s])
                // instead of a fixed 30s — efficient, still responsive to edits.
                let specs = automations.peek().clone();
                let runs = automation_runs.peek().clone();
                delay_ms = automation::next_wakeup_ms(&specs, &runs)
                    .map(|t| t.saturating_sub(automation::now_ms()))
                    .unwrap_or(30_000)
                    .clamp(10_000, 30_000);
            }
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        }
    });
    // Effort is shown by its own pill — keep the model label clean.
    let (ctx_used, ctx_cached) = {
        let u = usage.read();
        (u.0, u.2)
    };
    // Prompt-cache hit rate (ChatGPT/Codex + OpenAI/Anthropic report it) — shows
    // how much of the input was served from cache (cheaper + faster).
    let cache_suffix = if ctx_cached > 0 && ctx_used > 0 {
        format!(
            " · {}% cached",
            ctx_cached
                .saturating_mul(100)
                .saturating_div(ctx_used)
                .min(100)
        )
    } else {
        String::new()
    };
    let model_label = match *context_limit.read() {
        Some(limit) => format!("{model_name} · {}{cache_suffix}", fmt_tokens(limit)),
        None => format!("{model_name}{cache_suffix}"),
    };
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
        {
            let sel = *term_sel.peek();
            if let Some(t) = terms.write().get_mut(sel) {
                t.2.push(format!("$ {cmd}"));
            }
        }
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
            let sel = *term_sel.peek();
            if let Some(t) = terms.write().get_mut(sel) {
                t.2.push(text);
            }
        });
    };

    rsx! {
        style { {CSS} }
        style { {WTERM_CSS} }
        div { class: "app", "data-theme": "{cfg.read().theme}", "data-density": "{cfg.read().density}", style: "{accent_style}",
            onmousemove: move |e: dioxus::prelude::MouseEvent| {
                if let Some((which, sx, sw)) = *panel_drag.read() {
                    let x = e.client_coordinates().x;
                    let y = e.client_coordinates().y;
                    match which {
                        1 => sidebar_w.set((sw + (x - sx)).clamp(170.0, 440.0)),
                        3 => rpanel_w.set((sw + (sx - x)).clamp(320.0, 1100.0)),
                        4 => term_h.set((sw + (sx - y)).clamp(120.0, 600.0)),
                        _ => insp_w.set((sw + (sx - x)).clamp(220.0, 560.0)),
                    }
                }
            },
            onmouseup: move |_| {
                if panel_drag.read().is_some() {
                    panel_drag.set(None);
                    // Persist the new widths.
                    let mut cfg = cfg;
                    let mut c = cfg.read().clone();
                    c.sidebar_width = *sidebar_w.read();
                    c.inspector_width = *insp_w.read();
                    c.env_width = *rpanel_w.read();
                    cfg.set(c.clone());
                    if let Ok(t) = toml::to_string(&c) {
                        let _ = std::fs::write(workspace_of(&c).join("oxide.toml"), &t);
                        if let Some(home) = std::env::var_os("HOME") {
                            let d = std::path::PathBuf::from(home).join(".config/oxide");
                            let _ = std::fs::create_dir_all(&d);
                            let _ = std::fs::write(d.join("config.toml"), &t);
                        }
                    }
                }
            },
            // ── Sidebar ────────────────────────────────────────────────
            aside { class: if *sidebar_collapsed.read() { "sidebar collapsed" } else { "sidebar" },
                style: if *sidebar_collapsed.read() { String::new() } else { format!("width:{}px", *sidebar_w.read()) },
                oncontextmenu: move |e: dioxus::prelude::MouseEvent| { e.prevent_default(); let c = e.client_coordinates(); theme_menu_pos.set((c.x, c.y)); session_menu.set(None); show_theme_menu.set(true); },
                if *show_theme_menu.read() {
                    div { class: "menu-backdrop", onclick: move |_| show_theme_menu.set(false) }
                    div { class: "theme-menu", style: "left: {theme_menu_pos.read().0}px; top: {theme_menu_pos.read().1}px;",
                        button { class: "menu-item", onclick: {
                            let win = win.clone();
                            move |_| { let v = !*pinned.read(); pinned.set(v); win.set_always_on_top(v); show_theme_menu.set(false); }
                        },
                            Icon { name: "target" } span { class: "menu-name", "Pin window" }
                            if *pinned.read() { span { class: "menu-check", Icon { name: "check" } } }
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
                            if cfg.read().theme == "light" { span { class: "menu-check", Icon { name: "check" } } }
                        }
                        button { class: "menu-item", onclick: move |_| { set_theme(cfg, "dark"); show_theme_menu.set(false); },
                            Icon { name: "target" } span { class: "menu-name", "Dark" }
                            if cfg.read().theme == "dark" { span { class: "menu-check", Icon { name: "check" } } }
                        }
                        button { class: "menu-item", onclick: move |_| { set_theme(cfg, "system"); show_theme_menu.set(false); },
                            Icon { name: "settings" } span { class: "menu-name", "System" }
                            if cfg.read().theme == "system" { span { class: "menu-check", Icon { name: "check" } } }
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
                    img { class: "logo", src: logo_uri(),
                          title: "Collapse/expand sidebar",
                          onclick: move |_| { let v = *sidebar_collapsed.read(); sidebar_collapsed.set(!v); } }
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
                            engine.send(EngineCmd::Reconfigure(cfg.read().clone()));
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
                    button { class: "nav-item", onclick: move |_| {
                            settings_initial_tab.set("automations".to_string());
                            show_settings.set(true);
                        },
                        Icon { name: "clock" } span { "Automations" }
                    }
                }
                div { class: "section-row",
                    span { class: "section-label", "Projects" }
                    button { class: "section-add", title: "Open folder", onclick: move |_| open_folder(cfg, ui, engine),
                        Icon { name: "plus" }
                    }
                }
                div { class: "projects",
                    if cfg.read().workspace.is_none() {
                        button { class: "open-codebase", onclick: move |_| open_folder(cfg, ui, engine),
                            Icon { name: "folder" } span { "Open codebase" }
                        }
                    }
                    // Welcome state still lists known projects + their chats.
                    if cfg.read().workspace.is_some() || !projects_list.read().is_empty() {
                        {
                            let _ = sessions_refresh.read();
                            let pins: Vec<(PathBuf, String)> = oxide_core::db::pinned()
                                .into_iter()
                                .map(|m| (PathBuf::from(m.id), if m.title.trim().is_empty() { "Chat".to_string() } else { m.title }))
                                .collect();
                            if pins.is_empty() { rsx!{} } else {
                                rsx! {
                                    div { class: "section-label", "Pinned" }
                                    for (p, title) in pins {
                                        {
                                            let p_open = p.clone();
                                            let t_open = title.clone();
                                            let p_str = p.display().to_string();
                                            let p_pin = p_str.clone();
                                            let anchor_class = if restored_sessions.read().contains(&p_str) { "thread-anchor restored" } else { "thread-anchor" };
                                            rsx! {
                                                div { class: "{anchor_class}",
                                                    div { class: "row-actions",
                                                        button { class: "row-act-btn pinned", title: "Unpin", onclick: move |e: dioxus::prelude::MouseEvent| { e.stop_propagation(); toggle_pin(cfg, &p_pin); sessions_refresh.set(sessions_refresh() + 1); }, Icon { name: "pin" } }
                                                    }
                                                    div { class: "thread recent",
                                                        onclick: move |_| { show_board.set(false); open_session_tab(tabs, active_tab, messages, next_tab_id, cfg, ui, engine, busy_tabs, p_open.clone(), t_open.clone()); },
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
                                let pkey = pws.display().to_string();
                                let pname2 = pkey.clone();
                                let pname_col = pkey.clone();
                                let collapsed = collapsed_projects.read().contains(&pkey);
                                let total = sessions.len();
                                let requested = project_session_pages.read().get(&pkey).copied().unwrap_or(PROJECT_SESSION_PAGE_SIZE);
                                let shown = requested.min(total).max(PROJECT_SESSION_PAGE_SIZE.min(total));
                                let can_show_more = shown < total;
                                let show_more_label = if can_show_more { "Show more" } else { "Show less" };
                                let pws_switch = pws.clone();
                                rsx! {
                                  div { key: "{pkey}", class: "project-group",
                                    div { class: if is_current { "project current" } else { "project" },
                                        title: if is_current { "" } else { "Switch to this project" },
                                        onclick: move |_| { if !is_current { apply_workspace(cfg, ui, engine, pws_switch.clone()); } },
                                        span { class: if collapsed { "proj-caret closed" } else { "proj-caret" },
                                            onclick: move |e: dioxus::prelude::MouseEvent| {
                                                e.stop_propagation();
                                                let mut c = collapsed_projects.write();
                                                if !c.remove(&pname_col) { c.insert(pname_col.clone()); }
                                            },
                                            Icon { name: "chevron" }
                                        }
                                        Icon { name: "folder" }
                                        span { class: "project-name", "{pname}" }
                                        if is_current && (*streaming.read() || !busy_tabs.read().is_empty()) { span { class: "syn-spinner", style: "margin-left:6px" } }
                                        button { class: "project-del", title: "Remove this project's chats from the list",
                                            onclick: {
                                                let pdel = pws.clone();
                                                move |e: dioxus::prelude::MouseEvent| {
                                                    e.stop_propagation();
                                                    let key = pdel.display().to_string();
                                                    if confirm_archive_workspace.peek().as_deref() != Some(key.as_str()) {
                                                        confirm_archive_workspace.set(Some(key));
                                                        push_toast(toasts, toast_seq, "info", "Click project trash again to hide its chats");
                                                        return;
                                                    }
                                                    confirm_archive_workspace.set(None);
                                                    let restore_ids: Vec<String> = oxide_core::db::list(&pdel, 500)
                                                        .into_iter()
                                                        .map(|m| m.id)
                                                        .collect();
                                                    oxide_core::db::archive_workspace(&pdel);
                                                    let mut cfg = cfg;
                                                    let mut c = cfg.read().clone();
                                                    c.recent_workspaces.retain(|p| p != &pdel);
                                                    cfg.set(c);
                                                    sessions_refresh.set(sessions_refresh() + 1);
                                                    refresh_projects_list(projects_list, cfg);
                                                    if !restore_ids.is_empty() {
                                                        push_action_toast(
                                                            toasts,
                                                            toast_seq,
                                                            "info",
                                                            "Project chats hidden",
                                                            "Restore",
                                                            ToastAction::RestoreSessions(restore_ids),
                                                        );
                                                    }
                                                }
                                            },
                                            Icon { name: "trash" }
                                        }
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
                                    if is_current && !collapsed {
                                        for (i, t) in tabs.read().iter().enumerate() {
                                            {
                                                let id = t.id;
                                                let ttl = if t.title.is_empty() { "New chat".to_string() } else { t.title.clone() };
                                                let is_active = i == *active_tab.read();
                                                // Per-tab: a backgrounded tab keeps its spinner while its turn runs.
                                                let busy = busy_tabs.read().contains(&id) || (is_active && *streaming.read());
                                                let prov = t.provider.clone();
                                                let logo = provider_logo(&prov);
                                                let tab_status = tab_statuses.read().get(&id).cloned();
                                                let tab_status_class_name = tab_status.as_ref().map(tab_status_class).unwrap_or("");
                                                let tab_status_label_text = tab_status.as_ref().map(tab_status_label).unwrap_or("");
                                                let editing = *renaming_tab.read() == Some(id);
                                                let ttl_dc = ttl.clone();
                                                rsx! {
                                                    div { key: "tab{id}", class: if is_active { "thread active" } else { "thread" },
                                                        onclick: move |_| { show_board.set(false); switch_tab(tabs, active_tab, messages, cfg, engine, i); },
                                                        ondoubleclick: move |_| { rename_text.set(ttl_dc.clone()); renaming_tab.set(Some(id)); },
                                                        span { class: "sess-branch", Icon { name: "branch" } }
                                                        if busy { span { class: "syn-spinner" } }
                                                        else if let Some(l) = logo { span { class: "tab-prov", dangerous_inner_html: l } }
                                                        if editing {
                                                            input { class: "rename-input", value: "{rename_text}", autofocus: true,
                                                                oninput: move |e| rename_text.set(e.value()),
                                                                onkeydown: move |e| {
                                                                    if e.key() == Key::Enter { e.prevent_default(); let n = rename_text.read().trim().to_string(); if !n.is_empty() { rename_tab_title(tabs, id, &n); } renaming_tab.set(None); }
                                                                    else if e.key() == Key::Escape { renaming_tab.set(None); }
                                                                },
                                                                onblur: move |_| { let n = rename_text.read().trim().to_string(); if !n.is_empty() { rename_tab_title(tabs, id, &n); } renaming_tab.set(None); },
                                                                onclick: move |e| e.stop_propagation(),
                                                            }
                                                        } else {
                                                            span { class: "thread-title", title: "{ttl}", "{ttl}" }
                                                        }
                                                        if tab_status.is_some() {
                                                            span {
                                                                class: "tab-state {tab_status_class_name}",
                                                                title: "{tab_status_label_text}",
                                                                "{tab_status_label_text}"
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    {
                                        // One pinned-id query per project render, not per row.
                                        let pinned_ids: std::collections::HashSet<String> =
                                            oxide_core::db::pinned().into_iter().map(|m| m.id).collect();
                                        rsx! {
                                    for (path, title, reltime, sprov) in sessions.iter()
                                        .filter(|(p, _, _, _)| !is_current || !tabs.read().iter().any(|t| t.session.as_deref() == Some(p.as_path())))
                                        .take(if collapsed { 0 } else { shown }).cloned() {
                                        {
                                            let p_open = path.clone();
                                            let p_dbl = path.clone();
                                            let p_del = path.clone();
                                            let p_arch = path.clone();
                                            let p_arch2 = path.clone();
                                            let t_open = title.clone();
                                            let menu_open = session_menu.read().as_ref() == Some(&path);
                                            let path_str = path.display().to_string();
                                            let path_str_pin = path_str.clone();
                                            let path_str_archive = path_str.clone();
                                            let path_str_menu_archive = path_str.clone();
                                            let is_pinned = pinned_ids.contains(&path_str);
                                            let anchor_class = if restored_sessions.read().contains(&path_str) { "thread-anchor restored" } else { "thread-anchor" };
                                            rsx! {
                                                div { class: "{anchor_class}",
                                                    div { class: "row-actions",
                                                        button { class: if is_pinned { "row-act-btn pinned" } else { "row-act-btn" }, title: if is_pinned { "Unpin" } else { "Pin" },
                                                            onclick: move |e: dioxus::prelude::MouseEvent| { e.stop_propagation(); toggle_pin(cfg, &path_str_pin); sessions_refresh.set(sessions_refresh() + 1); }, Icon { name: "pin" } }
                                                        button { class: "row-act-btn", title: "Archive", onclick: move |e: dioxus::prelude::MouseEvent| {
                                                            e.stop_propagation();
                                                            archive_session(&p_arch2);
                                                            sessions_refresh.set(sessions_refresh() + 1);
                                                            refresh_projects_list(projects_list, cfg);
                                                            push_action_toast(
                                                                toasts,
                                                                toast_seq,
                                                                "info",
                                                                "Session archived",
                                                                "Restore",
                                                                ToastAction::RestoreSessions(vec![path_str_archive.clone()]),
                                                            );
                                                        }, Icon { name: "archive" } }
                                                        // Delete is non-destructive here — the visible row action only
                                                        // archives (hides). Permanent delete lives in the right-click
                                                        // menu and in Settings / Archived sessions, so a stray click
                                                        // never destroys a CLI-backed session.
                                                    }
                                                    div { class: "thread recent sub", title: "right-click / double-click for options",
                                                        onclick: move |_| { show_board.set(false); open_session_tab(tabs, active_tab, messages, next_tab_id, cfg, ui, engine, busy_tabs, p_open.clone(), t_open.clone()); },
                                                        oncontextmenu: {
                                                            let p = p_dbl.clone();
                                                            move |e: dioxus::prelude::MouseEvent| { e.prevent_default(); e.stop_propagation(); show_theme_menu.set(false); session_menu.set(Some(p.clone())); }
                                                        },
                                                        ondoubleclick: move |_| { let cur = session_menu.read().clone(); session_menu.set(if cur.as_ref() == Some(&p_dbl) { None } else { Some(p_dbl.clone()) }); },
                                                        span { class: "sess-branch", Icon { name: "branch" } }
                                                        if let Some(l) = provider_logo(&sprov) { span { class: "sess-logo prov-logo", dangerous_inner_html: l } }
                                                    span { class: "thread-title", title: "{title}", "{title}" }
                                                        span { class: "thread-time", "{reltime}" }
                                                    }
                                                    if menu_open {
                                                        div { class: "menu-backdrop", onclick: move |_| session_menu.set(None) }
                                                        div { class: "thread-menu",
                                                            button { class: "menu-item", onclick: move |_| {
                                                                archive_session(&p_arch);
                                                                session_menu.set(None);
                                                                sessions_refresh.set(sessions_refresh() + 1);
                                                                refresh_projects_list(projects_list, cfg);
                                                                push_action_toast(
                                                                    toasts,
                                                                    toast_seq,
                                                                    "info",
                                                                    "Session archived",
                                                                    "Restore",
                                                                    ToastAction::RestoreSessions(vec![path_str_menu_archive.clone()]),
                                                                );
                                                            },
                                                                Icon { name: "folder" } span { class: "menu-name", "Archive" }
                                                            }
                                                            button { class: "menu-item danger", onclick: move |_| {
                                                                let restore = capture_deleted_session(&p_del);
                                                                delete_session(&p_del);
                                                                session_menu.set(None);
                                                                sessions_refresh.set(sessions_refresh() + 1);
                                                                refresh_projects_list(projects_list, cfg);
                                                                if let Some(spec) = restore {
                                                                    push_action_toast(
                                                                        toasts,
                                                                        toast_seq,
                                                                        "ok",
                                                                        "Session deleted",
                                                                        "Restore",
                                                                        ToastAction::RestoreDeletedSession(spec),
                                                                    );
                                                                } else {
                                                                    push_toast(toasts, toast_seq, "ok", "Session deleted");
                                                                }
                                                            },
                                                                Icon { name: "trash" } span { class: "menu-name", "Delete" }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                        }
                                    }
                                    if total > PROJECT_SESSION_PAGE_SIZE && !collapsed {
                                        button { class: "show-more", onclick: move |_| {
                                            let mut pages = project_session_pages.write();
                                            let next = if shown >= total {
                                                PROJECT_SESSION_PAGE_SIZE
                                            } else {
                                                (shown + PROJECT_SESSION_PAGE_SIZE).min(total)
                                            };
                                            pages.insert(pname2.clone(), next);
                                        }, "{show_more_label}" }
                                    }
                                  }
                                }
                            }
                        }
                    }
                }
                if let Some((fam, plan, p, s, p_reset, s_reset)) = usage_info.read().clone() {
                    // Only show the quota of the provider that's actually active.
                    if fam == provider_family(&cfg.read().provider) {
                        {
                            let brand = if fam == "claude" { "Claude" } else { "ChatGPT" };
                            let pv = if p_reset.is_empty() { format!("{p}%") } else { format!("{p}% · {p_reset}") };
                            let sv = if s_reset.is_empty() { format!("{s}%") } else { format!("{s}% · {s_reset}") };
                            rsx! {
                                div { class: "usage-chip", title: "{brand} subscription quota",
                                    div { class: "usage-head", "Usage remaining" }
                                    div { class: "usage-row",
                                        span { class: "usage-k", "5h" }
                                        span { class: "usage-bar", span { class: "usage-fill", style: "width:{p}%" } }
                                        span { class: "usage-v", "{pv}" }
                                    }
                                    div { class: "usage-row",
                                        span { class: "usage-k", "wk" }
                                        span { class: "usage-bar", span { class: "usage-fill", style: "width:{s}%" } }
                                        span { class: "usage-v", "{sv}" }
                                    }
                                    div { class: "usage-plan", "{brand} {plan}" }
                                }
                            }
                        }
                    }
                }
                button { class: "settings-btn", onclick: move |_| {
                        settings_initial_tab.set("model".to_string());
                        show_settings.set(true);
                    },
                    Icon { name: "settings" } span { "Settings" }
                }
            }

            // ── Center column ──────────────────────────────────────────
            div { class: "panel-resizer", title: "Drag to resize sidebar",
                onmousedown: move |e: dioxus::prelude::MouseEvent| {
                    e.prevent_default();
                    panel_drag.set(Some((1, e.client_coordinates().x, *sidebar_w.read())));
                },
            }
            main { class: "main",
                if let Some(info) = update_info.read().clone() {
                    {
                    let pct = (*update_pct.read() * 100.0) as u32;
                    rsx! {
                    div { class: "update-banner",
                        span { class: "update-ic", Icon { name: "arrow-up" } }
                        span { class: "update-text",
                            "Update available · v{info.version}"
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
                            button { class: "update-x", onclick: move |_| update_info.set(None), Icon { name: "x" } }
                        }
                    }
                    }
                    }
                }
                if cfg.read().workspace.is_some() {
                    div { class: "agent-tabs",
                        div { class: "agent-tabs-scroll",
                        for (i, t) in tabs.read().iter().enumerate() {
                            {
                                let id = t.id;
                                let title = t.title.clone();
                                let logo = provider_logo(&t.provider);
                                let tab_status = tab_statuses.read().get(&id).cloned();
                                let tab_status_class_name = tab_status.as_ref().map(tab_status_class).unwrap_or("");
                                let tab_status_label_text = tab_status.as_ref().map(tab_status_label).unwrap_or("");
                                let is_active = i == *active_tab.read();
                                let many = tabs.read().len() > 1;
                                let tab_class = if *closing_tab.read() == Some(id) {
                                    "agent-tab closing"
                                } else if is_active {
                                    "agent-tab active"
                                } else {
                                    "agent-tab"
                                };
                                rsx! {
                                    div { key: "{id}", class: "{tab_class}",
                                        onclick: move |_| switch_tab(tabs, active_tab, messages, cfg, engine, i),
                                        if let Some(l) = logo { span { class: "agent-tab-logo prov-logo", dangerous_inner_html: l } }
                                        span { class: "agent-tab-title", "{title}" }
                                        if tab_status.is_some() {
                                            span {
                                                class: "tab-state {tab_status_class_name}",
                                                title: "{tab_status_label_text}",
                                                "{tab_status_label_text}"
                                            }
                                        }
                                        if many {
                                            button { class: "agent-tab-x", onclick: move |e| {
                                                e.stop_propagation();
                                                closing_tab.set(Some(id));
                                                spawn(async move {
                                                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                                                    let idx = tabs.read().iter().position(|t| t.id == id);
                                                    if let Some(idx) = idx { close_tab(tabs, active_tab, messages, cfg, engine, idx); }
                                                    closing_tab.set(None);
                                                });
                                            }, Icon { name: "x" } }
                                        }
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
                                    div { class: "menu-label", "New agent · Cmd-click for TUI" }
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
                        {
                            let open_tabs = tabs.read().len();
                            rsx! {
                                if open_tabs > 1 {
                                    span { class: "agent-tab-count", title: "Open agent tabs", "{open_tabs} open" }
                                }
                            }
                        }
                        div { class: "tab-actions",
                            button { class: "top-btn", onclick: move |_| open_folder(cfg, ui, engine),
                                Icon { name: "folder" } "Open folder"
                            }
                            button { class: if *show_env.read() && env_tab.read().as_str() == "files" && inspector_tab.read().as_str() == "agents" { "top-btn on" } else { "top-btn" },
                                onclick: move |_| {
                                    inspector_tab.set("agents".to_string());
                                    select_env_tab(env_tab, show_env, env_tab_by_tab, tabs, active_tab, "files", false);
                                }, Icon { name: "spark" } "Agents"
                            }
                            button { class: if *show_env.read() && env_tab.read().as_str() == "files" && inspector_tab.read().as_str() != "agents" { "top-btn on" } else { "top-btn" },
                                onclick: move |_| {
                                    select_env_tab(env_tab, show_env, env_tab_by_tab, tabs, active_tab, "files", true);
                                }, Icon { name: "plugins" } "Files"
                            }
                            button { class: if *show_env.read() && env_tab.read().as_str() == "term" { "top-btn on" } else { "top-btn" },
                                onclick: move |_| {
                                    select_env_tab(env_tab, show_env, env_tab_by_tab, tabs, active_tab, "term", true);
                                }, Icon { name: "terminal" } "Terminal"
                            }
                            button { class: if *show_split.read() { "top-btn on" } else { "top-btn" },
                                onclick: move |_| { let v = *show_split.read(); show_split.set(!v); }, Icon { name: "plugins" } "Split"
                            }
                            button { class: if *show_env.read() && env_tab.read().as_str() == "preview" { "top-btn on" } else { "top-btn" },
                                onclick: move |_| {
                                    let was_open = *show_env.read() && env_tab.read().as_str() == "preview";
                                    select_env_tab(env_tab, show_env, env_tab_by_tab, tabs, active_tab, "preview", true);
                                    if !was_open {
                                        spawn(async move { preview_ports.set(scan_ports().await); });
                                    }
                                }, Icon { name: "browser" } "Preview"
                            }
                            button { class: if *show_env.read() && env_tab.read().as_str() == "changes" { "top-btn on" } else { "top-btn" },
                                onclick: move |_| {
                                    let was_open = *show_env.read() && env_tab.read().as_str() == "changes";
                                    select_env_tab(env_tab, show_env, env_tab_by_tab, tabs, active_tab, "changes", true);
                                    if !was_open {
                                        let ws = ws_changes.clone();
                                        spawn(async move { changed_files.set(load_changed_files(&ws).await); });
                                    }
                                }, Icon { name: "branch" } "Changes"
                            }
                        }
                    }
                }

                div { class: if *show_env.read() { "center with-preview" } else { "center" },
                    style: if *show_env.read() { format!("--rpanel:{}px", *rpanel_w.read()) } else { String::new() },
                    if cfg.read().workspace.is_some() && !*show_env.read() && !active_is_tui {
                        {
                            let n_changed = changed_files.read().len();
                            let ta: u32 = changed_files.read().iter().map(|f| f.1).sum();
                            let td: u32 = changed_files.read().iter().map(|f| f.2).sum();
                            let n_terms = terms.read().len();
                            let br = branch.clone();
                            rsx! {
                                div { class: "env-card",
                                    div { class: "env-card-head",
                                        span { "Environment" }
                                        button { class: "env-card-gear", title: "Open environment", onclick: move |_| select_env_tab(env_tab, show_env, env_tab_by_tab, tabs, active_tab, "files", false), Icon { name: "settings" } }
                                    }
                                    button { class: "env-card-row", onclick: move |_| select_env_tab(env_tab, show_env, env_tab_by_tab, tabs, active_tab, "changes", false),
                                        Icon { name: "branch" } span { "Changes" }
                                        if n_changed > 0 {
                                            span { class: "env-card-badge",
                                                "{n_changed} · "
                                                span { class: "diff-adds", "+{ta}" }
                                                " "
                                                span { class: "diff-dels", "−{td}" }
                                            }
                                        }
                                    }
                                    {
                                        let ws_now = ui.workspace.read().clone();
                                        let in_wt = ws_now.to_string_lossy().contains("/.oxide/worktrees/env");
                                        let mode_label = if in_wt { "Worktree" } else { "Local" };
                                        rsx! {
                                            button { class: "env-card-row", title: "Switch between the project folder and an isolated git worktree",
                                                onclick: move |_| {
                                                    let ws_now = ui.workspace.peek().clone();
                                                    let in_wt = ws_now.to_string_lossy().contains("/.oxide/worktrees/env");
                                                    if in_wt {
                                                        // Back to the real project root.
                                                        let root = std::path::PathBuf::from(ws_now.to_string_lossy().split("/.oxide/worktrees/env").next().unwrap_or_default());
                                                        if root.exists() { apply_workspace(cfg, ui, engine, root); push_toast(toasts, toast_seq, "ok", "Back to local project"); }
                                                    } else {
                                                        let wt = ws_now.join(".oxide/worktrees/env");
                                                        spawn(async move {
                                                            if !wt.exists() {
                                                                let r = run_cmd(&ws_now, "git", &["worktree", "add", &wt.display().to_string(), "-b", "oxide/env"]).await;
                                                                if r.contains("fatal") && !r.contains("already exists") {
                                                                    // branch may exist from before — attach without -b
                                                                    let _ = run_cmd(&ws_now, "git", &["worktree", "add", &wt.display().to_string(), "oxide/env"]).await;
                                                                }
                                                            }
                                                            if wt.exists() {
                                                                apply_workspace(cfg, ui, engine, wt);
                                                                push_toast(toasts, toast_seq, "ok", "Switched to worktree (branch oxide/env)");
                                                            } else {
                                                                push_toast(toasts, toast_seq, "err", "Worktree create failed");
                                                            }
                                                        });
                                                    }
                                                },
                                                Icon { name: "terminal" } span { "{mode_label}" } span { class: "env-card-badge", Icon { name: "chevron" } }
                                            }
                                        }
                                    }
                                    div { class: "env-card-anchor",
                                        button { class: "env-card-row", onclick: move |_| {
                                                let v = *branch_menu.read(); branch_menu.set(!v);
                                                if !v {
                                                    let ws = ui.workspace.peek().clone();
                                                    spawn(async move {
                                                        let out = run_cmd(&ws, "git", &["branch", "--format=%(refname:short)"]).await;
                                                        branches_list.set(out.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect());
                                                    });
                                                }
                                            },
                                            Icon { name: "branch" } span { "{br}" } span { class: "env-card-badge", Icon { name: "chevron" } }
                                        }
                                        if *branch_menu.read() {
                                            div { class: "env-procs",
                                                for b in branches_list.read().iter().cloned() {
                                                    {
                                                        let cur_b = br.clone();
                                                        rsx! {
                                                            button { class: "env-proc as-btn", onclick: move |_| {
                                                                    branch_menu.set(false);
                                                                    let ws = ui.workspace.peek().clone();
                                                                    let b2 = b.clone();
                                                                    spawn(async move {
                                                                        let out = run_cmd(&ws, "git", &["checkout", &b2]).await;
                                                                        let ok = !out.contains("error") && !out.contains("fatal");
                                                                        let msg = if ok { format!("Switched to {b2}") } else { "Checkout failed".to_string() };
                                                                        push_toast(toasts, toast_seq, if ok { "ok" } else { "err" }, &msg);
                                                                        changed_files.set(load_changed_files(&ws).await);
                                                                    });
                                                                },
                                                                span { class: "env-proc-name", "{b}" }
                                                                if b == cur_b { span { class: "env-card-badge", Icon { name: "check" } } }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    div { class: "env-card-anchor",
                                        button { class: "env-card-row", onclick: move |_| { let v = *git_menu.read(); git_menu.set(!v); },
                                            Icon { name: "spark" } span { "Commit or push" } span { class: "env-card-badge", Icon { name: "chevron" } }
                                        }
                                        if *git_menu.read() {
                                            div { class: "env-procs",
                                                button { class: "env-proc as-btn", onclick: move |_| {
                                                        git_menu.set(false);
                                                        let ws = ui.workspace.peek().clone();
                                                        spawn(async move {
                                                            let _ = run_cmd(&ws, "git", &["add", "-A"]).await;
                                                            let r = run_cmd(&ws, "git", &["commit", "-m", "wip: changes from Oxide"]).await;
                                                            let ok = !r.contains("error") && !r.contains("fatal");
                                                            push_toast(toasts, toast_seq, if ok { "ok" } else { "err" }, if ok { "Committed" } else { "Commit failed" });
                                                            changed_files.set(load_changed_files(&ws).await);
                                                        });
                                                    }, span { class: "env-proc-name", "Commit all" } }
                                                button { class: "env-proc as-btn", onclick: move |_| {
                                                        git_menu.set(false);
                                                        let ws = ui.workspace.peek().clone();
                                                        spawn(async move {
                                                            let r = run_cmd(&ws, "git", &["push"]).await;
                                                            let ok = !r.contains("error") && !r.contains("fatal") && !r.contains("rejected");
                                                            push_toast(toasts, toast_seq, if ok { "ok" } else { "err" }, if ok { "Pushed" } else { "Push failed" });
                                                        });
                                                    }, span { class: "env-proc-name", "Push" } }
                                                button { class: "env-proc as-btn", onclick: move |_| {
                                                        git_menu.set(false);
                                                        let ws = ui.workspace.peek().clone();
                                                        spawn(async move {
                                                            let r = run_cmd(&ws, "git", &["pull", "--ff-only"]).await;
                                                            let ok = !r.contains("error") && !r.contains("fatal");
                                                            push_toast(toasts, toast_seq, if ok { "ok" } else { "err" }, if ok { "Pulled" } else { "Pull failed" });
                                                        });
                                                    }, span { class: "env-proc-name", "Pull (ff-only)" } }
                                                button { class: "env-proc-open", onclick: move |_| { git_menu.set(false); select_env_tab(env_tab, show_env, env_tab_by_tab, tabs, active_tab, "changes", false); },
                                                    "Open diffs / PR" Icon { name: "corner-up-right" }
                                                }
                                            }
                                        }
                                    }
                                    if let Some((fam, plan, pct5, pctw, _, _)) = usage_info.read().clone() {
                                        if fam == provider_family(&cfg.read().provider) {
                                            div { class: "env-card-row static usage", title: "Plan: {plan} — sisa kuota 5 jam / mingguan",
                                                Icon { name: "spark" } span { "Usage" }
                                                span { class: "env-card-badge nowrap", "5h {pct5}% · wk {pctw}%" }
                                            }
                                        }
                                    }
                                    button { class: "env-card-row", onclick: move |_| {
                                            let ws = ui.workspace.peek().clone();
                                            spawn(async move {
                                                let url = run_cmd(&ws, "git", &["remote", "get-url", "origin"]).await;
                                                let url = url.trim().to_string();
                                                if url.is_empty() { return; }
                                                let https = if let Some(rest) = url.strip_prefix("git@") {
                                                    format!("https://{}", rest.replacen(':', "/", 1)).trim_end_matches(".git").to_string()
                                                } else {
                                                    url.trim_end_matches(".git").to_string()
                                                };
                                                let _ = tokio::process::Command::new("open").arg(&https).output().await;
                                            });
                                        },
                                        Icon { name: "browser" } span { "Repository" } span { class: "env-card-badge", Icon { name: "external-link" } }
                                    }
                                    div { class: "env-card-sep" }
                                    div { class: "env-card-label", "Sources" }
                                    button { class: "env-card-row", onclick: move |_| {
                                            inspector_tab.set("agents".to_string());
                                            select_env_tab(env_tab, show_env, env_tab_by_tab, tabs, active_tab, "files", false);
                                        },
                                        Icon { name: "spark" } span { "Agents" }
                                        span { class: "env-card-badge", "{busy_tabs.read().len()} running" }
                                    }
                                    button { class: "env-card-row", onclick: move |_| select_env_tab(env_tab, show_env, env_tab_by_tab, tabs, active_tab, "term", false),
                                        Icon { name: "terminal" } span { "Terminals" } span { class: "env-card-badge", "{n_terms}" }
                                    }
                                    div { class: "env-card-section-head",
                                        span { class: "env-card-label inline", "Local Servers" }
                                        span { class: "env-card-badge", "{procs_list.read().len()}" }
                                        button { class: "env-card-mini", title: "Refresh local servers",
                                            onclick: move |e: dioxus::prelude::MouseEvent| {
                                                e.stop_propagation();
                                                spawn(async move {
                                                    procs_list.set(scan_procs().await);
                                                    preview_ports.set(scan_ports().await);
                                                });
                                            },
                                            Icon { name: "refresh" }
                                        }
                                    }
                                    div { class: "local-server-list",
                                        if procs_list.read().is_empty() {
                                            div { class: "local-server-empty", "No local servers running" }
                                        }
                                        for (port, name, pid) in procs_list.read().iter().cloned() {
                                            div { class: "local-server-row",
                                                button { class: "local-server-main", title: "Open localhost:{port} in Preview ({name})",
                                                    onclick: move |_| {
                                                        select_env_tab(env_tab, show_env, env_tab_by_tab, tabs, active_tab, "preview", false);
                                                        spawn(async move {
                                                            preview_proxy::set_target(port);
                                                            let pp = preview_proxy::ensure_proxy().await;
                                                            if pp != 0 { preview_url.set(format!("http://127.0.0.1:{pp}/")); }
                                                            else { preview_url.set(format!("http://localhost:{port}")); }
                                                        });
                                                    },
                                                    span { class: "server-dot" }
                                                    span { class: "local-server-copy",
                                                        span { class: "local-server-title", "{name}" }
                                                        span { class: "local-server-meta", "localhost:{port}" }
                                                    }
                                                    span { class: "local-server-port", ":{port}" }
                                                }
                                                button { class: "local-server-icon", title: "Open dev server",
                                                    onclick: move |_| {
                                                        let url = format!("http://localhost:{port}");
                                                        spawn(async move { let _ = tokio::process::Command::new("open").arg(url).output().await; });
                                                    },
                                                    Icon { name: "external-link" }
                                                }
                                                button { class: "local-server-stop", title: "Stop dev server",
                                                    onclick: move |_| {
                                                        spawn(async move {
                                                            let _ = tokio::process::Command::new("kill").arg("-9").arg(pid.to_string()).output().await;
                                                            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                                                            procs_list.set(scan_procs().await);
                                                            preview_ports.set(scan_ports().await);
                                                        });
                                                    },
                                                    Icon { name: "stop" }
                                                }
                                            }
                                        }
                                    }
                                    button { class: "env-card-row", onclick: move |_| { select_env_tab(env_tab, show_env, env_tab_by_tab, tabs, active_tab, "preview", false); spawn(async move { preview_ports.set(scan_ports().await); }); },
                                        Icon { name: "browser" } span { "Preview" }
                                    }
                                    button { class: "env-card-row", onclick: move |_| select_env_tab(env_tab, show_env, env_tab_by_tab, tabs, active_tab, "files", false),
                                        Icon { name: "plugins" } span { "Files" }
                                    }
                                    if !pinned_msgs.read().is_empty() {
                                        div { class: "env-card-sep" }
                                        button { class: "env-card-row", onclick: move |_| { let v = *pins_open.read(); pins_open.set(!v); },
                                            Icon { name: "pin" } span { "Pinned" } span { class: "env-card-badge", "{pinned_msgs.read().len()}" }
                                        }
                                        if *pins_open.read() {
                                            for (pi, (mi, snip, done)) in pinned_msgs.read().iter().cloned().enumerate() {
                                                div { class: if done { "env-pin done" } else { "env-pin" },
                                                    input { r#type: "checkbox", checked: done,
                                                        onchange: move |_| {
                                                            if let Some(p) = pinned_msgs.write().get_mut(pi) { p.2 = !p.2; }
                                                            thread_json_save(&ui.workspace.peek().clone(), "pins", &thread_stem(&tabs, &active_tab), &*pinned_msgs.read());
                                                        } }
                                                    span { class: "env-pin-text", onclick: move |_| jump_to_msg(mi), "{snip}" }
                                                    button { class: "env-proc-kill", title: "Unpin", onclick: move |_| {
                                                            pinned_msgs.write().retain(|p| p.0 != mi);
                                                            thread_json_save(&ui.workspace.peek().clone(), "pins", &thread_stem(&tabs, &active_tab), &*pinned_msgs.read());
                                                        }, Icon { name: "x" } }
                                                }
                                            }
                                        }
                                    }
                                    if !markers.read().is_empty() {
                                        div { class: "env-card-sep" }
                                        button { class: "env-card-row", onclick: move |_| { let v = *marks_open.read(); marks_open.set(!v); },
                                            span { class: "mark-swatch c0" } span { "Markers" } span { class: "env-card-badge", "{markers.read().len()}" }
                                        }
                                        if *marks_open.read() {
                                            for (ki, (mi, text, color, done)) in markers.read().iter().cloned().enumerate() {
                                                div { class: if done { "env-pin done" } else { "env-pin" },
                                                    span { class: "mark-swatch c{color}", title: "Cycle color",
                                                        onclick: move |_| {
                                                            if let Some(m) = markers.write().get_mut(ki) { m.2 = (m.2 + 1) % 4; }
                                                            thread_json_save(&ui.workspace.peek().clone(), "markers", &thread_stem(&tabs, &active_tab), &*markers.read());
                                                        } }
                                                    span { class: "env-pin-text", onclick: move |_| jump_to_msg(mi), "{text}" }
                                                    button { class: "env-proc-kill", title: "Remove", onclick: move |_| {
                                                            let mut mv = markers.write();
                                                            if ki < mv.len() { mv.remove(ki); }
                                                            drop(mv);
                                                            thread_json_save(&ui.workspace.peek().clone(), "markers", &thread_stem(&tabs, &active_tab), &*markers.read());
                                                        }, Icon { name: "x" } }
                                                }
                                            }
                                        }
                                    }
                                    if !recap_text.read().is_empty() {
                                        div { class: "env-card-sep" }
                                        button { class: "env-card-row", onclick: move |_| { let v = *recap_open.read(); recap_open.set(!v); },
                                            Icon { name: "brain" } span { "Recap" } span { class: if *recap_open.read() { "env-card-badge open" } else { "env-card-badge" }, Icon { name: "chevron" } }
                                        }
                                        if *recap_open.read() {
                                            div { class: "env-note recap", "{recap_text}" }
                                        }
                                    }
                                    div { class: "env-card-sep" }
                                    button { class: "env-card-row", onclick: move |_| { let v = *note_open.read(); note_open.set(!v); },
                                        Icon { name: "file" } span { "Notepad" } span { class: "env-card-badge", Icon { name: "chevron" } }
                                    }
                                    if *note_open.read() {
                                        textarea { class: "env-note-input", placeholder: "Notes for this thread…", value: "{note_text}",
                                            oninput: move |e| {
                                                let v = e.value();
                                                note_text.set(v.clone());
                                                // Autosave per thread under .oxide/notes/<session>.md
                                                let ws = ui.workspace.peek().clone();
                                                let cur = *active_tab.peek();
                                                let stem = tabs.peek().get(cur)
                                                    .and_then(|t| t.session.as_ref().and_then(|p| p.file_stem().map(|x| x.to_string_lossy().to_string())))
                                                    .unwrap_or_else(|| "default".into());
                                                let dir = ws.join(".oxide/notes");
                                                let _ = std::fs::create_dir_all(&dir);
                                                let _ = std::fs::write(dir.join(format!("{stem}.md")), v.chars().take(20_000).collect::<String>());
                                            },
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if *show_env.read() {
                        div { class: "env-panel",
                            div { class: "panel-resizer rp", onmousedown: move |e: dioxus::prelude::MouseEvent| {
                                e.prevent_default();
                                panel_drag.set(Some((3, e.client_coordinates().x, *rpanel_w.read())));
                            } }
                            div { class: "env-tabs",
                                for (tid, label, ic) in [("files", "Files", "plugins"), ("term", "Terminals", "terminal"), ("preview", "Preview", "spark"), ("changes", "Diffs", "branch")] {
                                    button { class: if env_tab.read().as_str() == tid { "env-tab on" } else { "env-tab" },
                                        onclick: move |_| select_env_tab(env_tab, show_env, env_tab_by_tab, tabs, active_tab, tid, false),
                                        Icon { name: ic } span { "{label}" }
                                    }
                                }
                                button { class: "env-x", title: "Close", onclick: move |_| show_env.set(false), Icon { name: "x" } }
                            }
                            div { class: "env-body",
                            if env_tab.read().as_str() == "changes" {
                                {
                                    let files = changed_files.read().clone();
                                    let n = files.len();
                                    let ta: u32 = files.iter().map(|f| f.1).sum();
                                    let td: u32 = files.iter().map(|f| f.2).sum();
                                    let ws_cp = workspace.clone();
                                    let ws_pr2 = workspace.clone();
                                    rsx! {
                                        div { class: "changes-panel",
                                            div { class: "changes-head",
                                                span { class: "changes-branch", Icon { name: "branch" } "{branch}" }
                                                span { class: "changes-stats", "{n} files " span { class: "diff-adds countup plus", style: "--n:{ta}" } " " span { class: "diff-dels countup minus", style: "--n:{td}" } }
                                                button { class: "git-act", onclick: move |_| {
                                                    let ws = ws_cp.clone();
                                                    spawn(async move {
                                                        let _ = run_cmd(&ws, "git", &["add", "-A"]).await;
                                                        let r = run_cmd(&ws, "git", &["commit", "-m", "wip: changes from Oxide"]).await;
                                                        let ok = !r.contains("error") && !r.contains("fatal");
                                                        push_toast(toasts, toast_seq, if ok { "ok" } else { "err" }, if ok { "Changes committed" } else { "Commit failed" });
                                                        changed_files.set(load_changed_files(&ws).await);
                                                    });
                                                }, "Commit" }
                                                button { class: "git-act", onclick: move |_| {
                                                    let ws = ws_pr2.clone();
                                                    spawn(async move {
                                                        let b = git_branch(&ws);
                                                        let _ = run_cmd(&ws, "git", &["push", "-u", "origin", &b]).await;
                                                        let _ = run_cmd(&ws, "gh", &["pr", "create", "--fill"]).await;
                                                    });
                                                },
                                                    if let Some(l) = provider_logo("github") { span { class: "git-act-logo prov-logo", dangerous_inner_html: l } }
                                                    "Create PR"
                                                }
                                                button { class: "term-x", onclick: move |_| show_env.set(false), Icon { name: "x" } }
                                            }
                                            div { class: "changes-list",
                                                if files.is_empty() { div { class: "insp-empty", "Working tree clean." } }
                                                for (path, a, d, diff) in files {
                                                    details { class: "changes-file",
                                                        summary { class: "changes-file-head",
                                                            span { class: "edits-caret", Icon { name: "chevron" } }
                                                            span { class: "changes-path", "{path}" }
                                                            span { class: "diff-adds", "+{a}" } span { class: "diff-dels", "−{d}" }
                                                        }
                                                        HunkedDiff { ws: workspace.clone(), path: path.clone(), diff }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            if env_tab.read().as_str() == "preview" {
                                div { class: "preview-panel",
                                    div { class: "preview-bar",
                                        input { class: "preview-addr", placeholder: "http://localhost:3000", value: "{preview_url}",
                                            oninput: move |e| preview_url.set(e.value()),
                                            onkeydown: move |e| if e.key() == Key::Enter {
                                                let mut u = preview_url.read().clone();
                                                if !u.is_empty() && !u.contains("://") { u = format!("http://{u}"); preview_url.set(u); }
                                            }
                                        }
                                        button { class: "preview-btn", title: "Rescan localhost ports", onclick: move |_| { spawn(async move { preview_ports.set(scan_ports().await); }); },
                                            Icon { name: "refresh" } "Scan"
                                        }
                                        button { class: "preview-btn pick", title: "Select an element to send to the composer", onclick: move |_| {
                                            spawn(async move { let _ = document::eval("document.querySelector('.preview-frame')?.contentWindow?.postMessage('oxide-pick-on','*')").await; });
                                        }, "Pick" }
                                        button { class: if *design_mode.read() { "preview-btn pick on" } else { "preview-btn" }, title: "Design Mode — click an element, edit it live, Apply writes the code", onclick: move |_| {
                                            let v = *design_mode.read();
                                            design_mode.set(!v);
                                            if v { design_sel.set(None); design_edits.set(Vec::new()); design_note.set(String::new()); }
                                            let msg = if v { "'oxide-design-off'" } else { "'oxide-design-on'" };
                                            let js = format!("document.querySelector('.preview-frame')?.contentWindow?.postMessage({msg},'*')");
                                            spawn(async move { let _ = document::eval(&js).await; });
                                        }, "Design" }
                                        button { class: "preview-btn", title: "Reload", onclick: move |_| { let u = preview_url.read().clone(); preview_url.set(String::new()); preview_url.set(u); }, "Reload" }
                                        button { class: "preview-btn", title: "Open in system browser", onclick: move |_| { let u = preview_url.read().clone(); if !u.is_empty() { let _ = std::process::Command::new("open").arg(u).spawn(); } },
                                            Icon { name: "external-link" }
                                        }
                                        button { class: "term-x", onclick: move |_| show_env.set(false), Icon { name: "x" } }
                                    }
                                    div { class: "preview-ports",
                                        if preview_ports.read().is_empty() {
                                            span { class: "preview-hint", "No localhost servers detected. Start a dev server, then scan again." }
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
                                    if *design_mode.read() {
                                        if let Some(sel) = design_sel.read().clone() {
                                            {
                                                let selection = design_selection_from_value(&sel);
                                                let selector = selection.selector.clone();
                                                let source = selection.source.clone();
                                                let component = selection.component.clone();
                                                let tag = sel["tag"].as_str().unwrap_or("").to_string();
                                                let cur_text = selection.text.clone();
                                                let styles = sel["styles"].clone();
                                                let pending = design_edits.read().clone();
                                                let pending_count = pending.len();
                                                let note_value = design_note.read().clone();
                                                let selection_review = selection.clone();
                                                let selection_apply = selection.clone();
                                                let selector_review = selector.clone();
                                                let selector_apply = selector.clone();
                                                let props = ["color", "background", "fontSize", "fontWeight", "padding", "margin", "borderRadius"];
                                                rsx! {
                                                    div { class: "design-panel",
                                                        div { class: "design-head",
                                                            span { class: "design-selector", "{selector}" }
                                                            if !component.is_empty() { span { class: "design-comp", "<{component}>" } }
                                                            if !tag.is_empty() { span { class: "design-comp", "{tag}" } }
                                                        }
                                                        div { class: "design-summary",
                                                            if !source.is_empty() { span { "source: {source}" } }
                                                            if !cur_text.is_empty() { span { "text: {cur_text}" } }
                                                        }
                                                        div { class: "design-row",
                                                            span { class: "design-lbl", "text" }
                                                            input { class: "design-input", value: "{cur_text}",
                                                                onchange: move |e| {
                                                                    let t = e.value();
                                                                    upsert_design_edit(&mut design_edits.write(), "text".into(), cur_text.clone(), t.clone());
                                                                    let js = format!("document.querySelector('.preview-frame')?.contentWindow?.postMessage({{type:'oxide-text-set',text:{}}},'*')", serde_json::to_string(&t).unwrap_or_default());
                                                                    spawn(async move { let _ = document::eval(&js).await; });
                                                                } }
                                                        }
                                                        for prop in props {
                                                            {
                                                                let cssname = match prop { "fontSize" => "font-size", "fontWeight" => "font-weight", "borderRadius" => "border-radius", p => p };
                                                                let cur = styles[prop].as_str().unwrap_or("").to_string();
                                                                rsx! {
                                                                    div { class: "design-row",
                                                                        span { class: "design-lbl", "{cssname}" }
                                                                        input { class: "design-input", value: "{cur}",
                                                                            onchange: move |e| {
                                                                                let val = e.value();
                                                                                upsert_design_edit(&mut design_edits.write(), cssname.to_string(), cur.clone(), val.clone());
                                                                                let js = format!("document.querySelector('.preview-frame')?.contentWindow?.postMessage({{type:'oxide-style-set',prop:'{cssname}',value:{}}},'*')", serde_json::to_string(&val).unwrap_or_default());
                                                                                spawn(async move { let _ = document::eval(&js).await; });
                                                                            } }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                        if pending_count > 0 {
                                                            div { class: "design-pending",
                                                                span { class: "design-pending-title", "{pending_count} pending edit(s)" }
                                                                for (prop, old, newv) in pending.iter().cloned() {
                                                                    div { class: "design-pending-row",
                                                                        span { class: "design-pending-prop", "{prop}" }
                                                                        span { class: "design-pending-val",
                                                                            span { "{old}" }
                                                                            Icon { name: "arrow-right" }
                                                                            span { "{newv}" }
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                        textarea { class: "design-note", placeholder: "Visual review note…", value: "{note_value}",
                                                            oninput: move |e| design_note.set(e.value())
                                                        }
                                                        div { class: "design-actions",
                                                            button { class: "preview-btn", onclick: move |_| {
                                                                let edits = design_edit_values(&design_edits.read());
                                                                let note = design_note.read().clone();
                                                                let prompt = format!(
                                                                    "Review this selected UI element before code changes. Use Design Workbench standards: token fit, contrast/accessibility, layout overflow, motion discipline, and source-code implementation risk. Do not edit files unless you find a concrete fix is needed.\n\n{}",
                                                                    build_design_apply_prompt(&selection_review, &edits, &note)
                                                                );
                                                                engine.send(EngineCmd::Submit { engine: prompt, display: format!("Review design edits · {selector_review}") });
                                                            }, "Review" }
                                                            button { class: "git-act", onclick: move |_| {
                                                                let edits = design_edit_values(&design_edits.read());
                                                                if edits.is_empty() { return; }
                                                                let prompt = build_design_apply_prompt(&selection_apply, &edits, &design_note.read());
                                                                engine.send(EngineCmd::Submit { engine: prompt, display: format!("Apply design edits · {selector_apply}") });
                                                                design_edits.set(Vec::new());
                                                                design_note.set(String::new());
                                                                spawn(async move { let _ = document::eval("document.querySelector('.preview-frame')?.contentWindow?.postMessage('oxide-design-reset','*')").await; });
                                                        }, Icon { name: "edit" } "Apply to code" }
                                                            button { class: "preview-btn", onclick: move |_| {
                                                                design_edits.set(Vec::new());
                                                                design_note.set(String::new());
                                                                spawn(async move { let _ = document::eval("document.querySelector('.preview-frame')?.contentWindow?.postMessage('oxide-design-reset','*')").await; });
                                                            }, "Reset" }
                                                        }
                                                    }
                                                }
                                            }
                                        } else {
                                            div { class: "design-hint", Icon { name: "edit" } span { "Design Mode aktif — klik elemen di preview untuk mengedit." } }
                                        }
                                    }
                                    if preview_url.read().is_empty() {
                                        div { class: "preview-empty", "Pick a detected server above, or type a URL. Build + run + see it without leaving Oxide." }
                                    } else {
                                        iframe { class: "preview-frame", src: "{preview_url}" }
                                    }
                                }
                            }
                    if env_tab.read().as_str() == "files" {
                        div { class: "panel-resizer", title: "Drag to resize inspector",
                            onmousedown: move |e: dioxus::prelude::MouseEvent| {
                                e.prevent_default();
                                panel_drag.set(Some((2, e.client_coordinates().x, *insp_w.read())));
                            },
                        }
                        aside { class: "files-panel",
                            div { class: "insp-tabs",
                                for (key, label) in [("agents","Agents"),("review","Review"),("files","Files"),("timeline","Timeline"),("sessions","Sessions"),("git","Git"),("memory","Memory"),("goal","Goal"),("browser","Browser"),("approvals","Approvals"),("checkpoints","Checkpoints"),("usage","Usage")] {
                                    {
                                        let active = *inspector_tab.read() == key;
                                        let badge = match key {
                                            "agents" => busy_tabs.read().len() + subagent_cards.read().iter().filter(|c| c.running).count(),
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
                                button { class: "term-x", onclick: move |_| show_env.set(false), Icon { name: "x" } }
                            }
                            div { class: "insp-body",
                                match inspector_tab.read().as_str() {
                                    "agents" => rsx! {
                                        {
                                            let tab_rows = tabs.read().clone();
                                            let active_idx = *active_tab.read();
                                            let running_tabs = busy_tabs.read().len();
                                            let running_subagents = subagent_cards.read().iter().filter(|c| c.running).count();
                                            let review_count = turn_edits.read().len();
                                            let artifact_count = messages.read().iter().filter(|m| m.author == Author::UiSpec).count();
                                            let bg_count = bg_jobs.read().len();
                                            let split_label = if *show_split.read() { "Split on" } else { "Split" };
                                            let changes_workspace = workspace.clone();
                                            let review_workspace = workspace.clone();
                                            let hermes_workspace = workspace.clone();
                                            rsx! {
                                                div { class: "agents-window",
                                                    div { class: "agents-hero",
                                                        div {
                                                            div { class: "agents-kicker", "Local workspace" }
                                                            div { class: "agents-title", "Agents" }
                                                            div { class: "agents-sub", "Local agent sessions, sub-agents, review queue, browser context, and artifacts in one control surface." }
                                                        }
                                                        div { class: "agents-hero-actions",
                                                            button { class: "agent-action primary", onclick: move |_| {
                                                                new_agent_tab(tabs, active_tab, messages, cfg, engine, next_tab_id, "codex", "", "Codex");
                                                            }, Icon { name: "plus" } span { "New Codex" } }
                                                            button { class: if *show_split.read() { "agent-action on" } else { "agent-action" }, onclick: move |_| {
                                                                let v = *show_split.read();
                                                                show_split.set(!v);
                                                            }, Icon { name: "plugins" } span { "{split_label}" } }
                                                        }
                                                    }
                                                    div { class: "agents-metrics",
                                                        div { class: "agents-metric", span { class: "agents-metric-num", "{tab_rows.len()}" } span { class: "agents-metric-label", "open agents" } }
                                                        div { class: "agents-metric live", span { class: "agents-metric-num", "{running_tabs}" } span { class: "agents-metric-label", "running turns" } }
                                                        div { class: "agents-metric", span { class: "agents-metric-num", "{running_subagents}" } span { class: "agents-metric-label", "sub-agents" } }
                                                        div { class: "agents-metric", span { class: "agents-metric-num", "{review_count}" } span { class: "agents-metric-label", "review files" } }
                                                    }
                                                    div { class: "agents-section",
                                                        div { class: "agents-section-head",
                                                            span { "Agent sessions" }
                                                            span { class: "agents-section-meta", "local" }
                                                        }
                                                        div { class: "agents-session-list",
                                                            for (idx, tab) in tab_rows.iter().cloned().enumerate() {
                                                                {
                                                                    let is_active = idx == active_idx;
                                                                    let is_running = busy_tabs.read().contains(&tab.id);
                                                                    let status = tab_statuses.read().get(&tab.id).cloned();
                                                                    let status_text = match status {
                                                                        Some(TabStatus::WaitingApproval) => "approval",
                                                                        Some(TabStatus::WaitingInput) => "input",
                                                                        Some(TabStatus::Failed) => "failed",
                                                                        Some(TabStatus::Running) => "running",
                                                                        None if is_running => "running",
                                                                        None => "idle",
                                                                    };
                                                                    let row_cls = if is_active {
                                                                        "agents-session active"
                                                                    } else if status_text == "failed" {
                                                                        "agents-session failed"
                                                                    } else if is_running {
                                                                        "agents-session running"
                                                                    } else {
                                                                        "agents-session"
                                                                    };
                                                                    let message_count = if is_active { messages.read().len() } else { tab.messages.len() };
                                                                    let artifact_count = if is_active {
                                                                        messages.read().iter().filter(|m| m.author == Author::UiSpec).count()
                                                                    } else {
                                                                        tab.messages.iter().filter(|m| m.author == Author::UiSpec).count()
                                                                    };
                                                                    let status_class = format!("agents-status {status_text}");
                                                                    rsx! {
                                                                        button { class: "{row_cls}", onclick: move |_| {
                                                                            switch_tab(tabs, active_tab, messages, cfg, engine, idx);
                                                                        },
                                                                            span { class: "agents-session-logo",
                                                                                if let Some(l) = provider_logo(&tab.provider) {
                                                                                    span { class: "prov-logo", dangerous_inner_html: l }
                                                                                } else {
                                                                                    Icon { name: "spark" }
                                                                                }
                                                                            }
                                                                            span { class: "agents-session-copy",
                                                                                span { class: "agents-session-title", "{tab.title}" }
                                                                                span { class: "agents-session-sub", "{tab.provider} · {tab.harness} · {tab.reasoning_effort}" }
                                                                            }
                                                                            span { class: "agents-session-meta",
                                                                                span { class: "{status_class}", "{status_text}" }
                                                                                span { "{message_count} msgs" }
                                                                                if artifact_count > 0 { span { "{artifact_count} UI" } }
                                                                            }
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                    div { class: "agents-section",
                                                        div { class: "agents-section-head",
                                                            span { "Local work" }
                                                            span { class: "agents-section-meta", "no cloud" }
                                                        }
                                                        div { class: "agents-work-grid",
                                                            button { class: "agents-work-card", onclick: move |_| {
                                                                inspector_tab.set("review".to_string());
                                                                select_env_tab(env_tab, show_env, env_tab_by_tab, tabs, active_tab, "files", false);
                                                            },
                                                                Icon { name: "branch" }
                                                                span { class: "agents-work-title", "Review queue" }
                                                                span { class: "agents-work-sub", "{review_count} file(s)" }
                                                            }
                                                            button { class: "agents-work-card", onclick: move |_| {
                                                                select_env_tab(env_tab, show_env, env_tab_by_tab, tabs, active_tab, "changes", false);
                                                                let ws = changes_workspace.clone();
                                                                spawn(async move { changed_files.set(load_changed_files(&ws).await); });
                                                            },
                                                                Icon { name: "edit" }
                                                                span { class: "agents-work-title", "Changes" }
                                                                span { class: "agents-work-sub", "git diff + commit" }
                                                            }
                                                            button { class: "agents-work-card", onclick: move |_| {
                                                                select_env_tab(env_tab, show_env, env_tab_by_tab, tabs, active_tab, "preview", false);
                                                                spawn(async move { preview_ports.set(scan_ports().await); });
                                                            },
                                                                Icon { name: "browser" }
                                                                span { class: "agents-work-title", "Preview" }
                                                                span { class: "agents-work-sub", "browser + design mode" }
                                                            }
                                                            button { class: "agents-work-card", onclick: move |_| {
                                                                let ws = review_workspace.clone();
                                                                spawn(async move {
                                                                    let diff = run_cmd(&ws, "git", &["diff"]).await;
                                                                    let diff: String = diff.chars().take(12_000).collect();
                                                                    let prompt = format!(
                                                                        "Act as Bugbot. Review the current working changes for bugs, security issues, logic errors, and regressions. For each finding give: file:line, severity, why it is wrong, and the concrete fix. If the diff is clean, say so plainly.\n\n```diff\n{diff}\n```"
                                                                    );
                                                                    if *streaming.read() {
                                                                        queue.write().push(prompt);
                                                                    } else {
                                                                        engine.send(EngineCmd::Submit {
                                                                            engine: prompt,
                                                                            display: "/review (Bugbot)".into(),
                                                                        });
                                                                    }
                                                                });
                                                            },
                                                                Icon { name: "shield" }
                                                                span { class: "agents-work-title", "Bugbot review" }
                                                                span { class: "agents-work-sub", "local git diff" }
                                                            }
                                                            button { class: "agents-work-card", onclick: move |_| {
                                                                let ws = hermes_workspace.clone();
                                                                let goal = hermes_goal.read().clone();
                                                                let validation = hermes_validation.read().clone();
                                                                let status_sig = hermes_status;
                                                                spawn(async move {
                                                                    let context = hermes_diff_context(&ws).await;
                                                                    let prompt = hermes::build_evolve_prompt(&goal, &validation, &context);
                                                                    submit_hermes_prompt(cfg, engine, streaming, status_sig, prompt, "Hermes evolve".to_string());
                                                                });
                                                            },
                                                                Icon { name: "spark" }
                                                                span { class: "agents-work-title", "Hermes evolve" }
                                                                span { class: "agents-work-sub", "{hermes_profiles.read().len()} profile(s)" }
                                                            }
                                                        }
                                                        if bg_count > 0 || artifact_count > 0 {
                                                            div { class: "agents-chip-row",
                                                                if bg_count > 0 { span { class: "agents-chip", "{bg_count} background job(s)" } }
                                                                if artifact_count > 0 { span { class: "agents-chip", "{artifact_count} UI artifact(s)" } }
                                                            }
                                                        }
                                                    }
                                                    div { class: "agents-section",
                                                        div { class: "agents-section-head",
                                                            span { "Sub-agents" }
                                                            span { class: "agents-section-meta", "{subagent_cards.read().len()} total" }
                                                        }
                                                        if subagent_cards.read().is_empty() {
                                                            div { class: "agents-empty", "No sub-agents running. Enable orchestrate/sub-agents for multi-lane local work." }
                                                        }
                                                        for card in subagent_cards.read().iter().cloned() {
                                                            {
                                                                let worker_summary = if card.summary.is_empty() {
                                                                    card.worker_id.clone()
                                                                } else {
                                                                    card.summary.clone()
                                                                };
                                                                let stop_worker = card.worker_id.clone();
                                                                let worker_class = if card.running {
                                                                    "agents-worker running"
                                                                } else if card.ok {
                                                                    "agents-worker done"
                                                                } else {
                                                                    "agents-worker fail"
                                                                };
                                                                rsx! {
                                                                    div { class: "{worker_class}",
                                                                        span { class: "agents-worker-status",
                                                                            if card.running {
                                                                                span { class: "syn-spinner" }
                                                                            } else if card.ok {
                                                                                Icon { name: "check" }
                                                                            } else {
                                                                                Icon { name: "alert" }
                                                                            }
                                                                        }
                                                                        div { class: "agents-worker-copy",
                                                                            div { class: "agents-worker-title", "{card.profile} · {card.task}" }
                                                                            div { class: "agents-worker-sub", "{worker_summary}" }
                                                                            if !card.logs.is_empty() {
                                                                                div { class: "agents-worker-logs", "{card.logs.len()} tool log(s)" }
                                                                            }
                                                                            if card.running {
                                                                                div { class: "agents-worker-actions",
                                                                                    button { class: "agent-action", title: "Stop this sub-agent", onclick: move |_| {
                                                                                        engine.send(EngineCmd::SubagentControl {
                                                                                            worker_id: stop_worker.clone(),
                                                                                            action: SubagentControlAction::Interrupt,
                                                                                        });
                                                                                    },
                                                                                        Icon { name: "x" }
                                                                                        span { "Stop" }
                                                                                    }
                                                                                }
                                                                            }
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                    div { class: "agents-section",
                                                        div { class: "agents-section-head",
                                                            span { "Recent activity" }
                                                            span { class: "agents-section-meta", "timeline" }
                                                        }
                                                        if timeline.read().is_empty() {
                                                            div { class: "agents-empty", "No activity yet." }
                                                        }
                                                        for item in timeline.read().iter().cloned().rev().take(6) {
                                                            div { class: "agents-timeline-row",
                                                                span { class: "agents-timeline-dot" }
                                                                span { class: "agents-timeline-copy",
                                                                    span { class: "agents-timeline-title", "{item.title}" }
                                                                    if !item.sub.is_empty() { span { class: "agents-timeline-sub", "{item.sub}" } }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    },
                                    "review" => rsx! {
                                        if turn_edits.read().is_empty() {
                                            div { class: "insp-empty", "No changes to review. Edits the agent makes appear here — accept to keep, reject to revert." }
                                        } else {
                                            div { class: "review-head",
                                                span { class: "review-count", "{turn_edits.read().len()} changed file(s)" }
                                                button { class: "ed-close", onclick: move |_| {
                                                    let edits = turn_edits.read().clone();
                                                    for (_, _, _, cp, _) in edits.iter().rev() { engine.send(EngineCmd::Rewind { id: *cp }); reverted.write().insert(*cp); }
                                                    turn_edits.write().clear();
                                                }, "Reject all" }
                                            }
                                            for (path, adds, dels, cp, diff) in turn_edits.read().clone() {
                                                {
                                                    let is_accepted = cp != 0 && accepted.read().contains(&cp);
                                                    let is_reverted = cp != 0 && reverted.read().contains(&cp);
                                                    let item_cls = if is_reverted {
                                                        "review-item resolved reverted"
                                                    } else if is_accepted {
                                                        "review-item resolved kept"
                                                    } else {
                                                        "review-item"
                                                    };
                                                    rsx! {
                                                        div { class: "{item_cls}",
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
                                                                if is_reverted {
                                                                    span { class: "diff-reverted slot-status icon-slot", Icon { name: "check" } SlotText { text: "Reverted".to_string(), reverse: true } }
                                                                } else if is_accepted {
                                                                    span { class: "diff-kept slot-status icon-slot", Icon { name: "check" } SlotText { text: "Kept".to_string() } }
                                                                } else {
                                                                    button { class: "review-accept", title: "Keep this change", onclick: move |_| {
                                                                        if cp != 0 { accepted.write().insert(cp); }
                                                                    }, SlotText { text: "Accept".to_string() } }
                                                                    button { class: "review-reject", title: "Revert this change", onclick: move |_| {
                                                                        engine.send(EngineCmd::Rewind { id: cp });
                                                                        if cp != 0 { reverted.write().insert(cp); }
                                                                    }, SlotText { text: "Reject".to_string(), reverse: true } }
                                                                }
                                                            }
                                                        }
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
                                                    button { class: "ed-save", onclick: move |_| { engine.send(EngineCmd::Approve { id, decision: ApprovalDecision::Approve }); }, "Approve" }
                                                    button { class: "ed-save", onclick: move |_| { engine.send(EngineCmd::Approve { id, decision: ApprovalDecision::ApproveForSession }); }, "Always" }
                                                    button { class: "ed-close", onclick: move |_| { engine.send(EngineCmd::Approve { id, decision: ApprovalDecision::Reject }); }, "Reject" }
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
                                                    button { class: "ed-close", onclick: move |_| { engine.send(EngineCmd::Rewind { id }); }, "Rewind to here" }
                                                }
                                            }
                                        }
                                    },
                                    "sessions" => rsx! {
                                        {
                                            let sessions = list_sessions(&workspace);
                                            rsx! {
                                                if sessions.is_empty() {
                                                    div { class: "insp-empty", "No saved sessions yet. Conversations persist to the Oxide session database." }
                                                }
                                                for session in sessions {
                                                    {
                                                        let id = session.id.clone();
                                                        let title = session.title.clone();
                                                        let provider = session.provider.clone();
                                                        let count = session.count;
                                                        let path = session.path.clone();
                                                        let open_path = path.clone();
                                                        let open_title = title.clone();
                                                        let replay_path = path.clone();
                                                        let replay_title = title.clone();
                                                        rsx! {
                                                            div { class: "insp-card",
                                                                div { class: "insp-card-title",
                                                                    if let Some(l) = provider_logo(&provider) { span { class: "sess-logo prov-logo", dangerous_inner_html: l } }
                                                                    span { "{title}" }
                                                                }
                                                                div { class: "insp-card-sub", "{count} message(s) · {id}" }
                                                                div { class: "insp-card-actions",
                                                                    button { class: "ed-save", onclick: move |_| {
                                                                        open_session_tab(
                                                                            tabs,
                                                                            active_tab,
                                                                            messages,
                                                                            next_tab_id,
                                                                            cfg,
                                                                            ui,
                                                                            engine,
                                                                            busy_tabs,
                                                                            open_path.clone(),
                                                                            open_title.clone(),
                                                                        );
                                                                    }, "Open" }
                                                                    button { class: "ed-close", onclick: move |_| {
                                                                        session_replay.set(Some(load_session_replay(&replay_path, replay_title.clone())));
                                                                    }, "Replay" }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                                if let Some(replay) = session_replay.read().clone() {
                                                    {
                                                        let open_path = replay.path.clone();
                                                        let open_title = replay.title.clone();
                                                        let hidden = replay.total.saturating_sub(replay.rows.len());
                                                        rsx! {
                                                            div { class: "replay-card",
                                                                div { class: "replay-head",
                                                                    div {
                                                                        div { class: "replay-title", "{replay.title}" }
                                                                        div { class: "replay-meta", "{replay.total} stored row(s)" }
                                                                    }
                                                                    button { class: "ed-save", onclick: move |_| {
                                                                        open_session_tab(
                                                                            tabs,
                                                                            active_tab,
                                                                            messages,
                                                                            next_tab_id,
                                                                            cfg,
                                                                            ui,
                                                                            engine,
                                                                            busy_tabs,
                                                                            open_path.clone(),
                                                                            open_title.clone(),
                                                                        );
                                                                    }, "Continue" }
                                                                }
                                                                if hidden > 0 {
                                                                    div { class: "replay-meta", "{hidden} earlier row(s) hidden" }
                                                                }
                                                                div { class: "replay-list",
                                                                    for row in replay.rows {
                                                                        div { class: "replay-row replay-{row.role}",
                                                                            div { class: "replay-row-title", "{row.title}" }
                                                                            if !row.detail.trim().is_empty() {
                                                                                div { class: "replay-row-detail", "{row.detail}" }
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
                                                }, Icon { name: "arrow-up" } "Push" }
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
                                                div { class: "tl-item", div { class: "tl-title", span { class: "tl-icon", Icon { name: "tool" } } "{s}" } }
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
                                            let (tin, tout, _cached) = *usage.read();
                                            let limit = context_limit.read().unwrap_or(0);
                                            let pct = if limit > 0 { (tin as f64 / limit as f64 * 100.0).min(100.0) } else { 0.0 };
                                            rsx! {
                                                div { class: "usage-grid",
                                                    div { class: "usage-stat", div { class: "usage-num", "{tin}" } div { class: "usage-lbl", "input tokens" } }
                                                    div { class: "usage-stat", div { class: "usage-num", "{tout}" } div { class: "usage-lbl", "output tokens" } }
                                                }
                                                if limit > 0 {
                                                    div { class: "usage-bar-wrap",
                                                        div { class: "usage-bar-label", "context · {fmt_tokens(tin)} / {fmt_tokens(limit)}" }
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
                                if env_tab.read().as_str() == "term" {
                                    div { class: "env-terms",
                                        div { class: "term-tabs",
                                            for ti in 0..terms.read().len() {
                                                {
                                                    let title = terms.read()[ti].1.clone();
                                                    rsx! {
                                                        button { class: if ti == *term_sel.read() { "term-tab on" } else { "term-tab" },
                                                            onclick: move |_| term_sel.set(ti),
                                                            "{title}"
                                                            if terms.read().len() > 1 {
                                                                span { class: "term-tab-x", onclick: move |e: dioxus::prelude::MouseEvent| {
                                                                    e.stop_propagation();
                                                                    let n = { let mut tv = terms.write(); if ti < tv.len() { tv.remove(ti); } tv.len() };
                                                                    if *term_sel.read() >= n { term_sel.set(n.saturating_sub(1)); }
                                                                }, Icon { name: "x" } }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            button { class: "term-tab add", title: "New terminal", onclick: move |_| {
                                                let id = *term_seq.read() + 1; term_seq.set(id);
                                                terms.write().push((id, format!("zsh {id}"), Vec::new()));
                                                let n = terms.read().len(); term_sel.set(n - 1);
                                            }, Icon { name: "plus" } }
                                            button { class: "term-tab add", title: "Clear output", onclick: move |_| { let sel = *term_sel.read(); if let Some(t) = terms.write().get_mut(sel) { t.2.clear(); } }, Icon { name: "backspace" } }
                                            button { class: "term-tab add", title: "Native GPU terminal (Metal · oxide-term)", onclick: move |_| {
                                                if !launch_native_terminal() {
                                                    let sel = *term_sel.read();
                                                    if let Some(t) = terms.write().get_mut(sel) { t.2.push("oxide-term not found — build it: cargo build -p oxide-term".to_string()); }
                                                }
                                            }, Icon { name: "terminal" } }
                                        }
                                        div { class: "term-body",
                                            {
                                                let sel = *term_sel.read();
                                                let tl: Vec<String> = terms.read().get(sel).map(|t| t.2.clone()).unwrap_or_default();
                                                rsx! { for l in tl { div { class: "term-line", "{l}" } } }
                                            }
                                        }
                                        div { class: "term-input-row",
                                            span { class: "term-prompt", Icon { name: "chevron" } }
                                            input { class: "term-input", placeholder: "command…", value: "{term_input}",
                                                oninput: move |e| term_input.set(e.value()),
                                                onkeydown: move |e| if e.key() == Key::Enter { run_term(); },
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // Persistent TUI terminals: every "tui" tab renders here ALWAYS,
                    // with a stable key, so switching tabs never unmounts it — which
                    // would close its PTY and kill the CLI (codex/claude), losing the
                    // session. Only the active tui tab is shown (display:contents so
                    // .xterm-host fills the content area exactly as before); the rest
                    // stay mounted but hidden (display:none). This is the only place
                    // TerminalView is mounted. (Synara lesson: persist via mount +
                    // stable id, hide with CSS — never mount/unmount on tab switch.)
                    for t in tabs.read().iter().filter(|t| t.mode == "tui") {
                        div {
                            key: "tuihost-{t.id}",
                            class: if active_is_tui && t.id == active_tab_id {
                                "tui-host-live"
                            } else {
                                "tui-host-off"
                            },
                            TerminalView {
                                id: t.id,
                                bin: t.bin.clone(),
                                ws: workspace.display().to_string(),
                                resume: t.resume.clone(),
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
                                    button { class: "board-btn", onclick: move |_| { let _ = workspace_of(&cfg.read()); run_board(board, cfg, workspace_of(&cfg.read())); }, Icon { name: "play" } "Run To-Do" }
                                    button { class: "board-btn ghost", onclick: move |_| {
                                            let root = workspace_of(&cfg.read());
                                            sync_board_issues(board, root, board_sync_status, board_syncing);
                                        },
                                        if *board_syncing.read() { "Syncing…" } else { span { Icon { name: "refresh" } "Sync issues" } }
                                    }
                                }
                            }
                            div { class: "board-sync-status", "{board_sync_status}" }
                            div { class: "board-cols four",
                                for (col, label) in [(board::TODO, "To Do"), (board::DOING, "In Progress"), (board::REVIEW, "Review"), (board::DONE, "Done")] {
                                    div { class: "board-col",
                                        div { class: "board-col-head", "{label}" }
                                        for card in board.read().cards.iter().filter(|c| c.column == col).cloned() {
                                            {
                                                let cid = card.id;
                                                let cbranch = card.branch.clone();
                                                let has_source = !card.source.is_empty();
                                                let meta = [
                                                    card.source_status.clone(),
                                                    card.source_priority.clone(),
                                                    card.source_assignee.clone(),
                                                ]
                                                .into_iter()
                                                .filter(|item| !item.trim().is_empty())
                                                .collect::<Vec<_>>()
                                                .join(" · ");
                                                let issue_url = card.source_url.clone();
                                                rsx! {
                                                    div { class: if col == board::DOING { "board-card doing" } else { "board-card" },
                                                        if has_source {
                                                            div { class: "board-card-meta",
                                                                span { class: if card.source == "Linear" { "board-source linear" } else { "board-source github" }, "{card.source}" }
                                                                if !meta.is_empty() { span { "{meta}" } }
                                                            }
                                                        }
                                                        div { class: "board-card-title", "{card.title}" }
                                                        if !issue_url.is_empty() {
                                                            a { class: "board-card-link", href: "{issue_url}", target: "_blank", "Open issue" }
                                                        }
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
                                                            }, Icon { name: "check" } "Merge" }
                                                        }
                                                        button { class: "board-card-x", onclick: move |_| {
                                                            board.write().cards.retain(|c| c.id != cid);
                                                            let snap = board.read().clone(); snap.save(&workspace_of(&cfg.read()));
                                                        }, Icon { name: "x" } }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else if active_is_tui {
                        // The active terminal is rendered by the persistent TUI layer
                        // above (kept mounted across tab switches); nothing here.
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
                            Composer { streaming, engine, cfg, model_label: model_label.clone(),
                                       bypass, project: project.clone(), branch: branch.clone(),
                                       context_used: ctx_used, context_limit: ctx_limit,
                                       workspace: workspace.clone(), plan_mode, pursue_goal, goal_text, queue, picked_element,
                                       on_settings: move |_| {
                                           settings_initial_tab.set("model".to_string());
                                           show_settings.set(true);
                                       },
                                       on_open_folder: move |_| open_folder(cfg, ui, engine), on_pick_workspace: move |dir| apply_workspace(cfg, ui, engine, dir) }
                            div { class: "suggestions",
                                for s in suggestions.iter() {
                                    button { class: "suggestion",
                                        onclick: {
                                            let p = s.to_string();
                                            move |_| { engine.send(EngineCmd::Submit { engine: p.clone(), display: p.clone() }); }
                                        },
                                        Icon { name: "spark" } span { "{s}" }
                                    }
                                }
                            }
                        }
                    } else {
                        div { class: if *streaming.read() { "scroll" } else { "scroll smooth" },
                            div { class: "jump-anchor",
                                button { class: "jump-bottom", title: "Scroll to bottom",
                                    onclick: move |_| { spawn(async move { let _ = dioxus::document::eval("const s=document.querySelector('.scroll'); if(s) s.scrollTo({top:s.scrollHeight, behavior:'smooth'});").await; }); },
                                    Icon { name: "arrow-down" }
                                }
                            }
                            div {
                                // Key on the active tab so switching tabs remounts the
                                // transcript and replays the crossfade (col-cross), instead
                                // of an instant content swap.
                                key: "col-{active_tab}",
                                class: if *streaming.read() { "col streaming" } else { "col" },
                                {
                                    let last_user_idx = messages.read().iter().rposition(|m| m.author == Author::User);
                                    let turns = {
                                        let msgs = messages.read();
                                        build_transcript_turns(&msgs)
                                    };
                                    rsx! {
                                        for turn in turns.into_iter() {
                                        div { class: "turn",
                                        for group in turn.groups.into_iter() {
                                            {
                                                let is_act = group.activity;
                                                let idxs = group.indices;
                                                let group_key = group.key;
                                                let group_live = group.live;
                                                rsx! {
                                            if is_act && idxs.len() > 2 {
                                                {
                                                    let _ = group_live;
                                                    let rows: Vec<(String, bool, bool)> = idxs.iter().map(|&i| {
                                                        let m = &messages.read()[i];
                                                        if let Author::Activity { running, ok, .. } = m.author { (m.text.clone(), running, ok) } else { (m.text.clone(), false, true) }
                                                    }).collect();
                                                    let (icon, label) = activity_group_display(&rows);
                                                    // Default COLLAPSED, even while live — an open group with dozens
                                                    // of (animating) rows lags hard. The header shows live progress;
                                                    // the user expands for detail. And cap rendered rows to the most
                                                    // recent so expanding a huge group stays light.
                                                    let is_open = act_open.read().get(&group_key).copied().unwrap_or(false);
                                                    const ACT_ROW_CAP: usize = 12;
                                                    let hidden = rows.len().saturating_sub(ACT_ROW_CAP);
                                                    let shown: Vec<(String, bool, bool)> = rows.into_iter().skip(hidden).collect();
                                                    rsx! {
                                                        details { class: "act-group", open: is_open,
                                                            summary { class: "act-group-head",
                                                                onclick: move |e: dioxus::prelude::MouseEvent| {
                                                                    e.prevent_default();
                                                                    let cur = act_open.read().get(&group_key).copied().unwrap_or(false);
                                                                    act_open.write().insert(group_key, !cur);
                                                                },
                                                                span { class: "diff-caret", Icon { name: "chevron" } }
                                                                span { class: "act-group-icon", Icon { name: icon } }
                                                                "{label}"
                                                            }
                                                            if hidden > 0 { div { class: "act-more", "… {hidden} earlier" } }
                                                            for (t, r, o, count) in coalesce_activity_rows(shown) {
                                                                {
                                                                    let view = activity_view(&t);
                                                                    if matches!(view.kind, ActivityKind::FileChange) {
                                                                        // Join the file's cumulative +/− from turn_edits by
                                                                        // basename so the coalesced row carries live counts.
                                                                        let want = std::path::Path::new(&view.detail)
                                                                            .file_name()
                                                                            .map(|n| n.to_owned());
                                                                        let (adds, dels) = turn_edits
                                                                            .read()
                                                                            .iter()
                                                                            .filter(|e| {
                                                                                std::path::Path::new(&e.0)
                                                                                    .file_name()
                                                                                    .map(|n| n.to_owned())
                                                                                    == want
                                                                            })
                                                                            .fold((0u32, 0u32), |(a, d), e| (a + e.1, d + e.2));
                                                                        rsx! { EditActivityRow { text: t, running: r, ok: o, count, adds, dels } }
                                                                    } else {
                                                                        rsx! { ActivityRow { text: t, running: r, ok: o } }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            } else {
                                                for i in idxs {
                                                    {
                                                        let m = messages.read()[i].clone();
                                                        match &m.author {
                                                            // Standalone diff boxes are consolidated into the
                                                            // "Edited files" summary card below — render nothing here.
                                                            Author::Diff(_, _) => rsx! {},
                                                            Author::User => {
                                                                // Display text may carry pasted images after \u{2} separators —
                                                                // either inline data URLs or persisted `wsimg:` file refs.
                                                                let markers: Vec<String> = m.text.split('\u{2}').skip(1).map(String::from).collect();
                                                                let imgs: Vec<String> = markers.iter()
                                                                    .filter_map(|s| {
                                                                        if let Some(rel) = s.strip_prefix("wsimg:") {
                                                                            Some(format!("/wsimg/{rel}"))
                                                                        } else if s.starts_with("data:image") {
                                                                            Some(s.to_string())
                                                                        } else { None }
                                                                    }).collect();
                                                                let text_files: Vec<(String, String)> = markers.iter()
                                                                    .filter_map(|s| s.strip_prefix("wstxt:").map(|rel| (text_attachment_name(rel), rel.to_string())))
                                                                    .collect();
                                                                let segs = user_segments(&m.text);
                                                                let copy = serde_json::to_string(&strip_scaffold(&m.text)).unwrap_or_default();
                                                                let edit_text = strip_scaffold(&m.text);
                                                                let idx = i;
                                                                let _ = last_user_idx; let row_cls = "row user sticky-turn";
                                                                // Clamp long prompts (esp. while sticky) — expandable. Clamp the
                                                                // TEXT only (line-clamp), never the bubble itself; masking the
                                                                // bubble fights its backdrop-filter and renders it blank.
                                                                let long = edit_text.chars().count() > 240 || edit_text.lines().count() > 3;
                                                                let expanded = expanded_user.read().contains(&idx);
                                                                let text_cls = if long && !expanded { "user-text clamped" } else { "user-text" };
                                                                rsx! {
                                                                    div { class: "{row_cls}",
                                                                        div { class: "bubble",
                                                                            if !imgs.is_empty() {
                                                                                div { class: "msg-imgs",
                                                                                    for src in imgs {
                                                                                        img { class: "msg-img", src: "{src}",
                                                                                            onclick: { let s = src.clone(); move |_| chat_img.set(Some(s.clone())) } }
                                                                                    }
                                                                                }
                                                                            }
                                                                            if !text_files.is_empty() {
                                                                                div { class: "msg-files",
                                                                                    for (name, rel) in text_files {
                                                                                        div { class: "msg-file", title: "{rel}",
                                                                                            Icon { name: "file" }
                                                                                            span { "{name}" }
                                                                                        }
                                                                                    }
                                                                                }
                                                                            }
                                                                            div { class: "{text_cls}",
                                                                                for (is_m, s) in segs {
                                                                                    if is_m { span { class: "inline-chip", "{s}" } } else { "{s}" }
                                                                                }
                                                                            }
                                                                            if long {
                                                                                button { class: "bubble-more",
                                                                                    onclick: move |_| {
                                                                                        let mut e = expanded_user.write();
                                                                                        if !e.insert(idx) { e.remove(&idx); }
                                                                                    },
                                                                                    if expanded { "show less" } else { "show more" }
                                                                                }
                                                                            }
                                                                        }
                                                                        div { class: "msg-actions",
                                                                            button { class: "msg-act", title: "Copy message", onclick: move |_| { let c = copy.clone(); spawn(async move { let _ = document::eval(&format!("navigator.clipboard.writeText({c})")).await; }); push_toast(toasts, toast_seq, "ok", "Copied"); }, Icon { name: "copy" } }
                                                                            if *confirm_restore.read() == Some(idx) {
                                                                                button { class: "msg-act danger", title: "Click again to confirm — reverts files and brings this message back to the composer to edit & resend", onclick: move |_| {
                                                                                    confirm_restore.set(None);
                                                                                    // Revert every file change from this turn onward.
                                                                                    let floor = { let ms = messages.read(); ms.iter().skip(idx).find_map(|mm| if let Author::Diff(_, cp) = mm.author { Some(cp) } else { None }) };
                                                                                    if let Some(fl) = floor {
                                                                                        let ids: Vec<u64> = checkpoints.read().iter().map(|(id, _)| *id).filter(|id| *id >= fl).collect();
                                                                                        for id in ids.into_iter().rev() { engine.send(EngineCmd::Rewind { id }); reverted.write().insert(id); }
                                                                                    }
                                                                                    // Drop this turn and everything after it (UI)…
                                                                                    messages.write().truncate(idx);
                                                                                    // …and trim the engine + session history so the
                                                                                    // model forgets the removed turns (no pile-up).
                                                                                    let hist: Vec<(String, String)> = messages.read().iter().filter_map(|mm| match mm.author {
                                                                                        Author::User => Some(("user".to_string(), strip_scaffold(&mm.text))),
                                                                                        Author::Agent if !mm.text.is_empty() => Some(("assistant".to_string(), mm.text.clone())),
                                                                                        _ => None,
                                                                                    }).collect();
                                                                                    engine.send(EngineCmd::SetHistory(hist));
                                                                                    // …and load the message back into the composer to edit & resend.
                                                                                    let t = edit_text.clone();
                                                                                    spawn(async move {
                                                                                        let js = format!("const e=document.getElementById('ce-input'); if(e){{ e.textContent={}; e.focus(); const r=document.createRange(); r.selectNodeContents(e); r.collapse(false); const s=window.getSelection(); s.removeAllRanges(); s.addRange(r); }} return true;", serde_json::to_string(&t).unwrap_or_default());
                                                                                        let _ = dioxus::document::eval(&js).join::<bool>().await;
                                                                                    });
                                                                                }, "Edit & restore?" }
                                                                            } else {
                                                                                button { class: "msg-act", title: "Restore — revert files and edit this message", onclick: move |_| confirm_restore.set(Some(idx)), Icon { name: "undo" } }
                                                                            }
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                            _ if m.author == Author::Note && m.text.starts_with("§thought\t") => {
                                                                let mut parts = m.text.splitn(3, '\t');
                                                                let _ = parts.next();
                                                                let secs = parts.next().unwrap_or("1").to_string();
                                                                let body = parts.next().unwrap_or("").to_string();
                                                                rsx! {
                                                                    details { class: "thought-row",
                                                                        summary { class: "thought-sum", "Thought for {secs}s" }
                                                                        div { class: "thought-body", "{body}" }
                                                                    }
                                                                }
                                                            }
                                                            _ => {
                                                                let is_live = *streaming.read() && m.author == Author::Agent && i + 1 == messages.read().len();
                                                                let pin_snip: String = m.text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").chars().take(64).collect();
                                                                let is_agent = m.author == Author::Agent && !m.text.is_empty();
                                                                let ws_pin = workspace.clone();
                                                                let ws_mark = workspace.clone();
                                                                let snip2 = pin_snip.clone();
                                                                rsx! {
                                                                    if is_live && !thinking.read().is_empty() {
                                                                        details { class: "thinking-box", open: think_open.read().unwrap_or(true),
                                                                            summary {
                                                                                class: "thinking-sum live",
                                                                                onclick: move |e: dioxus::prelude::MouseEvent| {
                                                                                    e.prevent_default();
                                                                                    let cur = think_open.read().unwrap_or(true);
                                                                                    think_open.set(Some(!cur));
                                                                                },
                                                                                span { class: "thinking-glow", "Reasoning" }
                                                                                {
                                                                                    let el = *elapsed_s.read();
                                                                                    if el >= 1 {
                                                                                        rsx! { span { class: "thinking-secs", "{el}s" } }
                                                                                    } else {
                                                                                        rsx! {}
                                                                                    }
                                                                                }
                                                                            }
                                                                            div { class: "thinking-body", "{thinking}" }
                                                                        }
                                                                    }
                                                                    div { id: "msg-{i}", class: "pinwrap",
                                                                        Message { author: m.author.clone(), text: m.text.clone(), live: is_live }
                                                                        if is_agent && !is_live {
                                                                            div { class: "msg-side",
                                                                                button { class: "msg-pin", title: "Pin message",
                                                                                    onclick: move |_| {
                                                                                        if !pinned_msgs.read().iter().any(|p| p.0 == i) {
                                                                                            pinned_msgs.write().push((i, pin_snip.clone(), false));
                                                                                            thread_json_save(&ws_pin, "pins", &thread_stem(&tabs, &active_tab), &*pinned_msgs.read());
                                                                                        }
                                                                                    }, Icon { name: "pin" } }
                                                                                button { class: "msg-pin", title: "Mark — highlights selected text (or the message)",
                                                                                    onclick: move |_| {
                                                                                        let ws3 = ws_mark.clone();
                                                                                        let fallback = snip2.clone();
                                                                                        spawn(async move {
                                                                                            let sel = dioxus::document::eval("return (window.getSelection()||'').toString();")
                                                                                                .join::<String>().await.unwrap_or_default();
                                                                                            let text = if sel.trim().is_empty() { fallback } else { sel.chars().take(80).collect() };
                                                                                            let color = (markers.peek().len() % 4) as u8;
                                                                                            markers.write().push((i, text, color, false));
                                                                                            thread_json_save(&ws3, "markers", &thread_stem(&tabs, &active_tab), &*markers.read());
                                                                                        });
                                                                                    }, span { class: "mark-swatch c0" } }
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
                                        if let Some(sum) = turn.done_summary {
                                            div { class: "pinwrap", Message { author: Author::Note, text: sum, live: false } }
                                        }
                                        }
                                        }
                                    }
                                }
                                if !*streaming.read() && !thinking.read().is_empty() {
                                    {
                                        let live = *streaming.read();
                                        rsx! {
                                            details { class: "thinking-box", open: think_open.read().unwrap_or(live),
                                                summary {
                                                    class: if live { "thinking-sum live" } else { "thinking-sum" },
                                                    onclick: move |e: dioxus::prelude::MouseEvent| {
                                                        e.prevent_default();
                                                        let cur = think_open.read().unwrap_or(live);
                                                        think_open.set(Some(!cur));
                                                    },
                                                    // Cursor-style: shimmering "Reasoning" + live timer while
                                                    // thinking; settles to a plain label once the turn ends.
                                                    if live {
                                                        span { class: "thinking-glow", "Reasoning" }
                                                        {
                                                            let el = *elapsed_s.read();
                                                            if el >= 1 {
                                                                rsx! { span { class: "thinking-secs", "{el}s" } }
                                                            } else {
                                                                rsx! {}
                                                            }
                                                        }
                                                    } else {
                                                        "Reasoning"
                                                    }
                                                }
                                                div { class: "thinking-body", "{thinking}" }
                                            }
                                        }
                                    }
                                }
                                if *streaming.read() {
                                    // Keep the pill mounted for the WHOLE turn — gating it on a
                                    // non-empty status made it unmount/remount whenever `status`
                                    // momentarily emptied between events, restarting the spinner's
                                    // CSS animation each time (it looked frozen/stuck). A stable
                                    // key + always-mounted pill lets the spin run continuously.
                                    StatusPill { text: status.read().clone(), elapsed_s: *elapsed_s.read() }
                                }
                                if !bg_jobs.read().is_empty() {
                                    div { class: "bg-bar",
                                        span { class: "bg-orbit" }
                                        span { class: "bg-label", "Background" }
                                        for (bi, job) in bg_jobs.read().iter().cloned().enumerate() {
                                            span { class: "bg-chip", title: "Running in background — result won't auto-return",
                                                span { class: "bg-dot" }
                                                span { class: "bg-chip-text", "{job}" }
                                                button { class: "bg-x", title: "Dismiss", onclick: move |_| { let mut v = bg_jobs.write(); if bi < v.len() { v.remove(bi); } }, Icon { name: "x" } }
                                            }
                                        }
                                    }
                                }
                                if !queue.read().is_empty() {
                                    div { class: "queue-bar",
                                        span { class: "queue-label", Icon { name: "clock" } "Queued ({queue.read().len()})" }
                                        for (qi, q) in queue.read().iter().enumerate() {
                                            {
                                                let preview = queue_preview(q);
                                                let full = q.clone();
                                                rsx! {
                                                    span { class: "queue-chip", title: "Click to edit this queued message",
                                                        onclick: move |_| {
                                                            // Pull the item back into the composer for editing.
                                                            let mut qv = queue.write();
                                                            if qi < qv.len() { qv.remove(qi); }
                                                            let full = strip_scaffold(&full);
                                                            let js = format!(
                                                                "const e=document.getElementById('ce-input'); if(e){{ e.textContent={}; e.focus(); const r=document.createRange(); r.selectNodeContents(e); r.collapse(false); const s=window.getSelection(); s.removeAllRanges(); s.addRange(r); }} return true;",
                                                                serde_json::to_string(&full).unwrap_or_default()
                                                            );
                                                                spawn(async move { let _ = dioxus::document::eval(&js).join::<bool>().await; });
                                                        },
                                                        span { class: "queue-index", "{qi + 1}" }
                                                        "{preview}"
                                                        button { class: "queue-steer", title: "Steer now — inject into the running turn instead of waiting",
                                                            onclick: move |e: dioxus::prelude::MouseEvent| {
                                                                e.stop_propagation();
                                                                let text = {
                                                                    let mut qv = queue.write();
                                                                    if qi < qv.len() { Some(qv.remove(qi)) } else { None }
                                                                };
                                                                if let Some(t) = text {
                                                                    let display = strip_scaffold(&t);
                                                                    engine.send(EngineCmd::Submit { engine: t, display });
                                                                }
                                                            }, Icon { name: "corner-up-right" } }
                                                        button { class: "queue-x", onclick: move |e: dioxus::prelude::MouseEvent| { e.stop_propagation(); let mut qv = queue.write(); if qi < qv.len() { qv.remove(qi); } }, Icon { name: "x" } }
                                                    }
                                                }
                                            }
                                        }
                                        if queue.read().len() > 1 {
                                            button { class: "queue-clear", title: "Clear queued prompts", onclick: move |_| queue.write().clear(), "Clear" }
                                        }
                                    }
                                }
                                for (qid, question, options) in questions.read().iter().cloned() {
                                    div { class: "question-card",
                                        div { class: "question-q", Icon { name: "help" } span { "{question}" } }
                                        div { class: "question-opts",
                                            for (oi, opt) in options.iter().enumerate() {
                                                {
                                                    let opt = opt.clone();
                                                    rsx! {
                                                        button { class: "question-opt", onclick: move |_| { engine.send(EngineCmd::Answer { id: qid, text: opt.clone() }); q_answer.set(String::new()); },
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
                                                        if !a.is_empty() { engine.send(EngineCmd::Answer { id: qid, text: a }); q_answer.set(String::new()); }
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
                                            button { class: "approval-yes", onclick: move |_| { engine.send(EngineCmd::Approve { id, decision: ApprovalDecision::Approve }); }, "Approve" }
                                            button { class: "approval-always", onclick: move |_| { engine.send(EngineCmd::Approve { id, decision: ApprovalDecision::ApproveForSession }); }, "Always" }
                                            button { class: "approval-no", onclick: move |_| { engine.send(EngineCmd::Approve { id, decision: ApprovalDecision::Reject }); }, "Reject" }
                                        }
                                    }
                                }
                                if !*streaming.read() && !turn_edits.read().is_empty() {
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
                                                        span { class: "edits-counts", span { class: "diff-adds countup plus", style: "--n:{total_add}" } " " span { class: "diff-dels countup minus", style: "--n:{total_del}" } }
                                                    }
                                                    if *edits_undone.read() {
                                                        span { class: "edits-undone slot-status icon-slot", Icon { name: "check" } SlotText { text: "Undone".to_string(), reverse: true } }
                                                    } else {
                                                        div { class: "edits-actions",
                                                            button { class: "edits-undo", onclick: move |_| {
                                                                for (_, _, _, cp, _) in turn_edits.read().iter() { engine.send(EngineCmd::Rewind { id: *cp }); reverted.write().insert(*cp); }
                                                                edits_undone.set(true);
                                                            }, Icon { name: "undo" } SlotText { text: "Undo".to_string(), reverse: true } }
                                                        }
                                                    }
                                                }
                                                for (path, a, d, cp, diff) in edits.iter().take(shown).cloned() {
                                                    {
                                                        let is_reverted = reverted.read().contains(&cp);
                                                        let pending = diff.is_empty() && cp == 0;
                                                        if pending {
                                                            // Live row: the CLI is editing this file right now;
                                                            // the diff lands at the end of the turn.
                                                            rsx! {
                                                                div { class: "edits-row pending",
                                                                    span { class: "syn-spinner" }
                                                                    span { class: "edits-path", "{path}" }
                                                                    span { class: "edits-rowcounts shimmer slot-status", SlotText { text: "editing…".to_string() } }
                                                                }
                                                            }
                                                        } else {
                                                            rsx! {
                                                                details { class: "edits-row-d",
                                                                    summary { class: "edits-row",
                                                                        span { class: "edits-caret", Icon { name: "chevron" } }
                                                                        span { class: "edits-path", "{path}" }
                                                                        span { class: "edits-rowcounts", span { class: "diff-adds", "+{a}" } " " span { class: "diff-dels", "−{d}" } }
                                                                        if is_reverted {
                                                                            span { class: "diff-reverted slot-status icon-slot", Icon { name: "check" } SlotText { text: "Reverted".to_string(), reverse: true } }
                                                                        } else if accepted.read().contains(&cp) {
                                                                            span { class: "diff-kept slot-status icon-slot", Icon { name: "check" } SlotText { text: "Kept".to_string() } }
                                                                        } else if cp != 0 {
                                                                            button { class: "edits-row-keep",
                                                                                onclick: move |e: dioxus::prelude::MouseEvent| { e.prevent_default(); e.stop_propagation(); accepted.write().insert(cp); },
                                                                                SlotText { text: "Keep".to_string() } }
                                                                            button { class: "edits-row-revert",
                                                                                onclick: move |e: dioxus::prelude::MouseEvent| { e.prevent_default(); e.stop_propagation(); engine.send(EngineCmd::Rewind { id: cp }); reverted.write().insert(cp); },
                                                                                SlotText { text: "Revert".to_string(), reverse: true } }
                                                                        }
                                                                    }
                                                                    HunkedDiff { ws: workspace.clone(), path: path.clone(), diff }
                                                                }
                                                            }
                                                        }
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
                        if !subagent_cards.read().is_empty() {
                            {
                                let cards = subagent_cards.read().clone();
                                let done = cards.iter().filter(|c| !c.running).count();
                                let total = cards.len();
                                rsx! {
                                    div { class: "subagents-card",
                                        div { class: "subagents-head",
                                            span { class: "workflow-ic", Icon { name: "spark" } }
                                            span { "Subagents {done}/{total}" }
                                        }
                                        for card in cards {
                                            {
                                                let row_cls = if card.running { "subagent-row running" } else if card.ok { "subagent-row done" } else { "subagent-row fail" };
                                                rsx! {
                                                    div { class: "{row_cls}",
                                                        span { class: "subagent-status",
                                                            if card.running { span { class: "syn-spinner" } }
                                                            else if card.ok { Icon { name: "check" } }
                                                            else { Icon { name: "alert" } }
                                                        }
                                                        div { class: "subagent-copy",
                                                            div { class: "subagent-title", "{card.profile} · {card.task}" }
                                                                if !card.summary.trim().is_empty() {
                                                                    div { class: "subagent-summary", "{card.summary}" }
                                                                }
                                                                if !card.logs.is_empty() {
                                                                    div { class: "subagent-logs",
                                                                        for log in card.logs {
                                                                            {
                                                                                let log_cls = if log.running { "subagent-log running" } else if log.ok { "subagent-log done" } else { "subagent-log fail" };
                                                                                let lines = log.output.lines().count();
                                                                                rsx! {
                                                                                    details { class: "{log_cls}", open: log.running,
                                                                                        summary { class: "subagent-log-head",
                                                                                            span { class: "subagent-log-status",
                                                                                                if log.running { span { class: "syn-spinner" } }
                                                                                                else if log.ok { Icon { name: "check" } }
                                                                                                else { Icon { name: "alert" } }
                                                                                            }
                                                                                            span { class: "subagent-log-command", "{log.command}" }
                                                                                            if lines > 0 {
                                                                                                span { class: "subagent-log-lines", "{lines} lines" }
                                                                                            }
                                                                                        }
                                                                                        if !log.output.trim().is_empty() {
                                                                                            pre { class: "subagent-log-output", "{log.output}" }
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
                                                    if st == "completed" { Icon { name: "check" } } else if st == "in_progress" { span { class: "syn-spinner" } } else { "" }
                                                }
                                                span { class: "todo-text", "{content}" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        div { class: "composer-dock",
                            if *streaming.read() && !turn_edits.read().is_empty() {
                                {
                                    let edits = turn_edits.read().clone();
                                    let n = edits.len();
                                    let total_add: u32 = edits.iter().map(|e| e.1).sum();
                                    let total_del: u32 = edits.iter().map(|e| e.2).sum();
                                    let pending = edits.iter().filter(|e| e.4.is_empty() && e.3 == 0).count();
                                    let plural = if n == 1 { "" } else { "s" };
                                    let shown = n.min(3);
                                    let more = n.saturating_sub(shown);
                                    let subtitle = if pending > 0 {
                                        format!("{pending} live · diffs settle after the turn")
                                    } else {
                                        "Diffs ready for review".to_string()
                                    };
                                    rsx! {
                                        div { class: "composer-live-changes",
                                            div { class: "live-changes-head",
                                                span { class: "live-changes-icon", Icon { name: "edit" } }
                                                div { class: "live-changes-copy",
                                                    span { class: "live-changes-title", "Changing {n} file{plural}" }
                                                    span { class: "live-changes-sub", "{subtitle}" }
                                                }
                                                if total_add + total_del > 0 {
                                                    span { class: "live-changes-counts",
                                                        span { class: "diff-adds", "+{total_add}" }
                                                        span { class: "diff-dels", "−{total_del}" }
                                                    }
                                                }
                                                // No empty skeleton pill while counts are 0 — the
                                                // subtitle ("… diffs settle after the turn") already
                                                // signals pending; a blank grey bar just looked broken.
                                                button { class: "live-changes-review", title: "Open diffs",
                                                    onclick: move |_| {
                                                        edits_expanded.set(true);
                                                        select_env_tab(env_tab, show_env, env_tab_by_tab, tabs, active_tab, "changes", false);
                                                    },
                                                    Icon { name: "branch" }
                                                }
                                            }
                                            div { class: "live-changes-files",
                                                for (path, a, d, cp, diff) in edits.iter().take(shown).cloned() {
                                                    {
                                                        let row_pending = diff.is_empty() && cp == 0;
                                                        let row_cls = if row_pending { "live-change-file pending" } else { "live-change-file" };
                                                        rsx! {
                                                            div { class: "{row_cls}",
                                                                if row_pending { span { class: "syn-spinner" } } else { span { class: "live-change-ready", Icon { name: "check" } } }
                                                                span { class: "live-change-path", "{path}" }
                                                                if row_pending {
                                                                    span { class: "live-change-state shimmer slot-status", SlotText { text: "editing…".to_string() } }
                                                                } else {
                                                                    span { class: "live-change-state", span { class: "diff-adds", "+{a}" } " " span { class: "diff-dels", "−{d}" } }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                                if more > 0 {
                                                    div { class: "live-change-more", "+{more} more" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            if !*streaming.read() && !followups.read().is_empty() && !messages.read().is_empty() {
                                div { class: "followups",
                                    for f in followups.read().iter().cloned() {
                                        button { class: "suggestion followup",
                                            onclick: {
                                                let p = f.clone();
                                                move |_| { engine.send(EngineCmd::Submit { engine: p.clone(), display: p.clone() }); }
                                            },
                                            Icon { name: "spark" } span { "{f}" }
                                        }
                                    }
                                    button { class: "followups-x", title: "Dismiss", onclick: move |_| followups.write().clear(), Icon { name: "x" } }
                                }
                            }
                            Composer { streaming, engine, cfg, model_label, bypass,
                                       followup: !messages.read().is_empty(),
                                       project: project.clone(), branch: branch.clone(),
                                       context_used: ctx_used, context_limit: ctx_limit,
                                       workspace: workspace.clone(), plan_mode, pursue_goal, goal_text, queue, picked_element,
                                       on_settings: move |_| {
                                           settings_initial_tab.set("model".to_string());
                                           show_settings.set(true);
                                       },
                                       on_open_folder: move |_| open_folder(cfg, ui, engine), on_pick_workspace: move |dir| apply_workspace(cfg, ui, engine, dir) }
                        }
                    }
                }

                // Terminal dock
            }

            // ── Right inspector (tabbed) ───────────────────────────────

            // ── Settings modal ─────────────────────────────────────────
            if *show_settings.read() {
                SettingsModal {
                    cfg,
                    ui,
                    engine,
                    sessions_refresh,
                    projects_list,
                    initial_tab: settings_initial_tab.read().clone(),
                    automations,
                    automation_runs,
                    automation_name,
                    automation_schedule,
                    automation_prompt,
                    automation_status,
                    automation_confirm_delete,
                    hermes_profiles,
                    hermes_profile_name,
                    hermes_goal,
                    hermes_validation,
                    hermes_review_prompt,
                    hermes_status,
                    hermes_confirm_delete,
                    streaming,
                    queue,
                    on_close: move |_| show_settings.set(false)
                }
            }
            if *show_skills.read() {
                SkillsModal { workspace: workspace.clone(), on_close: move |_| show_skills.set(false) }
            }
            if *show_mcp.read() {
                McpModal { cfg, engine, status: mcp_status, on_close: move |_| show_mcp.set(false) }
            }
            div { class: "toasts",
                for toast in toasts.read().iter().cloned() {
                    {
                        let tid = toast.id;
                        let kind = toast.kind.clone();
                        let text = toast.text.clone();
                        let action_label = toast.action_label.clone();
                        let action = toast.action.clone();
                        let toast_class = if action_label.is_some() {
                            format!("toast {kind} has-action")
                        } else {
                            format!("toast {kind}")
                        };
                        rsx! {
                            div { key: "{tid}", class: "{toast_class}",
                                onclick: move |_| { toasts.clone().write().retain(|t| t.id != tid); },
                                span { class: "toast-dot" }
                                span { "{text}" }
                                if let (Some(label), Some(action)) = (action_label.clone(), action.clone()) {
                                    button {
                                        class: "toast-action",
                                        onclick: move |e: dioxus::prelude::MouseEvent| {
                                            e.stop_propagation();
                                            match action.clone() {
                                                ToastAction::RestoreSessions(ids) => {
                                                    for id in &ids {
                                                        oxide_core::db::restore(id);
                                                    }
                                                    flash_restored_sessions(restored_sessions, ids);
                                                }
                                                ToastAction::RestoreDeletedSession(spec) => {
                                                    let restored_id = spec.id.clone();
                                                    restore_deleted_session(&spec);
                                                    flash_restored_sessions(restored_sessions, vec![restored_id]);
                                                }
                                            }
                                            toasts.clone().write().retain(|t| t.id != tid);
                                            sessions_refresh.set(sessions_refresh() + 1);
                                            refresh_projects_list(projects_list, cfg);
                                            push_toast(toasts, toast_seq, "ok", "Restored");
                                        },
                                        "{label}"
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // Lightbox for images attached to sent messages.
            if let Some(src) = chat_img.read().clone() {
                div { class: "img-lightbox", onclick: move |_| chat_img.set(None),
                    button { class: "img-lightbox-x", onclick: move |_| chat_img.set(None), Icon { name: "x" } }
                    img { class: "img-lightbox-img", src: "{src}", onclick: move |e| e.stop_propagation() }
                }
            }
            if *show_shortcuts.read() {
                div { class: "modal-overlay", onclick: move |_| show_shortcuts.set(false),
                    div { class: "modal shortcuts-modal", onclick: move |e| e.stop_propagation(),
                        div { class: "modal-head", h2 { "Keyboard shortcuts" } button { class: "term-x", onclick: move |_| show_shortcuts.set(false), Icon { name: "x" } } }
                        div { class: "modal-body shortcuts-body",
                            for (k, d) in [
                                ("Cmd-K", "Command palette + chat search"),
                                ("Cmd-/", "This shortcuts sheet"),
                                ("Cmd-B", "Toggle Files inspector"),
                                ("Cmd-L", "Focus / blur composer"),
                                ("Cmd-1-9", "Jump to agent tab N"),
                                ("Cmd-Shift-]", "Next tab"),
                                ("Cmd-Shift-[", "Previous tab"),
                                ("Cmd-Enter", "Send message"),
                                ("Shift-Enter", "New line in composer"),
                                ("Shift-Tab", "Toggle plan mode (in composer)"),
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
                            "Automations" => {
                                settings_initial_tab.set("automations".to_string());
                                show_settings.set(true);
                            }
                            "Files panel" => select_env_tab(env_tab, show_env, env_tab_by_tab, tabs, active_tab, "files", true),
                            "Terminal" => select_env_tab(env_tab, show_env, env_tab_by_tab, tabs, active_tab, "term", true),
                            "Settings…" => {
                                settings_initial_tab.set("model".to_string());
                                show_settings.set(true);
                            },
                            "Theme: Light" => set_theme(cfg, "light"),
                            "Theme: Dark" => set_theme(cfg, "dark"),
                            "Theme: System" => set_theme(cfg, "system"),
                            "Toggle density" => toggle_density(cfg),
                            _ => {}
                        }
                    };
                    let actions: Vec<(&str, &str)> = vec![
                        ("plus", "New chat"), ("folder", "Open folder…"), ("plugins", "Split view"),
                        ("plugins", "MCP servers"), ("target", "Skills"), ("list", "Board"), ("clock", "Automations"),
                        ("plugins", "Files panel"), ("terminal", "Terminal"), ("settings", "Settings…"),
                        ("spark", "Theme: Light"), ("target", "Theme: Dark"), ("settings", "Theme: System"),
                        ("list", "Toggle density"),
                    ];
                    let q = palette_query.read().to_lowercase();
                    let filtered: Vec<(&str, &str)> = actions.into_iter().filter(|(_, l)| q.is_empty() || l.to_lowercase().contains(&q)).collect();
                    let sel = if filtered.is_empty() { 0 } else { (*palette_sel.read()).min(filtered.len() - 1) };
                    let f2 = filtered.clone();
                    let status_map = tab_statuses.read().clone();
                    let open_tabs: Vec<(usize, u64, String, String, Option<TabStatus>)> = tabs
                        .read()
                        .iter()
                        .enumerate()
                        .filter_map(|(idx, tab)| {
                            let title = if tab.title.trim().is_empty() { "New chat".to_string() } else { tab.title.clone() };
                            let status = status_map.get(&tab.id).cloned();
                            let status_text = status.as_ref().map(tab_status_label).unwrap_or("");
                            let hay = format!("{title} {} {status_text}", tab.provider).to_lowercase();
                            if q.is_empty() || hay.contains(&q) {
                                Some((idx, tab.id, title, tab.provider.clone(), status))
                            } else {
                                None
                            }
                        })
                        .collect();
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
                                    if !open_tabs.is_empty() {
                                        div { class: "menu-label palette-section", "Tabs" }
                                        for (idx, id, title, provider, status) in open_tabs {
                                            {
                                                let is_active = tabs.read().get(*active_tab.read()).map(|tab| tab.id == id).unwrap_or(false);
                                                let status_label = status.as_ref().map(tab_status_label).unwrap_or("");
                                                let status_class = status.as_ref().map(tab_status_class).unwrap_or("");
                                                let meta = if status_label.is_empty() {
                                                    provider.clone()
                                                } else {
                                                    format!("{provider} · {status_label}")
                                                };
                                                rsx! {
                                                    button { class: if is_active { "palette-item sel" } else { "palette-item" },
                                                        onclick: move |_| {
                                                            show_palette.set(false);
                                                            show_board.set(false);
                                                            switch_tab(tabs, active_tab, messages, cfg, engine, idx);
                                                        },
                                                        Icon { name: "branch" }
                                                        span { class: "palette-copy",
                                                            span { class: "palette-label", "{title}" }
                                                            span { class: "palette-meta", "{meta}" }
                                                        }
                                                        if !status_label.is_empty() {
                                                            span { class: "palette-status {status_class}", "{status_label}" }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    if !q.is_empty() {
                                        {
                                            // Search ALL workspaces' sessions, not just the active one.
                                            let chats: Vec<(PathBuf, String)> = oxide_core::db::search(&q, 8).into_iter()
                                                .map(|m| (PathBuf::from(m.id), if m.title.trim().is_empty() { "Chat".to_string() } else { m.title }))
                                                .collect();
                                            if chats.is_empty() { rsx!{} } else {
                                                rsx! {
                                                    div { class: "menu-label", style: "padding:8px 12px 4px", "Chats" }
                                                    for (p, title) in chats {
                                                        {
                                                            let p2 = p.clone();
                                                            let t2 = title.clone();
                                                            rsx! {
                                                                button { class: "palette-item",
                                                                    onclick: move |_| { show_palette.set(false); show_board.set(false); open_session_tab(tabs, active_tab, messages, next_tab_id, cfg, ui, engine, busy_tabs, p2.clone(), t2.clone()); },
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
fn McpModal(
    cfg: Signal<Config>,
    engine: Coroutine<EngineCmd>,
    status: Signal<std::collections::HashMap<String, String>>,
    on_close: EventHandler<()>,
) -> Element {
    let mut name = use_signal(String::new);
    let mut command = use_signal(String::new);
    let mut args = use_signal(String::new);
    let servers = cfg.read().mcp_servers.clone();
    let workspace = workspace_of(&cfg.read());
    let imported: Vec<oxide_config::McpServerConfig> =
        oxide_core::discover_external_mcp_for_workspace(&workspace)
            .into_iter()
            .filter(|e| !servers.iter().any(|s| s.name == e.name))
            .collect();
    rsx! {
        div { class: "modal-overlay", onclick: move |_| on_close.call(()),
            div { class: "modal skills-modal", onclick: move |e| e.stop_propagation(),
                div { class: "modal-head",
                    h2 { "MCP servers" }
                    button { class: "term-x", onclick: move |_| on_close.call(()), Icon { name: "x" } }
                }
                div { class: "modal-body skills-body",
                    if servers.is_empty() {
                        div { class: "insp-empty", "No MCP servers. Add one below (e.g. npx @modelcontextprotocol/server-filesystem)." }
                    }
                    for (i, s) in servers.iter().enumerate() {
                        {
                            let st = status.read().get(&s.name).cloned();
                            let connected = st.as_deref().map(|x| x.starts_with("connected")).unwrap_or(false);
                            let cmdline = if s.url.is_empty() { format!("{} {}", s.command, s.args.join(" ")) } else { s.url.clone() };
                            let servers2 = servers.clone();
                            rsx! {
                                div { class: "mcp-item",
                                    div { class: "mcp-top",
                                        span { class: if connected { "mcp-dot on" } else { "mcp-dot" } }
                                        span { class: "skill-name", "{s.name}" }
                                        span { class: "mcp-tag", if s.url.is_empty() { "local" } else { "http" } }
                                        button { class: "mcp-remove", onclick: move |_| {
                                            let mut list = servers2.clone(); list.remove(i);
                                            let mut c = cfg.read().clone(); c.mcp_servers = list; cfg.set(c.clone());
                                            engine.send(EngineCmd::Reconfigure(c));
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
                                let source = if s.source.is_empty() { "imported".to_string() } else { s.source.clone() };
                                let trusted = s.clone();
                                rsx! {
                                    div { class: "mcp-item",
                                        div { class: "mcp-top",
                                            span { class: if connected { "mcp-dot on" } else { "mcp-dot" } }
                                            span { class: "skill-name", "{s.name}" }
                                            span { class: "mcp-tag", if disabled { "disabled" } else if s.url.is_empty() { "imported" } else { "http" } }
                                            button { class: "mcp-remove", onclick: move |_| {
                                                let mut server = trusted.clone();
                                                server.enabled = true;
                                                let mut list = cfg.read().mcp_servers.clone();
                                                if !list.iter().any(|item| item.name == server.name) {
                                                    list.push(server);
                                                    list.sort_by(|a, b| a.name.cmp(&b.name));
                                                }
                                                let mut c = cfg.read().clone(); c.mcp_servers = list; cfg.set(c.clone());
                                                engine.send(EngineCmd::Reconfigure(c));
                                            }, "Trust" }
                                        }
                                        div { class: "mcp-src", "{source}" }
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
                            list.push(oxide_config::McpServerConfig {
                                name: n,
                                command: cmd,
                                args: a,
                                ..oxide_config::McpServerConfig::default()
                            });
                            let mut c = cfg.read().clone(); c.mcp_servers = list; cfg.set(c.clone());
                            engine.send(EngineCmd::Reconfigure(c));
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
                    button { class: "term-x", onclick: move |_| on_close.call(()), Icon { name: "x" } }
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
fn apply_workspace(
    mut cfg: Signal<Config>,
    mut ui: Ui,
    engine: Coroutine<EngineCmd>,
    dir: PathBuf,
) {
    ui.workspace.set(dir.clone());
    ui.open_path.set(None);
    ui.expanded.set(HashSet::new());
    let mut c = cfg.read().clone();
    c.recent_workspaces.retain(|p| p != &dir);
    c.recent_workspaces.insert(0, dir.clone());
    c.recent_workspaces.truncate(8);
    c.workspace = Some(dir);
    cfg.set(c.clone());
    engine.send(EngineCmd::Reconfigure(c));
}

/// Switch the active agent tab: save the current transcript, load the target's.
fn switch_tab(
    mut tabs: Signal<Vec<AgentTab>>,
    mut active_tab: Signal<usize>,
    messages: Signal<Vec<ChatMsg>>,
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
        // t.session was bound from Event::SessionPath when this tab's engine
        // opened its file — no newest-file guessing (that mixes tabs up).
    }
    let t = tabs.read()[idx].clone();
    active_tab.set(idx);
    let mut c = cfg.read().clone();
    c.provider = t.provider.clone();
    c.model = t.model.clone();
    c.harness = t.harness.clone();
    c.reasoning_effort = t.reasoning_effort.clone();
    c.resume_path = t.session.clone();
    cfg.set(c.clone());
    engine.send(EngineCmd::SwitchTab {
        id: t.id,
        conf: c,
        msgs: t.messages.clone(),
    });
    scroll_chat_bottom();
}

/// Open a fresh agent tab for `provider` and make it active.
#[allow(clippy::too_many_arguments)]
fn new_agent_tab(
    mut tabs: Signal<Vec<AgentTab>>,
    mut active_tab: Signal<usize>,
    messages: Signal<Vec<ChatMsg>>,
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
        harness: cfg.read().harness.clone(),
        reasoning_effort: cfg.read().reasoning_effort.clone(),
        messages: Vec::new(),
        mode: "gui".to_string(),
        bin: String::new(),
        session: None,
        resume: None,
    });
    let idx = tabs.read().len() - 1;
    active_tab.set(idx);
    let mut c = cfg.read().clone();
    c.provider = provider.to_string();
    c.model = model.to_string();
    // Fresh tab = fresh conversation — never inherit another tab's session.
    c.resume_path = None;
    c.resume = false;
    cfg.set(c.clone());
    engine.send(EngineCmd::SwitchTab {
        id,
        conf: c,
        msgs: Vec::new(),
    });
}

/// Open an embedded-TUI tab running `bin` (codex/claude) in a PTY.
fn new_tui_tab(
    mut tabs: Signal<Vec<AgentTab>>,
    mut active_tab: Signal<usize>,
    messages: Signal<Vec<ChatMsg>>,
    mut next_id: Signal<u64>,
    bin: &str,
    title: &str,
) {
    let cur = *active_tab.read();
    if let Some(t) = tabs.write().get_mut(cur) {
        t.messages = messages.read().clone();
    }
    // If opened FROM a codex/claude chat, resume that chat's native CLI session so
    // the terminal continues the conversation instead of starting blank. Only when
    // the originating provider matches the CLI bin (a chatgpt/anthropic API chat has
    // no CLI session to hand off).
    let resume = {
        let tabs_ro = tabs.read();
        tabs_ro.get(cur).and_then(|t| {
            let matches = match bin {
                "codex" => t.provider == "codex",
                "claude" => t.provider == "claude" || t.provider == "claude_interactive",
                _ => false,
            };
            if !matches {
                return None;
            }
            t.session
                .as_ref()
                .and_then(|p| oxide_core::db::cli_session(&sid(p)))
        })
    };
    let id = *next_id.read();
    next_id.set(id + 1);
    tabs.write().push(AgentTab {
        id,
        title: format!("{title} (TUI)"),
        provider: bin.to_string(),
        model: String::new(),
        harness: "default".to_string(),
        reasoning_effort: "medium".to_string(),
        messages: Vec::new(),
        mode: "tui".to_string(),
        bin: bin.to_string(),
        session: None,
        resume,
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
    let cur = *active_tab.read();
    // Save the LIVE transcript into the current tab before mutating the list —
    // otherwise closing a background tab reverts the visible chat to a stale
    // snapshot.
    if let Some(t) = tabs_w.write().get_mut(cur) {
        t.messages = messages.read().clone();
    }
    // Stop the closed tab's own engine (engines are per-tab now).
    let closed_id = tabs_w.read().get(idx).map(|t| t.id).unwrap_or(0);
    engine.send(EngineCmd::CloseTab(closed_id));
    tabs_w.write().remove(idx);
    let len_after = tabs_w.read().len();
    if idx != cur {
        // Active tab survives: just remap the index — no engine restart,
        // no transcript reload (an in-flight stream keeps going).
        let new_idx = if idx < cur { cur - 1 } else { cur }.min(len_after - 1);
        let mut active = active_tab;
        active.set(new_idx);
        return;
    }
    let new_idx = cur.min(len_after - 1);
    let mut active = active_tab;
    active.set(usize::MAX);
    switch_tab(tabs_w, active, messages, cfg, engine, new_idx);
}

/// Rename a tab in memory AND persist to the session's DB row (survives reload).
fn rename_tab_title(mut tabs: Signal<Vec<AgentTab>>, id: u64, name: &str) {
    let mut sess = None;
    if let Some(t) = tabs.write().iter_mut().find(|t| t.id == id) {
        t.title = name.to_string();
        sess = t.session.clone();
    }
    if let Some(s) = sess {
        oxide_core::db::set_title(&sid(&s), name);
    }
}

/// Short tab title from the first user message.
fn make_title(text: &str) -> String {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
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
        "claude_interactive" => "Claude Code Interactive",
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
    let now_pinned = oxide_core::db::meta(path)
        .map(|m| m.pinned)
        .unwrap_or(false);
    oxide_core::db::set_pinned(path, !now_pinned);
    let c = cfg.read().clone();
    let _ = &c;
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
    c.density = if c.density == "compact" {
        "comfortable".to_string()
    } else {
        "compact".to_string()
    };
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

/// Native folder picker to switch workspace.
fn open_folder(cfg: Signal<Config>, ui: Ui, engine: Coroutine<EngineCmd>) {
    // MUST use the async dialog: the blocking `FileDialog::pick_folder()` runs
    // an NSOpenPanel modal loop on the main thread, which deadlocks the webview
    // when invoked from inside a synchronous JS to native event dispatch.
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
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
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

/// Class-based syntax highlight for one code block (theme colors come from CSS,
/// so dark/light both work). Falls back to escaped plain text.
/// Minimal percent-decoding for asset-handler request paths.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let h = |c: u8| (c as char).to_digit(16);
            if let (Some(a), Some(c)) = (h(b[i + 1]), h(b[i + 2])) {
                out.push((a * 16 + c) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn highlight_code(code: &str, lang: &str) -> String {
    use syntect::html::{ClassStyle, ClassedHTMLGenerator};
    use syntect::parsing::SyntaxSet;
    use syntect::util::LinesWithEndings;
    static SS: std::sync::OnceLock<SyntaxSet> = std::sync::OnceLock::new();
    let ss = SS.get_or_init(SyntaxSet::load_defaults_newlines);
    let syn = ss
        .find_syntax_by_token(lang)
        .or_else(|| ss.find_syntax_by_extension(lang))
        .unwrap_or_else(|| ss.find_syntax_plain_text());
    let mut gen = ClassedHTMLGenerator::new_with_class_style(syn, ss, ClassStyle::Spaced);
    for line in LinesWithEndings::from(code) {
        if gen
            .parse_html_for_line_which_includes_newline(line)
            .is_err()
        {
            return code
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
        }
    }
    gen.finalize()
}

/// Render agent markdown to safe HTML: raw HTML in the source is escaped
/// first (so injection is impossible), then markdown is converted. Fenced code
/// blocks get class-based syntax highlighting.
fn md_to_html(src: &str, live: bool) -> String {
    if live {
        return md_live_html(src);
    }

    // Render cache: re-renders (tab switches, scroll-driven updates) hit the
    // cache instead of re-running pulldown+syntect on every message again.
    thread_local! {
        static CACHE: std::cell::RefCell<std::collections::HashMap<u64, String>> =
            std::cell::RefCell::new(std::collections::HashMap::new());
    }
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        src.hash(&mut h);
        live.hash(&mut h);
        h.finish()
    };
    if let Some(hit) = CACHE.with(|c| c.borrow().get(&key).cloned()) {
        return hit;
    }
    let out = md_to_html_uncached(src, live);
    CACHE.with(|c| {
        let mut m = c.borrow_mut();
        if m.len() > 512 {
            m.clear();
        }
        m.insert(key, out.clone());
    });
    out
}

/// Live (streaming) markdown render. Earlier this just wrapped each raw line in
/// a <div> — so `#`, `**bold**`, lists etc. stayed as literal syntax until the
/// turn ended, then the whole message reflowed at once (a jarring jump). Now we
/// render real markdown progressively: every COMPLETED line is styled the moment
/// it finishes (headings/bold appear live, and there's no end-of-turn pop), while
/// the trailing in-progress line is kept raw so the actively-streaming line
/// doesn't relayout on every token.
fn md_live_html(src: &str) -> String {
    // Inside an unclosed code fence, render the whole buffer as markdown so the
    // partial code streams as a code block (pulldown extends an open fence to
    // EOF) instead of leaking raw fence lines. Both ``` and ~~~ open fences.
    if src.matches("```").count() % 2 == 1 || src.matches("~~~").count() % 2 == 1 {
        return md_to_html_uncached(src, true);
    }

    // Split off the trailing partial line at the last newline.
    let (stable, tail) = match src.rfind('\n') {
        Some(nl) => (&src[..=nl], &src[nl + 1..]),
        None => ("", src),
    };

    // A markdown TABLE row is being streamed: a table needs its rows in one
    // contiguous block, so keeping this "| … |" tail raw (below) would render it
    // as literal text beneath the already-parsed rows — the table looks broken
    // mid-stream. Render the whole buffer so the in-progress row joins its table,
    // same idea as the open code-fence case above. (Costs a re-parse per token
    // only while a table row streams, which is cheap and short-lived.) Also catch
    // GFM tables WITHOUT outer pipes ("a | b" / "--- | ---" / "c | d"): if the
    // in-progress tail and the line just above both carry an inner pipe, it's
    // almost certainly a mid-stream table body row. Worst-case false positive is
    // a harmless extra re-parse of prose — pulldown won't fabricate a table.
    let tail_is_table = {
        let t = tail.trim_start();
        t.starts_with('|')
            || (tail.contains('|')
                && stable
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .is_some_and(|l| l.contains('|')))
    };
    if tail_is_table {
        return md_to_html_uncached(src, true);
    }

    // The stable prefix only changes when a line completes — not on every
    // streamed token (the tail carries the in-progress line) — so cache its
    // markdown render and re-parse only when the prefix actually changes.
    let stable_html = if stable.is_empty() {
        String::new()
    } else {
        thread_local! {
            static LIVE_CACHE: std::cell::RefCell<(u64, String)> =
                const { std::cell::RefCell::new((0, String::new())) };
        }
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        stable.hash(&mut h);
        let key = h.finish();
        LIVE_CACHE.with(|c| {
            let mut cell = c.borrow_mut();
            if cell.0 != key {
                *cell = (key, md_to_html_uncached(stable, true));
            }
            cell.1.clone()
        })
    };

    let mut html = String::with_capacity(stable_html.len() + tail.len() + 64);
    html.push_str(&stable_html);
    if !tail.is_empty() {
        html.push_str("<div class=\"live-tail\">");
        html.push_str(&esc(tail));
        html.push_str("</div>");
    }
    html
}

fn md_to_html_uncached(src: &str, live: bool) -> String {
    use pulldown_cmark::{CodeBlockKind, Event as MdEvent, Options, Parser, Tag, TagEnd};
    let escaped = src
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(&escaped, opts);

    // Intercept fenced code blocks for syntect; stream the rest to the default
    // HTML writer in segments.
    let mut html = String::with_capacity(src.len() * 2);
    let mut seg: Vec<MdEvent> = Vec::new();
    let mut code = String::new();
    let mut lang = String::new();
    let mut in_code = false;
    for ev in parser {
        match ev {
            MdEvent::Start(Tag::CodeBlock(kind)) => {
                pulldown_cmark::html::push_html(&mut html, seg.drain(..));
                in_code = true;
                code.clear();
                lang = match kind {
                    CodeBlockKind::Fenced(l) => {
                        l.split_whitespace().next().unwrap_or("").to_string()
                    }
                    _ => String::new(),
                };
            }
            MdEvent::End(TagEnd::CodeBlock) => {
                in_code = false;
                // The source was pre-escaped; un-escape so syntect sees real code,
                // its output re-escapes safely.
                let raw = code
                    .replace("&lt;", "<")
                    .replace("&gt;", ">")
                    .replace("&amp;", "&");
                if lang == "mermaid" && !live {
                    // Render the diagram once the fence closes (never partial).
                    let esc = raw
                        .replace('&', "&amp;")
                        .replace('<', "&lt;")
                        .replace('>', "&gt;");
                    html.push_str(&format!("<div class=\"mermaid\">{esc}</div>"));
                } else {
                    let body = if live || lang == "mermaid" {
                        raw.replace('&', "&amp;")
                            .replace('<', "&lt;")
                            .replace('>', "&gt;")
                    } else {
                        highlight_code(&raw, &lang)
                    };
                    html.push_str(&format!("<pre><code class=\"hl\">{body}</code></pre>"));
                }
            }
            MdEvent::Text(t) if in_code => code.push_str(&t),
            other if !in_code => seg.push(other),
            _ => {}
        }
    }
    pulldown_cmark::html::push_html(&mut html, seg.drain(..));
    // Point local image sources at the workspace asset handler so they load.
    html.replace("<img src=\"./", "<img loading=\"lazy\" src=\"/wsimg/")
        .replace("<img src=\"/", "<img loading=\"lazy\" src=\"/wsimg/")
}

/// Bundled VSCode Material Icon Theme SVGs (MIT — material-extensions).
fn material_icon(name: &str, is_dir: bool) -> &'static str {
    if is_dir {
        return include_str!("../assets/ficons/folder-base.svg");
    }
    let n = name.to_ascii_lowercase();
    let ext = n.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => include_str!("../assets/ficons/rust.svg"),
        "ts" | "mts" | "cts" => include_str!("../assets/ficons/typescript.svg"),
        "tsx" => include_str!("../assets/ficons/react_ts.svg"),
        "jsx" => include_str!("../assets/ficons/react.svg"),
        "js" | "mjs" | "cjs" => include_str!("../assets/ficons/javascript.svg"),
        "json" | "jsonc" => include_str!("../assets/ficons/json.svg"),
        "md" => include_str!("../assets/ficons/markdown.svg"),
        "css" | "scss" | "less" => include_str!("../assets/ficons/css.svg"),
        "html" | "htm" => include_str!("../assets/ficons/html.svg"),
        "py" => include_str!("../assets/ficons/python.svg"),
        "go" => include_str!("../assets/ficons/go.svg"),
        "yaml" | "yml" => include_str!("../assets/ficons/yaml.svg"),
        "toml" | "ini" | "conf" => include_str!("../assets/ficons/toml.svg"),
        "sh" | "bash" | "zsh" => include_str!("../assets/ficons/console.svg"),
        "sql" => include_str!("../assets/ficons/database.svg"),
        "vue" => include_str!("../assets/ficons/vue.svg"),
        "svelte" => include_str!("../assets/ficons/svelte.svg"),
        "swift" => include_str!("../assets/ficons/swift.svg"),
        "java" | "kt" => include_str!("../assets/ficons/java.svg"),
        "c" | "h" => include_str!("../assets/ficons/c.svg"),
        "cpp" | "hpp" | "cc" | "cxx" => include_str!("../assets/ficons/cpp.svg"),
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico" => {
            include_str!("../assets/ficons/image.svg")
        }
        "lock" => include_str!("../assets/ficons/lock.svg"),
        "gitignore" => include_str!("../assets/ficons/git.svg"),
        _ => include_str!("../assets/ficons/document.svg"),
    }
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
            span { class: "ficon-svg", dangerous_inner_html: material_icon(&name, is_dir) }
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
    // Binary files (PDF, images) are previewed via the wsimg asset handler — the
    // webview renders a PDF natively — rather than dumped as garbage text.
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let is_pdf = ext == "pdf";
    let is_img = matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp"
    );
    let preview_src = {
        // Path-encode for the asset URL (the handler percent-decodes it).
        let enc: String = path
            .display()
            .to_string()
            .bytes()
            .map(|b| match b {
                b'/' | b'.' | b'-' | b'_' | b'~' | b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect();
        format!("/wsimg/{enc}")
    };

    rsx! {
        div { class: "editor",
            div { class: "editor-head",
                span { class: "editor-title",
                    "{title}"
                    if dirty && !is_pdf && !is_img { span { class: "dot-dirty", "●" } }
                }
                div { class: "editor-actions",
                    if !is_pdf && !is_img {
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
                    }
                    button { class: "ed-close", onclick: move |_| ui.open_path.set(None), "Close" }
                }
            }
            if is_pdf {
                embed { class: "editor-pdf", src: "{preview_src}", "type": "application/pdf" }
            } else if is_img {
                div { class: "editor-imgwrap", img { class: "editor-img", src: "{preview_src}" } }
            } else {
                textarea {
                    class: "editor-area",
                    spellcheck: false,
                    value: "{ui.editor_text}",
                    oninput: move |e| { ui.editor_text.set(e.value()); ui.dirty.set(true); },
                }
            }
        }
    }
}

#[component]
fn SettingsModal(
    cfg: Signal<Config>,
    ui: Ui,
    engine: Coroutine<EngineCmd>,
    sessions_refresh: Signal<u64>,
    projects_list: Signal<Vec<ProjectGroup>>,
    initial_tab: String,
    mut automations: Signal<Vec<automation::AutomationSpec>>,
    mut automation_runs: Signal<Vec<automation::AutomationRunSpec>>,
    mut automation_name: Signal<String>,
    mut automation_schedule: Signal<String>,
    mut automation_prompt: Signal<String>,
    mut automation_status: Signal<String>,
    mut automation_confirm_delete: Signal<Option<String>>,
    mut hermes_profiles: Signal<Vec<hermes::HermesProfile>>,
    mut hermes_profile_name: Signal<String>,
    mut hermes_goal: Signal<String>,
    mut hermes_validation: Signal<String>,
    mut hermes_review_prompt: Signal<String>,
    mut hermes_status: Signal<String>,
    mut hermes_confirm_delete: Signal<Option<String>>,
    streaming: Signal<bool>,
    mut queue: Signal<Vec<String>>,
    on_close: EventHandler<()>,
) -> Element {
    let base = cfg.read().clone();
    let mut provider = use_signal(|| base.provider.clone());
    let mut harness = use_signal(|| base.harness.clone());
    let harness_opts = list_harnesses(&base);
    let mut model = use_signal(|| base.model.clone());
    let mut effort = use_signal(|| base.reasoning_effort.clone());
    let mut fast = use_signal(|| base.fast_mode);
    let mut bypass = use_signal(|| matches!(base.approval_policy, ApprovalPolicy::Never));
    let mut ws = use_signal(|| workspace_of(&base));
    let mut orchestrate = use_signal(|| base.orchestrate);
    let mut front = use_signal(|| base.front_provider.clone());
    let mut backend = use_signal(|| base.backend_provider.clone());
    let mut subagents = use_signal(|| base.subagents);
    let upd_url = use_signal(|| base.update_url.clone());
    let gh_repo = use_signal(|| {
        if base.github_repo.trim().is_empty() {
            "MANFIT7/oxide".to_string()
        } else {
            base.github_repo.clone()
        }
    });
    let mut upd_status = use_signal(|| "Up to date".to_string());
    let mut tab_mode = use_signal(|| base.default_tab_mode.clone());
    let mut browser_headless = use_signal(|| base.browser_headless);
    let mut notification_sound = use_signal(|| base.notification_sound);
    let mut notification_volume = use_signal(|| base.notification_volume);
    // Archived-sessions manager: bump to re-query the list; confirm holds the
    // id awaiting a second click before a permanent delete.
    let mut arch_refresh = use_signal(|| 0u64);
    let mut arch_confirm = use_signal(|| None::<String>);

    // Oxide is a GUI wrapper around the user's logged-in agent CLIs + the ChatGPT
    // subscription — no raw API-key providers (openai/anthropic) in the picker.
    let providers = ["chatgpt", "codex", "claude", "echo", "mock"];

    let save = move |_| {
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
        c.notification_sound = *notification_sound.read();
        c.notification_volume = *notification_volume.read();
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
        engine.send(EngineCmd::Reconfigure(c));
        on_close.call(());
    };

    let mut settings_tab = use_signal(|| initial_tab.clone());
    rsx! {
        div { class: "modal-overlay", onclick: move |_| on_close.call(()),
            div { class: "modal settings-modal", onclick: move |e| e.stop_propagation(),
                div { class: "modal-head",
                    h2 { "Settings" }
                    button { class: "term-x", onclick: move |_| on_close.call(()), Icon { name: "x" } }
                }
                div { class: "settings-tabs",
                    for (key, label) in [("model", "Model"), ("access", "Access"), ("agents", "Agents"), ("hermes", "Hermes"), ("automations", "Automations"), ("sessions", "Sessions"), ("updates", "Updates")] {
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
                            for preset in effort_levels(&cfg.read().provider).iter() {
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
                            span { class: "field-label", "Orchestrate (front planner to backend implementer)" }
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
                                span { class: "field-label", "Sub-agents (split plan into tool-capable workers, then synthesize)" }
                            }
                        }
                    }
                    label { class: "field toggle-field",
                        input { r#type: "checkbox", checked: *browser_headless.read(),
                            onchange: move |e| browser_headless.set(e.checked()) }
                        span { class: "field-label", "Browser automation runs headless (background)" }
                    }
                    label { class: "field toggle-field",
                        input { r#type: "checkbox", checked: *notification_sound.read(),
                            onchange: move |e| notification_sound.set(e.checked()) }
                        span { class: "field-label", "Play sound when a turn finishes (only when the window isn't focused)" }
                    }
                    if *notification_sound.read() {
                        label { class: "field",
                            span { class: "field-label", "Notification volume · {(*notification_volume.read() * 100.0).round() as i32}%" }
                            input { r#type: "range", class: "field-input", min: "0", max: "100", step: "1",
                                value: "{(*notification_volume.read() * 100.0).round() as i32}",
                                oninput: move |e| {
                                    let v = e.value().parse::<f32>().unwrap_or(48.0) / 100.0;
                                    notification_volume.set(v);
                                },
                                onchange: move |e| {
                                    // Preview the chime at the chosen volume on release.
                                    let v = e.value().parse::<f32>().unwrap_or(48.0) / 100.0;
                                    spawn(async move {
                                        let js = format!(
                                            r#"try {{ const a=(window.__oxideDoneAudio||new Audio('/notify-sound/done.wav')); window.__oxideDoneAudio=a; const c=(!a.paused&&a.currentTime>0)?a.cloneNode():a; c.volume={v}; c.currentTime=0; const p=c.play(); if(p&&p.catch)p.catch(()=>{{}}); }} catch(_){{}} return true;"#
                                        );
                                        let _ = dioxus::document::eval(&js).join::<bool>().await;
                                    });
                                },
                            }
                        }
                    }
                    label { class: "field",
                        span { class: "field-label", "Default mode (new tabs / next launch)" }
                        select { class: "field-input", onchange: move |e| tab_mode.set(e.value()),
                            option { value: "gui", selected: tab_mode.read().as_str() == "gui", "GUI (chat)" }
                            option { value: "tui", selected: tab_mode.read().as_str() == "tui", "TUI (terminal)" }
                        }
                    }
                  }
                  if settings_tab.read().as_str() == "hermes" {
                    div { class: "field cgpt-field",
                        span { class: "field-label", "Hermes evolve profile" }
                        span { class: "settings-hint", "Local-first Hermes lanes save under `.oxide/hermes-profiles` and run with the `hermes` harness." }
                        label { class: "field",
                            span { class: "field-label", "Profile name" }
                            input {
                                class: "field-input",
                                value: "{hermes_profile_name}",
                                oninput: move |e| hermes_profile_name.set(e.value())
                            }
                        }
                        label { class: "field",
                            span { class: "field-label", "Goal" }
                            textarea {
                                class: "field-input",
                                rows: "4",
                                value: "{hermes_goal}",
                                oninput: move |e| hermes_goal.set(e.value())
                            }
                        }
                        label { class: "field",
                            span { class: "field-label", "Validation command(s)" }
                            textarea {
                                class: "field-input",
                                rows: "3",
                                value: "{hermes_validation}",
                                oninput: move |e| hermes_validation.set(e.value())
                            }
                        }
                        label { class: "field",
                            span { class: "field-label", "Review gate" }
                            textarea {
                                class: "field-input",
                                rows: "3",
                                value: "{hermes_review_prompt}",
                                oninput: move |e| hermes_review_prompt.set(e.value())
                            }
                        }
                        div { class: "field-folder",
                            span { class: "folder-path", "{hermes_status}" }
                            button { class: "ed-save", onclick: move |_| {
                                let root = workspace_of(&cfg.read());
                                match hermes::profile_from_fields(
                                    hermes_profile_name.read().as_str(),
                                    hermes_goal.read().as_str(),
                                    hermes_validation.read().as_str(),
                                    hermes_review_prompt.read().as_str(),
                                    automation::now_ms(),
                                ) {
                                    Ok(profile) => match hermes::write_profile(&root, &profile) {
                                        Ok(()) => {
                                            hermes_profiles.set(hermes::read_profiles(&root).unwrap_or_default());
                                            hermes_confirm_delete.set(None);
                                            hermes_status.set(format!("Saved Hermes profile: {}", profile.name));
                                        }
                                        Err(err) => hermes_status.set(format!("Hermes profile save failed: {err}")),
                                    },
                                    Err(err) => hermes_status.set(err.to_string()),
                                }
                            }, "Save profile" }
                            button { class: "ed-close", onclick: move |_| {
                                let root = workspace_of(&cfg.read());
                                let goal = hermes_goal.read().clone();
                                let validation = hermes_validation.read().clone();
                                let status_sig = hermes_status;
                                spawn(async move {
                                    let context = hermes_diff_context(&root).await;
                                    let prompt = hermes::build_evolve_prompt(&goal, &validation, &context);
                                    submit_hermes_prompt(cfg, engine, streaming, status_sig, prompt, "Hermes evolve".to_string());
                                });
                            }, "Run evolve" }
                            button { class: "ed-close", onclick: move |_| {
                                let goal = hermes_goal.read().clone();
                                let validation = hermes_validation.read().clone();
                                let review = hermes_review_prompt.read().clone();
                                let prompt = hermes::build_review_prompt(&goal, &validation, &review);
                                submit_hermes_prompt(cfg, engine, streaming, hermes_status, prompt, "Hermes review".to_string());
                            }, "Run review" }
                        }
                    }
                    div { class: "field",
                        span { class: "field-label", "Saved Hermes profiles" }
                        if hermes_profiles.read().is_empty() {
                            div { class: "archived-empty", "No Hermes profiles saved yet." }
                        } else {
                            div { class: "archived-list",
                                for profile in hermes_profiles.read().iter().cloned() {
                                    {
                                        let apply_profile = profile.clone();
                                        let run_profile = profile.clone();
                                        let review_profile = profile.clone();
                                        let delete_profile = profile.clone();
                                        let confirm = hermes_confirm_delete.read().as_deref() == Some(profile.id.as_str());
                                        rsx! {
                                            div { class: "archived-folder",
                                                div { class: "archived-folder-head", title: "{profile.goal}",
                                                    Icon { name: "spark" }
                                                    span { class: "archived-folder-name", "{profile.name}" }
                                                    span { class: "archived-count", "hermes" }
                                                }
                                                div { class: "archived-row",
                                                    span { class: "archived-title", title: "{profile.validation}", "{profile.validation}" }
                                                    button { class: "archived-restore", onclick: move |_| {
                                                        hermes_profile_name.set(apply_profile.name.clone());
                                                        hermes_goal.set(apply_profile.goal.clone());
                                                        hermes_validation.set(apply_profile.validation.clone());
                                                        hermes_review_prompt.set(apply_profile.review_prompt.clone());
                                                        hermes_status.set(format!("Applied Hermes profile: {}", apply_profile.name));
                                                    }, "Apply" }
                                                    button { class: "archived-restore", onclick: move |_| {
                                                        let root = workspace_of(&cfg.read());
                                                        let goal = run_profile.goal.clone();
                                                        let validation = run_profile.validation.clone();
                                                        let display = format!("Hermes evolve · {}", run_profile.name);
                                                        let status_sig = hermes_status;
                                                        spawn(async move {
                                                            let context = hermes_diff_context(&root).await;
                                                            let prompt = hermes::build_evolve_prompt(&goal, &validation, &context);
                                                            submit_hermes_prompt(cfg, engine, streaming, status_sig, prompt, display);
                                                        });
                                                    }, "Run" }
                                                    button { class: "archived-restore", onclick: move |_| {
                                                        let prompt = hermes::build_review_prompt(&review_profile.goal, &review_profile.validation, &review_profile.review_prompt);
                                                        submit_hermes_prompt(cfg, engine, streaming, hermes_status, prompt, format!("Hermes review · {}", review_profile.name));
                                                    }, "Review" }
                                                    button { class: if confirm { "archived-del danger" } else { "archived-del" }, onclick: move |_| {
                                                        let root = workspace_of(&cfg.read());
                                                        if !confirm {
                                                            hermes_confirm_delete.set(Some(delete_profile.id.clone()));
                                                            return;
                                                        }
                                                        match hermes::delete_profile(&root, &delete_profile.id) {
                                                            Ok(()) => {
                                                                hermes_profiles.set(hermes::read_profiles(&root).unwrap_or_default());
                                                                hermes_confirm_delete.set(None);
                                                                hermes_status.set(format!("Deleted Hermes profile: {}", delete_profile.name));
                                                            }
                                                            Err(err) => hermes_status.set(format!("Hermes profile delete failed: {err}")),
                                                        }
                                                    }, if confirm { "Sure?" } else { "Delete" } }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                  }
                  if settings_tab.read().as_str() == "sessions" {
                    div { class: "field",
                        span { class: "field-label", "Archived sessions" }
                        span { class: "settings-hint", "Hidden from the sidebar — the underlying CLI session is untouched. Restore brings one back; Delete removes it permanently." }
                        {
                            let _ = *arch_refresh.read(); // re-query when this bumps
                            let archived = oxide_core::db::list_archived();
                            if archived.is_empty() {
                                rsx! { div { class: "archived-empty", "No archived sessions." } }
                            } else {
                                // Group by workspace, preserving recency order.
                                let mut groups: Vec<(String, Vec<oxide_core::db::SessionMeta>)> = Vec::new();
                                for m in archived {
                                    if let Some(g) = groups.iter_mut().find(|(w, _)| *w == m.workspace) {
                                        g.1.push(m);
                                    } else {
                                        groups.push((m.workspace.clone(), vec![m]));
                                    }
                                }
                                rsx! {
                                    div { class: "archived-list",
                                        for (wsname, items) in groups {
                                            {
                                                let folder = wsname.rsplit('/').find(|s| !s.is_empty()).unwrap_or(wsname.as_str()).to_string();
                                                let count = items.len();
                                                rsx! {
                                                    div { class: "archived-folder",
                                                        div { class: "archived-folder-head", title: "{wsname}",
                                                            Icon { name: "folder" }
                                                            span { class: "archived-folder-name", "{folder}" }
                                                            span { class: "archived-count", "{count}" }
                                                        }
                                                        for m in items {
                                                            {
                                                                let id_r = m.id.clone();
                                                                let id_d = m.id.clone();
                                                                let id_key = m.id.clone();
                                                                let titletext = if m.title.trim().is_empty() { "(untitled)".to_string() } else { m.title.clone() };
                                                                let confirming = arch_confirm.read().as_deref() == Some(id_key.as_str());
                                                                rsx! {
                                                                    div { class: "archived-row",
                                                                        span { class: "archived-title", title: "{titletext}", "{titletext}" }
                                                                        button { class: "archived-restore", onclick: move |_| {
                                                                            oxide_core::db::restore(&id_r);
                                                                            arch_confirm.set(None);
                                                                            arch_refresh.set(arch_refresh() + 1);
                                                                            sessions_refresh.set(sessions_refresh() + 1);
                                                                            refresh_projects_list(projects_list, cfg);
                                                                        }, "Restore" }
                                                                        button { class: if confirming { "archived-del danger" } else { "archived-del" }, onclick: move |_| {
                                                                            if !confirming {
                                                                                arch_confirm.set(Some(id_key.clone()));
                                                                                return;
                                                                            }
                                                                            oxide_core::db::delete(&id_d);
                                                                            arch_confirm.set(None);
                                                                            arch_refresh.set(arch_refresh() + 1);
                                                                            sessions_refresh.set(sessions_refresh() + 1);
                                                                        }, if confirming { "Sure?" } else { "Delete" } }
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
                  if settings_tab.read().as_str() == "automations" {
                    div { class: "field cgpt-field",
                        span { class: "field-label", "Create automation" }
                        span { class: "settings-hint", "Runs in this workspace using RRULE-style intervals: FREQ=MINUTELY, HOURLY, or DAILY with INTERVAL=N." }
                        label { class: "field",
                            span { class: "field-label", "Name" }
                            input {
                                class: "field-input",
                                value: "{automation_name}",
                                oninput: move |e| automation_name.set(e.value())
                            }
                        }
                        label { class: "field",
                            span { class: "field-label", "Schedule" }
                            input {
                                class: "field-input",
                                value: "{automation_schedule}",
                                oninput: move |e| automation_schedule.set(e.value())
                            }
                        }
                        label { class: "field",
                            span { class: "field-label", "Prompt" }
                            textarea {
                                class: "field-input",
                                rows: "5",
                                value: "{automation_prompt}",
                                oninput: move |e| automation_prompt.set(e.value())
                            }
                        }
                        div { class: "field-folder",
                            span { class: "folder-path", "{automation_status}" }
                            button { class: "ed-save", onclick: move |_| {
                                let root = workspace_of(&cfg.read());
                                let name = automation_name.read().trim().to_string();
                                let schedule = automation_schedule.read().trim().to_string();
                                let prompt = automation_prompt.read().trim().to_string();
                                if name.is_empty() || schedule.is_empty() || prompt.is_empty() {
                                    automation_status.set("Name, schedule, and prompt are required".to_string());
                                    return;
                                }
                                if automation::interval_ms(&schedule).is_none() {
                                    automation_status.set("Schedule must be FREQ=MINUTELY|HOURLY|DAILY;INTERVAL=N".to_string());
                                    return;
                                }
                                let spec = automation::new_spec(&name, &schedule, &prompt, automation::now_ms());
                                match automation::write_spec(&root, &spec) {
                                    Ok(()) => {
                                        automations.set(automation::read_specs(&root).unwrap_or_default());
                                        automation_runs.set(automation::read_runs(&root).unwrap_or_default());
                                        automation_confirm_delete.set(None);
                                        automation_status.set(format!("Saved automation: {}", spec.name));
                                    }
                                    Err(err) => automation_status.set(format!("Automation save failed: {err}")),
                                }
                            }, "Save automation" }
                        }
                    }
                    div { class: "field",
                        span { class: "field-label", "Saved automations" }
                        if automations.read().is_empty() {
                            div { class: "archived-empty", "No automations saved yet." }
                        } else {
                            div { class: "archived-list",
                                for spec in automations.read().iter().cloned() {
                                    {
                                        let latest = automation::latest_run(&automation_runs.read(), &spec.id)
                                            .map(|run| format!("last {} · {}", run.trigger, relative_ms(run.started_ms)))
                                            .unwrap_or_else(|| "never run".to_string());
                                        let spec_run = spec.clone();
                                        let spec_toggle = spec.clone();
                                        let spec_delete = spec.clone();
                                        let confirm = automation_confirm_delete.read().as_deref() == Some(spec.id.as_str());
                                        let status_class = if spec.status == "ACTIVE" { "archived-count" } else { "archived-del" };
                                        rsx! {
                                            div { class: "archived-folder",
                                                div { class: "archived-folder-head", title: "{spec.prompt}",
                                                    Icon { name: "target" }
                                                    span { class: "archived-folder-name", "{spec.name}" }
                                                    span { class: "{status_class}", "{spec.status}" }
                                                }
                                                div { class: "archived-row",
                                                    span { class: "archived-title", title: "{spec.schedule}", "{spec.schedule} · {latest}" }
                                                    button { class: "archived-restore", onclick: move |_| {
                                                        let root = workspace_of(&cfg.read());
                                                        run_automation_turn(
                                                            root,
                                                            spec_run.clone(),
                                                            "manual",
                                                            engine,
                                                            streaming,
                                                            queue,
                                                            automation_runs,
                                                            automation_status,
                                                        );
                                                    }, "Run now" }
                                                    button { class: "archived-restore", onclick: move |_| {
                                                        let root = workspace_of(&cfg.read());
                                                        let next = automation::with_toggled_status(&spec_toggle);
                                                        match automation::write_spec(&root, &next) {
                                                            Ok(()) => {
                                                                automations.set(automation::read_specs(&root).unwrap_or_default());
                                                                automation_confirm_delete.set(None);
                                                                automation_status.set(format!("{} automation: {}", if next.status == "ACTIVE" { "Activated" } else { "Paused" }, next.name));
                                                            }
                                                            Err(err) => automation_status.set(format!("Automation update failed: {err}")),
                                                        }
                                                    }, if spec.status == "ACTIVE" { "Pause" } else { "Activate" } }
                                                    button { class: if confirm { "archived-del danger" } else { "archived-del" }, onclick: move |_| {
                                                        let root = workspace_of(&cfg.read());
                                                        if !confirm {
                                                            automation_confirm_delete.set(Some(spec_delete.id.clone()));
                                                            return;
                                                        }
                                                        match automation::delete_spec(&root, &spec_delete.id) {
                                                            Ok(()) => {
                                                                automations.set(automation::read_specs(&root).unwrap_or_default());
                                                                automation_confirm_delete.set(None);
                                                                automation_status.set(format!("Deleted automation: {}", spec_delete.name));
                                                            }
                                                            Err(err) => automation_status.set(format!("Automation delete failed: {err}")),
                                                        }
                                                    }, if confirm { "Sure?" } else { "Delete" } }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if !automation_runs.read().is_empty() {
                        div { class: "field",
                            span { class: "field-label", "Recent runs" }
                            div { class: "archived-list",
                                for run in automation_runs.read().iter().take(8).cloned() {
                                    div { class: "archived-row",
                                        span { class: "archived-title", title: "{run.prompt}", "{run.automation_name} · {run.trigger}" }
                                        span { class: "archived-count", "{run.status} · {relative_ms(run.started_ms)}" }
                                    }
                                }
                            }
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
                    button { class: "ed-save", onclick: save, "Save" }
                }
            }
        }
    }
}

#[component]
fn Composer(
    streaming: Signal<bool>,
    engine: Coroutine<EngineCmd>,
    cfg: Signal<Config>,
    #[props(default)] followup: bool,
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
    let mut show_plus = use_signal(|| false);
    let mut show_access = use_signal(|| false);
    let mut mention_sel = use_signal(|| 0usize);
    // Long pastes become workspace-local .txt attachments.
    let mut text_attachments = use_signal(Vec::<TextAttachment>::new);
    let mut paste_seq = use_signal(|| 0u64);
    // `@mention` picker driven by the contenteditable caret query.
    let mut mention_q = use_signal(|| None::<String>);
    // Leading `/query` in the contenteditable — drives the slash-command menu.
    let mut slash_q = use_signal(|| None::<String>);
    // Cached @mention results — computed off-thread on query change, NOT per keystroke in render.
    let mut mention_items_sig = use_signal(Vec::<String>::new);
    // Branch-menu data loaded async on open (sync git subprocesses in render froze the UI).
    let mut branch_data = use_signal(|| (Vec::<(PathBuf, String)>::new(), Vec::<String>::new()));
    let mut ce_empty = use_signal(|| true);
    // Pasted image attachments (data URLs), shown as preview cards.
    let mut attachments = use_signal(Vec::<String>::new);
    // Full-screen image preview (lightbox) when a thumbnail is clicked.
    let mut preview_img = use_signal(|| None::<String>);
    let ws_paste = workspace.clone();
    // Intercept image paste into the composer as an attachment card (not inline).
    use_future(move || {
        let ws_paste = ws_paste.clone();
        async move {
            let mut eval = dioxus::document::eval(
                r#"
            const attach = function(el){
              if (!el || el.__oxpaste) return;
              el.__oxpaste = true;
              el.addEventListener('paste', function(ev){
                const cd = ev.clipboardData || window.clipboardData;
                const items = (cd || {}).items || [];
                for (const it of items) {
                  if (it.type && it.type.indexOf('image') === 0) {
                    ev.preventDefault();
                    const f = it.getAsFile();
                    const r = new FileReader();
                    r.onload = function(){ dioxus.send(r.result); };
                    r.readAsDataURL(f);
                    return;
                  }
                }
                // Plain-text paste — strip the source app's font/colors/styles so
                // pasted text uses Oxide's own composer styling.
                const text = cd ? cd.getData('text/plain') : '';
                if (text) {
                  ev.preventDefault();
                  const lines = text.split('\n').length;
                  if (text.length > 800 || lines > 12) {
                    // Long paste becomes a text attachment (full text kept on disk).
                    dioxus.send('PASTE:' + text);
                  } else {
                    document.execCommand('insertText', false, text);
                  }
                }
              });
            };
            // Self-healing: the composer remounts (hero <-> chat) and replaces the
            // #ce-input element, which silently dropped the paste listener. Keep
            // re-attaching to whatever element currently holds the id.
            while (true) {
              attach(document.getElementById('ce-input'));
              await new Promise(r => setTimeout(r, 700));
            }
            "#,
            );
            while let Ok(msg) = eval.recv::<String>().await {
                if let Some(text) = msg.strip_prefix("PASTE:") {
                    let id = *paste_seq.peek() + 1;
                    paste_seq.set(id);
                    match save_pasted_text_attachment(&ws_paste, id, text) {
                        Ok(att) => {
                            text_attachments.write().push(att);
                        }
                        Err(_) => {
                            let fallback = text.to_string();
                            spawn(async move {
                                let _ = dioxus::document::eval(&ce_insert_plain_text_js(&fallback))
                                    .join::<bool>()
                                    .await;
                            });
                        }
                    }
                    ce_empty.set(false);
                } else {
                    attachments.write().push(msg);
                }
            }
        }
    });
    let mention_items: Vec<String> = match mention_q.read().as_ref() {
        Some(_) => mention_items_sig.read().clone(),
        None => Vec::new(),
    };
    let mention_open = mention_q.read().is_some();
    let msel = if mention_items.is_empty() {
        0
    } else {
        (*mention_sel.read()).min(mention_items.len() - 1)
    };
    // `/slash` command picker — driven by the contenteditable's leading "/query".
    let slash_items: Vec<(String, String)> = match slash_q.read().as_ref() {
        Some(q) => slash_commands(&workspace, q),
        None => Vec::new(),
    };
    let ws_kd = workspace.clone();
    let ws_oninput = workspace.clone();
    let ws_branch_load = workspace.clone();
    // Context-usage ring (conic donut) shown in the composer toolbar.
    let ring_pct = if context_limit > 0 {
        (context_used as f64 / context_limit as f64 * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    let ring_style = format!(
        "background: conic-gradient(var(--accent) {p}%, color-mix(in srgb, var(--text) 18%, transparent) {p}% 100%)",
        p = ring_pct
    );
    let ring_num = format!("{}", ring_pct.round() as u64);
    let ring_title = if context_limit > 0 {
        format!(
            "{}% context used · {} / {} tokens",
            ring_pct.round() as u64,
            fmt_tokens(context_used),
            fmt_tokens(context_limit)
        )
    } else {
        "context usage — send a message to populate".to_string()
    };
    let access_cls = if bypass {
        "pill access danger"
    } else {
        "pill access"
    };
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
    let ce_placeholder = if *streaming.read() {
        "Steer the agent (sent mid-run)…"
    } else if followup {
        "Add a follow-up"
    } else {
        "Do anything"
    };

    rsx! {
        div { class: if *streaming.read() { "composer working" } else { "composer" },
            if !slash_items.is_empty() {
                div { class: "mention-menu",
                    div { class: "menu-label", "Commands" }
                    for (name, desc) in slash_items.iter().cloned() {
                        {
                            let n = name.clone();
                            rsx! {
                                button { class: "menu-item",
                                    onclick: move |_| {
                                        // Replace the editor content with "/cmd " (the editor is
                                        // a contenteditable — writing a signal would do nothing).
                                        let js = format!(
                                            "const e=document.getElementById('ce-input'); if(e){{ e.textContent={}; e.focus(); const r=document.createRange(); r.selectNodeContents(e); r.collapse(false); const s=window.getSelection(); s.removeAllRanges(); s.addRange(r); }} return true;",
                                            serde_json::to_string(&format!("/{n} ")).unwrap_or_default()
                                        );
                                        spawn(async move { let _ = dioxus::document::eval(&js).join::<bool>().await; });
                                        slash_q.set(None);
                                        ce_empty.set(false);
                                    },
                                    Icon { name: "spark" }
                                    span { class: "menu-name", "/{name}" }
                                    if !desc.is_empty() { span { class: "menu-meta", "{desc}" } }
                                }
                            }
                        }
                    }
                }
            }
            if mention_open {
                if !mention_items.is_empty() {
                    div { class: "mention-menu",
                        for (i, path) in mention_items.iter().cloned().enumerate() {
                            {
                                let p_sel = path.clone();
                                let is_mcp = path.starts_with("mcp:");
                                let is_skill = path.starts_with("skill:");
                                let is_ctx = path.starts_with("ctx:");
                                let is_automation = path.starts_with("automation:");
                                let disp = if is_automation {
                                    mention_label(&path)
                                } else {
                                    path.strip_prefix("mcp:")
                                        .or_else(|| path.strip_prefix("skill:"))
                                        .or_else(|| path.strip_prefix("ctx:"))
                                        .unwrap_or(&path)
                                        .to_string()
                                };
                                let icon_name = if is_ctx { "branch" } else if is_automation { "target" } else if is_mcp { "plugins" } else if is_skill { "target" } else if path.ends_with('/') { "folder" } else { "file" };
                                let grp = |p: &str| if p.starts_with("ctx:") { 0 } else if p.starts_with("automation:") { 1 } else if p.starts_with("mcp:") { 2 } else if p.starts_with("skill:") { 3 } else { 4 };
                                // Section header when the group changes.
                                let group = grp(&path);
                                let prev_group = if i == 0 { -1 } else { grp(&mention_items[i - 1]) };
                                let header = if group != prev_group {
                                    Some(match group { 0 => "Context", 1 => "Automations", 2 => "MCP servers", 3 => "Skills", _ => "Files" })
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
                                        if is_automation { span { class: "menu-tag", "auto" } }
                                        else if is_mcp { span { class: "menu-tag", "mcp" } }
                                        else if is_skill { span { class: "menu-tag", "skill" } }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if let Some(src) = preview_img.read().clone() {
                div { class: "img-lightbox", tabindex: "0",
                    onclick: move |_| preview_img.set(None),
                    onkeydown: move |e: dioxus::prelude::KeyboardEvent| {
                        if e.key() == dioxus::prelude::Key::Escape { preview_img.set(None); }
                    },
                    onmounted: move |el| { spawn(async move { let _ = el.set_focus(true).await; }); },
                    button { class: "img-lightbox-x", onclick: move |_| preview_img.set(None), Icon { name: "x" } }
                    img { class: "img-lightbox-img", src: "{src}", onclick: move |e| e.stop_propagation() }
                }
            }
            if !attachments.read().is_empty() || !text_attachments.read().is_empty() {
                div { class: "attach-row",
                    for (i, src) in attachments.read().iter().cloned().enumerate() {
                        div { class: "attach-card",
                            img { src: "{src}", onclick: { let s = src.clone(); move |_| preview_img.set(Some(s.clone())) } }
                            button { class: "attach-x", onclick: move |_| { let mut v = attachments.write(); if i < v.len() { v.remove(i); } }, Icon { name: "x" } }
                        }
                    }
                    for (i, att) in text_attachments.read().iter().cloned().enumerate() {
                        {
                            let ws_remove = workspace.clone();
                            rsx! {
                                div { class: "attach-file-card", title: "{att.rel_path}",
                                    Icon { name: "file" }
                                    div { class: "attach-file-main",
                                        div { class: "attach-file-name", "{att.name}" }
                                        div { class: "attach-file-meta", "{att.lines} lines · {att.chars} chars" }
                                    }
                                    button { class: "attach-x", onclick: move |_| {
                                        let mut v = text_attachments.write();
                                        if i < v.len() {
                                            let removed = v.remove(i);
                                            let _ = std::fs::remove_file(ws_remove.join(&removed.rel_path));
                                        }
                                    }, Icon { name: "x" } }
                                }
                            }
                        }
                    }
                }
            }
            if let Some(p) = picked_element.read().clone() {
                {
                    let label = p.lines().find_map(|l| l.strip_prefix("- selector: ")).unwrap_or("element").to_string();
                    rsx! {
                        div { class: "elem-chip", title: "{p}",
                            span { class: "elem-pin", Icon { name: "pin" } }
                            span { class: "elem-sel", "{label}" }
                            span { class: "elem-note", "will be sent to change" }
                            button { class: "elem-x", onclick: move |_| picked_element.set(None), Icon { name: "x" } }
                        }
                    }
                }
            }
            div {
                class: "input ce-input",
                id: "ce-input",
                contenteditable: "true",
                "data-empty": "{ce_empty}",
                "data-ph": "{ce_placeholder}",
                oninput: move |_| {
                    let ws_oninput = ws_oninput.clone();
                    spawn(async move {
                        let j = dioxus::document::eval(CE_QUERY_JS).join::<String>().await.unwrap_or_default();
                        let v: serde_json::Value = serde_json::from_str(&j).unwrap_or(serde_json::Value::Null);
                        // Only write signals when the value actually changed —
                        // otherwise every keystroke re-renders and the caret jitters.
                        let new_q = v["q"].as_str().map(|s| s.to_string());
                        if *mention_q.read() != new_q {
                            mention_q.set(new_q.clone());
                            mention_sel.set(0);
                            // Walk the workspace off-thread; render/keydown read the cache.
                            if let Some(q) = new_q {
                                let ws = ws_oninput.clone();
                                let q2 = q.clone();
                                let items = tokio::task::spawn_blocking(move || all_mention_items(&ws, &q2)).await.unwrap_or_default();
                                // Drop stale results — a slower walk for an older query
                                // must not overwrite the newer one.
                                if mention_q.peek().as_deref() == Some(q.as_str()) {
                                    mention_items_sig.set(items);
                                }
                            } else {
                                mention_items_sig.set(Vec::new());
                            }
                        }
                        let new_empty = v["empty"].as_bool().unwrap_or(true);
                        if *ce_empty.read() != new_empty {
                            ce_empty.set(new_empty);
                        }
                        let new_slash = v["slash"].as_str().map(|s| s.to_string());
                        if *slash_q.read() != new_slash {
                            slash_q.set(new_slash);
                        }
                    });
                },
                onkeydown: move |e| {
                    // When the @mention popup is open, the keyboard drives it.
                    let q = mention_q.read().clone();
                    if let Some(q) = q {
                        let _ = &q; let items = mention_items_sig.read().clone();
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
                        } else if e.key() == Key::Enter && !e.modifiers().shift() {
                            // Items still loading — don't let Enter submit a half-typed mention.
                            e.prevent_default();
                            return;
                        }
                    }
                    // Slash menu: Esc closes, Enter inserts the top match.
                    if slash_q.read().is_some() {
                        if e.key() == Key::Escape { e.prevent_default(); slash_q.set(None); return; }
                        if e.key() == Key::Enter && !e.modifiers().shift() {
                            let items = slash_commands(&ws_kd, &slash_q.read().clone().unwrap_or_default());
                            if let Some((name, _)) = items.first() {
                                e.prevent_default();
                                let js = format!(
                                    "const e=document.getElementById('ce-input'); if(e){{ e.textContent={}; e.focus(); const r=document.createRange(); r.selectNodeContents(e); r.collapse(false); const s=window.getSelection(); s.removeAllRanges(); s.addRange(r); }} return true;",
                                    serde_json::to_string(&format!("/{name} ")).unwrap_or_default()
                                );
                                spawn(async move { let _ = dioxus::document::eval(&js).join::<bool>().await; });
                                slash_q.set(None);
                                return;
                            }
                        }
                    }
                    if e.key() == Key::Enter && !e.modifiers().shift() {
                        if e.data().is_composing() {
                            // IME candidate confirmation (CJK) — not a send.
                            return;
                        }
                        e.prevent_default();
                        let ws = ws_kd2.clone();
                        spawn(async move { submit_ce(streaming, engine, plan_mode, pursue_goal, goal_text, queue, attachments, text_attachments, picked_element, false, ws).await; });
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
                                                let path = file.path().to_path_buf();
                                                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
                                                // Images become thumbnail attachments (like paste),
                                                // not a text path — they preview inside the chat.
                                                if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp") {
                                                    use base64::Engine;
                                                    let bytes = file.read().await;
                                                    let mime = match ext.as_str() { "jpg" | "jpeg" => "image/jpeg", "gif" => "image/gif", "webp" => "image/webp", _ => "image/png" };
                                                    let url = format!("data:{};base64,{}", mime, base64::engine::general_purpose::STANDARD.encode(&bytes));
                                                    attachments.write().push(url);
                                                } else {
                                                    let tok = path.display().to_string();
                                                    let label = mention_label(&tok);
                                                    let js = format!(
                                                        "const ed=document.getElementById('ce-input'); if(ed){{ed.focus(); const c=document.createElement('span'); c.className='ce-chip'; c.setAttribute('contenteditable','false'); c.dataset.token={}; c.textContent={}; ed.appendChild(c); ed.appendChild(document.createTextNode(' '));}} return true;",
                                                        serde_json::to_string(&tok).unwrap(), serde_json::to_string(&label).unwrap()
                                                    );
                                                    let _ = dioxus::document::eval(&js).join::<bool>().await;
                                                    ce_empty.set(false);
                                                }
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
                                        engine.send(EngineCmd::Reconfigure(c));
                                    },
                                    Icon { name: "spark" }
                                    span { class: "plus-name", title: "Two-stage: a planner delegates to an implementer, then reviews (plan to do to review)", "Orchestrate" }
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
                                            if matches!(ap, ApprovalPolicy::Always) { span { class: "menu-check", Icon { name: "check" } } }
                                        }
                                        button { class: "menu-item", onclick: move |_| set_access_mode(cfg, engine, show_access, ApprovalPolicy::OnRequest, SandboxPolicy::WorkspaceWrite),
                                            Icon { name: "terminal" }
                                            span { class: "menu-copy", span { class: "menu-name", "Approve for me" } span { class: "menu-meta", "Auto-run safe; ask for risky actions" } }
                                            if matches!(ap, ApprovalPolicy::OnRequest) { span { class: "menu-check", Icon { name: "check" } } }
                                        }
                                        button { class: "menu-item", onclick: move |_| set_access_mode(cfg, engine, show_access, ApprovalPolicy::Never, SandboxPolicy::DangerFullAccess),
                                            Icon { name: "zap" }
                                            span { class: "menu-copy", span { class: "menu-name", "Full access" } span { class: "menu-meta", "Unrestricted files + network (yolo)" } }
                                            if matches!(ap, ApprovalPolicy::Never) { span { class: "menu-check", Icon { name: "check" } } }
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
                            engine.send(EngineCmd::Reconfigure(c));
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
                                if model_count == 0 {
                                    div { class: "menu-empty", "No matching model" }
                                }
                                {
                                    let visible: Vec<&'static ModelPreset> = MODEL_PRESETS.iter().filter(|preset| model_matches(preset, &query)).collect();
                                    rsx! {
                                        for (gi, preset) in visible.iter().cloned().enumerate() {
                                            {
                                        // Section header when the provider group changes (synara-style).
                                        let header = if gi == 0 || visible[gi - 1].provider_label != preset.provider_label {
                                            Some(preset.provider_label)
                                        } else { None };
                                        let selected = preset.provider == cur_provider && preset.model == cur_model;
                                        let logo = provider_logo(preset.provider);
                                        let prov = preset.provider.to_string();
                                        let model = preset.model.to_string();
                                        let is_fast = preset.fast;
                                        rsx! {
                                            if let Some(h) = header {
                                                div { class: "menu-label model-group",
                                                    if let Some(svg) = provider_logo(preset.provider) { span { class: "prov-logo sm", dangerous_inner_html: svg } }
                                                    "{h}"
                                                }
                                            }
                                            button {
                                                class: if selected { "menu-item sel" } else { "menu-item" },
                                                onclick: move |_| {
                                                    // Keep the user's chosen effort + fast toggle on model switch.
                                                    let _ = is_fast;
                                                    let mut c = cfg.read().clone();
                                                    c.provider = prov.clone();
                                                    c.model = model.clone();
                                                    cfg.set(c.clone());
                                                    engine.send(EngineCmd::Reconfigure(c));
                                                    show_models.set(false);
                                                },
                                                if let Some(svg) = logo {
                                                    span { class: "prov-logo", dangerous_inner_html: svg }
                                                } else {
                                                    span { class: "prov-logo dot" }
                                                }
                                                span { class: "menu-copy",
                                                    span { class: "menu-name", "{preset.label}" }
                                                    span { class: "menu-meta", "{preset.model} · {preset.summary}" }
                                                }
                                                span { class: if preset.fast { "menu-badge fast" } else { "menu-badge" }, "{preset.badge}" }
                                                if selected { span { class: "menu-check", Icon { name: "check" } } }
                                            }
                                        }
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
                                for preset in effort_levels(&cfg.read().provider).iter() {
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
                                                    engine.send(EngineCmd::Reconfigure(c));
                                                    show_effort.set(false);
                                                },
                                                Icon { name: "brain" }
                                                span { class: "menu-copy",
                                                    span { class: "menu-name", "{preset.label}" }
                                                    span { class: "menu-meta", "{preset.summary}" }
                                                }
                                                if selected { span { class: "menu-check", Icon { name: "check" } } }
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
                        button { class: "send steer", title: "Steer (inject into the running turn)", onclick: move |_| { let ws = ws_steer.clone(); spawn(async move { submit_ce(streaming, engine, plan_mode, pursue_goal, goal_text, queue, attachments, text_attachments, picked_element, true, ws).await; }); }, Icon { name: "corner-up-right" } }
                        button { class: "send stop", title: "Stop", onclick: move |_| { engine.send(EngineCmd::Interrupt); }, Icon { name: "stop" } }
                    } else {
                        button { class: "send", onclick: move |_| { let ws = ws_btn.clone(); spawn(async move { submit_ce(streaming, engine, plan_mode, pursue_goal, goal_text, queue, attachments, text_attachments, picked_element, false, ws).await; }); }, Icon { name: "arrow-up" } }
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
                button { class: "selector", onclick: move |_| {
                    let v = *show_branch.read(); show_branch.set(!v); show_proj.set(false);
                    if !v {
                        let ws = ws_branch_load.clone();
                        spawn(async move {
                            let data = tokio::task::spawn_blocking(move || (git_worktrees(&ws), git_branches(&ws))).await.unwrap_or_default();
                            branch_data.set(data);
                        });
                    }
                },
                    Icon { name: "branch" } "{branch}" span { class: "chev", Icon { name: "chevron" } }
                }
                if *show_branch.read() {
                    div { class: "menu-backdrop", onclick: move |_| show_branch.set(false) }
                    {
                        let (worktrees, branches) = branch_data.read().clone();
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
fn DoneNote(text: String) -> Element {
    let (label, meta) = done_note_display_parts(&text);
    rsx! {
        div { class: "row note done-row",
            div { class: "note-text done-note",
                span { class: "done-icon", Icon { name: "check" } }
                span { class: "done-label", "{label}" }
                for item in meta {
                    span { class: "done-sep", "·" }
                    span { class: "done-meta", "{item}" }
                }
            }
        }
    }
}

#[component]
fn ToolNote(text: String) -> Element {
    let Some((icon, label)) = prefixed_icon_text(&text) else {
        return rsx! { div { class: "row note", div { class: "note-text", "{text}" } } };
    };
    rsx! {
        div { class: "row tool",
            div { class: "tool-card tool-note",
                span { class: "tool-note-icon", Icon { name: icon } }
                pre { class: "tool-note-text", "{label}" }
            }
        }
    }
}

#[component]
fn StatusPill(text: String, #[props(default)] elapsed_s: u64) -> Element {
    let label = if text.is_empty() {
        "Working…".to_string()
    } else {
        text
    };
    let icon_parts = icon_text(&label);
    let shown = icon_parts
        .as_ref()
        .map(|(_, label)| label.as_str())
        .unwrap_or(label.as_str());
    rsx! {
        div { class: "status-pill",
            span { key: "status-spin", class: "status-spinner" }
            if let Some((icon, _)) = icon_parts {
                span { key: "status-icon", class: "status-icon", Icon { name: icon } }
            }
            // Stable key: without it, the conditional status-icon appearing/
            // disappearing shifts this sibling positionally and Dioxus remounts
            // it — restarting the ox-shimmer gradient mid-sweep on every label/
            // icon change. Keyed, the label node is stable so the sweep is smooth.
            span { key: "status-label", class: "status-shimmer", "{shown}" }
            if elapsed_s >= 3 {
                {
                    let txt = if elapsed_s >= 3600 {
                        format!("{}h {}m", elapsed_s / 3600, (elapsed_s % 3600) / 60)
                    } else if elapsed_s >= 60 {
                        format!("{}m {}s", elapsed_s / 60, elapsed_s % 60)
                    } else {
                        format!("{elapsed_s}s")
                    };
                    rsx! { span { class: "status-elapsed", "· {txt}" } }
                }
            }
        }
    }
}

#[component]
fn Message(author: Author, text: String, #[props(default)] live: bool) -> Element {
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
                    button { class: "msg-copy", title: "Copy message", onclick: move |_| { let c = copy.clone(); spawn(async move { let _ = document::eval(&format!("navigator.clipboard.writeText({c})")).await; }); }, Icon { name: "copy" } }
                }
            }
        }
        Author::Agent => {
            if text.is_empty() {
                if live {
                    return rsx! {
                        div { class: "row agent agent-waiting",
                            img { class: "avatar", src: logo_uri() }
                            div { class: "typing", span {}, span {}, span {} }
                        }
                    };
                }
                // A stray placeholder left after a turn renders nothing.
                return rsx! {};
            }
            let copy = serde_json::to_string(&text).unwrap_or_default();
            let body_cls = if live {
                "agent-text agent-md live"
            } else {
                "agent-text agent-md"
            };
            rsx! {
                div { class: "row agent",
                    img { class: "avatar", src: logo_uri() }
                    div { class: "{body_cls}", dangerous_inner_html: md_to_html(&text, live) }
                    if !live {
                        button { class: "msg-copy", title: "Copy message", onclick: move |_| { let c = copy.clone(); spawn(async move { let _ = document::eval(&format!("navigator.clipboard.writeText({c})")).await; }); }, Icon { name: "copy" } }
                    }
                }
            }
        }
        Author::Activity { running, ok, .. } => rsx! { ActivityRow { text, running, ok } },
        Author::UiSpec => match parse_ui_spec_message(&text) {
            Ok(spec) => rsx! {
                div { class: "row agent ui-spec-row",
                    img { class: "avatar", src: logo_uri() }
                    UiSpecView { spec }
                }
            },
            Err(e) => rsx! {
                div { class: "row note",
                    div { class: "note-text", "Invalid UI spec: {e}" }
                }
            },
        },
        Author::Diff(..) => rsx! {},
        Author::Note => {
            if text.starts_with(DONE_NOTE_MARK) {
                rsx! { DoneNote { text } }
            } else if prefixed_icon_text(&text).is_some() {
                rsx! { ToolNote { text } }
            } else {
                rsx! { div { class: "row note", div { class: "note-text", "{text}" } } }
            }
        }
    }
}

/// Embedded terminal entry: opens the standalone native GPU terminal (oxide-term)
/// in a separate window running `bin` (codex / claude / shell) and shows a small
/// in-panel card. A wgpu/Metal surface can't render inside the Dioxus webview, so
/// the terminal is a sibling native window, not embedded in the panel.
#[component]
fn TerminalView(id: u64, bin: String, ws: String, resume: Option<String>) -> Element {
    let host = format!("term-{id}");
    let host_js = host.clone();
    use_future(move || {
        let host = host_js.clone();
        let bin = bin.clone();
        let ws = ws.clone();
        let resume = resume.clone();
        async move {
            // Inject the self-contained wterm bundle once (it declares `var
            // OxideWTerm`); dioxus wraps eval in an async fn, so re-attach it to
            // window explicitly or later terminals won't see it.
            let inject = format!(
                r#"if (!window.OxideWTerm) {{ {WTERM_JS}
                try {{ window.OxideWTerm = OxideWTerm; }} catch (e) {{}} }}"#
            );
            // Mount wterm into the host div and wire its I/O: keystrokes/responses
            // (onData) → dioxus.send, PTY output (dioxus.recv) → term.write.
            let body = format!(
                r##"
                for (let i = 0; i < 300 && !window.OxideWTerm; i++) {{ await new Promise(r => setTimeout(r, 20)); }}
                const el = document.getElementById("{host}");
                if (!el || !window.OxideWTerm) return;
                el.innerHTML = "";
                try {{ await document.fonts.load("12.5px 'JetBrainsMono Nerd Font Mono'"); await document.fonts.ready; }} catch (e) {{}}
                let term;
                try {{
                    term = new window.OxideWTerm.WTerm(el, {{
                        cols: 110, rows: 32,
                        autoResize: true,
                        cursorBlink: true,
                        onData: (d) => dioxus.send(JSON.stringify({{ inp: d }})),
                        onResize: (cols, rows) => dioxus.send(JSON.stringify({{ resize: [rows, cols] }})),
                    }});
                    await term.init();
                }} catch (e) {{ return; }}
                term.focus();
                // wterm's InputHandler already does copy (Cmd/Ctrl-C on a
                // selection falls through to the browser), paste (native paste
                // event, bracketed-paste aware), and click-to-focus. A custom
                // Cmd-V handler here previously ate the paste event and sent raw
                // unbracketed text, so claude/codex didn't register the paste.
                (async () => {{ while (true) {{ const m = await dioxus.recv(); if (typeof m === "string" && m.length) term.write(Uint8Array.from(atob(m), c => c.charCodeAt(0))); }} }})();
                "##
            );
            let mut eval = dioxus::document::eval(&format!("{inject}\n{body}"));

            let pty = portable_pty::native_pty_system();
            let pair = match pty.openpty(portable_pty::PtySize {
                rows: 32,
                cols: 110,
                pixel_width: 0,
                pixel_height: 0,
            }) {
                Ok(p) => p,
                Err(_) => return,
            };
            // Empty bin → a plain login shell; codex/claude → their TUI with
            // permissions bypassed (yolo), resuming the originating session.
            let shell = if bin.is_empty() {
                std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string())
            } else {
                bin.clone()
            };
            let mut cmd = portable_pty::CommandBuilder::new(&shell);
            match bin.as_str() {
                "codex" => {
                    cmd.arg("--dangerously-bypass-approvals-and-sandbox");
                    if let Some(sid) = &resume {
                        cmd.arg("resume");
                        cmd.arg(sid);
                    }
                }
                "claude" => {
                    cmd.arg("--dangerously-skip-permissions");
                    if let Some(sid) = &resume {
                        cmd.arg("--resume");
                        cmd.arg(sid);
                    }
                }
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
    rsx! { div { id: "{host}", class: "wterm-host", tabindex: "0" } }
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
    Split {
        id: u64,
        vertical: bool,
        ratio: f64,
        a: Box<Tile>,
        b: Box<Tile>,
    },
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
        Tile::Split {
            id,
            vertical: v,
            ratio,
            a,
            b,
        } => Tile::Split {
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
        Tile::Split {
            id,
            vertical,
            ratio,
            a,
            b,
        } => match (tile_close(a, target), tile_close(b, target)) {
            (None, Some(x)) | (Some(x), None) => Some(x),
            (Some(a), Some(b)) => Some(Tile::Split {
                id: *id,
                vertical: *vertical,
                ratio: *ratio,
                a: Box::new(a),
                b: Box::new(b),
            }),
            (None, None) => None,
        },
    }
}

/// Set the ratio of split `split_id`.
fn tile_set_ratio(node: &Tile, split_id: u64, ratio: f64) -> Tile {
    match node {
        Tile::Leaf(id) => Tile::Leaf(*id),
        Tile::Split {
            id,
            vertical,
            ratio: r,
            a,
            b,
        } => Tile::Split {
            id: *id,
            vertical: *vertical,
            ratio: if *id == split_id {
                ratio.clamp(0.12, 0.88)
            } else {
                *r
            },
            a: Box::new(tile_set_ratio(a, split_id, ratio)),
            b: Box::new(tile_set_ratio(b, split_id, ratio)),
        },
    }
}

/// Collect leaf ids in order.
fn tile_leaves(node: &Tile, out: &mut Vec<u64>) {
    match node {
        Tile::Leaf(id) => out.push(*id),
        Tile::Split { a, b, .. } => {
            tile_leaves(a, out);
            tile_leaves(b, out);
        }
    }
}

#[component]
fn UiSpecView(spec: UiSpec) -> Element {
    rsx! {
        div { class: "ui-spec",
            if let Some(title) = spec.title.clone() {
                div { class: "ui-spec-title", "{title}" }
            }
            UiNodeView { node: spec.root }
        }
    }
}

#[component]
fn UiNodeView(node: UiNode) -> Element {
    let props = node.props.clone();
    match node.kind {
        UiNodeKind::Stack => rsx! {
            div { class: "ui-node ui-stack",
                for child in node.children { UiNodeView { node: child } }
            }
        },
        UiNodeKind::Row => rsx! {
            div { class: "ui-node ui-row-spec",
                for child in node.children { UiNodeView { node: child } }
            }
        },
        UiNodeKind::Card => {
            let tone = ui_tone_class(props.tone);
            rsx! {
                div { class: "ui-node ui-card-spec {tone}",
                    if let Some(title) = props.title {
                        div { class: "ui-card-title", "{title}" }
                    }
                    if let Some(caption) = props.caption {
                        div { class: "ui-card-caption", "{caption}" }
                    }
                    if let Some(text) = props.text {
                        div { class: "ui-text", "{text}" }
                    }
                    for child in node.children { UiNodeView { node: child } }
                }
            }
        }
        UiNodeKind::Text => {
            let tone = ui_tone_class(props.tone);
            let text = props.text.or(props.value).unwrap_or_default();
            rsx! {
                div { class: "ui-node ui-text {tone}",
                    "{text}"
                }
            }
        }
        UiNodeKind::Metric => {
            let tone = ui_tone_class(props.tone);
            let label = props.label.unwrap_or_default();
            let value = props.value.unwrap_or_default();
            rsx! {
                div { class: "ui-node ui-metric {tone}",
                    if !label.is_empty() {
                        div { class: "ui-metric-label", "{label}" }
                    }
                    div { class: "ui-metric-value", "{value}" }
                    if let Some(caption) = props.caption {
                        div { class: "ui-metric-caption", "{caption}" }
                    }
                }
            }
        }
        UiNodeKind::Table => {
            let columns = props.columns;
            let rows = props.rows;
            rsx! {
                div { class: "ui-node ui-table-wrap",
                    table { class: "ui-table",
                        thead {
                            tr {
                                for column in columns.iter() {
                                    th { "{column.label}" }
                                }
                            }
                        }
                        tbody {
                            for row in rows {
                                tr {
                                    for column in columns.iter() {
                                        td { "{ui_value_display(row.get(&column.key))}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        UiNodeKind::Code => {
            let language = props.language.unwrap_or_else(|| "text".to_string());
            let text = props.text.unwrap_or_default();
            rsx! {
                div { class: "ui-node ui-code-block",
                    div { class: "ui-code-lang", "{language}" }
                    pre { "{text}" }
                }
            }
        }
        UiNodeKind::Alert => {
            let tone = ui_tone_class(props.tone);
            let title = props.title.unwrap_or_else(|| "Notice".to_string());
            let text = props.text.unwrap_or_default();
            rsx! {
                div { class: "ui-node ui-alert {tone}",
                    div { class: "ui-alert-title", "{title}" }
                    if !text.is_empty() {
                        div { class: "ui-alert-text", "{text}" }
                    }
                }
            }
        }
        UiNodeKind::Divider => rsx! {
            div { class: "ui-node ui-divider" }
        },
        UiNodeKind::Action => {
            if let Some(action) = props.action {
                let label = action.label.clone();
                let payload = serde_json::to_string(&serde_json::json!({
                    "name": action.name,
                    "payload": action.payload,
                }))
                .unwrap_or_default();
                let clipboard = serde_json::to_string(&payload).unwrap_or_default();
                rsx! {
                    button {
                        class: "ui-node ui-action",
                        title: "Copy action payload",
                        onclick: move |_| {
                            let c = clipboard.clone();
                            spawn(async move {
                                let _ = document::eval(&format!("navigator.clipboard.writeText({c})")).await;
                            });
                        },
                        "{label}"
                    }
                }
            } else {
                let label = props
                    .label
                    .or(props.text)
                    .unwrap_or_else(|| "Action".to_string());
                rsx! { div { class: "ui-node ui-action ghost", "{label}" } }
            }
        }
    }
}

fn ui_tone_class(tone: Option<UiTone>) -> &'static str {
    match tone.unwrap_or(UiTone::Neutral) {
        UiTone::Neutral => "neutral",
        UiTone::Info => "info",
        UiTone::Success => "success",
        UiTone::Warning => "warning",
        UiTone::Danger => "danger",
    }
}

fn ui_value_display(value: Option<&serde_json::Value>) -> String {
    match value {
        None | Some(serde_json::Value::Null) => String::new(),
        Some(serde_json::Value::Bool(v)) => v.to_string(),
        Some(serde_json::Value::Number(v)) => v.to_string(),
        Some(serde_json::Value::String(v)) => v.clone(),
        Some(other) => other.to_string(),
    }
}

#[component]
fn ActivityRow(text: String, running: bool, ok: bool) -> Element {
    let view = activity_view(&text);
    let state = if running {
        "running"
    } else if ok {
        "done"
    } else {
        "fail"
    };
    let cls = format!("activity-card {state} activity-{}", view.kind.class_name());
    let lines = if view.output.is_empty() {
        0
    } else {
        view.output.lines().count()
    };
    rsx! {
        div { class: "row activity",
            if view.output.is_empty() {
                div { class: "{cls}",
                    span { class: "activity-tic", Icon { name: icon_static(&view.icon) } }
                    if running { span { class: "activity-spin" } }
                    else if ok { span { class: "activity-ic ok", Icon { name: "check" } } }
                    else { span { class: "activity-ic fail", Icon { name: "x" } } }
                    span { class: "activity-verb", "{view.verb}" }
                    if !view.detail.is_empty() { span { class: "activity-text", "{view.detail}" } }
                }
            } else {
                details { class: "{cls} has-out",
                    summary { class: "activity-sum",
                        span { class: "activity-tic", Icon { name: icon_static(&view.icon) } }
                        if ok { span { class: "activity-ic ok", Icon { name: "check" } } } else { span { class: "activity-ic fail", Icon { name: "x" } } }
                        span { class: "activity-verb", "{view.verb}" }
                        if !view.detail.is_empty() { span { class: "activity-text", "{view.detail}" } }
                        span { class: "activity-out-n", "{lines} lines" }
                        button { class: "copy-btn", title: "Copy output",
                            onclick: {
                                let out = view.output.clone();
                                move |e: dioxus::prelude::MouseEvent| {
                                    e.prevent_default();
                                    e.stop_propagation();
                                    copy_text_to_clipboard(out.clone());
                                }
                            },
                            Icon { name: "copy" }
                        }
                    }
                    pre { class: "activity-out", "{view.output}" }
                }
            }
        }
    }
}

/// One coalesced file-edit row: the verb + path, an optional `×N` repeat badge
/// when the same file was edited multiple times in a row, and animated
/// `+adds −dels` line counts (the `countup` CSS animates the numbers up as more
/// edits land). Replaces N identical `Edit /path` rows in the activity stream
/// with a single entry, so the stream stops wasting space on repeated tools.
#[component]
fn EditActivityRow(
    text: String,
    running: bool,
    ok: bool,
    count: usize,
    adds: u32,
    dels: u32,
) -> Element {
    let view = activity_view(&text);
    let state = if running {
        "running"
    } else if ok {
        "done"
    } else {
        "fail"
    };
    let cls = format!("activity-card {state} activity-{}", view.kind.class_name());
    rsx! {
        div { class: "row activity",
            div { class: "{cls}",
                span { class: "activity-tic", Icon { name: icon_static(&view.icon) } }
                if running { span { class: "activity-spin" } }
                else if ok { span { class: "activity-ic ok", Icon { name: "check" } } }
                else { span { class: "activity-ic fail", Icon { name: "x" } } }
                span { class: "activity-verb", "{view.verb}" }
                if !view.detail.is_empty() { span { class: "activity-text", "{view.detail}" } }
                if count > 1 { span { class: "activity-count", "×{count}" } }
                if adds + dels > 0 {
                    span { class: "activity-editcounts",
                        span { class: "diff-adds countup plus", style: "--n:{adds}" }
                        " "
                        span { class: "diff-dels countup minus", style: "--n:{dels}" }
                    }
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
        "eye" => "eye",
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
        Tile::Split {
            id,
            vertical,
            ratio,
            a,
            b,
        } => {
            let na = *a;
            let nb = *b;
            let cls = if vertical {
                "split split-row"
            } else {
                "split split-col"
            };
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
    let label = if is_tui {
        format!("{target} · TUI")
    } else {
        target.clone()
    };
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
                    button { class: "pane-btn", title: "Split right", onclick: move |_| on_split.call(true), Icon { name: "split-right" } }
                    button { class: "pane-btn", title: "Split down", onclick: move |_| on_split.call(false), Icon { name: "split-down" } }
                    if closable {
                        button { class: "pane-btn", title: "Close pane", onclick: move |_| on_close.call(()), Icon { name: "x" } }
                    }
                }
            }
            if is_tui {
                TerminalView { id: pane_id, bin: target.clone(), ws: workspace.display().to_string(), resume: None }
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
        .arg("-C")
        .arg(ws)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .await
        .ok()
        .map(|o| o.status.success())
        .unwrap_or(false);
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
        .arg("-C")
        .arg(ws)
        .args(["worktree", "add", "-B", &branch])
        .arg(&wt)
        .arg("HEAD")
        .output()
        .await;
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
            .arg("-C")
            .arg(&ws)
            .args(["worktree", "remove", "--force"])
            .arg(&wt)
            .output()
            .await;
        let _ = tokio::process::Command::new("git")
            .arg("-C")
            .arg(&ws)
            .args(["branch", "-D", &format!("oxide/pane-{id}")])
            .output()
            .await;
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
        style { {WTERM_CSS} }
        div { class: "app pip-win", "data-theme": "{theme}",
            if mode == "tui" {
                TerminalView { id: 990_001, bin: bin.clone(), ws: workspace.display().to_string(), resume: None }
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
    // Pending ask_user question in this pane: (question, options).
    let mut pane_question = use_signal(|| None::<(String, Vec<String>)>);
    let mut streaming = use_signal(|| false);
    let mut thinking = use_signal(String::new);
    let mut status = use_signal(String::new);

    let p0 = provider.clone();
    let m0 = model.clone();
    let w0 = workspace.clone();
    let pane = use_coroutine(move |mut rx: UnboundedReceiver<PaneCmd>| {
        let (p, m, w) = (p0.clone(), m0.clone(), w0.clone());
        async move {
            // Unbounded: same rationale as the primary engine coroutine — a bounded
            // forwarder would back-propagate into core and stall the pane's turn.
            let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
            let mut cfg = Config::load().unwrap_or_default();
            // Isolate non-primary panes in their own git worktree so parallel
            // agents never clobber each other's working tree.
            let ws_eff = if isolate {
                pane_worktree(&w, pane_id)
                    .await
                    .unwrap_or_else(|| w.clone())
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
                            if tx.send(e).is_err() {
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
                        Some(Event::ToolCallDelta { call_id, tool, accumulated, .. }) => {
                            let mut m = messages.write();
                            upsert_tool_input_preview(&mut m, call_id, tool, accumulated);
                        }
                        Some(Event::ToolCallBegin { call_id, tool, args, .. }) => {
                            if tool != "ask_user" {
                                let text = activity_label(&tool, &args);
                                let idx = { let g = messages.read(); activity_idx(&g, &call_id) };
                                if let Some(idx) = idx {
                                    if let Some(c) = messages.write().get_mut(idx) {
                                        c.text = text;
                                        if let Author::Activity { running, ok, .. } = &mut c.author { *running = true; *ok = true; }
                                    }
                                } else {
                                    messages.write().push(ChatMsg { author: Author::Activity { running: true, ok: true, key: Some(call_id) }, text });
                                }
                            }
                        }
                        Some(Event::ToolCallEnd { call_id, output, ok, .. }) => {
                            let mut out = output.trim().to_string();
                            if out.chars().count() > 4000 { out = out.chars().take(4000).collect::<String>() + "\n… (truncated)"; }
                            let idx = { let g = messages.read(); activity_idx(&g, &call_id) };
                            if let Some(idx) = idx {
                                if let Some(c) = messages.write().get_mut(idx) {
                                    if let Author::Activity { running, ok: o, .. } = &mut c.author { *running = false; *o = ok; }
                                    if !out.is_empty() { c.text.push('\t'); c.text.push_str(&out); }
                                }
                            }
                        }
                        Some(Event::FileDiff { path, diff, checkpoint, .. }) => { messages.write().push(ChatMsg { author: Author::Diff(path, checkpoint), text: diff }); }
                        Some(Event::UiSpec { spec, .. }) => { messages.write().push(ui_spec_message(*spec)); }
                        Some(Event::TurnStarted { .. }) => { thinking.set(String::new()); status.set("Working…".to_string()); }
                        Some(Event::TurnFinished { .. }) => { streaming.set(false); status.set(String::new()); pane_question.set(None); { let mut mm = messages.write(); for c in mm.iter_mut() { if let Author::Activity { running, .. } = &mut c.author { *running = false; } } } }
                        Some(Event::Info { text }) => { if is_stage_status(&text) { status.set(text); } }
                        Some(Event::Error { message }) => { messages.write().push(ChatMsg { author: Author::Note, text: format!("error: {message}") }); streaming.set(false); }
                        Some(Event::QuestionAsked { question, options, .. }) => {
                            messages.write().push(ChatMsg { author: Author::Note, text: format!("Question: {question}") });
                            pane_question.set(Some((question, options)));
                        }
                        Some(Event::AuditLog { .. })
                        | Some(Event::SubagentStarted { .. })
                        | Some(Event::SubagentStatus { .. })
                        | Some(Event::SubagentFinished { .. }) => {}
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
                        summary { class: "thinking-sum", "Reasoning" }
                        div { class: "thinking-body", "{thinking}" }
                    }
                }
                if *streaming.read() && !status.read().is_empty() {
                    StatusPill { text: status.read().clone() }
                }
            }
            if let Some((q, opts)) = pane_question.read().clone() {
                div { class: "question-card",
                    div { class: "question-q", Icon { name: "help" } span { "{q}" } }
                    div { class: "question-opts",
                        for opt in opts {
                            {
                                let o = opt.clone();
                                rsx! {
                                    button { class: "question-opt", onclick: move |_| {
                                        pane.send(PaneCmd::Submit(o.clone()));
                                        pane_question.set(None);
                                    }, "{opt}" }
                                }
                            }
                        }
                    }
                }
            }
            div { class: "pane-composer",
                textarea { class: "input", placeholder: "Message…", value: "{input}",
                    oninput: move |e| input.set(e.value()),
                    onkeydown: move |e| if e.key() == Key::Enter && !e.modifiers().shift() {
                        e.prevent_default();
                        let t = input.read().trim().to_string();
                        if !t.is_empty() { input.set(String::new()); pane_question.set(None); pane.send(PaneCmd::Submit(t)); }
                    }
                }
                if *streaming.read() {
                    button { class: "send stop", onclick: move |_| pane.send(PaneCmd::Interrupt), Icon { name: "stop" } }
                } else {
                    button { class: "send", onclick: move |_| {
                        let t = input.read().trim().to_string();
                        if !t.is_empty() { input.set(String::new()); pane_question.set(None); pane.send(PaneCmd::Submit(t)); }
                    }, Icon { name: "arrow-up" } }
                }
            }
        }
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Word-level diff for a paired -/+ line: common prefix/suffix kept, the
/// changed middle wrapped in a highlight span (Cursor-style).
fn word_diff(old: &str, new: &str) -> (String, String) {
    let ob: Vec<char> = old.chars().collect();
    let nb: Vec<char> = new.chars().collect();
    let mut p = 0;
    while p < ob.len() && p < nb.len() && ob[p] == nb[p] {
        p += 1;
    }
    let mut sfx = 0;
    while sfx < ob.len() - p
        && sfx < nb.len() - p
        && ob[ob.len() - 1 - sfx] == nb[nb.len() - 1 - sfx]
    {
        sfx += 1;
    }
    let seg = |c: &[char], a: usize, b: usize| -> String { c[a..b].iter().collect() };
    let o_mid = seg(&ob, p, ob.len() - sfx);
    let n_mid = seg(&nb, p, nb.len() - sfx);
    let pre = seg(&ob, 0, p);
    let suf = seg(&ob, ob.len() - sfx, ob.len());
    (
        format!(
            "{}<span class=\"dw del\">{}</span>{}",
            esc(&pre),
            esc(&o_mid),
            esc(&suf)
        ),
        format!(
            "{}<span class=\"dw add\">{}</span>{}",
            esc(&pre),
            esc(&n_mid),
            esc(&suf)
        ),
    )
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
        let (tag, rest) = l.split_at(
            l.char_indices()
                .next()
                .map(|(_, c)| c.len_utf8())
                .unwrap_or(0)
                .min(l.len()),
        );
        match tag {
            " " => {
                old_block.push_str(rest);
                old_block.push('\n');
                new_block.push_str(rest);
                new_block.push('\n');
            }
            "-" => {
                old_block.push_str(rest);
                old_block.push('\n');
            }
            "+" => {
                new_block.push_str(rest);
                new_block.push('\n');
            }
            _ => {}
        }
    }
    let file = ws.join(path);
    let Ok(content) = std::fs::read_to_string(&file) else {
        return false;
    };
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
                                    span { class: "hunk-done icon-slot", Icon { name: "undo" } "reverted" }
                                } else {
                                    button { class: "hunk-revert", title: "Undo just this hunk in the file",
                                        onclick: move |_| { if revert_hunk(&ws2, &path2, &lines2) { reverted.write().insert(hi); } }, "Revert hunk" }
                                }
                            }
                            {
                                // Pair consecutive -/+ lines for word-level highlights.
                                let mut rows: Vec<(&'static str, String)> = Vec::new();
                                let mut i = 0;
                                while i < lines.len() {
                                    let l = &lines[i];
                                    if l.starts_with('-') && i + 1 < lines.len() && lines[i + 1].starts_with('+') {
                                        let (oh, nh) = word_diff(&l[1..], &lines[i + 1][1..]);
                                        rows.push(("dl del", format!("-{oh}")));
                                        rows.push(("dl add", format!("+{nh}")));
                                        i += 2;
                                        continue;
                                    }
                                    let cls = if l.starts_with('+') { "dl add" } else if l.starts_with('-') { "dl del" } else { "dl ctx" };
                                    rows.push((cls, esc(l)));
                                    i += 1;
                                }
                                rsx! {
                                    pre { class: "diff-body",
                                        for (cls, html) in rows {
                                            div { class: "{cls}", dangerous_inner_html: "{html}" }
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
fn Icon(name: &'static str) -> Element {
    let body = match name {
        "edit" => {
            rsx! { path { d: "M12 20h9" } path { d: "M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4z" } }
        }
        "search" => {
            rsx! { circle { cx: "11", cy: "11", r: "7" } line { x1: "21", y1: "21", x2: "16.65", y2: "16.65" } }
        }
        "eye" => {
            rsx! { path { d: "M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12z" } circle { cx: "12", cy: "12", r: "3" } }
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
        "plus" => {
            rsx! { line { x1: "12", y1: "5", x2: "12", y2: "19" } line { x1: "5", y1: "12", x2: "19", y2: "12" } }
        }
        "trash" => {
            rsx! { polyline { points: "3 6 5 6 21 6" } path { d: "M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" } }
        }
        "paperclip" => {
            rsx! { path { d: "M21 12.5l-8.5 8.5a5 5 0 0 1-7-7l9-9a3.3 3.3 0 0 1 4.7 4.7l-9 9a1.7 1.7 0 0 1-2.4-2.4l8-8" } }
        }
        "list" => rsx! {
            polyline { points: "3 6 4 7 6 5" }
            polyline { points: "3 12 4 13 6 11" }
            line { x1: "9", y1: "6", x2: "21", y2: "6" }
            line { x1: "9", y1: "12", x2: "21", y2: "12" }
            line { x1: "9", y1: "18", x2: "21", y2: "18" }
        },
        "target" => {
            rsx! { circle { cx: "12", cy: "12", r: "9" } circle { cx: "12", cy: "12", r: "5" } circle { cx: "12", cy: "12", r: "1" } }
        }
        "clock" => {
            rsx! { circle { cx: "12", cy: "12", r: "9" } polyline { points: "12 7 12 12 15 14" } }
        }
        "check" => rsx! { polyline { points: "20 6 9 17 4 12" } },
        "x" => rsx! {
            line { x1: "18", y1: "6", x2: "6", y2: "18" }
            line { x1: "6", y1: "6", x2: "18", y2: "18" }
        },
        "archive" => rsx! {
            rect { x: "3", y: "4", width: "18", height: "4", rx: "1" }
            path { d: "M5 8v11a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8" }
            path { d: "M10 12h4" }
        },
        "arrow-up" => rsx! {
            line { x1: "12", y1: "19", x2: "12", y2: "5" }
            polyline { points: "5 12 12 5 19 12" }
        },
        "arrow-down" => rsx! {
            line { x1: "12", y1: "5", x2: "12", y2: "19" }
            polyline { points: "19 12 12 19 5 12" }
        },
        "arrow-right" => rsx! {
            line { x1: "5", y1: "12", x2: "19", y2: "12" }
            polyline { points: "12 5 19 12 12 19" }
        },
        "corner-up-right" => rsx! {
            polyline { points: "15 14 20 9 15 4" }
            path { d: "M4 20v-7a4 4 0 0 1 4-4h12" }
        },
        "stop" => rsx! { rect { x: "7", y: "7", width: "10", height: "10", rx: "2" } },
        "undo" => rsx! {
            path { d: "M9 14 4 9l5-5" }
            path { d: "M4 9h10a6 6 0 1 1-4.2 10.2" }
        },
        "external-link" => rsx! {
            path { d: "M14 3h7v7" }
            path { d: "M21 3l-9 9" }
            path { d: "M17 14v5a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V9a2 2 0 0 1 2-2h5" }
        },
        "backspace" => rsx! {
            path { d: "M21 5H8l-5 7 5 7h13z" }
            line { x1: "18", y1: "9", x2: "12", y2: "15" }
            line { x1: "12", y1: "9", x2: "18", y2: "15" }
        },
        "play" => rsx! { polygon { points: "8 5 19 12 8 19 8 5" } },
        "refresh" => rsx! {
            path { d: "M21 12a9 9 0 0 1-15.5 6.2" }
            polyline { points: "3 18 5.5 18.2 5.7 15.6" }
            path { d: "M3 12A9 9 0 0 1 18.5 5.8" }
            polyline { points: "21 6 18.5 5.8 18.3 8.4" }
        },
        "help" => rsx! {
            circle { cx: "12", cy: "12", r: "9" }
            path { d: "M9.5 9a2.7 2.7 0 0 1 5.1 1.3c0 1.8-2.6 2.2-2.6 4" }
            circle { cx: "12", cy: "18", r: ".35" }
        },
        "alert" => rsx! {
            path { d: "M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z" }
            line { x1: "12", y1: "9", x2: "12", y2: "13" }
            circle { cx: "12", cy: "17", r: ".35" }
        },
        "split-right" => rsx! {
            rect { x: "3", y: "4", width: "18", height: "16", rx: "2" }
            line { x1: "12", y1: "4", x2: "12", y2: "20" }
        },
        "split-down" => rsx! {
            rect { x: "3", y: "4", width: "18", height: "16", rx: "2" }
            line { x1: "3", y1: "12", x2: "21", y2: "12" }
        },
        "flask" => rsx! {
            path { d: "M9 3h6" }
            path { d: "M10 3v5l-5 9a3 3 0 0 0 2.6 4.5h8.8A3 3 0 0 0 19 17l-5-9V3" }
            path { d: "M8 15h8" }
        },
        "hook" => rsx! {
            path { d: "M9 4v8a4 4 0 1 0 8 0V7" }
            path { d: "M13 4H9" }
            path { d: "M17 7h4" }
        },
        "camera" => rsx! {
            path { d: "M4 7h3l2-3h6l2 3h3a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V9a2 2 0 0 1 2-2z" }
            circle { cx: "12", cy: "13", r: "4" }
        },
        "tool" => {
            rsx! { path { d: "M14.7 6.3a4 4 0 0 0-5 5L3 18l3 3 6.7-6.7a4 4 0 0 0 5-5l-2.4 2.4-2.8-2.8z" } }
        }
        "browser" => rsx! {
            rect { x: "3", y: "4", width: "18", height: "16", rx: "2" }
            line { x1: "3", y1: "9", x2: "21", y2: "9" }
            circle { cx: "7", cy: "6.5", r: ".45" }
            circle { cx: "10", cy: "6.5", r: ".45" }
        },
        "git" => rsx! {
            circle { cx: "6", cy: "6", r: "2" }
            circle { cx: "18", cy: "18", r: "2" }
            circle { cx: "6", cy: "18", r: "2" }
            path { d: "M8 7.5 16.5 16" }
            path { d: "M6 8v8" }
        },
        "copy" => rsx! {
            rect { x: "9", y: "9", width: "13", height: "13", rx: "2" }
            path { d: "M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" }
        },
        "pin" => rsx! { path { d: "M9 3h6l-1 6 3 3v2h-5v5l-1 2-1-2v-5H4v-2l3-3-1-6z" } },
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
