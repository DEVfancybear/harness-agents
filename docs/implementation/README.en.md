# Implementation handbook

English | [Tiếng Việt](README.vi.md)

**Status:** current implementation map, 29 September 2026. This handbook follows the source tree and executable gates. It is not a promise that every planned item is complete.

## 1. Source of truth

Use the [architecture overview](../ARCHITECTURE_OVERVIEW.en.md) for current boundaries, the [plugin architecture](../PLUGIN_ARCHITECTURE.en.md) for kernel/extension contracts, and the source/tests for behavior. The phase runbooks below describe ownership and verification; they do not create capabilities absent from the workspace.

When a runbook conflicts with Rust code, `Cargo.toml`, or a test, report the conflict and follow current source. Historical evidence is revision-bound and must not be reused as a fresh release claim.

## 2. Current workspace

The workspace has eleven Rust crates:

- `harness-types` — shared IDs, contracts, errors and schemas;
- `harness-kernel` — composition, scoped registration, leases and shutdown;
- `harness-store-sqlite` — SQLite authority, migrations and durable records;
- `harness-session` — admission, projections, snapshots and recovery views;
- `harness-providers` — provider adapters, mock provider and streaming;
- `harness-runtime` — run lifecycle, budgets and context admission;
- `harness-tools` — policy, approvals, filesystem/Git/process tools and receipts;
- `harness-orchestrator` — delegated tasks, workers, DAGs and workspaces;
- `harness-extensions` — MCP/extension negotiation and bounded transport;
- `harness-maintenance` — backup, migration, retention and diagnostics;
- `harness-cli` — `ha` composition, terminal UI, headless mode and loopback web.

## 3. Phase map

The active sequence follows executable test targets and current crate boundaries; removed capabilities are not implementation dependencies.

| Phase | Scope | Depends on | Estimate | Current source anchor |
|---|---|---|---:|---|
| P0 | Foundation, IDs and test fixtures | — | 3–4 | `harness-types`, CLI fixtures |
| P1 | Kernel, storage ownership and durable records | P0 | 8–11 | `harness-kernel`, `harness-store-sqlite` |
| P2 | Runtime, context admission and recovery | P1 | 7–10 | `harness-runtime`, `harness-session` |
| P3 | Coding tools, policy and receipts | P2 | 7–10 | `harness-tools` |
| P5 | Delegation, task DAG and isolated workspaces | P3 | 8–12 | `harness-orchestrator` |
| P6 | Extensions, skills and MCP | P5 | 5–8 | `harness-extensions` |
| P7 | Backup, migration, release and support evidence | P6 | 8–11 | `harness-maintenance`, CLI release checks |
| P8 | Loopback Web adapter | P7 | 10–15 | optional `harness-cli` web surface |

P0–P7 planning range: **46–66 person-days**. P8 remains optional. The absent phase row is intentional: a deleted subsystem must not remain as a false implementation dependency.

## 4. Execution rules

1. Read current source and predecessor evidence before changing a phase.
2. Keep one durable authority per domain. Do not add a second SQLite writer through a facade.
3. Treat provider output as a proposal; only host policy and receipts establish execution evidence.
4. Preserve sequence, fencing, budget, workspace and recovery invariants when adding a UI or extension path.
5. Stop at the phase boundary. Do not report a filtered or empty test selection as acceptance.

## 5. Verification commands

From the repository root:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest
```

For a phase with an executable target, run its focused gate and then the full workspace gate. A result is source-bound only when command, revision, platform and count are recorded together.

## 6. Evidence and recovery

The current Git revision and command output are the source for verification. Recovery must use committed store records, snapshots and receipts; prose cannot replace an execution record.

## 7. Adding a phase or crate

Before adding a phase, update the source map, dependency graph, bilingual runbook, manifest, acceptance ownership and docs verifier in one change. Before adding a crate, state its authority, inputs, outputs and shutdown behavior. A crate that only forwards calls must not become a new domain owner.

## 8. Review checklist

- Does the change match the current crate map?
- Is every side effect preceded by policy and, where required, approval?
- Is the durable ACK emitted only after the authoritative write?
- Are uncertain external outcomes visible and non-replayed by default?
- Are Windows support claims separated from Linux/macOS pending evidence?
- Do English and Vietnamese headings, links and command examples stay aligned?

## 9. Related documents

- [Architecture overview](../ARCHITECTURE_OVERVIEW.en.md)
- [Architecture review](../ARCHITECTURE_REVIEW.en.md)
- [Plugin architecture](../PLUGIN_ARCHITECTURE.en.md)
- [Acceptance map](ACCEPTANCE_MAP.en.md)
- [Operator guide](../OPERATOR_GUIDE.en.md)
- [Build and release](../BUILD_AND_RELEASE.md)
