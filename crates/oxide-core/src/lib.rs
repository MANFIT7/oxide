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

mod commands;
mod context;
mod hooks;
mod memory;
mod sandbox;
mod store;
mod tools;
pub use tools::{Routed, ToolRouter};

use oxide_config::Config;
use oxide_harness::{Harness, Registry};
use oxide_mcp::{is_mcp_tool, server_of, McpClient};
use oxide_protocol::{ApprovalDecision, Event, Op, ToolSpec, TurnId};
use oxide_providers::{Message, Provider, Role, StreamItem, TurnRequest};
use std::collections::HashSet;
use std::path::PathBuf;
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
        tools
    }

    /// Launch each configured MCP server and merge its tools. Failures are
    /// reported but never fatal — a missing server just means fewer tools.
    async fn connect_mcp_servers(&mut self) {
        for srv in self.config.mcp_servers.clone() {
            match McpClient::connect_stdio(&srv.name, &srv.command, &srv.args).await {
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

        // Keep the running history under the token budget before sending.
        let dropped = context::compact(&mut self.session, self.config.max_context_tokens, 6);
        if dropped > 0 {
            self.emit(Event::Compacted {
                dropped,
                tokens: context::estimate_tokens(&self.session),
            })
            .await;
        }

        let tools = self.all_tools();
        let mem_block = memory::Memory::new(&self.workspace).load_block();
        let harness = self.active_harness();
        let policy = harness.loop_policy();
        let mut sys = harness.system_prompt();
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
        let mut messages = vec![Message {
            role: Role::System,
            content: sys,
        }];
        messages.extend(self.session.iter().cloned());

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

        let req = TurnRequest {
            model: policy
                .model
                .clone()
                .unwrap_or_else(|| self.config.effective_model()),
            reasoning_effort: self.config.reasoning_effort.clone(),
            temperature: policy.temperature,
            messages,
            tools,
        };

        let (stream_tx, mut stream_rx) = mpsc::channel::<StreamItem>(STREAM_QUEUE);
        let provider = oxide_providers::build(&self.config.provider);
        let stream_task = tokio::spawn(async move { provider.stream(req, stream_tx).await });

        loop {
            tokio::select! {
                item = stream_rx.recv() => {
                    match item {
                        Some(StreamItem::TextDelta(t)) => {
                            assistant.push_str(&t);
                            self.emit(Event::AgentMessageDelta { turn, text: t }).await;
                        }
                        Some(StreamItem::ReasoningDelta(t)) => {
                            self.emit(Event::ReasoningDelta { turn, text: t }).await;
                        }
                        Some(StreamItem::ToolCall { name, arguments }) => {
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
                                self.emit(Event::ContextWindow { limit }).await;
                            }
                        }
                        Some(StreamItem::Done) | None => break,
                    }
                }
                op = op_rx.recv() => {
                    match op {
                        Some(Op::Interrupt) => { interrupted = true; break; }
                        Some(Op::Shutdown) => { interrupted = true; break; }
                        Some(other) => {
                            self.emit(Event::Info { text: format!("queued op ignored mid-turn: {other:?}") }).await;
                        }
                        None => break,
                    }
                }
            }
        }

        stream_task.abort();
        if !assistant.is_empty() {
            if let Some(store) = &self.session_store {
                let _ = store.append("assistant", &assistant);
            }
            self.session.push(Message {
                role: Role::Assistant,
                content: assistant,
            });
        }
        if interrupted {
            self.emit(Event::Info {
                text: "turn interrupted".into(),
            })
            .await;
        }
        self.fire_hooks("stop", serde_json::json!({})).await;
        self.emit(Event::TurnFinished { turn }).await;
    }

    /// Route one tool call through approval + sandbox and emit its result.
    /// Returns `true` if the turn was interrupted/shut down while waiting.
    async fn handle_tool_call(
        &mut self,
        turn: TurnId,
        name: String,
        arguments: serde_json::Value,
        op_rx: &mut mpsc::Receiver<Op>,
    ) -> bool {
        self.emit(Event::ToolCallBegin {
            turn,
            tool: name.clone(),
            args: arguments.clone(),
        })
        .await;

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

        // Snapshot the target before a write so the change can be rewound + diffed.
        let mut write_ctx: Option<(String, String, u64)> = None; // (path, prior, checkpoint)
        if name == "write_file" {
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

        // Memory tools (persistent + self-improvement), then MCP, then native sandbox.
        let (output, ok) = if name == "remember" {
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
        } else if is_mcp_tool(&name) {
            self.mcp_call(&name, &arguments).await
        } else {
            router.execute(&name, &arguments).await
        };
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
        if ok && name == "write_file" {
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
