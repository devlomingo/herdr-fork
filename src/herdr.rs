//! The herdr CLI, called through `$HERDR_BIN_PATH` as plugins should.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::adapter::{PaneInfo, Process};

pub fn bin() -> String {
    std::env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".into())
}

pub fn call(args: &[&str]) -> Result<Value> {
    let out = Command::new(bin()).args(args).output().with_context(|| format!("running {}", bin()))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let msg = if stderr.trim().is_empty() { stdout.trim() } else { stderr.trim() };
        bail!("herdr {} failed: {msg}", args.join(" "));
    }
    if stdout.trim().is_empty() {
        return Ok(Value::Null);
    }
    let v: Value = serde_json::from_str(&stdout).with_context(|| format!("herdr {} printed non-JSON", args.join(" ")))?;
    if let Some(e) = v.get("error") {
        bail!("herdr {} failed: {}", args.join(" "), e["message"].as_str().unwrap_or(&e.to_string()));
    }
    Ok(v)
}

pub fn pane_info(pane_id: &str) -> Result<PaneInfo> {
    let pane = call(&["pane", "get", pane_id])?["result"]["pane"].clone();
    let mut info = PaneInfo {
        agent: pane["agent"].as_str().map(String::from),
        agent_session: pane["agent_session"]["value"].as_str().map(String::from),
        cwd: pane["cwd"].as_str().map(PathBuf::from),
        ..Default::default()
    };
    // Process info is a bonus for finding the live session; a failure here
    // still leaves herdr's own record to fall back on.
    if let Ok(v) = call(&["pane", "process-info", "--pane", pane_id]) {
        let p = &v["result"]["process_info"];
        info.foreground_pgid = p["foreground_process_group_id"].as_u64().map(|n| n as u32);
        info.processes = p["foreground_processes"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|x| Process {
                        pid: x["pid"].as_u64().unwrap_or(0) as u32,
                        argv: x["argv"]
                            .as_array()
                            .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default();
    }
    Ok(info)
}

pub fn split(source: &str, direction: &str, cwd: Option<&Path>) -> Result<String> {
    let mut args = vec!["pane", "split", "--pane", source, "--direction", direction];
    let cwd_s;
    if let Some(c) = cwd.filter(|c| c.is_dir()) {
        cwd_s = c.to_string_lossy().into_owned();
        args.extend(["--cwd", cwd_s.as_str()]);
    }
    let v = call(&args)?;
    v["result"]["pane"]["pane_id"].as_str().map(String::from).context("herdr pane split returned no pane id")
}

pub fn run(pane: &str, argv: &[String]) -> Result<()> {
    call(&["pane", "run", pane, &shell_join(argv)])?;
    Ok(())
}

/// Quote argv for the shell herdr types the command into.
pub fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=@+,%".contains(c)) {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn notify(body: &str) {
    let _ = Command::new(bin())
        .args(["notification", "show", "herdr-fork", "--body", body, "--sound", "none"])
        .output();
}

pub fn reload_config() {
    let _ = Command::new(bin()).args(["server", "reload-config"]).output();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_only_what_needs_it() {
        let argv: Vec<String> = ["claude", "--resume", "ab-12", "it's here"].iter().map(|s| s.to_string()).collect();
        assert_eq!(shell_join(&argv), r"claude --resume ab-12 'it'\''s here'");
    }
}
