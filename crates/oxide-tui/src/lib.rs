//! Terminal frontend for Oxide.
//!
//! Immediate-mode ratatui app whose event loop multiplexes terminal input
//! (crossterm `EventStream`) and engine [`Event`]s in one `tokio::select!`,
//! redrawing only when state changes. It owns no agent logic — it submits
//! [`Op`]s and renders the [`Event`] stream, exactly like the future GUI will.

use async_trait::async_trait;
use crossterm::event::{
    Event as CtEvent, EventStream, KeyCode, KeyEvent, KeyModifiers, MouseEventKind,
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
    /// Buffer for the assistant message currently streaming in.
    streaming: String,
    status: String,
    harness: String,
    /// Set while the engine is waiting for the user to approve a tool call.
    pending_approval: Option<u64>,
    /// The active model's context window (from the backend), for the status bar.
    context_limit: Option<u64>,
    last_input_tokens: u64,
    /// Rows scrolled up from the bottom (0 = follow the newest output). Clamped
    /// to the max offset in `draw` once the wrapped height is known.
    scroll: usize,
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
                    Some(Ok(CtEvent::Paste(s))) => { state.input.push_str(&s); dirty = true; }
                    Some(Ok(CtEvent::Mouse(m))) => {
                        match m.kind {
                            MouseEventKind::ScrollUp => { state.scroll = state.scroll.saturating_add(3); dirty = true; }
                            MouseEventKind::ScrollDown => { state.scroll = state.scroll.saturating_sub(3); dirty = true; }
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
                if dirty {
                    draw(terminal, state)?;
                    dirty = false;
                }
            }
        }
    }
}

async fn handle_key(key: KeyEvent, handle: &EngineHandle, state: &mut State) -> anyhow::Result<()> {
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

    match (key.modifiers, key.code) {
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => state.quit = true,
        (_, KeyCode::Esc) => {
            handle.submit(Op::Interrupt).await?;
            state.status = "interrupt sent".into();
        }
        (_, KeyCode::Enter) => {
            let text = state.input.trim().to_string();
            if !text.is_empty() {
                state.input.clear();
                // Snap to the bottom so the user sees their message + the reply.
                state.scroll = 0;
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
        (_, KeyCode::PageUp) => state.scroll = state.scroll.saturating_add(10),
        (_, KeyCode::PageDown) => state.scroll = state.scroll.saturating_sub(10),
        (_, KeyCode::Backspace) => {
            state.input.pop();
        }
        (_, KeyCode::Char(c)) => state.input.push(c),
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
        if state.scroll > max_off {
            state.scroll = max_off;
        }
        let off = (max_off - state.scroll) as u16;
        let scrolled = if state.scroll > 0 { " ↑" } else { "" };
        let transcript = para
            .block(
                RatBlock::default()
                    .borders(Borders::ALL)
                    .title(format!(" Oxide · {}{} ", state.harness, scrolled)),
            )
            .scroll((off, 0));
        frame.render_widget(transcript, chunks[0]);

        // Input box.
        let input = Paragraph::new(state.input.as_str())
            .block(RatBlock::default().borders(Borders::ALL).title(" message "));
        frame.render_widget(input, chunks[1]);

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
