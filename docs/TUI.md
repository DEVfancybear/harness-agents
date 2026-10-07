# Terminal interface

`ha` draws a conversation on one left margin with a dock pinned to the bottom of
the window: a rounded input box, contextual keyboard hints and a status line.
By default it runs fullscreen (the alternate screen) and the transcript scrolls
inside the app; with fullscreen off it keeps the conversation in the terminal's
own scrollback. See "Fullscreen and scrolling" below. For the commands and
settings the interface reads, see the [operator guide](OPERATOR_GUIDE.en.md).

## What is on screen

**Header card.** A rounded card opens the session: `ha / <project>` with the
build on the top edge, then the model, the permission mode, the thinking level
and the git branch as badges, the project path (wrapped, never cut), and the
keys that open everything else. Setup problems and sign-in instructions appear
under it as warnings. While the card is still on screen, `/model` and `/effort`
(alias `/thinking`) rewrite it, so it names the model and level now in use; once
it has scrolled away, the status line is the current source.

**Turns.** Each turn sits on a colored rail: the operator's message on a blue
rail over a shaded background, the assistant's answer on a teal rail. The palette
is a teal-accented dark theme (true color where the terminal says it supports it,
the 256-color cube otherwise). A tool call is one card: a glyph and name in the
color of the tool's family (read, list, search, edit, write, shell, git, web,
agent, Python, MCP), its target in bright text, and the outcome with the duration
at the right edge. Its output hangs from a rail below, and a diff keeps added and
removed tints. Each turn closes on a rule with a badge naming its outcome (done,
canceled, paused, waiting, blocked, failed) and its counters: steps, tool calls
and elapsed time.

**Errors.** An error row shows its summary (the last line of a traceback). The
full text comes back in details or expanded mode.

**Detail modes.** Ctrl+O cycles prime-agent's three modes: collapsed (reasoning
hidden, three lines of each tool output), details (reasoning and full edit diffs)
and expanded (every output line). A mode change and a terminal resize redraw only
the visible screen; rows already in the scrollback keep the mode they were drawn
in.

**Input box.** A rounded box with a `>` prompt. While a turn runs its border
changes color and a chip in the top-right corner says what the box is doing
(running, waiting for approval, waiting for an answer); the chip is dropped on a
box narrower than 44 cells. Drafts wrap inside the borders and scroll internally
after eight rows. Messages queued while a turn runs appear as dim preview rows
above the box with the hint `/queue to edit queued messages`; the strip is gone
when the queue is empty.

**Status line.** Two ends. The left says what the app is doing: spinner, clock,
`esc to interrupt`, counters, the model and thinking level, and anything that
needs a decision (an approval's countdown, a selector's keys). The right carries
telemetry only worth a glance: cost, `⚡ cache NN%` (the share of the session's
prompt served from the provider's cache), `◔ ctx NN%` (how full the context is)
and the detail mode. A narrow console drops the right end piece by piece (detail
mode first, then cost and cache), keeping the context gauge last, before it
truncates the left.

**Toasts.** Short confirmations such as a clipboard copy appear as pills over the
top right for three seconds. A repeat coalesces (`(x3)`), and at most three stack.

**Agents view and `/tree`.** A background agent keeps running when you leave its
terminal. Left on an empty prompt, or a bare `/resume`, opens the agents view: every
running agent and saved conversation, grouped Running, Idle and Inactive. Enter or
Right opens a row, Space writes a reply, Ctrl+R renames, Ctrl+X stops or deletes
(press twice), Ctrl+N starts a session, and Esc leaves. An agent with subagents shows
`N subagents (M running)`; Alt+Right expands the forest and Ctrl+O shows the
program each was spawned with. `ha attach` brings a running agent back. `/tree`
(also a double Esc on an empty idle prompt) shows the conversation's branches:
arrows choose, Ctrl/Alt+Left and Right fold and unfold, typed text filters, Enter
picks a turn and offers to summarize the branch you leave.

## Run

From the repository in PowerShell:

```powershell
cargo run -p harness-cli --bin ha -- chat --fixture
```

The fixture needs no provider credentials. For normal use, build and install
with `pwsh -NoProfile -File scripts/Install-Ha.ps1`, then run `ha` and `/login`.
Use `/config` to inspect configuration and `/permissions` to choose a permission
mode.

## Keyboard

| Key | Action |
| --- | --- |
| Enter | Send the draft or accept a menu selection |
| Alt+Enter / Shift+Enter / Ctrl+J | Insert a newline (Shift+Enter only where the console reports the Shift; Ctrl+J and Alt+Enter always work) |
| `/` / `@` | Commands / file picker |
| Up / Down | Move between lines of a multiline draft, recall history, or move through a menu |
| Tab | Complete a suggestion |
| Esc | Close a panel or menu; interrupts a running turn; twice on an idle prompt clears the draft, or opens `/tree` when it is empty |
| Ctrl+C | Cancel the running turn; clears a draft; a second press within two seconds on an empty prompt exits |
| Ctrl+D | Exit on an empty prompt; deletes forward while there is text. A background agent keeps running: `ha attach` brings it back |
| Left (empty prompt) | Hand the terminal to the agents view (agent keeps running) |
| Ctrl+O | Cycle tool detail (collapsed, details, expanded) |
| Ctrl+L | Repaint the screen |
| Ctrl+G | Edit the draft in `$VISUAL` / `$EDITOR`; the saved text replaces the prompt |
| Ctrl+S | Stash the draft, or bring it back |
| Alt+M / Shift+Alt+M | Cycle the scoped models forward / back |
| Ctrl+V / Alt+V | Paste a clipboard image; Windows Terminal keeps Ctrl+V for text, so Alt+V or `/image` is the sure way |
| PageUp / PageDown | Scroll the transcript (fullscreen) |
| Shift+Alt+Up | Scroll to the top of the transcript (fullscreen) |
| Ctrl+End / Ctrl+Shift+Down | Back to the end, following the output (Windows Terminal keeps Ctrl+Shift+Down, so use Ctrl+End) |

Prompt editing follows prime-agent. Ctrl/Alt+Left and Right, Alt+B and Alt+F move
by word; Ctrl+A and Ctrl+E go to the line's start and end; Ctrl+B and Ctrl+F move
by character. Ctrl+K, Ctrl+U and Ctrl+W delete to the end of the line, to its
start and the word before the cursor into a kill ring that Ctrl+Y yanks back;
Alt+D or Alt+Delete deletes the next word; Alt+Backspace the previous one; Ctrl+T
transposes two characters. Ctrl+Z (or Ctrl+-) undoes an edit and Ctrl+Shift+Z
redoes it. A large paste (more than 10 lines or 1000 characters) becomes one
marker such as `[paste #1 +42 lines]`, expanded when the prompt is sent; the
marker is deleted as a unit. A paste with newlines is never run as separate
commands.

Keys can be rebound in `keybindings.json` next to `settings.json`, using
prime-agent's binding ids (`tui.editor.*`, `tui.input.*`, `tui.viewport.*`,
`app.*`). A binding answers only to the keys given, and an empty list turns it
off. Ids that ha has no action for are ignored.

Hints change when a menu or approval panel owns the keyboard. Approval choices
are `y` (once), `a` (the rest of this turn) and `n` (refuse).

Type `/` at the start of the draft or after a space to open command suggestions.
After `skill /`, skills appear first. Tab or Enter inserts the selected name at
the cursor. Inside a sentence, Enter sends the draft after completion is finished.
A complete slash command on its own runs when selected.

## Fullscreen and scrolling

Fullscreen is on by default. The transcript is a scrollable window over the
conversation; the scroll position belongs to the app, not the terminal. The mouse
wheel scrolls three rows, a drag selects text (anchored to the conversation's
rows, scrolling by itself at an edge) and releasing copies it, with a toast. Hold
Shift, Alt or Ctrl for the terminal's own selection. Leaving fullscreen prints
what it showed into the normal scrollback.

- `/fullscreen` toggles it and saves `terminal.fullscreen` in `settings.json`;
  `terminal.fullscreenMouse` turns mouse tracking off.
- `HA_FULLSCREEN=1` forces it on and any other value forces it off, overriding
  the setting.

With fullscreen off, history uses the terminal's own scrollback.

## Terminal compatibility

Windows 10/11 is the supported platform; Windows Terminal is recommended. Linux is
not supported yet. Use a UTF-8 terminal, preferably at least 80 columns by 24 rows.
The dock adapts to smaller sizes and shows a resize message below 12 columns or 5
available rows (7 rows while a dialog is open). After a resize the screen is
repainted once more when the terminal stops re-wrapping.

The plain renderer (line input, no dock) is used when:

- you pass `ha chat --plain` or set `HA_UI=plain`;
- `TERM=dumb`;
- the console is smaller than 60x10 at launch (`ha` says why);
- raw mode is unavailable (`ha` prints a notice and uses line input).

`NO_COLOR=1` (or `TERM=dumb`) removes every color but keeps the layout and glyphs.
The palette is designed for dark terminals; use `NO_COLOR=1` for the terminal's
default text colors on light backgrounds. Screen readers should use
`ha chat --plain`; fullscreen terminal accessibility varies by reader.

Vietnamese, CJK and ordinary emoji use terminal cell widths. Arabic text remains
in logical order: shaping and bidirectional display depend on the terminal.
Complex joined emoji can still differ in width between terminal emulators.

## Development and checks

```powershell
cargo fmt --all -- --check
cargo test -p harness-cli --bin ha interactive::tui
cargo test --workspace --locked
```

To look at the interface without a console, dump the frames the renderer paints:

```powershell
cargo test -p harness-cli --bin ha preview_dump -- --ignored --nocapture
```

That writes `target/tui-preview/<scene>.ansi` (the screen, with colors) and
`<scene>.txt` (the whole transcript, unclipped) for a fresh session, a finished
turn, a streaming turn, an approval, the command menu, an expanded tool, a diff,
and a failed turn, each at 100x44 and at 80x24.

Real console checks live in `crates/harness-cli/tests/interactive_terminal.rs`.
They drive `ha` through a pseudo-console (ConPTY) and cover launch, paste, resize,
approvals, cancellation, plain mode and color fallback, among others. They are
`#[ignore]`d, so `cargo test` skips them: ConPTY only delivers a transcript when the
process creating it owns a console. Run the suite from a local console with:

```powershell
pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1
pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1 -Filter <test name>
```

The script builds the test binary, runs it in a new console with a hard time bound
(`-TimeoutSeconds`, default 600) and writes the transcript under
`target/pty-acceptance/`. A run that leaves worker processes behind can make the
next one flaky; stop stray `ha` processes first.

If the installed interface looks old, check `Get-Command ha -All`: building
`target/debug/ha.exe` does not replace the installed copy in `.cargo/bin`. Run
`scripts/Install-Ha.ps1` again (see [BUILD_AND_RELEASE.md](BUILD_AND_RELEASE.md)).
