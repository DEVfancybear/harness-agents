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

Từ lượt này, `DEEPSEEK_API_KEY` (hoặc `HA_API_KEY`) là **toàn bộ** cấu hình provider:
endpoint và model mặc định theo giá trị `DeepSeek` công bố
(<https://api-docs.deepseek.com/> → `https://api.deepseek.com`, `deepseek-flash`), biến
do người dùng đặt luôn thắng. Smoke cũng theo cùng quy tắc, nên chạy paid smoke chỉ cần:

```powershell
$env:DEEPSEEK_API_KEY = '<key>'
pwsh -NoProfile -File scripts/Smoke-HaProvider.ps1
```

Test canh hợp đồng mới: `t08_one_deepseek_key_is_a_complete_provider_setup` (Rust) và
self test của smoke (đường mặc định + đường override).

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

## 13. `/key` — lưu API key trong app (lượt này, **chưa commit**)

Trạng thái thật: **implemented trong working tree, unit test xanh (153 test, 0 failed trong phiên
có quyền đổi ACL), chưa có commit nào.** `HEAD` vẫn là `aeaf7b7`. Hợp đồng ở SPEC mục 3d, số đo và
`not_run` ở evidence mục 11 (11.2 ghi hash, phép đo ACL và khác biệt môi trường).

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
- `/help` có `/key` và `/key <value>`; `SLASH_COMMANDS` **8** phần tử; `/status` + `/model` in
  **nguồn** credential (`credential from environment variable ...` / `credential from saved
  file ...`) và không bao giờ in giá trị; `SessionEvent::ProviderConfigured { source }` chỉ mang
  nguồn.
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
git status --porcelain                 # cây đang dở; credentials.rs untracked
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

1. **Cây chưa commit.** Toàn bộ feature `/key` nằm trong working tree; `credentials.rs` còn
   **untracked**; `HEAD` vẫn `aeaf7b7`. Cây đã **ngừng đổi** ở bản cuối này (hash ở evidence
   11.2), nhưng lượt này từng có writer song song sửa `interactive/*` (`service.rs` 1431 → 1489
   dòng, `credentials.rs` 476 → hơn 700 dòng) — xác nhận `git status` và mtime trước khi commit.
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
