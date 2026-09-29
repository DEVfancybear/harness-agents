# Plugin and extension architecture

English | [Tiếng Việt](PLUGIN_ARCHITECTURE.vi.md)

**Status:** current kernel/extension contract, 29 September 2026. `harness-kernel` and `harness-extensions` are implemented workspace crates; this document describes their boundaries, not a proposed SDK.

## 1. Ownership

`harness-kernel` owns in-process composition. It validates service requirements, registers values in scopes, tracks generations and leases, and shuts resources down in dependency order. `harness-extensions` owns negotiated extension sessions, MCP-style discovery/calls/tasks, skill composition and bounded transports. `harness-cli` composes these services but does not become a second authority.

```text
harness-cli
  -> harness-extensions -> harness-kernel -> harness-types
  -> harness-tools     -> harness-store-sqlite
  -> harness-runtime   -> harness-session
```

The host remains the authority for input admission, tool policy, approval, workspace scope, budgets and receipts. An extension may propose a tool or resource operation; it cannot grant itself host authority.

## 2. Kernel contract

A service requirement has a stable name, contract generation and required/optional mode. Composition must reject:

- a missing required service;
- an incompatible contract generation;
- duplicate registrations in one scope;
- cycles in the requirement graph;
- a stale generation attempting to unregister a replacement.

Scopes provide nearest-registration lookup and sibling isolation. A plugin receives owned handles for the resources it mounted. Unmount cancels owned tasks, waits for disposers in reverse dependency order and reports failures without silently skipping the remaining cleanup. The store closes last.

Kernel composition is not an OS sandbox. Same-process native code remains trusted code; a subprocess or stdio transport is only a process/transport boundary.

## 3. Extension handshake

Every extension session follows this order:

```text
spawn/connect
  -> protocol and version negotiation
  -> capability and limit negotiation
  -> schema validation
  -> discovery (tools/resources/prompts/tasks)
  -> bounded invocation
  -> result/error/cancellation settlement
  -> lease release and shutdown
```

The host records the negotiated protocol generation, extension identity, capabilities and limits before exposing operations. Unknown critical capabilities, malformed arguments, oversized frames, invalid IDs and unsupported protocol versions fail closed.

The adapter bounds frame size, discovery pages, call arguments, result capture, task lifetime and cancellation grace. A timeout or broken transport produces an explicit failure/uncertain outcome; it does not blindly replay a mutating call.

## 4. Capability model

| Capability | Host requirement | Extension responsibility |
|---|---|---|
| Tool | Host policy and approval before execution | Declare input schema and return bounded result/error |
| Resource | Host scope and read policy | Return a stable URI/payload within negotiated limits |
| Prompt/template | Host selects context and provider budget | Declare variables and render deterministic content |
| Task | Host owns lifecycle, cancellation and receipt | Report progress/result without claiming host acceptance |
| Skill | Host pins source/version and applies skill policy | Provide bounded instructions/resources; no implicit authority |
| Transport | Host selects allowed local/loopback transport | Respect frame, timeout and cancellation limits |

A role, skill name, server name or tool name is not an approval. Nested calls re-enter the same host gate and keep the original scope and correlation identifiers.

## 5. Lifecycle and failure handling

Mounting is transactional: validate requirements, allocate resources, register handles, then expose the plugin. If a later step fails, rollback removes only the generation created by that mount and cancels its owned tasks. Unmount is idempotent and observable.

During an active provider/tool call, removing a required service stops new admission, drains dependents and persists success, failure or uncertainty before closing storage. A plugin cannot keep a hidden task alive after its lease expires.

Extension results are evidence only when the host has persisted the corresponding command, policy decision and receipt. Presentation code may truncate output but must not rewrite the durable record.

## 6. Configuration and local operation

Configuration resolves before composition. The CLI can mount local extension processes and loopback integrations; the current release matrix is authoritative for platform support. Remote services, native-code isolation and OS sandbox claims require separate evidence and are not implied by this contract.

Useful checks include:

```powershell
cargo test -p harness-kernel --locked
cargo test -p harness-extensions --locked
cargo test -p harness-cli --test phase_p6 --locked
pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest
```

## 7. Review checklist

- Are required services validated before input admission?
- Does each registration carry a scope and generation?
- Are duplicate, stale-generation and cycle cases rejected?
- Does every extension call have bounded input, output, timeout and cancellation?
- Do nested tool calls re-enter policy instead of inheriting an unchecked grant?
- Are shutdown order and uncertain outcomes recorded?
- Is any isolation claim backed by a platform-specific capability result?
