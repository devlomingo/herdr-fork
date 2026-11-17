# claude-fork

A [Herdr](https://herdr.dev) plugin that forks a [Claude Code](https://claude.com/claude-code) conversation **from any message you pick** into a new pane.

Claude Code can fork a whole session (`claude --resume <id> --fork-session`), but it can't branch from an earlier point in the conversation. claude-fork can. Press one key, pick a message, and a second Claude opens beside the first. It knows everything up to that message and nothing after it. The original conversation is not touched.

```
Fork from which message?
enter: keep its answer   b: fork before it   j/k or arrows: move   q: cancel

  5  14:52  ok, now write the migration
  4  14:40  what about the index on orders.created_at?
▌ 3  14:31  explain how the cache is invalidated          ◀ fork here
  2  14:20  read src/cache and summarise it
  1  14:18  hi, we're working on the checkout service
```

Use it to:

- **Try another approach.** Take the conversation back to before a wrong turn and try something else, while the first attempt keeps running.
- **Ask a side question** without filling the main conversation with it.
- **Reuse a long setup.** Load the context once, then fork it into several panes for separate tasks.

## Requirements

- Herdr **0.9.0** or newer, on macOS or Linux
- Claude Code, with Herdr's Claude integration installed (`herdr integration install claude`) so Herdr knows which pane runs Claude
- Python **3.9+** (standard library only, no extra packages)

## Install

```bash
herdr plugin install devlomingo/herdr-claude-fork
```

Then bind a key. Run the plugin's **"install the prefix+shift+f keybinding"** action from Herdr's command palette, or add it to your config yourself:

```bash
herdr plugin action invoke --plugin devlomingo.claude-fork setup-keys
```

```toml
# ~/.config/herdr/config.toml
[[keys.command]]
key = "prefix+shift+f"
type = "plugin_action"
command = "devlomingo.claude-fork.fork"
description = "claude-fork: fork conversation from a message"
```

The setup action backs up your config to `config.toml.bak-claude-fork`, writes one marked block, and reloads Herdr. If `prefix+shift+f` is already taken, it changes nothing and tells you so.

## Use

1. Focus a pane that runs Claude Code.
2. Press **prefix+shift+f**. Your messages appear, newest first.
3. Choose where to fork:

| Key | What the new pane gets |
| --- | --- |
| `Enter` | Everything up to and including Claude's answer to that message. Carry on from there. |
| `b` | Everything **before** that message. The message is copied to your clipboard, so you can edit it and send it again. |
| `q` / `Esc` | Cancel. |

The fork opens to the right of the source pane, in the same directory, running `claude --resume <new-session>`. It is an ordinary Claude session: it appears in `claude --resume`, and you can fork it again.

## Settings

Optional. Create `config.json` in the plugin's config directory (`herdr plugin config-dir devlomingo.claude-fork`):

```json
{
  "direction": "down",
  "command": "claude --model opus"
}
```

| Key | Default | Meaning |
| --- | --- | --- |
| `direction` | `"right"` | Where the fork opens: `"right"` or `"down"`. |
| `command` | `"claude"` | What gets run. `--resume <id>` is appended. |

## How it works

1. **Find the conversation.** The action reads the focused pane from Herdr. The picker then gets the pane's Claude process (`herdr pane process-info`) and reads that process's session from `~/.claude/sessions/<pid>.json`. If that fails, it uses the session Herdr recorded for the pane. Herdr's record alone isn't enough: running `claude -p` inside a pane makes Herdr point the pane at that short-lived session.
2. **Cut the transcript.** Claude Code stores each session as JSON lines in `~/.claude/projects/<project>/<session>.jsonl`. The picker lists only messages you typed, not tool results, hook output, subagent turns or compaction summaries. Then it copies the lines up to the cut into a new `<uuid>.jsonl` next to the original, moves every line to the new session id, and points `last-prompt` at the cut so `--resume` continues from there.
3. **Open it.** `herdr pane split` next to the source pane, then `herdr pane run` with `claude --resume <new-id>`.

The original transcript is only read, never written. `CLAUDE_CONFIG_DIR` is honoured if you set it.

You can also use it from scripts, without Herdr:

```bash
python3 scripts/fork.py --list ~/.claude/projects/<project>/<session>.jsonl   # number, time, message
python3 scripts/fork.py --fork <transcript.jsonl> 2 after                     # prints the new session id
claude --resume <new-id>
```

## Troubleshooting

| You see | Cause and fix |
| --- | --- |
| *the focused pane is not running Claude Code* | Herdr doesn't recognise Claude in that pane. Run `herdr integration install claude`, then restart Claude in the pane. |
| *could not find this pane's Claude transcript* | The session has no messages saved yet. Send one message and try again. |
| *nothing comes before the first message* | You pressed `b` on the first message. Use `Enter`, or start a new Claude session. |
| Nothing happens on the key | Run `herdr plugin log devlomingo.claude-fork` to see what failed, and check the binding with `herdr config check`. |

## Limitations

- It depends on Claude Code's transcript format, which Anthropic doesn't document and may change. If a Claude Code update breaks forking, please open an issue.
- Each fork is a full copy of the transcript up to the cut, which can be a few MB for a long session.
- Windows isn't supported, because the picker uses `curses`.

## Uninstall

```bash
herdr plugin action invoke --plugin devlomingo.claude-fork remove-keys
herdr plugin uninstall devlomingo.claude-fork
```

## Development

```bash
git clone https://github.com/devlomingo/herdr-claude-fork && cd herdr-claude-fork
herdr plugin link "$PWD"          # live from your checkout; no build step
python3 -m unittest discover -s tests
```

## License

[MIT](LICENSE)
