use crate::sandbox::{self, PathCheck};
use crate::tools::augment_shell_env;
use oxide_protocol::{SandboxPolicy, ToolSpec};
use std::path::Path;
use std::time::Duration;

const GIT_TIMEOUT: Duration = Duration::from_secs(120);
const OUTPUT_LIMIT: usize = 20_000;

pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec::new(
            "git_status",
            "Read concise repository status and current branch through Oxide's structured Git broker.",
        )
        .params(serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {}
        })),
        ToolSpec::new(
            "git_diff",
            "Read a working-tree or staged Git diff, optionally limited to one workspace path.",
        )
        .params(serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "staged": { "type": "boolean", "description": "Show the staged diff instead of the working-tree diff." },
                "path": { "type": "string", "description": "Optional workspace-relative path." }
            }
        })),
        ToolSpec::new(
            "git_commit",
            "Commit only the explicitly listed workspace paths. Unrelated staged changes are excluded. Requires approval.",
        )
        .mutating(true)
        .params(serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "paths": {
                    "type": "array",
                    "minItems": 1,
                    "items": { "type": "string" },
                    "description": "Exact workspace-relative paths to stage and commit."
                },
                "message": { "type": "string", "minLength": 1, "maxLength": 500 }
            },
            "required": ["paths", "message"]
        })),
        ToolSpec::new(
            "git_push",
            "Push the current branch (or an explicitly named branch) without force. Requires approval and never accepts arbitrary flags.",
        )
        .mutating(true)
        .params(serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "remote": { "type": "string", "description": "Remote name; defaults to origin." },
                "branch": { "type": "string", "description": "Branch name; defaults to the current branch." }
            }
        })),
    ]
}

pub async fn execute(
    workspace: &Path,
    tool: &str,
    args: &serde_json::Value,
) -> Option<(String, bool)> {
    let result = match tool {
        "git_status" => run_git(workspace, &["status", "--short", "--branch"]).await,
        "git_diff" => git_diff(workspace, args).await,
        "git_commit" => git_commit(workspace, args).await,
        "git_push" => git_push(workspace, args).await,
        _ => return None,
    };
    Some(result)
}

async fn git_diff(workspace: &Path, args: &serde_json::Value) -> (String, bool) {
    let path = match args.get("path").and_then(|value| value.as_str()) {
        Some(path) if !path.trim().is_empty() => match safe_relative_path(workspace, path) {
            Ok(path) => Some(path),
            Err(error) => return (error, false),
        },
        _ => None,
    };
    let mut argv = vec!["diff".to_string()];
    if args["staged"].as_bool().unwrap_or(false) {
        argv.push("--cached".to_string());
    }
    if let Some(path) = path {
        argv.push("--".to_string());
        argv.push(path);
    }
    run_git_owned(workspace, argv).await
}

async fn git_commit(workspace: &Path, args: &serde_json::Value) -> (String, bool) {
    let message = args["message"].as_str().unwrap_or("").trim();
    if message.is_empty() || message.chars().count() > 500 {
        return (
            "git_commit: message must contain 1 to 500 characters".into(),
            false,
        );
    }
    let Some(raw_paths) = args["paths"].as_array() else {
        return ("git_commit: paths must be a non-empty array".into(), false);
    };
    if raw_paths.is_empty() || raw_paths.len() > 200 {
        return (
            "git_commit: paths must contain 1 to 200 entries".into(),
            false,
        );
    }
    let mut paths = Vec::with_capacity(raw_paths.len());
    for value in raw_paths {
        let Some(path) = value.as_str() else {
            return ("git_commit: every path must be a string".into(), false);
        };
        match safe_relative_path(workspace, path) {
            Ok(path) if !paths.contains(&path) => paths.push(path),
            Ok(_) => {}
            Err(error) => return (error, false),
        }
    }

    let mut add = vec!["add".to_string(), "--".to_string()];
    add.extend(paths.iter().cloned());
    let (add_output, add_ok) = run_git_owned(workspace, add).await;
    if !add_ok {
        return (format!("git_commit staging failed:\n{add_output}"), false);
    }

    // --only ensures unrelated paths that were already staged are not included.
    let mut commit = vec![
        "commit".to_string(),
        "--only".to_string(),
        "-m".to_string(),
        message.to_string(),
        "--".to_string(),
    ];
    commit.extend(paths);
    run_git_owned(workspace, commit).await
}

async fn git_push(workspace: &Path, args: &serde_json::Value) -> (String, bool) {
    let remote = args["remote"].as_str().unwrap_or("origin").trim();
    if !safe_ref_component(remote) {
        return ("git_push: invalid remote name".into(), false);
    }
    let branch = match args["branch"].as_str().map(str::trim) {
        Some(branch) if !branch.is_empty() => branch.to_string(),
        _ => {
            let (output, ok) = run_git(workspace, &["branch", "--show-current"]).await;
            if !ok || output.trim().is_empty() {
                return (
                    "git_push: cannot determine the current branch".into(),
                    false,
                );
            }
            output.trim().to_string()
        }
    };
    if !safe_ref_component(&branch) {
        return ("git_push: invalid branch name".into(), false);
    }
    run_git_owned(workspace, vec!["push".into(), remote.to_string(), branch]).await
}

fn safe_relative_path(workspace: &Path, requested: &str) -> Result<String, String> {
    let requested = requested.trim();
    if requested.is_empty() {
        return Err("git broker: path cannot be empty".into());
    }
    match sandbox::check_write(
        SandboxPolicy::WorkspaceWrite,
        workspace,
        Path::new(requested),
    ) {
        PathCheck::Denied(error) => Err(format!("git broker: {error}")),
        PathCheck::Ok(absolute) => absolute
            .strip_prefix(workspace)
            .map(|path| path.to_string_lossy().to_string())
            .map_err(|_| "git broker: path is outside the workspace".to_string()),
    }
}

fn safe_ref_component(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && !value.contains("..")
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._/-".contains(character))
}

async fn run_git(workspace: &Path, args: &[&str]) -> (String, bool) {
    run_git_owned(
        workspace,
        args.iter().map(|arg| (*arg).to_string()).collect(),
    )
    .await
}

async fn run_git_owned(workspace: &Path, args: Vec<String>) -> (String, bool) {
    if !workspace.join(".git").exists() {
        return (
            "git broker: workspace is not a Git repository".into(),
            false,
        );
    }

    let mut command = tokio::process::Command::new("git");
    command
        .args(&args)
        .current_dir(workspace)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    augment_shell_env(&mut command);
    #[cfg(unix)]
    command.process_group(0);

    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return (format!("git broker spawn error: {error}"), false),
    };
    #[cfg(unix)]
    let process_group = child.id().map(|id| id as i32);

    match tokio::time::timeout(GIT_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            let mut text = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.trim().is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str("[stderr] ");
                text.push_str(&stderr);
            }
            let text = text.chars().take(OUTPUT_LIMIT).collect::<String>();
            let text = if text.trim().is_empty() {
                "(no output)".to_string()
            } else {
                text
            };
            (text, output.status.success())
        }
        Ok(Err(error)) => (format!("git broker error: {error}"), false),
        Err(_) => {
            #[cfg(unix)]
            if let Some(process_group) = process_group {
                unsafe {
                    libc::killpg(process_group, libc::SIGKILL);
                }
            }
            ("git broker timed out after 120 seconds".into(), false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn rejects_paths_outside_workspace_and_protected_state() {
        let root = PathBuf::from("/tmp/project");
        assert!(safe_relative_path(&root, "src/lib.rs").is_ok());
        assert!(safe_relative_path(&root, "../secret").is_err());
        assert!(safe_relative_path(&root, ".git/config").is_err());
        assert!(safe_relative_path(&root, ".oxide/memory").is_err());
    }

    #[test]
    fn rejects_option_injection_in_remote_and_branch() {
        assert!(safe_ref_component("origin"));
        assert!(safe_ref_component("feature/git-broker"));
        assert!(!safe_ref_component("--force"));
        assert!(!safe_ref_component("main:evil"));
        assert!(!safe_ref_component("../outside"));
    }

    #[test]
    fn mutating_specs_require_router_approval() {
        let specs = specs();
        assert!(
            !specs
                .iter()
                .find(|tool| tool.name == "git_status")
                .unwrap()
                .mutating
        );
        assert!(
            !specs
                .iter()
                .find(|tool| tool.name == "git_diff")
                .unwrap()
                .mutating
        );
        assert!(
            specs
                .iter()
                .find(|tool| tool.name == "git_commit")
                .unwrap()
                .mutating
        );
        assert!(
            specs
                .iter()
                .find(|tool| tool.name == "git_push")
                .unwrap()
                .mutating
        );
    }
}
