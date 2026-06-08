//! The single chokepoint through which every tool call passes.
//!
//! Native tools and (later) MCP tools route here. The router enforces the
//! [`ApprovalPolicy`] and applies the sandbox before any filesystem/process
//! mutation. Centralizing this is what makes the security model auditable.

use crate::sandbox::{self, PathCheck};
use oxide_protocol::{ApprovalPolicy, SandboxPolicy, ToolSpec};
use std::collections::HashMap;
use std::path::PathBuf;

pub struct ToolRouter {
    pub approval_policy: ApprovalPolicy,
    pub sandbox: SandboxPolicy,
    workspace: PathBuf,
    specs: HashMap<String, ToolSpec>,
    /// Tools the user approved for the whole session.
    session_approved: std::collections::HashSet<String>,
}

/// Outcome of routing a tool call.
pub enum Routed {
    /// Safe to run now (already approved or policy allows).
    Run,
    /// Engine must ask the frontend before running.
    NeedsApproval,
    /// Rejected by policy / unknown tool.
    Denied(String),
}

impl ToolRouter {
    pub fn new(
        approval_policy: ApprovalPolicy,
        sandbox: SandboxPolicy,
        workspace: PathBuf,
        tools: &[ToolSpec],
    ) -> Self {
        let specs = tools.iter().cloned().map(|t| (t.name.clone(), t)).collect();
        Self {
            approval_policy,
            sandbox,
            workspace,
            specs,
            session_approved: Default::default(),
        }
    }

    pub fn approve_for_session(&mut self, tool: &str) {
        self.session_approved.insert(tool.to_string());
    }

    pub fn is_session_approved(&self, tool: &str) -> bool {
        self.session_approved.contains(tool)
    }

    /// Human-readable summary of a pending call, for the approval prompt.
    pub fn summarize(&self, tool: &str, args: &serde_json::Value) -> String {
        match tool {
            "write_file" => format!("write {}", args["path"].as_str().unwrap_or("?")),
            "shell" => format!("run: {}", args["command"].as_str().unwrap_or("?")),
            other => format!("{other} {args}"),
        }
    }

    /// Decide whether `tool` may run under the current policy.
    pub fn route(&self, tool: &str) -> Routed {
        let Some(spec) = self.specs.get(tool) else {
            return Routed::Denied(format!("unknown tool '{tool}'"));
        };
        if self.session_approved.contains(tool) {
            return Routed::Run;
        }
        match self.approval_policy {
            ApprovalPolicy::Never => Routed::Run,
            ApprovalPolicy::Always => Routed::NeedsApproval,
            ApprovalPolicy::OnRequest => {
                if spec.mutating {
                    Routed::NeedsApproval
                } else {
                    Routed::Run
                }
            }
        }
    }

    /// Execute a tool for real, enforcing the sandbox. Returns `(output, ok)`.
    pub async fn execute(&self, tool: &str, args: &serde_json::Value) -> (String, bool) {
        match tool {
            "read_file" => self.exec_read(args),
            "write_file" => self.exec_write(args),
            "search" => self.exec_search(args),
            "shell" => self.exec_shell(args).await,
            "browser_open" => self.exec_browser_request("browser_open", args),
            "browser_snapshot" => self.exec_browser_request("browser_snapshot", args),
            other => (format!("unknown tool '{other}'"), false),
        }
    }

    fn exec_read(&self, args: &serde_json::Value) -> (String, bool) {
        let Some(path) = args["path"].as_str() else {
            return ("read_file: missing 'path'".into(), false);
        };
        match sandbox::check_read(self.sandbox, &self.workspace, std::path::Path::new(path)) {
            PathCheck::Denied(why) => (why, false),
            PathCheck::Ok(abs) => {
                // Guard before reading: a non-regular file (fifo/device/socket)
                // would block read_to_string forever, and a multi-GB file would
                // stall the engine. Both showed up as a "stuck" turn.
                match std::fs::metadata(&abs) {
                    Ok(m) if !m.is_file() => {
                        return (format!("read_file: '{path}' is not a regular file (skipped)"), false);
                    }
                    Ok(m) if m.len() > 10_000_000 => {
                        return (format!("read_file: '{path}' is {} MB — too large to read whole. Use `search` to locate the region.", m.len() / 1_000_000), false);
                    }
                    _ => {}
                }
                match std::fs::read_to_string(&abs) {
                Ok(content) => {
                    // Cap very large reads, but TELL the model it was truncated so
                    // it edits with what it has instead of re-reading blindly.
                    if content.chars().count() > 20_000 {
                        let capped: String = content.chars().take(20_000).collect();
                        (format!("{capped}\n\n… [truncated at 20000 chars — this file is larger; use `search` to locate the exact region instead of re-reading the whole file]"), true)
                    } else {
                        (content, true)
                    }
                }
                Err(e) => (format!("read_file error: {e}"), false),
                }
            }
        }
    }

    fn exec_write(&self, args: &serde_json::Value) -> (String, bool) {
        let Some(path) = args["path"].as_str() else {
            return ("write_file: missing 'path'".into(), false);
        };
        let content = args["content"].as_str().unwrap_or("");
        match sandbox::check_write(self.sandbox, &self.workspace, std::path::Path::new(path)) {
            PathCheck::Denied(why) => (why, false),
            PathCheck::Ok(abs) => {
                if let Some(parent) = abs.parent() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        return (format!("write_file mkdir error: {e}"), false);
                    }
                }
                match std::fs::write(&abs, content) {
                    Ok(()) => (
                        format!("wrote {} bytes to {}", content.len(), abs.display()),
                        true,
                    ),
                    Err(e) => (format!("write_file error: {e}"), false),
                }
            }
        }
    }

    /// Plain substring search across the workspace (skips target/.git and binaries).
    fn exec_search(&self, args: &serde_json::Value) -> (String, bool) {
        let Some(query) = args["query"].as_str() else {
            return ("search: missing 'query'".into(), false);
        };
        const SKIP: &[&str] = &[
            ".git", "target", ".oxide", "node_modules", "dist", "build",
            ".next", "vendor", ".venv", "__pycache__", ".cache", "out", ".turbo",
        ];
        let mut hits = Vec::new();
        let mut stack = vec![self.workspace.clone()];
        let mut visited = 0usize;
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                visited += 1;
                if visited > 50_000 {
                    hits.push("… (search stopped: too many files — narrow the path)".into());
                    return (hits.join("\n"), true);
                }
                let p = entry.path();
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if SKIP.contains(&name.as_ref()) {
                    continue;
                }
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                // Skip big/binary files: never slurp a >2MB file into a String.
                if std::fs::metadata(&p).map(|m| m.len() > 2_000_000).unwrap_or(true) {
                    continue;
                }
                if let Ok(text) = std::fs::read_to_string(&p) {
                    for (i, line) in text.lines().enumerate() {
                        if line.contains(query) {
                            hits.push(format!("{}:{}: {}", p.display(), i + 1, line.trim()));
                            if hits.len() >= 100 {
                                hits.push("… (truncated at 100 hits)".into());
                                return (hits.join("\n"), true);
                            }
                        }
                    }
                }
            }
        }
        if hits.is_empty() {
            (format!("no matches for '{query}'"), true)
        } else {
            (hits.join("\n"), true)
        }
    }

    fn exec_browser_request(&self, tool: &str, args: &serde_json::Value) -> (String, bool) {
        let Some(url) = args["url"].as_str() else {
            return (format!("{tool}: missing 'url'"), false);
        };
        let url = url.trim();
        if !is_supported_browser_url(url) {
            return (
                format!("{tool}: unsupported URL '{url}' (expected http, https, or file URL)"),
                false,
            );
        }
        let note = args["note"].as_str().unwrap_or("").trim();
        let mut output = format!("browser target requested: {url}");
        if !note.is_empty() {
            output.push_str(&format!("\nnote: {note}"));
        }
        (output, true)
    }

    /// Run a shell command, sandboxed via Seatbelt on macOS. Times out (and kills
    /// the process) after 120s and uses a null stdin, so a hung/interactive
    /// command can never freeze the agent.
    async fn exec_shell(&self, args: &serde_json::Value) -> (String, bool) {
        let Some(command) = args["command"].as_str() else {
            return ("shell: missing 'command'".into(), false);
        };
        let mut cmd = self.build_shell_command(command);
        cmd.current_dir(&self.workspace);
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);
        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return (format!("shell spawn error: {e}"), false),
        };
        let dur = std::time::Duration::from_secs(120);
        match tokio::time::timeout(dur, child.wait_with_output()).await {
            Ok(Ok(out)) => {
                let mut s = String::new();
                s.push_str(&String::from_utf8_lossy(&out.stdout));
                let err = String::from_utf8_lossy(&out.stderr);
                if !err.trim().is_empty() {
                    s.push_str("\n[stderr] ");
                    s.push_str(&err);
                }
                let ok = out.status.success();
                let capped: String = s.chars().take(20_000).collect();
                (capped, ok)
            }
            Ok(Err(e)) => (format!("shell error: {e}"), false),
            Err(_) => (
                "shell: timed out after 120s and was killed. For long-running processes (dev servers, watchers) run them in the background (append ' &' or use nohup) and poll, instead of blocking.".into(),
                false,
            ),
        }
    }

    #[cfg(target_os = "macos")]
    fn build_shell_command(&self, command: &str) -> tokio::process::Command {
        if matches!(self.sandbox, SandboxPolicy::DangerFullAccess) {
            let mut c = tokio::process::Command::new("/bin/sh");
            c.arg("-c").arg(command);
            return c;
        }
        let profile = sandbox::seatbelt_profile(self.sandbox, &self.workspace);
        let mut c = tokio::process::Command::new("/usr/bin/sandbox-exec");
        c.arg("-p")
            .arg(profile)
            .arg("/bin/sh")
            .arg("-c")
            .arg(command);
        c
    }

    #[cfg(not(target_os = "macos"))]
    fn build_shell_command(&self, command: &str) -> tokio::process::Command {
        // Linux Landlock/seccomp sandbox lands in Fase 5; for now run directly.
        tracing::warn!("shell sandbox not yet implemented on this platform");
        let mut c = tokio::process::Command::new("/bin/sh");
        c.arg("-c").arg(command);
        c
    }
}

fn is_supported_browser_url(url: &str) -> bool {
    let url = url.trim();
    url.starts_with("http://") || url.starts_with("https://") || url.starts_with("file://")
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxide_protocol::ToolSpec;

    fn router(dir: &std::path::Path) -> ToolRouter {
        let tools = vec![
            ToolSpec::new("read_file", "r"),
            ToolSpec::new("write_file", "w").mutating(true),
            ToolSpec::new("search", "s"),
        ];
        ToolRouter::new(
            ApprovalPolicy::Never,
            SandboxPolicy::WorkspaceWrite,
            dir.to_path_buf(),
            &tools,
        )
    }

    #[tokio::test]
    async fn write_then_read_roundtrips() {
        let tmp = std::env::temp_dir().join(format!("oxide-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let r = router(&tmp);

        let (out, ok) = r
            .execute(
                "write_file",
                &serde_json::json!({ "path": "hello.txt", "content": "hi oxide" }),
            )
            .await;
        assert!(ok, "write failed: {out}");

        let (content, ok) = r
            .execute("read_file", &serde_json::json!({ "path": "hello.txt" }))
            .await;
        assert!(ok);
        assert_eq!(content, "hi oxide");

        let (hits, ok) = r
            .execute("search", &serde_json::json!({ "query": "oxide" }))
            .await;
        assert!(ok);
        assert!(hits.contains("hello.txt"));

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn write_escape_is_denied() {
        let tmp = std::env::temp_dir().join(format!("oxide-test-esc-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let r = router(&tmp);
        let (out, ok) = r
            .execute(
                "write_file",
                &serde_json::json!({ "path": "../escape.txt", "content": "x" }),
            )
            .await;
        assert!(!ok, "escape should be denied");
        assert!(out.contains("outside workspace"));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn browser_open_validates_url_and_returns_frontend_contract() {
        let tmp = std::env::temp_dir().join(format!("oxide-browser-open-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let tools = vec![ToolSpec::new("browser_open", "Open browser target").mutating(true)];
        let router = ToolRouter::new(
            ApprovalPolicy::Never,
            SandboxPolicy::WorkspaceWrite,
            tmp.clone(),
            &tools,
        );

        let (out, ok) = router
            .execute(
                "browser_open",
                &serde_json::json!({
                    "url": "http://localhost:3000",
                    "note": "Open login page"
                }),
            )
            .await;

        assert!(ok, "browser_open should validate: {out}");
        assert!(out.contains("browser target requested"));
        assert!(out.contains("http://localhost:3000"));

        let (missing, ok) = router
            .execute(
                "browser_open",
                &serde_json::json!({ "note": "missing url" }),
            )
            .await;
        assert!(!ok);
        assert!(missing.contains("missing 'url'"));
        std::fs::remove_dir_all(tmp).ok();
    }
}
