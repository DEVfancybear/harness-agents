# Rà soát kiến trúc — revision 2

[English](ARCHITECTURE_REVIEW.en.md) | Tiếng Việt

10/09/2026. Phạm vi: rà lại coding harness cá nhân Rust multi-agent theo plugin architecture DeepSeek và source memory Tencent, cập nhật hai ngôn ngữ, xuất bản tài liệu. Đây không phải cấp quyền hay bằng chứng runtime đã được triển khai.

## 1. Kết luận

Giữ hướng chính: plugin kernel nhỏ, execution dựa trên events, WorkingState có cấu trúc, memory tái sử dụng có scope, CLI/Web dùng chung application services. Plan đầu đúng hướng nhưng chưa quy định đủ lifecycle, transactions, identity và retention để hướng dẫn triển khai an toàn.

Revision 2 bổ sung các khoảng trống đó. Bước kỹ thuật tiếp theo là P0: schemas cụ thể và executable fixtures, sau đó xây lát cắt journal/restore nhỏ nhất. Chưa được tuyên bố sản phẩm “không bao giờ quên”: có thể kiểm thử việc giữ bằng chứng; hiểu mọi yêu cầu vẫn phụ thuộc model.

Đọc [plan cập nhật](RUST_HARNESS_PLAN.vi.md), [đặc tả plugin](PLUGIN_ARCHITECTURE.vi.md) và [đặc tả memory](MEMORY_AND_CONTINUITY.vi.md).

## 2. Phát hiện và thay đổi sau rà soát

Mức ưu tiên: P0 = cần trong contracts ban đầu; P1 = bắt buộc trước khi phát hành tính năng tương ứng, không nhất thiết nằm trong đợt code đầu.

| ID | Thiếu hoặc chưa rõ ở revision 1 | Quyết định revision 2 | Ưu tiên / nghiệm thu |
|---|---|---|---|
| R01 | “Plugin = trait” chưa mô tả đủ composition | Vai trò definition/provider/consumer; manifest, instance IDs, typed leases, contracts tương thích | P0 / K01–K02 |
| R02 | Có graph startup nhưng chưa rõ khi mất dependency | Dừng nhận việc, drain dependents, thu hồi generation cũ; shutdown chờ đúng thứ tự | P0 / K03, K05–K07 |
| R03 | Scope lẫn visibility, lifetime và security | Tách lookup/ownership/authority; nearest override; deny intersection; trust rõ | P0 / K04, K08, K14 |
| R04 | Đổi config/plugin có thể đổi nghĩa khi resume | Composition snapshots, event/projector versions, lỗi tương thích/migration rõ | P0 / K10–K11 |
| R05 | Post-hook result có thể bị coi là bằng chứng execution | Execution receipt bất biến tách model/UI view; mọi đường qua cùng gate | P0 / K09, K13 |
| R06 | “Cập nhật WorkingState đồng bộ” che rủi ro phân loại ngữ nghĩa | Lưu raw admitted instructions trước ACK; instruction ledger và project rules bắt buộc ngoài top-k | P0 / C18–C19 |
| R07 | Chỉ có session ownership; session mới có thể làm trùng task | Project/task/profile/run IDs ổn định; task/session fencing; một host được ghi mỗi data directory | P0 / C15, C23–C24 |
| R08 | Durable jobs chưa có quy tắc cursor/settlement chính xác | Source-work markers atomic; ranges cũ trước không chồng; contiguous cursors; strategy versions | P0 / C12, C16–C17, C22 |
| R09 | Invalidation chưa mô tả summary dẫn xuất | Dependency graph, kiểm tra revision khi nhận request, loại/dựng lại derived blocks | P0 / C09, C13, C20 |
| R10 | “Chia sẻ” giữa agent dễ thành semantic coordination mơ hồ | Child result và parent delivery bền vững; task state có thẩm quyền; kiểm chứng final revision | P0 / C07, C11, C25 |
| R11 | Persistence thiếu ranh giới vận hành và xóa dữ liệu | Disk-full fail-stop; scoped artifacts; retention/tombstones; backup/migration nhất quán | P1 / C21, C27–C29 |
| R12 | Extraction, retrieval, extensions chưa rõ giới hạn lỗi/chi phí | Memory budget riêng; backlog inspect được; protocol bounded; phân biệt empty/degraded/error | P1 / C05, C26, C30, K12 |

## 3. DeepSeek: cơ chế đã xác minh và khác biệt chủ động

HEAD repo được đọc vẫn là `2377c272a8e839e0a84c9f0e623b867a1dce2014` ở lần khảo sát thứ hai. [Trang tổng quan](https://deepseek.com/harness/en/) mô tả hướng composition; chi tiết dưới đây dựa trên snapshot source cố định.

| Bằng chứng | Quan sát | Quyết định Rust |
|---|---|---|
| [Service tutorial](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/docs/cordis-tutorial/03-services.md) | Required services điều khiển activation và teardown dependents | Theo dõi dependency health; từ chối composition thiếu trước nhận việc |
| [Lifecycle tutorial](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/docs/cordis-tutorial/02-lifecycle-and-effects.md) | Effects sở hữu cleanup; cần chú ý async completion | Shutdown theo pha rõ, store đóng cuối, giữ lỗi |
| [Capability roles](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/docs/user/develop/practice/index.md) | Definition/provider/consumer tách tại ranh giới cần thay thế | Runner độc lập với tool; không chia quá nhiều crate |
| [Scope source](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/packages/core/scope/src/store.ts) | Merge ancestor layers; tên gần nhất ghi đè; exact-entry undo | Registration IDs/generations và policy checks độc lập |
| [Composition tutorial](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/docs/cordis-tutorial/06-composition-and-hmr.md) | Entry IDs ổn định hỗ trợ config diff/HMR | Instance IDs ổn định; v1 cấm thay store/loop nóng |
| [Agent loop source](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/packages/core/agent-loop/src/index.ts) | Create/resume có owner, persistence handle, cleanup, sửa interrupted log | Giữ ownership; tách protocol repair khỏi side-effect outcome chưa rõ |
| [Tool pipeline](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/docs/tool-execution-pipeline.md) | Pre hooks, monotonic guards, execution/post hooks, final observation | Tách execution evidence khỏi presentation bị viết lại |

Đã đọc tests DeepSeek như ví dụ tổ chức contract, không chạy và không tính là bằng chứng cho repo này. Không tuyên bố Rust load được nguyên plugin Cordis.

## 4. Tencent: cơ chế đã xác minh và cách áp dụng

HEAD được đọc vẫn là `906b5823b5106eed8f842b62f16d23228838149a`, nhánh `feat/server_team`.

| Bằng chứng | Quan sát | Quyết định Rust |
|---|---|---|
| [Gateway](https://github.com/TencentCloud/TencentDB-Agent-Memory/blob/906b5823b5106eed8f842b62f16d23228838149a/MemoryCore/src/gateway/server.ts), [stateful manager](https://github.com/TencentCloud/TencentDB-Agent-Memory/blob/906b5823b5106eed8f842b62f16d23228838149a/MemoryCore/src/utils/stateful-pipeline-manager.ts) | Wiring hiện tại dùng backend scheduling và Store-backed extraction, có xử lý backlog | Không áp comment recovery buffer cũ cho toàn sản phẩm hiện tại |
| [Local state backend](https://github.com/TencentCloud/TencentDB-Agent-Memory/blob/906b5823b5106eed8f842b62f16d23228838149a/MemoryCore/src/core/state/local-backend.ts), [backend factory](https://github.com/TencentCloud/TencentDB-Agent-Memory/blob/906b5823b5106eed8f842b62f16d23228838149a/MemoryCore/src/core/state/index.ts) | Queue/claims/timers local dùng RAM; Redis integration load riêng, có thể thiếu trong build | Jobs SQLite native; không cần Redis để resume việc cá nhân |
| [Checkpoint](https://github.com/TencentCloud/TencentDB-Agent-Memory/blob/906b5823b5106eed8f842b62f16d23228838149a/MemoryCore/src/utils/checkpoint.ts), [worker](https://github.com/TencentCloud/TencentDB-Agent-Memory/blob/906b5823b5106eed8f842b62f16d23228838149a/MemoryCore/src/services/pipeline-worker.ts) | Cursor/scheduling có owner riêng, mutation tuần tự, kiểm tra claim/lock | Domain write ownership, fencing lúc commit; mutex không đồng nghĩa transaction xuyên file |
| [L1 factory](https://github.com/TencentCloud/TencentDB-Agent-Memory/blob/906b5823b5106eed8f842b62f16d23228838149a/MemoryCore/src/utils/pipeline-factory.ts#L506) | Extraction bounded cũ trước; có ghi edge case trang trùng timestamp | Paging theo sequence, tests tiến độ liên tục rõ ràng |
| [Profile scope](https://github.com/TencentCloud/TencentDB-Agent-Memory/blob/906b5823b5106eed8f842b62f16d23228838149a/MemoryCore/src/core/profile/profile-scope.ts) | Identity L2/L3 xuyên phiên; row lookup còn kiểm tra isolation | Profile ổn định cộng project scope; task state vẫn riêng |
| [Fixed assets](https://github.com/TencentCloud/TencentDB-Agent-Memory/blob/906b5823b5106eed8f842b62f16d23228838149a/MemoryProxy/src/injection/injectors/tdai-fixed-asset.ts), [profile injection](https://github.com/TencentCloud/TencentDB-Agent-Memory/blob/906b5823b5106eed8f842b62f16d23228838149a/MemoryProxy/src/injection/injectors/tdai-profile-memory-injector.ts) | Self/imported bindings; L3 + L2 index; cache session-init trong đường này | Retrieval bounded, source versions, invalidation tại request boundaries |
| [Dedup](https://github.com/TencentCloud/TencentDB-Agent-Memory/blob/906b5823b5106eed8f842b62f16d23228838149a/MemoryCore/src/core/record/l1-dedup.ts), [prompt resolver](https://github.com/TencentCloud/TencentDB-Agent-Memory/blob/906b5823b5106eed8f842b62f16d23228838149a/MemoryCore/src/core/memory-prompt/resolver.ts) | Candidate matching có scope, extraction strategy có version | CAS merge proposals; source batches bất biến; strategy generations rõ |

Chưa chạy Tencent, integration tests hoặc benchmarks. Đây không phải audit chứng minh Tencent mất dữ liệu hay có lỗi bảo mật. Bài học là chốt bảo đảm tại ranh giới storage/request/execution thật, không suy ra từ tên các lớp memory.

## 5. Hành vi bắt buộc và giới hạn còn lại

Implementation phải giữ inputs/results đã ACK, dựng lại công việc hiện tại khi không có extractor, không bịa evidence hay chạy lại side effect chưa rõ. Yêu cầu hiện hành bắt buộc tồn tại độc lập với optional memory search. Các agent phối hợp qua durable task messages, không qua độ tương đồng memory.

Giới hạn phải nói rõ:

- Context model hữu hạn không bảo đảm suy luận hoàn hảo hoặc giữ tùy ý nhiều yêu cầu; mandatory context quá lớn thì pause.
- Plugin cùng process là native code đáng tin. Worktrees/traits/stdio tự chúng không cô lập code độc hại.
- Xóa source payload hoặc mất thiết bị lưu trữ có thể làm mất continuity evidence. Retention và backup thuộc product contract.
- Một host được ghi mỗi data directory trong v1; host thoát thì việc nền dừng.
- Schema migration, dirty-worktree snapshots, sandbox enforcement và crash recovery đang chờ kiểm thử, chưa là capability có sẵn.
- Embeddings tự động, Wiki/CodeGraph đầy đủ, remote agents, marketplace, Wasm, daemon vẫn để sau.

## 6. Ảnh hưởng lộ trình và kiểm chứng lần rà soát

Contracts mở rộng nâng dự toán lên 53–76 ngày công trước dự phòng, khoảng 13–20 tuần làm việc cho một kỹ sư Rust có kinh nghiệm với 20–30% dự phòng. Web vẫn là đợt thêm 10–15 ngày công. Bảng milestones trong plan là nguồn chính về scope và phép tính.

Bộ tài liệu có bốn cặp Anh/Việt, 30 ca continuity và 14 ca plugin đề xuất. Repo cũng có documentation checker và GitHub workflow chỉ kiểm tra tài liệu.

Chạy tại repo root bằng PowerShell 7:

```powershell
pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest
```

Checker kiểm tra local file links, fences cân bằng, numbered headings hai ngôn ngữ, acceptance IDs chính xác, tổng milestones và commit tham chiếu upstream cố định. Negative controls đi qua đường lỗi thiếu bản dịch, link hỏng, fence chưa đóng, thiếu case, đổi dự toán, nguồn chưa pin. Không kiểm tra Markdown rendering, remote-link availability, ngữ nghĩa bản dịch hay runtime. Đọc source/bản dịch thủ công bổ sung cho checks; không có independent-agent verification.

Kết quả local: lệnh trên pass bằng PowerShell 7.6.5 với chín Markdown files/bốn cặp ngôn ngữ; cả sáu known-bad controls đều bị từ chối đúng. Ngoài ra, đã kiểm tra tồn tại cả 33 upstream blob paths được dẫn duy nhất trong Git objects tại commit đã pin. Những checks này xác minh cấu trúc tài liệu và đường source, không chứng minh mọi nhận định thiết kế hay bảo đảm runtime đều đúng.

Trước commit, còn phải xem staged diff, chạy `git diff --cached --check`, kiểm tra credentials/files không liên quan. Bằng chứng bàn giao là commit đã publish và kết luận documentation workflow. Runtime tests, coverage, mutation, sandbox, model evals vẫn **chưa thực hiện: chưa có runtime**. Workflow tài liệu xanh không được trình bày thành harness chạy được.
