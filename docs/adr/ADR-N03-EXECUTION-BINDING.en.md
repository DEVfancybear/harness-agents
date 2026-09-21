# ADR-N03 — Execution binding, final guard, host limitations and reconciliation

**Status:** accepted in M4 (22/09/2026).
**Scope:** tool authority (grant/intent/receipt), the final guard before dispatch, host limits, and unknown outcomes. It does not change ADR-N01/N02/N04.

## 1. Context

P3 already has one gate (normalize → policy → approval → intent → executor → receipt) with approvals bound to actor/action/workspace/revisions, atomic consume+intent, immutable receipts and reconciliation for pending work. M4 must: (a) bind the grant more tightly (invocation/task/session), (b) persist the provider's `tool_call_id` correlation, (c) state plainly that the host is not a sandbox and that TOCTOU cannot be closed completely, and (d) handle "the side effect may have happened" without turning it into failed/rerun.

## 2. Decisions

1. **Grant binding.** One approval binds `actor`, `task_id`, `session_id`, `invocation_id`, `action_hash`, `workspace_root` + `workspace_fingerprint`, `policy_revision`, `tool_revision` and `expiry`. The canonical `binding_hash` covers every one of them; dropping any field is caught by the A14 negative control (remove the invocation from the hash and the test fails).
2. **One-shot consume.** Consume grant + insert intent + event + projection happen in **one** transaction; the CAS is `active → consumed`. A grant for another invocation/task/session, or one already consumed/revoked/expired, is a typed denial and the executor is never called. `revoke` moves `active → revoked`; every later execute is denied.
3. **`tool_call_id` correlation.** The provider's `call_id` is recorded on `ToolRequest` → intent → receipt (optional, `serde(default)` for older records). The host `InvocationId` stays the authority; `call_id` only correlates with the transcript/model view.
4. **Final guard.** Immediately before dispatch: revalidate policy revision, workspace fingerprint, target hash (patch), cancellation and the admission permit. Any change is a typed deny/stale with no side effect. The TOCTOU window between the check and the OS action is **bounded and acknowledged**, not claimed to be closed by the database.
5. **Host limitations.** `filesystem_network_sandbox=false` and `strict_isolation=false` are facts that must be visible; path policy only constrains host-mediated operations, it does not confine a shell/process. Process trees: JobObject kill-on-close on Windows, process session on Unix; `tree_cleanup_confirmed` is true only when the backend confirms it, otherwise the receipt carries `uncertainty` (`outcome_unknown`).
6. **Effect before receipt.** A settlement failure after a side effect leaves the intent `recorded` and the receipt `outcome_unknown`; the host never reruns on its own and never rewrites it as failed-no-effect. `reconcile_pending` is an explicit action that writes a new event referencing the old intent, and settles `applied` only when the target content matches exactly.
7. **Long output.** Process output spools to an artifact with header/tail previews and page reads; memory never holds the whole log; quota and disk-full are typed errors and never produce a fake reference; the receipt carries `captured_bytes`/`content_hash`/`truncated` for exactly the bytes captured.
8. **Final evidence.** Check evidence carries `workspace_digest`; final criteria are satisfied only when a check's digest matches the final workspace fingerprint. Model prose and counters are not enough.

## 3. Consequences

- M4 adds binding columns (session/task/invocation/`call_id`) and tools schema v2 additively; older databases upgrade on open and older hosts refuse a newer database.
- A14/A11 prove binding, expiry, revoke and replay; A03/A04 prove receipt-before-checkpoint and effect-before-receipt with a really killed child process.
- A13/A16/A17 prove the permit queue, env allowlist/JIT secrets, backend tree cleanup and spool/quota.
- A08 uses a disposable repo and a real test runner with final criteria at the workspace digest.
- No milestone is advertised as a sandbox; host limits appear in `ha code capabilities` and in the evidence.
