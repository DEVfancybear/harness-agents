# P5 — Delegation, task DAG and isolated workspaces

English | [Tiếng Việt](P5_MULTI_AGENT.vi.md)

Implementation runbook; status: **not started**. Estimate: 8–12 person-days. All target files, Rust tests and `ha` commands below are future outputs unless already present in the checkout. This document alone is not completion evidence.

## 1. Outcome and entry gate

Require accepted [P4](P4_MEMORY.en.md). Deliver one coordinator plus up to three workers with independent contexts, durable task ownership/results and isolated editing worktrees. All run in the same writable host; a daemon and remote workers are outside this phase.

Read the [handbook](README.en.md), [plan](../RUST_HARNESS_PLAN.en.md), [plugin contract](../PLUGIN_ARCHITECTURE.en.md), [memory contract](../MEMORY_AND_CONTINUITY.en.md) and [acceptance map](ACCEPTANCE_MAP.en.md). Inspect actual predecessor evidence before coding.

## 2. Owned scope and target files

Own `crates/harness-orchestrator/`, host task/DAG/delivery commands, `WorkspaceManager` integration, agent profiles/presets, `crates/harness-cli/tests/phase_p5.rs`, clean/dirty repository fixtures and CLI `run --agents`, task status/cancel/handoff views. Extend existing memory grants and runtime factory, not separate agent engines.

## 3. Contracts to settle before implementation

Specify task lifecycle/DAG validation, delegated budgets/depth, actor/profile/run IDs, ownership generations, result acceptance and message deduplication. Define worktree input snapshot, file ownership, integration order and source fingerprint checks. Default editing delegation requires a clean fixture repo; dirty input must be refused until a preservation-tested snapshot path is implemented.

## 4. Ordered work items

### 4.1. P5-S01 — Define delegation and task contracts

Depends on: Accepted P4.

Write task brief/result schemas: objective, acceptance criteria, inputs, base snapshot, grants, budget, deadline, artifacts and exact checked revisions. Separate a worker report from host-accepted completion. Define parent cancel versus pause semantics and maximum depth/slots.

Evidence: No role name grants permission; no circular dependency or ambiguous task owner is admitted.

### 4.2. P5-S02 — Implement durable DAG, ownership and delivery

Depends on: P5-S01.

Create host-owned transactional task transitions, dependency checks, run generations and parent delivery records. Child completion and parent message commit together. Consume delivery once logically; notifications only wake readers. Pending parent sessions receive inbox data without arbitrary event-log appends.

Evidence: C11/C25 fixtures preserve a completed result across parent shutdown and notification loss.

### 4.3. P5-S03 — Implement agent scheduling and budgets

Depends on: P5-S01, P5-S02.

Spawn/factory-create child actors with independent contexts, inboxes and cancellation under owned plugin resources. Bound concurrency/depth/model requests and aggregate cost. Release compute permits while a parent waits; cancel/drain descendants on host shutdown.

Evidence: No parent-holds-all-slots deadlock; completed work is not reassigned after restoring the host.

### 4.4. P5-S04 — Implement clean-worktree isolation

Depends on: P5-S01.

Create host-managed editing worktrees from the same verified input snapshot. Serialize shared Git metadata operations, track branch/worktree ownership and reject unexpected external changes. Read-only workers get explicitly limited access. Add tested dirty-snapshot support only if preserving index/HEAD/untracked selections is demonstrated.

Evidence: Two workers cannot overwrite each other's files; dirty user changes are neither silently stashed nor reset.

### 4.5. P5-S05 — Implement revision-aware result integration

Depends on: P5-S02, P5-S03, P5-S04.

Validate result scope/artifacts/receipts, integrate in dependency order in an integration worktree, detect conflicts and run final checks. Before applying to the user's worktree recheck its fingerprint; reject or request conflict direction if changed.

Evidence: Per-branch test success cannot stand in for integrated-revision success; no worker pushes or integrates unilaterally.

### 4.6. P5-S06 — Connect scoped memory and crash recovery

Depends on: P5-S03, P5-S05.

Bind task/profile assets at spawn; record exact source versions in handoffs. Later updates enter at logged boundaries. Crash at child completion, parent delivery and integration boundaries; reconcile uncertain side effects and rebuild DAG progress from durable state.

Evidence: C07/C08/C24 hold with real delegated actors; semantic memory cannot decide whether a task completed.

### 4.7. P5-S07 — Expose delegation and run the complete demonstration

Depends on: P5-S01..P5-S06.

Wire `--agents 3`, task/worker status, blockers, cancel and result inspection. Run explorer/coder/verifier roles through one coordinated fixture with bounded requests. Keep role availability separate from actual concurrent slot count.

Evidence: The CLI displays ownership, input/result revisions and remaining work correctly after restart.

## 5. Tests and verification commands

Primary: C11, C25. Strengthen C03/C07/C08/C10/C15/C23/C24/K04/K06/K07 against real actors/worktrees. Add DAG cycle rejection, dependency failure propagation, parent-wait fairness, exhausted budgets, depth cap, concurrent Git metadata and final-worktree-change tests. Empty child responses and human-readable 'done' without required artifacts must not settle accepted work.

Future phase commands, to run after implementing the targets:

```powershell
cargo test -p harness-cli --test phase_p5 --locked
pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P5
```

The full gate includes the handbook's formatting, clippy, workspace tests, test-discovery checks and docs checks. Do not report a filtered zero-test run as passing acceptance.

## 6. Demonstration rehearsal

1. Start `ha run <fixture-task> --agents 3` with one coordinator and three worker slots.
2. Explorer records a source observation; coder edits its worktree; verifier checks the exact result revision.
3. Kill the host after one child result commit but before parent notification/consumption.
4. Resume; the completed child is not reassigned, and the unfinished child continues from durable handoff.
5. Integrate results, rerun final checks and display only the final integrated revision as current evidence.

## 7. Exit gate and forbidden shortcuts

Delegation, durable coordination and final-revision checks pass; no lost child result, duplicate ownership, DAG deadlock or cross-agent memory leak. Clean-repo editing is mandatory; dirty support is either preservation-tested or clearly rejected. No background-after-CLI-exit claim, remote agents or marketplace.

Do not advance phases on a summary alone. Bind results to the final tested revision, report missing checks, and preserve all predecessor regressions. Never alter fixture expectations merely to make implementation pass.

## 8. Delegation and handoff

After S01, workspace S04 and task store S02 can have separate owners; runtime S03 requires S02. Integration S05 is integrator-owned. P6 receives spawn/tool entry points, scope/authority propagation and cross-agent recovery fixtures so extensions cannot bypass them.

Deliver `docs/evidence/P5.en.md` and `P5.vi.md`, plus a resumable handoff under `docs/handoffs/`, following the handbook. Include completed step IDs, pending failures, schema changes, commands and next action. Publication requires explicit authorization in the coding assignment.

## 9. Ready-to-use agent prompt

```text
Implement P5 only. Read docs/implementation/README.en.md and
P5_MULTI_AGENT.en.md in that directory, all linked architecture contracts,
and applicable repository instructions. Verify the predecessor gate from source/evidence.
Create the phase SPEC, then implement P5-S01..P5-S07 in dependency order.
Stay within this phase's owned scope; preserve unrelated changes and accepted contracts.
Use real components for acceptance tests, with mocks only at appropriate external boundaries.
Run the phase gate and predecessor regressions; deliver bilingual evidence and a restart handoff.
Stop before the next phase. Do not spawn agents, commit, push or publish unless explicitly assigned.
If prerequisites or required verification are unavailable, report the exact gap without claiming completion.
```
