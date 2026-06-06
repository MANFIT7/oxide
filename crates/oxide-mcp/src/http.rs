//! Streamable HTTP / SSE transport for remote MCP servers.
//!
//! JSON-RPC requests are POSTed to the endpoint. The server may answer with a
//! plain JSON body or an SSE stream (`text/event-stream`); both are handled. A
//! `Mcp-Session-Id` header returned on initialize is echoed on later requests.

use crate::Transport;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex;

pub struct HttpTransport {
    client: reqwest::Client,
    url: String,
    next_id: AtomicU64,
    session: Mutex<Option<String>>,
}

impl HttpTransport {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            url: url.into(),
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
        if let Some(sid) = self.session.lock().await.clone() {
            req = req.header("Mcp-Session-Id", sid);
        }
        let resp = req.json(body).send().await?;
        if let Some(sid) = resp.headers().get("Mcp-Session-Id").and_then(|v| v.to_str().ok()) {
            *self.session.lock().await = Some(sid.to_string());
        }
        if !resp.status().is_success() {
            let s = resp.status();
            let t = resp.text().await.unwrap_or_default();
            anyhow::bail!("mcp http {s}: {t}");
        }
        let ct = resp
            .headers()
            .get("Content-Type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let text = resp.text().await?;
        let Some(want_id) = want_id else { return Ok(Value::Null) };

        // Collect candidate JSON-RPC messages: either a single JSON body, or the
        // `data:` payloads of an SSE stream.
        let msgs: Vec<Value> = if ct.contains("text/event-stream") {
            text.lines()
                .filter_map(|l| l.strip_prefix("data:").map(str::trim))
                .filter_map(|d| serde_json::from_str::<Value>(d).ok())
                .collect()
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

#[async_trait]
impl Transport for HttpTransport {
    async fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.post(&body, Some(id)).await
    }

    async fn notify(&self, method: &str, params: Value) -> anyhow::Result<()> {
        let body = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let _ = self.post(&body, None).await;
        Ok(())
    }
}
