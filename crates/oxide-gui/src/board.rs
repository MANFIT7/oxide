//! Kanban board — cards the AI auto-pulls, runs in isolated git worktrees, and
//! moves through To Do → In Progress → Done with results attached. Inspired by
//! openai/symphony, but local-first and built on the Oxide engine.

use oxide_config::Config;
use oxide_protocol::{ApprovalPolicy, Event, Op};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const TODO: &str = "todo";
pub const DOING: &str = "doing";
pub const REVIEW: &str = "review";
pub const DONE: &str = "done";

/// Merge `branch` into the current branch of `root`. Returns the git output.
pub async fn merge_branch(root: &Path, branch: &str) -> (bool, String) {
    let out = tokio::process::Command::new("git")
        .args(["merge", "--no-ff", "-m", &format!("merge {branch}"), branch])
        .current_dir(root)
        .output()
        .await;
    match out {
        Ok(o) => {
            let ok = o.status.success();
            let msg = if ok {
                "merged".to_string()
            } else {
                String::from_utf8_lossy(&o.stderr).trim().to_string()
            };
            (ok, msg)
        }
        Err(e) => (false, format!("git error: {e}")),
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Card {
    pub id: u64,
    pub title: String,
    #[serde(default)]
    pub desc: String,
    pub column: String,
    #[serde(default)]
    pub result: String,
    #[serde(default)]
    pub branch: String,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Board {
    #[serde(default)]
    pub cards: Vec<Card>,
    #[serde(default)]
    pub next_id: u64,
}

fn board_path(workspace: &Path) -> PathBuf {
    workspace.join(".oxide/board.json")
}

impl Board {
    pub fn load(workspace: &Path) -> Self {
        std::fs::read_to_string(board_path(workspace))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, workspace: &Path) {
        if let Some(dir) = board_path(workspace).parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(board_path(workspace), s);
        }
    }

    pub fn add(&mut self, title: String, desc: String) {
        let id = self.next_id;
        self.next_id += 1;
        self.cards.push(Card { id, title, desc, column: TODO.into(), result: String::new(), branch: String::new() });
    }
}

/// Run one card end-to-end in an isolated git worktree. Returns `(result, branch)`.
pub async fn run_card(base: Config, title: String, desc: String, id: u64, root: PathBuf) -> (String, String) {
    let branch = format!("oxide/card-{id}");
    let wt = root.join(format!(".oxide/worktrees/card-{id}"));
    let _ = tokio::process::Command::new("git")
        .args(["worktree", "add", "-B", &branch, &wt.to_string_lossy(), "HEAD"])
        .current_dir(&root)
        .output()
        .await;
    let workspace = if wt.exists() { wt.clone() } else { root.clone() };

    let mut cfg = base;
    cfg.workspace = Some(workspace.clone());
    cfg.harness = "coding".to_string();
    cfg.approval_policy = ApprovalPolicy::Never;
    cfg.persist = false;
    cfg.resume = false;
    cfg.orchestrate = false;
    cfg.subagents = false;

    let (handle, mut events) = match oxide_core::spawn(cfg) {
        Ok(x) => x,
        Err(e) => return (format!("spawn error: {e}"), branch),
    };
    let prompt = if desc.trim().is_empty() { title.clone() } else { format!("{title}\n\n{desc}") };
    let _ = handle.submit(Op::UserTurn { text: prompt }).await;

    let mut out = String::new();
    while let Some(ev) = events.recv().await {
        match ev {
            Event::AgentMessageDelta { text, .. } => out.push_str(&text),
            Event::Error { message } => out.push_str(&format!("\n[error] {message}")),
            Event::TurnFinished { .. } => break,
            Event::Shutdown => break,
            _ => {}
        }
    }
    let _ = handle.submit(Op::Shutdown).await;

    // Summarize file changes in the worktree.
    let stat = tokio::process::Command::new("git")
        .args(["status", "--short"])
        .current_dir(&workspace)
        .output()
        .await
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let changed = stat.lines().count();
    let summary = format!(
        "{}\n\n— {changed} file(s) changed · branch `{branch}` —",
        out.trim()
    );
    (summary, branch)
}

/// Import open GitHub issues (via `gh`) as cards `(title, body)`.
pub async fn import_github_issues(root: &Path) -> Vec<(String, String)> {
    let out = tokio::process::Command::new("gh")
        .args(["issue", "list", "--state", "open", "--limit", "30", "--json", "number,title,body"])
        .current_dir(root)
        .output()
        .await;
    let Ok(out) = out else { return Vec::new() };
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    v.as_array()
        .map(|arr| {
            arr.iter()
                .map(|i| {
                    let num = i["number"].as_u64().unwrap_or(0);
                    let title = i["title"].as_str().unwrap_or("");
                    let body = i["body"].as_str().unwrap_or("");
                    (format!("#{num} {title}"), body.to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}
