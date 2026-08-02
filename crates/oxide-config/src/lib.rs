//! Layered configuration for Oxide.
//!
//! Resolution order (later overrides earlier): built-in defaults -> user config
//! at `~/.config/oxide/config.toml` -> project `./oxide.toml` -> environment.
//! Kept intentionally small in Fase 0; grows as features land.

use anyhow::{Context, Result};
use oxide_protocol::{ApprovalPolicy, SandboxPolicy};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Id of the harness to activate on start (e.g. "default", "hermes").
    pub harness: String,
    /// Default model identifier passed to the provider.
    pub model: String,
    /// Reasoning/effort level passed to providers that support it.
    pub reasoning_effort: String,
    /// Prefer the fastest supported model for the active provider.
    pub fast_mode: bool,
    /// Which provider backend to use ("echo", "openai", "anthropic").
    pub provider: String,
    pub approval_policy: ApprovalPolicy,
    pub sandbox: SandboxPolicy,
    /// Enforce a read-only planning turn across native tools and CLI providers.
    /// Kept separate from the preferred access preset so leaving plan mode
    /// restores the user's previous approval/sandbox choice exactly.
    #[serde(default)]
    pub plan_mode: bool,
    /// Directory scanned for external harness manifests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness_dir: Option<PathBuf>,
    /// Root all tool filesystem/shell access is confined to. Defaults to cwd.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<PathBuf>,
    /// Approximate token budget before the engine compacts old context.
    pub max_context_tokens: u64,
    /// Persist the conversation to `.oxide/sessions/*.jsonl`.
    pub persist: bool,
    /// Seed history from the most recent session on start.
    pub resume: bool,
    /// External MCP tool servers to launch and expose to the model.
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
    /// Legacy compatibility flag. External MCP servers are discovered by the
    /// UI, but must be explicitly trusted before Oxide launches them.
    #[serde(default)]
    pub import_external_mcp: bool,
    /// Two-stage orchestration: a front planner delegates to a backend implementer.
    #[serde(default)]
    pub orchestrate: bool,
    /// Provider used for the planning stage (front agent).
    #[serde(default = "default_front")]
    pub front_provider: String,
    /// Provider used for the implementation stage (backend agent).
    #[serde(default = "default_backend")]
    pub backend_provider: String,
    /// Split the plan into backend sub-agents, then synthesize their results.
    #[serde(default)]
    pub subagents: bool,
    /// Recently opened workspace folders (most-recent first).
    #[serde(default)]
    pub recent_workspaces: Vec<PathBuf>,
    /// URL of an update manifest JSON (`{version,url,notes}`) for OTA updates.
    #[serde(default)]
    pub update_url: String,
    /// Versi app terakhir yang sudah dilihat user (penanda pill "What's New",
    /// Synara model): pill hanya muncul saat versi berjalan melompati ini;
    /// launch pertama diam-diam memajukan penanda tanpa pill.
    #[serde(default)]
    pub last_seen_version: String,
    /// GitHub repo (`owner/name`) to pull the latest release from for updates.
    #[serde(default = "default_github_repo")]
    pub github_repo: String,
    /// Default mode for new agent tabs / next launch: "gui" or "tui".
    #[serde(default = "default_tab_mode")]
    pub default_tab_mode: String,
    /// Run the automation browser headless (background, no window).
    #[serde(default = "default_true")]
    pub browser_headless: bool,
    /// Play a short notification sound when a turn finishes.
    #[serde(default = "default_true")]
    pub notification_sound: bool,
    /// Show native OS notifications for background turns, approvals, and updates.
    #[serde(default = "default_true")]
    pub native_notifications: bool,
    /// Local webhook listener port for automation triggers
    /// (`POST 127.0.0.1:{port}/hook/{automation_id}`); None = disabled.
    #[serde(default)]
    pub webhook_port: Option<u16>,
    /// Notification sound volume (0.0–1.0).
    #[serde(default = "default_notify_volume")]
    pub notification_volume: f32,
    /// UI theme: "dark", "light", or "system".
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Optional custom accent color (hex, e.g. "#e0913a"); empty = theme default.
    #[serde(default)]
    pub accent_color: String,
    /// UI density: "comfortable" or "compact".
    #[serde(default = "default_density")]
    pub density: String,
    /// Tool-call trace density in the transcript: "compact" (hide settled
    /// rows), "balanced" (default), or "detailed" (auto-expand outputs).
    #[serde(default = "default_tool_detail")]
    pub tool_detail: String,
    /// Pinned session file paths (shown in a top "Pinned" section).
    #[serde(default)]
    pub pinned_sessions: Vec<String>,
    /// After the agent edits files, run a build/typecheck and feed errors back
    /// so it auto-fixes before finishing (Cursor-style).
    #[serde(default = "default_true")]
    pub auto_verify: bool,
    /// Override the verify command (empty = auto-detect from project files).
    #[serde(default)]
    pub verify_command: String,
    /// Resume the engine's model context from this exact session file
    /// (transient — never persisted to disk).
    #[serde(skip)]
    pub resume_path: Option<PathBuf>,
    /// Persisted GUI panel widths (px): left sidebar / right inspector.
    #[serde(default = "default_sidebar_w")]
    pub sidebar_width: f64,
    #[serde(default = "default_insp_w")]
    pub inspector_width: f64,
    /// Persisted Environment panel width (px).
    #[serde(default = "default_env_w")]
    pub env_width: f64,
    /// Preferred external editor app (macOS app name for `open -a`).
    #[serde(default = "default_editor")]
    pub editor_app: String,
}

fn default_editor() -> String {
    "Visual Studio Code".to_string()
}

fn default_env_w() -> f64 {
    560.0
}

fn default_sidebar_w() -> f64 {
    250.0
}
fn default_insp_w() -> f64 {
    280.0
}

fn default_true() -> bool {
    true
}

fn default_notify_volume() -> f32 {
    0.48
}

fn default_tab_mode() -> String {
    "gui".to_string()
}
fn default_github_repo() -> String {
    "MANFIT7/oxide".to_string()
}
fn default_density() -> String {
    "comfortable".to_string()
}
fn default_tool_detail() -> String {
    "balanced".to_string()
}
fn default_theme() -> String {
    "dark".to_string()
}

fn default_front() -> String {
    "claude".to_string()
}
fn default_backend() -> String {
    "codex".to_string()
}

/// Authentication strategy for one MCP server.
///
/// Credentials are intentionally not represented here. OAuth tokens belong in
/// a secure credential store, while bearer tokens are referenced by environment
/// variable name through [`McpServerConfig::bearer_token_env_var`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum McpAuthMode {
    #[default]
    None,
    #[serde(rename = "oauth")]
    OAuth,
    BearerEnv,
}

impl McpAuthMode {
    fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }
}

/// One MCP server launcher (stdio command, or a remote HTTP/SSE `url`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServerConfig {
    /// Short name used to namespace its tools (`mcp__<name>__<tool>`).
    pub name: String,
    /// Executable to spawn (stdio transport). Empty when `url` is set.
    #[serde(default)]
    pub command: String,
    /// Arguments passed to the executable.
    #[serde(default)]
    pub args: Vec<String>,
    /// Remote MCP endpoint (Streamable HTTP/SSE). Used instead of `command`.
    #[serde(default)]
    pub url: String,
    /// Provider family for provider-specific, non-secret behavior.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub provider: String,
    /// Authentication strategy. Credential material is stored separately.
    #[serde(default, skip_serializing_if = "McpAuthMode::is_none")]
    pub auth_mode: McpAuthMode,
    /// Stable identifier used to locate credentials in a secure store.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub auth_profile_id: String,
    /// Provider-specific non-secret options only.
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        serialize_with = "serialize_mcp_provider_options",
        deserialize_with = "deserialize_mcp_provider_options"
    )]
    pub provider_options: BTreeMap<String, String>,
    /// Whether this server is active.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Source that was imported from (for UI/debug only).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// Resolve this trusted server by name from Codex/Claude config at runtime.
    /// The reference intentionally stores no copied command, headers, or secrets.
    #[serde(skip_serializing_if = "is_false")]
    pub external_ref: bool,
    /// Runtime provenance set only after an external reference resolves exactly.
    #[serde(skip)]
    pub trusted_external: bool,
    /// Working directory for stdio MCP launchers.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cwd: String,
    /// Static environment values for stdio MCP launchers.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Environment variable names to forward from Oxide's process.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub env_vars: Vec<McpEnvVar>,
    /// Bearer token environment variable for HTTP MCP servers.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub bearer_token_env_var: String,
    /// Static HTTP headers for remote MCP servers.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub http_headers: BTreeMap<String, String>,
    /// HTTP headers whose values are read from environment variables.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env_http_headers: BTreeMap<String, String>,
    /// Optional server startup/connect timeout, in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub startup_timeout_sec: Option<u64>,
    /// Optional per-request/tool timeout, in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_timeout_sec: Option<u64>,
    /// Optional allow list of bare MCP tool names exposed to the model.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub enabled_tools: Vec<String>,
    /// Optional deny list of bare MCP tool names hidden from the model.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub disabled_tools: Vec<String>,
    /// Whether this MCP server is required for the session.
    #[serde(skip_serializing_if = "is_false")]
    pub required: bool,
}

fn is_sensitive_mcp_provider_option(key: &str) -> bool {
    let key = key
        .trim()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect::<String>();
    matches!(
        key.as_str(),
        "token"
            | "accesstoken"
            | "refreshtoken"
            | "idtoken"
            | "clientsecret"
            | "servicerole"
            | "servicerolekey"
            | "authorization"
            | "auth"
            | "bearer"
            | "bearertoken"
            | "cookie"
            | "apikey"
            | "anonkey"
            | "privatekey"
            | "sig"
            | "signature"
            | "credential"
            | "credentials"
            | "password"
    ) || key.ends_with("token")
        || key.ends_with("secret")
        || key.ends_with("password")
        || key.ends_with("key")
        || key.ends_with("signature")
        || key.ends_with("credential")
        || key.ends_with("credentials")
}

fn mcp_args_contain_inline_credentials(args: &[String]) -> bool {
    args.iter().any(|argument| {
        let trimmed = argument.trim();
        if trimmed.starts_with('-') {
            let flag = trimmed
                .trim_start_matches('-')
                .split_once('=')
                .map(|(key, _)| key)
                .unwrap_or_else(|| trimmed.trim_start_matches('-'));
            let canonical = flag
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .flat_map(char::to_lowercase)
                .collect::<String>();
            if is_sensitive_mcp_provider_option(flag)
                || matches!(
                    canonical.as_str(),
                    "e" | "env"
                        | "environment"
                        | "h"
                        | "header"
                        | "headers"
                        | "httpheader"
                        | "requestheader"
                )
            {
                return true;
            }
        }
        ['=', ':'].iter().any(|separator| {
            trimmed.split_once(*separator).is_some_and(|(key, value)| {
                !value.trim().is_empty() && is_sensitive_mcp_provider_option(key)
            })
        }) || trimmed.to_ascii_lowercase().contains("bearer ")
    })
}

fn serialize_mcp_provider_options<S>(
    options: &BTreeMap<String, String>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    use serde::ser::Error as _;
    if let Some(key) = options
        .keys()
        .find(|key| is_sensitive_mcp_provider_option(key))
    {
        return Err(S::Error::custom(format!(
            "MCP provider_options cannot persist credential field '{key}'"
        )));
    }
    options.serialize(serializer)
}

fn deserialize_mcp_provider_options<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, String>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error as _;
    let options = BTreeMap::<String, String>::deserialize(deserializer)?;
    if let Some(key) = options
        .keys()
        .find(|key| is_sensitive_mcp_provider_option(key))
    {
        return Err(D::Error::custom(format!(
            "MCP provider_options cannot contain credential field '{key}'"
        )));
    }
    Ok(options)
}

pub fn is_valid_mcp_server_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains("__")
        && name
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_'))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum McpEnvVar {
    Name(String),
    Named {
        name: String,
        #[serde(default)]
        source: String,
    },
}

impl McpEnvVar {
    pub fn name(&self) -> &str {
        match self {
            Self::Name(name) => name,
            Self::Named { name, .. } => name,
        }
    }
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            command: String::new(),
            args: Vec::new(),
            url: String::new(),
            provider: String::new(),
            auth_mode: McpAuthMode::None,
            auth_profile_id: String::new(),
            provider_options: BTreeMap::new(),
            enabled: true,
            source: String::new(),
            external_ref: false,
            trusted_external: false,
            cwd: String::new(),
            env: BTreeMap::new(),
            env_vars: Vec::new(),
            bearer_token_env_var: String::new(),
            http_headers: BTreeMap::new(),
            env_http_headers: BTreeMap::new(),
            startup_timeout_sec: None,
            tool_timeout_sec: None,
            enabled_tools: Vec::new(),
            disabled_tools: Vec::new(),
            required: false,
        }
    }
}

impl McpServerConfig {
    pub fn is_oauth(&self) -> bool {
        matches!(self.auth_mode, McpAuthMode::OAuth)
    }

    pub fn tool_allowed(&self, bare_name: &str) -> bool {
        let explicitly_enabled = self.enabled_tools.is_empty()
            || self.enabled_tools.iter().any(|name| name == bare_name);
        explicitly_enabled && !self.disabled_tools.iter().any(|name| name == bare_name)
    }

    /// Persist only a trust reference for an externally discovered server.
    /// Runtime discovery supplies the launcher/auth fields from the source config.
    pub fn as_external_reference(&self) -> Self {
        Self {
            name: self.name.clone(),
            enabled: self.enabled,
            source: self.source.clone(),
            external_ref: true,
            provider: self.provider.clone(),
            auth_mode: self.auth_mode,
            auth_profile_id: self.auth_profile_id.clone(),
            startup_timeout_sec: self.startup_timeout_sec,
            tool_timeout_sec: self.tool_timeout_sec,
            enabled_tools: self.enabled_tools.clone(),
            disabled_tools: self.disabled_tools.clone(),
            required: self.required,
            ..Self::default()
        }
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl Default for Config {
    fn default() -> Self {
        Self {
            harness: "default".to_string(),
            // Empty = let the provider/CLI choose its own default model.
            model: String::new(),
            reasoning_effort: "medium".to_string(),
            fast_mode: false,
            provider: "echo".to_string(),
            approval_policy: ApprovalPolicy::default(),
            sandbox: SandboxPolicy::default(),
            plan_mode: false,
            harness_dir: None,
            workspace: None,
            max_context_tokens: 100_000,
            persist: true,
            resume: false,
            mcp_servers: Vec::new(),
            import_external_mcp: false,
            orchestrate: false,
            front_provider: default_front(),
            backend_provider: default_backend(),
            subagents: false,
            recent_workspaces: Vec::new(),
            update_url: String::new(),
            last_seen_version: String::new(),
            github_repo: default_github_repo(),
            default_tab_mode: default_tab_mode(),
            browser_headless: true,
            notification_sound: true,
            native_notifications: true,
            webhook_port: None,
            notification_volume: default_notify_volume(),
            theme: default_theme(),
            accent_color: String::new(),
            density: default_density(),
            tool_detail: default_tool_detail(),
            pinned_sessions: Vec::new(),
            auto_verify: true,
            verify_command: String::new(),
            resume_path: None,
            sidebar_width: 250.0,
            inspector_width: 280.0,
            env_width: 560.0,
            editor_app: default_editor(),
        }
    }
}

impl Config {
    /// Load defaults then overlay any discovered config files.
    pub fn load() -> Result<Self> {
        let mut cfg = Config::default();
        // A corrupt config file (torn write, hand-edit gone wrong) must not
        // brick startup — warn and continue with what still parses. The bad
        // file is left in place for the user to inspect/fix.
        if let Some(user) = user_config_path() {
            if let Err(e) = cfg.overlay_file(&user) {
                eprintln!(
                    "oxide: ignoring unreadable config {}: {e:#}",
                    user.display()
                );
            }
        }
        let project = PathBuf::from("oxide.toml");
        if project.exists() {
            if let Err(e) = cfg.overlay_file(&project) {
                eprintln!("oxide: ignoring unreadable oxide.toml: {e:#}");
            }
        }
        cfg.normalize_external_mcp_references();
        Ok(cfg)
    }

    /// Migrate previously auto-imported full configs to secret-free references.
    fn normalize_external_mcp_references(&mut self) {
        for server in &mut self.mcp_servers {
            if !server.source.trim().is_empty() && !server.external_ref {
                *server = server.as_external_reference();
            }
        }
    }

    /// Merge a TOML file on top of the current config (missing keys keep prior values).
    pub fn overlay_file(&mut self, path: &Path) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let overlay: toml::Value =
            toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?;
        let mut base = toml::Value::try_from(&*self).with_context(|| {
            format!("serializing base config before overlay {}", path.display())
        })?;
        merge_toml(&mut base, overlay);
        *self = base
            .try_into()
            .with_context(|| format!("merging config {}", path.display()))?;
        self.normalize_external_mcp_references();
        Ok(())
    }

    pub fn effective_model(&self) -> String {
        if self.fast_mode && self.model.trim().is_empty() {
            return fast_model_for_provider(&self.provider)
                .map(str::to_string)
                .unwrap_or_default();
        }
        self.model.clone()
    }

    /// Validate that an application-managed write cannot replicate inline MCP
    /// credentials. Legacy manual configs must migrate those values to
    /// environment references before a GUI rewrites the MCP section.
    pub fn validate_managed_mcp_persistence(&self) -> std::result::Result<(), String> {
        let mut names = std::collections::HashSet::with_capacity(self.mcp_servers.len());
        for server in &self.mcp_servers {
            if !is_valid_mcp_server_name(&server.name) {
                return Err(format!("MCP server name '{}' is invalid", server.name));
            }
            if !names.insert(server.name.as_str()) {
                return Err(format!(
                    "duplicate MCP server name '{}'; server names must be unique",
                    server.name
                ));
            }
            if !server.env.is_empty() {
                return Err(format!(
                    "MCP server '{}' contains inline environment values; move them to env_vars before saving",
                    server.name
                ));
            }
            if !server.http_headers.is_empty() {
                return Err(format!(
                    "MCP server '{}' contains inline HTTP headers; move them to env_http_headers before saving",
                    server.name
                ));
            }
            if mcp_args_contain_inline_credentials(&server.args) {
                return Err(format!(
                    "MCP server '{}' contains a credential-like command argument; use an environment reference before saving",
                    server.name
                ));
            }
            if !server.url.trim().is_empty() {
                let endpoint = url::Url::parse(&server.url).map_err(|error| {
                    format!("MCP server '{}' has an invalid URL: {error}", server.name)
                })?;
                if !endpoint.username().is_empty() || endpoint.password().is_some() {
                    return Err(format!(
                        "MCP server '{}' URL contains inline credentials; use an environment reference before saving",
                        server.name
                    ));
                }
                if endpoint
                    .query_pairs()
                    .any(|(key, _)| is_sensitive_mcp_provider_option(&key))
                {
                    return Err(format!(
                        "MCP server '{}' URL contains a credential query parameter; use native OAuth or an environment reference before saving",
                        server.name
                    ));
                }
            }
        }
        Ok(())
    }

    /// Runtime approval policy after applying product modes such as Plan.
    pub fn effective_approval_policy(&self) -> ApprovalPolicy {
        self.effective_permissions().0
    }

    /// Runtime sandbox after applying product modes such as Plan.
    pub fn effective_sandbox(&self) -> SandboxPolicy {
        self.effective_permissions().1
    }

    /// Normalize persisted/legacy permission pairs to the product's supported
    /// contracts. Unknown combinations must fail closed: older UI versions
    /// could persist `Never + WorkspaceWrite`, which otherwise auto-writes
    /// while the current UI labels it as approval-required.
    pub fn effective_permissions(&self) -> (ApprovalPolicy, SandboxPolicy) {
        if self.plan_mode {
            return (ApprovalPolicy::Never, SandboxPolicy::ReadOnly);
        }

        match (self.approval_policy, self.sandbox) {
            pair @ (ApprovalPolicy::Always, SandboxPolicy::WorkspaceWrite)
            | pair @ (ApprovalPolicy::OnRequest, SandboxPolicy::WorkspaceWrite)
            | pair @ (ApprovalPolicy::Never, SandboxPolicy::DangerFullAccess)
            | pair @ (ApprovalPolicy::Never, SandboxPolicy::ReadOnly) => pair,
            _ => (ApprovalPolicy::Always, SandboxPolicy::ReadOnly),
        }
    }
}

fn fast_model_for_provider(provider: &str) -> Option<&'static str> {
    match provider {
        "codex" => Some("gpt-5.3-codex-spark"),
        "openai" => Some("gpt-5.4"),
        "claude" | "claude_interactive" | "anthropic" => Some("claude-sonnet-4-6"),
        "gemini" => Some("gemini-3.5-flash"),
        "xai" => Some("grok-build-0.1"),
        "deepseek" => Some("deepseek-v4-flash"),
        "mistral" => Some("mistral-small-4"),
        _ => None,
    }
}

fn merge_toml(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base), toml::Value::Table(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(&key) {
                    Some(existing) => merge_toml(existing, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, overlay) => {
            *base = overlay;
        }
    }
}

fn user_config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/oxide/config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imported_mcp_config_is_reduced_to_secret_free_reference() {
        let mut server = McpServerConfig {
            name: "github".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "server".into()],
            source: "Codex user config".into(),
            provider: "github".into(),
            auth_mode: McpAuthMode::OAuth,
            auth_profile_id: "github-main".into(),
            provider_options: BTreeMap::from([
                ("access_token".into(), "must-not-copy".into()),
                ("project".into(), "oxide".into()),
            ]),
            env: BTreeMap::from([("GITHUB_TOKEN".into(), "secret".into())]),
            http_headers: BTreeMap::from([("Authorization".into(), "Bearer secret".into())]),
            required: true,
            ..Default::default()
        };

        server = server.as_external_reference();

        assert!(server.external_ref);
        assert_eq!(server.name, "github");
        assert_eq!(server.source, "Codex user config");
        assert_eq!(server.provider, "github");
        assert!(server.is_oauth());
        assert_eq!(server.auth_profile_id, "github-main");
        assert!(server.provider_options.is_empty());
        assert!(server.command.is_empty());
        assert!(server.args.is_empty());
        assert!(server.env.is_empty());
        assert!(server.http_headers.is_empty());
        assert!(server.required);
        let serialized = toml::to_string(&server).unwrap();
        assert!(!serialized.contains("secret"));
        assert!(!serialized.contains("must-not-copy"));
        assert!(!serialized.contains("access_token"));
        assert!(!serialized.contains("refresh_token"));
    }

    #[test]
    fn legacy_mcp_config_defaults_to_no_native_auth() {
        let server: McpServerConfig = toml::from_str(
            r#"
name = "legacy"
url = "https://example.com/mcp"
bearer_token_env_var = "LEGACY_MCP_TOKEN"
"#,
        )
        .unwrap();

        assert_eq!(server.auth_mode, McpAuthMode::None);
        assert!(server.provider.is_empty());
        assert!(server.auth_profile_id.is_empty());
        assert!(server.provider_options.is_empty());
        assert_eq!(server.bearer_token_env_var, "LEGACY_MCP_TOKEN");

        let serialized = toml::to_string(&server).unwrap();
        assert!(!serialized.contains("auth_mode"));
        let restored: McpServerConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(restored.auth_mode, McpAuthMode::None);
        assert_eq!(restored.bearer_token_env_var, "LEGACY_MCP_TOKEN");
    }

    #[test]
    fn oauth_mcp_config_roundtrips_without_token_fields() {
        let server: McpServerConfig = toml::from_str(
            r#"
name = "supabase"
url = "https://mcp.supabase.com/mcp"
provider = "supabase"
auth_mode = "oauth"
auth_profile_id = "supabase-main"
provider_options = { project_ref = "project-123", read_only = "true" }
"#,
        )
        .unwrap();

        assert!(server.is_oauth());
        assert_eq!(server.provider, "supabase");
        assert_eq!(server.auth_profile_id, "supabase-main");
        assert_eq!(
            server
                .provider_options
                .get("project_ref")
                .map(String::as_str),
            Some("project-123")
        );

        let serialized = toml::to_string(&server).unwrap();
        assert!(serialized.contains("auth_mode = \"oauth\""));
        assert!(!serialized.contains("access_token"));
        assert!(!serialized.contains("refresh_token"));

        let restored: McpServerConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(restored.auth_mode, McpAuthMode::OAuth);
        assert_eq!(restored.provider, server.provider);
        assert_eq!(restored.auth_profile_id, server.auth_profile_id);
        assert_eq!(restored.provider_options, server.provider_options);
    }

    #[test]
    fn managed_persistence_accepts_references_and_rejects_inline_mcp_secrets() {
        let mut config = Config::default();
        config.mcp_servers.push(McpServerConfig {
            name: "private".into(),
            env_vars: vec![McpEnvVar::Name("DATABASE_URL".into())],
            env_http_headers: BTreeMap::from([("Authorization".into(), "MCP_TOKEN".into())]),
            bearer_token_env_var: "MCP_TOKEN".into(),
            ..Default::default()
        });
        assert!(config.validate_managed_mcp_persistence().is_ok());

        let mut inline_env = config.clone();
        inline_env.mcp_servers[0]
            .env
            .insert("DATABASE_URL".into(), "postgres://secret".into());
        assert!(inline_env.validate_managed_mcp_persistence().is_err());

        let mut inline_header = config.clone();
        inline_header.mcp_servers[0]
            .http_headers
            .insert("Authorization".into(), "Bearer secret".into());
        assert!(inline_header.validate_managed_mcp_persistence().is_err());

        let mut inline_arg = config.clone();
        inline_arg.mcp_servers[0].args = vec!["--api-key=secret".into()];
        assert!(inline_arg.validate_managed_mcp_persistence().is_err());

        let mut inline_header_arg = config.clone();
        inline_header_arg.mcp_servers[0].args =
            vec!["-H".into(), "Authorization: Bearer secret".into()];
        assert!(inline_header_arg
            .validate_managed_mcp_persistence()
            .is_err());

        let mut inline_env_arg = config.clone();
        inline_env_arg.mcp_servers[0].args = vec!["--env".into(), "TOKEN=secret".into()];
        assert!(inline_env_arg.validate_managed_mcp_persistence().is_err());

        let mut inline_url = config.clone();
        inline_url.mcp_servers[0].url = "https://mcp.example/mcp?accessToken=secret".into();
        assert!(inline_url.validate_managed_mcp_persistence().is_err());

        let mut signed_url = config;
        signed_url.mcp_servers[0].url = "https://mcp.example/mcp?X-Amz-Signature=secret".into();
        assert!(signed_url.validate_managed_mcp_persistence().is_err());
    }

    #[test]
    fn unknown_mcp_auth_mode_is_rejected() {
        let result = toml::from_str::<McpServerConfig>(
            r#"
name = "invalid"
auth_mode = "custom"
"#,
        );

        assert!(result.is_err());
    }

    #[test]
    fn provider_options_reject_credential_material() {
        let parsed = toml::from_str::<McpServerConfig>(
            r#"
name = "unsafe"
provider_options = { project_ref = "safe", access_token = "must-not-persist" }
"#,
        );
        assert!(parsed
            .unwrap_err()
            .to_string()
            .contains("cannot contain credential field 'access_token'"));

        let server = McpServerConfig {
            name: "unsafe".to_string(),
            provider_options: BTreeMap::from([(
                "client_secret".to_string(),
                "must-not-persist".to_string(),
            )]),
            ..McpServerConfig::default()
        };
        assert!(toml::to_string(&server).is_err());

        for key in [
            "accessToken",
            "clientSecret",
            "apiKey",
            "serviceRoleKey",
            "private_key",
            "providerToken",
        ] {
            let source = format!(
                "name = \"unsafe\"\nprovider_options = {{ {key} = \"must-not-persist\" }}\n"
            );
            assert!(toml::from_str::<McpServerConfig>(&source).is_err(), "{key}");
        }
    }

    #[test]
    fn mcp_server_names_preserve_namespace_boundaries() {
        assert!(is_valid_mcp_server_name("supabase-main"));
        assert!(is_valid_mcp_server_name("supabase_main"));
        assert!(!is_valid_mcp_server_name("foo__bar"));
        assert!(!is_valid_mcp_server_name("contains space"));
        assert!(!is_valid_mcp_server_name(""));
    }

    #[test]
    fn trusted_external_provenance_cannot_be_loaded_from_toml() {
        let server: McpServerConfig = toml::from_str(
            r#"
name = "spoofed"
url = "https://attacker.example/mcp"
source = "Claude Desktop"
trusted_external = true
"#,
        )
        .unwrap();

        assert!(!server.trusted_external);
        assert!(!toml::to_string(&server)
            .unwrap()
            .contains("trusted_external"));
    }

    #[test]
    fn overlay_migrates_legacy_import_before_it_can_be_persisted_again() {
        let dir =
            std::env::temp_dir().join(format!("oxide-config-mcp-migration-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("oxide.toml");
        std::fs::write(
            &path,
            r#"
[[mcp_servers]]
name = "github"
command = "npx"
source = "Codex user config"
env = { GITHUB_TOKEN = "legacy-secret" }
"#,
        )
        .unwrap();
        let mut config = Config::default();

        config.overlay_file(&path).unwrap();

        assert_eq!(config.mcp_servers.len(), 1);
        assert!(config.mcp_servers[0].external_ref);
        assert!(config.mcp_servers[0].command.is_empty());
        assert!(config.mcp_servers[0].env.is_empty());
        assert!(!toml::to_string(&config).unwrap().contains("legacy-secret"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fast_mode_uses_latest_provider_fast_models() {
        let mut cfg = Config {
            provider: "codex".to_string(),
            fast_mode: true,
            ..Config::default()
        };
        assert_eq!(cfg.effective_model(), "gpt-5.3-codex-spark");

        cfg.provider = "openai".to_string();
        assert_eq!(cfg.effective_model(), "gpt-5.4");

        cfg.provider = "anthropic".to_string();
        assert_eq!(cfg.effective_model(), "claude-sonnet-4-6");

        cfg.provider = "gemini".to_string();
        assert_eq!(cfg.effective_model(), "gemini-3.5-flash");

        cfg.provider = "xai".to_string();
        assert_eq!(cfg.effective_model(), "grok-build-0.1");

        cfg.provider = "deepseek".to_string();
        assert_eq!(cfg.effective_model(), "deepseek-v4-flash");

        cfg.provider = "mistral".to_string();
        assert_eq!(cfg.effective_model(), "mistral-small-4");
    }

    #[test]
    fn plan_mode_is_a_read_only_overlay_that_preserves_baseline_access() {
        let mut cfg = Config {
            approval_policy: ApprovalPolicy::Never,
            sandbox: SandboxPolicy::DangerFullAccess,
            plan_mode: true,
            ..Config::default()
        };

        assert_eq!(cfg.effective_approval_policy(), ApprovalPolicy::Never);
        assert_eq!(cfg.effective_sandbox(), SandboxPolicy::ReadOnly);

        cfg.plan_mode = false;
        assert_eq!(cfg.effective_approval_policy(), ApprovalPolicy::Never);
        assert_eq!(cfg.effective_sandbox(), SandboxPolicy::DangerFullAccess);
    }

    #[test]
    fn unsupported_legacy_permission_pairs_fail_closed() {
        let cfg = Config {
            approval_policy: ApprovalPolicy::Never,
            sandbox: SandboxPolicy::WorkspaceWrite,
            ..Config::default()
        };

        assert_eq!(cfg.effective_approval_policy(), ApprovalPolicy::Always);
        assert_eq!(cfg.effective_sandbox(), SandboxPolicy::ReadOnly);
    }

    #[test]
    fn overlay_file_preserves_existing_values_for_missing_keys() {
        let dir = std::env::temp_dir().join(format!("oxide-config-overlay-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("oxide.toml");
        std::fs::write(&path, r#"provider = "codex""#).unwrap();

        let mut cfg = Config {
            model: "gpt-custom".to_string(),
            notification_sound: false,
            webhook_port: None,
            ..Config::default()
        };
        cfg.overlay_file(&path).unwrap();

        assert_eq!(cfg.provider, "codex");
        assert_eq!(cfg.model, "gpt-custom");
        assert!(!cfg.notification_sound);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
