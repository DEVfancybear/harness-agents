# Harness Agents — New product and architecture plan

**Revision 1 — 2026-09-15 — Planning only.**

[Detailed Vietnamese master plan](HARNESS_MASTER_PLAN.vi.md) · [Implementation roadmap](HARNESS_ROADMAP.vi.md) · [DeerFlow research and sources](research/DEERFLOW_RESEARCH_2026-09-15.md)

**Implementation guidance added 2026-09-19:** the [DeepSeek coding pack](implementation-next/README.vi.md) provides 13 milestone runbooks, concrete contracts, setup/trigger/oracle acceptance specifications, assignment prompts and restart handoff templates. It defaults new implementation to an isolated `vnext/` Cargo workspace. These are planning artifacts, not completed runtime features.

This is an English overview of the new plan. The Vietnamese master plan and roadmap contain the complete subsystem contracts, work items, coverage matrix and acceptance scenarios.

## 1. Scope and authority

Design a personal Rust coding-agent harness with CLI-first delivery, durable task continuity, optional reusable memory and bounded multi-agent delegation. Web, daemon scheduling and strict execution backends are later milestones with explicit prerequisites.

Per the updated request, this plan is independent of existing code and old implementation phases. It is not a repair backlog. For future development, use this plan and its new roadmap as the design baseline; preserve older P0–P8 documents as historical material. No runtime implementation, data migration or phase acceptance is implied by publishing these documents.

Research inspected selected DeerFlow documentation/source paths in two passes on September 14–15 at commit `6469833886487a71c25d17b6b543ecab9b0defca`. The second pass added goals, lineage, streaming gaps, scheduling, remote task handles, upload handling, secrets, auth and retention. No upstream runtime benchmark or full test suite was executed. This is bounded architecture research, not an exhaustive security audit.

## 2. Product defaults

| Area | Decision |
|---|---|
| User/platform | One local user; Windows primary, Linux secondary |
| Runtime | Rust/Tokio modular monolith; one writable host per data directory |
| Storage | Local SQLite plus content-addressed artifacts; authoritative transactional journal/domain state |
| Continuity | Structured WorkingState, checkpoints, journal tail and source-history retrieval |
| Models | API adapters; DeepSeek first, deterministic mock fixtures; explicit capability/version matrix |
| Tools | One host-owned validation/policy/approval/intent/execution/receipt pipeline |
| Extensions | Built-in traits, trusted local skills, MCP client and versioned process protocol |
| Delegation | Same agent runtime, independent contexts, durable DAG/inbox and isolated editing worktrees |
| Interfaces | CLI first; Web and daemon reuse application services |
| Isolation | Trusted-repository host mode is explicit; strict confinement requires a separately tested backend |

The initial coding journey must handle read → edit → failing test → correction → passing test → evidence-backed final output, including cancellation, user corrections and kill/reopen recovery.

## 3. Architecture and invariants

Logical modules are contracts, core domain, store, runtime, context, providers, tools, execution, memory, orchestrator, extensions, application composition and CLI/API/daemon adapters. Start with fewer crates if boundaries remain enforced. Core/runtime depend on ports, not UI frameworks or concrete transport/storage implementations.

Task, session, run, turn, step, provider attempt and tool invocation have separate identities and state machines. Run completion is distinct from task acceptance. Explicit stop reasons cover limits, errors, missing input, external waits and unknown outcomes.

Key invariants:

- ACK only after durable input admission; input deduplication and state projection share a transaction.
- Current owner generation fences writes/finalization. UI notifications are not the source of truth.
- Approval consumption and exact invocation intent are atomic; receipts and mandatory projections settle together.
- External side effects cannot generally be promised exactly once. Unknown outcomes require reconciliation.
- Context has one compiler and a frozen manifest. Summary, memory and child reports cannot become host authority.
- Compaction uses the original source watermark and preserves mandatory instructions/tool pairing, with deterministic fallback.
- History search/read uses scoped source references and a rebuildable journal index; memory is not required for resume.
- Streaming is incremental and bounded. Tool calls execute only after complete arguments and protocol validation.
- Budgets cover model calls, retries, summaries, evaluators, extraction and children; missing usage is not zero.
- Cancellation covers queues, requests, tools, process trees and descendants, followed by bounded cleanup.
- Worktree integration is checked at the final revision. Worker reports alone do not accept a task.
- Replay is read-only; checkpoint rollback does not undo filesystem or remote side effects.

## 4. Selected DeerFlow mechanisms

Adopt app/harness separation, ordered middleware contracts, durable context channels, source-history tools, bounded output synopses, deferred skill/tool discovery, explicit terminal reasons, subagent report contracts, structured events, sandbox lifecycle, goal limits and scheduler/job separation. See the [22-entry evidence register](research/DEERFLOW_RESEARCH_2026-09-15.md).

Adapt these mechanisms to Rust rather than importing the full LangGraph/Python application. Use a journal projection instead of a competing archive authority, durable extraction jobs instead of relying on in-process debounce, host-owned usage reservations, and integrated-revision acceptance rather than prose assertions.

Keep local skills small and versioned. MCP support needs a tested SDK release/protocol matrix; tools/resources, prompts, elicitation, sampling, remote auth and Tasks extensions are separate capabilities. Deferred schema visibility never grants execution permission.

Memory uses source-backed versioned assets, scoped FTS retrieval, CAS, invalidation and durable job/cursor settlement. Embeddings are deferred until recall measurements justify them. CodeGraph/LSP/web research have integration ports; building every indexer/crawler is unnecessary.

Daemon scheduling uses durable occurrences, timezone/DST/misfire/overlap semantics and delivery deduplication. Remote tasks retain handles and polling state, without blindly resubmitting uncertain mutations. Non-interactive execution waits or blocks when authority/input is missing.

Web consumes durable projections plus incremental events with cursor replay, gap handling and deduplication. Default local binding still requires transport identity/origin checks. Public/multi-user deployments need an additional security and isolation scope.

## 5. Security and data lifecycle

Permissions derive from host/user authority and can be narrowed by project/profile rules. Repository/web/tool/memory/skill text cannot grant rights. Scoped secrets stay out of prompts/journal and are not inherited wholesale by subprocesses; helper paths and agent sockets can also transfer authority.

Configuration distinguishes per-step snapshots, startup-only settings and trusted-only executable/secret configuration. Revocation reaches final execution checks. Retention preserves active tasks, referenced artifacts and fork/resume lineage; archive, cancel and delete are separate operations.

Backup/restore must cover a consistent database snapshot and reachable artifacts. Migrations are versioned and recoverable. Starting new implementation must not silently overwrite existing user data. Generated artifact previews are sandboxed; viewing a file does not authorize execution.

## 6. Roadmap

| Milestone | Scope | Person-days |
|---|---|---:|
| M0 | Contracts, ADR boundaries, schemas and executable skeleton | 3–5 |
| M1 | Durable store, artifacts, checkpoints and recovery | 7–10 |
| M2 | Provider messages, capabilities and incremental streaming | 5–8 |
| M3 | TurnDriver, input/approval, limits and goal outcomes | 6–9 |
| M4 | Coding tools, host execution and end-to-end evidence | 8–12 |
| M5 | Context, compaction, source history and fork/resume | 7–10 |
| M6 | Skills, MCP and extension lifecycle | 6–9 |
| M7 | Durable reusable memory | 6–9 |
| M8 | Multi-agent DAG, worktrees and integration acceptance | 9–14 |
| M9 | Data lifecycle, diagnostics, evaluation and CLI release | 6–9 |
| M10, optional | Web/API | 10–15 |
| M11, optional | Daemon, schedules and external-task workers | 8–12 |
| M12, conditional | One strict execution backend | 8–12 |

M0–M9 totals **63–95 person-days**, approximately **79–119 with 25% contingency**. M0–M5 provides the initial single-agent coding/continuity slice in **36–54 person-days** before contingency. Optional M10–M12 together add **26–39 person-days**. These are greenfield estimates, not additional time on top of the old roadmap or automatic-agent deadlines.

M2 depends on M0; M3 joins M1 and M2. M4–M9 proceed sequentially. M10/M11 depend on M9 and can be selected independently. M12 depends on M4 and must move before any release that promises untrusted-code confinement.

The [roadmap](HARNESS_ROADMAP.vi.md) defines **52 work items**, **25 coverage requirements**, **36 acceptance scenarios** and **11 planned ADRs**. All are planned specifications, not executable tests or completed milestones.

## 7. Verification and first assignment

Acceptance exercises real storage, drivers, tools and process fixtures with deterministic external model/network boundaries. Required cases cover crash/ACK/receipt gaps, ownership, stream parsing, false completion, question/approval correlation, cancellation, compaction/source recovery, scopes, skill/MCP lifecycle, memory CAS, child fairness/delivery, integration failures, backup/retention, Web reconnect, schedules and strict confinement.

Measure task acceptance, false completion, source recall, total cost, latency, storage growth, recovery/cancellation and integration effort. Compare single-agent, memory and multi-agent variants on the same tasks. Do not claim token savings or production success rates before measuring a baseline.

Start with **M0-01..M0-04** only: define domain contracts, ports, the smallest executable skeleton and the acceptance registry. Do not scaffold the entire architecture or treat older code with matching names as accepted automatically. Every milestone requires revision-bound evidence and a handoff before proceeding.
