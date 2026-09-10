# P8 — Web UI over the same host services

English | [Tiếng Việt](P8_WEB.vi.md)

Implementation runbook; status: **not started**. Estimate: 10–15 person-days. All target files, Rust tests and `ha` commands below are future outputs unless already present in the checkout. This document alone is not completion evidence.

## 1. Outcome and entry gate

Ship an optional local Web interface after [P7](P7_RELEASE.en.md) is accepted and the user explicitly assigns Web work. Reuse the tested runtime, task coordinator, policy gate and stores. A browser reconnect resumes presentation of existing work; it must not create another agent loop.

The default is a foreground `ha web` host bound to loopback, for one local user. It owns the same data-directory writer lock as CLI execution. Starting it while another writer owns that directory returns a clear busy error. Remote hosting, a background daemon and multiple user accounts require a separate design and authorization.

Read the [handbook](README.en.md), [plan](../RUST_HARNESS_PLAN.en.md), [plugin contract](../PLUGIN_ARCHITECTURE.en.md), [memory contract](../MEMORY_AND_CONTINUITY.en.md) and [acceptance map](ACCEPTANCE_MAP.en.md). Inspect actual predecessor evidence before coding.

## 2. Owned scope and target files

Own the thin `harness-web` adapter and a frontend directory selected in the P8 SPEC, plus Web integration tests, static packaging and operator docs. Extend application-service interfaces only where an equivalent CLI operation already exists or a reviewed read API is needed. Keep storage SQL, context composition, task ownership, approvals and memory policy in their established owners.

Target outputs: the Web adapter crate/module, frontend source and locked dependencies, `crates/harness-cli/tests/phase_p8.rs`, browser E2E tests, and W01–W06 entries in the acceptance registry. Choose a minimal frontend stack explicitly; no framework is mandated by this runbook.

## 3. Contracts to settle before implementation

Freeze route/version conventions, request IDs, application-service command mappings, event cursor semantics, filtered view models, error codes and frontend build ownership. State whether an endpoint is a read, input admission or a side effect.

Loopback is not authentication. Define a user bootstrap/session flow with high-entropy credentials, expiry, secure local handling and no token in URL/history/logs; origin/host allowlists, CSRF protection for cookie-authenticated mutation, and authorization for each object/artifact are required. No wildcard credentialed CORS or default public binding. Approval must preserve P3's actor, invocation, arguments, workspace and policy bindings. Pick and pin actual HTTP/frontend dependencies using their official documentation.

## 4. Ordered work items

### 4.1. P8-S01 — Freeze application and security mappings

Depends on: accepted P7.

Inspect CLI handlers and list the underlying commands/queries needed for sessions, chat, tasks, tool approvals, context and memory. Define the browser's authenticated principal at the host boundary. Set request/response limits, supported browser/platform matrix and local bootstrap UX. Record service gaps in the SPEC before changing contracts.

Evidence: Route-to-service matrix, threat cases and a screen inventory with explicit non-goals; no second authority in the design.

### 4.2. P8-S02 — Build the foreground HTTP host

Depends on: P8-S01.

Acquire the existing host writer lock before accepting commands. Use the same composition root and shutdown sequence as CLI mode. Adapt HTTP requests into existing services; validate schema, IDs, grants, origins and CSRF before admitting work. Add bounded request concurrency and structured errors. Keep credentials out of browser-visible telemetry and URLs.

Evidence: Real host tests prove unauthorized/invalid commands never reach input or tool admission; a competing CLI writer is refused without modifying state.

### 4.3. P8-S03 — Stream durable, scoped events with catch-up

Depends on: P8-S02.

Expose SSE or another explicitly chosen streaming transport over durable event cursors. Authorize and project each event into a safe view, not a raw journal dump. Reconnect from the last cursor, deduplicate logical events and handle retention gaps with an explicit resnapshot signal. Bound slow-client buffers; recheck access after revocation. Disconnection cancels the subscription, not the task.

Evidence: Disconnect/reconnect tests recover ordered visible events, report expired cursors and show no extra provider dispatch or duplicate input.

### 4.4. P8-S04 — Implement core session and approval screens

Depends on: P8-S03.

Add session/task selection, message entry, progress/tool timeline and bound approval dialogs. Keep client-generated command IDs stable across retry. Render persisted receipts, uncertainty, blocked states and stale approval errors explicitly. Treat model/tool/memory output as untrusted content: safe Markdown, no raw script execution, and allowlisted link schemes.

Evidence: Browser tests cover an admitted input retry, denied and expired approval, script-bearing tool output and a restored task with pending work.

### 4.5. P8-S05 — Add task, memory and context inspection

Depends on: P8-S04.

Show the task tree, child handoffs, effective scopes, memory source/version, extraction job status and exact sanitized context packet inspection. Use existing read/invalidate APIs and permission checks; do not add direct table editors. Explain missing/forgotten evidence and degraded retrieval. Every destructive or authority-changing action uses the established confirmation and service contract.

Evidence: UI and API assertions agree with CLI for the same task IDs, receipts, memory scope and pending jobs; cross-scope IDs and hashes disclose no content.

### 4.6. P8-S06 — Run Web acceptance and retained CLI regressions

Depends on: P8-S04 and P8-S05.

Implement W01–W06 in the acceptance map using real local host services plus a deterministic model boundary. Test transport retries, browser closure, host ownership, authentication/CSRF, content rendering and stream recovery. Run all existing C/K regressions against the same runtime; add an adapter regression whenever Web required a shared-service change.

Evidence: Nonempty browser/adapter test discovery, required platform results, negative controls and all prior gates; distinguish browser tests from HTTP-only tests.

### 4.7. P8-S07 — Package the local UI and hand off operations

Depends on: P8-S06.

Build and serve version-matched static assets without a development server dependency. Smoke-test the packaged foreground host and browser flow from a clean task-owned directory. Document safe bootstrap, local binding, close-browser versus stop-host behavior, writer-lock conflicts and how to return to CLI. Keep remote deployment and package publishing outside this assignment unless explicitly requested.

Evidence: Bilingual evidence/handoff, local startup instructions and artifact checksums bound to tested source; no claim of external deployment.

## 5. Tests and verification commands

Own the additional Web cases **W01–W06** defined in the [acceptance map](ACCEPTANCE_MAP.en.md). They do not replace any of the existing 44 continuity/plugin cases. Require browser-level checks for rendering and reconnect UX, adapter-level denial tests, real writer-lock tests, and the full P7 regression suite. A UI screenshot alone is not acceptance evidence.

P8 extends `Verify-Phase` to run frontend formatting/type/build checks and the selected browser test command with locked dependencies. Record exact commands and supported browser versions in the phase SPEC; do not silently skip a missing browser runner.

Future phase commands, to run after implementing the targets:

```powershell
cargo test -p harness-cli --test phase_p8 --locked
pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P8
```

The full gate includes the handbook's formatting, clippy, workspace tests, test-discovery checks and docs checks. Do not report a filtered zero-test run as passing acceptance.

## 6. Demonstration rehearsal

Start `ha web` on a loopback port in a disposable fixture data directory. Authenticate through the approved local flow, open an existing CLI-created task, submit one command and reconnect during a tool run. Verify the same command ID has one logical admission, pending work persists and receipts match the CLI read view. Close and reopen the browser while the foreground host stays alive; then stop the host normally and resume via CLI. Attempt a second writer and an unauthorized artifact read; both must be refused.

## 7. Exit gate and forbidden shortcuts

W01–W06 and predecessor gates pass on the declared platform/browser matrix. Browser reconnect is only a view subscription; app services retain control of execution and authority. Static assets and backend have a reproducible compatibility identity. Authentication, CSRF and output rendering controls are tested, not deferred because the server is local.

Forbidden: starting another loop for each tab, directly mutating SQLite from routes, client-authoritative approval/state, raw private event streaming, public bind by default, or turning this phase into a multi-user cloud service.

Do not advance phases on a summary alone. Bind results to the final tested revision, report missing checks, and preserve all predecessor regressions. Never alter fixture expectations merely to make implementation pass.

## 8. Delegation and handoff

If the user explicitly assigns parallel work, the backend adapter owner and frontend owner may work concurrently only after P8-S01 freezes API/security contracts. One integrator owns dependency locks, browser fixtures and shared-service changes. Do not split authentication responsibility between teams without one reviewer of the complete flow.

Deliver `docs/evidence/P8.en.md` and `P8.vi.md`, plus a resumable handoff under `docs/handoffs/`, following the handbook. Include completed step IDs, pending failures, schema changes, commands and next action. Publication requires explicit authorization in the coding assignment.

## 9. Ready-to-use agent prompt

```text
Implement P8 only. Read docs/implementation/README.en.md and
P8_WEB.en.md in that directory, all linked architecture contracts,
and applicable repository instructions. Verify the predecessor gate from source/evidence.
Create the phase SPEC, then implement P8-S01..P8-S07 in dependency order.
Stay within this phase's owned scope; preserve unrelated changes and accepted contracts.
Use real components for acceptance tests, with mocks only at appropriate external boundaries.
Run the phase gate and predecessor regressions; deliver bilingual evidence and a restart handoff.
Stop before the next phase. Do not spawn agents, commit, push or publish unless explicitly assigned.
If prerequisites or required verification are unavailable, report the exact gap without claiming completion.
```
