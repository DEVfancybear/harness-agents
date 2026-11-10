# Đồng đội, quy ước và skill đã kiểm chứng

[English](TEAMWORK.en.md) | Tiếng Việt

Phần ha lấy từ bốn bài báo đọc ngày 2026-11-10: nó hoạt động thế nào, và phần
nào không lấy.

- **[S] arXiv 2611.08615**, *s3: Simple Scaling of Swarms*: các agent làm việc
  cùng lúc, mỗi agent trên bản sao task của riêng nó, có tool nhắn tin và
  không có vai trò. Được huấn luyện với một phần thưởng chung, chúng lặp lại
  việc của nhau ít đi gần một nửa và thắng cùng số agent làm riêng lẻ. Phần ha
  dùng được mà không cần huấn luyện là harness và prompt chung.
- **[Z] arXiv 2611.06409**, *Zero-Shot Coordination among LLM Agents*: các
  agent không thống nhất trước được thì mỗi bên coi quy ước của mình là quy
  ước tự nhiên, rồi cùng hỏng; model mạnh hơn hay suy luận nhiều hơn không
  sửa được. Một *định nghĩa thân thiện với phối hợp* (CFD) ngắn trong prompt
  thì sửa được.
- **[E] arXiv 2611.07595**, *Evolver*: chiến lược chưng cất từ các lần chạy
  trước (Gene), mỗi cái kèm một ca cụ thể (Capsule), bản nháp được đánh dấu
  chưa kiểm chứng, cái hay hỏng bị hạn chế. Dùng lại làm tỉ lệ thành công tăng
  4,4 điểm và giảm 24% token trên tập bài báo chọn; bài báo cũng cảnh báo một
  chiến lược lỗi thời tốn hơn là không có.
- **[T] arXiv 2610.27307**, *TURTLE*: một proposer tiến hoá các task benchmark
  tới khi solver làm được khoảng một nửa số lần. Chưa lấy (xem dưới).

## Đồng đội ([S])

Code: `crates/harness-cli/src/host/coordination/delegation.rs`
(`TEAM_PROTOCOL`, `brief_on_team`, `announce`, `no_sibling`).

Trước đây một child gửi được `agent_message` cho sibling "theo tên" nhưng
không bao giờ được cho biết tên sibling, việc nó được giao, hay là nó có tồn
tại.

| [S] | ha | Vì sao |
|---|---|---|
| Mỗi agent được cho tên các đồng đội | Prompt đầu của child kết thúc bằng `Teammates:`: các child khác của cha nó còn đang làm, mỗi cái dạng `- tên (role): 240 ký tự đầu của brief` | Nó gọi được đúng tên, và thấy chỗ các việc chồng nhau |
| Đội cố định từ đầu | Child được tạo sau được báo cho các child đã nhận brief, bằng tin `[teammate-joined: tên]` ở bước kế của chúng | Gốc tạo child bất cứ lúc nào |
| Prompt chung: không vai trò, không bắt buộc nhắn, cái gì đáng gửi, tách đã kiểm chứng với đoán, không hỏi thăm | `TEAM_PROTOCOL`, trong chỉ dẫn của explorer và coder | Prompt của bài báo, cắt còn phần đúng với ha |
| `message(to, text)` tới một đồng đội hoặc tất cả; thông báo đánh thức agent đang rảnh | `agent_message` (`sibling` kèm tên, hoặc `all`), đọc ở bước kế của bên nhận; child đã xong được đánh thức | Đã có sẵn (tầm với của prime-agent) |
| Sai người nhận | Lời từ chối nêu tên các sibling đang có | Đoán sai được sửa trong một bước |

- **Chỉ báo một lần.** Child được đánh dấu đã nhận brief dưới cùng cái khoá
  mà danh sách đồng đội được đọc. Đứa đăng ký trước lần đọc đó thì nằm trong
  danh sách; đứa đăng ký sau thì tự báo. Không bao giờ cả hai, không bao giờ
  thiếu.
- **Ai thuộc đội.** Explorer và coder. Verifier phán một mình: không được báo
  về ai và không ai được báo về nó. Child mà host tự tạo cho mình (verifier
  của goal, strategist ở plateau) cũng vậy.
- **Child đã xong được để yên.** Đồng đội đã xong không nằm trong danh sách và
  không bị tin báo gia nhập đánh thức. Tin nhắn gửi cho nó thì vẫn đánh thức.
- **Không thêm lượt gọi model.** Danh sách là chữ trong prompt mà child đằng
  nào cũng nhận; tin báo gia nhập đi cùng bước kế của child.

## Quy ước ([Z])

Agent trong bài báo không nói chuyện được, nên CFD bảo chúng lùi về cách
không cần quy ước. Child của ha nói được, nên nói ra lựa chọn đứng trước và
cách lùi đứng sau. Trong `TEAM_PROTOCOL`:

> Where your work has to fit a teammate's or your parent's - a name, a format,
> an interface, who does which part - do not build on a choice they would have
> to guess. Take it from the brief or from what the project already does; when
> neither settles it, tell them your choice or ask before you build on it, or
> do what holds whichever way they chose.

Hướng dẫn delegate của gốc (`SUBAGENT_GUIDANCE` trong
`crates/harness-cli/src/host/prompting/prompt.rs`) yêu cầu nửa còn lại: thứ
các child phải thống nhất - tên, định dạng, interface, file nào của ai - được
ghi vào brief của từng đứa.

## Skill đã kiểm chứng ([E])

Code: `crates/harness-cli/src/host/memory/learned.rs` (`Usage::unproven`,
`Usage::restricted`, `record_outcome`) và `skill_recall.rs`.

Learned skill của ha vốn đã gần là một Gene: mô tả, các bước, ledger, số lần
dùng, kết quả do review phán, các giả định đã sai. Phần còn thiếu:

| [E] | ha | Vì sao |
|---|---|---|
| Capsule: một ca cụ thể bên cạnh chiến lược | Review phán skill `worked` thì giữ câu trích của nó làm một ca (3 ca mới nhất, mỗi ca một lần). Skill được recall mang ca mới nhất: `A turn it worked in showed: …` | Các bước nói phải làm gì; ca cho thấy lúc xong trông thế nào |
| Bản nháp được đánh dấu chưa kiểm chứng | Skill chưa có check nào qua sau khi dùng và chưa được review phán là chạy được thì được recall kèm `Nothing has shown it to work yet: it is a draft. Check each step's result as you go.` | Skill của ha có hiệu lực ngay khi viết; turn cần biết cái nào chưa từng được thấy là chạy |
| Hạn chế theo thất bại | Kể từ lần viết gần nhất, skill hỏng từ hai lần trở lên và hỏng nhiều hơn chạy thì không được xét recall. Nó vẫn ở trong thư viện, nơi curator được bảo viết lại | Skill khớp theo chữ mà hỏng theo hồ sơ thì mỗi lần tốn một turn |
| Thử thách: vị thế theo kết quả ghi nhận | Bộ đếm bắt đầu lại ở mỗi lần patch (`trial_good`, `trial_bad` trong `skill-usage/*.json`), nên skill đã viết lại được thử lại từ đầu | Không có nó thì một tuần xấu khoá skill mãi mãi |

Check qua hay hỏng sau khi skill được dùng cũng tính vào lần thử. Record chẩn
đoán `skill recalled` có thêm `cases` và `unproven`.

## Phần không lấy

- **Phần huấn luyện của [S]** (phần thưởng đội, advantage theo nhóm): ha không
  train model. Mức cải thiện bài báo đo được là của policy đã huấn luyện;
  riêng prompt là thứ [S] gọi là điểm xuất phát.
- **Thông báo số tin chưa đọc và `read_messages` của [S]:** ha đưa thẳng tin
  nhắn vào bước kế của bên nhận.
- **Chạy đua tới thành công đầu tiên của [S]:** không có gì dừng các child
  khác khi một đứa xong, vì ngoài check ra không có gì nói một child đã thành
  công.
- **CFD meta-learn và policy critic của [Z]:** cần các trò chơi phối hợp để
  chấm điểm một định nghĩa. ha không có.
- **Mạng chia sẻ tài sản (EvoMap), lưu trữ theo nội dung và cách ly trước lần
  dùng đầu của [E]:** skill vẫn có hiệu lực ngay khi viết, và
  `/refine --rollback` hoàn tác nó.
- **Tín hiệu khớp và kế hoạch kiểm chứng dạng trường riêng của [E]:** recall
  chấm trên chính chữ của skill (BM25).
- **Recall trong `ha exec` và cho child:** child được gửi cả thư viện đã học,
  còn `ha exec` thì không có gì ([đổi chiến lược và
  runtime](STRATEGY_AND_RUNTIME.vi.md) mô tả chính phần recall).
- **[T], toàn bộ, lúc này:** benchmark tự chỉnh là một lệnh riêng (`ha exec`
  làm solver, check của project làm người chấm, task được giữ quanh mức hoàn
  thành 50%). Mới đề xuất, chưa làm.

## Test

- Unit: `a_child_is_told_its_teammates_and_the_one_that_joins_later`,
  `a_sibling_that_does_not_exist_is_answered_with_the_ones_that_do` và
  `a_teammate_line_is_one_line_and_cut` trong `delegation.rs`;
  `a_skill_is_unproven_until_it_works_and_restricted_once_it_keeps_failing`
  trong `learned.rs`;
  `a_skill_that_keeps_failing_is_not_recalled_until_it_is_rewritten` và
  `the_block_says_a_skill_is_unproven_and_carries_the_case_it_worked_in` trong
  `skill_recall.rs`.
- Live: `scripts/testing/live-tests/team.sh` (hai explorer chạy cùng lúc biết
  về nhau và trao đổi thứ tìm được theo tên; verifier không thuộc đội nào) và
  mục 4b của `strategy.sh` (chưa kiểm chứng, ca đã chạy, bị giữ lại).
