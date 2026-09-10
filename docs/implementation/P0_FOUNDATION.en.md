# P0 — Foundation, contracts and test scaffolding

English | [Tiếng Việt](P0_FOUNDATION.vi.md)

Implementation runbook; status: **not started**. Estimate: 3–4 person-days. All target files, Rust tests and `ha` commands below are future outputs unless already present in the checkout. This document alone is not completion evidence.

## 1. Outcome and entry gate

Deliver a buildable, keyless skeleton and an executable contract/fixture foundation. No predecessor phase. Verify the architecture documents and current clean/dirty Git state; do not assume Rust or a C toolchain is already installed.

Read the [handbook](README.en.md), [plan](../RUST_HARNESS_PLAN.en.md), [plugin contract](../PLUGIN_ARCHITECTURE.en.md), [memory contract](../MEMORY_AND_CONTINUITY.en.md) and [acceptance map](ACCEPTANCE_MAP.en.md). Inspect actual predecessor evidence before coding.

## 2. Owned scope and target files

Own `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, minimal `crates/harness-types/` and `crates/harness-cli/`, `schemas/`, `tests/fixtures/`, `tests/acceptance/registry.json`, `scripts/Verify-Phase.ps1`, and the initial Rust CI workflow. Scaffold other crate groups only when needed; record their future owner/path mapping. Add ignore rules for build output/local runtime data, without ignoring evidence needed in Git.

## 3. Contracts to settle before implementation

Set the binary name to `ha`. Specify ID/envelope/schema versions, canonical serialization/hash inputs, sequence bounds, source authority labels and typed error codes. Record the proposed CLI command tree and stdout JSON versus stderr logs. Pin a verified Rust toolchain and dependency set with per-dependency justification; check official compatibility and licensing before installation. Keep the Rust plugin SDK sketch as a design until its object-safety tests exist.

## 4. Ordered work items

### 4.1. P0-S01 — Inventory and freeze the P0 contract

Depends on: None.

Inspect repository instructions, upstream-derived decisions and installed tools. Write the bilingual P0 SPEC, dependency/setup inventory and explicit non-goals. Fix only contradictory contract wording with an explained revision.

Evidence: A reviewer can identify every planned environment change; no runtime capability is claimed.

### 4.2. P0-S02 — Create a minimal workspace

Depends on: P0-S01.

Create the workspace, shared types crate and CLI crate with `[[bin]] name = "ha"`. Implement only `--help`, `--version` and typed configuration parsing needed for contract tests. Pin toolchain and lockfile; no model network calls.

Evidence: `cargo check --workspace --locked` and CLI help run; unknown options/config fields fail clearly.

### 4.3. P0-S03 — Define durable contracts

Depends on: P0-S01, P0-S02.

Create versioned schema definitions for event envelope, WorkingState, instructions, tool receipts, memory versions, plugin manifest and context/composition packet. Keep domain ownership explicit even if early types share a module. Add representative valid/invalid serialization fixtures.

Evidence: Round trips preserve values; invalid IDs, versions, missing authority and malformed payloads are rejected.

### 4.4. P0-S04 — Create the canonical continuation fixture

Depends on: P0-S03.

Describe a task with three requirements, two completed steps, one failed check and one remaining step. Include superseded decision A→B, unclassified user text, duplicate input ID, unknown event and a deliberately truncated/corrupt artifact. Specify expected state independently of a future projector.

Evidence: Fixtures include source sequences, hashes and expected unresolved work; no model-generated test oracle.

### 4.5. P0-S05 — Build acceptance discovery and the phase runner

Depends on: P0-S02, P0-S03, P0-S04.

Create `phase_p0.rs`, test registry and `Verify-Phase.ps1`. Register all future C/K owners as not implemented. Require nonempty named test discovery and reject ignored required tests. Emit machine-readable gate results and fail on missing commands, unreadable artifacts or child-process failures.

Evidence: Negative controls for zero discovered tests, failing test, missing fixture and ignored required test fail the gate; P0 itself passes.

### 4.6. P0-S06 — Establish repeatable local and CI checks

Depends on: P0-S05.

Run fmt/clippy/tests/docs on the skeleton, then define Windows/Linux CI using the pinned toolchain and limited permissions. Keep online model smoke separate and disabled by default. Record actual local/platform tool versions.

Evidence: A clone can run the documented gate without credentials; unavailable platforms remain explicitly unverified until CI runs.

### 4.7. P0-S07 — Review and hand off the foundation

Depends on: P0-S01..P0-S06.

Review schema/test registry consistency, dependency justification and startup commands. Write evidence, unresolved ADRs and the real owner/path map for P1. Do not mark future C/K tests implemented simply because fixture JSON exists.

Evidence: P0 gate passes at the final revision and the next agent can reproduce it from the checkout.

## 5. Tests and verification commands

Required P0 tests: schema valid/invalid round trips, deterministic hash input, typed error rendering, CLI help/config rejection, fixture completeness and phase-runner negative controls. There are no primary C/K implementation claims yet. A stub deliberately reporting `not_implemented` is acceptable scaffolding but must never be counted as a passing future acceptance case.

Future phase commands, to run after implementing the targets:

```powershell
cargo test -p harness-cli --test phase_p0 --locked
pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P0
```

The full gate includes the handbook's formatting, clippy, workspace tests, test-discovery checks and docs checks. Do not report a filtered zero-test run as passing acceptance.

## 6. Demonstration rehearsal

1. Clone/open the checkout on a clean test environment and run the P0 gate.
2. Run `cargo run -p harness-cli --bin ha -- --help` with no API key.
3. Validate the canonical fixture; show two completed steps plus the unresolved failure/remaining step.
4. Run each gate negative control in an isolated fixture and observe nonzero failure.
5. State explicitly that fixture validation is not a persisted-session recovery demonstration.

## 7. Exit gate and forbidden shortcuts

All seven steps complete; the workspace builds, P0 tests execute, the gate detects known-bad inputs and schemas/fixtures are versioned. Do not add an LLM loop, SQLite runtime, memory extraction, orchestration or Web. Do not install broad global tooling without the assignment's setup authorization.

Do not advance phases on a summary alone. Bind results to the final tested revision, report missing checks, and preserve all predecessor regressions. Never alter fixture expectations merely to make implementation pass.

## 8. Delegation and handoff

An integrator owns root manifests/lockfile and schema naming. After S03, a separately assigned fixture author may prepare tests while the integrator builds the gate; merge only after IDs/fixtures agree. Handoff to P1 includes the continuation fixture, contract revisions, test-discovery format and exact build instructions.

Deliver `docs/evidence/P0.en.md` and `P0.vi.md`, plus a resumable handoff under `docs/handoffs/`, following the handbook. Include completed step IDs, pending failures, schema changes, commands and next action. Publication requires explicit authorization in the coding assignment.

## 9. Ready-to-use agent prompt

```text
Implement P0 only. Read docs/implementation/README.en.md and
P0_FOUNDATION.en.md in that directory, all linked architecture contracts,
and applicable repository instructions. P0 has no predecessor; verify the architecture baseline.
Create the phase SPEC, then implement P0-S01..P0-S07 in dependency order.
Stay within this phase's owned scope; preserve unrelated changes and accepted contracts.
Use real components for acceptance tests, with mocks only at appropriate external boundaries.
Run the P0 gate and existing docs checks; deliver bilingual evidence and a restart handoff.
Stop before the next phase. Do not spawn agents, commit, push or publish unless explicitly assigned.
If prerequisites or required verification are unavailable, report the exact gap without claiming completion.
```
