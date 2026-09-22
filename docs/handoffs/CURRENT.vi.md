# CURRENT — bàn giao đang mở

**Cập nhật:** 22/09/2026 · **Assignment:** M4–M12 theo kế hoạch `implementation-next`, tuần tự theo dependency và gate từng checkpoint. **Checkpoint hiện tại:** M4-01, M4-02, M4-03.1 xong (M4-03.2/.3/.4 là bước kế tiếp). Tiền nhiệm: M0/M1/M2 (`4d6393e`/`f1fb002`, `a824b2c`, `6c91a62`) và M3 (`03bea9a`, verified_local).

## 1. Assignment hiện tại và ràng buộc mới nhất

- User (22/09/2026): triển khai M4–M12 theo dependencies, gate từng checkpoint, không chuyển tiếp khi prerequisites chưa đạt; cập nhật handoff sau mỗi checkpoint.
- Quyền: local cargo/pwsh, docs SPEC/ADR/evidence/handoff, commit/push cho công việc M (đã cấp trong session). **Không** cấp: đổi User PATH, cài thật (`Install-Ha.ps1`), paid smoke/live provider, publish/release — tới M9 sẽ dừng và xin phép trước các bước đó.
- Giữ `docs/OPERATOR_GUIDE.*` (thay đổi có trước của user, không commit).

## 2. Branch/base/source digest

- Branch `master`; các commit M4: `1344fcd` (M4-01a), `30ffa25` (docs), `c482c05` (M4-01b), M4-02 commit kế tiếp — tất cả trên base `03bea9a` (M3).
- Source digest lần gate checkpoint M4: **chưa có** vì gate dừng ở bước host flake (chưa in `GATE_RESULT_JSON`); digest của revision sẽ được ghi khi gate xanh.

## 3. Work item

| Item | Trạng thái | Evidence |
|---|---|---|
| M4-01 gate/approvals/receipts | `implemented_unverified` | binding mang session/task/invocation/`call_id`; consume+intent atomic; expiry/revoke/replay; descriptor registry; `call_id` vào intent+receipt; tools schema v2; A03/A04 crash boundaries + replay tool result; `milestone_m4` **7/7**. Gate checkpoint **blocked** vì host loopback (evidence §4) |
| M4-02 filesystem/git | `implemented_unverified` | `git_log` structured + descriptor read-only + A15 đầy đủ (traversal/absolute/junction/sensitive/stale/**locked**/CRLF-Unicode); locked-file typed error adopt từ working tree; `milestone_m4` **9/9** |
| M4-03 process/spool | `in_progress` (03.1 xong) | Permit queue host-wide cancellation-aware, `ProcessResult.queued`/`ToolOutput::Process.queued`, A13 xanh (`milestone_m4` **10/10**). Còn: env allowlist/JIT (A16), tree cleanup honest + grandchild, spool/page/quota (A17) |
| M4-04 E2E/recovery | planned | digest-bound check evidence (A08), A03/A04 child-kill |

## 4. File đã đổi (M4-01)

Sửa: `crates/harness-store-sqlite/src/{models,store}.rs`, `crates/harness-tools/src/{contracts,service,turn_driver,loop_service,lib}.rs`, `crates/harness-types/src/contracts.rs`, `schemas/tool-execution-receipt.v1.schema.json`, fixture literals (`p1_fixture_host`, `phase_p1`, `milestone_m1`, `phase_p5/support`, `phase_p6`, `delegation_cli`, `harness-types/tests/contracts.rs`), `crates/harness-cli/src/interactive/service.rs` (boxed future), `tests/acceptance/milestones.json`.
Thêm: `crates/harness-cli/tests/milestone_m4.rs`, `crates/harness-cli/src/bin/m4_fixture_host.rs` (child host cho A03/A04), `docs/specs/M4.vi.md`, `docs/adr/ADR-N03-EXECUTION-BINDING.{vi,en}.md`, `docs/evidence/M4.vi.md`.
Sửa thêm (M4-01b): `crates/harness-store-sqlite/src/store.rs` (`recovered_tool_results`), `crates/harness-runtime/src/lib.rs` (`RunRequest.recovered_messages`, `prepare_continuation`, `recovered_messages` rebuild từ attempt events + receipt events), `crates/harness-tools/src/service.rs` (receipt event mang `model_view`), `crates/harness-tools/src/turn_driver.rs` (`render_tool_output` dùng chung).
Sửa thêm (M4-02.1/.3): `crates/harness-tools/src/{contracts,policy,service,turn_driver,lib}.rs` (`ToolKind::GitLog`, schema `limit` 1..=100, dispatch `git_log_arguments`, đọc-only), `crates/harness-cli/tests/milestone_m4.rs` (+2 test), `crates/harness-cli/tests/phase_p3.rs` (schema count 9→10).
Sửa thêm (M4-02.2): `crates/harness-tools/src/workspace.rs` (adopt: lock → `Ok(None)` + fingerprint `unreadable:"locked"`; lỗi đọc khác → `StorageOpenFailed`; giữ permission target khi patch trên Unix), `milestone_m4.rs` thêm case locked vào A15.
Sửa thêm (M4-03.1): `crates/harness-tools/src/process.rs` (permit queue cancellation-aware, `ProcessResult.queued`), `contracts.rs` (`ToolOutput::Process.queued`), `service.rs` (`process_output` truyền queued), `turn_driver.rs` (render in queued), `milestone_m4.rs` (`a13_queued_process_cancel`).

**Push:** `1344fcd` (M4-01a), `30ffa25` (docs M4-01), `c482c05` (M4-01b), `c470d0c` (M4-02.1/.3), `71ca241` (M4-02.2). M4-03.1 commit kế tiếp.

## 5. Contract/ADR đã chốt — không đổi ngầm

- **ADR-N03** (accepted M4): grant bind actor/task/session/invocation/action/workspace/fingerprint/policy+tool revision/expiry; một lần consume chung transaction với intent; `call_id` chỉ là correlation, `InvocationId` là authority; final guard revalidate trước dispatch; host **không** phải sandbox (`filesystem_network_sandbox=false`, `strict_isolation=false`); effect-before-receipt ⇒ `outcome_unknown`, không auto-rerun; spool/quota typed; check evidence phải khớp workspace digest.
- `TOOLS_SCHEMA_VERSION=2` additive (`ensure_column` dùng PRAGMA + ALTER); DB v1 upgrade tại chỗ, host cũ từ chối DB mới hơn.
- `ToolExecutionReceipt.call_id` optional `serde(default)`; schema JSON regenerate, drift test xanh.
- `ToolDescriptor{id,revision,schema_digest,effect_class,capabilities}` + `coding_tool_descriptors()`; `prepare` từ chối tên không được advertise (external tools do host catalogue quyết).
- **M4-02:** `git_log` là built-in thứ 10 (schema count 9→10, `TOOL_CONTRACT_VERSION` giữ 1 vì digest từng tool mới là thứ versioned); `limit` bound 1..=100 (mặc định 20) kiểm ở parser trước khi có proposal; output tab-separated `%H %aI %s`, `--no-color`, không pager; path scope qua resolver như `git_diff`.
- **M4-03.1:** permit queue host-wide (`tokio::sync::Mutex` + fast path `try_lock`, queue path `select!` với `CancellationToken`); cancel khi đang chờ permit → `ProcessResult{canceled:true, queued:true, tree_cleanup_confirmed:true}` và **không spawn**; `ToolOutput::Process.queued` là field additive (không có JSON schema chính thức cho ToolOutput).
- A14 đã `implemented` trong registry; A11 giữ `planned` tới khi M4 gate đầy đủ (hai nửa M3+M4 đã có test riêng).

## 6. Lệnh đã chạy và kết quả

- `cargo test -p harness-cli --test milestone_m4 --locked` → **10/10 pass** (thêm `a13_queued_process_cancel`).
- Index-check M4-02.2 (stash toàn bộ phần working tree còn lại, chỉ giữ `workspace.rs` trong index): `harness-tools` 6/6, `milestone_m4` 9/9, `phase_p3` 21/21, `phase_p1` 21/21 — pass, chứng minh phần adopt tự đứng vững.
- Regressions (khi host cho phép): `phase_p3` 21, `phase_p6` 15, `phase_p2` 17, `phase_p1` 21, `phase_p7` 15, `milestone_m0` 11, `milestone_m1` 6, `milestone_m3` 19, `harness-types` 20, `harness-tools` 6 — pass.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` + `cargo fmt --all -- --check` → pass.
- `pwsh ... -Milestone M4` → **blocked**: `format/clippy/build/unit-tests/M4 required 5/closure-M3/closure-M1` xanh; `workspace-tests`/`closure-M2` đỏ vì loopback host (i03, a06_*, a07_401, m2_04). Bằng chứng môi trường: baseline `milestone_m2` (không có thay đổi harness M4) cũng fail 6-7/10 trong 3 lần chạy liên tiếp cùng ngày; từng test `--exact` một mình pass. Chi tiết ở `docs/evidence/M4.vi.md` §4. (Lần gate tới sẽ chạy M4 required **9 test**.)

## 7. Việc còn lại theo thứ tự (M4)

0. **Chạy lại gate checkpoint M4 cho xanh** khi host hết flake loopback (đề xuất reboot máy hoặc chạy trên host khác); đây là điều kiện để chốt M4-01/M4-02. Nếu vẫn đỏ ở `milestone_m2`/`interactive_launch`, chạy full suite baseline để xác nhận lại nguyên nhân môi trường trước khi nghi code.
1. **M4-03 còn lại**: env allowlist + secret JIT (A16) và tree cleanup trung thực; output spool + `read_process_output` page + quota/disk-full typed (A17). `process.rs` giờ là phần đã commit của ta (M4-03.1), tiếp tục sửa bình thường.
2. **M4-04**: `GoalEvidence.workspace_digest` + `EvidenceKind::Check` khớp fingerprint cuối; `a08_coding_e2e` repo tạm + cargo test thật; wrapper A03/A04.
3. Gate M4 đầy đủ + evidence + nghiệm thu; sau đó mới sang M5.

## 8. Next action chính xác

M4-03.2: đọc `crates/harness-tools/src/process.rs` (chỗ `CommandWrap::with_new`) và thêm env allowlist: `env_clear()` + allowlist tối thiểu (PATH/SystemRoot/TEMP trên Windows; PATH/HOME/TMPDIR trên Unix) + grant secret tường minh; viết `a16_process_tree_env` (sentinel secret trong env cha không xuất hiện trong output con; grandchild heartbeat dừng sau timeout/cancel), chạy `cargo test -p harness-cli --test milestone_m4 --locked`.

## 9. Blocked on

**Hai việc song song:** (1) host loopback — gate checkpoint M4 chưa xanh được vì môi trường (baseline M2 cũng fail khi chạy full suite; từng test một mình pass); cần reboot/host khác. (2) working tree còn thay đổi chưa commit của session khác ở `session`, `interactive`, `extensions`, `orchestrator`, `maintenance`, `store/maintenance.rs` — không thuộc M4, giữ nguyên. (M9 install/PATH/paid smoke/publish sẽ cần quyền riêng — sẽ hỏi khi tới.)

## 10. Không lặp lại

- Không commit file của session khác: stage theo path cụ thể hoặc `git add -p`; tránh `cargo fmt --all` khi tree còn code chưa commit của họ. Hai file đã adopt có chủ đích và index-check: `workspace.rs` (M4-02.2) và `process.rs` (M4-03.1).
- Không đổi `TOOLS_SCHEMA_VERSION` thêm lần nữa nếu không có contract change + test compat.
- Không dùng `call_id` làm global identity; không bỏ scope khỏi binding hash.
- Không claim sandbox/strict isolation; không claim M4 accepted khi M4-02..04 còn thiếu.
- Không chạy live provider/paid smoke; không cài đặt/đổi PATH user; không publish.
