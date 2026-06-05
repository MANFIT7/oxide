//! Wire protocol for Oxide.
//!
//! The engine ([`oxide-core`]) and every frontend (TUI, GUI, headless runner,
//! IDE/RPC bridge) communicate only through these types. Frontends send [`Op`]s
//! into the engine and subscribe to the [`Event`] stream it emits. Nothing in
//! this crate depends on a runtime, a UI toolkit, or a provider — it is the
//! stable contract that keeps TUI and GUI interchangeable.

use serde::{Deserialize, Serialize};

/// A monotonically increasing identifier for a single agent turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TurnId(pub u64);

impl std::fmt::Display for TurnId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "turn-{}", self.0)
    }
}

/// Operations a frontend submits into the engine.
///
/// This is the *only* way a frontend drives the agent. Because it is a message
/// (not a blocking call), interrupts and multi-frontend control come for free.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// User submitted a prompt; run one agent turn.
    UserTurn { text: String },
    /// Stop the in-flight turn as soon as possible.
    Interrupt,
    /// Switch the active harness (e.g. "default" -> "hermes").
    SetHarness { id: String },
    /// Approve or reject a pending tool call (see [`Event::ApprovalRequested`]).
    ApprovalResponse {
        request_id: u64,
        decision: ApprovalDecision,
    },
    /// Restore the workspace to a prior checkpoint (see [`Event::CheckpointCreated`]).
    Rewind { checkpoint_id: u64 },
    /// Answer a question the agent asked (see [`Event::QuestionAsked`]).
    QuestionAnswer { request_id: u64, answer: String },
    /// Graceful shutdown of the engine task.
    Shutdown,
}

/// Events the engine emits; frontends render these incrementally.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// Engine is up; carries the active harness id.
    Ready { harness: String },
    /// A new turn began.
    TurnStarted { turn: TurnId },
    /// A chunk of the assistant's streamed message.
    AgentMessageDelta { turn: TurnId, text: String },
    /// Model reasoning/thinking delta (optional to render).
    ReasoningDelta { turn: TurnId, text: String },
    /// Engine wants to run a tool and is asking the frontend for approval.
    ApprovalRequested {
        request_id: u64,
        tool: String,
        summary: String,
    },
    /// A tool call started executing.
    ToolCallBegin {
        turn: TurnId,
        tool: String,
        args: serde_json::Value,
    },
    /// A tool call finished.
    ToolCallEnd {
        turn: TurnId,
        tool: String,
        output: String,
        ok: bool,
    },
    /// A file patch was applied to disk.
    PatchApplied { turn: TurnId, path: String },
    /// A reviewable unified diff for a file the agent changed.
    FileDiff {
        turn: TurnId,
        path: String,
        diff: String,
        /// Checkpoint id to rewind this change.
        checkpoint: u64,
    },
    /// A lifecycle hook ran.
    HookFired {
        hook: String,
        command: String,
        blocked: bool,
    },
    /// The agent is asking the user a question, optionally with choices.
    QuestionAsked {
        request_id: u64,
        question: String,
        options: Vec<String>,
    },
    /// A checkpoint was recorded before a mutating tool ran.
    CheckpointCreated {
        turn: TurnId,
        id: u64,
        label: String,
    },
    /// The workspace was restored to a checkpoint.
    RewindDone { id: u64, restored: u64 },
    /// Old context was compacted to stay under the token budget.
    Compacted { dropped: u64, tokens: u64 },
    /// Token accounting for the turn.
    TokensUsed {
        turn: TurnId,
        input: u64,
        output: u64,
    },
    /// The active model's context window size (tokens), reported by the backend.
    ContextWindow { limit: u64 },
    /// Active harness changed.
    HarnessChanged { id: String },
    /// MCP server connection/tool discovery status.
    McpServerStatus {
        name: String,
        status: String,
        tool_count: usize,
        tools: Vec<String>,
        detail: String,
    },
    /// Agent requested the frontend to focus/open a browser target.
    BrowserTargetChanged {
        turn: TurnId,
        url: String,
        note: String,
    },
    /// Agent requested a browser/appshot snapshot from the frontend.
    BrowserSnapshotRequested {
        turn: TurnId,
        url: String,
        note: String,
    },
    /// The turn completed.
    TurnFinished { turn: TurnId },
    /// Free-form informational line for the transcript.
    Info { text: String },
    /// A recoverable or fatal error.
    Error { message: String },
    /// Engine task has stopped.
    Shutdown,
}

/// How a tool call is gated before it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicy {
    /// Never run tools without explicit per-call user approval.
    Always,
    /// Auto-approve read-only/safe tools; ask for mutating ones.
    #[default]
    OnRequest,
    /// Auto-approve everything (still sandboxed unless full-access).
    Never,
}

/// A user's answer to an [`Event::ApprovalRequested`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approve,
    Reject,
    /// Approve this and auto-approve the same tool for the rest of the session.
    ApproveForSession,
}

/// The sandbox strength applied to tool execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxPolicy {
    /// Read-only filesystem, no network.
    ReadOnly,
    /// Write only within workspace roots; `.git`/config forced read-only; no net by default.
    #[default]
    WorkspaceWrite,
    /// No sandbox. Dangerous.
    DangerFullAccess,
}

/// Declarative description of a tool the model may call.
///
/// Both native tools and MCP tools are surfaced to the model as `ToolSpec`s and
/// routed through the same approval/sandbox chokepoint in the engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool's arguments.
    pub parameters: serde_json::Value,
    /// Whether the tool mutates state (used by [`ApprovalPolicy::OnRequest`]).
    #[serde(default)]
    pub mutating: bool,
}

impl ToolSpec {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters: serde_json::json!({ "type": "object", "properties": {} }),
            mutating: false,
        }
    }

    pub fn mutating(mut self, yes: bool) -> Self {
        self.mutating = yes;
        self
    }

    pub fn params(mut self, schema: serde_json::Value) -> Self {
        self.parameters = schema;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_target_changed_event_serializes_contract() {
        let event = Event::BrowserTargetChanged {
            turn: TurnId(7),
            url: "http://localhost:3000".to_string(),
            note: "Open login page".to_string(),
        };

        let value = serde_json::to_value(&event).unwrap();

        assert_eq!(value["event"], "browser_target_changed");
        assert_eq!(value["turn"], 7);
        assert_eq!(value["url"], "http://localhost:3000");
        assert_eq!(value["note"], "Open login page");
    }

    #[test]
    fn browser_snapshot_requested_event_serializes_contract() {
        let event = Event::BrowserSnapshotRequested {
            turn: TurnId(8),
            url: "http://localhost:3000/dashboard".to_string(),
            note: "Capture dashboard state".to_string(),
        };

        let value = serde_json::to_value(&event).unwrap();

        assert_eq!(value["event"], "browser_snapshot_requested");
        assert_eq!(value["turn"], 8);
        assert_eq!(value["url"], "http://localhost:3000/dashboard");
        assert_eq!(value["note"], "Capture dashboard state");
    }
}
