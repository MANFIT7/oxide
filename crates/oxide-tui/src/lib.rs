//! Terminal frontend for Oxide.
//!
//! Immediate-mode ratatui app whose event loop multiplexes terminal input
//! (crossterm `EventStream`) and engine [`Event`]s in one `tokio::select!`,
//! redrawing only when state changes. It owns no agent logic — it submits
//! [`Op`]s and renders the [`Event`] stream, exactly like the future GUI will.

use async_trait::async_trait;
use crossterm::event::{
    Event as CtEvent, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use futures::StreamExt;
use oxide_core::EngineHandle;
use oxide_frontend::Frontend;
use oxide_protocol::{ApprovalDecision, Event, Op};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block as RatBlock, Borders, Paragraph, Wrap};
use tokio::sync::mpsc;

/// The terminal UI. Construct with [`Tui::new`] and run via [`Frontend`].
pub struct Tui {
    harness: String,
    workspace: std::path::PathBuf,
}

impl Tui {
    pub fn new(harness: impl Into<String>, workspace: impl Into<std::path::PathBuf>) -> Self {
        Self {
            harness: harness.into(),
            workspace: workspace.into(),
        }
    }
}

/// Warp-style "block": a command/tool/message and its output grouped into one
/// addressable unit with a stable id, instead of a flat scrollback of lines. A
/// keyed block (tool call_id / command id) is UPDATED in place when its result
/// lands, so begin+end render as one block, not two stray lines.
#[derive(Clone, Copy, PartialEq, Debug)]
enum BlockStatus {
    /// A plain note/message with no run state (no gutter glyph).
    Plain,
    Running,
    Ok,
    Fail,
}

#[derive(Clone)]
struct Block {
    /// Stable block id — the addressability anchor for Phase-2 (block nav,
    /// select, collapse). Assigned now so the render path is ready for it.
    #[allow(dead_code)]
    id: u64,
    /// Stable id (tool call_id / command id) for in-place updates; None = not updatable.
    key: Option<String>,
    status: BlockStatus,
    /// Header content (gutter glyph is prepended at render).
    title: Line<'static>,
    /// Indented detail lines beneath the header.
    body: Vec<Line<'static>>,
}

impl Block {
    fn note(id: u64, title: Line<'static>) -> Self {
        Block {
            id,
            key: None,
            status: BlockStatus::Plain,
            title,
            body: Vec::new(),
        }
    }
}

#[derive(Default)]
struct State {
    blocks: Vec<Block>,
    next_block_id: u64,
    input: String,
    /// Caret position in `input`, in CHARS (not bytes) — the composer supports
    /// readline/mac editing (Cmd/Option+arrows, kill words), not append-only.
    cursor: usize,
    /// Buffer for the assistant message currently streaming in.
    streaming: String,
    status: String,
    harness: String,
    /// Set while the engine is waiting for the user to approve a tool call.
    pending_approval: Option<u64>,
    /// The active model's context window (from the backend), for the status bar.
    context_limit: Option<u64>,
    last_input_tokens: u64,
    /// Rows scrolled up from the bottom (0 = follow the newest output) — the
    /// value the easing glide moves TOWARD. Wheel/page input mutates this;
    /// it's clamped to the max offset in `draw` once the wrapped height is known.
    scroll_target: usize,
    /// Animated scroll position in fractional rows, eased toward
    /// `scroll_target` each frame tick and rendered rounded — so a wheel
    /// flick glides instead of jumping. Kept in sync when snapping.
    scroll_pos: f32,
    quit: bool,
}

impl State {
    fn next_id(&mut self) -> u64 {
        let id = self.next_block_id;
        self.next_block_id += 1;
        id
    }

    /// Append a standalone single-header block (the common case for notes,
    /// info, errors, todos, etc. — keeps every existing call site working).
    fn push(&mut self, line: Line<'static>) {
        let id = self.next_id();
        self.blocks.push(Block::note(id, line));
    }

    /// Start a fresh keyed/status block (tool, command) and return its index.
    fn push_block(
        &mut self,
        key: Option<String>,
        status: BlockStatus,
        title: Line<'static>,
    ) -> usize {
        let id = self.next_id();
        self.blocks.push(Block {
            id,
            key,
            status,
            title,
            body: Vec::new(),
        });
        self.blocks.len() - 1
    }

    /// Find a keyed block (newest first) for an in-place update.
    fn block_by_key(&mut self, key: &str) -> Option<&mut Block> {
        self.blocks
            .iter_mut()
            .rev()
            .find(|b| b.key.as_deref() == Some(key))
    }

    fn flush_streaming(&mut self) {
        if !self.streaming.is_empty() {
            let text = std::mem::take(&mut self.streaming);
            let id = self.next_id();
            self.blocks.push(Block {
                id,
                key: None,
                status: BlockStatus::Plain,
                title: Line::from(vec![
                    Span::styled(
                        "oxide ",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(text),
                ]),
                body: Vec::new(),
            });
        }
    }
}

/// Byte offset of char index `ci` in `s` (caret math is in chars, string ops
/// need bytes — indexing bytes directly would split UTF-8).
fn byte_at(s: &str, ci: usize) -> usize {
    s.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(s.len())
}

/// Readline-style word jump left: skip spaces, then the word before `ci`.
fn word_left(s: &str, ci: usize) -> usize {
    let chars: Vec<char> = s.chars().collect();
    let mut i = ci.min(chars.len());
    while i > 0 && chars[i - 1].is_whitespace() {
        i -= 1;
    }
    while i > 0 && !chars[i - 1].is_whitespace() {
        i -= 1;
    }
    i
}

/// Readline-style word jump right: skip spaces, then to the end of the word.
fn word_right(s: &str, ci: usize) -> usize {
    let chars: Vec<char> = s.chars().collect();
    let mut i = ci.min(chars.len());
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    while i < chars.len() && !chars[i].is_whitespace() {
        i += 1;
    }
    i
}

/// One 60fps animation step of the scroll glide: move `pos` toward `target`
/// by 35% of the remaining distance (exponential ease-out), snapping the last
/// sub-row step so it settles exactly. Returns true while still moving — the
/// caller keeps repainting until it settles, then the frame clock goes idle
/// again (no repaint when nothing changes).
fn ease_scroll(pos: &mut f32, target: f32) -> bool {
    let d = target - *pos;
    if d.abs() < 0.01 {
        return false;
    }
    *pos += if d.abs() < 0.5 { d } else { d * 0.35 };
    true
}

/// Flatten a block to render lines: a header (status gutter + title) and its
/// 2-space-indented body. A Plain block with no body renders as a single line,
/// identical to the old flat transcript — so notes look unchanged.
fn block_render_lines(b: &Block) -> Vec<Line<'static>> {
    let (gutter, gcolor) = match b.status {
        BlockStatus::Running => ("◐ ", Color::Yellow),
        BlockStatus::Ok => ("✓ ", Color::Green),
        BlockStatus::Fail => ("✗ ", Color::Red),
        BlockStatus::Plain => ("", Color::DarkGray),
    };
    let mut header: Vec<Span<'static>> = Vec::new();
    if !gutter.is_empty() {
        header.push(Span::styled(
            gutter,
            Style::default().fg(gcolor).add_modifier(Modifier::BOLD),
        ));
    }
    header.extend(b.title.spans.iter().cloned());
    let mut out = vec![Line::from(header)];
    for bl in &b.body {
        let mut spans = vec![Span::raw("  ")];
        spans.extend(bl.spans.iter().cloned());
        out.push(Line::from(spans));
    }
    out
}

#[async_trait]
impl Frontend for Tui {
    fn name(&self) -> &str {
        "tui"
    }

    async fn run(
        self: Box<Self>,
        handle: EngineHandle,
        mut events: mpsc::Receiver<Event>,
    ) -> anyhow::Result<()> {
        let mut terminal = ratatui::init();
        // Multi-line pastes arrive as one Paste event instead of N keystrokes.
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste);
        // Mouse capture lets the wheel scroll the transcript.
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture);
        // Kitty keyboard protocol (where supported: Ghostty/kitty/WezTerm/iTerm2):
        // without it macOS terminals never report the Cmd (SUPER) modifier, so
        // Cmd+←/→/Backspace can't work. Ignored by terminals that lack it.
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::PushKeyboardEnhancementFlags(
                crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
            )
        );
        let mut reader = EventStream::new();
        let mut state = State {
            harness: self.harness.clone(),
            status: "ready — type, Enter to send, Esc to interrupt, Ctrl-C to quit".into(),
            ..Default::default()
        };
        state.push(Line::from(Span::styled(
            "Oxide TUI — type a message, Enter to send · tool calls prompt y/n/a",
            Style::default().fg(Color::DarkGray),
        )));
        // Show the last conversation on open (the engine resumes its context).
        load_last_session(&self.workspace, &mut state);

        let res = run_loop(&mut terminal, &mut reader, &mut events, &handle, &mut state).await;
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::PopKeyboardEnhancementFlags
        );
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste);
        ratatui::restore();
        res
    }
}

/// Render the most recent persisted session into the transcript so the TUI
/// opens on the last chat instead of a blank screen.
fn load_last_session(workspace: &std::path::Path, state: &mut State) {
    let dir = workspace.join(".oxide/sessions");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut files: Vec<std::path::PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("jsonl"))
        .collect();
    files.sort();
    let Some(latest) = files.last() else { return };
    let Ok(text) = std::fs::read_to_string(latest) else {
        return;
    };
    let mut shown = 0;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let role = v["role"].as_str().unwrap_or("");
        let content = v["content"].as_str().unwrap_or("").trim();
        if content.is_empty() {
            continue;
        }
        let (label, color) = match role {
            "user" => ("you ", Color::Green),
            "assistant" => ("oxide ", Color::Cyan),
            _ => continue, // skip tool/system noise in the recap
        };
        for (i, l) in content.lines().enumerate() {
            let prefix = if i == 0 { label } else { "  " };
            state.push(Line::from(vec![
                Span::styled(
                    prefix.to_string(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(l.to_string()),
            ]));
        }
        shown += 1;
    }
    if shown > 0 {
        state.push(Line::from(Span::styled(
            "─── resumed last session ───",
            Style::default().fg(Color::DarkGray),
        )));
    }
}

async fn run_loop(
    terminal: &mut ratatui::DefaultTerminal,
    reader: &mut EventStream,
    events: &mut mpsc::Receiver<Event>,
    handle: &EngineHandle,
    state: &mut State,
) -> anyhow::Result<()> {
    // Coalesce redraws to a 60fps frame clock: events set `dirty`, the frame
    // tick repaints only if something changed. A fast token stream or a paste
    // burst then yields ~60 repaints/sec instead of one per token/keystroke.
    let mut frame = tokio::time::interval(std::time::Duration::from_millis(16));
    frame.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut dirty = true;
    loop {
        if state.quit {
            let _ = handle.submit(Op::Shutdown).await;
            return Ok(());
        }
        tokio::select! {
            term = reader.next() => {
                match term {
                    Some(Ok(CtEvent::Key(key))) => { handle_key(key, handle, state).await?; dirty = true; }
                    Some(Ok(CtEvent::Paste(s))) => {
                        let b = byte_at(&state.input, state.cursor);
                        state.input.insert_str(b, &s);
                        state.cursor += s.chars().count();
                        dirty = true;
                    }
                    Some(Ok(CtEvent::Mouse(m))) => {
                        // Only the TARGET moves here; the frame tick below eases the
                        // rendered position toward it, so fast flicks accumulate into
                        // one smooth glide instead of 3-row jumps.
                        match m.kind {
                            MouseEventKind::ScrollUp => { state.scroll_target = state.scroll_target.saturating_add(3); dirty = true; }
                            MouseEventKind::ScrollDown => { state.scroll_target = state.scroll_target.saturating_sub(3); dirty = true; }
                            _ => {}
                        }
                    }
                    Some(Ok(CtEvent::Resize(_, _))) => { dirty = true; }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e.into()),
                    None => state.quit = true,
                }
            }
            ev = events.recv() => {
                match ev {
                    Some(event) => { apply_event(event, state); dirty = true; }
                    None => state.quit = true,
                }
            }
            _ = frame.tick() => {
                // One easing step per frame while the scroll glide is in flight;
                // it keeps `dirty` set until the position settles on the target.
                if ease_scroll(&mut state.scroll_pos, state.scroll_target as f32) {
                    dirty = true;
                }
                if dirty {
                    draw(terminal, state)?;
                    dirty = false;
                }
            }
        }
    }
}

async fn handle_key(key: KeyEvent, handle: &EngineHandle, state: &mut State) -> anyhow::Result<()> {
    // With the kitty protocol active some terminals also report key releases.
    if key.kind == KeyEventKind::Release {
        return Ok(());
    }
    // While a tool approval is pending, keys answer the prompt.
    if let Some(request_id) = state.pending_approval {
        let decision = match key.code {
            KeyCode::Char('y') => Some(ApprovalDecision::Approve),
            KeyCode::Char('a') => Some(ApprovalDecision::ApproveForSession),
            KeyCode::Char('n') | KeyCode::Esc => Some(ApprovalDecision::Reject),
            _ => None,
        };
        if let Some(decision) = decision {
            state.pending_approval = None;
            state.status = format!("approval: {decision:?}");
            handle
                .submit(Op::ApprovalResponse {
                    request_id,
                    decision,
                })
                .await?;
        }
        return Ok(());
    }

    // Mac/readline composer editing. Cmd arrives as SUPER only on terminals
    // speaking the kitty protocol; Home/End + Ctrl-A/E cover the rest.
    let m = key.modifiers;
    let (ctrl, alt, sup) = (
        m.contains(KeyModifiers::CONTROL),
        m.contains(KeyModifiers::ALT),
        m.contains(KeyModifiers::SUPER),
    );
    let len = state.input.chars().count();
    state.cursor = state.cursor.min(len);
    match key.code {
        KeyCode::Char('c') if ctrl => state.quit = true,
        KeyCode::Esc => {
            handle.submit(Op::Interrupt).await?;
            state.status = "interrupt sent".into();
        }
        KeyCode::Enter => {
            let text = state.input.trim().to_string();
            if !text.is_empty() {
                state.input.clear();
                state.cursor = 0;
                // Snap to the bottom so the user sees their message + the reply.
                // Instant (no glide): this is intent to see NOW, not navigation.
                state.scroll_target = 0;
                state.scroll_pos = 0.0;
                state.push(Line::from(vec![
                    Span::styled(
                        "you   ",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(text.clone()),
                ]));
                handle.submit(Op::UserTurn { text }).await?;
            }
        }
        KeyCode::PageUp => state.scroll_target = state.scroll_target.saturating_add(10),
        KeyCode::PageDown => state.scroll_target = state.scroll_target.saturating_sub(10),
        // ── caret movement ──
        KeyCode::Left if sup => state.cursor = 0,
        KeyCode::Right if sup => state.cursor = len,
        KeyCode::Left if alt => state.cursor = word_left(&state.input, state.cursor),
        KeyCode::Right if alt => state.cursor = word_right(&state.input, state.cursor),
        KeyCode::Left => state.cursor = state.cursor.saturating_sub(1),
        KeyCode::Right => state.cursor = (state.cursor + 1).min(len),
        KeyCode::Home => state.cursor = 0,
        KeyCode::End => state.cursor = len,
        KeyCode::Char('a') if ctrl => state.cursor = 0,
        KeyCode::Char('e') if ctrl => state.cursor = len,
        // ── kill edits ──
        KeyCode::Backspace if sup => {
            // Cmd+Backspace: delete to line start.
            let b = byte_at(&state.input, state.cursor);
            state.input.replace_range(..b, "");
            state.cursor = 0;
        }
        KeyCode::Backspace if alt => {
            // Option+Backspace: delete the word before the caret.
            let start = word_left(&state.input, state.cursor);
            let (b0, b1) = (
                byte_at(&state.input, start),
                byte_at(&state.input, state.cursor),
            );
            state.input.replace_range(b0..b1, "");
            state.cursor = start;
        }
        KeyCode::Char('w') if ctrl => {
            let start = word_left(&state.input, state.cursor);
            let (b0, b1) = (
                byte_at(&state.input, start),
                byte_at(&state.input, state.cursor),
            );
            state.input.replace_range(b0..b1, "");
            state.cursor = start;
        }
        KeyCode::Char('u') if ctrl => {
            let b = byte_at(&state.input, state.cursor);
            state.input.replace_range(..b, "");
            state.cursor = 0;
        }
        KeyCode::Char('k') if ctrl => {
            let b = byte_at(&state.input, state.cursor);
            state.input.truncate(b);
        }
        KeyCode::Backspace => {
            if state.cursor > 0 {
                let b0 = byte_at(&state.input, state.cursor - 1);
                let b1 = byte_at(&state.input, state.cursor);
                state.input.replace_range(b0..b1, "");
                state.cursor -= 1;
            }
        }
        KeyCode::Delete => {
            if state.cursor < len {
                let b0 = byte_at(&state.input, state.cursor);
                let b1 = byte_at(&state.input, state.cursor + 1);
                state.input.replace_range(b0..b1, "");
            }
        }
        // Plain typing (SHIFT included); swallow Cmd/Ctrl/Alt chords so a
        // terminal that forwards e.g. Cmd+K doesn't type a literal 'k'.
        KeyCode::Char(c) if !ctrl && !alt && !sup => {
            let b = byte_at(&state.input, state.cursor);
            state.input.insert(b, c);
            state.cursor += 1;
        }
        _ => {}
    }
    Ok(())
}

fn apply_event(event: Event, state: &mut State) {
    match event {
        Event::Ready { harness } => {
            state.harness = harness;
            state.status = "engine ready".into();
        }
        Event::SessionPath { .. } => {}
        Event::Followups { .. } => {}
        Event::TurnStarted { turn } => state.status = format!("{turn} running…"),
        Event::TurnStatus {
            state: s, detail, ..
        } => {
            if s == "retrying" {
                state.status = if detail.is_empty() {
                    "retrying…".into()
                } else {
                    format!("retrying… ({detail})")
                };
            }
        }
        Event::ApprovalRequested {
            request_id,
            tool,
            summary,
        } => {
            state.flush_streaming();
            state.pending_approval = Some(request_id);
            state.push(Line::from(Span::styled(
                format!("? approve {tool}: {summary}   [y]es / [n]o / [a]lways"),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            state.status = "awaiting approval: y / n / a".into();
        }
        Event::AgentMessageDelta { text, .. } => state.streaming.push_str(&text),
        Event::ReasoningDelta { .. } => {}
        Event::ToolCallDelta { tool, .. } => {
            state.status = format!("preparing {tool} input…");
        }
        Event::ToolCallBegin { call_id, tool, .. } => {
            state.flush_streaming();
            // Open a running block keyed by call_id; ToolCallEnd updates THIS block
            // in place (begin+end = one block, not two stray lines).
            state.push_block(
                Some(call_id),
                BlockStatus::Running,
                Line::from(Span::styled(
                    format!("⚙ {tool}"),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )),
            );
        }
        Event::ToolCallEnd {
            call_id,
            tool,
            output,
            ok,
            ..
        } => {
            let status = if ok {
                BlockStatus::Ok
            } else {
                BlockStatus::Fail
            };
            let out = output.trim().to_string();
            if let Some(b) = state.block_by_key(&call_id) {
                b.status = status;
                if !out.is_empty() {
                    for l in out.lines() {
                        b.body.push(Line::from(Span::raw(l.to_string())));
                    }
                }
            } else {
                // No matching begin block (shell/ask_user, or merged) — standalone.
                let color = if ok { Color::Green } else { Color::Red };
                state.push(Line::from(Span::styled(
                    format!("⚙ {tool}: {out}"),
                    Style::default().fg(color),
                )));
            }
        }
        Event::CommandStarted {
            command_id,
            command,
            background,
            ..
        } => {
            state.flush_streaming();
            state.push_block(
                Some(command_id),
                BlockStatus::Running,
                Line::from(Span::styled(
                    format!("⌘ {}{command}", if background { "background " } else { "" }),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )),
            );
        }
        Event::CommandOutput {
            command_id,
            stream,
            chunk,
            ..
        } => {
            let text = chunk.trim_end();
            if !text.is_empty() {
                let line = if stream == "stderr" {
                    Line::from(Span::styled(
                        format!("stderr: {text}"),
                        Style::default().fg(Color::Red),
                    ))
                } else {
                    Line::from(Span::styled(
                        text.to_string(),
                        Style::default().fg(Color::DarkGray),
                    ))
                };
                if let Some(b) = state.block_by_key(&command_id) {
                    b.body.push(line);
                } else {
                    state.push(line);
                }
            }
        }
        Event::CommandFinished {
            command_id,
            ok,
            exit_code,
            duration_ms,
            ..
        } => {
            let footer = format!(
                "exit {} · {duration_ms}ms",
                exit_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "?".into())
            );
            if let Some(b) = state.block_by_key(&command_id) {
                b.status = if ok {
                    BlockStatus::Ok
                } else {
                    BlockStatus::Fail
                };
                b.body.push(Line::from(Span::styled(
                    footer,
                    Style::default().fg(if ok { Color::Green } else { Color::Red }),
                )));
            } else {
                state.push(Line::from(Span::styled(
                    format!("⌘ {} · {footer}", if ok { "done" } else { "failed" }),
                    Style::default().fg(if ok { Color::Green } else { Color::Red }),
                )));
            }
        }
        Event::Todos { items } => {
            let done = items
                .iter()
                .filter(|(_, status)| status == "completed")
                .count();
            state.push(Line::from(Span::styled(
                format!("todos {done}/{} done", items.len()),
                Style::default().fg(Color::Blue),
            )));
            for (content, status) in items {
                let mark = match status.as_str() {
                    "completed" => "[x]",
                    "in_progress" => "[>]",
                    _ => "[ ]",
                };
                state.push(Line::from(format!("  {mark} {content}")));
            }
        }
        Event::PatchApplied { path, .. } => state.push(Line::from(Span::styled(
            format!("✎ patched {path}"),
            Style::default().fg(Color::Magenta),
        ))),
        Event::CheckpointCreated { id, label, .. } => state.push(Line::from(Span::styled(
            format!("⎌ checkpoint #{id}: {label}"),
            Style::default().fg(Color::DarkGray),
        ))),
        Event::RewindDone { id, restored } => state.push(Line::from(Span::styled(
            format!("⎌ rewound to #{id} ({restored} file(s) restored)"),
            Style::default().fg(Color::Blue),
        ))),
        Event::Compacted { dropped, tokens } => state.push(Line::from(Span::styled(
            format!("∿ compacted: dropped {dropped} msg(s), ~{tokens} tokens"),
            Style::default().fg(Color::DarkGray),
        ))),
        Event::TokensUsed { input, output, .. } => {
            state.last_input_tokens = input;
            state.status = match state.context_limit {
                Some(limit) => format!("ctx {}k/{}k · out {output}", input / 1000, limit / 1000),
                None => format!("tokens in={input} out={output}"),
            };
        }
        Event::ContextWindow { limit } => {
            state.context_limit = Some(limit);
        }
        Event::HarnessChanged { id } => {
            state.harness = id.clone();
            state.push(Line::from(Span::styled(
                format!("→ harness: {id}"),
                Style::default().fg(Color::Blue),
            )));
        }
        Event::McpServerStatus {
            name,
            status,
            tool_count,
            detail,
            ..
        } => state.push(Line::from(Span::styled(
            format!("mcp {name}: {status} · {tool_count} tool(s) · {detail}"),
            Style::default().fg(if status == "connected" {
                Color::Green
            } else {
                Color::Red
            }),
        ))),
        Event::BrowserTargetChanged { url, note, .. } => state.push(Line::from(Span::styled(
            format!("browser target: {url} · {note}"),
            Style::default().fg(Color::Blue),
        ))),
        Event::BrowserSnapshotRequested { url, note, .. } => state.push(Line::from(Span::styled(
            format!("browser snapshot requested: {url} · {note}"),
            Style::default().fg(Color::Blue),
        ))),
        Event::DesignSnapshotRequested { url, note, .. } => state.push(Line::from(Span::styled(
            format!("design snapshot requested: {url} · {note}"),
            Style::default().fg(Color::Cyan),
        ))),
        Event::DesignPatchProposed { proposal, .. } => state.push(Line::from(Span::styled(
            format!(
                "design patch proposal: {} ({} edit(s))",
                proposal.selection.selector,
                proposal.edits.len()
            ),
            Style::default().fg(Color::Cyan),
        ))),
        Event::DesignReviewCompleted { review, .. } => state.push(Line::from(Span::styled(
            format!(
                "design review: ok={} score={} finding(s)={}",
                review.ok,
                review.score,
                review.findings.len()
            ),
            Style::default().fg(Color::Cyan),
        ))),
        Event::TurnFinished { .. } => {
            state.flush_streaming();
            state.status = "ready".into();
        }
        Event::Info { text } => state.push(Line::from(Span::styled(
            text,
            Style::default().fg(Color::DarkGray),
        ))),
        Event::Error { message } => state.push(Line::from(Span::styled(
            format!("error: {message}"),
            Style::default().fg(Color::Red),
        ))),
        Event::FileDiff { path, .. } => state.push(Line::from(Span::styled(
            format!("± diff {path}"),
            Style::default().fg(Color::Magenta),
        ))),
        Event::UiSpec { spec, .. } => {
            let title = spec
                .title
                .as_deref()
                .or(spec.root.props.title.as_deref())
                .unwrap_or("Untitled UI");
            state.push(Line::from(Span::styled(
                format!("▣ UI artifact: {title}"),
                Style::default().fg(Color::Blue),
            )));
        }
        Event::HookFired {
            hook,
            command,
            blocked,
        } => state.push(Line::from(Span::styled(
            format!(
                "hook {hook}: {command}{}",
                if blocked { " (blocked)" } else { "" }
            ),
            Style::default().fg(Color::DarkGray),
        ))),
        Event::AuditLog {
            kind,
            title,
            status,
            ..
        } => state.push(Line::from(Span::styled(
            format!("audit {kind}: {status} · {title}"),
            Style::default().fg(Color::DarkGray),
        ))),
        Event::SubagentStarted { profile, task, .. } => state.push(Line::from(Span::styled(
            format!("subagent {profile}: {task}"),
            Style::default().fg(Color::Blue),
        ))),
        Event::SubagentStatus {
            profile,
            status,
            detail,
            ..
        } => state.push(Line::from(Span::styled(
            format!("subagent {profile} {status}: {detail}"),
            Style::default().fg(Color::Blue),
        ))),
        Event::SubagentFinished {
            profile,
            summary,
            ok,
            ..
        } => state.push(Line::from(Span::styled(
            format!(
                "subagent {profile} {}: {summary}",
                if ok { "done" } else { "stopped" }
            ),
            Style::default().fg(if ok { Color::Green } else { Color::Red }),
        ))),
        Event::RateLimit {
            plan,
            primary_pct,
            secondary_pct,
            ..
        } => {
            state.push(Line::from(Span::styled(
                format!("usage [{plan}] 5h {primary_pct}% · weekly {secondary_pct}%"),
                Style::default().fg(Color::DarkGray),
            )));
        }
        Event::QuestionAsked {
            question, options, ..
        } => {
            state.push(Line::from(Span::styled(
                format!("❓ {question}"),
                Style::default().fg(Color::Yellow),
            )));
            for (i, o) in options.iter().enumerate() {
                state.push(Line::from(format!("  {}. {o}", i + 1)));
            }
        }
        Event::Shutdown => state.quit = true,
    }
}

fn draw(terminal: &mut ratatui::DefaultTerminal, state: &mut State) -> anyhow::Result<()> {
    terminal.draw(|frame| {
        let chunks = Layout::vertical([
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(frame.area());

        // Flatten blocks to render lines (+ the in-flight streaming line).
        let mut lines: Vec<Line<'static>> = Vec::new();
        for b in &state.blocks {
            lines.extend(block_render_lines(b));
        }
        if !state.streaming.is_empty() {
            lines.push(Line::from(vec![
                Span::styled(
                    "oxide ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(state.streaming.clone()),
            ]));
        }
        // Word wrap makes logical lines ≠ visual rows, so scroll by exact wrapped
        // rows: render the whole transcript and offset it. `scroll` counts rows up
        // from the bottom (0 = follow newest); clamp it to the real max here.
        let inner_w = chunks[0].width.saturating_sub(2);
        let inner_h = chunks[0].height.saturating_sub(2) as usize;
        let para = Paragraph::new(lines).wrap(Wrap { trim: false });
        let total = para.line_count(inner_w);
        let max_off = total.saturating_sub(inner_h);
        if state.scroll_target > max_off {
            state.scroll_target = max_off;
        }
        // The animated position renders rounded to whole rows (terminals can't
        // draw sub-row offsets); clamp it too so a shrinking transcript can't
        // leave the glide aimed past the top.
        state.scroll_pos = state.scroll_pos.clamp(0.0, max_off as f32);
        let off = (max_off as f32 - state.scroll_pos).round() as u16;
        let scrolled = if state.scroll_target > 0 { " ↑" } else { "" };
        let transcript = para
            .block(
                RatBlock::default()
                    .borders(Borders::ALL)
                    .title(format!(" Oxide · {}{} ", state.harness, scrolled)),
            )
            .scroll((off, 0));
        frame.render_widget(transcript, chunks[0]);

        // Input box: keep the caret visible by h-scrolling once the text is
        // wider than the box, and draw a real terminal cursor at the caret.
        let inner_iw = chunks[1].width.saturating_sub(2) as usize;
        let cur = state.cursor.min(state.input.chars().count());
        let xoff = cur.saturating_sub(inner_iw.saturating_sub(1)) as u16;
        let input = Paragraph::new(state.input.as_str())
            .scroll((0, xoff))
            .block(RatBlock::default().borders(Borders::ALL).title(" message "));
        frame.render_widget(input, chunks[1]);
        frame.set_cursor_position((
            chunks[1].x + 1 + (cur as u16).saturating_sub(xoff),
            chunks[1].y + 1,
        ));

        // Status line.
        let status = Paragraph::new(Line::from(Span::styled(
            state.status.clone(),
            Style::default().fg(Color::DarkGray),
        )));
        frame.render_widget(status, chunks[2]);
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxide_protocol::TurnId;

    #[test]
    fn tool_begin_and_end_collapse_into_one_block() {
        let mut state = State::default();
        apply_event(
            Event::ToolCallBegin {
                turn: TurnId(1),
                call_id: "c1".into(),
                tool: "read".into(),
                args: serde_json::Value::Null,
            },
            &mut state,
        );
        assert_eq!(state.blocks.len(), 1);
        assert_eq!(state.blocks[0].status, BlockStatus::Running);
        assert_eq!(state.blocks[0].key.as_deref(), Some("c1"));

        apply_event(
            Event::ToolCallEnd {
                turn: TurnId(1),
                call_id: "c1".into(),
                tool: "read".into(),
                output: "hello\nworld".into(),
                ok: true,
            },
            &mut state,
        );
        // Same block, updated in place — NOT a second stray block.
        assert_eq!(state.blocks.len(), 1);
        assert_eq!(state.blocks[0].status, BlockStatus::Ok);
        assert_eq!(state.blocks[0].body.len(), 2);
    }

    #[test]
    fn command_lifecycle_is_one_block() {
        let mut state = State::default();
        for ev in [
            Event::CommandStarted {
                turn: TurnId(1),
                command_id: "k1".into(),
                worker_id: None,
                command: "cargo build".into(),
                cwd: ".".into(),
                background: false,
            },
            Event::CommandOutput {
                turn: TurnId(1),
                command_id: "k1".into(),
                worker_id: None,
                stream: "stdout".into(),
                chunk: "compiling\n".into(),
            },
            Event::CommandFinished {
                turn: TurnId(1),
                command_id: "k1".into(),
                worker_id: None,
                ok: false,
                exit_code: Some(1),
                duration_ms: 42,
            },
        ] {
            apply_event(ev, &mut state);
        }
        assert_eq!(state.blocks.len(), 1);
        assert_eq!(state.blocks[0].status, BlockStatus::Fail);
        // output line + exit footer
        assert_eq!(state.blocks[0].body.len(), 2);
    }

    #[test]
    fn word_jumps_and_byte_offsets_handle_unicode() {
        let s = "halo dunia ✨ oxide";
        // word_left from the end lands at the start of "oxide".
        let n = s.chars().count();
        assert_eq!(word_left(s, n), n - 5);
        // word_left from just after "✨ " lands on the "✨" itself.
        assert_eq!(word_left(s, n - 6), n - 7);
        // word_right from 0 lands after "halo".
        assert_eq!(word_right(s, 0), 4);
        // byte_at respects multi-byte chars (✨ = 3 bytes).
        let ci = s.chars().position(|c| c == '✨').unwrap();
        assert_eq!(&s[byte_at(s, ci)..byte_at(s, ci + 1)], "✨");
        // Past-the-end char index clamps to the byte length.
        assert_eq!(byte_at(s, 999), s.len());
        assert_eq!(word_left(s, 0), 0);
        assert_eq!(word_right(s, 999), n);
    }

    #[test]
    fn ease_scroll_converges_and_settles_exactly() {
        let mut pos = 0.0_f32;
        let target = 30.0_f32;
        let mut steps = 0;
        while ease_scroll(&mut pos, target) {
            steps += 1;
            assert!(pos <= target, "no overshoot: {pos}");
            assert!(steps < 120, "must settle within ~2s of frames");
        }
        assert_eq!(pos, target, "settles EXACTLY on the target row");
        // Idle after settling — the frame clock must go back to no-repaint.
        assert!(!ease_scroll(&mut pos, target));
    }

    #[test]
    fn ease_scroll_glides_down_too() {
        let mut pos = 50.0_f32;
        while ease_scroll(&mut pos, 0.0) {}
        assert_eq!(pos, 0.0);
    }

    #[test]
    fn unkeyed_tool_end_without_begin_is_standalone() {
        let mut state = State::default();
        apply_event(
            Event::ToolCallEnd {
                turn: TurnId(1),
                call_id: "missing".into(),
                tool: "shell".into(),
                output: "ok".into(),
                ok: true,
            },
            &mut state,
        );
        assert_eq!(state.blocks.len(), 1);
        assert!(state.blocks[0].key.is_none());
    }
}
