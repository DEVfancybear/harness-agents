# Evidence rà soát P0–P7 — 23/09/2026

[English](P0-P7_AUDIT.en.md) | Tiếng Việt

## 1. Kết quả

Audit source P0–P7 trên worktree sạch tại base
`5498f74071aae04998e51afdfa4eeb4281f5e870`. Đây là rà soát theo source/revision cụ
thể, không phải chứng nhận độc lập.

Linux giữ **pending** theo yêu cầu. Không gọi provider trả phí, không cài app,
không publish artifact. Checkout chính và thay đổi G01–G03 được giữ nguyên.

## 2. Sửa production code và test contract

| Area | Change | Regression/evidence |
|---|---|---|
| P6 extension transport | RAII guard luôn giảm inflight khi future bị drop; guard vô hiệu hoá handle, settle các waiter là uncertain và gọi `start_kill` đồng bộ trước khi schedule reap bounded. Recheck alive dưới pending lock chặn call đua cancel. | `m6_02_dropping_a_call_future_releases_and_stops_its_extension`: 1/1 pass; RED trước implementation chưa đo riêng. |
| P7 source retention | Invalidation đọc `memory_sources` có scope thay vì nhầm lineage; maintenance duyệt mọi project và asset phụ thuộc xuyên tầng; archive/invalidate/forget có semantics riêng. Forget xóa payload/FTS/lineage, tombstone và journal trong transaction fenced; version write kiểm tombstone cùng transaction. | `phase_p7`: retention, cascade, journal rollback, tombstone write và revision. |
| P7 journal atomicity | Status + FTS + invalidation record và journal entry của invalidate/archive cùng commit. Lỗi journal rollback toàn bộ. | `p7_s04_retention_status_and_journal_commit_together`: quan sát RED — lỗi journal nhưng status thành `Invalidated`; sau fix assertion `Active` pass. |
| P7 backup/restore | Backup chỉ nhận path mới, xây database/artifacts/manifest trong sibling staging và publish sau verify; mọi lỗi dọn staging. Đường đọc canonicalize và bắt buộc file thường nằm trong root, kiểm SQLite integrity/hash/length và khớp manifest với DB snapshot. Restore cũng staging, từ chối mọi path có sẵn. | RED/GREEN: destination cũ bị ghi; lỗi artifact để lại DB dở; manifest được sửa rồi rehash vẫn qua; restore đích rỗng cũ bị ghi. Test tương ứng nằm trong `phase_p7`; tất cả GREEN trong 30/30 suite cuối. |
| P7 artifact GC/pins | GC kiểm `artifacts/<id>.bin`, canonical parent, regular file; quarantine rollback khi SQL/commit lỗi hoặc future drop. Batch pin validate mọi ID trước insert; GC chỉ commit khi không có pin/reference trong cùng writer transaction. | RED/GREEN: DELETE fail mất bytes; thư mục artifact bị xóa; pin ID không tồn tại được báo thành công. Các regressions P7 GREEN; case symlink root có regression `cfg(unix)` nhưng chưa chạy trên host Windows. |
| P7 journal/tombstone | API retention và API store mức thấp dùng plain INSERT; journal không replace theo ID, tombstone không upsert; journal target giữ `kind:id`. Confirmation forget phải khớp đầy đủ `source_kind:source_id`. Retention target/file identity là global trong store; file ID hiện là workspace-relative path. | RED/GREEN: forget nhận ID-only token; store API lặp tombstone/entry ghi đè lịch sử. `p7_s04_forget_confirmation_binds_to_the_full_target`, `p7_s04_store_tombstone_api_preserves_existing_audit_rows`, retention journal assertions. |
| P7 retention/GC edge cases | Lý do trắng bị từ chối; tombstone lặp bị từ chối; mtime thiếu/lỗi là tuổi 0; grace period được kiểm tra lại trong transaction thu gom; pin batch lỗi không ghi một phần; asset/grant/binding deletion tăng `memory_revision`. | `phase_p7`, toàn suite cuối. |
| P1/P7 revision triggers | Xóa asset/grant/binding vẫn tăng `memory_revision`, để cache/version observers thấy thay đổi. | Chạy qua phase gate P7 cuối; có kiểm tra tại store schema. |
| CLI/provider loopback fixtures | Đọc đủ request body theo Content-Length trước khi đóng socket; server đáp ứng retry bounded; retry match thông báo đã sanitize đúng; giữ readiness và accept serialization. | `phase_p2`: 17/17; `interactive_launch`: 19/19 pass trên Windows; chưa chứng minh loại bỏ mọi flake trên host khác. |
| P1/P7 test contracts | P1 tests dùng `STORE_SCHEMA_VERSION`; P7 newer-schema fixture chỉ nâng row revision mới nhất để giữ PK uniqueness và so với hằng schema hiện hành. Các test tổng hợp dài được tách helper mà giữ nguyên assertions. | `milestone_m1`, `phase_p1`, `phase_p7`; Clippy workspace pass. |
| P4/M7 source invalidation contract | Sửa fixture semantic-merge cũ: legacy `source_file_hashes` chỉ là digest, không định danh path theo ADR M7; fixture dùng `MemorySource::file(relative_path, digest)` rồi invalidates đúng path. Không đổi production expectation. | `phase_p4::extended::p4_source_changes_semantic_merge_and_binding_are_versioned`: 1/1 pass sau cập nhật. |
| P4/M7 current-version lineage | Source invalidation và archive/invalidate duyệt source rows gắn với `memory_assets.current_version`; descendant traversal chỉ theo `memory_dependencies.derived_version` hiện hành. Nguồn/lineage cũ không làm stale hoặc archive asset hiện tại sau correction. | RED: bỏ current-version dependency filter khiến invalidation trả thêm summary đã chuyển lineage; GREEN: `p4_source_invalidation_only_uses_the_current_version_sources`. Toàn `phase_p4`: 28/28. |
| P7 historical forget | Forget cố ý đi ngược mọi source/dependency version lịch sử để xóa cả asset vẫn giữ bản cũ có dữ liệu nguồn đã quên; archive/invalidate chỉ ảnh hưởng current lineage. Cả xóa asset, version, FTS, grants/bindings, dependency và journal/tombstone đều atomic. | RED: current-only root selection quên **0** asset trong khi lịch sử còn 2; GREEN: `p4_source_forget_removes_assets_that_only_match_historical_lineage` xóa cả 2 asset và giữ asset độc lập. |
| M12 egress evidence | Canaries đọc đủ nonce và ACK. Child chỉ báo success sau ACK; listener giữ socket mở tới khi child đóng để ACK không bị reset trước khi peer đọc. Timeout hai đầu bounded và diagnostic nêu listener. | `a36_strict_confinement`: 1/1; `milestone_m12`: 11/11 pass sau lần thay đổi handshake cuối. |
| P0–P5 review | Đối chiếu finding lịch sử approval/compaction/process cancellation và review P4/P5; cập nhật trạng thái/handoff lịch sử, không coi test cũ là gate của source audit này. | `milestone_m4::a14_approval_binding`, `milestone_m5::a12_compaction_cas`, `milestone_m3::m3_02_canceled_queue_does_not_invoke`; gate cuối bên dưới. |
| Docs | Đồng bộ status README/runbooks; sửa trạng thái P4 và mâu thuẫn CI P6; ghi rõ revision lịch sử, target global và Linux pending. | `Verify-Docs.ps1 -SelfTest`: **PASS** sau cập nhật cuối; 171 Markdown, 15 cặp ngôn ngữ, C01–C30, K01–K14, R01–R12, W01–W06, P0–P8 63 bước, P0–P7 53–76 person-days; đủ 12 negative controls. |

Các regression mới không làm nới assertion cũ. P7 migration fixture bị phát hiện
đỏ do nâng tất cả PK version rows cùng lúc; fixture được sửa để thay đúng revision
hiện hành, không đổi contract hay production behavior.

SPEC này được viết trước phần rà tiếp và bổ sung append-only khi phát hiện các
finding mới. User đã yêu cầu thực hiện audit/sửa, nhưng **chưa có review độc lập
hoặc phê duyệt SPEC trước code**; mức tin cậy vì vậy không phải chứng nhận ngoài.

## 3. Verification

| Command | Result |
|---|---|
| `cargo fmt --all -- --check` | **PASS** sau khi bỏ comment trùng trong store. |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | **PASS** trên source tree audit hiện tại, Rust 1.97.1 |
| `cargo test -p harness-cli --test phase_p4 --locked` | **28/28 pass** sau khi current-version invalidation và historical forget được tách |
| `cargo test -p harness-cli --test phase_p7 --locked -- --test-threads=1` | **30/30 pass** sau shared store query change (current audit source) |
| `cargo test -p harness-cli --test milestone_m12 --locked` | **11/11 pass** sau handshake ACK keep-alive cuối |
| `cargo test -p harness-cli --test milestone_m6 m6_02_dropping_a_call_future_releases_and_stops_its_extension --locked -- --exact` | **1/1 pass**; workspace/predecessor gate cuối cũng pass P6 required selectors (12). |
| `cargo test -p harness-cli --test interactive_launch --locked` | **Pass** in full workspace run at the final P7 gate. |
| `cargo test -p harness-cli --test milestone_m1 --locked` | **6/6 pass**; official gate P1 closure **25/25** pass. |
| `cargo test --workspace --all-targets --locked -- --skip m9_04_release_candidate_has_checksums_and_is_not_published` | **PASS** in final P7 gate. |
| `pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P7 -Json` | **PASS** · 413 files · `sha256:e1da3e99101a7b407dbff67cc0bfe537d90b660de5080a12f9072b3974ffa37b` · format, Clippy, workspace tests, P0–P6 closure (8/25/17/19/21/19/12), P7 discovery (30), required tests (11), docs checks. Digest is the gate-time snapshot before final evidence/handoff text changed; no Rust source changed after the gate. |
| `pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest` | **PASS** after final evidence/handoff edits: 171 Markdown, 15 language pairs, C01–C30, K01–K14, R01–R12, W01–W06, P0–P8 63 steps, P0–P7 53–76 person-days and all 12 negative controls. |

RED/GREEN đã quan sát trong audit này: restore đích rỗng đã tồn tại; status commit
trước journal; GC unlink trước DB delete; write path không chặn tombstone; manifest
metadata không khớp snapshot; mtime thiếu bị hiểu là cũ; forget lặp sửa tombstone;
reason trắng được nhận; confirmation thiếu source kind vẫn xóa; backup ghi vào
directory có sẵn; backup lỗi để DB snapshot dở; GC xóa path là directory; store API
ghi đè tombstone/journal; và pin batch không tồn tại được báo thành công. Các ca đã
được sửa có GREEN. P7 migration fixture từng đỏ vì test setup tạo trùng PK; đó là
lỗi fixture, không production.

## 4. Giới hạn còn lại

- Linux chưa được chạy theo chỉ dẫn.
- P0–P7 historic gate/CI chỉ áp dụng đúng revision ghi trong từng evidence; audit
  này không giả định chúng tự động áp dụng lên source tree mới.
- Benchmarks P7 `retrieval_p95`/`restore_state` vẫn chưa được đo; release matrix
  phải tiếp tục báo `measured: null` cho tới khi có dataset và phép đo thật.
- Extension/process transport không phải OS sandbox; đổi mức `strict` cần ADR và
  backend phù hợp. Token estimates hiện byte-based; coverage/advisory/secret scans
  chưa hoàn tất theo các evidence phase.
- Host loopback trên Windows đã từng từ chối kết nối ngắt quãng. Acceptance fixture
  hiện có readiness và bounded retries; một lần pass không chứng minh host flake
  bị loại khỏi mọi môi trường.

## 5. Delivery

Audit được giao trên nhánh `codex/p0-p7-audit`; Git history của nhánh là nguồn xác
định revision. Chỉ nhánh audit được giao; không stage/commit/push thay đổi G01–G03
từ checkout chính.
