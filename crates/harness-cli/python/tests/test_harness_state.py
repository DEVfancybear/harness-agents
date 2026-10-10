"""Persistence regressions for the exact harness module embedded by ha (stdlib only)."""

import importlib.util
from concurrent.futures import ThreadPoolExecutor
import os
from pathlib import Path
import random
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

MODULE_PATH = Path(__file__).resolve().parents[1] / "rlm" / "harness.py"
SPEC = importlib.util.spec_from_file_location("ha_harness_under_test", MODULE_PATH)
harness = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = harness
SPEC.loader.exec_module(harness)


class HarnessPersistenceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ha-memory-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.path = self.root / "harness_state.json"
        harness._state_cache.clear()

    def state(self):
        return harness.HarnessState(self.path)

    def test_concurrent_writers_preserve_every_successful_memory(self):
        start = threading.Barrier(2)

        def write(side):
            state = self.state()
            start.wait(timeout=10)
            for number in range(24):
                state.create_memory(side, str(number), id=f"{side}-{number}")

        with ThreadPoolExecutor(max_workers=2) as pool:
            list(pool.map(write, ("a", "b")))
        self.assertEqual(
            {entry.id for entry in self.state().list("memory")},
            {f"{side}-{number}" for side in ("a", "b") for number in range(24)},
        )

    def test_separate_kernels_preserve_concurrent_global_writes(self):
        code = """
import importlib.util, pathlib, sys, time
spec = importlib.util.spec_from_file_location('harness_worker', sys.argv[1])
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)
state = module.HarnessState(sys.argv[2], scope='global')
start, side = pathlib.Path(sys.argv[3]), sys.argv[4]
start.with_name(side + '.ready').touch()
deadline = time.monotonic() + 10
while not start.exists():
    if time.monotonic() >= deadline: raise TimeoutError('start barrier')
    time.sleep(0.005)
for number in range(24):
    state.create_memory(side, str(number), id=f'{side}-{number}')
"""
        start = self.root / "start"
        workers = [
            subprocess.Popen(
                [sys.executable, "-c", code, str(MODULE_PATH), str(self.path), str(start), side],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            )
            for side in ("a", "b")
        ]
        try:
            deadline = time.monotonic() + 10
            while not all((self.root / (side + ".ready")).exists() for side in ("a", "b")):
                self.assertLess(time.monotonic(), deadline, "both kernels start")
                time.sleep(0.005)
            start.touch()
            for worker in workers:
                _, error = worker.communicate(timeout=20)
                self.assertEqual(worker.returncode, 0, error.decode(errors="replace"))
        finally:
            for worker in workers:
                if worker.poll() is None:
                    worker.kill()
                    worker.communicate()
        self.assertEqual(
            {entry.id for entry in self.state().list("memory")},
            {f"{side}-{number}" for side in ("a", "b") for number in range(24)},
        )

    def test_crud_survives_reopen_and_preserves_unicode_and_version(self):
        state = self.state()
        entry = state.create_memory("Ngôn ngữ", "Trả lời bằng tiếng Việt", id="language")
        self.assertEqual(entry.version, 1)
        reopened = self.state()
        self.assertEqual(reopened.get("memory", "language").content, "Trả lời bằng tiếng Việt")
        reopened.update_memory("language", "Ngôn ngữ", "Giữ nguyên tên hàm")
        self.assertEqual(self.state().get("memory", "language").version, 2)
        self.assertTrue(reopened.delete_memory("language"))
        self.assertEqual(self.state().list("memory"), [])
        self.assertFalse(reopened.delete_memory("language"))

    def test_removed_state_file_clears_cached_memories(self):
        state = self.state()
        state.create_memory("Removed", "This entry was removed", id="removed")
        self.path.unlink()
        self.assertEqual(state.list("memory"), [])

    def test_removed_state_file_clears_refinement_history(self):
        state = self.state()
        state.refinements.append(harness.RefinementEvent(id="review-1", trigger="test", changes=["memory"]))
        state.save()
        self.path.unlink()
        state.load()
        self.assertEqual(state.refinements, [])

    def test_new_write_does_not_resurrect_removed_memories(self):
        state = self.state()
        state.create_memory("Old", "Old memory", id="old")
        self.path.unlink()
        state.create_memory("New", "New memory", id="new")
        self.assertEqual([entry.id for entry in self.state().list("memory")], ["new"])

    def test_in_memory_reload_preserves_entries_and_history(self):
        state = harness.HarnessState(in_memory=True)
        state.create_memory("Volatile", "Keep until the kernel stops", id="kept")
        state.refinements.append(harness.RefinementEvent(id="review-1", trigger="test", changes=[]))
        state.load()
        self.assertEqual(state.get("memory", "kept").content, "Keep until the kernel stops")
        self.assertEqual(len(state.refinements), 1)

    def test_cached_writer_keeps_external_edits(self):
        cached = self.state()
        cached.create_memory("First", "First memory", id="first")
        previous_mtime = self.path.stat().st_mtime_ns
        other = self.state()
        other.create_memory("External", "Written by another process", id="external")
        stat = self.path.stat()
        os.utime(self.path, ns=(stat.st_atime_ns, previous_mtime + 1_000_000_000))
        self.assertIsNotNone(cached.get("memory", "external"))
        cached.create_memory("Third", "Written by the cached state", id="third")
        self.assertEqual({entry.id for entry in self.state().list("memory")}, {"first", "external", "third"})

    def test_local_and_global_state_remain_separate(self):
        local_dir, global_dir = self.root / "local", self.root / "global"
        with patch.dict(os.environ, {"RLM_HARNESS_STATE_DIR": str(local_dir), "RLM_GLOBAL_HARNESS_STATE_DIR": str(global_dir)}):
            state = harness.get_harness_state()
            state.create_memory("Local", "Only this conversation", id="same")
            state.create_memory("Global", "Every conversation", id="same", global_=True)
            self.assertEqual(state.get("memory", "same").content, "Only this conversation")
            self.assertEqual(state.get("memory", "same", global_=True).content, "Every conversation")
            self.assertEqual(harness.HarnessState(local_dir / "harness_state.json").get("memory", "same").scope, "local")
            self.assertEqual(harness.HarnessState(global_dir / "harness_state.json", scope="global").get("memory", "same").scope, "global")

    def test_corrupt_and_non_object_state_are_empty(self):
        for raw in ["{broken", "null", "[]", '"text"', '{"entries": {"memory": {"broken": {"title": 5}}}}']:
            with self.subTest(raw=raw):
                self.path.write_text(raw, encoding="utf-8")
                self.assertEqual(self.state().list(), [])

    def test_randomized_persistence_matches_dictionary_model(self):
        for seed in range(8):
            with self.subTest(seed=seed):
                self.path.unlink(missing_ok=True)
                state, expected = self.state(), {}
                randomizer = random.Random(seed)
                for step in range(60):
                    key = f"memory-{randomizer.randrange(6)}"
                    action = randomizer.choice(["write", "delete", "reset", "reopen"])
                    if action == "write":
                        value = f"Tiếng Việt {seed}/{step}"
                        state.upsert("memory", key, value, id=key)
                        expected[key] = value
                    elif action == "delete":
                        self.assertEqual(state.delete_memory(key), key in expected)
                        expected.pop(key, None)
                    elif action == "reset":
                        self.path.unlink(missing_ok=True)
                        expected.clear()
                    else:
                        state = self.state()
                    actual = {entry.id: entry.content for entry in state.list("memory")}
                    self.assertEqual(actual, expected, f"seed={seed}, step={step}, action={action}")
                    self.assertEqual({entry.id: entry.content for entry in self.state().list("memory")}, expected)


if __name__ == "__main__":
    unittest.main()
