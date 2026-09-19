# Evidence HA_LAUNCH — track H01–H08

Trạng thái: **H01–H03 xong ở mức được ghi dưới đây; H04–H08 chưa bắt đầu.** Tài liệu này được cập
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

## 3. Đã xác minh cho H02

| Kiểm chứng | Lệnh | Kết quả |
|---|---|---|
| Lint | `cargo clippy -p harness-cli --all-targets -- -D warnings` | exit 0, không warning |
| Unit test | `cargo test -p harness-cli --bin ha` | 25 passed, 0 failed (H01 8 + H02 17) |
| Integration test | `cargo test -p harness-cli --test interactive_launch` | 6 passed, 0 failed |
| Regression CLI | `cargo test -p harness-cli --tests --locked -- --test-threads=1` | 176 passed, 0 failed: P0 8, P1 21, P2 17, P3 20, P4 22, P5 27, P6 15, P7 15, unit 25, H01 integration 6 |
| Docs | `pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest` | DOCS_OK, 114 file Markdown, 15 cặp ngôn ngữ |

Selector H02: `h02_windows_defaults_follow_the_documented_user_locations`,
`h02_linux_defaults_prefer_xdg_then_home`, `h02_ha_home_overrides_the_platform_default_and_stays_isolated`,
`h02_explicit_data_dir_outranks_ha_home`, `h02_empty_variables_are_unset_and_an_unresolvable_home_is_an_error`,
`h02_missing_configuration_is_a_first_run_not_an_error`,
`h02_valid_configuration_loads_and_is_described_with_its_schema_version`,
`h02_unknown_field_and_unsupported_schema_keep_their_codes_and_name_the_path`,
`h02_corrupt_configuration_is_actionable_and_never_replaced_by_defaults`,
`h02_project_identity_follows_the_caller_directory_with_spaces_and_unicode`,
`h02_missing_or_non_directory_project_is_an_actionable_error`,
`h02_relative_cwd_resolves_from_the_caller_and_git_root_comes_from_the_project_tree`,
`h02_empty_home_without_credentials_opens_setup_state_and_writes_nothing`,
`h02_credential_presence_is_detected_without_exposing_its_value`,
`h02_two_terminals_in_one_project_share_a_store_and_the_second_is_busy`,
`h02_unresolvable_user_locations_are_reported_instead_of_guessed`,
`h02_boot_render_uses_the_resolved_context_and_keeps_the_prompt_alive`.

Đối chiếu acceptance, phần H02 chứng minh được:

- **I04** — project identity theo caller cwd với path có spaces và Unicode; `--cwd`
  tương đối resolve từ caller; Git root lấy từ cây project; project không có `.git`
  vẫn mở và header ghi rõ Git không khả dụng; test khẳng định project **không** đến từ
  `CARGO_MANIFEST_DIR`.
- **I05** — `HA_HOME` rỗng, không credential: app mở ở setup state, config là first
  run, setup hint nêu đúng file/biến cần sửa; launch **không tạo file nào** trong
  `HA_HOME` lẫn trong project (assert thư mục rỗng sau khi resolve).
- **I09** — config hỏng trả `config_parse_error` nêu path và hướng dẫn
  `ha config validate`, không echo giá trị bị từ chối, và file giữ nguyên byte;
  unknown field trả `config_unknown_field`; schema không hỗ trợ trả
  `unsupported_schema_version`; cwd không tồn tại và path là file đều trả lỗi nêu
  đường dẫn; môi trường không resolve được user location trả lỗi nêu `HA_HOME`.
- **I16** (phần store) — hai launch context cùng project resolve về cùng store dir,
  project khác resolve về store dir khác; mở writer lần thứ hai trên cùng thư mục trả
  `writer_locked` với thông báo "another writable host".
- **Không rò secret** — header, setup hint và mô tả config không chứa giá trị
  credential fixture; chỉ tên biến được nêu.
- **Path contract** — Windows dùng `APPDATA`/`LOCALAPPDATA`, Linux dùng XDG rồi
  `HOME`, `HA_HOME` override cả hai, explicit data dir ưu tiên cao nhất; tất cả test
  dùng environment tiêm, không đọc environment thật của máy chạy test.

Giới hạn của H02 (không được đọc là đã đạt end-to-end):

- I04/I05/I09/I16 ở đây là **unit test của bootstrap/paths/config** với fixture
  home/env, đúng như plan yêu cầu; bản end-to-end qua terminal thật thuộc H07 (PTY).
- **Thư mục project read-only chưa có test**: mới có "không tồn tại" và "là file".
  Đây là gap được ghi lại, không được coi là đã phủ.
- Provider endpoint/model chưa được resolve; H02 chỉ kiểm tra sự hiện diện của
  credential. Lượt model thật là H04.
- Render tiếng Việt trên terminal thật chưa kiểm chứng (H03/H07).

## 4. Đã xác minh cho H03

| Kiểm chứng | Lệnh | Kết quả |
|---|---|---|
| Lint | `cargo clippy -p harness-cli --all-targets -- -D warnings` | exit 0 |
| Unit test | `cargo test -p harness-cli --bin ha` | 50 passed, 0 failed (H01 8, H02 17, H03 25) |
| Integration test | `cargo test -p harness-cli --test interactive_launch` | 7 passed, 0 failed |
| Regression CLI (serial) | `cargo test -p harness-cli --tests --locked -- --test-threads=1` | 202 passed, 0 failed: P0 8, P1 21, P2 17, P3 20, P4 22, P5 27, P6 15, P7 15, unit 50, H01 integration 7 |

Selector H03 phía unit:

- Editor: `h03_editor_edits_vietnamese_text_by_character`,
  `h03_editor_keeps_boundaries_and_ignores_empty_submissions`,
  `h03_editor_submits_once_and_remembers_history`,
  `h03_editor_paste_never_submits_multiple_commands`,
  `h03_editor_ctrl_c_and_ctrl_d_follow_the_plan`.
- Từ vựng/state: `h03_phase_labels_and_active_run_are_explicit`,
  `h03_terminal_outcomes_are_labelled_for_the_transcript`.
- Port: `h03_pending_service_accepts_then_reports_that_nothing_ran`,
  `h03_fixture_service_is_labelled_and_streams_a_full_run`,
  `h03_cancel_reports_a_canceled_run`.
- View: `h03_prompt_marks_a_busy_phase_and_stays_readable`,
  `h03_help_lists_the_commands_the_plan_requires`,
  `h03_tool_and_run_lines_and_short_ids_are_stable`.
- Controller: `h03_one_admission_per_message_and_a_running_run_refuses_a_second`,
  `h03_ctrl_c_cancels_a_run_and_clears_an_idle_prompt`,
  `h03_fixture_run_streams_text_before_tools_and_returns_to_ready`,
  `h03_text_and_terminal_events_are_rendered_before_the_run_ends`,
  `h03_pending_service_says_connection_pending_and_keeps_setup_state`,
  `h03_slash_commands_are_parsed_and_staged_features_stay_honest`.
- Terminal backend: `h03_keys_are_mapped_from_real_crossterm_events`,
  `h03_paste_resize_and_key_release_are_handled`.
- Host loop với scripted backend (không PTY):
  `h03_scripted_terminal_renders_the_boot_header_and_exits_cleanly`,
  `h03_scripted_terminal_echoes_vietnamese_input_and_reports_connection_pending`,
  `h03_scripted_terminal_edits_with_backspace_before_submitting`,
  `h03_resize_redraws_the_prompt_without_losing_the_buffer`,
  `h03_fixture_run_renders_a_requested_failure_and_is_labelled`.
- Parser: `h03_fixture_is_rejected_for_a_headless_turn`.
- Integration: `i03_fixture_route_never_bypasses_the_terminal_or_headless_contract`.

Đối chiếu acceptance, phần H03 chứng minh được:

- **I06 (phần logic)**: gõ tiếng Việt theo ký tự, backspace/delete/home/end, history có
  nhớ draft, paste một lần không tự submit, resize vẽ lại prompt không mất buffer.
- **I07 (phần logic)**: Ctrl-C khi đang chạy gọi cancel và chuyển `Canceling`; Ctrl-C
  khi idle clear input; Ctrl-D trên buffer rỗng thoát; `/exit` hủy run active trước khi
  thoát.
- **I10 (phần render)**: `TextDelta` được render **trước** khi run kết thúc và trước
  dòng tool/terminal; test khẳng định thứ tự effect, không phải animation sau khi xong.
- **I12 (phần admission)**: một input được admit mỗi message; input thứ hai trong lúc
  chạy bị từ chối, không vào service.
- **Fixture honesty**: fixture có nhãn trong header, echo đúng buffer đã admit, render
  được cả đường failed; mặc định production vẫn báo `connection pending`, và
  `--fixture` không bypass guard non-TTY cũng không dùng được cho headless.

Chưa xác minh (không được coi là đạt):

- **Raw mode/PTY thật**: I01/I06/I07/I08 ở dạng transcript terminal thật thuộc H07.
  Cụ thể chưa chứng minh: Ctrl-C có được giao thành key event trên Windows/ConPTY,
  tiếng Việt hiển thị đúng trên terminal thật, và guard restore terminal sau lỗi thật
  (hiện có RAII + unit test đường thoát, chưa có test lỗi sau khi vào raw mode).
- **Multiline editing**: chưa có; paste nhiều dòng bị đổi newline thành space.
- **Linux**: chưa build/chạy.

## 5. Chưa xác minh (không được coi là đạt)

- **I01 PTY transcript**: chưa có. Cần terminal thật/PTY harness (H07). Unit test
  dùng fixture detector chỉ chứng minh logic capability, không phải bằng chứng
  interactive launch.
- **H02–H08**: chưa có dòng code nào. Mọi mục trong bảng staging của SPEC vẫn "chờ".
- **Live provider**: không chạy; không có credential/budget được cấp.
- **User PATH / cài binary thật**: không thực hiện; không được cấp quyền.
- **Publish release / push remote**: không thực hiện; không được cấp quyền.
- **Linux**: chưa build/chạy; mọi kết quả trên là Windows.

## 6. Ghi chú flake môi trường (đã điều tra, không che)

Test `phase_p2::p2_s02_provider_streams_and_deepseek_sse_adapter_are_normalized` thỉnh
thoảng đỏ ở tầng connect tới fixture server loopback trong chính process test:

```text
fixture adapter stream: ProviderError { code: ProviderProtocol,
  message: "provider request failed: error sending request for url (http://127.0.0.1:<port>/chat/completions)" }
```

Tần suất ghi nhận: **2 lần đỏ trong 5 lần chạy full suite** (`cargo test -p harness-cli
--tests --locked`) và **1 lần đỏ trong 4 lần chạy riêng** test đó; các lần còn lại
xanh. Khi full suite đỏ ở phase_p2 thì cargo dừng luôn nên phase_p3–p7 không chạy
trong lần đó.

Điều tra đã làm:

- Binary test `target/debug/deps/phase_p2-cf09fe581eff9fc0.exe` có mtime **trước** khi
  H01/H02 được viết, tức nó không được build lại trong các lần đỏ — loại giả thuyết
  "lần chạy đầu sau khi link lại".
- `git diff --stat` của H01/H02 chỉ có `crates/harness-cli`; `harness-providers` không
  bị sửa, và integration test này không link code của `harness-cli`. Cùng binary đó
  vừa xanh vừa đỏ qua các lần chạy.
- Server fixture bind `127.0.0.1:0` **thành công** (address lấy được, và không có panic
  `fixture listener`/`fixture accepts` nào xuất hiện); lỗi nằm ở phía client connect.
- Không có biến proxy trong environment; probe loopback TCP từ PowerShell cùng session
  connect được bình thường.

Kết luận: flake ở tầng network loopback của môi trường chạy test, **không phải**
regression của H01/H02. Đây là rủi ro mở được ghi lại cho H04 (test HTTP fixture qua
production adapter) và cho gate H07: gate không được báo xanh nếu bỏ qua flake này, và
không được sửa acceptance P2 đã được chấp nhận khi chưa có quyền.
Bổ sung sau khi lặp lại có kiểm soát (cùng một binary, không đổi code):

| Cách chạy | Số lần | Kết quả |
|---|---|---|
| `cargo test -p harness-cli --test phase_p2` (mặc định, test chạy song song) | 3 | 2 lần đỏ ở `p2_s02` |
| `cargo test -p harness-cli --test phase_p2 -- --test-threads=1` | 2 | 2 lần xanh, 17/17 |

Vì vậy đây là **độ nhạy của test fixture với việc chạy song song trong môi trường
này**, không phải lỗi sản phẩm: cùng binary, cùng test, chỉ khác mức song song. Hệ quả
được ghi cho H07: gate `Verify-HaLaunch.ps1` phải chạy các test bị ảnh hưởng với
`--test-threads=1` (hoặc cơ chế tương đương có ghi chú), nếu không gate sẽ đỏ ngẫu
nhiên và không đủ tư cách làm bằng chứng. Không sửa acceptance P2 đã được chấp nhận.
