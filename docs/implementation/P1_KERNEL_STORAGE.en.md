# P1 — Plugin kernel and durable storage

English | [Tiếng Việt](P1_KERNEL_STORAGE.vi.md)

Implementation runbook; status: **not started**. Estimate: 8–11 person-days. All target files, Rust tests and `ha` commands below are future outputs unless already present in the checkout. This document alone is not completion evidence.

## 1. Outcome and entry gate

Require an accepted [P0](P0_FOUNDATION.en.md) gate. Deliver the smallest durable work-continuation slice: input ACK only after commit, exact instruction retention, snapshot+tail restoration, one writer and scoped plugin ownership. Use scripted actors, not a production LLM loop.

Read the [handbook](README.en.md), [plan](../RUST_HARNESS_PLAN.en.md), [plugin contract](../PLUGIN_ARCHITECTURE.en.md), [memory contract](../MEMORY_AND_CONTINUITY.en.md) and [acceptance map](ACCEPTANCE_MAP.en.md). Inspect actual predecessor evidence before coding.

## 2. Owned scope and target files

Own `crates/harness-kernel/`, `crates/harness-store-sqlite/`, `crates/harness-session/`, their contract modules, initial migrations, `crates/harness-cli/tests/phase_p1.rs` and fixture-only crash helpers. Extend CLI `init`, `sessions list`, `status`, `plugins list/inspect` and `config explain` for implemented metadata only.

## 3. Contracts to settle before implementation

Set the SQLite transaction coordinator API and host/session/task ownership rules before consumers code against them. Define input idempotency, event sequence CAS, instruction ledger updates, snapshot coverage/version/hash, durable source-work markers and typed storage failures. Kernel shutdown phases and plugin generations must be testable without model calls. Reserve memory job schema but do not implement extraction here.

## 4. Ordered work items

### 4.1. P1-S01 — Finalize persistence and lifecycle interfaces

Depends on: Accepted P0.

Specify store transaction commands and read views, process lock lifetime, task/session fencing, plugin manifest/service keys and shutdown report. Write SQL constraints/migration plan together with P1 failure fixtures.

Evidence: No interface promises atomic commits across independent backends; missing dependency/errors have named outcomes.

### 4.2. P1-S02 — Implement SQLite foundation and ownership

Depends on: P1-S01.

Open local-disk SQLite with foreign keys, WAL, busy timeout and FULL durable writes. Implement one writable host lock and generation-checked writes; read-only clients must not migrate. Add transactional migrations and fault injection at storage boundaries.

Evidence: C15/C24 fixtures reject competing session/task owners; failed/stale writer cannot acknowledge data.

### 4.3. P1-S03 — Implement plugin registry and resources

Depends on: P1-S01.

Add graph validation, nearest scoped lookup, exact-registration undo, service leases, unpublished resource collection, rollback and dependent drain on provider loss. Join cleanup before closing dependent services; report failures.

Evidence: K01–K07 component fixtures cover duplicate/missing/cyclic services, optional extractor, stale disposer and racing shutdown.

### 4.4. P1-S04 — Commit input and event-derived WorkingState

Depends on: P1-S02.

One transaction writes inbox, admitted user text, instruction ledger, task projection and source-work marker. Apply stable input IDs, expected sequence and actor authority. Build deterministic event folds; model proposals never become observed receipts.

Evidence: C01 and duplicate-ID fixture show one logical input; unclassified text is retained even before later context tests.

### 4.5. P1-S05 — Implement artifacts, snapshots and recovery

Depends on: P1-S02, P1-S04.

Publish flushed artifact bytes before references, validate snapshot schema/hash, then fold committed tail. Fall back from damaged snapshot to journal; unknown critical/corrupt journal events are visible errors. Store pending intent as uncertain rather than marking it completed.

Evidence: C02 recovers a committed synthetic receipt absent from the snapshot; no-tail omission and corruption fixtures fail clearly.

### 4.6. P1-S06 — Exercise crash and I/O boundaries

Depends on: P1-S03, P1-S05.

Launch a disposable child host; synchronize through explicit failpoints and ACKs, terminate it, reopen the same data directory and assert state. Inject SQLite write failure in admission/intent/settlement without filling the user's disk. Verify store closes last.

Evidence: C21 has no false ACK or next side effect after commit failure. State survives real process termination, not only a recreated Rust struct.

### 4.7. P1-S07 — Expose inspectable state and complete the gate

Depends on: P1-S01..P1-S06.

Wire metadata/status commands to read services, JSON schema and diagnostics. Run the canonical two-completed/one-remaining recovery demo; register exact test names and prepare P2 contracts/evidence.

Evidence: Status output matches persisted evidence and explicitly says execution runtime is not yet available.

## 5. Tests and verification commands

Primary component cases: C01, C02, C15, C21, C24 and K01–K07. Add property tests for event folding/snapshot+tail equivalence, duplicate IDs and registration undo. K02 uses a minimal restore fixture with no extractor; K06 uses a controlled service call. P2/P3/P5 must strengthen these against the real loop/tools/children.

Future phase commands, to run after implementing the targets:

```powershell
cargo test -p harness-cli --test phase_p1 --locked
pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P1
```

The full gate includes the handbook's formatting, clippy, workspace tests, test-discovery checks and docs checks. Do not report a filtered zero-test run as passing acceptance.

## 6. Demonstration rehearsal

1. The acceptance helper creates a temporary store and admits the canonical task.
2. It commits two synthetic work receipts, leaves one remaining item, and prints an input/result ACK.
3. The parent terminates the helper at an acknowledged failpoint.
4. A new helper reopens the store; `ha status <session-id> --json` shows the correct state.
5. Attempt a competing writer and inject a write failure; verify explicit rejection/no false completion. Synthetic receipts are not real code-test results.

## 7. Exit gate and forbidden shortcuts

All primary cases and predecessor checks run; crash recovery uses a real disposable process. No in-memory-only queue may be the sole work record, no success ACK before commit, no `Drop`-only async cleanup. Do not add memory LLM extraction, shell tool execution or multi-agent scheduling.

Do not advance phases on a summary alone. Bind results to the final tested revision, report missing checks, and preserve all predecessor regressions. Never alter fixture expectations merely to make implementation pass.

## 8. Delegation and handoff

After S01, kernel S03 and store S02 may be assigned in parallel to disjoint owners. S04–S06 integrate after their dependencies. One integrator owns migration numbering and shared contracts. P2 receives transaction/recovery APIs, snapshot format, plugin leases and the failure-injection protocol.

Deliver `docs/evidence/P1.en.md` and `P1.vi.md`, plus a resumable handoff under `docs/handoffs/`, following the handbook. Include completed step IDs, pending failures, schema changes, commands and next action. Publication requires explicit authorization in the coding assignment.

## 9. Ready-to-use agent prompt

```text
Implement P1 only. Read docs/implementation/README.en.md and
P1_KERNEL_STORAGE.en.md in that directory, all linked architecture contracts,
and applicable repository instructions. Verify the predecessor gate from source/evidence.
Create the phase SPEC, then implement P1-S01..P1-S07 in dependency order.
Stay within this phase's owned scope; preserve unrelated changes and accepted contracts.
Use real components for acceptance tests, with mocks only at appropriate external boundaries.
Run the phase gate and predecessor regressions; deliver bilingual evidence and a restart handoff.
Stop before the next phase. Do not spawn agents, commit, push or publish unless explicitly assigned.
If prerequisites or required verification are unavailable, report the exact gap without claiming completion.
```
