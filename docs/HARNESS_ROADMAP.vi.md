# Harness Agents — Roadmap triển khai từ kế hoạch mới

**Revision 1 — 15/09/2026. Trạng thái tất cả milestones: planned.**

[Master plan](HARNESS_MASTER_PLAN.vi.md) · [English overview](HARNESS_MASTER_PLAN.en.md) · [Nguồn nghiên cứu](research/DEERFLOW_RESEARCH_2026-09-15.md)

**Bổ sung 19/09/2026:** dùng [sổ tay coding cho DeepSeek](implementation-next/README.vi.md) để triển khai roadmap này: runbook M0–M12, contracts cụ thể, 36 acceptance specifications có test oracle, prompt giao việc và mẫu handoff. IDs/ước lượng của roadmap giữ nguyên; bộ mới phân rã cách thực hiện, không nhận runtime đã được triển khai. Mọi M/H sửa source trong root workspace và cùng binary `ha`; bắt đầu bằng [bản đồ tích hợp](implementation-next/INTEGRATION_MAP.vi.md).

## 1. Cách dùng và phạm vi dự toán

**Ưu tiên trải nghiệm khởi động:** [track H01–H08](HA_LAUNCH_PLAN.vi.md) xử lý yêu cầu gõ `ha` để mở CLI tương tác, trên binary/CLI hiện tại. Track H có [prompt riêng](HA_LAUNCH_PROMPT.vi.md) và là lát cắt ưu tiên của cùng sản phẩm; không phải ngoại lệ kiến trúc. Các phần H đã triển khai được đối chiếu/test và reuse ở M2/M3/M4/M9; không bắt làm lại hoặc chờ toàn roadmap. Đọc evidence H hiện tại để biết trạng thái, không suy từ ngày lập plan.

Đây là roadmap **nâng cấp code hiện tại**. IDs M mô tả yêu cầu/coverage cần đối chiếu, không tuyên bố source chưa có tính năng tương ứng. Trước mỗi assignment phải phân loại từng requirement reuse_verified/adapt/missing/incompatible; giữ P/H regressions, bổ sung test cho gap và nối vào luồng `ha` hiện tại. Nhãn planned của tài liệu không reset tiến độ implementation.

Giả định một developer có kinh nghiệm Rust, model qua API, Windows/Linux, một local user và một writable host. Ngày công gồm code + integration tests + docs/handoff, chưa có số liệu velocity thực tế. Các công việc compatibility/live-provider cần re-estimate sau prototype.

Ba mốc có giá trị sử dụng:

- **M0–M5:** coding CLI một agent, có durable execution và tiếp tục sau compaction/restart.
- **M0–M9:** CLI v1 có skills/MCP, reusable memory và nhiều agent, đủ release theo profile tin cậy đã công bố.
- **M10–M12:** Web, daemon/scheduler và strict backend là nhánh mở rộng có điều kiện, không bắt buộc chờ nhau.

Các số ngày dưới đây là dự toán phạm vi đầy đủ ban đầu, chỉ giữ làm tham khảo. Sau inventory phải tính lại effort còn thiếu; không cộng lại phần P/H đã có. Không quy đổi số agents thành hệ số chia ngày.

## 2. Milestones, dependencies và điều kiện nghiệm thu

| Mốc | Phụ thuộc | Đầu ra | Ngày công | Exit gate |
|---|---|---|---:|---|
| M0 Baseline và contracts tích hợp | — | Source/test inventory, gaps IDs/state/ports, ADR/schema, CLI và registry hiện tại | 3–5 | Contracts validate/round-trip; boundaries không vòng; fixture gate chạy thật |
| M1 Durable store và recovery | M0 | Journal/inbox/projections/fence/artifacts/checkpoints | 7–10 | Crash tại ACK/receipt/checkpoint không mất committed work; read-only replay không dispatch |
| M2 Provider protocol và stream | M0 | Typed messages, mock + DeepSeek adapter, capability/config model, stream decoder | 5–8 | Multi-tool chunk conformance; partial/error/cancel; secrets ngoài records |
| M3 TurnDriver và application service | M1, M2 | Run/step/attempt, bounded loop, questions/approvals ports, budget, goal/stop logic | 6–9 | Multi-step với fixture tools; admit input một lần; mọi path terminal/recoverable đúng |
| M4 Coding tools và host execution | M3 | Read/search/patch/process/Git, policy gate, receipts, spool | 8–12 | Coding task có fail→fix→pass và exact revision; cancel/process/path/approval tests |
| M5 Context và source continuity | M4 | Full-request budget, compaction, WorkingState, history tools/FTS, fork semantics | 7–10 | Năm compactions + restart vẫn khôi phục chi tiết nguồn; no duplicate side effects |
| M6 Skills, MCP và extension boundary | M5 | Local skills/catalog, scoped tools/resources, bounded stdio, config explain | 6–9 | Discovery không nâng quyền; pinned schema/digest; extension failure có giới hạn |
| M7 Reusable memory | M6 | Durable extraction, assets/versions/CAS, retrieval, invalidation | 6–9 | Memory unavailable không chặn task resume; concurrent updates/scopes đúng |
| M8 Multi-agent và worktree integration | M7 | DAG/queues/child delivery, budgets, worktrees, acceptance verifier | 9–14 | Parent crash không mất child results; final integration checks đúng revision |
| M9 CLI release và operations | M8 | Backup/restore/retention, doctor, telemetry/eval, Windows/Linux release | 6–9 | Mandatory acceptance + compatibility/restore gates; reproducible evidence |
| M10 Web/API, tùy chọn | M9 | Shared app API/SSE, task/diff/artifact/approval UI, local auth | 10–15 | Reconnect/gap/dedupe đúng; API/UI không bypass host; a11y/preview tests |
| M11 Daemon/scheduler, tùy chọn | M9 | Local IPC, schedule/occurrences/delivery, external-task worker | 8–12 | Restart/misfire/overlap/non-interactive/remote-job tests; no double launch |
| M12 Strict backend, theo nhu cầu | M4; tích hợp gate ở mốc đưa vào sử dụng | Một backend có isolation capabilities được chứng minh | 8–12 | Filesystem/egress/process/resource/lifecycle tests trên supported environment |

M0–M9: **63–95 ngày công**, dự phòng 25% thành khoảng **79–119 ngày công**. M0–M5: **36–54 ngày công** trước dự phòng. M0–M6 có skills/MCP: **42–63 ngày công**. M10+M11+M12 nếu làm cả ba: thêm **26–39 ngày công**, chưa dự phòng.

M2 có thể phát triển độc lập sau M0 về mặt dependencies, nhưng roadmap mặc định giao tuần tự. Chỉ chia work cho agents/devs khi assignment có owner và phạm vi rõ. M12 phải đưa lên trước bất kỳ release nào quảng cáo xử lý untrusted code; không chờ M9 nếu strict isolation là yêu cầu ngay từ đầu.

## 3. Work items để giao thành PR

Mỗi milestone có bốn work items theo thứ tự. Các targets dưới đây là logical modules được ánh xạ vào source hiện tại qua bản đồ tích hợp. Reuse trước; refactor có mục tiêu và cập nhật callers/tests khi contracts cần thay đổi. Integration owner của milestone chịu trách nhiệm ghép các modules và kiểm chứng trên revision cuối.

### 3.1. M0 — Contracts và ranh giới

| ID | Việc giao | Owner/targets | Bằng chứng |
|---|---|---|---|
| M0-01 | Chốt product profiles, identities, task/session/run/step/invocation states, error taxonomy | contracts/core + ADR | State transition invalid bị reject; phân biệt run completed và task satisfied |
| M0-02 | Định nghĩa ports store/provider/tool/execution/context, event envelope và schemas | contracts + dependency rules | Serialization/version fixtures; core không import app/UI/SQL |
| M0-03 | Kiểm tra/mở rộng CLI config hiện tại, composition root và fixture driver | app/CLI + test harness | Config invalid/unknown field fail rõ; `--json`/exit codes có contract |
| M0-04 | Acceptance registry, failpoint interface, data samples không secrets | tests/docs | Gate chạy đúng test count; trạng thái planned khác accepted |

### 3.2. M1 — Durable execution data

| ID | Việc giao | Owner/targets | Bằng chứng |
|---|---|---|---|
| M1-01 | SQLite migrations, project/task/session IDs, writer lock/generation | store | Hai writer không cùng sở hữu; stale writer không append/finalize |
| M1-02 | Transactional input/inbox, journal và WorkingState projection | store/core | ACK chỉ sau commit; duplicate input/cursor/order đúng |
| M1-03 | Artifact publish, receipt records, checkpoint/hash/replay | store/artifacts/context ports | Crash matrix publish/commit/snapshot; corrupt snapshot fallback có giới hạn |
| M1-04 | Recovery commands, inspect/export nền tảng, compatibility readers | app/CLI/store | Reopen folds tail; replay zero provider/tool dispatch; unknown critical event blocked |

### 3.3. M2 — Model provider

| ID | Việc giao | Owner/targets | Bằng chứng |
|---|---|---|---|
| M2-01 | Typed message/tool protocol, capability registry và frozen request | providers/contracts | Pairing round-trip; unsupported parameters/capability rõ |
| M2-02 | Incremental SSE assembler và bounded stream | providers | UTF-8/frame splits, multiple indexes, usage-only/EOF/length termination |
| M2-03 | Mock + DeepSeek transport, credential references, timeout/retry taxonomy | providers/config | Stream progress trước terminal; cancel/backoff/auth failure đúng |
| M2-04 | Adapter conformance suite + optional capped live smoke profile | tests/CLI | Fake HTTP server chạy thật transport; live reports ghi model/revision, không secrets |

### 3.4. M3 — TurnDriver và điều khiển tác vụ

| ID | Việc giao | Owner/targets | Bằng chứng |
|---|---|---|---|
| M3-01 | Durable run/step/attempt loop, admission một lần, internal continuation | runtime/app/store | Fixture model→tool→model nhiều vòng, không tạo user input giả |
| M3-02 | HumanInputService, approval ports, steering/cancel và shutdown | runtime/app/inbox | Pause/reopen/answer dedupe; correction boundary; cancel queue/request |
| M3-03 | Budget reservation/settlement, step/deadline/retry caps, loop detection | runtime/accounting | Usage unknown không zero; no-progress hard stop; concurrent accounting deterministic |
| M3-04 | Typed acceptance và bounded goal continuation, real CLI composition | core/runtime/CLI | Empty/capped response không false success; blocked/external wait không auto-loop |

### 3.5. M4 — Coding tools dùng được

| ID | Việc giao | Owner/targets | Bằng chứng |
|---|---|---|---|
| M4-01 | Tool registry/schema/policy/approval binding và atomic intent | tools/store | Grant gắn invocation/principal/task/action; consume một lần; policy recheck |
| M4-02 | Read/list/search/patch/Git adapters, project identity và fingerprints | tools/execution | CRLF/Unicode/junction/binary/concurrent edit/dirty input fixtures |
| M4-03 | Structured process, minimal env, tree cleanup, spool/quota/artifact read | execution/artifacts | Canceled queued job không spawn; long output/deadline/disk-full rõ |
| M4-04 | End-to-end coding fixture và receipts/WorkingState integration | app/runtime/tests | Test fail→model sửa→test pass→diff/evidence; crash không chạy lại tool đã settle |

### 3.6. M5 — Context và continuity

| ID | Việc giao | Owner/targets | Bằng chứng |
|---|---|---|---|
| M5-01 | Context channels, provenance, full-request budget và config snapshot | context/contracts | Manifest đủ model/tool/config/source refs; no post-freeze injection |
| M5-02 | Compaction safe boundary/CAS, fallback và active instruction resolution | context/runtime | Concurrent correction không mất; summary fail vẫn dựng minimal packet |
| M5-03 | HistoryReader + FTS/source refs + task notes/tools | context/store/tools | Việt có/không dấu/identifiers, source paging, expired/foreign scope và index rebuild |
| M5-04 | Resume/fork/rollback contract, stale workspace evidence và continuity eval | app/context/tests | Năm compactions và kill/reopen; fork không copy grant; rollback không giả undo side effects |

### 3.7. M6 — Skills/MCP/extensions

| ID | Việc giao | Owner/targets | Bằng chứng |
|---|---|---|---|
| M6-01 | Local skill metadata, activation, content digest và context retention | extensions/context | Discovery≠activation≠permission; update boundary và restore version đúng |
| M6-02 | Service dependency/scope/lifecycle, bounded process plugin protocol | extensions/runtime ports | Missing/cycle/duplicate services, partial init rollback, reverse teardown |
| M6-03 | Pin MCP SDK/version matrix; tools/resources, stdio trust/cancel | extensions/tools | Unsupported feature fail rõ; every call qua gate, frames/inflight bounded |
| M6-04 | Deferred catalog, config explain, disable/unload compatibility | extensions/context/CLI | Schema promotion không tăng quyền; revoked/stale registration không dùng được |

MCP remote auth, Tasks, elicitation và server-initiated model calls không được nhận “đã hỗ trợ” chỉ vì SDK có API. M11 triển khai background Tasks service; remote integration thêm scope riêng khi cần. LSP/code graph/web research dùng ports/MCP ở mốc này, không bắt buộc tự viết indexer/crawler.

### 3.8. M7 — Reusable memory

| ID | Việc giao | Owner/targets | Bằng chứng |
|---|---|---|---|
| M7-01 | Asset/version/grant/binding/dependency schemas và memory service | memory/store | Read/propose/publish/invalidate tách quyền; version/CAS conflict không overwrite |
| M7-02 | Durable extraction queue, cursors/dispositions, background budget | memory/jobs/store | Kill/duplicate job/retry không mất nguồn; memory lỗi không chặn resume |
| M7-03 | FTS retrieval, source freshness, supersedes/revocation và injection | memory/context | Scope filter trước rank; latest correction thắng; không self-reinforcing evidence |
| M7-04 | Inspect/edit/reject/export commands và memory quality eval | CLI/tests | Có/không memory comparison, Việt/identifiers; failure/degradation nhìn thấy được |

### 3.9. M8 — Nhiều agent

| ID | Việc giao | Owner/targets | Bằng chứng |
|---|---|---|---|
| M8-01 | Task DAG, immutable briefs, scheduler/admission/budgets | orchestrator/core/runtime | Cycle/depth/queue limits; parent waiting nhả slot; no budget over-admission |
| M8-02 | Child result/delivery transaction, context grants và crash recovery | orchestrator/store/context | Commit result trước notification, duplicate delivery không nhân đôi task |
| M8-03 | Clean worktrees, ownership, integration workspace/fingerprint | orchestrator/execution | Concurrent edits không đè; dirty preservation hoặc reject rõ |
| M8-04 | Receipt-based verifier, integrated-revision checks và UI/CLI status | orchestrator/app/tests | Uncited self-report unverified; branch pass/merge fail không accepted; benchmark vs one agent |

### 3.10. M9 — Phát hành CLI

| ID | Việc giao | Owner/targets | Bằng chứng |
|---|---|---|---|
| M9-01 | Versioned backup/restore/retention, artifact reachability và migrations | store/artifacts | Restore DB+artifacts; interrupted migration; protected lineage không bị GC phá |
| M9-02 | Event taxonomy/trace/metrics, doctor và redacted support bundle | diagnostics/CLI/app | Correlation IDs xuyên tool/child; exporter down không mất transaction; secrets không mặc định export |
| M9-03 | Full regression/portability/compatibility/eval gates | tests/CI | M0–M8 required cases pass trên revision cuối; Windows/Linux evidence riêng |
| M9-04 | Packaging/checksums/install/uninstall/release notes/profile docs | release/docs | Fresh install/run/restore smoke; công bố host/strict limitations đúng |

### 3.11. M10 — Web/API tùy chọn

| ID | Việc giao | Owner/targets | Bằng chứng |
|---|---|---|---|
| M10-01 | Command/query HTTP adapter, identity/auth/origin, generated contracts | API/app | Duplicate requests, forged identity, unauthorized mutation bị chặn |
| M10-02 | SSE live/replay/gap/cursors và durable projections | API/events | Disconnect/reconnect/slow consumer/out-of-order không sai state |
| M10-03 | Task/chat/plan/diff/checks/agent tree/approval/context UI | Web | Thao tác dùng cùng app services; keyboard/a11y và tiếng Việt |
| M10-04 | Upload/artifact preview sandbox, local deployment/security gates | API/Web/tests | Traversal/size/active HTML bị giới hạn; public mode không tự bật |

### 3.12. M11 — Daemon và scheduler tùy chọn

| ID | Việc giao | Owner/targets | Bằng chứng |
|---|---|---|---|
| M11-01 | Daemon ownership, authenticated local IPC, attach/detach/shutdown | daemon/app | Client đóng không kill daemon run; hai hosts không tranh ownership |
| M11-02 | Schedules/occurrences/queue/misfire/overlap/timezone | daemon/scheduler/store | Fake-clock DST/restart/manual trigger; due occurrence không mất hay launch trùng |
| M11-03 | Durable external task handles, MCP Tasks driver/status/cancel | jobs/extensions/store | Submit timeout không submit lại mutation; restart poll/cancel và terminal receipt |
| M11-04 | Non-interactive approval handling, delivery outbox/notifications | daemon/app | Thiếu authority thì waiting/blocked; unchanged quiet; dedupe delivery |

### 3.13. M12 — Strict backend theo profile

| ID | Việc giao | Owner/targets | Bằng chứng |
|---|---|---|---|
| M12-01 | Chọn một backend, threat model/capabilities/support matrix | execution/ADR | Nêu rõ OS/container boundary, filesystem/egress/resource guarantees |
| M12-02 | Lifecycle acquire/mount/execute/cancel/release và scoped env | execution | Permissions/mounts không vượt grants; không leak host credentials/sockets |
| M12-03 | Orphan reconciliation và sandbox lease/artifact export | execution/store | Kill/restart không xóa live workspace; no orphan sau cleanup theo contract |
| M12-04 | Adversarial confinement/portability và profile release gates | tests/release | Test thực tế, không lấy worktree/Job Object làm proof sandbox |

## 4. Coverage matrix — mỗi phần có owner và nghiệm thu

| Requirement | Master plan | Mốc/owner | Failure mode phải kiểm tra |
|---|---|---|---|
| R01 Product scope/CLI modes | 1–3 | M0/M3 app | Model kết thúc nhưng task chưa đạt |
| R02 IDs/state/protocol compatibility | 4 | M0 contracts | IDs trùng, transition sai, unknown critical event |
| R03 Module/plugin boundaries | 5/11 | M0/M6 core/extensions | Dependency cycle hoặc bypass policy |
| R04 Durable admission/ownership | 6 | M1 store | ACK mất, stale writer finalization |
| R05 Tool transaction/unknown outcome | 6/10 | M4 tools/store | Crash sau side effect trước receipt |
| R06 Run/goal lifecycle | 7 | M3 runtime | Empty final, no progress, continuation vô hạn |
| R07 Human input/steering/cancel | 7 | M3/M4 app/runtime | Answer nhầm ID, cancel queue vẫn spawn |
| R08 Provider capabilities/stream | 8 | M2 providers | Multiple tool deltas/partial JSON/EOF/unsupported model |
| R09 Cost/budget/loop accounting | 8 | M3/M8 runtime | Usage missing/double count, child over-admission |
| R10 Context admission/authority | 9 | M5 context | Summary/skill injection vượt authority, request overflow |
| R11 Compaction/history/fork | 6/9 | M5 context/store | Stale CAS, mất exact source, foreign history |
| R12 Files/Git/process portability | 10 | M4 execution | Junction/path race/CRLF/locked file/process grandchildren |
| R13 Artifacts/uploads | 10 | M4/M10 artifacts/API | Truncated log không báo, traversal, untrusted HTML preview |
| R14 Skill lifecycle | 11 | M6 extensions | Catalog vs active lẫn, stale digest, script tự chạy |
| R15 MCP/resources/remote-task compatibility | 11 | M6/M11 extensions | Unsupported negotiation, cancel mất, resubmit mutation |
| R16 Code intelligence/web research | 11 | M6 integration ports | Stale index/source hoặc external content thành instructions |
| R17 Memory jobs/retrieval/invalidation | 12 | M7 memory | Lost update/cursor, revoked memory vào prompt |
| R18 Multi-agent/DAG/delivery | 13 | M8 orchestrator | Deadlock, cycle, parent crash/result mất |
| R19 Worktrees/integration acceptance | 13 | M8 orchestrator | Dirty user changes mất; final revision chưa test |
| R20 Daemon/schedule/notifications | 14 | M11 daemon | DST/misfire/double launch hoặc tự approve |
| R21 API/Web/reconnect | 15 | M10 API/Web | Forged identity, gap bị che, terminal regression |
| R22 Secrets/config/trust | 16 | M0/M4/M6 app/execution | Env/helper/socket leak, repo config nâng quyền |
| R23 Retention/migration/backup/delete | 16 | M1/M9 store | Broken lineage, DB backup thiếu artifacts/WAL |
| R24 Metrics/eval/release | 17 | M9 diagnostics/release | Zero tests false pass, secrets in telemetry, OS evidence thiếu |
| R25 Strict confinement | 10/16/18 | M12 execution | Host mode được quảng cáo như sandbox |

Đây là phạm vi coverage của bản plan, không chứng minh không có bug hoặc yêu cầu mới trong tương lai. Trước mỗi milestone, bổ sung failure mode phát hiện mới và giữ mapping đến owner/acceptance.

## 5. Bộ acceptance nền tảng

Các mã `A01–A36` là đặc tả tương lai, không phải tests đã chạy. M0 tạo registry; từng milestone bổ sung implementation và artifacts. Không map tự động sang C/K của roadmap cũ.

| ID | Kịch bản | Đạt khi | Mốc |
|---|---|---|---|
| A01 | Input retry sau ACK/crash | Một logical input, projection đúng | M1 |
| A02 | Hai writer và generation cũ | Chỉ owner hiện hành ghi/finalize | M1 |
| A03 | Tool settle rồi kill trước checkpoint | Resume dùng receipt, không chạy lại | M4 |
| A04 | Side effect xong nhưng chưa receipt | Outcome unknown, không blind retry | M4 |
| A05 | Snapshot corrupt/critical schema mới | Fallback hợp lệ hoặc block execution rõ | M1 |
| A06 | SSE nhiều tool xen kẽ/UTF-8/usage/EOF | Calls đúng pairing; error/terminal rõ | M2 |
| A07 | Auth lỗi/429/hung stream/cancel | Retry đúng loại, bounded timeout và cleanup | M2 |
| A08 | Coding read→patch→test fail→fix→pass | Final evidence ở đúng result revision | M4 |
| A09 | Model final rỗng/capped hoặc criteria thiếu | Không tự accepted task | M3 |
| A10 | Goal không có progress hoặc external wait | Bounded continuation, blocker rõ | M3 |
| A11 | Question/approval pending rồi restart/duplicate answer | Đúng ID, một lần consume, không tự approve | M3/M4 |
| A12 | User correction trong lúc compact | Candidate cũ không thắng correction | M5 |
| A13 | Hủy khi đợi process permit | Không spawn/side effect marker | M4 |
| A14 | Grant dùng cho invocation khác | Bị từ chối dù cùng action | M4 |
| A15 | Path equivalence/symlink/junction/concurrent file change | Gate không vượt scope hoặc overwrite stale file | M4 |
| A16 | Process grandchildren/timeout/env | Tree cleanup đúng, không truyền secrets mặc định | M4 |
| A17 | Log lớn/disk full/quota | Capture/truncation đúng, không false artifact | M4 |
| A18 | Năm compactions làm summary mất exact ID | Search/read nguồn để tạo output đúng | M5 |
| A19 | Summary/extractor lỗi | Resume từ WorkingState; jobs còn để catch-up | M5/M7 |
| A20 | Foreign scope/fork/revoked source | Không đọc trái quyền; grant không được copy | M5 |
| A21 | Rollback checkpoint sau file mutation | Không giả đã undo file; explicit reconciliation | M5 |
| A22 | Active skill compact/update/unavailable | Version/ref/fallback đúng, không nâng quyền | M6 |
| A23 | MCP malformed/frame flood/schema change/unload | Bounded failure, final gate vẫn áp dụng | M6 |
| A24 | Deferred discovery rồi quyền bị thu hồi | Schema visibility không cho phép execution | M6 |
| A25 | Memory concurrent publish/job crash/cursor retry | Không lost update/gap/duplicate settlement | M7 |
| A26 | Memory stale/contradiction/self-reinforcement | Provenance/supersedes/revocation đúng | M7 |
| A27 | Parent chờ children, queue/depth/budget đầy | Không deadlock/over-admit, reason rõ | M8 |
| A28 | Child commit rồi parent chết trước notify | Inbox readback một logical completion | M8 |
| A29 | Hai branches pass nhưng integration fail | Task unsatisfied/unverified, không accepted | M8 |
| A30 | Dirty repo/user edit trong lúc integrate | Bảo toàn user state hoặc reject/rebase rõ | M8 |
| A31 | Backup/restore/migration interrupted/retention | DB+artifacts+lineage nhất quán | M9 |
| A32 | Trace exporter offline/support bundle | Không ảnh hưởng commits; không secrets mặc định | M9 |
| A33 | Web reconnect/gap/out-of-order/duplicate | Reload/dedupe đúng, terminal state ổn định | M10 |
| A34 | Scheduler DST/misfire/restart/manual trigger | Occurrence/overlap policy đúng, no double launch | M11 |
| A35 | Remote submit timeout/cancel/restart poll | Không resubmit mutation mù; terminal receipt đúng | M11 |
| A36 | Strict backend confinement/orphan cleanup | Capability guarantees qua tests trên OS hỗ trợ | M12 |

Mỗi case ghi command chạy, exact source revision, environment, failpoint, expected/actual, artifact hashes và limitations. Không chỉ mock logic rồi gọi đó là integration test. Fake provider/network là boundary hợp lệ; store/driver/tool/runner thật phải tham gia ở mức acceptance.

## 6. ADR và schema phải chốt trước implementation

| ADR dự kiến | Default đề xuất | Chốt trước |
|---|---|---|
| ADR-N01 Domain identity/state | Task tách session/run/step; acceptance riêng | M0 |
| ADR-N02 Transaction authority | Một writable host, SQLite journal/projections; artifacts publish trước ref | M1 |
| ADR-N03 Execution authority | Host-issued capabilities, bound approvals, immutable receipts | M3/M4 |
| ADR-N04 Provider contract | Typed message blocks, incremental stream, capability matrix | M2 |
| ADR-N05 Context/provenance | Compiler duy nhất, no post-freeze injection, watermark CAS | M5 |
| ADR-N06 Extension ABI | Traits built-in, process protocol versioned, MCP adapter riêng | M6 |
| ADR-N07 Memory | Durable jobs + source-backed versions, FTS trước vector | M7 |
| ADR-N08 Delegation/workspace | Durable DAG, isolated editing worktrees, integration acceptance | M8 |
| ADR-N09 Data lifecycle | Retention pins/reachability, versioned backup/restore, compatibility readers | M9 |
| ADR-N10 Daemon/API transport | Authenticated clients, one app service/owner, durable event replay | M10/M11 |
| ADR-N11 Strict execution | Một backend, capabilities/test matrix cụ thể | M12 |

Không cần tạo 11 ADR rỗng ở M0. M0 chốt ranh giới và danh mục; owner của mỗi milestone hoàn tất quyết định khi đủ prototype/evidence.

## 7. Nhịp giao việc, nghiệm thu và quản lý thay đổi

Mỗi assignment gồm milestone/work-item IDs, scope, non-goals, contracts, target modules, dependencies, acceptance cases và ngân sách. Implementer bắt đầu bằng SPEC ngắn; mỗi PR chỉ nên có một thay đổi hành vi kiểm chứng được. Bắt buộc khảo sát/reuse code hiện tại và kiểm tra theo contracts; ghi test nào đã đủ, assertion nào còn thiếu để không viết lại subsystem.

Gate milestone: format/lint/type/build; required tests với discovery count; predecessor regressions; schema/backward-compat; docs/source links; review invariants; evidence/handoff. Tính accepted từ evidence ở revision cuối, không từ việc sửa status JSON. Runtime verification không bị thay bằng docs verification.

Sau M4, đo coding fixture để điều chỉnh M5–M9. Sau M5, quyết định mức local skills/MCP cần ngay; sau M7, đo memory quality trước embeddings; sau M8, đo speed/cost/conflict trước bật đa agent mặc định. Feature mới phải có owner và acceptance, hoặc ghi deferred rõ.

Prompt giao bước đầu:

```text
Triển khai M0 của kế hoạch mới tại docs/HARNESS_MASTER_PLAN.vi.md
và docs/HARNESS_ROADMAP.vi.md. Chỉ xử lý M0-01..M0-04.
Đọc INTEGRATION_MAP, khảo sát code/tests hiện tại, chỉ patch gaps theo contracts.
Giữ root workspace và binary ha; không tạo codebase/runtime/CLI thứ hai.
Giữ nguyên dữ liệu/thay đổi không liên quan. Chốt SPEC và ADR nền tảng,
nối chức năng vào modules hiện tại; mở rộng schema/state/boundary tests và registry.
Không triển khai trước M1–M12. Bàn giao source revision, commands/results,
limitations và next action. Commit/push/release theo quyền trong assignment.
```

## 8. Hoãn có chủ đích

Không nằm trong dự toán v1: distributed workers, Redis/Postgres/Kubernetes, nhiều người dùng/SSO, marketplace, tự sửa/publish skills, voice/multimedia generation, nhiều browser/search engines và mọi provider trên thị trường. CodeGraph/LSP/web fetch có ports nhưng integration chỉ được thêm khi cần.

Khi yêu cầu mới xuất hiện, cập nhật scope table, ADR, work item và acceptance; không lén thêm vào một milestone đang chạy. “Plan tổng thể” nghĩa các ranh giới và đường mở rộng đã được xác định, không có nghĩa phải xây tất cả ngay.
