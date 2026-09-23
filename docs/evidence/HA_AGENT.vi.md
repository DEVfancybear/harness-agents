# Evidence — HA_AGENT CP-A archive and CP-B / Bằng chứng — hồ sơ CP-A và CP-B HA_AGENT

**Current status / Trạng thái hiện tại:** `implemented_unverified` at CP-B. G04–G06 source and tests are implemented, but the last two Verify-HaLaunch runs failed one loopback test each in `acceptance-launch`. The retry cap is exhausted; no G07+ work started. / `implemented_unverified` tại CP-B. Source và test G04–G06 đã triển khai, nhưng hai lượt Verify-HaLaunch cuối đều lỗi một test loopback trong `acceptance-launch`. Đã hết retry budget; chưa làm G07 trở đi.

SPEC: [HA_AGENT.vi.md](../specs/HA_AGENT.vi.md) · Plan: [HA_AGENT_PLAN.vi.md](../HA_AGENT_PLAN.vi.md) · Assignment handoff: [HA_AGENT.vi.md](../handoffs/HA_AGENT.vi.md)

## Current checkpoint CP-B / Checkpoint hiện tại CP-B

### Source snapshot / Snapshot source

| Field / Trường | Value / Giá trị |
|---|---|
| Branch and HEAD / Branch và HEAD | `master`, `c1b4b6730f57c541f090516f9100d5cfcf9811bd` |
| Worktree / Worktree | Dirty; **48 changed paths** before the final evidence/handoff edits and **50 paths** after them, including CP-B implementation, SPEC/TUI updates, generated schemas, and related shared-worktree changes. / Bẩn; **48 path thay đổi** trước khi cập nhật evidence/handoff cuối và **50 path** sau cập nhật; gồm implementation CP-B, SPEC/TUI, schema sinh và thay đổi workspace dùng chung. |
| Source digest / Digest source | `sha256:7b59aa4dfedbcc26d92dac100b22ac046020d17517001e0aedb58497ef817d00`; **420 files**. Hash is SHA-256 over each sorted repo-relative path encoded UTF-8 + NUL, file bytes, then NUL, for tracked and non-ignored untracked files; excludes only this evidence and docs/handoffs/HA_AGENT.vi.md. / **420 file**; SHA-256 trên path repo-relative đã sắp xếp (UTF-8 + NUL), bytes file rồi NUL; gồm file tracked và untracked không ignore; chỉ loại evidence này và docs/handoffs/HA_AGENT.vi.md. |
| `Cargo.lock` SHA-256 | `193d2574da4a994777bf8961780e264a123c1397d04d7e66f7bf49d33d919876` |
| OS / Hệ điều hành | Windows 11 Pro, `10.0.26200.0`, x64 |
| Toolchain | `rustc 1.97.1 (8bab26f4f 2026-07-14)` with `RUSTUP_HOME=C:\Users\duong\.rustup-ha-agent-2026-09-23`; commands used `--locked`. / Chọn toolchain qua Rustup home riêng; lệnh Cargo dùng `--locked`. |
| Commit/push / Ghi commit/push | None in this CP-B continuation. / Lượt CP-B này không commit/push. |

G05 is present in the single existing `ToolPolicy`: modes, ordered deny/allow/mode decisions, confirmed persistent rule save, `/permissions`, `/mode`, audit transcript entries, and fail-closed headless defaults all have tests. This continuation did not find a missing G05 feature. / G05 hiện diện trong `ToolPolicy` duy nhất: mode, thứ tự deny/allow/mode, lưu rule dài hạn sau xác nhận, `/permissions`, `/mode`, audit transcript và headless fail-closed đều có test. Lượt này không phát hiện thiếu tính năng G05.

### CP-B command ledger / Nhật ký lệnh CP-B

| Command / Lệnh | Result / Kết quả |
|---|---|
| `cargo fmt --all` and `cargo fmt --all -- --check` | Pass / Đạt |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Pass, independently and in Verify-HaLaunch / Đạt riêng lẻ và trong Verify-HaLaunch |
| `cargo test -p harness-cli --bin ha --locked` | Attempt 1 **297 passed / 1 failed / 1 ignored** (`completion_service_resume_flow` loopback child timed out); attempt 2 **298 passed / 0 failed / 1 ignored**. / Lượt 1 **297 đạt / 1 lỗi / 1 ignored** (loopback child `completion_service_resume_flow` timeout); lượt 2 **298/0/1 ignored**. |
| Updated H03/H05/T06 unit tests | **1/1 each**; running input queues and dispatches after terminal, stale `y` never answers an expired gate, and panel distinguishes turn grant from persistent-rule confirmation. / Mỗi test **1/1**; queue input, `y` cũ không trả lời gate hết hạn, panel phân biệt grant lượt với xác nhận rule dài hạn. |
| G05 targeted selectors | CLI **9/9**; `harness-tools` **5/5**. / CLI **9/9**; `harness-tools` **5/5**. |
| `milestone_m4`, `phase_p3`, `interactive_session` (serial) | **23/23**, **21/21**, **14/14**. / **23/23**, **21/21**, **14/14**. |
| `interactive_terminal` normal cargo test | **0 passed, 0 failed, 18 ignored** because the cases require a real console. / **0 đạt, 0 lỗi, 18 ignored** vì cần console thật. |
| Real-console PTY selectors through `scripts/Invoke-HaPtyAcceptance.ps1` | `h05` attempt 1 failed on an incomplete loopback fixture read; attempt 2 **1/1** after fixture now reads Content-Length. `i05`, `i06`, `i07a`, `i07b`: **1/1 each**. Transcript files are under `target/pty-acceptance-cpb/`. / `h05` lượt 1 lỗi fixture đọc thiếu; lượt 2 **1/1** sau khi fixture đọc đủ Content-Length. `i05`, `i06`, `i07a`, `i07b`: mỗi case **1/1**. Transcript nằm trong `target/pty-acceptance-cpb/`. |
| Schema generator / Sinh schema | `cargo run -p harness-types --bin generate_schemas --locked`; `p0_f03` **1/1** and P0 **8/8** after generation. It rewrote `error-report.v1.schema.json` and `tool-execution-receipt.v1.schema.json`; no JSON was edited manually. / Generator đã tạo lại hai file trên; `p0_f03` **1/1**, P0 **8/8**; không sửa JSON bằng tay. |

Verify-HaLaunch attempt ledger: attempt 1 failed `regression-phase_p0` because generated receipt schema was stale while `acceptance-launch` was **19/19**. Attempt 2 failed `acceptance-launch` **18/19**, selector `i03_a_named_file_reaches_the_model_inside_the_message`; the other steps passed, including P0–P7. Attempt 3 failed `acceptance-launch` **18/19**, selector `i13_resume_continues_the_task_with_recovered_context_and_no_rerun`; the other steps passed, including P0–P7. The final report therefore has `failures: ["acceptance-launch"]`; retry budget is exhausted and neither test was edited or rerun separately. / Nhật ký Verify-HaLaunch: lượt 1 lỗi `regression-phase_p0` vì schema receipt chưa sinh lại, trong khi `acceptance-launch` **19/19**. Lượt 2 lỗi `acceptance-launch` **18/19**, selector `i03_a_named_file_reaches_the_model_inside_the_message`; các bước khác kể cả P0–P7 đều đạt. Lượt 3 lỗi `acceptance-launch` **18/19**, selector `i13_resume_continues_the_task_with_recovered_context_and_no_rerun`; các bước còn lại kể cả P0–P7 đều đạt. Vì vậy report cuối có `failures: ["acceptance-launch"]`; đã hết retry budget, không sửa hoặc chạy riêng lại hai test.

**Acceptance / Nghiệm thu:** CP-B remains `implemented_unverified`. No live/paid API, Linux verification, user installation/PATH mutation, manual database migration, or user/shared-project `config.local.toml` write was performed; G05 persistence tests wrote only inside disposable temp roots. G07+ was not started. / CP-B vẫn `implemented_unverified`. Không gọi API live/trả phí, kiểm chứng Linux, cài/PATH user, migration DB thủ công hoặc ghi `config.local.toml` của project user/workspace dùng chung; test G05 chỉ ghi trong thư mục tạm có thể xóa. Chưa bắt đầu G07 trở đi.

## Historical CP-A evidence / Evidence CP-A lưu trữ

The following sections preserve CP-A measurements as historical evidence; they do not describe the current CP-B state. / Các phần tiếp theo lưu số đo CP-A trong lịch sử, không mô tả trạng thái CP-B hiện tại.

### 1. Tested source / Source đã kiểm tra

| Field / Trường | Value / Giá trị |
|---|---|
| Assignment baseline / Revision gốc | `37e355dc2cc22a381c7e86ae3231b14c0c354be0` |
| HA_AGENT implementation commit / Commit triển khai HA_AGENT | `0b6d19da17a3387ddbe46daca91ab5f81e66f9d3` (`master`; pushed to `origin/master`) |
| Working-tree source digest / Digest source working tree | `sha256:b9fc76caf88fff2f8eba2a229f7082d3ff10c91104f894e46952b52a95b3a137` |
| Files hashed / Số file | **419**; includes the bilingual SPEC and generated config schema; excludes only this evidence and the assignment handoff |
| `Cargo.lock` SHA-256 | `9a778b3745b5ae8e1c954ad1128abb12c9807e1148b7237653a9c1be61a825a6` |
| OS / architecture | Windows `10.0.26200.0`, x64 |
| Toolchain | `rustc 1.97.1 (8bab26f4f 2026-07-14)`; commands used `--locked` |
| Clippy/toolchain | `cargo clippy` passed under `RUSTUP_HOME=C:\Users\duong\.rustup-ha-agent-2026-09-23`, Rust `1.97.1`. The default Rustup home was inconsistent: it reported Clippy installed while the driver was absent; a repair attempt met a locked `rustc_driver` DLL used by Rust Analyzer and left the default pinned toolchain without `rustc.exe`/`cargo.exe`. No repository files were changed by this toolchain repair. |

HEAD moved from `37f4035` to `5498f74` while the gates were running because another M7–M12 audit commit landed in the shared workspace. That commit was preserved. The reviewed HA_AGENT implementation was committed as `0b6d19d` (40 files) and pushed to `origin/master`; this evidence/handoff update is a separate documentation follow-up. / HEAD đổi từ `37f4035` sang `5498f74` trong lúc chạy gate do commit audit M7–M12 khác được ghi vào workspace chung. Commit đó được giữ nguyên. Implementation HA_AGENT đã review được commit thành `0b6d19d` (40 file) và push lên `origin/master`; evidence/handoff này được cập nhật riêng sau đó.

### 2. Commands and measured results / Lệnh và kết quả đo được

| Command / Lệnh | Result / Kết quả |
|---|---|
| `cargo fmt --all -- --check` | Pass / Đạt |
| `cargo test -p harness-cli --bin ha --locked` | **278 passed, 0 failed, 1 ignored** (manual clipboard test). Latest exact run passed. / Lần chạy exact cuối đạt **278/0/1 ignored** (clipboard cần thao tác tay). |
| `cargo test -p harness-cli --bin ha --locked g01_` | **5/5** |
| `cargo test -p harness-cli --bin ha --locked g02_` | **5/5** |
| `cargo test -p harness-cli --bin ha --locked g03_` | **3/3** (CLI-side model, cost, and thinking tests) |
| `cargo test -p harness-store-sqlite --locked g03_model_switch_applies_next_turn_only` | **1/1**; persisted setting applies from the next turn and survives reopen. / Cài đặt model áp dụng từ lượt kế tiếp và còn sau reopen. |
| `cargo test -p harness-cli --test milestone_m2 --locked -- --test-threads=1` | **10/10** |
| `cargo test -p harness-cli --test interactive_session --locked -- --test-threads=1` | **14/14** |
| `cargo test -p harness-cli --test interactive_launch --locked -- --test-threads=1` | **19/19** |
| `cargo test -p harness-cli --test phase_p0` through `phase_p7` (Verify-HaLaunch attempt 3, serial) | **8, 21, 17, 21, 26, 27, 15, 15** passed respectively. / Lần lượt đạt **8, 21, 17, 21, 26, 27, 15, 15**. |
| `cargo test -p harness-cli --test phase_p0 --locked p0_f03_contract_schemas_are_generated_from_real_types` | **1/1**; schema was generated with `cargo run -p harness-types --bin generate_schemas --locked`, not edited by hand. / Schema được sinh bằng generator, không sửa tay. |
| `cargo test -p harness-cli --test milestone_m6 --locked -- --test-threads=1` | **13/13** (ConfigExplain and adjacent M6 checks). / **13/13**. |
| `pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1 -TimeoutSeconds 900` | Final run on the current tree: **17/17**. Transcript: `target/pty-acceptance/pty-all.txt` (ignored build output). / Lượt cuối trên cây hiện tại đạt **17/17**. |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | **Pass** with the isolated Rust 1.97.1 toolchain at `C:\Users\duong\.rustup-ha-agent-2026-09-23`. / **Đạt** với toolchain Rust 1.97.1 cô lập tại đường dẫn trên. |
| `pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json` | Attempt 4: **failed only at `providers-streaming`**. Format, Clippy, discovery, `ha` **278 passed / 0 failed / 1 ignored**, `interactive_launch` **19/19**, `interactive_session` **14/14**, P0–P7 **8/21/17/21/26/27/15/15**, installer/release self-tests, and docs self-test passed. Provider suite was **31 passed / 1 failed**: `anthropic::tests::g03_anthropic_429_http_date_is_bounded`. The verifier retains only the last three output lines, so the assertion detail was not captured. / Lượt 4 chỉ thất bại ở `providers-streaming`; các gate còn lại đạt như số liệu bên trái. Provider suite **31 đạt / 1 lỗi** tại test HTTP-date nêu trên; verifier chỉ giữ ba dòng cuối nên không lưu chi tiết assertion. |

### 3. Flake and regression log / Nhật ký flake và regression

- **I08 exit-code regression:** the PTY test first observed exit 4 because terminal I/O used `StorageWriteFailed`; terminal failures are specified to keep generic exit 1. The terminal paths now use the existing exit-1 class while real storage write errors remain exit 4. The I08 selector and final full PTY suite passed. The test was not changed. / **Regression I08:** lần đầu test PTY thấy exit 4 do lỗi terminal bị phân loại thành `StorageWriteFailed`; đặc tả yêu cầu exit 1. Đường lỗi terminal nay dùng nhóm exit 1 hiện có, lỗi ghi storage thật vẫn exit 4. Selector I08 và PTY suite cuối đều đạt; không sửa test.
- **Unit tests / Windows ACL:** one parallel `ha` run had three credential-file tests fail with `Access is denied` while writing separate temp directories. The serial full unit run and two later exact default runs passed (**278/0/1 ignored**). This transient was recorded; no assertion was removed or weakened. / Một lượt unit chạy song song có ba test credential tạm lỗi ACL; cả lượt serial và hai lượt exact sau đều đạt.
- **G03 Anthropic loopback:** one filtered provider run was 3/4; the truncated-stream assertion received `service_unavailable: provider request failed: error sending request` before reaching the decoder contract. Two isolated diagnostic runs reproduced it. A complete provider suite in Verify-HaLaunch attempt 3 passed on the unchanged fixture. A temporary fixture transport edit was tried and two 4/4 runs passed, but it was reverted; those passes are excluded. The final Verify-HaLaunch attempt 4 then had 31/32 provider tests pass and failed `g03_anthropic_429_http_date_is_bounded`; the verifier did not preserve the assertion detail. No test was edited and no further loopback rerun was made after the retry limit. / Một lượt lọc provider đạt 3/4; test stream bị cắt nhận `service_unavailable: provider request failed: error sending request` trước khi tới decoder. Hai lượt cô lập cũng tái hiện lỗi. Provider suite đầy đủ ở Verify-HaLaunch lượt 3 đạt với fixture nguyên trạng. Từng thử chỉnh transport fixture tạm thời và có hai lượt 4/4 đạt, nhưng đã hoàn tác; không tính hai lượt đó. Verify-HaLaunch cuối lượt 4 có 31/32 provider test đạt, thất bại tại `g03_anthropic_429_http_date_is_bounded`; verifier không giữ assertion detail. Không sửa test và không chạy thêm loopback sau khi hết giới hạn retry.
- **PTY I05 loopback:** one full PTY run was 16/17 because the follow-up local provider request returned `error sending request`; the I05 selector then passed 1/1, and the next full PTY run passed 17/17. The test stayed unchanged. / Một lượt PTY đạt 16/17 vì request provider loopback ở bước follow-up lỗi; selector I05 đạt 1/1 và full PTY kế tiếp đạt 17/17. Không sửa test.
- Verify-HaLaunch attempts 1–2 exposed stale assertions after config/store contract changes plus a transient phase-P2 failure; those assertions were aligned to the implemented contracts without weakening them, and P2 passed in isolation and later gates. Attempt 3 failed only at Clippy startup; attempt 4 passed Clippy and every step except `providers-streaming`. / Hai lượt Verify đầu phát hiện assertion cũ sau thay đổi contract config/store và một lỗi P2 thoáng qua; assertion được sửa theo contract, không nới, P2 đạt ở lượt cô lập và các gate sau. Lượt 3 chỉ lỗi khởi chạy Clippy; lượt 4 đạt Clippy và mọi bước trừ `providers-streaming`.

### 4. Scope, dependencies, and not run / Phạm vi, dependency và chưa chạy

- G01–G03 only. No G04+, no new crate or product binary, and DeepSeek remains a separate preset on the generic OpenAI Chat adapter. `/init` prints a static sample and writes nothing. / Chỉ G01–G03; không thêm crate/binary sản phẩm; DeepSeek là preset riêng trên adapter OpenAI Chat tổng quát; `/init` chỉ in mẫu tĩnh.
- `chrono =0.4.45` (MIT OR Apache-2.0) is an exact workspace pin used for HTTP-date parsing; `reqwest =0.12.24` (MIT OR Apache-2.0) remains the transport. No new crate was added. Both are listed in the SPEC. / `chrono =0.4.45` (MIT OR Apache-2.0) là pin exact dùng parse HTTP-date; `reqwest =0.12.24` (MIT OR Apache-2.0) dùng transport; không thêm crate.
- No paid/live API call; no `config.local.toml` write; no HA user app install. The user authorized installing Clippy and committing/pushing this assignment; implementation commit `0b6d19d` is on `origin/master`. The additive SQLite migration was exercised by its tests; do not manually rerun or reset user data. / Không gọi API trả phí, không ghi `config.local.toml`, không cài HA lên máy user. User đã cho phép cài Clippy và commit/push assignment này; commit implementation `0b6d19d` đã có trên `origin/master`. Migration SQLite additive đã được test; không chạy migration thủ công hoặc reset dữ liệu user.
- **Not run / Chưa chạy:** Linux verification; paid provider smoke; real HA install/PATH mutation; repairing the default Rustup home while Rust Analyzer still holds the pinned toolchain DLL. / Chưa chạy xác minh Linux, smoke provider trả phí, cài HA/PATH thật; chưa phục hồi Rustup home mặc định khi Rust Analyzer còn giữ DLL của toolchain.
- `pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest` after saving both metadata files: passed; **170 Markdown files**, 15 language pairs, all structural negative controls passed. / Chạy sau khi lưu hai file metadata: đạt; **170 file Markdown**, 15 cặp ngôn ngữ, toàn bộ negative control cấu trúc đạt.

**Acceptance conclusion / Kết luận nghiệm thu:** implementation remains `implemented_unverified`; Clippy is green on the isolated exact toolchain, but the final HA launch gate is red on one G03 loopback test. The loopback retry budget is exhausted, so CP-A is not marked green. / Phần triển khai vẫn `implemented_unverified`; Clippy xanh với toolchain chính xác được cô lập, nhưng gate HA cuối đỏ ở một test G03 loopback. Đã hết lượt retry loopback nên chưa chốt CP-A xanh.
