# Plan cho DeepSeek: cài một lần, gõ `ha` để mở CLI tương tác

**19/09/2026 · Planning only · Feature track H01–H08.**

[Prompt giao DeepSeek](HA_LAUNCH_PROMPT.vi.md) · [Master plan](HARNESS_MASTER_PLAN.vi.md) · [Sổ tay M0–M12](implementation-next/README.vi.md)

## 1. Kết quả người dùng muốn

Sau khi cài, người dùng mở terminal ở project bất kỳ, gõ **`ha`**, thấy giao diện terminal của Harness, nhập yêu cầu và làm việc nhiều lượt. Không cần `cd` vào repo Harness, chạy Cargo, nhớ đường dẫn executable, truyền `--data-dir` hoặc tự tạo session ID.

“Giống gõ `codex` để mở CLI” trong yêu cầu là chuẩn trải nghiệm khởi động, không yêu cầu sao chép implementation, tài khoản hoặc tính năng của Codex. Plan này tập trung vào sản phẩm `ha`.

```text
PS C:\work\my-project> ha

Harness Agents  <version>
Project: C:\work\my-project     Provider: DeepSeek / <configured-model>
Session: new                    Mode: trusted host

Nhập yêu cầu. /help trợ giúp · /resume tiếp tục phiên · /exit thoát
> Sửa lỗi parser và chạy tests

Đang xử lý ...
[tool] read_file ...
[tool] test ... failed
...
Kết quả, diff và kiểm chứng ...
> <tiếp tục nhập yêu cầu>
```

Đây là mockup hành vi, không phải output hiện đã có. Không cần logo/animation/full-screen TUI phức tạp để nghiệm thu. Cần input loop, streaming, trạng thái chờ, hủy và phục hồi terminal thực sự hoạt động.

## 2. Căn cứ source: thiếu hai lớp khác nhau

**Baseline lịch sử**, khảo sát tại revision `547f0bb69acb62449ecf829ca38bff714ac404be`; bảng này không mô tả HEAD hiện tại. Source H đã phát triển sau đó: trước mỗi assignment phải đọc SPEC/evidence/handoff H đang có, inspect interactive modules/TurnDriver/tests và chỉ xử lý gap. Không ghi đè các tài liệu H đang được cập nhật. Chưa build/cài/chạy provider trong lượt lập plan này. Graph MCP không có tools callable nên đã đọc source trực tiếp.

| Căn cứ | Hành vi ở revision khảo sát | Khoảng trống ghi nhận lúc khảo sát |
|---|---|---|
| [CLI main](../crates/harness-cli/src/main.rs), `run`, match `None` | `None => Ok(())` | Gõ `ha` không tham số thoát ngay, chưa khởi động interactive app |
| [Cargo CLI](../crates/harness-cli/Cargo.toml) | Đã có binary tên `ha` | Không cần đổi tên binary hoặc tạo shell alias để giải quyết no-arg launch |
| [Install-Ha.ps1](../scripts/Install-Ha.ps1) | Build/copy hoặc Cargo install; không tự sửa PATH; mặc định `.cargo/bin` | Cần phân biệt cài cho developer với tải binary cho end user; PATH resolution cần kiểm chứng thật |
| [P7 install test](../crates/harness-cli/tests/phase_p7.rs), `p7_install_script_installs_a_working_binary` | Kiểm tra file được cài và `--version` | Chưa chứng minh bare `ha` mở session trong terminal ở thư mục khác |
| CLI `run_runtime` | Dựng MockProvider và `demo_workspace()` | Không được nối interactive input vào đường này rồi quảng cáo live coding |
| [CodingLoopService](../crates/harness-tools/src/loop_service.rs) | Một provider response, chạy tool calls, chưa gửi tool results lại model | Muốn hỏi agent và làm tiếp cần adapter runtime thật/multiple steps |
| [Provider interface](../crates/harness-providers/src/lib.rs) | Interface hiện tổng hợp stream events trước khi trả về caller | Muốn thấy response tăng dần phải có event stream/callback từ runtime, không render giả từng chữ sau khi xong |
| [Operator guide](OPERATOR_GUIDE.vi.md) | Bản source hiện chưa công bố artifacts phát hành | Chưa có đường tải release ổn định cho máy không cài Rust |

Việc terminal không tìm thấy `ha` là lỗi **command resolution/PATH**. Việc terminal tìm được `ha` nhưng chương trình thoát là lỗi **entrypoint/interactive startup**. Cả hai phải được kiểm chứng; `ha --version` thành công không chứng minh UI launch đã xong.

## 3. Quan hệ với plan M0–M12: ưu tiên feature trên CLI hiện tại

**Mọi track H và M triển khai trên source hiện tại**, chung root workspace, binary `ha`, runtime và store. H ưu tiên trải nghiệm khởi động, chủ yếu `crates/harness-cli`, installer và services cần nối; không phải ngoại lệ của một kế hoạch viết sản phẩm khác. Xem [bản đồ tích hợp](implementation-next/INTEGRATION_MAP.vi.md).

`InteractiveController` gọi service hiện có, nối `AgentSessionService`/`TurnDriver` qua ports. M2/M3/M4 mở rộng chính đường chạy này với regression checks; không viết hoặc thay sang engine thứ hai. Không sao chép business loop vào renderer. Các commands cũ `run`, `resume`, `memory`, `tasks`, `extensions`, `maintenance` giữ semantics/parser/JSON hiện có; đừng âm thầm biến `ha run` từ fixture thành live API trong feature này.

Ưu tiên đạt ba mốc tách biệt:

| Mốc | Phạm vi | Được nhận hoàn tất gì |
|---|---|---|
| Launch usable | H01–H03 + subset H06/H07 | Bare `ha` mở terminal app, nhập/thoát được, config/project đúng; nếu runtime chưa nối phải ghi connection pending |
| Interactive agent usable | H04–H05 + toàn H07 liên quan | Model thật qua cấu hình, nhiều lượt/steps, tool gate, stream/cancel/resume có bằng chứng |
| Install-and-run ready | H06–H08 + toàn H07 | Máy mới tải/cài binary, fresh terminal gõ `ha`, không cần source repo/Rust; installer/update/uninstall kiểm chứng |

Không gọi toàn tính năng hoàn tất chỉ vì đạt mốc đầu. Với mục tiêu “tải về rồi gõ `ha`”, mốc cuối là kết quả đầy đủ. Live-provider smoke có credential/budget được cấp mới chạy; fixture HTTP vẫn kiểm tra production adapter mà không trả phí.

## 4. Dispatch contract và CLI tương thích

Các lệnh mới dưới đây là mục tiêu, chưa được hỗ trợ ở revision khảo sát:

| Invocation | Behavior mục tiêu |
|---|---|
| `ha` trong terminal tương tác | Load InteractiveApp và chờ nhập; không tự thoát/no-op; không tự gửi prompt |
| `ha chat` | Alias entrypoint rõ cho interactive app, dùng chung controller với bare `ha` |
| `ha chat --cwd <path>` | Open project ở path được resolve từ caller cwd; không đổi repo Harness |
| `ha chat --resume <session-id>` | Resume đúng scope qua interactive controller; không thay `ha resume` cũ |
| `ha chat --headless --prompt <text> --json` | One turn dùng cùng application service, stdout JSON, logs stderr; không bật raw terminal |
| `ha --help`, `ha --version` | Chạy ngay, không config/provider/store init/network; giữ compatibility tests |
| `ha` khi stdin/stdout không là terminal | Không treo đợi prompt; in hướng dẫn ngắn qua stderr, exit 2; hướng đến explicit headless command |
| Các subcommands hiện có | Giữ dispatch hiện hành; không tự launch interactive app sau khi command kết thúc |

Interactive và headless options có conflicts rõ: `--headless` bắt buộc `--prompt`; `--json` chỉ headless ở command mới; `--resume` có thể đi với headless nếu SPEC quyết định behavior và tests, default chỉ interactive. Unknown option vẫn parser error, không được coi là prompt. Chưa thêm `ha "free text"` vì có thể đụng tên subcommands; không cần cho yêu cầu này.

`ha` là executable thật trên PATH, không phải alias/profile function hay wrapper gọi `cargo run` mỗi lần. Không phụ thuộc npm/Node chỉ để có global command; wrapper/package manager khác có thể thêm sau release pipeline.

## 5. Startup lifecycle, đường dẫn và cấu hình

Startup order:

1. Parse args; `--help/--version` fast path trước IO khác.
2. Chọn interactive/headless/subcommand; kiểm tra TTY/capabilities trước bật terminal modes.
3. Resolve caller cwd hoặc `--cwd`, canonical project identity; tìm Git root chỉ cho project context, không tự chdir về installation path.
4. Resolve user config/data locations, load non-secret config; dựng controller với dependencies lazy.
5. Restore terminal UI, render header/project/model/setup-state và input. Không scan toàn repo, load embeddings, start MCP servers hoặc network-check trước khi người dùng thấy input.
6. Khi user gửi yêu cầu đầu: resolve credentials/provider, acquire correct session store writer, admit input rồi dispatch. Lỗi setup/store phải hiện trong app và có cách sửa/thoát.
7. Khi exit: cancel/drain active run, checkpoint/close store, restore cursor/input/modes; return shell clean.

Proposed defaults, H02 phải ghi actual resolved paths trong diagnostics:

| Loại | Windows | Linux |
|---|---|---|
| User config | `%APPDATA%\HarnessAgents\config.toml` | `$XDG_CONFIG_HOME/harness-agents/config.toml` hoặc `~/.config/harness-agents/config.toml` |
| User runtime/catalog | `%LOCALAPPDATA%\HarnessAgents\data` | `$XDG_DATA_HOME/harness-agents` hoặc `~/.local/share/harness-agents` |
| Installed release binary | `%LOCALAPPDATA%\Programs\HarnessAgents\bin\ha.exe` | `~/.local/bin/ha` mặc định installer được chọn |
| Fixture override | `HA_HOME=<temp-root>` chứa config/data riêng | Cùng semantics |

User config/data nằm ngoài installation directory và caller repo; update không xóa sessions. New `HA_HOME` override chỉ áp startup mới nếu commands cũ explicit `--data-dir` thì giữ nguyên. Precedence: explicit `ha chat` options → `HA_HOME`/supported env overrides → user config → safe defaults. Repo config không được chỉ định executable/provider secret hoặc nâng quyền startup.

Mỗi project có registered identity và store directory dưới user data. Registry mutation atomic/locked; store writer vẫn theo runtime hiện tại. Hai terminals cùng project cần trả owner-busy với cách mở read-only/thoát, không steal lock hoặc cùng ghi. Không-Git directory vẫn mở app, chỉ báo Git features unavailable. Directory không tồn tại/read-only phải có lỗi rõ; chưa input thì không tạo file vào project.

First run thiếu provider/key: vẫn hiện app với setup panel; nhập key bị che và không vào command history/logs. Secret lưu qua credential resolver/OS store đã chọn hoặc hướng dẫn env var; không auto lưu plaintext vào repo. Không hiển thị model default chưa resolve như đã connected. Lỗi authentication khi gửi yêu cầu quay về input, không tự chuyển sang mock.

## 6. InteractiveController và terminal UI

Default H03 là terminal app tối thiểu với prompt editor và streaming output; fullscreen/alternate-screen chỉ dùng nếu cần. Chọn/pin thư viện terminal theo hỗ trợ Windows/Powershell/Linux tại lúc implementation, không ghi API library từ trí nhớ. Giữ business logic ngoài event/render loop.

```text
main dispatch
  -> resolve LaunchContext (cwd/config/data/terminal capability)
  -> InteractiveController
       input event -> UserCommand -> InteractiveSessionService
       backend event <- stream    <- application/runtime
       view state -> TerminalRenderer
  -> shutdown/drain -> terminal restoration
```

States: booting → ready/setup_required → running → waiting_approval/question → ready; cancellation có canceling, lỗi recoverable quay về ready; closed là terminal. Không render response rồi process exit ở cuối mỗi câu. Controller nhận channels có backpressure; renderer coalesce display updates nhưng không drop domain receipts/terminal outcomes.

Input/editor requirements:

- Enter gửi một yêu cầu; multiline dùng phím combo hỗ trợ được kiểm thử hoặc chế độ paste, không đoán mọi terminal hỗ trợ cùng keys.
- Gõ tiếng Việt/Unicode, backspace/history, resize và paste không làm prompt vỡ. Bracketed paste nếu hỗ trợ không tự gửi nhiều dòng như nhiều commands.
- Slash commands tối thiểu `/help`, `/exit`, `/new`, `/status`, `/model`, `/config`, `/resume`; args parse rõ. `/new` không bỏ active run: phải cancel/settle hoặc chờ boundary.
- Ctrl-C khi đang chạy: hủy run hiện tại và quay về prompt sau drain. Ctrl-C khi idle: clear input; Ctrl-D trên input rỗng hoặc `/exit`: thoát. Windows EOF/mapped keys được test rõ, không quảng cáo Ctrl-D nếu adapter không hỗ trợ.
- Không coi input là shell command; câu user chỉ vào agent service. Approval renderer hiện action/working directory/diff/scope thực tế, trả answer đúng request ID.
- Terminal cleanup dùng RAII/finally guard cho success/error/unwind. Fatal process kill không hứa restore được; không để test giả panic handler chứng minh được SIGKILL cleanup.

Headless adapter dùng cùng service, không TerminalRenderer; shutdown Ctrl-C dẫn typed cancellation và appropriate exit code. Không xuất ANSI vào JSON hoặc redirected output. Startup UI mở offline được; thực thi model thì cần credential/network đúng và báo lỗi thật.

## 7. Port nối runtime: bắt buộc để không tạo giao diện rỗng

Pseudocode thiết kế, không là API hiện có:

```text
InteractiveSessionService.open(LaunchContext, optional_session) -> SessionHandle
submit(session, input_id, text) -> Stream<SessionEvent>
answer(session, request_id, response) -> Ack
cancel(session, run_id) -> CancelAck
status/list_sessions/resume/close(...) -> typed results

SessionEvent = Accepted | TextDelta | ToolStarted | ToolSettled
             | ApprovalRequired | QuestionRequired | Usage
             | RunTerminal | RecoverableError
```

Service giữ session/task identity qua các lượt, durable input ACK và call/result correlation. Runtime/store/tool gate là owners business execution; renderer không SQL-write, không trực tiếp execute shell. Unknown effect sau crash được reconcile trước tiếp tục; in-memory chat history không đủ cho `/resume`.

H04 phải khảo sát ba prerequisites lúc coding:

| Gate | Yêu cầu | Nếu chưa có |
|---|---|---|
| G1 Provider thật | Configured adapter + credential resolver + incremental events | Refactor provider stream boundary tối thiểu, giữ tests/compat; không dùng MockProvider cho production chat |
| G2 Tool continuation | Typed assistant calls/tool results, bounded model→tool→model loop | Bổ sung application TurnDriver hoặc tool-port orchestration, không lặp `run_runtime` vì mỗi lần admit user input mới |
| G3 Durable sessions | Resume/replay/context đúng task, canceled/error settlement | Nối service store ports và define lifecycle; UI không tự fabricate summary/IDs |

Mọi change G1–G3 phải có SPEC nhỏ, call sites/tests mapping và giữ module dependency direction. Có thể reuse contracts từ plan M2–M5, nhưng không giao implement toàn M-roadmap. Nếu scope runtime phát sinh lớn, handoff H04 với gap/test cần làm; H01–H03 vẫn bàn giao riêng và không claim full interactive agent đã xong.

Có thể dùng explicit fixture provider để demo/test UI. Header phải ghi fixture/mock khi đó; default installed app không được trả “mock response” như agent thật. Không chỉ gọi executable `ha run` từ trong `ha` rồi parse stdout thành UI events.

## 8. Cài đặt, PATH và tải binary

### 8.1. Developer route vẫn giữ được

`scripts/Install-Ha.ps1` tiếp tục hỗ trợ source build/Copy/Cargo route và `-Destination` temp fixture. Những thay đổi cần lên plan:

- Dùng built artifact thực tế được Cargo báo, validate version/build commit; đừng chỉ so mtime một số source files để khẳng định binary đúng phiên bản. Cargo.toml/lock/toolchain/build scripts thay đổi phải không bị bỏ qua.
- Copy/install temp→verify→replace; running executable locked thì báo cần đóng app, không kill user's sessions. Nếu copy lỗi không gọi mọi lỗi là “file in use”; giữ error classification thật.
- Test absolute installed binary và **resolved `ha` command**; phát hiện có executable/alias/function khác tên `ha` đang đứng trước trên PATH. Không xóa command không thuộc installer.

### 8.2. End-user route cần prebuilt release

H08 đóng gói đúng một `ha` executable và assets thực sự cần; không kèm fixture server executables/test secrets. Mặc định hỗ trợ Windows x64 và Linux x64 theo binary đã test; ARM/macOS chỉ công bố khi có build/test riêng. Linux artifact phải công bố libc/minimum environment thực sự cần; không gọi binary portable nếu còn system dependencies chưa kiểm tra.

Distribution path: release bundle/version manifest/checksum từ repo release đã publish, downloader/installer chọn OS/arch, tải staging, verify, cài vào user bin rồi kiểm tra. Không bắt end user cài Rust/Git/Node hoặc clone repo. Không viết README với URL release tưởng tượng; chỉ gắn link sau artifact thật được publish theo quyền được giao. Checksum tải cùng server phát hiện corruption, không thay cryptographic publisher trust; provenance/signature nếu có phải verify và công bố đúng.

User-scoped install không cần admin. Installer biết install manifest gồm version/source/digest/owned files/added PATH entry, giúp update/uninstall không xóa chương trình khác. Update giữ config/sessions, kiểm tra data-schema compatibility trước chạy version mới; thất bại giữ binary cũ. Uninstall mặc định chỉ binary/owned PATH entry, giữ user data; `purge` riêng có path containment/confirmation theo assignment.

### 8.3. PATH contract đặc biệt trên Windows

H06 phải đọc **User PATH** riêng, append/de-duplicate install dir, giữ nguyên entries khác và Machine PATH. Không ghi giá trị `$env:PATH` tổng hợp của process vào User PATH: nó có thể chứa cả Machine PATH hoặc entries tạm. Chỉ sửa user environment khi installer route được user chọn có khai báo rõ; `-NoModifyPath`/custom destination cần báo manual action.

Environment của process mới thường kế thừa parent. Sửa persisted User PATH không sửa được mọi terminal/IDE đang mở: installer phân biệt registry/User PATH updated, current shell visible, và command resolution thật. Nếu chạy installer bằng child `pwsh -File`, child không cập nhật PATH của parent shell; báo mở lại terminal/terminal host đang giữ env cũ, đồng thời cho exact installed path để dùng ngay. [Microsoft: PowerShell environment scopes](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.core/about/about_environment_variables?view=powershell-7.5), [process environment inheritance](https://learn.microsoft.com/en-us/windows/win32/procthread/environment-variables).

Không giải quyết bằng sửa `$PROFILE` để chỉ PowerShell tìm được trong khi CMD không tìm được. Tests kiểm tra PowerShell/CMD command resolution; POSIX shell path handling có route riêng. Không dùng shell alias làm tiêu chí cài xong.

## 9. Backlog giao DeepSeek — H01–H08

Paths mới dưới đây là proposed files, tạo lúc implementation; filenames có thể điều chỉnh trong SPEC nhưng giữ ownership.

### H01 — Entry point và launch contract

**Depends:** none. **Targets:** `crates/harness-cli/src/main.rs`, `src/interactive/mod.rs`, `tests/interactive_launch.rs`.

1. Ghi SPEC source baseline, TTY dispatch table và compatibility list.
2. Đổi no-subcommand route sang interactive launch; thêm `chat` command dùng cùng entrypoint; giữ help/version/subcommands fast path.
3. Define terminal detector injected cho unit tests, real detector cho integration; non-TTY fail fast có instructions, không silently exit 0.
4. Tạo tests I01/I02/I03, parser compatibility hiện có.

**Exit:** bare `ha` dispatch đúng, chưa được nhận full UI/agent completed nếu H03/H04 chưa xong.

### H02 — Startup context, project và first-run config

**Depends:** H01. **Targets:** `src/interactive/{bootstrap,paths,config}.rs`, existing config/store ports.

1. Implement resolved caller cwd/project identity/HA_HOME paths, lazy state initialization, không chdir installation dir.
2. Dựng first-run/setup-required state và non-secret effective config; resolve keys qua credential port, không user command history.
3. Link store/catalog/session directory theo project; handle owner-busy/read-only/missing cwd/corrupt config với actionable error.
4. Tests I04/I05/I09/I16, fixture home/env hoàn toàn độc lập user thật.

**Exit:** mở app ngoài repo Harness, không yêu cầu flags/data-dir; chưa network call chỉ vì launch.

### H03 — Terminal app và input loop

**Depends:** H02. **Targets:** `src/interactive/{controller,events,view,terminal,input}.rs` hoặc presentation crate riêng nếu có boundary rõ.

1. Chọn/pin terminal library sau Windows/Linux spike; state reducer/controller tách renderer.
2. Header+prompt+progress+responses, Unicode/paste/resize/slash commands; ready loop vẫn sống sau một response.
3. Ctrl-C/EOF/exit and guard restoration, backend events bound channel; headless không đi qua raw mode.
4. PTY tests I01/I06/I07/I08 và screenshots/transcripts review layout nếu cần.

**Exit:** UI usable với fixture service có nhãn rõ. Không gắn mock adapter thành production mặc định.

### H04 — Application service và agent execution thật

**Depends:** H03; G1/G2/G3 được khảo sát và hoàn thiện trong scope này. **Targets:** `src/interactive/service.rs`, runtime/providers/tools/session ports; application module nếu cần.

1. Chốt service signatures và states, trace actual current call chain; có executable multi-step fixture trước refactor.
2. Nối configured provider + incremental events; credentials xử lý ngoài prompt/records. Không wrapper gọi CLI subcommand.
3. Admit một user input, giữ same session, tool results trở lại model, bounded step/token/deadline, cancellation và final receipts đúng.
4. Tests I10/I11/I12 + existing provider/tools/session regressions; bounded live smoke riêng khi được cấp credentials/budget.

**Exit:** câu thứ hai có conversation context thật, tool loop có fail/fix/final evidence; installed app không tự fallback mock. Nếu G1–G3 chưa hoàn tất ghi H04 blocked/in_progress, không bỏ yêu cầu để gọi done.

### H05 — Approval, resume và lifecycle

**Depends:** H04. **Targets:** interactive controller/service, existing question/approval/session APIs.

1. Render proposal/question đúng ID/action/cwd/scope, answer qua app authority; no auto blanket grant.
2. `/resume` list/select scoped sessions, recover actual state/effects trước submit; `/new` không bỏ chạy ngầm.
3. Ctrl-C active cancels current run, returns prompt sau cleanup; `/exit` handles active work và restore terminal/store ownership.
4. Tests I07/I12/I13/I16; kill/reopen cases chạy process thật, không chỉ throw exception.

**Exit:** tiếp tục đúng task, không duplicate side effect, mọi đường quit/error để terminal dùng tiếp được theo OS contract.

### H06 — Installer và command resolution

**Depends:** H01–H03 để có executable interactive kiểm tra; final verify lại sau H05. **Targets:** `scripts/Install-Ha.ps1`, install helpers/tests riêng.

1. Giữ dev routes, add explicit user PATH handling/install manifest và actual binary identity verification.
2. Scope User PATH merge đúng, custom dir/no-modify option, resolution conflict detection, fresh environment vs current shell distinctions.
3. Handle stale artifact/locked executable/permission failures/update rollback; đừng thay binary đang chạy bằng kill.
4. Tests I14/I15/I17/I18 trong disposable env; persisted PATH test dùng test account/VM nếu cần, không mutate real user của CI.

**Exit:** từ ngoài repo và session environment đúng, shell resolve `ha` trỏ installed binary; tuyệt đối không chỉ kiểm tra absolute path `--version`.

### H07 — End-to-end acceptance và operator handoff

**Depends:** H01–H06. **Targets:** `tests/interactive_launch.rs`, `tests/interactive_session.rs`, PTY fixture host, `scripts/Verify-HaLaunch.ps1`, CI.

1. Tạo gate exact selectors/discovery counts; headless CLI tests không được coi là PTY proof.
2. Run I01–I18 trên Windows/Linux phù hợp, PowerShell/CMD fresh command resolution, config missing/offline paths.
3. Update operator docs chỉ với behavior có thật; migration notes cho new no-arg non-TTY behavior; unchanged subcommand regressions.
4. Evidence/handoff gồm commit/tree digest, executable digest, resolved path, TTY transcript, tests/OS not-run và actual next action.

**Exit:** local interactive/install acceptance đủ; public download chưa xong nếu H08 chưa có artifact/clean-machine evidence.

### H08 — Prebuilt release và clean-machine install

**Depends:** H07. **Targets:** release workflow/package manifest/end-user installer, docs download/update/uninstall.

1. Build release bundles đúng tested revision, architecture/OS, checksums/provenance; không fixture bins/secrets.
2. Installer từ published artifact hoặc local release candidate hỗ trợ staging/verify/user bin/PATH/update/uninstall như mục 8.
3. I19/I20 test máy/VM không Rust/Git/Node và không source repo; user data survive update/uninstall.
4. Publish release/link khi assignment cấp quyền; nếu chưa cấp, bàn giao candidate và báo download route chưa public, không fabricate link.

**Exit:** người dùng có một đường tải/cài thật rồi fresh terminal `ha` mở app. Không yêu cầu cài build toolchain chỉ để sử dụng.

## 10. Acceptance I01–I20 và test oracle

Các tests dưới đây là **đặc tả tương lai**, chưa implement/chạy trong lượt lập plan. H07 tạo selectors ổn định `i01_*`...; H08 bổ sung I19/I20. Không thay acceptance A01–A36 của plan tổng thể.

| ID | Setup/trigger | Oracle bắt buộc | Owner |
|---|---|---|---|
| I01 | Spawn compiled `ha` không args trong PTY, cwd temp project | Header/project/input xuất hiện, process còn sống và nhận text; `/exit` thoát sạch | H01/H03 |
| I02 | `ha --help`, `--version`, old subcommands | Không launch UI/network/store ở fast paths; JSON/exit semantics cũ giữ | H01 |
| I03 | Piped stdin/stdout gọi bare `ha`; explicit headless prompt | Bare không hang, exit 2 hướng dẫn; headless chạy không ANSI/raw mode | H01/H04 |
| I04 | Run installed binary từ path có spaces/Unicode và no-Git directory | Project theo caller cwd, không theo install/repo Harness; Git unavailable rõ | H02 |
| I05 | Empty HA_HOME, offline, không API key | UI mở setup state, không trả mock; `/exit` hoạt động; startup không provider call | H02 |
| I06 | PTY nhập tiếng Việt, paste multiline, resize/history/backspace | Không corrupt text/double submit; prompt vẫn usable sau output | H03 |
| I07 | Ctrl-C khi run chờ tool/provider và khi idle | Active cancel/drain rồi prompt; idle clear input; exit restore terminal | H03/H05 |
| I08 | Inject render/backend error sau terminal init | Terminal modes/cursor restore khi recoverable failure/unwind; no swallowed fatal error | H03 |
| I09 | Corrupt config, invalid cwd, data directory permission lỗi | Actionable error trong UI hoặc nonzero headless, không silent overwrite/fallback | H02 |
| I10 | Fake HTTP production adapter gửi text delta rồi giữ terminal barrier | Text hiện trước complete; không sinh animation từ response đã hoàn tất | H04 |
| I11 | Two conversation inputs + tool fail→fix→pass với actual temp repo | Session/context đúng, one input admission mỗi message, tool result pairing và final evidence thật | H04 |
| I12 | Approval grant/denial/expiry và provider auth failure | Denied/expired tool không execute; UI tiếp được; auth fail không fallback mock/leak key | H04/H05 |
| I13 | Hard kill sau committed tool receipt, reopen `/resume` | Không rerun settled side effect; next step nhận result; unresolved effect reconcile | H05 |
| I14 | Install temp destination, unrelated cwd, launch shell `ha` | Resolved executable path/digest đúng và I01 pass trên installed executable | H06 |
| I15 | User PATH missing/present/duplicate; Machine PATH có entries khác | Append đúng user entry một lần, giữ nguyên other scopes; no mixed process PATH overwrite | H06 |
| I16 | Hai terminals cùng project, sessions của project khác | No concurrent writer corruption; busy message actionable; resume không vượt scope | H02/H05 |
| I17 | Existing khác binary/alias/function tên `ha` đứng trước | Installer báo resolved conflict, không xóa/replace command không sở hữu | H06 |
| I18 | Update locked exe/bad copy/checksum/build source change | Old binary usable khi update fail; correct artifact identity; no kill sessions | H06 |
| I19 | Clean VM không build toolchain tải/cài release artifact | Fresh shell gõ `ha`, UI mở, help/version đúng; không cần source repo/Cargo | H08 |
| I20 | Update rồi uninstall user install | Config/sessions preserved; only owned files/PATH entry removed; reinstall resume được | H08 |

PTY/ConPTY tests cần harness thích hợp Windows/Linux. `Command::output()` tạo redirected pipes, nên không thể dùng nó một mình để chứng minh no-arg interactive behavior. Fake terminal detector chỉ dùng unit tests; integration phải có actual terminal. Test runtime paid API không cần cho CI; fake HTTP server đi qua production adapter đủ kiểm tra transport/protocol. Live smoke ghi rõ not_run nếu thiếu authorization/credentials.

Windows persisted-environment test phải tách registry/User PATH state khỏi process inherited PATH; spawning child shell từ parent cũ vẫn có thể thấy env cũ. Clean logon/fresh host test hoặc explicit reconstructed environment fixture cần ghi phương pháp, không claim test reload registry nếu chỉ tự prepend PATH trong test.

Runtime gate dự kiến **H07 phải tạo**, chưa có hiện tại:

```powershell
pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1
```

Gate gồm targeted H tests, parser/subcommand regressions và affected runtime/tool/storage tests; exact discovery, timeout bounded, required OS checks không skip thành pass. Trước H07 dùng targeted tests thực có trong H01–H06 và ghi item status, không chạy lệnh chưa tồn tại rồi đoán success.

## 11. Definition of done và bàn giao

DeepSeek phải trả được bằng chứng cho cả ba câu:

1. **Terminal tìm thấy gì?** `Get-Command ha`/`where.exe ha` hoặc shell tương đương, installed path và binary version/digest.
2. **Gõ `ha` có mở app không?** PTY transcript từ cwd ngoài repo; header/input còn sống, hai lượt input, cancel/exit/terminal cleanup.
3. **App đang dùng backend nào?** Resolved provider/model hoặc explicit setup/fixture state; real service wiring, tools/receipts/resume tests; không nhận mock là live.

Cho download distribution thêm clean-machine install I19/I20 và actual artifact link/version. Không thay câu 2 bằng `--version`, không thay câu 3 bằng screenshot prompt đẹp.

Output coding: `docs/specs/HA_LAUNCH.vi.md`, `docs/evidence/HA_LAUNCH.vi.md`, `docs/handoffs/HA_LAUNCH.vi.md` là paths tương lai. Evidence ghi implemented H IDs, test IDs/exact selectors/count, OS/compiler/terminal version, source/executable digests, commands/log refs, limitations và next action. Spec lưu decisions về library/path/credential/backend changes; không cần user kể lại hội thoại.

## 12. Thứ tự giao việc đề nghị

**Giao H01–H03 trước** để `ha` thực sự mở CLI; tiếp H04–H05 để nói chuyện/làm việc qua backend; H06–H07 hoàn thiện install/resolution và regression; H08 thêm đường tải cho máy chưa có Rust. H06 có thể được chuẩn bị sau H03 nhưng không nhận đầy đủ trước backend gates.

Không yêu cầu DeepSeek nâng cấp toàn bộ app, làm Web hoặc daemon ở track này. User hiện yêu cầu plan; bộ tài liệu không tự cấp quyền sửa PATH, cài binary, gọi model trả phí hay phát hành release trong lượt lập kế hoạch.
