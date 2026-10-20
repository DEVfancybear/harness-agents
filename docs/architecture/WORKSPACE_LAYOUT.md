# Workspace folder layout

This is a directory-only refactor. Package names, public Rust module paths, binary names, commands, data locations and dependency edges are unchanged. It does not add an app or extract a new shared-host API.

## Repository

```text
crates/                    Existing Rust packages, kept as flat peers
  harness-cli/             ha composition root and host adapters
  harness-types/           Shared contracts and identifiers
  harness-kernel/          Plugin graph and service lifecycle
  harness-store-sqlite/    Durable storage
  harness-session/         Admission, projections and context
  harness-providers/       Provider protocols
  harness-runtime/         Run lifecycle and budgets
  harness-tools/           Coding tools and execution
  harness-orchestrator/    Delegation and worktree integration
  harness-extensions/      Extension protocols and composition
  harness-maintenance/     Backup, migration and retention
scripts/
  common/                  Shared script helpers
  install/                 Local installation
  release/                 Release bundles and npm packaging
  testing/                 Live, PTY and provider test tools
  verify/                  Repository gates
docs/
  architecture/            Architecture and folder guides
  guides/                  Operator, TUI, build and computer-use guides
  adr/                     Architecture decisions
  support/                 Platform support notes
  assets/                  Documentation images
```

`.agents/skills`, `e2e/`, `npm/`, `schemas/` and `tests/fixtures` retain their established paths. Root Cargo manifests and the lockfile stay in place. A cohesive package does not need extra folders merely to match a larger package.

## CLI host

`crates/harness-cli/src/main.rs` remains the executable entrypoint. Command handlers live in `src/commands/`; support reporting lives in `src/support/`. The old `src/interactive/` implementation now lives under `src/host/`:

| Folder | Responsibility |
|---|---|
| `application/` | Startup, session service, controller, lifecycle and verification |
| `configuration/` | Configuration, permissions, paths and resource settings |
| `coordination/` | Agents, delegation, autonomous runs, goals and scheduling |
| `integrations/` | Extensions, MCP, packages, Python REPL, skills and web tools |
| `memory/` | Continual harness, knowledge, task state, learned skills and checks |
| `models/` | Model catalog, credentials, routing and service tiers |
| `presentation/` | TUI, terminal input, views, attachments and rendering |
| `prompting/` | Instructions and bundled prompt assets |
| `protocols/` | CLI routing, headless mode, RPC and ACP |

The physical folder is named `host` because it contains more than terminal presentation. Explicit `#[path]` module declarations intentionally preserve `crate::interactive::*`, existing `super::` imports and test selectors. These are file-location mappings, not compatibility wrappers or new runtime layers.

## Library source

Library roots remain `src/lib.rs`. The source groups expose no new Rust namespace:

- `harness-types`: `domain/`, `contracts/`, `validation/`; cross-cutting errors and diagnostics remain at root.
- `harness-tools`: `filesystem/`, `processes/`, `security/`, `runtime/`; strict-execution code stays in `execution/`.
- `harness-providers`: `protocols/`, `support/`.
- `harness-runtime`: `control/`, `persistence/`.
- `harness-extensions`: `registry/`, `protocols/`, `composition/`; host and config remain at root.
- `harness-orchestrator`: `coordination/`, `workspaces/`.
- `harness-maintenance`: `data/`, `reporting/`.
- `harness-store-sqlite`: `records/`; the existing `store/` subtree now has `mod.rs` as its root.
- `harness-session` and `harness-kernel`: their already-small source layouts are unchanged.

## Development

Run gates from the repository root with their new paths:

```powershell
pwsh -NoProfile -File scripts/verify/Verify-Phase.ps1 -Phase P7
pwsh -NoProfile -File scripts/verify/Verify-Docs.ps1 -SelfTest
```

Use the same Cargo package and test-target names as before. The refactor moves compile-time include paths together with their owners; bundled prompt/catalog/assets and Python source payloads are not rewritten.

## Reference and future work

Prime Agent's [crates](https://github.com/PrimeIntellect-ai/prime-agent/tree/main/crates) and [workspace manifest](https://github.com/PrimeIntellect-ai/prime-agent/blob/main/Cargo.toml) keep CLI, TUI, daemon and shared responsibilities in separate flat packages. This layout takes the responsibility-based organization without copying package names or changing our existing crate graph.

A future app can use this map to identify the host, presentation and shared-library code. Extracting presentation-independent host interfaces is a separate API change, not something this folder move claims to accomplish.
