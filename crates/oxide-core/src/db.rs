//! Global session database (SQLite, WAL) — the opencode/synara model: ONE db
//! at `~/.config/oxide/oxide.db`, workspace as a column. Listing is a query
//! (never a filesystem scan), so sessions can't "disappear" when a project
//! falls out of the recents list. Legacy per-workspace JSONL files are
//! imported idempotently on first sight.

use rusqlite::{Connection, OpenFlags};
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
               cli_session_id TEXT,
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
             );
             CREATE TABLE IF NOT EXISTS session_tombstones (
               id TEXT PRIMARY KEY,
               deleted_at INTEGER NOT NULL
             );",
        );
        // Migration for existing dbs (errors harmlessly if the column is there).
        let _ = c.execute("ALTER TABLE sessions ADD COLUMN cli_session_id TEXT", []);
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

fn is_throwaway_workspace(workspace: &str) -> bool {
    workspace.starts_with("/var/folders/")
        || workspace.starts_with("/private/var/folders/")
        || workspace.starts_with("/tmp/")
        || workspace.starts_with("/private/tmp/")
}

fn clean_imported_title(title: &str) -> String {
    title
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("Codex session")
        .chars()
        .take(60)
        .collect()
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
    let throwaway = is_throwaway_workspace(wss.as_ref()) || std::env::var_os("OXIDE_NO_DB").is_some();
    if throwaway && !cfg!(test) {
        return;
    }
    let c = conn().lock().unwrap();
    let t = now_ms();
    let ws = workspace.display().to_string();
    let _ = c.execute("DELETE FROM session_tombstones WHERE id=?1", [id]);
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
        let first = content
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty() && !l.starts_with("Context files") && !l.starts_with('[') && !l.starts_with('@') && !l.starts_with("<system-reminder>"))
            .unwrap_or_else(|| content.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim());
        let title: String = first.chars().take(60).collect();
        let _ = c.execute(
            "UPDATE sessions SET title=?2 WHERE id=?1 AND title=''",
            rusqlite::params![id, title],
        );
    }
}

/// Update the provider stamp (model/provider switch on a live session).
/// Overwrite a session title (LLM-generated summary, or a cleaned first line).
pub fn set_title(id: &str, title: &str) {
    let t: String = title.trim().chars().take(60).collect();
    if t.is_empty() { return; }
    let c = conn().lock().unwrap();
    let _ = c.execute("UPDATE sessions SET title=?2 WHERE id=?1", rusqlite::params![id, t]);
}

/// Current title (empty if unset).
pub fn title_of(id: &str) -> String {
    meta(id).map(|m| m.title).unwrap_or_default()
}

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

/// Count user-visible messages without materializing the whole transcript.
pub fn message_count(id: &str) -> usize {
    let c = conn().lock().unwrap();
    c.query_row(
        "SELECT COUNT(*) FROM messages
         WHERE session_id=?1 AND role NOT IN ('meta', 'tool', 'system', 'event', 'summary')",
        [id],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n.max(0) as usize)
    .unwrap_or(0)
}

/// Replace the whole conversation (restore-to-message).
pub fn rewrite(id: &str, workspace: &Path, provider: &str, msgs: &[(String, String)]) {
    {
        let c = conn().lock().unwrap();
        let _ = c.execute("DELETE FROM session_tombstones WHERE id=?1", [id]);
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

/// Workspaces that Oxide itself has touched. Imported Codex Desktop rows do not
/// count; otherwise merely reading Codex history would populate unrelated
/// folders in the sidebar.
pub fn workspaces_opened_by_oxide() -> Vec<String> {
    let c = conn().lock().unwrap();
    let mut out = Vec::new();
    if let Ok(mut st) = c.prepare(
        "SELECT workspace, MAX(updated_ms) m FROM sessions
         WHERE archived_at IS NULL AND id NOT LIKE 'codex:%'
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

/// Archive every session of a workspace (removes them from the sidebar).
pub fn archive_workspace(workspace: &Path) {
    let c = conn().lock().unwrap();
    let _ = c.execute(
        "UPDATE sessions SET archived_at=?2 WHERE workspace=?1 AND archived_at IS NULL",
        rusqlite::params![workspace.display().to_string(), now_ms()],
    );
}

pub fn archive(id: &str) {
    let c = conn().lock().unwrap();
    let _ = c.execute(
        "UPDATE sessions SET archived_at=?2 WHERE id=?1",
        rusqlite::params![id, now_ms()],
    );
}

pub fn restore(id: &str) {
    let c = conn().lock().unwrap();
    let _ = c.execute("DELETE FROM session_tombstones WHERE id=?1", [id]);
    let _ = c.execute(
        "UPDATE sessions SET archived_at=NULL WHERE id=?1",
        rusqlite::params![id],
    );
}

/// Every archived session across all workspaces (for the restore manager in
/// Settings), most-recently-updated first.
pub fn list_archived() -> Vec<SessionMeta> {
    list_where("archived_at IS NOT NULL", [], 500)
}

/// Persist the provider's native CLI session id (codex thread / claude uuid) for
/// this Oxide session, so a resume after an app restart can hand the CLI back
/// its own session via `--resume` instead of starting a fresh one.
pub fn set_cli_session(id: &str, cli_session_id: &str) {
    let c = conn().lock().unwrap();
    let _ = c.execute(
        "UPDATE sessions SET cli_session_id=?2 WHERE id=?1",
        rusqlite::params![id, cli_session_id],
    );
}

/// The stored native CLI session id for this Oxide session, if any.
pub fn cli_session(id: &str) -> Option<String> {
    let c = conn().lock().unwrap();
    c.query_row(
        "SELECT cli_session_id FROM sessions WHERE id=?1",
        [id],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

pub fn delete(id: &str) {
    let c = conn().lock().unwrap();
    let _ = c.execute(
        "INSERT INTO session_tombstones (id, deleted_at) VALUES (?1, ?2)
         ON CONFLICT(id) DO UPDATE SET deleted_at=excluded.deleted_at",
        rusqlite::params![id, now_ms()],
    );
    let _ = c.execute("DELETE FROM messages WHERE session_id=?1", [id]);
    let _ = c.execute("DELETE FROM sessions WHERE id=?1", [id]);
}

/// Import Codex Desktop thread metadata from its local state db. This is
/// read-only against Codex's db; Oxide stores only a lightweight row with the
/// native Codex thread id in `cli_session_id`, so opening it can resume via the
/// existing Codex CLI session.
pub fn import_codex_desktop_threads(limit: usize) {
    let workspaces: Vec<PathBuf> = workspaces_opened_by_oxide()
        .into_iter()
        .map(PathBuf::from)
        .collect();
    import_codex_desktop_threads_for_workspaces(workspaces, limit);
}

pub fn import_codex_desktop_threads_for_workspaces<I, P>(workspaces: I, limit: usize)
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    static LAST: OnceLock<Mutex<Option<std::time::Instant>>> = OnceLock::new();
    {
        let mut g = LAST.get_or_init(Default::default).lock().unwrap();
        if let Some(t) = *g {
            if t.elapsed() < std::time::Duration::from_secs(5) { return; }
        }
        *g = Some(std::time::Instant::now());
    }

    let allowed: std::collections::HashSet<String> = workspaces
        .into_iter()
        .map(|p| p.as_ref().display().to_string())
        .filter(|p| !p.is_empty() && !is_throwaway_workspace(p))
        .collect();
    if allowed.is_empty() {
        return;
    }

    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return };
    let path = home.join(".codex/state_5.sqlite");
    import_codex_desktop_threads_from(&path, &allowed, limit);
}

fn import_codex_desktop_threads_from(
    path: &Path,
    allowed: &std::collections::HashSet<String>,
    limit: usize,
) {
    if !path.exists() { return; }
    let Ok(codex) = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) else { return };
    let _ = codex.busy_timeout(std::time::Duration::from_millis(250));

    let sql = "
        SELECT id, cwd, title,
               COALESCE(created_at_ms, created_at * 1000),
               COALESCE(updated_at_ms, updated_at * 1000)
        FROM threads
        WHERE archived = 0
          AND cwd <> ''
          AND title <> ''
          AND source NOT LIKE '%subagent%'
        ORDER BY COALESCE(updated_at_ms, updated_at * 1000) DESC
        LIMIT ?1";
    let Ok(mut st) = codex.prepare(sql) else { return };
    let Ok(rows) = st.query_map(rusqlite::params![limit as i64], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, i64>(4)?,
        ))
    }) else { return };

    let c = conn().lock().unwrap();
    for row in rows.flatten() {
        let (native_id, workspace, title, created_ms, updated_ms) = row;
        if native_id.trim().is_empty()
            || workspace.trim().is_empty()
            || is_throwaway_workspace(&workspace)
            || !allowed.contains(&workspace)
        {
            continue;
        }
        let id = format!("codex:{native_id}");
        let tombstoned = c
            .query_row(
                "SELECT 1 FROM session_tombstones WHERE id=?1",
                [&id],
                |_| Ok(()),
            )
            .is_ok();
        if tombstoned {
            continue;
        }
        let title = clean_imported_title(&title);
        let created_ms = created_ms.max(0);
        let updated_ms = updated_ms.max(created_ms);
        let _ = c.execute(
            "INSERT INTO sessions (id, workspace, provider, title, cli_session_id, created_ms, updated_ms)
             VALUES (?1, ?2, 'codex', ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
               workspace=excluded.workspace,
               provider='codex',
               title=excluded.title,
               cli_session_id=excluded.cli_session_id,
               updated_ms=MAX(sessions.updated_ms, excluded.updated_ms)",
            rusqlite::params![id, workspace, title, native_id, created_ms, updated_ms],
        );
    }
}

/// Import Claude Code CLI (TUI) transcripts for a workspace into the global db,
/// so TUI conversations show up and persist like normal chats. Claude stores
/// them at ~/.claude/projects/<slug>/<uuid>.jsonl (slug = cwd with '/'→'-').
/// Re-imported each call (claude appends live) — cheap, keyed by a stable id.
pub fn import_claude_sessions(workspace: &Path) {
    // Throttle: re-scan a workspace's claude dir at most every 5s.
    static LAST: OnceLock<Mutex<std::collections::HashMap<String, std::time::Instant>>> = OnceLock::new();
    {
        let mut g = LAST.get_or_init(Default::default).lock().unwrap();
        let key = workspace.display().to_string();
        if let Some(t) = g.get(&key) {
            if t.elapsed() < std::time::Duration::from_secs(5) { return; }
        }
        g.insert(key, std::time::Instant::now());
    }
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return };
    let slug = workspace.display().to_string().replace('/', "-").replace('.', "-");
    let dir = home.join(".claude/projects").join(&slug);
    let Ok(rd) = std::fs::read_dir(&dir) else { return };
    for e in rd.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("jsonl") { continue; }
        let stem = path.file_stem().and_then(|x| x.to_str()).unwrap_or("");
        if stem.is_empty() { continue; }
        let id = format!("claude-{stem}");
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let mut msgs: Vec<(String, String)> = Vec::new();
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
            let role = match v["type"].as_str() {
                Some("user") => "user",
                Some("assistant") => "assistant",
                _ => continue,
            };
            let content = match &v["message"]["content"] {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Array(a) => a.iter()
                    .filter_map(|x| if x["type"] == "text" { x["text"].as_str() } else { None })
                    .collect::<Vec<_>>().join("\n"),
                _ => String::new(),
            };
            let content = content.trim().to_string();
            if !content.is_empty() {
                msgs.push((role.to_string(), content));
            }
        }
        if msgs.is_empty() { continue; }
        // Only rewrite when the message count changed (claude appended).
        let existing = load(&id).len();
        if existing == msgs.len() { continue; }
        rewrite(&id, workspace, "claude", &msgs);
        // Preserve order by file mtime.
        if let Ok(meta) = std::fs::metadata(&path) {
            if let Ok(mt) = meta.modified() {
                if let Ok(d) = mt.duration_since(std::time::UNIX_EPOCH) {
                    let ms = d.as_millis() as i64;
                    let c = conn().lock().unwrap();
                    let _ = c.execute("UPDATE sessions SET created_ms=?2, updated_ms=?2 WHERE id=?1", rusqlite::params![id, ms]);
                }
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_codex_desktop_threads_as_resumable_sessions() {
        let path = std::env::temp_dir().join(format!(
            "oxide-codex-state-{}-{}.sqlite",
            std::process::id(),
            now_ms()
        ));
        let _ = std::fs::remove_file(&path);
        let codex = Connection::open(&path).expect("open temp codex db");
        codex.execute_batch(
            "CREATE TABLE threads (
               id TEXT NOT NULL,
               cwd TEXT NOT NULL,
               title TEXT NOT NULL,
               created_at INTEGER NOT NULL,
               updated_at INTEGER NOT NULL,
               created_at_ms INTEGER,
               updated_at_ms INTEGER,
               archived INTEGER NOT NULL,
               source TEXT NOT NULL
             );",
        ).expect("create threads table");
        codex.execute(
            "INSERT INTO threads (id, cwd, title, created_at, updated_at, created_at_ms, updated_at_ms, archived, source)
             VALUES (?1, ?2, ?3, 10, 20, 10000, 20000, 0, 'vscode')",
            rusqlite::params![
                "native-thread-1",
                "/Volumes/Data/oxide-test-import",
                "  Read README\nextra",
            ],
        ).expect("insert codex thread");
        codex.execute(
            "INSERT INTO threads (id, cwd, title, created_at, updated_at, created_at_ms, updated_at_ms, archived, source)
             VALUES (?1, ?2, ?3, 10, 20, 10000, 20000, 0, 'vscode')",
            rusqlite::params!["tmp-thread", "/private/var/folders/tmp-project", "Tmp"],
        ).expect("insert temp thread");
        codex.execute(
            "INSERT INTO threads (id, cwd, title, created_at, updated_at, created_at_ms, updated_at_ms, archived, source)
             VALUES (?1, ?2, ?3, 10, 20, 10000, 20000, 0, 'vscode')",
            rusqlite::params!["other-thread", "/Volumes/Data/unopened-by-oxide", "Unopened"],
        ).expect("insert unopened thread");
        drop(codex);

        let allowed =
            std::collections::HashSet::from(["/Volumes/Data/oxide-test-import".to_string()]);
        import_codex_desktop_threads_from(&path, &allowed, 10);

        let sessions = list(Path::new("/Volumes/Data/oxide-test-import"), 10);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "codex:native-thread-1");
        assert_eq!(sessions[0].provider, "codex");
        assert_eq!(sessions[0].title, "Read README");
        assert_eq!(cli_session(&sessions[0].id).as_deref(), Some("native-thread-1"));
        assert!(list(Path::new("/private/var/folders/tmp-project"), 10).is_empty());
        assert!(list(Path::new("/Volumes/Data/unopened-by-oxide"), 10).is_empty());

        archive("codex:native-thread-1");
        import_codex_desktop_threads_from(&path, &allowed, 10);
        assert!(list(Path::new("/Volumes/Data/oxide-test-import"), 10).is_empty());

        restore("codex:native-thread-1");
        assert_eq!(list(Path::new("/Volumes/Data/oxide-test-import"), 10).len(), 1);

        delete("codex:native-thread-1");
        import_codex_desktop_threads_from(&path, &allowed, 10);
        assert!(list(Path::new("/Volumes/Data/oxide-test-import"), 10).is_empty());

        let _ = std::fs::remove_file(&path);
    }
}
