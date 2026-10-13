"""Desktop skill regressions: all GUI input is fake; CI never moves the mouse."""

import asyncio
import base64
from contextlib import nullcontext
import importlib
import io
from pathlib import Path
import sys
import tempfile
import threading
import unittest
from unittest.mock import AsyncMock, patch
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(ROOT / ".agents/skills/computer-use/src"))
sys.path.insert(0, str(ROOT / ".agents/skills/attach-image/src"))
import computer_use

# The public package may re-export its implementation from a sibling module.
implementation = importlib.import_module(computer_use.run.__module__)
REAL_MUTEX = implementation._desktop_mutex


class FakeDesktop:
    def __init__(self):
        self.FAILSAFE = True
        self.PAUSE = 0.1
        self.events = []
        self.pointer = (100, 100)
        self.image_size = (800, 600)
        self.down_hook = None
        self.corner = False
        mapping = {char: ord(char.upper()) for char in "abcdefghijklmnopqrstuvwxyz"}
        mapping.update({char.upper(): value | 0x100 for char, value in list(mapping.items())})
        mapping.update({char: ord(char) for char in "0123456789"})
        mapping.update({"`": 0xC0, "-": 0xBD, "=": 0xBB, "[": 0xDB, "]": 0xDD,
                        "\\": 0xDC, ";": 0xBA, "'": 0xDE, ",": 0xBC, ".": 0xBE, "/": 0xBF,
                        " ": 0x20})
        shifted = dict(zip('~!@#$%^&*()_+{}|:"<>?', '`1234567890-=[]\\;\',./'))
        mapping.update({char: mapping[base] | 0x100 for char, base in shifted.items()})
        mapping.update({"space": 0x20, "enter": 0x0D, "tab": 0x09, "shift": 0x10,
                        "ctrl": 0x11, "alt": 0x12, "left": 0x25, "right": 0x27,
                        "backspace": 0x08})
        self.platformModule = SimpleNamespace(
            keyboardMapping=mapping,
            _keyUp=lambda key: self.events.append(("cleanup_key", key)),
            _mouseUp=lambda x, y, button: self.events.append(("cleanup_mouse", button)),
            _position=lambda: self.pointer,
            _sendMouseEvent=lambda event, x, y, dwData=0: self.events.append(("hwheel", dwData)),
        )

    def size(self):
        return (800, 600)

    def position(self):
        return self.pointer

    def screenshot(self):
        from PIL import Image
        return Image.new("RGB", self.image_size, "#aabbcc")

    def failSafeCheck(self):
        if self.corner:
            raise RuntimeError("fake fail-safe")

    def isShiftCharacter(self, key):
        return key.isupper() or key in '~!@#$%^&*()_+{}|:"<>?'

    def needsShift(self, key):
        return self.isShiftCharacter(key)

    def moveTo(self, x, y, duration=0, _pause=True):
        self.failSafeCheck()
        self.pointer = (x, y)
        self.events.append(("move", x, y))

    def mouseDown(self, x=None, y=None, button="left", _pause=True):
        self.failSafeCheck()
        self.events.append(("down", button))
        if self.down_hook:
            self.down_hook()

    def mouseUp(self, x=None, y=None, button="left", _pause=True):
        self.failSafeCheck()
        self.events.append(("up", button))

    def keyDown(self, key, _pause=True):
        self.failSafeCheck()
        self.events.append(("key_down", key))
        if self.down_hook:
            self.down_hook()

    def keyUp(self, key, _pause=True):
        self.failSafeCheck()
        self.events.append(("key_up", key))

    def scroll(self, clicks, x=None, y=None, _pause=True):
        self.failSafeCheck()
        self.events.append(("scroll", clicks))


class DesktopTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ha-desktop-unit-")
        self.addCleanup(self.temp.cleanup)
        self.desktop = FakeDesktop()
        self.attachments = []
        self.vision = {"id": "fake-vision", "input": ["image"]}

        async def host_request(kind, *args, **kwargs):
            self.assertEqual(kind, "model.info")
            return self.vision

        self.runtime = SimpleNamespace(host_request=host_request, emit=self.attachments.append)
        for patcher in (
            patch.dict(sys.modules, {"rlm": self.runtime}),
            patch.object(implementation, "_load_pyautogui", return_value=self.desktop),
            patch.object(implementation, "_desktop_mutex", side_effect=nullcontext),
            patch("pathlib.Path.cwd", return_value=Path(self.temp.name)),
            patch.object(implementation, "_send_unicode", side_effect=lambda char: self.desktop.events.append(("unicode", char))),
            patch.object(implementation, "_LAST_VIEW", None),
        ):
            patcher.start()
            self.addCleanup(patcher.stop)

    async def test_screenshot_emits_decodable_image_and_coordinate_mapping(self):
        from PIL import Image
        result = await computer_use.screenshot()
        self.assertEqual(result["raw_size"], [800, 600])
        payload = self.attachments[-1]["application/vnd.prime-agent.attachment+json"]
        with Image.open(io.BytesIO(base64.b64decode(payload["data"]))) as preview:
            self.assertEqual(result["preview_size"], list(preview.size))
            self.assertAlmostEqual(result["coordinate_mapping"]["preview_to_screen"]["x"], 800 / preview.width)
        self.assertTrue(Path(result["path"]).is_file())
        self.assertEqual(self.desktop.events, [])

    async def test_real_action_flow_uses_input_not_state_injection(self):
        result = await computer_use.run([
            {"type": "move", "x": 120, "y": 180},
            {"type": "click", "x": 120, "y": 180},
            {"type": "press", "key": "space"},
            {"type": "hotkey", "keys": ["ctrl", "a"]},
            {"type": "type", "text": "Aa!"},
            {"type": "scroll", "clicks": -2},
            {"type": "wait", "seconds": 0.01},
        ], space="screen")
        self.assertEqual(result["actions_completed"], 7)
        self.assertIn(("down", "left"), self.desktop.events)
        self.assertIn(("key_down", "ctrl"), self.desktop.events)
        self.assertIn(("scroll", -240), self.desktop.events)
        self.assertEqual(len(self.attachments), 1)

    async def test_noisy_4k_raw_png_keeps_bounded_decodable_preview(self):
        import random
        from PIL import Image

        attachment = importlib.import_module("attach_image.attach_image")
        size = (3840, 2160)
        pixels = random.Random(0).randbytes(size[0] * size[1] * 3)
        with patch.object(self.desktop, "size", return_value=size), patch.object(
            self.desktop, "screenshot", side_effect=lambda: Image.frombytes("RGB", size, pixels)
        ):
            result = await computer_use.run([{"type": "press", "key": "space"}])
        self.assertGreater(Path(result["path"]).stat().st_size, 20_000_000)
        payload = self.attachments[-1][attachment._ATTACHMENT_DISPLAY_MIME]
        self.assertLessEqual(len(payload["data"]), attachment._MAX_ATTACHMENT_DATA_CHARS)
        with Image.open(io.BytesIO(base64.b64decode(payload["data"], validate=True))) as image:
            image.load()
            self.assertEqual(result["preview_size"], list(image.size))
            self.assertEqual(result["coordinate_mapping"]["preview_to_screen"],
                             {"x": size[0] / image.width, "y": size[1] / image.height})
        self.assertEqual(result["actions_completed"], 1)

    async def test_invalid_final_action_does_not_allow_earlier_input(self):
        invalid = [
            {"type": "move", "x": -1, "y": 20},
            {"type": "move", "x": 800, "y": 20},
            {"type": "move", "x": float("nan"), "y": 20},
            {"type": "click", "x": True, "y": 20},
            {"type": "type", "text": "bell"},
            {"type": "press", "key": "invented-key"},
            {"type": "press", "key": "space", "presses": 21},
            {"type": "wait", "seconds": 2.1},
            {"type": "type", "text": "a" * 501},
            {"type": "mouseDown"},
            {"type": "click", "unexpected": 1},
        ]
        for bad in invalid:
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                await computer_use.run([{"type": "press", "key": "space"}, bad], space="screen")
            self.assertEqual(self.desktop.events, [])
            self.assertEqual(self.attachments, [])

    async def test_batch_count_and_time_limits_reject_before_input(self):
        for actions in (
            [{"type": "press", "key": "space"}] * 33,
            [{"type": "wait", "seconds": 2}] * 6,
        ):
            with self.assertRaises(ValueError):
                await computer_use.run(actions)
        self.assertEqual(self.desktop.events, [])

    async def test_nonvision_model_cannot_send_input(self):
        self.vision["input"] = ["text"]
        with self.assertRaises(RuntimeError):
            await computer_use.run([{"type": "press", "key": "space"}])
        self.assertEqual(self.desktop.events, [])

    async def test_geometry_mismatch_cannot_send_input(self):
        self.desktop.image_size = (1600, 1200)
        with self.assertRaises(RuntimeError):
            await computer_use.run([{"type": "press", "key": "space"}])
        self.assertEqual(self.desktop.events, [])

    async def test_disabled_failsafe_is_not_silently_accepted(self):
        self.desktop.FAILSAFE = False
        with self.assertRaises(RuntimeError):
            await computer_use.run([{"type": "press", "key": "space"}])
        self.assertEqual(self.desktop.events, [])

    async def test_failsafe_after_key_down_releases_only_owned_key(self):
        self.desktop.down_hook = lambda: setattr(self.desktop, "corner", True)
        with self.assertRaises(RuntimeError):
            await computer_use.run([{"type": "hotkey", "keys": ["ctrl", "a"]}])
        self.assertIn(("cleanup_key", "ctrl"), self.desktop.events)
        self.assertNotIn(("cleanup_key", "a"), self.desktop.events)
        self.assertTrue(self.desktop.FAILSAFE)

    async def test_layout_modifiers_override_us_punctuation_assumptions(self):
        mapping = self.desktop.platformModule.keyboardMapping
        for mapped, expected in (
            (mapping["q"] | 0x600, ["ctrl", "alt", "q"]),
            (mapping["q"], ["q"]),
        ):
            self.desktop.events.clear()
            mapping["@"] = mapped
            await computer_use.run([{"type": "type", "text": "@"}])
            self.assertEqual(self.desktop.events, [
                *(("key_down", key) for key in expected),
                *(("key_up", key) for key in reversed(expected)),
            ])

        self.desktop.events.clear()
        # A glyph the layout cannot type safely is still text: it goes as
        # Unicode input, never as the unsafe keystroke.
        self.desktop.events.clear()
        mapping["&"] = 0x31
        mapping["1"] = 0x131
        await computer_use.run([{"type": "type", "text": "&"}])
        self.assertEqual(self.desktop.events, [("unicode", "&")])
        # A key to press has no such fallback: it is refused before any input.
        self.desktop.events.clear()
        with self.assertRaises(ValueError):
            await computer_use.run([{"type": "press", "key": "space"}, {"type": "hotkey", "keys": ["ctrl", "&"]}])
        self.assertEqual(self.desktop.events, [])

    async def test_shifted_ascii_failure_releases_only_explicit_unmodified_keys(self):
        count = 0

        def corner_after_base_down():
            nonlocal count
            count += 1
            if count == 2:
                self.desktop.corner = True

        self.desktop.down_hook = corner_after_base_down
        with self.assertRaises(RuntimeError):
            await computer_use.run([{"type": "type", "text": "A!"}])
        self.assertEqual(self.desktop.events, [
            ("key_down", "shift"), ("key_down", "a"),
            ("cleanup_key", "a"), ("cleanup_key", "shift"),
        ])
        self.assertTrue(self.desktop.FAILSAFE)

    @unittest.skipUnless(sys.platform == "win32", "session mutex is Windows-only")
    async def test_named_mutex_excludes_a_different_kernel_process(self):
        import subprocess
        marker = Path(self.temp.name) / "locked"
        source = str(ROOT / ".agents/skills/computer-use/src")
        child_code = (
            "import sys,pathlib; sys.path.insert(0,sys.argv[1]); "
            "import computer_use,importlib; "
            "impl=importlib.import_module(computer_use.run.__module__); "
            "lock=impl._desktop_mutex(); lock.__enter__(); "
            "pathlib.Path(sys.argv[2]).touch(); sys.stdin.readline(); "
            "lock.__exit__(None,None,None)"
        )
        child = subprocess.Popen(
            [sys.executable, "-c", child_code, source, str(marker)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True,
        )
        try:
            for _ in range(200):
                if marker.exists() or child.poll() is not None:
                    break
                await asyncio.sleep(0.01)
            self.assertTrue(marker.exists(), "other kernel did not acquire mutex")
            with patch.object(implementation, "_desktop_mutex", REAL_MUTEX):
                with self.assertRaisesRegex(RuntimeError, "busy|control|using|locked"):
                    await computer_use.run([{"type": "press", "key": "space"}])
            self.assertEqual(self.desktop.events, [])
        finally:
            child.communicate("release\n", timeout=5)
            self.assertEqual(child.returncode, 0)

    async def test_repeated_cancellation_drains_atomic_input_and_releases_mouse(self):
        started = threading.Event()
        release = threading.Event()

        def block_after_down():
            started.set()
            if not release.wait(2):
                raise RuntimeError("test did not release fake input")

        self.desktop.down_hook = block_after_down
        task = asyncio.create_task(computer_use.run([
            {"type": "click"}, {"type": "press", "key": "space"},
        ], space="screen"))
        try:
            for _ in range(100):
                if started.is_set():
                    break
                await asyncio.sleep(0.01)
            self.assertTrue(started.is_set())
            task.cancel()
            await asyncio.sleep(0.01)
            task.cancel()
            await asyncio.sleep(0.01)
            self.assertFalse(task.done(), "cancellation must wait for atomic worker cleanup")
        finally:
            release.set()
        with self.assertRaises(asyncio.CancelledError):
            await task
        releases = [event for event in self.desktop.events
                    if event in (("up", "left"), ("cleanup_mouse", "left"))]
        self.assertEqual(len(releases), 1)
        self.assertNotIn(("key_down", "space"), self.desktop.events)
        settled = list(self.desktop.events)
        await asyncio.sleep(0.08)
        self.assertEqual(self.desktop.events, settled)


    async def test_preview_coordinates_map_to_the_screen_the_model_saw(self):
        from PIL import Image
        size = (2400, 1350)
        with patch.object(self.desktop, "size", return_value=size), patch.object(
            self.desktop, "screenshot", side_effect=lambda: Image.new("RGB", size, "#ffffff")
        ):
            with self.assertRaisesRegex(ValueError, "screenshot"):
                await computer_use.run([{"type": "click", "x": 10, "y": 10}])
            self.assertEqual(self.desktop.events, [])
            shot = await computer_use.screenshot()
            preview_w, preview_h = shot["preview_size"]
            await computer_use.run([{"type": "click", "x": preview_w / 2, "y": preview_h / 2}])
            moved = [event for event in self.desktop.events if event[0] == "move"]
            self.assertEqual(len(moved), 1)
            _, x, y = moved[0]
            self.assertAlmostEqual(x, size[0] / 2, delta=size[0] / preview_w)
            self.assertAlmostEqual(y, size[1] / 2, delta=size[1] / preview_h)
            self.desktop.events.clear()
            with self.assertRaises(ValueError):
                await computer_use.run([{"type": "click", "x": preview_w, "y": 0}])
            self.assertEqual(self.desktop.events, [])
        # The screen changed size since that screenshot: refused before input.
        with self.assertRaisesRegex(RuntimeError, "size changed"):
            await computer_use.run([{"type": "click", "x": 1, "y": 1}])
        self.assertEqual(self.desktop.events, [])

    async def test_text_in_any_language_goes_as_unicode_and_ascii_as_keys(self):
        await computer_use.run([{"type": "type", "text": "Xin chào 😀"}])
        typed = [event for event in self.desktop.events if event[0] in ("unicode", "key_down")]
        self.assertIn(("unicode", "à"), typed)
        self.assertIn(("unicode", "😀"), typed)
        self.assertIn(("key_down", "x"), typed, "ASCII stays keystrokes, for apps that read key codes")

    async def test_a_drag_holds_the_button_across_steps_and_releases_it(self):
        await computer_use.run([{"type": "drag", "x": 100, "y": 100, "to_x": 300, "to_y": 200}], space="screen")
        events = self.desktop.events
        down, up = events.index(("down", "left")), events.index(("up", "left"))
        steps = [event for event in events[down:up] if event[0] == "move"]
        self.assertEqual(len(steps), implementation._DRAG_STEPS)
        self.assertEqual(steps[-1], ("move", 300, 200))

    async def test_a_drag_stopped_midway_releases_its_button(self):
        moves = 0

        def corner_after_some_steps():
            nonlocal moves
            moves += 1

        original = self.desktop.moveTo

        def move(x, y, duration=0, _pause=True):
            original(x, y, duration, _pause)
            corner_after_some_steps()
            if moves == 4:
                self.desktop.corner = True

        with patch.object(self.desktop, "moveTo", side_effect=move):
            with self.assertRaises(RuntimeError):
                await computer_use.run([{"type": "drag", "x": 100, "y": 100, "to_x": 300, "to_y": 200}], space="screen")
        self.assertIn(("cleanup_mouse", "left"), self.desktop.events)

    async def test_horizontal_scroll_sends_the_horizontal_wheel(self):
        await computer_use.run([{"type": "scroll", "clicks": 3, "horizontal": True}])
        self.assertIn(("hwheel", 360), self.desktop.events)
        self.assertNotIn(("scroll", 360), self.desktop.events)

    async def test_zoom_shows_a_region_at_full_resolution_and_keeps_the_view(self):
        from PIL import Image
        await computer_use.screenshot()
        view = implementation._LAST_VIEW
        result = await computer_use.zoom([0, 0, 100, 50], space="screen")
        self.assertEqual(result["region"], [0, 0, 100, 50])
        payload = self.attachments[-1]["application/vnd.prime-agent.attachment+json"]
        with Image.open(io.BytesIO(base64.b64decode(payload["data"]))) as image:
            self.assertEqual(image.size, (100, 50))
        self.assertEqual(implementation._LAST_VIEW, view, "a zoom is not what preview coordinates read against")
        with self.assertRaises(ValueError):
            await computer_use.zoom([700, 0, 200, 10], space="screen")

    async def test_old_captures_are_pruned(self):
        with patch.object(implementation, "_KEEP_CAPTURES", 3):
            for _ in range(5):
                await computer_use.screenshot()
        folder = Path(self.temp.name) / ".harness" / "artifacts" / "computer-use"
        self.assertEqual(len(list(folder.glob("screen-*.png"))), 3)


if __name__ == "__main__":
    unittest.main()
