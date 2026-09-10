# Kiến trúc plugin và hợp đồng triển khai

[English](PLUGIN_ARCHITECTURE.en.md) | Tiếng Việt

Revision 2 — 10/09/2026. Chỉ là thiết kế: SDK, cấu hình và tests dưới đây chưa được triển khai. Đọc cùng [kế hoạch](RUST_HARNESS_PLAN.vi.md) và [hợp đồng memory](MEMORY_AND_CONTINUITY.vi.md).

## 1. Học điều gì từ DeepSeek

Khảo sát tại commit DeepSeek `2377c272a8e839e0a84c9f0e623b867a1dce2014`. Cordis kernel cung cấp composition/vòng đời; capability của agent nằm ngoài kernel. Không hiểu “everything is a plugin” thành “plugin nào cũng được vượt bất biến của host”. [Kiến trúc](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/docs/architecture.md).

| Cơ chế đã đọc | Áp dụng vào thiết kế Rust |
|---|---|
| Plugin cung cấp service; consumer khai báo `inject` | Resolve hợp đồng service độc lập với package implementation |
| Required service biến mất thì consumer bị unload; service trở lại cho phép load lại | Theo dõi dependency cả sau startup |
| Registration là effect có chủ sở hữu, kể cả child plugin | Teardown cần ownership, cancellation và chờ hoàn tất, không chỉ xóa registry |
| Config entry ID ổn định giúp phân biệt cập nhật với thay thế | Tách identity implementation và identity instance đang mount |
| Scoped registry kế thừa định nghĩa xuống dưới; listener tổ tiên quan sát descendants | Quy định lookup và event delivery riêng |
| Service definition, provider và consumer có thể thay đổi độc lập | Tách thực thi process khỏi tool trình bày cho model |

Nguồn: [Services](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/docs/cordis-tutorial/03-services.md), [Lifecycle](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/docs/cordis-tutorial/02-lifecycle-and-effects.md), [Composition](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/docs/cordis-tutorial/06-composition-and-hmr.md), [Scope](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/packages/core/scope/README.md), [Ba vai trò](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/docs/user/develop/practice/index.md).

Khác biệt quan trọng: Cordis có thể để consumer thiếu dependency ở trạng thái pending. CLI sản phẩm của ta từ chối composition thiếu thành phần bắt buộc trước khi nhận việc. Async disposers của Cordis có thể chạy đồng thời dù bắt đầu theo thứ tự ngược; Rust chờ rõ từng pha phụ thuộc. Đây là lựa chọn riêng, không phải mô tả hai implementation giống hệt nhau.

## 2. Kernel tối thiểu và capability nghiệp vụ thay thế được

Kernel chỉ quản lý service registry, plugin instances, dependency graph, scope tree, resource ownership, lifecycle và diagnostics. Không đưa prompt, memory ranking hay task semantics vào kernel.

Composition đáng tin của ứng dụng bắt buộc có session durability, execution policy và recovery. Chỉ thay implementation bằng provider đáng tin tương thích cùng hợp đồng. Tắt durability bắt buộc phải gây validation error; nếu sau này có profile benchmark dùng một lần thì phải công bố rõ bảo đảm khác biệt.

| Hợp đồng/definition | Provider | Consumer |
|---|---|---|
| `ModelProvider` | DeepSeek, mock, adapter tương lai | Agent loop; extractor có budget riêng |
| `ProcessRunner` | Runner Windows Job Object; runner Linux | Tool `run_process`, verifier |
| `FileSystem` | Workspace local; sandbox adapter tương lai | Tools đọc/sửa/tìm kiếm |
| `SessionStore` + `StoreCoordinator` giao dịch | SQLite | Runtime, task service, recovery |
| `AgentDriver` / agent factory | Loop mặc định | CLI application service, orchestrator |
| `MemoryStore`, `MemoryRetriever`, `MemoryExtractor` | Assets/FTS local; Tencent adapter tương lai | Context builder, memory tools, durable jobs |
| `Compactor`, `ContextContributor` | Compaction ưu tiên WorkingState; contributors skills/memory | Context builder |
| `SubagentBackend`, `WorkspaceManager` | Agent trong process, Git worktree do host sở hữu | Coordinator/task service |

Chỉ có interface không bảo đảm được transaction xuyên các backend độc lập. v1 dùng `StoreCoordinator` sở hữu unit of work SQLite chung; plugin gửi domain command có kiểu thay vì tự commit các bảng liên quan. Backend remote sau này cần outbox/reconciliation rõ ràng; không tuyên bố atomicity xuyên kho khi chưa có cơ chế.

Không tách sớm mỗi vai trò thành một crate. Giữ mười nhóm crate trong plan, thêm module cho các hợp đồng này. Kiểu nghiệp vụ nằm ở module contract sở hữu chúng; `harness-types` chỉ giữ IDs/envelopes dùng chung, tránh trở thành nơi gom mọi dependency.

## 3. Manifest, identity và lookup service có kiểu

Phân biệt và lưu rõ:

- `plugin_id`: họ implementation, ví dụ `memory.local`.
- `implementation_version` và build/content digest: identity mã chính xác.
- `instance_id`: ID dòng cấu hình ổn định; nhiều instance có thể dùng cùng implementation.
- `scope_id`: ranh giới application/project/agent do host tạo.
- `generation`: identity lần activation; chặn callback và lease cũ.
- `host_api_version`, versions hợp đồng services cung cấp/cần dùng, config schema version.
- Capabilities được đề nghị, restart policy, plugin vắng mặt có chặn recovery không.
- Durable event schemas và projection versions do implementation sở hữu.

v1 resolve từ catalog implementations đã compile. Typed service keys trả trait-object handles có kiểm tra version contract; consumer không import provider cụ thể hay ép JSON tùy ý thành service. SemVer implementation, host protocol version và event schema version là ba phép kiểm tra tương thích riêng.

Hợp đồng SDK minh họa, chưa phải SDK code biên dịch được:

```text
Plugin.describe() -> Manifest
Plugin.validate(config, available_contracts) -> ValidatedConfig
Plugin.mount(scoped_host, validated_config) -> OwnedResources
ServiceLease.call(request, cancellation) -> Result
OwnedResources.shutdown(deadline) -> ShutdownReport
ContextContributor.collect(read_only_snapshot, budget) -> ProposedBlocks
MemoryExtractor.extract(immutable_source_batch) -> ProposedMemoryMutations
```

Lifecycle futures phải hỗ trợ cancellation. Khi triển khai, chọn boxed futures object-safe hoặc cơ chế trait được hỗ trợ rõ; không giả định mọi async trait đều đưa nguyên vào `dyn`. Không dựa vào Rust ABI ổn định xuyên các binary.

## 4. Scope, kế thừa và quyền

```text
application
  project-A
    coordinator
      worker-1
      worker-2
  project-B
```

Ba câu hỏi độc lập phải có ba câu trả lời:

1. **Lookup:** implementation đăng ký nào nhìn thấy được?
2. **Lifetime:** owner nào phải cancel và dispose nó?
3. **Authority:** actor này được thực hiện thao tác gì?

Named lookup bắt đầu ở scope gần nhất rồi đi lên tổ tiên. Từ chối tên trùng trong cùng layer. Override ở child phải là config entry rõ ràng; gỡ nó thì lộ lại định nghĩa tổ tiên. Undo nhắm đúng `(instance, generation, registration_id)`, nên disposer đến muộn không xóa replacement cùng tên.

Hai scope ngang hàng không thấy private registrations của nhau. Tổ tiên có quyền có thể quan sát hoạt động scoped, nhưng payload event vẫn qua kiểm tra quyền dữ liệu; global observer không mặc nhiên được đọc secret hay mọi memory asset. Child nhìn thấy tổ tiên không có nghĩa đóng góp của child tự xuất hiện ngược lên trên.

Quyền là giao của host, user, project và grants task/agent. Override local không xóa được deny từ tổ tiên. Đóng băng tool definitions cho một request model và kiểm tra lại quyền ngay trước side effect. Approval gắn actor, invocation ID, resolved arguments, workspace revision, tool implementation và policy revision; thay đổi quan trọng làm approval mất hiệu lực.

`ScopeKey` của DeepSeek định tuyến registrations/events đáng tin trong cùng process; service thông thường không tự cô lập chỉ vì được gọi qua scoped context. Rust dùng API có scope rõ và principal do host tạo, không đưa toàn bộ service container cho worker. [Giới hạn scope](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/packages/core/scope/README.md), [Implementation layer](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/packages/core/scope/src/store.ts).

Rust built-in đáng tin vẫn chạy bằng đặc quyền process. Trait là ranh giới tổ chức code, không ngăn được native code độc hại. Plugin không đáng tin cần ranh giới process/OS được cưỡng chế; chỉ stdio không phải sandbox.

## 5. Vòng đời và xử lý lỗi

```text
declared -> validated -> waiting_dependencies -> initializing -> active
                                         failure -> failed
active -> draining -> stopped
stopped -> initializing (generation mới, chỉ khi được phép restart)
```

Startup validate toàn bộ graph bắt buộc: thiếu provider, lệch contract, cycle, tên trùng, cấu hình sai đều là lỗi. Có thể tắt memory extraction tùy chọn nhưng không tắt journal/WorkingState bắt buộc. Thiếu optional dependency phải có đường degradation xác định.

Mount gom registrations/tasks/process handles vào resource set chưa publish. Chỉ publish khi khởi tạo thành công; lỗi thì rollback set này và join việc đã chạy. Initialization không được gây thay đổi nghiệp vụ không thể thu hồi. Rollback resources không thể hoàn tác file edits hoặc network effects tùy ý đã xảy ra.

Khi mất required service: dừng nhận call phụ thuộc, đánh dấu provider unhealthy, drain/cancel consumers bị ảnh hưởng, rồi thu hồi registrations. Chỉ giữ `Arc` là chưa đủ: handle cũ phải bị chặn qua generation. Tool đang chạy ghi outcome đã xác định hoặc chưa rõ; remount không có nghĩa được chạy lại side effect.

Các pha shutdown:

1. Dừng nhận turn, claim và activation plugin mới.
2. Gửi cancellation cho agent/extractor; đóng approval với trạng thái canceled.
3. Drain requests và cây tool/process; lưu outcome hoặc uncertainty.
4. Lưu tiến độ giao dịch; để background jobs chưa xong có thể phục hồi.
5. Chờ cleanup resources consumer, rồi provider; đóng SQLite cuối cùng.

Các caller shutdown cạnh tranh cùng chờ một completion. Cleanup idempotent; gom lỗi thay vì chỉ log rồi bỏ qua lỗi storage. Hết deadline phải trả báo cáo shutdown chưa hoàn tất, không ghi receipt shutdown sạch. Tắt đột ngột vẫn phải phục hồi qua journal.

Nếu plugin trong process treo, Rust không thể an toàn giết tùy ý một thread và vẫn bảo toàn process. Từ chối reload không an toàn; pause công việc liên quan hoặc dừng/restart host theo recovery protocol. Plugin process riêng có thể bị dừng cả cây và đối chiếu các call chưa xong.

## 6. Hooks và execution receipts bất biến

DeepSeek có nhiều dispatch modes; waterfall là around-middleware với `next`, không phải phép fold đơn giản qua return values. Không biến mọi event thành asynchronous broadcast. [Dispatch semantics](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/docs/cordis-primer.md).

| API Rust | Ngữ nghĩa | Khi lỗi |
|---|---|---|
| Durable domain command/event | Một transactional owner; optimistic revision/fencing | Commit lỗi thì từ chối; không ACK thành công |
| Pre-execution transform | Biến đổi proposed call theo thứ tự xác định | Validate arguments đã đổi trước approval |
| Final guard | Tất cả guard áp dụng; deny hoặc abstain | Một deny/error là từ chối; allow sau không ghi đè |
| Execution wrapper | `next` có thứ tự, tối đa một lần, hỗ trợ cancel | Chuẩn hóa lỗi; không retry side effect chung chung |
| Result presentation | Dựng view model/UI từ execution receipt | Không đổi execution denied/failed thành thành công đã quan sát |
| UI/metrics observer | Notification có giới hạn, cursor đọc bù khi cần | Cô lập lỗi observer; không đổi outcome |

Host ghi riêng `ToolExecutionReceipt` và `ToolResultView`. Receipt ghi cái gì thực sự đã chạy và trên revision nào; presentation có thể truncate, redact, annotate view nhưng không sửa lịch sử execution. Mọi block model thấy được lưu trước request tiếp theo. Đây là ranh giới bằng chứng chặt hơn việc cho post-hook tự thay toàn bộ kết quả.

Chỉ publish settled-result notification cho observer sau khi receipt transaction commit. Cô lập lỗi/panic có thể phục hồi tại task boundary; nếu build abort process khi panic thì vẫn dùng journal recovery nhưng không được tuyên bố cô lập lỗi ngay khi chạy. Không dùng observer làm đường persistence duy nhất.

Mọi entry point—native tools, MCP, nested calls của code-mode tương lai, delegated tools—đều qua cùng execution gate. Tool gọi tool giữ parent invocation IDs và không được mở rộng quyền cho child. Pipeline DeepSeek là nguồn tham khảo, không phải bằng chứng hệ Rust của ta đã an toàn. [Tool pipeline](https://github.com/deepseek-ai/deepseek-harness/blob/2377c272a8e839e0a84c9f0e623b867a1dce2014/docs/tool-execution-pipeline.md).

## 7. Cấu hình và thay đổi an toàn

Thứ tự merge cấu hình dự kiến: built-in defaults → user config → project config được tin cậy rõ ràng → profile đã chọn → CLI overrides. Resolve rows theo stable ID, từ chối field lạ, giải thích nguồn mỗi giá trị hiệu lực. Field phân quyền dùng thu hẹp/giao tập, không dùng last-write-wins thông thường.

Cấu hình trong repository không tự được load executable, mở rộng capability hay resolve secret reference tùy ý nếu chưa có trust cấp user. Rust v1 không cần thực thi biểu thức JavaScript tương tự `!!js`. Credential là reference do host sở hữu, chỉ provider có quyền được resolve.

Fragment profile minh họa, chưa có implementation đọc định dạng này:

```toml
schema_version = 1
extends = "builtin:personal-coding"

[[plugins]]
instance_id = "project-memory"
plugin_id = "memory.local"
scope = "project"
enabled = true

[plugins.config]
retrieval = "fts5"
supplemental_token_budget = 2000

[agent]
max_workers = 3
max_delegation_depth = 2
```

`extends` resolve bundle có version được phân phối sẵn; fragment không phải toàn bộ composition bắt buộc. Include cycle và row operations trùng/mâu thuẫn phải báo lỗi.

Đóng băng `CompositionSnapshot` ở step áp dụng: implementation digests, schema versions, effective nonsecret config, policy revision, tool schemas và contributor versions. `ha config explain`, `ha plugins inspect <instance-id>` hiển thị dependencies, scope, state, active generation và yêu cầu restart.

Các loại thay đổi v1:

- Format observability: đổi ngay nếu không ảnh hưởng nội dung model thấy.
- Retrieval limits/profile: tại step boundary kế tiếp, lưu revision, giới hạn calls đang chạy.
- Chọn tool/model: drain step hiện tại; validate provider/tool compatibility trước request tiếp theo.
- Store, schema, loop hoặc required guards: stop/resume kèm migration/validation; không thay nóng.

Thu hồi quyền chặn ngay thao tác chưa bắt đầu và cancel việc đang chạy khi có thể. Không thể hoàn tác side effect đã xảy ra. Không đợi hết turn chỉ vì cập nhật config thông thường cần boundary.

## 8. Liên tục công việc khi thay plugin

Memory và tiến độ task thuộc host storage bền vững, không nằm riêng trong field của plugin object. Unload extractor vẫn giữ jobs/cursors. Reload không tạo lại jobs đã xong hoặc reset stable agent profile.

Mỗi durable event có `event_type`, `schema_version`, producer identity và phân loại continuity-critical hay optional telemetry. Projector/decoder bắt buộc thuộc bộ schema host được hỗ trợ. Giữ nguyên unknown events; unknown critical event chặn execution resume và báo lỗi tương thích rõ ràng. Optional telemetry có thể bỏ qua kèm diagnostic.

Snapshot ghi projector versions. Khi projection version đổi, dựng lại derived state từ journal events còn hỗ trợ; không sửa ngầm payload lịch sử. Exact historical replay dùng request packets đã lưu dù provider plugin ban đầu không còn. Muốn chạy tiếp cần composition tương thích hoặc provider migration được ghi nhận rõ.

Memory plugin trả candidate blocks/mutations. Chỉ context builder nhận blocks, áp provenance/budget và đóng băng model request. Plugin không được inject thêm text sau đó. Tool definitions, skill versions, workspace instructions và memory versions đều có trong packet manifest.

## 9. Protocol plugin bên ngoài và cấp mở rộng

Built-ins v1 là Rust compile chung; tool plugin phân phối độc lập đến ở P6. Provider bridge dùng cùng transport nhưng capability model/stream có version riêng. Không quảng bá hỗ trợ external loop hoặc storage tùy ý trước khi có contract.

Stdio JSON-RPC handshake khai báo protocol range, plugin identity/digest, capabilities và schema versions. Dùng UTF-8 JSON mỗi frame một dòng, giới hạn frame size; stdout chỉ protocol, stderr logs có giới hạn. Thỏa thuận method versions, stream chunk/terminal semantics, deadlines, max inflight calls và cancellation. Từ chối response IDs lạ/trùng, malformed frames, output floods.

Host cấp call IDs, principal, scopes, workspace roots, grants. Không mở host service lookup hay database queries tùy ý. Plugin xin permission không có nghĩa đã được cấp quyền. Chạy bằng environment tối thiểu, executable digest/path rõ ràng; đọc profile repo không tự download hay chạy code.

Pin plugin versions đã cài local. EOF/crash làm inflight calls chuyển uncertain khi cần; chỉ restart theo policy có giới hạn. Reconnect không cho phép tự retry mutating call. Có thể inspect health capability mà không gọi model.

## 10. Bộ nghiệm thu plugin

Đây là tests đề xuất, chưa phải kết quả pass. Dùng cùng C01–C30 trong tài liệu memory.

| Test | Fixture | Điều kiện đạt |
|---|---|---|
| K01 | Thiếu required service, sai version hoặc dependency cycle | CLI lỗi trước nhận input, có dependency chain |
| K02 | Không có optional extractor | Coding resume vẫn chạy; hiển thị extraction bị tắt |
| K03 | Mount lỗi sau khi đăng ký tool và timer | Rollback không để sót; owned tasks được join |
| K04 | Worker override tool; sibling resolve | Nearest lookup đúng; không rò sibling; chặn tên trùng cùng layer |
| K05 | Disposer generation cũ chạy sau replacement | Registration mới còn nguyên |
| K06 | Required provider biến mất giữa call | Dừng nhận việc; cleanup consumers; outcome/uncertainty được lưu |
| K07 | Hai caller shutdown và một async disposer lỗi | Cùng completion, đúng thứ tự pha, báo lỗi, store đóng cuối |
| K08 | Allow sau ancestor deny; arguments đổi sau approval | Không execute; approval cũ bị từ chối |
| K09 | Observer panic hoặc renderer báo success sau denial | Receipt vẫn denied; lỗi observer không đổi task evidence |
| K10 | Config đổi giữa model/tool batch | Composition cũ hoàn tất/đối chiếu; step mới lưu revision mới |
| K11 | Unknown critical event hoặc projector version không hỗ trợ | Không mất history ngầm; vẫn inspect read-only được |
| K12 | Stdio frame sai/quá lớn, ID sai, crash hoặc không cancel | Resource có giới hạn; protocol error; không retry mutation mù |
| K13 | MCP/nested tool tìm cách bỏ qua policy | Cùng gate, có child correlation, grant không tăng quyền |
| K14 | Repo profile đòi load executable hoặc secret | Từ chối nếu chưa trust cấp user; snapshot không có secret |

P0 định nghĩa contract; P1 triển khai K01–K07; P2/P3 bổ sung K08–K11; P6 thêm K12–K14 và chạy lại toàn bộ. Tests phải kiểm tra hành vi, không chỉ tìm tên method trong code.
