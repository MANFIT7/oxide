//! Global session database (SQLite, WAL) — the opencode/synara model: ONE db
//! at `~/.config/oxide/oxide.db`, workspace as a column. Listing is a query
//! (never a filesystem scan), so sessions can't "disappear" when a project
//! falls out of the recents list. Legacy per-workspace JSONL files are
//! imported idempotently on first sight.

use rusqlite::{Connection as SqliteConnection, OpenFlags};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Mutex, OnceLock};
use turso::{Connection as TursoConnection, IntoParams, Row};

fn db_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let dir = home.join(".config/oxide");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("oxide.db")
}

type DbJob = Box<dyn FnOnce(&mut LocalDb) + Send + 'static>;

struct DbWorker {
    tx: mpsc::Sender<DbJob>,
}

struct LocalDb {
    rt: tokio::runtime::Runtime,
    _database: turso::Database,
    conn: TursoConnection,
}

impl LocalDb {
    fn open(path: &str) -> Result<Self, String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| format!("build Turso runtime: {err}"))?;
        let database = rt
            .block_on(turso::Builder::new_local(path).build())
            .map_err(|err| format!("open Turso local database at {path}: {err}"))?;
        let conn = database
            .connect()
            .map_err(|err| format!("connect Turso local database at {path}: {err}"))?;
        let mut db = Self {
            rt,
            _database: database,
            conn,
        };
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&mut self) -> Result<(), String> {
        // Best-effort compatibility with the previous SQLite file. Turso may
        // ignore or reject a pragma as the engine evolves, but schema creation
        // below is the required part.
        let _ = self.try_execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;");
        self.try_execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
               id TEXT PRIMARY KEY,
               workspace TEXT NOT NULL,
               provider TEXT NOT NULL DEFAULT '',
               model TEXT NOT NULL DEFAULT '',
               harness TEXT NOT NULL DEFAULT '',
               reasoning_effort TEXT NOT NULL DEFAULT '',
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
        )?;
        // Migration for existing dbs (errors harmlessly if the column is there).
        let _ = self.rt.block_on(
            self.conn
                .execute("ALTER TABLE sessions ADD COLUMN cli_session_id TEXT", ()),
        );
        for sql in [
            "ALTER TABLE sessions ADD COLUMN model TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE sessions ADD COLUMN harness TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE sessions ADD COLUMN reasoning_effort TEXT NOT NULL DEFAULT ''",
        ] {
            let _ = self.rt.block_on(self.conn.execute(sql, ()));
        }
        // Backfill: legacy imports stamped rows with the import moment, which
        // flattened ordering/relative times. The id leads with the original
        // epoch-ms — restore created/updated from it when they disagree wildly.
        self.execute_batch(
            "backfill session timestamps",
            "UPDATE sessions SET
               created_ms = CAST(substr(id,1,13) AS INTEGER),
               updated_ms = CAST(substr(id,1,13) AS INTEGER)
             WHERE length(id) >= 13
               AND substr(id,1,13) GLOB '[0-9]*'
               AND ABS(created_ms - CAST(substr(id,1,13) AS INTEGER)) > 60000;",
        );
        Ok(())
    }

    fn try_execute_batch(&mut self, sql: &str) -> Result<(), String> {
        self.rt
            .block_on(self.conn.execute_batch(sql))
            .map_err(|err| format!("execute Turso batch: {err}"))
    }

    fn execute_batch(&mut self, op: &str, sql: &str) {
        if let Err(err) = self.try_execute_batch(sql) {
            tracing::warn!(operation = op, error = %err, "Turso database operation failed");
        }
    }

    fn execute<P>(&mut self, op: &str, sql: &str, params: P)
    where
        P: IntoParams,
    {
        if let Err(err) = self.rt.block_on(self.conn.execute(sql, params)) {
            tracing::warn!(operation = op, error = %err, "Turso database operation failed");
        }
    }

    fn query_map<P, T, F>(&mut self, op: &str, sql: &str, params: P, mut map: F) -> Vec<T>
    where
        P: IntoParams,
        F: FnMut(&Row) -> turso::Result<T>,
    {
        let result = self.rt.block_on(async {
            let mut rows = self.conn.query(sql, params).await?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().await? {
                out.push(map(&row)?);
            }
            Ok::<Vec<T>, turso::Error>(out)
        });
        match result {
            Ok(out) => out,
            Err(err) => {
                tracing::warn!(operation = op, error = %err, "Turso database query failed");
                Vec::new()
            }
        }
    }

    fn query_one<P, T, F>(&mut self, op: &str, sql: &str, params: P, map: F) -> Option<T>
    where
        P: IntoParams,
        F: FnMut(&Row) -> turso::Result<T>,
    {
        self.query_map(op, sql, params, map).into_iter().next()
    }
}

fn worker() -> &'static DbWorker {
    static DB: OnceLock<DbWorker> = OnceLock::new();
    DB.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<DbJob>();
        std::thread::Builder::new()
            .name("oxide-turso-db".to_string())
            .spawn(move || {
                // Unit tests must never touch the real user db.
                let path = if cfg!(test) {
                    ":memory:".to_string()
                } else {
                    db_path().display().to_string()
                };
                let mut db = LocalDb::open(&path)
                    .or_else(|err| {
                        tracing::warn!(
                            path = %path,
                            error = %err,
                            "Falling back to in-memory Turso session database"
                        );
                        LocalDb::open(":memory:")
                    })
                    .expect("Turso in-memory database");
                for job in rx {
                    job(&mut db);
                }
            })
            .expect("spawn Turso database worker");
        DbWorker { tx }
    })
}

fn with_db<T, F>(f: F) -> T
where
    T: Send + 'static,
    F: FnOnce(&mut LocalDb) -> T + Send + 'static,
{
    let (tx, rx) = mpsc::sync_channel(1);
    worker()
        .tx
        .send(Box::new(move |db| {
            let _ = tx.send(f(db));
        }))
        .expect("send Turso database job");
    rx.recv().expect("receive Turso database job result")
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
    pub model: String,
    pub harness: String,
    pub reasoning_effort: String,
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
    let id = id.to_string();
    with_db(move |db| {
        db.query_one(
            "check session exists",
            "SELECT 1 FROM sessions WHERE id=?1",
            turso::params![id],
            |r| r.get::<i64>(0),
        )
        .is_some()
    })
}

fn title_from_user_content(content: &str) -> String {
    let first = content
        .lines()
        .map(str::trim)
        .find(|line| {
            !line.is_empty()
                && !line.starts_with("Context files")
                && !line.starts_with('[')
                && !line.starts_with('@')
                && !line.starts_with("<system-reminder>")
        })
        .unwrap_or_else(|| {
            content
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("")
                .trim()
        });
    first.chars().take(60).collect()
}

#[allow(clippy::too_many_arguments)]
fn append_in_db(
    db: &mut LocalDb,
    id: &str,
    workspace: &str,
    provider: &str,
    model: &str,
    harness: &str,
    reasoning_effort: &str,
    role: &str,
    content: &str,
    ts_ms: i64,
) {
    db.execute(
        "clear session tombstone",
        "DELETE FROM session_tombstones WHERE id=?1",
        turso::params![id],
    );
    db.execute(
        "upsert session",
        "INSERT INTO sessions (id, workspace, provider, model, harness, reasoning_effort, title, created_ms, updated_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', ?7, ?7)
         ON CONFLICT(id) DO UPDATE SET
           updated_ms=?7,
           provider=?3,
           model=?4,
           harness=?5,
           reasoning_effort=?6",
        turso::params![
            id,
            workspace,
            provider,
            model,
            harness,
            reasoning_effort,
            ts_ms
        ],
    );
    db.execute(
        "append session message",
        "INSERT INTO messages (session_id, seq, role, content, ts_ms)
         VALUES (?1, COALESCE((SELECT MAX(seq)+1 FROM messages WHERE session_id=?1), 0), ?2, ?3, ?4)",
        turso::params![id, role, content, ts_ms],
    );
    if role == "user" {
        let title = title_from_user_content(content);
        db.execute(
            "set first session title",
            "UPDATE sessions SET title=?2 WHERE id=?1 AND title=''",
            turso::params![id, title],
        );
    }
}

/// Append one message; creates the session row on first use (lazy, so an
/// empty chat never leaves anything behind).
pub fn append(id: &str, workspace: &Path, provider: &str, role: &str, content: &str) {
    append_with_config(id, workspace, provider, "", "", "", role, content);
}

#[allow(clippy::too_many_arguments)]
pub fn append_with_config(
    id: &str,
    workspace: &Path,
    provider: &str,
    model: &str,
    harness: &str,
    reasoning_effort: &str,
    role: &str,
    content: &str,
) {
    // Never record throwaway workspaces (test temp dirs) in the global db.
    let wss = workspace.to_string_lossy();
    let throwaway =
        is_throwaway_workspace(wss.as_ref()) || std::env::var_os("OXIDE_NO_DB").is_some();
    if throwaway && !cfg!(test) {
        return;
    }
    let id = id.to_string();
    let ws = workspace.display().to_string();
    let provider = provider.to_string();
    let model = model.to_string();
    let harness = harness.to_string();
    let reasoning_effort = reasoning_effort.to_string();
    let role = role.to_string();
    let content = content.to_string();
    let t = now_ms();
    with_db(move |db| {
        append_in_db(
            db,
            &id,
            &ws,
            &provider,
            &model,
            &harness,
            &reasoning_effort,
            &role,
            &content,
            t,
        )
    });
}

/// Update the provider stamp (model/provider switch on a live session).
/// Overwrite a session title (LLM-generated summary, or a cleaned first line).
pub fn set_title(id: &str, title: &str) {
    let t: String = title.trim().chars().take(60).collect();
    if t.is_empty() {
        return;
    }
    let id = id.to_string();
    with_db(move |db| {
        db.execute(
            "set session title",
            "UPDATE sessions SET title=?2 WHERE id=?1",
            turso::params![id, t],
        );
    });
}

/// Current title (empty if unset).
pub fn title_of(id: &str) -> String {
    meta(id).map(|m| m.title).unwrap_or_default()
}

pub fn set_provider(id: &str, provider: &str) {
    set_session_config(id, provider, "", "", "");
}

pub fn set_session_config(
    id: &str,
    provider: &str,
    model: &str,
    harness: &str,
    reasoning_effort: &str,
) {
    let id = id.to_string();
    let provider = provider.to_string();
    let model = model.to_string();
    let harness = harness.to_string();
    let reasoning_effort = reasoning_effort.to_string();
    with_db(move |db| {
        db.execute(
            "set session runtime config",
            "UPDATE sessions SET provider=?2, model=?3, harness=?4, reasoning_effort=?5 WHERE id=?1",
            turso::params![id, provider, model, harness, reasoning_effort],
        );
    });
}

/// Load every message (role, content) in order.
pub fn load(id: &str) -> Vec<(String, String)> {
    let id = id.to_string();
    with_db(move |db| {
        db.query_map(
            "load session messages",
            "SELECT role, content FROM messages WHERE session_id=?1 ORDER BY seq",
            turso::params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    })
}

/// Count user-visible messages without materializing the whole transcript.
pub fn message_count(id: &str) -> usize {
    let id = id.to_string();
    with_db(move |db| {
        db.query_one(
            "count session messages",
            "SELECT COUNT(*) FROM messages
             WHERE session_id=?1 AND role NOT IN ('meta', 'tool', 'system', 'event', 'summary')",
            turso::params![id],
            |r| r.get::<i64>(0),
        )
        .map(|n| n.max(0) as usize)
        .unwrap_or(0)
    })
}

/// Replace the whole conversation (restore-to-message).
pub fn rewrite(id: &str, workspace: &Path, provider: &str, msgs: &[(String, String)]) {
    rewrite_with_config(id, workspace, provider, "", "", "", msgs);
}

pub fn rewrite_with_config(
    id: &str,
    workspace: &Path,
    provider: &str,
    model: &str,
    harness: &str,
    reasoning_effort: &str,
    msgs: &[(String, String)],
) {
    let id = id.to_string();
    let ws = workspace.display().to_string();
    let provider = provider.to_string();
    let model = model.to_string();
    let harness = harness.to_string();
    let reasoning_effort = reasoning_effort.to_string();
    let msgs = msgs.to_vec();
    with_db(move |db| {
        db.execute(
            "clear session tombstone",
            "DELETE FROM session_tombstones WHERE id=?1",
            turso::params![id.as_str()],
        );
        db.execute(
            "delete session messages",
            "DELETE FROM messages WHERE session_id=?1",
            turso::params![id.as_str()],
        );
        let base_ts = now_ms();
        for (idx, (role, content)) in msgs.iter().enumerate() {
            append_in_db(
                db,
                &id,
                &ws,
                &provider,
                &model,
                &harness,
                &reasoning_effort,
                role,
                content,
                base_ts + idx as i64,
            );
        }
        if msgs.is_empty() {
            // Nothing left — drop the row so it doesn't linger as an empty chat.
            db.execute(
                "delete empty session",
                "DELETE FROM sessions WHERE id=?1",
                turso::params![id],
            );
        }
    });
}

/// Sessions of one workspace, newest first (active only).
pub fn list(workspace: &Path, limit: usize) -> Vec<SessionMeta> {
    list_where(
        "workspace=?1 AND archived_at IS NULL",
        turso::params![workspace.display().to_string()],
        limit,
    )
}

/// Every workspace that has sessions, by recency.
pub fn workspaces() -> Vec<String> {
    with_db(move |db| {
        db.query_map(
            "list workspaces",
            "SELECT workspace, MAX(updated_ms) m FROM sessions WHERE archived_at IS NULL
             GROUP BY workspace ORDER BY m DESC LIMIT 50",
            (),
            |r| r.get::<String>(0),
        )
    })
}

/// Workspaces that Oxide itself has touched. Imported Codex Desktop rows do not
/// count; otherwise merely reading Codex history would populate unrelated
/// folders in the sidebar.
pub fn workspaces_opened_by_oxide() -> Vec<String> {
    with_db(move |db| {
        db.query_map(
            "list Oxide workspaces",
            "SELECT workspace, MAX(updated_ms) m FROM sessions
             WHERE archived_at IS NULL AND id NOT LIKE 'codex:%'
             GROUP BY workspace ORDER BY m DESC LIMIT 50",
            (),
            |r| r.get::<String>(0),
        )
    })
}

/// Title search across ALL workspaces (palette).
pub fn search(q: &str, limit: usize) -> Vec<SessionMeta> {
    let pat = format!("%{}%", q.replace('%', ""));
    list_where(
        "archived_at IS NULL AND title LIKE ?1",
        turso::params![pat],
        limit,
    )
}

fn list_where<P>(cond: &str, params: P, limit: usize) -> Vec<SessionMeta>
where
    P: IntoParams + Send + 'static,
{
    let cond = cond.to_string();
    let sql = format!(
        "SELECT id, workspace, provider, model, harness, reasoning_effort, title, pinned, updated_ms FROM sessions
         WHERE {cond} ORDER BY pinned DESC, updated_ms DESC LIMIT {limit}"
    );
    with_db(move |db| {
        db.query_map("list sessions", &sql, params, |r| {
            Ok(SessionMeta {
                id: r.get(0)?,
                workspace: r.get(1)?,
                provider: r.get(2)?,
                model: r.get(3)?,
                harness: r.get(4)?,
                reasoning_effort: r.get(5)?,
                title: r.get(6)?,
                pinned: r.get::<i64>(7)? != 0,
                updated_ms: r.get(8)?,
            })
        })
    })
}

/// Metadata of one session.
pub fn meta(id: &str) -> Option<SessionMeta> {
    list_where("id=?1", turso::params![id.to_string()], 1)
        .into_iter()
        .next()
}

/// Newest active session in a workspace.
pub fn latest(workspace: &Path) -> Option<String> {
    list(workspace, 1).into_iter().next().map(|m| m.id)
}

pub fn set_pinned(id: &str, pinned: bool) {
    let id = id.to_string();
    with_db(move |db| {
        db.execute(
            "set session pinned",
            "UPDATE sessions SET pinned=?2 WHERE id=?1",
            turso::params![id, pinned as i64],
        );
    });
}

/// Pinned sessions across all workspaces.
pub fn pinned() -> Vec<SessionMeta> {
    list_where("archived_at IS NULL AND pinned=1", (), 50)
}

/// Archive every session of a workspace (removes them from the sidebar).
pub fn archive_workspace(workspace: &Path) {
    let workspace = workspace.display().to_string();
    let ts_ms = now_ms();
    with_db(move |db| {
        db.execute(
            "archive workspace sessions",
            "UPDATE sessions SET archived_at=?2 WHERE workspace=?1 AND archived_at IS NULL",
            turso::params![workspace, ts_ms],
        );
    });
}

pub fn archive(id: &str) {
    let id = id.to_string();
    let ts_ms = now_ms();
    with_db(move |db| {
        db.execute(
            "archive session",
            "UPDATE sessions SET archived_at=?2 WHERE id=?1",
            turso::params![id, ts_ms],
        );
    });
}

pub fn restore(id: &str) {
    let id = id.to_string();
    with_db(move |db| {
        db.execute(
            "clear session tombstone",
            "DELETE FROM session_tombstones WHERE id=?1",
            turso::params![id.as_str()],
        );
        db.execute(
            "restore session",
            "UPDATE sessions SET archived_at=NULL WHERE id=?1",
            turso::params![id],
        );
    });
}

/// Every archived session across all workspaces (for the restore manager in
/// Settings), most-recently-updated first.
pub fn list_archived() -> Vec<SessionMeta> {
    list_where("archived_at IS NOT NULL", (), 500)
}

/// Persist the provider's native CLI session id (codex thread / claude uuid) for
/// this Oxide session, so a resume after an app restart can hand the CLI back
/// its own session via `--resume` instead of starting a fresh one.
pub fn set_cli_session(id: &str, cli_session_id: &str) {
    let id = id.to_string();
    let cli_session_id = cli_session_id.to_string();
    with_db(move |db| {
        db.execute(
            "set native CLI session id",
            "UPDATE sessions SET cli_session_id=?2 WHERE id=?1",
            turso::params![id, cli_session_id],
        );
    });
}

/// The stored native CLI session id for this Oxide session, if any.
pub fn cli_session(id: &str) -> Option<String> {
    let id = id.to_string();
    with_db(move |db| {
        db.query_one(
            "load native CLI session id",
            "SELECT cli_session_id FROM sessions WHERE id=?1",
            turso::params![id],
            |r| match r.get_value(0)? {
                turso::Value::Null => Ok(None),
                turso::Value::Text(value) => Ok(Some(value)),
                _ => r.get::<String>(0).map(Some),
            },
        )
        .flatten()
    })
}

pub fn delete(id: &str) {
    let id = id.to_string();
    let ts_ms = now_ms();
    with_db(move |db| {
        db.execute(
            "write session tombstone",
            "INSERT INTO session_tombstones (id, deleted_at) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET deleted_at=excluded.deleted_at",
            turso::params![id.as_str(), ts_ms],
        );
        db.execute(
            "delete session messages",
            "DELETE FROM messages WHERE session_id=?1",
            turso::params![id.as_str()],
        );
        db.execute(
            "delete session",
            "DELETE FROM sessions WHERE id=?1",
            turso::params![id],
        );
    });
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
            if t.elapsed() < std::time::Duration::from_secs(5) {
                return;
            }
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

    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return;
    };
    let path = home.join(".codex/state_5.sqlite");
    import_codex_desktop_threads_from(&path, &allowed, limit);
}

fn import_codex_desktop_threads_from(
    path: &Path,
    allowed: &std::collections::HashSet<String>,
    limit: usize,
) {
    if !path.exists() {
        return;
    }
    let Ok(codex) = SqliteConnection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return;
    };
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
    let Ok(mut st) = codex.prepare(sql) else {
        return;
    };
    let Ok(rows) = st.query_map(rusqlite::params![limit as i64], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, i64>(4)?,
        ))
    }) else {
        return;
    };

    let imports: Vec<_> = rows
        .flatten()
        .filter(|(native_id, workspace, _, _, _)| {
            !native_id.trim().is_empty()
                && !workspace.trim().is_empty()
                && !is_throwaway_workspace(workspace)
                && allowed.contains(workspace)
        })
        .collect();
    if imports.is_empty() {
        return;
    }

    with_db(move |db| {
        for (native_id, workspace, title, created_ms, updated_ms) in imports {
            let id = format!("codex:{native_id}");
            let tombstoned = db
                .query_one(
                    "check session tombstone",
                    "SELECT 1 FROM session_tombstones WHERE id=?1",
                    turso::params![id.as_str()],
                    |r| r.get::<i64>(0),
                )
                .is_some();
            if tombstoned {
                continue;
            }
            let title = clean_imported_title(&title);
            let created_ms = created_ms.max(0);
            let updated_ms = updated_ms.max(created_ms);
            db.execute(
                "import Codex Desktop thread",
                "INSERT INTO sessions (id, workspace, provider, title, cli_session_id, created_ms, updated_ms)
                 VALUES (?1, ?2, 'codex', ?3, ?4, ?5, ?6)
                 ON CONFLICT(id) DO UPDATE SET
                   workspace=excluded.workspace,
                   provider='codex',
                   title=excluded.title,
                   cli_session_id=excluded.cli_session_id,
                   updated_ms=MAX(sessions.updated_ms, excluded.updated_ms)",
                turso::params![id, workspace, title, native_id, created_ms, updated_ms],
            );
        }
    });
}

/// Import Claude Code CLI (TUI) transcripts for a workspace into the global db,
/// so TUI conversations show up and persist like normal chats. Claude stores
/// them at ~/.claude/projects/<slug>/<uuid>.jsonl (slug = cwd with '/'→'-').
/// Re-imported each call (claude appends live) — cheap, keyed by a stable id.
pub fn import_claude_sessions(workspace: &Path) {
    // Throttle: re-scan a workspace's claude dir at most every 5s.
    static LAST: OnceLock<Mutex<std::collections::HashMap<String, std::time::Instant>>> =
        OnceLock::new();
    {
        let mut g = LAST.get_or_init(Default::default).lock().unwrap();
        let key = workspace.display().to_string();
        if let Some(t) = g.get(&key) {
            if t.elapsed() < std::time::Duration::from_secs(5) {
                return;
            }
        }
        g.insert(key, std::time::Instant::now());
    }
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return;
    };
    let slug = workspace.display().to_string().replace(['/', '.'], "-");
    let dir = home.join(".claude/projects").join(&slug);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return;
    };
    for e in rd.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        let stem = path.file_stem().and_then(|x| x.to_str()).unwrap_or("");
        if stem.is_empty() {
            continue;
        }
        let id = format!("claude-{stem}");
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mut msgs: Vec<(String, String)> = Vec::new();
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let role = match v["type"].as_str() {
                Some("user") => "user",
                Some("assistant") => "assistant",
                _ => continue,
            };
            let content = match &v["message"]["content"] {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Array(a) => a
                    .iter()
                    .filter_map(|x| {
                        if x["type"] == "text" {
                            x["text"].as_str()
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => String::new(),
            };
            let content = content.trim().to_string();
            if !content.is_empty() {
                msgs.push((role.to_string(), content));
            }
        }
        if msgs.is_empty() {
            continue;
        }
        // Only rewrite when the message count changed (claude appended).
        let existing = load(&id).len();
        if existing == msgs.len() {
            continue;
        }
        rewrite(&id, workspace, "claude", &msgs);
        // Preserve order by file mtime.
        if let Ok(meta) = std::fs::metadata(&path) {
            if let Ok(mt) = meta.modified() {
                if let Ok(d) = mt.duration_since(std::time::UNIX_EPOCH) {
                    let ms = d.as_millis() as i64;
                    let id = id.clone();
                    with_db(move |db| {
                        db.execute(
                            "preserve Claude session mtime",
                            "UPDATE sessions SET created_ms=?2, updated_ms=?2 WHERE id=?1",
                            turso::params![id, ms],
                        );
                    });
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
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return;
    };
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
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        let mut provider = String::new();
        let mut msgs: Vec<(String, String)> = Vec::new();
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
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
                let id = id.clone();
                with_db(move |db| {
                    db.execute(
                        "preserve legacy session timestamp",
                        "UPDATE sessions SET created_ms=?2, updated_ms=?2 WHERE id=?1",
                        turso::params![id, ms],
                    );
                });
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
        let codex = SqliteConnection::open(&path).expect("open temp codex db");
        codex
            .execute_batch(
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
            )
            .expect("create threads table");
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
        assert_eq!(
            cli_session(&sessions[0].id).as_deref(),
            Some("native-thread-1")
        );
        assert!(list(Path::new("/private/var/folders/tmp-project"), 10).is_empty());
        assert!(list(Path::new("/Volumes/Data/unopened-by-oxide"), 10).is_empty());

        archive("codex:native-thread-1");
        import_codex_desktop_threads_from(&path, &allowed, 10);
        assert!(list(Path::new("/Volumes/Data/oxide-test-import"), 10).is_empty());

        restore("codex:native-thread-1");
        assert_eq!(
            list(Path::new("/Volumes/Data/oxide-test-import"), 10).len(),
            1
        );

        delete("codex:native-thread-1");
        import_codex_desktop_threads_from(&path, &allowed, 10);
        assert!(list(Path::new("/Volumes/Data/oxide-test-import"), 10).is_empty());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn session_meta_preserves_runtime_config() {
        let id = format!("session-meta-runtime-{}-{}", std::process::id(), now_ms());
        let workspace = Path::new("/Volumes/Data/oxide-session-meta-test");

        append_with_config(
            &id,
            workspace,
            "chatgpt",
            "gpt-5.5",
            "coding",
            "high",
            "user",
            "Test runtime config metadata",
        );

        let row = meta(&id).unwrap();
        assert_eq!(row.provider, "chatgpt");
        assert_eq!(row.model, "gpt-5.5");
        assert_eq!(row.harness, "coding");
        assert_eq!(row.reasoning_effort, "high");

        set_session_config(&id, "claude", "claude-fable-5", "debug", "max");
        let row = meta(&id).unwrap();
        assert_eq!(row.provider, "claude");
        assert_eq!(row.model, "claude-fable-5");
        assert_eq!(row.harness, "debug");
        assert_eq!(row.reasoning_effort, "max");

        delete(&id);
    }
}
