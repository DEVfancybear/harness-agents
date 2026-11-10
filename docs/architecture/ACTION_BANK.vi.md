# Action bank: các lệnh chạy không cần một bước model

[English](ACTION_BANK.en.md) | Tiếng Việt

Sau mỗi kết quả tool, một harness agent gọi model lần nữa, kể cả khi một bước
trước đã quyết định xong hành động tiếp theo: sau khi ghi một module thì lệnh
build đã biết; sau một lần build thành công thì lệnh test đã biết. Action bank
cho model ghi các lệnh như vậy một lần, kèm điều kiện để mỗi lệnh được chạy, rồi
harness chạy lệnh đã sẵn sàng mà không gọi model.

Đây là guarded decision reuse của HarnessMM (Jiang và cộng sự, "HarnessMM:
Recursive Self-Improvement for Harness Token Efficiency", arXiv 2610.32813, mục
4, C.4 và D). ha cài đặt cơ chế thực thi - executable decision graph (EDG),
guarded decision reuse (GDR) và certified callback substitution (CCS) - chứ
không cài vòng tự cải tiến prompt offline (RSI) của bài báo.

Nó **mặc định tắt**. Chính kết quả âm của bài báo (Terminal-Bench, GAIA2) là
những lần chạy mà bank hiếm khi dùng được, nên viết bank tốn output nhiều hơn số
lần gọi model nó tiết kiệm. Bật bằng `/bank on`, `"actionBank": true` trong
settings.json, hoặc `HA_ACTION_BANK=on`; hãy đo trên task của bạn (mục 8).

Code: `crates/harness-tools/src/runtime/bank.rs` (bank, guard, receipt, trait
selector), `TurnDriver::run_turn_inner`, `bank_settled` và `bank_next` trong
`crates/harness-tools/src/runtime/turn_driver.rs` (bộ thực thi), `turn_steps`
trong `crates/harness-runtime/src/lib.rs` và `record_issued_call` trong
`crates/harness-runtime/src/control/inbox.rs` (lưu bền), phần gộp cho Anthropic
trong `crates/harness-providers/src/protocols/anthropic.rs`, công tắc trong
`crates/harness-cli/src/host/coordination/action_bank.rs` và selector trong
`crates/harness-cli/src/host/integrations/jev.rs`.

## 1. Hợp đồng của bài báo, và cách ha ánh xạ

| HarnessMM | ha |
|---|---|
| Một bước chậm đưa ra hành động hiện tại và một EDG hữu hạn, không chu trình, gồm các quyết định tương lai `(hành động, tham số literal, lệnh trước, guard)` | Model gọi `action_bank` trong cùng response với các lệnh khác. Mỗi node có `id`, `after` (`root` = các lệnh khác của response này, hoặc id một node khác), `when` (guard), `tool` và `arguments`. Tối đa 8 node |
| Literal closure, kiểm bằng schema | Tham số của mỗi node phải qua schema của tool ngay khi bank được viết, như một lệnh gọi. Node chỉ được dùng tool trong danh sách tool của lượt, và không được là `action_bank`, `tool_schema`, `use_tool` hay `ask_user` |
| Không chu trình, lệnh trước phải tồn tại | Kiểm khi viết. Bank sai bị từ chối, lý do nằm trong kết quả của lệnh gọi, và không giữ gì |
| GDR: sau một receipt, đủ điều kiện = lệnh trước xong, guard đúng, tham số đóng | Khi một lệnh xong, các node có `after` là lệnh đó (C.4: "các mục có lệnh trước là lệnh vừa chạy") được kiểm với kết quả của nó. Guard chỉ đọc kết quả đã thấy |
| Đồ thị sống qua các lần chuyển tool | Bank sống suốt lượt. Kết quả của một node đã chạy làm các node con của nó sẵn sàng |
| CCS: một node đủ điều kiện chạy qua giao diện gốc, không có lệnh gọi chậm | Một node sẵn sàng chạy qua đúng pipeline cho một lệnh của model: gỡ use_tool, kiểm lệnh hỏng, regulator, learned rule, kiểm schema, policy, command guard của jev, hook, duyệt, intent và receipt. Không gọi model |
| Nhiều node đủ điều kiện: một selector có kiểu trả về một id hoặc bỏ phiếu trắng | Quyết định `bank` của jev (`choice` trên các id node cộng một lựa chọn bỏ phiếu trắng). Không có jev, dưới ngưỡng, hết giờ (10 giây) hoặc bỏ phiếu trắng: gọi model |
| Tập rỗng, bỏ phiếu trắng, trạng thái không hỗ trợ: actor chậm | Không node nào sẵn sàng, bỏ phiếu trắng, kết quả không rõ, steering, cảnh báo lặp, ghi chú regulation, hết chỗ dưới `max_tool_calls`: gọi model như thường |
| Đồ thị mới thay phần chưa giải quyết; một node chạy một lần | `action_bank` mới thay các node chưa chạy; mỗi node được giải quyết (chạy hoặc bỏ) nhiều nhất một lần; selector chỉ trả về id |
| Một chứng nhận cho mỗi lần bỏ qua | Một bản ghi diagnostics `bank/dispatched` cho mỗi lần (mục 5) |
| Tính chi phí trọn task | Usage của provider trong lượt đã gồm phần viết bank và mọi lần quay về model; bank thêm bảng đếm riêng (mục 6) |

Không lấy từ bài báo: vòng RSI sửa prompt, và request nhanh "single-encoding"
của bài báo (selector của ha là Jev, đọc state gọn của nó; transcript đầy đủ vẫn
ở với model).

## 2. Guard

Guard đọc kết quả công khai của lệnh nó đi sau:

| Guard | Đúng khi |
|---|---|
| `ok` | lệnh đã chạy và thành công: tiến trình hoặc lệnh git thoát 0, tool file trả về. Kết quả của một plugin hay MCP, vốn có thể chứa lỗi riêng, không phải `ok` cũng không phải `failed`; vẫn kiểm được phần chữ của nó |
| `failed` | lệnh đã chạy và thất bại: mã thoát khác 0 hoặc hết giờ |
| `exit_code: N` | mọi tiến trình của lệnh (hoặc của root) đều thoát với N; `exit_code: 0` còn cần không lệnh nào thất bại |
| `output_contains: "text"` | phần kết quả model được xem có chứa đoạn chữ |
| `output_lacks: "text"` | không chứa |
| `changed` | một lần ghi, sửa hay patch đã đổi file (hash trước và sau khác nhau) |

Một lệnh bị từ chối, bị chặn, hỏng, bị huỷ hoặc không rõ kết cục là *không rõ*,
và không guard nào đúng trên nó: model đọc nó. Các lệnh của root được đọc như một
kết quả: không rõ nếu có một lệnh không rõ, thất bại nếu có một lệnh thất bại, mã
thoát cuối cùng, mọi đoạn chữ. Bank được viết một mình trong response có root
rỗng, tức là `ok`: node đầu tiên chạy ngay, đúng chỗ lệnh của chính response lẽ ra chạy, và được đếm là `immediate`, không phải một bước đã tiết kiệm.

## 3. Lệnh được bỏ qua hiện ra với model thế nào

Bước thật tiếp theo của model thấy mỗi lệnh harness đã chạy như thể chính nó đã
gọi: một message assistant chứa lệnh, rồi kết quả. Dòng đầu kết quả nói harness
đã chạy nó:

```
[action bank node test: run by the harness without a model step]
```

Message assistant được đánh dấu `harness_issued` (`ProviderMessage`). Nó không
mang reasoning, vì không có response nào của model chứa nó. Theo từng protocol:

- **Chat Completions** và **Responses**: gửi như lệnh gọi và kết quả
  (`tool_calls` + `tool`; `function_call` + `function_call_output` không có item
  reasoning, như các lệnh của một lượt cũ được phát lại).
- **Anthropic có thinking**: API từ chối message assistant cuối không bắt đầu
  bằng một khối thinking có chữ ký. Lệnh do harness phát và kết quả của nó được
  gộp vào một khối text sau các kết quả đứng trước, nên message assistant cuối vẫn
  là của model:
  `[call from your action bank, run by the harness without a model step] run_process {...}` và kết quả.
  Không có thinking thì gửi như `tool_use` và `tool_result`.

Unit test dựng request body của từng protocol cho một transcript như vậy.

## 4. Lưu bền

Các lượt sau và một lần resume dựng lại hội thoại từ các provider attempt đã lưu
(`turn_steps`). Không attempt nào chứa lệnh do harness phát, nên receipt của nó
sẽ là một kết quả không có lệnh. Vì vậy mỗi lần bỏ qua được ghi như một run
command đã áp dụng với `{"host": true, "issued": {call_id, name, arguments, node}}`
(không có `text`, nên không bao giờ bị đọc là steering), và `turn_steps` đặt nó
lại giữa hai bước nó đã nằm giữa, với kết quả từ receipt và dòng ghi chú. Câu trả
lời cho một lệnh `action_bank`, vốn không chạy tool nào, được ghi là
`{"host": true, "answered": {call_id, text}}`, nên bản phát lại đưa đúng câu trả
lời đó chứ không phải "this call did not run".

## 5. Chứng nhận

Mỗi lần bỏ qua ghi `bank/dispatched` với: `graph` (hash các node đã viết),
`node`, `predecessor` và `predecessor_receipt` (hash kết quả guard đã đọc),
`tool` và `args_hash` (đúng tham số đã chạy), `tool_receipt` (id receipt bền) và
`tool_receipt_hash`, `status`, `replaced_step` (bước model mà nó thay) và
`model_steps` (số lần gọi model của lượt đến lúc đó, không tăng). Các bản ghi
khác: `bank/authored` (graph, node, cạnh, `author_estimate`: ước lượng chi phí viết tính bằng token; diagnostics giấu mọi trường có tên giống token),
`bank/refused` (lý do), `bank/abstained` (ứng viên, có hỏi selector không và
input token của nó), `bank/discarded` (các node và lý do: `guard_false`,
`not_chosen`, `replaced`, `deferred`, `turn_end`) và `bank/turn` (bảng đếm của
lượt). `/bug-report` có mục **Action bank**.

## 6. Tính chi phí

`TurnOutcome.bank` đếm số bank được viết và bị từ chối, số node, số lệnh đã chạy
(= số bước model tiết kiệm), số lần bỏ phiếu trắng, số node bị bỏ, số lần gọi
selector và input token của chúng, và chi phí viết. Provider báo output token
của cả một response chứ không phải phần của bank, nên chi phí viết là **ước
lượng**: số byte tham số của lệnh bank chia 4. Chi phí trọn task là usage
provider của lần chạy, vốn đã gồm bước viết bank và mọi lần quay về model.

- `ha exec --json` có object `bank`: `enabled`, `authored`, `refused`,
  `nodes`, `dispatched`, `model_steps_saved`, `immediate`, `abstained`, `discarded`,
  `selector_calls`, `selector_input_tokens`, `author_tokens_estimate`.
- `/context` có dòng `Bank:` khi bank bật hoặc đã làm gì đó.

## 7. Công tắc

`/bank on|off|status` lưu `actionBank` trong settings.json; `HA_ACTION_BANK=on|off`
ghi đè nó. Áp dụng từ lượt sau. Child chạy theo setting của agent cha. Quyết định
`bank` của jev (`/jev bank on|off`, ngưỡng `jev.thresholds.bank`, mặc định 0.95)
là selector.

## 8. Kết quả đo

`scripts/testing/live-tests/bank.sh` trên `openai-codex/gpt-6-luna`, thinking
`max` (2026-11-08):

- **Cơ chế, khi prompt yêu cầu dùng bank.** Model viết một module và file test
  của nó, và trong cùng response đưa lần chạy test vào bank; lần chạy test sau
  đó chạy mà không gọi model (một chứng nhận, 6 lần gọi model thay vì 7), test
  qua, và một hội thoại được resume trả lời từ kết quả của lần chạy trong bank.
  Một test trong bank thất bại thì lệnh commit trong bank sau nó không chạy
  (`guard_false`) và không có gì được commit. Với `--approval auto-edit`, một
  lệnh `git add` trong bank vẫn qua cổng duyệt, bị từ chối, và model được gọi.
  Backend Responses của ChatGPT nhận mọi request body có lệnh do harness phát.
- **Phiên bản đầu, đã đo.** Khi chưa yêu cầu gọi bank cùng response, model này
  ghi file trong một response và gọi bank một mình ở response sau: node đầu của
  bank chạy đúng chỗ lệnh của chính model lẽ ra chạy, và không tiết kiệm gì.
  Trường hợp đó giờ được đếm là `immediate`, không phải bước đã tiết kiệm, và
  mô tả tool mở đầu bằng "trong cùng response".
- **A/B, prompt trung tính** (ba task code nhỏ, bank tắt và bật, hai lần lặp,
  cùng prompt cho cả hai bên): thành công 6/6 khi tắt và 6/6 khi bật; số lần
  gọi model trung bình 9,00 và 8,83 (-1,9%), input token 96.020 và 98.289
  (+2,4%, schema của tool bank trong mọi request), output token 1.294 và 1.363
  (+5,3%). Model **không viết bank nào** trong mọi lần chạy, nên không bước nào
  được tiết kiệm: chênh lệch là nhiễu giữa các lần chạy cộng chi phí schema.

Vậy, như bài báo thấy với Terminal-Bench và GAIA2, bank chỉ có lợi khi model
thật sự đưa các lệnh tiếp theo vào bank. Với model này, điều đó xảy ra khi được
yêu cầu; chỉ được cấp tool thì nó tốn thêm một chút input mà không tiết kiệm gì.
Hãy để tắt trừ khi task hoặc chỉ dẫn của bạn làm bank được dùng thường xuyên, và
đo bằng `bank.sh` (`AB=1`, `REPEATS`).
