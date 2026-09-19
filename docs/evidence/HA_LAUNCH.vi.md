# Evidence HA_LAUNCH — track H01–H08

Trạng thái: **H01–H08 xong phần code** (H05 còn ca hard-kill giữa turn; H07 thiếu transcript PTY thật vì ConPTY không chạy trong sandbox; H08 không publish và chỉ mô phỏng máy sạch). Không có live provider smoke. H05 còn ca hard-kill giữa turn; H06 không ghi User PATH thật vì không được cấp quyền. H04/H05 chưa có live provider smoke vì không được cấp quyền. Tài liệu này được cập
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

## 5. H04 — tiến độ từng phần (G1, G2, G3 xong; còn live smoke không được cấp quyền)

H04 **chưa** hoàn tất. Ghi lại đúng phần đã xác minh để không bị đọc thành đã xong.

### 5.1. G1 — provider stream tăng dần

| Kiểm chứng G1 | Lệnh | Kết quả |
|---|---|---|
| Unit/integration của provider | `cargo test -p harness-providers` | 3 passed, 0 failed |
| Regression P2 (provider cũ) | `cargo test -p harness-cli --test phase_p2 --locked -- --test-threads=1` | 17 passed, 0 failed |

Selector G1: `g1_mock_stream_matches_the_buffered_boundary_event_for_event`,
`g1_cancellation_before_dispatch_reports_a_canceled_call`,
`g1_adapter_delivers_text_before_the_response_completes`.

Đã chứng minh:

- **Boundary tăng dần là additive**: `stream_events` là phương thức có default trên
  `ModelProvider` (mặc định bridge từ kết quả buffered), `MockProvider` và
  `DeepSeekAdapter` override; P2 regression 17/17 xanh nên call site cũ không đổi.
- **I10 (lõi)**: qua `DeepSeekAdapter` thật với fixture HTTP giữ body mở, client nhận
  `TextDelta` **trước khi** server gửi phần còn lại.
- **Cancellation**: hủy trước dispatch trả lỗi typed `provider_canceled`.

### 5.2. G2 — vòng lặp model→tool→model có bound

| Kiểm chứng G2 | Lệnh | Kết quả |
|---|---|---|
| Acceptance turn loop | `cargo test -p harness-cli --test interactive_session --locked` | 3 passed, 0 failed |
| Regression toàn CLI (serial) | `cargo test -p harness-cli --tests --locked -- --test-threads=1` | 205 passed, 0 failed |
| Lint toàn workspace | `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0 |

Selector G2: `g2_tool_results_return_to_the_model_and_the_turn_ends_with_the_answer`,
`g2_a_failed_tool_call_is_reported_instead_of_ending_the_turn`,
`g2_the_tool_loop_is_bounded_and_reports_which_bound_stopped_it`.

Đã chứng minh, trên store thật + workspace thật + tool gate thật (không mock gate):

- **Tool result quay lại model**: provider call thứ hai nhận `ProviderMessage` role
  `Tool` chứa kết quả `search_text`, và lượt đó kết thúc bằng câu trả lời cuối
  (`TurnStop::Final`).
- **Fail→fix**: tool call sai tên được báo lại cho model dưới dạng message lỗi và vòng
  lặp **tiếp tục** (2 provider call), không kết thúc turn — đây là hành vi mà I11 cần.
- **Bound**: provider luôn đòi thêm tool call thì vòng lặp dừng ở `TurnStop::StepLimit`
  với `max_steps=2` và số provider call không vượt bound.
- **Một admission cho mỗi user message**: `continue_run` không admit input mới; runtime
  kiểm tra rằng input identity được giữ nguyên.

Xem mục 5.3 cho G3, service thật và headless turn.


## 6. H05 — approval, resume và lifecycle (phần code xong)

| Kiểm chứng | Lệnh | Kết quả |
|---|---|---|
| Unit test (bin ha) | `cargo test -p harness-cli --bin ha --locked` | 58 passed, 0 failed |
| Launch + resume end-to-end | `cargo test -p harness-cli --test interactive_launch --locked -- --test-threads=1` | 11 passed, 0 failed |
| Session/turn acceptance | `cargo test -p harness-cli --test interactive_session --locked -- --test-threads=1` | 8 passed, 0 failed |
| Regression toàn CLI (serial) | `cargo test -p harness-cli --tests --locked -- --test-threads=1` | **222 passed, 0 failed** (P0 8, P1 21, P2 17, P3 20, P4 22, P5 27, P6 15, P7 15, unit 58, launch 11, session 8) |
| Lint toàn workspace | `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0 |

Selector H05 mới: `h05_a_denied_gated_action_is_not_executed_and_the_model_is_told`,
`h05_a_granted_gated_action_runs_once_after_the_answer`,
`h05_an_expired_approval_is_a_refusal_not_a_silent_grant`,
`h05_the_gate_expires_without_an_answer_and_never_grants_late`,
`h05_the_gate_forwards_the_users_answer`,
`h05_a_gated_action_is_rendered_and_answered_by_the_user`,
`h05_a_denial_is_recorded_and_a_stale_request_is_reported`,
`h05_resume_lists_sessions_and_selects_one_by_number`,
`h05_an_empty_listing_and_a_notice_are_rendered_honestly`,
`i13_resume_continues_the_task_with_recovered_context_and_no_rerun`,
`i13_resuming_an_unknown_session_fails_without_running_anything`.

Đã chứng minh:

- **I12 (approval)**: action bị gate chỉ chạy sau câu trả lời của user. Deny và expiry đều
  **không** thực thi (file fixture không đổi byte nào) và được báo lại cho model dưới dạng
  tool message; grant thì chạy đúng một lần. Gate hết hạn trả `Expired`, không bao giờ
  thành grant; id đã trả lời/không tồn tại trả `false` và UI nói rõ "not executed".
- **UI approval**: proposal hiện action/workspace/scope/request id; khi đang chờ, dòng khác
  không được admit thành request mới; trả lời y/n đổi phase đúng.
- **Resume**: `/resume` liệt kê session của **project hiện tại** (tối đa 20, mới nhất
  trước); `/resume <số|id>` chọn; id lạ bị từ chối tường minh; `/new` khi idle mở hội
  thoại mới và khi có run active thì từ chối.
- **I13 (continuation + recovery bằng process thật)**: hai lần chạy binary thật
  (`ha chat --headless ...` rồi `--resume <session>`) cho thấy lượt sau giữ nguyên task,
  dùng session mới, báo `resumed_from`, **và request gửi provider có chứa prompt của lượt
  trước** — tức context được phục hồi thật, không phải bắt đầu lại. Resume id lạ fail và
  không chạy gì.

Chưa chứng minh (còn lại của H05/H07):

- **Hard kill giữa turn sau khi receipt đã commit** rồi mở lại resume: chưa có ca test
  riêng (cần kill process thật giữa lúc tool đã settle). Cơ chế continuation/recovery đã
  được chứng minh, nhưng nhánh kill thì chưa.
- PTY/terminal thật cho I07/I08 vẫn thuộc H07.
- Live provider smoke: **not_run**.


## 7. H06 — installer và command resolution

| Kiểm chứng | Lệnh | Kết quả |
|---|---|---|
| Self test của installer | `pwsh -NoProfile -File scripts/Install-Ha.ps1 -SelfTest` | 14 check OK, exit 0 |
| Cài vào thư mục tạm (copy route) | `pwsh -NoProfile -File scripts/Install-Ha.ps1 -Destination <temp> -SkipBuild -Profile Debug` | exit 0, manifest khớp digest |
| Command resolution | `Get-Command ha -CommandType Application`, `where.exe ha` trong shell con với PATH có kiểm soát | resolve đúng binary đã cài, từ cwd ngoài repo có dấu cách |
| Binary đã cài chạy được | `ha --version`; `ha chat --fixture` qua pipe | `ha 0.1.0`; non-TTY exit 2 (giữ nguyên guard của H01) |
| Shadowing | fake `ha.cmd` đứng trước trên PATH | có WARNING, file lạ **vẫn còn** |
| PATH merge qua writer tiêm | `-ModifyUserPath` + provider/writer tiêm, chạy hai lần | lần 1 append đúng một lần; lần 2 không ghi gì (writerCalls=1, không nhân bản) |
| User PATH thật | so trước/sau mọi lần chạy trên | **không đổi** |
| Cargo route | `-UseCargoInstall -Destination <temp> -Profile Debug` | exit 0, binary + manifest trong temp root |

Check trong self test: `merge_appends_a_missing_directory`,
`merge_is_case_insensitive_and_ignores_a_trailing_separator`,
`merge_drops_empty_entries_and_keeps_order`,
`merge_never_folds_machine_or_process_entries_into_user_path`,
`injected_writer_receives_the_merged_user_path`,
`shadowing_command_is_found_before_the_owned_binary`,
`shadowing_command_is_never_deleted`,
`disposable_install_replaces_and_verifies_the_artifact`,
`installed_digest_matches_the_built_artifact`,
`install_manifest_records_version_and_digest`, `update_keeps_the_binary_usable`,
`locked_executable_is_reported_as_in_use`,
`a_failed_replacement_leaves_the_previous_binary_usable`,
`self_test_never_writes_the_real_user_path`.

Đã chứng minh:

- **I14**: binary cài vào đích tạm được shell resolve đúng (cả `Get-Command` và
  `where.exe`) từ cwd unrelated có dấu cách, digest khớp manifest, và chạy được.
- **I15**: entry được append đúng một lần vào **User** PATH, không nhân bản ở lần chạy
  sau; negative control khẳng định entry của Machine/process không bao giờ lọt vào giá trị
  ghi; User PATH thật không bị đụng.
- **I17**: `ha` lạ đứng trước bị báo, không bị xóa.
- **I18**: file bị khóa được phân loại `in_use` (không phải "mọi lỗi là file in use") và
  binary cũ vẫn chạy được sau lần thay thế thất bại — tức có rollback thật.

Chưa chạy (không được cấp quyền, ghi rõ thay vì mặc định đạt):

- **Ghi User PATH thật** qua registry và **cài vào vị trí thật của user**: không thực hiện.
  Đường ghi được chứng minh bằng writer tiêm + so User PATH trước/sau.
- **I01 trên binary đã cài trong PTY**: thuộc H07 (hiện mới chứng minh từ chối non-TTY và
  `--version`/`--help` trên binary đã cài).
- Gate `Verify-HaLaunch.ps1`: H07 phải gọi `Install-Ha.ps1 -SelfTest` và lặp lại các ca
  cài tạm này.


## 8. H07 — gate và operator docs

| Kiểm chứng | Lệnh | Kết quả |
|---|---|---|
| Gate runtime của track | `pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1` | xem verdict bên dưới |
| Self test của gate | `pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -SelfTest` | `GATE_SELFTEST_OK`, exit 0 |
| Operator docs | `pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest` | DOCS_OK, 114 file |

Gate chạy và **xanh** cho: format, clippy (`-D warnings`), discovery selector bắt buộc
(6 selector trên hai target), unit test binary `ha`, acceptance `interactive_launch`
(11 test), `interactive_session` (8 test), provider streaming (3 test), regression
P0–P7 (tất cả với `--test-threads=1`), installer self test và docs checker.

Gate in rõ **not_run** và không tính là pass: transcript PTY thật, live provider smoke,
Linux, và mọi thao tác thật lên PATH/profile của user.

Đã chứng minh ở H07:

- **Gate có thể fail thật**: trong lần chạy đầu, clippy bắt 2 lỗi trong file test PTY mới
  và gate báo `GATE_FAILED: clippy` (exit 1) — tức gate không phải hình thức.
- **Discovery chống mất coverage**: gate liệt kê test của từng target và fail nếu selector
  bắt buộc biến mất; self test của gate kiểm chính bộ parse này.
- **Migration note cho hành vi mới**: bare `ha` non-TTY giờ exit 2 kèm hướng dẫn; mục 12
  của operator guide (vi + en) ghi bảng tình huống, option `chat`, slash command, approval
  y/n, yêu cầu cấu hình provider, và danh sách chưa kiểm chứng.

**Blocker của môi trường (ghi rõ, không tính là đạt)**: harness PTY thật đã viết
(`portable-pty = "=0.9.0"` + `crates/harness-cli/tests/interactive_terminal.rs` với ba ca
I01/I06/I07), nhưng trong sandbox này ConPTY **spawn được process mà không đọc được byte
nào từ master và process con không thoát** — đã loại trừ lỗi của `ha` bằng cách thử
`cmd.exe /c echo`: cùng hiện tượng. Ba ca được `#[ignore]` kèm lý do và gate báo not_run.
Vì vậy I01/I06/I07/I08 ở dạng transcript terminal thật **chưa** đạt; render loop đang được
chứng minh bằng scripted backend (H03) và guard non-TTY bằng launch test.


## 9. H08 — release candidate và clean-machine install (không publish)

| Kiểm chứng | Lệnh | Kết quả |
|---|---|---|
| Self test packaging | `pwsh -NoProfile -File scripts/New-HaRelease.ps1 -SelfTest` | `RELEASE_SELFTEST_OK` (bundle contents, manifest digest, negative control file lạ) |
| Build candidate | `pwsh -NoProfile -File scripts/New-HaRelease.ps1 -SkipBuild` | bundle `ha-0.1.0-windows-x64` gồm `ha.exe`, `ha.release.json`, `checksums.txt`; zip sha256 `3d5b476e825ebb378a3ac10570a9f9222f7d99c784cfa2c3390e2cf5c9b7ae9a`; `published: false` |
| Gate runtime (đã thêm bước release) | `pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json` | `"passed": true`, `"failures": []` |
| Installer self test mở rộng | `pwsh -NoProfile -File scripts/Install-Ha.ps1 -SelfTest` | 18 check OK (thêm bundle install, bundle bị sửa bị từ chối, uninstall chỉ xóa file sở hữu, user data được giữ) |

**I19 (máy sạch — mô phỏng)**: cài từ bundle vào thư mục tạm, sau đó chạy với PATH tối
thiểu (chỉ thư mục cài + `System32`, `toolchainOnMinimalPath=False`):

- `ha --version` → `ha 0.1.0` (không cần Rust/Git/Node, không cần source repo);
- bare `ha` không có terminal → exit **2** kèm hướng dẫn (đúng hành vi mới, migration note
  đã ghi ở operator guide mục 12);
- `ha init --data-dir <temp>` tạo store thật → `userDataCreated=True`.

**I20 (update rồi uninstall)**: update từ cùng bundle → `stillRuns=True`; uninstall →

- `Removed:` đúng hai file sở hữu (`ha.exe`, `ha.install.json`);
- `foreignFileKept=True` (file lạ cạnh binary không bị xóa);
- `userDataKept=True` (config/store của user còn nguyên);
- `PATH:` báo không có entry nào do lần cài này thêm;
- reinstall sau uninstall → `reinstalledRuns=True`.

**Chưa chạy (ghi rõ, không tính là đạt)**:

- **Publish release**: không được cấp quyền → không publish, không URL, không tag.
- **VM/máy sạch thật** (I19/I20 trên máy không có build toolchain thực sự): chỉ **mô phỏng**
  bằng PATH tối thiểu + thư mục tạm; chưa có VM trong session này.
- **Linux x64 bundle**: không có host/toolchain Linux ở đây, nên chỉ công bố Windows x64.
- **Ghi User PATH thật**: vẫn không thực hiện; đường ghi chỉ được chứng minh bằng writer tiêm.


## 10. Bổ sung acceptance bằng process thật (I09/I16) và ca gián đoạn của H05

| Kiểm chứng | Lệnh | Kết quả |
|---|---|---|
| Launch acceptance (đã thêm I09/I16) | `cargo test -p harness-cli --test interactive_launch --locked -- --test-threads=1` | 14 passed, 0 failed |
| Session acceptance (đã thêm ca gián đoạn) | `cargo test -p harness-cli --test interactive_session --locked -- --test-threads=1` | 9 passed, 0 failed |
| Gate runtime với 9 selector bắt buộc | `pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json` | `"passed": true`, `"failures": []` |
| Lint toàn workspace | `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0 |

Selector mới: `i09_a_corrupt_configuration_stops_the_run_with_an_actionable_error`,
`i09_an_invalid_project_directory_stops_the_run_with_an_actionable_error`,
`i16_a_second_run_in_the_same_project_is_refused_while_the_first_holds_the_store`,
`h05_a_settled_receipt_is_not_re_executed_after_the_process_state_is_lost`.

Đã chứng minh:

- **I09**: config hỏng và `--cwd` không tồn tại đều **dừng trước khi chạy** (exit ≠ 0,
  stdout rỗng), thông báo nêu đúng đường dẫn và hướng dẫn `ha config validate`, và **không**
  in lại giá trị bị từ chối.
- **I16**: khi process thứ nhất đang giữ store của project (đã xác nhận nó tới được provider,
  tức writer đã mở), process thứ hai bị từ chối với `writer_locked` / "another writable
  host" — không có hai writer cùng ghi một store.
- **H05 (gián đoạn)**: sau khi receipt đã settle và **toàn bộ state in-memory bị drop**
  (mô phỏng kill trung thực cho durable state: writer generation mới mở từ đĩa), lượt tiếp
  theo qua `continue_task_streaming`: `tool_calls = 0`, side effect trên file **không đổi
  byte nào**, số receipt trong store vẫn **đúng 1**, và request gửi provider mang context
  của lượt trước.

**Phát hiện mới (ghi vào SPEC)**: `ToolPolicy` của P3 yêu cầu approval cho **mọi** action
không bị deny, nên đường headless (không có người để hỏi) **fail closed cho mọi tool call**.
Đó là hành vi đúng theo "không blanket grant", nhưng nghĩa là headless hiện không thể hoàn
thành công việc cần tool; một flag automation-approval tường minh là quyết định contract cần
user chốt nên chưa được thêm. Ca **kill process thật** giữa turn vẫn cần PTY (H07) và vẫn là
not_run.


## 11. Grant round 9 (publish + credential): đã chuẩn bị, chưa thực thi được

User cấp quyền **(a)** cho live smoke và publish candidate. Hai blocker đo được tại thời
điểm này (không phải suy đoán):

| Cần | Đo được | Hệ quả |
|---|---|---|
| Credential cho smoke | `DEEPSEEK_API_KEY`, `HA_API_KEY`, `HA_PROVIDER_ENDPOINT`, `HA_PROVIDER_MODEL` đều **unset** trong env | không thể gọi model; smoke vẫn phải `not_run` cho tới khi có credential thật |
| Kênh publish | `gh` **không cài**, `GH_TOKEN`/`GITHUB_TOKEN` **unset**, chưa có tag nào | không thể tạo release; không có đường publish nào để chạy |

Đã làm sẵn (đều verify được ngay bây giờ, không cần hai input trên):

- `scripts/Smoke-HaProvider.ps1`: `SMOKE_SELFTEST_OK`; khi thiếu cấu hình in
  `SMOKE_NOT_RUN` kèm danh sách biến thiếu, nêu rõ "no fixture is substituted and no paid
  call is made", và **exit 2** (đã đo). Khi có credential: chạy đúng **một** turn qua binary
  đã build, in model + endpoint đã che (scheme://host) + exit code + response, và **từ chối
  ghi evidence** nếu credential xuất hiện trong output.
- `scripts/New-HaRelease.ps1 -PublishDryRun`: in channel status (`NOT available`), tag
  `ha-v0.1.0`, và đúng các lệnh sẽ chạy (`git tag` → `git push origin <tag>` →
  `gh release create`), rồi `exit 0` mà **không** publish gì. Candidate hiện có:
  `target/release-candidate/ha-0.1.0-windows-x64/` + `.zip` (zip sha256
  `899ac8e62fa2e079beecaed197369a7b23d647b0146e278d4c35f0cdccd0e97d`; digest của
  `ha.exe` bên trong bundle là danh tính ổn định, zip đổi hash giữa các lần đóng gói).

**Việc cần từ user để đi tiếp** (một trong hai, hoặc cả hai):

1. Smoke: đặt `HA_PROVIDER_ENDPOINT`, `HA_PROVIDER_MODEL` và `DEEPSEEK_API_KEY`
   (hoặc `HA_API_KEY`) rồi chạy `pwsh -NoProfile -File scripts/Smoke-HaProvider.ps1`.
   Tôi không đọc secret từ chỗ khác và không tự tạo credential.
2. Publish: cài `gh` **hoặc** set `GH_TOKEN` (kèm xác nhận push tag `ha-v0.1.0` lên
   `origin`). Sau đó `-PublishDryRun` sẽ báo channel ready và bước publish thật mới chạy.


## 12. PTY thật: I01 đạt, i06/i07 còn lỗi đo được (round 10)

Phát hiện nguyên nhân gốc và sửa harness:

1. **Master handle phải sống suốt session** — drop nó là đóng pseudo-console (im lặng, con
   treo). Đã giữ trong struct.
2. **Harness phải làm việc của terminal emulator**: ConPTY hỏi vị trí con trỏ bằng
   `ESC[6n` khi app bật VT input mode và app **chặn** tới khi có trả lời. Harness nay trả
   `ESC[1;1R` khi thấy query.
3. **ConPTY cần console thật**: process tạo pseudo-console phải sở hữu một console; `cargo
   test` trong sandbox thì không. Vì vậy có runner có bound:
   `scripts/Invoke-HaPtyAcceptance.ps1` (mở console mới, timeout cứng, lưu transcript).
4. **Thứ tự env trong harness**: strip credential phải chạy *trước* env của test, nếu không
   nó xoá luôn credential mà test cố set.

| Kiểm chứng | Lệnh | Kết quả |
|---|---|---|
| I01 trong PTY thật | `pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1 -Filter i01_bare_launch -TimeoutSeconds 180` | `PTY_EXIT: 0`, `i01 ... ok`, transcript lưu ở `target/pty-acceptance/pty-transcript.txt` |

Đã chứng minh: bare `ha` trong **pseudo-console thật** render header (project/setup state),
giữ process sống ở prompt, nhận `/exit` và thoát **0**, trả terminal về trạng thái dùng
được — đây là I01, không còn là suy luận từ scripted backend.

Còn lỗi đo được (ghi đúng, chưa sửa):

- **i06** fails ở kỳ vọng bracketed paste (cần so lại chuỗi gửi/nhận trong console thật).
- **i07** **treo harness** khi chạy trong console (đã kill bằng timeout của runner) — nghi
  deadlock giữa luồng đọc (đang giữ lock writer để trả DSR) và `send`; cần sửa trước khi
  kết luận về Ctrl-C qua ConPTY.
- Vì vậy cả ba ca vẫn `#[ignore]` (lý do nay là "cần console thật + trạng thái i06/i07"),
  và gate vẫn báo chúng là not_run.

## 13. Chưa xác minh (không được coi là đạt)

- **I01 PTY transcript**: chưa có. Cần terminal thật/PTY harness (H07). Unit test
  dùng fixture detector chỉ chứng minh logic capability, không phải bằng chứng
  interactive launch.
- **H02–H08**: chưa có dòng code nào. Mọi mục trong bảng staging của SPEC vẫn "chờ".
- **Live provider**: không chạy; không có credential/budget được cấp.
- **User PATH / cài binary thật**: không thực hiện; không được cấp quyền.
- **Publish release / push remote**: không thực hiện; không được cấp quyền.
- **Linux**: chưa build/chạy; mọi kết quả trên là Windows.

## 14. Ghi chú flake môi trường (đã điều tra, không che)

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

### 5.3. G3 + service thật + headless turn

| Kiểm chứng | Lệnh | Kết quả |
|---|---|---|
| Session/turn acceptance | `cargo test -p harness-cli --test interactive_session --locked -- --test-threads=1` | 5 passed, 0 failed |
| Launch + headless end-to-end | `cargo test -p harness-cli --test interactive_launch --locked -- --test-threads=1` | 9 passed, 0 failed |
| Regression toàn CLI (serial) | `cargo test -p harness-cli --tests --locked -- --test-threads=1` | **211 passed, 0 failed** |
| Lint toàn workspace | `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0 |

Selector mới: `g3_a_second_input_in_the_same_session_carries_real_context`,
`g3_the_foundation_admits_one_input_per_session_and_says_so`,
`i03_headless_turn_runs_through_the_real_adapter_and_keeps_the_key_out_of_output`,
`i12_headless_turn_without_provider_configuration_fails_closed`.

Đã chứng minh:

- **Headless end-to-end thật**: binary `ha chat --headless --prompt ... --json` chạy qua
  production adapter tới fixture HTTP thật trong test, in JSON đúng
  (`response`, `stop: final`, `fixture: false`, `approvals: none`), exit 0; request
  mang `Authorization: Bearer …`; **không** stream nào chứa credential và không có ANSI.
- **Thiếu cấu hình thì fail closed**: không set endpoint/model/key → exit khác 0, stdout
  rỗng, stderr nêu đúng tên biến và nói rõ "no fixture answer was substituted".
- **Chuỗi session cho hội thoại**: lượt hai chạy qua `continue_task_streaming` với cùng
  task, session mới, packet khác lượt một và chứa input mới.
- **Luật nền tảng được ghi bằng test**: input thứ hai trong cùng session trả
  `idempotency_conflict` ("more than one admitted input"), nên không có đường nào âm
  thầm ghi đè journal.
- **Fixture không thể lọt vào production**: `PendingService` bị xóa; service thật là mặc
  định; `--fixture` vẫn chỉ là opt-in có nhãn và bị từ chối cho headless (test H03).

Chưa chứng minh / còn mở:

- **Live provider smoke: not_run** — assignment không cấp credential/budget.
- **Nhiều input trong đúng một session** không được nền tảng P1 hỗ trợ (gap đã ghi trong
  SPEC); hiện dùng chuỗi session cùng task.
- `ProjectId` trong một phiên vẫn sinh mới; identity bền theo project hiện là thư mục
  store (`project-<hash>`). Nối registry project là việc của H05.
- **I13** (hard kill rồi `/resume`) và lifecycle cancel/close đầy đủ thuộc H05.