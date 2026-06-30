//! Agentic-CLI driver providers.
//!
//! Instead of calling a model API with our own key, these drive the user's
//! already-authenticated local CLIs — `codex` and `claude` (Claude Code) — in
//! headless JSONL-streaming mode with permissions bypassed. No API key needed:
//! the CLI uses its own login. The CLI does its own tools, sandboxing and
//! context compaction; Oxide just streams its output into the same event model.
//!
//! - codex:  `codex exec --json --dangerously-bypass-approvals-and-sandbox`
//! - claude: `claude -p --output-format stream-json --verbose --dangerously-skip-permissions`

use crate::{Provider, Role, StreamItem, TurnRequest};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

/// Kills a CLI driver's whole process group on drop (unless disarmed) so that
/// when the engine aborts the stream task on interrupt, anything the CLI
/// spawned — most importantly a long `cargo build`/test — dies with it instead
/// of being orphaned and continuing to churn in the background.
#[cfg(unix)]
struct ProcessGroupGuard {
    pgid: i32,
    armed: bool,
}

#[cfg(unix)]
impl ProcessGroupGuard {
    fn kill_now(&mut self) {
        if self.armed && self.pgid > 1 {
            // SAFETY: killpg with a valid pgid is sound; a dead group yields
            // ESRCH which we ignore.
            unsafe {
                libc::killpg(self.pgid, libc::SIGKILL);
            }
            self.armed = false;
        }
    }
}

#[cfg(unix)]
impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        self.kill_now();
    }
}

/// CLI session ids per (binary, workspace) so consecutive turns RESUME the same
/// CLI conversation instead of starting amnesiac one-shots.
static SESSIONS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
static CLAUDE_INTERACTIVE_SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);

const CLAUDE_INTERACTIVE_POLL: Duration = Duration::from_millis(250);
const CLAUDE_INTERACTIVE_SETTLE: Duration = Duration::from_millis(1600);
const DEFAULT_CLI_TURN_TIMEOUT: Duration = Duration::from_secs(45 * 60);
const DEFAULT_CLAUDE_INTERACTIVE_TURN_TIMEOUT: Duration = Duration::from_secs(45 * 60);
const CLAUDE_INTERACTIVE_READY_TIMEOUT: Duration = Duration::from_secs(8);
const CLAUDE_INTERACTIVE_PROMPT_ACCEPT_TIMEOUT: Duration = Duration::from_secs(12);

fn session_key(bin: &str, conv: &str, cwd: &str) -> String {
    // Prefer the per-conversation id; fall back to cwd if absent.
    if conv.is_empty() {
        format!("{bin}|{cwd}")
    } else {
        format!("{bin}|{conv}")
    }
}

fn session_get(key: &str) -> Option<String> {
    SESSIONS
        .get_or_init(Default::default)
        .lock()
        .ok()?
        .get(key)
        .cloned()
}

fn session_set(key: &str, id: &str) {
    if let Ok(mut m) = SESSIONS.get_or_init(Default::default).lock() {
        m.insert(key.to_string(), id.to_string());
    }
}

/// Resolve a CLI binary. Honors `$env_override`, then probes common install
/// dirs — needed when launched from Finder (minimal PATH lacks the user's
/// shell paths where `codex`/`claude` live).
fn resolve_bin(name: &str, env_override: &str) -> String {
    if let Ok(p) = std::env::var(env_override) {
        if !p.trim().is_empty() {
            return p;
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [
        format!("{home}/.superconductor/bin/{name}"),
        format!("{home}/.local/bin/{name}"),
        format!("{home}/.bun/bin/{name}"),
        format!("{home}/.npm-global/bin/{name}"),
        format!("{home}/.codex/bin/{name}"),
        format!("/opt/homebrew/bin/{name}"),
        format!("/usr/local/bin/{name}"),
    ];
    for c in &candidates {
        if std::path::Path::new(c).exists() {
            return c.clone();
        }
    }
    name.to_string() // fall back to PATH lookup
}

fn duration_from_env(keys: &[&str], default: Duration) -> anyhow::Result<Duration> {
    for key in keys {
        let Ok(raw) = std::env::var(key) else {
            continue;
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let secs = trimmed.parse::<u64>().map_err(|e| {
            anyhow::anyhow!("{key} must be a positive integer number of seconds: {e}")
        })?;
        if secs == 0 {
            anyhow::bail!("{key} must be greater than 0 seconds");
        }
        return Ok(Duration::from_secs(secs));
    }
    Ok(default)
}

fn codex_cli_timeout() -> anyhow::Result<Duration> {
    duration_from_env(
        &["OXIDE_CODEX_CLI_TIMEOUT_SEC", "OXIDE_CLI_TIMEOUT_SEC"],
        DEFAULT_CLI_TURN_TIMEOUT,
    )
}

fn claude_cli_timeout() -> anyhow::Result<Duration> {
    duration_from_env(
        &["OXIDE_CLAUDE_CLI_TIMEOUT_SEC", "OXIDE_CLI_TIMEOUT_SEC"],
        DEFAULT_CLI_TURN_TIMEOUT,
    )
}

fn claude_interactive_timeout() -> anyhow::Result<Duration> {
    duration_from_env(
        &[
            "OXIDE_CLAUDE_INTERACTIVE_TIMEOUT_SEC",
            "OXIDE_CLAUDE_CLI_TIMEOUT_SEC",
            "OXIDE_CLI_TIMEOUT_SEC",
        ],
        DEFAULT_CLAUDE_INTERACTIVE_TURN_TIMEOUT,
    )
}

fn format_timeout(timeout: Duration) -> String {
    let secs = timeout.as_secs();
    if secs == 0 {
        format!("{} ms", timeout.as_millis())
    } else if secs.is_multiple_of(3600) {
        format!("{} hours", secs / 3600)
    } else if secs.is_multiple_of(60) {
        format!("{} minutes", secs / 60)
    } else {
        format!("{secs} seconds")
    }
}

/// Pull the latest user message — these CLIs take a single prompt, not a list.
fn last_user_prompt(req: &TurnRequest) -> String {
    req.messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .map(|m| m.content.clone())
        .unwrap_or_default()
}

/// Split the latest user prompt into clean text + absolute image paths. Pasted
/// images ride as `\u{2}wsimg:<relpath>` markers (relative to the workspace);
/// the "(user attached … NOT visible)" note that API providers get is dropped
/// because the CLI is handed the real files.
fn extract_cli_images(req: &TurnRequest) -> (String, Vec<String>) {
    let raw = last_user_prompt(req);
    let mut parts = raw.split('\u{2}');
    let mut prompt = parts.next().unwrap_or("").to_string();
    let ws = std::path::Path::new(&req.cwd);
    let mut imgs = Vec::new();
    for seg in parts {
        if let Some(rel) = seg.strip_prefix("wsimg:") {
            let p = if rel.starts_with('/') {
                std::path::PathBuf::from(rel)
            } else {
                ws.join(rel)
            };
            if p.exists() {
                imgs.push(p.display().to_string());
            }
        }
    }
    if !imgs.is_empty() {
        // Strip exactly the "(user attached … needed)" note. It is emitted with
        // no trailing newline before the user's typed text, so bound the removal
        // by the note's own closing ')'. Bounding by the next '\n' (as before) ate
        // the whole prompt when it was single-line — leaving an empty prompt that
        // fell back to the English "Inspect the attached image(s)." instruction.
        if let Some(idx) = prompt.find("(user attached ") {
            let end = prompt[idx..]
                .find(')')
                .map(|e| idx + e + 1)
                .unwrap_or(prompt.len());
            prompt.replace_range(idx..end, "");
        }
    }
    (prompt.trim().to_string(), imgs)
}

fn clean_cli_command(command: &str) -> String {
    command
        .strip_prefix("/bin/zsh -lc ")
        .unwrap_or(command)
        .strip_prefix("/bin/sh -c ")
        .unwrap_or(command)
        .trim_matches('\'')
        .trim()
        .to_string()
}

fn cli_item_id(item: &Value, fallback: &str) -> String {
    let raw = item["id"]
        .as_str()
        .or_else(|| item["item_id"].as_str())
        .or_else(|| item["call_id"].as_str())
        .unwrap_or(fallback);
    if raw.trim().is_empty() {
        fallback.to_string()
    } else {
        raw.to_string()
    }
}

fn cli_exit_code(item: &Value) -> Option<i32> {
    item["exit_code"]
        .as_i64()
        .or_else(|| {
            item["exit_code"]
                .as_str()
                .and_then(|s| s.parse::<i64>().ok())
        })
        .and_then(|code| i32::try_from(code).ok())
}

fn cli_duration_ms(item: &Value) -> u64 {
    item["duration_ms"]
        .as_u64()
        .or_else(|| item["elapsed_ms"].as_u64())
        .unwrap_or(0)
}

/// Spawn `program args...` and stream its stdout lines to `on_line`, closing
/// stdin so the CLI doesn't block waiting for piped input.
async fn run_jsonl<F>(
    program: &str,
    args: &[String],
    cwd: &str,
    stdin_text: Option<String>,
    timeout: Duration,
    sink: &mpsc::Sender<StreamItem>,
    mut on_line: F,
) -> anyhow::Result<()>
where
    F: FnMut(&Value, &mpsc::Sender<StreamItem>) -> bool,
{
    let pipe_stdin = stdin_text.as_ref().map(|s| !s.is_empty()).unwrap_or(false);
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .stdin(if pipe_stdin {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .stderr(Stdio::piped());
    // Run in the workspace — without this the CLI inherits the app's cwd
    // (Finder launches give `/`) and edits the wrong tree.
    if !cwd.is_empty() {
        cmd.current_dir(cwd);
    }
    // Put the CLI in its own process group (it becomes the leader, so pgid ==
    // its pid). On interrupt we SIGKILL the whole group via the guard below, so
    // anything it spawned (e.g. a long `cargo build`) dies with it instead of
    // being orphaned. kill_on_drop alone only reaps the direct child.
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn '{program}': {e}"))?;
    // Armed for the lifetime of this future: if it's dropped before the normal
    // end (the engine aborts the stream task on interrupt), the group is killed.
    // Disarmed on normal completion so backgrounded grandchildren can survive.
    #[cfg(unix)]
    let mut group_guard = child.id().map(|pid| ProcessGroupGuard {
        pgid: pid as i32,
        armed: true,
    });
    let stdin_task = if let Some(input) = stdin_text.filter(|s| !s.is_empty()) {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to open stdin for '{program}'"))?;
        Some(tokio::spawn(async move {
            stdin.write_all(input.as_bytes()).await?;
            stdin.shutdown().await
        }))
    } else {
        None
    };

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("no stdout"))?;
    // Collect stderr in the background so failures aren't silent.
    let stderr = child.stderr.take();
    let mut err_task = tokio::spawn(async move {
        let mut tail = String::new();
        if let Some(e) = stderr {
            let mut lines = BufReader::new(e).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                tail.push_str(&l);
                tail.push('\n');
                if tail.len() > 4000 {
                    tail = tail[tail.len() - 2000..].to_string();
                }
            }
        }
        tail
    });
    let mut lines = BufReader::new(stdout).lines();
    let mut emitted = false;
    let mut timed_out = false;
    let deadline = tokio::time::Instant::now() + timeout;
    while let Some(line) = match tokio::time::timeout_at(deadline, lines.next_line()).await {
        Ok(line) => line?,
        Err(_) => {
            timed_out = true;
            None
        }
    } {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            emitted = true;
            // on_line returns false to stop early.
            if !on_line(&v, sink) {
                break;
            }
        }
    }
    let status = if timed_out {
        None
    } else {
        match tokio::time::timeout_at(deadline, child.wait()).await {
            Ok(status) => Some(status),
            Err(_) => {
                timed_out = true;
                None
            }
        }
    };
    if timed_out {
        #[cfg(unix)]
        if let Some(g) = group_guard.as_mut() {
            g.kill_now();
        }
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
        if let Some(task) = &stdin_task {
            task.abort();
        }
        let tail = match tokio::time::timeout(Duration::from_secs(1), &mut err_task).await {
            Ok(Ok(tail)) => tail,
            _ => {
                err_task.abort();
                String::new()
            }
        };
        let tail = tail.trim();
        let message = format!(
            "{program} timed out after {}{}{}",
            format_timeout(timeout),
            if tail.is_empty() { "" } else { " — " },
            tail.chars().take(600).collect::<String>()
        );
        let _ = sink
            .send(StreamItem::Notice(format!("error: {message}")))
            .await;
        let _ = sink.send(StreamItem::Done).await;
        anyhow::bail!(message);
    }
    let status = status.unwrap_or_else(|| {
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "CLI process timeout elapsed",
        ))
    });
    let stdin_error = match stdin_task {
        Some(mut task) => match tokio::time::timeout(Duration::from_secs(1), &mut task).await {
            Ok(joined) => joined.ok().and_then(Result::err),
            Err(_) => {
                task.abort();
                None
            }
        },
        None => None,
    };
    let failed = status.map(|st| !st.success()).unwrap_or(true);
    if failed {
        let tail = match tokio::time::timeout(Duration::from_secs(1), &mut err_task).await {
            Ok(Ok(tail)) => tail,
            _ => {
                err_task.abort();
                String::new()
            }
        };
        let tail = tail.trim();
        if !emitted || !tail.is_empty() {
            let _ = sink
                .send(StreamItem::Notice(format!(
                    "error: {program} exited with failure{}{}",
                    if tail.is_empty() { "" } else { " — " },
                    tail.chars().take(600).collect::<String>()
                )))
                .await;
        }
    } else {
        err_task.abort();
        if let Some(err) = stdin_error {
            let _ = sink
                .send(StreamItem::Notice(format!(
                    "warning: failed to finish stdin for {program}: {err}"
                )))
                .await;
        }
    }
    // Reached the end cleanly — the child exited on its own. Don't kill the
    // group (any backgrounded grandchild it left is intentional).
    #[cfg(unix)]
    if let Some(g) = group_guard.as_mut() {
        g.armed = false;
    }
    let _ = sink.send(StreamItem::Done).await;
    Ok(())
}

fn send(sink: &mpsc::Sender<StreamItem>, item: StreamItem) {
    // Best-effort: the channel is generously sized; drop on the rare overflow.
    let _ = sink.try_send(item);
}

/// Drives the local `codex` CLI.
pub struct CodexCliProvider {
    bin: String,
}

impl CodexCliProvider {
    pub fn new() -> Self {
        Self {
            bin: resolve_bin("codex", "OXIDE_CODEX_BIN"),
        }
    }
}

impl Default for CodexCliProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for CodexCliProvider {
    fn name(&self) -> &str {
        "codex"
    }

    async fn stream(&self, req: TurnRequest, sink: mpsc::Sender<StreamItem>) -> anyhow::Result<()> {
        let (mut prompt, images) = extract_cli_images(&req);
        if prompt.trim().is_empty() && !images.is_empty() {
            prompt = "Inspect the attached image(s).".to_string();
        }
        if prompt.trim().is_empty() {
            anyhow::bail!(
                "Codex CLI prompt is empty; refusing to start codex exec without instructions"
            );
        }
        let skey = session_key(&self.bin, &req.conversation_id, &req.cwd);
        // Prefer the persisted link (survives app restarts) over the in-memory map.
        let resume = req.cli_resume.clone().or_else(|| session_get(&skey));
        let mut args = vec!["exec".to_string()];
        if let Some(id) = &resume {
            // Continue the same codex thread — context carries across turns.
            args.push("resume".to_string());
            args.push(id.clone());
        }
        args.push("--json".to_string());
        args.push("--dangerously-bypass-approvals-and-sandbox".to_string());
        args.push("--skip-git-repo-check".to_string());
        if !req.cwd.is_empty() {
            args.push("-C".to_string());
            args.push(req.cwd.clone());
        }
        if !req.model.is_empty() {
            args.push("-m".to_string());
            args.push(req.model.clone());
        }
        if !req.reasoning_effort.is_empty() {
            args.push("-c".to_string());
            args.push(format!(
                "model_reasoning_effort=\"{}\"",
                codex_effort(&req.reasoning_effort)
            ));
        }
        // Pasted/attached images become native codex attachments (one -i per file).
        for img in &images {
            args.push("-i".to_string());
            args.push(img.clone());
        }
        // Superconductor's codex wrapper can require stdin for `exec`/`resume`.
        // Passing `-` makes that contract explicit and avoids a null-stdin turn.
        args.push("-".to_string());

        let skey_cb = skey.clone();
        let timeout = codex_cli_timeout()?;
        // codex flushes its agent_message text atomically at item.completed, and
        // not reliably BEFORE the tool/command events that preceded it — so the
        // final answer could render ABOVE the command that produced it. Buffer the
        // agent text and emit it AFTER run_jsonl (i.e. after every live command/
        // edit/search row), so the transcript always reads command → answer, never
        // answer → command. Activity rows stay live for feedback. (claude_interactive
        // solves the same ordering with per-block transcript positions; codex's JSONL
        // carries no position, so we use "text last" instead.)
        let text_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let text_buf_cb = text_buf.clone();
        let result = run_jsonl(
            &self.bin,
            &args,
            &req.cwd,
            Some(prompt),
            timeout,
            &sink,
            move |v, sink| {
                match v["type"].as_str() {
                    Some("item.started") => {
                        // Live status while the CLI runs a command/edits a file.
                        let item = &v["item"];
                        match item["type"].as_str() {
                            Some("command_execution") => {
                                let cmd = clean_cli_command(item["command"].as_str().unwrap_or(""));
                                let id = cli_item_id(item, &format!("codex-command-{cmd}"));
                                send(
                                    sink,
                                    StreamItem::CommandStarted {
                                        id,
                                        command: cmd.clone(),
                                        cwd: String::new(),
                                        background: false,
                                    },
                                );
                                let cmd: String = cmd.chars().take(80).collect();
                                send(
                                    sink,
                                    StreamItem::Notice(format!("{} Running {cmd}", '\u{2699}')),
                                );
                            }
                            Some("file_change") => {
                                let p = item["path"]
                                    .as_str()
                                    .or_else(|| item["text"].as_str())
                                    .unwrap_or("file");
                                send(
                                    sink,
                                    StreamItem::Notice(format!("{} Editing {p}", '\u{2699}')),
                                );
                            }
                            Some("web_search") => {
                                let q = item["query"].as_str().unwrap_or("");
                                send(
                                    sink,
                                    StreamItem::Notice(format!("{} Searching {q}", '\u{2699}')),
                                );
                            }
                            _ => {}
                        }
                    }
                    Some("thread.started") => {
                        if let Some(id) = v["thread_id"].as_str() {
                            session_set(&skey_cb, id);
                            send(sink, StreamItem::CliSession(id.to_string()));
                        }
                    }
                    Some("item.completed") => {
                        let item = &v["item"];
                        match item["type"].as_str() {
                            Some("agent_message") => {
                                if let Some(t) = item["text"].as_str() {
                                    // Held until after the run so it lands below the
                                    // command/activity rows (see text_buf above).
                                    if let Ok(mut buf) = text_buf_cb.lock() {
                                        buf.push(t.to_string());
                                    }
                                }
                            }
                            Some("reasoning") => {
                                if let Some(t) = item["text"].as_str() {
                                    send(sink, StreamItem::ReasoningDelta(t.to_string()));
                                }
                            }
                            Some("command_execution") => {
                                let cmd = clean_cli_command(item["command"].as_str().unwrap_or(""));
                                let id = cli_item_id(item, &format!("codex-command-{cmd}"));
                                let out = item["aggregated_output"].as_str().unwrap_or("");
                                if !out.is_empty() {
                                    send(
                                        sink,
                                        StreamItem::CommandOutput {
                                            id: id.clone(),
                                            stream: "stdout".to_string(),
                                            chunk: out.to_string(),
                                        },
                                    );
                                }
                                let exit_code = cli_exit_code(item);
                                let ok = exit_code.map(|code| code == 0).unwrap_or(true);
                                send(
                                    sink,
                                    StreamItem::CommandFinished {
                                        id,
                                        ok,
                                        exit_code,
                                        duration_ms: cli_duration_ms(item),
                                    },
                                );
                                let out: String = out.chars().take(800).collect();
                                send(
                                    sink,
                                    StreamItem::Notice(
                                        format!("{} {cmd}\n{out}", '\u{2318}')
                                            .trim_end()
                                            .to_string(),
                                    ),
                                );
                            }
                            Some("file_change") => {
                                let summary = item["text"]
                                    .as_str()
                                    .or_else(|| item["path"].as_str())
                                    .unwrap_or("file change");
                                send(
                                    sink,
                                    StreamItem::Notice(format!("{} {summary}", '\u{270e}')),
                                );
                                // Single path or a changes[] list, depending on codex version.
                                if let Some(p) = item["path"].as_str() {
                                    send(sink, StreamItem::FileChanged(p.to_string()));
                                }
                                if let Some(arr) = item["changes"].as_array() {
                                    for c in arr {
                                        if let Some(p) = c["path"].as_str() {
                                            send(sink, StreamItem::FileChanged(p.to_string()));
                                        }
                                    }
                                }
                            }
                            Some("web_search") => {
                                let q = item["query"]
                                    .as_str()
                                    .or_else(|| item["text"].as_str())
                                    .unwrap_or("");
                                send(sink, StreamItem::Notice(format!("{} {q}", '\u{1f50e}')));
                            }
                            Some(_) | None => {}
                        }
                    }
                    Some("turn.completed") => {
                        let u = &v["usage"];
                        send(
                            sink,
                            StreamItem::Usage {
                                input: u["input_tokens"].as_u64().unwrap_or(0),
                                output: u["output_tokens"].as_u64().unwrap_or(0),
                                // codex doesn't report the window here; default 272k.
                                context_window: Some(272_000),
                                cached_input: u["cached_input_tokens"].as_u64().unwrap_or(0),
                                reasoning_output: u["reasoning_output_tokens"]
                                    .as_u64()
                                    .unwrap_or(0),
                            },
                        );
                    }
                    Some("error") => {
                        let msg = v["message"].as_str().unwrap_or("codex error");
                        send(sink, StreamItem::Notice(format!("error: {msg}")));
                    }
                    _ => {}
                }
                true
            },
        )
        .await;
        // Emit the buffered agent text now — after every live command/activity row
        // — so the answer renders below the commands that produced it, never above.
        if let Ok(mut buf) = text_buf.lock() {
            for t in buf.drain(..) {
                send(&sink, StreamItem::TextDelta(t));
            }
        }
        result
    }
}

/// Drives the local `claude` (Claude Code) CLI.
pub struct ClaudeCliProvider {
    bin: String,
}

impl ClaudeCliProvider {
    pub fn new() -> Self {
        Self {
            bin: resolve_bin("claude", "OXIDE_CLAUDE_BIN"),
        }
    }
}

impl Default for ClaudeCliProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for ClaudeCliProvider {
    fn name(&self) -> &str {
        "claude"
    }

    async fn stream(&self, req: TurnRequest, sink: mpsc::Sender<StreamItem>) -> anyhow::Result<()> {
        let (mut prompt, images) = extract_cli_images(&req);
        // claude -p has no --image flag; hand it the file paths so its own Read
        // tool (which renders images) loads them.
        if !images.is_empty() {
            prompt.push_str("\n\nAttached image file(s) — use your Read tool to view:\n");
            for img in &images {
                prompt.push_str(&format!("- {img}\n"));
            }
        }
        let skey = session_key(&self.bin, &req.conversation_id, &req.cwd);
        // Continuing an imported Claude TUI session ("claude-<uuid>") resumes
        // claude's OWN native session by that uuid, with full context and no replay.
        // The persisted link (survives restarts) wins over the in-memory map.
        let resume = req
            .cli_resume
            .clone()
            .or_else(|| session_get(&skey))
            .or_else(|| {
                req.conversation_id
                    .strip_prefix("claude-")
                    .map(str::to_string)
            });
        let mut args = vec![
            "-p".to_string(),
            prompt,
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--verbose".to_string(),
            // Token-level deltas: real streaming AND keeps the idle timer fed.
            "--include-partial-messages".to_string(),
            "--dangerously-skip-permissions".to_string(),
        ];
        if let Some(id) = &resume {
            // Continue the same Claude Code session — context carries across turns.
            args.push("--resume".to_string());
            args.push(id.clone());
        }
        if let Some(model) = claude_model_arg(&req.model) {
            args.push("--model".to_string());
            args.push(model.to_string());
        }
        if !req.reasoning_effort.is_empty() {
            args.push("--effort".to_string());
            args.push(claude_effort(&req.reasoning_effort).to_string());
        }

        let skey_cb = skey.clone();
        let timeout = claude_cli_timeout()?;
        // With partial messages on, text arrives via stream_event deltas; the
        // final `assistant` message would duplicate it, so skip its text blocks.
        let mut saw_partial = false;
        run_jsonl(
            &self.bin,
            &args,
            &req.cwd,
            None,
            timeout,
            &sink,
            move |v, sink| {
                match v["type"].as_str() {
                    Some("system") => {
                        if let Some(id) = v["session_id"].as_str() {
                            session_set(&skey_cb, id);
                            send(sink, StreamItem::CliSession(id.to_string()));
                        }
                    }
                    Some("stream_event") => {
                        let ev = &v["event"];
                        // Each new assistant message resets the dedupe latch, so a
                        // later message's final text isn't dropped because an earlier
                        // one streamed deltas.
                        if ev["type"].as_str() == Some("message_start") {
                            saw_partial = false;
                        }
                        if ev["type"].as_str() == Some("content_block_delta") {
                            match ev["delta"]["type"].as_str() {
                                Some("text_delta") => {
                                    if let Some(t) = ev["delta"]["text"].as_str() {
                                        saw_partial = true;
                                        send(sink, StreamItem::TextDelta(t.to_string()));
                                    }
                                }
                                Some("thinking_delta") => {
                                    if let Some(t) = ev["delta"]["thinking"].as_str() {
                                        send(sink, StreamItem::ReasoningDelta(t.to_string()));
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    Some("assistant") => {
                        if let Some(content) = v["message"]["content"].as_array() {
                            for block in content {
                                match block["type"].as_str() {
                                    Some("text") => {
                                        if !saw_partial {
                                            if let Some(t) = block["text"].as_str() {
                                                send(sink, StreamItem::TextDelta(t.to_string()));
                                            }
                                        }
                                    }
                                    Some("tool_use") => {
                                        let name = block["name"].as_str().unwrap_or("tool");
                                        // Pull the human-relevant arg so the live status reads
                                        // "Read src/lib.rs", not a bare tool name.
                                        let input = &block["input"];
                                        let detail = [
                                            "file_path",
                                            "path",
                                            "command",
                                            "pattern",
                                            "query",
                                            "url",
                                            "description",
                                        ]
                                        .iter()
                                        .find_map(|k| input[k].as_str())
                                        .unwrap_or("");
                                        let detail: String = detail.chars().take(80).collect();
                                        // A backgrounded command ("I'll let you know when done")
                                        // won't stream its result back — surface WHAT it's doing
                                        // with a distinct clock marker so the UI can show it persistently.
                                        let bg = input["run_in_background"].as_bool() == Some(true);
                                        let is_command = matches!(name, "Bash" | "Shell")
                                            || input["command"].as_str().is_some();
                                        if is_command {
                                            // A command is fully shown by its command row (started,
                                            // output, finished); emitting a duplicate Bash notice on top
                                            // would leave a second, redundant activity row lingering.
                                            let command = input["command"]
                                                .as_str()
                                                .unwrap_or(detail.as_str())
                                                .to_string();
                                            let id = block["id"]
                                                .as_str()
                                                .map(str::to_string)
                                                .unwrap_or_else(|| {
                                                    format!("claude-command-{command}")
                                                });
                                            send(
                                                sink,
                                                StreamItem::CommandStarted {
                                                    id,
                                                    command,
                                                    cwd: String::new(),
                                                    background: bg,
                                                },
                                            );
                                        } else {
                                            let label = if detail.is_empty() {
                                                format!("{} {name}", '\u{2699}')
                                            } else {
                                                format!("{} {name} {detail}", '\u{2699}')
                                            };
                                            send(sink, StreamItem::Notice(label));
                                        }
                                        if matches!(
                                            name,
                                            "Edit" | "Write" | "MultiEdit" | "NotebookEdit"
                                        ) {
                                            if let Some(p) = input["file_path"].as_str() {
                                                send(sink, StreamItem::FileChanged(p.to_string()));
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    Some("result") => {
                        if let Some(id) = v["session_id"].as_str() {
                            session_set(&skey_cb, id);
                            send(sink, StreamItem::CliSession(id.to_string()));
                        }
                        if v["is_error"].as_bool() == Some(true) {
                            let msg = v["result"].as_str().unwrap_or("Claude CLI error");
                            send(sink, StreamItem::Notice(format!("error: {msg}")));
                        }
                        let u = &v["usage"];
                        let window = v["modelUsage"]
                            .as_object()
                            .and_then(|m| m.values().next())
                            .and_then(|mu| mu["contextWindow"].as_u64());
                        send(
                            sink,
                            StreamItem::Usage {
                                input: u["input_tokens"].as_u64().unwrap_or(0),
                                output: u["output_tokens"].as_u64().unwrap_or(0),
                                context_window: window,
                                cached_input: u["cached_input_tokens"].as_u64().unwrap_or(0),
                                reasoning_output: u["reasoning_output_tokens"]
                                    .as_u64()
                                    .unwrap_or(0),
                            },
                        );
                    }
                    _ => {}
                }
                true
            },
        )
        .await
    }
}

/// Drives interactive Claude Code through a PTY while preserving Oxide's chat UI.
///
/// This intentionally avoids `claude -p`: Claude runs as a normal interactive
/// TTY session, receives the prompt through bracketed paste, and Oxide follows
/// Claude Code's native JSONL transcript for the assistant text/tool events.
pub struct ClaudeInteractiveProvider {
    bin: String,
}

impl ClaudeInteractiveProvider {
    pub fn new() -> Self {
        Self {
            bin: resolve_bin("claude", "OXIDE_CLAUDE_BIN"),
        }
    }
}

impl Default for ClaudeInteractiveProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for ClaudeInteractiveProvider {
    fn name(&self) -> &str {
        "claude_interactive"
    }

    async fn stream(&self, req: TurnRequest, sink: mpsc::Sender<StreamItem>) -> anyhow::Result<()> {
        let (mut prompt, images) = extract_cli_images(&req);
        if !images.is_empty() {
            prompt.push_str("\n\nAttached image file(s) — use your Read tool to view:\n");
            for img in &images {
                prompt.push_str(&format!("- {img}\n"));
            }
        }
        let skey = session_key(&self.bin, &req.conversation_id, &req.cwd);
        let resume = req
            .cli_resume
            .clone()
            .or_else(|| session_get(&skey))
            .or_else(|| {
                req.conversation_id
                    .strip_prefix("claude-")
                    .map(str::to_string)
            });
        let session_id = resume.clone().unwrap_or_else(new_claude_session_id);
        session_set(&skey, &session_id);
        send(&sink, StreamItem::CliSession(session_id.clone()));

        let transcript = claude_transcript_path(&req.cwd, &session_id)?;
        let baseline_lines = count_file_lines(&transcript);
        let timeout = claude_interactive_timeout()?;
        let result = run_claude_interactive_turn(ClaudeInteractiveTurn {
            bin: &self.bin,
            req: &req,
            prompt: &prompt,
            session_id: &session_id,
            resume: resume.as_deref(),
            transcript: &transcript,
            baseline_lines,
            timeout,
            sink: &sink,
        })
        .await;

        let _ = sink.send(StreamItem::Done).await;
        result
    }
}

struct PtyChildGuard {
    child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
}

impl PtyChildGuard {
    fn new(child: Box<dyn portable_pty::Child + Send + Sync>) -> Self {
        Self { child: Some(child) }
    }

    fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
        match self.child.as_mut() {
            Some(child) => child.try_wait(),
            None => Ok(None),
        }
    }

    fn kill(&mut self) -> std::io::Result<()> {
        match self.child.as_mut() {
            Some(child) => child.kill(),
            None => Ok(()),
        }
    }
}

impl Drop for PtyChildGuard {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}

#[derive(Debug, Clone)]
struct ClaudeToolUse {
    id: String,
    name: String,
    detail: String,
    command: Option<String>,
    file_path: Option<String>,
    background: bool,
    /// Transcript position (line, block) so this row emits in true order
    /// relative to assistant text — a tool never streams above the text that
    /// preceded it. Mirrors Synara's per-item sequence key.
    pos: (usize, usize),
}

/// An assistant text segment carrying its transcript position, so it interleaves
/// with `ClaudeToolUse` rows in the order the model actually emitted them
/// (text-before-its-tools), instead of being dumped after every tool row.
#[derive(Debug, Clone)]
struct ClaudeTextBlock {
    pos: (usize, usize),
    text: String,
}

#[derive(Debug, Clone)]
struct ClaudeToolResult {
    id: String,
    content: String,
    is_error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ClaudeTranscriptTail {
    #[default]
    None,
    User,
    AssistantText,
    AssistantToolUse,
    Other,
}

#[derive(Debug, Default)]
struct ClaudeTranscriptSnapshot {
    session_id: Option<String>,
    /// Full assistant text, concatenated — kept for "has activity" / settle
    /// checks. Ordered emission uses `assistant_blocks` instead.
    assistant_text: String,
    /// Assistant text segments with transcript positions, interleaved at emit
    /// time with `tool_uses` so order matches what the model produced.
    assistant_blocks: Vec<ClaudeTextBlock>,
    tail: ClaudeTranscriptTail,
    tool_uses: Vec<ClaudeToolUse>,
    tool_results: Vec<ClaudeToolResult>,
    user_prompt_seen: bool,
    turn_complete: bool,
    usage: Option<(u64, u64, Option<u64>)>,
}

struct ClaudeInteractiveTurn<'a> {
    bin: &'a str,
    req: &'a TurnRequest,
    prompt: &'a str,
    session_id: &'a str,
    resume: Option<&'a str>,
    transcript: &'a Path,
    baseline_lines: usize,
    timeout: Duration,
    sink: &'a mpsc::Sender<StreamItem>,
}

async fn run_claude_interactive_turn(turn: ClaudeInteractiveTurn<'_>) -> anyhow::Result<()> {
    let ClaudeInteractiveTurn {
        bin,
        req,
        prompt,
        session_id,
        resume,
        transcript,
        baseline_lines,
        timeout,
        sink,
    } = turn;
    let pty = portable_pty::native_pty_system();
    let pair = pty
        .openpty(portable_pty::PtySize {
            rows: 36,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| anyhow::anyhow!("failed to open Claude interactive PTY: {e}"))?;

    let mut cmd = portable_pty::CommandBuilder::new(bin);
    cmd.arg("--dangerously-skip-permissions");
    if let Some(id) = resume {
        cmd.arg("--resume");
        cmd.arg(id);
    } else {
        cmd.arg("--session-id");
        cmd.arg(session_id);
    }
    if let Some(model) = claude_model_arg(&req.model) {
        cmd.arg("--model");
        cmd.arg(model);
    }
    if !req.reasoning_effort.is_empty() {
        cmd.arg("--effort");
        cmd.arg(claude_effort(&req.reasoning_effort));
    }
    if !req.cwd.is_empty() {
        cmd.cwd(&req.cwd);
    }
    cmd.env("TERM", "xterm-256color");
    if let Ok(home) = std::env::var("HOME") {
        let path = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{home}/.superconductor/bin:{home}/.local/bin:{home}/.bun/bin:/opt/homebrew/bin:/usr/local/bin:{path}"));
    }

    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| anyhow::anyhow!("failed to spawn interactive Claude Code '{bin}': {e}"))?;
    let mut child = PtyChildGuard::new(child);
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| anyhow::anyhow!("failed to read Claude interactive PTY: {e}"))?;
    let mut writer = pair
        .master
        .take_writer()
        .map_err(|e| anyhow::anyhow!("failed to write Claude interactive PTY: {e}"))?;
    let _master = pair.master;
    let (pty_tx, pty_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if pty_tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut terminal_tail = String::new();
    wait_for_claude_prompt(&pty_rx, &mut terminal_tail);
    writer
        .write_all(&interactive_paste_bytes(prompt))
        .map_err(|e| anyhow::anyhow!("failed to send prompt to interactive Claude Code: {e}"))?;
    writer
        .flush()
        .map_err(|e| anyhow::anyhow!("failed to flush prompt to interactive Claude Code: {e}"))?;

    send(
        sink,
        StreamItem::Notice("Claude Code interactive session started".to_string()),
    );

    let started = Instant::now();
    let mut last_change = Instant::now();
    let mut last_pty_output = Instant::now();
    let mut pending_text = String::new();
    let mut emitted_tools: HashSet<String> = HashSet::new();
    let mut command_tools: HashSet<String> = HashSet::new();
    let mut emitted_text_pos: HashSet<(usize, usize)> = HashSet::new();
    let mut emitted_results: HashSet<String> = HashSet::new();
    let mut pending_usage: Option<(u64, u64, Option<u64>)> = None;
    let mut usage_emitted = false;
    let mut prompt_accepted = false;
    let mut prompt_retry_at: Option<Instant> = None;
    let mut interval = tokio::time::interval(CLAUDE_INTERACTIVE_POLL);

    loop {
        interval.tick().await;
        while let Ok(bytes) = pty_rx.try_recv() {
            last_pty_output = Instant::now();
            push_tail(&mut terminal_tail, &bytes);
        }

        if started.elapsed() > timeout {
            let _ = child.kill();
            return Err(anyhow::anyhow!(
                "Claude interactive turn timed out after {}",
                format_timeout(timeout)
            ));
        }

        let snapshot = parse_claude_transcript(transcript, baseline_lines);
        let transcript_has_activity = snapshot.user_prompt_seen
            || !snapshot.assistant_text.trim().is_empty()
            || !snapshot.tool_uses.is_empty()
            || !snapshot.tool_results.is_empty()
            || snapshot.turn_complete;
        if transcript_has_activity {
            prompt_accepted = true;
        }
        if let Some(id) = snapshot.session_id.as_deref() {
            send(sink, StreamItem::CliSession(id.to_string()));
        }
        // Emit assistant text and tool rows in true transcript order — text
        // that precedes a tool streams before it (fixes "command appears, then
        // its text below it"). The trailing text block is held back so the
        // final answer still waits out the settle window before it lands.
        if emit_claude_ordered(
            sink,
            &snapshot,
            &mut emitted_tools,
            &mut command_tools,
            &mut emitted_text_pos,
            false,
        ) {
            last_change = Instant::now();
        }
        for result in &snapshot.tool_results {
            if !command_tools.contains(&result.id) || !emitted_results.insert(result.id.clone()) {
                continue;
            }
            if !result.content.is_empty() {
                send(
                    sink,
                    StreamItem::CommandOutput {
                        id: result.id.clone(),
                        stream: if result.is_error { "stderr" } else { "stdout" }.to_string(),
                        chunk: result.content.clone(),
                    },
                );
            }
            send(
                sink,
                StreamItem::CommandFinished {
                    id: result.id.clone(),
                    ok: !result.is_error,
                    exit_code: None,
                    duration_ms: 0,
                },
            );
            last_change = Instant::now();
        }
        if snapshot.assistant_text != pending_text {
            pending_text = snapshot.assistant_text.clone();
            last_change = Instant::now();
        }
        if let Some(usage) = snapshot.usage {
            pending_usage = Some(usage);
        }

        if !prompt_accepted {
            let since_input = prompt_retry_at
                .map(|at| at.elapsed())
                .unwrap_or_else(|| started.elapsed());
            if since_input >= CLAUDE_INTERACTIVE_PROMPT_ACCEPT_TIMEOUT {
                if prompt_retry_at.is_none() {
                    send(
                        sink,
                        StreamItem::Notice(
                            "Claude interactive prompt was not accepted; retrying input"
                                .to_string(),
                        ),
                    );
                    writer.write_all(b"\x03").map_err(|e| {
                        anyhow::anyhow!("failed to reset interactive Claude prompt: {e}")
                    })?;
                    writer.flush().map_err(|e| {
                        anyhow::anyhow!("failed to flush interactive Claude reset: {e}")
                    })?;
                    wait_for_claude_prompt(&pty_rx, &mut terminal_tail);
                    writer
                        .write_all(&interactive_retry_bytes(prompt))
                        .map_err(|e| {
                            anyhow::anyhow!(
                                "failed to retry prompt to interactive Claude Code: {e}"
                            )
                        })?;
                    writer.flush().map_err(|e| {
                        anyhow::anyhow!(
                            "failed to flush retried prompt to interactive Claude Code: {e}"
                        )
                    })?;
                    prompt_retry_at = Some(Instant::now());
                    last_change = Instant::now();
                    continue;
                }
                return Err(anyhow::anyhow!(
                    "interactive Claude Code did not accept the prompt within {} seconds{}",
                    CLAUDE_INTERACTIVE_PROMPT_ACCEPT_TIMEOUT.as_secs(),
                    tail_context(&terminal_tail)
                ));
            }
        }

        if let Some(status) = child.try_wait()? {
            if !status.success() && pending_text.trim().is_empty() {
                return Err(anyhow::anyhow!(
                    "interactive Claude Code exited with status {}{}",
                    status.exit_code(),
                    tail_context(&terminal_tail)
                ));
            }
            emit_claude_ordered(
                sink,
                &snapshot,
                &mut emitted_tools,
                &mut command_tools,
                &mut emitted_text_pos,
                true,
            );
            emit_claude_usage(sink, pending_usage, &mut usage_emitted);
            break;
        }

        let final_text_ready = !pending_text.trim().is_empty()
            && last_change.elapsed() >= CLAUDE_INTERACTIVE_SETTLE
            && (snapshot.turn_complete
                || (snapshot.tail == ClaudeTranscriptTail::AssistantText
                    && last_pty_output.elapsed() >= CLAUDE_INTERACTIVE_SETTLE));
        if final_text_ready {
            emit_claude_ordered(
                sink,
                &snapshot,
                &mut emitted_tools,
                &mut command_tools,
                &mut emitted_text_pos,
                true,
            );
            emit_claude_usage(sink, pending_usage, &mut usage_emitted);
            break;
        }
    }

    let _ = child.kill();
    Ok(())
}

/// Emit assistant text segments and tool rows in transcript order, so a tool
/// never streams above the text that preceded it. Text and tools are merged by
/// their `(line, block)` position — the order the model produced them.
///
/// `include_trailing` controls the LAST text block: while polling we pass
/// `false` so the final answer isn't emitted until it settles; on the settle /
/// turn-complete path we pass `true` to flush it. Already-emitted rows are
/// skipped via `emitted_tools` / `emitted_text_pos`, so repeated polls don't
/// duplicate. Returns whether anything new was emitted (to bump `last_change`).
fn emit_claude_ordered(
    sink: &mpsc::Sender<StreamItem>,
    snapshot: &ClaudeTranscriptSnapshot,
    emitted_tools: &mut HashSet<String>,
    command_tools: &mut HashSet<String>,
    emitted_text_pos: &mut HashSet<(usize, usize)>,
    include_trailing: bool,
) -> bool {
    enum Item<'a> {
        Text(&'a ClaudeTextBlock),
        Tool(&'a ClaudeToolUse),
    }
    let mut items: Vec<((usize, usize), Item)> = Vec::new();
    for block in &snapshot.assistant_blocks {
        items.push((block.pos, Item::Text(block)));
    }
    for tool in &snapshot.tool_uses {
        items.push((tool.pos, Item::Tool(tool)));
    }
    items.sort_by_key(|(pos, _)| *pos);
    let last_idx = items.len().saturating_sub(1);

    let mut changed = false;
    for (idx, (_pos, item)) in items.iter().enumerate() {
        match item {
            Item::Tool(tool) => {
                if !emitted_tools.insert(tool.id.clone()) {
                    continue;
                }
                if tool.command.is_some() {
                    command_tools.insert(tool.id.clone());
                }
                emit_claude_tool_use(sink, tool);
                changed = true;
            }
            Item::Text(block) => {
                // Hold the trailing text (the in-progress final answer) until the
                // settle path passes include_trailing — a tool appearing after it
                // proves it's complete and lets it emit on the next poll.
                if idx == last_idx && !include_trailing {
                    continue;
                }
                if block.text.trim().is_empty() || !emitted_text_pos.insert(block.pos) {
                    continue;
                }
                send(sink, StreamItem::TextDelta(block.text.clone()));
                changed = true;
            }
        }
    }
    changed
}

fn emit_claude_usage(
    sink: &mpsc::Sender<StreamItem>,
    usage: Option<(u64, u64, Option<u64>)>,
    usage_emitted: &mut bool,
) {
    if !*usage_emitted {
        if let Some((input, output, context_window)) = usage {
            send(
                sink,
                StreamItem::Usage {
                    input,
                    output,
                    context_window,
                    cached_input: 0,
                    reasoning_output: 0,
                },
            );
            *usage_emitted = true;
        }
    }
}

fn emit_claude_tool_use(sink: &mpsc::Sender<StreamItem>, tool: &ClaudeToolUse) {
    if let Some(command) = &tool.command {
        // A command tool is fully represented by its command row (started,
        // output, finished). Emitting a duplicate Bash notice on top of it would
        // add a second, redundant activity row that lingers after the turn.
        send(
            sink,
            StreamItem::CommandStarted {
                id: tool.id.clone(),
                command: command.clone(),
                cwd: String::new(),
                background: tool.background,
            },
        );
    } else {
        let label = if tool.background {
            if tool.detail.is_empty() {
                format!("{} {}", '\u{23f3}', tool.name)
            } else {
                format!("{} {} {}", '\u{23f3}', tool.name, tool.detail)
            }
        } else if tool.detail.is_empty() {
            format!("{} {}", '\u{2699}', tool.name)
        } else {
            format!("{} {} {}", '\u{2699}', tool.name, tool.detail)
        };
        send(sink, StreamItem::Notice(label));
    }
    if let Some(path) = &tool.file_path {
        send(sink, StreamItem::FileChanged(path.clone()));
    }
}

fn wait_for_claude_prompt(rx: &std::sync::mpsc::Receiver<Vec<u8>>, terminal_tail: &mut String) {
    let started = Instant::now();
    loop {
        while let Ok(bytes) = rx.try_recv() {
            push_tail(terminal_tail, &bytes);
        }
        if claude_prompt_ready(terminal_tail)
            || started.elapsed() >= CLAUDE_INTERACTIVE_READY_TIMEOUT
        {
            std::thread::sleep(Duration::from_millis(250));
            while let Ok(bytes) = rx.try_recv() {
                push_tail(terminal_tail, &bytes);
            }
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn claude_prompt_ready(tail: &str) -> bool {
    tail.contains("Claude Code")
        && (tail.contains("Try")
            || tail.contains("bypass permissions")
            || tail.contains("/effort")
            || tail.contains('\u{276f}'))
}

fn parse_claude_transcript(path: &Path, baseline_lines: usize) -> ClaudeTranscriptSnapshot {
    let Ok(text) = std::fs::read_to_string(path) else {
        return ClaudeTranscriptSnapshot::default();
    };
    let mut snapshot = ClaudeTranscriptSnapshot::default();
    for (line_idx, line) in text.lines().enumerate().skip(baseline_lines) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if snapshot.session_id.is_none() {
            snapshot.session_id = transcript_session_id(&v);
        }
        if v["type"].as_str() == Some("system") && v["subtype"].as_str() == Some("turn_duration") {
            snapshot.turn_complete = true;
            snapshot.tail = ClaudeTranscriptTail::Other;
            continue;
        }
        if v["type"].as_str() == Some("last-prompt") && !snapshot.assistant_text.trim().is_empty() {
            snapshot.turn_complete = true;
            snapshot.tail = ClaudeTranscriptTail::Other;
            continue;
        }
        let role = v["type"]
            .as_str()
            .or_else(|| v["message"]["role"].as_str())
            .unwrap_or("");
        match role {
            "assistant" => parse_claude_assistant_line(&v, line_idx, &mut snapshot),
            "user" => parse_claude_user_line(&v, &mut snapshot),
            _ => snapshot.tail = ClaudeTranscriptTail::Other,
        }
    }
    snapshot
}

fn parse_claude_user_line(v: &Value, snapshot: &mut ClaudeTranscriptSnapshot) {
    snapshot.user_prompt_seen = true;
    if let Value::Array(blocks) = &v["message"]["content"] {
        for block in blocks {
            if block["type"].as_str() != Some("tool_result") {
                continue;
            }
            let id = block["tool_use_id"].as_str().unwrap_or("").trim();
            if id.is_empty() {
                continue;
            }
            snapshot.tool_results.push(ClaudeToolResult {
                id: id.to_string(),
                content: tool_result_content(block),
                is_error: block["is_error"].as_bool() == Some(true),
            });
        }
    }
    snapshot.tail = ClaudeTranscriptTail::User;
}

fn parse_claude_assistant_line(
    v: &Value,
    line_idx: usize,
    snapshot: &mut ClaudeTranscriptSnapshot,
) {
    let content = &v["message"]["content"];
    let mut text_parts: Vec<String> = Vec::new();
    let mut first_text_block: Option<usize> = None;
    let mut has_tool_use = false;
    match content {
        Value::String(s) => {
            if !s.trim().is_empty() {
                first_text_block = Some(0);
                text_parts.push(s.trim().to_string());
            }
        }
        Value::Array(blocks) => {
            for (block_idx, block) in blocks.iter().enumerate() {
                match block["type"].as_str() {
                    Some("text") => {
                        if let Some(text) = block["text"].as_str() {
                            if !text.trim().is_empty() {
                                first_text_block.get_or_insert(block_idx);
                                text_parts.push(text.trim().to_string());
                            }
                        }
                    }
                    Some("tool_use") => {
                        has_tool_use = true;
                        snapshot
                            .tool_uses
                            .push(parse_claude_tool_use(block, line_idx, block_idx));
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    if !text_parts.is_empty() {
        let joined = text_parts.join("\n");
        // Position the block at its first text block so it sorts before the
        // tool_use blocks that follow it within the same assistant message.
        snapshot.assistant_blocks.push(ClaudeTextBlock {
            pos: (line_idx, first_text_block.unwrap_or(0)),
            text: joined.clone(),
        });
        push_text_message(&mut snapshot.assistant_text, &joined);
    }
    snapshot.usage = transcript_usage(v).or(snapshot.usage);
    snapshot.tail = if has_tool_use {
        ClaudeTranscriptTail::AssistantToolUse
    } else if !text_parts.is_empty() {
        ClaudeTranscriptTail::AssistantText
    } else {
        ClaudeTranscriptTail::Other
    };
}

fn parse_claude_tool_use(block: &Value, line_idx: usize, block_idx: usize) -> ClaudeToolUse {
    let name = block["name"].as_str().unwrap_or("tool").to_string();
    let input = &block["input"];
    let detail = [
        "file_path",
        "path",
        "command",
        "pattern",
        "query",
        "url",
        "description",
    ]
    .iter()
    .find_map(|k| input[*k].as_str())
    .unwrap_or("")
    .chars()
    .take(120)
    .collect::<String>();
    let command = input["command"].as_str().map(str::to_string);
    let file_path = if matches!(
        name.as_str(),
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit"
    ) {
        input["file_path"].as_str().map(str::to_string)
    } else {
        None
    };
    ClaudeToolUse {
        id: block["id"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| format!("claude-interactive-tool-{line_idx}-{block_idx}")),
        name,
        detail,
        command,
        file_path,
        background: input["run_in_background"].as_bool() == Some(true),
        pos: (line_idx, block_idx),
    }
}

fn tool_result_content(block: &Value) -> String {
    match &block["content"] {
        Value::String(s) => s.chars().take(8000).collect(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                item["text"]
                    .as_str()
                    .or_else(|| item["content"].as_str())
                    .map(str::to_string)
            })
            .collect::<Vec<_>>()
            .join("\n")
            .chars()
            .take(8000)
            .collect(),
        _ => String::new(),
    }
}

fn transcript_session_id(v: &Value) -> Option<String> {
    v["sessionId"]
        .as_str()
        .or_else(|| v["session_id"].as_str())
        .filter(|id| !id.trim().is_empty())
        .map(str::to_string)
}

fn transcript_usage(v: &Value) -> Option<(u64, u64, Option<u64>)> {
    let usage = if v["message"]["usage"].is_object() {
        &v["message"]["usage"]
    } else {
        &v["usage"]
    };
    let input = usage["input_tokens"].as_u64().unwrap_or(0);
    let output = usage["output_tokens"].as_u64().unwrap_or(0);
    if input == 0 && output == 0 {
        return None;
    }
    let context_window = v["modelUsage"]
        .as_object()
        .and_then(|m| m.values().next())
        .and_then(|mu| mu["contextWindow"].as_u64());
    Some((input, output, context_window))
}

fn push_text_message(buf: &mut String, text: &str) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    if !buf.is_empty() {
        buf.push_str("\n\n");
    }
    buf.push_str(trimmed);
}

fn claude_transcript_path(cwd: &str, session_id: &str) -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME is not set; cannot locate Claude Code transcripts"))?;
    let workspace = if cwd.trim().is_empty() {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        PathBuf::from(cwd)
    };
    let slug = workspace.display().to_string().replace(['/', '.'], "-");
    Ok(home
        .join(".claude/projects")
        .join(slug)
        .join(format!("{session_id}.jsonl")))
}

fn count_file_lines(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

fn interactive_paste_bytes(prompt: &str) -> Vec<u8> {
    let normalized = prompt.replace("\r\n", "\n").replace('\r', "\n");
    if normalized.trim().is_empty() {
        return b"\r".to_vec();
    }
    format!("\x1b[200~{normalized}\x1b[201~\r").into_bytes()
}

fn interactive_retry_bytes(prompt: &str) -> Vec<u8> {
    let normalized = prompt.replace("\r\n", "\n").replace('\r', "\n");
    if normalized.trim().is_empty() {
        return b"\r".to_vec();
    }
    if !normalized.contains('\n') && normalized.len() <= 2000 {
        return format!("{normalized}\r").into_bytes();
    }
    interactive_paste_bytes(&normalized)
}

fn new_claude_session_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let count = CLAUDE_INTERACTIVE_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed) as u128;
    let pid = std::process::id() as u128;
    let mut x = now ^ (pid << 64) ^ count;
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51afd7ed558ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ceb9fe1a85ec53);
    x ^= x >> 33;
    let mut bytes = x.to_be_bytes();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
    )
}

fn push_tail(tail: &mut String, bytes: &[u8]) {
    let piece = String::from_utf8_lossy(bytes);
    tail.push_str(&strip_ansi(&piece));
    if tail.chars().count() > 2400 {
        let keep = tail
            .chars()
            .rev()
            .take(2000)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<String>();
        *tail = keep;
    }
}

fn strip_ansi(input: &str) -> String {
    let mut out = String::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            out.push(ch);
            continue;
        }
        if chars.peek() == Some(&'[') {
            let _ = chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() || matches!(c, '~') {
                    break;
                }
            }
        }
    }
    out
}

fn tail_context(tail: &str) -> String {
    let clean = tail.trim();
    if clean.is_empty() {
        String::new()
    } else {
        format!(" — {}", clean.chars().take(700).collect::<String>())
    }
}

fn codex_effort(effort: &str) -> &str {
    effort
}

fn claude_effort(effort: &str) -> &str {
    // claude --effort accepts low|medium|high|xhigh|max directly.
    effort
}

fn claude_model_arg(model: &str) -> Option<&str> {
    let model = model.trim();
    if model.is_empty() {
        return None;
    }
    let lower = model.to_ascii_lowercase();
    if lower.starts_with("claude-")
        || matches!(lower.as_str(), "fable" | "opus" | "sonnet" | "haiku")
    {
        Some(model)
    } else {
        None
    }
}

#[cfg(test)]
mod claude_interactive_tests {
    use super::*;
    use crate::Message;

    #[test]
    fn parses_interactive_transcript_after_baseline() {
        let path = std::env::temp_dir().join(format!(
            "oxide-claude-interactive-{}.jsonl",
            new_claude_session_id()
        ));
        let text = r#"{"type":"assistant","sessionId":"old","message":{"content":[{"type":"text","text":"old"}]}}
{"type":"user","sessionId":"abc","message":{"content":"check"}}
{"type":"assistant","sessionId":"abc","message":{"content":[{"type":"text","text":"I'll inspect."},{"type":"tool_use","id":"tool-1","name":"Bash","input":{"command":"git status"}}],"usage":{"input_tokens":10,"output_tokens":20}}}
{"type":"user","sessionId":"abc","message":{"content":[{"type":"tool_result","tool_use_id":"tool-1","content":"ok"}]}}
{"type":"assistant","sessionId":"abc","message":{"content":[{"type":"text","text":"Done."}],"usage":{"input_tokens":11,"output_tokens":22}}}
{"type":"system","subtype":"turn_duration","durationMs":1200,"sessionId":"abc"}
"#;
        std::fs::write(&path, text).unwrap();
        let snapshot = parse_claude_transcript(&path, 1);
        let _ = std::fs::remove_file(&path);

        assert_eq!(snapshot.session_id.as_deref(), Some("abc"));
        assert_eq!(snapshot.assistant_text, "I'll inspect.\n\nDone.");
        assert_eq!(snapshot.tail, ClaudeTranscriptTail::Other);
        assert!(snapshot.user_prompt_seen);
        assert!(snapshot.turn_complete);
        assert_eq!(snapshot.tool_uses.len(), 1);
        assert_eq!(snapshot.tool_uses[0].command.as_deref(), Some("git status"));
        assert_eq!(snapshot.tool_results.len(), 1);
        assert_eq!(snapshot.tool_results[0].id, "tool-1");
        assert_eq!(snapshot.tool_results[0].content, "ok");
        assert_eq!(snapshot.usage, Some((11, 22, None)));
        // Text and tool carry transcript positions so emission can interleave them.
        assert_eq!(snapshot.assistant_blocks.len(), 2);
        assert_eq!(snapshot.assistant_blocks[0].text, "I'll inspect.");
        assert!(snapshot.assistant_blocks[0].pos < snapshot.tool_uses[0].pos);
    }

    #[test]
    fn ordered_emit_streams_text_before_its_tool() {
        let path = std::env::temp_dir().join(format!(
            "oxide-claude-order-{}.jsonl",
            new_claude_session_id()
        ));
        // Assistant says "Looking.", runs a command, then answers "Found it.".
        let text = r#"{"type":"user","sessionId":"abc","message":{"content":"go"}}
{"type":"assistant","sessionId":"abc","message":{"content":[{"type":"text","text":"Looking."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","sessionId":"abc","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"out"}]}}
{"type":"assistant","sessionId":"abc","message":{"content":[{"type":"text","text":"Found it."}]}}
"#;
        std::fs::write(&path, text).unwrap();
        let snapshot = parse_claude_transcript(&path, 0);
        let _ = std::fs::remove_file(&path);

        let (tx, mut rx) = mpsc::channel::<StreamItem>(64);
        let mut emitted_tools = HashSet::new();
        let mut command_tools = HashSet::new();
        let mut emitted_text_pos = HashSet::new();

        // Polling pass: trailing answer is held back, but "Looking." emits before `ls`.
        emit_claude_ordered(
            &tx,
            &snapshot,
            &mut emitted_tools,
            &mut command_tools,
            &mut emitted_text_pos,
            false,
        );
        let mut order: Vec<String> = Vec::new();
        while let Ok(item) = rx.try_recv() {
            match item {
                StreamItem::TextDelta(t) => order.push(format!("text:{t}")),
                StreamItem::CommandStarted { command, .. } => order.push(format!("cmd:{command}")),
                _ => {}
            }
        }
        assert_eq!(
            order,
            vec!["text:Looking.".to_string(), "cmd:ls".to_string()]
        );

        // Settle pass: the final answer lands, exactly once, with no re-emit of the rest.
        emit_claude_ordered(
            &tx,
            &snapshot,
            &mut emitted_tools,
            &mut command_tools,
            &mut emitted_text_pos,
            true,
        );
        let mut tail: Vec<String> = Vec::new();
        while let Ok(item) = rx.try_recv() {
            if let StreamItem::TextDelta(t) = item {
                tail.push(t);
            }
        }
        assert_eq!(tail, vec!["Found it.".to_string()]);
    }

    #[test]
    fn startup_metadata_does_not_accept_interactive_prompt() {
        let path = std::env::temp_dir().join(format!(
            "oxide-claude-interactive-{}.jsonl",
            new_claude_session_id()
        ));
        let text = r#"{"type":"last-prompt","sessionId":"abc"}
{"type":"mode","mode":"normal","sessionId":"abc"}
{"type":"permission-mode","permissionMode":"bypassPermissions","sessionId":"abc"}
"#;
        std::fs::write(&path, text).unwrap();
        let snapshot = parse_claude_transcript(&path, 0);
        let _ = std::fs::remove_file(&path);

        assert_eq!(snapshot.session_id.as_deref(), Some("abc"));
        assert!(!snapshot.user_prompt_seen);
        assert!(snapshot.assistant_text.is_empty());
        assert!(snapshot.tool_uses.is_empty());
        assert!(!snapshot.turn_complete);
    }

    #[test]
    fn prompt_is_sent_as_bracketed_paste() {
        let bytes = interactive_paste_bytes("hello\nworld");
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(text, "\x1b[200~hello\nworld\x1b[201~\r");
    }

    #[test]
    fn retry_input_uses_plain_enter_for_short_single_line_prompts() {
        let bytes = interactive_retry_bytes("Hasilnya?");
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(text, "Hasilnya?\r");

        let bytes = interactive_retry_bytes("hello\nworld");
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(text, "\x1b[200~hello\nworld\x1b[201~\r");
    }

    #[tokio::test]
    async fn run_jsonl_writes_prompt_to_child_stdin() {
        let args = vec![
            "-c".to_string(),
            "IFS= read -r input; printf '{\"type\":\"ok\",\"text\":\"%s\"}\\n' \"$input\""
                .to_string(),
        ];
        let (tx, _rx) = mpsc::channel(8);
        let mut seen = String::new();

        run_jsonl(
            "/bin/sh",
            &args,
            "",
            Some("hello-stdin".to_string()),
            Duration::from_secs(5),
            &tx,
            |v, _sink| {
                seen = v["text"].as_str().unwrap_or("").to_string();
                true
            },
        )
        .await
        .unwrap();

        assert_eq!(seen, "hello-stdin");
    }

    #[tokio::test]
    async fn run_jsonl_times_out_silent_child() {
        let args = vec!["-c".to_string(), "sleep 5".to_string()];
        let (tx, mut rx) = mpsc::channel(8);

        let err = run_jsonl(
            "/bin/sh",
            &args,
            "",
            None,
            Duration::from_millis(100),
            &tx,
            |_v, _sink| true,
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(err.contains("timed out after 100 ms"));
        let Some(StreamItem::Notice(notice)) = rx.recv().await else {
            panic!("expected timeout notice");
        };
        assert!(notice.contains("timed out after 100 ms"));
        assert!(matches!(rx.recv().await, Some(StreamItem::Done)));
    }

    #[test]
    fn claude_cli_ignores_non_claude_models() {
        assert_eq!(claude_model_arg(""), None);
        assert_eq!(claude_model_arg("gpt-5.5"), None);
        assert_eq!(claude_model_arg("gpt-5.3-codex-spark"), None);
        assert_eq!(
            claude_model_arg("claude-sonnet-4-6"),
            Some("claude-sonnet-4-6")
        );
        assert_eq!(claude_model_arg("sonnet"), Some("sonnet"));
        assert_eq!(claude_model_arg("opus"), Some("opus"));
        assert_eq!(claude_model_arg("fable"), Some("fable"));
        assert_eq!(claude_model_arg("haiku"), Some("haiku"));
    }

    #[test]
    fn extract_cli_images_keeps_single_line_prompt_with_attachment() {
        // Real file: extract_cli_images only keeps markers whose path exists().
        let img =
            std::env::temp_dir().join(format!("oxide-cli-img-{}.png", new_claude_session_id()));
        std::fs::write(&img, b"\x89PNG").unwrap();

        // Mirrors the composer exactly: the "(user attached …)" note carries NO
        // trailing newline before the user's single-line prompt, then a \u{2}
        // image marker. The old newline-bounded strip ate the whole prompt here.
        let content = format!(
            "\n(user attached 1 image — image content is NOT visible to you; ask the user to describe it if needed)Cek struktur tim di schema\u{2}wsimg:{}",
            img.display(),
        );
        let req = TurnRequest {
            model: String::new(),
            reasoning_effort: String::new(),
            temperature: 0.0,
            messages: vec![Message::new(Role::User, content)],
            tools: Vec::new(),
            cwd: String::new(),
            conversation_id: String::new(),
            cli_resume: None,
        };

        let (prompt, images) = extract_cli_images(&req);
        let _ = std::fs::remove_file(&img);

        assert_eq!(prompt, "Cek struktur tim di schema");
        assert_eq!(images.len(), 1);
    }
}
