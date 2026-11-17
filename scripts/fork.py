#!/usr/bin/env python3
"""claude-fork: pick a message in a Claude Code conversation and fork it into a new herdr pane.

Entry points (see herdr-plugin.toml):
  fork.py --open            action: open the picker over the focused pane
  fork.py                   picker pane (needs FORK_SOURCE_PANE)
  fork.py --setup-keys      action: bind prefix+shift+f in herdr's config
  fork.py --remove-keys     action: remove that binding

Scripting / debugging, no herdr needed:
  fork.py --list <transcript.jsonl>
  fork.py --fork <transcript.jsonl> <n> after|before    prints the new session id
"""
import json
import os
import shutil
import subprocess
import sys
import uuid
from datetime import datetime

HERDR = os.environ.get("HERDR_BIN_PATH", "herdr")
PLUGIN_ID = os.environ.get("HERDR_PLUGIN_ID", "devlomingo.claude-fork")
CLAUDE_DIR = os.environ.get("CLAUDE_CONFIG_DIR") or os.path.expanduser("~/.claude")
DEFAULT_KEY = "prefix+shift+f"


# ---- transcript --------------------------------------------------------------

def load(path):
    with open(path, encoding="utf-8") as f:
        return [json.loads(line) for line in f if line.strip()]


def is_prompt(d):
    """A message the human typed: not a tool result, hook output, sidechain or compact summary."""
    if d.get("type") != "user" or d.get("isMeta") or d.get("isSidechain") or d.get("isCompactSummary"):
        return False
    c = d.get("message", {}).get("content")
    if isinstance(c, str):
        return bool(c.strip())
    return isinstance(c, list) and bool(c) and not any(b.get("type") == "tool_result" for b in c)


def prompt_text(d):
    c = d["message"]["content"]
    if isinstance(c, list):
        c = " ".join(b.get("text", "[image]") for b in c)
    return " ".join(c.split())


def prompts(lines):
    return [i for i, d in enumerate(lines) if is_prompt(d)]


def last_message_before(lines, end):
    # Meta lines just before a prompt (hook output) belong to the next turn, not this one.
    for k in range(end - 1, -1, -1):
        d = lines[k]
        if d.get("type") in ("user", "assistant") and d.get("uuid") and not d.get("isMeta") and not d.get("isSidechain"):
            return k
    return None


def fork(path, n, mode):
    """Copy the transcript up to the end of prompt n's turn ('after') or to just before
    prompt n ('before') into a new session next to it. Returns (session_id, cwd)."""
    lines = load(path)
    ps = prompts(lines)
    if not 0 <= n < len(ps):
        raise SystemExit(f"message {n} does not exist; this conversation has {len(ps)}")
    start = ps[n]
    end = start if mode == "before" else (ps[n + 1] if n + 1 < len(ps) else len(lines))
    k = last_message_before(lines, end)
    if k is None:
        raise SystemExit("nothing comes before the first message; fork after it instead")
    new_id = str(uuid.uuid4())
    out = []
    for d in lines[: k + 1]:
        if d.get("type") == "last-prompt":
            continue
        if "sessionId" in d:
            d["sessionId"] = new_id
        out.append(d)
    # `claude --resume` continues from the leaf named here, so point it at the cut.
    out.append({"type": "last-prompt", "leafUuid": lines[k]["uuid"], "sessionId": new_id})
    dest = os.path.join(os.path.dirname(path), new_id + ".jsonl")
    with open(dest, "w", encoding="utf-8") as f:
        for d in out:
            f.write(json.dumps(d, ensure_ascii=False) + "\n")
    return new_id, lines[k].get("cwd") or lines[start].get("cwd")


# ---- herdr -------------------------------------------------------------------

def herdr(*args):
    r = subprocess.run([HERDR, *args], capture_output=True, text=True)
    if r.returncode:
        raise SystemExit(f"herdr {' '.join(args)} failed: {r.stderr.strip() or r.stdout.strip()}")
    return json.loads(r.stdout) if r.stdout.strip() else {}


def live_session_id(pane_id):
    # herdr's agent_session can go stale: a `claude -p` run inside the pane re-reports the
    # pane with the child's session. Claude's own pid file for the pane's process does not.
    try:
        info = herdr("pane", "process-info", "--pane", pane_id)["result"]["process_info"]
        with open(os.path.join(CLAUDE_DIR, "sessions", f"{info['foreground_process_group_id']}.json")) as f:
            return json.load(f).get("sessionId")
    except (SystemExit, KeyError, OSError, ValueError):
        return None


def find_transcript(session_id):
    root = os.path.join(CLAUDE_DIR, "projects")
    for project in os.listdir(root) if os.path.isdir(root) else []:
        path = os.path.join(root, project, session_id + ".jsonl")
        if os.path.isfile(path):
            return path
    return None


def transcript_for_pane(pane_id):
    pane = herdr("pane", "get", pane_id)["result"]["pane"]
    if pane.get("agent") != "claude":
        raise SystemExit("the focused pane is not running Claude Code")
    for sid in filter(None, [live_session_id(pane_id), (pane.get("agent_session") or {}).get("value")]):
        path = find_transcript(sid)
        if path:
            return path
    raise SystemExit("could not find this pane's Claude transcript (send one message first?)")


def settings():
    s = {"direction": "right", "command": "claude"}
    cfg = os.path.join(os.environ.get("HERDR_PLUGIN_CONFIG_DIR", ""), "config.json")
    if os.path.isfile(cfg):
        with open(cfg) as f:
            s.update(json.load(f))
    return s


def copy_to_clipboard(text):
    for cmd in (["pbcopy"], ["wl-copy"], ["xclip", "-selection", "clipboard"], ["xsel", "--clipboard", "--input"]):
        if shutil.which(cmd[0]):
            subprocess.run(cmd, input=text, text=True, check=False)
            return True
    return False


# ---- picker ------------------------------------------------------------------

def stamp(d):
    try:
        return datetime.fromisoformat(d["timestamp"].replace("Z", "+00:00")).astimezone().strftime("%H:%M")
    except (KeyError, ValueError):
        return "     "


def pick(stdscr, items):
    import curses
    curses.curs_set(0)
    curses.use_default_colors()
    sel = 0
    while True:
        stdscr.erase()
        h, w = stdscr.getmaxyx()
        stdscr.addnstr(0, 0, "Fork from which message?", w - 1, curses.A_BOLD)
        stdscr.addnstr(1, 0, "enter: keep its answer   b: fork before it   j/k or arrows: move   q: cancel", w - 1, curses.A_DIM)
        rows = max(1, h - 3)
        top = max(0, sel - rows + 1)
        for row, (n, ts, text) in enumerate(items[top: top + rows]):
            attr = curses.A_REVERSE if top + row == sel else 0
            stdscr.addnstr(row + 3, 0, f"{n + 1:>3}  {ts}  {text}", w - 1, attr)
        key = stdscr.getch()
        if key in (curses.KEY_UP, ord("k")):
            sel = max(0, sel - 1)
        elif key in (curses.KEY_DOWN, ord("j")):
            sel = min(len(items) - 1, sel + 1)
        elif key in (curses.KEY_HOME, ord("g")):
            sel = 0
        elif key in (curses.KEY_END, ord("G")):
            sel = len(items) - 1
        elif key in (10, 13, curses.KEY_ENTER):
            return items[sel][0], "after"
        elif key == ord("b"):
            return items[sel][0], "before"
        elif key in (ord("q"), 27):
            return None, None


def run_picker():
    import curses
    source = os.environ.get("FORK_SOURCE_PANE")
    if not source:
        raise SystemExit("open the picker through the claude-fork action, not directly")
    path = transcript_for_pane(source)
    lines = load(path)
    ps = prompts(lines)
    if not ps:
        raise SystemExit("this conversation has no messages yet")
    items = [(n, stamp(lines[i]), prompt_text(lines[i])) for n, i in enumerate(ps)][::-1]
    n, mode = curses.wrapper(pick, items)
    if n is None:
        return
    new_id, cwd = fork(path, n, mode)
    if mode == "before":
        copy_to_clipboard(prompt_text(lines[ps[n]]))
    s = settings()
    split = ["pane", "split", "--pane", source, "--direction", s["direction"]]
    if cwd and os.path.isdir(cwd):
        split += ["--cwd", cwd]
    new_pane = herdr(*split)["result"]["pane"]["pane_id"]
    herdr("pane", "run", new_pane, f"{s['command']} --resume {new_id}")


def open_picker():
    ctx = json.loads(os.environ.get("HERDR_PLUGIN_CONTEXT_JSON") or "{}")
    source = ctx.get("focused_pane_id")
    if not source:
        raise SystemExit("no focused pane")
    herdr("plugin", "pane", "open", "--plugin", PLUGIN_ID, "--entrypoint", "picker",
          "--placement", "overlay", "--focus", "--env", f"FORK_SOURCE_PANE={source}")


# ---- keybinding --------------------------------------------------------------

BEGIN = f"# >>> {PLUGIN_ID} keys (managed by the claude-fork setup-keys / remove-keys actions)"
END = f"# <<< {PLUGIN_ID} keys"


def config_path():
    if "HERDR_CONFIG_PATH" in os.environ:
        return os.environ["HERDR_CONFIG_PATH"]
    base = os.environ.get("XDG_CONFIG_HOME") or os.path.expanduser("~/.config")
    return os.path.join(base, "herdr", "config.toml")


def strip_block(text):
    out, skip = [], False
    for line in text.splitlines(keepends=True):
        if line.rstrip("\n") == BEGIN:
            skip = True
        elif line.rstrip("\n") == END:
            skip = False
        elif not skip:
            out.append(line)
    return "".join(out)


def notify(title, body):
    subprocess.run([HERDR, "notification", "show", title, "--body", body, "--sound", "none"],
                   capture_output=True, check=False)
    print(body)


def setup_keys(remove=False):
    path = config_path()
    text = open(path, encoding="utf-8").read() if os.path.isfile(path) else ""
    rest = strip_block(text)
    if not remove:
        if f'"{DEFAULT_KEY}"' in rest:
            return notify("claude-fork", f"{DEFAULT_KEY} is already bound in {path}; bind {PLUGIN_ID}.fork yourself")
        block = (f"{BEGIN}\n[[keys.command]]\nkey = \"{DEFAULT_KEY}\"\ntype = \"plugin_action\"\n"
                 f"command = \"{PLUGIN_ID}.fork\"\ndescription = \"claude-fork: fork conversation from a message\"\n{END}\n")
        rest = block + "\n" + rest
    if os.path.isfile(path):
        shutil.copy2(path, path + ".bak-claude-fork")
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        f.write(rest)
    subprocess.run([HERDR, "server", "reload-config"], capture_output=True, check=False)
    notify("claude-fork", f"{DEFAULT_KEY} {'removed' if remove else 'now forks the focused Claude conversation'}")


# ---- main --------------------------------------------------------------------

def main(argv):
    if argv[:1] == ["--list"]:
        lines = load(argv[1])
        for n, i in enumerate(prompts(lines)):
            print(f"{n}\t{stamp(lines[i])}\t{prompt_text(lines[i])[:100]}")
        return
    if argv[:1] == ["--fork"]:
        print(fork(argv[1], int(argv[2]), argv[3])[0])
        return
    if argv[:1] == ["--open"]:
        return open_picker()
    if argv[:1] in (["--setup-keys"], ["--remove-keys"]):
        return setup_keys(remove=argv[0] == "--remove-keys")
    try:
        run_picker()
    except SystemExit as e:
        if e.code not in (None, 0):
            print(f"\n  claude-fork: {e.code}\n")
            input("  press enter to close ")


if __name__ == "__main__":
    main(sys.argv[1:])
