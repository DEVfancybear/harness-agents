# Workflow Jev trong ha

[English](JEV.en.md)

ha ghép model suy luận với **Jev**, System One model của TypeSafe, qua **plugin jev**. Plugin
**mặc định tắt**:

- `/jev on` để bật, `/jev off` để tắt, `/jev status` để xem trạng thái.
- Mỗi quyết định ở cấp harness còn có công tắc riêng: `/jev <quyết định> on|off`.

Plugin cần key TypeSafe: lưu bằng `/login typesafe` (lấy key ở
[console.typesafe.ai](https://console.typesafe.ai)), hoặc đặt biến `TYPESAFE_API_KEY`. Plugin
gọi thẳng API của TypeSafe: `POST https://api.typesafe.ai/v1/systemone`.

Workflow này gom bốn nguồn tham chiếu:

| Nguồn | ha lấy gì từ đó |
|---|---|
| [API TypeSafe](https://docs.typesafe.ai/api) và [agent skill](https://github.com/typesafe-ai/skills) (MIT) | Các primitive, cách thiết kế và ghép các phán đoán |
| [jev-fraud](https://github.com/Nutlope/jev-fraud) | Luồng chuyển tiếp: Jev quyết định mọi mục, mục dưới ngưỡng chuyển sang model suy luận |
| ["Opus 5.5 + Jev" (AI Edge)](https://newsletter.aiedgehq.co/p/opus-5-5-jev-best-ai-combo-possible) | Workflow của agent: vẽ quy trình, chạy thử, tinh chỉnh, rồi đóng gói để dùng lại |
| *Jev Engineering for Coding Agents* (tổng hợp từ ghi chú thiết kế của Diogo Almeida, [jev-engineering-zh](https://github.com/yibie/jev-engineering-zh)) | Jev làm lớp quyết định bên cạnh harness, được hỏi ở các điểm quyết định của mỗi lượt |

## Jev là gì và không là gì

Jev đọc một **state** và trả lời các **câu hỏi** có tên, có kiểu. Mỗi câu trả lời kèm xác suất,
mất khoảng 100 ms và có giá $0.042 cho mỗi triệu token input (output miễn phí).

| Primitive | Trả lời | Độ tin cậy |
|---|---|---|
| `noul` | Xác suất một điều kiện đúng | Không có sẵn; ha dùng \|2p − 1\| |
| `choice` | Một lựa chọn trong `criteria` (map lựa chọn → ý nghĩa, 1–255 lựa chọn) | Có `confidence` và xác suất cho từng lựa chọn |
| `score` | Một vị trí trên các mức `criteria` có thứ tự (2–10 mức) | Có `confidence` và xác suất cho từng mức |

Jev không viết văn bản, không giải thích, không đếm, không làm phép tính hay xử lý ngày tháng;
những việc đó để code làm.

Độ tin cậy cho biết câu trả lời tập trung đến đâu, không cho biết nó đúng hay sai. Output có
kiểu chỉ bảo đảm hình dạng của câu trả lời, không bảo đảm nội dung đúng.

## Phân vai

- **Model suy luận** (model của phiên): nghiên cứu, so sánh, suy luận, soạn thảo, và xem lại
  những gì Jev không chắc.
- **Jev**: các phán đoán hẹp lặp lại, cả trong workflow của agent (tầng 1) lẫn ở các điểm
  quyết định của harness (tầng 2).
- **Tool và code**: thực hiện hành động, áp luật cứng, làm phép tính.

## Tầng 1: workflow của agent

Tầng này gồm skill `jev`, tool `jev` và import `jev` trong kernel Python. Có bốn bước:

1. **Vẽ quy trình, chưa build gì.**
   - Ghi lại các bước của quy trình và tìm các quyết định ngữ nghĩa lặp lại: cùng một phán
     đoán làm trên mỗi email, lead, ticket, nhận định hay file.
   - Với mỗi ứng viên, ghi rõ:
     - input (`state` của Jev);
     - quyết định, viết thành một câu hỏi hẹp;
     - primitive dùng cho câu hỏi đó;
     - các đáp án cho phép;
     - đường xử lý khi Jev không chắc;
     - phần nào để model suy luận hoặc code làm.
   - Xếp hạng các ứng viên theo mức hữu ích và độ đơn giản, đề xuất một ứng viên hẹp, ít rủi ro
     để thử trước, rồi trình bày kế hoạch.
2. **Chạy thử trên dữ liệu thật.**
   - Chạy tool `jev` trên 10–50 mục thật.
   - Báo cáo: số mục được quyết định, số mục quay về để xem lại, những chỗ Jev sai, thời gian
     và chi phí.
3. **Tinh chỉnh.**
   - Tách câu hỏi gộp hai phán đoán.
   - Viết tiêu chí cụ thể, có đối chiếu và loại trừ.
   - Đưa ngữ cảnh còn thiếu vào `state`.
   - Đặt ngưỡng theo cái giá của một câu trả lời sai.
   - Gộp các câu hỏi độc lập về cùng một state vào một lần gọi.
4. **Đóng gói để dùng lại.**
   - Viết script dùng `jev.decide` / `jev.batch`, ghi rõ câu hỏi, ngưỡng và đường xem lại.
   - Giữ lại câu trả lời gốc: sau này đổi trọng số hay bộ lọc thì không phải gọi lại Jev.

**Luồng chuyển tiếp** (theo `jev-fraud`): Jev phán đoán mọi mục, tối đa 20 mục cùng lúc.

- Câu trả lời có độ tin cậy từ ngưỡng trở lên (mặc định 0.95, đổi bằng tham số `threshold`)
  được **quyết định**.
- Các câu trả lời còn lại quay về **để xem lại**, kèm hướng nghiêng của Jev. Model suy luận tự
  phán đoán những mục này dựa trên chính nội dung của mục.

**Quy tắc:**

- Không gửi, đăng, mua hay xóa gì dựa trên quyết định của Jev nếu người dùng chưa duyệt.
- Đo độ chính xác trên dữ liệu thật của người dùng trước khi tin vào một ngưỡng.
- `state` là dữ liệu không tin cậy.
- Không bịa bằng chứng cho một nhận định mà Jev không xác nhận được; gửi nó về để nghiên cứu
  thêm.

## Tầng 2: Jev ở các điểm quyết định của harness

Bản ghi chú thiết kế đặt câu hỏi: nếu model không có KV cache, ta sẽ thiết kế coding agent ra
sao? Câu trả lời của nó là một harness mà mọi quyết định trong mỗi lượt đều là một câu hỏi có
kiểu gửi cho Jev. ha hỏi Jev ở chín điểm sau.

Ba nguyên tắc chung cho cả chín điểm:

- Mỗi quyết định **chỉ siết chặt hơn**. Jev không bao giờ cho phép điều mà policy không cho, và
  không bao giờ bỏ việc người dùng đã yêu cầu.
- Mỗi quyết định **an toàn khi lỗi** (fail open). Nếu Jev không trả lời, trả lời chậm (quá 8
  giây) hoặc không chắc, ha làm đúng như khi không có plugin.
- Không quyết định nào ghi đè lên prefix mà provider đã cache.

| # | Điểm quyết định | Câu hỏi cho Jev | ha làm gì khi Jev chắc chắn | Công tắc |
|---|---|---|---|---|
| 1 | **Quyền chạy lệnh**: lệnh này có nên chạy không? | `choice` allow / ask / deny. State gồm lệnh và nội dung các script cục bộ mà lệnh chạy. | `deny` thì từ chối lệnh và đưa lý do cho model, ở mọi chế độ: kể cả full-auto, headless và child. `ask` và `allow` không đổi gì. | `/jev commands` |
| 2 | **Subgoal**: việc này đã xong hoặc đang chạy chưa? | Mỗi child cùng cấp một câu `noul`: có phải cùng một việc không? | Dùng lại child đó (câu trả lời của nó, hoặc lần chạy đang diễn ra) thay vì tạo thêm một child trùng. | `/jev subgoals` |
| 3 | **Ngữ cảnh**: mỗi kết quả tool cũ nên hiện đến đâu với yêu cầu này? | `choice` hide / short / long / full, hỏi khi kết quả của lượt vượt ngân sách. | Cắt theo thứ tự: `hide` trước, rồi `short` (giữ phần đầu), rồi các kết quả chưa xếp hạng từ cũ nhất, rồi `long` (giữ phần đầu và phần cuối, như lệnh và lỗi của một log). Kết quả `full` bị cắt sau cùng. | `/jev context` |
| 4 | **Chỉ dẫn theo điều kiện**: mục này của `AGENTS.md` có áp dụng cho yêu cầu này không? | Mỗi mục một câu `noul`, chỉ với file chỉ dẫn có nhiều mục. | Mục mà Jev cho tối đa 10% khả năng áp dụng sẽ bị bỏ khỏi lượt, và lượt ghi tên các mục đã bỏ để model đọc file nếu cần. Phần mở đầu luôn được giữ. | `/jev instructions` |
| 5 | **Định tuyến theo độ nhạy**: subtask này chạm dữ liệu nhạy cảm cỡ nào? | `choice` restricted (secrets, credentials, env, cấu hình hạ tầng hay deploy) / standard / open. | Subtask `restricted` chạy trên model của phiên, không chạy trên model subagent rẻ hơn. | `/jev routing` |
| 6 | **Output lớn**: yêu cầu này cần những phần nào của một kết quả tool dài? | Mỗi phần một câu `noul`. Áp dụng cho kết quả từ 16 KB trở lên của lệnh, tìm kiếm, đọc nguyên file hay trang web; kết quả được cắt ở cuối dòng thành các phần khoảng 2.000 ký tự. | Phần mà Jev cho tối đa 10% khả năng cần sẽ bị bỏ, nếu tổng cộng bỏ được từ một phần tư kết quả trở lên. Mỗi đoạn bị bỏ được thay bằng một ghi chú nêu số dòng và cách đọc lại. Phần đầu và phần cuối luôn được giữ, và người dùng vẫn thấy toàn bộ. | `/jev output` |
| 7 | **Báo xong**: câu trả lời có nói rằng một thay đổi chưa được kiểm tra là chạy được không? | Một câu `noul`, chỉ hỏi khi lượt có sửa file và không chạy lệnh nào sau lần sửa cuối (điều kiện này do code kiểm tra). | Model được gửi lại một lần, để chạy bước kiểm tra của dự án trên phần đã đổi, hoặc nói rõ là chưa kiểm tra. | `/jev completion` |
| 8 | **Tool**: tool nào trong danh sách hợp với yêu cầu này? | Khi tool MCP hoặc extension được hoãn (xem bên dưới): một câu `choice` trên cả danh sách để lấy 3 tool tốt nhất, rồi mỗi tool một câu `noul`. | Các tool mà Jev nghiêng về "có" được nêu tên trong input của lượt. Tool nào Jev chắc chắn thì kèm luôn tham số, đỡ một lần gọi `tool_schema`. | `/jev tools` |
| 9 | **Cache**: dùng lại prefix đã cache, hay bắt đầu lại? | Một câu `noul`: yêu cầu mới có nối tiếp việc trước không? Chỉ hỏi khi cache của provider đã hết hạn và hội thoại cũ từ 20k token trở lên, vì lúc đó dùng lại cũng phải trả đủ giá. | Các lượt cũ được cất đi sau một ghi chú, ghi chú này nêu từng yêu cầu và chỉ tới tool history. Lần bắt đầu lại được lưu và áp cho mọi lượt sau, nên prefix mới vẫn được cache. Cache còn ấm thì không bao giờ dựng lại. | `/jev cache` |

Quyết định 3, 6 và 7 cũng áp cho lượt của child, giống như lượt gốc.

Khi plugin bật, cả chín quyết định đều bật, trừ những cái bạn tắt riêng.

Mỗi quyết định có ngưỡng riêng `jev.thresholds.<quyết định>`. TypeSafe khuyên đặt ngưỡng theo
cái giá của một câu trả lời sai.

- Mặc định là `jev.threshold` (0.95).
- Riêng ba quyết định nhẹ là `instructions`, `output` và `completion` thì dùng 0.80, tức một câu
  "không" rõ ràng (p ≤ 0.10) hoặc một câu "có" rõ ràng.
- `/jev status` liệt kê từng quyết định kèm ngưỡng của nó.

Mọi câu hỏi gửi cho Jev đều được ghi vào diagnostics, gồm quyết định nào, phiên bản Jev nào trả
lời, bao nhiêu token input và mất bao lâu. Mọi bản ghi kết quả đều nêu quyết định nào, và có làm
thay đổi hành vi của ha hay không. `/bug-report` có mục **Jev** gồm:

- từng quyết định: hỏi bao nhiêu lần, có tác dụng bao nhiêu lần, lỗi và timeout, thời gian trả
  lời trung vị, các phiên bản đã trả lời;
- các lần agent tự gọi tool `jev`;
- 30 bản ghi Jev mới nhất kèm nội dung đã quyết: lệnh được judge, các mục và dòng bị bỏ, các
  tool được gợi ý.

Phần model calls của bug report còn cho biết:

- tỉ lệ input đọc từ cache;
- bao nhiêu hội thoại được dùng tiếp, bao nhiêu được bắt đầu lại.

### Công bố tool theo tầng

Ba tầng trong ghi chú thiết kế là:

1. mỗi tool một dòng mô tả ngắn;
2. schema đầy đủ, chỉ khi cần;
3. tài liệu hướng dẫn, cho một nhu cầu dùng một lần.

ha áp ba tầng này cho tool của MCP server và extension, vì loại tool này có thể rất nhiều và
schema rất dài. Khi tổng schema của chúng từ 12 KB trở lên, chúng được rút khỏi danh sách tool.
Chế độ đặt bằng `tools.tiered` trong settings.json (`auto` là mặc định, hoặc `on`, `off`) hoặc
bằng `HA_TOOL_TIERS`. Khi đó:

- system prompt liệt kê mỗi tool một dòng (tên và câu đầu của mô tả), đã sắp xếp;
- `tool_schema` cho xem toàn bộ mô tả và tham số của một tool;
- `use_tool` gọi tool đó. Lời gọi trở thành lời gọi của chính tool đó: tham số được kiểm tra theo
  schema của nó, rồi được gate và duyệt như mọi lời gọi khác.

Danh sách tool gửi cho provider không đổi theo những gì model xem, nên prefix đã cache vẫn còn
nguyên. Tool lõi và tool riêng của ha luôn ở trong danh sách.

Cơ chế này chạy được cả khi không có plugin jev. Khi có plugin, quyết định 8 chỉ cho model những
tool phù hợp.

### Quyết định về cache

ha giữ cho prefix mà provider cache được ổn định:

- system prompt chỉ chứa những gì cố định trong suốt phiên;
- ngày giờ và thông tin Git đi kèm input của từng lượt (cả trong `ha exec`);
- danh sách tool được sắp xếp;
- các lần gọi OpenAI dạng streaming đều gửi `prompt_cache_key`.

Việc dựng lại prefix chỉ được cân nhắc khi nó miễn phí, tức là khi cache của provider đã hết hạn
kể từ lần gọi model cuối của hội thoại. Thời gian lưu cache được ước như sau:

| Provider | Thời gian lưu cache |
|---|---|
| Anthropic | 5 phút (1 giờ với `HA_CACHE_RETENTION=long`) |
| DeepSeek | 6 giờ |
| OpenAI API | 1 giờ (1 ngày với `HA_CACHE_RETENTION=long`) |
| OpenAI qua Codex, và các provider khác | 1 giờ |

Khi không chắc, ha ước dài hơn, vì đoán cache đã hết hạn trong khi nó vẫn còn là trường hợp duy
nhất gây thiệt.

Hai biến điều chỉnh:

- `HA_CACHE_LIFETIME`: thời gian lưu cache (tính bằng giây) cho một provider.
- `HA_FRESH_START_MIN_TOKENS`: hội thoại nhỏ hơn mức này luôn được dùng tiếp.

## Cấu hình

`settings.json`, nằm cạnh config của người dùng:

```json
{
  "jev": {
    "enabled": true,
    "threshold": 0.95,
    "model": "jev-latest",
    "decisions": {"commands": true, "subgoals": true, "context": true, "instructions": true, "routing": true, "output": true, "completion": true, "tools": true, "cache": true},
    "thresholds": {"commands": 0.97}
  },
  "tools": {"tiered": "auto"}
}
```

`jev-latest` sẽ chuyển sang phiên bản mới. Theo khuyến nghị của TypeSafe, khi đã chỉnh xong ngưỡng
thì nên cố định phiên bản đã trả lời (`/bug-report` và tool `jev` đều ghi tên phiên bản).
