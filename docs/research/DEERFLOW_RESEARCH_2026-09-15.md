# Nghiên cứu DeerFlow cho master plan mới

**Hai lượt khảo sát: 14–15/09/2026.** [Master plan](../HARNESS_MASTER_PLAN.vi.md) · [Roadmap](../HARNESS_ROADMAP.vi.md) · [Source manifest](deer-flow-2026-09-15.sources.json)

## 1. Phạm vi, nguồn và giới hạn

Nguồn chính là repository công khai [bytedance/deer-flow](https://github.com/bytedance/deer-flow/tree/6469833886487a71c25d17b6b543ecab9b0defca), revision **`6469833886487a71c25d17b6b543ecab9b0defca`**, nhánh main tại lần tải phục vụ khảo sát. Mọi link source DeerFlow trong bộ tài liệu được pin revision này, không tự trôi theo main.

Lượt một đọc architecture/docs và các đường source: lead assembly, context/compaction/history, provider/runtime, skills/deferred tools, subagents/acceptance, sandbox, memory và event projections. Lượt hai mở rộng: goals/continuations, thread lineage, stream gaps, scheduled occurrences, long-running MCP tasks, upload handling, credentials/environment, auth, config reload và checkpoint retention.

Theo yêu cầu cập nhật, kết quả cuối là **bản thiết kế tổng thể mới**. Code cũ của Harness không được dùng để xác định thứ tự sửa lỗi hoặc giới hạn kiến trúc. Bản draft phân tích khoảng trống implementation cũ đã được thay bằng master plan/roadmap mới.

Phương pháp: đọc tài liệu và source/snippets liên quan, đối chiếu contract với call sites và test tiêu biểu. Graph MCP không có callable tools trong phiên; dùng source search trực tiếp. Không chạy DeerFlow server, không chạy toàn bộ test suite upstream và không đo live-model benchmark. Đây không phải line-by-line security audit hay cam kết không còn mọi lỗi.

Các dòng trong manifest là **evidence paths được đọc theo phạm vi chọn lọc**, không tuyên bố đọc/kiểm chứng toàn bộ từng file. Hash toàn file phục vụ nhận diện snapshot. Tài liệu có trạng thái draft được ghi riêng; không dùng draft như bằng chứng feature đã được nghiệm thu.

## 2. Nguồn DeerFlow và điều rút ra

| ID | Nguồn đã đọc | Nhận định và cách dùng |
|---|---|---|
| D01 | [Top-level architecture](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/docs/ARCHITECTURE.md), [lead assembly](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/packages/harness/deerflow/agents/lead_agent/agent.py) | Harness/app tách một chiều; cùng core phục vụ interfaces. Assembly source có nhiều middleware hơn sơ đồ đơn giản; học invariants/order, không copy sơ đồ thành runtime |
| D02 | [DurableContextMiddleware](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/packages/harness/deerflow/agents/middlewares/durable_context_middleware.py) | Summary, delegation ledger, skills và notes là data channels; static authority contract tách khỏi field values |
| D03 | [Summarization guide](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/docs/summarization.md) | Giữ tool pairing, summary channel; có caveat threshold theo summary model window. Plan Rust tính budget theo actual run request |
| D04 | [Task continuity guide](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/docs/task-continuity.md), [history/note tools](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/packages/harness/deerflow/agents/task_continuity/tools.py), [tests](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/tests/test_task_continuity.py) | Exact source search/read, bounded notes/retention/scope và graph compaction-resume tests. Plan dùng journal projection thay archive authority riêng |
| D05 | [Loop lifecycle](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/docs/LOOP_DETECTION.md) | Detection theo logical run, hash/frequency windows và severity ordering; không chỉ đếm số câu trả lời |
| D06 | [Token budget](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/packages/harness/deerflow/agents/middlewares/token_budget_middleware.py), [terminal response](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/packages/harness/deerflow/agents/middlewares/terminal_response_middleware.py), [model config](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/packages/harness/deerflow/config/model_config.py) | Usage/caps/stop reasons và empty post-tool final cần xử lý riêng. Plan thêm durable reservations để concurrent children không vượt admission budget |
| D07 | [Skill catalog](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/packages/harness/deerflow/skills/catalog.py), [skill filesystem permissions](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/packages/harness/deerflow/skills/permissions.py) | Metadata discovery, lazy content và bảo vệ resources khác nhau; catalog presence không là activation/permission |
| D08 | [Deferred tool search](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/packages/harness/deerflow/tools/builtins/tool_search.py) | Catalog hash, bounded search, promote schema; execution authorization vẫn riêng |
| D09 | [Parent snapshot](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/packages/harness/deerflow/subagents/context_snapshot.py), [report contract](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/packages/harness/deerflow/subagents/report_contract.py), [acceptance checks](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/packages/harness/deerflow/subagents/acceptance_checks.py), [status contract](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/packages/harness/deerflow/subagents/status_contract.py) | Snapshot không chuyển parent calls thành child executions; receipt-backed self-report và typed status. Checklist có giới hạn, không chứng minh toàn bộ correctness; Plan thêm integrated revision acceptance |
| D10 | [SandboxProvider](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/packages/harness/deerflow/sandbox/sandbox_provider.py), [output budget](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/packages/harness/deerflow/agents/middlewares/tool_output_budget_middleware.py) | Acquire/get/release/capabilities và externalized output synopsis. Plan giữ port này, thêm capture quotas/hash/transaction consistency |
| D11 | [Memory manager](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/packages/harness/deerflow/agents/memory/manager.py), [DeerMem queue](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/packages/harness/deerflow/agents/memory/backends/deermem/deermem/core/queue.py) | Backend-neutral operations và mode capabilities; queue đã đọc có list/timer trong process. Không suy mọi backend đều như vậy; Rust cần durable extraction jobs rõ |
| D12 | [Run event stream](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/docs/RUN_EVENT_STREAM.md), [stream bridge](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/packages/harness/deerflow/runtime/stream_bridge/base.py), [run manager](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/packages/harness/deerflow/runtime/runs/manager.py) | Thread-global sequence, durable events/projections, reconnect gap và ownership/cancel lifecycle. Source review không chứng nhận deployment multi-worker |
| D13 | [Frontend task result](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/frontend/src/core/tasks/subtask-result.ts), [task update reducer](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/frontend/src/core/tasks/subtask-update.ts) | Structured status/stop reason và idempotent merge; không parse văn bản model thành authoritative lifecycle |
| D14 | [Reload boundary registry](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/packages/harness/deerflow/config/reload_boundary.py) | Một số config apply ở call boundary, pool/storage/service capture lúc startup; cần explain/restart requirements |
| D15 | [Goal runtime](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/packages/harness/deerflow/runtime/goal.py), [thread lifecycle](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/docs/THREAD_LIFECYCLE.md) | Bounded continuation/no-progress/blockers; lineage lookup cho branch/regenerate. Plan không coi evaluator model là source proof và không lẫn rollback với undo external effects |
| D16 | [Schedule calculations](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/packages/harness/deerflow/scheduler/schedules.py), [scheduler service](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/app/scheduler/service.py) | Timezone/once/interval/cron, occurrence admission khác execution capacity, manual trigger/future schedule và terminal recovery cần riêng |
| D17 | [Upload manager](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/packages/harness/deerflow/uploads/manager.py) | Shared app-independent upload logic, staging/filename/path validation; plan thêm scopes/quotas và safe previews |
| D18 | [MCP task driver](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/packages/harness/deerflow/mcp/tasks/driver.py) | Protocol-neutral submit/get_status/cancel port; plan lưu remote handle và polling job bền vững |
| D19 | [Extension API package](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/packages/extension-api/pyproject.toml) | Public contract package tách host, dependencies tối thiểu. Chỉ dùng làm boundary pattern, không hứa tương thích Python extensions |
| D20 | [Auth design](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/docs/AUTH_DESIGN.md) | Server-owned identity xuyên repositories/files/memory; browser auth/CSRF và nhiều người là scope riêng. Plan v1 vẫn local user |
| D21 | [Sandbox environment policy](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/packages/harness/deerflow/sandbox/env_policy.py) | Credential env vars/helper paths/agent sockets là nhiều đường truyền quyền; plan dùng allowlist env + scoped injection |
| D22 | [Checkpoint retention contract — draft](https://github.com/bytedance/deer-flow/blob/6469833886487a71c25d17b6b543ecab9b0defca/backend/docs/checkpoint-retention-contract.md) | Protected lineage/resume targets/pending writes và reachability; đây là failure-mode contract draft, không bằng chứng retention implementation hoàn chỉnh |

## 3. Tài liệu nền tảng đối chiếu độc lập

| Nguồn chính thức | Điều xác minh / ảnh hưởng kế hoạch |
|---|---|
| [SQLite WAL](https://sqlite.org/wal.html) | WAL yêu cầu cùng host, không đặt DB trên network filesystem; checkpoint/WAL lifecycle phải được vận hành |
| [SQLite Online Backup](https://sqlite.org/backup.html) | Backup live database cần cơ chế nhất quán; artifact manifest vẫn là trách nhiệm riêng của harness |
| [Tokio graceful shutdown](https://tokio.rs/tokio/topics/shutdown) | Phân biệt phát hiện shutdown, truyền cancellation và chờ tasks kết thúc |
| [Git worktree](https://git-scm.com/docs/git-worktree) | Linked worktrees dùng chung một phần Git metadata; cần ownership/integration, không xem là sandbox |
| [Windows Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects) | Quản lý nhóm process/resource; không tự giải quyết mọi filesystem/network authority |
| [MCP specification 2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28) | Version và optional extensions phải được negotiate; plan không mặc định hỗ trợ mọi MCP feature |
| [Official Rust SDK](https://github.com/modelcontextprotocol/rust-sdk) | SDK công bố spec support và legacy lifecycle; pin/test một release tại implementation, không dùng main làm dependency mặc định |

Những nguồn không phải DeerFlow được đọc ngày 15/09/2026 và có thể thay đổi. Bản plan không pin số version crate chưa được tích hợp; milestone M2/M6/M9 phải ghi compatibility matrix và dependency versions đã kiểm chứng.

## 4. Kết quả chọn lọc

**Áp dụng sớm:** shared runtime/application service, typed run/tool outcomes, bounded model loop, context provenance, history retrieval, artifact synopsis, skill activation, deferred discovery, receipts, diagnostics.

**Áp dụng sau khi nền tảng ổn định:** reusable memory với durable jobs, multi-agent acceptance/worktrees, Web event replay, daemon/scheduler và remote long-running tasks.

**Điều chỉnh so với upstream:** journal-index thay archive authority riêng; host-owned usage ledger; strict tool grants; task acceptance theo integrated revision; Windows/Linux execution matrix; memory source/CAS/invalidation; supported protocol versions minh bạch.

**Không đưa vào v1:** toàn bộ LangGraph/Python/Next.js stack, multi-tenant server, Redis/Kubernetes, nhiều memory providers/vector DB, IM platforms, multimedia generation, self-evolving skills/marketplace. Các mục này có giá trị với sản phẩm khác, nhưng không bắt buộc cho personal coding harness.

## 5. Các kết luận không được suy diễn

- Source có class/test không có nghĩa toàn tính năng đã được xác minh production.
- Tool receipt/file exists không có nghĩa deliverable đúng yêu cầu.
- Summary/task note có citation không có nghĩa interpretation đúng.
- Retry/lease/dedupe không bảo đảm exactly-once side effect của hệ thống bên ngoài.
- Worktree/process group/path validation không là strict sandbox.
- Phiên bản SDK hỗ trợ protocol không đồng nghĩa ứng dụng đã hỗ trợ mọi feature.
- Prototype benchmark của upstream không phải số liệu chất lượng của bản Rust.

Bộ tài liệu cuối dùng các kết luận có phạm vi này làm cơ sở và bổ sung coverage/acceptance để phát hiện thiếu sót trong implementation tương lai.

## 6. Kiểm chứng tài liệu trước bàn giao

Đã chạy `pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest`: PASS, gồm 12 negative controls. Đây là checker cấu trúc tài liệu của repo; các số phase/case cũ trong output không phải nghiệm thu roadmap mới.

Kiểm tra tĩnh bổ sung trên tài liệu mới: PASS — 13 milestones, 52 work items duy nhất, R01–R25, A01–A36, D01–D22; tổng M0–M9 là 63–95 ngày công, ba mốc tùy chọn 26–39; 38 evidence paths upstream tồn tại và được ghi hash; links DeerFlow pin cùng revision. Đã kiểm tra local links, code fences và không còn references tới draft cũ.

Không chạy Rust runtime tests vì thay đổi chỉ gồm kế hoạch/nghiên cứu và đường dẫn đọc trong README. Không nhận bất kỳ acceptance A01–A36 nào đã pass runtime.
