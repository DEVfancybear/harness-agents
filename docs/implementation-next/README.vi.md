# Sổ tay triển khai cho DeepSeek — kế hoạch mới M0–M12

**19/09/2026 · Chỉ đặc tả triển khai, chưa có runtime mới.**

[Master plan](../HARNESS_MASTER_PLAN.vi.md) · [Roadmap](../HARNESS_ROADMAP.vi.md) · [Contracts](CONTRACTS.vi.md) · [Acceptance chi tiết](ACCEPTANCE.vi.md) · [Prompt giao việc](PROMPTS.vi.md) · [Mẫu SPEC/evidence/handoff](TEMPLATES.vi.md) · [Manifest máy đọc](manifest.json)

## 1. DeepSeek phải đọc gì và làm đến đâu

**Ngoại lệ cho assignment khởi động `ha`:** khi user giao [HA_LAUNCH H01–H08](../HA_LAUNCH_PLAN.vi.md), dùng [prompt H](../HA_LAUNCH_PROMPT.vi.md) và sửa CLI hiện tại; không áp default `vnext/`/`ha-next` của bộ M. Chỉ feature startup này được override; các assignments M vẫn theo sổ tay bên dưới.

Đây là bộ hướng dẫn coding, tách khỏi runbooks P0–P8 cũ. Mỗi lượt chỉ nhận **một milestone hoặc một work item**; mặc định bắt đầu M0-01, không đọc xong rồi triển khai toàn M0–M12. Bộ này không yêu cầu model nhớ toàn bộ cuộc trò chuyện: quyết định phải nằm trong SPEC/ADR, trạng thái đang làm phải nằm trong handoff.

Thứ tự đọc tối thiểu:

1. Quy định repo đang áp dụng, Git status và assignment mới nhất của người dùng.
2. File này và [contracts](CONTRACTS.vi.md), sau đó runbook đúng milestone.
3. Evidence/handoff của prerequisites và acceptance cases được runbook chỉ ra.
4. Các mục master plan được runbook dẫn; không tải mọi source/docs vào một prompt.

Thứ tự thẩm quyền: yêu cầu mới nhất của user → master plan → contracts/roadmap → runbook → SPEC implementation. Nếu user đã cho phép một thay đổi, không hỏi lại vì câu hướng dẫn chung. Routine implementation choices có default ở đây; tự quyết và ghi SPEC. Chỉ cần làm rõ khi yêu cầu mâu thuẫn thực sự, credentials không có, hoặc action ngoài phạm vi được giao.

## 2. Quyết định đường dẫn để bắt đầu mà không mắc vào code cũ

Default triển khai mới trong **`vnext/`**, một Cargo workspace độc lập có `[workspace]` riêng, lockfile/toolchain riêng; data directory fixture/runtime riêng. Binary trong thời gian phát triển tên **`ha-next`**. Không sửa members của workspace cũ, không xóa code cũ, không migrate data cũ ngầm. Đây là vùng code mới, không phải worktree mới hoặc repo mới.

M0 chỉ tạo contracts/core/app/CLI/testkit tối thiểu và placeholder ports cần dùng. Các crates còn lại chỉ tạo khi milestone sở hữu cần; không tạo 14 crates rỗng. M0 ghi `layout-map` trong SPEC để ánh xạ logical modules sang actual paths. Rename/gộp module được phép nếu cùng boundary và cập nhật map, imports, gate commands, docs trong cùng change. Default paths trong runbooks tránh DeepSeek tự chọn cấu trúc khác ở mỗi lượt.

Trong câu trên, “placeholder ports” nghĩa declarations của typed interfaces, **không phải methods trả success giả**. M0-01 được phép bootstrap `vnext/Cargo.toml` và contracts/core manifests tối thiểu để chạy reducer tests ngay; M0-03 hoàn thiện workspace config/CLI/testkit, không đợi tới M0-03 mới có test chạy được.

```text
vnext/
  Cargo.toml, Cargo.lock, rust-toolchain.toml
  crates/{contracts,core,app,cli,testkit}/...
  crates/{store,providers,runtime,tools,execution,context}/...  # tạo theo mốc
  crates/{extensions,memory,orchestrator}/...                # tạo sau
  tests/fixtures/<milestone>/...
  tests/acceptance/registry.json
  schemas/...
  scripts/Verify-Milestone.ps1                               # M0 tạo
  docs/{specs,adr,evidence,handoffs}/...
```

Giai đoạn M9 mới quyết định cutover tên `ha`/workspace chính, installation path và import dữ liệu qua ADR/migration có backup. Không gắn việc cutover vào M0. User có thể giao vị trí khác; ghi override trong SPEC trước edits và giữ data isolation.

Quy ước shorthand trong dòng **Files/ownership** của runbook:

| Shorthand | Default actual path |
|---|---|
| `contracts/x.rs`, `core/x.rs`, `runtime/x.rs`, `store/x.rs`, ... | `vnext/crates/<module>/src/x.rs` |
| `store/migrations`, `store/memory_schema.rs` | SQL files ở `vnext/crates/store/migrations/`; schema runner ở `vnext/crates/store/src/memory_schema.rs` |
| `cli/x.rs`, `api/x.rs`, `daemon/x.rs`, `testkit/x.rs` | `vnext/crates/<module>/src/x.rs`, tạo crate đúng milestone |
| `schemas`, `tests/fixtures`, `scripts`, `docs` | Thư mục cùng tên dưới `vnext/` |
| `web/...` | `vnext/web/...`, chỉ tạo tại M10 |
| `crates/...`, `Cargo.toml`, `rust-toolchain.toml` | Relative trực tiếp với `vnext/` |

Đừng tạo đồng thời `vnext/store` và `vnext/crates/store` vì đọc shorthand như path literal. Integration acceptance targets mặc định ở `vnext/crates/cli/tests/milestone_mn.rs` (package `ha-next-cli`, hoặc application integration-test package được SPEC chốt khi dependencies tăng); reusable fixtures ở `ha-next-testkit`. Gate phải gọi exact package/target thực có trong runtime registry, không giả file top-level `vnext/tests` tự được Cargo discover.

## 3. Chu trình một assignment

1. **Inventory:** xác minh branch/revision/status; liệt kê files được sửa; xác minh predecessor gate, không tin mỗi dòng “completed” trong handoff.
2. **SPEC:** ghi goal/non-goals, contracts, failure modes, paths, cases và commands. Chỉ cần ADR đã đến hạn trong runbook.
3. **Executable example:** tạo fixture nhỏ có input/expected output hoặc invariant có thể chứng minh fail khi implementation sai. Không tạo fixture expected bằng chính function đang test.
4. **Implement một lát cắt:** đi theo thứ tự work items; contracts/schema → storage/adapter → orchestration → CLI → tests. Không thêm TODO-success/no-op vào production path.
5. **Verify:** chạy targeted cases, rồi gate milestone + dependency closure. Đổi code sau gate thì chạy lại phần bị tác động và required final gate.
6. **Evidence:** ghi tested revision hoặc base revision + diff/tree hash, số tests thực sự chạy, skipped/failed, OS/dependencies, output paths/hashes. Không claim pass khi command chỉ được viết trong docs.
7. **Handoff:** ghi completed work items, remaining work, blockers, exact next action và invariant cần giữ. Dừng ở assignment boundary.

Không tự spawn agents, gọi paid APIs, commit/push/publish hay tạo automation trừ khi assignment/session đã cấp quyền tương ứng. Quyền đã cấp giữ hiệu lực; không xin lại để làm đúng phần đã giao. Pin toolchain/dependencies dựa trên compatibility đã kiểm tra ở milestone, không lấy version bịa từ trí nhớ.

## 4. Những chỗ roadmap dễ khiến code bị thiếu nối dây

| Thứ tự | Cách triển khai bắt buộc |
|---|---|
| M3 runtime trước M4 real tools | M3 dùng fixture ToolExecutor chạy qua cùng invocation/approval/receipt ports; M4 thay adapter, không viết engine khác |
| M3 runtime trước M5 full context | M3 cần compiler tối thiểu: policy + user input + state + paired tail, frozen manifest và overflow reject; M5 thêm retrieval/compaction, không vá một loop vốn gửi raw chat tùy tiện |
| M1 storage trước future domains | M1 tạo only core records; budget/questions ở M3, tools M4, memory M7, children M8 có migrations do owner tương ứng; không tạo mọi table sớm |
| M6 skills cần context | M5 định nghĩa Contributor port; M6 thêm SkillContributor, cùng freeze pipeline |
| M7 memory trước M8 actors | Extraction worker là durable job consumer, không cần subagent orchestration |
| M10 Web độc lập M11 daemon | M10 có host lifecycle của API process; chưa hứa task sống sau API exit. M11 mới thêm client attach/detach và long-lived daemon |
| M12 chỉ phụ thuộc M4 | Implement strict backend bằng ExecutionBackend port; không import M8/M10/M11; ghép lại regression tại mốc release sử dụng |

Không cần ghép code cũ để thỏa prerequisites mới. Reuse có thể thực hiện khi contract/test mới chứng minh behavior, và chỉ trong scope được giao.

## 5. Gate chạy được và cách tính accepted

**Chạy được ngay cho bộ tài liệu này:**

```powershell
pwsh -NoProfile -File scripts/Verify-NextPlan.ps1 -SelfTest
pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest
```

**Interface gate runtime tương lai — M0 phải tạo trước khi dùng:**

```powershell
pwsh -NoProfile -File vnext/scripts/Verify-Milestone.ps1 -Milestone M0
# Ví dụ sau M4; gate tự lấy dependency closure từ runtime registry:
pwsh -NoProfile -File vnext/scripts/Verify-Milestone.ps1 -Milestone M4
```

Gate runtime dùng manifest-path `vnext/Cargo.toml`, locked dependencies, fmt/clippy/build/test, schema fixtures và test discovery. Từng test suite thực thi trong Rust integration target có tên ổn định, ví dụ `milestone_m4`; registry ghi exact test names. `cargo test <filter>` exit 0 nhưng chạy 0 tests là **gate failure**. Ignored/skipped case bắt buộc không được tính pass; trường hợp OS-specific ghi platform requirement và phải có evidence từ OS tương ứng trước milestone acceptance đa nền tảng.

M0/M1/M2 bổ sung contract/integration tests nội bộ dù chưa có A-case bao hết. A01–A36 là acceptance liên tầng, không thay toàn bộ unit/integration test. Mỗi runbook có regressions và adversarial checks riêng.

Trạng thái implementation: planned → in_progress → implemented_unverified → verified_local → accepted; blocked là trạng thái cần ghi reason/next action, không có nghĩa completed. `accepted` cần đủ required platform gates + evidence + tất cả cases đến hạn. Reviewer/user có thể nghiệm thu; implementer không chỉ sửa một cờ status để tự nghiệm thu. Manifest trong bộ tài liệu luôn `planning_only`; execution state nằm ở `vnext/`.

## 6. Danh mục runbooks và dependencies

| Milestone | Prerequisites | Runbook |
|---|---|---|
| M0 | — | [Foundation](M0.vi.md) |
| M1 | M0 | [Store và recovery](M1.vi.md) |
| M2 | M0 | [Provider protocol](M2.vi.md) |
| M3 | M1, M2 | [TurnDriver](M3.vi.md) |
| M4 | M3 | [Coding tools](M4.vi.md) |
| M5 | M4 | [Context và continuity](M5.vi.md) |
| M6 | M5 | [Skills/MCP](M6.vi.md) |
| M7 | M6 | [Reusable memory](M7.vi.md) |
| M8 | M7 | [Multi-agent](M8.vi.md) |
| M9 | M8 | [CLI release](M9.vi.md) |
| M10 | M9 | [Web/API](M10.vi.md) |
| M11 | M9 | [Daemon/scheduler](M11.vi.md) |
| M12 | M4 | [Strict execution](M12.vi.md) |

M10/M11/M12 chỉ triển khai khi được giao. Assignment nhiều mốc không miễn gate giữa mốc. Ước lượng ngày công giữ ở roadmap; bộ chi tiết không tạo lời hứa chất lượng hay deadline mới.
