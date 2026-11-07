# Sleep: the background memory passes as a durable queue

ha keeps memory in tiers that change at different speeds: the turns of a
conversation (the journal), the conversation's own store, the global memory,
and the harness's evolved rules. The "Sleep" paper (arXiv 2606.03979) gives the
rule they follow: a faster tier is consolidated into the next one **before** it
is reset or overwritten, or what it held is lost. ha does this in text, through
background passes, all but the expiry calling a model:

| Pass | Reads | Writes |
|---|---|---|
| A conversation's review (the review gate, then the refinement it approves) | the turns since the last review | the conversation's store, the global memory, lessons, learned skills, knowledge, the memory queue (`memory_queue.json`, what waits for the person); then it closes each child proposal it read as kept or queued ([MEMORY_STORE.en.md](MEMORY_STORE.en.md) §15.1, §15.4) |
| Deep Dream (consolidation) | the global memory, or a conversation's store past 40 entries, in batches past a token budget | the same scope, merged and pruned |
| The harness evolution | the calls that failed in the last 14 days | rules and tool notes |
| The expiry (0.9.0), at most daily, no model call | when each entry of the global store and the conversation's was last written, read or found (`HA_MEMORY_EXPIRE_DAYS`, 28 by default) | the same stores: an unused entry closed, not deleted |

A model call fails: the provider is down, a limit is reached, the process
dies. Before 0.8.9 a failure lost the pass's work: the review's turn count was
zeroed as it was claimed, a review a session left for later was deleted as it
was read, a compaction inside the twenty-minute spacing was not reviewed at
all, the evolution marked itself as run before its call and waited a day, and
a failed Deep Dream was tried every hour forever. Since 0.8.9 every pass is a
**job on disk** first (`memory/sleep.rs`).

## The queue

- A job is one JSON file under `<data-dir>/sleep/`, written whole or not at all
  (a temporary file renamed into place). It names its kind (`review`,
  `consolidation`, `evolution`, `expiry`), what asked for it (`turn`, `compaction`,
  `idle`, `session_end`, `exec`, `due`), and for a review the project store,
  the conversation, the newest session to read and the turns it covers.
- **What triggered it is reset only once the job is written.** The app's count
  of turns and tool calls, `ha exec`'s `review_cadence.json` and a session's
  unreviewed tail are put back when the job cannot be written, so the turns are
  counted toward the next review instead of being forgotten.
- **Jobs of one conversation coalesce.** A review job waiting for a
  conversation takes a new one's span: the newer session is read (a session
  read whole stays whole), the turns add up, the learned skills in play join,
  and the earlier of the two times stands. A job that is running takes no new
  work: what came in meanwhile is a job of its own, run after it.
- **A claim holds a lock.** Claiming a job holds its key's lock file for as long
  as the pass runs, so one job of a conversation (and one Deep Dream, one
  evolution) runs at a time across ha's processes. A process that dies lets go
  of the lock with it, and its job is claimable again with no attempt counted.
  A running claim renews its 30-minute lease every 5 minutes, for up to two
  hours; a claim whose lease lapsed (a process that hung) is taken over, and
  the pass it was running can then neither remove the job nor write its
  failure over it: done and failure act only on the claim they hold. One
  consolidation or evolution is one pass: asked for again while it runs, it
  writes no second job. The last job of a key takes the key's lock file with
  it.
- **A store that moved keeps its jobs.** A review job names the project's
  store directory. When the store moves under another key (`ha project
  rekey`, a repository found again), the store that took it over (its
  `project.json` lists the old key as adopted) runs the job, and a later
  review of the conversation coalesced into it names the store as it is now.
- **Done removes the job; a failure keeps it.** A failed pass counts the
  attempt, records the error and its class (the provider's stable error code,
  `timeout`, `truncated` or `other`), and waits: 5 minutes for a review, an
  hour for a consolidation, an evolution or an expiry, doubled after each next failure, at
  most a day. New turns of a conversation bring its failed review forward; an
  upkeep pass asked for again at a turn's end keeps its wait.
- **After 6 failures in a row a job gives up.** It stays on disk, is no longer
  tried by itself, and `ha doctor` and the bug report say so. A conversation's
  new turns are a new review job; a consolidation or evolution that gave up
  takes no new job until its file is removed, or until its pass is no longer
  due because it succeeded another way (`/refine --curate` consolidates the
  memory too): then the job is forgotten.

## The triggers

The triggers are the ones ha had; each writes a job and then runs the due ones.

- **A turn's end** (every 25 turns or 50 tool calls, sooner after a compaction
  or a milestone): a review job of the conversation. Then every due review job
  of the project runs beside the conversation: this one, a session's end left
  by another session, one that failed earlier and waited out its backoff.
- **A compaction during a turn**: a job for the span it summarized, read from
  the journal, which keeps every turn. A compaction's review starts at most
  every twenty minutes (`review_started`); one inside that spacing is no longer
  skipped: its job waits until the spacing is over, as one job with any other
  review of the conversation meanwhile.
- **A session gone quiet** (ten minutes, `HA_IDLE_REVIEW_SECONDS`): a job for
  the work since the last review, run then.
- **A session's end**: a job for its unreviewed tail, run by the next turn in
  the project. A session ending does not wait for another process's change to
  the queue: the job is written as a file of its own then, coalesced later. A `refine-pending/` file an older ha left in the project joins
  the queue the first time the queue is read there.
- **`ha exec`'s exit**: a job when the conversation's runs did enough; then the
  conversation's due review jobs run before the process exits. A run that is
  not due a review of its own still runs a job an earlier run could not finish.
- **Deep Dream** (the global memory past 12 entries and 15 changes, or a
  conversation's store past 40 entries): marked done only when it applied; one
  consolidation at a time across ha's processes (`consolidation.lock` in the
  global harness directory, an OS lock the process holds while it runs).
  Since 0.9.0 a memory past 32,000 tokens of entries is consolidated in
  batches, one curator call each, the related entries of a batch side by
  side; each batch is applied and recorded (`<id>_b<n>`) as it is planned. A
  batch that fails is the job's failure: the queue tries the pass again, cut
  anew, and the batches already applied stay applied. Where the first batch
  ends moves from pass to pass, so neighbours a boundary parted meet later
  ([MEMORY_STORE.en.md](MEMORY_STORE.en.md), section 13). Each batch is a
  `refine.consolidation_batch` record (its scope, refinement, place, entries
  and edits applied, and the ids of a batch of 64 entries or fewer, `members`).
- **The evolution** (once a day): marked as run only once it saved; its lock
  and its job's claim keep a second run from starting meanwhile.
- **The expiry** (a turn's end in the app, `ha exec`'s exit): a job keyed
  `expiry:<global store>`, trigger `due`, when the last pass is a day old (or
  older than `HA_MEMORY_EXPIRE_DAYS`, when that is shorter). It closes the
  unused entries of the global store and the conversation's
  ([MEMORY_STORE.en.md](MEMORY_STORE.en.md) §12.5).

The learning loop's meditation already claimed itself for an hour and marked
itself only after success; it is not in the queue.

**Off.** `HA_MEMORY_OFF=reviews` (or `HA_MEMORY=off`) runs none of the model
passes: no review, Deep Dream or evolution, and `/refine` and `/learn` say they
did not run. The expiry follows `digest`: with it off the store is not touched.

## What shows it

- **Diagnostics**: `sleep` records `enqueued` (with `coalesced`), `claimed`,
  `done` (with `superseded` when another claim took the job over), `failed`, `gave_up` and `forgotten`, each with the job's kind, key, trigger,
  attempts, turns and span, and for a failure its class and next time.
  `refine.call_failed` records carry the error's `class`.
- **`ha doctor`** (State): how many passes are queued, which failed and wait
  to be tried again, and which gave up, with the file to remove to let one run
  again. Nothing is said of an empty queue.
- **The bug report**: the upkeep table counts the failed calls beside the
  ones that answered (`| upkeep call | calls | failed | ... |`, the failures by
  class), a *Sleep* section lists the jobs on disk and the window's failures,
  and the zip carries `sleep/*.json`.

## Settings for tests

`HA_SLEEP_RETRY_SECONDS` sets the first wait after a failure (doubled as
above), `HA_SLEEP_MAX_ATTEMPTS` the failures a job takes before it gives up,
`HA_COMPACTION_REVIEW_SECONDS` the spacing of compaction reviews, and
`HA_CONSOLIDATE_BATCH_TOKENS` how many tokens of entries one Deep Dream call
is given (32,000; a memory past it is consolidated in batches). They exist
for the live tests (`scripts/testing/live-tests/sleep.sh`); the defaults suit
everyday use.
