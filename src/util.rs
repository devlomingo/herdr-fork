use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};

use crate::adapter::Prompt;

/// Collapse every run of whitespace (newlines included) to one space.
pub fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Cut to at most `max` characters, never inside a character. Counting chars,
/// not bytes, keeps Arabic and other multi-byte text intact.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

pub fn clock(t: Option<DateTime<Utc>>) -> String {
    match t {
        Some(t) => t.with_timezone(&Local).format("%H:%M").to_string(),
        None => "     ".into(),
    }
}

pub fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|t| t.with_timezone(&Utc))
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// Create a directory (and missing parents) readable only by the owner, as
/// the agents keep their own session directories.
pub fn private_dir(dir: &Path) -> Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))
}

/// Write a new owner-only file; refuse to replace one that exists so a fork
/// can never clobber another session. Transcripts are private, like the
/// originals (0600).
pub fn write_new(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        private_dir(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    f.write_all(data)?;
    Ok(())
}

pub fn copy_to_clipboard(text: &str) -> bool {
    let candidates: &[&[&str]] = &[
        &["pbcopy"],
        &["wl-copy"],
        &["xclip", "-selection", "clipboard"],
        &["xsel", "--clipboard", "--input"],
    ];
    for argv in candidates {
        let child = Command::new(argv[0])
            .args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut child) = child {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            if child.wait().map(|s| s.success()).unwrap_or(false) {
                return true;
            }
        }
    }
    false
}

pub fn paste_from_clipboard() -> Option<String> {
    let candidates: &[&[&str]] = &[
        &["pbpaste"],
        &["wl-paste", "--no-newline"],
        &["xclip", "-selection", "clipboard", "-o"],
        &["xsel", "--clipboard", "--output"],
    ];
    candidates.iter().find_map(|argv| {
        let out = Command::new(argv[0]).args(&argv[1..]).stderr(Stdio::null()).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    })
}

fn normalize(s: &str) -> String {
    // Selections copied from a pane carry the agent's prompt glyph and bullets.
    let trimmed = s.trim().trim_start_matches(['❯', '›', '>', '●', '⏺', '•', '$', ' ']);
    collapse(trimmed).to_lowercase()
}

/// Which message a selection made in the pane points at: a prompt that
/// contains it (or that it contains) wins, else the turn whose answer contains
/// it. Newest match first, because that is what is on screen.
pub fn match_selection(prompts: &[Prompt], selection: &str) -> Option<usize> {
    let sel = normalize(selection);
    if sel.is_empty() {
        return None;
    }
    let hit = |hay: &str| {
        let hay = normalize(hay);
        !hay.is_empty() && (hay.contains(&sel) || sel.contains(&hay))
    };
    if let Some(i) = (0..prompts.len()).rev().find(|&i| hit(&prompts[i].text)) {
        return Some(i);
    }
    (0..prompts.len()).rev().find(|&i| {
        let ans = normalize(&prompts[i].answer);
        !ans.is_empty() && ans.contains(&sel)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(text: &str, answer: &str) -> Prompt {
        Prompt { text: text.into(), raw: text.into(), time: None, answer: answer.into() }
    }

    #[test]
    fn truncates_by_characters_not_bytes() {
        let ar = "مرحبا بالعالم كيف حالك";
        let t = truncate_chars(ar, 6);
        assert_eq!(t.chars().count(), 6);
        assert!(t.ends_with('…'));
        assert_eq!(truncate_chars("short", 10), "short");
        assert_eq!(collapse("a\n  b\tc "), "a b c");
    }

    #[test]
    fn selection_matches_prompt_then_answer() {
        let ps = vec![p("say ONE", "One"), p("say TWO", "Two is here"), p("say THREE", "Three")];
        assert_eq!(match_selection(&ps, "❯ say TWO"), Some(1));
        assert_eq!(match_selection(&ps, "  say  two\n"), Some(1));
        assert_eq!(match_selection(&ps, "two is"), Some(1));
        assert_eq!(match_selection(&ps, "nothing like it"), None);
        assert_eq!(match_selection(&ps, "   "), None);
        let ar = vec![p("اشرح الكود", ""), p("say x", "")];
        assert_eq!(match_selection(&ar, "اشرح"), Some(0));
    }

    #[test]
    fn new_files_and_dirs_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a/b/c.jsonl");
        write_new(&f, b"x").unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&f), 0o600);
        assert_eq!(mode(&d.path().join("a/b")), 0o700);
        assert!(write_new(&f, b"y").is_err());
    }
}
