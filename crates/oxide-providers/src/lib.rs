//! Provider abstraction.
//!
//! Every backend (OpenAI, Anthropic Messages, local) implements [`Provider`]
//! and streams normalized [`StreamItem`]s back through a channel. The engine
//! never sees provider-specific wire formats. [`EchoProvider`] needs no API key
//! and is used for offline/dev runs and tests.

mod anthropic;
mod catalog;
mod chatgpt;
mod cli;
mod openai;
pub use catalog::{
    default_model_for_provider, diagnose_provider, diagnose_providers, fast_model_for_provider,
    list_provider_capabilities, list_provider_models, list_providers, provider_info,
    DiagnosticStatus, ProviderAuth, ProviderCapability, ProviderDiagnostic, ProviderInfo,
    ProviderKind, ProviderModel, ProviderStability,
};
pub use cli::{
    claude_persistent_close, claude_persistent_interrupt, ClaudeCliProvider,
    ClaudeInteractiveProvider, ClaudePersistentProvider, CodexCliProvider,
};

use async_trait::async_trait;
use oxide_protocol::ToolSpec;
use tokio::sync::mpsc;

/// A tool call the assistant made, carried structurally so providers can emit a
/// proper `function_call`/`tool_use` item (with a stable id) on replay instead
/// of flattening it to text — which is what makes the model re-plan/re-call.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// One message in the conversation sent to the model.
#[derive(Debug, Clone)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// For an assistant message: the tool call it issued (paired with a later
    /// `Tool` message whose `tool_call_id` matches `tool_call.id`).
    pub tool_call: Option<ToolCall>,
    /// For a `Tool` message: which assistant tool call this is the result of.
    pub tool_call_id: Option<String>,
    /// Raw provider reasoning item (e.g. OpenAI Responses `reasoning` with
    /// `encrypted_content`) replayed verbatim so the model doesn't re-think
    /// from scratch every agentic round.
    pub reasoning_item: Option<serde_json::Value>,
}

impl Message {
    /// A plain text message (no tool call).
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_call: None,
            tool_call_id: None,
            reasoning_item: None,
        }
    }
    /// An assistant message that issued a tool call.
    pub fn with_tool_call(content: impl Into<String>, call: ToolCall) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_call: Some(call),
            tool_call_id: None,
            reasoning_item: None,
        }
    }
    /// A tool result paired to the assistant call `id`.
    pub fn tool_result(content: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_call: None,
            tool_call_id: Some(id.into()),
            reasoning_item: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A single model-call request, assembled by the engine from the active harness.
#[derive(Debug, Clone)]
pub struct TurnRequest {
    pub model: String,
    pub reasoning_effort: String,
    pub temperature: f32,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    /// Workspace directory CLI-driver providers run in (API providers ignore it).
    pub cwd: String,
    /// Stable id for THIS conversation (session file id). CLI drivers key their
    /// resume map on it so two tabs in one workspace don't share a CLI session.
    pub conversation_id: String,
    /// The provider's native CLI session id (codex thread / claude uuid) persisted
    /// from a previous run, so a resume after an app restart reattaches to the
    /// real CLI session instead of starting fresh. None = no prior link.
    pub cli_resume: Option<String>,
    /// Harness persona/policy to append to an external agent CLI's own system
    /// prompt (claude `--append-system-prompt`). The CLI analog of a Managed-
    /// Agents `agent_with_overrides` `system` override. None = leave the CLI
    /// agent's prompt untouched (the default for every non-opted-in harness).
    pub system_append: Option<String>,
    /// Custom subagents for an external agent CLI (claude `--agents <json>`).
    /// None = no override.
    pub claude_agents: Option<serde_json::Value>,
}

/// Normalized streaming output. Each provider maps its SSE events to these.
#[derive(Debug, Clone)]
pub enum StreamItem {
    /// A chunk of assistant text.
    TextDelta(String),
    /// A CLI-driver reported it changed this file (engine diffs it at turn end).
    FileChanged(String),
    /// A chunk of reasoning/thinking text.
    ReasoningDelta(String),
    /// A complete provider reasoning item (opaque, e.g. encrypted_content) to
    /// replay on later requests.
    ReasoningItem(serde_json::Value),
    /// The model wants to call a tool. `id` is the provider's call id (used to
    /// pair the result back); empty for backends that don't supply one.
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    /// Partial provider-native tool arguments while the model is still
    /// streaming them. This is UI-only preview data; the engine waits for the
    /// final `ToolCall` before executing anything.
    ToolInputDelta {
        id: String,
        name: String,
        delta: String,
        accumulated: String,
    },
    /// A provider-native CLI command started.
    CommandStarted {
        id: String,
        command: String,
        cwd: String,
        background: bool,
    },
    /// Incremental or final output from a provider-native CLI command.
    CommandOutput {
        id: String,
        stream: String,
        chunk: String,
    },
    /// A background command's output file path became known (CLI drivers run
    /// the process themselves; the file is what outlives the turn).
    BackgroundJob {
        id: String,
        command: String,
        path: String,
    },
    /// A provider-native CLI command finished.
    CommandFinished {
        id: String,
        ok: bool,
        exit_code: Option<i32>,
        duration_ms: u64,
    },
    /// A transcript note from the provider (e.g. an agentic CLI ran a command).
    Notice(String),
    /// The provider's native CLI session id (codex thread / claude uuid). The
    /// engine persists it so a later resume reattaches to the same CLI session.
    CliSession(String),
    /// Final token usage for the call. `context_window` is the model's limit if
    /// the backend reports it (CLI drivers do).
    Usage {
        input: u64,
        output: u64,
        context_window: Option<u64>,
        /// USD cost of the call when the backend reports it (claude CLI's
        /// `total_cost_usd`); None elsewhere (subscription/API without a rate).
        cost_usd: Option<f64>,
        /// Of `input`, how many tokens were served from the prompt cache (0 if the
        /// backend doesn't report it). Drives the cache-hit % in the usage UI.
        cached_input: u64,
        /// Of `output`, how many were reasoning tokens (0 if unreported).
        reasoning_output: u64,
    },
    /// Subscription rate-limit snapshot (from ChatGPT-subscription headers).
    RateLimit {
        plan: String,
        primary_pct: u8,
        secondary_pct: u8,
        primary_reset_s: u64,
        secondary_reset_s: u64,
    },
    /// Stream finished cleanly.
    Done,
}

/// A streaming chat/completions backend.
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;

    /// Run one model call, pushing [`StreamItem`]s into `sink` as they arrive.
    async fn stream(&self, req: TurnRequest, sink: mpsc::Sender<StreamItem>) -> anyhow::Result<()>;
}

/// No-network stub that echoes a canned, streamed reply token-by-token.
///
/// Lets the engine + TUI/GUI be exercised end-to-end before real providers land.
pub struct EchoProvider;

#[async_trait]
impl Provider for EchoProvider {
    fn name(&self) -> &str {
        "echo"
    }

    async fn stream(&self, req: TurnRequest, sink: mpsc::Sender<StreamItem>) -> anyhow::Result<()> {
        let last_user = req
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .map(|m| m.content.clone())
            .unwrap_or_default();

        let reply = format!(
            "[echo:{} | {} tools] You said: {}",
            req.model,
            req.tools.len(),
            last_user.trim()
        );

        for word in reply.split_inclusive(' ') {
            if sink
                .send(StreamItem::TextDelta(word.to_string()))
                .await
                .is_err()
            {
                break; // frontend went away / turn interrupted
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let _ = sink
            .send(StreamItem::Usage {
                cost_usd: None,
                input: last_user.split_whitespace().count() as u64,
                output: reply.split_whitespace().count() as u64,
                context_window: None,
                cached_input: 0,
                reasoning_output: 0,
            })
            .await;
        let _ = sink.send(StreamItem::Done).await;
        Ok(())
    }
}

/// Scripted planner for orchestration tests: emits a numbered plan and no tools.
pub struct MockPlanProvider;

#[async_trait]
impl Provider for MockPlanProvider {
    fn name(&self) -> &str {
        "mock_plan"
    }

    async fn stream(
        &self,
        _req: TurnRequest,
        sink: mpsc::Sender<StreamItem>,
    ) -> anyhow::Result<()> {
        let _ = sink
            .send(StreamItem::TextDelta(
                "1. Write the requested file\n2. Report what changed".to_string(),
            ))
            .await;
        let _ = sink.send(StreamItem::Done).await;
        Ok(())
    }
}

/// Scripted provider for tests/demos: emits one tool call then a short reply.
/// Drives the full tool-routing/approval/sandbox path without a network or API
/// key.
pub struct MockToolProvider;

#[async_trait]
impl Provider for MockToolProvider {
    fn name(&self) -> &str {
        "mock"
    }

    async fn stream(&self, req: TurnRequest, sink: mpsc::Sender<StreamItem>) -> anyhow::Result<()> {
        // Terminate the agentic loop once the tool has run.
        if req.messages.iter().any(|m| matches!(m.role, Role::Tool)) {
            let _ = sink.send(StreamItem::TextDelta("done.".to_string())).await;
            let _ = sink.send(StreamItem::Done).await;
            return Ok(());
        }
        let wants_ui = req.messages.iter().any(|m| {
            if !matches!(m.role, Role::User) {
                return false;
            }
            let content = m.content.to_ascii_lowercase();
            content.contains("ui spec") || content.contains("ui artifact")
        });
        if wants_ui {
            let _ = sink
                .send(StreamItem::ToolCall {
                    id: "m1".into(),
                    name: "render_ui_spec".to_string(),
                    arguments: serde_json::json!({
                        "spec": {
                            "title": "Mock UI",
                            "root": {
                                "type": "card",
                                "props": {
                                    "title": "Mock UI",
                                    "caption": "Rust-native structured artifact"
                                },
                                "children": [
                                    {
                                        "type": "metric",
                                        "props": {
                                            "label": "Status",
                                            "value": "Rendered",
                                            "tone": "success"
                                        }
                                    }
                                ]
                            }
                        }
                    }),
                })
                .await;
            let _ = sink.send(StreamItem::Done).await;
            return Ok(());
        }
        let _ = sink
            .send(StreamItem::ToolCall {
                id: "m1".into(),
                name: "write_file".to_string(),
                arguments: serde_json::json!({
                    "path": "oxide_mock.txt",
                    "content": "written by mock provider"
                }),
            })
            .await;
        let _ = sink.send(StreamItem::Done).await;
        Ok(())
    }
}

/// Scripted provider that calls an MCP tool (`mcp__demo__ping`). For testing the
/// MCP dispatch path end-to-end.
pub struct MockMcpProvider;

#[async_trait]
impl Provider for MockMcpProvider {
    fn name(&self) -> &str {
        "mock_mcp"
    }

    async fn stream(&self, req: TurnRequest, sink: mpsc::Sender<StreamItem>) -> anyhow::Result<()> {
        if req.messages.iter().any(|m| matches!(m.role, Role::Tool)) {
            let _ = sink.send(StreamItem::Done).await;
            return Ok(());
        }
        let _ = sink
            .send(StreamItem::ToolCall {
                id: "m2".into(),
                name: "mcp__demo__ping".to_string(),
                arguments: serde_json::json!({}),
            })
            .await;
        let _ = sink.send(StreamItem::Done).await;
        Ok(())
    }
}

/// Scripted provider that requests browser target + snapshot events.
pub struct MockBrowserProvider;

#[async_trait]
impl Provider for MockBrowserProvider {
    fn name(&self) -> &str {
        "mock_browser"
    }

    async fn stream(&self, req: TurnRequest, sink: mpsc::Sender<StreamItem>) -> anyhow::Result<()> {
        if req.messages.iter().any(|m| matches!(m.role, Role::Tool)) {
            let _ = sink.send(StreamItem::Done).await;
            return Ok(());
        }
        let _ = sink
            .send(StreamItem::ToolCall {
                id: "m3".into(),
                name: "browser_open".to_string(),
                arguments: serde_json::json!({
                    "url": "http://localhost:3000",
                    "note": "Open login page"
                }),
            })
            .await;
        let _ = sink
            .send(StreamItem::ToolCall {
                id: "m4".into(),
                name: "browser_snapshot".to_string(),
                arguments: serde_json::json!({
                    "url": "http://localhost:3000",
                    "note": "Capture login page"
                }),
            })
            .await;
        let _ = sink.send(StreamItem::Done).await;
        Ok(())
    }
}

/// Resolve a provider by id from config. Unknown ids fall back to echo.
/// Shared HTTP client: one connection pool for all provider calls, so each
/// agentic round reuses the TLS connection instead of re-handshaking.
pub fn http_client() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .read_timeout(std::time::Duration::from_secs(120))
                .build()
                .unwrap_or_default()
        })
        .clone()
}

/// Bounded backoff retries for a transient INITIAL request failure (429 / 5xx /
/// connection error) — safe to resend because no SSE bytes have been emitted yet.
/// Mid-stream failures are never retried here (the caller bails so the engine can
/// decide); a low cap keeps a hard outage from spinning.
pub(crate) const MAX_HTTP_RETRIES: u32 = 2;

/// Backoff (ms): honor a numeric `retry-after` (seconds) header when present,
/// else exponential off the attempt index, both capped.
pub(crate) fn http_retry_delay_ms(resp: Option<&reqwest::Response>, attempt: u32) -> u64 {
    if let Some(secs) = resp
        .and_then(|r| r.headers().get("retry-after"))
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
    {
        return (secs * 1000).min(10_000);
    }
    (300u64 << attempt.min(5)).min(4_000)
}

pub fn build(provider: &str) -> Box<dyn Provider> {
    let provider = provider_info(provider)
        .map(|info| info.id)
        .unwrap_or("echo");
    match provider {
        "openai" => Box::new(openai::OpenAiProvider::from_env()),
        "gemini" => Box::new(openai::OpenAiProvider::from_env_compatible(
            "gemini",
            "GEMINI_API_KEY",
            "GEMINI_BASE_URL",
            "https://generativelanguage.googleapis.com/v1beta/openai",
        )),
        "xai" => Box::new(openai::OpenAiProvider::from_env_compatible(
            "xai",
            "XAI_API_KEY",
            "XAI_BASE_URL",
            "https://api.x.ai/v1",
        )),
        "deepseek" => Box::new(openai::OpenAiProvider::from_env_compatible(
            "deepseek",
            "DEEPSEEK_API_KEY",
            "DEEPSEEK_BASE_URL",
            "https://api.deepseek.com",
        )),
        "mistral" => Box::new(openai::OpenAiProvider::from_env_compatible(
            "mistral",
            "MISTRAL_API_KEY",
            "MISTRAL_BASE_URL",
            "https://api.mistral.ai/v1",
        )),
        "anthropic" => Box::new(anthropic::AnthropicProvider::from_env()),
        // CLI drivers — use the user's logged-in codex/claude, no API key.
        "codex" => Box::new(cli::CodexCliProvider::new()),
        // Persistent (Synara-style) is the DEFAULT: one warm `--input-format
        // stream-json` process per conversation — in-process context, live
        // interrupt, background bash that survives turns. Idle children are
        // reaped after 30 minutes. `OXIDE_CLAUDE_PERSISTENT=0` opts back into
        // the one-shot-per-round driver.
        "claude" => {
            if std::env::var("OXIDE_CLAUDE_PERSISTENT")
                .map(|v| v == "0" || v.eq_ignore_ascii_case("false"))
                .unwrap_or(false)
            {
                Box::new(cli::ClaudeCliProvider::new())
            } else {
                Box::new(cli::ClaudePersistentProvider::new())
            }
        }
        "claude_interactive" => Box::new(cli::ClaudeInteractiveProvider::new()),
        // ChatGPT subscription, no API key / no CLI (reuses codex OAuth login).
        "chatgpt" => Box::new(chatgpt::ChatGptProvider::new()),
        "mock_plan" => Box::new(MockPlanProvider),
        "mock" => Box::new(MockToolProvider),
        "mock_mcp" => Box::new(MockMcpProvider),
        "mock_browser" => Box::new(MockBrowserProvider),
        _ => Box::new(EchoProvider),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_resolves_openai_compatible_provider_backends() {
        assert_eq!(build("gemini").name(), "gemini");
        assert_eq!(build("xai").name(), "xai");
        assert_eq!(build("deepseek").name(), "deepseek");
        assert_eq!(build("mistral").name(), "mistral");
        assert_eq!(build("claude_interactive").name(), "claude_interactive");
    }

    #[test]
    fn catalog_entries_resolve_to_matching_runtime_provider() {
        for info in list_providers() {
            assert_eq!(build(info.id).name(), info.id);
        }
    }

    #[test]
    fn unknown_provider_still_falls_back_to_echo() {
        assert_eq!(build("unknown").name(), "echo");
    }
}
