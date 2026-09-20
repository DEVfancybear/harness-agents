# Đặc tả triển khai HA_TUI — nâng `ha` thành TUI terminal

Trạng thái: **đặc tả triển khai đang hiệu lực cho track T01–T08**, theo
[plan HA_TUI](../HA_TUI_PLAN.vi.md). SPEC này ghi quyết định của lượt coding kèm số
đo trên máy này; nó không thay acceptance U01–U20 và không thay plan.

## 0. Quyền được giao trong assignment này

Assignment: *"Triển khai T01–T08 theo plan HA_TUI, đi theo checkpoint CP-A → CP-D;
không chuyển checkpoint khi gate chưa `failures: []`; cập nhật handoff HA_TUI sau
mỗi checkpoint."* Prompt mẫu trong [HA_TUI_PROMPT.vi.md](../HA_TUI_PROMPT.vi.md)
mục 4 **không** tự cấp quyền; plan mục 10 cũng ghi rõ bộ tài liệu không tự cấp quyền.

Người giao việc đã trả lời bốn câu hỏi quyền trước khi coding (ghi ở handoff):

| Hành động | Trạng thái | Hệ quả |
|---|---|---|
| Đọc/ghi source trong repo, build và chạy test local | **Được cấp** (là chính công việc) | T01–T08 code + test local |
| Commit local và **push** `origin/master` | **Được cấp** | Mỗi item/checkpoint một commit; gate vẫn là điều kiện chuyển checkpoint |
| Paid provider smoke (gọi model trả phí) | **Được cấp** | Được chạy `Smoke-HaProvider.ps1` với key thật ở bước gate; ghi kết quả vào evidence |
| Cài binary thật lên máy user (`Install-Ha.ps1`, User PATH) | **Được cấp** | U17/i14 được chạy trên bản cài thật, không chỉ staging |
| Publish release/tag | **Không được cấp** | Chỉ release candidate + checksum, không publish |
| Đổi schema store hoặc contract `TurnDriver`/`SessionPort` ngoài phần plan ghi | **Không được cấp** | T02 chỉ mở rộng vocabulary event, không đổi chữ ký driver |

Fail-closed: khi một checkpoint cần quyền chưa có, checkpoint đó dừng ở trạng thái
`blocked_on_authority` trong handoff, không được mô tả là đã đạt.

## 1. Prerequisite và source boundary

Base revision `c97c7dd` (`docs(ha-tui): plan the TUI track T01-T08 and the DeepSeek
prompts`), nhánh `master`, `origin/master` cùng revision. Working tree **không sạch**
khi bắt đầu: hai file sửa dở từ việc đo flake loopback của track H —
`crates/harness-cli/tests/phase_p2.rs` (chờ fixture task được schedule trước khi
client kết nối) và `scripts/Verify-Phase.ps1` (giữ parallelism, ghi lý do). Hai thay
đổi này **được giữ nguyên** và đi cùng commit T01; chúng không thuộc track T.

Plan khảo sát ở `9c62329`; revision đó là baseline hành vi H. Track T triển khai
trên **đúng binary `ha` hiện có**: không crate UI riêng, không CLI/engine thứ hai,
không renderer thứ ba (quyết định D4 của plan).

Xác minh baseline trước khi sửa (lượt coding này):

- `cargo build -p harness-cli --locked` xanh trên toolchain 1.97.1, Windows x64.
- `cargo tree -p harness-cli -i crossterm` trước T01: **một** bản `crossterm 0.29.0`,
  chỉ do `harness-cli`.
- Bộ test đơn vị của binary `ha`: **70 test** (`cargo test -p harness-cli --bin ha
  --locked` sau khi thêm spike: 74, trong đó 4 test T01 của spike).
- 10 ca PTY trong `interactive_terminal.rs` xanh trên console thật trước khi sửa
  (`PTY_EXIT: 0`, "10 passed; 0 failed").

## 2. Quyết định T01 (đo trên máy này, không suy đoán)

Bốn câu hỏi plan mục 6 yêu cầu trả lời bằng số đo. Mọi số dưới đây lấy từ lượt coding
này; log nằm trong `target/verification/` (đã ignore) và được trích ở
[evidence HA_TUI](../evidence/HA_TUI.vi.md).

### T01-a. Chiều cao inline viewport có đổi lúc chạy được không? → **Không**

`Viewport::Inline(height)` được tiêu thụ trong `Terminal::with_options`;
`Terminal::resize(area)` với viewport inline coi `area` là **kích thước terminal**,
chỉ tính lại origin theo con trỏ, và không có method công khai nào đổi chiều cao đã
lưu (`ratatui-core-0.1.2/src/terminal/resize.rs:23`, `viewport.rs:99`). Đo bằng test
`t01_inline_viewport_height_is_fixed_and_a_resize_keeps_it`: sau `resize(80x40)`
chiều cao giữ nguyên 5, chiều rộng theo terminal (80); muốn chiều cao khác phải tạo
`Terminal` mới. Đo thêm `t01_inline_height_is_clamped_to_the_terminal_on_a_small_console`:
xin 20 hàng trên console 4 hàng → nhận đúng 4 (bị kẹp, không báo lỗi).

**Hệ quả (áp mặc định plan mục 4):** chiều cao viewport được chốt **một lần lúc khởi
động**; `Resize` chỉ tính lại layout bên trong, không tạo lại `Terminal`. Composer
cuộn nội bộ khi vượt ngân sách hàng.

### T01-b. `TestBackend` có chạy được `Viewport::Inline` + `insert_before`? → **Có**

`t01_test_backend_supports_an_inline_viewport_and_insert_before`: viewport neo vào
hàng con trỏ của backend (đặt con trỏ hàng 3 → viewport `y=3`), `frame.area()` là
hình chữ nhật viewport chứ không phải cả màn hình, `insert_before` chạy không lỗi và
dòng chèn vào tới buffer. Vậy toàn bộ T03–T07 **unit test được không cần PTY**.

### T01-c. Alt+Enter có tới app như key riêng trên console này? → **Có** (đo được)

Ca PTY `t01_probe_key_delivery_measures_alt_enter_ctrl_j_and_paste` gửi bốn ứng viên
và ghi lại đúng thứ tự nhận được:

| Gửi đi | App nhận |
|---|---|
| `\r` | `code=Enter modifiers=0x0 kind=Press` |
| `ESC` + `\r` | `code=Enter modifiers=KeyModifiers(ALT) kind=Press` |
| `\n` | `code=Enter modifiers=KeyModifiers(CONTROL) kind=Press` |
| `ESC[200~pasted ESC[201~` | `paste: "pasted"` |
| `\x04` | `code=Char('d') modifiers=KeyModifiers(CONTROL)` |

Kết luận: **Alt+Enter phân biệt được với Enter** trên ConPTY của máy này, và **Ctrl-J
tới app dưới dạng `Enter + CONTROL`** (không phải `Char('j')`), nên `map_key` phải
ánh xạ `Enter + CONTROL` → `Key::Newline`. Bracketed paste tới đúng dạng `Event::Paste`
trong ca này (khác lần đo H07 của i06, nơi ConPTY không chuyển tiếp marker — hai kết
quả không mâu thuẫn: i06 gửi marker qua một console khác đường). Shift+Enter vẫn
**không** đo được và vì vậy không được ghi vào help.

### T01-d. `scrolling-regions` có an toàn và có lợi trên ConPTY? → **Chạy được, nhưng bị loại**

Đo hai lần trên cùng ca PTY, chỉ khác feature:

| Đường | `insert_before` phát ra | ConPTY |
|---|---|---|
| Mặc định (không feature) | mỗi dòng là **một run liền mạch**, rồi `clear_viewport` | 3 dòng đúng thứ tự, không rác |
| `scrolling-regions` | mỗi dòng bị cắt thành nhiều run (`CSI r` + `CSI T`, rồi MoveTo cho từng từ) | 3 dòng vẫn đúng thứ tự, không rác |

Cả hai đường đều xanh trên console thật (replay transcript bằng
`scripts/Read-HaTranscript.mjs`: `probe line 1/2/3` + `composer` đúng thứ tự), nên
**an toàn** không phải yếu tố quyết định. Yếu tố quyết định là số mảnh ghi: đường
`scrolling-regions` chia mỗi dòng chèn thành nhiều lệnh `MoveTo` (đo trên
`RecordingBackend`: 5 mảnh/dòng thay vì 1), tức nhiều round-trip hơn tới console, mà
lợi ích của nó (không phải vẽ lại viewport) lại bị plan D4 vô hiệu vì renderer vẫn
phải vẽ lại sau mỗi lần chèn. **Quyết định: không bật `scrolling-regions`.** Feature
`scrolling-regions` của `harness-cli` được giữ lại như công tắc đo, mặc định tắt.

### T01-e. `insert_before` trên ConPTY có để lại rác không? → **Không**, và có một cạm bẫy đã đo

Transcript PTY **không** đọc được bằng mắt: trong raw mode console này không dịch
line feed, nên nhiều hàng bị nối thành một dòng logic trong file transcript. Đo bằng
hai đường độc lập:

1. `t01_insert_before_emits_the_rows_in_order_and_clears_the_viewport` trên
   `RecordingBackend`: mỗi dòng chèn được ghi **đúng một lần**, đúng thứ tự, và
   viewport bị **xoá trắng** sau mỗi lần chèn.
2. Replay transcript thật bằng `scripts/Read-HaTranscript.mjs` (áp đúng escape
   sequence): 3 dòng chèn nằm đúng 3 hàng trên viewport, không ký tự lạ.

**Cạm bẫy đo được:** `insert_before` xoá viewport và **không** vẽ lại nó, nên nếu
renderer không `draw` lại ngay sau mỗi lần chèn thì người dùng thấy một lỗ trắng.
Đó là lý do luồng thật phải là `insert_before` → `draw` trong cùng một vòng lặp.

### T01-f. Thoát khỏi inline viewport để lại gì? → **Đo trên Windows Terminal**

Chụp màn hình Windows Terminal với spike (evidence:
`target/verification/t01-windows-terminal.png`):

- 3 dòng chèn hiện đúng thứ tự phía trên viewport;
- dòng `> ` của composer giữ đúng mốc chữ D5;
- **sau khi thoát, prompt của shell trở lại ở cột 0 trên dòng mới**;
- nhưng phải vẽ **một frame trắng** trước khi thoát: nếu chỉ `clear()` vùng
  viewport, các hàng "bóng" của frame cũ còn nằm giữa prompt và output (đo được ở
  lần chụp thứ ba, xem evidence). Đây là hành vi T07 phải làm đúng.

### T01-g. Điều kiện chấp nhận D1: một `crossterm` duy nhất → **Đạt**

```text
cargo tree -p harness-cli -i crossterm
crossterm v0.29.0
├── harness-cli v0.1.0
└── ratatui-crossterm v0.1.2
    └── ratatui v0.30.2
        └── harness-cli v0.1.0
```

Một bản duy nhất. `unicode-width` cũng một bản: `0.2.2`, dùng bởi cả `harness-cli` và
`ratatui-core`. License: `ratatui 0.30.2` MIT, `ratatui-core/crossterm/widgets` MIT;
MSRV 1.88 < toolchain 1.97.1 của repo.

### Dependency đã thêm

| Crate | Pin | Feature | Lý do |
|---|---|---|---|
| `ratatui` | `=0.30.2` | `crossterm`, `crossterm_0_29`, `layout-cache`, `underline-color` (không default) | D1. `crossterm_0_29` là bắt buộc để không kéo bản crossterm thứ hai. |
| `unicode-width` | `=0.2.2` | – | D1/plan 5.4: đo cell sau NFC. Pin đúng bản `ratatui-core` kéo vào. |
| `unicode-normalization` | `=0.1.25` (đã có trong workspace) | – | NFC trước khi đo width. |

`Cargo.lock` được cập nhật trong cùng commit T01; `cargo build --locked` xanh; không
package nào đang dùng bị đổi version (diff lockfile chỉ **thêm** package).

## 3. Spike T01 — phạm vi và cách xoá

POC nằm ở `crates/harness-cli/src/interactive/tui/spike.rs`, chỉ tồn tại trong build
debug (`#[cfg(debug_assertions)]`), điều khiển bằng cờ ẩn `ha chat --tui-spike` và ba
biến môi trường `HA_TUI_SPIKE_PROBE` / `HA_TUI_SPIKE_KEYS` (chỉ đọc trong spike).
Ca PTY `t01_probe_inline_viewport_on_a_real_console` và
`t01_probe_key_delivery_measures_alt_enter_ctrl_j_and_paste` là phần đo, `#[ignore]`
như mọi ca PTY khác trong file.

**Điều kiện đóng T01** (plan mục 6): SPEC có bảng đo ✅; POC bị xoá hoặc gắn
`#[cfg(test)]` — **xoá** trước khi T02 bắt đầu; `cargo build --locked` xanh ✅; không
còn cờ `tui-spike` ✅ (xoá cùng POC).

## 3b. TUI trong console thật (đo ở CP-A)

Renderer TUI đã được nối vào `app::run`: khi console ≥ 60×10, `TERM` khác `dumb`,
không có `--plain` và `HA_UI` khác `plain`, app chạy `tui::run`; mọi trường hợp
khác rơi về plain kèm **một dòng lý do ra stderr**.

Đo trên console thật (ConPTY của repo + ảnh Windows Terminal):

| Điều | Kết quả |
|---|---|
| Composer vẽ đúng byte UTF-8 | `> Nhập yêu cầu` và `> sửa lỗi parser` có trong transcript dạng UTF-8 hợp lệ |
| Header vào scrollback qua `insert_before` | "Harness Agents", "Project:", "setup required" nằm **trên** viewport |
| Không rác giữa các frame | không có ký tự lạ ngoài các dòng app tự viết |
| Thoát để lại con trỏ ở dòng mới | transcript kết thúc bằng `\r\n` (i01/i14) |
| Lỗi backend I08 trên TUI | vẫn exit 1 với "terminal input/output failed" (i08) |

**Cạm bẫy đo được (quan trọng cho T03+):** mỗi lần vẽ lại composer, ratatui chỉ gửi
các cell **thay đổi**, nên khi người dùng gõ thêm một ký tự, nó chèn `MoveTo` ngay
trước ký tự đó: transcript thô có `> sửa lỗi` + escape + `parser` chứ không phải một
chuỗi liền. Vì vậy:

1. Composer render **một run cho mỗi hàng** (marker và text trong cùng một `Span`).
   Nếu marker là span riêng, ratatui chèn `MoveTo` giữa `> ` và chữ, và mốc D5
   `> text` biến mất khỏi transcript — đã đo được và sửa.
2. Assertion PTY về chữ người dùng gõ phải so trên **transcript đã chuẩn hoá**
   (`normalized()` trong `interactive_terminal.rs`: bỏ escape sequence, gộp khoảng
   trắng). Assertion về output của app (dòng echo, `[run]`, `[tool]`) vẫn so trên
   transcript thô.

**Flake còn lại (ghi trung thực):** 10 ca PTY cũ chạy trên TUI mặc định **không xanh
đủ trong một lần chạy** ở CP-A: mỗi lần chạy 1–3 ca đỏ, và **tập ca đỏ đổi giữa các
lần** (i01/i05/i06/i07a/i21 từng đỏ ở lần này và xanh ở lần khác) trong khi cùng một
binary đã rebuild. Bằng chứng app đúng: transcript của ca đỏ vẫn chứa đủ header,
composer và echo. Vì vậy đây là **race của harness PTY** (phím gửi vào lúc console
chưa sẵn sàng, và echo của console chen vào giữa các lần vẽ), không phải lỗi renderer.
Hai sửa đã áp trong harness: `PtySession::send` chờ app vẽ xong trước khi gõ, và
transcript được lưu lại cho mọi ca để đọc khi đỏ. Việc còn lại của T07 là làm harness
hết flake **trước** khi thêm ca PTY mới của T08 — không được nới assertion để xanh.

## 3c. Quyết định T03–T08 (ghi khi làm, không suy đoán)

### T03 — composer

- **Đổi hợp đồng có chủ ý:** `normalize_paste` **giữ newline** (`\r\n` → `\n`, các ký
  tự điều khiển khác vẫn bị loại). Trước T03 paste bị ép thành một dòng vì prompt chỉ có
  một dòng; composer nhiều dòng làm lý do đó hết hiệu lực. Bất biến không đổi: **một paste
  = một submit**. Test cũ `h03_editor_paste_never_submits_multiple_commands` được cập nhật
  để assert đúng chuỗi có `\n`, không nới lỏng.
- **Wrap theo cell sau NFC:** `layout::plan` gọi `composer::wrap_with_prefix` **một lần**
  mỗi frame và cả chiều cao ô lẫn vị trí con trỏ đều đọc từ kết quả đó, nên ô được vẽ và ô
  được chừa chỗ không thể lệch nhau (test `t03_layout_uses_the_same_wrap_for_rows_and_cursor`).
- `←` từ đầu một hàng đi tới **ký tự cuối của hàng trên**, không đứng trên ký tự xuống
  dòng — nếu không, `Ctrl-U` sẽ xoá nhầm hàng. Đây là hành vi đã đo và có test.

### T04 — history và live block

- `Effect::Stream` được renderer chuyển thành `HistoryItem::Assistant` ngay khi flush, nên
  thứ tự trong scrollback vẫn là user → (assistant/tool xen kẽ) → `[run]`, đúng như
  `flush_stream` trước T02.
- Tool card settle **tại chỗ**: item `Tool{state: Started}` và item `Tool{state: Ok|Failed}`
  là hai item, và renderer cập nhật card khi nó còn trong viewport (test
  `t04_tool_card_settles_in_place_with_duration`).
- Khi text stream vượt ngân sách hàng, các dòng hoàn chỉnh cũ nhất được commit thành
  `HistoryItem::Assistant` theo đúng thứ tự (test `t04_long_stream_commits_overflow_lines_in_order`).
- `markdown.rs` là markdown-lite **không thêm dependency**: fence + nhãn ngôn ngữ, inline
  code, heading, bullet; text không nhận dạng được in nguyên văn (test
  `t04_markdown_rendering_keeps_every_visible_character`).

### T05 — status bar

- Spinner/elapsed/đếm ngược chỉ được vẽ khi `controller.tick()` trả effect, tức chỉ khi có
  lượt đang chạy hoặc có panel đang chờ. Vòng lặp **không** vẽ lại khi rảnh: đo bằng số lần
  `draw` trên renderer đếm được (`t05_idle_poll_does_not_redraw`: 3 lần poll, **1** lần vẽ).
- Giới hạn `step k/max` và `tools n/max` lấy từ `SessionPort::limits()`, mặc định khớp
  `TurnLimits::default()` (8 bước, 16 tool) — không hard-code lần thứ hai trong UI.
- Thanh trạng thái **cắt** nhãn model cho vừa bề rộng thay vì bỏ nó: console hẹp vẫn phải
  nói đang dùng model nào (`t05_the_bar_never_exceeds_the_console_width`).

### T06 — approval, picker, overlay

- Panel phê duyệt đọc `expires_at` **từ event** (gate phát kèm hạn thật), nên không có
  timeout thứ hai trong UI; `y`/`n` trả lời ngay trong TUI, gõ chữ rồi Enter vẫn dùng được
  như trước.
- `Esc` đóng panel/picker/overlay và **không** bao giờ trả lời hay huỷ lượt.
- Overlay (`/help`, `/status`, `/config`, `/model`) không ghi vào history khi đóng; plain
  mode vẫn in dòng như cũ.

### T07 — fallback, phục hồi, NO_COLOR

- Probe quyết định renderer là hàm thuần (`tui_fallback_reason`) nên test được không cần
  terminal: `--plain`, `HA_UI=plain`, console < 60×10, `TERM=dumb`; mọi lý do in ra stderr.
- Panic hook được bọc quanh toàn bộ vòng lặp sống của app (`install_panic_hook`), phục hồi
  terminal rồi mới để panic nổi lên; lỗi backend (I08) **không** phải panic và vẫn exit 1.
- Khi `/exit` hoặc Ctrl-D đến trong lúc run còn active, controller yêu cầu cancel nhưng
  **chưa phát `Exit` ngay**. Nó chỉ thoát sau `RunTerminal`/`RecoverableError`, để worker đóng
  run và nhả SQLite writer; PTY `i05` chứng minh host kế tiếp mở cùng workspace được.
- Thoát: vẽ frame trắng, xoá vùng viewport, để con trỏ ở cột 0 dòng mới, giữ scrollback.
- `NO_COLOR`/`TERM=dumb` → `Theme::plain()`: không SGR màu, vẫn có khung; ca PTY
  `t07_pty_no_color` quét transcript thật.

### T08 — gate, PTY, docs

- Gate có **selector T bắt buộc** (`$requiredTuiSelectors`, 4 selector) và in chúng trong
  `required_tui_tests` của báo cáo JSON; self test canh danh sách này không được ngắn đi.
- PTY runner có **16 ca**: 10 ca H cũ trên TUI mặc định + 6 ca T
  (`t01_tui_opens_with_status_and_composer`, `t03_pty_paste_keeps_newlines`,
  `t06_pty_approval_y_key`, `t07_pty_resize_keeps_the_draft`, `t07_pty_plain_flag`,
  `t07_pty_no_color`).
- Harness PTY (đo được, ghi lại vì nó từng làm CP-A flake): transcript thô của ConPTY **đảo
  thứ tự** echo của console và escape sequence của app, và mỗi lần vẽ lại chỉ gửi các cell
  thay đổi nên một từ đang gõ có thể bị cắt bởi `MoveTo`. Vì vậy:
  1. `PtySession::send` chờ app vẽ xong trước khi gõ;
  2. assertion về chữ người dùng gõ so trên transcript **chuẩn hoá** (`normalized()`), còn
     assertion về output của app (echo, `[run]`, `[tool]`) so trên transcript thô;
  3. mọi ca lưu transcript vào `target/pty-transcripts/` để đọc khi đỏ.

## 3d. Lưu API key trong app — `/key` (sau CP-D, chưa commit)

Lượt này thêm đường **tự lưu credential** để người dùng không phải mở shell đặt biến môi
trường trước khi mở `ha`. Đây là việc **ngoài** track T01–T08: plan HA_TUI không có item nào
cho việc nhập key (handoff mục 13). Nguồn: `crates/harness-cli/src/interactive/credentials.rs`
(mới) + `bootstrap.rs`, `controller.rs`, `events.rs`, `headless.rs`, `input.rs`, `service.rs`,
`view.rs`. Không thêm dependency nào, `Cargo.toml`/`Cargo.lock` không đổi.

### 3d.1. Credential là file riêng — **cố ý** không nằm trong `config.toml`

**Quyết định:** key không được ghi vào `config.toml` nghiêm ngặt, và sẽ không bao giờ. Ba lý do:

1. `config.toml` là file **non-secret** theo hợp đồng (`config.rs:1`) và schema P0 chỉ nhận
   `schema_version`; thêm một trường secret vào đó là đổi contract P0.
2. Parser TOML **có thể trích giá trị nó từ chối** trong thông điệp lỗi — chính `config.rs:83`
   ghi lý do đó — nên key nằm trong file strict sẽ có đường rò ra log.
3. Một file riêng siết được quyền riêng và hoàn tác được riêng: xoá `credentials.env` là xong,
   không phải sửa file config.

`save()` vì vậy chỉ ghi thêm `config.toml` **tối thiểu** (`schema_version = 1`) và **chỉ khi
file chưa tồn tại** (mục 3d.6).

### 3d.2. Đường dẫn, định dạng, parser

| Mục | Giá trị |
|---|---|
| Tên file | `credentials.env` (`credentials::CREDENTIAL_FILE_NAME`) |
| Thư mục | `HA_CREDENTIALS_DIR` nếu có (biến chứa **thư mục**, dùng **nguyên như đã cho**, không lồng thêm; giá trị rỗng không tính là override), ngược lại `<data dir>/private` (`CREDENTIAL_DIRECTORY_NAME = "private"`, `credentials.rs:54`) |
| Data dir | `--data-dir` > `HA_HOME/data` > platform (`paths.rs:145`) |
| Windows | `%LOCALAPPDATA%\HarnessAgents\data\private\credentials.env` |
| Linux | `$XDG_DATA_HOME/harness-agents/private/credentials.env` |
| Nội dung | đúng một dòng `DEEPSEEK_API_KEY="<key>"` |
| Biến được nhận | chỉ `DEEPSEEK_API_KEY`; tên khác bị từ chối chứ không được hiểu một nửa |

**Quyết định: file nằm trong thư mục con `private/`.** Data root còn chứa project store mà module
này **không** sở hữu, nên siết quyền ở chính data root sẽ đổi ACL của những thư mục không thuộc
nó; một thư mục con riêng thì siết được mà không đụng ai (`credentials.rs:48`–`54`). Override
`HA_CREDENTIALS_DIR` vì vậy cũng được dùng nguyên trạng — không tự thêm `private/` lần nữa
(assert bằng `k03_the_credential_directory_is_a_dedicated_subdirectory`).

Parser `credentials::parse` (`credentials.rs:170`) chấp nhận dòng trống và dòng bắt đầu `#`,
rồi đúng một dòng `NAME=value` với `NAME` là biến đã biết và giá trị được quote bằng `"` hoặc
`'`. Nó **từ chối**: dòng không có `=`; tên biến khác (`it names a variable this app does not
use`); khai hai lần (`the variable is set twice`); giá trị không quote. `\` và `"` được escape
khi ghi và hoàn nguyên khi đọc, nên key chứa `"` sống qua round trip.

Thông điệp lỗi của `load` nêu **path** và bước tiếp theo (`/key` hoặc xoá file) và **không**
lặp lại nội dung file — kể cả khi lỗi đến từ parser, vì chi tiết parser có thể trích giá trị bị
từ chối. File **thiếu** hoặc giá trị **rỗng/toàn khoảng trắng** là `Ok(None)`: không phải lỗi,
và không cấp credential.

`validate_credential_file` (`service.rs:296`, dùng ở đường headless) kiểm thêm rằng nguồn mà
`credentials::source` báo và file mà resolver đọc là **cùng một path**, và file đó đọc được —
để một override lệch không tạo ra trạng thái "nhận ở đây rồi hỏng ở lần gọi đầu".

### 3d.3. Thứ tự ưu tiên — biến môi trường **luôn** thắng file

`credentials::source` (`credentials.rs:113`) là nơi duy nhất quyết định nguồn: nó hỏi
`DEEPSEEK_API_KEY` rồi `HA_API_KEY` (đúng thứ tự `CREDENTIAL_VARIABLES`) và **chỉ khi cả hai
đều vắng hoặc rỗng** mới `stat` file. Hệ quả phải nói rõ: file là **fallback**; nếu biến vẫn
được export trong shell đang chạy `ha`, `/key` ghi được file nhưng lượt kế tiếp **vẫn dùng
biến**.

`CredentialSource` chỉ mang **tên** nguồn — `Environment { variable }` hoặc `File { path }` —
và `describe()` trả `environment variable <TÊN>` hoặc `saved file <path>` (`credentials.rs:59`).
Không biến thể nào mang giá trị, nên header, `/status`, `/model` và
`SessionEvent::ProviderConfigured { source }` (`events.rs:298`) không có đường rò.

### 3d.4. Thứ tự siết quyền trong `save()` — 0600 lúc **tạo** file, ACL thật trên Windows

`credentials::save` làm tuần tự:

1. `create_dir_all(thư mục)` — lỗi là `StorageOpenFailed` có path.
2. `restrict_directory(thư mục)`: Unix `0700`; ngoài Unix không đặt mode.
3. `restrict_acl(thư mục, <DOMAIN>\<USER>)`: **chỉ trên Windows** — gọi `icacls <dir>
   /inheritance:r` rồi `icacls <dir> /grant:r "<account>:(OI)(CI)F" /grant:r "SYSTEM:(OI)(CI)F"`
   (`credentials.rs:355`–`385`). File credential **thừa hưởng** ACL của thư mục. Shell ra
   `icacls` là chủ ý: cách còn lại là gọi API bảo mật Win32, mà crate này không có `unsafe`.
4. `write_staged` **tạo** file staging `.credentials.env.staged` bằng
   `OpenOptions::new().write(true).create(true).truncate(true)` cộng `mode(0o600)` trên Unix
   (`std::os::unix::fs::OpenOptionsExt`), ghi nội dung rồi `sync_all()` (`credentials.rs:393`).
5. `rename` staging → đích: người đọc không thấy file viết dở.
6. `restrict_file(path)`: gọi thêm một lần, cho file cũ có trước bản này.
7. `save()` **trả về** `Protection` — đúng thứ nó vừa áp, không phải thứ nó hy vọng.

**Unix:** mode `0600` được kernel áp **lúc tạo file**, không phải `set_permissions` sau khi ghi,
nên câu "the key is never in a file that anyone else can open, not even for the instant between
creating and tightening it" (`credentials.rs:12`–`18`) **đúng**, và được canh bằng
`k01_the_stage_file_is_created_with_restrictive_flags` (mode file staging) cùng
`k01_the_file_is_owner_only_on_unix` (`0600` file + `0700` thư mục).

**Windows:** ACL owner-only **có** được áp, qua `icacls`. Đo trước/sau trên
`%LOCALAPPDATA%\HarnessAgents` (bên giao việc đo, xem evidence 11.2): **trước** thư mục cha có
`DESKTOP-14QHC6K\CodexSandboxUsers:(I)(OI)(CI)(RX)` — một nhóm không phải người dùng đọc được
key; **sau** khi lưu key, thư mục credential chỉ còn `NT AUTHORITY\SYSTEM:(OI)(CI)(F)` và
`DESKTOP-14QHC6K\duong:(OI)(CI)(F)`, file thừa hưởng đúng hai mục đó với cờ `(I)`.

**Khi bước ACL bị từ chối, app nói thật.** `restrict_acl` trả `Protection::ProfileDefault` nếu
`icacls` fail (hoặc không resolve được account), và `Protection::describe()` in ra nguyên văn
*"the profile default only: no owner-only permission could be applied, so another account on this
machine may be able to read the file"*. Đo được: trong một môi trường bị chặn đổi ACL,
`icacls <dir> /inheritance:r` trả **exit 5 `Access is denied`**, `save()` trả `ProfileDefault`, và
test `k03_a_saved_key_is_restricted_to_this_account_by_an_acl` (assertion `OwnerOnlyAcl`) **đỏ** —
đúng như thiết kế: môi trường bị siết thì được **báo sự thật**, không được cấp một lời hứa suông.
Không có test nào **ép** nhánh fail đó (evidence mục 11.7).

**Bốn nhãn `Protection`** (`credentials.rs:58`–`87`) là hợp đồng trung thực, không phải trang trí:

| Nhãn | Nghĩa | Nguồn |
|---|---|---|
| `OwnerOnly` | Unix `0600`/`0700` do app áp | `restrict_acl` nhánh unix |
| `OwnerOnlyAcl` | ACL do app áp (account này + SYSTEM) | `icacls` chạy thành công |
| `ProfileDefault` | **Không** áp được gì; câu mô tả nói thẳng là tài khoản khác **có thể** đọc được | `icacls` fail/không có account |
| `NotReverified` | File tìm thấy lúc khởi động: app đã đặt nó khi lưu key, nhưng **lần chạy này không đo lại** | `credentials::source` |

`CredentialSource::File { path, protection }` mang nhãn này; `/status` in thêm dòng
`Provider: credential file protection: <mô tả>` (`service.rs:337`), còn `describe()` của nguồn
vẫn chỉ là tên file.

### 3d.5. Lệnh `/key`

| Dạng | Hành vi |
|---|---|
| `/key` | Composer vào **secret entry**: buffer mask `•` mỗi ký tự (`SECRET_MASK`, `input.rs:78`), không completion, không picker; Enter lưu, giá trị **không** vào history |
| `/key <giá-trị>` | Lưu trực tiếp; chỉ **token đầu tiên** của phần còn lại được dùng (`argument` cắt theo whitespace, `controller.rs:692`); `/help` nói rõ dạng này **kém riêng tư hơn** và chỉ nhận một từ |
| `/key` khi run đang chạy | Từ chối kèm notice "cannot enter an API key while a run is active" |
| `/key` với giá trị rỗng | Notice "no key was entered; nothing was saved" |

Giá trị không có đường tới màn hình: cả plain lẫn TUI đều đọc
`InteractiveController::display_buffer()` (`controller.rs:217`), và `UiState::buffer` được set
từ chính hàm đó (`controller.rs:234`) — TUI vẽ `state.buffer`. `InputOutcome::Secret` là đường
duy nhất mang giá trị ra khỏi editor, và `LineEditor::submit` trả nó **trước** khi chạm history
(`input.rs:402`–`407`).

Dạng có tham số **đã được mô tả trong app** (sửa của lượt này): `/help` có dòng thứ hai
(`view.rs:131`–`133`) — *"save it in one line: less private, because the value stays in this
terminal's history, and it takes one word, so use /key alone when in doubt"* — và notice sau
`/key` trần cũng nói cùng điều đó (`controller.rs:753`–`754`). Giới hạn một-token vì vậy là
**hạn chế đã biết và đã ghi**, không còn là điều chỉ tài liệu ngoài repo biết.

**Esc huỷ secret entry — đã sửa.** `Key::Esc` của editor nay có nhánh `self.secret` gọi
`cancel_secret()` rồi trả `InputOutcome::Redraw` (`input.rs:351`–`368`). Hai test canh đúng
đường bấm phím thật, không gọi hàm trực tiếp: `k01_secret_entry_masks_the_buffer_and_never_reaches_history`
bấm `Key::Esc` (`input.rs:773`–`781`) và
`k01_key_entry_masks_saves_clears_the_gate_and_admits_the_next_message` khẳng định sau Esc thì
prompt trở lại bình thường và **file key đã lưu không bị ghi đè** (`controller.rs:1373`–`1385`).
Ctrl-C khi rảnh vẫn chỉ gọi `editor.clear()` (`controller.rs:672`), tức xoá buffer mà giữ nguyên
secret mode — nay không còn là đường cụt vì Esc đã thoát được.

`/help` liệt kê `/key` và cả dạng `/key <value>` (`view.rs:129`–`133`). Lượt `/key` ghi ở đây là
"9 dòng cho 8 lệnh" và `SLASH_COMMANDS` **8** phần tử (`input.rs:596`); sau khi thêm `/more`, con số
đúng là `SLASH_COMMANDS` **9** phần tử (`input.rs:670`) và `view::help_lines()` **11** dòng
(`/more` ở `view.rs:135`). Số hiện tại thuộc mục 3e.1; hai dòng dưới đây giữ nguyên như bản ghi của
lượt `/key`:

- ~~`SLASH_COMMANDS` vẫn **8** phần tử (`input.rs:596`).~~ → nay **9**, xem 3e.1.

### 3d.6. Lưu xong thì **không cần restart** — cơ chế

`controller::save_key` (`controller.rs:878`) làm bốn việc trong cùng một bước:

1. `AgentSessionService::save_credential` ghi file rồi trả
   `SessionEvent::ProviderConfigured { source }` (`service.rs:634`) — event chỉ mang nguồn.
   Ghi lỗi trả `SessionEvent::RecoverableError` và **không** ghi file dở.
2. `LaunchContext::credential_saved(source)` (`bootstrap.rs:170`) đọc config; nếu file **chưa
   tồn tại** thì ghi `schema_version = 1\n` bằng `write_minimal_config`, rồi `config::load` lại.
   File config **hỏng** không bị ghi đè: nó trả đúng lỗi của lần load bình thường.
3. Context mới là `ProviderState::CredentialPresent` với `setup_required = false`
   (`bootstrap.rs:188`), nên gate setup được xoá; controller đổi phase
   `SetupRequired | Booting → Ready` và vẽ lại header (`controller.rs:899`–`908`).
4. Lượt kế tiếp chạy `resolve_provider` **mỗi lượt** (`service.rs:740`) và dựng
   `EnvironmentCredential` mới; resolver đọc file **lúc gọi** (`credentials::load` trong
   `resolve`, `service.rs:444`), nên key vừa lưu có hiệu lực ngay trong phiên đang chạy. Nhánh
   biến môi trường thì ngược lại: `CredentialSource::is_live()` trả `false` cho environment và
   `true` cho file (`credentials.rs:76`).

`submit` hỏi `provider_problem()` **lúc gửi** chứ không lúc boot (`controller.rs:619`), nên một
"setup required" cũ không chặn được yêu cầu mà app đã phục vụ được.

`/status` và `/model` in `provider_diagnostics`: dòng đầu là
`Provider: credential from <nguồn> (value hidden)` (`service.rs:331`); khi nguồn là **file**, có
thêm dòng `Provider: credential file protection: <mô tả>` (`service.rs:337`) lấy từ nhãn
`Protection` — file tìm thấy lúc khởi động là `NotReverified` ("not re-measured now"), vì đo lại
ACL trên mỗi lần render là shell ra `icacls`. Các dòng sau nói endpoint và model đến từ biến hay
từ mặc định, và endpoint có trả lời TCP không (kiểm tra kết nối, không gửi request, không cần
credential).

### 3d.7. Test canh hợp đồng (tên thật, chạy trên cây nguồn này)

| Test | Điều nó khẳng định |
|---|---|
| `k01_only_the_known_variable_is_accepted` | tên biến khác bị từ chối; thông điệp không lặp nội dung file |
| `k01_the_parser_detail_never_quotes_the_value` | giá trị không quote bị từ chối; thông điệp có path và `/key`, **không** có key |
| `k01_missing_and_blank_files_are_absent_not_errors` | file thiếu và giá trị rỗng là `None`, không phải lỗi |
| `k01_round_trip_keeps_the_key_and_leaves_no_staging_file` | round trip giữ key; không còn file `.staged` |
| `k01_a_quote_in_the_key_survives_a_round_trip` | key chứa `"` sống qua escape/unescape |
| `k01_the_file_is_owner_only_on_unix` (`#[cfg(unix)]`) | mode file `0600` **và** mode thư mục `0700` — **không** được biên dịch trên Windows |
| `k01_the_stage_file_is_created_with_restrictive_flags` | `write_staged` tạo file staging với mode `0600` (assertion Unix trong một test chạy được mọi nền tảng) và nội dung đúng một dòng |
| `k01_key_entry_masks_saves_clears_the_gate_and_admits_the_next_message` | cả chuỗi `/key` bằng phím thật: prompt mask, file ghi đúng dòng `DEEPSEEK_API_KEY="..."`, gate setup xoá, phase `Ready`, notice trong transcript, key **không** có trong transcript, lượt kế tiếp được nhận thật, `/key` bị từ chối khi run đang chạy, và Esc sau đó không ghi đè key đã lưu |
| `k02_the_credential_file_follows_the_explicit_directory` | `HA_CREDENTIALS_DIR` thắng; giá trị rỗng không phải override |
| `k02_a_source_is_described_by_name_never_by_value` | `describe()` chỉ trả tên; `is_live()`: file `true`, env `false` |
| `k01_saving_a_key_clears_the_setup_gate_and_writes_the_minimal_config` | `schema_version = 1\n`; gate xoá; header nêu nguồn và không vẽ key |
| `k01_secret_entry_masks_the_buffer_and_never_reaches_history` | mask một-ký-tự-một-mask; Enter trả `Secret`; history rỗng; **bấm `Key::Esc` thật** thì thoát secret entry và không để lại gì |
| `k02_a_saved_file_configures_the_provider_and_the_environment_still_wins` | file là nguồn hợp lệ; biến môi trường thắng file |
| `k02_a_saved_file_is_readable_and_a_corrupt_one_is_refused` | file hỏng bị từ chối kèm path, không lặp nội dung |
| `k02_diagnostics_name_the_source_and_never_the_value` | `/status` nêu `credentials.env` + `value hidden`, không có key |
| `k03_the_credential_directory_is_a_dedicated_subdirectory` | thư mục mặc định là `<data dir>/private`; `HA_CREDENTIALS_DIR` được dùng **nguyên trạng**, không lồng thêm `private/` |
| `k03_a_saved_key_is_restricted_to_this_account_by_an_acl` (`#[cfg(windows)]`) | `save()` trả `OwnerOnlyAcl`; `icacls` đọc lại cho thấy ACL nêu đúng `<DOMAIN>\<USER>` và `SYSTEM`, **không** còn `CodexSandboxUsers`/`S-1-15-3-`; app vẫn `load()` được key vừa siết. Assertion trên chữ `SYSTEM` là **tiếng Anh** — ghi rõ trong test, không hứa cho Windows bản địa hoá khác |
| `k03_the_protection_labels_say_they_grant` | bốn nhãn `Protection` nói đúng thứ chúng cấp: `OwnerOnly` → `0600`, `OwnerOnlyAcl` → `SYSTEM`, `ProfileDefault` → "may be able to read", `NotReverified` → "not re-measured" |
| `k01_a_secret_buffer_is_painted_as_a_mask` (TUI) | **khung hình đã vẽ** (`ScriptedRenderer::draw_state`) với `state.buffer = mask_secret(true, "sk-live-secret")` **không** chứa key và **có** chứa `•` — nên một renderer lách qua state để lấy buffer thô sẽ không qua được |

**Số test, ghi kèm ngày đo:** binary `ha` có **153 test**, đo lúc 10:49 ngày 20/09/2026 bằng
`cargo test -p harness-cli --bin ha --locked -- --list`; trong đó **18** tên khớp `k0[123]_` biên
dịch trên Windows (10 `k01_*` + 5 `k02_*` + 3 `k03_*`), cộng `k01_the_file_is_owner_only_on_unix`
(`#[cfg(unix)]`) là **19 test của feature**. Đây là **số đo có ngày**, không phải hằng số: lần sau
thêm test thì con số này đổi.

Chạy đầy đủ: `cargo test --release -p harness-cli --bin ha --locked` cho **153 passed; 0 failed**
trong phiên có quyền đổi ACL (bên giao việc, 20/09/2026); trong phiên soạn tài liệu này (file
policy `workspace-write`) cùng bản cây cho **152 passed; 1 failed**, và test đỏ đúng là
`k03_a_saved_key_is_restricted_to_this_account_by_an_acl` vì `icacls` bị từ chối — chi tiết và cách
đọc kết quả ở evidence 11.2/11.7.

Đảm bảo "key không lên màn hình" nay được assert ở **ba tầng**, không chỉ một: editor
(`k01_secret_entry_masks_the_buffer_and_never_reaches_history`), controller
(`k01_key_entry_masks_saves_clears_the_gate_and_admits_the_next_message`: prompt đã mask, key không
có trong transcript), và **khung hình đã vẽ** (`k01_a_secret_buffer_is_painted_as_a_mask`).

Điều các test **không** phủ: một lượt gọi provider thật (`not_run` vì môi trường không có
credential — evidence mục 11), một ca PTY cho `/key` (ConPTY cần console mà sandbox build không
có), và **nhánh `icacls` thất bại** — không test nào **ép** `restrict_acl` trả `ProfileDefault`,
nên "fallback trung thực" được chứng minh bằng phép đo thủ công trong môi trường bị chặn ACL
(evidence 11.2/11.3), không bằng test. Đường controller `save_key`, hành vi Esc, đường dẫn
`private/`, bước ACL Windows và mask ở tầng khung hình **đã** được phủ.

## 3e. Đọc câu trả lời dài ngay trong app: `/more`, phím cuộn và con lăn chuột

Ba commit sau CP-D (`1340129` `/more` + 13:36, `cce5c13` alternate scroll + 13:45, `5883d59`
hai flake loopback + 13:58, ngày 20/09/2026) đóng đúng khoảng trống mà evidence mục 11.7 đã
ghi: **một câu trả lời dài bị viewport cắt mất phần đầu thì không có đường đọc lại trong
app**. Mục này ghi quyết định kèm số đo; phần chưa chứng minh nằm ở evidence mục 12.4.

### 3e.1. `/more` — mở lại transcript gần nhất, **từ dòng đầu**

| Mục | Hợp đồng |
|---|---|
| Lệnh | `/more`; vào bảng `SLASH_COMMANDS` (nay **9** phần tử, `input.rs:670`) và vào `view::help_lines()` (`view.rs:135`) |
| Panel | Cùng loại overlay với `/help`/`/status`/`/config`/`/model` (`reference()`, `controller.rs:875`), **mở ở dòng đầu**: `open_overlay` đặt `scroll = 0` (`input.rs:193`) |
| Nội dung | Buffer hồi tưởng **có chặn 500 dòng** trong controller (`RECALL_LINES`, `controller.rs:1044`); thêm cả `pending_text` đang stream (`recall_lines()`, `controller.rs:1053`) |
| Ghi buffer | Đúng những điểm ghi transcript: `flush_stream` (`controller.rs:1000`), `flush_stream_overflow` (`controller.rs:1028`), `push_history` (`controller.rs:1034`) |
| Plain mode | In các dòng như mọi lệnh tham chiếu khác, tức vào history (`reference()`, nhánh `self.plain`) |
| History | Mở và cuộn panel **không** ghi history; `Esc` đóng panel — acceptance U09 vẫn đúng |

Hai quyết định đáng ghi:

1. **Mở ở dòng đầu, không mở ở cuối.** Viewport chỉ giữ đuôi câu trả lời, nên thứ người
   dùng bị mất chính là **phần đầu**; một panel mở ở cuối bắt người đọc cuộn trước khi thấy
   thứ họ cần — đúng cái vấn đề lệnh này sinh ra để sửa. Test
   `k04_more_opens_the_recent_transcript_from_its_first_line_and_scrolls`
   (`controller.rs:2185`) khẳng định `scroll == 0` và panel chứa **cả** `answer line 0` lẫn
   `answer line 19` của một câu trả lời 20 dòng.
2. **Buffer 500 dòng là bound, không phải cửa sổ đọc.** Nó không thay scrollback của
   terminal (nơi app commit mọi thứ); nó chỉ đủ để đọc lại câu trả lời vừa rồi mà không phải
   rời app, và bị chặn để một phiên dài không phình vô hạn. Hệ quả phải nói rõ: `/more`
   **không** phải lịch sử đầy đủ — nó cắt theo dòng logic, và phiên dài hơn 500 dòng thì
   phần cũ nhất chỉ còn trong scrollback của terminal.

### 3e.2. Phím cuộn của panel — và một chỗ lời gợi ý nói rộng hơn code

`PageUp`/`PageDown` cuộn 8 dòng, `Home` về dòng đầu, `End` về dòng cuối
(`controller.rs:331`–`335`). `Overlay::scroll_by` bão hoà ở đỉnh (không quấn vòng), `scroll_end`
đặt offset `usize::MAX / 2` và **để renderer kẹp** — vì nó là thứ duy nhất biết bao nhiêu dòng
vừa (`help.rs:29`–`31`).

Viền dưới của panel nói trạng thái cuộn (`help.rs:33`–`45`): `Esc đóng` khi vừa hết,
`còn N dòng` ở đỉnh, `dòng x/y` ở giữa, `cuối` khi tới đáy.

**Mâu thuẫn đo được, ghi lại chứ không sửa (không thuộc phạm vi tài liệu):** chuỗi gợi ý mở
đầu bằng `↑↓/PgUp/PgDn cuộn` (`help.rs:36`, `38`, `41`), nhưng controller **không** cho `↑`/`↓`
cuộn panel: nhánh overlay chỉ bắt `PageUp`/`PageDown`/`Home`/`End` (`controller.rs:331`–`337`),
còn `↑`/`↓` rơi xuống editor — và ở đó chúng đổi **buffer soạn thảo** (recall history hoặc di
chuyển theo hàng, `input.rs:443`–`460`) trong khi panel vẫn mở. Vậy hai chữ `↑↓` trong gợi ý
mô tả một phím **không** cuộn panel, và thao tác đó còn sửa draft phía sau panel. Đây là lỗi
chữ trong UI, không phải lỗi tài liệu: test không phủ chuỗi gợi ý nên không test nào đỏ.

### 3e.3. Con lăn chuột: alternate scroll (DECSET 1007), **không** phải mouse capture

App bật `ESC [ ? 1007 h` khi vào raw mode, và tắt ở **cả** `RawModeGuard::drop` lẫn panic hook
(`terminal.rs:152`–`158`, `202`, `218`, `181`). `ModeControl` có thêm
`enable_alternate_scroll`/`disable_alternate_scroll` (`terminal.rs:119`–`120`); thứ tự mode
nay được assert đầy đủ trong test I08: `enable, paste on, alternate scroll on, paste off,
alternate scroll off, disable`.

**Quyết định (cố ý, không phải thiếu sót):** đây **không** phải mouse capture
(`1000`/`1002`/`1006`). Capture lấy con lăn khỏi terminal và **xoá scrollback** — mà scrollback
chính là nơi app commit mọi thứ nó flush (`insert_before`), nên bật capture sẽ đổi "mất phần
đầu câu trả lời trong viewport" lấy "mất luôn phần đầu trong scrollback". Với 1007, con lăn
vẫn cuộn scrollback của terminal, app **không** nói giao thức chuột nào, và không phím nào đổi
nghĩa.

**Ranh giới của bằng chứng:** 1007 là hành vi **phía terminal**. Không test nào chứng minh được
một terminal cụ thể tôn trọng nó; app chỉ gửi đúng chuỗi và không làm gì khác. Chưa đo trên
Windows Terminal hay bất kỳ emulator nào trong lượt này (evidence 12.4).

### 3e.4. Hai flake loopback đã sửa (có số đo trước/sau)

Đây là hai nguyên nhân **còn lại** sau ba nguyên nhân đã sửa trước đó (evidence mục 8):

| Flake | Cơ chế | Sửa | Số đo |
|---|---|---|---|
| `providers-streaming` (SSE fixture) | Body SSE được kết thúc bằng **đóng kết nối**; client không phân biệt được "body xong" với "kết nối bị reset", nên một close tới dưới dạng RST đọc ra thành body cụt | Chunked body thật: `Transfer-Encoding: chunked` + chunk cuối `0\r\n\r\n` (`streaming.rs:381`–`396`), socket vẫn được giữ 500 ms sau chunk cuối (`streaming.rs:441`–`453`) | **0 đỏ trong 10 lần liên tiếp** `cargo test -p harness-providers --locked`; trước khi sửa khoảng **1 lần đỏ trong 8 lần chạy** |
| `completion_service_resume_flow` | Ca chạy trong process con; dưới tải cả workspace, runtime của con bị đói worker thread trước lần connect đầu | Con chạy **tối đa hai lần**; thất bại sau lần thử lại **vẫn được báo** (`service_completion_tests.rs:20`–`43`) | **0 đỏ trong 5 lần liên tiếp** chạy cả suite; trước khi sửa khoảng **1 lần đỏ trong 3 lần chạy** |

Không nới assertion nào: lần thử lại chỉ cứu **fixture**, còn lỗi thật vẫn đỏ ngay lần đầu.

## 4. Kiến trúc chốt cho T02–T08

Theo plan mục 4, với hai điều chỉnh đã đo:

1. Chiều cao viewport **cố định lúc khởi động** (T01-a), công thức
   `min(rows / 2, 14)` với sàn 5 hàng; layout bên trong chia
   `live | modal | composer | status`.
2. Luồng chèn history luôn là `insert_before(h)` rồi `draw` (T01-e), và thoát phải vẽ
   frame trắng rồi mới trả terminal (T01-f).

`scrolling-regions` **không** được bật (T01-d).

### Hợp đồng text landmark (D5) — không được đổi

| Dòng | Luôn bắt đầu bằng |
|---|---|
| Composer | `> ` (idle/setup) hoặc `.. ` (running) |
| User trong history | `> ` |
| Tool card (plain) | `[tool] ` |
| Kết thúc lượt | `[run] ` |
| Approval | `[approval] ` |
| Lỗi | `[error] ` |
| Thông tin | `[info] ` |

Lý do: bộ assertion PTY hiện có grep theo các mốc này (i01/i05/i06/i07a/i07b/i12/i13/i21);
đổi mốc phải ghi lý do vào SPEC này và cập nhật test có chủ ý.

**Bổ sung sau CP-D (mục 3e):** đường đọc lại câu trả lời dài — `/more`, phím cuộn panel và
alternate scroll `1007` — **không** đổi mốc chữ D5 nào, và cũng không đổi luồng
`insert_before` → `draw`; nó chỉ thêm một panel đọc và một chuỗi mode phía terminal.
