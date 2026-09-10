# Implementation acceptance and ownership map

English | [Tiếng Việt](ACCEPTANCE_MAP.vi.md)

## 1. How to read this map

C01–C30 and K01–K14 retain their definitions in the [memory contract](../MEMORY_AND_CONTINUITY.en.md) and [plugin contract](../PLUGIN_ARCHITECTURE.en.md). This table assigns **one phase responsible for the first component proof** of each case, not a replacement acceptance condition. P0 creates fixtures/test registry; it cannot claim the 44 runtime cases ran.

“Strengthen” means repeat with downstream components or a more complete integration. P1 uses controlled receipts and synthetic principals before real process tools/agents exist; P3/P5 must add real proof at those boundaries. P7 runs all 44 cases against the integrated runtime. P8 repeats those regressions and adds six Web cases. Every case here is currently **not implemented**.

Primary ownership is recorded in the [manifest](manifest.json); detailed steps are in the [phase runbooks](README.en.md). A case may need multiple test functions and fault windows. Do not force it into one function while dropping half its conditions.

## 2. Continuity C01–C30

| Case | Owning phase | Required fixture and result | Strengthen through P7 |
|---|---|---|---|
| C01 | P1 | Kill immediately after durable input ACK; reopen yields the same input ID once. | P2, P7 |
| C02 | P1 | Commit a result, kill before snapshot, fold the tail without scheduling that invocation again. P1 uses a controlled receipt; P3 adds a real tool. | P2, P3, P7 |
| C03 | P3 | Perform a disposable side effect, kill before receipt commit, then detect uncertainty and reconcile before retry. | P5, P6, P7 |
| C04 | P2 | Force five compactions of the canonical task; assert objectives, corrections, decisions, failures and pending work in each next request. | P7 |
| C05 | P4 | Fail summary and optional embedding/extraction boundaries; resume still uses WorkingState. Retryable jobs remain durable; no production embedding feature is required for this failure fixture. | P7 |
| C06 | P2 | Open the same task from a new session; retain checkpoint, artifact scope and remaining steps; exclude another task's data. | P7 |
| C07 | P4 | Three host-created principals publish concurrently: immutable versions, explicit CAS conflict, separate cursors. P4 exercises the memory API; P5 repeats with real agents. | P5, P7 |
| C08 | P4 | Forge actor/project/scope IDs and read/export an asset directly; host authority, not caller parameters, decides visibility. | P5, P6, P7 |
| C09 | P2 | Record explicit decision A then replacement B; mandatory context uses B and marks A superseded through resume and compaction. | P4, P7 |
| C10 | P3 | Change tracked or dirty files outside the host; compare meaningful workspace fingerprints, stale receipts and code facts before reuse. | P5, P7 |
| C11 | P5 | Stop the parent around child completion; recovered coordinator consumes the durable child result without repeating completed work. | P7 |
| C12 | P4 | Inject crashes around extraction asset/job/cursor settlement; retry produces no duplicate version and no cursor/outcome split. | P7 |
| C13 | P4 | Reuse the same source through ten context injections; lineage prevents injected copies being counted as independent observations. | P7 |
| C14 | P2 | Replay persisted sanitized packets with provider and tools disabled; include chunk/format and version fixtures without new execution. | P6, P7 |
| C15 | P1 | Race two host processes for one session and attempt a stale commit after takeover; only the current fencing generation writes. | P2, P5, P7 |
| C16 | P4 | Use more than two pages of same-timestamp events; sequence cursor consumes every record without skipping page siblings. | P7 |
| C17 | P4 | Attempt to settle a later extraction range before a gap; contiguous watermark stays behind the gap, which remains visible. Serial v1 still tests this invalid completion. | P7 |
| C18 | P2 | Leave an admitted instruction unclassified, then compact and reopen; original sanitized instruction remains mandatory. | P4, P7 |
| C19 | P2 | Give a required project rule zero retrieval relevance; mandatory admission includes it independently of top-k. | P4, P7 |
| C20 | P4 | Revoke a source used by a cached summary or packet candidate; exclude/rebuild dependencies and recheck before dispatch, with scoped audit. | P6, P7 |
| C21 | P1 | Inject storage exhaustion at input, intent and result commits, not by filling the user's disk. P1 proves transaction behavior; P3 proves no unjournaled new tool effect and uncertain-result recovery. | P3, P7 |
| C22 | P4 | Unavailable extractor, changed strategy/model schema and invalid JSON cannot advance cursor or promote policy; job records expose retryable/blocked outcomes. | P7 |
| C23 | P3 | Compare separate clones, moved roots and linked worktrees with similar remotes/paths; explicit project IDs prevent accidental task/memory merge. | P5, P7 |
| C24 | P1 | Two sessions contend for one task continuation; task lease and fencing prevent simultaneous owners even when session IDs differ. | P2, P5, P7 |
| C25 | P5 | Commit child result then miss parent notification; durable inbox/outbox replay delivers once logically and references the original result. | P7 |
| C26 | P4 | Query empty FTS, Vietnamese text and code identifiers; simulate timeout of an optional embedding boundary. Report empty/degraded/error distinctly without invented memory. | P7 |
| C27 | P7 | Restore a consistent DB/artifact backup to a fresh directory, migrate supported versions and reject unsupported old writers without mutation. | P7 |
| C28 | P7 | Forget a source and resume its old task; tombstones block re-extraction, dependent content is unavailable, and missing evidence is disclosed. | P7 |
| C29 | P3 | Use a fake secret fixture and another scope's artifact hash; capture/export redaction and independent object authorization both hold. | P4, P6, P7 |
| C30 | P4 | Exhaust a memory-job budget and exit during extraction; foreground state is safe, durable job pauses and resumes under a fresh finite budget. | P7 |

## 3. Plugin K01–K14

| Case | Owning phase | Required fixture and result | Strengthen through P7 |
|---|---|---|---|
| K01 | P1 | Reject missing services, incompatible contracts and cycles before admitting input; error includes the dependency chain. | P6, P7 |
| K02 | P1 | Omit the optional extractor without breaking required composition. P1 proves mounting; P2/P4 prove coding resume and visible disabled state. | P2, P4, P7 |
| K03 | P1 | Fail mount after tool/timer registration; rollback removes handles, cancels and joins owned tasks. | P6, P7 |
| K04 | P1 | Override a tool in one child scope; nearest lookup, sibling isolation and same-layer duplicate rejection hold. P5 uses actual workers. | P5, P7 |
| K05 | P1 | Run an old generation disposer after replacement; it cannot remove the new generation's registration. | P6, P7 |
| K06 | P1 | Remove a required provider during an active call; stop admission, drain dependents and persist result or uncertainty before closing storage. | P3, P5, P7 |
| K07 | P1 | Race two shutdown callers and fail an async disposer; share completion, await dependency order, report error, close store last. | P3, P5, P7 |
| K08 | P3 | Ancestor deny survives a later allow; changed arguments/workspace/policy invalidate approval before execution. | P6, P7 |
| K09 | P3 | Panic an observer or render success after denial; immutable receipt and task evidence remain denied. | P7 |
| K10 | P2 | Change configuration mid-request/batch; frozen composition finishes or reconciles, next step records the new revision. | P4, P6, P7 |
| K11 | P2 | Encounter unknown critical event or unsupported projector version; refuse unsafe resume, retain read-only inspection and history. | P6, P7 |
| K12 | P6 | Send malformed/oversized stdio frames, wrong IDs, crash or ignore cancellation; bound resource use and never blindly retry a mutating invocation. | P7 |
| K13 | P6 | Invoke MCP or a nested tool attempting to bypass policy; the same host gate verifies correlated calls and non-escalating grants. | P7 |
| K14 | P6 | Request executable loading or secrets from a repository profile; require explicit user-level trust and redact snapshots. | P7 |

## 4. Additional Web W01–W06

These belong only to optional P8 and are not counted in the 44 CLI cases. They extend the implementation specification for Web UI reuse of host services.

| Case | Owning phase | Required fixture and result |
|---|---|---|
| W01 | P8 | Same task through CLI and Web services: identical authoritative IDs, receipts, scope and pending steps; no direct adapter SQL writes. |
| W02 | P8 | Disconnect/reconnect with a retained cursor recovers visible events once logically; expired cursor asks for a resnapshot, no extra model dispatch. |
| W03 | P8 | Retry the same POST command ID after losing its response; one logical admission. A stale approval is rejected before tool execution. |
| W04 | P8 | Missing/expired auth, wrong origin, CSRF and cross-scope IDs/hash reads are denied; revocation also affects active streams without private leakage. |
| W05 | P8 | Close/reopen browser while the foreground host runs: same task continues. A second writer cannot acquire that data directory. |
| W06 | P8 | Render hostile model/tool/memory HTML and dangerous links safely; fake credentials do not appear in page source, event payloads or logs. |

## 5. Registry, gates and evidence

P0 creates `tests/acceptance/registry.json`, mapping IDs to test targets/names, fixtures, platforms and readiness. This runbook does not create a runtime registry or fake tests. At phase N, the gate must require every case with a primary phase no later than N, N's strengthening cases and all accepted regressions. For P0, run separate foundation tests; all C/K remain `not_implemented`.

The runner checks exact test discovery, executed counts, ignored/skip reasons and platform coverage. A required case missing implementation or execution fails the gate; exit code 0 from an empty filter is insufficient. Expand manifest `ALL_C`/`ALL_K` into every ID, not one representative smoke test.

Evidence per case records revision, target/function(s), fixture ID/seed, fault window, observed assertion, result, platform and secret-free artifact. Distinguish component fixture, real multi-agent execution, browser E2E and live-provider evaluation. Do not rename a mock result “end-to-end”.

Phases also own tests beyond C/K/W: P0 foundation, provider streaming, path safety, process-tree cleanup, budget limits, packaging and benchmarks. The 44 cases are not the entire test suite. See the [shared verification contract](README.en.md) and each phase's section 5.

The current documentation checker validates only structure, coverage IDs/ownership, phase dependencies and step parity. It does not prove runtime behavior.
