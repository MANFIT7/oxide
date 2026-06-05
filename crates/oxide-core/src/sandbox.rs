//! Sandbox policy enforcement for tool execution.
//!
//! One [`SandboxPolicy`] enum (in `oxide-protocol`) is compiled down to each
//! platform's primitives. macOS uses Seatbelt via `sandbox-exec` with a
//! deny-by-default profile; Linux will add Landlock+seccomp (Fase 5). Path
//! checks (workspace containment, `.git` protection) are enforced in Rust
//! regardless of platform so the filesystem tools are safe everywhere.

use oxide_protocol::SandboxPolicy;
use std::path::{Component, Path, PathBuf};

/// Result of validating a path against the active policy + workspace root.
pub enum PathCheck {
    Ok(PathBuf),
    Denied(String),
}

/// Normalize a path lexically (no symlink resolution, no disk access) so we can
/// reason about containment even for files that don't exist yet.
fn lexical_normalize(base: &Path, p: &Path) -> PathBuf {
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    };
    let mut out = PathBuf::new();
    for comp in joined.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// True if `path` is inside `root` (after lexical normalization).
fn within(root: &Path, path: &Path) -> bool {
    path.starts_with(root)
}

/// Whether the path touches a protected dir (`.git`, `.oxide`) that must stay
/// read-only even under workspace-write.
fn is_protected(path: &Path) -> bool {
    path.components()
        .any(|c| matches!(c, Component::Normal(name) if name == ".git" || name == ".oxide"))
}

/// Validate a read access.
pub fn check_read(policy: SandboxPolicy, root: &Path, requested: &Path) -> PathCheck {
    let abs = lexical_normalize(root, requested);
    match policy {
        SandboxPolicy::DangerFullAccess => PathCheck::Ok(abs),
        // Reads are allowed within the workspace for both read-only and write.
        _ => {
            if within(root, &abs) {
                PathCheck::Ok(abs)
            } else {
                PathCheck::Denied(format!("read outside workspace: {}", abs.display()))
            }
        }
    }
}

/// Validate a write access.
pub fn check_write(policy: SandboxPolicy, root: &Path, requested: &Path) -> PathCheck {
    let abs = lexical_normalize(root, requested);
    match policy {
        SandboxPolicy::DangerFullAccess => PathCheck::Ok(abs),
        SandboxPolicy::ReadOnly => {
            PathCheck::Denied("sandbox is read-only; writes are denied".to_string())
        }
        SandboxPolicy::WorkspaceWrite => {
            if !within(root, &abs) {
                PathCheck::Denied(format!("write outside workspace: {}", abs.display()))
            } else if is_protected(&abs) {
                PathCheck::Denied(format!("write to protected path: {}", abs.display()))
            } else {
                PathCheck::Ok(abs)
            }
        }
    }
}

/// Build a macOS Seatbelt profile string for shell execution under `policy`.
///
/// Deny-by-default; allow process basics and reads everywhere, writes only to
/// the workspace root, and gate network off unless full-access.
#[cfg(target_os = "macos")]
pub fn seatbelt_profile(policy: SandboxPolicy, root: &Path) -> String {
    let root = root.display();
    match policy {
        SandboxPolicy::DangerFullAccess => "(version 1)\n(allow default)\n".to_string(),
        SandboxPolicy::ReadOnly => format!(
            "(version 1)\n(deny default)\n(allow process*)\n(allow sysctl-read)\n\
             (allow file-read*)\n(deny file-write*)\n(deny network*)\n\
             ; root: {root}\n"
        ),
        SandboxPolicy::WorkspaceWrite => format!(
            "(version 1)\n(deny default)\n(allow process*)\n(allow sysctl-read)\n\
             (allow file-read*)\n(deny network*)\n\
             (allow file-write* (subpath \"{root}\"))\n\
             (deny file-write* (subpath \"{root}/.git\"))\n\
             (deny file-write* (subpath \"{root}/.oxide\"))\n"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_outside_workspace_denied() {
        let root = Path::new("/work/proj");
        assert!(matches!(
            check_write(
                SandboxPolicy::WorkspaceWrite,
                root,
                Path::new("../etc/passwd")
            ),
            PathCheck::Denied(_)
        ));
    }

    #[test]
    fn write_to_git_denied() {
        let root = Path::new("/work/proj");
        assert!(matches!(
            check_write(
                SandboxPolicy::WorkspaceWrite,
                root,
                Path::new(".git/hooks/pre-commit")
            ),
            PathCheck::Denied(_)
        ));
    }

    #[test]
    fn write_inside_workspace_ok() {
        let root = Path::new("/work/proj");
        assert!(matches!(
            check_write(
                SandboxPolicy::WorkspaceWrite,
                root,
                Path::new("src/main.rs")
            ),
            PathCheck::Ok(_)
        ));
    }

    #[test]
    fn readonly_denies_writes() {
        let root = Path::new("/work/proj");
        assert!(matches!(
            check_write(SandboxPolicy::ReadOnly, root, Path::new("src/main.rs")),
            PathCheck::Denied(_)
        ));
    }
}
