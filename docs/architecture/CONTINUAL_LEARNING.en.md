# What survives a conversation

English | [Tiếng Việt](CONTINUAL_LEARNING.vi.md)

What ha took from one paper read on 2026-11-12, how it works, and what is not
taken.

- **arXiv 2611.14505**, *Continual-ARC: Measuring Skill Acquisition over a
  Working Lifetime* (Lenton, Arif, Mahmood). A stream of 200 puzzle instances
  over 25 tasks, where the same task returns in a fresh conversation. It ran
  three agent harnesses, prime-agent (which ha ports much of) among them, and
  asked what each one's persistence saved on a returning task. Left alone, it
  saved nothing measurable. The authors read the stores to see why:
  - prime-agent's stores default to the session, and an entry reached a later
    session only when written global. In 600 sessions the model never did.
  - A harness that did store a rule in words showed six entries, each cut to a
    few lines, chosen before the task was known. The rule reached the model on
    40 of 154 returns; where it did, the instance cost less than half.
  - One message after every verdict, asking the harness to save anything that
    helps the task's future instances, made prime-agent learn (0.42 of its
    learnable budget saved, where as shipped it saved nothing).
  - What paid most was a tested, executable artifact per task, run first when
    the task came back (0.65 to 0.81 saved). A rule in words that ends "confirm
    against the demonstrations" costs the lookup it was meant to save.
  - No harness kept the evidence it had paid for. Each bought it again.

ha's side of the first, second and fourth already stood on different ground:
the `memory` tool makes the model name a scope, the digest is ranked against
the request, and a learned skill may carry `scripts/`. What was left were the
defaults and the words around them, and the places where a finding of the
paper had no counterpart. They are six small changes.

## Scope: say who sees an entry (P1)

Code: `AUTO_SCOPE_POLICY`, the scope policy of the review's prompt and
`Edit::observed_project_fact` in `crates/harness-cli/src/host/memory/refine.rs`;
`SCOPE_REACH` in `memory/harness.rs`; `_LOCAL_WRITE_NOTE` in
`crates/harness-cli/python/rlm/harness.py`; the continual-harness section of
`prompting/prompts/core.md`.

| Paper | ha | Why |
|---|---|---|
| Every default points to the session; the model never opts in | The review's policy no longer says edits go to the conversation's store unless marked. Local is for what only this conversation needs; a fact about the project that a tool result or a file showed outlives it | The policy was the default the model followed |
| (same) | A new memory entry the review calls `about: project`, with a quote and no scope, is written with scope project, tagged with the project. One it chose `local` for, one `about` the user, and one with no quote stay as they were | The review's own word decides; the host matches no text |
| prime-agent's digest told the model to default to local | The digest's header says who sees each scope: global, every conversation; project, this project's; local, only this one, and it is gone when it ends | The model chooses a scope by what it is told each one reaches |
| `global_=False` is the Python default | Unchanged (prime-agent's code relies on it). A write without `global_=True` prints one line: the entry is this conversation's only, and `global_=True` keeps it for later ones. The entry it returns is the same | The default stays; its cost is said where the write happens |

Every safeguard on what the review may write is as it was. A rule or a
constraint still needs the user's own words or a repeat. The review queue, the
provenance checks and the markdown-store exception apply to a project entry as
they do to a global one: it is the global store's, tagged with its project.
Only the fallback for a fact about the project changed, and only for a new
entry with a quote.

## An entry that fits the request is shown whole (P2)

Code: `whole_entry`, `Pass` and `WHOLE_CONTENT_LIMIT` in `memory/harness.rs`;
`standout_of` in `crates/harness-types/src/readout.rs`.

| Paper | ha | Why |
|---|---|---|
| Six entries, cut to a few lines, chosen before the task | The digest was already ranked against the request. The one entry that stands out for it is now shown up to 1,200 characters instead of 180 | A rule cut at 180 characters loses the step that made it worth storing |
| (a rule reached the model on 40 of 154 returns) | An entry stands out when its score lies a deviation above the mean of its kind, as learned-skill recall judges it, and it shares at least two of the request's words | A fixed score or a word list would not hold across stores; a word that one entry alone holds is not a fit |

`standout_of` is the choice `skill_recall::best_of` made, taken out so both use
one: the best score beats the next and, among five entries or more, lies
`STANDOUT` deviations above the mean. At most one entry of a digest is whole,
the best across the kinds. A constraint is never a candidate (it is shown for
every request already), and a pinned entry is as it was. The digest block keeps
its 3,000-token bound; the whole entry adds at most about a thousand
characters.

## At a verified end, ask what to keep (P3)

Code: `KeepAsk` and `GoalHost::with_keep` in `coordination/goal.rs`; wired in
`application/service.rs`.

| Paper | ha | Why |
|---|---|---|
| One message after every verdict asks to save whatever helps future instances | When a goal completes verified, the result of `goal_complete` ends with one paragraph: if this task may come back, save now what would let you do it at once next time. A fact about the project that a tool result or a file showed: the `memory` tool, scope project. The steps that worked: in the reply, in order, as the exact commands with the result that showed they worked, for the review to keep as a learned skill, and as a script when they were a command sequence. Memory written with scope local is gone with this conversation | ha asked only before a compaction summary. A verified goal is the one verdict ha trusts |

The paragraph is text in a result the agent reads anyway: no model call. It is
asked once, since a goal completes once. Each sentence appears only when a
feature that can act on it is on (`digest_on` for the memory tool,
`reviews_on` for the review); with both off nothing is added. A goal that is
refused is asked nothing.

The main agent cannot write a learned skill itself; the review does, after the
turn, from the conversation. So the paragraph asks the agent for what the review
needs to read, not for a tool call that does not exist.

## A script that ran is a tested script (P4)

Code: `SkillScript`, `record_script_runs`, `clear_script_marks` and
`LearnedSkill::scripts` in `memory/learned.rs`; `confirmed_script_runs` in
`memory/refine.rs`; `scripts_note` in `memory/skill_recall.rs`.

| Paper | ha | Why |
|---|---|---|
| A tested program per task, run first when the task returns | A learned skill's `scripts/` are listed in the recalled skill: each with the day a run of it succeeded, or "not run yet". For a tested one: run it first, before redoing the work by hand; if it fails, fix the script and say so | A rule in words has to be worked out again; a script is run |
| (tested before it is stored) | The review may put `script_runs` on a skill intent: `{path, evidence}`, the quote of a successful run. The host marks the script tested only if the quote is in a tool result that reports a clean exit (`exit=Some(0) timed_out=false`) and the call that made the result names the script's file | The host cannot run a model-written script (the approval policy forbids it), so "tested" is what the conversation showed, checked |
| (a stored program must still be the one that ran) | The mark holds the script's SHA-256. A script written again, by a patch or an update, loses it; so does one edited on disk | A mark is of one file's content |
| The expert guidance told the harness to save a program | The review is told that a command sequence that solved a task which may come back is worth keeping as a script of the skill, with the quote of its run | The review decides what to keep; no word list |

The marks are in the skill's usage record (`skill-usage/*.json`), beside
`cases`. The list in the recalled block is at most eight scripts and 1,200
characters, tested first, and sits before the case and the failures, so a cut
of the block's 1,500 tokens loses those first.

The check has limits. A quote in a page, a file read or a child's report is no
run, and neither is a run that failed or one of another file. A command that
ran the script but whose result does not carry the host's exit line is not
confirmed. The record of a refinement lists `scriptRuns` and
`scriptRunsUnconfirmed`.

## A search here that finds nothing says when earlier conversations hold it (P5)

Code: `earlier_conversations` in `crates/harness-tools/src/runtime/service.rs`;
`ToolOutput::HistorySearch::earlier` in `contracts.rs`; the rendering in
`runtime/turn_driver.rs`.

| Paper | ha | Why |
|---|---|---|
| No harness kept evidence it had paid for | ha keeps every tool result searchable, with scope `task` by default. When a search of this conversation finds no source holding every word, the same query is run at scope `project` over the same index, and the result ends with `N match(es) in earlier conversations of this project do - search again with scope project` | The model did not know an earlier conversation held what it was buying again |

The earlier hits are not returned, and the default scope is unchanged: what was
not asked for is not sent. Only sources that hold every word of the query are
counted, so the line is not made of partial matches. The project's other
conversations are caught up for at most a second; the rest is caught up in the
background. A readout-mode search, and one that already asks for the project,
say nothing of this. A failure of the extra search is a count of zero, never a
failure of the search.

## A child gets the user's appended prompt (P6)

Code: `ChildLaunch::appended_system`, `ChildLaunch::config_dir` and
`inherited_system` in `coordination/delegation.rs`; `appended_system` in
`configuration/resources.rs`; the launches in `application/service.rs` and
`protocols/headless.rs`.

| Paper | ha | Why |
|---|---|---|
| A child's prompt lacked what the parent's system prompt carried | A child's system policy ends with the per-model additions of the model the child runs on, then the text the user appended (`--append-system-prompt` or `APPEND_SYSTEM.md`), in the root's order. Up to 4,000 characters of the user's text; a cut says how long it was and that the parent has the rest | "Answer in Vietnamese" and "never touch vendor/" reached the root and no child |

It is part of the system policy built once when the child starts, so the
child's system prompt is the same in every turn and the provider's cached prefix
holds. Grandchildren inherit it with the rest of the launch. `ha exec` builds
its children's launch the same way.

## Not taken

- **The benchmark as a test suite in ha.** It measures harnesses on a puzzle
  stream; it does not change what one does. Each part has a unit test and a
  live check of its own instead.
- **Weight updates and DAgger-style learning from the stream.** ha does not
  train models.
- **An automatic program library keyed by task id** (its method PL, which runs
  the stored program first with no model call). ha has no task ids, and it
  cannot tell that a request is the same task except by its words. The tested
  script of a recalled skill, found by the request's words, is the nearest ha
  has, and the model still decides to run it.
- **Host-run scripts at promotion.** The approval policy forbids the host to
  run a model-written script. The mark is of a run the conversation made.
- **Asking after every verdict.** The paper asked after each instance's verdict.
  A coding turn has no verdict ha trusts but a goal's verified end and a
  compaction; asking after every turn would add a paragraph to each.
- **Showing more than one entry whole.** Each costs up to a thousand
  characters in every turn. The paper's finding is about the entry for the task
  at hand, which is the one that stands out.
- **Making `global_=True` the Python default.** prime-agent's code and ha's
  tests rely on `global_=False`; the write says what that costs.
- **Returning the earlier conversations' hits unasked.** A search's default is
  this conversation; the count is a line, not a change of scope.
- **The paper's findings on method advice** (general lessons helped every
  instance a little and no returning one more). ha's lessons are unchanged.

## Tests

- Unit, P1: `a_project_fact_the_review_gives_no_scope_is_kept_for_the_project`
  in `refine.rs`; `the_digest_says_which_conversations_see_each_scope` in
  `harness.rs`; `test_a_local_write_says_it_is_gone_with_the_conversation` in
  `crates/harness-cli/python/tests/test_harness_state.py`.
- Unit, P2: `the_entry_that_stands_out_for_the_request_is_shown_whole`,
  `no_entry_is_shown_whole_when_none_stands_out` and
  `one_entry_per_digest_is_shown_whole_and_never_a_constraint` in `harness.rs`;
  `one_score_stands_out_by_the_spread_of_the_rest` in `readout.rs`; the
  existing recall tests in `skill_recall.rs` run through the same function.
- Unit, P3: `a_verified_goal_asks_what_to_keep_for_the_task_s_return` in
  `goal.rs`.
- Unit, P4:
  `a_script_run_is_recorded_as_tested_and_writing_it_again_clears_it` in
  `learned.rs`;
  `a_script_run_is_confirmed_only_by_a_clean_result_of_a_call_that_named_it`
  and `a_skill_intent_with_a_confirmed_run_marks_its_script_tested` in
  `refine.rs`;
  `the_block_lists_the_scripts_and_says_to_run_the_tested_one_first`,
  `the_scripts_list_stays_within_its_bound` and
  `a_recalled_skill_carries_its_scripts_and_a_changed_one_is_untested` in
  `skill_recall.rs`.
- Integration, P5:
  `a20b_a_search_that_finds_nothing_here_counts_the_earlier_conversations` in
  `crates/harness-cli/tests/milestone_m5.rs`.
- Unit, P6:
  `a_child_gets_the_appended_text_and_the_additions_of_its_own_model` and
  `what_a_child_inherits_is_bounded_and_empty_when_there_is_nothing` in
  `delegation.rs`.
