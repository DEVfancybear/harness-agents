# Bàn giao khởi động lại HA_LAUNCH

Tài liệu này là điểm vào cho lượt coding tiếp theo. Cập nhật sau mỗi checkpoint.

## 1. Ranh giới hiện tại

- **H01 xong** ở mức dispatch contract/parser/guard non-TTY. Bằng chứng: mục 2 của
  [evidence HA_LAUNCH](../evidence/HA_LAUNCH.vi.md); quyết định: mục 5 của
  [SPEC HA_LAUNCH](../specs/HA_LAUNCH.vi.md).
- **H02–H08 chưa bắt đầu.** Không có mục nào trong bảng staging được đánh dấu xong.
- Gõ `ha` trong terminal thật **chưa** được chứng minh: cần H03 (UI thật) và H07
  (PTY harness). Hiện tại TTY path chỉ mở boot shell line-mode ghi rõ
  `connection pending`; đây là hành vi có chủ đích, không phải agent đã nối.
- `ha chat --headless` trả `service_unavailable` (exit 1). H04 thay bằng turn thật.

## 2. Quyền: cái gì được và không được cấp

Assignment này **không** nêu quyền cho: sửa User PATH thật, cài binary thật lên máy
user, paid provider smoke, publish release, push remote. Vì vậy các việc đó không
được thực hiện và không được tuyên bố. Prompt mẫu trong HA_LAUNCH_PROMPT mục 4 không
tự cấp quyền (chính prompt đó nói vậy).

Được cấp: sửa source trong repo, build/test local, tạo process con trong thư mục tạm,
cài vào `-Destination` tạm khi test installer, commit local cho checkpoint (không push).

Checkpoint nào cần quyền chưa có sẽ dừng ở `blocked_on_authority`, không được mô tả
là đã đạt:

| Checkpoint | Cần gì | Trạng thái quyền |
|---|---|---|
| H04 live smoke | credential + budget | **chưa cấp** → I10–I12 chỉ dùng HTTP fixture; live ghi `not_run` |
| H06 User PATH | quyền ghi User PATH thật | **chưa cấp** → test bằng fixture trong bộ nhớ, không mutate registry |
| H06/H08 cài thật | quyền cài lên máy user | **chưa cấp** → chỉ `-Destination` tạm |
| H08 publish | quyền phát hành release | **chưa cấp** → chỉ release candidate + checksum local |

## 3. Việc tiếp theo chính xác

**H02 — startup context, project và first-run config.** Prerequisite H01 đã đạt (bảng
staging trong SPEC, mục 2 evidence). Phạm vi và thứ tự:

1. `interactive/paths.rs`: resolve user config/data với `HA_HOME` override và
   precedence đã ghi ở plan mục 5; env phải **inject được** để test hermetic.
2. `interactive/config.rs`: load config non-secret, trạng thái `setup_required`,
   lỗi actionable cho config hỏng (I09); không gọi provider khi launch (I05).
3. `interactive/bootstrap.rs`: caller cwd / `--cwd`, project identity, Git root chỉ
   cho project context (không chdir về installation dir), store/catalog theo project,
   owner-busy khi hai terminal cùng project (I16).
4. Thay `BootContext::resolve` tạm ở `interactive/app.rs` bằng bootstrap thật; header
   phải hiện project/provider/setup state đúng và ghi actual resolved paths trong
   diagnostics.
5. Test H02: I04 (đường dẫn spaces/Unicode, no-Git), I05 (HA_HOME rỗng, offline,
   không key), I09 (config hỏng/cwd sai/quyền), I16 (hai writer cùng project) — fixture
   home/env hoàn toàn tách khỏi user thật.

Sau H02: H03 (terminal app thật, chọn/pin thư viện terminal sau spike Windows), rồi
H04 (G1/G2/G3). Không nhảy sang H04–H08 trước khi H02/H03 có evidence.

## 4. Trạng thái test ở checkpoint này

- `cargo test -p harness-cli --bin ha` → 10 passed.
- `cargo test -p harness-cli --test interactive_launch` → 6 passed.
- `cargo clippy -p harness-cli --all-targets -- -D warnings` → sạch; `cargo fmt --all -- --check` → sạch.
- Regression `cargo test -p harness-cli --tests --locked` (phase_p0..p7 + H01) → kết quả
  ghi ở mục 2 evidence; một flake loopback của phase_p2 đã được điều tra và ghi lại ở
  mục 4 evidence, không phải regression của H01.
- Chưa chạy: `--workspace --all-targets` đầy đủ (H07 gate), Linux, PTY.
