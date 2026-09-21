# Audit bảo mật & hiệu năng — 22/09/2026

**Phạm vi:** toàn bộ source Rust trong root workspace (12 crate) và các cam kết trong
docs kiến trúc/spec/ADR/evidence. **Branch:** `audit/security-perf-fixes`
(worktree `../harness-agents-audit`). **Cách làm:** đọc code và tự xác nhận từng
finding trước khi sửa; không sửa theo suy đoán.

> Phần lớn fix của lượt audit này đã nằm trong `master` qua các commit M4 của
> session song song (`1344fcd`, `9dcfaf1`). Branch này bổ sung phần còn thiếu:
> `harness-providers`, index/streaming của memory, `ContentHash::from_reader`,
> compaction CAS, và regression test cho known-defect.

## 1. Bảo mật (đã fix)

| # | Vấn đề | File | Fix |
|---|---|---|---|
| S1 | **Path traversal (critical)** khi `verify_backup`/`restore_backup`: manifest không tin cậy chứa `..\..\x` hoặc đường dẫn tuyệt đối cho `database_file`/artifact → đọc/ghi ngoài thư mục backup | `harness-maintenance/src/contracts.rs` | `validate_relative_path` từ chối absolute/`..`/prefix cho mọi tên trong manifest |
| S2 | **GC xoá artifact vừa được pin**: danh sách candidate là snapshot, check pin cũ, `remove_artifact_record` chỉ kiểm tra receipts | `harness-store-sqlite/src/store/maintenance.rs`, `harness-maintenance/src/retention.rs` | `collect_artifact`: kiểm tra pin+receipt+scope và unlink trong **cùng một transaction**; `remove_artifact_record` thêm check pin/scope |
| S3 | **MCP server kế thừa toàn bộ env** (provider API key, token) | `harness-extensions/src/mcp.rs` | `env_clear()` + `ENVIRONMENT_ALLOWLIST` (giống plugin transport) |
| S4 | **Đọc dòng plugin không giới hạn** (`BufReader::split` giữ nguyên dòng tới khi có `\n`) → OOM | `harness-extensions/src/transport.rs` | `CappedLines`: cap 1 MiB (stdout) / 64 KiB (stderr), bytes vượt cap bị drain |
| S5 | **Provider SSE**: nhánh index-only/id-only tạo identity không bị đếm (`max_tool_calls` bypass) → OOM | `harness-providers/src/lib.rs` | `note_identity` đếm mọi nhánh |
| S6 | **Provider SSE**: `max_arguments_bytes` chỉ check từng fragment, không check tổng cộng dồn; output decoded không có cap | `harness-providers/src/lib.rs` | `arguments_bytes` theo call + `max_output_bytes` cho text/arguments |
| S7 | **Provider redirect**: 307/308 gửi lại **toàn bộ body** (hội thoại, ảnh base64, tool schema) sang host trong `Location` | `harness-providers/src/lib.rs` | `redirect(Policy::none())` |
| S8 | **Provider endpoint http từ xa**: gửi API key cleartext | `harness-providers/src/lib.rs` | `validate_endpoint`: https bắt buộc, http chỉ cho loopback (fixture/proxy cục bộ) |
| S9 | **Lỗi provider kèm URL** (có thể chứa token trong query/userinfo) vào message được persist/render | `harness-providers/src/{lib,streaming}.rs` | `error.without_url()` trước khi format |
| S10 | **`/key` bypass**: dòng bắt đầu bằng space không được nhận là command → API key bị gửi thẳng cho provider và lưu vào history | `harness-cli/src/interactive/controller.rs` | `text.trim_start().starts_with('/')`; `forget_submission(line)` dùng nguyên văn buffer |
| S11 | **Attachment symlink**: `notes.txt -> ~/.ssh/id_rsa` vượt deny-list (check theo tên, `fs::read` theo link) | `harness-cli/src/interactive/attachments.rs` | canonicalize trước, check deny-list trên đường dẫn thật, đọc từ path đã resolve |
| S12 | **Attachment size**: file khai báo ảnh được đọc hết trước khi check 8 MiB | `harness-cli/src/interactive/attachments.rs` | check `metadata.len()` trước khi `read` |
| S13 | **Deny-list thiếu**: `.git-credentials`, `.netrc`, `.npmrc`, mọi `id_*`, `.p12/.pfx/...` | `harness-cli/src/interactive/attachments.rs` | mở rộng `looks_like_credentials` |
| S14 | **Paste PNG ghi qua symlink** (path content-addressed, đoán được) | `harness-cli/src/interactive/attachments.rs` | `OpenOptions::create_new(true)` |
| S15 | **Credential staging**: file cũ (quyền rộng hơn/symlink) bị tái sử dụng khi ghi key | `harness-cli/src/interactive/credentials.rs` | remove link cũ + `create_new` + `mode(0600)` |
| S16 | **Terminal escape injection**: text từ model/file/tool ghi thẳng ra terminal (OSC 52, đổi title, di chuyển cursor) | `harness-cli/src/interactive/app.rs` | `terminal_safe`: bỏ control chars, giữ `\n`/`\t`, áp ở ranh giới ghi |
| S17 | **Workspace fingerprint gặp file bị khoá** (store nằm trong workspace qua `HA_HOME`): mọi turn fail và báo sai `workspace_escape` | `harness-tools/src/workspace.rs` | lock violation (Windows 32/33) → entry `content_hash: null, unreadable: "locked"`; lỗi đọc khác → `storage_open_failed` có tên file |
| S18 | **Patch làm mất quyền file** trên Unix (rename thay bằng file temp 0644) | `harness-tools/src/workspace.rs` | copy permission của target sang file temp trước rename |
| S19 | **Process đã cancel trước spawn vẫn chạy** (cancel khi đang chờ permit) | `harness-tools/src/process.rs` | check cancellation sau khi lấy permit, trả kết quả canceled không spawn |
| S20 | **Orchestrator scope bypass**: rename chỉ kiểm tra đích (`old -> new`), `git add --all` sau khi check snapshot (TOCTOU), lock git riêng cho integrator, generation fence không kiểm tra | `harness-orchestrator/src/{workspace,integration,coordinator}.rs` | parse cả 2 phía rename; stage rồi validate `diff --cached --name-status`; chia sẻ `git_lock`; từ chối worker khác generation |
| S21 | **Scheduler double charge**: mỗi dispatch trừ 2 request; reserved key rò khi charge fail | `harness-orchestrator/src/scheduler.rs` | charge 1 lần trước spawn; xoá reserved khi charge fail |
| S22 | **Runtime reservation leak**: cancel/memory-recheck trước dispatch bỏ quên reservation đã charge | `harness-runtime/src/lib.rs` | `ledger.release(...)` trước khi break |

## 2. Hiệu năng (đã fix)

- `session_summary`: 1 row theo PK thay vì `list_sessions()` (GROUP BY toàn bộ) rồi tìm — `harness-store-sqlite/src/store.rs`.
- `fold_event` O(n²) → O(n) bằng `FoldSeen` sets — `harness-session/src/lib.rs`.
- Memory: `sort_by_cached_key` (không normalize lại mỗi lần so sánh) — `harness-memory/src/retrieval.rs`; thêm index `memory_bindings(principal_id)` và `memory_versions(normalized_content)` — `harness-store-sqlite/src/store/memory.rs`.
- `ContentHash::from_reader` + orchestrator fingerprint stream từng file (không nạp cả file vào RAM; file không đọc được là lỗi typed thay vì coi như rỗng) — `harness-types/src/hash.rs`, `harness-orchestrator/src/workspace.rs`.
- Provider `drain_frames`: single-pass, bỏ `Vec::drain` per-frame (O(n²) memmove) — `harness-providers/src/lib.rs`.

## 3. Đúng đắn (đã fix)

- **Compaction CAS luôn pass**: `expected_last` được đọc *sau* khi build packet nên event commit xen giữa bị bỏ qua im lặng — `harness-runtime/src/lib.rs` (dùng `recovered.replayed_through_sequence`).
- **Known defect D1** (`known_defects.rs`) chuyển từ test ignored thành regression test thật, pass trên máy này.

## 4. Verify

- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`: **pass**.
- `cargo test --workspace --all-targets --locked --no-fail-fast` (RUST_TEST_THREADS=4):
  pass toàn bộ, **trừ 2 test `milestone_m2`** thuộc flake loopback có sẵn của host
  (`error sending request` / `error decoding response body`). Đã chứng minh flake
  bằng cách chạy cùng test ở HEAD **không có fix của audit**: cũng fail 2/3 lần với
  cùng chữ ký.
- Gate `Verify-Milestone.ps1 -Milestone M4`: format/clippy/build **pass**;
  `workspace-tests` đỏ do chính flake trên (host flake đã được ghi trong SPEC M4 §11).
- `known_defects` (test mới) và `phase_p2` (17 test, gồm đường compaction): pass.

## 5. Chưa fix (có lý do, ghi lại để không mất)

- `tree_cleanup_confirmed` hard-code + Windows grandchild: thuộc M4-03 (permit queue + JobObject test) theo SPEC, không sửa lẻ.
- Headless cần approval cho mọi tool call: quyết định contract (HA_LAUNCH), cần user chốt.
- N+1 đọc memory (`confirm_version`, merge/derived sources), snapshot định kỳ cho journal dài, cap ảnh/text ở tầng runtime cho host khác CLI: cần thiết kế/limit policy riêng.
- Provider `timeout` tổng 120 s là chủ ý (docs M2), giữ nguyên.
- Zeroize secret trong RAM: cần thêm dependency (allowlist gate), để riêng.
