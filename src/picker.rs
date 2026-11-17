//! The overlay picker: newest message first, one key to fork.

use std::io::{stdout, Write};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Print, SetAttribute};
use crossterm::{cursor, execute, queue, terminal};

use crate::adapter::{Mode, Prompt};
use crate::util::{clock, truncate_chars};

pub struct Item {
    /// Index into the oldest-first prompt list.
    pub n: usize,
    pub line: String,
}

pub fn items(prompts: &[Prompt]) -> Vec<Item> {
    prompts
        .iter()
        .enumerate()
        .rev()
        .map(|(n, p)| Item { n, line: format!("{:>3}  {}  {}", n + 1, clock(p.time), p.text) })
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
pub enum Choice {
    Fork(usize, Mode),
    Cancel,
}

pub struct Picker {
    pub title: String,
    pub hint: String,
    pub note: Option<String>,
    pub items: Vec<Item>,
    pub sel: usize,
    /// Only Enter and cancel make sense (whole-session fork).
    pub enter_only: bool,
}

impl Picker {
    /// Apply one key; `Some` ends the picker.
    pub fn key(&mut self, k: KeyEvent, page: usize) -> Option<Choice> {
        let last = self.items.len().saturating_sub(1);
        match k.code {
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => return Some(Choice::Cancel),
            KeyCode::Up | KeyCode::Char('k') => self.sel = self.sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.sel = (self.sel + 1).min(last),
            KeyCode::PageUp => self.sel = self.sel.saturating_sub(page),
            KeyCode::PageDown => self.sel = (self.sel + page).min(last),
            KeyCode::Home | KeyCode::Char('g') => self.sel = 0,
            KeyCode::End | KeyCode::Char('G') => self.sel = last,
            KeyCode::Enter => return self.items.get(self.sel).map(|i| Choice::Fork(i.n, Mode::After)),
            KeyCode::Char('b') if !self.enter_only => {
                return self.items.get(self.sel).map(|i| Choice::Fork(i.n, Mode::Before))
            }
            KeyCode::Char('q') | KeyCode::Esc => return Some(Choice::Cancel),
            _ => {}
        }
        None
    }

    fn draw(&self, out: &mut impl Write) -> Result<()> {
        let (w, h) = terminal::size()?;
        let (w, h) = (w as usize, h as usize);
        let fit = |s: &str| truncate_chars(s, w.saturating_sub(1).max(1));
        queue!(out, terminal::Clear(terminal::ClearType::All), cursor::MoveTo(0, 0))?;
        queue!(out, SetAttribute(Attribute::Bold), Print(fit(&self.title)), SetAttribute(Attribute::Reset))?;
        queue!(out, cursor::MoveTo(0, 1), SetAttribute(Attribute::Dim), Print(fit(&self.hint)), SetAttribute(Attribute::Reset))?;
        let mut top_row = 3;
        if let Some(note) = &self.note {
            queue!(out, cursor::MoveTo(0, 2), Print(fit(note)))?;
            top_row = 4;
        }
        let rows = h.saturating_sub(top_row).max(1);
        let top = (self.sel + 1).saturating_sub(rows);
        for (r, item) in self.items.iter().enumerate().skip(top).take(rows) {
            queue!(out, cursor::MoveTo(0, (top_row + r - top) as u16))?;
            if r == self.sel {
                queue!(out, SetAttribute(Attribute::Reverse), Print(fit(&item.line)), SetAttribute(Attribute::Reset))?;
            } else {
                queue!(out, Print(fit(&item.line)))?;
            }
        }
        out.flush()?;
        Ok(())
    }

    pub fn run(mut self) -> Result<Choice> {
        let mut out = stdout();
        terminal::enable_raw_mode()?;
        execute!(out, terminal::EnterAlternateScreen, cursor::Hide)?;
        let result = (|| -> Result<Choice> {
            loop {
                self.draw(&mut out)?;
                if let Event::Key(k) = event::read()? {
                    if k.kind == KeyEventKind::Release {
                        continue;
                    }
                    let page = terminal::size()?.1.saturating_sub(4).max(1) as usize;
                    if let Some(c) = self.key(k, page) {
                        return Ok(c);
                    }
                }
            }
        })();
        let _ = execute!(out, cursor::Show, terminal::LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picker() -> Picker {
        let ps: Vec<Prompt> = ["one", "two", "three"]
            .iter()
            .map(|t| Prompt { text: t.to_string(), raw: t.to_string(), time: None, answer: String::new() })
            .collect();
        Picker { title: String::new(), hint: String::new(), note: None, items: items(&ps), sel: 0, enter_only: false }
    }

    fn k(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    #[test]
    fn newest_first_and_keys_map_to_choices() {
        let mut p = picker();
        assert!(p.items[0].line.starts_with("  3  "));
        assert!(p.items[0].line.ends_with("three"));
        assert_eq!(p.key(k(KeyCode::Char('j')), 10), None);
        assert_eq!(p.key(k(KeyCode::Enter), 10), Some(Choice::Fork(1, Mode::After)));
        assert_eq!(p.key(k(KeyCode::Char('b')), 10), Some(Choice::Fork(1, Mode::Before)));
        p.key(k(KeyCode::Char('G')), 10);
        assert_eq!(p.key(k(KeyCode::Enter), 10), Some(Choice::Fork(0, Mode::After)));
        p.key(k(KeyCode::Char('g')), 10);
        p.key(k(KeyCode::Up), 10);
        assert_eq!(p.sel, 0);
        assert_eq!(p.key(k(KeyCode::Esc), 10), Some(Choice::Cancel));
        assert_eq!(p.key(k(KeyCode::Char('q')), 10), Some(Choice::Cancel));
        p.enter_only = true;
        assert_eq!(p.key(k(KeyCode::Char('b')), 10), None);
    }
}
