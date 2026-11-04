# Sleep: các lượt memory chạy nền thành một hàng đợi bền

ha giữ memory theo nhiều tầng thay đổi với tốc độ khác nhau: các lượt của một
hội thoại (journal), store riêng của hội thoại, memory toàn cục, và các luật
harness tự tiến hoá. Bài "Sleep" (arXiv 2606.03979) nêu quy tắc chúng tuân
theo: một tầng nhanh phải được hợp nhất vào tầng kế tiếp **trước khi** nó bị
đặt lại hay ghi đè, nếu không những gì nó giữ sẽ mất. ha làm việc này bằng văn
bản, qua các lượt chạy nền, mỗi lượt gọi một model:

| Lượt | Đọc | Ghi |
|---|---|---|
| Review một hội thoại (review gate, rồi lần refine nó duyệt) | các lượt từ lần review trước | store của hội thoại, memory toàn cục, bài học, skill tự học, knowledge |
| Deep Dream (hợp nhất) | memory toàn cục, hoặc store của một hội thoại khi quá 40 entry, theo từng lô khi vượt ngân sách token | chính phạm vi đó, đã gộp và tỉa |
| Tiến hoá harness | các lần gọi tool hỏng trong 14 ngày qua | luật và ghi chú cho tool |

Một lần gọi model có thể hỏng: provider sập, chạm giới hạn, process chết.
Trước 0.8.9, một lần hỏng làm mất việc của lượt đó: bộ đếm lượt của review bị
về 0 ngay khi review được nhận, review mà một session để lại bị xoá ngay khi
đọc, một lần compact nằm trong khoảng cách hai mươi phút không được review gì
cả, lượt tiến hoá tự đánh dấu đã chạy trước khi gọi model rồi phải chờ một
ngày, và Deep Dream hỏng bị thử lại mỗi giờ mãi mãi. Từ 0.8.9 mỗi lượt là một
**job trên đĩa** trước đã (`memory/sleep.rs`).

## Hàng đợi

- Một job là một file JSON trong `<data-dir>/sleep/`, ghi trọn vẹn hoặc không
  ghi gì (file tạm rồi đổi tên). Nó ghi loại (`review`, `consolidation`,
  `evolution`), cái gì yêu cầu nó (`turn`, `compaction`, `idle`,
  `session_end`, `exec`, `due`), và với review thì store của project, hội
  thoại, session mới nhất cần đọc và số lượt nó bao.
- **Cái kích hoạt nó chỉ được đặt lại khi job đã ghi xong.** Bộ đếm lượt và
  lần gọi tool của app, `review_cadence.json` của `ha exec` và phần đuôi chưa
  review của một session được trả lại khi job không ghi được, nên các lượt đó
  được tính cho lần review sau thay vì bị quên.
- **Các job của một hội thoại gộp lại.** Một job review đang chờ của một hội
  thoại nhận phần việc của job mới: session mới hơn được đọc (session đã đọc
  trọn vẫn là trọn), số lượt cộng lại, các skill tự học đang dùng gộp vào, và
  thời điểm sớm hơn trong hai cái được giữ. Job đang chạy không nhận việc mới:
  việc đến trong lúc đó là một job riêng, chạy sau nó.
- **Nhận một job là giữ một khoá.** Khi nhận job, process giữ file khoá theo
  key của job suốt lúc lượt chạy, nên mỗi lúc chỉ một job của một hội thoại
  (và một Deep Dream, một lượt tiến hoá) chạy, trên mọi process của ha. Process
  chết thì khoá mất theo, và job của nó lại nhận được mà không tính thêm lần
  thử. Lần nhận đang chạy tự gia hạn thời hạn 30 phút của nó mỗi 5 phút, tối đa
  hai giờ; lần nhận hết hạn (process bị treo) bị nhận lại, và lượt nó đang chạy
  khi đó không thể xoá job hay ghi đè lỗi của nó lên: xong và hỏng chỉ tác động
  lên đúng lần nhận chúng giữ. Một lần hợp nhất hay tiến hoá là một lượt: được
  yêu cầu lại trong lúc đang chạy thì không ghi job thứ hai. Job cuối cùng của
  một key mang file khoá của key đi theo.
- **Store đã chuyển chỗ vẫn giữ job của nó.** Job review ghi thư mục store của
  project. Khi store chuyển sang key khác (`ha project rekey`, repository được
  tìm lại), store đã nhận nó (`project.json` liệt kê key cũ trong adopted)
  chạy job đó, và review sau của hội thoại gộp vào nó sẽ ghi store như hiện
  tại.
- **Xong thì job bị xoá; hỏng thì job ở lại.** Lượt hỏng đếm thêm một lần thử,
  ghi lỗi và loại lỗi (mã lỗi ổn định của provider, `timeout`, `truncated`
  hoặc `other`), rồi chờ: 5 phút với review, một giờ với hợp nhất hay tiến
  hoá, gấp đôi sau mỗi lần hỏng tiếp theo, tối đa một ngày. Lượt mới của một
  hội thoại kéo review hỏng của nó lên sớm; một lượt bảo trì được yêu cầu lại ở
  cuối lượt thì vẫn giữ thời gian chờ của nó.
- **Sau 6 lần hỏng liên tiếp job bỏ cuộc.** Nó vẫn nằm trên đĩa, không tự được
  thử lại nữa, và `ha doctor` cùng bug report nói ra điều đó. Lượt mới của một
  hội thoại là một job review mới; một lần hợp nhất hay tiến hoá đã bỏ cuộc thì
  không nhận job mới cho tới khi file của nó bị xoá, hoặc cho tới khi lượt đó
  không còn đến hạn vì đã thành công theo cách khác (`/refine --curate` cũng
  hợp nhất memory): khi đó job được bỏ đi.

## Các điểm kích hoạt

Vẫn là các điểm kích hoạt ha đã có; mỗi điểm ghi một job rồi chạy các job đến
hạn.

- **Cuối một lượt** (mỗi 25 lượt hoặc 50 lần gọi tool, sớm hơn sau khi compact
  hay một mốc): một job review của hội thoại. Sau đó mọi job review đến hạn của
  project chạy bên cạnh hội thoại: job này, phần cuối một session khác để lại,
  một job hỏng trước đó đã chờ hết thời gian chờ.
- **Compact trong một lượt**: một job cho đoạn đã bị tóm tắt, đọc từ journal,
  nơi mọi lượt vẫn còn. Review của compact bắt đầu nhiều nhất hai mươi phút một
  lần (`review_started`); một lần compact nằm trong khoảng đó không còn bị bỏ
  qua: job của nó chờ hết khoảng đó, gộp với mọi review khác của hội thoại
  trong lúc ấy thành một job.
- **Session im lặng** (mười phút, `HA_IDLE_REVIEW_SECONDS`): một job cho phần
  việc từ lần review trước, chạy ngay lúc đó.
- **Session kết thúc**: một job cho phần đuôi chưa review, được lượt kế tiếp
  trong project chạy. Session kết thúc không chờ thay đổi của process khác
  trên hàng đợi: lúc đó job được ghi thành một file riêng, gộp lại sau. File `refine-pending/` mà ha cũ để lại trong project vào
  hàng đợi ngay lần đầu hàng đợi được đọc ở đó.
- **`ha exec` thoát**: một job khi các lần chạy của hội thoại đã làm đủ; rồi các
  job review đến hạn của hội thoại chạy trước khi process thoát. Một lần chạy
  không tự đến hạn review vẫn chạy job mà một lần chạy trước chưa làm xong.
- **Deep Dream** (memory toàn cục quá 12 entry và 15 thay đổi, hoặc store của
  một hội thoại quá 40 entry): chỉ được đánh dấu xong khi đã áp dụng; mỗi lúc
  một lần hợp nhất trên mọi process của ha (`consolidation.lock` trong thư mục
  harness toàn cục, một khoá của hệ điều hành mà process giữ khi chạy).
  Từ 0.9.0 memory vượt 32.000 token entry được hợp nhất theo từng lô, mỗi lô
  một lần gọi curator, các entry liên quan trong một lô đứng cạnh nhau; mỗi lô
  được áp dụng và ghi lại (`<id>_b<n>`) ngay khi lập xong. Một lô thất bại là
  thất bại của job: hàng đợi thử lại cả lượt, cắt lô lại từ đầu, và các lô đã
  áp dụng vẫn được giữ. Chỗ lô đầu tiên kết thúc dời đi giữa các lượt, nên hai
  entry bị một ranh giới tách ra sẽ gặp nhau ở lượt sau
  ([MEMORY_STORE.vi.md](MEMORY_STORE.vi.md), mục 16). Mỗi lô là một bản ghi
  `refine.consolidation_batch` (phạm vi, refinement, vị trí, số entry và số
  sửa đổi đã áp dụng).
- **Tiến hoá** (một lần mỗi ngày): chỉ được đánh dấu đã chạy khi đã lưu xong;
  khoá của nó và việc nhận job ngăn lượt thứ hai bắt đầu trong lúc đó.

Lượt meditation của vòng học vốn đã tự giữ chỗ một giờ và chỉ đánh dấu sau khi
thành công; nó không nằm trong hàng đợi.

## Những gì cho thấy nó

- **Diagnostics**: bản ghi `sleep` với `enqueued` (kèm `coalesced`),
  `claimed`, `done` (kèm `superseded` khi một lần nhận khác đã lấy job), `failed`, `gave_up` và `forgotten`, mỗi bản ghi có loại job, key, điểm
  kích hoạt, số lần thử, số lượt và span, và với lần hỏng thì loại lỗi cùng thời
  điểm thử lại. Bản ghi `refine.call_failed` có thêm `class` của lỗi.
- **`ha doctor`** (State): bao nhiêu lượt đang xếp hàng, lượt nào hỏng và chờ
  thử lại, lượt nào đã bỏ cuộc, kèm file cần xoá để cho nó chạy lại. Hàng đợi
  trống thì không nói gì.
- **Bug report**: bảng chi phí bảo trì đếm cả các lần gọi hỏng bên cạnh các lần
  có trả lời (`| upkeep call | calls | failed | ... |`, lỗi theo loại), mục
  *Sleep* liệt kê các job trên đĩa và các lần hỏng trong khung thời gian, và
  file zip có `sleep/*.json`.

## Biến môi trường cho test

`HA_SLEEP_RETRY_SECONDS` đặt thời gian chờ đầu tiên sau một lần hỏng (gấp đôi
như trên), `HA_SLEEP_MAX_ATTEMPTS` số lần hỏng trước khi job bỏ cuộc, và
`HA_COMPACTION_REVIEW_SECONDS` khoảng cách giữa các review của compact, và
`HA_CONSOLIDATE_BATCH_TOKENS` số token entry mà một lần gọi Deep Dream được
đưa (32.000; memory vượt mức này được hợp nhất theo từng lô). Chúng
dành cho live test (`scripts/testing/live-tests/sleep.sh`); giá trị mặc định
hợp với dùng hằng ngày.
