//! Lifecycle hooks — shell commands fired at agent events, configured in
//! `.oxide/hooks.toml`. The payload (JSON) is passed via `$OXIDE_HOOK_PAYLOAD`.
//!
//! ```toml
//! pre_tool  = ["./scripts/guard.sh"]      # non-zero exit blocks the tool
//! post_tool = ["cargo fmt"]
//! stop      = ["cargo test"]
//! ```

use std::collections::HashMap;
use std::path::Path;

#[derive(Default)]
pub struct Hooks {
    map: HashMap<String, Vec<String>>,
}

impl Hooks {
    pub fn load(workspace: &Path) -> Self {
        let mut map = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(workspace.join(".oxide/hooks.toml")) {
            if let Ok(toml::Value::Table(t)) = text.parse::<toml::Value>() {
                for (k, v) in t {
                    let cmds = match v {
                        toml::Value::String(s) => vec![s],
                        toml::Value::Array(a) => {
                            a.into_iter().filter_map(|x| x.as_str().map(String::from)).collect()
                        }
                        _ => Vec::new(),
                    };
                    if !cmds.is_empty() {
                        map.insert(k, cmds);
                    }
                }
            }
        }
        Self { map }
    }

    pub fn commands(&self, event: &str) -> &[String] {
        self.map.get(event).map(|v| v.as_slice()).unwrap_or(&[])
    }
}
