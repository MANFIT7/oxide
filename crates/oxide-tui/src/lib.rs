//! Terminal frontend for Oxide.
//!
//! Immediate-mode ratatui app whose event loop multiplexes terminal input
//! (crossterm `EventStream`) and engine [`Event`]s in one `tokio::select!`,
//! redrawing only when state changes. It owns no agent logic — it submits
//! [`Op`]s and renders the [`Event`] stream, exactly like the future GUI will.

use async_trait::async_trait;
use crossterm::event::{Event as CtEvent, EventStream, KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt;
use oxide_core::EngineHandle;
use oxide_frontend::Frontend;
use oxide_protocol::{ApprovalDecision, Event, Op};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
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

#[derive(Default)]
struct State {
    transcript: Vec<Line<'static>>,
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
    quit: bool,
}

impl State {
    fn push(&mut self, line: Line<'static>) {
        self.transcript.push(line);
    }

    fn flush_streaming(&mut self) {
        if !self.streaming.is_empty() {
            let text = std::mem::take(&mut self.streaming);
            self.push(Line::from(vec![
                Span::styled(
                    "oxide ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(text),
            ]));
        }
    }
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
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste);
        ratatui::restore();
        res
    }
}

/// Render the most recent persisted session into the transcript so the TUI
/// opens on the last chat instead of a blank screen.
fn load_last_session(workspace: &std::path::Path, state: &mut State) {
    let dir = workspace.join(".oxide/sessions");
    let Ok(rd) = std::fs::read_dir(&dir) else { return };
    let mut files: Vec<std::path::PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("jsonl"))
        .collect();
    files.sort();
    let Some(latest) = files.last() else { return };
    let Ok(text) = std::fs::read_to_string(latest) else { return };
    let mut shown = 0;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
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
                Span::styled(prefix.to_string(), Style::default().fg(color).add_modifier(Modifier::BOLD)),
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
        Event::WorkflowSelected { title, steps, .. } => {
            state.push(Line::from(Span::styled(
                format!("workflow: {title}"),
                Style::default().fg(Color::Blue),
            )));
            for (i, step) in steps.iter().enumerate() {
                state.push(Line::from(format!("  {}. {step}", i + 1)));
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
        Event::ToolCallBegin { tool, .. } => {
            state.flush_streaming();
            state.push(Line::from(Span::styled(
                format!("⚙ {tool} …"),
                Style::default().fg(Color::Yellow),
            )));
        }
        Event::ToolCallEnd {
            tool, output, ok, ..
        } => {
            let color = if ok { Color::Yellow } else { Color::Red };
            state.push(Line::from(Span::styled(
                format!("⚙ {tool}: {output}"),
                Style::default().fg(color),
            )));
        }
        Event::Todos { .. } => {}
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
        Event::HookFired { hook, command, blocked } => state.push(Line::from(Span::styled(
            format!("hook {hook}: {command}{}", if blocked { " (blocked)" } else { "" }),
            Style::default().fg(Color::DarkGray),
        ))),
        Event::AuditLog { kind, title, status, .. } => state.push(Line::from(Span::styled(
            format!("audit {kind}: {status} · {title}"),
            Style::default().fg(Color::DarkGray),
        ))),
        Event::SubagentStarted { profile, task, .. } => state.push(Line::from(Span::styled(
            format!("subagent {profile}: {task}"),
            Style::default().fg(Color::Blue),
        ))),
        Event::SubagentFinished { profile, summary, ok, .. } => state.push(Line::from(Span::styled(
            format!("subagent {profile} {}: {summary}", if ok { "done" } else { "stopped" }),
            Style::default().fg(if ok { Color::Green } else { Color::Red }),
        ))),
        Event::RateLimit { plan, primary_pct, secondary_pct, .. } => {
            state.push(Line::from(Span::styled(
                format!("usage [{plan}] 5h {primary_pct}% · weekly {secondary_pct}%"),
                Style::default().fg(Color::DarkGray),
            )));
        }
        Event::QuestionAsked { question, options, .. } => {
            state.push(Line::from(Span::styled(format!("❓ {question}"), Style::default().fg(Color::Yellow))));
            for (i, o) in options.iter().enumerate() {
                state.push(Line::from(format!("  {}. {o}", i + 1)));
            }
        }
        Event::Shutdown => state.quit = true,
    }
}

fn draw(terminal: &mut ratatui::DefaultTerminal, state: &State) -> anyhow::Result<()> {
    terminal.draw(|frame| {
        let chunks = Layout::vertical([
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(frame.area());

        // Transcript (+ in-flight streaming line).
        let mut lines = state.transcript.clone();
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
        let visible = chunks[0].height.saturating_sub(2) as usize;
        let start = lines.len().saturating_sub(visible);
        let transcript = Paragraph::new(lines[start..].to_vec())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" Oxide · {} ", state.harness)),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(transcript, chunks[0]);

        // Input box.
        let input = Paragraph::new(state.input.as_str())
            .block(Block::default().borders(Borders::ALL).title(" message "));
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
