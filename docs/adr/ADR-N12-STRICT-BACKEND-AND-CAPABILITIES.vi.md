# ADR-N12 — Strict execution backend: backend chọn, capability đo được và ranh giới hỗ trợ

**Trạng thái:** accepted trong M12 · **Ngày:** 23/09/2026 · **Phạm vi:** M12-01..M12-04

[Runbook M12](../implementation-next/M12.vi.md) · [SPEC M12](../specs/M12.vi.md) · [Support matrix](../support/STRICT_EXECUTION_SUPPORT.vi.md) · [ADR-N11](ADR-N11-SCHEDULE-AND-DAEMON-SEMANTICS.vi.md)

> Runbook M12 gọi ADR đến hạn là "ADR-N11"; đó là cách đánh số ở thời điểm lập kế hoạch. M11 đã dùng N11, nên ADR của
> M12 là **N12**. Không có ADR nào bị bỏ qua.

## 1. Bối cảnh

M4 đã có một host runner thật: child được tạo `CREATE_SUSPENDED`, gán vào **Job Object** rồi mới resume; `KillOnDrop`
bật `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`; môi trường con bị **clear** rồi chỉ nhận allowlist + binding đã grant; output
đi vào spool có quota; deadline và cancellation giết cả cây rồi `wait` để xác nhận job rỗng. Đó là containment thật.

Nhưng `IsolationMode::Strict` **đã** bị từ chối từ P3 bằng một câu hard-code: *"this host has lifecycle cleanup but no
verified strict isolation sandbox"*. Câu đó **đúng**, và nó không có bằng chứng. M12 phải trả lời câu hỏi mà không
milestone nào được phép trả lời bằng cách đoán: **trên host này, cái gì enforce được, cái gì không, và ai là người
enforce?**

Đây là ADR về **ranh giới**, không phải về một tính năng. Giá trị của nó nằm ở chỗ nó nói "không" ở đâu.

## 2. Quyết định

### D1 — Backend được chọn là Job Object containment đang có, **được đo**; không thêm backend thứ hai

Không có cơ chế mới nào được thêm vào đường chạy. Cái M12 thêm là **measurement, tên gọi và từ chối**. Lý do: (a) một
backend thứ hai không có bằng chứng sẽ là một tuyên bố không kiểm được; (b) ADR-N10 D1 — ưu tiên thứ đã có trong lock
và đã có test; (c) containment hiện tại chưa từng được chứng minh bằng negative control, và đó là khoảng trống thật.

### D2 — Capability là **đo được**, và `unsupported` phải có bằng chứng dương

`CapabilityMatrix` sinh ra bằng probe trên host thật. Mỗi verdict mang `CapabilityEvidence { probe_id, method,
observation }`. **`unsupported` không có nghĩa "chưa test"**: nó phải hoặc (a) kèm một **escape quan sát được** (canary
đã đọc được, nonce đã tới listener, 4 descendant còn sống), hoặc (b) một lý do cấu trúc chỉ ra được (API không tồn tại
trong backend đang pin). Một verdict không có bằng chứng là một bug, và test A36 khẳng định điều đó.

Hệ quả: **không** pin version từ trí nhớ. `os`, `os_version`, `arch`, `backend`, `backend_version` nằm trong matrix;
cache khác identity không được tái sử dụng.

### D3 — `strict` nghĩa là **Full**, và trên host này nó **bị từ chối**

`IsolationMode::Strict` đòi confinement filesystem + network + credential socket + env + containment. Host này không
enforce được ba nhóm đầu (D4). Vì vậy `strict` bị từ chối bằng `StrictIsolationUnavailable` (`retry_class = Never`),
thông điệp **nêu tên** capability thiếu, **không** spawn process nào.

**Không hạ cấp im lặng.** Containment **không** bao giờ được trả lời cho một request `strict`. Nếu tương lai có người
muốn "strict nhưng chỉ containment", đó là một **tên khác** và một quyết định khác — không phải việc của một `bool`.

### D4 — Phân loại từ vựng: containment ≠ confinement

| Từ | Nghĩa trong repo này |
|---|---|
| **containment** | cây process không thoát khỏi job, bị giết trọn khi hết hạn/cancel/host chết; môi trường con bị clear |
| **resource bound** | deadline, quota output, số process bị giới hạn bởi vòng đời job |
| **confinement** | child **không đọc/ghi được** ngoài phạm vi cho phép, **không** nối được ra ngoài, **không** mở được credential socket |

Host runner là **containment**, không phải confinement. Mọi receipt, CLI output, doc và test dùng đúng hai từ này;
`ToolCapabilities.filesystem_network_sandbox` giữ `false`, `strict_isolation` giữ `false`.

### D5 — Kiểm soát thật đến từ **assign-trước-resume**, không từ path normalization

Child của một job member tự động thuộc job (backend pinned **không** set `BREAKAWAY_OK`/`SILENT_BREAKAWAY_OK`), và
`CREATE_SUSPENDED` đóng cửa sổ đua nơi child kịp spawn grandchild **trước** khi bị gán. Đây là lý do một "detached
grandchild" vẫn bị reaped — và nó **được đo** bằng `P-CONT`, không được suy ra từ việc đọc code.

Normalize đường dẫn trong `validate_workspace_action` là **policy**, không phải sandbox. M12 không được phép để bất kỳ
tài liệu nào biến nó thành bằng chứng enforcement.

### D6 — Lease bền + **liveness lock do OS giữ** là thứ phân biệt orphan với owner còn sống

Trước khi một execution được expose, một row `backend_leases` ở trạng thái `acquiring` được ghi bền (handle, pid,
owner generation, lock path). Owner **giữ exclusive advisory lock** (`File::try_lock`) trên file lock của lease trong
suốt vòng đời. Recovery chỉ được dọn khi **cả hai** điều kiện đúng: lease quá grace **và** acquire được lock. Owner
còn sống ⇒ lock không acquire được ⇒ recovery trả `still_owned` và **không** xoá gì.

**Lý do:** sự vắng mặt của một process cục bộ không phải bằng chứng rằng tài nguyên không còn owner — đó là luật của
runbook M12-03, và một lock do OS giữ theo vòng đời process là bằng chứng mà ta kiểm được mà không cần `unsafe`, không
cần PID (PID tái sử dụng được), và không cần đoán.

### D7 — Export trước, cleanup sau; provenance ở bản ghi lease, **không** ở receipt

Artifact (digest, `byte_len`, đường dẫn tương đối) bền **trước** khi handle bị bỏ, để một host chết ở giữa vẫn để lại
bằng chứng đọc được. Provenance (backend, profile, capability `enforced`/`not_claimed`, digest artifact) nằm ở
`backend_leases`. **`tool-execution-receipt.v1` không đổi**: nó là hợp đồng đã pin với schema sinh tự động, và thêm
trường vào đó chỉ để chứa metadata vận hành là mở rộng vô ích. Artifact export từ chối mọi đường dẫn resolve ra ngoài
artifact root.

### D8 — Từ chối dùng mã lỗi **đang có**, không thêm mã mới

`StrictIsolationUnavailable` (exit code theo CONTRACTS §9, `retry_class = Never`) đã đúng nghĩa: đây là một cấu hình
**không thể** phục vụ, không phải một sự cố tạm thời. Thêm mã mới sẽ làm người vận hành phải học hai tên cho một việc.
Điều M12 sửa là **lý do**: từ một câu hard-code thành danh sách capability thiếu, đo được.

### D9 — Điều gì sẽ lật quyết định này

M12 bị lật khi — và chỉ khi — có một backend confinement **thật** và probe của nó **xanh trên host đang chạy**:

1. **AppContainer** (`CreateAppContainerProfile` + `STARTUPINFOEX` + capability SID): confinement filesystem và
   egress thật, nhưng cần `unsafe` Win32 ⇒ phải là một crate **opt-out** khỏi `unsafe_code = "forbid"`, kèm ADR
   riêng, ACL grant cho workspace, và một probe chứng minh con **không** đọc được canary ngoài grant.
2. **WSL / container / VM** (`bubblewrap`, `nsjail`, OCI): chỉ khi host có chúng và có bằng chứng version thật.

Cả hai đều **ngoài quyền của lượt này** (SPEC §12) và ngoài phần cứng/dịch vụ đang có. Cho tới lúc đó, strict profile
**fail closed** là hành vi đúng, không phải hành vi tạm.

## 3. Threat model và ranh giới kiểm soát

| Bề mặt | Mô hình đe dọa | Kiểm soát trên host này | Trạng thái |
|---|---|---|---|
| Process tree | child spawn descendant "detached" để sống sót qua cancel | Job Object, assign-trước-resume, `TerminateJobObject` + `wait` | **enforced** |
| Environment | child đọc credential của host từ env kế thừa | `env_clear` + allowlist + binding đã grant | **enforced** (bởi host, không bởi OS) |
| Filesystem đọc | child đọc secret/SSH key/nguồn ngoài workspace | không có | **unsupported** (escape có bằng chứng) |
| Filesystem ghi | child sửa file ngoài workspace | không có | **unsupported** |
| Network egress | child gửi dữ liệu ra endpoint ngoài | không có | **unsupported** |
| Credential socket | child mở named pipe/agent socket của host | không có | **unsupported** |
| Resource (memory / số process) | child làm cạn RAM/CPU của host | chỉ có deadline + quota output; **không** có cap cấp OS | **unsupported** |
| Control boundary | model tự nới quyền bằng cách yêu cầu `strict` | approval + intent + receipt; `strict` bị từ chối trước khi spawn | **enforced** |
| Ownership | process thứ hai dọn tài nguyên của owner còn sống | lease bền + liveness lock + grace | **enforced** |

**Residual risk được chấp nhận và nói ra:** một tool call chạy trên host này **có** quyền của user đang chạy `ha`:
nó đọc/ghi được mọi thứ user đó đọc/ghi được, và nối được ra mạng. Vì vậy containment **không** biến một hành động
chưa được duyệt thành an toàn; nó chỉ bảo đảm vòng đời và môi trường. Đây là lý do `strict` phải từ chối thay vì
"tạm chấp nhận".

## 4. Phương án bị bác bỏ

- **Map `strict` → containment (hạ cấp "có thông báo").** Người gọi yêu cầu confinement và nhận được thứ khác; một
  tên gọi đắt hơn một refusal.
- **Tự viết `unsafe` Win32 (restricted token/AppContainer) ngay trong `harness-tools`.** Vi phạm `unsafe_code =
  "forbid"` của workspace, và một sandbox tự viết mà chưa đo được là đúng thứ M12 sinh ra để chống.
- **ACL/deny-ACE để "khoá" thư mục ngoài workspace.** Cùng một token ⇒ cha cũng bị chặn; đây là thay đổi phá hủy
  trên máy người dùng, không phải confinement.
- **`runas /trustlevel` (SAFER) như một confinement.** Một trust level hạ quyền **admin**, không giới hạn **đường
  dẫn**: user vẫn đọc/ghi đúng những gì user đọc/ghi được. Nó không trả lời câu hỏi của A36. (Đánh giá từ tài liệu;
  **không** chạy — nó có thể mở hộp thoại tương tác trên máy người dùng.)
- **Coi "process đã biến mất" là đủ để dọn tài nguyên.** PID tái sử dụng được, và một peer hợp lệ có thể đang chạy
  trên host khác với cùng owner generation. Lock do OS giữ là bằng chứng thay thế.
- **Nhồi provenance vào `tool-execution-receipt.v1`.** Mở rộng một schema đã pin cho metadata vận hành, và buộc phải
  regenerate + review một hợp đồng không cần đổi.
- **Đặt `A36 = implemented` sau khi containment xanh.** Hai clause của A36 (file/network/socket canary) không được
  chứng minh trên host này; verdict phải phản ánh điều đó.

## 5. Hệ quả

| Hệ quả | Xử lý |
|---|---|
| Runtime store schema 5 → **6** | Một bảng `backend_leases` additive; DB cũ mở được và upgrade tại chỗ |
| `ToolCapabilities` thêm trường | Additive; **không** bump `TOOL_CONTRACT_VERSION`; `strict_isolation`/`filesystem_network_sandbox` giữ nguyên nghĩa và giá trị |
| `tool-execution-receipt.v1` | **Không đổi** |
| CLI | `ha sandbox capabilities\|probe\|leases` — surface của matrix và lease; `ha` vẫn là binary duy nhất |
| A36 | `planned` + note nêu clause thiếu; không claim accepted |
| Nền tảng | Bằng chứng M12 là **Windows 10 Pro 19045**. Không có bằng chứng Linux nào tồn tại (không WSL/Docker; CI chưa chạy lần nào) |
