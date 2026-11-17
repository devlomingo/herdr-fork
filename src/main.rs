//! herdr-fork: fork any coding agent's conversation from any message, into a
//! new herdr pane.
//!
//! Entry points (see herdr-plugin.toml):
//!   herdr-fork open          action: open the picker over the focused pane
//!   herdr-fork pick          picker pane (needs FORK_SOURCE_PANE)
//!   herdr-fork setup-keys    action: bind prefix+shift+f
//!   herdr-fork remove-keys   action: remove that binding
//!
//! Debugging, no herdr needed:
//!   herdr-fork list <agent> <session-path-or-id>
//!   herdr-fork fork <agent> <session-path-or-id> <n> after|before

mod adapter;
mod adapters;
mod herdr;
mod keys;
mod picker;
mod settings;
mod util;

use std::io::BufRead;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use adapter::{select, Adapter, Mode, Support};
use picker::{Choice, Item, Picker};
use settings::Settings;
use util::{clock, copy_to_clipboard, match_selection, truncate_chars};

const PLUGIN_ID: &str = "devlomingo.herdr-fork";
const USAGE: &str = "usage: herdr-fork pick | open | setup-keys | remove-keys
       herdr-fork list <agent> <session-path-or-id>
       herdr-fork fork <agent> <session-path-or-id> <n> after|before";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");
    let result = match cmd {
        "pick" => {
            // The picker owns its pane: show any error there and wait, or the
            // pane would vanish before anyone could read it.
            if let Err(e) = pick() {
                println!("\n  herdr-fork: {e:#}\n");
                println!("  press enter to close");
                let _ = std::io::stdin().lock().read_line(&mut String::new());
            }
            Ok(())
        }
        "open" => open().inspect_err(|e| herdr::notify(&format!("{e:#}"))),
        "setup-keys" => keys::run(false),
        "remove-keys" => keys::run(true),
        "list" if args.len() == 3 => list(&args[1], &args[2]),
        "fork" if args.len() == 5 => fork_cmd(&args[1], &args[2], &args[3], &args[4]),
        "--version" | "-V" => {
            println!("herdr-fork {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("herdr-fork: {e:#}");
        std::process::exit(1);
    }
}

fn display_name(agent: &str) -> String {
    match agent {
        "claude" => "Claude Code".into(),
        "codex" => "Codex".into(),
        "copilot" => "Copilot CLI".into(),
        other => other.into(),
    }
}

fn message_adapter(agent: &str) -> Result<Box<dyn Adapter>> {
    match select(Some(agent), &Settings::load_from_env()?) {
        Support::Message(a) => Ok(a),
        Support::WholeSession(_) => bail!("{agent} only supports whole-session forks"),
        Support::Unsupported(m) => bail!("{m}"),
    }
}

fn list(agent: &str, session: &str) -> Result<()> {
    let a = message_adapter(agent)?;
    let s = a.session_from_arg(session)?;
    for (n, p) in a.prompts(&s)?.iter().enumerate() {
        println!("{}\t{}\t{}", n + 1, clock(p.time), truncate_chars(&p.text, 100));
    }
    Ok(())
}

fn fork_cmd(agent: &str, session: &str, n: &str, mode: &str) -> Result<()> {
    let a = message_adapter(agent)?;
    let s = a.session_from_arg(session)?;
    let n: usize = n.parse().context("n must be a message number from `list`")?;
    if n == 0 {
        bail!("messages are numbered from 1");
    }
    println!("{}", a.fork(&s, n - 1, Mode::parse(mode)?)?.id);
    Ok(())
}

fn open() -> Result<()> {
    let ctx: Value = std::env::var("HERDR_PLUGIN_CONTEXT_JSON")
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    let source = ctx["focused_pane_id"].as_str().context("no focused pane to fork from")?;
    let plugin = std::env::var("HERDR_PLUGIN_ID").unwrap_or_else(|_| PLUGIN_ID.into());
    let env_source = format!("FORK_SOURCE_PANE={source}");
    let mut args = vec![
        "plugin", "pane", "open", "--plugin", &plugin, "--entrypoint", "picker",
        "--placement", "overlay", "--focus", "--env", &env_source,
    ];
    // Text selected in the pane when the action ran: the picker starts on the
    // message it belongs to.
    let env_sel;
    if let Some(sel) = ctx["selected_text"].as_str().filter(|s| !s.trim().is_empty()) {
        env_sel = format!("FORK_SELECTED_TEXT={}", truncate_chars(sel, 2000));
        args.extend(["--env", env_sel.as_str()]);
    }
    herdr::call(&args)?;
    Ok(())
}

fn pick() -> Result<()> {
    let source = std::env::var("FORK_SOURCE_PANE")
        .ok()
        .filter(|s| !s.is_empty())
        .context("open the picker through the herdr-fork action, not directly")?;
    let settings = Settings::load_from_env()?;
    let pane = herdr::pane_info(&source)?;
    let agent = pane.agent.clone().unwrap_or_default();
    let name = display_name(&agent);

    let whole = |id: String, command: Vec<String>, why: String| -> Result<()> {
        let p = Picker {
            title: format!("herdr-fork · {name} · fork the whole conversation"),
            hint: "enter: fork whole conversation   q: cancel".into(),
            note: Some(why),
            items: vec![Item { n: 0, line: format!("  fork whole conversation {id}") }],
            sel: 0,
            enter_only: true,
        };
        if p.run()? == Choice::Cancel {
            return Ok(());
        }
        let new_pane = herdr::split(&source, &settings.direction, pane.cwd.as_deref())?;
        herdr::run(&new_pane, &command)
    };

    let adapter = match select(pane.agent.as_deref(), &settings) {
        Support::Unsupported(msg) => bail!("{msg}"),
        Support::WholeSession(w) => {
            let id = pane.agent_session.clone().context("herdr has no session id for this pane yet")?;
            let why = format!("{name} can only fork a whole session natively; message-level forking is not supported yet");
            return whole(id.clone(), w.command_for(&id), why);
        }
        Support::Message(a) => a,
    };
    let name = display_name(adapter.id());
    let session = adapter.find_session(&pane)?;
    let prompts = match adapter.prompts(&session) {
        Ok(p) => p,
        Err(e) => match adapter.whole_session_command(&session) {
            Some(cmd) => {
                let why = format!("message-level fork unavailable ({e:#}); {name}'s own fork copies the whole session");
                return whole(session.id.clone(), cmd, why);
            }
            None => return Err(e),
        },
    };
    if prompts.is_empty() {
        bail!("this conversation has no messages yet");
    }

    let items = picker::items(&prompts);
    let mut sel = 0;
    let mut note = None;
    if let Ok(text) = std::env::var("FORK_SELECTED_TEXT") {
        if !text.trim().is_empty() {
            match match_selection(&prompts, &text) {
                Some(n) => {
                    sel = items.iter().position(|i| i.n == n).unwrap_or(0);
                    note = Some(format!("selection is in message {}; enter or b forks from it", n + 1));
                }
                None => note = Some("the selected text is not in any of your messages or answers".into()),
            }
        }
    }
    let picker = Picker {
        title: format!("herdr-fork · {name} · fork from which message?"),
        hint: "enter: fork after (keep its answer)   b: fork before it (message copied)   j/k: move   q: cancel".into(),
        note,
        items,
        sel,
        enter_only: false,
    };
    let Choice::Fork(n, mode) = picker.run()? else {
        return Ok(());
    };
    let new = adapter.fork(&session, n, mode)?;
    if mode == Mode::Before {
        copy_to_clipboard(&prompts[n].raw);
    }
    let new_pane = herdr::split(&source, &settings.direction, new.cwd.as_deref().or(pane.cwd.as_deref()))?;
    herdr::run(&new_pane, &adapter.resume_command(&new))
}
