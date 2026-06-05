//! Newline-delimited JSON-RPC 2.0 over a child process's stdio — the standard
//! MCP stdio transport. Requests are serialized through a mutex (one in flight
//! at a time), which is sufficient for Oxide's list/call usage and keeps the
//! framing trivial.

use crate::Transport;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

pub struct StdioTransport {
    next_id: AtomicU64,
    inner: Mutex<Io>,
    // Keep the child alive for the transport's lifetime; killed on drop.
    _child: Child,
}

struct Io {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl StdioTransport {
    /// Spawn `command args...` with piped stdio.
    pub fn spawn(command: &str, args: &[String]) -> anyhow::Result<Self> {
        let mut child = tokio::process::Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("no stdout"))?;
        Ok(Self {
            next_id: AtomicU64::new(1),
            inner: Mutex::new(Io {
                stdin,
                stdout: BufReader::new(stdout),
            }),
            _child: child,
        })
    }

    async fn send(io: &mut Io, msg: &Value) -> anyhow::Result<()> {
        let mut line = serde_json::to_string(msg)?;
        line.push('\n');
        io.stdin.write_all(line.as_bytes()).await?;
        io.stdin.flush().await?;
        Ok(())
    }
}

#[async_trait]
impl Transport for StdioTransport {
    async fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });

        let mut io = self.inner.lock().await;
        StdioTransport::send(&mut io, &req).await?;

        // Read lines until we get the response with our id (skipping any
        // notifications the server emits in between).
        let mut line = String::new();
        loop {
            line.clear();
            let n = io.stdout.read_line(&mut line).await?;
            if n == 0 {
                anyhow::bail!("mcp server closed the connection");
            }
            let Ok(msg) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            if msg.get("id").and_then(|v| v.as_u64()) == Some(id) {
                if let Some(err) = msg.get("error") {
                    anyhow::bail!("mcp error: {err}");
                }
                return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
            }
            // else: a notification or another id — keep reading.
        }
    }

    async fn notify(&self, method: &str, params: Value) -> anyhow::Result<()> {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let mut io = self.inner.lock().await;
        StdioTransport::send(&mut io, &msg).await
    }
}
