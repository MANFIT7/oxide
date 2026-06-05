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
use std::path::Path;

/// Tunables that shape a single turn's loop.
#[derive(Debug, Clone)]
pub struct LoopPolicy {
    /// Max tool-call iterations before the turn is forced to finish.
    pub max_steps: u32,
    pub temperature: f32,
    /// Optional model override; falls back to config when `None`.
    pub model: Option<String>,
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

mod builtin {
    use super::{Harness, LoopPolicy};
    use oxide_protocol::ToolSpec;

    fn core_tools() -> Vec<ToolSpec> {
        vec![
            ToolSpec::new("read_file", "Read a file from the workspace.").params(
                serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"]
                }),
            ),
            ToolSpec::new("write_file", "Create or overwrite a file.")
                .mutating(true)
                .params(serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" }, "content": { "type": "string" } },
                    "required": ["path", "content"]
                })),
            ToolSpec::new("shell", "Run a shell command inside the sandbox.")
                .mutating(true)
                .params(serde_json::json!({
                    "type": "object",
                    "properties": { "command": { "type": "string" } },
                    "required": ["command"]
                })),
            ToolSpec::new("search", "Search the workspace for a pattern.").params(
                serde_json::json!({
                    "type": "object",
                    "properties": { "query": { "type": "string" } },
                    "required": ["query"]
                }),
            ),
            ToolSpec::new(
                "browser_open",
                "Request the frontend to open or focus a browser target URL.",
            )
            .mutating(true)
            .params(serde_json::json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string" },
                    "note": { "type": "string" }
                },
                "required": ["url"]
            })),
            ToolSpec::new(
                "browser_snapshot",
                "Request the frontend to capture browser visual evidence for a URL.",
            )
            .mutating(true)
            .params(serde_json::json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string" },
                    "note": { "type": "string" }
                },
                "required": ["url"]
            })),
        ]
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
             - For multi-step work, state a short plan (the files/functions you'll change) first. Skip planning for trivial tasks.\n\
             - Send a one-line note before tool calls describing the next step.\n\n\
             Editing discipline:\n\
             - Make the smallest diff that solves the task. Don't touch unrelated code; don't refactor or 'improve' beyond what was asked.\n\
             - Code must be immediately runnable: add every needed import/dependency; no placeholders, stubs, or TODOs.\n\
             - Match existing style. No license headers. No comments unless the WHY is non-obvious.\n\n\
             Verify before claiming done:\n\
             - Run the project's tests/build/linter with `shell` and READ the output; iterate until it passes. Show the command and result as evidence — never claim success you didn't verify.\n\
             - For web/UI changes, use the browser tools to load and check the result.\n\
             - Don't loop more than ~3 times on the same error; change approach instead of guessing.\n\n\
             Scope & safety: fix the root cause, not the symptom. Take reversible actions freely; for hard-to-reverse ones (git commit/push, destructive shell) ask first — never commit unless asked.\n\n\
             When a real decision is needed (ambiguous requirements, a new dependency, a cross-cutting refactor), search the code/docs first; if a branching choice remains, call the `ask_user` tool with a clear question and up to 4 concrete options, lead with your recommendation, then wait. Don't guess silently or bury the question in prose.\n\n\
             More working rules (from strong agents):\n\
             - No surprise edits: if a change touches more than ~3 files or multiple subsystems, show a short plan first. No new dependencies without asking.\n\
             - If the user asks how to approach or plan something, answer that first — don't jump straight to edits. If they only want to plan or research, make no persistent changes.\n\
             - Verify in order: typecheck → lint → tests → build. Report results as counts (pass/fail). Scope around unrelated pre-existing failures and say so.\n\
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
        fn loop_policy(&self) -> LoopPolicy {
            LoopPolicy {
                max_steps: 48,
                temperature: 0.1,
                model: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Registry;

    #[test]
    fn builtin_harnesses_expose_browser_contract_tools() {
        let registry = Registry::with_builtins();
        let default = registry.get("default").unwrap();
        let tools = default.tools();

        assert!(tools.iter().any(|tool| tool.name == "browser_open"));
        assert!(tools.iter().any(|tool| tool.name == "browser_snapshot"));
    }
}
