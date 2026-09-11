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
- **Cao — SSE:** parser chỉ lấy tool delta đầu tiên, không giữ ID theo index
  khi các chunk sau thiếu ID. Cần fixture nhiều tool và nhiều chunk.
- **Cao — compaction:** lấy sequence CAS sau khi dựng candidate, nên có thể
  nhận candidate cũ khi tail đã thay đổi; cần CAS theo source và rebase có giới hạn.
- **Vừa — hủy process đang đợi:** đợi mutex rồi spawn trước khi chọn nhánh
  cancellation, nên tác vụ đã hủy vẫn có thể khởi chạy trong thời gian ngắn.

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
