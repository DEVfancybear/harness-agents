# Harness Agents

Personal coding-agent harness planned in Rust: CLI first, multiple delegated agents, Web UI later.

Harness coding agent cá nhân dự kiến viết bằng Rust: CLI trước, giao việc cho nhiều agent, bổ sung Web UI sau.

**Status / Trạng thái:** architecture and planning only. No Rust runtime, SDK or `ha` commands exist yet. / Chỉ có kiến trúc và kế hoạch; chưa có Rust runtime, SDK hay lệnh `ha` thực thi được.

## Documentation / Tài liệu

| Read in order / Thứ tự đọc | Tiếng Việt | English |
|---|---|---|
| 1. Architecture review and decisions / Rà soát và quyết định | [Rà soát](docs/ARCHITECTURE_REVIEW.vi.md) | [Review](docs/ARCHITECTURE_REVIEW.en.md) |
| 2. Architecture and delivery plan / Kiến trúc và lộ trình | [Kế hoạch](docs/RUST_HARNESS_PLAN.vi.md) | [Plan](docs/RUST_HARNESS_PLAN.en.md) |
| 3. Plugin contracts / Hợp đồng plugin | [Plugin](docs/PLUGIN_ARCHITECTURE.vi.md) | [Plugins](docs/PLUGIN_ARCHITECTURE.en.md) |
| 4. Memory and work continuity / Memory và tiếp tục công việc | [Memory](docs/MEMORY_AND_CONTINUITY.vi.md) | [Memory](docs/MEMORY_AND_CONTINUITY.en.md) |
| 5. Phase-by-phase implementation / Triển khai từng phase | [Sổ tay giao việc](docs/implementation/README.vi.md) | [Implementation handbook](docs/implementation/README.en.md) |
| 6. Acceptance ownership / Phân công nghiệm thu | [Bảng nghiệm thu](docs/implementation/ACCEPTANCE_MAP.vi.md) | [Acceptance map](docs/implementation/ACCEPTANCE_MAP.en.md) |

The central design separates an execution journal, structured WorkingState and reusable memory. Resuming work must not depend on a final LLM summary or a live extraction worker.

Thiết kế tách journal thực thi, WorkingState có cấu trúc và memory tái sử dụng. Phục hồi công việc không được phụ thuộc bản tóm tắt LLM cuối phiên hoặc extractor còn chạy.

## Start implementation / Bắt đầu triển khai

Assign [P0 in English](docs/implementation/P0_FOUNDATION.en.md) or [P0 tiếng Việt](docs/implementation/P0_FOUNDATION.vi.md) first. Copy its section 9 prompt to the coding agent. The pack has nine phases, 63 ordered steps, scoped file ownership, tests, exit gates and bilingual evidence/handoff requirements. Accept each predecessor before starting the next phase; Web is optional P8. This documentation does not mark any phase implemented.

Giao P0 trước, copy prompt ở mục 9 cho coding agent. Bộ tài liệu có chín phases, 63 bước, phạm vi files, tests, điều kiện nghiệm thu và yêu cầu evidence/handoff hai ngôn ngữ. Nghiệm thu phase trước rồi mới bắt đầu phase sau; Web là P8 tùy chọn. Chưa phase nào được coi là đã triển khai chỉ vì có tài liệu này.

## Documentation checks / Kiểm tra tài liệu

Requires PowerShell 7; no additional packages. / Cần PowerShell 7, không cần package bổ sung.

```powershell
pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest
```

Checks local links, paired sections, acceptance IDs/ownership, phase dependencies and steps, milestone estimates, fixed source revisions and fenced blocks. Includes in-memory negative controls. It does **not** execute the 30 continuity, 14 plugin or six optional Web cases; those are future runtime acceptance specifications. The GitHub workflow runs documentation checks only.

Kiểm tra links local, sections hai ngôn ngữ, acceptance IDs/ownership, dependencies và steps của phase, dự toán mốc, revision nguồn cố định, code fences; có negative controls trong RAM. **Không** chạy 30 ca continuity, 14 ca plugin hay sáu ca Web tùy chọn: đó là đặc tả nghiệm thu runtime tương lai. GitHub workflow hiện chỉ kiểm tra tài liệu.

## Research references / Nguồn khảo sát

Source inspection at fixed commits, not vendored runtime code / Đọc source tại commit cố định, không đưa runtime của dự án khác vào repo:

- [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness/tree/2377c272a8e839e0a84c9f0e623b867a1dce2014) and [architecture overview](https://deepseek.com/harness/en/).
- [TencentDB Agent Memory](https://github.com/TencentCloud/TencentDB-Agent-Memory/tree/906b5823b5106eed8f842b62f16d23228838149a).

Research date / Ngày khảo sát: 2026-09-10. Upstream snapshots inform the design; compatibility with their plugins or guarantees is not implied.
