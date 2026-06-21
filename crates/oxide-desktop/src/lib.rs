//! Rust-native desktop command center for Oxide.
//!
//! This is the Codex-like desktop direction for Oxide: a multi-panel app shell,
//! animated agent timeline, settings surfaces, and the same `oxide-core` engine.

use eframe::egui::{
    self, Align, CentralPanel, Color32, ColorImage, Context, FontId, Frame, Key, Layout, Margin,
    RichText, ScrollArea, Sense, SidePanel, Stroke, TextEdit, TextureHandle, TextureOptions,
    TopBottomPanel, Ui, Vec2, Visuals,
};
use oxide_config::{Config, McpServerConfig};
use oxide_protocol::{ApprovalDecision, ApprovalPolicy, Event, Op, SandboxPolicy};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc as tokio_mpsc;

const ACCENT: Color32 = Color32::from_rgb(72, 220, 163);
const BG: Color32 = Color32::from_rgb(24, 24, 27);
const PANEL: Color32 = Color32::from_rgb(31, 31, 36);
const PANEL_HI: Color32 = Color32::from_rgb(42, 42, 48);
const TEXT: Color32 = Color32::from_rgb(237, 237, 240);
const MUTED: Color32 = Color32::from_rgb(158, 158, 168);
const FAINT: Color32 = Color32::from_rgb(102, 102, 114);
const DANGER: Color32 = Color32::from_rgb(224, 145, 58);
const FILE_CONTEXT_CHAR_LIMIT: usize = 12_000;
const REPO_INDEX_ENTRY_LIMIT: usize = 800;
const REPO_INDEX_MAX_DEPTH: usize = 6;
const COMPOSER_CONTEXT_SUGGESTION_LIMIT: usize = 6;
const SESSION_RESUME_CHAR_LIMIT: usize = 14_000;
const AUTOMATION_TICK_INTERVAL_MS: u64 = 30_000;
const GOAL_MODE_MARKER: &str = "[oxide-goal-context]";

#[derive(Clone)]
struct ModelPreset {
    provider: &'static str,
    model: &'static str,
    provider_label: &'static str,
    label: &'static str,
    summary: &'static str,
    badge: &'static str,
    fast: bool,
}

const MODELS: &[ModelPreset] = &[
    ModelPreset {
        provider: "codex",
        model: "gpt-5.5",
        provider_label: "Codex",
        label: "GPT-5.5",
        summary: "Deep coding agent, long tasks, careful reviews",
        badge: "Smart",
        fast: false,
    },
    ModelPreset {
        provider: "codex",
        model: "gpt-5.3-codex-spark",
        provider_label: "Codex",
        label: "GPT-5.3 Codex Spark",
        summary: "Ultra-fast real-time coding lane for Codex",
        badge: "Fast",
        fast: true,
    },
    ModelPreset {
        provider: "claude",
        model: "claude-opus-4-8",
        provider_label: "Claude Code",
        label: "Opus 4.8",
        summary: "Hardest architecture and reasoning work",
        badge: "Smart",
        fast: false,
    },
    ModelPreset {
        provider: "claude",
        model: "claude-sonnet-4-6",
        provider_label: "Claude Code",
        label: "Sonnet 4.6",
        summary: "Balanced daily coding agent work",
        badge: "Fast",
        fast: true,
    },
    ModelPreset {
        provider: "openai",
        model: "gpt-5.5",
        provider_label: "OpenAI API",
        label: "GPT-5.5",
        summary: "API-backed complex coding workflows",
        badge: "Smart",
        fast: false,
    },
    ModelPreset {
        provider: "openai",
        model: "gpt-5.4",
        provider_label: "OpenAI API",
        label: "GPT-5.4",
        summary: "API-backed faster frontier coding lane",
        badge: "Fast",
        fast: true,
    },
    ModelPreset {
        provider: "gemini",
        model: "gemini-3.1-pro",
        provider_label: "Gemini API",
        label: "Gemini 3.1 Pro",
        summary: "Advanced intelligence for complex agentic and coding work",
        badge: "Smart",
        fast: false,
    },
    ModelPreset {
        provider: "gemini",
        model: "gemini-3.5-flash",
        provider_label: "Gemini API",
        label: "Gemini 3.5 Flash",
        summary: "Stable fast lane for agentic and coding tasks",
        badge: "Fast",
        fast: true,
    },
    ModelPreset {
        provider: "xai",
        model: "grok-4.3",
        provider_label: "xAI API",
        label: "Grok 4.3",
        summary: "Strong agentic tool calling with configurable reasoning",
        badge: "Smart",
        fast: false,
    },
    ModelPreset {
        provider: "xai",
        model: "grok-build-0.1",
        provider_label: "xAI API",
        label: "Grok Build 0.1",
        summary: "Fast coding model for agentic coding workflows",
        badge: "Fast",
        fast: true,
    },
    ModelPreset {
        provider: "deepseek",
        model: "deepseek-v4-pro",
        provider_label: "DeepSeek API",
        label: "V4 Pro",
        summary: "DeepSeek V4 smart lane with thinking support",
        badge: "Smart",
        fast: false,
    },
    ModelPreset {
        provider: "deepseek",
        model: "deepseek-v4-flash",
        provider_label: "DeepSeek API",
        label: "V4 Flash",
        summary: "DeepSeek V4 fast lane with thinking support",
        badge: "Fast",
        fast: true,
    },
    ModelPreset {
        provider: "mistral",
        model: "mistral-medium-3-5",
        provider_label: "Mistral API",
        label: "Medium 3.5",
        summary: "Frontier-class multimodal model optimized for coding",
        badge: "Smart",
        fast: false,
    },
    ModelPreset {
        provider: "mistral",
        model: "mistral-small-4",
        provider_label: "Mistral API",
        label: "Small 4",
        summary: "Efficient hybrid instruct, reasoning, and coding model",
        badge: "Fast",
        fast: true,
    },
    ModelPreset {
        provider: "anthropic",
        model: "claude-opus-4-8",
        provider_label: "Anthropic API",
        label: "Opus 4.8",
        summary: "API-backed deep reasoning",
        badge: "Smart",
        fast: false,
    },
    ModelPreset {
        provider: "anthropic",
        model: "claude-sonnet-4-6",
        provider_label: "Anthropic API",
        label: "Sonnet 4.6",
        summary: "API-backed fast daily agent",
        badge: "Fast",
        fast: true,
    },
    ModelPreset {
        provider: "echo",
        model: "",
        provider_label: "Dev",
        label: "Echo",
        summary: "Offline UI smoke test",
        badge: "Dev",
        fast: true,
    },
    ModelPreset {
        provider: "mock",
        model: "",
        provider_label: "Dev",
        label: "Mock Tool",
        summary: "Tool routing demo",
        badge: "Dev",
        fast: true,
    },
];

#[derive(Clone)]
struct EffortPreset {
    value: &'static str,
    label: &'static str,
    summary: &'static str,
}

const EFFORTS: &[EffortPreset] = &[
    EffortPreset {
        value: "low",
        label: "Low",
        summary: "Fastest response, light reasoning",
    },
    EffortPreset {
        value: "medium",
        label: "Medium",
        summary: "Balanced default",
    },
    EffortPreset {
        value: "high",
        label: "High",
        summary: "Deeper planning and verification",
    },
    EffortPreset {
        value: "xhigh",
        label: "Extra",
        summary: "Long, hard agent runs",
    },
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum NavSurface {
    Chat,
    Search,
    Plugins,
    Automations,
    Hermes,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InspectorTab {
    Timeline,
    Goal,
    Approvals,
    Checkpoints,
    Usage,
    Files,
    Diff,
    Terminal,
    Browser,
    Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsTab {
    General,
    Personalization,
    Appearance,
    Models,
    Permissions,
    Automations,
    Plugins,
    Hermes,
    Git,
    Browser,
    Memory,
    Shortcuts,
    Advanced,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DetailLevel {
    Default,
    Coding,
}

#[derive(Clone, PartialEq, Eq)]
enum MsgKind {
    User,
    Agent,
    Note,
}

#[derive(Clone)]
struct ChatMsg {
    kind: MsgKind,
    text: String,
}

#[derive(Clone)]
struct SessionSummary {
    id: String,
    title: String,
    path: PathBuf,
    message_count: usize,
    last_ts_ms: u128,
    pinned: bool,
    archived: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SessionMeta {
    id: String,
    title: Option<String>,
    pinned: bool,
    archived: bool,
    updated_ms: u64,
}

#[derive(Clone, Default)]
struct GitSnapshot {
    status: String,
    diff_stat: String,
    raw_diff: String,
    changed_files: Vec<GitChangedFile>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GitChangedFile {
    status: String,
    path: String,
    display_path: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct GitBranchSnapshot {
    current_branch: String,
    branches: Vec<String>,
    worktrees: Vec<GitWorktreeInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GitWorktreeInfo {
    path: String,
    branch: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct McpServerHealth {
    name: String,
    status: String,
    tool_count: usize,
    tools: Vec<String>,
    detail: String,
}

struct TerminalJob {
    id: u64,
    command: String,
    child: Arc<Mutex<std::process::Child>>,
    stopping: bool,
}

enum TerminalEvent {
    Line(String),
    Finished { id: u64, code: i32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalStream {
    Stdout,
    Stderr,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct AutomationSpec {
    id: String,
    name: String,
    kind: String,
    status: String,
    schedule: String,
    prompt: String,
    created_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct AutomationRunSpec {
    id: String,
    automation_id: String,
    automation_name: String,
    trigger: String,
    status: String,
    prompt: String,
    started_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct AppshotSpec {
    id: String,
    title: String,
    path: String,
    note: String,
    #[serde(default)]
    annotations: Vec<AppshotAnnotation>,
    #[serde(default)]
    source_url: String,
    #[serde(default)]
    browser_action_id: String,
    created_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct AppshotAnnotation {
    label: String,
    target: String,
    note: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct BrowserActionSpec {
    id: String,
    action: String,
    url: String,
    note: String,
    created_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct DesktopStateSpec {
    #[serde(default)]
    recent_workspaces: Vec<RecentWorkspaceSpec>,
    #[serde(default)]
    preferences: DesktopPreferences,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct DesktopPreferences {
    #[serde(default = "default_true")]
    motion_enabled: bool,
    #[serde(default)]
    compact_sidebar: bool,
    #[serde(default = "default_true")]
    fold_tool_results: bool,
    #[serde(default)]
    prevent_sleep: bool,
    #[serde(default)]
    attach_memory_to_prompt: bool,
    #[serde(default)]
    attach_appshots_to_prompt: bool,
    #[serde(default = "default_detail_level_id")]
    detail_level: String,
    #[serde(default = "default_personalization_tone_id")]
    personalization_tone: String,
    #[serde(default)]
    custom_instructions: String,
    #[serde(default)]
    goal_mode_enabled: bool,
    #[serde(default)]
    active_goal: String,
    #[serde(default)]
    goal_success_criteria: String,
}

impl Default for DesktopPreferences {
    fn default() -> Self {
        Self {
            motion_enabled: true,
            compact_sidebar: false,
            fold_tool_results: true,
            prevent_sleep: false,
            attach_memory_to_prompt: false,
            attach_appshots_to_prompt: false,
            detail_level: default_detail_level_id(),
            personalization_tone: default_personalization_tone_id(),
            custom_instructions: String::new(),
            goal_mode_enabled: false,
            active_goal: String::new(),
            goal_success_criteria: String::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct RecentWorkspaceSpec {
    path: String,
    name: String,
    last_opened_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct MemorySpec {
    id: String,
    title: String,
    body: String,
    enabled: bool,
    created_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct HermesProfile {
    id: String,
    name: String,
    goal: String,
    validation: String,
    review_prompt: String,
    created_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ShortcutSpec {
    id: &'static str,
    title: &'static str,
    keys: &'static str,
    scope: &'static str,
    detail: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CommandSpec {
    id: &'static str,
    title: &'static str,
    detail: &'static str,
    keys: &'static str,
    scope: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiagnosticItem {
    label: String,
    value: String,
    status: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SearchResultItem {
    kind: String,
    title: String,
    detail: String,
    target: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RepoIndexEntry {
    path: PathBuf,
    relative: String,
    is_dir: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingApproval {
    request_id: u64,
    tool: String,
    summary: String,
    created_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceCheckpoint {
    id: u64,
    label: String,
    created_ms: u64,
    rewound: bool,
    restored_files: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TokenUsageRecord {
    turn: u64,
    input: u64,
    output: u64,
    created_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CompactionRecord {
    dropped: u64,
    tokens: u64,
    created_ms: u64,
}

struct GlobalSearchInputs<'a> {
    workspace: &'a Path,
    sessions: &'a [SessionSummary],
    memories: &'a [MemorySpec],
    automations: &'a [AutomationSpec],
    appshots: &'a [AppshotSpec],
    hermes_profiles: &'a [HermesProfile],
    mcp_servers: &'a [McpServerConfig],
    shortcuts: &'a [ShortcutSpec],
    goal_mode_enabled: bool,
    active_goal: &'a str,
    goal_success_criteria: &'a str,
    query: &'a str,
}

struct CommandPaletteInputs<'a> {
    search: GlobalSearchInputs<'a>,
    repo_index: &'a [RepoIndexEntry],
    has_selected_session: bool,
    has_pending_browser_snapshot: bool,
    has_selected_git_file: bool,
    has_active_terminal_job: bool,
}

#[derive(Clone)]
struct TimelineItem {
    title: String,
    detail: String,
    state: TimelineState,
    request_id: Option<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TimelineState {
    Running,
    Done,
    Waiting,
    Error,
}

enum RuntimeCmd {
    Op(Op),
    Reconfigure(Config),
}

pub fn run(config: Config) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Oxide")
            .with_inner_size([1320.0, 860.0])
            .with_min_inner_size([1040.0, 680.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Oxide",
        options,
        Box::new(move |cc| Ok(Box::new(OxideDesktop::new(config, cc)))),
    )
    .map_err(|e| anyhow::anyhow!("desktop failed: {e}"))
}

struct OxideDesktop {
    cfg: Config,
    workspace: PathBuf,
    recent_workspaces: Vec<RecentWorkspaceSpec>,
    workspace_input: String,
    workspace_message: String,
    engine_tx: tokio_mpsc::UnboundedSender<RuntimeCmd>,
    event_rx: mpsc::Receiver<Event>,
    term_rx: mpsc::Receiver<TerminalEvent>,
    term_tx: mpsc::Sender<TerminalEvent>,
    nav: NavSurface,
    inspector: InspectorTab,
    settings_tab: SettingsTab,
    chat: Vec<ChatMsg>,
    timeline: Vec<TimelineItem>,
    pending_approvals: Vec<PendingApproval>,
    checkpoints: Vec<WorkspaceCheckpoint>,
    token_usage: Vec<TokenUsageRecord>,
    compactions: Vec<CompactionRecord>,
    goal_mode_enabled: bool,
    active_goal: String,
    goal_success_criteria: String,
    goal_message: String,
    sessions: Vec<SessionSummary>,
    selected_session: Option<String>,
    pending_session_context: Option<String>,
    session_query: String,
    show_archived_sessions: bool,
    rename_session_title: String,
    git_snapshot: GitSnapshot,
    git_branches: GitBranchSnapshot,
    selected_git_file: Option<GitChangedFile>,
    selected_git_file_diff: String,
    git_review_message: String,
    git_commit_message: String,
    git_push_remote: String,
    git_push_branch: String,
    git_pr_base: String,
    git_pr_title: String,
    git_pr_body: String,
    automations: Vec<AutomationSpec>,
    automation_runs: Vec<AutomationRunSpec>,
    last_automation_tick_ms: u64,
    appshots: Vec<AppshotSpec>,
    appshot_textures: BTreeMap<String, TextureHandle>,
    pending_browser_snapshot: Option<AppshotSpec>,
    browser_actions: Vec<BrowserActionSpec>,
    memories: Vec<MemorySpec>,
    project_rules: String,
    hermes_profiles: Vec<HermesProfile>,
    prompt: String,
    queued_prompts: VecDeque<String>,
    steer_text: String,
    palette_query: String,
    model_query: String,
    evolve_goal: String,
    evolve_validation: String,
    hermes_profile_name: String,
    hermes_review_prompt: String,
    hermes_message: String,
    automation_name: String,
    automation_schedule: String,
    automation_prompt: String,
    mcp_name: String,
    mcp_command: String,
    mcp_args: String,
    mcp_message: String,
    mcp_health: BTreeMap<String, McpServerHealth>,
    appshot_title: String,
    appshot_path: String,
    appshot_note: String,
    appshot_annotation_label: String,
    appshot_annotation_target: String,
    appshot_annotation_note: String,
    attach_appshots_to_prompt: bool,
    browser_target_url: String,
    browser_action_note: String,
    browser_message: String,
    attach_memory_to_prompt: bool,
    memory_title: String,
    memory_body: String,
    memory_message: String,
    shortcut_query: String,
    diagnostics: Vec<DiagnosticItem>,
    diagnostics_filter: String,
    worktree_branch: String,
    git_operation_message: String,
    terminal_input: String,
    terminal_lines: VecDeque<String>,
    active_terminal_job: Option<TerminalJob>,
    selected_file: Option<PathBuf>,
    selected_file_text: String,
    repo_index: Vec<RepoIndexEntry>,
    file_query: String,
    composer_file_query: String,
    file_message: String,
    context_window: Option<u64>,
    streaming: bool,
    detail_level: DetailLevel,
    personalization_tone: String,
    custom_instructions: String,
    fold_tool_results: bool,
    prevent_sleep: bool,
    sleep_guard: Option<std::process::Child>,
    show_palette: bool,
    show_settings: bool,
    motion_enabled: bool,
    compact_sidebar: bool,
    last_tick: Instant,
}

impl OxideDesktop {
    fn new(config: Config, cc: &eframe::CreationContext<'_>) -> Self {
        install_style(&cc.egui_ctx);
        let workspace = workspace_of(&config);
        let (engine_tx, event_rx) = spawn_engine(config.clone());
        let (term_tx, term_rx) = mpsc::channel();
        let mut desktop_state = read_desktop_state();
        desktop_state.recent_workspaces = upsert_recent_workspace(
            desktop_state.recent_workspaces,
            &workspace,
            &project_name(&workspace),
            now_ms(),
            12,
        );
        let _ = write_desktop_state(&desktop_state);
        let preferences = desktop_state.preferences.clone();
        let sessions = read_all_session_summaries(&workspace).unwrap_or_default();
        let git_snapshot = git_workspace_snapshot(&workspace);
        let git_branches = git_branch_snapshot(&workspace);
        let git_push_branch = git_branches.current_branch.clone();
        let worktree_branch = default_worktree_branch(&git_branches);
        let automations = read_automation_specs(&workspace).unwrap_or_default();
        let automation_runs = read_automation_run_specs(&workspace).unwrap_or_default();
        let appshots = read_appshot_specs(&workspace).unwrap_or_default();
        let browser_actions = read_browser_action_specs(&workspace).unwrap_or_default();
        let memories = read_memory_specs(&workspace).unwrap_or_default();
        let project_rules = read_project_rules(&workspace).unwrap_or_default();
        let hermes_profiles = read_hermes_profiles(&workspace).unwrap_or_default();
        let diagnostics = collect_diagnostics(&config, &workspace);
        let repo_index = collect_repo_index(&workspace, REPO_INDEX_ENTRY_LIMIT);
        let mcp_health = configured_mcp_health(&config.mcp_servers);
        let mut app = Self {
            cfg: config,
            workspace: workspace.clone(),
            recent_workspaces: desktop_state.recent_workspaces,
            workspace_input: workspace.display().to_string(),
            workspace_message: String::new(),
            engine_tx,
            event_rx,
            term_rx,
            term_tx,
            nav: NavSurface::Chat,
            inspector: InspectorTab::Timeline,
            settings_tab: SettingsTab::General,
            chat: Vec::new(),
            sessions,
            selected_session: None,
            pending_session_context: None,
            session_query: String::new(),
            show_archived_sessions: false,
            rename_session_title: String::new(),
            git_snapshot,
            git_branches,
            selected_git_file: None,
            selected_git_file_diff: String::new(),
            git_review_message: String::new(),
            git_commit_message: String::new(),
            git_push_remote: "origin".to_string(),
            git_push_branch,
            git_pr_base: "main".to_string(),
            git_pr_title: String::new(),
            git_pr_body: String::new(),
            automations,
            automation_runs,
            last_automation_tick_ms: 0,
            appshots,
            appshot_textures: BTreeMap::new(),
            pending_browser_snapshot: None,
            browser_actions,
            memories,
            project_rules,
            hermes_profiles,
            timeline: vec![
                TimelineItem {
                    title: "Desktop shell".to_string(),
                    detail: "Rust-native command center ready".to_string(),
                    state: TimelineState::Done,
                    request_id: None,
                },
                TimelineItem {
                    title: "Hermes lane".to_string(),
                    detail: "Evolve and workflow surfaces are staged".to_string(),
                    state: TimelineState::Waiting,
                    request_id: None,
                },
            ],
            pending_approvals: Vec::new(),
            checkpoints: Vec::new(),
            token_usage: Vec::new(),
            compactions: Vec::new(),
            goal_mode_enabled: preferences.goal_mode_enabled,
            active_goal: preferences.active_goal,
            goal_success_criteria: preferences.goal_success_criteria,
            goal_message: String::new(),
            prompt: String::new(),
            queued_prompts: VecDeque::new(),
            steer_text: String::new(),
            palette_query: String::new(),
            model_query: String::new(),
            evolve_goal: "Improve Oxide toward Codex desktop parity while staying Rust-native"
                .to_string(),
            evolve_validation: "cargo test -p oxide-desktop && cargo check -p oxide-cli"
                .to_string(),
            hermes_profile_name: "Desktop parity".to_string(),
            hermes_review_prompt: "Review spec compliance, UX parity, compile risks, and whether the implementation moves Oxide closer to Codex Desktop without leaving Rust-native boundaries.".to_string(),
            hermes_message: String::new(),
            automation_name: "Daily workspace review".to_string(),
            automation_schedule: "FREQ=DAILY;INTERVAL=1".to_string(),
            automation_prompt:
                "Review the workspace and propose the next highest-impact Oxide improvement."
                    .to_string(),
            mcp_name: "fs".to_string(),
            mcp_command: "npx".to_string(),
            mcp_args: "-y @modelcontextprotocol/server-filesystem .".to_string(),
            mcp_message: String::new(),
            mcp_health,
            appshot_title: String::new(),
            appshot_path: String::new(),
            appshot_note: String::new(),
            appshot_annotation_label: "A".to_string(),
            appshot_annotation_target: String::new(),
            appshot_annotation_note: String::new(),
            attach_appshots_to_prompt: preferences.attach_appshots_to_prompt,
            browser_target_url: String::new(),
            browser_action_note: String::new(),
            browser_message: String::new(),
            attach_memory_to_prompt: preferences.attach_memory_to_prompt,
            memory_title: String::new(),
            memory_body: String::new(),
            memory_message: String::new(),
            shortcut_query: String::new(),
            diagnostics,
            diagnostics_filter: String::new(),
            worktree_branch,
            git_operation_message: String::new(),
            terminal_input: String::new(),
            terminal_lines: VecDeque::from(["$ oxide desktop ready".to_string()]),
            active_terminal_job: None,
            selected_file: None,
            selected_file_text: String::new(),
            repo_index,
            file_query: String::new(),
            composer_file_query: String::new(),
            file_message: String::new(),
            context_window: None,
            streaming: false,
            detail_level: detail_level_from_id(&preferences.detail_level),
            personalization_tone: personalization_tone_id(&preferences.personalization_tone)
                .to_string(),
            custom_instructions: preferences.custom_instructions,
            fold_tool_results: preferences.fold_tool_results,
            prevent_sleep: preferences.prevent_sleep,
            sleep_guard: None,
            show_palette: false,
            show_settings: false,
            motion_enabled: preferences.motion_enabled,
            compact_sidebar: preferences.compact_sidebar,
            last_tick: Instant::now(),
        };
        if app.prevent_sleep {
            app.sync_sleep_guard();
        }
        app
    }

    fn poll_runtime(&mut self) {
        while let Ok(event) = self.term_rx.try_recv() {
            match event {
                TerminalEvent::Line(line) => {
                    self.terminal_lines.push_back(line);
                    trim_terminal(&mut self.terminal_lines);
                }
                TerminalEvent::Finished { id, code } => {
                    self.terminal_lines.push_back(terminal_finished_line(code));
                    trim_terminal(&mut self.terminal_lines);
                    if self
                        .active_terminal_job
                        .as_ref()
                        .map(|job| job.id == id)
                        .unwrap_or(false)
                    {
                        self.active_terminal_job = None;
                    }
                    self.timeline.push(TimelineItem {
                        title: "Terminal finished".to_string(),
                        detail: terminal_finished_line(code),
                        state: if code == 0 {
                            TimelineState::Done
                        } else {
                            TimelineState::Error
                        },
                        request_id: None,
                    });
                }
            }
        }
        while let Ok(event) = self.event_rx.try_recv() {
            self.apply_event(event);
        }
    }

    fn apply_event(&mut self, event: Event) {
        match event {
            Event::Ready { harness } => {
                self.timeline.push(TimelineItem {
                    title: "Engine ready".to_string(),
                    detail: format!("Harness: {harness}"),
                    state: TimelineState::Done,
                    request_id: None,
                });
            }
            Event::SessionPath { .. } => {}
            Event::Followups { .. } => {}
            Event::TurnStarted { turn } => {
                self.streaming = true;
                self.timeline.push(TimelineItem {
                    title: format!("{turn} started"),
                    detail: "Agent is thinking and may use tools".to_string(),
                    state: TimelineState::Running,
                    request_id: None,
                });
            }
            Event::AgentMessageDelta { text, .. } => {
                if let Some(last) = self.chat.last_mut() {
                    if last.kind == MsgKind::Agent {
                        last.text.push_str(&text);
                        return;
                    }
                }
                self.chat.push(ChatMsg {
                    kind: MsgKind::Agent,
                    text,
                });
            }
            Event::ReasoningDelta { text, .. } => {
                if self.detail_level == DetailLevel::Coding {
                    self.timeline.push(TimelineItem {
                        title: "Reasoning".to_string(),
                        detail: text,
                        state: TimelineState::Running,
                        request_id: None,
                    });
                }
            }
            Event::ApprovalRequested {
                request_id,
                tool,
                summary,
            } => {
                self.inspector = InspectorTab::Approvals;
                upsert_pending_approval(
                    &mut self.pending_approvals,
                    PendingApproval {
                        request_id,
                        tool: tool.clone(),
                        summary: summary.clone(),
                        created_ms: now_ms(),
                    },
                );
                self.timeline.push(TimelineItem {
                    title: format!("Approval: {tool}"),
                    detail: summary,
                    state: TimelineState::Waiting,
                    request_id: Some(request_id),
                });
            }
            Event::ToolCallBegin { tool, args, .. } => {
                if self.detail_level == DetailLevel::Coding {
                    self.timeline.push(TimelineItem {
                        title: format!("Tool: {tool}"),
                        detail: args.to_string(),
                        state: TimelineState::Running,
                        request_id: None,
                    });
                }
            }
            Event::ToolCallEnd {
                tool, output, ok, ..
            } => {
                if self.detail_level == DetailLevel::Coding || !ok {
                    self.timeline.push(TimelineItem {
                        title: format!("Tool finished: {tool}"),
                        detail: compact_tool_result(&output, self.fold_tool_results),
                        state: if ok {
                            TimelineState::Done
                        } else {
                            TimelineState::Error
                        },
                        request_id: None,
                    });
                }
            }
            Event::Todos { .. } => {}
            Event::PatchApplied { path, .. } => {
                self.inspector = InspectorTab::Diff;
                self.refresh_repo_index();
                self.timeline.push(TimelineItem {
                    title: "Patch applied".to_string(),
                    detail: path,
                    state: TimelineState::Done,
                    request_id: None,
                });
            }
            Event::CheckpointCreated { id, label, .. } => {
                upsert_checkpoint(
                    &mut self.checkpoints,
                    WorkspaceCheckpoint {
                        id,
                        label: label.clone(),
                        created_ms: now_ms(),
                        rewound: false,
                        restored_files: None,
                    },
                );
                self.timeline.push(TimelineItem {
                    title: format!("Checkpoint #{id}"),
                    detail: label,
                    state: TimelineState::Done,
                    request_id: None,
                });
            }
            Event::RewindDone { id, restored } => {
                mark_checkpoint_rewound(&mut self.checkpoints, id, restored);
                self.git_snapshot = git_workspace_snapshot(&self.workspace);
                self.refresh_repo_index();
                self.selected_git_file = None;
                self.selected_git_file_diff.clear();
                self.timeline.push(TimelineItem {
                    title: format!("Rewound #{id}"),
                    detail: format!("Restored {restored} file(s)"),
                    state: TimelineState::Done,
                    request_id: None,
                });
            }
            Event::Compacted { dropped, tokens } => {
                record_compaction(&mut self.compactions, dropped, tokens, now_ms());
                self.timeline.push(TimelineItem {
                    title: "Context compacted".to_string(),
                    detail: format!("Dropped {dropped} messages, ~{tokens} tokens remain"),
                    state: TimelineState::Done,
                    request_id: None,
                });
            }
            Event::TokensUsed {
                turn,
                input,
                output,
            } => {
                record_token_usage(&mut self.token_usage, turn.0, input, output, now_ms());
                self.timeline.push(TimelineItem {
                    title: "Token usage".to_string(),
                    detail: format!("Input {input}, output {output}"),
                    state: TimelineState::Done,
                    request_id: None,
                });
            }
            Event::ContextWindow { limit } => {
                self.context_window = Some(limit);
            }
            Event::HarnessChanged { id } => {
                self.cfg.harness = id.clone();
                self.timeline.push(TimelineItem {
                    title: "Harness changed".to_string(),
                    detail: id,
                    state: TimelineState::Done,
                    request_id: None,
                });
            }
            Event::McpServerStatus {
                name,
                status,
                tool_count,
                tools,
                detail,
            } => {
                apply_mcp_status_event(
                    &mut self.mcp_health,
                    name.clone(),
                    status.clone(),
                    tool_count,
                    tools,
                    detail.clone(),
                );
                self.timeline.push(TimelineItem {
                    title: format!("MCP {status}: {name}"),
                    detail,
                    state: if status == "connected" {
                        TimelineState::Done
                    } else {
                        TimelineState::Error
                    },
                    request_id: None,
                });
            }
            Event::BrowserTargetChanged { url, note, .. } => {
                self.browser_target_url = url.clone();
                self.browser_action_note = if note.trim().is_empty() {
                    "Agent requested browser target".to_string()
                } else {
                    note.clone()
                };
                self.record_browser_action("agent-open-target");
                self.inspector = InspectorTab::Browser;
                self.timeline.push(TimelineItem {
                    title: "Browser target requested".to_string(),
                    detail: url,
                    state: TimelineState::Waiting,
                    request_id: None,
                });
            }
            Event::BrowserSnapshotRequested { url, note, .. } => {
                self.browser_target_url = url.clone();
                let snapshot_note = if note.trim().is_empty() {
                    "Agent requested browser snapshot".to_string()
                } else {
                    note.clone()
                };
                self.browser_action_note = snapshot_note.clone();
                let browser_action = self.record_browser_action("agent-snapshot-request");
                let browser_action_id = browser_action
                    .as_ref()
                    .map(|action| action.id.clone())
                    .unwrap_or_else(|| latest_browser_action_id(&self.browser_actions));
                match browser_snapshot_appshot_draft(
                    &self.workspace,
                    &url,
                    &snapshot_note,
                    &browser_action_id,
                    now_ms(),
                ) {
                    Ok(draft) => {
                        self.appshot_title = draft.title.clone();
                        self.appshot_path = draft.path.clone();
                        self.appshot_note = draft.note.clone();
                        self.browser_message = format!("Snapshot draft ready: {}", draft.title);
                        self.pending_browser_snapshot = Some(draft);
                    }
                    Err(e) => {
                        self.browser_message = e.to_string();
                    }
                }
                self.inspector = InspectorTab::Browser;
                self.timeline.push(TimelineItem {
                    title: "Browser snapshot requested".to_string(),
                    detail: format!("{} · {}", url, empty_label(&note)),
                    state: TimelineState::Waiting,
                    request_id: None,
                });
            }
            Event::Info { text } => {
                if !text.starts_with("session") {
                    self.chat.push(ChatMsg {
                        kind: MsgKind::Note,
                        text,
                    });
                }
            }
            Event::AuditLog {
                kind,
                title,
                detail,
                status,
                ..
            } => {
                let state = match status.as_str() {
                    "failed" | "blocked" | "interrupted" => TimelineState::Error,
                    "running" => TimelineState::Running,
                    _ => TimelineState::Done,
                };
                let detail = if detail.trim().is_empty() {
                    status
                } else {
                    format!("{status} · {detail}")
                };
                self.timeline.push(TimelineItem {
                    title: format!("{kind}: {title}"),
                    detail,
                    state,
                    request_id: None,
                });
            }
            Event::SubagentStarted { profile, task, .. } => {
                self.timeline.push(TimelineItem {
                    title: format!("Subagent: {profile}"),
                    detail: task,
                    state: TimelineState::Running,
                    request_id: None,
                });
            }
            Event::SubagentFinished {
                profile,
                summary,
                ok,
                ..
            } => {
                self.timeline.push(TimelineItem {
                    title: format!("Subagent finished: {profile}"),
                    detail: summary,
                    state: if ok {
                        TimelineState::Done
                    } else {
                        TimelineState::Error
                    },
                    request_id: None,
                });
            }
            Event::Error { message } => {
                self.streaming = false;
                self.chat.push(ChatMsg {
                    kind: MsgKind::Note,
                    text: format!("error: {message}"),
                });
                self.timeline.push(TimelineItem {
                    title: "Error".to_string(),
                    detail: message,
                    state: TimelineState::Error,
                    request_id: None,
                });
            }
            Event::TurnFinished { .. } => {
                self.streaming = false;
                self.timeline.push(TimelineItem {
                    title: "Turn finished".to_string(),
                    detail: "Agent is idle".to_string(),
                    state: TimelineState::Done,
                    request_id: None,
                });
                if let Some(next) = self.queued_prompts.pop_front() {
                    self.timeline.push(TimelineItem {
                        title: "Queued prompt started".to_string(),
                        detail: truncate_title(&next),
                        state: TimelineState::Running,
                        request_id: None,
                    });
                    self.dispatch_user_prompt(next);
                }
            }
            Event::Shutdown => {
                self.streaming = false;
            }
            Event::CommandStarted {
                command,
                background,
                ..
            } => {
                self.timeline.push(TimelineItem {
                    title: if background {
                        "Background command".to_string()
                    } else {
                        "Command started".to_string()
                    },
                    detail: command,
                    state: TimelineState::Running,
                    request_id: None,
                });
            }
            Event::CommandOutput { chunk, .. } => {
                if !chunk.trim().is_empty() {
                    self.timeline.push(TimelineItem {
                        title: "Command output".to_string(),
                        detail: chunk.chars().take(400).collect(),
                        state: TimelineState::Running,
                        request_id: None,
                    });
                }
            }
            Event::CommandFinished { ok, exit_code, .. } => {
                self.timeline.push(TimelineItem {
                    title: if ok {
                        "Command finished".to_string()
                    } else {
                        "Command failed".to_string()
                    },
                    detail: exit_code
                        .map(|code| format!("exit {code}"))
                        .unwrap_or_else(|| "exit unknown".to_string()),
                    state: if ok {
                        TimelineState::Done
                    } else {
                        TimelineState::Error
                    },
                    request_id: None,
                });
            }
            Event::FileDiff { .. }
            | Event::HookFired { .. }
            | Event::QuestionAsked { .. }
            | Event::RateLimit { .. } => {}
        }
    }

    fn submit_prompt(&mut self) {
        let text = self.prompt.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.prompt.clear();
        if self.streaming {
            self.queued_prompts.push_back(text.clone());
            self.timeline.push(TimelineItem {
                title: "Prompt queued".to_string(),
                detail: truncate_title(&text),
                state: TimelineState::Waiting,
                request_id: None,
            });
            return;
        }
        self.dispatch_user_prompt(text);
    }

    fn dispatch_user_prompt(&mut self, text: String) {
        let session_text = if let Some(context) = self.pending_session_context.take() {
            self.timeline.push(TimelineItem {
                title: "Selected thread context attached".to_string(),
                detail: truncate_title(&context),
                state: TimelineState::Done,
                request_id: None,
            });
            build_prompt_with_session_context(&text, &context)
        } else {
            text.clone()
        };
        let personalized_text = build_prompt_with_personalization(
            &session_text,
            &self.personalization_tone,
            &self.custom_instructions,
        );
        let goal_text = build_prompt_with_goal_mode(
            &personalized_text,
            self.goal_mode_enabled,
            &self.active_goal,
            &self.goal_success_criteria,
        );
        let memory_text = if self.attach_memory_to_prompt {
            build_prompt_with_memory(&goal_text, &self.project_rules, &self.memories)
        } else {
            goal_text
        };
        let browser_text = build_prompt_with_browser_context(
            &memory_text,
            &self.browser_target_url,
            &self.browser_actions,
        );
        let engine_text = if self.attach_appshots_to_prompt {
            build_prompt_with_appshots(&browser_text, &self.appshots)
        } else {
            browser_text
        };
        self.chat.push(ChatMsg {
            kind: MsgKind::User,
            text: text.clone(),
        });
        self.chat.push(ChatMsg {
            kind: MsgKind::Agent,
            text: String::new(),
        });
        self.streaming = true;
        let _ = self
            .engine_tx
            .send(RuntimeCmd::Op(Op::UserTurn { text: engine_text }));
    }

    fn reconfigure(&mut self) {
        if self.streaming {
            self.timeline.push(TimelineItem {
                title: "Configuration deferred".to_string(),
                detail: "Finish or stop the active turn before changing runtime settings"
                    .to_string(),
                state: TimelineState::Waiting,
                request_id: None,
            });
            return;
        }
        self.workspace = workspace_of(&self.cfg);
        self.record_recent_workspace();
        self.mcp_health = configured_mcp_health(&self.cfg.mcp_servers);
        match save_project_config(&self.cfg) {
            Ok(()) => {
                self.timeline.push(TimelineItem {
                    title: "Settings saved".to_string(),
                    detail: self.workspace.join("oxide.toml").display().to_string(),
                    state: TimelineState::Done,
                    request_id: None,
                });
            }
            Err(e) => {
                self.timeline.push(TimelineItem {
                    title: "Settings save failed".to_string(),
                    detail: e.to_string(),
                    state: TimelineState::Error,
                    request_id: None,
                });
            }
        }
        self.refresh_workspace_views();
        let _ = self
            .engine_tx
            .send(RuntimeCmd::Reconfigure(self.cfg.clone()));
        self.timeline.push(TimelineItem {
            title: "Configuration applied".to_string(),
            detail: format!(
                "{} / {} / {}",
                self.cfg.provider,
                display_model(&self.cfg),
                effort_label(&self.cfg.reasoning_effort)
            ),
            state: TimelineState::Done,
            request_id: None,
        });
    }

    fn open_workspace_from_input(&mut self) {
        let input = self.workspace_input.trim().to_string();
        if input.is_empty() {
            self.workspace_message = "Workspace path is required".to_string();
            return;
        }
        self.switch_workspace(PathBuf::from(input));
    }

    fn switch_workspace(&mut self, path: PathBuf) {
        if self.active_terminal_job.is_some() {
            self.workspace_message =
                "Stop the running terminal command before switching".to_string();
            return;
        }
        let canonical = match std::fs::canonicalize(&path) {
            Ok(path) if path.is_dir() => path,
            Ok(path) => {
                self.workspace_message = format!("Not a directory: {}", path.display());
                return;
            }
            Err(e) => {
                self.workspace_message = format!("Cannot open workspace: {e}");
                return;
            }
        };
        self.cfg.workspace = Some(canonical.clone());
        self.workspace_input = canonical.display().to_string();
        self.workspace_message = format!("Opened {}", project_name(&canonical));
        self.chat.clear();
        self.selected_session = None;
        self.pending_session_context = None;
        self.rename_session_title.clear();
        self.selected_file = None;
        self.selected_file_text.clear();
        self.composer_file_query.clear();
        self.file_message.clear();
        self.nav = NavSurface::Chat;
        self.reconfigure();
        self.timeline.push(TimelineItem {
            title: "Workspace switched".to_string(),
            detail: canonical.display().to_string(),
            state: TimelineState::Done,
            request_id: None,
        });
    }

    fn attach_file_context_to_prompt(&mut self, path: &Path) {
        match read_file_context(path, FILE_CONTEXT_CHAR_LIMIT) {
            Ok(context) => {
                self.prompt = build_prompt_with_file_context(&self.prompt, path, &context);
                self.selected_file = Some(path.to_path_buf());
                self.selected_file_text = context;
                let label = relative_path_label(&self.workspace, path);
                self.file_message = format!("Inserted {label} into composer");
                self.inspector = InspectorTab::Files;
                self.timeline.push(TimelineItem {
                    title: "File context attached".to_string(),
                    detail: label,
                    state: TimelineState::Done,
                    request_id: None,
                });
            }
            Err(e) => {
                self.file_message = format!("Failed to attach file: {e}");
                self.timeline.push(TimelineItem {
                    title: "File context attach failed".to_string(),
                    detail: e.to_string(),
                    state: TimelineState::Error,
                    request_id: None,
                });
            }
        }
    }

    fn record_recent_workspace(&mut self) {
        self.recent_workspaces = upsert_recent_workspace(
            self.recent_workspaces.clone(),
            &self.workspace,
            &project_name(&self.workspace),
            now_ms(),
            12,
        );
        let mut state = read_desktop_state();
        state.recent_workspaces = self.recent_workspaces.clone();
        if let Err(e) = write_desktop_state(&state) {
            self.workspace_message = format!("Recent workspace save failed: {e}");
        }
    }

    fn desktop_preferences(&self) -> DesktopPreferences {
        DesktopPreferences {
            motion_enabled: self.motion_enabled,
            compact_sidebar: self.compact_sidebar,
            fold_tool_results: self.fold_tool_results,
            prevent_sleep: self.prevent_sleep,
            attach_memory_to_prompt: self.attach_memory_to_prompt,
            attach_appshots_to_prompt: self.attach_appshots_to_prompt,
            detail_level: detail_level_id(self.detail_level).to_string(),
            personalization_tone: personalization_tone_id(&self.personalization_tone).to_string(),
            custom_instructions: self.custom_instructions.clone(),
            goal_mode_enabled: self.goal_mode_enabled,
            active_goal: self.active_goal.clone(),
            goal_success_criteria: self.goal_success_criteria.clone(),
        }
    }

    fn persist_desktop_preferences(&mut self) {
        let mut state = read_desktop_state();
        state.recent_workspaces = self.recent_workspaces.clone();
        state.preferences = self.desktop_preferences();
        if let Err(e) = write_desktop_state(&state) {
            self.workspace_message = format!("Desktop settings save failed: {e}");
        }
    }

    fn refresh_workspace_views(&mut self) {
        self.refresh_repo_index();
        self.sessions = read_all_session_summaries(&self.workspace).unwrap_or_default();
        self.git_snapshot = git_workspace_snapshot(&self.workspace);
        self.git_branches = git_branch_snapshot(&self.workspace);
        self.automations = read_automation_specs(&self.workspace).unwrap_or_default();
        self.automation_runs = read_automation_run_specs(&self.workspace).unwrap_or_default();
        self.appshots = read_appshot_specs(&self.workspace).unwrap_or_default();
        self.browser_actions = read_browser_action_specs(&self.workspace).unwrap_or_default();
        self.memories = read_memory_specs(&self.workspace).unwrap_or_default();
        self.project_rules = read_project_rules(&self.workspace).unwrap_or_default();
        self.hermes_profiles = read_hermes_profiles(&self.workspace).unwrap_or_default();
        self.diagnostics = collect_diagnostics(&self.cfg, &self.workspace);
    }

    fn refresh_repo_index(&mut self) {
        self.repo_index = collect_repo_index(&self.workspace, REPO_INDEX_ENTRY_LIMIT);
    }

    fn toggle_fast_mode(&mut self) {
        self.cfg.fast_mode = !self.cfg.fast_mode;
        if self.cfg.fast_mode {
            if let Some(preset) = fast_model_for(&self.cfg.provider) {
                self.cfg.model = preset.model.to_string();
            }
            self.cfg.reasoning_effort = "low".to_string();
        } else if self.cfg.reasoning_effort == "low" {
            self.cfg.reasoning_effort = "medium".to_string();
        }
        self.reconfigure();
    }

    fn load_session(&mut self, session: &SessionSummary) {
        match load_session_chat(&session.path) {
            Ok(messages) => {
                let resume_context =
                    build_session_resume_context(session, &messages, SESSION_RESUME_CHAR_LIMIT);
                self.chat = messages;
                self.selected_session = Some(session.id.clone());
                self.pending_session_context = Some(resume_context);
                self.rename_session_title = session.title.clone();
                self.nav = NavSurface::Chat;
                self.timeline.push(TimelineItem {
                    title: "Session loaded for resume".to_string(),
                    detail: format!("{} will be attached to the next prompt", session.title),
                    state: TimelineState::Done,
                    request_id: None,
                });
            }
            Err(e) => {
                self.timeline.push(TimelineItem {
                    title: "Session load failed".to_string(),
                    detail: e.to_string(),
                    state: TimelineState::Error,
                    request_id: None,
                });
            }
        }
    }

    fn selected_session_summary(&self) -> Option<SessionSummary> {
        let id = self.selected_session.as_deref()?;
        self.sessions
            .iter()
            .find(|session| session.id == id)
            .cloned()
    }

    fn update_session_meta<F>(&mut self, session: &SessionSummary, update: F)
    where
        F: FnOnce(&mut SessionMeta),
    {
        let mut meta = session_meta_from_summary(session);
        update(&mut meta);
        meta.updated_ms = now_ms();
        match write_session_meta(&self.workspace, &meta) {
            Ok(()) => {
                self.refresh_workspace_views();
                self.timeline.push(TimelineItem {
                    title: "Thread updated".to_string(),
                    detail: session.id.clone(),
                    state: TimelineState::Done,
                    request_id: None,
                });
            }
            Err(e) => {
                self.timeline.push(TimelineItem {
                    title: "Thread update failed".to_string(),
                    detail: e.to_string(),
                    state: TimelineState::Error,
                    request_id: None,
                });
            }
        }
    }

    fn rename_selected_session(&mut self) {
        let Some(session) = self.selected_session_summary() else {
            return;
        };
        let title = self.rename_session_title.trim().to_string();
        if title.is_empty() {
            self.timeline.push(TimelineItem {
                title: "Thread rename failed".to_string(),
                detail: "Title is required".to_string(),
                state: TimelineState::Error,
                request_id: None,
            });
            return;
        }
        self.update_session_meta(&session, |meta| {
            meta.title = Some(title);
        });
    }

    fn toggle_selected_session_pin(&mut self) {
        let Some(session) = self.selected_session_summary() else {
            return;
        };
        self.update_session_meta(&session, |meta| {
            meta.pinned = !session.pinned;
        });
    }

    fn toggle_selected_session_archive(&mut self) {
        let Some(session) = self.selected_session_summary() else {
            return;
        };
        self.update_session_meta(&session, |meta| {
            meta.archived = !session.archived;
        });
    }

    fn start_evolve(&mut self) {
        let prompt = build_evolve_prompt(
            &self.evolve_goal,
            &self.evolve_validation,
            &self.git_snapshot.status,
        );
        self.cfg.harness = "hermes".to_string();
        let _ = self.engine_tx.send(RuntimeCmd::Op(Op::SetHarness {
            id: "hermes".to_string(),
        }));
        self.timeline.push(TimelineItem {
            title: "Hermes evolve started".to_string(),
            detail: self.evolve_goal.clone(),
            state: if self.streaming {
                TimelineState::Waiting
            } else {
                TimelineState::Running
            },
            request_id: None,
        });
        if self.streaming {
            self.queued_prompts.push_back(prompt);
        } else {
            self.nav = NavSurface::Chat;
            self.dispatch_user_prompt(prompt);
        }
    }

    fn save_hermes_profile(&mut self) {
        let created_ms = now_ms();
        let profile = match hermes_profile_from_fields(
            &self.hermes_profile_name,
            &self.evolve_goal,
            &self.evolve_validation,
            &self.hermes_review_prompt,
            created_ms,
        ) {
            Ok(profile) => profile,
            Err(e) => {
                self.hermes_message = e.to_string();
                self.timeline.push(TimelineItem {
                    title: "Hermes profile not saved".to_string(),
                    detail: self.hermes_message.clone(),
                    state: TimelineState::Error,
                    request_id: None,
                });
                return;
            }
        };
        match write_hermes_profile(&self.workspace, &profile) {
            Ok(()) => {
                self.hermes_profiles = read_hermes_profiles(&self.workspace).unwrap_or_default();
                self.hermes_message = format!("Saved profile {}", profile.name);
            }
            Err(e) => {
                self.hermes_message = e.to_string();
                self.timeline.push(TimelineItem {
                    title: "Hermes profile save failed".to_string(),
                    detail: self.hermes_message.clone(),
                    state: TimelineState::Error,
                    request_id: None,
                });
            }
        }
    }

    fn apply_hermes_profile(&mut self, profile: &HermesProfile) {
        self.hermes_profile_name = profile.name.clone();
        self.evolve_goal = profile.goal.clone();
        self.evolve_validation = profile.validation.clone();
        self.hermes_review_prompt = profile.review_prompt.clone();
        self.hermes_message = format!("Applied profile {}", profile.name);
    }

    fn run_hermes_profile(&mut self, profile: &HermesProfile) {
        self.apply_hermes_profile(profile);
        self.start_evolve();
    }

    fn queue_hermes_review(&mut self) {
        let prompt = build_hermes_review_prompt(
            &self.evolve_goal,
            &self.evolve_validation,
            &self.hermes_review_prompt,
        );
        if self.streaming {
            self.queued_prompts.push_back(prompt);
            self.timeline.push(TimelineItem {
                title: "Hermes review queued".to_string(),
                detail: self.evolve_goal.clone(),
                state: TimelineState::Waiting,
                request_id: None,
            });
        } else {
            self.nav = NavSurface::Chat;
            self.dispatch_user_prompt(prompt);
        }
    }

    fn delete_hermes_profile_ui(&mut self, profile: &HermesProfile) {
        match delete_hermes_profile(&self.workspace, &profile.id) {
            Ok(()) => {
                self.hermes_profiles = read_hermes_profiles(&self.workspace).unwrap_or_default();
                self.hermes_message = format!("Deleted profile {}", profile.name);
            }
            Err(e) => {
                self.hermes_message = e.to_string();
                self.timeline.push(TimelineItem {
                    title: "Hermes profile delete failed".to_string(),
                    detail: self.hermes_message.clone(),
                    state: TimelineState::Error,
                    request_id: None,
                });
            }
        }
    }

    fn create_automation(&mut self) {
        let name = self.automation_name.trim();
        let prompt = self.automation_prompt.trim();
        if name.is_empty() || prompt.is_empty() {
            self.timeline.push(TimelineItem {
                title: "Automation not created".to_string(),
                detail: "Name and prompt are required".to_string(),
                state: TimelineState::Error,
                request_id: None,
            });
            return;
        }
        let spec = AutomationSpec {
            id: automation_id(name),
            name: name.to_string(),
            kind: "cron".to_string(),
            status: "ACTIVE".to_string(),
            schedule: self.automation_schedule.trim().to_string(),
            prompt: prompt.to_string(),
            created_ms: now_ms(),
        };
        match write_automation_spec(&self.workspace, &spec) {
            Ok(()) => {
                self.automations = read_automation_specs(&self.workspace).unwrap_or_default();
                self.timeline.push(TimelineItem {
                    title: "Automation saved".to_string(),
                    detail: spec.name,
                    state: TimelineState::Done,
                    request_id: None,
                });
            }
            Err(e) => {
                self.timeline.push(TimelineItem {
                    title: "Automation save failed".to_string(),
                    detail: e.to_string(),
                    state: TimelineState::Error,
                    request_id: None,
                });
            }
        }
    }

    fn run_automation_now(&mut self, spec: &AutomationSpec) {
        self.run_automation(spec, "manual");
    }

    fn run_automation(&mut self, spec: &AutomationSpec, trigger: &str) {
        let run = automation_run_from_spec(spec, trigger, "queued", now_ms());
        match write_automation_run_spec(&self.workspace, &run) {
            Ok(()) => {
                self.automation_runs =
                    read_automation_run_specs(&self.workspace).unwrap_or_default();
            }
            Err(e) => self.timeline.push(TimelineItem {
                title: "Automation run history failed".to_string(),
                detail: e.to_string(),
                state: TimelineState::Error,
                request_id: None,
            }),
        }
        let prompt = build_automation_run_prompt(spec);
        if self.streaming {
            self.queued_prompts.push_back(prompt);
            self.timeline.push(TimelineItem {
                title: format!("Automation queued ({trigger})"),
                detail: spec.name.clone(),
                state: TimelineState::Waiting,
                request_id: None,
            });
        } else {
            self.nav = NavSurface::Chat;
            self.dispatch_user_prompt(prompt);
        }
    }

    fn tick_automations(&mut self) {
        let now = now_ms();
        if now.saturating_sub(self.last_automation_tick_ms) < AUTOMATION_TICK_INTERVAL_MS {
            return;
        }
        self.last_automation_tick_ms = now;
        let due = self
            .automations
            .iter()
            .filter(|spec| automation_is_due(spec, &self.automation_runs, now))
            .cloned()
            .collect::<Vec<_>>();
        for spec in due {
            self.run_automation(&spec, "scheduled");
        }
    }

    fn toggle_automation_status(&mut self, spec: &AutomationSpec) {
        let next = automation_with_toggled_status(spec);
        match write_automation_spec(&self.workspace, &next) {
            Ok(()) => {
                self.automations = read_automation_specs(&self.workspace).unwrap_or_default();
                self.timeline.push(TimelineItem {
                    title: "Automation updated".to_string(),
                    detail: format!("{} -> {}", next.name, next.status),
                    state: TimelineState::Done,
                    request_id: None,
                });
            }
            Err(e) => self.timeline.push(TimelineItem {
                title: "Automation update failed".to_string(),
                detail: e.to_string(),
                state: TimelineState::Error,
                request_id: None,
            }),
        }
    }

    fn delete_automation(&mut self, spec: &AutomationSpec) {
        match delete_automation_spec(&self.workspace, &spec.id) {
            Ok(()) => {
                self.automations = read_automation_specs(&self.workspace).unwrap_or_default();
                self.timeline.push(TimelineItem {
                    title: "Automation deleted".to_string(),
                    detail: spec.name.clone(),
                    state: TimelineState::Done,
                    request_id: None,
                });
            }
            Err(e) => self.timeline.push(TimelineItem {
                title: "Automation delete failed".to_string(),
                detail: e.to_string(),
                state: TimelineState::Error,
                request_id: None,
            }),
        }
    }

    fn save_mcp_server_from_form(&mut self) {
        let args = match parse_mcp_args_input(&self.mcp_args) {
            Ok(args) => args,
            Err(e) => {
                self.mcp_message = e.to_string();
                self.timeline.push(TimelineItem {
                    title: "MCP server not saved".to_string(),
                    detail: self.mcp_message.clone(),
                    state: TimelineState::Error,
                    request_id: None,
                });
                return;
            }
        };
        let server = McpServerConfig {
            name: self.mcp_name.trim().to_string(),
            command: self.mcp_command.trim().to_string(),
            args,
            ..McpServerConfig::default()
        };
        if let Err(e) = validate_mcp_server(&server) {
            self.mcp_message = e.to_string();
            self.timeline.push(TimelineItem {
                title: "MCP server not saved".to_string(),
                detail: self.mcp_message.clone(),
                state: TimelineState::Error,
                request_id: None,
            });
            return;
        }
        upsert_mcp_server(&mut self.cfg, server.clone());
        self.mcp_message = format!("Saved MCP server {}", server.name);
        self.reconfigure();
    }

    fn edit_mcp_server(&mut self, server: &McpServerConfig) {
        self.mcp_name = server.name.clone();
        self.mcp_command = server.command.clone();
        self.mcp_args = server.args.join(" ");
        self.settings_tab = SettingsTab::Plugins;
        self.nav = NavSurface::Settings;
    }

    fn delete_mcp_server(&mut self, name: &str) {
        remove_mcp_server(&mut self.cfg, name);
        self.mcp_message = format!("Removed MCP server {name}");
        self.reconfigure();
    }

    fn create_memory_note(&mut self) {
        let title = self.memory_title.trim();
        let body = self.memory_body.trim();
        if title.is_empty() || body.is_empty() {
            self.memory_message = "Title and body are required".to_string();
            self.timeline.push(TimelineItem {
                title: "Memory not saved".to_string(),
                detail: self.memory_message.clone(),
                state: TimelineState::Error,
                request_id: None,
            });
            return;
        }
        let spec = MemorySpec {
            id: memory_id(title),
            title: title.to_string(),
            body: body.to_string(),
            enabled: true,
            created_ms: now_ms(),
        };
        match write_memory_spec(&self.workspace, &spec) {
            Ok(()) => {
                self.memories = read_memory_specs(&self.workspace).unwrap_or_default();
                self.memory_title.clear();
                self.memory_body.clear();
                self.memory_message = format!("Saved memory {}", spec.title);
                self.timeline.push(TimelineItem {
                    title: "Memory saved".to_string(),
                    detail: spec.title,
                    state: TimelineState::Done,
                    request_id: None,
                });
            }
            Err(e) => {
                self.memory_message = e.to_string();
                self.timeline.push(TimelineItem {
                    title: "Memory save failed".to_string(),
                    detail: self.memory_message.clone(),
                    state: TimelineState::Error,
                    request_id: None,
                });
            }
        }
    }

    fn toggle_memory_note(&mut self, memory: &MemorySpec) {
        let mut next = memory.clone();
        next.enabled = !next.enabled;
        match write_memory_spec(&self.workspace, &next) {
            Ok(()) => {
                self.memories = read_memory_specs(&self.workspace).unwrap_or_default();
            }
            Err(e) => {
                self.timeline.push(TimelineItem {
                    title: "Memory update failed".to_string(),
                    detail: e.to_string(),
                    state: TimelineState::Error,
                    request_id: None,
                });
            }
        }
    }

    fn delete_memory_note(&mut self, memory: &MemorySpec) {
        match delete_memory_spec(&self.workspace, &memory.id) {
            Ok(()) => {
                self.memories = read_memory_specs(&self.workspace).unwrap_or_default();
                self.timeline.push(TimelineItem {
                    title: "Memory deleted".to_string(),
                    detail: memory.title.clone(),
                    state: TimelineState::Done,
                    request_id: None,
                });
            }
            Err(e) => {
                self.timeline.push(TimelineItem {
                    title: "Memory delete failed".to_string(),
                    detail: e.to_string(),
                    state: TimelineState::Error,
                    request_id: None,
                });
            }
        }
    }

    fn create_appshot(&mut self) {
        let title = self.appshot_title.trim();
        let path = self.appshot_path.trim();
        if title.is_empty() || path.is_empty() {
            self.timeline.push(TimelineItem {
                title: "Appshot not saved".to_string(),
                detail: "Title and local file path are required".to_string(),
                state: TimelineState::Error,
                request_id: None,
            });
            return;
        }
        let file_path = PathBuf::from(path);
        if !file_path.is_file() {
            self.timeline.push(TimelineItem {
                title: "Appshot not saved".to_string(),
                detail: format!("File does not exist: {path}"),
                state: TimelineState::Error,
                request_id: None,
            });
            return;
        }
        let spec = appshot_with_browser_source(
            AppshotSpec {
                id: appshot_id(title),
                title: title.to_string(),
                path: path.to_string(),
                note: self.appshot_note.trim().to_string(),
                annotations: Vec::new(),
                source_url: String::new(),
                browser_action_id: String::new(),
                created_ms: now_ms(),
            },
            &self.browser_target_url,
            &latest_browser_action_id(&self.browser_actions),
        );
        match write_appshot_spec(&self.workspace, &spec) {
            Ok(()) => {
                self.appshots = read_appshot_specs(&self.workspace).unwrap_or_default();
                self.appshot_title.clear();
                self.appshot_path.clear();
                self.appshot_note.clear();
                self.timeline.push(TimelineItem {
                    title: "Appshot saved".to_string(),
                    detail: spec.title,
                    state: TimelineState::Done,
                    request_id: None,
                });
            }
            Err(e) => {
                self.timeline.push(TimelineItem {
                    title: "Appshot save failed".to_string(),
                    detail: e.to_string(),
                    state: TimelineState::Error,
                    request_id: None,
                });
            }
        }
    }

    fn capture_screen_appshot(&mut self) {
        let title = if self.appshot_title.trim().is_empty() {
            "Screen capture"
        } else {
            self.appshot_title.trim()
        };
        let created_ms = now_ms();
        let path = appshot_capture_path_for(&self.workspace, title, created_ms);
        match run_screen_capture(&path) {
            Ok(()) => {
                let spec = appshot_with_browser_source(
                    captured_appshot_spec(title, &path, self.appshot_note.trim(), created_ms),
                    &self.browser_target_url,
                    &latest_browser_action_id(&self.browser_actions),
                );
                match write_appshot_spec(&self.workspace, &spec) {
                    Ok(()) => {
                        self.appshots = read_appshot_specs(&self.workspace).unwrap_or_default();
                        self.appshot_title.clear();
                        self.appshot_path.clear();
                        self.appshot_note.clear();
                        self.timeline.push(TimelineItem {
                            title: "Screen captured".to_string(),
                            detail: spec.path,
                            state: TimelineState::Done,
                            request_id: None,
                        });
                    }
                    Err(e) => {
                        self.timeline.push(TimelineItem {
                            title: "Screen capture metadata failed".to_string(),
                            detail: e.to_string(),
                            state: TimelineState::Error,
                            request_id: None,
                        });
                    }
                }
            }
            Err(e) => {
                self.timeline.push(TimelineItem {
                    title: "Screen capture failed".to_string(),
                    detail: e,
                    state: TimelineState::Error,
                    request_id: None,
                });
            }
        }
    }

    fn capture_pending_browser_snapshot(&mut self) {
        let Some(draft) = self.pending_browser_snapshot.clone() else {
            self.browser_message = "No pending browser snapshot request".to_string();
            self.timeline.push(TimelineItem {
                title: "Pending browser snapshot missing".to_string(),
                detail: self.browser_message.clone(),
                state: TimelineState::Error,
                request_id: None,
            });
            return;
        };
        let path = PathBuf::from(&draft.path);
        match run_screen_capture(&path) {
            Ok(()) => {
                let spec = AppshotSpec {
                    path: path.display().to_string(),
                    created_ms: now_ms(),
                    ..draft
                };
                match write_appshot_spec(&self.workspace, &spec) {
                    Ok(()) => {
                        self.appshots = read_appshot_specs(&self.workspace).unwrap_or_default();
                        self.appshot_title.clear();
                        self.appshot_path.clear();
                        self.appshot_note.clear();
                        self.pending_browser_snapshot = None;
                        self.browser_message = format!("Captured pending snapshot {}", spec.title);
                        self.timeline.push(TimelineItem {
                            title: "Pending browser snapshot captured".to_string(),
                            detail: spec.path,
                            state: TimelineState::Done,
                            request_id: None,
                        });
                    }
                    Err(e) => {
                        self.browser_message = e.to_string();
                        self.timeline.push(TimelineItem {
                            title: "Pending browser snapshot metadata failed".to_string(),
                            detail: self.browser_message.clone(),
                            state: TimelineState::Error,
                            request_id: None,
                        });
                    }
                }
            }
            Err(e) => {
                self.browser_message = e.clone();
                self.timeline.push(TimelineItem {
                    title: "Pending browser snapshot failed".to_string(),
                    detail: e,
                    state: TimelineState::Error,
                    request_id: None,
                });
            }
        }
    }

    fn dismiss_pending_browser_snapshot(&mut self) {
        let Some(draft) = self.pending_browser_snapshot.take() else {
            self.browser_message = "No pending browser snapshot request".to_string();
            return;
        };
        if self.appshot_path == draft.path {
            self.appshot_title.clear();
            self.appshot_path.clear();
            self.appshot_note.clear();
        }
        self.browser_message = format!("Dismissed pending snapshot {}", draft.title);
        self.timeline.push(TimelineItem {
            title: "Pending browser snapshot dismissed".to_string(),
            detail: draft.source_url,
            state: TimelineState::Done,
            request_id: None,
        });
    }

    fn add_appshot_annotation(&mut self, appshot: &AppshotSpec) {
        match appshot_with_added_annotation(
            appshot,
            &self.appshot_annotation_label,
            &self.appshot_annotation_target,
            &self.appshot_annotation_note,
        ) {
            Ok(next) => match write_appshot_spec(&self.workspace, &next) {
                Ok(()) => {
                    self.appshots = read_appshot_specs(&self.workspace).unwrap_or_default();
                    self.appshot_annotation_label =
                        next_appshot_annotation_label(next.annotations.len());
                    self.appshot_annotation_target.clear();
                    self.appshot_annotation_note.clear();
                    self.timeline.push(TimelineItem {
                        title: "Appshot annotation saved".to_string(),
                        detail: next.title,
                        state: TimelineState::Done,
                        request_id: None,
                    });
                }
                Err(e) => {
                    self.timeline.push(TimelineItem {
                        title: "Appshot annotation failed".to_string(),
                        detail: e.to_string(),
                        state: TimelineState::Error,
                        request_id: None,
                    });
                }
            },
            Err(e) => {
                self.timeline.push(TimelineItem {
                    title: "Appshot annotation failed".to_string(),
                    detail: e.to_string(),
                    state: TimelineState::Error,
                    request_id: None,
                });
            }
        }
    }

    fn create_git_worktree(&mut self) {
        let branch = self.worktree_branch.trim();
        if branch.is_empty() {
            self.git_operation_message = "Branch name is required".to_string();
            self.timeline.push(TimelineItem {
                title: "Worktree not created".to_string(),
                detail: self.git_operation_message.clone(),
                state: TimelineState::Error,
                request_id: None,
            });
            return;
        }
        let target = worktree_path_for(&self.workspace, branch);
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.workspace)
            .arg("worktree")
            .arg("add")
            .arg(&target)
            .arg(branch)
            .output();
        match output {
            Ok(out) if out.status.success() => {
                let mut detail = String::from_utf8_lossy(&out.stdout).to_string();
                detail.push_str(&String::from_utf8_lossy(&out.stderr));
                if detail.trim().is_empty() {
                    detail = target.display().to_string();
                }
                self.git_operation_message = detail.trim().to_string();
                self.refresh_workspace_views();
                self.timeline.push(TimelineItem {
                    title: "Worktree created".to_string(),
                    detail: self.git_operation_message.clone(),
                    state: TimelineState::Done,
                    request_id: None,
                });
            }
            Ok(out) => {
                let mut detail = String::from_utf8_lossy(&out.stdout).to_string();
                detail.push_str(&String::from_utf8_lossy(&out.stderr));
                self.git_operation_message = if detail.trim().is_empty() {
                    format!("git exited with {}", out.status.code().unwrap_or(-1))
                } else {
                    detail.trim().to_string()
                };
                self.timeline.push(TimelineItem {
                    title: "Worktree create failed".to_string(),
                    detail: self.git_operation_message.clone(),
                    state: TimelineState::Error,
                    request_id: None,
                });
            }
            Err(e) => {
                self.git_operation_message = format!("git failed: {e}");
                self.timeline.push(TimelineItem {
                    title: "Worktree create failed".to_string(),
                    detail: self.git_operation_message.clone(),
                    state: TimelineState::Error,
                    request_id: None,
                });
            }
        }
    }

    fn refresh_git_review(&mut self) {
        self.git_snapshot = git_workspace_snapshot(&self.workspace);
        self.git_branches = git_branch_snapshot(&self.workspace);
        if self.git_push_branch.trim().is_empty() {
            self.git_push_branch = self.git_branches.current_branch.clone();
        }
        self.selected_git_file = None;
        self.selected_git_file_diff.clear();
    }

    fn stage_selected_git_file(&mut self) {
        let Some(file) = self.selected_git_file.clone() else {
            self.git_review_message = "Select a changed file first".to_string();
            return;
        };
        match stage_git_file(&self.workspace, &file) {
            Ok(output) => {
                self.git_review_message = if output.trim().is_empty() {
                    format!("Staged {}", file.display_path)
                } else {
                    output.trim().to_string()
                };
                self.refresh_git_review();
                self.timeline.push(TimelineItem {
                    title: "Git staged file".to_string(),
                    detail: file.display_path,
                    state: TimelineState::Done,
                    request_id: None,
                });
            }
            Err(e) => {
                self.git_review_message = e.clone();
                self.timeline.push(TimelineItem {
                    title: "Git stage failed".to_string(),
                    detail: e,
                    state: TimelineState::Error,
                    request_id: None,
                });
            }
        }
    }

    fn unstage_selected_git_file(&mut self) {
        let Some(file) = self.selected_git_file.clone() else {
            self.git_review_message = "Select a changed file first".to_string();
            return;
        };
        match unstage_git_file(&self.workspace, &file) {
            Ok(output) => {
                self.git_review_message = if output.trim().is_empty() {
                    format!("Unstaged {}", file.display_path)
                } else {
                    output.trim().to_string()
                };
                self.refresh_git_review();
                self.timeline.push(TimelineItem {
                    title: "Git unstaged file".to_string(),
                    detail: file.display_path,
                    state: TimelineState::Done,
                    request_id: None,
                });
            }
            Err(e) => {
                self.git_review_message = e.clone();
                self.timeline.push(TimelineItem {
                    title: "Git unstage failed".to_string(),
                    detail: e,
                    state: TimelineState::Error,
                    request_id: None,
                });
            }
        }
    }

    fn commit_staged_from_ui(&mut self) {
        match commit_staged_changes(&self.workspace, &self.git_commit_message) {
            Ok(output) => {
                self.git_review_message = if output.trim().is_empty() {
                    "Committed staged changes".to_string()
                } else {
                    output.trim().to_string()
                };
                self.git_commit_message.clear();
                self.refresh_git_review();
                self.timeline.push(TimelineItem {
                    title: "Git commit created".to_string(),
                    detail: self.git_review_message.clone(),
                    state: TimelineState::Done,
                    request_id: None,
                });
            }
            Err(e) => {
                self.git_review_message = e.clone();
                self.timeline.push(TimelineItem {
                    title: "Git commit failed".to_string(),
                    detail: e,
                    state: TimelineState::Error,
                    request_id: None,
                });
            }
        }
    }

    fn selected_git_publish_branch(&self) -> String {
        let branch = self.git_push_branch.trim();
        if branch.is_empty() {
            self.git_branches.current_branch.trim().to_string()
        } else {
            branch.to_string()
        }
    }

    fn push_current_branch_from_ui(&mut self) {
        let branch = self.selected_git_publish_branch();
        match push_git_branch(&self.workspace, &self.git_push_remote, &branch) {
            Ok(output) => {
                self.git_review_message = if output.trim().is_empty() {
                    format!("Pushed {branch}")
                } else {
                    output.trim().to_string()
                };
                self.refresh_git_review();
                self.timeline.push(TimelineItem {
                    title: "Git branch pushed".to_string(),
                    detail: self.git_review_message.clone(),
                    state: TimelineState::Done,
                    request_id: None,
                });
            }
            Err(e) => {
                self.git_review_message = e.clone();
                self.timeline.push(TimelineItem {
                    title: "Git push failed".to_string(),
                    detail: e,
                    state: TimelineState::Error,
                    request_id: None,
                });
            }
        }
    }

    fn copy_git_pr_draft_from_ui(&mut self, ui: &mut Ui) {
        let branch = self.selected_git_publish_branch();
        let summary = build_git_review_summary(&self.git_snapshot);
        let draft = build_git_pr_draft(
            &self.git_pr_title,
            &self.git_pr_body,
            &branch,
            &self.git_pr_base,
            &summary,
        );
        ui.output_mut(|output| {
            output.copied_text = draft;
        });
        self.git_review_message = "Copied PR draft to clipboard".to_string();
        self.timeline.push(TimelineItem {
            title: "Git PR draft copied".to_string(),
            detail: format!(
                "{} -> {}",
                empty_label(&branch),
                empty_label(&self.git_pr_base)
            ),
            state: TimelineState::Done,
            request_id: None,
        });
    }

    fn copy_github_compare_url_from_ui(&mut self, ui: &mut Ui) {
        let branch = self.selected_git_publish_branch();
        match github_compare_url_for_remote(
            &self.workspace,
            &self.git_push_remote,
            &branch,
            &self.git_pr_base,
        ) {
            Ok(url) => {
                ui.output_mut(|output| {
                    output.copied_text = url.clone();
                });
                self.git_review_message = format!("Copied compare URL: {url}");
                self.timeline.push(TimelineItem {
                    title: "Git compare URL copied".to_string(),
                    detail: url,
                    state: TimelineState::Done,
                    request_id: None,
                });
            }
            Err(e) => {
                self.git_review_message = e.clone();
                self.timeline.push(TimelineItem {
                    title: "Git compare URL failed".to_string(),
                    detail: e,
                    state: TimelineState::Error,
                    request_id: None,
                });
            }
        }
    }

    fn run_terminal_command(&mut self) {
        let cmd = self.terminal_input.trim().to_string();
        if cmd.is_empty() {
            return;
        }
        if let Some(job) = &self.active_terminal_job {
            self.terminal_lines
                .push_back(format!("terminal busy: '{}' is still running", job.command));
            trim_terminal(&mut self.terminal_lines);
            return;
        }
        self.terminal_input.clear();
        self.terminal_lines.push_back(format!("$ {cmd}"));
        trim_terminal(&mut self.terminal_lines);
        let tx = self.term_tx.clone();
        let cwd = self.workspace.clone();
        let mut child = match std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&cmd)
            .current_dir(cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                self.terminal_lines.push_back(format!("error: {e}"));
                trim_terminal(&mut self.terminal_lines);
                return;
            }
        };
        let id = now_ms();
        if let Some(stdout) = child.stdout.take() {
            spawn_terminal_reader(stdout, TerminalStream::Stdout, tx.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_terminal_reader(stderr, TerminalStream::Stderr, tx.clone());
        }
        let child = Arc::new(Mutex::new(child));
        spawn_terminal_watcher(id, child.clone(), tx);
        self.active_terminal_job = Some(TerminalJob {
            id,
            command: cmd,
            child,
            stopping: false,
        });
    }

    fn stop_terminal_command(&mut self) {
        let Some(job) = self.active_terminal_job.as_mut() else {
            self.terminal_lines
                .push_back("no terminal process is running".to_string());
            trim_terminal(&mut self.terminal_lines);
            return;
        };
        if job.stopping {
            self.terminal_lines
                .push_back("terminal stop already requested".to_string());
            trim_terminal(&mut self.terminal_lines);
            return;
        }
        job.stopping = true;
        let result = job
            .child
            .lock()
            .map_err(|e| format!("terminal lock failed: {e}"))
            .and_then(|mut child| child.kill().map_err(|e| format!("kill failed: {e}")));
        match result {
            Ok(()) => {
                self.terminal_lines
                    .push_back(format!("stopping: {}", job.command));
                self.timeline.push(TimelineItem {
                    title: "Terminal stop requested".to_string(),
                    detail: job.command.clone(),
                    state: TimelineState::Waiting,
                    request_id: None,
                });
            }
            Err(e) => {
                self.terminal_lines.push_back(e.clone());
                self.timeline.push(TimelineItem {
                    title: "Terminal stop failed".to_string(),
                    detail: e,
                    state: TimelineState::Error,
                    request_id: None,
                });
            }
        }
        trim_terminal(&mut self.terminal_lines);
    }
}

impl eframe::App for OxideDesktop {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.poll_runtime();
        self.tick_automations();
        if ctx.input(|i| i.key_pressed(Key::K) && i.modifiers.command) {
            self.show_palette = true;
        }
        if ctx.input(|i| i.key_pressed(Key::Comma) && i.modifiers.command) {
            self.show_settings = true;
            self.nav = NavSurface::Settings;
        }

        if self.motion_enabled || self.streaming || self.active_terminal_job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        ctx.request_repaint_after(Duration::from_millis(AUTOMATION_TICK_INTERVAL_MS));

        TopBottomPanel::top("title_bar")
            .frame(panel_frame())
            .show(ctx, |ui| self.render_top_bar(ui));
        SidePanel::left("sidebar")
            .resizable(false)
            .exact_width(if self.compact_sidebar { 72.0 } else { 258.0 })
            .frame(panel_frame())
            .show(ctx, |ui| self.render_sidebar(ui));
        SidePanel::right("inspector")
            .resizable(true)
            .default_width(360.0)
            .width_range(300.0..=520.0)
            .frame(panel_frame())
            .show(ctx, |ui| self.render_inspector(ui));
        CentralPanel::default()
            .frame(Frame::default().fill(BG))
            .show(ctx, |ui| self.render_center(ui));

        if self.show_palette {
            self.render_command_palette(ctx);
        }
        if self.show_settings {
            self.render_settings_window(ctx);
        }
    }
}

impl Drop for OxideDesktop {
    fn drop(&mut self) {
        if let Some(mut child) = self.sleep_guard.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl OxideDesktop {
    fn render_top_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("Oxide").strong().size(18.0).color(TEXT));
            ui.label(RichText::new("Rust-native command center").color(MUTED));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Settings").clicked() {
                    self.show_settings = true;
                    self.nav = NavSurface::Settings;
                }
                if ui.button("Palette").clicked() {
                    self.show_palette = true;
                }
                if self.goal_mode_enabled && !self.active_goal.trim().is_empty() {
                    let fill = timeline_state_fill(
                        TimelineState::Running,
                        self.last_tick.elapsed().as_secs_f32(),
                        self.motion_enabled,
                    );
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(goal_chip_text(&self.active_goal)).color(TEXT),
                            )
                            .fill(fill),
                        )
                        .clicked()
                    {
                        self.inspector = InspectorTab::Goal;
                    }
                }
                ui.label(RichText::new(status_line(&self.cfg, self.context_window)).color(MUTED));
            });
        });
    }

    fn render_sidebar(&mut self, ui: &mut Ui) {
        ui.vertical(|ui| {
            ui.add_space(8.0);
            if nav_button(ui, "New chat", self.nav == NavSurface::Chat).clicked() {
                self.nav = NavSurface::Chat;
                self.chat.clear();
                self.selected_session = None;
                self.pending_session_context = None;
                self.rename_session_title.clear();
            }
            if nav_button(ui, "Search", self.nav == NavSurface::Search).clicked() {
                self.nav = NavSurface::Search;
            }
            if nav_button(ui, "Plugins", self.nav == NavSurface::Plugins).clicked() {
                self.nav = NavSurface::Plugins;
            }
            if nav_button(ui, "Automations", self.nav == NavSurface::Automations).clicked() {
                self.nav = NavSurface::Automations;
            }
            if nav_button(ui, "Hermes", self.nav == NavSurface::Hermes).clicked() {
                self.nav = NavSurface::Hermes;
            }
            ui.separator();
            ui.label(section_text("Projects"));
            let current_path = self.workspace.display().to_string();
            let recent_workspaces = self.recent_workspaces.clone();
            for workspace in recent_workspaces.iter().take(8) {
                let selected = workspace.path == current_path;
                let label = format!("{}  ·  {}", workspace.name, workspace.path);
                if project_row(ui, &label, selected).clicked() && !selected {
                    self.switch_workspace(PathBuf::from(&workspace.path));
                }
            }
            ui.add(
                TextEdit::singleline(&mut self.workspace_input)
                    .hint_text("Workspace path")
                    .desired_width(f32::INFINITY),
            );
            ui.horizontal(|ui| {
                if ui.button("Open").clicked() {
                    self.open_workspace_from_input();
                }
                if ui.button("Use current").clicked() {
                    self.workspace_input = self.workspace.display().to_string();
                }
            });
            if !self.workspace_message.trim().is_empty() {
                ui.label(
                    RichText::new(&self.workspace_message)
                        .color(MUTED)
                        .size(12.0),
                );
            }
            ui.separator();
            ui.label(section_text("Threads"));
            ui.add(TextEdit::singleline(&mut self.session_query).hint_text("Search threads"));
            ui.horizontal(|ui| {
                if ui.button("Refresh").clicked() {
                    self.refresh_workspace_views();
                }
                if toggle_pill(ui, "Archived", self.show_archived_sessions, ACCENT).clicked() {
                    self.show_archived_sessions = !self.show_archived_sessions;
                }
            });
            let sessions = filter_sessions(
                &self.sessions,
                &self.session_query,
                self.show_archived_sessions,
            );
            for session in sessions.iter().take(12) {
                let selected = self.selected_session.as_deref() == Some(session.id.as_str());
                let prefix = if session.pinned { "pin  " } else { "" };
                let label = format!("{prefix}{}  ({})", session.title, session.message_count);
                if thread_button(ui, &label, selected).clicked() {
                    self.load_session(session);
                }
                if selected {
                    ui.horizontal(|ui| {
                        let pin_label = if session.pinned { "Unpin" } else { "Pin" };
                        if ui.button(pin_label).clicked() {
                            self.toggle_selected_session_pin();
                        }
                        let archive_label = if session.archived {
                            "Restore"
                        } else {
                            "Archive"
                        };
                        if ui.button(archive_label).clicked() {
                            self.toggle_selected_session_archive();
                        }
                    });
                }
            }
            if sessions.is_empty() {
                ui.label(
                    RichText::new("No matching sessions")
                        .color(FAINT)
                        .size(12.0),
                );
            }
            ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                if nav_button(ui, "Settings", self.nav == NavSurface::Settings).clicked() {
                    self.nav = NavSurface::Settings;
                    self.show_settings = true;
                }
            });
        });
    }

    fn render_center(&mut self, ui: &mut Ui) {
        match self.nav {
            NavSurface::Chat => self.render_chat(ui),
            NavSurface::Search => self.render_search(ui),
            NavSurface::Plugins => self.render_plugins(ui),
            NavSurface::Automations => self.render_automations(ui),
            NavSurface::Hermes => self.render_hermes(ui),
            NavSurface::Settings => self.render_settings_page(ui),
        }
    }

    fn render_chat(&mut self, ui: &mut Ui) {
        ui.vertical_centered_justified(|ui| {
            if self.chat.is_empty() {
                ui.add_space(78.0);
                ui.label(
                    RichText::new(format!(
                        "What should we build in {}?",
                        project_name(&self.workspace)
                    ))
                    .size(30.0)
                    .color(TEXT),
                );
                ui.add_space(10.0);
                self.render_composer(ui);
                ui.add_space(12.0);
                quick_actions(ui, &mut self.prompt);
            } else {
                ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(20.0);
                        let t = self.last_tick.elapsed().as_secs_f32();
                        let streaming_tail_index = if self.streaming {
                            self.chat
                                .iter()
                                .rposition(|msg| matches!(msg.kind, MsgKind::Agent))
                        } else {
                            None
                        };
                        for (index, msg) in self.chat.iter().enumerate() {
                            render_message(
                                ui,
                                msg,
                                streaming_tail_index == Some(index),
                                t,
                                self.motion_enabled,
                            );
                        }
                        if self.streaming && streaming_tail_index.is_none() {
                            render_typing(ui, t);
                        }
                    });
                ui.add_space(8.0);
                self.render_composer(ui);
            }
        });
    }

    fn render_composer(&mut self, ui: &mut Ui) {
        Frame::default()
            .fill(Color32::from_rgb(34, 34, 39))
            .stroke(Stroke::new(1.0, Color32::from_rgb(50, 50, 58)))
            .rounding(16.0)
            .inner_margin(Margin::same(12.0))
            .show(ui, |ui| {
                if self.goal_mode_enabled && !self.active_goal.trim().is_empty() {
                    Frame::default()
                        .fill(timeline_state_fill(
                            TimelineState::Waiting,
                            self.last_tick.elapsed().as_secs_f32(),
                            self.motion_enabled,
                        ))
                        .rounding(8.0)
                        .inner_margin(Margin::symmetric(9.0, 6.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(goal_chip_text(&self.active_goal))
                                        .strong()
                                        .color(TEXT),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui.button("Edit").clicked() {
                                        self.inspector = InspectorTab::Goal;
                                    }
                                });
                            });
                        });
                    ui.add_space(8.0);
                }
                let response = ui.add_sized(
                    [ui.available_width().min(760.0), 88.0],
                    TextEdit::multiline(&mut self.prompt)
                        .hint_text("Do anything")
                        .font(FontId::proportional(16.0)),
                );
                if response.lost_focus()
                    && ui.input(|i| i.key_pressed(Key::Enter) && !i.modifiers.shift)
                {
                    self.submit_prompt();
                }
                let context_suggestions = context_file_suggestions(
                    &self.repo_index,
                    &self.composer_file_query,
                    COMPOSER_CONTEXT_SUGGESTION_LIMIT,
                );
                let mut attach_context: Option<PathBuf> = None;
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Context").color(MUTED).size(12.0));
                    let context_response = ui.add_sized(
                        [220.0, 24.0],
                        TextEdit::singleline(&mut self.composer_file_query)
                            .hint_text("Search files"),
                    );
                    let attach_with_tab = context_response.has_focus()
                        && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Tab));
                    if attach_with_tab {
                        if let Some(entry) = context_suggestions.iter().find(|entry| !entry.is_dir)
                        {
                            attach_context = Some(entry.path.clone());
                        }
                    }
                    if ui
                        .add_enabled(
                            context_suggestions.iter().any(|entry| !entry.is_dir),
                            egui::Button::new("Attach"),
                        )
                        .clicked()
                    {
                        if let Some(entry) = context_suggestions.iter().find(|entry| !entry.is_dir)
                        {
                            attach_context = Some(entry.path.clone());
                        }
                    }
                });
                if !context_suggestions.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        for entry in &context_suggestions {
                            if !entry.is_dir
                                && ui
                                    .button(format!("+ {}", entry.relative))
                                    .on_hover_text(entry.path.display().to_string())
                                    .clicked()
                            {
                                attach_context = Some(entry.path.clone());
                            }
                        }
                    });
                }
                if let Some(path) = attach_context {
                    self.attach_file_context_to_prompt(&path);
                    self.composer_file_query.clear();
                }
                ui.horizontal(|ui| {
                    permission_button(ui, &mut self.cfg);
                    if self.streaming {
                        ui.label(
                            RichText::new("model locked during turn")
                                .color(FAINT)
                                .size(12.0),
                        );
                    } else if toggle_pill(ui, "Fast", self.cfg.fast_mode, ACCENT).clicked() {
                        self.toggle_fast_mode();
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(send_button(self.streaming)).clicked() {
                            self.submit_prompt();
                        }
                        ui.add_enabled_ui(!self.streaming, |ui| {
                            self.render_effort_picker(ui);
                            self.render_model_picker(ui);
                        });
                    });
                });
                if !self.queued_prompts.is_empty() {
                    ui.label(
                        RichText::new(format!("{} prompt(s) queued", self.queued_prompts.len()))
                            .color(ACCENT)
                            .size(12.0),
                    );
                }
            });
    }

    fn render_model_picker(&mut self, ui: &mut Ui) {
        egui::ComboBox::from_id_salt("model_picker")
            .selected_text(display_model(&self.cfg))
            .width(300.0)
            .show_ui(ui, |ui| {
                ui.add(
                    TextEdit::singleline(&mut self.model_query)
                        .hint_text("Search model/provider/fast"),
                );
                let query = self.model_query.trim().to_ascii_lowercase();
                ui.separator();
                for preset in MODELS.iter().filter(|m| model_matches(m, &query)) {
                    let selected = self.cfg.provider == preset.provider
                        && self.cfg.model == preset.model
                        && self.cfg.fast_mode == preset.fast;
                    let text = format!(
                        "{} · {}    {}",
                        preset.provider_label, preset.label, preset.badge
                    );
                    if ui
                        .selectable_label(selected, text)
                        .on_hover_text(preset.summary)
                        .clicked()
                    {
                        self.cfg.provider = preset.provider.to_string();
                        self.cfg.model = preset.model.to_string();
                        self.cfg.fast_mode = preset.fast;
                        if preset.fast {
                            self.cfg.reasoning_effort = "low".to_string();
                        } else if self.cfg.reasoning_effort == "low" {
                            self.cfg.reasoning_effort = "medium".to_string();
                        }
                        self.reconfigure();
                        ui.close_menu();
                    }
                }
            });
    }

    fn render_effort_picker(&mut self, ui: &mut Ui) {
        egui::ComboBox::from_id_salt("effort_picker")
            .selected_text(effort_label(&self.cfg.reasoning_effort))
            .width(120.0)
            .show_ui(ui, |ui| {
                for effort in EFFORTS {
                    if ui
                        .selectable_label(self.cfg.reasoning_effort == effort.value, effort.label)
                        .on_hover_text(effort.summary)
                        .clicked()
                    {
                        self.cfg.reasoning_effort = effort.value.to_string();
                        if effort.value != "low" {
                            self.cfg.fast_mode = false;
                        }
                        self.reconfigure();
                        ui.close_menu();
                    }
                }
            });
    }

    fn render_inspector(&mut self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            inspector_tab(ui, &mut self.inspector, InspectorTab::Timeline, "Timeline");
            inspector_tab(ui, &mut self.inspector, InspectorTab::Goal, "Goal");
            inspector_tab(
                ui,
                &mut self.inspector,
                InspectorTab::Approvals,
                "Approvals",
            );
            inspector_tab(
                ui,
                &mut self.inspector,
                InspectorTab::Checkpoints,
                "Checkpoints",
            );
            inspector_tab(ui, &mut self.inspector, InspectorTab::Usage, "Usage");
            inspector_tab(ui, &mut self.inspector, InspectorTab::Files, "Files");
            inspector_tab(ui, &mut self.inspector, InspectorTab::Diff, "Diff");
            inspector_tab(ui, &mut self.inspector, InspectorTab::Terminal, "Terminal");
            inspector_tab(ui, &mut self.inspector, InspectorTab::Browser, "Browser");
            inspector_tab(ui, &mut self.inspector, InspectorTab::Settings, "Settings");
        });
        ui.separator();
        match self.inspector {
            InspectorTab::Timeline => self.render_timeline(ui),
            InspectorTab::Goal => self.render_goal(ui),
            InspectorTab::Approvals => self.render_approvals(ui),
            InspectorTab::Checkpoints => self.render_checkpoints(ui),
            InspectorTab::Usage => self.render_usage(ui),
            InspectorTab::Files => self.render_files(ui),
            InspectorTab::Diff => self.render_diff(ui),
            InspectorTab::Terminal => self.render_terminal(ui),
            InspectorTab::Browser => self.render_browser(ui),
            InspectorTab::Settings => self.render_settings_page(ui),
        }
    }

    fn render_timeline(&mut self, ui: &mut Ui) {
        if self.streaming || !self.queued_prompts.is_empty() {
            Frame::default()
                .fill(Color32::from_rgb(34, 34, 39))
                .stroke(Stroke::new(1.0, PANEL_HI))
                .rounding(8.0)
                .inner_margin(Margin::same(9.0))
                .show(ui, |ui| {
                    ui.label(RichText::new("Queue / steer").strong().color(TEXT));
                    ui.add(
                        TextEdit::singleline(&mut self.steer_text)
                            .hint_text("Steer note for the next agent turn"),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Queue steer").clicked() {
                            let note = self.steer_text.trim().to_string();
                            if !note.is_empty() {
                                self.queued_prompts.push_back(build_steer_prompt(&note));
                                self.steer_text.clear();
                            }
                        }
                        if ui.button("Clear queue").clicked() {
                            self.queued_prompts.clear();
                        }
                    });
                    if !self.queued_prompts.is_empty() {
                        ui.label(
                            RichText::new(format!("Queued: {}", self.queued_prompts.len()))
                                .color(ACCENT)
                                .size(12.0),
                        );
                    }
                });
            ui.add_space(8.0);
        }
        ScrollArea::vertical().show(ui, |ui| {
            let t = self.last_tick.elapsed().as_secs_f32();
            for item in self.timeline.clone().iter().rev().take(80) {
                Frame::default()
                    .fill(timeline_state_fill(item.state, t, self.motion_enabled))
                    .rounding(8.0)
                    .inner_margin(Margin::same(9.0))
                    .show(ui, |ui| {
                        ui.label(RichText::new(&item.title).strong().color(TEXT));
                        ui.label(RichText::new(&item.detail).color(MUTED).size(12.0));
                        if let Some(id) = item.request_id {
                            ui.horizontal(|ui| {
                                if ui.button("Approve").clicked() {
                                    self.answer_approval(id, ApprovalDecision::Approve);
                                }
                                if ui.button("Always this session").clicked() {
                                    self.answer_approval(id, ApprovalDecision::ApproveForSession);
                                }
                                if ui.button("Reject").clicked() {
                                    self.answer_approval(id, ApprovalDecision::Reject);
                                }
                            });
                        }
                    });
                ui.add_space(6.0);
            }
        });
    }

    fn answer_approval(&mut self, request_id: u64, decision: ApprovalDecision) {
        remove_pending_approval(&mut self.pending_approvals, request_id);
        clear_timeline_approval_request(&mut self.timeline, request_id);
        let _ = self.engine_tx.send(RuntimeCmd::Op(Op::ApprovalResponse {
            request_id,
            decision,
        }));
        self.timeline.push(TimelineItem {
            title: "Approval answered".to_string(),
            detail: format!("{decision:?}"),
            state: TimelineState::Done,
            request_id: None,
        });
    }

    fn save_goal_mode(&mut self) {
        self.active_goal = self.active_goal.trim().to_string();
        self.goal_success_criteria = self.goal_success_criteria.trim().to_string();
        if self.goal_mode_enabled && self.active_goal.is_empty() {
            self.goal_message = "Goal is required before enabling goal mode".to_string();
            self.timeline.push(TimelineItem {
                title: "Goal mode not saved".to_string(),
                detail: self.goal_message.clone(),
                state: TimelineState::Error,
                request_id: None,
            });
            return;
        }
        self.persist_desktop_preferences();
        self.goal_message = if self.goal_mode_enabled {
            "Goal mode enabled".to_string()
        } else {
            "Goal mode saved but inactive".to_string()
        };
        self.timeline.push(TimelineItem {
            title: "Goal mode saved".to_string(),
            detail: empty_label(&self.active_goal).to_string(),
            state: TimelineState::Done,
            request_id: None,
        });
    }

    fn clear_goal_mode(&mut self) {
        self.goal_mode_enabled = false;
        self.active_goal.clear();
        self.goal_success_criteria.clear();
        self.goal_message = "Goal mode cleared".to_string();
        self.persist_desktop_preferences();
        self.timeline.push(TimelineItem {
            title: "Goal mode cleared".to_string(),
            detail: "No active durable objective".to_string(),
            state: TimelineState::Done,
            request_id: None,
        });
    }

    fn render_goal(&mut self, ui: &mut Ui) {
        ui.heading("Goal mode");
        ui.horizontal(|ui| {
            if toggle_pill(ui, "Active", self.goal_mode_enabled, ACCENT).clicked() {
                self.goal_mode_enabled = !self.goal_mode_enabled;
            }
            if self.goal_mode_enabled && !self.active_goal.trim().is_empty() {
                ui.label(
                    RichText::new("attached to next prompt")
                        .color(ACCENT)
                        .size(12.0),
                );
            }
        });
        ui.add_space(8.0);
        ui.add(
            TextEdit::multiline(&mut self.active_goal)
                .desired_rows(3)
                .hint_text("Goal"),
        );
        ui.add(
            TextEdit::multiline(&mut self.goal_success_criteria)
                .desired_rows(4)
                .hint_text("Success criteria"),
        );
        ui.horizontal(|ui| {
            if ui.button("Save goal").clicked() {
                self.save_goal_mode();
            }
            if ui.button("Clear").clicked() {
                self.clear_goal_mode();
            }
            if ui.button("Use in Hermes").clicked() {
                let goal = self.active_goal.trim();
                if goal.is_empty() {
                    self.goal_message = "Goal is required before applying to Hermes".to_string();
                } else {
                    self.evolve_goal = goal.to_string();
                    if !self.goal_success_criteria.trim().is_empty() {
                        self.hermes_review_prompt = self.goal_success_criteria.trim().to_string();
                    }
                    self.nav = NavSurface::Hermes;
                    self.goal_message = "Goal copied into Hermes".to_string();
                }
            }
        });
        if !self.goal_message.trim().is_empty() {
            ui.label(RichText::new(&self.goal_message).color(MUTED).size(12.0));
        }
        ui.add_space(8.0);
        let preview = build_prompt_with_goal_mode(
            "Next user request will appear here.",
            self.goal_mode_enabled,
            &self.active_goal,
            &self.goal_success_criteria,
        );
        Frame::default()
            .fill(PANEL)
            .stroke(Stroke::new(1.0, PANEL_HI))
            .rounding(8.0)
            .inner_margin(Margin::same(9.0))
            .show(ui, |ui| {
                ui.label(RichText::new("Prompt context preview").strong().color(TEXT));
                ui.label(RichText::new(preview).color(MUTED).size(12.0));
            });
    }

    fn render_approvals(&mut self, ui: &mut Ui) {
        ui.heading("Approvals");
        ui.label(
            RichText::new("Review pending tool requests before they run.")
                .color(MUTED)
                .size(12.0),
        );
        ui.add_space(8.0);
        if self.pending_approvals.is_empty() {
            ui.label(RichText::new("No pending approvals").color(FAINT));
            return;
        }
        ScrollArea::vertical().show(ui, |ui| {
            for approval in self.pending_approvals.clone().iter().rev() {
                Frame::default()
                    .fill(PANEL)
                    .stroke(Stroke::new(1.0, PANEL_HI))
                    .rounding(8.0)
                    .inner_margin(Margin::same(10.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&approval.tool).strong().color(TEXT));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(
                                    RichText::new(format!("#{}", approval.request_id))
                                        .color(FAINT)
                                        .size(11.0),
                                );
                            });
                        });
                        ui.label(RichText::new(&approval.summary).color(MUTED).size(12.0));
                        ui.horizontal(|ui| {
                            if ui.button("Approve").clicked() {
                                self.answer_approval(
                                    approval.request_id,
                                    ApprovalDecision::Approve,
                                );
                            }
                            if ui.button("Always this session").clicked() {
                                self.answer_approval(
                                    approval.request_id,
                                    ApprovalDecision::ApproveForSession,
                                );
                            }
                            if ui.button("Reject").clicked() {
                                self.answer_approval(approval.request_id, ApprovalDecision::Reject);
                            }
                        });
                    });
                ui.add_space(8.0);
            }
        });
    }

    fn render_checkpoints(&mut self, ui: &mut Ui) {
        ui.heading("Checkpoints");
        ui.label(
            RichText::new("Restore the workspace to a checkpoint created before mutating tools.")
                .color(MUTED)
                .size(12.0),
        );
        ui.add_space(8.0);
        if self.checkpoints.is_empty() {
            ui.label(RichText::new("No checkpoints yet").color(FAINT));
            return;
        }
        ScrollArea::vertical().show(ui, |ui| {
            for checkpoint in self.checkpoints.clone().iter().rev() {
                Frame::default()
                    .fill(PANEL)
                    .stroke(Stroke::new(1.0, PANEL_HI))
                    .rounding(8.0)
                    .inner_margin(Margin::same(10.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("Checkpoint #{}", checkpoint.id))
                                    .strong()
                                    .color(TEXT),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let status = if checkpoint.rewound {
                                    format!(
                                        "rewound {} file(s)",
                                        checkpoint.restored_files.unwrap_or(0)
                                    )
                                } else {
                                    "ready".to_string()
                                };
                                ui.label(RichText::new(status).color(MUTED).size(11.0));
                            });
                        });
                        ui.label(RichText::new(&checkpoint.label).color(MUTED).size(12.0));
                        if !checkpoint.rewound && ui.button("Rewind").clicked() {
                            self.rewind_checkpoint(checkpoint.id);
                        }
                    });
                ui.add_space(8.0);
            }
        });
    }

    fn rewind_checkpoint(&mut self, checkpoint_id: u64) {
        match self
            .engine_tx
            .send(RuntimeCmd::Op(Op::Rewind { checkpoint_id }))
        {
            Ok(()) => {
                self.timeline.push(TimelineItem {
                    title: "Rewind requested".to_string(),
                    detail: format!("Checkpoint #{checkpoint_id}"),
                    state: TimelineState::Waiting,
                    request_id: None,
                });
            }
            Err(e) => {
                self.timeline.push(TimelineItem {
                    title: "Rewind request failed".to_string(),
                    detail: e.to_string(),
                    state: TimelineState::Error,
                    request_id: None,
                });
            }
        }
    }

    fn render_usage(&mut self, ui: &mut Ui) {
        ui.heading("Usage / context");
        ui.label(
            RichText::new("Track token usage, context window, and compaction events.")
                .color(MUTED)
                .size(12.0),
        );
        ui.add_space(8.0);
        Frame::default()
            .fill(PANEL)
            .stroke(Stroke::new(1.0, PANEL_HI))
            .rounding(8.0)
            .inner_margin(Margin::same(10.0))
            .show(ui, |ui| {
                let latest_context = latest_context_tokens(&self.token_usage, &self.compactions);
                ui.label(RichText::new("Context window").strong().color(TEXT));
                match (self.context_window, latest_context) {
                    (Some(limit), Some(used)) => {
                        ui.label(
                            RichText::new(format!(
                                "{} / {} tokens",
                                compact_number(used),
                                compact_number(limit)
                            ))
                            .color(MUTED)
                            .size(12.0),
                        );
                        if let Some(percent) = context_usage_percent(Some(limit), Some(used)) {
                            ui.add(
                                egui::ProgressBar::new((percent / 100.0).clamp(0.0, 1.0))
                                    .text(format!("{percent:.1}%")),
                            );
                        }
                    }
                    (Some(limit), None) => {
                        ui.label(
                            RichText::new(format!("{} token window", compact_number(limit)))
                                .color(MUTED)
                                .size(12.0),
                        );
                    }
                    _ => {
                        ui.label(RichText::new("Waiting for provider usage data").color(FAINT));
                    }
                }
                if let Some(summary) = latest_token_usage_summary(&self.token_usage) {
                    ui.separator();
                    ui.label(RichText::new("Latest turn").strong().color(TEXT));
                    ui.label(RichText::new(summary).color(MUTED).size(12.0));
                }
            });
        ui.add_space(8.0);
        ui.label(RichText::new("Recent token usage").strong().color(TEXT));
        if self.token_usage.is_empty() {
            ui.label(RichText::new("No token usage events yet").color(FAINT));
        } else {
            for record in self.token_usage.iter().rev().take(8) {
                ui.label(
                    RichText::new(format_token_usage_record(record))
                        .monospace()
                        .size(12.0)
                        .color(MUTED),
                );
            }
        }
        ui.separator();
        ui.label(RichText::new("Compactions").strong().color(TEXT));
        if self.compactions.is_empty() {
            ui.label(RichText::new("No compactions yet").color(FAINT));
        } else {
            for record in self.compactions.iter().rev().take(8) {
                ui.label(
                    RichText::new(format!(
                        "dropped {} message(s), ~{} tokens remain",
                        record.dropped,
                        compact_number(record.tokens)
                    ))
                    .color(MUTED)
                    .size(12.0),
                );
            }
        }
    }

    fn render_files(&mut self, ui: &mut Ui) {
        ui.label(section_text(self.workspace.display().to_string()));
        ui.add(
            TextEdit::singleline(&mut self.file_query)
                .hint_text("Search files")
                .desired_width(f32::INFINITY),
        );
        if !self.file_message.trim().is_empty() {
            ui.label(RichText::new(&self.file_message).color(MUTED).size(12.0));
        }
        if ui.button("Refresh index").clicked() {
            self.refresh_repo_index();
            self.file_message = format!("Indexed {} workspace item(s)", self.repo_index.len());
        }
        ui.add_space(6.0);
        let entries = self
            .repo_index
            .iter()
            .filter(|entry| repo_index_entry_matches_query(entry, &self.file_query))
            .take(160)
            .cloned()
            .collect::<Vec<_>>();
        ScrollArea::vertical().show(ui, |ui| {
            if entries.is_empty() {
                ui.label(RichText::new("No matching files").color(FAINT).size(12.0));
            }
            for entry in entries {
                let prefix = if entry.is_dir { "dir" } else { "file" };
                if entry.is_dir {
                    if ui.button(format!("{prefix}  {}", entry.relative)).clicked() {
                        self.file_query = entry.relative.clone();
                    }
                    continue;
                }
                if ui.button(format!("{prefix}  {}", entry.relative)).clicked() {
                    self.selected_file = Some(entry.path.clone());
                    self.selected_file_text =
                        read_file_context(&entry.path, FILE_CONTEXT_CHAR_LIMIT)
                            .unwrap_or_else(|e| format!("cannot open file: {e}"));
                    self.file_message = format!("Opened {}", entry.relative);
                }
            }
        });
        if let Some(path) = self.selected_file.clone() {
            ui.separator();
            ui.label(
                RichText::new(path.display().to_string())
                    .color(MUTED)
                    .size(12.0),
            );
            ui.horizontal(|ui| {
                if ui.button("Insert context").clicked() {
                    self.attach_file_context_to_prompt(&path);
                }
                if ui.button("Copy path").clicked() {
                    ui.output_mut(|output| output.copied_text = path.display().to_string());
                    self.file_message = "Copied file path".to_string();
                }
                if ui.button("Reveal").clicked() {
                    match reveal_path(&path) {
                        Ok(()) => {
                            self.file_message = "Revealed file".to_string();
                            self.timeline.push(TimelineItem {
                                title: "File revealed".to_string(),
                                detail: path.display().to_string(),
                                state: TimelineState::Done,
                                request_id: None,
                            });
                        }
                        Err(e) => {
                            self.file_message = format!("Reveal failed: {e}");
                            self.timeline.push(TimelineItem {
                                title: "Reveal file failed".to_string(),
                                detail: e.to_string(),
                                state: TimelineState::Error,
                                request_id: None,
                            });
                        }
                    }
                }
            });
            ui.add(
                TextEdit::multiline(&mut self.selected_file_text)
                    .desired_rows(12)
                    .font(FontId::monospace(12.0)),
            );
        }
    }

    fn render_diff(&mut self, ui: &mut Ui) {
        ui.heading("Review diffs");
        self.render_git_controls(ui);
        ui.add_space(8.0);
        Frame::default()
            .fill(Color32::from_rgb(18, 18, 20))
            .rounding(8.0)
            .inner_margin(Margin::same(10.0))
            .show(ui, |ui| {
                ui.label(RichText::new("Status").strong().color(TEXT));
                ui.label(
                    RichText::new(&self.git_snapshot.status)
                        .monospace()
                        .color(MUTED),
                );
                if !self.git_snapshot.diff_stat.trim().is_empty() {
                    ui.separator();
                    ui.label(RichText::new("Diff stat").strong().color(TEXT));
                    ui.label(
                        RichText::new(&self.git_snapshot.diff_stat)
                            .monospace()
                            .color(MUTED),
                    );
                }
                if !self.git_snapshot.changed_files.is_empty() {
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Changed files").strong().color(TEXT));
                        if ui.button("Copy summary").clicked() {
                            ui.output_mut(|output| {
                                output.copied_text = build_git_review_summary(&self.git_snapshot);
                            });
                            self.git_review_message =
                                "Copied git review summary to clipboard".to_string();
                        }
                    });
                    if !self.git_review_message.trim().is_empty() {
                        ui.label(
                            RichText::new(&self.git_review_message)
                                .color(MUTED)
                                .size(12.0),
                        );
                    }
                    let changed_files = self.git_snapshot.changed_files.clone();
                    ScrollArea::vertical().max_height(150.0).show(ui, |ui| {
                        for file in changed_files {
                            let selected = self
                                .selected_git_file
                                .as_ref()
                                .map(|selected| selected == &file)
                                .unwrap_or(false);
                            let label = format!("{}  {}", file.status, file.display_path);
                            if thread_button(ui, &label, selected).clicked() {
                                match read_git_diff_for_path(&self.workspace, &file) {
                                    Ok(diff) => {
                                        self.selected_git_file = Some(file.clone());
                                        self.selected_git_file_diff = diff;
                                        self.git_review_message =
                                            format!("Opened diff for {}", file.display_path);
                                    }
                                    Err(e) => {
                                        self.selected_git_file = Some(file.clone());
                                        self.selected_git_file_diff = e.clone();
                                        self.git_review_message = e;
                                    }
                                }
                            }
                        }
                    });
                }
                if let Some(file) = self.selected_git_file.clone() {
                    ui.separator();
                    ui.label(
                        RichText::new(format!("File diff: {}", file.display_path))
                            .strong()
                            .color(TEXT),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Stage file").clicked() {
                            self.stage_selected_git_file();
                        }
                        if ui.button("Unstage file").clicked() {
                            self.unstage_selected_git_file();
                        }
                    });
                    ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                        ui.label(
                            RichText::new(&self.selected_git_file_diff)
                                .monospace()
                                .size(11.5)
                                .color(TEXT),
                        );
                    });
                }
                if !self.git_snapshot.raw_diff.trim().is_empty() {
                    ui.separator();
                    ui.label(RichText::new("Raw diff preview").strong().color(TEXT));
                    ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                        ui.label(
                            RichText::new(&self.git_snapshot.raw_diff)
                                .monospace()
                                .size(11.5)
                                .color(TEXT),
                        );
                    });
                }
            });
        ui.add_space(8.0);
        ui.label(
            RichText::new("Agent patch/checkpoint history")
                .strong()
                .color(TEXT),
        );
        for item in self
            .timeline
            .iter()
            .filter(|i| i.title.contains("Patch") || i.title.contains("Checkpoint"))
            .rev()
        {
            ui.label(RichText::new(&item.title).strong());
            ui.label(RichText::new(&item.detail).color(MUTED));
            ui.separator();
        }
    }

    fn render_terminal(&mut self, ui: &mut Ui) {
        if let Some(command) = self
            .active_terminal_job
            .as_ref()
            .map(|job| job.command.clone())
        {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("Running: {command}"))
                        .color(ACCENT)
                        .size(12.0),
                );
                if ui.button("Stop").clicked() {
                    self.stop_terminal_command();
                }
            });
            ui.add_space(4.0);
        }
        ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
            for line in &self.terminal_lines {
                ui.label(RichText::new(line).monospace().size(12.0).color(TEXT));
            }
        });
        ui.horizontal(|ui| {
            let response =
                ui.add(TextEdit::singleline(&mut self.terminal_input).hint_text("run command"));
            if response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                self.run_terminal_command();
            }
            if ui
                .add_enabled(self.active_terminal_job.is_none(), egui::Button::new("Run"))
                .clicked()
            {
                self.run_terminal_command();
            }
        });
    }

    fn render_browser(&mut self, ui: &mut Ui) {
        ui.heading("Browser / computer use");
        ui.label(
            RichText::new(
                "Track browser targets, operator actions, screenshots, and visual evidence.",
            )
            .color(MUTED),
        );
        ui.add_space(10.0);
        Frame::default()
            .fill(PANEL)
            .stroke(Stroke::new(1.0, PANEL_HI))
            .rounding(10.0)
            .inner_margin(Margin::same(12.0))
            .show(ui, |ui| {
                ui.label(RichText::new("Browser target").strong().color(TEXT));
                ui.add(
                    TextEdit::singleline(&mut self.browser_target_url)
                        .hint_text("https://localhost:3000 or file:///..."),
                );
                ui.add(
                    TextEdit::multiline(&mut self.browser_action_note)
                        .desired_rows(2)
                        .hint_text("Action note or observation"),
                );
                ui.horizontal(|ui| {
                    if ui.button("Open target").clicked() {
                        self.open_browser_target();
                    }
                    if ui.button("Log action").clicked() {
                        self.record_browser_action("operator-note");
                    }
                    if ui.button("Insert browser context").clicked() {
                        self.insert_browser_context_in_prompt();
                    }
                });
                if let Some(pending) = self.pending_browser_snapshot.clone() {
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new("Pending snapshot").strong().color(TEXT));
                            ui.label(
                                RichText::new(format!(
                                    "{} · {}",
                                    pending.title,
                                    empty_label(&pending.source_url)
                                ))
                                .color(MUTED)
                                .size(12.0),
                            );
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button("Dismiss").clicked() {
                                self.dismiss_pending_browser_snapshot();
                            }
                            if ui.button("Capture").clicked() {
                                self.capture_pending_browser_snapshot();
                            }
                        });
                    });
                }
                if !self.browser_message.trim().is_empty() {
                    ui.label(RichText::new(&self.browser_message).color(MUTED).size(12.0));
                }
                if !self.browser_actions.is_empty() {
                    ui.separator();
                    ui.label(RichText::new("Recent browser actions").strong().color(TEXT));
                    for action in self.browser_actions.iter().take(6) {
                        ui.label(
                            RichText::new(format!(
                                "{} · {} · {}",
                                action.action,
                                action.url,
                                empty_label(&action.note)
                            ))
                            .color(MUTED)
                            .size(12.0),
                        );
                    }
                }
            });
        ui.add_space(10.0);
        self.render_appshots_panel(ui);
        feature_card(
            ui,
            "Browser annotations",
            "Click, inspect, and mark UI issues for the agent.",
            true,
        );
        feature_card(
            ui,
            "Computer use",
            "Locked-mode desktop control lane for high-friction workflows.",
            false,
        );
    }

    fn render_git_controls(&mut self, ui: &mut Ui) {
        Frame::default()
            .fill(PANEL)
            .stroke(Stroke::new(1.0, PANEL_HI))
            .rounding(10.0)
            .inner_margin(Margin::same(12.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Git workspace").strong().color(TEXT));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Refresh").clicked() {
                            self.refresh_git_review();
                            self.git_review_message = "Git snapshot refreshed".to_string();
                        }
                    });
                });
                ui.label(
                    RichText::new(format!(
                        "Current branch: {}",
                        empty_label(&self.git_branches.current_branch)
                    ))
                    .color(MUTED),
                );
                ui.add_space(6.0);
                ui.label(RichText::new("Commit staged changes").strong().color(TEXT));
                ui.add(
                    TextEdit::singleline(&mut self.git_commit_message).hint_text("Commit message"),
                );
                ui.horizontal(|ui| {
                    if ui.button("Commit staged").clicked() {
                        self.commit_staged_from_ui();
                    }
                    if ui.button("Refresh git").clicked() {
                        self.refresh_git_review();
                        self.git_review_message = "Git snapshot refreshed".to_string();
                    }
                });
                ui.separator();
                ui.label(
                    RichText::new("Publish branch / PR draft")
                        .strong()
                        .color(TEXT),
                );
                ui.horizontal(|ui| {
                    ui.add(
                        TextEdit::singleline(&mut self.git_push_remote)
                            .desired_width(110.0)
                            .hint_text("remote"),
                    );
                    ui.add(
                        TextEdit::singleline(&mut self.git_push_branch)
                            .desired_width(180.0)
                            .hint_text("branch"),
                    );
                    ui.add(
                        TextEdit::singleline(&mut self.git_pr_base)
                            .desired_width(120.0)
                            .hint_text("base"),
                    );
                });
                ui.add(TextEdit::singleline(&mut self.git_pr_title).hint_text("PR title"));
                ui.add(
                    TextEdit::multiline(&mut self.git_pr_body)
                        .desired_rows(2)
                        .hint_text("PR body notes"),
                );
                ui.horizontal(|ui| {
                    if ui.button("Push branch").clicked() {
                        self.push_current_branch_from_ui();
                    }
                    if ui.button("Copy PR draft").clicked() {
                        self.copy_git_pr_draft_from_ui(ui);
                    }
                    if ui.button("Copy compare URL").clicked() {
                        self.copy_github_compare_url_from_ui(ui);
                    }
                });
                ui.label(
                    RichText::new(format!(
                        "Compare target: {}...{}",
                        empty_label(&self.git_pr_base),
                        empty_label(&self.selected_git_publish_branch())
                    ))
                    .monospace()
                    .size(11.5)
                    .color(FAINT),
                );
                ui.separator();
                ui.horizontal(|ui| {
                    let branches = self.git_branches.branches.clone();
                    egui::ComboBox::from_id_salt("worktree_branch_picker")
                        .selected_text(empty_label(&self.worktree_branch))
                        .width(180.0)
                        .show_ui(ui, |ui| {
                            for branch in branches {
                                if ui
                                    .selectable_label(self.worktree_branch == branch, &branch)
                                    .clicked()
                                {
                                    self.worktree_branch = branch;
                                    ui.close_menu();
                                }
                            }
                        });
                    ui.add(
                        TextEdit::singleline(&mut self.worktree_branch)
                            .hint_text("branch for worktree"),
                    );
                });
                let command = build_worktree_command(&self.workspace, &self.worktree_branch);
                ui.label(RichText::new(command).monospace().size(11.5).color(FAINT));
                ui.horizontal(|ui| {
                    if ui.button("Create worktree").clicked() {
                        self.create_git_worktree();
                    }
                    if ui.button("Refresh worktrees").clicked() {
                        self.git_branches = git_branch_snapshot(&self.workspace);
                    }
                });
                if !self.git_operation_message.trim().is_empty() {
                    ui.label(
                        RichText::new(&self.git_operation_message)
                            .monospace()
                            .size(11.5)
                            .color(MUTED),
                    );
                }
                ui.separator();
                ui.label(RichText::new("Worktrees").strong().color(TEXT));
                if self.git_branches.worktrees.is_empty() {
                    ui.label(RichText::new("No git worktrees found").color(FAINT));
                }
                for worktree in self.git_branches.worktrees.clone() {
                    ui.label(
                        RichText::new(format!("{}  ·  {}", worktree.branch, worktree.path))
                            .color(MUTED)
                            .size(12.0),
                    );
                }
            });
    }

    fn render_appshots_panel(&mut self, ui: &mut Ui) {
        Frame::default()
            .fill(PANEL)
            .stroke(Stroke::new(1.0, PANEL_HI))
            .rounding(10.0)
            .inner_margin(Margin::same(12.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Appshots").strong().color(TEXT));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Refresh").clicked() {
                            self.appshots = read_appshot_specs(&self.workspace).unwrap_or_default();
                        }
                    });
                });
                if ui
                    .checkbox(
                        &mut self.attach_appshots_to_prompt,
                        "Attach saved appshots to next prompts",
                    )
                    .changed()
                {
                    self.persist_desktop_preferences();
                }
                ui.add(TextEdit::singleline(&mut self.appshot_title).hint_text("Title"));
                ui.add(TextEdit::singleline(&mut self.appshot_path).hint_text("Local image path"));
                ui.add(
                    TextEdit::multiline(&mut self.appshot_note)
                        .desired_rows(2)
                        .hint_text("Visual note"),
                );
                ui.horizontal(|ui| {
                    if ui.button("Save appshot").clicked() {
                        self.create_appshot();
                    }
                    if ui.button("Capture screen").clicked() {
                        self.capture_screen_appshot();
                    }
                    if ui.button("Insert latest").clicked() {
                        if let Some(appshot) = self.appshots.first() {
                            self.prompt =
                                build_prompt_with_appshots(&self.prompt, &[appshot.clone()]);
                            self.nav = NavSurface::Chat;
                        }
                    }
                });
            });
        ui.add_space(8.0);
        ui.label(RichText::new("Saved appshots").strong().color(TEXT));
        if self.appshots.is_empty() {
            ui.label(RichText::new("No appshots saved yet").color(FAINT));
        }
        for appshot in self.appshots.clone() {
            Frame::default()
                .fill(Color32::from_rgb(24, 24, 28))
                .stroke(Stroke::new(1.0, PANEL_HI))
                .rounding(8.0)
                .inner_margin(Margin::same(9.0))
                .show(ui, |ui| {
                    ui.label(RichText::new(&appshot.title).strong().color(TEXT));
                    if let Some(texture) = self.appshot_texture(ui.ctx(), &appshot) {
                        let size = thumbnail_size(texture.size_vec2(), 210.0, 130.0);
                        ui.add(egui::Image::new((texture.id(), size)));
                    }
                    ui.label(
                        RichText::new(&appshot.path)
                            .monospace()
                            .size(11.5)
                            .color(MUTED),
                    );
                    if !appshot.note.trim().is_empty() {
                        ui.label(RichText::new(&appshot.note).color(FAINT).size(12.0));
                    }
                    if !appshot.source_url.trim().is_empty() {
                        ui.label(
                            RichText::new(format!("Source: {}", appshot.source_url))
                                .color(MUTED)
                                .size(12.0),
                        );
                    }
                    if !appshot.browser_action_id.trim().is_empty() {
                        ui.label(
                            RichText::new(format!("Action: {}", appshot.browser_action_id))
                                .color(MUTED)
                                .size(12.0),
                        );
                    }
                    if !appshot.annotations.is_empty() {
                        ui.separator();
                        ui.label(
                            RichText::new(format!("{} annotation(s)", appshot.annotations.len()))
                                .strong()
                                .color(TEXT),
                        );
                        for annotation in &appshot.annotations {
                            ui.label(
                                RichText::new(format!(
                                    "[{}] {}: {}",
                                    annotation.label, annotation.target, annotation.note
                                ))
                                .color(MUTED)
                                .size(12.0),
                            );
                        }
                    }
                    ui.separator();
                    ui.label(
                        RichText::new("Add annotation")
                            .strong()
                            .color(TEXT)
                            .size(12.0),
                    );
                    ui.horizontal(|ui| {
                        ui.add(
                            TextEdit::singleline(&mut self.appshot_annotation_label)
                                .hint_text("A")
                                .desired_width(42.0),
                        );
                        ui.add(
                            TextEdit::singleline(&mut self.appshot_annotation_target)
                                .hint_text("Target")
                                .desired_width(150.0),
                        );
                    });
                    ui.add(
                        TextEdit::multiline(&mut self.appshot_annotation_note)
                            .desired_rows(2)
                            .hint_text("Annotation note"),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Add annotation").clicked() {
                            self.add_appshot_annotation(&appshot);
                        }
                        if ui.button("Insert in prompt").clicked() {
                            self.prompt =
                                build_prompt_with_appshots(&self.prompt, &[appshot.clone()]);
                            self.nav = NavSurface::Chat;
                        }
                        if ui.button("Reveal").clicked() {
                            self.reveal_appshot(&appshot);
                        }
                    });
                });
            ui.add_space(6.0);
        }
    }

    fn appshot_texture(&mut self, ctx: &Context, appshot: &AppshotSpec) -> Option<TextureHandle> {
        if !is_previewable_appshot_path(Path::new(&appshot.path)) {
            return None;
        }
        if let Some(texture) = self.appshot_textures.get(&appshot.id) {
            return Some(texture.clone());
        }
        match load_appshot_color_image(Path::new(&appshot.path)) {
            Ok(image) => {
                let texture = ctx.load_texture(
                    format!("appshot-{}", appshot.id),
                    image,
                    TextureOptions::LINEAR,
                );
                self.appshot_textures
                    .insert(appshot.id.clone(), texture.clone());
                Some(texture)
            }
            Err(e) => {
                self.timeline.push(TimelineItem {
                    title: "Appshot preview failed".to_string(),
                    detail: format!("{}: {e}", appshot.path),
                    state: TimelineState::Error,
                    request_id: None,
                });
                None
            }
        }
    }

    fn reveal_appshot(&mut self, appshot: &AppshotSpec) {
        let output = reveal_path(Path::new(&appshot.path));
        match output {
            Ok(()) => self.timeline.push(TimelineItem {
                title: "Appshot revealed".to_string(),
                detail: appshot.path.clone(),
                state: TimelineState::Done,
                request_id: None,
            }),
            Err(e) => self.timeline.push(TimelineItem {
                title: "Reveal appshot failed".to_string(),
                detail: e.to_string(),
                state: TimelineState::Error,
                request_id: None,
            }),
        }
    }

    fn record_browser_action(&mut self, action: &str) -> Option<BrowserActionSpec> {
        let created_ms = now_ms();
        match browser_action_from_fields(
            action,
            &self.browser_target_url,
            &self.browser_action_note,
            created_ms,
        ) {
            Ok(spec) => match write_browser_action_spec(&self.workspace, &spec) {
                Ok(()) => {
                    self.browser_actions =
                        read_browser_action_specs(&self.workspace).unwrap_or_default();
                    self.browser_action_note.clear();
                    self.browser_message = format!("Logged browser action {}", spec.action);
                    self.timeline.push(TimelineItem {
                        title: "Browser action logged".to_string(),
                        detail: format!("{} · {}", spec.action, spec.url),
                        state: TimelineState::Done,
                        request_id: None,
                    });
                    Some(spec)
                }
                Err(e) => {
                    self.browser_message = e.to_string();
                    self.timeline.push(TimelineItem {
                        title: "Browser action log failed".to_string(),
                        detail: self.browser_message.clone(),
                        state: TimelineState::Error,
                        request_id: None,
                    });
                    None
                }
            },
            Err(e) => {
                self.browser_message = e.to_string();
                self.timeline.push(TimelineItem {
                    title: "Browser action not logged".to_string(),
                    detail: self.browser_message.clone(),
                    state: TimelineState::Error,
                    request_id: None,
                });
                None
            }
        }
    }

    fn open_browser_target(&mut self) {
        let url = self.browser_target_url.trim().to_string();
        if url.is_empty() {
            self.browser_message = "Browser target URL is required".to_string();
            return;
        }
        match open_url_external(&url) {
            Ok(()) => {
                if self.browser_action_note.trim().is_empty() {
                    self.browser_action_note = "Opened externally".to_string();
                }
                self.record_browser_action("open-target");
            }
            Err(e) => {
                self.browser_message = e.to_string();
                self.timeline.push(TimelineItem {
                    title: "Browser target open failed".to_string(),
                    detail: self.browser_message.clone(),
                    state: TimelineState::Error,
                    request_id: None,
                });
            }
        }
    }

    fn insert_browser_context_in_prompt(&mut self) {
        self.prompt = build_prompt_with_browser_context(
            &self.prompt,
            &self.browser_target_url,
            &self.browser_actions,
        );
        self.browser_message = "Inserted browser context into prompt".to_string();
        self.nav = NavSurface::Chat;
    }

    fn open_search_result(&mut self, item: &SearchResultItem) {
        if let Some(id) = item.target.strip_prefix("session:") {
            if let Some(session) = self
                .sessions
                .iter()
                .find(|session| session.id == id)
                .cloned()
            {
                self.load_session(&session);
            }
            return;
        }
        if let Some(id) = item.target.strip_prefix("command:") {
            self.execute_command(id);
            return;
        }
        if let Some(id) = item.target.strip_prefix("automation-run:") {
            if let Some(spec) = self
                .automations
                .iter()
                .find(|automation| automation.id == id)
                .cloned()
            {
                self.run_automation_now(&spec);
            }
            return;
        }
        if let Some(path) = item.target.strip_prefix("file:") {
            let path = PathBuf::from(path);
            if path.is_file() {
                self.selected_file = Some(path.clone());
                self.selected_file_text = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| format!("cannot open file: {e}"));
                self.inspector = InspectorTab::Files;
            } else if path.is_dir() {
                self.workspace = path;
                self.refresh_workspace_views();
                self.inspector = InspectorTab::Files;
            }
            return;
        }
        match item.target.as_str() {
            "settings:memory" => {
                self.nav = NavSurface::Settings;
                self.settings_tab = SettingsTab::Memory;
            }
            "settings:plugins" => {
                self.nav = NavSurface::Settings;
                self.settings_tab = SettingsTab::Plugins;
            }
            "settings:shortcuts" => {
                self.nav = NavSurface::Settings;
                self.settings_tab = SettingsTab::Shortcuts;
            }
            "goal:active" => {
                self.inspector = InspectorTab::Goal;
            }
            target if target.starts_with("automation:") => {
                self.nav = NavSurface::Settings;
                self.settings_tab = SettingsTab::Automations;
            }
            target if target.starts_with("appshot:") => {
                self.inspector = InspectorTab::Browser;
            }
            target if target.starts_with("hermes:") => {
                self.nav = NavSurface::Settings;
                self.settings_tab = SettingsTab::Hermes;
            }
            _ => {}
        }
    }

    fn execute_command(&mut self, command_id: &str) -> bool {
        if settings_command_opens_window(command_id) {
            self.nav = NavSurface::Settings;
            if let Some(tab) = settings_tab_for_command(command_id) {
                self.settings_tab = tab;
            }
            self.show_settings = true;
            return true;
        }
        match command_id {
            "command-palette" => {
                self.show_palette = true;
            }
            "new-chat" => {
                self.chat.clear();
                self.selected_session = None;
                self.pending_session_context = None;
                self.rename_session_title.clear();
                self.nav = NavSurface::Chat;
            }
            "search-threads" => {
                self.nav = NavSurface::Search;
            }
            "workspace-switch" => {
                self.nav = NavSurface::Settings;
                self.settings_tab = SettingsTab::General;
            }
            "inspector-terminal" => {
                self.inspector = InspectorTab::Terminal;
            }
            "inspector-goal" => {
                self.inspector = InspectorTab::Goal;
            }
            "inspector-approvals" => {
                self.inspector = InspectorTab::Approvals;
            }
            "inspector-checkpoints" => {
                self.inspector = InspectorTab::Checkpoints;
            }
            "inspector-usage" => {
                self.inspector = InspectorTab::Usage;
            }
            "git-refresh-diff" => {
                self.refresh_git_review();
                self.inspector = InspectorTab::Diff;
            }
            "git-stage-selected-file" => {
                self.stage_selected_git_file();
                self.inspector = InspectorTab::Diff;
            }
            "hermes-evolve" => {
                self.nav = NavSurface::Hermes;
            }
            "hermes-start-evolve" => {
                self.start_evolve();
            }
            "appshot-capture-screen" => {
                self.inspector = InspectorTab::Browser;
                self.capture_screen_appshot();
            }
            "browser-open-target" => {
                self.inspector = InspectorTab::Browser;
                self.open_browser_target();
            }
            "browser-insert-context" => {
                self.insert_browser_context_in_prompt();
            }
            "browser-capture-pending-snapshot" => {
                self.inspector = InspectorTab::Browser;
                self.capture_pending_browser_snapshot();
            }
            "terminal-stop-running-command" => {
                self.inspector = InspectorTab::Terminal;
                self.stop_terminal_command();
            }
            "thread-rename-selected" => {
                self.rename_selected_session();
            }
            "thread-pin-selected" => {
                self.toggle_selected_session_pin();
            }
            "thread-archive-selected" => {
                self.toggle_selected_session_archive();
            }
            _ => return false,
        }
        true
    }

    fn render_search(&mut self, ui: &mut Ui) {
        ui.heading("Search");
        ui.label(
            RichText::new(
                "Search threads, files, commands, tools, and memories from one command surface.",
            )
            .color(MUTED),
        );
        ui.add_space(10.0);
        ui.add(TextEdit::singleline(&mut self.session_query).hint_text("Search everything"));
        let shortcuts = shortcut_catalog();
        let results = build_global_search_results_with_repo_index(
            GlobalSearchInputs {
                workspace: &self.workspace,
                sessions: &self.sessions,
                memories: &self.memories,
                automations: &self.automations,
                appshots: &self.appshots,
                hermes_profiles: &self.hermes_profiles,
                mcp_servers: &self.cfg.mcp_servers,
                shortcuts: &shortcuts,
                goal_mode_enabled: self.goal_mode_enabled,
                active_goal: &self.active_goal,
                goal_success_criteria: &self.goal_success_criteria,
                query: &self.session_query,
            },
            &self.repo_index,
        );
        ui.separator();
        if results.is_empty() {
            ui.label(RichText::new("No matching results").color(FAINT));
        }
        for item in results.into_iter().take(80) {
            Frame::default()
                .fill(Color32::from_rgb(24, 24, 28))
                .stroke(Stroke::new(1.0, PANEL_HI))
                .rounding(8.0)
                .inner_margin(Margin::same(9.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&item.title).strong().color(TEXT));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(RichText::new(&item.kind).color(ACCENT).size(12.0));
                            if ui.button("Open").clicked() {
                                self.open_search_result(&item);
                            }
                        });
                    });
                    ui.label(RichText::new(&item.detail).color(MUTED).size(12.0));
                });
            ui.add_space(6.0);
        }
    }

    fn render_plugins(&mut self, ui: &mut Ui) {
        ui.heading("Plugins & skills");
        ui.label(
            RichText::new(
                "Codex-like plugin lane for MCP servers, skills, and local harness capabilities.",
            )
            .color(MUTED),
        );
        ui.add_space(10.0);
        self.render_mcp_server_list(ui, false);
        if ui.button("Configure MCP servers").clicked() {
            self.nav = NavSurface::Settings;
            self.settings_tab = SettingsTab::Plugins;
        }
        ui.add_space(8.0);
        feature_card(
            ui,
            "Skills",
            "Reusable task behaviors and project conventions.",
            true,
        );
        feature_card(ui, "Marketplace", "Future install/update surface.", false);
    }

    fn render_mcp_server_list(&mut self, ui: &mut Ui, editable: bool) {
        ui.label(RichText::new("MCP servers").strong().color(TEXT));
        if self.cfg.mcp_servers.is_empty() {
            ui.label(RichText::new("No MCP servers configured").color(FAINT));
            return;
        }
        let servers = self.cfg.mcp_servers.clone();
        let mut edit: Option<McpServerConfig> = None;
        let mut remove: Option<String> = None;
        let mut reconnect = false;
        for server in servers {
            let health = mcp_health_for(&self.mcp_health, &server);
            Frame::default()
                .fill(Color32::from_rgb(24, 24, 28))
                .stroke(Stroke::new(1.0, PANEL_HI))
                .rounding(8.0)
                .inner_margin(Margin::same(9.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&server.name).strong().color(TEXT));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let color = match health.status.as_str() {
                                "connected" => ACCENT,
                                "error" => DANGER,
                                _ => MUTED,
                            };
                            ui.label(
                                RichText::new(format!(
                                    "{} · {} tool(s)",
                                    health.status, health.tool_count
                                ))
                                .color(color)
                                .size(12.0),
                            );
                        });
                    });
                    ui.label(
                        RichText::new(format!("{} {}", server.command, server.args.join(" ")))
                            .monospace()
                            .size(11.5)
                            .color(MUTED),
                    );
                    ui.label(RichText::new(&health.detail).color(MUTED).size(12.0));
                    if !health.tools.is_empty() {
                        ui.label(
                            RichText::new(health.tools.join(", "))
                                .monospace()
                                .size(11.0)
                                .color(FAINT),
                        );
                    }
                    if editable {
                        ui.horizontal(|ui| {
                            if ui.button("Reconnect").clicked() {
                                reconnect = true;
                            }
                            if ui
                                .add_enabled(
                                    !health.tools.is_empty(),
                                    egui::Button::new("Copy tools"),
                                )
                                .clicked()
                            {
                                ui.output_mut(|output| {
                                    output.copied_text = health.tools.join("\n");
                                });
                                self.mcp_message = format!("Copied tools for {}", server.name);
                            }
                            if ui.button("Edit").clicked() {
                                edit = Some(server.clone());
                            }
                            if ui.button("Remove").clicked() {
                                remove = Some(server.name.clone());
                            }
                        });
                    }
                });
            ui.add_space(6.0);
        }
        if let Some(server) = edit {
            self.edit_mcp_server(&server);
        }
        if let Some(name) = remove {
            self.delete_mcp_server(&name);
        }
        if reconnect {
            self.mcp_message = "Reconnecting MCP servers".to_string();
            self.reconfigure();
        }
    }

    fn render_automations(&mut self, ui: &mut Ui) {
        ui.heading("Automations");
        ui.label(RichText::new("Recurring jobs and heartbeat follow-ups, designed for Mac Mini / orchestrator use later.").color(MUTED));
        ui.add_space(10.0);
        Frame::default()
            .fill(PANEL)
            .stroke(Stroke::new(1.0, PANEL_HI))
            .rounding(10.0)
            .inner_margin(Margin::same(12.0))
            .show(ui, |ui| {
                ui.label(RichText::new("Create automation").strong().color(TEXT));
                ui.add(TextEdit::singleline(&mut self.automation_name).hint_text("Name"));
                ui.add(TextEdit::singleline(&mut self.automation_schedule).hint_text("Schedule"));
                ui.add(
                    TextEdit::multiline(&mut self.automation_prompt)
                        .desired_rows(3)
                        .hint_text("Prompt"),
                );
                if ui.button("Save automation").clicked() {
                    self.create_automation();
                }
            });
        ui.add_space(10.0);
        ui.label(RichText::new("Saved automations").strong().color(TEXT));
        self.render_automation_list(ui);
        ui.separator();
        feature_card(
            ui,
            "Heartbeat",
            "Wake this thread later and continue context-aware work.",
            true,
        );
        feature_card(
            ui,
            "Cron jobs",
            "Run workspace tasks on schedule in local or worktree mode.",
            true,
        );
        feature_card(
            ui,
            "Monitors",
            "Watch CI, logs, deploys, or business signals.",
            false,
        );
    }

    fn render_automation_list(&mut self, ui: &mut Ui) {
        if self.automations.is_empty() {
            ui.label(RichText::new("No automations saved yet").color(FAINT));
            return;
        }
        let automations = self.automations.clone();
        let mut run: Option<AutomationSpec> = None;
        let mut toggle: Option<AutomationSpec> = None;
        let mut delete: Option<AutomationSpec> = None;
        for spec in automations {
            let latest_run = latest_automation_run(&self.automation_runs, &spec.id);
            Frame::default()
                .fill(Color32::from_rgb(24, 24, 28))
                .stroke(Stroke::new(1.0, PANEL_HI))
                .rounding(8.0)
                .inner_margin(Margin::same(9.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&spec.name).strong().color(TEXT));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                RichText::new(&spec.status)
                                    .color(if spec.status == "ACTIVE" {
                                        ACCENT
                                    } else {
                                        MUTED
                                    })
                                    .size(12.0),
                            );
                        });
                    });
                    ui.label(
                        RichText::new(format!("{} · {}", spec.kind, spec.schedule)).color(MUTED),
                    );
                    ui.label(RichText::new(&spec.prompt).color(FAINT).size(12.0));
                    if let Some(run) = latest_run {
                        ui.label(
                            RichText::new(format!(
                                "Last run: {} · {} · {}",
                                run.trigger,
                                run.status,
                                format_time_ms(run.started_ms)
                            ))
                            .color(MUTED)
                            .size(12.0),
                        );
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Run now").clicked() {
                            run = Some(spec.clone());
                        }
                        if ui
                            .button(if spec.status == "ACTIVE" {
                                "Pause"
                            } else {
                                "Activate"
                            })
                            .clicked()
                        {
                            toggle = Some(spec.clone());
                        }
                        if ui.button("Delete").clicked() {
                            delete = Some(spec.clone());
                        }
                    });
                });
            ui.add_space(6.0);
        }
        if let Some(spec) = run {
            self.run_automation_now(&spec);
        }
        if let Some(spec) = toggle {
            self.toggle_automation_status(&spec);
        }
        if let Some(spec) = delete {
            self.delete_automation(&spec);
        }
        if !self.automation_runs.is_empty() {
            ui.separator();
            ui.label(RichText::new("Recent runs").strong().color(TEXT));
            for run in self.automation_runs.iter().take(8) {
                ui.label(
                    RichText::new(format!(
                        "{} · {} · {} · {}",
                        run.automation_name,
                        run.trigger,
                        run.status,
                        format_time_ms(run.started_ms)
                    ))
                    .color(MUTED)
                    .size(12.0),
                );
            }
        }
    }

    fn render_hermes(&mut self, ui: &mut Ui) {
        ui.heading("Hermes");
        ui.label(
            RichText::new("Harness lane for evolve, evaluation, and improvement loops.")
                .color(MUTED),
        );
        ui.add_space(10.0);
        Frame::default()
            .fill(PANEL)
            .stroke(Stroke::new(1.0, PANEL_HI))
            .rounding(10.0)
            .inner_margin(Margin::same(12.0))
            .show(ui, |ui| {
                ui.label(RichText::new("Evolve workflow").strong().color(TEXT));
                ui.add(
                    TextEdit::multiline(&mut self.evolve_goal)
                        .desired_rows(3)
                        .hint_text("Goal"),
                );
                ui.add(
                    TextEdit::singleline(&mut self.evolve_validation)
                        .hint_text("Validation command(s)"),
                );
                ui.horizontal(|ui| {
                    if ui.button("Start evolve").clicked() {
                        self.start_evolve();
                    }
                    if ui.button("Refresh diff context").clicked() {
                        self.git_snapshot = git_workspace_snapshot(&self.workspace);
                    }
                });
            });
        ui.add_space(10.0);
        feature_card(
            ui,
            "Evolve",
            "Generate proposal, run checks, compare, accept or reject.",
            true,
        );
        feature_card(
            ui,
            "Review loop",
            "Subagent review, fix, verify, and summarize.",
            false,
        );
        feature_card(
            ui,
            "Harness settings",
            "Switch prompts, tools, max steps, and provider policy.",
            true,
        );
        if ui.button("Switch to Hermes harness").clicked() {
            self.cfg.harness = "hermes".to_string();
            let _ = self.engine_tx.send(RuntimeCmd::Op(Op::SetHarness {
                id: "hermes".to_string(),
            }));
        }
    }

    fn render_settings_page(&mut self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            settings_tab(ui, &mut self.settings_tab, SettingsTab::General, "General");
            settings_tab(
                ui,
                &mut self.settings_tab,
                SettingsTab::Personalization,
                "Personalization",
            );
            settings_tab(
                ui,
                &mut self.settings_tab,
                SettingsTab::Appearance,
                "Appearance",
            );
            settings_tab(ui, &mut self.settings_tab, SettingsTab::Models, "Models");
            settings_tab(
                ui,
                &mut self.settings_tab,
                SettingsTab::Permissions,
                "Permissions",
            );
            settings_tab(
                ui,
                &mut self.settings_tab,
                SettingsTab::Automations,
                "Automations",
            );
            settings_tab(ui, &mut self.settings_tab, SettingsTab::Plugins, "Plugins");
            settings_tab(ui, &mut self.settings_tab, SettingsTab::Hermes, "Hermes");
            settings_tab(ui, &mut self.settings_tab, SettingsTab::Git, "Git");
            settings_tab(ui, &mut self.settings_tab, SettingsTab::Browser, "Browser");
            settings_tab(ui, &mut self.settings_tab, SettingsTab::Memory, "Memory");
            settings_tab(
                ui,
                &mut self.settings_tab,
                SettingsTab::Shortcuts,
                "Shortcuts",
            );
            settings_tab(
                ui,
                &mut self.settings_tab,
                SettingsTab::Advanced,
                "Advanced",
            );
        });
        ui.separator();
        match self.settings_tab {
            SettingsTab::General => self.settings_general(ui),
            SettingsTab::Personalization => self.settings_personalization(ui),
            SettingsTab::Appearance => self.settings_appearance(ui),
            SettingsTab::Models => self.settings_models(ui),
            SettingsTab::Permissions => self.settings_permissions(ui),
            SettingsTab::Automations => self.settings_automations(ui),
            SettingsTab::Plugins => self.settings_plugins(ui),
            SettingsTab::Hermes => self.settings_hermes(ui),
            SettingsTab::Git => self.settings_git(ui),
            SettingsTab::Browser => self.settings_browser(ui),
            SettingsTab::Memory => self.settings_memory(ui),
            SettingsTab::Shortcuts => self.settings_shortcuts(ui),
            SettingsTab::Advanced => self.settings_advanced(ui),
        }
    }

    fn settings_general(&mut self, ui: &mut Ui) {
        ui.heading("General");
        ui.checkbox(&mut self.cfg.persist, "Persist sessions");
        ui.checkbox(&mut self.cfg.resume, "Resume latest session");
        if ui
            .checkbox(&mut self.prevent_sleep, "Prevent sleep while running")
            .changed()
        {
            self.sync_sleep_guard();
            self.persist_desktop_preferences();
        }
        if ui
            .checkbox(
                &mut self.fold_tool_results,
                "Fold long tool and MCP results",
            )
            .changed()
        {
            self.persist_desktop_preferences();
        }
        ui.horizontal(|ui| {
            ui.label("Detail level");
            if ui
                .selectable_label(self.detail_level == DetailLevel::Default, "Default")
                .clicked()
            {
                self.detail_level = DetailLevel::Default;
                self.persist_desktop_preferences();
            }
            if ui
                .selectable_label(self.detail_level == DetailLevel::Coding, "Coding")
                .clicked()
            {
                self.detail_level = DetailLevel::Coding;
                self.persist_desktop_preferences();
            }
        });
        ui.horizontal(|ui| {
            ui.label("Workspace");
            ui.monospace(self.workspace.display().to_string());
        });
        ui.add(TextEdit::singleline(&mut self.workspace_input).hint_text("Workspace path"));
        ui.horizontal(|ui| {
            if ui.button("Open workspace").clicked() {
                self.open_workspace_from_input();
            }
            if ui.button("Use current").clicked() {
                self.workspace_input = self.workspace.display().to_string();
            }
        });
        if !self.workspace_message.trim().is_empty() {
            ui.label(
                RichText::new(&self.workspace_message)
                    .color(MUTED)
                    .size(12.0),
            );
        }
        if !self.recent_workspaces.is_empty() {
            ui.label(RichText::new("Recent workspaces").strong().color(TEXT));
            let recent = self.recent_workspaces.clone();
            for item in recent.iter().take(6) {
                if ui
                    .button(format!("{}  ·  {}", item.name, item.path))
                    .clicked()
                {
                    self.switch_workspace(PathBuf::from(&item.path));
                }
            }
        }
        if let Some(session) = self.selected_session_summary() {
            ui.separator();
            ui.label(RichText::new("Selected thread").strong().color(TEXT));
            ui.label(
                RichText::new(&session.id)
                    .monospace()
                    .size(11.5)
                    .color(MUTED),
            );
            if self.pending_session_context.is_some() {
                ui.label(
                    RichText::new("Resume context will attach to the next prompt")
                        .color(ACCENT)
                        .size(12.0),
                );
            }
            ui.add(TextEdit::singleline(&mut self.rename_session_title).hint_text("Thread title"));
            ui.horizontal(|ui| {
                if ui.button("Rename").clicked() {
                    self.rename_selected_session();
                }
                if ui
                    .button(if session.pinned { "Unpin" } else { "Pin" })
                    .clicked()
                {
                    self.toggle_selected_session_pin();
                }
                if ui
                    .button(if session.archived {
                        "Restore"
                    } else {
                        "Archive"
                    })
                    .clicked()
                {
                    self.toggle_selected_session_archive();
                }
            });
        }
        if ui.button("Apply").clicked() {
            self.reconfigure();
        }
    }

    fn settings_personalization(&mut self, ui: &mut Ui) {
        ui.heading("Personalization");
        ui.horizontal(|ui| {
            ui.label("Tone");
            let current = personalization_tone_id(&self.personalization_tone);
            if ui
                .selectable_label(current == "friendly", "Friendly")
                .clicked()
            {
                self.personalization_tone = "friendly".to_string();
                self.persist_desktop_preferences();
            }
            if ui.selectable_label(current == "direct", "Direct").clicked() {
                self.personalization_tone = "direct".to_string();
                self.persist_desktop_preferences();
            }
        });
        if ui
            .add(
                TextEdit::multiline(&mut self.custom_instructions)
                    .desired_rows(5)
                    .hint_text("Custom instructions"),
            )
            .changed()
        {
            self.persist_desktop_preferences();
        }
        if ui.button("Clear custom instructions").clicked() {
            self.custom_instructions.clear();
            self.persist_desktop_preferences();
        }
    }

    fn settings_appearance(&mut self, ui: &mut Ui) {
        ui.heading("Appearance");
        if ui
            .checkbox(&mut self.motion_enabled, "Enable motion")
            .changed()
        {
            self.persist_desktop_preferences();
        }
        if ui
            .checkbox(&mut self.compact_sidebar, "Compact sidebar")
            .changed()
        {
            self.persist_desktop_preferences();
        }
        ui.label(RichText::new("Dark Codex-like theme is active. Reduced motion is respected by the animation loop.").color(MUTED));
    }

    fn settings_models(&mut self, ui: &mut Ui) {
        ui.heading("Models");
        if self.streaming {
            ui.label(
                RichText::new("Model settings are locked while a turn is running.")
                    .color(FAINT)
                    .size(12.0),
            );
        }
        ui.add_enabled_ui(!self.streaming, |ui| {
            self.render_model_picker(ui);
            self.render_effort_picker(ui);
            if ui.checkbox(&mut self.cfg.fast_mode, "Fast mode").changed() && self.cfg.fast_mode {
                self.cfg.reasoning_effort = "low".to_string();
                if let Some(preset) = fast_model_for(&self.cfg.provider) {
                    self.cfg.model = preset.model.to_string();
                }
            }
            if ui.button("Apply model settings").clicked() {
                if self.cfg.fast_mode {
                    self.cfg.reasoning_effort = "low".to_string();
                    if let Some(preset) = fast_model_for(&self.cfg.provider) {
                        self.cfg.model = preset.model.to_string();
                    }
                }
                self.reconfigure();
            }
        });
        if self.streaming && ui.button("Open usage").clicked() {
            self.inspector = InspectorTab::Usage;
        }
    }

    fn settings_permissions(&mut self, ui: &mut Ui) {
        ui.heading("Permissions");
        ui.horizontal(|ui| {
            ui.label("Approval");
            if ui
                .selectable_label(
                    matches!(self.cfg.approval_policy, ApprovalPolicy::Never),
                    "Full access",
                )
                .clicked()
            {
                self.cfg.approval_policy = ApprovalPolicy::Never;
            }
            if ui
                .selectable_label(
                    matches!(self.cfg.approval_policy, ApprovalPolicy::OnRequest),
                    "Ask first",
                )
                .clicked()
            {
                self.cfg.approval_policy = ApprovalPolicy::OnRequest;
            }
            if ui
                .selectable_label(
                    matches!(self.cfg.approval_policy, ApprovalPolicy::Always),
                    "Always ask",
                )
                .clicked()
            {
                self.cfg.approval_policy = ApprovalPolicy::Always;
            }
        });
        ui.horizontal(|ui| {
            ui.label("Sandbox");
            if ui
                .selectable_label(
                    matches!(self.cfg.sandbox, SandboxPolicy::ReadOnly),
                    "Read only",
                )
                .clicked()
            {
                self.cfg.sandbox = SandboxPolicy::ReadOnly;
            }
            if ui
                .selectable_label(
                    matches!(self.cfg.sandbox, SandboxPolicy::WorkspaceWrite),
                    "Workspace write",
                )
                .clicked()
            {
                self.cfg.sandbox = SandboxPolicy::WorkspaceWrite;
            }
            if ui
                .selectable_label(
                    matches!(self.cfg.sandbox, SandboxPolicy::DangerFullAccess),
                    "Danger full access",
                )
                .clicked()
            {
                self.cfg.sandbox = SandboxPolicy::DangerFullAccess;
            }
        });
        if ui.button("Apply permissions").clicked() {
            self.reconfigure();
        }
    }

    fn settings_automations(&mut self, ui: &mut Ui) {
        ui.heading("Automations");
        ui.label(
            RichText::new("Local automation drafts can be run immediately or kept as schedules.")
                .color(MUTED),
        );
        ui.add_space(8.0);
        Frame::default()
            .fill(PANEL)
            .stroke(Stroke::new(1.0, PANEL_HI))
            .rounding(10.0)
            .inner_margin(Margin::same(12.0))
            .show(ui, |ui| {
                ui.label(RichText::new("Automation defaults").strong().color(TEXT));
                ui.add(
                    TextEdit::singleline(&mut self.automation_schedule)
                        .hint_text("Default schedule"),
                );
                ui.add(
                    TextEdit::multiline(&mut self.automation_prompt)
                        .desired_rows(3)
                        .hint_text("Default prompt"),
                );
                ui.horizontal(|ui| {
                    if ui.button("Save automation").clicked() {
                        self.create_automation();
                    }
                    if ui.button("Refresh automations").clicked() {
                        self.automations =
                            read_automation_specs(&self.workspace).unwrap_or_default();
                    }
                });
            });
        ui.add_space(10.0);
        self.render_automation_list(ui);
    }

    fn settings_hermes(&mut self, ui: &mut Ui) {
        ui.heading("Hermes");
        ui.label(
            RichText::new("Evolve profiles, review gates, and validation defaults.").color(MUTED),
        );
        ui.add_space(8.0);
        Frame::default()
            .fill(PANEL)
            .stroke(Stroke::new(1.0, PANEL_HI))
            .rounding(10.0)
            .inner_margin(Margin::same(12.0))
            .show(ui, |ui| {
                ui.label(RichText::new("Evolve profile").strong().color(TEXT));
                ui.add(
                    TextEdit::singleline(&mut self.hermes_profile_name).hint_text("Profile name"),
                );
                ui.add(
                    TextEdit::multiline(&mut self.evolve_goal)
                        .desired_rows(3)
                        .hint_text("Goal"),
                );
                ui.add(
                    TextEdit::singleline(&mut self.evolve_validation)
                        .hint_text("Validation command(s)"),
                );
                ui.add(
                    TextEdit::multiline(&mut self.hermes_review_prompt)
                        .desired_rows(3)
                        .hint_text("Review gate"),
                );
                ui.horizontal(|ui| {
                    if ui.button("Save profile").clicked() {
                        self.save_hermes_profile();
                    }
                    if ui.button("Start evolve").clicked() {
                        self.start_evolve();
                    }
                    if ui.button("Queue review").clicked() {
                        self.queue_hermes_review();
                    }
                });
                if !self.hermes_message.trim().is_empty() {
                    ui.label(RichText::new(&self.hermes_message).color(MUTED).size(12.0));
                }
            });
        ui.add_space(10.0);
        ui.label(RichText::new("Saved profiles").strong().color(TEXT));
        if self.hermes_profiles.is_empty() {
            ui.label(RichText::new("No Hermes profiles saved yet").color(FAINT));
        }
        let profiles = self.hermes_profiles.clone();
        let mut apply: Option<HermesProfile> = None;
        let mut run: Option<HermesProfile> = None;
        let mut delete: Option<HermesProfile> = None;
        for profile in profiles {
            Frame::default()
                .fill(Color32::from_rgb(24, 24, 28))
                .stroke(Stroke::new(1.0, PANEL_HI))
                .rounding(8.0)
                .inner_margin(Margin::same(9.0))
                .show(ui, |ui| {
                    ui.label(RichText::new(&profile.name).strong().color(TEXT));
                    ui.label(RichText::new(&profile.goal).color(MUTED).size(12.0));
                    ui.label(
                        RichText::new(&profile.validation)
                            .monospace()
                            .color(FAINT)
                            .size(11.5),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Apply").clicked() {
                            apply = Some(profile.clone());
                        }
                        if ui.button("Run").clicked() {
                            run = Some(profile.clone());
                        }
                        if ui.button("Delete").clicked() {
                            delete = Some(profile.clone());
                        }
                    });
                });
            ui.add_space(6.0);
        }
        if let Some(profile) = apply {
            self.apply_hermes_profile(&profile);
        }
        if let Some(profile) = run {
            self.run_hermes_profile(&profile);
        }
        if let Some(profile) = delete {
            self.delete_hermes_profile_ui(&profile);
        }
    }

    fn settings_plugins(&mut self, ui: &mut Ui) {
        ui.heading("Plugins");
        ui.label(
            RichText::new(
                "MCP servers are saved into the project oxide.toml and reloaded by the engine.",
            )
            .color(MUTED),
        );
        ui.label(
            RichText::new("Saving rewrites the project config as a full TOML file.")
                .color(FAINT)
                .size(12.0),
        );
        ui.add_space(8.0);
        Frame::default()
            .fill(PANEL)
            .stroke(Stroke::new(1.0, PANEL_HI))
            .rounding(10.0)
            .inner_margin(Margin::same(12.0))
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Add or update MCP server")
                        .strong()
                        .color(TEXT),
                );
                ui.add(TextEdit::singleline(&mut self.mcp_name).hint_text("Name, e.g. fs"));
                ui.add(TextEdit::singleline(&mut self.mcp_command).hint_text("Command, e.g. npx"));
                ui.add(
                    TextEdit::singleline(&mut self.mcp_args)
                        .hint_text("Args as shell words or JSON array"),
                );
                ui.horizontal(|ui| {
                    if ui.button("Save MCP server").clicked() {
                        self.save_mcp_server_from_form();
                    }
                    if ui.button("Reset form").clicked() {
                        self.mcp_name = "fs".to_string();
                        self.mcp_command = "npx".to_string();
                        self.mcp_args = "-y @modelcontextprotocol/server-filesystem .".to_string();
                        self.mcp_message.clear();
                    }
                });
                if !self.mcp_message.trim().is_empty() {
                    ui.label(RichText::new(&self.mcp_message).color(MUTED).size(12.0));
                }
            });
        ui.add_space(10.0);
        self.render_mcp_server_list(ui, true);
    }

    fn settings_git(&mut self, ui: &mut Ui) {
        ui.heading("Git");
        ui.label(
            RichText::new("Branch and worktree controls for isolated agent runs.").color(MUTED),
        );
        ui.add_space(8.0);
        self.render_git_controls(ui);
    }

    fn settings_browser(&mut self, ui: &mut Ui) {
        ui.heading("Browser");
        ui.label(
            RichText::new("Appshots are stored locally and can be injected into prompts.")
                .color(MUTED),
        );
        ui.add_space(8.0);
        self.render_appshots_panel(ui);
    }

    fn settings_memory(&mut self, ui: &mut Ui) {
        ui.heading("Memory");
        ui.label(
            RichText::new("Project rules and local notes can be attached to future prompts.")
                .color(MUTED),
        );
        if ui
            .checkbox(
                &mut self.attach_memory_to_prompt,
                "Attach memory context to prompts",
            )
            .changed()
        {
            self.persist_desktop_preferences();
        }
        ui.horizontal(|ui| {
            ui.label("Context budget");
            ui.monospace(format!("{} tokens", self.cfg.max_context_tokens));
        });
        if ui.button("Refresh rules and notes").clicked() {
            self.memories = read_memory_specs(&self.workspace).unwrap_or_default();
            self.project_rules = read_project_rules(&self.workspace).unwrap_or_default();
        }
        ui.separator();
        ui.label(RichText::new("Project rules").strong().color(TEXT));
        if self.project_rules.trim().is_empty() {
            ui.label(RichText::new("No AGENTS.md found in this workspace").color(FAINT));
        } else {
            let mut rules = self.project_rules.clone();
            ui.add(
                TextEdit::multiline(&mut rules)
                    .desired_rows(8)
                    .font(FontId::monospace(12.0))
                    .interactive(false),
            );
        }
        ui.separator();
        Frame::default()
            .fill(PANEL)
            .stroke(Stroke::new(1.0, PANEL_HI))
            .rounding(10.0)
            .inner_margin(Margin::same(12.0))
            .show(ui, |ui| {
                ui.label(RichText::new("Add memory note").strong().color(TEXT));
                ui.add(TextEdit::singleline(&mut self.memory_title).hint_text("Title"));
                ui.add(
                    TextEdit::multiline(&mut self.memory_body)
                        .desired_rows(3)
                        .hint_text("Memory body"),
                );
                if ui.button("Save memory").clicked() {
                    self.create_memory_note();
                }
                if !self.memory_message.trim().is_empty() {
                    ui.label(RichText::new(&self.memory_message).color(MUTED).size(12.0));
                }
            });
        ui.add_space(10.0);
        ui.label(RichText::new("Saved memory notes").strong().color(TEXT));
        if self.memories.is_empty() {
            ui.label(RichText::new("No memory notes saved yet").color(FAINT));
        }
        let memories = self.memories.clone();
        let mut toggle: Option<MemorySpec> = None;
        let mut delete: Option<MemorySpec> = None;
        for memory in memories {
            Frame::default()
                .fill(Color32::from_rgb(24, 24, 28))
                .stroke(Stroke::new(1.0, PANEL_HI))
                .rounding(8.0)
                .inner_margin(Margin::same(9.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&memory.title).strong().color(TEXT));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                RichText::new(if memory.enabled {
                                    "enabled"
                                } else {
                                    "disabled"
                                })
                                .color(if memory.enabled { ACCENT } else { MUTED })
                                .size(12.0),
                            );
                        });
                    });
                    ui.label(RichText::new(&memory.body).color(MUTED).size(12.0));
                    ui.horizontal(|ui| {
                        if ui
                            .button(if memory.enabled { "Disable" } else { "Enable" })
                            .clicked()
                        {
                            toggle = Some(memory.clone());
                        }
                        if ui.button("Delete").clicked() {
                            delete = Some(memory.clone());
                        }
                    });
                });
            ui.add_space(6.0);
        }
        if let Some(memory) = toggle {
            self.toggle_memory_note(&memory);
        }
        if let Some(memory) = delete {
            self.delete_memory_note(&memory);
        }
    }

    fn settings_shortcuts(&mut self, ui: &mut Ui) {
        ui.heading("Shortcuts");
        ui.label(
            RichText::new("Keyboard shortcuts and command-center actions available in Oxide.")
                .color(MUTED),
        );
        ui.add(TextEdit::singleline(&mut self.shortcut_query).hint_text("Search shortcuts"));
        ui.horizontal(|ui| {
            if ui.button("Open palette").clicked() {
                self.show_palette = true;
            }
            if ui.button("Open terminal").clicked() {
                self.inspector = InspectorTab::Terminal;
            }
            if ui.button("Open settings").clicked() {
                self.show_settings = true;
                self.nav = NavSurface::Settings;
            }
        });
        ui.separator();
        let shortcuts = shortcut_catalog();
        let filtered = filter_shortcuts(&shortcuts, &self.shortcut_query);
        if filtered.is_empty() {
            ui.label(RichText::new("No matching shortcuts").color(FAINT));
        }
        for shortcut in filtered {
            Frame::default()
                .fill(Color32::from_rgb(24, 24, 28))
                .stroke(Stroke::new(1.0, PANEL_HI))
                .rounding(8.0)
                .inner_margin(Margin::same(9.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(shortcut.title).strong().color(TEXT));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                RichText::new(shortcut.keys)
                                    .monospace()
                                    .color(ACCENT)
                                    .size(12.0),
                            );
                        });
                    });
                    ui.label(RichText::new(shortcut.scope).color(MUTED).size(12.0));
                    ui.label(RichText::new(shortcut.detail).color(FAINT).size(12.0));
                });
            ui.add_space(6.0);
        }
    }

    fn settings_advanced(&mut self, ui: &mut Ui) {
        ui.heading("Advanced");
        ui.label(
            RichText::new("Runtime diagnostics, config preview, and provider readiness.")
                .color(MUTED),
        );
        ui.horizontal(|ui| {
            if ui.button("Refresh diagnostics").clicked() {
                self.diagnostics = collect_diagnostics(&self.cfg, &self.workspace);
            }
            if ui.button("Open terminal").clicked() {
                self.inspector = InspectorTab::Terminal;
            }
            if ui.button("Refresh git").clicked() {
                self.git_snapshot = git_workspace_snapshot(&self.workspace);
                self.git_branches = git_branch_snapshot(&self.workspace);
            }
        });
        ui.add(TextEdit::singleline(&mut self.diagnostics_filter).hint_text("Filter diagnostics"));
        ui.separator();
        let diagnostics = filter_diagnostics(&self.diagnostics, &self.diagnostics_filter);
        if diagnostics.is_empty() {
            ui.label(RichText::new("No diagnostics match the filter").color(FAINT));
        }
        for item in diagnostics {
            Frame::default()
                .fill(Color32::from_rgb(24, 24, 28))
                .stroke(Stroke::new(1.0, PANEL_HI))
                .rounding(8.0)
                .inner_margin(Margin::same(9.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&item.label).strong().color(TEXT));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                RichText::new(&item.status)
                                    .color(diagnostic_status_color(&item.status))
                                    .size(12.0),
                            );
                        });
                    });
                    ui.label(
                        RichText::new(&item.value)
                            .monospace()
                            .size(11.5)
                            .color(MUTED),
                    );
                });
            ui.add_space(6.0);
        }
        ui.separator();
        ui.label(RichText::new("Config preview").strong().color(TEXT));
        let mut preview = diagnostic_config_preview(&self.cfg);
        ui.add(
            TextEdit::multiline(&mut preview)
                .desired_rows(10)
                .font(FontId::monospace(12.0))
                .interactive(false),
        );
    }

    fn sync_sleep_guard(&mut self) {
        if self.prevent_sleep {
            self.start_sleep_guard();
        } else {
            self.stop_sleep_guard();
        }
    }

    fn start_sleep_guard(&mut self) {
        if self.sleep_guard.is_some() {
            return;
        }
        #[cfg(target_os = "macos")]
        let child = std::process::Command::new("caffeinate")
            .arg("-dimsu")
            .spawn();
        #[cfg(not(target_os = "macos"))]
        let child: Result<std::process::Child, std::io::Error> = Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "unsupported",
        ));

        match child {
            Ok(child) => {
                self.sleep_guard = Some(child);
                self.timeline.push(TimelineItem {
                    title: "Prevent sleep enabled".to_string(),
                    detail: "Desktop sleep guard is active".to_string(),
                    state: TimelineState::Done,
                    request_id: None,
                });
            }
            Err(e) => {
                self.prevent_sleep = false;
                self.timeline.push(TimelineItem {
                    title: "Prevent sleep failed".to_string(),
                    detail: e.to_string(),
                    state: TimelineState::Error,
                    request_id: None,
                });
            }
        }
    }

    fn stop_sleep_guard(&mut self) {
        if let Some(mut child) = self.sleep_guard.take() {
            let _ = child.kill();
            let _ = child.wait();
            self.timeline.push(TimelineItem {
                title: "Prevent sleep disabled".to_string(),
                detail: "Desktop sleep guard stopped".to_string(),
                state: TimelineState::Done,
                request_id: None,
            });
        }
    }

    fn render_command_palette(&mut self, ctx: &Context) {
        let mut open = self.show_palette;
        let mut close_palette = false;
        egui::Window::new("Command palette")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .fixed_size([520.0, 360.0])
            .show(ctx, |ui| {
                ui.add(
                    TextEdit::singleline(&mut self.palette_query)
                        .hint_text("Run command or search"),
                );
                ui.separator();
                let shortcuts = shortcut_catalog();
                let results = build_command_palette_results(CommandPaletteInputs {
                    search: GlobalSearchInputs {
                        workspace: &self.workspace,
                        sessions: &self.sessions,
                        memories: &self.memories,
                        automations: &self.automations,
                        appshots: &self.appshots,
                        hermes_profiles: &self.hermes_profiles,
                        mcp_servers: &self.cfg.mcp_servers,
                        shortcuts: &shortcuts,
                        goal_mode_enabled: self.goal_mode_enabled,
                        active_goal: &self.active_goal,
                        goal_success_criteria: &self.goal_success_criteria,
                        query: &self.palette_query,
                    },
                    repo_index: &self.repo_index,
                    has_selected_session: self.selected_session.is_some(),
                    has_pending_browser_snapshot: self.pending_browser_snapshot.is_some(),
                    has_selected_git_file: self.selected_git_file.is_some(),
                    has_active_terminal_job: self.active_terminal_job.is_some(),
                });
                if results.is_empty() {
                    ui.label(RichText::new("No matching results").color(FAINT));
                }
                egui::ScrollArea::vertical()
                    .max_height(286.0)
                    .show(ui, |ui| {
                        for item in results.into_iter().take(80) {
                            let title = format!("{} · {}", item.kind, item.title);
                            if palette_button(ui, &title, &item.detail).clicked() {
                                self.open_search_result(&item);
                                close_palette = true;
                            }
                        }
                    });
            });
        self.show_palette = open && !close_palette;
    }

    fn render_settings_window(&mut self, ctx: &Context) {
        let mut open = self.show_settings;
        egui::Window::new("Settings")
            .open(&mut open)
            .resizable(true)
            .default_size([720.0, 560.0])
            .show(ctx, |ui| self.render_settings_page(ui));
        self.show_settings = open;
    }
}

fn spawn_engine(
    config: Config,
) -> (
    tokio_mpsc::UnboundedSender<RuntimeCmd>,
    mpsc::Receiver<Event>,
) {
    let (cmd_tx, mut cmd_rx) = tokio_mpsc::unbounded_channel::<RuntimeCmd>();
    let (event_tx, event_rx) = mpsc::channel::<Event>();
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                let _ = event_tx.send(Event::Error {
                    message: format!("runtime: {e}"),
                });
                return;
            }
        };
        rt.block_on(async move {
            let mut current = config;
            let mut handle = None;
            let mut forwarder: Option<tokio::task::JoinHandle<()>> = None;

            loop {
                if handle.is_none() {
                    match oxide_core::spawn(current.clone()) {
                        Ok((h, mut events)) => {
                            let tx = event_tx.clone();
                            forwarder = Some(tokio::spawn(async move {
                                while let Some(event) = events.recv().await {
                                    if tx.send(event).is_err() {
                                        break;
                                    }
                                }
                            }));
                            handle = Some(h);
                        }
                        Err(e) => {
                            let _ = event_tx.send(Event::Error {
                                message: format!("engine: {e}"),
                            });
                        }
                    }
                }

                match cmd_rx.recv().await {
                    Some(RuntimeCmd::Op(op)) => {
                        let shutdown = matches!(op, Op::Shutdown);
                        if let Some(h) = &handle {
                            let _ = h.submit(op).await;
                        }
                        if shutdown {
                            break;
                        }
                    }
                    Some(RuntimeCmd::Reconfigure(next)) => {
                        if let Some(task) = forwarder.take() {
                            task.abort();
                        }
                        if let Some(h) = handle.take() {
                            let _ = h.submit(Op::Shutdown).await;
                        }
                        current = next;
                    }
                    None => break,
                }
            }
        });
    });
    (cmd_tx, event_rx)
}

fn install_style(ctx: &Context) {
    use egui::{Rounding, TextStyle};

    let mut style = (*ctx.style()).clone();
    let mut visuals = Visuals::dark();

    // ── Colours ─────────────────────────────────────────────
    visuals.panel_fill = PANEL;
    visuals.window_fill = Color32::from_rgb(28, 28, 32);
    visuals.extreme_bg_color = BG;
    visuals.widgets.noninteractive.bg_fill = PANEL;
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(38, 38, 44);
    visuals.widgets.hovered.bg_fill = PANEL_HI;
    visuals.widgets.active.bg_fill = Color32::from_rgb(52, 52, 60);
    visuals.selection.bg_fill = Color32::from_rgb(46, 96, 78);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.hyperlink_color = ACCENT;

    // Softer separators / strokes so panels read calmer.
    let hairline = Color32::from_rgb(40, 40, 46);
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, hairline);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(48, 48, 55));
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(60, 60, 68));
    visuals.window_stroke = Stroke::new(1.0, Color32::from_rgb(48, 48, 55));

    // ── Rounded corners everywhere (kills the boxy look) ────
    let r = Rounding::same(9.0);
    visuals.widgets.noninteractive.rounding = r;
    visuals.widgets.inactive.rounding = r;
    visuals.widgets.hovered.rounding = r;
    visuals.widgets.active.rounding = r;
    visuals.widgets.open.rounding = r;
    visuals.window_rounding = Rounding::same(13.0);
    visuals.menu_rounding = Rounding::same(11.0);

    style.visuals = visuals;

    // ── Breathing room ──────────────────────────────────────
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 7.0);
    style.spacing.menu_margin = Margin::same(8.0);
    style.spacing.window_margin = Margin::same(12.0);
    style.spacing.interact_size.y = 30.0;
    style.spacing.indent = 18.0;

    // ── Typography ──────────────────────────────────────────
    style
        .text_styles
        .insert(TextStyle::Body, FontId::proportional(14.5));
    style
        .text_styles
        .insert(TextStyle::Button, FontId::proportional(14.0));
    style
        .text_styles
        .insert(TextStyle::Heading, FontId::proportional(23.0));
    style
        .text_styles
        .insert(TextStyle::Small, FontId::proportional(12.0));
    style
        .text_styles
        .insert(TextStyle::Monospace, FontId::monospace(12.5));

    ctx.set_style(style);
}

fn workspace_of(config: &Config) -> PathBuf {
    config
        .workspace
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn project_name(path: &Path) -> String {
    path.file_name()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "workspace".to_string())
}

fn selected_model(config: &Config) -> Option<&'static ModelPreset> {
    MODELS
        .iter()
        .find(|m| {
            m.provider == config.provider && m.model == config.model && m.fast == config.fast_mode
        })
        .or_else(|| {
            MODELS
                .iter()
                .find(|m| m.provider == config.provider && m.model == config.model)
        })
}

fn display_model(config: &Config) -> String {
    selected_model(config)
        .map(|m| m.label.to_string())
        .unwrap_or_else(|| {
            if config.model.is_empty() {
                config.provider.clone()
            } else {
                config.model.clone()
            }
        })
}

fn status_line(config: &Config, context_window: Option<u64>) -> String {
    let window = context_window
        .map(|w| format!(" · {}k", w / 1000))
        .unwrap_or_default();
    format!(
        "{} · {} · {}{}",
        config.provider,
        display_model(config),
        effort_label(&config.reasoning_effort),
        window
    )
}

fn effort_label(value: &str) -> &'static str {
    EFFORTS
        .iter()
        .find(|e| e.value == value)
        .map(|e| e.label)
        .unwrap_or("Medium")
}

fn fast_model_for(provider: &str) -> Option<&'static ModelPreset> {
    MODELS.iter().find(|m| m.provider == provider && m.fast)
}

fn model_matches(model: &ModelPreset, query: &str) -> bool {
    query.is_empty()
        || model.provider.contains(query)
        || model.model.contains(query)
        || model.label.to_ascii_lowercase().contains(query)
        || model.provider_label.to_ascii_lowercase().contains(query)
        || model.summary.to_ascii_lowercase().contains(query)
        || model.badge.to_ascii_lowercase().contains(query)
}

fn panel_frame() -> Frame {
    Frame::default()
        .fill(PANEL)
        .stroke(Stroke::new(1.0, Color32::from_rgb(43, 43, 49)))
}

fn section_text(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).color(FAINT).size(12.0).strong()
}

fn nav_button(ui: &mut Ui, text: &str, selected: bool) -> egui::Response {
    let rich = if selected {
        RichText::new(text).color(TEXT).strong()
    } else {
        RichText::new(text).color(MUTED)
    };
    ui.add_sized(
        [ui.available_width(), 34.0],
        egui::Button::new(rich).fill(if selected {
            PANEL_HI
        } else {
            Color32::TRANSPARENT
        }),
    )
}

fn project_row(ui: &mut Ui, text: &str, selected: bool) -> egui::Response {
    ui.add_sized(
        [ui.available_width(), 28.0],
        egui::Button::new(if selected {
            RichText::new(format!("folder  {text}"))
                .color(TEXT)
                .strong()
        } else {
            RichText::new(format!("folder  {text}")).color(MUTED)
        })
        .fill(if selected {
            Color32::from_rgb(38, 38, 44)
        } else {
            Color32::TRANSPARENT
        }),
    )
}

fn thread_button(ui: &mut Ui, text: &str, selected: bool) -> egui::Response {
    ui.add_sized(
        [ui.available_width(), 27.0],
        egui::Button::new(if selected {
            RichText::new(text).color(TEXT).strong()
        } else {
            RichText::new(text).color(MUTED)
        })
        .fill(if selected {
            Color32::from_rgb(38, 38, 44)
        } else {
            Color32::TRANSPARENT
        }),
    )
}

fn motion_phase(seconds: f32) -> f32 {
    seconds.rem_euclid(1.0)
}

fn timeline_state_fill(state: TimelineState, seconds: f32, motion: bool) -> Color32 {
    let base = match state {
        TimelineState::Running => Color32::from_rgb(36, 40, 44),
        TimelineState::Done => PANEL,
        TimelineState::Waiting => Color32::from_rgb(45, 39, 30),
        TimelineState::Error => Color32::from_rgb(50, 30, 30),
    };
    if !motion {
        return base;
    }
    let phase = motion_phase(seconds);
    let pulse = if phase < 0.5 {
        phase * 2.0
    } else {
        (1.0 - phase) * 2.0
    };
    match state {
        TimelineState::Running => lerp_color(base, Color32::from_rgb(43, 65, 57), pulse),
        TimelineState::Waiting => lerp_color(base, Color32::from_rgb(58, 48, 31), pulse),
        TimelineState::Error => lerp_color(base, Color32::from_rgb(68, 35, 35), pulse),
        TimelineState::Done => base,
    }
}

fn lerp_color(from: Color32, to: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let channel = |a: u8, b: u8| -> u8 { (a as f32 + (b as f32 - a as f32) * t).round() as u8 };
    Color32::from_rgb(
        channel(from.r(), to.r()),
        channel(from.g(), to.g()),
        channel(from.b(), to.b()),
    )
}

fn message_display_text(text: &str, is_streaming_tail: bool, seconds: f32, motion: bool) -> String {
    if !is_streaming_tail {
        return text.to_string();
    }
    if !motion || motion_phase(seconds) < 0.5 {
        format!("{text} |")
    } else {
        text.to_string()
    }
}

fn render_message(ui: &mut Ui, msg: &ChatMsg, is_streaming_tail: bool, t: f32, motion: bool) {
    let fill = match msg.kind {
        MsgKind::User => Color32::from_rgb(45, 45, 52),
        MsgKind::Agent => BG,
        MsgKind::Note => Color32::from_rgb(36, 36, 42),
    };
    Frame::default()
        .fill(fill)
        .rounding(12.0)
        .inner_margin(Margin::same(12.0))
        .show(ui, |ui| {
            let label = match msg.kind {
                MsgKind::User => "You",
                MsgKind::Agent => "Oxide",
                MsgKind::Note => "Note",
            };
            ui.label(RichText::new(label).color(FAINT).size(12.0).strong());
            ui.label(
                RichText::new(message_display_text(
                    &msg.text,
                    is_streaming_tail,
                    t,
                    motion,
                ))
                .color(TEXT)
                .size(14.5),
            );
        });
    ui.add_space(8.0);
}

fn render_typing(ui: &mut Ui, t: f32) {
    let dots = match ((t * 3.0) as usize) % 3 {
        0 => ".",
        1 => "..",
        _ => "...",
    };
    ui.label(RichText::new(format!("Oxide is working{dots}")).color(MUTED));
}

fn quick_actions(ui: &mut Ui, prompt: &mut String) {
    for text in [
        "Build the missing Codex-like feature surface",
        "Review this repository and propose next implementation slice",
        "Run Hermes evolve on the current workspace",
    ] {
        if ui.button(text).clicked() {
            *prompt = text.to_string();
        }
    }
}

fn permission_button(ui: &mut Ui, config: &mut Config) {
    let full = matches!(config.approval_policy, ApprovalPolicy::Never);
    let text = if full { "Full access" } else { "Ask first" };
    if toggle_pill(ui, text, full, DANGER).clicked() {
        config.approval_policy = if full {
            ApprovalPolicy::OnRequest
        } else {
            ApprovalPolicy::Never
        };
    }
}

fn toggle_pill(ui: &mut Ui, text: &str, on: bool, color: Color32) -> egui::Response {
    ui.add(
        egui::Button::new(if on {
            RichText::new(text).color(color).strong()
        } else {
            RichText::new(text).color(MUTED)
        })
        .fill(if on {
            Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 20)
        } else {
            Color32::TRANSPARENT
        })
        .stroke(Stroke::new(1.0, if on { color } else { PANEL_HI })),
    )
}

fn send_button(disabled: bool) -> egui::Button<'static> {
    let label = if disabled { "Queue" } else { "Send" };
    egui::Button::new(
        RichText::new(label)
            .color(if disabled { ACCENT } else { BG })
            .strong(),
    )
    .fill(if disabled {
        Color32::TRANSPARENT
    } else {
        ACCENT
    })
    .stroke(Stroke::new(1.0, if disabled { ACCENT } else { ACCENT }))
}

fn inspector_tab(ui: &mut Ui, tab: &mut InspectorTab, value: InspectorTab, label: &str) {
    if ui.selectable_label(*tab == value, label).clicked() {
        *tab = value;
    }
}

fn settings_tab(ui: &mut Ui, tab: &mut SettingsTab, value: SettingsTab, label: &str) {
    if ui.selectable_label(*tab == value, label).clicked() {
        *tab = value;
    }
}

fn feature_card(ui: &mut Ui, title: &str, body: &str, available: bool) {
    Frame::default()
        .fill(PANEL)
        .stroke(Stroke::new(1.0, PANEL_HI))
        .rounding(10.0)
        .inner_margin(Margin::same(12.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).strong().color(TEXT));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(if available { "wired" } else { "planned" })
                            .color(if available { ACCENT } else { MUTED }),
                    );
                });
            });
            ui.label(RichText::new(body).color(MUTED));
        });
    ui.add_space(8.0);
}

fn diagnostic_status_color(status: &str) -> Color32 {
    match status {
        "ok" | "found" | "present" | "configured" | "active" => ACCENT,
        "missing" => DANGER,
        _ => MUTED,
    }
}

fn palette_button(ui: &mut Ui, title: &str, subtitle: &str) -> egui::Response {
    Frame::default()
        .fill(Color32::TRANSPARENT)
        .inner_margin(Margin::same(7.0))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(title).strong().color(TEXT));
                ui.label(RichText::new(subtitle).color(MUTED).size(12.0));
            })
            .response
        })
        .response
        .interact(Sense::click())
}

fn command_catalog() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "command-palette",
            title: "Open command palette",
            detail: "Run commands and navigate surfaces.",
            keys: "Cmd+K",
            scope: "Global",
        },
        CommandSpec {
            id: "new-chat",
            title: "New chat",
            detail: "Clear transcript and focus composer.",
            keys: "Palette",
            scope: "Chat",
        },
        CommandSpec {
            id: "search-threads",
            title: "Search threads",
            detail: "Open thread search and archive scope.",
            keys: "Palette",
            scope: "Navigation",
        },
        CommandSpec {
            id: "workspace-switch",
            title: "Switch workspace",
            detail: "Open recent workspace and path controls.",
            keys: "Palette",
            scope: "Global",
        },
        CommandSpec {
            id: "settings",
            title: "Open settings",
            detail: "Open the full settings surface.",
            keys: "Cmd+,",
            scope: "Global",
        },
        CommandSpec {
            id: "settings-personalization",
            title: "Personalization settings",
            detail: "Open tone and custom instruction settings.",
            keys: "Palette",
            scope: "Settings",
        },
        CommandSpec {
            id: "settings-plugins",
            title: "Configure MCP servers",
            detail: "Open plugin connector settings.",
            keys: "Palette",
            scope: "Settings",
        },
        CommandSpec {
            id: "settings-memory",
            title: "Memory settings",
            detail: "Open project rules and memory notes.",
            keys: "Palette",
            scope: "Settings",
        },
        CommandSpec {
            id: "settings-automations",
            title: "Automation settings",
            detail: "Open schedules and run-now actions.",
            keys: "Palette",
            scope: "Settings",
        },
        CommandSpec {
            id: "settings-hermes",
            title: "Hermes settings",
            detail: "Open evolve profiles and review gates.",
            keys: "Palette",
            scope: "Settings",
        },
        CommandSpec {
            id: "settings-advanced",
            title: "Advanced diagnostics",
            detail: "Open runtime doctor and config preview.",
            keys: "Palette",
            scope: "Settings",
        },
        CommandSpec {
            id: "inspector-terminal",
            title: "Toggle terminal",
            detail: "Open right terminal panel.",
            keys: "Palette",
            scope: "Inspector",
        },
        CommandSpec {
            id: "inspector-goal",
            title: "Open goal mode",
            detail: "Set a durable objective and success criteria for future turns.",
            keys: "Palette",
            scope: "Goal",
        },
        CommandSpec {
            id: "inspector-approvals",
            title: "Open approvals",
            detail: "Review pending tool approval requests.",
            keys: "Palette",
            scope: "Inspector",
        },
        CommandSpec {
            id: "inspector-checkpoints",
            title: "Open checkpoints",
            detail: "Review and rewind workspace checkpoints.",
            keys: "Palette",
            scope: "Inspector",
        },
        CommandSpec {
            id: "inspector-usage",
            title: "Open usage",
            detail: "Inspect token usage, context window, and compactions.",
            keys: "Palette",
            scope: "Inspector",
        },
        CommandSpec {
            id: "git-refresh-diff",
            title: "Refresh git diff",
            detail: "Update workspace status and diff preview.",
            keys: "Palette",
            scope: "Git",
        },
        CommandSpec {
            id: "git-stage-selected-file",
            title: "Stage selected file",
            detail: "Stage the file currently selected in the diff inspector.",
            keys: "Palette",
            scope: "Git",
        },
        CommandSpec {
            id: "hermes-evolve",
            title: "Hermes evolve",
            detail: "Open future evolve workflow lane.",
            keys: "Palette",
            scope: "Hermes",
        },
        CommandSpec {
            id: "hermes-start-evolve",
            title: "Start Hermes evolve",
            detail: "Run evolve with the current goal and validation contract.",
            keys: "Palette",
            scope: "Hermes",
        },
        CommandSpec {
            id: "appshot-capture-screen",
            title: "Capture screen appshot",
            detail: "Capture local visual evidence for the next prompt.",
            keys: "Palette",
            scope: "Browser",
        },
        CommandSpec {
            id: "browser-open-target",
            title: "Open browser target",
            detail: "Open the current browser target URL externally and log it.",
            keys: "Palette",
            scope: "Browser",
        },
        CommandSpec {
            id: "browser-insert-context",
            title: "Insert browser context",
            detail: "Attach the current browser target and action log to the prompt.",
            keys: "Palette",
            scope: "Browser",
        },
        CommandSpec {
            id: "browser-capture-pending-snapshot",
            title: "Capture pending browser snapshot",
            detail: "Capture the latest agent-requested browser snapshot draft.",
            keys: "Palette",
            scope: "Browser",
        },
        CommandSpec {
            id: "terminal-stop-running-command",
            title: "Stop running terminal command",
            detail: "Stop the active terminal process if one is running.",
            keys: "Palette",
            scope: "Terminal",
        },
        CommandSpec {
            id: "thread-rename-selected",
            title: "Rename selected thread",
            detail: "Apply the title field from settings.",
            keys: "Palette",
            scope: "Thread",
        },
        CommandSpec {
            id: "thread-pin-selected",
            title: "Pin selected thread",
            detail: "Toggle pinned status.",
            keys: "Palette",
            scope: "Thread",
        },
        CommandSpec {
            id: "thread-archive-selected",
            title: "Archive selected thread",
            detail: "Toggle archived status.",
            keys: "Palette",
            scope: "Thread",
        },
    ]
}

fn command_visible_for_state(
    command_id: &str,
    has_selected_session: bool,
    has_pending_browser_snapshot: bool,
    has_selected_git_file: bool,
    has_active_terminal_job: bool,
) -> bool {
    match command_id {
        "thread-rename-selected" | "thread-pin-selected" | "thread-archive-selected" => {
            has_selected_session
        }
        "browser-capture-pending-snapshot" => has_pending_browser_snapshot,
        "git-stage-selected-file" => has_selected_git_file,
        "terminal-stop-running-command" => has_active_terminal_job,
        _ => true,
    }
}

fn upsert_pending_approval(approvals: &mut Vec<PendingApproval>, approval: PendingApproval) {
    if let Some(existing) = approvals
        .iter_mut()
        .find(|item| item.request_id == approval.request_id)
    {
        *existing = approval;
        return;
    }
    approvals.push(approval);
}

fn remove_pending_approval(approvals: &mut Vec<PendingApproval>, request_id: u64) {
    approvals.retain(|approval| approval.request_id != request_id);
}

fn clear_timeline_approval_request(timeline: &mut [TimelineItem], request_id: u64) {
    for item in timeline {
        if item.request_id == Some(request_id) {
            item.request_id = None;
        }
    }
}

fn upsert_checkpoint(checkpoints: &mut Vec<WorkspaceCheckpoint>, checkpoint: WorkspaceCheckpoint) {
    if let Some(existing) = checkpoints.iter_mut().find(|item| item.id == checkpoint.id) {
        *existing = checkpoint;
        return;
    }
    checkpoints.push(checkpoint);
}

fn mark_checkpoint_rewound(checkpoints: &mut [WorkspaceCheckpoint], id: u64, restored_files: u64) {
    for checkpoint in checkpoints {
        if checkpoint.id >= id {
            checkpoint.rewound = true;
            checkpoint.restored_files = if checkpoint.id == id {
                Some(restored_files)
            } else {
                None
            };
        }
    }
}

fn record_token_usage(
    records: &mut Vec<TokenUsageRecord>,
    turn: u64,
    input: u64,
    output: u64,
    created_ms: u64,
) {
    records.push(TokenUsageRecord {
        turn,
        input,
        output,
        created_ms,
    });
}

fn record_compaction(
    records: &mut Vec<CompactionRecord>,
    dropped: u64,
    tokens: u64,
    created_ms: u64,
) {
    records.push(CompactionRecord {
        dropped,
        tokens,
        created_ms,
    });
}

fn latest_token_usage_summary(records: &[TokenUsageRecord]) -> Option<String> {
    records.last().map(format_token_usage_record)
}

fn format_token_usage_record(record: &TokenUsageRecord) -> String {
    format!(
        "turn-{} · input {} · output {} · total {}",
        record.turn,
        record.input,
        record.output,
        record.input + record.output
    )
}

fn latest_compaction_tokens(records: &[CompactionRecord]) -> Option<u64> {
    records.last().map(|record| record.tokens)
}

fn latest_context_tokens(
    usage: &[TokenUsageRecord],
    compactions: &[CompactionRecord],
) -> Option<u64> {
    match (usage.last(), compactions.last()) {
        (Some(usage), Some(compaction)) if usage.created_ms >= compaction.created_ms => {
            Some(usage.input)
        }
        (Some(_), Some(compaction)) => Some(compaction.tokens),
        (Some(usage), None) => Some(usage.input),
        (None, Some(_)) => latest_compaction_tokens(compactions),
        (None, None) => None,
    }
}

fn context_usage_percent(limit: Option<u64>, used: Option<u64>) -> Option<f32> {
    let limit = limit?;
    let used = used?;
    if limit == 0 {
        return None;
    }
    Some((used as f32 / limit as f32) * 100.0)
}

fn compact_number(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}m", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}k", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

fn goal_chip_text(goal: &str) -> String {
    format!("Goal: {}", truncate_title(goal.trim()))
}

fn settings_tab_for_command(command_id: &str) -> Option<SettingsTab> {
    match command_id {
        "settings-personalization" => Some(SettingsTab::Personalization),
        "settings-plugins" => Some(SettingsTab::Plugins),
        "settings-memory" => Some(SettingsTab::Memory),
        "settings-automations" => Some(SettingsTab::Automations),
        "settings-hermes" => Some(SettingsTab::Hermes),
        "settings-advanced" => Some(SettingsTab::Advanced),
        _ => None,
    }
}

fn settings_command_opens_window(command_id: &str) -> bool {
    command_id == "settings" || settings_tab_for_command(command_id).is_some()
}

#[cfg(test)]
fn filter_commands(commands: &[CommandSpec], query: &str) -> Vec<CommandSpec> {
    let query = normalize_query(query);
    commands
        .iter()
        .filter(|item| {
            query.is_empty()
                || normalize_query(item.id).contains(&query)
                || normalize_query(item.title).contains(&query)
                || normalize_query(item.detail).contains(&query)
                || normalize_query(item.keys).contains(&query)
                || normalize_query(item.scope).contains(&query)
        })
        .cloned()
        .collect()
}

fn command_to_search_result(command: &CommandSpec) -> SearchResultItem {
    SearchResultItem {
        kind: "Command".to_string(),
        title: command.title.to_string(),
        detail: format!("{} · {} · {}", command.keys, command.scope, command.detail),
        target: format!("command:{}", command.id),
    }
}

fn search_result_visible_for_palette(
    item: &SearchResultItem,
    has_selected_session: bool,
    has_pending_browser_snapshot: bool,
    has_selected_git_file: bool,
    has_active_terminal_job: bool,
) -> bool {
    let Some(command_id) = item.target.strip_prefix("command:") else {
        return true;
    };
    command_id != "command-palette"
        && command_visible_for_state(
            command_id,
            has_selected_session,
            has_pending_browser_snapshot,
            has_selected_git_file,
            has_active_terminal_job,
        )
}

fn build_command_palette_results(input: CommandPaletteInputs<'_>) -> Vec<SearchResultItem> {
    let has_selected_session = input.has_selected_session;
    let has_pending_browser_snapshot = input.has_pending_browser_snapshot;
    let has_selected_git_file = input.has_selected_git_file;
    let has_active_terminal_job = input.has_active_terminal_job;

    if input.search.query.trim().is_empty() {
        return command_catalog()
            .into_iter()
            .filter(|command| command.id != "command-palette")
            .filter(|command| {
                command_visible_for_state(
                    command.id,
                    has_selected_session,
                    has_pending_browser_snapshot,
                    has_selected_git_file,
                    has_active_terminal_job,
                )
            })
            .map(|command| command_to_search_result(&command))
            .collect();
    }

    build_global_search_results_with_repo_index(input.search, input.repo_index)
        .into_iter()
        .filter(|item| {
            search_result_visible_for_palette(
                item,
                has_selected_session,
                has_pending_browser_snapshot,
                has_selected_git_file,
                has_active_terminal_job,
            )
        })
        .collect()
}

fn shortcut_catalog() -> Vec<ShortcutSpec> {
    vec![
        ShortcutSpec {
            id: "command-palette",
            title: "Open command palette",
            keys: "Cmd+K",
            scope: "Global",
            detail: "Run commands and navigate surfaces.",
        },
        ShortcutSpec {
            id: "settings",
            title: "Open settings",
            keys: "Cmd+,",
            scope: "Global",
            detail: "Open the full settings surface.",
        },
        ShortcutSpec {
            id: "workspace-switch",
            title: "Switch workspace",
            keys: "Palette",
            scope: "Global",
            detail: "Open recent workspace and path controls.",
        },
        ShortcutSpec {
            id: "send-prompt",
            title: "Send or queue prompt",
            keys: "Enter",
            scope: "Composer",
            detail: "Send immediately, or queue while the agent is working.",
        },
        ShortcutSpec {
            id: "new-line",
            title: "Insert prompt newline",
            keys: "Shift+Enter",
            scope: "Composer",
            detail: "Keep editing a multiline prompt.",
        },
        ShortcutSpec {
            id: "terminal-run",
            title: "Run terminal command",
            keys: "Enter",
            scope: "Terminal",
            detail: "Run the command typed in the terminal panel.",
        },
        ShortcutSpec {
            id: "approval-approve",
            title: "Approve tool request",
            keys: "Button",
            scope: "Approvals",
            detail: "Review and approve, approve for this session, or reject a pending request.",
        },
        ShortcutSpec {
            id: "checkpoint-rewind",
            title: "Rewind checkpoint",
            keys: "Button",
            scope: "Checkpoints",
            detail: "Restore workspace files to a prior checkpoint.",
        },
        ShortcutSpec {
            id: "usage-open",
            title: "Open usage",
            keys: "Palette",
            scope: "Usage",
            detail: "Inspect token usage, context window, and compactions.",
        },
        ShortcutSpec {
            id: "goal-open",
            title: "Open goal mode",
            keys: "Palette",
            scope: "Goal",
            detail: "Set and monitor the durable objective for future turns.",
        },
    ]
}

fn filter_shortcuts(shortcuts: &[ShortcutSpec], query: &str) -> Vec<ShortcutSpec> {
    let query = normalize_query(query);
    shortcuts
        .iter()
        .filter(|item| {
            query.is_empty()
                || normalize_query(item.title).contains(&query)
                || normalize_query(item.keys).contains(&query)
                || normalize_query(item.scope).contains(&query)
                || normalize_query(item.detail).contains(&query)
        })
        .cloned()
        .collect()
}

fn shortcut_search_target(shortcut_id: &str) -> &'static str {
    match shortcut_id {
        "goal-open" => "command:inspector-goal",
        "usage-open" => "command:inspector-usage",
        _ => "settings:shortcuts",
    }
}

#[cfg(test)]
fn build_global_search_results(input: GlobalSearchInputs<'_>) -> Vec<SearchResultItem> {
    let repo_index = collect_repo_index(input.workspace, REPO_INDEX_ENTRY_LIMIT);
    build_global_search_results_with_repo_index(input, &repo_index)
}

fn build_global_search_results_with_repo_index(
    input: GlobalSearchInputs<'_>,
    repo_index: &[RepoIndexEntry],
) -> Vec<SearchResultItem> {
    let query = input.query.trim().to_ascii_lowercase();
    let mut results = Vec::new();

    push_search_result(
        &mut results,
        &query,
        SearchResultItem {
            kind: "Workspace".to_string(),
            title: project_name(input.workspace),
            detail: input.workspace.display().to_string(),
            target: format!("file:{}", input.workspace.display()),
        },
    );

    for session in input.sessions {
        push_search_result(
            &mut results,
            &query,
            SearchResultItem {
                kind: "Thread".to_string(),
                title: session.title.clone(),
                detail: format!("{} message(s) · {}", session.message_count, session.id),
                target: format!("session:{}", session.id),
            },
        );
    }

    for entry in repo_index
        .iter()
        .filter(|entry| query.is_empty() || repo_index_entry_search_item_matches(entry, &query))
        .take(160)
    {
        let kind = if entry.is_dir { "Directory" } else { "File" };
        results.push(SearchResultItem {
            kind: kind.to_string(),
            title: entry.relative.clone(),
            detail: entry.path.display().to_string(),
            target: format!("file:{}", entry.path.display()),
        });
    }

    for memory in input.memories {
        push_search_result(
            &mut results,
            &query,
            SearchResultItem {
                kind: "Memory".to_string(),
                title: memory.title.clone(),
                detail: memory.body.clone(),
                target: "settings:memory".to_string(),
            },
        );
    }

    for automation in input.automations {
        push_search_result(
            &mut results,
            &query,
            SearchResultItem {
                kind: "Automation".to_string(),
                title: automation.name.clone(),
                detail: format!(
                    "{} · {} · {}",
                    automation.status, automation.schedule, automation.prompt
                ),
                target: format!("automation:{}", automation.id),
            },
        );
        push_search_result(
            &mut results,
            &query,
            SearchResultItem {
                kind: "Command".to_string(),
                title: format!("Run automation: {}", automation.name),
                detail: format!(
                    "{} · {} · {}",
                    automation.status, automation.schedule, automation.prompt
                ),
                target: format!("automation-run:{}", automation.id),
            },
        );
    }

    for appshot in input.appshots {
        push_search_result(
            &mut results,
            &query,
            SearchResultItem {
                kind: "Appshot".to_string(),
                title: appshot.title.clone(),
                detail: format!("{} · {}", appshot.path, appshot.note),
                target: format!("appshot:{}", appshot.id),
            },
        );
    }

    for profile in input.hermes_profiles {
        push_search_result(
            &mut results,
            &query,
            SearchResultItem {
                kind: "Hermes".to_string(),
                title: profile.name.clone(),
                detail: format!(
                    "{} · {} · {}",
                    profile.goal, profile.validation, profile.review_prompt
                ),
                target: format!("hermes:{}", profile.id),
            },
        );
    }

    for server in input.mcp_servers {
        push_search_result(
            &mut results,
            &query,
            SearchResultItem {
                kind: "MCP".to_string(),
                title: server.name.clone(),
                detail: format!("{} {}", server.command, server.args.join(" ")),
                target: "settings:plugins".to_string(),
            },
        );
    }

    if input.goal_mode_enabled && !input.active_goal.trim().is_empty() {
        push_search_result(
            &mut results,
            &query,
            SearchResultItem {
                kind: "Goal".to_string(),
                title: "Goal Mode".to_string(),
                detail: format!(
                    "{} · {}",
                    input.active_goal.trim(),
                    empty_label(input.goal_success_criteria)
                ),
                target: "goal:active".to_string(),
            },
        );
    }

    for command in command_catalog() {
        push_search_result(&mut results, &query, command_to_search_result(&command));
    }

    for shortcut in input.shortcuts {
        push_search_result(
            &mut results,
            &query,
            SearchResultItem {
                kind: "Shortcut".to_string(),
                title: shortcut.title.to_string(),
                detail: format!(
                    "{} · {} · {}",
                    shortcut.keys, shortcut.scope, shortcut.detail
                ),
                target: shortcut_search_target(shortcut.id).to_string(),
            },
        );
    }

    results
}

fn push_search_result(results: &mut Vec<SearchResultItem>, query: &str, item: SearchResultItem) {
    if query.is_empty() || search_item_matches(&item, query) {
        results.push(item);
    }
}

fn search_item_matches(item: &SearchResultItem, query: &str) -> bool {
    let haystack = format!(
        "{} {} {} {}",
        item.kind, item.title, item.detail, item.target
    )
    .to_ascii_lowercase();
    let query = query.to_ascii_lowercase();
    if haystack.contains(&query) {
        return true;
    }
    query
        .split_whitespace()
        .filter(|part| !part.is_empty())
        .all(|part| haystack.contains(part))
}

#[cfg(test)]
fn palette_action_matches(title: &str, subtitle: &str, query: &str) -> bool {
    let query = normalize_query(query);
    query.is_empty()
        || normalize_query(title).contains(&query)
        || normalize_query(subtitle).contains(&query)
}

fn normalize_query(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn provider_binary_for(provider: &str) -> Option<&'static str> {
    match provider {
        "codex" => Some("codex"),
        "claude" => Some("claude"),
        _ => None,
    }
}

fn provider_auth_hint(
    provider: &str,
    openai_key_present: bool,
    anthropic_key_present: bool,
    gemini_key_present: bool,
    xai_key_present: bool,
    deepseek_key_present: bool,
    mistral_key_present: bool,
) -> (&'static str, &'static str) {
    match provider {
        "openai" => (
            "OPENAI_API_KEY",
            if openai_key_present {
                "present"
            } else {
                "missing"
            },
        ),
        "anthropic" => (
            "ANTHROPIC_API_KEY",
            if anthropic_key_present {
                "present"
            } else {
                "missing"
            },
        ),
        "gemini" => (
            "GEMINI_API_KEY",
            if gemini_key_present {
                "present"
            } else {
                "missing"
            },
        ),
        "xai" => (
            "XAI_API_KEY",
            if xai_key_present {
                "present"
            } else {
                "missing"
            },
        ),
        "deepseek" => (
            "DEEPSEEK_API_KEY",
            if deepseek_key_present {
                "present"
            } else {
                "missing"
            },
        ),
        "mistral" => (
            "MISTRAL_API_KEY",
            if mistral_key_present {
                "present"
            } else {
                "missing"
            },
        ),
        "codex" => ("codex CLI", "required"),
        "claude" => ("claude CLI", "required"),
        _ => ("none", "not required"),
    }
}

fn secret_value_present(value: Option<&str>) -> bool {
    value.map(|v| !v.trim().is_empty()).unwrap_or(false)
}

fn env_key_present(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .as_deref()
        .map(|value| secret_value_present(Some(value)))
        .unwrap_or(false)
}

fn diagnostic_config_preview(config: &Config) -> String {
    format!(
        "provider = {}\nmodel = {}\nreasoning_effort = {}\nfast_mode = {}\nharness = {}\nworkspace = {}\nmax_context_tokens = {}\npersist = {}\nresume = {}\nmcp_servers = {}",
        config.provider,
        config.effective_model(),
        config.reasoning_effort,
        config.fast_mode,
        config.harness,
        workspace_of(config).display(),
        config.max_context_tokens,
        config.persist,
        config.resume,
        config.mcp_servers.len()
    )
}

fn collect_diagnostics(config: &Config, workspace: &Path) -> Vec<DiagnosticItem> {
    let mut items = Vec::new();
    items.push(DiagnosticItem {
        label: "Workspace".to_string(),
        value: workspace.display().to_string(),
        status: if workspace.exists() { "ok" } else { "missing" }.to_string(),
    });
    items.push(DiagnosticItem {
        label: "Project config".to_string(),
        value: workspace.join("oxide.toml").display().to_string(),
        status: if workspace.join("oxide.toml").exists() {
            "ok"
        } else {
            "missing"
        }
        .to_string(),
    });
    items.push(DiagnosticItem {
        label: "Provider".to_string(),
        value: format!("{} / {}", config.provider, display_model(config)),
        status: "active".to_string(),
    });
    let (auth_label, auth_status) = provider_auth_hint(
        &config.provider,
        env_key_present("OPENAI_API_KEY"),
        env_key_present("ANTHROPIC_API_KEY"),
        env_key_present("GEMINI_API_KEY"),
        env_key_present("XAI_API_KEY"),
        env_key_present("DEEPSEEK_API_KEY"),
        env_key_present("MISTRAL_API_KEY"),
    );
    items.push(DiagnosticItem {
        label: auth_label.to_string(),
        value: auth_status.to_string(),
        status: auth_status.to_string(),
    });
    if let Some(binary) = provider_binary_for(&config.provider) {
        let found = command_exists(binary);
        items.push(DiagnosticItem {
            label: format!("{binary} binary"),
            value: binary.to_string(),
            status: if found { "found" } else { "missing" }.to_string(),
        });
    }
    items.push(DiagnosticItem {
        label: "git binary".to_string(),
        value: "git".to_string(),
        status: if command_exists("git") {
            "found"
        } else {
            "missing"
        }
        .to_string(),
    });
    items.push(DiagnosticItem {
        label: "MCP servers".to_string(),
        value: config.mcp_servers.len().to_string(),
        status: if config.mcp_servers.is_empty() {
            "none"
        } else {
            "configured"
        }
        .to_string(),
    });
    items
}

fn filter_diagnostics(items: &[DiagnosticItem], query: &str) -> Vec<DiagnosticItem> {
    let query = query.trim().to_ascii_lowercase();
    items
        .iter()
        .filter(|item| {
            query.is_empty()
                || item.label.to_ascii_lowercase().contains(&query)
                || item.value.to_ascii_lowercase().contains(&query)
                || item.status.to_ascii_lowercase().contains(&query)
        })
        .cloned()
        .collect()
}

fn command_exists(binary: &str) -> bool {
    std::process::Command::new(binary)
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn read_children(path: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(path)
        .ok()
        .into_iter()
        .flat_map(|rd| rd.flatten().map(|e| e.path()))
        .filter(|p| p.file_name().map(|n| n != "target").unwrap_or(true))
        .collect();
    entries.sort_by(|a, b| {
        b.is_dir()
            .cmp(&a.is_dir())
            .then(a.file_name().cmp(&b.file_name()))
    });
    entries
}

fn repo_index_ignored_name(name: &str) -> bool {
    matches!(
        name,
        "target" | ".git" | ".oxide" | "node_modules" | ".next" | "dist" | "build" | ".DS_Store"
    )
}

fn relative_path_label(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

fn collect_repo_index(workspace: &Path, limit: usize) -> Vec<RepoIndexEntry> {
    let mut entries = Vec::new();
    let mut queue = VecDeque::from([(workspace.to_path_buf(), 0usize)]);
    while let Some((dir, depth)) = queue.pop_front() {
        for path in read_children(&dir) {
            if entries.len() >= limit {
                return entries;
            }
            let name = path
                .file_name()
                .map(|value| value.to_string_lossy().to_string())
                .unwrap_or_default();
            if repo_index_ignored_name(&name) {
                continue;
            }
            let is_dir = path.is_dir();
            let relative = relative_path_label(workspace, &path);
            entries.push(RepoIndexEntry {
                path: path.clone(),
                relative,
                is_dir,
            });
            if is_dir && depth < REPO_INDEX_MAX_DEPTH {
                queue.push_back((path, depth + 1));
            }
        }
    }
    entries
}

fn repo_index_entry_matches_query(entry: &RepoIndexEntry, query: &str) -> bool {
    let query = normalize_query(query);
    if query.is_empty() {
        return true;
    }
    normalize_query(&entry.relative).contains(&query)
        || file_entry_matches_query(&entry.path, &query)
}

fn repo_index_entry_search_item_matches(entry: &RepoIndexEntry, query: &str) -> bool {
    let kind = if entry.is_dir { "Directory" } else { "File" };
    search_item_matches(
        &SearchResultItem {
            kind: kind.to_string(),
            title: entry.relative.clone(),
            detail: entry.path.display().to_string(),
            target: format!("file:{}", entry.path.display()),
        },
        query,
    )
}

fn context_file_suggestions(
    repo_index: &[RepoIndexEntry],
    query: &str,
    limit: usize,
) -> Vec<RepoIndexEntry> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    repo_index
        .iter()
        .filter(|entry| !entry.is_dir && repo_index_entry_matches_query(entry, query))
        .cloned()
        .take(limit)
        .collect()
}

fn trim_terminal(lines: &mut VecDeque<String>) {
    while lines.len() > 240 {
        lines.pop_front();
    }
}

#[derive(Deserialize)]
struct SessionLine {
    role: String,
    content: String,
    ts_ms: u128,
}

#[cfg(test)]
fn read_session_summaries(workspace: &Path) -> std::io::Result<Vec<SessionSummary>> {
    let dir = workspace.join(".oxide/sessions");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let meta = read_session_meta_map(workspace).unwrap_or_default();
    let mut sessions = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        let text = std::fs::read_to_string(&path)?;
        let lines: Vec<SessionLine> = text
            .lines()
            .filter_map(|line| serde_json::from_str::<SessionLine>(line).ok())
            .collect();
        let title = lines
            .iter()
            .find(|line| line.role == "user")
            .map(|line| truncate_title(&line.content))
            .unwrap_or_else(|| {
                path.file_stem()
                    .map(|stem| stem.to_string_lossy().to_string())
                    .unwrap_or_else(|| "Untitled session".to_string())
            });
        let id = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
            .unwrap_or_else(|| title.clone());
        let session_meta = meta.get(&id);
        let title = session_meta
            .and_then(|m| m.title.clone())
            .filter(|t| !t.trim().is_empty())
            .unwrap_or(title);
        let pinned = session_meta.map(|m| m.pinned).unwrap_or(false);
        let archived = session_meta.map(|m| m.archived).unwrap_or(false);
        if archived {
            continue;
        }
        let last_ts_ms = lines.iter().map(|line| line.ts_ms).max().unwrap_or(0);
        sessions.push(SessionSummary {
            id,
            title,
            path,
            message_count: lines.len(),
            last_ts_ms,
            pinned,
            archived,
        });
    }
    sessions.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then_with(|| b.last_ts_ms.cmp(&a.last_ts_ms))
            .then_with(|| b.id.cmp(&a.id))
    });
    Ok(sessions)
}

fn read_all_session_summaries(workspace: &Path) -> std::io::Result<Vec<SessionSummary>> {
    let dir = workspace.join(".oxide/sessions");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let meta = read_session_meta_map(workspace).unwrap_or_default();
    let mut sessions = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        let text = std::fs::read_to_string(&path)?;
        let lines: Vec<SessionLine> = text
            .lines()
            .filter_map(|line| serde_json::from_str::<SessionLine>(line).ok())
            .collect();
        let fallback_title = lines
            .iter()
            .find(|line| line.role == "user")
            .map(|line| truncate_title(&line.content))
            .unwrap_or_else(|| {
                path.file_stem()
                    .map(|stem| stem.to_string_lossy().to_string())
                    .unwrap_or_else(|| "Untitled session".to_string())
            });
        let id = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
            .unwrap_or_else(|| fallback_title.clone());
        let session_meta = meta.get(&id);
        let title = session_meta
            .and_then(|m| m.title.clone())
            .filter(|t| !t.trim().is_empty())
            .unwrap_or(fallback_title);
        sessions.push(SessionSummary {
            id,
            title,
            path,
            message_count: lines.len(),
            last_ts_ms: lines.iter().map(|line| line.ts_ms).max().unwrap_or(0),
            pinned: session_meta.map(|m| m.pinned).unwrap_or(false),
            archived: session_meta.map(|m| m.archived).unwrap_or(false),
        });
    }
    sessions.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then_with(|| b.last_ts_ms.cmp(&a.last_ts_ms))
            .then_with(|| b.id.cmp(&a.id))
    });
    Ok(sessions)
}

fn read_session_meta_map(workspace: &Path) -> anyhow::Result<BTreeMap<String, SessionMeta>> {
    let dir = workspace.join(".oxide/session-meta");
    if !dir.exists() {
        return Ok(BTreeMap::new());
    }
    let mut map = BTreeMap::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let text = std::fs::read_to_string(path)?;
        let meta = toml::from_str::<SessionMeta>(&text)?;
        map.insert(meta.id.clone(), meta);
    }
    Ok(map)
}

fn write_session_meta(workspace: &Path, meta: &SessionMeta) -> anyhow::Result<()> {
    let safe_id = safe_session_meta_id(&meta.id)?;
    let dir = workspace.join(".oxide/session-meta");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{safe_id}.toml"));
    let text = toml::to_string_pretty(meta)?;
    std::fs::write(path, text)?;
    Ok(())
}

fn session_meta_from_summary(session: &SessionSummary) -> SessionMeta {
    SessionMeta {
        id: session.id.clone(),
        title: Some(session.title.clone()),
        pinned: session.pinned,
        archived: session.archived,
        updated_ms: now_ms(),
    }
}

fn filter_sessions(
    sessions: &[SessionSummary],
    query: &str,
    archived_scope: bool,
) -> Vec<SessionSummary> {
    let query = query.trim().to_ascii_lowercase();
    sessions
        .iter()
        .filter(|session| session.archived == archived_scope)
        .filter(|session| {
            query.is_empty()
                || session.id.to_ascii_lowercase().contains(&query)
                || session.title.to_ascii_lowercase().contains(&query)
        })
        .cloned()
        .collect()
}

fn safe_session_meta_id(id: &str) -> anyhow::Result<String> {
    let safe = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if safe.is_empty() {
        anyhow::bail!("session id is empty");
    }
    Ok(safe)
}

fn load_session_chat(path: &Path) -> std::io::Result<Vec<ChatMsg>> {
    let text = std::fs::read_to_string(path)?;
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str::<SessionLine>(line).ok())
        .filter_map(|line| {
            let kind = match line.role.as_str() {
                "user" => MsgKind::User,
                "assistant" => MsgKind::Agent,
                "system" => return None,
                _ => MsgKind::Note,
            };
            Some(ChatMsg {
                kind,
                text: line.content,
            })
        })
        .collect())
}

fn git_workspace_snapshot(workspace: &Path) -> GitSnapshot {
    let status = run_git(workspace, &["status", "--short"]).unwrap_or_else(|e| e);
    let is_repo = !status.contains("not a git repository")
        && !status.contains("not a git worktree")
        && !status.contains("fatal:");
    if !is_repo {
        return GitSnapshot {
            status: if status.trim().is_empty() {
                "not a git repository".to_string()
            } else {
                status
            },
            diff_stat: String::new(),
            raw_diff: String::new(),
            changed_files: Vec::new(),
        };
    }
    let changed_files = parse_git_changed_files(&status);
    GitSnapshot {
        status: if status.trim().is_empty() {
            "clean".to_string()
        } else {
            status
        },
        diff_stat: run_git(workspace, &["diff", "--stat"]).unwrap_or_default(),
        raw_diff: truncate_diff(&run_git(workspace, &["diff"]).unwrap_or_default()),
        changed_files,
    }
}

fn parse_git_changed_files(status: &str) -> Vec<GitChangedFile> {
    status
        .lines()
        .filter_map(|line| {
            if line.trim().is_empty() || line.contains("fatal:") {
                return None;
            }
            let status = line.chars().take(2).collect::<String>();
            let status = if status.trim().is_empty() {
                "?".to_string()
            } else {
                status.trim().to_string()
            };
            let raw_path = line.chars().skip(2).collect::<String>();
            let display_path = raw_path.trim().to_string();
            if display_path.is_empty() {
                return None;
            }
            let path = git_status_target_path(&display_path);
            Some(GitChangedFile {
                status,
                path,
                display_path,
            })
        })
        .collect()
}

fn git_status_target_path(display_path: &str) -> String {
    if let Some((_, target)) = display_path.rsplit_once(" -> ") {
        return target.trim().to_string();
    }
    if let Some(target) = display_path.split('\t').next_back() {
        return target.trim().to_string();
    }
    display_path.to_string()
}

fn build_git_review_summary(snapshot: &GitSnapshot) -> String {
    let mut summary = String::from("Git review summary\n\n");
    summary.push_str("Status:\n");
    summary.push_str(snapshot.status.trim());
    summary.push('\n');
    if !snapshot.diff_stat.trim().is_empty() {
        summary.push_str("\nDiff stat:\n");
        summary.push_str(snapshot.diff_stat.trim());
        summary.push('\n');
    }
    if !snapshot.changed_files.is_empty() {
        summary.push_str("\nChanged files:\n");
        for file in &snapshot.changed_files {
            summary.push_str(&format!("- {} {}\n", file.status, file.display_path));
        }
    }
    summary
}

fn read_git_diff_for_path(workspace: &Path, file: &GitChangedFile) -> Result<String, String> {
    let cached = run_git(workspace, &["diff", "--cached", "--", &file.path]).unwrap_or_default();
    let working = run_git(workspace, &["diff", "--", &file.path]).unwrap_or_default();
    let mut chunks = Vec::new();
    if !cached.trim().is_empty() {
        chunks.push(format!("Staged diff:\n{}", cached.trim_end()));
    }
    if !working.trim().is_empty() {
        chunks.push(format!("Working diff:\n{}", working.trim_end()));
    }
    if chunks.is_empty() && file.status == "??" {
        let path = workspace.join(&file.path);
        if path.is_file() {
            let context = read_file_context(&path, FILE_CONTEXT_CHAR_LIMIT)
                .map_err(|e| format!("cannot read untracked file {}: {e}", path.display()))?;
            chunks.push(format!("Untracked file preview:\n{context}"));
        }
    }
    if chunks.is_empty() {
        chunks.push(format!("No tracked diff found for {}", file.display_path));
    }
    Ok(truncate_diff(&chunks.join("\n\n")))
}

fn stage_git_file(workspace: &Path, file: &GitChangedFile) -> Result<String, String> {
    run_git(workspace, &["add", "--", &file.path])
}

fn unstage_git_file(workspace: &Path, file: &GitChangedFile) -> Result<String, String> {
    run_git(workspace, &["restore", "--staged", "--", &file.path]).or_else(|e| {
        if e.contains("could not resolve HEAD") || e.contains("fatal: ambiguous argument") {
            run_git(workspace, &["rm", "--cached", "-q", "--", &file.path])
        } else {
            Err(e)
        }
    })
}

fn commit_staged_changes(workspace: &Path, message: &str) -> Result<String, String> {
    let message = message.trim();
    if message.is_empty() {
        return Err("Commit message is required".to_string());
    }
    run_git(workspace, &["commit", "-m", message])
}

fn push_git_branch(workspace: &Path, remote: &str, branch: &str) -> Result<String, String> {
    let remote = remote.trim();
    if remote.is_empty() {
        return Err("Remote name is required".to_string());
    }
    let branch = branch.trim();
    if branch.is_empty() {
        return Err("Branch name is required".to_string());
    }
    run_git(workspace, &["push", "-u", remote, branch])
}

fn git_remote_url(workspace: &Path, remote: &str) -> Result<String, String> {
    let remote = remote.trim();
    if remote.is_empty() {
        return Err("Remote name is required".to_string());
    }
    run_git_stdout(workspace, &["remote", "get-url", remote]).map(|value| value.trim().to_string())
}

fn github_compare_url_for_remote(
    workspace: &Path,
    remote: &str,
    branch: &str,
    base: &str,
) -> Result<String, String> {
    let branch = branch.trim();
    if branch.is_empty() {
        return Err("Branch name is required".to_string());
    }
    let base = base.trim();
    if base.is_empty() {
        return Err("Base branch is required".to_string());
    }
    let remote_url = git_remote_url(workspace, remote)?;
    github_compare_url(&remote_url, branch, base)
        .ok_or_else(|| format!("Remote is not a supported GitHub URL: {remote_url}"))
}

fn github_compare_url(remote_url: &str, branch: &str, base: &str) -> Option<String> {
    let branch = branch.trim();
    let base = base.trim();
    if branch.is_empty() || base.is_empty() {
        return None;
    }
    let repo = github_repo_path(remote_url.trim())?;
    Some(format!(
        "https://github.com/{repo}/compare/{base}...{branch}?expand=1"
    ))
}

fn github_repo_path(remote_url: &str) -> Option<String> {
    let path = if let Some(path) = remote_url.strip_prefix("https://github.com/") {
        path
    } else if let Some(path) = remote_url.strip_prefix("git@github.com:") {
        path
    } else {
        return None;
    };
    let path = path.trim_end_matches(".git").trim_matches('/');
    let mut parts = path.split('/').filter(|part| !part.is_empty());
    let owner = parts.next()?;
    let repo = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

fn build_git_pr_draft(title: &str, body: &str, branch: &str, base: &str, summary: &str) -> String {
    let title = title.trim();
    let title = if title.is_empty() { "Draft PR" } else { title };
    let body = body.trim();
    let branch = branch.trim();
    let base = base.trim();
    let summary = summary.trim();
    let mut draft = format!(
        "# {title}\n\nBase: {}\nBranch: {}\n",
        empty_label(base),
        empty_label(branch)
    );
    if !body.is_empty() {
        draft.push_str("\nNotes:\n");
        draft.push_str(body);
        draft.push('\n');
    }
    if !summary.is_empty() {
        draft.push_str("\n");
        draft.push_str(summary);
        draft.push('\n');
    }
    draft
}

fn spawn_terminal_reader<R: Read + Send + 'static>(
    stream: R,
    stream_kind: TerminalStream,
    tx: mpsc::Sender<TerminalEvent>,
) {
    std::thread::spawn(move || {
        let reader = BufReader::new(stream);
        for line in reader.lines().map_while(Result::ok) {
            let _ = tx.send(TerminalEvent::Line(terminal_stream_line(
                stream_kind,
                &line,
            )));
        }
    });
}

fn spawn_terminal_watcher(
    id: u64,
    child: Arc<Mutex<std::process::Child>>,
    tx: mpsc::Sender<TerminalEvent>,
) {
    std::thread::spawn(move || loop {
        let status = {
            let Ok(mut child) = child.lock() else {
                let _ = tx.send(TerminalEvent::Finished { id, code: -1 });
                return;
            };
            child.try_wait()
        };
        match status {
            Ok(Some(status)) => {
                let _ = tx.send(TerminalEvent::Finished {
                    id,
                    code: status.code().unwrap_or(-1),
                });
                return;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(80)),
            Err(_) => {
                let _ = tx.send(TerminalEvent::Finished { id, code: -1 });
                return;
            }
        }
    });
}

fn terminal_stream_line(stream: TerminalStream, line: &str) -> String {
    match stream {
        TerminalStream::Stdout => line.to_string(),
        TerminalStream::Stderr => format!("stderr: {line}"),
    }
}

fn terminal_finished_line(code: i32) -> String {
    format!("(exit {code})")
}

fn git_branch_snapshot(workspace: &Path) -> GitBranchSnapshot {
    let branch = match run_git_stdout(workspace, &["branch", "--show-current"]) {
        Ok(value) => value,
        Err(e) => {
            return GitBranchSnapshot {
                current_branch: compact_line(&e),
                branches: Vec::new(),
                worktrees: Vec::new(),
            };
        }
    };
    let branches =
        run_git_stdout(workspace, &["branch", "--format=%(refname:short)"]).unwrap_or_default();
    let worktrees =
        run_git_stdout(workspace, &["worktree", "list", "--porcelain"]).unwrap_or_default();
    parse_git_branch_snapshot(&branch, &branches, &worktrees)
}

fn parse_git_branch_snapshot(
    current_branch: &str,
    branches: &str,
    worktrees: &str,
) -> GitBranchSnapshot {
    let branch = current_branch.trim().to_string();
    let branch_list = branches
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    GitBranchSnapshot {
        current_branch: branch,
        branches: branch_list,
        worktrees: parse_git_worktrees(worktrees),
    }
}

fn parse_git_worktrees(text: &str) -> Vec<GitWorktreeInfo> {
    text.split("\n\n")
        .filter_map(|block| {
            let mut path = String::new();
            let mut branch = String::from("(detached)");
            for line in block.lines() {
                if let Some(value) = line.strip_prefix("worktree ") {
                    path = value.trim().to_string();
                } else if let Some(value) = line.strip_prefix("branch ") {
                    branch = value
                        .trim()
                        .strip_prefix("refs/heads/")
                        .unwrap_or_else(|| value.trim())
                        .to_string();
                }
            }
            if path.is_empty() {
                None
            } else {
                Some(GitWorktreeInfo { path, branch })
            }
        })
        .collect()
}

fn run_git(workspace: &Path, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .output()
        .map_err(|e| format!("git failed: {e}"))?;
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    if output.status.success() {
        Ok(text)
    } else {
        Err(text)
    }
}

fn run_git_stdout(workspace: &Path, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .output()
        .map_err(|e| format!("git failed: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let mut text = String::from_utf8_lossy(&output.stderr).to_string();
        if text.trim().is_empty() {
            text = String::from_utf8_lossy(&output.stdout).to_string();
        }
        Err(text)
    }
}

fn save_project_config(config: &Config) -> anyhow::Result<()> {
    let workspace = workspace_of(config);
    std::fs::create_dir_all(&workspace)?;
    let path = workspace.join("oxide.toml");
    let text = toml::to_string_pretty(config)?;
    std::fs::write(path, text)?;
    Ok(())
}

fn desktop_state_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/oxide/desktop.toml"))
}

fn read_desktop_state() -> DesktopStateSpec {
    desktop_state_path()
        .and_then(|path| read_desktop_state_at(&path).ok())
        .unwrap_or_default()
}

fn write_desktop_state(state: &DesktopStateSpec) -> anyhow::Result<()> {
    let Some(path) = desktop_state_path() else {
        anyhow::bail!("HOME is not available for desktop state");
    };
    write_desktop_state_at(&path, state)
}

fn read_desktop_state_at(path: &Path) -> anyhow::Result<DesktopStateSpec> {
    if !path.exists() {
        return Ok(DesktopStateSpec::default());
    }
    let text = std::fs::read_to_string(path)?;
    Ok(toml::from_str::<DesktopStateSpec>(&text)?)
}

fn write_desktop_state_at(path: &Path, state: &DesktopStateSpec) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(state)?;
    std::fs::write(path, text)?;
    Ok(())
}

fn upsert_recent_workspace(
    existing: Vec<RecentWorkspaceSpec>,
    path: &Path,
    name: &str,
    opened_ms: u64,
    limit: usize,
) -> Vec<RecentWorkspaceSpec> {
    let path_text = path.display().to_string();
    let mut items = existing
        .into_iter()
        .filter(|item| item.path != path_text)
        .collect::<Vec<_>>();
    items.insert(
        0,
        RecentWorkspaceSpec {
            path: path_text,
            name: if name.trim().is_empty() {
                project_name(path)
            } else {
                name.trim().to_string()
            },
            last_opened_ms: opened_ms,
        },
    );
    items.truncate(limit.max(1));
    items
}

fn parse_mcp_args_input(input: &str) -> anyhow::Result<Vec<String>> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if trimmed.starts_with('[') {
        return serde_json::from_str::<Vec<String>>(trimmed)
            .map_err(|e| anyhow::anyhow!("invalid JSON args array: {e}"));
    }
    Ok(trimmed
        .split_whitespace()
        .filter(|part| !part.trim().is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

fn upsert_mcp_server(config: &mut Config, server: McpServerConfig) {
    config.mcp_servers.retain(|item| item.name != server.name);
    config.mcp_servers.push(server);
    config.mcp_servers.sort_by(|a, b| a.name.cmp(&b.name));
}

fn remove_mcp_server(config: &mut Config, name: &str) {
    config.mcp_servers.retain(|item| item.name != name);
}

fn configured_mcp_health(servers: &[McpServerConfig]) -> BTreeMap<String, McpServerHealth> {
    servers
        .iter()
        .map(|server| {
            (
                server.name.clone(),
                McpServerHealth {
                    name: server.name.clone(),
                    status: "configured".to_string(),
                    tool_count: 0,
                    tools: Vec::new(),
                    detail: "waiting for engine connection".to_string(),
                },
            )
        })
        .collect()
}

fn apply_mcp_status_event(
    health: &mut BTreeMap<String, McpServerHealth>,
    name: String,
    status: String,
    tool_count: usize,
    tools: Vec<String>,
    detail: String,
) {
    health.insert(
        name.clone(),
        McpServerHealth {
            name,
            status,
            tool_count,
            tools,
            detail,
        },
    );
}

fn mcp_health_for(
    health: &BTreeMap<String, McpServerHealth>,
    server: &McpServerConfig,
) -> McpServerHealth {
    health
        .get(&server.name)
        .cloned()
        .unwrap_or_else(|| McpServerHealth {
            name: server.name.clone(),
            status: "configured".to_string(),
            tool_count: 0,
            tools: Vec::new(),
            detail: "waiting for engine connection".to_string(),
        })
}

fn validate_mcp_server(server: &McpServerConfig) -> anyhow::Result<()> {
    if server.name.trim().is_empty() {
        anyhow::bail!("MCP server name is required");
    }
    if !server
        .name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        anyhow::bail!("MCP server name may only contain letters, numbers, '-' and '_'");
    }
    if server.command.trim().is_empty() {
        anyhow::bail!("MCP server command is required");
    }
    Ok(())
}

fn read_automation_specs(workspace: &Path) -> anyhow::Result<Vec<AutomationSpec>> {
    let dir = workspace.join(".oxide/automations");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut specs = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let text = std::fs::read_to_string(path)?;
        specs.push(toml::from_str::<AutomationSpec>(&text)?);
    }
    specs.sort_by(|a, b| {
        b.created_ms
            .cmp(&a.created_ms)
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(specs)
}

fn write_automation_spec(workspace: &Path, spec: &AutomationSpec) -> anyhow::Result<()> {
    let dir = workspace.join(".oxide/automations");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.toml", spec.id));
    let text = toml::to_string_pretty(spec)?;
    std::fs::write(path, text)?;
    Ok(())
}

fn delete_automation_spec(workspace: &Path, id: &str) -> anyhow::Result<()> {
    let path = workspace
        .join(".oxide/automations")
        .join(format!("{id}.toml"));
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

fn read_automation_run_specs(workspace: &Path) -> anyhow::Result<Vec<AutomationRunSpec>> {
    let dir = workspace.join(".oxide/automation-runs");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut runs = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let text = std::fs::read_to_string(path)?;
        runs.push(toml::from_str::<AutomationRunSpec>(&text)?);
    }
    runs.sort_by(|a, b| {
        b.started_ms
            .cmp(&a.started_ms)
            .then_with(|| a.automation_name.cmp(&b.automation_name))
    });
    Ok(runs)
}

fn write_automation_run_spec(workspace: &Path, run: &AutomationRunSpec) -> anyhow::Result<()> {
    let dir = workspace.join(".oxide/automation-runs");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.toml", run.id));
    let text = toml::to_string_pretty(run)?;
    std::fs::write(path, text)?;
    Ok(())
}

fn automation_with_toggled_status(spec: &AutomationSpec) -> AutomationSpec {
    let mut next = spec.clone();
    next.status = if spec.status == "ACTIVE" {
        "PAUSED".to_string()
    } else {
        "ACTIVE".to_string()
    };
    next
}

fn build_automation_run_prompt(spec: &AutomationSpec) -> String {
    format!(
        "Run automation now\n\nName: {}\nKind: {}\nSchedule: {}\nStatus: {}\n\nAutomation prompt:\n{}",
        spec.name, spec.kind, spec.schedule, spec.status, spec.prompt
    )
}

fn automation_run_from_spec(
    spec: &AutomationSpec,
    trigger: &str,
    status: &str,
    started_ms: u64,
) -> AutomationRunSpec {
    AutomationRunSpec {
        id: format!("{}-{}-{started_ms}", slug_fragment(&spec.name), trigger),
        automation_id: spec.id.clone(),
        automation_name: spec.name.clone(),
        trigger: trigger.to_string(),
        status: status.to_string(),
        prompt: spec.prompt.clone(),
        started_ms,
    }
}

fn automation_is_due(spec: &AutomationSpec, runs: &[AutomationRunSpec], now_ms: u64) -> bool {
    if spec.status != "ACTIVE" {
        return false;
    }
    let Some(interval_ms) = automation_interval_ms(&spec.schedule) else {
        return false;
    };
    let last_run = runs
        .iter()
        .filter(|run| run.automation_id == spec.id)
        .map(|run| run.started_ms)
        .max();
    let anchor = last_run.unwrap_or(spec.created_ms);
    now_ms >= anchor.saturating_add(interval_ms)
}

fn automation_interval_ms(schedule: &str) -> Option<u64> {
    let mut freq = None;
    let mut interval = 1u64;
    for part in schedule.split(';') {
        let (key, value) = part.split_once('=')?;
        match key.trim().to_ascii_uppercase().as_str() {
            "FREQ" => freq = Some(value.trim().to_ascii_uppercase()),
            "INTERVAL" => interval = value.trim().parse::<u64>().ok()?,
            _ => {}
        }
    }
    let base: u64 = match freq.as_deref()? {
        "MINUTELY" => 60_000,
        "HOURLY" => 3_600_000,
        "DAILY" => 86_400_000,
        _ => return None,
    };
    base.checked_mul(interval.max(1))
}

fn latest_automation_run<'a>(
    runs: &'a [AutomationRunSpec],
    automation_id: &str,
) -> Option<&'a AutomationRunSpec> {
    runs.iter()
        .filter(|run| run.automation_id == automation_id)
        .max_by_key(|run| run.started_ms)
}

fn format_time_ms(value: u64) -> String {
    format!("{value} ms")
}

fn read_memory_specs(workspace: &Path) -> anyhow::Result<Vec<MemorySpec>> {
    let dir = workspace.join(".oxide/memories");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut specs = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let text = std::fs::read_to_string(path)?;
        specs.push(toml::from_str::<MemorySpec>(&text)?);
    }
    specs.sort_by(|a, b| {
        b.enabled
            .cmp(&a.enabled)
            .then_with(|| b.created_ms.cmp(&a.created_ms))
            .then_with(|| a.title.cmp(&b.title))
    });
    Ok(specs)
}

fn write_memory_spec(workspace: &Path, spec: &MemorySpec) -> anyhow::Result<()> {
    let dir = workspace.join(".oxide/memories");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.toml", spec.id));
    let text = toml::to_string_pretty(spec)?;
    std::fs::write(path, text)?;
    Ok(())
}

fn delete_memory_spec(workspace: &Path, id: &str) -> anyhow::Result<()> {
    let path = workspace.join(".oxide/memories").join(format!("{id}.toml"));
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

fn read_hermes_profiles(workspace: &Path) -> anyhow::Result<Vec<HermesProfile>> {
    let dir = workspace.join(".oxide/hermes-profiles");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut profiles = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let text = std::fs::read_to_string(path)?;
        profiles.push(toml::from_str::<HermesProfile>(&text)?);
    }
    profiles.sort_by(|a, b| {
        b.created_ms
            .cmp(&a.created_ms)
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(profiles)
}

fn write_hermes_profile(workspace: &Path, profile: &HermesProfile) -> anyhow::Result<()> {
    let dir = workspace.join(".oxide/hermes-profiles");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.toml", profile.id));
    let text = toml::to_string_pretty(profile)?;
    std::fs::write(path, text)?;
    Ok(())
}

fn delete_hermes_profile(workspace: &Path, id: &str) -> anyhow::Result<()> {
    let path = workspace
        .join(".oxide/hermes-profiles")
        .join(format!("{id}.toml"));
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

fn hermes_profile_from_fields(
    name: &str,
    goal: &str,
    validation: &str,
    review_prompt: &str,
    created_ms: u64,
) -> anyhow::Result<HermesProfile> {
    let name = name.trim();
    let goal = goal.trim();
    let validation = validation.trim();
    if name.is_empty() {
        anyhow::bail!("Hermes profile name is required");
    }
    if goal.is_empty() {
        anyhow::bail!("Hermes profile goal is required");
    }
    if validation.is_empty() {
        anyhow::bail!("Hermes profile validation command is required");
    }
    Ok(HermesProfile {
        id: format!("{}-{created_ms}", slug_fragment(name)),
        name: name.to_string(),
        goal: goal.to_string(),
        validation: validation.to_string(),
        review_prompt: review_prompt.trim().to_string(),
        created_ms,
    })
}

fn read_project_rules(workspace: &Path) -> anyhow::Result<String> {
    for file in ["AGENTS.md", "agents.md"] {
        let path = workspace.join(file);
        if path.is_file() {
            return Ok(std::fs::read_to_string(path)?);
        }
    }
    Ok(String::new())
}

fn read_appshot_specs(workspace: &Path) -> anyhow::Result<Vec<AppshotSpec>> {
    let dir = workspace.join(".oxide/appshots");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut specs = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let text = std::fs::read_to_string(path)?;
        specs.push(toml::from_str::<AppshotSpec>(&text)?);
    }
    specs.sort_by(|a, b| {
        b.created_ms
            .cmp(&a.created_ms)
            .then_with(|| a.title.cmp(&b.title))
    });
    Ok(specs)
}

fn write_appshot_spec(workspace: &Path, spec: &AppshotSpec) -> anyhow::Result<()> {
    let dir = workspace.join(".oxide/appshots");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.toml", spec.id));
    let text = toml::to_string_pretty(spec)?;
    std::fs::write(path, text)?;
    Ok(())
}

fn read_browser_action_specs(workspace: &Path) -> anyhow::Result<Vec<BrowserActionSpec>> {
    let dir = workspace.join(".oxide/browser-actions");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut specs = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let text = std::fs::read_to_string(path)?;
        specs.push(toml::from_str::<BrowserActionSpec>(&text)?);
    }
    specs.sort_by(|a, b| {
        b.created_ms
            .cmp(&a.created_ms)
            .then_with(|| a.action.cmp(&b.action))
    });
    Ok(specs)
}

fn write_browser_action_spec(workspace: &Path, spec: &BrowserActionSpec) -> anyhow::Result<()> {
    let dir = workspace.join(".oxide/browser-actions");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.toml", spec.id));
    let text = toml::to_string_pretty(spec)?;
    std::fs::write(path, text)?;
    Ok(())
}

fn browser_action_from_fields(
    action: &str,
    url: &str,
    note: &str,
    created_ms: u64,
) -> anyhow::Result<BrowserActionSpec> {
    let action = action.trim();
    let url = url.trim();
    if action.is_empty() {
        anyhow::bail!("Browser action is required");
    }
    if url.is_empty() {
        anyhow::bail!("Browser target URL is required");
    }
    Ok(BrowserActionSpec {
        id: format!("{}-{created_ms}", slug_fragment(action)),
        action: action.to_string(),
        url: url.to_string(),
        note: note.trim().to_string(),
        created_ms,
    })
}

fn appshot_capture_path_for(workspace: &Path, title: &str, created_ms: u64) -> PathBuf {
    workspace
        .join(".oxide/appshots/files")
        .join(format!("{}-{created_ms}.png", slug_fragment(title)))
}

fn run_screen_capture(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    #[cfg(target_os = "macos")]
    let output = std::process::Command::new("screencapture")
        .arg("-x")
        .arg(path)
        .output();
    #[cfg(not(target_os = "macos"))]
    let output: Result<std::process::Output, std::io::Error> = Err(std::io::Error::new(
        std::io::ErrorKind::Other,
        "unsupported",
    ));

    match output {
        Ok(out) if out.status.success() && path.is_file() => Ok(()),
        Ok(out) => {
            let mut detail = String::from_utf8_lossy(&out.stdout).to_string();
            detail.push_str(&String::from_utf8_lossy(&out.stderr));
            if detail.trim().is_empty() {
                detail = format!(
                    "screencapture exited with {}",
                    out.status.code().unwrap_or(-1)
                );
            }
            Err(detail)
        }
        Err(e) => Err(e.to_string()),
    }
}

fn browser_snapshot_appshot_draft(
    workspace: &Path,
    source_url: &str,
    note: &str,
    browser_action_id: &str,
    created_ms: u64,
) -> anyhow::Result<AppshotSpec> {
    let source_url = source_url.trim();
    if source_url.is_empty() {
        anyhow::bail!("Browser snapshot URL is required");
    }
    let title = browser_snapshot_title(source_url);
    let path = appshot_capture_path_for(workspace, &title, created_ms);
    let note = if note.trim().is_empty() {
        "Agent requested browser snapshot"
    } else {
        note.trim()
    };
    Ok(appshot_with_browser_source(
        captured_appshot_spec(&title, &path, note, created_ms),
        source_url,
        browser_action_id,
    ))
}

fn browser_snapshot_title(source_url: &str) -> String {
    let trimmed = source_url.trim().trim_end_matches('/');
    let last_segment = trimmed
        .rsplit(['/', '#', '?'])
        .find(|part| !part.is_empty() && !part.contains(':'))
        .unwrap_or("target");
    format!(
        "Browser snapshot {}",
        slug_fragment(last_segment).replace('-', " ")
    )
}

fn captured_appshot_spec(title: &str, path: &Path, note: &str, created_ms: u64) -> AppshotSpec {
    AppshotSpec {
        id: format!("{}-{created_ms}", slug_fragment(title)),
        title: title.to_string(),
        path: path.display().to_string(),
        note: note.to_string(),
        annotations: Vec::new(),
        source_url: String::new(),
        browser_action_id: String::new(),
        created_ms,
    }
}

fn appshot_with_added_annotation(
    appshot: &AppshotSpec,
    label: &str,
    target: &str,
    note: &str,
) -> anyhow::Result<AppshotSpec> {
    let label = label.trim();
    let target = target.trim();
    let note = note.trim();
    if note.is_empty() {
        anyhow::bail!("Annotation note is required");
    }
    let mut next = appshot.clone();
    next.annotations.push(AppshotAnnotation {
        label: if label.is_empty() {
            format!("{}", next.annotations.len() + 1)
        } else {
            label.to_string()
        },
        target: if target.is_empty() {
            "visual target".to_string()
        } else {
            target.to_string()
        },
        note: note.to_string(),
    });
    Ok(next)
}

fn appshot_with_browser_source(
    mut appshot: AppshotSpec,
    source_url: &str,
    browser_action_id: &str,
) -> AppshotSpec {
    appshot.source_url = source_url.trim().to_string();
    appshot.browser_action_id = browser_action_id.trim().to_string();
    appshot
}

fn next_appshot_annotation_label(count: usize) -> String {
    let index = count % 26;
    let byte = b'A' + index as u8;
    (byte as char).to_string()
}

fn is_previewable_appshot_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref(),
        Some("png" | "jpg" | "jpeg")
    )
}

fn load_appshot_color_image(path: &Path) -> anyhow::Result<ColorImage> {
    let bytes = std::fs::read(path)?;
    let image = image::load_from_memory(&bytes)?.to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    Ok(ColorImage::from_rgba_unmultiplied(size, image.as_raw()))
}

fn thumbnail_size(size: Vec2, max_width: f32, max_height: f32) -> Vec2 {
    if size.x <= 0.0 || size.y <= 0.0 {
        return Vec2::new(max_width, max_height);
    }
    let scale = (max_width / size.x).min(max_height / size.y).min(1.0);
    Vec2::new(size.x * scale, size.y * scale)
}

fn reveal_path(path: &Path) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    let output = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .output();
    #[cfg(target_os = "windows")]
    let output = std::process::Command::new("explorer")
        .arg("/select,")
        .arg(path)
        .output();
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let output = std::process::Command::new("xdg-open")
        .arg(path.parent().unwrap_or_else(|| Path::new(".")))
        .output();

    match output {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let mut detail = String::from_utf8_lossy(&out.stdout).to_string();
            detail.push_str(&String::from_utf8_lossy(&out.stderr));
            if detail.trim().is_empty() {
                detail = format!("open exited with {}", out.status.code().unwrap_or(-1));
            }
            anyhow::bail!(detail)
        }
        Err(e) => Err(e.into()),
    }
}

fn open_url_external(url: &str) -> anyhow::Result<()> {
    let url = url.trim();
    if url.is_empty() {
        anyhow::bail!("Browser target URL is required");
    }
    #[cfg(target_os = "macos")]
    let output = std::process::Command::new("open").arg(url).output();
    #[cfg(target_os = "windows")]
    let output = std::process::Command::new("rundll32")
        .arg("url.dll,FileProtocolHandler")
        .arg(url)
        .output();
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let output = std::process::Command::new("xdg-open").arg(url).output();

    match output {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let mut detail = String::from_utf8_lossy(&out.stdout).to_string();
            detail.push_str(&String::from_utf8_lossy(&out.stderr));
            if detail.trim().is_empty() {
                detail = format!("open URL exited with {}", out.status.code().unwrap_or(-1));
            }
            anyhow::bail!(detail)
        }
        Err(e) => Err(e.into()),
    }
}

fn build_evolve_prompt(goal: &str, validation: &str, diff_context: &str) -> String {
    format!(
        "Hermes evolve\n\nGoal:\n{goal}\n\nValidation command(s):\n{validation}\n\nCurrent workspace diff/status context:\n{diff_context}\n\nInstructions:\n1. Inspect the current repository state.\n2. Propose the smallest high-impact improvement toward the goal.\n3. Implement it with focused edits.\n4. Run the validation command(s) when practical.\n5. Report changed files, validation result, and remaining risks."
    )
}

fn build_hermes_review_prompt(goal: &str, validation: &str, review_prompt: &str) -> String {
    format!(
        "Hermes review loop\n\nGoal:\n{goal}\n\nValidation command(s):\n{validation}\n\nReview gate:\n{review_prompt}\n\nInstructions:\n1. Review the latest workspace changes against the goal.\n2. Identify spec gaps, code quality risks, UX regressions, and missing validation.\n3. Fix concrete issues when practical.\n4. Re-run validation when practical.\n5. Report what changed, what passed, and what remains risky."
    )
}

fn build_steer_prompt(note: &str) -> String {
    format!(
        "Steer the current workspace task with this operator note:\n\n{}\n\nUse it as guidance for the next actionable agent step. Do not repeat completed work unless the note asks for it.",
        note.trim()
    )
}

fn build_prompt_with_appshots(prompt: &str, appshots: &[AppshotSpec]) -> String {
    let base = prompt.trim();
    if appshots.is_empty() {
        return base.to_string();
    }
    let mut text = base.to_string();
    text.push_str("\n\nAttached Appshots:\n");
    for appshot in appshots {
        text.push_str(&format!(
            "- {}: {}\n  Note: {}\n",
            appshot.title,
            appshot.path,
            empty_label(&appshot.note)
        ));
        if !appshot.source_url.trim().is_empty() {
            text.push_str(&format!("  Source URL: {}\n", appshot.source_url));
        }
        if !appshot.browser_action_id.trim().is_empty() {
            text.push_str(&format!(
                "  Browser action: {}\n",
                appshot.browser_action_id
            ));
        }
        if !appshot.annotations.is_empty() {
            text.push_str("  Annotations:\n");
            for annotation in &appshot.annotations {
                text.push_str(&format!(
                    "  [{}] {}: {}\n",
                    annotation.label, annotation.target, annotation.note
                ));
            }
        }
    }
    text
}

fn build_prompt_with_browser_context(
    prompt: &str,
    target_url: &str,
    actions: &[BrowserActionSpec],
) -> String {
    let mut text = prompt.trim().to_string();
    let target_url = target_url.trim();
    if target_url.is_empty() && actions.is_empty() {
        return text;
    }
    if text.contains("Browser Context:") {
        return text;
    }
    text.push_str("\n\nBrowser Context:\n");
    if !target_url.is_empty() {
        text.push_str(&format!("Browser Target: {target_url}\n"));
    }
    if !actions.is_empty() {
        text.push_str("Recent Browser Actions:\n");
        for action in actions.iter().take(8) {
            text.push_str(&format!(
                "- {}: {}\n  Note: {}\n",
                action.action,
                action.url,
                empty_label(&action.note)
            ));
        }
    }
    text
}

fn latest_browser_action_id(actions: &[BrowserActionSpec]) -> String {
    actions
        .iter()
        .max_by_key(|action| action.created_ms)
        .map(|action| action.id.clone())
        .unwrap_or_default()
}

fn build_prompt_with_file_context(prompt: &str, path: &Path, content: &str) -> String {
    let mut text = prompt.trim().to_string();
    if !text.is_empty() {
        text.push_str("\n\n");
    }
    text.push_str("Attached file context:\n");
    text.push_str(&format!("Path: {}\n\n", path.display()));
    text.push_str("```text\n");
    text.push_str(content.trim_end());
    text.push_str("\n```");
    text
}

fn build_session_resume_context(
    session: &SessionSummary,
    messages: &[ChatMsg],
    max_chars: usize,
) -> String {
    let mut selected = Vec::new();
    let mut used = 0usize;
    for message in messages.iter().rev() {
        let role = match message.kind {
            MsgKind::User => "user",
            MsgKind::Agent => "assistant",
            MsgKind::Note => "note",
        };
        let line = format!("{role}: {}", message.text.trim());
        let line_chars = line.chars().count() + 1;
        if !selected.is_empty() && used + line_chars > max_chars {
            break;
        }
        used += line_chars;
        selected.push(line);
    }
    selected.reverse();

    let mut context = format!(
        "Resume selected thread context:\nThread: {}\nID: {}\nMessages: {}\n\nRecent transcript:\n",
        session.title, session.id, session.message_count
    );
    if selected.is_empty() {
        context.push_str("(No previous messages available.)");
    } else {
        context.push_str(&selected.join("\n"));
    }
    context
}

fn build_prompt_with_session_context(prompt: &str, session_context: &str) -> String {
    format!(
        "Continue the selected thread using the prior context below. Treat the current user request as the next turn in that same thread.\n\n{}\n\nCurrent user request:\n{}",
        session_context.trim(),
        prompt.trim()
    )
}

fn build_prompt_with_personalization(
    prompt: &str,
    tone: &str,
    custom_instructions: &str,
) -> String {
    let tone = personalization_tone_id(tone);
    let custom_instructions = custom_instructions.trim();
    if tone == default_personalization_tone_id() && custom_instructions.is_empty() {
        return prompt.trim().to_string();
    }
    let mut text = prompt.trim().to_string();
    text.push_str("\n\nPersonalization:\n");
    text.push_str(&format!("- Tone: {}\n", personalization_tone_label(tone)));
    if !custom_instructions.is_empty() {
        text.push_str("- Custom instructions:\n");
        text.push_str(custom_instructions);
        text.push('\n');
    }
    text
}

fn build_prompt_with_goal_mode(
    prompt: &str,
    enabled: bool,
    goal: &str,
    success_criteria: &str,
) -> String {
    let goal = goal.trim();
    if !enabled || goal.is_empty() {
        return prompt.trim().to_string();
    }
    let mut text = prompt.trim().to_string();
    if text.contains(GOAL_MODE_MARKER) {
        return text;
    }
    text.push_str("\n\nGoal Mode:\n");
    text.push_str(GOAL_MODE_MARKER);
    text.push('\n');
    text.push_str(&format!("- Active goal: {goal}\n"));
    let success_criteria = success_criteria.trim();
    if !success_criteria.is_empty() {
        text.push_str("- Success criteria:\n");
        text.push_str(success_criteria);
        text.push('\n');
    }
    text
}

fn build_prompt_with_memory(prompt: &str, project_rules: &str, memories: &[MemorySpec]) -> String {
    let enabled = memories
        .iter()
        .filter(|memory| memory.enabled)
        .collect::<Vec<_>>();
    if project_rules.trim().is_empty() && enabled.is_empty() {
        return prompt.trim().to_string();
    }
    let mut text = prompt.trim().to_string();
    text.push_str("\n\nWorkspace memory context:\n");
    if !project_rules.trim().is_empty() {
        text.push_str("\nProject rules:\n");
        text.push_str(project_rules.trim());
        text.push('\n');
    }
    if !enabled.is_empty() {
        text.push_str("\nMemory notes:\n");
        for memory in enabled {
            text.push_str(&format!("- {}: {}\n", memory.title, memory.body));
        }
    }
    text
}

fn file_entry_matches_query(path: &Path, query: &str) -> bool {
    let query = normalize_query(query);
    if query.is_empty() {
        return true;
    }
    let name = path
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_default();
    let extension = path
        .extension()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_default();
    normalize_query(&name).contains(&query)
        || normalize_query(&extension).contains(&query)
        || normalize_query(&path.display().to_string()).contains(&query)
}

fn read_file_context(path: &Path, max_chars: usize) -> anyhow::Result<String> {
    let bytes = std::fs::read(path)
        .map_err(|e| anyhow::anyhow!("cannot read file context for {}: {e}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    if text.chars().count() <= max_chars {
        return Ok(text.to_string());
    }
    let prefix = text.chars().take(max_chars).collect::<String>();
    Ok(format!("{prefix}\n\n... file context truncated ..."))
}

fn compact_tool_result(output: &str, fold: bool) -> String {
    const LIMIT: usize = 1_600;
    const HEAD: usize = 900;
    const TAIL: usize = 300;
    if !fold || output.chars().count() <= LIMIT {
        return output.to_string();
    }
    let head = output.chars().take(HEAD).collect::<String>();
    let tail = output
        .chars()
        .rev()
        .take(TAIL)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("{head}\n\n... tool output folded ...\n\n{tail}")
}

fn automation_id(name: &str) -> String {
    let mut id = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if id.is_empty() {
        id = "automation".to_string();
    }
    format!("{id}-{}", now_ms())
}

fn appshot_id(title: &str) -> String {
    let mut id = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if id.is_empty() {
        id = "appshot".to_string();
    }
    format!("{id}-{}", now_ms())
}

fn memory_id(title: &str) -> String {
    let id = slug_fragment(title);
    format!("{id}-{}", now_ms())
}

fn worktree_path_for(workspace: &Path, branch: &str) -> PathBuf {
    let base = workspace
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "workspace".to_string());
    let dir_name = format!("{base}-{}", slug_fragment(branch));
    workspace
        .parent()
        .map(|parent| parent.join(&dir_name))
        .unwrap_or_else(|| PathBuf::from(dir_name))
}

fn build_worktree_command(workspace: &Path, branch: &str) -> String {
    format!(
        "git worktree add {} {}",
        worktree_path_for(workspace, branch).display(),
        branch.trim()
    )
}

fn default_worktree_branch(snapshot: &GitBranchSnapshot) -> String {
    if !snapshot.current_branch.trim().is_empty() && !snapshot.current_branch.contains("fatal:") {
        return snapshot.current_branch.clone();
    }
    snapshot
        .branches
        .first()
        .cloned()
        .unwrap_or_else(|| "main".to_string())
}

fn slug_fragment(value: &str) -> String {
    let slug = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        "branch".to_string()
    } else {
        slug
    }
}

fn default_true() -> bool {
    true
}

fn default_detail_level_id() -> String {
    "coding".to_string()
}

fn default_personalization_tone_id() -> String {
    "friendly".to_string()
}

fn detail_level_id(level: DetailLevel) -> &'static str {
    match level {
        DetailLevel::Default => "default",
        DetailLevel::Coding => "coding",
    }
}

fn detail_level_from_id(value: &str) -> DetailLevel {
    match value.trim().to_ascii_lowercase().as_str() {
        "default" => DetailLevel::Default,
        _ => DetailLevel::Coding,
    }
}

fn personalization_tone_id(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        "direct" => "direct",
        _ => "friendly",
    }
}

fn personalization_tone_label(value: &str) -> &'static str {
    match personalization_tone_id(value) {
        "direct" => "Direct",
        _ => "Friendly",
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn truncate_title(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= 42 {
        return compact;
    }
    let mut out = compact.chars().take(39).collect::<String>();
    out.push_str("...");
    out
}

fn truncate_diff(text: &str) -> String {
    const LIMIT: usize = 16_000;
    if text.chars().count() <= LIMIT {
        return text.to_string();
    }
    let prefix = text.chars().take(LIMIT).collect::<String>();
    format!("{prefix}\n\n... diff truncated ...")
}

fn empty_label(value: &str) -> &str {
    if value.trim().is_empty() {
        "None"
    } else {
        value
    }
}

fn compact_line(value: &str) -> String {
    let text = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        "unavailable".to_string()
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "oxide-desktop-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn session_summaries_use_first_user_message_as_title() {
        let tmp = unique_tmp("sessions");
        let dir = tmp.join(".oxide/sessions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("100.jsonl"),
            r#"{"role":"system","content":"sys","ts_ms":1}
{"role":"user","content":"Build a Rust desktop","ts_ms":2}
{"role":"assistant","content":"ok","ts_ms":3}
"#,
        )
        .unwrap();

        let sessions = read_session_summaries(&tmp).unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].title, "Build a Rust desktop");
        assert_eq!(sessions[0].message_count, 3);
        assert_eq!(sessions[0].id, "100");
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn session_metadata_roundtrip_from_workspace_store() {
        let tmp = unique_tmp("session-meta");
        std::fs::create_dir_all(&tmp).unwrap();
        let meta = SessionMeta {
            id: "100".to_string(),
            title: Some("Renamed thread".to_string()),
            pinned: true,
            archived: false,
            updated_ms: 25,
        };

        write_session_meta(&tmp, &meta).unwrap();
        let loaded = read_session_meta_map(&tmp).unwrap();

        assert_eq!(loaded.get("100"), Some(&meta));
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn session_summaries_apply_rename_pin_and_archive_metadata() {
        let tmp = unique_tmp("sessions-meta-apply");
        let dir = tmp.join(".oxide/sessions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("older.jsonl"),
            r#"{"role":"user","content":"Older task","ts_ms":1}
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("newer.jsonl"),
            r#"{"role":"user","content":"Newer task","ts_ms":10}
"#,
        )
        .unwrap();
        write_session_meta(
            &tmp,
            &SessionMeta {
                id: "older".to_string(),
                title: Some("Pinned renamed".to_string()),
                pinned: true,
                archived: false,
                updated_ms: 20,
            },
        )
        .unwrap();
        write_session_meta(
            &tmp,
            &SessionMeta {
                id: "newer".to_string(),
                title: None,
                pinned: false,
                archived: true,
                updated_ms: 30,
            },
        )
        .unwrap();

        let sessions = read_session_summaries(&tmp).unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "older");
        assert_eq!(sessions[0].title, "Pinned renamed");
        assert!(sessions[0].pinned);
        assert!(!sessions[0].archived);
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn filter_sessions_matches_title_id_and_archive_scope() {
        let sessions = vec![
            SessionSummary {
                id: "alpha".to_string(),
                title: "Pinned Build".to_string(),
                path: PathBuf::from("alpha.jsonl"),
                message_count: 3,
                last_ts_ms: 5,
                pinned: true,
                archived: false,
            },
            SessionSummary {
                id: "beta".to_string(),
                title: "Archived Review".to_string(),
                path: PathBuf::from("beta.jsonl"),
                message_count: 2,
                last_ts_ms: 9,
                pinned: false,
                archived: true,
            },
        ];

        let active = filter_sessions(&sessions, "build", false);
        let archived = filter_sessions(&sessions, "beta", true);

        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "alpha");
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].id, "beta");
    }

    #[test]
    fn session_resume_context_keeps_recent_messages_with_thread_identity() {
        let messages = vec![
            ChatMsg {
                kind: MsgKind::User,
                text: "old request".repeat(20),
            },
            ChatMsg {
                kind: MsgKind::Agent,
                text: "recent answer".to_string(),
            },
            ChatMsg {
                kind: MsgKind::User,
                text: "follow up".to_string(),
            },
        ];
        let summary = SessionSummary {
            id: "alpha".to_string(),
            title: "Build terminal".to_string(),
            path: PathBuf::from("alpha.jsonl"),
            message_count: messages.len(),
            last_ts_ms: 10,
            pinned: false,
            archived: false,
        };

        let context = build_session_resume_context(&summary, &messages, 80);

        assert!(context.contains("Thread: Build terminal"));
        assert!(context.contains("ID: alpha"));
        assert!(context.contains("assistant: recent answer"));
        assert!(context.contains("user: follow up"));
        assert!(!context.contains("old requestold request"));
    }

    #[test]
    fn prompt_with_session_context_wraps_user_prompt_once() {
        let prompt = build_prompt_with_session_context(
            "Continue the work",
            "Resume selected thread context:\nuser: prior task",
        );

        assert!(prompt.contains("Continue the selected thread"));
        assert!(prompt.contains("user: prior task"));
        assert!(prompt.contains("Current user request:\nContinue the work"));
    }

    #[test]
    fn unified_search_collects_threads_files_and_workspace_artifacts() {
        let workspace = unique_tmp("unified-search");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("README.md"), "hello").unwrap();
        let sessions = vec![SessionSummary {
            id: "alpha".to_string(),
            title: "Pinned Build".to_string(),
            path: PathBuf::from("alpha.jsonl"),
            message_count: 3,
            last_ts_ms: 5,
            pinned: true,
            archived: false,
        }];
        let memories = vec![MemorySpec {
            id: "workflow".to_string(),
            title: "Workflow".to_string(),
            body: "Prefer focused Rust-native slices.".to_string(),
            enabled: true,
            created_ms: 1,
        }];
        let automations = vec![AutomationSpec {
            id: "daily-review".to_string(),
            name: "Daily review".to_string(),
            kind: "cron".to_string(),
            status: "ACTIVE".to_string(),
            schedule: "FREQ=DAILY;INTERVAL=1".to_string(),
            prompt: "Review the workspace".to_string(),
            created_ms: 10,
        }];
        let appshots = vec![AppshotSpec {
            id: "login".to_string(),
            title: "Login screen".to_string(),
            path: "/tmp/login.png".to_string(),
            note: "Modal clipped".to_string(),
            annotations: Vec::new(),
            source_url: String::new(),
            browser_action_id: String::new(),
            created_ms: 2,
        }];
        let hermes = vec![HermesProfile {
            id: "desktop-parity".to_string(),
            name: "Desktop parity".to_string(),
            goal: "Improve parity".to_string(),
            validation: "cargo test".to_string(),
            review_prompt: "Review risks".to_string(),
            created_ms: 12,
        }];
        let mcp = vec![McpServerConfig {
            name: "fs".to_string(),
            command: "npx".to_string(),
            args: vec!["server".to_string()],
            ..McpServerConfig::default()
        }];

        let results = build_global_search_results(GlobalSearchInputs {
            workspace: &workspace,
            sessions: &sessions,
            memories: &memories,
            automations: &automations,
            appshots: &appshots,
            hermes_profiles: &hermes,
            mcp_servers: &mcp,
            shortcuts: &shortcut_catalog(),
            goal_mode_enabled: false,
            active_goal: "",
            goal_success_criteria: "",
            query: "review",
        });

        assert!(results.iter().any(|item| item.kind == "Automation"));
        assert!(results.iter().any(|item| item.kind == "Shortcut"));
        assert!(results.iter().any(|item| item.kind == "Hermes"));
        std::fs::remove_dir_all(workspace).ok();
    }

    #[test]
    fn unified_search_file_result_targets_workspace_path() {
        let workspace = unique_tmp("unified-search-file");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("README.md"), "hello").unwrap();

        let results = build_global_search_results(GlobalSearchInputs {
            workspace: &workspace,
            sessions: &[],
            memories: &[],
            automations: &[],
            appshots: &[],
            hermes_profiles: &[],
            mcp_servers: &[],
            shortcuts: &[],
            goal_mode_enabled: false,
            active_goal: "",
            goal_success_criteria: "",
            query: "readme",
        });

        let file = results.iter().find(|item| item.kind == "File").unwrap();
        assert_eq!(file.title, "README.md");
        assert!(file.target.starts_with("file:"));
        std::fs::remove_dir_all(workspace).ok();
    }

    #[test]
    fn repo_index_collects_nested_files_and_skips_heavy_dirs() {
        let workspace = unique_tmp("repo-index");
        std::fs::create_dir_all(workspace.join("src/bin")).unwrap();
        std::fs::create_dir_all(workspace.join("target/debug")).unwrap();
        std::fs::create_dir_all(workspace.join(".git")).unwrap();
        std::fs::write(workspace.join("src/bin/main.rs"), "fn main() {}").unwrap();
        std::fs::write(workspace.join("target/debug/build.log"), "skip").unwrap();
        std::fs::write(workspace.join(".git/config"), "skip").unwrap();

        let entries = collect_repo_index(&workspace, 80);

        assert!(entries
            .iter()
            .any(|entry| entry.relative == "src/bin/main.rs" && !entry.is_dir));
        assert!(!entries
            .iter()
            .any(|entry| entry.relative.contains("target/debug")));
        assert!(!entries
            .iter()
            .any(|entry| entry.relative.contains(".git/config")));
        std::fs::remove_dir_all(workspace).ok();
    }

    #[test]
    fn global_search_finds_nested_repo_index_files() {
        let workspace = unique_tmp("global-search-nested-file");
        std::fs::create_dir_all(workspace.join("crates/oxide/src")).unwrap();
        std::fs::write(
            workspace.join("crates/oxide/src/lib.rs"),
            "pub fn oxide() {}",
        )
        .unwrap();

        let results = build_global_search_results(GlobalSearchInputs {
            workspace: &workspace,
            sessions: &[],
            memories: &[],
            automations: &[],
            appshots: &[],
            hermes_profiles: &[],
            mcp_servers: &[],
            shortcuts: &[],
            goal_mode_enabled: false,
            active_goal: "",
            goal_success_criteria: "",
            query: "oxide lib",
        });

        assert!(results.iter().any(|item| {
            item.kind == "File"
                && item.title == "crates/oxide/src/lib.rs"
                && item.target.starts_with("file:")
        }));
        std::fs::remove_dir_all(workspace).ok();
    }

    #[test]
    fn global_search_filters_repo_index_before_result_limit() {
        let workspace = unique_tmp("global-search-index-limit");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut repo_index = (0..170)
            .map(|index| RepoIndexEntry {
                path: workspace.join(format!("folder-{index}")),
                relative: format!("folder-{index}"),
                is_dir: true,
            })
            .collect::<Vec<_>>();
        repo_index.push(RepoIndexEntry {
            path: workspace.join("src/deep/needle.rs"),
            relative: "src/deep/needle.rs".to_string(),
            is_dir: false,
        });

        let results = build_global_search_results_with_repo_index(
            GlobalSearchInputs {
                workspace: &workspace,
                sessions: &[],
                memories: &[],
                automations: &[],
                appshots: &[],
                hermes_profiles: &[],
                mcp_servers: &[],
                shortcuts: &[],
                goal_mode_enabled: false,
                active_goal: "",
                goal_success_criteria: "",
                query: "needle",
            },
            &repo_index,
        );

        assert!(results
            .iter()
            .any(|item| item.kind == "File" && item.title == "src/deep/needle.rs"));
        std::fs::remove_dir_all(workspace).ok();
    }

    #[test]
    fn context_file_suggestions_return_matching_files_only() {
        let workspace = unique_tmp("context-file-suggestions");
        std::fs::create_dir_all(workspace.join("src/ui")).unwrap();
        std::fs::write(workspace.join("src/ui/palette.rs"), "pub fn palette() {}").unwrap();
        let repo_index = collect_repo_index(&workspace, REPO_INDEX_ENTRY_LIMIT);

        let suggestions = context_file_suggestions(&repo_index, "palette", 4);

        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].relative, "src/ui/palette.rs");
        assert!(!suggestions[0].is_dir);
        std::fs::remove_dir_all(workspace).ok();
    }

    #[test]
    fn mcp_args_parser_supports_json_array_and_shell_words() {
        assert_eq!(
            parse_mcp_args_input(r#"["-y","@modelcontextprotocol/server-filesystem","."]"#)
                .unwrap(),
            vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-filesystem".to_string(),
                ".".to_string()
            ]
        );
        assert_eq!(
            parse_mcp_args_input("-y package .").unwrap(),
            vec!["-y".to_string(), "package".to_string(), ".".to_string()]
        );
    }

    #[test]
    fn mcp_server_upsert_replaces_existing_by_name() {
        let mut cfg = Config::default();
        upsert_mcp_server(
            &mut cfg,
            McpServerConfig {
                name: "fs".to_string(),
                command: "npx".to_string(),
                args: vec!["old".to_string()],
                ..McpServerConfig::default()
            },
        );
        upsert_mcp_server(
            &mut cfg,
            McpServerConfig {
                name: "fs".to_string(),
                command: "bunx".to_string(),
                args: vec!["new".to_string()],
                ..McpServerConfig::default()
            },
        );

        assert_eq!(cfg.mcp_servers.len(), 1);
        assert_eq!(cfg.mcp_servers[0].command, "bunx");
        assert_eq!(cfg.mcp_servers[0].args, vec!["new"]);
    }

    #[test]
    fn mcp_server_remove_deletes_matching_name_only() {
        let mut cfg = Config::default();
        cfg.mcp_servers = vec![
            McpServerConfig {
                name: "fs".to_string(),
                command: "npx".to_string(),
                args: Vec::new(),
                ..McpServerConfig::default()
            },
            McpServerConfig {
                name: "linear".to_string(),
                command: "bunx".to_string(),
                args: Vec::new(),
                ..McpServerConfig::default()
            },
        ];

        remove_mcp_server(&mut cfg, "fs");

        assert_eq!(cfg.mcp_servers.len(), 1);
        assert_eq!(cfg.mcp_servers[0].name, "linear");
    }

    #[test]
    fn mcp_status_event_updates_health_catalog() {
        let mut health = BTreeMap::new();
        apply_mcp_status_event(
            &mut health,
            "fs".to_string(),
            "connected".to_string(),
            2,
            vec!["mcp__fs__read".to_string(), "mcp__fs__write".to_string()],
            "ready".to_string(),
        );

        let item = health.get("fs").unwrap();
        assert_eq!(item.status, "connected");
        assert_eq!(item.tool_count, 2);
        assert_eq!(item.tools.len(), 2);
        assert_eq!(item.detail, "ready");

        apply_mcp_status_event(
            &mut health,
            "fs".to_string(),
            "error".to_string(),
            0,
            Vec::new(),
            "connect failed".to_string(),
        );

        let item = health.get("fs").unwrap();
        assert_eq!(item.status, "error");
        assert_eq!(item.tool_count, 0);
        assert!(item.tools.is_empty());
        assert_eq!(item.detail, "connect failed");
    }

    #[test]
    fn configured_mcp_health_falls_back_before_runtime_status() {
        let mut health = BTreeMap::new();
        let server = McpServerConfig {
            name: "fs".to_string(),
            command: "npx".to_string(),
            args: vec!["server".to_string()],
            ..McpServerConfig::default()
        };

        let item = mcp_health_for(&health, &server);

        assert_eq!(item.name, "fs");
        assert_eq!(item.status, "configured");
        assert_eq!(item.tool_count, 0);

        apply_mcp_status_event(
            &mut health,
            "fs".to_string(),
            "connected".to_string(),
            1,
            vec!["mcp__fs__read".to_string()],
            "ready".to_string(),
        );

        assert_eq!(mcp_health_for(&health, &server).status, "connected");
    }

    #[test]
    fn git_snapshot_reports_clean_workspace_without_git() {
        let tmp = unique_tmp("git");
        std::fs::create_dir_all(&tmp).unwrap();

        let snapshot = git_workspace_snapshot(&tmp);

        assert!(snapshot.status.contains("not a git repository"));
        assert!(snapshot.diff_stat.is_empty());
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn save_project_config_writes_oxide_toml() {
        let tmp = unique_tmp("config");
        std::fs::create_dir_all(&tmp).unwrap();
        let mut cfg = Config::default();
        cfg.workspace = Some(tmp.clone());
        cfg.provider = "codex".to_string();
        cfg.model = "gpt-5.5".to_string();
        cfg.reasoning_effort = "high".to_string();

        save_project_config(&cfg).unwrap();

        let written = std::fs::read_to_string(tmp.join("oxide.toml")).unwrap();
        assert!(written.contains("provider = \"codex\""));
        assert!(written.contains("model = \"gpt-5.5\""));
        assert!(written.contains("reasoning_effort = \"high\""));
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn desktop_state_roundtrip_preserves_recent_workspaces() {
        let tmp = unique_tmp("desktop-state");
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("desktop.toml");
        let state = DesktopStateSpec {
            recent_workspaces: vec![RecentWorkspaceSpec {
                path: "/tmp/oxide".to_string(),
                name: "oxide".to_string(),
                last_opened_ms: 42,
            }],
            ..Default::default()
        };

        write_desktop_state_at(&path, &state).unwrap();
        let loaded = read_desktop_state_at(&path).unwrap();

        assert_eq!(loaded, state);
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn desktop_state_roundtrip_preserves_desktop_preferences() {
        let tmp = unique_tmp("desktop-preferences");
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("desktop.toml");
        let state = DesktopStateSpec {
            preferences: DesktopPreferences {
                motion_enabled: false,
                compact_sidebar: true,
                fold_tool_results: false,
                prevent_sleep: true,
                attach_memory_to_prompt: true,
                attach_appshots_to_prompt: true,
                detail_level: "default".to_string(),
                personalization_tone: "direct".to_string(),
                custom_instructions: "Prefer concise Indonesian status updates.".to_string(),
                goal_mode_enabled: true,
                active_goal: "Ship Codex-like Goal Mode".to_string(),
                goal_success_criteria: "Prompt context includes the goal once.".to_string(),
            },
            ..Default::default()
        };

        write_desktop_state_at(&path, &state).unwrap();
        let loaded = read_desktop_state_at(&path).unwrap();

        assert_eq!(loaded.preferences, state.preferences);
        assert!(loaded.preferences.goal_mode_enabled);
        assert_eq!(loaded.preferences.active_goal, "Ship Codex-like Goal Mode");
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn desktop_state_defaults_preferences_for_existing_recent_only_file() {
        let tmp = unique_tmp("desktop-preferences-default");
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("desktop.toml");
        std::fs::write(
            &path,
            "recent_workspaces = [{ path = \"/tmp/oxide\", name = \"oxide\", last_opened_ms = 42 }]\n",
        )
        .unwrap();

        let loaded = read_desktop_state_at(&path).unwrap();

        assert_eq!(loaded.preferences, DesktopPreferences::default());
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn recent_workspace_upsert_dedupes_and_keeps_latest_first() {
        let existing = vec![
            RecentWorkspaceSpec {
                path: "/tmp/beta".to_string(),
                name: "beta".to_string(),
                last_opened_ms: 1,
            },
            RecentWorkspaceSpec {
                path: "/tmp/oxide".to_string(),
                name: "old oxide".to_string(),
                last_opened_ms: 2,
            },
        ];

        let next = upsert_recent_workspace(existing, Path::new("/tmp/oxide"), "oxide", 99, 8);

        assert_eq!(next.len(), 2);
        assert_eq!(next[0].path, "/tmp/oxide");
        assert_eq!(next[0].name, "oxide");
        assert_eq!(next[0].last_opened_ms, 99);
        assert_eq!(next[1].path, "/tmp/beta");
    }

    #[test]
    fn automation_specs_roundtrip_from_workspace_store() {
        let tmp = unique_tmp("automations");
        std::fs::create_dir_all(&tmp).unwrap();
        let spec = AutomationSpec {
            id: "daily-review".to_string(),
            name: "Daily review".to_string(),
            kind: "cron".to_string(),
            status: "ACTIVE".to_string(),
            schedule: "FREQ=DAILY;INTERVAL=1".to_string(),
            prompt: "Review the workspace".to_string(),
            created_ms: 10,
        };

        write_automation_spec(&tmp, &spec).unwrap();
        let specs = read_automation_specs(&tmp).unwrap();

        assert_eq!(specs, vec![spec]);
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn automation_status_toggle_switches_active_and_paused() {
        let active = AutomationSpec {
            id: "daily-review".to_string(),
            name: "Daily review".to_string(),
            kind: "cron".to_string(),
            status: "ACTIVE".to_string(),
            schedule: "FREQ=DAILY;INTERVAL=1".to_string(),
            prompt: "Review the workspace".to_string(),
            created_ms: 10,
        };

        let paused = automation_with_toggled_status(&active);
        let active_again = automation_with_toggled_status(&paused);

        assert_eq!(paused.status, "PAUSED");
        assert_eq!(active_again.status, "ACTIVE");
    }

    #[test]
    fn automation_run_prompt_preserves_schedule_context() {
        let spec = AutomationSpec {
            id: "daily-review".to_string(),
            name: "Daily review".to_string(),
            kind: "cron".to_string(),
            status: "ACTIVE".to_string(),
            schedule: "FREQ=DAILY;INTERVAL=1".to_string(),
            prompt: "Review the workspace".to_string(),
            created_ms: 10,
        };

        let prompt = build_automation_run_prompt(&spec);

        assert!(prompt.contains("Run automation now"));
        assert!(prompt.contains("Daily review"));
        assert!(prompt.contains("Review the workspace"));
        assert!(prompt.contains("FREQ=DAILY"));
    }

    #[test]
    fn delete_automation_spec_removes_toml_file() {
        let tmp = unique_tmp("automation-delete");
        std::fs::create_dir_all(&tmp).unwrap();
        let spec = AutomationSpec {
            id: "daily-review".to_string(),
            name: "Daily review".to_string(),
            kind: "cron".to_string(),
            status: "ACTIVE".to_string(),
            schedule: "FREQ=DAILY;INTERVAL=1".to_string(),
            prompt: "Review the workspace".to_string(),
            created_ms: 10,
        };
        write_automation_spec(&tmp, &spec).unwrap();

        delete_automation_spec(&tmp, &spec.id).unwrap();

        assert!(read_automation_specs(&tmp).unwrap().is_empty());
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn automation_run_specs_roundtrip_from_workspace_store() {
        let tmp = unique_tmp("automation-runs");
        std::fs::create_dir_all(&tmp).unwrap();
        let run = AutomationRunSpec {
            id: "daily-review-20".to_string(),
            automation_id: "daily-review".to_string(),
            automation_name: "Daily review".to_string(),
            trigger: "manual".to_string(),
            status: "queued".to_string(),
            prompt: "Review the workspace".to_string(),
            started_ms: 20,
        };

        write_automation_run_spec(&tmp, &run).unwrap();
        let runs = read_automation_run_specs(&tmp).unwrap();

        assert_eq!(runs, vec![run]);
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn automation_due_uses_schedule_interval_and_latest_run() {
        let spec = AutomationSpec {
            id: "daily-review".to_string(),
            name: "Daily review".to_string(),
            kind: "cron".to_string(),
            status: "ACTIVE".to_string(),
            schedule: "FREQ=MINUTELY;INTERVAL=5".to_string(),
            prompt: "Review the workspace".to_string(),
            created_ms: 1_000,
        };
        let recent_run = AutomationRunSpec {
            id: "daily-review-10000".to_string(),
            automation_id: spec.id.clone(),
            automation_name: spec.name.clone(),
            trigger: "scheduled".to_string(),
            status: "queued".to_string(),
            prompt: spec.prompt.clone(),
            started_ms: 10_000,
        };

        assert!(!automation_is_due(&spec, &[], 120_000));
        assert!(automation_is_due(&spec, &[], 310_000));
        assert!(!automation_is_due(&spec, &[recent_run.clone()], 250_000));
        assert!(automation_is_due(&spec, &[recent_run], 311_000));
    }

    #[test]
    fn automation_due_ignores_paused_or_invalid_schedules() {
        let mut spec = AutomationSpec {
            id: "daily-review".to_string(),
            name: "Daily review".to_string(),
            kind: "cron".to_string(),
            status: "PAUSED".to_string(),
            schedule: "FREQ=MINUTELY;INTERVAL=1".to_string(),
            prompt: "Review the workspace".to_string(),
            created_ms: 1,
        };

        assert!(!automation_is_due(&spec, &[], 120_000));
        spec.status = "ACTIVE".to_string();
        spec.schedule = "bad schedule".to_string();
        assert!(!automation_is_due(&spec, &[], 120_000));
    }

    #[test]
    fn memory_specs_roundtrip_from_workspace_store() {
        let tmp = unique_tmp("memories");
        std::fs::create_dir_all(&tmp).unwrap();
        let spec = MemorySpec {
            id: "repo-rule".to_string(),
            title: "Repo rule".to_string(),
            body: "Always use Bun for JS tasks.".to_string(),
            enabled: true,
            created_ms: 11,
        };

        write_memory_spec(&tmp, &spec).unwrap();
        let specs = read_memory_specs(&tmp).unwrap();

        assert_eq!(specs, vec![spec]);
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn project_rules_reader_returns_agents_markdown_when_present() {
        let tmp = unique_tmp("rules");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("AGENTS.md"), "Always respond in Indonesian.").unwrap();

        let rules = read_project_rules(&tmp).unwrap();

        assert!(rules.contains("Always respond in Indonesian."));
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn prompt_with_memory_includes_enabled_notes_and_rules() {
        let notes = vec![
            MemorySpec {
                id: "enabled".to_string(),
                title: "Workflow".to_string(),
                body: "Prefer focused Rust-native slices.".to_string(),
                enabled: true,
                created_ms: 1,
            },
            MemorySpec {
                id: "disabled".to_string(),
                title: "Disabled".to_string(),
                body: "Do not include this.".to_string(),
                enabled: false,
                created_ms: 2,
            },
        ];

        let prompt = build_prompt_with_memory("Continue", "Project rule", &notes);

        assert!(prompt.contains("Continue"));
        assert!(prompt.contains("Project rule"));
        assert!(prompt.contains("Prefer focused Rust-native slices."));
        assert!(!prompt.contains("Do not include this."));
    }

    #[test]
    fn prompt_with_personalization_includes_tone_and_custom_instructions() {
        let prompt = build_prompt_with_personalization(
            "Continue",
            "direct",
            "Keep progress updates short and practical.",
        );

        assert!(prompt.contains("Continue"));
        assert!(prompt.contains("Tone: Direct"));
        assert!(prompt.contains("Keep progress updates short and practical."));
    }

    #[test]
    fn prompt_with_goal_mode_includes_active_goal_once() {
        let prompt = build_prompt_with_goal_mode(
            "Continue implementation",
            true,
            "Reach Codex desktop parity",
            "Keep it Rust-native and verified.",
        );

        assert!(prompt.contains("Goal Mode:"));
        assert!(prompt.contains(GOAL_MODE_MARKER));
        assert!(prompt.contains("Reach Codex desktop parity"));
        assert!(prompt.contains("Keep it Rust-native and verified."));

        let repeated = build_prompt_with_goal_mode(
            &prompt,
            true,
            "Reach Codex desktop parity",
            "Keep it Rust-native and verified.",
        );
        assert_eq!(repeated.matches("Goal Mode:").count(), 1);
    }

    #[test]
    fn prompt_with_goal_mode_does_not_skip_user_mentioned_goal_mode_text() {
        let prompt = build_prompt_with_goal_mode(
            "Please inspect the text `Goal Mode:` in this file.",
            true,
            "Keep durable objective attached",
            "",
        );

        assert!(prompt.contains("Please inspect the text"));
        assert!(prompt.contains(GOAL_MODE_MARKER));
        assert!(prompt.contains("Keep durable objective attached"));
    }

    #[test]
    fn prompt_with_goal_mode_ignores_disabled_or_empty_goal() {
        assert_eq!(
            build_prompt_with_goal_mode("Continue", false, "Goal", "Criteria"),
            "Continue"
        );
        assert_eq!(
            build_prompt_with_goal_mode("Continue", true, "   ", "Criteria"),
            "Continue"
        );
    }

    #[test]
    fn file_entry_matches_query_across_name_extension_and_path() {
        let path = PathBuf::from("/tmp/oxide/crates/oxide-desktop/src/lib.rs");

        assert!(file_entry_matches_query(&path, "desktop"));
        assert!(file_entry_matches_query(&path, "src/lib"));
        assert!(file_entry_matches_query(&path, "rs"));
        assert!(file_entry_matches_query(&path, ""));
        assert!(!file_entry_matches_query(&path, "package-json"));
    }

    #[test]
    fn read_file_context_limits_content_without_breaking_utf8() {
        let tmp = unique_tmp("file-context");
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("note.md");
        std::fs::write(&path, "alpha\nbeta\ncargo 🧪\n").unwrap();

        let context = read_file_context(&path, 12).unwrap();

        assert_eq!(context, "alpha\nbeta\nc\n\n... file context truncated ...");
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn prompt_with_file_context_includes_path_and_content() {
        let prompt = build_prompt_with_file_context(
            "Review this",
            Path::new("/tmp/oxide/README.md"),
            "# Oxide",
        );

        assert!(prompt.contains("Review this"));
        assert!(prompt.contains("Attached file context:"));
        assert!(prompt.contains("Path: /tmp/oxide/README.md"));
        assert!(prompt.contains("```text\n# Oxide\n```"));
    }

    #[test]
    fn prompt_with_appshots_includes_visual_annotations() {
        let appshot = AppshotSpec {
            id: "login".to_string(),
            title: "Login screen".to_string(),
            path: "/tmp/login.png".to_string(),
            note: "Modal clipped".to_string(),
            annotations: vec![
                AppshotAnnotation {
                    label: "A".to_string(),
                    target: "top-right modal".to_string(),
                    note: "Close button overlaps title".to_string(),
                },
                AppshotAnnotation {
                    label: "B".to_string(),
                    target: "footer".to_string(),
                    note: "Button is below fold".to_string(),
                },
            ],
            source_url: "http://localhost:3000/login".to_string(),
            browser_action_id: "open-target-42".to_string(),
            created_ms: 1,
        };

        let prompt = build_prompt_with_appshots("Fix this UI", &[appshot]);

        assert!(prompt.contains("Attached Appshots"));
        assert!(prompt.contains("- Login screen: /tmp/login.png"));
        assert!(prompt.contains("Source URL: http://localhost:3000/login"));
        assert!(prompt.contains("Browser action: open-target-42"));
        assert!(prompt.contains("Annotations:"));
        assert!(prompt.contains("[A] top-right modal: Close button overlaps title"));
        assert!(prompt.contains("[B] footer: Button is below fold"));
    }

    #[test]
    fn appshot_specs_roundtrip_preserves_annotations() {
        let tmp = unique_tmp("appshot-annotations");
        std::fs::create_dir_all(&tmp).unwrap();
        let spec = AppshotSpec {
            id: "annotated".to_string(),
            title: "Annotated".to_string(),
            path: "/tmp/annotated.png".to_string(),
            note: "Visual check".to_string(),
            annotations: vec![AppshotAnnotation {
                label: "1".to_string(),
                target: "header".to_string(),
                note: "Text clips".to_string(),
            }],
            source_url: String::new(),
            browser_action_id: String::new(),
            created_ms: 2,
        };

        write_appshot_spec(&tmp, &spec).unwrap();
        let specs = read_appshot_specs(&tmp).unwrap();

        assert_eq!(specs, vec![spec]);
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn appshot_with_added_annotation_trims_fields_and_rejects_empty_note() {
        let spec = AppshotSpec {
            id: "annotated".to_string(),
            title: "Annotated".to_string(),
            path: "/tmp/annotated.png".to_string(),
            note: "Visual check".to_string(),
            annotations: Vec::new(),
            source_url: String::new(),
            browser_action_id: String::new(),
            created_ms: 2,
        };

        let updated =
            appshot_with_added_annotation(&spec, " A ", " header ", " Text clips ").unwrap();

        assert_eq!(updated.annotations.len(), 1);
        assert_eq!(updated.annotations[0].label, "A");
        assert_eq!(updated.annotations[0].target, "header");
        assert_eq!(updated.annotations[0].note, "Text clips");
        assert!(appshot_with_added_annotation(&spec, "A", "header", "").is_err());
    }

    #[test]
    fn appshot_with_browser_source_trims_url_and_action_id() {
        let spec = AppshotSpec {
            id: "login".to_string(),
            title: "Login".to_string(),
            path: "/tmp/login.png".to_string(),
            note: String::new(),
            annotations: Vec::new(),
            source_url: String::new(),
            browser_action_id: String::new(),
            created_ms: 2,
        };

        let updated =
            appshot_with_browser_source(spec, " http://localhost:3000/login ", " open-target-42 ");

        assert_eq!(updated.source_url, "http://localhost:3000/login");
        assert_eq!(updated.browser_action_id, "open-target-42");
    }

    #[test]
    fn browser_snapshot_appshot_draft_uses_url_note_and_action_provenance() {
        let workspace = PathBuf::from("/tmp/oxide");

        let draft = browser_snapshot_appshot_draft(
            &workspace,
            "http://localhost:3000/login",
            "Capture login error",
            "agent-snapshot-request-42",
            99,
        )
        .unwrap();

        assert_eq!(draft.title, "Browser snapshot login");
        assert_eq!(draft.note, "Capture login error");
        assert_eq!(draft.source_url, "http://localhost:3000/login");
        assert_eq!(draft.browser_action_id, "agent-snapshot-request-42");
        assert_eq!(
            PathBuf::from(&draft.path),
            PathBuf::from("/tmp/oxide/.oxide/appshots/files/browser-snapshot-login-99.png")
        );
    }

    #[test]
    fn browser_snapshot_appshot_draft_uses_default_note_when_empty() {
        let workspace = PathBuf::from("/tmp/oxide");

        let draft = browser_snapshot_appshot_draft(
            &workspace,
            "http://localhost:3000",
            "",
            "agent-snapshot-request-42",
            99,
        )
        .unwrap();

        assert_eq!(draft.title, "Browser snapshot target");
        assert_eq!(draft.note, "Agent requested browser snapshot");
    }

    #[test]
    fn model_catalog_uses_two_current_models_per_external_provider() {
        let models_for = |provider: &str| {
            MODELS
                .iter()
                .filter(|model| model.provider == provider)
                .map(|model| model.model)
                .collect::<Vec<_>>()
        };

        assert_eq!(models_for("codex"), vec!["gpt-5.5", "gpt-5.3-codex-spark"]);
        assert_eq!(models_for("openai"), vec!["gpt-5.5", "gpt-5.4"]);
        assert_eq!(
            models_for("gemini"),
            vec!["gemini-3.1-pro", "gemini-3.5-flash"]
        );
        assert_eq!(models_for("xai"), vec!["grok-4.3", "grok-build-0.1"]);
        assert_eq!(
            models_for("deepseek"),
            vec!["deepseek-v4-pro", "deepseek-v4-flash"]
        );
        assert_eq!(
            models_for("mistral"),
            vec!["mistral-medium-3-5", "mistral-small-4"]
        );
        assert_eq!(
            models_for("anthropic"),
            vec!["claude-opus-4-8", "claude-sonnet-4-6"]
        );
        assert!(MODELS.iter().all(|model| model.model != "chat-latest"));
    }

    #[test]
    fn shortcut_catalog_contains_core_desktop_actions() {
        let shortcuts = shortcut_catalog();

        assert!(shortcuts.iter().any(|item| item.id == "command-palette"));
        assert!(shortcuts.iter().any(|item| item.id == "settings"));
        assert!(shortcuts.iter().any(|item| item.id == "send-prompt"));
        assert!(shortcuts.iter().any(|item| item.id == "terminal-run"));
        assert!(shortcuts
            .iter()
            .any(|item| item.id == "approval-approve" && item.scope == "Approvals"));
        assert!(shortcuts
            .iter()
            .any(|item| item.id == "checkpoint-rewind" && item.scope == "Checkpoints"));
        assert!(shortcuts
            .iter()
            .any(|item| item.id == "usage-open" && item.scope == "Usage"));
        assert!(shortcuts
            .iter()
            .any(|item| item.id == "goal-open" && item.scope == "Goal"));
    }

    #[test]
    fn filter_shortcuts_matches_title_scope_and_keys() {
        let shortcuts = shortcut_catalog();

        let palette = filter_shortcuts(&shortcuts, "cmd+k");
        let terminal = filter_shortcuts(&shortcuts, "terminal");

        assert_eq!(palette[0].id, "command-palette");
        assert!(terminal.iter().any(|item| item.id == "terminal-run"));
    }

    #[test]
    fn command_catalog_contains_core_executable_actions() {
        let commands = command_catalog();

        assert!(commands.iter().any(|item| item.id == "new-chat"));
        assert!(commands.iter().any(|item| item.id == "search-threads"));
        assert!(commands
            .iter()
            .any(|item| item.id == "settings-personalization"));
        assert!(commands.iter().any(|item| item.id == "settings-plugins"));
        assert!(commands.iter().any(|item| item.id == "git-refresh-diff"));
        assert!(commands
            .iter()
            .any(|item| item.id == "git-stage-selected-file"));
        assert!(commands.iter().any(|item| item.id == "hermes-start-evolve"));
        assert!(commands
            .iter()
            .any(|item| item.id == "appshot-capture-screen"));
        assert!(commands.iter().any(|item| item.id == "browser-open-target"));
        assert!(commands
            .iter()
            .any(|item| item.id == "browser-insert-context"));
        assert!(commands
            .iter()
            .any(|item| item.id == "browser-capture-pending-snapshot"));
        assert!(commands
            .iter()
            .any(|item| item.id == "terminal-stop-running-command"));
        assert!(commands.iter().any(|item| item.id == "inspector-approvals"));
        assert!(commands
            .iter()
            .any(|item| item.id == "inspector-checkpoints"));
        assert!(commands.iter().any(|item| item.id == "inspector-usage"));
        assert!(commands.iter().any(|item| item.id == "inspector-goal"));
        assert!(commands.iter().any(|item| item.id == "inspector-terminal"));
    }

    #[test]
    fn command_visibility_uses_contextual_state() {
        assert!(!command_visible_for_state(
            "thread-rename-selected",
            false,
            false,
            false,
            false
        ));
        assert!(command_visible_for_state(
            "thread-rename-selected",
            true,
            false,
            false,
            false
        ));
        assert!(!command_visible_for_state(
            "browser-capture-pending-snapshot",
            true,
            false,
            false,
            false
        ));
        assert!(command_visible_for_state(
            "browser-capture-pending-snapshot",
            false,
            true,
            false,
            false
        ));
        assert!(!command_visible_for_state(
            "git-stage-selected-file",
            false,
            false,
            false,
            false
        ));
        assert!(command_visible_for_state(
            "git-stage-selected-file",
            false,
            false,
            true,
            false
        ));
        assert!(!command_visible_for_state(
            "terminal-stop-running-command",
            false,
            false,
            true,
            false
        ));
        assert!(command_visible_for_state(
            "terminal-stop-running-command",
            false,
            false,
            false,
            true
        ));
        assert!(command_visible_for_state(
            "settings", false, false, false, false
        ));
    }

    #[test]
    fn settings_commands_map_to_tabs_and_open_window() {
        assert_eq!(
            settings_tab_for_command("settings-personalization"),
            Some(SettingsTab::Personalization)
        );
        assert!(settings_command_opens_window("settings-personalization"));
        assert!(settings_command_opens_window("settings"));
    }

    #[test]
    fn pending_approval_queue_upserts_and_removes_requests() {
        let mut approvals = Vec::new();

        upsert_pending_approval(
            &mut approvals,
            PendingApproval {
                request_id: 7,
                tool: "write_file".to_string(),
                summary: "Write README.md".to_string(),
                created_ms: 1,
            },
        );
        upsert_pending_approval(
            &mut approvals,
            PendingApproval {
                request_id: 7,
                tool: "write_file".to_string(),
                summary: "Write README.md safely".to_string(),
                created_ms: 2,
            },
        );

        assert_eq!(approvals.len(), 1);
        assert_eq!(approvals[0].summary, "Write README.md safely");
        remove_pending_approval(&mut approvals, 7);
        assert!(approvals.is_empty());
    }

    #[test]
    fn timeline_approval_request_is_cleared_after_answer() {
        let mut timeline = vec![
            TimelineItem {
                title: "Approval: write_file".to_string(),
                detail: "Write README.md".to_string(),
                state: TimelineState::Waiting,
                request_id: Some(7),
            },
            TimelineItem {
                title: "Other approval".to_string(),
                detail: "Keep this one active".to_string(),
                state: TimelineState::Waiting,
                request_id: Some(8),
            },
        ];

        clear_timeline_approval_request(&mut timeline, 7);

        assert_eq!(timeline[0].request_id, None);
        assert_eq!(timeline[1].request_id, Some(8));
    }

    #[test]
    fn checkpoint_queue_upserts_and_marks_rewound() {
        let mut checkpoints = Vec::new();

        upsert_checkpoint(
            &mut checkpoints,
            WorkspaceCheckpoint {
                id: 3,
                label: "write README.md".to_string(),
                created_ms: 1,
                rewound: false,
                restored_files: None,
            },
        );
        upsert_checkpoint(
            &mut checkpoints,
            WorkspaceCheckpoint {
                id: 3,
                label: "write README.md safely".to_string(),
                created_ms: 2,
                rewound: false,
                restored_files: None,
            },
        );

        mark_checkpoint_rewound(&mut checkpoints, 3, 2);

        assert_eq!(checkpoints.len(), 1);
        assert_eq!(checkpoints[0].label, "write README.md safely");
        assert!(checkpoints[0].rewound);
        assert_eq!(checkpoints[0].restored_files, Some(2));
    }

    #[test]
    fn checkpoint_rewind_marks_target_and_newer_checkpoints() {
        let mut checkpoints = vec![
            WorkspaceCheckpoint {
                id: 1,
                label: "older".to_string(),
                created_ms: 1,
                rewound: false,
                restored_files: None,
            },
            WorkspaceCheckpoint {
                id: 2,
                label: "target".to_string(),
                created_ms: 2,
                rewound: false,
                restored_files: None,
            },
            WorkspaceCheckpoint {
                id: 3,
                label: "newer".to_string(),
                created_ms: 3,
                rewound: false,
                restored_files: None,
            },
        ];

        mark_checkpoint_rewound(&mut checkpoints, 2, 4);

        assert!(!checkpoints[0].rewound);
        assert!(checkpoints[1].rewound);
        assert_eq!(checkpoints[1].restored_files, Some(4));
        assert!(checkpoints[2].rewound);
        assert_eq!(checkpoints[2].restored_files, None);
    }

    #[test]
    fn token_usage_records_latest_and_context_percent() {
        let mut records = Vec::new();

        record_token_usage(&mut records, 2, 1200, 300, 10);
        record_token_usage(&mut records, 3, 2200, 400, 11);

        let latest = latest_token_usage_summary(&records).unwrap();
        assert_eq!(latest, "turn-3 · input 2200 · output 400 · total 2600");
        assert_eq!(context_usage_percent(Some(5000), Some(2500)), Some(50.0));
        assert_eq!(context_usage_percent(Some(0), Some(2500)), None);
    }

    #[test]
    fn compaction_records_keep_latest_context_tokens() {
        let mut records = Vec::new();

        record_compaction(&mut records, 4, 1800, 20);

        assert_eq!(latest_compaction_tokens(&records), Some(1800));
        assert_eq!(records[0].dropped, 4);
    }

    #[test]
    fn latest_context_tokens_uses_newest_usage_or_compaction_event() {
        let usage = vec![TokenUsageRecord {
            turn: 5,
            input: 3200,
            output: 200,
            created_ms: 30,
        }];
        let compactions = vec![CompactionRecord {
            dropped: 4,
            tokens: 1800,
            created_ms: 20,
        }];

        assert_eq!(latest_context_tokens(&usage, &compactions), Some(3200));

        let newer_compactions = vec![CompactionRecord {
            dropped: 2,
            tokens: 1500,
            created_ms: 40,
        }];
        assert_eq!(
            latest_context_tokens(&usage, &newer_compactions),
            Some(1500)
        );
    }

    #[test]
    fn filter_commands_matches_id_title_detail_and_keys() {
        let commands = command_catalog();

        let by_key = filter_commands(&commands, "cmd+k");
        let by_id = filter_commands(&commands, "git refresh");
        let by_detail = filter_commands(&commands, "connector");

        assert_eq!(by_key[0].id, "command-palette");
        assert!(by_id.iter().any(|item| item.id == "git-refresh-diff"));
        assert!(by_detail.iter().any(|item| item.id == "settings-plugins"));
    }

    #[test]
    fn global_search_includes_command_results() {
        let tmp = unique_tmp("global-search-commands");
        std::fs::create_dir_all(&tmp).unwrap();

        let results = build_global_search_results(GlobalSearchInputs {
            workspace: &tmp,
            sessions: &[],
            memories: &[],
            automations: &[],
            appshots: &[],
            hermes_profiles: &[],
            mcp_servers: &[],
            shortcuts: &[],
            goal_mode_enabled: false,
            active_goal: "",
            goal_success_criteria: "",
            query: "git diff",
        });

        assert!(results
            .iter()
            .any(|item| { item.kind == "Command" && item.target == "command:git-refresh-diff" }));
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn global_search_usage_shortcut_opens_usage_command() {
        let tmp = unique_tmp("global-search-usage-shortcut");
        std::fs::create_dir_all(&tmp).unwrap();
        let shortcuts = shortcut_catalog();

        let results = build_global_search_results(GlobalSearchInputs {
            workspace: &tmp,
            sessions: &[],
            memories: &[],
            automations: &[],
            appshots: &[],
            hermes_profiles: &[],
            mcp_servers: &[],
            shortcuts: &shortcuts,
            goal_mode_enabled: false,
            active_goal: "",
            goal_success_criteria: "",
            query: "open usage",
        });

        assert!(results
            .iter()
            .any(|item| { item.kind == "Shortcut" && item.target == "command:inspector-usage" }));
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn global_search_includes_active_goal_result() {
        let tmp = unique_tmp("global-search-goal");
        std::fs::create_dir_all(&tmp).unwrap();

        let results = build_global_search_results(GlobalSearchInputs {
            workspace: &tmp,
            sessions: &[],
            memories: &[],
            automations: &[],
            appshots: &[],
            hermes_profiles: &[],
            mcp_servers: &[],
            shortcuts: &[],
            goal_mode_enabled: true,
            active_goal: "Reach Codex desktop parity",
            goal_success_criteria: "All goal prompts are idempotent.",
            query: "desktop parity",
        });

        assert!(results
            .iter()
            .any(|item| { item.kind == "Goal" && item.target == "goal:active" }));
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn global_search_includes_run_action_for_automation_results() {
        let tmp = unique_tmp("global-search-automation-run");
        std::fs::create_dir_all(&tmp).unwrap();
        let automation = AutomationSpec {
            id: "daily-review".to_string(),
            name: "Daily review".to_string(),
            kind: "cron".to_string(),
            status: "ACTIVE".to_string(),
            schedule: "daily".to_string(),
            prompt: "Review workspace".to_string(),
            created_ms: 1,
        };

        let results = build_global_search_results(GlobalSearchInputs {
            workspace: &tmp,
            sessions: &[],
            memories: &[],
            automations: &[automation],
            appshots: &[],
            hermes_profiles: &[],
            mcp_servers: &[],
            shortcuts: &[],
            goal_mode_enabled: false,
            active_goal: "",
            goal_success_criteria: "",
            query: "run daily",
        });

        assert!(results.iter().any(|item| {
            item.kind == "Command"
                && item.title == "Run automation: Daily review"
                && item.target == "automation-run:daily-review"
        }));
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn command_palette_empty_query_starts_with_visible_commands_only() {
        let tmp = unique_tmp("palette-empty");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("README.md"), "hello").unwrap();
        let repo_index = collect_repo_index(&tmp, REPO_INDEX_ENTRY_LIMIT);

        let results = build_command_palette_results(CommandPaletteInputs {
            search: GlobalSearchInputs {
                workspace: &tmp,
                sessions: &[],
                memories: &[],
                automations: &[],
                appshots: &[],
                hermes_profiles: &[],
                mcp_servers: &[],
                shortcuts: &[],
                goal_mode_enabled: false,
                active_goal: "",
                goal_success_criteria: "",
                query: "",
            },
            repo_index: &repo_index,
            has_selected_session: false,
            has_pending_browser_snapshot: false,
            has_selected_git_file: false,
            has_active_terminal_job: false,
        });

        assert_eq!(results[0].target, "command:new-chat");
        assert!(results.iter().all(|item| item.kind == "Command"));
        assert!(!results
            .iter()
            .any(|item| item.target == "command:command-palette"));
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn command_palette_query_searches_threads_files_and_artifacts() {
        let tmp = unique_tmp("palette-search");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("README.md"), "hello").unwrap();
        let repo_index = collect_repo_index(&tmp, REPO_INDEX_ENTRY_LIMIT);
        let sessions = vec![SessionSummary {
            id: "alpha".to_string(),
            title: "Pinned Build".to_string(),
            path: PathBuf::from("alpha.jsonl"),
            message_count: 3,
            last_ts_ms: 5,
            pinned: true,
            archived: false,
        }];

        let file_results = build_command_palette_results(CommandPaletteInputs {
            search: GlobalSearchInputs {
                workspace: &tmp,
                sessions: &sessions,
                memories: &[],
                automations: &[],
                appshots: &[],
                hermes_profiles: &[],
                mcp_servers: &[],
                shortcuts: &[],
                goal_mode_enabled: true,
                active_goal: "Reach Codex desktop parity",
                goal_success_criteria: "",
                query: "readme",
            },
            repo_index: &repo_index,
            has_selected_session: false,
            has_pending_browser_snapshot: false,
            has_selected_git_file: false,
            has_active_terminal_job: false,
        });
        assert!(file_results
            .iter()
            .any(|item| item.kind == "File" && item.title == "README.md"));

        let goal_results = build_command_palette_results(CommandPaletteInputs {
            search: GlobalSearchInputs {
                workspace: &tmp,
                sessions: &sessions,
                memories: &[],
                automations: &[],
                appshots: &[],
                hermes_profiles: &[],
                mcp_servers: &[],
                shortcuts: &[],
                goal_mode_enabled: true,
                active_goal: "Reach Codex desktop parity",
                goal_success_criteria: "",
                query: "desktop parity",
            },
            repo_index: &repo_index,
            has_selected_session: false,
            has_pending_browser_snapshot: false,
            has_selected_git_file: false,
            has_active_terminal_job: false,
        });
        assert!(goal_results.iter().any(|item| item.target == "goal:active"));

        let thread_results = build_command_palette_results(CommandPaletteInputs {
            search: GlobalSearchInputs {
                workspace: &tmp,
                sessions: &sessions,
                memories: &[],
                automations: &[],
                appshots: &[],
                hermes_profiles: &[],
                mcp_servers: &[],
                shortcuts: &[],
                goal_mode_enabled: false,
                active_goal: "",
                goal_success_criteria: "",
                query: "pinned build",
            },
            repo_index: &repo_index,
            has_selected_session: false,
            has_pending_browser_snapshot: false,
            has_selected_git_file: false,
            has_active_terminal_job: false,
        });
        assert!(thread_results
            .iter()
            .any(|item| item.kind == "Thread" && item.target == "session:alpha"));
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn command_palette_query_preserves_workspace_artifact_categories() {
        let tmp = unique_tmp("palette-artifacts");
        std::fs::create_dir_all(&tmp).unwrap();
        let memories = vec![MemorySpec {
            id: "workflow".to_string(),
            title: "Workflow memory".to_string(),
            body: "Prefer focused Rust-native review slices.".to_string(),
            enabled: true,
            created_ms: 1,
        }];
        let automations = vec![AutomationSpec {
            id: "daily-review".to_string(),
            name: "Daily review".to_string(),
            kind: "cron".to_string(),
            status: "ACTIVE".to_string(),
            schedule: "daily".to_string(),
            prompt: "Review workspace".to_string(),
            created_ms: 2,
        }];
        let appshots = vec![AppshotSpec {
            id: "login".to_string(),
            title: "Login screen".to_string(),
            path: "/tmp/login.png".to_string(),
            note: "Review modal clipped".to_string(),
            annotations: Vec::new(),
            source_url: String::new(),
            browser_action_id: String::new(),
            created_ms: 3,
        }];
        let hermes = vec![HermesProfile {
            id: "desktop-parity".to_string(),
            name: "Desktop parity".to_string(),
            goal: "Improve parity".to_string(),
            validation: "cargo test".to_string(),
            review_prompt: "Review risks".to_string(),
            created_ms: 4,
        }];
        let mcp = vec![McpServerConfig {
            name: "filesystem".to_string(),
            command: "npx".to_string(),
            args: vec!["server".to_string()],
            ..McpServerConfig::default()
        }];
        let shortcuts = shortcut_catalog();
        let repo_index = collect_repo_index(&tmp, REPO_INDEX_ENTRY_LIMIT);

        let results_for = |query: &str| {
            build_command_palette_results(CommandPaletteInputs {
                search: GlobalSearchInputs {
                    workspace: &tmp,
                    sessions: &[],
                    memories: &memories,
                    automations: &automations,
                    appshots: &appshots,
                    hermes_profiles: &hermes,
                    mcp_servers: &mcp,
                    shortcuts: &shortcuts,
                    goal_mode_enabled: false,
                    active_goal: "",
                    goal_success_criteria: "",
                    query,
                },
                repo_index: &repo_index,
                has_selected_session: false,
                has_pending_browser_snapshot: false,
                has_selected_git_file: false,
                has_active_terminal_job: false,
            })
        };

        assert!(results_for("rust-native review")
            .iter()
            .any(|item| item.kind == "Memory"));
        assert!(results_for("run daily")
            .iter()
            .any(|item| item.target == "automation-run:daily-review"));
        assert!(results_for("login screen")
            .iter()
            .any(|item| item.kind == "Appshot"));
        assert!(results_for("desktop parity")
            .iter()
            .any(|item| item.kind == "Hermes"));
        assert!(results_for("filesystem server")
            .iter()
            .any(|item| item.kind == "MCP"));
        assert!(results_for("open usage")
            .iter()
            .any(|item| item.kind == "Shortcut" && item.target == "command:inspector-usage"));
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn command_palette_hides_contextual_commands_until_state_allows_them() {
        let tmp = unique_tmp("palette-contextual");
        std::fs::create_dir_all(&tmp).unwrap();
        let repo_index = collect_repo_index(&tmp, REPO_INDEX_ENTRY_LIMIT);

        let targets_for =
            |query: &str, has_snapshot: bool, has_git_file: bool, has_terminal_job: bool| {
                build_command_palette_results(CommandPaletteInputs {
                    search: GlobalSearchInputs {
                        workspace: &tmp,
                        sessions: &[],
                        memories: &[],
                        automations: &[],
                        appshots: &[],
                        hermes_profiles: &[],
                        mcp_servers: &[],
                        shortcuts: &[],
                        goal_mode_enabled: false,
                        active_goal: "",
                        goal_success_criteria: "",
                        query,
                    },
                    repo_index: &repo_index,
                    has_selected_session: false,
                    has_pending_browser_snapshot: has_snapshot,
                    has_selected_git_file: has_git_file,
                    has_active_terminal_job: has_terminal_job,
                })
                .into_iter()
                .map(|item| item.target)
                .collect::<Vec<_>>()
            };

        assert!(
            !targets_for("capture pending browser snapshot", false, false, false)
                .iter()
                .any(|target| target == "command:browser-capture-pending-snapshot")
        );
        assert!(
            targets_for("capture pending browser snapshot", true, false, false)
                .iter()
                .any(|target| target == "command:browser-capture-pending-snapshot")
        );
        assert!(!targets_for("stage selected file", false, false, false)
            .iter()
            .any(|target| target == "command:git-stage-selected-file"));
        assert!(targets_for("stage selected file", false, true, false)
            .iter()
            .any(|target| target == "command:git-stage-selected-file"));
        assert!(
            !targets_for("stop running terminal command", false, true, false)
                .iter()
                .any(|target| target == "command:terminal-stop-running-command")
        );
        assert!(
            targets_for("stop running terminal command", false, false, true)
                .iter()
                .any(|target| target == "command:terminal-stop-running-command")
        );
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn palette_action_matches_query_title_and_subtitle() {
        assert!(palette_action_matches(
            "Configure MCP servers",
            "Open plugin connector settings",
            "connector"
        ));
        assert!(!palette_action_matches(
            "Configure MCP servers",
            "Open plugin connector settings",
            "archive"
        ));
    }

    #[test]
    fn provider_binary_for_reports_cli_backends_only() {
        assert_eq!(provider_binary_for("codex"), Some("codex"));
        assert_eq!(provider_binary_for("claude"), Some("claude"));
        assert_eq!(provider_binary_for("openai"), None);
        assert_eq!(provider_binary_for("anthropic"), None);
    }

    #[test]
    fn provider_auth_hint_reports_required_api_key_state() {
        assert!(!secret_value_present(None));
        assert!(!secret_value_present(Some("   ")));
        assert!(secret_value_present(Some("secret")));
        assert_eq!(
            provider_auth_hint("openai", false, false, false, false, false, false),
            ("OPENAI_API_KEY", "missing")
        );
        assert_eq!(
            provider_auth_hint("openai", true, false, false, false, false, false),
            ("OPENAI_API_KEY", "present")
        );
        assert_eq!(
            provider_auth_hint("anthropic", false, true, false, false, false, false),
            ("ANTHROPIC_API_KEY", "present")
        );
        assert_eq!(
            provider_auth_hint("gemini", false, false, false, false, false, false),
            ("GEMINI_API_KEY", "missing")
        );
        assert_eq!(
            provider_auth_hint("xai", false, false, false, false, false, false),
            ("XAI_API_KEY", "missing")
        );
        assert_eq!(
            provider_auth_hint("deepseek", false, false, false, false, false, false),
            ("DEEPSEEK_API_KEY", "missing")
        );
        assert_eq!(
            provider_auth_hint("mistral", false, false, false, false, false, false),
            ("MISTRAL_API_KEY", "missing")
        );
        assert_eq!(
            provider_auth_hint("codex", false, false, false, false, false, false),
            ("codex CLI", "required")
        );
    }

    #[test]
    fn diagnostic_config_preview_contains_runtime_settings() {
        let mut cfg = Config::default();
        cfg.provider = "codex".to_string();
        cfg.model = "gpt-5.5".to_string();
        cfg.reasoning_effort = "high".to_string();
        cfg.workspace = Some(PathBuf::from("/tmp/oxide"));

        let preview = diagnostic_config_preview(&cfg);

        assert!(preview.contains("provider = codex"));
        assert!(preview.contains("model = gpt-5.5"));
        assert!(preview.contains("reasoning_effort = high"));
        assert!(preview.contains("/tmp/oxide"));
    }

    #[test]
    fn appshot_specs_roundtrip_from_workspace_store() {
        let tmp = unique_tmp("appshots");
        std::fs::create_dir_all(&tmp).unwrap();
        let spec = AppshotSpec {
            id: "login-screen".to_string(),
            title: "Login screen".to_string(),
            path: "/tmp/login.png".to_string(),
            note: "Modal overlaps footer".to_string(),
            annotations: Vec::new(),
            source_url: String::new(),
            browser_action_id: String::new(),
            created_ms: 44,
        };

        write_appshot_spec(&tmp, &spec).unwrap();
        let specs = read_appshot_specs(&tmp).unwrap();

        assert_eq!(specs, vec![spec]);
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn browser_actions_roundtrip_from_workspace_store() {
        let tmp = unique_tmp("browser-actions");
        std::fs::create_dir_all(&tmp).unwrap();
        let spec = BrowserActionSpec {
            id: "open-target-42".to_string(),
            action: "open-target".to_string(),
            url: "http://localhost:3000".to_string(),
            note: "Login page ready".to_string(),
            created_ms: 42,
        };

        write_browser_action_spec(&tmp, &spec).unwrap();
        let specs = read_browser_action_specs(&tmp).unwrap();

        assert_eq!(specs, vec![spec]);
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn browser_action_from_fields_trims_and_requires_url() {
        let spec =
            browser_action_from_fields(" open-target ", " http://localhost:3000 ", " ready ", 42)
                .unwrap();

        assert_eq!(spec.id, "open-target-42");
        assert_eq!(spec.action, "open-target");
        assert_eq!(spec.url, "http://localhost:3000");
        assert_eq!(spec.note, "ready");
        assert!(browser_action_from_fields("open-target", " ", "ready", 42).is_err());
    }

    #[test]
    fn prompt_with_browser_context_includes_target_and_recent_actions() {
        let actions = vec![BrowserActionSpec {
            id: "open-target-42".to_string(),
            action: "open-target".to_string(),
            url: "http://localhost:3000".to_string(),
            note: "Login page ready".to_string(),
            created_ms: 42,
        }];

        let prompt =
            build_prompt_with_browser_context("Fix this flow", "http://localhost:3000", &actions);

        assert!(prompt.contains("Browser Target: http://localhost:3000"));
        assert!(prompt.contains("- open-target: http://localhost:3000"));
        assert!(prompt.contains("Login page ready"));
    }

    #[test]
    fn prompt_with_browser_context_does_not_duplicate_existing_context() {
        let actions = vec![BrowserActionSpec {
            id: "open-target-42".to_string(),
            action: "open-target".to_string(),
            url: "http://localhost:3000".to_string(),
            note: "Login page ready".to_string(),
            created_ms: 42,
        }];
        let prompt = build_prompt_with_browser_context(
            "Fix this flow\n\nBrowser Context:\nBrowser Target: http://localhost:3000",
            "http://localhost:3000",
            &actions,
        );

        assert_eq!(prompt.matches("Browser Context:").count(), 1);
    }

    #[test]
    fn appshot_capture_path_uses_files_store_and_slug() {
        let workspace = PathBuf::from("/tmp/oxide");

        let path = appshot_capture_path_for(&workspace, "Login Screen", 42);

        assert_eq!(
            path,
            PathBuf::from("/tmp/oxide/.oxide/appshots/files/login-screen-42.png")
        );
    }

    #[test]
    fn captured_appshot_spec_uses_path_title_note_and_timestamp() {
        let path = PathBuf::from("/tmp/login-screen-42.png");

        let spec = captured_appshot_spec("Login Screen", &path, "Modal clipped", 42);

        assert_eq!(spec.id, "login-screen-42");
        assert_eq!(spec.title, "Login Screen");
        assert_eq!(spec.path, "/tmp/login-screen-42.png");
        assert_eq!(spec.note, "Modal clipped");
        assert_eq!(spec.created_ms, 42);
    }

    #[test]
    fn appshot_previewable_path_accepts_png_jpeg_and_webp() {
        assert!(is_previewable_appshot_path(Path::new("screen.png")));
        assert!(is_previewable_appshot_path(Path::new("screen.JPG")));
        assert!(is_previewable_appshot_path(Path::new("screen.jpeg")));
        assert!(!is_previewable_appshot_path(Path::new("screen.txt")));
    }

    #[test]
    fn load_appshot_color_image_decodes_png_dimensions() {
        let tmp = unique_tmp("appshot-image");
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("one.png");
        image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255]))
            .save(&path)
            .unwrap();

        let image = load_appshot_color_image(&path).unwrap();

        assert_eq!(image.size, [1, 1]);
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn git_branch_snapshot_parses_branch_and_worktree_porcelain() {
        let branch = "main\n";
        let branches = "main\nfeature/evolve\nrelease\n";
        let worktrees = "worktree /tmp/oxide\nHEAD abc123\nbranch refs/heads/main\n\nworktree /tmp/oxide-evolve\nHEAD def456\nbranch refs/heads/feature/evolve\n";

        let snapshot = parse_git_branch_snapshot(branch, branches, worktrees);

        assert_eq!(snapshot.current_branch, "main");
        assert_eq!(
            snapshot.branches,
            vec![
                "main".to_string(),
                "feature/evolve".to_string(),
                "release".to_string()
            ]
        );
        assert_eq!(snapshot.worktrees.len(), 2);
        assert_eq!(snapshot.worktrees[1].path, "/tmp/oxide-evolve");
        assert_eq!(snapshot.worktrees[1].branch, "feature/evolve");
    }

    #[test]
    fn git_status_parser_extracts_codes_paths_and_rename_targets() {
        let files = parse_git_changed_files(
            " M crates/oxide-desktop/src/lib.rs\nA  README.md\n?? notes/todo.md\nR  old.rs -> new.rs\n",
        );

        assert_eq!(files.len(), 4);
        assert_eq!(files[0].status, "M");
        assert_eq!(files[0].path, "crates/oxide-desktop/src/lib.rs");
        assert_eq!(files[2].status, "??");
        assert_eq!(files[2].path, "notes/todo.md");
        assert_eq!(files[3].status, "R");
        assert_eq!(files[3].path, "new.rs");
        assert_eq!(files[3].display_path, "old.rs -> new.rs");
    }

    #[test]
    fn git_review_summary_includes_status_stat_and_file_rows() {
        let snapshot = GitSnapshot {
            status: " M src/lib.rs\n?? README.md\n".to_string(),
            diff_stat: " src/lib.rs | 2 +-\n".to_string(),
            raw_diff: String::new(),
            changed_files: vec![
                GitChangedFile {
                    status: "M".to_string(),
                    path: "src/lib.rs".to_string(),
                    display_path: "src/lib.rs".to_string(),
                },
                GitChangedFile {
                    status: "??".to_string(),
                    path: "README.md".to_string(),
                    display_path: "README.md".to_string(),
                },
            ],
        };

        let summary = build_git_review_summary(&snapshot);

        assert!(summary.contains("Git review summary"));
        assert!(summary.contains("src/lib.rs | 2 +-"));
        assert!(summary.contains("- M src/lib.rs"));
        assert!(summary.contains("- ?? README.md"));
    }

    #[test]
    fn git_stage_unstage_and_commit_helpers_mutate_temp_repo_index() {
        let tmp = unique_tmp("git-actions");
        std::fs::create_dir_all(&tmp).unwrap();
        run_git(&tmp, &["init"]).unwrap();
        run_git(&tmp, &["config", "user.email", "oxide@example.test"]).unwrap();
        run_git(&tmp, &["config", "user.name", "Oxide Test"]).unwrap();
        std::fs::write(tmp.join("note.txt"), "hello").unwrap();
        let file = GitChangedFile {
            status: "??".to_string(),
            path: "note.txt".to_string(),
            display_path: "note.txt".to_string(),
        };

        stage_git_file(&tmp, &file).unwrap();
        let staged = run_git(&tmp, &["diff", "--cached", "--name-only"]).unwrap();
        assert!(staged.contains("note.txt"));

        unstage_git_file(&tmp, &file).unwrap();
        let unstaged = run_git(&tmp, &["diff", "--cached", "--name-only"]).unwrap();
        assert!(!unstaged.contains("note.txt"));

        stage_git_file(&tmp, &file).unwrap();
        commit_staged_changes(&tmp, "Add note").unwrap();
        let log = run_git(&tmp, &["log", "--oneline", "-1"]).unwrap();
        assert!(log.contains("Add note"));
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn commit_staged_changes_rejects_empty_message() {
        let tmp = unique_tmp("git-empty-message");
        std::fs::create_dir_all(&tmp).unwrap();

        let err = commit_staged_changes(&tmp, "   ").unwrap_err();

        assert!(err.contains("Commit message is required"));
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn push_git_branch_pushes_to_local_bare_remote() {
        let tmp = unique_tmp("git-push-branch");
        let repo = tmp.join("repo");
        let remote = tmp.join("remote.git");
        std::fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init"]).unwrap();
        run_git(&repo, &["checkout", "-b", "main"]).unwrap();
        run_git(&repo, &["config", "user.email", "oxide@example.test"]).unwrap();
        run_git(&repo, &["config", "user.name", "Oxide Test"]).unwrap();
        std::fs::write(repo.join("note.txt"), "hello").unwrap();
        run_git(&repo, &["add", "note.txt"]).unwrap();
        run_git(&repo, &["commit", "-m", "Add note"]).unwrap();
        std::process::Command::new("git")
            .arg("init")
            .arg("--bare")
            .arg(&remote)
            .output()
            .unwrap();
        run_git(
            &repo,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        )
        .unwrap();

        push_git_branch(&repo, "origin", "main").unwrap();

        let output = std::process::Command::new("git")
            .arg("--git-dir")
            .arg(&remote)
            .arg("rev-parse")
            .arg("--verify")
            .arg("refs/heads/main")
            .output()
            .unwrap();
        assert!(output.status.success());
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn push_git_branch_rejects_empty_remote_or_branch() {
        let tmp = unique_tmp("git-push-empty");
        std::fs::create_dir_all(&tmp).unwrap();

        assert!(push_git_branch(&tmp, " ", "main")
            .unwrap_err()
            .contains("Remote name is required"));
        assert!(push_git_branch(&tmp, "origin", " ")
            .unwrap_err()
            .contains("Branch name is required"));
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn github_compare_url_supports_https_and_ssh_remotes() {
        assert_eq!(
            github_compare_url("https://github.com/owner/repo.git", "feature/ui", "main"),
            Some("https://github.com/owner/repo/compare/main...feature/ui?expand=1".to_string())
        );
        assert_eq!(
            github_compare_url("git@github.com:owner/repo.git", "feature/ui", "develop"),
            Some("https://github.com/owner/repo/compare/develop...feature/ui?expand=1".to_string())
        );
        assert_eq!(
            github_compare_url("https://example.com/owner/repo", "feature", "main"),
            None
        );
    }

    #[test]
    fn build_git_pr_draft_includes_title_branch_base_body_and_summary() {
        let draft = build_git_pr_draft(
            "Desktop parity",
            "Adds publish controls",
            "feature/git-publish",
            "main",
            "Git review summary\n\nStatus:\nclean",
        );

        assert!(draft.contains("# Desktop parity"));
        assert!(draft.contains("Base: main"));
        assert!(draft.contains("Branch: feature/git-publish"));
        assert!(draft.contains("Adds publish controls"));
        assert!(draft.contains("Git review summary"));
    }

    #[test]
    fn terminal_stream_line_preserves_stdout_and_marks_stderr() {
        assert_eq!(
            terminal_stream_line(TerminalStream::Stdout, "ready"),
            "ready"
        );
        assert_eq!(
            terminal_stream_line(TerminalStream::Stderr, "warning"),
            "stderr: warning"
        );
    }

    #[test]
    fn terminal_finished_line_reports_exit_code() {
        assert_eq!(terminal_finished_line(0), "(exit 0)");
        assert_eq!(terminal_finished_line(-1), "(exit -1)");
    }

    #[test]
    fn motion_phase_is_bounded_and_repeating() {
        let a = motion_phase(0.25);
        let b = motion_phase(1.25);

        assert!((0.0..=1.0).contains(&a));
        assert!((0.0..=1.0).contains(&b));
        assert!((a - b).abs() < f32::EPSILON);
    }

    #[test]
    fn timeline_state_fill_pulses_running_state_when_motion_enabled() {
        let still = timeline_state_fill(TimelineState::Running, 0.5, false);
        let moving = timeline_state_fill(TimelineState::Running, 0.5, true);

        assert_ne!(still, moving);
        assert_eq!(timeline_state_fill(TimelineState::Done, 0.5, true), PANEL);
    }

    #[test]
    fn message_display_text_adds_streaming_caret_when_tail_is_active() {
        assert_eq!(
            message_display_text("hello", true, 0.1, true),
            "hello |".to_string()
        );
        assert_eq!(
            message_display_text("hello", true, 0.8, true),
            "hello".to_string()
        );
        assert_eq!(
            message_display_text("hello", false, 0.1, true),
            "hello".to_string()
        );
    }

    #[test]
    fn terminal_reader_and_watcher_emit_live_lines_and_finish() {
        let (tx, rx) = mpsc::channel();
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("printf 'out\\n'; printf 'err\\n' >&2")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let child = Arc::new(Mutex::new(child));

        spawn_terminal_reader(stdout, TerminalStream::Stdout, tx.clone());
        spawn_terminal_reader(stderr, TerminalStream::Stderr, tx.clone());
        spawn_terminal_watcher(7, child, tx);

        let mut lines = Vec::new();
        let mut finished = None;
        for _ in 0..12 {
            match rx.recv_timeout(Duration::from_secs(1)).unwrap() {
                TerminalEvent::Line(line) => lines.push(line),
                TerminalEvent::Finished { id, code } => {
                    finished = Some((id, code));
                    break;
                }
            }
        }

        assert!(lines.iter().any(|line| line == "out"));
        assert!(lines.iter().any(|line| line == "stderr: err"));
        assert_eq!(finished, Some((7, 0)));
    }

    #[test]
    fn worktree_command_uses_branch_and_workspace_sibling_path() {
        let workspace = PathBuf::from("/tmp/oxide");

        let command = build_worktree_command(&workspace, "feature/evolve");

        assert_eq!(
            command,
            "git worktree add /tmp/oxide-feature-evolve feature/evolve"
        );
    }

    #[test]
    fn compact_tool_result_folds_long_output_when_enabled() {
        let long = "x".repeat(2_400);

        let folded = compact_tool_result(&long, true);

        assert!(folded.contains("tool output folded"));
        assert!(folded.len() < long.len());
        assert_eq!(compact_tool_result("short", true), "short");
        assert_eq!(compact_tool_result(&long, false), long);
    }

    #[test]
    fn steer_prompt_wraps_operator_note() {
        let prompt = build_steer_prompt("focus on the failing test");

        assert!(prompt.contains("Steer the current workspace task"));
        assert!(prompt.contains("focus on the failing test"));
    }

    #[test]
    fn evolve_prompt_contains_goal_diff_and_validation_contract() {
        let prompt = build_evolve_prompt(
            "Improve the desktop UI",
            "cargo test -p oxide-desktop",
            "M src/lib.rs\n",
        );

        assert!(prompt.contains("Hermes evolve"));
        assert!(prompt.contains("Improve the desktop UI"));
        assert!(prompt.contains("cargo test -p oxide-desktop"));
        assert!(prompt.contains("M src/lib.rs"));
    }

    #[test]
    fn hermes_profiles_roundtrip_from_workspace_store() {
        let tmp = unique_tmp("hermes-profiles");
        std::fs::create_dir_all(&tmp).unwrap();
        let profile = HermesProfile {
            id: "desktop-parity".to_string(),
            name: "Desktop parity".to_string(),
            goal: "Improve Codex desktop parity".to_string(),
            validation: "cargo test -p oxide-desktop".to_string(),
            review_prompt: "Review UX and compile risks".to_string(),
            created_ms: 12,
        };

        write_hermes_profile(&tmp, &profile).unwrap();
        let profiles = read_hermes_profiles(&tmp).unwrap();

        assert_eq!(profiles, vec![profile]);
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn hermes_profile_from_fields_requires_name_goal_and_validation() {
        let profile = hermes_profile_from_fields(
            "Desktop parity",
            "Improve parity",
            "cargo test -p oxide-desktop",
            "Review risks",
            12,
        )
        .unwrap();

        assert_eq!(profile.id, "desktop-parity-12");
        assert_eq!(profile.name, "Desktop parity");
        assert!(hermes_profile_from_fields("", "goal", "test", "", 1).is_err());
        assert!(hermes_profile_from_fields("name", "", "test", "", 1).is_err());
        assert!(hermes_profile_from_fields("name", "goal", "", "", 1).is_err());
    }

    #[test]
    fn hermes_review_prompt_contains_goal_validation_and_review_gate() {
        let prompt = build_hermes_review_prompt(
            "Improve parity",
            "cargo test -p oxide-desktop",
            "Review UX and compile risks",
        );

        assert!(prompt.contains("Hermes review loop"));
        assert!(prompt.contains("Improve parity"));
        assert!(prompt.contains("cargo test -p oxide-desktop"));
        assert!(prompt.contains("Review UX and compile risks"));
    }
}
