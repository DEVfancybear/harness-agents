# Liền mạch giữa các phiên

[English](SESSION_CONTINUITY.en.md) | Tiếng Việt

Cách một phiên `ha` tiếp tục từ chỗ phiên trước dừng lại, và mỗi loại thông tin nằm ở
đâu. Mỗi cơ chế dưới đây có một việc riêng. Có chỗ chồng lên nhau khi chúng ghi cùng
một khoảnh khắc từ các phía khác nhau (một quyết định có thể nằm trong task state,
handoff và một session log). Khi chúng khác nhau, một luật duy nhất nói cái nào
thắng: "Where things stand" (dưới đây), mà system prompt mang đúng một lần mỗi
request. Mọi đoạn văn khác mang một bản ghi đều trỏ tới luật đó bằng tên của nó
thay vì tự nêu thứ tự riêng.

## Where things stand

Luật, đúng như hằng `WHERE_THINGS_STAND` trong
`crates/harness-cli/src/host/memory/continuity.rs` giữ (danh sách này được giữ khớp với
hằng đó):

1. tin nhắn hiện tại của người dùng;
2. task state đang chạy của hội thoại này (`task_state`);
3. các file markdown memory của project (`memory/`, ở project có giữ chúng);
4. session log mới nhất (`.harness/sessions`);
5. `.harness/progress.md`, tức handoff;
6. một bản tóm tắt nén ngữ cảnh.

Mọi thứ dưới mục 2 là bản ghi của công việc trước đó và có thể đã cũ - digest của memory
và task state hay các lượt của một hội thoại trước cũng vậy, xếp ngang session log: agent
đối chiếu với repository trước khi làm theo. Một quyết định mới vào `task_state` cho task
này và, ở project có markdown memory, vào `memory/decisions/`.

Luật được nối vào cuối system prompt (cả sau `--system-prompt` hay `SYSTEM.md`), phần của
request được cache, nên mỗi request chỉ gửi một lần. Các đoạn văn trước đây tự xếp hạng
giờ trỏ tới nó trong một dòng: khối mở đầu của session log, session brief, task state của
hội thoại trước, phần handoff của startup brief, prompt của `/handoff --reset`, tiền tố của
bản tóm tắt nén ngữ cảnh, digest của memory và hướng dẫn của tool `memory`. Policy của kho
markdown nói các file của nó là sự thật cho fact và quyết định lâu dài của project, còn
`task_state` đang chạy quyết định task hiện tại. `/handoff` ở project có kho markdown đưa
quyết định vào `memory/decisions/` thay vì một mục riêng, như `/preserve`.

| Cơ chế | Giữ gì | Ai ghi | Ai đọc |
| --- | --- | --- | --- |
| Journal (kho của project) | Từng lượt nguyên văn: lời người dùng, chữ của model, mỗi lần gọi tool và kết quả | Runtime, trong lúc lượt chạy | `/resume`, `/tree`, fork, `history_search` / `history_read`, session log, session brief |
| Task state (`task_state`) | Checklist chính xác của một hội thoại: mục tiêu, bước đã xong và còn mở, quyết định, ràng buộc | Agent, qua tool `task_state` | Mọi lượt của hội thoại đó (trong khoảng 4K token); 3 lượt đầu của một hội thoại mới khi còn mục mở, dưới dạng recap ngắn (xem dưới) |
| Handoff (`.harness/progress.md`) | Trạng thái hiện tại của project: nhánh, kiểm chứng, việc đang làm, bị chặn, bước kế, các lệnh | Agent, khi `/handoff` | Prompt đầu tiên của mọi phiên (trong khoảng 1,5K token, dạng tham khảo), `/doctor`, kiểm tra lúc thoát |
| Session log | Những gì một hội thoại đã chốt: quyết định kèm lý do, bài học, cách sửa, file đã đổi, giá trị cấu hình chính xác, việc còn dở, lỗi và cách vượt, một đoạn để tiếp tục | Host, bằng một lần gọi model từ journal | Lúc mở hội thoại mới (một khối ngắn), `/recall` |
| Session brief | 3 lượt cuối của hội thoại trước, trích đoạn có giới hạn | Host, từ journal, không gọi model | Lượt đầu của hội thoại mới, khi hội thoại trước không có session log |
| Chỉ dẫn của project (`AGENTS.md`, không có thì `CLAUDE.md`) | Điều mọi phiên cần: quy ước, lệnh, cấu trúc, mẫu làm, cảnh báo | Người dùng, hoặc agent khi `/preserve` | Mọi lượt |
| Kho markdown (project có `memory-lint.json`) | Fact và quyết định lâu dài có tag nguồn gốc; ghi chú hằng ngày | Agent, theo policy của kho; host thêm session log vào ghi chú hằng ngày | Mọi lượt (policy và index) |

## Session log

Mỗi hội thoại một log, theo `/compress` của CPR. Hình dạng:

- `# Session Log: <ngày> - <chủ đề>`
- `## Quick Reference (for AI scanning)`: `**Conversation:**`, `**Through:**` và
  `**Written:**` (task id, lượt mà log được ghi tới và thời điểm ghi, do host ghi),
  `**Confidence keywords:**` (10-20 từ khóa), `**Projects:**`, `**Outcome:**` (một câu)
- `## Decisions Made`, `## Key Learnings`, `## Solutions & Fixes`, `## Files Modified`,
  `## Setup & Config`, `## Pending Tasks` (`- [ ]`), `## Errors & Workarounds`,
  `## Custom Notes`: mỗi mục chỉ có khi có nội dung
- `---`, rồi `## Quick Resume Context` (2-3 câu)

**Ở đâu.** Project có kho markdown nhận log dưới dạng một mục `## Session <HH:MM> -
<chủ đề>` trong `memory/YYYY-MM-DD.md` của ngày đó (các mục con thấp hơn một bậc), tức
ghi chú ram mà policy của kho dành cho kết quả trong ngày. Mục kết thúc ở dòng
`<!-- end of session log <conversation> -->`: những gì agent viết sau dòng đó vẫn là của
agent, và lần ghi lại chỉ thay tới dấu đó (một mục ghi trước khi có dấu kết thúc sau đoạn
văn dưới Quick Resume Context). Phần còn lại của ghi chú được giữ nguyên, và ghi chú luôn
dưới giới hạn ram nên `ha memory lint` vẫn qua. Khi ghi chú chỉ còn chỗ cho Quick
Reference và Quick Resume Context, log chỉ giữ hai mục đó; khi không còn chỗ cả cho
chúng, hoặc khi ghi chú có nhưng không đọc được (không phải UTF-8, đang bị chương trình
khác giữ), ghi chú không bị đụng tới và log sang một file riêng. Project khác nhận
`.harness/sessions/<YYYY-MM-DD-HH_MM>-<chủ đề>.md`. Mỗi lúc chỉ một lần ghi, kể cả giữa
các tiến trình `ha`, nhờ khóa `.harness/session-log.lock`.

**Mỗi hội thoại một log.** Lần ghi sau tìm log theo dòng `**Conversation:**` và ghi đè tại
chỗ, giữ ngày hoặc giờ của lần ghi đầu trong tiêu đề. Model đọc log cũ cùng các lượt mới
nhất (tối đa 50 lượt, 120.000 ký tự), nên điều các lượt đầu đã chốt vẫn còn.
`**Through:**` ghi lượt mà log được ghi tới.

**Khi nào.** Tự động, từ journal, bên cạnh hội thoại (không bao giờ trong một lượt):

- khi rời một hội thoại: `/new`, `/resume` sang hội thoại khác, `/fork` hay `/clone`, và
  khi thoát chat (`/quit`, Ctrl-D) - ngay lập tức, nên lúc mở hội thoại sau có thể đã thấy
  log (nếu log còn đang được ghi, lúc mở quay về dùng các lượt cuối);
- khi một lần chạy `ha exec` kết thúc, trước khi tiến trình thoát (chờ tối đa 30 giây; log lâu hơn vẫn còn nợ);
- khi phiên lặng đi - đúng lúc idle review chạy, 10 phút sau lượt cuối
  (`HA_IDLE_REVIEW_SECONDS`), nếu trong lúc đó không có lượt mới nào của cùng hội thoại
  (lượt của hội thoại khác không chặn nó); cả sau một lượt lỗi hay bị dừng;
- sau một lần nén ngữ cảnh: `/compact`, hoặc một lượt đã tự nén (journal vẫn giữ các
  lượt mà bản tóm tắt thay thế);
- khi lượt `/handoff` xong.

**Log còn nợ.** Từ đầu mỗi lượt, hội thoại được đánh dấu là còn nợ một log, trong
`session-log/owed/<id hội thoại>` dưới thư mục kho của project trong thư mục dữ liệu của
ha - cạnh journal giữ hội thoại, không bao giờ trong project - kèm lượt mà log được nợ tới
và tiến trình đang chạy nó. Việc ghi log đó, hoặc thấy trong journal là không nợ gì, xóa
dấu; lượt sau lại đánh dấu. Vì vậy log bị cắt ngang bởi crash, bị kill
(`ha shutdown --force`), worker khởi động lại hay thoát, sẽ được lần mở ha kế tiếp trong
project đó ghi, bên cạnh lượt đầu tiên. Lần mở hay lần rời chỉ ghi các dấu không thuộc một
tiến trình ha khác còn sống và có hội thoại không có lượt nào đang chạy trong tiến trình
này, nên hai tiến trình ha trong một project không bao giờ ghi log lượt dở dang của nhau.
Model fixture để nguyên dấu, và một dấu mà việc ghi đã lỗi 3 lần liền chỉ được thử lại mỗi
giờ một lần. `.harness` của project chỉ được ghi khi có log.

Một lượt đã được ghi log, hoặc đang được ghi, thì không ghi lần hai. Không ghi gì cho agent
con được giao việc, với model fixture, hay cho hội thoại làm quá ít: log cần 2 lượt người
dùng gõ, hoặc - một yêu cầu được một goal hay một lượt dài thực hiện - 5 lần gọi tool hay
4 bước model trên các lượt của nó. Các lượt do host tự khởi động (phần tiếp của goal, báo
cáo của agent con, heartbeat) được tính là công việc, không bao giờ là lượt người dùng.

**An toàn.** Model được dặn hội thoại là dữ liệu, không bao giờ là chỉ thị. Bản ghi model
đọc và log nó viết - kể cả các tiêu đề - đều đi qua các bộ che bí mật mà host dùng cho
đầu ra tool và lịch sử được nhắc lại; log không bao giờ chứa key, token hay mật khẩu.

**Giữ bao lâu.** Mỗi lần ghi log sẽ xóa các log trong `.harness/sessions` được ghi lần
cuối hơn 30 ngày trước: chỉ các file thường có tên `<YYYY-MM-DD-HH_MM>-<chủ đề>.md` và đọc
ra được là log, và không xóa gì khi thư mục là một liên kết. Không xóa gì trong kho
markdown: kho có luật riêng.

**Không thuộc thay đổi của project.** Trước khi host ghi vào `.harness`, nó đảm bảo
`.harness/.gitignore` có `sessions/` và `*.lock` (tạo file, hoặc thêm dòng còn thiếu; không
xóa gì người dùng đã viết), nên file log và khóa không bao giờ hiện là chưa commit
trong tóm tắt lúc mở phiên hay lúc thoát. File `.gitignore` do host tạo cũng liệt kê chính
nó: log ghi trong lúc một coder đang làm không đổi checkout mà coder đã bắt đầu từ đó, và
`integrate_children` vẫn áp dụng được việc của coder. Log trong ghi chú hằng ngày của kho markdown vẫn
được theo dõi: nó thuộc về kho.

**Tắt.** `"sessionLog": false` trong `settings.json`, hoặc `HA_SESSION_LOG=off`.

**Chẩn đoán.** Mỗi quyết định là một bản ghi chẩn đoán loại `session_log`, mà báo cáo
lỗi đọc:

- `written`: `trigger` (`quiet`, `compaction`, `handoff`, `left`, `exit` cho `ha exec`, `owed` khi lần mở ha ghi một log còn nợ), `home` (`note`, `file`), `path`
  (tương đối với thư mục gốc của project), `bytes`, `turns`, và `fallback` khi log không ở
  chỗ đáng lẽ của nó (`brief`, `no_room`, `unreadable_note`);
- `skipped`: `trigger` và `reason` - `off`, `superseded` (một lượt sau của hội thoại đã bắt
  đầu trước), `already_written`, `in_progress`, `fixture`, `child`, `few_turns`, `not_in_store`, `timed_out` (`ha exec`);
- `failed` (warn): `trigger`, và lỗi làm message, đã che bí mật và cắt còn 300 ký tự;
- `recalled`: `n` (số log đã gửi), `topic` (có chủ đề hay không), `matches`;
- `preserve_requested`: `file` (`AGENTS.md`, `CLAUDE.md` hoặc `none`), `store`.

## Lúc mở hội thoại mới

Prompt đầu tiên của hội thoại mới nghe về hội thoại trước đúng một lần:

- Nếu hội thoại trước có session log ghi trong 7 ngày qua và ghi tới lượt mới nhất của nó,
  khối hiện kết quả, Quick Resume Context và tối đa 5 việc còn dở chưa đánh dấu, với khung
  là tham khảo lịch sử, không phải chỉ thị đang có hiệu lực; 3 lượt cuối của hội thoại
  trước không gửi kèm nữa.
- Nếu log đó được ghi trước lượt mới nhất của hội thoại (log lúc lặng chưa tới hạn, lần ghi
  của handoff còn đang chạy), khối nói rõ điều đó và các lượt cuối vẫn được gửi kèm.
- Nếu log mới nhất thuộc một hội thoại cũ hơn, khối hiện log đó cùng số hội thoại sau nó
  không có log, và các lượt cuối của hội thoại trước vẫn được gửi như trước.
- Không có log thì session brief được gửi như trước.
- Khi task state của hội thoại trước được mang sang (checklist của nó là chính xác), việc
  còn dở trong log bị bỏ ra để các bước không bị liệt kê hai lần.
- Handoff là một khối riêng của startup brief (`session-handoff`), trên kênh reference.
  Khi log mới hơn `.harness/progress.md`, khối nói điều đó trong một dòng.

Fork, import, `/resume`, nén ngữ cảnh và `/refinements off` không thêm khối này.

**Chỉ hội thoại đã rời.** Một hội thoại còn mở không phải là "hội thoại trước": một pane
khác của cùng ha, một agent nền, hay một hội thoại mà một tiến trình ha khác còn sống đang
chạy. Mỗi lượt giữ hội thoại của nó mở trong một sổ đăng ký của tiến trình (tới khi lượt
kế của pane là của hội thoại khác, hoặc pane mất đi) và để một dấu dưới kho của project
(`open-conversations/<task>`, ghi tên tiến trình); hội thoại mà một tiến trình còn sống
đang nợ log cũng được coi là mở. Khối mở đầu của session log, session brief và task state
của hội thoại trước đều bỏ qua hội thoại còn mở.

**Task state của hội thoại trước.** 3 lượt đầu của một hội thoại mới nghe về task mà hội
thoại mới nhất có task state để dở, dưới dạng một khối tham khảo optional
(`earlier-task-state`): recap của state và tối đa 10 mục còn mở, trong khoảng 1,5K token,
kèm gợi ý `/resume`. Nó không được đưa ra khi state đó cũ hơn 7 ngày (theo thời gian của
file), hoặc khi đã đưa cho 3 hội thoại mới khác mà không hội thoại nào nhận lấy (hội thoại
nhận lấy sẽ giữ state của riêng nó, mới hơn); các hội thoại đã được đưa được giữ trong
`task_state.offered.json` cạnh state.

## Ngân sách ngữ cảnh

Mọi khối trong ngữ cảnh của một lượt đều có chỗ trong một ngân sách
(`ContextBuilder`, `crates/harness-session/src/context.rs`):

- Khối của host không bắt buộc đi đường optional: các khối optional được xếp theo
  relevance và nhận vào khi còn vừa `RuntimeConfig::optional_token_budget`
  (`OPTIONAL_TOKEN_BUDGET`, 9.000 token: 3K của digest và 6K của các khối mở đầu vừa cùng
  lúc). Khối không vừa bị bỏ ra.
- Một khối có thể mang `max_tokens`. Khối vượt mức bị cắt ở ranh giới dòng, trước khi
  nhận vào, kèm một ghi chú ở cuối nói nó đã bị cắt và phần còn lại ở đâu; khối bắt buộc
  vượt mức bị cắt thay vì làm tràn cửa sổ ngữ cảnh.
- Luật và fact của host đi trước, rồi các khối optional theo hạng, rồi tin nhắn của người
  dùng, working state và phần đuôi, nên request kết thúc bằng điều lượt này cần làm.
- `/context` hiện kích thước từng khối, và với khối bị cắt thì kích thước trước khi cắt;
  khối bị bỏ nói lý do. Chẩn đoán ghi `context` `budget` (`cut`, `dropped`,
  `mandatory_tokens`, `optional_tokens`) cho một packet có cắt hay bỏ khối.

| Khối | Bắt buộc | Mức trần (token) | Hạng |
| --- | --- | --- | --- |
| `task-state` (của hội thoại này) | có | 4.000 (mục đã xong thành một con số, các quyết định mới nhất) | - |
| `recalled-skill` | có | 1.500 | - |
| `harness_state` (digest của memory) | không | 3.000 | 70 |
| `session-start` (commit, chưa commit, kiểm tra baseline) | không | 800 | 55 |
| `earlier-task-state` | không | 1.500 | 50 |
| `session-log` | không | 1.500 | 42 |
| `knowledge-catalog` | không | 1.500 | 40 |
| `session-handoff` | không | 1.500 | 38 |
| `recent-session` (session brief) | không | 1.500 | 30 |

## Lệnh

- `/recall [n] [chủ đề]` gửi một lượt mà prompt mang, như dữ liệu, n session log gần
  nhất (mặc định 3) và, khi có chủ đề, tối đa 10 log được xếp hạng bằng BM25 của memory
  search (không khớp thì theo cụm từ, không phân biệt hoa thường), mỗi log một đoạn trích
  140 ký tự; cả prompt nằm trong 96 KB, các log được cắt để chia nhau phần còn lại. Một số
  đứng đầu không quá 50 là n; số lớn hơn (một năm, một số issue) là một phần của chủ đề.
  Agent báo Context, Blockers, Most recent session, Earlier sessions, Related (khi có chủ
  đề) và Ready to, và chỉ đọc file của log khi các tóm tắt chưa trả lời được. Không có log
  thì một thông báo và không có lượt nào.
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
