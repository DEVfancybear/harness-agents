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
    --confirm src/lib.rs --surviving-copy backup-2026-01 --json
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

`forget` requires `--confirm` to equal `--source-id` exactly. A mismatch or an
empty value is refused with `retention_refused`, and nothing is recorded.

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

The pin exists to close one specific race: a backup promises to keep an artifact,
and a concurrent collection would otherwise delete it between the promise and the
copy. Because the pin is checked before any file is removed, and the pin lives in
the same database the collection just read, a pinned artifact survives. Backups
record their pins in the manifest, so the promise is auditable afterwards.

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
| `ha maintenance retain` | `invalidate`, `archive` or `forget` a source | `forget` without `--confirm` equal to `--source-id` |
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

Inside the app: `/help`, `/status`, `/config`, `/model`, `/new`, `/resume [number|id]`,
`/exit`. A gated action prints the action, working directory and scope, then waits for
`y` (run it once) or `n` (refuse); there is no implicit approval and no answer in time
counts as a refusal. A real model call needs `HA_PROVIDER_ENDPOINT`, `HA_PROVIDER_MODEL`
and a credential (`DEEPSEEK_API_KEY` or `HA_API_KEY`); without them the app still opens
in setup state and says what is missing, and it never fabricates an answer.

Setting the credential for the current shell:

```powershell
$env:DEEPSEEK_API_KEY = '<your key>'         # or $env:HA_API_KEY
$env:HA_PROVIDER_MODEL = 'deepseek-chat'      # provider dependent
ha                                            # opens the TUI; the status bar names the model
```

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
| `Enter` | Submit the request (an empty buffer is not submitted) |
| `Ctrl-J` | Insert a line break in the composer |
| `Alt+Enter` | Insert a line break (measured on this Windows Terminal's ConPTY; see the limits below) |
| Multi-line paste | Keeps its newlines and never submits; the whole block is **one** request |
| `↑` / `↓` | Single-line buffer: history; multi-line buffer: move by row |
| `←` `→` `Home` `End` | Move by character |
| `Ctrl-A` / `Ctrl-E` | Start / end of the current row |
| `Ctrl-U` / `Ctrl-W` | Erase to the row start / erase one word |
| `Tab` | Complete a slash command when there is exactly one candidate |
| `Esc` | Close a panel or clear a suggestion; it never cancels a running turn |
| `Ctrl-C` | Running: cancel the turn · idle: clear the buffer |
| `Ctrl-D` | Empty buffer: leave |
| `Ctrl-L` | Repaint the bottom area without clearing the scrollback |
| `y` / `n` | Answer the approval panel (or type `yes`/`no` and press Enter) |

The approval panel shows the action, workspace, scope and a **countdown** to the gate's
deadline; when it expires the action does **not** run and the panel closes.

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
