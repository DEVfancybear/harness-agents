# ADR-N01 — Domain identity, state machines and acceptance

**Status:** accepted in M0 (2026-09-21).
**Scope:** identity/state/acceptance shared by M0–M12.
**Supersedes:** nothing. ADR-0001 (P0 foundation boundaries) remains in force.

## 1. Context

M0 has to fix identity and state before M1 creates the run/step tables and M3
builds the TurnDriver. The current source already has typed IDs, a
`WorkingState` projection, a small run state machine inside `harness-runtime`,
and an acceptance decision inside the orchestrator
(`DelegatedResult::accepted_completion`). Without an explicit split, "the run
ended" is easily used in place of "the task was accepted".

## 2. Decisions

1. **Four separate identities.** `SessionId` is the durable conversation
   container; `TaskId` is the unit of acceptance; `AgentRunId` is one execution;
   `StepId` is one model/tool cycle inside a run. `StepId` is a public type from
   M0; the runs/steps tables belong to M3.
2. **Ordering is sequence only.** `(session_id, seq)` is the only transaction
   order. IDs and timestamps never decide ordering.
3. **Three independent state machines.** Run (`AgentState`), acceptance
   (`AcceptanceRecord`), projection (`WorkingState`). A reducer is a pure
   `(state, validated command) -> (next state, proposed events)` function.
4. **Terminal states cannot regress.** A terminal run accepts only `Dispose`; an
   accepted task accepts no further command.
5. **Acceptance needs evidence.** A required criterion is `Satisfied` only with
   typed evidence (`FileChanged`/`CheckExecuted`/`ArtifactProduced`); any
   `pending_effects` keeps the task unaccepted. A human acceptance records
   `actor_id` + `SourceRef` and **does not** mark criteria satisfied — it is a
   recorded override, not a fake test pass.
6. **Errors carry a retry class.** `ErrorReport { schema_version, code,
   retry_class, safe_message, correlation_id?, details_ref? }`; `RetryClass` is
   derived from `ErrorCode`, never from the message. The CLI exit code is derived
   from `ErrorCode::exit_code()`.
7. **Scope is host-created.** `ScopeContext` (principal, project/worktree, task,
   session, capabilities, config revision, owner generation) is built by the
   host; tool arguments and contributions are checked *against* it and can never
   widen it.

## 3. Consequences

- M1 implements `StorePort` on `SQLite`; M3 adds step/run tables without
  changing the IDs.
- M3/M4 must not derive "accepted" from assistant final text; they call the
  acceptance reducer.
- Any `ErrorCode` change must update `retry_class()`/`exit_code()` (exhaustive
  match, enforced by the compiler).
- Adding a command to the run machine must update the `ALLOWED` table in the
  `harness-runtime` table-driven test.
