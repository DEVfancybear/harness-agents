# Bản đồ tích hợp vào source hiện tại — một CLI `ha`

**19/09/2026 · Quyết định áp dụng cho toàn M0–M12 và H01–H08.**

[Sổ tay](README.vi.md) · [Contracts](CONTRACTS.vi.md) · [Prompts](PROMPTS.vi.md) · [Track khởi động](../HA_LAUNCH_PLAN.vi.md)

## 1. Quyết định đã sửa

Kế hoạch tạo workspace và CLI sản phẩm riêng trước đây bị rút lại. Mục tiêu là nâng cấp Harness đang có. Root `Cargo.toml` quản lý các crate hiện tại; sản phẩm người dùng cài/gọi vẫn là `ha` trong `harness-cli`. `implementation-next` là thư mục tài liệu, không là source tree hay executable mới.

Tách module/trait/service để quản lý trách nhiệm là hợp lệ trong cùng workspace. Không sao chép store, agent loop, config hay installer rồi để hai implementation cùng phát triển. API/Web/daemon tương lai gọi chung services và ownership protocol; không tạo runtime authority khác.

## 2. Điểm nối đã kiểm tra trong source

Bảng được đối chiếu file/module/manifests tại baseline `7e4087b`; nó không tuyên bố các acceptance M đã pass. Working tree đang thay đổi: trước code phải lấy revision/status mới nhất và đọc đúng symbols/callers. File được nêu là điểm vào để khảo sát; không bắt tạo một file mới có tên như shorthand trong runbook.

| Milestone / năng lực | Source owner hiện tại và cách nối | Test/gate hiện có cần đối chiếu |
|---|---|---|
| M0 IDs/contracts/config | `harness-types/src/{ids,contracts,error,schema}.rs`; `harness-session/src/lib.rs`; `harness-runtime/src/lib.rs`; config/composition trong `harness-cli/src/interactive/` | `phase_p0.rs`, schema tests và các launch/config tests |
| M1 durable store/recovery | `harness-store-sqlite/src/{store,models}.rs` và `src/store/`; `SessionService` tại `harness-session/src/lib.rs`. Mở rộng transactions/schema hiện có | `phase_p1.rs`, `phase_p2.rs`, receipt/recovery tests H |
| M2 provider/stream | `harness-providers/src/{lib,streaming}.rs`; consumer trong interactive service/TurnDriver. Reuse stream protocol, chỉ bổ sung conformance/capabilities còn thiếu | `phase_p3.rs`, `interactive_session.rs`, provider unit tests |
| M3 loop/input/budget/acceptance | `harness-runtime::RuntimeService`, `harness-tools/src/turn_driver.rs::TurnDriver`, `harness-cli/src/interactive/service.rs::AgentSessionService`. Reuse continuation và approval/cancel ports đã có | `phase_p3.rs`, `interactive_session.rs`, `interactive_terminal.rs` |
| M4 coding tools/execution | `harness-tools/src/{service,policy,process,workspace,contracts}.rs` và `turn_driver.rs`. Cùng intent/policy/receipt pipeline cho mọi caller | `phase_p4.rs`, `phase_p4/`, gates P4, loop/PTY tests H |
| M5 context/continuity | `harness-session/src/context.rs::ContextBuilder`, `RuntimeService` compaction/resume, store journal/history. Giữ một compiler và authoritative source | Context/runtime/store unit tests, P2/P3 và resume tests H |
| M6 extensions/skills/MCP | `harness-kernel/src/lib.rs`, `harness-extensions/src/{host,skills,mcp,transport,bridges,contracts}.rs`, `harness-cli/src/extension_cli.rs` | `phase_p6.rs`, `phase_p6/`; fixture binaries hiện có là test helpers |
| M7 memory | `harness-memory/src/{extraction,retrieval,maintenance}.rs`, store và `harness-cli/src/memory_cli.rs`. Reuse assets/jobs/indexes hiện có | `phase_p5.rs`, `phase_p5/`, memory/store unit tests |
| M8 orchestration | `harness-orchestrator/src/{coordinator,scheduler,workspace,integration,contracts}.rs`, `harness-cli/src/delegation_cli.rs`. Agent con dùng cùng execution services | `phase_p5.rs`, `phase_p5/`, coordinator/workspace tests |
| M9 operations/release | `harness-maintenance/src/{backup,retention,migration}.rs`, `maintenance_cli.rs`, `scripts/Install-Ha.ps1`, `scripts/New-HaRelease.ps1`. Reuse installer và data lifecycle P7/H | `phase_p7.rs`, `phase_p7/`, `Verify-HaLaunch.ps1`, fresh-shell install tests |
| H startup/session | `harness-cli/src/main.rs` → `interactive/{mod,bootstrap,app,controller,service,terminal}.rs`; headless cũng gọi `TurnDriver` | `interactive_launch.rs`, `interactive_session.rs`, `interactive_terminal.rs`, `Verify-HaLaunch.ps1` |
| M10/M11 API/daemon | Module/crate nội bộ trong root workspace khi được giao. Tách reusable application service từ composition hiện tại nếu cần, giữ một writable owner/data directory | Thêm transport/auth/reconnect/ownership assertions và chạy regressions runtime hiện có |
| M12 strict backend | ExecutionBackend port nối `harness-tools` process/workspace/policy; cùng tool admission/receipt path | Reuse P4 + thêm isolation capability tests trên backend được chọn |

Tên target P/H chỉ là điểm bắt đầu tìm test; từng requirement M cần ghi exact selector và assertions thực sự chứng minh. Không suy “P3 pass” thành “M2/M3 accepted”.

## 3. Luồng tích hợp bắt buộc

```text
ha (main / parser)
  -> interactive controller hoặc command adapter hiện tại
  -> application/session service
  -> TurnDriver / RuntimeService / SessionService hiện tại
  -> provider stream hoặc tool policy -> intent -> executor -> receipt
  -> SQLite journal/projections + session/context
  -> events/results trả về cùng CLI
```

Sơ đồ là ranh giới trách nhiệm, không yêu cầu viết thêm một service cho mỗi dòng. Chức năng mới phải có caller thật từ luồng này, kể cả lỗi/cancel/restart. Unit test helper riêng không đủ chứng minh đã tích hợp.

Hiện `harness-tools` phụ thuộc `harness-runtime`; runtime đã phụ thuộc provider/session/store/memory. Không thêm chiều runtime → tools hoặc move TurnDriver vào runtime bằng cách gây vòng dependency. Nếu cần tách ports: ghi graph trước/sau, chuyển interface thuần sang tầng phù hợp, cập nhật tất cả callers rồi test. Mục tiêu kiến trúc sạch không phải lý do viết engine khác hoặc đổi toàn dependency graph trong một item.

## 4. Quy trình DeepSeek cho mỗi item

1. Lấy Git revision/status, đọc SPEC/handoff đang có; giữ thay đổi không thuộc assignment.
2. Tìm source symbol, callers, schema/config và exact tests của chức năng. Ưu tiên graph MCP nếu callable; source search là fallback.
3. Điền inventory dưới đây trước sửa; không gán `missing` chỉ từ một tên file không tồn tại.
4. Với `reuse_verified`: giữ implementation và viện dẫn assertion/evidence phù hợp. Với `adapt`: sửa đường hiện có và tất cả callers. Với `missing`: thêm module nhỏ vào owner đã xác định rồi nối caller thật. Với `incompatible`: ghi migration/compatibility adapter cần thiết, không fork subsystem.
5. Thêm test gap, chạy affected P/H regressions và M cases đến hạn. Chứng minh feature đi từ `ha` qua service hiện tại tới state/output; error/cancel/restore cũng dùng cùng đường.
6. Ghi evidence/handoff ở `docs/`, báo rõ reuse gì, đổi gì, phần nào chưa verify. Không tự làm tiếp toàn roadmap.

| Requirement | Symbol/path + caller | Test selector/assertion + evidence revision | Disposition | Gap và thay đổi tối thiểu |
|---|---|---|---|---|
| Ví dụ M3 multi-step | Inspect TurnDriver + AgentSessionService hiện tại | Điền test thực sự model→tool→model, không dựa tên target | Chưa phân loại trước khi kiểm tra | Chỉ bổ sung behavior/assertion còn thiếu |

`reuse_verified` cần evidence khớp source hoặc rerun phù hợp; trạng thái này không thay requirement nghiệm thu/platform. Không bắt chạy lại toàn test suite khi chỉ mapping docs, nhưng sửa runtime phải chạy required checks của phần bị tác động.

## 5. Compatibility và điều kiện bàn giao

- Một product entrypoint `ha`; help/subcommands/JSON/exit codes và config/data locations hiện được hỗ trợ tiếp tục hoạt động. Thay public contract cần versioning/migration rõ, không thay ngầm.
- DB/config changes có fixture phiên bản trước, migration trong đúng milestone, reopen/resume và failure-recovery assertions. Test temp homes là isolation của fixture, không phải thay data home của sản phẩm.
- Registry `tests/acceptance/registry.json` và gates P/H được giữ. Mapping M tham chiếu tests đã có hoặc thêm coverage vào cùng runner. Gate M không thay pass status hiện tại bằng mặc định planned.
- Docs `manifest.json` là planning-only; không dùng để kết luận runtime chưa có hoặc đã hoàn tất. Evidence thực thi nằm trong `docs/evidence/`, handoff trong `docs/handoffs/`.
- Hoàn tất một item phải chứng minh reuse/integration, không chỉ file/module mới biên dịch được. Không phát sinh CLI sản phẩm thứ hai; test helper binaries hiện có vẫn hợp lệ.
