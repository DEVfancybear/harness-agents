# Handoff HA_TUI — nâng `ha` thành TUI terminal (T01–T08)

Cập nhật: **sau CP-A (T01–T02)**. SPEC: [specs/HA_TUI.vi.md](../specs/HA_TUI.vi.md) ·
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

- Nhánh `master`, base `c97c7dd` (plan T), `origin/master` cùng revision lúc bắt đầu.
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

## 4. Việc đang dở và ranh giới cuối cùng thành công

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
| `crates/harness-cli/src/interactive/controller.rs` | T02 reducer + test T02 |
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

## 9. Next action chính xác

```text
pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json
```

Kỳ vọng: `passed: true`, `failures: []`. Nếu đỏ ở `docs`, kiểm tra link trong
`docs/specs/HA_TUI.vi.md` (mọi link `../…` phải trỏ tới file đã tồn tại).

Sau đó:

```text
pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1 -TimeoutSeconds 900
```

Kỳ vọng: `PTY_EXIT: 0` và "10 passed; 0 failed" **trên TUI mặc định** (lần chạy trước
diễn ra trước khi TUI được nối vào `app::run`).

## 10. Blocked on

Không. Quyền commit/push, paid smoke và cài thật đã được cấp.

## 11. Không lặp lại

- Không thêm lại POC T01 hay cờ `--tui-spike`.
- Không bật `scrolling-regions` (đã đo và loại).
- Không chạy lại `cargo metadata` không `--locked` để "sửa" lockfile: lockfile đã đúng,
  `cargo build --locked` xanh.
- Không chạy lại `Invoke-HaPtyAcceptance.ps1` với filter `t01` (các ca đó thuộc POC đã xoá).
