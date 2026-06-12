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
    provider: std::sync::Mutex<String>,
}

impl SessionStore {
    /// Fresh session in `workspace` (nothing persisted until the first append).
    pub fn open(workspace: &Path) -> std::io::Result<Self> {
        Ok(Self {
            id: crate::db::new_id(),
            workspace: workspace.to_path_buf(),
            provider: std::sync::Mutex::new(String::new()),
        })
    }

    /// Attach to an EXISTING session by id — appends continue it.
    pub fn attach(id: &str, workspace: &Path) -> std::io::Result<Self> {
        if !crate::db::exists(id) {
            return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "session not found"));
        }
        Ok(Self {
            id: id.to_string(),
            workspace: workspace.to_path_buf(),
            provider: std::sync::Mutex::new(String::new()),
        })
    }

    /// Stable identifier handed to the UI (was a file path; now the db id).
    pub fn path_str(&self) -> String {
        self.id.clone()
    }

    /// Provider stamp (sidebar logos). Applied immediately if the session
    /// already exists, and to every future append.
    pub fn set_meta(&self, content: &str) {
        let p = content.strip_prefix("provider=").unwrap_or(content).to_string();
        if crate::db::exists(&self.id) {
            crate::db::set_provider(&self.id, &p);
        }
        if let Ok(mut g) = self.provider.lock() {
            *g = p;
        }
    }

    pub fn append(&self, role: &str, content: &str) -> std::io::Result<()> {
        let prov = self.provider.lock().map(|g| g.clone()).unwrap_or_default();
        crate::db::append(&self.id, &self.workspace, &prov, role, content);
        Ok(())
    }

    /// Replace the whole conversation (restore-to-message).
    pub fn rewrite(&self, msgs: &[(String, String)]) -> std::io::Result<()> {
        let prov = self.provider.lock().map(|g| g.clone()).unwrap_or_default();
        crate::db::rewrite(&self.id, &self.workspace, &prov, msgs);
        Ok(())
    }

    /// Load every message of a session id.
    pub fn load(id: &str) -> std::io::Result<Vec<StoredMessage>> {
        let rows = crate::db::load(id);
        if rows.is_empty() && !crate::db::exists(id) {
            return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "session not found"));
        }
        Ok(rows
            .into_iter()
            .map(|(role, content)| StoredMessage { role, content, ts_ms: 0 })
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

/// In-memory checkpoint log enabling rewind of file writes within a session.
#[derive(Default)]
pub struct CheckpointStore {
    next_id: u64,
    checkpoints: Vec<Checkpoint>,
}

impl CheckpointStore {
    /// Snapshot with EXPLICIT prior bytes (e.g. reconstructed from a git
    /// baseline for CLI-driver edits, where the file is already modified).
    pub fn snapshot_with(&mut self, path: &Path, prior: Option<Vec<u8>>) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.checkpoints.push(Checkpoint {
            id,
            files: vec![FileSnapshot { path: path.to_path_buf(), prior }],
        });
        id
    }

    /// Snapshot `path`'s current bytes (capturing absence) under a new checkpoint.
    /// Returns the checkpoint id to surface to the frontend.
    pub fn snapshot(&mut self, path: &Path) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        let prior = std::fs::read(path).ok();
        self.checkpoints.push(Checkpoint {
            id,
            files: vec![FileSnapshot {
                path: path.to_path_buf(),
                prior,
            }],
        });
        id
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
            for snap in cp.files {
                match snap.prior {
                    Some(bytes) => {
                        if std::fs::write(&snap.path, bytes).is_ok() {
                            restored += 1;
                        }
                    }
                    None => {
                        // File didn't exist before; remove it on rewind.
                        if std::fs::remove_file(&snap.path).is_ok() {
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

#[cfg(test)]
mod tests {
    use super::*;

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
