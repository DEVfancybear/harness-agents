# Thư viện knowledge như một wiki

Một thư viện knowledge (`.harness/knowledge` của dự án, `<data-dir>/knowledge`
cho phạm vi global) được viết lần lượt qua từng cuộc hội thoại. Mỗi cuộc chỉ
thấy những gì nó đã thấy: một trang liên kết tới một trang chưa từng được viết,
hai cuộc hội thoại viết cùng một chủ đề dưới hai tên, và không trang nào cho
biết các trang khác ở đâu. Bài "The components of agent memory" của Anthropic
(6/11/2026) mô tả cách lấp các khoảng trống này: một lượt chạy nền thứ hai đọc
toàn bộ wiki, trỏ lại chín liên kết tới một trang không tồn tại về một trang có
sẵn cùng tài liệu, thêm một trang mục lục, gộp các trang trùng và đối chiếu các
khẳng định với nguồn. Bài cũng nêu lỗi cần tránh: một wiki lớn hơn cả các tài
liệu nó tóm tắt, nên chẳng trừu tượng hóa được gì (tấm bản đồ to bằng cả đế
chế của Borges). Lượt wiki của ha làm việc này (`knowledge/wiki.rs`,
`knowledge/wiki_store.rs`).

## Liên kết treo

Trước đây một liên kết trong nội dung trỏ tới trang không tồn tại bị bỏ khỏi đồ
thị mà không để lại dấu vết. Từ bản này store giữ nó làm **liên kết treo**
(dangling link) của trang:

- lệnh đọc của tool `knowledge` liệt kê các liên kết `dangling` của trang, và
  kết quả list hay write cũng mang chúng;
- `/knowledge status` đếm chúng theo từng thư viện và nêu năm liên kết đầu dưới
  dạng `trang -> đích`;
- `ha doctor` (mục State) cảnh báo thư viện có liên kết treo và nêu tên chúng;
  thư viện không có liên kết treo thì đạt.

Liên kết treo không phải là một cạnh: backlink, đồ thị và quy tắc xóa vẫn chỉ
thấy các liên kết tới trang có thật. Nó tự được giải quyết khi trang đích được
viết.

## Lượt chạy

Lượt chạy là một job nền của hàng đợi Sleep (`Kind::Wiki`,
[SLEEP.vi.md](SLEEP.vi.md)), mỗi thư viện một job, nên mỗi thư viện chỉ có một
lượt chạy tại một thời điểm, kể cả giữa các tiến trình; nó thất bại, chờ lùi và
bỏ cuộc như các job khác.

**Khi nào đến hạn** (`wiki::due`, kiểm tra ở cuối mỗi lượt): thư viện đang bật,
có từ hai trang trở lên, đã thay đổi kể từ lượt trước (một dấu vân tay của
đường dẫn và hash các trang, lưu trong `.ha-wiki.json`), có việc cho một lượt
chạy, và hoặc một review đã viết một trang kể từ lượt trước (trigger `review`)
hoặc đã qua một ngày (trigger `due`). Thư viện mà lượt chạy chưa thấy bao giờ
bắt đầu thời gian chờ một ngày ngay lần kiểm tra đầu, nên một thư viện có từ
trước khi có lượt wiki không bị gửi cho model ngay lượt đầu sau khi nâng cấp.
`HA_WIKI_DUE_NOW=1` bỏ thời gian chờ, cho kiểm thử. Thư viện đã thay đổi nhưng không có việc thì không được kiểm tra lại
cho tới khi nó đổi tiếp. Lượt chạy chỉ diễn ra khi review tự động chạy
(`autoRefine`, `/refinements`, tính năng `reviews`) và knowledge đang bật
(`/knowledge off`, `HA_MEMORY_OFF` `knowledge`).

**Nó đọc gì**: đường dẫn, tiêu đề và tóm tắt của mọi trang, và toàn văn (cắt ở
6.000 ký tự mỗi trang, tổng cộng 100.000) các trang mà mỗi loại việc cần:

| Việc | Tìm bằng |
|---|---|
| Liên kết treo | store, kèm các trang có sẵn có từ ngữ gần nhất với tên của trang còn thiếu (một lượt tìm trong store theo tên tệp) |
| Hai trang cùng một chủ đề | phép kiểm tra cùng chủ đề của store, theo từng cặp: phần lớn từ ngữ của tiêu đề và tóm tắt một trang, có trọng số theo độ hiếm, nằm trong trang kia (`SAME_SUBJECT_SHARE`, 0,7), tối đa sáu cặp |
| Một trang mục lục | hơn `INDEX_MIN_PAGES` (12) trang và không có trang nào liên kết tới 60% các trang còn lại |
| Khẳng định cần đối chiếu | các trang có nguồn là một tệp trong workspace (một đường dẫn trong workspace, cho phép `:dòng`) hoặc một cuộc hội thoại của dự án (một conversation id trong nguồn), chưa được đối chiếu kể từ khi trang hoặc nguồn đổi lần cuối; tối đa bốn, mới nhất trước, mỗi nguồn cắt ở 12.000 ký tự. Một đường dẫn được bảo vệ (`.env`, `.git`, `.harness`, một khóa, một tên có `secret`, `credential` hay `password`, như các tool tệp từ chối) và một tệp Git bỏ qua không bao giờ là nguồn: văn bản của chúng sẽ bị gửi cho model. Thư viện global, dùng chung cho mọi dự án, không đọc đường dẫn tương đối theo workspace này: chỉ một đường dẫn tuyệt đối vào nó, hoặc một cuộc hội thoại |

Catalog (tối đa 30.000 ký tự, rồi số trang bị bỏ ra) và các liên kết treo (tối đa 8.000) tính vào cùng ngân sách. Một lần đối chiếu chỉ được đánh dấu xong khi trang và nguồn của nó có trong prompt; lần mà ngân sách bỏ ra chờ lượt sau. Mọi thứ nó đọc là dữ liệu, không phải chỉ dẫn: prompt của người biên tập nói
rõ điều đó, và host kiểm tra từng chỉnh sửa được đề xuất trước khi áp dụng.

**Nó được làm gì** - một danh sách JSON tối đa mười hai chỉnh sửa, mỗi chỉnh sửa
được áp dụng qua store với hash của trang mà lượt chạy đã đọc (trang người dùng
đổi trong lúc đó bị từ chối vì đã cũ), được ghi vào lịch sử trang là của wiki
(`cause` và `actor` là `wiki`, dòng log `by wiki`):

| Chỉnh sửa | Được kiểm tra |
|---|---|
| `repoint` một liên kết treo về một trang có sẵn | liên kết là liên kết treo của trang; trang đích tồn tại và không phải chính trang đó |
| `unlink` một liên kết treo, giữ nhãn của nó | liên kết là liên kết treo của trang |
| `create` trang còn thiếu | đường dẫn là một trang còn thiếu mà một liên kết trỏ tới; nó nêu các trang mà nó được viết từ đó, và các trang này chứa ít nhất 80% trọng số từ ngữ của nó (`CREATE_SUPPORT`); nguồn của nó là nguồn của các trang đó và nó liên quan tới chúng |
| `merge` hai trang | cặp là cặp mà phép kiểm tra cùng chủ đề tìm thấy; văn bản gộp giữ ít nhất 80% trọng số từ ngữ của mỗi trang (`MERGE_KEEP`); host nối nguồn và các trang liên quan của cả hai, trỏ mọi trang từng liên kết tới trang bị gộp về trang được giữ, và xóa trang bị gộp (lịch sử vẫn giữ nó, nên `revert` đưa nó trở lại) |
| `index` | được yêu cầu; không có gì trông như bí mật; chỉ gồm tiêu đề mục dài tối đa 120 ký tự và các dòng `- [Tiêu đề](đường-dẫn)`; liên kết tới trang không có thì bị bỏ, các trang bị sót được liệt kê ở cuối, và mục lục sót hơn một phần năm số trang thì bị từ chối; được viết ở `topics.md` (một trang của chính người dùng ở đó không bao giờ bị đụng tới) |
| `fix` một khẳng định | nguồn là một nguồn của trang và đọc được; câu trích có trong nguồn từng chữ (bỏ qua khoảng trắng và chữ hoa, ít nhất 8 ký tự); có văn bản thay thế; đoạn văn bị thay xuất hiện đúng một lần trong trang và là một câu dài tối đa 300 ký tự, hoặc tối đa một phần tư phần văn của trang, và không bao giờ quá 2.000 ký tự (bản sửa sửa một khẳng định, không viết lại trang; một trang chỉ có một khẳng định được sửa bằng cách thay khẳng định đó); ít nhất 80% trọng số từ ngữ mà bản sửa đưa vào thuộc về câu trích (`FIX_SUPPORT`; dấu chú thích cuối không phải là từ); có thể kèm `note`, một dòng định nghĩa chú thích cuối mà host thêm vào trang, được kiểm tra cùng các chú thích khác của trang ([bên dưới](#phát-biểu-có-bằng-chứng-somem)) |

Trọng số từ ngữ là thống kê token: `harness::query_terms` trên văn bản đã bỏ
dấu, mỗi từ có trọng số theo việc ít trang chứa nó (một IDF đã làm trơn). Không
danh sách từ nào phán xét văn bản; chỗ con số không quyết được thì model quyết.

## Giới hạn kích thước

Các trang có nguồn đọc được được giữ dưới `MAX_SIZE_RATIO` (1,0) lần số ký tự
của các nguồn đó, mỗi nguồn tính một lần: một thư viện lớn hơn những gì nó tóm
tắt thì chẳng trừu tượng hóa được gì. Chỉnh sửa nào đẩy tỉ lệ vượt ngưỡng (và
cao hơn mức hiện tại) bị từ chối. Các trang mà host không đọc được nguồn (URL,
tài liệu ở nơi khác) và trang mục lục không được tính. `/knowledge status` và
`ha doctor` hiện tỉ lệ: `size: N characters of P page(s) over S of their
readable sources (ratio R, kept under 1.00)`.

## Nơi thấy nó

- Phiên nhận một thông báo khi lượt chạy bắt đầu và một thông báo về việc nó
  đã làm (`made 2 repoint, 1 merge; 0 dangling link(s) left`).
- Diagnostics: `wiki.applied` và `wiki.refused` cho mỗi chỉnh sửa (phạm vi,
  loại chỉnh sửa, trang, và lý do từ chối), `wiki.pass` cho mỗi lượt (số áp
  dụng, số từ chối, số liên kết treo trước và sau, tỉ lệ kích thước); các bản
  ghi `sleep` của job.
- Mục *Knowledge wiki* của bug report đếm số lượt và từng loại chỉnh sửa đã áp
  dụng và bị từ chối, kèm các lần từ chối gần nhất; mục *Sleep* liệt kê job
  wiki đang chờ hoặc đã bỏ cuộc.

## Những gì nó không làm

- Nó không bao giờ sửa một trang chỉ liên kết tới trang có thật, một trang của
  người dùng ở `topics.md`, hay văn phong của một trang: chỉ liên kết, trang
  trùng, mục lục và những gì nguồn mâu thuẫn.
- Nó không tải URL để đối chiếu một trang: chỉ tệp trong workspace và các cuộc
  hội thoại của dự án là nguồn nó đọc được.
- Nó không biên tập các memory entry: Deep Dream làm việc đó
  ([MEMORY_STORE.vi.md](MEMORY_STORE.vi.md), mục 13).

## Phát biểu có bằng chứng (SoMem)

Những gì ha lấy từ một bài báo đọc ngày 13/11/2026, cách nó hoạt động, và
những gì không lấy.

- **arXiv 2611.11929**, *Can LLM Agents Organize Their Own Memory?* (Fu và
  cộng sự), ở đây gọi là SoMem. Bài giữ bộ nhớ thành ba tầng. **Source** là
  bản ghi bất biến của các tương tác. **Topic** là các tài liệu Markdown mà
  agent tự viết và sửa; đơn vị là một đoạn văn, có định danh ổn định để tài
  liệu khác liên kết tới, và mỗi phát biểu mang một chú thích cuối với **thời
  điểm sự kiện** (năm, tháng hoặc ngày, chính xác đến mức nguồn cho phép) và
  các **tham chiếu** tới những bản ghi Source làm căn cứ. **Core** là một tài
  liệu nhỏ, có giới hạn, luôn nằm trong ngữ cảnh. Runtime, không phải model,
  kiểm tra mọi lần sửa (Source không bị đụng tới, cấu trúc, mọi tham chiếu và
  liên kết đều giải quyết được) và một lần sửa bị từ chối để bộ nhớ nguyên như
  cũ. Sau một lần sửa được nhận, runtime tự dẫn ra ba góc nhìn mà model không
  phải viết: **Graph** (liên kết và backlink), **Timeline** (các phát biểu gom
  theo thời điểm sự kiện, mỗi cái trỏ về đoạn văn và Source của nó) và
  **Recent** (K đơn vị mới nhất, nằm trong ngữ cảnh đầu). Agent đọc bằng cách
  tìm kiếm (có lọc theo đường dẫn và khoảng ngày), mở tài liệu, theo liên kết,
  và mở Source đằng sau một phát biểu.
- Thí nghiệm bỏ từng phần trên LoCoMo: bỏ Topic hoặc Source thì kém đi, bỏ
  Source kém nhất; bỏ Core hay bất kỳ một góc nhìn dẫn xuất nào thì nằm trong
  nhiễu. Vậy điều quan trọng là bằng chứng giải quyết được. Các góc nhìn chỉ
  là đường đi tới nó.

ha đã có sẵn các tầng: journal là Source và không bao giờ bị sửa
(`history_search`, `history_read`), một trang knowledge là một Topic, host dẫn
ra đồ thị mỗi lần quét và không lưu gì cho nó, và mỗi trang giữ lịch sử của
mình. Nhưng một trang không có cách nói một phát biểu dựa vào nguồn nào: các
`sources` của nó là chuỗi ở mức trang mà không gì tra cứu khi agent ghi. Không
có thời điểm sự kiện (chỉ có lúc trang được ghi), không có địa chỉ cho một
đoạn văn (liên kết chỉ nêu trang và phần fragment bị bỏ), và không thể tìm
theo đường dẫn hay theo thời gian.

### Định dạng

Một trang vẫn là Markdown thường. Thêm hai thứ, cả hai đều tùy chọn.

```text
Joined A in May.[^e1] Moved to B in August.[^e2] ^work-2026

[^e1]: 2026-05; src/hr.rs:12-14, event_01a0f16c-740a-7185-8a4a5bf83b00
[^e2]: 2026-08; https://example.com/news
```

- **Chú thích cuối.** Dấu `[^id]` đứng sau phát biểu. Một dòng định nghĩa nó:
  `[^id]: <thời điểm sự kiện>; <tham chiếu>, <tham chiếu>`. Id dài 1 đến 32
  chữ cái, chữ số, `_` hoặc `-`. Thời điểm sự kiện là tùy chọn và là `YYYY`,
  `YYYY-MM` hoặc `YYYY-MM-DD`, giữ nguyên như đã viết; một định nghĩa chỉ có
  thời điểm vẫn hợp lệ. Các tham chiếu cách nhau bằng dấu phẩy và một dấu
  cách. Một chú thích có thể không có tham chiếu, và một phát biểu có thể
  không có chú thích.
- **Một tham chiếu** là một trong:
  - một tệp trong workspace: `path`, `path:line` hoặc `path:đầu-cuối`, tra
    trong workspace agent đang chạy rồi tới checkout chính của dự án;
  - một source của journal của dự án này, đúng như `history_search` trả về
    (`event_<uuid>`) và `history_read` nhận;
  - một cuộc hội thoại của dự án này (`task_<uuid>`);
  - một URL `http://` hoặc `https://`. Chỉ kiểm tra cú pháp: khi offline nó
    không giải quyết được, và ha không bịa ra một kết quả giải quyết có thể
    sai.
- **Một anchor** đặt địa chỉ cho một đoạn văn, một tiêu đề hoặc một mục danh
  sách: dòng cuối của nó kết thúc bằng một dấu cách và `^anchor` (3 đến 32
  chữ cái, chữ số hoặc `-`), duy nhất trong trang. Đó là block id của
  Obsidian và cũng là cú pháp của chính bài báo. Nó là văn bản thường nên
  không trình render hay bộ lọc nào bỏ được, và bước quét liên kết bỏ qua nó.
  Một liên kết gọi nó là `page.md#^anchor`; fragment không có dấu `^` là liên
  kết tới tiêu đề thông thường và không bị kiểm tra.

Cú pháp được phân tích, không được đoán nghĩa: không danh sách từ nào quyết
định thế nào là một phát biểu. Phát biểu là câu mà dấu theo sau, trong phạm vi
đoạn văn, mục danh sách hoặc tiêu đề: từ `.`, `!` hay `?` cuối cùng (hoặc dạng
CJK) theo sau bởi dấu cách, hoặc từ dấu chú thích trước đó, đến dấu chú
thích, tối đa 300 ký tự. Code, chú thích HTML và đoạn code nội dòng không được
đọc, như với liên kết.

Thư viện global, dùng chung cho mọi dự án, không đọc đường dẫn tương đối theo
một workspace. Trong đó một tham chiếu là URL, một source hay cuộc hội thoại
của dự án mà agent đang ở, hoặc một đường dẫn tuyệt đối vào workspace đó. Một
source của dự án khác được trích ở đây không mở được; `evidence` báo điều đó
cho tham chiếu ấy và không thất bại.

### Host kiểm tra gì khi ghi một trang

Với mọi lần ghi của agent, của review hay của lượt wiki:

| Kiểm tra | Bị từ chối khi |
|---|---|
| Ghép cặp | một dấu không có định nghĩa, một chú thích được định nghĩa hai lần, hoặc một định nghĩa không dấu nào dùng |
| Thời điểm | thời điểm sự kiện không phải `YYYY`, `YYYY-MM` hay `YYYY-MM-DD`, hoặc không phải ngày có thật |
| Tham chiếu | tệp không tồn tại, là đường dẫn được bảo vệ (`.env`, `.git`, khóa, tên có `secret`) hoặc tệp Git bỏ qua; dòng được trích vượt quá cuối tệp; source hay cuộc hội thoại không có trong journal của dự án này hoặc không còn dùng được; URL không có host |
| Anchor | hai đoạn văn của một trang có cùng anchor |
| Liên kết tới đoạn văn | một liên kết trong nội dung `page.md#^anchor` hoặc một mục `related` nêu trang không có anchor đó (liên kết tới trang không tồn tại vẫn là liên kết treo) |

Một lần ghi bị từ chối không đổi gì, và thông báo nêu tên chú thích, tham chiếu
và điều gì sẽ giải quyết được: `[^e1]: reference src/hr.rs:99: src/hr.rs has 5
line(s): lines 99-99 are out of range; cite lines within 1-5`. Kiểm tra bí
mật cũng phủ các chú thích vì chúng là một phần nội dung.

Chỉ những gì lần ghi mang vào mới bị kiểm tra. Một định nghĩa chú thích mà
trang đã có sẽ không bị tra lại, vì tệp nó trích có thể đã đổi và một lần sửa
không liên quan không được bị từ chối vì nó. Một lỗi trang đã có từ trước (một
chú thích cũ viết bằng văn xuôi, một liên kết tới đoạn đã bị viết lại) không bị
nêu lại. Trang không có chú thích không bị kiểm tra gì ở đây và được ghi từng
byte như trước; không trang nào bị di chuyển định dạng. Một mục `related` có
thể nêu `page.md#^anchor` và được kiểm tra, nhưng `related` vẫn là danh sách
trang: đoạn văn được trích bằng liên kết trong nội dung, nơi giữ nguyên
fragment.

Lượt wiki ghi qua cùng một lần kiểm tra. Một `fix` có thể mang `note`, một dòng
định nghĩa chú thích mà host thêm vào trang bên dưới khẳng định nó sửa (model
kết thúc `new` bằng dấu); một `create` hay `merge` giữ chú thích trong nội
dung. Chữ trong một dấu hay một định nghĩa không tính là chữ mà lần sửa mang
vào. Một store host không trang bị workspace và journal (một bài test, một
tool không ghi chú thích) chỉ đọc cú pháp của tham chiếu.

### Đọc nó

`knowledge` có thêm ba action, đều chỉ đọc nên agent con dùng được.

- `evidence` với `path` và `note` (id): với mỗi tham chiếu của chú thích đó,
  điều nó đang chứa. Một tệp cho các dòng được trích đánh dấu `>` kèm hai dòng
  xung quanh (40 dòng đầu khi không trích dòng); một source của journal cho
  văn bản của nó, cắt ở 4.000 ký tự, và `history_read` đọc phần còn lại; một
  URL được trả nguyên. Một tham chiếu không còn giải quyết được thì báo lý do
  riêng cho tham chiếu đó.
- `timeline`: các phát biểu có ngày của thư viện, cũ nhất trước, mỗi cái kèm
  thời điểm sự kiện như đã viết, trang và dòng, anchor của đoạn văn, chú thích
  và các tham chiếu. Một năm xếp trước các tháng của nó và một tháng trước các
  ngày. `path` là tiền tố thư mục; `since` và `until` là khoảng
  `[since, until)` mà `history_search` và tìm kiếm memory nhận, và một phát
  biểu nằm trong khoảng khi quãng thời gian của nó giao với khoảng (phát biểu
  ghi năm 2026 nằm trong mọi khoảng bên trong 2026). `limit` và `offset` để
  duyệt từng trang.
- `recent`: các trang mới nhất (`limit`, mặc định 8) kèm tiêu đề, tóm tắt,
  lúc nào và do ai. Đây là mức trang. Câu trả lời mức đoạn văn phải so từng
  phiên bản trang với phiên bản trước, mà lịch sử trang giữ nguyên các phiên
  bản trong 30 ngày (và mười bản mới nhất của mỗi trang), nên chỉ dẫn ra được
  cho trang mới đổi gần đây, phần còn lại sẽ là đoán.

`search` nhận cùng `path`, `since` và `until`. Một trang khớp khoảng khi nó
có một phát biểu mà thời điểm sự kiện nằm trong đó; trang không có phát biểu
nào có ngày thì khớp theo `updated_at`. Mỗi kết quả nói là cái nào:
`time_match` là `event_time` hoặc `updated_at`. Trang có các phát biểu có ngày
đều nằm ngoài khoảng thì bị loại, dù nó được ghi trong khoảng.

Khi đọc một trang sẽ thấy các `anchors` nó định nghĩa và `cited_by`: mỗi trang
liên kết tới một đoạn của nó, anchor, và phần đầu của đoạn. Một cạnh của đồ
thị mang các `anchors` mà liên kết trích. Một liên kết có đoạn đã mất (trang
đích được viết lại sau đó) không bị lượt quét từ chối: trang liệt kê nó là
`dangling_anchors`, và lần ghi kế tiếp mang nó vào sẽ bị từ chối.
`/knowledge status` thêm `, N dangling anchor(s) (trang -> đích#^anchor; ...)`
sau các liên kết treo, cùng dạng và chỉ khi có; thư viện không có thì đọc như
trước. `ha doctor` không báo chúng.

Không có gì được lưu cho các góc nhìn này. Một timeline ghi vào thư viện dưới
dạng tệp sẽ bị quét như trang và phải giữ đồng bộ bằng tay.

Catalog đi vào mỗi lượt thêm một phần ngắn, `recently written (not in the list
above)`: ba trang mới nhất theo `updated_at` mà danh sách xếp hạng chưa hiện.
Chỗ cho chúng được giữ bên trong giới hạn kích thước của khối (3.000 ký tự mỗi
thư viện), không cộng thêm vào. SoMem đặt Recent trong ngữ cảnh đầu; thí
nghiệm bỏ từng phần cho thấy tác động nhỏ, nên ở đây chỉ là ba dòng.

`/knowledge timeline [THƯ MỤC]` và `/knowledge recent [SỐ]` gọi cùng các
action đó.

### ha lấy gì, và không lấy gì

| SoMem | ha | Vì sao |
|---|---|---|
| Source: bản ghi bất biến | Journal, đọc bằng `history_search` và `history_read`. Không đổi | Đã có, và không bao giờ bị sửa |
| Topic: Markdown do agent viết | Các trang knowledge, không đổi. Định dạng thêm chú thích và anchor vào nội dung | Một trang phải vẫn là một trang: thư viện cũ, console và mọi trình render vẫn chạy |
| Đoạn văn là đơn vị, có id ổn định | Một anchor, ` ^id` ở cuối đoạn văn, tiêu đề hoặc mục danh sách | Cú pháp của chính bài báo. Văn bản thường nên không bộ lọc nào bỏ được |
| Chú thích có thời điểm sự kiện và tham chiếu Source | `[^id]: <thời điểm>; <ref>, <ref>`, tùy chọn với mọi phát biểu | Bài báo thấy các tham chiếu là điều quan trọng. Trang không cần thì vẫn hợp lệ |
| Runtime kiểm tra mọi lần sửa | Store kiểm tra ghép cặp, thời điểm, tham chiếu, anchor và liên kết ở mọi lần ghi, kể cả lượt wiki và review. Lần ghi bị từ chối không đổi gì | Một lần kiểm tra trong store là cùng một lần kiểm tra cho mọi người ghi |
| Graph: liên kết và backlink | Dẫn ra mỗi lần quét, như trước. Backlink nói nó trích đoạn nào; cạnh mang các anchor của nó | Hành vi ở mức trang không đổi |
| Timeline: phát biểu theo thời điểm sự kiện, dưới dạng tệp | `timeline`, dẫn ra mỗi lần gọi và không lưu ở đâu | Tệp trong thư viện sẽ bị quét như trang |
| Recent: K đơn vị mới nhất | `recent` (mức trang, K=8) và ba dòng trong catalog mỗi lượt | Tác động nhỏ trong bài; ở đây cũng nhỏ |
| Core: tài liệu có giới hạn luôn trong ngữ cảnh | Không thêm | `constraints` và các mục được ghim đã có trong mọi lượt ([MEMORY_STORE.vi.md](MEMORY_STORE.vi.md)) |
| Đọc: tìm kiếm có lọc đường dẫn và ngày, đọc, theo liên kết, mở Source | `search` với `path`, `since`, `until`; `read` với `cited_by`; `evidence` | Cùng các công cụ, thành các action của một tool `knowledge` |

**Không lấy.**

- **Tìm kiếm dày (embedding).** Bài cho agent chọn BM25 hay dày cho mỗi truy
  vấn. ha đã bỏ tìm kiếm ngữ nghĩa ở 0.8.8 có chủ ý: không gì rời máy, không
  tải model nào, và khi trượt thì model tìm lại bằng từ khác.
- **Timeline dưới dạng tệp**, `timeline/YYYY/MM/DD.md`. Chúng sẽ bị quét như
  trang, tính vào giới hạn kích thước, và phải giữ đồng bộ với các trang mà
  chúng lấy từ đó.
- **Một tài liệu Core riêng.** Thí nghiệm bỏ từng phần thấy nó nằm trong
  nhiễu, và `constraints` cùng các mục ghim đã làm việc của nó.
- **Một lượt cuối tổ chức lại cả bộ sưu tập.** Lượt wiki đã chăm thư viện ở nền
  và từ chối điều nó không kiểm tra được; một lượt viết lại mọi trang sẽ vừa
  chạm giới hạn kích thước vừa phải duyệt những gì nó đổi.
- **Chú thích cho mọi phát biểu.** Một trang văn xuôi chắt lọc không có thời
  điểm sự kiện và không có tệp phía sau sẽ buộc phải bịa tham chiếu. Phát biểu
  không chú thích thì hợp lệ; phát biểu có chú thích không giải quyết được thì
  không.
- **Giải quyết một URL.** Địa chỉ web chỉ kiểm tra cú pháp. Khi offline host
  không biết được, và tải nó từ một lần ghi sẽ gửi nội dung trang tới máy chủ.
- **Tra dòng theo phiên bản cũ của tệp.** Một tham chiếu được tra trong tệp
  như hiện tại. Trang có tệp được trích đổi về sau vẫn giữ chú thích của nó;
  `evidence` báo là nó không còn giải quyết được, và lượt wiki là nơi đối chiếu
  một khẳng định với nguồn đã đổi.
- **Thời điểm sự kiện thành mốc thời gian thật.** Bài giữ năm, tháng hoặc ngày
  như đã viết. ha cũng vậy, và một thời điểm thô giao với mọi khoảng chạm tới
  nó.
- **Một mục `related` giữ fragment.** `related` là danh sách trang trong
  metadata của trang và trong code di chuyển và gộp trang. Đoạn văn được trích
  trong nội dung.

### Test

- Định dạng, `notes.rs`: `a_time_keeps_the_precision_it_was_written_at_and_a_year_sorts_before_its_months`,
  `a_time_overlaps_a_window_by_its_whole_span`,
  `a_marker_takes_the_sentence_it_follows_and_its_definition_gives_time_and_references`,
  `markers_in_code_are_not_read_and_a_page_without_notes_has_none`,
  `faults_name_an_undefined_marker_an_unused_definition_a_bad_time_and_a_repeated_anchor`,
  `only_what_a_write_brings_in_is_checked`.
- Liên kết, `links.rs`:
  `a_footnote_definition_is_not_a_link_and_a_fragment_that_names_a_paragraph_is_a_citation`.
- Store và các góc nhìn, `views.rs`:
  `a_footnote_that_cites_a_missing_file_a_line_out_of_range_or_an_unknown_source_is_refused_and_nothing_is_written`,
  `a_valid_footnote_is_written_and_evidence_returns_the_cited_lines_and_the_journal_source`,
  `a_page_without_footnotes_round_trips_byte_for_byte_and_old_free_text_footnotes_do_not_block_an_edit`,
  `a_link_or_related_entry_to_a_missing_anchor_is_refused_and_a_scan_reports_one_that_went_missing`,
  `a_backlink_names_the_paragraph_it_cites_and_the_graph_edge_carries_the_anchor`,
  `the_timeline_orders_statements_by_event_time_across_precisions_and_filters_by_window_and_path`,
  `a_search_keeps_pages_under_a_path_and_in_a_window_and_says_which_time_matched`,
  `the_catalog_adds_the_newest_pages_the_ranking_did_not_show_within_its_bound`,
  `recent_lists_the_newest_pages_first_and_names_who_wrote_the_one_the_store_wrote`,
  `a_link_to_a_paragraph_is_checked_against_the_page_it_names`; và, đã đổi,
  `the_catalog_is_ranked_by_the_goal_and_says_what_it_leaves_out` trong
  `store.rs`.
- Tool, `tools.rs`:
  `the_knowledge_tool_checks_a_footnote_and_a_read_only_agent_opens_evidence_timeline_and_recent`,
  `a_footnote_holding_a_secret_is_refused_before_it_is_looked_up`.
- Lượt wiki, `wiki.rs`:
  `a_fix_may_cite_the_place_of_its_quote_and_a_note_that_does_not_resolve_changes_nothing`,
  `a_created_page_may_carry_footnotes_that_are_not_counted_as_new_words`.
- Yêu cầu lập bản đồ, `lifecycle.rs`:
  `the_bootstrap_request_asks_for_sources_scope_certainty_and_boundaries`.
