# CURRENT — bàn giao đang mở

**Cập nhật:** 23/09/2026 · **Assignment:** M7–M12 theo kế hoạch `implementation-next`, tuần tự theo dependency và gate từng checkpoint; commit+push sau mỗi action. **Checkpoint hiện tại:** **M7-01..M7-04 xong, gate M7 đã xanh** (chi tiết §11). Tiền nhiệm: M0/M1/M2 (`4d6393e`/`f1fb002`, `a824b2c`, `6c91a62`), M3 (`03bea9a`), M4 (`79c5165`), M5 (`b6e99bc`/`b8fa717`), M6 (`2b88396`/`9bc7d49`).

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
1. **Flake loopback của host vẫn còn** ở `interactive_launch::i04`/`i13` và `milestone_m2::a07`. Gate đã có fallback tách-test, nhưng **chưa có ai sửa gốc**. Việc của M2.
2. **Gate đa nền tảng**: `milestone_m5`/`milestone_m6`/`milestone_m7` **không** nằm trong `ci.yml` (chỉ P0/P3–P7 trên ubuntu+windows). Muốn `accepted` đa nền tảng phải thêm bước gate M vào CI hoặc chạy trên Linux/WSL.
3. **M8** — prerequisites M7 đã xong. Đọc `docs/implementation-next/M8.vi.md`, xác minh gate M7 trên revision hiện tại, rồi viết SPEC M8 trước khi code.
4. Không thuộc M7: StorePort vẫn chưa implement (handoff M3); live/paid smoke; backup/restore/retention (M9).

## 8. Next action chính xác

**Đi tiếp M8.** Đọc `docs/implementation-next/M8.vi.md` + `CONTRACTS.vi.md` §8 + acceptance A27–A30; lấy revision/status mới nhất; xác minh gate M7 trên revision hiện tại; viết `docs/specs/M8.vi.md` (path map, gap inventory, oracle, commands) **trước** khi code; dùng `crates/harness-orchestrator/src/{coordinator,scheduler,workspace,integration,contracts}.rs` và `harness-cli/src/delegation_cli.rs` đang có; thêm target `crates/harness-cli/tests/milestone_m8.rs`; registry `milestones.json` thêm M8 (prerequisites `["M7"]`); gate `pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M8`; evidence + handoff; commit + push.

## 9. Blocked on

Không có blocker kỹ thuật cho M7. Ba điểm cần người quyết: (1) `accepted` đa nền tảng cần chạy Linux (máy này không có WSL/Docker — CI là đường duy nhất); (2) đổi disposition `no_facts` → `filtered` cho range không có nguồn đọc được cần reviewer xác nhận; (3) flake loopback cần một lượt M2 riêng nếu muốn `workspace-tests` xanh ổn định không cần fallback.

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

## 11. Checkpoint M7 (lượt này) — tóm tắt

- **Base:** `9bc7d49`; thay đổi: 13 file sửa + 4 file mới (`milestone_m7.rs`, `ADR-N07`, `docs/specs/M7.vi.md`, `docs/evidence/M7.vi.md`).
- **Gate:** `-Milestone M7` **passed một lần thử**, digest `sha256:b5d266d2…` (356 file), 9/9 required + 1 unit test + closure M6/M5/M4/M3/M1/M2/M0.
- **Bốn bug thật** do test/gate bắt và đã sửa: `validate_source_evidence` từ chối keyed source (M7 làm nó hợp lệ); job `blocked` không leaseable và bị `catch_up` bỏ qua; self-reference guard dùng lookup active-only nên không bắt được candidate của chính nó; `assert!` không có message nên một failure thật báo "no failing integration test to isolate".
- **Ba bug của fallback gate** (xem §10) — mỗi cái bị bắt bằng cách chạy gate thật, không bằng đọc code.
- **Một thay đổi contract có rủi ro review:** disposition `no_facts` → `filtered` cho range không có nguồn đọc được ⇒ `p4_c13` phải đổi expected.

### Checkpoint trước (tham chiếu)

- **M6** (`2b88396`, gate xanh sau retry, digest `sha256:2435d287…`, 352 file): `docs/evidence/M6.vi.md`.
- **M5** (`b6e99bc`, digest `sha256:3afe4125…`, 346 file); **M4** (`79c5165`, digest `sha256:f34047b5…`, 341 file); **M3** `03bea9a` (`verified_local`); **M0/M1/M2** `4d6393e`/`f1fb002`/`a824b2c`/`6c91a62`.
