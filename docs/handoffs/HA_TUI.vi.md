# Handoff HA_TUI — nâng `ha` thành TUI terminal (T01–T08)

Cập nhật: **sau CP-D (T01–T08) — track T xong về code, gate và tài liệu**. SPEC: [specs/HA_TUI.vi.md](../specs/HA_TUI.vi.md) ·
Evidence: [evidence/HA_TUI.vi.md](../evidence/HA_TUI.vi.md) ·
Plan: [HA_TUI_PLAN.vi.md](../HA_TUI_PLAN.vi.md).

## 1. Assignment hiện tại và ràng buộc của người giao việc

*"Triển khai T01–T08 theo plan HA_TUI, đi theo checkpoint CP-A → CP-D; không chuyển
checkpoint khi gate chưa `failures: []`; cập nhật handoff HA_TUI sau mỗi checkpoint."*

Quyền đã được người giao việc xác nhận bằng câu hỏi trực tiếp trước khi coding:

| Hành động | Trạng thái |
|---|---|
| Commit local + **push** `origin/master` | **Được cấp** |
| Paid provider smoke | **Được cấp** |
| Cài thật lên máy này (`Install-Ha.ps1`, User PATH) | **Được cấp** |
| Publish release/tag | Không được cấp |
| Đổi schema store / contract `TurnDriver`–`SessionPort` ngoài plan | Không được cấp |

Phạm vi lượt này: **cả track T01–T08**, dừng báo cáo ở mỗi checkpoint.

## 2. Branch / base / source digest

- Nhánh `master`. Base khi bắt đầu track T: `c97c7dd`; `HEAD` lúc chốt CP-A:
  **`b0ba93f`** (`feat(ha-tui): inline-viewport TUI, typed effects and the CP-A
  checkpoint (HA_TUI T01-T02)`), đã **push** lên `origin/master`.
- Giữa lúc bắt đầu và lúc chốt, `origin/master` nhận thêm 5 commit của **track H**
  (`8ea37c7`, `d7e6a2a`, `01a7bee`, `ed22df4`, `41322e3` — CI runner, gate
  parallelism, evidence H07). Chúng đến từ một phiên làm việc khác trên **cùng
  workspace**; xem mục 12.
- Cây nguồn khi bắt đầu **không sạch**: `crates/harness-cli/tests/phase_p2.rs` và
  `scripts/Verify-Phase.ps1` sửa dở từ việc đo flake loopback của track H (chờ fixture
  task được schedule; giữ parallelism trong `Verify-Phase.ps1`). Hai file này đi cùng
  commit T01 — **không** phải việc của track T, nhưng không được để mất.

## 3. Việc đã xong

### T01 — spike và quyết định (đóng)

- Thêm `ratatui = "=0.30.2"` (không default feature; bật `crossterm`, `crossterm_0_29`,
  `layout-cache`, `underline-color`) và `unicode-width = "=0.2.2"` vào workspace +
  `harness-cli`; `unicode-normalization` (đã có) dùng cho NFC.
- Đo: **một** crossterm 0.29.0, **một** unicode-width 0.2.2.
- Bốn câu hỏi plan yêu cầu đã có số (SPEC mục 2): chiều cao inline viewport **không**
  đổi lúc chạy; `TestBackend` chạy được inline + `insert_before`; **Alt+Enter đo được**
  trên ConPTY này (`Enter + ALT`), Ctrl-J tới dưới dạng `Enter + CONTROL`; paste tới
  dạng `Event::Paste`; `scrolling-regions` chạy được nhưng **bị loại** (nhiều mảnh ghi
  hơn, lợi ích bị D4 vô hiệu).
- POC và cờ `--tui-spike` đã **xoá**; cờ `HA_TUI_SPIKE_*` không còn trong cây.
- `map_key` được sửa theo số đo: `Enter + CONTROL` (Ctrl-J) và `Enter + ALT` (Alt+Enter)
  → `Key::Newline`; test `t01_line_feed_and_alt_enter_are_the_multiline_key_on_this_console`.

### T03–T07 (đóng)

- **T03 composer**: paste giữ newline (đổi hợp đồng có chủ ý, test cũ cập nhật theo);
  Ctrl-U/W/A/E; ↑↓ theo hàng khi nhiều dòng; Tab hoàn thành slash command duy nhất;
  wrap theo cell sau NFC và **một** hàm wrap dùng chung cho chiều cao ô lẫn con trỏ.
- **T04 history/live block**: `Effect::Stream` → `HistoryItem::Assistant` theo đúng thứ tự
  `flush_stream`; tool card settle tại chỗ kèm thời lượng; overflow commit theo thứ tự;
  markdown-lite không thêm dependency và không nuốt ký tự.
- **T05 status bar**: spinner/step/tools/elapsed/model/session/hint; giới hạn lấy từ
  `SessionPort::limits()`; **không** vẽ lại khi rảnh (đo: 3 poll → 1 draw).
- **T06 approval/picker/overlay**: panel đếm ngược theo `expires_at` từ event, `y`/`n` trả
  lời ngay; picker ↑↓/Enter/Esc; overlay không ghi history; `Esc` không bao giờ huỷ lượt.
- **T07 fallback/phục hồi**: probe renderer là hàm thuần (`--plain`, `HA_UI=plain`,
  console < 60×10, `TERM=dumb`) với lý do ra stderr; resize giữ draft; panic hook phục hồi
  terminal mà không đổi hành vi I08; thoát để con trỏ ở dòng mới; `NO_COLOR` không SGR màu.

### T08 — gate, PTY, docs (đóng)

- Gate có `$requiredTuiSelectors` (4 selector T), in trong `required_tui_tests`, và self
  test canh danh sách không được ngắn đi.
- PTY runner có **16 ca** (10 ca H cũ trên TUI mặc định + 6 ca T) và lưu transcript từng ca.
- Operator guide **vi + en** mục 12.1–12.3: bảng phím, plain fallback có lý do, giới hạn
  Windows đo được (Shift+Enter không phân biệt được; kill cứng không phục hồi được).
- README trỏ tới plan/SPEC/evidence/handoff của track T.

### T02 — view model và refactor controller (đóng)

- `events.rs`: `HistoryItem`, `ToolState`, `UiState`, `Modal`, `Key` mới,
  `SessionEvent::{StepStarted, ApprovalExpired}`, `ApprovalRequired` mang `expires_at`.
- `controller.rs`: `Effect::{History(HistoryItem), Stream(String), Redraw, Exit(u8)}`,
  `ui_state()`, `tick()`, đếm `steps`/`tool_calls`, mở/đóng modal, `y`/`n` trả lời ngay
  trong TUI, `Esc` đóng panel mà không trả lời.
- `view.rs`: `HistoryItem::plain_lines()` tái dùng đúng các hàm cũ; `HistoryItem::Message`
  cho các dòng in nguyên văn (`/help`, `/status`, dòng trống của Ctrl-C).
- `service.rs`: map `TurnProgress::StepStarted` (bỏ `return`), đo thời lượng tool theo
  `Instant` trong observer, phát `SessionEvent::ApprovalExpired` **trước** khi trả
  `ApprovalAnswer::Expired`; `SessionPort::{limits, tool_elapsed}`.
- `app.rs`: chọn renderer (`tui_fallback_reason`), cờ `--plain` / `HA_UI=plain`, fallback
  in lý do ra stderr; renderer plain đọc `plain_lines`.
- **TUI renderer mới** (`interactive/tui/`): `mod.rs` (inline viewport + `insert_before`
  + thoát vẽ frame trắng), `layout.rs`, `theme.rs`, `history.rs`, `markdown.rs`,
  `widgets/{composer,status,approval,picker,help}.rs`.

## 4. Trạng thái hiện tại (CP-D)

- **T01–T08 xong.** Không còn việc dở trong track T.
- Bằng chứng cuối: `cargo test -p harness-cli --bin ha --locked` → **133 passed, 0 failed**;
  `cargo clippy --workspace --all-targets --locked -- -D warnings` sạch;
  `Verify-HaLaunch.ps1 -Json` → `passed: true, failures: []`;
  `Invoke-HaPtyAcceptance.ps1 -TimeoutSeconds 900` → `PTY_EXIT: 0`, **16 passed; 0 failed**
  trong một lần chạy (21.98 s); `Verify-Docs.ps1 -SelfTest` → `DOCS_OK`.
- Audit nối luồng thật sau CP-D đã sửa `/exit` giữa run: controller chờ terminal event trước
  khi thoát để service nhả SQLite writer; `i05` xanh 3/3 riêng và xanh trong bộ PTY đầy đủ.
  Cùng audit đã làm ổn định oracle ACL, self-test PATH của installer và capture stderr của
  gate trên Windows PowerShell 5.1.
- Không đạt (ghi thẳng, xem evidence mục 8): **paid smoke chưa chạy vì môi trường không có
  credential**, **chưa cài lên máy user**, chưa build/chạy Linux, chưa đo conhost cũ riêng.
- Còn một điểm tên test lệch plan: plan gọi U06 là `t05_status_reflects_phase_steps_tools_and_elapsed`
  và U05 là `t04_tool_card_settles_in_place_with_duration`; trong cây hiện có
  `t05_a_running_status_reports_spinner_steps_tools_and_the_clock` (status widget) +
  `t05_idle_poll_does_not_redraw` (vòng lặp) và `t04_tool_card_settles_in_place_with_duration`.
  Nội dung acceptance được phủ; tên khác plan nên **không** đưa vào `required_tui_tests`.

## 4b. Lịch sử: việc đang dở ở CP-A (đã xong ở CP-D)

- CP-A **đã xong về code và số đo**. T03+ **chưa bắt đầu**.
- Trạng thái cuối: `cargo test -p harness-cli --bin ha --locked` → **127 passed, 0 failed**;
  clippy `-D warnings` sạch; `interactive_session` 9/9; `interactive_launch` 18/18;
  `phase_p1` 21/21; gate H `passed: true, failures: []` (chạy một mình).
- **TUI đã được nối vào `app::run`** (`tui::run` khi console ≥ 60×10, `TERM` ≠ `dumb`,
  không `--plain`, `HA_UI` ≠ `plain`; ngược lại plain + một dòng lý do ra stderr).
- Đo trên console thật: composer vẽ đúng UTF-8 (`> Nhập yêu cầu`, `> sửa lỗi parser`),
  header vào scrollback phía trên viewport, thoát để con trỏ ở dòng mới, I08 vẫn exit 1.
  Chi tiết + cạm bẫy ở SPEC mục 3b.
- **Việc còn lại của CP-A:** 10 ca PTY cũ trên TUI mặc định **chưa xanh đủ trong một lần
  chạy**: mỗi lần 1–3 ca đỏ và **tập ca đỏ đổi giữa các lần** với cùng một binary. Đã
  loại trừ lỗi renderer (transcript của ca đỏ vẫn đủ header + composer + echo; từng ca
  đỏ ở lần này lại xanh ở lần khác). Kết luận: race của **harness PTY**, và đây là việc
  đầu tiên của lượt sau — T07 yêu cầu harness hết flake trước khi thêm ca mới.

## 5. File đã đổi và lý do

| File | Lý do |
|---|---|
| `Cargo.toml`, `crates/harness-cli/Cargo.toml`, `Cargo.lock` | D1: pin ratatui/unicode-width; feature `scrolling-regions` (mặc định tắt) |
| `crates/harness-cli/src/interactive/events.rs` | T02 vocabulary |
| `crates/harness-cli/src/interactive/controller.rs` | T02 reducer; active exit chờ run terminal event để nhả writer |
| `crates/harness-cli/src/interactive/view.rs` | `plain_lines`, `seconds_label`, `clock_label` |
| `crates/harness-cli/src/interactive/input.rs` | T03 editor: paste giữ newline, Ctrl-U/W/A/E, ↑↓ theo hàng, Tab, picker/overlay |
| `crates/harness-cli/src/interactive/service.rs` | StepStarted, thời lượng tool, `ApprovalExpired`, `limits` |
| `crates/harness-cli/src/interactive/terminal.rs` | map phím mới, `size()`, `ScriptedBackend::with_size` |
| `crates/harness-cli/src/interactive/app.rs` | chọn renderer, `--plain`, plain đọc `plain_lines` |
| `crates/harness-cli/src/interactive/{mod.rs,main.rs}` | `plain` trong `LaunchMode`, cờ `--plain`, `HA_UI` |
| `crates/harness-cli/src/interactive/tui/**` | renderer TUI mới |
| `crates/harness-cli/tests/interactive_terminal.rs` | helper `spawn_process` (dùng cho ca PTY mới ở T08) |
| `scripts/Read-HaTranscript.mjs` | đọc transcript PTY thành màn hình (tool bằng chứng) |
| `docs/specs/HA_TUI.vi.md`, `docs/evidence/HA_TUI.vi.md`, `docs/handoffs/HA_TUI.vi.md` | tài liệu track T |
| `crates/harness-cli/tests/phase_p2.rs`, `scripts/Verify-Phase.ps1` | thay đổi có sẵn từ track H, đi cùng commit |

## 6. Hợp đồng / quyết định đã chốt — không đổi ngầm

- D1–D7 của plan, cộng kết quả T01: chiều cao viewport **cố định lúc khởi động**
  (`min(rows/2, 14)`, sàn 5); `insert_before` **luôn** kèm `draw`; thoát phải vẽ frame
  trắng rồi trả terminal; **không** bật `scrolling-regions`.
- Mốc chữ D5: composer `> `/`.. `, user `> `, tool `[tool] `, run `[run] `,
  approval `[approval] `, error `[error] `, info `[info] `. Test PTY cũ grep theo các mốc này.
- Thay đổi hợp đồng **có chủ ý** ở T03: `normalize_paste` **giữ newline**
  (`\r\n` → `\n`); test cũ `h03_editor_paste_never_submits_multiple_commands` đã được
  cập nhật để assert đúng điều đó, vẫn "một paste = một submit". Ghi ở SPEC và mục 3.3.
- Luật H không đổi: một input mỗi session, approval fail-closed, không mock fallback,
  headless không ANSI, exit 0/1/2.

## 7. Lệnh đã chạy và lần đỏ gần nhất

- `cargo test -p harness-cli --bin ha --locked` → 124 passed.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` → sạch.
- `pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1 -TimeoutSeconds 900` →
  `PTY_EXIT: 0`, 10 passed (chạy khi TUI chưa nối vào `app::run`).
- `pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json` → lần đầu **đỏ ở `docs`**
  (`BROKEN_LINK: docs/specs/HA_TUI.vi.md -> ../evidence/HA_TUI.vi.md` vì evidence chưa
  tồn tại); sau khi tạo evidence + handoff phải xanh lại — **đó là việc đầu tiên của
  lượt sau nếu chưa chạy lại**.

## 8. TODO theo thứ tự, kèm oracle

1. **Làm harness PTY hết flake** (việc chặn CP-B):
   - `cargo test -p harness-cli --test interactive_terminal --locked --no-run` rồi chạy
     `scripts/Invoke-HaPtyAcceptance.ps1 -TimeoutSeconds 900` **ba lần** và ghi lại ca đỏ
     mỗi lần, để thấy đúng race (không sửa assertion cho xanh).
   - Ứng viên: gửi phím theo từng ký tự với khoảng nghỉ ngắn và **chờ echo** trước khi gửi
     ký tự kế tiếp (hiện `send` chỉ chờ lần vẽ đầu), và/hoặc đọc transcript bằng cửa sổ
     theo thời gian thay vì so chuỗi một lần.
   - Điều kiện đóng: `PTY_EXIT: 0`, "10 passed; 0 failed" trong **một** lần chạy, hai lần
     liên tiếp.
2. **Chạy lại gate H một mình** (đừng chạy song song với build cargo khác: một lần gate
   đỏ `acceptance-launch`/`phase_p1`/`phase_p2` khi có build chạy cùng lúc; chạy riêng
   thì cả ba xanh).
3. **Commit + push T01–T02** (CP-A).
4. **T03 composer**: `tui/widgets/composer.rs` đã có wrap theo cell + cursor; cần test
   `TestBackend` cho U02/U03/U10 và ca PTY `i06`/`i21`.
5. **T04 history/live block**: commit dòng overflow theo thứ tự `flush_stream`, tool card
   settle tại chỗ (`HistoryItem::Assistant` hiện chưa được dùng — T04 sẽ dùng).
6. **T05 status bar**: nối `controller.tick()` vào vòng lặp TUI (`Effect::Redraw` mỗi
   100 ms khi có run), test `t05_idle_loop_does_not_redraw` bằng số lần `draw`.
7. **T06/T07/T08** theo plan, rồi CP-B → CP-C → CP-D.

## 8b. Gate CP-D — các lần chạy thật

| Lần | Kết quả | Ca đỏ | Xử lý |
|---|---|---|---|
| 1 | `passed: true`, `failures: []` | — | lần được nhận |
| 2 | `passed: false` | `regression-phase_p2` | chạy riêng: **17 passed, 0 failed** |
| 3 | `passed: false` | `providers-streaming` | chạy riêng: **3 passed, 0 failed** |

Hai lần đỏ đó là **flake loopback**, và nó đã được **sửa gốc** sau đó (ba nguyên nhân:
thiếu readiness handshake, fixture panic trên RST của probe, retry quá ngắn — chi tiết và
số đo ở evidence mục 8). Đo lại **sau khi sửa**: gate `Verify-HaLaunch.ps1 -Json`
**3/3 lần `passed: true, failures: []`** liên tiếp, và ca `i13_resume` xanh khi chạy cả
suite chứ không chỉ khi chạy riêng. Không sửa assertion nào để đạt điều đó: chỉ fixture
được làm cho kiên định, còn lỗi thật vẫn đỏ ngay lần thử đầu.
PTY trong cùng khoảng thời gian: `PTY_EXIT: 0`, **16 passed; 0 failed**.

## 9. Next action chính xác

Track T và audit nối luồng thật không còn việc code cục bộ. Việc còn lại **chỉ** là ba điều kiện môi trường ở mục 8 của
evidence, theo thứ tự:

1. **Paid provider smoke** (cần credential — quyền đã có):
   ```text
   $env:DEEPSEEK_API_KEY = '<key>'
   pwsh -NoProfile -File scripts/Smoke-HaProvider.ps1
   ```
   Kỳ vọng: provider thật trả lời, kết quả ghi vào evidence mục 8.
2. **Cài thật lên máy user** (quyền đã có) — chạy `scripts/Install-Ha.ps1` theo hướng dẫn
   operator guide mục 11, rồi chạy lại PTY `i14` để chứng minh bản cài mở TUI.
3. **Linux**: build + gate trên một máy Linux (hiện chỉ có Windows x64).

Sau **bất kỳ** thay đổi nào, chạy lại:

```text
cargo test -p harness-cli --bin ha --locked
pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json
pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1 -TimeoutSeconds 900
```

Kỳ vọng: `133 passed`; `passed: true, failures: []`; `PTY_EXIT: 0` với **16** ca xanh.

## 9b. Một key là đủ (thay đổi T08)

`DEEPSEEK_API_KEY` (hoặc `HA_API_KEY`) là **toàn bộ** cấu hình provider: endpoint và model
mặc định theo giá trị `DeepSeek` công bố (<https://api-docs.deepseek.com/> →
`https://api.deepseek.com`, `deepseek-flash`), biến do người dùng đặt luôn thắng. Smoke
theo cùng quy tắc, nên chạy paid smoke chỉ cần:

```powershell
$env:DEEPSEEK_API_KEY = '<key>'
pwsh -NoProfile -File scripts/Smoke-HaProvider.ps1
```

Test canh hợp đồng mới: `t08_one_deepseek_key_is_a_complete_provider_setup` và self test
của smoke (đường mặc định + đường override).

## 9c. Paid smoke đã chạy (CP-D)

Một lượt thật, một turn, prompt `Reply with the single word: ready`:

```text
model:    deepseek-flash (DeepSeek default)   endpoint: https://api.deepseek.com (DeepSeek default)
SMOKE_EXIT: 0 after 1 s   SMOKE_RESPONSE: ready   SMOKE_OK
```

Credential đến từ store của app (không có biến môi trường nào đặt trong lượt này). Smoke
đã được sửa theo đúng cách đó: nó không tự quyết định "chưa cấu hình" nữa — app là nơi
quyết định, smoke chỉ chạy một turn rồi báo cáo; khi không có credential ở đâu, app
fail-closed và smoke thoát 2 với `SMOKE_NOT_RUN`. Còn lại của mục 8 evidence:
**cài thật lên máy user** (chưa làm) và **Linux** (chưa có máy).

## 10. Blocked on

Không. Quyền commit/push, paid smoke và cài thật đã được cấp.

## 12. Cảnh báo: workspace có writer song song

Trong lúc track T đang chạy, **một phiên/agent khác đã và đang sửa cùng workspace**:

- `HEAD` nhảy từ `c97c7dd` sang `41322e3` qua 5 commit track H mà phiên này không tạo.
- `scripts/Verify-HaLaunch.ps1` được sửa để thêm `$requiredTuiSelectors` (4 selector T)
  và `crates/harness-cli/src/interactive/headless.rs` được thêm `acceptance_trace` —
  **không phải việc của phiên này**, nhưng đã nằm trong commit `b0ba93f` vì cùng cây.
- Một số file bị sửa **trong lúc** phiên này đang làm (`tui/mod.rs`, `layout.rs`,
  `composer.rs`, `status.rs` có `LastWriteTime` xen giữa các lần đọc của phiên này).
- Bốn selector T trong gate trùng **đúng tên** với test của plan
  (`t02_plain_transcript_is_byte_identical_to_h03`, `t03_paste_keeps_newlines_and_submits_once`,
  `t04_history_order_is_user_tool_assistant_run`, `t06_y_key_grants_exactly_the_pending_request`)
  và cả bốn hiện **có** trong `ha --list` và xanh — nên gate vẫn `passed: true`.

**Hệ quả cần người giao việc quyết định:** nếu phiên kia vẫn đang chạy, hai phiên có thể
ghi đè lên nhau (đã suýt xảy ra: một lần `controller.rs` bị ghi rỗng giữa lượt, và một
lần file bị sửa ngoài ý muốn của phiên này). Đề nghị: **chỉ một phiên tiếp tục track T**;
phiên còn lại nên dừng trước khi lượt sau bắt đầu T03, hoặc hai phiên chia file rõ ràng.

Cách kiểm tra nhanh trước khi tiếp tục:

```text
git log --oneline -3            # HEAD phải là b0ba93f hoặc commit kế tiếp do chính bạn tạo
git status --porcelain          # phải sạch, hoặc chỉ có thay đổi của chính bạn
```

## 11. Không lặp lại

- Không thêm lại POC T01 hay cờ `--tui-spike`.
- Không bật `scrolling-regions` (đã đo và loại).
- Không chạy lại `cargo metadata` không `--locked` để "sửa" lockfile: lockfile đã đúng,
  `cargo build --locked` xanh.
- Không chạy lại `Invoke-HaPtyAcceptance.ps1` với filter `t01` (các ca đó thuộc POC đã xoá).

## 13. `/key` — lưu API key trong app (lượt này, **đã commit ở `385a98f`**)

Trạng thái thật: **đã commit và push.** `/key` nằm trong commit `385a98f`; ba bản sửa tiếp theo
(`e315b44` 404, `081d36a` 422, `9df9b32` ACL + fixture) và các lượt sau (`2e93180`, `1340129`,
`cce5c13`, `5883d59`, `213884a`) cũng đã push. Lúc soạn mục này `HEAD` = `origin/master` =
`213884a` (khác với `aeaf7b7` ghi trong bản gốc bên dưới). Hợp đồng ở SPEC mục 3d, số đo và
`not_run` ở evidence mục 11 (11.2 ghi hash, phép đo ACL và khác biệt môi trường).

> Phần còn lại của mục 13 giữ nguyên như bản ghi của lượt `/key`; mọi câu nói "chưa commit",
> "untracked" hay "HEAD `aeaf7b7`" trong đó **đã cũ**, xem mục 14 để biết trạng thái hiện tại.

### 13.1. Việc đã xong

- `credentials.rs` (mới): file `credentials.env` trong **`<data dir>/private/`** (hoặc
  `HA_CREDENTIALS_DIR`, dùng nguyên trạng), parser đúng một dòng `DEEPSEEK_API_KEY="..."` (từ chối
  tên biến khác, khai hai lần, giá trị không quote; không lặp giá trị trong thông điệp lỗi),
  `save()` staging rồi `rename`, `CredentialSource` chỉ mang **tên** nguồn.
- **Siết quyền lúc tạo file**: `write_staged` mở file staging bằng `OpenOptions` + `mode(0o600)`
  trên Unix rồi `sync_all()`, nên key không bao giờ nằm trong file ai cũng mở được, kể cả trong
  khoảnh khắc giữa tạo và siết; thư mục `0700`.
- **ACL Windows thật**: `restrict_acl` gọi `icacls <dir> /inheritance:r` rồi `/grant:r
  "<USERDOMAIN>\<USERNAME>:(OI)(CI)F"` + `/grant:r "SYSTEM:(OI)(CI)F"`; file thừa hưởng ACL đó.
  Đo trước/sau trên `%LOCALAPPDATA%\HarnessAgents`: trước có `CodexSandboxUsers:(I)(OI)(CI)(RX)`
  (nhóm không phải user đọc được), sau chỉ còn `NT AUTHORITY\SYSTEM` + `DESKTOP-14QHC6K\duong`
  (file thừa hưởng với cờ `(I)`).
- **Trung thực hoá bằng kiểu**: `Protection` có bốn nhãn (`OwnerOnly`, `OwnerOnlyAcl`,
  `ProfileDefault`, `NotReverified`); `save()` **trả về** nhãn nó vừa áp; `/status` in thêm dòng
  `Provider: credential file protection: …`. `icacls` fail ⇒ `ProfileDefault`, với câu mô tả nói
  thẳng là tài khoản khác **có thể** đọc được — không hứa suông.
- `/key` (không tham số) vào secret-entry mode, buffer mask `•` mỗi ký tự, Enter lưu, giá trị
  không vào history; **Esc huỷ** secret entry (`input.rs:351`–`368`);
  `/key <giá-trị>` lưu trực tiếp, chỉ một token, và **đã được mô tả trong `/help`** là kém riêng
  tư hơn vì giá trị nằm trong history của terminal.
- Lưu xong: ghi `config.toml` tối thiểu `schema_version = 1` **chỉ khi file chưa tồn tại**,
  reload config, xoá gate `setup_required`, đổi phase `SetupRequired | Booting → Ready`, vẽ lại
  header; lượt kế tiếp `resolve_provider` lại nên **không cần restart**.
- `/help` có `/key` và `/key <value>`; `SLASH_COMMANDS` khi đó **8** phần tử (nay **9** sau `/more`,
  xem mục 14); `/status` + `/model` in **nguồn** credential (`credential from environment variable
  ...` / `credential from saved file ...`) và không bao giờ in giá trị;
  `SessionEvent::ProviderConfigured { source }` chỉ mang nguồn.
- 19 test mới cho feature (`k01_*`, `k02_*`, `k03_*`; 18 biên dịch trên Windows) — danh sách +
  oracle ở evidence mục 11.5. Mask được assert ở **ba tầng**: editor
  (`k01_secret_entry_masks_the_buffer_and_never_reaches_history`), controller
  (`k01_key_entry_masks_saves_clears_the_gate_and_admits_the_next_message`) và **khung hình đã vẽ**
  (`tui::tests::k01_a_secret_buffer_is_painted_as_a_mask`).
- **Số test của binary `ha`: 153, đo lúc 10:49 ngày 20/09/2026** bằng
  `cargo test -p harness-cli --bin ha --locked -- --list`; `cargo test --release -p
  harness-cli --bin ha --locked` cho **153 passed; 0 failed** trong phiên có quyền đổi ACL (cùng
  ngày), còn phiên soạn tài liệu này (policy `workspace-write`) cho **152 passed; 1 failed** vì
  `icacls` bị từ chối — xem 13.5 mục 2. Đây là **số đo có ngày**, không phải hằng số.
- Đo end-to-end bằng binary release thật, **dùng đường dẫn mặc định mới**: với
  `HA_HOME/data/private/credentials.env` (không set `HA_CREDENTIALS_DIR`, không có biến môi trường
  nào giữ key) và endpoint loopback chết, `ha chat --headless` đọc được file, mở store, nhận lượt
  rồi đi tới lời gọi provider và exit 1 — chi tiết + lệnh ở evidence mục 11.4.

### 13.2. File đã đụng

| File | Việc |
|---|---|
| `crates/harness-cli/src/interactive/credentials.rs` | **mới**: file credential trong `private/`, parser, `save`, `write_staged` (tạo file `0600` trên Unix), `restrict_acl` (`icacls`), `Protection`, `source`, `resolve_file`, `CredentialSource` |
| `crates/harness-cli/src/interactive/service.rs` | credential lấy từ `credentials::source`; `EnvironmentCredential` đọc file lúc gọi; `provider_diagnostics` (kèm dòng protection); `save_credential`; `validate_credential_file` |
| `crates/harness-cli/src/interactive/controller.rs` | nhánh `/key`, `save_key`, phase/header sau khi lưu, `display_buffer` cho mask, notice nói `/key <value>` kém riêng tư, **test controller cho cả chuỗi `/key`** |
| `crates/harness-cli/src/interactive/input.rs` | secret entry (`begin_secret_entry`/`take_secret`/`cancel_secret`), `SECRET_MASK`, `InputOutcome::Secret`, **nhánh `Key::Esc` huỷ secret entry** |
| `crates/harness-cli/src/interactive/tui/mod.rs` | **test khung hình đã vẽ** cho mask (`k01_a_secret_buffer_is_painted_as_a_mask`) |
| `crates/harness-cli/src/interactive/bootstrap.rs` | `credential_saved`, `write_minimal_config`, header nêu nguồn |
| `crates/harness-cli/src/interactive/events.rs` | `SessionEvent::ProviderConfigured { source }` |
| `crates/harness-cli/src/interactive/view.rs` | hai dòng `/help`: `/key` và `/key <value>` (kém riêng tư hơn, một token) |
| `crates/harness-cli/src/interactive/headless.rs` | `validate_credential_file` trước lượt headless |
| `crates/harness-cli/src/interactive/mod.rs` | khai báo module `credentials` |
| `docs/specs/HA_TUI.vi.md`, `docs/evidence/HA_TUI.vi.md`, `docs/OPERATOR_GUIDE.vi.md`, `docs/handoffs/HA_TUI.vi.md` | tài liệu lượt này |

Không đụng `Cargo.toml`/`Cargo.lock` (không thêm dependency) và không đụng `.rs` nào khác.

### 13.3. Lệnh kiểm chứng (đúng thứ tự)

```text
git status --porcelain                 # cây đang dở (writer song song: memory/project)
cargo check -p harness-cli --all-targets --locked
cargo test -p harness-cli --bin ha --locked
cargo test --release -p harness-cli --bin ha --locked
cargo test -p harness-cli --bin ha --locked -- --list | Select-String 'k0[123]_'
cargo clippy --workspace --all-targets --locked -- -D warnings
pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json
pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1 -TimeoutSeconds 900
pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest
```

Kỳ vọng đo được (20/09/2026): `153 passed; 0 failed` với `cargo test --release -p harness-cli --bin
ha --locked` **trong phiên có quyền đổi ACL**; phiên bị chặn đổi ACL (như phiên soạn tài liệu
này) sẽ thấy **152 passed; 1 failed** ở đúng test ACL — đó là khác biệt quyền, không phải hồi quy
(13.5 mục 2). Docs: `DOCS_OK`. Clippy, gate và PTY **chưa chạy** cho feature này.

### 13.4. Plan item để đánh dấu: **không có**

`docs/HA_TUI_PLAN.vi.md` chỉ có work item T01–T08, và mục 11 ghi rõ ngoài phạm vi là full-screen,
theme tuỳ chỉnh, chuột, cuộn history trong app, i18n — **không** nhắc việc nhập key. Vậy plan
**không** có item nào cho `/key`, nên **không** có gì để ghi "đã implement"; plan **không** được
sửa (đúng luật của chính plan). Lưu ý tên test dùng tiền tố `k01_`/`k02_` là **cục bộ của feature
này**, không phải case K01/K02 của P1/P6 (K01–K14 trong `PLUGIN_ARCHITECTURE.vi.md` là chuyện
plugin, không liên quan) — đừng map nhầm khi đọc registry.

### 13.5. Việc còn lại / open items

1. ~~**Cây chưa commit.** Toàn bộ feature `/key` nằm trong working tree; `credentials.rs` còn
   **untracked**; `HEAD` vẫn `aeaf7b7`.~~ **Đã đóng ở mục 14:** `/key` được commit ở `385a98f`,
   `credentials.rs` đã được track, và `HEAD` = `origin/master` = `213884a` (kiểm ngày 20/09/2026).
   Điều còn đúng từ mục này: một writer song song vẫn đang sửa `interactive/*` và các crate memory,
   nên **luôn** xác nhận `git status` và mtime trước khi commit.
2. **Test ACL phụ thuộc quyền của môi trường chạy.** `k03_a_saved_key_is_restricted_to_this_account_by_an_acl`
   **xanh** ở phiên có quyền đổi ACL (bên giao việc: `153 passed; 0 failed`), nhưng **đỏ** ở phiên
   bị chặn (soạn tài liệu này: `152 passed; 1 failed`) vì `icacls <dir> /inheritance:r` trả
   **exit 5 `Access is denied`** ⇒ `save()` trả `Protection::ProfileDefault`. Đó là **fallback
   trung thực**, không phải lỗi logic — nhưng cần quyết định trước khi commit: hoặc ghi rõ yêu cầu
   quyền cho CI, hoặc gate test theo khả năng đổi ACL. Thêm hai điều kiện đã biết: assertion dựa
   vào chữ `SYSTEM` **tiếng Anh**, và **không** test nào ép nhánh `icacls` thất bại.
3. **`/key <giá-trị>`**: vẫn chỉ **token đầu tiên** được lưu (key chứa dấu cách sẽ bị cắt). Hạn
   chế này nay **đã được ghi trong app** (`/help` `view.rs:131`–`133` + notice
   `controller.rs:753`–`754`), nên chỉ còn là hạn chế đã biết.
4. **Chưa có ca PTY cho `/key`.** ConPTY cần console mà sandbox build không có. Mask đã được canh
   ở ba tầng không cần console (editor, controller, khung hình đã vẽ), nhưng **chưa** có bằng
   chứng transcript thật rằng terminal nhận đúng `•`.
5. **Paid smoke chưa chạy**: môi trường không có credential/budget, nên chưa có lượt gọi provider
   thật nào bằng key lưu trong app. Đường CLI thật đọc **file** credential đã được đo end-to-end
   với endpoint loopback chết (evidence 11.4), nhưng đó không phải xác thực.
6. **Clippy/gate/PTY chưa chạy cho feature này**: `cargo clippy --workspace --all-targets --locked
   -- -D warnings`, `Verify-HaLaunch.ps1 -Json`, `Invoke-HaPtyAcceptance.ps1`.

Đã đóng trong lượt này (không còn là open item): **Esc huỷ secret entry** (implement + test bấm
phím thật ở editor và controller), **thứ tự siết quyền** (`write_staged` tạo file với
`mode(0o600)` trên Unix + test mode file staging), **test controller cho cả chuỗi `/key`**
(`k01_key_entry_masks_saves_clears_the_gate_and_admits_the_next_message`), **mô tả `/key <value>`
trong app**, **đường dẫn `<data dir>/private/`**, **ACL Windows + `Protection` + dòng `/status`**,
và **mask ở tầng khung hình đã vẽ** (`tui::tests::k01_a_secret_buffer_is_painted_as_a_mask`).

### 13.6. Next action chính xác

1. Xác nhận cây đã dừng đổi: `git status --porcelain` và mtime của
   `crates/harness-cli/src/interactive/*.rs`; đối chiếu hash với evidence 11.2.
2. `cargo clippy --workspace --all-targets --locked -- -D warnings` và
   `cargo test --release -p harness-cli --bin ha --locked` → kỳ vọng `153 passed; 0 failed` trong
   phiên có quyền đổi ACL (nếu chạy ở môi trường bị chặn ACL, đọc 13.5 mục 2 trước khi kết luận).
3. `pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json` → `passed: true, failures: []`,
   rồi `pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1 -TimeoutSeconds 900` →
   `PTY_EXIT: 0`.
4. Quyết định 13.5 mục 2 (yêu cầu quyền ACL cho CI / gate test theo khả năng đổi ACL) trước khi
   commit, vì đó là điều kiện môi trường ảnh hưởng tới gate.
5. Commit feature + docs thành một commit, rồi cập nhật lại mục 9 và 13.3 theo bản cuối cùng.

## 14. Sau CP-D: `/more`, phím cuộn, con lăn chuột và hai flake loopback (lượt này)

Trạng thái: **implemented + committed + pushed**. Ba commit trên `origin/master`, đo ngày
20/09/2026: `1340129` (`/more` + panel cuộn, 13:36) → `cce5c13` (alternate scroll `1007`, 13:45)
→ `5883d59` (hai flake loopback, 13:58). `HEAD` = `origin/master` = `5883d59` lúc soạn mục này.
Hợp đồng ở SPEC mục 3e; số đo, test và khoảng trống ở evidence mục 12.

**Đính chính mục 13 ở trên (đã cũ, đừng đọc theo):** mục 13 và 13.5 mục 1 nói feature `/key`
"chưa có commit nào, `credentials.rs` untracked, `HEAD` vẫn `aeaf7b7`". Điều đó **không còn
đúng**: `/key` đã được commit ở `385a98f` và đi qua các commit sau đó; `HEAD` nay là `5883d59`.
Mục 13 giữ nguyên như bản ghi lịch sử của lượt đó, không sửa lại.

### 14.1. Việc đã xong

- **`/more`** (`controller.rs`, `view.rs`, `input.rs`): mở lại transcript gần nhất trong **đúng
  panel overlay** mà `/help` dùng, và **mở ở dòng đầu** — viewport chỉ giữ đuôi câu trả lời, nên
  phần bị mất chính là phần đầu. Buffer hồi tưởng **chặn 500 dòng** (`RECALL_LINES`,
  `controller.rs:1044`), ghi ở đúng ba điểm ghi transcript: `flush_stream`,
  `flush_stream_overflow`, `push_history`; nội dung đang stream (`pending_text`) cũng được nối vào
  khi mở panel. `SLASH_COMMANDS` nay **9** phần tử (`input.rs:670`) và `/more` có trong
  `view::help_lines()` (`view.rs:135`). Plain mode in các dòng như mọi lệnh tham chiếu khác.
- **Panel cuộn được** (`tui/widgets/help.rs`, `controller.rs:331`–`337`): `PageUp`/`PageDown` 8
  dòng, `Home` về dòng đầu, `End` về dòng cuối; offset được **kẹp trong `help::render`** vì đó là
  chỗ duy nhất biết bao nhiêu dòng vừa; viền dưới báo `còn N dòng` / `dòng x/y` / `cuối` cộng
  `Esc đóng`. Phím cuộn **không** ghi history và `Esc` vẫn đóng panel, nên acceptance U09 vẫn đúng.
- **Con lăn chuột = alternate scroll `DECSET 1007`** (`terminal.rs`): bật `ESC [ ? 1007 h` khi vào
  raw mode, tắt ở `RawModeGuard::drop` **và** trong panic hook; `ModeControl` có thêm
  `enable_alternate_scroll`/`disable_alternate_scroll`. **Cố ý không** dùng mouse capture
  (`1000`/`1002`/`1006`): capture lấy con lăn khỏi terminal và **xoá scrollback** — mà scrollback
  là nơi app commit mọi thứ (`insert_before`), nên capture sẽ đổi "mất phần đầu trong viewport"
  lấy "mất luôn phần đầu trong scrollback".
- **Hai flake loopback đã sửa gốc**: body SSE fixture nay **chunked** (`Transfer-Encoding:
  chunked` + chunk cuối `0\r\n\r\n`, `streaming.rs:381`–`396`) thay vì được kết thúc bằng đóng kết
  nối — trước đó client không phân biệt được body xong với kết nối bị reset; socket vẫn được giữ
  500 ms sau chunk cuối. `completion_service_resume_flow` nay chạy con **tối đa hai lần**
  (`service_completion_tests.rs:20`–`43`), và **thất bại sau lần thử lại vẫn được báo** — không
  nới assertion nào.

### 14.2. File đã đụng

| File | Việc |
|---|---|
| `crates/harness-cli/src/interactive/controller.rs` | nhánh `/more`, `reference()` dùng chung, buffer `recall` + `remember()` (500 dòng), nhánh phím cuộn của overlay, **hai** trong ba test `k04_*` (`k04_more_opens_...`, `k04_a_long_overlay_is_readable_...`) |
| `crates/harness-cli/src/interactive/view.rs` | thêm dòng `/more` vào `help_lines()` |
| `crates/harness-cli/src/interactive/input.rs` | `SLASH_COMMANDS` lên 9; `Overlay` có `scroll`, `scroll_by`/`scroll_home`/`scroll_end`, `scroll_overlay`/`scroll_overlay_to` |
| `crates/harness-cli/src/interactive/events.rs` | `Modal::Overlay` mang thêm `scroll` |
| `crates/harness-cli/src/interactive/tui/widgets/help.rs` | kẹp offset theo số dòng vừa; viền dưới báo trạng thái cuộn |
| `crates/harness-cli/src/interactive/tui/widgets/mod.rs` | truyền `scroll` xuống `help::render` |
| `crates/harness-cli/src/interactive/terminal.rs` | `ModeControl::{enable,disable}_alternate_scroll`; bật/tắt `1007` (drop + panic hook); test I08 assert đủ thứ tự mode |
| `crates/harness-cli/src/interactive/tui/mod.rs` | test `k04_a_long_streamed_answer_keeps_its_end_visible_and_its_start_in_scrollback` |
| `crates/harness-providers/src/streaming.rs` | fixture SSE chunked + chunk cuối; giữ socket 500 ms |
| `crates/harness-cli/src/interactive/service_completion_tests.rs` | `completion_service_resume_flow` chạy con tối đa 2 lần |
| `docs/specs/HA_TUI.vi.md` (mục 3e), `docs/evidence/HA_TUI.vi.md` (mục 4b, 12), `docs/OPERATOR_GUIDE.vi.md` (mục 12.6), `docs/handoffs/HA_TUI.vi.md` (mục này) | tài liệu lượt này |

Không thêm dependency nào; `Cargo.toml`/`Cargo.lock` không đổi.

### 14.3. Số đo (có ngày, không phải hằng số)

```text
cargo test --release -p harness-cli --bin ha --locked      -> 163 passed; 0 failed
                                                              (5 lần liên tiếp, cả 5 xanh; 20/09/2026)
cargo test -p harness-providers --locked                   -> 10 lần liên tiếp xanh
cargo clippy --workspace --all-targets --locked -- -D warnings -> sạch
cargo fmt --all -- --check                                 -> sạch
pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1          -> GATE_OK: every required step passed (exit 0)
ha chat --headless --prompt "say ok"                       -> model trả lời qua binary release đã cài
C:\Users\duong\.cargo\bin\ha.exe SHA-256
  cae17188122301a6f5e392cce8178fd4b4c042324b943661fac4100e20dd0d37
```

Flake: `providers-streaming` khoảng **1 đỏ / 8 lần chạy** trước khi sửa → **0 đỏ trong 10 lần
liên tiếp** sau khi sửa; `completion_service_resume_flow` khoảng **1 đỏ / 3 lần chạy** cả suite →
**0 đỏ trong 5 lần liên tiếp**. Vì cây đang được writer khác sửa song song, `163` và `10` là **số
đo của ngày 20/09/2026**: lần sau chạy lại lệnh rồi đọc số mới, đừng trích lại.

### 14.4. Việc còn lại / open items

1. **TUI, `/more`, phím cuộn và con lăn chưa từng được lái trong console thật.** ConPTY cần
   console mà sandbox build không có; ba test `k04_*` khẳng định trên state của controller/editor,
   **không** phải terminal. **Không** có ca PTY nào gõ `/more`, `PageUp`, `PageDown`, `Home` hay
   `End`.
2. **`1007` là hành vi phía terminal — không test nào chứng minh được.** Chưa đo trên Windows
   Terminal hay emulator nào khác. Việc còn lại: một lần lái tay trên console thật (mở answer dài,
   lăn chuột, xác nhận scrollback cuộn và không có giao thức chuột nào được gửi).
3. **Mâu thuẫn chữ trong UI, chưa sửa (việc code, không phải việc tài liệu).** Viền dưới panel mở
   đầu bằng `↑↓/PgUp/PgDn cuộn` (`help.rs:36`/`38`/`41`), nhưng `↑`/`↓` **không** cuộn panel:
   `controller.rs:331`–`337` chỉ bắt `PageUp`/`PageDown`/`Home`/`End`, nên `↑`/`↓` rơi xuống editor
   và **sửa buffer soạn thảo** (recall history / di chuyển theo hàng, `input.rs:443`–`460`) trong
   khi panel vẫn mở; `Home`/`End` thì **có** tác dụng nhưng không được nhắc. Không test nào assert
   chuỗi gợi ý. Hoặc bỏ `↑↓` khỏi gợi ý, hoặc cho `↑`/`↓` cuộn panel — cần người giao việc chọn.
4. **`/more` không phải lịch sử đầy đủ**: cắt ở 500 dòng logic gần nhất; không test nào đo đúng
   ngưỡng 500.
5. **Một lần gate đỏ thoáng qua chưa giải thích được**: `GATE_FAILED: discovery:i13_... ,
   discovery:i09_...` trong khi hai selector **có** trong file và lệnh discovery chạy trực tiếp trả
   exit 0; lần chạy sau `GATE_OK` (exit 0). Ghi làm quan sát, **không** kết luận là đã sửa.
6. **Khoảng trống cũ vẫn còn**: paid provider smoke, cài thật + User PATH, Linux, conhost cũ,
   ACL Windows chỉ đo trên một máy (mục 8 evidence, mục 13.5).

### 14.5. Next action chính xác

1. Trên **console thật** (không phải sandbox build): `ha chat`, hỏi một câu trả lời dài, rồi
   `/more` — xác nhận panel mở ở dòng đầu, `PgUp`/`PgDn`/`Home`/`End` cuộn, viền dưới đổi chữ, và
   `Esc` đóng mà **không** ghi gì vào transcript. Đây là điều duy nhất ba test `k04_*` không chứng
   minh được; transcript nên lưu lại như bằng chứng.
2. Trên cùng console đó, lăn chuột khi panel đang mở và khi đang stream: scrollback phải cuộn và
   app **không** được gửi giao thức chuột nào. Nếu terminal không tôn trọng `1007`, ghi lại đúng
   terminal + version — đó là dữ liệu, không phải lỗi.
3. Quyết định open item 14.4 mục 3 (bỏ `↑↓` khỏi gợi ý hay cho `↑`/`↓` cuộn panel) rồi sửa **một**
   trong hai, kèm test assert chuỗi gợi ý.
4. Giữ nguyên hai flake đã sửa: chạy `cargo test -p harness-providers --locked` và
   `cargo test --release -p harness-cli --bin ha --locked` vài lần liên tiếp; **số mới ghi đè số
   cũ** trong evidence mục 12.2 và mục này, và **không** nới assertion nếu có lần đỏ.

## 15. Hai lỗi nhìn thấy trên màn hình thật (screenshot của người giao việc)

Người giao việc gửi hai ảnh chụp TUI đang chạy. Cả hai lỗi **không** test nào bắt được: chúng chỉ
hiện ra khi nhìn khung hình vẽ thật. Nội dung trong history vẫn đúng — lỗi nằm ở renderer.

### 15.1. Lỗi 1 — `**đậm**` in nguyên dấu sao, dòng dài bị cắt ở mép console

**Triệu chứng (ảnh):** câu trả lời hiện `**Tool call:**` với đủ bốn dấu `*`; những dòng dài bị
terminal cắt cụt, chữ mất hẳn chứ không xuống dòng.

**Nguyên nhân (đọc code, không suy đoán):** `tui/markdown.rs::render` chỉ có nhánh cho heading,
bullet và fence — **không** có nhánh nào cho emphasis; và nó trả về **đúng một** `Line` cho mỗi
dòng văn bản, không đo bề rộng, nên terminal tự cắt phần vượt.

**Đã sửa:**

| Việc | Chỗ sửa |
|---|---|
| `**…**` → `Modifier::BOLD`, bỏ marker; `` ` `` giữ nguyên vì đó là ký tự model viết | `markdown.rs::inline_spans` (mới) |
| Mọi block đi qua `wrap_spans(spans, width)`, `width` từ `area.width` | `markdown.rs::render`, `history.rs`, `composer.rs::render_live` |
| Ngắt tại khoảng trắng, không cắt giữa từ; token không có khoảng trắng vẫn ngắt được | `markdown.rs::break_point`, `wrap_spans` |
| Marker chưa đóng (`a ** b`) và `****` là **chữ**, in đúng một lần, đúng vị trí | `inline_spans`: nhìn trước bằng `after.find("**").filter(\|at\| *at > 0)` |

Bất biến giữ nguyên: **không ký tự nào bị mất**; thứ duy nhất bị bỏ là khoảng trắng ngay tại điểm
ngắt dòng (chính chỗ ngắt đã thay nó) và marker `**` đã thành style.

**Test mới (8 ca trong `markdown.rs`, tất cả xanh):** `t04_emphasis_markers_become_styling_instead_of_asterisks`,
`t04_markers_with_nothing_between_them_are_text`, `t04_an_unclosed_marker_is_text`,
`t04_long_rows_wrap_instead_of_being_clipped`, `t04_wrapping_breaks_at_spaces`,
`t04_a_word_wider_than_the_row_still_wraps`, `t04_wrapping_keeps_every_word_of_a_long_paragraph`,
cùng ca cũ `t04_markdown_rendering_keeps_every_visible_character`.

### 15.2. Lỗi 2 — panel duyệt in trùng khối proposal, gợi ý chỉ sai chỗ

**Triệu chứng (ảnh):** khối `[approval] …` hiện **hai lần** — một lần trong scrollback phía trên
viewport, một lần trong panel — nên panel trông như bản sao lỗi của transcript. Viền composer ghi
`trả lời panel ở trên`, không nói phím nào.

**Nguyên nhân:** `controller.rs` đẩy `HistoryItem::Approval` vào history **ngay khi request tới**,
rồi `layout::plan` lại vẽ panel từ `pending_approval` — cùng dữ liệu, hai chỗ vẽ. Và
`composer::hint` trả **một** câu cho mọi loại modal.

**Đã sửa:**

1. Ở TUI, panel là chỗ **duy nhất** in proposal: `push_history(HistoryItem::Approval)` chỉ chạy khi
   `self.plain`. Chế độ plain **không** có panel nên ở đó history **vẫn** giữ khối — U20 không đổi.
2. `composer::hint` khớp theo `state.modal`: approval → `y chạy · n từ chối`; picker →
   `↑↓ · Enter · Esc`; overlay → `PgUp/PgDn · Home/End · Esc`. Đây cũng là câu trả lời cho open item
   14.4 mục 3 theo hướng **giữ** `↑↓` cho picker (nơi `↑`/`↓` thật sự có tác dụng,
   `controller.rs` xử lý picker bằng mũi tên) và **không** nhắc `↑↓` cho overlay — đúng như
   `help.rs:33`–`37` đã ghi.

**Test mới:** `controller::tests::t06_the_open_panel_is_the_only_place_the_proposal_is_shown`,
`controller::tests::t06_a_plain_session_still_records_the_proposal_it_cannot_panel`,
`tui::tests::t06_a_pending_approval_frame_holds_one_copy_of_the_proposal` (đọc **khung vẽ thật**
qua `ScriptedRenderer`, đếm `path=src/parser.rs` xuất hiện **đúng 1** lần),
`composer::tests::t03_the_hint_names_the_keys_of_the_panel_that_is_open`.

### 15.3. Số đo của lượt này

```text
cargo test -p harness-cli --bin ha --locked              -> 188 passed; 0 failed
cargo test -p harness-cli --test interactive_launch --locked -- --test-threads=1
                                                          -> 18 passed; 0 failed  (3 lần liên tiếp)
cargo clippy --workspace --all-targets --locked -- -D warnings -> sạch
cargo fmt --all -- --check                               -> sạch
pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json  -> passed: true, failures: []  (lần chạy đầu)
pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1 -> PTY_EXIT: 0, 16 passed; 0 failed (22.85 s)
cài thật (release, -SkipBuild không dùng):
  pwsh -NoProfile -File scripts/Install-Ha.ps1           -> INSTALL_EXIT 0
  C:\Users\duong\.cargo\bin\ha.exe SHA-256
    59c3b88025483bd41ed3e91f20ef6dc434a8a32401e2166250c6ab67062a2c18
  PTY i14 trên artifact **mới cài**: PTY_EXIT: 0, ca xanh
paid smoke qua binary đã cài, prompt buộc markdown:
  -> SMOKE_EXIT: 0 after 2 s · SMOKE_STOP: final · SMOKE_RESPONSE_LENGTH: 710
     model trả về **bold** + một đoạn văn dài -> xác nhận ** là chữ model thật viết
```

Hai lần chạy PTY giữa lượt **có đỏ**, và cả hai đều không phải hồi quy của bản sửa:

| Lần | Ca đỏ | Đọc transcript | Kết luận |
|---|---|---|---|
| 1 | `t07_pty_plain_flag` | Lúc đó cây **chưa build được** (`Modal` chưa import trong `composer.rs`) — lỗi của chính lượt này, đã sửa | Lỗi thật, đã sửa |
| 2 | `i14_the_installed_artifact_opens_the_app_in_a_real_terminal` | Assert cuối `transcript.ends_with("\r\n")`; chạy **một mình** ca đó `PTY_EXIT: 0` | Flake console: lần đọc cuối của PTY giành với lúc tiến trình thoát. **Không** nới assertion |

Lần chạy đủ 16 ca ngay sau đó xanh hết, và gate lần chạy đầu sau khi vá `i13` cũng xanh.

### 15.4. Flake thứ ba của loopback — `i13_resume_...` (đã sửa)

Gate lần chạy **thứ hai** của lượt này đỏ ở `acceptance-launch`, ca chạy 40.77 s:

```text
provider_protocol: provider_protocol: provider_protocol: provider request failed:
  error sending request for url (http://127.0.0.1:59208/chat/completions)
left: 1   right: 0        (interactive_launch.rs:660)
```

Chạy **một mình** ca đó: xanh **4/4 lần liên tiếp**. Nghĩa là đỏ chỉ xuất hiện dưới tải cả suite —
đúng họ với ba flake loopback đã sửa trước đó (SPEC 3e.4), không phải hồi quy.

**Sửa, cùng cách đã dùng ở `phase_p2`:** `i13` chạy `ha chat --headless --resume` như một **tiến
trình thật**, và tiến trình thật **không** có bậc thử lại nào; fixture `sse_fixture_multi` thì vẫn
nhận kết nối nên **không** bị tiêu mất response. Vì vậy bậc thử lại được đặt ở phía test:

- `LOOPBACK_ATTEMPTS = 10`, backoff `50 ms × 2^min(attempt, 6)` — cùng công thức đã dùng.
- **Chỉ** thử lại khi `code() != 0` **và** stderr chứa đúng `error sending request for url`. Một
  lỗi thật (protocol, 4xx, JSON hỏng) vẫn đỏ ngay lần đầu.
- Fixture **không** đổi: nó đã đếm đúng "request thật" và bỏ qua probe, nên `requests.len() == 2`
  vẫn là oracle nguyên vẹn.

Số đo sau khi sửa: **3/3 lần chạy liên tiếp** cả suite `interactive_launch` xanh (34.07 s, 34.49 s,
34.73 s), rồi gate `failures: []`.

## 16. Gate phê duyệt: đọc không hỏi, ghi vẫn hỏi

### 16.1. Việc đã xong

Người giao việc gặp panel duyệt cho `ListFiles: list .` khi chat, và hỏi đúng: Claude Code với
Codex CLI không hỏi vậy. Đã **research trước, code sau**: tra tài liệu gốc Anthropic
([permissions](https://code.claude.com/docs/en/permissions),
[permission modes](https://code.claude.com/docs/en/permission-modes)) và OpenAI
([approvals](https://mintlify.wiki/openai/codex/concepts/approvals),
[sandboxing](https://mintlify.wiki/openai/codex/concepts/sandboxing)), rồi đọc thẳng
`claude --help` (2.1.218), `codex --help` (0.149.0), `~/.claude/settings.json` và
`~/.codex/config.toml` trên máy này. Bảng so sánh ở SPEC 3g.1.

Chốt 4 mục với người giao việc **trước khi viết code**, rồi làm đủ 4:

1. 5 action chỉ-đọc (`read_file`, `list_files`, `search_text`, `git_status`, `git_diff`) được
   miễn hỏi trong workspace — allowlist ở `ToolKind::is_read_only`, chứ không suy từ tên.
2. Danh sách bảo vệ **không** bị nới: `.env`, `.env.*`, `.git`, `.harness`, tên chứa
   `credential`/`secret`/`password`/`private_key`, đuôi `.pem`/`.key`/`.p12`/`.pfx`/`.clixml`,
   và mọi đường ra ngoài workspace — tất cả bị từ chối ở `prepare`, **trước khi** proposal tồn
   tại. Lượt này thêm test **chứng minh** điều đó, không đổi cơ chế.
3. Ghi/patch/lệnh vẫn hỏi từng lần; test liệt kê đủ 10 kind nên thêm kind mới là đỏ ngay.
4. Panel có `a` = chạy action này **và** cho phép đọc cả lượt. Cờ là `AtomicBool` trên object,
   `finish_run()` thu hồi, thanh trạng thái hiện `· reads tự động`, và mỗi lần miễn hỏi ghi một
   dòng `[info] read-only, allowed for this turn: ...`. **Không** làm mục ghi policy vĩnh viễn.

**Quyết định thiết kế quan trọng nhất:** thi hành nằm ở **một** chỗ, `ChannelApprovalGate`,
không phải driver và không phải UI. Nhờ vậy plain mode được miễn hỏi y hệt mà không phải viết
thêm dòng nào, và `ApprovalMode`/trait `ApprovalGate` không đổi hình dạng — chỉ một chỗ dựng
proposal phải sửa.

### 16.2. File đã đụng

```text
crates/harness-tools/src/contracts.rs                 ToolKind::is_read_only (allowlist 5)
crates/harness-tools/src/turn_driver.rs               ApprovalProposal::read_only + doc trait
crates/harness-tools/src/service.rs                   2 test mới (kind + đường dẫn bị chặn)
crates/harness-cli/src/interactive/events.rs          ApprovalRequired.read_only, Modal::Approval.read_only, UiState.reads_for_run
crates/harness-cli/src/interactive/service.rs         ApprovalDecision::GrantReadsForRun, cổng tự duyệt, grant/clear, 4 test
crates/harness-cli/src/interactive/controller.rs      phím a, nhãn transcript, finish_run thu hồi, 3 test
crates/harness-cli/src/interactive/tui/widgets/approval.rs  PANEL_ROWS=7 + dòng gợi ý a
crates/harness-cli/src/interactive/tui/widgets/composer.rs  hint theo read_only
crates/harness-cli/src/interactive/tui/widgets/status.rs    badge "reads tự động"
crates/harness-cli/src/interactive/tui/layout.rs      modal_rows dùng PANEL_ROWS
docs/OPERATOR_GUIDE.vi.md                             mục "Phê duyệt: khi nào bị hỏi, và khi nào không"
docs/specs/HA_TUI.vi.md                               §3g (so sánh + ranh giới + test)
docs/evidence/HA_TUI.vi.md                            §14 (số đo + không được chứng minh)
```

### 16.3. Số đo

```text
cargo test -p harness-cli --bin ha --locked                    -> 195 passed; 0 failed
cargo test -p harness-tools --locked                           -> 6 passed; 0 failed
cargo clippy --workspace --all-targets --locked -- -D warnings -> sạch
pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json        -> passed: true, failures: []
pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1       -> PTY_EXIT: 0, 16 passed; 0 failed
```

Không có lần đỏ nào trong lượt này. Gate xanh **lần chạy đầu**.

### 16.4. Việc còn lại / open items

1. **Chưa có ca PTY bấm `a`.** Đường `a` được chứng minh ở controller + cổng, chưa trên ConPTY.
2. **Panel 7 hàng ở viewport thấp nhất chưa đo bằng mắt.** `PANEL_ROWS` bị `modal_rows` cắt theo
   chỗ trống, nên ở 10 hàng dòng cuối có thể không hiện; ngưỡng hỏng dịch lên **1 hàng** so với
   trước. Cần một lần lái tay trên console thật ở console thấp.
3. **Hai cửa sổ `ha` trên cùng project có cổng riêng** — `a` ở cửa sổ này không mở cổng ở cửa sổ
   kia. Đúng thiết kế (trạng thái một tiến trình), nhưng chưa có test và chưa đo thật.
4. **Bộ 5 kind chỉ-đọc là allowlist tôi chọn**, hẹp hơn nhiều so với bộ lệnh shell chỉ-đọc dựng
   sẵn của Claude Code. Muốn rộng hơn thì phải mở đường chạy shell chỉ-đọc — việc riêng, cần
   người giao việc quyết.
5. Open item cũ **vẫn nguyên**: paid smoke, cài thật + User PATH, Linux, conhost cũ.

### 16.5. Next action chính xác

1. Trên console thật: `ha chat`, hỏi một câu khiến model **đọc** file/list thư mục, xác nhận panel
   hiện với dòng `a cho phép mọi thao tác chỉ-đọc trong lượt này`, bấm `a`, rồi xác nhận các thao
   tác đọc sau đó **không** hỏi nữa và thanh trạng thái hiện `· reads tự động`. Lưu transcript.
2. Trong cùng lượt đó, xác nhận một `apply_patch` (hoặc lệnh) **vẫn** hỏi, để thấy ranh giới bằng
   mắt chứ không chỉ bằng test.
3. Ở cùng console **thấp** (dưới ~20 hàng), mở lại panel và xem dòng `a` có bị cắt không; nếu có,
   hoặc gộp gợi ý `a` vào dòng `y ... n ...`, hoặc cho panel biết chỗ trống thật.
4. Nếu muốn: thêm ca PTY gõ `a` (sửa `scripts/Invoke-HaPtyAcceptance.ps1` chỉ cần nêu lại con số 16
   thành 17) — hiện `a` chưa từng đi qua ConPTY.

### 16.6. Nhánh việc khác đang chạy song song: memory dài hạn

Memory trong chat được sửa ở một luồng riêng, không thuộc T01–T08, nên số đo của nó nằm ở
[handoff memory](MEMORY_RECALL.vi.md) chứ không ở đây. Trạng thái tại lúc viết: commit `eb32a4d` đã
đóng cả tám phát hiện M1–M8 của lần review đối kháng (§7.7 của handoff đó), `d881e4f` đóng nốt việc
còn lại — bản ghi lượt giữ đủ câu trả lời thay vì 200 ký tự đầu, và ngân sách memory được chia đều
thay vì khối dài nuốt hết (§7.9) — và memory được mở rộng từ "chỉ dẫn người dùng" sang "nhật ký hội
thoại", quyết định hợp đồng ghi ở §19 của `docs/MEMORY_AND_CONTINUITY.vi.md`. Hai chỗ giao với lượt
này: `HA_MEMORY=on` là điều kiện để thấy bất kỳ dòng `memory:` nào trong transcript, và gate phê
duyệt ở §16 không đổi vì memory không đi qua `ApprovalGate`.

## 17. `a` = cho phép cả lượt, cho **mọi** hành động (21/09/2026)

**§16.4 mục 1 và §16.5 bước 1–2 đã bị mục này thay thế.** Người giao việc gửi ảnh chụp panel
`RunProcess: run git log -1 --stat --format=fuller` và báo bị hỏi lặp từng lệnh; nguyên nhân là
`a` cũ chỉ phủ 5 kind chỉ-đọc nên `run_process` không có phím nào để thoát. Quyết định: `a` phủ
**mọi** kind trong **một lượt** (không thêm chế độ tin cậy vĩnh viễn, không phân loại lại shell
chỉ-đọc).

Hợp đồng, danh sách đổi tên API, test và ranh giới bằng chứng: [SPEC 3j](../specs/HA_TUI.vi.md).
Tóm tắt cho người mở lại: `ApprovalDecision::GrantForRun`, `SessionPort::{grant,revoke}_run_approval`,
`ChannelApprovalGate::{grant_for_run, clear_grant_for_run, granted_for_run}`, `UiState::granted_for_run`,
thanh trạng thái `· tự động cả lượt`, nhãn transcript `granted (every action allowed for this turn)`.

Việc còn lại của lượt này:

1. **Commit `a5ac2e2` KHÔNG build được, và nó đã được push.** Writer song song (§12) commit cây
   nguồn **giữa lúc** đổi tên nhiều file của lượt này, nên bản đã commit có `service.rs` mang tên
   mới (`ApprovalDecision::GrantForRun`, `SessionPort::grant_run_approval`) trong khi
   `controller.rs` vẫn dùng tên cũ (`GrantReadsForRun`, `approve_reads_for_run`) — tức
   `cargo build` ở `a5ac2e2` đỏ. Cây làm việc hiện tại đã hoàn tất việc đổi tên và xanh test;
   cần **một commit sửa** (không amend được vì đã push). Đây là bài học cho §12: đừng commit cây
   khi một writer khác đang giữa một thay đổi nhiều file.
2. **Chưa có ca PTY bấm `a`** (§16.4 mục 1 vẫn đúng): đường `a` được chứng minh ở cổng thật +
   khung hình đã vẽ, chưa trên ConPTY. Nếu thêm, `scripts/Invoke-HaPtyAcceptance.ps1` phải nêu lại
   con số 16 thành 17.
3. **Chưa đo bằng mắt trên console thật**: panel giờ **luôn** 5 hàng nội dung (thêm dòng `a` cho
   mọi panel, không chỉ panel chỉ-đọc), nên panel ghi trước kia 4 hàng → nay 5. Ở console thấp cần
   một lần lái tay (cùng việc §16.4 mục 2).
4. **Việc riêng chưa làm, đã ghi ở SPEC 3j.5**: `RunProcess`/`RunShell` không được
   `validate_workspace_action` kiểm gì và không có sandbox filesystem/network, nên sau khi bấm `a`
   một lệnh do model đề xuất chạy không hỏi. Đó là quyền người giao việc đã chọn; muốn hẹp hơn thì
   phải phân loại lệnh shell chỉ-đọc (open item §16.4 mục 4, vẫn chưa làm).
