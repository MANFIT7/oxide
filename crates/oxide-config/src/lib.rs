//! Layered configuration for Oxide.
//!
//! Resolution order (later overrides earlier): built-in defaults -> user config
//! at `~/.config/oxide/config.toml` -> project `./oxide.toml` -> environment.
//! Kept intentionally small in Fase 0; grows as features land.

use anyhow::{Context, Result};
use oxide_protocol::{ApprovalPolicy, SandboxPolicy};
use serde::{Deserialize, Serialize};
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
    /// Auto-import MCP servers configured in other agent CLIs (codex's
    /// `~/.codex/config.toml`) on load, so existing plugins work without
    /// re-configuring. Names already present in `mcp_servers` win.
    #[serde(default = "default_true")]
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
fn default_theme() -> String {
    "dark".to_string()
}

fn default_front() -> String {
    "claude".to_string()
}
fn default_backend() -> String {
    "codex".to_string()
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
    /// Whether this server is active.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Source that was imported from (for UI/debug only).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub source: String,
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
            enabled: true,
            source: String::new(),
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
    pub fn tool_allowed(&self, bare_name: &str) -> bool {
        let explicitly_enabled = self.enabled_tools.is_empty()
            || self.enabled_tools.iter().any(|name| name == bare_name);
        explicitly_enabled && !self.disabled_tools.iter().any(|name| name == bare_name)
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
            harness_dir: None,
            workspace: None,
            max_context_tokens: 100_000,
            persist: true,
            resume: false,
            mcp_servers: Vec::new(),
            import_external_mcp: true,
            orchestrate: false,
            front_provider: default_front(),
            backend_provider: default_backend(),
            subagents: false,
            recent_workspaces: Vec::new(),
            update_url: String::new(),
            github_repo: default_github_repo(),
            default_tab_mode: default_tab_mode(),
            browser_headless: true,
            notification_sound: true,
            notification_volume: default_notify_volume(),
            theme: default_theme(),
            accent_color: String::new(),
            density: default_density(),
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
        if cfg.import_external_mcp {
            cfg.merge_imported_mcp(import_codex_mcp_servers());
        }
        Ok(cfg)
    }

    /// Add imported MCP servers that aren't already configured (by name).
    /// Explicit config in `mcp_servers` always wins over an import.
    pub fn merge_imported_mcp(&mut self, imported: Vec<McpServerConfig>) {
        for s in imported {
            if self.mcp_servers.iter().any(|e| e.name == s.name) {
                continue;
            }
            self.mcp_servers.push(s);
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

/// Parse codex's `[mcp_servers.<name>]` tables into Oxide MCP configs. Codex and
/// Oxide share the same MCP wire protocol, so a stdio (`command`/`args`/`env`)
/// or remote (`url`) server defined for codex runs as-is in Oxide.
fn parse_codex_mcp(text: &str) -> Vec<McpServerConfig> {
    let Ok(root) = toml::from_str::<toml::Value>(text) else {
        return Vec::new();
    };
    let Some(servers) = root.get("mcp_servers").and_then(|v| v.as_table()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (name, def) in servers {
        let Some(def) = def.as_table() else { continue };
        let command = def
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let url = def
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        // Skip entries that aren't launchable (neither a command nor a URL).
        if command.is_empty() && url.is_empty() {
            continue;
        }
        let args = def
            .get("args")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let env = def
            .get("env")
            .and_then(|v| v.as_table())
            .map(|t| {
                t.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        let bearer_token_env_var = def
            .get("bearer_token_env_var")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        out.push(McpServerConfig {
            name: name.clone(),
            command,
            args,
            url,
            enabled: true,
            source: "codex".to_string(),
            env,
            bearer_token_env_var,
            ..Default::default()
        });
    }
    out
}

/// Read + parse codex's MCP servers from `~/.codex/config.toml` (empty if absent).
fn import_codex_mcp_servers() -> Vec<McpServerConfig> {
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    let path = PathBuf::from(home).join(".codex/config.toml");
    match std::fs::read_to_string(&path) {
        Ok(text) => parse_codex_mcp(&text),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_codex_mcp_servers_stdio_and_remote() {
        let text = r#"
[mcp_servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_TOKEN = "x" }

[mcp_servers.remote]
url = "https://example.com/mcp"
bearer_token_env_var = "TOK"

[mcp_servers.broken]
description = "no command or url — skipped"
"#;
        let mut servers = parse_codex_mcp(text);
        servers.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(servers.len(), 2); // "broken" skipped
        let gh = servers.iter().find(|s| s.name == "github").unwrap();
        assert_eq!(gh.command, "npx");
        assert_eq!(gh.args, ["-y", "@modelcontextprotocol/server-github"]);
        assert_eq!(gh.env.get("GITHUB_TOKEN").map(String::as_str), Some("x"));
        assert_eq!(gh.source, "codex");
        assert!(gh.enabled);
        let r = servers.iter().find(|s| s.name == "remote").unwrap();
        assert_eq!(r.url, "https://example.com/mcp");
        assert_eq!(r.bearer_token_env_var, "TOK");
    }

    #[test]
    fn merge_imported_mcp_keeps_explicit_config() {
        let mut cfg = Config {
            mcp_servers: vec![McpServerConfig {
                name: "github".into(),
                command: "mine".into(),
                ..Default::default()
            }],
            ..Config::default()
        };
        cfg.merge_imported_mcp(vec![
            McpServerConfig {
                name: "github".into(),
                command: "codex".into(),
                source: "codex".into(),
                ..Default::default()
            },
            McpServerConfig {
                name: "fs".into(),
                command: "npx".into(),
                source: "codex".into(),
                ..Default::default()
            },
        ]);
        // Explicit "github" wins; new "fs" is added.
        assert_eq!(cfg.mcp_servers.len(), 2);
        let gh = cfg.mcp_servers.iter().find(|s| s.name == "github").unwrap();
        assert_eq!(gh.command, "mine");
        assert_eq!(gh.source, "");
        assert!(cfg
            .mcp_servers
            .iter()
            .any(|s| s.name == "fs" && s.source == "codex"));
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
    fn overlay_file_preserves_existing_values_for_missing_keys() {
        let dir = std::env::temp_dir().join(format!("oxide-config-overlay-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("oxide.toml");
        std::fs::write(&path, r#"provider = "codex""#).unwrap();

        let mut cfg = Config {
            model: "gpt-custom".to_string(),
            notification_sound: false,
            ..Config::default()
        };
        cfg.overlay_file(&path).unwrap();

        assert_eq!(cfg.provider, "codex");
        assert_eq!(cfg.model, "gpt-custom");
        assert!(!cfg.notification_sound);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
