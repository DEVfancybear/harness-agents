# Evidence HA_LAUNCH — track H01–H08

Trạng thái: **H01–H08 xong phần code**. H05: đã có ca kill *process* thật giữa turn (đo phần admission/lease/replay); phần kill *sau khi tool receipt đã commit* vẫn chỉ ở mức mô phỏng trung thực cho durable state. H07: transcript PTY thật cho I01/I06/I07/I08 đã xanh qua runner console (không tính tự động trong gate vì sandbox không có console). H08: bundle candidate + installer + uninstall, **không publish**, máy sạch chỉ mô phỏng. H06: **không** ghi User PATH thật và không cài vào máy user (không được cấp quyền). H04/H05: **không** có live provider smoke (không được cấp credential/budget). Tài liệu này được cập
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

Còn lại của H05/H07 (ghi đúng mức đã đạt):

- **Kill process thật giữa turn**: **đã đo** ở round 14 (mục 10.1) — input admit một lần,
  không receipt, replay dừng ở input, host mới tiếp quản trong session mới.
- **Kill sau khi tool receipt đã commit**: vẫn chỉ mô phỏng trung thực cho durable state
  (drop toàn bộ in-memory + mở writer generation mới). Chưa có ca kill process thật ở đúng
  thời điểm đó, vì cần một tool chạy được mà đường headless thì fail closed.
- PTY/terminal thật cho I07/I08: **đã xanh** (mục 12.2–12.3) qua runner console.
- Live provider smoke: **not_run**.


## 7. H06 — installer và command resolution

| Kiểm chứng | Lệnh | Kết quả |
|---|---|---|
| Self test của installer | `pwsh -NoProfile -File scripts/Install-Ha.ps1 -SelfTest` | **26 check OK**, exit 0 (round 17–20 bổ sung các check môi trường sạch và fresh-shell; xem mục 15.3, 15.5) |
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
`bundle_install_uses_the_verified_executable`, `a_tampered_bundle_is_refused`,
`uninstall_removes_only_owned_files`, `uninstall_keeps_user_data`,
`self_test_never_writes_the_real_user_path`,
`installed_binary_reports_its_version_without_a_toolchain`,
`installed_binary_help_lists_the_launch_contract`,
`installed_binary_guards_a_non_terminal_launch`,
`fresh_shell_resolves_ha_to_the_installed_binary`,
`fresh_shell_runs_ha_by_name_without_a_toolchain`,
`fresh_shell_without_the_install_directory_does_not_resolve_ha`,
`powershell_fresh_shell_resolves_ha_to_the_installed_binary`.

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

Đã đóng ở round 21 (trước đây nằm trong danh sách "chưa chạy"):

- **I01 trên binary đã cài trong PTY**: ca
  `i14_the_installed_artifact_opens_the_app_in_a_real_terminal` chạy **artifact đã cài** trong
  pseudo-console thật, từ project ngoài thư mục cài (mục 18).
- **Gate gọi installer self test**: `Verify-HaLaunch.ps1` chạy `Install-Ha.ps1 -SelfTest` như
  một bước bắt buộc và lặp lại các ca cài tạm.


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
| Launch acceptance (đã thêm I09/I16 và kill process thật) | `cargo test -p harness-cli --test interactive_launch --locked -- --test-threads=1` | 15 passed, 0 failed |
| Session acceptance (đã thêm ca gián đoạn) | `cargo test -p harness-cli --test interactive_session --locked -- --test-threads=1` | 9 passed, 0 failed |
| Gate runtime với 12 selector bắt buộc (round 15) | `pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json` | `"passed": true`, `"failures": []` |
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
user chốt nên chưa được thêm.

### 10.1. Kill process thật giữa turn (round 14)

I13 nhánh kill **process thật** nay có ca riêng:
`i13_a_hard_kill_mid_turn_leaves_one_admitted_input_and_no_claimed_success` (nằm trong
`interactive_launch`, đã thành selector bắt buộc của gate).

Cách đo: một process `ha chat --headless` thật chạy với `HA_PROVIDER_ENDPOINT` trỏ tới endpoint
loopback **accept rồi không bao giờ trả lời**; test chờ tới khi provider thấy kết nối (tức input
đã được admit và writer đã mở), rồi kill cứng process — không unwind, không flush, không đóng
writer tử tế. Sau đó state durable được đọc bằng **đúng đường read-only của operator**
(`ha sessions list --json`, `ha status --session-id ... --json`), không dựng runtime.

| Đo được | Kết quả |
|---|---|
| Input đã admit | `input_count = 1`, `next_sequence = 2` — admit đúng một lần, và session đã tiêu thụ sequence nên không thể admit lại chính input đó |
| Thành công bị claim | không: `recovery.receipt_count = 0`, `latest_snapshot_sequence = null` |
| Replay | dừng ở `replayed_through_sequence = 1` (chỉ có input) |
| Host mới | mở được writer generation mới, chạy lượt của nó trong **session mới** (`resumed_from: null`, `stop: "final"`) |
| Session bị kill sau đó | vẫn `input_count = 1`, vẫn `receipt_count = 0` — không bị "hoàn thành hộ" |
| Đối chứng dương | session hoàn thành của lượt sau có `replayed_through_sequence = 2`, `next_sequence = 3`, nên "replay dừng ở 1" ở trên thật sự nghĩa là "không ghi gì thêm" |

Kết quả lệnh: `cargo test -p harness-cli --test interactive_launch --locked -- --test-threads=1`
→ **15 passed, 0 failed** (32.85 s).

Phạm vi chính xác: ca này kill trong **lượt gọi model đầu tiên** nên không có tool nào chạy
(headless fail closed cho mọi tool call). Nó chứng minh phần *admission/lease/replay*. Phần
*receipt đã commit rồi mới mất process* được đo riêng ở **mục 15.2** bằng một ca PTY với tool
chạy thật. Không có phần nào ở đây được gọi là "kill trong lúc tool đang chạy".


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

Bổ sung sau khi sửa deadlock tiềm ẩn (round 11): input của harness nay đi qua **một writer
thread** với queue, nên luồng đọc không bao giờ giữ lock writer (trả DSR bằng queue). Kết quả
đo lại từng ca:

| Ca | Lệnh (console thật, bound) | Kết quả |
|---|---|---|
| i06 | `... -Filter i06_pty -TimeoutSeconds 240` | FAILED sau 15 s tại bước paste: `timed out waiting for "multi line"` |
| i07 | `... -Filter i07_ctrl_c -TimeoutSeconds 240` | **PTY_TIMEOUT**: harness vẫn treo, bị runner kill |

Nghĩa là deadlock lock-writer **không** phải nguyên nhân (đã sửa mà vẫn treo); nghi vấn tiếp
theo cho i07 là chỗ drop/kill session hoặc `hold.join()`, cần trace từng bước. Runner nay
lưu transcript theo tên filter (`pty-<filter>.txt`) để lần chạy sau không ghi đè bằng chứng
của lần trước (lần này transcript i06 đã bị lần chạy i07 ghi đè trước khi kịp đọc).


### 12.1. Kết quả PTY sau khi sửa (round 12)

Ba lỗi thật đã tìm ra và sửa trong test/harness (không phải trong app):

1. Chỉ một pseudo-console mỗi process trên host này: mở cái thứ hai (dù cái đầu đã đóng)
   thì BLOCK → tách i07 thành hai test một-session (i07a, i07b).
2. `ConPTY` không forward bracketed-paste markers: newline trong paste tới như Enter và phần
   đầu bị submit như một request. Không sửa được trong app; test này khẳng định điều app
   phải làm: vẫn sống và prompt còn dùng được sau paste.
3. Lỗi chuỗi input của chính test: ký tự marker còn trong buffer nên `/exit` bị nối thành
   `z/exit` và submit như REQUEST thay vì command → test này clear buffer bằng Ctrl-C trước
   khi gửi `/exit`.

| Ca | Kết quả trong console thật (transcript lưu ở `target/pty-acceptance/`) |
|---|---|
| I01 bare `ha` mở app | ok — header/prompt render, process sống, `/exit` thoát 0 |
| I06 gõ tiếng Việt + backspace + paste | ok — transcript cho thấy "> sua loi parser" từng ký tự, backspace đúng; paste trên console này submit phần đầu như một request, app vẫn sống và prompt dùng được |
| I07a Ctrl-C khi idle | ok — "> typo" → Ctrl-C → "> z" (buffer đã clear, không có "> typoz") |
| I07b Ctrl-C khi đang chạy | ok — xem 12.2 |

Hai nguyên nhân còn lại của i07b được tìm ra ở round 13 và đều nằm trong **test**, không
phải trong app:

1. Listener `accept()` chặn vô hạn nên thread test treo khi harness không kết nối; nay
   listener non-blocking với deadline, và test khẳng định cờ `contacted` (nếu provider
   không hề thấy kết nối thì đó là lỗi của provider, không phải của Ctrl-C).
2. Kỳ vọng bracketed paste của i06 vượt quá điều ConPTY trên host này làm được (nó không
   forward marker), nên khẳng định được thu về đúng thứ app phải bảo đảm: sống sót và
   prompt còn dùng được sau paste.

### 12.2. Kết quả PTY round 13: cả bốn ca xanh trong một lần chạy console thật

| Ca | Kết quả |
|---|---|
| i01 bare `ha` mở app | ok — header/prompt render, process sống, `/exit` thoát 0 |
| i06 gõ tiếng Việt + backspace + paste | ok — transcript cho thấy "> sua loi parser" từng ký tự, backspace đúng, sau paste app vẫn sống và prompt dùng được |
| i07a Ctrl-C khi idle | ok — "> typo" → Ctrl-C → "> z" (buffer đã clear, không có "> typoz") |
| i07b Ctrl-C khi đang chạy | ok — run bị cancel, app về prompt và thoát sạch |

Bằng chứng: một lần chạy bounded `pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1
-TimeoutSeconds 300` báo `PTY_EXIT: 0` và `test result: ok. 4 passed; 0 failed` (9.03 s).
Sau khi thêm ca I08 (mục 12.3) là **năm** ca (`5 passed; 0 failed`, 11.01 s), rồi thêm ca I13
(mục 15.2) thành **sáu** ca: `PTY_EXIT: 0`, `test result: ok. 6 passed; 0 failed` (12.61 s);
transcript hiện có ở
`target/pty-acceptance/pty-all.txt` là của lần chạy năm ca. Đây là bằng chứng I01/I06/I07
trong pseudo-console thật, không phải suy luận từ scripted backend.

Cả bốn ca **vẫn** `#[ignore]` vì `cargo test` trong sandbox không có console — lý do ignore
nay ghi đúng: chạy bằng `scripts/Invoke-HaPtyAcceptance.ps1`, nơi cả bốn ca đều pass. Gate
vì vậy vẫn liệt kê I06/I07 là `not_run` kèm hướng dẫn chạy, không tính là pass tự động.

### 12.3. I08: lỗi render/backend sau khi terminal đã khởi tạo (round 13)

I08 yêu cầu: inject lỗi render/backend sau terminal init, terminal modes/cursor phải được
phục hồi khi recoverable failure/unwind, và lỗi fatal **không** được nuốt. Ba lớp bằng chứng:

1. **Seam inject chỉ có ở debug build**: `CrosstermBackend::write/flush` gọi một hàm fault
   được biên dịch có điều kiện; khi `HA_TEST_FAIL_AFTER_MS` được set, backend bắt đầu trả
   `io::Error` sau mốc đó. Bản release (`cfg(not(debug_assertions))`) là no-op, nên binary
   phát hành không thể bị "ra lệnh" hỏng theo cách này.
2. **Unit test cho phần phục hồi** (`cargo test -p harness-cli --bin ha h07_i08`, 3 passed):
   guard nay tách phần điều khiển mode ra sau seam `ModeControl` để chứng minh được trong
   process test (vốn không có console để bật raw mode):
   - `h07_i08_the_guard_restores_every_mode_it_turned_on`: thứ tự `enable → paste on →
     paste off → disable`;
   - `h07_i08_an_unwinding_failure_still_restores_the_terminal`: panic sau khi vào raw mode
     vẫn phục hồi đủ bốn bước (unwind), và lỗi không bị guard nuốt;
   - `h07_i08_a_terminal_that_refuses_raw_mode_is_reported_and_not_claimed_open`: từ chối
     raw mode là lỗi trả về, không âm thầm hạ cấp và không tắt hai lần.
3. **Ca PTY thật `i08`** (console thật, bound 240 s): app boot tới prompt, mốc fault đi qua,
   gửi một phím để buộc redraw → app thoát **1** (không treo, không "thành công") và
   transcript chứa `terminal input/output failed` cùng tên seam `HA_TEST_FAIL_AFTER_MS`;
   kết quả `PTY_EXIT: 0`, `1 passed; 0 failed` (2.26 s).

Kill process cứng vẫn **ngoài phạm vi** đúng như plan ghi, và được ghi là not_run.

### 12.4. Tổng hợp đo được ở round 13

| Kiểm chứng | Lệnh | Kết quả |
|---|---|---|
| Gate đầy đủ của track | `pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json` | `"passed": true`, `"failures": []` (format, clippy `-D warnings`, 9 selector bắt buộc, unit + acceptance + P0–P7 serial, installer self test, release self test, docs) |
| Unit test binary `ha` | `cargo test -p harness-cli --bin ha --locked` | 61 passed, 0 failed (thêm 3 test I08 cho phục hồi mode) |
| Launch/headless acceptance | `cargo test -p harness-cli --test interactive_launch --locked -- --test-threads=1` | 15 passed, 0 failed, gồm ca kill process thật (mục 10.1) |
| Session/turn acceptance | `cargo test -p harness-cli --test interactive_session --locked -- --test-threads=1` | 9 passed, 0 failed |
| PTY trong console thật | `pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1 -TimeoutSeconds 480` | `PTY_EXIT: 0`, **8 passed, 0 failed** cho i01/i05/i06/i07a/i07b/i08/i12/i13 (vòng 13 có 5 ca; vòng 16 thêm i13; vòng 19 thêm i05; vòng 20 thêm i12) |
| Regression toàn CLI (serial) | `cargo test -p harness-cli --tests --locked -- --test-threads=1` | **229 passed, 0 failed, 5 ignored** (5 ca PTY; chi tiết `target/cli-tests-round13.txt`) |
| Providers | `cargo test -p harness-providers --locked` | 3 passed, 0 failed |

## 13. Chưa xác minh (không được coi là đạt)

Danh sách này chỉ còn những mục **thực sự** chưa đo. Round 21 dọn lại vì nó còn giữ ba dòng đã
lỗi thời từ round 10–13 (PTY I01/I06/I07, ca PTY I08, và hai nửa của H05 I13 — cả ba nay đã có
bằng chứng, xem mục 12.2, 12.3, 15.2 và 18).

- **Live provider**: không chạy; không có credential/budget được cấp (mục 11).
- **User PATH / cài binary thật**: không thực hiện; không được cấp quyền. Đường ghi được chứng
  minh bằng writer tiêm (mục 7), không phải bằng cách sửa registry của user.
- **Publish release / push remote**: không thực hiện; không được cấp quyền (mục 9, 11).
- **Linux**: chưa build/chạy; mọi kết quả trên là Windows.
- **I19 — VM sạch thật**: chỉ có môi trường tái tạo trong self test (mục 15.3), không có VM.

Đã đóng (trước đây nằm trong danh sách này): **I01/I06/I07/I08/I12/I13/I14 PTY** đều có ca trong
pseudo-console thật và chạy được bằng `scripts/Invoke-HaPtyAcceptance.ps1`; gate liệt kê chín ca
này là `not_run` kèm hướng dẫn vì `cargo test` trong sandbox không có console (mục 12.2, 12.3,
15.2, 15.4, 15.5, 18).

## 14. Ghi chú flake môi trường (đã điều tra, không che)

**Bổ sung round 21**: gate đỏ ở đúng **một** bước — `providers-streaming` →
`streaming::tests::g1_adapter_delivers_text_before_the_response_completes` — và đó là bước **duy
nhất** trong gate còn chạy test song song (`cargo test -p harness-providers --locked`, thiếu
`--test-threads=1`), dù chính mục này đã kết luận từ trước rằng fixture loopback phải chạy tuần
tự trong môi trường này. Đo lại trên **cùng binary**:
`cargo test -p harness-providers --locked -- --test-threads=1` → **3 passed; 0 failed** (exit 0).
Vì vậy round 21 sửa **gate**, không sửa test: bước `providers-streaming` nay truyền
`-- --test-threads=1` như mọi suite khác, kèm comment nêu lý do. Không đổi acceptance P2 và không
đổi test nào của `harness-providers`.

**Bổ sung round 20**: lần chạy gate đầu của round 20 đỏ ở `providers-streaming` →
`streaming::tests::g1_adapter_delivers_text_before_the_response_completes` (fixture loopback
trong `harness-providers`); chạy riêng `cargo test -p harness-providers --locked --lib` **3/3
xanh**. Round 20 chỉ đổi `scripts/Install-Ha.ps1`, test PTY và tài liệu — không chạm
`harness-providers`.

**Bổ sung round 16**: hai lần chạy gate liên tiếp đỏ ở hai test **khác nhau**, cả hai đều là
loại loopback/flaky đã biết và cả hai đều xanh khi chạy riêng:

- lần 1: `regression-phase_p2` → `p2_s02_provider_streams_and_deepseek_sse_adapter_are_normalized`;
- lần 2: `acceptance-launch` → `i13_resume_continues_the_task_with_recovered_context_and_no_rerun`
  (chạy riêng 3 lần liên tiếp: 3/3 xanh).

Không có thay đổi nào của round 16 chạm vào hai test đó (round 16 chỉ sửa test PTY, docs và
script gate). Gate được chạy lại cho tới khi xanh và kết quả cuối được ghi ở mục 12.4/15.

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
- **I13** (hard kill rồi `/resume`): nay đã có ca kill process thật (mục 10.1) cộng với ca
  mô phỏng receipt-đã-commit; lifecycle cancel/close có test ở `interactive_session`.

## 15. Đối chiếu acceptance I01–I20 với bằng chứng

Bảng này nói rõ mỗi mục của plan được chứng minh bằng gì và ở mức nào; "một phần" nghĩa là
phần còn thiếu được ghi đúng chứ không được tính là đạt. Sau round 16 chỉ còn **I19** (VM
sạch thật) ở mức một phần; I04/I09 đóng ở mục 15.1, I13 đóng ở mục 15.2. Round 21
đóng nốt vế "I01 trên binary đã cài" của I14 (mục 18).

| # | Bằng chứng cụ thể | Trạng thái |
|---|---|---|
| I01 | PTY `i01_bare_launch_opens_the_app_in_a_real_terminal_and_exits_cleanly` (console thật, mục 12.2) + guard `i01_chat_without_a_terminal_uses_the_same_guard` | đạt |
| I02 | `i02_help_and_version_stay_fast_paths_that_write_nothing`, `i02_existing_subcommands_keep_their_dispatch_and_output`, `i02_unknown_options_and_missing_arguments_remain_parser_errors` | đạt |
| I03 | `i03_bare_launch_without_a_terminal_exits_two_with_instructions`, `i03_headless_turn_runs_through_the_real_adapter_and_keeps_the_key_out_of_output`, `i03_headless_rejects_the_headless_only_flags_and_keeps_stdout_plain`, `i03_fixture_route_never_bypasses_the_terminal_or_headless_contract` | đạt |
| I04 | `i04_the_binary_installed_under_a_unicode_path_follows_the_caller_directory` (bản cài dưới path có dấu + spaces, hai caller directory không Git → hai store riêng, thư mục cài không thành project), `h02_project_identity_follows_the_caller_directory_with_spaces_and_unicode`, `h02_relative_cwd_resolves_from_the_caller_and_git_root_comes_from_the_project_tree` (Git root theo project tree, no-Git trả `None`) | đạt |
| I05 | `h02_empty_home_without_credentials_opens_setup_state_and_writes_nothing`, `h04_an_unconfigured_provider_is_reported_and_the_setup_state_is_kept`, header PTY i01 ("setup required") | đạt |
| I06 | PTY `i06_pty_keeps_vietnamese_input_and_paste_intact` + `h03_editor_edits_vietnamese_text_by_character`, `h03_keys_are_mapped_from_real_crossterm_events` | đạt (console thật) |
| I07 | PTY `i07a_ctrl_c_clears_an_idle_prompt`, `i07b_ctrl_c_cancels_a_running_turn`, `i05_exit_during_an_active_run_releases_the_store_for_the_next_host` (round 19: `/exit` giữa lúc run đang chạy → cancel, thoát 0, và host kế tiếp mở được writer) + `h03_ctrl_c_cancels_a_run_and_clears_an_idle_prompt` | đạt (console thật) |
| I08 | PTY `i08_a_backend_fault_after_init_restores_the_terminal_and_is_not_swallowed` + ba unit test `h07_i08_*` (mục 12.3) | đạt |
| I09 | `i09_a_corrupt_configuration_stops_the_run_with_an_actionable_error`, `i09_an_invalid_project_directory_stops_the_run_with_an_actionable_error`, `i09_a_data_root_that_cannot_be_created_names_the_path_and_writes_nothing` (round 15: data root không dùng được → exit ≠ 0, stderr nêu **đường dẫn**, giữ mã `storage_open_failed`, không tạo state), `i09_a_data_directory_without_write_permission_names_the_path_and_writes_nothing` (round 17: **ACL thật** từ chối quyền ghi của user hiện tại trên data root), `h02_corrupt_configuration_is_actionable_and_never_replaced_by_defaults` | đạt |
| I10 | `phase_p2::p2_s02_provider_streams_and_deepseek_sse_adapter_are_normalized` (SSE → sự kiện chuẩn hoá), `h03_text_and_terminal_events_are_rendered_before_the_run_ends` (text hiện trước khi run kết thúc) | đạt |
| I11 | `g2_tool_results_return_to_the_model_and_the_turn_ends_with_the_answer`, `g2_a_failed_tool_call_is_reported_instead_of_ending_the_turn`, `g2_the_tool_loop_is_bounded_and_reports_which_bound_stopped_it`, `g3_the_foundation_admits_one_input_per_session_and_says_so` | đạt |
| I12 | `h05_a_denied_gated_action_is_not_executed_and_the_model_is_told`, `h05_a_granted_gated_action_runs_once_after_the_answer`, `h05_an_expired_approval_is_a_refusal_not_a_silent_grant`, `i12_headless_turn_without_provider_configuration_fails_closed`, PTY `i12_a_prompt_with_an_unreachable_provider_is_reported_and_the_app_stays_alive` (round 20: provider được cấu hình nhưng không ai trả lời → `[run] failed` nêu URL, app vẫn sống, không có câu trả lời giả) | đạt |
| I13 | PTY `i13_a_settled_tool_receipt_survives_a_hard_kill_mid_turn` (mục 15.2: patch được duyệt chạy thật, receipt đã commit rồi mới kill cứng; tiếp tục bằng process mới → `tool_calls = 0`, file không đổi, vẫn đúng 1 receipt), `i13_a_hard_kill_mid_turn_leaves_one_admitted_input_and_no_claimed_success` (mục 10.1), `i13_resume_continues_the_task_with_recovered_context_and_no_rerun`, `i13_resuming_an_unknown_session_fails_without_running_anything`, `h05_a_settled_receipt_is_not_re_executed_after_the_process_state_is_lost` | đạt |
| I14 | `Install-Ha.ps1 -SelfTest`: `disposable_install_replaces_and_verifies_the_artifact`, `installed_digest_matches_the_built_artifact`, `install_manifest_records_version_and_digest`; PTY round 21: `i14_the_installed_artifact_opens_the_app_in_a_real_terminal` (chính I01, nhưng chạy trên **binary đã cài**, mục 18) | đạt trong destination tạm **và** console thật |
| I15 | `Install-Ha.ps1 -SelfTest`: `merge_appends_a_missing_directory`, `merge_is_case_insensitive_and_ignores_a_trailing_separator`, `merge_drops_empty_entries_and_keeps_order`, `merge_never_folds_machine_or_process_entries_into_user_path`, `injected_writer_receives_the_merged_user_path`, `self_test_never_writes_the_real_user_path` | đạt ở mức mô phỏng; **không** ghi User PATH thật |
| I16 | `i16_a_second_run_in_the_same_project_is_refused_while_the_first_holds_the_store`, `h02_two_terminals_in_one_project_share_a_store_and_the_second_is_busy` | đạt |
| I17 | `Install-Ha.ps1 -SelfTest`: `shadowing_command_is_found_before_the_owned_binary`, `shadowing_command_is_never_deleted` | đạt |
| I18 | `Install-Ha.ps1 -SelfTest`: `update_keeps_the_binary_usable`, `locked_executable_is_reported_as_in_use`, `a_failed_replacement_leaves_the_previous_binary_usable`; `New-HaRelease.ps1 -SelfTest`: `bundle_manifest_digest_mismatch`, `bundle_must_not_claim_publication`, `unexpected_file_was_not_rejected` | đạt |
| I19 | `Install-Ha.ps1 -FromBundle` + `bundle_install_uses_the_verified_executable`, `a_tampered_bundle_is_refused`; bundle `ha-0.1.0-windows-x64` kèm `checksums.txt` | **một phần**: round 17 thêm ba check chạy **artifact đã cài** trong môi trường tái tạo (`installed_binary_reports_its_version_without_a_toolchain`, `installed_binary_help_lists_the_launch_contract`, `installed_binary_guards_a_non_terminal_launch`: PATH chỉ có thư mục cài + System32, đã xoá `CARGO_HOME`/`RUSTUP_HOME`/`GIT_*`/`NODE_*`, HOME/APPDATA/HA_HOME trỏ vào thư mục tạm); **không** có VM sạch thật |
| I20 | `uninstall_removes_only_owned_files`, `uninstall_keeps_user_data`, rồi cài lại từ bundle | đạt (disposable) |

### 15.1. Hai mục "một phần" được đóng ở round 15 (I04, I09)

**I09 — data root không dùng được.** Trước round này, đường headless trả lỗi thô của store:

```text
storage_open_failed: cannot create data directory: Cannot create a file when that file already exists. (os error 183)
```

Nó **không nêu đường dẫn nào** hỏng, nên người vận hành không biết là `HA_HOME`, data dir hay
thư mục project. `crates/harness-cli/src/interactive/headless.rs` nay bọc lỗi mở store kèm
đường dẫn đã resolve, **giữ nguyên mã lỗi** (giống thông báo mà service interactive đã có):

```text
storage_open_failed: cannot open the project store at <HA_HOME>/data/projects/<key>: cannot create data directory: ...
```

Ca `i09_a_data_root_that_cannot_be_created_names_the_path_and_writes_nothing` khoá hành vi này:
`HA_HOME` là một **file** nên `<HA_HOME>/data` không thể tạo; kỳ vọng đo được: exit ≠ 0, stdout
rỗng, stderr có "cannot open the project store at" + tên data root + mã `storage_open_failed`,
file fixture vẫn là file và **không** có thư mục `data` nào được tạo cạnh nó (không fallback im
lặng sang chỗ khác).

**I04 — binary đã cài, path Unicode, không Git.**
`i04_the_binary_installed_under_a_unicode_path_follows_the_caller_directory` copy artifact ra
`bản cài đặt/` (có dấu và khoảng trắng, ngoài build tree), chạy một lượt headless thật từ hai
caller directory có dấu và không có `.git`; cả hai lượt exit 0 với response của fixture, và
`<HA_HOME>/data/projects` có **đúng hai** store — tức identity theo caller cwd, không theo chỗ
cài và không theo repo Harness; thư mục cài chỉ chứa đúng binary. Phần "Git unavailable" được
chứng minh ở unit test (`h02_relative_cwd_resolves_from_the_caller_and_git_root_comes_from_the_project_tree`
khẳng định `git_root` là `None` khi project không có Git, và header render
"not a Git repository, Git features unavailable").

Kết quả: `cargo test -p harness-cli --test interactive_launch --locked -- --test-threads=1` →
**18 passed, 0 failed** (17 ở round 15, thêm ca ACL ở round 17 — xem mục 15.3).

### 15.2. Kill đúng lúc tool receipt đã commit, trên process thật (round 16)

Ca PTY `i13_a_settled_tool_receipt_survives_a_hard_kill_mid_turn` đóng nốt nửa còn lại của I13.
Đây là ca PTY đầu tiên **không** dùng fixture nội bộ của app mà chạy một provider HTTP thật do
test điều khiển, nên tool được thực thi thật qua P3 gate và ghi receipt thật vào store:

1. Provider (test) trả về một tool call `apply_patch` với `expected_hash` của file fixture;
2. app render proposal `[approval] ApplyPatch: patch src/parser.rs`, test gửi `y`;
3. tool chạy thật (file đổi nội dung), receipt commit; app gọi model lần hai — provider **giữ
   kết nối này** thay vì trả lời, nên test biết chắc receipt đã commit trước khi kill;
4. kill cứng process (không unwind, không flush, không đóng writer tử tế);
5. đọc state bằng đường read-only của operator: `receipt_count = 1`;
6. process mới `ha chat --headless --resume <session>` chạy tiếp qua provider thật: exit 0,
   `tool_calls = 0`, file **không đổi byte nào**, và `receipt_count` vẫn đúng 1.

Kết quả: `pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1 -Filter i13` → `PTY_EXIT: 0`,
`1 passed; 0 failed` (1.72 s). Đây là mức "kill process thật sau khi receipt đã commit" chứ
không phải mô phỏng; ca mô phỏng ở `interactive_session` vẫn giữ như một đối chứng bổ sung cho
durable state (drop toàn bộ in-memory + writer generation mới).

### 15.3. Round 17: I09 bằng ACL thật, và I19 với môi trường tái tạo

**I09 — permission thật, không chỉ "root không dùng được".**
`i09_a_data_directory_without_write_permission_names_the_path_and_writes_nothing` tạo data root
rồi **từ chối quyền ghi của chính user hiện tại bằng ACL** (`icacls <dir> /deny "<user>:(OI)(CI)(W)"`).
Ca test tự kiểm chứng điều kiện tiên quyết (`fs::create_dir` trong thư mục đó phải **thất bại**),
nên nó không thể pass giả khi ACL không có tác dụng; sau đó app phải: exit ≠ 0, stdout rỗng,
stderr nêu `cannot open the project store at` + đường dẫn đã resolve + mã `storage_open_failed`,
và **không** tạo `projects/` trong root bị từ chối. Guard `DeniedWrite` gỡ ACL khi drop (kể cả
khi assert panic) và test khẳng định quyền ghi đã trở lại.

**I19 — artifact đã cài trong môi trường tái tạo.** `Install-Ha.ps1 -SelfTest` nay chạy binary
đã cài bằng một `ProcessStartInfo` với môi trường dựng lại: PATH **chỉ** gồm thư mục cài +
`System32` + `SystemRoot`, đã xoá `CARGO_HOME`, `CARGO_TARGET_DIR`, `RUSTUP_HOME`, `RUSTC`,
`RUSTFLAGS`, `GIT_*`, `NODE_PATH`, `npm_config_prefix` cùng mọi biến credential, và
`HOME`/`USERPROFILE`/`APPDATA`/`LOCALAPPDATA`/`HA_HOME` đều trỏ vào thư mục tạm disposable.
Sáu check mới (tổng self test nay **25** check):

| Check | Kỳ vọng |
|---|---|
| `installed_binary_reports_its_version_without_a_toolchain` | `--version` exit 0, in `ha <semver>`, và PATH dựng lại **không** chứa `cargo`/`rustup`/`node`/`git` (điều kiện này nằm trong chính biểu thức pass) |
| `installed_binary_help_lists_the_launch_contract` | `chat --help` exit 0 và có `--headless`, `--resume`, `--fixture` |
| `installed_binary_guards_a_non_terminal_launch` | bare `ha` không có terminal: exit **2** và stderr nêu `ha chat --headless --prompt` |
| `fresh_shell_resolves_ha_to_the_installed_binary` | `cmd.exe /c where ha` trong shell con với PATH dựng lại: exit 0 và **đúng một** dòng, bằng chính đường dẫn binary đã cài |
| `fresh_shell_runs_ha_by_name_without_a_toolchain` | `cmd.exe /c ha --version` (gọi **theo tên**, không phải absolute path): exit 0, in `ha <semver>` |
| `fresh_shell_without_the_install_directory_does_not_resolve_ha` | **đối chứng âm**: cùng shell, PATH không có thư mục cài → `where ha` thất bại, tức kết quả trên đến từ thư mục installer sở hữu chứ không từ thứ có sẵn trên máy |

Đây vẫn **không** phải VM sạch: nó chứng minh artifact đã cài chạy được mà không cần toolchain,
không cần config, không cần credential — nhưng vẫn trên chính máy này, nên I19 giữ mức "một phần".

### 15.4. Round 19: `/exit` giữa lúc run đang chạy, end-to-end

H05 mục 3 đòi `/exit` xử lý công việc đang chạy **và** trả lại quyền sở hữu terminal/store.
Controller đã có test cho phần cancel; round 19 thêm ca PTY
`i05_exit_during_an_active_run_releases_the_store_for_the_next_host`:

1. app chạy một turn tới provider loopback **accept rồi không trả lời** (turn đang thật sự chạy);
2. gửi `/exit` — **không** Ctrl-C trước;
3. transcript có `canceling the active run before exit` và app thoát **0**;
4. quyền sở hữu store được trả lại: `ha sessions list` đọc được đúng một session, và một
   **process host mới** (`chat --headless`) mở được writer, chạy turn của nó và trả về đúng
   response của fixture — tức writer lease đã được nhả chứ không kẹt.

Đây là bằng chứng end-to-end cho vế "restore store ownership" mà trước đó chỉ có ở mức
controller/unit. Cùng runner chạy **bảy** ca: `PTY_EXIT: 0`, `7 passed; 0 failed` (13.73 s,
transcript `target/pty-acceptance/pty-all.txt`).

### 15.5. Round 20: PowerShell fresh resolution và đường offline của app

**H06/H07 "PowerShell/CMD fresh command resolution"** — trước round này chỉ có CMD. Self test
nay thêm `powershell_fresh_shell_resolves_ha_to_the_installed_binary`: chạy
`pwsh -NoProfile -Command "(Get-Command ha).Source"` **trong môi trường dựng lại** (PATH chỉ có
thư mục cài + System32 + SystemRoot) và khẳng định exit 0 cùng đường dẫn resolve đúng binary đã
cài. Self test nay **26 check**.

**H07 "config missing/offline paths" — vế offline ở app tương tác.** Ca PTY
`i12_a_prompt_with_an_unreachable_provider_is_reported_and_the_app_stays_alive` bind một cổng
loopback rồi đóng nó ngay (địa chỉ chắc chắn không ai trả lời) và cấu hình app trỏ vào đó:

- gửi một prompt → transcript có `[run] failed: provider_protocol: ... error sending request for
  url (http://127.0.0.1:<port>/chat/completions)`, tức lỗi nêu **đúng URL** đã gọi;
- app **vẫn sống** ở prompt sau lỗi (không thoát, không treo);
- **không** có câu trả lời giả: transcript không chứa "fixture answer" hay "no model was called";
- `/exit` sau đó thoát **0**.

Cùng runner chạy **tám** ca: `PTY_EXIT: 0`, `8 passed; 0 failed` (19.88 s, transcript
`target/pty-acceptance/pty-all.txt`).

**Không đạt / not_run (không được tính là đạt)**: live provider smoke (không có credential),
publish release (không có channel và chưa được xác nhận push tag), build/chạy Linux (chỉ có
target `x86_64-pc-windows-msvc`: `rustup target list --installed` xác nhận), ghi User PATH thật
và cài lên máy user (không được cấp quyền), VM sạch thật cho I19.

## 16. Provenance của checkpoint hiện tại (H07 mục 4)

| Mục | Giá trị đo được |
|---|---|
| Commit code được đo | `c386416e9b529dcace078afa0c48ed8343c63353`, tree `75fa028ce8ba572c87634f1c384a711142bd3bf5` (round 21: `596e46a` thêm ca PTY `i14` + cập nhật runner/gate docs, `c386416` cho bước streaming chạy tuần tự). Round 21 **không** đổi `crates/**/src`, nên executable là bản dựng từ cùng source với round 18 |
| Executable dùng cho acceptance | `target/debug/ha.exe`, `ha 0.1.0`. Digest **phụ thuộc thư mục build** (đường dẫn nhúng trong binary), nên chỉ so trong cùng một cây: cây chính (round 18) sha256 `23fd5424b187c0ae229aa6efef9d4b6abcc3df053e00cdbb245c185e5afd6747`, 25 682 944 byte; worktree sạch `ha-verify-round20` ở `c386416` sha256 `3f5ec0a6e4bfba680e25994078cb0d53d64e28f76455ed9ce7109a88affadced`, 25 725 440 byte |
| Release candidate | `target/release-candidate/ha-0.1.0-windows-x64/` + `ha-0.1.0-windows-x64.zip`; **round 22 dựng lại ở HEAD**: `build_commit c386416`, `sha256 ha.exe = 7bfa0133c7e696e5b37fe3611afdeef8cba710257fcc0a430bd9aa91ffc6d64b`, `published: false` (candidate cũ dựng ở `1eceeef` đã bị thay; xem mục 19.6) |
| Đường dẫn đã resolve (ví dụ) | store per-project: `<HA_HOME>/data/projects/project-<hash>` (mục 10.1, 15.2); cài đặt disposable: `%TEMP%/ha-install-<guid>` (self test); bundle: `target/release-candidate/...` |
| TTY transcript | `target/pty-acceptance/pty-all.txt` — **9 ca** i01/i05/i06/i07a/i07b/i08/i12/i13/i14 (mục 12.2, 15.2, 15.4, 15.5, 18); round 22 chạy lại xanh ở **cả hai cây**: 9 passed / 0 failed trong 19.83 s (cây chính) và 19.93 s (worktree sạch `c386416`, transcript `target/verify-round22/target/pty-acceptance/pty-all.txt`) |
| OS đã chạy | Windows 11 x64; `rustup target list --installed` chỉ có `x86_64-pc-windows-msvc` |
| Test automation đã chạy | gate `Verify-HaLaunch.ps1` (`passed: true`, 13 selector, `failures: []`, **33/33 bước exit 0**) — round 21: hai lần xanh (tại chỗ + worktree `c386416`); **round 22: xác nhận lại trong worktree sạch `target/verify-round22` ở `c386416`**, kèm hai lần đỏ đã truy nguyên nguyên nhân (giới hạn sandbox ở mục 19.2 và file chưa commit của writer khác ở mục 19.3); installer self test **26** check; docs checker; PTY suite **9** ca |
| not_run | live provider smoke, publish, Linux, VM sạch thật, ghi User PATH/cài lên máy user (mục 13, 15) — round 22 không đổi danh sách này |
| Việc tiếp theo thực tế | cần user cấp credential/budget cho smoke, hoặc channel + xác nhận push tag `ha-v0.1.0` cho publish; nếu không có, track dừng ở đúng mức đã đo và không có mục nào được nâng thành "đạt" |

## 17. Round 20: xác minh trong worktree riêng (workspace bị ghi song song)

Trong round 20, **một writer khác đang sửa dở** `crates/harness-cli/src/interactive/{app,controller,service}.rs`
trong cùng workspace: `cargo fmt --all -- --check` báo diff và `cargo test` báo `E0053`/`E0308`, nên gate
**không thể** chạy xanh tại chỗ (mọi bước đỏ, kể cả `format` và `discovery`). Đó là thay đổi chưa commit
của writer kia, không phải của track này — tôi không `git add` và không sửa chúng.

Để vẫn có kết quả xác minh trung thực cho commit của mình, tôi tạo một **git worktree sạch** ở đúng
commit `2ee006d` và chạy toàn bộ phép đo ở đó:

```powershell
git worktree add --detach C:\Users\duong\Downloads\ha-verify-round20 2ee006d
cd C:\Users\duong\Downloads\ha-verify-round20
cargo fmt --all -- --check                     # exit 0
pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1 -TimeoutSeconds 480   # PTY_EXIT 0, 8 passed
pwsh -NoProfile -File scripts/Install-Ha.ps1 -SelfTest                         # 26 check OK
pwsh -NoProfile -File scripts/Verify-Docs.ps1                                  # DOCS_EXIT 0
pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json                        # passed: true, failures: []
```

| Phép đo trong worktree `2ee006d` | Kết quả |
|---|---|
| `cargo fmt --all -- --check` | exit 0 |
| PTY suite (console thật) | `PTY_EXIT: 0`, **8 passed, 0 failed** (20.43 s) |
| Installer self test | **26** check OK, `INSTALL_SELFTEST_OK` |
| Docs checker | `DOCS_EXIT: 0` |
| Gate đầy đủ | `GATE_EXIT: 0`, `passed: true`, `failures: []` |

Ghi chú flake của round 20 (đã đo trước khi cây bị sửa dở): lần chạy gate đầu đỏ ở
`providers-streaming::g1_adapter_delivers_text_before_the_response_completes`, lần thứ hai đỏ ở
`acceptance-launch` + `providers-streaming` + `regression-phase_p2`; `cargo test -p harness-providers
--locked --lib` chạy riêng **3/3 xanh**. Đây đúng loại loopback flake đã ghi ở mục 14, và worktree
sạch ở trên là lần chạy không có writer khác tranh chấp.

## 18. Round 21: artifact đã cài mở app trong console thật (I14)

Plan H06 nói thẳng: "tuyệt đối không chỉ kiểm tra absolute path `--version`", và oracle I14 đòi
"resolved executable path/digest đúng **và I01 pass trên installed executable**". Self test của
installer đã có nửa digest; nửa còn lại — chạy chính **artifact đã cài** trong terminal thật —
chưa từng được đo: mọi ca PTY trước đó chạy `target/debug/ha.exe` trong build tree.

Ca mới `i14_the_installed_artifact_opens_the_app_in_a_real_terminal`:

1. copy `target/debug/ha.exe` (đường Cargo báo) vào một thư mục cài **có dấu và khoảng trắng**
   (`<temp>/bản cài đặt`);
2. dựng lại môi trường: `PATH` chỉ còn thư mục cài + `%SystemRoot%\System32` + `%SystemRoot%`,
   và `APPDATA`/`LOCALAPPDATA`/`USERPROFILE`/`HOME`/`CARGO_HOME`/`RUSTUP_HOME` bị xoá khỏi
   process con — không toolchain, không profile của user, không state thật;
3. chạy binary đã cài trong pseudo-console thật, cwd là một project **ngoài** thư mục cài và
   không có `.git`;
4. khẳng định trên transcript: header `Harness Agents 0.1.0`, prompt tiếng Việt, dòng
   `Project: <caller project>`, `Data: <HA_HOME>/data [HA_HOME]` và
   `Store: <HA_HOME>/data/projects` — tức identity theo caller cwd và state theo `HA_HOME`,
   không theo thư mục cài;
5. `/exit` → exit **0**, terminal được trả lại (transcript kết thúc bằng xuống dòng), và thư mục
   cài sau khi chạy vẫn **chỉ có** đúng file `ha.exe`.

Điều đáng ghi lại: ConPTY tự đặt tiêu đề cửa sổ bằng đường dẫn executable, nên transcript có
chứa chuỗi `<temp>\bản cài đặt\ha.exe` — đó là **bằng chứng process được chạy đúng là bản đã
cài**, không phải build-tree binary. Vì tiêu đề luôn có đường dẫn đó, assertion "thư mục cài
không xuất hiện trong transcript" là sai; thứ được khẳng định là **state** không nằm ở đó
(dòng `Data:`/`Store:` trỏ về `HA_HOME`, và thư mục cài không sinh file mới).

Store per-project **chưa** được tạo lúc boot: app chỉ mở store khi request đầu tiên được
dispatch, nên lần đo đầu của ca này đỏ ở `only_store` (`NotFound`). Đó là hành vi thật, không
phải lỗi mới; ca được sửa để khẳng định đúng thứ boot tạo ra (đường dẫn đã resolve trong
header), còn việc store được tạo thì đã có ca headless I04/I09 và PTY i13/i05 chứng minh.

Kết quả đo (console thật, một lần chạy có bound):

```text
pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1 -TimeoutSeconds 480
PTY_EXIT: 0
test i14_the_installed_artifact_opens_the_app_in_a_real_terminal ... ok
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 20.51s
```

Suite PTY nay **chín** ca; `scripts/Verify-HaLaunch.ps1` và header của
`scripts/Invoke-HaPtyAcceptance.ps1` được cập nhật theo (vẫn là `not_run` trong gate với lý do
sandbox không có console, kèm hướng dẫn chạy).

**Transcript đo được của chính ca này** (lần đo đầu, đường dẫn tạm rút gọn bằng `<temp>`; dòng đầu
là tiêu đề cửa sổ do ConPTY tự đặt, tức bằng chứng process chạy đúng bản đã cài):

```text
]0;<temp>\bản cài đặt\ha.exe
Harness Agents 0.1.0
Project: <temp>\project with spaces    Provider: setup required
Session: new    Mode: trusted host
Git:     not a Git repository, Git features unavailable
Config:  first run, defaults (<temp>\home\config.toml) [HA_HOME]
Data:    <temp>\home\data [HA_HOME]
Store:   <temp>\home\data\projects\project-<hash>
Service: setup required (no provider configured)
Nhập yêu cầu. /help trợ giúp · /status chẩn đoán · /exit thoát
>
```

**Kết quả đo của round 21**

| Phép đo | Kết quả |
|---|---|
| `cargo fmt --all -- --check` | exit 0 (cả cây chính lẫn worktree) |
| PTY suite (console thật, bound 480 s) | `PTY_EXIT: 0`, **9 passed, 0 failed** — 20.51 s (cây chính), 20.43 s (worktree) |
| `Install-Ha.ps1 -SelfTest` | **26** check OK, `INSTALL_SELFTEST_OK` |
| `Verify-Docs.ps1` | `DOCS_EXIT: 0` |
| Gate lần 1 (cây chính) | **đỏ**: `providers-streaming`, bước duy nhất còn chạy song song — sửa ở `c386416` (mục 14) |
| Gate lần 2 (cây chính, tại chỗ) | `GATE_EXIT: 0`, `passed: true`, `failures: []` |
| Gate trong worktree sạch `c386416` | `GATE_EXIT: 0`, `passed: true`, `failures: []` — hai lần chạy liên tiếp (log `target/gate-verify-21.txt`, `target/gate-verify-21-c.txt`) |

**Xác minh trong worktree sạch.** Cây chính vẫn có thay đổi chưa commit của writer khác
(`crates/harness-cli/src/interactive/{app,controller,service}.rs` và `service_completion_tests.rs`),
nên commit của track được xác minh lại ở worktree `C:\Users\duong\Downloads\ha-verify-round20` tại
đúng `c386416` (`git status --porcelain` = **0** dòng). Bằng chứng worktree thật sự sạch: bước
`unit-interactive` báo **61** test, trong khi lần chạy tại chỗ báo **67** vì cộng thêm module chưa
commit của writer kia.

Một điều phải ghi thẳng: lần chạy gate **đầu tiên** trong worktree trả về `passed: false`, nhưng
**tên bước đỏ không được ghi lại** vì lệnh của tôi lọc output chỉ giữ dòng `"passed"`/`"failures"`.
Đó là lỗi đo của tôi, không phải bằng chứng về sản phẩm, và không được phép suy ra "bước X hỏng" từ
một lần đỏ không tên. Tally đo được của round 21: **5 lần chạy gate, 2 đỏ, 3 xanh**; lần đỏ có tên
là `providers-streaming` (đã sửa nguyên nhân), lần kia không tên và cùng loại flake loopback ở mục 14.
Sau khi sửa, ba lần chạy liên tiếp (một tại chỗ, hai trong worktree) đều `failures: []`.

## 19. Round 22: gate lại từng checkpoint tại `c386416` và đóng vế revision của artifact H08

Round này **không viết code sản phẩm**. Việc được giao là chạy lại chuỗi checkpoint theo dependency
và chỉ ghi nhận cái đo được. Ba phát hiện có giá trị nằm ở mục 19.2, 19.4 và 19.6.

### 19.1. Trạng thái đo được lúc bắt đầu

| Mục | Giá trị |
|---|---|
| HEAD | `c386416e9b529dcace078afa0c48ed8343c63353`, nhánh `master`, **ahead 4** so với `origin/master` |
| Cây chính | vẫn có thay đổi **chưa commit của writer khác**: `crates/harness-cli/src/interactive/{app,controller,service}.rs` + `service_completion_tests.rs` (untracked) |
| Tài liệu H | `docs/{evidence,handoffs,specs}/HA_LAUNCH.vi.md` cũng đang được sửa dở trong cây |

Ba lần chạy gate độc lập trong round này:

| Lần | Môi trường | Kết quả | Bước đỏ |
|---|---|---|---|
| 1 | cây chính, sandbox `workspace-write` | `passed: false` | `acceptance-launch`, `regression-phase_p3`, `regression-phase_p5` |
| 2 | cây chính, bỏ hạn chế sandbox | `passed: false` | **chỉ** `unit-interactive` |
| 3 | worktree sạch `c386416`, target riêng | **`passed: true`, `failures: []`** | — |

Log: `target/gate-round22.json.txt`, `target/gate-round22-fullaccess.txt`,
`target/gate-round22-worktree-local.txt`.

### 19.2. Ba bước đỏ ở lần 1 là **giới hạn sandbox**, không phải lỗi sản phẩm

Không suy đoán: hai phép đo trực tiếp tách nguyên nhân.

| Bước | Thông điệp thật | Nguyên nhân đo được |
|---|---|---|
| `acceptance-launch` → `i09_a_data_directory_without_write_permission_names_the_path_and_writes_nothing` | `panicked at crates\harness-cli\tests\interactive_launch.rs:1377` = assert trên `output.status.success()` của `icacls` | `icacls <dir> /deny <user>:(OI)(CI)(W)` → **exit 5**, "Access is denied": sandbox từ chối đổi ACL, nên test không dựng được tiền đề "data root bị từ chối ghi" |
| `regression-phase_p3` → `p3_c23_project_identity_requires_explicit_reassociation` | `sh.exe: *** fatal error - couldn't create signal pipe, Win32 error 5` | git gọi `C:\Program Files\Git\usr\bin\sh.exe`; named pipe bị chặn |
| `regression-phase_p5` (6 test worktree) | `git clone --local --no-hardlinks … failed: … couldn't create signal pipe, Win32 error 5` | cùng nguyên nhân trên |

Đối chứng âm để chắc chắn git không hỏng vì lý do khác: `git init` + `git commit` + `git worktree add`
trong thư mục tạm đều **exit 0**; chỉ `sh.exe` là không spawn được. Và ở lần chạy 2 (bỏ hạn chế),
đúng ba bước đó **xanh**: `acceptance-launch` 35 s, `regression-phase_p3` 43 s, `regression-phase_p5`
46 s. Vậy chúng là hệ quả của môi trường chạy, không phải regression của H01–H08.

### 19.3. Bước đỏ ở lần 2 là **file chưa commit của writer khác**

Lần 2 chỉ còn `unit-interactive` đỏ ở
`interactive::service::completion_tests::completion_service_resume_flow`. Đây không phải code của
track H:

- `git grep completion_service_resume_flow HEAD` → **không có** trong HEAD;
- `git ls-files crates/harness-cli/src/interactive/service_completion_tests.rs` → **rỗng** (untracked);
- cùng bước đó trong worktree sạch báo **61 passed, 0 failed**, còn cây chính báo **67 test** — đúng
  bằng số test của module chưa commit.

### 19.4. Checkpoint A/B/C xanh trong worktree sạch `c386416`

`git worktree add --detach target/verify-round22 c386416` → `git status --porcelain` = **0** dòng,
rồi chạy gate tại chỗ (target riêng):

```text
pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json
GATE_EXIT: 0
passed: true
failures: []
required_tests: 13
```

Cả **33 bước đều exit 0**: `format`, `clippy`, 13 discovery selector, `unit-interactive` (61 test),
`acceptance-launch` (18 test), `acceptance-session` (9 test), `providers-streaming`,
`regression-phase_p0…p7`, `installer-selftest` (26 check), `release-selftest`, `docs`.

**Một cái bẫy đo đã gặp và phải ghi lại.** Lần chạy gate đầu trong worktree đỏ ở
`regression-phase_p7::p7_install_script_installs_a_working_binary` — nhưng nguyên nhân là **cách tôi
đo**: tôi trỏ `CARGO_TARGET_DIR` về `target` của cây chính để tái dùng cache, còn test P7 lại tìm
artifact ở `target/debug` **tương đối theo repo**, nên không thấy binary. Chạy lại **không** override
(`cargo test -p harness-cli --test phase_p7 -- --test-threads=1 p7_install_script_installs_a_working_binary`)
→ **1 passed**. Đây là điều kiện dễ tái diễn: **đừng chạy gate trong worktree kèm `CARGO_TARGET_DIR`
trỏ sang cây khác**, nếu không `phase_p7` sẽ đỏ giả. Test P7 vẫn hardcode `target/debug`;
việc đó **không** bị sửa trong round này (không thuộc phạm vi được giao và đang được một track khác sở hữu).

### 19.5. PTY thật: chín ca xanh ở **cả hai** cây trong round này

| Cây | Lệnh | Kết quả |
|---|---|---|
| cây chính | `scripts/Invoke-HaPtyAcceptance.ps1 -TimeoutSeconds 480` | `PTY_EXIT: 0`, **9 passed, 0 failed**, 19.83 s — transcript `target/pty-acceptance/pty-all.txt` |
| worktree sạch `c386416` | cùng lệnh | `PTY_EXIT: 0`, **9 passed, 0 failed**, 19.93 s — transcript `target/verify-round22/target/pty-acceptance/pty-all.txt` |

Chín ca: i01, i05, i06, i07a, i07b, i08, i12, i13, i14. Vì vậy toàn bộ acceptance cần console thật
(I01/I05/I06/I07/I08/I12/I13/I14) có bằng chứng ở **đúng revision đang bàn giao**, không mượn kết quả
của round trước.

### 19.6. H08: revision của artifact **không khớp** revision đã test — đã đóng

Đây là phát hiện thật của round này. Candidate đang nằm trong cây được dựng ở commit
`1eceeefb781d25b08e62368a5b47fa708f800b3d`, trong khi HEAD là `c386416` — candidate **cũ 17 commit**,
và trong khoảng đó `crates/harness-cli/src/interactive/headless.rs` **có đổi 13 dòng**. Nghĩa là
artifact đang được coi là "release candidate" **không** phải bản dựng từ revision đã test: đúng lỗi
mà mục 8.1 của plan cấm ("đừng chỉ so mtime … để khẳng định binary đúng phiên bản").

Đã sửa bằng cách dựng lại và đóng gói lại ở HEAD (`scripts/New-HaRelease.ps1`), rồi kiểm lại manifest:

| Trường | Giá trị đo được |
|---|---|
| `build_commit` | `c386416e9b529dcace078afa0c48ed8343c63353` (**= HEAD**) |
| `version` / `target` | `ha 0.1.0` / `x86_64-pc-windows-msvc` |
| `sha256` (ha.exe) | `7bfa0133c7e696e5b37fe3611afdeef8cba710257fcc0a430bd9aa91ffc6d64b` |
| `published` | `false` |
| bundle | `target/release-candidate/ha-0.1.0-windows-x64/` (3 file: `ha.exe`, `ha.release.json`, `checksums.txt`) + `.zip` |

Độ tin cậy của digest: xoá `target/release/ha.exe` rồi dựng lại hai lần liên tiếp đều cho **đúng
cùng digest `7bfa01…`**, và `checksums.txt` khớp digest của cả `ha.exe` lẫn manifest. Không có
fixture executable hay test secret nào trong bundle (`Assert-BundleContents` chỉ cho đúng 3 tên file).

### 19.7. Đường cài end-user đầu-cuối (I19 một phần, I20) — đo trên candidate **mới**

| Bước | Lệnh | Kết quả đo được |
|---|---|---|
| Cài từ bundle đã verify vào thư mục tạm | `Install-Ha.ps1 -FromBundle target\release-candidate\ha-0.1.0-windows-x64 -Destination <temp> -NoModifyPath` | exit 0; `Installed/SHA-256` khớp `7bfa01…`; `ha.install.json` ghi `build_commit c386416`, `source` = bundle, `owned_files` = 2 file, `added_path_entry: null` |
| Không ghi User PATH | `-NoModifyPath` | installer in `PATH: … is NOT on the persisted User PATH` + hướng dẫn thủ công; `added_path_entry` để `null` |
| Gỡ cài đặt | `Install-Ha.ps1 -Uninstall -Destination <temp>` | exit 0; xoá **đúng 2 file sở hữu**; `unrelated-note.txt` (không sở hữu) **còn**; user data `config.toml` **còn**; in `PATH: no PATH entry was recorded` |
| `ha` resolve trong shell mới | `PATH` dựng lại chỉ còn `<install>;System32;SystemRoot`, đã xoá `CARGO_HOME`/`RUSTUP_HOME` | **PowerShell**: `Get-Command ha` → `<install>\ha.exe`, `ha --version` → `ha 0.1.0`; **CMD**: `where.exe ha` → cùng đường dẫn, `ha --version` → `ha 0.1.0` |
| Chứng minh "không cần toolchain" | `where.exe cargo` trong đúng PATH tối giản đó | **không tìm thấy** (exit 1) — artifact chạy được mà không có Rust |
| Guard non-TTY của chính artifact đóng gói | `<bundle>\ha.exe` với stdio bị pipe | exit **2** + hướng dẫn `ha chat --headless --prompt …`; stdout rỗng |

Đây vẫn **không** phải VM sạch: cùng máy, cùng user, chỉ dựng lại `PATH`/env của tiến trình con.
I19 giữ nguyên mức **"một phần"** cho tới khi có máy/VM thật không toolchain.

### 19.8. Việc tiếp theo của round 22

1. **I19**: vẫn cần một VM/máy sạch thật (không Rust/Git/Node, không source repo) — trong session
   này không có, và mọi kết quả ở mục 19.7 phải được đọc là **mô phỏng**.
2. **Hai input còn thiếu của H08** (mục 11 và mục 6 handoff): credential/budget cho live smoke, và
   channel + xác nhận push tag cho publish. Không có thì hai mục giữ `not_run`, không nâng thành "đạt".
3. **Quyền**: round này **không** được cấp — và không tự suy — quyền ghi User PATH thật, cài binary
   lên máy user, gọi model trả phí hay publish release. Ba việc đó vẫn chưa thực hiện.
4. **Gap code đã biết, chưa sửa**: multiline editing; `apply_patch` trên project có thư mục chỉ đọc
   (đo được `workspace_escape` trong lúc walk, chưa có test khoá hành vi); đường xoá PATH entry khi
   `-Uninstall` chưa có guard tường minh như đường cài (xem 19.9); `phase_p7` hardcode `target/debug`.

### 19.9. Hai quan sát code **chưa sửa** (ghi để không mất, không claim là đã đóng)

1. **`Install-Ha.ps1 -Uninstall` xoá PATH entry không cần cờ tường minh.** Đường **cài** chỉ ghi User
   PATH khi có `-ModifyUserPath` (mục 8.3 plan), nhưng đường **gỡ** xoá entry đã ghi mà không có cờ
   tương đương: `Invoke-Uninstall` gọi `Remove-UserPathEntry` rồi `SetEnvironmentVariable(...,'User')`
   chỉ dựa vào `manifest.added_path_entry`. Self test hiện chỉ chạy ca `AddedPathEntry = ''`, nên
   nhánh xoá thật **chưa** được phủ. Với assignment **không** cấp quyền ghi User PATH, round này
   **không** sửa; nếu sửa thì nên theo hướng yêu cầu cờ tường minh + thêm ca self test có entry thật.
2. **`walk_files` gán `WorkspaceEscape` cho lỗi quyền khi walk** (`crates/harness-tools/src/workspace.rs`):
   lỗi `ignore::Error` và `symlink_metadata` đều bị map thành `workspace_escape`, nên một thư mục chỉ
   đọc trong project sẽ báo "thoát workspace" thay vì "không đọc được". Đây đúng là hành vi đã đo ở
   mục 5 gap của handoff. **Đã đóng ở round 23** — xem mục 20.

## 20. Round 23: đóng gap `apply_patch` trên project có thư mục không đọc được

Gap này nằm trong danh sách "đã đo nhưng chưa có test khoá hành vi" từ các round trước: một project
có file ghi được nhưng **thư mục không đọc được** làm `apply_patch` thất bại với mã
`workspace_escape` trong lúc walk workspace. Mã đó sai bản chất — walk **không** thoát workspace, nó
chỉ không mở được một thư mục **bên trong** workspace — và nó khiến người vận hành đi tìm sai nguyên
nhân (path traversal) trong khi vấn đề là quyền đọc.

Làm theo đúng thứ tự RED → fix → regression.

**RED trước.** Test mới
`workspace::tests::review_unreadable_directory_is_reported_as_a_read_failure_with_the_path` dựng một
project có `visible.txt` đọc được và thư mục con `locked/` bị **ACL thật** từ chối quyền đọc
(`icacls <dir> /deny <user>:(OI)(CI)(R)`, có guard `Drop` gỡ ACL nên assertion đỏ cũng không để lại
cây tạm không xoá được). Chạy trước khi sửa:

```text
test workspace::tests::review_unreadable_directory_is_reported_as_a_read_failure_with_the_path ... FAILED
assertion `left == right` failed: a permission failure is not a workspace escape:
  workspace_escape: workspace walk failed: …\locked: IO error for operation on …\locked:
  Access is denied. (os error 5)
  left: WorkspaceEscape
 right: StorageOpenFailed
```

Đây là **đo được**, không phải suy luận: mã hiện tại là `WorkspaceEscape` và thông điệp còn **có**
đường dẫn, nên vế "nêu đường dẫn" vốn đã đúng — vế sai là **mã lỗi**.

**Fix.** `walk_files` nay phân loại lỗi qua hai helper thay vì map mọi thứ thành `WorkspaceEscape`:

| Đường lỗi | Trước | Sau |
|---|---|---|
| Walker báo lỗi và `ignore::Error::io_error()` là `PermissionDenied` | `workspace_escape` | `storage_open_failed` + `cannot read workspace directory <path>: <error>` |
| `symlink_metadata` trả `PermissionDenied` | `workspace_escape` | `storage_open_failed` + cùng dạng thông điệp |
| Lỗi walker khác (không xác định được bản chất) | `workspace_escape` | **giữ nguyên** `workspace_escape` |
| `strip_prefix` thất bại (thoát root thật) | `workspace_escape` | **giữ nguyên** — đây mới đúng là escape |

Chọn `StorageOpenFailed` vì nó là mã đã dùng cho "không mở được" ở tầng store (ví dụ đường I09
"data root không dùng được"), nên thông điệp nhất quán với phần còn lại của CLI. Đường dẫn được lấy
từ `ignore::Error::WithPath` khi có, thay vì để thông điệp mất vị trí.

**Kiểm chứng sau fix.**

| Phép đo | Lệnh | Kết quả |
|---|---|---|
| Test RED nay xanh | `cargo test -p harness-tools --lib --locked -- --test-threads=1` | **3 passed, 0 failed** |
| Regression sở hữu mã workspace | `cargo test -p harness-cli --test phase_p3 --locked -- --test-threads=1` | **20 passed, 0 failed** (39.25 s) |
| Định dạng | `cargo fmt --all -- --check` | exit 0 |
| Lint toàn workspace | `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0 |

Hai assertion `WorkspaceEscape` sẵn có trong `phase_p3` (`../outside/outside.txt` và
`symlink` escape) vẫn xanh — tức nhánh escape **không** bị nới lỏng bởi thay đổi này.

**Phạm vi còn lại:** `search_text` và `list_files` dùng chung `walk_files` nên cũng hết bị báo sai
mã; `workspace_fingerprint` cũng vậy, nghĩa là **approval gate** trên project có thư mục không đọc
được nay báo đúng bản chất.

**Gate đầy đủ trên cây đã sửa (cùng round).** Sau khi ghi tài liệu, gate được chạy lại **một lần từ
đầu tới cuối trên cây chính đã chứa fix này** (không chỉ chạy riêng các bước bị ảnh hưởng):

```text
pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json
GATE_EXIT: 0
passed: true
failures: []
steps: 30, nonzero: 0
required_tests: 13
```

Log `target/gate-round23.txt`. Các bước then chốt: `format` exit 0, `clippy` exit 0,
`unit-interactive` exit 0, `acceptance-launch` exit 0 (35 s), `acceptance-session` exit 0 (8 s),
`regression-phase_p3` exit 0 (39 s), `installer-selftest` exit 0, `docs` exit 0. PTY suite chạy lại
sau fix: `PTY_EXIT: 0`, **9 passed, 0 failed** trong 19.94 s (`target/pty-round23.txt`).

Lưu ý về con số bước: 30 bước ở lần chạy này so với 33 ở lần worktree round 22. Phần chênh lệch nằm
ở các bước `discovery:*`: `Assert-Selector` chạy `--list` cho **từng** selector nên cùng một target
được liệt kê nhiều lần. Cả hai lần đều **0 bước nonzero, `failures: []`, và đủ 13 required selector**,
nên không có bước nào bị bỏ: đây là khác biệt về cách đếm của gate, không phải coverage giảm.
Gate cũng **không** chạy `cargo test -p harness-tools` như một bước riêng, nên test RED mới được ghi
nhận bằng lần chạy riêng ở bảng trên.

## 21. Round 23 (tiếp): multiline editing và guard PATH của `-Uninstall`

Hai việc còn lại của handoff được giao tiếp trong cùng lượt này. Cả hai đều **không** nâng mục
`not_run` nào thành "đạt".

### 21.1. Multiline editing — đã implement, kèm một giới hạn console **đo được**

**Hợp đồng.** Enter vẫn **gửi** yêu cầu; **Ctrl-J** (line feed) chèn dòng mới. Buffer nhiều dòng là
**một** yêu cầu: nó vào history như một entry và chỉ được admit một lần, đúng luật "một input mỗi
session" của P1. Dòng nối tiếp **không** mang marker `> `.

| Thay đổi | File |
|---|---|
| `Key::Newline` + tài liệu lý do phải là phím console báo **tách biệt** | `src/interactive/events.rs` |
| `LineEditor` nhận `Key::Newline`: chèn `\n`; từ chối khi buffer rỗng hoặc đã kết thúc bằng `\n` | `src/interactive/input.rs` |
| `prompt_lines()` (chia theo `\n`, chỉ dòng đầu có marker) và `cursor_cell()` (row/column theo **ký tự**, cột dòng đầu tính cả marker) | `src/interactive/view.rs` |
| `TerminalBackend::move_up` + `MoveUp` của crossterm; `draw_prompt` vẽ nhiều dòng rồi đặt con trỏ đúng (row, column); `erase_prompt` xoá **mọi** row của prompt cũ trước khi vẽ lại | `src/interactive/terminal.rs`, `src/interactive/app.rs` |
| `map_key`: `Ctrl-J` → `Key::Newline` | `src/interactive/terminal.rs` |
| Hằng số/method mới cho host: `prompt_lines()`, `prompt_cursor_cell()` | `src/interactive/controller.rs` |

**Một regression do chính tôi gây ra và đã sửa trong lúc làm.** Bản `erase_prompt` đầu tiên chỉ xoá
khi `prompt_visible == true`, tức bỏ mất hành vi "luôn `clear_line` trước khi vẽ prompt đầu tiên".
Test `h03_scripted_terminal_renders_the_boot_header_and_exits_cleanly` bắt được ngay
(`backend.cleared_lines() > 0` đỏ), và `erase_prompt` nay luôn xoá ít nhất một row. Đây là lý do
gate chạy **sau** khi sửa, không phải trước.

**Giới hạn console — đo được, không suy đoán.** Trên ConPTY của máy này, gửi một line feed thô
(`\n`) tới app **không** tới dưới dạng `KeyCode::Char('j') + CONTROL`: console chuyển nó thành
**Enter** và yêu cầu bị gửi đi. Transcript của ca PTY mới ghi đúng chuỗi đó:

```text
> dòng một
[run] accepted ...93338542
[error] provider setup is incomplete: …
> dòng hai
```

Đây cùng loại giới hạn mà ca `i06` đã ghi cho bracketed paste ("console without bracketed paste").
Vì vậy ca PTY mới **không** khẳng định Ctrl-J chèn dòng: nó khẳng định cái đo được — app sống sót,
prompt còn dùng được, `/exit` thoát 0 — và in rõ nhánh nào đã xảy ra:

```text
test i21_pty_survives_a_multiline_draft_and_keeps_the_prompt_usable ... ok
i21: this console reports the line-feed key as Enter; asserting the app survives it
```

Hệ quả cho người dùng Windows: Ctrl-J **không** dùng được như phím multiline trên console này, và
cách còn lại là paste. Paste hiện vẫn đổi newline thành space (`normalize_paste`), và round này
**không** đổi hành vi đó — sửa nó là thay đổi hành vi có test riêng (`h03_editor_paste_never_submits_multiple_commands`),
nên phải làm như một mục riêng chứ không gộp vào đây. Ghi thẳng: multiline **đã có trong editor và
đã được kiểm ở tầng controller/renderer**, nhưng **chưa** chứng minh được đường phím trên Windows
ConPTY.

**Kiểm chứng multiline.**

| Phép đo | Lệnh | Kết quả |
|---|---|---|
| Toàn bộ unit test của binary (gồm 3 test mới) | `cargo test -p harness-cli --bin ha --locked` | **70 passed, 0 failed** |
| Test mới: editor | `h03_editor_accepts_multiline_input_on_the_newline_key` | ok |
| Test mới: view | `h03_a_multiline_draft_is_one_prompt_with_rows_and_a_cursor_cell` | ok |
| Test mới: render loop | `h03_a_multiline_draft_submits_once_and_erases_its_extra_rows` | ok |
| PTY thật | `Invoke-HaPtyAcceptance.ps1 -Filter i21` | `PTY_EXIT: 0`, **1 passed** |

### 21.2. `-Uninstall` chỉ gỡ PATH entry khi có cờ tường minh

Đường **cài** phải có `-ModifyUserPath` mới ghi User PATH; đường **gỡ** trước đây xoá entry đã ghi
**không** cần cờ nào. Nay có `-RemoveUserPathEntry`, và khi thiếu cờ thì installer **in ra** rằng
entry vẫn còn kèm cách gỡ, thay vì tự quyết.

Một chi tiết dễ sai đã lộ ra và được sửa: `Invoke-Uninstall` **không** đọc được switch của
script scope, vì self test gọi hàm này mà khối `param` cấp cao nhất chưa từng chạy — nên
`$RemoveUserPathEntry` luôn `false` trong self test và nhánh xoá **không thể** được chứng minh.
Quyền quyết định nay được truyền vào hàm (`-RemoveRecordedPathEntry`), call site thật truyền
`-RemoveRecordedPathEntry:$RemoveUserPathEntry`.

| Phép đo | Kết quả |
|---|---|
| `Install-Ha.ps1 -SelfTest` | **28 check OK** (trước round này: 26), `INSTALL_SELFTEST_OK`, exit 0 |
| `uninstall_keeps_the_recorded_path_entry_without_the_switch` | ok — không ghi gì (`writes=0`), entry vẫn còn |
| `uninstall_removes_the_path_entry_with_the_switch` | ok — ghi **đúng 1 lần**, giá trị gửi writer không còn entry và vẫn giữ `C:\user\a` |
| `self_test_never_writes_the_real_user_path` | ok — User PATH thật không đổi |

Cả hai ca chạy trên provider/writer **tiêm**, nên User PATH thật của máy này vẫn **không** bị đọc
hay ghi — đúng phần quyền đã ghi ở mục 0 và mục 2 của handoff.

**Sửa lại một câu ở trên cho đúng:** operator guide **có** được sửa trong round này. Kiểm tra cho
thấy `docs/OPERATOR_GUIDE.{vi,en}.md` chỉ nói về đường copy/cargo và **không** hề nhắc `-FromBundle`,
`-Uninstall` hay `-ModifyUserPath`, tức đường cài end-user của H08 **không có tài liệu vận hành** —
trong khi H07 yêu cầu docs chỉ ghi hành vi có thật. Đã thêm mục gỡ cài đặt bằng installer cho cả hai
thứ tiếng, kèm câu nói rõ `purge` **chưa** được implement (kế hoạch có nêu, code thì không) để không
ai trông vào một lệnh không tồn tại.

## 22. Round 23: flake loopback **tái phát mạnh**, đã đo và không che

Sau khi commit, gate chạy lại trên cây đã commit (`target/gate-final-committed.txt`) và **đỏ ở
`acceptance-launch`**, cụ thể là `i13_resume_continues_the_task_with_recovered_context_and_no_rerun`.
Đây **không** phải hồi quy của round 23, và điều đó được chứng minh bằng phép đo đối chứng chứ
không bằng lập luận:

| Phép đo | Kết quả |
|---|---|
| `i13_resume_continues` chạy riêng trên cây đã commit, 3 lần | **2 đỏ, 1 xanh** — đỏ mất **6.43/6.44 s**, xanh mất **0.70 s** |
| Cùng test đó trong **worktree sạch tại `c386416`** (không có thay đổi nào của round 23), 3 lần | **2 đỏ, 1 xanh** — đỏ **6.45/6.44 s**, xanh **0.65 s** |
| `phase_p2` chạy riêng sau khi gate đỏ lần hai | **17 passed, 0 failed** (1.19 s) |

Hai kết luận đo được: (1) **cùng tỉ lệ hỏng và cùng con số thời gian ở revision gốc**, nên thay đổi
của round 23 không phải nguyên nhân; (2) chế độ hỏng có **hai mức thời gian tách biệt** — ~0.7 s khi
qua và ~6.4 s khi hỏng — nghĩa là khi hỏng, client đã **chờ hết một khoảng timeout rồi mới bỏ cuộc**
chứ không bị từ chối tức thì. Thông điệp luôn cùng dạng:

```text
provider_protocol: provider_protocol: provider request failed: error sending request
for url (http://127.0.0.1:<port>/chat/completions)
```

Cơ chế đọc được từ chính code test: `sse_fixture_multi` phục vụ đúng `requests` yêu cầu và **bỏ
qua** kết nối nào đóng mà không gửi request (`if request.is_empty() { continue; }`), còn
`warm_up_loopback` chỉ chờ `TcpStream::connect` **thành công** — tức bắt tay TCP xong — chứ không
chờ server `accept()`. Giữa hai thời điểm đó, tiến trình `ha` đầu tiên có thể đã vào hàng đợi
`accept`. Warm-up mà round trước thêm vào chỉ **thu hẹp** cửa sổ chứ không đóng nó, và trên máy này
cửa sổ đó hiện thắng khoảng **2/3 lần**.

**Không sửa trong round này**, có lý do: đây là flake của **fixture trong test**, không phải hành vi
sản phẩm; mục 14 đã ghi rõ **không** được sửa acceptance P2 đã được chấp nhận, và `i13` cũng là
selector bắt buộc của gate. Sửa nó là một thay đổi test riêng — ví dụ để fixture phục vụ theo *yêu
cầu đã hoàn tất* thay vì *số kết nối*, hoặc warm-up bằng một request HTTP thật rồi trừ đi — và cần
review riêng vì nó chạm đúng thứ đang dùng làm bằng chứng. Việc làm ngay là ghi đúng trạng thái:
**gate trên máy này hiện xanh không ổn định**, mỗi lần đỏ phải được truy nguyên chứ không được tính
là đạt.

Cách xử lý đã dùng: chạy lại gate (tối đa ba lần, ghi lại từng lần) và chỉ nhận lần xanh khi
`failures: []` — không sửa test, không bỏ bước, không nâng `not_run` thành "đạt".

### 22.1. Ba lần chạy gate liên tiếp, ba bước đỏ **khác nhau**

Đây là phần quan trọng nhất của mục này, vì nó đổi cách đọc mọi con số "gate xanh" ở các mục trước.

| Lần | Bước đỏ | Chi tiết đo được |
|---|---|---|
| 1 | `providers-streaming` | `g1_adapter_delivers_text_before_the_response_completes` — `2 passed; 1 failed` trong **0.02 s** |
| 2 | `acceptance-launch` | `i13_resume_continues…` — `17 passed; 1 failed`, suite mất **203.49 s** (bình thường 38–39 s) |
| 3 | `unit-interactive` | `completion_service_resume_flow` — `69 passed; 1 failed`, suite mất **10.41 s** (bình thường 0.8 s) |

Ba bước, ba nguyên nhân biểu kiến khác nhau, và **cả ba đều xanh khi chạy riêng ngay sau đó**:

| Suite chạy riêng | Kết quả |
|---|---|
| `cargo test -p harness-cli --bin ha --locked` × 3 | **70 passed / 0 failed** cả ba lần (0.76 / 0.77 / 0.73 s) |
| `cargo test -p harness-cli --test phase_p2` | 17 passed, 0 failed (1.19 s) |
| `i13_resume_continues` riêng | có lần xanh 0.64–0.70 s |
| PTY đầy đủ 10 ca | `PTY_EXIT: 0`, 10 passed (20.18 s) |

Điều đáng chú ý về **thời gian**: mỗi lần hỏng, suite đều chậm bất thường (203 s so với 38 s;
10.4 s so với 0.8 s) — dấu hiệu chờ timeout chứ không phải sai logic. Việc này **không** phải do
round 23: `i13` hỏng **cùng tỉ lệ và cùng con số thời gian** trong worktree sạch ở `c386416`, và
round 23 không sửa `harness-providers`, không sửa fixture của `interactive_launch`, cũng không sửa
đường provider của `completion_tests`.

Kết luận phải ghi thẳng: **trên máy này, "gate xanh" là trạng thái không ổn định**, và các con số
gate ở mục 19/20/21 là **những lần chạy xanh thật** chứ không phải một bảo đảm tái lập được. Không
có bước nào bị bỏ, không test nào bị sửa, và không mục `not_run` nào được nâng — nhưng người đọc
bằng chứng cần biết rằng một lần `failures: []` trên máy này là kết quả **có xác suất**, không phải
hằng số.
