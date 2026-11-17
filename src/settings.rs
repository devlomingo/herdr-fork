//! `$HERDR_PLUGIN_CONFIG_DIR/config.json`:
//!
//! ```json
//! {"direction": "right",
//!  "commands": {"claude": "claude", "codex": "codex"},
//!  "whole_session_forks": {"opencode": ["opencode", "--session", "{id}", "--fork"]}}
//! ```

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Where the fork opens next to the original: "right" or "down".
    pub direction: String,
    /// Per-agent binary that replaces the default name in resume commands.
    pub commands: BTreeMap<String, String>,
    /// Native whole-session fork commands for agents without an adapter,
    /// argv with `{id}` for the session id.
    pub whole_session_forks: BTreeMap<String, Vec<String>>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            direction: "right".into(),
            commands: BTreeMap::new(),
            whole_session_forks: BTreeMap::new(),
        }
    }
}

impl Settings {
    pub fn command_for(&self, agent: &str) -> String {
        self.commands.get(agent).cloned().unwrap_or_else(|| agent.to_string())
    }

    pub fn load_from_env() -> Result<Settings> {
        match std::env::var_os("HERDR_PLUGIN_CONFIG_DIR") {
            Some(dir) => Settings::load(&Path::new(&dir).join("config.json")),
            None => Ok(Settings::default()),
        }
    }

    pub fn load(path: &Path) -> Result<Settings> {
        if !path.is_file() {
            return Ok(Settings::default());
        }
        let text = std::fs::read_to_string(path)?;
        Settings::parse(&text).with_context(|| format!("reading {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Settings> {
        let v: Value = serde_json::from_str(text)?;
        let mut s = Settings::default();
        if let Some(d) = v.get("direction").and_then(Value::as_str) {
            if d != "right" && d != "down" {
                bail!("direction must be \"right\" or \"down\", not {d:?}");
            }
            s.direction = d.to_string();
        }
        if let Some(m) = v.get("commands").and_then(Value::as_object) {
            for (k, v) in m {
                if let Some(cmd) = v.as_str().filter(|c| !c.is_empty()) {
                    s.commands.insert(k.clone(), cmd.to_string());
                }
            }
        }
        if let Some(m) = v.get("whole_session_forks").and_then(Value::as_object) {
            for (k, v) in m {
                let argv: Vec<String> = v
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                if argv.is_empty() || !argv.iter().any(|a| a.contains("{id}")) {
                    bail!("whole_session_forks.{k} must be an argv array containing \"{{id}}\"");
                }
                s.whole_session_forks.insert(k.clone(), argv);
            }
        }
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_overrides() {
        let s = Settings::parse(r#"{"direction":"down","commands":{"claude":"/opt/claude"}}"#).unwrap();
        assert_eq!(s.direction, "down");
        assert_eq!(s.command_for("claude"), "/opt/claude");
        assert_eq!(s.command_for("codex"), "codex");
        assert!(Settings::parse(r#"{"direction":"left"}"#).is_err());
        assert!(Settings::parse(r#"{"whole_session_forks":{"x":["x","--fork"]}}"#).is_err());
    }
}
