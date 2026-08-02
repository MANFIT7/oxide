//! Streamable HTTP / SSE transport for remote MCP servers.
//!
//! JSON-RPC requests are POSTed to the endpoint. The server may answer with a
//! plain JSON body or an SSE stream (`text/event-stream`); both are handled. A
//! Legacy lifecycle connections echo `Mcp-Session-Id`; MCP 2026 connections
//! use per-request metadata and do not establish transport sessions.

use crate::oauth::OAuthCoordinatorError;
use crate::secret::{redact_oauth_secrets, SecretString};
use crate::{prepare_request_params, McpJsonRpcError, Transport};
use anyhow::Context;
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use futures::StreamExt;
use rmcp::transport::auth::WWWAuthenticateParams;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Supplies a fresh bearer token for every HTTP request.
#[async_trait]
pub trait BearerTokenProvider: Send + Sync {
    async fn bearer_token(&self) -> anyhow::Result<SecretString>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HttpAuthChallengeKind {
    AuthorizationRequired,
    InsufficientScope,
}

/// Structured 401/403 challenge returned by an MCP HTTP server.
#[derive(Clone, Eq, PartialEq)]
pub struct HttpAuthChallenge {
    kind: HttpAuthChallengeKind,
    status: u16,
    www_authenticate: Option<String>,
    resource_metadata_url: Option<String>,
    scope: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

impl std::fmt::Debug for HttpAuthChallenge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpAuthChallenge")
            .field("kind", &self.kind)
            .field("status", &self.status)
            .field(
                "www_authenticate",
                &self.www_authenticate.as_deref().map(redact_oauth_secrets),
            )
            .field("resource_metadata_url", &self.resource_metadata_url)
            .field("scope", &self.scope)
            .field("error", &self.error)
            .field("error_description", &self.error_description)
            .finish()
    }
}

impl HttpAuthChallenge {
    fn from_response(status: reqwest::StatusCode, header: Option<String>, endpoint: &str) -> Self {
        let parsed = reqwest::Url::parse(endpoint).ok().and_then(|base_url| {
            header
                .as_deref()
                .map(|header| WWWAuthenticateParams::parse(header, &base_url))
        });
        let kind = if status == reqwest::StatusCode::FORBIDDEN
            || parsed
                .as_ref()
                .is_some_and(WWWAuthenticateParams::is_insufficient_scope)
        {
            HttpAuthChallengeKind::InsufficientScope
        } else {
            HttpAuthChallengeKind::AuthorizationRequired
        };
        Self {
            kind,
            status: status.as_u16(),
            www_authenticate: header,
            resource_metadata_url: parsed
                .as_ref()
                .and_then(|params| params.resource_metadata_url.as_ref())
                .map(ToString::to_string),
            scope: parsed.as_ref().and_then(|params| params.scope.clone()),
            error: parsed.as_ref().and_then(|params| params.error.clone()),
            error_description: parsed
                .as_ref()
                .and_then(|params| params.error_description.clone())
                .map(|description| redact_oauth_secrets(&description)),
        }
    }

    pub fn kind(&self) -> HttpAuthChallengeKind {
        self.kind
    }

    pub fn status(&self) -> u16 {
        self.status
    }

    pub fn www_authenticate(&self) -> Option<&str> {
        self.www_authenticate.as_deref()
    }

    pub fn resource_metadata_url(&self) -> Option<&str> {
        self.resource_metadata_url.as_deref()
    }

    pub fn scope(&self) -> Option<&str> {
        self.scope.as_deref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn error_description(&self) -> Option<&str> {
        self.error_description.as_deref()
    }

    pub(crate) fn into_header(self) -> Option<String> {
        self.www_authenticate
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HttpTransportError {
    #[error("MCP HTTP authorization challenge ({status})", status = .0.status())]
    Authentication(HttpAuthChallenge),
    #[error("MCP HTTP OAuth token provider failed: {0}")]
    OAuthTokenProvider(#[source] OAuthCoordinatorError),
    #[error("MCP HTTP token provider failed: {0}")]
    TokenProvider(String),
    #[error("invalid MCP HTTP transport configuration: {0}")]
    InvalidConfiguration(String),
    #[error("invalid MCP HTTP request: {0}")]
    InvalidRequest(String),
    #[error("MCP HTTP request failed with status {status}: {body}")]
    Status { status: u16, body: String },
}

/// Extract a typed authentication challenge from an `anyhow` transport error.
pub fn auth_challenge_from_error(error: &anyhow::Error) -> Option<&HttpAuthChallenge> {
    match error.downcast_ref::<HttpTransportError>() {
        Some(HttpTransportError::Authentication(challenge)) => Some(challenge),
        _ => None,
    }
}

/// Extract a typed OAuth coordinator error from an HTTP transport failure.
pub fn oauth_coordinator_error_from_error(error: &anyhow::Error) -> Option<&OAuthCoordinatorError> {
    match error.downcast_ref::<HttpTransportError>() {
        Some(HttpTransportError::OAuthTokenProvider(error)) => Some(error),
        _ => error.downcast_ref::<OAuthCoordinatorError>(),
    }
}

#[derive(Clone, Debug)]
struct ToolParamHeader {
    argument_path: Vec<String>,
    header: String,
    primitive: ToolHeaderPrimitive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ToolHeaderPrimitive {
    String,
    Integer,
    Boolean,
}

pub struct HttpTransport {
    client: Result<reqwest::Client, String>,
    url: String,
    bearer_token: String,
    bearer_token_env_var: String,
    headers: BTreeMap<String, String>,
    env_headers: BTreeMap<String, String>,
    token_provider: Option<Arc<dyn BearerTokenProvider>>,
    next_id: AtomicU64,
    session: Mutex<Option<String>>,
    protocol_version: std::sync::RwLock<String>,
    tool_param_headers: std::sync::RwLock<BTreeMap<String, Vec<ToolParamHeader>>>,
}

impl HttpTransport {
    pub fn new(url: impl Into<String>) -> Self {
        Self::new_with(url, HttpOptions::default())
    }

    pub fn new_with(url: impl Into<String>, options: HttpOptions) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(options.request_timeout)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|error| redact_oauth_secrets(&error.to_string())),
            url: url.into(),
            bearer_token: options.bearer_token,
            bearer_token_env_var: options.bearer_token_env_var,
            headers: options.headers,
            env_headers: options.env_headers,
            token_provider: None,
            next_id: AtomicU64::new(1),
            session: Mutex::new(None),
            protocol_version: std::sync::RwLock::new(String::new()),
            tool_param_headers: std::sync::RwLock::new(BTreeMap::new()),
        }
    }

    /// Construct an HTTP transport that resolves and refreshes its bearer token per request.
    pub fn new_with_token_provider(
        url: impl Into<String>,
        options: HttpOptions,
        token_provider: Arc<dyn BearerTokenProvider>,
    ) -> Self {
        let mut transport = Self::new_with(url, options);
        transport.token_provider = Some(token_provider);
        transport
    }

    /// Cache the routing headers declared by one MCP tool input schema.
    pub fn register_tool_input_schema(
        &self,
        tool_name: &str,
        input_schema: &Value,
    ) -> Result<(), String> {
        let annotations = match tool_param_header_annotations(input_schema) {
            Ok(annotations) => annotations,
            Err(error) => {
                if let Ok(mut schemas) = self.tool_param_headers.write() {
                    schemas.remove(tool_name);
                }
                return Err(error);
            }
        };
        self.tool_param_headers
            .write()
            .map_err(|_| "tool header schema cache is poisoned".to_string())?
            .insert(tool_name.to_string(), annotations);
        Ok(())
    }

    async fn post(&self, body: &Value, want_id: Option<u64>) -> anyhow::Result<Value> {
        let managed_token_auth = self.token_provider.is_some()
            || !self.bearer_token.is_empty()
            || !self.bearer_token_env_var.is_empty();
        let custom_authorization = self
            .headers
            .keys()
            .chain(self.env_headers.keys())
            .any(|header| header.eq_ignore_ascii_case("authorization"));
        let custom_header_credentials = !self.env_headers.is_empty()
            || self
                .headers
                .keys()
                .any(|header| is_sensitive_custom_header(header));
        validate_http_endpoint(
            &self.url,
            managed_token_auth || custom_authorization || custom_header_credentials,
        )?;
        validate_custom_headers(&self.headers, &self.env_headers, managed_token_auth)?;
        let protocol_version = self
            .protocol_version
            .read()
            .map(|value| value.clone())
            .unwrap_or_default();
        let client = self.client.as_ref().map_err(|error| {
            HttpTransportError::InvalidConfiguration(format!(
                "failed to construct HTTP client: {error}"
            ))
        })?;
        let mut req = client
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        if crate::is_current_protocol(&protocol_version) {
            if let Some(method) = body.get("method").and_then(Value::as_str) {
                req = req.header("Mcp-Method", method);
                let name = match method {
                    "tools/call" | "prompts/get" => body
                        .get("params")
                        .and_then(|params| params.get("name"))
                        .and_then(Value::as_str),
                    "resources/read" | "resources/subscribe" | "resources/unsubscribe" => body
                        .get("params")
                        .and_then(|params| params.get("uri"))
                        .and_then(Value::as_str),
                    "tasks/get" | "tasks/update" | "tasks/cancel" => body
                        .get("params")
                        .and_then(|params| params.get("taskId"))
                        .and_then(Value::as_str),
                    _ => None,
                };
                if let Some(name) = name {
                    req = req.header("Mcp-Name", encode_standard_header_value(name));
                }
                if method == "tools/call" {
                    let params = body.get("params");
                    let tool_name = params
                        .and_then(|params| params.get("name"))
                        .and_then(Value::as_str);
                    let arguments = params.and_then(|params| params.get("arguments"));
                    if let Some(tool_name) = tool_name {
                        let annotations = self
                            .tool_param_headers
                            .read()
                            .ok()
                            .and_then(|schemas| schemas.get(tool_name).cloned())
                            .unwrap_or_default();
                        if let Some(arguments) = arguments {
                            for (header, value) in tool_parameter_headers(&annotations, arguments)?
                            {
                                req = req.header(header, encode_standard_header_value(&value));
                            }
                        }
                    }
                }
            }
        }
        if let Some(token_provider) = &self.token_provider {
            let token = token_provider
                .bearer_token()
                .await
                .map_err(token_provider_error)?;
            req = req.bearer_auth(token.expose_secret());
        } else if !self.bearer_token.is_empty() {
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
        if !crate::is_current_protocol(&protocol_version) {
            if let Some(sid) = self.session.lock().await.clone() {
                req = req.header("Mcp-Session-Id", sid);
            }
        }
        if !protocol_version.is_empty() {
            req = req.header("MCP-Protocol-Version", &protocol_version);
        }
        let resp = req.json(body).send().await?;
        if !crate::is_current_protocol(&protocol_version) {
            if let Some(sid) = resp
                .headers()
                .get("Mcp-Session-Id")
                .and_then(|v| v.to_str().ok())
            {
                *self.session.lock().await = Some(sid.to_string());
            }
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let www_authenticate = resp
                .headers()
                .get(reqwest::header::WWW_AUTHENTICATE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let body = resp.text().await.unwrap_or_default();
            if matches!(
                status,
                reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
            ) {
                return Err(
                    HttpTransportError::Authentication(HttpAuthChallenge::from_response(
                        status,
                        www_authenticate,
                        &self.url,
                    ))
                    .into(),
                );
            }
            if let Some(error) = non_success_json_rpc_error(&body, want_id) {
                return Err(error.into());
            }
            return Err(HttpTransportError::Status {
                status: status.as_u16(),
                body: redact_oauth_secrets(&body),
            }
            .into());
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
        if ct.contains("text/event-stream") {
            return read_sse_response(resp, want_id).await;
        }
        let text = resp.text().await?;
        let msgs: Vec<Value> = serde_json::from_str::<Value>(&text).into_iter().collect();
        for m in msgs {
            if m.get("id").and_then(|v| v.as_u64()) == Some(want_id) {
                if let Some(err) = m.get("error") {
                    return Err(McpJsonRpcError::from_value(err).into());
                }
                return Ok(m.get("result").cloned().unwrap_or(Value::Null));
            }
        }
        anyhow::bail!("mcp http: no response for id {want_id}");
    }
}

fn non_success_json_rpc_error(body: &str, want_id: Option<u64>) -> Option<McpJsonRpcError> {
    let want_id = want_id?;
    let response = serde_json::from_str::<Value>(body).ok()?;
    if response.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || response.get("id").and_then(Value::as_u64) != Some(want_id)
    {
        return None;
    }
    response.get("error").map(McpJsonRpcError::from_value)
}

fn token_provider_error(error: anyhow::Error) -> HttpTransportError {
    match error.downcast::<OAuthCoordinatorError>() {
        Ok(error) => HttpTransportError::OAuthTokenProvider(error),
        Err(error) => HttpTransportError::TokenProvider(redact_oauth_secrets(&error.to_string())),
    }
}

fn validate_http_endpoint(
    endpoint: &str,
    credentials_managed_by_transport: bool,
) -> Result<(), HttpTransportError> {
    let url = reqwest::Url::parse(endpoint)
        .map_err(|error| HttpTransportError::InvalidConfiguration(error.to_string()))?;
    let is_loopback = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    if !matches!(url.scheme(), "http" | "https") {
        return Err(HttpTransportError::InvalidConfiguration(
            "MCP HTTP endpoint must use HTTP or HTTPS".to_string(),
        ));
    }
    if credentials_managed_by_transport
        && url.scheme() != "https"
        && !(url.scheme() == "http" && is_loopback)
    {
        return Err(HttpTransportError::InvalidConfiguration(
            "HTTPS is required for bearer/OAuth credentials except on loopback".to_string(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(HttpTransportError::InvalidConfiguration(
            "userinfo and URL fragments are not allowed in MCP endpoints".to_string(),
        ));
    }
    Ok(())
}

fn validate_custom_headers(
    headers: &BTreeMap<String, String>,
    env_headers: &BTreeMap<String, String>,
    managed_auth: bool,
) -> Result<(), HttpTransportError> {
    let custom_authorization_count = headers
        .keys()
        .chain(env_headers.keys())
        .filter(|header| header.eq_ignore_ascii_case("authorization"))
        .count();
    if custom_authorization_count > 1 || (custom_authorization_count == 1 && managed_auth) {
        return Err(HttpTransportError::InvalidConfiguration(
            "custom Authorization conflicts with another configured credential source".to_string(),
        ));
    }
    for header in headers.keys().chain(env_headers.keys()) {
        if reqwest::header::HeaderName::from_bytes(header.as_bytes()).is_err() {
            return Err(HttpTransportError::InvalidConfiguration(format!(
                "invalid custom header name {header:?}"
            )));
        }
        let normalized = header.to_ascii_lowercase();
        if matches!(
            normalized.as_str(),
            "accept"
                | "connection"
                | "content-length"
                | "content-type"
                | "host"
                | "mcp-method"
                | "mcp-name"
                | "mcp-protocol-version"
                | "mcp-session-id"
                | "proxy-authorization"
                | "transfer-encoding"
        ) || normalized.starts_with("mcp-param-")
        {
            return Err(HttpTransportError::InvalidConfiguration(format!(
                "custom header {header:?} is reserved by the HTTP/MCP transport"
            )));
        }
    }
    Ok(())
}

fn is_sensitive_custom_header(header: &str) -> bool {
    let normalized = header.to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "api-key"
            | "apikey"
            | "authorization"
            | "cookie"
            | "proxy-authorization"
            | "x-access-token"
            | "x-api-key"
            | "x-auth-token"
    ) || normalized.ends_with("-api-key")
        || normalized.ends_with("-secret")
        || normalized.ends_with("-token")
}

fn tool_param_header_annotations(input_schema: &Value) -> Result<Vec<ToolParamHeader>, String> {
    let mut seen_headers = HashSet::new();
    let mut annotations = Vec::new();
    collect_tool_param_header_annotations(
        input_schema,
        &mut Vec::new(),
        &mut seen_headers,
        &mut annotations,
    )?;
    if count_header_annotations(input_schema) != annotations.len() {
        return Err(
            "x-mcp-header must be attached directly to an object property schema".to_string(),
        );
    }
    Ok(annotations)
}

fn collect_tool_param_header_annotations(
    schema: &Value,
    path: &mut Vec<String>,
    seen_headers: &mut HashSet<String>,
    annotations: &mut Vec<ToolParamHeader>,
) -> Result<(), String> {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Ok(());
    };
    for (argument, property_schema) in properties {
        path.push(argument.clone());
        if let Some(raw_header) = property_schema.get("x-mcp-header") {
            let display_path = path.join(".");
            let header = raw_header.as_str().ok_or_else(|| {
                format!("property {display_path:?} x-mcp-header must be a string")
            })?;
            if header.is_empty() || !header.chars().all(is_http_token_character) {
                return Err(format!(
                    "property {display_path:?} has invalid x-mcp-header {header:?}"
                ));
            }
            if !seen_headers.insert(header.to_ascii_lowercase()) {
                return Err(format!("duplicate x-mcp-header {header:?}"));
            }
            let primitive = match property_schema.get("type").and_then(Value::as_str) {
                Some("string") => ToolHeaderPrimitive::String,
                Some("integer") => {
                    validate_safe_integer_schema(property_schema, &display_path)?;
                    ToolHeaderPrimitive::Integer
                }
                Some("boolean") => ToolHeaderPrimitive::Boolean,
                _ => {
                    return Err(format!(
                        "property {display_path:?} x-mcp-header requires string, integer, or boolean"
                    ));
                }
            };
            annotations.push(ToolParamHeader {
                argument_path: path.clone(),
                header: format!("Mcp-Param-{header}"),
                primitive,
            });
        }
        collect_tool_param_header_annotations(property_schema, path, seen_headers, annotations)?;
        path.pop();
    }
    Ok(())
}

fn count_header_annotations(value: &Value) -> usize {
    match value {
        Value::Object(fields) => {
            usize::from(fields.contains_key("x-mcp-header"))
                + fields.values().map(count_header_annotations).sum::<usize>()
        }
        Value::Array(items) => items.iter().map(count_header_annotations).sum(),
        _ => 0,
    }
}

fn validate_safe_integer_schema(schema: &Value, path: &str) -> Result<(), String> {
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
    for bound in ["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"] {
        let Some(value) = schema.get(bound) else {
            continue;
        };
        let Some(value) = value.as_f64() else {
            return Err(format!("property {path:?} has non-numeric {bound}"));
        };
        if value.abs() > MAX_SAFE_INTEGER {
            return Err(format!(
                "property {path:?} {bound} exceeds the JavaScript safe integer range"
            ));
        }
    }
    Ok(())
}

fn is_http_token_character(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(
            character,
            '!' | '#'
                | '$'
                | '%'
                | '&'
                | '\''
                | '*'
                | '+'
                | '-'
                | '.'
                | '^'
                | '_'
                | '`'
                | '|'
                | '~'
        )
}

fn tool_parameter_headers(
    annotations: &[ToolParamHeader],
    arguments: &Value,
) -> Result<Vec<(String, String)>, HttpTransportError> {
    let mut headers = Vec::new();
    for annotation in annotations {
        let Some(value) = value_at_argument_path(arguments, &annotation.argument_path) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let path = annotation.argument_path.join(".");
        let value = match annotation.primitive {
            ToolHeaderPrimitive::String => value.as_str().map(str::to_string),
            ToolHeaderPrimitive::Boolean => value.as_bool().map(|value| value.to_string()),
            ToolHeaderPrimitive::Integer => safe_integer_header_value(value),
        }
        .ok_or_else(|| {
            HttpTransportError::InvalidRequest(format!(
                "tool argument {path:?} is not a valid {:?} header value",
                annotation.primitive
            ))
        })?;
        headers.push((annotation.header.clone(), value));
    }
    Ok(headers)
}

fn value_at_argument_path<'a>(arguments: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter()
        .try_fold(arguments, |value, segment| value.get(segment))
}

fn safe_integer_header_value(value: &Value) -> Option<String> {
    const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
    if let Some(value) = value.as_i64() {
        return (value >= -MAX_SAFE_INTEGER && value <= MAX_SAFE_INTEGER)
            .then(|| value.to_string());
    }
    value
        .as_u64()
        .filter(|value| *value <= MAX_SAFE_INTEGER as u64)
        .map(|value| value.to_string())
}

fn encode_standard_header_value(value: &str) -> String {
    let bytes = value.as_bytes();
    let needs_base64 = !value.is_empty()
        && (matches!(bytes.first(), Some(b' ' | b'\t'))
            || matches!(bytes.last(), Some(b' ' | b'\t'))
            || value
                .chars()
                .any(|character| (character as u32) < 0x20 || (character as u32) > 0x7e)
            || (value.starts_with("=?base64?") && value.ends_with("?=")));
    if needs_base64 {
        format!("=?base64?{}?=", BASE64_STANDARD.encode(value))
    } else {
        value.to_string()
    }
}

async fn read_sse_response(resp: reqwest::Response, want_id: u64) -> anyhow::Result<Value> {
    let mut stream = resp.bytes_stream();
    let mut pending = Vec::new();
    while let Some(chunk) = stream.next().await {
        pending.extend_from_slice(&chunk?);
        for message in take_complete_sse_messages(&mut pending) {
            if let Some(result) = response_result_for_id(message, want_id)? {
                return Ok(result);
            }
        }
    }
    if !pending.is_empty() {
        let tail = String::from_utf8_lossy(&pending);
        for message in parse_sse_json_messages(&tail) {
            if let Some(result) = response_result_for_id(message, want_id)? {
                return Ok(result);
            }
        }
    }
    anyhow::bail!("mcp http: SSE stream ended without response for id {want_id}")
}

fn response_result_for_id(message: Value, want_id: u64) -> anyhow::Result<Option<Value>> {
    if message.get("id").and_then(Value::as_u64) != Some(want_id) {
        return Ok(None);
    }
    if let Some(error) = message.get("error") {
        return Err(McpJsonRpcError::from_value(error).into());
    }
    Ok(Some(message.get("result").cloned().unwrap_or(Value::Null)))
}

fn take_complete_sse_messages(pending: &mut Vec<u8>) -> Vec<Value> {
    let mut messages = Vec::new();
    while let Some((end, delimiter_len)) = sse_event_end(pending) {
        let event = pending.drain(..end + delimiter_len).collect::<Vec<_>>();
        let text = String::from_utf8_lossy(&event);
        messages.extend(parse_sse_json_messages(&text));
    }
    messages
}

fn sse_event_end(bytes: &[u8]) -> Option<(usize, usize)> {
    let lf = bytes.windows(2).position(|window| window == b"\n\n");
    let crlf = bytes.windows(4).position(|window| window == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(left), Some(right)) if left <= right => Some((left, 2)),
        (Some(_), Some(right)) => Some((right, 4)),
        (Some(left), None) => Some((left, 2)),
        (None, Some(right)) => Some((right, 4)),
        (None, None) => None,
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
        let protocol_version = self
            .protocol_version
            .read()
            .map(|value| value.clone())
            .unwrap_or_default();
        let params = prepare_request_params(params, &protocol_version)?;
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.post(&body, Some(id)).await
    }

    async fn notify(&self, method: &str, params: Value) -> anyhow::Result<()> {
        let protocol_version = self
            .protocol_version
            .read()
            .map(|value| value.clone())
            .unwrap_or_default();
        let params = prepare_request_params(params, &protocol_version)?;
        let body = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let result = self.post(&body, None).await;
        if let Err(error) = &result {
            tracing::warn!(method, error = %error, "mcp http notification failed");
        }
        result
            .map(|_| ())
            .with_context(|| format!("mcp http notification {method} failed"))
    }

    fn set_protocol_version(&self, version: &str) {
        if let Ok(mut current) = self.protocol_version.write() {
            *current = version.to_string();
        }
    }

    fn register_tool_input_schema(
        &self,
        tool_name: &str,
        input_schema: &Value,
    ) -> Result<(), String> {
        HttpTransport::register_tool_input_schema(self, tool_name, input_schema)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct ReauthorizationTokenProvider;

    #[async_trait]
    impl BearerTokenProvider for ReauthorizationTokenProvider {
        async fn bearer_token(&self) -> anyhow::Result<SecretString> {
            Err(OAuthCoordinatorError::AuthorizationRequired.into())
        }
    }

    async fn read_http_request(socket: &mut tokio::net::TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 2048];
        loop {
            let count = socket.read(&mut chunk).await.unwrap();
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..count]);
            let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&bytes[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if bytes.len() >= header_end + 4 + content_length {
                break;
            }
        }
        String::from_utf8_lossy(&bytes).to_string()
    }

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

    #[test]
    fn chunked_sse_parser_waits_for_complete_event() {
        let mut pending = b"data: {\"id\":1,".to_vec();
        assert!(take_complete_sse_messages(&mut pending).is_empty());
        pending.extend_from_slice(b"\"result\":{\"ok\":true}}\n\n");

        let messages = take_complete_sse_messages(&mut pending);

        assert_eq!(messages, vec![json!({ "id": 1, "result": { "ok": true } })]);
        assert!(pending.is_empty());
    }

    #[test]
    fn standard_header_value_base64_wraps_non_ascii_names() {
        assert_eq!(encode_standard_header_value("deploy"), "deploy");
        assert_eq!(
            encode_standard_header_value("alat/terjemah"),
            "alat/terjemah"
        );
        assert_eq!(
            encode_standard_header_value("alat-ñ"),
            "=?base64?YWxhdC3DsQ==?="
        );
    }

    #[test]
    fn tool_schema_promotes_annotated_primitive_arguments() {
        let annotations = tool_param_header_annotations(&json!({
            "type": "object",
            "properties": {
                "region": { "type": "string", "x-mcp-header": "Region" },
                "dry_run": { "type": "boolean", "x-mcp-header": "Dry-Run" }
            }
        }))
        .unwrap();

        assert_eq!(annotations.len(), 2);
        assert!(annotations.iter().any(|annotation| {
            annotation.argument_path == ["region".to_string()]
                && annotation.header == "Mcp-Param-Region"
        }));
    }

    #[test]
    fn nested_tool_headers_are_promoted_and_invalid_schemas_are_rejected() {
        let annotations = tool_param_header_annotations(&json!({
            "type": "object",
            "properties": {
                "deployment": {
                    "type": "object",
                    "properties": {
                        "region": { "type": "string", "x-mcp-header": "Region" }
                    }
                }
            }
        }))
        .unwrap();
        let headers = tool_parameter_headers(
            &annotations,
            &json!({ "deployment": { "region": "ap-southeast-1" } }),
        )
        .unwrap();

        assert_eq!(
            annotations[0].argument_path,
            ["deployment".to_string(), "region".to_string()]
        );
        assert_eq!(
            headers,
            [("Mcp-Param-Region".to_string(), "ap-southeast-1".to_string())]
        );
        assert!(tool_param_header_annotations(&json!({
            "type": "object",
            "properties": {
                "one": { "type": "string", "x-mcp-header": "Route" },
                "two": { "type": "boolean", "x-mcp-header": "route" }
            }
        }))
        .is_err());
        assert!(tool_param_header_annotations(&json!({
            "type": "object",
            "properties": {
                "object": { "type": "object", "x-mcp-header": "Invalid" }
            }
        }))
        .is_err());
    }

    #[test]
    fn tool_header_integer_must_be_javascript_safe() {
        let annotations = tool_param_header_annotations(&json!({
            "type": "object",
            "properties": {
                "sequence": { "type": "integer", "x-mcp-header": "Sequence" }
            }
        }))
        .unwrap();

        assert!(tool_parameter_headers(
            &annotations,
            &json!({ "sequence": 9_007_199_254_740_992_u64 })
        )
        .is_err());
        assert!(tool_param_header_annotations(&json!({
            "type": "object",
            "properties": {
                "sequence": {
                    "type": "integer",
                    "maximum": 9_007_199_254_740_992_u64,
                    "x-mcp-header": "Sequence"
                }
            }
        }))
        .is_err());
    }

    #[test]
    fn negotiated_protocol_version_is_retained_for_http_headers() {
        let transport = HttpTransport::new("https://example.com/mcp");

        transport.set_protocol_version("2025-11-25");

        assert_eq!(
            transport.protocol_version.read().unwrap().as_str(),
            "2025-11-25"
        );
    }

    #[tokio::test]
    async fn current_protocol_headers_are_sent_without_legacy_session() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let request = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0u8; 1024];
            while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = socket.read(&mut chunk).await.unwrap();
                if count == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..count]);
            }
            let request = String::from_utf8_lossy(&bytes).to_string();
            let body = r#"{"jsonrpc":"2.0","id":1,"result":{}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            request
        });
        let transport = HttpTransport::new(format!("http://{address}/mcp"));
        transport.set_protocol_version(crate::PROTOCOL_VERSION);
        *transport.session.lock().await = Some("legacy-session".to_string());

        transport.call("ping", json!({})).await.unwrap();
        let request = request.await.unwrap().to_ascii_lowercase();

        assert!(request.contains(&format!(
            "mcp-protocol-version: {}",
            crate::PROTOCOL_VERSION
        )));
        assert!(request.contains("mcp-method: ping"));
        assert!(!request.contains("mcp-session-id"));
    }

    #[tokio::test]
    async fn first_http_discover_uses_current_wire_metadata() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let request = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_http_request(&mut socket).await;
            let body = format!(
                r#"{{"jsonrpc":"2.0","id":1,"result":{{"resultType":"complete","supportedVersions":["{}"],"capabilities":{{}},"ttlMs":0,"cacheScope":"private"}}}}"#,
                crate::PROTOCOL_VERSION
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            request
        });

        let client =
            crate::McpClient::connect_http("wire-current", &format!("http://{address}/mcp"))
                .await
                .unwrap();
        let request = request.await.unwrap();
        let headers = request
            .split_once("\r\n\r\n")
            .map(|(headers, _)| headers.to_ascii_lowercase())
            .unwrap();
        let body = request
            .split_once("\r\n\r\n")
            .map(|(_, body)| serde_json::from_str::<Value>(body).unwrap())
            .unwrap();

        assert_eq!(client.lifecycle(), crate::McpLifecycle::Discover);
        assert!(headers.contains(&format!(
            "mcp-protocol-version: {}",
            crate::PROTOCOL_VERSION
        )));
        assert!(headers.contains("mcp-method: server/discover"));
        assert_eq!(body.get("method"), Some(&json!("server/discover")));
        assert_eq!(
            body.pointer("/params/_meta/io.modelcontextprotocol~1protocolVersion"),
            Some(&json!(crate::PROTOCOL_VERSION))
        );
    }

    #[tokio::test]
    async fn http_404_method_not_found_falls_back_with_legacy_wire_version() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = tokio::spawn(async move {
            let mut requests = Vec::new();
            for step in 0..3 {
                let (mut socket, _) = listener.accept().await.unwrap();
                requests.push(read_http_request(&mut socket).await);
                let (status, body) = match step {
                    0 => (
                        "404 Not Found",
                        r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}"#.to_string(),
                    ),
                    1 => (
                        "200 OK",
                        format!(
                            r#"{{"jsonrpc":"2.0","id":2,"result":{{"protocolVersion":"{}","capabilities":{{}}}}}}"#,
                            crate::LEGACY_PROTOCOL_VERSION
                        ),
                    ),
                    _ => ("202 Accepted", String::new()),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
            requests
        });

        let client =
            crate::McpClient::connect_http("wire-legacy", &format!("http://{address}/mcp"))
                .await
                .unwrap();
        let requests = requests.await.unwrap();
        let discover_headers = requests[0]
            .split_once("\r\n\r\n")
            .unwrap()
            .0
            .to_ascii_lowercase();
        let initialize_headers = requests[1]
            .split_once("\r\n\r\n")
            .unwrap()
            .0
            .to_ascii_lowercase();
        let initialize_body = requests[1].split_once("\r\n\r\n").unwrap().1;

        assert_eq!(client.lifecycle(), crate::McpLifecycle::LegacyInitialize);
        assert!(discover_headers.contains(&format!(
            "mcp-protocol-version: {}",
            crate::PROTOCOL_VERSION
        )));
        assert!(discover_headers.contains("mcp-method: server/discover"));
        assert!(initialize_headers.contains(&format!(
            "mcp-protocol-version: {}",
            crate::LEGACY_PROTOCOL_VERSION
        )));
        assert!(!initialize_headers.contains("mcp-method:"));
        assert_eq!(
            serde_json::from_str::<Value>(initialize_body)
                .unwrap()
                .pointer("/params/protocolVersion"),
            Some(&json!(crate::LEGACY_PROTOCOL_VERSION))
        );
    }

    #[tokio::test]
    async fn generic_http_failure_does_not_trigger_legacy_fallback() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_http_request(&mut socket).await;
            socket
                .write_all(
                    b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnope",
                )
                .await
                .unwrap();
            let second =
                tokio::time::timeout(std::time::Duration::from_millis(250), listener.accept())
                    .await;
            (request, second.is_ok())
        });

        let error =
            match crate::McpClient::connect_http("wire-error", &format!("http://{address}/mcp"))
                .await
            {
                Ok(_) => panic!("generic HTTP failure must not fall back"),
                Err(error) => error,
            };
        let (request, saw_second_request) = server.await.unwrap();

        assert!(error.to_string().contains("status 500"));
        assert!(request.contains("server/discover"));
        assert!(!saw_second_request);
    }

    #[tokio::test]
    async fn current_tool_call_sends_schema_promoted_parameter_headers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let request = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0_u8; 1024];
            while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = socket.read(&mut chunk).await.unwrap();
                if count == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..count]);
            }
            let request = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
            let body = r#"{"jsonrpc":"2.0","id":1,"result":{}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            request
        });
        let transport = HttpTransport::new(format!("http://{address}/mcp"));
        transport.set_protocol_version(crate::PROTOCOL_VERSION);
        transport
            .register_tool_input_schema(
                "deploy",
                &json!({
                    "type": "object",
                    "properties": {
                        "region": { "type": "string", "x-mcp-header": "Region" }
                    }
                }),
            )
            .unwrap();

        transport
            .call(
                "tools/call",
                json!({ "name": "deploy", "arguments": { "region": "ap-southeast-1" } }),
            )
            .await
            .unwrap();

        assert!(request
            .await
            .unwrap()
            .contains("mcp-param-region: ap-southeast-1"));
    }

    #[tokio::test]
    async fn redirects_do_not_forward_requests_or_authorization_headers() {
        let target_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_address = target_listener.local_addr().unwrap();
        let target_request = tokio::spawn(async move {
            tokio::time::timeout(
                std::time::Duration::from_millis(250),
                target_listener.accept(),
            )
            .await
        });

        let source_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source_address = source_listener.local_addr().unwrap();
        let source_request = tokio::spawn(async move {
            let (mut socket, _) = source_listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0_u8; 1024];
            while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = socket.read(&mut chunk).await.unwrap();
                if count == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..count]);
            }
            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: http://{target_address}/capture\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&bytes).to_ascii_lowercase()
        });
        let options = HttpOptions {
            headers: BTreeMap::from([(
                "Authorization".to_string(),
                "Bearer redirect-secret".to_string(),
            )]),
            ..HttpOptions::default()
        };
        let transport = HttpTransport::new_with(format!("http://{source_address}/mcp"), options);

        let error = transport.call("ping", json!({})).await.unwrap_err();

        assert!(matches!(
            error.downcast_ref::<HttpTransportError>(),
            Some(HttpTransportError::Status { status: 302, .. })
        ));
        assert!(source_request
            .await
            .unwrap()
            .contains("authorization: bearer redirect-secret"));
        assert!(target_request.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn token_provider_preserves_typed_reauthorization_error() {
        let transport = HttpTransport::new_with_token_provider(
            "https://example.com/mcp",
            HttpOptions::default(),
            Arc::new(ReauthorizationTokenProvider),
        );

        let error = transport.call("ping", json!({})).await.unwrap_err();

        assert!(matches!(
            oauth_coordinator_error_from_error(&error),
            Some(OAuthCoordinatorError::AuthorizationRequired)
        ));
    }

    #[tokio::test]
    async fn rejects_insecure_credentials_and_conflicting_custom_authorization() {
        assert!(validate_http_endpoint("http://example.com/mcp", false).is_ok());
        let error = validate_http_endpoint("http://example.com/mcp", true).unwrap_err();
        assert!(matches!(error, HttpTransportError::InvalidConfiguration(_)));
        assert!(validate_custom_headers(
            &BTreeMap::from([("Authorization".to_string(), "Bearer legacy".to_string())]),
            &BTreeMap::new(),
            false,
        )
        .is_ok());

        let options = HttpOptions {
            bearer_token_env_var: "OXIDE_TEST_TOKEN".to_string(),
            headers: BTreeMap::from([("Authorization".to_string(), "Bearer unsafe".to_string())]),
            ..HttpOptions::default()
        };
        let conflicting = HttpTransport::new_with("https://example.com/mcp", options);
        let error = conflicting.call("ping", json!({})).await.unwrap_err();
        assert!(matches!(
            error.downcast_ref::<HttpTransportError>(),
            Some(HttpTransportError::InvalidConfiguration(_))
        ));
    }

    #[tokio::test]
    async fn rejects_remote_http_for_env_and_sensitive_static_headers() {
        let env_backed = HttpTransport::new_with(
            "http://example.com/mcp",
            HttpOptions {
                env_headers: BTreeMap::from([(
                    "X-API-Key".to_string(),
                    "OXIDE_TEST_API_KEY".to_string(),
                )]),
                ..HttpOptions::default()
            },
        );
        let error = env_backed.call("ping", json!({})).await.unwrap_err();
        assert!(matches!(
            error.downcast_ref::<HttpTransportError>(),
            Some(HttpTransportError::InvalidConfiguration(_))
        ));

        let static_cookie = HttpTransport::new_with(
            "http://example.com/mcp",
            HttpOptions {
                headers: BTreeMap::from([("Cookie".to_string(), "session=not-sent".to_string())]),
                ..HttpOptions::default()
            },
        );
        let error = static_cookie.call("ping", json!({})).await.unwrap_err();
        assert!(matches!(
            error.downcast_ref::<HttpTransportError>(),
            Some(HttpTransportError::InvalidConfiguration(_))
        ));
    }

    #[tokio::test]
    async fn surfaces_typed_www_authenticate_challenge() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = [0_u8; 2048];
            let _ = socket.read(&mut bytes).await.unwrap();
            socket
                .write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Bearer resource_metadata=\"/.well-known/oauth-protected-resource\", scope=\"database.read\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        });
        let transport = HttpTransport::new(format!("http://{address}/mcp"));

        let error = transport.call("initialize", json!({})).await.unwrap_err();
        let challenge = auth_challenge_from_error(&error).unwrap();

        assert_eq!(
            challenge.kind(),
            HttpAuthChallengeKind::AuthorizationRequired
        );
        assert_eq!(challenge.status(), 401);
        assert_eq!(challenge.scope(), Some("database.read"));
        assert!(challenge
            .resource_metadata_url()
            .unwrap()
            .contains("/.well-known/oauth-protected-resource"));
    }

    #[test]
    fn auth_challenge_debug_redacts_header_secrets() {
        let challenge = HttpAuthChallenge::from_response(
            reqwest::StatusCode::UNAUTHORIZED,
            Some("Bearer error_description=\"state=secret-state\"".to_string()),
            "https://example.com/mcp",
        );

        assert!(!format!("{challenge:?}").contains("secret-state"));
    }
}
