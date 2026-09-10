# P7 — Recovery hardening and CLI release

English | [Tiếng Việt](P7_RELEASE.vi.md)

Implementation runbook; status: **not started**. Estimate: 8–11 person-days. All target files, Rust tests and `ha` commands below are future outputs unless already present in the checkout. This document alone is not completion evidence.

## 1. Outcome and entry gate

Require accepted [P6](P6_EXTENSIONS.en.md). Harden and package the CLI; prove consistent backups/migrations and explicit data retention. This is the CLI release gate, not permission to deploy software to a production host or publish a GitHub release.

Read the [handbook](README.en.md), [plan](../RUST_HARNESS_PLAN.en.md), [plugin contract](../PLUGIN_ARCHITECTURE.en.md), [memory contract](../MEMORY_AND_CONTINUITY.en.md) and [acceptance map](ACCEPTANCE_MAP.en.md). Inspect actual predecessor evidence before coding.

## 2. Owned scope and target files

Own backup/restore/migration/retention modules through `harness-store-sqlite` and memory/session APIs, `ha doctor` and maintenance commands, `crates/harness-cli/tests/phase_p7.rs`, packaging/release automation, continuity eval fixtures and operator documentation. Preserve all earlier runtime and plugin contracts.

## 3. Contracts to settle before implementation

Specify backup manifest/schema/artifact pins, restore-into-new-directory activation, migration version checks, tombstones, invalidation versus deletion and GC retention roots. Define supported platform/capability matrix, release quality gates and provider-smoke policy. Deletion/restore fixtures operate only on disposable data, never the user's active store.

## 4. Ordered work items

### 4.1. P7-S01 — Freeze the release matrix and failure model

Depends on: Accepted P6.

List supported OS/toolchain/provider/runner modes and every C/K case with executable names. Identify unimplemented or component-only checks requiring end-to-end strengthening. Define nonfunctional benchmark hardware/dataset and finite live-evaluation budgets.

Evidence: The release checklist cannot silently omit a failing or unsupported required platform/case.

### 4.2. P7-S02 — Implement consistent backup and isolated restore

Depends on: P7-S01.

Use a supported SQLite snapshot/backup mechanism with versioned manifest, referenced artifacts and hashes. Hold retention pins during backup. Restore into a new directory, validate integrity/schema/artifacts/tombstones, and activate only by an explicit maintenance action.

Evidence: C27 rejects incomplete backup, missing artifacts and overwrite of an active data directory.

### 4.3. P7-S03 — Implement upgrade/downgrade safeguards

Depends on: P7-S02.

Test migrations from supported old fixtures on copies, preserve old event decoding and rebuild versioned projections. Require a recoverable backup before destructive schema evolution. Old binaries refuse incompatible writes; diagnosis may stay read-only.

Evidence: C27 upgrade works with backlog/incomplete work; interrupted migration is safe and unsupported downgrade cannot mutate.

### 4.4. P7-S04 — Implement retention, forgetting and safe collection

Depends on: P7-S01, P7-S02.

Separate invalidate/archive/forget APIs. Require an explicit deletion target/confirmation, propagate deletion to derived payloads/indexes/caches, add tombstones and prevent re-extraction. Pin unfinished work and backups; GC only unreferenced artifacts after the chosen grace period.

Evidence: C28 reports reduced old-session evidence and prevents deleted content from reappearing; remaining external/backup copies are disclosed.

### 4.5. P7-S05 — Run adversarial recovery and privacy suite

Depends on: P7-S03, P7-S04.

Execute all C01–C30/K01–K14 using real stores/processes and integrated actors where applicable. Repeat race/crash fixtures with recorded seeds; cover quota, disk I/O failure, corrupted snapshot/journal, secret/path policy and unsupported schema. Test checker failure paths too.

Evidence: All required tests actually execute without ignore/zero-test shortcuts; no stale intermediate run is reported as final evidence.

### 4.6. P7-S06 — Measure and package the actual CLI

Depends on: P7-S05.

Measure retrieval/restore targets on the documented dataset, run ten multi-step continuity evaluations against fixed baselines, and report model variability. Build Windows/Linux artifacts, checksums and supported-dependency notices. Smoke the packaged binaries, not only `cargo run`; live provider checks use explicit local credentials/budget.

Evidence: Performance misses are disclosed; no durability guard is weakened. Keyless tests and live-model results remain separately labeled.

### 4.7. P7-S07 — Prepare release evidence and operator handoff

Depends on: P7-S01..P7-S06.

Write install/config/doctor/resume/recovery/backup guides and known limitations in both languages. Verify packaged-binary startup and release candidate source/CI identities. If publication is authorized, publish only the tested artifacts and verify remote state; otherwise hand off a local release candidate.

Evidence: The user sees precisely what is implemented, tested, unverified and not deployed.

## 5. Tests and verification commands

Primary: C27, C28. Final release gate reruns every C and K case plus earlier platform/process/integration suites. Add migration interruption, tombstone/backlog restoration, backup-GC race, active-store overwrite denial and corrupted artifact tests. Benchmark targets remain measured design targets, not achieved numbers by declaration. Required platform failures block claiming a fully verified release.

Future phase commands, to run after implementing the targets:

```powershell
cargo test -p harness-cli --test phase_p7 --locked
pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P7
```

The full gate includes the handbook's formatting, clippy, workspace tests, test-discovery checks and docs checks. Do not report a filtered zero-test run as passing acceptance.

## 6. Demonstration rehearsal

1. Start a task with two completed steps, a failed check, pending child work and extraction backlog.
2. Create a consistent backup while work is present; restore into a fresh data directory.
3. Resume and verify source state, task ownership and pending jobs without duplicate side effects.
4. Forget one selected source in a disposable copy; show its derived memory cannot reappear and old replay reports the gap.
5. Run the same recovery from the packaged Windows/Linux binary, then record checksums and exact evidence.

## 7. Exit gate and forbidden shortcuts

All 44 C/K cases have final executable evidence, backup/retention gates pass and the supported platform matrix is honest. No claim of production activation from local/CI success. If required live-provider verification lacks credentials, report it as unverified and obtain a release-scope decision rather than inventing success. Do not implement Web or a daemon here.

Do not advance phases on a summary alone. Bind results to the final tested revision, report missing checks, and preserve all predecessor regressions. Never alter fixture expectations merely to make implementation pass.

## 8. Delegation and handoff

Backup/restore S02 and retention design can be assigned after S01, but GC implementation waits for backup pin contracts. One store owner controls migrations. Evaluations/packaging work follows tested contracts and converges on one exact release candidate revision. P8 receives stable application services, command/error schemas and the CLI baseline to protect.

Deliver `docs/evidence/P7.en.md` and `P7.vi.md`, plus a resumable handoff under `docs/handoffs/`, following the handbook. Include completed step IDs, pending failures, schema changes, commands and next action. Publication requires explicit authorization in the coding assignment.

## 9. Ready-to-use agent prompt

```text
Implement P7 only. Read docs/implementation/README.en.md and
P7_RELEASE.en.md in that directory, all linked architecture contracts,
and applicable repository instructions. Verify the predecessor gate from source/evidence.
Create the phase SPEC, then implement P7-S01..P7-S07 in dependency order.
Stay within this phase's owned scope; preserve unrelated changes and accepted contracts.
Use real components for acceptance tests, with mocks only at appropriate external boundaries.
Run the phase gate and predecessor regressions; deliver bilingual evidence and a restart handoff.
Stop before the next phase. Do not spawn agents, commit, push or publish unless explicitly assigned.
If prerequisites or required verification are unavailable, report the exact gap without claiming completion.
```
