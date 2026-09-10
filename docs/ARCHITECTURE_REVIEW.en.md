# Architecture review — revision 2

English | [Tiếng Việt](ARCHITECTURE_REVIEW.vi.md)

September 10, 2026. Scope: re-review the proposed personal Rust multi-agent coding harness against DeepSeek plugin architecture and Tencent memory source, update both languages, and publish documentation. This is not authorization or evidence of runtime implementation.

## 1. Conclusion

Keep the core direction: a small plugin kernel, event-backed execution, structured WorkingState, scoped reusable memory, and CLI/Web over shared application services. The first plan was directionally sound but did not yet specify enough lifecycle, transaction, identity and retention behavior to guide implementation safely.

Revision 2 fills those gaps. The next engineering step is P0: concrete schemas and executable fixtures, followed by the smallest journal/restore slice. It is not yet appropriate to claim the product “never forgets”: preserving evidence can be tested; understanding every requirement is still model-dependent.

Read the [updated plan](RUST_HARNESS_PLAN.en.md), [plugin specification](PLUGIN_ARCHITECTURE.en.md), and [memory specification](MEMORY_AND_CONTINUITY.en.md).

## 2. Review findings and changes

Priority meanings: P0 = required in the initial contracts; P1 = required before the relevant feature ships, not necessarily in the first coding increment.

| ID | Gap or ambiguity in revision 1 | Revision-2 decision | Priority / acceptance |
|---|---|---|---|
| R01 | “Plugin = trait” did not describe full composition | Definition/provider/consumer roles; manifest, instance IDs, typed leases, compatible contracts | P0 / K01–K02 |
| R02 | Startup graph without dependency-loss semantics | Stop admission, drain dependents, revoke stale generations; ordered awaited shutdown | P0 / K03, K05–K07 |
| R03 | Scope mixed visibility, lifetime and security | Separate lookup/ownership/authority; nearest override; deny intersection; explicit trust | P0 / K04, K08, K14 |
| R04 | Config changes and plugin upgrades could change resume meaning | Composition snapshots, event/projector versions, explicit compatibility failure/migration | P0 / K10–K11 |
| R05 | A post-hook result could be mistaken for actual execution evidence | Immutable execution receipt separate from model/UI view; all paths share gate | P0 / K09, K13 |
| R06 | “WorkingState updated synchronously” hid semantic-classification risk | Save raw admitted instructions before ACK; mandatory instruction ledger and project rules outside top-k | P0 / C18–C19 |
| R07 | Only session ownership; new sessions could duplicate a task | Stable project/task/profile/run IDs; task and session fencing; one writable host per data directory | P0 / C15, C23–C24 |
| R08 | Durable jobs lacked exact cursor/settlement rules | Atomic source-work markers; non-overlapping oldest-first ranges, contiguous cursors, strategy versions | P0 / C12, C16–C17, C22 |
| R09 | Memory invalidation did not describe derived summaries | Dependency graph, request admission revision check, remove/rebuild affected derived blocks | P0 / C09, C13, C20 |
| R10 | Cross-agent “sharing” could become vague semantic coordination | Durable child result and parent delivery; task state authoritative; final-revision verification | P0 / C07, C11, C25 |
| R11 | Persistence lacked operational and deletion boundaries | Disk-full fail-stop; scoped artifacts; retention/tombstones; consistent backup and migration | P1 / C21, C27–C29 |
| R12 | Extraction, retrieval and extensions had unspecified failure/cost limits | Separate memory budget; inspectable backlog; bounded protocol; empty/degraded/error distinction | P1 / C05, C26, C30, K12 |

## 3. DeepSeek: verified mechanisms and deliberate differences

The reviewed repository HEAD still matched `2377c272a8e839e0a84c9f0e623b867a1dce2014` during this second inspection. The [public overview](https://deepseek.com/harness/en/) describes the composable direction; the implementation details below are grounded in the fixed source snapshot.

| Evidence | Observation | Rust decision |
|---|---|---|
| [Service tutorial](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/docs/cordis-tutorial/03-services.md) | Required services govern activation and dependent teardown | Track dependency health; reject missing required composition before accepting work |
| [Lifecycle tutorial](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/docs/cordis-tutorial/02-lifecycle-and-effects.md) | Effects own cleanup; async disposer completion needs care | Explicit phased shutdown, store closed last, failures retained |
| [Capability roles](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/docs/user/develop/practice/index.md) | Definition/provider/consumer split at swappable boundaries | Process runner independent of its tool; no unnecessary crate explosion |
| [Scope source](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/packages/core/scope/src/store.ts) | Ancestor layers merge with nearest named shadow; exact-entry undo | Registration IDs/generations and independent policy checks |
| [Composition tutorial](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/docs/cordis-tutorial/06-composition-and-hmr.md) | Stable entry IDs support config diffing/HMR | Stable instance IDs; v1 forbids live store/loop replacement |
| [Agent loop source](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/packages/core/agent-loop/src/index.ts) | Owned create/resume, persistence handle, lifecycle cleanup and interrupted-log repair | Preserve ownership model; distinguish protocol repair from unknown side-effect outcome |
| [Tool pipeline](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/docs/tool-execution-pipeline.md) | Pre hooks, monotonic guards, execution/post hooks, final observation | Keep execution evidence separate from rewritten presentation |

Existing DeepSeek tests were inspected as examples of contract organization, not executed or counted as proof for this repository. No claim is made that Rust can load Cordis plugins unchanged.

## 4. Tencent: verified mechanisms and applicability

The reviewed HEAD still matched `906b5823b5106eed8f842b62f16d23228838149a`, branch `feat/server_team`.

| Evidence | Observation | Rust decision |
|---|---|---|
| [Gateway](https://github.com/TencentCloud/TencentDB-Agent-Memory/blob/906b5823b5106eed8f842b62f16d23228838149a/MemoryCore/src/gateway/server.ts), [stateful manager](https://github.com/TencentCloud/TencentDB-Agent-Memory/blob/906b5823b5106eed8f842b62f16d23228838149a/MemoryCore/src/utils/stateful-pipeline-manager.ts) | Current wiring uses backend scheduling and Store-backed extraction, including backlog handling | Do not generalize legacy buffer recovery comments to the current product |
| [Local state backend](https://github.com/TencentCloud/TencentDB-Agent-Memory/blob/906b5823b5106eed8f842b62f16d23228838149a/MemoryCore/src/core/state/local-backend.ts), [backend factory](https://github.com/TencentCloud/TencentDB-Agent-Memory/blob/906b5823b5106eed8f842b62f16d23228838149a/MemoryCore/src/core/state/index.ts) | Local queue/claims/timers use process memory; Redis integration is separately loaded and may be unavailable in a build | Native SQLite jobs; no requirement for Redis to resume personal work |
| [Checkpoint](https://github.com/TencentCloud/TencentDB-Agent-Memory/blob/906b5823b5106eed8f842b62f16d23228838149a/MemoryCore/src/utils/checkpoint.ts), [worker](https://github.com/TencentCloud/TencentDB-Agent-Memory/blob/906b5823b5106eed8f842b62f16d23228838149a/MemoryCore/src/services/pipeline-worker.ts) | Distinct cursor/scheduling owners, serialized mutation and claim/lock checks | Domain write ownership and commit-time fencing; do not equate a mutex with a cross-file transaction |
| [L1 factory](https://github.com/TencentCloud/TencentDB-Agent-Memory/blob/906b5823b5106eed8f842b62f16d23228838149a/MemoryCore/src/utils/pipeline-factory.ts#L506) | Bounded oldest-first extraction; documented equal-timestamp page edge | Sequence-based paging and explicit contiguous progress tests |
| [Profile scope](https://github.com/TencentCloud/TencentDB-Agent-Memory/blob/906b5823b5106eed8f842b62f16d23228838149a/MemoryCore/src/core/profile/profile-scope.ts) | L2/L3 identity spans sessions; row lookup also checks isolation | Stable profiles plus project scope; task state remains separate |
| [Fixed assets](https://github.com/TencentCloud/TencentDB-Agent-Memory/blob/906b5823b5106eed8f842b62f16d23228838149a/MemoryProxy/src/injection/injectors/tdai-fixed-asset.ts), [profile injection](https://github.com/TencentCloud/TencentDB-Agent-Memory/blob/906b5823b5106eed8f842b62f16d23228838149a/MemoryProxy/src/injection/injectors/tdai-profile-memory-injector.ts) | Self/imported bindings; L3 plus L2 index; session-init caching in this path | Bounded retrieval with source versions and invalidation at request boundaries |
| [Dedup](https://github.com/TencentCloud/TencentDB-Agent-Memory/blob/906b5823b5106eed8f842b62f16d23228838149a/MemoryCore/src/core/record/l1-dedup.ts), [prompt resolver](https://github.com/TencentCloud/TencentDB-Agent-Memory/blob/906b5823b5106eed8f842b62f16d23228838149a/MemoryCore/src/core/memory-prompt/resolver.ts) | Scoped candidate matching and versioned extraction strategy | CAS merge proposals; immutable source batches; explicit strategy generations |

We did not run Tencent, its integration tests or benchmarks. This is not an audit proving Tencent data loss or security flaws. The lesson is to define guarantees at actual storage, request and execution boundaries, rather than infer them from layer names.

## 5. Non-negotiable behavior and remaining limits

The implementation must preserve acknowledged inputs/results, reconstruct current work without the extractor, and refuse to invent evidence or repeat uncertain side effects. Mandatory current requirements survive independently of optional memory search. Multiple agents coordinate through durable task messages, not memory similarity.

Limits to keep explicit:

- No finite model context guarantees perfect reasoning or retention of arbitrarily large requirements; overflow of mandatory context pauses execution.
- Same-process plugins are trusted native code. Worktrees/traits/stdio do not by themselves isolate malicious code.
- Deleting source payloads or losing the storage device can remove continuity evidence. Retention and backups are part of the product contract.
- One writable host per data directory in v1; background work stops when that host exits.
- Schema migrations, dirty-worktree snapshots, sandbox enforcement and crash recovery are designs awaiting tests, not existing capabilities.
- Automatic embeddings, complete Wiki/CodeGraph, remote agents, marketplace, Wasm and daemon mode remain deferred.

## 6. Delivery impact and review verification

The expanded contracts increase the planning estimate to 53–76 person-days before contingency, approximately 13–20 working weeks for one experienced Rust engineer with 20–30% contingency. Web remains a separate 10–15-person-day increment. The milestone table in the plan is authoritative for scope and arithmetic.

The documentation set has four English/Vietnamese pairs, 30 proposed continuity cases and 14 proposed plugin cases. The repository also includes a documentation checker and a documentation-only GitHub workflow.

Run from the repository root with PowerShell 7:

```powershell
pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest
```

The checker validates local file links, balanced fences, paired numbered headings, exact acceptance IDs, milestone totals and fixed upstream commit references. Its negative controls exercise missing translation, broken link, unclosed fence, missing case, changed estimate and unpinned source failures. It does not validate Markdown rendering, remote-link availability, translation semantics or runtime behavior. Manual source/translation review complements these checks; no independent-agent verification was performed.

Local review result: the command above passed on PowerShell 7.6.5 for nine Markdown files/four language pairs; all six known-bad controls were rejected as expected. Separately, all 33 unique cited upstream blob paths were checked against the pinned Git objects. These checks establish document structure and source-path existence, not correctness of every design claim or runtime guarantee.

Before committing, also review the staged diff, run `git diff --cached --check`, and inspect for credentials/unrelated files. Delivery evidence is the published commit and its documentation-workflow conclusion. Runtime tests, coverage, mutation, sandbox and model evaluations remain **not performed: no runtime exists**. A green documentation workflow must not be presented as a working harness.
