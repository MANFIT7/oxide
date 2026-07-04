//! Persistent memory + self-improvement store (Hermes-style).
//!
//! Lives under `.oxide/memory/` in the workspace (sandbox-protected from the
//! agent's raw file tools). Two kinds of durable knowledge survive across
//! sessions and are injected into every turn's system prompt:
//!
//! - **Facts** — appended to `MEMORY.md` via the `remember` tool.
//! - **Skills** — reusable procedures the agent writes to `skills/<name>.md`
//!   via the `save_skill` tool (the self-improvement loop).

use std::path::{Path, PathBuf};

pub struct Memory {
    dir: PathBuf,
}

impl Memory {
    pub fn new(workspace: &Path) -> Self {
        Self {
            dir: workspace.join(".oxide/memory"),
        }
    }

    fn ensure(&self) {
        let _ = std::fs::create_dir_all(self.dir.join("skills"));
    }

    /// The memory block injected into the system prompt (facts + skill index).
    pub fn load_block(&self) -> String {
        let mut s = String::new();
        if let Ok(m) = std::fs::read_to_string(self.dir.join("MEMORY.md")) {
            if !m.trim().is_empty() {
                s.push_str("## Remembered facts\n");
                s.push_str(m.trim());
                s.push('\n');
            }
        }
        let skills = self.skills();
        if !skills.is_empty() {
            s.push_str("\n## Learned skills (read the file before reusing)\n");
            for (name, summary) in &skills {
                s.push_str(&format!("- `{name}` — {summary}\n"));
            }
        }
        s
    }

    /// Append a durable fact.
    pub fn remember(&self, text: &str) -> std::io::Result<()> {
        self.ensure();
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("MEMORY.md"))?;
        writeln!(f, "- {}", text.trim())
    }

    /// Save (or overwrite) a reusable skill.
    pub fn save_skill(&self, name: &str, content: &str) -> std::io::Result<()> {
        self.ensure();
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let safe = if safe.trim_matches('-').is_empty() {
            "skill".to_string()
        } else {
            safe
        };
        std::fs::write(self.dir.join("skills").join(format!("{safe}.md")), content)
    }

    /// Mechanical curator (hermes' lifecycle, deterministic v1): skills
    /// untouched for 45+ days move to `skills/archive/` — recoverable, never
    /// deleted, and out of the system-prompt index. Runs at most once per
    /// 24h (state file guard). Returns how many were archived.
    pub fn curate(&self) -> usize {
        const ARCHIVE_AFTER: std::time::Duration = std::time::Duration::from_secs(45 * 86_400);
        const RUN_EVERY: std::time::Duration = std::time::Duration::from_secs(86_400);
        let state = self.dir.join(".curator_state");
        let ran_recently = std::fs::metadata(&state)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|age| age < RUN_EVERY)
            .unwrap_or(false);
        if ran_recently {
            return 0;
        }
        let _ = std::fs::write(&state, "");
        let skills_dir = self.dir.join("skills");
        let archive = skills_dir.join("archive");
        let mut moved = 0usize;
        if let Ok(rd) = std::fs::read_dir(&skills_dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.extension().and_then(|x| x.to_str()) != Some("md") {
                    continue;
                }
                let stale = std::fs::metadata(&p)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .map(|age| age >= ARCHIVE_AFTER)
                    .unwrap_or(false);
                if !stale {
                    continue;
                }
                let _ = std::fs::create_dir_all(&archive);
                if let Some(name) = p.file_name() {
                    if std::fs::rename(&p, archive.join(name)).is_ok() {
                        moved += 1;
                    }
                }
            }
        }
        moved
    }

    /// Facts as individual lines (the `- ` bullets of MEMORY.md), for the GUI
    /// memory panel.
    pub fn facts(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("MEMORY.md"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let t = l.trim();
                t.strip_prefix("- ")
                    .map(str::to_string)
                    .or_else(|| (!t.is_empty()).then(|| t.to_string()))
            })
            .collect()
    }

    /// Remove one fact by its index in [`facts`] (GUI delete button).
    pub fn remove_fact(&self, index: usize) -> std::io::Result<()> {
        let facts = self.facts();
        let next: String = facts
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != index)
            .map(|(_, f)| format!("- {f}\n"))
            .collect();
        std::fs::write(self.dir.join("MEMORY.md"), next)
    }

    /// Move one skill to `skills/archive/` (GUI archive button).
    pub fn archive_skill(&self, name: &str) -> std::io::Result<()> {
        let skills = self.dir.join("skills");
        let archive = skills.join("archive");
        std::fs::create_dir_all(&archive)?;
        let file = format!("{name}.md");
        std::fs::rename(skills.join(&file), archive.join(&file))
    }

    /// Days since a skill file was last touched (freshness in the panel).
    pub fn skill_age_days(&self, name: &str) -> Option<u64> {
        std::fs::metadata(self.dir.join("skills").join(format!("{name}.md")))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|d| d.as_secs() / 86_400)
    }

    /// How many skills sit in `skills/archive/`.
    pub fn archived_count(&self) -> usize {
        std::fs::read_dir(self.dir.join("skills/archive"))
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("md"))
                    .count()
            })
            .unwrap_or(0)
    }

    /// Full path of a skill file (GUI "open in editor").
    pub fn skill_path(&self, name: &str) -> PathBuf {
        self.dir.join("skills").join(format!("{name}.md"))
    }

    /// `(name, one-line summary)` for each saved skill.
    pub fn skills(&self) -> Vec<(String, String)> {
        let mut v = Vec::new();
        if let Ok(rd) = std::fs::read_dir(self.dir.join("skills")) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.extension().and_then(|x| x.to_str()) != Some("md") {
                    continue;
                }
                let name = p
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let summary = std::fs::read_to_string(&p)
                    .ok()
                    .and_then(|t| {
                        t.lines().find(|l| !l.trim().is_empty()).map(|l| {
                            l.trim()
                                .trim_start_matches('#')
                                .trim()
                                .chars()
                                .take(80)
                                .collect::<String>()
                        })
                    })
                    .unwrap_or_default();
                v.push((name, summary));
            }
        }
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remember_and_load_roundtrips() {
        let tmp = std::env::temp_dir().join(format!("oxide-mem-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let m = Memory::new(&tmp);
        m.remember("user prefers Rust").unwrap();
        m.save_skill("deploy", "# Deploy\nrun cargo build then ship")
            .unwrap();
        let block = m.load_block();
        assert!(block.contains("user prefers Rust"));
        assert!(block.contains("deploy"));
        assert_eq!(m.skills().len(), 1);
        std::fs::remove_dir_all(&tmp).ok();
    }
}
