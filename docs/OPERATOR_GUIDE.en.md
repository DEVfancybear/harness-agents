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

- **Maintenance has no daemon.** Every maintenance action below is a foreground
  command an operator runs; nothing retries or collects on its own. Interactive
  sessions do run in background workers ([12.7](#127-background-agents)).
- **There is no server to connect to.** Remote MCP endpoints and OS-level
  sandboxing are declared unsupported in the release matrix; transport isolation
  is not a sandbox.
- **There is no published release artifact.** Windows builds are exercised by
  the phase gates; `scripts/New-HaRelease.ps1` builds a local candidate with
  checksums ([BUILD_AND_RELEASE.md](BUILD_AND_RELEASE.md)), but nothing was signed
  or published.
- **Linux support is pending.** Its CI job runs for visibility and does not gate a
  push; the release matrix reports Linux as `unverified`.
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

- No background process keeps the system tidy. If you do not run `gc`, nothing is
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

The 18 core tools cross the host policy and receipt gate. Skill tools (`list_skills`, `activate_skill`, `read_skill_file`), web tools (`web_search`, `web_fetch`), the Python REPL (`ipython`) and, without the REPL, `mcp__<server>__<tool>` appear when available.

**MCP, as prime-agent reaches it.** With the Python REPL available, MCP servers are not native tools: the kernel's pre-imported `mcp` object (prime-agent's `rlm.mcp`, vendored) opens a configured server itself - `await mcp.list_tools("<server>")`, `await mcp.call_tool("<server>", "<tool>", arguments)`, `await mcp.list_connections()` - and the prompt lists the enabled servers. The servers are the ones `ha mcp add` configures; the host hands the kernel each server's configuration in prime-agent's shape (`secret://NAME` becomes an `{"env": "NAME"}` reference; `streamable_http` becomes `http` with `bearerTokenEnvVar`). The `mcp` Python package comes with the kernel venv. prime-agent's service catalog is not ported (plugin listings are empty); its OAuth sign-in is, as `ha mcp login` ([12.8](#128-more-from-prime-agent)). Without the REPL, or when a message attaches a server's resource with `@server:uri`, the servers are connected natively as before; `/mcp` says which route each server takes.

**Web.** `web_search` searches the web: Google through Serper when `SERPER_API_KEY` is set or `/login serper` saved a key (a free key at serper.dev), otherwise DuckDuckGo, which needs no key. `web_fetch` opens one http(s) page and returns readable text plus its links, in windows continued with `start_index`. Local and private addresses are refused, including as a redirect target; binary files are refused. `web_search` runs without a panel (it sends only the query); `web_fetch` asks, because a URL can carry data out - answer `a` to allow it for the turn, or use `full-auto`. `HA_WEB=off` removes both tools.

**Python REPL.** The `ipython` tool runs cells in one persistent Python kernel - prime-agent's own runtime, vendored under `crates/harness-cli/python` and written to `<data-dir>/runtime` on first use. Top-level `await` works; variables and imports persist across cells and turns; a kernel that dies is restarted on the next call and the result says the old state is gone. `bash('cmd')` starts a command in the background and returns a handle (`tail`, `output`, `poll`, `kill`, `await`); a command left running that finishes with nobody reading it is announced as prime-agent's `[bash-done pid:... exit:...]` - steered into a running turn, or a turn of its own when idle; on Windows it runs in Git Bash, found in its default location or set with `HA_REPL_SHELL`. A cell runs for at most 10 minutes, then it is interrupted; on Windows only a cell waiting at an `await` can be interrupted, so a cell stuck in synchronous code restarts the kernel. Running Python is running code, so each cell asks like `run_shell`, unless the mode is `full-auto`. The kernel starts with the project root as its working directory and the user's environment. It needs Python 3.11+ (`HA_PYTHON`, else `python3`, `python`, `py -3`); without one the tool is not offered. `HA_REPL=off` removes it.

With delegation available, the kernel's `rlm` object runs children as prime-agent does: `await rlm.spawn('task', name='worker')` starts a child and returns at admission; `await rlm.collect([...], timeout_ms=...)` waits for their answers; `rlm.list_subagents()`, `rlm.delete_subagent(...)` and `rlm.find_models()` work as in prime-agent. A child belongs to the session, not to the turn that started it: the turn may end while its children keep working. When a child settles and nobody is waiting for it, the parent is told with prime-agent's notice - `[child-failed child:<name>]`, `[child-exited: cancelled child:<name>]` or `[child-exited: no-reply child:<name>]` with its last answer - in a turn of its own when the parent is idle, or after the running turn ends (never steered into it). Ctrl-C stops the parent's turn, not its children; `/agents stop <name>` (or `/agents stop` for all) stops them, and `/new` or a resume into another conversation stops them quietly. A child runs on the parent's model unless `rlm.spawn(..., model='provider/id')` names another catalog model with a credential, else prime-agent's `subagentDefaultModel` in `settings.json`, else `[agents] default_model`; a model that cannot be used fails the spawn. `/subagent-model` shows that model and where it comes from; `/subagent-model <provider/model>` (Tab or Enter lists the models) saves `subagentDefaultModel` for the next child, and `/subagent-model inherit` removes it. `/subagent-login <provider>` logs the delegated children in with an account of their own - an API key, or the ChatGPT browser sign-in - kept in `subagent-auth.json` beside `auth.json`: a child uses its own login for a provider when there is one and the main model's otherwise, and the main model never uses the children's. `/subagent-logout <provider>` removes it. A child runs at the parent's thinking level unless `rlm.spawn(..., thinking='high')` names one - as in prime-agent, a level the child's model does not offer fails the spawn - or `/subagent-effort <level>` saved `subagentDefaultThinking` in `settings.json` (clamped to the nearest level the child's model offers; `/subagent-effort inherit` removes it). Every message the agents of a session send each other (`agent_message`, `rlm` messaging) shows in the conversation as prime-agent's row, `◆ agent message · <from> → <to> · <start>`, with the whole message in the details and expanded modes (Ctrl-O); `/agents messages` lists every exchange of the session, whole and in order. `rlm.create_session` starts a separate top-level agent ([12.7](#127-background-agents)).

**Python skills.** prime-agent's skills ship with the app (`.agents/skills`, MIT; see `.agents/PRIME-AGENT-SOURCE.md`): `edit`, `websearch`, `attach_image`, `goal`, `compact`, `refine`, `agent_message`, `agent_observe`, `rlm_heartbeat`, plus the `mcp` and `skill-creator` guides. A skill with a Python package is imported into the kernel by its name when the kernel starts - `await edit(path=..., old_str=..., new_str=...)`, `await goal.complete()`, `await compact.run()` - and listed in the prompt with its `python_import`. A skill whose import fails is replaced by a stub that says why, and the first cell reports it. The host answers the skills' requests: `goal.*` drives `/goal` (a goal the model creates is carried like one you set, with its token budget), `compact.run` schedules `/compact` for the end of the turn, `model.info` names the model, `agent_observe` reads the session's children, `rlm_heartbeat` keeps recurring prompts for the session (`every 5m` by default; a `steer` one reaches a running turn through the `/steer` inbox, a `follow_up` one waits for it to end), and the images `attach_image` loads are shown to the model with the tool result. `agent_message.send(message, receiver_role='child', receiver_name=...)` (or `'all'`) messages running children; a child messages its parent or a sibling with its own `agent_message` tool, and leaves a one-line `progress_note` its parent reads in `list_subagents` and `/agents`. As in prime-agent a message holds at most 16 384 characters, a sender gets three at once and one more each second, and it arrives as `[agent-message from <relationship>:<name>]` at the recipient's next step - or, for an idle parent, as a turn of its own. A message to a child that has finished is refused (a finished child is not woken again).

**Kernel venv.** As prime-agent does, the kernel runs in a venv under `<data-dir>/kernel-venv`, built with `uv` on first use - Python 3.11, `dill`, prime-agent's default packages (requests, httpx, pyyaml, tomli, python-dotenv, pandas, numpy, scipy, beautifulsoup4, lxml, pydantic, tyro) and `pillow` - and rebuilt when that list changes. ha never installs `uv` itself: without it the kernel runs on the system Python, the first cell says so, and skills whose packages are missing (`websearch` needs httpx, `attach_image` needs pillow) are reported unavailable. `HA_PYTHON` names an interpreter to use as it is.

**Skills** follow the Agent Skills layout (a directory with `SKILL.md` plus its own files). They are found in the bundled set, `<config-dir>/skills`, `~/.agents/skills`, every directory listed in `HA_SKILL_PATHS` (separated like `PATH`, e.g. `~/.claude/skills`), and - for a trusted project - `.harness/skills` and `.agents/skills` in the workspace and each parent up to the Git root. The system prompt lists each skill's name, description and location in an `<available_skills>` block, the way prime-agent does; the model activates one by name (the digest from `list_skills` is an optional pin, accepted with or without `sha256:`). Activation adds the instructions plus the skill's directory and file list, and `read_skill_file` reads those files - only inside that skill. The three skill tools only read the trusted catalogue, so they run without an approval panel; a deny rule still blocks them. `disable-model-invocation: true` in the front matter hides a skill from the model; `/skill:<name>` still runs it. Written at the start of a message, `/skill:<name> [args]` sends the skill with the rest as the request, as prime-agent does; written inside a message (`... using /skill:<name>`), it sends that skill with the whole message as the request.

| Group | Tools | Purpose |
| --- | --- | --- |
| Workspace | `read_file`, `list_files`, `search_text`, `glob` | Bounded reads and searches within the workspace |
| File edits | `apply_patch`, `write_file`, `edit_file` | Writes with hash or match conditions and approval; `write_file` creates missing directories inside the workspace |
| Process | `run_process`, `run_shell`, `read_process_output` | Run commands and read bounded output artifacts |
| Git | `git_status`, `git_diff`, `git_log` | Observe the repository |
| Task | `task_update`, `delegate` | Record a next action; delegate to explorer or coder |
| History | `history_search`, `history_read` | Search and read the task journal |
| Human | `ask_user` | Pause to ask; does not grant tool permission. Its options are a menu (arrows and Enter, or an option's number while the composer is empty); typing answers in the composer. A delegated child's question is shown named after the child and the child waits for the answer; questions and approvals from several agents open one after another |

As in prime-agent, children nest up to `RLM_MAX_DEPTH`, 2 by default: a child of the root agent gets a `delegate` tool of its own that starts explorers (a coder is started by the root only), and a child is not finished until its own children are - their notices open one more turn of the child, which then reports to its parent. A child messages its parent, siblings and its own children (`agent_message` with `receiver_role` `parent`, `sibling`, `child` or `all`); a grandchild does not reach the root. `/rlm-max-depth` shows the value and where it comes from (chat, global, env or default); `/rlm-max-depth <n>` sets it for this conversation and `/rlm-max-depth <n> --global` also saves `rlmMaxDepth` in `settings.json` beside the user config; `RLM_MAX_DEPTH` is read next. ha runs at most 2 levels (the delegation contract's cap); a child keeps the value it was created with. Each level runs at most three children at once. As a prime-agent child does, a child has its parent's tools and permissions - the permission mode, the allow and deny rules and what the user allowed for the turn: an explorer (and an `rlm.spawn` child) works in the parent's workspace, a coder in a host provisioned clean Git worktree. If a worktree cannot be provisioned, the tool returns `role_unavailable`. `/agents` lists every child with its role, model, state, time, tool calls, cost and latest note (`/agents messages`: what the agents told each other); `/agents stop [name]` stops them (Ctrl-C does not). `delegate` waits for the child's answer by default; with `wait: false` it returns at once and the result arrives as a notice. Several `delegate` calls in one response run side by side. A child inherits the parent's web tools (`web_search`, `web_fetch`). A child that breaks - a provider error, a loop, an empty reply - comes back as a failed result the parent can act on, `[child-failed explorer] <code>: <reason>`, never as completed. Any turn, parent or child, stops as `loop_detected` when it reads the same thing (same tool, same arguments) three times with nothing changed in between, even when other calls come between the repeats.

### 12.2. Chat commands and keys

| Group | Commands |
| --- | --- |
| Help and state | `/help`, `/hotkeys`, `/fullscreen`, `/session` (`/status`), `/config`, `/model [search]`, `/effort [level]` (`/thinking`), `/cost`, `/context` (`/usage`), `/system-prompt`, `/permissions`, `/hooks`, `/mcp`, `/agents`, `/subagent-model`, `/subagent-effort`, `/subagent-login`, `/subagent-logout`, `/rlm-max-depth`, `/skills`, `/logs` (where the logs are: every `*.log` under the data directory) |
| Session and answer | `/new` (`/clear` also clears the viewport), `/resume [id]`, `/name [name]` (`/rename`), `/more`, `/compact [instructions]`, `/export [path]` (an HTML page by default; `.md`, `.html` for one self-contained page, or `.jsonl` for ha's session file - prime-agent's JSONL shape, a header then one line per message - holding what the model is sent for this conversation, secrets redacted), `/import <path.jsonl>` (a new conversation that continues an exported one, or a prime-agent session file; the file is copied under `<data>/imports`), `/share`, `/new [prompt]`, `/heartbeat <instruction>`, `/heartbeats`, `/copy`, `/fork [n]`, `/clone`, `/tree [n]`, `/btw <question>` (`/side`), `/quit` (`/exit`) |
| Workspace and control | `/undo`, `/trust [yes]`, `/init`, `/permissions [ask|auto-edit|full-auto]` (`/permission`, `/mode`), `/steer <text>`, `/queue [text|list|edit n text|drop n|up n|down n]` (`/followup`), `/stash`, `/goal <objective>|status|pause|resume|clear`, `/autonomous [status|off|on ...]`, `/schedule [list|add <when> -- <prompt>|pause|resume|cancel <id>]` |

**Queue, stash, side questions and branches (prime-agent's).** Enter while the agent works steers the running turn; a message the turn cannot take yet waits in the steering lane. `/queue <text>` adds a follow-up that runs as its own turn after the running one; `/queue` lists the queue and `/queue edit|drop|up|down <n>` changes it. What waits is sent after the turn - steering first, then follow-ups - one message per turn, or all of a lane at once with `[queue] steering_mode = "all"` / `follow_up_mode = "all"`. Inside a running turn steering follows the same mode, as prime-agent's loop does: one message reaches each model call, after the tool results of the step it arrived in. After Ctrl-C the queue (and any child reports) waits for your next turn. Ctrl-S (or `/stash`) puts the draft aside and, on an empty prompt, brings it back. `/btw <question>` asks the model about the conversation without tools and without adding anything to it; the answer opens in a panel, and a later `/btw` follows up. `/fork` lists your messages and `/fork <n>` starts a new conversation just before message n, with that message back in the editor; `/clone` starts one with the whole history; both show up separately in `/resume`. `/tree` lists this conversation's turns and `/tree <n>` continues after turn n; `/tree <n> --summarize [focus]` also has the model summarize the turns it leaves with prime-agent's branch prompt (the auxiliary model if one is set), and the next message is read with that `[branch-summary]` ahead of it. `/tree label <n> [text]` labels a turn, shown as `[label] ` in the list; an empty text clears it.

**Your own models, scoped cycling and model routing (prime-agent's).** A `models.json` beside the user config (`<HA_HOME>/models.json`) adds models in prime-agent's schema - `{"providers": {"<id>": {"baseUrl", "api", "apiKey", "models": [{"id", "name", "reasoning", "input", "cost", "contextWindow", "maxTokens"}], "modelOverrides": {"<model id>": {...}}}}}`, `//` comments allowed. `api` is `openai-completions`, `anthropic-messages`, `openai-responses` or `openai-codex-responses`; a new provider needs `baseUrl` and `apiKey`; missing fields default to prime-agent's (`contextWindow` 128000, `maxTokens` 16384, text input). `apiKey` is the **name of the environment variable** that holds the key, never the key. A model with a built-in provider and id replaces it, and `modelOverrides` change only the fields they name. A file that cannot be used is reported once at start (`models.json: <problem> - using built-in models only`). The models show up in `/model` once their key is set.

`[routing]` in the config: `scoped = ["deepseek/*", "openai/gpt-5*:high"]` (globs over `provider/id` or `id`, any case, optional `:level`) are the models `/model next`, `/model prev`, Alt+M and Shift+Alt+M move through - those with a credential, at least two. `/scoped-models <pattern>...` saves a scope for this and later launches (above the config, like `/model`), `/scoped-models` shows it, `/scoped-models clear` drops it. `auxiliary = "provider/id"` writes compaction summaries and `/refine` reviews (if it cannot, the session model does, with a notice). `backup = "provider/id"` takes over the turn when the session model has failed with a rate limit or an unavailable service on every retry (`Primary model unavailable (...) — retrying on backup model ...`); the next turn goes back to the session model (`Primary provider recovered — back on ...`). `image = "provider/id"` answers requests with images when the session model's catalog entry takes text only; without it such a request fails with `This model does not accept images; set [routing] image in config` instead of sending the image to a text model. A rate-limited provider (HTTP 429, or a structured quota code such as `insufficient_quota`; error code `rate_limited`) is waited out as prime-agent does - 1 s doubling to 5 min a check, 30 checks and 15 minutes at most, the server's `Retry-After` first - with `Waiting for provider usage to recover (n/30), next check in Ns... (esc to cancel)` in the status line; Esc cancels. `wait_for_usage = false` turns the wait off. When the provider reports a usage reset beyond that bound - its `Retry-After`, or a window its error names such as "Try again in ~7272 min" - the session is parked as in prime-agent: the turn ends with `Session parked until <time> and will resume automatically`, and a one-shot job kept with the conversation (`/schedule` lists it, `/schedule cancel <id>` drops it) sends prime-agent's resume prompt 30 seconds after the reset, at most a day later. The job survives quitting `ha`: it runs when the conversation is open again. `/tier` shows the service tier in force and the ones the model takes; `/tier default|flex|priority|auto` sets it and `/fast` toggles `priority`, as in prime-agent. The tier is sent as the request's `service_tier` to OpenAI (API key: `auto` for every model; `priority` and `flex` for gpt-5.4, gpt-5.5, gpt-5.6, gpt-5.6-* and gpt-6-astra) and to ChatGPT sign-in (the same, without `flex`); other providers take the default only, and a tier the model does not take is sent as `default`. It is kept with the conversation and saved as `defaultServiceTier` in `settings.json` beside the user config for new ones; the status line shows `fast` for priority and the tier's name for another one.

**Autonomous mode and schedules (prime-agent's).** `/autonomous on` keeps the session working after the model stops: each turn that ends cleanly is followed by another with prime-agent's continuation prompt, until a budget runs out - by default 3 continuations, 12 turns, 80,000 tokens and 30 minutes, checked in that order (`--max-continuations`, `--max-turns`, `--max-tokens`, `--timeout-ms`; naming any of them makes the unnamed ones unlimited; `unlimited` is accepted). `--gate "<command>"` (repeatable) adds a quality gate: after each turn the gates run in the workspace, in the shell and scrubbed environment the model's shell tool gets (5 minutes each, `--gate-timeout-ms`); all passing ends the run (`autonomous: quality gates passed`), a failure continues it with the command's exit and output (`[autonomous-continuation: gate-failed] ...`), and more than `--gate-retries` (3) failures end it. A gate that failed is not rerun while the git worktree is unchanged; that still counts as an attempt. While delegated children work, the run waits for their reports. `/autonomous` shows `[autonomous-status: ...]`; `/autonomous off` stops it (and any running gate). A goal and autonomous mode do not run together: setting one turns the other off, with a notice. `/schedule add <when> -- <prompt>` schedules a prompt for this conversation: `in 10m` / `in 2h` / `in 1d` (once), `every 30s` / `every 1h` (at least 10 seconds), `at 2030-01-01T09:00:00`, five-field cron in local time (`0 9 * * 1-5`) or `@hourly|@daily|@weekly|@monthly`; `--steer` sends it into a running turn instead of after it. Jobs are kept in `<data dir>/schedules/<task>.json` and come back when you resume the conversation; they run only while `ha` is open, runs missed while it was closed run once, and the next run counts from then. `/schedule` lists the jobs, `/schedule pause|resume|cancel <id>` changes one. A due job arrives as `[heartbeat: <when> run#N]` followed by the prompt.
| Content and extensions | `/login`, `/logout`, `/image`, `/attach <path>`, `/skill:<name> [args]`, `/reload`; templates in `.harness/commands` or `<config-dir>/commands` run as `/name` |

The command table, its order, wording and aliases follow prime-agent's `slash-commands.ts`, and so does the menu: typing `/` lists every command, letters narrow it with prime-agent's fuzzy match (`/skil` offers `/skills` and every `/skill:<name>`; prompt commands join too), and a command that takes an argument opens an argument menu on Tab or Enter - `/effort` offers the levels, `/model` the models, `/login` the providers - where Enter on a value applies it. A mistyped command is answered with the closest one. `/effort <level>` (alias `/thinking`) chooses how much the model reasons, from `off` through `minimal`, `low`, `medium`, `high`, `xhigh` to `max`, as prime-agent does; `/effort` alone shows the level in force and the levels the model offers. A level the model lacks is clamped to the nearest one it has (DeepSeek V4 offers `off`, `high` and `xhigh`, sent as `max`). The choice is kept with the conversation and, as prime-agent's `setThinkingLevel` does, saved as `defaultThinkingLevel` in `settings.json` for new conversations (not an `off` on a model that does not think); `provider.thinking` / `HA_PROVIDER_THINKING` is read after it. With thinking on, DeepSeek gets each assistant message's `reasoning_content` back and Claude its signed thinking block, within the turn; reasoning is never stored. A model the built-in table does not know is treated as not reasoning.

**While the agent works**, as in prime-agent and Claude Code: a message sent with Enter joins the running turn - the model reads it at its next step, or right after the answer it is writing, and the turn goes on - instead of waiting for the task to finish (it is queued only when the turn cannot take it yet). `/effort`, `/model` and `/permissions` are applied from the running turn's next model call or action, not refused.

`/goal <objective>` sets a persistent goal. Every turn carries it in its context, and the model gets a `goal_complete` tool (allowed without asking). A turn that ends without `goal_complete` is continued automatically, at most 10 times, then the goal pauses. Ctrl-C and `/goal pause` pause it; `/goal resume` continues it; `/goal clear` removes it. The goal is stored with the conversation: `/resume` restores it paused, `/new` drops it. Inside one turn, once the turn's own transcript passes about 200 KB, the oldest tool results (all but the newest six) are replaced by a one-line note; the model can call the tool again.

`@` opens the file picker; `@<server>:<uri>` attaches an MCP text resource. `!cmd` runs shell through approval; `!!cmd` only displays output. Enter submits, Ctrl-J inserts a newline, ↑↓ chooses a menu entry or history item, PgUp/PgDn and Home/End scroll the `/more` panel, Esc closes a panel or interrupts the turn, Ctrl-C cancels the turn (twice within 2 seconds on an empty prompt exits), Ctrl-O cycles the detail mode as prime-agent does: collapsed hides reasoning and cuts every tool output to three lines, details shows reasoning and the full diffs of file edits, expanded shows every output line; a call's card looks the same in all three. Ctrl-D on an empty line exits. As in prime-agent, the TUI renders **fullscreen** by default: the alternate screen holds a scrollable window over the conversation with the composer, hints and status line pinned to the bottom and the chat's name (its `/name`, else the workspace directory) and spend pinned to the top. The mouse wheel scrolls three rows, PgUp/PgDn a page (a panel or menu keeps its own PgUp/PgDn), Shift+Alt+Up goes to the top and Ctrl+End or Ctrl+Shift+Down back to the end, following the output again (`ctrl+end to follow` shows while you read back; Windows Terminal keeps Ctrl+Shift+Down for its own scrollback). Dragging selects text - it stays on the rows it covered while the answer streams, and a drag held at an edge scrolls on - and releasing copies it (`Copied selection to clipboard`). Leaving fullscreen prints what it showed into the terminal's scrollback. `/fullscreen [on|off]` switches at once and keeps the choice as `terminal.fullscreen` in `settings.json`; `terminal.fullscreenMouse: false` leaves the mouse to the terminal, and `HA_FULLSCREEN` (prime-agent's `PI_FULLSCREEN`: on only for `1`) wins over both. Inline, TUI history remains in terminal scrollback; plain mode prints lines. As Codex does, a launch says `Update available! <this> -> <newest>` when the npm registry lists a newer `harness-agents` (asked on a background thread at every launch and kept in `<data-dir>/cache/version.json`, so the notice shows from the launch after a release); `HA_NO_UPDATE_CHECK=1` or `"checkForUpdates": false` in `settings.json` turns it off.

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

When the model asks for several tools at once they run side by side, as in prime-agent; a batch holding `ipython`, a file write, `run_process`, `run_shell` or `ask_user` runs one call at a time. Approvals, intents and receipts stay in call order. Read-only tools no longer fingerprint the workspace, and a fingerprint rehashes only files whose size or modification time changed. On Windows `run_shell` uses `pwsh`, falling back to `powershell.exe` when pwsh is missing; the receipt records the selected shell. Strict isolation is claimed only where a measured backend supports it. Start with `/status` and `/config` when provider or permissions differ from expectations. As in prime-agent, a turn has no step, tool-call or time bound: it runs until the model is done, you stop it, or the tokens run out; `HA_TURN_MAX_STEPS`, `HA_TURN_MAX_TOOL_CALLS` and `HA_TURN_DEADLINE_SECONDS` set one, and a turn that hits it is continued automatically up to twice (`HA_TURN_CONTINUATIONS`). The whole conversation is kept, and when a request nears the model's window (the window minus the answer's room and a reserve of up to 16384 tokens) it is compacted as prime-agent does: the earlier turns become a summary the model writes while the recent end (about 20000 tokens) stays word for word, then the turn's oldest tool results are shortened, and the turn goes on. `/compact` and the automatic checkpoint summarise the whole conversation, not only the last turn. Only a request that still cannot fit the window fails. If `ha` behaves like an older build, `Get-Command ha -All` shows which executable runs; reinstall with `scripts/Install-Ha.ps1`. M0–M6, H, and PTY gates have separate evidence. Linux support is pending: its CI job does not gate a push, and no Linux claim is made from it.

### 12.6. `/resume`

`/resume` replays the selected conversation's durable turns to the model and shows them on screen. It does not infer or create an additional cross-session data source.

### 12.7. Background agents

Like prime-agent's daemon, an interactive session runs in a background worker and the terminal attaches to it. Closing the terminal - `/quit`, ctrl+d, closing the window - detaches it: the running turn finishes, and a goal, the autonomous gates, queued messages, heartbeats, `/schedule` jobs and delegated children carry on. The terminal says so on the way out (`agent <id> keeps running in the background`).

| Command | What it does |
| --- | --- |
| `ha agents` (`ha list`) | Every running agent: id, name, status, idle time, project and last request; `*` marks one a terminal is attached to. `--json` for scripts. Agents a reboot or a dead worker left in a journal are started again first. |
| `ha attach <agent>` | Bring the agent back to this terminal: the conversation so far is drawn, and typing goes on. Several terminals can attach to one agent, as prime-agent's clients view one session: each draws what the agent does, a key is answered to the terminal that typed it, and `/quit` detaches only that terminal. |
| `ha send <agent> "<message>"` | prime-agent's `send`: an idle agent starts a turn with it; a busy one reads it at its next step (`--steer`, the default) or after its turn (`--follow-up`). `--from <agent>` names the sender. |
| `ha rename <agent> <name>` | A name `attach`, `send` and `stop` accept. |
| `ha stop <agent>` | Stop the agent: its turn and its children are cancelled. The conversation stays in the store. |
| `ha shutdown [--force]` | Stop every agent and worker; without `--force` it asks first. |

`<agent>` is an id, a name or an id prefix no other agent shares.

One worker serves one project, and all of that project's agents share its store, so two agents of a project run side by side instead of waiting on the store's writer lock. In the agent's Python REPL, `await rlm.create_session('<prompt>', name=..., model=..., thinking=..., cwd=...)` starts a separate top-level agent (prime-agent's API): in the same worker, or in the worker of the project `cwd` belongs to.

An agent with no terminal, no running work and no schedule stops after prime-agent's `idleEvictionMinutes` (90 by default; set a number or `"off"` in `settings.json`). The worker leaves when its last agent is gone. Its descriptor (port and token, owner-only) and its log are in `<data-dir>/workers/`; a connection must present the token, and the client's environment - where the credentials come from - travels over the loopback socket and is never written.

`ha exec` runs its turn in the project's worker as well, as prime-agent runs a headless session through its daemon: the turn writes through the store the agents share, so an agent in the middle of a turn no longer makes `ha exec` fail with `writer_locked`. Its output, JSON and exit code are the same as before; Ctrl-C cancels the turn. With `HA_DAEMON=off`, or when no worker of this build runs, the turn runs in the calling process. A worker is started from a terminal only: one started by a command whose output is redirected would keep that output open for as long as it runs (on Windows a child inherits the handles), so `ha exec ... | tool` would never end.

As prime-agent's supervisor does, a worker runs under a supervisor process that starts it again when it dies - after 250 ms, 1 s and 5 s; a worker that keeps dying is given up. The worker keeps a journal of its agents (`<data-dir>/workers/<project>.journal`: ids, names and conversations - never the environment), and a worker that starts brings them back on the same ids and conversations, then opens every conversation of the project that still has a `/schedule` job or a quota park, so the job fires with no terminal open. After a reboot the next `ha` in the project, or `ha agents` in a terminal, starts the worker and the agents come back; they run on the environment of the terminal that started the worker. When `ha` is updated, a terminal of the new build that finds the old build's worker idle has it stop with its journal kept and starts a new one, which brings the agents back; a worker whose agents are working is left alone, and the terminal says so and runs the session itself.

Where ha differs from prime-agent: there is one worker per project rather than per session, and a turn a crash interrupts is not resumed - the recovered agent continues the conversation, and the interrupted turn is replayed as interrupted. `HA_DAEMON=off` or `"daemon": false` in `settings.json` runs the session inside the terminal as before; the plain renderer always does.

### 12.8. More from prime-agent

**Entry points.** `ha "fix the parser"` opens the app and sends the message; with input piped in (`cat log | ha "why?"`) the input follows the message and the answer is printed, as `ha exec` prints it. `ha exec` (and `ha chat --headless`) take `--model`, `--thinking`, `--system-prompt` and `--append-system-prompt`. `ha model list [search] [--json]` lists the catalog models with a credential, `ha prompt [--cwd] [--json]` prints the system prompt a session would get. `--offline` (or `HA_OFFLINE=1`) skips every startup download - the model catalog, the logged-in providers' model lists and the update check.

**RPC and ACP.** `ha --mode rpc [--model M]` serves prime-agent's RPC mode: JSON commands on stdin (`prompt` with `streamingBehavior` `steer` or `followUp` while a turn runs, `steer`, `follow_up`, `abort`, `new_session`, `get_state`, `set_model`, `cycle_model`, `get_available_models`, `set_thinking_level`, `compact`, `get_last_assistant_text`, `set_session_name`), responses `{id, type: "response", command, success, data | error}` and prime-agent's session events (`agent_start`, `message_start` / `message_update` / `message_end`, `tool_execution_start` / `_update` / `_end`, `compaction_start` / `_end`, `agent_end`) on stdout. `ha --mode acp` serves the Agent Client Protocol for editors over stdio: `initialize`, `session/new` (one session per connection, in the `cwd` it names), `session/prompt` streaming `session/update` chunks and tool calls and answered with its `stopReason`, `session/cancel` and `session/close`. Nobody can answer an approval in either mode, so a gated action is refused, as `ha exec` refuses it.

**Background agents.** `ha send --wait` prints the answer of the turn the message started; `ha abort <agent>` ends an agent's running turn and keeps the agent. `ha send <session id> "<message>"` to a saved conversation that no agent runs resumes it as a new agent, with the message as its first prompt. `ha schedule list [--all] [agent] [--json]`, `ha schedule add <agent> <when> -- <message>` and `ha schedule cancel [agent] <job-id>` manage an agent's scheduled prompts through its worker. Heartbeats (`/heartbeat <instruction>`, `/heartbeats`) are kept with the conversation like `/schedule` jobs, and stopping an agent cancels its schedules.

**Prompt and screen.** The prompt edits as prime-agent's does: word motion (Ctrl/Alt+Left/Right, Alt+B/F), Ctrl+K and Ctrl+Y (kill ring), Alt+D, Ctrl+T, Ctrl+Z / Ctrl+- undo and Ctrl+Shift+Z redo, Ctrl+D deletes forward, and a large paste shows as `[paste #N +L lines]`, sent as the text it stands for. Ctrl+G edits the message in `$VISUAL` / `$EDITOR`. A second Esc within half a second clears the draft, or opens the session tree on an empty prompt. A bare `/model` opens the model menu; `/tree` and `/fork` open their selectors (arrows, Enter, Esc). A leading command, `@path` and `--flag` tokens are coloured as in prime-agent. While messages wait, the queue strip above the prompt shows each one (`Steering:` / `Follow-up:`). While a tool call's arguments stream the working line reads `Writing code`. A multi-line error shows its summary line until Ctrl+O expands it. Copies are acknowledged with a short toast, also through OSC 52 over SSH and tmux. Markdown tables and links render as in prime-agent. `keybindings.json` beside `settings.json` rebinds prime-agent's binding ids - `{"app.editor.external": "ctrl+e", "tui.editor.yank": []}`: a configured binding answers to exactly the keys given, and an empty list turns it off.

**Models and providers.** `allowedModels` in the global `settings.json` (exact `provider/id`, bare id or glob) bounds the models a turn, `/model` and a delegated child may use; a model outside it fails with prime-agent's refusal and never falls back to another. Anthropic gets prime-agent's `cache_control` (`HA_CACHE_RETENTION=long` for one hour), fine-grained tool streaming and interleaved thinking; cached prompt tokens are counted and priced apart. A request the provider reports as too long is compacted and sent again; compaction writes prime-agent's structured summary and updates it the next time instead of summarizing again, and after a compaction the next prompt opens with prime-agent's `[python-state]` note naming what the kernel still defines. `/login serper` saves the Serper key for `web_search`.

**MCP servers.** `ha mcp login <server> [--client-id] [--scope]` signs in to a Streamable HTTP server that asks for OAuth (RFC 9728 / RFC 8414 discovery, dynamic client registration, PKCE on `localhost:53700-53709` or a pasted redirect URL); the tokens are saved in `auth.json` under `mcp:<server>`, refreshed before they expire and sent only to that server's URL, and `ha mcp logout <server>` forgets them. A server takes `enabled = false`, `headers`, `header_env` (a header from an environment variable) and `startup_timeout_seconds`.

**Conversation.** Goals carry a token budget (`goal.create(..., token_budget=...)`) and pause as budget-limited when it is spent. `/share` publishes the conversation as a secret GitHub gist (through `gh`); `/new <prompt>` starts a new conversation with that prompt; `/export` writes an HTML page by default. `/import` also reads prime-agent session files. `/btw` declares the session's tools and refuses every call, as prime-agent's side question does. `autoRefine.enabled` and `autoRefine.turnInterval` in `settings.json` control the automatic review. `run_shell` refuses a git command that would discard uncommitted work (`git checkout -- .`, `git reset --hard`, `git clean -f`, ...) unless `HA_ALLOW_DESTRUCTIVE_GIT=1`. `rlm.rename(...)` renames the session or a direct child.

Not ported: Claude Pro/Max, Copilot and xAI sign-ins (they impersonate another client), the Prime platform pieces (inference, traces, telemetry, cloud, the service catalog, Factory), and the agents view, activity dock, `/settings` and themes of prime-agent's TUI.
