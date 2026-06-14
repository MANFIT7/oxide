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
use std::sync::{Mutex, OnceLock};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

/// CLI session ids per (binary, workspace) so consecutive turns RESUME the same
/// CLI conversation instead of starting amnesiac one-shots.
static SESSIONS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn session_key(bin: &str, conv: &str, cwd: &str) -> String {
    // Prefer the per-conversation id; fall back to cwd if absent.
    if conv.is_empty() { format!("{bin}|{cwd}") } else { format!("{bin}|{conv}") }
}

fn session_get(key: &str) -> Option<String> {
    SESSIONS.get_or_init(Default::default).lock().ok()?.get(key).cloned()
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
        if let Some(idx) = prompt.find("(user attached ") {
            let end = prompt[idx..].find('\n').map(|e| idx + e).unwrap_or(prompt.len());
            prompt.replace_range(idx..end, "");
        }
    }
    (prompt.trim().to_string(), imgs)
}

/// Spawn `program args...` and stream its stdout lines to `on_line`, closing
/// stdin so the CLI doesn't block waiting for piped input.
async fn run_jsonl<F>(
    program: &str,
    args: &[String],
    cwd: &str,
    sink: &mpsc::Sender<StreamItem>,
    mut on_line: F,
) -> anyhow::Result<()>
where
    F: FnMut(&Value, &mpsc::Sender<StreamItem>) -> bool,
{
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .stderr(Stdio::piped());
    // Run in the workspace — without this the CLI inherits the app's cwd
    // (Finder launches give `/`) and edits the wrong tree.
    if !cwd.is_empty() {
        cmd.current_dir(cwd);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn '{program}': {e}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("no stdout"))?;
    // Collect stderr in the background so failures aren't silent.
    let stderr = child.stderr.take();
    let err_task = tokio::spawn(async move {
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
    while let Some(line) = lines.next_line().await? {
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
    let status = child.wait().await;
    let failed = status.map(|st| !st.success()).unwrap_or(true);
    if failed {
        let tail = err_task.await.unwrap_or_default();
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
        let (prompt, images) = extract_cli_images(&req);
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
        // Pasted/attached images → native codex attachments (one -i per file).
        for img in &images {
            args.push("-i".to_string());
            args.push(img.clone());
        }
        args.push(prompt);

        let skey_cb = skey.clone();
        run_jsonl(&self.bin, &args, &req.cwd, &sink, move |v, sink| {
            match v["type"].as_str() {
                Some("item.started") => {
                    // Live status while the CLI runs a command/edits a file.
                    let item = &v["item"];
                    match item["type"].as_str() {
                        Some("command_execution") => {
                            let cmd = item["command"].as_str().unwrap_or("");
                            let cmd = cmd.strip_prefix("/bin/zsh -lc ").unwrap_or(cmd)
                                .strip_prefix("/bin/sh -c ").unwrap_or(cmd)
                                .trim_matches('\'');
                            let cmd: String = cmd.chars().take(80).collect();
                            send(sink, StreamItem::Notice(format!("⚙ Running {cmd}")));
                        }
                        Some("file_change") => {
                            let p = item["path"].as_str().or_else(|| item["text"].as_str()).unwrap_or("file");
                            send(sink, StreamItem::Notice(format!("⚙ Editing {p}")));
                        }
                        Some("web_search") => {
                            let q = item["query"].as_str().unwrap_or("");
                            send(sink, StreamItem::Notice(format!("⚙ Searching {q}")));
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
                                send(sink, StreamItem::TextDelta(t.to_string()));
                            }
                        }
                        Some("reasoning") => {
                            if let Some(t) = item["text"].as_str() {
                                send(sink, StreamItem::ReasoningDelta(t.to_string()));
                            }
                        }
                        Some("command_execution") => {
                            let cmd = item["command"].as_str().unwrap_or("");
                            let cmd = cmd.strip_prefix("/bin/zsh -lc ").unwrap_or(cmd)
                                .strip_prefix("/bin/sh -c ").unwrap_or(cmd)
                                .trim_matches('\'');
                            let exit = item["exit_code"].as_str().unwrap_or("");
                            let out = item["aggregated_output"].as_str().unwrap_or("");
                            let out: String = out.chars().take(800).collect();
                            send(sink, StreamItem::Notice(format!("⌘ {cmd}\n{out}").trim_end().to_string()));
                            let _ = exit;
                        }
                        Some("file_change") => {
                            let summary = item["text"].as_str()
                                .or_else(|| item["path"].as_str())
                                .unwrap_or("file change");
                            send(sink, StreamItem::Notice(format!("✎ {summary}")));
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
                            let q = item["query"].as_str().or_else(|| item["text"].as_str()).unwrap_or("");
                            send(sink, StreamItem::Notice(format!("🔎 {q}")));
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
        })
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
        // claude's OWN native session by that uuid → full context, no replay.
        // The persisted link (survives restarts) wins over the in-memory map.
        let resume = req.cli_resume.clone()
            .or_else(|| session_get(&skey))
            .or_else(|| req.conversation_id.strip_prefix("claude-").map(str::to_string));
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
        if !req.model.is_empty() {
            args.push("--model".to_string());
            args.push(req.model.clone());
        }
        if !req.reasoning_effort.is_empty() {
            args.push("--effort".to_string());
            args.push(claude_effort(&req.reasoning_effort).to_string());
        }

        let skey_cb = skey.clone();
        // With partial messages on, text arrives via stream_event deltas; the
        // final `assistant` message would duplicate it, so skip its text blocks.
        let mut saw_partial = false;
        run_jsonl(&self.bin, &args, &req.cwd, &sink, move |v, sink| {
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
                                    // "⚙ Read src/lib.rs", not a bare tool name.
                                    let input = &block["input"];
                                    let detail = ["file_path", "path", "command", "pattern", "query", "url", "description"]
                                        .iter()
                                        .find_map(|k| input[k].as_str())
                                        .unwrap_or("");
                                    let detail: String = detail.chars().take(80).collect();
                                    // A backgrounded command ("I'll let you know when done")
                                    // won't stream its result back — surface WHAT it's doing
                                    // with a distinct ⏳ marker so the UI can show it persistently.
                                    let bg = input["run_in_background"].as_bool() == Some(true);
                                    let label = if bg {
                                        if detail.is_empty() { format!("⏳ {name}") } else { format!("⏳ {name} {detail}") }
                                    } else if detail.is_empty() {
                                        format!("⚙ {name}")
                                    } else {
                                        format!("⚙ {name} {detail}")
                                    };
                                    send(sink, StreamItem::Notice(label));
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
                        },
                    );
                }
                _ => {}
            }
            true
        })
        .await
    }
}

fn codex_effort(effort: &str) -> &str {
    effort
}

fn claude_effort(effort: &str) -> &str {
    // claude --effort accepts low|medium|high|xhigh|max directly.
    effort
}
