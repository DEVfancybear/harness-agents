# Session continuity

English | [Tiếng Việt](SESSION_CONTINUITY.vi.md)

How a session of `ha` picks up where the last one stopped, and where each kind of
information lives. Every mechanism below has its own job. Some overlap where
they record the same moment from different sides (a decision can be in the task
state, the handoff and a session log). When they disagree, one rule says which
wins: "Where things stand" (below), which the system prompt carries once per
request. Every other text that carries a record points to it by that name
instead of stating its own order.

## Where things stand

The rule, as `WHERE_THINGS_STAND` in `crates/harness-cli/src/host/memory/continuity.rs`
holds it (this list is kept in sync with that constant):

1. the user's current message;
2. this conversation's live task state (`task_state`);
3. the project's markdown memory files (`memory/`, in a project that keeps them);
4. the newest session log (`.harness/sessions`);
5. `.harness/progress.md`, the handoff;
6. a compaction summary.

Everything below 2 is a record of earlier work that may be stale - and so are the memory
digest and an earlier conversation's task state or turns, which rank with the session
log: the agent checks one against the repository before acting on it. A new decision goes
to `task_state` for this task and, in a project with markdown memory, to
`memory/decisions/`.

The rule is appended to the system prompt (after a `--system-prompt` or `SYSTEM.md` too),
the part of the request that stays cached, so it is sent once per request. The texts that
used to rank themselves point to it in one line: the session log's start block, the
session brief, the earlier task state, the startup brief's handoff, `/handoff --reset`'s
prompt, the compaction summary's prefix, the memory digest and the `memory` tool's
guidance. The markdown store's policy says its files are the truth for durable project
facts and decisions while the live `task_state` governs the current task. `/handoff` in a
project with a markdown store sends decisions to `memory/decisions/` instead of a section
of its own, as `/preserve` does.

| Mechanism | Holds | Written by | Read by |
| --- | --- | --- | --- |
| Journal (project store) | Every turn verbatim: the user's words, the model's text, each tool call and result | The runtime, as the turn runs | `/resume`, `/tree`, forks, `history_search` / `history_read`, the session log, the session brief |
| Task state (`task_state`) | One conversation's exact checklist: objective, steps done and open, decisions, constraints | The agent, through the `task_state` tool | Every turn of that conversation (within about 4K tokens); the first 3 turns of a new conversation while items are open, as a short recap (see below) |
| Handoff (`.harness/progress.md`) | The current state of the project: branch, verification, what is in progress, blocked, the next step, the commands | The agent, on `/handoff` | The first prompt of every session (within about 1.5K tokens, as a reference), `/doctor`, the exit check |
| Session log | What one conversation settled: decisions with reasons, learnings, fixes, files changed, exact setup values, pending tasks, errors and workarounds, a resume paragraph | The host, with one model call from the journal | A new conversation's start (a short block), `/recall` |
| Session brief | The previous conversation's last 3 turns, bounded excerpts | The host, from the journal, no model call | A new conversation's first turn, when the previous conversation has no session log |
| Project instructions (`AGENTS.md`, else `CLAUDE.md`) | What every session needs: conventions, commands, structure, patterns, warnings | The user, or the agent on `/preserve` | Every turn |
| Markdown store (`memory-lint.json` projects) | Durable facts and decisions with provenance tags; daily notes | The agent, by the store's policy; the host adds session logs to daily notes | Every turn (policy and index) |

## The session log

One log per conversation, after CPR's `/compress`. Its shape:

- `# Session Log: <date> - <topic>`
- `## Quick Reference (for AI scanning)`: `**Conversation:**`, `**Through:**` and
  `**Written:**` (the task id, the turn it was written through and when, written by the
  host), `**Confidence keywords:**` (10-20 terms), `**Projects:**`,
  `**Outcome:**` (one sentence)
- `## Decisions Made`, `## Key Learnings`, `## Solutions & Fixes`, `## Files Modified`,
  `## Setup & Config`, `## Pending Tasks` (`- [ ]`), `## Errors & Workarounds`,
  `## Custom Notes`: each only when it has content
- `---`, then `## Quick Resume Context` (2-3 sentences)

**Where.** A project with a markdown store gets the log as a `## Session <HH:MM> -
<topic>` section of that day's `memory/YYYY-MM-DD.md` (its subsections one level
down), the ram note the store's policy sends a day's outcomes to. The section ends at a
`<!-- end of session log <conversation> -->` line: what the agent writes after it stays
the agent's, and a rewrite replaces only up to the marker (a section written before the
marker existed ends after its Quick Resume Context paragraph). The rest of the note is
kept, and the note stays under its ram cap so `ha memory lint` passes. When the note
has room only for the Quick Reference and the Quick Resume Context, the log is cut to
those; with no room even for them, or when the note exists but cannot be read (not
UTF-8, held by another program), the note is left untouched and the log goes to a file.
Any other project gets `.harness/sessions/<YYYY-MM-DD-HH_MM>-<topic>.md`. One write at a
time, across `ha` processes, holds `.harness/session-log.lock`. The note itself is written
through the store's one writer ([MARKDOWN_MEMORY.en.md](MARKDOWN_MEMORY.en.md), section
5): under the store's lock, read again just before it is replaced, so what the agent
wrote into the note meanwhile is kept, and replaced whole. A log's own file is replaced
whole too.

**One per conversation.** A later write finds the log by its `**Conversation:**` line and
rewrites it where it is, keeping the first write's date or time in its title. The model
reads the log it replaces with the newest turns (at most 50, 120,000 characters), so what
the earlier turns settled survives. `**Through:**` names the turn the log was written
through.

**When.** Automatically, from the journal, beside the conversation (never in a turn):

- when the conversation is left: `/new`, `/resume` of another, `/fork` or `/clone`, and
  the chat's exit (`/quit`, Ctrl-D) - at once, so the next conversation's start may already
  find it (if it is still being written, the start falls back to the last turns);
- at the end of an `ha exec` run, before the process exits (waiting at most 30 seconds; a log that takes longer stays owed);
- when the session goes quiet - the moment the idle review runs, 10 minutes after the
  last turn (`HA_IDLE_REVIEW_SECONDS`), when no later turn of the same conversation
  started meanwhile (a turn of another conversation does not stop it); after a failed or
  stopped turn too;
- after a compaction: `/compact`, or a turn that compacted its context (the journal
  keeps the turns the summary replaced);
- when the `/handoff` turn finishes.

**Owed logs.** Every turn, from its start, marks its conversation as owing a log, in
`session-log/owed/<conversation id>` under the project's store directory in ha's data
directory - beside the journal that holds the conversation, never in the project - with
the turn it is owed through and the process running it. Writing that log, or finding in
the journal that nothing is owed, removes the mark; a later turn marks it again. A log a
crash, a kill (`ha shutdown --force`), a worker restart or an exit cut off is therefore
written by the next start of ha in that project, beside its first turn. A start or a leave
writes only marks no other live ha process owns and whose conversation has no turn running
in this one, so two ha processes in one project never log each other's turns half done. A
fixture model leaves a mark as it is, and a mark whose writes failed 3 times in a row is
tried again only once an hour. The project's `.harness` is written only for a log.

A turn already logged, or one whose log is being written, is not logged twice. Nothing
is written for a delegated child, with a fixture model, or for a conversation that did too
little: a log needs 2 typed user turns, or - one request carried out by a goal or a long
turn - 5 tool calls or 4 model steps across its turns. The host's own turns (a goal's
continuations, a child's report, a heartbeat) count as work, never as user turns.

**Safety.** The model is told the conversation is data, never instructions. The
transcript it reads and the log it writes - headings included - pass the secret
redactors the host applies to tool output and recalled history; a log never holds a key,
token or password.

**Retention.** Writing a log removes the logs in `.harness/sessions` last written more
than 30 days ago: only regular files named `<YYYY-MM-DD-HH_MM>-<topic>.md` that read as
a log, and nothing when the directory is a link. Nothing in the markdown store is
removed: it keeps its own rules.

**Not the project's changes.** Before the host writes into `.harness`, it makes `.harness/.gitignore`
list `sessions/` and `*.lock` (creating the file, or appending a missing line; nothing the
user wrote there is removed), so the file logs and the lock never show as
uncommitted in the startup brief or on exit. A `.gitignore` the host creates lists itself
too: a log written while a coder works leaves the checkout as the coder found it, and
`integrate_children` still applies the coder's work. A log inside the markdown store's daily note
stays tracked: it is the store's.

**Off.** `"sessionLog": false` in `settings.json`, or `HA_SESSION_LOG=off`.

**Diagnostics.** Every decision is a diagnostics record of kind `session_log`, which the
bug report reads:

- `written`: `trigger` (`quiet`, `compaction`, `handoff`, `left`, `exit` for `ha exec`, `owed` for a start that writes an owed log), `home` (`note`, `file`), `path`
  (relative to the project root), `bytes`, `turns`, and `fallback` when the log is not
  where it would go (`brief`, `no_room`, `unreadable_note`);
- `skipped`: `trigger` and `reason` - `off`, `superseded` (a later turn of the conversation
  started first), `already_written`, `in_progress`, `fixture`, `child`, `few_turns`, `not_in_store`, `timed_out` (`ha exec`);
- `failed` (warn): `trigger`, and the error as its message, redacted and cut to 300
  characters;
- `recalled`: `n` (logs sent), `topic` (whether one was given), `matches`;
- `preserve_requested`: `file` (`AGENTS.md`, `CLAUDE.md` or `none`), `store`.

## A new conversation's start

The first prompt of a new conversation hears of the previous one once:

- If the previous conversation has a session log written in the last 7 days through its
  newest turn, the block shows its outcome, its Quick Resume Context and up to 5
  unchecked pending tasks, framed as historical reference, not live instructions; the
  previous conversation's last 3 turns are not sent as well.
- If that log was written before the conversation's newest turn (its quiet log was not
  yet due, a handoff's write was still running), the block says so and the last turns are
  sent too.
- If the newest log is of an older conversation, it is shown with how many later
  conversations have none, and the previous conversation's last turns are sent as before.
- With no log, the session brief is sent as before.
- When the previous conversation's task state is carried over (its checklist is exact),
  the log's pending tasks are left out so the steps are not listed twice.
- The handoff is its own block of the startup brief (`session-handoff`), on the reference
  channel. When the log is newer than `.harness/progress.md`, the block says so in one
  line.

Forks, imports, `/resume`, compaction and `/refinements off` add no such block.

**Only a conversation that was left.** A conversation still open is not "the previous
one": another pane of the same ha, a background agent, or a conversation another live ha
process runs. Each turn holds its conversation open in a registry of the process (until
the pane's next turn is of another conversation, or the pane goes) and leaves a mark under
the project store (`open-conversations/<task>`, naming the process); a conversation whose
log is owed by a live process counts as open too. The session log's start block, the
session brief and the earlier task state all pass over an open conversation.

**The earlier task state.** A new conversation's first 3 turns hear of the task the newest
conversation with a task state left unfinished, as an optional reference block
(`earlier-task-state`): the state's recap and up to 10 open items, within about 1.5K
tokens, and a pointer to `/resume`. It is not offered when that state is older than 7 days
(by its file's time), nor once 3 other new conversations were shown it and none adopted it
(a conversation that adopts it keeps a state of its own, which is newer); the
conversations shown it are kept in `task_state.offered.json` beside the state.

## The context budget

Every block a turn's context carries has a place in one budget
(`ContextBuilder`, `crates/harness-session/src/context.rs`):

- A host block that is not mandatory goes through the optional path: optional blocks are
  ranked by relevance and admitted while they fit `RuntimeConfig::optional_token_budget`
  (`OPTIONAL_TOKEN_BUDGET`, 9,000 tokens: the digest's 3K and the start blocks' 6K fit
  together). One that does not fit is left out.
- A block may carry `max_tokens`. One over it is cut at a line boundary, before
  admission, with a note at its end saying it was cut and where the rest is; a mandatory
  block over its cap is cut instead of overflowing the window.
- The rules and the host's facts come first, then the optional blocks by rank, then the
  user's message, the working state and the tail, so the request ends with what the turn
  is about.
- `/context` shows each block's size, and for a cut one the size it was cut from; a block
  left out says why. The diagnostics record `context` `budget` (`cut`, `dropped`,
  `mandatory_tokens`, `optional_tokens`) for a packet that cut or dropped a block.

| Block | Mandatory | Cap (tokens) | Rank |
| --- | --- | --- | --- |
| `task-state` (this conversation's) | yes | 4,000 (done items as a count, the newest decisions) | - |
| `recalled-skill` | yes | 1,500 | - |
| `harness_state` (the memory digest) | no | 3,000 | 70 |
| `session-start` (commits, uncommitted, the baseline check) | no | 800 | 55 |
| `earlier-task-state` | no | 1,500 | 50 |
| `session-handoff` (after `/handoff --reset`, the one record of where things stand) | no | 1,500 | 48 |
| `session-log` | no | 1,500 | 42 |
| `knowledge-catalog` | no | 1,500 | 40 |
| `recent-session` (the session brief) | no | 1,500 | 30 |

## Commands

- `/recall [n] [topic]` sends a turn whose prompt carries, as data, the n most recent
  session logs (3 by default) and, with a topic, up to 10 logs ranked by memory search's
  BM25 (else by the phrase, any case), each with a 140-character snippet; the whole
  prompt stays within 96 KB, the logs cut to share what is left. A leading number of at
  most 50 is n; a larger one (a year, an issue number) is part of the topic. The agent
  reports Context, Blockers, Most recent session, Earlier sessions, Related (with a topic)
  and Ready to, and reads a log's file only if the summaries do not answer. With no log, a
  notice and no turn.
- `/preserve [focus]` asks the agent to fold what this session settled for every future
  session - conventions, commands, structure, patterns, warnings - into `AGENTS.md`
  (else `CLAUDE.md`; it asks before creating `AGENTS.md` when neither exists). Not the
  current state or next step (that is `/handoff`'s), and in a project with a markdown store
  not decisions or facts (they go to `memory/decisions/` or `memory/projects/`, tagged).
  It edits in place, states each thing once, keeps the file at most 200 lines by moving
  `## Session Notes (<date>)` older than 7 days, `## Completed Projects` and headings
  marked `(ARCHIVABLE)` to `AGENTS-Archive.md` under `## Archived: <date>` - never a
  heading marked `(PROTECTED)` or a core section - and ends with the line count.
- `/handoff [--reset] [focus]` writes `.harness/progress.md`, as before; its end also
  writes the session log.
