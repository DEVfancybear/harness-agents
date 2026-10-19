"""The kernel side of ``rlm.harness``: a shim over the ha host (stdlib only).

The host keeps the harness state (``harness_kernel.rs``, with its own tests);
these check what the shim decides before a request leaves the kernel: the
scope a call targets, Python's argument errors, the factory gate and dry run,
and how a reply becomes entries or an exception.
"""

import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import rlm  # noqa: E402,F401

# `rlm.harness` is the namespace proxy; the module is the one under test.
harness = sys.modules["rlm.harness"]
repl = sys.modules.get("rlm.repl") or __import__("rlm.repl").repl


class FakeHost:
    def __init__(self, result=None, error=None):
        self.requests = []
        self.result = result
        self.error = error

    def __call__(self, data, timeout=60.0):
        self.requests.append(dict(data))
        if self.error is not None:
            return {"status": "error", "error": self.error}
        return {"status": "ok", "result": self.result}


ENTRY = {
    "id": "language",
    "kind": "memory",
    "title": "Language",
    "content": "Answer in Vietnamese",
    "path": "general",
    "scope": "global",
    "reference": {},
    "arguments": {},
    "metadata": {},
    "source": "agent",
    "created_at": "2026-10-19T00:00:00+00:00",
    "updated_at": "2026-10-19T00:00:00+00:00",
    "version": 1,
    "history": ["a field the shim does not know"],
}


class HarnessShimTests(unittest.TestCase):
    def setUp(self):
        harness._state_cache.clear()
        self.state = harness.get_harness_state()

    def host(self, **kwargs):
        fake = FakeHost(**kwargs)
        patcher = patch.object(repl, "host_request_blocking", fake)
        patcher.start()
        self.addCleanup(patcher.stop)
        return fake

    def test_a_scope_prefix_or_flag_routes_to_the_global_store(self):
        fake = self.host(result=ENTRY)
        entry = self.state.update_memory("global:language", "Language", "Answer in Vietnamese")
        self.assertEqual(entry.id, "language")
        self.assertEqual(entry.scope, "global")
        sent = fake.requests[-1]
        self.assertEqual(sent["type"], "harness")
        self.assertEqual(sent["op"], "update")
        self.assertEqual(sent["id"], "language")
        self.assertTrue(sent["global"])
        self.state.create_memory("Language", "x", **{"global": True})
        self.assertTrue(fake.requests[-1]["global"])
        self.state.create_memory("Branch", "feature/x")
        self.assertFalse(fake.requests[-1]["global"])
        self.assertIsNone(fake.requests[-1]["id"])
        harness.get_harness_state(global_=True).list()
        self.assertTrue(fake.requests[-1]["global"])

    def test_a_payload_type_cannot_reroute_the_request(self):
        fake = self.host(result=[])
        self.state.list(kind="memory")
        self.assertEqual(fake.requests[-1]["type"], "harness")
        self.assertEqual(fake.requests[-1]["kind"], "memory")

    def test_python_argument_errors_stay_python_errors(self):
        fake = self.host(result=[])
        with self.assertRaises(TypeError):
            self.state.create_memory("t", "c", bogus=1)
        with self.assertRaises(TypeError):
            self.state.list(**{"global": "yes"})
        with self.assertRaises(TypeError):
            self.state.search(5)
        with self.assertRaises(TypeError):
            self.state.search("x", limit=True)
        self.assertEqual(fake.requests, [])

    def test_a_host_rejection_is_a_value_error(self):
        self.host(error="memory entry 't' rejected: content must be a non-empty string, got a list")
        with self.assertRaisesRegex(ValueError, "content must be a non-empty string"):
            self.state.create_memory("t", ["a"])

    def test_replies_become_entries_and_events(self):
        self.host(result={"file_path": "x/harness_state.json", "scope": "local",
                          "entries": {"memory": {"language": ENTRY}}, "refinements": [
                              {"id": "refine_0001", "trigger": "t", "changes": ["c"], "evidence": "",
                               "outcome": "", "created_at": ""}]})
        self.assertEqual(self.state.entries["memory"]["language"].content, "Answer in Vietnamese")
        self.assertEqual(self.state.entries["factory"], {})
        self.assertEqual(self.state.refinements[0].id, "refine_0001")
        self.assertEqual(self.state.file_path, Path("x/harness_state.json"))

    def test_a_skill_needs_a_python_reference_before_any_request(self):
        fake = self.host(result=ENTRY)
        with self.assertRaisesRegex(ValueError, "skill reference.type must be 'python'"):
            self.state.create_skill("s", "c", reference={"type": "shell"})
        self.assertEqual(fake.requests, [])

    def test_the_factory_is_off_until_ha_settings_turn_it_on(self):
        fake = self.host(result={**ENTRY, "kind": "factory"})
        with tempfile.TemporaryDirectory() as directory:
            settings = Path(directory) / "settings.json"
            with patch.dict(os.environ, {"HA_SETTINGS_FILE": str(settings)}):
                with self.assertRaisesRegex(ValueError, "the factory is disabled"):
                    self.state.create_factory("f", "c", dag={})
                with self.assertRaisesRegex(ValueError, "the factory is disabled"):
                    self.state.create("factory", "f", "c", arguments={"dag": {}})
                self.assertEqual(fake.requests, [])
                settings.write_text(json.dumps({"factory": {"enabled": True}}), encoding="utf-8")
                with self.assertRaises(ValueError):
                    self.state.create_factory("f", "c", dag={"nodes": "not a list"})
                self.assertEqual(fake.requests, [], "an invalid spec never reaches the host")
                spec = {"nodes": [{"id": "a", "prompt": "do a"}]}
                if not harness.validate_factory_spec(spec):
                    self.state.create_factory("f", "c", dag=spec)
                    sent = fake.requests[-1]
                    self.assertEqual(sent["kind"], "factory")
                    self.assertEqual(sent["arguments"], {"dag": spec})
                    self.assertIsNot(sent["arguments"]["dag"], spec, "the spec is copied")

    def test_a_kernel_without_a_host_says_so(self):
        with patch.object(repl, "_loop", None):
            with self.assertRaisesRegex(RuntimeError, "not serving"):
                self.state.create_memory("t", "c")

    def test_an_unpersisted_local_store_refuses_local_writes_only(self):
        fake = self.host(result=ENTRY)
        state = harness.HarnessState(local_write_error="no local store")
        with self.assertRaisesRegex(RuntimeError, "no local store"):
            state.create_memory("t", "c")
        state.create_memory("t", "c", global_=True)
        self.assertTrue(fake.requests[-1]["global"])


if __name__ == "__main__":
    unittest.main()
