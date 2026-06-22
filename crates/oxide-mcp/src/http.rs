//! Streamable HTTP / SSE transport for remote MCP servers.
//!
//! JSON-RPC requests are POSTed to the endpoint. The server may answer with a
//! plain JSON body or an SSE stream (`text/event-stream`); both are handled. A
//! `Mcp-Session-Id` header returned on initialize is echoed on later requests.

use crate::Transport;
use anyhow::Context;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex;

pub struct HttpTransport {
    client: reqwest::Client,
    url: String,
    bearer_token: String,
    bearer_token_env_var: String,
    headers: BTreeMap<String, String>,
    env_headers: BTreeMap<String, String>,
    next_id: AtomicU64,
    session: Mutex<Option<String>>,
}

impl HttpTransport {
    pub fn new(url: impl Into<String>) -> Self {
        Self::new_with(url, HttpOptions::default())
    }

    pub fn new_with(url: impl Into<String>, options: HttpOptions) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(options.request_timeout)
                .build()
                .unwrap_or_default(),
            url: url.into(),
            bearer_token: options.bearer_token,
            bearer_token_env_var: options.bearer_token_env_var,
            headers: options.headers,
            env_headers: options.env_headers,
            next_id: AtomicU64::new(1),
            session: Mutex::new(None),
        }
    }

    async fn post(&self, body: &Value, want_id: Option<u64>) -> anyhow::Result<Value> {
        let mut req = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        if !self.bearer_token.is_empty() {
            req = req.bearer_auth(&self.bearer_token);
        } else if !self.bearer_token_env_var.is_empty() {
            if let Ok(token) = std::env::var(&self.bearer_token_env_var) {
                if !token.is_empty() {
                    req = req.bearer_auth(token);
                }
            }
        }
        for (key, value) in &self.headers {
            req = req.header(key.as_str(), value.as_str());
        }
        for (key, env_name) in &self.env_headers {
            if let Ok(value) = std::env::var(env_name) {
                req = req.header(key.as_str(), value);
            }
        }
        if let Some(sid) = self.session.lock().await.clone() {
            req = req.header("Mcp-Session-Id", sid);
        }
        let resp = req.json(body).send().await?;
        if let Some(sid) = resp
            .headers()
            .get("Mcp-Session-Id")
            .and_then(|v| v.to_str().ok())
        {
            *self.session.lock().await = Some(sid.to_string());
        }
        if !resp.status().is_success() {
            let s = resp.status();
            let t = resp.text().await.unwrap_or_default();
            anyhow::bail!("mcp http {s}: {t}");
        }
        let Some(want_id) = want_id else {
            return Ok(Value::Null);
        };
        let ct = resp
            .headers()
            .get("Content-Type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let text = resp.text().await?;

        // Collect candidate JSON-RPC messages: either a single JSON body, or the
        // `data:` payloads of an SSE stream.
        let msgs: Vec<Value> = if ct.contains("text/event-stream") {
            parse_sse_json_messages(&text)
        } else {
            serde_json::from_str::<Value>(&text).into_iter().collect()
        };
        for m in msgs {
            if m.get("id").and_then(|v| v.as_u64()) == Some(want_id) {
                if let Some(err) = m.get("error") {
                    anyhow::bail!("mcp error: {err}");
                }
                return Ok(m.get("result").cloned().unwrap_or(Value::Null));
            }
        }
        anyhow::bail!("mcp http: no response for id {want_id}");
    }
}

fn parse_sse_json_messages(text: &str) -> Vec<Value> {
    let mut messages = Vec::new();
    let mut data = String::new();

    for line in text.lines() {
        if line.is_empty() {
            push_sse_data_message(&mut messages, &mut data);
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        if field == "data" {
            data.push_str(value);
            data.push('\n');
        }
    }
    push_sse_data_message(&mut messages, &mut data);
    messages
}

fn push_sse_data_message(messages: &mut Vec<Value>, data: &mut String) {
    if data.is_empty() {
        return;
    }
    if data.ends_with('\n') {
        data.pop();
    }
    if let Ok(message) = serde_json::from_str::<Value>(data.trim()) {
        messages.push(message);
    }
    data.clear();
}

pub struct HttpOptions {
    pub bearer_token: String,
    pub bearer_token_env_var: String,
    pub headers: BTreeMap<String, String>,
    pub env_headers: BTreeMap<String, String>,
    pub request_timeout: std::time::Duration,
}

impl Default for HttpOptions {
    fn default() -> Self {
        Self {
            bearer_token: String::new(),
            bearer_token_env_var: String::new(),
            headers: BTreeMap::new(),
            env_headers: BTreeMap::new(),
            request_timeout: std::time::Duration::from_secs(30),
        }
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.post(&body, Some(id)).await
    }

    async fn notify(&self, method: &str, params: Value) -> anyhow::Result<()> {
        let body = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let result = self.post(&body, None).await;
        if let Err(error) = &result {
            tracing::warn!(method, error = %error, "mcp http notification failed");
        }
        result
            .map(|_| ())
            .with_context(|| format!("mcp http notification {method} failed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiline_sse_data_event() {
        let messages = parse_sse_json_messages(
            "event: message\n\
             data: {\"jsonrpc\":\"2.0\",\n\
             data: \"id\":1,\n\
             data: \"result\":{\"ok\":true}}\n\
             \n",
        );

        assert_eq!(
            messages,
            vec![json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": { "ok": true }
            })]
        );
    }

    #[test]
    fn parses_multiple_sse_data_events() {
        let messages = parse_sse_json_messages(
            "data: {\"id\":1,\"result\":\"one\"}\n\
             \n\
             data: {\"id\":2,\"result\":\"two\"}\n\
             \n",
        );

        assert_eq!(
            messages,
            vec![
                json!({ "id": 1, "result": "one" }),
                json!({ "id": 2, "result": "two" })
            ]
        );
    }
}
