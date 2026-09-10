# P2 — Agent runtime, context and compaction

English | [Tiếng Việt](P2_RUNTIME_CONTEXT.vi.md)

Implementation runbook; status: **not started**. Estimate: 7–10 person-days. All target files, Rust tests and `ha` commands below are future outputs unless already present in the checkout. This document alone is not completion evidence.

## 1. Outcome and entry gate

Require accepted [P1](P1_KERNEL_STORAGE.en.md). Deliver a single-agent model loop with durable request history, context inspection, repeated compaction and resume. It can use deterministic fixture tools; production filesystem/process tools belong to P3.

Read the [handbook](README.en.md), [plan](../RUST_HARNESS_PLAN.en.md), [plugin contract](../PLUGIN_ARCHITECTURE.en.md), [memory contract](../MEMORY_AND_CONTINUITY.en.md) and [acceptance map](ACCEPTANCE_MAP.en.md). Inspect actual predecessor evidence before coding.

## 2. Owned scope and target files

Own `crates/harness-runtime/`, `crates/harness-providers/`, context/projection modules in `harness-session`, packet/composition migrations, `crates/harness-cli/tests/phase_p2.rs`, mock HTTP/SSE fixtures and CLI `run`, `resume`, `continue`, `context inspect`, `session replay --offline`. Keep reusable-memory service optional through an empty/test implementation.

## 3. Contracts to settle before implementation

Specify normalized provider messages/tool-call IDs, stream finalization versus attempts, cancellation, model capabilities and provider-private fields. Define input/output token reservations, mandatory-context admission, compaction CAS and immutable request packet format. Choose supported provider capabilities explicitly; recheck current official DeepSeek API documentation when implementing the adapter rather than hard-coding an assumed model window.

## 4. Ordered work items

### 4.1. P2-S01 — Specify the actor and provider boundary

Depends on: Accepted P1.

Define agent states and turn/step transitions, durable inbox claim/consumption, bounded retries and cancellation. Separate the application service API from CLI presentation. Write model capability and normalized request/stream contracts; record exact config/provenance.

Evidence: No CLI-only loop or hidden provider request mutation; event state transitions have invalid-transition tests.

### 4.2. P2-S02 — Build mock provider and DeepSeek adapter

Depends on: P2-S01.

Implement deterministic mock responses and local HTTP/SSE parsing fixtures, then a DeepSeek HTTP adapter using host-resolved credentials. Test chunk splits, malformed/incomplete tool arguments, cancellation, transport errors and capability mismatch. Retain provider-specific fields only through explicit adapter support.

Evidence: Incomplete tool calls never execute; no key is persisted; mock tests need no external network.

### 4.3. P2-S03 — Build mandatory context admission

Depends on: P2-S01.

Assemble system policy, instruction ledger, objectives, current decisions, WorkingState, owed tool pairs and recent tail before optional contributors. Retain unclassified admitted instructions and mandatory project rules independent of top-k. Calculate budgets and explain omissions.

Evidence: C18/C19 pass with a deliberately irrelevant search query; mandatory overflow produces a visible pause.

### 4.4. P2-S04 — Wire the persisted loop and frozen requests

Depends on: P2-S02, P2-S03.

Claim durable input, resolve composition, commit the exact sanitized request and manifest, then call the provider. Record settled messages separately from failed/canceled attempts. Route fixture tools through a typed execution boundary; do not add production shell tools yet.

Evidence: Request reconstruction matches recorded packets; context/config changes cannot alter an in-flight request.

### 4.5. P2-S05 — Implement compaction with deterministic fallback

Depends on: P2-S03, P2-S04.

Record source sequence, build a candidate outside SQL transactions, validate mandatory state/tool pairs and CAS the checkpoint. Retry/rebase when the tail changes. On summary failure use deterministic WorkingState context; never delete raw source events.

Evidence: C04/C09 survive five compactions and A→B correction; summary failure does not erase requirements.

### 4.6. P2-S06 — Implement resume, continuation and offline replay

Depends on: P2-S04, P2-S05.

Reacquire ownership, restore state/inbox, reconcile pending fixture calls and compare composition/event versions. New-session continuation links the same task without importing unrelated work. Offline replay reads recorded packets and never dispatches models/tools.

Evidence: C06/C14/K10/K11 pass; unsupported critical events refuse execution while inspection remains available.

### 4.7. P2-S07 — Integrate CLI and recovery acceptance

Depends on: P2-S01..P2-S06.

Display the recovery receipt before acting; expose token sources, skipped optional blocks and effective config revisions. Run keyless end-to-end loop/restart tests. Offer a separately authorized, cost-bounded provider smoke only when credentials are locally available.

Evidence: P2 evidence distinguishes mock/local adapter tests from any actually performed live API smoke.

## 5. Tests and verification commands

Primary: C04, C06, C09, C14, C18, C19, K10, K11. Strengthen C01/C02/C15/C24/K02 with the live actor; test the summary-failure part of C05 now, while full extraction/embedding failure belongs to P4. Assert zero network/model/tool calls in offline replay; exact provider output is not expected to be deterministic in live smoke.

Future phase commands, to run after implementing the targets:

```powershell
cargo test -p harness-cli --test phase_p2 --locked
pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P2
```

The full gate includes the handbook's formatting, clippy, workspace tests, test-discovery checks and docs checks. Do not report a filtered zero-test run as passing acceptance.

## 6. Demonstration rehearsal

1. Run the scripted three-requirement task through a mock model with fixture tools.
2. Force five compactions, replacing decision A with B midway.
3. Kill the fixture host after a durable result; start `ha resume <session-id>`.
4. Inspect the recovery receipt and context manifest; require the failure/remaining step and B to remain.
5. Disable all providers and replay offline; compare recorded packets and assert no dispatched calls.

## 7. Exit gate and forbidden shortcuts

Single-agent loop, context/compaction and resume gates pass. Missing optional memory cannot block restoration. No silent discard of mandatory instructions, no unsnapshotted prompt injection, no tool execution from partial JSON and no blind retry of an uncertain call. No real coding toolset or multi-agent scheduler in this phase.

Do not advance phases on a summary alone. Bind results to the final tested revision, report missing checks, and preserve all predecessor regressions. Never alter fixture expectations merely to make implementation pass.

## 8. Delegation and handoff

After S01, providers S02 and context S03 can have separate assigned owners. One runtime integrator owns S04 and packet schema integration. P3 receives the execution boundary, actor cancellation contract, model/tool schemas, recovery receipts and fixture transcript.

Deliver `docs/evidence/P2.en.md` and `P2.vi.md`, plus a resumable handoff under `docs/handoffs/`, following the handbook. Include completed step IDs, pending failures, schema changes, commands and next action. Publication requires explicit authorization in the coding assignment.

## 9. Ready-to-use agent prompt

```text
Implement P2 only. Read docs/implementation/README.en.md and
P2_RUNTIME_CONTEXT.en.md in that directory, all linked architecture contracts,
and applicable repository instructions. Verify the predecessor gate from source/evidence.
Create the phase SPEC, then implement P2-S01..P2-S07 in dependency order.
Stay within this phase's owned scope; preserve unrelated changes and accepted contracts.
Use real components for acceptance tests, with mocks only at appropriate external boundaries.
Run the phase gate and predecessor regressions; deliver bilingual evidence and a restart handoff.
Stop before the next phase. Do not spawn agents, commit, push or publish unless explicitly assigned.
If prerequisites or required verification are unavailable, report the exact gap without claiming completion.
```
