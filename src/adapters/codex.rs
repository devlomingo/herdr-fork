//! Codex: threads live in `~/.codex/sessions/YYYY/MM/DD/rollout-*-<id>.jsonl`
//! plus SQLite indexes (`state_5.sqlite`, `thread_history_1.sqlite`).
//!
//! Codex already forks natively and accepts a cut point: its app-server's
//! `thread/fork` takes `lastTurnId` ("fork through this turn, inclusive") and
//! records `forkedFromId` on the new thread. So this adapter edits no files at
//! all: it lists turns with `thread/turns/list`, then asks Codex to fork. The
//! same request backs the TUI's own fork; `codex fork <id>` is the
//! whole-session version.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use crate::adapter::{Adapter, Mode, NewSession, PaneInfo, Prompt, Session};
use crate::util::{collapse, home};

/// One JSON-RPC conversation with Codex.
pub trait Rpc {
    fn call(&mut self, method: &str, params: Value) -> Result<Value>;
}

type RpcFactory = Box<dyn Fn() -> Result<Box<dyn Rpc>>>;

pub struct Codex {
    bin: String,
    home: PathBuf,
    rpc: RpcFactory,
}

impl Codex {
    pub fn new(bin: String) -> Codex {
        let home = std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".codex"));
        let b = bin.clone();
        Codex { bin, home, rpc: Box::new(move || Ok(Box::new(AppServer::start(&b)?) as Box<dyn Rpc>)) }
    }

    #[cfg(test)]
    pub fn with_rpc(home: &Path, rpc: RpcFactory) -> Codex {
        Codex { bin: "codex".into(), home: home.into(), rpc }
    }

    fn rollout(&self, id: &str) -> Option<PathBuf> {
        fn walk(dir: &Path, suffix: &str, depth: u8) -> Option<PathBuf> {
            for e in std::fs::read_dir(dir).ok()?.flatten() {
                let p = e.path();
                if p.is_dir() && depth > 0 {
                    if let Some(f) = walk(&p, suffix, depth - 1) {
                        return Some(f);
                    }
                } else if p.file_name().is_some_and(|n| n.to_string_lossy().ends_with(suffix)) {
                    return Some(p);
                }
            }
            None
        }
        walk(&self.home.join("sessions"), &format!("{id}.jsonl"), 3)
    }

    fn session(&self, id: &str) -> Session {
        Session { id: id.to_string(), path: self.rollout(id).unwrap_or_default() }
    }

    fn turns(&self, rpc: &mut dyn Rpc, thread: &str) -> Result<Vec<Value>> {
        let mut turns = Vec::new();
        let mut cursor = Value::Null;
        loop {
            let r = rpc.call(
                "thread/turns/list",
                json!({"threadId": thread, "sortDirection": "asc", "itemsView": "full", "cursor": cursor}),
            )?;
            turns.extend(r["data"].as_array().cloned().unwrap_or_default());
            match r.get("nextCursor").and_then(Value::as_str) {
                Some(c) => cursor = json!(c),
                None => return Ok(turns),
            }
        }
    }
}

/// Session ids are the trailing UUID of a rollout file name.
fn id_from(value: &str) -> String {
    let name = Path::new(value).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    if value.ends_with(".jsonl") && name.len() >= 36 {
        name[name.len() - 36..].to_string()
    } else {
        value.to_string()
    }
}

fn user_text(turn: &Value) -> Option<String> {
    user_raw(turn).map(|t| collapse(&t))
}

fn user_raw(turn: &Value) -> Option<String> {
    let items = turn["items"].as_array()?;
    let msg = items.iter().find(|i| i["type"] == "userMessage")?;
    let parts: Vec<String> = msg["content"]
        .as_array()?
        .iter()
        .map(|c| match c["type"].as_str() {
            Some("text") => c["text"].as_str().unwrap_or("").to_string(),
            Some("image") | Some("localImage") => "[image]".into(),
            _ => String::new(),
        })
        .filter(|s| !s.is_empty())
        .collect();
    let text = parts.join(" ");
    (!text.trim().is_empty()).then_some(text)
}

fn answer_text(turn: &Value) -> String {
    let texts: Vec<&str> = turn["items"]
        .as_array()
        .map(|items| {
            items.iter().filter(|i| i["type"] == "agentMessage").filter_map(|i| i["text"].as_str()).collect()
        })
        .unwrap_or_default();
    collapse(&texts.join(" "))
}

impl Adapter for Codex {
    fn id(&self) -> &str {
        "codex"
    }

    fn find_session(&self, pane: &PaneInfo) -> Result<Session> {
        // Codex keeps no per-process record. herdr's record comes from Codex's
        // own hook, which ignores `codex exec` children (CODEX_THREAD_ID), so it
        // is the live one; a `codex resume <id>` argv is the fallback.
        if let Some(v) = pane.agent_session.as_deref().filter(|v| !v.is_empty()) {
            return Ok(self.session(&id_from(v)));
        }
        if let Some(id) = pane.argv_value(&["resume"]) {
            return Ok(self.session(&id));
        }
        bail!("could not find this pane's Codex thread (send one message first?)")
    }

    fn session_from_arg(&self, arg: &str) -> Result<Session> {
        Ok(self.session(&id_from(arg)))
    }

    fn prompts(&self, s: &Session) -> Result<Vec<Prompt>> {
        let mut rpc = (self.rpc)()?;
        let turns = self.turns(rpc.as_mut(), &s.id)?;
        Ok(turns
            .iter()
            .filter_map(|t| {
                Some(Prompt {
                    text: user_text(t)?,
                    raw: user_raw(t)?,
                    time: t["startedAt"].as_i64().and_then(|s| DateTime::<Utc>::from_timestamp(s, 0)),
                    answer: answer_text(t),
                })
            })
            .collect())
    }

    fn fork(&self, s: &Session, n: usize, mode: Mode) -> Result<NewSession> {
        let mut rpc = (self.rpc)()?;
        let turns = self.turns(rpc.as_mut(), &s.id)?;
        let pt: Vec<usize> = (0..turns.len()).filter(|&i| user_text(&turns[i]).is_some()).collect();
        if n >= pt.len() {
            bail!("message {} does not exist; this conversation has {}", n + 1, pt.len());
        }
        // Like Claude: "after" runs to just before the next typed message, so
        // follow-up turns without one stay with this turn.
        let last = match mode {
            Mode::After => pt.get(n + 1).map(|&next| next - 1).unwrap_or(turns.len() - 1),
            Mode::Before => match pt[n].checked_sub(1) {
                Some(i) if i >= pt[0] => i,
                _ => bail!("nothing comes before the first message; fork after it instead"),
            },
        };
        let turn_id = turns[last]["id"].as_str().context("turn without an id")?;
        let r = rpc.call(
            "thread/fork",
            json!({"threadId": s.id, "lastTurnId": turn_id, "excludeTurns": true}),
        )?;
        let thread = &r["thread"];
        let id = thread["id"].as_str().context("thread/fork returned no thread id")?.to_string();
        let cwd = thread["cwd"].as_str().or(r["cwd"].as_str()).map(PathBuf::from);
        Ok(NewSession { id, cwd })
    }

    fn resume_command(&self, new: &NewSession) -> Vec<String> {
        vec![self.bin.clone(), "resume".into(), new.id.clone()]
    }

    fn whole_session_command(&self, s: &Session) -> Option<Vec<String>> {
        Some(vec![self.bin.clone(), "fork".into(), s.id.clone()])
    }
}

/// `codex app-server` over stdio, newline-delimited JSON-RPC.
struct AppServer {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    next_id: u64,
}

const TIMEOUT: Duration = Duration::from_secs(30);

impl AppServer {
    fn start(bin: &str) -> Result<AppServer> {
        let mut child = Command::new(bin)
            .arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("starting `{bin} app-server`"))?;
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stdin = child.stdin.take();
        let mut s = AppServer { child, stdin, lines: rx, next_id: 0 };
        s.call("initialize", json!({"clientInfo": {"name": "herdr-fork", "version": env!("CARGO_PKG_VERSION")}}))?;
        s.send(&json!({"jsonrpc": "2.0", "method": "initialized"}))?;
        Ok(s)
    }

    fn send(&mut self, v: &Value) -> Result<()> {
        let stdin = self.stdin.as_mut().context("codex app-server closed")?;
        writeln!(stdin, "{v}")?;
        stdin.flush()?;
        Ok(())
    }
}

impl Rpc for AppServer {
    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        loop {
            let line = self
                .lines
                .recv_timeout(TIMEOUT)
                .map_err(|_| anyhow!("codex app-server did not answer {method}"))?;
            let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
            if msg.get("id") != Some(&json!(id)) || msg.get("method").is_some() {
                continue; // notifications and other traffic
            }
            if let Some(e) = msg.get("error") {
                bail!("codex {method}: {}", e["message"].as_str().unwrap_or(&e.to_string()));
            }
            return Ok(msg["result"].clone());
        }
    }
}

impl Drop for AppServer {
    fn drop(&mut self) {
        self.stdin.take();
        for _ in 0..20 {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A synthetic thread as `thread/turns/list` returns it: an opening turn
    /// with no typed message, three typed prompts (the second with a command
    /// call), and a goal-continuation turn after the second prompt.
    fn turns() -> Value {
        let user = |t: &str| json!({"type": "userMessage", "id": "u", "content": [{"type": "text", "text": t}]});
        let agent = |t: &str| json!({"type": "agentMessage", "id": "a", "text": t});
        json!([
            {"id": "t0", "status": "completed", "items": [agent("ready")]},
            {"id": "t1", "status": "completed", "startedAt": 1794953455, "items": [user("say ONE"), agent("ONE")]},
            {"id": "t2", "status": "completed", "items": [user("say\nTWO"), {"type": "commandExecution", "command": "ls"}, agent("TWO")]},
            {"id": "t2b", "status": "completed", "items": [agent("continuing")]},
            {"id": "t3", "status": "completed", "items": [user("say THREE"), agent("THREE")]}
        ])
    }

    #[derive(Default)]
    struct Log {
        calls: Vec<(String, Value)>,
    }

    struct Fake(Rc<RefCell<Log>>);

    impl Rpc for Fake {
        fn call(&mut self, method: &str, params: Value) -> Result<Value> {
            self.0.borrow_mut().calls.push((method.into(), params.clone()));
            match method {
                // Two pages, to exercise the cursor.
                "thread/turns/list" if params["cursor"].is_null() => {
                    Ok(json!({"data": turns().as_array().unwrap()[..2], "nextCursor": "p2"}))
                }
                "thread/turns/list" => Ok(json!({"data": turns().as_array().unwrap()[2..], "nextCursor": null})),
                "thread/fork" => Ok(json!({"thread": {"id": "new-thread", "forkedFromId": params["threadId"], "cwd": "/w"}})),
                m => bail!("unexpected {m}"),
            }
        }
    }

    fn setup() -> (tempfile::TempDir, Codex, Session, Rc<RefCell<Log>>, Vec<u8>) {
        let dir = tempfile::tempdir().unwrap();
        let day = dir.path().join("sessions/2026/11/18");
        std::fs::create_dir_all(&day).unwrap();
        let id = "01a1eb90-0afa-7142-98f0-83199f5c7542";
        let rollout = day.join(format!("rollout-2026-11-18T01-10-48-{id}.jsonl"));
        std::fs::write(&rollout, "{\"type\":\"session_meta\"}\n").unwrap();
        let bytes = std::fs::read(&rollout).unwrap();
        let log = Rc::new(RefCell::new(Log::default()));
        let l = log.clone();
        let c = Codex::with_rpc(dir.path(), Box::new(move || Ok(Box::new(Fake(l.clone())) as Box<dyn Rpc>)));
        let s = c.session_from_arg(rollout.to_str().unwrap()).unwrap();
        assert_eq!(s.id, id);
        assert_eq!(s.path, rollout);
        (dir, c, s, log, bytes)
    }

    fn forked_through(log: &Rc<RefCell<Log>>) -> Value {
        let calls = &log.borrow().calls;
        let (m, p) = calls.last().unwrap();
        assert_eq!(m, "thread/fork");
        p["lastTurnId"].clone()
    }

    #[test]
    fn lists_only_typed_prompts() {
        let (_d, c, s, _, _) = setup();
        let ps = c.prompts(&s).unwrap();
        let texts: Vec<&str> = ps.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(texts, ["say ONE", "say TWO", "say THREE"]);
        assert_eq!(ps[1].answer, "TWO");
        assert!(ps[0].time.is_some());
    }

    #[test]
    fn after_keeps_the_whole_turn_and_its_follow_ups() {
        let (_d, c, s, log, _) = setup();
        let new = c.fork(&s, 1, Mode::After).unwrap();
        assert_eq!(forked_through(&log), "t2b");
        assert_eq!(new, NewSession { id: "new-thread".into(), cwd: Some("/w".into()) });
        assert_eq!(c.resume_command(&new), ["codex", "resume", "new-thread"]);
    }

    #[test]
    fn before_stops_at_the_previous_answer() {
        let (_d, c, s, log, _) = setup();
        c.fork(&s, 1, Mode::Before).unwrap();
        assert_eq!(forked_through(&log), "t1");
    }

    #[test]
    fn last_message_keeps_everything() {
        let (_d, c, s, log, _) = setup();
        c.fork(&s, 2, Mode::After).unwrap();
        assert_eq!(forked_through(&log), "t3");
    }

    #[test]
    fn new_id_is_used_and_the_original_is_untouched() {
        let (_d, c, s, log, before) = setup();
        let new = c.fork(&s, 0, Mode::After).unwrap();
        assert_eq!(new.id, "new-thread");
        let calls = &log.borrow().calls;
        // Only reads and one native fork of the original id; nothing that writes to it.
        assert!(calls.iter().all(|(m, p)| (m == "thread/turns/list" || m == "thread/fork") && p["threadId"] == s.id));
        assert_eq!(std::fs::read(&s.path).unwrap(), before);
    }

    #[test]
    fn before_first_and_out_of_range_error() {
        let (_d, c, s, _, _) = setup();
        let e = c.fork(&s, 0, Mode::Before).unwrap_err().to_string();
        assert!(e.contains("nothing comes before the first message"), "{e}");
        let e = c.fork(&s, 3, Mode::After).unwrap_err().to_string();
        assert!(e.contains("does not exist"), "{e}");
    }

    #[test]
    fn session_lookup_uses_herdr_then_argv() {
        let (_d, c, s, _, _) = setup();
        let pane = PaneInfo { agent_session: Some(s.id.clone()), ..Default::default() };
        assert_eq!(c.find_session(&pane).unwrap(), s);
        let pane = PaneInfo {
            processes: vec![crate::adapter::Process { pid: 1, argv: vec!["codex".into(), "resume".into(), s.id.clone()] }],
            ..Default::default()
        };
        assert_eq!(c.find_session(&pane).unwrap(), s);
        assert!(c.find_session(&PaneInfo::default()).is_err());
        assert_eq!(c.whole_session_command(&s).unwrap(), ["codex", "fork", s.id.as_str()]);
    }
}
