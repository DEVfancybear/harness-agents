# Self-evaluation of a finished turn

English | [Tiếng Việt](SELF_EVALUATION.vi.md)

A turn's final answer scored on five quality axes, by a model call of the
host's, not by the agent inside its own turn. It comes from ECC's
[`agent-self-evaluation`](https://github.com/affaan-m/ECC/tree/main/skills/agent-self-evaluation)
skill. ECC only describes the evaluation. ha runs it, checks it, acts on it
once and keeps it.

Code: `crates/harness-cli/src/host/memory/self_eval.rs` (the judge, the
report, the log and the stats) and `TurnEvaluation` in
`crates/harness-tools/src/runtime/turn_driver.rs` (the gate and the send-back).
It is wired in `host/application/service.rs` (chat) and
`host/protocols/headless.rs` (`ha exec`).

## 1. How it maps to ECC's skill

| ECC | ha |
|---|---|
| Five axes of equal weight, each an integer from 1 to 5: accuracy, completeness, clarity, actionability, conciseness | The same axes, scale and wording, in the judge's prompt |
| Every score below 5 cites evidence | Checked: an axis below 5 with no evidence makes the evaluation invalid. It is recorded as invalid. No score is invented or raised |
| Edge rules: an ambiguous request is not penalised, a simple task can score 5, an error fixed during the turn still scores 5, a tool result that contradicts a claim caps accuracy at 2 | In the prompt. The judge reads the turn's tool results, so it can see the contradiction |
| Anti-patterns: all 5s without evidence, penalising scope nobody asked for, preference used as evidence | In the prompt |
| Self-check: "would the user agree?" | Asked, and kept as `user_would_agree` with one sentence of why |
| Overall = mean, one decimal; verdict by the weakest axis | Computed by the host from the scores, not taken from the model |
| The report template (bars, `OVERALL`, `CRITICAL ISSUES`, `TOP IMPROVEMENTS`, `VERDICT`) | `/self-eval` prints it |
| `evaluate.py` scores with regular expressions | No text heuristics. A model judges, and the host checks only the shape of its answer |
| Nothing is kept | Each evaluation is one line in `evaluations.jsonl`, and `ha learning report` sums them up |
| The hook echoes a reminder the model may never see | The turn driver sends a weak turn back once, with the evidence |

Deliberate differences:
- **The judge is outside the turn.** An agent that grades its own answer in
  the same turn is checking its own reasoning. The host makes a separate call
  with the helper model, and the conversation is given to it as data, never as
  instructions.
- **Not a lesson.** The scores never feed the learning loop
  (`LEARNING_LOOP.en.md`). The agent's work judged by a model is not an
  independent signal of what the user wants. The stats are printed next to the
  lessons, never among them.

## 2. When a turn is evaluated

ECC's triggers are work across 3 or more files or 50 or more lines, a
multi-step workflow, debugging that took 3 or more attempts, a design or an
analysis, or the user asking. It skips single tool calls and conversation.
ha uses only the triggers it can check from structure:

- the turn changed **3 or more files** (`files`);
- its changes touched **50 or more lines** (`lines`): the `+`/`-` lines of each
  edit's diff, and every line of a whole-file write (an upper bound when the
  file already existed);
- it **finished a goal**: `goal_complete` passed in it (`goal`);
- **the user asks** with `/self-eval` (`manual`).

A turn that only talked, or made one small change, is not judged. "A design or
an analysis" and "debugging that took 3 attempts" would need the text judged to
detect them. The maintainer's rule is that no word list may judge text, so
these are left to `/self-eval`.

The run `ha memory migrate` starts is not judged either: the store's lint checks
it right after, and a judge that reads only the last tool results sent a whole
migration back for a read it could not see.

## 3. The end-of-turn chain

When a turn's answer has no tool call, the checks run in order, and at most one
of them sends the turn back:

1. The goal evaluator, when the turn has a goal spec (`ha exec --goal`).
2. Stop hooks.
3. **The verify reminder or jev** (`GUARDS.en.md`). It is deterministic: code
   changed and no command ran after the last change. With jev's completion
   check on, jev judges here instead of the reminder.
4. **Self-evaluation.** It runs only when none of the above sent the turn back,
   jev's completion check is off (jev judges when it is on), a step is left,
   the turn was not evaluated before, and the gate above passes.

What the result does:
- **Weakest axis ≤ 2:** the turn goes back once with `Before you finish: a
  self-evaluation of this turn scored <axis> N/5 (<evidence>; fix:
  <improvement>). Fix it, or say plainly why it stands.`
- **Weakest axis = 3:** the turn goes back once, asked to fix those axes or
  name them in its answer.
- **Otherwise:** nothing is sent and the turn ends.

The conversation shows `self-evaluation: an axis scored low; asking for a fix`.
A turn is evaluated at most once and sent back at most once. The answer after
a send-back ends the turn.

A judge that cannot answer, or answers out of shape, sends nothing back. The
failure is recorded.

## 4. `/self-eval [focus]`

`/self-eval` judges the last turn that ended with an answer. With a focus, it
asks the judge to look closely at that. It prints ECC's report:

```
AGENT SELF-EVALUATION REPORT
Overall score 3.8/5 across 5 quality axes.

Accuracy       ████░ 4/5
  Evidence: ...
  Improvement: ...
...
OVERALL 3.8/5

CRITICAL ISSUES (axes <= 2)
  - Completeness (2/5): ...

TOP IMPROVEMENTS
  1. Completeness (2/5): ...

VERDICT: REDO: fix the critical issues before delivering
Would the user agree: yes - ...
```

It never sends the agent back. The report is for the user. While a turn runs,
`/self-eval` waits for it to end. There is no `ha self-eval` CLI. `ha exec`
judges automatically, and `ha learning report` reads the results.

## 5. Switches

- `"selfEvaluation": "auto" | "manual" | "off"` in `settings.json` (default
  `auto`). `manual` keeps only `/self-eval`. `off` turns off both.
- `HA_SELF_EVAL=auto|manual|off` overrides the setting.
- Delegated children are never evaluated: their parent judges their result.
- `ha exec` is evaluated as the chat is when the mode is `auto`. The mock
  profile is not.

## 6. What is kept

Each evaluation, automatic or manual, appends one line to
`<data dir>/harness/evaluations.jsonl`:
- `at`, `conversation`, `project` (the project key), `trigger`,
  `changed_files`;
- `scores` per axis, `mean`, `weakest`, `verdict`, `sent_back`,
  `user_would_agree`;
- or `valid: false` with the `error`.

The request is kept to its first 200 characters, after the learned-skill
secret rules redact it. Error text is redacted the same way. The answer and the
tool results are not kept.

The log is moved to `evaluations.jsonl.old` past 5 MB, as the kernel's stderr
log is.

`ha learning report` ends with the last 30 days:
- how many evaluations were valid and how many were refused;
- the share sent back;
- per axis the mean and the count;
- the weakest axis.

Each evaluation also reports `self_eval evaluated` to the diagnostics, with
`trigger`, `mean`, `min_axis`, `min_score`, `verdict` and `sent_back`. A
refused one reports `self_eval invalid`. Both reach the bug report.

## 7. Tests

- `turn_driver.rs`:
  - `a_turn_is_evaluated_only_when_it_did_enough`;
  - `the_reminder_and_jev_come_first_and_a_turn_is_judged_once`;
  - `changed_lines_are_counted_per_file`;
  - `the_review_names_each_result_by_its_tool_and_keeps_its_end`.
- `self_eval.rs`:
  - `an_evaluation_out_of_shape_is_refused` (out of range, a fraction, a
    missing axis, no evidence);
  - `the_verdict_follows_the_weakest_axis`;
  - `only_a_weak_axis_sends_the_turn_back`;
  - `the_report_follows_ecc_template`;
  - `the_switches_set_the_mode`;
  - `the_stats_read_the_last_thirty_days`;
  - `a_full_log_is_moved_aside`;
  - with a mock provider: `a_weak_turn_is_sent_back_kept_and_reported`,
    `an_invalid_evaluation_is_kept_as_such_and_sends_nothing`,
    `self_eval_without_a_turn_says_so`.
- `tests/milestone_m4.rs`, a real turn driver with a scripted provider:
  - `self_evaluation_sends_a_qualifying_turn_back_once`;
  - `self_evaluation_waits_behind_the_verify_reminder`.
