"""Bounded Windows desktop input. Importing this module never imports PyAutoGUI."""

from __future__ import annotations

import asyncio
import base64
import importlib
import io
import math
import sys
import threading
import time
import uuid
from contextlib import contextmanager
from pathlib import Path
from typing import Any

_MAX_ACTIONS = 32
_MAX_REPEATS = 20
_MAX_TEXT = 500
_MAX_CHORD_KEYS = 8
_MAX_WAIT = 2.0
_MAX_BATCH_SECONDS = 10.0
_MAX_COORDINATE = 1_000_000
_MAX_SCREEN_PIXELS = 36_000_000  # attach_image's source pixel limit
_EVENT_INTERVAL = 0.005
_ESTIMATED_CALL_SECONDS = 0.005
_MUTEX_NAME = r"Local\PrimeAgent.ComputerUse.PrimaryDesktop.v1"
_SHIFTED_BASES = dict(zip('~!@#$%^&*()_+{}|:"<>?', '`1234567890-=[]\\;\',./'))
_TEXT_CONTROLS = {"\n": "enter", "\t": "tab"}
_DRAG_STEPS = 12
_KEEP_CAPTURES = 100
_MOUSEEVENTF_HWHEEL = 0x01000
_COORDINATE_SPACES = ("preview", "screen")

# The last full-screen view the model was shown: its raw and preview sizes.
# `space="preview"` coordinates are read against it, the way computer-use
# models act in the coordinates of the screenshot they saw.
_LAST_VIEW: dict | None = None


class _WorkerStopped(Exception):
    """Private cooperative-cancellation signal, never a new input request."""


def _number(value: Any, name: str, maximum: float) -> int | float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError(f"{name} must be a finite number")
    if isinstance(value, float) and not math.isfinite(value):
        raise ValueError(f"{name} must be finite")
    if not 0 <= value <= maximum:
        raise ValueError(f"{name} must be between 0 and {maximum:g}")
    return value


def _count(value: Any, name: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not 1 <= value <= _MAX_REPEATS:
        raise ValueError(f"{name} must be an integer between 1 and {_MAX_REPEATS}")
    return value


def _key_name(value: Any) -> str:
    if not isinstance(value, str) or not value or len(value) > 32:
        raise ValueError("key must be a supported key name or one ASCII character")
    if not value.isascii():
        raise ValueError("Unicode keys are unsupported; use supported ASCII keys only")
    if len(value) == 1 and not (32 <= ord(value) <= 126 or value in _TEXT_CONTROLS):
        raise ValueError("unsupported ASCII control character in key")
    return value.lower() if len(value) > 1 else value


def _static_key_length(key: str) -> int:
    if len(key) == 1 and not key.isascii():
        return 1  # One Unicode down/up pair (two for a surrogate pair, same cost).
    return 2 if len(key) == 1 and (key.isupper() or key in _SHIFTED_BASES) else 1


def _typable(char: str) -> bool:
    """Printable text, newline and tab; never another control or a lone surrogate."""
    if char in _TEXT_CONTROLS:
        return True
    code = ord(char)
    if code < 32 or 0x7F <= code < 0xA0 or 0xD800 <= code <= 0xDFFF:
        return False
    return True


def _estimate(actions: list[dict], *, compiled: bool = False) -> float:
    total = 0.0
    for action in actions:
        kind = action["type"]
        if kind == "wait":
            total += action["seconds"]
            continue
        if kind in {"press", "type", "hotkey"}:
            if compiled:
                lengths = [len(chord) for chord in action["_chords"]]
            elif kind == "press":
                lengths = [_static_key_length(action["key"])]
            elif kind == "type":
                lengths = [_static_key_length(char) for char in action["text"]]
            else:
                lengths = [sum(_static_key_length(key) for key in action["keys"])]
            cost = sum(2 * length * _ESTIMATED_CALL_SECONDS + _EVENT_INTERVAL for length in lengths)
            total += cost * action.get("presses", 1)
        elif kind == "click":
            total += action["clicks"] * (2 * _ESTIMATED_CALL_SECONDS + _EVENT_INTERVAL)
            if "x" in action:
                total += _ESTIMATED_CALL_SECONDS
        elif kind == "drag":
            total += (_DRAG_STEPS + 3) * (_ESTIMATED_CALL_SECONDS + _EVENT_INTERVAL)
        else:
            total += _ESTIMATED_CALL_SECONDS + _EVENT_INTERVAL
            if kind == "scroll" and "x" in action:
                total += _ESTIMATED_CALL_SECONDS
    return total


def _validate_actions(actions: list[dict]) -> list[dict]:
    """Copy and validate the whole schema before awaiting anything or sending input."""
    if not isinstance(actions, list):
        raise ValueError("actions must be a list of dictionaries")
    if len(actions) > _MAX_ACTIONS:
        raise ValueError(f"a batch may contain at most {_MAX_ACTIONS} actions")
    schemas = {
        "move": ({"type", "x", "y"}, {"x", "y"}),
        "click": ({"type", "x", "y", "button", "clicks"}, set()),
        "drag": ({"type", "x", "y", "to_x", "to_y", "button"}, {"x", "y", "to_x", "to_y"}),
        "scroll": ({"type", "x", "y", "clicks", "horizontal"}, {"clicks"}),
        "press": ({"type", "key", "presses"}, {"key"}),
        "hotkey": ({"type", "keys"}, {"keys"}),
        "type": ({"type", "text"}, {"text"}),
        "wait": ({"type", "seconds"}, {"seconds"}),
    }
    validated = []
    for index, original in enumerate(actions):
        if not isinstance(original, dict):
            raise ValueError(f"action {index} must be a dictionary")
        kind = original.get("type")
        if not isinstance(kind, str) or kind not in schemas:
            raise ValueError(f"action {index} has an unsupported type")
        allowed, required = schemas[kind]
        if set(original) - allowed or not required <= set(original):
            raise ValueError(f"action {index} has unknown or missing fields for {kind}")
        action = dict(original)
        if "x" in action or "y" in action:
            if "x" not in action or "y" not in action:
                raise ValueError(f"action {index} requires both x and y")
            action["x"] = _number(action["x"], "x", _MAX_COORDINATE)
            action["y"] = _number(action["y"], "y", _MAX_COORDINATE)
        if kind in ("click", "drag"):
            button = action.get("button", "left")
            if button not in ("left", "middle", "right"):
                raise ValueError("button must be left, middle, or right")
            action["button"] = button
        if kind == "click":
            action["clicks"] = _count(action.get("clicks", 1), "clicks")
        elif kind == "drag":
            action["to_x"] = _number(action["to_x"], "to_x", _MAX_COORDINATE)
            action["to_y"] = _number(action["to_y"], "to_y", _MAX_COORDINATE)
        elif kind == "scroll":
            clicks = action["clicks"]
            if isinstance(clicks, bool) or not isinstance(clicks, int) or not -_MAX_REPEATS <= clicks <= _MAX_REPEATS:
                raise ValueError(f"scroll clicks must be an integer between -{_MAX_REPEATS} and {_MAX_REPEATS}")
            horizontal = action.get("horizontal", False)
            if not isinstance(horizontal, bool):
                raise ValueError("scroll horizontal must be true or false")
            action["horizontal"] = horizontal
        elif kind == "press":
            action["key"] = _key_name(action["key"])
            action["presses"] = _count(action.get("presses", 1), "presses")
        elif kind == "hotkey":
            keys = action["keys"]
            if not isinstance(keys, list) or not 1 <= len(keys) <= _MAX_CHORD_KEYS:
                raise ValueError(f"hotkey keys must be a list of 1-{_MAX_CHORD_KEYS} keys")
            action["keys"] = [_key_name(key) for key in keys]
            if len(set(action["keys"])) != len(action["keys"]):
                raise ValueError("hotkey keys must be distinct")
        elif kind == "type":
            text = action["text"]
            if not isinstance(text, str) or len(text) > _MAX_TEXT:
                raise ValueError(f"text must be a string of at most {_MAX_TEXT} characters")
            if not all(_typable(char) for char in text):
                raise ValueError("text supports printable characters (any language), newline, and tab; other control characters are rejected")
        elif kind == "wait":
            action["seconds"] = _number(action["seconds"], "seconds", _MAX_WAIT)
        validated.append(action)
    if _estimate(validated) > _MAX_BATCH_SECONDS + 1e-9:
        raise ValueError(f"estimated batch duration exceeds {_MAX_BATCH_SECONDS:g} seconds")
    return validated


async def _preflight_vision() -> None:
    try:
        from rlm import host_request
    except ImportError as error:
        raise RuntimeError("computer_use must run in the Prime Agent Python kernel (rlm runtime missing)") from error
    info = await host_request("model.info")
    if "image" not in info.get("input", []):
        model = info.get("id") or "the current model"
        raise RuntimeError(f"{model} does not support vision; switch to a vision-capable model before computer use")
    # Fail on missing attachment dependencies before any desktop inputs, not after.
    try:
        importlib.import_module("attach_image.attach_image")
        importlib.import_module("PIL.Image")
    except ImportError as error:
        raise RuntimeError("computer_use needs the attach-image skill and Pillow installed in the kernel") from error


def _check_interactive_desktop() -> None:
    """Reject services, headless sessions, and a different/secure input desktop."""
    import ctypes
    from ctypes import wintypes

    user32 = ctypes.WinDLL("user32", use_last_error=True)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    user32.GetProcessWindowStation.restype = wintypes.HANDLE
    user32.GetUserObjectInformationW.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD, ctypes.POINTER(wintypes.DWORD)]
    user32.GetUserObjectInformationW.restype = wintypes.BOOL
    user32.OpenInputDesktop.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    user32.OpenInputDesktop.restype = wintypes.HANDLE
    user32.CloseDesktop.argtypes = [wintypes.HANDLE]
    user32.CloseDesktop.restype = wintypes.BOOL
    user32.GetThreadDesktop.argtypes = [wintypes.DWORD]
    user32.GetThreadDesktop.restype = wintypes.HANDLE
    kernel32.GetCurrentThreadId.restype = wintypes.DWORD

    class UserObjectFlags(ctypes.Structure):
        _fields_ = [("inherit", wintypes.BOOL), ("reserved", wintypes.BOOL), ("flags", wintypes.DWORD)]

    flags = UserObjectFlags()
    needed = wintypes.DWORD()
    station = user32.GetProcessWindowStation()
    if not station or not user32.GetUserObjectInformationW(station, 1, ctypes.byref(flags), ctypes.sizeof(flags), ctypes.byref(needed)) or not flags.flags & 1:
        raise RuntimeError("computer_use needs a visible, logged-in Windows desktop; headless/service sessions are unsupported")
    desktop = user32.OpenInputDesktop(0, False, 0x0001)  # DESKTOP_READOBJECTS only
    if not desktop:
        raise RuntimeError("computer_use cannot access the input desktop; log in and unlock the Windows desktop")
    try:
        def name(handle: Any) -> str:
            buffer = ctypes.create_unicode_buffer(256)
            if not handle or not user32.GetUserObjectInformationW(handle, 2, buffer, ctypes.sizeof(buffer), ctypes.byref(needed)):
                raise RuntimeError("computer_use cannot inspect the Windows input desktop")
            return buffer.value

        current = user32.GetThreadDesktop(kernel32.GetCurrentThreadId())
        if name(desktop).lower() != "default" or name(current).lower() != "default":
            raise RuntimeError("computer_use supports only the logged-in Default desktop, not locked, secure, or alternate desktops")
    finally:
        user32.CloseDesktop(desktop)


def _load_pyautogui():
    if sys.platform != "win32":
        raise RuntimeError("computer_use supports Windows only, on the primary logged-in desktop; this platform is unsupported")
    _check_interactive_desktop()
    try:
        pg = importlib.import_module("pyautogui")
        importlib.import_module("PIL.Image")
    except ImportError as error:
        raise RuntimeError("computer_use needs PyAutoGUI==0.9.54 and Pillow installed in the kernel; skill dependencies are not auto-installed") from error
    except Exception as error:
        raise RuntimeError("computer_use could not initialize PyAutoGUI; a logged-in, unlocked Windows desktop is required") from error
    if getattr(pg, "__version__", None) != "0.9.54":
        raise RuntimeError("computer_use requires exactly PyAutoGUI==0.9.54; install that version in the kernel")
    return pg


@contextmanager
def _desktop_mutex():
    """Session-local, cross-process exclusion, acquired/released on this worker thread."""
    if sys.platform != "win32":
        raise RuntimeError("computer_use supports Windows only; this platform is unsupported")
    import ctypes
    from ctypes import wintypes

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.CreateMutexW.argtypes = [ctypes.c_void_p, wintypes.BOOL, wintypes.LPCWSTR]
    kernel32.CreateMutexW.restype = wintypes.HANDLE
    kernel32.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    kernel32.WaitForSingleObject.restype = wintypes.DWORD
    kernel32.ReleaseMutex.argtypes = [wintypes.HANDLE]
    kernel32.ReleaseMutex.restype = wintypes.BOOL
    kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel32.CloseHandle.restype = wintypes.BOOL
    handle = kernel32.CreateMutexW(None, False, _MUTEX_NAME)
    if not handle:
        raise RuntimeError(f"computer_use cannot create the desktop mutex: {ctypes.WinError(ctypes.get_last_error())}")
    acquired = False
    try:
        status = kernel32.WaitForSingleObject(handle, 0)  # Never wait on a busy controller.
        if status == 0x102:
            raise RuntimeError("computer_use desktop is busy: another controller is running a batch or screenshot")
        if status not in (0, 0x80):  # WAIT_OBJECT_0 / WAIT_ABANDONED
            raise RuntimeError(f"computer_use cannot acquire the desktop mutex: {ctypes.WinError(ctypes.get_last_error())}")
        acquired = True
        yield
    finally:
        try:
            if acquired and not kernel32.ReleaseMutex(handle):
                raise RuntimeError(f"computer_use cannot release the desktop mutex: {ctypes.WinError(ctypes.get_last_error())}")
        finally:
            kernel32.CloseHandle(handle)


def _mapping(pg) -> dict:
    mapping = getattr(getattr(pg, "platformModule", None), "keyboardMapping", None)
    if not isinstance(mapping, dict):
        raise RuntimeError("computer_use cannot inspect the Windows keyboard mapping")
    return mapping


def _plain_mapping(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool) and 1 <= value <= 0xFF


def _implicit_shift(key: str) -> bool:
    return len(key) == 1 and (key.isupper() or key in _SHIFTED_BASES)


def _plan_key(pg, key: str) -> list[str]:
    """Expand ASCII to explicit modifiers + an unmodified backend key.

    The Windows backend otherwise adds implicit modifier down/up events. Avoid
    those so *every* potentially held key is tracked, including on exceptions.
    Reject absent/-1 mappings rather than PyAutoGUI's silent no-op behavior.
    """
    mapping = _mapping(pg)
    key = _TEXT_CONTROLS.get(key, key)
    value = mapping.get(key)
    if len(key) > 1:
        if not _plain_mapping(value):
            raise ValueError(f"unsupported Windows key: {key!r}")
        return [key]
    # Uppercase can always be expressed with the mapped lowercase key. This also
    # works with backends whose exposed key-name list omits uppercase letters.
    if value is None and key.isupper() and _plain_mapping(mapping.get(key.lower())):
        value = mapping[key.lower()] | 0x100
    if not isinstance(value, int) or isinstance(value, bool) or value < 0 or value > 0x7FF:
        raise ValueError(f"ASCII character {key!r} is unsupported by the current Windows keyboard layout")
    mods, virtual_key = divmod(value, 0x100)
    if not mods and not _implicit_shift(key):
        return [key]
    # Prefer printable unshifted keys, then named keys, with the exact same VK.
    candidates = list("abcdefghijklmnopqrstuvwxyz0123456789`-=[]\\;',./ ")
    candidates += [name for name in mapping if isinstance(name, str) and len(name) > 1]
    base = next((name for name in candidates if mapping.get(name) == virtual_key and not _implicit_shift(name)), None)
    if base is None:
        raise ValueError(f"ASCII character {key!r} cannot be safely typed on the current keyboard layout")
    modifiers = [name for bit, name in ((2, "ctrl"), (4, "alt"), (1, "shift")) if mods & bit]
    if any(not _plain_mapping(mapping.get(name)) for name in modifiers):
        raise ValueError(f"ASCII character {key!r} needs an unsupported modifier")
    return modifiers + [base]


def _text_chord(pg, char: str) -> list[str] | tuple[str, str]:
    """Keystrokes for ASCII the layout types safely; Unicode injection otherwise.

    Keystrokes keep what games and shortcuts listen for. Text the layout cannot
    produce - Vietnamese, CJK, an `@` behind an unsafe mapping - arrives as
    Unicode input (`KEYEVENTF_UNICODE`), which text fields read as characters.
    """
    if char.isascii():
        try:
            return _plan_key(pg, char)
        except ValueError:
            if char in _TEXT_CONTROLS:
                raise
    return ("unicode", char)


def _compile_actions(pg, actions: list[dict], size: tuple[int, int]) -> list[dict]:
    compiled = []
    for original in actions:
        action = dict(original)
        for x_key, y_key in (("x", "y"), ("to_x", "to_y")):
            if x_key in action:
                if action[x_key] >= size[0] or action[y_key] >= size[1]:
                    raise ValueError(f"coordinates must be inside the primary screen, 0 <= x < {size[0]}, 0 <= y < {size[1]}")
                action[x_key], action[y_key] = int(action[x_key]), int(action[y_key])
        kind = action["type"]
        if kind == "press":
            action["_chords"] = [_plan_key(pg, action["key"])]
        elif kind == "type":
            action["_chords"] = [_text_chord(pg, char) for char in action["text"]]
        elif kind == "hotkey":
            chord = [part for key in action["keys"] for part in _plan_key(pg, key)]
            physical_keys = [_mapping(pg)[part] for part in chord]
            if len(chord) > _MAX_CHORD_KEYS or len(set(physical_keys)) != len(physical_keys):
                raise ValueError(f"hotkey must expand to at most {_MAX_CHORD_KEYS} distinct physical keys; use lowercase letters with explicit modifiers")
            action["_chords"] = [chord]
        compiled.append(action)
    if _estimate(compiled, compiled=True) > _MAX_BATCH_SECONDS + 1e-9:
        raise ValueError(f"estimated batch duration exceeds {_MAX_BATCH_SECONDS:g} seconds with this keyboard layout")
    return compiled


def _screen_size(pg) -> tuple[int, int]:
    try:
        size = tuple(pg.size())
    except Exception as error:
        raise RuntimeError("computer_use cannot read the primary Windows screen size; a visible logged-in desktop is required") from error
    if len(size) != 2 or any(isinstance(value, bool) or not isinstance(value, int) or not 0 < value <= _MAX_COORDINATE for value in size):
        raise RuntimeError("computer_use received an invalid primary screen size")
    if size[0] * size[1] > _MAX_SCREEN_PIXELS:
        raise RuntimeError("computer_use primary screen exceeds the attach-image 36-megapixel limit")
    return size


def _capture(pg, expected_size: tuple[int, int] | None = None):
    size = _screen_size(pg)
    if expected_size is not None and size != expected_size:
        raise RuntimeError("computer_use primary screen size changed during the batch; take a new screenshot")
    try:
        image = pg.screenshot()  # PyScreeze defaults to the primary monitor on Windows.
    except Exception as error:
        raise RuntimeError("computer_use could not capture the desktop; check Pillow/PyScreeze and the logged-in, unlocked Windows desktop") from error
    if tuple(image.size) != size or _screen_size(pg) != size:
        image.close()
        raise RuntimeError("computer_use screenshot/primary screen DPI or coordinate mismatch; fix Windows DPI scaling and restart the kernel before input")
    return image, size


def _require_failsafe(pg) -> None:
    if getattr(pg, "FAILSAFE", None) is not True:
        raise RuntimeError("computer_use requires PyAutoGUI.FAILSAFE=True; never disable the corner emergency stop")


def _cancel_check(stop: threading.Event) -> None:
    if stop.is_set():
        raise _WorkerStopped("computer_use batch cancelled")


def _checkpoint(pg, stop: threading.Event, deadline: float) -> None:
    _cancel_check(stop)
    if time.monotonic() >= deadline:
        raise TimeoutError("computer_use batch reached its 10-second input deadline")
    _require_failsafe(pg)
    pg.failSafeCheck()


def _pace(stop: threading.Event) -> None:
    if stop.wait(_EVENT_INTERVAL):
        raise _WorkerStopped("computer_use batch cancelled")


class _HeldInputs:
    def __init__(self, pg):
        self.pg = pg
        self.keys: list[str] = []
        self.buttons: list[tuple[str, int, int]] = []

    def cleanup(self) -> None:
        """Only release attempted skill-owned inputs; no FAILSAFE/global changes.

        All tracked keys are unmodified backend keys: _keyUp cannot inject
        implicit modifier presses. _mouseUp has no movement/down flags. This is
        the sole narrow fail-safe bypass, and runs before releasing the mutex.
        """
        failures = []
        backend = self.pg.platformModule
        for key in list(reversed(self.keys)):
            try:
                backend._keyUp(key)
                self.keys.remove(key)
            except Exception as error:
                failures.append(error)
        for button, x, y in list(reversed(self.buttons)):
            try:
                backend._mouseUp(x, y, button)
                self.buttons.remove((button, x, y))
            except Exception as error:
                failures.append(error)
        if failures:
            raise RuntimeError("computer_use could not release all skill-held inputs; manually release keys/buttons before continuing") from failures[0]


def _send_unicode(char: str) -> None:
    """Type one character with `SendInput` and `KEYEVENTF_UNICODE`.

    PyAutoGUI drops a character its layout table lacks without a word. Every
    UTF-16 unit goes down and up in the same `SendInput` call, so nothing is
    left held if the batch stops between characters.
    """
    import ctypes
    from ctypes import wintypes

    keyeventf_keyup, keyeventf_unicode, input_keyboard = 0x0002, 0x0004, 1

    class KEYBDINPUT(ctypes.Structure):
        _fields_ = [("wVk", wintypes.WORD), ("wScan", wintypes.WORD), ("dwFlags", wintypes.DWORD),
                    ("time", wintypes.DWORD), ("dwExtraInfo", ctypes.c_size_t)]

    class MOUSEINPUT(ctypes.Structure):
        _fields_ = [("dx", wintypes.LONG), ("dy", wintypes.LONG), ("mouseData", wintypes.DWORD),
                    ("dwFlags", wintypes.DWORD), ("time", wintypes.DWORD), ("dwExtraInfo", ctypes.c_size_t)]

    class _UNION(ctypes.Union):
        _fields_ = [("ki", KEYBDINPUT), ("mi", MOUSEINPUT)]

    class INPUT(ctypes.Structure):
        _fields_ = [("type", wintypes.DWORD), ("union", _UNION)]

    data = char.encode("utf-16-le")
    units = [int.from_bytes(data[index:index + 2], "little") for index in range(0, len(data), 2)]
    events = []
    for unit in units:
        for flags in (keyeventf_unicode, keyeventf_unicode | keyeventf_keyup):
            event = INPUT(type=input_keyboard)
            event.union.ki = KEYBDINPUT(0, unit, flags, 0, 0)
            events.append(event)
    array = (INPUT * len(events))(*events)
    user32 = ctypes.WinDLL("user32", use_last_error=True)
    user32.SendInput.argtypes = [wintypes.UINT, ctypes.c_void_p, ctypes.c_int]
    user32.SendInput.restype = wintypes.UINT
    sent = user32.SendInput(len(events), array, ctypes.sizeof(INPUT))
    if sent != len(events):
        raise RuntimeError(f"computer_use could not type {char!r}: {ctypes.WinError(ctypes.get_last_error())}")


def _press_chord(pg, chord, held: _HeldInputs, stop: threading.Event, deadline: float) -> None:
    if isinstance(chord, tuple):
        _checkpoint(pg, stop, deadline)
        _send_unicode(chord[1])
        _pace(stop)
        return
    try:
        for key in chord:
            _checkpoint(pg, stop, deadline)
            # Track before calling: a backend may raise after sending key-down.
            held.keys.append(key)
            pg.keyDown(key, _pause=False)
        for key in reversed(chord):
            _checkpoint(pg, stop, deadline)
            pg.keyUp(key, _pause=False)
            held.keys.remove(key)
    finally:
        if held.keys:
            held.cleanup()
    _pace(stop)


def _pointer(pg, size: tuple[int, int]) -> tuple[int, int]:
    point = tuple(pg.position())
    if len(point) != 2 or not (0 <= point[0] < size[0] and 0 <= point[1] < size[1]):
        raise RuntimeError("computer_use pointer is outside the primary screen; move it inside before clicking/scrolling")
    return int(point[0]), int(point[1])


def _drag(pg, action: dict, held: _HeldInputs, stop: threading.Event, deadline: float, size: tuple[int, int]) -> None:
    """Press at (x, y), move to (to_x, to_y) in steps, release.

    The steps are what sliders, canvases and drag-and-drop listen for; a jump
    straight to the target is a click somewhere else to many of them. The
    button is tracked, so an error or a cancellation midway releases it.
    """
    start, end, button = (action["x"], action["y"]), (action["to_x"], action["to_y"]), action["button"]
    pg.moveTo(*start, _pause=False)
    _checkpoint(pg, stop, deadline)
    x, y = _pointer(pg, size)
    try:
        held.buttons.append((button, x, y))
        pg.mouseDown(button=button, _pause=False)
        for step in range(1, _DRAG_STEPS + 1):
            _checkpoint(pg, stop, deadline)
            pg.moveTo(
                round(start[0] + (end[0] - start[0]) * step / _DRAG_STEPS),
                round(start[1] + (end[1] - start[1]) * step / _DRAG_STEPS),
                _pause=False,
            )
            _pace(stop)
        _checkpoint(pg, stop, deadline)
        pg.mouseUp(button=button, _pause=False)
        held.buttons.remove((button, x, y))
    finally:
        if held.buttons:
            held.cleanup()
    _pace(stop)


def _run_action(pg, action: dict, held: _HeldInputs, stop: threading.Event, deadline: float, size: tuple[int, int]) -> None:
    _checkpoint(pg, stop, deadline)
    if _screen_size(pg) != size:
        raise RuntimeError("computer_use primary screen size changed during the batch; take a new screenshot")
    kind = action["type"]
    if kind == "wait":
        if stop.wait(action["seconds"]):
            raise _WorkerStopped("computer_use batch cancelled")
    elif kind == "drag":
        _drag(pg, action, held, stop, deadline, size)
    elif kind in {"move", "click", "scroll"}:
        if "x" in action:
            pg.moveTo(action["x"], action["y"], _pause=False)
        if kind == "move":
            _pace(stop)
        elif kind == "scroll":
            _checkpoint(pg, stop, deadline)
            x, y = _pointer(pg, size)
            if action.get("horizontal"):
                # PyAutoGUI's hscroll sends the vertical wheel on Windows;
                # the horizontal wheel is its own event. Positive is right.
                pg.platformModule._sendMouseEvent(_MOUSEEVENTF_HWHEEL, x, y, dwData=action["clicks"] * 120)
            else:
                pg.scroll(action["clicks"] * 120, _pause=False)  # Windows WHEEL_DELTA per notch.
            _pace(stop)
        else:
            for _ in range(action["clicks"]):
                _checkpoint(pg, stop, deadline)
                x, y = _pointer(pg, size)
                button = action["button"]
                try:
                    held.buttons.append((button, x, y))
                    pg.mouseDown(button=button, _pause=False)
                    _checkpoint(pg, stop, deadline)
                    pg.mouseUp(button=button, _pause=False)
                    held.buttons.remove((button, x, y))
                finally:
                    if held.buttons:
                        held.cleanup()
                _pace(stop)
    else:
        for _ in range(action.get("presses", 1)):
            for chord in action["_chords"]:
                _press_chord(pg, chord, held, stop, deadline)


def _prune_captures(folder: Path) -> None:
    """Keep the newest captures; a long session wrote one per batch, forever."""
    try:
        captures = sorted(folder.glob("screen-*.png"), key=lambda path: path.stat().st_mtime, reverse=True)
    except OSError:
        return
    for stale in captures[_KEEP_CAPTURES:]:
        try:
            stale.unlink()
        except OSError:
            pass


def _worker(
    actions: list[dict],
    stop: threading.Event,
    expected_size: tuple[int, int] | None = None,
    region: tuple[int, int, int, int] | None = None,
) -> dict:
    # The mutex must cover pre-input capture, all events, emergency cleanup, and
    # the after-capture. Windows mutex ownership is thread-bound, so do not put
    # acquire/release in the async caller or in separate to_thread calls.
    with _desktop_mutex():
        _cancel_check(stop)
        pg = _load_pyautogui()
        _cancel_check(stop)
        if actions:
            _require_failsafe(pg)
        # Coordinates read from an earlier screenshot are only right on the
        # screen that screenshot showed.
        image, size = _capture(pg, expected_size)
        held = _HeldInputs(pg)
        completed = 0
        try:
            compiled = _compile_actions(pg, actions, size)  # Validate EVERY key/coordinate before ANY input.
            if compiled:
                image.close()
                image = None
                deadline = time.monotonic() + _MAX_BATCH_SECONDS
                for action in compiled:
                    _run_action(pg, action, held, stop, deadline, size)
                    completed += 1
                _cancel_check(stop)
                image, size = _capture(pg, size)
            _cancel_check(stop)
            if region is not None:
                left, top, width, height = region
                if left + width > size[0] or top + height > size[1]:
                    raise ValueError(f"zoom region must be inside the primary screen {size[0]}x{size[1]}")
                cropped = image.crop((left, top, left + width, top + height))
                image.close()
                image = cropped
            folder = Path.cwd() / ".harness" / "artifacts" / "computer-use"
            folder.mkdir(parents=True, exist_ok=True)
            path = folder / f"screen-{uuid.uuid4().hex}.png"
            image.save(path, format="PNG")
            _prune_captures(folder)
            _cancel_check(stop)
            return {"path": str(path), "raw_size": list(size), "actions_completed": completed}
        finally:
            try:
                held.cleanup()
            finally:
                if image is not None:
                    image.close()


async def _drain_worker(
    actions: list[dict],
    expected_size: tuple[int, int] | None = None,
    region: tuple[int, int, int, int] | None = None,
) -> dict:
    stop = threading.Event()
    worker = asyncio.create_task(asyncio.to_thread(_worker, actions, stop, expected_size, region))
    try:
        return await asyncio.shield(worker)
    except asyncio.CancelledError:
        stop.set()
        # Repeated task.cancel() calls must never detach the worker. Do not
        # release the mutex or return to the caller until its finally has run.
        while not worker.done():
            try:
                await asyncio.shield(worker)
            except asyncio.CancelledError:
                stop.set()
            except BaseException:
                break
        try:
            worker.result()  # Retrieve exceptions, including cooperative stop.
        except BaseException:
            pass
        raise


def _emit_preview(path: str) -> tuple[list[int], str | None]:
    """Use attach_image's encoder once, then inspect the exact emitted payload.

    Its byte budget can force a second shrink below 1200px. Do not predict the
    dimensions from that maximum, or independently resize and encode twice.
    """
    attachment = importlib.import_module("attach_image.attach_image")
    from PIL import Image
    from rlm import emit

    filepath = Path(path)
    # This is our own lossless capture, already bounded to 36MP by _capture.
    # The arbitrary-file attach API's 20MB limit would reject noisy 4K PNGs.
    with Image.open(filepath) as source:
        dimensions = source.size
    data, emitted_mime, note = attachment._resize_image(
        filepath, "image/png", filepath.stat().st_size, dimensions
    )
    with Image.open(io.BytesIO(base64.b64decode(data, validate=True))) as preview:
        preview_size = list(preview.size)
    emit({
        attachment._ATTACHMENT_DISPLAY_MIME: {"mime_type": emitted_mime, "data": data, "path": str(filepath)},
        "text/plain": f"Loaded image into context: {filepath}",
    })
    return preview_size, note


def _preview_point(view: dict, x: float, y: float) -> tuple[int, int]:
    """A point of the last screenshot's preview, as a primary-screen pixel."""
    (raw_w, raw_h), (preview_w, preview_h) = view["raw"], view["preview"]
    if x >= preview_w or y >= preview_h:
        raise ValueError(
            f"preview coordinates must be inside the last screenshot, 0 <= x < {preview_w}, 0 <= y < {preview_h}"
        )
    # The centre of the preview pixel, so a click lands inside what was seen.
    return (
        min(raw_w - 1, int((x + 0.5) * raw_w / preview_w)),
        min(raw_h - 1, int((y + 0.5) * raw_h / preview_h)),
    )


def _in_screen_space(actions: list[dict], space: str) -> tuple[list[dict], tuple[int, int] | None]:
    """Actions with screen-pixel coordinates, and the screen size they assume.

    `preview` reads coordinates against the last full screenshot this kernel
    showed: the image the model chose them from. The screen must still be that
    size when the batch runs, or the batch is refused before any input.
    """
    if space not in _COORDINATE_SPACES:
        raise ValueError("space must be 'preview' (the last screenshot's image) or 'screen' (raw pixels)")
    positioned = [action for action in actions if "x" in action]
    if space == "screen" or not positioned:
        return actions, None
    view = _LAST_VIEW
    if view is None:
        raise ValueError(
            "space='preview' reads coordinates from the last screenshot; call computer_use.screenshot() first, or pass space='screen' with raw pixels"
        )
    mapped = []
    for action in actions:
        action = dict(action)
        for x_key, y_key in (("x", "y"), ("to_x", "to_y")):
            if x_key in action:
                action[x_key], action[y_key] = _preview_point(view, action[x_key], action[y_key])
        mapped.append(action)
    return mapped, tuple(view["raw"])


async def _execute(
    actions: list[dict],
    *,
    space: str = "preview",
    region: tuple[float, float, float, float] | None = None,
) -> dict:
    global _LAST_VIEW
    validated = _validate_actions(actions)
    validated, expected = _in_screen_space(validated, space)
    screen_region = None
    if region is not None:
        screen_region, region_expected = _zoom_region(region, space)
        expected = expected or region_expected
    await _preflight_vision()  # MUST precede every input, including a run without a prior screenshot.
    result = await _drain_worker(validated, expected, screen_region)
    preview_size, note = _emit_preview(result["path"])
    raw = result["raw_size"]
    result["preview_size"] = preview_size
    if screen_region is None:
        result["coordinate_mapping"] = {
            "space": "primary-desktop-pixels",
            "preview_to_screen": {"x": raw[0] / preview_size[0], "y": raw[1] / preview_size[1]},
        }
        # What the next `space="preview"` batch is read against.
        _LAST_VIEW = {"raw": tuple(raw), "preview": tuple(preview_size)}
    else:
        left, top, width, height = screen_region
        result["region"] = [left, top, width, height]
        result["coordinate_mapping"] = {
            "space": "zoomed-region",
            "preview_to_screen": {"x": width / preview_size[0], "y": height / preview_size[1]},
            "origin": {"x": left, "y": top},
            "note": "a zoom does not change what space='preview' coordinates refer to: the last full screenshot",
        }
    if note:
        result["preview_note"] = note
    return result


def _zoom_region(region, space: str) -> tuple[tuple[int, int, int, int], tuple[int, int] | None]:
    """A zoom region `(x, y, width, height)` in screen pixels."""
    if not isinstance(region, (list, tuple)) or len(region) != 4:
        raise ValueError("region must be [x, y, width, height]")
    x, y, width, height = (_number(value, name, _MAX_COORDINATE) for value, name in zip(region, ("x", "y", "width", "height")))
    if width < 1 or height < 1:
        raise ValueError("region width and height must be at least 1")
    if space not in _COORDINATE_SPACES:
        raise ValueError("space must be 'preview' (the last screenshot's image) or 'screen' (raw pixels)")
    if space == "screen":
        return (int(x), int(y), int(width), int(height)), None
    view = _LAST_VIEW
    if view is None:
        raise ValueError("a zoom in space='preview' needs a screenshot first; or pass space='screen'")
    (raw_w, raw_h), (preview_w, preview_h) = view["raw"], view["preview"]
    if x + width > preview_w or y + height > preview_h:
        raise ValueError(f"zoom region must be inside the last screenshot {preview_w}x{preview_h}")
    left, top = int(x * raw_w / preview_w), int(y * raw_h / preview_h)
    right = min(raw_w, math.ceil((x + width) * raw_w / preview_w))
    bottom = min(raw_h, math.ceil((y + height) * raw_h / preview_h))
    return (left, top, max(1, right - left), max(1, bottom - top)), tuple(view["raw"])


async def screenshot() -> dict:
    """Capture and emit the primary Windows desktop without sending input.

    Returns path, raw_size, actual emitted preview_size, coordinate_mapping,
    and actions_completed=0. Requires a vision-capable model and the logged-in,
    unlocked Default desktop. This capture is NOT a lease for the next batch.
    The next `run(..., space="preview")` reads coordinates against it.
    """
    return await _execute([])


async def zoom(region: list[float], *, space: str = "preview") -> dict:
    """Capture `region` = [x, y, width, height] at the screen's full resolution.

    For small text and icons the shrunk preview blurs. Coordinates are in the
    last screenshot's preview (default) or raw screen pixels (`space="screen"`).
    The zoom is shown to the model but does not change what later
    `space="preview"` coordinates refer to.
    """
    return await _execute([], space=space, region=tuple(region) if isinstance(region, list) else region)


async def run(actions: list[dict], *, space: str = "preview") -> dict:
    """Validate and execute a bounded batch, then emit one after-screenshot.

    Each dict has a type: move(x,y), click(optional x,y, button='left', clicks=1),
    drag(x,y,to_x,to_y, button='left'), scroll(clicks, optional x,y,
    horizontal=False), press(key, presses=1), hotkey(keys), type(text), or
    wait(seconds). Coordinates are read in the last screenshot's preview image
    (`space="preview"`, the default) or as raw primary-screen pixels
    (`space="screen"`). Text may be any language; ASCII the keyboard layout
    types safely goes as keystrokes, the rest as Unicode input. Limits: 32
    actions, 20 repeats, 500 text characters, 8 distinct physical chord keys,
    2-second waits, 10-second estimated input batch. No clipboard, held/down/up
    API, or per-action permissions.

    Vision and the entire supplied batch are checked before input. Errors and
    cancellation can leave partial effects that cannot be rolled back. On
    cancellation this call waits for worker cleanup and mutex release even if
    cancelled repeatedly. Ordinary input requires PyAutoGUI.FAILSAFE=True.
    """
    return await _execute(actions, space=space)
