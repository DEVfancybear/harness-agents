# Sổ tay triển khai theo phase

[English](README.en.md) | Tiếng Việt

Runbook revision 1 — 10/09/2026. Tách từ kiến trúc revision 2 tại commit `b636208`. Thư mục này đặc tả công việc tương lai; mọi phase hiện **chưa bắt đầu**. Việc viết tài liệu không tạo Rust runtime hay phase test runner.

## 1. Cách dùng bộ tài liệu

Giao cho coding agent prompt ở mục 9 của phase đã chọn. Bắt đầu P0. Cấp quyền truy cập repo và giao việc rõ ràng; chỉ đọc bộ docs không có nghĩa được chạy phase sau, spawn worker, cài công cụ tùy ý hoặc phát hành release.

Mỗi phase có bảy work items theo thứ tự, file ownership dự kiến, quyết định contract, tests, demo, exit gate và yêu cầu handoff. 63 step IDs là định danh công việc ổn định. Target paths là file cần tạo/mở rộng sau khi inspect code thật; không dựng scaffold mới đè implementation đã có.

Nguồn chuẩn: [plan kiến trúc](../RUST_HARNESS_PLAN.vi.md), [hợp đồng plugin](../PLUGIN_ARCHITECTURE.vi.md), [hợp đồng memory](../MEMORY_AND_CONTINUITY.vi.md), rồi đến runbooks này. Runbook phân rã contract, không âm thầm làm yếu nó. Khi có mâu thuẫn thật, nêu bằng chứng và đề xuất cách giải quyết cụ thể.

## 2. Thứ tự và phạm vi phases

| Phase | Runbook | Điều kiện trước | Ngày công | Trạng thái hiện tại |
|---|---|---|---:|---|
| P0 | [Nền tảng, contracts và khung kiểm thử](P0_FOUNDATION.vi.md) | — | 3–4 | not_started |
| P1 | [Plugin kernel và lưu trữ bền vững](P1_KERNEL_STORAGE.vi.md) | P0 | 8–11 | not_started |
| P2 | [Agent runtime, context và compaction](P2_RUNTIME_CONTEXT.vi.md) | P1 | 7–10 | not_started |
| P3 | [Coding tools, execution policy và receipts](P3_CODING_TOOLS.vi.md) | P2 | 7–10 | not_started |
| P4 | [Memory tái sử dụng và extraction phục hồi được](P4_MEMORY.vi.md) | P3 | 7–10 | not_started |
| P5 | [Giao việc, task DAG và workspace cô lập](P5_MULTI_AGENT.vi.md) | P4 | 8–12 | not_started |
| P6 | [Skills, MCP và protocol plugin bên ngoài](P6_EXTENSIONS.vi.md) | P5 | 5–8 | not_started |
| P7 | [Gia cố recovery và phát hành CLI](P7_RELEASE.vi.md) | P6 | 8–11 | not_started |
| P8 | [Web UI dùng chung host services](P8_WEB.vi.md) | P7 | 10–15 | not_started |

P0 → P1 → P2 → P3 → P4 → P5 → P6 → P7 → P8 tùy chọn. Chỉ bắt đầu phase khi integration gate của phase trước đã được chấp nhận. Có thể chồng thời gian đọc thiết kế/chuẩn bị fixtures, nhưng không merge code theo interface tiền nhiệm chưa được chốt.

P0–P7 vẫn 53–76 ngày công trước dự phòng; P8 tính thêm. Đây không phải deadline chạy agent. Nhiều agent không đồng nghĩa lấy số tuần chia cho số agent.

`manifest.json` là danh mục kế hoạch chứa dependencies, steps và case ownership. Nó **không** là task database của sản phẩm. Hoàn tất trong tương lai phải có evidence tại source revision, không chỉ sửa status trong JSON.

## 3. Quy trình bắt đầu chung cho coding agent

1. Đọc quy định repo áp dụng, Git status, phase được giao và handoff/evidence tiền nhiệm. Giữ nguyên thay đổi không liên quan.
2. Xác minh revision tiền nhiệm và tests liên quan trong checkout thật. Lời báo “xong” của agent khác chưa đủ.
3. Viết phase SPEC ngắn ở `docs/specs/Pn.en.md`, `Pn.vi.md`: scope, test mapping, failure modes, dependencies, thay đổi môi trường, quyết định chưa chốt. Đây là output tương lai, không phải links đã tồn tại.
4. Xác nhận với user trước khi mở rộng thay đổi contract/security/storage quan trọng. “Triển khai phase” không cho phép xóa dữ liệu, gọi API trả phí không giới hạn, public release hoặc activate host.
5. Nhận đúng step/file ownership đã thống nhất với coordinator. Triển khai từng lát cắt kiểm thử được; tái hiện thất bại kỳ vọng trước khi sửa khi phù hợp.
6. Chạy phase gate và regression suite có sẵn sau thay đổi cuối. Kiểm tra test discovery thật: Cargo filter chạy 0 tests và exit 0 không là bằng chứng.
7. Tạo evidence gắn source và continuation handoff cả khi blocked/gián đoạn. Không đánh dấu phase xong nếu required gate chưa chạy.

## 4. Hợp đồng dùng chung giữa phases

P0 chốt serialization versions, ID formats, typed error conventions, test registry format, public CLI naming. Fields/hành vi đặc thù được chốt tiếp trong SPEC phase sở hữu. Revision phải rõ: giữ event decoding, migrate storage, cập nhật hai ngôn ngữ và test fixtures cũ.

Ownership mặc định:

- `harness-types`: shared IDs, event envelopes; không có business policy.
- `harness-kernel`: plugin graph, scoped registries, leases, resource lifecycle.
- `harness-session`: instruction ledger, task/session projections, WorkingState, recovery.
- `harness-store-sqlite`: migrations, transaction coordinator, durable queues, artifacts, query storage.
- `harness-runtime`: agent actor, application services, context admission, request lifecycle.
- `harness-providers`: mock và DeepSeek adapters.
- `harness-tools`: policy gate, receipts, filesystem/Git/process adapters.
- `harness-memory`: assets, extraction, retrieval, provenance, invalidation.
- `harness-orchestrator`: task DAG, agent ownership, workspaces, handoff integration.
- `harness-cli`: CLI presentation/composition, cross-crate acceptance fixtures.

Ban đầu có thể dùng module thay crate, nhưng handoff P0 phải map mỗi logical owner sang path thật. Domain records có một authoritative writer; thêm crate không tạo database authority thứ hai. Web adapter tương lai gọi application services, không gọi CLI internals hay SQL mutation trực tiếp.

## 5. Hợp đồng kiểm chứng

Lệnh duy nhất hiện chạy được trong bộ này là:

```powershell
pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest
```

P0 phải tạo interface **tương lai** sau. Lệnh của các phase tiếp theo giả định gate đã có:

```powershell
pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P0
```

Phase runner phải fail closed và chạy:

- `cargo fmt --all -- --check`.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`.
- `cargo test --workspace --all-targets --locked`.
- Phase acceptance target đã khai báo, test discovery không rỗng, không ignore required cases.
- Documentation checks và OS/process/crash/migration gates do phase đó bổ sung.

P0 tạo `crates/harness-cli/tests/phase_p0.rs`; phase sau thêm `phase_pN.rs`. Các cross-crate acceptance targets điều phối implementation thật; unit tests ở gần module sở hữu. Không mock transaction engine, policy gate hay chính component đang kiểm chứng. Chỉ mock boundaries như model/network/clock khi phù hợp.

P0 tạo `tests/acceptance/registry.json`: case ID, phase sở hữu, target/test names, fixture, platforms bắt buộc, readiness. Tests phase tương lai để `not_implemented`, không để xanh hoặc skip ngầm. `Verify-Phase` kiểm tra phase được chọn và gates đã được chấp nhận trước đó; P7 yêu cầu đủ C01–C30/K01–K14. P8 thêm W01–W06 riêng.

Component fixture có thể có trước bài end-to-end tăng cường; [bảng ownership nghiệm thu](ACCEPTANCE_MAP.vi.md) phân biệt rõ. P1 pass synthetic receipt fixture không có nghĩa process runner P3 đã hoạt động.

## 6. Định dạng evidence và handoff

Khi xong phase, tạo cặp `docs/evidence/Pn.en.md`, `Pn.vi.md`. Lưu machine-readable results/artifacts vào vị trí thuộc task được chọn ở P0; không đưa credentials hay raw private project content vào Git. Evidence gồm:

```text
phase / assigned steps / result: passed | failed | blocked | partial
base revision / tested revision hoặc source-tree digest
changed files và ownership
thay đổi contract/schema/dependency
case ID -> exact test -> command -> observed result
platform và toolchain versions
demo command và artifact references
checks chưa chạy kèm lý do; known limitations
steps còn lại và blockers
next safe action và điều kiện cho phase tiếp
commit/push/CI state: kết quả thật hoặc not requested
```

Kết quả gắn với source cụ thể. Tests trước thay đổi code cuối là stale. Thiếu OS hoặc provider credential phải báo; required release gate vẫn chưa hoàn tất. Documentation validation không thay runtime verification.

Trong lúc làm, duy trì handoff gọn: `docs/handoffs/Pn.en.md`, `Pn.vi.md`, ghi steps đã xong, lỗi, exact commands, file fingerprints, pending operations, next action. Đây là handoff khi phát triển, không thay durable memory của harness sau này.

## 7. Nhiều implementation agents

User/coordinator giao việc song song rõ ràng. Trong một phase, workers có thể làm module không trùng sau khi shared contracts được chấp nhận. Một integrator sở hữu root Cargo files/lockfile, shared schemas, migration numbering và acceptance registry. Không để hai worker sửa cùng file nếu chưa thống nhất handoff.

Assignment worker phải ghi phase/step IDs, input revision, owned paths, allowed dependencies, acceptance tests và forbidden changes. Worker trả results/evidence; integrator xử lý overlap, test revision đã tích hợp và cho chuyển phase. Không khởi chạy agents làm P0–P8 đồng thời.

Branches/workspaces cô lập tùy chọn phải giữ thay đổi user. Tuân theo worktree tooling của môi trường thật; bộ docs không cấp quyền thao tác Orca-managed state. Không có quyền commit/push mặc định: assignment coding phải nói rõ có yêu cầu publish không.

Quy định xin quyền trước khi spawn agents áp dụng cho trợ lý coding bổ sung. Không cấm chạy product-agent actors cô lập mà acceptance tests của phase được giao yêu cầu.

## 8. Quy tắc dừng và phục hồi

Dừng phần việc bị ảnh hưởng khi thiếu predecessor bắt buộc, contract schema/security mâu thuẫn, tests cho thấy mất dữ liệu, mutation outcome chưa rõ, hoặc sắp ghi đè dữ liệu user được bảo vệ. Tiếp tục chẩn đoán an toàn/fixtures cô lập; không bỏ qua guard đang fail.

Khi hết context hoặc đổi agent, agent mới đọc handoff, Git diff, evidence, chạy lại baseline cần thiết và tiếp tục step chưa xong. Không tạo lại cả phase đã hoàn tất chỉ vì mất lịch sử hội thoại.

Delivery có ích đầu tiên là demo kill/reopen P1; P3 thêm coding thật, P5 thêm delegated agents, P7 là CLI release gate. Web vẫn riêng.

## 9. Prompt giao việc đầu tiên

```text
Chỉ triển khai P0 trong repository này.
Đọc docs/implementation/README.vi.md và P0_FOUNDATION.vi.md trong cùng thư mục,
sau đó đọc các hợp đồng kiến trúc được link và quy định repo áp dụng.
Inspect checkout hiện tại; tạo SPEC P0 và thực hiện P0-S01..P0-S07.
Không triển khai runtime, memory extractor, multi-agent scheduler hoặc Web UI.
Dùng fixtures không cần credentials thật. Chứng minh test discovery và negative controls.
Bàn giao evidence hai ngôn ngữ cùng handoff gắn với source revision đã kiểm thử.
Không bắt đầu P1 hoặc spawn agents khác nếu tôi chưa giao việc đó rõ ràng.
Không commit/push nếu assignment này chưa yêu cầu publish.
```
