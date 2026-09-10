# Plugin architecture and contracts

English | [Tiếng Việt](PLUGIN_ARCHITECTURE.vi.md)

Revision 2 — September 10, 2026. Design only: the SDK, configuration and tests below are proposed, not implemented. Read alongside the [plan](RUST_HARNESS_PLAN.en.md) and [memory contract](MEMORY_AND_CONTINUITY.en.md).

## 1. What to take from DeepSeek

Research is pinned to DeepSeek commit `2377c272a8e839e0a84c9f0e623b867a1dce2014`. Its Cordis kernel supplies composition/lifetimes; agent capabilities live outside that kernel. Do not interpret “everything is a plugin” as “every plugin may bypass host invariants.” [Architecture](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/docs/architecture.md).

| Inspected mechanism | Implication for our Rust design |
|---|---|
| Plugins provide services; consumers declare `inject` requirements | Resolve service contracts independently of implementation packages |
| A required service disappearing unloads its consumers; its return permits reloading | Dependency tracking must continue beyond startup |
| Registrations are owned effects, including child plugins | Teardown needs ownership, cancellation and completion, not just registry removal |
| Stable configuration entry IDs distinguish updates from replacement | Separate plugin implementation identity from mounted instance identity |
| Scoped registries inherit definitions downward; ancestor listeners observe descendants | Specify registration lookup and event delivery separately |
| Service definition, provider and consumer can evolve independently | Separate process execution from its model-facing tool |

Sources: [Services](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/docs/cordis-tutorial/03-services.md), [Lifecycle](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/docs/cordis-tutorial/02-lifecycle-and-effects.md), [Composition](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/docs/cordis-tutorial/06-composition-and-hmr.md), [Scope](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/packages/core/scope/README.md), [Three roles](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/docs/user/develop/practice/index.md).

Important differences: Cordis can leave an unsatisfied consumer pending. Our production CLI rejects an incomplete required composition before accepting work. Cordis async disposers may run concurrently despite reverse start order; Rust shutdown explicitly awaits dependent phases. These are deliberate choices, not descriptions of identical implementations.

## 2. Minimal kernel and replaceable domain capabilities

Keep the kernel small: service registry, plugin instances, dependency graph, scope tree, resource ownership, lifecycle and diagnostics. It must not contain prompts, memory ranking or task semantics.

The trusted application composition supplies mandatory session durability, execution policy and recovery services. Those implementations can be replaced only by compatible trusted providers that satisfy the same contracts. Disabling required durability must fail validation; a disposable benchmark profile, if added later, must clearly advertise different guarantees.

| Contract/definition | Provider | Consumer |
|---|---|---|
| `ModelProvider` | DeepSeek, mock, future adapters | Agent loop; separately budgeted extractor |
| `ProcessRunner` | Windows Job Object runner; Linux runner | `run_process` tool, verifier |
| `FileSystem` | Local workspace; future sandbox adapter | Read/edit/search tools |
| `SessionStore` + transactional `StoreCoordinator` | SQLite | Runtime, task service, recovery |
| `AgentDriver` / agent factory | Default loop | CLI application service, orchestrator |
| `MemoryStore`, `MemoryRetriever`, `MemoryExtractor` | Local assets/FTS; future Tencent adapter | Context builder, memory tools, durable jobs |
| `Compactor`, `ContextContributor` | WorkingState-first compaction; skills/memory contributors | Context builder |
| `SubagentBackend`, `WorkspaceManager` | In-process agents, host-owned Git worktrees | Coordinator/task service |

An interface alone cannot guarantee a transaction across independent backends. The v1 `StoreCoordinator` owns the shared SQLite unit of work; plugins submit typed domain commands rather than independently committing related tables. A future remote store needs an explicit outbox/reconciliation contract, not a false cross-store atomicity claim.

Do not split every role into its own crate prematurely. Use the ten initial crate groups in the plan, adding modules for these contracts. Domain-specific types belong to their owning contract modules; `harness-types` contains only shared IDs/envelopes, avoiding a dependency dumping ground.

## 3. Manifest, identity and typed service lookup

Persist these distinctions:

- `plugin_id`: implementation family, for example `memory.local`.
- `implementation_version` and build/content digest: exact code identity.
- `instance_id`: stable configuration row ID; multiple instances may use one implementation.
- `scope_id`: host-created application/project/agent visibility boundary.
- `generation`: activation identity; stale callbacks and leases are rejected.
- `host_api_version`, provided/required service contract versions, config schema version.
- Requested capabilities, restart policy, and whether missing the plugin blocks recovery.
- Durable event schemas and projection versions owned by the implementation.

v1 resolves a compiled catalog of implementations. Typed service keys return trait-object handles with checked contract versions; consumers do not import a concrete provider or cast arbitrary JSON into a service. Implementation SemVer, host protocol version and event schema version are separate compatibility checks.

An illustrative SDK contract, not compilable SDK code:

```text
Plugin.describe() -> Manifest
Plugin.validate(config, available_contracts) -> ValidatedConfig
Plugin.mount(scoped_host, validated_config) -> OwnedResources
ServiceLease.call(request, cancellation) -> Result
OwnedResources.shutdown(deadline) -> ShutdownReport
ContextContributor.collect(read_only_snapshot, budget) -> ProposedBlocks
MemoryExtractor.extract(immutable_source_batch) -> ProposedMemoryMutations
```

Lifecycle futures must be cancellation-aware. In the implementation, choose object-safe boxed futures or another explicitly supported trait mechanism; do not assume every async trait can be put into `dyn` unchanged. No stable cross-binary Rust ABI is assumed.

## 4. Scope, inheritance and authority

```text
application
  project-A
    coordinator
      worker-1
      worker-2
  project-B
```

Three independent questions must have separate answers:

1. **Lookup:** which registered implementation is visible?
2. **Lifetime:** which owner must cancel and dispose it?
3. **Authority:** which operation may this actor perform?

Named lookup starts at the nearest scope and walks ancestors. Reject duplicate names within a layer. A child override requires an explicit configuration entry; removing it exposes the ancestor again. Registration undo targets an exact `(instance, generation, registration_id)`, so a late disposer cannot remove a replacement with the same name.

Siblings cannot discover each other's private registrations. Scoped activity can be observed by authorized ancestors, but event payloads still require data-access checks; a global observer is not an entitlement to read secrets or every memory asset. Child visibility does not make child contributions visible upward.

Permissions intersect across host, user, project and task/agent grants. A local override cannot erase an ancestor denial. Freeze the visible tool definitions for a model request and recheck effective authority immediately before side effects. Approval binds actor, invocation ID, resolved arguments, workspace revision, tool implementation and policy revision; a material change invalidates that approval.

DeepSeek's `ScopeKey` routes trusted in-process registrations and events; ordinary services do not become isolated merely because callers hold a scoped context. Rust uses explicit scoped APIs and host-created principals, not a clone of the whole service container handed to workers. [Scope limitations](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/packages/core/scope/README.md), [Layer implementation](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/packages/core/scope/src/store.ts).

Trusted built-in Rust code still executes with process privileges. Trait boundaries are maintainability boundaries, not protection against malicious native code. Untrusted plugins require an enforced process/OS boundary; stdio alone is not a sandbox.

## 5. Lifecycle and failure behavior

```text
declared -> validated -> waiting_dependencies -> initializing -> active
                                         failure -> failed
active -> draining -> stopped
stopped -> initializing (new generation, only if restart is permitted)
```

Startup validates the complete required graph: missing provider, contract mismatch, cycles, duplicate names and invalid config are errors. Optional memory extraction can be disabled without disabling the required journal/WorkingState. Optional dependency absence must have an explicit degradation path.

Mounting collects registrations/tasks/process handles in an unpublished resource set. Publish after successful initialization; rollback on failure removes that set and joins started work. Initialization must not make irreversible domain changes. Resource rollback cannot undo arbitrary file edits or network effects already performed.

On required service loss: stop new dependent calls, mark the provider unhealthy, drain/cancel affected consumers, then revoke registrations. `Arc` ownership alone is insufficient: a stale handle must fail its generation check. Ongoing tools record their settled or uncertain outcomes; remounting never implies permission to repeat a side effect.

Shutdown phases:

1. Stop accepting new turns, claims and plugin activations.
2. Signal agent/extractor cancellation; resolve approvals as canceled.
3. Drain requests and tool/process trees; persist outcomes or uncertainty.
4. Save transactional progress; leave unfinished background jobs recoverable.
5. Await consumer resource cleanup, then provider cleanup; close SQLite last.

Racing shutdown callers await the same completion. Cleanup is idempotent; collect failures rather than silently logging away storage errors. An expired deadline yields an incomplete shutdown report, not a clean-shutdown receipt. Abrupt termination remains recoverable through the journal.

For a hung in-process plugin, Rust cannot safely kill one arbitrary thread and preserve process correctness. Reject unsafe reload; pause affected work or terminate/restart the host under the recovery protocol. Process plugins can be terminated as a tree and their outstanding calls reconciled.

## 6. Hooks and immutable execution receipts

DeepSeek has different dispatch modes; notably its waterfall is around-middleware with `next`, not a simple fold over returned values. Do not implement every event as an asynchronous broadcast. [Dispatch semantics](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/docs/cordis-primer.md).

| Rust surface | Semantics | Failure rule |
|---|---|---|
| Durable domain command/event | One transactional owner; optimistic revision/fencing | Reject on failed commit; no success ACK |
| Pre-execution transform | Explicit ordered transform of a proposed call | Revalidate changed arguments before approval |
| Final guard | All applicable guards; deny or abstain | Any deny/error denies; no later allow override |
| Execution wrapper | Ordered `next`, at most once, cancellation-aware | Normalize failure; no generic side-effect retry |
| Result presentation | Derive model/UI view from execution receipt | Cannot turn denied/failed execution into observed success |
| UI/metrics observer | Bounded notification, replayable cursor where needed | Isolate observer failure; cannot alter outcome |

The host writes `ToolExecutionReceipt` separately from `ToolResultView`. The receipt records what actually ran and on which revision; presentation can truncate, redact or annotate the view but not rewrite execution history. Every resulting model-visible block is recorded before its next request. This deliberately makes the evidentiary boundary stricter than a freely replaceable post-hook result.

Publish settled-result observer notifications only after the receipt transaction commits. Isolate recoverable observer errors/panics at the task boundary; if the build aborts the process on panic, journal recovery still applies but live fault containment cannot be claimed. Do not use observers as the only persistence path.

All entry points—native tool calls, MCP, future code-mode nested calls and delegated tools—go through the same execution gate. Tools that invoke other tools retain parent invocation IDs and must not obtain broader child authority. DeepSeek provides a useful reference pipeline, but its hooks are not our completed Rust security proof. [Tool pipeline](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/docs/tool-execution-pipeline.md).

## 7. Configuration and safe changes

The proposed configuration merge order is built-in defaults → user config → explicitly trusted project config → selected profile → CLI overrides. Resolve rows by stable IDs, reject unknown fields and report each effective value's source. Permission fields use narrowing/intersection rules, not ordinary last-write-wins merging.

Repository configuration cannot load executables, expand capabilities or resolve arbitrary secret references without user-level trust. No JavaScript expression evaluation equivalent to `!!js` is needed in Rust v1. Credentials remain host-owned references resolved only by authorized providers.

Illustrative profile fragment, not an existing file format implementation:

```toml
schema_version = 1
extends = "builtin:personal-coding"

[[plugins]]
instance_id = "project-memory"
plugin_id = "memory.local"
scope = "project"
enabled = true

[plugins.config]
retrieval = "fts5"
supplemental_token_budget = 2000

[agent]
max_workers = 3
max_delegation_depth = 2
```

`extends` resolves a shipped, versioned bundle; the fragment is not the entire required composition. Include cycles and duplicate/conflicting row operations are errors.

Freeze a `CompositionSnapshot` at each applicable step: implementation digests, schema versions, effective nonsecret config, policy revision, tool schemas and contributor versions. `ha config explain` and `ha plugins inspect <instance-id>` expose dependencies, scope, state, active generation and restart requirements.

v1 change categories:

- Observability formatting: safe immediate change if no model-visible content changes.
- Retrieval limits/profile choices: next step boundary, recorded revision, bounded outstanding calls.
- Tool/model selection: drain current step; validate provider/tool compatibility before the next request.
- Store, schema, loop or required guards: stop/resume with migration/validation; no live replacement.

A revoked permission blocks the next not-yet-started action immediately and cancels in-flight work where possible. It cannot undo a side effect already completed. Do not wait until the next turn merely because ordinary configuration updates wait for a boundary.

## 8. Continuity across plugin changes

Memory and task progress belong to durable host storage, not plugin object fields. Unloading an extractor leaves its jobs and cursors intact. Reloading it does not recreate completed jobs or reset a stable agent profile.

Every durable event has `event_type`, `schema_version`, producer identity and classification as continuity-critical or optional telemetry. Required projectors/decoders belong to the supported host schema set. Preserve unknown events; an unknown critical event prevents execution resume and produces an actionable compatibility error. Optional telemetry may be skipped with an explicit diagnostic.

Snapshots record projector versions. Rebuild derived state from supported journal events when a projection version changes; do not mutate historical payloads silently. Exact historical replay uses stored request packets even if the original provider plugin is absent. Continuing execution requires a compatible composition or an explicitly recorded provider migration.

Memory plugins return candidate blocks/mutations. The context builder alone admits blocks, enforces provenance/budget, and freezes the model request. Plugins cannot inject additional text afterward. Tool definitions, skill versions, workspace instructions and memory versions all appear in the packet manifest.

## 9. External plugin protocol and extension tiers

v1 built-ins are compiled Rust; independently distributed tool plugins arrive at P6. A provider bridge uses the same transport with a separately versioned model/stream capability. Do not advertise support for an arbitrary external loop or storage backend before its contract exists.

Define a stdio JSON-RPC handshake with protocol range, plugin identity/digest, advertised capabilities and schema versions. Use newline-delimited UTF-8 JSON with bounded frame size; stdout is protocol only, stderr bounded logs. Negotiate method versions, stream chunk/terminal semantics, deadlines, max inflight calls and cancellation. Reject duplicate/unknown response IDs, malformed frames and output floods.

The host assigns call IDs, principal, scopes, workspace roots and grants. Never expose unrestricted host service lookup or arbitrary database queries. Plugin-requested permissions are requests, not granted authority. Run with a minimal environment and explicit executable digest/path; no implicit download or code execution while reading a repository profile.

Pin installed plugin versions locally. On EOF/crash, mark inflight calls uncertain where appropriate; restart only within a bounded policy. A reconnection does not authorize automatic retry of a mutating call. Capability health is inspectable without invoking a model.

## 10. Plugin acceptance cases

These are proposed tests, not passing results. Pair them with C01–C30 in the memory document.

| Test | Fixture | Required result |
|---|---|---|
| K01 | Missing required service, bad version or dependency cycle | CLI fails before input admission with dependency chain |
| K02 | Optional extractor absent | Coding resume works; extraction is visibly disabled |
| K03 | Mount fails after registering a tool and timer | Neither survives rollback; owned tasks joined |
| K04 | Worker overrides a tool; sibling resolves it | Correct nearest lookup; no sibling leak; duplicate same-layer name denied |
| K05 | Old generation disposer runs after replacement | New registration remains intact |
| K06 | Required provider disappears during a call | Admission stops; dependent cleanup and durable outcome/uncertainty |
| K07 | Two shutdown callers plus a failing async disposer | Shared completion, ordered phases, error reported, store closes last |
| K08 | Late allow follows ancestor deny; args change after approval | No execution; stale approval rejected |
| K09 | Observer panics or renderer reports success after denial | Receipt remains denied; observer failure cannot change task evidence |
| K10 | Config changes midway through a model/tool batch | Old frozen composition finishes/reconciles; next step records new revision |
| K11 | Unknown critical event or unsupported projector version | No silent history loss; read-only inspection available |
| K12 | Stdio malformed/oversized frame, wrong ID, crash or ignored cancel | Bounded resources; protocol error; no blind mutating retry |
| K13 | MCP/nested tool path attempts to bypass policy | Same gate, correlated child call and non-escalating grant |
| K14 | Repository profile requests executable loading or secrets | Rejected without explicit user-level trust; no secret in snapshots |

P0 defines the contracts; P1 implements K01–K07; P2/P3 add K08–K11; P6 adds K12–K14 and reruns the complete suite. Tests must exercise behavior, not only search for method names.
