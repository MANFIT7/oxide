//! Slash commands — user-defined `/name args` shortcuts that expand into a
//! templated prompt before the turn runs. Files live in `.oxide/commands/*.md`
//! with optional YAML frontmatter (`description:`) and a markdown body where
//! `$ARGUMENTS` is replaced by whatever the user typed after the command.

use std::path::Path;

const BUILTIN_COMMANDS: &[(&str, &str)] = &[(
    "init",
    "Initialize repository instructions and Oxide agent context",
)];

fn commands_dir(workspace: &Path) -> std::path::PathBuf {
    workspace.join(".oxide/commands")
}

/// Strip a leading `--- ... ---` frontmatter block, returning `(description, body)`.
fn split_frontmatter(text: &str) -> (String, String) {
    if let Some(rest) = text.strip_prefix("---") {
        if let Some(end) = rest.find("\n---") {
            let fm = &rest[..end];
            let body = rest[end + 4..].trim_start_matches('\n');
            let desc = fm
                .lines()
                .find_map(|l| l.trim().strip_prefix("description:"))
                .map(|d| d.trim().trim_matches('"').to_string())
                .unwrap_or_default();
            return (desc, body.to_string());
        }
    }
    (String::new(), text.to_string())
}

fn builtin_command(name: &str, args: &str) -> Option<String> {
    match name {
        "init" => {
            let focus = args.trim();
            let focus = if focus.is_empty() {
                String::new()
            } else {
                format!("\n\nUser focus/constraints: {focus}")
            };
            Some(format!(
                "Initialize this repository for future Oxide/Codex-style agent work.\n\n\
Do the smallest useful setup pass:\n\
1. Inspect the repository structure, build/test commands, framework conventions, and existing agent config.\n\
2. Create or update AGENTS.md with concise, durable instructions for coding agents: architecture, commands, verification, style, and safety notes. Preserve useful existing content.\n\
3. If an Oxide config or .oxide commands/skills setup is missing and would clearly help, propose it first unless the change is obviously safe and local.\n\
4. Do not refactor application code as part of initialization.\n\
5. Finish by summarizing what changed and which verification command you ran.{focus}"
            ))
        }
        _ => None,
    }
}

/// If `text` is a `/command [args]`, return the expanded prompt. Returns `None`
/// for non-slash text; `Some(Err)` style handled by the caller emitting a note.
pub fn expand(workspace: &Path, text: &str) -> Option<String> {
    let text = text.trim();
    let rest = text.strip_prefix('/')?;
    let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    if name.is_empty() {
        return None;
    }
    let path = commands_dir(workspace).join(format!("{name}.md"));
    if let Ok(raw) = std::fs::read_to_string(&path) {
        let (_desc, body) = split_frontmatter(&raw);
        return Some(
            body.replace("$ARGUMENTS", args.trim())
                .replace("$ARG", args.trim()),
        );
    }
    builtin_command(name, args)
}

/// `(name, description)` for each available command.
#[allow(dead_code)]
pub fn list(workspace: &Path) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = BUILTIN_COMMANDS
        .iter()
        .map(|(name, desc)| ((*name).to_string(), (*desc).to_string()))
        .collect();
    if let Ok(rd) = std::fs::read_dir(commands_dir(workspace)) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let name = p
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let desc = std::fs::read_to_string(&p)
                .map(|t| split_frontmatter(&t).0)
                .unwrap_or_default();
            if let Some(existing) = out.iter_mut().find(|(n, _)| n == &name) {
                *existing = (name, desc);
            } else {
                out.push((name, desc));
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> std::path::PathBuf {
        let p = std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join(format!("target/tmp/{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn expand_substitutes_args() {
        let tmp = test_dir("oxide-cmd");
        std::fs::create_dir_all(tmp.join(".oxide/commands")).unwrap();
        std::fs::write(
            tmp.join(".oxide/commands/review.md"),
            "---\ndescription: Review code\n---\nReview this: $ARGUMENTS",
        )
        .unwrap();
        let out = expand(&tmp, "/review the auth module").unwrap();
        assert_eq!(out, "Review this: the auth module");
        let listed = list(&tmp);
        assert!(listed.iter().any(|(name, desc)| name == "init"
            && desc == "Initialize repository instructions and Oxide agent context"));
        assert!(listed
            .iter()
            .any(|(name, desc)| name == "review" && desc == "Review code"));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn expand_builtin_init() {
        let tmp = test_dir("oxide-cmd-init");
        std::fs::create_dir_all(&tmp).unwrap();
        let out = expand(&tmp, "/init focus on Rust checks").unwrap();
        assert!(out.contains("Initialize this repository"));
        assert!(out.contains("focus on Rust checks"));
        std::fs::remove_dir_all(&tmp).ok();
    }
}
