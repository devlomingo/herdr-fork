import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "scripts"))
import fork  # noqa: E402

SID = "11111111-1111-1111-1111-111111111111"


def msg(uid, parent, kind, content, **extra):
    d = {"type": kind, "uuid": uid, "parentUuid": parent, "sessionId": SID, "cwd": "/work",
         "timestamp": "2026-01-01T10:00:00Z", "message": {"role": kind, "content": content}}
    d.update(extra)
    return d


TRANSCRIPT = [
    {"type": "last-prompt", "leafUuid": "a3", "sessionId": SID},
    msg("u1", None, "user", "first question"),
    msg("a1", "u1", "assistant", [{"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}]),
    msg("r1", "a1", "user", [{"type": "tool_result", "tool_use_id": "t1", "content": "ok"}]),
    msg("a1b", "r1", "assistant", [{"type": "text", "text": "first answer"}]),
    msg("m1", "a1b", "user", "hook noise", isMeta=True),
    msg("u2", "m1", "user", [{"type": "text", "text": "second   question"}]),
    msg("a2", "u2", "assistant", [{"type": "text", "text": "second answer"}]),
    msg("u3", "a2", "user", "third question"),
    msg("a3", "u3", "assistant", [{"type": "text", "text": "third answer"}]),
]


class ForkTest(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.path = os.path.join(self.dir.name, SID + ".jsonl")
        with open(self.path, "w") as f:
            for d in TRANSCRIPT:
                f.write(json.dumps(d) + "\n")

    def tearDown(self):
        self.dir.cleanup()

    def forked(self, n, mode):
        new_id, cwd = fork.fork(self.path, n, mode)
        return new_id, cwd, fork.load(os.path.join(self.dir.name, new_id + ".jsonl"))

    def test_lists_only_typed_messages(self):
        lines = fork.load(self.path)
        self.assertEqual([fork.prompt_text(lines[i]) for i in fork.prompts(lines)],
                         ["first question", "second question", "third question"])

    def test_after_keeps_the_whole_turn_including_tools(self):
        new_id, cwd, out = self.forked(0, "after")
        uuids = [d.get("uuid") for d in out if d.get("uuid")]
        self.assertEqual(uuids, ["u1", "a1", "r1", "a1b"])
        self.assertEqual(out[-1], {"type": "last-prompt", "leafUuid": "a1b", "sessionId": new_id})
        self.assertEqual(cwd, "/work")

    def test_before_stops_at_the_previous_answer(self):
        _, _, out = self.forked(2, "before")
        self.assertEqual(out[-2]["uuid"], "a2")
        self.assertNotIn("u3", [d.get("uuid") for d in out])

    def test_last_message_keeps_everything(self):
        _, _, out = self.forked(2, "after")
        self.assertEqual(out[-2]["uuid"], "a3")

    def test_every_line_moves_to_the_new_session_and_old_leaf_is_dropped(self):
        new_id, _, out = self.forked(1, "after")
        self.assertTrue(all(d["sessionId"] == new_id for d in out))
        self.assertEqual(sum(d["type"] == "last-prompt" for d in out), 1)

    def test_original_is_untouched(self):
        before = open(self.path).read()
        self.forked(1, "after")
        self.assertEqual(open(self.path).read(), before)

    def test_cannot_fork_before_the_first_message(self):
        with self.assertRaises(SystemExit):
            fork.fork(self.path, 0, "before")

    def test_out_of_range(self):
        with self.assertRaises(SystemExit):
            fork.fork(self.path, 9, "after")


class KeysTest(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.cfg = os.path.join(self.dir.name, "config.toml")
        os.environ["HERDR_CONFIG_PATH"] = self.cfg
        os.environ["HERDR_BIN_PATH"] = "true"
        fork.HERDR = "true"

    def tearDown(self):
        self.dir.cleanup()

    def test_setup_appends_after_top_level_keys_and_remove_restores(self):
        original = "onboarding = false\n\n[ui]\nx = 1\n"
        with open(self.cfg, "w") as f:
            f.write(original)
        fork.setup_keys()
        text = open(self.cfg).read()
        self.assertTrue(text.startswith(original))
        self.assertTrue(text.rstrip().endswith(fork.END))
        fork.setup_keys()
        self.assertEqual(open(self.cfg).read().count(fork.BEGIN), 1)
        fork.setup_keys(remove=True)
        self.assertEqual(open(self.cfg).read().strip(), original.strip())

    def test_setup_leaves_a_taken_key_alone(self):
        taken = f'[[keys.command]]\nkey = "{fork.DEFAULT_KEY}"\n'
        with open(self.cfg, "w") as f:
            f.write(taken)
        fork.setup_keys()
        self.assertEqual(open(self.cfg).read(), taken)

    def test_strip_block_removes_only_the_managed_block(self):
        text = f"a = 1\n{fork.BEGIN}\nkey = 'x'\n{fork.END}\nb = 2\n"
        self.assertEqual(fork.strip_block(text), "a = 1\nb = 2\n")


if __name__ == "__main__":
    unittest.main()
