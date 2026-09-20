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

### 4b. Commands thật đã chạy cho ba commit sau CP-D (`1340129`, `cce5c13`, `5883d59`)

Đo ngày **20/09/2026** (commit cuối `5883d59` lúc 13:58), trên cây sạch ở đúng ba commit đó:

```text
cargo test --release -p harness-cli --bin ha --locked   -> 163 passed; 0 failed
                                                            (5 lần liên tiếp, cả 5 xanh)
cargo test -p harness-providers --locked                 -> 10 lần liên tiếp xanh
cargo clippy --workspace --all-targets --locked -- -D warnings -> sạch
cargo fmt --all -- --check                              -> sạch
pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1       -> GATE_OK: every required step passed, exit 0
ha chat --headless --prompt "say ok"                    -> trả lời của model qua binary release đã cài
```

`163 passed` và `10 lần` là **số đo có ngày**, không phải hằng số: cây đang được sửa song song
nên lần sau chạy lại phải đọc số mới, đừng trích con số này như một cam kết.

Binary đã cài lúc kết thúc lượt này (đo lại 14:05 ngày 20/09/2026):

```text
C:\Users\duong\.cargo\bin\ha.exe
SHA-256 cae17188122301a6f5e392cce8178fd4b4c042324b943661fac4100e20dd0d37
```


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

Ba test của lượt sau CP-D (`k04_*`, mục 12) **không** nằm trong U01–U20: chúng canh hành vi
được thêm sau CP-D (`/more`, phím cuộn, đuôi câu trả lời dài), nên không được tính là bằng
chứng cho acceptance nào ở bảng trên. Tên `k04_` là **cục bộ của lượt này**, không phải case K04
của plugin (`PLUGIN_ARCHITECTURE.vi.md`).

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
| `/more` phải mở ở **dòng đầu** | cho `open_overlay` nhận `scroll` từ nơi gọi, hoặc mở ở đáy | `k04_more_opens_the_recent_transcript_from_its_first_line_and_scrolls` |
| Cuộn panel không được ghi history | cho nhánh overlay của `handle_key` rơi xuống `push_history` | `k04_more_opens_the_recent_transcript_from_its_first_line_and_scrolls` (so `transcript().len()` trước/sau khi `PageDown`/`End`/`Home`) |
| Viewport không được giữ phần đầu câu trả lời dài | bỏ `flush_stream_overflow`, cho live block lớn theo câu trả lời | `k04_a_long_streamed_answer_keeps_its_end_visible_and_its_start_in_scrollback` |

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
- **Paid provider smoke: ĐÃ CHẠY, xanh** (một lượt thật trên máy này, prompt
  `Reply with the single word: ready`):

  ```text
  model:    deepseek-flash (DeepSeek default)
  endpoint: https://api.deepseek.com (DeepSeek default)
  credential: from the app's saved credential store, or none (value never printed)
  SMOKE_EXIT: 0 after 1 s
  SMOKE_RESPONSE_LENGTH: 5
  SMOKE_STOP: final
  SMOKE_RESPONSE: ready
  SMOKE_OK: one live turn completed and was recorded without leaking the credential.
  ```

  Nghĩa là: model mặc định `deepseek-flash` **có thật và trả lời**, endpoint mặc định
  đúng, và đường credential đã lưu hoạt động. Credential trong lượt này đến từ store của
  app (không biến môi trường nào được đặt), nên smoke đã được sửa để **không tự từ chối**
  khi biến môi trường trống — app là nơi quyết định, smoke chỉ chạy một turn rồi báo cáo.
  Khi không tìm thấy credential ở đâu cả, app fail-closed với `service_unavailable` và
  smoke thoát **2** kèm `SMOKE_NOT_RUN`, vẫn không thay thế fixture.
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

## 11. Lưu API key trong app (`/key`) — đo trong lượt này

Trạng thái: **implemented, unit test xanh (153 test, 0 failed trong phiên có quyền đổi ACL);
CHƯA commit, CHƯA qua gate/PTY/paid smoke.** Mục này ghi đúng những gì đã đo, đo ở đâu, và những
gì **không**. Quyết định và hợp đồng ở SPEC mục 3d. Mọi con số test ở đây là **số đo có ngày**,
không phải hằng số.

### 11.1. Work items

| Item | Trạng thái | Bằng chứng chính |
|---|---|---|
| K01 — file credential, parser, secret entry, gate setup, mask ở khung hình | implemented (unit) | 11 test `k01_*` (10 chạy được trên Windows) |
| K02 — thứ tự ưu tiên, chẩn đoán, file hỏng bị từ chối | implemented (unit) | 5 test `k02_*` |
| K03 — thư mục `private/`, ACL Windows, nhãn `Protection` | implemented, test xanh khi môi trường cho đổi ACL | 3 test `k03_*` |
| Esc huỷ secret entry | **đã sửa, test bấm phím thật** | `input.rs:773`–`781` + `controller.rs:1373`–`1385` |
| Mode `0600` áp lúc **tạo** file (Unix) | **đã sửa, có test** | `k01_the_stage_file_is_created_with_restrictive_flags`, `k01_the_file_is_owner_only_on_unix` |
| `/key <value>` được mô tả trong app | **đã sửa** | `/help` `view.rs:131`–`133`; notice `controller.rs:753`–`754` |
| ACL Windows thật (account + SYSTEM) | **đã áp + đã đo** trên `%LOCALAPPDATA%\HarnessAgents` | 11.2; test `k03_a_saved_key_is_restricted_to_this_account_by_an_acl` |
| Nhãn `Protection` + dòng `Provider: credential file protection: …` trong `/status` | implemented; **nhãn** có test, **dòng đã render** thì chưa | `service.rs:335`–`338`; `k03_the_protection_labels_say_they_grant`; khoảng trống ở 11.7 |
| Mask không lên màn hình | assert ở **3 tầng**: editor, controller, khung hình đã vẽ | 3 test ở 11.5 |
| `/key` chạy trong console thật | **not_run** | không có ca PTY nào cho `/key` |
| Một lượt gọi provider thật bằng key lưu trong app | **not_run** | môi trường không có credential/budget |

### 11.2. Tested source, phép đo ACL, và một khác biệt môi trường phải nói rõ

`HEAD` khi đo là `aeaf7b7` (`feat(ha-tui): /model and /status say which provider settings are
actually in use`). Feature `/key` **chưa có commit nào tại thời điểm đo**;
`crates/harness-cli/src/interactive/credentials.rs` khi đó còn **untracked**. Cây đã **ngừng đổi**
trong lượt đo; hash dưới đây là bản cuối của lượt đó.

> **Cập nhật 20/09/2026 (sau khi đo):** `/key` đã được commit ở `385a98f`, `credentials.rs` đã
> được track, và các lượt sau nối tiếp tới `213884a` (= `origin/master` lúc ghi). Bảng dưới giữ
> nguyên như **bản ghi của lượt đo**, không phải trạng thái hiện tại; số test hiện tại ở mục 4b/12.

| | Giá trị |
|---|---|
| sha256 (16 ký tự đầu, đo lại 10:51 ngày 20/09/2026 sau khi cây ngừng đổi) | `credentials.rs 247DD32D6DDBDFF8`, `service.rs 30DC718E5FBF91AA`, `controller.rs 40568110E2005CB2`, `bootstrap.rs B2FA70F70BFB3151`, `input.rs F11C299E64170B34`, `view.rs F5B31462FED96083`, `events.rs 5A84C0BC7E302E6E`, `headless.rs AA4BFE1DBBE0EE27`, `tui/mod.rs 7CD12D2953E36362` |
| `git status --porcelain` | 8 file source dưới `interactive/` modified + `credentials.rs` untracked (cùng file docs/scripts của track trước) |
| Unit test | **153 passed; 0 failed** — đo lúc 10:49 ngày 20/09/2026 bằng `cargo test --release -p harness-cli --bin ha --locked` trong phiên có quyền đổi ACL (bên giao việc; 3 lần liên tiếp cho riêng test ACL đều `1 passed; 0 failed`); `cargo check --release -p harness-cli --all-targets --locked` **không warning** |
| Tổng số test | **153**, đo lúc 10:49 ngày 20/09/2026 bằng `cargo test -p harness-cli --bin ha --locked -- --list`; **18** tên khớp `k0[123]_` trên Windows (10 `k01_*` + 5 `k02_*` + 3 `k03_*`), cộng 1 test `#[cfg(unix)]` = 19 test của feature |
| Test trong phiên soạn tài liệu này | **152 passed; 1 failed** (đo 10:49 ngày 20/09/2026, debug và release đều vậy) — cùng bản cây, khác quyền file (xem dưới); test đỏ đúng là `k03_a_saved_key_is_restricted_to_this_account_by_an_acl` |

**Phép đo ACL trước/sau (giá trị của feature này), trên `%LOCALAPPDATA%\HarnessAgents`:**

```text
TRƯỚC  icacls <thư mục cha của đường dẫn credential>
       ... DESKTOP-14QHC6K\CodexSandboxUsers:(I)(OI)(CI)(RX)     <- nhóm KHÔNG phải user đọc được
SAU    icacls <thư mục credential của một key đã lưu>
       NT AUTHORITY\SYSTEM:(OI)(CI)(F)
       DESKTOP-14QHC6K\duong:(OI)(CI)(F)
       (file credential thừa hưởng đúng hai mục này, thêm cờ (I))
```

Nghĩa là: sau khi `/key` lưu, quyền kế thừa của profile bị cắt (`/inheritance:r`) và chỉ còn tài
khoản đang dùng cộng `SYSTEM`; app vẫn đọc và xoá được thứ nó vừa siết, `load()` vẫn trả key.

**Khác biệt môi trường — đây là chỗ dễ báo sai nhất.** Cùng bản cây đó, trong **phiên soạn tài
liệu này** (file policy `workspace-write`), bước đổi ACL **bị hệ điều hành/sandbox từ chối**:

```text
icacls <dir> /inheritance:r   -> exit 5, "Access is denied"     (đo trong phiên này, 2 lần, 2 thư mục khác nhau)
k03_a_saved_key_is_restricted_to_this_account_by_an_acl
  assertion `left == right` failed: the ACL step must report what it did
    left: ProfileDefault
   right: OwnerOnlyAcl
```

Đây **không** phải lỗi logic: `restrict_acl` trả `Protection::ProfileDefault` khi `icacls` fail,
và `describe()` của nhãn đó nói thẳng *"no owner-only permission could be applied, so another
account on this machine may be able to read the file"*. Tức trong môi trường bị siết, app **báo
sự thật** thay vì hứa suông — đó là hành vi đúng và là lý do có bốn nhãn `Protection`. Hệ quả cần
biết: **test này phụ thuộc quyền của môi trường chạy** — runner không được đổi ACL sẽ thấy nó đỏ
(evidence 11.7 ghi là khoảng trống còn lại, không phải bằng chứng code sai).

*Ghi chú lịch sử (ngắn, để đọc log cũ):* trong lượt này cây từng có hai bản trung gian — bản A
(`149 passed; 0 failed`, chưa có `private/`/`Protection`) và bản B1 (`148 passed; 2 failed`, hai
test cũ còn assert đường dẫn cũ). Cả hai đã bị bản cuối ở trên thay thế; đừng dùng số của chúng.

### 11.3. Commands thật đã chạy

```text
# Phiên bên giao việc (có quyền đổi ACL) — số của bản cuối, đo 20/09/2026:
cargo test --release -p harness-cli --bin ha --locked  -> 153 passed; 0 failed
cargo test --release -p harness-cli --bin ha --locked k03_a_saved_key  -> ok, 1 passed; 0 failed (3 lần)
cargo check --release -p harness-cli --all-targets --locked            -> không warning

# Phiên soạn tài liệu này (file policy workspace-write) — cùng bản cây, đo 10:49 ngày 20/09/2026:
cargo test -p harness-cli --bin ha --locked            -> 152 passed; 1 failed
cargo test --release -p harness-cli --bin ha --locked  -> 152 passed; 1 failed
                                                          (cùng một test: ProfileDefault != OwnerOnlyAcl)
cargo test -p harness-cli --bin ha --locked -- --list  -> 153 test; 18 tên khớp k0[123]_ trên Windows
                                                          (10 k01 + 5 k02 + 3 k03; k01_the_file_is_owner_only_on_unix KHÔNG có — cfg(unix))
cargo build --release -p harness-cli --bin ha --locked -> dùng cho phép đo end-to-end ở 11.4
pwsh -NoProfile -File scripts/Verify-Docs.ps1          -> DOCS_OK: 121 files, 15 language pairs
pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest -> DOCS_OK + NEGATIVE_CONTROL_OK
cargo clippy --workspace --all-targets --locked -- -D warnings         -> CHƯA chạy
scripts/Verify-HaLaunch.ps1 -Json / Invoke-HaPtyAcceptance.ps1        -> CHƯA chạy
scripts/Smoke-HaProvider.ps1                                          -> CHƯA chạy
```

Bằng chứng phụ cho nhánh `ProfileDefault` (chạy tay trong phiên này, không phải suy đoán):

```text
USERDOMAIN='DESKTOP-14QHC6K'  USERNAME='duong'      (account resolve được, không phải lý do fail)
icacls <scratch dir>                    -> exit 0 (đọc được ACL)
icacls <scratch dir> /inheritance:r     -> exit 5, "Access is denied"   (2 thư mục khác nhau, 2 lần)
```

### 11.4. End-to-end: CLI thật đọc **file** credential ở `private/` rồi đi tới lời gọi provider

Đây là phép đo chạy binary release thật (không phải unit test), chạy lại được, và **không** dùng
`HA_CREDENTIALS_DIR` — nó kiểm luôn đường dẫn mặc định mới `<data dir>/private/`:

```powershell
# KHÔNG set DEEPSEEK_API_KEY / HA_API_KEY trong shell này
$root = "<repo>\target\e2e-cred-check2"
#   $root\home\data\private\credentials.env  chứa đúng: DEEPSEEK_API_KEY="sk-e2e-file-only"
$env:HA_HOME              = "$root\home"          # KHÔNG set HA_CREDENTIALS_DIR
$env:HA_PROVIDER_ENDPOINT = 'http://127.0.0.1:9/chat/completions'   # cổng chết, không có gì lắng nghe
& "<repo>\target\release\ha.exe" chat --headless --prompt hello --cwd "$root\project" --json
```

Kết quả đo được (phiên soạn tài liệu này):

```text
file written at: <...>\target\e2e-cred-check2\home\data\private\credentials.env
key env present: False/False
EXIT: 1
provider_protocol: provider_protocol: provider_protocol: provider request failed: error sending request for url (http://127.0.0.1:9/chat/completions)
key value in output: False
```

Cây tạm sau lượt chạy (chứng minh store đã mở và lượt đã được nhận trước khi request thất bại):

```text
home\data\private\credentials.env
home\data\projects\project-994371608010cd34\harness.sqlite3
home\data\projects\project-994371608010cd34\harness.sqlite3-shm
home\data\projects\project-994371608010cd34\harness.sqlite3-wal
home\data\projects\project-994371608010cd34\writer.lock
```

Điều phép đo này **chứng minh**:

1. Key **chỉ** đến từ file: cả `DEEPSEEK_API_KEY` lẫn `HA_API_KEY` đều vắng trong môi trường.
2. Đường dẫn mặc định **`<data dir>/private/credentials.env`** là đúng: app tìm thấy key ở đó mà
   không cần `HA_CREDENTIALS_DIR`.
3. Đường CLI thật (`ha chat --headless`) đọc được file đó: nếu không, nó dừng ở
   `no credential; set one of ...` (mã `SecretNotGranted`) chứ không đi tiếp.
4. App đã mở store, nhận lượt và **đi tới lời gọi provider** (xem cây tạm ở trên).
5. **Không có request nào rời máy**: endpoint là cổng loopback chết `127.0.0.1:9`.
6. Giá trị key **không** xuất hiện trong output (`Contains('sk-e2e-file-only')` → `False`).

Điều phép đo này **không** chứng minh: không có lời gọi DeepSeek thật nào (endpoint chết, không
credential thật), nên **không** nói gì về việc key có xác thực được hay không. Nó cũng không đo
ACL: thư mục `private/` ở đây nằm trong `target\` của workspace.

### 11.5. Test nào chứng minh điều gì

| Test | Điều được chứng minh | Oracle |
|---|---|---|
| `k01_only_the_known_variable_is_accepted` | tên biến khác bị từ chối | `load` trả `ConfigParseError`; chuỗi lỗi chứa `does not use`, **không** chứa giá trị fixture |
| `k01_the_parser_detail_never_quotes_the_value` | giá trị không quote bị từ chối, thông điệp có path và `/key` | chuỗi lỗi không chứa `sk-secret-without-quotes` |
| `k01_missing_and_blank_files_are_absent_not_errors` | file thiếu và giá trị rỗng là `None` | `load` trả `Ok(None)` cho cả hai |
| `k01_round_trip_keeps_the_key_and_leaves_no_staging_file` | `save` → `load` giữ nguyên key; không còn file `*staged*` | quét `read_dir` thư mục đích |
| `k01_a_quote_in_the_key_survives_a_round_trip` | key chứa `"` sống qua escape/unescape | `save`/`load` với `sk-with"quote` |
| `k01_the_file_is_owner_only_on_unix` (`#[cfg(unix)]`) | mode file `0600` **và** mode thư mục `0700` sau `save` | `metadata().permissions().mode() & 0o777` cho cả hai — **không biên dịch trên Windows** |
| `k01_the_stage_file_is_created_with_restrictive_flags` | file staging được **tạo** với mode `0600` và nội dung đúng một dòng | gọi thẳng `write_staged`, đọc mode (assertion Unix trong test chạy mọi nền tảng) |
| `k01_key_entry_masks_saves_clears_the_gate_and_admits_the_next_message` | cả chuỗi `/key` bằng **phím thật**: prompt mask; file ghi đúng `DEEPSEEK_API_KEY="sk-controller-fixture"\n`; gate setup xoá; phase `Ready`; notice trong transcript; key không có trong transcript và không tạo submission; lượt kế tiếp được nhận thật; `/key` bị từ chối khi run đang chạy; Esc sau đó **không** ghi đè key đã lưu | `controller.editor.secret_entry()`, `controller.prompt()`, `ui_state().setup_required`, `phase()`, `transcript()`, submission log của port, đọc lại file credential |
| `k01_a_secret_buffer_is_painted_as_a_mask` (TUI) | **khung hình đã vẽ** không chứa `sk-live-secret` và **có** chứa `•` khi `state.buffer` là buffer đã mask | vẽ bằng `ScriptedRenderer::draw_state(80×24)` rồi soi `painted()`; một renderer lấy buffer thô sẽ trượt |
| `k02_the_credential_file_follows_the_explicit_directory` | `HA_CREDENTIALS_DIR` thắng data dir; giá trị rỗng không phải override | so `resolve_file` với path mong đợi |
| `k02_a_source_is_described_by_name_never_by_value` | `describe()` chỉ trả tên nguồn; `is_live()` env `false` / file `true` | assert trên `CredentialSource` |
| `k01_saving_a_key_clears_the_setup_gate_and_writes_the_minimal_config` | `config.toml` = `"schema_version = 1\n"`; `setup_required` false; header nêu `credentials.env` và **không** vẽ key | `bootstrap::credential_saved` + đọc lại file config |
| `k01_secret_entry_masks_the_buffer_and_never_reaches_history` | mask một-ký-tự-một-mask; Enter trả `InputOutcome::Secret`; history rỗng; **`Key::Esc` thật** trả `Redraw`, thoát secret entry và không để lại gì | `display_buffer()`, `handle(Key::Enter)`, `handle(Key::Esc)`, `history()` |
| `k02_a_saved_file_configures_the_provider_and_the_environment_still_wins` | file là nguồn credential hợp lệ; biến môi trường thắng file | `resolve_provider` với/không có `DEEPSEEK_API_KEY` |
| `k02_a_saved_file_is_readable_and_a_corrupt_one_is_refused` | file lưu đọc được; file hỏng **dừng** launch kèm path, không lặp nội dung | `validate_credential_file` |
| `k02_diagnostics_name_the_source_and_never_the_value` | `/status` nêu `credentials.env` + `value hidden`, không có key. **Không** phủ dòng protection mới (`service.rs:337`) | `provider_diagnostics` join |

### 11.6. Negative controls

| Bất biến | Cách phá | Test sẽ đỏ |
|---|---|---|
| Chỉ nhận đúng một tên biến | cho parser nhận mọi `NAME=` | `k01_only_the_known_variable_is_accepted` |
| Thông điệp lỗi không rò giá trị | nhét giá trị bị từ chối vào `detail` | `k01_the_parser_detail_never_quotes_the_value`, `k02_a_saved_file_is_readable_and_a_corrupt_one_is_refused` |
| Biến môi trường thắng file | đảo thứ tự trong `credentials::source` (stat file trước) | `k02_a_saved_file_configures_the_provider_and_the_environment_still_wins` |
| Key không vào history | cho `LineEditor::submit` chạy nhánh history trước nhánh `secret` | `k01_secret_entry_masks_the_buffer_and_never_reaches_history` |
| `/key` là setup trọn vẹn | bỏ `write_minimal_config` khỏi `credential_saved` | `k01_saving_a_key_clears_the_setup_gate_and_writes_the_minimal_config` |
| Không rò key qua chẩn đoán | cho `describe()` trả cả giá trị | `k02_diagnostics_name_the_source_and_never_the_value` |
| Esc phải huỷ secret entry | bỏ nhánh `self.secret` khỏi `Key::Esc` | `k01_secret_entry_masks_the_buffer_and_never_reaches_history` (đơn vị) + `k01_key_entry_masks_saves_clears_the_gate_and_admits_the_next_message` (controller) |
| Không có cửa sổ file ai cũng mở được (Unix) | đổi `write_staged` sang `fs::write` + `set_permissions` sau | `k01_the_stage_file_is_created_with_restrictive_flags` (chỉ đỏ trên Unix) |
| Esc không được ghi đè key đã lưu | cho `Key::Esc` gọi `save_key` với buffer | `k01_key_entry_masks_saves_clears_the_gate_and_admits_the_next_message` |
| Renderer không được lách qua state để lấy buffer thô | cho composer vẽ `editor.buffer()` thay vì `state.buffer` | `k01_a_secret_buffer_is_painted_as_a_mask` |
| ACL phải nêu đúng account + SYSTEM | bỏ `/grant:r "SYSTEM:..."` khỏi `restrict_acl` | `k03_a_saved_key_is_restricted_to_this_account_by_an_acl` (chỉ chạy/đỏ ở môi trường cho đổi ACL) |
| Nhãn `Protection` không được nói quá | cho `ProfileDefault.describe()` trả "owner-only" | `k03_the_protection_labels_say_they_grant` |

### 11.7. **Không** được chứng minh (đọc kỹ trước khi báo cáo)

- **Chưa có lượt gọi DeepSeek thật nào.** Môi trường này **không** có credential/budget
  (`DEEPSEEK_API_KEY` và `HA_API_KEY` đều rỗng), nên **không** có bằng chứng rằng một key lưu
  bằng `/key` thật sự xác thực được với provider. Toàn bộ chuỗi "lưu → lượt kế tiếp dùng" chỉ
  được chứng minh ở mức unit: `resolve_provider` đọc lại nguồn, `credentials::load` đọc lại
  file — **không** có request HTTP nào được gửi.
- **TUI chưa được lái trong console thật ở lượt này.** ConPTY cần một console mà sandbox build
  không có, nên **không** có ca PTY nào cho `/key`: không có bằng chứng transcript thật rằng ký tự
  `•` là thứ terminal nhận được. Mask được canh ở **ba tầng không cần console** — editor
  (`display_buffer()`), controller (`prompt()`/`transcript()`), và **khung hình đã vẽ**
  (`k01_a_secret_buffer_is_painted_as_a_mask`) — nhưng đó là `ScriptedRenderer`, không phải ConPTY.
- **ACL Windows: đo trên một máy, và phụ thuộc quyền của môi trường chạy.** Phép đo trước/sau ở
  11.2 là trên `%LOCALAPPDATA%\HarnessAgents` của **máy này**, trong phiên **có** quyền đổi ACL;
  test `k03_a_saved_key_is_restricted_to_this_account_by_an_acl` cũng xanh ở đó. Trong phiên soạn
  tài liệu này (policy `workspace-write`) `icacls /inheritance:r` bị từ chối (exit 5) nên `save()`
  trả `ProfileDefault` và **cùng test đó đỏ** — nghĩa là test **phụ thuộc quyền của runner**, và
  một CI bị siết ACL sẽ thấy nó đỏ. Thêm nữa: assertion trong test dựa vào chữ **`SYSTEM`** theo
  tiếng Anh (chính test ghi rõ điều này), nên **chưa** kiểm chứng cho Windows bản địa hoá khác.
- **Nhánh `icacls` thất bại không có test ép.** "Fallback trung thực" (`ProfileDefault` khi
  `icacls` fail) được chứng minh bằng phép đo thủ công trong môi trường bị chặn ACL, **không** bằng
  một test tiêm lỗi.
- **Dòng protection trong `/status` chưa được test assert.** `provider_diagnostics` in
  `Provider: credential file protection: <mô tả>` (`service.rs:335`–`338`), và **nhãn** thì có test
  (`k03_the_protection_labels_say_they_grant`), nhưng **không** test nào assert chuỗi đã render —
  nên một thay đổi ở dòng đó sẽ không làm test nào đỏ.
- **Hai assertion Unix `0600`/`0700` không chạy trên máy này.** `k01_the_file_is_owner_only_on_unix`
  là `#[cfg(unix)]` (không biên dịch, không chạy trên Windows), và assertion mode bên trong
  `k01_the_stage_file_is_created_with_restrictive_flags` cũng nằm trong khối `#[cfg(unix)]` — trên
  Windows test đó chỉ khẳng định nội dung file staging. Vậy "0600 lúc tạo" được canh bằng test
  nhưng **chưa từng được đo** trong lượt này.
- **`/key <giá-trị>` chỉ lưu token đầu tiên.** Hạn chế này **đã được ghi trong app** (`/help`
  `view.rs:131`–`133` và notice `controller.rs:753`–`754`), nhưng vẫn là hạn chế: key chứa dấu
  cách sẽ bị cắt.
- Chưa commit, chưa gate, chưa PTY, chưa cài lên máy user trong lượt này; không có phép đo nào
  trên VM sạch.

Đã sửa trong lượt này (trước đây nằm trong danh sách "không đạt"): **Esc huỷ secret entry** (implement +
test bấm phím thật ở editor và controller); **mode `0600` áp lúc tạo file staging** (Unix) thay cho
`set_permissions` sau khi ghi; **`/key <value>` được mô tả trong `/help`**; **đường `/key` của
controller có test end-to-end**; **file credential nằm trong `<data dir>/private/`**; **ACL Windows
thật (account + SYSTEM) và `Protection` được `save()` trả về, `/status` in ra**; và **mask được
assert ở cả tầng khung hình đã vẽ**.

## 12. Đọc câu trả lời dài trong app — `/more`, phím cuộn, con lăn chuột, hai flake loopback

Trạng thái: **implemented + committed + pushed** (`1340129`, `cce5c13`, `5883d59`, tất cả trên
`origin/master`; `HEAD` = `origin/master` = `5883d59` tại thời điểm soạn mục này). Hợp đồng và
quyết định ở SPEC mục 3e. Mọi con số ở đây là **số đo có ngày 20/09/2026**, không phải hằng số —
cây nguồn đang được một writer khác sửa song song, nên lần sau phải chạy lại lệnh chứ không trích
lại con số.

### 12.1. Work items

| Item | Trạng thái | Bằng chứng chính |
|---|---|---|
| `/more` mở lại transcript gần nhất, **từ dòng đầu** | implemented + test | `k04_more_opens_the_recent_transcript_from_its_first_line_and_scrolls`; `view.rs:135`; `SLASH_COMMANDS` 9 phần tử (`input.rs:670`) |
| Buffer hồi tưởng có chặn **500 dòng** | implemented + test (qua `/more`) | `RECALL_LINES = 500` (`controller.rs:1044`); ghi ở ba điểm `flush_stream`/`flush_stream_overflow`/`push_history` |
| Panel cuộn được: `PgUp`/`PgDn` 8 dòng, `Home` đầu, `End` cuối | implemented + test | `controller.rs:331`–`337`, `input.rs:73`–`93`, kẹp ở `help.rs:29`–`31`; `k04_a_long_overlay_is_readable_from_its_first_row` |
| Viền dưới báo `còn N dòng` / `dòng x/y` / `cuối` + `Esc đóng` | implemented, **không có test assert chuỗi** | `help.rs:33`–`45` |
| Cuộn panel **không** ghi history; `Esc` đóng (U09) | implemented + test | `k04_more_opens_...` so `transcript().len()` trước/sau |
| `/more` trong plain mode in dòng như mọi lệnh tham chiếu | implemented (dùng chung `reference()`) | `controller.rs:875`–`883` |
| Con lăn chuột = alternate scroll `DECSET 1007`, bật/tắt đúng chỗ | implemented + test I08 | `terminal.rs:152`–`158`, `202`, `218`, panic hook `terminal.rs:181`; thứ tự mode assert đầy đủ |
| **Không** dùng mouse capture (`1000`/`1002`/`1006`) | quyết định có chủ ý | SPEC 3e.3 — capture lấy con lăn **và** xoá scrollback |
| Flake SSE fixture của `providers-streaming` | **đã sửa** | chunked body + chunk cuối (`streaming.rs:381`–`396`), giữ socket 500 ms (`streaming.rs:441`–`453`) |
| Flake `completion_service_resume_flow` | **đã sửa** | con chạy tối đa 2 lần, thất bại vẫn báo (`service_completion_tests.rs:20`–`43`) |
| TUI/`/more`/phím cuộn/con lăn trong **console thật** | **not_run** | không có ca PTY nào cho `/more` hay phím cuộn; 1007 là hành vi phía terminal |
| Chuỗi gợi ý viền dưới có `↑↓` nhưng `↑`/`↓` **không** cuộn panel | **mâu thuẫn đã đo, chưa sửa** | `help.rs:36`/`38`/`41` vs `controller.rs:331`–`337` (xem 12.4) |

### 12.2. Số đo

```text
cargo test --release -p harness-cli --bin ha --locked      -> 163 passed; 0 failed
                                                              (5 lần liên tiếp, cả 5 xanh; đo 20/09/2026)
cargo test -p harness-providers --locked                   -> 10 lần liên tiếp xanh
cargo clippy --workspace --all-targets --locked -- -D warnings -> sạch
cargo fmt --all -- --check                                 -> sạch
pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1          -> GATE_OK: every required step passed (exit 0)
ha chat --headless --prompt "say ok"                       -> model trả lời qua binary release đã cài
```

Hai phép đo "trước/sau" của flake (đây là **số đo**, không phải suy đoán):

| Flake | Trước | Sau |
|---|---|---|
| `providers-streaming` (SSE fixture) | khoảng **1 lần đỏ trong 8 lần chạy** | **0 đỏ trong 10 lần liên tiếp** |
| `completion_service_resume_flow` | khoảng **1 lần đỏ trong 3 lần chạy** cả suite | **0 đỏ trong 5 lần liên tiếp** chạy cả suite |

Binary đã cài, đo lại 14:05 ngày 20/09/2026: `C:\Users\duong\.cargo\bin\ha.exe`, SHA-256
`cae17188122301a6f5e392cce8178fd4b4c042324b943661fac4100e20dd0d37`, 20.235.264 byte.

### 12.3. Test nào chứng minh điều gì

| Test | Điều được chứng minh | Oracle |
|---|---|---|
| `k04_more_opens_the_recent_transcript_from_its_first_line_and_scrolls` | `/more` mở **panel** (không phải history) ở `scroll == 0`; panel chứa cả `answer line 0` lẫn `answer line 19` của câu trả lời 20 dòng; `PageDown` → `scroll == 8`; `End` → `scroll > 8`; `Home` → `scroll == 0`; `transcript().len()` **không đổi** qua mọi lần cuộn; `Esc` đóng panel | `ui_state().modal` phải là `Modal::Overlay { title, lines, scroll }`; số dòng transcript trước/sau |
| `k04_a_long_overlay_is_readable_from_its_first_row` | overlay 40 dòng mở ở 0; cuộn lên quá đỉnh **bão hoà** ở 0 (không quấn vòng); `End` xin đáy (`scroll > 30`); cuộn khi **không** có overlay trả `false` | `Overlay::scroll()` trực tiếp trên editor |
| `k04_a_long_streamed_answer_keeps_its_end_visible_and_its_start_in_scrollback` | đuôi câu trả lời (`THE-LAST-LINE-OF-THE-ANSWER`) nằm trong `live_text`; đầu (`line 0 of the answer`) **không** còn trong live block nhưng **có** trong `transcript()`; một delta 12 dòng ("burst") không đẩy đuôi ra khỏi viewport | `ui_state().live_text` + `transcript().join("\n")` |

Test **không** soi khung hình đã vẽ: cả ba khẳng định trên state của controller/editor
(`ui_state()`, `transcript()`, `Overlay::scroll()`, `Modal::Overlay`). `ScriptedRenderer` có tồn
tại trong `tui::tests` và được dùng ở các test khác (`tui/mod.rs:549`, `569`, `625`), nhưng
**không** test `k04_*` nào vẽ frame — nên chúng chứng minh **state và luật cuộn**, không chứng
minh cái terminal nhận được.

### 12.4. **Không** được chứng minh (đọc kỹ trước khi báo cáo)

- **TUI, `/more`, phím cuộn và con lăn chuột chưa từng được lái trong console thật trong lượt
  này.** ConPTY cần một console mà sandbox build không có. Ba test `k04_*` khẳng định trên state
  của controller/editor — **không** phải trên khung hình của một terminal thật. Hệ quả: "panel
  cuộn được" được chứng minh ở tầng state, không phải bằng transcript thật.
- **Không có ca PTY nào cho `/more` hay cho phím cuộn.** Bộ 16 ca PTY hiện có **không** ca nào gõ
  `/more`, `PageUp`, `PageDown`, `Home` hay `End` trong panel. Nên U09 ("Esc đóng overlay không
  ghi history") vẫn đúng theo test cũ, nhưng **đường cuộn** thì không có bằng chứng PTY.
- **Alternate scroll (`1007`) không thể được chứng minh bằng test.** Đây là hành vi **phía
  terminal**: app chỉ gửi `ESC [ ? 1007 h` / `ESC [ ? 1007 l` và không đọc gì về chuột. Không
  test nào — và không phép đo nào trong lượt này — chứng minh Windows Terminal (hay bất kỳ
  emulator nào khác) tôn trọng nó. **Chưa đo trên Windows Terminal**; chưa đo trên emulator nào.
- **Chuỗi gợi ý viền dưới nói rộng hơn code (mâu thuẫn thật, đã đo).** Viền dưới mở đầu bằng
  `↑↓/PgUp/PgDn cuộn` (`help.rs:36`, `38`, `41`), nhưng `↑`/`↓` **không** cuộn panel:
  `controller.rs:331`–`337` chỉ bắt `PageUp`/`PageDown`/`Home`/`End`, nên `↑`/`↓` rơi xuống editor
  và ở đó chúng **sửa buffer soạn thảo** (recall history / di chuyển theo hàng, `input.rs:443`–`460`)
  trong khi panel vẫn mở. Gợi ý cũng không nói `Home`/`End` — hai phím **có** tác dụng. Không test
  nào assert chuỗi gợi ý, nên đây là lỗi chữ trong UI không có test canh; sửa nó là việc **code**,
  không thuộc lượt tài liệu này — ghi lại để không bị đọc thành "đã có".
- **Buffer 500 dòng không phải lịch sử đầy đủ.** `/more` cắt theo **dòng logic** ở 500 dòng gần
  nhất; phần cũ hơn chỉ còn trong scrollback của terminal. Không test nào đo hành vi cắt ở đúng
  500 (test chỉ chạy 20–40 dòng).
- **Một lần gate đỏ thoáng qua, không tái hiện.** Một lần chạy `Verify-HaLaunch.ps1` **exit 1**
  với `GATE_FAILED: discovery:i13_... , discovery:i09_...` **trong khi** hai selector đó **có**
  trong file và đúng lệnh discovery chạy trực tiếp trả **exit 0**; lần chạy sau cùng lệnh gate cho
  `GATE_OK: every required step passed` (exit 0). Đây là **flake của bước discovery trong gate**,
  không phải lỗi code của app hay của selector — ghi lại làm quan sát, **không** kết luận là đã sửa.
- **Phép đo ACL Windows (mục 11.2) vẫn chỉ trên một máy.** Không đổi trong lượt này.
- `1007` cũng **không** được kiểm tra trong `NO_COLOR`/`TERM=dumb` hay ở đường plain: nó chỉ được
  bật trong `RawModeGuard::enter`, tức chỉ khi renderer TUI thực sự vào raw mode — nhưng điều đó
  được suy ra từ đường code, **không** có phép đo console thật cho nó.

## 13. Hai lỗi nhìn thấy trên màn hình thật — sửa trong lượt này (screenshot)

Nguồn của mục này là **hai ảnh chụp TUI đang chạy** do người giao việc gửi, không phải suy luận.
Đây là loại lỗi mà bộ test hiện có **không** bắt được: cả hai đều nằm ở khung hình vẽ ra, không nằm
ở dữ liệu.

### 13.1. Work items

| Lỗi (ảnh) | Nguyên nhân đọc được từ code | Sửa | Test canh mới |
|---|---|---|---|
| Câu trả lời in `**Tool call:**` với đủ dấu `*` | `markdown::render` không có nhánh emphasis | `inline_spans` → `Modifier::BOLD`, bỏ marker | `t04_emphasis_markers_become_styling_instead_of_asterisks` |
| Dòng dài bị terminal cắt ở mép, chữ mất | `render` trả một `Line`/dòng văn bản, không đo bề rộng | mọi block qua `wrap_spans(spans, width)` | `t04_long_rows_wrap_instead_of_being_clipped`, `t04_wrapping_breaks_at_spaces`, `t04_a_word_wider_than_the_row_still_wraps`, `t04_wrapping_keeps_every_word_of_a_long_paragraph` |
| Khối `[approval] …` hiện hai lần (scrollback + panel) | `HistoryItem::Approval` được đẩy vào history **ngay khi request tới**, rồi panel vẽ lại cùng dữ liệu | TUI: panel là chỗ duy nhất; history chỉ nhận khi `self.plain` | `t06_the_open_panel_is_the_only_place_the_proposal_is_shown`, `t06_a_plain_session_still_records_the_proposal_it_cannot_panel`, `tui::tests::t06_a_pending_approval_frame_holds_one_copy_of_the_proposal` |
| Viền composer ghi `trả lời panel ở trên`, không nói phím nào | `composer::hint` trả một câu cho mọi modal | `hint` khớp theo `state.modal` | `composer::tests::t03_the_hint_names_the_keys_of_the_panel_that_is_open` |

### 13.2. Số đo

```text
cargo test -p harness-cli --bin ha --locked                    -> 188 passed; 0 failed
cargo test -p harness-cli --test interactive_launch --locked -- --test-threads=1
                                                               -> 18 passed; 0 failed (3 lần liên tiếp)
cargo clippy --workspace --all-targets --locked -- -D warnings -> sạch
cargo fmt --all -- --check                                     -> sạch
pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json        -> passed: true, failures: [] (lần chạy đầu)
pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1       -> PTY_EXIT: 0, 16 passed; 0 failed (22.85 s)
pwsh -NoProfile -File scripts/Install-Ha.ps1                   -> INSTALL_EXIT 0 (release)
  C:\Users\duong\.cargo\bin\ha.exe SHA-256
    59c3b88025483bd41ed3e91f20ef6dc434a8a32401e2166250c6ab67062a2c18
  PTY i14 trên artifact mới cài                                -> PTY_EXIT: 0, ca xanh
paid smoke qua binary đã cài (prompt buộc markdown)             -> SMOKE_EXIT: 0 after 2 s,
                                                                  SMOKE_STOP: final, độ dài 710
```

Ba lần đỏ gặp trong lượt, ghi lại đầy đủ:

| Lần | Ca đỏ | Đọc được gì | Phân loại |
|---|---|---|---|
| 1 | `t07_pty_plain_flag` | Cây **chưa build được**: `Modal` chưa được import trong `composer.rs` (lỗi E0433 của chính lượt này) | Lỗi thật của lượt này — đã sửa |
| 2 | `i14_the_installed_artifact_opens_the_app_in_a_real_terminal` | Assert cuối `transcript.ends_with("\r\n")` (`interactive_terminal.rs:694`); chạy **một mình** ca đó cho `PTY_EXIT: 0` | Flake console: lần đọc cuối của PTY giành với lúc tiến trình thoát. **Không** nới assertion |
| 3 | `i13_resume_continues_the_task_with_recovered_context_and_no_rerun` (trong gate) | `error sending request for url (http://127.0.0.1:59208/chat/completions)`, ca chạy 40.77 s; chạy **một mình** xanh **4/4** | Flake loopback dưới tải — xem 13.2b |

### 13.2b. Flake loopback thứ ba: `i13_resume_...` — đã sửa

Đây là **cùng một họ** với ba flake loopback đã sửa trước đó (mục 12.2 và SPEC 3e.4): máy này thỉnh
thoảng từ chối một kết nối tới listener **đã** bind và đang accept, khi cả workspace chạy nặng.

Khác biệt so với ba lần trước: `i13` chạy `ha chat --headless --resume` như một **tiến trình thật**,
nên bậc thử lại trong `harness-providers` (chỉ có trong tiến trình test) **không** che được nó.
Fixture `sse_fixture_multi` thì đã đúng: nó đếm "request thật" và bỏ qua probe, nên **không** bị
tiêu mất response.

Sửa: bậc thử lại đặt ở phía test, đúng cách `phase_p2` đã làm.

```text
const LOOPBACK_ATTEMPTS: usize = 10;   // backoff 50 ms × 2^min(attempt, 6)
điều kiện thử lại: code() != 0 && stderr chứa "error sending request for url"
```

Hệ quả phải kiểm: một lỗi **thật** (protocol, 4xx, JSON hỏng) không khớp chữ ký đó nên vẫn đỏ ngay
lần đầu; oracle `requests.len() == 2` của ca vẫn nguyên vẹn vì fixture chỉ đếm request thật.

Số đo: **3/3** lần chạy liên tiếp cả `interactive_launch` xanh (34.07 s / 34.49 s / 34.73 s), sau đó
gate `failures: []`. Trước khi sửa: 1 đỏ trong 2 lần chạy gate.

### 13.3. **Không** được chứng minh (đọc kỹ trước khi báo cáo)

- **Chưa có ảnh chụp màn hình sau khi sửa.** Phép kiểm gần nhất với mắt người là
  `tui::tests::t06_a_pending_approval_frame_holds_one_copy_of_the_proposal`: nó vẽ khung hình thật
  qua `ScriptedRenderer` ở **100×30** và đếm `path=src/parser.rs` xuất hiện **đúng một** lần, đồng
  thời khẳng định live block nhường vùng trên cho panel. Đây là **khung hình của `TestBackend`**,
  không phải của ConPTY — nhưng nó là **cùng một** hàm `draw_state` mà đường TUI thật gọi.
- **Đường markdown thì đã có chữ thật của model làm bằng chứng.** Paid smoke ở 13.2 trả về đúng
  `**bold**` cộng một đoạn văn dài, và test
  `t04_a_real_answer_loses_no_marker_and_no_word` dùng **nguyên văn** câu trả lời đó ở width 72:
  khẳng định không còn `**` trên màn hình, mọi dòng ≤ 72 cell, và mọi từ còn đủ theo thứ tự. Cái
  **chưa** đo được là khung hình ConPTY của chính lượt chat đó — nhưng nội dung đầu vào đã là chữ
  thật, không phải chữ tự nghĩ ra.
- **Bề rộng wrap lấy từ `area.width` của khung**, nên hành vi ở console hẹp (< 60 cột) không được
  đo: dưới ngưỡng đó app chuyển sang plain (mục 8), nên đường wrap của TUI không chạy.
- **Bậc thử lại của `i13` không có ca âm chứng minh nó không che lỗi thật.** Lập luận là đọc code
  (chỉ khớp đúng chuỗi `error sending request for url`), **không** phải một ca đỏ cố ý. Đây là cùng
  mức bằng chứng với ba flake loopback trước đó.

## 14. Gate phê duyệt: đọc không hỏi, ghi vẫn hỏi (lượt này)

Nguồn: người giao việc chat và gặp panel duyệt cho `ListFiles: list .`, rồi hỏi vì sao Claude
Code và Codex CLI không hỏi như vậy. Đã tra tài liệu gốc hai bên và đọc binary trên máy, rồi
chốt 4 mục với người giao việc **trước khi** viết code (SPEC 3g có bảng so sánh).

### 14.1. Work items

| # | Việc | Chỗ sửa |
|---|---|---|
| 1 | 5 action chỉ-đọc được miễn hỏi trong workspace | `harness-tools/src/contracts.rs` (`ToolKind::is_read_only`), `turn_driver.rs` (`ApprovalProposal::read_only`) |
| 2 | Danh sách bảo vệ không bao giờ được miễn | **không** thêm gì: đã có sẵn ở `prepare` → `resolve_relative` → `is_sensitive_relative`; việc của lượt này là **chứng minh** nó chặn trước cổng |
| 3 | Ghi/patch/lệnh vẫn hỏi | `is_read_only` trả `false` cho 5 kind còn lại; test liệt kê đủ 10 |
| 4 | Panel thêm `a`, chỉ trong lượt | `ChannelApprovalGate` (`reads_for_run` + `grant`/`clear`), `ApprovalDecision::GrantReadsForRun`, `controller.rs` (phím `a`, nhãn, `finish_run` thu hồi), `tui/widgets/approval.rs` + `composer.rs` + `status.rs` + `layout.rs` |

### 14.2. Số đo

```text
cargo test -p harness-cli --bin ha --locked                    -> 195 passed; 0 failed
cargo test -p harness-tools --locked                           -> 6 passed; 0 failed
cargo clippy --workspace --all-targets --locked -- -D warnings -> sạch
cargo fmt --all -- --check                                     -> sạch
pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json        -> passed: true, failures: []
pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1       -> PTY_EXIT: 0, 16 passed; 0 failed (23.66 s)
pwsh -NoProfile -File scripts/Verify-Docs.ps1                  -> DOCS_OK (exit 0)
```

`t06_pty_approval_y_key` xanh trong lượt này là bằng chứng **console thật** cho panel mới: nó
gõ `request approval fixture`, đọc `[approval] fixture_action`, gửi `y`, rồi đọc
`[approval] granted fixture-approval-1` — tức panel 7 dòng vẫn vẽ vừa viewport và đường trả lời
không đổi.

### 14.3. Test nào chứng minh điều gì

| Test | Điều được chứng minh | Oracle |
|---|---|---|
| `tool_kinds_classify_read_only_by_construction_not_by_name` | đúng 5 kind chỉ-đọc, đúng 5 kind không; liệt kê **đủ** cả 10 nên thêm kind mới là đỏ | `ToolKind::is_read_only()` |
| `a_read_only_kind_cannot_reach_a_protected_path_through_the_gate` | `.env` → `SensitivePathDenied`, `../outside.txt` và `src/../../outside.txt` → `WorkspaceEscape`, **tất cả trước khi proposal tồn tại**; `src/main.rs` cùng kind thì prepare được | mã lỗi của `ToolExecutionService::prepare` trên một task **đã admit** |
| `t08_a_granted_read_is_not_asked_about_again_and_is_still_recorded` | không có `ApprovalRequired`/`ApprovalExpired`, nhưng **có** `Notice` chứa `read-only` + summary | timeout 30 ms: nếu có chờ thì đã ra `Expired` |
| `t08_a_granted_read_never_covers_a_mutating_action` | `ApplyPatch` vẫn ra `Expired` + vẫn có `ApprovalRequired` dù cổng đang mở | cùng cổng, cùng cờ, khác `read_only` |
| `t08_a_read_is_asked_about_until_the_user_allows_reads` | trước khi cho phép thì đọc vẫn mở panel, và event mang `read_only: true` | `reads_granted_for_run() == false` |
| `t08_the_wider_answer_grants_the_pending_action_and_the_run` | `GrantReadsForRun` trả `Granted` cho action đang chờ **và** mở cổng; `clear` rồi thì đọc lại `Expired` | hai `oneshot` qua cổng thật |
| `t06_the_wider_grant_is_offered_for_reads_and_not_for_writes` | modal write có `read_only: false`; bấm `a` trên write **không** gửi quyết định nào; modal read có `read_only: true` và hint có chữ `a` | `ui_state().modal`, log `answers` |
| `t06_the_read_only_key_grants_the_action_and_the_run` | quyết định là `GrantReadsForRun`, port nhận `approve_reads_for_run`, transcript có `[approval] granted (reads allowed for this turn) req-read-1` | log của `RecordingPort` |
| `t06_the_read_only_grant_does_not_survive_the_turn` | quanh `RunTerminal` port nhận đúng `[true, false]`, và `reads_for_run` về `false` | thứ tự lời gọi, không chỉ trạng thái cuối |

### 14.4. **Không** được chứng minh (đọc kỹ trước khi báo cáo)

- **Không có ca PTY nào bấm `a`.** Ca PTY duyệt dùng `y`. Đường `a` được chứng minh ở tầng
  controller + cổng (3 test), **không** phải trên ConPTY. Việc còn lại: một lần chạy tay, hoặc
  thêm ca PTY gõ `a`.
- **Chưa đo panel 7 dòng ở console thấp nhất.** `PANEL_ROWS = 7` bị `modal_rows` cắt theo
  `available`, nên ở viewport 10 hàng dòng cuối (`a ...`) có thể không hiện. Trước lượt này panel
  là 6 hàng, nên **ngưỡng hỏng dịch lên đúng 1 hàng** — chưa đo bằng mắt ở ngưỡng đó.
- **`reads_for_run` là trạng thái một tiến trình.** Hai cửa sổ `ha` trên cùng project có cổng
  riêng; bấm `a` ở cửa sổ này **không** mở cổng ở cửa sổ kia. Đúng thiết kế, nhưng chưa có test
  khẳng định nó, và cũng chưa đo hai cửa sổ thật.
- **Không** có mục "ghi vào policy vĩnh viễn" như `[p]` của Codex. Đây là quyết định, không phải
  thiếu sót: nó ghi ra file và cần người dùng quyết riêng.
- **Bộ 5 kind chỉ-đọc là allowlist do tôi chọn**, không phải kết quả đo. Claude Code dùng bộ lệnh
  shell dựng sẵn rộng hơn nhiều (`ls cat grep find git ...`); bản này chỉ có 5 action của
  `CodingToolAction`, vì app **không** chạy lệnh shell tuỳ ý qua đường chỉ-đọc.
