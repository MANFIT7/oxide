//! ChatGPT-subscription provider — no API key, no codex CLI subprocess.
//!
//! Reuses the OAuth token the `codex` CLI already stored at `~/.codex/auth.json`
//! (ChatGPT Plus/Pro login) and calls the same backend the CLI uses directly:
//! `POST https://chatgpt.com/backend-api/codex/responses` (Responses API, SSE).
//!
//! ⚠ This is OpenAI's internal endpoint, reached with subscription credentials —
//! it can change without notice and is ToS-grey. It is gated behind the explicit
//! `chatgpt` provider so nothing uses it unless asked.

use crate::{Message, Provider, Role, StreamItem, TurnRequest};
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

const ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
const DEFAULT_MODEL: &str = "gpt-5.5";
const CONTEXT_WINDOW: u64 = 272_000;

pub struct ChatGptProvider {
    client: reqwest::Client,
    auth_path: String,
}

impl ChatGptProvider {
    pub fn new() -> Self {
        let home = std::env::var("HOME").unwrap_or_default();
        let auth_path = std::env::var("OXIDE_CODEX_AUTH")
            .unwrap_or_else(|_| format!("{home}/.codex/auth.json"));
        Self { client: reqwest::Client::new(), auth_path }
    }

    /// Read `(access_token, account_id)` from the codex auth file.
    fn credentials(&self) -> anyhow::Result<(String, String)> {
        let text = std::fs::read_to_string(&self.auth_path).map_err(|e| {
            anyhow::anyhow!("ChatGPT login not found ({}): {e}. Run `codex` and log in first.", self.auth_path)
        })?;
        let v: Value = serde_json::from_str(&text)?;
        let at = v["tokens"]["access_token"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("no access_token in codex auth"))?
            .to_string();
        let acc = v["tokens"]["account_id"].as_str().unwrap_or("").to_string();
        Ok((at, acc))
    }
}

impl Default for ChatGptProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// A UUID-shaped session id (format-valid; not cryptographically random).
fn session_id() -> String {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let h = format!("{n:032x}");
    format!(
        "{}-{}-4{}-8{}-{}",
        &h[0..8],
        &h[8..12],
        &h[13..16],
        &h[17..20],
        &h[20..32]
    )
}

fn build_body(req: &TurnRequest) -> Value {
    let mut instructions = String::new();
    let mut input: Vec<Value> = Vec::new();
    for m in &req.messages {
        match m.role {
            Role::System => {
                if !instructions.is_empty() {
                    instructions.push_str("\n\n");
                }
                instructions.push_str(&m.content);
            }
            Role::User | Role::Tool => input.push(json!({
                "type": "message", "role": "user",
                "content": [{ "type": "input_text", "text": m.content }]
            })),
            Role::Assistant => input.push(json!({
                "type": "message", "role": "assistant",
                "content": [{ "type": "output_text", "text": m.content }]
            })),
        }
    }
    let model = if req.model.is_empty() { DEFAULT_MODEL } else { req.model.as_str() };
    let effort = if req.reasoning_effort.is_empty() { "medium" } else { req.reasoning_effort.as_str() };
    json!({
        "model": model,
        "instructions": instructions,
        "input": input,
        "stream": true,
        "store": false,
        "reasoning": { "effort": effort }
    })
}

#[async_trait]
impl Provider for ChatGptProvider {
    fn name(&self) -> &str {
        "chatgpt"
    }

    async fn stream(
        &self,
        req: TurnRequest,
        sink: mpsc::Sender<StreamItem>,
    ) -> anyhow::Result<()> {
        let (access, account) = self.credentials()?;
        let resp = self
            .client
            .post(ENDPOINT)
            .bearer_auth(&access)
            .header("chatgpt-account-id", account)
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .header("OpenAI-Beta", "responses=experimental")
            .header("originator", "codex_cli_rs")
            .header("session_id", session_id())
            .json(&build_body(&req))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if status.as_u16() == 401 {
                anyhow::bail!("ChatGPT token expired — run `codex` to refresh login. ({text})");
            }
            anyhow::bail!("chatgpt {status}: {text}");
        }

        let mut stream = resp.bytes_stream().eventsource();
        while let Some(ev) = stream.next().await {
            let ev = ev?;
            let v: Value = match serde_json::from_str(&ev.data) {
                Ok(v) => v,
                Err(_) => continue,
            };
            match v["type"].as_str() {
                Some("response.output_text.delta") => {
                    if let Some(t) = v["delta"].as_str() {
                        if sink.send(StreamItem::TextDelta(t.to_string())).await.is_err() {
                            return Ok(());
                        }
                    }
                }
                Some("response.reasoning_summary_text.delta") | Some("response.reasoning_text.delta") => {
                    if let Some(t) = v["delta"].as_str() {
                        let _ = sink.send(StreamItem::ReasoningDelta(t.to_string())).await;
                    }
                }
                Some("response.completed") => {
                    let u = &v["response"]["usage"];
                    let _ = sink
                        .send(StreamItem::Usage {
                            input: u["input_tokens"].as_u64().unwrap_or(0),
                            output: u["output_tokens"].as_u64().unwrap_or(0),
                            context_window: Some(CONTEXT_WINDOW),
                        })
                        .await;
                }
                Some("response.failed") => {
                    let msg = v["response"]["error"]["message"].as_str().unwrap_or("response failed");
                    let _ = sink.send(StreamItem::Notice(format!("error: {msg}"))).await;
                }
                _ => {}
            }
        }
        let _ = sink.send(StreamItem::Done).await;
        Ok(())
    }
}
