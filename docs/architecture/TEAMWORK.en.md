# Teammates, conventions and proven skills

English | [Tiếng Việt](TEAMWORK.vi.md)

What ha took from four papers read on 2026-11-10, how it works, and what is
not taken.

- **[S] arXiv 2611.08615**, *s3: Simple Scaling of Swarms*: agents that all
  work at once, each on its own copy of the task, with a message tool and no
  roles. Trained with one team reward, they repeat nearly half as much of each
  other's work and beat the same number of agents working alone. What ha can
  use without training is the harness and the shared prompt.
- **[Z] arXiv 2611.06409**, *Zero-Shot Coordination among LLM Agents*: agents
  that cannot agree beforehand each take their own convention for the natural
  one and fail together; a stronger model or more reasoning does not fix it. A
  short *coordination-friendly definition* (CFD) in the prompt does.
- **[E] arXiv 2611.07595**, *Evolver*: strategies distilled from past runs
  (Genes), each with a concrete case (Capsule), drafts marked unproven,
  failing ones restricted. Reuse raised success 4.4 points and cut tokens 24%
  on its selected set; it warns that an outdated strategy costs more than
  none.
- **[T] arXiv 2610.27307**, *TURTLE*: a proposer evolves benchmark tasks until
  a solver completes them about half the time. Not taken yet (below).

## Teammates ([S])

Code: `crates/harness-cli/src/host/coordination/delegation.rs`
(`TEAM_PROTOCOL`, `brief_on_team`, `announce`, `no_sibling`).

Before, a child could send `agent_message` to a sibling "by name" and was
never told a sibling's name, what it was asked, or that it existed.

| [S] | ha | Why |
|---|---|---|
| Each agent is given its teammates' names | A child's first prompt ends with `Teammates:`: its parent's other children that still work, each as `- name (role): the first 240 characters of its brief` | It can address one, and sees where tasks overlap |
| The team is fixed at the start | A child started later is announced to the ones already briefed, as a `[teammate-joined: name]` message at their next step | The root starts children whenever it likes |
| A shared prompt: no roles, no required message, what is worth sending, checked apart from guessed, no polling | `TEAM_PROTOCOL`, in an explorer's and a coder's instructions | The paper's prompt, cut to what holds for ha |
| `message(to, text)` to one teammate or all; a notice wakes an idle agent | `agent_message` (`sibling` with a name, or `all`), read at the receiver's next step; a finished child is woken | Already there (prime-agent's reach) |
| A wrong recipient | The refusal names the siblings there are | A guess is corrected in one step |

- **Told once.** A child is marked briefed under the lock its teammates are
  read under. One that registers before that read is listed; one that
  registers after announces itself. Never both, never neither.
- **Who is on a team.** Explorers and coders. A verifier judges alone: it is
  told of nobody and named to nobody. So is a child the host starts for itself
  (a goal's verifier, a plateau's strategist).
- **Settled children are left alone.** A teammate that finished is not listed
  and is not woken by a join notice. A message sent to it still wakes it.
- **No extra model call.** The roster is text in a prompt the child was sent
  anyway; a join notice rides the child's next step.

## Conventions ([Z])

The paper's agents cannot talk, so its CFD tells them to fall back to what
needs no convention. ha's children can talk, so saying the choice comes first
and the fallback last. In `TEAM_PROTOCOL`:

> Where your work has to fit a teammate's or your parent's - a name, a format,
> an interface, who does which part - do not build on a choice they would have
> to guess. Take it from the brief or from what the project already does; when
> neither settles it, tell them your choice or ask before you build on it, or
> do what holds whichever way they chose.

The root's delegation guidance (`SUBAGENT_GUIDANCE` in
`crates/harness-cli/src/host/prompting/prompt.rs`) asks for the other half:
what children must agree on - names, format, interface, which files are whose
- goes in every one's brief.

## Proven skills ([E])

Code: `crates/harness-cli/src/host/memory/learned.rs` (`Usage::unproven`,
`Usage::restricted`, `record_outcome`) and `skill_recall.rs`.

ha's learned skills already were most of a Gene: a description, steps, a
ledger, uses, outcomes judged by the review, the assumptions that failed.
What was missing:

| [E] | ha | Why |
|---|---|---|
| Capsule: a concrete case beside the strategy | A review that judges a skill `worked` keeps its quote as a case (the newest 3, each once). The recalled skill carries the newest: `A turn it worked in showed: …` | The steps say what to do; the case shows what done looked like |
| A draft is marked unproven | A skill no check passed after and no review judged to work is recalled with `Nothing has shown it to work yet: it is a draft. Check each step's result as you go.` | ha's skills go live when written; the turn should know which were never seen to work |
| Failure-based restriction | Since it was last written, a skill that failed twice or more and more often than it worked is not a candidate for recall. It stays in the library, where the curator is told to rewrite it | A skill that fits by its words and fails by its record cost a turn each time |
| Probation: standing follows recorded outcomes | The count starts again at every patch (`trial_good`, `trial_bad` in `skill-usage/*.json`), so a rewritten skill is tried afresh | Without it one bad week would bar a skill for ever |

A check that passed or failed after a skill was used counts in the trial too.
The diagnostics' `skill recalled` record carries `cases` and `unproven`.

## Not taken

- **[S]'s training** (team reward, group-relative advantage): ha does not
  train models. Its measured gains are a trained policy's; the prompt alone is
  what [S] calls its starting point.
- **[S]'s unread-count notices and `read_messages`:** ha puts the message
  itself in the receiver's next step.
- **[S]'s race to the first success:** nothing stops the other children when
  one finishes, because nothing but a check says a child succeeded.
- **[Z]'s meta-learned CFD and policy critic:** they need coordination games
  to score a definition on. ha has none.
- **[E]'s network of shared assets (EvoMap), content-addressed storage and
  quarantine before first use:** a skill is still live when written, and
  `/refine --rollback` undoes it.
- **[E]'s matching signals and validation plan as fields:** recall scores the
  skill's own words (BM25).
- **Recall in `ha exec` and for children:** a child is sent the whole learned
  library instead, and `ha exec` none ([strategy and
  runtime](STRATEGY_AND_RUNTIME.en.md) has the recall itself).
- **[T], all of it, for now:** a self-tuning benchmark is a command of its
  own (`ha exec` as the solver, a project's checks as the grader, tasks kept
  near a 50% completion rate). It is proposed, not built.

## Tests

- Unit: `a_child_is_told_its_teammates_and_the_one_that_joins_later`,
  `a_sibling_that_does_not_exist_is_answered_with_the_ones_that_do` and
  `a_teammate_line_is_one_line_and_cut` in `delegation.rs`;
  `a_skill_is_unproven_until_it_works_and_restricted_once_it_keeps_failing` in
  `learned.rs`; `a_skill_that_keeps_failing_is_not_recalled_until_it_is_rewritten`
  and `the_block_says_a_skill_is_unproven_and_carries_the_case_it_worked_in` in
  `skill_recall.rs`.
- Live: `scripts/testing/live-tests/team.sh` (two explorers at once learn of
  each other and exchange what they found by name; a verifier is on nobody's
  team) and section 4b of `strategy.sh` (unproven, the case, held back).
