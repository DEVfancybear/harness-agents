# Prompt giao DeepSeek: mở CLI bằng lệnh `ha`

[Plan chi tiết](HA_LAUNCH_PLAN.vi.md)

## 1. Prompt bắt đầu — H01–H03

```text
Triển khai H01–H03 trong docs/HA_LAUNCH_PLAN.vi.md.
Mục tiêu: mở terminal ở thư mục bất kỳ, gõ ha không tham số thì vào CLI tương tác,
hiện đúng project/cấu hình, có ô nhập và giữ process sống tới khi user thoát.

Đọc quy định repo, Git status, plan H và các file source được plan dẫn.
Mọi feature H/M dùng source hiện tại, root workspace và binary ha duy nhất.
Đọc docs/implementation-next/INTEGRATION_MAP.vi.md; kiểm tra SPEC/evidence/handoff H
và source HEAD trước khi sửa. Reuse phần đã đạt, chỉ implement/refactor gaps;
không tạo CLI/engine thứ hai hoặc đợi toàn M0–M12.

Cập nhật docs/specs/HA_LAUNCH.vi.md, giữ tiến độ/decisions đã có. Xác minh no-arg
dispatch ở HEAD, help/version fast paths, TTY detection và commands compatibility.
None => Ok(()) chỉ là baseline cũ, không được giả định source hiện tại còn như vậy.
Implement bootstrap/config/project resolution và InteractiveController tách renderer.
Thiếu key vẫn mở setup UI và thoát được; không tự gọi API khi launch, không pretend
mock response là model thật. Fixture service cho tests phải có label rõ.

Dùng PTY/ConPTY integration để test bare ha; Command::output() không đủ vì đó là
non-interactive pipes. Test Unicode/paste/resize/Ctrl-C/exit/error cleanup, cwd ngoài
repo, no-Git project, missing/corrupt config. Giữ old subcommands/JSON/exit semantics.

Chạy tests thực có, báo source digest/exact test selectors/count/OS limitations.
Tạo docs/evidence/HA_LAUNCH.vi.md và docs/handoffs/HA_LAUNCH.vi.md.
Chỉ nhận launch/UI slice xong nếu tests đạt; không claim interactive agent usable
hoặc public install-and-run xong khi H04–H08 chưa hoàn tất.
Dừng sau H01–H03. Không tự sửa user PATH/cài binary/paid API/publish ngoài quyền
đã được user cấp trong assignment/session.
```

## 2. Prompt nối model thật — H04–H05

```text
Tiếp tục H04–H05 theo docs/HA_LAUNCH_PLAN.vi.md và handoff HA_LAUNCH.
Verify H01–H03 source/tests trước sửa. Nối InteractiveSessionService vào application
runtime; khảo sát và hoàn thiện G1 real incremental provider, G2 model-tool-model
continuation và G3 durable sessions. Không wrapper spawn ha run rồi parse stdout.
Giữ cùng session qua nhiều lượt, admit một input mỗi user message, tool calls qua
policy/approval/receipt gate. Implement cancel, questions, resume và failure handling.
Không production fallback MockProvider; không fake streaming bằng chia response cuối.
Test I10–I13/I16 với HTTP fixture qua adapter thật, tools/process/store thật trong
temp repo; live provider chỉ khi user cấp credentials và budget.
Chạy affected regressions, ghi gaps nếu G1–G3 chưa hoàn thiện, bàn giao evidence và
handoff. Dừng sau H04–H05, không báo full feature done trước installer/release gates.
```

## 3. Prompt cài và kiểm chứng — H06–H08

```text
Triển khai H06–H08 theo docs/HA_LAUNCH_PLAN.vi.md sau khi H01–H05 có evidence.
Giữ developer install route; thêm end-user prebuilt route không cần Rust/Git/Node.
Windows PATH xử lý User scope riêng, không ghi merged process PATH vào User PATH.
Detect old ha/alias/function shadowing; không thay command không sở hữu hoặc kill
running processes. Update/uninstall giữ config/sessions và chỉ quản lý owned files.
Test installed command resolution từ shell và unrelated cwd, không chỉ absolute
binary --version. Chạy PTY I01 trên installed binary, I14–I20 và compatibility tests.
Tạo scripts/Verify-HaLaunch.ps1 với exact discovery và per-platform evidence.
Phân biệt user PATH đã persist với terminal đang kế thừa env cũ; test phương pháp rõ.
Build checksummed release candidate, clean-machine install test; publish artifacts
chỉ khi assignment/session đã cấp quyền release, nếu chưa thì bàn giao candidate và
ghi public download chưa có. Không invent release URL hoặc claim tests chưa chạy.
Bàn giao docs/specs, evidence và handoff HA_LAUNCH với exact next action.
```

## 4. Nếu muốn giao cả feature trong một assignment

Dùng: “Triển khai H01–H08 theo plan HA_LAUNCH, thực hiện theo dependencies và gate từng checkpoint; không chuyển tiếp khi prerequisites chưa đạt.” Việc này có thể cần nhiều lượt coding; cập nhật handoff sau mỗi checkpoint. Quyền thay User PATH/cài thật/paid smoke/publish cần được nêu trong assignment, không suy từ việc chỉ đưa prompt mẫu.
