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
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

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

/// Spawn `program args...` and stream its stdout lines to `on_line`, closing
/// stdin so the CLI doesn't block waiting for piped input.
async fn run_jsonl<F>(
    program: &str,
    args: &[String],
    sink: &mpsc::Sender<StreamItem>,
    mut on_line: F,
) -> anyhow::Result<()>
where
    F: FnMut(&Value, &mpsc::Sender<StreamItem>) -> bool,
{
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn '{program}': {e}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("no stdout"))?;
    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            // on_line returns false to stop early.
            if !on_line(&v, sink) {
                break;
            }
        }
    }
    let _ = child.wait().await;
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
        let prompt = last_user_prompt(&req);
        let mut args = vec![
            "exec".to_string(),
            "--json".to_string(),
            "--dangerously-bypass-approvals-and-sandbox".to_string(),
        ];
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
        args.push(prompt);

        run_jsonl(&self.bin, &args, &sink, |v, sink| {
            match v["type"].as_str() {
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
        let prompt = last_user_prompt(&req);
        let mut args = vec![
            "-p".to_string(),
            prompt,
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--verbose".to_string(),
            "--dangerously-skip-permissions".to_string(),
        ];
        if !req.model.is_empty() {
            args.push("--model".to_string());
            args.push(req.model.clone());
        }
        if !req.reasoning_effort.is_empty() {
            args.push("--effort".to_string());
            args.push(claude_effort(&req.reasoning_effort).to_string());
        }

        run_jsonl(&self.bin, &args, &sink, |v, sink| {
            match v["type"].as_str() {
                Some("assistant") => {
                    if let Some(content) = v["message"]["content"].as_array() {
                        for block in content {
                            match block["type"].as_str() {
                                Some("text") => {
                                    if let Some(t) = block["text"].as_str() {
                                        send(sink, StreamItem::TextDelta(t.to_string()));
                                    }
                                }
                                Some("tool_use") => {
                                    let name = block["name"].as_str().unwrap_or("tool");
                                    send(sink, StreamItem::Notice(format!("⚙ {name}")));
                                }
                                _ => {}
                            }
                        }
                    }
                }
                Some("result") => {
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
    match effort {
        "xhigh" => "max",
        other => other,
    }
}
