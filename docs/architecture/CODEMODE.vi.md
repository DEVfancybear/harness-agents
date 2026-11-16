# Codemode: một chương trình thay cho nhiều lần gọi tool

[English](CODEMODE.en.md) | Tiếng Việt

Khi một bước cần nhiều lần gọi tool mà model chỉ lọc, đếm hay gộp kết quả,
mỗi kết quả vẫn vào cuộc hội thoại và được gửi lại ở mọi request sau đó.
Codemode cho model viết bước đó thành một chương trình JavaScript: chương
trình gọi các tool bằng `await tools.<name>(args)`, và chỉ những gì nó in ra
bằng `text()` hay `console.log` cùng giá trị nó trả về mới quay lại. Các lần
gọi nó đã làm, và toàn bộ kết quả của chúng, nằm ngoài context.

Nó theo codemode của pi (`@earendil-works/pi-codemode`, package ở
`github.com/earendil-works/pi/tree/main/packages/codemode`). pi chạy chương
trình trong QuickJS biên dịch sang WebAssembly, trên một worker thread. ha
**không có VM**: chương trình được chạy bởi một interpreter viết bằng Rust,
trong crate `harness-codemode`, trên một thread riêng.

Nó **tắt theo mặc định**: nó thêm một tool, cùng phần khai báo các tool, vào
mọi request. Bật bằng `/codemode on`, `"codemode": true` trong settings.json,
hoặc `HA_CODEMODE=on`, biến này thắng setting.

Code: `crates/harness-codemode` (lexer, parser và evaluator, các built-in,
dòng `// @options`, phần khai báo, runner),
`crates/harness-tools/src/runtime/codemode.rs` (tool, các tool một chương
trình gọi được, store), `run_codemode`, `codemode_batch`, `codemode_gate` và
`codemode_request` trong `crates/harness-tools/src/runtime/turn_driver.rs`
(cổng cho các lần gọi của chương trình), và công tắc trong
`crates/harness-cli/src/host/coordination/codemode.rs`.

## 1. ha đã tương đương chưa? Bảng khoảng trống

Trước đó ha có vài tính năng gần với codemode. Không cái nào là codemode:

| codemode của pi | ha đã có gì | Khoảng trống, và đã làm gì |
|---|---|---|
| Model viết một chương trình gọi các tool của host; các lần gọi lồng nhau và kết quả của chúng không bao giờ vào context của LLM | Python kernel (`python_repl`, `HA_REPL`) chạy code model viết, và `rlm.mcp.call_tool` gọi được tool MCP từ đó. Nó không gọi được tool của chính ha (`read_file`, `search_text`, `run_process`, `web_search`, memory, ...), và cần Python 3.11+ | **Thiếu.** Đã port: tool `codemode`, chương trình của nó gọi được mọi tool của lượt |
| Khả năng duy nhất của chương trình là gọi các tool được đưa vào: không timer, `fetch`, `process`, `require`, module | Kernel là Python đầy đủ, có cả hệ điều hành trong tay; nó được duyệt như `run_shell` | **Thiếu.** Đã port: interpreter không có khả năng nào ngoài `tools` |
| Mỗi lần gọi lồng nhau là lần gọi của host | `use_tool` và các tool hoãn lại (`HA_TOOL_TIERS`) gọi một tool theo tên, mỗi lần gọi là một lần gọi model. Action bank (`/bank`) chạy các lệnh model đã định sẵn, với tham số cố định và guard, nhưng không có logic giữa chúng | **Thiếu.** Một chương trình tính tham số của lần gọi sau từ kết quả lần trước. Dùng lại: mỗi lần gọi lồng nhau qua cổng của lượt (bên dưới), kể cả tool hoãn lại |
| `tools.<name>` với tên được chuẩn hóa thành identifier (`my-tool` là `tools.my_tool`), `ALL_TOOLS`, lỗi nêu tên gần đúng cho một thành viên không tồn tại | Không có | **Thiếu.** Port nguyên như vậy |
| `text()`, `console.*`, `exit()`, output và giá trị của kết quả, các loại lỗi `script`, `timeout`, `aborted`, `sandbox`, số dòng `codemode.js:<line>` | Output in ra của một cell | **Thiếu.** Đã port |
| `store(key, value)` / `load(key)`: JSON nhỏ giữ qua các lần chạy, 256 Ki ký tự một giá trị, 1 Mi tổng cộng, chỉ lần chạy thành công mới ghi | Kernel giữ biến giữa các cell | **Thiếu** cho codemode. Đã port; store sống suốt phiên app, theo từng cuộc hội thoại |
| Dòng đầu `// @options: {"timeout_ms", "max_output_tokens"}` | `timeout_ms` của `python_repl` | **Thiếu.** Đã port; `timeout_ms` là hạn chót của lần chạy (tối đa 30 phút) và `max_output_tokens` là cỡ của phần model đọc |
| `renderDeclarations` / `schemaToType`: khai báo TypeScript từ JSON Schema của tool, tag ràng buộc, `$ref` cục bộ | `tool_schema` trả JSON Schema thô của một tool | **Thiếu.** Đã port; mô tả của tool khai báo các tool model được gửi, đủ ngắn cho một ngân sách, và nêu tên các tool hoãn lại |
| Giới hạn output (16 Mi ký tự, 100.000 mục), giới hạn bộ nhớ, đệ quy sâu bắt được, script chờ một promise không gì giải quyết được thì hỏng ngay | Timeout của cell và cắt output | **Thiếu.** Đã port |
| Lần gọi chưa được await khi script kết thúc bị hủy và ghi là `cancelled` | - | Port khác đi: một lần gọi chạy khi được await, nên lần gọi không ai await thì không bao giờ chạy và được ghi là đã hủy |
| Hook duyệt cho lần gọi lồng nhau bị bỏ qua trừ khi host chuyển chúng qua `runToolCall()` | Cổng của ha | Tốt hơn ở ha: mỗi lần gọi lồng nhau qua đúng cổng như lần gọi của chính model |
| QuickJS biên dịch sang WebAssembly, worker thread, `InlineCodemodeSandbox`, `RemoteCodemodeSandbox`, Durable Object của Cloudflare, `acquireWorker`, `onWorkerStarted`, interrupt budget | - | **Không port**: ha không cần VM. Interpreter chạy trên thread riêng; hạn chót và lệnh hủy được kiểm tra mỗi 256 bước |
| `sandbox.start()`: lần chạy tất định (đồng hồ ảo, ngẫu nhiên có seed, kết quả theo thứ tự `seq`, fingerprint) để chạy lại sau khi khởi động lại | ha không chạy lại một chương trình từ log | **Không port**: không có gì trong ha dùng nó |
| `CODEMODE_SOURCE_GRAMMAR`, grammar Lark cho tool input ràng buộc bằng grammar | Provider của ha nhận tham số tool dạng JSON | **Không port** |
| `image()` | Kết quả tool có ảnh đến model dưới dạng đính kèm | **Không port**: kết quả của chương trình là văn bản |

## 2. Ngôn ngữ

Một chương trình là thân của một async function: `return` và `await` ở mức
trên cùng đều dùng được. Interpreter đọc thứ JavaScript mà chương trình được
viết bằng: `const`/`let`/`var`, function và arrow (cả async), destructuring
có giá trị mặc định và rest, spread, template literal, optional chaining và
`??`, `if`/`for`/`for...of`/`for...in`/`while`/`do`/`switch`, label,
`try`/`catch`/`finally`, biểu thức chính quy (có look-around và
back-reference, qua `fancy-regex`), và tự chèn dấu chấm phẩy. Các built-in
là `JSON`, `Math`, `Object`, `Array`, `String`, `Number`, `Boolean`,
`Promise`, `Map`, `Set`, `RegExp`, `Date` (UTC), các lỗi,
`parseInt`/`parseFloat`, các hàm URI và `structuredClone`. Object, `Map` và
`Set` giữ thứ tự chèn.

Những gì chương trình không dùng được sẽ bị từ chối kèm thông báo nói rõ:
`class`, generator, getter và setter, `import`/`export`, BigInt, tagged
template, `with`. Không có timer, nên `setTimeout` không được định nghĩa và
một `new Promise` không ai resolve sẽ hỏng ngay. Vị trí trong chuỗi đếm theo
ký tự (Unicode scalar value), không theo code unit UTF-16.

## 3. Các lần gọi của chương trình

`tools.<name>(args)` trả một promise. Lần gọi chạy khi có thứ await nó;
`await Promise.all([...])` đưa cho host cùng lúc mọi lần gọi trong danh
sách. Lần gọi không ai await thì không bao giờ chạy và được liệt kê trong kết
quả là chưa chạy.

Driver trả lời mỗi lô như trả lời một lô lần gọi của chính model:

1. Lần gọi tới một tool điều khiển (`codemode`, `action_bank`,
   `drop_context`, `tool_schema`, `use_tool`, `ask_user`) hay tới một tool mà
   lượt không có bị từ chối. Tham số được kiểm tra theo schema của tool, cùng
   cách ép kiểu và cùng thông báo lỗi như lần gọi của model.
2. Mỗi lần gọi qua cổng của nó theo thứ tự chương trình tạo ra: policy,
   command guard, hook `pre_tool_use`, lần duyệt, intent bền vững. Lần gọi bị
   cổng từ chối là một `Error` mà chương trình bắt được.
3. Các side effect chạy song song và hoàn tất theo thứ tự, kèm receipt và
   hook `post_tool_use`. Một lần gọi phải chạy riêng (ghi file, chạy process)
   làm cả lô chạy lần lượt từng lần gọi.
   - Các rule học được từ lỗi của project áp dụng như với lần gọi của model.
     Điều các hook trả lời đến model cùng kết quả của chương trình, và một
     hook kết thúc lượt thì lượt kết thúc.
   - Một chương trình viết lại được file nó vừa đọc: hash file của mỗi lần
     gọi được giữ ngay khi nó hoàn tất, nên lần ghi sau mang theo nó.
   - Khi chương trình bị dừng (hết hạn, hoặc lượt bị hủy), các lần gọi đang
     chạy bị hủy và hoàn tất kèm receipt trước khi chương trình biết.
   - Một chương trình được gọi 1.000 lần. Chúng không tính vào giới hạn số
     lần gọi tool của chính lượt, và việc từ chối một lần gọi lặp lại lần
     vừa hỏng không áp dụng cho chúng: logic của chương trình quyết định nó
     gọi gì.
4. Mỗi lần gọi là một thẻ riêng cho người dùng (`<id lần gọi codemode>.1`,
   `.2`, ...) và là một execution của lượt, nên danh sách file đã đổi, lời
   nhắc kiểm tra và bằng chứng của goal đều thấy nó. Model chỉ thấy kết quả
   của chương trình.

Một lần gọi trả về văn bản model đáng lẽ đã đọc cho nó; `read_file` trả văn
bản của file không kèm số dòng (secret vẫn bị che, và file dài bị cắt kèm
ghi chú). Chương trình tự parse kết quả JSON.

## 4. Model đọc được gì

Output theo thứ tự, rồi `=> <JSON của giá trị trả về>` hoặc lỗi, rồi các lần
gọi không thành công:

```text
functions: 1, 3
=> 4
```

```text
started
Script error: Error: stop here
    at codemode.js:2
[1 of 3 tool calls did not succeed: run_process failed]
```

Output bị cắt ở giữa khi quá 60.000 ký tự, hoặc quá `max_output_tokens` × 4
khi dòng options đặt nó. Lần chạy hỏng giữ output tới lúc hỏng và không ghi
gì vào store.

## 5. Giới hạn

| Giới hạn | Giá trị |
|---|---|
| Một lần chạy, kể cả các lần gọi | 5 phút; `timeout_ms` tới 30 phút |
| Output | 16 Mi ký tự hoặc 100.000 lần gọi `text()`/`console`; quá một trong hai thì script hỏng, kể cả bên trong `catch` |
| Bộ nhớ | khoảng 512 MiB chuỗi, mảng và object; quá thì script hỏng vì hết bộ nhớ |
| Đệ quy | 400 lần gọi lồng nhau, rồi một `RangeError` bắt được |
| Store | 256 Ki ký tự JSON một giá trị, 1 Mi tổng cộng |
| Mảng | 50 triệu phần tử |
| Lần gọi | 1.000 một chương trình |

Mọi thứ một lần chạy tạo ra được giải phóng khi nó kết thúc, kể cả tham chiếu vòng.

Script bị dừng được báo ở lần kiểm tra kế tiếp; script kẹt trong một
built-in dài không được chờ quá 3 giây.

## 6. Chẩn đoán

Mỗi lần chạy ghi một bản ghi `codemode/run` (số lần gọi, số lần gọi hỏng,
số mục output, mili giây, loại lỗi), và mỗi lần gọi lồng nhau ghi intent,
receipt và bản ghi cổng như mọi lần gọi. `/codemode status` cho biết nó có
bật không và cái gì đang ghi đè.

## 7. Kiểm thử

- `crates/harness-codemode`: lexer, parser, giá trị, phần khai báo và dòng
  options (unit test); script chạy trọn vẹn - output, giá trị trả về, lần gọi
  và lô, store, lỗi và số dòng của chúng, built-in, biểu thức chính quy,
  promise, các khả năng bị thiếu, hạn chót, các giới hạn
  (`tests/script.rs`); hạn chót và lệnh hủy của runner khi một tool đang chạy
  hay script đang tính (`tests/execute.rs`).
- `crates/harness-cli/tests/codemode.rs`: vòng lặp lượt thật với một model
  theo kịch bản - các lần gọi qua cổng và chỉ output đến được model, lần ghi
  bị từ chối là lỗi chương trình bắt được, tham số được kiểm tra và tool điều
  khiển không gọi được, chương trình hỏng báo lỗi kèm output, và tool chỉ được
  đưa ra khi bật.
- `scripts/testing/live-tests/codemode.sh`: `ha` thật trên một model thật.
