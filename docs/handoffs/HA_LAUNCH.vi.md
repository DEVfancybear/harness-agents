# Bàn giao khởi động lại HA_LAUNCH

Tài liệu này là điểm vào cho lượt coding tiếp theo. Cập nhật sau mỗi checkpoint.

Provenance (commit/tree digest, digest executable, đường dẫn resolve, transcript, not_run và
việc tiếp theo) ở **mục 16 của evidence**: [evidence HA_LAUNCH](../evidence/HA_LAUNCH.vi.md).

> **Workspace dùng chung — đã giải quyết ở round 23.** Cây **đã sạch**: thay đổi của writer khác
> (`crates/harness-cli/src/interactive/{app,controller,service}.rs` + `service_completion_tests.rs`)
> cùng toàn bộ việc của track đã được commit ở **`893afc0`** và **push lên `origin/master`** theo
> yêu cầu của user. Round 23 đo được test của writer kia **nay xanh** (`completion_service_resume_flow`,
> `unit-interactive` 70 passed), nên cây commit được là cây xanh — trước đó ở round 22 nó đỏ và vì
> thế số liệu phải lấy từ worktree sạch. Ghi lại để lượt sau không phải suy: commit này **gộp cả**
> phần của writer kia, và điều đó là quyết định có ý thức, không phải vô tình `git add -A`.

## 1. Ranh giới hiện tại

- **Round 25 — CI XANH 12/12 (`cb12940`).** Sau round 24, CI đỏ lại vì track TUI của writer khác:
  lần này **clippy `-D warnings`** chặn trước khi test chạy — `unused variable` và hai variant
  chỉ-dựng-trên-Windows trong `interactive/credentials.rs`, cùng một test barrier vượt 100 dòng trong
  `providers/streaming.rs`. Sửa ở `cb12940` (allow đối xứng `cfg_attr(unix, …)`, và tách fixture của
  test barrier thành helper có tên, giữ nguyên assertion). Run `cb12940` → **12/12 success**.
  Chi tiết: mục 23.1 evidence.
- **Một flake CÒN ĐÓ, chưa sửa:** `providers::streaming::g1_adapter_delivers_text_before_the_response_completes`
  tự nó đỏ khoảng **1/4 lần** với `provider stream failed: error decoding response body`, và mất
  ~165 s khi đỏ. Đã đo trên revision gốc nên **không** do round 25. Nếu CI đỏ lại kèm thông điệp đó
  thì đây là nghi phạm đầu tiên — đọc tên test trước khi suy đoán gì khác.
- **Round 24 — CI đã XANH 12/12 job.** Trước đó mọi commit từ `9bbd632` đều đỏ. Nguyên nhân thật
  (đọc từ log CI sau khi user cài `gh`) là **test không portable**, không phải flake loopback như
  tôi kết luận sai ở round 23: (a) tempdir trên runner Windows giữ tên 8.3 nên so raw-vs-canonical
  sai; (b) Linux dừng ở `config_read_error` còn Windows ở `storage_open_failed` cho cùng một root
  hỏng; (c) fixture ACL (`icacls`+`USERNAME`) là Windows-only, gồm cả test read-denial trong
  `harness-tools`. Sửa ở `01a7bee` + `ed22df4`; run `35459853068` → **12/12 success**. Chi tiết:
  mục 23 evidence.
- **Round 23 (tiếp)**: hai gap còn lại của handoff **đã đóng** — multiline editing (Enter gửi,
  Ctrl-J chèn dòng, prompt nhiều dòng, con trỏ theo ký tự/row) và guard PATH của `-Uninstall`
  (`-RemoveUserPathEntry`; self test 26 → **28 check**). Gate đầy đủ trên cây đã sửa:
  **`passed: true`, `failures: []`, 30 bước, 0 nonzero**, và PTY thật **10/10** xanh trong một lần
  chạy (`PTY_EXIT: 0`, 20.18 s). Giới hạn **đo được** của multiline trên Windows ConPTY được ghi
  thẳng ở mục 21.1 evidence, không tô hồng.
- **Round 23 (gap đầu)**: project có thư mục **không đọc được** nay báo `storage_open_failed` kèm
  đường dẫn thay vì `workspace_escape` — RED → fix → regression, test RED dùng ACL thật
  (`icacls /deny …(R)`); `harness-tools` lib 3/3, `phase_p3` 20/20, `fmt` + `clippy -D warnings` xanh
  (evidence mục 20).
- **Round 22 (checkpoint re-verification, không viết code sản phẩm)**: gate chạy lại trong worktree
  sạch `c386416` → **`passed: true`, `failures: []`, 33/33 bước exit 0**; PTY **9/9** xanh ở cả cây
  chính lẫn worktree; release candidate **cũ 17 commit** đã được dựng lại ở đúng HEAD (mục 19.6);
  đường cài/gỡ end-user từ bundle được chạy thật đầu-cuối (mục 19.7). Không có mục `not_run` nào
  được nâng thành "đạt".
- **Round 23 (đóng một gap code, không đổi acceptance)**: project có thư mục **không đọc được** nay
  báo `storage_open_failed` kèm đường dẫn thay vì `workspace_escape` — làm theo RED → fix →
  regression, test RED dùng ACL thật (`icacls /deny …(R)`); `harness-tools` lib 3/3, `phase_p3` 20/20,
  `fmt` + `clippy -D warnings` xanh (evidence mục 20).
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
  `/resume` liệt kê/chọn session của project, `/new` không bỏ chạy ngầm, headless
  `--resume <session-id>` tiếp tục task với context phục hồi, và `/exit` giữa lúc run đang
  chạy thì cancel + thoát 0 + nhả store. Cả hai nửa kill **process thật** đều đã đo: giữa lượt
  gọi model (round 14) và **sau khi tool receipt đã commit** (round 16, mục 15.2 evidence).
- **H06 xong phần code**: installer dùng artifact Cargo báo + digest + manifest, thay thế có
  staging/rollback, phân loại lỗi khóa file, User PATH merge tách biệt có test, cảnh báo
  shadowing; `-SelfTest` **28 check** xanh (fresh-shell resolution qua cả CMD và PowerShell +
  đối chứng âm + chạy artifact đã cài trong môi trường dựng lại; round 23 thêm hai ca cho guard
  PATH của `-Uninstall`). Round 21 đóng nốt vế "I01 trên
  binary đã cài" bằng ca PTY `i14`. **Không** ghi User PATH thật và không cài vào vị trí thật
  của user.
- **H07 xong phần gate + docs**: `scripts/Verify-HaLaunch.ps1` chạy xanh toàn bộ
  (format, clippy `-D warnings`, discovery selector bắt buộc, unit + acceptance + regression
  P0–P7 với `--test-threads=1`, installer self test, docs checker) và in rõ danh sách
  **not_run**. Operator guide (vi + en) đã có mục 12 với bảng migration cho hành vi
  non-TTY exit 2.
- **PTY thật (round 23): mười ca xanh trong một lần chạy.** `scripts/Invoke-HaPtyAcceptance.ps1
  -TimeoutSeconds 600` → `PTY_EXIT: 0`, **10 passed; 0 failed** (20.18 s, transcript
  `target/pty-acceptance/pty-all.txt`; log `target/pty-round23-full2.txt`), gồm i01, i05, i06, i07a,
  i07b, i08, i12, i13, i14 và ca mới **i21** (multiline). Điều kiện đo được: ConPTY chỉ hoạt động khi
  process tạo pseudo-console **sở hữu một console**, mà `cargo test` trong sandbox thì không — nên
  các ca vẫn `#[ignore]` và phải chạy bằng runner đó; gate liệt kê chúng là `not_run` kèm hướng dẫn,
  không tính là pass tự động.
- **H08 xong phần code**: bundle candidate + checksum + manifest (`published:false`), installer
  `-FromBundle` (verify trước khi cài) và `-Uninstall` (chỉ xóa file sở hữu, giữ user data),
  cùng chứng minh disposable I19/I20. Round 22 dựng lại candidate ở **đúng HEAD** (candidate cũ
  dựng ở `1eceeef`, cũ 17 commit) và chạy đường cài/gỡ đầu-cuối trên candidate mới — evidence
  mục 19.6/19.7. **Không publish** và chưa có VM sạch thật.
- Phát hiện nền tảng quan trọng: journal P1 chỉ cho **một input mỗi session** và task lease
  cần **generation mới** — nên "cùng phiên" = cùng task + chuỗi session nối nhau, không
  phải một session nhiều input (chi tiết + test ở mục 5.3 evidence và mục 5 SPEC).
- **I08 đã có bằng chứng** (fault seam chỉ ở debug build + 3 unit test phục hồi mode + ca PTY
  `i08` thoát 1 kèm lỗi nêu tên, không treo).

Bằng chứng: mục 2–4 của [evidence HA_LAUNCH](../evidence/HA_LAUNCH.vi.md). Quyết định:
mục 5 của [SPEC HA_LAUNCH](../specs/HA_LAUNCH.vi.md).

## 2. Quyền: cái gì được và không được cấp

**Cập nhật round 23 — user đã cấp thêm quyền trong hội thoại:** commit **và push**, sửa gap
`-Uninstall` (liên quan User PATH), cùng các mục trước đây `not_run` (paid smoke, publish, cài thật).
Bảng dưới đây ghi trạng thái **thực tế sau khi cấp quyền**, gồm cả việc quyền đã có nhưng **đầu vào
kỹ thuật vẫn thiếu** — cấp quyền không tự tạo ra credential.

| Checkpoint | Cần gì | Trạng thái |
|---|---|---|
| H04 live smoke | credential + budget | **quyền: đã cấp (round 23)**; **đầu vào: thiếu** — `HA_PROVIDER_ENDPOINT`, `HA_PROVIDER_MODEL`, `DEEPSEEK_API_KEY`/`HA_API_KEY` đều chưa set → `Smoke-HaProvider.ps1` in `SMOKE_NOT_RUN`, exit 2, **không** thay fixture và **không** gọi trả phí |
| H06 User PATH | quyền ghi User PATH thật | **quyền: đã cấp (round 23)**; chưa thực hiện vì nó gắn với "cài thật" bên dưới, và mọi thứ cần chứng minh đã chứng minh được bằng provider/writer tiêm |
| H06/H08 cài thật | quyền cài lên máy user | **quyền: đã cấp (round 23)**; **chưa thực hiện** — chưa có yêu cầu chạy cụ thể và `-SelfTest` vẫn là đường không đụng máy user |
| H08 publish | quyền phát hành release | **quyền: đã cấp (round 23)**; **đầu vào: thiếu** — `gh` không có trong máy và `GH_TOKEN`/`GITHUB_TOKEN` chưa set, nên `New-HaRelease.ps1 -PublishDryRun` báo `channel: NOT available` và **không** publish gì |
| Push remote | quyền push | **đã cấp và đã thực hiện (round 23)**: `9bbd632..893afc0 master -> master`, exit 0 |

Fail-closed vẫn giữ: ở đâu đầu vào thiếu thì ở đó `not_run`, không nâng thành "đạt", và **không**
tạo URL/credential giả.

## 3. Việc tiếp theo chính xác

1. **Hai đầu vào còn thiếu** (mục 6, "Bước 0"): (a) credential cho live smoke — script đã sẵn và tự
   từ chối khi thiếu; (b) channel publish — cài `gh` hoặc set `GH_TOKEN`, rồi xác nhận push tag
   `ha-v0.1.0`. Quyền đã có, chỉ còn giá trị thật.
2. **I19 — VM/máy sạch thật**: cần một môi trường không Rust/Git/Node và không source repo. Máy này
   **không** có đường nào: không elevated (Docker/Windows Sandbox cần admin), `wsl --list` báo WSL
   chưa cài, không có VBoxManage/vmrun. I19 giữ "một phần" và mọi kết quả cài đặt phải đọc là mô phỏng.
3. **Việc code còn lại**: đường **paste giữ newline** (hiện `normalize_paste` đổi newline thành
   space, nên multiline trên Windows — nơi Ctrl-J không dùng được — vẫn chưa có đường vào). Đây là
   thay đổi hành vi có test riêng (`h03_editor_paste_never_submits_multiple_commands`), nên làm như
   một mục riêng.
4. Giữ nguyên luật nền tảng đã ghi: một input mỗi session, mỗi lượt một writer generation.
5. Trước khi tin một lần gate đỏ: kiểm `git status` trước (round 23 đã commit nên cây sạch).
   **Đừng chạy gate trong một worktree kèm `CARGO_TARGET_DIR` trỏ sang cây khác** — `phase_p7`
   hardcode `target/debug` theo repo nên sẽ đỏ giả (round 22 đã mắc và đã xác minh lại: chạy không
   override → 1 passed; evidence mục 19.4).
6. **Nếu một test có vẻ không được build lại**: round 23 gặp `target/debug/.fingerprint` của
   `harness-cli` bị kẹt nên `cargo test` báo `Fresh` dù source mới hơn, và test mới **không** xuất
   hiện trong `--list`. Cách chữa đã dùng: xoá `target/debug/.fingerprint/harness-cli-*` rồi build
   lại. Đừng tin "0 failed" khi số test không khớp với số `#[test]` trong source.

## 3b. Trạng thái gate từng checkpoint (round 22, tại `c386416`)

Gate chạy trong worktree sạch `target/verify-round22` (`git status --porcelain` = 0 dòng):
`pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json` → **`passed: true`, `failures: []`,
33/33 bước exit 0, 13 required selector đều được discovery thấy**.

| Checkpoint | Phạm vi | Prerequisite | Kết quả gate round 22 |
|---|---|---|---|
| A | H01–H03: dispatch, launch context, terminal app | — | **đạt**: `unit-interactive` 61 test, `acceptance-launch` 18 test, PTY i01/i06/i07a/i07b/i08 xanh trên console thật |
| B | H04–H05: provider thật, tool loop, approval, resume/cancel | A | **đạt**: `acceptance-session` 9 test, `providers-streaming`, PTY i05/i12/i13 xanh; live smoke `not_run` (chưa cấp credential) |
| C | H06–H07: installer, command resolution, gate + docs | A, B | **đạt**: `installer-selftest` 26 check, `release-selftest`, `docs`, PTY i14 xanh; resolve `ha` qua PowerShell **và** CMD trong env dựng lại (mục 19.7) |
| D | H08: release candidate + clean-machine route | C | **đạt phần code**: candidate dựng lại ở đúng HEAD, cài/gỡ từ bundle chạy thật (I20 + I19 mô phỏng); **không publish** (không được cấp quyền), **I19 vẫn "một phần"** vì không có VM sạch |

Không checkpoint nào được nâng trạng thái dựa trên một bước chưa chạy: `not_run` của gate vẫn là
PTY (cần console thật, chạy bằng runner riêng), live smoke, Linux, và ghi User PATH/cài lên máy user.

**Bổ sung round 23 (sau bảng trên).** Một thay đổi source mới nằm trong phạm vi của checkpoint B
(`crates/harness-tools/src/workspace.rs`, đường approval gate + tool walk): mã lỗi cho thư mục không
đọc được đổi từ `workspace_escape` sang `storage_open_failed`. Provenance của thay đổi này **đã có**:
gate đầy đủ chạy lại một lần từ đầu tới cuối trên cây đã sửa → **`passed: true`, `failures: []`,
30 bước, 0 bước nonzero, đủ 13 required selector** (`target/gate-round23.txt`), và PTY **9/9** xanh
kèm transcript (`target/pty-round23.txt`). Bảng checkpoint A–D ở trên vẫn gắn với revision `c386416`
đã đo ở round 22; cây hiện tại = `c386416` + fix này.

### 3c. Trạng thái bàn giao (cập nhật round 23 — **đã commit và push**)

Round 23 đóng nốt việc commit theo yêu cầu của user:

| Mục | Giá trị |
|---|---|
| Commit | **`893afc0`** `feat(ha-launch): checkpoint gates, multiline editing, uninstall PATH guard (HA_LAUNCH H01-H08)` |
| Nội dung | 15 file: code H (interactive + `harness-tools`), `scripts/Install-Ha.ps1`, 3 tài liệu H, `docs/specs/HA_CLI_COMPLETION.vi.md`, `service_completion_tests.rs` |
| Push | **`9bbd632..893afc0  master -> master`**, exit 0; `git rev-list --left-right --count origin/master...HEAD` = `0 0` |
| Cây sau push | `git status --porcelain` **sạch** |

Ghi thẳng một điều để lượt sau không phải suy: commit này **gộp cả** thay đổi của writer khác
(`crates/harness-cli/src/interactive/{app,controller,service}.rs` + `service_completion_tests.rs`),
vì phần của họ nằm xen trong cùng file với phần của track và cây đã xanh hoàn toàn. Đó là quyết định
có ý thức theo yêu cầu "commit and push", không phải vô tình `git add -A`. Nếu cần tách lịch sử
theo track thì phải làm lại từ `c386416`, không phải sửa tiếp trên commit này.

Worktree `target/verify-round22` (detached tại `c386416`) **chỉ là chỗ đo**, không phải nhánh bàn giao.

Lưu ý kỹ thuật đã biết: fixture loopback trong môi trường này flaky khi test chạy song
song (mục 14 evidence) — test HTTP fixture chạy với `--test-threads=1`, và **không** sửa
acceptance P2 đã được chấp nhận.

## 4. Trạng thái test ở checkpoint này

Số liệu dưới đây là **đo được ở round 23 trên cây đã commit** (`893afc0`), lấy từ gate đầy đủ
(log `target/gate-round23-full.txt`) và runner PTY (`target/pty-round23-full2.txt`).

| Suite | Kết quả round 23 (cây `893afc0`, sạch) |
|---|---|
| `cargo test -p harness-cli --bin ha --locked` (qua gate) | **70 passed**, 0 failed (gồm 3 unit test multiline mới) |
| `cargo test -p harness-cli --test interactive_launch` (serial, qua gate) | **18 passed**, 0 failed |
| `cargo test -p harness-cli --test interactive_session` (serial, qua gate) | **9 passed**, 0 failed |
| `cargo test -p harness-providers --locked -- --test-threads=1` | exit 0 |
| `regression-phase_p0…p7` (serial) | **cả tám xanh** |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0 |
| `pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1` | **`passed: true`, `failures: []`, 30 bước, 0 nonzero, đủ 13 required selector** |
| `Invoke-HaPtyAcceptance.ps1 -TimeoutSeconds 600` | **`PTY_EXIT: 0`, 10 passed, 0 failed** (20.18 s) |
| `Install-Ha.ps1 -SelfTest` | **28** check, `INSTALL_SELFTEST_OK` |
| `New-HaRelease.ps1 -SkipBuild -PublishDryRun` | `channel: NOT available` (không có `gh`/`GH_TOKEN`) — không publish gì |
| `Smoke-HaProvider.ps1` | `SMOKE_NOT_RUN`, exit 2 (thiếu credential) — không gọi trả phí |
| `cargo test -p harness-tools --lib` | **3 passed** (gồm test RED read-only nay xanh) |
| `cargo test -p harness-cli --test interactive_terminal` | **10 ignored** khi chạy bằng `cargo test` (cần console thật) — chạy bằng runner thì `10 passed` |

Hai lần đo đáng ghi vì suýt bị đọc sai: (a) lần chạy PTY đầy đủ **đầu tiên** của round 23 đỏ ở
`i05` với `provider request failed ... 127.0.0.1` — chạy riêng **1 passed trong 0.87 s**, đúng loại
flake loopback ở mục 14, và lần chạy lại đủ 10 ca thì xanh; (b) `cargo test` từng báo `Fresh` cho
`harness-cli` dù source mới hơn, làm 3 test mới **không** được build — xem mục 3.6.

### 4b. Flake loopback hiện **nặng** — đọc trước khi tin một lần gate đỏ

Sau khi commit, gate đỏ ở `acceptance-launch::i13_resume_continues…`, rồi lần sau đỏ ở
`regression-phase_p2`. Cả hai đã được truy nguyên bằng đo đối chứng, **không** phải hồi quy:

| Phép đo | Kết quả |
|---|---|
| `i13_resume_continues` riêng, cây đã commit, 3 lần | 2 đỏ (6.43/6.44 s) / 1 xanh (0.70 s) |
| Cùng test trong worktree sạch `c386416`, 3 lần | 2 đỏ (6.45/6.44 s) / 1 xanh (0.65 s) |
| `phase_p2` riêng sau khi gate đỏ | 17 passed, 0 failed (1.19 s) |

Tỉ lệ hỏng và con số thời gian **giống nhau ở revision gốc**, nên không phải do round 23. Chế độ
hỏng có hai mức thời gian tách biệt sẽ (~0.7 s) và hỏng (~6.4 s), tức client chờ hết timeout rồi bỏ
cuộc. Xem mục 22 evidence để có cơ chế đầy đủ và lý do **không** sửa test trong round này.

Hệ quả thực hành: gate trên máy này **xanh không ổn định**. Ba lần chạy liên tiếp cho **ba bước đỏ
khác nhau** (`providers-streaming`, `acceptance-launch`, `unit-interactive`), và mỗi lần suite hỏng
đều chậm bất thường — 203 s so với 38 s, 10.4 s so với 0.8 s — tức chờ timeout. Cả ba suite đó
**xanh khi chạy riêng** ngay sau đó (`--bin ha` 3/3 lần 70 passed; `phase_p2` 17 passed; `i13` riêng
có lần xanh 0.64 s). Đừng kết luận "track hỏng" từ một lần đỏ, và cũng **đừng** bỏ qua: chạy lại,
ghi lại từng lần, chỉ nhận trạng thái xanh khi `failures: []`, và **không** sửa test để làm nó xanh.

- Chưa chạy: Linux (chỉ có target `x86_64-pc-windows-msvc`), live provider smoke, publish, VM
  sạch thật, ghi User PATH thật/cài lên máy user.

## 5. Gap đã biết cần đóng

- **I19**: chưa có VM/máy sạch thật; hiện chỉ mô phỏng PATH tối giản + artifact đã cài trong
  thư mục tạm (self test, evidence mục 15.3). Đây là **mục duy nhất còn "một phần"** trong bảng
  I01–I20 (evidence mục 15).
- Multiline editing trong editor (đã ghi là giới hạn một dòng). Round 23 đọc lại đường code:
  `map_key` chỉ map Enter/Letters/Editing keys, `KeyCode::Char` bắt **mọi** ký tự nên `Shift+Enter`
  hiện thành `Key::Char('\n')`; muốn mở multiline phải sửa `terminal.rs` (nhận biết phím combo) +
  `input.rs` (buffer nhiều dòng, renderer biết vẽ prompt nhiều dòng) và cần **PTY thật** để chứng
  minh. Với assignment này (không cấp thêm quyền, và bằng chứng PTY phải chạy lại toàn bộ) đây là
  việc của một lượt riêng, không phải một thay đổi nhỏ — nên **chưa** làm.
- ~~Hành vi `apply_patch` trên project có thư mục chỉ đọc~~ — **đã đóng ở round 23**: test RED
  `workspace::tests::review_unreadable_directory_is_reported_as_a_read_failure_with_the_path` (ACL
  thật `icacls /deny …(R)`), `walk_files` nay trả `storage_open_failed` kèm đường dẫn cho
  `PermissionDenied` và **giữ** `workspace_escape` cho escape thật; `phase_p3` 20/20, `fmt` và
  `clippy -D warnings` xanh (evidence mục 20).
- **Mới, round 22**: `Install-Ha.ps1 -Uninstall` xoá PATH entry đã ghi mà **không** cần cờ tường
  minh như đường cài (`-ModifyUserPath`), và self test chỉ chạy ca `AddedPathEntry = ''` nên nhánh
  xoá thật chưa được phủ. Không sửa trong round này vì assignment không cấp quyền ghi User PATH
  (mục 19.9).
- Provider endpoint/model resolution (H04) và credential resolver ngoài env var.
- Live smoke và publish: chờ quyền/đầu vào (mục 2 và mục 6), không phải việc code.

Bảng đối chiếu acceptance I01–I20 (mục 15 evidence): **chỉ còn I19 ở mức một phần**. Round 15
đóng I04 (bản cài dưới path Unicode + hai caller directory không Git → hai store) và I09 (data
root không dùng được → lỗi nêu đường dẫn, giữ mã `storage_open_failed`, không tạo state). Round
16 đóng I13 (PTY: patch chạy thật + receipt commit rồi mới kill cứng; process mới `--resume`
không chạy lại, file không đổi, vẫn một receipt). Round 21 đóng vế "I01 trên binary đã cài" của
I14 bằng ca PTY `i14`. I04/I09/I13 là selector bắt buộc của gate; I14 là ca PTY trong runner
(gate ghi `not_run` kèm hướng dẫn vì sandbox không có console).

## 6. Việc tiếp theo chi tiết (H08)

**Bước 0 — hai input còn thiếu (user cấp trong round 9, chưa có giá trị thật):**

1. Smoke: set `HA_PROVIDER_ENDPOINT` + `HA_PROVIDER_MODEL` + `DEEPSEEK_API_KEY` (hoặc
   `HA_API_KEY`), rồi chạy `pwsh -NoProfile -File scripts/Smoke-HaProvider.ps1` (một turn
   có bound; script tự từ chối nếu thiếu và không thay fixture). Kết quả cần ghi vào mục 11
   của evidence, thay dòng `not_run`.
2. Publish: cài `gh` hoặc set `GH_TOKEN`; xác nhận việc push tag `ha-v0.1.0` lên
   `origin`; chạy `pwsh -NoProfile -File scripts/New-HaRelease.ps1 -SkipBuild -PublishDryRun`
   để xem channel, rồi mới publish thật và cập nhật evidence bằng URL thật (không tạo URL giả).

Các bước H08 còn lại (phần code đã xong, chỉ chờ hai input trên):

1. **Build release candidate** đúng revision đã test: bundle Windows x64 và (nếu có
   toolchain) Linux x64, kèm checksum sha256 và manifest nguồn; **không** kèm fixture
   executables hay test secrets.
2. **Installer end-user** dùng artifact candidate: staging → verify checksum → cài vào user
   bin (trong test: thư mục tạm) → ghi manifest → update/uninstall chỉ đụng file sở hữu.
3. **Clean-machine route**: máy/VM không có Rust/Git/Node — trong session này chỉ có thể
   mô phỏng bằng PATH tối thiểu + artifact đã cài trong thư mục tạm, phải ghi rõ là mô phỏng,
   không phải VM thật.
4. **Publish: KHÔNG được cấp quyền.** Bàn giao candidate + checksum, ghi rõ "download route
   chưa public", không tạo URL giả.
5. **PTY**: đã chạy trên console thật ở round 21:
   `pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1 -TimeoutSeconds 480` →
   `PTY_EXIT: 0`, 9 passed, 0 failed (gồm `i14`: chính artifact đã cài mở app).
