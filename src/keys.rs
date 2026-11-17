//! The setup-keys / remove-keys actions: one marked block in herdr's
//! config.toml binding prefix+shift+f to the fork action.
//!
//! The block is always appended at the end of the file. A `[[keys.command]]`
//! table placed above top-level keys would swallow them (the 0.1.0 bug).

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::herdr;
use crate::util::home;

pub const KEY: &str = "prefix+shift+f";
pub const BEGIN: &str = "# >>> devlomingo.herdr-fork keys (managed by the herdr-fork setup-keys / remove-keys actions)";
pub const END: &str = "# <<< devlomingo.herdr-fork keys";

fn block() -> String {
    format!(
        "{BEGIN}\n[[keys.command]]\nkey = \"{KEY}\"\ntype = \"plugin_action\"\ncommand = \"devlomingo.herdr-fork.fork\"\ndescription = \"herdr-fork: fork the conversation from a message\"\n{END}\n"
    )
}

pub fn config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("HERDR_CONFIG_PATH") {
        return PathBuf::from(p);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config"));
    base.join("herdr").join("config.toml")
}

/// The file without our block, and whether it had one.
fn strip(text: &str) -> (String, bool) {
    let mut out = String::new();
    let mut skipping = false;
    let mut found = false;
    for line in text.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\n', '\r']);
        if bare == BEGIN {
            skipping = true;
            found = true;
        } else if bare == END && skipping {
            skipping = false;
        } else if !skipping {
            out.push_str(line);
        }
    }
    (out, found)
}

fn key_taken(text: &str) -> bool {
    text.lines().any(|l| {
        let l = l.trim();
        !l.starts_with('#') && l.starts_with("key") && l.contains(&format!("\"{KEY}\""))
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Write(String, String),
    Unchanged(String),
}

pub fn plan_setup(text: &str) -> Outcome {
    let (rest, found) = strip(text);
    if found {
        return Outcome::Unchanged(format!("{KEY} is already set up for herdr-fork"));
    }
    if key_taken(&rest) {
        return Outcome::Unchanged(format!(
            "{KEY} is already bound in your config; left it alone. Bind devlomingo.herdr-fork.fork to another key yourself"
        ));
    }
    let sep = match rest.as_str() {
        "" => "",
        r if r.ends_with('\n') => "\n",
        _ => "\n\n",
    };
    Outcome::Write(format!("{rest}{sep}{}", block()), format!("{KEY} now forks the focused conversation"))
}

pub fn plan_remove(text: &str) -> Outcome {
    let (mut rest, found) = strip(text);
    if !found {
        return Outcome::Unchanged("no herdr-fork keybinding to remove".into());
    }
    // Undo the blank line setup added in front of the block.
    if rest.ends_with("\n\n") {
        rest.pop();
    }
    Outcome::Write(rest, format!("{KEY} binding removed"))
}

fn apply(path: &Path, outcome: Outcome) -> Result<String> {
    match outcome {
        Outcome::Unchanged(msg) => Ok(msg),
        Outcome::Write(text, msg) => {
            if path.is_file() {
                let mut bak = path.as_os_str().to_owned();
                bak.push(".bak-herdr-fork");
                std::fs::copy(path, PathBuf::from(bak))?;
            }
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, text)?;
            herdr::reload_config();
            Ok(msg)
        }
    }
}

pub fn run(remove: bool) -> Result<()> {
    let path = config_path();
    let text = if path.is_file() { std::fs::read_to_string(&path)? } else { String::new() };
    let plan = if remove { plan_remove(&text) } else { plan_setup(&text) };
    let msg = apply(&path, plan)?;
    herdr::notify(&msg);
    println!("herdr-fork: {msg} ({})", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "theme = \"dark\"\nprefix = \"ctrl+b\"\n\n[ui]\nmouse_capture = true\n";

    fn written(o: Outcome) -> String {
        match o {
            Outcome::Write(t, _) => t,
            Outcome::Unchanged(m) => panic!("expected a write, got: {m}"),
        }
    }

    #[test]
    fn appends_after_top_level_keys() {
        let out = written(plan_setup(CONFIG));
        assert!(out.starts_with(CONFIG));
        assert!(out.ends_with(&block()));
        let table = out.find("[[keys.command]]").unwrap();
        assert!(out.find("prefix = ").unwrap() < table);
        assert!(out.find("[ui]").unwrap() < table);
    }

    #[test]
    fn twice_gives_one_block() {
        let once = written(plan_setup(CONFIG));
        assert!(matches!(plan_setup(&once), Outcome::Unchanged(_)));
        assert_eq!(once.matches(BEGIN).count(), 1);
    }

    #[test]
    fn remove_restores_the_original() {
        for original in [CONFIG, "", "a = 1\n\n", "x = 2\n"] {
            let once = written(plan_setup(original));
            assert_eq!(written(plan_remove(&once)), original, "for {original:?}");
        }
        assert!(matches!(plan_remove(CONFIG), Outcome::Unchanged(_)));
    }

    #[test]
    fn a_taken_key_is_left_alone() {
        let taken = format!("{CONFIG}\n[[keys.command]]\nkey = \"{KEY}\"\ntype = \"shell\"\ncommand = \"x\"\n");
        match plan_setup(&taken) {
            Outcome::Unchanged(m) => assert!(m.contains("already bound"), "{m}"),
            Outcome::Write(..) => panic!("must not touch a taken key"),
        }
        let commented = format!("{CONFIG}# key = \"{KEY}\"\n");
        assert!(matches!(plan_setup(&commented), Outcome::Write(..)));
    }
}
