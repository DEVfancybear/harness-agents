# Harness Agents

Personal coding-agent harness planned in Rust: CLI first, multiple delegated agents, Web UI later.

Harness coding agent cá nhân dự kiến viết bằng Rust: CLI trước, giao việc cho nhiều agent, bổ sung Web UI sau.

**Status / Trạng thái:** P0 foundation is implemented locally and stops at contracts, schema generation, fixture verification, and the minimal `ha` CLI. It has no session store, provider/model loop, tool runtime, memory runtime, multi-agent orchestration, Web API, or UI; P1 and later remain unimplemented. See the bilingual [P0 evidence](docs/evidence/P0.en.md) and [restart handoff](docs/handoffs/P0.en.md). / Nền tảng P0 đã được triển khai cục bộ và chỉ dừng ở contracts, tạo schema, kiểm chứng fixture và CLI `ha` tối thiểu. Chưa có session store, vòng lặp provider/model, tool runtime, memory runtime, điều phối đa agent, Web API hay UI; P1 trở đi chưa được triển khai. Xem [evidence P0](docs/evidence/P0.vi.md) và [bàn giao khởi động lại](docs/handoffs/P0.vi.md) song ngữ.

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

P0 was implemented against [P0 in English](docs/implementation/P0_FOUNDATION.en.md) and [P0 tiếng Việt](docs/implementation/P0_FOUNDATION.vi.md), with a phase SPEC and evidence/handoff above. It remains uncommitted and unpublished because neither action was authorized. Accept its evidence before assigning P1; the pack has nine phases, 63 ordered steps, scoped file ownership, tests, exit gates and bilingual evidence/handoff requirements. Web is optional P8.

P0 đã được triển khai theo [P0 tiếng Việt](docs/implementation/P0_FOUNDATION.vi.md) và [P0 in English](docs/implementation/P0_FOUNDATION.en.md), có SPEC và evidence/handoff ở trên. Thay đổi vẫn chưa commit hay publish vì hai việc đó chưa được giao. Hãy nghiệm thu evidence trước khi giao P1; bộ tài liệu có chín phase, 63 bước, phạm vi files, tests, điều kiện nghiệm thu và evidence/handoff song ngữ. Web là P8 tùy chọn.

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
