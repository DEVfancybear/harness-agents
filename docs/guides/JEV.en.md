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
is a harness whose every per-turn decision is a typed question to Jev. ha asks Jev at five of
those points.

Each decision **only narrows** what the harness does. Jev never grants a permission that the
policy did not grant, and never removes work the user asked for. Each decision also **fails
open**: if Jev is unavailable, slow or unsure, ha does what it does without the plugin.

| # | Decision point | Question to Jev | What ha does with a confident answer | Switch |
|---|---|---|---|---|
| 1 | **Permissions.** Should this command run? | `choice` allow / ask / deny. The state is the command, plus the contents of any local script it runs. | `deny` refuses the command and gives the model Jev's reason. This happens in every mode, including full-auto, headless and children. `ask` and `allow` change nothing. | `/jev commands` |
| 2 | **Subgoals.** Is this subtask already done or in flight? | A `noul` per sibling: is it the same work? | ha reuses that child (its answer, or its run in flight) instead of starting a duplicate. | `/jev subgoals` |
| 3 | **Context.** How visible should each earlier tool result be for this request? | `choice` hide / short / full, asked when a turn's results overflow their budget. | Results are shortened in that order: `hide`, then `short` (the head kept), then the oldest of the rest. `full` results are cut last. | `/jev context` |
| 4 | **Conditional instructions.** Does this `AGENTS.md` section apply to this request? | A `noul` per section, for instruction files with several sections. | A section Jev gives at most a 10% chance of applying is left out of the turn. The turn names what was left out, so the model can read the file. The preamble is always kept. | `/jev instructions` |
| 5 | **Security-aware routing.** How sensitive is what this subtask will touch? | `choice` restricted (secrets, credentials, env, infrastructure or deploy config) / standard / open. | A `restricted` subtask runs on the session's own model, not the cheaper subagent model. | `/jev routing` |

The decisions are on while the plugin is on, unless switched off one by one. The threshold is
`jev.threshold`, 0.95 by default. Every decision is recorded in the diagnostics (`jev`
records), so `/bug-report` shows what Jev decided.

Two points from the notes are not in this release:
- **Cache reuse.** Rebuilding against reusing the provider's prefix cache depends on each
  provider's caching.
- **Tiered tool disclosure.** Snippets first and schemas on demand would change how tools are
  declared.

## Settings

`settings.json`, beside the user config:

```json
{
  "jev": {
    "enabled": true,
    "threshold": 0.95,
    "model": "jev-latest",
    "decisions": {"commands": true, "subgoals": true, "context": true, "instructions": true, "routing": true}
  }
}
```
