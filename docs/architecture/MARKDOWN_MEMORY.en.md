# Markdown memory store

English | [Tiếng Việt](MARKDOWN_MEMORY.vi.md)

A project can keep its memory as markdown files at its root, in the layout of
[markdown-agent-memory](https://github.com/ucsandman/Agnostic-AI/tree/master/packages/markdown-agent-memory)
(MIT, read at commit `aab09ff`, 2 October 2026). ha writes an empty store, puts
its rules and its index in every turn, and checks it with a Rust port of the
package's `memory-lint.mjs`. The store runs beside ha's own memory
([MEMORY_STORE.en.md](MEMORY_STORE.en.md)); it does not replace it.

Code: `crates/harness-cli/src/host/memory/markdown.rs`; the bundled files are in
`crates/harness-cli/assets/markdown-memory/`.

## 1. Layout

| Path | Tier | Rule |
|---|---|---|
| `MEMORY.md` | ROM | The routing index: pointers only, never facts. Capped (15,000 characters). |
| `memory/people/`, `memory/projects/`, `memory/decisions/` | Disk | Every fact line carries `[stated]`, `[observed]`, `[inferred]` or `[suggested]`. A changed fact is struck through and dated, never deleted. |
| `memory/context/`, `memory/YYYY-MM-DD.md` | RAM | Rewritten freely, capped (30,000 characters). What lasts moves to a disk file. |
| `memory/archive/` | Tape | Frozen: a document enters whole and is never edited. |
| `memory-lint.json` | | Which file sits in which tier, and the caps. Its presence marks the project as keeping a store. |

Git is the store's history: `log` is when a fact held, `blame` who wrote a line,
`diff` the supersession.

## 2. Which memory gets what

| | ha's `memory` tool | The markdown store |
|---|---|---|
| Where | `<data>/harness/memory.sqlite3`, per conversation and global | Files in the project, in Git |
| Holds | The user's preferences and decisions across projects, task state | This project's people, decisions, state and lessons |
| Written by | The `memory` tool, the review, Deep Dream | The agent's file tools and people with an editor; the host too: the learning loop's decisions and facts, and the session log's section of the day's note |
| Read | The digest in every turn, `memory` search | `MEMORY.md` in every turn; the files when the index routes there |

## 3. Commands

### `ha memory init [--root DIR] [--hook] [--examples]`

Writes `MEMORY.md` (the package's index with its example rows left out),
`memory-lint.json` (the package's template) and the five tier directories, each
with a `.gitkeep`. The root is the working directory when `--root` is left out.

- A file that exists is kept. Nothing a store holds is overwritten.
- When the project has `AGENTS.md` (or `CLAUDE.md`), it is added to the ROM files
  with `maxLines: 200`, the length `ha doctor` advises for an entry file.
- `--hook` writes `.git/hooks/pre-commit` (through `git rev-parse --git-path`, so a
  worktree or `core.hooksPath` is followed). The hook runs
  `ha memory lint --staged --quiet` from the store's directory and blocks the
  commit on a failure. An existing hook is left alone, and the command says what
  to add to it.
- `--examples` writes the package's example files into `memory/templates/`:
  `PERSON.md`, `PROJECT.md`, `DECISION.md` and `DAILY.md`. That folder is in no
  tier, so their sample facts are never read as the project's own and the lint
  leaves them alone. The policy tells the agent to follow their formats.

### `ha memory migrate [--root DIR] [--from FILE] [--model M] [--thinking T] [--approval MODE]`

The install prompt's first step: a flat memory file becomes the store.

1. Without `memory-lint.json` the store is written first, as `init` writes it.
2. The flat file (`--from`, else the root's `MEMORY.md`) is copied whole to
   `memory/archive/<date>-<name>.flat.md`. A tape file takes a document whole,
   so nothing in it can be lost. A flat `MEMORY.md` is replaced by the empty
   index. An empty file, or the empty index `init` wrote, has nothing to migrate.
3. A run of `ha exec`, in the root, with `--approval auto-edit` by default (it
   only edits files), asks the agent to:
   - split the file into people, projects, decisions and daily notes;
   - tag every fact line: `[stated]` only where the file says the user said it;
   - strike changed facts with their dates;
   - rebuild `MEMORY.md` as a pure index whose paths exist;
   - leave out only what can be re-derived.

   The store's rules are in that run's turns. The run is not self-evaluated:
   the lint in step 4 is its check.
4. The store is linted against the working tree. A failure exits 1, and the
   archived file is named.

### `ha memory lint`

The package's `memory-lint.mjs` 1.0.0 in Rust. The checks, flags, text report,
`--json` shape and exit codes are the same, so a hook or health check written
for one works with the other.

| Flag | Meaning |
|---|---|
| `--root DIR` | The store's root (the working directory) |
| `--config FILE` | The config, relative to the root (`memory-lint.json`) |
| *(none)* | The working tree against `HEAD`, untracked files included |
| `--staged` | What the next commit records |
| `--base REF` | The working tree against `REF` |
| `--no-diff` | No diff: only the caps and the index links |
| `--json`, `--quiet` | One JSON line; only the failures and the result line |
| `--version` | `1.0.0`, the memory-lint version reproduced |

| Check | Fails when |
|---|---|
| `rom-caps` | A boot file is over its character or line cap, or the boot files together are over `totalMaxChars` |
| `ram-caps` | A working file changed in the diff is over its cap (an untouched one only warns) |
| `disk-provenance` | An added fact line in people, projects or decisions has no tag |
| `disk-history` | A struck line was removed with no matching line added anywhere in the diff |
| `tape-frozen` | An archived file was modified, deleted, retyped, or renamed with a content change |
| `index-links` | A path in `MEMORY.md` points at nothing |

Every verdict prints how much it checked. Exit codes: 0 pass, 1 a check failed,
2 a bad config (missing, not JSON, an invalid `ram.patterns` regex).

Differences from the original, each deliberate:

- Untracked files with a non-ASCII name are read. The original lists them with
  git's quoting on, cannot open the quoted name and skips the file, so an
  untagged fact in `memory/people/bảo.md` passed.
- A JSON error is worded by serde, not by V8.
- Tags are matched as words, not as regular expressions; `--staged`, `--base` and
  `--no-diff` exclude each other instead of the last one winning.

The port was checked against the original on 69 stores (both checkers run on the
same files: text, `--json` and `--quiet` outputs and exit codes compared): 68
match byte for byte, and the one that differs is the JSON error's wording.

## 4. In the turn

When `memory-lint.json` is at the project root, every turn carries two
mandatory blocks:

- `markdown-memory:policy`: the package's operating policy, whole, in ha's terms
  (`assets/markdown-memory/policy.md`, about 1,100 words). It covers:
  - the source of truth, and the tiers;
  - when and where to write: decisions, state changes, blockers, mistakes,
    lessons, stable preferences, "remember this";
  - the provenance tags, with the origin test for decisions and the warning
    about summaries;
  - the promotion gate (3+ signals across 2+ sessions, 30-day decay, contrary
    signals, the trail, lessons as data);
  - supersession;
  - calibration: claims matched to the evidence, dated figures, nothing
    re-derivable, a live check of named tools and versions;
  - the read rules: narrowest file first, at most 5 sources, stale or unknown,
    conflicts fixed in the file, search as a locator;
  - upkeep: caps, consolidation, the archive, the failure standard;
  - `ha memory lint`.

  It carries the user's authority, as `AGENTS.md` does, since the user chose to
  keep the store.
- `markdown-memory:MEMORY.md`: the index, sent where the memory digest is sent,
  framed as data and without instruction authority (the agent and people write
  it). It is cut at the index's `maxChars` in the config (15,000 when none), on a
  whole character, and the turn is told when it was cut.

The automatic review is told the project keeps a store, so it saves no fact
about the project as a memory edit: the agent writes those in the store
([LEARNING_LOOP.en.md](LEARNING_LOOP.en.md)). The `memory` tool refuses a new
`scope: project` entry too, and its refusal points at the store: a fact kept in
both would go stale in one.

The index is read only from inside the project and never from a protected path.
A config that cannot be read still sends the rules, and the turn is told why the
index is missing. A project without `memory-lint.json` gets neither block. The
chat and `ha exec` carry them; a delegated child does not, since the agent that
delegates keeps the memory.

## 5. What the host writes, and how

The agent writes the store with its file tools, and so do people with an editor.
The host writes it too:

| Writer | Files |
|---|---|
| `ha memory init` | `MEMORY.md`, `memory-lint.json`, the `.gitkeep` of each tier, the examples: only those not there yet |
| `ha memory migrate` | The flat file's copy in `memory/archive/` (a new name each time), then the empty index in place of a flat `MEMORY.md` |
| The session log ([SESSION_CONTINUITY.en.md](SESSION_CONTINUITY.en.md)) | Its section of the day's note `memory/YYYY-MM-DD.md` |
| The learning loop ([LEARNING_LOOP.en.md](LEARNING_LOOP.en.md)) | A rule's decision file and its index pointer; a fact's line in `memory/projects/<project>.md` and its index row; the strike when one is retired or reworded |

Every one of them goes through one writer (`markdown_memory::edit`):

- **One lock.** It holds `.harness/markdown-memory.lock` at the store's root
  while it reads and replaces a file, across processes. It is the innermost lock:
  the session log holds its own lock around it and the learning loop its
  lessons' lock, and nothing takes another lock while holding it.
  `.harness/.gitignore` lists `*.lock`, so the lock is never one of the
  project's changes.
- **Read again before the replace.** The writer reads the file, makes its
  change, and reads the file again just before it replaces it. When the agent or
  the user wrote it in between, the change is made again on what they wrote, so
  their write is never lost; a file that keeps changing is left as it is after
  five tries, with an error that names it.
- **Whole or not at all.** The new text goes to a uniquely named file beside the
  target, is synced, and is renamed over it: a reader sees the old file or the
  new one, and a failed write leaves the old one whole.
- **Never over a file it cannot read.** A file that is not UTF-8 text, or that
  another program holds, is not written. The session log puts its log in a file
  of its own instead.
- **Inside the store.** The writer takes plain relative paths only, never one
  that leaves the root or goes through a hidden directory. The index is the one
  the config names (`MEMORY.md` when it names none); an index outside the
  project or on a protected path is never written.

The agent's own file tools check the file's hash before they replace it, so a
host write between the agent's read and its write is caught there.

**The index holds pointers.** A learned rule's line in `## Current Decisions` is
`` - <title> (learned <date>, <lesson>) - `<file>` ``: a title of at most 60
characters, cut at a whole character, and the decision file that holds the
rule. A title that would read as a path to the lint (`docs/x.md`, a backtick or
a parenthesis around one) is written without it, so the decision file is the
line's only link and `index-links` passes. Lines the learning loop wrote before
0.8.8 held up to 160 characters of the rule; they are still struck when their
rule is retired or reworded.

## 6. `ha doctor`

The State subsystem gains one finding per lint check that ran with `--no-diff`
(the caps and the index links; the diff checks belong to the commit hook): FAIL
and WARN as the lint grades them, with the first detail and how to fix it. A
project without a store gets no finding.

## 7. Tests

- Unit tests in `markdown.rs` cover every check, the report's text and JSON,
  `init`, the turn's blocks, the index cap and the path refusal. The expected
  values are what `memory-lint.mjs` printed on the same stores. They also cover
  the writer: two writers on one note both land, a write between the read and
  the replace is kept, a failed write leaves the file whole, a file that is not
  text is never written. The learning loop's tests in `lessons.rs` check that an
  index line is a pointer, that two non-ASCII rules of one day get two files,
  that an old index line is struck, and that the store passes the lint.
- The live suite `scripts/testing/live-tests/markdown-memory.sh` drives `ha`
  against a real model: the agent writes a tagged fact, supersedes a decision by
  striking it, answers from the index, and the hook blocks an untagged fact. Its
  last section imports two Vietnamese rules with `ha learning import` (no model)
  and checks the two decision files and the pointer lines.
