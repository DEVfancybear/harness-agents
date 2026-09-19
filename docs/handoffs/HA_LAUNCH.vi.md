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
- **H05–H08 chưa bắt đầu.**
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

**H05 — approval, resume và lifecycle.** Prerequisite H04 đã đạt phần code.

1. **Render/answer approval thật**: hiện đường tương tác dùng `ApprovalMode::None` nên
   action bị gate sẽ fail closed. H05 phải render proposal (action, cwd, scope, diff) và
   trả answer đúng request ID qua app authority, không blanket grant; I12 (grant/deny/
   expiry) và I16 (busy/read-only) chạy với process thật.
2. **`/resume`**: list session theo scope rồi chọn, phục hồi state/effect thật trước khi
   submit; dùng chuỗi session + continuation link (`continue_task_streaming`) và
   `SessionService::recover`. Không tin in-memory history.
3. **`/new` và `/exit` khi có run active**: cancel + drain rồi mới chuyển; restore
   terminal/store ownership trên mọi đường thoát.
4. **I13**: hard kill sau receipt đã commit rồi mở lại `/resume` — không rerun side effect
   đã settle; reconcile effect chưa rõ. Test bằng process thật, không chỉ throw exception.
5. Giữ nguyên luật nền tảng đã ghi: một input mỗi session, mỗi lượt một writer generation;
   nếu H05 cần khác thì phải mở quyết định riêng vì đó là thay đổi nền tảng P1.

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
