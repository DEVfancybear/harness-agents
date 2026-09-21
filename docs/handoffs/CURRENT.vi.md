# CURRENT — bàn giao đang mở

**Cập nhật:** 22/09/2026 · **Assignment:** M4–M12 theo kế hoạch `implementation-next`, tuần tự theo dependency và gate từng checkpoint. **Checkpoint hiện tại:** M4-01 xong (M4-02 là bước kế tiếp). Tiền nhiệm: M0/M1/M2 (`4d6393e`/`f1fb002`, `a824b2c`, `6c91a62`) và M3 (`03bea9a`, verified_local).

## 1. Assignment hiện tại và ràng buộc mới nhất

- User (22/09/2026): triển khai M4–M12 theo dependencies, gate từng checkpoint, không chuyển tiếp khi prerequisites chưa đạt; cập nhật handoff sau mỗi checkpoint.
- Quyền: local cargo/pwsh, docs SPEC/ADR/evidence/handoff, commit/push cho công việc M (đã cấp trong session). **Không** cấp: đổi User PATH, cài thật (`Install-Ha.ps1`), paid smoke/live provider, publish/release — tới M9 sẽ dừng và xin phép trước các bước đó.
- Giữ `docs/OPERATOR_GUIDE.*` (thay đổi có trước của user, không commit).

## 2. Branch/base/source digest

- Branch `master`; M4-01 trên base `03bea9a` (M3).
- Source digest lần gate checkpoint M4: **chưa có** vì gate dừng ở bước host flake (chưa in `GATE_RESULT_JSON`); digest của revision sẽ được ghi khi gate xanh. Cây nguồn hiện tại = base `03bea9a` + diff M4-01 đã ghi trong evidence §2.

## 3. Work item

| Item | Trạng thái | Evidence |
|---|---|---|
| M4-01 gate/approvals/receipts | `implemented_unverified` | binding mang session/task/invocation/`call_id`; consume+intent atomic; expiry/revoke/replay; descriptor registry; `call_id` vào intent+receipt; tools schema v2; A03/A04 crash boundaries + replay tool result; `milestone_m4` **7/7**. Gate checkpoint **blocked** vì host loopback (evidence §4) |
| M4-02 filesystem/git | planned | P3 coverage giữ; còn `git log`, locked-file typed error, A15 assertions |
| M4-03 process/spool | planned | permit queue (A13), env allowlist (A16), spool/quota (A17) |
| M4-04 E2E/recovery | planned | digest-bound check evidence (A08), A03/A04 child-kill |

## 4. File đã đổi (M4-01)

Sửa: `crates/harness-store-sqlite/src/{models,store}.rs`, `crates/harness-tools/src/{contracts,service,turn_driver,loop_service,lib}.rs`, `crates/harness-types/src/contracts.rs`, `schemas/tool-execution-receipt.v1.schema.json`, fixture literals (`p1_fixture_host`, `phase_p1`, `milestone_m1`, `phase_p5/support`, `phase_p6`, `delegation_cli`, `harness-types/tests/contracts.rs`), `crates/harness-cli/src/interactive/service.rs` (boxed future), `tests/acceptance/milestones.json`.
Thêm: `crates/harness-cli/tests/milestone_m4.rs`, `crates/harness-cli/src/bin/m4_fixture_host.rs` (child host cho A03/A04), `docs/specs/M4.vi.md`, `docs/adr/ADR-N03-EXECUTION-BINDING.{vi,en}.md`, `docs/evidence/M4.vi.md`.
Sửa thêm (M4-01b): `crates/harness-store-sqlite/src/store.rs` (`recovered_tool_results`), `crates/harness-runtime/src/lib.rs` (`RunRequest.recovered_messages`, `prepare_continuation`, `recovered_messages` rebuild từ attempt events + receipt events), `crates/harness-tools/src/service.rs` (receipt event mang `model_view`), `crates/harness-tools/src/turn_driver.rs` (`render_tool_output` dùng chung).

## 5. Contract/ADR đã chốt — không đổi ngầm

- **ADR-N03** (accepted M4): grant bind actor/task/session/invocation/action/workspace/fingerprint/policy+tool revision/expiry; một lần consume chung transaction với intent; `call_id` chỉ là correlation, `InvocationId` là authority; final guard revalidate trước dispatch; host **không** phải sandbox (`filesystem_network_sandbox=false`, `strict_isolation=false`); effect-before-receipt ⇒ `outcome_unknown`, không auto-rerun; spool/quota typed; check evidence phải khớp workspace digest.
- `TOOLS_SCHEMA_VERSION=2` additive (`ensure_column` dùng PRAGMA + ALTER); DB v1 upgrade tại chỗ, host cũ từ chối DB mới hơn.
- `ToolExecutionReceipt.call_id` optional `serde(default)`; schema JSON regenerate, drift test xanh.
- `ToolDescriptor{id,revision,schema_digest,effect_class,capabilities}` + `coding_tool_descriptors()`; `prepare` từ chối tên không được advertise (external tools do host catalogue quyết).
- A14 đã `implemented` trong registry; A11 giữ `planned` tới khi M4 gate đầy đủ (hai nửa M3+M4 đã có test riêng).

## 6. Lệnh đã chạy và kết quả

- `cargo test -p harness-cli --test milestone_m4 --locked` → **7/7 pass** (thêm A03/A04 sau lần gate đầu).
- Regressions (khi host cho phép): `phase_p3` 21, `phase_p6` 15, `phase_p2` 17, `phase_p1` 21, `phase_p7` 15, `milestone_m0` 11, `milestone_m1` 6, `milestone_m3` 19, `harness-types` 20, `harness-tools` 6 — pass.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` + `cargo fmt --all -- --check` → pass.
- `pwsh ... -Milestone M4` → **blocked**: `format/clippy/build/unit-tests/M4 required 5/closure-M3/closure-M1` xanh; `workspace-tests`/`closure-M2` đỏ vì loopback host (i03, a06_*, a07_401, m2_04). Bằng chứng môi trường: baseline `milestone_m2` (không có thay đổi harness M4) cũng fail 6-7/10 trong 3 lần chạy liên tiếp cùng ngày; từng test `--exact` một mình pass. Chi tiết ở `docs/evidence/M4.vi.md` §4.

## 7. Việc còn lại theo thứ tự (M4)

0. **Chạy lại gate checkpoint M4 cho xanh** khi host hết flake loopback (đề xuất reboot máy hoặc chạy trên host khác); đây là điều kiện để chuyển sang M4-02. Nếu vẫn đỏ ở `milestone_m2`/`interactive_launch`, chạy full suite baseline để xác nhận lại nguyên nhân môi trường trước khi nghi code.
1. **M4-02**: `git log` structured; `hash_file` locked → typed (không `WorkspaceEscape`); test A15 gộp (traversal/junction/alias/stale/CRLF/locked).
3. **M4-03**: permit queue cancellation-aware + queued state (A13); env allowlist + secret JIT (A16); tree cleanup trung thực + Windows JobObject grandchild test; output spool + `read_process_output` page + quota/disk-full typed (A17).
4. **M4-04**: `GoalEvidence.workspace_digest` + `EvidenceKind::Check` khớp fingerprint cuối; `a08_coding_e2e` repo tạm + cargo test thật; wrapper A03/A04.
5. Gate M4 đầy đủ + evidence + nghiệm thu; sau đó mới sang M5.

## 8. Next action chính xác

Mở `crates/harness-cli/tests/milestone_m4.rs` và thêm `a03_receipt_before_checkpoint`/`a04_effect_before_receipt` với fixture host bị kill thật (barrier env), rồi chạy `cargo test -p harness-cli --test milestone_m4 --locked` trước khi làm M4-02.

## 9. Blocked on

**Host loopback**: gate checkpoint M4 không xanh được vì môi trường (baseline M2 cũng fail 6-7/10 khi chạy full suite; từng test một mình pass). Cần reboot máy hoặc chạy gate trên host/OS khác. (M9 install/PATH/paid smoke/publish sẽ cần quyền riêng — sẽ hỏi khi tới.)

## 10. Không lặp lại

- Không đổi `TOOLS_SCHEMA_VERSION` thêm lần nữa nếu không có contract change + test compat.
- Không dùng `call_id` làm global identity; không bỏ scope khỏi binding hash.
- Không claim sandbox/strict isolation; không claim M4 accepted khi M4-02..04 còn thiếu.
- Không chạy live provider/paid smoke; không cài đặt/đổi PATH user; không publish.
