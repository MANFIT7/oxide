//! Wire protocol for Oxide.
//!
//! The engine ([`oxide-core`]) and every frontend (TUI, GUI, headless runner,
//! IDE/RPC bridge) communicate only through these types. Frontends send [`Op`]s
//! into the engine and subscribe to the [`Event`] stream it emits. Nothing in
//! this crate depends on a runtime, a UI toolkit, or a provider — it is the
//! stable contract that keeps TUI and GUI interchangeable.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub use oxide_design::{
    DesignEdit, DesignPatchProposal, DesignReview, DesignReviewInput, DesignSelection,
    DesignSystem, DesignTokenContract,
};

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
    /// Control a specific sub-agent worker while a turn is running.
    SubagentControl {
        worker_id: String,
        action: SubagentControlAction,
    },
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
    /// Replace the conversation history (role, content) — used by "restore to
    /// this message", which trims the transcript so the model forgets the
    /// removed turns.
    SetHistory { msgs: Vec<(String, String)> },
    /// Graceful shutdown of the engine task.
    Shutdown,
}

/// Events the engine emits; frontends render these incrementally.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// Engine is up; carries the active harness id.
    Ready { harness: String },
    /// Path of the session file this engine opened (so the UI can bind the
    /// active tab to its exact transcript instead of guessing).
    SessionPath { path: String },
    /// Model-generated follow-up prompt suggestions for the composer.
    Followups { items: Vec<String> },
    /// A new turn began.
    TurnStarted { turn: TurnId },
    /// Authoritative working-state for the turn, pushed by the engine so a
    /// frontend renders it directly instead of inferring from lifecycle events
    /// (which is fragile — a dropped TurnFinished leaves a tab stuck "running").
    /// `state` ∈ "working" | "retrying" | "idle". Carries the reason for transient
    /// retries so the UI shows "retrying…" instead of an apparent freeze.
    TurnStatus {
        turn: TurnId,
        state: String,
        #[serde(default)]
        detail: String,
    },
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
        /// Provider-assigned call id, paired with the matching `ToolCallEnd`.
        call_id: String,
        tool: String,
        args: serde_json::Value,
    },
    /// A tool call's input is still streaming from the provider. Frontends may
    /// render this as a live preview; the engine executes only after
    /// `ToolCallBegin` with final parsed args.
    ToolCallDelta {
        turn: TurnId,
        call_id: String,
        tool: String,
        delta: String,
        accumulated: String,
    },
    /// A tool call finished.
    ToolCallEnd {
        turn: TurnId,
        /// Matches the `call_id` of the `ToolCallBegin` that opened this call.
        call_id: String,
        tool: String,
        output: String,
        ok: bool,
    },
    /// A shell/CLI command started. `worker_id` is set for sub-agent-owned commands.
    CommandStarted {
        turn: TurnId,
        command_id: String,
        worker_id: Option<String>,
        command: String,
        cwd: String,
        background: bool,
    },
    /// Incremental command output.
    CommandOutput {
        turn: TurnId,
        command_id: String,
        worker_id: Option<String>,
        stream: String,
        chunk: String,
    },
    /// A shell/CLI command finished.
    CommandFinished {
        turn: TurnId,
        command_id: String,
        worker_id: Option<String>,
        ok: bool,
        exit_code: Option<i32>,
        duration_ms: u64,
    },
    /// A file patch was applied to disk.
    PatchApplied { turn: TurnId, path: String },
    /// The agent's current task checklist `(content, status)` where status is
    /// "pending" | "in_progress" | "completed".
    Todos { items: Vec<(String, String)> },
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
    /// Durable audit row for tools, hooks, workflows, and verification.
    AuditLog {
        turn: Option<TurnId>,
        kind: String,
        title: String,
        detail: String,
        status: String,
    },
    /// A sub-agent worker started.
    SubagentStarted {
        turn: TurnId,
        worker_id: String,
        profile: String,
        task: String,
    },
    /// A sub-agent worker changed lifecycle state or received an operator action.
    SubagentStatus {
        turn: TurnId,
        worker_id: String,
        profile: String,
        status: String,
        detail: String,
    },
    /// A sub-agent worker finished.
    SubagentFinished {
        turn: TurnId,
        worker_id: String,
        profile: String,
        task: String,
        summary: String,
        ok: bool,
    },
    /// The agent is asking the user a question, optionally with choices.
    QuestionAsked {
        request_id: u64,
        question: String,
        options: Vec<String>,
    },
    /// Subscription usage snapshot (ChatGPT plan rate limits).
    RateLimit {
        plan: String,
        primary_pct: u8,
        secondary_pct: u8,
        primary_reset_s: u64,
        secondary_reset_s: u64,
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
        /// Of `input`, tokens served from the prompt cache (0 if unreported).
        #[serde(default)]
        cached_input: u64,
        /// Of `output`, reasoning tokens (0 if unreported).
        #[serde(default)]
        reasoning_output: u64,
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
    /// Agent requested the frontend Design Workbench to capture a visual target.
    DesignSnapshotRequested {
        turn: TurnId,
        url: String,
        note: String,
    },
    /// A selected element and proposed edits were converted into a typed design
    /// patch proposal. Frontends can render this as a review card before the
    /// agent applies source-code edits.
    DesignPatchProposed {
        turn: TurnId,
        proposal: Box<DesignPatchProposal>,
    },
    /// Deterministic review of a Design Workbench selection/edit set.
    DesignReviewCompleted {
        turn: TurnId,
        review: Box<DesignReview>,
    },
    /// A constrained, Rust-typed UI artifact spec that frontends may render
    /// natively. This is Oxide's json-render-style contract: the model can
    /// describe UI only through this catalog, never arbitrary HTML/JS.
    UiSpec { turn: TurnId, spec: Box<UiSpec> },
    /// The turn completed.
    TurnFinished { turn: TurnId },
    /// Free-form informational line for the transcript.
    Info { text: String },
    /// A recoverable or fatal error.
    Error { message: String },
    /// Engine task has stopped.
    Shutdown,
}

/// Operator actions that can be routed to a live sub-agent worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentControlAction {
    /// Interrupt only this worker, leaving sibling workers running.
    Interrupt,
    /// Add a steering instruction to only this worker's isolated context.
    Steer { text: String },
}

const UI_SPEC_MAX_DEPTH: usize = 8;
const UI_SPEC_MAX_NODES: usize = 80;
const UI_SPEC_MAX_TEXT_CHARS: usize = 4_000;
const UI_SPEC_MAX_COLUMNS: usize = 12;
const UI_SPEC_MAX_ROWS: usize = 80;
const UI_SPEC_MAX_CELL_CHARS: usize = 1_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub root: UiNode,
}

impl UiSpec {
    pub fn validate(&self) -> Result<(), String> {
        bounded_text("title", self.title.as_deref(), UI_SPEC_MAX_TEXT_CHARS)?;
        let mut nodes = 0usize;
        self.root.validate_inner(0, &mut nodes)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiNode {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub kind: UiNodeKind,
    #[serde(default)]
    pub props: UiProps,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<UiNode>,
}

impl UiNode {
    fn validate_inner(&self, depth: usize, nodes: &mut usize) -> Result<(), String> {
        if depth > UI_SPEC_MAX_DEPTH {
            return Err(format!("ui spec exceeds max depth {UI_SPEC_MAX_DEPTH}"));
        }
        *nodes += 1;
        if *nodes > UI_SPEC_MAX_NODES {
            return Err(format!("ui spec exceeds max nodes {UI_SPEC_MAX_NODES}"));
        }
        bounded_text("id", self.id.as_deref(), 120)?;
        self.props.validate(self.kind)?;
        self.validate_shape()?;
        for child in &self.children {
            child.validate_inner(depth + 1, nodes)?;
        }
        Ok(())
    }

    fn validate_shape(&self) -> Result<(), String> {
        match self.kind {
            UiNodeKind::Stack | UiNodeKind::Row | UiNodeKind::Card => Ok(()),
            UiNodeKind::Table if !self.children.is_empty() => {
                Err("ui table nodes cannot contain children".to_string())
            }
            UiNodeKind::Action if self.props.action.is_none() => {
                Err("ui action nodes require props.action".to_string())
            }
            UiNodeKind::Text
            | UiNodeKind::Metric
            | UiNodeKind::Code
            | UiNodeKind::Alert
            | UiNodeKind::Divider
            | UiNodeKind::Action
                if !self.children.is_empty() =>
            {
                Err(format!(
                    "ui {} nodes cannot contain children",
                    self.kind.as_str()
                ))
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiNodeKind {
    Stack,
    Row,
    Card,
    Text,
    Metric,
    Table,
    Code,
    Alert,
    Divider,
    Action,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct UiProps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<UiTone>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<UiTableColumn>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<BTreeMap<String, serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<UiAction>,
}

impl UiProps {
    fn validate(&self, kind: UiNodeKind) -> Result<(), String> {
        bounded_text("title", self.title.as_deref(), UI_SPEC_MAX_TEXT_CHARS)?;
        bounded_text("text", self.text.as_deref(), UI_SPEC_MAX_TEXT_CHARS)?;
        bounded_text("label", self.label.as_deref(), UI_SPEC_MAX_TEXT_CHARS)?;
        bounded_text("value", self.value.as_deref(), UI_SPEC_MAX_TEXT_CHARS)?;
        bounded_text("caption", self.caption.as_deref(), UI_SPEC_MAX_TEXT_CHARS)?;
        bounded_text("language", self.language.as_deref(), 40)?;
        if let Some(action) = &self.action {
            action.validate()?;
        }
        if self.columns.len() > UI_SPEC_MAX_COLUMNS {
            return Err(format!(
                "ui table exceeds max columns {UI_SPEC_MAX_COLUMNS}"
            ));
        }
        if self.rows.len() > UI_SPEC_MAX_ROWS {
            return Err(format!("ui table exceeds max rows {UI_SPEC_MAX_ROWS}"));
        }
        for column in &self.columns {
            column.validate()?;
        }
        for row in &self.rows {
            for (key, value) in row {
                bounded_text("row key", Some(key), 120)?;
                bounded_text(
                    "row value",
                    Some(&ui_value_to_string(value)),
                    UI_SPEC_MAX_CELL_CHARS,
                )?;
            }
        }
        match kind {
            UiNodeKind::Table => {
                if self.columns.is_empty() {
                    return Err("ui table requires columns".to_string());
                }
                if let Some(missing) = self
                    .rows
                    .iter()
                    .flat_map(|row| row.keys())
                    .find(|key| !self.columns.iter().any(|column| column.key == **key))
                {
                    return Err(format!("ui table row key has no column: {missing}"));
                }
                if self.action.is_some() {
                    return Err("ui table nodes cannot define action props".to_string());
                }
            }
            UiNodeKind::Action => {
                if self.action.is_none() {
                    return Err("ui action requires action props".to_string());
                }
                if !self.columns.is_empty() || !self.rows.is_empty() {
                    return Err("ui action nodes cannot define table props".to_string());
                }
            }
            _ => {
                if !self.columns.is_empty() || !self.rows.is_empty() {
                    return Err(format!(
                        "ui {} nodes cannot define table props",
                        kind.as_str()
                    ));
                }
                if self.action.is_some() {
                    return Err(format!(
                        "ui {} nodes cannot define action props",
                        kind.as_str()
                    ));
                }
            }
        }
        Ok(())
    }
}

impl UiNodeKind {
    fn as_str(self) -> &'static str {
        match self {
            UiNodeKind::Stack => "stack",
            UiNodeKind::Row => "row",
            UiNodeKind::Card => "card",
            UiNodeKind::Text => "text",
            UiNodeKind::Metric => "metric",
            UiNodeKind::Table => "table",
            UiNodeKind::Code => "code",
            UiNodeKind::Alert => "alert",
            UiNodeKind::Divider => "divider",
            UiNodeKind::Action => "action",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiTone {
    Neutral,
    Info,
    Success,
    Warning,
    Danger,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiTableColumn {
    pub key: String,
    pub label: String,
}

impl UiTableColumn {
    fn validate(&self) -> Result<(), String> {
        bounded_text("column key", Some(&self.key), 120)?;
        bounded_text("column label", Some(&self.label), 120)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiAction {
    pub name: String,
    pub label: String,
    #[serde(default)]
    pub payload: serde_json::Value,
}

impl UiAction {
    fn validate(&self) -> Result<(), String> {
        bounded_text("action name", Some(&self.name), 120)?;
        bounded_text("action label", Some(&self.label), 120)?;
        let payload = self.payload.to_string();
        bounded_text("action payload", Some(&payload), UI_SPEC_MAX_TEXT_CHARS)?;
        Ok(())
    }
}

fn bounded_text(name: &str, value: Option<&str>, max_chars: usize) -> Result<(), String> {
    if let Some(value) = value {
        let len = value.chars().count();
        if len > max_chars {
            return Err(format!("{name} exceeds max length {max_chars} chars"));
        }
    }
    Ok(())
}

fn ui_value_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::Bool(v) => v.to_string(),
        serde_json::Value::Number(v) => v.to_string(),
        serde_json::Value::String(v) => v.clone(),
        other => other.to_string(),
    }
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

    #[test]
    fn design_events_serialize_rust_native_contracts() {
        let snapshot = Event::DesignSnapshotRequested {
            turn: TurnId(9),
            url: "http://localhost:3000".to_string(),
            note: "Inspect hero".to_string(),
        };
        let snapshot_value = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(snapshot_value["event"], "design_snapshot_requested");
        assert_eq!(snapshot_value["url"], "http://localhost:3000");

        let proposal = DesignPatchProposal {
            selection: DesignSelection {
                selector: ".hero-title".to_string(),
                ..Default::default()
            },
            edits: vec![DesignEdit {
                property: "font-size".to_string(),
                old_value: "32px".to_string(),
                new_value: "40px".to_string(),
            }],
            instruction: "Keep tokenized.".to_string(),
        };
        let proposed = Event::DesignPatchProposed {
            turn: TurnId(10),
            proposal: Box::new(proposal),
        };
        let proposed_value = serde_json::to_value(&proposed).unwrap();
        assert_eq!(proposed_value["event"], "design_patch_proposed");
        assert_eq!(
            proposed_value["proposal"]["selection"]["selector"],
            ".hero-title"
        );
    }

    #[test]
    fn ui_spec_event_serializes_rust_native_contract() {
        let spec = UiSpec {
            title: Some("Build Health".to_string()),
            root: UiNode {
                id: Some("root".to_string()),
                kind: UiNodeKind::Card,
                props: UiProps {
                    title: Some("Build Health".to_string()),
                    ..Default::default()
                },
                children: vec![UiNode {
                    id: Some("tests".to_string()),
                    kind: UiNodeKind::Metric,
                    props: UiProps {
                        label: Some("Tests".to_string()),
                        value: Some("204 passed".to_string()),
                        tone: Some(UiTone::Success),
                        ..Default::default()
                    },
                    children: Vec::new(),
                }],
            },
        };
        spec.validate().unwrap();

        let event = Event::UiSpec {
            turn: TurnId(9),
            spec: Box::new(spec),
        };
        let value = serde_json::to_value(&event).unwrap();

        assert_eq!(value["event"], "ui_spec");
        assert_eq!(value["turn"], 9);
        assert_eq!(value["spec"]["root"]["type"], "card");
        assert_eq!(value["spec"]["root"]["children"][0]["type"], "metric");
        assert_eq!(
            value["spec"]["root"]["children"][0]["props"]["tone"],
            "success"
        );
    }

    #[test]
    fn ui_spec_validation_bounds_generated_ui() {
        let mut node = UiNode {
            id: None,
            kind: UiNodeKind::Stack,
            props: UiProps::default(),
            children: Vec::new(),
        };
        for _ in 0..10 {
            node = UiNode {
                id: None,
                kind: UiNodeKind::Stack,
                props: UiProps::default(),
                children: vec![node],
            };
        }
        let spec = UiSpec {
            title: None,
            root: node,
        };

        assert!(spec.validate().unwrap_err().contains("max depth"));
    }

    #[test]
    fn ui_spec_validation_rejects_invalid_action_and_table_shapes() {
        let action_without_payload = UiSpec {
            title: None,
            root: UiNode {
                id: None,
                kind: UiNodeKind::Action,
                props: UiProps::default(),
                children: Vec::new(),
            },
        };
        assert!(action_without_payload
            .validate()
            .unwrap_err()
            .contains("action"));

        let table_with_unknown_key = UiSpec {
            title: None,
            root: UiNode {
                id: None,
                kind: UiNodeKind::Table,
                props: UiProps {
                    columns: vec![UiTableColumn {
                        key: "name".to_string(),
                        label: "Name".to_string(),
                    }],
                    rows: vec![BTreeMap::from([(
                        "unknown".to_string(),
                        serde_json::json!("value"),
                    )])],
                    ..Default::default()
                },
                children: Vec::new(),
            },
        };
        assert!(table_with_unknown_key
            .validate()
            .unwrap_err()
            .contains("no column"));
    }
}
