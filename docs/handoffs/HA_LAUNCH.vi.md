# Bàn giao khởi động lại HA_LAUNCH

Tài liệu này là điểm vào cho lượt coding tiếp theo. Cập nhật sau mỗi checkpoint.

## 1. Ranh giới hiện tại

- **H01 xong**: dispatch contract, parser `ha chat`, TTY detector, guard non-TTY exit 2.
- **H02 xong**: launch context — paths (`HA_HOME`/platform default/explicit), config
  non-secret + setup state, project identity + Git context, store dir theo project.
- **H03 xong phần code**: terminal app thật với `crossterm = "=0.29.0"` — controller
  state machine tách renderer, editor Unicode/history/paste, Ctrl-C/Ctrl-D, raw mode
  có RAII guard, fallback line mode, fixture opt-in có nhãn. Hai điều H03 để ngỏ (Ctrl-C
  có tới app như key event trên ConPTY không; tiếng Việt nhập/hiển thị ra sao) nay đã
  **được kiểm bằng PTY thật** ở H07.
- **H04 xong phần code**: G1 (stream tăng dần), G2 (TurnDriver bounded), G3 (chuỗi session
  cùng task + continuation link), **service thật** thay `PendingService`, và **headless
  turn thật** chạy qua production adapter. Live provider smoke **not_run** (không được cấp
  credential/budget).
- **H05 xong phần code**: approval gate thật (render + answer, deny/expiry không thực thi),
  `/resume` liệt kê/chọn session của project, `/new` không bỏ chạy ngầm, và headless
  `--resume <session-id>` tiếp tục task với context phục hồi. Còn **một ca test**: hard kill
  giữa turn sau khi receipt đã commit.
- **H06 xong phần code**: installer dùng artifact Cargo báo + digest + manifest, thay thế có
  staging/rollback, phân loại lỗi khóa file, User PATH merge tách biệt có test, cảnh báo
  shadowing; `-SelfTest` 14 check xanh. **Không** ghi User PATH thật và không cài vào vị
  trí thật của user (không được cấp quyền).
- **H07 xong phần gate + docs**: `scripts/Verify-HaLaunch.ps1` chạy xanh toàn bộ
  (format, clippy `-D warnings`, discovery selector bắt buộc, unit + acceptance + regression
  P0–P7 với `--test-threads=1`, installer self test, docs checker) và in rõ danh sách
  **not_run**. Operator guide (vi + en) đã có mục 12 với bảng migration cho hành vi
  non-TTY exit 2.
- **PTY thật (round 13): I01/I06/I07 đã xanh.** Một lần chạy bounded
  `scripts/Invoke-HaPtyAcceptance.ps1` báo `PTY_EXIT: 0`, `5 passed; 0 failed` trong 11.01 s
  (i01, i06, i07a, i07b, i08; transcript `target/pty-acceptance/pty-all.txt`). Điều kiện đo được:
  ConPTY chỉ hoạt động khi process tạo pseudo-console **sở hữu một console**, mà `cargo test`
  trong sandbox thì không — nên năm ca vẫn `#[ignore]` và phải chạy bằng runner đó; gate
  liệt kê chúng là `not_run` kèm hướng dẫn, không tính là pass tự động.
- **H08 xong phần code**: bundle candidate + checksum + manifest (`published:false`), installer
  `-FromBundle` (verify trước khi cài) và `-Uninstall` (chỉ xóa file sở hữu, giữ user data),
  cùng chứng minh disposable I19/I20. **Không publish** và chưa có VM sạch thật.
- Phát hiện nền tảng quan trọng: journal P1 chỉ cho **một input mỗi session** và task lease
  cần **generation mới** — nên "cùng phiên" = cùng task + chuỗi session nối nhau, không
  phải một session nhiều input (chi tiết + test ở mục 5.3 evidence và mục 5 SPEC).
- **I08 đã có bằng chứng** (fault seam chỉ ở debug build + 3 unit test phục hồi mode + ca PTY
  `i08` thoát 1 kèm lỗi nêu tên, không treo). Kill process cứng vẫn ngoài phạm vi như plan ghi.

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
| H08 publish | quyền phát hành release | **chưa cấp trong assignment**; round 9 user chọn phương án (a) cấp quyền publish + credential, nhưng **đầu vào cụ thể vẫn thiếu** (`gh`/`GH_TOKEN` và xác nhận push tag `ha-v0.1.0`) → vẫn chỉ có release candidate + checksum local |

## 3. Việc tiếp theo chính xác

**H08 — prebuilt release và clean-machine install (chưa publish).** H07 đã có gate xanh và
transcript PTY thật cho I01/I06/I07; phần chặn còn lại là đầu vào publish/credential (mục 2)
và I08. Chi tiết từng bước ở mục 6.

Chi tiết từng bước ở mục 6; nhắc lại: H08 **không** được publish vì assignment không cấp quyền.

1. ~~Approval thật~~ **đã xong**: driver `ApprovalGate` + `ChannelApprovalGate` (event +
   oneshot, timeout 5 phút), controller render/answer y/n, deny/expiry fail closed kèm tool
   message cho model.
2. ~~`/resume`~~ **đã xong**: liệt kê session của project (read-only, tối đa 20), chọn theo
   số/id, resume = đặt nguồn hội thoại rồi tiếp tục bằng `continue_task_streaming`.
3. ~~`/new`/`/exit` khi có run active~~ **đã xong**: `/new` từ chối khi đang chạy,
   `/exit` cancel + đóng writer trước khi thoát.
4. **Đã làm phần lớn**: ca gián đoạn với durable state thật (drop toàn bộ in-memory,
   writer generation mới) chứng minh receipt đã settle không bị chạy lại. **Còn lại**: kill
   *process* thật giữa turn — cần PTY (H07) và vẫn not_run.
   Lưu ý contract: P3 yêu cầu approval cho **mọi** action không bị deny, nên headless fail
   closed cho mọi tool call; muốn automation chạy tool phải có flag approval tường minh do
   user chốt (xem SPEC).
5. Giữ nguyên luật nền tảng đã ghi: một input mỗi session, mỗi lượt một writer generation.

Lưu ý kỹ thuật đã biết: fixture loopback trong môi trường này flaky khi test chạy song
song (mục 6 evidence) — test HTTP fixture của H04 nên chạy với `--test-threads=1` hoặc
thiết kế retry có ghi chú, và **không** sửa acceptance P2 đã được chấp nhận.

Sau H04: H05 (approval/resume/lifecycle), rồi H06 (installer/PATH, test disposable),
H07 (gate `Verify-HaLaunch.ps1` + PTY harness + I01–I18), H08 (release candidate).

## 4. Trạng thái test ở checkpoint này

- `cargo test -p harness-cli --bin ha` → 61 passed (gồm 3 unit test I08 cho phục hồi mode).
- `cargo test -p harness-cli --test interactive_launch` → 14 passed.
- `cargo test -p harness-cli --test interactive_session` → 9 passed.
- `cargo test -p harness-cli --test interactive_terminal` → 5 ignored (cần console thật; chạy
  bằng runner, `5 passed`).
- `cargo test -p harness-providers` → 3 passed.
- `cargo test -p harness-cli --tests --locked -- --test-threads=1` → **229 passed, 0 failed,
  5 ignored** (5 ignored là các ca PTY; chi tiết trong `target/cli-tests-round13.txt`).
- `cargo clippy --workspace --all-targets --locked -- -D warnings` → exit 0.
- `pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1` → `"passed": true`, `failures: []`
  (format, clippy, discovery selector, unit + acceptance + P0–P7 serial, installer self test,
  release self test, docs).
- Năm ca PTY: `scripts/Invoke-HaPtyAcceptance.ps1` → `PTY_EXIT: 0`, `5 passed; 0 failed`
  (console thật; `cargo test` trong sandbox vẫn `#[ignore]` năm ca này).
- Chưa chạy: Linux, live provider smoke, publish, VM sạch thật, kill process cứng giữa turn.

## 5. Gap đã biết cần đóng

- Kill *process* cứng giữa turn (H05 I13): hiện chỉ mô phỏng bằng mất state in-memory + writer
  generation mới; I01/I06/I07/I08 đã xanh trong console thật (mục 12.2–12.3 evidence).
- Multiline editing trong editor.
- Project directory read-only chưa có test.
- Provider endpoint/model resolution (H04) và credential resolver ngoài env var.
- Kill *process* thật giữa turn rồi để host khác tiếp quản (H05 I13): hiện chỉ mô phỏng
  bằng mất state in-memory + writer generation mới.

## 6. Việc tiếp theo chi tiết (H08)

**Bước -1 — PTY (round 13): I01/I06/I07 đã đạt trong console thật.**
`pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1` → `PTY_EXIT: 0`,
`4 passed; 0 failed` (9.03 s, transcript `target/pty-acceptance/pty-all.txt`). Hai lỗi còn lại
của round 12 đều nằm ở **test**: listener `accept()` chặn vô hạn (nay non-blocking + deadline,
kèm khẳng định cờ `contacted`), và kỳ vọng bracketed paste vượt quá điều ConPTY host này làm
được (nay khẳng định đúng thứ app phải bảo đảm: sống sót và prompt còn dùng được sau paste).
Còn lại của H07: ca **I08** (inject lỗi render/backend sau khi terminal khởi tạo) và transcript
tương ứng trong gate. Bốn ca vẫn `#[ignore]` vì `cargo test` trong sandbox không có console.

**Bước 0 — hai input còn thiếu (user cấp trong round 9, chưa có giá trị thật):**

1. Smoke: set `HA_PROVIDER_ENDPOINT` + `HA_PROVIDER_MODEL` + `DEEPSEEK_API_KEY` (hoặc
   `HA_API_KEY`), rồi chạy `pwsh -NoProfile -File scripts/Smoke-HaProvider.ps1` (một turn
   có bound; script tự từ chối nếu thiếu và không thay fixture). Kết quả cần ghi vào mục 11
   của evidence, thay dòng `not_run`.
2. Publish: cài `gh` hoặc set `GH_TOKEN`; xác nhận việc push tag `ha-v0.1.0` lên
   `origin`; chạy `pwsh -NoProfile -File scripts/New-HaRelease.ps1 -SkipBuild -PublishDryRun`
   để xem channel, rồi mới publish thật và cập nhật evidence bằng URL thật (không tạo URL giả).


1. **Build release candidate** đúng revision đã test: bundle Windows x64 và (nếu có
   toolchain) Linux x64, kèm checksum sha256 và manifest nguồn; **không** kèm fixture
   executables hay test secrets.
2. **Installer end-user** dùng artifact candidate: staging → verify checksum → cài vào user
   bin (trong test: thư mục tạm) → ghi manifest → update/uninstall chỉ đụng file sở hữu.
3. **Clean-machine route**: máy/VM không có Rust/Git/Node — trong session này chỉ có thể
   mô phỏng bằng PATH tối thiểu + thư mục tạm, phải ghi rõ là mô phỏng, không phải VM thật.
4. **Publish: KHÔNG được cấp quyền.** Bàn giao candidate + checksum, ghi rõ "download route
   chưa public", không tạo URL giả.
5. **PTY**: nếu có môi trường có console thật (không phải sandbox này), chạy
   `cargo test -p harness-cli --test interactive_terminal -- --ignored --test-threads=1` để
   lấy transcript I01/I06/I07 và đóng nốt ca hard-kill của H05; ngược lại giữ not_run.
