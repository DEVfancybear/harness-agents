# Implementation handbook

English | [Tiếng Việt](README.vi.md)

Runbook revision 1 — September 10, 2026. Derived from architecture revision 2 at commit `b636208`. This directory specifies future implementation work; every phase is currently **not started**. No Rust runtime or phase test runner has been created by writing these documents.

## 1. How to use this pack

Start an implementation agent with the prompt in section 9 of the selected phase. Begin with P0. Give the agent repository access and an explicit assignment; merely reading this pack is not permission to run later phases, spawn workers, install arbitrary tools or publish a release.

Every phase contains seven ordered work items, proposed file ownership, contract decisions, tests, a demonstration, an exit gate and handoff requirements. The 63 step IDs are stable task identifiers. Target paths describe files to create or extend after inspecting actual code; do not replace existing implementations with fresh scaffolding blindly.

Source of truth: [architecture plan](../RUST_HARNESS_PLAN.en.md), [plugin contracts](../PLUGIN_ARCHITECTURE.en.md), [memory contracts](../MEMORY_AND_CONTINUITY.en.md), then these runbooks. A runbook decomposes those contracts; it does not silently weaken them. Escalate genuine contradictions with a concrete proposed resolution.

## 2. Phase order and scope

| Phase | Runbook | Prerequisite | Person-days | Current status |
|---|---|---|---:|---|
| P0 | [Foundation, contracts and test scaffolding](P0_FOUNDATION.en.md) | — | 3–4 | not_started |
| P1 | [Plugin kernel and durable storage](P1_KERNEL_STORAGE.en.md) | P0 | 8–11 | not_started |
| P2 | [Agent runtime, context and compaction](P2_RUNTIME_CONTEXT.en.md) | P1 | 7–10 | not_started |
| P3 | [Coding tools, execution policy and receipts](P3_CODING_TOOLS.en.md) | P2 | 7–10 | not_started |
| P4 | [Reusable memory and recovery-safe extraction](P4_MEMORY.en.md) | P3 | 7–10 | not_started |
| P5 | [Delegation, task DAG and isolated workspaces](P5_MULTI_AGENT.en.md) | P4 | 8–12 | not_started |
| P6 | [Skills, MCP and external plugin protocol](P6_EXTENSIONS.en.md) | P5 | 5–8 | not_started |
| P7 | [Recovery hardening and CLI release](P7_RELEASE.en.md) | P6 | 8–11 | not_started |
| P8 | [Web UI over the same host services](P8_WEB.en.md) | P7 | 10–15 | not_started |

P0 → P1 → P2 → P3 → P4 → P5 → P6 → P7 → optional P8. A phase starts after its predecessor's integration gate is accepted. Design review and fixture preparation may overlap, but do not merge code against an unaccepted predecessor interface.

P0–P7 remain 53–76 person-days before contingency; P8 is additional. These estimates are not agent execution deadlines. Multiple agents do not automatically divide the calendar estimate by their count.

`manifest.json` is a planning catalog of dependencies, steps and case ownership. It is **not** the live product task database. Future completion status requires evidence at a source revision, not an edited JSON status alone.

## 3. Common startup procedure for every coding agent

1. Read applicable repository instructions, current Git status, assigned phase and predecessor handoff/evidence. Preserve unrelated edits.
2. Verify the predecessor source revision and relevant tests in the actual checkout. A prose “done” from another agent is insufficient.
3. Write a short phase SPEC under `docs/specs/Pn.en.md` and `Pn.vi.md`: selected scope, test mapping, failure modes, dependencies, environment changes and unresolved decisions. These paths are future outputs, not existing links.
4. Confirm material contract/security/storage changes with the user before expanding scope. Never infer approval for data deletion, paid unlimited API calls, public releases or host activation from “implement phase.”
5. Claim the exact step/file ownership agreed with the coordinator. Implement one testable slice at a time; first reproduce expected failure where practical.
6. Run the phase gate and the existing regression suite after the last change. Inspect actual test discovery: a zero-test successful Cargo filter is not proof.
7. Produce source-bound evidence and a continuation handoff even if blocked or interrupted. Do not mark the phase complete when a required gate has not run.

## 4. Contracts shared across phases

P0 fixes serialization versions, ID formats, typed error conventions, test registry format and public CLI naming. Feature-specific fields/behaviors are then finalized in the owning phase SPEC. Revisions are explicit: preserve event decoding, migrate storage, update both languages and test old fixtures.

Default ownership:

- `harness-types`: shared IDs and event envelopes; no business policy.
- `harness-kernel`: plugin graph, scoped registries, leases and resource lifecycle.
- `harness-session`: instruction ledger, task/session projections, WorkingState and recovery.
- `harness-store-sqlite`: migrations, transaction coordinator, durable queues, artifacts and query storage.
- `harness-runtime`: agent actor, application services, context admission and request lifecycle.
- `harness-providers`: mock and DeepSeek adapters.
- `harness-tools`: policy gate, receipts, filesystem/Git/process adapters.
- `harness-memory`: assets, extraction, retrieval, provenance and invalidation.
- `harness-orchestrator`: task DAG, agent ownership, workspaces and handoff integration.
- `harness-cli`: CLI presentation/composition and cross-crate acceptance fixtures.

Initially modules may stand in for crates, but the P0 handoff must map each logical owner to a real path. Domain records have a single authoritative writer; a new crate does not create a second database authority. The future Web adapter calls application services, never CLI internals or direct mutation SQL.

## 5. Verification contract

The only currently executable command in this pack is:

```powershell
pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest
```

P0 must create the following **future** interface. All later phase commands assume that gate exists:

```powershell
pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P0
```

The phase runner must fail closed and run:

- `cargo fmt --all -- --check`.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`.
- `cargo test --workspace --all-targets --locked`.
- The declared phase acceptance target, with nonempty test discovery and no ignored required cases.
- Documentation checks, plus additional OS/process/crash/migration gates introduced by that phase.

P0 creates `crates/harness-cli/tests/phase_p0.rs`; each later phase adds `phase_pN.rs`. These cross-crate acceptance targets orchestrate the real implementation; unit tests remain near their owning modules. Do not mock the transaction engine, policy gate or component being verified. Mock only boundaries such as model/network/clock where appropriate.

Create `tests/acceptance/registry.json` in P0: case ID, owning phase, target/test names, fixture, required platforms and readiness. Tests introduced in future phases remain `not_implemented`, never green or silently skipped. `Verify-Phase` checks the selected phase and previously accepted gates; P7 requires all C01–C30/K01–K14. P8 additionally defines its own W01–W06 cases.

A component fixture may precede an end-to-end strengthening test; [acceptance ownership](ACCEPTANCE_MAP.en.md) states that distinction. Passing a P1 synthetic receipt fixture does not claim the P3 process runner works.

## 6. Evidence and handoff format

At phase completion, create paired `docs/evidence/Pn.en.md` and `Pn.vi.md`. Store machine-readable run results/artifacts under a task-owned location chosen in P0; keep credentials and raw private project content out of Git. Evidence includes:

```text
phase / assigned steps / result: passed | failed | blocked | partial
base revision / tested revision or source-tree digest
changed files and ownership
contract/schema/dependency changes
case ID -> exact test -> command -> observed result
platform and toolchain versions
demo command and artifact references
skipped checks with reason; known limitations
remaining steps and blockers
next safe action and prerequisite for the next phase
commit/push/CI state: actual outcome or not requested
```

The result is source-specific. Tests from before the last code change are stale. If the environment lacks an OS or provider credential, report the gap; a required release gate remains incomplete. Documentation validation is not runtime verification.

Keep a small task handoff while working: `docs/handoffs/Pn.en.md` and `Pn.vi.md`, including completed step IDs, failures, exact commands, file fingerprints, pending operations and next action. This is development handoff material, not a substitute for the harness's eventual durable memory.

## 7. Multiple implementation agents

The user/coordinator assigns parallel work explicitly. Within a phase, workers may implement disjoint modules only after shared contracts are accepted. One integrator owns root Cargo files/lockfile, shared schemas, migrations numbering and acceptance registry. No two workers edit the same file without an agreed handoff.

Restrictions on spawning agents refer to additional implementation assistants. They do not prohibit running isolated product-agent actors that the assigned phase's acceptance tests require.

Worker assignment must state phase/step IDs, input revision, owned paths, allowed dependencies, acceptance tests and forbidden changes. Workers return results and evidence; the integrator resolves overlaps, tests the integrated revision and authorizes the phase transition. Do not launch agents for P0–P8 simultaneously.

Optional isolated branches/workspaces preserve the user's existing changes. Follow the actual environment's worktree tooling; this pack does not authorize manipulating Orca-managed state. No blanket commit/push permission: each coding assignment must state whether publication is requested.

## 8. Stop and recovery rules

Stop the affected work when a required predecessor is absent, a schema/security contract conflicts, tests expose data loss, a mutation outcome is unknown, or protected user data would be overwritten. Continue safe diagnosis and isolated fixtures; do not bypass the failing guard.

On context exhaustion or agent replacement, the next agent reads the handoff, Git diff and evidence, reruns the necessary baseline, and continues the next unfinished step. It must not regenerate a completed phase merely because its conversation history is missing.

The first useful delivery is P1's kill/reopen demonstration; P3 adds actual coding, P5 adds delegated agents, P7 is the CLI release gate. Web remains separate.

## 9. Initial assignment prompt

```text
Implement P0 only in this repository.
Read docs/implementation/README.en.md and P0_FOUNDATION.en.md in that directory,
then the linked architecture contracts and any repository instructions.
Inspect the current checkout; create the P0 SPEC and implement P0-S01..P0-S07.
Do not implement the runtime, memory extractor, multi-agent scheduler or Web UI.
Use fixtures without real credentials. Prove test discovery and negative controls.
Deliver bilingual evidence and a handoff bound to the tested source revision.
Do not start P1 or spawn additional agents unless I explicitly assign that work.
Do not commit or push unless I request publication in this assignment.
```
