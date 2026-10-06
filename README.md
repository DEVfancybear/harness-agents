# Harness Agents

**A local coding agent that remembers what it did and picks up where it left off.** / **Coding agent chạy trên máy, nhớ việc đã làm và tiếp tục đúng chỗ đã dừng.**

`ha` is a coding agent written in Rust. It runs in your terminal against your project, calls a model provider (DeepSeek by default), reads and edits code through approval-gated tools, and keeps every conversation, tool result and learned fact in a local SQLite store, so a session can be resumed, inspected or continued later.

**Language / Ngôn ngữ:** [English](#english) · [Tiếng Việt](#tiếng-việt)

## English

### What it does

- **Chat in the terminal.** `ha` opens an interactive app (TUI, or plain line mode). `ha exec "…"` runs one prompt for scripts and CI, with `text`, `json` or `stream-json` output.
- **Works on your code with guarded tools.** Read, search, glob, patch/edit/write files, run processes and shell commands, inspect Git, ask you a question, delegate to a sub-agent. Every action goes through the host policy: `y` runs once, `a` allows the rest of the turn, `n` refuses; `/permissions` picks `ask`, `auto-edit` or `full-auto` from a menu (`full-auto` asks for nothing; deny rules still apply).
- **Reads the web.** `web_search` (Google via Serper with `SERPER_API_KEY`, DuckDuckGo without a key) and `web_fetch` (a page as readable text with its links) support research workflows. Local and private addresses are refused.
- **Runs a persistent Python REPL.** The `ipython` tool is prime-agent's kernel: variables persist across cells and turns, `bash('cmd')` starts commands in the background and returns a handle, and `await rlm.spawn(...)` / `rlm.collect(...)` run explorer children in parallel. prime-agent's Python skills come pre-imported (`edit`, `websearch`, `attach_image`, `goal`, `compact`, `refine`, …). With `uv` installed the kernel gets its own venv with prime-agent's default packages; otherwise it runs on Python 3.11+ from the system. Git Bash is needed on Windows for `bash()`. Each cell asks for approval like `run_shell` unless the mode is `full-auto`.
- **Works toward a goal.** `/goal <objective>` keeps the app working across turns until the model calls `goal_complete` (at most 10 automatic turns, then it pauses). `/goal status|pause|resume|clear`; Ctrl-C pauses it, and `/resume` brings it back paused. Long turns shorten their oldest tool results to stay within the context budget.
- **Resumes conversations.** `/resume` lists your conversations (one row each) and replays the chosen one's questions and answers to the model and on screen.
- **Uses skills.** Agent Skills directories (`SKILL.md` plus references and scripts) from the bundled set, `~/.agents/skills`, a trusted project's `.agents/skills`, or any folder in `HA_SKILL_PATHS` (e.g. `~/.claude/skills`). The model activates a matching skill by name and reads its files with `read_skill_file`; `/skills` lists them, `/skill:<name>` runs one.
- **Checks its own work.** `[verify]` in `.harness/config.toml` lists the project's checks (types, tests, ...); the harness runs them itself before a goal completes, as the default `/autonomous` gates and on `/verify`, and an independent `verifier` agent (fresh context, cannot edit) judges a goal before it ends. `.harness/features.json` is a feature list the agent works one feature at a time; only a harness-run verification marks a feature passing. `/review` and `/features` show the rest.
- **Hands work over between sessions.** A session starts from a brief of recent commits, uncommitted files and the last handoff; `/handoff` writes `.harness/progress.md` for the next one (`--reset` continues in a fresh conversation), and leaving names what is unfinished. `/doctor` (or `ha doctor`) audits how ready the repository is for an agent, with a fix for each gap.
- **Learns skills.** `/refine` (and the automatic review every 25 turns or 50 tool calls, run in the background) can write `SKILL.md` skills of its own into `.harness/skills` (trusted project) or `<config-dir>/skills` (global). A checker lands only a skill that passes its lint (format, size, safety, secrets) and keeps a ledger of why it changed. Their use is counted; one never used is archived to `.archive/` after its probation. `/learn [focus]` keeps a lesson now; a lesson a command can detect may be proposed as a check, which joins `[verify]` only after `/checks accept`; `/skills` shows each one's use and the recent learning runs.
- **Extends.** MCP servers (`/mcp add` in chat, or `ha mcp add`), prompt templates and hooks.

### Platform status

| Platform | Status |
| --- | --- |
| Windows 10/11 x64 | Supported; all CI gates run here |
| Linux | **Pending support.** It builds and its CI job runs for visibility, but it is not a supported platform yet and does not block CI |
| macOS | Not tested |

### Install from npm

```powershell
npm install -g harness-agents
ha --version
```

This installs the Windows x64 release build; npm puts `ha` in its global directory (`npm prefix -g`), which Node's installer keeps on your PATH. If `ha` is not found in a new terminal, add that directory to your PATH. To build from source instead, follow the quick start below.

### Quick start (Windows, PowerShell 7)

Prerequisites: [Rust](https://rustup.rs) (the toolchain in [`rust-toolchain.toml`](rust-toolchain.toml) is installed automatically), Git, and PowerShell 7 (`pwsh`).

```powershell
git clone https://github.com/DEVfancybear/harness-agents.git
cd harness-agents
pwsh -NoProfile -File scripts/Install-Ha.ps1        # release build, installed to %USERPROFILE%\.cargo\bin
ha --version
ha                                                   # open the app, then /login to pick a provider
```

**Updating:** pull, then run `scripts/Install-Ha.ps1` again. Building with `cargo build --release` alone does **not** update the `ha` you type: that command runs the copy in `.cargo\bin`. If the app behaves like an older version, check which binary runs:

```powershell
Get-Command ha -All | Select-Object Source
```

To try the interface without a provider or a key: `ha chat --fixture`, or `ha exec --mock "hello" --output-format json`.

Building a release package (checksums, manifest) is described in [docs/BUILD_AND_RELEASE.md](docs/BUILD_AND_RELEASE.md).

### Everyday use

| You want to | Type |
| --- | --- |
| Log in to a provider | `/login` (DeepSeek, OpenAI, Anthropic, OpenCode Zen/Go with an API key; ChatGPT Plus/Pro with a browser sign-in), `/logout` |
| Pick a model | `/model` opens the menu of models your logins can use; `/model opencode/kimi-k2.6` picks one |
| See every command | `/` opens the menu (fuzzy: `/skil` lists the skills), `/help`, `/hotkeys` |
| Continue an earlier conversation | `/resume`, then pick one |
| Start over | `/new` |
| Attach a file or image | `@` (file picker), `/attach <path>`, `/image` |
| Run a shell command | `!command` (goes through approval) |
| Correct a running turn | `/steer <text>` |
| Choose how much the model reasons | `/effort` (or `/thinking`), then pick a level from the menu |
| Keep working until something is done | `/goal <objective>`, then `/goal status`, `/goal pause`, `/goal resume`, `/goal clear` |
| Shrink a long conversation | `/compact [what to keep]` |
| See cost, context, permissions | `/cost`, `/context`, `/permissions` |

Keys: Enter sends, Ctrl-J, Alt+Enter or Shift+Enter (where the terminal reports Shift) adds a line, Ctrl-V (or Alt-V where the terminal keeps Ctrl-V) pastes a screenshot or files copied in Explorer as attachments, ↑↓ recall history or move in a menu, Esc closes a panel or interrupts a run, Ctrl-C cancels a run (pressed twice on an empty prompt it exits), Ctrl-O cycles the detail mode (collapsed, details, expanded), Ctrl-S puts the draft aside and brings it back, Alt+M / Shift+Alt+M move through the scoped models, Ctrl-D on an empty line exits. The TUI labels user and assistant messages, indents tool calls with status symbols, and puts keyboard hints and status below a rounded input box. See [Terminal interface](docs/TUI.md) for layout, compatibility and testing.

### Configuration

Settings merge default → user `config.toml` → trusted project `.harness/config.toml` → environment → command line; `/config` shows where each value came from. The most used environment variables:

| Variable | Default | Meaning |
| --- | --- | --- |
| `DEEPSEEK_API_KEY`, `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `OPENCODE_API_KEY` | — | Provider keys; a key saved with `/login` (in `auth.json`) wins over them |
| `HA_PROVIDER_ENDPOINT` | `https://api.deepseek.com/chat/completions` | Any OpenAI-compatible chat endpoint |
| `HA_PROVIDER_MODEL` | `deepseek-v4-flash` | Model name |
| `HA_TURN_MAX_STEPS` / `HA_TURN_MAX_TOOL_CALLS` | no limit | Model calls and tool calls per turn before it pauses; unset, a turn runs until the model is done, you stop it, or the tokens run out |
| `HA_TURN_DEADLINE_SECONDS` / `HA_TURN_CONTINUATIONS` | 900 / 2 | Time per turn; automatic continuations after a bound |
| `HA_SKILL_PATHS` | — | Extra skill directories, separated like `PATH` |
| `HA_AUTO_REFINE` | on | `off` stops the automatic refine review |
| `HA_SKILL_LIFECYCLE` | on | `off` keeps every learned skill, used or not |
| `SERPER_API_KEY` | — | Google results for `web_search` (without it: DuckDuckGo) |
| `HA_WEB` | on | `off` removes `web_search` and `web_fetch` |
| `HA_REPL` | on | `off` removes the `ipython` tool |
| `HA_PYTHON` | `python3`, `python`, `py -3` | Interpreter for the REPL kernel (3.11 or newer) |
| `HA_REPL_SHELL` | Git Bash on Windows, `/bin/bash` | Absolute path of the POSIX shell `bash()` runs in |
| `HA_UI` | auto | `plain` forces line mode |

### Development

```powershell
cargo build -p harness-cli --bin ha --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
pwsh -NoProfile -File scripts/Verify-Docs.ps1
```

CI (`.github/workflows/ci.yml`) runs the phase gates (`scripts/Verify-Phase.ps1`) and milestone gates (`scripts/Verify-Milestone.ps1`) on Windows. A test that fails in the crowded full-suite run is re-run alone before the gate fails; the log names it with `GATE_TEST_FAILED` / `GATE_TEST_RETRIED`.

### Documentation

- [Operator guide](docs/OPERATOR_GUIDE.en.md) — installing, configuring and running `ha`
- [Build and release](docs/BUILD_AND_RELEASE.md) — building, installing and packaging a release candidate
- [Architecture review](docs/ARCHITECTURE_REVIEW.en.md) · [Plugin architecture](docs/PLUGIN_ARCHITECTURE.en.md) · [Rust harness plan](docs/RUST_HARNESS_PLAN.en.md)

## Tiếng Việt

### `ha` làm được gì

- **Chat trong terminal.** `ha` mở ứng dụng tương tác (TUI, hoặc chế độ dòng lệnh thuần). `ha exec "…"` chạy một prompt cho script và CI, xuất `text`, `json` hoặc `stream-json`.
- **Làm việc với code qua công cụ có kiểm soát.** Đọc, tìm kiếm, glob, sửa/ghi file, chạy tiến trình và lệnh shell, xem Git, hỏi lại bạn, giao việc cho agent con. Mọi thao tác đi qua chính sách của host: `y` chạy một lần, `a` cho phép đến hết lượt, `n` từ chối; `/permissions` chọn `ask`, `auto-edit` hoặc `full-auto` từ menu (`full-auto` không hỏi gì; rule deny vẫn áp dụng).
- **Đọc web.** `web_search` (Google qua Serper khi có `SERPER_API_KEY`, DuckDuckGo khi không có key) và `web_fetch` (một trang dưới dạng văn bản kèm link) hỗ trợ các quy trình nghiên cứu. Địa chỉ local và mạng nội bộ bị từ chối.
- **Python REPL bền.** Tool `ipython` là kernel của prime-agent: biến được giữ qua các cell và các lượt, `bash('cmd')` chạy lệnh nền và trả về handle, `await rlm.spawn(...)` / `rlm.collect(...)` chạy song song các agent con (explorer). Các Python skill của prime-agent được import sẵn (`edit`, `websearch`, `attach_image`, `goal`, `compact`, `refine`, …). Khi có `uv`, kernel có venv riêng với bộ package mặc định của prime-agent; nếu không thì chạy trên Python 3.11+ của hệ thống. Trên Windows cần Git Bash cho `bash()`. Mỗi cell hỏi phê duyệt như `run_shell`, trừ chế độ `full-auto`.
- **Làm tới khi xong mục tiêu.** `/goal <mục tiêu>` giữ ứng dụng làm việc qua nhiều lượt cho tới khi model gọi `goal_complete` (tối đa 10 lượt tự động, sau đó tạm dừng). `/goal status|pause|resume|clear`; Ctrl-C tạm dừng mục tiêu, `/resume` khôi phục nó ở trạng thái tạm dừng. Lượt dài tự rút gọn các kết quả tool cũ nhất để không vượt ngân sách context.
- **Tiếp tục hội thoại.** `/resume` liệt kê các hội thoại (mỗi hội thoại một dòng) và phát lại các câu hỏi, câu trả lời của hội thoại được chọn cho model và trên màn hình.
- **Dùng skill.** Thư mục Agent Skills (`SKILL.md` cùng references và scripts) từ bộ tích hợp sẵn, `~/.agents/skills`, `.agents/skills` của project đã trust, hoặc bất kỳ thư mục nào trong `HA_SKILL_PATHS` (ví dụ `~/.claude/skills`). Model kích hoạt skill phù hợp theo tên và đọc file của skill bằng `read_skill_file`; `/skills` liệt kê, `/skill:<name>` chạy một skill.
- **Tự kiểm chứng.** `[verify]` trong `.harness/config.toml` liệt kê các check của project (types, tests, ...); harness tự chạy chúng trước khi một goal hoàn thành, làm gate mặc định của `/autonomous` và khi gõ `/verify`, rồi một agent `verifier` độc lập (context mới, không được sửa file) chấm goal trước khi kết thúc. `.harness/features.json` là feature list mà agent làm từng feature một; chỉ lượt kiểm chứng do harness chạy mới đánh dấu feature là passing. `/review` và `/features` lo phần còn lại.
- **Bàn giao giữa các phiên.** Mỗi phiên bắt đầu với phần tóm tắt commit gần đây, file chưa commit và handoff cuối; `/handoff` ghi `.harness/progress.md` cho phiên sau (`--reset` tiếp tục trong hội thoại mới), và khi thoát harness nêu những gì còn dở. `/doctor` (hoặc `ha doctor`) audit mức sẵn sàng của repo cho agent, kèm cách sửa cho từng chỗ thiếu.
- **Tự học skill.** `/refine` (và lượt tự xem xét mỗi 25 lượt hoặc 50 lần gọi tool, chạy nền) có thể tự viết skill `SKILL.md` vào `.harness/skills` (project đã trust) hoặc `<config-dir>/skills` (global). Bộ kiểm tra chỉ ghi skill qua được lint (định dạng, độ dài, an toàn, secret) và lưu sổ lý do mỗi lần thay đổi. Lượt dùng được đếm; skill không ai dùng hết thời gian thử việc sẽ bị chuyển vào `.archive/`. `/learn [trọng tâm]` giữ lại một bài học ngay; bài học mà một lệnh phát hiện được có thể được đề xuất thành check, và chỉ vào `[verify]` sau `/checks accept`; `/skills` hiện số lượt dùng của từng skill và các lượt học gần đây.
- **Mở rộng.** MCP server (`/mcp add` trong chat, hoặc `ha mcp add`), prompt template và hook.

### Nền tảng

| Nền tảng | Trạng thái |
| --- | --- |
| Windows 10/11 x64 | Hỗ trợ; mọi gate CI chạy trên Windows |
| Linux | **Đang chờ hỗ trợ.** Vẫn build được và job CI vẫn chạy để theo dõi, nhưng chưa phải nền tảng được hỗ trợ và không chặn CI |
| macOS | Chưa kiểm thử |

### Cài từ npm

```powershell
npm install -g harness-agents
ha --version
```

Lệnh này cài bản release Windows x64; npm đặt `ha` vào thư mục global của nó (`npm prefix -g`), thư mục mà trình cài Node giữ trong PATH. Nếu mở terminal mới mà không thấy `ha`, hãy thêm thư mục đó vào PATH. Muốn build từ mã nguồn thì làm theo phần bắt đầu nhanh bên dưới.

### Bắt đầu nhanh (Windows, PowerShell 7)

Cần có: [Rust](https://rustup.rs) (toolchain trong [`rust-toolchain.toml`](rust-toolchain.toml) được cài tự động), Git và PowerShell 7 (`pwsh`).

```powershell
git clone https://github.com/DEVfancybear/harness-agents.git
cd harness-agents
pwsh -NoProfile -File scripts/Install-Ha.ps1        # build release, cài vào %USERPROFILE%\.cargo\bin
ha --version
ha                                                   # mở app, rồi /login để chọn provider
```

**Cập nhật:** `git pull` rồi chạy lại `scripts/Install-Ha.ps1`. Chỉ chạy `cargo build --release` thì **không** cập nhật lệnh `ha` bạn gõ, vì lệnh đó chạy bản trong `.cargo\bin`. Nếu app vẫn chạy như bản cũ, kiểm tra xem đang chạy file nào:

```powershell
Get-Command ha -All | Select-Object Source
```

Thử giao diện mà không cần provider hay key: `ha chat --fixture`, hoặc `ha exec --mock "hello" --output-format json`.

Cách build gói release (checksum, manifest) nằm trong [docs/BUILD_AND_RELEASE.md](docs/BUILD_AND_RELEASE.md).

### Dùng hằng ngày

| Bạn muốn | Gõ |
| --- | --- |
| Đăng nhập provider | `/login` (DeepSeek, OpenAI, Anthropic, OpenCode Zen/Go bằng API key; ChatGPT Plus/Pro bằng đăng nhập trình duyệt), `/logout` |
| Chọn model | `/model` mở menu các model mà tài khoản đã đăng nhập dùng được; `/model opencode/kimi-k2.6` chọn thẳng |
| Xem mọi lệnh | `/` mở menu (tìm mờ: `/skil` liệt kê các skill), `/help`, `/hotkeys` |
| Tiếp tục hội thoại cũ | `/resume` rồi chọn |
| Bắt đầu lại | `/new` |
| Đính kèm file hoặc ảnh | `@` (chọn file), `/attach <path>`, `/image` |
| Chạy lệnh shell | `!command` (đi qua bước phê duyệt) |
| Chỉnh hướng một lượt đang chạy | `/steer <text>` |
| Chọn mức suy luận của model | `/effort` (hoặc `/thinking`) rồi chọn mức trong menu |
| Làm tới khi xong việc | `/goal <mục tiêu>`, rồi `/goal status`, `/goal pause`, `/goal resume`, `/goal clear` |
| Rút gọn hội thoại dài | `/compact [điều cần giữ]` |
| Xem chi phí, context, quyền | `/cost`, `/context`, `/permissions` |

Phím: Enter gửi, Ctrl-J hoặc Alt+Enter xuống dòng, Ctrl-V (hoặc Alt-V khi terminal giữ Ctrl-V) dán ảnh chụp màn hình hoặc file đã copy trong Explorer làm tệp đính kèm, ↑↓ gọi lại lịch sử hoặc di chuyển trong menu, Esc đóng panel hoặc ngắt lượt đang chạy, Ctrl-C hủy lượt đang chạy (bấm hai lần trên dòng trống để thoát), Ctrl-O đổi chế độ chi tiết (thu gọn, chi tiết, mở rộng), Ctrl-S cất bản nháp và lấy lại, Alt+M / Shift+Alt+M đổi qua các model trong phạm vi, Ctrl-D trên dòng trống để thoát. TUI có nhãn BẠN/HA, tool call thụt vào kèm ký hiệu trạng thái, ô nhập bo góc và gợi ý phím cùng dòng trạng thái ở bên dưới. Xem [hướng dẫn giao diện terminal](docs/TUI.md).

### Cấu hình

Cấu hình được gộp theo thứ tự mặc định → `config.toml` của người dùng → `.harness/config.toml` của project đã trust → biến môi trường → dòng lệnh; `/config` cho biết mỗi giá trị đến từ đâu. Các biến môi trường hay dùng:

| Biến | Mặc định | Ý nghĩa |
| --- | --- | --- |
| `DEEPSEEK_API_KEY`, `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `OPENCODE_API_KEY` | — | Khóa provider; khóa lưu bằng `/login` (trong `auth.json`) được ưu tiên hơn |
| `HA_PROVIDER_ENDPOINT` | `https://api.deepseek.com/chat/completions` | Bất kỳ endpoint chat tương thích OpenAI |
| `HA_PROVIDER_MODEL` | `deepseek-v4-flash` | Tên model |
| `HA_TURN_MAX_STEPS` / `HA_TURN_MAX_TOOL_CALLS` | không giới hạn | Số lời gọi model và tool mỗi lượt trước khi tạm dừng; không đặt thì lượt chạy tới khi model xong, bạn dừng, hoặc hết token |
| `HA_TURN_DEADLINE_SECONDS` / `HA_TURN_CONTINUATIONS` | 900 / 2 | Thời gian mỗi lượt; số lần tự tiếp tục sau khi chạm giới hạn |
| `HA_SKILL_PATHS` | — | Thư mục skill bổ sung, phân tách như `PATH` |
| `HA_AUTO_REFINE` | on | `off` tắt lượt tự xem xét refine |
| `HA_SKILL_LIFECYCLE` | on | `off` giữ mọi skill đã học, kể cả skill không dùng |
| `SERPER_API_KEY` | — | Kết quả Google cho `web_search` (không có thì dùng DuckDuckGo) |
| `HA_WEB` | bật | `off` để gỡ `web_search` và `web_fetch` |
| `HA_REPL` | bật | `off` để gỡ tool `ipython` |
| `HA_PYTHON` | `python3`, `python`, `py -3` | Trình thông dịch cho kernel REPL (3.11 trở lên) |
| `HA_REPL_SHELL` | Git Bash trên Windows, `/bin/bash` | Đường dẫn tuyệt đối tới shell POSIX mà `bash()` dùng |
| `HA_UI` | tự động | `plain` để ép chế độ dòng lệnh |

### Phát triển

```powershell
cargo build -p harness-cli --bin ha --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
pwsh -NoProfile -File scripts/Verify-Docs.ps1
```

CI (`.github/workflows/ci.yml`) chạy các gate phase (`scripts/Verify-Phase.ps1`) và milestone (`scripts/Verify-Milestone.ps1`) trên Windows. Test nào fail trong lượt chạy toàn bộ sẽ được chạy lại riêng trước khi gate báo đỏ; log ghi rõ bằng `GATE_TEST_FAILED` / `GATE_TEST_RETRIED`.

### Tài liệu

- [Hướng dẫn vận hành](docs/OPERATOR_GUIDE.vi.md) — cài đặt, cấu hình và chạy `ha`
- [Build và release](docs/BUILD_AND_RELEASE.md) — build, cài đặt và đóng gói bản release
- [Đánh giá kiến trúc](docs/ARCHITECTURE_REVIEW.vi.md) · [Kiến trúc plugin](docs/PLUGIN_ARCHITECTURE.vi.md) · [Kế hoạch Rust harness](docs/RUST_HARNESS_PLAN.vi.md)

---

Design references / Nguồn tham khảo thiết kế: [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness/tree/2377c272a8e839e0a84c9f0e623b867a1dce2014) · [TencentDB Agent Memory](https://github.com/TencentCloud/TencentDB-Agent-Memory/tree/906b5823b5106eed8f842b62f16d23228838149a) · [pi](https://github.com/earendil-works/pi/tree/d5629e20489ccf770ed90b5a33941cb3b7ef24d0) · [deer-flow](https://github.com/bytedance/deer-flow/tree/29dbce45a12cffe4ce7c395dde6e4b913748596b).
