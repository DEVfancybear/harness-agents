# Kho memory dạng markdown

[English](MARKDOWN_MEMORY.en.md) | Tiếng Việt

Một project có thể giữ memory bằng các file markdown ở thư mục gốc, theo bố cục
của
[markdown-agent-memory](https://github.com/ucsandman/Agnostic-AI/tree/master/packages/markdown-agent-memory)
(MIT, đọc ở commit `aab09ff`, ngày 2/10/2026). ha tạo một kho rỗng, đưa luật và
index của kho vào mọi lượt, và kiểm tra kho bằng bản port sang Rust của
`memory-lint.mjs` trong package. Kho này chạy song song với memory riêng của ha
([MEMORY_STORE.vi.md](MEMORY_STORE.vi.md)), không thay thế nó.

Code: `crates/harness-cli/src/host/memory/markdown.rs`; các file đi kèm nằm trong
`crates/harness-cli/assets/markdown-memory/`.

## 1. Bố cục

| Đường dẫn | Tầng | Luật |
|---|---|---|
| `MEMORY.md` | ROM | Index định tuyến: chỉ có con trỏ, không có dữ kiện. Có giới hạn (15.000 ký tự). |
| `memory/people/`, `memory/projects/`, `memory/decisions/` | Disk | Mỗi dòng dữ kiện mang `[stated]`, `[observed]`, `[inferred]` hoặc `[suggested]`. Dữ kiện đổi thì gạch ngang và ghi ngày, không bao giờ xoá. |
| `memory/context/`, `memory/YYYY-MM-DD.md` | RAM | Viết lại thoải mái, có giới hạn (30.000 ký tự). Điều gì cần giữ lâu thì chuyển sang file disk. |
| `memory/archive/` | Tape | Đóng băng: một tài liệu vào nguyên vẹn và không bao giờ bị sửa. |
| `memory-lint.json` | | File nào thuộc tầng nào, và các giới hạn. Có file này nghĩa là project giữ một kho. |

Git là lịch sử của kho: `log` cho biết dữ kiện đúng trong khoảng nào, `blame` ai
viết dòng đó, `diff` là việc thay thế.

## 2. Memory nào giữ gì

| | Tool `memory` của ha | Kho markdown |
|---|---|---|
| Nằm ở | `<data>/harness/memory.sqlite3`, theo hội thoại và global | Các file trong project, trong Git |
| Giữ | Sở thích và quyết định của người dùng xuyên project, trạng thái việc | Người, quyết định, trạng thái và bài học của project này |
| Ai ghi | Tool `memory`, lượt review, Deep Dream | Tool file của agent, và người dùng bằng editor |
| Đọc | Digest ở mọi lượt, tìm bằng `memory` | `MEMORY.md` ở mọi lượt; các file khi index chỉ tới |

## 3. Lệnh

### `ha memory init [--root DIR] [--hook] [--examples]`

Ghi `MEMORY.md` (index của package, bỏ các dòng ví dụ), `memory-lint.json`
(template của package) và năm thư mục tầng, mỗi thư mục có một `.gitkeep`. Khi
không có `--root` thì gốc là thư mục đang đứng.

- File nào đã có thì giữ nguyên. Không ghi đè thứ gì kho đang giữ.
- Nếu project có `AGENTS.md` (hoặc `CLAUDE.md`), file đó được thêm vào ROM với
  `maxLines: 200`, độ dài `ha doctor` khuyên cho file entry.
- `--hook` ghi `.git/hooks/pre-commit` (qua `git rev-parse --git-path`, nên
  worktree và `core.hooksPath` đều đúng). Hook chạy
  `ha memory lint --staged --quiet` từ thư mục của kho và chặn commit khi có lỗi.
  Hook đã có thì để nguyên, và lệnh nói cần thêm gì vào đó.
- `--examples` ghi các file ví dụ của package vào `memory/templates/`:
  `PERSON.md`, `PROJECT.md`, `DECISION.md` và `DAILY.md`. Thư mục này không
  thuộc tầng nào, nên dữ kiện mẫu không bao giờ bị đọc như dữ kiện của project và
  lint bỏ qua chúng. Policy dặn agent theo đúng định dạng của các file này.

### `ha memory migrate [--root DIR] [--from FILE] [--model M] [--thinking T] [--approval MODE]`

Bước đầu tiên của install prompt: biến một file memory phẳng thành kho.

1. Chưa có `memory-lint.json` thì kho được tạo trước, như `init`.
2. File phẳng (`--from`, nếu không có thì là `MEMORY.md` ở gốc) được chép nguyên
   vẹn vào `memory/archive/<ngày>-<tên>.flat.md`. File tape nhận nguyên một tài
   liệu, nên không mất gì. Một `MEMORY.md` phẳng được thay bằng index rỗng. File
   rỗng, hoặc index rỗng do `init` ghi, thì không có gì để migrate.
3. Một lượt `ha exec` chạy ở gốc, mặc định với `--approval auto-edit` (chỉ cần
   sửa file), giao cho agent:
   - tách file thành people, projects, decisions và daily note;
   - gắn tag cho mọi dòng dữ kiện: `[stated]` chỉ khi file nói người dùng đã nói;
   - gạch ngang kèm ngày các dữ kiện đã đổi;
   - dựng lại `MEMORY.md` thành index thuần, với các đường dẫn có thật;
   - chỉ bỏ những gì suy lại được.

   Luật của kho có mặt trong các lượt của lần chạy đó.
4. Kho được lint so với working tree. Có lỗi thì exit 1, kèm tên file đã lưu
   trữ.

### `ha memory lint`

`memory-lint.mjs` 1.0.0 của package, viết lại bằng Rust. Check, flag, báo cáo
dạng text, cấu trúc `--json` và exit code đều giống bản gốc, nên hook hay health
check viết cho bản này dùng được cho bản kia.

| Flag | Ý nghĩa |
|---|---|
| `--root DIR` | Gốc của kho (thư mục đang đứng) |
| `--config FILE` | Config, tính từ gốc (`memory-lint.json`) |
| *(không flag)* | Working tree so với `HEAD`, gồm cả file chưa track |
| `--staged` | Những gì commit tiếp theo sẽ ghi |
| `--base REF` | Working tree so với `REF` |
| `--no-diff` | Không đọc diff: chỉ kiểm giới hạn và link của index |
| `--json`, `--quiet` | Một dòng JSON; chỉ các lỗi và dòng kết quả |
| `--version` | `1.0.0`, phiên bản memory-lint được tái hiện |

| Check | Fail khi |
|---|---|
| `rom-caps` | Một file boot vượt giới hạn ký tự hoặc dòng, hoặc tổng các file boot vượt `totalMaxChars` |
| `ram-caps` | Một file làm việc có trong diff vượt giới hạn (file không bị đụng tới chỉ cảnh báo) |
| `disk-provenance` | Một dòng dữ kiện mới trong people, projects hoặc decisions không có tag |
| `disk-history` | Một dòng đã gạch bị xoá mà diff không thêm lại dòng nào giống nó |
| `tape-frozen` | Một file đã lưu trữ bị sửa, bị xoá, đổi loại, hoặc đổi tên kèm sửa nội dung |
| `index-links` | Một đường dẫn trong `MEMORY.md` trỏ tới chỗ không có gì |

Mỗi kết luận in ra nó đã kiểm bao nhiêu. Exit code: 0 là qua, 1 là có check
fail, 2 là config hỏng (thiếu, không phải JSON, regex trong `ram.patterns` sai).

Khác bản gốc, và đều có chủ ý:

- File chưa track có tên không phải ASCII vẫn được đọc. Bản gốc liệt kê chúng khi
  git còn bật quoting, không mở được tên đã quote nên bỏ qua file, vì thế một dòng
  thiếu tag trong `memory/people/bảo.md` vẫn qua.
- Lỗi JSON do serde diễn đạt, không phải V8.
- Tag được so như từ, không như biểu thức chính quy; `--staged`, `--base` và
  `--no-diff` loại trừ nhau thay vì flag cuối thắng.

Bản port được so với bản gốc trên 69 kho (chạy cả hai trên cùng file, so output
text, `--json`, `--quiet` và exit code): 68 khớp từng byte, trường hợp còn lại chỉ
khác câu báo lỗi JSON.

## 4. Trong lượt

Khi có `memory-lint.json` ở gốc project, mọi lượt mang hai block bắt buộc:

- `markdown-memory:policy`: toàn bộ policy vận hành của package, diễn đạt theo
  ha (`assets/markdown-memory/policy.md`, khoảng 1.100 từ). Gồm:
  - nguồn sự thật và các tầng;
  - khi nào và ghi vào đâu: quyết định, thay đổi trạng thái, vướng mắc, lỗi, bài
    học, sở thích ổn định, "nhớ cái này";
  - tag nguồn gốc, kèm phép thử nguồn gốc cho quyết định và lưu ý về bản tóm tắt;
  - ngưỡng promote (3+ tín hiệu trên 2+ phiên, quá 30 ngày tính nửa, tín hiệu
    phản bác, dấu vết, bài học là dữ liệu);
  - thay thế bằng gạch ngang;
  - hiệu chỉnh: lời khẳng định khớp bằng chứng, số liệu có ngày, không ghi điều
    suy lại được, kiểm tra trực tiếp tool và phiên bản có tên;
  - luật đọc: file hẹp nhất trước, tối đa 5 nguồn, stale hoặc unknown, sửa mâu
    thuẫn ngay trong file, tìm kiếm chỉ để định vị;
  - bảo trì: giới hạn, gộp file, lưu trữ, tiêu chuẩn khi thất bại;
  - `ha memory lint`.

  Block này mang quyền của người dùng, như `AGENTS.md`, vì người dùng đã chọn giữ
  kho.
- `markdown-memory:MEMORY.md`: index, gửi cùng chỗ với memory digest, đóng khung
  là dữ liệu và không có quyền ra chỉ dẫn (agent và người dùng cùng viết nó).
  Index bị cắt ở `maxChars` của nó trong config (15.000 nếu không có), cắt trọn
  ký tự, và lượt được báo khi bị cắt.

Review tự động được báo rằng project có kho, nên nó không lưu dữ kiện nào của
project thành edit memory: agent ghi chúng vào kho
([LEARNING_LOOP.vi.md](LEARNING_LOOP.vi.md)). Tool `memory` cũng từ chối tạo
entry `scope: project` mới, và lời từ chối chỉ sang kho: một dữ kiện giữ ở cả hai
nơi thì sẽ có một nơi bị cũ.

Index chỉ được đọc từ bên trong project và không bao giờ từ đường dẫn được bảo vệ.
Config không đọc được thì vẫn gửi luật, và lượt được báo vì sao thiếu index.
Project không có `memory-lint.json` thì không nhận block nào. Chat và
`ha exec` mang các block này; child được giao việc thì không, vì agent giao việc
là bên giữ memory.

## 5. `ha doctor`

Phần State có thêm một dòng cho mỗi check lint chạy với `--no-diff` (giới hạn và
link của index; các check theo diff thuộc về hook commit): FAIL và WARN đúng như
lint chấm, kèm chi tiết đầu tiên và cách sửa. Project không có kho thì không có
dòng nào.

## 6. Kiểm thử

- Unit test trong `markdown.rs` phủ mọi check, báo cáo text và JSON, `init`, các
  block của lượt, giới hạn của index và việc từ chối đường dẫn. Giá trị mong đợi
  là những gì `memory-lint.mjs` in ra trên cùng các kho.
- Live suite `scripts/testing/live-tests/markdown-memory.sh` chạy `ha` với model
  thật: agent ghi một dữ kiện có tag, thay một quyết định bằng cách gạch ngang,
  trả lời từ index, và hook chặn một dữ kiện thiếu tag.
