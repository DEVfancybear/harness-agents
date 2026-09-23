# CURRENT — bàn giao đang mở

## Audit đang thực hiện — P0–P7 code review và sửa defects

**Cập nhật:** 23/09/2026 · **Branch/worktree:** `codex/p0-p7-audit` tại
`C:\Users\duong\AppData\Local\Temp\harness-agents-p0-p7-audit` · **Base:**
`5498f74071aae04998e51afdfa4eeb4281f5e870`. Đây là bàn giao có hiệu lực của task
audit hiện tại; phần M7–M12 bên dưới là checkpoint/lịch sử riêng, không phải kết
quả của nhánh audit này.

- Đã sửa Rust production code: P6 cancellation guard kill đồng bộ + accounting; P4/M7
  source invalidation và descendant cascade chỉ theo lineage của phiên bản hiện hành;
  P7 archive/invalidate cũng theo nguồn hiện hành, còn forget quét mọi version lịch sử;
  journal atomic và tombstone enforcement;
  backup staging/publish atomic và verify manifest/snapshot; GC quarantine/path checks;
  pin chỉ chấp nhận artifact tồn tại; journal/tombstone store API insert-only. Sửa CLI,
  operator guide, handoff và test fixtures đồng bộ.
- RED/GREEN đã quan sát: restore directory có sẵn; status trước journal; GC DB delete
  fail; tombstone write path; manifest metadata mismatch; mtime thiếu; forget lặp;
  reason trắng; confirmation thiếu kind; backup path đã tồn tại/lỗi để snapshot dở;
  GC directory/path escape; store API ghi đè journal/tombstone; batch pin ID thiếu.
- Kết quả regression trên source hiện tại: `phase_p4` **28/28** (gồm current-source,
  stale dependency và purge lịch sử); `phase_p7` **30/30** sau khi cập nhật shared
  store query; `milestone_m12` **11/11** sau khi giữ socket tới ACK peer;
  `interactive_launch` qua trong workspace run cuối. Official P7 gate cuối **PASS**:
  format, Clippy, workspace tests, predecessor P0–P6, P7 discovery **30**, required
  **11**, docs checks đều xanh; P0/P1/P2/P3/P4/P5/P6 closure lần lượt **8/25/17/19/
  21/19/12**. Gate-time source tree: **413 files**, digest
  `sha256:e1da3e99101a7b407dbff67cc0bfe537d90b660de5080a12f9072b3974ffa37b`.
  Sau gate chỉ có evidence/handoff thay đổi, không có Rust code thay đổi. Lượt trước từng dừng ở A36 do ACK bị
  reset; listener/child nay giữ handshake tới khi peer đóng. Trước đó `i13_resume...`
  lộ fixture upload dở tiêu thụ câu trả lời; fixture nay bỏ request chưa hoàn chỉnh.
- P2 loopback server đọc hết body `Content-Length`, phục vụ bounded retries và match
  lỗi transport sau khi URL được sanitize; `phase_p2` **17/17 PASS**. Những lần gate
  đầu dừng ở test/fixture được liệt kê cùng kết quả thật trong
  [evidence P0–P7](../evidence/P0-P7_AUDIT.vi.md). `Verify-Docs.ps1 -SelfTest` pass
  sau cập nhật evidence/handoff cuối: 171 Markdown, 15 cặp ngôn ngữ, đủ 12 negative
  controls, 63 bước P0–P8.
- Có regression symlink artifact root chỉ biên dịch trên Unix; Windows hiện tại không
  chạy ca đó. Linux giữ **pending** theo yêu cầu. Retention target là kind:id global
  trong store; file ID hiện là path tương đối workspace. Backup manifest không có chữ ký.
- Delivery chỉ gồm nhánh audit `codex/p0-p7-audit`; Git history trên nhánh xác định
  revision chính xác. Không đưa thay đổi G01–G03 từ checkout chính vào.
- Linux vẫn **pending** theo chỉ dẫn người dùng. Không claim toàn workspace/Linux
  đã verified. Spec approval trước code và independent review không có; evidence
  phải ghi rõ giới hạn này.
- Checkout chính cùng G01–G03 giữ nguyên; tuyệt đối không stage/commit/push thay đổi
  đó từ nhánh này.

## Follow-up audit M7–M12 — lịch sử checkpoint trước đó

**Cập nhật:** 23/09/2026 · **Branch:** `master` · Phần này supersede trạng thái cũ bên dưới khi có mâu thuẫn; checkpoint cũ giữ làm lịch sử.

- **Đã sửa:** M7 freshness/provenance của recall; M8 A29 final-revision `ChecksFailed`; M10 Host/Origin/cursor/body/store/SSE bounds và raw-client retry idempotent; M4/M12 semantics đúng cho direct-child cleanup + CLI contracts; M11 Windows daemon endpoint probe fail-safe (PID hiện tại sống, `tasklist` CSV exact PID, output lỗi/không rõ không xóa endpoint). M11 bug được phát hiện khi gate M10 closure báo daemon thứ hai thành `writer_locked`; test ownership đã pass sau fix.
- **Kiểm tra trên source sau M11 fix:** format và `cargo build --workspace --locked` pass; M4 **14/14**, M7 **9/9**, M8 **6/6**, M10 **2/2**, M11 **9 integration tests/4 targets + 2 Windows parser unit tests**, M12 **11/11** (gồm A36 probe), `harness-memory` lib **2/2**. Ba provider-loopback integration failures trong full workspace chạy song song đều pass khi chạy riêng. Full workspace suite vì thế **chưa được ghi xanh**.
- **Gates:** M7 và M8 official gates pass trước M11 fix (lần lượt digest `sha256:dee8ea13e6b3685fb8fdface3061d4e0491b114b64833b618e5dabb89b7d2481`, `sha256:3dd01dc796a1d5381a0c7f7f93ac687b3d316b495ae61f307a3a1e93b70ad994`). M10 gate hậu kiểm sau fix chưa hoàn tất: toolchain Rust 1.97.1 thiếu `clippy-driver.exe`; metadata nói component up-to-date, remove báo file thiếu, rustup reinstall bị proxy trả `InvalidContentType`. Không coi Clippy/full gate là pass. M10 direct target và M11 ownership regressions đều pass.
- **Linux:** pending theo chỉ dẫn mới nhất của người dùng; không claim Linux verified.
- **Scope còn mở theo SPEC/ADR, không phải fix code nhỏ:** A36 cần OS confinement backend và quyết định kiến trúc; M10-04 upload/artifact preview + browser E2E chưa triển khai; M8 benchmark chưa đo; M11 chưa có notification connector thật và chưa có CLI cho external jobs/approval. Không tự đổi trạng thái reviewer-owned `planned`/`in_progress`.
- **Phần G01/HA_AGENT trong working tree thuộc task khác:** `docs/specs/HA_AGENT.vi.md`, `interactive/instructions.rs`, `interactive/prompt.rs`, và các thay đổi G01 khác giữ nguyên, không stage/commit trong lượt M7–M12.

**Commit/push:** user đã yêu cầu trực tiếp; bundle M7–M12 này sẽ được commit và push trong lượt hiện tại. SHA sẽ được trả trong kết quả task. Clippy bị chặn bởi toolchain; không ghi một kết quả giả.

**Cập nhật:** 23/09/2026 · **Assignment:** M7–M12 theo kế hoạch `implementation-next`, tuần tự theo dependency và gate từng checkpoint; commit+push sau mỗi action. **Checkpoint hiện tại:** M7 (`e5ef55c`), M8 (`32c3400`), M9 (`4035f8b`), M10 (`0a1167d`), M11 (`9d55914`), **M12-01..04 xong** (`65a084e`/`5985bd0`/`cfb215a`/`56d4f72`/`dd3c3fa`/`3030233`) + **gate M12 xanh hai lần** (cây cuối: digest `sha256:65b41f72400c58378c0f8ceeb6849a7b7c895e9cf741c0ceacfb4672c9a03cf5`, **411** file, **10 required test**, closure M4/M3/M1/M2/M0). **Milestone còn lại: không còn — M0–M12 đã triển khai xong; việc tiếp theo là reviewer nghiệm thu.**


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
- **Lịch sử tại gate đầu:** M10-04 (uploads/artifact preview/CSP), phần UI đầy đủ và browser E2E vẫn là scope chưa triển khai; fixture A33 khi đó chưa tạo gap retention thật. M10 hiện có trong matrix CI Windows. Follow-up API/SSE được ghi ở đầu handoff và `docs/evidence/M10.vi.md` §11.
**Bắt đầu M10.** Prerequisites M9 đã xong về kỹ thuật (gate xanh, 6/6). Việc cụ thể: (1) đọc `docs/implementation-next/M10.vi.md` + `CONTRACTS.vi.md` §9 + acceptance A33; (2) chạy lại `pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M9` để xác minh prereq trên revision hiện tại; (3) viết `docs/specs/M10.vi.md` trước khi code; (4) implement trong root workspace, dùng chung runtime/services, **không** tạo CLI/workspace thứ hai; (5) thêm target `crates/harness-cli/tests/milestone_m10.rs`; (6) registry `milestones.json` thêm M10 (prerequisites `["M9"]`); (7) gate `pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M10`; (8) evidence + handoff; (9) commit + push. **Việc còn nợ của M9** (ghi ở §7c) phải được nêu lại trong evidence M10 nếu chưa giải quyết: chưa có bằng chứng CI/Linux, chưa có eval baseline.

## 7e. M11 — XONG CẢ BỐN ITEM (milestone vẫn chưa accepted)

- **Xong + gate xanh:** M11-01 (daemon host + IPC), M11-02 (schedules/occurrences, A34), M11-03 (external task driver + polling, A35), **M11-04 (non-interactive policy + notification outbox)**. Registry: cả bốn item `implemented_unverified`; **A35 = implemented**; required test của M11 = **6**, tất cả bằng qualified selector nên gate tự discover + tự chạy (9 test trên 4 target). Gate cuối: `sha256:f461406c…`, 396 file, **6/6 required**, `workspace-tests` **passed ngay lần đầu** (cả gate 6 và gate 7).
- **M11-03 (A35) chứng minh gì:** ghi request digest **trước** khi gửi; mất answer ⇒ `ambiguous` + **không** resubmit (nộp lại ⇒ `idempotency_conflict`, fixture được gọi **đúng 1** lần); `reconcile` bằng handle của người ⇒ poll tới **một** settlement + **một** delivery; restart poll theo handle bền mà không resubmit; cancel thua race ⇒ settle **`completed`**, không phải `cancelled`; deadline ⇒ `unresolved: true` + poll bounded (3 dòng); daemon tick tự settle (consumer thật).
- **M11-04 chứng minh gì:** schedule `edit_workspace: true` tới hạn ⇒ occurrence **`waiting`** + `schedule_approvals` (cửa sổ 1 giờ), `occurrences_launched == 0`, `status.waiting[]` có prompt/expiry/next action; **restart không vứt câu hỏi**; `decide(approved)` qua control ⇒ **một** launch, quyết định thứ hai `unchanged`; `denied` ⇒ `skipped`; hết cửa sổ ⇒ `expired`; outbox dedupe theo **thay đổi có nghĩa**, **không gửi gì** khi chưa có connector, connector hỏng ⇒ bounded 5 lần rồi `failed`, connector tốt ⇒ gửi đúng một lần.
- **Ba bug M11-04:** (12) shutdown để **writer lock sống sót** vì control task giữ store handle và `tokio::spawn` không được theo dõi ⇒ host sau bị `writer_locked` tới 5s; nay dùng `JoinSet` + `shutdown()` (test restart khẳng định `attempt == 1`). (13) quyết định **không thể** ghi từ process khác (đúng thiết kế một-writer) ⇒ thêm command control `decide` + `daemon::decide()`. (14) `resolve_waiting` thiếu kết quả "vẫn chờ" ⇒ thêm `WaitingResolution::Wait`.
- **Bug đáng nhớ nhất của M11-03:** tôi phân loại JSON-RPC error là `Definite` ("chắc chắn không apply"). Negative control chứng minh **ngược lại**: guard SEP-2663 của SDK tạo task **rồi** mới trả error ⇒ fixture đã tạo task dù client nhận error. Luật đúng: chỉ refusal **trước khi gửi** là `Definite`.
- **Fix ngoài scope M11, có lý do đo được:** `test(m2)` — `a07_401` đỏ trong gate vì loopback response bị mất (đo **5/30** lần) được adapter retry **đúng theo thiết kế**. Nay khẳng định trên **provider attempt record**; stress sau sửa **25/25** xanh, và hai gate M11 cuối không cần retry.
- **Giới hạn còn lại:** chưa có connector thật (chỉ trait + test double) ⇒ **không** claim đã gửi thông báo ra ngoài; chưa có CLI `ha` cho external job và cho approve/deny; DST chỉ UTC + central European 2026–2027; remote transport chỉ stdio MCP; **chưa có bằng chứng Linux** (M11 đã vào matrix CI `milestone: [M0 … M11]` nhưng workflow chưa chạy lần nào trên revision này).

## 7f. M12 — XONG CẢ BỐN ITEM (milestone vẫn chưa accepted)

- **Xong + gate xanh:** M12-01 (capability spike + backend selection), M12-02 (confinement + execution mapping),
  M12-03 (ownership leases + orphan recovery), M12-04 (adversarial gates + profile documentation). Registry: cả bốn
  item `implemented_unverified`; **10 required test** (tất cả bằng qualified selector nên gate tự discover + tự chạy).
  Gate cuối: `sha256:65b41f72…`, **411** file, **10/10 required**, closure M4/M3/M1/M2/M0 xanh, `workspace-tests`
  passed **sau 1 retry** (flake loopback đã biết `interactive_launch::i13…`, tách ra chạy một mình thì xanh — **không**
  test M12 nào đỏ). Self-test gate: `MILESTONE_GATE_SELFTEST_OK: M12` (9 negative control).
- **M12 chứng minh gì:** ma trận capability **đo được** trên host thật (`process_containment`, `process_tree_kill`,
  `environment_allowlist`, `deadline_enforced`, `output_bounds` = `enforced`; `filesystem_read/write_confinement`,
  `network_egress_denial`, `credential_socket_denial`, `resource_limit_memory/process_count` = `unsupported` **kèm
  bằng chứng escape hoặc lý do cấu trúc**); `strict` (= Full) **bị từ chối** với mã `strict_isolation_unavailable`
  nêu tên 6 capability thiếu và **không** spawn gì; profile `containment` phục vụ được; lease ghi **trước** khi
  process tồn tại, reconcile chỉ settle lease mà nó **acquire được lock do OS giữ**, owner còn sống ⇒ `still_owned`
  và row **không** bị sửa; export artifact kiểm digest và từ chối đường dẫn ra ngoài export root; support matrix trong
  `docs/support` được test **so trực tiếp** với matrix đo được.
- **Chín bug thật:** (1) `reaped_on_exit` **không** chứng minh job rỗng (probe `spawn-detach`: run trả về sau 106 ms
  trong khi 4 descendant vẫn ghi; thứ giết cây là việc thả handle job) — **không sửa**, đây là hợp đồng M4;
  (2) descendant thừa hưởng handle stdio *inheritable* của cha ⇒ đọc tới EOF mất 7.9 s cho cây lifetime 9 s;
  (3) seam host-env tra tên case-sensitive ⇒ `PATH` không khớp `Path`, **mọi** child tool mất search path (M4 `a16`
  bắt được); (4) lock lease nằm dưới spool root ⇒ M4 `a17` đỏ; (5) probe M12-01 phụ thuộc tải (sampling không được
  serialize) ⇒ verdict lật sang `Unsupported`; (6) canary network đua với process permit host-wide; (7) fixture của
  probe tự sai tham số (chính nó làm lộ bug 1); (8) registry: case `planned` không được có `selectors` (`m0_04` bắt);
  (9) gate `format` đỏ vì test CLI thêm sau lần fmt cuối.
- **Giới hạn còn lại:** **A36 vẫn `planned`** — clause "denied filesystem/network/credential-socket" **không** chứng
  minh được trên host này (không có cơ chế OS nào; probe đo escape), nên strict phải từ chối thay vì giả vờ; **chưa có
  bằng chứng Linux** (CI hiện **chỉ chạy Windows** — `504cf95` của session khác thu hẹp phạm vi — và workflow **chưa
  chạy lần nào** trên revision này); không có confinement backend
  (AppContainer cần crate opt-out `unsafe` + ADR — **cần người quyết**); bộ probe tốn ~60 s và `P-MEM` cấp phát thật 256 MiB.

## 8. Next action chính xác

Hoàn tất fmt/clippy và chạy full verification gates M7, M8, M10, M12 trên Windows; cập nhật digest/log vào các evidence §11 và phần đầu handoff; stage theo path cụ thể để không kéo G01 vào; commit và push lên `origin/master`. Sau khi push, bàn giao các mục deferred/decision-required ở phần đầu để task tiếp theo có trạng thái chính xác.
## 9. Blocked on / quyết định còn cần

- Không có blocker để hoàn tất commit/push của lượt audit này.
- Linux verification đang **pending theo yêu cầu người dùng**; không chặn local Windows gates.
- A36 vẫn `planned`: muốn strict filesystem/network/socket confinement cần chọn và triển khai backend OS phù hợp, có ADR/probe mới. Không tự bật `unsafe` hoặc đổi nghĩa `strict`.
- Reviewer vẫn quyết định `verified_local`/`accepted` và việc disposition M7 `filtered` có khớp contract; implementer không ghi verdict thay reviewer.
- M10-04/UI đầy đủ, benchmark M8, connector notification thật và CLI external-job/approval là scope còn thiếu hoặc phụ thuộc cấu hình. Không được mô tả chúng như đã hoàn tất bởi các sửa lỗi audit này.


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
- **Không lặp lại thử nghiệm đã bác bỏ (M12):**
  - **Không** dùng `reaped_on_exit` như bằng chứng "job rỗng". Đo được: một run mà direct child thoát trước descendant
    trả về với nhãn đó **trong khi cây vẫn đang ghi**; cái giết cây là `KILL_ON_JOB_CLOSE` lúc thả handle. Nếu cần
    "đã reap", phải **đo** (tick file ngừng tăng), không đọc nhãn.
  - **Không** đọc output của một process tới EOF khi cây có thể còn sống: descendant thừa hưởng handle *inheritable*
    của cha nên write end của pipe vẫn mở (đo: 7.9 s cho cây lifetime 9 s). Giết cây **trước**, drain **sau**, và
    control thì drain có trần.
  - **Không** tra tên biến môi trường theo case-sensitive trên Windows: `Path` ≠ `PATH` ⇒ child mất search path. Dùng
    luật của nền tảng (`same_name`).
  - **Không** đặt lock/lease dưới spool root: `a17_output_quota` khẳng định spool rỗng sau khi publish capture. Trạng
    thái vòng đời thuộc data dir của store.
  - **Không** kết luận một probe từ **một** cặp before/after: execution được serialize nhưng sampling thì không, nên
    dưới tải song song một writer tới muộn lật verdict. Serialize cả bộ probe theo process **và** chỉ kết luận khi hai
    cửa sổ liên tiếp đồng ý.
  - **Không** đua một canary với process permit host-wide: đọc connection **sau** khi run xong (OS đã buffer), thay vì
    `accept` song song rồi hết hạn.
  - **Không** set `selectors` cho acceptance case `planned`: `m0_04_milestone_registry_and_gate_self_test` từ chối
    (danh sách required test của milestone là danh sách khác).
  - **Không** thêm test sau lần `cargo fmt` cuối: bước `format` của gate sẽ đỏ và phải chạy lại gate.
  - **Không** hạ cấp `strict` thành containment dù có "thông báo": tên gọi đắt hơn một refusal. Matrix là dữ liệu;
    thiếu measurement thì **từ chối**, không đoán.


## 11. Checkpoint M12 (lượt này) — tóm tắt

- **Sáu commit:** `65a084e` (SPEC M12 + ADR-N12), `5985bd0` (M12-01: capability probe + từ chối có tên),
  `cfb215a` (M12-02: execution plan + export có digest), `56d4f72` (M12-03: lease + reconcile + store slice 6),
  `dd3c3fa` (M12-04: A36 + support matrix + registry), `3030233` (fmt).
- **Gate cuối:** `-Milestone M12` **passed hai lần** (digest cây cuối `sha256:65b41f72…`, **411** file), **10/10
  required**, closure M4/M3/M1/M2/M0; mỗi lần `workspace-tests` **1 retry** cho flake loopback đã biết, ở hai test khác
  nhau cùng họ `interactive_launch` (`i13`, rồi `i03`), cả hai xanh khi chạy một mình. `-SelfTest`:
  `MILESTONE_GATE_SELFTEST_OK: M12`.
- **Hai thay đổi contract của lượt này:** runtime store schema **5 → 6** (`backend_leases`, unique
  `(host_id, tool_execution_id)`); `IsolationMode::Strict` **giữ** nghĩa Full và **bị từ chối** trên host này (không
  nới nghĩa, không hạ cấp). `tool-execution-receipt.v1` và `ToolCapabilities` **không** đổi.
- **Một đính chính nền tảng:** host này là **build 26200 (Windows 11, 25H2)**, không phải `19045`. Đã sửa ở SPEC M12 §2
  và ghi đính chính vào evidence M9/M10. Matrix **đo** `os_version` ở mỗi lần probe thay vì viết lại chuỗi cũ.
- **Chín bug thật** (§7f) — trong đó hai bug do **closure M4** bắt (case-sensitive env, spool root), một bug do
  **gate** bắt (format), một bug do **self-test registry** bắt (selectors của case `planned`).
- **Giới hạn đã ghi:** A36 `planned` (không chứng minh được clause filesystem/network/socket); chưa có bằng chứng
  Linux/CI; không có confinement backend (cần quyết định về crate `unsafe` opt-out); `tree_cleanup_confirmed` mạnh hơn
  bằng chứng (hợp đồng M4, chưa sửa); CLI `export|leases|reconcile` chưa có test tự động.

### Checkpoint trước (tham chiếu M11) — tóm tắt

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
