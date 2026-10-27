# Đổi chiến lược và runtime harness

Phần ha lấy từ hai bài báo: nó hoạt động thế nào, và phần nào không lấy. Kế
hoạch và các quyết định của chủ dự án nằm trong [plan](STRATEGY_AND_RUNTIME_PLAN.vi.md).

- **[A] arXiv 2608.19072**, *What Is Missing from AI Post-Training AI*: agent
  thực thi tốt nhưng khoá cứng vào chiến lược ban đầu. Chỉ 2,1% số lần thử
  liền nhau có đổi chiến lược. Nhật ký và thêm suy luận đều không làm nó đổi,
  "cân nhắc lại" mà không nêu phương án cũng không. Một lời bảo đổi giữa chừng
  có nêu tên chiến lược khác thì thắng phần agent tự chạy tiếp tới 17 điểm.
- **[B] arXiv 2605.22166**, *LIFE-HARNESS*: cải thiện model bằng cách sửa
  runtime quanh nó, không sửa model. Có bốn lớp (contract, skill, realization,
  regulation), được tiến hoá từ các trajectory hỏng, và mỗi thay đổi chỉ được
  giữ khi đo được là có ích.

## Các lớp runtime ([B])

| Lớp | Ở đâu | Làm gì |
|---|---|---|
| Theo dõi trajectory | `harness-tools/src/runtime/regulation.rs`, gọi sau mỗi lần gọi tool trong `turn_driver.rs` | Một tool hỏng cùng một lỗi 3 lần, dù tham số khác nhau, thì có ghi chú ngay dưới lần gọi đó. 5 lần thì ghi chú bảo dừng. Lệnh trả exit code khác 0 cũng tính là hỏng. Hai lần sửa file xen kẽ, cái sau hoàn tác cái trước (A, B, A, B), cũng được chỉ ra. Khi turn sắp hết step hoặc lượt gọi tool thì được báo một lần. |
| Kiểm lệnh trước khi chạy | cùng chỗ, trước khi chạy lệnh | Lệnh y hệt lệnh ngay trước nó vừa hỏng thì không chạy, nếu giữa hai lần không có lệnh nào khác chạy và người dùng không nói gì. Chỉ áp dụng cho tool file, và chỉ cho lỗi do chính lệnh: approval, workspace cũ hay provider bận không bao giờ là lý do để chặn. Luật học được (`rules.rs`) đổi tên tham số mà tool không nhận, hoặc từ chối một hình dạng tham số chưa từng chạy được. |
| Contract của tool | `evolution.rs`, hàm `annotate` | Ghi chú học từ các lần hỏng được thêm vào mô tả tool. |
| Skill | `memory/skill_recall.rs` | Request (cùng goal) được chấm BM25 với từng learned skill. Chỉ skill top-1 được đưa vào turn, và chỉ khi điểm nổi bật hẳn so với phần còn lại. Kèm theo là các giả định đã sai trong những lần skill đó thất bại trước. |

Mỗi lần gọi tool xong để lại một record `call settled` gồm hash và hình dạng
tham số (tên và kiểu, không bao giờ có giá trị) cùng loại lỗi: mã lỗi, và câu
lỗi đã bỏ số, đường dẫn, phần trong ngoặc kép và tên file. Lần gọi bị harness
hay người dùng từ chối được đánh dấu `refused`. Bug report có mục *Strategy and
regulation* đếm tất cả những thứ này.

### Tiến hoá tự động

`memory/evolution.rs` chạy tối đa một lần mỗi ngày, sau một turn. Nó giữ khoá
để mỗi lúc chỉ một process chạy, và đánh dấu lần chạy ngay khi bắt đầu.

1. Chấm các luật theo những gì chúng đã làm. Luật đang chạy bóng được bật khi
   lẽ ra đã tác động đủ 3 lần, mà cả 3 lần lời gọi đều hỏng. Nó bị bỏ ngay lần
   đầu lẽ ra tác động lên một lời gọi đã chạy được. Luật đổi tên đang bật bị bỏ
   nếu các lời gọi nó đổi tên vẫn tiếp tục hỏng.
2. Khai thác các nhóm hỏng của một tool với cùng một loại lỗi, từ 3 lần trở
   lên, mà chưa từng đưa ra trước đó (hoặc đã tăng gấp đôi kể từ lần trước).
   Đưa cho model kèm các hình dạng tham số đã hỏng và đã chạy được.
3. Model phân loại mỗi nhóm theo thứ tự ưu tiên của [B]: realization,
   contract, degeneration, reasoning. Chỉ nhóm realization và contract mới
   được đề xuất thay đổi: một luật trong bộ từ vựng, hoặc một ghi chú.
4. Luật phải replay khớp với ít nhất 3 lần hỏng đã ghi và không khớp lần thành
   công nào, rồi mới bắt đầu ở chế độ chạy bóng.

Thứ ha dùng thay cho điểm benchmark của [B] là chính các record của nó: replay
trước, rồi quan sát ở chế độ chạy bóng. Luật và ghi chú áp dụng cho turn gốc,
cho các child được delegate và cho `ha exec`. Xoá file `harness-evolution.json`
là quên hết.

## Đổi chiến lược ([A])

- **`task_state`** ghi tên chiến lược (cùng các phương án đã cân nhắc), nhật ký
  theo kiểu của bài báo (plan, observation, lesson, eval, eval analysis) và các
  quyết định.
- **Mỗi lần `goal_complete` bị từ chối là một lần đánh giá.** Điểm là công việc
  đã đi tới đâu: checklist, rồi check, rồi verifier. Nếu bằng nhau thì xét còn
  lại bao nhiêu ở bước đó, rồi đến lỗi in ra bao nhiêu dòng. Nhờ vậy test giảm
  từ 40 lỗi xuống 2 được tính là có tiến triển. Task mà model chưa từng dùng
  `task_state` vẫn được đánh giá.
- **Sau mỗi lần đánh giá là tới lúc quyết định.** Lời từ chối và task state ở
  mọi turn đều nói điều đó. Đây chỉ là lời khuyên, không chặn (theo quyết định
  của chủ dự án): model tự cân nhắc bằng chứng rồi ghi `decide` continue hoặc
  switch.
- **Plateau.** Ba lần đánh giá dưới cùng một chiến lược mà không tiến thêm là
  plateau. Khi đó một child explorer với context mới (strategist) được hỏi một
  lần cho mỗi plateau, để nêu tên 2–3 chiến lược *khác*. Lời từ chối và task
  state mang các phương án đó. Nếu quyết định tiếp tục, phải qua thêm đủ một
  plateau đánh giá nữa thì strategist mới được hỏi lại.
- **`compare_children`** chạy check của project ngay trong worktree của từng
  coder, để chọn được bản tốt hơn khi hai chiến lược được thử song song, trước
  khi integrate.
- **`/strategy` cho xem tất cả**, và TUI báo khi có plateau. Không có bước nào
  bắt người dùng phải chọn.
- **Review đọc bản ghi chiến lược** cùng cuộc trò chuyện, để chiến lược đã
  giậm chân trở thành bài học cho task sau.

## Phần không lấy

- **Tín hiệu huấn luyện từ các nhánh fork của [A]:** ha không train model.
- **Code harness tự do của [B]:** luật là dữ liệu trong một bộ từ vựng đóng.
- **Chấm nhận bằng điểm benchmark của [B]:** ha không có benchmark. Replay và
  chạy bóng thay cho nó.
- **Chưa có ở bản này:** lớp chiến lược chỉ chạy theo `/goal`. `/verify` giữa
  chừng, check của feature và các ghi chú lỗi lặp lại của regulator chưa được
  tính là lần đánh giá chiến lược. `switch` chưa tự fork một nhánh mới, và
  verifier chưa trỏ được vào worktree của coder.
