#!/usr/bin/env python3
"""Opt-in, real Windows desktop integration test; never run during unit tests.

Run in a native Windows Python environment containing the skill's dependencies:
    python scripts/testing/smoke_computer_use.py --run

This takes over the foreground with an owned Tk scratch window, moves the real
mouse, sends real keyboard/wheel input, and saves full-primary-desktop screenshots
locally. Do not touch the mouse/keyboard or switch windows during the test. No
provider, kernel, game, dependency installer, or simulated GUI backend is used.
"""

from __future__ import annotations

import argparse
import asyncio
import base64
import ctypes
import importlib
import io
import json
import os
import shutil
import sys
import time
import types
from pathlib import Path
from typing import Any

REPO = Path(__file__).resolve().parents[2]
ARTIFACTS = REPO / ".harness" / "artifacts" / "computer-use-smoke"
ATTACHMENT_MIME = "application/vnd.prime-agent.attachment+json"
MARKER_COLOR = (20, 120, 200)
ASCII_TEXT = "computer-use-Smoke!"


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


class ScratchDesktop:
    """Owned Tk widgets plus read-only native foreground/coordinate checks."""

    def __init__(self, tk: Any, backend: Any) -> None:
        from ctypes import wintypes

        self.backend = backend
        self.user32 = ctypes.WinDLL("user32", use_last_error=True)
        self.user32.GetForegroundWindow.restype = wintypes.HWND
        self.user32.GetAncestor.argtypes = [wintypes.HWND, wintypes.UINT]
        self.user32.GetAncestor.restype = wintypes.HWND
        self.user32.WindowFromPoint.argtypes = [wintypes.POINT]
        self.user32.WindowFromPoint.restype = wintypes.HWND
        self.user32.GetClientRect.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.RECT)]
        self.user32.GetClientRect.restype = wintypes.BOOL
        self.user32.ClientToScreen.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.POINT)]
        self.user32.ClientToScreen.restype = wintypes.BOOL
        self.user32.SetForegroundWindow.argtypes = [wintypes.HWND]
        self.user32.SetForegroundWindow.restype = wintypes.BOOL
        self.point_type = wintypes.POINT
        self.rect_type = wintypes.RECT
        self.root = tk.Tk()
        self.closed = False
        self.counts = {name: 0 for name in (
            "button_commands", "mouse_down", "mouse_up", "mouse_motion",
            "key_down", "key_up", "wheel",
        )}
        self.events: list[dict[str, Any]] = []
        try:
            self.root.title("computer_use smoke: OWNED SCRATCH WINDOW ONLY")
            self.root.configure(background="white")
            self.root.protocol("WM_DELETE_WINDOW", self._closed_by_user)
            self.root.state("zoomed")
            self.root.attributes("-topmost", True)
            self.marker = tk.Frame(self.root, height=64, background="#1478c8")
            self.marker.pack(fill="x", padx=24, pady=(20, 12))
            tk.Label(
                self.root,
                text="Real desktop smoke test — do not touch mouse/keyboard or switch windows",
                background="white", font=("Segoe UI", 13),
            ).pack(pady=6)
            self.button = tk.Button(
                self.root, text="Scratch click target", command=self._button_command,
                font=("Segoe UI", 14), width=28,
            )
            self.button.pack(pady=12)
            tk.Label(self.root, text="ASCII typing / BackSpace target", background="white").pack()
            self.textbox = tk.Text(self.root, height=3, width=50, font=("Consolas", 14))
            self.textbox.pack(padx=24, pady=8)
            tk.Label(self.root, text="Wheel target (scratch text only)", background="white").pack()
            self.scrollbox = tk.Text(self.root, height=9, width=60, font=("Consolas", 12))
            self.scrollbox.pack(fill="both", expand=True, padx=24, pady=(8, 24))
            self.scrollbox.insert("1.0", "\n".join(f"Scratch scroll line {i:03d}" for i in range(200)))
            # Observe before Tk's widget/class bindings (Text may return break).
            # This is only a listener; normal native widget behavior stays intact.
            observer = f"ScratchSmokeObserver-{id(self)}"
            pending = [self.root]
            while pending:
                widget = pending.pop()
                widget.bindtags((observer,) + widget.bindtags())
                pending.extend(widget.winfo_children())
            for sequence, name in (
                ("<ButtonPress>", "mouse_down"), ("<ButtonRelease>", "mouse_up"),
                ("<Motion>", "mouse_motion"), ("<KeyPress>", "key_down"),
                ("<KeyRelease>", "key_up"), ("<MouseWheel>", "wheel"),
            ):
                self.root.bind_class(observer, sequence, lambda event, name=name: self._record(name, event), add="+")
            self.root.update()
            self.hwnd = self.user32.GetAncestor(self.root.winfo_id(), 2)  # GA_ROOT
            require(bool(self.hwnd), "Cannot identify the scratch window's native HWND")
            self.root.lift()
            self.root.focus_force()
            self.user32.SetForegroundWindow(self.hwnd)
            self.root.update()
        except BaseException:
            self.destroy()
            raise

    def _closed_by_user(self) -> None:
        # Keep the owned window present until any cancelled backend work drains.
        self.closed = True

    def _button_command(self) -> None:
        self.counts["button_commands"] += 1
        self.events.append({"event": "button_command", "time": time.monotonic()})

    def _record(self, name: str, event: Any) -> None:
        self.counts[name] += 1
        if len(self.events) < 256:
            self.events.append({
                "event": name, "time": time.monotonic(),
                "keysym": str(getattr(event, "keysym", "")),
                "char": str(getattr(event, "char", "")),
                "delta": getattr(event, "delta", 0),
                "x": getattr(event, "x_root", 0), "y": getattr(event, "y_root", 0),
            })

    def pump(self, check: bool = True) -> None:
        self.root.update()
        if check:
            self.verify_foreground()

    def verify_foreground(self) -> None:
        require(not self.closed, "Scratch window was closed by the user; aborting")
        require(self.user32.GetForegroundWindow() == self.hwnd,
                "Scratch window lost foreground; aborting all further input")
        require(self.backend.FAILSAFE is True, "PyAutoGUI fail-safe must stay enabled")
        require(bool(self.root.winfo_viewable()), "Scratch window is not visible")

    def client_bounds(self) -> tuple[int, int, int, int]:
        rect = self.rect_type()
        origin = self.point_type(0, 0)
        require(bool(self.user32.GetClientRect(self.hwnd, ctypes.byref(rect))),
                "Cannot obtain scratch client bounds")
        require(bool(self.user32.ClientToScreen(self.hwnd, ctypes.byref(origin))),
                "Cannot obtain scratch screen origin")
        return origin.x, origin.y, origin.x + rect.right, origin.y + rect.bottom

    def coordinate(self, widget: Any) -> tuple[int, int]:
        self.verify_foreground()
        x = widget.winfo_rootx() + widget.winfo_width() // 2
        y = widget.winfo_rooty() + widget.winfo_height() // 2
        left, top, right, bottom = self.client_bounds()
        width, height = self.backend.size()
        require(left + 8 <= x < right - 8 and top + 8 <= y < bottom - 8,
                f"Target {(x, y)} is not safely inside the owned scratch client")
        require(8 <= x < width - 8 and 8 <= y < height - 8,
                f"Target {(x, y)} is outside the primary screen or near a fail-safe corner")
        target = self.user32.WindowFromPoint(self.point_type(x, y))
        require(self.user32.GetAncestor(target, 2) == self.hwnd,
                f"Target {(x, y)} belongs to another window; refusing input")
        return x, y

    def validate_actions(self, actions: list[dict[str, Any]]) -> None:
        self.verify_foreground()
        left, top, right, bottom = self.client_bounds()
        for action in actions:
            if "x" in action or "y" in action or action["type"] in {"click", "scroll"}:
                if "x" in action or "y" in action:
                    x, y = action["x"], action["y"]
                else:
                    x, y = self.backend.position()
                require(left + 8 <= x < right - 8 and top + 8 <= y < bottom - 8,
                        "Batch contains coordinates outside the owned scratch window")
                target = self.user32.WindowFromPoint(self.point_type(x, y))
                require(self.user32.GetAncestor(target, 2) == self.hwnd,
                        "Batch target is obscured by another window")
            if action["type"] in {"type", "press", "hotkey"}:
                focus = self.root.focus_get()
                require(focus is not None and focus.winfo_toplevel() == self.root,
                        "Keyboard target is not owned by the scratch window")

    def text(self) -> str:
        return self.textbox.get("1.0", "end-1c")

    def destroy(self) -> None:
        try:
            self.root.destroy()
        except Exception:
            pass


class Attachments:
    """Stub only the runtime transport, never screenshot/input implementations."""

    def __init__(self) -> None:
        self.emissions: list[dict[str, Any]] = []
        self.manifest: list[dict[str, Any]] = []
        self.host_requests: list[str] = []

    async def host_request(self, request_type: str, payload: Any = None) -> dict[str, Any]:
        require(request_type == "model.info", f"Unexpected host request: {request_type}")
        self.host_requests.append(request_type)
        return {"id": "local-smoke-no-provider", "input": ["image"]}

    def emit(self, data: dict[str, Any]) -> None:
        require(ATTACHMENT_MIME in data, "Expected a genuine attach_image attachment")
        self.emissions.append(data)

    def decode_since(self, before: int, image_module: Any) -> list[dict[str, Any]]:
        require(len(self.emissions) > before, "Screenshot did not emit an image attachment")
        decoded = []
        for emission in self.emissions[before:]:
            attachment = emission[ATTACHMENT_MIME]
            data = base64.b64decode(attachment["data"], validate=True)
            with image_module.open(io.BytesIO(data)) as image:
                image.load()  # Decode real image pixels; signatures alone are not evidence.
                require(image.width > 0 and image.height > 0, "Attachment image is empty")
                suffix = {"image/png": ".png", "image/jpeg": ".jpg"}.get(attachment["mime_type"], ".img")
                path = ARTIFACTS / f"attachment-{len(self.manifest) + 1:02d}{suffix}"
                path.write_bytes(data)
                record = {"path": str(path), "source": attachment["path"],
                          "mime_type": attachment["mime_type"], "size": list(image.size),
                          "bytes": len(data)}
                self.manifest.append(record)
                decoded.append(record)
        return decoded


def import_skills(attachments: Attachments) -> tuple[Any, Any, list[str]]:
    sources = [REPO / ".agents/skills/computer-use/src", REPO / ".agents/skills/attach-image/src"]
    for source in sources:
        require(source.is_dir(), f"Missing repository skill sources: {source}")
    previous_rlm = sys.modules.get("rlm")
    stub = types.ModuleType("rlm")
    stub.host_request = attachments.host_request
    stub.emit = attachments.emit
    sys.modules["rlm"] = stub
    added = [str(source) for source in sources]
    sys.path[:0] = added
    try:
        attach_image = importlib.import_module("attach_image")
        computer_use = importlib.import_module("computer_use")
        for module, source in ((computer_use, sources[0]), (attach_image, sources[1])):
            require(Path(module.__file__).resolve().is_relative_to(source.resolve()),
                    f"Imported {module.__name__} outside the repository sources")
        require(callable(computer_use.screenshot) and callable(computer_use.run),
                "Expected async computer_use.screenshot() and run(actions)")
        return computer_use, previous_rlm, added
    except BaseException:
        restore_imports(previous_rlm, added)
        raise


def restore_imports(previous_rlm: Any, added: list[str]) -> None:
    if previous_rlm is None:
        sys.modules.pop("rlm", None)
    else:
        sys.modules["rlm"] = previous_rlm
    for source in added:
        if source in sys.path:
            sys.path.remove(source)


async def drain(task: asyncio.Task[Any], desktop: ScratchDesktop) -> None:
    task.cancel()
    # Never abandon an in-flight native worker when an assertion/timeout occurs.
    # The skill's bounded cooperative drain must finish before this window closes.
    while not task.done():
        desktop.pump(check=False)
        await asyncio.sleep(0.01)
    await asyncio.gather(task, return_exceptions=True)


async def guarded(awaitable: Any, desktop: ScratchDesktop, timeout: float = 12.0) -> Any:
    task = asyncio.create_task(awaitable)
    deadline = time.monotonic() + timeout
    try:
        while not task.done():
            desktop.pump()
            require(time.monotonic() < deadline, "Timed out waiting for bounded desktop operation")
            await asyncio.sleep(0.01)
        result = task.result()
        desktop.pump()
        return result
    except BaseException:
        await drain(task, desktop)
        raise


async def exercise(computer_use: Any, desktop: ScratchDesktop, attachments: Attachments,
                   image_module: Any, evidence: dict[str, Any]) -> None:
    async def batch(actions: list[dict[str, Any]]) -> dict[str, Any]:
        desktop.validate_actions(actions)
        before = len(attachments.emissions)
        result = await guarded(computer_use.run(actions, space="screen"), desktop)
        require(isinstance(result, dict), "run() must return screenshot metadata")
        require(result.get("actions_completed") == len(actions), "Batch did not complete every action")
        decoded = attachments.decode_since(before, image_module)
        require(decoded[-1]["size"] == result["preview_size"], "Incorrect emitted preview dimensions")
        evidence["batches"].append({"actions": actions, "result": result, "attachments": decoded})
        return result

    async def screenshot(label: str) -> None:
        before = len(attachments.emissions)
        result = await guarded(computer_use.screenshot(), desktop)
        require(isinstance(result, dict), "screenshot() must return metadata")
        decoded = attachments.decode_since(before, image_module)
        require(decoded[-1]["size"] == result["preview_size"], "Incorrect emitted preview dimensions")
        source = Path(result["path"])
        with image_module.open(source) as image:
            image.load()
            require(tuple(image.size) == tuple(desktop.backend.size()),
                    "Screenshot size differs from native primary coordinates (possible DPI mismatch)")
            require(tuple(result["raw_size"]) == tuple(image.size), "Incorrect raw screenshot dimensions")
            x, y = desktop.coordinate(desktop.marker)
            pixel = image.convert("RGB").getpixel((x, y))
            require(all(abs(actual - expected) <= 12 for actual, expected in zip(pixel, MARKER_COLOR)),
                    "Real screenshot does not contain the scratch marker at native coordinates")
            image.crop(desktop.client_bounds()).save(ARTIFACTS / f"{label}-scratch.png")
        raw_copy = ARTIFACTS / f"{label}-desktop{source.suffix}"
        if source.resolve() != raw_copy.resolve():
            shutil.copyfile(source, raw_copy)
        evidence["screenshots"].append({"label": label, "result": result,
                                        "raw_copy": str(raw_copy), "attachments": decoded})
        print(f"PASS {label}: genuine screenshot decoded; scratch marker matches native coordinates", flush=True)

    # Tk's update() can precede the native maximize/foreground animation.
    # Do not send any input until Windows presents the initial scratch frame.
    until = time.monotonic() + 0.5
    while time.monotonic() < until:
        desktop.pump(check=False)  # No input yet; let native show/focus transitions settle.
        await asyncio.sleep(0.01)
    desktop.pump()  # Foreground and fail-safe must hold before the first capture/input.
    await screenshot("before")
    before_click = desktop.counts["button_commands"]
    x, y = desktop.coordinate(desktop.button)
    await batch([{"type": "click", "x": x, "y": y}])
    require(desktop.counts["button_commands"] == before_click + 1,
            "Scratch button did not receive the native click")
    print("PASS click: scratch button command received", flush=True)

    x, y = desktop.coordinate(desktop.textbox)
    await batch([{"type": "click", "x": x, "y": y}])
    require(desktop.root.focus_get() == desktop.textbox, "Scratch textbox did not receive focus")
    await batch([{"type": "type", "text": ASCII_TEXT}])
    require(desktop.text() == ASCII_TEXT, "Scratch textbox did not receive exact ASCII typing")
    before_keys = desktop.counts["key_down"]
    await batch([{"type": "press", "key": "backspace"}])
    require(desktop.text() == ASCII_TEXT[:-1], "Native BackSpace did not mutate the scratch textbox")
    require(desktop.counts["key_down"] > before_keys, "Scratch did not receive a native key event")
    print("PASS typing/key: exact ASCII text and actual BackSpace mutation", flush=True)

    x, y = desktop.coordinate(desktop.scrollbox)
    await batch([{"type": "click", "x": x, "y": y}])
    require(desktop.root.focus_get() == desktop.scrollbox, "Scratch wheel target did not receive focus")
    before_wheel = desktop.counts["wheel"]
    before_view = desktop.scrollbox.yview()
    await batch([{"type": "scroll", "clicks": -3}])
    require(desktop.counts["wheel"] > before_wheel, "Scratch did not receive native wheel input")
    wheel_events = [event for event in desktop.events if event["event"] == "wheel"]
    require(wheel_events[-1]["delta"] == -360, "Three wheel notches must produce native delta -360")
    require(desktop.scrollbox.yview()[0] > before_view[0], "Scratch scroll view did not actually move")
    print("PASS scroll: native wheel event and actual scratch view movement", flush=True)

    # Cancel only after an actual click is received: the batch is genuinely active.
    # The two-second wait gives a deterministic cancellation point, and the final
    # click exposes an orphan worker that continues after cancellation settles.
    x, y = desktop.coordinate(desktop.button)
    actions = [{"type": "click", "x": x, "y": y}, {"type": "wait", "seconds": 2.0},
               {"type": "click", "x": x, "y": y}]
    desktop.validate_actions(actions)
    before_click = desktop.counts["button_commands"]
    task = asyncio.create_task(computer_use.run(actions, space="screen"))
    cancelled_at = None
    deadline = time.monotonic() + 12.0
    try:
        while not task.done():
            desktop.pump()
            require(time.monotonic() < deadline, "Cancellation did not cooperatively settle")
            if cancelled_at is None and desktop.counts["button_commands"] == before_click + 1:
                cancelled_at = time.monotonic()
                task.cancel()
            await asyncio.sleep(0.01)
        require(cancelled_at is not None, "Batch finished without an observed mid-batch cancellation")
        try:
            task.result()
        except asyncio.CancelledError:
            pass
        else:
            raise AssertionError("Cancelled batch did not re-raise CancelledError after drain")
        settled_at = time.monotonic()
        snapshot = dict(desktop.counts)
        require(snapshot["button_commands"] == before_click + 1, "Cancellation allowed the final click")
        # Longer than the cancelled wait: catch delayed input from an orphan worker.
        quiet_until = settled_at + 2.5
        while time.monotonic() < quiet_until:
            desktop.pump()
            require(desktop.counts == snapshot, "Input arrived after cancellation settled")
            await asyncio.sleep(0.01)
        evidence["cancellation"] = {"actions": actions, "drain_seconds": settled_at - cancelled_at,
                                    "quiet_seconds": 2.5, "settled_counts": snapshot,
                                    "final_counts": dict(desktop.counts), "cancelled_error": True}
    except BaseException:
        await drain(task, desktop)
        raise
    print("PASS cancellation: mid-batch CancelledError/drain; no input for 2.5s after settlement", flush=True)
    await screenshot("after")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--run", action="store_true", help="explicitly allow real Windows desktop input and local screenshots")
    args = parser.parse_args()
    if not args.run:
        print("SKIP: real desktop input is opt-in. Review this script, then pass --run on native Windows.")
        return 0
    if sys.platform != "win32":
        print("ERROR: --run requires native Windows Python and an interactive desktop.", file=sys.stderr)
        return 2
    print("WARNING: real desktop input + full-primary screenshots; do not interact or switch windows.", flush=True)
    print(f"Evidence (local only): {ARTIFACTS}", flush=True)
    try:
        import tkinter as tk
    except ImportError:
        print("ERROR: this native Python has no Tkinter; use a Windows Python build with Tcl/Tk.", file=sys.stderr)
        return 2
    try:
        # Import before any HWND: PyAutoGUI establishes its native DPI behavior.
        # Never modify FAILSAFE, FAILSAFE_POINTS, PAUSE, or backend implementations.
        import pyautogui
        from PIL import Image
    except ImportError as error:
        print(f"ERROR: missing native skill dependency: {error}. Prepare dependencies outside this script.", file=sys.stderr)
        return 2

    desktop = None
    imported = None
    attachments = Attachments()
    evidence: dict[str, Any] = {"status": "failed", "python": sys.executable,
                                "platform": sys.platform, "batches": [], "screenshots": []}
    previous_cwd = Path.cwd()
    started = time.monotonic()
    try:
        os.chdir(REPO)  # Keep the skill's own screenshot files local to this repo.
        ARTIFACTS.mkdir(parents=True, exist_ok=True)
        require(pyautogui.FAILSAFE is True, "PyAutoGUI fail-safe is disabled; refusing to run")
        computer_use, previous_rlm, added = import_skills(attachments)
        imported = (previous_rlm, added)
        desktop = ScratchDesktop(tk, pyautogui)
        desktop.pump()
        require(desktop.root.winfo_width() >= 640 and desktop.root.winfo_height() >= 480,
                "Interactive desktop is too small for safe scratch targets")
        evidence["scratch_bounds"] = list(desktop.client_bounds())
        asyncio.run(exercise(computer_use, desktop, attachments, Image, evidence))
        evidence["status"] = "passed"
        print("PASS computer_use genuine Windows desktop smoke (click/type/key/scroll/screenshot/cancel)", flush=True)
        return 0
    except (Exception, KeyboardInterrupt) as error:
        evidence["error"] = f"{type(error).__name__}: {error}"
        print(f"FAIL computer_use smoke: {evidence['error']}", file=sys.stderr, flush=True)
        return 1
    finally:
        # asyncio.run also joins its native executor before this finally destroys
        # the scratch window, including on Ctrl+C or a failed assertion.
        if desktop is not None:
            evidence["counts"] = dict(desktop.counts)
            evidence["events"] = desktop.events
            evidence["textbox"] = desktop.text()
            desktop.destroy()
        if imported is not None:
            restore_imports(*imported)
        evidence["elapsed_seconds"] = time.monotonic() - started
        evidence["attachments"] = attachments.manifest
        evidence["host_requests"] = attachments.host_requests
        try:
            ARTIFACTS.mkdir(parents=True, exist_ok=True)
            (ARTIFACTS / "summary.json").write_text(json.dumps(evidence, indent=2, default=str) + "\n", encoding="utf-8")
            print(f"Summary: {ARTIFACTS / 'summary.json'}", flush=True)
        finally:
            os.chdir(previous_cwd)


if __name__ == "__main__":
    raise SystemExit(main())
