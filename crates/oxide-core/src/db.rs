//! Global session database (SQLite, WAL) — the opencode/synara model: ONE db
//! at `~/.config/oxide/oxide.db`, workspace as a column. Listing is a query
//! (never a filesystem scan), so sessions can't "disappear" when a project
//! falls out of the recents list. Legacy per-workspace JSONL files are
//! imported idempotently on first sight.

use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

fn db_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let dir = home.join(".config/oxide");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("oxide.db")
}

fn conn() -> &'static Mutex<Connection> {
    static DB: OnceLock<Mutex<Connection>> = OnceLock::new();
    DB.get_or_init(|| {
        // Unit tests must never touch the real user db.
        let c = if cfg!(test) {
            Connection::open_in_memory().expect("sqlite in-memory")
        } else {
            Connection::open(db_path()).unwrap_or_else(|_| {
                Connection::open_in_memory().expect("sqlite in-memory")
            })
        };

        let _ = c.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS sessions (
               id TEXT PRIMARY KEY,
               workspace TEXT NOT NULL,
               provider TEXT NOT NULL DEFAULT '',
               title TEXT NOT NULL DEFAULT '',
               pinned INTEGER NOT NULL DEFAULT 0,
               archived_at INTEGER,
               created_ms INTEGER NOT NULL,
               updated_ms INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_sessions_ws ON sessions(workspace, updated_ms DESC);
             CREATE TABLE IF NOT EXISTS messages (
               session_id TEXT NOT NULL,
               seq INTEGER NOT NULL,
               role TEXT NOT NULL,
               content TEXT NOT NULL,
               ts_ms INTEGER NOT NULL,
               PRIMARY KEY (session_id, seq)
             );",
        );
        // Backfill: legacy imports stamped rows with the import moment, which
        // flattened ordering/relative times. The id leads with the original
        // epoch-ms — restore created/updated from it when they disagree wildly.
        let _ = c.execute_batch(
            "UPDATE sessions SET
               created_ms = CAST(substr(id,1,13) AS INTEGER),
               updated_ms = CAST(substr(id,1,13) AS INTEGER)
             WHERE length(id) >= 13
               AND substr(id,1,13) GLOB '[0-9]*'
               AND ABS(created_ms - CAST(substr(id,1,13) AS INTEGER)) > 60000;",
        );
        Mutex::new(c)
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Session metadata row for listings.
#[derive(Debug, Clone)]
pub struct SessionMeta {
    pub id: String,
    pub workspace: String,
    pub provider: String,
    pub title: String,
    pub pinned: bool,
    pub updated_ms: i64,
}

/// Mint a fresh session id (row is created lazily on the first message).
pub fn new_id() -> String {
    format!("{}-{}", now_ms(), std::process::id())
}

/// True if the session row exists.
pub fn exists(id: &str) -> bool {
    let c = conn().lock().unwrap();
    c.query_row("SELECT 1 FROM sessions WHERE id=?1", [id], |_| Ok(()))
        .is_ok()
}

/// Append one message; creates the session row on first use (lazy, so an
/// empty chat never leaves anything behind).
pub fn append(id: &str, workspace: &Path, provider: &str, role: &str, content: &str) {
    // Never record throwaway workspaces (test temp dirs) in the global db.
    let wss = workspace.to_string_lossy();
    let throwaway = wss.starts_with("/var/folders/") || wss.starts_with("/tmp/") || std::env::var_os("OXIDE_NO_DB").is_some();
    if throwaway && !cfg!(test) {
        return;
    }
    let c = conn().lock().unwrap();
    let t = now_ms();
    let ws = workspace.display().to_string();
    let _ = c.execute(
        "INSERT INTO sessions (id, workspace, provider, title, created_ms, updated_ms)
         VALUES (?1, ?2, ?3, '', ?4, ?4)
         ON CONFLICT(id) DO UPDATE SET updated_ms=?4",
        rusqlite::params![id, ws, provider, t],
    );
    let _ = c.execute(
        "INSERT INTO messages (session_id, seq, role, content, ts_ms)
         VALUES (?1, COALESCE((SELECT MAX(seq)+1 FROM messages WHERE session_id=?1), 0), ?2, ?3, ?4)",
        rusqlite::params![id, role, content, t],
    );
    // First user line becomes the title.
    if role == "user" {
        let first = content.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        let title: String = first.chars().take(60).collect();
        let _ = c.execute(
            "UPDATE sessions SET title=?2 WHERE id=?1 AND title=''",
            rusqlite::params![id, title],
        );
    }
}

/// Update the provider stamp (model/provider switch on a live session).
pub fn set_provider(id: &str, provider: &str) {
    let c = conn().lock().unwrap();
    let _ = c.execute(
        "UPDATE sessions SET provider=?2 WHERE id=?1",
        rusqlite::params![id, provider],
    );
}

/// Load every message (role, content) in order.
pub fn load(id: &str) -> Vec<(String, String)> {
    let c = conn().lock().unwrap();
    let mut out = Vec::new();
    if let Ok(mut st) =
        c.prepare("SELECT role, content FROM messages WHERE session_id=?1 ORDER BY seq")
    {
        if let Ok(rows) = st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?))) {
            for row in rows.flatten() {
                out.push(row);
            }
        }
    }
    out
}

/// Replace the whole conversation (restore-to-message).
pub fn rewrite(id: &str, workspace: &Path, provider: &str, msgs: &[(String, String)]) {
    {
        let c = conn().lock().unwrap();
        let _ = c.execute("DELETE FROM messages WHERE session_id=?1", [id]);
    }
    for (role, content) in msgs {
        append(id, workspace, provider, role, content);
    }
    if msgs.is_empty() {
        // Nothing left — drop the row so it doesn't linger as an empty chat.
        let c = conn().lock().unwrap();
        let _ = c.execute("DELETE FROM sessions WHERE id=?1", [id]);
    }
}

/// Sessions of one workspace, newest first (active only).
pub fn list(workspace: &Path, limit: usize) -> Vec<SessionMeta> {
    list_where(
        "workspace=?1 AND archived_at IS NULL",
        rusqlite::params![workspace.display().to_string()],
        limit,
    )
}

/// Every workspace that has sessions, by recency.
pub fn workspaces() -> Vec<String> {
    let c = conn().lock().unwrap();
    let mut out = Vec::new();
    if let Ok(mut st) = c.prepare(
        "SELECT workspace, MAX(updated_ms) m FROM sessions WHERE archived_at IS NULL
         GROUP BY workspace ORDER BY m DESC LIMIT 50",
    ) {
        if let Ok(rows) = st.query_map([], |r| r.get::<_, String>(0)) {
            for row in rows.flatten() {
                out.push(row);
            }
        }
    }
    out
}

/// Title search across ALL workspaces (palette).
pub fn search(q: &str, limit: usize) -> Vec<SessionMeta> {
    let pat = format!("%{}%", q.replace('%', ""));
    list_where(
        "archived_at IS NULL AND title LIKE ?1",
        rusqlite::params![pat],
        limit,
    )
}

fn list_where(cond: &str, params: impl rusqlite::Params, limit: usize) -> Vec<SessionMeta> {
    let c = conn().lock().unwrap();
    let sql = format!(
        "SELECT id, workspace, provider, title, pinned, updated_ms FROM sessions
         WHERE {cond} ORDER BY pinned DESC, updated_ms DESC LIMIT {limit}"
    );
    let mut out = Vec::new();
    if let Ok(mut st) = c.prepare(&sql) {
        if let Ok(rows) = st.query_map(params, |r| {
            Ok(SessionMeta {
                id: r.get(0)?,
                workspace: r.get(1)?,
                provider: r.get(2)?,
                title: r.get(3)?,
                pinned: r.get::<_, i64>(4)? != 0,
                updated_ms: r.get(5)?,
            })
        }) {
            for row in rows.flatten() {
                out.push(row);
            }
        }
    }
    out
}

/// Metadata of one session.
pub fn meta(id: &str) -> Option<SessionMeta> {
    list_where("id=?1", rusqlite::params![id], 1).into_iter().next()
}

/// Newest active session in a workspace.
pub fn latest(workspace: &Path) -> Option<String> {
    list(workspace, 1).into_iter().next().map(|m| m.id)
}

pub fn set_pinned(id: &str, pinned: bool) {
    let c = conn().lock().unwrap();
    let _ = c.execute(
        "UPDATE sessions SET pinned=?2 WHERE id=?1",
        rusqlite::params![id, pinned as i64],
    );
}

/// Pinned sessions across all workspaces.
pub fn pinned() -> Vec<SessionMeta> {
    list_where("archived_at IS NULL AND pinned=1", [], 50)
}

pub fn archive(id: &str) {
    let c = conn().lock().unwrap();
    let _ = c.execute(
        "UPDATE sessions SET archived_at=?2 WHERE id=?1",
        rusqlite::params![id, now_ms()],
    );
}

pub fn delete(id: &str) {
    let c = conn().lock().unwrap();
    let _ = c.execute("DELETE FROM messages WHERE session_id=?1", [id]);
    let _ = c.execute("DELETE FROM sessions WHERE id=?1", [id]);
}

/// Import legacy per-workspace JSONL sessions (idempotent — skips ids that are
/// already in the DB). Files are left in place, renamed to `.imported`.
pub fn import_workspace(ws: &Path) {
    // Once per workspace per process — this is called from list paths that can
    // run per render; the dir scan must not repeat.
    static DONE: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    {
        let mut g = DONE.get_or_init(Default::default).lock().unwrap();
        if !g.insert(ws.display().to_string()) {
            return;
        }
    }
    let dir = ws.join(".oxide/sessions");
    let Ok(rd) = std::fs::read_dir(&dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        let id = p
            .file_stem()
            .and_then(|x| x.to_str())
            .unwrap_or_default()
            .to_string();
        if id.is_empty() || exists(&id) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        let mut provider = String::new();
        let mut msgs: Vec<(String, String)> = Vec::new();
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
            let role = v["role"].as_str().unwrap_or("");
            let content = v["content"].as_str().unwrap_or("");
            if role == "meta" {
                if let Some(pv) = content.strip_prefix("provider=") {
                    provider = pv.to_string();
                }
            } else if !role.is_empty() {
                msgs.push((role.to_string(), content.to_string()));
            }
        }
        if msgs.is_empty() {
            let _ = std::fs::remove_file(&p);
            continue;
        }
        for (role, content) in &msgs {
            append(&id, ws, &provider, role, content);
        }
        // Preserve the ORIGINAL creation time (id prefix), not the import moment.
        if id.len() >= 13 {
            if let Ok(ms) = id[..13].parse::<i64>() {
                let c = conn().lock().unwrap();
                let _ = c.execute(
                    "UPDATE sessions SET created_ms=?2, updated_ms=?2 WHERE id=?1",
                    rusqlite::params![id, ms],
                );
            }
        }
        let _ = std::fs::rename(&p, p.with_extension("jsonl.imported"));
    }
}
