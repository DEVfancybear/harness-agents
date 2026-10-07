# Harness Agents

[![npm](https://img.shields.io/npm/v/harness-agents?label=npm&color=0f766e)](https://www.npmjs.com/package/harness-agents)
[![release](https://img.shields.io/github/v/release/DEVfancybear/harness-agents?color=0f766e)](https://github.com/DEVfancybear/harness-agents/releases)
![platform](https://img.shields.io/badge/platform-Windows%20x64-0f766e)
![rust](https://img.shields.io/badge/built%20with-Rust-0f766e)

**A terminal coding agent whose harness, not the model, decides when the work is done.**
**Coding agent chạy trong terminal, nơi harness chứ không phải model quyết định khi nào việc đã xong.**

`ha` reads, edits and runs your code through approval-gated tools, keeps every conversation in a local SQLite store, checks its own work with your project's checks and an independent verifier, and learns reusable skills from the sessions you have with it. It works with DeepSeek, OpenAI, Anthropic, ChatGPT Plus/Pro and OpenCode, and keeps their prompt caches hitting from one step to the next.

**Language / Ngôn ngữ:** [English](#english) · [Tiếng Việt](#tiếng-việt)

```powershell
npm install -g harness-agents
ha                      # then /login and start typing
```

---

## English

### Highlights

| | |
| --- | --- |
| **Guarded tools** | Read, search, patch and write files, run shell commands and processes, inspect Git, fetch and search the web. Every action passes the host policy: `ask`, `auto-edit` or `full-auto`, with deny rules that always hold. |
| **Persistent Python REPL** | prime-agent's kernel: variables survive across turns, `bash()` runs commands in the background, `rlm.spawn()` fans out explorer agents in parallel. |
| **Verified work** | `[verify]` checks the harness runs itself, an independent `verifier` agent that cannot edit, goals that only complete when both pass, and a feature list only a real verification can mark passing. |
| **Long-running work** | `/goal` keeps working across turns, `ha exec --autonomous` loops until your gates pass, and background agents keep running after the terminal closes. |
| **Session continuity** | Every session starts from a brief of recent commits, uncommitted files and the last `/handoff`; `/resume`, `/tree` and forks move around a conversation's history. |
| **Learned skills** | `/refine`, `/learn` and a background review write `SKILL.md` skills that pass a lint, are counted when used and archived when never used. |
| **Prompt caching** | A request's prefix stays byte-for-byte still across steps and turns; each provider gets its cache key, breakpoints and retention, and the status line shows `⚡ cache NN%`. |
| **Open ecosystem** | Agent Skills, MCP servers with OAuth sign-in and a service catalog (`/plugins`), prompt templates, hooks, `ha package` (npm, git, local), and `--mode rpc` / `--mode acp` for editors. |

### Install

**From npm** (Windows x64 release build):

```powershell
npm install -g harness-agents
ha --version
```

npm puts `ha` in its global directory (`npm prefix -g`), which Node's installer keeps on your PATH.

**From source** (needs [Rust](https://rustup.rs), Git and PowerShell 7):

```powershell
git clone https://github.com/DEVfancybear/harness-agents.git
cd harness-agents
pwsh -NoProfile -File scripts/Install-Ha.ps1        # release build into %USERPROFILE%\.cargo\bin
```

To update a source install, pull and run `Install-Ha.ps1` again; `cargo build` alone does not replace the `ha` on your PATH (`Get-Command ha -All` shows which one runs). No key yet? `ha chat --fixture` opens the interface against a scripted provider.

### First run

```text
ha                         open the app
/login                     DeepSeek, OpenAI, Anthropic, OpenCode (API key) or ChatGPT Plus/Pro (browser)
/model                     pick a model your logins can use
/effort                    pick how much it reasons
```

`ha "fix the failing test"` opens the app with a first prompt; `ha exec "…" --output-format json` runs one prompt for scripts and CI.

### Everyday commands

| You want to | Type |
| --- | --- |
| See every command | `/` (fuzzy menu), `/help`, `/hotkeys` |
| Continue or branch a conversation | `/resume`, `/tree`, `/fork`, `/new` |
| Attach files or images | `@`, `/attach <path>`, Ctrl-V for screenshots |
| Run a shell command | `!command` (goes through approval) |
| Redirect a running turn | `/steer <text>`, or just type while it runs |
| Work until something is done | `/goal <objective>`, `/goal status\|pause\|resume\|clear` |
| Check the work | `/verify`, `/review`, `/features` |
| Hand over to the next session | `/handoff`, `/handoff --reset` |
| Keep a lesson | `/learn [focus]`, `/refine`, `/skills` |
| Shrink a long conversation | `/compact [what to keep]` |
| Connect services | `/plugins`, `/mcp add`, `/mcp login <service>` |
| See cost, context, permissions | `/cost`, `/context`, `/permissions` |
| Audit the repository for agents | `/doctor` or `ha doctor` |

Keys: Enter sends, Shift+Enter / Alt+Enter / Ctrl-J add a line, Esc interrupts, Ctrl-C cancels (twice on an empty prompt exits), Ctrl-O cycles collapsed / details / expanded, Ctrl-S stashes the draft, Alt+M cycles models. See [docs/TUI.md](docs/TUI.md).

### Background agents and automation

```powershell
ha agents                          # the agents still running for this project
ha attach <name>                   # reattach this terminal
ha send <name> "next step" --wait  # wake a saved conversation
ha exec --autonomous --autonomous-gate "cargo test" "make the suite pass"
ha --mode rpc                      # JSON commands on stdin, events on stdout
ha --mode acp                      # Agent Client Protocol over stdio
```

A per-project worker keeps sessions alive after the terminal closes and recovers them from its journal (`HA_DAEMON=off` disables it).

### Configuration

Settings merge default → user `config.toml` → trusted project `.harness/config.toml` → environment → command line; `/config` shows where each value came from. Project checks live under `[verify]`:

```toml
[[verify.checks]]
name = "tests"
command = "cargo test --workspace"
hint = "Fix the failing test before finishing."
```

| Variable | Default | Meaning |
| --- | --- | --- |
| `DEEPSEEK_API_KEY`, `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `OPENCODE_API_KEY` | — | Provider keys; a key saved with `/login` wins |
| `HA_PROVIDER_ENDPOINT` / `HA_PROVIDER_MODEL` | DeepSeek / `deepseek-v4-flash` | Any OpenAI-compatible chat endpoint and model |
| `HA_CACHE_RETENTION` | short | `long`: Anthropic's 1-hour cache, OpenAI's 24-hour retention |
| `HA_TURN_MAX_STEPS` / `HA_TURN_MAX_TOOL_CALLS` | no limit | Model and tool calls per turn before it pauses |
| `HA_TURN_DEADLINE_SECONDS` | 900 | Time per turn |
| `HA_SKILL_PATHS` | — | Extra skill directories (e.g. `~/.claude/skills`) |
| `HA_AUTO_REFINE` / `HA_SKILL_LIFECYCLE` | on | `off` stops the background review / keeps unused learned skills |
| `SERPER_API_KEY` | — | Google results for `web_search` (DuckDuckGo without it) |
| `HA_WEB` / `HA_REPL` / `HA_DAEMON` | on | `off` removes web tools / the Python REPL / background agents |
| `HA_PYTHON` | `python3`, `python`, `py -3` | Interpreter for the REPL kernel (3.11+) |
| `HA_UI` | auto | `plain` forces line mode |

### Platform

| Platform | Status |
| --- | --- |
| Windows 10/11 x64 | Supported; every CI gate runs here |
| Linux | Pending: it builds and CI runs it, but it is not supported yet |
| macOS | Not tested |

### Development

```powershell
cargo build -p harness-cli --bin ha --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
pwsh -NoProfile -File scripts/Verify-Docs.ps1
pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1   # the terminal-driven suite
```

CI runs the phase and milestone gates on Windows; a test that fails in the crowded full run is re-run alone before the gate fails.

### Documentation

- [Operator guide](docs/OPERATOR_GUIDE.en.md): every command, setting and workflow
- [Architecture overview](docs/ARCHITECTURE_OVERVIEW.en.md) · [Terminal interface](docs/TUI.md) · [Plugin architecture](docs/PLUGIN_ARCHITECTURE.en.md)
- [Build and release](docs/BUILD_AND_RELEASE.md) · [Changelog](https://github.com/DEVfancybear/harness-agents/releases)

---

## Tiếng Việt

### Điểm nổi bật

| | |
| --- | --- |
| **Công cụ có kiểm soát** | Đọc, tìm, sửa và ghi file, chạy lệnh shell và tiến trình, xem Git, tìm và đọc web. Mọi thao tác đi qua chính sách của host: `ask`, `auto-edit` hoặc `full-auto`, rule deny luôn được giữ. |
| **Python REPL bền** | Kernel của prime-agent: biến được giữ qua các lượt, `bash()` chạy lệnh nền, `rlm.spawn()` chạy song song các agent khám phá. |
| **Kiểm chứng thật** | Các check `[verify]` do harness tự chạy, agent `verifier` độc lập không được sửa file, goal chỉ hoàn thành khi cả hai qua, và feature list chỉ được đánh dấu passing bằng một lượt kiểm chứng thật. |
| **Việc dài hơi** | `/goal` làm tiếp qua nhiều lượt, `ha exec --autonomous` lặp tới khi các gate qua, agent nền vẫn chạy sau khi đóng terminal. |
| **Liền mạch giữa các phiên** | Mỗi phiên bắt đầu với tóm tắt commit gần đây, file chưa commit và `/handoff` cuối; `/resume`, `/tree` và fork để đi lại trong lịch sử hội thoại. |
| **Tự học skill** | `/refine`, `/learn` và lượt xem xét nền viết skill `SKILL.md` qua lint, được đếm khi dùng và lưu trữ khi không ai dùng. |
| **Prompt caching** | Phần đầu request giữ nguyên từng byte qua các bước và các lượt; mỗi provider nhận cache key, breakpoint và thời hạn riêng, thanh trạng thái hiện `⚡ cache NN%`. |
| **Hệ sinh thái mở** | Agent Skills, MCP server có đăng nhập OAuth và danh mục dịch vụ (`/plugins`), prompt template, hook, `ha package` (npm, git, local), và `--mode rpc` / `--mode acp` cho editor. |

### Cài đặt

**Từ npm** (bản release Windows x64):

```powershell
npm install -g harness-agents
ha --version
```

npm đặt `ha` vào thư mục global của nó (`npm prefix -g`), thư mục mà trình cài Node giữ trong PATH.

**Từ mã nguồn** (cần [Rust](https://rustup.rs), Git và PowerShell 7):

```powershell
git clone https://github.com/DEVfancybear/harness-agents.git
cd harness-agents
pwsh -NoProfile -File scripts/Install-Ha.ps1        # build release vào %USERPROFILE%\.cargo\bin
```

Cập nhật bản build từ nguồn: `git pull` rồi chạy lại `Install-Ha.ps1`; chỉ `cargo build` thì không thay `ha` trong PATH (`Get-Command ha -All` cho biết bản nào đang chạy). Chưa có key? `ha chat --fixture` mở giao diện với provider giả lập.

### Lần chạy đầu

```text
ha                         mở app
/login                     DeepSeek, OpenAI, Anthropic, OpenCode (API key) hoặc ChatGPT Plus/Pro (trình duyệt)
/model                     chọn model mà tài khoản dùng được
/effort                    chọn mức suy luận
```

`ha "sửa test đang fail"` mở app kèm prompt đầu tiên; `ha exec "…" --output-format json` chạy một prompt cho script và CI.

### Lệnh hằng ngày

| Bạn muốn | Gõ |
| --- | --- |
| Xem mọi lệnh | `/` (menu tìm mờ), `/help`, `/hotkeys` |
| Tiếp tục hoặc rẽ nhánh hội thoại | `/resume`, `/tree`, `/fork`, `/new` |
| Đính kèm file hoặc ảnh | `@`, `/attach <path>`, Ctrl-V để dán ảnh chụp |
| Chạy lệnh shell | `!command` (đi qua phê duyệt) |
| Chỉnh hướng lượt đang chạy | `/steer <text>`, hoặc gõ luôn khi nó đang chạy |
| Làm tới khi xong việc | `/goal <mục tiêu>`, `/goal status\|pause\|resume\|clear` |
| Kiểm tra kết quả | `/verify`, `/review`, `/features` |
| Bàn giao cho phiên sau | `/handoff`, `/handoff --reset` |
| Giữ lại bài học | `/learn [trọng tâm]`, `/refine`, `/skills` |
| Rút gọn hội thoại dài | `/compact [điều cần giữ]` |
| Kết nối dịch vụ | `/plugins`, `/mcp add`, `/mcp login <service>` |
| Xem chi phí, context, quyền | `/cost`, `/context`, `/permissions` |
| Audit repo cho agent | `/doctor` hoặc `ha doctor` |

Phím: Enter gửi, Shift+Enter / Alt+Enter / Ctrl-J xuống dòng, Esc ngắt lượt, Ctrl-C hủy (bấm hai lần trên dòng trống để thoát), Ctrl-O đổi thu gọn / chi tiết / mở rộng, Ctrl-S cất bản nháp, Alt+M đổi model. Xem [docs/TUI.md](docs/TUI.md).

### Agent nền và tự động hóa

```powershell
ha agents                          # các agent còn chạy của project
ha attach <name>                   # gắn lại terminal này
ha send <name> "bước tiếp" --wait  # đánh thức một hội thoại đã lưu
ha exec --autonomous --autonomous-gate "cargo test" "làm cho bộ test qua"
ha --mode rpc                      # lệnh JSON qua stdin, sự kiện qua stdout
ha --mode acp                      # Agent Client Protocol qua stdio
```

Mỗi project có một worker giữ các phiên sống sau khi đóng terminal và khôi phục chúng từ journal (`HA_DAEMON=off` để tắt).

### Cấu hình

Cấu hình được gộp theo thứ tự mặc định → `config.toml` của người dùng → `.harness/config.toml` của project đã trust → biến môi trường → dòng lệnh; `/config` cho biết mỗi giá trị đến từ đâu. Check của project nằm trong `[verify]`:

```toml
[[verify.checks]]
name = "tests"
command = "cargo test --workspace"
hint = "Sửa test đang fail trước khi kết thúc."
```

| Biến | Mặc định | Ý nghĩa |
| --- | --- | --- |
| `DEEPSEEK_API_KEY`, `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `OPENCODE_API_KEY` | — | Khóa provider; khóa lưu bằng `/login` được ưu tiên |
| `HA_PROVIDER_ENDPOINT` / `HA_PROVIDER_MODEL` | DeepSeek / `deepseek-v4-flash` | Endpoint chat tương thích OpenAI và model bất kỳ |
| `HA_CACHE_RETENTION` | short | `long`: cache 1 giờ của Anthropic, giữ 24 giờ của OpenAI |
| `HA_TURN_MAX_STEPS` / `HA_TURN_MAX_TOOL_CALLS` | không giới hạn | Số lời gọi model và tool mỗi lượt trước khi tạm dừng |
| `HA_TURN_DEADLINE_SECONDS` | 900 | Thời gian mỗi lượt |
| `HA_SKILL_PATHS` | — | Thư mục skill bổ sung (ví dụ `~/.claude/skills`) |
| `HA_AUTO_REFINE` / `HA_SKILL_LIFECYCLE` | bật | `off` tắt lượt xem xét nền / giữ cả skill không dùng |
| `SERPER_API_KEY` | — | Kết quả Google cho `web_search` (không có thì DuckDuckGo) |
| `HA_WEB` / `HA_REPL` / `HA_DAEMON` | bật | `off` gỡ tool web / Python REPL / agent nền |
| `HA_PYTHON` | `python3`, `python`, `py -3` | Trình thông dịch cho kernel REPL (3.11+) |
| `HA_UI` | tự động | `plain` ép chế độ dòng lệnh |

### Nền tảng

| Nền tảng | Trạng thái |
| --- | --- |
| Windows 10/11 x64 | Hỗ trợ; mọi gate CI chạy ở đây |
| Linux | Đang chờ: build được và CI có chạy, nhưng chưa được hỗ trợ |
| macOS | Chưa kiểm thử |

### Phát triển

```powershell
cargo build -p harness-cli --bin ha --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
pwsh -NoProfile -File scripts/Verify-Docs.ps1
pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1   # bộ test điều khiển qua terminal
```

CI chạy các gate phase và milestone trên Windows; test fail trong lượt chạy toàn bộ được chạy lại riêng trước khi gate báo đỏ.

### Tài liệu

- [Hướng dẫn vận hành](docs/OPERATOR_GUIDE.vi.md): mọi lệnh, cấu hình và quy trình
- [Tổng quan kiến trúc](docs/ARCHITECTURE_OVERVIEW.vi.md) · [Giao diện terminal](docs/TUI.md) · [Kiến trúc plugin](docs/PLUGIN_ARCHITECTURE.vi.md)
- [Build và release](docs/BUILD_AND_RELEASE.md) · [Changelog](https://github.com/DEVfancybear/harness-agents/releases)

---

Design references / Nguồn tham khảo thiết kế: [prime-agent](https://github.com/PrimeIntellect-ai/prime-agent) · [pi](https://github.com/earendil-works/pi/tree/d5629e20489ccf770ed90b5a33941cb3b7ef24d0) · [Codex](https://github.com/openai/codex) · [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness/tree/2377c272a8e839e0a84c9f0e623b867a1dce2014) · [learn-harness-engineering](https://github.com/walkinglabs/learn-harness-engineering) · [TencentDB Agent Memory](https://github.com/TencentCloud/TencentDB-Agent-Memory/tree/906b5823b5106eed8f842b62f16d23228838149a) · [deer-flow](https://github.com/bytedance/deer-flow/tree/29dbce45a12cffe4ce7c395dde6e4b913748596b).
