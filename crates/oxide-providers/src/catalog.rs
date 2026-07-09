//! Static provider catalog plus local, no-network diagnostics.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Local,
    OpenAiCompatible,
    Anthropic,
    ChatGptSubscription,
    Cli,
    Test,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::OpenAiCompatible => "openai-compatible",
            Self::Anthropic => "anthropic",
            Self::ChatGptSubscription => "chatgpt-subscription",
            Self::Cli => "cli",
            Self::Test => "test",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderStability {
    Stable,
    Experimental,
    Test,
}

impl ProviderStability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Experimental => "experimental",
            Self::Test => "test",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderCapability {
    Text,
    Streaming,
    Reasoning,
    ToolCalls,
    ToolInputDeltas,
    NativeCliTools,
    FileChanges,
    Images,
    RateLimits,
    McpTools,
    BrowserControl,
}

impl ProviderCapability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Streaming => "streaming",
            Self::Reasoning => "reasoning",
            Self::ToolCalls => "tool-calls",
            Self::ToolInputDeltas => "tool-input-deltas",
            Self::NativeCliTools => "native-cli-tools",
            Self::FileChanges => "file-changes",
            Self::Images => "images",
            Self::RateLimits => "rate-limits",
            Self::McpTools => "mcp-tools",
            Self::BrowserControl => "browser-control",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderAuth {
    None,
    EnvVar {
        key: &'static str,
        base_url_env: Option<&'static str>,
    },
    CliBinary {
        binary: &'static str,
        env_override: &'static str,
    },
    CodexOAuth {
        path: &'static str,
    },
    TestOnly,
}

impl ProviderAuth {
    pub fn summary(self) -> String {
        match self {
            Self::None => "no auth required".to_string(),
            Self::EnvVar { key, base_url_env } => {
                let base = base_url_env
                    .map(|env| format!("; optional base URL override: {env}"))
                    .unwrap_or_default();
                format!("requires {key}{base}")
            }
            Self::CliBinary {
                binary,
                env_override,
            } => format!("requires `{binary}` on PATH or {env_override}"),
            Self::CodexOAuth { path } => format!("requires Codex OAuth at {path}"),
            Self::TestOnly => "test/demo provider".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderModel {
    pub id: &'static str,
    pub display_name: &'static str,
    pub is_default: bool,
    pub is_fast: bool,
    pub context_window: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderInfo {
    pub id: &'static str,
    pub display_name: &'static str,
    pub kind: ProviderKind,
    pub stability: ProviderStability,
    pub auth: ProviderAuth,
    pub models: &'static [ProviderModel],
    pub capabilities: &'static [ProviderCapability],
    pub notes: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticStatus {
    Ready,
    Warning,
    Missing,
}

impl DiagnosticStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Warning => "warning",
            Self::Missing => "missing",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDiagnostic {
    pub provider_id: &'static str,
    pub status: DiagnosticStatus,
    pub summary: String,
    pub detail: String,
}

const BASIC_CAPS: &[ProviderCapability] =
    &[ProviderCapability::Text, ProviderCapability::Streaming];
const API_CAPS: &[ProviderCapability] = &[
    ProviderCapability::Text,
    ProviderCapability::Streaming,
    ProviderCapability::Reasoning,
    ProviderCapability::ToolCalls,
];
const CHATGPT_CAPS: &[ProviderCapability] = &[
    ProviderCapability::Text,
    ProviderCapability::Streaming,
    ProviderCapability::Reasoning,
    ProviderCapability::ToolCalls,
    ProviderCapability::ToolInputDeltas,
    ProviderCapability::Images,
    ProviderCapability::RateLimits,
];
const CODEX_CLI_CAPS: &[ProviderCapability] = &[
    ProviderCapability::Text,
    ProviderCapability::Streaming,
    ProviderCapability::Reasoning,
    ProviderCapability::NativeCliTools,
    ProviderCapability::FileChanges,
    ProviderCapability::Images,
];
const CLAUDE_CLI_CAPS: &[ProviderCapability] = &[
    ProviderCapability::Text,
    ProviderCapability::Streaming,
    ProviderCapability::Reasoning,
    ProviderCapability::NativeCliTools,
    ProviderCapability::FileChanges,
    ProviderCapability::Images,
];
const MOCK_CAPS: &[ProviderCapability] = &[
    ProviderCapability::Text,
    ProviderCapability::Streaming,
    ProviderCapability::ToolCalls,
];
const MCP_MOCK_CAPS: &[ProviderCapability] = &[
    ProviderCapability::Text,
    ProviderCapability::Streaming,
    ProviderCapability::ToolCalls,
    ProviderCapability::McpTools,
];
const BROWSER_MOCK_CAPS: &[ProviderCapability] = &[
    ProviderCapability::Text,
    ProviderCapability::Streaming,
    ProviderCapability::ToolCalls,
    ProviderCapability::BrowserControl,
];

const ECHO_MODELS: &[ProviderModel] = &[ProviderModel {
    id: "echo-local",
    display_name: "Echo local stub",
    is_default: true,
    is_fast: true,
    context_window: None,
}];
const OPENAI_MODELS: &[ProviderModel] = &[
    ProviderModel {
        id: "gpt-5.4",
        display_name: "GPT-5.4",
        is_default: true,
        is_fast: true,
        context_window: None,
    },
    ProviderModel {
        id: "gpt-5-codex",
        display_name: "GPT-5 Codex",
        is_default: false,
        is_fast: false,
        context_window: None,
    },
];
const CODEX_MODELS: &[ProviderModel] = &[
    ProviderModel {
        id: "gpt-5.6-sol",
        display_name: "GPT-5.6-Sol",
        is_default: false,
        is_fast: false,
        context_window: Some(372_000),
    },
    ProviderModel {
        id: "gpt-5.6-terra",
        display_name: "GPT-5.6-Terra",
        is_default: false,
        is_fast: false,
        context_window: Some(372_000),
    },
    ProviderModel {
        id: "gpt-5.6-luna",
        display_name: "GPT-5.6-Luna",
        is_default: false,
        is_fast: false,
        context_window: Some(372_000),
    },
    ProviderModel {
        id: "gpt-5.3-codex",
        display_name: "GPT-5.3 Codex",
        is_default: true,
        is_fast: false,
        context_window: None,
    },
    ProviderModel {
        id: "gpt-5.3-codex-spark",
        display_name: "GPT-5.3 Codex Spark",
        is_default: false,
        is_fast: true,
        context_window: None,
    },
];
const CLAUDE_MODELS: &[ProviderModel] = &[
    ProviderModel {
        id: "claude-sonnet-4-6",
        display_name: "Claude Sonnet 4.6",
        is_default: true,
        is_fast: true,
        context_window: None,
    },
    ProviderModel {
        id: "sonnet",
        display_name: "Claude Code Sonnet alias",
        is_default: false,
        is_fast: false,
        context_window: None,
    },
    ProviderModel {
        id: "opus",
        display_name: "Claude Code Opus alias",
        is_default: false,
        is_fast: false,
        context_window: None,
    },
    ProviderModel {
        id: "haiku",
        display_name: "Claude Code Haiku alias",
        is_default: false,
        is_fast: true,
        context_window: None,
    },
];
const CHATGPT_MODELS: &[ProviderModel] = &[
    ProviderModel {
        id: "gpt-5.5",
        display_name: "GPT-5.5",
        is_default: true,
        is_fast: false,
        context_window: Some(400_000),
    },
    ProviderModel {
        id: "gpt-5.6-sol",
        display_name: "GPT-5.6-Sol",
        is_default: false,
        is_fast: false,
        context_window: Some(372_000),
    },
    ProviderModel {
        id: "gpt-5.6-terra",
        display_name: "GPT-5.6-Terra",
        is_default: false,
        is_fast: false,
        context_window: Some(372_000),
    },
    ProviderModel {
        id: "gpt-5.6-luna",
        display_name: "GPT-5.6-Luna",
        is_default: false,
        is_fast: false,
        context_window: Some(372_000),
    },
];
const GEMINI_MODELS: &[ProviderModel] = &[
    ProviderModel {
        id: "gemini-3.5-pro",
        display_name: "Gemini 3.5 Pro",
        is_default: true,
        is_fast: false,
        context_window: None,
    },
    ProviderModel {
        id: "gemini-3.5-flash",
        display_name: "Gemini 3.5 Flash",
        is_default: false,
        is_fast: true,
        context_window: None,
    },
];
const XAI_MODELS: &[ProviderModel] = &[ProviderModel {
    id: "grok-build-0.1",
    display_name: "Grok Build 0.1",
    is_default: true,
    is_fast: true,
    context_window: None,
}];
const DEEPSEEK_MODELS: &[ProviderModel] = &[
    ProviderModel {
        id: "deepseek-v4",
        display_name: "DeepSeek V4",
        is_default: true,
        is_fast: false,
        context_window: None,
    },
    ProviderModel {
        id: "deepseek-v4-flash",
        display_name: "DeepSeek V4 Flash",
        is_default: false,
        is_fast: true,
        context_window: None,
    },
];
const MISTRAL_MODELS: &[ProviderModel] = &[
    ProviderModel {
        id: "mistral-medium-4",
        display_name: "Mistral Medium 4",
        is_default: true,
        is_fast: false,
        context_window: None,
    },
    ProviderModel {
        id: "mistral-small-4",
        display_name: "Mistral Small 4",
        is_default: false,
        is_fast: true,
        context_window: None,
    },
];
const MOCK_MODELS: &[ProviderModel] = &[ProviderModel {
    id: "mock-local",
    display_name: "Mock local script",
    is_default: true,
    is_fast: true,
    context_window: None,
}];

const PROVIDERS: &[ProviderInfo] = &[
    ProviderInfo {
        id: "echo",
        display_name: "Echo",
        kind: ProviderKind::Local,
        stability: ProviderStability::Stable,
        auth: ProviderAuth::None,
        models: ECHO_MODELS,
        capabilities: BASIC_CAPS,
        notes: "Offline echo stub for local smoke tests.",
    },
    ProviderInfo {
        id: "openai",
        display_name: "OpenAI",
        kind: ProviderKind::OpenAiCompatible,
        stability: ProviderStability::Stable,
        auth: ProviderAuth::EnvVar {
            key: "OPENAI_API_KEY",
            base_url_env: Some("OPENAI_BASE_URL"),
        },
        models: OPENAI_MODELS,
        capabilities: API_CAPS,
        notes: "Chat Completions over reqwest/SSE.",
    },
    ProviderInfo {
        id: "gemini",
        display_name: "Gemini",
        kind: ProviderKind::OpenAiCompatible,
        stability: ProviderStability::Stable,
        auth: ProviderAuth::EnvVar {
            key: "GEMINI_API_KEY",
            base_url_env: Some("GEMINI_BASE_URL"),
        },
        models: GEMINI_MODELS,
        capabilities: API_CAPS,
        notes: "OpenAI-compatible Gemini endpoint.",
    },
    ProviderInfo {
        id: "xai",
        display_name: "xAI",
        kind: ProviderKind::OpenAiCompatible,
        stability: ProviderStability::Stable,
        auth: ProviderAuth::EnvVar {
            key: "XAI_API_KEY",
            base_url_env: Some("XAI_BASE_URL"),
        },
        models: XAI_MODELS,
        capabilities: API_CAPS,
        notes: "OpenAI-compatible xAI endpoint.",
    },
    ProviderInfo {
        id: "deepseek",
        display_name: "DeepSeek",
        kind: ProviderKind::OpenAiCompatible,
        stability: ProviderStability::Stable,
        auth: ProviderAuth::EnvVar {
            key: "DEEPSEEK_API_KEY",
            base_url_env: Some("DEEPSEEK_BASE_URL"),
        },
        models: DEEPSEEK_MODELS,
        capabilities: API_CAPS,
        notes: "OpenAI-compatible DeepSeek endpoint.",
    },
    ProviderInfo {
        id: "mistral",
        display_name: "Mistral",
        kind: ProviderKind::OpenAiCompatible,
        stability: ProviderStability::Stable,
        auth: ProviderAuth::EnvVar {
            key: "MISTRAL_API_KEY",
            base_url_env: Some("MISTRAL_BASE_URL"),
        },
        models: MISTRAL_MODELS,
        capabilities: API_CAPS,
        notes: "OpenAI-compatible Mistral endpoint.",
    },
    ProviderInfo {
        id: "anthropic",
        display_name: "Anthropic",
        kind: ProviderKind::Anthropic,
        stability: ProviderStability::Stable,
        auth: ProviderAuth::EnvVar {
            key: "ANTHROPIC_API_KEY",
            base_url_env: Some("ANTHROPIC_BASE_URL"),
        },
        models: CLAUDE_MODELS,
        capabilities: API_CAPS,
        notes: "Anthropic Messages API over reqwest/SSE.",
    },
    ProviderInfo {
        id: "codex",
        display_name: "Codex CLI",
        kind: ProviderKind::Cli,
        stability: ProviderStability::Stable,
        auth: ProviderAuth::CliBinary {
            binary: "codex",
            env_override: "OXIDE_CODEX_BIN",
        },
        models: CODEX_MODELS,
        capabilities: CODEX_CLI_CAPS,
        notes: "Uses the local authenticated Codex CLI.",
    },
    ProviderInfo {
        id: "claude",
        display_name: "Claude Code CLI",
        kind: ProviderKind::Cli,
        stability: ProviderStability::Stable,
        auth: ProviderAuth::CliBinary {
            binary: "claude",
            env_override: "OXIDE_CLAUDE_BIN",
        },
        models: CLAUDE_MODELS,
        capabilities: CLAUDE_CLI_CAPS,
        notes: "Uses the local authenticated Claude Code CLI.",
    },
    ProviderInfo {
        id: "claude_interactive",
        display_name: "Claude Code Interactive",
        kind: ProviderKind::Cli,
        stability: ProviderStability::Experimental,
        auth: ProviderAuth::CliBinary {
            binary: "claude",
            env_override: "OXIDE_CLAUDE_BIN",
        },
        models: CLAUDE_MODELS,
        capabilities: CLAUDE_CLI_CAPS,
        notes: "Experimental PTY-backed Claude Code mode.",
    },
    ProviderInfo {
        id: "chatgpt",
        display_name: "ChatGPT Subscription",
        kind: ProviderKind::ChatGptSubscription,
        stability: ProviderStability::Experimental,
        auth: ProviderAuth::CodexOAuth {
            path: "~/.codex/auth.json",
        },
        models: CHATGPT_MODELS,
        capabilities: CHATGPT_CAPS,
        notes: "Uses Codex OAuth credentials and the ChatGPT subscription backend.",
    },
    ProviderInfo {
        id: "mock_plan",
        display_name: "Mock Planner",
        kind: ProviderKind::Test,
        stability: ProviderStability::Test,
        auth: ProviderAuth::TestOnly,
        models: MOCK_MODELS,
        capabilities: BASIC_CAPS,
        notes: "Scripted planner for orchestration tests.",
    },
    ProviderInfo {
        id: "mock",
        display_name: "Mock Tool",
        kind: ProviderKind::Test,
        stability: ProviderStability::Test,
        auth: ProviderAuth::TestOnly,
        models: MOCK_MODELS,
        capabilities: MOCK_CAPS,
        notes: "Scripted tool-call provider for tests.",
    },
    ProviderInfo {
        id: "mock_mcp",
        display_name: "Mock MCP",
        kind: ProviderKind::Test,
        stability: ProviderStability::Test,
        auth: ProviderAuth::TestOnly,
        models: MOCK_MODELS,
        capabilities: MCP_MOCK_CAPS,
        notes: "Scripted MCP dispatch provider for tests.",
    },
    ProviderInfo {
        id: "mock_browser",
        display_name: "Mock Browser",
        kind: ProviderKind::Test,
        stability: ProviderStability::Test,
        auth: ProviderAuth::TestOnly,
        models: MOCK_MODELS,
        capabilities: BROWSER_MOCK_CAPS,
        notes: "Scripted browser target/snapshot provider for tests.",
    },
];

pub fn list_providers() -> &'static [ProviderInfo] {
    PROVIDERS
}

pub fn provider_info(id: &str) -> Option<&'static ProviderInfo> {
    PROVIDERS.iter().find(|provider| provider.id == id)
}

pub fn list_provider_models(id: &str) -> Option<&'static [ProviderModel]> {
    provider_info(id).map(|provider| provider.models)
}

pub fn list_provider_capabilities(id: &str) -> Option<&'static [ProviderCapability]> {
    provider_info(id).map(|provider| provider.capabilities)
}

pub fn default_model_for_provider(id: &str) -> Option<&'static str> {
    provider_info(id).and_then(|provider| {
        provider
            .models
            .iter()
            .find(|model| model.is_default)
            .map(|model| model.id)
    })
}

pub fn fast_model_for_provider(id: &str) -> Option<&'static str> {
    provider_info(id).and_then(|provider| {
        provider
            .models
            .iter()
            .find(|model| model.is_fast)
            .map(|model| model.id)
    })
}

pub fn diagnose_providers() -> Vec<ProviderDiagnostic> {
    PROVIDERS.iter().map(diagnose_provider_info).collect()
}

pub fn diagnose_provider(id: &str) -> Option<ProviderDiagnostic> {
    provider_info(id).map(diagnose_provider_info)
}

fn diagnose_provider_info(provider: &'static ProviderInfo) -> ProviderDiagnostic {
    match provider.auth {
        ProviderAuth::None => ProviderDiagnostic {
            provider_id: provider.id,
            status: DiagnosticStatus::Ready,
            summary: "No local credential needed".to_string(),
            detail: provider.notes.to_string(),
        },
        ProviderAuth::EnvVar { key, base_url_env } => diagnose_env(provider, key, base_url_env),
        ProviderAuth::CliBinary {
            binary,
            env_override,
        } => diagnose_cli(provider, binary, env_override),
        ProviderAuth::CodexOAuth { path } => diagnose_codex_oauth(provider, path),
        ProviderAuth::TestOnly => ProviderDiagnostic {
            provider_id: provider.id,
            status: DiagnosticStatus::Warning,
            summary: "Test-only provider".to_string(),
            detail: provider.notes.to_string(),
        },
    }
}

fn diagnose_env(
    provider: &'static ProviderInfo,
    key: &'static str,
    base_url_env: Option<&'static str>,
) -> ProviderDiagnostic {
    let key_set = std::env::var(key)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    if !key_set {
        return ProviderDiagnostic {
            provider_id: provider.id,
            status: DiagnosticStatus::Missing,
            summary: format!("{key} is not set"),
            detail: provider.auth.summary(),
        };
    }
    let base = base_url_env
        .and_then(|env| std::env::var(env).ok().map(|value| (env, value)))
        .filter(|(_, value)| !value.trim().is_empty())
        .map(|(env, _)| format!("; {env} override is set"))
        .unwrap_or_default();
    ProviderDiagnostic {
        provider_id: provider.id,
        status: DiagnosticStatus::Ready,
        summary: format!("{key} is set"),
        detail: format!("{}{}", provider.notes, base),
    }
}

fn diagnose_cli(
    provider: &'static ProviderInfo,
    binary: &'static str,
    env_override: &'static str,
) -> ProviderDiagnostic {
    match find_cli_binary(binary, env_override) {
        Ok(path) => ProviderDiagnostic {
            provider_id: provider.id,
            status: DiagnosticStatus::Ready,
            summary: format!("`{binary}` found"),
            detail: format!("{} at {}", provider.notes, path.display()),
        },
        Err(detail) => ProviderDiagnostic {
            provider_id: provider.id,
            status: DiagnosticStatus::Missing,
            summary: format!("`{binary}` was not found"),
            detail,
        },
    }
}

fn diagnose_codex_oauth(provider: &'static ProviderInfo, path: &str) -> ProviderDiagnostic {
    let Some(path) = expand_home(path) else {
        return ProviderDiagnostic {
            provider_id: provider.id,
            status: DiagnosticStatus::Missing,
            summary: "HOME is not set".to_string(),
            detail: "Cannot locate Codex OAuth credentials without HOME.".to_string(),
        };
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            return ProviderDiagnostic {
                provider_id: provider.id,
                status: DiagnosticStatus::Missing,
                summary: "Codex OAuth credentials were not found".to_string(),
                detail: format!("{}: {error}", path.display()),
            };
        }
    };
    let value = serde_json::from_str::<serde_json::Value>(&text).unwrap_or_default();
    let has_token = ["access_token", "refresh_token"].iter().any(|key| {
        value["tokens"][key]
            .as_str()
            .map(|token| !token.trim().is_empty())
            .unwrap_or(false)
    });
    if !has_token {
        return ProviderDiagnostic {
            provider_id: provider.id,
            status: DiagnosticStatus::Missing,
            summary: "Codex OAuth token is missing".to_string(),
            detail: format!(
                "{} exists but does not contain tokens.access_token or tokens.refresh_token",
                path.display()
            ),
        };
    }
    ProviderDiagnostic {
        provider_id: provider.id,
        status: DiagnosticStatus::Ready,
        summary: "Codex OAuth credentials found".to_string(),
        detail: provider.notes.to_string(),
    }
}

fn find_cli_binary(binary: &str, env_override: &str) -> Result<PathBuf, String> {
    if let Some(raw) = std::env::var_os(env_override).filter(|value| !value.is_empty()) {
        let override_path = PathBuf::from(&raw);
        if override_path.components().count() > 1 {
            return path_exists(&override_path)
                .then_some(override_path.clone())
                .ok_or_else(|| {
                    format!(
                        "{env_override} points to missing file {}",
                        override_path.display()
                    )
                });
        }
        if let Some(path) = find_on_path(&override_path.to_string_lossy()) {
            return Ok(path);
        }
        return Err(format!(
            "{env_override} is set to `{}` but it was not found on PATH",
            override_path.display()
        ));
    }

    for candidate in common_binary_candidates(binary) {
        if path_exists(&candidate) {
            return Ok(candidate);
        }
    }
    find_on_path(binary).ok_or_else(|| {
        format!(
            "`{binary}` was not found in common install locations or PATH; set {env_override} to the binary path"
        )
    })
}

fn common_binary_candidates(binary: &str) -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    [
        ".superconductor/bin",
        ".local/bin",
        ".bun/bin",
        ".npm-global/bin",
        ".codex/bin",
    ]
    .into_iter()
    .map(|dir| home.join(dir).join(binary))
    .chain([
        PathBuf::from("/opt/homebrew/bin").join(binary),
        PathBuf::from("/usr/local/bin").join(binary),
    ])
    .collect()
}

fn find_on_path(binary: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join(binary))
        .find(|path| path_exists(path))
}

fn path_exists(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
}

fn expand_home(path: &str) -> Option<PathBuf> {
    if let Some(rest) = path.strip_prefix("~/") {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(rest));
    }
    Some(PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_lists_runtime_provider_ids() {
        let ids: Vec<&str> = list_providers()
            .iter()
            .map(|provider| provider.id)
            .collect();

        assert_eq!(
            ids,
            vec![
                "echo",
                "openai",
                "gemini",
                "xai",
                "deepseek",
                "mistral",
                "anthropic",
                "codex",
                "claude",
                "claude_interactive",
                "chatgpt",
                "mock_plan",
                "mock",
                "mock_mcp",
                "mock_browser",
            ]
        );
    }

    #[test]
    fn public_model_and_capability_lookup_uses_provider_id() {
        let models = list_provider_models("codex").expect("codex models");
        let caps = list_provider_capabilities("codex").expect("codex caps");

        assert!(models.iter().any(|model| model.id == "gpt-5.6-sol"));
        assert!(models.iter().any(|model| model.id == "gpt-5.6-terra"));
        assert!(models.iter().any(|model| model.id == "gpt-5.6-luna"));
        assert!(models.iter().any(|model| model.id == "gpt-5.3-codex"));
        assert_eq!(
            fast_model_for_provider("codex"),
            Some("gpt-5.3-codex-spark")
        );
        assert!(caps.contains(&ProviderCapability::NativeCliTools));
        assert_eq!(list_provider_models("unknown"), None);
    }

    #[test]
    fn chatgpt_catalog_includes_gpt_5_6_family_without_changing_default() {
        let models = list_provider_models("chatgpt").expect("chatgpt models");

        assert!(models.iter().any(|model| model.id == "gpt-5.6-sol"));
        assert!(models.iter().any(|model| model.id == "gpt-5.6-terra"));
        assert!(models.iter().any(|model| model.id == "gpt-5.6-luna"));
        assert_eq!(default_model_for_provider("chatgpt"), Some("gpt-5.5"));
    }

    #[test]
    fn diagnostic_for_local_provider_is_ready() {
        let diagnostic = diagnose_provider("echo").expect("echo diagnostic");

        assert_eq!(diagnostic.provider_id, "echo");
        assert_eq!(diagnostic.status, DiagnosticStatus::Ready);
    }

    #[test]
    fn auth_summaries_are_actionable() {
        let openai = provider_info("openai").expect("openai provider");
        let codex = provider_info("codex").expect("codex provider");

        assert!(openai.auth.summary().contains("OPENAI_API_KEY"));
        assert!(codex.auth.summary().contains("OXIDE_CODEX_BIN"));
    }
}
