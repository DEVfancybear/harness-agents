# Harness Agents

**A coding agent that can pick up where it left off.** / **Coding agent có thể tiếp tục công việc đang dang dở.**

Harness Agents is a coding-agent project written in Rust and designed to run locally. Its single `ha` CLI brings conversations, coding tools, durable sessions, memory, and delegated tasks into one workflow. The goal is simple: make development with an agent easier to continue, inspect, and control.

Harness Agents là dự án coding agent chạy ưu tiên trên máy cá nhân, được viết bằng Rust. Một CLI `ha` kết hợp hội thoại, công cụ lập trình, phiên làm việc bền vững, bộ nhớ và giao việc cho agent khác trong cùng một quy trình. Mục tiêu của dự án là giúp việc phát triển cùng agent dễ tiếp tục, dễ kiểm tra và dễ kiểm soát hơn.

**Language / Ngôn ngữ:** [English](#english) · [Tiếng Việt](#tiếng-việt)

## English

### The idea

Coding work rarely fits into one prompt. A useful agent needs to remember the task, show what it actually did, and recover when a session stops midway. Harness Agents treats those needs as part of the product: work is recorded as it happens, decisions stay visible, and a new session can continue from durable state.

### What you can do

- **Work in the terminal.** Start `ha` for an interactive coding session, or run a bounded turn in headless mode for scripts and automation.
- **Keep your place.** Resume sessions with persisted history, checkpoints, and structured working context.
- **Use tools deliberately.** Coding actions go through host-controlled permissions, approvals, and execution records.
- **Reuse useful context.** Search scoped memory and keep reusable knowledge separate from the record of what happened.
- **Delegate work.** Split tasks across agents and bring their results back through a checked integration path.
- **Grow the workflow.** Add local skills and MCP integrations; use the local Web surface and operational commands where they fit.

The project favors durable state, explicit limits, and evidence from actual execution. Its Rust CLI is the primary experience; the Web surface and broader agent experience are still evolving.

### Try it locally

Install the Rust toolchain specified by [`rust-toolchain.toml`](rust-toolchain.toml), then run these commands in PowerShell on Windows:

```powershell
cargo build -p harness-cli --bin ha --locked
.\target\debug\ha.exe --version
.\target\debug\ha.exe chat --headless --mock --prompt "Hello" --json
.\target\debug\ha.exe chat --fixture
```

The last two commands use deterministic local fixtures, so you can explore the interface without configuring a model provider. For installation, provider setup, and everyday commands, see the [operator guide](docs/OPERATOR_GUIDE.en.md).

The `ha` executable includes the 18 skills in [`.agents/skills`](.agents/skills), including their supporting scripts and references. In a normal chat, `/skills` lists them as `builtin`; `/skill:<name>` activates one. The agent can also call `list_skills` and `activate_skill` when a task matches. Skill bodies load on activation, while skills supplied by another project still follow that project's trust setting. The executable prepares its bundled files in the user's configuration directory on first use.

### Where the project stands

Harness Agents is under active development. The core CLI and its supporting workflows are available in the repository. The Web experience and Linux verification are still in progress. Full strict isolation is unavailable on the measured Windows backend, so `ha` refuses requests that require it. The [current handoff](docs/handoffs/CURRENT.vi.md) records the latest status and known gaps.

The next product track focuses on a more complete coding-agent experience, including richer tools, configuration, interaction, and automation. See the [product plan](docs/HA_AGENT_PLAN.vi.md) for that direction.

## Tiếng Việt

### Ý tưởng

Công việc lập trình hiếm khi gói gọn trong một prompt. Một agent hữu ích cần nhớ tác vụ, cho thấy nó đã thực sự làm gì và phục hồi khi phiên làm việc dừng giữa chừng. Harness Agents coi đó là chức năng cốt lõi: công việc được ghi lại trong lúc thực hiện, các quyết định có thể kiểm tra và phiên mới có thể tiếp tục từ trạng thái đã lưu.

### Bạn có thể làm gì

- **Làm việc trong terminal.** Mở `ha` để dùng giao diện tương tác, hoặc chạy một lượt có giới hạn ở chế độ headless cho script và tự động hóa.
- **Tiếp tục đúng chỗ.** Khôi phục phiên từ lịch sử, checkpoint và ngữ cảnh làm việc có cấu trúc.
- **Dùng công cụ có chủ đích.** Các thao tác lập trình đi qua quyền hạn, bước phê duyệt và bản ghi thực thi do host kiểm soát.
- **Tái sử dụng ngữ cảnh hữu ích.** Tìm kiếm memory theo phạm vi, tách tri thức dùng lại khỏi bản ghi những gì đã xảy ra.
- **Giao việc cho agent khác.** Chia tác vụ và đưa kết quả trở lại qua bước kiểm tra tích hợp.
- **Mở rộng quy trình.** Thêm skill cục bộ và tích hợp MCP; sử dụng giao diện Web cục bộ cùng các lệnh vận hành khi cần.

Dự án ưu tiên trạng thái bền vững, giới hạn tường minh và bằng chứng từ việc thực thi thật. CLI viết bằng Rust là trải nghiệm chính; giao diện Web và trải nghiệm agent đầy đủ hơn đang tiếp tục phát triển.

### Chạy thử trên máy

Cài Rust toolchain được chỉ định trong [`rust-toolchain.toml`](rust-toolchain.toml), rồi chạy các lệnh sau bằng PowerShell trên Windows:

```powershell
cargo build -p harness-cli --bin ha --locked
.\target\debug\ha.exe --version
.\target\debug\ha.exe chat --headless --mock --prompt "Hello" --json
.\target\debug\ha.exe chat --fixture
```

Hai lệnh cuối dùng fixture cục bộ có kết quả xác định, nên bạn có thể thử giao diện mà chưa cần cấu hình model provider. Xem [hướng dẫn vận hành](docs/OPERATOR_GUIDE.vi.md) để cài đặt, cấu hình provider và sử dụng hằng ngày.

File `ha.exe` tích hợp 18 skill trong [`.agents/skills`](.agents/skills), gồm cả script và tài liệu đi kèm. Trong chat thông thường, `/skills` liệt kê chúng với nguồn `builtin`; `/skill:<name>` kích hoạt một skill. Agent cũng có thể gọi `list_skills` và `activate_skill` khi tác vụ phù hợp. Nội dung skill chỉ được nạp lúc kích hoạt; skill do một project khác cung cấp vẫn theo thiết lập trust của project đó. Lần dùng đầu, ứng dụng chuẩn bị các tệp tích hợp trong thư mục cấu hình người dùng.

### Dự án đang ở đâu

Harness Agents đang được phát triển tích cực. CLI cốt lõi và các quy trình hỗ trợ đã có trong repo. Trải nghiệm Web và việc xác minh trên Linux vẫn đang hoàn thiện. Backend Windows đã đo chưa hỗ trợ cách ly strict đầy đủ, nên `ha` sẽ từ chối yêu cầu cần chế độ này. [Handoff hiện tại](docs/handoffs/CURRENT.vi.md) ghi trạng thái và các điểm còn mở mới nhất.

Track sản phẩm tiếp theo hướng tới trải nghiệm coding agent hoàn chỉnh hơn, gồm công cụ, cấu hình, tương tác và tự động hóa phong phú hơn. Xem [kế hoạch sản phẩm](docs/HA_AGENT_PLAN.vi.md) để biết định hướng này.

---

**Built for work that continues. / Xây dựng cho công việc cần được tiếp nối.**

Design references / Nguồn tham khảo thiết kế: [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness/tree/2377c272a8e839e0a84c9f0e623b867a1dce2014) · [TencentDB Agent Memory](https://github.com/TencentCloud/TencentDB-Agent-Memory/tree/906b5823b5106eed8f842b62f16d23228838149a).
