//! Programmatic Tool Calling (hermes/Anthropic "PTC"): the model writes a
//! small Python script; inside it, `oxide_call(name, args)` round-trips over a
//! local Unix socket back into a restricted READ-ONLY tool router. Loops and
//! batches over many tool calls run in-process instead of burning one model
//! round-trip per call. Only the script's stdout returns to the model.

use crate::tools::ToolRouter;
use oxide_protocol::SandboxPolicy;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

/// Tools scripts may call — read-only, sandbox-checked in the router. Never
/// expose mutating tools here: PTC calls bypass the approval flow entirely.
const ALLOWED: &[&str] = &["read_file", "search"];
const MAX_CALLS: usize = 50;
const MAX_STDOUT: usize = 50 * 1024;
const MAX_STDERR: usize = 8 * 1024;
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

static PTC_SEQ: AtomicU64 = AtomicU64::new(0);

/// Prelude prepended to every script: connects to the RPC socket and defines
/// `oxide_call`. Underscored names keep the model's namespace clean.
const PY_BOOT: &str = r#"
import json as _json, os as _os, socket as _socket
_conn = _socket.socket(_socket.AF_UNIX, _socket.SOCK_STREAM)
_conn.connect(_os.environ["OXIDE_PTC_SOCKET"])
_rpc = _conn.makefile("rw")
def oxide_call(name, args=None):
    _rpc.write(_json.dumps({"name": name, "args": args or {}}) + "\n")
    _rpc.flush()
    _line = _rpc.readline()
    if not _line:
        raise RuntimeError("oxide rpc closed")
    _r = _json.loads(_line)
    if not _r.get("ok"):
        raise RuntimeError(_r.get("output") or "tool call failed")
    return _r.get("output", "")
"#;

/// Run one PTC script to completion. Returns `(tool_output, ok)`.
pub async fn run(workspace: &Path, sandbox: SandboxPolicy, code: &str) -> (String, bool) {
    let seq = PTC_SEQ.fetch_add(1, Ordering::SeqCst);
    let sock_path =
        std::env::temp_dir().join(format!("oxide-ptc-{}-{seq}.sock", std::process::id()));
    let _ = std::fs::remove_file(&sock_path);
    let listener = match tokio::net::UnixListener::bind(&sock_path) {
        Ok(l) => l,
        Err(e) => return (format!("execute_code: rpc socket failed: {e}"), false),
    };

    let mut child = match tokio::process::Command::new("python3")
        .arg("-c")
        .arg(format!("{PY_BOOT}\n{code}"))
        .env("OXIDE_PTC_SOCKET", &sock_path)
        .current_dir(workspace)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&sock_path);
            return (format!("execute_code: python3 failed to start: {e}"), false);
        }
    };
    let out_task = tokio::spawn(read_capped(child.stdout.take(), MAX_STDOUT));
    let err_task = tokio::spawn(read_capped(child.stderr.take(), MAX_STDERR));

    // RPC server: sequential line-JSON requests from the script; each request
    // dispatches an allowed read-only tool and answers on the same stream.
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_srv = calls.clone();
    let router = ToolRouter::new(
        oxide_protocol::ApprovalPolicy::Never,
        sandbox,
        workspace.to_path_buf(),
        &[],
    );
    let server = tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        while let Ok((stream, _)) = listener.accept().await {
            let (r, mut w) = stream.into_split();
            let mut lines = BufReader::new(r).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let resp = handle_rpc(&router, &calls_srv, &line).await;
                if w.write_all(format!("{resp}\n").as_bytes()).await.is_err() {
                    break;
                }
            }
        }
    });

    let status = tokio::time::timeout(TIMEOUT, child.wait()).await;
    let timed_out = status.is_err();
    if timed_out {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
    server.abort();
    let _ = std::fs::remove_file(&sock_path);
    let stdout = out_task.await.unwrap_or_default();
    let stderr = err_task.await.unwrap_or_default();

    let exit_ok = matches!(&status, Ok(Ok(s)) if s.success());
    let n = calls.load(Ordering::SeqCst);
    let mut out = if stdout.trim().is_empty() {
        "(no stdout — remember: only what the script print()s comes back)".to_string()
    } else {
        stdout
    };
    if !exit_ok && !stderr.trim().is_empty() {
        out.push_str(&format!("\n[stderr]\n{}", stderr.trim_end()));
    }
    if timed_out {
        out.push_str("\n[execute_code: killed after 5 minutes]");
    }
    out.push_str(&format!(
        "\n[ptc: {n} tool call(s), {}]",
        if exit_ok { "exit 0" } else { "nonzero exit" }
    ));
    (out, exit_ok)
}

async fn handle_rpc(router: &ToolRouter, calls: &AtomicUsize, line: &str) -> String {
    let req: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return rpc_err(&format!("bad request json: {e}")),
    };
    let name = req["name"].as_str().unwrap_or_default();
    if !ALLOWED.contains(&name) {
        return rpc_err(&format!(
            "tool '{name}' is not callable from scripts (allowed: {})",
            ALLOWED.join(", ")
        ));
    }
    if calls.fetch_add(1, Ordering::SeqCst) >= MAX_CALLS {
        return rpc_err("tool-call budget exhausted (max 50 per script)");
    }
    let (output, ok) = router.execute(name, &req["args"]).await;
    serde_json::json!({ "ok": ok, "output": output }).to_string()
}

fn rpc_err(msg: &str) -> String {
    serde_json::json!({ "ok": false, "output": msg }).to_string()
}

/// Drain a child pipe fully (so the process never blocks on a full pipe) but
/// keep only the first `cap` bytes.
async fn read_capped<R>(pipe: Option<R>, cap: usize) -> String
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let Some(mut pipe) = pipe else {
        return String::new();
    };
    let mut kept = Vec::with_capacity(1024.min(cap));
    let mut chunk = [0u8; 8192];
    let mut truncated = false;
    loop {
        match pipe.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if kept.len() < cap {
                    let take = n.min(cap - kept.len());
                    kept.extend_from_slice(&chunk[..take]);
                    truncated = take < n;
                } else {
                    truncated = true;
                }
            }
        }
    }
    let mut s = String::from_utf8_lossy(&kept).to_string();
    if truncated {
        s.push_str("\n[output truncated]");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_ws(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("oxide-ptc-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[tokio::test]
    async fn script_stdout_round_trips() {
        let ws = tmp_ws("stdout");
        let (out, ok) = run(&ws, SandboxPolicy::WorkspaceWrite, "print('hi', 1 + 1)").await;
        assert!(ok, "{out}");
        assert!(out.contains("hi 2"), "{out}");
        std::fs::remove_dir_all(&ws).ok();
    }

    #[tokio::test]
    async fn script_calls_read_file_via_rpc() {
        let ws = tmp_ws("rpc");
        std::fs::write(ws.join("data.txt"), "alpha\nbeta\n").unwrap();
        let (out, ok) = run(
            &ws,
            SandboxPolicy::WorkspaceWrite,
            "t = oxide_call('read_file', {'path': 'data.txt'})\nprint(len(t.splitlines()), 'lines')",
        )
        .await;
        assert!(ok, "{out}");
        assert!(out.contains("lines"), "{out}");
        assert!(out.contains("1 tool call(s)"), "{out}");
        std::fs::remove_dir_all(&ws).ok();
    }

    #[tokio::test]
    async fn mutating_tools_rejected() {
        let ws = tmp_ws("deny");
        let (out, ok) = run(
            &ws,
            SandboxPolicy::WorkspaceWrite,
            "try:\n    oxide_call('write_file', {'path': 'x', 'content': 'y'})\nexcept Exception as e:\n    print('denied:', e)",
        )
        .await;
        assert!(ok, "{out}");
        assert!(out.contains("denied:"), "{out}");
        assert!(!ws.join("x").exists());
        std::fs::remove_dir_all(&ws).ok();
    }
}
