# Desktop computer use

ha ships a Python-backed `computer-use` skill for a vision-capable model on an unlocked Windows desktop. It uses real mouse/keyboard input and screenshots, including browser canvas games and native apps. `web_fetch` still only reads web text; it does not operate an app.

## Start

Build/install the updated ha, then start a new session and invoke `/skill:computer-use`. The managed kernel includes `pyautogui==0.9.54` and Pillow. A custom `HA_PYTHON` needs these packages installed separately. Skill imports do not capture the desktop or import PyAutoGUI until called.

Ask the agent to observe before acting:

```python
screen = await computer_use.screenshot()
# Coordinates below are EXAMPLES, not a known target on your desktop.
result = await computer_use.run([
    {"type": "click", "x": 450, "y": 350},
    {"type": "press", "key": "space"},
    {"type": "wait", "seconds": 0.2},
])
```

The result includes the raw screenshot path, raw/preview dimensions, preview-to-screen scale and completed action count. Coordinates are read in the last screenshot the model was shown (`space="preview"`, the default), the way computer-use models act in the image they see; the skill maps them to primary-display pixels, and refuses a batch if the screen changed size since that screenshot. `space="screen"` takes raw pixels. `zoom([x, y, width, height])` shows a region at full resolution for small text. Actions include `drag` (held in steps), horizontal `scroll`, and `type` in any language: ASCII the layout types safely goes as keystrokes, everything else as Unicode input (`SendInput` with `KEYEVENTF_UNICODE`; PyAutoGUI alone drops such characters silently). Secondary displays and locked/headless desktops are not supported. No clipboard changes. The newest 100 captures are kept.

## Safety and limits

Layout modifiers come from Windows, not a US punctuation table; unsafe mappings reject the whole batch before input. Start with Caps Lock off and user-held modifiers released. Raw PNG captures stay lossless and are bounded to 36MP. Noisy 4K captures may exceed the arbitrary-file `attach_image` source limit of 20MB; computer-use feeds its own capture through the shared preview encoder. Previews keep the 1200-pixel edge and 350,000-base64-character budgets.

- Use a dedicated desktop without sensitive windows; screenshots capture the primary display. Keep one agent in control. A named mutex prevents simultaneous skill calls across kernels, but does not reserve the desktop between observation and the next action or block manual input.
- Input is real and can affect any focused app. The skill runs through approval-gated `ipython`; it is not an OS sandbox and has no separate per-action authorization. Screen content is untrusted data, not instructions. Ask before destructive, financial, credential or other sensitive actions.
- Keep PyAutoGUI fail-safe enabled. Moving the mouse to the top-left corner `(0, 0)` aborts normal input. Other corners are cached by PyAutoGUI; restart the kernel after changing display resolution. Esc/cancellation stops the batch cooperatively, waits for its worker and releases only keys/buttons the skill pressed. Already-completed input cannot be rolled back. Killing the process forcibly is not cooperative cancellation.
- A batch has at most 32 actions, at most 20 repeats per action, at most 500 ASCII characters per typing action, at most 2 seconds per wait and an estimated 10-second budget. Large operations need new observations and smaller batches. Scroll `clicks` means wheel notches (120 native Windows wheel-delta units per notch). Read the skill for the exact schema.
- Games keep running during model/network latency. Short local batches help, but this does not guarantee high scores in real-time games. Verify natural progress/collision/restart on screen; never modify game state, scores, physics or timers to claim gameplay.

## Verification

Normal CI uses mock input and never touches the desktop:

```text
python -m unittest discover -s crates/harness-cli/python/tests -v
```

The opt-in genuine test needs Windows, Tkinter, Pillow and PyAutoGUI, plus an interactive desktop. Review the script before running; it opens a scratch target and captures screenshots:

```text
python scripts/smoke_computer_use.py --run
```

It checks actual target input events and decoded screenshot attachments, then checks that cancellation leaves no continuing input. Evidence is saved under `.harness/artifacts/computer-use-smoke`. It does not test an external game or use a paid model provider.

The native Windows smoke passed on 2026-10-13: real button click, exact `computer-use-Smoke!` typing, BackSpace mutation, three downward wheel notches (native delta -360), and before/after screenshot marker checks at 1920x1080 with decoded 1200x675 attachments. Cancellation drained the worker with no new input events for 2.5 seconds after settlement. See `.harness/artifacts/computer-use-smoke/summary.json`. The test waits 0.5 seconds for the initial Windows show/maximize animation without sending input, then enforces foreground checks before capture and every input batch. Model metadata and attachment transport are local test stubs, not a model/provider round trip; interactive agent use still requires a vision-capable model.

## Research

- [PyAutoGUI documentation and fail-safe](https://pyautogui.readthedocs.io/en/latest/)
- [Keyboard input limitations](https://pyautogui.readthedocs.io/en/latest/keyboard.html)
- [OpenAI computer-use observation/action loop](https://developers.openai.com/api/docs/guides/tools-computer-use)
- [Anthropic computer-use safety guidance](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool)
