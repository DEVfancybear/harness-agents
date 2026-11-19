# Command and secret guards

English | [Tiếng Việt](GUARDS.vi.md)

What a command or a file write must never do, in any approval mode. These are
the guard hooks of [Agnostic-AI](https://github.com/ucsandman/Agnostic-AI)
(read at commit `aab09ff`), ported into ha's tool dispatch. They are compiled
Rust, not hook scripts: a hook with a syntax error cannot switch them off.

Code: `crates/harness-tools/src/security/command_guard.rs` and
`secret_values.rs`, called from `runtime/service.rs` and the process drain in
`processes/process.rs`.

## 1. What ha already had, and what was added

| Agnostic-AI hook | Before | Added |
|---|---|---|
| `secret-path-guard` | The file tools refuse `.env*`, `.git`, `.harness`, names with `credential`/`secret`/`password`/`private_key`, `*.pem`/`.key`/`.p12`/`.pfx` | The same files through the shell (`cat .env`, `Get-Content .env`, `> .env`, `git add .env`) |
| `secret-guard` | A knowledge page holding a key is refused | A file write (`write_file`, `edit_file`, `apply_patch`) that brings in a key-shaped value the file does not already hold |
| `tool-output-secret-watch` | A command gets no host variable but those on an allowlist, so an environment dump shows no host key; granted `secret://` values are redacted; `key=value` lines are redacted in previews | Any key-shaped value in a command's output is redacted before the model, the spool file or a preview sees it |
| `git-tree-guard` | `git_guard.rs`: a discard of uncommitted work (`reset --hard`, `checkout -- .`, `clean -f`, `restore .`) is refused | (kept) |
| `rm-guard` | none | Recursive deletes |
| `process-kill-guard` | none | Kills by name |
| `slow-command-guard` | none | Recursive searches of a whole drive or the home folder |
| `gate-freeze` | The file tools stay in the workspace | ha's configuration, hooks and state, and the project's `.harness`, are not changed through the shell |

Not taken:
- `scope-lock` (the maintainer's decision);
- `guard-canary` (compiled guards cannot be disabled by a broken script);
- `mods-liveness` (Claude Code only);
- `dev-server-guard` and `security-tier-check` (ha's approval modes and strict
  execution cover them);
- `slopsquat-guard` (it needs a registry lookup in the tool path; left for a
  later change).

The end-of-turn verification reminder (`forced-verify-stop-gate`) builds on what
ha had: the turn driver asked a `CompletionCheck` whether an answer that changed
files and ran nothing after them claims success, and sent it back once. Only the
jev plugin implemented that check. Now a turn without a check is reminded too
(`TurnDriver::with_verify_reminder`):
- When a turn changed a file that is not prose (`.md`, `.markdown`, `.txt`,
  `.rst`, `.adoc`) and ran no command after its last change, the model is told
  once to run the project's check on it and report the result, or to say in one
  line that it cannot be checked here or is not verified.
- The conversation shows `verify reminder: code changed and nothing ran after
  it`.
- It is on for the chat, `ha exec` and children. `"verifyReminder": false` in
  settings or `HA_VERIFY_REMINDER=off` turns it off.
- With jev's completion check on, jev judges instead.

## 2. How a command is read

`command_guard::parse` reads a command line the way bash and PowerShell read
one:

- single and double quotes, and `#` comments;
- separators `;` `&&` `||` `|` `&` and newlines, and `( )` and `{ }`;
- `$(...)` substitutions, whose commands are commands too;
- redirections (`>`, `>>`, `2>`; `2>&1` is a stream, not a file);
- leading `sudo`, `env`, `nice`, `time`, `command`, `exec`, `nohup` and `NAME=value`
  assignments, which are dropped;
- PowerShell's `&` call operator;
- `${HOME}`.

A shell that runs a command given as an argument - `bash -c "..."`,
`powershell -Command ...`, `cmd /c ...` - has that command read too, up to four
levels deep. A `run_process` call is read as one command, its executable first.

## 3. The guards

Each refusal starts with its guard's name in brackets and names the variable
that lifts it. The variable is read from ha's own environment when ha starts,
never from the command, so `HA_ALLOW_...=1 rm -rf x` lifts nothing.

| Guard | Refuses | Lifted by |
|---|---|---|
| `recursive-delete` | `rm -r`/`-rf`/`--recursive`, `Remove-Item -Recurse`, `del`/`rd`/`rmdir /s`, `find ... -delete`, unless every target can be had back | `HA_ALLOW_RECURSIVE_DELETE` |
| `kill-by-name` | `Stop-Process -Name` (and `spps`, `kill`), `Get-Process <name> \| Stop-Process`, `taskkill /IM`, `pkill`, `killall`, `& $var -Name`, `iex ... -Name` | `HA_ALLOW_KILL_BY_NAME` |
| `root-search` | `grep -r`, `rg`, `ag`, `find`, `fd`, `Get-ChildItem -Recurse`, `dir /s`, `ls -R` from `/`, a drive (`C:`, `/c`, `/mnt/c`), `C:\Users`, `~`, `$HOME`, `$env:USERPROFILE` or the home folder | `HA_ALLOW_ROOT_SEARCH` |
| `secret-path` | Any command naming a secret file, except those that only check whether it exists (`echo`, `Test-Path`, `ls`, `git status`, `git rm --cached`, ...) | `HA_ALLOW_SECRET_PATHS` |
| `harness-integrity` | A command that changes (removes, moves, copies onto, writes, `sed -i`, `chmod`, ...) or redirects into ha's configuration directory, its data directory, or the workspace's `.harness` | `HA_ALLOW_HARNESS_WRITES` |
| `secret-write` | A file write that brings in a key-shaped value | `HA_ALLOW_SECRET_WRITES` |

**When a recursive delete may run.** Every target must be one of:
- a build or cache folder (`node_modules`, `dist`, `build`, `target`, `out`,
  `.next`, `coverage`, `__pycache__`, `.cache`, `tmp`, ...), reached without `..`;
- inside the workspace and in git, with `git status --porcelain
  --untracked-files=all --ignored=matching` listing nothing under it: every
  file is tracked and unchanged, so git can give it back.

The rest is refused:
- a target outside the workspace (the system's temporary folder too: it holds
  other programs' files);
- the whole workspace;
- a variable, glob or `~`;
- a relative path after a `cd`;
- a delete fed by a pipe.

**Secret files** are `.env`, `.envrc`, `.env.*` (except `.example`, `.sample`,
`.template`, `.dist`), `id_rsa`/`id_ed25519`/`id_ecdsa`/`id_dsa`, `.secrets`,
`*private_key*`, `credentials.json`, `.aws/credentials`, `.npmrc`, `.netrc`,
`.pgpass`, `.git-credentials`, and `*.pem`/`*.key`/`*.p12`/`*.pfx`/`*.clixml`.

**ha's own directories** are named once per process (`protect_host_dirs`), by the
turn and by `ha exec`, so a child's tools guard them too. A coder's worktree
lives in ha's state directory; inside the workspace only `.harness` counts, so a
child changes its own worktree freely.

**Where a command runs.** Every `run_shell` and `run_process` call used to start at
the workspace root with the host's `PATH`, so a `cd sub` (or `Set-Location sub`)
in one call was lost by the next, and the model ran the next command in the wrong
place. The Python REPL kept `os.chdir`; the shell did not. arXiv 2611.22299,
*Harnesses That Improve Themselves*, lists a persistent shell location among the
runtime mechanisms that self-evolving harnesses keep inventing. Now
(`processes/location.rs`):

- The host puts a recording in front of each `run_shell` command, never after it
  (text appended to a command is read as part of it: an unterminated heredoc or
  a trailing backslash swallowed it, and a command that exits itself never
  reached it), on the command's own first line, so the line numbers of every
  message about the command are the model's. It writes the shell's final
  directory and `PATH` to a side file when the shell exits, however it does, and
  leaves the status as the command made it:
  - `sh`: an `EXIT` trap that saves `$?`, starts with `set +eu` (a command that
    turned on `set -e` or `set -u` keeps its status: the write, or an unset
    `PATH`, cannot change it), writes `pwd` and `$PATH`, and exits with the saved
    status. A status above 128 that names a signal is a program the signal
    killed, which the shell cannot tell from `exit 143`: the trap then raises
    the signal on the shell itself, so the call ends by the signal, as it did
    without the trap. A command that sets its own `EXIT` trap replaces ours: no
    location is kept for it (the old behavior). A syntax error in the first line
    stops the shell before the trap is set: status 2, the place unchanged.
  - `pwsh` and `powershell.exe`: a `PowerShell.Exiting` engine event registered
    with `-SupportEvent`, so it is not a job: `Get-Job | Wait-Job` waits for the
    command's jobs only, and `Get-Job | Remove-Job` finds nothing of ours. It
    runs on `exit N` too (checked on PowerShell 7 and 5.1). PowerShell wants
    `using` statements and a `param` block (attributes before it) at the top of a
    script, so the registration goes after the last of them, on the same line,
    found by their grammar; with none, before the first statement. Comments and
    `#requires` stay first. A command that does not close one of them does not
    parse, and nothing is registered. A status line follows the command after a
    blank line. Measured on both: `-Command` exits 0 when the last statement
    succeeded, 1 when it failed (a native program's own code is lost) and N for
    `exit N`. The status line exits 0 on success, the native program's
    `$LASTEXITCODE` when the command failed and it is a non-zero integer, else
    1; so a native `exit 5` now gives 5 (it gave 1), a success stays 0, and
    `exit N` stays N. A command that does not parse runs nothing, so the place
    stays.
  - A command that is killed, times out or is canceled writes no file, so the
    place stays as it was.
  - The side file is in a directory made by the host for its user alone (mode
    0700 on Unix, a random name, checked to be the directory just made, not a
    link, owned by the user), under a name nobody can guess. It used to be in the
    shared spool (`/tmp/ha-tool-spool`), with a name another local user could
    guess, written through links. The host reads it only when it is a plain file
    of the user, not a link, and removes it.
- The host keeps, per conversation (task), the last directory and `PATH`; the
  next `run_shell` and `run_process` of that conversation start there. It keeps
  the places of the 256 conversations used last: past that the least recently
  used goes, never one with a call in its turn.
- The calls of one conversation take turns: a call reads its start place and
  stores its end place while it holds the conversation's lock, after any call
  before it has stored its own. It used to read the place before it held the
  process permit, so two calls that ran together could store the old place over
  the new one. (A turn's batch already runs `run_shell` and `run_process` one at
  a time; this holds when they do not, and the Python REPL's `bash()` does not
  read or change the place.)
- The directory is kept only when it is inside the workspace root once resolved
  (a link out of it does not count). Outside it, the next call starts at the root
  and the result says so in one line. `PATH` is kept only when it differs from
  the host's. No other variable is kept: an environment carries secrets.
- The kept `PATH` is only what the command runs with. The shell is found in the
  host's own `PATH` and started by its full path, so a call that emptied `PATH`
  does not leave the next one without a shell. A `run_process` of a program named
  bare is looked up in the kept `PATH` and, when it is not there, does not start
  (`the program did not start ... it is not on the PATH an earlier shell call
  set; nothing ran`), as a shell with that `PATH` would say. Windows would
  otherwise have found it in the host's `PATH` too.
- Jev's command guard judges a call at the gate, before the approval, from where
  the shell is at that moment. The conversation's lock cannot be held from there
  to the dispatch (the guard is a model call and the approval waits for a
  person), so the dispatch, holding the lock, compares the directory it starts in
  with the one the guard judged and, when they differ (a call of the conversation
  ran in between), asks the guard again from the right one. A refusal then is
  returned as the call's result; the command is never run on a verdict given for
  another directory.
- The guards above read a relative path from the directory the command runs in,
  not from the root: `recursive-delete` (a delete of untracked work named from
  the kept folder is refused; read from the root it named nothing and passed),
  `harness-integrity`, the destructive-git guard (its `git status` runs there)
  and Jev's command guard (`working_dir`, and the scripts it reads). A `cd`
  inside the same command still leaves the targets after it unchecked, as before.
  The file lists of a compaction summary read a relative word of a command from
  the folder its result names.
- The person who is asked to approve a command is shown where it starts when
  that is not the root with the host's `PATH` (the folder, and the `PATH`), and
  the approval binds it: one given for a call in one place does not run it in
  another. An allow rule or a persistent approval does not decide a command
  while a shell call has changed `PATH`: it asks (full-auto still allows; deny
  rules still deny).
- Every path the guards compare is in one form: the verbatim prefix Windows'
  `canonicalize` gives (`\\?\F:\w`) is taken off, `/` and `\` both separate, and
  `.` and `..` are applied as text. In a verbatim path `..` is a name, so
  `../.harness/x` from a folder, or `./.harness/x` from the root, went unseen.
- The note `ran in <folder>` is always in the result of a shell or process call
  while the location is kept (`ran in .` at the root), so the last such line of
  a result is the host's and a line the command printed (`ran in 0.4s`) is not
  taken for the folder.
- The result says where the command ran (`ran in sub`; `ran in .` at the root) and, when the command moved, where the next one starts.
- `HA_SHELL_KEEP_LOCATION=off` restores the old behaviour: every call starts at
  the root with the host's `PATH`.

## 4. Key-shaped values

`secret_values` recognizes:

- provider keys by prefix and length: Anthropic, OpenAI (`sk-`, `sk-proj-`),
  Stripe live, AWS access key ids, GitHub, Slack, Google, npm and Hugging Face
  tokens;
- a PEM private key header;
- a long value assigned to a field that names a secret (`api_key`, `secret`,
  `token`, `password`, `auth`);
- a bearer token.

A match counts only when its value is random enough: at least 10 distinct
characters and a Shannon entropy of 3 to 4 bits per character, by shape. So a
placeholder such as `sk-xxxxxxxxxxxxxxxxxxxx` or `your-api-key-goes-here` is not
a key, and no list of placeholder words is needed.

- **Writes:** a value is refused only when the file does not hold it already.
  A key already in a file is the user's to move, and refusing every edit of
  that file would help nobody.
- **Output:** values are replaced by `[REDACTED <kind>]` as the stream is read,
  one line at a time (a carriage return ends a line, so a progress bar still
  moves). The value never reaches the spool file, the artifact, a preview or
  the model.
- **Not redacted:** a file the user attaches, or one the agent reads with
  `read_file`. The user handed the first over; the second is the text an edit
  must match. The file tools refuse secret files by name either way.

## 5. Tests

- Unit tests in both modules:
  - the parser, wrapped shells included;
  - every guard's refusals and passes;
  - a recursive delete against a real git repository: tracked and clean,
    untracked, outside the workspace, after `cd`, a variable, the whole
    workspace, and `build/..`;
  - a worktree inside ha's state;
  - key shapes and placeholders, and redaction split across reads.
- The live suite `scripts/testing/live-tests/guards.sh` asks a real model to call
  each command and reads what the model was sent:
  - a delete of untracked work is refused and the work stays, while a build
    folder's delete runs;
  - a kill by name, a read of `.env`, a change to `.harness` and a whole-drive
    search are refused;
  - a key printed from a settings file arrives redacted;
  - a write of a key is refused.
