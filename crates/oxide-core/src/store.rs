//! Session persistence + workspace checkpoints.
//!
//! Both live under `.oxide/` in the workspace, which the sandbox forces
//! read-only for *tools* — so the agent can never tamper with its own history
//! or checkpoints, only the engine writes here. Sessions are append-only JSONL
//! (one message per line); checkpoints snapshot a file's prior bytes so a write
//! can be rewound.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One persisted conversation message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub role: String,
    pub content: String,
    pub ts_ms: u128,
}

/// Session handle backed by the GLOBAL SQLite db (see `crate::db`). The id is
/// minted eagerly; the row + messages are created lazily on the first append,
/// so an empty chat stores nothing.
pub struct SessionStore {
    pub id: String,
    workspace: PathBuf,
    config: std::sync::Mutex<SessionRuntimeConfig>,
}

#[derive(Clone, Default)]
struct SessionRuntimeConfig {
    provider: String,
    model: String,
    harness: String,
    reasoning_effort: String,
}

impl SessionStore {
    /// Fresh session in `workspace` (nothing persisted until the first append).
    pub fn open(workspace: &Path) -> std::io::Result<Self> {
        Ok(Self {
            id: crate::db::new_id(),
            workspace: workspace.to_path_buf(),
            config: std::sync::Mutex::new(SessionRuntimeConfig::default()),
        })
    }

    /// Sesi baru dengan suffix id eksplisit — dipakai sesi anak sub-agent agar
    /// dua worker yang selesai pada milidetik yang sama tidak bertabrakan id.
    pub fn open_child(workspace: &Path, suffix: &str) -> std::io::Result<Self> {
        let safe: String = suffix
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .take(24)
            .collect();
        Ok(Self {
            id: format!("{}-{}", crate::db::new_id(), safe),
            workspace: workspace.to_path_buf(),
            config: std::sync::Mutex::new(SessionRuntimeConfig::default()),
        })
    }

    /// Attach to an EXISTING session by id — appends continue it.
    pub fn attach(id: &str, workspace: &Path) -> std::io::Result<Self> {
        if !crate::db::exists(id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "session not found",
            ));
        }
        Ok(Self {
            id: id.to_string(),
            workspace: workspace.to_path_buf(),
            config: std::sync::Mutex::new(SessionRuntimeConfig::default()),
        })
    }

    /// Stable identifier handed to the UI (was a file path; now the db id).
    pub fn path_str(&self) -> String {
        self.id.clone()
    }

    pub fn set_runtime_config(
        &self,
        provider: &str,
        model: &str,
        harness: &str,
        reasoning_effort: &str,
    ) {
        let current = {
            let mut config = self.config.lock().unwrap();
            config.provider = provider.to_string();
            config.model = model.to_string();
            config.harness = harness.to_string();
            config.reasoning_effort = reasoning_effort.to_string();
            config.clone()
        };
        self.persist_config(&current);
    }

    fn persist_config(&self, config: &SessionRuntimeConfig) {
        if crate::db::exists(&self.id) {
            crate::db::set_session_config(
                &self.id,
                &config.provider,
                &config.model,
                &config.harness,
                &config.reasoning_effort,
            );
        }
    }

    pub fn append(&self, role: &str, content: &str) -> std::io::Result<()> {
        let config = self.config.lock().map(|g| g.clone()).unwrap_or_default();
        crate::db::append_with_config(
            &self.id,
            &self.workspace,
            &config.provider,
            &config.model,
            &config.harness,
            &config.reasoning_effort,
            role,
            content,
        );
        Ok(())
    }

    /// Replace the whole conversation (restore-to-message).
    pub fn rewrite(&self, msgs: &[(String, String)]) -> std::io::Result<()> {
        let config = self.config.lock().map(|g| g.clone()).unwrap_or_default();
        crate::db::rewrite_with_config(
            &self.id,
            &self.workspace,
            &config.provider,
            &config.model,
            &config.harness,
            &config.reasoning_effort,
            msgs,
        );
        Ok(())
    }

    /// Load every message of a session id.
    pub fn load(id: &str) -> std::io::Result<Vec<StoredMessage>> {
        let rows = crate::db::load(id);
        if rows.is_empty() && !crate::db::exists(id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "session not found",
            ));
        }
        Ok(rows
            .into_iter()
            .map(|(role, content)| StoredMessage {
                role,
                content,
                ts_ms: 0,
            })
            .collect())
    }

    /// Newest active session id in a workspace.
    pub fn latest(workspace: &Path) -> Option<String> {
        crate::db::import_workspace(workspace);
        crate::db::latest(workspace)
    }
}

/// Snapshot of one file's prior state, taken before a mutating tool runs.
struct FileSnapshot {
    path: PathBuf,
    /// Prior bytes, or `None` if the file did not exist (so rewind deletes it).
    prior: Option<Vec<u8>>,
}

struct Checkpoint {
    id: u64,
    files: Vec<FileSnapshot>,
}

/// Checkpoint log enabling rewind of file writes. Mirrored to
/// `.oxide/checkpoints/<id>/` so rewind survives an app restart (Cursor-style
/// durable checkpoints); in-memory only when constructed via `default()`.
#[derive(Default)]
pub struct CheckpointStore {
    next_id: u64,
    checkpoints: Vec<Checkpoint>,
    dir: Option<PathBuf>,
}

impl CheckpointStore {
    /// Load persisted checkpoints from `.oxide/checkpoints/` (newest ~50 kept;
    /// older ones are pruned — a rewind that far back crosses too many turns
    /// to be meaningful).
    pub fn load(workspace: &Path) -> Self {
        const KEEP: usize = 50;
        let dir = workspace.join(".oxide/checkpoints");
        let mut ids: Vec<u64> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| e.file_name().to_string_lossy().parse::<u64>().ok())
                    .collect()
            })
            .unwrap_or_default();
        ids.sort_unstable();
        while ids.len() > KEEP {
            let old = ids.remove(0);
            let _ = std::fs::remove_dir_all(dir.join(old.to_string()));
        }
        let mut checkpoints = Vec::new();
        for id in &ids {
            let cp_dir = dir.join(id.to_string());
            let Ok(meta) = std::fs::read_to_string(cp_dir.join("meta.json")) else {
                continue;
            };
            let Ok(meta) = serde_json::from_str::<serde_json::Value>(&meta) else {
                continue;
            };
            let mut files = Vec::new();
            for (fi, f) in meta["files"].as_array().into_iter().flatten().enumerate() {
                let Some(path) = f["path"].as_str() else {
                    continue;
                };
                let existed = f["existed"].as_bool().unwrap_or(false);
                let prior = if existed {
                    std::fs::read(cp_dir.join(format!("file-{fi}.bin"))).ok()
                } else {
                    None
                };
                files.push(FileSnapshot {
                    path: PathBuf::from(path),
                    prior,
                });
            }
            if !files.is_empty() {
                checkpoints.push(Checkpoint { id: *id, files });
            }
        }
        Self {
            next_id: ids.last().copied().unwrap_or(0),
            checkpoints,
            dir: Some(dir),
        }
    }

    /// Mirror one checkpoint to disk (best-effort — rewind still works from
    /// memory if the write fails).
    fn persist(&self, cp: &Checkpoint) {
        let Some(dir) = &self.dir else { return };
        let cp_dir = dir.join(cp.id.to_string());
        if std::fs::create_dir_all(&cp_dir).is_err() {
            return;
        }
        let files: Vec<serde_json::Value> = cp
            .files
            .iter()
            .map(|f| {
                serde_json::json!({
                    "path": f.path.display().to_string(),
                    "existed": f.prior.is_some(),
                })
            })
            .collect();
        let meta = serde_json::json!({ "id": cp.id, "files": files });
        let _ = std::fs::write(cp_dir.join("meta.json"), meta.to_string());
        for (fi, f) in cp.files.iter().enumerate() {
            if let Some(bytes) = &f.prior {
                let _ = std::fs::write(cp_dir.join(format!("file-{fi}.bin")), bytes);
            }
        }
    }

    fn unpersist(&self, id: u64) {
        if let Some(dir) = &self.dir {
            let _ = std::fs::remove_dir_all(dir.join(id.to_string()));
        }
    }

    /// Snapshot with EXPLICIT prior bytes (e.g. reconstructed from a git
    /// baseline for CLI-driver edits, where the file is already modified).
    pub fn snapshot_with(&mut self, path: &Path, prior: Option<Vec<u8>>) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        let cp = Checkpoint {
            id,
            files: vec![FileSnapshot {
                path: path.to_path_buf(),
                prior,
            }],
        };
        self.persist(&cp);
        self.checkpoints.push(cp);
        id
    }

    /// Snapshot `path`'s current bytes (capturing absence) under a new checkpoint.
    /// Returns the checkpoint id to surface to the frontend.
    pub fn snapshot(&mut self, path: &Path) -> u64 {
        let prior = std::fs::read(path).ok();
        self.snapshot_with(path, prior)
    }

    /// Restore the workspace to checkpoint `id`, undoing it and every checkpoint
    /// taken after it (LIFO). Returns the number of files restored.
    pub fn rewind(&mut self, id: u64) -> u64 {
        let mut restored = 0u64;
        while let Some(cp) = self.checkpoints.last() {
            if cp.id < id {
                break;
            }
            let cp = self.checkpoints.pop().unwrap();
            self.unpersist(cp.id);
            for snap in cp.files {
                match snap.prior {
                    Some(bytes) => {
                        if std::fs::write(&snap.path, bytes).is_ok() {
                            restored += 1;
                        }
                    }
                    None => {
                        // `prior=None` normally means "didn't exist before this
                        // turn" — but it is ALSO what gets recorded when the
                        // baseline lookup failed (absolute path, workspace that
                        // is a subdir of the git root, …). Deleting outright
                        // would destroy a pre-existing file in that case, so
                        // move it aside instead of removing it.
                        if trash_on_rewind(&snap.path).is_ok() {
                            restored += 1;
                        }
                    }
                }
            }
            if cp.id == id {
                break;
            }
        }
        restored
    }
}

/// "Delete" a rewound new-file by moving it to `.oxide/trash/` next to the
/// nearest `.oxide` dir up the tree (same filesystem → cheap rename), falling
/// back to a sibling `<name>.rewind-removed`. Never `remove_file`: if the
/// checkpoint mislabeled a pre-existing file as new, the bytes must survive.
fn trash_on_rewind(path: &std::path::Path) -> std::io::Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    for anc in path.ancestors().skip(1) {
        let oxide = anc.join(".oxide");
        if oxide.is_dir() {
            let trash = oxide.join("trash");
            std::fs::create_dir_all(&trash)?;
            let mut dest = trash.join(&name);
            let mut n = 1u32;
            while dest.exists() {
                dest = trash.join(format!("{name}.{n}"));
                n += 1;
            }
            return std::fs::rename(path, dest);
        }
    }
    std::fs::rename(path, path.with_extension("rewind-removed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoints_persist_across_reload() {
        let ws = std::env::temp_dir().join(format!("oxide-cp-reload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        std::fs::create_dir_all(&ws).unwrap();
        let target = ws.join("code.txt");
        std::fs::write(&target, "v1").unwrap();
        let id = {
            let mut cps = CheckpointStore::load(&ws);
            let id = cps.snapshot(&target);
            std::fs::write(&target, "v2").unwrap();
            id
        };
        // Fresh process simulation: reload from disk, then rewind.
        let mut cps = CheckpointStore::load(&ws);
        assert_eq!(cps.rewind(id), 1);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "v1");
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn session_append_and_load_roundtrips() {
        let tmp = std::env::temp_dir().join(format!("oxide-sess-{}", std::process::id()));
        let store = SessionStore::open(&tmp).unwrap();
        store.append("user", "hi").unwrap();
        store.append("assistant", "hello").unwrap();
        let latest = SessionStore::latest(&tmp).unwrap();
        let msgs = SessionStore::load(&latest).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[1].content, "hello");
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn checkpoint_rewind_restores_prior_content() {
        let tmp = std::env::temp_dir().join(format!("oxide-cp-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let f = tmp.join("file.txt");
        std::fs::write(&f, "original").unwrap();

        let mut cps = CheckpointStore::default();
        let id = cps.snapshot(&f);
        std::fs::write(&f, "modified").unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "modified");

        let restored = cps.rewind(id);
        assert_eq!(restored, 1);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "original");
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn checkpoint_rewind_deletes_newly_created_file() {
        let tmp = std::env::temp_dir().join(format!("oxide-cp2-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let f = tmp.join("new.txt");

        let mut cps = CheckpointStore::default();
        let id = cps.snapshot(&f); // file absent
        std::fs::write(&f, "created").unwrap();
        assert!(f.exists());

        cps.rewind(id);
        assert!(
            !f.exists(),
            "rewind should delete a file that didn't exist before"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }
}
