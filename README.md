# herdr-fork

Fork any coding agent's conversation from any message, into a new
[herdr](https://herdr.dev) pane.

Press one key in a pane running Claude Code, Codex or Copilot CLI, pick one of
your earlier messages, and a fork of the conversation from that point opens in
a split beside it, running the same agent. The fork knows everything up to that
message and nothing after it. The original keeps running, untouched.

```
herdr-fork · Claude Code · fork from which message?
enter: fork after (keep its answer)   b: fork before it (message copied)   j/k: move   q: cancel

  5  14:52  ok, now write the migration
  4  14:40  what about the index on orders.created_at?
  3  14:31  explain how the cache is invalidated          ◀ highlighted
  2  14:20  read src/cache and summarise it
  1  14:18  hi, we're working on the checkout service
```

Use it to try another approach from before a wrong turn while the first
attempt keeps going, to ask a side question without polluting the main thread,
or to load a long setup once and fork it into several panes.

## How this relates to each agent's built-in fork/rewind

Every agent here can already rewind or fork in some form. herdr-fork follows
their lead and writes the same shape they do; what it adds is the same flow for
every agent, one key from herdr, a picker over the whole history, and the fork
in a new pane next to an original that keeps running.

| Agent | What it does natively | What herdr-fork adds |
|---|---|---|
| Claude Code 2.1.326 | `/rewind` (Esc Esc) goes back to before a message in the same session; `/branch [name]` copies the current conversation into a new session; `/fork [prompt]` copies it into a background session; `claude --resume <id> --fork-session` copies a whole session. `--resume-session-at` exists but only in print mode. | Branch from **any** earlier message, not just the current point, without rewinding the original. The fork file is shaped like Claude's own `/branch` output (see below). |
| Codex 0.160 | `codex fork <id>` copies a whole thread; the app-server's `thread/fork` accepts `lastTurnId` and records `forkedFromId`. | Uses that native `thread/fork` with a cut point, so no Codex file is edited. Falls back to `codex fork <id>` if the app-server is unavailable. |
| Copilot CLI 0.0.414 | `--resume <id>`; Esc Esc rewinds *files* to git snapshots. No conversation fork. | The conversation fork itself, as a copy of the session directory. |
| Droid 0.89 | `/fork` copies the session; `/rewind` forks into a new session at a chosen message. Both TUI-only. | Not yet: see the support table. |

## Support

Verified live on macOS with the versions listed. "Message-level" means
forking after or before any message; "whole-session" means only a full copy.

| Agent | Message-level | Whole-session | Storage | Resume command |
|---|---|---|---|---|
| Claude Code (`claude`) | yes | yes (fork after the last message) | `~/.claude/projects/<project>/<id>.jsonl` (respects `CLAUDE_CONFIG_DIR`) | `claude --resume <id>` |
| Codex (`codex`) | yes, through Codex's own `thread/fork` | `codex fork <id>` fallback | `~/.codex/sessions/YYYY/MM/DD/rollout-*-<id>.jsonl` plus `state_5.sqlite` and `thread_history_1.sqlite` (respects `CODEX_HOME`) | `codex resume <id>` |
| Copilot CLI (`copilot`) | yes | yes | `~/.copilot/session-state/<id>/` (`events.jsonl`, `workspace.yaml`, `checkpoints/`, `files/`) | `copilot --resume <id>` |
| Droid (`droid`) | not yet | not from herdr | `~/.factory/sessions/<cwd-slug>/<id>.jsonl` + `<id>.settings.json` | `droid --resume <id>` |
| Cursor Agent (`cursor`) | not yet | not from herdr | not inspected | `cursor-agent --resume <chatId>` |
| Pi, OpenCode, Kimi, Devin, Kilo, OMP, others | not yet | if you configure one (see Settings) | — | — |

Droid and Cursor Agent were installed on the test machine but not logged in,
so their formats could not be verified by running them; Droid's own
`forkSession` code shows the shape (first line `session_start` with a `parent`
field, copied up to a message id), which makes it the obvious next adapter. The
other agents were not installed. For an agent without an adapter the picker
says `herdr-fork: <agent> is not supported yet` and waits for Enter; it never
guesses a file format.

## Requirements

- herdr **0.9.0** or newer, macOS or Linux
- Rust and cargo (the plugin builds itself with `cargo build --release --locked`
  on install)
- herdr's integration for each agent you use, so herdr knows what runs in a
  pane: `herdr integration install claude` (and `codex`, `copilot`, ...)

## Install

```bash
herdr plugin install devlomingo/herdr-fork
herdr plugin action invoke devlomingo.herdr-fork.setup-keys
```

The second line binds `prefix+shift+f`. To bind it yourself instead:

```toml
# ~/.config/herdr/config.toml — put this at the END of the file
[[keys.command]]
key = "prefix+shift+f"
type = "plugin_action"
command = "devlomingo.herdr-fork.fork"
description = "herdr-fork: fork the conversation from a message"
```

## Usage

Focus an agent pane and press `prefix+shift+f` (or run the "herdr-fork: fork
conversation from a message" action). The picker opens over the pane, newest
message first.

| Key | Does |
|---|---|
| `↑`/`↓` or `k`/`j` | move |
| `g` / `G` | first / last |
| `Enter` | fork **after** the message: the fork keeps it and its whole answer |
| `b` | fork **before** the message, like the agents' own rewind; the message is copied to the clipboard (pbcopy, wl-copy, xclip or xsel) so you can edit and resend it |
| `q` / `Esc` | cancel |

**Fork from selected text.** Select text in the agent pane (a prompt, or part of
an answer) and press the key: the picker opens with that message already
highlighted and says so, so Enter or `b` forks right there. herdr passes the
selection to the action as `selected_text`; a prompt that contains the
selection wins, otherwise the turn whose answer contains it.
herdr clears a mouse selection when you press the prefix key, so when no
selection arrives the picker looks for the text you last copied (herdr copies
on select) and starts on its message if it finds one.

## Settings

`$(herdr plugin config-dir devlomingo.herdr-fork)/config.json`:

```json
{
  "direction": "right",
  "commands": { "claude": "claude", "codex": "codex", "copilot": "copilot" },
  "whole_session_forks": { "opencode": ["opencode", "--session", "{id}", "--fork"] }
}
```

- `direction`: where the fork opens, `right` or `down`.
- `commands`: the binary used in the resume command per agent, e.g. a wrapper
  script.
- `whole_session_forks`: for an agent herdr-fork has no adapter for, a native
  command that copies a whole session (`{id}` is herdr's session id). The
  picker then offers only "fork whole conversation". Nothing is configured by
  default; the example above is illustrative, check your agent's own docs.

## How it works

1. The action reads the focused pane from `HERDR_PLUGIN_CONTEXT_JSON` and opens
   the picker as an overlay plugin pane.
2. `herdr pane get` gives the agent label (`claude`, `codex`, ...), which picks
   the adapter. The session is found from the agent's own live record for the
   pane's process first (`herdr pane process-info`), then herdr's
   `agent_session`, else an error. herdr's record can go stale: a `claude -p`
   run inside the pane re-reports the pane with the child's session.
3. The adapter lists your typed messages and, on a choice, writes the fork
   under a new id. **The original's files and database are never modified.**
4. `herdr pane split` opens a pane in the session's directory and
   `herdr pane run` starts the agent's resume command there.

Per agent:

- **Claude Code.** Live id from `~/.claude/sessions/<pid>.json`. The
  conversation is the `parentUuid` chain from the newest line (so rewound-away
  branches are skipped, as `/rewind` would). The cut is the line the next
  prompt hangs off, exactly where `/rewind` resumes. The new
  `<uuid>.jsonl` is what `/branch` writes: only the chain, every `sessionId` set
  to the new id, `forkedFrom: {sessionId, messageUuid}` on every message line,
  the session owner, mode lines, matching file-history snapshots, a
  `custom-title`/`agent-name` of "<first message> (Fork)" (Claude uses
  "(Branch)") plus `<id>/custom-title.json`, and a closing `last-prompt` whose
  `leafUuid` is the cut. It shows up in `claude --resume` like a native branch.
- **Codex.** No per-process record, so herdr's session (reported by Codex's own
  hook, which ignores `codex exec` children) is used, then a `codex resume <id>`
  argv. herdr-fork starts `codex app-server`, lists turns with
  `thread/turns/list` and calls `thread/fork` with `lastTurnId`: after = through
  the turn before your next message, before = through the turn before this one.
- **Copilot CLI.** Live id from `~/.copilot/logs/process-*-<pid>.log`
  ("Workspace initialized: <id>"). The session directory is copied, minus lock
  files; `events.jsonl` stops just before the next `user.message` (after) or
  this one (before); `session.start` and `workspace.yaml` get the new id.

## Adding an adapter

1. Find where the agent stores sessions: start a scratch session, send three
   messages, and look under its home directory. Check its CLI help for
   `resume`, `fork`, `session` flags.
2. Look for a native fork or rewind first. If it accepts a cut point, call it
   (like Codex) instead of editing files.
3. Add `src/adapters/<agent>.rs` implementing `Adapter` from `src/adapter.rs`
   (`id`, `find_session`, `session_from_arg`, `prompts`, `fork`,
   `resume_command`, optionally `whole_session_command`), register it in
   `select`, and add a display name in `main.rs`.
4. Unit tests with a synthetic fixture (never a real session): only typed
   prompts listed; after keeps the whole turn; before stops at the previous
   answer; the last message keeps everything; the new id is everywhere; the
   original is unchanged byte for byte; before-first and out-of-range error.
5. Prove it live in herdr (fork after/before message 2 of a ONE/TWO/THREE
   session and ask the fork what your last message was), then add it to the
   tables above.

## Building from source

```bash
git clone https://github.com/devlomingo/herdr-fork && cd herdr-fork
cargo test --locked
cargo build --release --locked
herdr plugin link "$PWD"        # link does not build; the line above does
```

Debug subcommands that need no herdr (messages are numbered from 1, as in the
picker):

```bash
./target/release/herdr-fork list claude <session-path-or-id>
./target/release/herdr-fork fork claude <session-path-or-id> 2 after   # prints the new id
```

## Troubleshooting

- **"could not find this pane's ... session"**: send one message first, and
  make sure herdr's integration for that agent is installed
  (`herdr integration status`).
- **"<agent> is not supported yet"**: no adapter; see the support table.
- **The key does nothing**: run `herdr config check`; the binding must be a
  `[[keys.command]]` block at the end of the file. setup-keys leaves the key
  alone if something else already uses it and says so.
- **Codex: "codex app-server did not answer"**: herdr-fork offers Codex's
  whole-session `codex fork <id>` instead.
- Logs: `herdr plugin log list --plugin devlomingo.herdr-fork`.
- `herdr plugin action invoke devlomingo.herdr-fork.setup-keys` is the action
  syntax; `herdr plugin update` needs `--yes` when not interactive; installing
  over a linked copy is refused (unlink first).

## Limitations

- Agents' storage formats are undocumented and change between versions; an
  adapter can break on an agent update. The original session is never touched,
  so the worst case is a failed fork, not a damaged conversation.
- Forking rewinds the conversation, not your files. Code changes made after the
  chosen message stay on disk (Claude's `/rewind` can restore code; use it or
  git for that).
- Claude: large tool results stored in the original session's directory are
  still read from there.
- Copilot: checkpoints made after the cut are copied as they are.

## Uninstall

```bash
herdr plugin action invoke devlomingo.herdr-fork.remove-keys
herdr plugin uninstall devlomingo.herdr-fork
```

Forks are ordinary sessions of each agent; delete them the way you delete any
other session.

## License

MIT
