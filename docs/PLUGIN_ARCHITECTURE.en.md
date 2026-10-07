# Plugin and extension architecture

English | [Tiếng Việt](PLUGIN_ARCHITECTURE.vi.md)

**Status:** current kernel/extension contract, 7 October 2026. `harness-kernel` and `harness-extensions` are implemented workspace crates; this document describes their boundaries, not a proposed SDK.

## 1. Ownership

`harness-kernel` owns in-process composition. It validates service requirements, registers values in scopes, tracks generations and leases, and shuts resources down in dependency order. `harness-extensions` owns negotiated extension sessions, MCP-style discovery/calls/tasks, skill composition and bounded transports. `harness-cli` composes these services and owns the user-facing surfaces built on them (`ha mcp`, `ha package`, `ha extensions`, `/plugins`, skill and prompt-template loading) but does not become a second authority.

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

Configuration resolves before composition. The CLI can mount local extension processes (NDJSON over stdio) and MCP servers over stdio or Streamable HTTP. HTTP MCP requires TLS (cleartext only on loopback) and reads bearer credentials from the environment or from an OAuth sign-in (section 7). The current release matrix is authoritative for platform support. Native-code isolation and OS sandbox claims require separate evidence and are not implied by this contract.

Useful checks include:

```powershell
cargo test -p harness-kernel --locked
cargo test -p harness-extensions --locked
cargo test -p harness-cli --test phase_p6 --locked
pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest
```

## 7. What the CLI composes on top

These surfaces live in `harness-cli`; every tool they expose still goes through the host policy gate and approval path described above.

- **MCP servers.** `ha mcp add|list|get|remove|login|logout` and `/mcp` manage user-configured servers (`crates/harness-cli/src/mcp_cli.rs`, `interactive/mcp*.rs`). MCP calls are dispatched through `McpToolDispatcher`; the raw client call is not reachable from other crates.
- **MCP OAuth.** `ha mcp login <name>` signs in to a Streamable HTTP server: protected-resource and authorization-server metadata discovery (RFC 9728 / 8414), dynamic client registration (RFC 7591) unless a client id is configured, and a PKCE authorization-code flow against a loopback callback on ports 53700-53709 (or a pasted redirect URL). Tokens are stored in `auth.json` under `mcp:<server>`, bound to the server URL and token endpoint, and refreshed before expiry. `ha mcp logout` forgets them.
- **MCP service catalog and `/plugins`.** `/plugins [search]` browses and connects external services. The catalog resolves per id: compiled built-ins, the user's `mcp-services.json`, then a public catalog refreshed daily (a bundled snapshot serves until then). Cards show connection state and never a secret.
- **Skills.** Skill roots are bundled skills, the user config directory, `~/.agents/skills`, `HA_SKILL_PATHS` and, only for a trusted project, `.harness/skills` and `.agents/skills` up to the repository root. Each source carries a trust level; a skill is instructions, not authority. Skills ha learned itself (`/refine`, `interactive/learned.rs`) are plain `SKILL.md` folders with a `.ha-learned.json` marker; the model never writes them directly, and a single promotion path lints and lands them.
- **Packages.** `ha package install|remove|update|list` installs packages of skills, prompt templates and themes from `npm:`, `git:`/URL or local-path sources into user or project (`--local`) settings (`interactive/packages/`).
- **Prompt templates.** Prompt commands load from the `prompts` settings arrays, `.harness/prompts`, the config directory's `prompts/` and package prompts, limited to 256 files and 256 KiB each.
- **Hooks.** Configured command hooks run for `pre_tool_use`, `post_tool_use`, `stop`, `subagent_stop`, `notification`, `user_prompt_submit`, `session_start`, `session_end` and `pre_compact`; any other event name is a config error.

## 8. Review checklist

- Are required services validated before input admission?
- Does each registration carry a scope and generation?
- Are duplicate, stale-generation and cycle cases rejected?
- Does every extension call have bounded input, output, timeout and cancellation?
- Do nested tool calls re-enter policy instead of inheriting an unchecked grant?
- Are shutdown order and uncertain outcomes recorded?
- Is any isolation claim backed by a platform-specific capability result?
