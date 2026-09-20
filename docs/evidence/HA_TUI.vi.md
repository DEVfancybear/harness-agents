# Evidence HA_TUI — TUI terminal cho `ha`

Trạng thái: **T01–T08 implemented_verified (CP-D)**. Theo template
[implementation-next/TEMPLATES.vi.md](../implementation-next/TEMPLATES.vi.md) mục 2.
Quyết định và hợp đồng ở [SPEC HA_TUI](../specs/HA_TUI.vi.md); plan ở
[HA_TUI_PLAN.vi.md](../HA_TUI_PLAN.vi.md); handoff ở
[handoffs/HA_TUI.vi.md](../handoffs/HA_TUI.vi.md).

## 1. Work items covered

| Item | Trạng thái | Bằng chứng chính |
|---|---|---|
| T01 — spike và quyết định | implemented_verified | SPEC mục 2 (a–g) + `t01_*` |
| T02 — view model, refactor controller | implemented_verified | `t02_*` (U20 byte-identical) |
| T03 — composer | implemented_verified | `t03_*`, PTY `i06`/`i21`/`t03_pty_paste_keeps_newlines` |
| T04 — history và live block | implemented_verified | `t04_*` |
| T05 — status bar | implemented_verified | `t05_*` (rảnh: 3 poll, **1** draw) |
| T06 — approval, picker, overlay | implemented_verified | `t06_*`, PTY `t06_pty_approval_y_key` |
| T07 — fallback, phục hồi, NO_COLOR | implemented_verified | `t07_*`, PTY `t07_pty_*`, `i08` |
| T08 — gate, PTY, docs | implemented_verified | gate có `required_tui_tests`; 16 ca PTY; guide vi/en mục 12 |

## 2. Tested source

- Nhánh `master`. Commit CP-A: `b0ba93f`; commit CP-D: xem handoff mục 2.
- Cây nguồn sạch khi đo (kiểm bằng `git status --porcelain`).
- Lockfile: `ratatui 0.30.2`, `ratatui-core 0.1.2`, `ratatui-crossterm 0.1.2`,
  `ratatui-widgets 0.3.2`, `unicode-width 0.2.2`, `crossterm 0.29.0` — **một bản mỗi loại**.

## 3. Môi trường

| Mục | Giá trị |
|---|---|
| OS | Windows 11 x64; console thật là Windows Terminal (`WT_SESSION` set) |
| Toolchain | `rustc 1.97.1`, `cargo 1.97.1`, `pwsh 7.6.6` |
| crossterm | `0.29.0` — một bản duy nhất (`cargo tree -p harness-cli -i crossterm`) |
| ratatui | `0.30.2` (MIT), MSRV 1.88 |
| Fixture | `FixtureService` (có nhãn) + HTTP/SSE fixture qua adapter thật |

## 4. Commands thực sự đã chạy (CP-D)

```text
cargo fmt --all -- --check                                     -> ok
cargo clippy --workspace --all-targets --locked -- -D warnings -> ok
cargo test -p harness-cli --bin ha --locked                    -> 133 passed; 0 failed
pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json        -> passed: true, failures: []
pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1 -TimeoutSeconds 900
                                                               -> PTY_EXIT: 0, "16 passed; 0 failed" (21.98 s)
pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest        -> DOCS_OK: 121 files, 15 language pairs
cargo tree -p harness-cli -i crossterm                         -> một bản 0.29.0
cargo tree -p harness-cli -i unicode-width                     -> một bản 0.2.2
```

Số test đơn vị của binary `ha`: **70 trước T01 → 133 sau T08**. Không test nào bị xoá;
test cũ đổi assertion sang `plain_lines`/`effects_to_plain` theo plan T02, và đúng **một**
test đổi hợp đồng có chủ ý (paste giữ newline, T03 — ghi ở SPEC).

## 5. Acceptance (U01–U20) — test tương ứng

| ID | Test / ca | Kết quả |
|---|---|---|
| U01 | PTY `t01_tui_opens_with_status_and_composer` | pass |
| U02 | `t03_vietnamese_text_is_measured_in_cells_after_nfc`, `t03_wrapping_places_the_cursor_on_the_right_row_and_column`, PTY `i06` | pass |
| U03 | `t03_paste_keeps_newlines_and_submits_once`, PTY `t03_pty_paste_keeps_newlines`, `i21` | pass |
| U04 | `t04_stream_text_shows_in_the_live_block_before_run_terminal`, `t04_history_order_is_user_tool_assistant_run` | pass |
| U05 | `t04_tool_card_settles_in_place_with_duration`, `t02_a_settled_tool_card_reports_the_measured_duration` | pass |
| U06 | `t05_a_running_status_reports_spinner_steps_tools_and_the_clock`, `t05_idle_poll_does_not_redraw` | pass |
| U07 | `t06_y_key_grants_exactly_the_pending_request`, `t02_expired_approval_closes_the_modal_and_never_grants`, `interactive_session::h05_*` | pass |
| U08 | `t06_picker_enter_resumes_the_highlighted_session`, `t06_esc_closes_the_picker_without_changing_the_source` | pass |
| U09 | `t06_help_overlay_is_not_written_to_history` | pass |
| U10 | `t03_tab_completes_only_a_unique_slash_command` | pass |
| U11 | PTY `t07_pty_resize_keeps_the_draft` | pass |
| U12 | `t07_the_renderer_choice_is_explainable`, `t07_plain_requested_only_honours_the_exact_value`, PTY `t07_pty_plain_flag` | pass |
| U13 | PTY `t07_pty_no_color` | pass |
| U14 | PTY `i05`, `i07a`, `i07b` trên TUI mặc định | pass |
| U15 | PTY `i08` + phục hồi terminal khi thoát | pass |
| U16 | `interactive_launch` i03 (headless, không ANSI) | pass |
| U17 | PTY `i14` (artifact staged, **không** cài lên máy user) | pass |
| U18 | `interactive_launch` i02/i04/i16 | pass |
| U19 | PTY `i13` | pass |
| U20 | `t02_plain_transcript_is_byte_identical_to_h03`, `t02_the_recorded_transcript_is_what_the_plain_renderer_printed` | pass |

## 6. Negative controls

| Bất biến | Cách phá | Test sẽ đỏ |
|---|---|---|
| Approval không bao giờ được cấp khi hết hạn | bỏ `SessionEvent::ApprovalExpired` | `t02_expired_approval_closes_the_modal_and_never_grants` |
| Plain mode không được lệch | đổi một nhãn trong `plain_lines` | `t02_plain_transcript_is_byte_identical_to_h03` |
| Tool card phải có thời lượng thật | trả `Duration::ZERO` | `t02_a_settled_tool_card_reports_the_measured_duration` |
| Rảnh thì không vẽ lại | cho `tick()` trả `Redraw` khi rảnh | `t05_idle_poll_does_not_redraw` |
| Một crossterm duy nhất | bỏ feature `crossterm_0_29` | `cargo tree -i crossterm` ra hai bản |
| Gate không tự giảm coverage | xoá một selector T | `Verify-HaLaunch.ps1 -SelfTest` đỏ |
| Không SGR màu khi `NO_COLOR` | bỏ `Theme::plain()` | PTY `t07_pty_no_color` |
| Paste không được thành nhiều lệnh | cho `normalize_paste` trả về nguyên văn có `\n` rồi submit từng dòng | `t03_paste_keeps_newlines_and_submits_once` |
| Thoát giữa run phải nhả writer | phát `Exit` ngay sau `cancel()` thay vì chờ terminal event | PTY `i05_exit_during_an_active_run_releases_the_store_for_the_next_host` |

## 7. Artifacts

| Đường dẫn | Nội dung |
|---|---|
| `target/pty-acceptance/pty-all.txt` | kết quả 16 ca PTY trong một lần chạy |
| `target/pty-transcripts/*.txt` | transcript thô của **từng** ca (đọc khi đỏ) |
| `target/verification/gate-final.json` | báo cáo JSON của gate |
| `scripts/Read-HaTranscript.mjs` | đọc transcript thành màn hình (áp escape sequence) |

`target/` nằm trong ignore rule: bằng chứng ở đây là **số đo tái lập được**, không phải
file được commit. Mọi lệnh ở mục 4 tái lập được từ commit CP-D.

## 8. Remaining limitations / not_run

- **Linux**: chưa build/chạy; phiên này chỉ có Windows x64.
- **Flake còn lại**: các fixture loopback khác trong workspace vẫn theo mẫu cũ (read có
  `expect`) và chưa gặp lại trong 3 lần chạy gate sau khi sửa; nếu tái hiện thì áp cùng
  cách sửa (read lỗi = request rỗng + readiness handshake + backoff).
- **Flake loopback dưới tải nặng: chưa hết hẳn.** Sau khi sửa ba nguyên nhân, nâng budget
  lên 10 lần thử (backoff tối đa 3,2 s) và nâng bound của hai ca có child process lên 2
  phút, vẫn còn **2/6 lần chạy gate đỏ**, với **tập ca đỏ đổi giữa các lần**
  (`providers-streaming` + `regression-phase_p2`, `acceptance-launch`,
  `unit-interactive`). Ba ca đó xanh 6/6 khi chạy riêng trên cùng binary, nên đây là
  loopback của môi trường từ chối kết nối theo từng đợt khi cả workspace chạy, không phải
  lỗi logic của test hay của app. Không nới assertion nào.
  **Đề nghị:** giữ quy tắc "chạy lại tối đa 3 lần, chỉ nhận lần `failures: []`" của plan
  mục 8, hoặc tách ba ca phụ thuộc loopback thành suite riêng — quyết định của người giao việc.
- **Paid provider smoke: chưa chạy** — quyền đã được cấp nhưng **không có credential**
  trong môi trường (`DEEPSEEK_API_KEY` và `HA_API_KEY` đều rỗng). Đây là `not_run` vì thiếu
  đầu vào, không phải thiếu quyền. **Một key là đủ**: endpoint và model mặc định theo giá
  trị `DeepSeek` công bố (`https://api.deepseek.com`, `deepseek-flash`), nên chỉ cần
  `$env:DEEPSEEK_API_KEY = '<key>'` rồi chạy `pwsh -NoProfile -File scripts/Smoke-HaProvider.ps1`.
  Đây là thay đổi hành vi của T08 (trước đó smoke đòi cả `HA_PROVIDER_ENDPOINT` và
  `HA_PROVIDER_MODEL`), có test `t08_one_deepseek_key_is_a_complete_provider_setup`.
- **Cài thật lên máy user: chưa chạy.** U17 chứng minh artifact đã staged mở được app,
  nhưng `Install-Ha.ps1` chưa ghi User PATH thật trong lượt này.
- **conhost cũ** (không phải Windows Terminal): chưa đo riêng; đã đo trên Windows Terminal
  và trên ConPTY của repo.
- **Shift+Enter**: không phân biệt được trên Windows — không hứa, không ghi vào help.
- **`scrolling-regions`**: chạy được nhưng bị loại có lý do (SPEC T01-d).
- **Flake loopback: đã sửa gốc.** Ba nguyên nhân, mỗi nguyên nhân có số đo:

  1. **Client connect trước khi accept loop chạy.** `TcpListener::bind` chỉ đưa socket vào
     listen; task accept chưa được poll. Fixture `harness-providers::streaming` không có
     handshake nào, `service_completion_tests` chỉ accept trong một nhánh `join!`, và
     fixture `interactive_launch`/`phase_p2` chỉ *tự nhận* là đã sẵn sàng. Sửa: task phát
     tín hiệu readiness, client chờ tín hiệu rồi **probe** vài lần trước khi gọi thật.
  2. **Fixture panic trên connection reset.** Một probe (connect rồi close, không gửi gì)
     làm Windows trả RST, nên `socket.read(...).expect("fixture reads")` panic **trong thread
     fixture** và listener chết — đó là lý do ca đỏ liên tục *connection refused* chứ không
     phải lỗi protocol. Đã đo: sau khi bỏ `expect` (coi read lỗi như request rỗng, accept
     tiếp), `i13_resume` hết đỏ khi chạy cả suite.
  3. **Retry quá ngắn.** Dưới tải, loopback từ chối kết nối vài trăm ms; ba lần thử cách
     nhau 50 ms không đủ. Sửa: 6 lần với backoff 100/200/400/800/1600 ms, **chỉ** retry
     đúng chữ ký `error sending request for url`; lỗi thật vẫn đỏ ngay lần đầu.

  Đo lại: gate `Verify-HaLaunch.ps1 -Json` **3/3 lần `passed: true, failures: []`** liên
  tiếp (trước khi sửa: 0/3). Ca `i13_resume` xanh khi chạy cả suite, không chỉ khi chạy riêng.

## 9. Gate

- `pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json` → `passed: true`, `failures: []`,
  `required_tests` 13 selector H + `required_tui_tests` 4 selector T.
- `pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1 -TimeoutSeconds 900` →
  `PTY_EXIT: 0`, "16 passed; 0 failed" trong **một** lần chạy.

## 10. Audit nối luồng thật sau CP-D (20/09/2026)

- Tìm thấy lỗi lifecycle thật: `/exit` khi model đang chạy phát `Exit` ngay sau `cancel()`,
  nên process có thể chết trước khi worker đóng run và nhả SQLite writer. Controller nay
  giữ cờ `exit_after_run`, chuyển sang `Canceling`, rồi chỉ phát `Exit(0)` khi nhận
  `RunTerminal` hoặc `RecoverableError`. PTY `i05` xanh **3/3** lần chạy riêng và xanh trong
  bộ 16 ca.
- Acceptance ACL nay lấy principal thật bằng `whoami` và deny quyền kế thừa trên đúng temp
  directory; không còn giả định `USERDOMAIN\\USERNAME` là identity của sandbox token.
- Installer self-test dùng cơ chế resolve PATH gốc của `cmd.exe` (`%~$PATH:I`) thay cho
  `where.exe`, vì `where` không liệt kê được temp install root trong sandbox dù `ha.exe`
  thực thi được.
- Gate wrapper phân biệt stderr tiến độ của native command với failure bằng exit code; Cargo
  ghi progress ra stderr không còn làm Windows PowerShell 5.1 dừng gate.
- Gate đầy đủ sau các sửa trên: format, Clippy, **133 unit**, **18 launch acceptance**,
  **9 session acceptance**, provider streaming, P0–P7, installer, release và docs đều xanh;
  JSON trả `passed: true`, `failures: []`.
