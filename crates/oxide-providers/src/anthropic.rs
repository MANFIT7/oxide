//! Anthropic Messages provider (streaming).
//!
//! No official Rust SDK exists, so this is a thin hand-rolled client over
//! `reqwest` + SSE — the approach most production agents take. The system
//! prompt is lifted out of the message list into the top-level `system` field
//! (Anthropic requirement). Maps `content_block_delta` events onto
//! [`StreamItem`]s. Auth from `ANTHROPIC_API_KEY`.

use crate::{Provider, Role, StreamItem, TurnRequest};
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use oxide_protocol::ToolSpec;
use serde_json::{json, Value};
use tokio::sync::mpsc;

const API_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u64 = 4096;

pub struct AnthropicProvider {
    api_key: String,
    base_url: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn from_env() -> Self {
        Self {
            api_key: std::env::var("ANTHROPIC_API_KEY").unwrap_or_default(),
            base_url: std::env::var("ANTHROPIC_BASE_URL")
                .unwrap_or_else(|_| "https://api.anthropic.com/v1".to_string()),
            client: crate::http_client(),
        }
    }
}

fn tool_json(t: &ToolSpec) -> Value {
    json!({
        "name": t.name,
        "description": t.description,
        "input_schema": t.parameters,
    })
}

fn body(req: &TurnRequest) -> Value {
    // Anthropic wants the system prompt separate from the turn list.
    let system: String = req
        .messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("\n\n");

    // Tool calls become assistant `tool_use` blocks and results become user
    // `tool_result` blocks paired by id — flattening them to text makes the
    // model re-plan/re-call the same tool every round.
    let messages: Vec<Value> = req
        .messages
        .iter()
        .filter(|m| m.role != Role::System)
        .map(|m| {
            if let Some(tc) = &m.tool_call {
                let mut content = Vec::new();
                if !m.content.is_empty() {
                    content.push(json!({ "type": "text", "text": m.content }));
                }
                content.push(json!({
                    "type": "tool_use", "id": tc.id, "name": tc.name, "input": tc.arguments,
                }));
                return json!({ "role": "assistant", "content": content });
            }
            if m.role == Role::Tool {
                if let Some(id) = &m.tool_call_id {
                    return json!({
                        "role": "user",
                        "content": [{ "type": "tool_result", "tool_use_id": id, "content": m.content }]
                    });
                }
                return json!({ "role": "user", "content": m.content });
            }
            let role = match m.role {
                Role::Assistant => "assistant",
                _ => "user",
            };
            json!({ "role": role, "content": m.content })
        })
        .collect();

    let mut b = json!({
        "model": req.model,
        "max_tokens": DEFAULT_MAX_TOKENS,
        "temperature": req.temperature,
        "stream": true,
        "messages": messages,
    });
    if !system.is_empty() {
        b["system"] = Value::String(system);
    }
    if !req.tools.is_empty() {
        b["tools"] = Value::Array(req.tools.iter().map(tool_json).collect());
    }
    if !req.reasoning_effort.is_empty() {
        b["output_config"] = json!({ "effort": anthropic_effort(&req.reasoning_effort) });
    }
    b
}

fn anthropic_effort(effort: &str) -> &str {
    match effort {
        "xhigh" => "max",
        other => other,
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    async fn stream(&self, req: TurnRequest, sink: mpsc::Sender<StreamItem>) -> anyhow::Result<()> {
        if self.api_key.is_empty() {
            anyhow::bail!("ANTHROPIC_API_KEY not set");
        }
        // Initial POST with bounded backoff on transient 429 / 5xx (Anthropic 529
        // "overloaded" is routine) / connection errors. Idempotent: no SSE bytes
        // are emitted until a 2xx, so a resend cannot duplicate output.
        let payload = body(&req);
        let mut attempt = 0u32;
        let resp = loop {
            let send = self
                .client
                .post(format!("{}/messages", self.base_url))
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", API_VERSION)
                .json(&payload)
                .send()
                .await;
            match send {
                Ok(r) if r.status().is_success() => break r,
                Ok(r) => {
                    let status = r.status();
                    if (status.as_u16() == 429 || status.is_server_error())
                        && attempt < crate::MAX_HTTP_RETRIES
                    {
                        let wait = crate::http_retry_delay_ms(Some(&r), attempt);
                        attempt += 1;
                        tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
                        continue;
                    }
                    let text = r.text().await.unwrap_or_default();
                    anyhow::bail!("anthropic {status}: {text}");
                }
                Err(_e) if attempt < crate::MAX_HTTP_RETRIES => {
                    let wait = crate::http_retry_delay_ms(None, attempt);
                    attempt += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        };

        // Tool-use accumulation for the currently open content block.
        let mut cur_tool: Option<String> = None;
        let mut cur_tool_id = String::new();
        let mut cur_args = String::new();
        let mut input_tokens = 0u64;

        let mut stream = resp.bytes_stream().eventsource();
        while let Some(ev) = stream.next().await {
            let ev = ev?;
            let data: Value = match serde_json::from_str(&ev.data) {
                Ok(v) => v,
                Err(_) => continue,
            };
            match ev.event.as_str() {
                "message_start" => {
                    input_tokens = data["message"]["usage"]["input_tokens"]
                        .as_u64()
                        .unwrap_or(0);
                }
                "content_block_start" => {
                    let block = &data["content_block"];
                    if block["type"] == "tool_use" {
                        cur_tool = block["name"].as_str().map(|s| s.to_string());
                        cur_tool_id = block["id"].as_str().unwrap_or("").to_string();
                        cur_args.clear();
                    }
                }
                "content_block_delta" => {
                    let delta = &data["delta"];
                    match delta["type"].as_str() {
                        Some("text_delta") => {
                            if let Some(t) = delta["text"].as_str() {
                                if sink
                                    .send(StreamItem::TextDelta(t.to_string()))
                                    .await
                                    .is_err()
                                {
                                    return Ok(());
                                }
                            }
                        }
                        Some("thinking_delta") => {
                            if let Some(t) = delta["thinking"].as_str() {
                                let _ = sink.send(StreamItem::ReasoningDelta(t.to_string())).await;
                            }
                        }
                        Some("input_json_delta") => {
                            if let Some(p) = delta["partial_json"].as_str() {
                                cur_args.push_str(p);
                            }
                        }
                        _ => {}
                    }
                }
                "content_block_stop" => {
                    if let Some(name) = cur_tool.take() {
                        let arguments = serde_json::from_str(&cur_args)
                            .unwrap_or(Value::Object(Default::default()));
                        let id = std::mem::take(&mut cur_tool_id);
                        let _ = sink
                            .send(StreamItem::ToolCall {
                                id,
                                name,
                                arguments,
                            })
                            .await;
                        cur_args.clear();
                    }
                }
                "message_delta" => {
                    let output = data["usage"]["output_tokens"].as_u64().unwrap_or(0);
                    let _ = sink
                        .send(StreamItem::Usage {
                            input: input_tokens,
                            output,
                            context_window: None,
                        })
                        .await;
                }
                "message_stop" => break,
                _ => {}
            }
        }
        let _ = sink.send(StreamItem::Done).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Message;
    use oxide_protocol::ToolSpec;

    fn req() -> TurnRequest {
        TurnRequest {
            model: "claude-sonnet-4-6".into(),
            reasoning_effort: "medium".into(),
            temperature: 0.2,
            messages: vec![
                Message::new(Role::System, "be terse"),
                Message::new(Role::User, "hi"),
            ],
            tools: vec![ToolSpec::new("read_file", "read a file")],
            cwd: "/tmp".into(),
            conversation_id: "session".into(),
            cli_resume: None,
        }
    }

    #[test]
    fn system_is_lifted_out_of_messages() {
        let b = body(&req());
        assert_eq!(b["system"], "be terse");
        let msgs = b["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1, "system must not appear in messages");
        assert_eq!(msgs[0]["role"], "user");
        assert!(b["stream"].as_bool().unwrap());
    }

    #[test]
    fn tools_use_input_schema_key() {
        let b = body(&req());
        assert_eq!(b["tools"][0]["name"], "read_file");
        assert!(b["tools"][0].get("input_schema").is_some());
    }
}
