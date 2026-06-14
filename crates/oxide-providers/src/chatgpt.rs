//! ChatGPT-subscription provider — no API key, no codex CLI subprocess.
//!
//! Reuses the OAuth token the `codex` CLI already stored at `~/.codex/auth.json`
//! (ChatGPT Plus/Pro login) and calls the same backend the CLI uses directly:
//! `POST https://chatgpt.com/backend-api/codex/responses` (Responses API, SSE).
//!
//! ⚠ This is OpenAI's internal endpoint, reached with subscription credentials —
//! it can change without notice and is ToS-grey. It is gated behind the explicit
//! `chatgpt` provider so nothing uses it unless asked.

use crate::{Provider, Role, StreamItem, TurnRequest};
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

const ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
const DEFAULT_MODEL: &str = "gpt-5.5";
const CONTEXT_WINDOW: u64 = 272_000;

/// Best-known context window (tokens) for a ChatGPT-backend model, so
/// compaction adapts automatically per model instead of a fixed number.
fn model_context_window(model: &str) -> u64 {
    let m = model.to_ascii_lowercase();
    if m.contains("gpt-5") || m.contains("gpt5") {
        400_000
    } else if m.contains("gpt-4.1") || m.contains("o3") || m.contains("o4") {
        1_000_000
    } else if m.contains("gpt-4") {
        128_000
    } else {
        CONTEXT_WINDOW
    }
}

pub struct ChatGptProvider {
    client: reqwest::Client,
    auth_path: String,
}

impl ChatGptProvider {
    pub fn new() -> Self {
        let home = std::env::var("HOME").unwrap_or_default();
        let auth_path = std::env::var("OXIDE_CODEX_AUTH")
            .unwrap_or_else(|_| format!("{home}/.codex/auth.json"));
        Self {
            client: crate::http_client(),
            auth_path,
        }
    }

    /// Read `(access_token, account_id)` from the codex auth file.
    fn credentials(&self) -> anyhow::Result<(String, String)> {
        let text = std::fs::read_to_string(&self.auth_path).map_err(|e| {
            anyhow::anyhow!(
                "ChatGPT subscription login not found ({}): {e}. Run `codex login` or open Codex Desktop and sign in again.",
                self.auth_path
            )
        })?;
        let v: Value = serde_json::from_str(&text)?;
        let at = v["tokens"]["access_token"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("ChatGPT subscription auth is missing access_token — run `codex login` to refresh it."))?
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

fn hash64(input: &str) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for b in input.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn session_id_for(conversation_id: &str) -> String {
    let conv = conversation_id.trim();
    if conv.is_empty() {
        return session_id();
    }
    let h = format!(
        "{:016x}{:016x}",
        hash64(&format!("oxide-chatgpt-session:{conv}")),
        hash64(&format!("oxide-chatgpt-session-v2:{conv}"))
    );
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
            // A tool result → a `function_call_output` paired by call_id, so the
            // model sees its call was satisfied (instead of an orphan user text).
            Role::Tool if m.tool_call_id.is_some() => input.push(json!({
                "type": "function_call_output",
                "call_id": m.tool_call_id.clone().unwrap_or_default(),
                "output": m.content,
            })),
            Role::User | Role::Tool => input.push(json!({
                "type": "message", "role": "user",
                "content": [{ "type": "input_text", "text": m.content }]
            })),
            Role::Assistant => {
                // Replay the model's own (encrypted) reasoning first so it keeps
                // its train of thought across rounds instead of re-thinking.
                if let Some(r) = &m.reasoning_item {
                    input.push(r.clone());
                }
                // Any assistant prose first, then a structured function_call item.
                if !m.content.is_empty() {
                    input.push(json!({
                        "type": "message", "role": "assistant",
                        "content": [{ "type": "output_text", "text": m.content }]
                    }));
                }
                if let Some(tc) = &m.tool_call {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": tc.id,
                        "name": tc.name,
                        "arguments": serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "{}".into()),
                    }));
                }
            }
        }
    }
    let model = if req.model.is_empty() { DEFAULT_MODEL } else { req.model.as_str() };
    let effort = if req.reasoning_effort.is_empty() { "medium" } else { req.reasoning_effort.as_str() };
    let mut body = json!({
        "model": model,
        "instructions": instructions,
        "input": input,
        "stream": true,
        "store": false,
        "include": ["reasoning.encrypted_content"],
        "reasoning": { "effort": effort }
    });
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                    "strict": false,
                })
            })
            .collect();
        body["tools"] = json!(tools);
        body["tool_choice"] = json!("auto");
    }
    body
}

fn parse_tool_args(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| json!({}))
}

fn response_tool_call(
    item: &Value,
    pending: &mut HashMap<String, (String, String)>,
) -> Option<(Vec<String>, String, Value)> {
    match item["type"].as_str()? {
        "function_call" => {
            let item_id = item["id"].as_str().unwrap_or("").to_string();
            let pending_item = if item_id.is_empty() {
                None
            } else {
                pending.remove(&item_id)
            };
            let name = item["name"]
                .as_str()
                .map(str::to_string)
                .or_else(|| pending_item.as_ref().map(|(name, _)| name.clone()))?;
            let raw = item["arguments"]
                .as_str()
                .map(str::to_string)
                .or_else(|| pending_item.map(|(_, args)| args))
                .unwrap_or_else(|| "{}".to_string());
            let call_id = item["call_id"].as_str().unwrap_or("").to_string();
            Some((vec![call_id, item_id], name, parse_tool_args(&raw)))
        }
        "shell_call" => {
            let commands: Vec<String> = item["action"]["commands"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|cmd| cmd.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if commands.is_empty() {
                return None;
            }
            let call_id = item["call_id"].as_str().unwrap_or("").to_string();
            let item_id = item["id"].as_str().unwrap_or("").to_string();
            Some((vec![call_id, item_id], "shell".to_string(), json!({ "command": commands.join("\n") })))
        }
        _ => None,
    }
}

async fn send_tool_call(
    sink: &mpsc::Sender<StreamItem>,
    sent: &mut HashSet<String>,
    ids: Vec<String>,
    name: String,
    arguments: Value,
) -> bool {
    if ids.iter().any(|id| !id.is_empty() && sent.contains(id)) {
        return true;
    }
    let id = ids
        .iter()
        .find(|id| !id.is_empty())
        .cloned()
        .unwrap_or_else(|| format!("{name}:{arguments}"));
    sent.insert(id.clone());
    for alias in ids {
        if !alias.is_empty() {
            sent.insert(alias);
        }
    }
    sink.send(StreamItem::ToolCall { id, name, arguments }).await.is_ok()
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
        let chatgpt_session_id = session_id_for(&req.conversation_id);
        let resp = self
            .client
            .post(ENDPOINT)
            .bearer_auth(&access)
            .header("chatgpt-account-id", account)
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .header("OpenAI-Beta", "responses=experimental")
            .header("originator", "codex_cli_rs")
            .header("session_id", chatgpt_session_id)
            .json(&build_body(&req))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if status.as_u16() == 401 {
                anyhow::bail!("ChatGPT subscription token expired — run `codex login` or sign in again from Codex Desktop. ({text})");
            }
            if status.as_u16() == 403 {
                anyhow::bail!("ChatGPT subscription rejected the request ({status}). Check that this account has Codex/ChatGPT subscription access and re-authenticate if needed. ({text})");
            }
            if status.as_u16() == 429 {
                anyhow::bail!("ChatGPT subscription rate limit reached ({status}). Wait for the plan reset shown in Usage, then retry. ({text})");
            }
            if status.as_u16() == 413 || text.to_ascii_lowercase().contains("context") {
                anyhow::bail!("ChatGPT subscription context is too large ({status}). Compact the chat or remove large attachments, then retry. ({text})");
            }
            anyhow::bail!("chatgpt {status}: {text}");
        }

        // Subscription rate-limit snapshot from response headers.
        {
            let h = resp.headers();
            let hv = |k: &str| h.get(k).and_then(|v| v.to_str().ok());
            let pct = |k: &str| hv(k).and_then(|s| s.parse::<u8>().ok());
            if let (Some(p), Some(sec)) = (pct("x-codex-primary-used-percent"), pct("x-codex-secondary-used-percent")) {
                let plan = hv("x-codex-plan-type").or_else(|| hv("x-codex-active-limit")).unwrap_or("").to_string();
                let resets = |k: &str| hv(k).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
                let _ = sink
                    .send(StreamItem::RateLimit {
                        plan,
                        primary_pct: p,
                        secondary_pct: sec,
                        primary_reset_s: resets("x-codex-primary-reset-after-seconds"),
                        secondary_reset_s: resets("x-codex-secondary-reset-after-seconds"),
                    })
                    .await;
            }
        }

        let mut stream = resp.bytes_stream().eventsource();
        let mut pending_function_args: HashMap<String, (String, String)> = HashMap::new();
        let mut sent_tools: HashSet<String> = HashSet::new();
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
                Some("response.output_item.done") => {
                    let item = &v["item"];
                    if item["type"].as_str() == Some("reasoning") {
                        let _ = sink.send(StreamItem::ReasoningItem(item.clone())).await;
                    }
                    if let Some((ids, name, arguments)) = response_tool_call(item, &mut pending_function_args) {
                        if !send_tool_call(&sink, &mut sent_tools, ids, name, arguments).await {
                            return Ok(());
                        }
                    }
                }
                Some("response.function_call_arguments.done") => {
                    let item_id = v["item_id"].as_str().unwrap_or("").to_string();
                    let name = v["name"].as_str().unwrap_or("").to_string();
                    let arguments = v["arguments"].as_str().unwrap_or("{}").to_string();
                    if !item_id.is_empty() && !name.is_empty() {
                        pending_function_args.insert(item_id, (name, arguments));
                    }
                }
                Some("response.completed") => {
                    let u = &v["response"]["usage"];
                    let _ = sink
                        .send(StreamItem::Usage {
                            input: u["input_tokens"].as_u64().unwrap_or(0),
                            output: u["output_tokens"].as_u64().unwrap_or(0),
                            context_window: Some(model_context_window(if req.model.is_empty() { DEFAULT_MODEL } else { &req.model })),
                        })
                        .await;
                }
                Some("response.failed") => {
                    let msg = v["response"]["error"]["message"].as_str().unwrap_or("response failed");
                    anyhow::bail!("ChatGPT response failed: {msg}");
                }
                Some("response.incomplete") => {
                    let reason = v["response"]["incomplete_details"]["reason"]
                        .as_str()
                        .unwrap_or("unknown");
                    anyhow::bail!("ChatGPT response incomplete: {reason}. Compact context or retry with a smaller prompt.");
                }
                _ => {}
            }
        }
        for (item_id, (name, raw_args)) in pending_function_args {
            if !send_tool_call(
                &sink,
                &mut sent_tools,
                vec![item_id],
                name,
                parse_tool_args(&raw_args),
            )
            .await
            {
                return Ok(());
            }
        }
        let _ = sink.send(StreamItem::Done).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxide_protocol::ToolSpec;

    fn req_with_tools(tools: Vec<ToolSpec>) -> TurnRequest {
        TurnRequest {
            model: "gpt-5.5".to_string(),
            reasoning_effort: "medium".to_string(),
            temperature: 0.2,
            messages: Vec::new(),
            tools,
            cwd: "/tmp".to_string(),
            conversation_id: "session".to_string(),
            cli_resume: None,
        }
    }

    #[test]
    fn body_uses_responses_function_tool_shape() {
        let req = req_with_tools(vec![
            ToolSpec::new("shell", "Run a shell command").mutating(true).params(json!({
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"]
            })),
        ]);

        let body = build_body(&req);
        let tool = &body["tools"][0];
        assert_eq!(tool["type"], "function");
        assert_eq!(tool["name"], "shell");
        assert_eq!(tool["strict"], false);
        assert_eq!(tool["parameters"]["required"][0], "command");
        assert_eq!(body["tool_choice"], "auto");
    }

    #[test]
    fn output_item_uses_pending_function_arguments_when_needed() {
        let mut pending = HashMap::from([(
            "item_1".to_string(),
            (
                "todo_write".to_string(),
                r#"{"todos":[{"content":"Build","status":"in_progress"}]}"#.to_string(),
            ),
        )]);
        let item = json!({
            "type": "function_call",
            "id": "item_1",
            "call_id": "call_1"
        });

        let (ids, name, args) = response_tool_call(&item, &mut pending).unwrap();

        assert_eq!(ids, vec!["call_1".to_string(), "item_1".to_string()]);
        assert_eq!(name, "todo_write");
        assert_eq!(args["todos"][0]["content"], "Build");
        assert!(pending.is_empty());
    }

    #[test]
    fn native_shell_call_maps_to_engine_shell_tool() {
        let mut pending = HashMap::new();
        let item = json!({
            "type": "shell_call",
            "id": "item_shell",
            "call_id": "call_shell",
            "action": { "commands": ["pwd", "ls -la"] }
        });

        let (ids, name, args) = response_tool_call(&item, &mut pending).unwrap();

        assert_eq!(ids, vec!["call_shell".to_string(), "item_shell".to_string()]);
        assert_eq!(name, "shell");
        assert_eq!(args["command"], "pwd\nls -la");
    }

    #[test]
    fn chatgpt_session_id_is_stable_per_conversation() {
        let a1 = session_id_for("oxide-session-a");
        let a2 = session_id_for("oxide-session-a");
        let b = session_id_for("oxide-session-b");

        assert_eq!(a1, a2);
        assert_ne!(a1, b);
        assert_eq!(a1.len(), 36);
        assert_eq!(a1.chars().filter(|c| *c == '-').count(), 4);
    }
}
