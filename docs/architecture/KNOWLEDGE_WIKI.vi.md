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
| `fix` một khẳng định | nguồn là một nguồn của trang và đọc được; câu trích có trong nguồn từng chữ (bỏ qua khoảng trắng và chữ hoa, ít nhất 8 ký tự); có văn bản thay thế; đoạn văn bị thay xuất hiện đúng một lần trong trang và là một câu dài tối đa 300 ký tự, hoặc tối đa một phần tư phần văn của trang, và không bao giờ quá 2.000 ký tự (bản sửa sửa một khẳng định, không viết lại trang; một trang chỉ có một khẳng định được sửa bằng cách thay khẳng định đó); ít nhất 80% trọng số từ ngữ mà bản sửa đưa vào thuộc về câu trích (`FIX_SUPPORT`) |

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
