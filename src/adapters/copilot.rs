//! GitHub Copilot CLI: `~/.copilot/session-state/<id>/` holding `events.jsonl`
//! (one event per line: `session.start`, `user.message`, `assistant.*`, ...),
//! `workspace.yaml`, `checkpoints/` and `files/`.
//!
//! Copilot has no conversation fork of its own; its Esc-Esc rewind restores
//! files from git snapshots. `copilot --resume <id>` replays `events.jsonl`, so
//! a fork is a copy of the session directory whose event log stops at the cut,
//! with the new id in `session.start` and `workspace.yaml`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

use crate::adapter::{Adapter, Mode, NewSession, PaneInfo, Prompt, Session};
use crate::util::{collapse, home, parse_time, write_new};

pub struct Copilot {
    bin: String,
    root: PathBuf,
}

impl Copilot {
    pub fn new(bin: String) -> Copilot {
        Copilot { bin, root: home().join(".copilot") }
    }

    #[cfg(test)]
    pub fn with_root(root: &Path) -> Copilot {
        Copilot { bin: "copilot".into(), root: root.into() }
    }

    fn dir(&self, id: &str) -> PathBuf {
        self.root.join("session-state").join(id)
    }

    fn session(&self, id: &str) -> Option<Session> {
        let path = self.dir(id).join("events.jsonl");
        path.is_file().then(|| Session { id: id.to_string(), path })
    }

    /// Copilot logs to `logs/process-<ms>-<pid>.log` and names the session it
    /// opens ("Workspace initialized: <id>"), including after an in-app /resume.
    fn live_session_id(&self, pid: u32) -> Option<String> {
        let suffix = format!("-{pid}.log");
        let mut logs: Vec<PathBuf> = std::fs::read_dir(self.root.join("logs"))
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().ends_with(&suffix)))
            .collect();
        logs.sort();
        let text = std::fs::read_to_string(logs.last()?).ok()?;
        text.lines().rev().find_map(|l| {
            let rest = l.split("Workspace initialized: ").nth(1)?;
            Some(rest.split_whitespace().next()?.to_string())
        })
    }
}

fn load(path: &Path) -> Result<(Vec<String>, Vec<Value>)> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let raw: Vec<String> = text.lines().filter(|l| !l.trim().is_empty()).map(String::from).collect();
    let vals = raw.iter().map(|l| serde_json::from_str(l).unwrap_or(Value::Null)).collect();
    Ok((raw, vals))
}

fn is_prompt(v: &Value) -> bool {
    v["type"] == "user.message" && v["data"]["content"].as_str().is_some_and(|c| !c.trim().is_empty())
}

fn copy_dir(from: &Path, to: &Path, skip: &dyn Fn(&Path) -> bool) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)?.flatten() {
        let p = e.path();
        if skip(&p) {
            continue;
        }
        let dest = to.join(e.file_name());
        if p.is_dir() {
            copy_dir(&p, &dest, skip)?;
        } else {
            std::fs::copy(&p, &dest)?;
        }
    }
    Ok(())
}

impl Adapter for Copilot {
    fn id(&self) -> &str {
        "copilot"
    }

    fn find_session(&self, pane: &PaneInfo) -> Result<Session> {
        for pid in pane.pids() {
            if let Some(s) = self.live_session_id(pid).and_then(|id| self.session(&id)) {
                return Ok(s);
            }
        }
        let herdr = pane.agent_session.clone();
        let argv = pane.argv_value(&["--resume"]);
        for v in [herdr, argv].into_iter().flatten() {
            if let Ok(s) = self.session_from_arg(&v) {
                return Ok(s);
            }
        }
        bail!("could not find this pane's Copilot session (send one message first?)")
    }

    fn session_from_arg(&self, arg: &str) -> Result<Session> {
        let p = Path::new(arg);
        let id = if p.is_dir() || p.is_file() {
            let dir = if p.is_file() { p.parent().unwrap_or(p) } else { p };
            dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        } else {
            arg.to_string()
        };
        self.session(&id).ok_or_else(|| anyhow!("no Copilot session {id} under {}", self.root.display()))
    }

    fn prompts(&self, s: &Session) -> Result<Vec<Prompt>> {
        let (_, vals) = load(&s.path)?;
        let ps: Vec<usize> = (0..vals.len()).filter(|&i| is_prompt(&vals[i])).collect();
        Ok(ps
            .iter()
            .enumerate()
            .map(|(k, &i)| {
                let end = ps.get(k + 1).copied().unwrap_or(vals.len());
                let answer: Vec<&str> = vals[i + 1..end]
                    .iter()
                    .filter(|v| v["type"] == "assistant.message")
                    .filter_map(|v| v["data"]["content"].as_str())
                    .collect();
                Prompt {
                    text: collapse(vals[i]["data"]["content"].as_str().unwrap_or("")),
                    raw: vals[i]["data"]["content"].as_str().unwrap_or("").to_string(),
                    time: vals[i]["timestamp"].as_str().and_then(parse_time),
                    answer: collapse(&answer.join(" ")),
                }
            })
            .collect())
    }

    fn fork(&self, s: &Session, n: usize, mode: Mode) -> Result<NewSession> {
        let (raw, vals) = load(&s.path)?;
        let ps: Vec<usize> = (0..vals.len()).filter(|&i| is_prompt(&vals[i])).collect();
        if n >= ps.len() {
            bail!("message {} does not exist; this conversation has {}", n + 1, ps.len());
        }
        let end = match mode {
            Mode::After => ps.get(n + 1).copied().unwrap_or(raw.len()),
            Mode::Before => ps[n],
        };
        if !vals[..end].iter().any(is_prompt) {
            bail!("nothing comes before the first message; fork after it instead");
        }
        if vals.first().map(|v| &v["type"]) != Some(&Value::from("session.start")) {
            bail!("{} does not start with session.start; not a Copilot event log this version understands", s.path.display());
        }

        let new_id = uuid::Uuid::new_v4().to_string();
        let src = self.dir(&s.id);
        let dst = self.dir(&new_id);
        if dst.exists() {
            bail!("{} already exists", dst.display());
        }
        // Lock files belong to the running original; the event log is rewritten below.
        copy_dir(&src, &dst, &|p| {
            p == s.path.as_path() || p.extension().is_some_and(|e| e == "lock")
        })?;

        let mut start = vals[0].clone();
        start["data"]["sessionId"] = Value::from(new_id.clone());
        let mut out = serde_json::to_string(&start)? + "\n";
        for line in &raw[1..end] {
            out.push_str(line);
            out.push('\n');
        }
        write_new(&dst.join("events.jsonl"), out.as_bytes())?;

        let ws = dst.join("workspace.yaml");
        let mut cwd = vals[0]["data"]["context"]["cwd"].as_str().map(PathBuf::from);
        if ws.is_file() {
            let text = std::fs::read_to_string(&ws)?;
            let mut lines: Vec<String> = Vec::new();
            for l in text.lines() {
                if l.starts_with("id:") {
                    lines.push(format!("id: {new_id}"));
                } else {
                    if let Some(c) = l.strip_prefix("cwd:") {
                        cwd = Some(PathBuf::from(c.trim()));
                    }
                    lines.push(l.to_string());
                }
            }
            std::fs::write(&ws, lines.join("\n") + "\n")?;
        }
        Ok(NewSession { id: new_id, cwd })
    }

    fn resume_command(&self, new: &NewSession) -> Vec<String> {
        vec![self.bin.clone(), "--resume".into(), new.id.clone()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    const OLD: &str = "82efcca9-d69a-4b1b-82ea-47c179dbfd49";

    /// Shaped like Copilot CLI 0.0.414's events.jsonl, with a tool call in the
    /// second turn and an info event after the last answer.
    fn fixture() -> String {
        let ev = |ty: &str, data: Value| json!({"type": ty, "data": data, "id": "e", "timestamp": "2026-11-17T22:12:55.936Z", "parentId": null});
        let lines = vec![
            json!({"type":"session.start","data":{"sessionId":OLD,"version":1,"producer":"copilot-agent","context":{"cwd":"/w"}},"id":"s","timestamp":"2026-11-17T22:12:40.235Z","parentId":null}),
            ev("user.message", json!({"content":"say ONE","transformedContent":"<x>say ONE</x>","attachments":[]})),
            ev("assistant.turn_start", json!({"turnId":"0"})),
            ev("assistant.message", json!({"messageId":"m1","content":"ONE","toolRequests":[]})),
            ev("assistant.turn_end", json!({"turnId":"0"})),
            ev("user.message", json!({"content":"say\nTWO","attachments":[]})),
            ev("assistant.turn_start", json!({"turnId":"0"})),
            ev("assistant.message", json!({"messageId":"m2","content":"","toolRequests":[{"name":"bash"}]})),
            ev("tool.execution_complete", json!({"result":"ok"})),
            ev("assistant.message", json!({"messageId":"m3","content":"TWO","toolRequests":[]})),
            ev("assistant.turn_end", json!({"turnId":"0"})),
            ev("user.message", json!({"content":"say THREE","attachments":[]})),
            ev("assistant.message", json!({"messageId":"m4","content":"THREE","toolRequests":[]})),
            ev("assistant.turn_end", json!({"turnId":"0"})),
            ev("session.info", json!({"message":"done"})),
        ];
        lines.iter().map(|v| v.to_string() + "\n").collect()
    }

    fn setup() -> (tempfile::TempDir, Copilot, Session, Vec<u8>) {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("session-state").join(OLD);
        fs::create_dir_all(dir.join("checkpoints")).unwrap();
        fs::write(dir.join("events.jsonl"), fixture()).unwrap();
        fs::write(dir.join("workspace.yaml"), format!("id: {OLD}\ncwd: /w\nsummary: say ONE\n")).unwrap();
        fs::write(dir.join("checkpoints/index.md"), "# Checkpoint History\n").unwrap();
        fs::write(dir.join("inuse.lock"), "1").unwrap();
        let c = Copilot::with_root(d.path());
        let s = c.session_from_arg(OLD).unwrap();
        let bytes = fs::read(&s.path).unwrap();
        (d, c, s, bytes)
    }

    fn events(c: &Copilot, new: &NewSession) -> Vec<Value> {
        fs::read_to_string(c.dir(&new.id).join("events.jsonl"))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn kinds(ev: &[Value]) -> Vec<String> {
        ev.iter().map(|v| v["type"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn lists_only_typed_prompts() {
        let (_d, c, s, _) = setup();
        let ps = c.prompts(&s).unwrap();
        let t: Vec<&str> = ps.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(t, ["say ONE", "say TWO", "say THREE"]);
        assert_eq!(ps[1].answer, "TWO");
    }

    #[test]
    fn after_keeps_the_whole_turn_including_tool_calls() {
        let (_d, c, s, _) = setup();
        let new = c.fork(&s, 1, Mode::After).unwrap();
        let ev = events(&c, &new);
        assert_eq!(ev.len(), 11);
        assert_eq!(kinds(&ev).last().unwrap(), "assistant.turn_end");
        assert!(kinds(&ev).contains(&"tool.execution_complete".to_string()));
        assert_eq!(new.cwd.as_deref(), Some(Path::new("/w")));
    }

    #[test]
    fn before_stops_at_the_previous_answer() {
        let (_d, c, s, _) = setup();
        let new = c.fork(&s, 1, Mode::Before).unwrap();
        let ev = events(&c, &new);
        assert_eq!(ev.len(), 5);
        assert_eq!(ev[3]["data"]["content"], "ONE");
    }

    #[test]
    fn last_message_keeps_everything() {
        let (_d, c, s, _) = setup();
        let new = c.fork(&s, 2, Mode::After).unwrap();
        assert_eq!(events(&c, &new).len(), 15);
    }

    #[test]
    fn new_id_is_everywhere_and_the_original_is_unchanged() {
        let (_d, c, s, before) = setup();
        let new = c.fork(&s, 2, Mode::After).unwrap();
        let dir = c.dir(&new.id);
        let ev = fs::read_to_string(dir.join("events.jsonl")).unwrap();
        assert!(!ev.contains(OLD));
        assert!(ev.contains(&new.id));
        let ws = fs::read_to_string(dir.join("workspace.yaml")).unwrap();
        assert!(ws.starts_with(&format!("id: {}\n", new.id)));
        assert!(dir.join("checkpoints/index.md").is_file());
        assert!(!dir.join("inuse.lock").exists());
        assert_eq!(fs::read(&s.path).unwrap(), before);
        assert_eq!(c.resume_command(&new), ["copilot", "--resume", new.id.as_str()]);
    }

    #[test]
    fn before_first_and_out_of_range_error() {
        let (_d, c, s, _) = setup();
        assert!(c.fork(&s, 0, Mode::Before).unwrap_err().to_string().contains("nothing comes before"));
        assert!(c.fork(&s, 9, Mode::After).unwrap_err().to_string().contains("does not exist"));
    }

    #[test]
    fn finds_the_live_session_from_the_process_log() {
        let (d, c, s, _) = setup();
        fs::create_dir_all(d.path().join("logs")).unwrap();
        fs::write(
            d.path().join("logs/process-1794953559610-4242.log"),
            format!("2026 [INFO] Workspace initialized: stale (checkpoints: 0)\n2026 [INFO] Workspace initialized: {OLD} (checkpoints: 0)\n"),
        )
        .unwrap();
        let pane = PaneInfo { foreground_pgid: Some(4242), agent_session: Some("other".into()), ..Default::default() };
        assert_eq!(c.find_session(&pane).unwrap(), s);
        assert!(c.find_session(&PaneInfo::default()).is_err());
    }
}
