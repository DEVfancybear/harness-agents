# Bàn giao khởi động lại HA_LAUNCH

Tài liệu này là điểm vào cho lượt coding tiếp theo. Cập nhật sau mỗi checkpoint.

## 1. Ranh giới hiện tại

- **H01 xong**: dispatch contract, parser `ha chat`, TTY detector, guard non-TTY exit 2.
- **H02 xong**: launch context thật — paths (`HA_HOME`/platform default/explicit),
  config non-secret + setup state, project identity + Git context, store dir theo
  project, header in actual resolved paths.
- **H03–H08 chưa bắt đầu.** Không mục nào trong bảng staging được đánh dấu xong.
- Gõ `ha` trong terminal thật **chưa** được chứng minh end-to-end: hiện TTY path mở
  boot shell line-mode (đã in header/setup state thật từ H02) nhưng chưa có raw mode,
  editor, streaming. Việc đó là H03; bản PTY transcript là H07.
- `ha chat --headless` vẫn trả `service_unavailable` (exit 1). H04 thay bằng turn thật.

Bằng chứng: mục 2–3 của [evidence HA_LAUNCH](../evidence/HA_LAUNCH.vi.md). Quyết định:
mục 5 của [SPEC HA_LAUNCH](../specs/HA_LAUNCH.vi.md).

## 2. Quyền: cái gì được và không được cấp

Assignment này **không** nêu quyền cho: sửa User PATH thật, cài binary thật lên máy
user, paid provider smoke, publish release, push remote. Các việc đó không được thực
hiện và không được tuyên bố. Prompt mẫu trong HA_LAUNCH_PROMPT mục 4 không tự cấp quyền.

Được cấp: sửa source trong repo, build/test local, process con trong thư mục tạm, cài
vào `-Destination` tạm khi test installer, commit local cho checkpoint (không push).

| Checkpoint | Cần gì | Trạng thái quyền |
|---|---|---|
| H04 live smoke | credential + budget | **chưa cấp** → I10–I12 chỉ dùng HTTP fixture; live ghi `not_run` |
| H06 User PATH | quyền ghi User PATH thật | **chưa cấp** → test bằng fixture trong bộ nhớ |
| H06/H08 cài thật | quyền cài lên máy user | **chưa cấp** → chỉ `-Destination` tạm |
| H08 publish | quyền phát hành release | **chưa cấp** → chỉ release candidate + checksum local |

## 3. Việc tiếp theo chính xác

**H03 — terminal app và input loop.** Prerequisite H02 đã đạt (unit test xanh, clippy
sạch). Thứ tự đề nghị:

1. **Spike và pin thư viện terminal** trước khi viết renderer: khảo sát khả năng raw
   mode Windows/ConPTY + Linux, chọn phiên bản cụ thể, ghi quyết định vào mục 5 SPEC.
   Không viết API thư viện từ trí nhớ; đọc tài liệu/version thật rồi mới code.
2. Tách `interactive/{controller,events,view,input,terminal}.rs`: state reducer
   (booting → ready/setup_required → running → waiting_approval/question → ready,
   canceling, closed) tách khỏi renderer; channel có backpressure; renderer coalesce
   nhưng không drop domain receipt.
3. Editor tối thiểu: Enter gửi một yêu cầu, backspace/history, Unicode tiếng Việt,
   bracketed paste không tự submit nhiều dòng, resize không vỡ prompt.
4. Slash commands `/help`, `/exit`, `/new`, `/status`, `/model`, `/config`,
   `/resume`; `/new` không bỏ active run. Ctrl-C khi chạy: cancel + drain rồi về
   prompt; Ctrl-C khi idle: clear input; EOF/`/exit`: thoát. RAII guard restore
   terminal cho mọi đường thoát (không hứa restore khi process bị kill cứng).
5. Test H03 không cần PTY: controller/reducer + editor với fixture backend (có nhãn
   rõ `fixture`). PTY thật I01/I06/I07/I08 thuộc H07 — không được thay bằng test giả.

Sau H03: H04 (G1 provider incremental stream, G2 model→tool→model loop, G3 durable
session/resume) — đây là phần lớn nhất và phải khảo sát G1–G3 trước khi refactor.

## 4. Trạng thái test ở checkpoint này

- `cargo test -p harness-cli --bin ha` → 25 passed (H01 11, H02 14).
- `cargo test -p harness-cli --test interactive_launch` → 6 passed.
- `cargo clippy -p harness-cli --all-targets -- -D warnings` và `cargo fmt --all -- --check` → sạch.
- `cargo test -p harness-cli --tests --locked`: một lần xanh toàn bộ (161 test), một
  lần đỏ vì flake loopback của `phase_p2` (không phải regression; chi tiết ở mục 5
  evidence). **Không** được báo gate xanh nếu bỏ qua flake này.
- Chưa chạy: `--workspace --all-targets` đầy đủ (H07), Linux, PTY.

## 5. Gap đã biết cần đóng

- Project directory **read-only** chưa có test (mới có không tồn tại và là file).
- Provider endpoint/model chưa resolve; H04 phải chốt và ghi vào SPEC.
- Render tiếng Việt trên terminal thật chưa kiểm chứng.
- Migration note cho hành vi mới "bare `ha` non-TTY exit 2" phải vào operator docs ở H07.
