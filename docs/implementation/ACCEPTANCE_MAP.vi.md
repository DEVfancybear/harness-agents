# Bảng sở hữu và nghiệm thu triển khai

[English](ACCEPTANCE_MAP.en.md) | Tiếng Việt

## 1. Cách đọc bảng

Các case IDs C01–C30, K01–K14 giữ nguyên định nghĩa trong [memory contract](../MEMORY_AND_CONTINUITY.vi.md) và [plugin contract](../PLUGIN_ARCHITECTURE.vi.md). Bảng này gán **một phase phụ trách bằng chứng component đầu tiên** cho mỗi case, không thay thế điều kiện gốc. P0 dựng fixture/test registry; không được nhận đã chạy 44 runtime cases.

“Củng cố” nghĩa là chạy lại bằng component downstream hoặc tích hợp đầy đủ hơn. P1 dùng controlled receipts và synthetic principals khi chưa có process tool/agent thật; P3/P5 phải bổ sung chứng minh thật ở boundary đó. P7 chạy toàn bộ 44 cases trên runtime tích hợp. P8 chạy lại các regressions đó và thêm sáu ca Web. Mọi case trong bảng hiện **chưa triển khai**.

Primary ownership được khai báo trong [manifest](manifest.json); step triển khai cụ thể nằm ở [runbooks từng phase](README.vi.md). Một case có thể cần nhiều test functions và fault windows. Không ép một function chứa tất cả rồi bỏ qua một nửa điều kiện.

## 2. Continuity C01–C30

| Case | Phase phụ trách | Fixture và kết quả bắt buộc | Củng cố tới P7 |
|---|---|---|---|
| C01 | P1 | Kill ngay sau durable ACK của input; mở lại có đúng một input cùng ID. | P2, P7 |
| C02 | P1 | Commit result, kill trước snapshot, fold tail không lên lịch lại invocation đó. P1 dùng receipt kiểm soát; P3 bổ sung tool thật. | P2, P3, P7 |
| C03 | P3 | Thực hiện side effect trên fixture tạm, kill trước receipt commit; nhận diện uncertainty và reconcile trước retry. | P5, P6, P7 |
| C04 | P2 | Ép task mẫu compact năm lần; kiểm tra objectives, corrections, decisions, failures, pending work trong mỗi request tiếp. | P7 |
| C05 | P4 | Làm summary và boundaries embedding/extraction tùy chọn lỗi; vẫn resume bằng WorkingState. Jobs retryable còn bền vững; fixture không đòi feature embedding production. | P7 |
| C06 | P2 | Mở cùng task ở session mới; giữ checkpoint, artifact scope, bước còn lại; loại dữ liệu task khác. | P7 |
| C07 | P4 | Ba principals do host tạo publish đồng thời: versions bất biến, CAS conflict rõ, cursors riêng. P4 test memory API; P5 chạy lại bằng agents thật. | P5, P7 |
| C08 | P4 | Giả actor/project/scope IDs và read/export asset trực tiếp; host authority quyết định visibility, không tin tham số caller. | P5, P6, P7 |
| C09 | P2 | Ghi decision A rồi thay rõ bằng B; mandatory context dùng B, đánh dấu A superseded qua resume/compaction. | P4, P7 |
| C10 | P3 | Sửa tracked hoặc dirty files bên ngoài host; so fingerprint workspace có ý nghĩa, làm stale receipts/code facts trước tái sử dụng. | P5, P7 |
| C11 | P5 | Dừng parent quanh lúc child hoàn tất; coordinator phục hồi nhận durable child result, không làm lại việc đã xong. | P7 |
| C12 | P4 | Chèn crash quanh settlement asset/job/cursor extraction; retry không tạo version trùng hay tách cursor khỏi outcome. | P7 |
| C13 | P4 | Tái dùng cùng source qua mười lần context injection; lineage ngăn tính bản inject như các quan sát độc lập. | P7 |
| C14 | P2 | Replay sanitized packets đã lưu khi tắt provider/tools; dùng fixtures chunks/format/versions nhưng không execution mới. | P6, P7 |
| C15 | P1 | Cho hai host processes tranh một session, thử stale commit sau takeover; chỉ fencing generation hiện tại được ghi. | P2, P5, P7 |
| C16 | P4 | Dùng hơn hai trang events cùng timestamp; sequence cursor xử lý mọi record, không bỏ siblings giữa pages. | P7 |
| C17 | P4 | Thử settle extraction range sau trước gap; watermark liên tục không vượt gap, gap vẫn hiển thị. v1 serial vẫn test completion không hợp lệ này. | P7 |
| C18 | P2 | Giữ instruction đã nhận ở trạng thái chưa classify, rồi compact/mở lại; instruction gốc đã sanitize vẫn mandatory. | P4, P7 |
| C19 | P2 | Cho project rule bắt buộc relevance bằng không; mandatory admission vẫn thêm rule độc lập top-k. | P4, P7 |
| C20 | P4 | Revoke source mà cached summary hoặc packet candidate đang dùng; loại/rebuild dependencies, recheck trước dispatch, audit đúng scope. | P6, P7 |
| C21 | P1 | Inject storage exhaustion tại input/intent/result commit, không làm đầy disk người dùng. P1 chứng minh transaction; P3 chứng minh không chạy tool mới thiếu intent và phục hồi uncertain result. | P3, P7 |
| C22 | P4 | Extractor unavailable, đổi strategy/model schema, JSON lỗi không được đẩy cursor hay nâng policy; jobs ghi outcome retryable/blocked rõ. | P7 |
| C23 | P3 | So clones riêng, roots đã move, linked worktrees có remote/path giống nhau; project IDs rõ ngăn nhập nhầm task/memory. | P5, P7 |
| C24 | P1 | Hai sessions tranh quyền tiếp tục một task; task lease/fencing ngăn hai owners dù session IDs khác nhau. | P2, P5, P7 |
| C25 | P5 | Commit child result rồi mất parent notification; durable inbox/outbox replay giao một lần về logic, tham chiếu result gốc. | P7 |
| C26 | P4 | Query FTS rỗng, tiếng Việt, code identifiers; mô phỏng timeout boundary embedding tùy chọn. Phân biệt empty/degraded/error, không bịa memory. | P7 |
| C27 | P7 | Restore backup DB/artifacts nhất quán vào directory mới, migrate versions hỗ trợ, từ chối old writer không hỗ trợ mà không mutation. | P7 |
| C28 | P7 | Forget source rồi resume task cũ; tombstones chặn re-extraction, derived content không còn dùng được, báo rõ evidence thiếu. | P7 |
| C29 | P3 | Dùng secret giả trong fixture và artifact hash scope khác; capture/export có redaction và object authorization độc lập vẫn có hiệu lực. | P4, P6, P7 |
| C30 | P4 | Hết budget memory job và exit giữa extraction; foreground state an toàn, job pause bền vững rồi resume với budget hữu hạn mới. | P7 |

## 3. Plugin K01–K14

| Case | Phase phụ trách | Fixture và kết quả bắt buộc | Củng cố tới P7 |
|---|---|---|---|
| K01 | P1 | Từ chối thiếu services, contracts không tương thích, cycles trước input admission; lỗi có dependency chain. | P6, P7 |
| K02 | P1 | Bỏ optional extractor không phá required composition. P1 chứng minh mount; P2/P4 chứng minh coding resume và trạng thái disabled rõ. | P2, P4, P7 |
| K03 | P1 | Làm mount lỗi sau khi đăng ký tool/timer; rollback xóa handles, cancel và join tasks sở hữu. | P6, P7 |
| K04 | P1 | Override tool ở child scope; nearest lookup, sibling isolation, từ chối duplicate cùng layer đúng. P5 dùng workers thật. | P5, P7 |
| K05 | P1 | Chạy disposer generation cũ sau replacement; không được xóa registration generation mới. | P6, P7 |
| K06 | P1 | Gỡ required provider khi call đang chạy; dừng admission, drain dependents, lưu result/uncertainty trước đóng storage. | P3, P5, P7 |
| K07 | P1 | Cho hai shutdown callers tranh nhau, một async disposer lỗi; dùng chung completion, await đúng dependency order, báo lỗi, đóng store cuối. | P3, P5, P7 |
| K08 | P3 | Ancestor deny vẫn hiệu lực khi có allow sau; đổi arguments/workspace/policy làm approval mất hiệu lực trước execution. | P6, P7 |
| K09 | P3 | Cho observer panic hoặc renderer báo success sau denial; receipt bất biến và task evidence vẫn denied. | P7 |
| K10 | P2 | Đổi config giữa request/batch; frozen composition hoàn tất hoặc reconcile, step tiếp ghi revision mới. | P4, P6, P7 |
| K11 | P2 | Gặp critical event lạ/projector version không hỗ trợ; từ chối unsafe resume, giữ read-only inspection và history. | P6, P7 |
| K12 | P6 | Gửi stdio frame lỗi/quá cỡ, IDs sai, crash hoặc bỏ cancel; resource bị giới hạn, không retry mù mutating invocation. | P7 |
| K13 | P6 | Gọi MCP/nested tool cố bypass policy; cùng host gate kiểm tra call tương quan và grants không leo quyền. | P7 |
| K14 | P6 | Repo profile yêu cầu load executable hoặc secrets; phải có user-level trust rõ và redact snapshots. | P7 |

## 4. Web W01–W06 bổ sung

Chỉ thuộc P8 tùy chọn; không tính vào 44 ca CLI. Định nghĩa này là phần mở rộng triển khai cho yêu cầu Web UI dùng chung services.

| Case | Phase phụ trách | Fixture và kết quả bắt buộc |
|---|---|---|
| W01 | P8 | Cùng task qua CLI/Web services: cùng authoritative IDs, receipts, scope, pending steps; adapter không ghi SQL trực tiếp. |
| W02 | P8 | Disconnect/reconnect với cursor còn giữ lấy lại visible events một lần về logic; cursor hết hạn yêu cầu resnapshot, không dispatch model thêm. |
| W03 | P8 | Retry POST cùng command ID sau mất response; chỉ một logical admission. Approval stale bị từ chối trước tool execution. |
| W04 | P8 | Thiếu/hết hạn auth, sai origin, CSRF, reads IDs/hash khác scope bị từ chối; revoke cũng áp dụng streams đang mở mà không leak dữ liệu riêng. |
| W05 | P8 | Đóng/mở browser khi foreground host còn chạy: cùng task tiếp tục. Writer thứ hai không được chiếm data directory đó. |
| W06 | P8 | Render HTML và links nguy hiểm từ model/tool/memory an toàn; credentials giả không xuất hiện trong page source, event payloads hay logs. |

## 5. Registry, gates và evidence

P0 tạo `tests/acceptance/registry.json`, ánh xạ IDs tới test targets/names, fixtures, platforms và readiness. Runbook này không tạo registry runtime hay tests giả. Ở phase N, gate phải yêu cầu mọi case có primary phase không muộn hơn N, các strengthening cases của N và mọi regression đã được nghiệm thu. Với P0, chạy foundation tests riêng; tất cả C/K vẫn `not_implemented`.

Test runner kiểm tra exact test discovery, số tests thực chạy, ignored/skip reasons và platform coverage. Required case chưa implement hoặc chưa chạy làm gate không đạt; exit code 0 của filter rỗng không đủ. `ALL_C`/`ALL_K` trong manifest phải expand đủ IDs, không hiểu thành một smoke test đại diện.

Evidence cho từng case ghi: revision, test target/function(s), fixture ID/seed, fault window, assertion quan sát được, kết quả, platform và artifact sạch secrets. Phân biệt component fixture, multi-agent thật, browser E2E và live-provider eval. Không thay mock result bằng chữ “end-to-end”.

Các phase còn có tests riêng ngoài C/K/W: P0 foundation, provider streaming, path safety, process-tree cleanup, budget limits, packaging và benchmarks. 44 cases không phải toàn bộ test suite. Xem [hợp đồng kiểm chứng chung](README.vi.md) và mục 5 của mỗi phase.

Checker tài liệu hiện tại chỉ kiểm tra cấu trúc, coverage IDs/ownership, phase dependencies và step parity. Nó không chứng minh runtime đã hoạt động.
