# Evidence HA_LAUNCH — track H01–H08

Trạng thái: **H01 xong phần dispatch; H02–H08 chưa bắt đầu.** Tài liệu này được cập
nhật lại sau mỗi checkpoint; trạng thái ở đây là trạng thái thật tại thời điểm ghi,
không phải trạng thái dự kiến.

## 1. Kết quả và source

Assignment: triển khai H01–H08 theo [plan HA_LAUNCH](../HA_LAUNCH_PLAN.vi.md), gate
từng checkpoint, không chuyển tiếp khi prerequisite chưa đạt. Quyền được giao và
quyền **không** được giao nằm ở mục 0 của [SPEC HA_LAUNCH](../specs/HA_LAUNCH.vi.md).

Base revision: `d6a346951197e53d931fc572dd05c42a3c3fe002`, working tree sạch trước khi
sửa. Baseline build trước khi viết code: `cargo test --workspace --no-run` trả về
exit 0 (toàn bộ test binary của workspace dựng được trên Windows, rustc 1.97.1,
cargo 1.97.1).

Checkpoint commit của H01: `047859b2f3e62a157b1b93aef5e880b9c55fb5f4` (tree
`d9b0c4f20da9a0d48b46a782608c98290f65e21e`), commit local, **không push** — push
không được cấp quyền trong assignment này. Digest sha256 rút gọn của file H01:
`mod.rs c0cd13d9bf1cc8fe`, `detector.rs f14b2e134f15ba03`, `app.rs bee70860ae5a3831`,
`headless.rs 76c4ca842ff02ad9`, `interactive_launch.rs 0a72d70d6f86acb1`.

H01 kết thúc ở mức **dispatch contract + parser + guard non-TTY**. Không có tuyên bố
nào về UI tương tác thật (H03) hay agent service (H04); `ha chat --headless` trả lỗi
typed `service_unavailable` và được H04 thay thế.

### 1.1. Thay đổi source của H01

| File | Thay đổi |
|---|---|
| `crates/harness-cli/src/main.rs` | +72/-2: `mod interactive`, subcommand `chat`, `run()` định tuyến launch contract, hàm cũ đổi tên thành `legacy_run` với thân không đổi, `main` map `Ok(ExitCode)` |
| `crates/harness-cli/src/interactive/mod.rs` | mới: `LaunchMode`, `mode_from_args`, `USAGE_EXIT_CODE`, guard non-TTY, guidance stderr |
| `crates/harness-cli/src/interactive/detector.rs` | mới: `TerminalCapability`, trait `TerminalDetector`, `SystemTerminalDetector`, fixture detector chỉ dùng trong unit test |
| `crates/harness-cli/src/interactive/app.rs` | mới: boot header tối thiểu + prompt line-mode, ghi rõ `connection pending`, `/help` và `/exit` |
| `crates/harness-cli/src/interactive/headless.rs` | mới: contract headless; chưa nối service, fail closed |
| `crates/harness-cli/tests/interactive_launch.rs` | mới: 6 integration test I01/I02/I03 qua binary thật |
| `docs/specs/HA_LAUNCH.vi.md` | mới: SPEC, bảng quyền, dispatch table, staging theo checkpoint |

## 2. Đã xác minh cho H01

Lệnh và kết quả thật, chạy trên Windows 11 x64, rustc 1.97.1, PowerShell 7:

| Kiểm chứng | Lệnh | Kết quả |
|---|---|---|
| Format | `cargo fmt --all -- --check` | xanh |
| Lint | `cargo clippy -p harness-cli --all-targets -- -D warnings` | exit 0, không warning |
| Unit test | `cargo test -p harness-cli --bin ha` | 10 passed, 0 failed |
| Integration test | `cargo test -p harness-cli --test interactive_launch` | 6 passed, 0 failed |
| Regression CLI | `cargo test -p harness-cli --tests --locked` | 161 passed, 0 failed: P0 8, P1 21, P2 17, P3 20, P4 22, P5 27, P6 15, P7 15, H01 unit 10, H01 integration 6 |

Selector unit test (H01): `h01_mode_from_args_accepts_bare_and_optioned_interactive_launch`,
`h01_mode_from_args_requires_prompt_for_headless`,
`h01_mode_from_args_rejects_headless_only_flags_without_headless`,
`h01_headless_mode_carries_prompt_and_json_flag`,
`h01_non_terminal_guidance_points_at_the_explicit_headless_command`,
`h01_capability_requires_both_streams`,
`h01_fixed_detector_reports_the_injected_capability`,
`h01_line_action_parses_slash_commands_without_treating_text_as_commands`,
`h01_boot_context_labels_pending_setup_instead_of_a_fake_provider`,
`h01_boot_context_marks_resume_as_pending_until_h05`.

Selector integration test (H01):

- `i03_bare_launch_without_a_terminal_exits_two_with_instructions` — chạy binary thật
  với stdin **vẫn mở** (không EOF) và có timeout 30s: process thoát exit 2 ngay, nên
  guard chứng minh được là không đọc stdin, không treo. stderr có hướng dẫn
  `ha chat --headless --prompt`, stdout rỗng, sandbox không có file nào được ghi.
- `i01_chat_without_a_terminal_uses_the_same_guard` — `ha chat` qua pipe cũng exit 2.
- `i02_help_and_version_stay_fast_paths_that_write_nothing` — `--help`, `--version`,
  `memory --help`, `maintenance --help`, `chat --help` đều exit 0 và không tạo file
  nào trong `HA_HOME` tạm.
- `i02_existing_subcommands_keep_their_dispatch_and_output` — `config validate` và
  `config validate --json` giữ nguyên text/JSON cũ (fixture P0).
- `i02_unknown_options_and_missing_arguments_remain_parser_errors` — unknown option và
  thiếu required argument vẫn là parser error exit 2, không bị coi là prompt.
- `i03_headless_rejects_the_headless_only_flags_and_keeps_stdout_plain` — `--headless`
  thiếu `--prompt`, `--prompt`/`--json` không có `--headless` đều exit 2; lượt headless
  hợp lệ không in ký tự điều khiển terminal ra stdout và trả typed `service_unavailable`.

## 3. Chưa xác minh (không được coi là đạt)

- **I01 PTY transcript**: chưa có. Cần terminal thật/PTY harness (H07). Unit test
  dùng fixture detector chỉ chứng minh logic capability, không phải bằng chứng
  interactive launch.
- **H02–H08**: chưa có dòng code nào. Mọi mục trong bảng staging của SPEC vẫn "chờ".
- **Live provider**: không chạy; không có credential/budget được cấp.
- **User PATH / cài binary thật**: không thực hiện; không được cấp quyền.
- **Publish release / push remote**: không thực hiện; không được cấp quyền.
- **Linux**: chưa build/chạy; mọi kết quả trên là Windows.

## 4. Ghi chú flake môi trường (đã điều tra, không che)

Lần chạy `cargo test -p harness-cli --tests` đầu tiên sau khi thêm H01 có 1 test đỏ:
`phase_p2::p2_s02_provider_streams_and_deepseek_sse_adapter_are_normalized`, lỗi
`provider request failed: error sending request for url (http://127.0.0.1:<port>/...)`
(tầng connect tới fixture server loopback trong chính process test).

Điều tra: source `harness-providers` không bị sửa trong H01 (`git diff --stat` chỉ có
`crates/harness-cli/src/main.rs`), test này không link code của `harness-cli`. Chạy
riêng lại: 3/3 pass; chạy lại ở revision baseline (stash toàn bộ thay đổi H01): pass;
sau khi restore: 3/3 pass. Kết luận: flake môi trường ở tầng loopback, không phải
regression của H01 — nhưng đây là rủi ro mở cho các test HTTP fixture của H04, nên
được ghi lại thay vì bỏ qua.

