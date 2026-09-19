# Sổ tay triển khai cho DeepSeek — kế hoạch mới M0–M12

**19/09/2026 · Kế hoạch nâng cấp source hiện tại; một workspace, một CLI `ha`.**

[Master plan](../HARNESS_MASTER_PLAN.vi.md) · [Roadmap](../HARNESS_ROADMAP.vi.md) · [Contracts](CONTRACTS.vi.md) · [Acceptance chi tiết](ACCEPTANCE.vi.md) · [Prompt giao việc](PROMPTS.vi.md) · [Mẫu SPEC/evidence/handoff](TEMPLATES.vi.md) · [Manifest máy đọc](manifest.json)

## 1. DeepSeek phải đọc gì và làm đến đâu

**Áp dụng cho mọi assignment M và H:** sửa và tích hợp vào source hiện tại, dùng root Cargo workspace và binary `ha`. Tên thư mục `implementation-next` chỉ đặt tên cho bộ tài liệu cho bước phát triển tiếp theo. [HA_LAUNCH H01–H08](../HA_LAUNCH_PLAN.vi.md) và [prompt H](../HA_LAUNCH_PROMPT.vi.md) tuân theo cùng nguyên tắc, không có ngoại lệ tạo codebase hoặc CLI thứ hai.

Đây là bộ hướng dẫn nâng cấp project đang có. Runbooks/evidence P0–P8 và H là căn cứ kiểm tra implementation hiện tại; M0–M12 bổ sung yêu cầu và acceptance, không reset thành quả đã làm. Mỗi lượt chỉ nhận **một milestone hoặc một work item**; mặc định bắt đầu M0-01, không đọc xong rồi triển khai toàn M0–M12. Bộ này không yêu cầu model nhớ toàn bộ cuộc trò chuyện: quyết định phải nằm trong SPEC/ADR, trạng thái đang làm phải nằm trong handoff.

Thứ tự đọc tối thiểu:

1. Quy định repo đang áp dụng, Git status và assignment mới nhất của người dùng.
2. File này và [contracts](CONTRACTS.vi.md), sau đó runbook đúng milestone.
3. Evidence/handoff của prerequisites và acceptance cases được runbook chỉ ra.
4. Các mục master plan được runbook dẫn; không tải mọi source/docs vào một prompt.

Thứ tự thẩm quyền: yêu cầu mới nhất của user → master plan → contracts/roadmap → runbook → SPEC implementation. Nếu user đã cho phép một thay đổi, không hỏi lại vì câu hướng dẫn chung. Routine implementation choices có default ở đây; tự quyết và ghi SPEC. Chỉ cần làm rõ khi yêu cầu mâu thuẫn thực sự, credentials không có, hoặc action ngoài phạm vi được giao.

## 2. Một workspace hiện tại, một CLI `ha`

Mọi thay đổi nằm trong root `Cargo.toml`, dùng chung `Cargo.lock`, toolchain và các crate `harness-*` hiện có. Entry point sản phẩm vẫn là `crates/harness-cli/src/main.rs`, binary `ha`. Thêm module/refactor code đang có khi cần; không tạo workspace lồng, binary sản phẩm khác hoặc một runtime chạy song song.

Đọc [bản đồ tích hợp](INTEGRATION_MAP.vi.md) trước khi sửa. Bảng dưới ánh xạ **logical shorthand** của runbooks; tên file là mục tiêu cần đối chiếu, không là lệnh tạo bản sao nếu symbol đã nằm ở file khác.

| Shorthand | Owner hiện tại / đường dẫn cần kiểm tra |
|---|---|
| `contracts/*` | `crates/harness-types/src/` |
| `core/*`, `context/*` | `crates/harness-session/src/`; trạng thái run/acceptance đối chiếu thêm types/runtime |
| `store/*` | `crates/harness-store-sqlite/src/`, migrations trong crate này |
| `providers/*`, `runtime/*` | `crates/harness-providers/src/`, `crates/harness-runtime/src/` |
| `tools/*`, `execution/*` | `crates/harness-tools/src/`; TurnDriver hiện ở crate này phải được tái sử dụng |
| `extensions/*` | `crates/harness-extensions/src/`; kernel lifecycle thuộc `crates/harness-kernel/src/` |
| `memory/*`, `orchestrator/*` | `crates/harness-memory/src/`, `crates/harness-orchestrator/src/` |
| Backup/restore/retention | `crates/harness-maintenance/src/`, phối hợp store hiện tại |
| `app/*`, `cli/*` | Composition/controllers trong `crates/harness-cli/src/`, gọi services hiện có; không nhét business loop vào renderer |
| `testkit/*` | Helpers/fixtures của test suites hiện tại; chỉ tách shared helper khi có consumer thật |
| `api/*`, `daemon/*`, `web/*` | Chỉ thêm ở M10/M11 khi được giao, trong cùng repo/workspace, dùng chung runtime/services |
| `schemas`, `tests`, `scripts`, `docs` | Thư mục tại root repository; giữ các registry/gate/evidence hiện có |

M0 là kiểm kê và chuẩn hóa contracts trên code đang có. Mỗi requirement phải ghi `reuse_verified / adapt / missing / incompatible`, source symbol, tests đã có, khoảng trống và hành động. Không tự scaffold lại types/store/runtime. Crate nội bộ mới chỉ được tạo trong root workspace khi SPEC chứng minh boundary cần tách và không tạo engine mới.

Integration tests ưu tiên mở rộng target hiện tại. Khi cần target M riêng, dùng `crates/harness-cli/tests/milestone_mn.rs` (package `harness-cli`). Tái sử dụng acceptance registry `tests/acceptance/registry.json`; nếu schema hiện tại không chứa mapping M, thêm `tests/acceptance/milestones.json` tham chiếu cùng test selectors, không tạo verdict authority cạnh tranh.

Data directory/session/store hiện tại phải tiếp tục được hỗ trợ. Schema thay đổi qua migrations versioned, fixture dữ liệu phiên bản trước và recovery khi migration lỗi. Không âm thầm reset database hoặc đổi data home để tránh xử lý compatibility. Fixture tests dùng temp directory riêng; điều này không tạo runtime sản phẩm riêng.

## 3. Chu trình một assignment

1. **Inventory:** xác minh branch/revision/status; liệt kê files được sửa và thay đổi của người dùng cần giữ; đối chiếu source/test với requirement qua bản đồ tích hợp; xác minh predecessor gate, không tin mỗi dòng “completed” trong handoff.
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

Phải nối vào code hiện tại để đáp ứng prerequisites. Reuse behavior đã có bằng test phù hợp; bổ sung assertion nếu contract mới rộng hơn. Không coi nhãn phase đã pass là đủ, cũng không bắt viết lại một tính năng chỉ vì nó chưa mang ID M.

## 5. Gate chạy được và cách tính accepted

**Chạy được ngay cho bộ tài liệu này:**

```powershell
pwsh -NoProfile -File scripts/Verify-NextPlan.ps1 -SelfTest
pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest
```

**Interface gate runtime tương lai — M0 mở rộng gate hiện có hoặc tạo wrapper dùng chung runner trước khi dùng:**

```powershell
pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M0
# Ví dụ sau M4; gate tự lấy dependency closure từ runtime registry:
pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M4
```

Gate runtime kế thừa `scripts/Verify-Phase.ps1` và các regressions hiện có, dùng manifest-path `Cargo.toml`, locked dependencies, fmt/clippy/build/test, schema fixtures và test discovery. Từng test suite thực thi trong Rust integration target có tên ổn định, ví dụ `milestone_m4`; registry ghi exact test names. `cargo test <filter>` exit 0 nhưng chạy 0 tests là **gate failure**. Ignored/skipped case bắt buộc không được tính pass; trường hợp OS-specific ghi platform requirement và phải có evidence từ OS tương ứng trước milestone acceptance đa nền tảng.

M0/M1/M2 bổ sung contract/integration tests nội bộ dù chưa có A-case bao hết. A01–A36 là acceptance liên tầng, không thay toàn bộ unit/integration test. Mỗi runbook có regressions và adversarial checks riêng.

Trạng thái implementation: planned → in_progress → implemented_unverified → verified_local → accepted; blocked là trạng thái cần ghi reason/next action, không có nghĩa completed. `accepted` cần đủ required platform gates + evidence + tất cả cases đến hạn. Reviewer/user có thể nghiệm thu; implementer không chỉ sửa một cờ status để tự nghiệm thu. Manifest trong bộ tài liệu luôn `planning_only`; implementation evidence/handoff nằm trong `docs/evidence/` và `docs/handoffs/`; runtime state vẫn dùng store hiện tại.

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
