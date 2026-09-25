# Harness Agents

**A local coding agent that remembers what it did and picks up where it left off.** / **Coding agent chạy trên máy, nhớ việc đã làm và tiếp tục đúng chỗ đã dừng.**

`ha` is a coding agent written in Rust. It runs in your terminal against your project, calls a model provider (DeepSeek by default), reads and edits code through approval-gated tools, and keeps every conversation, tool result and learned fact in a local SQLite store, so a session can be resumed, inspected or continued later.

**Language / Ngôn ngữ:** [English](#english) · [Tiếng Việt](#tiếng-việt)

## English

### What it does

- **Chat in the terminal.** `ha` opens an interactive app (TUI, or plain line mode). `ha exec "…"` runs one prompt for scripts and CI, with `text`, `json` or `stream-json` output.
- **Works on your code with guarded tools.** Read, search, glob, patch/edit/write files, run processes and shell commands, inspect Git, ask you a question, delegate to a sub-agent. Every action goes through the host policy: `y` runs once, `a` allows the rest of the turn, `n` refuses; `/mode` sets `ask`, `auto-edit` or `full-auto`.
- **Resumes conversations.** `/resume` lists your conversations (one row each) and replays the chosen one's questions and answers to the model and on screen.
- **Learns from the conversation.** Memory is on by default: after a turn, the model extracts durable facts (preferences, decisions, conventions, corrections) in the background; facts with confidence ≥ 0.7 are used from then on, the rest wait for review in `ha memory candidates`. Say "remember that …" / "ghi nhớ …" to store something verbatim.
- **Uses skills.** Agent Skills directories (`SKILL.md` plus references and scripts) from the bundled set, `~/.agents/skills`, a trusted project's `.agents/skills`, or any folder in `HA_SKILL_PATHS` (e.g. `~/.claude/skills`). The model activates a matching skill by name and reads its files with `read_skill_file`; `/skills` lists them, `/skill:<name>` runs one.
- **Extends.** MCP servers (`ha mcp add`), prompt templates, hooks, and a local web surface (`ha web`).

### Platform status

| Platform | Status |
| --- | --- |
| Windows 10/11 x64 | Supported; all CI gates run here |
| Linux | **Pending support.** It builds and its CI job runs for visibility, but it is not a supported platform yet and does not block CI |
| macOS | Not tested |

### Quick start (Windows, PowerShell 7)

Prerequisites: [Rust](https://rustup.rs) (the toolchain in [`rust-toolchain.toml`](rust-toolchain.toml) is installed automatically), Git, and PowerShell 7 (`pwsh`).

```powershell
git clone https://github.com/DEVfancybear/harness-agents.git
cd harness-agents
pwsh -NoProfile -File scripts/Install-Ha.ps1        # release build, installed to %USERPROFILE%\.cargo\bin
ha --version
$env:DEEPSEEK_API_KEY = "sk-..."                     # or type /key inside the app
ha                                                   # open the app in the current project
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
| See every command | `/help` (grouped card), `/help all` (full table), or `/` to open the menu |
| Continue an earlier conversation | `/resume`, then pick one |
| Start over | `/new` |
| Attach a file or image | `@` (file picker), `/attach <path>`, `/image` |
| Run a shell command | `!command` (goes through approval) |
| Correct a running turn | `/steer <text>` |
| Shrink a long conversation | `/compact [what to keep]` |
| See cost, context, permissions | `/cost`, `/context`, `/permissions` |

Keys: Enter sends, Ctrl-J or Alt+Enter adds a line, ↑↓ recall history or move in a menu, Esc closes a panel, Ctrl-C cancels a run, Ctrl-D on an empty line exits.

### Configuration

Settings merge default → user `config.toml` → trusted project `.harness/config.toml` → environment → command line; `/config` shows where each value came from. The most used environment variables:

| Variable | Default | Meaning |
| --- | --- | --- |
| `DEEPSEEK_API_KEY` | — | Provider key (also settable with `/key`, stored in the credentials file) |
| `HA_PROVIDER_ENDPOINT` | `https://api.deepseek.com/chat/completions` | Any OpenAI-compatible chat endpoint |
| `HA_PROVIDER_MODEL` | `deepseek-flash` | Model name |
| `HA_MEMORY` | on | `off` (or `0`, `false`, `no`) disables memory |
| `HA_TURN_MAX_STEPS` / `HA_TURN_MAX_TOOL_CALLS` | 30 / 80 | Model calls and tool calls per turn before it pauses |
| `HA_TURN_DEADLINE_SECONDS` / `HA_TURN_CONTINUATIONS` | 900 / 2 | Time per turn; automatic continuations after a bound |
| `HA_SKILL_PATHS` | — | Extra skill directories, separated like `PATH` |
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
- [Memory and continuity](docs/MEMORY_AND_CONTINUITY.en.md) — how sessions resume and what memory keeps (sections 19–21 describe the current behaviour)
- [Architecture review](docs/ARCHITECTURE_REVIEW.en.md) · [Plugin architecture](docs/PLUGIN_ARCHITECTURE.en.md) · [Rust harness plan](docs/RUST_HARNESS_PLAN.en.md)

## Tiếng Việt

### `ha` làm được gì

- **Chat trong terminal.** `ha` mở ứng dụng tương tác (TUI, hoặc chế độ dòng lệnh thuần). `ha exec "…"` chạy một prompt cho script và CI, xuất `text`, `json` hoặc `stream-json`.
- **Làm việc với code qua công cụ có kiểm soát.** Đọc, tìm kiếm, glob, sửa/ghi file, chạy tiến trình và lệnh shell, xem Git, hỏi lại bạn, giao việc cho agent con. Mọi thao tác đi qua chính sách của host: `y` chạy một lần, `a` cho phép đến hết lượt, `n` từ chối; `/mode` chọn `ask`, `auto-edit` hoặc `full-auto`.
- **Tiếp tục hội thoại.** `/resume` liệt kê các hội thoại (mỗi hội thoại một dòng) và phát lại các câu hỏi, câu trả lời của hội thoại được chọn cho model và trên màn hình.
- **Học từ hội thoại.** Memory mặc định bật: sau mỗi lượt, model trích ra ở chế độ nền các fact bền (sở thích, quyết định, quy ước, chỉnh sửa); fact có confidence ≥ 0.7 được dùng từ đó, phần còn lại chờ duyệt trong `ha memory candidates`. Nói "ghi nhớ …" / "remember that …" để lưu nguyên văn.
- **Dùng skill.** Thư mục Agent Skills (`SKILL.md` cùng references và scripts) từ bộ tích hợp sẵn, `~/.agents/skills`, `.agents/skills` của project đã trust, hoặc bất kỳ thư mục nào trong `HA_SKILL_PATHS` (ví dụ `~/.claude/skills`). Model kích hoạt skill phù hợp theo tên và đọc file của skill bằng `read_skill_file`; `/skills` liệt kê, `/skill:<name>` chạy một skill.
- **Mở rộng.** MCP server (`ha mcp add`), prompt template, hook, và giao diện web cục bộ (`ha web`).

### Nền tảng

| Nền tảng | Trạng thái |
| --- | --- |
| Windows 10/11 x64 | Hỗ trợ; mọi gate CI chạy trên Windows |
| Linux | **Đang chờ hỗ trợ.** Vẫn build được và job CI vẫn chạy để theo dõi, nhưng chưa phải nền tảng được hỗ trợ và không chặn CI |
| macOS | Chưa kiểm thử |

### Bắt đầu nhanh (Windows, PowerShell 7)

Cần có: [Rust](https://rustup.rs) (toolchain trong [`rust-toolchain.toml`](rust-toolchain.toml) được cài tự động), Git và PowerShell 7 (`pwsh`).

```powershell
git clone https://github.com/DEVfancybear/harness-agents.git
cd harness-agents
pwsh -NoProfile -File scripts/Install-Ha.ps1        # build release, cài vào %USERPROFILE%\.cargo\bin
ha --version
$env:DEEPSEEK_API_KEY = "sk-..."                     # hoặc gõ /key trong app
ha                                                   # mở app trong project hiện tại
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
| Xem mọi lệnh | `/help` (bảng gọn theo nhóm), `/help all` (bảng đầy đủ), hoặc `/` để mở menu |
| Tiếp tục hội thoại cũ | `/resume` rồi chọn |
| Bắt đầu lại | `/new` |
| Đính kèm file hoặc ảnh | `@` (chọn file), `/attach <path>`, `/image` |
| Chạy lệnh shell | `!command` (đi qua bước phê duyệt) |
| Chỉnh hướng một lượt đang chạy | `/steer <text>` |
| Rút gọn hội thoại dài | `/compact [điều cần giữ]` |
| Xem chi phí, context, quyền | `/cost`, `/context`, `/permissions` |

Phím: Enter gửi, Ctrl-J hoặc Alt+Enter xuống dòng, ↑↓ gọi lại lịch sử hoặc di chuyển trong menu, Esc đóng panel, Ctrl-C hủy lượt đang chạy, Ctrl-D trên dòng trống để thoát.

### Cấu hình

Cấu hình được gộp theo thứ tự mặc định → `config.toml` của người dùng → `.harness/config.toml` của project đã trust → biến môi trường → dòng lệnh; `/config` cho biết mỗi giá trị đến từ đâu. Các biến môi trường hay dùng:

| Biến | Mặc định | Ý nghĩa |
| --- | --- | --- |
| `DEEPSEEK_API_KEY` | — | Khóa provider (cũng đặt được bằng `/key`, lưu trong file credentials) |
| `HA_PROVIDER_ENDPOINT` | `https://api.deepseek.com/chat/completions` | Bất kỳ endpoint chat tương thích OpenAI |
| `HA_PROVIDER_MODEL` | `deepseek-flash` | Tên model |
| `HA_MEMORY` | bật | `off` (hoặc `0`, `false`, `no`) để tắt memory |
| `HA_TURN_MAX_STEPS` / `HA_TURN_MAX_TOOL_CALLS` | 30 / 80 | Số lời gọi model và tool mỗi lượt trước khi tạm dừng |
| `HA_TURN_DEADLINE_SECONDS` / `HA_TURN_CONTINUATIONS` | 900 / 2 | Thời gian mỗi lượt; số lần tự tiếp tục sau khi chạm giới hạn |
| `HA_SKILL_PATHS` | — | Thư mục skill bổ sung, phân tách như `PATH` |
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
- [Memory và tính liên tục](docs/MEMORY_AND_CONTINUITY.vi.md) — cách tiếp tục phiên và những gì memory giữ lại (mục 19–21 mô tả hành vi hiện tại)
- [Đánh giá kiến trúc](docs/ARCHITECTURE_REVIEW.vi.md) · [Kiến trúc plugin](docs/PLUGIN_ARCHITECTURE.vi.md) · [Kế hoạch Rust harness](docs/RUST_HARNESS_PLAN.vi.md)

---

Design references / Nguồn tham khảo thiết kế: [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness/tree/2377c272a8e839e0a84c9f0e623b867a1dce2014) · [TencentDB Agent Memory](https://github.com/TencentCloud/TencentDB-Agent-Memory/tree/906b5823b5106eed8f842b62f16d23228838149a) · [pi](https://github.com/earendil-works/pi/tree/d5629e20489ccf770ed90b5a33941cb3b7ef24d0) · [deer-flow](https://github.com/bytedance/deer-flow/tree/29dbce45a12cffe4ce7c395dde6e4b913748596b).
