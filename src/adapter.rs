//! The adapter contract every agent implements, and the registry that picks one
//! from herdr's agent label.

use std::path::PathBuf;

use anyhow::{bail, Result};
use chrono::{DateTime, Utc};

use crate::adapters::{claude::Claude, codex::Codex, copilot::Copilot};
use crate::settings::Settings;

/// What herdr knows about the pane we fork from.
#[derive(Debug, Clone, Default)]
pub struct PaneInfo {
    pub agent: Option<String>,
    /// herdr's `agent_session.value`: an id, sometimes a path. Can go stale.
    pub agent_session: Option<String>,
    pub cwd: Option<PathBuf>,
    pub foreground_pgid: Option<u32>,
    pub processes: Vec<Process>,
}

#[derive(Debug, Clone, Default)]
pub struct Process {
    pub pid: u32,
    pub argv: Vec<String>,
}

impl PaneInfo {
    /// Pids to check for an agent's live record, foreground group leader first.
    pub fn pids(&self) -> Vec<u32> {
        let mut pids: Vec<u32> = self.foreground_pgid.into_iter().collect();
        for p in &self.processes {
            if !pids.contains(&p.pid) {
                pids.push(p.pid);
            }
        }
        pids
    }

    /// The value after `flag` in any foreground process's argv, e.g. `--resume <id>`.
    pub fn argv_value(&self, flags: &[&str]) -> Option<String> {
        self.processes.iter().find_map(|p| {
            p.argv
                .windows(2)
                .find(|w| flags.contains(&w[0].as_str()))
                .map(|w| w[1].clone())
        })
    }
}

/// One conversation on disk (or in an agent's database).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub path: PathBuf,
}

/// A message the person typed.
#[derive(Debug, Clone, PartialEq)]
pub struct Prompt {
    /// Whitespace collapsed, for the picker.
    pub text: String,
    /// As typed, for the clipboard.
    pub raw: String,
    pub time: Option<DateTime<Utc>>,
    /// The agent's visible reply for this turn; used to match a selection that
    /// was made in an answer rather than in the prompt.
    pub answer: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Keep message n and its whole answer.
    After,
    /// Stop just before message n, as the agents' own rewind does.
    Before,
}

impl Mode {
    pub fn parse(s: &str) -> Result<Mode> {
        match s {
            "after" => Ok(Mode::After),
            "before" => Ok(Mode::Before),
            _ => bail!("mode must be after or before, not {s:?}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSession {
    pub id: String,
    pub cwd: Option<PathBuf>,
}

pub trait Adapter {
    /// herdr's agent label: "claude", "codex", ...
    fn id(&self) -> &str;
    fn find_session(&self, pane: &PaneInfo) -> Result<Session>;
    /// A session from a path or id given on the command line (debug subcommands).
    fn session_from_arg(&self, arg: &str) -> Result<Session>;
    /// The person's typed messages, oldest first.
    fn prompts(&self, s: &Session) -> Result<Vec<Prompt>>;
    fn fork(&self, s: &Session, n: usize, mode: Mode) -> Result<NewSession>;
    /// argv to run in the new pane.
    fn resume_command(&self, new: &NewSession) -> Vec<String>;
    /// The agent's native whole-session fork, offered when message-level
    /// forking is unavailable (for example an older agent version).
    fn whole_session_command(&self, _s: &Session) -> Option<Vec<String>> {
        None
    }
}

/// How far herdr-fork can go for a given agent label.
pub enum Support {
    /// Fork from any message.
    Message(Box<dyn Adapter>),
    /// The agent can only copy a whole session natively; the picker offers
    /// just that one choice.
    WholeSession(WholeSessionFork),
    /// Nothing safe to do; the message says why.
    Unsupported(String),
}

/// A native "copy this whole session" command, e.g. `codex fork <id>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WholeSessionFork {
    pub agent: String,
    /// argv with `{id}` standing for the session id.
    pub command: Vec<String>,
}

impl WholeSessionFork {
    pub fn command_for(&self, session_id: &str) -> Vec<String> {
        self.command.iter().map(|a| a.replace("{id}", session_id)).collect()
    }
}

/// Agents herdr recognises whose storage we have not verified, with a pointer
/// to what they can do natively.
const KNOWN_UNSUPPORTED: &[(&str, &str)] = &[
    ("droid", "use Droid's own /fork or /rewind"),
    ("cursor", "use Cursor Agent's own resume"),
];

pub fn select(agent: Option<&str>, settings: &Settings) -> Support {
    let Some(agent) = agent.filter(|a| !a.is_empty()) else {
        return Support::Unsupported(
            "this pane is not running a coding agent herdr recognises".into(),
        );
    };
    let bin = |name: &str| settings.command_for(name);
    match agent {
        "claude" => Support::Message(Box::new(Claude::new(bin("claude")))),
        "codex" => Support::Message(Box::new(Codex::new(bin("codex")))),
        "copilot" => Support::Message(Box::new(Copilot::new(bin("copilot")))),
        other => {
            if let Some(cmd) = settings.whole_session_forks.get(other) {
                return Support::WholeSession(WholeSessionFork {
                    agent: other.to_string(),
                    command: cmd.clone(),
                });
            }
            let hint = KNOWN_UNSUPPORTED
                .iter()
                .find(|(a, _)| *a == other)
                .map(|(_, h)| format!(" ({h})"))
                .unwrap_or_default();
            Support::Unsupported(format!("{other} is not supported yet{hint}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_adapter_by_agent_label() {
        let s = Settings::default();
        for agent in ["claude", "codex", "copilot"] {
            match select(Some(agent), &s) {
                Support::Message(a) => assert_eq!(a.id(), agent),
                _ => panic!("{agent} should be message-level"),
            }
        }
    }

    #[test]
    fn unknown_agent_is_unsupported_with_a_clear_message() {
        let s = Settings::default();
        match select(Some("kimi"), &s) {
            Support::Unsupported(m) => assert_eq!(m, "kimi is not supported yet"),
            _ => panic!("kimi should be unsupported"),
        }
        match select(Some("droid"), &s) {
            Support::Unsupported(m) => assert!(m.contains("/fork"), "{m}"),
            _ => panic!("droid should be unsupported"),
        }
        match select(None, &s) {
            Support::Unsupported(m) => assert!(m.contains("not running a coding agent")),
            _ => panic!("a shell pane should be unsupported"),
        }
    }

    #[test]
    fn whole_session_fallback_when_only_a_native_fork_exists() {
        let mut s = Settings::default();
        s.whole_session_forks.insert(
            "opencode".into(),
            vec!["opencode".into(), "--session".into(), "{id}".into(), "--fork".into()],
        );
        match select(Some("opencode"), &s) {
            Support::WholeSession(w) => {
                assert_eq!(w.command_for("abc"), ["opencode", "--session", "abc", "--fork"])
            }
            _ => panic!("opencode should fall back to a whole-session fork"),
        }
    }

    #[test]
    fn argv_value_finds_resume_flag() {
        let p = PaneInfo {
            processes: vec![Process {
                pid: 1,
                argv: vec!["codex".into(), "resume".into(), "abc".into()],
            }],
            ..Default::default()
        };
        assert_eq!(p.argv_value(&["resume"]).as_deref(), Some("abc"));
        assert_eq!(p.argv_value(&["--resume"]), None);
    }
}
