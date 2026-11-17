//! Claude Code: `~/.claude/projects/<project>/<session>.jsonl`.
//!
//! Every message line carries `uuid` and `parentUuid`; the conversation is the
//! chain walked back from the newest line. Claude Code's own `/rewind` never
//! rewrites the file: the next prompt simply points its `parentUuid` at an
//! earlier line. Its `/branch` copies that chain into a new file, sets
//! `sessionId` to the new id, adds `forkedFrom: {sessionId, messageUuid}` to
//! every message line, carries over the file-history snapshots, and names the
//! branch "<first prompt> (Branch)". We do the same, cut at the point `/rewind`
//! would have used, and name ours "(Fork)".

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use crate::adapter::{Adapter, Mode, NewSession, PaneInfo, Prompt, Session};
use crate::util::{collapse, home, parse_time, truncate_chars, write_new};

pub struct Claude {
    bin: String,
    root: PathBuf,
}

impl Claude {
    pub fn new(bin: String) -> Claude {
        let root = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".claude"));
        Claude { bin, root }
    }

    #[cfg(test)]
    pub fn with_root(bin: &str, root: &Path) -> Claude {
        Claude { bin: bin.into(), root: root.into() }
    }

    /// Claude's own record of which session a running process is in. It follows
    /// /branch, /resume and /clear, unlike herdr's record, which a `claude -p`
    /// child in the same pane can overwrite.
    fn live_session_id(&self, pid: u32) -> Option<String> {
        let path = self.root.join("sessions").join(format!("{pid}.json"));
        let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
        v.get("sessionId")?.as_str().map(String::from)
    }

    fn transcript(&self, id: &str) -> Option<PathBuf> {
        let projects = self.root.join("projects");
        std::fs::read_dir(projects).ok()?.flatten().find_map(|e| {
            let p = e.path().join(format!("{id}.jsonl"));
            p.is_file().then_some(p)
        })
    }

    fn session_at(path: PathBuf) -> Session {
        let id = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        Session { id, path }
    }
}

impl Adapter for Claude {
    fn id(&self) -> &str {
        "claude"
    }

    fn find_session(&self, pane: &PaneInfo) -> Result<Session> {
        for pid in pane.pids() {
            if let Some(path) = self.live_session_id(pid).and_then(|id| self.transcript(&id)) {
                return Ok(Claude::session_at(path));
            }
        }
        if let Some(v) = &pane.agent_session {
            if v.ends_with(".jsonl") && Path::new(v).is_file() {
                return Ok(Claude::session_at(PathBuf::from(v)));
            }
            if let Some(path) = self.transcript(v) {
                return Ok(Claude::session_at(path));
            }
        }
        bail!("could not find this pane's Claude transcript (send one message first?)")
    }

    fn session_from_arg(&self, arg: &str) -> Result<Session> {
        if Path::new(arg).is_file() {
            return Ok(Claude::session_at(PathBuf::from(arg)));
        }
        self.transcript(arg)
            .map(Claude::session_at)
            .ok_or_else(|| anyhow!("no Claude transcript for {arg} under {}", self.root.display()))
    }

    fn prompts(&self, s: &Session) -> Result<Vec<Prompt>> {
        let t = Transcript::load(&s.path)?;
        let chain = t.chain(t.leaf());
        let ps: Vec<usize> = chain.iter().copied().filter(|&i| is_prompt(&t.lines[i])).collect();
        Ok(ps
            .iter()
            .enumerate()
            .map(|(k, &i)| {
                let end = ps.get(k + 1).copied().unwrap_or(usize::MAX);
                let answer = chain
                    .iter()
                    .filter(|&&j| j > i && j < end)
                    .filter_map(|&j| assistant_text(&t.lines[j]))
                    .collect::<Vec<_>>()
                    .join(" ");
                Prompt {
                    text: prompt_text(&t.lines[i]),
                    raw: raw_prompt(&t.lines[i]),
                    time: t.lines[i].get("timestamp").and_then(Value::as_str).and_then(parse_time),
                    answer,
                }
            })
            .collect())
    }

    fn fork(&self, s: &Session, n: usize, mode: Mode) -> Result<NewSession> {
        let t = Transcript::load(&s.path)?;
        let leaf = t.leaf();
        let chain = t.chain(leaf);
        let ps: Vec<usize> = chain.iter().copied().filter(|&i| is_prompt(&t.lines[i])).collect();
        if n >= ps.len() {
            bail!("message {} does not exist; this conversation has {}", n + 1, ps.len());
        }
        // The same point Claude's /rewind resumes from: the line the next
        // prompt hangs off.
        let cut = match mode {
            Mode::After => match ps.get(n + 1) {
                Some(&next) => t.parent(next),
                None => leaf,
            },
            Mode::Before => t.parent(ps[n]),
        };
        // Hook context written just before a prompt belongs to that prompt's turn.
        let mut cut = cut;
        while let Some(c) = cut {
            if t.lines[c].get("isMeta").and_then(Value::as_bool) == Some(true) {
                cut = t.parent(c);
            } else {
                break;
            }
        }
        let keep = cut.map(|c| t.chain(Some(c))).unwrap_or_default();
        let Some(first_prompt) = keep.iter().copied().find(|&i| is_prompt(&t.lines[i])) else {
            bail!("nothing comes before the first message; fork after it instead");
        };
        let cut = *keep.last().expect("non-empty chain");
        let last_prompt = keep.iter().rev().copied().find(|&i| is_prompt(&t.lines[i])).unwrap();

        let old_id = s.id.clone();
        let new_id = uuid::Uuid::new_v4().to_string();
        let title = format!("{} (Fork)", truncate_chars(&prompt_text(&t.lines[first_prompt]), 60));
        let kept: HashSet<&str> = keep.iter().filter_map(|&i| t.uuid(i)).collect();
        let retag = |v: &Value| -> Value {
            let mut v = v.clone();
            if let Some(o) = v.as_object_mut() {
                if o.contains_key("sessionId") {
                    o.insert("sessionId".into(), json!(new_id));
                }
            }
            v
        };
        let last_of = |ty: &str| t.lines.iter().rev().find(|v| v["type"] == ty);

        let mut out: Vec<Value> = Vec::new();
        if let Some(owner) = last_of("session-owner") {
            out.push(retag(owner));
        }
        for &i in &keep {
            let mut v = retag(&t.lines[i]);
            let uuid = t.uuid(i).unwrap_or_default().to_string();
            v.as_object_mut()
                .unwrap()
                .insert("forkedFrom".into(), json!({"sessionId": old_id, "messageUuid": uuid}));
            out.push(v);
        }
        out.push(json!({"type": "custom-title", "customTitle": title, "sessionId": new_id}));
        out.push(json!({"type": "agent-name", "agentName": title, "sessionId": new_id}));
        for ty in ["mode", "permission-mode"] {
            if let Some(v) = last_of(ty) {
                out.push(retag(v));
            }
        }
        for v in &t.lines {
            if v["type"] == "file-history-snapshot"
                && v.get("messageId").and_then(Value::as_str).is_some_and(|m| kept.contains(m))
            {
                out.push(v.clone());
            }
        }
        out.push(json!({
            "type": "last-prompt",
            "lastPrompt": raw_prompt(&t.lines[last_prompt]),
            "leafUuid": t.uuid(cut).unwrap_or_default(),
            "sessionId": new_id,
        }));

        let mut data = String::new();
        for v in &out {
            data.push_str(&serde_json::to_string(v)?);
            data.push('\n');
        }
        let dir = s.path.parent().context("transcript has no directory")?;
        write_new(&dir.join(format!("{new_id}.jsonl")), data.as_bytes())?;
        write_new(
            &dir.join(&new_id).join("custom-title.json"),
            serde_json::to_string(&json!({"customTitle": title}))?.as_bytes(),
        )?;
        let cwd = [cut, last_prompt]
            .iter()
            .find_map(|&i| t.lines[i].get("cwd").and_then(Value::as_str))
            .map(PathBuf::from);
        Ok(NewSession { id: new_id, cwd })
    }

    fn resume_command(&self, new: &NewSession) -> Vec<String> {
        vec![self.bin.clone(), "--resume".into(), new.id.clone()]
    }
}

struct Transcript {
    lines: Vec<Value>,
    by_uuid: HashMap<String, usize>,
}

impl Transcript {
    fn load(path: &Path) -> Result<Transcript> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        // A line Claude is still writing may not parse yet; it cannot be part of
        // a finished turn, so it is left out rather than failing the fork.
        let lines: Vec<Value> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        let mut by_uuid = HashMap::new();
        for (i, v) in lines.iter().enumerate() {
            if is_sidechain(v) {
                continue;
            }
            if let Some(u) = v.get("uuid").and_then(Value::as_str) {
                by_uuid.insert(u.to_string(), i);
            }
        }
        Ok(Transcript { lines, by_uuid })
    }

    fn uuid(&self, i: usize) -> Option<&str> {
        self.lines[i].get("uuid").and_then(Value::as_str)
    }

    /// The newest line of the main conversation.
    fn leaf(&self) -> Option<usize> {
        (0..self.lines.len()).rev().find(|&i| self.uuid(i).is_some() && !is_sidechain(&self.lines[i]))
    }

    /// The line a message hangs off. A compact boundary has no parent but keeps
    /// a logical one, so history before a compaction stays reachable.
    fn parent(&self, i: usize) -> Option<usize> {
        let v = &self.lines[i];
        ["parentUuid", "logicalParentUuid"]
            .iter()
            .find_map(|k| v.get(*k).and_then(Value::as_str))
            .and_then(|u| self.by_uuid.get(u).copied())
    }

    /// Oldest-first chain ending at `leaf`.
    fn chain(&self, leaf: Option<usize>) -> Vec<usize> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut cur = leaf;
        while let Some(i) = cur {
            if !seen.insert(i) {
                break;
            }
            out.push(i);
            cur = self.parent(i);
        }
        out.reverse();
        out
    }
}

fn is_sidechain(v: &Value) -> bool {
    v.get("isSidechain").and_then(Value::as_bool) == Some(true)
}

fn flag(v: &Value, k: &str) -> bool {
    v.get(k).and_then(Value::as_bool) == Some(true)
}

/// A message the person typed: not a tool result, hook output, sidechain,
/// compact summary or slash-command echo.
pub fn is_prompt(v: &Value) -> bool {
    if v["type"] != "user" || flag(v, "isMeta") || flag(v, "isSidechain") || flag(v, "isCompactSummary") {
        return false;
    }
    match &v["message"]["content"] {
        Value::String(s) => {
            let s = s.trim();
            !s.is_empty() && !s.starts_with("<command-") && !s.starts_with("<local-command")
        }
        Value::Array(blocks) => {
            !blocks.is_empty() && !blocks.iter().any(|b| b["type"] == "tool_result")
        }
        _ => false,
    }
}

fn raw_prompt(v: &Value) -> String {
    match &v["message"]["content"] {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .map(|b| b.get("text").and_then(Value::as_str).unwrap_or("[image]"))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

fn prompt_text(v: &Value) -> String {
    collapse(&raw_prompt(v))
}

fn assistant_text(v: &Value) -> Option<String> {
    if v["type"] != "assistant" {
        return None;
    }
    let blocks = v["message"]["content"].as_array()?;
    let text: Vec<&str> = blocks
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect();
    (!text.is_empty()).then(|| collapse(&text.join(" ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const OLD: &str = "11111111-1111-4111-8111-111111111111";

    /// A synthetic transcript in the shape Claude Code 2.1.326 writes: session
    /// metadata, SessionStart attachments, three typed prompts (the second with
    /// a tool call and a hook-context meta line), api-request records, a
    /// rewound-away branch, and turn bookkeeping lines.
    fn fixture() -> String {
        let s = OLD;
        let lines = vec![
            json!({"type":"last-prompt","leafUuid":"a0","sessionId":s}),
            json!({"type":"mode","mode":"normal","sessionId":s}),
            json!({"type":"permission-mode","permissionMode":"default","sessionId":s}),
            json!({"type":"session-owner","accountUuid":"acc","sessionId":s}),
            json!({"type":"attachment","uuid":"a0","parentUuid":null,"isSidechain":false,"attachment":{"type":"hook_success"},"sessionId":s,"cwd":"/w"}),
            json!({"type":"file-history-snapshot","messageId":"u1","snapshot":{"messageId":"u1"},"isSnapshotUpdate":false}),
            json!({"type":"user","uuid":"u1","parentUuid":"a0","isSidechain":false,"message":{"role":"user","content":"say ONE"},"timestamp":"2026-11-17T22:07:58.999Z","sessionId":s,"cwd":"/w"}),
            json!({"type":"api-request","sessionId":s,"id":"r1"}),
            json!({"type":"assistant","uuid":"x1","parentUuid":"u1","isSidechain":false,"message":{"role":"assistant","content":[{"type":"text","text":"One"}]},"sessionId":s,"session_id":s,"cwd":"/w"}),
            json!({"type":"system","subtype":"turn_duration","uuid":"d1","parentUuid":"x1","isSidechain":false,"isMeta":false,"sessionId":s,"cwd":"/w"}),
            // A branch the person rewound away from: still in the file, not on the chain.
            json!({"type":"user","uuid":"old2","parentUuid":"d1","isSidechain":false,"message":{"role":"user","content":"say OLD"},"sessionId":s}),
            json!({"type":"assistant","uuid":"oldx","parentUuid":"old2","isSidechain":false,"message":{"role":"assistant","content":[{"type":"text","text":"Old"}]},"sessionId":s}),
            json!({"type":"file-history-snapshot","messageId":"u2","snapshot":{"messageId":"u2"},"isSnapshotUpdate":false}),
            json!({"type":"api-request-blob","sessionId":s,"message":{"role":"user","content":"say TWO"}}),
            json!({"type":"user","uuid":"u2","parentUuid":"d1","isSidechain":false,"message":{"role":"user","content":[{"type":"text","text":"say\n  TWO"}]},"timestamp":"2026-11-17T22:08:02.532Z","sessionId":s,"cwd":"/w"}),
            json!({"type":"assistant","uuid":"x2a","parentUuid":"u2","isSidechain":false,"message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]},"sessionId":s}),
            json!({"type":"user","uuid":"r2","parentUuid":"x2a","isSidechain":false,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]},"sessionId":s}),
            json!({"type":"assistant","uuid":"side","parentUuid":"r2","isSidechain":true,"message":{"role":"assistant","content":[{"type":"text","text":"subagent"}]},"sessionId":s}),
            json!({"type":"assistant","uuid":"x2b","parentUuid":"r2","isSidechain":false,"message":{"role":"assistant","content":[{"type":"text","text":"Two"}]},"sessionId":s}),
            json!({"type":"system","subtype":"stop_hook_summary","uuid":"h2","parentUuid":"x2b","isSidechain":false,"sessionId":s,"session_id":s}),
            json!({"type":"user","uuid":"m3","parentUuid":"h2","isSidechain":false,"isMeta":true,"message":{"role":"user","content":"hook context for THREE"},"sessionId":s}),
            json!({"type":"user","uuid":"u3","parentUuid":"m3","isSidechain":false,"message":{"role":"user","content":"say THREE"},"timestamp":"2026-11-17T22:08:06.122Z","sessionId":s,"cwd":"/w"}),
            json!({"type":"user","uuid":"c3","parentUuid":"u3","isSidechain":false,"message":{"role":"user","content":"<command-name>/model</command-name>"},"sessionId":s}),
            json!({"type":"assistant","uuid":"x3","parentUuid":"c3","isSidechain":false,"message":{"role":"assistant","content":[{"type":"text","text":"Three"}]},"sessionId":s}),
            json!({"type":"system","subtype":"turn_duration","uuid":"d3","parentUuid":"x3","isSidechain":false,"sessionId":s}),
            json!({"type":"cost-state","sessionId":s,"totalCostUSD":0.01}),
        ];
        lines.iter().map(|v| v.to_string() + "\n").collect()
    }

    fn setup() -> (tempfile::TempDir, Claude, Session, Vec<u8>) {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("projects").join("-w");
        fs::create_dir_all(&proj).unwrap();
        let path = proj.join(format!("{OLD}.jsonl"));
        fs::write(&path, fixture()).unwrap();
        let bytes = fs::read(&path).unwrap();
        let c = Claude::with_root("claude", dir.path());
        let s = c.session_from_arg(OLD).unwrap();
        (dir, c, s, bytes)
    }

    fn read_fork(s: &Session, new: &NewSession) -> Vec<Value> {
        let p = s.path.parent().unwrap().join(format!("{}.jsonl", new.id));
        fs::read_to_string(p).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    fn uuids(lines: &[Value]) -> Vec<&str> {
        lines.iter().filter_map(|v| v.get("uuid").and_then(Value::as_str)).collect()
    }

    #[test]
    fn lists_only_typed_prompts_on_the_live_chain() {
        let (_d, c, s, _) = setup();
        let ps = c.prompts(&s).unwrap();
        let texts: Vec<&str> = ps.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(texts, ["say ONE", "say TWO", "say THREE"]);
        assert_eq!(ps[1].answer, "Two");
        assert!(ps[0].time.is_some());
    }

    #[test]
    fn after_keeps_the_whole_turn_and_no_trailing_hook_lines() {
        let (_d, c, s, _) = setup();
        let new = c.fork(&s, 1, Mode::After).unwrap();
        let out = read_fork(&s, &new);
        assert_eq!(uuids(&out), ["a0", "u1", "x1", "d1", "u2", "x2a", "r2", "x2b", "h2"]);
        let last = out.last().unwrap();
        assert_eq!(last["type"], "last-prompt");
        assert_eq!(last["leafUuid"], "h2");
        assert_eq!(last["lastPrompt"], "say\n  TWO");
        assert_eq!(new.cwd.as_deref(), Some(Path::new("/w")));
    }

    #[test]
    fn before_stops_at_the_previous_answer() {
        let (_d, c, s, _) = setup();
        let new = c.fork(&s, 1, Mode::Before).unwrap();
        let out = read_fork(&s, &new);
        assert_eq!(uuids(&out), ["a0", "u1", "x1", "d1"]);
        assert_eq!(out.last().unwrap()["leafUuid"], "d1");
    }

    #[test]
    fn last_message_keeps_everything() {
        let (_d, c, s, _) = setup();
        let new = c.fork(&s, 2, Mode::After).unwrap();
        let out = read_fork(&s, &new);
        assert_eq!(
            uuids(&out),
            ["a0", "u1", "x1", "d1", "u2", "x2a", "r2", "x2b", "h2", "m3", "u3", "c3", "x3", "d3"]
        );
    }

    #[test]
    fn new_id_is_everywhere_and_forked_from_points_back() {
        let (_d, c, s, _) = setup();
        let new = c.fork(&s, 2, Mode::After).unwrap();
        let out = read_fork(&s, &new);
        assert!(uuid::Uuid::parse_str(&new.id).is_ok());
        let text = out.iter().map(|v| v.to_string()).collect::<String>();
        for v in &out {
            if let Some(sid) = v.get("sessionId") {
                assert_eq!(sid, &json!(new.id), "{v}");
            }
        }
        // Claude's /branch leaves the snake_case session_id alone; so do we.
        assert!(text.contains(&format!("\"session_id\":\"{OLD}\"")));
    }

    #[test]
    fn fork_matches_the_shape_of_claudes_own_branch() {
        let (_d, c, s, _) = setup();
        let new = c.fork(&s, 1, Mode::After).unwrap();
        let out = read_fork(&s, &new);
        assert_eq!(out[0]["type"], "session-owner");
        for v in out.iter().filter(|v| v.get("uuid").is_some()) {
            assert_eq!(v["forkedFrom"], json!({"sessionId": OLD, "messageUuid": v["uuid"]}));
        }
        let types: Vec<&str> = out.iter().map(|v| v["type"].as_str().unwrap()).collect();
        for gone in ["api-request", "api-request-blob", "cost-state"] {
            assert!(!types.contains(&gone), "{gone} should not be copied");
        }
        assert_eq!(types.iter().filter(|t| **t == "last-prompt").count(), 1);
        let title = out.iter().find(|v| v["type"] == "custom-title").unwrap();
        assert_eq!(title["customTitle"], "say ONE (Fork)");
        assert!(out.iter().any(|v| v["type"] == "agent-name" && v["agentName"] == "say ONE (Fork)"));
        let snaps: Vec<&Value> = out.iter().filter(|v| v["type"] == "file-history-snapshot").map(|v| &v["messageId"]).collect();
        assert_eq!(snaps, [&json!("u1"), &json!("u2")]);
        // Every parent the chain names is in the file, so resume sees one thread.
        let ids: HashSet<&str> = uuids(&out).into_iter().collect();
        for v in &out {
            if let Some(p) = v.get("parentUuid").and_then(Value::as_str) {
                assert!(ids.contains(p), "dangling parent {p}");
            }
        }
        let side = s.path.parent().unwrap().join(&new.id).join("custom-title.json");
        assert_eq!(fs::read_to_string(side).unwrap(), r#"{"customTitle":"say ONE (Fork)"}"#);
    }

    #[test]
    fn original_is_unchanged_byte_for_byte() {
        let (_d, c, s, before) = setup();
        c.fork(&s, 0, Mode::After).unwrap();
        c.fork(&s, 2, Mode::Before).unwrap();
        assert_eq!(fs::read(&s.path).unwrap(), before);
    }

    #[test]
    fn before_first_and_out_of_range_error() {
        let (_d, c, s, _) = setup();
        let e = c.fork(&s, 0, Mode::Before).unwrap_err().to_string();
        assert!(e.contains("nothing comes before the first message"), "{e}");
        let e = c.fork(&s, 3, Mode::After).unwrap_err().to_string();
        assert!(e.contains("does not exist"), "{e}");
    }

    #[test]
    fn finds_the_live_session_before_herdrs_record() {
        let (d, c, s, _) = setup();
        let sessions = d.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(sessions.join("4242.json"), format!(r#"{{"pid":4242,"sessionId":"{OLD}"}}"#)).unwrap();
        let pane = PaneInfo {
            foreground_pgid: Some(4242),
            agent_session: Some("stale-id".into()),
            ..Default::default()
        };
        assert_eq!(c.find_session(&pane).unwrap(), s);
        let pane = PaneInfo { agent_session: Some(OLD.into()), ..Default::default() };
        assert_eq!(c.find_session(&pane).unwrap(), s);
        let pane = PaneInfo { agent_session: Some("nope".into()), ..Default::default() };
        assert!(c.find_session(&pane).is_err());
    }
}
