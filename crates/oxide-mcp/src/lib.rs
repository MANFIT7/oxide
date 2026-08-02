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
use rmcp::model::{DiscoverResult, ProtocolVersion};
use serde_json::{json, Value};
use std::collections::HashSet;

mod http;
pub mod oauth;
pub mod providers;
mod secret;
mod stdio;
pub use http::{
    auth_challenge_from_error, oauth_coordinator_error_from_error, BearerTokenProvider,
    HttpAuthChallenge, HttpAuthChallengeKind, HttpOptions, HttpTransport, HttpTransportError,
};
pub use oauth::{
    clear_native_credentials, native_credential_store, native_oauth_coordinator, CredentialStore,
    LoopbackCallbackError, LoopbackCallbackServer, NativeCredentialStore, NativeOAuthCoordinator,
    OAuthAuthorizationLaunch, OAuthCallback, OAuthClientIdentity, OAuthCoordinatorError,
    OAuthCoordinatorStatus, OAuthDiscoverySource, OAuthStartRequest,
};
pub use providers::{
    SupabaseFeature, SupabaseMcpPreset, SupabasePresetBuilder, SupabasePresetError,
};
pub use secret::{redact_oauth_secrets, SecretString};
pub use stdio::{StdioSpawnOptions, StdioTransport};

const PROTOCOL_VERSION: &str = "2026-07-28";
const LEGACY_PROTOCOL_VERSION: &str = "2025-11-25";
const LEGACY_SUPPORTED_PROTOCOL_VERSIONS: &[&str] =
    &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const METHOD_NOT_FOUND: i64 = -32601;
const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;

/// Separator used to namespace a server's tools: `mcp__<server>__<tool>`.
pub const PREFIX: &str = "mcp__";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum McpLifecycle {
    Discover,
    LegacyInitialize,
}

/// A structured JSON-RPC error returned by an MCP server.
#[derive(Clone)]
pub struct McpJsonRpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl McpJsonRpcError {
    pub(crate) fn from_value(value: &Value) -> Self {
        let data = value.get("data").cloned().map(|data| {
            let redacted = redact_oauth_secrets(&data.to_string());
            serde_json::from_str(&redacted).unwrap_or(Value::String(redacted))
        });
        Self {
            code: value.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: redact_oauth_secrets(
                value
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown MCP JSON-RPC error"),
            ),
            data,
        }
    }
}

impl std::fmt::Debug for McpJsonRpcError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpJsonRpcError")
            .field("code", &self.code)
            .field("message", &redact_oauth_secrets(&self.message))
            .field(
                "data",
                &self
                    .data
                    .as_ref()
                    .map(|data| redact_oauth_secrets(&data.to_string())),
            )
            .finish()
    }
}

impl std::fmt::Display for McpJsonRpcError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "MCP JSON-RPC error {}: {}",
            self.code,
            redact_oauth_secrets(&self.message)
        )
    }
}

impl std::error::Error for McpJsonRpcError {}

pub fn json_rpc_error_from_error(error: &anyhow::Error) -> Option<&McpJsonRpcError> {
    error.downcast_ref::<McpJsonRpcError>()
}

/// A JSON-RPC request/notification channel to one MCP server.
#[async_trait]
pub trait Transport: Send + Sync {
    /// Send a request and await its result (the JSON-RPC `result` field).
    async fn call(&self, method: &str, params: Value) -> anyhow::Result<Value>;
    /// Send a notification (no response expected).
    async fn notify(&self, method: &str, params: Value) -> anyhow::Result<()>;
    /// Record the protocol version selected during lifecycle negotiation. HTTP
    /// uses this for the mandatory MCP-Protocol-Version request header.
    fn set_protocol_version(&self, _version: &str) {}
    /// Cache transport-specific routing metadata from a listed tool schema.
    fn register_tool_input_schema(
        &self,
        _tool_name: &str,
        _input_schema: &Value,
    ) -> Result<(), String> {
        Ok(())
    }
}

fn prepare_request_params(mut params: Value, protocol_version: &str) -> anyhow::Result<Value> {
    if protocol_version != PROTOCOL_VERSION {
        return Ok(params);
    }
    if params.is_null() {
        params = json!({});
    }
    let params = params
        .as_object_mut()
        .context("MCP 2026 request params must be a JSON object")?;
    let meta = params.entry("_meta").or_insert_with(|| json!({}));
    let meta = meta
        .as_object_mut()
        .context("MCP request _meta must be a JSON object")?;
    meta.insert(
        "io.modelcontextprotocol/protocolVersion".to_string(),
        Value::String(protocol_version.to_string()),
    );
    meta.insert(
        "io.modelcontextprotocol/clientInfo".to_string(),
        json!({ "name": "oxide", "version": env!("CARGO_PKG_VERSION") }),
    );
    meta.insert(
        "io.modelcontextprotocol/clientCapabilities".to_string(),
        json!({}),
    );
    Ok(Value::Object(params.clone()))
}

fn is_current_protocol(protocol_version: &str) -> bool {
    protocol_version == PROTOCOL_VERSION
}

fn highest_mutual_legacy_version(error: &McpJsonRpcError) -> Option<&'static str> {
    if error.code != UNSUPPORTED_PROTOCOL_VERSION {
        return None;
    }
    let supported = error
        .data
        .as_ref()
        .and_then(|data| data.get("supported"))
        .and_then(Value::as_array)?;
    LEGACY_SUPPORTED_PROTOCOL_VERSIONS
        .iter()
        .copied()
        .find(|candidate| {
            supported
                .iter()
                .any(|version| version.as_str() == Some(*candidate))
        })
}

fn requires_session_initialization(error: &anyhow::Error) -> bool {
    let Some(HttpTransportError::Status { status: 400, body }) =
        error.downcast_ref::<HttpTransportError>()
    else {
        return false;
    };
    let body = body.to_ascii_lowercase();
    body.contains("mcp-session-id") && body.contains("required") && body.contains("initialization")
}

/// A connected MCP server, surfacing its tools to Oxide.
pub struct McpClient {
    server: String,
    transport: Box<dyn Transport>,
    instructions: String,
    protocol_version: String,
    lifecycle: McpLifecycle,
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
            instructions: String::new(),
            protocol_version: String::new(),
            lifecycle: McpLifecycle::Discover,
        };
        let (instructions, protocol_version, lifecycle) = client.establish_lifecycle().await?;
        client.instructions = instructions;
        client.protocol_version = protocol_version;
        client.lifecycle = lifecycle;
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

    /// Connect over HTTP using a refresh-compatible bearer token provider.
    pub async fn connect_http_with_token_provider(
        server: impl Into<String>,
        url: &str,
        options: HttpOptions,
        token_provider: std::sync::Arc<dyn BearerTokenProvider>,
    ) -> anyhow::Result<Self> {
        Self::connect(
            server,
            Box::new(HttpTransport::new_with_token_provider(
                url,
                options,
                token_provider,
            )),
        )
        .await
    }

    pub fn server(&self) -> &str {
        &self.server
    }

    pub fn instructions(&self) -> &str {
        &self.instructions
    }

    pub fn protocol_version(&self) -> &str {
        &self.protocol_version
    }

    pub fn lifecycle(&self) -> McpLifecycle {
        self.lifecycle
    }

    async fn establish_lifecycle(&self) -> anyhow::Result<(String, String, McpLifecycle)> {
        self.transport.set_protocol_version(PROTOCOL_VERSION);
        match self.discover().await {
            Ok((instructions, protocol_version)) => {
                Ok((instructions, protocol_version, McpLifecycle::Discover))
            }
            Err(error)
                if json_rpc_error_from_error(&error)
                    .is_some_and(|error| error.code == METHOD_NOT_FOUND)
                    || requires_session_initialization(&error) =>
            {
                let (instructions, protocol_version) = self
                    .initialize_legacy_with_version(LEGACY_PROTOCOL_VERSION)
                    .await?;
                Ok((
                    instructions,
                    protocol_version,
                    McpLifecycle::LegacyInitialize,
                ))
            }
            Err(error) => {
                match json_rpc_error_from_error(&error).and_then(highest_mutual_legacy_version) {
                    Some(requested) => {
                        let (instructions, protocol_version) =
                            self.initialize_legacy_with_version(requested).await?;
                        Ok((
                            instructions,
                            protocol_version,
                            McpLifecycle::LegacyInitialize,
                        ))
                    }
                    None => Err(error),
                }
            }
        }
    }

    async fn discover(&self) -> anyhow::Result<(String, String)> {
        let mut retried_current = false;
        loop {
            let params = prepare_request_params(json!({}), PROTOCOL_VERSION)?;
            match self.transport.call("server/discover", params).await {
                Ok(result) => {
                    let result: DiscoverResult = serde_json::from_value(result)
                        .context("MCP server/discover returned an invalid result")?;
                    let current = ProtocolVersion::V_2026_07_28;
                    if !result.supported_versions.contains(&current) {
                        anyhow::bail!(
                            "MCP server does not support required protocol version {PROTOCOL_VERSION}; supported: {:?}",
                            result.supported_versions
                        );
                    }
                    self.transport.set_protocol_version(PROTOCOL_VERSION);
                    return Ok((
                        result.instructions.unwrap_or_default().trim().to_string(),
                        PROTOCOL_VERSION.to_string(),
                    ));
                }
                Err(error)
                    if !retried_current
                        && json_rpc_error_from_error(&error).is_some_and(|error| {
                            error.code == UNSUPPORTED_PROTOCOL_VERSION
                                && error
                                    .data
                                    .as_ref()
                                    .and_then(|data| data.get("supported"))
                                    .and_then(Value::as_array)
                                    .is_some_and(|versions| {
                                        versions.iter().any(|version| {
                                            version.as_str() == Some(PROTOCOL_VERSION)
                                        })
                                    })
                        }) =>
                {
                    retried_current = true;
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn initialize_legacy_with_version(
        &self,
        requested_version: &str,
    ) -> anyhow::Result<(String, String)> {
        self.transport.set_protocol_version(requested_version);
        let result = self
            .transport
            .call(
                "initialize",
                json!({
                    "protocolVersion": requested_version,
                    "capabilities": {},
                    "clientInfo": { "name": "oxide", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
            .await?;
        let negotiated = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or(requested_version);
        if !LEGACY_SUPPORTED_PROTOCOL_VERSIONS.contains(&negotiated) {
            anyhow::bail!(
                "mcp server {} selected unsupported protocol version {negotiated}",
                self.server
            );
        }
        self.transport.set_protocol_version(negotiated);
        // Per spec, follow up with the initialized notification.
        self.transport
            .notify("notifications/initialized", json!({}))
            .await
            .with_context(|| {
                format!("mcp server {} initialized notification failed", self.server)
            })?;
        Ok((
            result
                .get("instructions")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string(),
            negotiated.to_string(),
        ))
    }

    /// List the server's tools as namespaced [`ToolSpec`]s.
    pub async fn list_tools(&self) -> anyhow::Result<Vec<ToolSpec>> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen_cursors = HashSet::new();
        for page in 0..100 {
            let params = cursor
                .as_ref()
                .map(|value| json!({ "cursor": value }))
                .unwrap_or_else(|| json!({}));
            let result = self.call("tools/list", params).await?;
            tools.extend(
                result
                    .get("tools")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            );
            let next = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|value| !value.is_empty());
            let Some(next) = next else { break };
            if !seen_cursors.insert(next.clone()) {
                anyhow::bail!("mcp tools/list returned a repeated pagination cursor");
            }
            if page == 99 {
                anyhow::bail!("mcp tools/list exceeded 100 pagination pages");
            }
            cursor = Some(next);
        }
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
                if let Err(error) = self.transport.register_tool_input_schema(&name, &schema) {
                    tracing::warn!(
                        server = %self.server,
                        tool = %name,
                        error,
                        "excluded MCP tool with invalid x-mcp-header schema"
                    );
                    return None;
                }
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
            .call(
                "tools/call",
                json!({ "name": bare, "arguments": arguments }),
            )
            .await?;
        let is_error = result
            .get("isError")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        let text = render_tool_result(&result);
        Ok((text, !is_error))
    }

    async fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let params = prepare_request_params(params, &self.protocol_version)?;
        self.transport.call(method, params).await
    }
}

fn render_tool_result(result: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(items) = result.get("content").and_then(Value::as_array) {
        for item in items {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                parts.push(text.to_string());
                continue;
            }
            let mut safe = item.clone();
            if matches!(
                safe.get("type").and_then(Value::as_str),
                Some("image" | "audio")
            ) {
                if let Some(data) = safe.get_mut("data") {
                    let size = data.as_str().map(str::len).unwrap_or(0);
                    *data = Value::String(format!("<omitted {size} base64 chars>"));
                }
            }
            parts.push(serde_json::to_string_pretty(&safe).unwrap_or_else(|_| safe.to_string()));
        }
    }
    if let Some(structured) = result.get("structuredContent") {
        parts.push(format!(
            "[structuredContent]\n{}",
            serde_json::to_string_pretty(structured).unwrap_or_else(|_| structured.to_string())
        ));
    }
    if parts.is_empty() {
        serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string())
    } else {
        parts.join("\n")
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
    use std::sync::{Arc, Mutex};

    /// Canned in-memory MCP server for tests.
    struct MockTransport {
        last_call: Mutex<Option<(String, Value)>>,
    }

    struct InitNotifyFailsTransport;
    struct PaginatedTransport;
    struct UnsupportedVersionTransport;
    struct LegacyAdvertisedTransport {
        requested_version: Arc<Mutex<Option<String>>>,
    }
    struct RejectingToolSchemaTransport;
    struct DiscoverTransport {
        calls: Arc<Mutex<Vec<String>>>,
        notifications: Arc<Mutex<Vec<String>>>,
    }
    struct DiscoverFailureTransport {
        initialize_called: Arc<Mutex<bool>>,
    }

    fn method_not_found() -> anyhow::Error {
        McpJsonRpcError {
            code: METHOD_NOT_FOUND,
            message: "Method not found".to_string(),
            data: None,
        }
        .into()
    }

    #[async_trait]
    impl Transport for MockTransport {
        async fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
            *self.last_call.lock().unwrap() = Some((method.to_string(), params.clone()));
            if method == "server/discover" {
                return Err(method_not_found());
            }
            Ok(match method {
                "initialize" => {
                    json!({ "protocolVersion": LEGACY_PROTOCOL_VERSION, "capabilities": {} })
                }
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
            if method == "server/discover" {
                return Err(method_not_found());
            }
            Ok(match method {
                "initialize" => {
                    json!({ "protocolVersion": LEGACY_PROTOCOL_VERSION, "capabilities": {} })
                }
                _ => json!({}),
            })
        }

        async fn notify(&self, _method: &str, _params: Value) -> anyhow::Result<()> {
            anyhow::bail!("write failed")
        }
    }

    #[async_trait]
    impl Transport for PaginatedTransport {
        async fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
            if method == "server/discover" {
                return Err(method_not_found());
            }
            Ok(match method {
                "initialize" => {
                    json!({ "protocolVersion": LEGACY_PROTOCOL_VERSION, "capabilities": {} })
                }
                "tools/list" if params.get("cursor").is_none() => json!({
                    "tools": [{ "name": "one", "inputSchema": { "type": "object" } }],
                    "nextCursor": "page-2"
                }),
                "tools/list" => json!({
                    "tools": [{ "name": "two", "inputSchema": { "type": "object" } }]
                }),
                _ => json!({}),
            })
        }

        async fn notify(&self, _method: &str, _params: Value) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl Transport for UnsupportedVersionTransport {
        async fn call(&self, method: &str, _params: Value) -> anyhow::Result<Value> {
            if method == "server/discover" {
                return Err(method_not_found());
            }
            Ok(json!({ "protocolVersion": "2099-01-01", "capabilities": {} }))
        }

        async fn notify(&self, _method: &str, _params: Value) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl Transport for LegacyAdvertisedTransport {
        async fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
            match method {
                "server/discover" => Err(McpJsonRpcError {
                    code: UNSUPPORTED_PROTOCOL_VERSION,
                    message: "unsupported protocol version".to_string(),
                    data: Some(json!({ "supported": [LEGACY_PROTOCOL_VERSION] })),
                }
                .into()),
                "initialize" => {
                    *self.requested_version.lock().unwrap() = params
                        .get("protocolVersion")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    Ok(json!({
                        "protocolVersion": LEGACY_PROTOCOL_VERSION,
                        "capabilities": {}
                    }))
                }
                _ => Ok(json!({})),
            }
        }

        async fn notify(&self, _method: &str, _params: Value) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl Transport for RejectingToolSchemaTransport {
        async fn call(&self, method: &str, _params: Value) -> anyhow::Result<Value> {
            if method == "server/discover" {
                return Err(method_not_found());
            }
            Ok(match method {
                "initialize" => {
                    json!({ "protocolVersion": LEGACY_PROTOCOL_VERSION, "capabilities": {} })
                }
                "tools/list" => json!({
                    "tools": [{
                        "name": "unsafe-routing-schema",
                        "inputSchema": { "type": "object" }
                    }]
                }),
                _ => json!({}),
            })
        }

        async fn notify(&self, _method: &str, _params: Value) -> anyhow::Result<()> {
            Ok(())
        }

        fn register_tool_input_schema(
            &self,
            _tool_name: &str,
            _input_schema: &Value,
        ) -> Result<(), String> {
            Err("invalid x-mcp-header".to_string())
        }
    }

    #[async_trait]
    impl Transport for DiscoverTransport {
        async fn call(&self, method: &str, _params: Value) -> anyhow::Result<Value> {
            self.calls.lock().unwrap().push(method.to_string());
            Ok(match method {
                "server/discover" => json!({
                    "resultType": "complete",
                    "supportedVersions": [PROTOCOL_VERSION],
                    "capabilities": { "tools": {} },
                    "instructions": " current instructions ",
                    "ttlMs": 0,
                    "cacheScope": "private"
                }),
                "tools/list" => json!({ "tools": [] }),
                "initialize" => panic!("current lifecycle must not initialize"),
                _ => json!({}),
            })
        }

        async fn notify(&self, method: &str, _params: Value) -> anyhow::Result<()> {
            self.notifications.lock().unwrap().push(method.to_string());
            Ok(())
        }
    }

    #[async_trait]
    impl Transport for DiscoverFailureTransport {
        async fn call(&self, method: &str, _params: Value) -> anyhow::Result<Value> {
            if method == "initialize" {
                *self.initialize_called.lock().unwrap() = true;
            }
            anyhow::bail!("discover transport failed")
        }

        async fn notify(&self, _method: &str, _params: Value) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn lists_and_namespaces_tools() {
        let t = Box::new(MockTransport {
            last_call: Mutex::new(None),
        });
        let client = McpClient::connect("fs", t).await.unwrap();
        assert_eq!(client.protocol_version(), LEGACY_PROTOCOL_VERSION);
        assert_eq!(client.lifecycle(), McpLifecycle::LegacyInitialize);
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

    #[tokio::test]
    async fn tools_list_follows_pagination_cursor() {
        let client = McpClient::connect("paged", Box::new(PaginatedTransport))
            .await
            .unwrap();

        let tools = client.list_tools().await.unwrap();

        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "mcp__paged__one");
        assert_eq!(tools[1].name, "mcp__paged__two");
    }

    #[tokio::test]
    async fn unsupported_negotiated_protocol_version_is_rejected() {
        let error = match McpClient::connect("future", Box::new(UnsupportedVersionTransport)).await
        {
            Ok(_) => panic!("unsupported protocol version should fail"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("unsupported protocol version"));
    }

    #[tokio::test]
    async fn unsupported_current_with_advertised_legacy_uses_highest_mutual_version() {
        let requested_version = Arc::new(Mutex::new(None));
        let client = McpClient::connect(
            "legacy-advertised",
            Box::new(LegacyAdvertisedTransport {
                requested_version: requested_version.clone(),
            }),
        )
        .await
        .unwrap();

        assert_eq!(client.lifecycle(), McpLifecycle::LegacyInitialize);
        assert_eq!(client.protocol_version(), LEGACY_PROTOCOL_VERSION);
        assert_eq!(
            requested_version.lock().unwrap().as_deref(),
            Some(LEGACY_PROTOCOL_VERSION)
        );
    }

    #[tokio::test]
    async fn tools_with_invalid_transport_routing_schema_are_excluded() {
        let client = McpClient::connect("invalid-schema", Box::new(RejectingToolSchemaTransport))
            .await
            .unwrap();

        assert!(client.list_tools().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn current_lifecycle_uses_discover_without_initialized_notification() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let notifications = Arc::new(Mutex::new(Vec::new()));
        let transport = Box::new(DiscoverTransport {
            calls: calls.clone(),
            notifications: notifications.clone(),
        });
        let client = McpClient::connect("current", transport).await.unwrap();

        assert_eq!(client.protocol_version(), PROTOCOL_VERSION);
        assert_eq!(client.lifecycle(), McpLifecycle::Discover);
        assert_eq!(client.instructions(), "current instructions");
        client.list_tools().await.unwrap();
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            ["server/discover", "tools/list"]
        );
        assert!(notifications.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn auto_lifecycle_does_not_fallback_on_transport_failure() {
        let initialize_called = Arc::new(Mutex::new(false));
        let transport = Box::new(DiscoverFailureTransport {
            initialize_called: initialize_called.clone(),
        });

        let error = match McpClient::connect("offline", transport).await {
            Ok(_) => panic!("transport failure must not trigger legacy initialization"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("discover transport failed"));
        assert!(!*initialize_called.lock().unwrap());
    }

    #[test]
    fn current_request_metadata_is_complete_and_preserves_extensions() {
        let params = prepare_request_params(
            json!({ "_meta": { "example.test/extension": 7 } }),
            PROTOCOL_VERSION,
        )
        .unwrap();
        let meta = params.get("_meta").unwrap();

        assert_eq!(
            meta.get("io.modelcontextprotocol/protocolVersion"),
            Some(&json!(PROTOCOL_VERSION))
        );
        assert!(meta
            .get("io.modelcontextprotocol/clientCapabilities")
            .is_some());
        assert!(meta.get("io.modelcontextprotocol/clientInfo").is_some());
        assert_eq!(meta.get("example.test/extension"), Some(&json!(7)));
    }

    #[test]
    fn structured_and_non_text_tool_results_are_not_silently_dropped() {
        let rendered = render_tool_result(&json!({
            "content": [
                { "type": "image", "mimeType": "image/png", "data": "abcd" },
                { "type": "resource_link", "uri": "file:///tmp/report.json", "name": "report" }
            ],
            "structuredContent": { "count": 3 }
        }));

        assert!(rendered.contains("<omitted 4 base64 chars>"));
        assert!(rendered.contains("file:///tmp/report.json"));
        assert!(rendered.contains("structuredContent"));
        assert!(rendered.contains("\"count\": 3"));
    }
}
