//! The single chokepoint through which every tool call passes.
//!
//! Native tools and (later) MCP tools route here. The router enforces the
//! [`ApprovalPolicy`] and applies the sandbox before any filesystem/process
//! mutation. Centralizing this is what makes the security model auditable.

use crate::sandbox::{self, PathCheck};
use oxide_protocol::{ApprovalPolicy, Event, SandboxPolicy, ToolSpec, TurnId};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::io::{AsyncRead, AsyncReadExt, BufReader};
use tokio::sync::mpsc;

pub struct ToolRouter {
    pub approval_policy: ApprovalPolicy,
    pub sandbox: SandboxPolicy,
    workspace: PathBuf,
    specs: HashMap<String, ToolSpec>,
    /// Tools the user approved for the whole session.
    session_approved: std::collections::HashSet<String>,
    /// The CURRENT call was explicitly user-approved — run it unsandboxed
    /// (codex semantics: approval lifts the sandbox, otherwise `git commit`
    /// or `git push` still dies on the .git/network deny even after a Yes).
    approved: bool,
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
            approved: false,
        }
    }

    pub fn approve_for_session(&mut self, tool: &str) {
        self.session_approved.insert(tool.to_string());
    }

    /// Mark the current call as explicitly user-approved (runs unsandboxed).
    pub fn set_approved(&mut self, v: bool) {
        self.approved = v;
    }

    pub fn is_session_approved(&self, tool: &str) -> bool {
        self.session_approved.contains(tool)
    }

    /// Human-readable summary of a pending call, for the approval prompt.
    pub fn summarize(&self, tool: &str, args: &serde_json::Value) -> String {
        match tool {
            "read_file" => format!("Read file:\n{}", args["path"].as_str().unwrap_or("?")),
            "write_file" => format!(
                "Write file:\n{}\n\nContent: {} bytes",
                args["path"].as_str().unwrap_or("?"),
                args["content"].as_str().map(|s| s.len()).unwrap_or(0)
            ),
            "edit" => format!(
                "Edit file:\n{}\n\nFind: {} bytes\nReplace with: {} bytes",
                args["path"].as_str().unwrap_or("?"),
                args["old_string"].as_str().map(|s| s.len()).unwrap_or(0),
                args["new_string"].as_str().map(|s| s.len()).unwrap_or(0)
            ),
            "shell" => {
                let timeout = args["timeout_seconds"]
                    .as_u64()
                    .unwrap_or(120)
                    .clamp(1, 600);
                format!(
                    "Command:\n{}\n\nWorking directory:\n{}\nTimeout: {timeout}s\n\nThis can modify files, run networked commands, or affect git depending on the command.",
                    args["command"].as_str().unwrap_or("?"),
                    self.workspace.display()
                )
            }
            "git_commit" => {
                let paths = args["paths"].as_array().map_or(0, Vec::len);
                format!(
                    "Commit {paths} explicitly listed path(s):\n{}",
                    args["message"].as_str().unwrap_or("?")
                )
            }
            "git_push" => format!(
                "Push branch '{}' to remote '{}' (force is not supported)",
                args["branch"].as_str().unwrap_or("current"),
                args["remote"].as_str().unwrap_or("origin")
            ),
            "todo_write" => {
                let n = args["todos"].as_array().map(|a| a.len()).unwrap_or(0);
                format!("Update task checklist:\n{n} item(s)")
            }
            "browser_navigate" | "browser_open" => {
                format!(
                    "Open browser target:\n{}",
                    args["url"].as_str().unwrap_or("?")
                )
            }
            "browser_snapshot" => {
                format!(
                    "Request browser snapshot:\n{}",
                    args["url"].as_str().unwrap_or("?")
                )
            }
            "browser_click" => format!(
                "Click browser selector:\n{}",
                args["selector"].as_str().unwrap_or("?")
            ),
            "browser_type" => format!(
                "Type into browser selector:\n{}\n\nText: {} chars",
                args["selector"].as_str().unwrap_or("?"),
                args["text"]
                    .as_str()
                    .map(|s| s.chars().count())
                    .unwrap_or(0)
            ),
            "execute_code" => {
                let code = args["code"].as_str().unwrap_or("?");
                let preview: String = code.lines().take(12).collect::<Vec<_>>().join("\n");
                let more = code.lines().count().saturating_sub(12);
                format!(
                    "Run Python script (read-only tool RPC):\n{preview}{}",
                    if more > 0 {
                        format!("\n… +{more} more line(s)")
                    } else {
                        String::new()
                    }
                )
            }
            "web_search" => format!("Search web:\n{}", args["query"].as_str().unwrap_or("?")),
            "fetch_url" => format!("Fetch URL:\n{}", args["url"].as_str().unwrap_or("?")),
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
                        return (
                            format!("read_file: '{path}' is not a regular file (skipped)"),
                            false,
                        );
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
            ".git",
            "target",
            ".oxide",
            "node_modules",
            "dist",
            "build",
            ".next",
            "vendor",
            ".venv",
            "__pycache__",
            ".cache",
            "out",
            ".turbo",
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
                if std::fs::metadata(&p)
                    .map(|m| m.len() > 2_000_000)
                    .unwrap_or(true)
                {
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
        let timeout_s = args["timeout_seconds"]
            .as_u64()
            .unwrap_or(120)
            .clamp(1, 600);
        let started = std::time::Instant::now();
        let mut cmd = self.build_shell_command(command);
        cmd.current_dir(&self.workspace);
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);
        // Own process group: a timeout can kill the whole tree (otherwise a
        // spawned dev server survives the kill, holds its port, AND keeps the
        // stdout pipe open so wait_with_output never sees EOF).
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return (format!("shell spawn error: {e}"), false),
        };
        #[cfg(unix)]
        let pgid = child.id().map(|id| id as i32);

        let (line_tx, mut line_rx) = mpsc::channel::<(&'static str, String)>(64);
        if let Some(stdout) = child.stdout.take() {
            spawn_shell_reader(stdout, "stdout", line_tx.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_shell_reader(stderr, "stderr", line_tx.clone());
        }
        drop(line_tx);

        let mut capture = ShellCapture::default();
        let mut wait_task = tokio::spawn(async move { child.wait().await });
        let deadline = tokio::time::sleep(std::time::Duration::from_secs(timeout_s));
        tokio::pin!(deadline);
        let mut rx_open = true;

        let status = loop {
            tokio::select! {
                result = &mut wait_task => {
                    break result.unwrap_or_else(|err| Err(std::io::Error::other(err)));
                }
                line = line_rx.recv(), if rx_open => {
                    match line {
                        Some((stream, chunk)) => capture.push(stream, &chunk),
                        None => rx_open = false,
                    }
                }
                _ = &mut deadline => {
                    #[cfg(unix)]
                    if let Some(pg) = pgid {
                        unsafe { libc::killpg(pg, libc::SIGKILL); }
                    }
                    if tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        &mut wait_task,
                    )
                    .await
                    .is_err()
                    {
                        wait_task.abort();
                    }
                    return (
                        format!(
                            "$ {command}\n[timeout after {timeout_s}s · {}]\n{}\nFor long-running processes (dev servers, watchers), start them detached with output redirected — e.g. `nohup npm run dev >/tmp/oxide-dev.log 2>&1 &` — then poll the log or port instead of blocking.",
                            format_elapsed(started.elapsed()),
                            capture.body(),
                        ),
                        false,
                    );
                }
            }
        };

        while let Ok(Some((stream, chunk))) =
            tokio::time::timeout(std::time::Duration::from_millis(50), line_rx.recv()).await
        {
            capture.push(stream, &chunk);
        }

        match status {
            Ok(exit) => {
                let ok = exit.success();
                let code = exit.status_code_string();
                let elapsed = format_elapsed(started.elapsed());
                (
                    format!("$ {command}\n[exit {code} · {elapsed}]\n{}", capture.body()),
                    ok,
                )
            }
            Err(e) => (format!("shell error: {e}"), false),
        }
    }

    /// Streaming variant used by GUI/subscription paths. It preserves the same
    /// sandbox/approval command construction as `exec_shell`, but emits command
    /// output chunks while the process is still running.
    pub async fn exec_shell_streaming(
        &self,
        args: &serde_json::Value,
        turn: TurnId,
        command_id: String,
        worker_id: Option<String>,
        event_tx: mpsc::Sender<Event>,
    ) -> (String, bool) {
        let Some(command) = args["command"].as_str() else {
            return ("shell: missing 'command'".into(), false);
        };
        let timeout_s = args["timeout_seconds"]
            .as_u64()
            .unwrap_or(120)
            .clamp(1, 600);
        let started = std::time::Instant::now();
        let mut cmd = self.build_shell_command(command);
        cmd.current_dir(&self.workspace);
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return (format!("shell spawn error: {e}"), false),
        };
        #[cfg(unix)]
        let pgid = child.id().map(|id| id as i32);

        let (line_tx, mut line_rx) = mpsc::channel::<(&'static str, String)>(64);
        if let Some(stdout) = child.stdout.take() {
            spawn_shell_reader(stdout, "stdout", line_tx.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_shell_reader(stderr, "stderr", line_tx.clone());
        }
        drop(line_tx);

        let mut capture = ShellCapture::default();
        let mut wait_task = tokio::spawn(async move { child.wait().await });
        let deadline = tokio::time::sleep(std::time::Duration::from_secs(timeout_s));
        tokio::pin!(deadline);
        let mut rx_open = true;

        let status = loop {
            tokio::select! {
                result = &mut wait_task => {
                    break result.unwrap_or_else(|err| Err(std::io::Error::other(err)));
                }
                line = line_rx.recv(), if rx_open => {
                    match line {
                        Some((stream, chunk)) => {
                            capture.push(stream, &chunk);
                            let _ = event_tx.send(Event::CommandOutput {
                                turn,
                                command_id: command_id.clone(),
                                worker_id: worker_id.clone(),
                                stream: stream.to_string(),
                                chunk,
                            }).await;
                        }
                        None => rx_open = false,
                    }
                }
                _ = &mut deadline => {
                    #[cfg(unix)]
                    if let Some(pg) = pgid {
                        unsafe { libc::killpg(pg, libc::SIGKILL); }
                    }
                    if tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        &mut wait_task,
                    )
                    .await
                    .is_err()
                    {
                        wait_task.abort();
                    }
                    let body = capture.body();
                    return (
                        format!(
                            "$ {command}\n[timeout after {timeout_s}s · {}]\n{body}\nFor long-running processes (dev servers, watchers), start them detached with output redirected — e.g. `nohup npm run dev >/tmp/oxide-dev.log 2>&1 &` — then poll the log or port instead of blocking.",
                            format_elapsed(started.elapsed())
                        ),
                        false,
                    );
                }
            }
        };

        while let Ok(Some((stream, chunk))) =
            tokio::time::timeout(std::time::Duration::from_millis(50), line_rx.recv()).await
        {
            capture.push(stream, &chunk);
            let _ = event_tx
                .send(Event::CommandOutput {
                    turn,
                    command_id: command_id.clone(),
                    worker_id: worker_id.clone(),
                    stream: stream.to_string(),
                    chunk,
                })
                .await;
        }

        match status {
            Ok(exit) => {
                let ok = exit.success();
                let code = exit.status_code_string();
                let elapsed = format_elapsed(started.elapsed());
                let body = capture.body();
                (
                    format!("$ {command}\n[exit {code} · {elapsed}]\n{body}"),
                    ok,
                )
            }
            Err(e) => (format!("shell error: {e}"), false),
        }
    }

    #[cfg(target_os = "macos")]
    fn build_shell_command(&self, command: &str) -> tokio::process::Command {
        // Explicit user approval lifts the sandbox for THIS call — that's what
        // the approval is for (e.g. an approved `git commit`/`git push` must
        // not still die on the .git/network deny).
        let mut c = if self.approved || matches!(self.sandbox, SandboxPolicy::DangerFullAccess) {
            let mut c = tokio::process::Command::new("/bin/sh");
            c.arg("-c").arg(command);
            c
        } else {
            let profile = sandbox::seatbelt_profile(self.sandbox, &self.workspace);
            let mut c = tokio::process::Command::new("/usr/bin/sandbox-exec");
            c.arg("-p")
                .arg(profile)
                .arg("/bin/sh")
                .arg("-c")
                .arg(command);
            c
        };
        augment_shell_env(&mut c);
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

const SHELL_OUTPUT_HEAD_BYTES: usize = 8_000;
const SHELL_OUTPUT_TAIL_BYTES: usize = 12_000;

#[derive(Default)]
struct ShellCapture {
    head: Vec<u8>,
    tail: std::collections::VecDeque<u8>,
    total: usize,
    last_stream: Option<&'static str>,
}

impl ShellCapture {
    fn push(&mut self, stream: &'static str, chunk: &str) {
        let marker = if self.last_stream == Some(stream) {
            &[][..]
        } else if stream == "stderr" {
            b"\n[stderr] "
        } else if self.last_stream.is_some() {
            b"\n[stdout] "
        } else {
            &[][..]
        };
        self.push_bytes(marker);
        self.push_bytes(chunk.as_bytes());
        self.last_stream = Some(stream);
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        self.total = self.total.saturating_add(bytes.len());
        let head_room = SHELL_OUTPUT_HEAD_BYTES.saturating_sub(self.head.len());
        let split = head_room.min(bytes.len());
        self.head.extend_from_slice(&bytes[..split]);
        for byte in &bytes[split..] {
            if self.tail.len() == SHELL_OUTPUT_TAIL_BYTES {
                self.tail.pop_front();
            }
            self.tail.push_back(*byte);
        }
    }

    fn body(&self) -> String {
        if self.total == 0 {
            return "(no output)".to_string();
        }
        if self.total <= SHELL_OUTPUT_HEAD_BYTES + SHELL_OUTPUT_TAIL_BYTES {
            let mut bytes = self.head.clone();
            bytes.extend(self.tail.iter().copied());
            return String::from_utf8_lossy(&bytes).into_owned();
        }
        let omitted = self.total.saturating_sub(self.head.len() + self.tail.len());
        let tail: Vec<u8> = self.tail.iter().copied().collect();
        format!(
            "{}\n… [output truncated: {omitted} bytes omitted] …\n{}",
            String::from_utf8_lossy(&self.head),
            String::from_utf8_lossy(&tail)
        )
    }
}

fn spawn_shell_reader<R>(reader: R, stream: &'static str, tx: mpsc::Sender<(&'static str, String)>)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut reader = BufReader::new(reader);
        let mut buf = vec![0u8; 4096];
        while let Ok(n) = reader.read(&mut buf).await {
            if n == 0 {
                break;
            }
            let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
            if tx.send((stream, chunk)).await.is_err() {
                break;
            }
        }
    });
}

trait ExitStatusExt {
    fn status_code_string(&self) -> String;
}

impl ExitStatusExt for std::process::ExitStatus {
    fn status_code_string(&self) -> String {
        self.code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".to_string())
    }
}

fn format_elapsed(d: std::time::Duration) -> String {
    if d.as_secs() >= 60 {
        format!("{}m{}s", d.as_secs() / 60, d.as_secs() % 60)
    } else if d.as_secs() > 0 {
        format!("{}s", d.as_secs())
    } else {
        format!("{}ms", d.as_millis())
    }
}

/// Give shell commands a usable login-ish environment. A GUI app launched from
/// Finder inherits a MINIMAL env: a short PATH (no Homebrew / ~/.local/bin /
/// gh) and no `SSH_AUTH_SOCK` — so `git push` (ssh) and `gh` fail with
/// "permission denied / not logged in" even under Full access. Prepend the
/// common bin dirs and recover the launchd ssh-agent socket so auth works the
/// same as it does from a terminal.
pub(crate) fn augment_shell_env(cmd: &mut tokio::process::Command) {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut dirs = vec![
        format!("{home}/.local/bin"),
        "/opt/homebrew/bin".into(),
        "/opt/homebrew/sbin".into(),
        "/usr/local/bin".into(),
        format!("{home}/.cargo/bin"),
        format!("{home}/.bun/bin"),
        format!("{home}/.superconductor/bin"),
        "/usr/bin".into(),
        "/bin".into(),
        "/usr/sbin".into(),
        "/sbin".into(),
    ];
    if let Ok(p) = std::env::var("PATH") {
        for seg in p.split(':') {
            if !seg.is_empty() && !dirs.iter().any(|d| d == seg) {
                dirs.push(seg.to_string());
            }
        }
    }
    cmd.env("PATH", dirs.join(":"));
    // SSH auth: if the inherited env has no agent socket, ask launchd (macOS
    // keeps it per-session). Cached so we don't spawn launchctl per command.
    if std::env::var_os("SSH_AUTH_SOCK").is_none() {
        if let Some(sock) = launchd_ssh_auth_sock() {
            cmd.env("SSH_AUTH_SOCK", sock);
        }
    }
}

#[cfg(target_os = "macos")]
fn launchd_ssh_auth_sock() -> Option<String> {
    use std::sync::OnceLock;
    static SOCK: OnceLock<Option<String>> = OnceLock::new();
    SOCK.get_or_init(|| {
        let out = std::process::Command::new("/bin/launchctl")
            .args(["getenv", "SSH_AUTH_SOCK"])
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    })
    .clone()
}

#[cfg(not(target_os = "macos"))]
fn launchd_ssh_auth_sock() -> Option<String> {
    None
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
            ToolSpec::new("shell", "sh").mutating(true),
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
    async fn shell_output_includes_command_status_and_duration() {
        let tmp = std::env::temp_dir().join(format!("oxide-shell-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let r = router(&tmp);

        let (out, ok) = r
            .execute(
                "shell",
                &serde_json::json!({ "command": "printf hello", "timeout_seconds": 5 }),
            )
            .await;

        assert!(ok, "shell should succeed: {out}");
        assert!(out.contains("$ printf hello"));
        assert!(out.contains("[exit 0"));
        assert!(out.contains("hello"));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn shell_timeout_keeps_bounded_partial_output() {
        let tmp = std::env::temp_dir().join(format!("oxide-shell-timeout-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let r = router(&tmp);

        let (out, ok) = r
            .execute(
                "shell",
                &serde_json::json!({ "command": "printf before-timeout; sleep 5", "timeout_seconds": 1 }),
            )
            .await;

        assert!(!ok);
        assert!(out.contains("timeout after 1s"));
        assert!(out.contains("before-timeout"));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn shell_capture_preserves_order_and_marks_truncation() {
        let mut capture = ShellCapture::default();
        capture.push("stdout", "start");
        capture.push("stderr", "problem");
        capture.push("stdout", &"x".repeat(40_000));
        capture.push("stdout", "end");

        let body = capture.body();
        assert!(body.starts_with("start\n[stderr] problem\n[stdout] "));
        assert!(body.contains("output truncated:"));
        assert!(body.ends_with("end"));
        assert!(body.len() < 22_000);
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
