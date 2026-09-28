# Prompt giao DeepSeek: track Q01–Q15 (đóng khoảng cách với prime-agent)

[Plan chi tiết Q01–Q15](HA_PRIME_PLAN.vi.md) · [Track G01–G14](HA_AGENT_PLAN.vi.md) · [Templates SPEC/evidence/handoff](implementation-next/TEMPLATES.vi.md)

Mỗi prompt là một assignment độc lập, dừng ở checkpoint ghi trong plan (mục 12). Prompt **không tự cấp quyền** commit/push/API trả phí/cài lên máy; muốn cấp thì thêm một dòng ghi rõ vào assignment. Trước mỗi assignment DeepSeek đọc lại `git status`/HEAD, `docs/handoffs/HA_PRIME.vi.md` (nếu đã có) và chỉ làm phần **chưa xong thật**.

## 1. CP-1 — Q00–Q04: agent con chạy nền

```text
Triển khai Q00–Q04 trong docs/HA_PRIME_PLAN.vi.md. Chưa làm Q05 trở đi.

Đọc TRƯỚC khi code, theo thứ tự:
1. docs/HA_PRIME_PLAN.vi.md mục 3 (cách làm việc bắt buộc) — đọc hết, làm đúng vòng lặp
   RED → GREEN cho từng bước.
2. Mục 2 (căn cứ source) và mục 4 (quyết định D1–D6), rồi Q00–Q04.
3. Các file ha plan dẫn: interactive/service.rs (AgentSessionService, SessionPort, submit,
   run_turn), interactive/delegation.rs, interactive/controller.rs (pump_events,
   RunTerminal handler, dispatch), interactive/events.rs, crates/harness-cli/tests/
   interactive_terminal.rs. Mở đúng hàm, đừng chỉ tin số dòng.
4. Tham chiếu prime ở mục 2.2 cho spawn, thông báo, agent_message.

Làm tuần tự Q00 → Q01 → Q02 → Q03 → Q04, từng bước đánh số trong plan. Mỗi bước:
viết test tên đúng như plan, chạy thấy ĐỎ đúng lý do, code tối thiểu, chạy XANH, rồi
chạy cargo test -p harness-cli --bin ha --locked. Ghi cả lần đỏ và lần xanh vào evidence.

Q01 bước 3 (đổi run_turn sang StoreLease) là bước rủi ro nhất: sau bước đó chạy toàn bộ
unit test harness-cli và PTY i05 + i13 bằng scripts/Invoke-HaPtyAcceptance.ps1 -Filter;
không đi tiếp khi chưa xanh.

Luật: port theo prime, không heuristic dò từ khoá; không crate mới; store chỉ một
writer (dùng SharedStore D1, không mở writer thứ hai); Ctrl+C không huỷ agent con;
chuỗi thông báo lấy nguyên văn D4; không xoá/nới test cũ (nếu phải đổi kỳ vọng, ghi
lý do vào SPEC). Test chập chờn đã biết ở mục 3.3: chạy riêng và ghi lại, không sửa.

Tạo docs/specs/HA_PRIME.vi.md (Q00 bước 1) TRƯỚC khi code Q01. Cuối assignment: cargo
fmt, clippy -D warnings, cargo test --workspace --locked --no-fail-fast, PTY đầy đủ
bằng scripts/Invoke-HaPtyAcceptance.ps1 -TimeoutSeconds 900 (PTY_EXIT: 0), Verify-Docs.
Cập nhật OPERATOR_GUIDE .en và .vi cho hành vi agent con mới. Tạo
docs/evidence/HA_PRIME.vi.md và docs/handoffs/HA_PRIME.vi.md (exact next action).
Dừng ở CP-1. Không commit/push/API trả phí/cài lên máy nếu không được cấp.
```

## 2. CP-2 — Q05–Q09: hàng đợi, `/btw`, fork/tree, HTML

```text
Tiếp tục Q05–Q09 theo docs/HA_PRIME_PLAN.vi.md và docs/handoffs/HA_PRIME.vi.md.
Trước khi sửa: xác nhận CP-1 thật sự xanh (chạy lại các test q01_–q04_ và ghi kết quả).

Đọc lại mục 3 của plan. Làm tuần tự Q05 → Q06 → Q07 → Q08 → Q09 theo từng bước.
Q06 bước 1 và Q11 cần ĐO phím qua PTY trước khi hứa phím nào; phím không tới được thì
làm lệnh thay thế plan ghi. Q08 bắt đầu bằng bước 0 (đo nguồn khác task) và ghi kết
quả + phương án chọn (A hoặc B) vào SPEC trước khi code bước 1.
Alt+Enter vẫn là xuống dòng (D7) — follow-up nhập bằng /queue.
/btw không được ghi gì vào store hay lịch sử phiên (test q07_side_question_is_not_recorded).
HTML export không tải gì từ mạng và phải escape mọi chữ của người dùng/model.

Cuối assignment chạy đủ gate như CP-1, cập nhật evidence/handoff/OPERATOR_GUIDE.
Dừng ở CP-2. Không commit/push/API trả phí/cài lên máy nếu không được cấp.
```

## 3. CP-3 — Q10–Q13: models.json, scoped, định tuyến, chờ quota

```text
Tiếp tục Q10–Q13 theo docs/HA_PRIME_PLAN.vi.md và docs/handoffs/HA_PRIME.vi.md.
Xác nhận CP-2 xanh trước khi sửa.

Q13 bước 1 thêm ErrorCode::RateLimited: sinh schema bằng
cargo run -p harness-types --bin generate_schemas --locked (không sửa JSON bằng tay),
p0_f03 phải xanh. 429 → RateLimited dựa trên mã HTTP và mã lỗi JSON có cấu trúc,
không dò câu chữ tự do. Test chờ quota không được ngủ thật nhiều giây: đưa hằng số
chờ vào RuntimeConfig và dùng giá trị nhỏ trong test.
Q12: model phụ/dự phòng/ảnh chỉ dùng khi có trong catalog + có credential; không dùng
được thì notice đúng chuỗi plan và quay về model phiên; không bao giờ gửi ảnh vào
model không nhận ảnh.

Cuối assignment chạy đủ gate, negative control của CP-3 (plan mục 9), cập nhật
evidence/handoff/OPERATOR_GUIDE. Dừng ở CP-3. Không commit/push/API trả phí/cài lên
máy nếu không được cấp.
```

## 4. CP-4 — Q14–Q15: `/autonomous` và lịch chạy bền

```text
Tiếp tục Q14–Q15 theo docs/HA_PRIME_PLAN.vi.md và docs/handoffs/HA_PRIME.vi.md.
Xác nhận CP-3 xanh trước khi sửa.

Q14: viết hàm thuần decide() và test bảng TRƯỚC (q14_decide_follows_prime_order, mỗi
nhánh một case), rồi mới nối vào controller. Prompt tiếp tục/gate-failed lấy nguyên
văn prime (autonomous.ts). Gate chạy như !!cmd, có timeout, output cắt 6000 ký tự.
/autonomous và /goal không bật cùng lúc.
Q15: test parser bằng bảng trước; lưu file nguyên tử (ghi tạm rồi rename); lỡ nhiều lần
chỉ chạy bù một lần; next_run tính từ thời điểm chạy. Ghi vào SPEC bạn dùng lại
daemon::schedule::next_after hay viết parser cron mới, và vì sao.

Cuối assignment chạy toàn bộ gate mục 9 của plan, cập nhật evidence/handoff/
OPERATOR_GUIDE, ghi phần còn thiếu (mục 11 của plan) vào handoff. Dừng ở CP-4.
Không commit/push/API trả phí/cài lên máy nếu không được cấp.
```

## 5. Prompt tiếp tục sau khi hết context

```text
Tiếp tục assignment HA_PRIME đang ghi trong docs/handoffs/HA_PRIME.vi.md.
Đối chiếu git status/HEAD, docs/specs/HA_PRIME.vi.md, docs/evidence/HA_PRIME.vi.md và
docs/HA_PRIME_PLAN.vi.md trước khi sửa. Mở code và test của item đang dở để xác nhận
trạng thái thật — đừng chỉ tin handoff. Đọc lại mục 3 của plan (vòng lặp RED → GREEN).
Làm exact next action, chạy test bị ảnh hưởng, cập nhật handoff. Không tự chuyển sang
checkpoint kế tiếp khi checkpoint hiện tại chưa đạt.
```

## 6. Khi DeepSeek báo bị kẹt

Dán kèm prompt đang dùng:

```text
Dừng code. Viết vào handoff: (1) bước nào của item nào đang kẹt, (2) test nào đỏ và
dòng lỗi nguyên văn, (3) bạn đã đọc những file/hàm nào, (4) hai giả thuyết về nguyên
nhân và cách kiểm từng giả thuyết mà không sửa code production. Chạy các kiểm tra đó,
ghi kết quả, rồi mới đề xuất sửa. Không nới test, không thêm #[allow], không bỏ bước.
```
