# What the review is told before it plans again

English | [Tiếng Việt](SELF_IMPROVEMENT_GATE.vi.md)

What ha took from two papers, how it works, and what was not taken:

- **[GUARD] arXiv 2611.23476.** A self-improvement loop is gated by *regret*:
  each criterion a candidate is graded on is told apart as one that
  **regressed** (it worked before the edit and fails now), one **still
  unsolved**, or one **mastered**, and each is handled differently. A
  **ledger** of rejected edits (the last `L_max`) is read back every round, so
  the proposer does not offer them again. A **Soft-HURT gate** promotes a
  candidate only if it does not hurt what already worked, with one
  scope-repair try for a candidate that does.
- **[SIM] arXiv 2611.18856.** A self-improvement memory keeps, for every
  attempt - accepted *and* regressed - the root-cause hypothesis, the edit,
  the outcome it predicted and the outcome observed, and shows it in full to
  the proposer. A proposer that sees only what failed repeats the cause it
  already tried.

Code: `crates/harness-cli/src/host/memory/refused.rs` (the ledger),
`learned.rs` (`Revision`, `Standing`, `begin_trial`, `settle_trial`,
`library_overview`) and `refine.rs` (`apply`, `plan`, `planner_given`). Where
the revision and its trial are decided, see [cost and
skills](COST_AND_SKILLS.en.md) section 4.

## 1. What ha 0.9.14 already had, and what changed

| GUARD / SIM | ha 0.9.14 | What changed |
|---|---|---|
| A ledger of rejected edits, read back each round | A learned skill kept its last 5 rejected revisions and showed them as "rejected before" lines. A **memory or prompt-note** edit the host refused left its reason only in the refinement's record (`review.applied`), so the next review planned it again | Each scope keeps the memory and prompt edits the host refused (`refused.json`, at most 12, deduplicated); every planner that proposes memory edits gets a `<refused_before>` part |
| Regressed / unsolved / mastered | A skill's counters were shown (judged worked and failed, checks, "last failure"), but no skill was told apart as having *lost* something | Each learned skill is marked `regressed`, `unsolved` or `mastered` from its usage record, with one instruction for each; a mastered skill shows what it worked in |
| A memory of hypothesis, edit, prediction, outcome | The trial kept the edits and the incumbent's counters; a rolled-back revision kept a reason | A revision carries a `cause` and an `expect`; the trial, the accepted and the rejected records keep them with the observed counts |
| Accepted attempts kept as well | Only rejected revisions were kept; an accepted one left a ledger line and nothing the planner saw | The last 5 accepted revisions are kept per skill and shown with the rejected ones, as one history |
| A candidate checked against what worked, with a scope repair | The revision goes on trial and is rolled back when its judged uses go worse (SkillOpt, section 4 of the cost document); the repair pass fixes a refused intent | Unchanged (see "Not taken") |

## 2. The ledger of refused edits

A memory or prompt-note edit a review planned, that the host refused, is kept
in the scope it was applied to, in `refused.json` beside that scope's state:
`<data>/harness/refused.json` for the global scope and
`<data>/sessions/<task>/harness/refused.json` for a conversation's local one.
It is a refusal when the edit is **not applied and has an error**; one that was
**skipped** (its entry changed while the review planned, a merge whose
survivor was not written) or that **waits for the person** in the memory queue
is not, and a rollback or an accepted queued change is not a planner's edit.

```json
{"refused": [
  {"at": "2026-11-19T08:15:02Z", "kind": "memory", "action": "create",
   "target": "deploy_key", "reason": "the harness never keeps secrets; this looks like ...",
   "summary": "Deploy key: The deploy key is [REDACTED]",
   "evidence": "the deploy key is [REDACTED]"}
]}
```

- **Bounded and deduplicated.** At most 12 records per scope (`KEPT`, GUARD's
  `L_max`), the oldest dropped. A record with the same kind, target and reason
  replaces the older one. The reason, the summary of what was proposed and the
  evidence quote are one line each, clipped to 300, 200 and 200 characters, and
  redacted as memory text is.
- **Refusals for safety keep no word of the edit.** An edit refused because it
  holds a secret, steering text (an injection or exfiltration pattern), or a
  fetched page's words made a rule (and a Deep Dream edit whose quote holds a
  secret) is marked `safety` where it is refused. Its record keeps only the
  kind, the action, the reason, and the target: the entry's id when the edit
  named one, `a new entry` for a create (a new entry's id and title are the
  edit's own words). It has no summary and no quote, and the part says *refused
  for safety: its words are not kept*, so the ledger never carries the text
  back into every later prompt.
- **Read back.** Every planner of memory edits - the automatic review, `/refine`
  and `/learn` (`plan`), and Deep Dream's batch curator (`memory_edits`) - gets a
  `<refused_before>` part, the newest first, up to 12 over both scopes, after
  this sentence: *The host refused these memory and prompt edits earlier, with
  why. Do not propose the same change again unless the conversation shows
  something the refusal did not know.*
- **Diagnostics.** `refine` / `refusal_kept` for each record kept (and `refine`
  / `refusal_not_kept` when the file cannot be read or written; an unreadable
  ledger is not written over). `refine` / `planner_given` is written for each
  plan, with the names of the prompt's parts and counts only (refused lines,
  standings, "worked before" and "revised" lines, all counted from the prompt
  text the planner is given, not from the library): the prompt itself is not
  stored, so a bug report and the live tests see that the parts reached the
  planner.

## 3. A revision's cause and expectation, and its history

The planner is asked, in the learned-skill format, to give every `edit`,
`patch` or `update` of an existing skill two optional sentences: `cause`, the
root cause of the failure or gap the revision addresses, and `expect`, what
following the revised skill should now do differently.

- When a revision lands, `begin_trial` keeps both in the trial. Changes that
  join one revision join their causes (`a; b`); a fix that replaces a revision
  judged worse starts with its own.
- When the trial settles, an **accepted** revision is kept in the skill's
  `revisions` list: `{at, kind: "accepted", ledger_id, cause, expect,
  edits_summary, good, bad}`, the last 5. A revision that is replaced by a
  later one while judged no worse is kept as accepted too. A **rolled-back** or
  **superseded** revision keeps its `cause`, `expect` and the observed `good`
  and `bad` in the `rejected` list; a **refused** one keeps its `cause` and
  `expect` (it never ran, so no counts).
- The library view the planner, the curator and the repair pass read merges the
  three into one history, oldest first, the last three lines (the revision on
  trial last):

  ```
  revised 2026-11-17 (rolled_back): cause: the port was busy; expected: it frees the port first; observed: 0 good, 2 bad; why not kept: judged worse than the version it replaced ...; tried: append "Use --fast."
  revised 2026-11-19 (on trial): cause: the flag name was wrong; expected: the flag works; observed: 0 good, 1 bad; tried: append "Use --lean."
  ```

  A record without a cause (from before) prints without it. The sentences that
  explain the lines say: a revision that was rolled back, refused or superseded
  did not stand, so do not propose it again, and when its cause was right but
  its change wrong, address the same cause another way; an accepted one stands.

## 4. Regressed, unsolved, mastered

Each learned skill's usage record gives it one standing, with no model call
(`LearnedSkill::standing`), shown as `; status: ...` in the library view:

| Standing | When | What the planner is told |
|---|---|---|
| `regressed` | a revision is on trial, at least one judged use since it failed, and the incumbent (the version before it) was sound: it worked, or its checks passed | Narrow when the new text applies: add a condition ("When <condition>: ...") around the changed steps, instead of rewriting what the incumbent did |
| `mastered` | it worked at least twice and has not failed since it was last revised (or ever, when never revised) | Edit it only to add; never remove or replace a step its "worked before" lines rely on. Its last two cases (evidence quotes, clipped to 200 characters) are shown under it as `worked before: ...` |
| `unsolved` | it failed (or a check failed after it) and has never worked, and no trial of it is regressed | It may be rewritten |

"Since its last revision" is the usage record's `trial_good` / `trial_bad`,
which a revision starts again. A trial with only good uses is not regressed;
a skill that worked twice and failed once since its last revision is none of
the three. The same sentences are in the learned-skill format text that the
planner, the curator and the repair pass read, and the repair prompt asks to
keep the condition or the "only add" when it fixes an intent.

**A prune is not such a revision.** The "only add" rule is about revisions that
change what a skill does. The prune pass (`ha learning prune`, [cost and
skills](COST_AND_SKILLS.en.md) section 5) reads the same format text and the same
status, and mastered skills are its usual candidates: its prompt says a prune may
shorten the wording and drop what none of the skill's "worked before" lines
relies on, but keeps every step those lines rely on, and adds no rule.

## 5. Not taken

- **GUARD's Soft-HURT gate** (checking a revision against what worked before it
  lands, with one scope-repair try). The maintainer chose not to take it: it
  would add a model call to every revision of a skill that has cases, and the
  online trial with rollback stands for it - a revision that hurts what worked
  is judged by the uses that follow and put back when two go bad. A `regressed`
  mark and the "narrow with a condition" instruction are what GUARD's regret
  gives the planner here, without a check before landing.
- **GUARD's M >= 2 candidates per round, and ContrastEvolve**: they multiply
  the model calls of every review by M and add a contrast pass; ha proposes one
  revision a review.
- **GUARD's per-task checklist graded per criterion**: ha judges a skill's use
  as a whole (worked or failed, with a quote), not criterion by criterion.
- **Rerunning past tasks to measure a candidate**: a coding agent's past
  conversations have side effects (files written, commands run, calls made);
  ha does not replay them. A skill's recorded cases (evidence quotes) are
  what it must keep working, shown to the planner, and the next real uses are
  the test.
- **"Harnesses That Improve Themselves" (arXiv 2611.22299)**: it evolves the
  harness's own code. ha's runtime is compiled Rust, and its mechanisms (the
  pre-stop check, the repeated-call breaker) are fixed code, not text the
  model rewrites.
- **"Inference after coding agents" (arXiv 2611.23174)**: a statistical
  procedure for data analysis after an agent's run; at most a skill, not a
  harness mechanism.
- **"Recirculation" (arXiv 2608.17981)**: it needs the model's activations,
  which a hosted model does not give.

## 6. Tests

- Unit tests: `refused.rs` (a refusal is one with an error; kept,
  deduplicated, capped at 12, redacted; skipped and queued edits not kept; an
  unreadable ledger not written over; the part and its instruction);
  `refine.rs` (`an_edit_the_host_refused_is_kept_and_the_next_planner_is_told`,
  `a_skipped_or_queued_edit_is_not_kept_but_a_refused_one_among_them_is`,
  `the_planner_is_told_a_skills_standing_and_asked_for_a_cause`,
  `an_edit_refused_for_safety_is_kept_without_its_words`,
  `the_planner_given_record_counts_the_standings_from_the_prompt`; `refused.rs` also
  `a_refusal_for_safety_keeps_no_word_of_the_edit_and_an_ordinary_one_keeps_them`);
  `learned.rs` (`a_revisions_cause_and_expectation_live_in_its_trial_and_its_history_line`,
  `a_rolled_back_a_superseded_and_a_refused_revision_keep_their_cause_and_counts`,
  `the_accepted_history_keeps_the_last_five`,
  `a_skill_is_regressed_unsolved_or_mastered_by_its_usage_record`).
- Live: `scripts/testing/live-tests/guard.sh`, with a real model: a refused
  memory edit kept in `refused.json` and told to the next review, the
  standings and history lines reaching the planner, and a revision's cause and
  expectation kept in its trial.
