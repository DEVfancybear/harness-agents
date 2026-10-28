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
