# Operator guide — running and recovering the harness

Ngôn ngữ: [Tiếng Việt](OPERATOR_GUIDE.vi.md)

This guide is written for the person who has to run, back up, restore, migrate and
retire a harness data directory. It states the commands that exist in this
release, what each one refuses to do, and the limits an operator must not
discover the hard way. For how the pieces fit together, see
[ARCHITECTURE_OVERVIEW.en.md](ARCHITECTURE_OVERVIEW.en.md) and
[PLUGIN_ARCHITECTURE.en.md](PLUGIN_ARCHITECTURE.en.md); for the TUI,
[TUI.md](TUI.md).

## 1. What this release is

The harness is a single-host, foreground coding agent runtime. One writable host
owns a data directory at a time; the ownership is an operating-system file lock
plus a fencing generation recorded in the database, not a network service.

What that means in practice:

- **Maintenance has no daemon.** Every maintenance action below is a foreground
  command an operator runs; nothing retries or collects on its own. Interactive
  sessions do run in background workers ([12.7](#127-background-agents)).
- **There is no server to connect to.** Remote MCP endpoints and OS-level
  sandboxing are declared unsupported in the release matrix; transport isolation
  is not a sandbox.
- **Release artifacts come from the release script.** Windows builds are exercised
  by the test gates; `scripts/New-HaRelease.ps1` builds a candidate bundle with
  checksums ([BUILD_AND_RELEASE.md](BUILD_AND_RELEASE.md)). The release matrix still
  reports published artifacts as unverified, and nothing is code-signed.
- **Linux support is pending.** Its CI job runs for visibility and does not gate a
  push; the release matrix reports Linux as `unverified`.
- **Provider credentials are not exercised.** The gates never call a paid model
  API, so no release claim depends on one.

The machine-readable statement of the verification state is
`ha maintenance release-matrix --json`.

## 2. Diagnosing a data directory

```console
ha maintenance doctor --data-dir <DATA_DIR> --json
ha maintenance doctor --cwd <PROJECT> --json
```

Every `maintenance` subcommand that takes `--data-dir` can instead take
`--cwd <PROJECT>` (before or after the subcommand), which resolves the project's
store the way `ha chat` does. This is not the same as `ha doctor` ([12.1](#121-agent-tools)),
which audits a repository's readiness for an agent, not a data directory.

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

## 6. Tombstones

```console
ha maintenance tombstones --data-dir <DATA_DIR> --json
```

A `tombstone` is the durable record that a source was deliberately forgotten,
together with every external or backup copy that may still contain its data. The
source-level `invalidate`/`archive`/`forget` retention actions are not part of
the current command surface, so this build does not write new tombstones; older
store records survive backup and restore unchanged, and
`ha maintenance tombstones` lists them.

## 7. Garbage collection

```console
ha maintenance gc --data-dir <DATA_DIR> --grace-seconds 604800 --dry-run --json
ha maintenance gc --data-dir <DATA_DIR> --grace-seconds 604800 --json
```

Garbage collection removes artifact bytes, and it removes only an artifact that
is simultaneously:

1. **unreferenced** — no receipt or tool artifact scope points at it;
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
- **verified_cases**, **unverified_checks**, **out_of_scope** — the 32 continuity
  and plugin cases this release exercises, the checks that were not run and why,
  and what this release explicitly does not do.

The `verdict` is the honest one-line summary: a failing platform makes the
release "not release-ready", an unexercised platform makes it "partially
verified", and an unmeasured benchmark is named as unmeasured even when every
platform is green.

## 9. What an operator must not assume

- No background process keeps the store tidy. If you do not run `gc`, nothing is
  collected; if you do not run `backup`, nothing is protected.
- A restore is not a switch. It produces a candidate directory; activating it is
  an operator decision with its own consequences.
- A tombstone is local and durable, not global. Read `surviving_copies` before
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
| `ha maintenance tombstones` | List forgotten sources and surviving copies | Never |
| `ha maintenance gc` | Collect unreferenced, unpinned, old artifacts | Never; it reports what it retained and why |
| `ha maintenance migrate-copy` | Migrate a store on a copy | The destination is occupied; the source has no store |
| `ha maintenance release-matrix` | Report platforms, capabilities and benchmark honesty | Never; it reports what is unverified |
| `ha bug-report` | Gather the diagnostics, workers' logs, store records and harness files of a window into one zip with a `SUMMARY.md`, secrets redacted ([12.12](#1212-diagnostics-and-bug-reports)). Use this bundle to debug | The window is not a number followed by `s`, `m`, `h` or `d` |
| `ha diag path`, `ha diag timeline` | Show where the diagnostics log is; print the merged timeline | Never |
| `ha maintenance support-bundle` | Write a bounded, redacted bundle (metadata, counts, schema revisions, correlation references; with `--config <FILE>` the redacted config) for a host you cannot reach; every redaction is counted in its manifest | The `--into` directory already holds files |

Other top-level commands are covered where they belong: `ha`/`ha chat`/`ha exec`
([12](#12-interactive-and-headless-ha), [12.4](#124-automation)), `ha agents|list|attach|send|abort|stop|rename|shutdown|schedule`
([12.7](#127-background-agents), [12.8](#128-more-from-prime-agent)), `ha mcp` ([12.3](#123-config-v2-permissions-and-hooks)),
`ha package` and `ha config` ([12.9](#129-packages-resources-and-the-rest-of-prime-agent)), `ha model` and `ha prompt` ([12.8](#128-more-from-prime-agent)), `ha doctor` ([12.1](#121-agent-tools)).
The remaining groups - `init`, `sessions`, `status`, `input`, `plugins`, `run`, `resume`, `continue`,
`context`, `session`, `code`, `sandbox`, `tasks` and `extensions` - are lower-level inspection and
fixture commands for development and gates (`ha <group> --help` lists them); an operator does not need them.

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
nothing to a package registry, and it does not edit your `PATH` unless you pass
`-ModifyUserPath`. When the install directory is not on `PATH`, it prints the exact
directory to add instead of changing your profile behind your back; with
`-ModifyUserPath` it touches only the **User** PATH (appended once, de-duplicated,
never the Machine or process PATH) and says a new terminal is needed to see it. It
also prints the installed path and the `ha --version` output, so you can see which
binary you are about to run.

The script also installs exactly the artifact Cargo reports, verifying the digest
and `--version` of the staged copy before replacing anything and keeping the old
file as a rollback; writes an `ha.install.json` manifest beside the binary (version,
sha256, source, build commit, owned files) so update and uninstall touch only its
own files; classifies a failed replace (`in_use`: close the app and retry, the script
kills no process; `access_denied`); and warns when another `ha` comes earlier on
`PATH` without deleting it. `-SelfTest` checks these rules without installing anything.

Both routes build from the same source tree; only the cargo profile differs. If you
want the exact artifact the tests exercised, use the default release profile.

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
  user data (no `purge` route exists); remove the config and data directories by hand if you want
  them gone.

Two things to know after installing:

- **Maintenance is foreground.** No daemon runs it: a backup, a collection or a
  retention action happens exactly when you ask for it and at no other time. (An
  interactive session keeps running in a background worker after its terminal
  closes; see [12.7](#127-background-agents).)
- **A fresh directory is a valid target.** `ha maintenance doctor --data-dir <DIR>`
  works on a directory that holds no store yet and reports it as uninitialized,
  which is the state this binary may create a store in. It does not pretend a
  store already exists, and backing such a directory up is still refused.

Building from source stays available for development:
`cargo build -p harness-cli --bin ha --locked` writes `target/debug/ha`, and
`cargo run -p harness-cli --bin ha -- <args>` runs it without installing anything.


## 12. Interactive and headless `ha`

`ha` or `ha chat` opens chat in a terminal. `ha chat --cwd <project>` selects a workspace; if stdin/stdout is not a terminal, interactive launch exits 2 rather than waiting. `ha chat --plain` uses line output, and `ha chat --fixture` uses a local fixture. `/session` (also `/status`) shows the project, provider, data and setup state. `/login` follows prime-agent: DeepSeek, OpenAI, Anthropic, OpenCode Zen and OpenCode Go take an API key typed into a masked prompt; ChatGPT Plus/Pro signs in in the browser (OAuth with PKCE on `localhost:1455`; when the browser is on another machine, paste the final redirect URL). Credentials are saved per provider in `auth.json` under the private data directory and win over the provider's environment variables (`DEEPSEEK_API_KEY`, `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `OPENCODE_API_KEY`); `/logout <provider>` removes one. Values never appear in status or the transcript. `/model` lists the models of the providers you are logged in to, from prime-agent's model catalog (a snapshot ships with the app and is refreshed daily) plus the models DeepSeek and OpenCode list themselves, so a release newer than the catalog appears too; each model's thinking levels come from its catalog entry, and `/effort` offers only those; the choice is saved in `selection.json` beside the user config. Anthropic's Claude Pro/Max sign-in is not offered: it only works by presenting the app as Claude Code, so Anthropic uses an API key.

### 12.1. Agent tools

The 18 core tools cross the host policy and receipt gate. Skill tools (`list_skills`, `activate_skill`, `read_skill_file`), web tools (`web_search`, `web_fetch`), the Python REPL (`python_repl`) and, without the REPL, `mcp__<server>__<tool>` appear when available.

**MCP, as prime-agent reaches it.** With the Python REPL available, MCP servers are not native tools: the kernel's pre-imported `mcp` object (prime-agent's `rlm.mcp`, vendored) opens a configured server itself - `await mcp.list_tools("<server>")`, `await mcp.call_tool("<server>", "<tool>", arguments)`, `await mcp.list_connections()` - and the prompt lists the enabled servers. The servers are the ones `ha mcp add` configures; the host hands the kernel each server's configuration in prime-agent's shape (`secret://NAME` becomes an `{"env": "NAME"}` reference; `streamable_http` becomes `http` with `bearerTokenEnvVar`). The `mcp` Python package comes with the kernel venv. prime-agent's service catalog is not ported (plugin listings are empty); its OAuth sign-in is, as `ha mcp login` ([12.8](#128-more-from-prime-agent)). Without the REPL, or when a message attaches a server's resource with `@server:uri`, the servers are connected natively as before; `/mcp` says which route each server takes.

**Web.** `web_search` searches the web: Google through Serper when `SERPER_API_KEY` is set or `/login serper` saved a key (a free key at serper.dev), otherwise DuckDuckGo, which needs no key. `web_fetch` opens one http(s) page and returns readable text plus its links, in windows continued with `start_index`. Local and private addresses are refused, including as a redirect target; binary files are refused. `web_search` runs without a panel (it sends only the query); `web_fetch` asks, because a URL can carry data out - answer `a` to allow it for the turn, or use `full-auto`. `HA_WEB=off` removes both tools.

**Python REPL.** The `python_repl` tool (named `ipython` before, as in prime-agent; a call by that name, or by `python`, still reaches it) runs cells in one persistent Python kernel - prime-agent's own runtime, vendored under `crates/harness-cli/python` and written to `<data-dir>/runtime` on first use. Top-level `await` works; variables and imports persist across cells and turns; a kernel that dies is restarted on the next call and the result says the old state is gone. `bash('cmd')` starts a command in the background and returns a handle (`tail`, `output`, `poll`, `kill`, `await`); a command left running that finishes with nobody reading it is announced as prime-agent's `[bash-done pid:... exit:...]` - steered into a running turn, or a turn of its own when idle; on Windows it runs in Git Bash, found in its default location or set with `HA_REPL_SHELL`. A cell runs for at most 10 minutes, then it is interrupted; on Windows only a cell waiting at an `await` can be interrupted, so a cell stuck in synchronous code restarts the kernel. Running Python is running code, so each cell asks like `run_shell`, unless the mode is `full-auto`. The kernel starts with the project root as its working directory and the user's environment. It needs Python 3.11+ (`HA_PYTHON`, else `python3`, `python`, `py -3`); without one the tool is not offered. `HA_REPL=off` removes it.

With delegation available, the kernel's `rlm` object runs children as prime-agent does: `await rlm.spawn('task', name='worker')` starts a child and returns at admission; `await rlm.collect([...], timeout_ms=...)` waits for their answers; `rlm.list_subagents()`, `rlm.delete_subagent(...)` and `rlm.find_models()` work as in prime-agent. A child belongs to the session, not to the turn that started it: the turn may end while its children keep working. When a child settles and nobody is waiting for it, the parent is told with prime-agent's notice - `[child-failed child:<name>]`, `[child-exited: cancelled child:<name>]` or `[child-exited: no-reply child:<name>]` with its last answer - in a turn of its own when the parent is idle, or after the running turn ends (never steered into it). Ctrl-C stops the parent's turn, not its children; `/agents stop <name>` (or `/agents stop` for all) stops them, and `/new` or a resume into another conversation stops them quietly. A child runs on the parent's model unless `rlm.spawn(..., model='provider/id')` names another catalog model with a credential, else prime-agent's `subagentDefaultModel` in `settings.json`, else `[agents] default_model`; a model that cannot be used fails the spawn. `/subagent-model` shows that model and where it comes from; `/subagent-model <provider/model>` (Tab or Enter lists the models) saves `subagentDefaultModel` for the next child, and `/subagent-model inherit` removes it. `/subagent-login <provider>` logs the delegated children in with an account of their own - an API key, or the ChatGPT browser sign-in - kept in `subagent-auth.json` beside `auth.json`: a child uses its own login for a provider when there is one and the main model's otherwise, and the main model never uses the children's. `/subagent-logout <provider>` removes it. A child runs at the parent's thinking level unless `rlm.spawn(..., thinking='high')` names one - as in prime-agent, a level the child's model does not offer fails the spawn - or `/subagent-effort <level>` saved `subagentDefaultThinking` in `settings.json` (clamped to the nearest level the child's model offers; `/subagent-effort inherit` removes it). Every message the agents of a session send each other (`agent_message`, `rlm` messaging) shows in the conversation as prime-agent's row, `◆ agent message · <from> → <to> · <start>`, with the whole message in the details and expanded modes (Ctrl-O); `/agents messages` lists every exchange of the session, whole and in order. `rlm.create_session` starts a separate top-level agent ([12.7](#127-background-agents)).

**Factories (`rlm.factory`, opt-in).** prime-agent's factory, vendored with the kernel: a stored state machine of child agents - entry states, guarded transitions, joins, bounded re-entry, `foreach` and budgets - that the kernel runs with `rlm.spawn` and `rlm.collect`. The model writes one with `rlm.harness.create_factory(title, content, dag=... or machine=...)`, which validates the spec before anything is stored, then `await rlm.factory.run('<id>')` starts it and returns; `status`, `stop`, `resume`, `graph` and `watch` follow it, and `rlm.factory.help()` is the full authoring guide. Three machines ship with it (`builder`, `pr-manager`, `review-sweep`), runnable by name. A run lives in the kernel's memory, so it does not survive a kernel restart; its children do. It is off by default, as in prime-agent: `/factory on` turns it on (`factory.enabled` in `settings.json` beside the user config), `/factory off` turns it off and `/factory status` says which; until it is on, every call but `help()` says so.

**Python skills.** prime-agent's skills ship with the app (`.agents/skills`, MIT; see `.agents/PRIME-AGENT-SOURCE.md`): `edit`, `websearch`, `attach_image`, `goal`, `compact`, `refine`, `agent_message`, `agent_observe`, `rlm_heartbeat`, plus the `mcp` and `skill-creator` guides. A skill with a Python package is imported into the kernel by its name when the kernel starts - `await edit(path=..., old_str=..., new_str=...)`, `await goal.complete()`, `await compact.run()` - and listed in the prompt with its `python_import`. A skill whose import fails is replaced by a stub that says why, and the first cell reports it. The host answers the skills' requests: `goal.*` drives `/goal` (a goal the model creates is carried like one you set, with its token budget), `compact.run` schedules `/compact` for the end of the turn, `model.info` names the model, `agent_observe` reads the session's children, `rlm_heartbeat` keeps recurring prompts for the session (`every 5m` by default; a `steer` one reaches a running turn through the `/steer` inbox, a `follow_up` one waits for it to end), and the images `attach_image` loads are shown to the model with the tool result. `agent_message.send(message, receiver_role='child', receiver_name=...)` (or `'all'`) messages running children; a child messages its parent or a sibling with its own `agent_message` tool, and leaves a one-line `progress_note` its parent reads in `list_subagents` and `/agents`. As in prime-agent a message holds at most 16 384 characters, a sender gets three at once and one more each second, and it arrives as `[agent-message from <relationship>:<name>]` at the recipient's next step - or, for an idle parent, as a turn of its own. A message to a child that has finished is refused (a finished child is not woken again).

**Kernel venv.** As prime-agent does, the kernel runs in a venv under `<data-dir>/kernel-venv`, built with `uv` on first use - Python 3.11, `dill`, prime-agent's default packages (requests, httpx, pyyaml, tomli, python-dotenv, pandas, numpy, scipy, beautifulsoup4, lxml, pydantic, tyro) and `pillow` - and rebuilt when that list changes. ha never installs `uv` itself: without it the kernel runs on the system Python, the first cell says so, and skills whose packages are missing (`websearch` needs httpx, `attach_image` needs pillow) are reported unavailable. `HA_PYTHON` names an interpreter to use as it is.

**Skills** follow the Agent Skills layout (a directory with `SKILL.md` plus its own files). They are found in the bundled set, `<config-dir>/skills`, `~/.agents/skills`, every directory listed in `HA_SKILL_PATHS` (separated like `PATH`, e.g. `~/.claude/skills`), and - for a trusted project - `.harness/skills` and `.agents/skills` in the workspace and each parent up to the Git root. The system prompt lists each skill's name, description and location in an `<available_skills>` block, the way prime-agent does; the model activates one by name (the digest from `list_skills` is an optional pin, accepted with or without `sha256:`). Activation adds the instructions plus the skill's directory and file list, and `read_skill_file` reads those files - only inside that skill. The three skill tools only read the trusted catalogue, so they run without an approval panel; a deny rule still blocks them. `disable-model-invocation: true` in the front matter hides a skill from the model; `/skill:<name>` still runs it. Written at the start of a message, `/skill:<name> [args]` sends the skill with the rest as the request, as prime-agent does; written inside a message (`... using /skill:<name>`), it sends that skill with the whole message as the request.

**Learned skills (after tigerless-labs/autoharness).** Besides prompt notes, memories, Python skills and subagent specs, a refinement can propose `skillFiles`: `create`, `update`, `patch` (an `old_string` that occurs once), `remove_file` or `delete` of a `SKILL.md` skill ha writes for itself, each with a `reason` and a verbatim `evidence` quote. The model never writes those files; the host lints each intent in memory and lands only a clean one, support files first and `SKILL.md` last, each through a temporary file and a rename. The lint refuses a front matter whose `name` is not the skill's, a description over 200 characters, a body over 30 non-blank lines, support files outside `references/`, `templates/`, `scripts/` or `assets/` or not referenced from `SKILL.md`, a referenced file that does not exist, `TODO`/`FIXME`/`TBD`, secrets, explicit harmful content (a download piped into a shell, "ignore previous instructions", destructive or persistence commands), a global skill that names the workspace's paths, a name another skill already has, and any change to a skill ha did not write. A `delete` with `absorbed_into` must name another live learned skill. What was refused is listed in the refinement row with its reasons. A project skill lands in `.harness/skills/<name>` (trusted projects only), a global one in `<config-dir>/skills/<name>`; ha's own skills carry `.ha-learned.json` and an append-only `.ledger.jsonl` (reason and redacted evidence per change). `/refine --rollback <id>` undoes the skill changes too. Use is counted in `<data-dir>/skill-usage`: an activation or `/skill:<name>` is a use, `read_skill_file` or a `read_file` in the skill's folder is a view. A skill is on probation for 100 requests in its project (300 for a global one); ending it with no use and no view archives it, and past 20 global or 50 project graduates the least used go first. Archiving moves the folder to `.archive/` in its layer; moving it back revives it. `HA_SKILL_LIFECYCLE=off` keeps them all. The automatic review is due after 25 turns or 50 tool calls, whichever comes first, and runs beside the conversation instead of at the end of the turn. A session that ends with at least 5 turns or 10 tool calls since its last review does not make you wait on exit: it leaves the review in the project's data directory (`refine-pending/`), and the next turn in the project runs it in the background on that session's conversation (`auto-refine: reviewing the last N turn(s) of an earlier session`). A refinement reads the conversation the way prime-agent serializes it: each turn's question, the model's text, its tool calls numbered `#N` with their arguments, and each call's result (its at most 2,000 characters: the last 500, the head filling the rest, and a line counting what was cut), so lessons come from what the tools did as well as from what was said. In a linked Git worktree, project skills are written to and read from `.harness/skills` of the main checkout, so what is learned there outlives the worktree and every worktree shares one library. Every 250 requests in the project (or `/refine --curate [instructions]`) the curator reads the whole learned library and folds narrow skills into class-level ones: it patches or creates an umbrella and deletes the absorbed skills with `absorbed_into`, through the same checks. The library is copied to `<data-dir>/skill-snapshots/<time>/` first (the five newest are kept), and the pass is recorded as a global refinement that `/refine --rollback <id>` undoes. `/learn [focus]` asks for this now: a refinement told to keep what the conversation taught as learned skills (corrected approaches, techniques, fixes, setup steps, how you want a kind of work done), comparing first and patching what exists. An intent the checks refuse is not dropped: its findings go back to the model once, and the fixed intent goes through the same checks (the row says `Refused learned skill ..., sent back for repair`). A review remembers the last session it read (`review_watermark` in the conversation's local harness directory) and marks the turns before it as already reviewed, so the next one learns from new turns instead of the same ones again. The learned skills a turn activated or read are passed to the next review and refinement as the skills in play, to be patched first when the conversation shows them wrong; the refiner and the curator also see each skill's use (`used N, viewed M in R requests, on probation`), which the curator may use only to rewrite the description of a skill nobody reaches. When a turn that used a learned skill then ran the harness's verification checks (`goal_complete`, a feature's `verify`), how the last run ended is counted for that skill (`checked_pass`, `checked_fail` in `skill-usage`) and shown as `checks after use: N pass, M fail`; repeated fails make the skill a candidate for a patch or a split into conditional variants, never a reason to delete it, and the lifecycle does not use them. After Mengram, whose procedures evolve from their failures, the automatic review also judges each learned skill in play from the conversation - `worked` or `failed`, with a verbatim quote, and for a failure what the skill took for granted that turned out false - and counts it (`worked`, `failed`, the latest five `failures`); the refiner and the curator see `judged after use: N worked, M failed; last failure: ...` and patch the skill to check that first. A turn that ran no check counts nothing. The system prompt names the learned skills under the skill list so the model checks them first, and `/skills` lists the recent learning runs - what landed and what was refused - including those that ran in the background.

**Definition of done: checks, an independent verifier and the feature list (after walkinglabs/learn-harness-engineering).** A project says what "done" means as commands, in `.harness/config.toml` or the user config - like hooks, never from an untrusted project or `config.local.toml`:

```toml
[verify]
timeout_seconds = 300   # per check, unless the check names its own
judge = true            # an independent verifier judges a goal (the default)
[[verify.checks]]
name = "types"
command = "cargo check --workspace"
[[verify.checks]]
name = "tests"
command = "cargo test --workspace"
hint = "Fix the failing test or the code; never delete or weaken a test."
```

The harness runs the checks itself, in order, stopping at the first failure, in the shell and scrubbed environment the model's shell tool gets. A failure reaches the agent as what failed, the command's output (its first 1,500 and last 4,500 characters) and how to fix it (`hint`, else "fix the cause, do not weaken the check"). The checks run when the model calls `goal_complete`, when a feature is verified, as the gates of `/autonomous on` when no `--gate` is given, and on `/verify`, which shows each check's result. The `delegate` tool has a third role, `verifier`: a child with a fresh context, in the parent's workspace, that is neither offered nor allowed `write_file`, `edit_file` or `apply_patch` (in any mode), cannot delegate, and ends its answer with `VERDICT: PASS` or `VERDICT: FAIL` after findings with file and line; `[routing] verifier = "provider/id"` picks its model (else the children's default). `/review [focus]` asks the model to have one judge the work in the workspace and report its verdict unsoftened. The feature list is `.harness/features.json`, worked through the `feature` tool: `add` (a title, the behavior, the shell commands that verify it), `start` (one feature is active at a time; starting another while one is active is refused), `block` (with the reason), `update` (not for a passing feature, which keeps the definition it was verified with), `list` and `verify`. Only `verify` makes a feature passing: the harness runs the feature's commands - or the project's checks when it names none - and on a pass records the evidence (`at`, `commit`, which checks ran, `verified_by: "ha"`). A feature the file marks passing without that evidence is listed as `passing?` and counted as unverified, so editing the file does not finish work. Commands the model wrote for a feature ride in the request and ask for approval like a shell command (a list changed after the approval is refused); the project's own checks run without asking. Each turn carries where the list stands (counts, the active feature, the rules), and `/features` shows it with the verified completion rate.

**Session lifecycle, nested instructions, the repository audit and learned checks.** The first prompt of a session (a launch, `/resume`, `/new`, after `/compact`) carries a brief the harness gathers itself: the five latest commits, what is uncommitted, and the handoff the last session left in `.harness/progress.md` (its first 6 KB). It asks the agent to check the baseline (the project's checks pass) before new work only when the prompt concerns the project: one of its terms, split the way memory search splits them, matches a tracked path, one of the 50 latest commit subjects, a pending change or a handoff line, and is found in at most half of them. Otherwise the brief leaves the call to the model: change the project, check the baseline first; a greeting or a question, answer directly. That covers a task written in words the project does not use, such as another language. No language or word list decides. `/handoff [focus]` asks the agent to write that file in a fixed shape - current state, verification, done, in progress, blocked, decisions and why, the next step, the commands - and `/handoff --reset [focus]` then starts a fresh conversation that continues from it, the context reset to use when a long conversation runs out of room. Leaving (`/quit`, Ctrl-D) names what would trip the next session: uncommitted files, a feature still active, a handoff missing or older than the latest changes. Instruction files load from the project root down to the working directory as before; each directory's `AGENTS.local.md` (or `CLAUDE.local.md`) follows its shared file, so personal rules come later and win. An `AGENTS.md` below the working directory is added to the result of the first call that reaches its directory - a read, an edit, a listing - once per conversation and at most 16 KB per turn, shown under the result rather than as hook feedback. `/doctor` (and `ha doctor [--cwd DIR] [--json]` outside the app) audits the repository on the five subsystems: the entry file exists, stays within 200 lines and its links resolve; the project is trusted and its skills, hooks and MCP servers; Git, a lock file beside each manifest and a toolchain pin; the handoff and whether it is older than the last commit, the feature list, more than one active feature, passing features without evidence; `[verify]` checks, the judge and CI. Every shortfall comes with its fix, and the weakest subsystem is named as the place to start - only failed runs show the real bottleneck. A refinement (`/refine`, `/learn` and the automatic review) may also propose `checks`: a mistake the conversation showed that a command can detect, as a shell line that exits non-zero when the problem is present and the hint the agent reads when it fails. A proposal is linted (a kebab-case name, one command line up to 300 characters, a hint of 20-400 characters, a verbatim evidence quote, no secrets, none of the harmful shapes learned skills are refused for, not a copy of a project check or a waiting proposal) and waits in `.harness/proposed-checks.json` of the main checkout - a trusted project only. Nothing proposed runs: `/checks` lists them, `/checks accept <name>` adds one to `[[verify.checks]]` in `.harness/config.toml` (refused if the file would no longer load) and `/checks reject <name>` drops it.

**Speed of the file tools.** Reads, listings, globs and searches share one walk of the workspace: it runs on all cores, never opens `.git` or other protected folders, and is kept until something changes - this process's own writes, a process or hook it ran, and (through a file watcher, `HA_FILE_WATCH=off` to disable) any other writer; without a watcher a walk is trusted for one second. A walk stops at 200,000 files or after 10 seconds and the tools return what it found, marked truncated, instead of failing. `search_text` searches whole files with the regex engine's literal prefilters on all cores, with the same results and the same cut at 512 matches or 50 KB as before; its `context_lines` are shown around each match (`path-LINE- text`). A workspace of 5,000 files or more gets an n-gram index, built in the background on its first search, so a search reads only the files that can hold a match (`HA_SEARCH_INDEX=always` or `off`). The workspace fingerprint of a write reads `HEAD` and the index from `.git` instead of starting `git`, and the output of a read is not flushed to disk. While the model streams a response, each finished read starts its work before the response ends (`HA_PREFETCH=off` to disable); the gate still runs every call in order. A batch runs in segments: the calls between two that must run alone (writes, processes, questions) run side by side, and the reads after a write see it. `read_file` takes `paths` (up to 10 files, read in parallel, one result); `edit_file` takes `edits` (up to 50, applied in order and written together), and after the exact and normalized matches it finds whole lines whose indentation is wrong and re-indents the replacement. With hashline editing - `"hashline": true` in the global `settings.json`, or a list of model patterns as `allowedModels` takes them, or `HA_HASHLINE=1|0` - `read_file` and `search_text` show each line as `LINE#HASH: text` and an edit may name the lines it replaces (`start`, `end`) instead of quoting them; a line that changed since it was read refuses the edit and shows its current anchors. `cargo run --release -p harness-tools --example file_tools_bench` times every file tool through the full gate.

**Speed of the store.** Every tool call writes about ten small commits to the project store's write-ahead log, and copying that log back into the database (a checkpoint) is the most expensive thing the store does: on a Windows disk with antivirus scanning, 0.4 to 1.4 s at a time. A second connection does that copy once a few hundred commits have gathered and the writer has gone quiet - in a turn, while the model is thinking - and the writer itself checkpoints only past 16 MiB of log. A run whose model answers at once leaves the copy for the store's close. After each turn of the app, the task's journal is indexed in the background, so the next `history_search` does not pay for that turn's entries. `ha exec` reads the prompt's Git facts while the store opens. `cargo run --release -p harness-cli --example turn_bench -- --model-ms 600` times what the host adds to a turn; `--ha` compares another build.

**Speed of the history tools.** `history_search` brings the task's journal index up to date before it searches, and a session whose last entry is already indexed costs nothing: it is skipped in the same query that lists the task's sessions. New entries are indexed a session at a time, up to 128 per transaction, with their terms written many rows per statement. A hit carries only the head of its source for the preview, and `history_read` takes only the requested page out of the database. `cargo run --release -p harness-tools --example history_bench -- --sessions 100` times indexing, search and reads. A search with `scope: "project"` first indexes every conversation of the project the index has not reached yet; after that it costs the same as a task search.

**Memory (`memory`).** The model keeps durable facts, the user's preferences and decisions (memory entries) and narrow rules (prompt notes) in the continual harness state: `<data-dir>/harness/harness_state.json` for every conversation, `<data-dir>/sessions/<task>/harness/harness_state.json` for one. A fact about one project (`scope: project`) is kept in the global file tagged with the project and reaches that project's conversations only: another project's turns, digests and searches never see it. Every turn carries a digest of the few entries ranked highest for the request, cut short; the `memory` tool searches the rest (an unaccented query finds accented entries), reads one whole, and writes: a new entry needs `scope` (`global` or `local`), an update or a delete needs the `expected_version` a read returned, a title already taken is refused with the way to update it, and an entry is at most 4,000 characters and never a secret. A new entry that reads as one already kept - the share of their words, each weighed by how rare it is among the entries, from 60% - is refused with that entry's id and version, unless `distinct: true` says it is a different fact; one that shares less but still much comes back with those entries as `related`, so one it replaces or contradicts gets updated. Every write records where it came from (`origin`: the conversation, and the memory tool, `rlm.harness` or a refinement). An update or a delete keeps what it replaced in `memory_history.jsonl` beside the state (the latest 500 changes): `history` lists an entry's earlier versions, a deleted one's too, and `restore` brings one back as the next version, or a deleted entry as it was. An id is made from the title: an update with a new title gives the entry the new title's id (unless another entry has it), the old id still finds it, and its history follows it.

**Harness refinements off (`/refinements off`).** prime-agent's global master switch, `harnessRefinements` in `settings.json`, on by default. Off, no saved memory, prompt note or skill entry reaches a turn (no digest, no `memory` tool, children included), the kernel's `rlm.harness` refuses (and with it `rlm.factory`), and nothing is refined: no automatic review, `/refine` and `/learn` refuse (`/refine --history` still shows), and `refine.run()` says so. What is saved is kept; `/refinements on` brings it all back, and `/refinements` says which it is. The Python kernel's `rlm.harness` no longer writes the file itself: each of its calls is a request the host answers, under the same lock (`harness_state.json.lock`) the tool and `/refine` take, so no writer overwrites another and one validator checks every entry. The tool is allowed in every permission mode, since it changes only ha's own state; a `memory()` deny rule turns it off. Skills and subagent specs stay with `rlm.harness`.

**Pinned memory, consolidation, compaction.** An entry written with `pinned: true` (the user's language, how to address them, a hard rule) is shown whole at the top of every turn's digest, whatever the request; pinned entries share 4,000 characters, a pin past that is refused with the way out, and `pinned: false` unpins. Once the global memory holds at least 12 entries and has changed by 15 (creates, updates, deletes) since its last consolidation, a background pass after a turn merges duplicates, lets the newer entry win a conflict, removes the stale and the temporary and keeps pinned entries pinned (after CowAgent's Deep Dream); its edits go through the same checks as a refinement and are recorded as one, so `/refine --rollback <id>` undoes them. `/refine --curate` runs it now, with the learned-skill curator. A single turn whose own steps outgrow the window - a long goal turn reading many files - has its earlier steps summarized into its first step while its newest steps stay whole; the summary is kept for the turn's later steps and only extended when the kept steps double, so it costs a model call now and then, not every step. A turn that compacted the context brings the automatic review forward once there has been a session end's worth of work (5 turns or 10 tool calls) since the last one.

**Self-improvement: the automatic review, its history, and stale state.** After a turn, an automatic review reads the conversation and refines when it holds something worth keeping: every 25 turns or 50 tool calls (`autoRefine.turnInterval`), when a session ends with work not yet reviewed, and earlier after a turn that compacted the context or reached a milestone - a goal completed, coders' work merged with `integrate_children` - once there has been 5 turns or 10 tool calls of work since the last review. Like CowAgent's Self-Evolution it reaches past the conversation: a memory edit it marks global goes to the global store as a refinement of its own (its id ends in `_global`), so each part rolls back on its own, and substantive knowledge the agent did not save becomes a new knowledge page (project scope unless shared; never over an existing page; a rollback deletes the page only while it is as written). A reply with no JSON in it is asked for once more. As in Google Research's Harness2, what the review reads (the conversation, tool results, files, fetched pages) is data, not instructions: an instruction planted there never becomes a memory, a skill or a knowledge page unless you said it yourself; every edit carries a verbatim quote as its evidence and a confidence, a global edit without its quote stays in the conversation, and a lesson seen once stays local; verified facts are written as facts labelled with where they hold, procedures for the class of work, and repeated deterministic work goes into a script a learned skill carries. `/refine --history [N]` lists the latest refinements of the global store and this conversation - when, what and why, every change with its reason, and the id `/refine --rollback` takes - without calling a model. The memory curator aims for about 30 global entries, since each one reaches every conversation, and records the count before and after. Every entry says how old it is (`updated 3d ago`) in the digest, in search results and in a read: an entry that records a state (progress, a status, a count) goes stale, and the review updates or deletes it when the conversation shows it changed. Global memory never holds task state such as commit ids, test counts or a done/in-progress status; those stay local, or become knowledge. Memory search takes `queries`, up to four other wordings beside `query` (synonyms, a translation), fused by rank, so a request in one language finds an entry written in another. A project's knowledge library (`.harness/knowledge`) keeps itself out of Git with a `.gitignore` of its own, so it never makes the checkout dirty for a coder.

**Speed of a turn.** One HTTP client per timeout pair serves every model call of the process, so a turn's first call reuses a warm connection. MCP servers are started together and kept between turns while their configuration, the workspace and their tool lists are unchanged; a failed turn, a configuration change or the end of the session stops them, and a kept server's notifications can arrive between turns. The model catalog, `settings.json` and the bundled skills are read again only when their files change; the branch is read from `.git`, and the prompt's Git facts are gathered off the turn's thread. Earlier turns of a conversation are decoded once and kept, and the store has per-session indexes. The Python kernel starts in the background when a turn starts (`HA_REPL_PREWARM=off` to disable). Short internal processes (Git reads, hooks, guard probes) run up to four at once instead of waiting for each other; a process's output stays in memory up to 64 KiB and leaves no files behind. While a run is active the worker sends the attached terminal at most about 30 frames a second, and only the clock when nothing else changed. Model calls negotiate HTTP/2 over TLS. A finished process or hook no longer voids the cached walk: ha writes a marker into `.harness` (or `.git`) and waits up to 200 ms for the watcher to report it, so every change the process made has been seen first. A write flushes the file to disk before replacing it, so a power loss cannot leave it empty or torn; `HA_WRITE_SYNC=off` skips that flush (about 70 ms per write on Windows) for those who prefer speed - a crash of ha itself still loses nothing.

| Group | Tools | Purpose |
| --- | --- | --- |
| Workspace | `read_file`, `list_files`, `search_text`, `glob` | Bounded reads and searches within the workspace |
| File edits | `apply_patch`, `write_file`, `edit_file` | Writes with hash or match conditions and approval; `write_file` creates missing directories inside the workspace |
| Process | `run_process`, `run_shell`, `read_process_output` | Run commands and read bounded output artifacts |
| Git | `git_status`, `git_diff`, `git_log` | Observe the repository |
| Task | `task_update`, `delegate`, `integrate_children`, `feature` | Record a next action; delegate to an explorer, a coder or a verifier; bring coders' work into the checkout; work the feature list |
| History | `history_search`, `history_read` | Search and read the conversation's journal; with `scope: "project"`, every earlier conversation of the project (a hit from another one names it and when it started) |
| Memory | `memory` | List, search (with up to four `queries` wordings), read, write and delete the model's memories and prompt notes, global or for this conversation, with or without the Python REPL; each entry says how old it is |
| Human | `ask_user` | Pause to ask; does not grant tool permission. Its options are a menu (arrows and Enter, or an option's number while the composer is empty); typing answers in the composer. A delegated child's question is shown named after the child and the child waits for the answer; questions and approvals from several agents open one after another |

As in prime-agent, children nest up to `RLM_MAX_DEPTH`, 2 by default: a child of the root agent gets a `delegate` tool of its own that starts explorers (a coder is started by the root only), and a child is not finished until its own children are - their notices open one more turn of the child, which then reports to its parent. A child messages its parent, siblings and its own children (`agent_message` with `receiver_role` `parent`, `sibling`, `child` or `all`); a grandchild does not reach the root. `/rlm-max-depth` shows the value and where it comes from (chat, global, env or default); `/rlm-max-depth <n>` sets it for this conversation and `/rlm-max-depth <n> --global` also saves `rlmMaxDepth` in `settings.json` beside the user config; `RLM_MAX_DEPTH` is read next. ha runs at most 2 levels (the delegation contract's cap); a child keeps the value it was created with. Each level runs at most three children at once. As a prime-agent child does, a child has its parent's tools and permissions - the permission mode, the allow and deny rules and what the user allowed for the turn: an explorer (and an `rlm.spawn` child) works in the parent's workspace, a coder in a host provisioned clean Git worktree. If a worktree cannot be provisioned, the tool returns `role_unavailable`. `/agents` opens the agents view (every session, its subagents nested under it) when the app runs in the background worker, as a bare `/resume` and Left on an empty prompt do; elsewhere it lists every child in a table with its state, time, tool calls, cost, model and latest note. A running `delegate` card names the agent's role and brief, with a line of its progress (tool calls so far and the latest one) under it (`/agents messages`: what the agents told each other); `/agents stop [name]` stops them, and Ctrl-C on a running turn stops them all with it; their conversations are kept, and the root agent's own `agent_message` tool (`receiver_name`, or `receiver_role: "all"`) wakes a stopped child where it left off, with or without the Python REPL. A resumed conversation tells the root which children ended unfinished. A child cut off by its agent shutting down (`ha shutdown`, `ha stop`) says so, not that the user stopped it. `delegate` waits for the child's answer by default; with `wait: false` it returns at once and the result arrives as a notice. Several `delegate` calls in one response run side by side. A child inherits the parent's web tools (`web_search`, `web_fetch`). A child that breaks - a provider error, a loop, an empty reply - comes back as a failed result the parent can act on, `[child-failed explorer] <code>: <reason>`, never as completed. Any turn, parent or child, stops as `loop_detected` when it reads the same thing (same tool, same arguments) three times with nothing changed in between, even when other calls come between the repeats.

**Bringing a coder's work into your checkout (`integrate_children`).** A coder edits only its own worktree and branch; nothing copies its work into your checkout by itself. The root agent calls `integrate_children` (by default every finished coder not yet integrated, oldest first; `children` picks and orders them). The host commits what each coder left uncommitted, merges the branches in that order in a separate integration worktree (`<data-dir>/delegation/integration/worktree`), and runs the project's `[verify]` checks on the merged revision - the same shell and time limits as `/verify`. The result is `ready` (with the final commit), `conflicted` (with the conflicting files and the coder that did not merge), `checks_failed` (with the failing check and its output) or `refused` when your checkout changed (a commit, an edit, a new file) since the coders started. Without `apply: true` your checkout does not change. With `apply: true` a `ready` result fast-forwards your current branch to the final commit - never a reset, a stash or a write over uncommitted work; the checkout's fingerprint is checked again at that moment, and the result names the undo (`git reset --keep <old head>`). The call goes through approval like any other writing tool. The merge commits are authored as `harness-p5`; a coder's own commit is titled by the files it changed, with the coder's summary in its body. A coder keeps its worktree across a restart of its agent: the ledger records it, a coder cut off by `ha shutdown` is woken in it with `agent_message` and carries on with the work already there, and one already integrated is never offered again; only a coder whose worktree was removed has to be delegated anew.

### 12.2. Chat commands and keys

| Group | Commands |
| --- | --- |
| Help and state | `/help`, `/hotkeys`, `/fullscreen`, `/session` (`/status`), `/config`, `/model [search]`, `/effort [level]` (`/thinking`), `/cost`, `/context` (`/usage`), `/system-prompt`, `/permissions`, `/hooks`, `/mcp`, `/agents`, `/subagent-model`, `/subagent-effort`, `/subagent-login`, `/subagent-logout`, `/rlm-max-depth`, `/skills`, `/bug-report` (this conversation's diagnostics in one zip, [12.12](#1212-diagnostics-and-bug-reports)) |
| Session and answer | `/new [--name <name>] [[--] prompt]` (`/clear` also clears the viewport), `/resume [id]`, `/name [name]` (`/rename`), `/more`, `/compact [instructions]`, `/export [path]` (an HTML page by default; `.md`, `.html` for one self-contained page, or `.jsonl` for ha's session file - prime-agent's JSONL shape, a header then one line per message - holding what the model is sent for this conversation, secrets redacted), `/import <path.jsonl>` (a new conversation that continues an exported one, or a prime-agent session file; the file is copied under `<data>/imports`), `/share`, `/new [prompt]`, `/heartbeat <instruction>`, `/heartbeats`, `/copy`, `/fork [n]`, `/clone`, `/tree [n]`, `/btw <question>` (`/side`), `/quit` (`/exit`) |
| Workspace and control | `/undo`, `/trust [yes]`, `/init`, `/permissions [ask|auto-edit|full-auto]` (`/permission`, `/mode`), `/steer <text>`, `/queue [text|list|edit n text|drop n|up n|down n]` (`/followup`), `/stash`, `/goal <objective>|status|pause|resume|clear`, `/handoff [--reset] [focus]`, `/verify`, `/review [focus]`, `/features`, `/doctor`, `/checks [list|accept|reject <name>]`, `/autonomous [status|off|on ...]`, `/schedule [list|add <when> -- <prompt>|pause|resume|cancel <id>]` |

**Queue, stash, side questions and branches (prime-agent's).** Enter while the agent works steers the running turn; a message the turn cannot take yet waits in the steering lane. `/queue <text>` adds a follow-up that runs as its own turn after the running one; `/queue` lists the queue and `/queue edit|drop|up|down <n>` changes it. What waits is sent after the turn - steering first, then follow-ups - one message per turn, or all of a lane at once with `[queue] steering_mode = "all"` / `follow_up_mode = "all"`. Inside a running turn steering follows the same mode, as prime-agent's loop does: one message reaches each model call, after the tool results of the step it arrived in. After Ctrl-C the queue (and any child reports) waits for your next turn. Ctrl-S (or `/stash`) puts the draft aside and, on an empty prompt, brings it back. `/btw <question>` asks the model about the conversation without tools and without adding anything to it; the answer opens in a panel, and a later `/btw` follows up. `/fork` lists your messages and `/fork <n>` starts a new conversation just before message n, with that message back in the editor; `/clone` starts one with the whole history; both show up separately in `/resume`. `/tree` lists this conversation's turns and `/tree <n>` continues after turn n; `/tree <n> --summarize [focus]` also has the model summarize the turns it leaves with prime-agent's branch prompt (the auxiliary model if one is set), and the next message is read with that `[branch-summary]` ahead of it. `/tree label <n> [text]` labels a turn, shown as `[label] ` in the list; an empty text clears it.

**Your own models, scoped cycling and model routing (prime-agent's).** A `models.json` beside the user config (`<HA_HOME>/models.json`) adds models in prime-agent's schema - `{"providers": {"<id>": {"baseUrl", "api", "apiKey", "models": [{"id", "name", "reasoning", "input", "cost", "contextWindow", "maxTokens"}], "modelOverrides": {"<model id>": {...}}}}}`, `//` comments allowed. `api` is `openai-completions`, `anthropic-messages`, `openai-responses` or `openai-codex-responses`; a new provider needs `baseUrl` and `apiKey`; missing fields default to prime-agent's (`contextWindow` 128000, `maxTokens` 16384, text input). `apiKey` is the **name of the environment variable** that holds the key, never the key. A model with a built-in provider and id replaces it, and `modelOverrides` change only the fields they name. A file that cannot be used is reported once at start (`models.json: <problem> - using built-in models only`). The models show up in `/model` once their key is set.

`[routing]` in the config: `scoped = ["deepseek/*", "openai/gpt-5*:high"]` (globs over `provider/id` or `id`, any case, optional `:level`) are the models `/model next`, `/model prev`, Alt+M and Shift+Alt+M move through - those with a credential, at least two. `/scoped-models <pattern>...` saves a scope for this and later launches (above the config, like `/model`), `/scoped-models` shows it, `/scoped-models clear` drops it. `auxiliary = "provider/id"` writes compaction summaries and `/refine` reviews (if it cannot, the session model does, with a notice). `backup = "provider/id"` takes over the turn when the session model has failed with a rate limit or an unavailable service on every retry (`Primary model unavailable (...) — retrying on backup model ...`); the next turn goes back to the session model (`Primary provider recovered — back on ...`). `image = "provider/id"` looks at the images of a session whose model's catalog entry takes text only: it describes each image once (its text verbatim, layout, what looks wrong) and the session model reads the description, so the turn stays on the session model (after CowAgent's vision tool); an image it cannot describe sends that request to it whole; without it such a request fails with `This model does not accept images; set [routing] image in config` instead of sending the image to a text model. A rate-limited provider (HTTP 429, or a structured quota code such as `insufficient_quota`; error code `rate_limited`) is waited out as prime-agent does - 1 s doubling to 5 min a check, 30 checks and 15 minutes at most, the server's `Retry-After` first - with `Waiting for provider usage to recover (n/30), next check in Ns... (esc to cancel)` in the status line; Esc cancels. `wait_for_usage = false` turns the wait off. When the provider reports a usage reset beyond that bound - its `Retry-After`, or a window its error names such as "Try again in ~7272 min" - the session is parked as in prime-agent: the turn ends with `Session parked until <time> and will resume automatically`, and a one-shot job kept with the conversation (`/schedule` lists it, `/schedule cancel <id>` drops it) sends prime-agent's resume prompt 30 seconds after the reset, at most a day later. The job survives quitting `ha`: it runs when the conversation is open again. `/tier` shows the service tier in force and the ones the model takes; `/tier default|flex|priority|auto` sets it and `/fast` toggles `priority`, as in prime-agent. The tier is sent as the request's `service_tier` to OpenAI (API key: `auto` for every model; `priority` and `flex` for gpt-5.4, gpt-5.5, gpt-5.6, gpt-5.6-* and gpt-6-astra) and to ChatGPT sign-in (the same, without `flex`); other providers take the default only, and a tier the model does not take is sent as `default`. It is kept with the conversation and saved as `defaultServiceTier` in `settings.json` beside the user config for new ones; the status line shows `fast` for priority and the tier's name for another one.

**Autonomous mode and schedules (prime-agent's).** `/autonomous on` keeps the session working after the model stops: each turn that ends cleanly is followed by another with prime-agent's continuation prompt, until a budget runs out - by default 3 continuations, 12 turns, 80,000 tokens and 30 minutes, checked in that order (`--max-continuations`, `--max-turns`, `--max-tokens`, `--timeout-ms`; naming any of them makes the unnamed ones unlimited; `unlimited` is accepted). `--gate "<command>"` (repeatable) adds a quality gate: after each turn the gates run in the workspace, in the shell and scrubbed environment the model's shell tool gets (5 minutes each, `--gate-timeout-ms`); all passing ends the run (`autonomous: quality gates passed`), a failure continues it with the command's exit and output (`[autonomous-continuation: gate-failed] ...`), and more than `--gate-retries` (3) failures end it. A gate that failed is not rerun while the git worktree is unchanged; that still counts as an attempt. While delegated children work, the run waits for their reports. `/autonomous` shows `[autonomous-status: ...]`; `/autonomous off` stops it (and any running gate). A goal and autonomous mode do not run together: setting one turns the other off, with a notice. `/schedule add <when> -- <prompt>` schedules a prompt for this conversation: `in 10m` / `in 2h` / `in 1d` (once), `every 30s` / `every 1h` (at least 10 seconds), `at 2030-01-01T09:00:00`, five-field cron in local time (`0 9 * * 1-5`) or `@hourly|@daily|@weekly|@monthly`; `--steer` sends it into a running turn instead of after it. Jobs are kept in `<data dir>/schedules/<task>.json` and come back when you resume the conversation; they run only while `ha` is open, runs missed while it was closed run once, and the next run counts from then. `/schedule` lists the jobs, `/schedule pause|resume|cancel <id>` changes one. A due job arrives as `[heartbeat: <when> run#N]` followed by the prompt.
| Content and extensions | `/login`, `/logout`, `/image`, `/attach <path>`, `/skill:<name> [args]`, `/reload`; templates in `.harness/commands` or `<config-dir>/commands` run as `/name` |

The command table, its order, wording and aliases follow prime-agent's `slash-commands.ts`, and so does the menu: typing `/` lists every command, letters narrow it with prime-agent's fuzzy match (`/skil` offers `/skills` and every `/skill:<name>`; prompt commands join too), and a command that takes an argument opens an argument menu on Tab or Enter - `/effort` offers the levels, `/model` the models, `/login` the providers - where Enter on a value applies it. A mistyped command is answered with the closest one. `/effort <level>` (alias `/thinking`) chooses how much the model reasons, from `off` through `minimal`, `low`, `medium`, `high`, `xhigh` to `max`, as prime-agent does; `/effort` alone shows the level in force and the levels the model offers. A level the model lacks is clamped to the nearest one it has (DeepSeek V4 offers `off`, `high` and `xhigh`, sent as `max`). The choice is kept with the conversation and, as prime-agent's `setThinkingLevel` does, saved as `defaultThinkingLevel` in `settings.json` for new conversations (not an `off` on a model that does not think); `provider.thinking` / `HA_PROVIDER_THINKING` is read after it. With thinking on, DeepSeek gets each assistant message's `reasoning_content` back and Claude its signed thinking block, within the turn; reasoning is never stored. A model the built-in table does not know is treated as not reasoning.

**While the agent works**, as in prime-agent and Claude Code: a message sent with Enter joins the running turn - the model reads it at its next step, or right after the answer it is writing, and the turn goes on - instead of waiting for the task to finish (it is queued only when the turn cannot take it yet). `/effort`, `/model` and `/permissions` are applied from the running turn's next model call or action, not refused.

`/goal <objective>` sets a persistent goal. Every turn carries it in its context, and the model gets a `goal_complete` tool (allowed without asking). The call is a request: the project's `[verify]` checks run, then an independent verifier judges the work (`[verify] judge = false` skips it), and only when both pass does the goal complete, with `[verified: ...]` in its summary; otherwise the call answers what failed and how to fix it, and the goal stays active. Asking again while nothing in the Git worktree changed since a failed verification runs nothing. A turn that ends without a completed goal is continued automatically, with no limit on the number of turns: the goal goes on until it is completed, its token budget is spent, or you stop it. Ctrl-C and `/goal pause` pause it; `/goal resume` continues it; `/goal clear` removes it. The goal is stored with the conversation: `/resume` restores it paused, `/new` drops it. Inside one turn, once the turn's own transcript passes about 200 KB, the oldest tool results (all but the newest six) are replaced by a one-line note; the model can call the tool again.

`@` opens the file picker; `@<server>:<uri>` attaches an MCP text resource. `!cmd` runs shell through approval; `!!cmd` only displays output. Enter submits, Ctrl-J inserts a newline, ↑↓ chooses a menu entry or history item, PgUp/PgDn and Home/End scroll the `/more` panel, Esc closes a panel or interrupts the turn, Ctrl-C cancels the turn (twice within 2 seconds on an empty prompt exits), Ctrl-O cycles the detail mode as prime-agent does: collapsed hides reasoning and cuts every tool output to three lines, details shows reasoning and the full diffs of file edits, expanded shows every output line; a call's card looks the same in all three. Ctrl-D on an empty line exits. As in prime-agent, the TUI renders **fullscreen** by default: the alternate screen holds a scrollable window over the conversation with the composer, hints and status line pinned to the bottom and the chat's name (its `/name`, else the workspace directory) and spend pinned to the top. The mouse wheel scrolls three rows, PgUp/PgDn a page (a panel or menu keeps its own PgUp/PgDn), Shift+Alt+Up goes to the top and Ctrl+End or Ctrl+Shift+Down back to the end, following the output again (`ctrl+end to follow` shows while you read back; Windows Terminal keeps Ctrl+Shift+Down for its own scrollback). Dragging selects text - it stays on the rows it covered while the answer streams, and a drag held at an edge scrolls on - and releasing copies it (`Copied selection to clipboard`). Leaving fullscreen prints what it showed into the terminal's scrollback; opening the agents view (`/agents`, a bare `/resume`) does not, and the session draws its conversation again when it is opened. A wheel notch moves the rows already on screen with the terminal's own scroll and writes only the rows that come into view. `/fullscreen [on|off]` switches at once and keeps the choice as `terminal.fullscreen` in `settings.json`; `terminal.fullscreenMouse: false` leaves the mouse to the terminal, `terminal.sidebar: false` hides herdr's sidebar (the workspace, branch, model and every agent with its working, blocked, done or idle state, shown at the left of a console at least 112 columns wide), and `HA_FULLSCREEN` (prime-agent's `PI_FULLSCREEN`: on only for `1`) wins over both. Inline, TUI history remains in terminal scrollback; plain mode prints lines. As Codex does, a launch says `Update available! <this> -> <newest>` when the npm registry lists a newer `harness-agents` (asked on a background thread at every launch and kept in `<data-dir>/cache/version.json`, so the notice shows from the launch after a release); `HA_NO_UPDATE_CHECK=1` or `"checkForUpdates": false` in `settings.json` turns it off.

### 12.3. Config v2, permissions and hooks

Config v2 merges default → user (`<config-dir>/config.toml`) → trusted project (`.harness/config.toml`, then `.harness/config.local.toml` for local permissions) → environment → CLI. `/config` identifies each value's source. Untrusted projects cannot load project config, hooks or skills. `/trust` requires confirmation. Profiles and models can be selected for the next turn.

`[permissions]` supports `mode = "ask" | "auto-edit" | "full-auto"`, `allow`, and `deny`; deny and protected paths take priority. Approval uses `y` (one action), `a` (the turn), and `n` (deny). `A` proposes a persistent rule, written to `.harness/config.local.toml` only after separate confirmation. Headless does not grant approval implicitly. `--allowed-tools`, `--disallowed-tools`, and `--approval` use the same policy. `[hooks]` follows Claude Code's hook model; hooks load only from trusted layers, have a timeout (at most 60 s), and cannot turn ask into allow. `/hooks` shows effective hooks.

```toml
[[hooks.pre_tool_use]]
matcher = "write_file|edit_file"   # `*`, names joined with `|`, or a regex such as `git_.*`
command = "python"                 # an executable, not shell text
args = ["scripts/guard.py"]
timeout_seconds = 10
```

| Event | When | What a hook can do |
|---|---|---|
| `pre_tool_use` | before a tool call, after the policy | block it (exit 2, `decision: "block"`, or `permissionDecision: "deny"`); ask the user even where the policy allows (`"ask"`); rewrite the arguments (`updatedInput`, judged by the policy again); add `additionalContext` |
| `post_tool_use` | after a call that ran | read `tool_input` and `tool_response`; send feedback to the model (`decision: "block"` + `reason`, `additionalContext`) |
| `stop` / `subagent_stop` | the main agent / a delegated child finishes | `decision: "block"` + `reason` sends the turn on with the reason; `stop_hook_active` is `true` on the second stop |
| `user_prompt_submit` | before a prompt is sent | block it; stdout or `additionalContext` is added to the prompt |
| `session_start` | before the first prompt of a session; `source` is `startup`, `resume` (`/resume`, `/fork`, `/clone`), `clear` (`/new`) or `compact` (after `/compact`) | stdout or `additionalContext` is added to the prompt |
| `session_end` | the app closes | observe only |
| `pre_compact` | before `/compact` | observe only |
| `notification` | a question or an approval waits | observe only |

A hook gets one JSON object on stdin (`hook_event_name`, `session_id`, `cwd`, and per event `tool_name`, `tool_input`, `tool_response`, `prompt`, `last_assistant_message`, `stop_hook_active`), at most 8 KiB, secret-looking arguments redacted. Exit 0 goes on; exit 2 blocks with the first line of stdout (else stderr) as the reason. Exit 0 with a JSON object on stdout is read for `continue: false` + `stopReason` (end the turn, `hook_stopped`), `systemMessage` (shown to the user), `decision`/`reason`, and `hookSpecificOutput`. Any other exit or a timeout blocks a `pre_tool_use` call and is only reported for the other events. `permissionDecision: "allow"` is not honored: a hook cannot answer an approval.

`[mcp_servers.<name>]` configures stdio or Streamable HTTP. Stdio uses `command`, `args`, `cwd`, and `env` secret references; HTTP bearer comes from an environment variable. `enabled_tools`, `disabled_tools`, `tool_timeout` (maximum 120 seconds), and `required` constrain a server. Servers start on demand. In chat, prime-agent's `/mcp add <name> [--env KEY=VALUE] [--cwd DIR] [--force] -- <command> [args...]` (or `--url <https-url> [--bearer-token-env-var VAR]`), `/mcp list`, `/mcp get <name>` and `/mcp remove <name>` manage servers in `mcp-servers.json` beside the user config, read at the user layer and effective from the next turn, without rewriting `config.toml`; bare `/mcp` shows each server's status. `ha mcp add|list|get|remove` edits `config.toml` from the command line. MCP tools cross the same policy/approval gate and leave receipts. Skills are discovered in `<config-dir>/skills`, `~/.agents/skills`, and trusted project roots (`.agents/skills`, `.harness/skills`). The initial prompt carries names and descriptions only; activation loads content by digest.

### 12.4. Automation

```text
ha exec "Summarize the changed files" --output-format text
ha exec --prompt - --output-format json < prompt.txt
ha exec "Continue the task" --continue --goal "Task complete" --max-turns 4 --output-format stream-json
```

`--prompt -` reads up to 10 MiB from stdin; a positional `-` is literal text. `text` prints the answer, `json` prints a schema 1 envelope with additive fields (a satisfied `--goal` includes `acceptance.command_id`), and `stream-json` prints one NDJSON event per line without ANSI. `--continue` selects the newest session in the project. Exit codes: 0 complete, 2 usage, 3 waiting for a question or approval, 4 failure, 5 ownership conflict, 130 cancel. Legacy `ha chat --headless --json` remains compatible. `--mock` and `--fixture` are local tests; real provider calls may cost money.

### 12.5. Limits and checks

When the model asks for several tools at once they run side by side, as in prime-agent; a batch holding `python_repl`, a file write, `run_process`, `run_shell` or `ask_user` runs one call at a time. Approvals, intents and receipts stay in call order. Read-only tools no longer fingerprint the workspace, and a fingerprint rehashes only files whose size or modification time changed. On Windows `run_shell` uses `pwsh`, falling back to `powershell.exe` when pwsh is missing; the receipt records the selected shell. Strict isolation is claimed only where a measured backend supports it. Start with `/status` and `/config` when provider or permissions differ from expectations. As in prime-agent, a turn has no step, tool-call or time bound: it runs until the model is done, you stop it, or the tokens run out; `HA_TURN_MAX_STEPS`, `HA_TURN_MAX_TOOL_CALLS` and `HA_TURN_DEADLINE_SECONDS` set one, and a turn that hits it is continued automatically up to twice (`HA_TURN_CONTINUATIONS`). The whole conversation is kept, as pi and prime-agent keep it: an earlier turn comes back with its tool calls, their results (up to 32000 characters each) and what you sent while it ran, not only its answer, so the model remembers what it read, ran and changed. When a request nears the model's window (the window minus the answer's room and a reserve of up to 16384 tokens) it is compacted as prime-agent does: the earlier turns become a summary the model writes while the recent end (about 20000 tokens) stays word for word, then the turn's oldest tool results are shortened, and the turn goes on. `/compact` and the automatic checkpoint summarise the whole conversation, not only the last turn, and the files it read and changed carry over from one summary to the next. As in prime-agent, the summary of the earlier part is kept with the conversation and extended with what came after rather than written again, it is told the newest assistant message that stays (the recent-state anchor), a transcript too long for one summary request is summarised in pieces, and a single turn too large to keep is cut at an assistant message with its start summarised on its own (a split turn). What the model was told during a turn - your steering, a goal continuation, stop-hook feedback, the repeated-read warning - stays with that turn. A delegated child (an explorer, a coder, the verifier) is sent the conversation's harness memory and the skills ha learned from you, so a lesson the parent learned also holds for its children. Only a request that still cannot fit the window fails. If `ha` behaves like an older build, `Get-Command ha -All` shows which executable runs; reinstall with `scripts/Install-Ha.ps1`. The unit, integration and PTY suites have separate evidence (`scripts/Invoke-HaPtyAcceptance.ps1` runs the PTY cases that `cargo test` skips). Linux support is pending: its CI job does not gate a push, and no Linux claim is made from it.

### 12.6. `/resume`

`/resume` replays the selected conversation's durable turns to the model and shows them on screen. It does not infer or create an additional cross-session data source.

`/resume <id|name|latest>` selects directly without opening the list first; numbers still select from the last list. Names may contain spaces. A unique ID prefix or suffix needs at least six characters. Names, conversation IDs and abbreviated IDs select the conversation's newest turn; a full session ID keeps its selected historical position. Ambiguous or invalid selectors leave the current conversation intact. `/resume --compact [selector]` loads and summarizes the conversation before you continue, defaulting to `latest`. If the model summary fails, HA retains the replayable transcript only when it fits; otherwise compaction fails and the checkpoint stays unchanged. `ha chat --resume <selector>`, `ha exec --resume <selector>` and `--continue` use the same selection rules; `latest` excludes delegated children. If a background agent already owns the conversation, CLI resume attaches to that agent at its current position. Explicit launch overrides are rejected; change settings in the attached session or stop it first. An in-session switch to another agent's conversation and a concurrent headless resume are rejected with instructions for reaching the owner. After a crash, recorded tool results are replayed; check whether a tool without a recorded result took effect before retrying it. A restored goal is paused; `/goal resume` continues it. From another client, `ha send <agent> "/goal pause" --wait` pauses a busy goal immediately.

### 12.7. Background agents

Like prime-agent's daemon, an interactive session runs in a background worker and the terminal attaches to it. Closing the terminal - `/quit`, ctrl+d, closing the window - detaches it: the running turn finishes, and a goal, the autonomous gates, queued messages, heartbeats, `/schedule` jobs and delegated children carry on. The terminal says so on the way out (`agent <id> keeps running in the background`).

| Command | What it does |
| --- | --- |
| `ha agents` | prime-agent's agents view, in a terminal: every running agent and this project's saved conversations, grouped Running / Idle / Inactive and refreshed every second. Typing searches (a ranked list); ↑/↓ choose; Enter or → opens - attaching to a running agent, resuming a saved conversation as one; Space writes a reply (sent to the agent, or the prompt a saved conversation resumes with in the background); Ctrl+R renames an agent; Ctrl+X twice stops one; Ctrl+N starts a new session; Esc leaves. In a session the worker hosts, ← on an empty prompt or a bare `/resume` comes back to the view. Out of a terminal, or with `--json`, it prints the list as `ha list` does. |
| `ha list` | Every running agent: id, name, status, idle time, project and last request; `*` marks one a terminal is attached to. `--json` for scripts. Agents a reboot or a dead worker left in a journal are started again first. |
| `ha attach <agent>` | Bring the agent back to this terminal: the conversation so far is drawn, and typing goes on. Several terminals can attach to one agent, as prime-agent's clients view one session: each draws what the agent does, a key is answered to the terminal that typed it, and `/quit` detaches only that terminal. |
| `ha send <agent> "<message>"` | prime-agent's `send`: an idle agent starts a turn with it; a busy one reads it at its next step (`--steer`, the default) or after its turn (`--follow-up`). `--from <agent>` names the sender. |
| `ha rename <agent> <name>` | A name `attach`, `send` and `stop` accept. |
| `ha stop <agent>` | Stop the agent: its turn and its children are cancelled. The conversation stays in the store. |
| `ha shutdown [--force]` | Stop every agent and worker; without `--force` it asks first. |

`<agent>` is an id, a name or an id prefix no other agent shares.

One worker serves one project, and all of that project's agents share its store, so two agents of a project run side by side instead of waiting on the store's writer lock. In the agent's Python REPL, `await rlm.create_session('<prompt>', name=..., model=..., thinking=..., cwd=...)` starts a separate top-level agent (prime-agent's API): in the same worker, or in the worker of the project `cwd` belongs to.

An agent with no terminal, no running work and no schedule stops after prime-agent's `idleEvictionMinutes` (90 by default; set a number or `"off"` in `settings.json`). The worker leaves when its last agent is gone. Its descriptor (port and token, owner-only) and its log are in `<data-dir>/workers/`; a connection must present the token, and the client's environment - where the credentials come from - travels over the loopback socket and is never written.

`ha exec` runs its turn in the project's worker as well, as prime-agent runs a headless session through its daemon: the turn writes through the store the agents share, so an agent in the middle of a turn no longer makes `ha exec` fail with `writer_locked`. Its output, JSON and exit code are the same as before; Ctrl-C cancels the turn. With `HA_DAEMON=off`, or when no worker of this build runs, the turn runs in the calling process. A worker is started from a terminal only: one started by a command whose output is redirected would keep that output open for as long as it runs (on Windows a child inherits the handles), so `ha exec ... | tool` would never end.

As prime-agent's supervisor does, a worker runs under a supervisor process that starts it again when it dies - after 250 ms, 1 s and 5 s; a worker that keeps dying is given up. The worker keeps a journal of its agents (`<data-dir>/workers/<project>.journal`: ids, names and conversations - never the environment), and a worker that starts brings them back on the same ids and conversations, and their schedules and quota parks continue with no terminal open. Stopped conversations are not reopened merely because they still have old schedules. After a reboot the next `ha` in the project, or `ha agents` in a terminal, starts the worker and the agents come back; they run on the environment of the terminal that started the worker. When `ha` is updated, a terminal of the new build that finds the old build's worker idle has it stop with its journal kept and starts a new one, which brings the agents back; a worker whose agents are working is left alone, and the terminal says so and runs the session itself.

Where ha differs from prime-agent: there is one worker per project rather than per session, and a turn a crash interrupts is not resumed - the recovered agent continues the conversation, and the interrupted turn is replayed as interrupted. `HA_DAEMON=off` or `"daemon": false` in `settings.json` runs the session inside the terminal as before; the plain renderer always does.

### 12.8. More from prime-agent

**From prime-agent 8d58e5b (0.5.8).** Fixes and features ported from prime-agent's commits after the Rust port:

- Ctrl-C stops a context summary too: compaction (automatic, `/compact`, or a long turn folding its own steps) used to run its model call to the end, up to five minutes.
- A Chat Completions stream ends at its `[DONE]` and an Anthropic one at `message_stop`, even when a proxy keeps the connection open.
- A `maxTokens` set in `models.json`'s `modelOverrides` is asked for as written, not cut to the default request cap.
- In ACP mode, a final answer cut at the output cap ends the prompt with `max_tokens` instead of `end_turn`; the app says the answer stopped at the output limit.
- A revived or fresh kernel says the working directory and environment variables are the session's again, so the model re-applies an `os.chdir` or `os.environ` change it still needs.
- The prompt says: no reply to a notice that needs no action; a background `bash()` is not awaited but collected when its notice arrives; `rlm.harness` calls are synchronous.
- **Per-model prompt additions.** Rules map model selectors to markdown files that join the system prompt before its session tail, where they stay cached. ha ships prime-agent's rule (GLM 5.3: `bash()` over `subprocess`); `model-prompts.toml` beside the user config adds your own, files resolving in that directory:

  ```toml
  [[rule]]
  match = ["deepseek-*"]
  files = ["deepseek.md"]
  ```

  A glob is tried against every final run of the selector (`provider/model`), case-insensitively. A broken map (bad TOML, an unknown key, a missing file, a path outside the directory) turns the additions off and is reported once; it never blocks the model.
- **Compaction threshold.** `"compaction": {"thresholdPercent": 60}` in `settings.json` compacts at that share of the window (1 to 100; 95 by default); the output and the reserve can make it earlier, and it never goes below what the summary passes need.
- A prompt stashed with Ctrl-S comes back to the emptied editor once another prompt is sent.
- A Python cell that is mostly `bash()` calls shows as a **Bash** card with its first command (`+N more`).
- `/session`'s cost includes what the session's children spent, deleted ones too: `$0.0123 (children $0.0040)`.
- Certificate authorities in a PEM file - `HA_EXTRA_CA_CERTS`, `NODE_EXTRA_CA_CERTS` or `SSL_CERT_FILE` - are trusted besides the system's, for a TLS-inspecting proxy.
- `ha exec` delegates as the app does, and the run waits for its children: one started with `wait: false` reports when it settles and its notice opens one more turn (up to eight), as in the app. Nobody can approve there, so a child's gated action is refused, as the root's is.
- The REPL tool is `python_repl`; a call by `ipython` or `python` still reaches it.
- `/new --name <name> [-- <prompt>]` names the new conversation (a quoted name may have spaces).
- An MCP server's `oauth_client_id` and `oauth_callback_port` in `[mcp_servers.<name>]` (or `ha mcp login --client-id --callback-port`) sign in with a client registered in advance, for a server without dynamic client registration; a taken callback port is reported instead of redirecting to a port the client does not know.

**Entry points.** `ha "fix the parser"` opens the app and sends the message; with input piped in (`cat log | ha "why?"`) the input follows the message and the answer is printed, as `ha exec` prints it. `ha exec` (and `ha chat --headless`) take `--model`, `--thinking`, `--system-prompt` and `--append-system-prompt`. `ha model list [search] [--json]` lists the catalog models with a credential, `ha prompt [--cwd] [--json]` prints the system prompt a session would get. `--offline` (or `HA_OFFLINE=1`) skips every startup download - the model catalog, the logged-in providers' model lists and the update check.

**RPC and ACP.** `ha --mode rpc [--model M]` serves prime-agent's RPC mode: JSON commands on stdin (`prompt` with `streamingBehavior` `steer` or `followUp` while a turn runs, `steer`, `follow_up`, `abort`, `new_session`, `get_state`, `set_model`, `cycle_model`, `get_available_models`, `set_thinking_level`, `compact`, `get_last_assistant_text`, `set_session_name`), responses `{id, type: "response", command, success, data | error}` and prime-agent's session events (`agent_start`, `message_start` / `message_update` / `message_end`, `tool_execution_start` / `_update` / `_end`, `compaction_start` / `_end`, `agent_end`) on stdout. `ha --mode acp` serves the Agent Client Protocol for editors over stdio: `initialize`, `session/new` (one session per connection, in the `cwd` it names), `session/prompt` streaming `session/update` chunks and tool calls and answered with its `stopReason`, `session/cancel` and `session/close`. Nobody can answer an approval in either mode, so a gated action is refused, as `ha exec` refuses it.

**Background agents.** `ha send --wait` prints the answer of the turn the message started; `ha abort <agent>` ends an agent's running turn and keeps the agent. `ha send <session id> "<message>"` to a saved conversation that no agent runs resumes it as a new agent, with the message as its first prompt. `ha schedule list [--all] [agent] [--json]`, `ha schedule add <agent> <when> -- <message>` and `ha schedule cancel [agent] <job-id>` manage an agent's scheduled prompts through its worker. Heartbeats (`/heartbeat <instruction>`, `/heartbeats`) are kept with the conversation like `/schedule` jobs, and stopping an agent cancels its schedules.

**Prompt and screen.** The prompt edits as prime-agent's does: word motion (Ctrl/Alt+Left/Right, Alt+B/F), Ctrl+K and Ctrl+Y (kill ring), Alt+D, Ctrl+T, Ctrl+Z / Ctrl+- undo and Ctrl+Shift+Z redo, Ctrl+D deletes forward, and a large paste shows as `[paste #N +L lines]`, sent as the text it stands for. Ctrl+G edits the message in `$VISUAL` / `$EDITOR`. A second Esc within half a second clears the draft, or opens the session tree on an empty prompt. A bare `/model` opens the model menu; `/tree` and `/fork` open their selectors (arrows, Enter, Esc). A leading command, `@path` and `--flag` tokens are coloured as in prime-agent. While messages wait, the queue strip above the prompt shows each one (`Steering:` / `Follow-up:`). While a tool call's arguments stream the working line reads `Writing code`. A multi-line error shows its summary line until Ctrl+O expands it. Copies are acknowledged with a short toast, also through OSC 52 over SSH and tmux. Markdown tables and links render as in prime-agent. `keybindings.json` beside `settings.json` rebinds prime-agent's binding ids - `{"app.editor.external": "ctrl+e", "tui.editor.yank": []}`: a configured binding answers to exactly the keys given, and an empty list turns it off.

**Models and providers.** `allowedModels` in the global `settings.json` (exact `provider/id`, bare id or glob) bounds the models a turn, `/model` and a delegated child may use; a model outside it fails with prime-agent's refusal and never falls back to another. Anthropic gets prime-agent's `cache_control` (`HA_CACHE_RETENTION=long` for one hour), fine-grained tool streaming and interleaved thinking; cached prompt tokens are counted and priced apart. A request the provider reports as too long is compacted and sent again; compaction writes prime-agent's structured summary and updates it the next time instead of summarizing again, and after a compaction the next prompt opens with prime-agent's `[python-state]` note naming what the kernel still defines. `/login serper` saves the Serper key for `web_search`.

**Prompt caching.** Every provider ha talks to caches a request by its prefix - tools, then the system prompt, then the messages in order - and charges a cached prompt token a fraction of the price (about a tenth on Anthropic and OpenAI, a fiftieth on DeepSeek), so ha keeps that prefix byte for byte the same for as long as it can, the way Claude Code, Codex and pi do. The system prompt holds only what is fixed for the session; the date, the Git branch and the changed-file count ride each turn's input instead. A turn appends and never rewrites: every step sends the turn's first input packet again unchanged, the goal continuation and the "earlier turns not shown" note are user messages rather than system ones, old tool results are shortened with a quarter of headroom so the next steps do not rewrite another one, and a compaction's cut moves only every eighth user turn so its summary stays the same for several turns. Across turns the same holds: an earlier turn is sent again with the input packet it sent, not its bare question, so a follow-up request begins with the previous turn's last request word for word; a block an earlier turn already sent (an activated skill, the project rules, a summary, an unchanged state) is not repeated in the new packet; and the tool definitions are sorted by name. Per provider: Anthropic gets breakpoints on the system prompt, the last tool, the last user message and the user message before it (the results of one batch of calls go back as one message); OpenAI's Responses and Chat Completions get `prompt_cache_key` (the conversation, `<conversation>:agents` for its child agents); DeepSeek caches automatically. `HA_CACHE_RETENTION=long` keeps the cache an hour on Anthropic and a day on OpenAI's Responses API (`prompt_cache_retention: "24h"`; the ChatGPT backend keeps its own). The status line shows `↯ cache NN%`, the session's prompt tokens read from the cache (DeepSeek's `prompt_cache_hit_tokens`, OpenAI's `cached_tokens`, Anthropic's `cache_read_input_tokens`); a share that falls means something changed the prefix. Each model call's usage is counted once, even from an endpoint that repeats it on every frame.

**MCP servers.** `ha mcp login <server> [--client-id] [--scope]` signs in to a Streamable HTTP server that asks for OAuth (RFC 9728 / RFC 8414 discovery, dynamic client registration, PKCE on `localhost:53700-53709` or a pasted redirect URL); the tokens are saved in `auth.json` under `mcp:<server>`, refreshed before they expire and sent only to that server's URL, and `ha mcp logout <server>` forgets them. A server takes `enabled = false`, `headers`, `header_env` (a header from an environment variable) and `startup_timeout_seconds`.

**Conversation.** Goals carry a token budget (`goal.create(..., token_budget=...)`) and pause as budget-limited when it is spent. `/share` publishes the conversation as a secret GitHub gist (through `gh`); `/new <prompt>` starts a new conversation with that prompt; `/export` writes an HTML page by default. `/import` also reads prime-agent session files. `/btw` declares the session's tools and refuses every call, as prime-agent's side question does. `autoRefine.enabled` and `autoRefine.turnInterval` in `settings.json` control the automatic review. `run_shell` refuses a git command that would discard uncommitted work (`git checkout -- .`, `git reset --hard`, `git clean -f`, ...) unless `HA_ALLOW_DESTRUCTIVE_GIT=1`. `rlm.rename(...)` renames the session or a direct child.

Not ported: Claude Pro/Max, Copilot and xAI sign-ins (they impersonate another client), the Prime platform pieces (inference, traces, telemetry, cloud), the activity dock, `/settings` and themes of prime-agent's TUI, and the cost column of its agents view.

### 12.9. Packages, resources and the rest of prime-agent

**Packages.** prime-agent removed its TS/JS extensions; what it keeps is its package manager, and that is what ha has. `ha package install <source> [--local]` installs a package of skills, prompt templates and themes from npm (`npm:@scope/name[@version]`), git (`git:github.com/user/repo[@ref]`, `https://...`, `ssh://...`) or a local directory, and records it in the user `settings.json` (`--local`: `.harness/settings.json` of the project); `ha package remove|uninstall <source> [--local]`, `ha package update [source]` and `ha package list` manage them. A package names its resources in the `pi` manifest of its `package.json`, or keeps them in `skills/`, `prompts/` and `themes/`; a settings entry `{"source": "...", "skills": [...], "prompts": [...]}` filters what it adds. Nothing in a package runs: skills and prompts are read like the user's own, and project packages count only in a trusted project. In a session a package that is not installed yet is skipped (an install would write over the TUI); `ha package install` / `update` installs it. `--offline` keeps the package manager off the network.

**Resources.** `ha config` opens prime-agent's resource selector over every skill, prompt template and theme (built-in, user, project and package ones): typing filters, Space turns one on or off and writes the change to the settings (the `skills` / `prompts` / `themes` arrays with prime's `+path` / `-path` patterns, or the package entry's filter), Esc closes. `ha config validate` and `ha config explain` are unchanged. Prompt templates load from `prompts/` in the config directory and `.harness/prompts/`, from the `prompts` settings arrays and from packages, then from the earlier `commands/` folders; they take prime's `$1`..`$N`, `$@`, `$ARGUMENTS` and `${@:N[:L]}`. `SYSTEM.md` (`.harness/SYSTEM.md` of a trusted project, else in the config directory) replaces the static layers of the system prompt, and `APPEND_SYSTEM.md` is added at its end; `--system-prompt` and `--append-system-prompt` take their place.

**Launch flags.** `--thinking`, `--goal`, `--system-prompt` and `--append-system-prompt` also work for the interactive app (`ha`, `ha "msg"`, `ha chat`) and travel to the background worker (prime itself reads the last three in print mode only). `ha exec` takes prime's `--autonomous`, `--autonomous-gate <command>` (repeatable), `--autonomous-gate-retries`, `--autonomous-gate-timeout-ms`, `--autonomous-max-continuations`, `--autonomous-max-turns`, `--autonomous-max-tokens` and `--autonomous-timeout-ms`: after a turn that ends cleanly the gates run in the workspace and the run continues until they pass or a budget runs out, and a gate still failing (or an ungated run a limit stopped) exits 1 with prime's stderr line.

**RPC and ACP, continued.** `--mode rpc` also serves `cycle_thinking_level`, `set_steering_mode`, `set_follow_up_mode`, `refine`, `set_auto_compaction`, `set_auto_retry`, `abort_retry`, `switch_session`, `fork`, `clone`, `get_fork_messages`, `get_messages`, `export_html`, `get_session_stats` and `get_commands`, and answers the scheduling, messaging, `observe` and `bash` commands as prime's in-process transport does. The toggles are kept as prime's settings (`compaction.enabled`, `retry.enabled`, `steeringMode`, `followUpMode`). `--mode acp` advertises image prompts and HTTP MCP servers: `session/new` admits the client's `mcpServers` (held in memory for that session) and answers prime's `configOptions`, `session/set_config_option` switches the `model` and `thought_level` pickers, a prompt's image blocks are attached, and goals and refinements arrive as `session_info_update`.

**MCP catalog.** `/plugins [search]` opens prime's service catalog - the Linear and Notion built-ins, the user's `mcp-services.json` beside the config, and prime's public catalog (refreshed daily, a snapshot compiled in) - connected services first; Enter (or `/plugins connect <id>`) adds an OAuth service as an MCP server and starts its browser sign-in. `/mcp login <service>` and `/mcp logout <service>` work in the app too. The `mcp` skill's `mcp.list_plugins` / `mcp.search_plugins` answer from the same cards.

**Agents.** Top-level agents are siblings, as in prime: `agent_observe.list_agents` lists the other running agents and `agent_message.send(..., receiver_role="sibling", receiver_name=...)` reaches one. `/agents messages status|pause|resume|clear` is prime's ingestion control: a paused agent refuses other agents' messages ("Agent messaging is paused"), and pause and clear drop the queued ones (never the user's own queue). A message to a child that finished wakes it for a follow-up turn, and each conversation's spawn ledger (`<data>/delegation/ledger/`) brings its children back when it is opened again - a child that was running when its worker stopped comes back failed as interrupted. In the agents view an agent with subagents carries a `N subagents (M running)` line: Alt+Right (or Enter on it) expands it, Ctrl+O shows each subagent's spawn program, a row shows its heartbeats as `◷N`, Ctrl+X twice deletes a saved session from the listings (the store keeps it), and the mouse selects and scrolls.

**/tree.** `/tree` opens prime's tree selector over every turn of the conversation, its branches included: `├─`/`└─` connectors, `•` on the current path, Ctrl/Alt+Left folds a branch (⊞) and Ctrl/Alt+Right unfolds it, typing filters, Ctrl+D/U/L/A/T switch prime's filters and Ctrl+O cycles them, Shift+L edits a turn's label and Shift+T shows label times. Enter asks prime's "Summarize branch?" (`branchSummary.skipPrompt` skips it) and then continues from the chosen answer, or puts a chosen message back in the editor.

### 12.10. Long tasks and compaction

A task of hundreds of steps passes through many compactions, and what a compaction keeps is a summary the model wrote. 0.5.9 keeps the parts of a task that must not become a paraphrase out of the summary, and checks the summary it does write.

- **Task state.** The `task_state` tool keeps the task's objective, a checklist with each item's status, the decisions taken and why, the user's constraints, open questions and blockers, and the key files. It is stored with the conversation (`sessions/<task>/harness/task_state.json` in the data directory), every turn carries it whole, and no summary replaces it. With an active goal the model is told to keep its plan there, and `goal_complete` refuses while a checklist item is open. It changes host state only, so it needs no approval.
- **The user's words.** A summary of the earlier conversation ends with what the user said in the part it covers, word for word: each question as typed (not the prompt the host composed around it, with its notices and attached files) and every steering message, oldest first, a repeated message once, a goal's continuations left out, up to about half the summary's own budget (some 4k tokens at the default 8192) with the oldest dropped first and counted. The block is carried from summary to summary mechanically, like the file lists, and it outranks the summary where the two disagree.
- **A long turn's own steps.** When a turn outgrows the window, its earlier steps are summarized with the same checkpoint format, preserve rules, recent-state anchor, file lists and budget as the earlier turns, and what the user sent while it ran stays word for word (it used the split-turn prompt, told to be concise, at half the budget).
- **Summary check.** When a summary is updated, the constraints, done items, decisions, critical context and exact names (paths, identifiers in backticks) of the previous one are looked for in the new one; whatever it dropped is put back under `## Carried Forward`.
- **Recitation and saving.** After a summary, and every 40 steps of a long turn, the host adds a `[task-recap]` note near the newest message: the goal, the checklist's progress and next item, the constraints, the decisions, and the files `git status` shows changed (re-read before editing). Once per turn, when the context reaches 80% of its compaction point, a `[context-note]` asks the model to save its task state and memory first. Both are the host's, not instructions from the user, and neither changes an earlier message, so the cache holds.
- **Checkpoints.** `/compact` and the recovery from an overflow summarize with the same budget as the rolling summary (8192 tokens, not 2048; four fifths of the compaction reserve, which is at most a quarter of the window). When the replayed conversation reaches half its bound (4 MiB or 1000 turns), it is checkpointed before any turn can fall out of it; past the bound the oldest turns used to disappear with the summary that stood for them. A continued turn joins its conversation as soon as it is admitted, so a checkpoint its first step must write summarizes the earlier turns too (it covered only the turn itself, and stood for everything before it).
- **Smaller.** A tool result shortened to fit the window keeps its first 400 characters, where an error says what failed. The summaries' file lists keep the most recently touched files, not the first ones by name. A new summary, a fold of a turn's steps and a checkpoint all count as compactions, so the memory review that a compaction brings forward runs after them too.

`scripts/live-tests/compaction.sh` runs one task of eight modules through a 40k compaction point with a real model, adds a rule while it runs, and checks recall, the disk and what the host kept.

### 12.11. Memory that reaches the work

0.6.0 makes what is remembered reach the turns that need it, and keeps it clean on the way in.

- **Ranking.** A turn's memory digest is ranked by the goal, what the user typed (not the prompt built around it), the task state's recap, the user's two previous messages and the feature in progress, so a "continue" no longer ranks by one word. A delegated child's digest is ranked by its brief (it used to be in name order), and `ha exec`'s children get one too.
- **Constraints.** A memory or prompt note whose path is `constraints` or `constraints/<topic>` is shown in every turn ahead of the ranked entries (up to 12), whatever the request: a rule the user set no longer falls out of the six shown.
- **The task an earlier conversation left.** A new conversation's first three turns carry the task state of the newest earlier conversation in the project when its checklist has open items, as context the model may adopt with `task_state` (or the user may `/resume`); one whose work is done shows nothing.
- **Reviews.** A turn that compacts has its memory reviewed beside it as it goes on, not only after it ends (the task's `review_started` setting records when and why). A session that goes quiet - ten minutes without a turn (`HA_IDLE_REVIEW_SECONDS`) - has the work since its last review reviewed then; a background agent stays alive after its terminal closes, so the review runs even if nobody comes back, and an agent stopped sooner still leaves it to the next turn. A refinement the agent asks for judges the learned skills in play.
- **Writes.** The `memory` tool refuses a new entry that a longer entry already holds (85% of its weighted words), not only one alike as a whole, and refuses a duplicate before the call runs, so it reads as a plain refusal. `rlm.harness` writes from the kernel pass the same checks: no secret, no duplicate memory or prompt note, and the `expected_version` an update names. An id made from a title is at most 48 characters, cut at a word; a title over 200 characters is refused with how to fix it.
- **Prompt.** Commit messages go through `git commit -m` or a temp file, never a file in the checkout.

### 12.12. Diagnostics and bug reports

0.6.1 records what happened, so a bug can be found from one file instead of a screenshot.

- **The diagnostics log.** Every `ha` process writes one JSON object per line to `<data dir>/diagnostics/<YYYYMMDD>-<role>-<pid>.jsonl`. `ha diag path` prints the directory, and `HA_DIAG_DIR` moves it. The role is `tui`, `exec`, `worker`, `supervisor`, `rpc`, `acp` or `cli`.
  - **Each record has:** its time, process, thread and level; a `kind.event`; a message; the ids it is about (`agent`, `task`, `session`, `request`, `call`); and its numbers.
  - **What is recorded:**
    - the process's start and exit, and panics with their place and backtrace;
    - how a worker died: a panic, a stack overflow or an access violation;
    - agents that started and agents that were lost;
    - every notice and error the user is shown, every failed tool call, and how each turn ended;
    - provider attempts that failed, and how long each retry waits;
    - each compaction, with its window, threshold, and token counts before and after, and any summary that failed;
    - a delegated child that failed. Before 0.6.1 only the parent model saw this;
    - the outcome of each background review, curation and consolidation, even when no terminal is listening;
    - who holds a project's store when another process cannot open it.
  - **Not recorded:** streamed text, tool output and prompts.
- **Level and size.**
  - `HA_DIAG=off|error|warn|info|debug` sets the level (default `info`).
  - A plain `ha` command (`ha list`, `ha doctor`) writes a file only for a warning or an error.
  - A file rolls over at 8 MiB, and a process past 24 MiB keeps only errors and warnings.
  - At start, files older than 14 days are deleted, then the oldest files until all of them fit in 128 MiB.
  - Secrets are redacted as each record is written, and any field named like a key, token, secret or password is withheld.
- **Who holds the store.** The process that holds a project's writer lock writes a note about itself to `writer.owner.json`, beside `writer.lock`. A `writer_locked` error now says, for example, `held by worker pid 5678 for 42s`.
- **`ha bug-report`.** Writes `ha-bug-report-<time>.zip` in the current directory, or to `--out FILE`. The zip holds:
  - every process's diagnostics within the window (`--since 24h` by default; also `30m`, `3d`);
  - the workers' logs, journals and descriptors, without their token;
  - from each project store: runs, steps, provider attempts, failed commands, leases, checkpoints and settings;
  - each conversation's harness files: task state, memory history, applied refinements, the review watermark and kernel stderr;
  - `env.json` (version, platform, terminal, `HA_*` variables), the configuration, `settings.json` and `models.json` with every secret withheld;
  - the shared REPL's logs, and the delegated children's directories (names and sizes).

  The diagnostics hold one line per model call (model, input, cached and output tokens, duration, tool calls), per tool call (name, duration, failed or not), per turn, and per compaction (whether it took a model call). A tool call's approval notice is a debug record.

  `SUMMARY.md` comes first. It lists:
  - crashes, with each worker log's panics placed in time and grouped;
  - processes that started and never recorded an exit. They are still running, or were killed; a stack overflow leaves no panic;
  - the errors and warnings recorded, grouped by `kind.event`;
  - what the stores show went wrong, with alike problems grouped;
  - the turns, with their outcome, steps and tool calls;
  - the model calls: tokens, median, 95th percentile and longest duration, the slowest calls;
  - the compactions: how many took a model call and how many read a kept summary back;
  - the tools, by calls, failures and time;
  - one merged timeline.

  Options and limits:
  - `--task` and `--session` narrow the report to one conversation. Such a report keeps the diagnostics of the processes that worked on that conversation, and says how many others it left out.
  - Without `--include-content`, no request body, checkpoint or long setting is read, only its size.
  - `private/`, the kernel's pickled state, attachments and artifacts are never read.
  - Known secret values are redacted (the provider keys in the environment and in the credential file), and so are secret patterns. `manifest.json` has the count.
- **`/bug-report`** writes the same zip for the last 24 hours of the open conversation, into `<data dir>/bug-reports/`.
- **`ha diag timeline`** prints the merged timeline. It takes `--since 2h`, `--task`, `--session`, `--level warn` and `--json`.

### 12.13. Knowledge, Self-Evolution and Deep Dream

0.6.2 tightens the knowledge library, the automatic review (Self-Evolution) and the global memory consolidation (Deep Dream).

- **The knowledge catalog.** It is ranked the way the memory digest is: by the goal, what the user typed, the task state's recap, the user's previous messages and the feature in progress. Before, it was ranked by the message alone, so a "continue" listed the first pages by path. A catalog that leaves pages out ends with how many it left out, and its lines no longer carry the page hash (a read gives it).
- **Secrets.** A knowledge page never keeps a secret, whether the agent writes it or the review proposes it; the refusal names what it looks like. A refinement's memory and prompt-note writes pass the same checks as the `memory` tool: no secret, the length limits, and no duplicate. Covered by these checks are the automatic review, `/refine`, `/learn` and Deep Dream. A fact written anew in place of an entry that the same refinement deletes is not counted as a duplicate.
- **A review's pages.** The review sees the library ranked by the conversation, so it sees the page it would duplicate however large the library is.
  - A proposed page on a subject that a page already has is refused, and the refusal names that page.
  - A proposed page needs a verbatim quote from the conversation as its evidence.
  - A page the review wrote says so in its metadata (`origin: review`), and keeps that through later edits. `log.md` says who wrote each change.
- **The next turn is told.** What a review running beside the conversation changed reaches the next turn once, with the refinement id that `/refine --rollback` undoes.
- **Deep Dream** (the global memory consolidation):
  - It only organizes: it creates no entry.
  - An entry under `constraints` keeps that path, and a pinned entry stays pinned.
  - Neither is deleted unless a surviving entry keeps what it was.
  - One pass deletes at most half the entries. What the pass dropped from the model's edits is stated in its rationale.
  - It runs once enough changes have been saved (`memory_changes` counts every create, update and delete). The old sum of versions fell with every delete, so it could miss changes.
  - A pass that fails is tried again after an hour, instead of waiting for fifteen more changes.
  - It never runs at the same time as the skill curator's memory pass.
- **What is never learned.** The review, the refinement and the consolidation do not record:
  - an environment failure, such as a missing binary or interpreter, a credential or a network error;
  - a claim that a tool does not work;
  - an error that went away on retry.
- **Library upkeep.**
  - A full `log.md` drops its oldest lines; it used to refuse every later write.
  - An interrupted write whose file someone changed since is set aside as `.ha-pending.abandoned.json`, and the index is rebuilt. Before, the library refused every call, `/knowledge off` included.
  - A delete that backlinks block names the pages that link to the page.
  - A search across the project and global libraries merges them by rank. A keyword score and a hybrid score cannot be compared.

### 12.14. A review that is one transaction, page age, and reviews in `ha exec`

0.6.3 finishes the knowledge and Self-Evolution work of 0.6.2.

- **A review is one transaction.** The automatic review, `/refine`, `/learn` and the skill curator write knowledge pages and learned skills before the refinement that records them is applied. Each write is now noted in a pending record (`pending_refinements/` in the scope's harness directory) as it lands:
  - When the apply fails (the state's lock is held past its 30 seconds, a save or the history write fails), the writes are undone: a page is deleted while it is as written, a learned skill is restored. The error says what was undone. Before, the pages and skills stayed with no refinement to roll them back.
  - A review cut off mid-way (Ctrl-C in `ha exec`, the process stopped) is undone the same way. A process that died leaves its record behind, and the next refinement in that scope undoes it; one still running holds a lock on its record and is left alone.
  - When the local part of a review is applied but the global memory part fails, the local refinement stands: it is shown, noted for the next turn and the conversation is marked reviewed, and the global failure is reported beside it (`auto-refine: the global memory part failed: ...`).
- **Page age.** A knowledge page records when the store last wrote it (`updated_at` in its metadata; a retry that changes nothing keeps the time). A page written by hand, or before 0.6.3, is as old as its file. The catalog shows it (`"updated":"3d ago"`) and so does the list the review sees, so a page that records a state can be checked before it is trusted.
- **A conversation's own memory is consolidated.** A conversation's local store past 40 memory entries and prompt notes is consolidated after a turn, by the Deep Dream curator with the same guard rails, told that the current state of the task in progress may stay (one entry per subject, the latest). It is recorded as a local refinement that `/refine --rollback` undoes, and runs again only after 15 more changes. Notice: `memory: consolidating this conversation's memory in the background`.
- **`ha exec` reviews its conversation.** A headless run counts its turns and tool calls toward the conversation's next review, across `ha exec --resume` runs (`review_cadence.json` in the conversation's harness directory). When the work since the last review is enough for a session's end in the app (5 turns or 10 tool calls, or `autoRefine.turnInterval` turns when that is fewer), or the run compacted after doing anything, the review runs after the answer is printed and before the process exits; it reports on stderr. It follows `autoRefine.enabled` and `HA_AUTO_REFINE=off`, needs harness refinements on, and is skipped for `--mock` and cancelled runs. A local store past 40 entries is consolidated there too.
- **Hybrid search embeds every query variant.** With semantic search on, each of a search's `query`/`queries` variants is embedded (the ones not cached in one request), and each page is ranked by its best similarity to any of them; each result's `matched_queries` names the variants it is near. Before, only the first variant was embedded.
