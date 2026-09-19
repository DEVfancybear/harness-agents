# Plan cho DeepSeek: nâng `ha` thành TUI terminal (track T01–T08)

**19/09/2026 · Planning only · Feature track HA_TUI T01–T08. Chưa có dòng code T nào.**

[Prompt giao DeepSeek](HA_TUI_PROMPT.vi.md) · [Track khởi động H01–H08](HA_LAUNCH_PLAN.vi.md) · [SPEC HA_LAUNCH](specs/HA_LAUNCH.vi.md) · [Handoff HA_LAUNCH](handoffs/HA_LAUNCH.vi.md) · [Bản đồ tích hợp](implementation-next/INTEGRATION_MAP.vi.md) · [Templates SPEC/evidence/handoff](implementation-next/TEMPLATES.vi.md)

## 1. Kết quả người dùng muốn

Track H đã làm được: gõ `ha` là vào app tương tác, có streaming, tool gate, approval, resume, cancel, multiline bằng Ctrl-J. Nhưng giao diện hiện là **dòng chữ nối tiếp trên một prompt `> `**: không có thanh trạng thái, không thấy đang ở bước mấy, approval là bốn dòng text, `/resume` là danh sách đánh số, output model không phân biệt được với output tool, paste nhiều dòng bị ép thành một dòng.

Track T nâng đúng app đó thành **TUI kiểu Codex/Claude Code**: phần hội thoại vẫn chảy vào scrollback của terminal (cuộn được bằng chính terminal), còn **đáy màn hình là một vùng cố định** gồm ô soạn thảo nhiều dòng, thanh trạng thái và các panel tạm (approval, chọn session, trợ giúp). Không phải app full-screen chiếm cả màn hình.

```text
Harness Agents 0.1.0                                   ← scrollback (in một lần)
Project: C:\work\my-project    Provider: deepseek-chat via https://api.deepseek.com
Git:     repository at C:\work\my-project
Session: new    Mode: trusted host

> Sửa lỗi parser và chạy tests                         ← history: user
● read_file  src/parser.rs                    ok 12ms  ← history: tool card
● test       cargo test -p parser         failed 3.1s
Tôi thấy lỗi ở dòng 42: thiếu `?` sau parse_expr…      ← history: assistant (đã commit)
[run] done · 3 steps · 2 tool calls · 14.2s
───────────────────────────────────────────────────────── ← từ đây là viewport cố định
Vì vậy tôi sẽ sửa như sau:                              ← live block: text đang stream
┌ > Nhập yêu cầu (Enter gửi · Ctrl-J xuống dòng · /help) ┐
│ > _                                                    │  ← composer (1..8 dòng, tự cao)
└────────────────────────────────────────────────────────┘
 ⠹ running · step 2/8 · tools 2/16 · 00:07   deepseek-chat   session …a1b2c3d4   Ctrl-C hủy
```

Khi có approval, một panel thay chỗ live block; khi gõ `/resume` không tham số, một picker có mũi tên; `/help` là overlay đóng bằng Esc. Đây là **mockup hành vi mục tiêu**, không phải output hiện có. Chi tiết layout, phím và màu ở mục 5.

## 2. Căn cứ source ở HEAD `9c62329`

Khảo sát tại `9c62329` (`docs(ha-launch): measure the loopback flake…`), cây sạch, nhánh `master`. Bảng dưới là **baseline lúc lập plan**; trước mỗi assignment phải đọc lại SPEC/evidence/handoff H, `git status`, và chỉ xử lý gap thật.

| Căn cứ | Hành vi hiện có (đã có test) | Khoảng trống cho TUI |
|---|---|---|
| [`interactive/controller.rs`](../crates/harness-cli/src/interactive/controller.rs) `InteractiveController` | State machine `Booting → Ready/SetupRequired → Running → WaitingApproval/Canceling → Ready`, không chạm terminal, trả `Effect::{WriteLine, WritePartial, RedrawPrompt, Exit}` | Effect là **chuỗi phẳng**: renderer không biết dòng nào là user/tool/assistant/error để tô màu hay dựng card; không có snapshot trạng thái cho thanh status; không có khái niệm modal/picker/overlay |
| [`interactive/app.rs`](../crates/harness-cli/src/interactive/app.rs) `run_loop`/`draw_prompt`/`erase_prompt` | Renderer tự tay: xoá prompt, in dòng, vẽ lại prompt nhiều hàng bằng `MoveUp` + `\r\x1b[{n}C`; poll phím 50 ms; fallback line mode khi raw mode hỏng | Không có layout vùng; mỗi lần vẽ là xoá và in lại; không có status bar; live text in thẳng bằng `WritePartial` nên không tách khỏi vùng nhập |
| [`interactive/terminal.rs`](../crates/harness-cli/src/interactive/terminal.rs) `TerminalBackend`, `CrosstermBackend`, `RawModeGuard`, `map_key` | crossterm `=0.29.0`, raw mode + bracketed paste, guard RAII, fault seam I08 chỉ debug build; map Enter/Ctrl-C/Ctrl-D/Ctrl-J/mũi tên/Home/End/Backspace/Delete/Paste/Resize | Chưa map Esc, Tab, PageUp/PageDown, Ctrl-L, Ctrl-U/W/A/E, Alt+Enter; không có alternate hoặc inline viewport; không có panic hook phục hồi terminal |
| [`interactive/input.rs`](../crates/harness-cli/src/interactive/input.rs) `LineEditor` | Editor đếm theo **ký tự** (tiếng Việt không vỡ), history có draft, multiline bằng `Key::Newline`; `normalize_paste` đổi newline thành space | Paste mất newline (gap đã ghi ở handoff HA_LAUNCH mục 3.3); không có wrap theo độ rộng cột; không có Ctrl-U/W; không có autocomplete slash |
| [`interactive/view.rs`](../crates/harness-cli/src/interactive/view.rs) | Chuỗi thuần: `prompt_lines`, `cursor_cell`, `help_lines`, `tool_line`, `approval_lines`, `run_line`, `short_id` | Không có model cho card/status; đây là điểm giữ lại cho **plain mode** |
| [`interactive/events.rs`](../crates/harness-cli/src/interactive/events.rs) `Key`, `SessionEvent`, `AppPhase` | Từ vựng ổn định; `SessionEvent` có Accepted/TextDelta/ToolStarted/ToolSettled/ApprovalRequired/SessionsListed/Notice/RunTerminal/RecoverableError | Không có `StepStarted` (service **bỏ** `TurnProgress::StepStarted` ở `service.rs:287`), không có sự kiện approval hết hạn, tool card không có thời lượng |
| [`interactive/service.rs`](../crates/harness-cli/src/interactive/service.rs) `AgentSessionService`, `ChannelApprovalGate` | Nối thật vào `TurnDriver`/store/tool gate; approval timeout 5 phút → `ApprovalAnswer::Expired` gửi cho driver | Hết hạn chỉ tới model, **không** có event cho UI nên phase có thể kẹt `WaitingApproval` tới khi `RunTerminal` |
| [`interactive_terminal.rs`](../crates/harness-cli/tests/interactive_terminal.rs) + [`Invoke-HaPtyAcceptance.ps1`](../scripts/Invoke-HaPtyAcceptance.ps1) | 10 ca PTY thật (i01, i05, i06, i07a/b, i08, i12, i13, i14, i21) xanh trong console thật; harness trả lời `ESC[6n` cho ConPTY; ghi transcript | Assertion dựa vào text trong transcript (`> `, `[run]`, `[tool]`); TUI phải giữ được các mốc chữ này hoặc test phải được cập nhật có chủ ý |
| [`Verify-HaLaunch.ps1`](../scripts/Verify-HaLaunch.ps1) | Gate: fmt, clippy `-D warnings`, 13 selector bắt buộc, unit + acceptance + regression P0–P7 serial, installer/release self test, docs | Chưa có bước/selector nào cho TUI |
| Quyết định H03 trong SPEC HA_LAUNCH mục 5 | "Không dùng ratatui/full-screen TUI" | Đó là quyết định **cho phạm vi H03**, không cấm track sau; plan này thay bằng quyết định D1–D2 dưới đây có căn cứ đo được |

Hai luật nền tảng H đã đo và **không đổi ở track T**: một input mỗi session (hội thoại là chuỗi session cùng task), và approval fail-closed (không trả lời = hết hạn = không chạy).

## 3. Quyết định kiến trúc

Các quyết định dưới đây là mặc định của plan. DeepSeek xác nhận D1–D2 bằng spike T01 có số đo; muốn đổi phải ghi lý do vào SPEC, không đổi ngầm.

**D1 — Thư viện: `ratatui = "=0.30.2"` (default features) + `unicode-width` pin đúng version ratatui kéo vào.** Kiểm tra lúc lập plan bằng `cargo info`: ratatui 0.30.2 mặc định dùng `ratatui-crossterm` 0.1.2 với feature `crossterm_0_29`, tức **cùng crossterm 0.29 đã pin**; MIT; MSRV 1.88 < toolchain 1.97. Điều kiện chấp nhận ở T01: `cargo tree -p harness-cli -i crossterm` cho **đúng một** phiên bản; nếu ra hai phiên bản thì dừng và ghi vào SPEC. Không dùng `tui-textarea`/`ratatui-textarea`: editor hiện có đã đúng Unicode và có test, chỉ mở rộng nó.

**D2 — Inline viewport, không alternate screen.** `Terminal::with_options(backend, TerminalOptions { viewport: Viewport::Inline(h) })`; lịch sử hội thoại đẩy vào scrollback bằng `Terminal::insert_before`. Lý do: (a) người dùng cuộn lịch sử bằng terminal, không phải reimplement scroll; (b) transcript PTY vẫn là văn bản tuyến tính nên bộ test i01…i21 và cách đọc evidence còn dùng được; (c) thoát app thì hội thoại vẫn còn trên màn hình như Codex; (d) ít rủi ro ConPTY hơn alternate screen. Full-screen là **ngoài phạm vi** track này.

**D3 — Controller vẫn thuần, đổi output từ chuỗi sang kiểu.** `Effect::WriteLine(String)` → `Effect::History(HistoryItem)`; `Effect::WritePartial` → `Effect::Stream(String)`; thêm `InteractiveController::ui_state(&self, now) -> UiState` là snapshot cho vùng viewport. `HistoryItem::plain_lines()` phải trả **đúng chuỗi cũ** để plain mode và các test scripted hiện có giữ nguyên hành vi; test cũ được đổi assertion sang `plain_lines` một cách cơ học, không nới lỏng.

**D4 — Hai renderer, một controller.** `tui` là mặc định khi raw mode bật được, kích thước ≥ 60 cột × 10 hàng, `TERM != dumb`; ngược lại về **plain mode** (chính `run_loop` hiện tại). Buộc plain bằng `ha chat --plain` hoặc `HA_UI=plain`; lý do rơi về plain in ra stderr một dòng. Line mode (khi raw mode hỏng) giữ nguyên. Không có renderer thứ ba.

**D5 — Giữ mốc chữ trong transcript.** Dòng đầu composer luôn bắt đầu bằng `> `; dòng user trong history là `> <text>`; kết thúc lượt là `[run] …`; tool card plain là `[tool] …`. Nhờ đó transcript PTY vẫn grep được và assertion cũ chỉ cần cập nhật tối thiểu.

**D6 — Kiểm chứng TUI bằng `ratatui::backend::TestBackend` + PTY thật.** Unit test vẽ frame vào `TestBackend` và assert buffer; PTY thật chứng minh trên console. **Không** test TUI bằng cách mock host TUI. Không mock `TurnDriver`/store để chứng minh UI: dùng `FixtureService` có nhãn hoặc HTTP fixture qua adapter thật như H.

**D7 — Không đổi luật đã chốt.** Không thêm đường nào tự cấp approval, không mock fallback, headless không xuất ANSI, exit code 0/1/2 giữ nguyên, các subcommand cũ không đụng.

## 4. Kiến trúc module và luồng dữ liệu

```text
crates/harness-cli/src/interactive/
  mod.rs            (+ cờ --plain / HA_UI vào LaunchMode::Interactive)
  app.rs            host chọn renderer: tui::run | run_loop (plain) | run_line_mode
  controller.rs     reducer; thêm ui_state(), tick(), phím mới, modal state
  events.rs         + Key::{Esc, Tab, PageUp, PageDown, CtrlL, CtrlU, CtrlW, CtrlA, CtrlE, AltEnter}
                    + SessionEvent::{StepStarted{step}, ApprovalExpired{request_id}}
                    + HistoryItem, ToolState, UiState, Modal
  input.rs          editor: paste giữ newline, Ctrl-U/W/A/E, autocomplete slash
  view.rs           plain strings (giữ) + HistoryItem::plain_lines()
  terminal.rs       + map phím mới, panic hook restore, capability probe (size, TERM, NO_COLOR)
  service.rs        + map StepStarted, phát ApprovalExpired, thời lượng tool
  tui/
    mod.rs          run(): tạo Terminal inline, vòng lặp poll → controller → insert_before/draw
    layout.rs       chia viewport: live block | modal | composer | status
    theme.rs        palette 16 màu, Theme::plain() khi NO_COLOR/TERM=dumb
    widgets/
      composer.rs   ô nhập nhiều dòng, wrap theo unicode-width, con trỏ, placeholder
      status.rs     thanh trạng thái + spinner
      history.rs    HistoryItem → Vec<Line> có style (user/assistant/tool card/run/error/notice)
      markdown.rs   markdown-lite: fence + nhãn ngôn ngữ, inline code, heading, bullet
      approval.rs   panel approval + đếm ngược hết hạn
      picker.rs     danh sách session, mũi tên/Enter/Esc
      help.rs       overlay /help, /status
```

Luồng một lượt:

```text
phím → terminal::map → Key → controller.handle_key → Vec<Effect>
                                     ↑
SessionEvent (channel) → controller.pump_events → Vec<Effect>
Effect::History(item)  → tui: Terminal::insert_before(rows, |buf| history::render(item))
                       → plain: WriteLine(item.plain_lines())
Effect::Stream(text)   → controller giữ trong pending_text; tui vẽ live block từ ui_state()
Effect::Redraw         → tui: terminal.draw(frame(ui_state))   plain: draw_prompt
Effect::Exit(code)     → xoá viewport (để lại scrollback), restore modes, thoát
```

Ràng buộc thứ tự **giữ như hiện nay**: text đang stream được flush vào history **trước** tool line/terminal line (`flush_stream`), nên history luôn là user → (assistant/tool xen kẽ theo thời gian thật) → `[run]`. Live block chỉ là phần `pending_text` chưa commit; khi vượt ngân sách hàng của viewport, commit các dòng hoàn chỉnh cũ nhất vào history để viewport không phình.

Chiều cao viewport: T01 phải kiểm tra API ratatui 0.30.2 có cho đổi chiều cao inline viewport lúc chạy không (đọc docs.rs `Terminal`, không đoán). Mặc định nếu **không** đổi được: chiều cao cố định `min(rows / 2, 14)` chọn lúc khởi động và tính lại khi `Resize` bằng cách tạo lại `Terminal` (đo flicker trong PTY); composer cuộn nội bộ khi vượt ngân sách. Nếu đổi được: tăng/giảm theo nhu cầu (status 1 + composer 1..8 + live ≤ 10 + modal).

## 5. Đặc tả UI

### 5.1. Vùng và trạng thái

| Vùng | Ready / SetupRequired | Running / Canceling | WaitingApproval | Picker / Help |
|---|---|---|---|---|
| Live block | ẩn | text đang stream (cắt theo ngân sách) | ẩn | ẩn |
| Modal | ẩn | ẩn | panel approval | picker hoặc overlay help |
| Composer | có focus, placeholder | có focus, marker `.. `, gõ được nhưng Enter bị từ chối kèm hướng dẫn Ctrl-C (luật cũ) | **mất focus**, hiện `y/n để trả lời` | mất focus |
| Status | `ready · <model> · session … · /help` hoặc `setup required: …` | spinner · step k/8 · tools n/16 · mm:ss · Ctrl-C hủy | `approval · còn mm:ss` | `↑↓ chọn · Enter · Esc` |

### 5.2. Bàn phím (chỉ những phím đã kiểm được)

| Phím | Idle | Đang chạy | Approval | Picker/Help |
|---|---|---|---|---|
| Enter | gửi (không gửi buffer rỗng) | từ chối + hướng dẫn | không làm gì (trả lời bằng y/n hoặc gõ chữ + Enter như cũ) | chọn mục / đóng help |
| Ctrl-J | xuống dòng | xuống dòng | — | — |
| Alt+Enter | xuống dòng **nếu** terminal báo được; T01 đo trên Windows Terminal + ConPTY, không hứa trước | | | |
| Paste | chèn nguyên văn, **giữ newline**, không gửi | như idle | bỏ qua | bỏ qua |
| ↑ / ↓ | buffer một dòng: lịch sử; nhiều dòng: di chuyển hàng | như idle | — | di chuyển chọn |
| ← → Home End | theo ký tự | | — | — |
| Ctrl-A / Ctrl-E | đầu / cuối dòng | | | |
| Ctrl-U / Ctrl-W | xoá tới đầu dòng / xoá từ | | | |
| Tab | hoàn thành slash command (`/re` → `/resume`) | | | |
| Esc | xoá gợi ý autocomplete | không hủy run (Ctrl-C mới hủy) | không từ chối (phải gõ n) | đóng modal |
| Ctrl-C | xoá buffer | hủy run → Canceling | hủy run (như cũ) | đóng modal rồi như idle |
| Ctrl-D | buffer rỗng: thoát | hủy + thoát 0 (như i05) | như đang chạy | đóng modal |
| Ctrl-L | vẽ lại viewport, không xoá scrollback | | | |
| y / n | ký tự thường | ký tự thường | trả lời approval | — |

Windows/ConPTY **không** phân biệt Shift+Enter với Enter (đã đo ở H07, evidence mục 21.1): không được ghi Shift+Enter vào help nếu T01 không đo được.

### 5.3. History items

| Item | Plain (giữ nguyên chuỗi cũ) | TUI |
|---|---|---|
| `Banner(lines)` | các dòng header hiện có | như plain, tên app in đậm |
| `User(text)` | `> text` (nhiều dòng: dòng đầu có marker) | nền/viền nhạt, marker `>` màu |
| `Assistant(text)` | text nguyên văn | markdown-lite: fence có nhãn ngôn ngữ và nền, inline code, `#` heading đậm, `-`/`*` bullet |
| `Tool{name, summary, state}` | `[tool] name summary` rồi `[tool] name ok\|failed` | một card `● name summary … ok 12ms` (xanh) / `failed 3.1s` (đỏ); card cập nhật tại chỗ nếu còn trong viewport, ngược lại in dòng settle |
| `Run(outcome, steps, tool_calls, elapsed)` | `[run] done` (giữ) | `[run] done · 3 steps · 2 tool calls · 14.2s` |
| `Error(msg)` | `[error] msg` | đỏ |
| `Notice(msg)` | `[info] msg` | xám |
| `Approval{…}` | 4 dòng như `approval_lines` | vẫn ghi vào history khi được trả lời: `[approval] granted/denied/expired <id>` |
| `Sessions(list)` | danh sách đánh số như cũ | picker; sau khi chọn ghi `continuing from session …` |
| `Help`, `Status(lines)` | như cũ | overlay; khi đóng **không** ghi vào history |

### 5.4. Màu, độ rộng, ngôn ngữ

- Chỉ dùng 16 màu chuẩn; không yêu cầu true color. `NO_COLOR` hoặc `TERM=dumb` → `Theme::plain()` (không SGR màu, vẫn có border ASCII).
- Độ rộng ký tự theo `unicode-width` sau NFC (`unicode-normalization` đã có trong workspace). Ký tự tiếng Việt tổ hợp không được làm lệch con trỏ.
- Chuỗi giao diện giữ ngôn ngữ như hiện tại (header tiếng Anh, hint tiếng Việt); không thêm hệ i18n ở track này.

## 6. Work items T01–T08

Mỗi item: mục tiêu, phụ thuộc, việc cụ thể, test oracle, tiêu chí đóng, ước lượng (S ≈ 1 lượt coding, M ≈ 2–3, L ≈ 4+). Mỗi item đều làm RED → GREEN, chạy `cargo fmt`, `clippy -D warnings`, affected tests; không nhận item xong khi còn `ignored` trong selector bắt buộc.

### T01 — Spike và quyết định (S/M)

Phụ thuộc: không. Mục tiêu: chứng minh D1–D2 chạy trên máy này trước khi refactor.

1. Thêm `ratatui = "=0.30.2"` và `unicode-width` (pin đúng version ratatui kéo vào) vào `crates/harness-cli/Cargo.toml`; cập nhật `Cargo.lock`; `cargo tree -p harness-cli -i crossterm` **một** phiên bản; `cargo deny`/license không có trong repo nên ghi license (MIT) vào SPEC.
2. POC tối thiểu **sau cờ tạm** `HA_UI=tui-spike` (xoá ở T02): inline viewport 6 hàng, `insert_before` một dòng mỗi giây, một ô nhập một dòng, thoát bằng Ctrl-D. Chạy trong: Windows Terminal, conhost cũ, và **PTY harness của repo** (`interactive_terminal.rs` với `ESC[6n` reply). Đo: dòng có vào scrollback đúng thứ tự không, viewport có nhảy khi resize không, `insert_before` có để lại rác trên ConPTY không.
3. Trả lời bằng đo: (a) ratatui 0.30.2 có cho đổi chiều cao inline viewport lúc chạy không; (b) `TestBackend` có hỗ trợ `Viewport::Inline` + `insert_before` để unit test không; (c) Alt+Enter có tới app như key event riêng trên Windows Terminal/ConPTY không; (d) feature `scrolling-regions` có giảm flicker không và có an toàn trên ConPTY không.
4. Ghi quyết định vào `docs/specs/HA_TUI.vi.md` mục "Quyết định T01" kèm số đo; nếu (a) là không thì áp mặc định ở mục 4.

Tiêu chí đóng: SPEC có bảng đo; POC bị xoá hoặc gắn `#[cfg(test)]`; `cargo build --locked` xanh; không còn cờ `tui-spike`.

### T02 — View model và refactor controller (M)

Phụ thuộc: T01. Mục tiêu: controller trả kiểu thay vì chuỗi; plain mode giữ transcript **byte-identical**.

1. `events.rs`: thêm `HistoryItem`, `ToolState { Started, Ok{elapsed}, Failed{elapsed} }`, `UiState`, `Modal { Approval{…, expires_at}, Picker{items, selected}, Help, Status }`, `SessionEvent::StepStarted{step}`, `SessionEvent::ApprovalExpired{request_id}`; các `Key` mới.
2. `controller.rs`: `Effect::{History(HistoryItem), Stream(String), Redraw, Exit(u8)}`; `ui_state(&self, now: Instant) -> UiState`; `tick(&mut self, now) -> Vec<Effect>` (chỉ cho spinner/elapsed/đếm ngược, không đổi phase); đếm `steps`/`tool_calls` từ event; nhận `ApprovalExpired` → đóng modal, ghi `[approval] expired <id>`, phase về `Running`.
3. `view.rs`: `impl HistoryItem { pub fn plain_lines(&self) -> Vec<String> }` tái dùng đúng các hàm hiện có (`tool_line`, `approval_lines`, `run_line`…).
4. `app.rs` plain: `WriteLine` ← `plain_lines()`; giữ nguyên `draw_prompt`/`erase_prompt`.
5. `service.rs`: map `TurnProgress::StepStarted` (bỏ dòng `return` ở 287); đo thời lượng tool trong observer (Instant từ Started tới Settled cùng `name`); khi gate hết hạn gửi `ApprovalExpired` **trước** khi trả `Expired` cho driver.
6. Test: mọi test controller/app hiện có đổi assertion sang `plain_lines`/`effects_to_plain()`; thêm test `t02_plain_transcript_is_byte_identical_to_h03` chạy cùng kịch bản scripted với snapshot chuỗi lấy từ HEAD trước refactor; test `t02_step_started_updates_the_counter`; test `t02_expired_approval_closes_the_modal_and_never_grants`.

Tiêu chí đóng: `cargo test -p harness-cli --bin ha --locked` ≥ 70 + test mới, không test nào bị xoá hoặc nới; `interactive_session` 9/9; gate H xanh một lần.

### T03 — Composer (M)

Phụ thuộc: T02. Mục tiêu: ô nhập nhiều dòng dùng được thật, paste giữ newline.

1. `input.rs`: `normalize_paste` giữ `\n` (chuẩn hoá `\r\n` → `\n`, vẫn loại ký tự điều khiển khác); `Key::{CtrlU, CtrlW, CtrlA, CtrlE, AltEnter}`; ↑/↓ di chuyển hàng khi buffer nhiều dòng, lịch sử khi một dòng và con trỏ ở hàng đầu/cuối; `completions(prefix) -> Vec<&'static str>` cho slash; Tab áp dụng gợi ý duy nhất.
2. Sửa có chủ ý test `h03_editor_paste_never_submits_multiple_commands`: vẫn assert **một** submit và nội dung là một message, nhưng nay assert newline được giữ (`"fix the parser\nrm -rf /\n:q"`); ghi thay đổi hợp đồng vào SPEC.
3. `tui/widgets/composer.rs`: wrap theo cột (unicode-width), con trỏ đúng cell sau wrap, chiều cao 1..8 rồi cuộn nội bộ, placeholder khi rỗng, marker `> `/`.. ` theo phase, tiêu đề viền là hint ngắn.
4. Test `TestBackend`: `t03_composer_wraps_vietnamese_and_places_the_cursor`, `t03_paste_keeps_newlines_and_submits_once`, `t03_tab_completes_a_unique_slash_command`, `t03_history_navigation_only_on_a_single_line_buffer`.

### T04 — History rendering và live block (M)

Phụ thuộc: T02. Mục tiêu: hội thoại đọc được, tách rõ user/assistant/tool.

1. `tui/widgets/history.rs`: render từng `HistoryItem` thành `Vec<Line>` có style; `insert_before(height, …)` với height tính sau wrap theo độ rộng hiện tại.
2. `tui/widgets/markdown.rs`: markdown-lite **không** thêm dependency; chỉ fence, inline code, heading, bullet; text không nhận dạng được in nguyên văn (không được nuốt ký tự).
3. Live block: vẽ `pending_text` (đã wrap) tối đa N hàng; khi vượt, controller commit các dòng hoàn chỉnh cũ nhất thành `HistoryItem::Assistant` (thêm `Effect::History`) — thứ tự với tool line giữ như `flush_stream`.
4. Tool card cập nhật tại chỗ: card `Started` nằm trong viewport tới khi `Settled` hoặc tới khi bị đẩy lên history; không giữ hai bản.
5. Test: `t04_stream_text_shows_in_the_live_block_before_run_terminal`, `t04_history_order_is_user_tool_assistant_run`, `t04_markdown_fence_keeps_every_character`, `t04_long_stream_commits_overflow_lines_in_order`.

### T05 — Status bar và tiến độ (S)

Phụ thuộc: T02. `tui/widgets/status.rs`: spinner (tick 100 ms **chỉ** khi `has_active_run()`), `step k/max_steps`, `tools n/max_tool_calls` (giới hạn lấy từ `TurnLimits::default()` qua service label hoặc event Accepted mở rộng — chọn cách không đổi contract `TurnDriver`), elapsed, model label, session ngắn, hint theo phase; setup-required hiển thị đúng chuỗi `setup_hint` hiện có. Host **không** vẽ lại khi idle và không có effect (đo bằng số lần `draw` trên `TestBackend`). Test: `t05_status_reflects_phase_steps_tools_and_elapsed`, `t05_idle_loop_does_not_redraw`.

### T06 — Approval panel, session picker, help overlay (M)

Phụ thuộc: T03, T05.

1. Approval: panel hiện action/summary/workspace/scope/request id + đếm ngược từ `expires_at` (timeout 5 phút của gate; lấy giá trị từ event, không hard-code lần hai); `y`/`n` trả lời ngay; vẫn nhận `yes/no/grant/deny/Enter` như cũ; deny/expire không thực thi (tái dùng test h05 của `interactive_session`).
2. Picker: `/resume` không tham số → `SessionsListed` mở picker; ↑/↓/Enter/Esc; `/resume <n>` và `/resume <id>` giữ nguyên; picker không mở khi đang chạy (luật cũ).
3. Help/Status overlay: `/help`, `/status`, `/config`, `/model` mở overlay đóng bằng Esc/Enter; plain mode vẫn in dòng như cũ.
4. Test: `t06_y_key_grants_exactly_the_pending_request`, `t06_esc_closes_the_picker_without_changing_the_source`, `t06_picker_enter_resumes_the_highlighted_session`, `t06_help_overlay_is_not_written_to_history`; ca `interactive_session` cũ giữ xanh.

### T07 — Robustness, fallback và phục hồi terminal (M)

Phụ thuộc: T03–T06.

1. Capability probe trong `terminal.rs`: size, `TERM`, `NO_COLOR`, `HA_UI`; `ha chat --plain` (clap; conflict với `--headless`); rơi về plain có một dòng stderr nêu lý do.
2. Resize: `Resize` → recompute layout (và tạo lại Terminal nếu T01 kết luận vậy); buffer composer không mất; test `TestBackend` resize + PTY.
3. Panic hook: bật trước khi tạo Terminal; hook xoá viewport, restore modes, in panic ra stderr; **không** đổi hành vi I08 (lỗi backend vẫn propagate exit 1, không panic). Kill cứng vẫn ngoài phạm vi.
4. Thoát: xoá vùng viewport, để scrollback nguyên, con trỏ ở cột 0 dòng mới; `/exit` khi đang chạy vẫn cancel + exit 0 + nhả store (i05).
5. `NO_COLOR`: transcript PTY không chứa SGR màu (`\x1b[3x m`, `\x1b[9x m`), vẫn có thể có sequence di chuyển con trỏ.
6. Test: `t07_small_terminal_falls_back_to_plain_and_says_why`, `t07_plain_flag_and_env_select_the_plain_renderer`, `t07_no_color_emits_no_color_sgr` (PTY), `t07_panic_hook_restores_the_terminal` (process test có seam debug-only như I08, không seam trong release), chạy lại i05/i07a/i07b/i08/i12/i13 trên TUI mặc định.

### T08 — Gate, PTY evidence, docs (S/M)

Phụ thuộc: T07.

1. `Verify-HaLaunch.ps1`: thêm bước `unit-tui` (hoặc gộp vào `unit-interactive` nếu cùng binary) và ít nhất 4 selector bắt buộc T (`t02_plain_transcript_is_byte_identical_to_h03`, `t03_paste_keeps_newlines_and_submits_once`, `t04_history_order_is_user_tool_assistant_run`, `t06_y_key_grants_exactly_the_pending_request`); cập nhật `$notRun` với các ca PTY mới.
2. `Invoke-HaPtyAcceptance.ps1`: thêm ca PTY `t01_tui_opens_with_status_and_composer`, `t03_pty_paste_keeps_newlines`, `t06_pty_approval_y_key`, `t07_pty_resize_keeps_the_draft`, `t07_pty_plain_flag`, `t07_pty_no_color`; cập nhật mô tả đầu script; toàn bộ ca cũ + mới xanh trong **một** lần chạy, ghi `PTY_EXIT`, số ca, thời gian.
3. Docs: `docs/OPERATOR_GUIDE.{vi,en}.md` mục 12 thêm bảng phím, plain fallback, giới hạn Windows (Shift+Enter, Alt+Enter theo kết quả T01); `docs/evidence/HA_TUI.vi.md` và `docs/handoffs/HA_TUI.vi.md` theo templates; README thêm một dòng trỏ tới plan/evidence T; `Verify-Docs.ps1 -SelfTest` xanh.
4. Không publish, không cài lên máy user, không paid smoke trừ khi assignment cấp.

## 7. Acceptance U01–U20

| ID | Điều phải đúng | Oracle | Test dự kiến (planned, chưa tồn tại) |
|---|---|---|---|
| U01 | Bare `ha` trên console thật mở TUI: header trong scrollback, status + composer hiện, thoát 0 sạch | PTY transcript có header, `> `, dòng status; exit 0 | `t01_tui_opens_with_status_and_composer` |
| U02 | Gõ tiếng Việt/backspace đúng cell, không vỡ | TestBackend buffer + PTY i06 trên TUI | `t03_composer_wraps_vietnamese_and_places_the_cursor`, i06 |
| U03 | Enter gửi một lần; Ctrl-J và paste nhiều dòng là **một** message giữ newline | fixture echo nhận đúng chuỗi có `\n`; một `Accepted` | `t03_paste_keeps_newlines_and_submits_once`, i21 |
| U04 | Text stream hiện trong live block trước `RunTerminal`; history đúng thứ tự | TestBackend theo thời gian; HTTP fixture SSE qua adapter thật | `t04_stream_text_shows_in_the_live_block_before_run_terminal`, `t04_history_order_is_user_tool_assistant_run` |
| U05 | Tool card có start/settle, ok/failed, thời lượng | TestBackend | `t04_tool_card_settles_in_place_with_duration` |
| U06 | Status hiện phase/spinner/steps/tools/elapsed khi chạy; idle không vẽ lại | đếm `draw` | `t05_status_reflects_phase_steps_tools_and_elapsed`, `t05_idle_loop_does_not_redraw` |
| U07 | Approval: `y`/`n`/gõ chữ; deny và hết hạn không thực thi; hết hạn đóng panel | store không có receipt; event `ApprovalExpired` | `t06_y_key_grants_exactly_the_pending_request`, `t02_expired_approval_closes_the_modal_and_never_grants`, h05 cũ |
| U08 | Picker `/resume` chọn đúng session; Esc không đổi nguồn; số vẫn dùng được | `resume(Some(id))` đúng id | `t06_picker_enter_resumes_the_highlighted_session`, `t06_esc_closes_the_picker_without_changing_the_source` |
| U09 | `/help`, `/status` là overlay, không ghi history; plain vẫn in dòng | history không có dòng help | `t06_help_overlay_is_not_written_to_history` |
| U10 | Tab hoàn thành slash command duy nhất | buffer sau Tab | `t03_tab_completes_a_unique_slash_command` |
| U11 | Resize không mất draft, viewport vẽ lại đúng | TestBackend resize + PTY | `t07_pty_resize_keeps_the_draft` |
| U12 | Terminal nhỏ / `TERM=dumb` / `--plain` / `HA_UI=plain` → plain, lý do ở stderr | process test | `t07_small_terminal_falls_back_to_plain_and_says_why`, `t07_plain_flag_and_env_select_the_plain_renderer` |
| U13 | `NO_COLOR` → không SGR màu | scan transcript | `t07_pty_no_color` |
| U14 | Ctrl-C/Ctrl-D/`/exit` giữ đúng luật H05 trên TUI | i05/i07a/i07b chạy lại trên TUI mặc định | i05, i07a, i07b |
| U15 | Terminal phục hồi khi thoát thường, lỗi backend (I08), panic; scrollback còn | PTY: sau exit gõ được, không raw mode | i08, `t07_panic_hook_restores_the_terminal` |
| U16 | Headless không đổi: không ANSI, JSON ổn định | test i03 headless cũ | i03 |
| U17 | Artifact đã cài mở TUI | i14 trên TUI | i14 |
| U18 | Subcommand cũ, exit code, `--help/--version` không đổi | i02 | i02 |
| U19 | Kill cứng giữa lượt: không thêm ghi nào từ UI | i13 trên TUI | i13 |
| U20 | Plain transcript byte-identical với H03 cho cùng kịch bản scripted | snapshot chuỗi | `t02_plain_transcript_is_byte_identical_to_h03` |

## 8. Gate và bằng chứng

- **Gate cục bộ mỗi item**: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test -p harness-cli --bin ha --locked`, các suite bị ảnh hưởng với `--test-threads=1`.
- **Gate checkpoint** (sau T02, T05, T07, T08): `pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json` → `passed: true`, `failures: []`; PTY: `pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1 -TimeoutSeconds 900` → `PTY_EXIT: 0`, đủ số ca.
- **Flake loopback đã biết** (evidence HA_LAUNCH mục 22): gate trên máy này có thể đỏ ở `i13`/`providers-streaming`/`unit-interactive` do fixture loopback, không phải hồi quy. Quy trình: chạy lại tối đa ba lần, ghi từng lần, chỉ nhận lần `failures: []`; **không** sửa test để xanh; nếu suite đỏ chậm bất thường (timeout) thì ghi đúng là flake và chạy riêng suite đó.
- **Evidence** theo `TEMPLATES.vi.md` mục 2: source digest, OS, lệnh thật, số test kỳ vọng/thực tế, PTY transcript path, negative controls (ví dụ: bỏ `ApprovalExpired` thì test U07 phải đỏ), `not_run` (Linux, live smoke, VM sạch).
- **Không nới `--locked`**: thêm dependency phải kèm `Cargo.lock` cập nhật trong cùng commit và ghi version/license vào SPEC.

## 9. Rủi ro và giới hạn nền tảng

| Rủi ro | Cách xử lý trong plan |
|---|---|
| `insert_before` trên ConPTY/conhost cũ để lại rác hoặc cuộn sai | T01 đo trước; nếu hỏng trên conhost cũ mà Windows Terminal ổn thì probe `WT_SESSION`/capability và rơi về plain trên conhost cũ, ghi rõ trong SPEC và operator guide |
| Không đổi được chiều cao inline viewport lúc chạy | Mặc định mục 4: chiều cao cố định + composer cuộn nội bộ; tạo lại Terminal khi resize nếu đo được chấp nhận |
| Shift+Enter/Alt+Enter không phân biệt được trên Windows | Chỉ ghi phím đã đo; Ctrl-J và paste là đường chính thức |
| Assertion PTY cũ vỡ vì layout mới | D5 giữ mốc chữ; mỗi assertion đổi phải ghi lý do trong SPEC; không xoá ca |
| Hai crossterm trong lockfile | T01 điều kiện dừng; cùng lắm nâng pin crossterm theo ratatui-crossterm, không dùng hai bản |
| Flicker do vẽ lại toàn viewport mỗi delta | Coalesce: một `draw` mỗi vòng poll (50 ms), không draw khi không có effect; đo số draw bằng TestBackend |
| Tiếng Việt tổ hợp làm lệch con trỏ | NFC trước khi đo width; test riêng với ký tự tổ hợp (`ệ` dạng decomposed) |
| Test TUI chỉ qua mock rồi tuyên bố đạt | D6: TestBackend là unit; console thật là điều kiện đóng T07/T08; không mock host |
| Refactor T02 làm lệch plain transcript | U20 snapshot byte-identical là selector bắt buộc |

## 10. Quyền và điểm dừng

Plan này **không tự cấp quyền**. Trong assignment mặc định DeepSeek được: đọc/sửa source, thêm dependency đã pin, build/test local, tạo tiến trình con và thư mục tạm, chạy PTY runner trên máy hiện tại. **Không** được nếu assignment không nêu: commit/push, gọi paid API, cài lên máy user, ghi User PATH, publish, xoá/nới test đã được chấp nhận, đổi schema store hoặc contract `TurnDriver`/`SessionPort` ngoài phần plan ghi.

Điểm dừng bắt buộc: sau T01 (quyết định), sau T02 (refactor nền), sau T05, sau T07, sau T08. Mỗi điểm dừng cập nhật `docs/handoffs/HA_TUI.vi.md` với exact next action. Không chuyển tiếp khi gate checkpoint chưa `failures: []` ít nhất một lần.

## 11. Thứ tự giao và checkpoint

| Checkpoint | Phạm vi | Prerequisite | Nhận là đạt khi |
|---|---|---|---|
| CP-A | T01–T02 | H01–H08 evidence hiện có | SPEC có quyết định T01 kèm số đo; U20 xanh; gate H xanh một lần; không còn code POC |
| CP-B | T03–T05 | CP-A | U02–U06, U10 xanh trên TestBackend; i06/i21 xanh trên TUI trong console thật |
| CP-C | T06–T07 | CP-B | U07–U09, U11–U15 xanh; toàn bộ ca PTY cũ xanh trên TUI mặc định trong một lần chạy |
| CP-D | T08 | CP-C | Gate có selector T, PTY runner đủ ca, operator guide + evidence + handoff; `Verify-Docs.ps1 -SelfTest` xanh |

Ba mốc trải nghiệm để không tuyên bố sớm: **"TUI mở được"** (CP-A + T05) chưa phải **"TUI dùng được"** (CP-C); chỉ CP-D mới là "track T xong". Full-screen, theme tuỳ chỉnh, chuột, cuộn history trong app, i18n là **ngoài phạm vi** và không được làm thêm khi chưa giao.
