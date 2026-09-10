# P3 — Coding tools, execution policy and receipts

English | [Tiếng Việt](P3_CODING_TOOLS.vi.md)

Implementation runbook; status: **not started**. Estimate: 7–10 person-days. All target files, Rust tests and `ha` commands below are future outputs unless already present in the checkout. This document alone is not completion evidence.

## 1. Outcome and entry gate

Require accepted [P2](P2_RUNTIME_CONTEXT.en.md). Deliver a useful single-agent coding CLI that reads/edits a fixture repository, runs checks and resumes with trustworthy execution evidence. Host-development mode must report its actual protection level.

Read the [handbook](README.en.md), [plan](../RUST_HARNESS_PLAN.en.md), [plugin contract](../PLUGIN_ARCHITECTURE.en.md), [memory contract](../MEMORY_AND_CONTINUITY.en.md) and [acceptance map](ACCEPTANCE_MAP.en.md). Inspect actual predecessor evidence before coding.

## 2. Owned scope and target files

Own `crates/harness-tools/` modules for policy/approval, filesystem/search, Git, process runners and receipts; revision observation in session services; `crates/harness-cli/tests/phase_p3.rs`; disposable repository/process fixtures. Extend the loop's existing tool boundary; do not create a second executor.

## 3. Contracts to settle before implementation

Before coding, specify path/root canonicalization, symlink/junction policy, before/after content hashes, binary/encoding behavior, process executable/argv versus shell script types, output limits and invocation-bound approval. Test receipts reference the checked worktree fingerprint, not only HEAD. Declare runner capabilities; strict isolation requested on an unsupported platform must fail closed.

## 4. Ordered work items

### 4.1. P3-S01 — Specify tools, policy and receipt schemas

Depends on: Accepted P2.

Define the initial read/list/search/edit/process/Git/task-update tools and required capabilities. Separate proposed action, execution intent, immutable receipt and model/UI view. Bind approval to actor, resolved args, invocation, workspace, tool and policy revision.

Evidence: Schema validation and guard errors produce denied/failed outcomes, never permission expansion.

### 4.2. P3-S02 — Implement one execution gate

Depends on: P3-S01.

Validate arguments/identity, apply transforms, revalidate, obtain approval, run monotonic guards, commit intent/approval consumption and only then dispatch. Check cancel/revocation at admission. Persist receipt before notifying observers; presentation cannot change evidence.

Evidence: K08/K09 reject late allow, changed arguments and forged success; observer failure leaves a durable receipt.

### 4.3. P3-S03 — Implement filesystem and search tools

Depends on: P3-S01, P3-S02.

Implement rooted read/list/search and patch operations with output/size limits, ignore rules and explicit sensitive-path policy. Recheck observed content hash before editing; serialize host edits and detect external modifications. Handle Unicode, CRLF, spaces, symlink/junction escape and binary refusal.

Evidence: Stale edit/path traversal fixtures cannot overwrite unexpected content; searches are bounded and preserve source references.

### 4.4. P3-S04 — Implement Windows/Linux process runners

Depends on: P3-S02.

Support executable/argv and explicit shell requests, bounded stdout/stderr, timeouts and tree cancellation. On Windows test Job Object ownership and descendants; on Linux test the selected process-tree mechanism. Do not equate canceled futures with stopped processes.

Evidence: A grandchild holding an output pipe is terminated/reaped or visibly unresolved; no hanging shutdown or false clean receipt.

### 4.5. P3-S05 — Bind Git observations and checks to actual files

Depends on: P3-S03, P3-S04.

Implement status/diff and project registration/reassociation checks. Capture tracked/untracked fixture fingerprints around tests and edits. Changes after a test retain its historical result but invalidate its current applicability; task_update cannot fabricate a runner receipt.

Evidence: C10/C23 and C29 artifact/secret fixtures reject stale evidence, unrelated project merging and cross-scope hash reads.

### 4.6. P3-S06 — Implement uncertainty reconciliation

Depends on: P3-S02..P3-S05.

Crash after a real disposable patch/command side effect but before result commit. Restore the intent, compare before/after state, and classify applied/not applied/unknown with operation-specific checks. Never blindly rerun an arbitrary shell command.

Evidence: C03 passes; strengthen C02/C21 with real tool effects and failed settlement storage.

### 4.7. P3-S07 — Integrate the coding flow and verify platforms

Depends on: P3-S01..P3-S06.

Run the parser-fix fixture through the P2 loop and these tools. Show failed and passed checks on exact revisions, edits and remaining work after restart. Record platform capabilities and package-independent startup instructions.

Evidence: A single-agent CLI performs real fixture edits/tests; unsupported strict sandbox remains an explicit denial, not a claimed security guarantee.

## 5. Tests and verification commands

Primary: C03, C10, C23, C29, K08, K09. Strengthen C02/C21/K06/K07 using real subprocesses. Add path escape, stale hash, encoding, binary, oversized output, timeout, descendant cleanup and cancellation race tests on Windows/Linux. Use generated fake credentials only; never run destructive fixtures in the user's repository.

Future phase commands, to run after implementing the targets:

```powershell
cargo test -p harness-cli --test phase_p3 --locked
pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P3
```

The full gate includes the handbook's formatting, clippy, workspace tests, test-discovery checks and docs checks. Do not report a filtered zero-test run as passing acceptance.

## 6. Demonstration rehearsal

1. Create a temporary parser fixture with one failing test and a public API constraint.
2. Run the agent with mock/model responses that exercise the real read/edit/process tools.
3. Capture a passing result on R1, then modify a source to R2 and show that the old receipt is stale.
4. Kill after a non-repeatable fixture command writes a marker but before its receipt; resume without writing that marker twice.
5. Exercise cancel on a child/grandchild process tree and inspect the terminal receipt.

## 7. Exit gate and forbidden shortcuts

Real edits/checks and uncertainty handling pass on required platforms. Denied capability must remain denied through nested/presentation code. No forced user reset/stash, unbounded process output, approval reuse across materially changed calls or unsupported sandbox success. Multi-agent worktrees remain P5.

Do not advance phases on a summary alone. Bind results to the final tested revision, report missing checks, and preserve all predecessor regressions. Never alter fixture expectations merely to make implementation pass.

## 8. Delegation and handoff

After S02, filesystem S03 and process S04 can be assigned separately; Git observation S05 integrates both. The policy/receipt owner controls shared types. P4 receives trusted source receipts, sensitive-data handling, project fingerprints and the common execution gate.

Deliver `docs/evidence/P3.en.md` and `P3.vi.md`, plus a resumable handoff under `docs/handoffs/`, following the handbook. Include completed step IDs, pending failures, schema changes, commands and next action. Publication requires explicit authorization in the coding assignment.

## 9. Ready-to-use agent prompt

```text
Implement P3 only. Read docs/implementation/README.en.md and
P3_CODING_TOOLS.en.md in that directory, all linked architecture contracts,
and applicable repository instructions. Verify the predecessor gate from source/evidence.
Create the phase SPEC, then implement P3-S01..P3-S07 in dependency order.
Stay within this phase's owned scope; preserve unrelated changes and accepted contracts.
Use real components for acceptance tests, with mocks only at appropriate external boundaries.
Run the phase gate and predecessor regressions; deliver bilingual evidence and a restart handoff.
Stop before the next phase. Do not spawn agents, commit, push or publish unless explicitly assigned.
If prerequisites or required verification are unavailable, report the exact gap without claiming completion.
```
