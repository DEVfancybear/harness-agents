# Bàn giao khởi động lại HA_LAUNCH

Tài liệu này là điểm vào cho lượt coding tiếp theo. Cập nhật sau mỗi checkpoint.

## 1. Ranh giới hiện tại

- **H01 xong**: dispatch contract, parser `ha chat`, TTY detector, guard non-TTY exit 2.
- **H02 xong**: launch context — paths (`HA_HOME`/platform default/explicit), config
  non-secret + setup state, project identity + Git context, store dir theo project.
- **H03 xong phần code**: terminal app thật với `crossterm = "=0.29.0"` — controller
  state machine tách renderer, editor Unicode/history/paste, Ctrl-C/Ctrl-D, raw mode
  có RAII guard, fallback line mode, fixture opt-in có nhãn.
- **H04 xong phần code**: G1 (stream tăng dần), G2 (TurnDriver bounded), G3 (chuỗi session
  cùng task + continuation link), **service thật** thay `PendingService`, và **headless
  turn thật** chạy qua production adapter. Live provider smoke **not_run** (không được cấp
  credential/budget).
- **H05 xong phần code**: approval gate thật (render + answer, deny/expiry không thực thi),
  `/resume` liệt kê/chọn session của project, `/new` không bỏ chạy ngầm, và headless
  `--resume <session-id>` tiếp tục task với context phục hồi. Còn **một ca test**: hard kill
  giữa turn sau khi receipt đã commit.
- **H06–H08 chưa bắt đầu.**
- Phát hiện nền tảng quan trọng: journal P1 chỉ cho **một input mỗi session** và task lease
  cần **generation mới** — nên "cùng phiên" = cùng task + chuỗi session nối nhau, không
  phải một session nhiều input (chi tiết + test ở mục 5.3 evidence và mục 5 SPEC).
- **Chưa có gì được chứng minh trên terminal thật/PTY**: I01/I06/I07/I08 transcript
  thuộc H07. Hiện tại UI mới được chứng minh qua scripted backend + unit test.
- `ha chat --headless` vẫn trả `service_unavailable` (exit 1). H04 thay bằng turn thật.

Bằng chứng: mục 2–4 của [evidence HA_LAUNCH](../evidence/HA_LAUNCH.vi.md). Quyết định:
mục 5 của [SPEC HA_LAUNCH](../specs/HA_LAUNCH.vi.md).

## 2. Quyền: cái gì được và không được cấp

Assignment này **không** nêu quyền cho: sửa User PATH thật, cài binary thật lên máy
user, paid provider smoke, publish release, push remote. Prompt mẫu trong
HA_LAUNCH_PROMPT mục 4 không tự cấp quyền. Được cấp: sửa source trong repo, build/test
local, process con trong thư mục tạm, cài vào `-Destination` tạm, commit local (không push).

| Checkpoint | Cần gì | Trạng thái quyền |
|---|---|---|
| H04 live smoke | credential + budget | **chưa cấp** → I10–I12 dùng HTTP fixture qua production adapter; live ghi `not_run` |
| H06 User PATH | quyền ghi User PATH thật | **chưa cấp** → test bằng fixture trong bộ nhớ |
| H06/H08 cài thật | quyền cài lên máy user | **chưa cấp** → chỉ `-Destination` tạm |
| H08 publish | quyền phát hành release | **chưa cấp** → chỉ release candidate + checksum local |

## 3. Việc tiếp theo chính xác

**Đóng nốt H05 (ca hard-kill), rồi H06 — installer và command resolution.**
Prerequisite H05 đã đạt phần code.

1. ~~Approval thật~~ **đã xong**: driver `ApprovalGate` + `ChannelApprovalGate` (event +
   oneshot, timeout 5 phút), controller render/answer y/n, deny/expiry fail closed kèm tool
   message cho model.
2. ~~`/resume`~~ **đã xong**: liệt kê session của project (read-only, tối đa 20), chọn theo
   số/id, resume = đặt nguồn hội thoại rồi tiếp tục bằng `continue_task_streaming`.
3. ~~`/new`/`/exit` khi có run active~~ **đã xong**: `/new` từ chối khi đang chạy,
   `/exit` cancel + đóng writer trước khi thoát.
4. **Còn lại**: ca **hard kill giữa turn sau khi receipt đã commit** rồi mở lại resume
   (I13 nhánh kill) — cần process thật bị kill giữa lúc tool đã settle, và kiểm tra không
   rerun side effect. Nếu làm cùng PTY harness của H07 thì ghi chung một ca.
5. Giữ nguyên luật nền tảng đã ghi: một input mỗi session, mỗi lượt một writer generation.

Lưu ý kỹ thuật đã biết: fixture loopback trong môi trường này flaky khi test chạy song
song (mục 6 evidence) — test HTTP fixture của H04 nên chạy với `--test-threads=1` hoặc
thiết kế retry có ghi chú, và **không** sửa acceptance P2 đã được chấp nhận.

Sau H04: H05 (approval/resume/lifecycle), rồi H06 (installer/PATH, test disposable),
H07 (gate `Verify-HaLaunch.ps1` + PTY harness + I01–I18), H08 (release candidate).

## 4. Trạng thái test ở checkpoint này

- `cargo test -p harness-cli --bin ha` → 52 passed.
- `cargo test -p harness-cli --test interactive_launch` → 9 passed.
- `cargo test -p harness-cli --test interactive_session` → 5 passed.
- `cargo test -p harness-providers` → 3 passed.
- `cargo test -p harness-cli --tests --locked -- --test-threads=1` → **211 passed, 0 failed**.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` → exit 0.
- Chưa chạy: `--workspace --all-targets` đầy đủ (H07), Linux, PTY thật, live provider.

## 5. Gap đã biết cần đóng

- PTY/ConPTY thật cho I01/I06/I07/I08 (H07): Ctrl-C delivery trên Windows, tiếng Việt
  trên terminal thật, restore terminal sau lỗi sau khi đã vào raw mode.
- Multiline editing trong editor.
- Project directory read-only chưa có test.
- Provider endpoint/model resolution (H04) và credential resolver ngoài env var.
- Migration note cho hành vi mới "bare `ha` non-TTY exit 2" (H07, operator docs).
