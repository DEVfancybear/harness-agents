# Bàn giao khởi động lại HA_LAUNCH

Tài liệu này là điểm vào cho lượt coding tiếp theo. Cập nhật sau mỗi checkpoint.

## 1. Ranh giới hiện tại

- **H01 xong**: dispatch contract, parser `ha chat`, TTY detector, guard non-TTY exit 2.
- **H02 xong**: launch context — paths (`HA_HOME`/platform default/explicit), config
  non-secret + setup state, project identity + Git context, store dir theo project.
- **H03 xong phần code**: terminal app thật với `crossterm = "=0.29.0"` — controller
  state machine tách renderer, editor Unicode/history/paste, Ctrl-C/Ctrl-D, raw mode
  có RAII guard, fallback line mode, fixture opt-in có nhãn.
- **H04 đang làm**: khảo sát G1–G3 xong, **G1 xong** (stream tăng dần + test barrier),
  **G2 xong** (TurnDriver bounded model→tool→model, tool result quay lại model, fail→fix,
  bound báo lý do dừng). Còn lại: G3, service thật thay `PendingService`, headless turn.
- **H05–H08 chưa bắt đầu.**
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

**H04 — application service và agent execution thật.** Prerequisite H03 đã đạt.

1. ~~Khảo sát G1–G3~~ **đã xong** (bảng gate ở mục 5 SPEC).
2. ~~G1 stream tăng dần~~ **đã xong**: `crates/harness-providers/src/streaming.rs`
   (`StreamingModelProvider`, `ProviderEventStream`, `collect_events`) + test barrier.
   Khi nối runtime, dùng boundary này thay vì đọc `Vec` rồi chia nhỏ.
3. ~~G2~~ **đã xong**: `harness-tools/src/turn_driver.rs` (`TurnDriver`, `TurnLimits`,
   `TurnObserver`, `TurnStop`) + runtime `run_streaming`/`continue_run`; test ở
   `crates/harness-cli/tests/interactive_session.rs` (3 ca). Khi nối service, map
   `TurnProgress` → `SessionEvent` và truyền `ApprovalMode::None` cho đường tương tác
   (approval thật là H05).
4. **G3**: nối store/session theo project, resume/replay đúng task; I12 (approval
   grant/deny/expiry + auth failure không fallback mock, không lộ key).
5. **Nối `interactive/service.rs`** thật thay `PendingService` (giữ nguyên port và
   `SessionEvent`), và làm `ha chat --headless --prompt <text> --json` chạy một turn
   thật, stdout JSON, log ra stderr, không bật raw mode.
6. **Live smoke**: ghi `not_run` vì chưa được cấp credential/budget.

Lưu ý kỹ thuật đã biết: fixture loopback trong môi trường này flaky khi test chạy song
song (mục 6 evidence) — test HTTP fixture của H04 nên chạy với `--test-threads=1` hoặc
thiết kế retry có ghi chú, và **không** sửa acceptance P2 đã được chấp nhận.

Sau H04: H05 (approval/resume/lifecycle), rồi H06 (installer/PATH, test disposable),
H07 (gate `Verify-HaLaunch.ps1` + PTY harness + I01–I18), H08 (release candidate).

## 4. Trạng thái test ở checkpoint này

- `cargo test -p harness-cli --bin ha` → 50 passed (H01 8, H02 17, H03 25).
- `cargo test -p harness-cli --test interactive_launch` → 7 passed.
- `cargo clippy -p harness-cli --all-targets -- -D warnings` và `cargo fmt --all -- --check` → sạch.
- `cargo test -p harness-cli --tests --locked -- --test-threads=1` → kết quả ghi ở mục 4 evidence.
- Chưa chạy: `--workspace --all-targets` đầy đủ (H07), Linux, PTY thật.

## 5. Gap đã biết cần đóng

- PTY/ConPTY thật cho I01/I06/I07/I08 (H07): Ctrl-C delivery trên Windows, tiếng Việt
  trên terminal thật, restore terminal sau lỗi sau khi đã vào raw mode.
- Multiline editing trong editor.
- Project directory read-only chưa có test.
- Provider endpoint/model resolution (H04) và credential resolver ngoài env var.
- Migration note cho hành vi mới "bare `ha` non-TTY exit 2" (H07, operator docs).
