//! Lifecycle hooks — shell commands fired at agent events, configured in
//! `.oxide/hooks.toml`. The payload (JSON) is passed via `$OXIDE_HOOK_PAYLOAD`.
//!
//! Backward-compatible shape:
//!
//! ```toml
//! pre_tool  = ["./scripts/guard.sh"]      # non-zero exit blocks the tool
//! post_tool = ["cargo fmt"]
//! stop      = ["cargo test"]
//! ```
//!
//! Codex-like shape:
//!
//! ```toml
//! [auto]
//! guard_dangerous_shell = true
//! lint = true
//! summarize = true
//!
//! [[hooks.PreToolUse]]
//! matcher = "shell"
//!
//! [[hooks.PreToolUse.hooks]]
//! type = "command"
//! command = "./scripts/guard.sh"
//! timeout = 30
//! statusMessage = "Checking shell command"
//! async = false
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

const DCG_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1500);

#[derive(Clone, Debug)]
pub struct HookCommand {
    pub command: String,
    pub matcher: String,
    pub timeout: u64,
    pub status_message: String,
    pub background: bool,
}

#[derive(Clone, Debug)]
pub struct HookAuto {
    pub guard_dangerous_shell: bool,
    pub lint: bool,
    pub lint_command: String,
    pub summarize: bool,
}

#[derive(Default)]
pub struct Hooks {
    map: HashMap<String, Vec<HookCommand>>,
    auto: HookAuto,
}

impl Default for HookAuto {
    fn default() -> Self {
        Self {
            guard_dangerous_shell: true,
            lint: false,
            lint_command: String::new(),
            summarize: false,
        }
    }
}

impl Hooks {
    pub fn load(workspace: &Path) -> Self {
        if let Ok(text) = std::fs::read_to_string(workspace.join(".oxide/hooks.toml")) {
            return Self::from_text(&text).unwrap_or_default();
        }
        Self::default()
    }

    pub fn from_text(text: &str) -> Result<Self, String> {
        let value = text
            .parse::<toml::Value>()
            .map_err(|error| format!("invalid hooks.toml: {error}"))?;
        let toml::Value::Table(table) = value else {
            return Err("hooks.toml must contain a TOML table".to_string());
        };
        let mut hooks = Self {
            map: HashMap::new(),
            auto: HookAuto::default(),
        };
        hooks.load_table(table);
        Ok(hooks)
    }

    pub fn auto(&self) -> &HookAuto {
        &self.auto
    }

    pub fn commands_for(&self, event: &str, matcher: &str) -> Vec<HookCommand> {
        let event = normalize_event(event);
        self.map
            .get(&event)
            .into_iter()
            .flatten()
            .filter(|cmd| matcher_matches(&cmd.matcher, matcher))
            .cloned()
            .collect()
    }

    fn load_table(&mut self, table: toml::map::Map<String, toml::Value>) {
        for (key, value) in &table {
            if key == "hooks" || key == "auto" {
                continue;
            }
            let event = normalize_event(key);
            for command in simple_commands(value) {
                self.push(
                    &event,
                    HookCommand {
                        command,
                        matcher: String::new(),
                        timeout: 60,
                        status_message: String::new(),
                        background: false,
                    },
                );
            }
        }

        if let Some(auto) = table.get("auto").and_then(|value| value.as_table()) {
            if let Some(value) = auto.get("guard_dangerous_shell").and_then(|v| v.as_bool()) {
                self.auto.guard_dangerous_shell = value;
            }
            if let Some(value) = auto.get("lint").and_then(|v| v.as_bool()) {
                self.auto.lint = value;
            }
            if let Some(value) = auto.get("summarize").and_then(|v| v.as_bool()) {
                self.auto.summarize = value;
            }
            if let Some(value) = auto
                .get("lint_command")
                .or_else(|| auto.get("lintCommand"))
                .and_then(|v| v.as_str())
            {
                self.auto.lint_command = value.to_string();
            }
        }

        let Some(hooks) = table.get("hooks").and_then(|value| value.as_table()) else {
            return;
        };
        for (event_name, value) in hooks {
            let event = normalize_event(event_name);
            match value {
                toml::Value::String(_) | toml::Value::Array(_) => {
                    for command in simple_commands(value) {
                        self.push(
                            &event,
                            HookCommand {
                                command,
                                matcher: String::new(),
                                timeout: 60,
                                status_message: String::new(),
                                background: false,
                            },
                        );
                    }
                }
                toml::Value::Table(table) => {
                    for command in command_hooks(table, "") {
                        self.push(&event, command);
                    }
                    if let Some(hooks) = table.get("hooks").and_then(|value| value.as_array()) {
                        for item in hooks.iter().filter_map(|value| value.as_table()) {
                            for command in command_hooks(item, "") {
                                self.push(&event, command);
                            }
                        }
                    }
                }
                _ => {}
            }
            if let Some(groups) = value.as_array() {
                for group in groups.iter().filter_map(|value| value.as_table()) {
                    let matcher = group
                        .get("matcher")
                        .and_then(|value| value.as_str())
                        .unwrap_or("")
                        .to_string();
                    if let Some(hooks) = group.get("hooks").and_then(|value| value.as_array()) {
                        for item in hooks.iter().filter_map(|value| value.as_table()) {
                            for command in command_hooks(item, &matcher) {
                                self.push(&event, command);
                            }
                        }
                    }
                }
            }
        }
    }

    fn push(&mut self, event: &str, command: HookCommand) {
        if !command.command.trim().is_empty() {
            self.map.entry(event.to_string()).or_default().push(command);
        }
    }
}

pub fn dangerous_tool_reason(tool: &str, args: &serde_json::Value) -> Option<String> {
    if tool != "shell" {
        return None;
    }
    let command = args.get("command")?.as_str()?.trim();
    let compact = command.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = compact.to_ascii_lowercase();
    let dangerous = [
        ("rm -rf /", "recursive delete from filesystem root"),
        ("rm -rf /*", "recursive delete from filesystem root"),
        ("sudo rm", "privileged delete"),
        ("git reset --hard", "destructive git reset"),
        ("git checkout --", "destructive git checkout"),
        ("git clean -fd", "destructive git clean"),
        ("chmod -r 777", "broad permission weakening"),
        (":(){", "fork bomb"),
    ];
    dangerous.iter().find_map(|(needle, reason)| {
        lower
            .contains(needle)
            .then(|| format!("blocked dangerous shell command ({reason}): {command}"))
    })
}

/// Locate a user-installed Destructive Command Guard. Oxide intentionally does
/// not bundle DCG; when absent, the built-in catastrophic rules remain active.
pub fn dcg_binary() -> Option<PathBuf> {
    if let Some(value) = std::env::var_os("DCG_BIN").filter(|value| !value.is_empty()) {
        let candidate = PathBuf::from(value);
        if candidate.components().count() > 1 || candidate.is_absolute() {
            return candidate.is_file().then_some(candidate);
        }
        return find_on_path(&candidate);
    }
    find_on_path(Path::new("dcg")).or_else(|| {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        [
            home.as_ref().map(|path| path.join(".local/bin/dcg")),
            home.as_ref().map(|path| path.join(".cargo/bin/dcg")),
            Some(PathBuf::from("/opt/homebrew/bin/dcg")),
            Some(PathBuf::from("/usr/local/bin/dcg")),
        ]
        .into_iter()
        .flatten()
        .find(|candidate| candidate.is_file())
    })
}

fn find_on_path(name: &Path) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Evaluate an Oxide-native shell call with a user-installed DCG. A denial is
/// returned as a tool result; missing binaries, timeouts, and DCG errors fail
/// open so the fallback guard/sandbox/approval layers continue to work.
pub async fn dcg_tool_reason(tool: &str, args: &serde_json::Value) -> Option<String> {
    let binary = dcg_binary()?;
    dcg_tool_reason_with_binary(&binary, tool, args).await
}

async fn dcg_tool_reason_with_binary(
    binary: &Path,
    tool: &str,
    args: &serde_json::Value,
) -> Option<String> {
    if tool != "shell" {
        return None;
    }
    let command = args.get("command")?.as_str()?.trim();
    if command.is_empty() {
        return None;
    }
    let mut child = tokio::process::Command::new(binary);
    child
        .args(["--robot", "test", command])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = match tokio::time::timeout(DCG_TIMEOUT, child.output()).await {
        Ok(Ok(output)) => output,
        _ => return None,
    };
    if output.status.code() != Some(1) {
        return None;
    }
    Some(format_dcg_denial(command, &output.stdout))
}

fn format_dcg_denial(command: &str, stdout: &[u8]) -> String {
    let parsed = serde_json::from_slice::<serde_json::Value>(stdout).ok();
    let reason = parsed
        .as_ref()
        .and_then(|value| value.get("reason"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("destructive command matched an enabled DCG rule");
    let rule = parsed
        .as_ref()
        .and_then(|value| value.get("rule_id"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty());
    match rule {
        Some(rule) => {
            format!("blocked by destructive command guard [{rule}]: {reason}\ncommand: {command}")
        }
        None => format!("blocked by destructive command guard: {reason}\ncommand: {command}"),
    }
}

fn simple_commands(value: &toml::Value) -> Vec<String> {
    match value {
        toml::Value::String(command) => vec![command.clone()],
        toml::Value::Array(items) => items
            .iter()
            .filter_map(|item| item.as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    }
}

fn command_hooks(
    table: &toml::map::Map<String, toml::Value>,
    inherited_matcher: &str,
) -> Vec<HookCommand> {
    let hook_type = table
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("command");
    let is_async = table
        .get("async")
        .or_else(|| table.get("background"))
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let Some(command) = table.get("command").and_then(|value| value.as_str()) else {
        return Vec::new();
    };
    if hook_type != "command" {
        return Vec::new();
    }
    vec![HookCommand {
        command: command.to_string(),
        matcher: table
            .get("matcher")
            .and_then(|value| value.as_str())
            .unwrap_or(inherited_matcher)
            .to_string(),
        timeout: table
            .get("timeout")
            .and_then(|value| value.as_integer())
            .and_then(|value| u64::try_from(value).ok())
            .filter(|value| *value > 0)
            .unwrap_or(60),
        status_message: table
            .get("statusMessage")
            .or_else(|| table.get("status_message"))
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string(),
        background: is_async,
    }]
}

fn normalize_event(event: &str) -> String {
    match event {
        "PreToolUse" | "pre_tool" => "pre_tool",
        "PostToolUse" | "post_tool" => "post_tool",
        "Stop" | "stop" => "stop",
        "SubagentStart" | "subagent_start" => "subagent_start",
        "SubagentStop" | "subagent_stop" => "subagent_stop",
        other => other,
    }
    .to_string()
}

fn matcher_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() || pattern == "*" {
        return true;
    }
    pattern.split('|').any(|part| {
        let part = part.trim().trim_start_matches('^').trim_end_matches('$');
        if let Some(prefix) = part.strip_suffix(".*") {
            value.starts_with(prefix)
        } else {
            part == value
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_codex_style_hooks_with_matcher() {
        let text = r#"
[auto]
lint = true
summarize = true

[[hooks.PreToolUse]]
matcher = "shell|mcp__fs__.*"

[[hooks.PreToolUse.hooks]]
type = "command"
command = "./guard.sh"
timeout = 12
statusMessage = "Guard"
"#;
        let mut hooks = Hooks {
            map: HashMap::new(),
            auto: HookAuto::default(),
        };
        hooks.load_table(
            text.parse::<toml::Value>()
                .unwrap()
                .as_table()
                .unwrap()
                .clone(),
        );

        let shell = hooks.commands_for("pre_tool", "shell");
        let read = hooks.commands_for("pre_tool", "read_file");

        assert!(hooks.auto().lint);
        assert!(hooks.auto().summarize);
        assert_eq!(shell.len(), 1);
        assert_eq!(shell[0].timeout, 12);
        assert_eq!(shell[0].status_message, "Guard");
        assert!(read.is_empty());
    }

    #[test]
    fn parses_background_command_hooks() {
        let text = r#"
[[hooks.Stop.hooks]]
type = "command"
command = "./summarize.sh"
async = true
"#;
        let mut hooks = Hooks {
            map: HashMap::new(),
            auto: HookAuto::default(),
        };
        hooks.load_table(
            text.parse::<toml::Value>()
                .unwrap()
                .as_table()
                .unwrap()
                .clone(),
        );

        let stop = hooks.commands_for("stop", "");

        assert_eq!(stop.len(), 1);
        assert!(stop[0].background);
    }

    #[test]
    fn blocks_known_dangerous_shell_commands() {
        let reason = dangerous_tool_reason(
            "shell",
            &serde_json::json!({ "command": "git reset --hard HEAD" }),
        );
        assert!(reason.unwrap().contains("destructive git reset"));
    }

    #[test]
    fn formats_dcg_robot_denial_with_rule_id() {
        let reason = format_dcg_denial(
            "git reset --hard HEAD",
            br#"{"reason":"would discard changes","rule_id":"core.git.reset-hard"}"#,
        );
        assert!(reason.contains("core.git.reset-hard"));
        assert!(reason.contains("would discard changes"));
        assert!(reason.contains("git reset --hard HEAD"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dcg_denial_blocks_shell_but_errors_fail_open() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!("oxide-dcg-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let deny = root.join("deny-dcg");
        std::fs::write(
            &deny,
            "#!/bin/sh\nprintf '%s' '{\"reason\":\"destructive reset\",\"rule_id\":\"core.git.reset-hard\"}'\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&deny, std::fs::Permissions::from_mode(0o755)).unwrap();
        let args = serde_json::json!({ "command": "git reset --hard HEAD" });

        let blocked = dcg_tool_reason_with_binary(&deny, "shell", &args)
            .await
            .unwrap();
        assert!(blocked.contains("core.git.reset-hard"));

        let error = root.join("error-dcg");
        std::fs::write(&error, "#!/bin/sh\nexit 3\n").unwrap();
        std::fs::set_permissions(&error, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(dcg_tool_reason_with_binary(&error, "shell", &args)
            .await
            .is_none());

        let _ = std::fs::remove_dir_all(root);
    }
}
