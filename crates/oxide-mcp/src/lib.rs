//! Model Context Protocol (MCP) client.
//!
//! Connects out to external MCP tool servers over newline-delimited JSON-RPC on
//! stdio, lists their tools, and exposes each as a native [`ToolSpec`] named
//! `mcp__<server>__<tool>`. The engine merges these into the model's tool set
//! and routes calls back here — so an MCP tool goes through the exact same
//! approval/sandbox chokepoint as a built-in tool.
//!
//! The [`Transport`] trait keeps the protocol logic testable without spawning a
//! real process: production uses [`StdioTransport`]; tests use an in-memory one.

use anyhow::Context;
use async_trait::async_trait;
use oxide_protocol::ToolSpec;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};

mod http;
mod stdio;
pub use http::{HttpOptions, HttpTransport};
pub use stdio::{StdioSpawnOptions, StdioTransport};

const PROTOCOL_VERSION: &str = "2024-11-05";

/// Separator used to namespace a server's tools: `mcp__<server>__<tool>`.
pub const PREFIX: &str = "mcp__";

/// A JSON-RPC request/notification channel to one MCP server.
#[async_trait]
pub trait Transport: Send + Sync {
    /// Send a request and await its result (the JSON-RPC `result` field).
    async fn call(&self, method: &str, params: Value) -> anyhow::Result<Value>;
    /// Send a notification (no response expected).
    async fn notify(&self, method: &str, params: Value) -> anyhow::Result<()>;
}

/// A connected MCP server, surfacing its tools to Oxide.
pub struct McpClient {
    server: String,
    transport: Box<dyn Transport>,
    next_id: AtomicU64,
    instructions: String,
}

impl McpClient {
    /// Wrap an already-constructed transport and run the MCP handshake.
    pub async fn connect(
        server: impl Into<String>,
        transport: Box<dyn Transport>,
    ) -> anyhow::Result<Self> {
        let mut client = Self {
            server: server.into(),
            transport,
            next_id: AtomicU64::new(1),
            instructions: String::new(),
        };
        client.instructions = client.initialize().await?;
        Ok(client)
    }

    /// Spawn `command args...` as a stdio MCP server and connect.
    pub async fn connect_stdio(
        server: impl Into<String>,
        command: &str,
        args: &[String],
    ) -> anyhow::Result<Self> {
        let transport = StdioTransport::spawn(command, args)?;
        Self::connect(server, Box::new(transport)).await
    }

    /// Spawn `command args...` with environment/cwd options and connect.
    pub async fn connect_stdio_with(
        server: impl Into<String>,
        command: &str,
        args: &[String],
        options: StdioSpawnOptions,
    ) -> anyhow::Result<Self> {
        let transport = StdioTransport::spawn_with(command, args, options)?;
        Self::connect(server, Box::new(transport)).await
    }

    /// Connect to a remote MCP server over Streamable HTTP/SSE.
    pub async fn connect_http(server: impl Into<String>, url: &str) -> anyhow::Result<Self> {
        Self::connect(server, Box::new(HttpTransport::new(url))).await
    }

    /// Connect to a remote MCP server over Streamable HTTP/SSE with auth/header options.
    pub async fn connect_http_with(
        server: impl Into<String>,
        url: &str,
        options: HttpOptions,
    ) -> anyhow::Result<Self> {
        Self::connect(server, Box::new(HttpTransport::new_with(url, options))).await
    }

    pub fn server(&self) -> &str {
        &self.server
    }

    pub fn instructions(&self) -> &str {
        &self.instructions
    }

    async fn initialize(&self) -> anyhow::Result<String> {
        let result = self
            .transport
            .call(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "oxide", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
            .await?;
        // Per spec, follow up with the initialized notification.
        self.transport
            .notify("notifications/initialized", json!({}))
            .await
            .with_context(|| {
                format!("mcp server {} initialized notification failed", self.server)
            })?;
        let _ = self.next_id.fetch_add(1, Ordering::Relaxed);
        Ok(result
            .get("instructions")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string())
    }

    /// List the server's tools as namespaced [`ToolSpec`]s.
    pub async fn list_tools(&self) -> anyhow::Result<Vec<ToolSpec>> {
        let result = self.transport.call("tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        let specs = tools
            .into_iter()
            .filter_map(|t| {
                let name = t.get("name")?.as_str()?.to_string();
                let description = t
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();
                let schema = t
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
                // MCP tools may mutate external state → always gated for approval.
                Some(
                    ToolSpec::new(format!("{PREFIX}{}__{}", self.server, name), description)
                        .params(schema)
                        .mutating(true),
                )
            })
            .collect();
        Ok(specs)
    }

    /// Call a tool by its namespaced name. Returns `(text_output, ok)`.
    pub async fn call_tool(
        &self,
        full_name: &str,
        arguments: &Value,
    ) -> anyhow::Result<(String, bool)> {
        let bare = strip_prefix(full_name, &self.server).unwrap_or(full_name);
        let result = self
            .transport
            .call(
                "tools/call",
                json!({ "name": bare, "arguments": arguments }),
            )
            .await?;
        let is_error = result
            .get("isError")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        let text = result
            .get("content")
            .and_then(|c| c.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|i| i.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        Ok((text, !is_error))
    }
}

/// True if `name` is an MCP tool (any server).
pub fn is_mcp_tool(name: &str) -> bool {
    name.starts_with(PREFIX)
}

/// Extract the server segment of an `mcp__<server>__<tool>` name.
pub fn server_of(name: &str) -> Option<&str> {
    name.strip_prefix(PREFIX)?.split("__").next()
}

/// Given `mcp__<server>__<tool>`, return the bare `<tool>` for that server.
fn strip_prefix<'a>(full: &'a str, server: &str) -> Option<&'a str> {
    full.strip_prefix(PREFIX)?
        .strip_prefix(server)?
        .strip_prefix("__")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Canned in-memory MCP server for tests.
    struct MockTransport {
        last_call: Mutex<Option<(String, Value)>>,
    }

    struct InitNotifyFailsTransport;

    #[async_trait]
    impl Transport for MockTransport {
        async fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
            *self.last_call.lock().unwrap() = Some((method.to_string(), params.clone()));
            Ok(match method {
                "initialize" => json!({ "protocolVersion": PROTOCOL_VERSION, "capabilities": {} }),
                "tools/list" => json!({
                    "tools": [
                        { "name": "echo", "description": "echo back",
                          "inputSchema": { "type": "object", "properties": { "msg": { "type": "string" } } } }
                    ]
                }),
                "tools/call" => json!({
                    "content": [ { "type": "text", "text": format!("called {}", params["name"]) } ],
                    "isError": false
                }),
                _ => json!({}),
            })
        }
        async fn notify(&self, _method: &str, _params: Value) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl Transport for InitNotifyFailsTransport {
        async fn call(&self, method: &str, _params: Value) -> anyhow::Result<Value> {
            Ok(match method {
                "initialize" => json!({ "protocolVersion": PROTOCOL_VERSION, "capabilities": {} }),
                _ => json!({}),
            })
        }

        async fn notify(&self, _method: &str, _params: Value) -> anyhow::Result<()> {
            anyhow::bail!("write failed")
        }
    }

    #[tokio::test]
    async fn lists_and_namespaces_tools() {
        let t = Box::new(MockTransport {
            last_call: Mutex::new(None),
        });
        let client = McpClient::connect("fs", t).await.unwrap();
        let tools = client.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "mcp__fs__echo");
        assert!(tools[0].mutating);
        assert!(is_mcp_tool(&tools[0].name));
        assert_eq!(server_of(&tools[0].name), Some("fs"));
    }

    #[tokio::test]
    async fn calls_strip_namespace_before_dispatch() {
        let t = Box::new(MockTransport {
            last_call: Mutex::new(None),
        });
        let client = McpClient::connect("fs", t).await.unwrap();
        let (out, ok) = client
            .call_tool("mcp__fs__echo", &json!({ "msg": "hi" }))
            .await
            .unwrap();
        assert!(ok);
        // The server should have received the bare tool name, not the namespaced one.
        assert!(out.contains("echo"));
    }

    #[tokio::test]
    async fn connect_surfaces_initialized_notification_failure() {
        let err = match McpClient::connect("fs", Box::new(InitNotifyFailsTransport)).await {
            Ok(_) => panic!("connect should fail when initialized notification fails"),
            Err(err) => err,
        };
        let message = format!("{err:#}");
        assert!(message.contains("mcp server fs initialized notification failed"));
        assert!(message.contains("write failed"));
    }
}
