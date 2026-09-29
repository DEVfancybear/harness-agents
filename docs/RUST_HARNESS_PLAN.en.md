# Rust Harness Agents implementation plan

English | [Tiếng Việt](RUST_HARNESS_PLAN.vi.md)

**Status:** current implementation map, 29 September 2026. The workspace already contains the runtime and service boundaries described here. Remaining work must be verified against the current source; this document is no longer a pre-implementation promise.

Read the [architecture overview](ARCHITECTURE_OVERVIEW.en.md), [implementation handbook](implementation/README.en.md), [plugin architecture](PLUGIN_ARCHITECTURE.en.md) and [architecture review](ARCHITECTURE_REVIEW.en.md) together.

## 1. Product boundary

`ha` is a local, foreground coding-agent host. It accepts terminal/headless/web commands, persists session and run records in SQLite, calls a selected provider, and routes proposed coding actions through policy, approval and receipt gates. The host can delegate bounded tasks and mount negotiated local extensions. It does not claim daemon execution after exit, remote-worker durability or OS sandboxing merely from a transport boundary.

## 2. Current crate graph

```text
harness-types
  ├── harness-kernel
  ├── harness-store-sqlite -> harness-session
  ├── harness-providers
  ├── harness-runtime
  ├── harness-tools
  ├── harness-orchestrator
  ├── harness-extensions
  └── harness-maintenance
                         └── harness-cli composes the services
```

| Crate | Implementation responsibility |
|---|---|
| `harness-types` | IDs, schemas, contracts, errors and serialization vocabulary |
| `harness-kernel` | Service requirements, scopes, generations, leases and shutdown |
| `harness-store-sqlite` | SQLite opening/fencing, migrations, transactions, queues and artifacts |
| `harness-session` | Input admission, journal/projections, snapshots and recovery views |
| `harness-providers` | Provider request/response types, mocks and streaming adapters |
| `harness-runtime` | Run state machine, context admission, budgets and provider attempts |
| `harness-tools` | Canonicalization, policy, approvals, process/filesystem/Git execution and receipts |
| `harness-orchestrator` | Task DAGs, workers, ownership, handoffs, budgets and workspaces |
| `harness-extensions` | Handshake, capability negotiation, MCP-style operations and transport bounds |
| `harness-maintenance` | Backup/restore, compatibility, migrations, retention and diagnostics |
| `harness-cli` | Composition, terminal UI, headless commands and loopback web adapter |

## 3. Durable flow

```text
CLI input
  -> atomic session admission
  -> committed event/sequence ACK
  -> bounded context and frozen provider request
  -> provider stream/attempt record
  -> proposed answer or tool action
  -> policy + approval + durable intent
  -> execution receipt/artifact references
  -> projection, UI and recovery view
```

A provider response is a proposal. A durable ACK is returned only after the authoritative write. An uncertain external outcome stays visible and is not replayed by default.

## 4. Recovery and authority

The store is the durable authority, but each domain has one owner:

- session owns input admission and projections;
- runtime owns run lifecycle, budgets and provider attempts;
- tools own policy decisions, intents and receipts;
- orchestrator owns delegated task settlement and delivery;
- extensions own negotiated sessions and leases;
- maintenance owns backup, migration, retention and support diagnostics;
- CLI owns presentation and composition, not direct domain mutation.

Recovery folds committed snapshots and journal tails into a deterministic view. It can refuse an unsafe resume when an event/projector version is unknown, a lease is stale, a workspace changed or an external effect is uncertain.

## 5. Security and platform limits

The implementation must keep these boundaries explicit:

- required service composition fails closed;
- tool policy and approval precede mutating execution;
- output, context, process capture, extension frames and delegation are bounded;
- one writable host owns a data directory at a time;
- native same-process extensions are trusted code;
- subprocess/stdio is not automatically an OS sandbox;
- Windows 10/11 x64 is supported, Linux is pending, and macOS is untested.

## 6. Work sequence

| Sequence | Scope | Source anchor |
|---|---|---|
| P0 | IDs, foundation and fixtures | `harness-types`, CLI fixtures |
| P1 | kernel and durable storage | `harness-kernel`, `harness-store-sqlite` |
| P2 | runtime/session/recovery | `harness-runtime`, `harness-session` |
| P3 | coding tools and receipts | `harness-tools` |
| P5 | delegation and workspaces | `harness-orchestrator` |
| P6 | extensions and MCP | `harness-extensions` |
| P7 | maintenance and release evidence | `harness-maintenance` |
| P8 | optional loopback Web reuse | `harness-cli` |

There is intentionally no active phase for a removed service. New work must add a source owner, tests, a bilingual runbook and manifest entry in one change.

## 7. Verification

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest
```

Focused crate checks are useful during development, but release claims require the full workspace command plus platform, revision and artifact evidence.

## 8. Non-goals

This plan does not add a hosted service, background daemon, remote worker fleet, second database authority, unchecked provider-side effects or an unsupported platform claim. It also does not turn historical phase documents into current acceptance evidence.
