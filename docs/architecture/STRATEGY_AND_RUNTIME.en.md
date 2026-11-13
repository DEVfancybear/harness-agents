# Strategy revision and the runtime harness

What ha took from two papers, how it works, and what is not taken.

- **[A] arXiv 2608.19072**, *What Is Missing from AI Post-Training AI*: agents
  execute well and lock into the strategy they start with. Only 2.1% of
  adjacent attempts changed it. A journal and more reasoning did not move it,
  and "reconsider" with no alternative named did not either. One mid-run
  instruction that named another strategy beat the agent's own continuation by
  up to 17 points.
- **[B] arXiv 2605.22166**, *LIFE-HARNESS*: improve a frozen model by adapting
  the runtime around it. It has four layers (environment contract, procedural
  skill, action realization, trajectory regulation), evolved from failed
  trajectories, and a change is kept only when it measurably helps.

## The runtime layers ([B])

| Layer | Where | What it does |
|---|---|---|
| Trajectory regulation | `crates/harness-tools/src/runtime/regulation.rs`, called after every call in `crates/harness-tools/src/runtime/turn_driver.rs` | The same error from one tool three times, whatever the arguments, gets a note under the call naming it; five times, a note to stop. A command that exits non-zero is a failure too. Two edits undoing each other (A, B, A, B) are named. A turn near its step or call limit is told once. |
| Action realization | the same, before a call runs | The same call as the one right before it, which failed, is answered without running, when nothing ran or was said between them. File tools only, and only failures of the call itself: an approval, a stale workspace or a busy provider is never a reason. Learned rules (`crates/harness-tools/src/runtime/rules.rs`) rename an argument the tool does not take, or refuse a shape that only ever failed. |
| Environment contract | `crates/harness-cli/src/host/memory/evolution.rs` `annotate` | Notes learned from failures are added to the tools' descriptions. |
| Procedural skill | `crates/harness-cli/src/host/memory/skill_recall.rs` | The request (and the goal) is scored against every learned skill by BM25. Only the top skill goes into the turn, and only when it stands out from the rest. It comes with the assumptions it got wrong when it failed before, with the words of a turn it worked in, and said to be unproven when nothing has shown it to work; one that keeps failing is not recalled ([teamwork](TEAMWORK.en.md)). |

Every settled call leaves a `call settled` record with its argument hash and
shape (names and kinds, never values) and its error class: the code, and the
message with numbers, paths, quotes and file names left out. Calls the harness
or the user refused are marked `refused`. The bug report's *Strategy and
regulation* section counts all of it.

### Evolution, by itself

`crates/harness-cli/src/host/memory/evolution.rs` runs at most once a day, after a turn. It holds a lock,
so one process runs it at a time, and marks the run only once it saved: a run
that fails is tried again by its job in the queue of background passes
([Sleep](SLEEP.en.md)) after a backoff, and gives up after 6 failures. Before
0.8.9 it was marked when it started, and one failed call put it off a day.

1. It judges the rules on what they did. A rule in shadow is promoted once it
   would have acted three times, each on a call that failed, and retired the
   first time it would have acted on a call that worked. An active rename is
   retired once the calls it renamed kept failing.
2. It mines failures of one tool with one class, three or more, not shown
   before (or grown to double since). It shows them to the model with the
   argument shapes that failed and the ones that worked.
3. The model sorts each group by [B]'s priority (realization, contract,
   degeneration, reasoning). Only realization and contract groups get a
   change: a rule in the vocabulary, or a note.
4. A rule must replay onto three or more recorded failures and no recorded
   success, then starts in shadow.

What ha has in place of [B]'s benchmark score is its own records, replayed and
then observed in shadow. Rules and notes apply to the root's turns, to
delegated children and to `ha exec`. Deleting `harness-evolution.json`
forgets them.

## Strategy revision ([A])

Code: `crates/harness-cli/src/host/memory/strategy.rs` (evaluations, plateau,
decisions) and `crates/harness-cli/src/host/coordination/goal.rs` (where a goal
is evaluated).

- **`task_state`** names the strategy (with what it was weighed against). It
  also keeps the paper's typed journal (plan, observation, lesson, eval, eval
  analysis) and the decisions.
- **Every refused `goal_complete` is an evaluation**, scored by how far the
  work got: checklist, then checks, then verifier. Ties are broken by how much
  is left at that stage, and then by how much the failure printed, so a test
  run going from forty failures to two counts as progress. A task whose model
  never used `task_state` is evaluated too.
- **So is a goal's turn that ends short of the goal** and evaluated nothing:
  the host evaluates it after the turn (`evaluate_turn` in `goal.rs`), whether
  or not the model asked. A turn that changed nothing in the workspace is an
  unchanged evaluation; one that changed something runs the project's checks,
  or reads the checklist when there are none.
- **After an evaluation, a decision is due.** The refusal and every turn's
  task state say so. It advises and never blocks (the maintainer's choice):
  the model weighs the evidence and records `decide` continue or switch.
- **Plateau.** Three evaluations under one strategy without getting further is
  a plateau. An explorer child with a fresh context, the strategist, is asked
  once per plateau for two or three *other* strategies, named. The refusal and
  the state carry them. An answer that names none, and a consult that fails,
  count as asked (the diagnosis, if it gave one, is still shown): asked again
  at every evaluation, a plateau cost a consult of two or three minutes each
  time. A decision to continue made at a plateau gets a
  plateau's worth of evaluations before the strategist is asked again. The
  `continue` decided after an ordinary evaluation does not hold it back.
- **`compare_children`** runs the project's checks in each coder's own
  worktree, so two strategies tried side by side are judged before one is
  integrated.
- **`/strategy` shows it all**, and the TUI says when a plateau is reached.
  Nothing asks the user to choose.
- **The review reads the task's strategy record** with the conversation, so a
  strategy that stalled can become a lesson for the next task.

## Off

- `HA_MEMORY_OFF=reviews` (or `HA_MEMORY=off`, or `harnessRefinements` off)
  stops the evolution: it does not run, and no rule or note is added.
- `HA_MEMORY_OFF=lessons` (or `HA_MEMORY=off`) removes the learned rules, the
  tool notes and the recalled learned skill from the root's turns, delegated
  children and `ha exec`. What was learned stays in `harness-evolution.json`.
- The memory switch is `crates/harness-cli/src/host/memory/switch.rs`.

## Not taken

- **[A]'s training signal from forked runs**: ha does not train models.
- **[B]'s free-form harness code**: rules are data in a closed vocabulary.
- **[B]'s acceptance by benchmark score**: ha has no benchmark. Replay and
  shadow observation stand in for it.
- **Not in this version:** the strategy layer is driven by `/goal`. A
  mid-task `/verify`, a feature's check and the regulator's
  repeated-failure notes are not evaluations of the strategy yet. A `switch`
  does not fork a branch by itself, and the verifier cannot yet be pointed at
  a coder's worktree.
- **Proposed and not built:** the rule vocabulary's `block_repeat`,
  `require_read_before` and `normalize_path` (it has `RenameArgument` and
  `RefuseShape` only), a signal for a streak of empty results, a
  `/harness evolve` command, and `workspace_rev` in call records.
