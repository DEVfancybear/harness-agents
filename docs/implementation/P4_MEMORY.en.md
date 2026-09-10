# P4 — Reusable memory and recovery-safe extraction

English | [Tiếng Việt](P4_MEMORY.vi.md)

Implementation runbook; status: **not started**. Estimate: 7–10 person-days. All target files, Rust tests and `ha` commands below are future outputs unless already present in the checkout. This document alone is not completion evidence.

## 1. Outcome and entry gate

Require accepted [P3](P3_CODING_TOOLS.en.md). Deliver inspectable, scoped cross-session memory with native SQLite/FTS and recoverable background extraction. The current task continues from journal/WorkingState even when every optional extractor or embedding adapter is unavailable.

Read the [handbook](README.en.md), [plan](../RUST_HARNESS_PLAN.en.md), [plugin contract](../PLUGIN_ARCHITECTURE.en.md), [memory contract](../MEMORY_AND_CONTINUITY.en.md) and [acceptance map](ACCEPTANCE_MAP.en.md). Inspect actual predecessor evidence before coding.

## 2. Owned scope and target files

Own `crates/harness-memory/`, memory store/query migrations through the existing SQLite coordinator, source dependency tracking, test extractors/contributors, `crates/harness-cli/tests/phase_p4.rs`, and CLI memory search/read/inspect/invalidate/jobs/catch-up. Do not deploy Tencent, Redis or a separate vector database.

## 3. Contracts to settle before implementation

Finalize asset/version/binding/grant/dependency records, authority/validity distinctions, extractor strategy digests, immutable input batches, job state transitions and contiguous cursor keys. Scopes include project/task/profile/session, supplied by the host. FTS is the shipping baseline; mock optional vector adapters test degradation without claiming real vector support. Keep L3 user-confirmed/manual initially.

## 4. Ordered work items

### 4.1. P4-S01 — Define memory contracts and publication policy

Depends on: Accepted P3.

Write schemas and authorized actions for assets, versions, scopes, grants and bindings. Specify which checked observations may publish, which model inferences remain candidates, and how current user decisions supersede old facts. Decide retention interfaces now; destructive purge ships in P7.

Evidence: Model confidence, role name or shared account ownership cannot become permission or runner evidence.

### 4.2. P4-S02 — Implement scoped assets and versioned writes

Depends on: P4-S01.

Use the existing transaction coordinator for immutable versions, current pointers, provenance/dependencies and CAS. Check scopes before top-k selection and again on direct reads, artifact access and exports. Distinguish query availability from authorization failure.

Evidence: C07/C08 fixtures use three actor identities/concurrent proposals with no lost update or unauthorized read.

### 4.3. P4-S03 — Implement durable extraction scheduling

Depends on: P4-S01, P4-S02.

Turn committed source-work markers into bounded non-overlapping event ranges. Lease with generation fencing, run extraction outside SQL, then atomically settle versions, dependencies, disposition, cursor and job. Retry with bounded backoff; startup discovers unprocessed ranges even after lost notifications.

Evidence: C12/C16/C17/C22 reject duplicate settlement, timestamp paging loss, cursor gaps, invalid JSON and stale workers.

### 4.4. P4-S04 — Implement L1/L2 and controlled profile updates

Depends on: P4-S03.

Extract atomic source-backed candidates from journal projections; build L2 from versioned L1 changes including invalidation. Validate output/source references, semantic-merge proposals and expected versions. Store empty extraction disposition. Strategy upgrades select explicit replay ranges, never silently reset progress.

Evidence: Extracted summaries/injected content cannot become independent evidence; confirmed L3 preferences are not inferred authority.

### 4.5. P4-S05 — Implement bounded retrieval and context contribution

Depends on: P4-S02.

Build parameterized FTS/metadata retrieval, Unicode/Vietnamese/identifier normalization, source-aware read tools and bounded index/bootstrap blocks. Return found/empty/degraded/error statuses. Only the P2 context builder admits/finalizes blocks; mandatory rules never compete in top-k.

Evidence: C05/C26 cover disabled optional services, empty hits and timeouts; journal-based resume still works.

### 4.6. P4-S06 — Implement invalidation, cache consistency and budgets

Depends on: P4-S04, P4-S05.

Track transitive sources through summaries, invalidate affected blocks and compare policy/asset revisions again before dispatch. Detect changed source files/revisions. Limit memory calls/tokens/cost independently; pause jobs durably on shutdown or exhausted budget.

Evidence: C13/C20/C30 cover reinjection loops, revoked cached summaries and extraction interrupted by CLI exit.

### 4.7. P4-S07 — Integrate memory CLI and end-to-end tests

Depends on: P4-S01..P4-S06.

Expose provenance/version/validity/source inspection and explicit catch-up with a finite budget. Restart the same task with extraction disabled, then process backlog using a deterministic mock extractor. Run concurrent writer and retrieval fixtures against real SQLite.

Evidence: Memory is useful and inspectable without being required for task restoration; capture/queue/index freshness is visible.

## 5. Tests and verification commands

Primary: C05, C07, C08, C12, C13, C16, C17, C20, C22, C26, C30. Strengthen C09/C18/C19/C29/K02/K10. C17 injects an invalid out-of-order completion even though the v1 scheduler processes each source stream sequentially. Test both no-facts success and failed extraction; only the former advances the cursor. FTS precision/recall fixtures cover Vietnamese with/without diacritics and code symbols.

Future phase commands, to run after implementing the targets:

```powershell
cargo test -p harness-cli --test phase_p4 --locked
pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P4
```

The full gate includes the handbook's formatting, clippy, workspace tests, test-discovery checks and docs checks. Do not report a filtered zero-test run as passing acceptance.

## 6. Demonstration rehearsal

1. Complete a fixture observation/check and capture its source event, keeping extraction stopped.
2. Restart the CLI; continue the unfinished coding task from WorkingState.
3. Run `ha memory catch-up --budget <finite-limit>` with the mock extractor and inspect source-linked L1/L2.
4. Correct a decision, invalidate the old asset and confirm a new context excludes its derived summary.
5. Publish concurrent proposals from three identities, then restart extraction after a settlement failpoint; prove no missing or duplicate source disposition.

## 7. Exit gate and forbidden shortcuts

All primary cases and previous gates pass; no live Tencent service or embeddings are required. No memory writes outside CAS/transaction ownership, actor IDs trusted from model arguments, timestamp-only progress, prompt injection after request freeze, self-reinforcing evidence or unlimited background spending. Full physical deletion/backup migration remains P7.

Do not advance phases on a summary alone. Bind results to the final tested revision, report missing checks, and preserve all predecessor regressions. Never alter fixture expectations merely to make implementation pass.

## 8. Delegation and handoff

After contracts S01, asset store S02 and isolated retrieval-fixture preparation can be assigned separately; retrieval code needs S02. One owner controls cursor settlement; another can implement FTS S05 after schemas are accepted. P5 receives asset/grant APIs, durable job semantics and a clear rule that task coordination never uses semantic search.

Deliver `docs/evidence/P4.en.md` and `P4.vi.md`, plus a resumable handoff under `docs/handoffs/`, following the handbook. Include completed step IDs, pending failures, schema changes, commands and next action. Publication requires explicit authorization in the coding assignment.

## 9. Ready-to-use agent prompt

```text
Implement P4 only. Read docs/implementation/README.en.md and
P4_MEMORY.en.md in that directory, all linked architecture contracts,
and applicable repository instructions. Verify the predecessor gate from source/evidence.
Create the phase SPEC, then implement P4-S01..P4-S07 in dependency order.
Stay within this phase's owned scope; preserve unrelated changes and accepted contracts.
Use real components for acceptance tests, with mocks only at appropriate external boundaries.
Run the phase gate and predecessor regressions; deliver bilingual evidence and a restart handoff.
Stop before the next phase. Do not spawn agents, commit, push or publish unless explicitly assigned.
If prerequisites or required verification are unavailable, report the exact gap without claiming completion.
```
