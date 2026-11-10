# Documentation index

Start with the [project README](../README.md) for installation and a feature
summary. These documents describe the `ha` CLI and host as they are in the code
of this release; when a document and the code disagree, the code is right and the
document is a bug.

Bắt đầu từ [README của dự án](../README.md) để cài đặt và xem tính năng. Các tài
liệu mô tả CLI và host `ha` đúng như code của bản release này; khi tài liệu và
code khác nhau thì code đúng, còn tài liệu là lỗi cần sửa.

## Guides / Hướng dẫn

| Task / Nhu cầu | Read / Tài liệu |
| --- | --- |
| Every command, setting and workflow, and what each release changed / Mọi lệnh, cấu hình và quy trình, và mỗi bản release đã đổi gì | [Operator guide (EN)](guides/OPERATOR_GUIDE.en.md) · [Hướng dẫn vận hành (VI)](guides/OPERATOR_GUIDE.vi.md) |
| Agent sidebar, child panes, keys, status line and frame pacing / Sidebar agent, pane agent con, phím tắt, thanh trạng thái và nhịp khung hình | [Terminal interface](guides/TUI.md) |
| Build, test, package and release / Build, kiểm thử, đóng gói và phát hành | [Build and release](guides/BUILD_AND_RELEASE.md) |
| The live suites that drive the real `ha` against a real model / Các suite live chạy `ha` thật với model thật | [Live tests](../scripts/testing/live-tests/README.md) |
| Jev beside the agent and the harness (plugin, off by default) / Jev bên cạnh agent và harness (plugin, mặc định tắt) | [Jev workflow](guides/JEV.en.md) · [Workflow Jev](guides/JEV.vi.md) |
| Screenshots, mouse and keyboard on Windows / Chụp màn hình, chuột và bàn phím Windows | [Computer use](guides/COMPUTER_USE.md) (in development / đang phát triển) |

## Architecture / Kiến trúc

| Topic / Chủ đề | Read / Tài liệu |
| --- | --- |
| Crates, ownership, dependencies and folder layout / Crate, trách nhiệm, dependency và bố cục | [Architecture (EN)](architecture/ARCHITECTURE_OVERVIEW.en.md) · [Kiến trúc (VI)](architecture/ARCHITECTURE_OVERVIEW.vi.md) |
| The memory store: entries, search, validity, trust, citations, the review queue, memory on and off / Kho memory: entry, tìm kiếm, hiệu lực, độ tin, trích dẫn, hàng đợi duyệt, bật tắt memory | [Memory store (EN)](architecture/MEMORY_STORE.en.md) · [Kho memory (VI)](architecture/MEMORY_STORE.vi.md) |
| A project's memory as markdown files, `ha memory init` and `lint` / Memory của project dạng file markdown, `ha memory init` và `lint` | [Markdown memory (EN)](architecture/MARKDOWN_MEMORY.en.md) · [Memory markdown (VI)](architecture/MARKDOWN_MEMORY.vi.md) |
| The memory readout, MEMORILLA in ha: memory read by the question asked / Readout của memory, MEMORILLA trong ha: đọc bộ nhớ theo câu hỏi | [Memory readout (EN)](architecture/MEMORY_READOUT.en.md) · [Đọc bộ nhớ theo câu hỏi (VI)](architecture/MEMORY_READOUT.vi.md) |
| Where each kind of session information lives: task state, handoff, session log, journal, `/recall`, `/preserve` / Mỗi loại thông tin của phiên nằm ở đâu | [Session continuity (EN)](architecture/SESSION_CONTINUITY.en.md) · [Liền mạch giữa các phiên (VI)](architecture/SESSION_CONTINUITY.vi.md) |
| Lessons from corrections and wrong assumptions, `ha learning` / Bài học từ lời sửa và giả định sai, `ha learning` | [Learning loop (EN)](architecture/LEARNING_LOOP.en.md) · [Vòng học (VI)](architecture/LEARNING_LOOP.vi.md) |
| Background memory passes as a durable queue: reviews, Deep Dream, evolution, expiry, the wiki pass / Các lượt memory chạy nền thành hàng đợi bền | [Sleep (EN)](architecture/SLEEP.en.md) · [Sleep (VI)](architecture/SLEEP.vi.md) |
| Strategy revision, trajectory regulation and learned rules / Điều chỉnh chiến lược, điều tiết quỹ đạo và rule đã học | [Strategy and runtime (EN)](architecture/STRATEGY_AND_RUNTIME.en.md) · [Đổi chiến lược và runtime (VI)](architecture/STRATEGY_AND_RUNTIME.vi.md) |
| Scoring a finished turn on five quality axes, `/self-eval` / Chấm một lượt đã xong trên năm trục chất lượng | [Self-evaluation (EN)](architecture/SELF_EVALUATION.en.md) · [Tự đánh giá (VI)](architecture/SELF_EVALUATION.vi.md) |
| Calls the model already decided, run without a model step, `/bank` / Các lệnh model đã quyết định xong, chạy không cần một bước model, `/bank` | [Action bank (EN)](architecture/ACTION_BANK.en.md) · [Action bank (VI)](architecture/ACTION_BANK.vi.md) |
| What a command or file write may never do / Những gì một lệnh hay lần ghi file không được làm | [Guards (EN)](architecture/GUARDS.en.md) · [Guard (VI)](architecture/GUARDS.vi.md) |
| Which store a project is kept in, and a repository that moves, `ha project rekey` / Project được giữ ở kho nào, repo bị di chuyển | [Project identity (EN)](architecture/PROJECT_IDENTITY.en.md) · [Định danh project (VI)](architecture/PROJECT_IDENTITY.vi.md) |
| Skills, extensions, MCP and tiered tools / Skill, extension, MCP và tool phân tầng | [Plugin architecture (EN)](architecture/PLUGIN_ARCHITECTURE.en.md) · [Kiến trúc plugin (VI)](architecture/PLUGIN_ARCHITECTURE.vi.md) |
| Isolation limits and measured support / Giới hạn cô lập và hỗ trợ đã đo | [Strict execution support](support/STRICT_EXECUTION_SUPPORT.vi.md) |

## Scripts / Script

Scripts are grouped in `scripts/common/`, `install/`, `release/`, `testing/` and
`verify/`; the build guide and the live-test README give the current commands.
How a change is tested and released is in [AGENTS.md](../AGENTS.md).

Script được nhóm trong `scripts/common/`, `install/`, `release/`, `testing/` và
`verify/`; hướng dẫn build và README live-test có các lệnh hiện hành. Cách kiểm
thử và phát hành một thay đổi nằm trong [AGENTS.md](../AGENTS.md).
