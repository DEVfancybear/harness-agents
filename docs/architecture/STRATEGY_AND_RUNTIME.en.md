# Strategy revision and the runtime harness

What ha took from two papers, how it works, and what is not taken. The plan
and the maintainer's decisions are in [the plan](STRATEGY_AND_RUNTIME_PLAN.vi.md).

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
| Trajectory regulation | `harness-tools/src/runtime/regulation.rs`, called after every call in `turn_driver.rs` | The same error from one tool three times, whatever the arguments, gets a note under the call naming it; five times, a note to stop. A command that exits non-zero is a failure too. Two edits undoing each other (A, B, A, B) are named. A turn near its step or call limit is told once. |
| Action realization | the same, before a call runs | The same call as the one right before it, which failed, is answered without running, when nothing ran or was said between them. File tools only, and only failures of the call itself: an approval, a stale workspace or a busy provider is never a reason. Learned rules (`rules.rs`) rename an argument the tool does not take, or refuse a shape that only ever failed. |
| Environment contract | `evolution.rs` `annotate` | Notes learned from failures are added to the tools' descriptions. |
| Procedural skill | `memory/skill_recall.rs` | The request (and the goal) is scored against every learned skill by BM25. Only the top skill goes into the turn, and only when it stands out from the rest. It comes with the assumptions it got wrong when it failed before. |

Every settled call leaves a `call settled` record with its argument hash and
shape (names and kinds, never values) and its error class: the code, and the
message with numbers, paths, quotes and file names left out. Calls the harness
or the user refused are marked `refused`. The bug report's *Strategy and
regulation* section counts all of it.

### Evolution, by itself

`memory/evolution.rs` runs at most once a day, after a turn. It holds a lock,
so one process runs it at a time, and marks the run when it starts.

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

- **`task_state`** names the strategy (with what it was weighed against). It
  also keeps the paper's typed journal (plan, observation, lesson, eval, eval
  analysis) and the decisions.
- **Every refused `goal_complete` is an evaluation**, scored by how far the
  work got: checklist, then checks, then verifier. Ties are broken by how much
  is left at that stage, and then by how much the failure printed, so a test
  run going from forty failures to two counts as progress. A task whose model
  never used `task_state` is evaluated too.
- **After an evaluation, a decision is due.** The refusal and every turn's
  task state say so. It advises and never blocks (the maintainer's choice):
  the model weighs the evidence and records `decide` continue or switch.
- **Plateau.** Three evaluations under one strategy without getting further is
  a plateau. An explorer child with a fresh context, the strategist, is asked
  once per plateau for two or three *other* strategies, named. The refusal and
  the state carry them. A decision to continue gets a plateau's worth of
  evaluations before the strategist is asked again.
- **`compare_children`** runs the project's checks in each coder's own
  worktree, so two strategies tried side by side are judged before one is
  integrated.
- **`/strategy` shows it all**, and the TUI says when a plateau is reached.
  Nothing asks the user to choose.
- **The review reads the task's strategy record** with the conversation, so a
  strategy that stalled can become a lesson for the next task.

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
