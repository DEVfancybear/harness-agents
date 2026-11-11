"""The Jev pilot and settled screenshots: fake desktop, fake Jev; CI never moves the mouse."""

import asyncio
from contextlib import nullcontext
import importlib
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(ROOT / ".agents/skills/computer-use/src"))
sys.path.insert(0, str(ROOT / ".agents/skills/attach-image/src"))
sys.path.insert(0, str(ROOT / ".agents/skills/jev/src"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
import computer_use
from test_computer_use import FakeDesktop

desktop = importlib.import_module(computer_use.run.__module__)
pilot_module = importlib.import_module(computer_use.pilot.__module__)


class ReactingDesktop(FakeDesktop):
    """A screen that changes after each click or typed character, unless `frozen`."""

    def __init__(self):
        super().__init__()
        self.frozen = False
        self.platformModule.keyboardMapping["end"] = 0x23  # VK_END, as on Windows

    def screenshot(self):
        from PIL import Image, ImageDraw

        image = Image.new("RGB", self.image_size, "#aabbcc")
        if not self.frozen:
            done = sum(1 for event in self.events if event[0] in ("down", "key_down", "unicode", "scroll"))
            ImageDraw.Draw(image).rectangle((0, 0, 40 * done, 300), fill="#112233")
        return image


def typed(events):
    """The text a run of fake key events types: shift + a key is its capital."""
    text, shift = "", False
    for event in events:
        if event[0] == "unicode":
            text += event[1]
        elif event[0] == "key_down":
            key = event[1]
            if key == "shift":
                shift = True
            elif key == "space":
                text += " "
            elif len(key) == 1:
                text += key.upper() if shift else key
        elif event[0] == "key_up" and event[1] == "shift":
            shift = False
    return text


def answer(choice, confidence, probabilities=None):
    return {"type": "choice", "choice": choice, "confidence": confidence,
            "probabilities": probabilities or {choice: confidence}}


def target(left, top, right, bottom, confidence=0.9):
    return {"type": "box", "box": {"left": left, "top": top, "right": right, "bottom": bottom},
            "confidence": confidence}


class FakeJev:
    """Answers each screen request with the next scripted answers; close-ups with `closeup`."""

    JevError = RuntimeError

    def __init__(self, steps, closeup=None):
        self.steps = list(steps)
        self.closeup = closeup
        self.requests = []

    def _api_key(self):
        return "test-key"

    async def decide(self, state, questions, *, assets=None, threshold=None, model=None, timeout=45):
        self.requests.append({"state": state, "questions": questions, "assets": assets, "model": model})
        if state.get("screen") == "closeup":
            answers = {"target": self.closeup}
        else:
            answers = self.steps.pop(0)
        return {"answers": answers, "usage": {"input_tokens": 1000}, "cost": 0.000042}


class PilotTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ha-pilot-unit-")
        self.addCleanup(self.temp.cleanup)
        self.desktop = ReactingDesktop()
        self.attachments = []
        self.vision = {"id": "fake-vision", "input": ["image"]}
        self.opened = []
        self.known_hosts = set()

        async def host_request(kind, *args, **kwargs):
            self.assertEqual(kind, "model.info")
            return self.vision

        self.runtime = SimpleNamespace(host_request=host_request, emit=self.attachments.append)
        for patcher in (
            patch.dict(sys.modules, {"rlm": self.runtime}),
            patch.object(desktop, "_load_pyautogui", return_value=self.desktop),
            patch.object(desktop, "_desktop_mutex", side_effect=nullcontext),
            patch("pathlib.Path.cwd", return_value=Path(self.temp.name)),
            patch.object(desktop, "_send_unicode", side_effect=lambda char: self.desktop.events.append(("unicode", char))),
            patch.object(desktop, "_LAST_VIEW", None),
            patch.object(desktop, "_REACT_SECONDS", 0.3),
            patch.object(desktop, "_SETTLE_INTERVAL", 0.01),
            patch.object(pilot_module.webbrowser, "open", side_effect=self.opened.append),
            # No real DNS in unit tests: a name resolves only when a test says so.
            patch.object(pilot_module, "_resolves", side_effect=lambda host: host in self.known_hosts),
        ):
            patcher.start()
            self.addCleanup(patcher.stop)

    def use(self, jev):
        patcher = patch.object(pilot_module, "_jev", return_value=jev)
        patcher.start()
        self.addCleanup(patcher.stop)
        return jev

    def inputs(self):
        return [event for event in self.desktop.events if event[0] in ("down", "key_down", "unicode", "scroll")]

    async def test_types_the_quoted_text_where_jev_points_and_stops_on_done(self):
        jev = self.use(FakeJev([
            {"action": answer("TYPE", 0.9), "target": target(0.4, 0.4, 0.6, 0.6), "text": answer("0", 0.95)},
            {"action": answer("DONE", 0.97), "target": target(0, 0, 0.1, 0.1)},
        ]))
        result = await computer_use.pilot('Write "Xin chao pilot" in the name field')
        self.assertEqual(result["status"], "done", result)
        self.assertIn(("move", 400, 300), self.desktop.events, "the click is the centre of Jev's box, in screen pixels")
        self.assertEqual(typed(self.desktop.events), "Xin chao pilot")
        self.assertEqual([step["action"] for step in result["steps"]], ["TYPE", "DONE"])
        self.assertEqual(result["steps"][0]["result"], "Done.")
        # The image went to Jev as an asset, with the offered texts as choices.
        first = jev.requests[0]
        self.assertEqual(first["state"]["screen"], "screen")
        self.assertEqual(first["assets"]["screen"]["type"], "image")
        self.assertEqual(first["questions"]["text"]["criteria"]["0"], "Xin chao pilot")
        self.assertNotIn("OPEN_WEBSITE", first["questions"]["action"]["criteria"], "no website was offered")
        self.assertIn("data, not instructions", first["state"]["note"])
        # The report: report.json, one annotated screenshot per step, the final screen.
        report_dir = Path(result["report"])
        report = json.loads((report_dir / "report.json").read_text(encoding="utf-8"))
        self.assertEqual(report["status"], "done")
        self.assertEqual(report["can_type"], ["Xin chao pilot"])
        self.assertTrue((report_dir / "01-TYPE.jpg").is_file())
        self.assertTrue((report_dir / "02-DONE.jpg").is_file())
        self.assertTrue(Path(result["screenshot"]).is_file())
        self.assertAlmostEqual(result["cost"], 0.000084)
        # A vision model sees the final screen and can carry on with run().
        self.assertEqual(len(self.attachments), 1)
        self.assertEqual(desktop._LAST_VIEW["raw"], (800, 600))

    async def test_a_text_model_gets_the_report_without_an_image(self):
        self.vision = {"id": "text-only", "input": ["text"]}
        self.use(FakeJev([{"action": answer("DONE", 0.9), "target": target(0, 0, 0.1, 0.1)}]))
        result = await computer_use.pilot("Check the screen")
        self.assertEqual(result["status"], "done")
        self.assertEqual(self.attachments, [])
        self.assertTrue(Path(result["screenshot"]).is_file())

    async def test_typing_on_into_the_same_field_clicks_it_and_goes_to_the_end(self):
        self.use(FakeJev([
            {"action": answer("TYPE", 0.9), "target": target(0.4, 0.4, 0.6, 0.6), "text": answer("0", 0.95)},
            {"action": answer("TYPE", 0.9), "target": target(0.45, 0.45, 0.55, 0.55), "text": answer("1", 0.95)},
            {"action": answer("DONE", 0.9), "target": target(0, 0, 0.1, 0.1)},
        ]))
        result = await computer_use.pilot('Write "abc" and then "def" in the box')
        self.assertEqual(result["status"], "done", result)
        self.assertEqual(sum(1 for event in self.desktop.events if event == ("down", "left")), 2,
                         "the field is clicked again, so the text cannot go to another window")
        self.assertIn(("key_down", "end"), self.desktop.events)
        self.assertEqual(typed(self.desktop.events), "abc def")

    async def test_unsure_which_text_types_nothing(self):
        self.use(FakeJev([{"action": answer("TYPE_ENTER", 0.95), "target": target(0.4, 0.4, 0.6, 0.6),
                           "text": answer("0", 0.2)}]))
        result = await computer_use.pilot('Search for "aliens"')
        self.assertEqual(result["status"], "unsure")
        self.assertEqual(self.inputs(), [])
        self.assertIn("which text", result["steps"][0]["note"])

    async def test_arguments_are_checked_before_anything(self):
        self.use(FakeJev([]))
        for kwargs in ({"texts": "hello"}, {"websites": "https://x.test"}, {"max_steps": 0}, {"min_confidence": 2}):
            with self.assertRaises(ValueError):
                await computer_use.pilot("Click save", **kwargs)
        self.assertEqual(self.desktop.events, [])

    async def test_cancelling_mid_step_releases_the_button_and_says_stopped(self):
        self.use(FakeJev([{"action": answer("CLICK", 0.9), "target": target(0.4, 0.4, 0.6, 0.6)}]))
        loop = asyncio.get_running_loop()
        task = asyncio.ensure_future(computer_use.pilot("Click the save button"))

        def cancel_while_pressed():
            loop.call_soon_threadsafe(task.cancel)
            import time
            time.sleep(0.3)  # the worker sees the stop at its next checkpoint

        self.desktop.down_hook = cancel_while_pressed
        with self.assertRaises(asyncio.CancelledError):
            await task
        self.assertIn(("cleanup_mouse", "left"), self.desktop.events)
        self.assertNotIn(("up", "left"), self.desktop.events)
        folder = Path(self.temp.name) / ".harness/artifacts/computer-use/pilot"
        report = json.loads(next(folder.glob("*/report.json")).read_text(encoding="utf-8"))
        self.assertEqual(report["status"], "stopped")

    async def test_unsure_jev_does_nothing(self):
        self.use(FakeJev([{"action": answer("CLICK", 0.3), "target": target(0.4, 0.4, 0.6, 0.6)}]))
        result = await computer_use.pilot("Click the save button")
        self.assertEqual(result["status"], "unsure")
        self.assertEqual(self.inputs(), [])
        self.assertIn("Not done", result["steps"][0]["result"])

    async def test_harmless_actions_need_less_confidence(self):
        self.use(FakeJev([
            {"action": answer("SCROLL_DOWN", 0.3), "target": target(0.4, 0.4, 0.6, 0.6)},
            {"action": answer("DONE", 0.9), "target": target(0, 0, 0.1, 0.1)},
        ]))
        result = await computer_use.pilot("Scroll to the end of the list")
        self.assertEqual(result["status"], "done")
        self.assertIn(("scroll", -pilot_module.SCROLL_NOTCHES * 120), self.desktop.events)
        self.assertIn(("move", 400, 300), self.desktop.events, "it scrolls over what Jev pointed at")

    async def test_dry_run_decides_once_and_does_no_input(self):
        self.use(FakeJev([{"action": answer("CLICK", 0.9), "target": target(0.4, 0.4, 0.6, 0.6)}]))
        result = await computer_use.pilot("Click the save button", dry_run=True)
        self.assertEqual(result["status"], "dry_run")
        self.assertEqual(self.inputs(), [])
        self.assertEqual(result["steps"][0]["detail"], "at (400, 300)")

    async def test_a_text_jev_was_not_offered_is_never_typed(self):
        self.use(FakeJev([{"action": answer("TYPE", 0.9), "target": target(0.4, 0.4, 0.6, 0.6), "text": answer("99", 0.99)}]))
        result = await computer_use.pilot('Write "hello" in the box')
        self.assertEqual(result["status"], "unsure")
        self.assertEqual(self.inputs(), [])

    async def test_an_action_jev_was_not_offered_is_never_done(self):
        self.use(FakeJev([{"action": answer("OPEN_WEBSITE", 0.99), "target": target(0.4, 0.4, 0.6, 0.6)}]))
        result = await computer_use.pilot("Click the save button")
        self.assertEqual(result["status"], "unsure")
        self.assertEqual(self.opened, [])

    async def test_split_between_click_and_typing_does_only_the_click(self):
        split = {"CLICK": 0.3, "TYPE": 0.15, "TYPE_ENTER": 0.3, "WAIT": 0.25}
        self.use(FakeJev([
            {"action": answer("TYPE_ENTER", 0.3, split), "target": target(0.4, 0.4, 0.6, 0.6), "text": answer("0", 0.9)},
            {"action": answer("DONE", 0.9), "target": target(0, 0, 0.1, 0.1)},
        ]))
        result = await computer_use.pilot('Search for "aliens"')
        self.assertEqual(result["steps"][0]["action"], "CLICK")
        self.assertIn("split", result["steps"][0]["note"])
        self.assertFalse([event for event in self.desktop.events if event[0] == "key_down"], "nothing was typed")

    async def test_an_unsure_target_is_asked_again_on_a_close_up(self):
        # The close-up is the square around the box's centre; its answer maps back.
        jev = self.use(FakeJev([
            {"action": answer("CLICK", 0.9), "target": target(0.45, 0.45, 0.55, 0.55, confidence=0.3)},
            {"action": answer("DONE", 0.9), "target": target(0, 0, 0.1, 0.1)},
        ], closeup=target(0.0, 0.0, 0.1, 0.1, confidence=0.8)))
        result = await computer_use.pilot("Click the tiny icon")
        self.assertEqual(sum(1 for request in jev.requests if request["state"]["screen"] == "closeup"), 1)
        self.assertIn("Zoomed in", result["steps"][0]["note"])
        # The close-up is 324 px around (400, 300) from (238, 138); its box centre
        # (0.05, 0.05) is 16.2 px in: the click is at (254, 154).
        moves = [event for event in self.desktop.events if event[0] == "move"]
        self.assertEqual(moves[0], ("move", 254, 154))

    async def test_a_screen_that_never_changes_stops_after_five_steps(self):
        self.desktop.frozen = True
        steps = [{"action": answer("CLICK", 0.9), "target": target(0.4, 0.4, 0.6, 0.6)} for _ in range(6)]
        self.use(FakeJev(steps, closeup=target(0.4, 0.4, 0.6, 0.6, confidence=0.9)))
        result = await computer_use.pilot("Click the save button")
        self.assertEqual(result["status"], "unsure")
        self.assertEqual(len(result["steps"]), pilot_module.UNCHANGED_LIMIT)
        self.assertTrue(all("nothing changed" in step["result"] for step in result["steps"]))

    async def test_websites_open_only_from_the_task_or_the_caller(self):
        self.use(FakeJev([{"action": answer("DONE", 0.9), "target": target(0, 0, 0.1, 0.1)}]))
        result = await computer_use.pilot("Look at the docs", websites=["https://docs.example.test/start"])
        self.assertEqual(self.opened, ["https://docs.example.test/start"], "the first website opens before the first look")
        self.assertEqual(result["status"], "done")
        with self.assertRaises(ValueError):
            await computer_use.pilot("Look", websites=["file:///C:/Windows"])

    async def test_a_site_named_bare_opens_when_dns_knows_it(self):
        self.known_hosts = {"docs.example.test", "guide.md"}
        jev = self.use(FakeJev([
            {"action": answer("OPEN_WEBSITE", 0.9), "target": target(0, 0, 0.1, 0.1), "website": answer("0", 0.9)},
            {"action": answer("DONE", 0.9), "target": target(0, 0, 0.1, 0.1)},
        ]))
        result = await computer_use.pilot("Go to docs.example.test/start and compare with guide.md and report.json")
        self.assertEqual(result["status"], "done", result)
        # Nothing opens up front for a bare name: Jev chose to open it.
        self.assertEqual(self.opened, ["https://docs.example.test/start"])
        self.assertEqual(jev.requests[0]["state"]["history"], ["nothing yet"])
        websites = jev.requests[0]["questions"]["website"]["criteria"]
        self.assertEqual(list(websites.values()), ["https://docs.example.test/start", "https://guide.md"],
                         "report.json does not resolve; guide.md does, and is offered, never opened unasked")

    async def test_jev_model_is_pinned_to_the_one_upstream_was_tuned_on(self):
        jev = self.use(FakeJev([{"action": answer("DONE", 0.9), "target": target(0, 0, 0.1, 0.1)}] * 2))
        await computer_use.pilot("Check the screen")
        self.assertEqual(jev.requests[0]["model"], "jev-1.14.0")
        await computer_use.pilot("Check the screen", model="jev-latest")
        self.assertEqual(jev.requests[1]["model"], "jev-latest")
        with self.assertRaises(ValueError):
            await computer_use.pilot("Check the screen", model="")

    async def test_max_steps_hands_back(self):
        steps = [{"action": answer("SCROLL_DOWN", 0.9), "target": target(0.4, 0.4, 0.6, 0.6)} for _ in range(2)]
        self.use(FakeJev(steps))
        result = await computer_use.pilot("Scroll", max_steps=2)
        self.assertEqual(result["status"], "gave_up")
        self.assertEqual(len(result["steps"]), 2)

    async def test_no_key_is_refused_before_any_input(self):
        jev = importlib.import_module("jev.jev")
        with patch.dict("os.environ", {"TYPESAFE_API_KEY": "", "HA_CREDENTIAL_FILE": ""}):
            with self.assertRaises(jev.JevError) as caught:
                await computer_use.pilot("Click the save button")
        self.assertIn("/login typesafe", str(caught.exception))
        self.assertEqual(self.desktop.events, [])

    async def test_without_the_jev_plugin_it_says_to_turn_it_on(self):
        # The jev skill reaches the kernel only with /jev on (seen live: "no module jev").
        real = importlib.import_module

        def without_jev(name, *args, **kwargs):
            if name == "jev.jev":
                raise ModuleNotFoundError("No module named 'jev'")
            return real(name, *args, **kwargs)

        with patch.object(pilot_module.importlib, "import_module", side_effect=without_jev):
            with self.assertRaises(RuntimeError) as caught:
                await computer_use.pilot("Click the save button")
        self.assertIn("/jev on", str(caught.exception))
        self.assertEqual(self.desktop.events, [])

    async def test_old_runs_are_pruned(self):
        folder = Path(self.temp.name) / ".harness/artifacts/computer-use/pilot"
        for index in range(pilot_module.KEEP_RUNS + 5):
            (folder / f"20000101-0000{index:02d}-aaaaaa").mkdir(parents=True)
        self.use(FakeJev([{"action": answer("DONE", 0.9), "target": target(0, 0, 0.1, 0.1)}]))
        await computer_use.pilot("Check")
        self.assertEqual(len([path for path in folder.iterdir() if path.is_dir()]), pilot_module.KEEP_RUNS)


class OptionTests(unittest.TestCase):
    def test_text_options(self):
        options = pilot_module.text_options('Search youtube for Aliens then post "Great film!" at 1.30', ["given"])
        self.assertEqual(options[:2], ["given", "Great film!"])
        for phrase in ("Aliens", "youtube for Aliens", "1", "30", "1:30"):
            self.assertIn(phrase, options)
        self.assertNotIn("Great", options, "quoted text is offered whole, not in pieces")
        self.assertEqual(pilot_module.quoted_texts('post "Hello world'), ["Hello world"])

    def test_website_options_take_urls_and_names_dns_knows(self):
        task = ('Open x.com, then https://example.test/a, then en.wikipedia.org/wiki/Jev. and post "see go.dev" '
                "to me@mail.test; read notes.txt and C:/site/app.example.test, version 1.30, e.g. this")
        known = {"x.com", "en.wikipedia.org", "go.dev", "mail.test", "app.example.test", "notes.txt"} - {"notes.txt"}
        asked = []

        def resolves(host):
            asked.append(host)
            return host in known

        urls = pilot_module.website_options(task, ["https://given.test"], resolves)
        self.assertEqual(urls, ["https://given.test", "https://x.com", "https://example.test/a",
                                "https://en.wikipedia.org/wiki/Jev"])
        self.assertNotIn("go.dev", asked, "quoted text is text to type")
        self.assertNotIn("mail.test", asked, "an e-mail address is not a website")
        self.assertNotIn("app.example.test", asked, "part of a path is not a website")
        self.assertIn("notes.txt", asked)
        # Without a resolver (DNS timed out), only URLs and the caller's websites.
        self.assertEqual(pilot_module.website_options(task, [], None), ["https://example.test/a"])

    def test_a_bare_name_that_is_a_file_here_is_not_a_website(self):
        import os

        prior = os.getcwd()
        with tempfile.TemporaryDirectory() as folder:
            Path(folder, "build.zip").write_text("")
            os.chdir(folder)
            try:
                urls = pilot_module.website_options("Unpack build.zip", [], lambda host: True)
            finally:
                os.chdir(prior)
        self.assertEqual(urls, [])

    def test_split_chain(self):
        self.assertEqual(pilot_module._split_chain("CLICK", 0.9, {}, 0.5), ("CLICK", 0.9, ""))
        action, confidence, note = pilot_module._split_chain(
            "TYPE_ENTER", 0.3, {"CLICK": 0.1, "TYPE": 0.3, "TYPE_ENTER": 0.3}, 0.5)
        self.assertEqual(action, "TYPE")
        self.assertAlmostEqual(confidence, 0.6)
        self.assertIn("TYPE / TYPE_ENTER", note)


class SettleTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ha-settle-unit-")
        self.addCleanup(self.temp.cleanup)
        self.desktop = ReactingDesktop()

        async def host_request(kind, *args, **kwargs):
            return {"id": "fake-vision", "input": ["image"]}

        for patcher in (
            patch.dict(sys.modules, {"rlm": SimpleNamespace(host_request=host_request, emit=lambda _: None)}),
            patch.object(desktop, "_load_pyautogui", return_value=self.desktop),
            patch.object(desktop, "_desktop_mutex", side_effect=nullcontext),
            patch("pathlib.Path.cwd", return_value=Path(self.temp.name)),
            patch.object(desktop, "_LAST_VIEW", None),
            patch.object(desktop, "_REACT_SECONDS", 0.3),
            patch.object(desktop, "_SETTLE_INTERVAL", 0.01),
        ):
            patcher.start()
            self.addCleanup(patcher.stop)

    async def test_settled_run_says_the_screen_changed(self):
        result = await computer_use.run([{"type": "click", "x": 10, "y": 10}], space="screen", settle=True)
        self.assertIs(result["screen_changed"], True)
        self.assertIn("waited", result["settle"])

    async def test_settled_run_says_when_nothing_changed(self):
        self.desktop.frozen = True
        result = await computer_use.run([{"type": "click", "x": 10, "y": 10}], space="screen", settle=True)
        self.assertIs(result["screen_changed"], False)

    async def test_plain_run_does_not_wait(self):
        result = await computer_use.run([{"type": "click", "x": 10, "y": 10}], space="screen")
        self.assertNotIn("screen_changed", result)

    def test_what_moves_on_its_own_is_ignored(self):
        from PIL import Image, ImageDraw

        before = Image.new("L", (400, 300), 0)
        blink = before.copy()
        ImageDraw.Draw(blink).rectangle((10, 10, 30, 30), fill=255)
        mask = desktop._changed(before, blink)
        self.assertGreater(desktop._share(mask), desktop._REACTED)
        self.assertLessEqual(desktop._share(mask, desktop._grow(mask)), desktop._REACTED)


class JevAssetTests(unittest.IsolatedAsyncioTestCase):
    async def test_images_go_as_assets_with_the_image_header(self):
        jev = importlib.import_module("jev.jev")
        sent = []

        class Client:
            async def post(self, url, json, headers):
                sent.append((json, headers))
                return SimpleNamespace(status_code=200, headers={}, json=lambda: {"answers": {}}, text="")

        with patch.dict("os.environ", {"TYPESAFE_API_KEY": "test-key"}):
            await jev._post(Client(), {"state": {}, "questions": {}})
            await jev._post(Client(), {"state": {"screen": "screen"}, "questions": {},
                                       "assets": {"screen": {"type": "image", "format": "jpg", "data_base64": "AA=="}}})
        self.assertNotIn(jev.IMAGE_HEADER, sent[0][1])
        self.assertEqual(sent[1][1][jev.IMAGE_HEADER], "true")

    async def test_decide_passes_assets_and_judges_a_box(self):
        jev = importlib.import_module("jev.jev")
        bodies = []

        async def post(client, body):
            bodies.append(body)
            return {"answers": {"target": {"type": "box", "box": {"left": 0.1, "top": 0.1, "right": 0.2, "bottom": 0.2},
                                           "confidence": 0.97}}, "usage": {"input_tokens": 10}}

        with patch.object(jev, "_post", side_effect=post):
            result = await jev.decide({"screen": "screen"}, {"target": {"type": "box", "instructions": "x"}},
                                      assets={"screen": {"type": "image"}}, threshold=0.9)
        self.assertEqual(bodies[0]["assets"], {"screen": {"type": "image"}})
        self.assertEqual(result["answers"]["target"]["decision"]["left"], 0.1)
        self.assertTrue(result["answers"]["target"]["accepted"])


if __name__ == "__main__":
    unittest.main()
