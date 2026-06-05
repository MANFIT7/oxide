//! Provider abstraction.
//!
//! Every backend (OpenAI, Anthropic Messages, local) implements [`Provider`]
//! and streams normalized [`StreamItem`]s back through a channel. The engine
//! never sees provider-specific wire formats. [`EchoProvider`] needs no API key
//! and is used for offline/dev runs and tests.

mod anthropic;
mod chatgpt;
mod cli;
mod openai;
pub use cli::{ClaudeCliProvider, CodexCliProvider};

use async_trait::async_trait;
use oxide_protocol::ToolSpec;
use tokio::sync::mpsc;

/// One message in the conversation sent to the model.
#[derive(Debug, Clone)]
pub struct Message {
    pub role: Role,
    pub content: String,
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
}

/// Normalized streaming output. Each provider maps its SSE events to these.
#[derive(Debug, Clone)]
pub enum StreamItem {
    /// A chunk of assistant text.
    TextDelta(String),
    /// A chunk of reasoning/thinking text.
    ReasoningDelta(String),
    /// The model wants to call a tool.
    ToolCall {
        name: String,
        arguments: serde_json::Value,
    },
    /// A transcript note from the provider (e.g. an agentic CLI ran a command).
    Notice(String),
    /// Final token usage for the call. `context_window` is the model's limit if
    /// the backend reports it (CLI drivers do).
    Usage {
        input: u64,
        output: u64,
        context_window: Option<u64>,
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
                input: last_user.split_whitespace().count() as u64,
                output: reply.split_whitespace().count() as u64,
                context_window: None,
            })
            .await;
        let _ = sink.send(StreamItem::Done).await;
        Ok(())
    }
}

/// Scripted provider for tests/demos: emits one `write_file` tool call then a
/// short reply. Drives the full tool-routing/approval/sandbox path without a
/// network or API key.
pub struct MockToolProvider;

#[async_trait]
impl Provider for MockToolProvider {
    fn name(&self) -> &str {
        "mock"
    }

    async fn stream(
        &self,
        _req: TurnRequest,
        sink: mpsc::Sender<StreamItem>,
    ) -> anyhow::Result<()> {
        let _ = sink
            .send(StreamItem::ToolCall {
                name: "write_file".to_string(),
                arguments: serde_json::json!({
                    "path": "oxide_mock.txt",
                    "content": "written by mock provider"
                }),
            })
            .await;
        let _ = sink.send(StreamItem::TextDelta("done.".to_string())).await;
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

    async fn stream(
        &self,
        _req: TurnRequest,
        sink: mpsc::Sender<StreamItem>,
    ) -> anyhow::Result<()> {
        let _ = sink
            .send(StreamItem::ToolCall {
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

    async fn stream(
        &self,
        _req: TurnRequest,
        sink: mpsc::Sender<StreamItem>,
    ) -> anyhow::Result<()> {
        let _ = sink
            .send(StreamItem::ToolCall {
                name: "browser_open".to_string(),
                arguments: serde_json::json!({
                    "url": "http://localhost:3000",
                    "note": "Open login page"
                }),
            })
            .await;
        let _ = sink
            .send(StreamItem::ToolCall {
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
pub fn build(provider: &str) -> Box<dyn Provider> {
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
        "claude" => Box::new(cli::ClaudeCliProvider::new()),
        // ChatGPT subscription, no API key / no CLI (reuses codex OAuth login).
        "chatgpt" => Box::new(chatgpt::ChatGptProvider::new()),
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
    }
}
