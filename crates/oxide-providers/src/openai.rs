//! OpenAI Chat Completions provider (streaming).
//!
//! Hand-rolled over `reqwest` + SSE rather than a generated SDK, so the wire
//! shape stays under our control and matches the Anthropic path. Maps the
//! delta stream onto [`StreamItem`]s. Auth from `OPENAI_API_KEY`; base URL
//! overridable via `OPENAI_BASE_URL` for OpenAI-compatible endpoints.

use crate::{Message, Provider, Role, StreamItem, TurnRequest};
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use oxide_protocol::ToolSpec;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tokio::sync::mpsc;

pub struct OpenAiProvider {
    name: String,
    api_key_env: String,
    api_key: String,
    base_url: String,
    client: reqwest::Client,
}

impl OpenAiProvider {
    pub fn from_env() -> Self {
        Self::from_env_compatible(
            "openai",
            "OPENAI_API_KEY",
            "OPENAI_BASE_URL",
            "https://api.openai.com/v1",
        )
    }

    pub fn from_env_compatible(
        name: &str,
        api_key_env: &str,
        base_url_env: &str,
        default_base_url: &str,
    ) -> Self {
        Self {
            name: name.to_string(),
            api_key_env: api_key_env.to_string(),
            api_key: std::env::var(api_key_env).unwrap_or_default(),
            base_url: std::env::var(base_url_env)
                .unwrap_or_else(|_| default_base_url.to_string())
                .trim_end_matches('/')
                .to_string(),
            client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .read_timeout(std::time::Duration::from_secs(120))
                .build()
                .unwrap_or_default(),
        }
    }
}

fn role_str(r: Role) -> &'static str {
    match r {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn tool_json(t: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": t.name,
            "description": t.description,
            "parameters": t.parameters,
        }
    })
}

fn body(req: &TurnRequest) -> Value {
    // Tool calls/results must stay structurally paired (tool_calls + tool_call_id)
    // or the model loses track of executed tools and the API rejects orphan
    // role:"tool" messages.
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|m: &Message| {
            if let Some(tc) = &m.tool_call {
                return json!({
                    "role": "assistant",
                    "content": if m.content.is_empty() { Value::Null } else { Value::String(m.content.clone()) },
                    "tool_calls": [{
                        "id": tc.id,
                        "type": "function",
                        "function": {
                            "name": tc.name,
                            "arguments": serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "{}".into()),
                        }
                    }]
                });
            }
            if m.role == Role::Tool {
                if let Some(id) = &m.tool_call_id {
                    return json!({ "role": "tool", "tool_call_id": id, "content": m.content });
                }
                // Legacy unpaired tool note — fold into a user message.
                return json!({ "role": "user", "content": m.content });
            }
            json!({ "role": role_str(m.role), "content": m.content })
        })
        .collect();
    let mut b = json!({
        "model": req.model,
        "temperature": req.temperature,
        "stream": true,
        "stream_options": { "include_usage": true },
        "messages": messages,
    });
    if !req.tools.is_empty() {
        b["tools"] = Value::Array(req.tools.iter().map(tool_json).collect());
    }
    if !req.reasoning_effort.is_empty() {
        b["reasoning_effort"] = Value::String(req.reasoning_effort.clone());
    }
    b
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn stream(&self, req: TurnRequest, sink: mpsc::Sender<StreamItem>) -> anyhow::Result<()> {
        if self.api_key.trim().is_empty() {
            anyhow::bail!("{} key not set: {}", self.name, self.api_key_env);
        }
        let resp = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body(&req))
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("{} {status}: {text}", self.name);
        }

        // Accumulate streamed tool calls by index until the stream ends.
        let mut tool_names: BTreeMap<u64, String> = BTreeMap::new();
        let mut tool_args: BTreeMap<u64, String> = BTreeMap::new();
        let mut tool_ids: BTreeMap<u64, String> = BTreeMap::new();

        let mut stream = resp.bytes_stream().eventsource();
        while let Some(ev) = stream.next().await {
            let ev = ev?;
            if ev.data == "[DONE]" {
                break;
            }
            let chunk: Value = match serde_json::from_str(&ev.data) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "bad openai chunk");
                    continue;
                }
            };

            if let Some(usage) = chunk.get("usage").filter(|u| !u.is_null()) {
                let _ = sink
                    .send(StreamItem::Usage {
                        input: usage["prompt_tokens"].as_u64().unwrap_or(0),
                        output: usage["completion_tokens"].as_u64().unwrap_or(0),
                        context_window: None,
                    })
                    .await;
            }

            let Some(delta) = chunk["choices"].get(0).map(|c| &c["delta"]) else {
                continue;
            };
            if let Some(text) = delta["content"].as_str() {
                if !text.is_empty()
                    && sink
                        .send(StreamItem::TextDelta(text.to_string()))
                        .await
                        .is_err()
                {
                    return Ok(());
                }
            }
            if let Some(calls) = delta["tool_calls"].as_array() {
                for c in calls {
                    let idx = c["index"].as_u64().unwrap_or(0);
                    if let Some(id) = c["id"].as_str() {
                        tool_ids.entry(idx).or_default().push_str(id);
                    }
                    if let Some(name) = c["function"]["name"].as_str() {
                        tool_names.entry(idx).or_default().push_str(name);
                    }
                    if let Some(args) = c["function"]["arguments"].as_str() {
                        tool_args.entry(idx).or_default().push_str(args);
                    }
                }
            }
        }

        for (idx, name) in tool_names {
            let raw = tool_args.remove(&idx).unwrap_or_default();
            let arguments = serde_json::from_str(&raw).unwrap_or(Value::Object(Default::default()));
            let id = tool_ids.remove(&idx).unwrap_or_default();
            let _ = sink.send(StreamItem::ToolCall { id, name, arguments }).await;
        }
        let _ = sink.send(StreamItem::Done).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> TurnRequest {
        TurnRequest {
            model: "gpt-5-codex".into(),
            reasoning_effort: "high".into(),
            temperature: 0.2,
            messages: vec![
                Message::new(Role::System, "sys"),
                Message::new(Role::User, "hi"),
            ],
            tools: vec![ToolSpec::new("shell", "run a command").mutating(true)],
        }
    }

    #[test]
    fn body_streams_with_messages_and_tools() {
        let b = body(&req());
        assert!(b["stream"].as_bool().unwrap());
        assert!(b["stream_options"]["include_usage"].as_bool().unwrap());
        let msgs = b["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(b["tools"][0]["type"], "function");
        assert_eq!(b["tools"][0]["function"]["name"], "shell");
    }

    #[tokio::test]
    async fn compatible_provider_reports_own_missing_key_env() {
        let provider = OpenAiProvider::from_env_compatible(
            "gemini",
            "OXIDE_TEST_MISSING_GEMINI_API_KEY",
            "OXIDE_TEST_GEMINI_BASE_URL",
            "https://example.invalid/v1",
        );
        let (tx, _rx) = tokio::sync::mpsc::channel(1);

        let err = provider.stream(req(), tx).await.unwrap_err().to_string();

        assert!(err.contains("gemini key not set"));
        assert!(err.contains("OXIDE_TEST_MISSING_GEMINI_API_KEY"));
    }
}
