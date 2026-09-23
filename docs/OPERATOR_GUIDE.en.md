# Operator guide — running and recovering the harness

Ngôn ngữ: [Tiếng Việt](OPERATOR_GUIDE.vi.md)

This guide is written for the person who has to run, back up, restore, migrate and
retire a harness data directory. It states the commands that exist in this
release, what each one refuses to do, and the limits an operator must not
discover the hard way. It is the operator-facing companion to
[P7_RELEASE.en.md](implementation/P7_RELEASE.en.md).

## 1. What this release is

The harness is a single-host, foreground coding agent runtime. One writable host
owns a data directory at a time; the ownership is an operating-system file lock
plus a fencing generation recorded in the database, not a network service.

What that means in practice:

- **There is no daemon.** Work stops when the host process exits. No background
  service keeps running, retries, or collects anything on its own. Every
  maintenance action below is a foreground command an operator runs.
- **There is no server to connect to.** Remote MCP endpoints and OS-level
  sandboxing are declared unsupported in the release matrix; transport isolation
  is not a sandbox.
- **There is no published release artifact.** Linux and Windows builds are
  exercised by the phase gates, but nothing was packaged, signed or published.
- **Provider credentials are not exercised.** The gates never call a paid model
  API, so no release claim depends on one.

The authoritative, machine-readable statement of all of this is
`ha maintenance release-matrix --json`. If this guide and that output ever
disagree, the output is correct and the guide is a bug.

## 2. Diagnosing a data directory

```console
ha maintenance doctor --data-dir <DATA_DIR> --json
```

`doctor` opens the store writable, reports the schema revisions it found, the
`SQLite` settings in force, the session, delegated-task, artifact and retention
counts, and — importantly — a `not_verified` list naming what it did **not**
check. It never reports a clean bill of health for something it did not inspect.

Two fields matter before anything else:

- `writable` — whether this binary may write the directory. A store written by a
  **newer** binary reports `false`: the schema is ahead of this build, writes are
  refused, and read-only inspection still works so the directory can be
  diagnosed rather than corrupted.
- `compatibility` — a plain-language sentence for the same fact.

## 3. Backup

```console
ha maintenance backup --data-dir <DATA_DIR> --into <NEW_BACKUP_DIR> --json
ha maintenance verify-backup --backup <BACKUP_DIR> --json
```

A backup is a complete snapshot of one data directory into a directory that does
not exist yet:

- The database snapshot is produced by `SQLite` itself after the write-ahead log
  is folded in, so it is a consistent standalone database rather than a
  half-copied WAL set.
- Every artifact the store references is copied and hashed. The manifest records
  each artifact's identity, relative path, content hash and byte length.
- The schema revisions of the snapshot, the outstanding retention pins and the
  active tombstones are recorded in the manifest.
- The manifest carries a digest over its own body, so a later edit is detectable.

A backup directory holds the snapshot `harness.sqlite3`, the copied artifacts, and
`backup-manifest.json` — the file that names every artifact, hash and revision the
snapshot contains.

`verify-backup` re-reads the manifest, checks its digest, checks the database
hash, and checks every artifact's hash. Use it before you trust a backup, and
again after moving a backup to other media.

What backup refuses:

- **It never overwrites or merges.** An existing backup directory is refused, so
  a backup can never silently mix two snapshots. Take a new directory per backup.
- **It does not back up an unsaved store.** A directory with no database is
  refused (`backup_manifest_invalid`) rather than producing an empty backup.
- **It does not touch the source.** The source directory is read, never written,
  apart from the write-ahead-log checkpoint that makes the snapshot consistent.

The backup directory is self-contained. Copying it elsewhere is enough; there is
no catalog or index to keep in sync.

## 4. Restore

```console
ha maintenance restore --backup <BACKUP_DIR> --into <NEW_DATA_DIR> --json
```

A restore validates the backup, copies the snapshot and every artifact, and then
proves the result before returning:

- `database_verified` — `SQLite`'s own integrity check passed on the restored copy.
- `artifacts_verified`, `artifacts_missing`, `artifacts_corrupt` — every artifact
  was re-hashed after it was written.
- `tombstones_restored` — a forgotten source stays forgotten across a restore.

**A restore does not activate.** It writes into a fresh directory, records
`restore.json` with `"activated": false`, and stops. Nothing points at the
restored copy until an operator decides to move to it. That separation is the
whole point: a bad restore cannot replace a working directory, because a restore
never replaces anything.

What restore refuses:

- **A destination that already holds a store.** `restore_target_conflict`. This
  includes restoring over the live directory and restoring into the backup
  directory itself.
- **A destination marked active.** A directory holding `.active` is refused.
- **An incomplete or corrupt snapshot.** A manifest that does not match its own
  contents, a database that fails its integrity check, or an artifact whose bytes
  no longer match is refused with `backup_manifest_invalid`. A partial restore is
  an error, not a warning.

Activation is a separate, explicit step performed by the operator's tooling, not
by this command. Only after the restored copy has been inspected should it be
opened writable and marked active.

## 5. Upgrading and downgrading

```console
ha maintenance migrate-copy --data-dir <DATA_DIR> --into <NEW_DATA_DIR> --json
```

Migration always runs on a **copy**:

- The source directory is copied into a fresh destination, and the copy is opened
  writable so the ordinary migration path runs against it.
- The source is left byte-identical. An interrupted migration leaves the source
  readable and unchanged, and the incomplete copy can simply be deleted.
- The writer lock is process state, not data. It is never copied; the migration
  open acquires its own.

What migration refuses:

- **An occupied destination.** `restore_target_conflict`, so a migration can
  never merge into an existing store.
- **A source with no store.** `migration_failed`.

Downgrade is refused rather than attempted. If the store records a schema
revision newer than the running binary supports, `doctor` reports
`writable: false`, writes fail with a typed error, and read-only inspection keeps
working. The remedy is a newer binary, never a hand-edited schema row.

## 6. Retention: invalidate, archive and forget

```console
ha maintenance retain --data-dir <DATA_DIR> --action invalidate \
    --source-kind file --source-id src/lib.rs --reason "content changed" --json
ha maintenance retain --data-dir <DATA_DIR> --action archive \
    --source-kind file --source-id src/lib.rs --reason "kept for audit" --json
ha maintenance retain --data-dir <DATA_DIR> --action forget \
    --source-kind file --source-id src/lib.rs --reason "operator request" \
    --confirm file:src/lib.rs --surviving-copy backup-2026-01 --json
ha maintenance tombstones --data-dir <DATA_DIR> --json
```

These are three different operations, and the difference matters:

| Action | What it does | Removes content? | Needs confirmation? |
| --- | --- | --- | --- |
| `invalidate` | Marks derived knowledge unusable, keeps the history | No | No |
| `archive` | Moves content out of active use, keeps it restorable | No | No |
| `forget` | Removes the content and records a tombstone | **Yes** | **Yes** |

**`invalidate` is never `delete`.** An invalidated item is refused for future use
while its record — and the fact that it existed and changed — is retained. Only
`forget` removes content.

`forget` requires `--confirm` to equal the full target
`--source-kind:--source-id` (for example, `file:src/lib.rs`). An ID-only token,
the wrong kind/ID or an empty value is refused with `retention_refused`, and
nothing is recorded. Retention applies across all projects in the store that use
the same kind/ID pair; file source identities are currently workspace-relative
paths.

A `tombstone` is the durable record that a source was deliberately forgotten. It
is written in the same transaction as the forget, it survives backup and restore,
and it blocks re-extraction: any later pass that would re-read that source is
refused, so forgotten data cannot quietly reappear. `ha maintenance tombstones`
lists them.

**A tombstone cannot reach outside this data directory.** The delete is local;
copies you made elsewhere are not touched. That is why `forget` takes
`--surviving-copy` (repeatable) and reports it back: the tombstone records every
external or backup copy that may still contain the data, so the operator has an
explicit list to chase. Passing an empty list is allowed, and it means you assert
there is no other copy — the tool will not invent one, and it will not pretend
the deletion was global.

## 7. Garbage collection

```console
ha maintenance gc --data-dir <DATA_DIR> --grace-seconds 604800 --dry-run --json
ha maintenance gc --data-dir <DATA_DIR> --grace-seconds 604800 --json
```

Garbage collection removes artifact bytes, and it removes only an artifact that
is simultaneously:

1. **unreferenced** — no receipt, tool artifact scope or memory version points at it;
2. **unpinned** — no backup or unfinished task holds a retention pin on it; and
3. **older than the grace period** — the default is 604800 seconds (7 days).

The report names exactly why each survivor survived: `retained_pinned`,
`retained_referenced` or `retained_young`. Use `--dry-run` first; it reports the
same analysis and deletes nothing.

The pin exists to close one specific race: a backup promises to keep an artifact
while collection wants to remove it. The candidate list is only a snapshot; before
quarantining bytes, the store rechecks pins, references and mtime in the writer
transaction. An unreadable mtime keeps the artifact inside the grace period.
`pin_artifacts` accepts only IDs with an existing artifact row and refuses the
whole batch if any ID is missing. Backups record their pins in the manifest for
later audit.

## 8. The release matrix

```console
ha maintenance release-matrix --json
ha maintenance release-matrix --json --retrieval-p95-ms 900 --restore-ms 1200
```

The matrix reports four separate things and refuses to blur them:

- **platforms** — each supported target triple with `verified`, `unverified` or
  `failing`, plus the evidence for that status. A platform that was never
  exercised is `unverified`, never omitted.
- **capabilities** — `supported`, `component_only` (implemented but not exercised
  end to end) or `unsupported`. An unsupported surface is named with the reason.
- **benchmarks** — a stated `target` with a `measured` value that is `null` until
  a real run produced one. `met` is `null` while `measured` is `null`. A target is
  a target; it is never reported as achieved because it was written down.
- **verified_cases**, **unverified_checks**, **out_of_scope** — the 44 continuity
  and plugin cases this release exercises, the checks that were not run and why,
  and what this release explicitly does not do.

The `verdict` is the honest one-line summary: a failing platform makes the
release "not release-ready", an unexercised platform makes it "partially
verified", and an unmeasured benchmark is named as unmeasured even when every
platform is green.

## 9. What an operator must not assume

- No background process keeps the system tidy. If you do not run `gc`, nothing is
  collected; if you do not run `backup`, nothing is protected.
- A restore is not a switch. It produces a candidate directory; activating it is
  an operator decision with its own consequences.
- A forget is local and durable, not global. Read `surviving_copies` before
  telling anyone the data is gone.
- A backup is verified only when `verify-backup` says so on the media you hold.
- An artifact inside the grace period, pinned, or referenced will not be
  collected, and that is correct behaviour rather than a failure.
- The gates prove the behaviour they ran. Platforms, capabilities and benchmarks
  they did not exercise are reported as such; treat any claim not in the matrix
  as unverified.

## 10. Command summary

| Command | Purpose | Refuses when |
| --- | --- | --- |
| `ha maintenance doctor` | Diagnose a data directory | Never; reports `writable: false` instead |
| `ha maintenance backup` | Snapshot into a new directory | The backup directory exists; the source has no store |
| `ha maintenance verify-backup` | Validate a backup and its artifacts | The manifest, database or any artifact fails its hash |
| `ha maintenance restore` | Restore into a new directory, without activating | The destination holds a store or is active; the snapshot is incomplete |
| `ha maintenance retain` | `invalidate`, `archive` or `forget` a source | `forget` without `--confirm` equal to `--source-kind:--source-id` |
| `ha maintenance tombstones` | List forgotten sources and surviving copies | Never |
| `ha maintenance gc` | Collect unreferenced, unpinned, old artifacts | Never; it reports what it retained and why |
| `ha maintenance migrate-copy` | Migrate a store on a copy | The destination is occupied; the source has no store |
| `ha maintenance release-matrix` | Report platforms, capabilities and benchmark honesty | Never; it reports what is unverified |

## 11. Getting the CLI into your terminal

```console
pwsh -NoProfile -File scripts/Install-Ha.ps1
ha --version
ha maintenance doctor --data-dir <DATA_DIR>
```

`ha` is an ordinary executable, so installing it means putting that executable on
your `PATH`. The repository ships one script that does it, in two routes:

| Route | Command | What it does |
| --- | --- | --- |
| Copy (default) | `scripts/Install-Ha.ps1` | Builds `ha` with `cargo build --release -p harness-cli --bin ha --locked` and copies the compiled artifact into `$HOME/.cargo/bin`, the directory the Rust installer already put on `PATH` |
| Cargo | `scripts/Install-Ha.ps1 -UseCargoInstall` | Runs `cargo install --path crates/harness-cli --locked --root $HOME/.cargo`, so `cargo uninstall harness-cli` can remove it later |

Useful variations:

```console
pwsh -NoProfile -File scripts/Install-Ha.ps1 -Profile Debug        # faster build, for local iteration
pwsh -NoProfile -File scripts/Install-Ha.ps1 -Force                # rebuild even if the binary looks current
pwsh -NoProfile -File scripts/Install-Ha.ps1 -Destination <DIR>    # install somewhere else
pwsh -NoProfile -File scripts/Install-Ha.ps1 -SkipBuild            # install the already built binary
```

What the script does **not** do, on purpose: it downloads nothing, it publishes
nothing to a package registry, and it never edits your `PATH`. When the install
directory is not on `PATH`, it prints the exact directory to add instead of
changing your profile behind your back. It also prints the installed path and the
`ha --version` output, so you can see which binary you are about to run.

Both routes build from the same source tree the phase gate tests; only the cargo
profile differs. If you want the exact artifact the release gate exercised, use
the default release profile.

Removing it again:

```console
Remove-Item "$HOME/.cargo/bin/ha.exe"     # the copy route
cargo uninstall harness-cli               # the cargo route
```

If you installed from a **release bundle** (`-FromBundle <dir>`, the end-user route), remove
it with the installer itself so only what it recorded is deleted:

```console
pwsh -NoProfile -File scripts/Install-Ha.ps1 -Uninstall -Destination <DIR>
pwsh -NoProfile -File scripts/Install-Ha.ps1 -Uninstall -Destination <DIR> -RemoveUserPathEntry
```

- The first command removes **exactly the files in the manifest** and **leaves the `PATH`
  entry** the install added (it prints that entry and how to remove it). The second one
  removes the entry too: it needs its own switch because installing also needed
  `-ModifyUserPath` before writing to the User PATH.
- Neither touches your config or session data. There is currently **no** command that deletes
  user data: the plan describes a separate `purge` route with path containment and
  confirmation, but it is **not** implemented, so do not count on it.

Two things to know after installing:

- **`ha` is foreground.** There is no daemon: nothing runs between your commands,
  so a backup, a collection or a retention action happens exactly when you ask for
  it and at no other time.
- **A fresh directory is a valid target.** `ha maintenance doctor --data-dir <DIR>`
  works on a directory that holds no store yet and reports it as uninitialized,
  which is the state this binary may create a store in. It does not pretend a
  store already exists, and backing such a directory up is still refused.

Building from source stays available for development:
`cargo build -p harness-cli --bin ha --locked` writes `target/debug/ha`, and
`cargo run -p harness-cli --bin ha -- <args>` runs it without installing anything.


## 12. Starting the interactive app with `ha`

Typing bare `ha` in a terminal opens the interactive app instead of exiting: the header
shows the project, the provider and the setup state, then the prompt appears.

**Behaviour change worth knowing (migration):** bare `ha` used to exit 0 silently. It now:

| Situation | Behaviour |
| --- | --- |
| `ha` in a real terminal | Opens the app; exits only on `/exit`, Ctrl-D on an empty line, or Ctrl-C while waiting |
| `ha` with redirected stdin/stdout (pipe, CI, script) | Does **not** wait for input: prints short guidance on stderr and exits with code **2** |
| `ha --help`, `ha --version`, the existing subcommands | Unchanged; the app is not started |

| Command | What it does |
| --- | --- |
| `ha chat` | Same entrypoint as bare `ha` |
| `ha chat --cwd <path>` | Opens that project instead of the current directory |
| `ha chat --resume <session-id>` | Continues from a persisted session (the app also has `/resume`) |
| `ha chat --headless --prompt "<text>" [--json]` | One turn without a terminal; result on stdout, logs on stderr |
| `ha chat --fixture` | Labelled fixture backend for trying the UI; no model is called |

**Behaviour change worth knowing (identity):** a turn used to generate a new project
identity every time, so anything scoped to the project — tool artifacts, approvals,
memory — belonged to an identity the next turn could not name again. A workspace root now
registers **one** project identity in its store, and every later turn, in this process or
a later one, resolves that same identity.

Inside the app: `/help`, `/status`, `/config`, `/model`, `/new`, `/resume [number|id]`,
`/exit`. A gated action prints the action, working directory and scope, then waits for
`y` (run it once), `a` (allow every action for this turn) or `n` (refuse); there is no
implicit approval and no answer in time counts as a refusal. A real model call needs
`HA_PROVIDER_ENDPOINT`, `HA_PROVIDER_MODEL`
and a credential (`DEEPSEEK_API_KEY` or `HA_API_KEY`); without them the app still opens
in setup state and says what is missing, and it never fabricates an answer.

**Approval: every panel offers the same three answers, and `a` ends the questions for the
turn.** It is the answer to a measured complaint: a turn of `git log`, `git status`,
`git diff` asked about every single command, because `run_process` is not a read-only action
and the older read-only grant could not cover it.

| Key | Meaning | How long it lasts |
| --- | --- | --- |
| `y` | run this action once | this action only |
| `a` | run this action **and** allow **every** action for **this turn** - including file writes and commands | until the turn ends |
| `n` | refuse | the action does not run |

After `a`, the status row shows `· tự động cả lượt` (automatic for this turn) so an open gate
is never silent, and the transcript records one `[info] allowed for this turn: <action>` line
per action it covered (reads add `read-only, `). The grant dies with the turn - a later
request asks again from the first action - and it never bypasses the checks that run *before*
a panel exists: a path inside a protected file (`.env`, `.git`, `.harness`, `*credential*`,
`*.pem`…) or outside the workspace is refused outright, so there is nothing for `a` to allow.
There is currently **no** permanent trust mode for a workspace.

**One API key is enough.** `DEEPSEEK_API_KEY` (or `HA_API_KEY`) is the whole
credential: the endpoint and the model default to the values `DeepSeek` publishes -
`https://api.deepseek.com` and `deepseek-flash` (see <https://api-docs.deepseek.com/>).
Set `HA_PROVIDER_ENDPOINT` or `HA_PROVIDER_MODEL` for another provider or model; a
variable you set always wins over the default.

```powershell
$env:DEEPSEEK_API_KEY = '<your key>'   # this line is the whole setup
ha                                     # opens the TUI; the status bar names the model

# optional, only for another model or provider:
$env:HA_PROVIDER_MODEL = 'deepseek-v4-pro'
```

**When the provider answers with an error, run `/model` or `/status` inside the
app.** They print which variable the credential came from (never its value), whether the
endpoint and the model are yours or the defaults, and whether the endpoint is reachable. If
the last line says `endpoint did not answer`, the problem is the network or a proxy; if it
says `no credential; set one of ...`, the key was not set **in the shell that launched
`ha`** (an environment variable only reaches processes started after you set it).

To check the real provider before opening the app (costs one paid call):

```powershell
pwsh -NoProfile -File scripts/Smoke-HaProvider.ps1
```

### 12.1. The TUI (HA_TUI track)

The interactive app draws an **inline viewport** at the bottom of the console: the
conversation still flows into the terminal's own scrollback (scroll it with the terminal),
while the bottom of the screen is a fixed area holding the composer, the status bar and the
temporary panels. It is **not** a full-screen app.

Bàn phím / keys (only the combinations measured on a real console):

| Key | What it does |
| --- | --- |
| `Enter` | Submit the request (an empty buffer is not submitted). With the command menu open: **complete** the half-typed command; the next Enter runs it |
| `Ctrl-J` | Insert a line break in the composer |
| `Alt+Enter` | Insert a line break (measured on this Windows Terminal's ConPTY; see the limits below) |
| Multi-line paste | Keeps its newlines and never submits; the whole block is **one** request |
| `↑` / `↓` | Command menu open: move the highlight · single-line buffer: history; multi-line buffer: move by row |
| `←` `→` `Home` `End` | Move by character |
| `Ctrl-A` / `Ctrl-E` | Start / end of the current row |
| `Ctrl-U` / `Ctrl-W` | Erase to the row start / erase one word |
| `Tab` | Accept the highlighted row of the command menu; with no menu, complete a slash command when there is exactly one candidate |
| `Esc` | Close a panel, an overlay or the command menu; it never cancels a running turn |
| `Ctrl-C` | Running: cancel the turn · idle: clear the buffer |
| `Ctrl-D` | Empty buffer: leave |
| `Ctrl-L` | Repaint the bottom area without clearing the scrollback |
| `y` / `n` | Answer the approval panel (or type `yes`/`no` and press Enter) |
| `a` | Run this action and allow **every** action for **this turn** (including file writes and commands); the turn ends it (or type `all` and press Enter) |

**Typing `/` lists the commands.** The menu appears directly above the composer and narrows with every
character. It is **not** a modal window: the cursor stays in the composer and the draft is untouched.

```text
❯ /help            list these commands
  /status          show project, config, data and provider state
  /key             save the provider API key; the value is masked and never kept in history
  /more            reopen the recent transcript in a scrollable panel (PgUp/PgDn, Home/End)
  /new             start a new session when nothing is running
  /model           show which model the next run would use
 ↑↓ chọn · Tab/Enter nhận · Esc đóng · 11 lệnh ─────────────
> /_
```

When more than **6** commands match, the list is a **window that follows the highlight**: the selected
row is always visible instead of being cut off. A row shows the arguments a command takes
(`/attach <path>`, `/resume <id>`), so it reads as the thing to type. Accepting a suggestion does
**not** append a space, deliberately: `/key ` would turn the following keystrokes into the **visible**
form of the command, while a bare `/key` opens the **masked** entry path.

The menu exists only where it is **drawn**: in plain mode (no menu at all) and while a
panel/picker/overlay is open, `Enter`/`Tab`/`↑`/`↓` keep their old meaning — no key ever acts on a list
you cannot see. While an API key is being entered (masked buffer) the menu never appears either.

The approval panel shows the action, workspace, scope and a **countdown** to the gate's
deadline; when it expires the action does **not** run and the panel closes. Every panel offers
the same three keys: `y` run once · `a` allow every action for this turn (including file
writes and commands) · `n` refuse.

### 12.2. When the app uses the plain interface

The TUI is the default. The app falls back to **plain mode** (the previous interface: plain
lines above a `> ` prompt) when any of these holds, and it always prints the reason to
stderr: `ha chat --plain` or `HA_UI=plain`; a console smaller than 60 columns by 10 rows;
`TERM=dumb`; or raw mode refusing to start (which selects line input instead).
`--plain` conflicts with `--headless` (clap rejects it, exit 2).

### 12.3. Windows limits (measured)

- **Shift+Enter is indistinguishable from Enter** on a Windows console, so it is not
  documented as a key and is not a way to add a line. Use `Ctrl-J` or `Alt+Enter`.
- On exit the app leaves the cursor at column zero of a fresh line and the conversation
  stays in the scrollback.
- A hard kill (Task Manager, power loss) cannot restore the terminal; that is a known limit
  of every terminal application, not a defect of `ha`.

**Not verified on this machine:** the real PTY transcript (ConPTY does not work in the
sandbox in use — see section 8 of `docs/evidence/HA_LAUNCH.vi.md`) and the live provider
smoke (no credential or budget is granted).

### 12.4. Reading the end of a turn

Every prompt ends with one `[run]` line, and the four words it can use mean different things:

| Line | What happened |
| --- | --- |
| `[run] done` | The model answered and asked for nothing more. |
| `[run] paused: step limit reached` | The turn stopped at one of your bounds — steps, tool calls or the deadline — before the model answered. **Nothing was lost:** every tool receipt is durable, the transcript stays in scrollback, and the next prompt continues the same task. Bounds are `step 2/8` in the status bar, and a step is one model call. |
| `[run] failed: <reason>` | Something broke: the provider was unreachable, a tool could not be prepared, or the run itself errored. The reason is printed after the colon. |
| `[run] canceled` | You canceled it (`Ctrl-C`). |

A tool card that fails says why: `failed 962ms · invalid_payload: optional tool path must
not be blank` is a call the model shaped wrongly (it is told the same thing and usually
retries), while `failed 1.2s · policy_denied: denied by the user` is a refusal you made.
**A bound is not the budget, so the app carries on past it.** A step or tool-call bound
stops a loop that has gone wrong; it does not mean your task is finished, and being made to
type "continue" to let your own agent keep working reads as a stall. When a turn stops that
way the app sends the next request itself, and says so in the transcript:

```text
[run] paused: step limit reached · 8 steps · 14 tool calls · 46.8s
[info] step limit reached; continuing automatically (1 of 4) — Ctrl-C stops this
[auto] continue: the previous turn stopped at a bound, not because the task was finished — …
```

`[auto]` marks the requests the app made on your behalf, so the transcript still separates
what you asked for from what it did. `Ctrl-C` spends the rest of the budget: after it, a
bound is a real stop until you speak again. The **deadline** never continues itself — that
is wall-clock time already spent, and continuing it would spend the same time over and over.

Four variables move the bounds, and `/status` always prints the values in force:

| Variable | Default | Effect |
| --- | --- | --- |
| `HA_TURN_MAX_STEPS` | 8 | Model calls in one turn. |
| `HA_TURN_MAX_TOOL_CALLS` | 16 | Tool calls in one turn. |
| `HA_TURN_DEADLINE_SECONDS` | 600 | Wall-clock seconds in one turn. |
| `HA_TURN_CONTINUATIONS` | 4 | Turns the app may continue by itself after a step or tool-call bound. `0` turns that off, so every bound waits for you. |

With the defaults a single request can reach thirty-two model calls (8 × (1 + 4)) before it
stops for good — enough for real agentic work, still bounded, and never unbounded. Only a
positive integer counts for the three bounds: `HA_TURN_MAX_STEPS=unlimited` is a typo and
keeps the default, because a misspelled value must not remove the net. A headless turn
(`--headless --json`) is still exactly one turn and never continues itself: it reports
`"stop":"step_limit"`, and a script resumes with `--resume`.


### 12.5. Chat memory (opt-in)

Memory is **off unless you ask for it**. Set `HA_MEMORY=on` in the shell that launches
`ha`; any other value, or leaving it unset, keeps the behaviour above: no memory is read
and nothing about the conversation is stored.

With it on, one turn does two bounded things:

- **before the request**, your text is the retrieval query; the memory of this workspace
  that matches is added to the context the model receives, together with the exact memory
  version each block came from;
- **after the turn**, the text the journal admitted is stored once as a confirmed,
  project-scoped memory asset, and **one record of that turn** is written to the
  conversation log (see "What gets remembered" below — the log keeps an excerpt of the
  model's answer).

The workspace root keeps one project identity in the store, so memory written by one run
is readable by the next one, including a new terminal, a new session or a new task.

**How retrieval works (changed).** It used to match documents that contained **every** term
of the question, and retried with the four longest terms when that found nothing. That
failed on the most ordinary case: you ask "what marker did I ask you to remember?", the
instruction says "Remember this marker for later", and the conjunction fails on the words
only the question has. The answer was in the store while the model reported it was not.

The app now takes the **union** of the terms and keeps only the hits that contain **at
least two** of them (one, for a one-term question). The floor is the part that matters: a
single shared word is an accident of vocabulary rather than evidence of aboutness, and
without it a wider query would trade a silent miss for a confident wrong answer — which is
worse. If nothing clears the floor it falls back to the exact conjunction, so a question
that means a specific phrase still gets one.

**Two kinds of material, asked in order.** The store holds **durable memory** (what you
asked to keep, what the runtime observed, derived L2 — it does not expire) and the
**conversation log** (one record per turn). A turn record contains your input verbatim, so
it and the directive that stored the same input overlap almost completely, and the record
also holds part of the answer. The app therefore asks **durable memory first**; it asks the
log only when durable memory holds nothing for that question. The two used to be asked
together, and it was measured that the block injected for a directive's own words was
sometimes the **turn record** — your instruction arriving framed as something you were
quoted saying rather than as an instruction. When the log is what answered, the transcript
says so: `... injected from the conversation log, not from durable memory`.

Every turn prints what happened (`memory: 1 hit(s), 1 block(s) injected`). The two kinds of
empty are named differently: `nothing matching this question yet (no term overlap)` means it
searched and found nothing, `nothing to search for in this message` means the message held
nothing to search for. A headless run reports the same in its `--json` result under
`memory`.

**What gets remembered (changed).** Two different things are written, and neither replaces
the other:

- **Durable memory**: only **directives and statements**. A **question** is not: it is you
  asking, not you telling, and storing each one as a confirmed `user_instruction` asset is
  how the corpus filled with questions that then outranked the answers. When an input is
  skipped the app says so (`memory: not stored (a question is not an instruction)`) instead
  of staying silent.
- **The conversation log**: **every** turn, including one that was only a question. Each
  record keeps `asked:` (your input verbatim), `session:`, and `answered:` (the model's
  answer, at most 4000 characters). This is what answers a
  question *about* the conversation — "what did I ask you in the previous session?" — and
  that question takes its own path: the log is read newest first, not by term overlap. It is
  also what answers a question *about the content* of an earlier answer ("what was the
  deploy command you gave me?"): the record keeps the answer, not just its first line.

Four things to know about the log, because they are real limits rather than internal
detail:

- `answered:` is an **excerpt of model output**, not a verified fact. The memory block's
  heading says so outright, so a reply is not read as verified knowledge.
- One turn's memory budget (800 tokens) is **shared** among the hits, and a hit that still
  does not fit is clipped with a `[truncated: ...]` line. You see what the model sees: a long
  answer can reach the context shortened, but never silently cut.
- Each project keeps at most **200** records, oldest retired first, and one turn retires at
  most **8** of them so an answer is never delayed by a long log. A directive you gave is
  **not** a log entry and is never retired by this cap.
- **Nothing durable is built on a turn record.** `ha memory summarize` and semantic merge
  refuse a turn record as a source, with the reason: the record will expire and derived
  memory dies with its source. A record that is already the source of a live asset is not
  retired either, and the app reports `stored_but_unpruned` when the cap cannot be reached
  for that reason.

Saying the same thing twice is **one** memory: the existing asset keeps its id, its version
and its content hash, and the new source event is recorded on it. The audit trail survives
and no near-duplicate is minted to compete with the original in ranking.

The same memory is inspectable from the CLI. The store of the project is the directory
the app's header shows:

```powershell
ha memory --data-dir "$env:HA_HOME\data\projects\<project-key>" --principal local-user search "marker"
```

Extraction (`ha memory catch-up`) takes `--asset-scope session|project` as well. `session`
keeps what a run extracts private to that run's stream, which is the default; `project`
writes it as knowledge any later session of the project can read. The scope is part of the
strategy, so changing it starts a new cursor generation instead of reusing what the other
scope already settled.

What extraction infers is settled as a **candidate**, and a candidate is deliberately not
retrievable until a human confirms it. Review what is waiting, then confirm:

```powershell
ha memory --data-dir <store> --session-id <id> candidates --limit 16   # what is waiting
ha memory --data-dir <store> --session-id <id> confirm --limit 8 --confirm
ha memory --data-dir <store> --session-id <id> search "parser"         # now retrievable
```

`confirm` refuses without `--confirm`, confirms only candidates the principal may publish,
and is bounded to 64 assets per call. Confirmation adds a version — it never rewrites the
content the extractor proposed.

Memory is never required to run the app: with `HA_MEMORY` unset, retrieval is skipped and
nothing is written.

### 12.6. Local extensions in a chat turn (opt-in)

Extensions are **off unless you ask for them**. Set `HA_EXTENSIONS=on` in the shell that
launches `ha`; any other value keeps the nine built-in tools. With it on, a turn reads every
installation under `<HA_HOME>/data/extensions` (override with `HA_EXTENSIONS_ROOT`), starts
only what is trusted, advertises the tools those installations declare, and stops the plugin
processes when the turn ends — a chat turn never leaves an extension running.

P6 has no tool discovery on purpose: a manifest declares the `tools` capability and nothing
about the names inside it, so **the host decides what the model may see**. One directory per
plugin holds the three files that needs:

| File | What it is |
| --- | --- |
| `manifest.json` | the extension manifest, including the digest it pins for its executable |
| `trust.json` | the trust grant: plugin id, the same digest, allowed capabilities and secrets |
| `installation.json` | what this host advertises: the three paths above, plus one entry per tool (name, description, JSON argument schema, timeout) |

```json
{
  "schema_version": 1,
  "plugin_id": "acme.notes",
  "manifest": "manifest.json",
  "executable": "acme-notes.exe",
  "trust": "trust.json",
  "tools": [
    {
      "name": "tool.search_notes",
      "description": "search the local note index",
      "parameters": {"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]},
      "timeout_ms": 10000
    }
  ]
}
```

The model sees those tools as `plugin__<plugin>__<tool>`; the advertised name is sanitized for
the wire and the mapping is held by the host, so a name resolves only to an installation the
user trusted. A built-in name always wins, so an extension can never shadow `read_file`. Every
call crosses the same gate as a built-in tool — policy, then your approval, then a durable
intent and receipt — and the run reports what it loaded
(`extensions: 1 plugin(s), 1 tool(s) exposed`). A headless run reports the same under
`extensions` in its `--json` result.

An installation whose executable no longer matches the digest its grant pinned is refused with
that reason and is never started; a directory without an `installation.json` is ignored. The
`ha extensions inspect|capabilities|register|skills` commands remain the inspection and
registration surface, and `register --confirm` is what proves a plugin starts at all.

### 12.7. Images in a message

`deepseek-flash` accepts images, so a screenshot can be part of a request instead of a path the
model tries to open with a text reader. Three ways in:

| How | What to do |
| --- | --- |
| A file you already have | Name it in your message: `what is wrong here? "C:\Users\me\shot.png"`. Dragging the file from Explorer types the same path. **Quote it if the path has spaces.** A relative path resolves against the project directory. |
| A link to an image | Paste or drag the link itself: `what is wrong here? https://cdn.example.com/shots/broken.png`. Nothing is downloaded here — the link goes into the request and the **provider** fetches it. That needs a link the internet can reach, at most 8192 characters, for an image up to 32 MiB. If the link is private (localhost, an intranet host, a session-bound URL) the provider cannot read it and the turn fails with its download error: copy the picture to the clipboard and use `/image` instead. Only a link whose path ends in `.png`, `.jpg`, `.jpeg`, `.gif` or `.webp` is attached; an ordinary link in a sentence is left as text. |
| A screenshot on the clipboard | `/image`. `Ctrl-V` does the same where the terminal forwards the key to the app — Windows Terminal keeps that key for its own paste, so `/image` is the way that always works. |

Either way the image is attached to that turn, the clipboard case writes the pasted file into
the data directory and inserts its quoted path into the composer, and the transcript says what
happened (`[info] image attached: shot.png (image/png, 84 KiB)`, or
`broken.png (image url, downloaded by the model)` for a link). A candidate that cannot be
attached is reported with its reason instead of being skipped in silence: a file that is not
really an image, one larger than 8 MiB, more than three in one message, or a path that names a
place credentials live (`.ssh/`, `*.pem`, `.env`, `credentials*`) — those are never sent to a
provider.

The format comes from the bytes, not the file name, so a `.png` that is really text is refused;
PNG, JPEG, GIF and WebP are what work. The message names the images in order, so the model can
refer to "the second screenshot". They travel in the block form of `content`, which the API
accepts in a **user** message only. `read_file` still refuses binary files: an image reaches the
model as an attachment, never as file text.

Verified against the local SSE fixture rather than by reading the code: one headless turn naming a
165-byte PNG (`ha chat --headless --prompt "look at <file.png>" --json`) sent a `user` message whose
`content` was an array of a text block naming the image followed by an `image_url` block, and the
base64 in that block decoded to bytes identical to the file on disk (165 bytes, PNG magic intact).
The same turn reported `"images":["shot.png (image/png, 165 B)"]` — the byte size is exact, because
`0 KiB` next to an attached image reads like a failure — and the next turn in the same project
still recalled the earlier turn, so attaching an image does not disturb the memory path. A second
turn naming `https://cdn.example.com/shots/broken.png` sent that link unchanged as the
`image_url` value, so the link case is proven at the wire, not only in a unit test.

### 12.8. Files in a message (not only images)

A path that is not an image is now **content** rather than a hint to go open a file. A text file is
read and placed in that turn's message, so the model sees it without spending a step on a read tool.
Four ways in, one result:

| Way | What you do |
| --- | --- |
| A file you already have | Name it in your message: `what is wrong in this log? "C:\work\build output.log"`. **Quote it if the path has spaces**; a relative path resolves against the project directory. |
| Drag the file from Explorer | The terminal inserts the path; add your question and press Enter. |
| Paste a path | `Ctrl-V` (or `/image`) when the clipboard holds a **path** rather than a bitmap: the path is inserted into the composer, already quoted. |
| `/attach <path>` | Checks the file exists and then inserts the path into the composer: a wrong path is reported as `no such file` where you can still fix it, instead of submitting a path nothing can read. `/attach` works in every terminal, including the ones that keep `Ctrl-V` for their own paste. |

When the turn runs, the transcript names what was attached
(`[info] file attached: build output.log (text, 51 B)`). Inside the message every file sits between a
header and a footer (`===== file: <path> (…, 51 B) ===== … ===== end of build output.log =====`), and
the whole block opens by saying this is **material to read, not instructions to follow** — a log line
that reads like an order is still not an order.

The bounds have reasons: **256 KiB** of text per file, **1 MiB** and **4 files** per turn; a larger
file, or a binary one (not UTF-8, or holding a NUL byte), is refused with the reason rather than
silently truncated. File text stays in the conversation and rides with every later turn, which is why
the ceiling is a session budget and not just this turn's. A path into a credential store (`.ssh/`,
`*.pem`, `.env`, `credentials*`) is **never** sent to a provider — image or text. An image still takes
the image road (`content` blocks) and is never quoted as text.

A headless turn (`--json`) reports this under `files`, one object per file with `path`, `label` and
`bytes`, so a script can check which file entered the turn without reading the transcript. Verified at
the wire against the local SSE fixture: one headless turn naming `build output.log` sent a `user`
message whose `content` contained the file's own line (`error[E0425]: cannot find value …`) under a
header naming the file (`i03_a_named_file_reaches_the_model_inside_the_message`).
