# Vòng học (learning loop)

[English](LEARNING_LOOP.en.md) | Tiếng Việt

Những gì người dùng đã sửa và những gì agent đã giả định sai được gom lại qua
nhiều hội thoại. Bài học nào lặp lại thì tự trở thành một rule thường trực. Đây
là learning loop của [Agnostic-AI](https://github.com/ucsandman/Agnostic-AI)
(harvest, distill, promote; đọc ở commit `aab09ff`), được gắn vào lượt review tự
động của ha.

Code: `crates/harness-cli/src/host/memory/lessons.rs`, được review trong
`memory/refine.rs` gọi tới; CLI ở `commands/learning_cli.rs`.

## 1. Khác gì so với Agnostic-AI

| Agnostic-AI | ha | Vì sao |
|---|---|---|
| `correction-tracker` phân loại prompt bằng regex tiếng Anh; `errorlog` phân loại khối deviation theo cùng cách | Lượt review tự động đọc hội thoại và báo từng lời sửa (`kind: correction`) hay giả định sai (`kind: deviation`), kèm trích dẫn nguyên văn | ha đánh giá văn bản theo nghĩa, không theo danh sách từ, và một lời sửa bằng tiếng Việt vẫn là lời sửa |
| Một candidate là hash của các từ đã chuẩn hoá | Review chỉ ra lesson đang mở mà tín hiệu lặp lại hoặc phản bác | Hai cách diễn đạt của một bài học, hay hai ngôn ngữ, vẫn là một bài học |
| Thành rule khi thấy ở 3+ ngày; phát hiện mâu thuẫn bằng từ khoá | Ngưỡng của markdown-agent-memory: trọng số 3 từ 2+ hội thoại, tín hiệu quá 30 ngày tính nửa; tín hiệu phản bác do review chỉ ra | Đúng ngưỡng mà policy memory đã nêu |
| File proposal; người duyệt trên dashboard | Tự promote, trong các giới hạn; `ha learning retire` gỡ lại | Lựa chọn của maintainer |
| Rule đã duyệt được nối vào `global-rules.md` rồi port đi | Lesson global thành entry `constraints` trong memory global; lesson của một project vào kho markdown của project đó, hoặc entry của project | Đúng chỗ ha vốn đặt những gì mọi lượt phải thấy |
| Job ban đêm trên Windows Task Scheduler | Ngưỡng chạy sau mỗi lần review ghi được tín hiệu, và khi gọi `ha learning distill` | Không cần cài gì; review vốn đã chạy |
| Bậc 1 của thang: dữ kiện của repo, từ một lần thấy | Deviation về một project là fact, được ghi ngay (§5) | Đó là điều quan sát được, không phải suy ra; ngưỡng của policy chỉ dành cho suy luận |
| "Rule có tác dụng khi bucket của nó ngừng tăng" | Đếm tín hiệu sau khi promote; rule bị sửa lại nhiều như lúc nó cần để thành rule thì bị gắn nhãn còn bị sửa (§5) | Cùng thước đo, áp cho từng rule |
| Meditation hằng đêm trên error log, tổng hợp hằng tuần | Meditation mỗi ngày một lần sau review: gộp, đổi câu chữ, retire, tổng hợp (§5) | Review vốn đã chạy; mỗi ngày một lượt |
| `errorlog.html`: những gì sai lặp lại | `ha learning report [--html FILE]` (§6) | |

Không lấy: file few-shot example (không có gì đọc chúng), giới hạn năm trait và
bậc Trait (ha không có file trait), luật đẩy rule dài quá 180 ký tự sang example
(thay bằng việc cắt rule ở 400 ký tự), error log phía con người, và việc thu từ
thư mục của agent khác.

## 2. Tín hiệu

Prompt của lượt review tự động có phần `<lessons>`: cần báo gì, và tối đa 30
lesson đang mở của mọi project và của project này, mới nhất trước, mỗi lesson
kèm trạng thái và độ vững. Review trả về mảng `lessons`:

```json
{"lesson": "les_…, hoặc bỏ trống với lesson mới",
 "text": "khi <tình huống>, <điều hoá ra đúng>",
 "kind": "correction | deviation | resolution | preference",
 "domain": "code-style | testing | git | debugging | workflow | file-patterns | other",
 "scope": "global | project",
 "against": false,
 "evidence": "trích dẫn nguyên văn từ hội thoại"}
```

Host kiểm tra từng tín hiệu:

- Evidence phải xuất hiện trong các lượt chưa được review (bỏ qua khoảng trắng
  và hoa thường) và phải là một cụm từ (từ 12 ký tự trở lên): một hai chữ thì ở
  đâu cũng có.
- Nó phải do đúng người nói theo kind của nó: lời sửa hay sở thích thì là lời
  của người dùng; giả định sai hay cách sửa lỗi thì là tool result hoặc lời của
  người dùng. Lời của chính agent không bao giờ là bằng chứng, và những gì nó đọc
  lại từ memory của mình (kết quả `memory`, `knowledge`, `history_*`) cũng vậy.
- Không có secret, trong text hay trong trích dẫn.
- Một hội thoại góp tối đa 2 vào weight của một lesson, nên ngưỡng 3 luôn cần
  một hội thoại thứ hai.
- Tín hiệu chỉ tới một lesson thì lesson đó phải tồn tại, chưa bị retire, và
  phải thuộc project này nếu là lesson của project.
- Lesson mới cần có text, kind, và project khi scope là `project`. Text là dữ
  liệu, không bao giờ là chỉ dẫn: "khi X thì Y là đúng".
- `against: true` phải chỉ ra lesson mà nó phản bác.

Mỗi tín hiệu được ghi cùng refinement (`appliedEdits`, kind `lesson`), dù được
áp dụng hay bị từ chối (kèm lý do). Rollback refinement không xoá tín hiệu, vì đó
là điều các hội thoại đã cho thấy.

Lượt gate của review được dặn rằng một lời sửa hay một giả định sai là đáng
review, nên hội thoại chỉ có mỗi bài học đó vẫn tới được bước refine.

### Chỉ ghi một lần: memory của chính review

Review vốn đã lưu ngay một quy tắc người dùng nói rõ ("từ nay ...") thành entry
memory global, đúng như policy memory yêu cầu. Learning loop không được ghi lại
quy tắc đó lần thứ hai:

- Review báo lesson đó kèm `saved_as`, là id của edit memory global mà nó tạo
  cho lesson. Chỉ id nằm trong các edit memory global của chính lần review đó
  mới được tin (nếu không, record ghi `saved_as_ignored`). Lesson được promote
  ngay dưới dạng entry đó (placement `covered`). Tín hiệu về sau chỉ cộng thêm
  bằng chứng, không ghi gì mới.
- Nếu rốt cuộc entry không được ghi (phần local hoặc global lỗi, hoặc memory từ
  chối edit), lesson quay lại thành candidate.
- Trước khi ngưỡng ghi một rule, nó hỏi memory xem đã có entry nào nói điều đó
  chưa, bằng đúng thước đo memory tool dùng để từ chối trùng (giống 60 %, hoặc
  85 % nằm sẵn trong một entry). Nếu có, lesson được promote dưới dạng entry đó
  thay vì ghi thêm.
- Ngưỡng chạy sau khi phần global của review đã được ghi, nên rule mà review
  vừa lưu trong cùng lượt sẽ được tìm thấy.

Khi project có kho markdown, review còn được dặn không lưu dữ kiện của project
đó thành edit memory: chúng thuộc về kho, do agent ghi. Lesson về project vẫn
nằm trong `lessons`.

Prompt dặn review bỏ qua lần thất bại của một learned skill, vì skill outcome
của nó đã ghi lại.

## 3. Ngưỡng

Độ vững của một lesson tại một thời điểm:

- **weight**: mỗi tín hiệu ủng hộ độc lập được 1, tín hiệu quá 30 ngày được 0,5.
  Hai lượt review đọc cùng một trích dẫn trong cùng hội thoại chỉ tính một.
- **conversations**: số hội thoại có tín hiệu ủng hộ.
- **contrary**: số hội thoại phản bác.

| Điều kiện | Kết quả |
|---|---|
| contrary ≥ 2 | Retire; rule đã promote bị gỡ |
| weight ≥ 3, conversations ≥ 2, contrary = 0 | Promote |
| weight ≥ 3, conversations ≥ 2, contrary = 1 | Giữ lại: có một hội thoại phản bác |
| Hơn 2 lesson đủ điều kiện trong một lần chạy | Phần còn lại đợi lần sau |
| Phạm vi (mọi project, hoặc một project) đã có 8 rule | Giữ lại: retire bớt một rule trước |

## 4. Rule nằm ở đâu

- **Mọi project:** entry `learned_<id>` của memory global, tại
  `constraints/learned`. Digest luôn hiện entry này ở mọi lượt, trước các entry
  được xếp hạng. Nội dung là bài học kèm dấu vết của nó (tín hiệu, hội thoại,
  ngày, id lesson). Store ghi thay đổi là do `learning` làm, với cause `learn`.
- **Một project có kho markdown** (có `memory-lint.json` ở gốc nơi lesson được
  thấy, xem [MARKDOWN_MEMORY.vi.md](MARKDOWN_MEMORY.vi.md)):
  - file `memory/decisions/<ngày>-learned-<slug>.md`, mỗi dòng mang tag
    `[inferred]` (rule, lý do, khi nào xem lại) hoặc `[observed]` (từng trích
    dẫn). Slug là các chữ cái và chữ số ASCII của rule; rule không có ký tự
    nào như vậy (tiếng Việt, tiếng Trung) lấy id của lesson làm tên. Tên đã có
    file khác giữ thì thêm `-2`, `-3`..., nên rule không bao giờ ghi đè rule
    khác; đặt lại cùng một lesson thì tìm lại đúng file của nó;
  - một con trỏ dưới `## Current Decisions` của index mà config của kho chỉ
    định (mặc định `MEMORY.md`), thay cho placeholder `(none yet)`:
    `` - <tiêu đề> (learned <ngày>, <lesson>) - `<file>` ``, tiêu đề là 60 ký
    tự đầu của rule. Nội dung rule nằm trong file decision: index có mặt ở
    mọi lượt và dẫn agent tới đó. Retire hay đổi lời rule sẽ gạch dòng nhắc
    tới file đó, ở dạng này hay ở dạng dài hơn mà 0.8.7 trở về trước đã ghi.

  Cả hai đi qua bộ ghi duy nhất của kho
  ([MARKDOWN_MEMORY.vi.md](MARKDOWN_MEMORY.vi.md), mục 5). Kho vẫn qua
  `ha memory lint`.
- **Một project không có kho:** entry memory global như trường hợp đầu, đánh dấu
  thuộc project đó, nên chỉ các lượt của project đó thấy.

## 5. Fact, rule đã làm được gì, và meditation

**Fact.** Một deviation về một project (một file, một lệnh hay một check không
như agent giả định) là điều project đã cho thấy. Đó là quan sát, không phải suy
luận, nên không cần lặp lại: nó được giữ thành fact (tier `fact`) ngay lần đầu
thấy.
- Có kho markdown: thành dòng `- [observed] <text> (<ngày>, <lesson>)` dưới
  `## Facts` của `memory/projects/<project>.md`; project được thêm một dòng vào
  index nếu chưa có.
- Không có kho: thành entry memory được xếp hạng của project đó, tại
  `facts/learned`, được gọi ra khi công việc chạm tới, không nằm trong mọi lượt.
- Fact mà memory đã có thì chính là entry đó.
- Một `resolution` về một project (lỗi mà tool result cho thấy, và cách sửa đã
  làm nó hết) cũng được giữ như vậy.
- Hai hội thoại phản bác thì retire nó và gạch ngang dòng của nó.
- Fact không tính vào giới hạn rule nào.

Review được dặn chỉ báo deviation của project khi có tool result hoặc file cho
thấy. Lời sửa, và lesson cho mọi project, là rule và phải đợi ngưỡng.

**Rule đã làm được gì.** Tín hiệu sau khi promote là hồ sơ của rule: rule có tác
dụng thì không còn bị sửa nữa. Rule bị sửa sau khi promote nhiều như lúc nó cần
để thành rule (weight 3 từ 2 hội thoại) bị gắn nhãn `STILL CORRECTED`:
- `list`, `show` và report đều nêu;
- danh sách lesson trong review cũng nêu;
- meditation đổi câu chữ hoặc retire nó.

**Meditation.** Mỗi ngày tối đa một lần, sau một review, khi có hai lesson đang
mở trở lên, một lượt model đọc tối đa 60 lesson đang mở, mỗi lesson kèm độ vững
và các trích dẫn mới nhất. Nó trả về:
- `merge`: các lesson thực chất là một (khác chữ hoặc khác ngôn ngữ), cùng phạm
  vi và tier. Tín hiệu dồn về một lesson, các lesson kia retire với lý do đã
  gộp.
- `reword`: rule vẫn bị sửa, hoặc lesson đọc như một chỉ dẫn.
  - Rule do loop ghi có phiên bản mới của entry memory, hoặc dòng decision được
    thay thế: gạch ngang, dòng mới đặt bên cạnh.
  - Dòng của fact cũng được thay thế như vậy.
  - Entry `covered` là lời của người dùng nên không bị đổi chữ.
- `retire`: lesson mà các trích dẫn sau này phản bác.
- `synthesis`: hai ba câu về những gì lặp lại. Được giữ cùng các thay đổi (30
  meditation gần nhất), và report hiện bản mới nhất.

- `generalize` và `evolve`: xem §6.

Ngưỡng chạy lại sau đó, vì tín hiệu đã gộp có thể đủ điều kiện. Hội thoại được
báo từng thay đổi: `learning loop: meditation: merged lesson …`.

## 6. Từ continuous-learning-v2 của ECC

Continuous-learning-v2 của [ECC](https://github.com/affaan-m/ECC) (đọc ở
`25278e7`) giữ các *instinct*: một điều "khi X thì làm Y" kèm độ tin cậy, domain,
và phạm vi project hoặc global. Lesson của ha vốn đã là như vậy, nên các tính
năng của v2 được đưa vào lesson chứ không đặt thành một cơ chế bên cạnh.

| ECC v2 | ha | Vì sao |
|---|---|---|
| Một observer Haiku chạy nền đọc mọi lời gọi tool, mỗi 5 phút hoặc mỗi 20 quan sát | Review tự động đọc hội thoại, kể cả tool result, sau các lượt và khi phiên lặng | Một bên đọc hội thoại, không phải hai |
| Bốn mẫu: lời sửa, cách gỡ lỗi, workflow lặp lại, sở thích dùng tool | Các kind `correction`, `deviation`, `resolution` (một lỗi và cách sửa đã làm nó hết), `preference` (tool hay cách làm người dùng đã chọn); workflow lặp lại thuộc về learned skill | Observer của ECC hoàn toàn không thấy lời sửa (nó không ghi prompt); review của ha thì thấy |
| `domain` cho mỗi instinct | `domain` cho mỗi lesson: code-style, testing, git, debugging, workflow, file-patterns, other | |
| Độ tin cậy 0.3 / 0.5 / 0.7 / 0.85 theo số lần; +0.05 / -0.1 / suy giảm chỉ có trong tài liệu, không có trong code | `confidence` tính từ độ vững: weight dưới 3 là 0.3, 3-5 là 0.5, 6-10 là 0.7, từ 11 là 0.85, mỗi hội thoại phản bác trừ 0.1; tín hiệu cũ tính một nửa | Ngưỡng vẫn là thứ promote; confidence cho biết độ vững đã tăng bao nhiêu |
| SessionStart chèn instinct từ 0.7, tối đa 6 | Rule đã promote là entry `constraints`, có ở mọi lượt | |
| `promote <id>`; tự promote khi có ở 2+ project và trung bình 0.8 | `ha learning promote <id>`; `generalize` của meditation gộp cùng một lesson ở hai project trở lên thành một lesson cho mọi project, rồi lesson đó qua ngưỡng như mọi lesson khác | Model phán xét "cùng một lesson", không phải trùng từ khoá; ngưỡng quyết định, không phải trung bình |
| `/evolve` gom instinct theo từ khoá trong trigger thành skill, command và agent, ghi vào chỗ không gì nạp | `evolve` của meditation gom hai rule đã promote trở lên của cùng một workflow thành một learned skill, qua bộ promote learned skill và các check của nó, vào thư viện agent nạp skill | Skill dùng được ngay và giữ các rule: entry hay dòng decision riêng của từng rule được gỡ ra (placement `skill`), nên workflow chỉ nằm ở một chỗ. Các rule phải có hiệu lực ở cùng chỗ với skill: tất cả cho mọi project, hoặc tất cả của project này |
| `export` / `import` (file hoặc https), `status` | `ha learning export`, `import <file>`, `status` | Chỉ file: import là việc của người dùng |

Một rule mà review ghi vào learned skill (`skillFiles`) được báo với
`saved_as: "skill:<tên>"`: lesson chính là skill đó (placement `skill`), và loop
không ghi thêm rule nào nói lại điều ấy.

**Status** liệt kê lesson đang mở theo domain, mỗi lesson một thanh mười ô cho
confidence. **Export** ghi một tài liệu JSON (`kind: ha-lessons`) gồm các lesson
đang mở có confidence từ `--min-confidence`, theo `--scope global|project|all`,
mỗi lesson kèm tín hiệu; placement không đi theo. **Import** đọc tài liệu đó:
- lesson đã có (cùng id, hoặc cùng text và phạm vi) nhận thêm các tín hiệu nó
  chưa có; lesson mới là candidate, rồi ngưỡng chạy;
- hội thoại của tín hiệu nhập vào là `import:<id>`, nên nhập cùng file lần thứ
  hai không thêm gì;
- lesson của project thành lesson của project ở thư mục hiện tại;
- text hay trích dẫn có giá trị trông như key thì bị bỏ qua;
- `--dry-run` cho biết sẽ đổi gì.

**Promote** biến rule của một project thành rule cho mọi project: rule của
project được gỡ, và ngưỡng ghi nó vào chỗ dành cho rule global. Fact thì ở lại
với project của nó.

## 7. `ha learning`

| Lệnh | Làm gì |
|---|---|
| `ha learning list [--all] [--json]` | Các lesson với trạng thái, tier, phạm vi, kind, weight, số hội thoại và rule đã làm được gì kể từ khi promote (kèm lesson đã retire khi có `--all`) |
| `ha learning show <id>` | Chỗ đặt rule, mọi tín hiệu kèm trích dẫn, và lịch sử |
| `ha learning distill` | Chạy ngưỡng ngay; in những gì đã promote, retire và giữ lại |
| `ha learning retire <id> [--reason …]` | Gỡ rule: xoá entry memory, hoặc gạch ngang (có ghi ngày) dòng rule trong decision và dòng trong index (hoặc dòng của fact); là thay thế, không phải xoá. Entry của lesson `covered` là lời của người dùng nên được giữ lại; khi retire, hoặc khi hai hội thoại phản bác, ha báo rõ điều đó |
| `ha learning report [--all] [--html FILE]` | Những gì lặp lại: rule còn bị sửa, rule, candidate theo weight, fact, các con số, và phần tổng hợp của meditation gần nhất; với `--html` thì xuất thêm một trang riêng. Cuối report là các lần tự đánh giá trong 30 ngày gần nhất ([SELF_EVALUATION.vi.md](SELF_EVALUATION.vi.md)) |
| `ha learning status` | Các lesson đang mở theo domain, kèm confidence |
| `ha learning export [-o FILE] [--min-confidence N] [--scope global\|project\|all]` | Các lesson đang mở thành một tài liệu JSON để máy khác import |
| `ha learning import <FILE> [--dry-run] [--min-confidence N] [--scope …]` | Gộp một bản export: lesson mới thành candidate, lesson đã có nhận thêm tín hiệu còn thiếu; rồi chạy ngưỡng |
| `ha learning promote <id>` | Rule của một project thành rule cho mọi project; rồi chạy ngưỡng |

Hội thoại đang chạy được báo những gì ngưỡng vừa đổi sau review:
`learning loop: learned rule les_…: …`, `learned fact …`, `retired lesson …`.

## 8. Lưu trữ

`<data>/harness/lessons.json`, ghi nguyên file dưới khoá `lessons.lock`. Mỗi
lesson giữ:

- id, text, kind, domain và tier;
- project (`key`, `name`, `root`);
- trạng thái (`candidate`, `promoted`, `retired`) và thời điểm promote;
- tín hiệu (thời điểm, hội thoại, `against`, trích dẫn);
- chỗ đặt rule: key `memory`, key `covered`, root và file `markdown`, root và
  file `markdown_fact`, hoặc tên `skill`;
- các sự kiện.

File cũng giữ các meditation. Mỗi lesson giữ 50 tín hiệu và 100 sự kiện mới
nhất. File không parse được thì được báo lỗi và không bao giờ bị ghi đè.

**Lesson đi theo entry của nó.** Mỗi lần chạy ngưỡng, trước tiên ha kiểm tra
entry memory của lesson đã promote còn không:
- entry đổi tên thì đi theo;
- entry `covered` mà Deep Dream đã gộp mất thì được tìm lại theo nội dung, bằng
  các thước đo của việc từ chối entry trùng; không tìm thấy thì lesson thành
  candidate trở lại;
- rule do loop ghi mà người dùng đã xoá thì lesson của nó retire.

Deep Dream không thấy các entry của chính loop (`source: learning`): meditation
lo các entry đó.
`lessons_meditated` nằm cạnh đó ghi lần meditation gần nhất.

## 9. Kiểm thử

- Unit test trong `lessons.rs` phủ:
  - cách đếm của ngưỡng, kiểm tra trích dẫn, các giới hạn và phản bác;
  - mọi chỗ đặt rule và việc retire, fact có và không có kho;
  - rule vẫn bị sửa sau khi promote, report và trang HTML;
  - merge, reword (phiên bản mới của entry; entry của người dùng giữ nguyên) và
    retire của meditation;
  - file hỏng.

  Một test trong `refine.rs` chạy ba lượt review qua `auto_refine`: hai tín hiệu
  trong cùng một hội thoại vẫn chỉ là candidate, còn tín hiệu từ hội thoại thứ
  hai tạo ra entry `constraints`.
- Live suite `scripts/testing/live-tests/learning.sh` chạy `ha exec` với model
  thật, review sau mỗi lượt:
  - một lời sửa nói ngay lúc đó ("dài quá, 1-2 câu thôi") thành lesson kèm
    trích dẫn, và một hội thoại thì chưa tạo rule;
  - hội thoại thứ hai biến nó thành rule, chỉ ghi một entry: của loop, hoặc
    entry của chính review khi review đã lưu sở thích đó;
  - request kế tiếp mang rule đó;
  - quy tắc người dùng nói rõ thì không có entry thứ hai;
  - retire chỉ gỡ những gì loop đã ghi;
  - lesson của một project thành decision trong kho markdown của project, qua
    lint, và retire thì gạch ngang.
