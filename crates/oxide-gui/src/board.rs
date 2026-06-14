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
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub source_id: String,
    #[serde(default)]
    pub source_url: String,
    #[serde(default)]
    pub source_status: String,
    #[serde(default)]
    pub source_priority: String,
    #[serde(default)]
    pub source_assignee: String,
    #[serde(default)]
    pub source_updated_at: String,
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
        self.cards.push(Card {
            id,
            title,
            desc,
            column: TODO.into(),
            result: String::new(),
            branch: String::new(),
            source: String::new(),
            source_id: String::new(),
            source_url: String::new(),
            source_status: String::new(),
            source_priority: String::new(),
            source_assignee: String::new(),
            source_updated_at: String::new(),
        });
    }

    pub fn upsert_issues(&mut self, issues: Vec<IssueCard>) -> (usize, usize) {
        let mut added = 0;
        let mut updated = 0;
        for issue in issues {
            let by_source = self
                .cards
                .iter()
                .position(|card| !card.source_id.is_empty() && card.source_id == issue.source_id);
            let by_legacy_title = self
                .cards
                .iter()
                .position(|card| card.source_id.is_empty() && card.title == issue.title);
            if let Some(idx) = by_source.or(by_legacy_title) {
                if self.cards[idx].apply_issue(issue) {
                    updated += 1;
                }
            } else {
                let id = self.next_id;
                self.next_id += 1;
                self.cards.push(Card::from_issue(id, issue));
                added += 1;
            }
        }
        (added, updated)
    }
}

impl Card {
    fn from_issue(id: u64, issue: IssueCard) -> Self {
        Self {
            id,
            title: issue.title,
            desc: issue.desc,
            column: TODO.into(),
            result: String::new(),
            branch: String::new(),
            source: issue.source,
            source_id: issue.source_id,
            source_url: issue.url,
            source_status: issue.status,
            source_priority: issue.priority,
            source_assignee: issue.assignee,
            source_updated_at: issue.updated_at,
        }
    }

    fn apply_issue(&mut self, issue: IssueCard) -> bool {
        let before = (
            self.title.clone(),
            self.desc.clone(),
            self.source.clone(),
            self.source_id.clone(),
            self.source_url.clone(),
            self.source_status.clone(),
            self.source_priority.clone(),
            self.source_assignee.clone(),
            self.source_updated_at.clone(),
        );
        self.title = issue.title;
        self.desc = issue.desc;
        self.source = issue.source;
        self.source_id = issue.source_id;
        self.source_url = issue.url;
        self.source_status = issue.status;
        self.source_priority = issue.priority;
        self.source_assignee = issue.assignee;
        self.source_updated_at = issue.updated_at;
        before
            != (
                self.title.clone(),
                self.desc.clone(),
                self.source.clone(),
                self.source_id.clone(),
                self.source_url.clone(),
                self.source_status.clone(),
                self.source_priority.clone(),
                self.source_assignee.clone(),
                self.source_updated_at.clone(),
            )
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

#[derive(Clone, Debug, PartialEq)]
pub struct IssueCard {
    pub source: String,
    pub source_id: String,
    pub title: String,
    pub desc: String,
    pub url: String,
    pub status: String,
    pub priority: String,
    pub assignee: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct IssueFetch {
    pub issues: Vec<IssueCard>,
    pub github_count: usize,
    pub linear_count: usize,
    pub linear_skipped: bool,
    pub errors: Vec<String>,
}

pub async fn fetch_issue_cards(root: &Path) -> IssueFetch {
    let mut out = IssueFetch::default();
    match fetch_github_issues(root).await {
        Ok(issues) => {
            out.github_count = issues.len();
            out.issues.extend(issues);
        }
        Err(err) => out.errors.push(format!("GitHub: {err}")),
    }
    match fetch_linear_issues().await {
        Ok(Some(issues)) => {
            out.linear_count = issues.len();
            out.issues.extend(issues);
        }
        Ok(None) => out.linear_skipped = true,
        Err(err) => out.errors.push(format!("Linear: {err}")),
    }
    out
}

pub fn sync_summary(fetch: &IssueFetch, added: usize, updated: usize) -> String {
    let mut parts = vec![format!(
        "Synced {added} new, {updated} updated · GitHub {} · Linear {}",
        fetch.github_count, fetch.linear_count
    )];
    if fetch.linear_skipped {
        parts.push("Linear skipped: set LINEAR_API_KEY".to_string());
    }
    if !fetch.errors.is_empty() {
        parts.push(fetch.errors.join(" · "));
    }
    parts.join(" · ")
}

async fn fetch_github_issues(root: &Path) -> Result<Vec<IssueCard>, String> {
    let repo = github_repo(root)
        .await
        .unwrap_or_else(|| "github".to_string());
    let out = tokio::process::Command::new("gh")
        .args([
            "issue",
            "list",
            "--state",
            "open",
            "--limit",
            "30",
            "--json",
            "number,title,body,url,state,labels,assignees,updatedAt",
        ])
        .current_dir(root)
        .output()
        .await;
    let out = out.map_err(|err| format!("gh issue list failed: {err}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            "gh issue list failed".to_string()
        } else {
            stderr
        });
    }
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|err| format!("invalid gh JSON: {err}"))?;
    let Some(arr) = v.as_array() else {
        return Ok(Vec::new());
    };
    Ok(arr
        .iter()
        .filter_map(|issue| github_issue_from_value(&repo, issue))
        .collect())
}

async fn github_repo(root: &Path) -> Option<String> {
    let out = tokio::process::Command::new("gh")
        .args([
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "--jq",
            ".nameWithOwner",
        ])
        .current_dir(root)
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let repo = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!repo.is_empty()).then_some(repo)
}

fn github_issue_from_value(repo: &str, issue: &serde_json::Value) -> Option<IssueCard> {
    let num = issue["number"].as_u64()?;
    let title = issue["title"].as_str().unwrap_or("").trim();
    let body = issue["body"].as_str().unwrap_or("").trim();
    let url = issue["url"].as_str().unwrap_or("").to_string();
    let status = issue["state"].as_str().unwrap_or("OPEN").to_string();
    let updated_at = issue["updatedAt"].as_str().unwrap_or("").to_string();
    let labels = json_names(&issue["labels"]);
    let assignees = json_names(&issue["assignees"]);
    let mut desc = String::new();
    if !url.is_empty() {
        desc.push_str(&format!("GitHub: {url}\n"));
    }
    if !labels.is_empty() {
        desc.push_str(&format!("Labels: {}\n", labels.join(", ")));
    }
    if !assignees.is_empty() {
        desc.push_str(&format!("Assignee: {}\n", assignees.join(", ")));
    }
    if !body.is_empty() {
        if !desc.is_empty() {
            desc.push('\n');
        }
        desc.push_str(body);
    }
    Some(IssueCard {
        source: "GitHub".to_string(),
        source_id: format!("github:{repo}#{num}"),
        title: format!("#{num} {title}"),
        desc,
        url,
        status,
        priority: labels.join(", "),
        assignee: assignees.join(", "),
        updated_at,
    })
}

async fn fetch_linear_issues() -> Result<Option<Vec<IssueCard>>, String> {
    let token = std::env::var("LINEAR_API_KEY")
        .or_else(|_| std::env::var("LINEAR_TOKEN"))
        .ok()
        .filter(|value| !value.trim().is_empty());
    let Some(token) = token else {
        return Ok(None);
    };
    let query = r#"
        query OxideAssignedIssues($first: Int!) {
          viewer {
            assignedIssues(first: $first, orderBy: updatedAt) {
              nodes {
                identifier
                title
                description
                priority
                url
                branchName
                updatedAt
                archivedAt
                completedAt
                canceledAt
                state { name type }
                labels { nodes { name } }
                assignee { name email }
                team { key name }
                project { name }
              }
            }
          }
        }
    "#;
    let client = reqwest::Client::builder()
        .user_agent("oxide-board-sync")
        .build()
        .map_err(|err| format!("client error: {err}"))?;
    let resp = client
        .post("https://api.linear.app/graphql")
        .bearer_auth(token)
        .json(&serde_json::json!({ "query": query, "variables": { "first": 30 } }))
        .send()
        .await
        .map_err(|err| format!("request failed: {err}"))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|err| format!("response read failed: {err}"))?;
    if !status.is_success() {
        return Err(format!("api {status}: {}", short_text(&text)));
    }
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|err| format!("invalid response JSON: {err}"))?;
    if let Some(errors) = v.get("errors").and_then(|x| x.as_array()) {
        if !errors.is_empty() {
            return Err(short_text(
                &errors
                    .iter()
                    .map(|err| err.to_string())
                    .collect::<Vec<_>>()
                    .join("; "),
            ));
        }
    }
    let Some(nodes) = v
        .pointer("/data/viewer/assignedIssues/nodes")
        .and_then(|nodes| nodes.as_array())
    else {
        return Ok(Some(Vec::new()));
    };
    Ok(Some(
        nodes.iter().filter_map(linear_issue_from_value).collect(),
    ))
}

fn linear_issue_from_value(issue: &serde_json::Value) -> Option<IssueCard> {
    if !issue["archivedAt"].is_null()
        || !issue["completedAt"].is_null()
        || !issue["canceledAt"].is_null()
    {
        return None;
    }
    let state_type = issue
        .pointer("/state/type")
        .and_then(|x| x.as_str())
        .unwrap_or("");
    if matches!(state_type, "completed" | "canceled") {
        return None;
    }
    let id = issue["identifier"].as_str()?.trim();
    let title = issue["title"].as_str().unwrap_or("").trim();
    let description = issue["description"].as_str().unwrap_or("").trim();
    let url = issue["url"].as_str().unwrap_or("").to_string();
    let status = issue
        .pointer("/state/name")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let updated_at = issue["updatedAt"].as_str().unwrap_or("").to_string();
    let labels = issue
        .pointer("/labels/nodes")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item["name"].as_str().map(ToString::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
        .join(", ");
    let assignee = issue
        .pointer("/assignee/name")
        .and_then(|x| x.as_str())
        .or_else(|| issue.pointer("/assignee/email").and_then(|x| x.as_str()))
        .unwrap_or("")
        .to_string();
    let team = issue
        .pointer("/team/key")
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let project = issue
        .pointer("/project/name")
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let branch = issue["branchName"].as_str().unwrap_or("");
    let priority = linear_priority(issue["priority"].as_i64().unwrap_or(0)).to_string();
    let mut desc = String::new();
    if !url.is_empty() {
        desc.push_str(&format!("Linear: {url}\n"));
    }
    if !status.is_empty() {
        desc.push_str(&format!("Status: {status}\n"));
    }
    if !priority.is_empty() {
        desc.push_str(&format!("Priority: {priority}\n"));
    }
    if !team.is_empty() || !project.is_empty() {
        desc.push_str(
            &format!("Team/project: {team} {project}\n")
                .trim_end()
                .to_string(),
        );
        desc.push('\n');
    }
    if !labels.is_empty() {
        desc.push_str(&format!("Labels: {labels}\n"));
    }
    if !branch.is_empty() {
        desc.push_str(&format!("Branch: {branch}\n"));
    }
    if !description.is_empty() {
        if !desc.is_empty() {
            desc.push('\n');
        }
        desc.push_str(description);
    }
    Some(IssueCard {
        source: "Linear".to_string(),
        source_id: format!("linear:{id}"),
        title: format!("{id} {title}"),
        desc,
        url,
        status,
        priority,
        assignee,
        updated_at,
    })
}

fn linear_priority(value: i64) -> &'static str {
    match value {
        1 => "Urgent",
        2 => "High",
        3 => "Normal",
        4 => "Low",
        _ => "",
    }
}

fn json_names(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item["name"]
                        .as_str()
                        .or_else(|| item["login"].as_str())
                        .map(ToString::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn short_text(text: &str) -> String {
    let cleaned = text.replace('\n', " ");
    cleaned.chars().take(500).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_upsert_dedupes_by_source_id() {
        let mut board = Board::default();
        let issue = IssueCard {
            source: "GitHub".to_string(),
            source_id: "github:owner/repo#1".to_string(),
            title: "#1 Fix crash".to_string(),
            desc: "Initial".to_string(),
            url: "https://github.com/owner/repo/issues/1".to_string(),
            status: "OPEN".to_string(),
            priority: "bug".to_string(),
            assignee: "ana".to_string(),
            updated_at: "2026-06-14T00:00:00Z".to_string(),
        };

        assert_eq!(board.upsert_issues(vec![issue.clone()]), (1, 0));
        assert_eq!(
            board.upsert_issues(vec![IssueCard {
                desc: "Updated".to_string(),
                ..issue
            }]),
            (0, 1)
        );
        assert_eq!(board.cards.len(), 1);
        assert_eq!(board.cards[0].desc, "Updated");
        assert_eq!(board.cards[0].column, TODO);
    }

    #[test]
    fn github_issue_parser_builds_stable_source_id() {
        let raw = serde_json::json!({
            "number": 7,
            "title": "Board sync",
            "body": "Do it",
            "url": "https://github.com/owner/repo/issues/7",
            "state": "OPEN",
            "updatedAt": "2026-06-14T01:00:00Z",
            "labels": [{"name": "enhancement"}],
            "assignees": [{"login": "octo"}]
        });

        let issue = github_issue_from_value("owner/repo", &raw).unwrap();
        assert_eq!(issue.source_id, "github:owner/repo#7");
        assert_eq!(issue.title, "#7 Board sync");
        assert_eq!(issue.priority, "enhancement");
        assert_eq!(issue.assignee, "octo");
        assert!(issue.desc.contains("Do it"));
    }

    #[test]
    fn linear_issue_parser_skips_completed_items() {
        let raw = serde_json::json!({
            "identifier": "DEV-1",
            "title": "Done",
            "archivedAt": null,
            "completedAt": "2026-06-14T01:00:00Z",
            "canceledAt": null,
            "state": {"name": "Done", "type": "completed"}
        });

        assert!(linear_issue_from_value(&raw).is_none());
    }
}
