# Hoàn thiện CLI hiện tại sau review

Ngày: 19/09/2026. Baseline: `9bbd632`; có các thay đổi HA_LAUNCH của DeepSeek trong working tree, giữ nguyên. Assignment: đọc code hiện tại, sửa các lỗi tích hợp CLI.

## Phạm vi và hợp đồng

Một root workspace, binary `ha`, dùng AgentSessionService/TurnDriver/store hiện tại. Không thêm dependency, đổi database schema, cài lên user profile, gọi paid API hoặc publish release trong lát cắt này.

1. `--resume <id>` phải chọn nguồn hội thoại thật, không chỉ in thông báo placeholder. Session chỉ được tiếp tục khi nằm trong project store; dùng đúng task của source. `/new` tạo task mới. Chọn nguồn đồng bộ trước submit; xác minh persisted source trước provider dispatch, không có race giữa resume và submit.
2. `/resume` khi đang chạy không đổi nguồn; selector 0 không chọn nhầm session đầu. ID được hiển thị phải dùng lại được. `/exit`, `/quit` và Ctrl-D phải hủy run đang chạy, kể cả chờ approval; không chuyển thành approval grant hoặc user prompt.
3. Vẽ lại prompt do gõ/backspace/resize không thêm newline cho từng phím. Output đến phải tách khỏi prompt và giữ buffer đang gõ. Không ghi response dính vào prompt.
4. Line-mode fallback phải bơm events khi chưa có dòng input mới; EOF đi cùng đường exit/cancel. Mode fallback không được chờ người dùng Enter mới hiện kết quả.

## Failure model và kiểm chứng

- Sai context/task hoặc đổi session giữa run: source-selection unit tests, real SQLite continuation tests, invalid/cross-project source không dispatch provider.
- Approval bị hiểu sai thành exit/input: controller tests ở Running/WaitingApproval/idle, giữ grant/deny tests hiện có.
- UI lỗi dù test substring pass: kiểm tra write boundaries/newlines bằng scripted backend và chạy actual PTY regression khi console khả dụng.
- Fallback không stream: async event producer với input channel giữ mở; bounded timeout phải thấy output trước dòng input tiếp theo.
- Regression: baseline `cargo test -p harness-cli --bin ha --locked` có 61 pass; sau sửa chạy fmt/clippy, CLI/runtime tests, HA_LAUNCH gate và docs checker.

## Cách thực hiện và giới hạn bằng chứng

Dùng skill old-coder theo chế độ tự thực hiện đã được giao. `spec approval: not obtained (autonomous run)`; SPEC để người dùng/DeepSeek review sau, không coi yêu cầu ban đầu là phê duyệt từng assertion. RED trước implementation; giữ các assertion cũ trừ hành vi phải sửa được ghi rõ ở đây. Không thêm dependency/tool cài mới. Coverage/mutation/PTY chỉ báo số thực chạy; layer thiếu công cụ/môi trường phải ghi rõ trong evidence. Không claim live provider/Linux/VM sạch từ HTTP fixtures hoặc Windows tests.

Source owner: `crates/harness-cli/src/interactive/{app,controller,service}.rs`; helpers input/terminal khi cần. Evidence riêng để không ghi đè handoff/evidence H đang được DeepSeek sửa.
