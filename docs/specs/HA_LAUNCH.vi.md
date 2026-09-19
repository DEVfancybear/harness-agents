# Đặc tả triển khai HA_LAUNCH — gõ `ha` để mở CLI tương tác

Trạng thái: **đặc tả triển khai đang hiệu lực cho track H01–H08**, theo
[plan HA_LAUNCH](../HA_LAUNCH_PLAN.vi.md). SPEC này ghi quyết định của lượt coding,
không thay acceptance I01–I20 và không thay plan.

## 0. Quyền được giao trong assignment này

Assignment: *"Triển khai H01–H08 theo plan HA_LAUNCH, thực hiện theo dependencies
và gate từng checkpoint; không chuyển tiếp khi prerequisites chưa đạt."* Prompt mẫu
trong [HA_LAUNCH_PROMPT.vi.md](../HA_LAUNCH_PROMPT.vi.md) mục 4 **không** tự cấp
quyền; mục 12 của plan cũng ghi rõ bộ tài liệu không tự cấp quyền.

| Hành động | Trạng thái trong assignment này | Hệ quả lên H06–H08 |
|---|---|---|
| Đọc/ghi source trong repo, build và chạy test local | **Được cấp** (là chính công việc) | H01–H08 code + test local |
| Tạo tiến trình con trong thư mục tạm, cài vào `-Destination` tạm | **Được cấp** (test installer, không đụng máy user) | I14/I17/I18 chạy được |
| Ghi **User PATH** thật (HKCU/registry) | **Không được cấp** | H06 kiểm chứng bằng fixture trong bộ nhớ + registry state tổng hợp, không mutate User PATH thật |
| Cài binary thật lên máy user (ngoài thư mục tạm) | **Không được cấp** | H06/H08 chỉ cài vào đích tạm; fresh-shell `ha` trên máy này không được tuyên bố |
| Paid provider smoke (gọi model trả phí) | **Không được cấp** | I10–I12 dùng HTTP fixture qua production adapter; live smoke ghi `not_run` |
| Publish release/artifact/tag | **Không được cấp** | H08 bàn giao release **candidate** + checksum; không publish, không invent URL |
| Push source lên remote | **Không được cấp** | Chỉ commit local cho từng checkpoint; không push |

Fail-closed: khi một checkpoint cần quyền chưa có, checkpoint đó dừng ở trạng thái
`blocked_on_authority` trong handoff, không được mô tả là đã đạt.

## 1. Prerequisite và source boundary

Base revision `d6a346951197e53d931fc572dd05c42a3c3fe002` (`docs: plan installed ha
interactive startup for DeepSeek`), working tree sạch, nhánh `master`. Revision
khảo sát của plan là `547f0bb`; plan được viết ở revision `d6a3469` nhưng chưa có
dòng code H nào.

Xác minh baseline trước khi sửa (lượt coding này):

- `cargo test --workspace --no-run` dựng được toàn bộ test binary của workspace
  trên Windows/rustc 1.97.1 — digest và log ghi ở evidence.
- `crates/harness-cli/src/main.rs` có `None => Ok(())` tại dòng 392, xác nhận
  hành vi bare `ha` thoát ngay đúng như plan mô tả.
- Binary `ha` đã tồn tại trong `crates/harness-cli/Cargo.toml`
  (`[[bin]] name = "ha"`), nên H01 không đổi tên binary và không tạo alias.

Track H triển khai **trên CLI hiện tại**, không tạo `vnext/`, không đổi sang
`ha-next`. Các subcommand cũ (`memory`, `init`, `config`, `sessions`, `status`,
`plugins`, `run`, `resume`, `continue`, `context`, `session`, `code`, `tasks`,
`extensions`, `maintenance`) giữ nguyên parser, JSON và exit semantics; đây là
compatibility list được regression ở H01 và H07.

## 2. Dispatch contract (H01)

TTY được phát hiện qua trait inject được, để unit/integration test không cần
terminal thật:

| Invocation | stdin/stdout là terminal | Không là terminal |
|---|---|---|
| `ha` (không tham số) | Vào interactive app, chờ nhập, chỉ thoát khi user thoát | In hướng dẫn ngắn ra **stderr**, exit **2**, không treo |
| `ha chat` | Cùng entrypoint với bare `ha` | Như bare `ha` (exit 2 + hướng dẫn) |
| `ha chat --cwd <path>` | Mở project ở path resolve từ caller cwd | exit 2 + hướng dẫn |
| `ha chat --resume <session-id>` | Resume trong interactive app | exit 2 + hướng dẫn |
| `ha chat --headless --prompt <text> [--json]` | Một turn, kết quả ra stdout, log ra stderr | Như trái — đây là đường được hướng đến |
| `ha --help`, `ha --version` | Fast path: không config/provider/store/network | Như trái |
| Subcommand cũ | Giữ dispatch hiện hành, không tự mở interactive app | Như trái |

Quy tắc conflict do clap ép ở parser (exit 2): `--headless` bắt buộc `--prompt`;
`--prompt` và `--json` chỉ hợp lệ cùng `--headless`; unknown option vẫn là parser
error và **không** được coi là prompt. Không thêm `ha "free text"`.

Exit code: `0` thành công, `1` lỗi runtime/IO, `2` usage hoặc non-TTY guard. Guard
non-TTY in hướng dẫn rồi trả `Ok(ExitCode::from(2))` — thoát có chủ đích, không
phải lỗi bị nuốt, và process không chờ stdin.

## 3. Kiến trúc module (H01–H03)

```text
main dispatch (crates/harness-cli/src/main.rs)
  -> interactive::launch(LaunchMode)
       Interactive -> detector -> bootstrap (H02) -> app/controller (H03)
       Headless    -> application service (H04)
```

| Module | Owner | Nội dung |
|---|---|---|
| `interactive/mod.rs` | H01 | `LaunchMode`, dispatch, exit code, guard non-TTY |
| `interactive/detector.rs` | H01 | `TerminalDetector` trait, real detector (`std::io::IsTerminal`), fixture detector cho test |
| `interactive/app.rs` | H01→H03 | boot header + prompt loop; H03 thay bằng controller/renderer |
| `interactive/paths.rs` | H02 | resolve user config/data, `HA_HOME` override, precedence |
| `interactive/config.rs` | H02 | load non-secret config, setup-required state, lỗi actionable |
| `interactive/bootstrap.rs` | H02 | caller cwd/project identity/Git root/lazy state |
| `interactive/{controller,events,view,input,terminal}.rs` | H03 | state reducer, event enum, renderer, line editor, terminal backend |
| `interactive/service.rs` | H04 | `InteractiveSessionService` nối runtime/store/tool gate |

Quyết định đã chốt ở H01:

- Không tạo crate presentation riêng; boundary renderer/controller nằm trong
  `interactive` của `harness-cli` (plan cho phép cả hai; chọn phương án ít rủi ro
  hơn cho một binary crate đang có 1005 dòng `main.rs`).
- Detector là trait; real implementation chỉ dùng trong binary. Integration test
  non-TTY chạy binary thật qua pipe — đúng thực tế, không cần PTY giả.
- Guard non-TTY là hành vi **mới** so với revision khảo sát (trước đây bare `ha`
  thoát 0 im lặng). Migration note thuộc H07 và được ghi ở operator docs.
- Thư viện terminal (H03) **chưa chốt** ở H01: phải spike Windows/PowerShell/Linux
  rồi pin phiên bản cụ thể trước khi viết renderer, không viết API từ trí nhớ.

## 4. Trạng thái staging theo checkpoint

Không checkpoint nào được nhận "done" khi prerequisite chưa đạt.

| ID | Nội dung | Phụ thuộc | Trạng thái |
|---|---|---|---|
| H01 | Entry point, dispatch, TTY detector, parser compat | — | dispatch + guard + parser xong; UI thật chờ H03 |
| H02 | Launch context, paths/HA_HOME, config/setup state | H01 | chờ H01 pass |
| H03 | Terminal app, controller/renderer, input loop | H02 | chờ H02 pass |
| H04 | G1 provider incremental, G2 tool continuation, G3 durable session | H03 + khảo sát G1–G3 | chờ H03 pass |
| H05 | Approval, resume, lifecycle | H04 | chờ H04 pass |
| H06 | Installer, User PATH scope, install manifest | H01–H03 | logic + test disposable; **không** ghi User PATH thật |
| H07 | Gate `Verify-HaLaunch.ps1`, PTY fixture, acceptance I01–I18 | H01–H06 | chờ |
| H08 | Release candidate, clean-machine route | H07 | chờ |

Bảng này được cập nhật lại ở mỗi checkpoint cùng evidence/handoff; trạng thái
"chờ" không được đổi thành "xong" chỉ vì code đã viết mà chưa có test chạy.

## 5. Nhật ký quyết định theo checkpoint

Mục này ghi quyết định library/path/credential/backend ngay khi chốt, để người
tiếp nhận không phải hỏi lại hội thoại.

### H01 — Entry point và launch contract

- `None => Ok(())` được thay bằng `interactive::launch`; toàn bộ thân hàm cũ được
  giữ nguyên trong `legacy_run(cli: Cli)` nên mọi subcommand cũ không đổi hành vi.
- Thêm `ha chat` với `--cwd/--resume/--headless/--prompt/--json`; conflict do clap
  ép ở parser nên exit code là 2 giống mọi usage error khác.
- Headless *execution* chưa nối application service ở H01: trả lỗi typed
  `service_unavailable` (exit 1) và được H04 thay bằng turn thật. Không mock nào
  được gắn vào đường này và không có gì được quảng cáo là đã xong.
- Interactive app ở H01 là boot header + prompt line-mode tối thiểu ghi rõ
  `connection pending`; H03 thay bằng controller/renderer thật có raw mode.



