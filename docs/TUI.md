# Terminal interface

`ha` keeps the conversation in the terminal's scrollback. The input dock is
anchored to the bottom of the window: a rounded input box, contextual keyboard
hints, and a status line.

Each turn is drawn on one left margin. `BẠN` and `HA` sit on a colored rail, and
the operator's own message is shaded as a card, so the start of a turn is
findable while scrolling without reading the text. A tool call and its output
share one panel block; a diff keeps its added and removed tints inside it. Each
turn closes on a rule that names its outcome and counters. Ctrl-O cycles the
detail mode as prime-agent does: collapsed (reasoning hidden, three output
lines), details (reasoning and full edit diffs), expanded (every output line).

The status line has two ends: what the app is doing on the left (spinner, clock,
`esc to interrupt`, counters, model) and the telemetry that is only worth a glance
on the right (cost, context, detail mode). A narrow console drops the telemetry
before it truncates the activity. While a turn runs, the input box border changes
color and carries a phase chip, so a glance at the box answers "is it still
working?".

## Run

From the repository in PowerShell:

```powershell
cargo run -p harness-cli --bin ha -- chat --fixture
```

The fixture needs no provider credentials. For normal use, build and install
with `pwsh -NoProfile -File scripts/Install-Ha.ps1`, then run `ha` and `/login`.
Use `/config` to inspect configuration and `/permissions` to choose a permission
mode. The opening header shows the project and model; the footer reflects model
changes during the session.

## Keyboard

| Key | Action |
| --- | --- |
| Enter | Send the draft or accept a menu selection |
| Alt+Enter / Ctrl-J | Insert a newline |
| `/` / `@` | Commands / file picker |
| Up / Down | Recall history or move through a menu |
| Tab | Complete a suggestion |
| Esc | Close a panel or interrupt the running turn |
| Ctrl-C | Cancel; twice on an empty prompt exits |
| Ctrl-D | Exit on an empty prompt |
| Ctrl-O | Cycle tool detail |
| Ctrl-V / Alt-V | Paste clipboard images or files |

Hints change when a menu or approval panel owns the keyboard. Approval choices
are `y` (once), `a` (the rest of this turn), and `n` (refuse).

Type `/` at the start of the draft or after a space to open command suggestions.
After `skill /`, skills appear first. Tab or Enter inserts the selected name at
the cursor. Inside a sentence, Enter sends the draft after completion is finished.
A complete slash command on its own runs when selected.

## Terminal compatibility

Windows 10/11 is the supported platform; Windows Terminal is recommended.
Use a UTF-8 terminal, preferably at least 80 columns by 24 rows. The dock adapts
to smaller sizes and shows a resize message below 12 columns or 5 available
viewport rows (7 rows while a dialog is open). Drafts wrap inside the borders and scroll to keep the cursor
visible. History uses the terminal's own scrollback.

`NO_COLOR=1` removes colors. `TERM=dumb` and `--plain` use the existing plain
fallback. The color palette is designed for dark terminals; use `NO_COLOR=1`
for the terminal's default text colors on light backgrounds. Screen readers
should use `ha chat --plain`; fullscreen terminal accessibility varies by reader.

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
and a failed turn - each at 100x44 and at 80x24.

Real console checks live in `crates/harness-cli/tests/interactive_terminal.rs`.
They cover launch, paste, resize, approvals, cancellation, plain mode and color
fallback, and are ignored in ordinary headless test runs because they need ConPTY.
Run `scripts/Invoke-HaPtyAcceptance.ps1` from a local console for that suite.

If the installed interface looks old, check `Get-Command ha -All`: building
`target/debug/ha.exe` does not replace the installed copy in `.cargo/bin`.
