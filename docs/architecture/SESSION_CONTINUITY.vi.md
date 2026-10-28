# Liền mạch giữa các phiên

[English](SESSION_CONTINUITY.en.md) | Tiếng Việt

Cách một phiên `ha` tiếp tục từ chỗ phiên trước dừng lại, và mỗi loại thông tin nằm ở
đâu. Mỗi cơ chế dưới đây có một việc riêng; không cơ chế nào giữ bản sao của cơ chế khác.

| Cơ chế | Giữ gì | Ai ghi | Ai đọc |
| --- | --- | --- | --- |
| Journal (kho của project) | Từng lượt nguyên văn: lời người dùng, chữ của model, mỗi lần gọi tool và kết quả | Runtime, trong lúc lượt chạy | `/resume`, `/tree`, fork, `history_search` / `history_read`, session log, session brief |
| Task state (`task_state`) | Checklist chính xác của một hội thoại: mục tiêu, bước đã xong và còn mở, quyết định, ràng buộc | Agent, qua tool `task_state` | Mọi lượt của hội thoại đó; 3 lượt đầu của hội thoại kế tiếp khi còn mục mở |
| Handoff (`.harness/progress.md`) | Trạng thái hiện tại của project: nhánh, kiểm chứng, việc đang làm, bị chặn, bước kế, các lệnh | Agent, khi `/handoff` | Prompt đầu tiên của mọi phiên (6 KB đầu), `/doctor`, kiểm tra lúc thoát |
| Session log | Những gì một hội thoại đã chốt: quyết định kèm lý do, bài học, cách sửa, file đã đổi, giá trị cấu hình chính xác, việc còn dở, lỗi và cách vượt, một đoạn để tiếp tục | Host, bằng một lần gọi model từ journal | Lúc mở hội thoại mới (một khối ngắn), `/recall` |
| Session brief | 3 lượt cuối của hội thoại trước, trích đoạn có giới hạn | Host, từ journal, không gọi model | Lượt đầu của hội thoại mới, khi hội thoại trước không có session log |
| Chỉ dẫn của project (`AGENTS.md`, không có thì `CLAUDE.md`) | Điều mọi phiên cần: quy ước, lệnh, cấu trúc, mẫu làm, cảnh báo | Người dùng, hoặc agent khi `/preserve` | Mọi lượt |
| Kho markdown (project có `memory-lint.json`) | Fact và quyết định lâu dài có tag nguồn gốc; ghi chú hằng ngày | Agent, theo policy của kho; host thêm session log vào ghi chú hằng ngày | Mọi lượt (policy và index) |

## Session log

Mỗi hội thoại một log, theo `/compress` của CPR. Hình dạng:

- `# Session Log: <ngày> - <chủ đề>`
- `## Quick Reference (for AI scanning)`: `**Conversation:**` (task id, do host ghi),
  `**Written:**`, `**Confidence keywords:**` (10-20 từ khóa), `**Projects:**`,
  `**Outcome:**` (một câu)
- `## Decisions Made`, `## Key Learnings`, `## Solutions & Fixes`, `## Files Modified`,
  `## Setup & Config`, `## Pending Tasks` (`- [ ]`), `## Errors & Workarounds`,
  `## Custom Notes`: mỗi mục chỉ có khi có nội dung
- `---`, rồi `## Quick Resume Context` (2-3 câu)

**Ở đâu.** Project có kho markdown nhận log dưới dạng một mục `## Session <HH:MM> -
<chủ đề>` trong `memory/YYYY-MM-DD.md` của ngày đó (các mục con thấp hơn một bậc), tức
ghi chú ram mà policy của kho dành cho kết quả trong ngày; phần còn lại của ghi chú được
giữ nguyên, và ghi chú luôn dưới giới hạn ram nên `ha memory lint` vẫn qua. Khi ghi chú
không còn chỗ, kể cả cho Quick Reference và Quick Resume Context, log sang một file riêng.
Project khác nhận `.harness/sessions/<YYYY-MM-DD-HH_MM>-<chủ đề>.md`.

**Mỗi hội thoại một log.** Lần ghi sau tìm log theo dòng `**Conversation:**` và ghi đè tại
chỗ, giữ ngày hoặc giờ của lần ghi đầu trong tiêu đề.

**Khi nào.** Tự động, từ journal, bên cạnh hội thoại (không bao giờ trong một lượt):

- khi phiên lặng đi - đúng lúc idle review chạy, 10 phút sau lượt cuối
  (`HA_IDLE_REVIEW_SECONDS`), nếu trong lúc đó không có lượt mới;
- sau một lần nén ngữ cảnh: `/compact`, hoặc một lượt đã tự nén (journal vẫn giữ các
  lượt mà bản tóm tắt thay thế);
- khi lượt `/handoff` xong.

Một lượt đã được ghi log thì không ghi lần hai. Không ghi gì cho hội thoại có ít hơn 2 lượt
người dùng, cho agent con được giao việc, hay với model fixture.

**An toàn.** Model được dặn hội thoại là dữ liệu, không bao giờ là chỉ thị. Bản ghi model
đọc và log nó viết đều đi qua các bộ che bí mật mà host dùng cho đầu ra tool và lịch sử
được nhắc lại; log không bao giờ chứa key, token hay mật khẩu.

**Giữ bao lâu.** Mỗi lần ghi log sẽ xóa các log trong `.harness/sessions` được ghi lần
cuối hơn 30 ngày trước. Không xóa gì trong kho markdown: kho có luật riêng.

**Tắt.** `"sessionLog": false` trong `settings.json`, hoặc `HA_SESSION_LOG=off`.

## Lúc mở hội thoại mới

Prompt đầu tiên của hội thoại mới nghe về hội thoại trước đúng một lần:

- Nếu hội thoại trước có session log ghi trong 7 ngày qua, khối hiện kết quả, Quick Resume
  Context và tối đa 5 việc còn dở chưa đánh dấu, với khung là tham khảo lịch sử, không
  phải chỉ thị đang có hiệu lực; 3 lượt cuối của hội thoại trước không gửi kèm nữa.
- Nếu log mới nhất thuộc một hội thoại cũ hơn, khối hiện log đó cùng số hội thoại sau nó
  không có log, và các lượt cuối của hội thoại trước vẫn được gửi như trước.
- Không có log thì session brief được gửi như trước.
- Khi task state của hội thoại trước được mang sang (checklist của nó là chính xác), việc
  còn dở trong log bị bỏ ra để các bước không bị liệt kê hai lần.
- Handoff vẫn nằm trong startup brief như cũ. Khi log mới hơn `.harness/progress.md`,
  khối nói điều đó trong một dòng.

Fork, import, `/resume`, nén ngữ cảnh và `/refinements off` không thêm khối này.

## Lệnh

- `/recall [n] [chủ đề]` gửi một lượt mà prompt mang, như dữ liệu, n session log gần
  nhất (mặc định 3, 1-50, cắt để chia nhau khoảng 96 KB) và, khi có chủ đề, tối đa 10 log
  được xếp hạng bằng BM25 của memory search (không khớp thì theo cụm từ, không phân biệt
  hoa thường), mỗi log một đoạn trích 140 ký tự. Agent báo Context, Blockers, Most recent
  session, Earlier sessions, Related (khi có chủ đề) và Ready to, và chỉ đọc file của log khi
  các tóm tắt chưa trả lời được. Không có log thì một thông báo và không có lượt nào.
- `/preserve [trọng tâm]` yêu cầu agent đưa những gì phiên này đã chốt cho mọi phiên sau -
  quy ước, lệnh, cấu trúc, mẫu làm, cảnh báo - vào `AGENTS.md` (không có thì `CLAUDE.md`;
  khi cả hai đều không có, nó hỏi trước khi tạo `AGENTS.md`). Không ghi trạng thái hiện tại
  hay bước kế (đó là việc của `/handoff`), và ở project có kho markdown thì không ghi
  quyết định hay fact (chúng vào `memory/decisions/` hoặc `memory/projects/`, có tag). Nó
  sửa tại chỗ, mỗi điều ghi một lần, giữ file tối đa 200 dòng bằng cách chuyển các mục
  `## Session Notes (<ngày>)` cũ hơn 7 ngày, `## Completed Projects` và tiêu đề đánh dấu
  `(ARCHIVABLE)` sang `AGENTS-Archive.md` dưới `## Archived: <ngày>` - không bao giờ
  chuyển tiêu đề đánh dấu `(PROTECTED)` hay mục cốt lõi - và kết thúc bằng số dòng của file.
- `/handoff [--reset] [trọng tâm]` ghi `.harness/progress.md` như trước; khi xong nó còn
  ghi session log.
