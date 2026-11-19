# What a turn costs, and skills trained like weights

English | [Tiếng Việt](COST_AND_SKILLS.vi.md)

What ha took from four papers, read in full on 2026-11-18, how it works, and
what was not taken:

- **[P1] arXiv 2611.19823**, *EfficientSkill: Learning Skills for
  Token-Efficient Multi-Turn Agents*. First get a skill working, with no
  penalty on its length (Phase 1); then, from that working skill, prune it
  (Phase 2): shorter instructions, fewer turns, repeated code moved into
  helper scripts. A pruned version is kept only while its accuracy stays
  above 0.9 of the working one's and its output tokens drop. Output tokens
  fell 38-80% on 9 of 12 model-benchmark pairs and accuracy rose on all three
  benchmarks. **Negative results:** pruning straight from the seed skill
  stalled (3.6% against 72.5% on SpreadsheetBench); Grok on ALFWorld saved
  only 3.2%, and Gemma emitted 4.4% more; running the accuracy-only search
  longer did not help; the optimizer's own calls cost more than they saved on
  cheap single-turn runs.
- **[P2] arXiv 2611.12122**, *System Optimizations for Cost-Efficient Agent
  Harnesses* (Agency). The harness barely moves accuracy but moves cost about
  2×. Reading the cached context dominates long runs (up to 73% of the cost),
  only 27% of tool-output tokens are used later, and every tool call re-reads
  the context. Five techniques together brought cost to 0.77 at an accuracy
  ratio of 0.97. **Negative results:** the batching prompt raised cost 2% on
  GPT-6-Luna (it added needless calls); the SQLite file index was almost never
  queried on coding tasks (0.2 lookups a run) and Luna misused it; 17-29% of
  tasks cost more than the baseline.
- **[P3] arXiv 2605.23904**, *SkillOpt: Executive Strategy for Self-Evolving
  Agent Skills*. The skill document is trained like weights: a few structured
  edits per step (the textual learning rate, 4 by default), a held-out gate
  that accepts an edit only when it does strictly better, a buffer of rejected
  edits fed back, and a slow epoch-wise update in a protected region. Best or
  tied on 52 of 52 cells; final skills of 379-1,995 tokens from 1-4 accepted
  edits. **Negative results (ablations):** an unbounded rewrite did worse
  (84.6/75.7/57.3 against 87.1/77.5/61.3), so did dropping the rejected
  buffer (-1.6 to -4.6 points) and the slow/meta update (-22.5 on
  SpreadsheetBench); batch sizes and schedules mattered little.
- **[P4] arXiv 2611.17167**, *Actor, Judge, or Teacher? Understanding
  Decision Models in Long-Horizon Agents*. A decision model that acts poorly
  (JEV: 14.7% on tasks whose subgoals must come in order) judges well once it
  sees the whole finished trajectory (Hindsight) and is asked whether each
  action helped achieve the goal (Progress) rather than whether the task will
  succeed (Value): Progress puts milestone actions 0.36 above no-ops, Value
  only 0.05. As a teacher in RL it raised a 1.5B policy from 72.8% to 82.9%
  on ALFWorld. **Negative results:** a hint to "consider dependencies"
  changed acting by -2.8 to +5.2 points; with only the prefix visible the two
  questions taught about equally.

## 1. Is ha already equivalent? The gap tables

### P1 EfficientSkill

| Paper | ha 0.9.11 | Gap, and what was done |
|---|---|---|
| A skill is instructions plus workspace helpers whose bodies stay out of the prompt | A learned skill is `SKILL.md` plus `scripts/`, `references/` (at most 16 files); the recalled skill lists each script's path and says to run a tested one first | **Equivalent** |
| Phase 1: repair from failures until it works | The review and `/refine` patch skills from judged outcomes; `unproven()` until a use worked | Equivalent in spirit; the measured gate is §4 |
| The cost of a skill in use: output tokens and turns | Not measured: the usage record counts offers, uses, views and outcomes only | **Gap.** Done: §5, the cost of each skill in play |
| Phase 2: prune only a working skill, kept while accuracy ≥ 0.9·A* and tokens fall | No compression pass; length is bounded by the lint only | **Gap.** Done: §5, the prune pass |

### P2 Agency

| Paper | ha 0.9.11 | Gap, and what was done |
|---|---|---|
| Cost = cache read + cache write + uncached input + output, each at its price | `calculate_cost` with the four prices | Equivalent |
| Where the money went, by part | `/cost` gave one total; `ha exec --json` gave tokens, no dollars | **Gap.** Done: §3 |
| Tool output compaction with cross-output line pointers | Size caps, tail cuts, the edit's diff, jev's chunk admission (off by default); a re-read of an unchanged file was sent whole again | **Gap.** Done: §2 |
| Context garbage collection by predicted reuse | `trim_transcript`, `HA_CONTEXT_DROP`, the runtime's fit, all cache-aware | Partial; not taken (§8) |
| Subtask stack machine | The model-written fold of a turn's steps; children return reports | Partial; not taken (§8) |
| Metadata-assisted search (an SQLite index of files and symbols) | None the agent can query | Not taken (§8) |
| Tool-call batching prompt | "request them together in one step", calls run concurrently, codemode | **Equivalent** |

### P3 SkillOpt

| Paper | ha 0.9.11 | Gap, and what was done |
|---|---|---|
| Edits are append / insert_after / replace / delete | `patch` was one exact replace; `update` a whole rewrite | **Gap.** Done: §4, the `edit` action |
| At most L edits a step (the textual learning rate) | No bound | **Gap.** Done: §4, at most 4 a refinement |
| Accepted only when strictly better on held-out tasks, else rolled back | Lint and safety only; a failing skill was held back from recall; rollback by hand (`/refine --rollback`) | **Gap.** Done: §4, the trial; ha has no held-out set, the next judged uses stand in for it |
| Rejected edits fed back | The repair pass saw this refinement's refusals; the history said `failed` with no reason | **Gap.** Done: §4 |
| A per-step report of each edit | `.ledger.jsonl` | Equivalent |
| Protected slow-update region, optimizer meta skill | None | Not taken (§8) |

### P4 Actor, Judge, or Teacher?

| Paper | ha 0.9.11 | Gap, and what was done |
|---|---|---|
| A decision model answering typed questions | The jev integration: ten online decisions, `noul` / `choice` / `score` | Equivalent (the interface) |
| Judge every action of a finished trajectory, Hindsight + Progress | Nothing scored past actions: self-evaluation judges the final answer; a check's result was credited to every skill used before it | **Gap.** Done: §6 |
| Teacher: step scores in the RL advantage | ha trains no model | Not taken; the analogue is §6's use in the review |

## 2. A re-read the model holds is not sent again (P2)

Code: `crates/harness-tools/src/runtime/reread.rs`, called from the turn
driver (`crates/harness-tools/src/runtime/turn_driver.rs`) where a call's
result becomes the text the model reads.

Only the text the model reads changes. The tool runs as before, the user is
shown the whole result, and the stored output (executions, memory, receipts,
codemode) is whole.

- **An unchanged re-read.** A `read_file` of a file whose content has the
  same sha256 as an earlier `read_file` result of this turn, for the same
  lines or lines inside it, answers:
  `notes/plan.txt: unchanged since your read_file call <id> (sha256:<12 hex>, lines A-B); that result above holds it. Pass "fresh": true to read it again.`
  `read_file` takes `fresh: true` to read it whole anyway.
- **A re-read after a change.** When the file changed since that read and the
  change is small (at most half of the lines, and at most 200), the result
  shows only the changed lines, numbered as `read_file` numbers them, with one
  line per unchanged run (`lines 12-80 unchanged since call <id>`, and
  where they were before when they moved) and the new hash. A larger change,
  or lines with hashline anchors that moved, is read whole.
- **Only what the model still holds is pointed to.** A result counts while it
  is in the turn's transcript and not elided, shortened, dropped
  (`HA_CONTEXT_DROP`), fitted by the runtime or folded into a summary. Before
  each request, a pointer whose target is gone by now is replaced by its full
  text. Results of earlier turns are never pointed to: the runtime may
  summarize or shorten them when it builds the next request, and the driver
  cannot see which. Two windows remain, both recoverable: the runtime fits or
  folds inside the request it sends, so a pointer made in that very step is
  restored from the next one; and a pointer kept in the conversation may, in
  a later turn, sit after a result the runtime shortened. The shortened
  result says it was shortened, and the pointer says to pass `"fresh": true`.
- **Repeated lines of command output.** In `run_shell`, `run_process` and
  `read_process_output` results, a run of 4 or more identical lines (trailing
  whitespace aside) is the first line and `[previous line repeated N more
  times]`. The captured output keeps every line.
- **Kept.** Diagnostics `read.unchanged`, `read.delta`,
  `output.repeats_collapsed` and `read.restored`, each with the bytes;
  `ha exec --json` has `reread: {unchanged, delta, collapsed, saved_bytes}`,
  net of pointers sent whole again.
- **The switch.** `HA_REREAD=off` (or `0`, `false`); on by default.

## 3. What a run cost, by part (P2)

Code: `crates/harness-cli/src/host/presentation/cost.rs` (`split_cost`,
`CostParts`, `CostTracker::breakdown`) and
`crates/harness-cli/src/host/models/usage_tally.rs`.

- `calculate_cost` is the sum of `split_cost`'s four parts: uncached input
  (input less cache read and cache write), cache read, cache write and
  output, each at its price; a missing cache price is the input price.
- `ha exec --json` gives `cost_usd: {uncached_input, cache_read, cache_write,
  output, total}` in `usage`, `children_usage` and `upkeep_usage`, and for the
  run. Each call is priced at the model it was made with (a child on another
  model at its own price). A tally holding a call with no price is `null`
  rather than a partial sum.
- `/cost` keeps its first line and adds the parts with their shares and
  tokens: `cache read $0.0123 (61%, 1.2M tok) · cache write ... · uncached
  input ... · output ...`.
- The bug report's *Model calls* section gives the same split when every call
  is priced.

## 4. A skill's revision is a few edits, on trial (P3)

Code: `crates/harness-cli/src/host/memory/learned.rs` (`edit`,
`EditBudget`, `begin_trial`, `settle_trial`, `roll_back_trial`, the rejected
buffer, `revision_stats`) and `crates/harness-cli/src/host/memory/refine.rs`
(the shared budget, the repair pass, the history).

- **The `edit` action.** A revision of an existing learned skill is
  `{"action": "edit", "edits": [{"op": "append" | "insert_after" | "replace" |
  "delete", "target": ..., "content": ...}]}`, applied in order to
  `SKILL.md`. A target must occur once; a missing or ambiguous target, an
  unknown op or missing content refuses the whole intent, naming `edits[n]`.
- **The budget.** At most 4 edits (`SKILL_EDIT_BUDGET`) to one skill in one
  refinement, summed over its intents and the repair pass; a `patch` is one.
  A whole `update` of a proven skill (one whose use worked or whose checks
  passed) is refused: it must be edits. An unproven skill may still be
  rewritten.
- **The trial.** When a revision lands on an existing skill, its usage record
  opens a `trial`: the ledger id, the incumbent's counters (worked, failed,
  checks passed and failed, trial good and bad) and the undo of every file
  the change wrote. Then each judged use decides:
  - two good uses, more than bad, accept the revision (`skill
    revision_accepted`); a tie keeps the trial open;
  - two bad uses, more than good, **roll it back by itself** when the
    incumbent was sound (a use worked or a check passed): its files and
    counters come back (`skill rollback`). A skill changed since the revision
    is not overwritten; the record says so (`rollback_skipped`);
  - an incumbent that was failing too is not restored; the skill stays held
    back from recall as before.
  The review judges the text the conversation followed, so its outcomes are
  recorded before the same refinement's changes land: with a revision on
  trial, the use counts for or against that revision (and a revision that
  then replaces a failing one keeps the sound incumbent, the failing one kept
  as `superseded`); with none, it counts for the incumbent.
- **Revisions are kept and shown as one history.** Each skill keeps its last
  five refused (the promoter refused it even after the repair pass),
  rolled-back, or superseded (replaced while judged worse) revisions and its
  last five accepted ones, each with the cause it addressed, what it expected
  and the judged uses it got (`cause` and `expect` are asked of the planner;
  see [the self-improvement gate](SELF_IMPROVEMENT_GATE.en.md)). The refiner's
  and the curator's view of the library shows the last three, oldest first,
  under the skill (`revised 2026-11-18 (rolled_back): cause: ...; expected:
  ...; observed: 0 good, 2 bad; why not kept: ...; tried: ...`), with the
  skill's standing (`regressed`, `unsolved`, `mastered`), and the history names
  the reason a skill edit failed.
- **`ha learning report`** counts, over 30 days, revisions, those on trial
  now, accepted, rolled back and refused (from
  `<data>/skill-usage/revisions.jsonl`).

## 5. What a skill costs, and its prune (P1)

Code: `learned.rs` (`TurnCost`, `Cost`, `record_turn`, `prune_candidates`,
the prune trial's cost rule, `cost_stats`), `refine.rs` (`prune`,
`PRUNE_SYSTEM_PROMPT`), `service.rs` (the turn's cost, the automatic pass,
`prune_command`) and `commands/learning_cli.rs` (`ha learning prune`).

- **What a skill costs.** After each turn of the app, every learned skill in
  play (activated, viewed, or put whole into the turn by recall) adds the
  turn's model steps, tool calls and output tokens to its usage record
  (`cost: {turns, steps, tool_calls, output_tokens, output_turns}`); a turn
  with a tool call and no learned skill adds to the layer's `baseline`. Output
  tokens are the session's before and after the turn; a turn where they are
  unknown counts its steps and calls only. The refiner's view of the library
  shows `cost: N turns, mean S steps, C calls, O output tokens (turns without
  a skill: mean ...)`, and `ha learning report` lists each skill's mean cost
  next to the baseline. `ha exec` does not record skill use or cost: it never
  did record uses.
- **Which skills are pruned.** Only proven ones (P1's lesson: pruning a skill
  that does not work yet stalls): a use worked or a check passed at least
  twice, not on trial, not held back, and long (20 or more body lines) or
  costly (more steps a turn than the baseline, over at least two measured
  turns of each). At most three a pass.
- **The prune.** One model call with Phase 2's objective: keep every rule and
  check that made the skill work; cut repetition, verbose prose and steps the
  record shows are not needed; prefer fewer turns; move a command sequence
  the skill repeats into `scripts/<name>`; never add task-specific values. It
  answers with `edit` intents under §4's budget. The promoter refuses a prune
  that is not an `edit`, or whose `SKILL.md` is not shorter in non-blank lines
  or characters (naming both sizes).
- **The prune is on trial, with cost.** A landed prune opens a trial with the
  objective `prune` and the incumbent's mean cost a turn. It is rolled back
  when, over two measured turns, its mean steps (or, when both are known, its
  mean output tokens) exceed 1.10× the incumbent's, even when its uses are
  judged good (P1's acceptance rule); it is accepted when two judged uses are
  good and its mean steps are no more than the incumbent's; in between it
  waits. A repair trial (§4) is not held to cost: P1's first phase is
  unconstrained. A revision that replaces a failing one keeps the sound
  incumbent to go back to, and the replaced revision is kept as
  `superseded`.
- **When.** After the curator, on its cadence (every 250 requests), when
  there are candidates; `HA_SKILL_PRUNE=off` stops it. `ha learning prune
  [--skill NAME] [--model PROVIDER/MODEL] [--thinking LEVEL]` runs it now
  (whatever `HA_SKILL_PRUNE` says; not with learned skills off). Diagnostics:
  `skill prune` with the candidates, landed and refused.
- **A mastered skill is pruned too.** A skill marked `mastered` (see [the
  self-improvement gate](SELF_IMPROVEMENT_GATE.en.md)) is a usual candidate. The
  "only add" rule is about revisions that change what a skill does; the prune
  prompt says a prune may shorten the wording and drop what none of the skill's
  "worked before" lines relies on, keeps every step those lines rely on, and adds
  no rule.

## 6. A hindsight judge of each step (P4)

Code: `crates/harness-cli/src/host/memory/step_judge.rs` (the gate, the
prompt, the shape check, the log, the review's block, `skill_progress`,
`stats`), `turn_actions` in `crates/harness-runtime/src/lib.rs` (a turn's
calls, arguments and results from the journal), wired in
`application/service.rs` (the app, after the turn, off the turn's path) and
`protocols/headless.rs` (`ha exec`, before it exits).

- **When.** After a turn that ended with an answer and made 6 or more tool
  calls, in the app and in `ha exec`; not for a delegated child, not in the
  mock profile, and not with memory's reviews off. `"stepJudge": "auto" |
  "off"` in `settings.json`, or `HA_STEP_JUDGE`, which wins.
- **The question and the context are P4's best.** One call to the helper
  model self-evaluation uses, with the request (its first 4,000
  characters), every action in order (numbered from 1: the tool, its
  arguments, the head and end of its result) and the final answer (its last
  2,000 characters), as data. It asks, for each action, *did this action help
  achieve the request?* (Progress), with the whole turn in view (Hindsight):
  a step whose use shows only later helped; a step repeated without need,
  undone later, whose result was never used, or unrelated to the request did
  not; whether the turn succeeded is not the question. No outcome label
  reaches the judge: no self-evaluation, no goal verdict, no check summary.
  A turn of more than 80 actions is judged by its last 80, numbered as they
  were.
- **The host checks only the shape.** Every action once, `helped` a number
  from 0 to 1, `why` a string. Out of shape, the judgment is recorded
  invalid; nothing is filled in.
- **Kept.** One line a turn in `<data>/harness/step-judgments.jsonl` (moved
  aside past 5 MB): the steps with their scores and reasons, the mean, the
  steps below 0.3, and for each learned skill activated in the turn the mean
  of the steps from its activation to the next activation or the end.
  Diagnostics `step_judge evaluated` / `step_judge invalid`.
- **Used.**
  - The next review of the conversation gets a `<step_judgments>` block: at
    most 12 steps judged not to help since the last review given them, as
    data; a pattern that recurs may become a lesson or a skill edit, a single
    one does not. The block is marked given only after the review's call
    succeeded.
  - The refiner's view of a learned skill shows how helpful the steps after
    its activation were judged, over the last 30 days (`steps after its
    activation judged helpful in hindsight: 72% over 5 turn(s)`): P4's
    teacher, for a harness that trains no model.
  - `ha learning report` gives the turns judged, the mean, the share judged
    not to help and the tools judged not to help most often.
  - `ha exec --json` has `step_judge: {judged, actions, mean, low}`, or
    `{judged: false, reason}` (`off`, `reviews_off`, `mock`, `no_answer`,
    `too_few_actions`, `invalid`, `provider_failed`, ...). In `ha exec` the
    judge's call counts in `upkeep_usage`, and the process waits for it
    (at most 3 minutes) before it exits, as it waits for self-evaluation:
    a scripted run that does not want the extra call sets
    `HA_STEP_JUDGE=off`.

## 7. Switches

| Switch | Default | What |
|---|---|---|
| `HA_REREAD=off` | on | §2 |
| — | on | §3 has no switch: it only reports |
| — | on | §4 is how a learned skill is revised; memory off (`HA_MEMORY=off`) revises nothing |
| `HA_SKILL_PRUNE=off` | on | §5's automatic pass; `ha learning prune` still runs |
| `HA_STEP_JUDGE=off`, `"stepJudge": "off"` | on | §6; memory's reviews off turns it off too |

## 8. Not taken

- **P2's context GC by a learned model** (LightGBM over line age,
  identifiers and references): it needs labelled traces ha does not keep.
  ha's trims stay by age and size, cache-aware.
- **P2's subtask stack machine**: the maintainer chose not to build it this
  time; the paper measured 91% a fold, but 9% more calls to fetch dropped
  text again, and it fired about once in 20 turns.
- **P2's metadata-assisted search**: on coding tasks it was almost never
  queried, and Luna misused the SQLite tables.
- **P2's batching prompt**: ha has the guidance already, and the paper's
  wording cost Luna 2% more.
- **P3's protected slow-update region and optimizer meta skill**: both need
  epochs of rollouts over a task set; ha has no task set to replay.
- **P3's held-out validation split**: ha has none; the next judged uses of
  the revised skill stand in for it, and replace "strictly better on the
  split" with "two judged uses better than worse".
- **P4's Teacher**: ha trains no model.
- **GUARD's check of a revision against what worked before it lands**: the maintainer chose not to take it; the trial with rollback stands for it ([the self-improvement gate](SELF_IMPROVEMENT_GATE.en.md), section 5).

## 9. Tests

- Unit tests: `reread.rs` and `turn_driver::reread_driver_tests`;
  `contracts::read_file_takes_a_fresh_boolean`; `cost.rs`,
  `usage_tally.rs`, `bug_report.rs`; the trial, budget and buffer tests in
  `learned.rs` and `refine.rs`.
- `crates/harness-cli/tests/milestone_m4.rs`, a real turn driver with a
  scripted provider: a held re-read is a pointer, `fresh` and a drop send it
  whole; a read after an edit is the changed lines and the switch turns it
  off; repeated output lines are collapsed and the capture is whole.
  `tests/g13_headless.rs`: `cost_usd` in the JSON.
- `step_judge.rs`: the prompt holds the turn and no outcome label, the gate,
  the switches, out-of-shape answers, the log and its rotation, each skill's
  progress, the review's block since the last review, the stats, a turn
  judged from its journal.
- Live, with a real model: `scripts/testing/live-tests/reread.sh` (§2, §3),
  `scripts/testing/live-tests/skill-trial.sh` (§4, §5) and
  `scripts/testing/live-tests/step-judge.sh` (§6).
