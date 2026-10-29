# Đọc bộ nhớ theo câu hỏi: MEMORILLA trong ha

[English](MEMORY_READOUT.en.md) | Tiếng Việt

**Trạng thái:** có từ 0.8.3; từ 0.8.8 chỉ đọc theo từ (embedding đã bị bỏ, xem
[Kho memory §14](MEMORY_STORE.vi.md)). Tài liệu này mô tả cách chuyển thể MEMORILLA
(Fallahpour, Idehpour, Kothapalli và cộng sự, "Latent semantic memory for LLMs",
arXiv 2610.21375, Stanford) cho ha. Code là chuẩn.

## 1. Bài báo làm gì

MEMORILLA cho một LLM đóng băng truy cập cả một bộ tài liệu, thông qua một số
cố định "memory token" phụ thuộc vào câu hỏi:

1. **Mã hoá một lần.** Một encoder đóng băng (Qwen3-Embedding-4B) nhúng mỗi tài
   liệu (mean-pool) vào một kho lưu bền. Chỉ tài liệu mới hoặc đã đổi mới phải
   nhúng lại.
2. **Truy xuất theo câu hỏi.** Với mỗi câu hỏi, `K` ô (`K = 16`) được khởi tạo
   từ các vector đã học, rồi được điều kiện hoá theo embedding của câu hỏi bằng
   cross-attention. Sau đó các ô chú ý tới toàn bộ embedding trong kho qua
   `L_ref = 2` khối cross-attention, kiểu Perceiver. Đây là một lần đọc liên
   tục trên cả bộ tài liệu, không phải chọn top-k rời rạc.
3. **Chiếu.** Các ô đã tinh chỉnh được chiếu vào không gian embedding đầu vào
   của LLM, thành `K` soft token đặt trước prompt.
4. **Huấn luyện.** Chỉ module bộ nhớ (194 triệu tham số) được huấn luyện: trước
   là tái tạo câu diễn đạt lại, rồi QA và tóm tắt, rồi các benchmark. Với
   agent (TextWorld), module được huấn luyện bằng GRPO trên phần thưởng của môi
   trường, và đọc các lượt cũ của agent như tài liệu.

Kết quả bài báo nêu:

- trung bình tốt nhất trên các bài cá nhân hoá, tài liệu dài và hỏi đáp tri
  thức, chỉ với 16 token thay cho tới 50.000;
- nhớ lại được bối cảnh cá nhân không chung từ nào với câu hỏi, ví dụ bệnh
  thiếu máu của người dùng khi họ hỏi một câu chung chung;
- lợi thế càng lớn khi câu hỏi phải kết hợp nhiều thực thể hoặc nhiều sự kiện;
- truy xuất thường và đọc tiềm ẩn trả lời những câu hỏi *khác nhau*, nên hợp
  của hai cách vượt cả hai, và đó là lý do đề xuất kết hợp;
- agent ít đi vòng giữa những phòng đã khám phá.

## 2. Phần không chuyển sang được

ha gọi model qua API, mà API chỉ nhận văn bản chứ không nhận embedding đầu vào;
máy chạy ha cũng có thể không có GPU. Soft memory token chỉ đưa được vào một
decoder mã nguồn mở chạy cục bộ: checkpoint đã công bố
`memorilla/Memorilla-Qwen3-8B` cùng Qwen3-8B chạy sau prompt embeddings của
vLLM, cần khoảng 24 GB VRAM. Maintainer chọn chuyển thể phương pháp cho ha thay
vì làm backend đó. Cũng không huấn luyện gì: bài báo cho thấy module chưa huấn
luyện làm agent kém đi, nên ha lấy cấu trúc của phương pháp, không lấy trọng
số.

## 3. Phần ha lấy

`harness_types::readout` giữ những phần không cần huấn luyện hay soft token:

| MEMORILLA | ha |
|---|---|
| Encoder đóng băng, kho tính một lần | Vector TF-IDF của từ, tính mỗi lần đọc: trọng số lấy từ kho, không có danh sách từ. Đến 0.8.7 thì dùng embedding người dùng đã cấu hình, nếu có. |
| `K` ô điều kiện theo câu hỏi | Tối đa `K` ô (1 tới 16 cho lời gọi tool, mặc định 8), mỗi ô điều kiện theo câu hỏi. |
| Ô có danh tính riêng (`Q0` đã học) | Mỗi ô đọc phần của câu hỏi mà các ô trước còn để lại: phần dư của câu hỏi sau khi trừ các lần đọc trước (Gram-Schmidt). Câu hỏi về hai việc sẽ chia vào hai ô. |
| Cross-attention trên mọi tài liệu, `L_ref = 2` | Attention softmax trên mọi vector theo cosine (nhiệt độ 0,05), hai vòng, dùng câu hỏi làm kết nối dư. |
| `K` token bất kể kho lớn cỡ nào | Tối đa `K` ô, mỗi ô thể hiện bằng các tài liệu mang 80 % attention của nó (tối đa 3), mỗi tài liệu chỉ thuộc một ô. Tài liệu chỉ được hiện khi độ tương đồng cao hơn trung bình của kho một độ lệch chuẩn: đây là thống kê của kho, nên đúng với mọi thang đo của encoder. |
| Kết hợp với truy xuất (việc tương lai) | Đến 0.8.7 digest kết hợp phép đọc theo nghĩa với xếp hạng theo từ; từ 0.8.8 digest xếp theo từ, độ mới và mức dùng. |

## 4. Chạy ở đâu

- **Digest của mỗi lượt.** Không còn nữa: đến 0.8.7, khi đã cấu hình
  embedding, digest kết hợp phép đọc theo nghĩa với xếp hạng theo từ và đánh dấu
  entry như vậy là `[recalled by meaning]`. 0.8.8 bỏ phần đó cùng embedding.
- **Tool `memory`, `search` với `mode: readout`.** Cùng cách đọc đó, trên các
  entry mà bộ lọc (`tree`, `meta`, `meta_predicate`, `temporal`, `grep`) giữ
  lại, chia vào `slots` ô (mặc định 8).
  - Mỗi ô liệt kê entry của nó kèm `attention`.
  - Kết quả ghi `encoder: words`; khi không nhớ lại được gì, nó ghi câu hỏi đã
    đọc và khuyên tìm lại bằng từ khác.
  - Các entry nó hiện được đếm là hit (`memory_usage`).
  - Chế độ này chỉ đọc memory; knowledge tìm bằng `mode: rank`.
- **`history_search` với `mode: readout`.** Đây là thiết lập học liên tục của
  bài báo: các lượt cũ của cuộc trò chuyện là tài liệu, gồm tin nhắn của người
  dùng, của trợ lý và kết quả công cụ.
  - Đọc 1.500 nguồn mới nhất trong phạm vi, mỗi nguồn 1.500 ký tự đầu, với cùng
    bộ lọc khoảng thời gian và loại nguồn.
  - Vector là vector TF-IDF của từ trong mỗi nguồn.
  - Mỗi hit ghi `slot` và `attention` của nó, nên những gì đã thử, đã quyết
    hoặc đã thấy trong một cuộc trò chuyện dài trở lại cùng nhau, kể cả các
    lượt đã bị compaction.

## 5. Không làm

- **Soft memory token, module đã huấn luyện, GRPO.** Không làm được qua API văn
  bản (xem §2).
- **Tự chèn các lượt cũ ở mọi bước.** Bộ nhớ agent của bài báo bổ sung cho lịch
  sử đầy đủ mà model vẫn thấy. Context của ha vốn đã mang lịch sử cho tới lúc
  compaction. Sau compaction, model tự lấy phần còn lại bằng `history_search`
  (`mode: readout`), và bản tóm tắt compaction có nhắc tới tool đó.

## 6. Bằng chứng

- **Unit test** bao quát:
  - chính phép đọc: các ô chia câu hỏi hai vế, ngân sách cố định trên 500 tài
    liệu, thống kê "nổi bật";
  - readout của tool: yêu cầu hai vế tìm ra cả hai entry và không lấy entry
    không liên quan;
  - readout của lịch sử: các nguồn ứng với từng vế của câu hỏi trở lại trong
    các ô, nguồn không liên quan thì không.
- **Live**, `memory-store.sh` mục 12 chạy với một server embedding giả đến
  0.8.7; 0.8.8 giữ các check theo từ: readout chia một yêu cầu hai vế.
