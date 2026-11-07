# The action bank: calls run without a model step

English | [Tiếng Việt](ACTION_BANK.vi.md)

After each tool result an agent harness calls the model again, even when an
earlier step already fixed the next action: after writing a module, the build
command is known; after a build that succeeds, the test command is known. The
action bank lets the model write such calls down once, with the condition under
which each may run, and the harness then runs a ready one with no model call.

It is HarnessMM's guarded decision reuse (Jiang et al., "HarnessMM: Recursive
Self-Improvement for Harness Token Efficiency", arXiv 2610.32813, sections 4,
C.4 and D). ha implements the execution mechanism - executable decision graphs
(EDG), guarded decision reuse (GDR) and certified callback substitution (CCS) -
and not the paper's offline recursive self-improvement of prompts.

It is **off by default**. The paper's own negative results (Terminal-Bench,
GAIA2) are runs where banks were rarely usable, so authoring them cost more
output than the model calls they saved. Turn it on with `/bank on`,
`"actionBank": true` in settings.json, or `HA_ACTION_BANK=on`; measure it on
your own tasks (section 8).

Code: `crates/harness-tools/src/runtime/bank.rs` (the bank, its guards and
receipts, the selector trait), `TurnDriver::run_turn_inner`, `bank_settled` and
`bank_next` in `crates/harness-tools/src/runtime/turn_driver.rs` (the executor),
`turn_steps` in `crates/harness-runtime/src/lib.rs` and `record_issued_call` in
`crates/harness-runtime/src/control/inbox.rs` (persistence), the Anthropic fold
in `crates/harness-providers/src/protocols/anthropic.rs`, the switch in
`crates/harness-cli/src/host/coordination/action_bank.rs` and the selector in
`crates/harness-cli/src/host/integrations/jev.rs`.

## 1. The paper's contract, and ha's mapping

| HarnessMM | ha |
|---|---|
| A slow step emits the current action and a bounded acyclic EDG of future decisions `(action, literal args, predecessor, guard)` | The model calls `action_bank` in the same response as its other calls. Each node has an `id`, `after` (`root` = the other calls of this response, or another node's id), `when` (the guard), `tool` and `arguments`. At most 8 nodes |
| Literal closure, checked by the schema | Each node's arguments must pass its tool's schema when the bank is authored, as a call's would. A node may name only a tool in the turn's tool list, and not `action_bank`, `tool_schema`, `use_tool` or `ask_user` |
| Acyclic, predecessors exist | Checked when authored. An invalid bank is refused with its reason as the call's result, and nothing is kept |
| GDR: after a receipt, eligible = predecessor completed, guard true, args closed | After a call settles, the nodes whose `after` is that call (C.4: "entries whose predecessor was that command") are checked against its result. Guards read only the observed result |
| The graph persists across tool transitions | The bank lives for the whole turn. A dispatched node's result makes its own children ready |
| CCS: one eligible node runs through the native interface with no slow call | One ready node runs through the same per-call pipeline as a model call: use_tool unwrap, malformed-call check, regulator, learned rules, schema check, policy, jev command guard, hooks, approval, intent and receipt. No model call is made |
| Several eligible: a typed selector returns an id or abstains | The jev `bank` decision (`choice` over the node ids plus an abstention). Without jev, below its threshold, on its time-out (10 s) or abstaining: the model is called |
| Empty set, abstention, unsupported state: the slow actor | No ready node, an abstention, an unclear result, steering, a loop warning, a regulation note, no room under `max_tool_calls`: the model is called as usual |
| A new graph replaces the unresolved remainder; a node runs once | A new `action_bank` replaces the nodes not run yet; a node is resolved (run or dropped) at most once; the selector returns only an id |
| A certificate per bypass | A `bank/dispatched` diagnostic record per bypass (section 5) |
| Complete-task accounting | The turn's provider usage already holds the authoring and every fallback; the bank adds its own tally (section 6) |

Not taken from the paper: the RSI loop that edits prompts, and the paper's
"single-encoding" fast request (ha's selector is Jev, which reads its own
compact state; the model's full transcript stays with the model).

## 2. Guards

A guard reads the public result of the call it follows:

| Guard | Holds when |
|---|---|
| `ok` | the call ran and succeeded: a process exited 0, a file tool returned |
| `failed` | the call ran and failed: a non-zero exit or a time-out |
| `exit_code: N` | a process exited with N |
| `output_contains: "text"` | what the model is shown of the result contains the text |
| `output_lacks: "text"` | it does not |
| `changed` | a write, edit or patch changed its file (the hashes before and after differ) |

A call that was refused, denied, malformed, cancelled or of unknown outcome is
*unclear*, and no guard holds on it: the model reads it. The root's calls are
read as one result: unclear if any is, failed if any failed, the last exit
code, all their texts. A bank authored alone in its response has an empty root,
which is `ok`: its first node then runs at once, where the response's own call would have run, and is counted as `immediate`, not as a saved step.

## 3. How a bypassed call appears to the model

The model's next step sees each bypassed call as if it had made it: an
assistant message with the call, then its result. The result's first line says
it was run by the harness:

```
[action bank node test: run by the harness without a model step]
```

The assistant message is marked `harness_issued` (`ProviderMessage`). It
carries no reasoning, since no model response holds it. Per protocol:

- **Chat Completions** and **Responses**: sent as the call and its result
  (`tool_calls` + `tool`; `function_call` + `function_call_output` with no
  reasoning item, as a replayed earlier turn's calls are).
- **Anthropic with thinking**: the API refuses a last assistant message that
  does not begin with a signed thinking block. The harness-issued call and its
  result are folded into one text block after the results before them, so the
  last assistant message stays the model's own:
  `[call from your action bank, run by the harness without a model step] run_process {...}` and the result.
  Without thinking it is sent as a `tool_use` and its `tool_result`.

Unit tests build each protocol's request body for such a transcript.

## 4. Persistence

Later turns and a resume rebuild a conversation from the stored provider
attempts (`turn_steps`). No attempt holds a harness-issued call, so its receipt
would be a result with no call. Each bypass is therefore recorded as an applied
run command with `{"host": true, "issued": {call_id, name, arguments, node}}`
(it carries no `text`, so it is never read as steering), and `turn_steps` puts
it back between the steps it came between, with its receipt's result and the
note line. The answer to an `action_bank` call, which ran no tool, is recorded
as `{"host": true, "answered": {call_id, text}}`, so the replay gives it and
not "this call did not run".

## 5. Certificates

Each bypass writes `bank/dispatched` with: `graph` (a hash of the authored
nodes), `node`, `predecessor` and `predecessor_receipt` (a hash of the result
the guard read), `tool` and `args_hash` (the exact arguments run),
`tool_receipt` (the durable receipt ids) and `tool_receipt_hash`, `status`,
`replaced_step` (the model step it stands in for) and `model_steps` (the
turn's model calls so far, which did not move). Other records: `bank/authored`
(graph, nodes, edges, `author_estimate`: the authoring estimate in tokens; the diagnostics withhold any field named like a token), `bank/refused` (reason),
`bank/abstained` (candidates, whether a selector was asked and its input
tokens), `bank/discarded` (nodes and reason: `guard_false`, `not_chosen`,
`replaced`, `deferred`, `turn_end`) and `bank/turn` (the turn's tally).
`/bug-report` has an **Action bank** section.

## 6. Accounting

`TurnOutcome.bank` counts banks authored and refused, nodes, calls dispatched
(= model steps saved), abstentions, dropped nodes, selector calls and their
input tokens, and the authoring cost. The provider reports one response's
output tokens, not the bank's share, so the authoring cost is an **estimate**:
the bank call's argument bytes / 4. The complete cost of a task is the run's
provider usage, which already includes the authoring step and every fallback.

- `ha exec --json` has a `bank` object: `enabled`, `authored`, `refused`,
  `nodes`, `dispatched`, `model_steps_saved`, `immediate`, `abstained`, `discarded`,
  `selector_calls`, `selector_input_tokens`, `author_tokens_estimate`.
- `/context` has a `Bank:` line while the bank is on or did anything.

## 7. The switch

`/bank on|off|status` saves `actionBank` in settings.json; `HA_ACTION_BANK=on|off`
overrides it. It applies from the next turn. Children run with their parent's
setting. The jev `bank` decision (`/jev bank on|off`, threshold
`jev.thresholds.bank`, default 0.95) is the selector.

## 8. Measured result

See section 8 of the live suite report (`scripts/testing/live-tests/bank.sh`,
`$RX/bank-ab.md`).
