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
