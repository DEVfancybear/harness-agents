# Desktop computer use

ha's agent can use the Windows desktop as a person sitting at it would. It sees the screen in screenshots, then moves the mouse, clicks, drags, scrolls, types and presses keys with real input. It can work in any program: File Explorer, a browser, a canvas game or a native app.

The work is done by the native `computer` tool. It is offered on Windows to a model that reads images (for example `openai-codex/gpt-6-luna`, the Claude models, or `gpt-6.1-sol`), in the conversation you talk to. Delegated children do not get it, so only one agent controls the screen at a time.

## Turning it on and off

The tool is on by default. To remove it, set either of these:

- `"computerUse": false` in `settings.json`, beside `config.toml`;
- `HA_COMPUTER_USE=0` for one run. `HA_COMPUTER_USE=1` turns it back on over the setting.

It follows the approval mode like every other tool:

- **`ask` and `auto-edit`:** the first `computer(...)` call asks you. Choose *always* to let the rest of the session run without asking.
- **`full-auto`:** it runs without asking.
- A deny rule such as `computer()` in your permissions removes it everywhere.

## What the model sends

The schema uses the action names computer-use models are trained on (Anthropic's `computer` tool):

| Action | Fields |
| --- | --- |
| `screenshot` | none |
| `zoom` | `region` `[x0, y0, x1, y1]`, shown at full screen resolution |
| `cursor_position` | none |
| `mouse_move` | `coordinate` `[x, y]` |
| `left_click`, `right_click`, `middle_click`, `double_click`, `triple_click` | optional `coordinate`; optional `text` with the modifiers to hold, e.g. `shift` or `ctrl` |
| `left_click_drag` | `start_coordinate`, `coordinate` |
| `left_mouse_down`, `left_mouse_up` | optional `coordinate` |
| `scroll` | `scroll_direction` (`up`/`down`/`left`/`right`), `scroll_amount` in notches (default 3), optional `coordinate` and modifiers |
| `type` | `text`, in any language; `\n` presses Enter |
| `key` | `text` with a key or a chord (`Return`, `space`, `ctrl+l`, `alt+F4`, `shift+Tab`); chords separated by spaces are pressed in turn; optional `repeat` |
| `hold_key` | `text` and `duration` in seconds |
| `wait` | `duration` in seconds, at most 10 |

There are two ways to call it:

- **One step:** `{"action": "left_click", "coordinate": [640, 360]}`.
- **Several steps:** `{"actions": [...]}`, up to 60 steps. They run back to back with no model call between them, the way OpenAI's computer tool batches actions. Use this for anything timed, such as a game or a menu that closes:

  ```json
  {"actions": [
    {"action": "key", "text": "space"},
    {"action": "wait", "duration": 0.45},
    {"action": "key", "text": "space"}
  ]}
  ```

### What comes back

- A step that changes the screen answers with a screenshot taken about 150 ms after it, so seeing the result costs no extra call. `"screenshot_after": false` skips that screenshot.
- `screenshot` and `zoom` steps inside a batch add their own images, at most 4 per call.
- Each result also gives the screen size, the screenshot size, where the pointer is, and the title of the window that has the keyboard.

### Coordinates

Coordinates are pixels of the screenshot image the model was shown, never of the physical screen.

- The screenshot is the primary display fitted into 1280x800, the size these models locate things best in. A 1920x1080 screen is shown at 1280x720.
- The tool maps each point to the centre of the screen area it covers. For example, `[640, 360]` is screen pixel `(960, 540)`.
- Capture and input both run per-monitor DPI aware. At 150% display scaling, the screen is still 1920x1080 physical pixels, and clicks land where the screenshot shows.

### Typing

- `type` sends characters as Unicode input, not as key presses, so an input method cannot rewrite them. On 0.7.2, typing `C:/Users` with Vietnamese Telex on arrived as `C:/U Ú Ué ẻ é`.
- `key` presses real keys, which is what games and shortcuts read. A single character such as `?` is pressed with the Shift its layout needs.

## Safety

- Input is real. It goes to whatever window has the keyboard, and the screen can show anything that is open. Use a desktop without sensitive windows, and keep an eye on it.
- What the screen shows is data, not instructions. The tool's description tells the model to ask before deleting, buying, sending messages or typing passwords.
- Don't use the mouse during a batch. Within a batch, the tool checks that the pointer is still where it left it. If it moved, someone else is using the mouse, so the batch stops and the result says so.
- Cancelling the turn (Esc) stops a batch at its next step. Keys and buttons that a step pressed are released.
- If the model left the left button held with `left_mouse_down`, it is released when the conversation's tools are dropped.
- Locked desktops, the secure desktop (UAC prompts) and windows running as administrator cannot be driven. The result names the reason instead of failing silently.
- Each screenshot is saved as a JPEG under ha's data directory, in `computer-use/<conversation>/`, for you to look back at. The newest 200 are kept. They may show private information.

## Context

Within a turn, only the newest tool results keep their images. Once more than six results hold images, all but the newest three lose them, and their text stays. The model is told to take a new screenshot to see the current state. A screenshot is a JPEG of about 100–200 KB.

With the Responses API (OpenAI and `openai-codex`), a tool's image goes inside its `function_call_output` as an `input_image` part, as Codex sends it. It is no longer a user message after the output.

## The Python skill

The older `computer-use` skill (`/skill:computer-use`, PyAutoGUI in the Python kernel) is still bundled for scripts that drive the desktop from Python. Its mock tests run in CI:

```text
python -m unittest discover -s crates/harness-cli/python/tests -v
```

`scripts/testing/smoke_computer_use.py --run` is its opt-in real-desktop smoke test.

## Live test

`scripts/testing/live-tests/computer.sh` drives a real `ha` agent through a real model and checks what happened from outside the agent:

1. **The screen and the keyboard.** The agent reads a code that only a window on the screen shows, clicks into the window's text box, types Vietnamese text and a path, and presses Enter. The window records what arrived.
2. **The browser.** In Edge, the agent fills in a local form with a Vietnamese name and a colour, submits it, and reads back the receipt number the server made.
3. **A game.** The agent opens the Flappy Bird-style game from File Explorer (This PC → Downloads → flappy-bird → `index.html`) and flies it by hand. A model's look takes far longer than the bird takes to fall, so for 20 gates in one flight it writes an autopilot: code that reads the screen's pixels and presses Space with real input, without touching the game.

A UI Automation watcher reads the game's state (ready, flying, ended, score) without touching it. The suite also checks:

- that the game's files are unchanged;
- that no developer tools opened;
- that the first three turns used only the `computer` tool.
