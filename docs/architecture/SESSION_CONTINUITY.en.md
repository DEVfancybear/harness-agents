# Session continuity

English | [Tiếng Việt](SESSION_CONTINUITY.vi.md)

How a session of `ha` picks up where the last one stopped, and where each kind of
information lives. Every mechanism below has its own job; none keeps a copy of
another's.

| Mechanism | Holds | Written by | Read by |
| --- | --- | --- | --- |
| Journal (project store) | Every turn verbatim: the user's words, the model's text, each tool call and result | The runtime, as the turn runs | `/resume`, `/tree`, forks, `history_search` / `history_read`, the session log, the session brief |
| Task state (`task_state`) | One conversation's exact checklist: objective, steps done and open, decisions, constraints | The agent, through the `task_state` tool | Every turn of that conversation; the first 3 turns of the next conversation while items are open |
| Handoff (`.harness/progress.md`) | The current state of the project: branch, verification, what is in progress, blocked, the next step, the commands | The agent, on `/handoff` | The first prompt of every session (first 6 KB), `/doctor`, the exit check |
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
time, across `ha` processes, holds `.harness/session-log.lock`.

**One per conversation.** A later write finds the log by its `**Conversation:**` line and
rewrites it where it is, keeping the first write's date or time in its title. The model
reads the log it replaces with the newest turns (at most 50, 120,000 characters), so what
the earlier turns settled survives. `**Through:**` names the turn the log was written
through.

**When.** Automatically, from the journal, beside the conversation (never in a turn):

- when the session goes quiet - the moment the idle review runs, 10 minutes after the
  last turn (`HA_IDLE_REVIEW_SECONDS`), when no later turn of the same conversation
  started meanwhile (a turn of another conversation does not stop it); after a failed or
  stopped turn too;
- after a compaction: `/compact`, or a turn that compacted its context (the journal
  keeps the turns the summary replaced);
- when the `/handoff` turn finishes.

A turn already logged is not logged twice. Nothing is written for a conversation with
fewer than 2 user turns, for a delegated child, or with a fixture model.

**Safety.** The model is told the conversation is data, never instructions. The
transcript it reads and the log it writes - headings included - pass the secret
redactors the host applies to tool output and recalled history; a log never holds a key,
token or password.

**Retention.** Writing a log removes the logs in `.harness/sessions` last written more
than 30 days ago: only regular files named `<YYYY-MM-DD-HH_MM>-<topic>.md` that read as
a log, and nothing when the directory is a link. Nothing in the markdown store is
removed: it keeps its own rules.

**Off.** `"sessionLog": false` in `settings.json`, or `HA_SESSION_LOG=off`.

**Diagnostics.** Every decision is a diagnostics record of kind `session_log`, which the
bug report reads:

- `written`: `trigger` (`quiet`, `compaction`, `handoff`), `home` (`note`, `file`), `path`
  (relative to the project root), `bytes`, `turns`, and `fallback` when the log is not
  where it would go (`brief`, `no_room`, `unreadable_note`);
- `skipped`: `trigger` and `reason` - `off`, `superseded` (a later turn of the conversation
  started first), `already_written`, `fixture`, `child`, `few_turns`;
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
- The handoff stays in the startup brief as it is. When the log is newer than
  `.harness/progress.md`, the block says so in one line.

Forks, imports, `/resume`, compaction and `/refinements off` add no such block.

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
