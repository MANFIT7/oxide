//! Layered configuration for Oxide.
//!
//! Resolution order (later overrides earlier): built-in defaults -> user config
//! at `~/.config/oxide/config.toml` -> project `./oxide.toml` -> environment.
//! Kept intentionally small in Fase 0; grows as features land.

use anyhow::{Context, Result};
use oxide_protocol::{ApprovalPolicy, SandboxPolicy};
use serde::{Deserialize, Serialize};
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
    /// Two-stage orchestration: a front planner delegates to a backend implementer.
    #[serde(default)]
    pub orchestrate: bool,
    /// Provider used for the planning stage (front agent).
    #[serde(default = "default_front")]
    pub front_provider: String,
    /// Provider used for the implementation stage (backend agent).
    #[serde(default = "default_backend")]
    pub backend_provider: String,
    /// Fan the plan out to parallel backend sub-agents (then synthesize).
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

fn default_editor() -> String { "Visual Studio Code".to_string() }

fn default_env_w() -> f64 { 560.0 }

fn default_sidebar_w() -> f64 { 250.0 }
fn default_insp_w() -> f64 { 280.0 }

fn default_true() -> bool {
    true
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
            orchestrate: false,
            front_provider: default_front(),
            backend_provider: default_backend(),
            subagents: false,
            recent_workspaces: Vec::new(),
            update_url: String::new(),
            github_repo: default_github_repo(),
            default_tab_mode: default_tab_mode(),
            browser_headless: true,
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
        if let Some(user) = user_config_path() {
            cfg.overlay_file(&user)?;
        }
        let project = PathBuf::from("oxide.toml");
        if project.exists() {
            cfg.overlay_file(&project)?;
        }
        Ok(cfg)
    }

    /// Merge a TOML file on top of the current config (missing keys keep prior values).
    pub fn overlay_file(&mut self, path: &Path) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let parsed: Config =
            toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?;
        *self = parsed;
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
        "claude" | "anthropic" => Some("claude-sonnet-4-6"),
        "gemini" => Some("gemini-3.5-flash"),
        "xai" => Some("grok-build-0.1"),
        "deepseek" => Some("deepseek-v4-flash"),
        "mistral" => Some("mistral-small-4"),
        _ => None,
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
}
