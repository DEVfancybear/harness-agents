# Harness Agents — current product and architecture plan

English | [Tiếng Việt](HARNESS_MASTER_PLAN.vi.md)

**Status:** current-source plan, 29 September 2026. The product is no longer an empty scaffold: the workspace contains the runtime, durable store, tools, delegation, extensions, maintenance and CLI layers described below.

## 1. Product outcome

Provide a local foreground coding-agent host with durable sessions, bounded provider runs, policy-gated coding tools, recoverable task delegation, negotiated local extensions and operator-visible diagnostics. The host must favor inspectable evidence over optimistic model narration.

## 2. Current architecture

```text
operator -> harness-cli
              ├── harness-runtime -> harness-session -> harness-store-sqlite
              ├── harness-providers
              ├── harness-tools -> filesystem / Git / process adapters
              ├── harness-orchestrator
              ├── harness-extensions -> harness-kernel
              └── harness-maintenance
                         all share harness-types contracts
```

SQLite is the durable authority. CLI, UI and web adapters compose application services and must not become alternate domain writers. One writable host owns a data directory at a time.

## 3. Capability sequence

| Sequence | Capability | Current owner |
|---|---|---|
| P0 | Identity, contracts and fixtures | `harness-types`, CLI tests |
| P1 | Composition, storage and fencing | `harness-kernel`, `harness-store-sqlite` |
| P2 | Session, runtime, budgets and recovery | `harness-session`, `harness-runtime` |
| P3 | Coding tools, policy and receipts | `harness-tools` |
| P5 | Delegated tasks, workers and workspaces | `harness-orchestrator` |
| P6 | Extension/MCP negotiation and bounded transport | `harness-extensions` |
| P7 | Backup, migration, retention and release checks | `harness-maintenance` |
| P8 | Optional authenticated loopback Web adapter | `harness-cli` |

The sequence intentionally has no row for a removed subsystem. A role, skill or provider is not a new authority boundary.

## 4. Invariants

- raw input is durably admitted before its ACK;
- provider requests are frozen and budgeted;
- tool actions are canonicalized, policy-checked and recorded with receipts;
- uncertain external outcomes are visible and not silently replayed;
- task ownership and child delivery survive parent pause/restart;
- extension calls are versioned, schema-validated and bounded;
- backups/migrations expose manifest and compatibility evidence;
- unsupported platform or isolation claims fail closed.

## 5. User-facing modes

- interactive terminal/TUI or line mode;
- headless execution for scripts and automation;
- authenticated loopback Web projection over the same application services;
- maintenance commands for diagnostics, backup and migration.

All modes share the same admission, policy, persistence and recovery rules.

## 6. Delivery rules

Implement changes in the owning crate, add focused tests, update the bilingual runbook and then run the workspace gate. Do not revive a deleted phase by leaving a stale manifest row or link. Historical evidence remains tied to its source revision.

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest
```

## 7. Related documents

- [Architecture overview](ARCHITECTURE_OVERVIEW.en.md)
- [Architecture review](ARCHITECTURE_REVIEW.en.md)
- [Rust implementation plan](RUST_HARNESS_PLAN.en.md)
- [Plugin architecture](PLUGIN_ARCHITECTURE.en.md)
- [Implementation handbook](implementation/README.en.md)
- [Operator guide](OPERATOR_GUIDE.en.md)
