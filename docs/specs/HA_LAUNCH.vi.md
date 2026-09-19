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
- Thư viện terminal đã chốt ở H03: `crossterm = "=0.29.0"` (xem mục 5).

### H03 — Terminal app và input loop (hoàn tất phần code)

- **Chia module thật**: `events.rs` (từ vựng `Key`/`SessionEvent`/`AppPhase`/`RunOutcome`),
  `input.rs` (editor), `service.rs` (port + staged service + fixture), `view.rs` (chuỗi
  thuần), `controller.rs` (state reducer, **không** chạm terminal), `terminal.rs`
  (backend thật + guard), `app.rs` (host vòng lặp render). Controller chỉ trả
  `Effect`; host dịch effect sang lời gọi terminal, nên toàn bộ luật được unit test
  không cần PTY.
- **State machine**: `Booting` → `Ready`/`SetupRequired` (chuyển khi `boot_lines` thật
  sự render header) → `Running` → `Canceling` → về `Ready`/`SetupRequired`;
  `Closed` khi thoát. Một input chỉ được admit khi không có run active; input thứ hai
  bị từ chối kèm hướng dẫn Ctrl-C.
- **Bàn phím** (map từ `crossterm::event`, có test): Enter gửi, Backspace/Delete,
  Left/Right/Home/End theo **ký tự** (không theo byte, nên tiếng Việt không vỡ), Up/Down
  history có nhớ draft, Ctrl-C = cancel khi đang chạy / clear input khi idle, Ctrl-D
  trên buffer rỗng = thoát, `Event::Paste` = chèn một lần (newline thành space, không
  tự submit), `Event::Resize` = vẽ lại prompt không mất buffer.
- **Streaming thật theo sự kiện**: `TextDelta` được flush mỗi vòng pump và **trước**
  dòng tool/terminal, nên text hiện khi run còn đang chạy (không phải animation sau
  khi xong).
- **Raw mode an toàn**: `RawModeGuard` bật raw mode + bracketed paste và restore khi
  `Drop` cho mọi đường thoát (bình thường, lỗi, unwind). Process bị kill cứng không
  hứa restore — đúng như plan. Không dùng alternate screen.
- **Fallback line mode**: nếu không bật được raw mode, app dùng lại **cùng controller**,
  mỗi lần một dòng; nói rõ lý do ra stderr. Không có đường nào biến fallback thành
  production backend giả.
- **Fixture là opt-in tường minh**: `ha chat --fixture` dùng backend fixture **có nhãn**
  ("fixture (no model was called)") và echo lại đúng text đã admit; fixture bị **từ chối**
  khi đi với `--headless` vì lượt headless phải báo trạng thái backend thật. Mặc định
  production vẫn là staged service báo `connection pending` (H04 nối thật), không tự
  fallback mock.
- **Giới hạn đã biết của H03**: editor một dòng (paste nhiều dòng bị đổi newline thành
  space, chưa có multiline mode); việc Ctrl-C có được crossterm giao thành key event
  trên Windows/ConPTY, và độ hiển thị tiếng Việt, **chưa** được chứng minh — phải kiểm
  bằng PTY thật ở H07.


## 4. Trạng thái staging theo checkpoint

Không checkpoint nào được nhận "done" khi prerequisite chưa đạt.

| ID | Nội dung | Phụ thuộc | Trạng thái |
|---|---|---|---|
| H01 | Entry point, dispatch, TTY detector, parser compat | — | dispatch + guard + parser xong; UI thật chờ H03 |
| H02 | Launch context, paths/HA_HOME, config/setup state | H01 | context + paths + setup state xong bằng unit test; UI thật chờ H03 |
| H03 | Terminal app, controller/renderer, input loop | H02 | controller/renderer/editor/terminal + fixture route xong bằng test; PTY thật thuộc H07 |
| H04 | G1 provider incremental, G2 tool continuation, G3 durable session | H03 + khảo sát G1–G3 | **đang làm**: khảo sát G1–G3 xong, G1 xong (additive, có test barrier); G2/G3 + service thật còn lại |
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

### H02 — Startup context, project và first-run config

- **Provider/secret vẫn không vào file config.** Hợp đồng P0 `HarnessConfig` (chỉ
  `schema_version` + `cli`, `deny_unknown_fields`) được giữ nguyên. H02 chỉ **kiểm
  tra sự hiện diện** của credential qua biến môi trường `DEEPSEEK_API_KEY` hoặc
  `HA_API_KEY`; giá trị không bao giờ được đọc vào chuỗi hiển thị hay log. Endpoint
  và model thật là quyết định của H04; H02 không đoán URL mặc định.
- **Store writer là lazy.** Bootstrap chỉ resolve đường dẫn; writer chỉ mở khi user
  gửi yêu cầu đầu, nên mở app không tạo database và không khóa project. Hai terminal
  cùng project vì vậy chỉ xung đột khi bắt đầu làm việc, và lỗi là `writer_locked`
  có sẵn của store port (đã test trên thư mục fixture).
- **Project identity** = `sha256` của đường dẫn project đã canonicalize; store của
  project ở `<data>/projects/project-<16 hex đầu>`. Project lấy từ caller cwd hoặc
  `--cwd`, không bao giờ từ thư mục cài đặt; Git root chỉ là context (đi lên theo
  cây thư mục, không chdir).
- **Config hỏng là lỗi typed, không fallback và không ghi đè.** Message nêu đường dẫn
  và hướng dẫn `ha config validate`, nhưng **không** in lại text parser thô: lỗi type
  của TOML có thể trích giá trị bị từ chối, mà giá trị đó có thể là secret người dùng
  đặt nhầm file. Test khẳng định file giữ nguyên byte.
- **Header in actual resolved paths**: project, Git, config (kèm origin `HA_HOME` /
  `platform default`), data dir, store dir của project, và setup hint khi thiếu
  config/credential. Đây là phần diagnostics mà plan mục 5 yêu cầu.
- **UI dùng tiếng Việt** theo mockup của plan. Render tiếng Việt trên terminal thật
  chưa được kiểm chứng ở H02; việc đó thuộc H03/H07 với PTY.

### H03 — Terminal app và input loop

- **Thư viện terminal đã chốt: `crossterm` pin `=0.29.0`** (style pin exact của repo).
  Khảo sát thật, không viết API từ trí nhớ: `cargo search crossterm` trả 0.29.0 là bản
  mới nhất trên registry; docs.rs 0.29.0 (`all.html` + trang feature) xác nhận các item
  cần dùng đều tồn tại — `terminal::enable_raw_mode`, `terminal::disable_raw_mode`,
  `terminal::is_raw_mode_enabled`, `terminal::size`, `terminal::{Clear, ClearType,
  EnterAlternateScreen, LeaveAlternateScreen}`, `cursor::{Hide, Show, MoveTo}`,
  `event::{read, poll, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
  EnableBracketedPaste, DisableBracketedPaste}`, macro `execute!`/`queue!`, trait `tty::IsTty`.
- **Feature**: bản 0.29.0 bật mặc định `events`, `bracketed-paste`, `windows` — đúng
  thứ H03 cần cho raw mode + key event + ConPTY. Không bật thêm feature nào; đặc biệt
  **không** bật `use-dev-tty` (mở `/dev/tty` là quyết định riêng) và không cần
  `event-stream` vì event được đọc trong reader thread riêng.
- **Không dùng ratatui/full-screen TUI**: plan yêu cầu app terminal tối thiểu với prompt
  editor + streaming output; alternate screen chỉ dùng nếu thật cần.
- Detector TTY của H01 vẫn là điều kiện tiên quyết: raw mode chỉ được bật sau khi
  detector nói stdin/stdout là terminal.
- Thêm dependency sẽ cập nhật `Cargo.lock`; mọi lệnh gate dùng `--locked` nên lock phải
  được cập nhật trong cùng checkpoint.

### H04 — Application service và agent execution thật (đang triển khai)

**Khảo sát G1–G3 tại revision hiện tại** (đọc source thật, không suy đoán):

| Gate | Hiện trạng source | Khoảng trống |
|---|---|---|
| G1 provider tăng dần | `ModelProvider::stream` trả `ProviderFuture` = `Vec<ProviderStreamEvent>` sau khi đã đọc hết byte stream; `assemble_stream` gộp text/tool delta thành một response | Không có event tăng dần cho caller → text không thể hiện trước khi xong |
| G2 tool continuation | `RuntimeService::run_with_cancellation` admit input rồi gọi provider **một lần**; `CodingLoopService::run_once` thực thi tool call sau đó nhưng **không** gửi tool result trở lại model | Thiếu vòng lặp model→tool→model có bound |
| G3 durable session | `SessionService::{admit_input, record_synthetic_receipt, recover}` + `SqliteStore` (writer lock/fence, journal, receipt, snapshot) đã có | Chưa có lifecycle interactive (resume/canceled/error) và chưa nối vào service |

**Quyết định G1 (đã hoàn tất trong checkpoint này)**:

- Thêm **boundary additive** `StreamingModelProvider` + `ProviderEventStream` trong
  `crates/harness-providers/src/streaming.rs`; `ModelProvider`, call sites và test P2
  **không đổi**. `collect_events` là cầu nối về dạng buffered cho caller cũ, và có
  test khẳng định hai boundary trả cùng tập event.
- Truyền event qua **bounded channel** (4 cho mock, 16 cho adapter) + `poll_fn`, nên
  consumer chậm sẽ tạo backpressure thay vì buffer vô hạn.
- `DeepSeekAdapter` decode SSE và **forward từng event ngay khi decode**, không đợi hết
  body; `MockProvider` phát script theo từng event (có delay/cancel).
- Cancellation được kiểm tra trước dispatch, trong lúc chờ response và trong lúc đọc
  stream; lỗi trả về là `provider_canceled` typed.
- **Bằng chứng tăng dần (lõi I10)**: test dựng fixture HTTP thật, gửi delta đầu rồi
  **giữ body mở**; client phải nhận được `TextDelta` trong lúc barrier còn giữ. Đây là
  thứ boundary buffered không thể vượt qua.

**Còn lại của H04**: G2 (TurnDriver bounded model→tool→model dùng policy/approval/receipt
hiện có), G3 (lifecycle session/resume trong service), nối `interactive/service.rs`
thật thay `PendingService`, và `ha chat --headless` chạy turn thật. Live smoke ghi
`not_run` vì chưa được cấp credential/budget.
