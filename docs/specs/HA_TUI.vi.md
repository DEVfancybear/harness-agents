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

## 3f. Hai khiếm khuyết nhìn thấy trên màn hình thật (screenshot, lượt này)

Hai lỗi này **không** test nào bắt được trước đó: chúng chỉ hiện ra khi nhìn khung hình vẽ thật.
Cả hai đều là lỗi renderer, không phải lỗi dữ liệu — nội dung trong history vẫn đúng.

### 3f.1. `**đậm**` in ra nguyên dấu sao, và dòng dài bị terminal cắt cụt

| Điều quan sát được | Nguyên nhân | Sửa |
|---|---|---|
| Câu trả lời in ra `**Tool call:**` với đủ bốn dấu `*` | `markdown::render` chỉ xử lý heading/bullet/fence, **không** có nhánh nào cho emphasis: `inline_spans` chưa tồn tại | `inline_spans` biến `**…**` thành `Modifier::BOLD` và **bỏ** marker; `` ` `` vẫn giữ vì đó là ký tự model viết |
| Dòng dài bị cắt ở mép console, chữ mất hẳn | `render` trả về **một** `Line` cho mỗi dòng văn bản, không đo bề rộng; terminal cắt phần vượt | Mọi block đi qua `wrap_spans(spans, width)`; `width` lấy từ `area.width` của khung |

Quy tắc đã chốt, không đổi ngầm:

1. **Không ký tự nào bị mất.** Hàm `render` là toàn phần: mọi ký tự vào ra đều có mặt, đúng thứ tự.
   Thứ duy nhất bị bỏ là **khoảng trắng tại điểm ngắt dòng** — chính chỗ ngắt đã thay nó — và marker
   `**` đã thành style. Test `t04_markdown_rendering_keeps_every_visible_character` giữ bất biến này.
2. **Ngắt dòng tại khoảng trắng**, không cắt giữa từ (`break_point` trả về index **sau** khoảng trắng
   cuối, rồi `trim_trailing_spaces` cắt nó khỏi dòng vừa xong). Một token không có khoảng trắng nào
   (đường dẫn, URL dài) **vẫn** bị ngắt giữa từ — vì phương án còn lại là terminal cắt mất chữ.
3. **Marker chưa đóng là chữ.** `a ** b` in ra đúng `a ** b`, không nhân đôi. `****` (không có gì ở
   giữa) cũng là chữ. Quyết định bằng cách nhìn trước (`after.find("**").filter(|at| *at > 0)`) chứ
   **không** bằng cờ trạng thái — cờ chỉ có thể gắn marker vào cuối dòng, sai vị trí.
4. **Dòng không bao giờ được in thừa khoảng trắng**, kể cả dòng cuối: `trim_trailing_spaces` chạy cả
   ở nhánh kết thúc.

Test canh (chạy trên cây nguồn này, `crates/harness-cli/src/interactive/tui/markdown.rs`):

```text
t04_markdown_rendering_keeps_every_visible_character   -> ok
t04_emphasis_markers_become_styling_instead_of_asterisks -> ok  (**Tool call:** -> "Tool call:")
t04_an_unclosed_marker_is_text                          -> ok  (a ** b ` c -> nguyên văn)
t04_markers_with_nothing_between_them_are_text           -> ok  (a **** b -> nguyên văn)
t04_long_rows_wrap_instead_of_being_clipped             -> ok  (mọi dòng <= width)
t04_wrapping_breaks_at_spaces                           -> ok  ("alpha beta" / "gamma delta")
t04_a_word_wider_than_the_row_still_wraps               -> ok  (25 ký tự / width 10 -> 3 dòng)
t04_wrapping_keeps_every_word_of_a_long_paragraph        -> ok  (không mất, không đổi thứ tự từ)
```

### 3f.2. Panel duyệt in trùng khối proposal, và gợi ý chỉ sai chỗ

| Điều quan sát được | Nguyên nhân | Sửa |
|---|---|---|
| Khối `[approval] …` hiện **hai lần**: một lần trong scrollback phía trên, một lần trong panel | `controller.rs` đẩy `HistoryItem::Approval` vào history **ngay khi request tới**, rồi `layout::plan` lại vẽ panel từ `pending_approval` — cùng một dữ liệu, hai chỗ vẽ | Ở TUI, panel là chỗ **duy nhất** in proposal: `push_history` chỉ chạy khi `self.plain`. Chế độ plain **không** có panel nên ở đó history vẫn giữ khối (U20) |
| Viền composer ghi `trả lời panel ở trên` — không nói phím nào, và panel không "ở trên" theo nghĩa người đọc hiểu | `composer::hint` trả **một** câu cho mọi loại modal | `hint` khớp theo `state.modal`: approval → `y chạy · n từ chối`, picker → `↑↓ · Enter · Esc`, overlay → `PgUp/PgDn · Home/End · Esc` |

Hệ quả đã kiểm: `i14`/`t06_pty_approval_y_key` vẫn thấy `[approval] granted fixture-approval-1` trong
transcript (dòng resolution **không** đổi), và mốc D5 `[approval] ` vẫn còn trên màn hình vì panel tự
in nó ở dòng đầu.

Test canh:

```text
controller::tests::t06_the_open_panel_is_the_only_place_the_proposal_is_shown   -> ok (TUI: history rỗng)
controller::tests::t06_a_plain_session_still_records_the_proposal_it_cannot_panel -> ok (plain: có khối)
tui::tests::t06_a_pending_approval_frame_holds_one_copy_of_the_proposal         -> ok (khung vẽ thật: 1 lần)
composer::tests::t03_the_hint_names_the_keys_of_the_panel_that_is_open          -> ok
```

### 3f.3. Chữ thật của model là đầu vào của test, không phải chữ tự nghĩ ra

Một paid turn thật (prompt buộc markdown) trả về đúng `**bold**` cộng một đoạn văn dài 710 ký tự.
Đó chính là hình dạng câu trả lời đã sinh ra cả hai lỗi trong ảnh, nên nó được đóng thành test
`t04_a_real_answer_loses_no_marker_and_no_word`: nguyên văn câu trả lời đó, ở width **72** (console
hẹp — nơi lỗi xuất hiện), khẳng định (a) không còn `**` trên màn hình, (b) mọi dòng ≤ 72 cell —
tức terminal không còn gì để cắt, (c) mọi từ còn đủ và đúng thứ tự.

## 3g. Gate phê duyệt: đọc thì không hỏi, ghi thì vẫn hỏi (lượt này)

### 3g.1. Vì sao đổi — và vì sao **không** phải nới gate

Người giao việc chat và gặp panel duyệt cho `ListFiles: list .`. Câu hỏi đúng: Claude Code
và Codex CLI **không** hỏi ở trường hợp đó. Đã tra tài liệu gốc của cả hai, rồi đọc thẳng
binary trên máy này:

| | Claude Code 2.1.218 | Codex CLI 0.149.0 |
|---|---|---|
| Cách phân loại | bảng theo **loại tool**: chỉ-đọc **không** hỏi trong working directory | **không** liệt kê tool: mặc định `sandbox = workspace-write` + `-a on-request`, nên thứ gì trong biên giới thì tự chạy |
| Lệnh shell chỉ-đọc | bộ dựng sẵn `ls cat echo pwd head tail grep find wc which diff stat du cd` + `git` chỉ-đọc | trong biên giới sandbox |
| Lựa chọn khi panel hiện | `Yes` / `Yes, and don't ask again` / `No` (đôi khi chỉ one-time) | `[a] Accept once` · `[s] Accept for session` · `[p] Accept and add to policy` · `[d] Decline` · `[c] Cancel turn` |
| Mặc định trên máy này | không đặt `defaultMode` → Manual, và Manual chỉ chạy **reads** mà không hỏi | `~/.codex/config.toml` có `trust_level = "trusted"` cho workspace này |

Bản của tôi trước lượt này là `ApprovalMode::Ask` cho **mọi** action (`service.rs`), không có
khái niệm chỉ-đọc, không có bậc thẩm quyền, không có trust. Đó là **thiếu sót thiết kế**, không
phải người dùng dùng sai.

### 3g.2. Ranh giới đã chốt — 4 mục, không hơn

1. **Miễn hỏi cho 5 action chỉ-đọc** trong workspace: `read_file`, `list_files`,
   `search_text`, `git_status`, `git_diff`.
2. **Danh sách bảo vệ không bao giờ được miễn** — điểm tựa an toàn của cả thay đổi.
3. **Ghi / patch / chạy lệnh vẫn hỏi từng lần.**
4. **Panel thêm lựa chọn `a`** (cho phép đọc cả lượt), giống `[s] Accept for session` của Codex
   nhưng **chỉ** cho chỉ-đọc và **chỉ** trong một lượt. **Không** làm mục "ghi vào policy vĩnh
   viễn" như `[p]` của Codex: nó ghi ra file và cần người dùng quyết riêng.

**Điểm tựa an toàn, đọc từ code chứ không suy đoán:** `ToolExecutionService::prepare` gọi
`validate_workspace_action` → `resolve_relative` **trước khi** một `ApprovalProposal` tồn tại.
Hàm đó từ chối: đường dẫn tuyệt đối, `..`, symlink/reparse point, đường dẫn canonicalise ra
ngoài root, và mọi tên khớp `is_sensitive_relative` (`.git`, `.harness`, `.env`, `.env.*`, tên
chứa `credential`/`secret`/`password`/`private_key`, đuôi `.pem`/`.key`/`.p12`/`.pfx`/`.clixml`).

Nghĩa là: một action chỉ-đọc **theo kind** vẫn có thể bị **từ chối thẳng** trước khi tới cổng.
Miễn hỏi bỏ **câu hỏi**, không bao giờ bỏ **kiểm tra**. Đó là lý do thay đổi này an toàn để làm,
và cũng là lý do `read_only` trong proposal nghĩa là "không ghi gì", **không** phải "đã được phép".

### 3g.3. Thi hành nằm ở một chỗ duy nhất

`ChannelApprovalGate` là nơi duy nhất quyết định miễn hỏi — không phải driver, không phải UI:

```text
request(proposal):
  if proposal.read_only && reads_for_run  ->  Notice + Granted   (không mở panel)
  else                                    ->  ApprovalRequired   (panel như cũ)
```

Hệ quả đã kiểm: **plain mode được miễn hỏi y hệt** mà không phải viết thêm dòng nào, vì hai
renderer dùng chung một cổng. `ApprovalMode` và trait `ApprovalGate` **không** đổi hình dạng —
`read_only` đi kèm proposal, nên chỉ đúng một chỗ dựng proposal (`proposal_for`) phải sửa.

Cờ `reads_for_run` là `AtomicBool` **trên object**, không ghi ra đĩa; `finish_run()` của
controller thu hồi nó, nên nó không sống qua lượt sau kể cả khi lượt kết thúc bằng lỗi hay
Ctrl-C. Thanh trạng thái hiện `· reads tự động` khi cổng đang mở: nới quyền mà không nói ra là
điều tệ nhất có thể làm ở đây.

### 3g.4. Test canh ranh giới (tên thật, đã chạy)

```text
harness-tools:
  tool_kinds_classify_read_only_by_construction_not_by_name       -> 5 chỉ-đọc, 5 không, liệt kê đủ
  a_read_only_kind_cannot_reach_a_protected_path_through_the_gate -> .env và ../ bị TỪ CHỐI,
                                                                     không phải được mời duyệt
harness-cli (cổng):
  t08_a_granted_read_is_not_asked_about_again_and_is_still_recorded -> không panel, nhưng CÓ Notice
  t08_a_granted_read_never_covers_a_mutating_action                 -> apply_patch vẫn mở panel
  t08_a_read_is_asked_about_until_the_user_allows_reads             -> mặc định vẫn hỏi
  t08_the_wider_answer_grants_the_pending_action_and_the_run        -> `a` chạy action + mở cổng;
                                                                       clear thì đóng lại
harness-cli (controller/UI):
  t06_the_wider_grant_is_offered_for_reads_and_not_for_writes       -> `a` trên write không có tác dụng
  t06_the_read_only_key_grants_the_action_and_the_run               -> quyết định + nhãn transcript
  t06_the_read_only_grant_does_not_survive_the_turn                 -> [true, false] quanh RunTerminal
```

## 3h. File trong một yêu cầu — không chỉ ảnh (lượt này)

### 3h.1. Vì sao có mục này

Người dùng báo hai điều trong một câu: dán ảnh vào terminal **không hoạt động**, và muốn **support
cả file**. Điều tra ra hai nguyên nhân khác nhau, và chỉ một trong hai là lỗi code:

1. **Binary đang chạy không có tính năng ảnh.** `target/release/ha.exe` trên máy này là bản dựng
   `18/09/2026 16:29`, tức **trước** hai commit ảnh (`62e3f98`, `4225363`). Trong binary đó không
   có chuỗi `arboard`, không có `image ready: `, không có `/image` — nên phím cũng như lệnh đều
   không tồn tại. Đây là **binary cũ**, không phải code sai; muốn có tính năng thì phải dựng lại.
2. **Đường dẫn không phải ảnh bị bỏ qua im lặng.** `from_message` chỉ nhận ảnh; một file text tên
   trong tin nhắn không được gắn, không được báo, và model chỉ thấy đường dẫn trần. Đây là **thiếu
   sót thiết kế** thật, và là phần lượt này sửa.

Một dữ kiện của terminal phải nói rõ, vì nó quyết định cả thiết kế: **Ctrl-V là phím của terminal,
không phải của app.** Windows Terminal giữ nó cho paste của chính nó, nên app không bao giờ thấy
phím; cái app nhận được là *text* đã dán. Vì vậy mọi tính năng "dán" phải có một đường **không phụ
thuộc bàn phím**: ở đây là `/image` và `/attach`.

### 3h.2. Hợp đồng đã chốt

1. **Bytes quyết định, tên file không quyết định.** File có magic PNG/JPEG/GIF/WebP đi đường ảnh
   (block `content`), kể cả khi tên là `.txt`. Tên `.png` mà bytes không phải ảnh bị từ chối kèm
   lý do, không bị quote thành text.
2. **Text đi trong chính message.** API không có block cho file, nên nội dung file được nối vào
   `request.text` — và vì `request.text` là thứ được persist vào packet, lượt sau vẫn thấy đúng
   những gì lượt trước đã đọc. Không thêm field nào vào `RunRequest`.
3. **Khối file tự khai nó là gì.** Mỗi file nằm giữa `===== file: <path> (<label>) =====` và
   `===== end of <name> =====`, cả khối mở đầu bằng một câu nói rõ đây là **tài liệu để đọc,
   không phải chỉ dẫn** — prompt injection qua nội dung file là rủi ro thật khi nội dung do người
   dùng đưa, và cách rẻ nhất để chặn là nói thẳng vai trò của nó cho model.
4. **Trần là ngân sách của session, không phải của lượt.** 256 KiB mỗi file, 1 MiB và 4 file mỗi
   lượt; đọc có trần (`take(MAX+1)`) nên một file 2 GiB không bao giờ được nạp vào bộ nhớ để rồi
   bị từ chối.
5. **Binary bị từ chối kèm lý do**, không bị cắt âm thầm: không phải UTF-8, hoặc có byte NUL →
   `not UTF-8 text` / `binary (it holds NUL bytes)`. Model đọc byte thô hoặc ký tự thay thế thì
   biết ít hơn là được nói thẳng loại file.
6. **Credential không bao giờ đi** — `.ssh/`, `*.pem`, `.env`, `credentials*`, như đường ảnh.
7. **`/attach <path>` kiểm tra file tồn tại rồi mới chèn path vào composer**, để sai đường dẫn bị
   báo tại chỗ (`no such file`) thay vì gửi một path không đọc được. Nó **không** mở pipeline thứ
   hai: path vào composer rồi đi đúng đường quét message như path gõ tay.
8. **Paste không có bitmap thì dán path.** `Ctrl-V`/`/image` thử bitmap trước; không có bitmap thì
   lấy text clipboard (path do Explorer copy), quote nếu có dấu cách, và **không** dán một khối
   text dài không liên quan — đó là việc của paste terminal, và một Ctrl-V lỡ tay không được biến
   bản nháp thành tài liệu.

### 3h.3. Test canh hợp đồng (tên thật, đã chạy)

```text
harness-cli (attachments, unit):
  a_text_file_is_read_into_the_message          -> khối file + header + câu "material to read"
  a_binary_file_is_refused_by_its_bytes         -> NUL, không-UTF-8, và .txt chứa PNG
  file_bounds_are_enforced_with_a_reason        -> 256 KiB, 4 file, cùng file hai lần là một
  a_pasted_file_path_is_read_the_same_way       -> path quote / trần / có dấu cách cuối
harness-cli (controller):
  t_attach_names_a_file_in_the_message_and_the_turn_reads_it
  t_attach_refuses_a_path_that_is_not_there_and_explains_itself
  t_a_pasted_path_is_quoted_once
harness-cli (wire, fixture SSE thật):
  i03_a_named_file_reaches_the_model_inside_the_message
harness-cli: 242 passed; 0 failed (cargo test -p harness-cli --bin ha)
```

### 3h.4. Khiếm khuyết bắt được trong lượt này — **chưa sửa, ghi lại thay vì im lặng**

Dựng test wire cho phần file thì mỗi lượt `ha chat --headless` trả về:

```text
workspace_escape: cannot hash workspace file: The process cannot access the file because
another process has locked a portion of the file. (os error 33)
```

Truy ra: `observe_workspace` **hash mọi file** nó đi qua (`workspace_fingerprint` →
`hash_file`), mà một store `SQLite` đang mở thì giữ **byte-range lock** trên
`harness.sqlite3-shm`/`-wal`; Windows trả `ERROR_LOCK_VIOLATION` (33) cho lần đọc đó. Đo được
bằng `SqliteStore` thật đặt trong workspace: `observe_workspace` trả đúng lỗi trên
(`crates/harness-cli/tests/known_defects.rs`, test `#[ignore]`, chạy tay để tái hiện).

Hai điều sai, và **không** điều nào được sửa trong lượt này vì mỗi điều là một thay đổi riêng:

1. **Chẩn đoán sai chỗ:** lock violation không phải `workspace_escape`; thông báo gửi người
   đọc đi tìm một lỗi đường dẫn không tồn tại.
2. **File bị khoá làm lượt chết**, thay vì bị bỏ qua: walk đã bỏ qua đường dẫn nhạy cảm, và
   một store do chính app sở hữu cũng thuộc loại "không phải nội dung workspace".

Bố cục mặc định **không** dính: trên Windows store nằm ở
`%LOCALAPPDATA%\HarnessAgents\data`, ngoài project. Nó chỉ tới được qua `HA_HOME` khi biến đó
trỏ vào chính project đang chạy — và khi đó lượt **đầu** vẫn chạy (store chưa tồn tại), mọi
lượt **sau** mới chết. Test wire vì vậy chạy đúng như một lần chạy thật: tiến trình con khởi
động trong project, còn `HA_HOME` ở ngoài project.

## 3i. Gõ `/` là ra danh sách lệnh (lượt này)

### 3i.1. Vì sao có mục này

Người dùng báo: *"khi tôi dùng codex: gõ `/` thì sẽ gợi ý câu lệnh, hiện tại project chưa có"*.
Đúng, và khoảng trống này có số đo: `LineEditor` **đã** tính `suggestions()` từ T03, nhưng thứ duy
nhất nhìn thấy được là **tiêu đề viền** của ô soạn thảo (`composer::hint` in `Tab: /help  /status
…`), một dòng bị cắt ở console hẹp và **không** nói lệnh nào làm gì. Người dùng phải biết lệnh
trước khi gõ `/`, hoặc mở `/help` rồi đọc lại - tức danh sách đến **sau** khi cần.

Codex CLI và Claude Code đều trả lời câu hỏi này bằng một **menu ngay trên ô soạn thảo**, lọc theo
từng ký tự. Mục này làm đúng thứ đó, trên đường ống đã có: bảng lệnh, editor, layout và renderer
hiện tại, không thêm dependency và không thêm renderer thứ ba.

### 3i.2. Hợp đồng đã chốt

1. **Menu không phải modal.** Ô soạn thảo **giữ** con trỏ và bản nháp vẫn nằm trên màn hình; menu
   chỉ chiếm các hàng ngay trên nó (`Plan::suggest`). Một modal thay cả vùng trên và lấy con trỏ
   (`layout::cursor_cell` trả `None` khi có modal), nên nó không thể là modal: người dùng đang gõ.
2. **Một bảng là nguồn duy nhất.** `SLASH_COMMANDS: [SlashCommand; 11]` mang `name`, `arguments`,
   `summary`; menu vẽ từ đó, và `view::help_lines()` **sinh** trang `/help` từ chính bảng đó
   (thêm một dòng viết tay: dạng kém riêng tư `/key <value>`, và dòng cuối về `Ctrl-C`/`Ctrl-D`).
   Vì vậy một lệnh mới không thể có trong menu mà thiếu ở `/help`, hay ngược lại.
3. **Mở từ ký tự `/` đầu tiên, hẹp dần theo từng ký tự.** Điều kiện hiện danh sách: buffer bắt đầu
   `/`, **không** có khoảng trắng (tức con trỏ còn ở trong từ lệnh), và **chưa** là một lệnh hoàn
   chỉnh. `/help` gõ đủ thì menu tắt: một hàng lặp lại đúng thứ vừa gõ là tiếng ồn.
4. **Trần 6 hàng, và cửa sổ đi theo con trỏ chọn.** Nhiều hơn 6 gợi ý thì danh sách **cuộn theo
   highlight** (`suggest::window`) chứ không cắt: lệnh đang chọn **luôn** nhìn thấy. Hàng lấy từ
   vùng live block, **không** lấy từ ô soạn thảo - cùng nguyên tắc "bản nháp đang gõ không bị ép".
5. **Bốn phím, và chỉ khi menu đang được vẽ.** `↑`/`↓` đổi highlight (bão hoà hai đầu như picker,
   không quấn vòng), `Tab` và `Enter` nhận hàng đang chọn, `Esc` đóng menu (lần gõ tiếp theo mở
   lại). Quyết định nằm ở **controller** (`suggestion_menu_open`), không ở editor: editor chỉ giữ
   danh sách và highlight, còn "có nhìn thấy hay không" chỉ host biết.
6. **Menu không lấy phím ở nơi nó không được vẽ.** Hai nơi: **plain mode** (không có menu) và **khi
   một panel/picker/overlay đang mở** (panel thay menu bằng chính nó). Cùng một điều kiện mà
   `layout::plan` dùng để chừa hàng, nên "vẽ" và "nhận phím" không thể lệch nhau. Hệ quả: `/he` +
   Enter ở plain mode vẫn là `unknown command /he` như trước, không tự hoàn thành một danh sách
   người dùng chưa từng thấy.
7. **Enter hoàn thành lệnh gõ dở, Enter thứ hai mới chạy.** `/he` + Enter → buffer thành `/help`;
   Enter nữa → mở panel `/help`. Trước lượt này `/he` + Enter trả `unknown command /he`.
8. **Nhận gợi ý KHÔNG thêm dấu cách.** Cố ý: `/key ` sẽ biến các ký tự gõ sau đó thành dạng **lộ**
   của lệnh (`/key <value>`), trong khi `/key` trần là đường **mask**. Một Enter thừa để chạy lệnh
   là giá rẻ hơn nhiều so với việc đẩy người dùng vào dạng kém riêng tư mà họ không chọn.
9. **Buffer secret không bao giờ mở menu.** `refresh_suggestion` thoát sớm khi `self.secret`, kể cả
   khi có ai đó dán `/re` vào lúc đang nhập key.

### 3i.3. Đổi hợp đồng có chủ ý (ghi lại, không đổi ngầm)

| Trước | Sau | Lý do |
|---|---|---|
| `Tab` chỉ hoàn thành khi có **đúng một** gợi ý; nhiều gợi ý thì `Tab` không làm gì | Menu đang vẽ: `Tab` nhận **hàng đang chọn** (mặc định hàng đầu) | Đó là điều một menu có highlight để làm; luật cũ vẫn đúng ở tầng editor và vẫn áp khi menu không được vẽ |
| `Enter` trên `/he` → notice `unknown command /he` | `Enter` hoàn thành hàng đang chọn; Enter lần sau chạy lệnh | Gõ dở rồi Enter là ý định rõ ràng; thông báo lỗi cho một lệnh gần đúng là câu trả lời vô ích |
| Tiêu đề viền in `Tab: /help  /status  …` (danh sách bị cắt) | Tiêu đề in `↑↓ chọn · Tab/Enter nhận · Esc đóng · N lệnh` | Viền nói **phím**, menu nói **nội dung**; đếm `N` cho biết còn bao nhiêu lệnh ngoài cửa sổ |
| `completions(prefix) -> Vec<&'static str>` | `matching(prefix) -> Vec<&'static SlashCommand>` | Menu cần cả `summary`/`arguments`, không chỉ tên; `completions` không còn ai gọi nên bị xoá thay vì để hai đường |
| `/help` là **11 dòng viết tay** | `/help` **sinh** từ bảng + 1 dòng `/key <value>` + 1 dòng cuối | Hai bản danh sách là hai bản sẽ lệch nhau; test canh cả hai chiều |
| Thứ tự `/help`: `/new` trước `/more` | Theo đúng thứ tự bảng: `/more` trước `/new` | Một thứ tự duy nhất cho menu và trang help |

### 3i.4. Test canh hợp đồng (tên thật, đã chạy)

```text
harness-cli (editor, input.rs):
  slash_the_menu_opens_on_the_slash_and_narrows_with_every_letter
  slash_the_arrows_move_the_highlight_and_stop_at_both_ends
  slash_accepting_the_highlight_never_appends_a_space   -> "/ke"+accept == "/key", rồi Enter -> Submit("/key")
  slash_escape_hides_the_menu_until_the_next_edit
  slash_a_secret_buffer_never_offers_a_command
  slash_the_command_table_is_unique_and_every_row_is_described
  t03_tab_completes_only_a_unique_slash_command          (editor vẫn từ chối danh sách mơ hồ)
harness-cli (controller):
  slash_the_snapshot_carries_the_menu_and_the_highlight
  slash_tab_accepts_the_row_the_menu_has_highlighted     -> "/" + Down Down + Tab == "/key"
  slash_enter_completes_a_half_typed_command_then_runs_it -> không submit, rồi mở panel /help
  slash_the_menu_never_takes_a_key_where_it_is_not_drawn -> plain: "unknown command /he";
                                                            overlay mở: cũng vậy
harness-cli (layout + widget):
  slash_the_menu_sits_above_the_composer_which_keeps_the_cursor
  slash_a_long_list_is_capped_and_the_draft_is_never_squeezed
  slash_a_panel_hides_the_menu
  slash_every_command_has_a_row_that_says_what_it_does
  slash_a_row_shows_the_argument_a_command_takes
  slash_the_highlight_moves_with_the_selection
  slash_a_long_list_windows_around_the_highlight
harness-cli (khung hình đã vẽ, tui/mod.rs):
  slash_typing_a_slash_paints_the_menu_above_the_composer -> "❯ /help" ở hàng TRÊN "> /",
                                                             viền ghi "Tab/Enter nhận",
                                                             "/at" -> "❯ /attach <path>",
                                                             Tab -> "> /resume" và menu biến mất
harness-cli (một bảng, hai chỗ đọc):
  slash_the_help_page_and_the_menu_read_one_table
harness-cli: 260 passed; 1 failed; 1 ignored (cargo test -p harness-cli --bin ha --locked)
```

Con số 260/1 là **số đo có ngày** (lượt này, toolchain 1.97.1, Windows): 242 test trước đó cộng
**19** test mới của mục này. Test đỏ là
`k03_a_saved_key_is_restricted_to_this_account_by_an_acl` - đỏ **đúng như thiết kế** trong phiên bị
từ chối `icacls` (mục 3d.4, evidence 11.2/11.7), không liên quan tới menu.

### 3i.5. Ranh giới của bằng chứng

- **Chưa có ca PTY** cho menu: ConPTY không chạy được trong phiên này (mục 12 của
  [evidence HA_LAUNCH](../evidence/HA_LAUNCH.vi.md)). Bằng chứng mạnh nhất hiện có là **khung hình
  đã vẽ** (`ScriptedRenderer` + `TestBackend`), đúng tầng mà T03-T07 vẫn dùng.
- **Mô tả lệnh bị cắt ở console hẹp.** Tên lệnh nằm ở cột đầu nên thứ bị cắt là phần mô tả, không
  phải lệnh; hàng không tự ngắt dòng (menu là danh sách, không phải văn bản).
- **Chuột không được hỗ trợ** trong menu: ô soạn thảo không bật mouse capture (mục 3e.3), nên chỉ
  có bàn phím.
- Phím `↑`/`↓` **không** cuộn panel `/help`/`/more` (lỗi chữ đã ghi ở mục 3e.2); mục này không sửa
  nó, và cũng không làm nó nặng thêm: menu chỉ tồn tại khi **không** có panel nào mở.

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
