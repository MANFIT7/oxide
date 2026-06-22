//! Pluggable harness system.
//!
//! A *harness* packages everything that makes the agent behave a certain way:
//! its system prompt, the set of tools it may call, and per-turn loop policy
//! (max steps, temperature, model override). The engine stays fixed; behavior
//! is updated by swapping or adding harnesses — including external ones loaded
//! from TOML manifests at runtime. "hermes" ships as a builtin example so new
//! harnesses can be dropped in the same way later.
//!
//! ```text
//! Registry
//!  ├─ builtin: default   (general coding agent)
//!  ├─ builtin: hermes     (planning-forward harness)
//!  └─ manifest: <dir>/*.toml  (user/third-party, hot-droppable)
//! ```

use anyhow::{Context, Result};
use oxide_protocol::ToolSpec;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Tunables that shape a single turn's loop.
#[derive(Debug, Clone)]
pub struct LoopPolicy {
    /// Max tool-call iterations before the turn is forced to finish.
    pub max_steps: u32,
    pub temperature: f32,
    /// Optional model override; falls back to config when `None`.
    pub model: Option<String>,
}

/// Lightweight workflow hint a harness can auto-select from user intent.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SkillRoute {
    pub id: String,
    pub triggers: Vec<String>,
    pub instructions: String,
    pub template: Vec<String>,
}

impl SkillRoute {
    pub fn is_valid(&self) -> bool {
        !self.id.trim().is_empty() && !self.instructions.trim().is_empty()
    }
}

/// Where a skill bundle should be resolved from.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillBundleSource {
    Builtin,
    /// Current workspace, next to project-local manifests.
    #[default]
    Workspace,
    /// User-level local config, e.g. `~/.config/oxide`.
    UserConfig,
}

/// Local-first collection of workflow routes.
///
/// This is intentionally pure data so a future manifest loader can deserialize
/// it from TOML or YAML without teaching the engine about filesystem formats.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SkillBundle {
    pub id: String,
    pub name: String,
    pub description: String,
    pub source: SkillBundleSource,
    pub routes: Vec<SkillRoute>,
}

impl Default for SkillBundle {
    fn default() -> Self {
        Self {
            id: "workspace".to_string(),
            name: "Workspace Skill Bundle".to_string(),
            description: "Local workflow routes loaded from the active workspace.".to_string(),
            source: SkillBundleSource::Workspace,
            routes: Vec::new(),
        }
    }
}

impl SkillBundle {
    pub fn from_routes(
        id: impl Into<String>,
        name: impl Into<String>,
        routes: Vec<SkillRoute>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            routes,
            ..Self::default()
        }
    }

    pub fn builtin(
        id: impl Into<String>,
        name: impl Into<String>,
        routes: Vec<SkillRoute>,
    ) -> Self {
        Self {
            source: SkillBundleSource::Builtin,
            ..Self::from_routes(id, name, routes)
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.routes.iter().any(SkillRoute::is_valid)
    }

    pub fn valid_routes(&self) -> Vec<SkillRoute> {
        self.routes
            .iter()
            .filter(|route| route.is_valid())
            .cloned()
            .collect()
    }
}

impl Default for LoopPolicy {
    fn default() -> Self {
        Self {
            max_steps: 24,
            temperature: 0.2,
            model: None,
        }
    }
}

/// A behavior package the engine can run against.
///
/// Implemented by builtins and by [`ManifestHarness`] (loaded from TOML), so
/// native and external harnesses are indistinguishable to the engine.
pub trait Harness: Send + Sync {
    /// Stable identifier used in config and `SetHarness`.
    fn id(&self) -> &str;
    /// Human-friendly name.
    fn display_name(&self) -> &str;
    /// System prompt prepended to every turn (stable-prefix for prompt caching).
    fn system_prompt(&self) -> String;
    /// Tools this harness exposes to the model.
    fn tools(&self) -> Vec<ToolSpec>;
    /// Per-turn loop tunables.
    fn loop_policy(&self) -> LoopPolicy {
        LoopPolicy::default()
    }
    /// Harness-owned workflow routes auto-selected from the user request.
    fn skill_routes(&self) -> Vec<SkillRoute> {
        Vec::new()
    }
    /// Harness-owned skill bundle. Existing engines can keep projecting this
    /// into `skill_routes`; manifest loaders can preserve the richer metadata.
    fn skill_bundle(&self) -> SkillBundle {
        SkillBundle::from_routes(
            format!("{}-skills", self.id()),
            format!("{} Skills", self.display_name()),
            self.skill_routes(),
        )
    }
}

/// A harness defined entirely by data (TOML manifest) — the extensibility path.
///
/// Drop a `*.toml` into the harness dir and the registry picks it up. This is
/// how features get updated/added without recompiling Oxide.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ManifestHarness {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// Inline prompt, or use `system_prompt_file`.
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub system_prompt_file: Option<String>,
    #[serde(default)]
    pub tools: Vec<ManifestTool>,
    /// Preferred future shape for local-first skill manifests.
    #[serde(default)]
    pub skill_bundle: Option<SkillBundle>,
    /// Legacy inline routes kept for current harness TOML compatibility.
    #[serde(default)]
    pub skill_routes: Vec<ManifestSkillRoute>,
    #[serde(default)]
    pub max_steps: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub model: Option<String>,
    /// Resolved at load time from `system_prompt_file`.
    #[serde(skip)]
    resolved_prompt: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ManifestTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub mutating: bool,
    #[serde(default)]
    pub parameters: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ManifestSkillRoute {
    pub id: String,
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub template: Vec<String>,
}

impl ManifestHarness {
    /// Load and validate a single manifest file.
    pub fn from_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading harness manifest {}", path.display()))?;
        let mut m: ManifestHarness = toml::from_str(&text)
            .with_context(|| format!("parsing harness manifest {}", path.display()))?;
        if m.id.trim().is_empty() {
            anyhow::bail!("harness manifest {} has empty id", path.display());
        }
        if m.name.is_empty() {
            m.name = m.id.clone();
        }
        if let Some(rel) = &m.system_prompt_file {
            let base = path.parent().unwrap_or_else(|| Path::new("."));
            let p = base.join(rel);
            m.resolved_prompt = Some(
                std::fs::read_to_string(&p)
                    .with_context(|| format!("reading system_prompt_file {}", p.display()))?,
            );
        }
        Ok(m)
    }

    fn inline_skill_routes(&self) -> Vec<SkillRoute> {
        self.skill_routes
            .iter()
            .map(|route| SkillRoute {
                id: route.id.clone(),
                triggers: route.triggers.clone(),
                instructions: route.instructions.clone(),
                template: route.template.clone(),
            })
            .filter(SkillRoute::is_valid)
            .collect()
    }

    fn default_skill_bundle_id(&self) -> String {
        format!("{}-skills", self.id)
    }

    fn default_skill_bundle_name(&self) -> String {
        let name = if self.name.trim().is_empty() {
            self.id.as_str()
        } else {
            self.name.as_str()
        };
        format!("{name} Skills")
    }

    fn resolved_skill_bundle(&self) -> SkillBundle {
        let mut bundle = self.skill_bundle.clone().unwrap_or_else(|| {
            SkillBundle::from_routes(
                self.default_skill_bundle_id(),
                self.default_skill_bundle_name(),
                Vec::new(),
            )
        });

        if bundle.id.trim().is_empty() {
            bundle.id = self.default_skill_bundle_id();
        }
        if bundle.name.trim().is_empty() {
            bundle.name = self.default_skill_bundle_name();
        }

        let mut routes = bundle.valid_routes();
        routes.extend(self.inline_skill_routes());
        bundle.routes = routes;
        bundle
    }
}

impl Harness for ManifestHarness {
    fn id(&self) -> &str {
        &self.id
    }
    fn display_name(&self) -> &str {
        &self.name
    }
    fn system_prompt(&self) -> String {
        self.resolved_prompt
            .clone()
            .unwrap_or_else(|| self.system_prompt.clone())
    }
    fn tools(&self) -> Vec<ToolSpec> {
        self.tools
            .iter()
            .map(|t| {
                let mut spec = ToolSpec::new(&t.name, &t.description).mutating(t.mutating);
                if let Some(p) = &t.parameters {
                    spec = spec.params(p.clone());
                }
                spec
            })
            .collect()
    }
    fn loop_policy(&self) -> LoopPolicy {
        let d = LoopPolicy::default();
        LoopPolicy {
            max_steps: self.max_steps.unwrap_or(d.max_steps),
            temperature: self.temperature.unwrap_or(d.temperature),
            model: self.model.clone(),
        }
    }
    fn skill_routes(&self) -> Vec<SkillRoute> {
        self.resolved_skill_bundle().routes
    }
    fn skill_bundle(&self) -> SkillBundle {
        self.resolved_skill_bundle()
    }
}

/// Holds every available harness and resolves the active one.
pub struct Registry {
    harnesses: BTreeMap<String, Box<dyn Harness>>,
}

impl Registry {
    /// Registry seeded with builtin harnesses (`default`, `hermes`).
    pub fn with_builtins() -> Self {
        let mut reg = Registry {
            harnesses: BTreeMap::new(),
        };
        reg.insert(Box::new(builtin::DefaultHarness));
        reg.insert(Box::new(builtin::HermesHarness));
        reg.insert(Box::new(builtin::DesignHarness));
        reg
    }

    pub fn insert(&mut self, h: Box<dyn Harness>) {
        self.harnesses.insert(h.id().to_string(), h);
    }

    pub fn get(&self, id: &str) -> Option<&dyn Harness> {
        self.harnesses.get(id).map(|b| b.as_ref())
    }

    pub fn ids(&self) -> Vec<String> {
        self.harnesses.keys().cloned().collect()
    }

    /// Scan a directory for `*.toml` manifests and register each. Bad manifests
    /// are logged and skipped so one broken file can't take down startup.
    pub fn load_dir(&mut self, dir: &Path) -> Result<usize> {
        if !dir.exists() {
            return Ok(0);
        }
        let mut n = 0;
        for entry in std::fs::read_dir(dir)
            .with_context(|| format!("scanning harness dir {}", dir.display()))?
        {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            match ManifestHarness::from_file(&path) {
                Ok(m) => {
                    tracing::info!(id = %m.id, "loaded harness manifest");
                    self.insert(Box::new(m));
                    n += 1;
                }
                Err(e) => tracing::warn!(error = %e, "skipping bad harness manifest"),
            }
        }
        Ok(n)
    }
}

/// Manifest directories that should be scanned for external harnesses.
///
/// An explicit `harness_dir` wins. Relative paths are resolved against the
/// active workspace so desktop app launches are not sensitive to process cwd.
/// Without an explicit directory, Oxide scans the conventional
/// `<workspace>/harnesses` folder.
pub fn manifest_dirs(explicit: Option<&Path>, workspace: Option<&Path>) -> Vec<PathBuf> {
    let dir = match explicit {
        Some(dir) if dir.is_absolute() => dir.to_path_buf(),
        Some(dir) => workspace.map_or_else(|| dir.to_path_buf(), |ws| ws.join(dir)),
        None => workspace.map_or_else(|| PathBuf::from("harnesses"), |ws| ws.join("harnesses")),
    };
    vec![dir]
}

mod builtin {
    use super::{Harness, LoopPolicy, SkillBundle, SkillRoute};
    use oxide_protocol::ToolSpec;

    fn core_tools() -> Vec<ToolSpec> {
        vec![
            ToolSpec::new("read_file", "Read a whole file from the workspace (large files are truncated — use `search`/`codebase_search` to locate the region instead of slicing). Read with a clear purpose that informs your next step; do NOT re-read a file you already read this turn — its content is in context. Call in parallel for multiple files.").params(
                serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"]
                }),
            ),
            ToolSpec::new("write_file", "Create a NEW file or fully overwrite one. ALWAYS prefer `edit` for changing part of an existing file — use this only for brand-new files or a full rewrite.")
                .mutating(true)
                .params(serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" }, "content": { "type": "string" } },
                    "required": ["path", "content"]
                })),
            ToolSpec::new("edit", "Make a surgical change to an existing file: replace `old_string` with `new_string`. Read the file once first to confirm the exact text, then edit — don't re-read to feel sure. `old_string` must match exactly (incl. whitespace) and be unique unless `replace_all` is set. Make the smallest change that solves the task; don't rewrite the whole file.")
                .mutating(true)
                .params(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "old_string": { "type": "string", "description": "Exact text to find (with surrounding context to be unique)." },
                        "new_string": { "type": "string", "description": "Replacement text." },
                        "replace_all": { "type": "boolean", "description": "Replace every occurrence (default false)." }
                    },
                    "required": ["path", "old_string", "new_string"]
                })),
            ToolSpec::new("shell", "Run a shell command inside the sandbox. Use `timeout_seconds` for known long checks; for dev servers/watchers, start them detached and poll instead of blocking.")
                .mutating(true)
                .params(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" },
                        "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": 600, "description": "Optional timeout; default 120 seconds, max 600." }
                    },
                    "required": ["command"]
                })),
            ToolSpec::new("search", "Search the workspace for an exact string/pattern across files (skips vendor/build dirs, caps at 100 hits). Use a tight query with a clear next action — not a broad scan. For 'where is X implemented' by concept, use `codebase_search` instead.").params(
                serde_json::json!({
                    "type": "object",
                    "properties": { "query": { "type": "string" } },
                    "required": ["query"]
                }),
            ),
            // NOTE: the legacy `browser_open`/`browser_snapshot` stubs were removed —
            // the engine's real automation tools (`browser_navigate`, `browser_read`,
            // `browser_screenshot`, …) are added in `all_tools()`.
        ]
    }

    fn default_skill_routes() -> Vec<SkillRoute> {
        vec![
            SkillRoute {
                id: "frontend".to_string(),
                triggers: vec!["frontend", "ui", "ux", "css", "animation", "animasi", "responsive", "component"]
                    .into_iter()
                    .map(String::from)
                    .collect(),
                instructions: "Use the frontend workflow: inspect existing UI conventions, make the real interface work, verify rendered behavior with browser tools when practical, and avoid cosmetic-only changes.".to_string(),
                template: vec![
                    "Inspect existing UI conventions and affected components.",
                    "Implement the real interaction/state changes.",
                    "Verify the rendered UI or relevant build/test path.",
                    "Report changed files and residual risk.",
                ].into_iter().map(String::from).collect(),
            },
            SkillRoute {
                id: "review".to_string(),
                triggers: vec!["review", "audit", "risiko", "bug", "regression"]
                    .into_iter()
                    .map(String::from)
                    .collect(),
                instructions: "Use review workflow: prioritize concrete bugs, regressions, missing tests, and risky behavior. Lead with findings and file references before summaries.".to_string(),
                template: vec![
                    "Read the diff and relevant call-sites.",
                    "Check correctness, regressions, security, and test gaps.",
                    "Return findings first with file references.",
                ].into_iter().map(String::from).collect(),
            },
            SkillRoute {
                id: "release".to_string(),
                triggers: vec!["release", "tag", "dmg", "github release", "publish", "push"]
                    .into_iter()
                    .map(String::from)
                    .collect(),
                instructions: "Use release workflow: keep staging scoped, verify build artifacts, check GitHub Actions/release status, and do not assume a release succeeded without evidence.".to_string(),
                template: vec![
                    "Confirm staged scope and current branch.",
                    "Run the relevant validation/build.",
                    "Commit, tag, and push only the intended changes.",
                    "Watch GitHub Actions and verify release assets.",
                ].into_iter().map(String::from).collect(),
            },
            SkillRoute {
                id: "github-action".to_string(),
                triggers: vec!["github action", "workflow", "ci", "failing check", "actions"]
                    .into_iter()
                    .map(String::from)
                    .collect(),
                instructions: "Use CI workflow: inspect workflow definitions and current logs when available, reproduce the failing command locally when possible, then patch the smallest root cause.".to_string(),
                template: vec![
                    "Inspect the workflow and latest failing logs.",
                    "Reproduce the failing command locally when possible.",
                    "Patch the smallest root cause.",
                    "Re-run the targeted validation.",
                ].into_iter().map(String::from).collect(),
            },
            SkillRoute {
                id: "browser-test".to_string(),
                triggers: vec!["browser", "screenshot", "playwright", "localhost", "web test"]
                    .into_iter()
                    .map(String::from)
                    .collect(),
                instructions: "Use browser-test workflow: open the target, verify visual state and interactions, collect screenshots or readable page state, and report what was actually observed.".to_string(),
                template: vec![
                    "Open the target URL or local app.",
                    "Exercise the requested interaction.",
                    "Capture readable state or screenshot evidence.",
                    "Report observed behavior and fixes.",
                ].into_iter().map(String::from).collect(),
            },
        ]
    }

    fn default_skill_bundle() -> SkillBundle {
        let mut bundle = SkillBundle::builtin(
            "default-workflows",
            "Default Workflows",
            default_skill_routes(),
        );
        bundle.description =
            "Builtin workflow routes shared by the default coding harness.".to_string();
        bundle
    }

    fn hermes_skill_bundle() -> SkillBundle {
        let mut bundle = SkillBundle::builtin(
            "hermes-workflows",
            "Hermes Workflows",
            default_skill_routes(),
        );
        bundle.description =
            "Builtin planning-forward workflow routes for the Hermes harness.".to_string();
        bundle
    }

    fn design_skill_routes() -> Vec<SkillRoute> {
        let mut routes = default_skill_routes();
        routes.push(SkillRoute {
            id: "design-workbench".to_string(),
            triggers: vec![
                "design",
                "desain",
                "open design",
                "design system",
                "design mode",
                "ui polish",
                "visual qa",
                "prototype",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            instructions: "Use the Design Workbench workflow: inspect the existing UI and DESIGN.md, extract tokens, capture/select the target, propose structured edits, run visual review, then apply code changes through the normal edit path.".to_string(),
            template: vec![
                "Read the local design system or infer tokens from existing UI.",
                "Capture or inspect the target preview and selected element.",
                "Propose minimal visual edits with token-aware reasoning.",
                "Run design review for accessibility, motion, and token risks.",
                "Apply source-code changes and verify the rendered result.",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
        });
        routes
    }

    fn design_skill_bundle() -> SkillBundle {
        let mut bundle = SkillBundle::builtin(
            "design-workflows",
            "Design Workflows",
            design_skill_routes(),
        );
        bundle.description =
            "Builtin Open Design-style local-first design workflow routes.".to_string();
        bundle
    }

    /// General-purpose coding agent.
    pub struct DefaultHarness;
    impl Harness for DefaultHarness {
        fn id(&self) -> &str {
            "default"
        }
        fn display_name(&self) -> &str {
            "Default"
        }
        fn system_prompt(&self) -> String {
            "You are Oxide, a fast Rust-native coding agent. Solve the user's coding task fully and correctly.\n\n\
             Workflow — explore, plan, implement, verify:\n\
             - Read the relevant files BEFORE editing them. Use `search` to locate code; pull context just-in-time — don't read the whole repo.\n\
             - For multi-step work, state a short plan (the files/functions you'll change) first. Skip planning for trivial tasks. For genuinely multi-phase work (>2 edits or multiple subsystems) track progress with the `todo_write` checklist (exactly one task in_progress); never use it for simple tasks.\n\
             - Add a brief note only when it clarifies a non-obvious step — do NOT narrate 'I'll check X' before every tool call. Let the actions speak.\n\
             - Never re-read a file you already read this turn — its content is in the context above; act on it. Avoid tiny repeated slices; read one larger window. Read independent files in parallel.\n\
             - Prefer the smallest set of high-signal tool calls that complete and verify the task. Batch related reads/searches/edits; don't make exploratory calls without a clear next action. Read or search only with a purpose that informs your next step — not to browse.\n\
             - Before editing, confirm the exact symbols/signatures you'll touch (one targeted look), then edit — don't re-explore to feel sure.\n\n\
             Editing discipline:\n\
             - DEFAULT TO ACTING. Reading and searching are means to an edit, not the goal — apply changes with the `edit`/`write_file` tools. Apply edits and run reversible commands without asking permission.\n\
             - Do not announce an action and then stop. If you say 'I'll update X', actually call the tool in the SAME turn before yielding. Never end your turn having only described, planned, or read when the task asks for a change — make the change, then verify.\n\
             - Make the smallest diff that solves the task. Don't touch unrelated code; don't refactor or 'improve' beyond what was asked.\n\
             - Code must be immediately runnable: add every needed import/dependency; no placeholders, stubs, or TODOs.\n\
             - Match existing style. No license headers. No comments unless the WHY is non-obvious.\n\n\
             Finish the whole task, not one edit:\n\
             - Do the task end-to-end — don't hand back half-baked work or stop after a single edit. A change usually touches more than one spot: check for other files/call-sites that need the same edit for it to actually work.\n\
             - Complete EVERY step you stated. If your plan said 'then run typecheck/lint/tests', you MUST run them and fix what breaks before ending — don't announce a verification step and then skip it.\n\n\
             Verify before claiming done:\n\
             - Run the project's tests/build/linter with `shell` and READ the output; iterate until it passes. Show the command and result as evidence — never claim success you didn't verify.\n\
             - For web/UI changes, use the browser tools to load and check the result.\n\
             - Don't loop more than ~3 times on the same error; change approach instead of guessing. If you catch yourself calling the same tool repeatedly without progress, stop spinning — change tactic or ask the user.\n\n\
             Scope & safety: fix the root cause, not the symptom. Take reversible actions freely; for hard-to-reverse ones (git commit/push, destructive shell) ask first — never commit unless asked.\n\n\
             When a real decision is needed (ambiguous requirements, a new dependency, a cross-cutting refactor), search the code/docs first; if a branching choice remains, call the `ask_user` tool with a clear question and up to 4 concrete options, lead with your recommendation, then wait. Don't guess silently or bury the question in prose.\n\n\
             More working rules (from strong agents):\n\
             - No surprise edits: if a change touches more than ~3 files or multiple subsystems, show a short plan first. No new dependencies without asking.\n\
             - If the user asks how to approach or plan something, answer that first — don't jump straight to edits. If they only want to plan or research, make no persistent changes.\n\
             - Verify in order: typecheck → lint → tests → build. Report results as counts (pass/fail). Only the files you changed are your concern — NEVER fix pre-existing errors in files you didn't touch (don't go chasing an unrelated typecheck failure); note them and move on.\n\
             - Never suppress compiler, type, or linter errors (no `as any`, no blanket ignore directives) unless the user explicitly asks.\n\
             - Don't assume a test framework or that a library is available — check the codebase, AGENTS.md, or README first.\n\
             - Skip flattery — never open with 'great question' / 'excellent'; respond directly.\n\
             - Simple-first, reuse-first: prefer reusing existing code and the simplest solution. Avoid over-engineering — a local guard beats a cross-layer refactor; a single-purpose helper beats a new abstraction.\n\
             - Stop gathering context as soon as you can act: once you can name the exact files/symbols to change or reproduce the failure, start. Trace only what you'll modify; avoid transitive expansion.\n\
             - Output: no inner monologue or filler, no emojis/decorative symbols, don't repeat tool output already shown, use workspace-relative paths.\n\n\
             Final status: keep it to a few lines — lead with what changed and why, include the verification result (pass/fail counts), and offer a sensible next step. Keep going until the task is fully resolved and verified."
                .to_string()
        }
        fn tools(&self) -> Vec<ToolSpec> {
            core_tools()
        }
        fn skill_routes(&self) -> Vec<SkillRoute> {
            default_skill_bundle().routes
        }
        fn skill_bundle(&self) -> SkillBundle {
            default_skill_bundle()
        }
    }

    /// Planning-forward harness — example of a swappable behavior pack.
    pub struct HermesHarness;
    impl Harness for HermesHarness {
        fn id(&self) -> &str {
            "hermes"
        }
        fn display_name(&self) -> &str {
            "Hermes"
        }
        fn system_prompt(&self) -> String {
            "You are Oxide running the Hermes harness. Think in explicit plans: \
             outline a numbered plan first, then execute step by step, narrating \
             each tool call and verifying results before moving on."
                .to_string()
        }
        fn tools(&self) -> Vec<ToolSpec> {
            core_tools()
        }
        fn skill_routes(&self) -> Vec<SkillRoute> {
            hermes_skill_bundle().routes
        }
        fn skill_bundle(&self) -> SkillBundle {
            hermes_skill_bundle()
        }
        fn loop_policy(&self) -> LoopPolicy {
            LoopPolicy {
                max_steps: 48,
                temperature: 0.1,
                model: None,
            }
        }
    }

    /// Local-first Open Design-style harness.
    pub struct DesignHarness;
    impl Harness for DesignHarness {
        fn id(&self) -> &str {
            "design"
        }
        fn display_name(&self) -> &str {
            "Design"
        }
        fn system_prompt(&self) -> String {
            "You are Oxide running the Design Workbench harness: a Rust-native, local-first Open Design-style workflow.\n\n\
             Operating model:\n\
             - Treat `DESIGN.md`, existing CSS tokens, and rendered UI behavior as the source of truth.\n\
             - Use `design_read_system` or `design_extract_tokens` before making visual changes when a design system exists.\n\
             - Use `design_snapshot` or browser tools to inspect the actual preview when UI/UX is involved.\n\
             - Use `design_review` and `design_propose_patch` for selected element edits before applying code changes.\n\
             - Prefer existing classes, CSS variables, and component conventions over raw inline values.\n\
             - Keep motion purposeful: hover/feedback under 200ms when frequent, non-navigation transitions under 500ms, and transform motion must respect reduced-motion.\n\
             - Make the smallest source-code patch, then verify with the relevant check or rendered preview."
                .to_string()
        }
        fn tools(&self) -> Vec<ToolSpec> {
            core_tools()
        }
        fn skill_routes(&self) -> Vec<SkillRoute> {
            design_skill_bundle().routes
        }
        fn skill_bundle(&self) -> SkillBundle {
            design_skill_bundle()
        }
        fn loop_policy(&self) -> LoopPolicy {
            LoopPolicy {
                max_steps: 64,
                temperature: 0.15,
                model: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        manifest_dirs, Harness, ManifestHarness, Registry, SkillBundle, SkillBundleSource,
    };
    use std::path::{Path, PathBuf};

    #[test]
    fn builtin_harnesses_expose_core_tools() {
        let registry = Registry::with_builtins();
        let default = registry.get("default").unwrap();
        let tools = default.tools();

        assert!(tools.iter().any(|tool| tool.name == "read_file"));
        assert!(tools.iter().any(|tool| tool.name == "search"));
    }

    #[test]
    fn manifest_dirs_resolve_relative_paths_from_workspace() {
        let dirs = manifest_dirs(
            Some(Path::new("custom-harnesses")),
            Some(Path::new("/tmp/ws")),
        );

        assert_eq!(dirs, vec![PathBuf::from("/tmp/ws/custom-harnesses")]);
    }

    #[test]
    fn manifest_dirs_default_to_workspace_harnesses() {
        let dirs = manifest_dirs(None, Some(Path::new("/tmp/ws")));

        assert_eq!(dirs, vec![PathBuf::from("/tmp/ws/harnesses")]);
    }

    #[test]
    fn skill_bundle_default_is_workspace_local_and_empty() {
        let bundle = SkillBundle::default();

        assert_eq!(bundle.id, "workspace");
        assert_eq!(bundle.source, SkillBundleSource::Workspace);
        assert!(bundle.is_empty());
        assert!(bundle.valid_routes().is_empty());
    }

    #[test]
    fn hermes_exposes_builtin_skill_bundle_metadata() {
        let registry = Registry::with_builtins();
        let hermes = registry.get("hermes").unwrap();
        let bundle = hermes.skill_bundle();

        assert_eq!(bundle.id, "hermes-workflows");
        assert_eq!(bundle.source, SkillBundleSource::Builtin);
        assert!(bundle
            .valid_routes()
            .iter()
            .any(|route| route.id == "release"));
    }

    #[test]
    fn design_harness_exposes_design_workflow_route() {
        let registry = Registry::with_builtins();
        let design = registry.get("design").unwrap();
        let bundle = design.skill_bundle();

        assert_eq!(bundle.id, "design-workflows");
        assert_eq!(bundle.source, SkillBundleSource::Builtin);
        assert!(bundle
            .valid_routes()
            .iter()
            .any(|route| route.id == "design-workbench"));
        assert!(design.system_prompt().contains("Design Workbench"));
    }

    #[test]
    fn manifest_harness_accepts_nested_skill_bundle_routes() {
        let manifest: ManifestHarness = toml::from_str(
            r#"
id = "local"
name = "Local Harness"
system_prompt = "Use local workflows."

[skill_bundle]
id = "workspace-workflows"
name = "Workspace Workflows"
description = "Project-local routes."
source = "workspace"

[[skill_bundle.routes]]
id = "qa"
triggers = ["qa", "test"]
instructions = "Run the relevant local validation."
template = ["Find the test command.", "Run it and read the output."]

[[skill_routes]]
id = "legacy-review"
triggers = ["review"]
instructions = "Review the diff."
"#,
        )
        .unwrap();

        let bundle = manifest.skill_bundle();
        let route_ids: Vec<String> = bundle
            .valid_routes()
            .into_iter()
            .map(|route| route.id)
            .collect();

        assert_eq!(bundle.id, "workspace-workflows");
        assert_eq!(bundle.name, "Workspace Workflows");
        assert_eq!(bundle.source, SkillBundleSource::Workspace);
        assert_eq!(
            route_ids,
            vec!["qa".to_string(), "legacy-review".to_string()]
        );
    }
}
