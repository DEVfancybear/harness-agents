# The Jev workflow in ha

[Tiếng Việt](JEV.vi.md)

ha pairs its reasoning model with **Jev**, TypeSafe's System One model, through the **jev
plugin**. The plugin is **off by default**: `/jev on` turns it on, `/jev off` turns it off, and
`/jev status` shows its state. Each harness decision also has its own switch (`/jev <decision>
on|off`).

The plugin needs a TypeSafe key. Save one with `/login typesafe` (from
[console.typesafe.ai](https://console.typesafe.ai)), or set `TYPESAFE_API_KEY`. Calls go to
TypeSafe's API, `POST https://api.typesafe.ai/v1/systemone`.

The workflow brings together four references:

| Reference | What it gives ha |
|---|---|
| [TypeSafe API](https://docs.typesafe.ai/api) and its [agent skill](https://github.com/typesafe-ai/skills) (MIT) | The primitives, and how to design and compose judgments |
| [jev-fraud](https://github.com/Nutlope/jev-fraud) | Escalation: Jev decides every item, and items below the threshold go to the reasoning model |
| ["Opus 5.5 + Jev" (AI Edge)](https://newsletter.aiedgehq.co/p/opus-5-5-jev-best-ai-combo-possible) | The agent workflow: map the workflow, pilot, tune, then make it reusable |
| *Jev Engineering for Coding Agents* (a synthesis of Diogo Almeida's design notes, [jev-engineering-zh](https://github.com/yibie/jev-engineering-zh)) | Jev as the decision layer beside the harness, asked at the per-turn decision points |

## What Jev is, and what it is not

Jev reads a **state** and answers named, typed **questions**. Each answer comes with
probabilities, takes about 100 ms, and costs $0.042 per million input tokens (output is free).
There are three kinds of question:

| Primitive | Answers | Confidence |
|---|---|---|
| `noul` | The probability that a condition holds | None of its own. ha uses \|2p − 1\| |
| `choice` | One option from `criteria` (an option → meaning map, 1 to 255 options) | `confidence`, plus a probability for each option |
| `score` | A position on ordered `criteria` levels (2 to 10) | `confidence`, plus a probability for each level |

Jev does not write text, explain itself, count, do arithmetic or handle dates. Keep those in
code. Confidence measures how concentrated an answer is, not whether it is true. Typed output
guarantees the shape of an answer, not its correctness.

## Roles

- **The reasoning model** (the session's model) does research, comparison, reasoning and
  drafting. It also reviews whatever Jev is not sure of.
- **Jev** makes the repeated, narrow judgments, both in the agent's own workflows (layer 1)
  and at the harness's decision points (layer 2).
- **Tools and code** take the actions, apply the hard rules and do the calculations.

## Layer 1: the agent's workflow

This layer is the `jev` skill, the `jev` tool, and the `jev` import in the Python kernel.

1. **Map the workflow, without building anything yet.** Write down its steps and find the
   repeated semantic decisions: the same judgment made on every email, lead, ticket, claim or
   file. For each candidate, record:
   - the input, which is Jev's `state`;
   - the decision, as one narrow question;
   - the primitive;
   - the allowed answers;
   - the uncertainty path;
   - what stays with the reasoning model or with code.

   Rank the candidates by usefulness and simplicity. Recommend one narrow, low-risk candidate
   to try first, and show the plan.
2. **Pilot it on real material.** Run the `jev` tool on 10 to 50 real items. Report:
   - how many were decided and how many came back for review;
   - what Jev got wrong;
   - the time taken and the cost.
3. **Tune it.**
   - Split a question that mixes two judgments.
   - Make the criteria concrete, with contrasts and exclusions.
   - Put missing context into `state`.
   - Set the threshold from the cost of a wrong answer.
   - Ask independent questions about the same state in one call.
4. **Make it reusable.** Write a script with `jev.decide` and `jev.batch`, recording the
   questions, the threshold and the review path. Keep the raw answers, so that changing a
   weight or a filter later needs no new calls.

**Escalation** works as in jev-fraud. Jev judges every item, up to 20 at once. An answer whose
confidence reaches the threshold (0.95 by default, or `threshold` in the call) is **decided**.
Every other answer comes back **for review**, with Jev's leaning shown, and the reasoning model
judges it from the item itself.

**Rules:**
- Nothing leaves the machine on a Jev decision without the user's approval: nothing is sent,
  published, bought or deleted.
- Measure accuracy on the user's own data before trusting a threshold.
- The state is untrusted data.
- Never invent evidence for a claim Jev could not support. Send the claim back for more
  research instead.

## Layer 2: Jev at the harness's decision points

The design notes ask how a coding agent would be built if models had no KV cache. Their answer
is a harness whose every per-turn decision is a typed question to Jev. ha asks Jev at nine of
those points.

Each decision **only narrows** what the harness does. Jev never grants a permission that the
policy did not grant, and never removes work the user asked for. Each decision also **fails
open**: if Jev is unavailable, slow (8 seconds) or unsure, ha does what it does without the
plugin. And none of them rewrites a prefix the provider has cached.

| # | Decision point | Question to Jev | What ha does with a confident answer | Switch |
|---|---|---|---|---|
| 1 | **Permissions.** Should this command run? | `choice` allow / ask / deny. The state is the command, plus the contents of any local script it runs. | `deny` refuses the command and gives the model Jev's reason. This happens in every mode, including full-auto, headless and children. `ask` and `allow` change nothing. | `/jev commands` |
| 2 | **Subgoals.** Is this subtask already done or in flight? | A `noul` per sibling: is it the same work? | ha reuses that child (its answer, or its run in flight) instead of starting a duplicate. | `/jev subgoals` |
| 3 | **Context.** How visible should each earlier tool result be for this request? | `choice` hide / short / long / full, asked when a turn's results overflow their budget. | Results are shortened in that order: `hide`, then `short` (the start kept), then the oldest unranked, then `long` (the start and the end kept, as a log's command and its failure). `full` results are cut last. | `/jev context` |
| 4 | **Conditional instructions.** Does this `AGENTS.md` section apply to this request? | A `noul` per section, for instruction files with several sections. | A section Jev gives at most a 10% chance of applying is left out of the turn. The turn names what was left out, so the model can read the file. The preamble is always kept. | `/jev instructions` |
| 5 | **Security-aware routing.** How sensitive is what this subtask will touch? | `choice` restricted (secrets, credentials, env, infrastructure or deploy config) / standard / open. | A `restricted` subtask runs on the session's own model, not the cheaper subagent model. | `/jev routing` |
| 6 | **Large output.** Which parts of a long tool result does this request need? | A `noul` per part, for a result of 16 KB or more from a command, a search, a whole-file read or a fetched page, cut into parts of about 2,000 characters at line ends. | A part Jev gives at most a 10% chance of being needed is left out, when a quarter or more of the result goes. Each run of left-out parts becomes a note naming its lines and how to read them again. The first and the last part always stay, and the user still sees all of it. | `/jev output` |
| 7 | **Completion.** Does the answer say an unchecked change works? | A `noul`, asked only when the turn changed files and ran no command after its last change (a code gate). | The model is sent back once, to run the project's check on what changed or to say plainly that the change is not verified. | `/jev completion` |
| 8 | **Tools.** Which of the listed tools fit this request? | When MCP or extension tools are deferred (see below): a `choice` over the roster for the best three, then a `noul` for each. | The tools Jev leans yes on are named in the turn's input; one it is sure of comes with its arguments, which saves a `tool_schema` call. | `/jev tools` |
| 9 | **Cache.** Reuse the cached prefix, or start fresh? | A `noul`: does the new request continue the earlier work? Asked only when the provider's cache has expired and the earlier conversation is 20k tokens or more, since reusing it then costs its full price anyway. | The earlier turns are set aside behind one note that names each request and points at the history tools. The fresh start is kept and applied to every later turn, so the new prefix stays cached. A warm cache is never rebuilt. | `/jev cache` |

Decisions 3, 6 and 7 judge children's turns too, as the root's are judged.

The decisions are on while the plugin is on, unless switched off one by one. A decision acts
at its own threshold, `jev.thresholds.<decision>`, set from the cost of its wrong answer as
TypeSafe advises. The default is `jev.threshold` (0.95), except for the three mild ones -
`instructions`, `output` and `completion` - which act at 0.80, a clear no (p ≤ 0.10) or a clear
yes. `/jev status` lists each decision with its threshold.

Every question asked of Jev is recorded in the diagnostics with its decision, the Jev version
that answered, its input tokens and how long it took, and every outcome record names its
decision and whether it changed what ha did. `/bug-report` has a **Jev** section:

- each decision: how often Jev was asked, how often it acted, its failures and time-outs, the
  median answer time and the versions that answered;
- the agent's own `jev` calls;
- the newest 30 Jev records, with what each decided (the command judged, the sections and lines
  left out, the tools suggested).

The bug report's model calls also give the share of input read from the cache, and how many
conversations were carried on or started fresh.

### Tiered tool disclosure

The design notes' three tiers - a snippet per tool, the schema on demand, the docs for a one-off
need - apply to the tools of MCP servers and extensions, which can be many and long. When their
schemas take 12 KB or more (`tools.tiered` in settings.json: `auto`, the default; `on`; `off`;
or `HA_TOOL_TIERS`), they leave the tool list:

- the system prompt lists each one on one line (its name and the first sentence of its
  description), sorted;
- `tool_schema` shows a tool's whole description and arguments;
- `use_tool` calls it. The call becomes the tool's own call: its arguments are checked against
  its schema, and it is gated and approved like any other.

The tool list sent to the provider does not change with what the model looks at, so the cached
prefix stays whole. The core tools and ha's own always stay in the list. With the Python REPL on,
MCP servers are reached through its `mcp` object instead, which is on demand already; the tiers
apply to native MCP tools (`HA_REPL=off`, or no Python) and to extension tools. This works without the
jev plugin; with it, decision 8 points the model at the tools that fit.

### The cache decision

ha keeps the prefix the provider caches stable: the system prompt holds what stays for the
session, the date and Git facts go with each turn's input (in `ha exec` too), the tool list is
sorted, and streamed OpenAI calls carry `prompt_cache_key`. Rebuilding that prefix is considered
only when it is free: once the provider's cache has expired since the conversation's last model
call. The lifetime is 5 minutes on Anthropic (an hour with `HA_CACHE_RETENTION=long`), 6 hours on
DeepSeek, an hour on OpenAI and others (a day on OpenAI's API with `HA_CACHE_RETENTION=long`),
taken long when unsure, because a cache thought expired while it holds is the one guess that
costs. `HA_CACHE_LIFETIME` (seconds) sets it for a provider, and `HA_FRESH_START_MIN_TOKENS` the
size below which a conversation is always carried on.

## Settings

`settings.json`, beside the user config:

```json
{
  "jev": {
    "enabled": true,
    "threshold": 0.95,
    "model": "jev-latest",
    "decisions": {"commands": true, "subgoals": true, "context": true, "instructions": true, "routing": true, "output": true, "completion": true, "tools": true, "cache": true},
    "thresholds": {"commands": 0.97}
  },
  "tools": {"tiered": "auto"}
}
```

`jev-latest` moves to new versions; once thresholds are tuned, pin the version that answered
(`/bug-report` and the `jev` tool name it), as TypeSafe advises.
