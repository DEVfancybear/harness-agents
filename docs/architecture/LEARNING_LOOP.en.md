# The learning loop

English | [Tiếng Việt](LEARNING_LOOP.vi.md)

What the user corrected and what the agent assumed wrongly is gathered across
conversations. A lesson that recurs becomes a standing rule by itself. This is
the learning loop of [Agnostic-AI](https://github.com/ucsandman/Agnostic-AI)
(harvest, distill, promote; read at commit `aab09ff`), built into ha's
automatic review.

Code: `crates/harness-cli/src/host/memory/lessons.rs`, called from the review in
`memory/refine.rs`; the CLI is `commands/learning_cli.rs`.

## 1. What changed from Agnostic-AI

| Agnostic-AI | ha | Why |
|---|---|---|
| `correction-tracker` buckets prompts with English regexes; `errorlog` buckets deviation blocks the same way | The automatic review reads the conversation and reports each correction (`kind: correction`) or wrong assumption (`kind: deviation`) with a verbatim quote | ha judges text by what it says, not by word lists, and a Vietnamese correction is a correction |
| A candidate is a hash of its normalized words | The review names the open lesson a signal repeats or contradicts | Two wordings of one lesson, or two languages, are one lesson |
| Rule at 3+ sighting days; keyword heuristics for contradiction | markdown-agent-memory's gate: weight 3 from 2+ conversations, a signal older than 30 days counting half; contrary signals named by the review | The gate the memory policy states |
| A proposal file; a human approves in the dashboard | Promoted by itself, within ceilings; `ha learning retire` undoes it | The maintainer's choice |
| Approved rules appended to `global-rules.md` and ported | A global lesson becomes a `constraints` entry of the global memory; a project's lesson goes into its markdown store, or its project entry | Where ha already puts what every turn must see |
| Nightly jobs in Windows Task Scheduler | The gate runs after each review that recorded a signal, and on `ha learning distill` | Nothing to install; the review already runs |
| The ladder's Tier 1: a repo fact, from one sighting | A deviation about one project is a fact, kept at once (§5) | It is observed, not inferred: the policy's gate is for inferences |
| "A rule works iff its bucket stops accruing records" | Signals after a promotion are counted; a rule corrected as often as it took to become one is named still corrected (§5) | The same measure, on each rule |
| The nightly meditation over the error log, weekly synthesis | A meditation once a day after a review: merge, reword, retire, synthesis (§5) | The review already runs; one pass a day |
| `errorlog.html`: what goes wrong repeatedly | `ha learning report [--html FILE]` (§6) | |

Not taken: the few-shot example fixtures (nothing reads them), the five-trait
ceiling and the Trait tier (ha has no traits file), the length rule that sent a
rule over 180 characters to examples (a rule is cut to 400 characters instead),
the human-side error log, and the harvest from other agents' folders.

## 2. Signals

The automatic review's prompt carries a `<lessons>` part: what to report, and up
to 30 open lessons of every project and this project, newest first, each with
its state and standing. The review answers with a `lessons` array:

```json
{"lesson": "les_…, or left out for a new one",
 "text": "when <situation>, <what turned out right>",
 "kind": "correction | deviation | resolution | preference",
 "domain": "code-style | testing | git | debugging | workflow | file-patterns | other",
 "scope": "global | project",
 "against": false,
 "evidence": "a verbatim quote from the conversation"}
```

The host checks each signal:

- The evidence must occur in the turns not reviewed yet, whitespace and case
  aside, and be a phrase (12 characters or more): a word or two is found
  anywhere.
- It must be said by whom its kind needs: a correction or a preference in the
  user's words; a wrong assumption or an error's fix in a tool result or the
  user's words. The agent's own text is never evidence, and neither is what it
  read back from its memory (`memory`, `knowledge`, `history_*` results).
- No secret, in the text or the quote.
- One conversation gives a lesson at most 2 of its weight, so the gate's 3
  always needs a second conversation.
- A signal that names a lesson must name one that exists, is not retired, and
  is this project's when it is a project's.
- A new lesson needs its text, a kind, and a project when its scope is `project`.
  The text is data, never an instruction: "when X, Y was right".
- `against: true` must name the lesson it contradicts.

Each signal is recorded with the refinement (`appliedEdits`, kind `lesson`),
applied or refused with the reason. A rollback of the refinement leaves the
signals alone: they are what conversations showed.

The review gate is told that a correction or a wrong assumption is worth a
review, so a conversation whose only lesson is one still reaches the refiner.

### Kept once: the review's own memory

The review already saves a rule the user states outright ("from now on, ...")
as a global memory entry, at once, as the memory policy asks. The loop must not
write that rule a second time:

- The review reports such a lesson with `saved_as`, the id of the global memory
  edit it makes for it. Only an id among this review's global memory edits is
  believed (otherwise the record says `saved_as_ignored`). The lesson is then
  promoted at once as that entry (placement `covered`). Later signals add to its
  evidence and write nothing.
- If the entry is not written after all - the local or global part failed, or
  the memory refused the edit - the lesson becomes a candidate again.
- Before the gate writes a rule, it asks the memory whether an entry already
  says it, by the measures the memory tool refuses a duplicate with (60 %
  alike, or 85 % of it held in an entry). If one does, the lesson is promoted as
  that entry instead of writing another.
- The gate runs after the review's global part is written, so a rule the review
  saved in the same pass is found.

When a project keeps a markdown store, the review is also told not to save
facts about that project as memory edits: they belong in the store, which the
agent writes. A lesson about the project still goes in `lessons`.

The prompt asks the review to leave out a learned skill's failure, which its
skill outcome already records.

## 3. The gate

A lesson's standing at a given time:

- **weight**: one per independent supporting signal, half for one older than 30
  days. Two reviews reading the same quote in the same conversation count once.
- **conversations**: the conversations the supporting signals came from.
- **contrary**: the conversations that contradicted it.

| Condition | Outcome |
|---|---|
| contrary ≥ 2 | Retired; a promoted rule is undone |
| weight ≥ 3, conversations ≥ 2, contrary = 0 | Promoted |
| weight ≥ 3, conversations ≥ 2, contrary = 1 | Held: a conversation contradicted it |
| More than 2 ready in one run | The rest held for the next run |
| 8 rules standing in its scope (every project, or one project) | Held: retire one first |

## 4. Where a rule goes

- **Every project:** a global memory entry `learned_<id>` at
  `constraints/learned`, which the digest shows in every turn ahead of the
  ranked entries. Its content is the lesson and its trail (signals,
  conversations, days, the lesson id). The store records the change as made by
  `learning` with cause `learn`.
- **One project with a markdown store** (`memory-lint.json` at the root the
  lesson was seen in, [MARKDOWN_MEMORY.en.md](MARKDOWN_MEMORY.en.md)):
  - `memory/decisions/<date>-learned-<slug>.md`, whose lines are tagged
    `[inferred]` (the rule, why, the review trigger) and `[observed]` (each
    quote);
  - a line under the index's `## Current Decisions`, which replaces its
    `(none yet)` placeholder. The index is in every turn, so the rule is too.

  The store passes `ha memory lint`.
- **One project without a store:** a global memory entry like the first, marked
  as that project's, so only its turns show it.

## 5. Facts, what a rule did, and the meditation

**Facts.** A deviation about one project - a file, a command or a check that was
not what the agent assumed - is what the project showed. That is observed, not
inferred, so it needs no recurrence: it is kept as a fact (tier `fact`) on its
first sighting.
- With a markdown store, it becomes a `- [observed] <text> (<date>, <lesson>)`
  line under `## Facts` of `memory/projects/<project>.md`, and the project gets a
  row in the index when it has none.
- Without a store, it becomes a ranked memory entry of that project at
  `facts/learned`: recalled when the work touches it, not put in every turn.
- A fact the memory already holds is that entry.
- A `resolution` about one project (an error a tool result showed, and the fix
  that made it go away) is kept the same way.
- Two contrary conversations retire it, striking its line.
- Facts count toward no rule ceiling.

The review is told to report a project deviation only when a tool result or a
file shows it. Corrections, and lessons about every project, are rules and wait
for the gate.

**What a rule did.** The signals after a promotion are the rule's record: a rule
that works stops being corrected. A rule corrected after its promotion as often
as it took to become one (weight 3 from 2 conversations) is named `STILL
CORRECTED`:
- `list`, `show` and the report say it;
- the review's lesson list says it;
- the meditation rewords it or retires it.

**The meditation.** At most once a day, after a review, when two or more lessons
are open, a model pass reads up to 60 open lessons, each with its case and its
newest quotes. It answers with:
- `merge`: lessons that are one lesson in other words or another language, of
  one scope and tier. The signals move to one lesson, and the others retire as
  merged.
- `reword`: a rule still corrected, or a lesson that reads as an instruction.
  - A rule the loop wrote gets a new version of its memory entry, or its decision
    line is superseded: struck through, with the new line beside it.
  - A fact's line is superseded too.
  - A `covered` entry is the user's own words and is not reworded.
- `retire`: a lesson its later quotes contradict.
- `synthesis`: two or three sentences on what recurs. It is kept with the
  changes (the last 30 meditations), and the report shows the newest.

- `generalize` and `evolve`: see §6.

The gate runs again after it, since merged signals may pass. The conversation is
told each change: `learning loop: meditation: merged lesson …`.

## 6. From ECC's continuous-learning-v2

[ECC](https://github.com/affaan-m/ECC)'s continuous-learning-v2 (read at
`25278e7`) keeps *instincts*: atomic "when X, do Y" with a confidence, a domain
and a project or global scope. ha's lessons already were that, so v2's features
went into them, not beside them.

| ECC v2 | ha | Why |
|---|---|---|
| A Haiku observer reads every tool call in the background, every 5 minutes or 20 observations | The automatic review reads the conversation, tool results included, after its turns and when a session goes quiet | One reader of the conversation, not two |
| Four patterns: corrections, error resolutions, repeated workflows, tool preferences | Kinds `correction`, `deviation`, `resolution` (an error and the fix that made it go away), `preference` (a tool or way of working the user chose); a repeated workflow is a learned skill's | ECC's observer cannot see corrections at all (it records no prompt); ha's review does |
| `domain` per instinct | `domain` per lesson, one of code-style, testing, git, debugging, workflow, file-patterns, other | |
| Confidence 0.3 / 0.5 / 0.7 / 0.85 by count; +0.05 / -0.1 / decay are documented, not implemented | `confidence` from the case: weight under 3 is 0.3, 3-5 is 0.5, 6-10 is 0.7, 11+ is 0.85, each contradicting conversation 0.1 off; old signals count half | The gate still promotes; the confidence says how much the case has grown |
| SessionStart injects instincts at 0.7+, top 6 | A promoted rule is a `constraints` entry, in every turn | |
| `promote <id>`; auto-promote at 2+ projects and average 0.8 | `ha learning promote <id>`; the meditation's `generalize` makes one lesson for every project of the same lesson in two or more projects, which then passes the gate as any other | A model judges "the same lesson", not keyword overlap; the gate, not an average, decides |
| `/evolve` clusters instincts by trigger keywords into skills, commands and agents written where nothing loads them | The meditation's `evolve` gathers two or more promoted rules of one workflow into a learned skill, through the learned-skill promoter and its checks, into the library the agent loads | The skill is live, and holds the rules: each rule's own entry or decision line is taken back (placement `skill`), so the workflow is kept in one place. Its rules must hold where the skill does: all for every project, or all this project's |
| `export` / `import` (file or https), `status` | `ha learning export`, `import <file>`, `status` | Files only: an import is the user's |

A rule the review writes into a learned skill (`skillFiles`) is reported with
`saved_as: "skill:<name>"`: the lesson is that skill (placement `skill`), and
the loop writes no rule that says it again.

**Status** lists the open lessons by domain, a bar of ten for the confidence.
**Export** writes a JSON document (`kind: ha-lessons`) of the open lessons at or
above `--min-confidence`, by `--scope global|project|all`, each with its
signals; the placement stays behind. **Import** reads one:
- a lesson already here (the same id, or the same text and scope) gains the
  signals it lacks; a new one is a candidate, and the gate runs;
- an imported signal's conversation is `import:<id>`, so a second import of the
  same file adds nothing;
- a project lesson becomes the current directory's project's;
- a text or quote with a key-shaped value is skipped;
- `--dry-run` says what would change.

**Promote** makes a project's rule one for every project: its project rule is
taken back, and the gate writes it where a global rule goes. A fact stays with
its project.

## 7. `ha learning`

| Command | Does |
|---|---|
| `ha learning list [--all] [--json]` | Lessons with state, tier, scope, kind, weight, conversations and what a rule did since its promotion (retired ones with `--all`) |
| `ha learning show <id>` | Its rule's placement, every signal with its quote, and its history |
| `ha learning distill` | Runs the gate now; prints what it promoted, retired and held |
| `ha learning retire <id> [--reason …]` | Undoes the rule: deletes the memory entry, or strikes the decision's rule line and the index line (or the fact's line) through, dated (supersession, never deletion). A `covered` lesson's entry is the user's words and stays; retiring it, or two contrary conversations, says so |
| `ha learning report [--all] [--html FILE]` | What recurs: rules still corrected, rules, candidates by weight, facts, the counts, and the last meditation's synthesis; with `--html`, also as a standalone page. It ends with the self-evaluations of the last 30 days ([SELF_EVALUATION.en.md](SELF_EVALUATION.en.md)) |
| `ha learning status` | The open lessons by domain, each with its confidence |
| `ha learning export [-o FILE] [--min-confidence N] [--scope global\|project\|all]` | The open lessons as a JSON document another machine imports |
| `ha learning import <FILE> [--dry-run] [--min-confidence N] [--scope …]` | Merge an export: new lessons as candidates, known ones gain the signals they lack; then the gate |
| `ha learning promote <id>` | A project's rule becomes one for every project; then the gate |

The running conversation is told what a review's gate changed:
`learning loop: learned rule les_…: …`, `learned fact …`, `retired lesson …`.

## 8. Storage

`<data>/harness/lessons.json`, written whole under `lessons.lock`. Each lesson
holds:
- its id, text, kind, domain and tier;
- its project (`key`, `name`, `root`);
- its state (`candidate`, `promoted`, `retired`) and when it was promoted;
- its signals (time, conversation, `against`, quote);
- its placement: a `memory` key, a `covered` key, a `markdown` root and file, a
  `markdown_fact` root and file, or a `skill` name;
- its events.

The file also keeps the meditations. A lesson keeps its newest 50 signals and
100 events. A file that cannot be parsed is reported and never overwritten.

**A lesson follows its entry.** Each gate run first checks that a promoted
lesson's memory entry is still there:
- an entry renamed is followed;
- a `covered` entry Deep Dream merged away is found again by what it says, by
  the duplicate refusal's measures; with none, the lesson is a candidate again;
- a rule the loop wrote that the user deleted retires its lesson.

Deep Dream does not see the loop's own entries (`source: learning`): the
meditation keeps them.
`lessons_meditated` beside it records the last meditation.

## 9. Tests

- Unit tests in `lessons.rs` cover:
  - the gate's counting, the quote check, the ceilings and contradiction;
  - every placement and its retirement, facts with and without a store;
  - a rule still corrected after its promotion, and the report and its page;
  - a meditation's merge, reword (a new version of the entry; a user's entry
    left) and retirement;
  - a broken file.

  A test in `refine.rs` runs three reviews through `auto_refine`: one
  conversation's two signals stay a candidate, and the second conversation's
  signal makes the `constraints` entry.
- The live suite `scripts/testing/live-tests/learning.sh` drives `ha exec` with a
  real model and its review after every turn:
  - a correction made in the moment ("too long, 1-2 sentences") becomes a lesson
    with its quote, and one conversation writes no rule;
  - a second conversation makes it a rule, kept as one entry: the loop's, or
    the review's own when it saved the preference;
  - the next request carries it;
  - a rule the user states outright gets no second entry;
  - retiring undoes only what the loop wrote;
  - a project's lesson becomes a decision of its markdown store, passing the
    lint, and retiring strikes it through.
