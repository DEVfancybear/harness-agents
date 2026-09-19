# Bàn giao khởi động lại HA_LAUNCH

Tài liệu này là điểm vào cho lượt coding tiếp theo. Cập nhật sau mỗi checkpoint.

Provenance (commit/tree digest, digest executable, đường dẫn resolve, transcript, not_run và
việc tiếp theo) ở **mục 16 của evidence**: [evidence HA_LAUNCH](../evidence/HA_LAUNCH.vi.md).

> **Cảnh báo workspace dùng chung (cập nhật round 22)**: một writer khác vẫn đang sửa
> `crates/harness-cli/src/interactive/{app,controller,service}.rs` và thêm
> `service_completion_tests.rs` trong cây này (chưa commit). Round 22 đo được: các thay đổi đó
> **compile** và `cargo fmt --check` sạch, nhưng **một test của họ đang đỏ** —
> `interactive::service::completion_tests::completion_service_resume_flow` — làm bước
> `unit-interactive` của gate đỏ **tại chỗ** (67 test so với 61 test của HEAD). Đó không phải
> track HA_LAUNCH: `git grep completion_service_resume_flow HEAD` không thấy gì và
> `git ls-files …/service_completion_tests.rs` rỗng. Đừng `git add` chúng và **đừng sửa chúng**.
> Muốn số liệu sạch cho track này thì chạy gate trong worktree sạch
> `target/verify-round22` (`c386416`), như round 22 đã làm — evidence mục 19.

## 1. Ranh giới hiện tại

- **Round 22 (checkpoint re-verification, không viết code sản phẩm)**: gate chạy lại trong worktree
  sạch `c386416` → **`passed: true`, `failures: []`, 33/33 bước exit 0**; PTY **9/9** xanh ở cả cây
  chính lẫn worktree; release candidate **cũ 17 commit** đã được dựng lại ở đúng HEAD (mục 19.6);
  đường cài/gỡ end-user từ bundle được chạy thật đầu-cuối (mục 19.7). Không có mục `not_run` nào
  được nâng thành "đạt".
- **Round 23 (đóng một gap code, không đổi acceptance)**: project có thư mục **không đọc được** nay
  báo `storage_open_failed` kèm đường dẫn thay vì `workspace_escape` — làm theo RED → fix →
  regression, test RED dùng ACL thật (`icacls /deny …(R)`); `harness-tools` lib 3/3, `phase_p3` 20/20,
  `fmt` + `clippy -D warnings` xanh (evidence mục 20). Multiline editing **vẫn** để ngỏ, có ghi lý do.
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
  shadowing; `-SelfTest` **26 check** xanh (fresh-shell resolution qua cả CMD và PowerShell +
  đối chứng âm + chạy artifact đã cài trong môi trường dựng lại). Round 21 đóng nốt vế "I01 trên
  binary đã cài" bằng ca PTY `i14`. **Không** ghi User PATH thật và không cài vào vị trí thật
  của user (không được cấp quyền).
- **H07 xong phần gate + docs**: `scripts/Verify-HaLaunch.ps1` chạy xanh toàn bộ
  (format, clippy `-D warnings`, discovery selector bắt buộc, unit + acceptance + regression
  P0–P7 với `--test-threads=1`, installer self test, docs checker) và in rõ danh sách
  **not_run**. Operator guide (vi + en) đã có mục 12 với bảng migration cho hành vi
  non-TTY exit 2.
- **PTY thật (round 22 xác nhận lại): chín ca xanh ở cả hai cây.** `scripts/Invoke-HaPtyAcceptance.ps1
  -TimeoutSeconds 480` → `PTY_EXIT: 0`, `9 passed; 0 failed` (19.83 s cây chính, transcript
  `target/pty-acceptance/pty-all.txt`; 19.93 s worktree sạch `c386416`, transcript
  `target/verify-round22/target/pty-acceptance/pty-all.txt`), gồm i01, i05, i06, i07a, i07b, i08,
  i12, i13, i14. Điều kiện đo được: ConPTY chỉ hoạt động khi process tạo pseudo-console **sở hữu một
  console**, mà `cargo test` trong sandbox thì không — nên chín ca vẫn `#[ignore]` và phải chạy bằng
  runner đó; gate liệt kê chúng là `not_run` kèm hướng dẫn, không tính là pass tự động.
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

Assignment này **không** nêu quyền cho: sửa User PATH thật, cài binary thật lên máy
user, paid provider smoke, publish release, push remote. Prompt mẫu trong
HA_LAUNCH_PROMPT mục 4 không tự cấp quyền. Được cấp: sửa source trong repo, build/test
local, process con trong thư mục tạm, cài vào `-Destination` tạm, commit local (không push).
Round 22 **không** thay đổi bảng này: mọi việc đã làm đều nằm trong "được cấp", và bốn mục
"chưa cấp" vẫn giữ nguyên trạng thái — không có mục nào được tự suy ra quyền từ việc chỉ có
prompt mẫu.

| Checkpoint | Cần gì | Trạng thái quyền |
|---|---|---|
| H04 live smoke | credential + budget | **chưa cấp** → I10–I12 dùng HTTP fixture qua production adapter; live ghi `not_run` |
| H06 User PATH | quyền ghi User PATH thật | **chưa cấp** → test bằng fixture trong bộ nhớ + writer tiêm |
| H06/H08 cài thật | quyền cài lên máy user | **chưa cấp** → chỉ `-Destination` tạm và bundle local |
| H08 publish | quyền phát hành release | **chưa cấp trong assignment**; round 9 user chọn phương án (a) cấp quyền publish + credential, nhưng **đầu vào cụ thể vẫn thiếu** (`gh`/`GH_TOKEN` và xác nhận push tag `ha-v0.1.0`) → vẫn chỉ có release candidate + checksum local |

## 3. Việc tiếp theo chính xác

1. **Chờ user cấp hai đầu vào còn thiếu** (mục 6, "Bước 0"): credential cho live smoke và
   channel + xác nhận push tag cho publish. Không có chúng thì hai mục này giữ nguyên
   `not_run`, không được nâng thành "đạt", và cũng không được tạo URL giả.
2. **I19 — VM/máy sạch thật**: cần một môi trường không Rust/Git/Node và không source repo.
   Trong session này chỉ có môi trường tái tạo (self test) nên I19 giữ "một phần".
3. **Việc code còn lại (không chặn acceptance)**: multiline editing trong editor; và hành vi
   `apply_patch` khi project có file ghi được nhưng thư mục chỉ đọc — hiện **đã đo** là thất bại
   trong lúc walk workspace với mã `workspace_escape` nhưng **chưa** thành test; nếu muốn đóng
   thì viết test trước rồi mới sửa.
4. Giữ nguyên luật nền tảng đã ghi: một input mỗi session, mỗi lượt một writer generation.
5. Trước khi tin một lần gate đỏ: kiểm `git status` xem writer khác có đang sửa `crates/**`
   không (cảnh báo đầu tài liệu). **Đừng chạy gate trong một worktree kèm `CARGO_TARGET_DIR`
   trỏ sang cây khác** — `phase_p7` hardcode `target/debug` theo repo nên sẽ đỏ giả (round 22
   đã mắc và đã xác minh lại: chạy không override → 1 passed; evidence mục 19.4).

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

### 3c. Trạng thái bàn giao của round 22 (đọc trước khi commit)

Round 22 **không tạo commit nào**. Lý do đo được, không phải sở thích: ba file tài liệu của track
(`docs/{evidence,handoffs,specs}/HA_LAUNCH.vi.md`) **đã có thay đổi chưa commit của writer khác**
trong cùng cây, nên `git add` chúng sẽ kéo luôn phần không thuộc track vào commit này. Vì vậy:

- Cây làm việc hiện chứa: thay đổi code chưa commit của writer khác (`crates/harness-cli/src/interactive/*`),
  tài liệu H đã cập nhật của round này, và hai file chưa được track của writer kia.
- Worktree dùng để đo là `target/verify-round22` (detached tại `c386416`, `git status` sạch). Đây
  **chỉ là chỗ đo**, không phải nhánh bàn giao — đừng trỏ người nhận vào đó như một commit.
- Việc cần làm khi có người sở hữu cây: tách phần tài liệu H ra khỏi thay đổi của writer khác rồi
  commit theo từng checkpoint; provenance hiện tại vẫn là **working tree**, chưa phải commit.

Lưu ý kỹ thuật đã biết: fixture loopback trong môi trường này flaky khi test chạy song
song (mục 14 evidence) — test HTTP fixture chạy với `--test-threads=1`, và **không** sửa
acceptance P2 đã được chấp nhận.

## 4. Trạng thái test ở checkpoint này

Số liệu dưới đây là **đo được ở round 22**, lấy từ gate trong worktree sạch `c386416`
(log `target/gate-round22-worktree-local.txt`); cây chính lệch đúng phần chưa commit của
writer khác (xem cảnh báo đầu tài liệu).

| Suite | Kết quả round 22 (worktree sạch `c386416`) | Cây chính |
|---|---|---|
| `cargo test -p harness-cli --bin ha --locked` | **61 passed**, 0 failed | 67 passed ở lần chạy 2, trong đó 1 test của writer khác **đỏ** (`completion_service_resume_flow`) |
| `cargo test -p harness-cli --test interactive_launch --locked -- --test-threads=1` | **18 passed**, 0 failed | — |
| `cargo test -p harness-cli --test interactive_session --locked -- --test-threads=1` | **9 passed**, 0 failed | — |
| `cargo test -p harness-providers --locked -- --test-threads=1` | passed (bước `providers-streaming` exit 0, 22 s) | exit 0 |
| `regression-phase_p0…p7` (serial) | **cả tám xanh** (p3 43 s, p5 46 s, p6 39 s) | p3 và p5 **đỏ vì sandbox chặn `sh.exe`/named pipe** (mục 19.2) |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0 (45 s) | exit 0 |
| `pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1` | **`passed: true`, `failures: []`, 33/33 bước exit 0** | lần 1: ba bước đỏ do sandbox; lần 2 (bỏ hạn chế): chỉ `unit-interactive` đỏ |
| `Invoke-HaPtyAcceptance.ps1 -TimeoutSeconds 480` | **`PTY_EXIT: 0`, 9 passed, 0 failed** (19.93 s) | **`PTY_EXIT: 0`, 9 passed, 0 failed** (19.83 s) |
| `Install-Ha.ps1 -SelfTest` | **26** check, `INSTALL_SELFTEST_OK` | exit 0 |
| `New-HaRelease.ps1` (bundle tại HEAD) | manifest `build_commit c386416`, `sha256 ha.exe 7bfa01…`, `published: false` | — |
| Cài + gỡ từ bundle (I19 mô phỏng / I20) | exit 0 hai chiều; digest khớp; chỉ 2 file sở hữu bị xoá; user data + file lạ còn | — |
| `ha` resolve trong shell mới (PATH dựng lại, không toolchain) | PowerShell `Get-Command ha` + CMD `where.exe ha` đều trỏ binary đã cài; `cargo` không tồn tại trong PATH đó | — |
| `cargo test -p harness-cli --test interactive_terminal` | **9 ignored** khi chạy bằng `cargo test` (cần console thật) — chạy bằng runner thì `9 passed` | như trái |

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
