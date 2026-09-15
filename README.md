# Harness Agents

Personal coding-agent harness planned in Rust: CLI first, multiple delegated agents, Web UI later.

Harness coding agent cá nhân dự kiến viết bằng Rust: CLI trước, giao việc cho nhiều agent, bổ sung Web UI sau.

**Status / Trạng thái:** P0–P3 are accepted; P4 adds scoped reusable memory, SQLite FTS5 and explicit bounded extraction catch-up. Final P4 verification and delivery state are recorded in the [English evidence](docs/evidence/P4.en.md) / [evidence tiếng Việt](docs/evidence/P4.vi.md). P5 multi-agent orchestration and later phases have not started. / P0–P3 đã được chấp nhận; P4 bổ sung memory có scope, SQLite FTS5 và extraction catch-up hữu hạn. Trạng thái kiểm chứng và bàn giao P4 nằm trong evidence; chưa bắt đầu P5 hoặc phase sau.

## Documentation / Tài liệu

| Read in order / Thứ tự đọc | Tiếng Việt | English |
|---|---|---|
| 1. Architecture review and decisions / Rà soát và quyết định | [Rà soát](docs/ARCHITECTURE_REVIEW.vi.md) | [Review](docs/ARCHITECTURE_REVIEW.en.md) |
| 2. Architecture and delivery plan / Kiến trúc và lộ trình | [Kế hoạch](docs/RUST_HARNESS_PLAN.vi.md) | [Plan](docs/RUST_HARNESS_PLAN.en.md) |
| 3. Plugin contracts / Hợp đồng plugin | [Plugin](docs/PLUGIN_ARCHITECTURE.vi.md) | [Plugins](docs/PLUGIN_ARCHITECTURE.en.md) |
| 4. Memory and work continuity / Memory và tiếp tục công việc | [Memory](docs/MEMORY_AND_CONTINUITY.vi.md) | [Memory](docs/MEMORY_AND_CONTINUITY.en.md) |
| 5. Phase-by-phase implementation / Triển khai từng phase | [Sổ tay giao việc](docs/implementation/README.vi.md) | [Implementation handbook](docs/implementation/README.en.md) |
| 6. Acceptance ownership / Phân công nghiệm thu | [Bảng nghiệm thu](docs/implementation/ACCEPTANCE_MAP.vi.md) | [Acceptance map](docs/implementation/ACCEPTANCE_MAP.en.md) |
| 7. P0 implementation record / Hồ sơ triển khai P0 | [Evidence](docs/evidence/P0.vi.md), [bàn giao](docs/handoffs/P0.vi.md) | [Evidence](docs/evidence/P0.en.md), [handoff](docs/handoffs/P0.en.md) |

The central design separates an execution journal, structured WorkingState and reusable memory. Resuming work must not depend on a final LLM summary or a live extraction worker.

Thiết kế tách journal thực thi, WorkingState có cấu trúc và memory tái sử dụng. Phục hồi công việc không được phụ thuộc bản tóm tắt LLM cuối phiên hoặc extractor còn chạy.

## Start implementation / Bắt đầu triển khai

Use the [P4 SPEC](docs/specs/P4.en.md) and [restart handoff](docs/handoffs/P4.en.md) for the current phase. The nine-phase plan preserves CLI-first delivery; Web is optional P8.

Đọc [SPEC P4](docs/specs/P4.vi.md) và [bàn giao khởi động lại](docs/handoffs/P4.vi.md) cho phase hiện tại. Kế hoạch chín phase giữ CLI trước; Web là P8 tùy chọn.

```powershell
cargo build -p harness-cli --bin ha --locked
ha memory --data-dir <data-dir> --session-id <session-id> catch-up --budget 4 --extractor mock
ha memory --data-dir <data-dir> --session-id <session-id> jobs --json
pwsh -NoProfile -File scripts/Verify-P4Gauntlet.ps1
```

The extractor defaults to disabled. The explicit mock is a deterministic local fixture; inferred memories stay candidates until manual confirmation. See `ha memory --help` for search/read/inspect, versioned publication, summaries and invalidation. Host CLI identity options are trusted local user input and must never be populated directly from model arguments.

Extractor mặc định tắt. Mock là fixture local xác định; memory suy luận giữ candidate đến khi được xác nhận thủ công. `ha memory --help` liệt kê search/read/inspect, publication có version, summary và invalidation. Các option identity CLI thuộc host/user local, không lấy trực tiếp từ model arguments.

## Documentation checks / Kiểm tra tài liệu

Requires PowerShell 7; no additional packages. / Cần PowerShell 7, không cần package bổ sung.

```powershell
pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest
```

Checks local links, paired sections, acceptance IDs/ownership, phase dependencies and steps, milestone estimates, fixed source revisions and fenced blocks. Includes in-memory negative controls. It does **not** execute the 30 continuity, 14 plugin or six optional Web cases; those are future runtime acceptance specifications. The GitHub workflow runs Rust phase gates and predecessor regressions on Ubuntu and Windows.

Kiểm tra links local, sections hai ngôn ngữ, acceptance IDs/ownership, dependencies và steps của phase, dự toán mốc, revision nguồn cố định, code fences; có negative controls trong RAM. **Không** chạy 30 ca continuity, 14 ca plugin hay sáu ca Web tùy chọn: đó là đặc tả nghiệm thu runtime tương lai. GitHub workflow chạy Rust phase gates và regressions tiền nhiệm trên Ubuntu và Windows.

## Research references / Nguồn khảo sát

Source inspection at fixed commits, not vendored runtime code / Đọc source tại commit cố định, không đưa runtime của dự án khác vào repo:

- [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness/tree/2377c272a8e839e0a84c9f0e623b867a1dce2014) and [architecture overview](https://deepseek.com/harness/en/).
- [TencentDB Agent Memory](https://github.com/TencentCloud/TencentDB-Agent-Memory/tree/906b5823b5106eed8f842b62f16d23228838149a).

Research date / Ngày khảo sát: 2026-09-10. Upstream snapshots inform the design; compatibility with their plugins or guarantees is not implied.
