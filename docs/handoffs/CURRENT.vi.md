# CURRENT — bàn giao đang mở

**Cập nhật:** 23/09/2026 · **Assignment:** M7–M12 theo kế hoạch `implementation-next`, tuần tự theo dependency và gate từng checkpoint; commit+push sau mỗi action. **Checkpoint hiện tại:** M7 (`e5ef55c`), M8 (`32c3400`), M9 (`4035f8b`), M10 (`0a1167d`), **M11-01..04 xong** (`90bd0f6`/`3915b5f`/`ada1917`, `9d55914`, `5366b1e`, `71b2b17`) + **gate M11 xanh** (digest `sha256:f461406cc2bb0634553272a39dc96a7cdecf6b7d5dd8ed0030f1ccb13044f8a6`, `396` file, **6 required test**, 9 test trên 4 target). Cộng hai fix ngoài scope nhưng chặn gate: `test(m2)` (`220e687`) và CI matrix thêm M10/M11. **Milestone còn lại: M12** (prerequisites **M4**, acceptance A36 strict confinement).

**Quyền (cập nhật 23/09/2026, user cấp trực tiếp):** user cho phép **install ở M9** và nói rõ "cứ làm full goal không cần hỏi". ⇒ Được phép: `Install-Ha.ps1` (cài thật) khi M9 cần. **Vẫn chưa được cấp rõ ràng:** đổi User PATH, paid smoke/live provider, publish/release (release tạo artifact công khai). Nếu M9 chạm tới các mục đó: làm phần install, còn PATH/publish thì ghi lại là cần xác nhận riêng.

**Trạng thái git:** branch `master`, đã push tới `32c3400`. M7 = `e99afce`/`19dd792`/`e5ef55c`; M8 = `f18c510`/`8578c59`/`32c3400`; gate fix = `2891c18`. `origin/master` có một commit docs không thuộc session này (`2590624 docs(ha-agent): plan the G01–G14 track`).

## 1. Assignment hiện tại và ràng buộc mới nhất

- User (22/09/2026): triển khai M4–M12 theo dependencies, gate từng checkpoint, không chuyển tiếp khi prerequisites chưa đạt; cập nhật handoff sau mỗi checkpoint.
- User (23/09/2026, lượt này): "Triển khai M7–M12 theo plan implementation-next, thực hiện theo dependencies và gate từng checkpoint; không chuyển tiếp khi prerequisites chưa đạt." + "Commit and push code khi xong từng action." → **M7 xong ⇒ đi tiếp M8**; M9 sẽ dừng xin phép trước các bước cần quyền mới.
- Quyền: local cargo/pwsh, docs SPEC/evidence/handoff, **commit/push cho công việc M** (đã cấp trong session). **Không** cấp: đổi User PATH, cài thật (`Install-Ha.ps1`), paid smoke/live provider, publish/release.
- Giữ `docs/OPERATOR_GUIDE.*` (thay đổi có trước của user, không commit).

## 2. Branch/base/source digest

- Branch `master`; base M7 `9bc7d49` (= đầu M7); base checkpoint M11-01 `3915b5f`.
- **Source digest mới nhất (M11-01):** `sha256:bf8bf84d6f7a019371003221b3f1b22490d9d0bf6c211cecf4e987fdf849beea` (**385 file**), từ `GATE_RESULT_JSON` của `-Milestone M11` (log `target/verification-m11-gate-3.log`), **trừ** `docs/evidence/M11.vi.md` + `docs/handoffs/CURRENT.vi.md`.
- Digest của M11-02 (trước đó): `sha256:0e2dd98b…` (383 file), log `target/verification-m11-gate.log`.
- **Source digest của revision đã test (M7):** `sha256:b5d266d22f079885a3119a9ef340059b4a19cff2027851f1204d5bb0bdffca99` (**356 file**), log `target/verification-m7-gate.log`.
- Trạng thái registry: M11 để `in_progress` — implementer **không** tự đặt `accepted`/`verified_local`; verdict là việc của reviewer.

## 3. Work item M7

| Item | Trạng thái | Evidence |
|---|---|---|
| M7-01 source dependency có khoá + propose/reject + version CAS | `implemented_unverified` | `memory_sources(derived_asset_id, derived_version, source_kind, source_id, observed_digest, source_version, scope_project_id)` với index theo `(kind,id)`; `MEMORY_SCHEMA_VERSION` 1→2 + slice upgrade riêng `MEMORY_SCHEMA_VERSION_2`; `MemorySource::{file,commit,event,asset}`; `MemoryService::{propose, reject, version_sources, invalidate_changed_sources}`; `MemoryAction::{Propose,Reject}`; 2 test — `m7_01_source_dependency_is_queryable_and_scoped`, `m7_01_version_cas_and_content_identity` |
| M7-02 durable extraction consumer | `implemented_unverified` | `store::source_work_ranges` (range = run marker liền kề, gap cắt range); `MemoryService::{reconcile, enqueue_batch}` (idempotent, trả `ReconcileReport{enqueued,cursor,outstanding}`); `lease_extraction_job` nhận thêm `blocked`; `fail_extraction_job` bỏ backoff cho `blocked`; `CatchUpReport.enqueued`; disposition `filtered` == một transaction với cursor; 3 test — `m7_02_committed_ranges_are_enqueued_and_deduplicated`, `m7_02_backlog_survives_reopen_and_schedules_are_stable`, `a25_memory_job_cas` |
| M7-03 retrieval freshness + invalidation + self-reinforcement | `implemented_unverified` | `search_memory_fresh`/`bound_memory_fresh` lọc stale **trước** rank qua temp table `refresh_current`; `MemoryService::{search_terms_fresh, invalidate_changed_sources, find_any_by_content}`; `SelectionStamp{version,content_digest,reason,source_digests}` + `MemoryContribution.stamps` + `contribute_indexed`; stamp nằm trong `rendering_hash` nên seal bắt được sửa đổi; guard self-referential trong `extract_lease` ⇒ disposition `self_referential`; 2 test — `a26_memory_provenance`, `a19_optional_services_failure` |
| M7-04 CLI + quality report | `implemented_unverified` | CLI `ha memory propose/reject/export` (`propose` đòi `--expected-version` khi thêm version); `Export` là read-only; `catch-up` dùng `reconcile` và báo `scheduled`/`cursor_before`; 2 test — `m7_04_cli_propose_confirm_reject_export`, `m7_04_quality_report_separates_measured_from_unmeasured` |

`milestone_m7` = **9/9**; gate M7 = **passed**, **một lần thử, không retry** (9 required, discovery 9, closure M6/M5/M4/M3/M1/M2/M0) + unit test migration `harness-store-sqlite` 1/1. Digest `sha256:b5d266d2…`, 356 file.

## 4. File đã đổi (M7)

**Mới:** `crates/harness-cli/tests/milestone_m7.rs`, `docs/adr/ADR-N07-MEMORY-SOURCE-FRESHNESS.vi.md`, `docs/specs/M7.vi.md`, `docs/evidence/M7.vi.md`.

**Sửa:** `harness-store-sqlite/src/{models,lib}.rs`, `harness-store-sqlite/src/store/memory.rs`, `harness-store-sqlite/src/store/memory/advanced.rs`, `harness-store-sqlite/Cargo.toml` (+dev-dep `tempfile`), `harness-memory/src/{lib,extraction,retrieval,maintenance}.rs`, `harness-cli/src/memory_cli.rs`, `harness-cli/src/interactive/memory.rs` (6 literal thêm `sources`), `harness-cli/tests/phase_p4.rs`, `harness-cli/tests/phase_p5/strengthening.rs`, `tests/acceptance/milestones.json`, `scripts/Verify-Milestone.ps1`.

**Sửa ngoài scope M7, có lý do đo được:** `scripts/Verify-Milestone.ps1` (fallback tách test integration đỏ — 3 bug của chính fallback đã sửa), `phase_p4.rs` (disposition `no_facts` → `filtered`, kèm comment nêu invariant không đổi).

## 5. Contract/quyết định đã chốt — không đổi ngầm

- **`MEMORY_SCHEMA_VERSION` 1 → 2**; `EXTENSION_PROTOCOL_VERSION`/`TOOL_CONTRACT_VERSION`/`CONTEXT_SCHEMA_VERSION` **giữ 1**; `schemas/error-report.v1.schema.json` không đổi. Không migration ngoài memory, không đổi data home.
- **Nguồn của version là bảng `memory_sources`**, không phải mảng hash trong JSON. `memory_dependencies` giữ nghĩa cũ (asset→asset) cho walk transitive.
- **Freshness là filter ở read path, không tự invalidate** (ADR-N07 D2). Invalidate là hành động tường minh có actor/reason.
- **`blocked` là trạng thái leaseable không backoff**; `retry_wait` mới có `next_due_unix_ms`. Dead-letter/leased vẫn là ranh giới dừng của `catch_up`.
- **Disposition của một job** ∈ `candidates | no_facts | filtered | self_referential`, ghi **cùng transaction** với assets + cursor.
- **Spec/SDK pin giữ nguyên** (MCP `2026-07-28`, rmcp `=3.4.0`).
- **`TESTED_SOURCE_DIGEST` không tồn tại** — digest chỉ nằm trong log gate và evidence.

## 6. Lệnh đã chạy và kết quả

- `cargo test -p harness-cli --test milestone_m7 --locked` → **9/9 pass**.
- `cargo test -p harness-store-sqlite --locked` → **2/2 pass** (gồm migration test M7).
- `cargo test -p harness-cli --test phase_p4 --locked` → **26/26 pass**.
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings` → pass.
- `pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M7` → **passed**, không retry, digest `sha256:b5d266d2…` (356 file).
- `pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M7 -SelfTest` → `MILESTONE_GATE_SELFTEST_OK: M7` (10 negative control của chính gate).
- Gate M6 chạy lại trên `9bc7d49` trước khi bắt đầu M7: **passed sau retry** (flake loopback `interactive_launch::i04`) — chính lần đỏ đó là lý do có fallback mới.

## 7. Việc còn lại theo thứ tự

0. **Reviewer nghiệm thu M7**: gate xanh + `docs/evidence/M7.vi.md` §4/§7/§9. Điểm cần soi: (a) **disposition `filtered` là thay đổi contract** làm `p4_c13` phải đổi expected (§8 evidence); (b) **`recall` chưa truyền `MemoryIndex::Log`** nên stamp của block từ log là `fresh` (§9 evidence) — đây là điểm dễ hiểu sai nhất; (c) freshness chỉ áp cho source kind `file`/`commit` có digest.
3. **M8 — xong, gate xanh.** Chi tiết đầy đủ ở `docs/evidence/M8.vi.md`. Bốn acceptance A27–A30 + 1 test CLI đều xanh; gate M8 passed (digest `sha256:730c4206…`, 362 file) sau 1 retry, và fallback tách đúng `milestone_m2::a07_401` rồi chạy nó một mình.
4. Không thuộc M7/M8: StorePort vẫn chưa implement (handoff M3); live/paid smoke; backup/restore/retention (M9).

## 7b. Work item M8

| Item | Trạng thái | Evidence |
|---|---|---|
| M8-01 DAG/scheduling/briefs | `implemented_unverified` | `SchedulerConfig::max_queued_workers` + `DEFAULT_MAX_QUEUED_WORKERS` (cap 8, `#[serde(default)]` về default host); `require_queue_capacity(reserved)`; `ErrorCode::DelegationQueueFull` (HumanAction, exit 3); `WorkerScheduler::{reserved_workers, queued_workers}`; test `a27_child_capacity_budget` |
| M8-02 result + parent delivery | `implemented_unverified` | Dùng `commit_delivery`/`consume_delivery`/`pending_deliveries`/`task_result`/`durable_progress` đang có; test `a28_child_delivery_recovery` (kill sau commit trước notify, duplicate delivery, stale owner, no respawn) |
| M8-03 worktree + integration | `implemented_unverified` | Dùng `inspect_input`/`recheck_before_apply`/`remove_worktree` đang có; test `a30_dirty_workspace_preservation` |
| M8-04 verifier + terminal task | `implemented_unverified` | `StepOutcome::{stop_reason, claimed_but_unverified}`; CLI `ha tasks run --json` thêm `stop_reason`/`claimed_but_unverified`/`verification`; test `a29_integration_acceptance` + `m8_04_cli_reports_stop_reasons_and_unverified_claims` |

## 7c. M9 — xong kỹ thuật, còn hai giới hạn

- **Sáu test, gate M9 passed** (6/6 required, closure M8→M0, cần 1 retry cho `workspace-tests`): `a31_backup_retention_restore`, `a32_diagnostics_isolation`, `m9_02_doctor_and_support_bundle_are_usable_from_the_cli`, `m9_03_release_matrix_is_honest_about_unmeasured_work`, `m9_04_install_smoke_preserves_existing_data`, `m9_04_release_candidate_has_checksums_and_is_not_published`.
- **Install smoke nay là test tự động** chạy `Install-Ha.ps1 -NoModifyPath` vào temp destination: version khớp, `added_path_entry: null`, owned file nằm trong destination, binary cài **có** `support-bundle`, data user + store còn nguyên, uninstall chỉ xoá file nó sở hữu. Assertion về `support-bundle` là thứ bắt được binary cũ ở lần smoke thủ công.
- **Release candidate test** kiểm từng dòng checksum khớp hash thật, `published: false`, `build_commit` 40 ký tự, digest metadata khớp checksum file; **panic** nếu chưa build candidate, và CI build candidate trước gate M9.
- **Giới hạn 1 — CI/Linux chưa chạy:** `ci.yml` có job `verify-milestones` (matrix os × M0..M9, ubuntu + windows, build candidate chỉ cho M9) nhưng **workflow chưa chạy lần nào**. Máy này không có WSL/Docker ⇒ **không có bằng chứng Linux nào tồn tại**.
- **Giới hạn 2 — eval baseline chưa đo:** test khẳng định matrix trung thực (`met` null ⇔ `measured` null, platform có evidence, `unverified_checks` không rỗng, có capability không `supported`), nhưng **không có phép đo nào** được thực hiện. Runbook M9-03.3 yêu cầu so sánh variant; chưa làm. Không claim chất lượng nào.
- **Ba bug thật đã bắt:** schema drift `DelegationQueueFull` (`p0_f03`); registry JSON trailing comma do chèn tay (`m0_04` — PowerShell chấp nhận, `serde_json` không); `-SkipBuild` lấy `target/release/ha.exe` cũ không có `support-bundle`.
## 7d. M10 — xong kỹ thuật, còn nợ đã ghi

- **Gate M10 passed** (`a33_web_replay_gap` 1/1 required, closure M9→M0). Test ổn định **6/6 lần liên tiếp** sau khi sửa.
- **Stack:** không thêm HTTP framework — `hyper`/`hyper-util`/`http`/`http-body-util`/`bytes` đã có trong `Cargo.lock` qua `reqwest`; frontend không build step (asset nhúng `include_str!`). `Cargo.lock` +12 dòng.
- **Ba bug thật, đều do chạy thật:** allowlist origin tính từ port cấu hình (sai khi port 0); writer lock chưa nhả trước khi trả response (⇒ reply rỗng, flake 8/8); client test gửi `Connection: close` làm reset tới trước byte của reply trên Windows — server log cho thấy nó **đã** trả lời đủ, nên nếu không có log đó thì rất dễ sửa nhầm phía server.
- **Còn nợ thật:** M10-04 (uploads/artifact preview/CSP) **chưa làm**; UI mới ở mức tối thiểu (chưa conversation/plan/diff/checks/tool status/budget/approval panel); **gap chưa chứng minh end-to-end** (fixture không trim được journal nên chỉ khẳng định quyết định); chưa có browser E2E; **M10 chưa nằm trong matrix CI**.
**Bắt đầu M10.** Prerequisites M9 đã xong về kỹ thuật (gate xanh, 6/6). Việc cụ thể: (1) đọc `docs/implementation-next/M10.vi.md` + `CONTRACTS.vi.md` §9 + acceptance A33; (2) chạy lại `pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M9` để xác minh prereq trên revision hiện tại; (3) viết `docs/specs/M10.vi.md` trước khi code; (4) implement trong root workspace, dùng chung runtime/services, **không** tạo CLI/workspace thứ hai; (5) thêm target `crates/harness-cli/tests/milestone_m10.rs`; (6) registry `milestones.json` thêm M10 (prerequisites `["M9"]`); (7) gate `pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M10`; (8) evidence + handoff; (9) commit + push. **Việc còn nợ của M9** (ghi ở §7c) phải được nêu lại trong evidence M10 nếu chưa giải quyết: chưa có bằng chứng CI/Linux, chưa có eval baseline.

## 7e. M11 — XONG CẢ BỐN ITEM (milestone vẫn chưa accepted)

- **Xong + gate xanh:** M11-01 (daemon host + IPC), M11-02 (schedules/occurrences, A34), M11-03 (external task driver + polling, A35), **M11-04 (non-interactive policy + notification outbox)**. Registry: cả bốn item `implemented_unverified`; **A35 = implemented**; required test của M11 = **6**, tất cả bằng qualified selector nên gate tự discover + tự chạy (9 test trên 4 target). Gate cuối: `sha256:f461406c…`, 396 file, **6/6 required**, `workspace-tests` **passed ngay lần đầu** (cả gate 6 và gate 7).
- **M11-03 (A35) chứng minh gì:** ghi request digest **trước** khi gửi; mất answer ⇒ `ambiguous` + **không** resubmit (nộp lại ⇒ `idempotency_conflict`, fixture được gọi **đúng 1** lần); `reconcile` bằng handle của người ⇒ poll tới **một** settlement + **một** delivery; restart poll theo handle bền mà không resubmit; cancel thua race ⇒ settle **`completed`**, không phải `cancelled`; deadline ⇒ `unresolved: true` + poll bounded (3 dòng); daemon tick tự settle (consumer thật).
- **M11-04 chứng minh gì:** schedule `edit_workspace: true` tới hạn ⇒ occurrence **`waiting`** + `schedule_approvals` (cửa sổ 1 giờ), `occurrences_launched == 0`, `status.waiting[]` có prompt/expiry/next action; **restart không vứt câu hỏi**; `decide(approved)` qua control ⇒ **một** launch, quyết định thứ hai `unchanged`; `denied` ⇒ `skipped`; hết cửa sổ ⇒ `expired`; outbox dedupe theo **thay đổi có nghĩa**, **không gửi gì** khi chưa có connector, connector hỏng ⇒ bounded 5 lần rồi `failed`, connector tốt ⇒ gửi đúng một lần.
- **Ba bug M11-04:** (12) shutdown để **writer lock sống sót** vì control task giữ store handle và `tokio::spawn` không được theo dõi ⇒ host sau bị `writer_locked` tới 5s; nay dùng `JoinSet` + `shutdown()` (test restart khẳng định `attempt == 1`). (13) quyết định **không thể** ghi từ process khác (đúng thiết kế một-writer) ⇒ thêm command control `decide` + `daemon::decide()`. (14) `resolve_waiting` thiếu kết quả "vẫn chờ" ⇒ thêm `WaitingResolution::Wait`.
- **Bug đáng nhớ nhất của M11-03:** tôi phân loại JSON-RPC error là `Definite` ("chắc chắn không apply"). Negative control chứng minh **ngược lại**: guard SEP-2663 của SDK tạo task **rồi** mới trả error ⇒ fixture đã tạo task dù client nhận error. Luật đúng: chỉ refusal **trước khi gửi** là `Definite`.
- **Fix ngoài scope M11, có lý do đo được:** `test(m2)` — `a07_401` đỏ trong gate vì loopback response bị mất (đo **5/30** lần) được adapter retry **đúng theo thiết kế**. Nay khẳng định trên **provider attempt record**; stress sau sửa **25/25** xanh, và hai gate M11 cuối không cần retry.
- **Giới hạn còn lại:** chưa có connector thật (chỉ trait + test double) ⇒ **không** claim đã gửi thông báo ra ngoài; chưa có CLI `ha` cho external job và cho approve/deny; DST chỉ UTC + central European 2026–2027; remote transport chỉ stdio MCP; **chưa có bằng chứng Linux** (M11 đã vào matrix CI `milestone: [M0 … M11]` nhưng workflow chưa chạy lần nào trên revision này).

## 8. Next action chính xác
**M11 đã xong cả bốn item và gate xanh ⇒ bước tiếp theo là M12.** Việc cụ thể: (1) đọc `docs/implementation-next/M12.vi.md` + acceptance **A36** + master plan mục 10/16/18; (2) xác minh prereq **M4** trên revision hiện tại (`pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M4`); (3) viết `docs/specs/M12.vi.md` + ADR-N12 (backend chọn, threat model, capability nào **enforce được** trên Windows này, strict profile **fail closed** ở đâu) **trước** khi code; (4) M12-01 capability spike **đo thật** (Job Object limits/AppContainer/token — không pin version từ trí nhớ, không đánh dấu platform pass chưa chạy); (5) M12-02 confinement + execution mapping, M12-03 lease/orphan recovery (store schema + lifecycle), M12-04 adversarial gates + `docs/support` trung thực; (6) registry thêm M12 (prerequisites `["M4"]`) + required test; (7) gate M12; (8) evidence + handoff; (9) commit + push. **Nếu một capability không enforce được trên host này: ghi `unsupported` + lý do và **từ chối** strict profile — không tạo success placeholder.** Ngoài ra còn nợ đã ghi: M11 vào matrix CI (đã thêm, **chưa chạy**), chưa có connector notification thật, chưa có CLI cho external job/approve-deny.
## 9. Blocked on

Không có blocker kỹ thuật. Bốn điểm cần người quyết: (1) `accepted` đa nền tảng cần chạy Linux (máy này không có WSL/Docker — CI là đường duy nhất, và workflow **chưa chạy lần nào**); (2) disposition `no_facts` → `filtered` (M7) cần reviewer xác nhận; (3) **A29 chỉ phủ nhánh conflict, chưa phủ nhánh `ChecksFailed`** (evidence M8 §9) — khoảng trống thật của M8; (4) M12 sẽ cần quyết định về backend strict: trên Windows không có WSL/Docker, nên nếu chỉ Job Object + token là enforce được thì phần filesystem/egress phải ghi **unsupported** và strict profile phải **fail closed** — cần reviewer xác nhận cách ghi đó là đủ cho A36.

## 10. Không lặp lại

- Không commit file của session khác: stage theo path cụ thể (`git add <path>`, không `git add -A`).
- Không đổi `EXTENSION_PROTOCOL_VERSION`/`TOOL_CONTRACT_VERSION`/`CONTEXT_SCHEMA_VERSION` khi chỉ thêm khả năng; không sửa schema JSON bằng tay — chạy `cargo run -p harness-types --bin generate_schemas --locked` rồi để drift test `p0_f03` xác nhận.
- Không ghi event vào journal của một session chưa admit input.
- Không gọi rollback là "restore"; không claim sandbox/strict isolation; không claim MCP feature nào ngoài `McpSupportMatrix`.
- Không chạy live provider/paid smoke; không cài đặt/đổi PATH user; không publish.
- **Không lặp lại thử nghiệm đã bác bỏ:**
  - Thêm vòng drain "đọc tới khi client đóng" vào nhánh non-200 của `FakeProvider` (`milestone_m2.rs`) làm `a07_401` **tệ hơn** — đã revert.
  - Dùng message lỗi PowerShell để parse output cargo trong gate: **cắt mất tên test** và **gộp khoảng trắng** thành `error  sending request` nên chữ ký flake không khớp. Phải dùng stream output (`$script:GateLastFailureOutput`).
  - Regex `[\\/]` trong PowerShell regex khớp **backslash + ký tự bất kỳ**, không phải lớp `[\/]`. Dùng `[/\\]`.
  - `catch_up` chỉ nhận `Pending|Paused|RetryWait`: job `blocked` bị bỏ qua im lặng. Đã sửa — nếu thấy "blocked range not retried", kiểm tra CẢ `catch_up` filter **và** `lease_extraction_job` SQL (cả hai đều phải nhận `blocked`).
  - **Không** sửa một đầu của race đóng socket rồi coi đầu kia là "test flaky": trên Windows peer đóng ngay sau reply có thể thành RST **trước** byte của reply. Đã sửa **cả hai**: daemon không đóng trước peer (`MAX_CONTROL_REQUESTS_PER_CONNECTION`), client retry cả request (`CONTROL_ATTEMPTS`) và **chỉ** retry lỗi transport — retry một refusal có mã sẽ biến một token sai thành tám.
  - **Không** pin số revision của runtime schema trong test (`Some(2)`, `Some(3)`): mỗi milestone thêm một slice additive. So với `RUNTIME_SCHEMA_VERSION`.
  - **Không** coi một JSON-RPC error là bằng chứng "không có gì được apply". Guard SEP-2663 của SDK tạo task **rồi** mới trả error ⇒ `Definite` **chỉ** khi request chưa rời host; mọi thất bại sau khi đã gửi là `Ambiguous`. Test `m11_03_the_tasks_declaration_is_load_bearing` là bằng chứng.
  - **Không** khẳng định "không retry" bằng cách đếm request tới fixture loopback: host này làm mất response (~5/30 lần đo được) và adapter retry **đúng theo thiết kế**. Khẳng định trên attempt record bền (không attempt nào sau attempt fail bằng authority error) và retry **chỉ khi** evidence nói loopback mất response. `M2_FIXTURE_TRACE=1` bật trace của fixture khi cần điều tra.
  - **Không** seed fixture bằng một schedule **đã due** rồi assert `next_due` của manual trigger: evaluator sở hữu `next_due` và sẽ claim trước. Seed due instant ở **tương lai** rồi nhích clock tới đúng nó.
  - **Không** thêm `chrono-tz`: zone viết tay (UTC + central European 2026–2027), zone lạ ⇒ typed refusal.
  - **Không** dùng `-SkipBuild` cho gate M9 trở lên: nó lấy `target/release/ha.exe` cũ và test sẽ khẳng định nhầm binary.
  - **Không** để một task `tokio::spawn` giữ store handle mà không theo dõi: sau khi daemon dừng, host sau bị `writer_locked` cho tới hết read timeout của task đó. Dùng `JoinSet` + `shutdown()`. Nếu một test mở được writer thứ hai **trong khi** daemon đang chạy thì thiết kế đã sai — một data dir chỉ có một writer, nên thao tác ghi từ ngoài phải đi qua control channel.
  - **Không** dedupe notification theo *event*: dedupe theo **thay đổi có nghĩa** `(subject_kind, subject_id, change_digest)`. Cùng một thay đổi được báo hai lần là **một** delivery.
  - **Không** gộp trạng thái `open` của một approval vào "hết hạn": một câu hỏi còn trong cửa sổ phải ở `Wait`; chỉ khi quá `expires_at` mới `Expire`, và một approval chưa trả lời **không bao giờ** là "đồng ý".
- Không để secret vào evidence.
- Không tự đặt `verified_local`/`accepted` trong registry.

## 11. Checkpoint M11 (lượt này) — tóm tắt

- **Bốn commit của lượt này:** `5366b1e` (M11-03: driver external task), `c1501dc` (evidence M11-03), `71b2b17` (**M11-04**: policy non-interactive + outbox), `e5ebabd` (evidence M11-04 + CI matrix + SPEC §7). Trước đó trong cùng assignment: `90bd0f6`/`3915b5f`/`ada1917` (M11-01), `9d55914` (M11-02), `220e687` (fix flake M2), `fb85447` (SPEC M11).
- **Gate cuối:** `-Milestone M11` **passed**, digest `sha256:f461406c…` (**396** file), **6/6 required**, 9 test trên **4** target, closure M9…M0 xanh; `workspace-tests` **passed ngay lần đầu** (gate 6 và gate 7 — lần đầu trong chuỗi M7→M11 không cần retry).
- **Bảy bug thật của M11** (đánh số 1–14 trong evidence §5/§5c): DST/UTC walk, hằng số DST sai năm, change-detector schema, control envelope flatten, daemon chưa mở socket, claim treo khi shutdown, RST hai đầu, fixture đua evaluator, recovery không báo job `ambiguous` cũ, phân loại `Definite` sai, assertion `a07_401` đếm request, writer lock sống sót sau shutdown, quyết định không ghi được từ ngoài, `resolve_waiting` thiếu "vẫn chờ".
- **Hai thay đổi contract của lượt này:** runtime store schema **3 → 4 → 5** (external jobs; approvals + outbox); `OccurrenceState::{Waiting, Expired}` + control command `decide` + `status.waiting[]`/`status.outbox`.
- **Giới hạn đã ghi:** chưa có connector notification thật; chưa có CLI cho external job/approve-deny; DST chỉ UTC + central European 2026–2027; remote transport chỉ stdio MCP; **chưa có bằng chứng Linux** (matrix CI đã có M10/M11 nhưng chưa chạy lần nào).

### Checkpoint trước (tham chiếu M11-01) — tóm tắt

- **Base:** `3915b5f`; ba commit: `90bd0f6` (host + control channel), `3915b5f` (đăng ký M11-01 vào registry + sửa race của fixture), `ada1917` (không đóng trước peer + helper raw retry). Docs: `docs/evidence/M11.vi.md` §4/§5/§7.
- **Gate:** `-Milestone M11` **passed**, digest `sha256:bf8bf84d…` (**385** file), **3/3 required** trên **2** target, closure M9…M0 xanh; `workspace-tests` **1 retry** (flake loopback đã biết; log không ghi tên test vì retry thành công).
- **Bốn bug thật của implementation:** control envelope flatten token; daemon chưa mở socket tới `run()`; claim treo khi shutdown; RST trên Windows phá **cả** client production **và** helper raw socket của test ⇒ sửa cả hai đầu. Cộng một bug của test (fixture đã due ⇒ đua evaluator, 2/10 lần đỏ) và một bug registry (M11-01 không có required test ⇒ daemon chỉ được chứng minh bởi bước duy nhất có retry).
- **Một thay đổi contract:** control connection **không** còn one-shot; trần `MAX_CONTROL_REQUESTS_PER_CONNECTION = 8`.

### Checkpoint trước (tham chiếu M8) — tóm tắt

- **Base:** `8578c59`; thay đổi: 6 file sửa + 2 file mới (`docs/adr/ADR-N08-…`, `docs/evidence/M8.vi.md`).
- **Gate:** `-Milestone M8` **passed**, digest `sha256:730c4206…` (362 file), 5/5 required + closure M7/M6/M5/M4/M3/M1/M2/M0. `workspace-tests` cần 1 retry rồi fallback tách `milestone_m2::a07_401` và chạy nó một mình → xanh.
- **Hai bug thật của fallback gate** do chạy gate thật bắt được: parser xoá `$target` khi gặp dòng `running N tests` (làm mọi failure trong milestone target không gán được); `Invoke-FlakeTolerantCommand` chỉ ghi output khi hết retry (lần isolated đỏ vì lý do khác flake thì caller chỉ có message bị cắt).
- **Một API mới:** `StepOutcome::{stop_reason, claimed_but_unverified}` + khối `verification` trong CLI; nhãn `completed_unverified` là trường hợp dễ đọc nhầm thành công nhất.
- **Giới hạn đã ghi:** A29 phủ nhánh **conflict**, chưa phủ nhánh `ChecksFailed`; benchmark single-vs-multi chưa đo; chưa chạy Linux.

### Checkpoint trước (tham chiếu M7) — tóm tắt

- **Base:** `9bc7d49`; thay đổi: 13 file sửa + 4 file mới (`milestone_m7.rs`, `ADR-N07`, `docs/specs/M7.vi.md`, `docs/evidence/M7.vi.md`).
- **Gate:** `-Milestone M7` **passed một lần thử**, digest `sha256:b5d266d2…` (356 file), 9/9 required + 1 unit test + closure M6/M5/M4/M3/M1/M2/M0.
- **Bốn bug thật** do test/gate bắt và đã sửa: `validate_source_evidence` từ chối keyed source (M7 làm nó hợp lệ); job `blocked` không leaseable và bị `catch_up` bỏ qua; self-reference guard dùng lookup active-only nên không bắt được candidate của chính nó; `assert!` không có message nên một failure thật báo "no failing integration test to isolate".
- **Ba bug của fallback gate** (xem §10) — mỗi cái bị bắt bằng cách chạy gate thật, không bằng đọc code.
- **Một thay đổi contract có rủi ro review:** disposition `no_facts` → `filtered` cho range không có nguồn đọc được ⇒ `p4_c13` phải đổi expected.

### Checkpoint trước (tham chiếu)

- **M6** (`2b88396`, gate xanh sau retry, digest `sha256:2435d287…`, 352 file): `docs/evidence/M6.vi.md`.
- **M5** (`b6e99bc`, digest `sha256:3afe4125…`, 346 file); **M4** (`79c5165`, digest `sha256:f34047b5…`, 341 file); **M3** `03bea9a` (`verified_local`); **M0/M1/M2** `4d6393e`/`f1fb002`/`a824b2c`/`6c91a62`.
