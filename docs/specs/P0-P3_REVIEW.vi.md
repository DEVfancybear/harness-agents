# Đặc tả rà soát và sửa lỗi P0–P3

[English](P0-P3_REVIEW.en.md) | Tiếng Việt

Ngày: 2026-09-11. Đối chiếu source P0–P3 với SPEC/runbook, sửa lỗi có ca tái
hiện; chưa triển khai P4. Chưa lấy phê duyệt SPEC riêng: thực hiện tự động theo
yêu cầu review và cập nhật của người dùng.

## Tiêu chí

1. ID P0 từ chối UUID sai variant RFC như schema hiện có quy định.
2. Ngân sách context P2 tính cả nhãn và dấu phân cách; nội dung bắt buộc quá
   giới hạn phải báo lỗi, phần tùy chọn không được làm packet vượt ngân sách.
3. Parser tool P3 từ chối trường lạ và sai kiểu path/isolation, không âm thầm
   mở rộng phạm vi hoặc hạ mức isolation.
4. Policy chuẩn hóa thành phần đường dẫn và chữ hoa/thường trên Windows;
   đọc đệ quy thư mục tổ tiên không vượt quyền deny. `.` và `./` dùng được cho
   thao tác thư mục gốc workspace.
5. Đọc giới hạn dung lượng vẫn từ chối UTF-8 sai; chỉ bỏ ký tự cuối chưa đủ
   byte do cắt output.
6. Search che dữ liệu nhạy cảm trên toàn dòng trước khi rút gọn preview.

Mỗi lỗi phải có test RED trước sửa, GREEN sau sửa; dùng file/SQLite tạm thật.
Giữ schema và toàn bộ regression P0–P3, không thêm dependency/migration,
không gọi provider thật, không commit/push/deploy.

## Kiểm chứng

Chạy test mục tiêu, gate P3 và docs self-test. Evidence riêng ghi kết quả,
giới hạn coverage, kiểm tra độc lập, nền tảng chưa chạy và phát hiện còn lại.
Evidence phase cũ là lịch sử, không đại diện cho source đã sửa.

## Bổ sung 2 — theo dõi lời gọi P1

Từ chối ID lời gọi service đang hoạt động bị trùng trước khi tăng bộ đếm.
Nếu không, kết thúc một lời gọi có thể xóa lời gọi còn lại khỏi tập kết quả
chưa rõ. Cho phép dùng lại ID sau khi kết thúc; kiểm tra drain/join thật.
