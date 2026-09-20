# Kết quả review P0–P3

[English](P0-P3_REVIEW.en.md) | Tiếng Việt

Ngày: 2026-09-11. Theo [SPEC review](../specs/P0-P3_REVIEW.vi.md).
Thực hiện tự động, chưa có phê duyệt SPEC riêng hoặc kiểm tra bởi agent độc lập.

## Đã sửa

1. P0: UUID sai variant RFC vẫn được parser chấp nhận dù schema từ chối.
2. P1: ID lời gọi service trùng làm sai tập công việc chưa rõ kết quả.
3. P2: ngân sách context bỏ sót nhãn và dấu phân cách của packet.
4. P3: parser bỏ qua trường lạ, path sai kiểu mở rộng thành toàn workspace,
   isolation sai kiểu bị hạ xuống best effort.
5. P3: policy bị vượt bằng đường dẫn tương đương hoặc đọc thư mục tổ tiên.
6. P3: `.` và `./` bị từ chối khi liệt kê workspace gốc.
7. P3: bộ đọc giới hạn cắt lùi byte để che mất UTF-8 sai ở giữa nội dung.
8. P3: search cắt preview trước khi che dữ liệu nhạy cảm.

Thêm 10 test; 8 test đã quan sát RED trước sửa. Hai test bảo vệ hành vi có sẵn
(từ chối UTF-8 ở gate và cắt đúng ký tự Unicode) đã xanh từ đầu. Mapping tên
test cụ thể nằm trong bản English. Review có đối chiếu SPEC/runbook và các
đường thực thi chính, không phải chứng nhận mọi dòng source hoặc mọi race.

## Kiểm chứng

Lệnh chạy lại:

```powershell
pwsh -NoProfile -File scripts/Verify-P0P3Review.ps1
```

Kết quả cuối: `P0_P3_REVIEW_OK`.

- Format, clippy không warning và toàn bộ workspace tests: PASS, gồm 10 test mới.
- Ca bắt buộc: P0 8/8, P1 18/18, P2 15/15, P3 15/15 — tổng 56 ca PASS.
- P3 phát hiện 20 test; toàn bộ được chạy trong workspace tests.
- 3/3 mutation bị bắt: UUID variant, context overflow và thứ tự redaction;
  source đã khôi phục. Script từ chối mutation trực tiếp ở workspace (exit 1).
- Docs self-test: PASS, 12 negative control, 61 file Markdown và 15 cặp ngôn ngữ.
- CLI smoke: PASS, 9 schema tool; Windows Job Object, không có strict sandbox.
- Đối chiếu file review với bản sao đã test: PASS trước khi chốt metadata evidence.

Chạy trên Windows, Rust/Cargo 1.97.1, PowerShell 7.6.5. Sau lúc bắt đầu với
checkout sạch, xuất hiện công việc P4 song song. Script xuất baseline
`c9bb106cce67c5f0b7e3c02fafe1484e5f69d379`, chép đúng file review vào thư mục
tạm rồi kiểm chứng; không sửa hoặc tuyên bố đã test tích hợp phần P4 đó.

Digest gate của bản sao đã test:
`sha256:b2245719b7c2b0333e3fa3a958dc72c2a69bca5503ef11b83e8802c117494f09`
(129 file, bao gồm evidence nháp). Chốt metadata báo cáo làm đổi digest tổng
nhưng không đổi Rust source đã test. Hash tổng 10 file Rust/test đã sửa là
`27e974e726815d29b6f00b113572167eba1cd2befb060895999a8a2244adf0ac`;
cách tính và phạm vi có trong bản English và script. Sau cập nhật metadata,
chạy thêm docs self-test cho báo cáo cuối.

## Vấn đề còn lại cần ưu tiên

- **Cao — approval:** chưa gắn invocation/session/task vào binding; grant có
  thể khớp một proposal khác nếu actor/action/workspace giống nhau và chưa dùng.
- **Cao — compaction:** lấy sequence CAS sau khi dựng candidate, nên có thể
  nhận candidate cũ khi tail đã thay đổi; cần CAS theo source và rebase có giới hạn.
- **Vừa — hủy process đang đợi:** đợi mutex rồi spawn trước khi chọn nhánh
  cancellation, nên tác vụ đã hủy vẫn có thể khởi chạy trong thời gian ngắn.

### Sửa sau đó: danh tính tool call trong stream (finding 2 của review này)

Finding thứ hai ở trên — `parse_sse_payload` chỉ lấy tool delta đầu tiên và rơi về
`tool-call` khi frame thiếu ID — đã được sửa trong `crates/harness-providers/src/lib.rs`.
Decoder giờ nhớ call id đã announce cho từng `index` trong stream và đóng dấu id đó lên
mọi fragment sau, phát **mọi** fragment mà một frame mang, giữ cả prose lẫn fragment
trong cùng frame; call không bao giờ có tên được báo là incomplete. Năm test hồi quy:
`sse_fragments_of_one_call_keep_one_identity` và
`sse_parallel_calls_in_one_frame_stay_separate` đã được đo **RED** với decoder cũ trước
khi sửa (chúng tái hiện đúng ca đã đo: một call có tên nhưng rỗng arguments và một call
ẩn danh giữ arguments), còn
`sse_prose_and_a_fragment_in_one_frame_are_both_kept`,
`sse_fragments_without_an_index_continue_the_announced_call` và
`sse_an_anonymous_fragment_is_reported_as_incomplete` là control cho hành vi mà bản sửa
không được đổi.

Đo trên cây hiện tại: `cargo test -p harness-providers --locked` pass 13 test,
`cargo test -p harness-cli --test phase_p2 --locked` pass 17, và
`cargo test -p harness-cli --test interactive_session --locked` pass 10 (trong đó
`g2_a_malformed_streamed_call_is_refused_with_its_own_reason` phủ phía driver từ chối).
Bản sửa này **không** được coi là đã kiểm chứng bằng `scripts/Verify-P0P3Review.ps1`:
entry point đó chép file review hiện tại đè lên baseline đóng băng
`c9bb106cce67c5f0b7e3c02fafe1484e5f69d379`, và nó không còn build được ở đó — đo được
`error[E0004]: non-exhaustive patterns: &CodingToolAction::ExternalTool { .. } not covered`
vì `crates/harness-tools/src/contracts.rs` (thuộc danh sách owned) đã đi trước
`crates/harness-tools/src/service.rs` của baseline. Entry point review cần baseline mới
(hoặc danh sách owned rộng hơn) trước khi chạy lại được.

Các mục này chưa sửa trong bộ thay đổi này. Vì vậy chưa thể kết luận toàn bộ
P0–P3 đáp ứng đầy đủ hợp đồng. Runbook vẫn ghi `not started` vì là bản kế hoạch
lịch sử; evidence phase và báo cáo review mới phản ánh kết quả triển khai.

## Giới hạn

Ngân sách context vẫn dùng ước lượng byte, không bảo đảm tokenizer cho toàn
request/tool schema. Policy không phải sandbox, redaction theo từ khóa không
phát hiện mọi bí mật. Chưa chứng minh mọi race file/kernel hoặc provider thật.
Không thêm dependency/migration, không dùng credential thật, không deploy,
commit/push. Chưa đo coverage dòng thay đổi hoặc chạy kiểm tra độc lập.
Linux/CI cho bản source này chưa được xác minh.
