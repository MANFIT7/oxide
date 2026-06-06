//! Session persistence + workspace checkpoints.
//!
//! Both live under `.oxide/` in the workspace, which the sandbox forces
//! read-only for *tools* — so the agent can never tamper with its own history
//! or checkpoints, only the engine writes here. Sessions are append-only JSONL
//! (one message per line); checkpoints snapshot a file's prior bytes so a write
//! can be rewound.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// One persisted conversation message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub role: String,
    pub content: String,
    pub ts_ms: u128,
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Append-only JSONL session log.
pub struct SessionStore {
    path: PathBuf,
    pub id: String,
}

impl SessionStore {
    /// Open a fresh session file `.oxide/sessions/<id>.jsonl` under `workspace`.
    pub fn open(workspace: &Path) -> std::io::Result<Self> {
        let dir = workspace.join(".oxide/sessions");
        std::fs::create_dir_all(&dir)?;
        let id = format!("{}", now_ms());
        let path = dir.join(format!("{id}.jsonl"));
        // Touch the file so it exists even before the first message.
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        Ok(Self { path, id })
    }

    pub fn append(&self, role: &str, content: &str) -> std::io::Result<()> {
        let rec = StoredMessage {
            role: role.to_string(),
            content: content.to_string(),
            ts_ms: now_ms(),
        };
        let line = serde_json::to_string(&rec).unwrap_or_default();
        // Do NOT recreate the file: if the user deleted this session, stop
        // persisting instead of resurrecting it. The file is created in `open`.
        let mut f = std::fs::OpenOptions::new()
            .create(false)
            .append(true)
            .open(&self.path)?;
        writeln!(f, "{line}")
    }

    /// Load every message from a session file (for resume).
    pub fn load(path: &Path) -> std::io::Result<Vec<StoredMessage>> {
        let text = std::fs::read_to_string(path)?;
        Ok(text
            .lines()
            .filter_map(|l| serde_json::from_str::<StoredMessage>(l).ok())
            .collect())
    }

    /// Most recently modified session file in the workspace, if any.
    pub fn latest(workspace: &Path) -> Option<PathBuf> {
        let dir = workspace.join(".oxide/sessions");
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
            .collect();
        entries.sort();
        entries.pop()
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
