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
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
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

const DEFAULT_CLI_TURN_TIMEOUT: Duration = Duration::from_secs(45 * 60);

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

/// A claude `tool_result` content is either a plain string or an array of
/// content blocks ({type:"text", text:"…"}); flatten either to a string.
fn tool_result_text(v: &serde_json::Value) -> String {
    if let Some(s) = v.as_str() {
        return s.to_string();
    }
    if let Some(arr) = v.as_array() {
        let mut out = String::new();
        for b in arr {
            if let Some(t) = b["text"].as_str() {
                out.push_str(t);
            }
        }
        return out;
    }
    String::new()
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
        // agent text and flush it at turn.completed / error — after every live
        // command/edit/search row — so the transcript reads command → answer, never
        // the reverse. CRUCIAL: the flush happens INSIDE the stream, before
        // run_jsonl emits its terminal StreamItem::Done. The engine consumer stops
        // reading at Done, so text emitted AFTER the run is dropped and the answer
        // would vanish entirely. (claude_interactive solves the same ordering with
        // per-block transcript positions; codex's JSONL carries none, so we buffer
        // and flush at the turn boundary instead.)
        let text_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        // Has the model acted (run a command / edited / searched) yet this turn?
        // An agent_message BEFORE any action is a preamble ("First I'll check…")
        // and must stay inline, ABOVE the command rows; one AFTER an action is the
        // answer and is buffered to land below them. (Buffering everything put the
        // preamble below the command too.) Emitting inline is also safe vs the
        // Done-ordering trap — it streams mid-turn, well before run_jsonl's Done.
        let mut acted = false;
        run_jsonl(
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
                        if matches!(
                            item["type"].as_str(),
                            Some("command_execution" | "file_change" | "web_search")
                        ) {
                            acted = true;
                        }
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
                                    if acted {
                                        // Post-action text = the answer. Hold until
                                        // turn.completed so it lands below the command/
                                        // activity rows (see text_buf above).
                                        if let Ok(mut buf) = text_buf.lock() {
                                            buf.push(t.to_string());
                                        }
                                    } else {
                                        // Preamble before any action — stream inline so
                                        // it stays above the command rows it precedes.
                                        send(sink, StreamItem::TextDelta(t.to_string()));
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
                        // Flush the buffered answer now — after all command/edit rows
                        // and before run_jsonl's terminal Done (see text_buf above).
                        if let Ok(mut buf) = text_buf.lock() {
                            for t in buf.drain(..) {
                                send(sink, StreamItem::TextDelta(t));
                            }
                        }
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
                        // Preserve any partial answer captured before the error, again
                        // before run_jsonl's Done (see text_buf above).
                        if let Ok(mut buf) = text_buf.lock() {
                            for t in buf.drain(..) {
                                send(sink, StreamItem::TextDelta(t));
                            }
                        }
                        let msg = v["message"].as_str().unwrap_or("codex error");
                        send(sink, StreamItem::Notice(format!("error: {msg}")));
                    }
                    _ => {}
                }
                true
            },
        )
        .await
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
        // Harness persona/policy layered onto Claude Code's own base prompt — the
        // CLI analog of a Managed-Agents `system` override. Append, never replace:
        // Claude Code's tuned base prompt + self-driven workspace gathering stay
        // intact. Empty/absent for every harness that doesn't opt in.
        if let Some(extra) = req
            .system_append
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            args.push("--append-system-prompt".to_string());
            args.push(extra.to_string());
        }

        let timeout = claude_cli_timeout()?;
        // Shared per-line mapping (also used by the persistent driver) so the two
        // can't drift; `false` = don't suppress the result's error notice here.
        let mut st = ClaudeLineState::new(skey.clone());
        run_jsonl(
            &self.bin,
            &args,
            &req.cwd,
            None,
            timeout,
            &sink,
            move |v, sink| {
                claude_handle_line(v, sink, &mut st, false);
                true
            },
        )
        .await
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Persistent stream-json claude driver (`--input-format stream-json`).
//
// One long-lived `claude` process per conversation, fed user turns over stdin
// as JSONL and read over stdout. Each turn ends on the child's `result` event;
// the process stays warm so context lives in-process (no per-turn respawn or
// `--resume` reload), and the stdin steer channel is the substrate for true
// mid-turn steering (a future engine hook calls `claude_persistent_steer`).
//
// Gated behind OXIDE_CLAUDE_PERSISTENT; the proven one-shot ClaudeCliProvider
// stays the default. The line mapping below MIRRORS the ClaudeCliProvider
// closure — keep the two in sync. NEEDS LIVE TEST.
// ─────────────────────────────────────────────────────────────────────────

/// Tracks background commands so their output reaches the command's row instead
/// of being dropped. A background `Bash` returns an output-file path in its
/// tool_result; claude then polls that file with `Read` — both are routed here.
#[derive(Default)]
struct BgTracker {
    /// Background-command tool_use id → its command string, awaiting the
    /// "written to <path>" result (the command also yields redirect targets).
    started: HashMap<String, String>,
    /// Output file path → the background command's row id.
    files: HashMap<String, String>,
    /// A `Read` tool_use id polling a bg file → that command's row id.
    reads: HashMap<String, String>,
    /// Command row id → lines already forwarded (each poll appends only new ones).
    shown: HashMap<String, usize>,
}

/// Shell redirect targets (`> file`, `>> file`) in a command — so a background
/// command that writes to its own file (`… > out.log`) can still have that
/// file's growth routed to its row. Skips fd redirects like `2>&1`.
fn parse_redirect_targets(cmd: &str) -> Vec<String> {
    let toks: Vec<&str> = cmd.split_whitespace().collect();
    let mut out = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        let path = if *t == ">" || *t == ">>" {
            toks.get(i + 1).copied()
        } else if let Some(p) = t.strip_prefix(">>").or_else(|| t.strip_prefix('>')) {
            Some(p).filter(|p| !p.is_empty())
        } else {
            None
        };
        if let Some(p) = path {
            let p = p.trim_matches(['"', '\'']);
            if !p.is_empty() && !p.starts_with('&') {
                out.push(p.to_string());
            }
        }
    }
    out
}

/// Route a tool_result belonging to a background command — the initial "running
/// in background… written to <path>" line, or a `Read` poll of that file — into
/// the command row's live output. Returns true if it consumed the result (the
/// caller then skips normal handling).
fn bg_on_tool_result(
    bg: &mut BgTracker,
    tool_use_id: &str,
    content: &Value,
    sink: &mpsc::Sender<StreamItem>,
) -> bool {
    // The background Bash result carries the output file path; the command may
    // also redirect to its own file. Track both so a later `Read` of either one
    // routes here. Wording is "…written to: /path" (colon), and varies.
    if let Some(cmd) = bg.started.remove(tool_use_id) {
        let text = tool_result_text(content);
        if let Some(path) = text
            .split("written to")
            .nth(1)
            .map(|s| s.trim_start_matches([':', ' ', '\t']))
            .and_then(|s| s.split_whitespace().next())
            .map(|s| s.trim_end_matches('.').to_string())
            .filter(|s| !s.is_empty())
        {
            bg.files.insert(path, tool_use_id.to_string());
        }
        for target in parse_redirect_targets(&cmd) {
            bg.files.insert(target, tool_use_id.to_string());
        }
        send(
            sink,
            StreamItem::CommandOutput {
                id: tool_use_id.to_string(),
                stream: "stdout".to_string(),
                chunk: text.lines().next().unwrap_or("").to_string(),
            },
        );
        return true;
    }
    // A `Read` poll of a bg file → forward only the newly-appended lines.
    if let Some(cmd_id) = bg.reads.remove(tool_use_id) {
        let text = tool_result_text(content);
        // Strip the Read tool's "<n>\t" line-number prefix.
        let lines: Vec<&str> = text
            .lines()
            .map(|l| l.split_once('\t').map(|(_, r)| r).unwrap_or(l))
            .collect();
        let shown = bg.shown.get(&cmd_id).copied().unwrap_or(0);
        if lines.len() > shown {
            let chunk = lines[shown..].join("\n");
            send(
                sink,
                StreamItem::CommandOutput {
                    id: cmd_id.clone(),
                    stream: "stdout".to_string(),
                    chunk: chunk.chars().take(4000).collect(),
                },
            );
            bg.shown.insert(cmd_id, lines.len());
        }
        return true;
    }
    false
}

/// Per-turn state for the claude stream-json line mapping.
struct ClaudeLineState {
    /// Once partial deltas streamed, the final `assistant` text duplicates them.
    saw_partial: bool,
    /// tool_use ids surfaced as command rows, finished by the later tool_result.
    command_ids: std::collections::HashSet<String>,
    /// session-map key for persisting the native session id.
    skey: String,
    /// Background-command output routing.
    bg: BgTracker,
}

impl ClaudeLineState {
    fn new(skey: String) -> Self {
        Self {
            saw_partial: false,
            command_ids: std::collections::HashSet::new(),
            skey,
            bg: BgTracker::default(),
        }
    }
}

/// Map one claude stream-json line to StreamItems. Returns true when the line is
/// a turn-terminating `result` event (the persistent driver ends the turn on
/// it; a one-shot run would just read on to process exit). Mirrors the
/// ClaudeCliProvider closure body — keep them in sync.
fn claude_handle_line(
    v: &Value,
    sink: &mpsc::Sender<StreamItem>,
    st: &mut ClaudeLineState,
    suppress_result_error: bool,
) -> bool {
    match v["type"].as_str() {
        Some("system") => {
            if let Some(id) = v["session_id"].as_str() {
                session_set(&st.skey, id);
                send(sink, StreamItem::CliSession(id.to_string()));
            }
        }
        Some("stream_event") => {
            let ev = &v["event"];
            if ev["type"].as_str() == Some("message_start") {
                st.saw_partial = false;
            }
            if ev["type"].as_str() == Some("content_block_delta") {
                match ev["delta"]["type"].as_str() {
                    Some("text_delta") => {
                        if let Some(t) = ev["delta"]["text"].as_str() {
                            st.saw_partial = true;
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
                            if !st.saw_partial {
                                if let Some(t) = block["text"].as_str() {
                                    send(sink, StreamItem::TextDelta(t.to_string()));
                                }
                            }
                        }
                        Some("tool_use") => {
                            let name = block["name"].as_str().unwrap_or("tool");
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
                            let bg = input["run_in_background"].as_bool() == Some(true);
                            let is_command = matches!(name, "Bash" | "Shell")
                                || input["command"].as_str().is_some();
                            if is_command {
                                let command = input["command"]
                                    .as_str()
                                    .unwrap_or(detail.as_str())
                                    .to_string();
                                let id = block["id"]
                                    .as_str()
                                    .map(str::to_string)
                                    .unwrap_or_else(|| format!("claude-command-{command}"));
                                if !bg {
                                    st.command_ids.insert(id.clone());
                                } else {
                                    // Await this bg command's output-file path (+
                                    // any redirect target) to route its output.
                                    st.bg.started.insert(id.clone(), command.clone());
                                }
                                send(
                                    sink,
                                    StreamItem::CommandStarted {
                                        id,
                                        command,
                                        cwd: String::new(),
                                        background: bg,
                                    },
                                );
                            } else if name == "Read"
                                && input["file_path"]
                                    .as_str()
                                    .map(|p| st.bg.files.contains_key(p))
                                    .unwrap_or(false)
                            {
                                // A poll of a background command's output file:
                                // route its result to the command row, no notice.
                                if let (Some(p), Some(tid)) =
                                    (input["file_path"].as_str(), block["id"].as_str())
                                {
                                    if let Some(cmd) = st.bg.files.get(p).cloned() {
                                        st.bg.reads.insert(tid.to_string(), cmd);
                                    }
                                }
                            } else {
                                let label = if detail.is_empty() {
                                    format!("{} {name}", '\u{2699}')
                                } else {
                                    format!("{} {name} {detail}", '\u{2699}')
                                };
                                send(sink, StreamItem::Notice(label));
                            }
                            if matches!(name, "Edit" | "Write" | "MultiEdit" | "NotebookEdit") {
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
        Some("user") => {
            if let Some(content) = v["message"]["content"].as_array() {
                for block in content {
                    if block["type"].as_str() != Some("tool_result") {
                        continue;
                    }
                    // Background-command output (initial path line + `Read` polls)
                    // routes to the command row before the normal command handling.
                    if let Some(rid) = block["tool_use_id"].as_str() {
                        if bg_on_tool_result(&mut st.bg, rid, &block["content"], sink) {
                            continue;
                        }
                    }
                    let id = match block["tool_use_id"].as_str() {
                        Some(id) if st.command_ids.remove(id) => id.to_string(),
                        _ => continue,
                    };
                    let out = tool_result_text(&block["content"]);
                    if !out.is_empty() {
                        send(
                            sink,
                            StreamItem::CommandOutput {
                                id: id.clone(),
                                stream: "stdout".to_string(),
                                chunk: out.chars().take(4000).collect(),
                            },
                        );
                    }
                    let ok = block["is_error"].as_bool() != Some(true);
                    send(
                        sink,
                        StreamItem::CommandFinished {
                            id,
                            ok,
                            exit_code: if ok { None } else { Some(1) },
                            duration_ms: 0,
                        },
                    );
                }
            }
        }
        Some("result") => {
            if let Some(id) = v["session_id"].as_str() {
                session_set(&st.skey, id);
                send(sink, StreamItem::CliSession(id.to_string()));
            }
            if !suppress_result_error && v["is_error"].as_bool() == Some(true) {
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
                    reasoning_output: u["reasoning_output_tokens"].as_u64().unwrap_or(0),
                },
            );
        }
        _ => {}
    }
    v["type"].as_str() == Some("result")
}

type PersistentReader = tokio::io::Lines<BufReader<tokio::process::ChildStdout>>;

struct PersistentChild {
    /// Held only so the child is killed when the entry is dropped (kill_on_drop).
    _child: tokio::process::Child,
    stdin_tx: mpsc::UnboundedSender<String>,
    reader: PersistentReader,
    /// Set by `claude_persistent_interrupt`; the read loop consumes it on the
    /// next `result` so a steer-triggered abort isn't shown as a turn error.
    interrupt: Arc<AtomicBool>,
}

#[allow(clippy::type_complexity)]
static PERSISTENT: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<PersistentChild>>>>> =
    OnceLock::new();
/// Per-conversation stdin sender + an interrupt flag the persistent read loop
/// checks so a steer-triggered abort isn't surfaced as a turn error.
struct SteerHandle {
    stdin_tx: mpsc::UnboundedSender<String>,
    interrupt: Arc<AtomicBool>,
}

static STEER_TX: OnceLock<Mutex<HashMap<String, SteerHandle>>> = OnceLock::new();

#[allow(clippy::type_complexity)]
fn persistent_map() -> &'static Mutex<HashMap<String, Arc<tokio::sync::Mutex<PersistentChild>>>> {
    PERSISTENT.get_or_init(Default::default)
}

fn steer_map() -> &'static Mutex<HashMap<String, SteerHandle>> {
    STEER_TX.get_or_init(Default::default)
}

/// Build the JSONL user-message line `claude --input-format stream-json` reads
/// on stdin.
fn claude_user_line(prompt: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": [ { "type": "text", "text": prompt } ] }
    })
    .to_string()
}

/// The stream-json control message that aborts the in-flight turn. claude
/// replies with a `control_response`; the abort then surfaces as a `result`
/// event (`is_error: true`, `subtype: "error_during_execution"`).
fn claude_interrupt_line() -> String {
    serde_json::json!({
        "type": "control_request",
        "request_id": "oxide-interrupt",
        "request": { "subtype": "interrupt" }
    })
    .to_string()
}

/// Abort the in-flight turn of a live persistent claude process so a mid-turn
/// steer redirects it immediately instead of waiting for the current answer to
/// finish. The steer text itself still flows through the engine's normal
/// next-round path; this only interrupts. Returns false when no persistent child
/// is running for the conversation (every other provider / the one-shot driver),
/// where the caller's next-round steering already applies on its own.
pub fn claude_persistent_interrupt(conversation_id: &str, cwd: &str) -> bool {
    let bin = resolve_bin("claude", "OXIDE_CLAUDE_BIN");
    let key = session_key(&bin, conversation_id, cwd);
    let handle = steer_map().lock().ok().and_then(|m| {
        m.get(&key)
            .map(|h| (h.stdin_tx.clone(), h.interrupt.clone()))
    });
    match handle {
        Some((tx, interrupt)) => {
            // Mark BEFORE sending so the read loop knows the next `result` is the
            // abort and doesn't surface it as a turn error.
            interrupt.store(true, Ordering::SeqCst);
            if tx.send(claude_interrupt_line()).is_ok() {
                true
            } else {
                interrupt.store(false, Ordering::SeqCst);
                false
            }
        }
        None => false,
    }
}

fn remove_persistent(key: &str) {
    if let Ok(mut m) = persistent_map().lock() {
        m.remove(key);
    }
    if let Ok(mut m) = steer_map().lock() {
        m.remove(key);
    }
}

fn spawn_persistent(
    bin: &str,
    args: &[String],
    cwd: &str,
    key: &str,
) -> anyhow::Result<Arc<tokio::sync::Mutex<PersistentChild>>> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if !cwd.is_empty() {
        cmd.current_dir(cwd);
    }
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn '{bin}': {e}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("no stdin for '{bin}'"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("no stdout for '{bin}'"))?;
    // Drain stderr so a wedged child can't block on a full pipe.
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(_)) = lines.next_line().await {}
        });
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            if stdin.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            if stdin.write_all(b"\n").await.is_err() {
                break;
            }
            let _ = stdin.flush().await;
        }
    });
    let interrupt = Arc::new(AtomicBool::new(false));
    let entry = Arc::new(tokio::sync::Mutex::new(PersistentChild {
        _child: child,
        stdin_tx: tx.clone(),
        reader: BufReader::new(stdout).lines(),
        interrupt: interrupt.clone(),
    }));
    if let Ok(mut m) = persistent_map().lock() {
        m.insert(key.to_string(), entry.clone());
    }
    if let Ok(mut m) = steer_map().lock() {
        m.insert(
            key.to_string(),
            SteerHandle {
                stdin_tx: tx,
                interrupt,
            },
        );
    }
    Ok(entry)
}

/// Persistent variant of [`ClaudeCliProvider`]: holds one long-lived
/// `claude --print --input-format stream-json --output-format stream-json`
/// process per conversation. Selected when OXIDE_CLAUDE_PERSISTENT is set.
pub struct ClaudePersistentProvider {
    bin: String,
}

impl ClaudePersistentProvider {
    pub fn new() -> Self {
        Self {
            bin: resolve_bin("claude", "OXIDE_CLAUDE_BIN"),
        }
    }
}

impl Default for ClaudePersistentProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for ClaudePersistentProvider {
    fn name(&self) -> &str {
        // Same name as the one-shot driver so the engine treats it as a CLI
        // driver (self-agentic, next-round steering) identically.
        "claude"
    }

    async fn stream(&self, req: TurnRequest, sink: mpsc::Sender<StreamItem>) -> anyhow::Result<()> {
        let (mut prompt, images) = extract_cli_images(&req);
        if !images.is_empty() {
            prompt.push_str("\n\nAttached image file(s) — use your Read tool to view:\n");
            for img in &images {
                prompt.push_str(&format!("- {img}\n"));
            }
        }
        if prompt.trim().is_empty() {
            prompt = "Continue.".to_string();
        }
        let skey = session_key(&self.bin, &req.conversation_id, &req.cwd);

        // Get-or-spawn the long-lived child for this conversation.
        let existing = persistent_map()
            .lock()
            .ok()
            .and_then(|m| m.get(&skey).cloned());
        let entry = match existing {
            Some(e) => e,
            None => {
                let mut args = vec![
                    "--print".to_string(),
                    "--input-format".to_string(),
                    "stream-json".to_string(),
                    "--output-format".to_string(),
                    "stream-json".to_string(),
                    "--verbose".to_string(),
                    "--include-partial-messages".to_string(),
                    "--dangerously-skip-permissions".to_string(),
                ];
                if let Some(model) = claude_model_arg(&req.model) {
                    args.push("--model".to_string());
                    args.push(model.to_string());
                }
                if !req.reasoning_effort.is_empty() {
                    args.push("--effort".to_string());
                    args.push(claude_effort(&req.reasoning_effort).to_string());
                }
                if let Some(extra) = req
                    .system_append
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    args.push("--append-system-prompt".to_string());
                    args.push(extra.to_string());
                }
                spawn_persistent(&self.bin, &args, &req.cwd, &skey)?
            }
        };

        let timeout = claude_cli_timeout()?;
        let mut st = ClaudeLineState::new(skey.clone());
        let mut guard = entry.lock().await;
        // Fresh turn: clear any stale interrupt flag from a boundary-race steer.
        guard.interrupt.store(false, Ordering::SeqCst);

        // Feed this turn's user message; a closed channel means the child died.
        if guard.stdin_tx.send(claude_user_line(&prompt)).is_err() {
            drop(guard);
            remove_persistent(&skey);
            // Fall back to a fresh one-shot turn so the user still gets a reply.
            return ClaudeCliProvider::new().stream(req, sink).await;
        }

        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match tokio::time::timeout_at(deadline, guard.reader.next_line()).await {
                Ok(Ok(Some(line))) => {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    if let Ok(v) = serde_json::from_str::<Value>(line) {
                        // Consume the interrupt flag on the turn-ending `result`
                        // so a steer-triggered abort is not surfaced as an error.
                        let suppress = v["type"].as_str() == Some("result")
                            && guard.interrupt.swap(false, Ordering::SeqCst);
                        if claude_handle_line(&v, &sink, &mut st, suppress) {
                            break; // `result` → this turn is done; keep the child warm
                        }
                    }
                }
                Ok(Ok(None)) => {
                    drop(guard);
                    remove_persistent(&skey);
                    let _ = sink
                        .send(StreamItem::Notice(
                            "error: claude persistent process exited".to_string(),
                        ))
                        .await;
                    let _ = sink.send(StreamItem::Done).await;
                    return Ok(());
                }
                Ok(Err(e)) => {
                    drop(guard);
                    remove_persistent(&skey);
                    let _ = sink
                        .send(StreamItem::Notice(format!(
                            "error: claude read failed: {e}"
                        )))
                        .await;
                    let _ = sink.send(StreamItem::Done).await;
                    return Ok(());
                }
                Err(_) => {
                    drop(guard);
                    remove_persistent(&skey);
                    let _ = sink
                        .send(StreamItem::Notice(format!(
                            "error: claude timed out after {}",
                            format_timeout(timeout)
                        )))
                        .await;
                    let _ = sink.send(StreamItem::Done).await;
                    return Ok(());
                }
            }
        }
        drop(guard);
        let _ = sink.send(StreamItem::Done).await;
        Ok(())
    }
}

/// Drives interactive Claude Code through a PTY while preserving Oxide's chat UI.
///
/// This intentionally avoids `claude -p`: Claude runs as a normal interactive
/// TTY session, receives the prompt through bracketed paste, and Oxide follows
/// Claude Code's native JSONL transcript for the assistant text/tool events.
pub struct ClaudeInteractiveProvider;

impl ClaudeInteractiveProvider {
    pub fn new() -> Self {
        Self
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
        // The PTY + JSONL-transcript-scrape interactive driver scrambled the
        // transcript on any degraded turn: its error path dumped the raw TUI
        // framebuffer, which strip_ansi can't linearize (no cursor/erase model),
        // so each redraw appended into a jumble of meta-narration + chopped
        // answer. Route through the clean headless stream-json provider instead —
        // text and tool rows arrive already correctly ordered, with no scrape.
        // Mid-run steering returns when the persistent stream-json driver
        // (--input-format stream-json) lands.
        ClaudeCliProvider::new().stream(req, sink).await
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
mod cli_driver_tests {
    use super::*;
    use crate::Message;

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
        let img = std::env::temp_dir().join(format!("oxide-cli-img-{}.png", std::process::id()));
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
            system_append: None,
        };

        let (prompt, images) = extract_cli_images(&req);
        let _ = std::fs::remove_file(&img);

        assert_eq!(prompt, "Cek struktur tim di schema");
        assert_eq!(images.len(), 1);
    }

    #[tokio::test]
    async fn run_jsonl_emits_done_last_after_all_content() {
        // The engine consumer stops reading at StreamItem::Done — anything sent
        // after it is dropped (the v0.0.107 regression class). Lock the invariant
        // run_jsonl guarantees: every item the closure emits lands BEFORE the one
        // terminal Done, and Done is strictly last.
        let args = vec![
            "-c".to_string(),
            r#"printf '{"t":"a"}\n{"t":"b"}\n'"#.to_string(),
        ];
        let (tx, mut rx) = mpsc::channel(16);
        run_jsonl(
            "/bin/sh",
            &args,
            "",
            None,
            Duration::from_secs(5),
            &tx,
            |v, sink| {
                if let Some(t) = v["t"].as_str() {
                    send(sink, StreamItem::TextDelta(t.to_string()));
                }
                true
            },
        )
        .await
        .unwrap();
        drop(tx);

        let mut items = Vec::new();
        while let Some(it) = rx.recv().await {
            items.push(it);
        }
        assert!(
            matches!(items.last(), Some(StreamItem::Done)),
            "Done must be the final item: {items:?}"
        );
        assert_eq!(
            items
                .iter()
                .filter(|i| matches!(i, StreamItem::Done))
                .count(),
            1,
            "exactly one Done"
        );
        let texts: Vec<&str> = items
            .iter()
            .filter_map(|i| match i {
                StreamItem::TextDelta(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["a", "b"], "all content before Done, in order");
    }

    #[test]
    fn claude_user_line_is_valid_single_line_stream_json() {
        // The persistent driver writes one user message per NDJSON line; a prompt
        // with quotes/newlines must serialize without breaking the framing.
        let line = claude_user_line("hi \"there\"\nsecond line");
        assert!(!line.contains('\n'), "embedded newline would split NDJSON");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["role"], "user");
        assert_eq!(v["message"]["content"][0]["type"], "text");
        assert_eq!(
            v["message"]["content"][0]["text"],
            "hi \"there\"\nsecond line"
        );
    }

    #[test]
    fn claude_persistent_interrupt_false_without_child() {
        // No process running for this conversation → nothing to interrupt; caller
        // falls back to engine next-round steering.
        assert!(!claude_persistent_interrupt(
            "no-such-conversation",
            "/tmp/nowhere"
        ));
    }

    // Live integration smoke test for the persistent driver: two turns must run
    // through ONE warm `claude` process (proving stdin-feed + read-to-`result` +
    // per-turn Done + process reuse). Ignored by default — spawns a real claude
    // and makes two subscription calls. Run with:
    //   cargo test -p oxide-providers -- --ignored persistent_driver
    #[tokio::test]
    #[ignore = "spawns a real `claude` process and makes 2 API calls"]
    async fn persistent_driver_two_turns_one_process() {
        let provider = ClaudePersistentProvider::new();
        let conv = format!("persist-smoke-{}", std::process::id());
        for word in ["ONE", "TWO"] {
            let (tx, mut rx) = mpsc::channel::<StreamItem>(256);
            let reader = tokio::spawn(async move {
                let (mut text, mut done) = (String::new(), false);
                while let Some(it) = rx.recv().await {
                    match it {
                        StreamItem::TextDelta(t) => text.push_str(&t),
                        StreamItem::Done => {
                            done = true;
                            break;
                        }
                        _ => {}
                    }
                }
                (text, done)
            });
            let req = TurnRequest {
                model: String::new(),
                reasoning_effort: String::new(),
                temperature: 0.0,
                messages: vec![Message::new(
                    Role::User,
                    format!("reply with exactly the word {word}, nothing else"),
                )],
                tools: Vec::new(),
                cwd: String::new(),
                conversation_id: conv.clone(),
                cli_resume: None,
                system_append: None,
            };
            provider.stream(req, tx).await.unwrap();
            let (text, done) = reader.await.unwrap();
            assert!(done, "turn must end with Done");
            assert!(
                text.to_uppercase().contains(word),
                "turn {word}: got {text:?}"
            );
        }
        // After two turns the conversation still has exactly one warm child.
        let live = persistent_map()
            .lock()
            .unwrap()
            .keys()
            .filter(|k| k.contains(&conv))
            .count();
        assert_eq!(live, 1, "two turns share one persistent process");
        remove_persistent(&session_key(&provider.bin, &conv, ""));
    }

    // Live test for mid-turn interrupt: a long turn aborted via
    // claude_persistent_interrupt must end with Done and NO error notice (the
    // suppress flag swallows the interrupt's error_during_execution result).
    // Ignored by default — spawns a real claude. Run with:
    //   cargo test -p oxide-providers -- --ignored persistent_driver_interrupt
    #[tokio::test]
    #[ignore = "spawns a real `claude` process and makes 1 API call"]
    async fn persistent_driver_interrupt_aborts_turn() {
        let conv = format!("persist-int-{}", std::process::id());
        let (tx, mut rx) = mpsc::channel::<StreamItem>(512);
        let conv_turn = conv.clone();
        let turn = tokio::spawn(async move {
            let provider = ClaudePersistentProvider::new();
            let req = TurnRequest {
                model: String::new(),
                reasoning_effort: String::new(),
                temperature: 0.0,
                messages: vec![Message::new(
                    Role::User,
                    "Write the numbers 1 through 40, each on its own line as \
                     'N: <one short factual sentence>'. Go slowly and thoroughly."
                        .to_string(),
                )],
                tools: Vec::new(),
                cwd: String::new(),
                conversation_id: conv_turn,
                cli_resume: None,
                system_append: None,
            };
            provider.stream(req, tx).await.unwrap();
        });

        // Let the turn start generating, then interrupt mid-stream.
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(
            claude_persistent_interrupt(&conv, ""),
            "interrupt should reach the live child"
        );

        let mut got_done = false;
        let mut saw_error = false;
        while let Some(it) = rx.recv().await {
            match it {
                StreamItem::Notice(n) if n.starts_with("error:") => saw_error = true,
                StreamItem::Done => {
                    got_done = true;
                    break;
                }
                _ => {}
            }
        }
        turn.await.unwrap();
        assert!(got_done, "interrupted turn must still end with Done");
        assert!(!saw_error, "interrupt must not surface a turn error");
        remove_persistent(&session_key(
            &ClaudePersistentProvider::new().bin,
            &conv,
            "",
        ));
    }

    #[tokio::test]
    async fn bg_routing_parses_path_then_streams_new_read_lines() {
        let (tx, mut rx) = mpsc::channel::<StreamItem>(64);
        let mut bg = BgTracker::default();

        // Background Bash result → extract the wrapper path (colon wording) AND
        // the command's redirect target.
        bg.started.insert(
            "toolu_bash".to_string(),
            "for i in 1 2 3; do echo tick-$i; done > /tmp/redir.log 2>&1".to_string(),
        );
        let bash_res = serde_json::json!(
            "Command running in background with ID: byz85bp8o. \
             Output is being written to: /tmp/bg-out-123.log"
        );
        assert!(bg_on_tool_result(&mut bg, "toolu_bash", &bash_res, &tx));
        assert_eq!(
            bg.files.get("/tmp/bg-out-123.log").map(String::as_str),
            Some("toolu_bash"),
            "wrapper path tracked"
        );
        assert_eq!(
            bg.files.get("/tmp/redir.log").map(String::as_str),
            Some("toolu_bash"),
            "redirect target tracked"
        );

        // First Read poll → forward all lines (line-number prefix stripped).
        bg.reads
            .insert("toolu_r1".to_string(), "toolu_bash".to_string());
        let read1 = serde_json::json!("1\ttick-1\n2\ttick-2\n3\ttick-3");
        assert!(bg_on_tool_result(&mut bg, "toolu_r1", &read1, &tx));

        // Second poll → only the newly-appended lines.
        bg.reads
            .insert("toolu_r2".to_string(), "toolu_bash".to_string());
        let read2 = serde_json::json!("1\ttick-1\n2\ttick-2\n3\ttick-3\n4\ttick-4\n5\ttick-5");
        assert!(bg_on_tool_result(&mut bg, "toolu_r2", &read2, &tx));

        // Unrelated result → not consumed.
        assert!(!bg_on_tool_result(
            &mut bg,
            "toolu_other",
            &serde_json::json!("x"),
            &tx
        ));

        drop(tx);
        let mut outs = Vec::new();
        while let Some(it) = rx.recv().await {
            if let StreamItem::CommandOutput { id, chunk, .. } = it {
                outs.push((id, chunk));
            }
        }
        assert_eq!(outs.len(), 3, "path line + two poll appends: {outs:?}");
        assert_eq!(outs[0].0, "toolu_bash");
        assert!(outs[0].1.contains("running in background"));
        assert_eq!(
            outs[1],
            (
                "toolu_bash".to_string(),
                "tick-1\ntick-2\ntick-3".to_string()
            )
        );
        assert_eq!(
            outs[2],
            ("toolu_bash".to_string(), "tick-4\ntick-5".to_string())
        );
    }

    // Live end-to-end: a real background command's output must reach a
    // CommandOutput row (validates the tool_use hooks + tool_result routing).
    // Ignored — spawns a real claude. Run with:
    //   cargo test -p oxide-providers -- --ignored persistent_driver_streams_background
    #[tokio::test]
    #[ignore = "spawns a real `claude` process and makes API calls"]
    async fn persistent_driver_streams_background_output() {
        let conv = format!("persist-bg-{}", std::process::id());
        let (tx, mut rx) = mpsc::channel::<StreamItem>(1024);
        let conv_turn = conv.clone();
        let turn = tokio::spawn(async move {
            let provider = ClaudePersistentProvider::new();
            let req = TurnRequest {
                model: String::new(),
                reasoning_effort: String::new(),
                temperature: 0.0,
                messages: vec![Message::new(
                    Role::User,
                    "Run this command in the background: \
                     'for i in 1 2 3; do echo tick-$i; sleep 1; done'. \
                     Then poll its output file until it finishes and report what it printed."
                        .to_string(),
                )],
                tools: Vec::new(),
                cwd: String::new(),
                conversation_id: conv_turn,
                cli_resume: None,
                system_append: None,
            };
            provider.stream(req, tx).await.unwrap();
        });

        let mut got_bg_start = false;
        let mut got_tick = false;
        while let Some(it) = rx.recv().await {
            match it {
                StreamItem::CommandOutput { chunk, .. } => {
                    if chunk.contains("running in background") {
                        got_bg_start = true;
                    }
                    if chunk.contains("tick-") {
                        got_tick = true;
                    }
                }
                StreamItem::Done => break,
                _ => {}
            }
        }
        turn.await.unwrap();
        // Reliable: the bg command's start line always routes to its row.
        assert!(
            got_bg_start,
            "background command's start line must route to a CommandOutput row"
        );
        // Best-effort: tick output routes when claude reads a tracked file.
        eprintln!("bg tick output routed to the command row: {got_tick}");
        remove_persistent(&session_key(
            &ClaudePersistentProvider::new().bin,
            &conv,
            "",
        ));
    }
}
