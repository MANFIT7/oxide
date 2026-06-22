//! Newline-delimited JSON-RPC 2.0 over a child process's stdio — the standard
//! MCP stdio transport. Requests are serialized through a mutex (one in flight
//! at a time), which is sufficient for Oxide's list/call usage and keeps the
//! framing trivial.

use crate::Transport;
use anyhow::Context;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

pub struct StdioTransport {
    next_id: AtomicU64,
    inner: Mutex<Io>,
    request_timeout: std::time::Duration,
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
        Self::spawn_with(command, args, StdioSpawnOptions::default())
    }

    /// Spawn `command args...` with optional cwd/env inherited from an existing MCP config.
    pub fn spawn_with(
        command: &str,
        args: &[String],
        options: StdioSpawnOptions,
    ) -> anyhow::Result<Self> {
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(cwd) = options.cwd {
            cmd.current_dir(cwd);
        }
        for (key, value) in options.env {
            cmd.env(key, value);
        }
        for key in options.env_vars {
            if let Ok(value) = std::env::var(&key) {
                cmd.env(key, value);
            }
        }
        let mut child = cmd.spawn()?;
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
            request_timeout: options.request_timeout,
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

        // Both the WRITE and the read are inside the timeout: a server that
        // stopped reading stdin would otherwise block write_all forever while
        // holding the transport mutex (wedging every later call).
        let mut line = String::new();
        let read = async {
            StdioTransport::send(&mut io, &req).await?;
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
            }
        };
        match tokio::time::timeout(self.request_timeout, read).await {
            Ok(r) => r,
            Err(_) => anyhow::bail!("mcp request timed out"),
        }
    }

    async fn notify(&self, method: &str, params: Value) -> anyhow::Result<()> {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let write = async {
            let mut io = self.inner.lock().await;
            StdioTransport::send(&mut io, &msg).await
        };
        match tokio::time::timeout(self.request_timeout, write).await {
            Ok(result) => result.with_context(|| format!("mcp notification {method} failed")),
            Err(_) => anyhow::bail!("mcp notification {method} timed out"),
        }
    }
}

pub struct StdioSpawnOptions {
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub env_vars: Vec<String>,
    pub request_timeout: std::time::Duration,
}

impl Default for StdioSpawnOptions {
    fn default() -> Self {
        Self {
            cwd: None,
            env: BTreeMap::new(),
            env_vars: Vec::new(),
            request_timeout: std::time::Duration::from_secs(60),
        }
    }
}
