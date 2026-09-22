# CURRENT — bàn giao đang mở

**Cập nhật:** 22/09/2026 · **Assignment:** M4–M12 theo kế hoạch `implementation-next`, tuần tự theo dependency và gate từng checkpoint. **Checkpoint hiện tại:** **M4-01..M4-04 xong, gate M4 đã xanh** (chi tiết §12). Tiền nhiệm: M0/M1/M2 (`4d6393e`/`f1fb002`, `a824b2c`, `6c91a62`), M3 (`03bea9a`, verified_local).

## 1. Assignment hiện tại và ràng buộc mới nhất

- User (22/09/2026): triển khai M4–M12 theo dependencies, gate từng checkpoint, không chuyển tiếp khi prerequisites chưa đạt; cập nhật handoff sau mỗi checkpoint.
- User (22/09/2026, lượt này): "Triển khai M4 trong scope M4-01..M4-04 … Dừng sau M4; không tự chạy milestone tiếp." → **M4 dừng ở đây**; M5 chỉ bắt đầu khi có assignment mới.
- Quyền: local cargo/pwsh, docs SPEC/ADR/evidence/handoff, commit/push cho công việc M (đã cấp trong session). **Không** cấp: đổi User PATH, cài thật (`Install-Ha.ps1`), paid smoke/live provider, publish/release — tới M9 sẽ dừng và xin phép trước các bước đó.
- Giữ `docs/OPERATOR_GUIDE.*` (thay đổi có trước của user, không commit).

## 2. Branch/base/source digest

- Branch `master`; base M4 `03bea9a` (M3). Các commit M4: `1344fcd`, `30ffa25`, `c482c05`, `c470d0c`, `71ca241`, `b712b4c`, `22d0b60`, `4310b2a` (merge audit), các bản sửa CI `4b8a844`/`6159823`/`90171e0`/`8a2d431`, và **`79c5165`** (M4-03.2/.3/.4 + M4-04, lượt này).
- **Source digest của revision đã test:** `sha256:f34047b54d252b72de5e00baeee0536d47bab62c942bb146bc5f0a0d98c97ede` (341 file), lấy từ `GATE_RESULT_JSON` — **hai lần gate khớp nhau**: lần 1 trên working tree (nội dung về sau là `79c5165`, log `target/verification-m4-gate-m4032.log`), lần 2 trên chính `79c5165` (log `target/verification-m4-gate-79c5165.log`). Digest được tính ở cuối lần chạy trên `git ls-files --cached --others --exclude-standard` (trừ evidence + handoff), nên nó mô tả cây nguồn tại thời điểm gate kết thúc.

## 3. Work item

| Item | Trạng thái | Evidence |
|---|---|---|
| M4-01 gate/approvals/receipts | `implemented_unverified` | binding mang session/task/invocation/`call_id`; consume+intent atomic; expiry/revoke/replay; descriptor registry; `call_id` vào intent+receipt; tools schema v2; A03/A04 crash boundaries + replay tool result |
| M4-02 filesystem/git | `implemented_unverified` | `git_log` structured + descriptor read-only + A15 đầy đủ (traversal/absolute/junction/sensitive/stale/**locked**/CRLF-Unicode); locked-file typed error |
| M4-03 process/spool | `implemented_unverified` | permit queue cancellation-aware (A13); **env allowlist + secret JIT (A16)**; **tree cleanup trung thực + grandchild heartbeat (A16)**; **spool quota + preview/tail + `read_process_output` page (A17)** |
| M4-04 E2E/recovery | `implemented_unverified` | **digest-bound check evidence** + **`a08_coding_e2e`** (crate Rust thật, `cargo test` thật fail→pass) + negative control `m4_04_check_evidence_is_digest_bound` |

`milestone_m4` = **14/14**; gate M4 = **passed** (14 required, closure M3/M1/M2/M0).

## 4. File đã đổi (M4-01..M4-03.1)

Sửa: `crates/harness-store-sqlite/src/{models,store}.rs`, `crates/harness-tools/src/{contracts,service,turn_driver,loop_service,lib}.rs`, `crates/harness-types/src/contracts.rs`, `schemas/tool-execution-receipt.v1.schema.json`, fixture literals (`p1_fixture_host`, `phase_p1`, `milestone_m1`, `phase_p5/support`, `phase_p6`, `delegation_cli`, `harness-types/tests/contracts.rs`), `crates/harness-cli/src/interactive/service.rs` (boxed future), `tests/acceptance/milestones.json`.
Thêm: `crates/harness-cli/tests/milestone_m4.rs`, `crates/harness-cli/src/bin/m4_fixture_host.rs` (child host cho A03/A04), `docs/specs/M4.vi.md`, `docs/adr/ADR-N03-EXECUTION-BINDING.{vi,en}.md`, `docs/evidence/M4.vi.md`.
Sửa thêm (M4-01b): `store.rs` (`recovered_tool_results`), `harness-runtime/src/lib.rs` (`RunRequest.recovered_messages`, `prepare_continuation`), `service.rs` (receipt event mang `model_view`), `turn_driver.rs` (`render_tool_output` dùng chung).
Sửa thêm (M4-02.1/.3): `harness-tools/src/{contracts,policy,service,turn_driver,lib}.rs` (`ToolKind::GitLog`, `limit` 1..=100, `git_log_arguments`), `milestone_m4.rs` (+2 test), `phase_p3.rs` (schema count 9→10).
Sửa thêm (M4-02.2): `harness-tools/src/workspace.rs` (lock → `Ok(None)` + fingerprint `unreadable:"locked"`; lỗi đọc khác → `StorageOpenFailed`).
Sửa thêm (M4-03.1): `harness-tools/src/process.rs` (permit queue cancellation-aware, `ProcessResult.queued`), `contracts.rs`, `service.rs`, `turn_driver.rs`, `milestone_m4.rs` (`a13_queued_process_cancel`).

## 5. Contract/ADR đã chốt — không đổi ngầm

- **ADR-N03** (accepted M4): grant bind actor/task/session/invocation/action/workspace/fingerprint/policy+tool revision/expiry; một lần consume chung transaction với intent; `call_id` chỉ là correlation, `InvocationId` là authority; final guard revalidate trước dispatch; host **không** phải sandbox (`filesystem_network_sandbox=false`, `strict_isolation=false`); effect-before-receipt ⇒ `outcome_unknown`, không auto-rerun; spool/quota typed; check evidence phải khớp workspace digest.
- `TOOLS_SCHEMA_VERSION=2` additive (`ensure_column` dùng PRAGMA + ALTER); DB v1 upgrade tại chỗ, host cũ từ chối DB mới hơn.
- `ToolExecutionReceipt.call_id` optional `serde(default)`; schema JSON regenerate, drift test xanh.
- `ToolDescriptor{id,revision,schema_digest,effect_class,capabilities}` + `coding_tool_descriptors()`; `prepare` từ chối tên không được advertise.
- **M4-02:** `git_log` built-in thứ 10; `limit` 1..=100 (mặc định 20) kiểm ở parser; output tab-separated `%H %aI %s`, `--no-color`, không pager.
- **M4-03.1:** permit queue host-wide; cancel khi đang chờ permit → `canceled:true, queued:true, tree_cleanup_confirmed:true` và **không spawn**.
- **M4-03.2:** `CodingToolAction::{RunProcess,RunShell}.env: Vec<EnvBinding>` (`#[serde(default, skip_serializing_if)]` rỗng ⇒ action hash của call cũ không đổi); reference **bắt buộc** `secret://NAME` — model không bao giờ đưa value; `ToolPolicy.granted_secrets` (host config, mặc định rỗng) là hàng rào thứ hai, thiếu → `SecretNotGranted` **trước proposal**; env con = `env_clear()` + `PROCESS_ENVIRONMENT_ALLOWLIST` (hằng số publish cho operator) + value đã resolve; value bị redact **khi ghi** (streaming) nên không vào spool/artifact/preview/model view/intent/error.
- **M4-03.3:** `tree_cleanup_confirmed` chỉ true khi `wait()` của backend reap xong container; thêm spelling `tree_cleanup ∈ {nothing_to_clean, reaped_on_exit, killed_and_reaped}`; reap không xác nhận → `ProcessOutcomeUnknown` ⇒ receipt `outcome_unknown` (không claim cleanup).
- **M4-03.4:** built-in thứ 11 `read_process_output` (read-only; schema count 10→11; `TOOL_CONTRACT_VERSION` giữ 1); artifact capture = `header_line + stdout + stderr` (header versioned `CAPTURE_HEADER_VERSION=1`); quota **mỗi stream** (mặc định 1 MiB), head preview 64 KiB, tail preview 4 KiB; receipt trỏ vào **capture**, không trỏ bản serialize lại; page read kiểm scope project+task (`artifact_is_scoped_to`) → sai scope `ScopeAuthorityDenied`; offset vượt capture = denial `invalid_payload` **trước intent**; spool không ghi được → `ArtifactWriteFailed` ⇒ `outcome_unknown` + không ref giả.
- **M4-04:** `GoalEvidence.workspace_digest` (fingerprint sau execution settled cuối) + `checks: Vec<CheckObservation{command_digest, workspace_digest, exit_code, passed}>`; `EvidenceKind::Check` chỉ satisfied khi có check `passed` ở **đúng digest cuối**; digest thiếu ⇒ không satisfied (fail closed).
- Registry: A08/A11/A16/A17 chuyển `planned` → `implemented`; M4 required_tests 10 → **14**; M4-03/M4-04 `implemented_unverified`.

## 6. Lệnh đã chạy và kết quả

- `cargo test -p harness-cli --test milestone_m4 --locked` → **14/14 pass**.
- `cargo test -p harness-cli --test phase_p3 --locked` → **21/21** (p3_s04 timeout/descendant và p3_s07 capabilities vẫn xanh sau khi spool thay đường đọc output; hai count assertion 10→11).
- `cargo clippy --workspace --all-targets --locked -- -D warnings` + `cargo fmt --all -- --check` → pass (10 lỗi clippy của code mới đã sửa, không nới lint).
- `pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M4` → **passed** (lần đầu xanh): `format`/`clippy`/`build`/`workspace-tests`/`dependency-allowlist (44 edges)`/`unit-tests`/`test-discovery:milestone_m4 (14)`/`required-tests (14)`/`closure-M3 (19)`/`closure-M1 (11)`/`closure-M2 (12)`/`closure-M0 (11)`. Log `target/verification-m4-gate-m4032.log`; lần chạy lại trên revision đã commit ở `target/verification-m4-gate-79c5165.log`.
- Negative control (đã khôi phục, không còn dấu vết): bỏ `env_clear()` → A16 **FAIL** (con nhận 82 biến host); bỏ wrap `JobObject` → A16 **treo** (call không hoàn tất); bỏ quota capture → A17 **FAIL**; `EvidenceKind::Check => checks_passed > 0` → negative control digest **FAIL**.

## 7. Việc còn lại theo thứ tự

0. **Reviewer nghiệm thu M4**: gate đã xanh + evidence §8; implementer không tự đặt `accepted`. Điểm cần soi kỹ: (a) cơ chế reap `wait()` **không được test phân biệt** trên host này (§8.4 evidence) — chỉ kết quả "cả cây chết" được kiểm; (b) page đọc byte thô của section, không parse dòng.
1. **Gate đa nền tảng**: nhánh Unix của A16/A17/A08 mới chỉ được *biên dịch* trên Windows (pattern `cfg!`); CI ubuntu 12/12 xanh cho tới `90171e0`, nhưng các test M4 mới chưa từng chạy trên Linux. Nếu cần `accepted` đa nền tảng: push và xem job ubuntu, hoặc chạy trên host Linux/WSL.
2. **M5** (context/continuity) chỉ khi được giao — prerequisites M4 đã xong.
3. Việc không thuộc M4: StorePort vẫn chưa implement (handoff M3); live/paid smoke; backup/restore/retention (M9).

## 8. Next action chính xác

**Dừng.** Không chạy M5 hay milestone khác cho tới khi user giao. Nếu user yêu cầu tiếp: đọc `docs/implementation-next/M5.vi.md` + `CONTRACTS.vi.md` §7, xác minh gate M4 trên revision hiện tại, rồi làm SPEC M5 trước khi code. Nếu user yêu cầu chứng minh đa nền tảng cho M4: push `79c5165` (nếu chưa) và đọc job ubuntu của `ci.yml`, đối chiếu `a16_process_tree_env`/`a17_output_quota`/`a08_coding_e2e`.

## 9. Blocked on

Không còn blocker cho M4: gate xanh trên host Windows này. Hai điểm cần người quyết: (1) `accepted` đa nền tảng cần chạy Linux (chưa có WSL/Docker trên máy này — CI là đường duy nhất); (2) M9 sẽ cần quyền riêng cho install/PATH/paid smoke/publish — sẽ hỏi khi tới.

## 10. Không lặp lại

- Không commit file của session khác: stage theo path cụ thể; tránh `cargo fmt --all` khi tree còn code chưa commit của họ. `milestone_m4.rs` là file dùng chung — lượt này chỉ sửa phần của mình (thêm `env: Vec::new()` vào hai literal A13 mà session kia vừa viết lại) và giữ nguyên logic A13 của họ.
- Không đổi `TOOLS_SCHEMA_VERSION` thêm lần nữa nếu không có contract change + test compat.
- Không dùng `call_id` làm global identity; không bỏ scope khỏi binding hash.
- Không claim sandbox/strict isolation.
- Không chạy live provider/paid smoke; không cài đặt/đổi PATH user; không publish.
- **Không viết code chỉ tồn tại trên một OS mà không có đường biên dịch nó ở OS kia** (bài học CI 21/09, §11).
- **Không hardcode `tree_cleanup_confirmed = true`** và không ghi artifact ref khi bytes chưa publish: cả hai đã có test âm.
- **Không để secret vào evidence**: giá trị đã resolve chỉ được tồn tại trong env của con; redaction xảy ra lúc ghi spool, không phải lúc render.

## 11. CI GitHub đỏ từ 21/09 — nguyên nhân, bản sửa (session song song, cùng ngày)

`gh run list --workflow ci.yml` cho thấy **mọi** run trên `master` đỏ từ commit `feat(chat): a named
file…` (21/09), cả **12 job** (P0–P7 × ubuntu/windows). Hai nguyên nhân **khác nhau**, mỗi OS thấy
một cái — vì vậy gate local (Windows) xanh mà CI vẫn đỏ:

| OS | Job/bước đỏ | Nguyên nhân | Bản sửa |
|---|---|---|---|
| ubuntu | `clippy` mọi phase, đỏ sau ~1 phút | `crates/harness-cli/tests/milestone_m4.rs`: `millis as f64 / 1000.0` trong helper **`#[cfg(unix)]`** `write_then_sleep` → `clippy::cast_precision_loss` dưới `-D warnings` (đúng lint đã từng bị ở P3, sửa bởi `f0a4ac8`) | helper bị **thay** bằng `write_then_wait(marker, release)` không còn cast; hai nhánh shell dùng `cfg!` để nhánh OS kia **vẫn được biên dịch** khi gate chạy |
| windows | `workspace-tests` → `milestone_m4::a13_queued_process_cancel` | test hẹp thời gian: `sleep(300ms)` sau khi spawn B rồi mới cancel; máy chậm thì cancel tới **trước** durable intent của B → nhánh `ProcessCanceled` "before the durable intent" (`service.rs`) trả `Denied`, không phải `Settled` | A **giữ permit tới khi test thả** (chờ file `a13-release.txt`); test chờ **2 pending intent** rồi mới cancel. Nhánh `Denied` giữ nguyên — nó trung thực cho call chưa có intent |
| ubuntu | `workspace-tests` → `milestone_m4::a15_path_patch_safety` (chỉ lộ ra **sau** khi sửa clippy) | case `C:/Windows/win.ini` bị assert là "phải bị từ chối": trên POSIX đó là đường dẫn **tương đối** hợp lệ (thư mục tên `C:`), nên `prepare` cho qua | danh sách dựng theo nền tảng bằng `cfg!`; `/etc/hosts` giữ case đường dẫn tuyệt đối cho POSIX. **Không** đổi `resolve_relative`: sản phẩm đúng, test mới là chỗ sai nền tảng |
| windows | `P0 (windows-latest)` bị **cancel** (không phải test đỏ) | `timeout-minutes: 10` của job P0: lần đầu gate chạy hết test trên windows cần ~12-13 phút, nên job bị chính timeout của nó cắt ở 10,1 phút | `.github/workflows/ci.yml`: cả sáu phase job lên `timeout-minutes: 30`, kèm số đo từng OS trong comment |

Kiểm chứng (Windows): `milestone_m4` 10/10 (gồm A13/A15); `a13_queued_process_cancel` lặp 5 lần → 5/5;
`fmt`/`clippy` xanh. Push `4b8a844` (run `35682323925`) → ubuntu qua `format`+`clippy`, a13 xanh cả hai
OS; push `6159823` (run `35683233548`) → 11/12 job xanh; push `90171e0`
(run [35684353612](https://github.com/DEVfancybear/harness-agents/actions/runs/35684353612)) → **12/12 job xanh**,
lần đầu kể từ 21/09. Chi tiết + số đo: `docs/evidence/M4.vi.md` §4c.

## 12. Checkpoint M4-03.2/.3/.4 + M4-04 (lượt này) — tóm tắt

- **Commit:** `79c5165` (19 file, +3188/−208) trên `8a2d431`; hai file mới `harness-tools/src/{capture,secrets}.rs`.
- **Gate:** `-Milestone M4` **passed hai lần với cùng digest** `sha256:f34047b5…` (một lần trên working tree, một lần trên chính `79c5165`); `required-tests` 14/14; closure M3/M1/M2/M0; `workspace-tests` xanh cả hai lần.
- **Bốn negative control** đều cho kết quả mong đợi (§6) — chi tiết ở `docs/evidence/M4.vi.md` §8.4.
- **Giới hạn đã ghi:** cơ chế reap `wait()` không được test phân biệt; nhánh Unix chưa chạy; không sandbox; page là byte thô.
- **SPEC:** `docs/specs/M4.vi.md` §12 chốt contract của bốn lát này trước khi code.
