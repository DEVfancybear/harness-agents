# CURRENT — bàn giao đang mở

**Cập nhật:** 23/09/2026 · **Assignment:** M7–M12 theo kế hoạch `implementation-next`, tuần tự theo dependency và gate từng checkpoint; commit+push sau mỗi action. **Checkpoint hiện tại:** M7 (`e5ef55c`), M8 (`32c3400`), M9 (`4035f8b`) xong + gate xanh; **M10 xong về kỹ thuật + gate xanh** (`5c20c3b` → evidence, digest `sha256:9f0242e8…`, 377 file, 1/1 required). **Còn nợ:** M9 CI/Linux + eval baseline; M10-04 (uploads/preview), UI đầy đủ, gap end-to-end, browser E2E, M10 chưa vào matrix CI. **Milestone kế tiếp: M11** (prerequisites M9, đã xong).

**Quyền (cập nhật 23/09/2026, user cấp trực tiếp):** user cho phép **install ở M9** và nói rõ "cứ làm full goal không cần hỏi". ⇒ Được phép: `Install-Ha.ps1` (cài thật) khi M9 cần. **Vẫn chưa được cấp rõ ràng:** đổi User PATH, paid smoke/live provider, publish/release (release tạo artifact công khai). Nếu M9 chạm tới các mục đó: làm phần install, còn PATH/publish thì ghi lại là cần xác nhận riêng.

**Trạng thái git:** branch `master`, đã push tới `32c3400`. M7 = `e99afce`/`19dd792`/`e5ef55c`; M8 = `f18c510`/`8578c59`/`32c3400`; gate fix = `2891c18`. `origin/master` có một commit docs không thuộc session này (`2590624 docs(ha-agent): plan the G01–G14 track`).

## 1. Assignment hiện tại và ràng buộc mới nhất

- User (22/09/2026): triển khai M4–M12 theo dependencies, gate từng checkpoint, không chuyển tiếp khi prerequisites chưa đạt; cập nhật handoff sau mỗi checkpoint.
- User (23/09/2026, lượt này): "Triển khai M7–M12 theo plan implementation-next, thực hiện theo dependencies và gate từng checkpoint; không chuyển tiếp khi prerequisites chưa đạt." + "Commit and push code khi xong từng action." → **M7 xong ⇒ đi tiếp M8**; M9 sẽ dừng xin phép trước các bước cần quyền mới.
- Quyền: local cargo/pwsh, docs SPEC/evidence/handoff, **commit/push cho công việc M** (đã cấp trong session). **Không** cấp: đổi User PATH, cài thật (`Install-Ha.ps1`), paid smoke/live provider, publish/release.
- Giữ `docs/OPERATOR_GUIDE.*` (thay đổi có trước của user, không commit).

## 2. Branch/base/source digest

- Branch `master`; base M7 `9bc7d49` (= đầu M7).
- **Source digest của revision đã test:** `sha256:b5d266d22f079885a3119a9ef340059b4a19cff2027851f1204d5bb0bdffca99` (**356 file**), từ `GATE_RESULT_JSON` của `-Milestone M7` (log `target/verification-m7-gate.log`), **trừ** `docs/evidence/M7.vi.md` + `docs/handoffs/CURRENT.vi.md`.
- Trạng thái registry: M7 để `in_progress` — implementer **không** tự đặt `accepted`/`verified_local`; verdict là việc của reviewer.

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

## 8. Next action chính xác

**Bắt đầu M11.** Prerequisites M9 đã xong. Việc cụ thể: (1) đọc `docs/implementation-next/M11.vi.md` + acceptance A34/A35; (2) chạy lại `pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M10` để xác minh prereq trên revision hiện tại; (3) viết `docs/specs/M11.vi.md` + ADR-N11 (daemon/scheduler ownership, clock semantics, external handle) **trước** khi code; (4) implement trong root workspace, dùng chung runtime/services, **không** tạo daemon authority thứ hai; (5) thêm target `crates/harness-cli/tests/milestone_m11.rs`; (6) registry `milestones.json` thêm M11 (prerequisites `["M9"]`); (7) gate `pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M11`; (8) evidence + handoff; (9) commit + push. Sau M11 còn **M12** (prerequisites **M4**, không phụ thuộc M10/M11).
**Hoàn tất M9-03/M9-04 trước khi sang M10.** Cụ thể: (1) thêm `m9_04_install_smoke_preserves_existing_data` vào `milestone_m9.rs` — chạy `Install-Ha.ps1 -NoModifyPath` vào temp destination, kiểm version/checksum/subcommand/uninstall-giữ-data; (2) thêm `m9_04_release_candidate_has_checksums_and_no_secrets` — kiểm `checksums.txt` khớp `ha.exe` và `ha.release.json` có `published: false`; (3) ghi eval baseline trung thực (`measured: None` khi chưa đo) và/hoặc test `m9_03_*`; (4) chạy lại gate M9; (5) push để CI chạy `verify-milestones` lần đầu rồi ghi kết quả vào evidence; (6) commit + push. Sau đó **M10** (`M10.vi.md`, prerequisites M9; M10 và M11 **cùng** phụ thuộc M9).

## 9. Blocked on

Không có blocker kỹ thuật. Bốn điểm cần người quyết: (1) `accepted` đa nền tảng cần chạy Linux (máy này không có WSL/Docker — CI là đường duy nhất); (2) disposition `no_facts` → `filtered` (M7) cần reviewer xác nhận; (3) flake loopback cần một lượt M2 riêng — gate M7/M8 chỉ xanh được nhờ fallback tách test; (4) **A29 chỉ phủ nhánh conflict, chưa phủ nhánh `ChecksFailed`** (xem evidence M8 §9) — đây là khoảng trống thật của M8, cần một lần bổ sung.

Không có blocker kỹ thuật. Bốn điểm cần người quyết: (1) `accepted` đa nền tảng cần chạy Linux (máy này không có WSL/Docker — CI là đường duy nhất); (2) đổi disposition `no_facts` → `filtered` cho range không có nguồn đọc được cần reviewer xác nhận; (3) flake loopback cần một lượt M2 riêng nếu muốn `workspace-tests` xanh ổn định không cần fallback; (4) quyền mới (đổi PATH/cài thật/paid smoke/publish) chưa được cấp và sẽ chỉ cần ở M9.

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
- Không để secret vào evidence.
- Không tự đặt `verified_local`/`accepted` trong registry.

## 11. Checkpoint M8 (lượt này) — tóm tắt

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
