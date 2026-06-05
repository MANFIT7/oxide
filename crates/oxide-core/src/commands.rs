//! Slash commands — user-defined `/name args` shortcuts that expand into a
//! templated prompt before the turn runs. Files live in `.oxide/commands/*.md`
//! with optional YAML frontmatter (`description:`) and a markdown body where
//! `$ARGUMENTS` is replaced by whatever the user typed after the command.

use std::path::Path;

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
    let raw = std::fs::read_to_string(&path).ok()?;
    let (_desc, body) = split_frontmatter(&raw);
    Some(body.replace("$ARGUMENTS", args.trim()).replace("$ARG", args.trim()))
}

/// `(name, description)` for each available command.
pub fn list(workspace: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
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
            out.push((name, desc));
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_substitutes_args() {
        let tmp = std::env::temp_dir().join(format!("oxide-cmd-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join(".oxide/commands")).unwrap();
        std::fs::write(
            tmp.join(".oxide/commands/review.md"),
            "---\ndescription: Review code\n---\nReview this: $ARGUMENTS",
        )
        .unwrap();
        let out = expand(&tmp, "/review the auth module").unwrap();
        assert_eq!(out, "Review this: the auth module");
        assert_eq!(list(&tmp)[0].1, "Review code");
        std::fs::remove_dir_all(&tmp).ok();
    }
}
