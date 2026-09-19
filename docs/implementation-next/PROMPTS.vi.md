# Prompt giao việc cho DeepSeek

[Sổ tay](README.vi.md) · [Templates](TEMPLATES.vi.md)

## 1. Prompt bắt đầu — nên dùng ngay

```text
Hãy triển khai M0-01 của kế hoạch mới Harness Agents, chưa làm work item khác.
Đọc quy định repo áp dụng, docs/implementation-next/README.vi.md,
CONTRACTS.vi.md và M0.vi.md trong cùng thư mục. Mục tiêu/kiến trúc ở
docs/HARNESS_MASTER_PLAN.vi.md và INTEGRATION_MAP.vi.md.
Dùng source hiện tại, root Cargo workspace và binary ha duy nhất cho mọi M/H.
Đối chiếu source/tests/evidence P/H với contracts M; lập gap inventory theo từng
requirement: reuse_verified/adapt/missing/incompatible. Ghi symbol/callers/tests,
chỉ sửa hoặc bổ sung phần thiếu và nối vào luồng ha hiện có; không scaffold lại.
Viết SPEC ngắn rồi bổ sung/refactor identities/state transitions/errors và tests của
M0-01. Tự quyết các chi tiết thường lệ theo defaults; ghi khác biệt vào SPEC.
Không nhận task completed chỉ vì run completed, không tạo production stub-success.
Chạy tests thực sự, báo test count và source revision/digest; nếu chưa có full
milestone gate thì ghi rõ item verification, không nhận M0 accepted.
Cập nhật handoff CURRENT với exact next action. Dừng sau M0-01.
Không tự triển khai M0-02..M12 hoặc gọi paid API. Commit/push chỉ theo quyền
đã được cấp trong assignment/session; không coi prompt mẫu này tự cấp quyền.
```

## 2. Giao một milestone sau khi prerequisites đã đạt

Thay `Mn` bằng một milestone cụ thể. Nếu muốn nhỏ hơn, thay phạm vi bằng một work item; không đưa hai giá trị khác nhau trong cùng prompt.

```text
Chỉ triển khai milestone Mn, work items Mn-01..Mn-04.
Đọc docs/implementation-next/README.vi.md, CONTRACTS.vi.md, Mn.vi.md,
acceptance cases được dẫn và evidence/handoff của dependency closure.
Verify prerequisites theo source/test thật; nếu thiếu prerequisite ngoài scope,
ghi chính xác khoảng trống, không giả lập thành completed để đi tiếp.
Viết SPEC rồi thực hiện theo work-item order. Dùng actual path map trong SPEC,
giữ public contracts/authority/durability đã chấp nhận. Sửa source hiện tại,
reuse services/tests theo INTEGRATION_MAP; không tạo workspace, CLI hoặc engine
thứ hai. Không mở rộng tính năng chưa giao hoặc ghi đè thay đổi không liên quan.
Thêm integration tests chạy component thật và targeted fault injection;
chỉ mock external model/network boundaries phù hợp. Đừng mock implementation
đang cần chứng minh. Gate có exact discovery; zero/ignored required tests là fail.
Hoàn tất evidence và restart handoff theo TEMPLATES.vi.md; nói rõ OS/gates chưa
chạy. Không nhận accepted khi còn required verification thiếu. Dừng trước mốc sau.
```

## 3. Tiếp tục sau khi hết context

```text
Tiếp tục assignment đang ghi trong docs/handoffs/CURRENT.vi.md.
Trước edits, đối chiếu Git status/source revision, SPEC, evidence và runbook.
Đừng chỉ tin summary; inspect files/symbols/tests của work item đang dở.
Giữ nguyên decisions đã chốt nếu không có bằng chứng mâu thuẫn. Không lặp paid
calls, migrations hay commands có side effects đã hoàn tất. Làm exact next action,
hoàn tất scope còn lại, chạy affected tests và gate cần thiết, cập nhật handoff.
Không tự chuyển sang milestone khác khi assignment hiện tại kết thúc.
```

## 4. Review/nghiệm thu một milestone

```text
Review milestone Mn theo docs/implementation-next/Mn.vi.md và CONTRACTS.vi.md.
Đọc diff/source/evidence, truy đường application -> runtime -> store/tool gate,
kiểm tra failure paths và actual test discovery. Chạy các gate được yêu cầu nếu
môi trường cho phép. Đối chiếu acceptance IDs, source digest và OS evidence.
Báo actionable findings với file/symbol, điều kiện tái hiện, impact và invariant
bị vi phạm. Phân biệt verified pass, chưa chạy và unsupported. Không sửa fixture
expectations để cho pass; không đánh dấu milestone accepted nếu gate thiếu.
```

## 5. Phạm vi giao nhỏ và việc không được giao mơ hồ

Nên giao `M1-02`, hoặc `M3-01..M3-02 sau khi M1/M2 accepted`, thay vì “code theo tất cả docs”. Một work item là đơn vị giao việc, không bắt buộc một turn phải xong. Nếu scope chưa hoàn tất, handoff cho đúng item; không giảm requirement để có dòng done.

Các runbooks có prompt riêng ghi exact prerequisites, targets, smoke scenario và điểm dừng. Người dùng không cần gửi toàn bộ docs vào chat: chỉ cần đường dẫn + ID assignment khi model có quyền đọc repository.
