# Architecture review — current source

English | [Tiếng Việt](ARCHITECTURE_REVIEW.vi.md)

**Review date:** 29 September 2026. **Scope:** compare the architecture documents with the current Rust workspace. This is a review of boundaries and evidence, not an authorization to claim unsupported capabilities.

## 1. Findings

### A. The source tree has a layered local host

The workspace contains eleven crates: shared types, kernel composition, SQLite store, session, providers, runtime, tools, orchestrator, extensions, maintenance and CLI. The CLI composes these services; it is not the owner of every domain.

### B. Durable evidence is the central invariant

Input admission, run transitions, tool intents, policy decisions, provider attempts, task settlement and receipts cross a SQLite-backed durable boundary. Provider text is a proposal. Recovery must distinguish a committed result from an uncertain external effect and must not silently replay the latter.

### C. The extension boundary is real, but not a sandbox claim

Kernel registration/scopes/leases and extension handshake/capability negotiation are implemented contracts. Native same-process code remains trusted. Stdio, a child process and loopback transport bound communication; none alone proves hostile-code isolation.

### D. Delegation is a host-owned task system

The orchestrator persists task ownership, dependencies, worker budgets, handoffs and delivery. A role or skill name does not grant tool authority. Worker proposals still pass the normal host policy and receipt path.

## 2. Required invariants

- One writable host owns a data directory at a time.
- Required service composition fails closed before input admission.
- Durable ACK follows the authoritative write.
- Mutating tools require canonicalization, policy and approval when configured.
- Context, output, process capture, extension frames and delegation are bounded.
- UI and web adapters call application services instead of becoming database writers.
- Recovery can inspect or refuse an unsafe resume without a model extractor.

## 3. Current gaps and limits

- Windows 10/11 x64 is the supported platform; Linux is pending and macOS is untested.
- The product is a local foreground binary; no post-exit daemon or remote-worker service is claimed.
- Native extension code is trusted and is not an OS sandbox.
- Remote provider authentication and paid API behavior require provider-specific evidence.
- Historical phase documents are not current acceptance evidence unless their revision matches the checked source.

## 4. Verification required for changes

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest
```

Record the source revision, platform, command and relevant test counts. A green documentation checker verifies links and structure only; it does not prove runtime behavior.

## 5. Decision

Keep the current crate boundaries and durable-authority model. Update plans and runbooks to point at the eleven current crates. Do not reintroduce a removed domain owner through a documentation-only phase, facade or compatibility table.
