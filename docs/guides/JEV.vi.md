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
kiểu gửi cho Jev. ha hỏi Jev ở năm điểm sau.

Hai nguyên tắc chung cho cả năm điểm:

- Mỗi quyết định **chỉ siết chặt hơn**. Jev không bao giờ cho phép điều mà policy không cho, và
  không bao giờ bỏ việc người dùng đã yêu cầu.
- Mỗi quyết định **an toàn khi lỗi** (fail open). Nếu Jev không trả lời, trả lời chậm hoặc
  không chắc, ha làm đúng như khi không có plugin.

| # | Điểm quyết định | Câu hỏi cho Jev | ha làm gì khi Jev chắc chắn | Công tắc |
|---|---|---|---|---|
| 1 | **Quyền chạy lệnh**: lệnh này có nên chạy không? | `choice` allow / ask / deny. State gồm lệnh và nội dung các script cục bộ mà lệnh chạy. | `deny` thì từ chối lệnh và đưa lý do cho model, ở mọi chế độ: kể cả full-auto, headless và child. `ask` và `allow` không đổi gì. | `/jev commands` |
| 2 | **Subgoal**: việc này đã xong hoặc đang chạy chưa? | Mỗi child cùng cấp một câu `noul`: có phải cùng một việc không? | Dùng lại child đó (câu trả lời của nó, hoặc lần chạy đang diễn ra) thay vì tạo thêm một child trùng. | `/jev subgoals` |
| 3 | **Ngữ cảnh**: mỗi kết quả tool cũ nên hiện đến đâu với yêu cầu này? | `choice` hide / short / full, hỏi khi kết quả của lượt vượt ngân sách. | Cắt theo thứ tự: `hide` trước, rồi `short` (giữ phần đầu), rồi phần còn lại từ cũ nhất. Kết quả `full` bị cắt sau cùng. | `/jev context` |
| 4 | **Chỉ dẫn theo điều kiện**: mục này của `AGENTS.md` có áp dụng cho yêu cầu này không? | Mỗi mục một câu `noul`, chỉ với file chỉ dẫn có nhiều mục. | Mục mà Jev cho tối đa 10% khả năng áp dụng sẽ bị bỏ khỏi lượt, và lượt ghi tên các mục đã bỏ để model đọc file nếu cần. Phần mở đầu luôn được giữ. | `/jev instructions` |
| 5 | **Định tuyến theo độ nhạy**: subtask này chạm dữ liệu nhạy cảm cỡ nào? | `choice` restricted (secrets, credentials, env, cấu hình hạ tầng hay deploy) / standard / open. | Subtask `restricted` chạy trên model của phiên, không chạy trên model subagent rẻ hơn. | `/jev routing` |

Khi plugin bật, cả năm quyết định đều bật, trừ những cái bạn tắt riêng. Ngưỡng là
`jev.threshold` (mặc định 0.95). Mọi quyết định đều được ghi vào diagnostics (bản ghi `jev`), nên
`/bug-report` cho thấy Jev đã quyết định những gì.

Hai điểm trong ghi chú chưa có trong bản này:

- **Dùng lại cache**: chọn giữa dựng lại và dùng lại prefix cache phụ thuộc cách cache của từng
  provider.
- **Công bố tool theo tầng** (mô tả ngắn trước, schema khi cần): phải đổi cách khai báo tool.

## Cấu hình

`settings.json`, nằm cạnh config của người dùng:

```json
{
  "jev": {
    "enabled": true,
    "threshold": 0.95,
    "model": "jev-latest",
    "decisions": {"commands": true, "subgoals": true, "context": true, "instructions": true, "routing": true}
  }
}
```
