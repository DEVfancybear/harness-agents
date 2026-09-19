# Prompt giao DeepSeek: nâng `ha` thành TUI terminal

[Plan chi tiết T01–T08](HA_TUI_PLAN.vi.md) · [Track khởi động H01–H08](HA_LAUNCH_PLAN.vi.md) · [Prompt track H](HA_LAUNCH_PROMPT.vi.md)

Mỗi prompt là một assignment độc lập, dừng ở điểm dừng ghi trong plan. Prompt mẫu **không tự cấp quyền** commit/push/paid API/cài thật; nếu muốn cấp thì ghi rõ trong assignment.

## 1. Prompt bắt đầu — T01–T02 (spike + refactor nền)

```text
Triển khai T01–T02 trong docs/HA_TUI_PLAN.vi.md. Chưa làm T03 trở đi.
Mục tiêu: chốt thư viện/inline viewport bằng số đo thật trên máy này, rồi đổi
InteractiveController sang trả HistoryItem/UiState mà plain mode vẫn cho transcript
byte-identical với hiện tại.

Đọc quy định repo, git status/HEAD, docs/HA_TUI_PLAN.vi.md (mục 2–4, T01, T02),
docs/specs/HA_LAUNCH.vi.md, docs/handoffs/HA_LAUNCH.vi.md và các file source plan dẫn.
Mọi việc làm trên root workspace và binary ha duy nhất; không tạo crate UI riêng,
không CLI/engine thứ hai. Không đổi luật đã chốt: một input mỗi session, approval
fail-closed, không mock fallback, headless không ANSI, exit 0/1/2 giữ nguyên.

T01: thêm ratatui pin exact theo plan, xác minh cargo tree chỉ có một crossterm;
POC inline viewport + insert_before sau cờ tạm, đo trên Windows Terminal, conhost và
PTY harness của repo; trả lời bốn câu hỏi đo (đổi chiều cao viewport lúc chạy,
TestBackend với inline, Alt+Enter trên ConPTY, scrolling-regions). Ghi vào
docs/specs/HA_TUI.vi.md kèm số đo; xoá POC/cờ tạm trước khi sang T02.

T02: Effect::{History(HistoryItem), Stream, Redraw, Exit}; ui_state()/tick();
SessionEvent::StepStarted và ApprovalExpired có producer thật trong service.rs;
HistoryItem::plain_lines() tái dùng view.rs. Đổi assertion test cũ sang plain_lines
một cách cơ học; không xoá, không nới test; thêm test snapshot byte-identical U20,
test StepStarted và test approval hết hạn đóng modal mà không grant.

Chạy fmt, clippy -D warnings, cargo test -p harness-cli --bin ha --locked,
interactive_session và interactive_launch với --test-threads=1, rồi
scripts/Verify-HaLaunch.ps1 -Json một lần xanh (đọc mục 8 plan về flake loopback:
chạy lại tối đa ba lần, ghi từng lần, không sửa test để xanh).
Tạo docs/evidence/HA_TUI.vi.md và docs/handoffs/HA_TUI.vi.md theo
docs/implementation-next/TEMPLATES.vi.md; báo số test, source digest, OS, not_run.
Dừng sau T02. Không commit/push/paid API/cài lên máy user nếu assignment không cấp.
```

## 2. Prompt TUI dùng được — T03–T05

```text
Tiếp tục T03–T05 theo docs/HA_TUI_PLAN.vi.md và docs/handoffs/HA_TUI.vi.md.
Verify T01–T02 trong source/tests trước khi sửa; giữ quyết định T01 đã ghi trong
docs/specs/HA_TUI.vi.md trừ khi có số đo mâu thuẫn (ghi lại nếu đổi).

T03 composer: paste giữ newline (sửa có chủ ý test
h03_editor_paste_never_submits_multiple_commands: vẫn một submit, nay giữ \n; ghi
thay đổi hợp đồng vào SPEC), Ctrl-U/W/A/E, ↑/↓ theo hàng khi nhiều dòng, Tab
hoàn thành slash command, widget composer wrap theo unicode-width sau NFC.
T04 history + live block: HistoryItem → Line có style, markdown-lite không thêm
dependency và không nuốt ký tự, live block từ pending_text, commit overflow theo
đúng thứ tự flush_stream, tool card settle tại chỗ kèm thời lượng.
T05 status bar: spinner chỉ tick khi có run, step/tools/elapsed/model/session/hint;
không draw khi idle và không có effect.

Kiểm chứng bằng ratatui TestBackend (unit) cho U02–U06, U10, U20; chạy lại PTY i06 và
i21 trên TUI mặc định trong console thật bằng scripts/Invoke-HaPtyAcceptance.ps1;
không mock host TUI; FixtureService có nhãn hoặc HTTP fixture qua adapter thật.
Chạy affected regressions và gate H một lần xanh. Cập nhật evidence/handoff HA_TUI.
Dừng sau T05; không tự làm approval/picker/fallback của T06–T07.
```

## 3. Prompt hoàn thiện và nghiệm thu — T06–T08

```text
Triển khai T06–T08 theo docs/HA_TUI_PLAN.vi.md sau khi T01–T05 có evidence.
T06: approval panel với y/n và đếm ngược hết hạn lấy từ event, giữ đường trả lời gõ
chữ; picker /resume với ↑↓/Enter/Esc, /resume <n>|<id> giữ nguyên, không mở khi
đang chạy; overlay /help /status /config /model không ghi vào history, plain mode
vẫn in dòng. Tái dùng test h05 của interactive_session để chứng minh deny/expire
không thực thi.
T07: probe size/TERM/NO_COLOR/HA_UI, cờ ha chat --plain (conflict với --headless),
rơi về plain có lý do ở stderr; resize giữ draft; panic hook restore terminal
không đổi hành vi I08; thoát xoá viewport để lại scrollback; NO_COLOR không SGR màu.
Chạy lại toàn bộ ca PTY cũ trên TUI mặc định trong một lần chạy.
T08: thêm selector T bắt buộc và bước gate vào scripts/Verify-HaLaunch.ps1, thêm ca
PTY t01/t03/t06/t07 vào scripts/Invoke-HaPtyAcceptance.ps1 và mô tả đầu script;
operator guide vi+en mục 12 (bảng phím, plain fallback, giới hạn Windows theo số đo
T01); README một dòng trỏ tới plan/evidence T; Verify-Docs.ps1 -SelfTest xanh.

Bằng chứng: gate Verify-HaLaunch.ps1 -Json passed:true failures:[] (ghi từng lần
chạy nếu gặp flake loopback, không sửa test), PTY_EXIT 0 đủ ca kèm transcript,
negative controls, not_run (Linux, live smoke, VM sạch). Không publish, không cài
lên máy user, không paid smoke nếu assignment không cấp. Cập nhật evidence/handoff
HA_TUI với exact next action; chỉ nhận "track T xong" khi CP-D đạt.
```

## 4. Nếu muốn giao cả track trong một assignment

Dùng: “Triển khai T01–T08 theo plan HA_TUI, đi theo checkpoint CP-A → CP-D; không chuyển checkpoint khi gate chưa `failures: []`; cập nhật handoff HA_TUI sau mỗi checkpoint.” Việc này chắc chắn cần nhiều lượt coding. Quyền commit/push, paid smoke, cài thật phải được ghi trong assignment; đưa prompt mẫu này không tự cấp quyền.

## 5. Prompt tiếp tục sau khi hết context

```text
Tiếp tục assignment HA_TUI đang ghi trong docs/handoffs/HA_TUI.vi.md.
Đối chiếu git status/HEAD, docs/specs/HA_TUI.vi.md, evidence và plan trước khi sửa;
inspect symbol/test của item đang dở, đừng chỉ tin summary. Giữ quyết định T01 và
D1–D7 của plan nếu không có số đo mâu thuẫn. Không lặp lệnh có side effect đã xong.
Làm exact next action, chạy affected tests và gate cần thiết, cập nhật handoff.
Không tự chuyển sang item/checkpoint kế tiếp khi assignment hiện tại kết thúc.
```
