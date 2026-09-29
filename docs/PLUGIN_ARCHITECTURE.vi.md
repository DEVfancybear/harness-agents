# Kiến trúc plugin và extension

[English](PLUGIN_ARCHITECTURE.en.md) | Tiếng Việt

**Trạng thái:** contract kernel/extension hiện tại, ngày 29/09/2026. `harness-kernel` và `harness-extensions` là crate đã có trong workspace; tài liệu này mô tả boundary thật, không phải SDK được đề xuất.

## 1. Ownership

`harness-kernel` sở hữu composition trong process. Nó validate service requirement, đăng ký value theo scope, theo dõi generation và lease, rồi shutdown resource theo thứ tự dependency. `harness-extensions` sở hữu negotiated extension session, discovery/call/task theo MCP, composition skill và bounded transport. `harness-cli` compose các service này nhưng không trở thành authority thứ hai.

```text
harness-cli
  -> harness-extensions -> harness-kernel -> harness-types
  -> harness-tools     -> harness-store-sqlite
  -> harness-runtime   -> harness-session
```

Host vẫn là authority cho input admission, tool policy, approval, workspace scope, budget và receipt. Extension có thể đề xuất tool hoặc resource operation; nó không thể tự cấp host authority cho mình.

## 2. Kernel contract

Một service requirement có tên ổn định, contract generation và mode required/optional. Composition phải từ chối:

- thiếu required service;
- contract generation không tương thích;
- duplicate registration trong cùng scope;
- cycle trong requirement graph;
- generation cũ cố unregister replacement.

Scope cung cấp nearest-registration lookup và cô lập sibling. Plugin nhận owned handle cho resource nó mount. Unmount cancel task do nó sở hữu, chờ disposer theo thứ tự dependency ngược và báo lỗi mà không âm thầm bỏ qua cleanup còn lại. Store đóng cuối cùng.

Kernel composition không phải OS sandbox. Native code cùng process vẫn là trusted code; subprocess hay stdio transport chỉ là process/transport boundary.

## 3. Handshake extension

Mọi extension session đi theo thứ tự:

```text
spawn/connect
  -> negotiate protocol và version
  -> negotiate capability và limit
  -> validate schema
  -> discovery (tool/resource/prompt/task)
  -> invocation có giới hạn
  -> settle result/error/cancellation
  -> release lease và shutdown
```

Host ghi lại protocol generation, extension identity, capability và limit đã negotiate trước khi expose operation. Critical capability không biết, argument malformed, frame quá lớn, ID sai và protocol version không hỗ trợ đều fail closed.

Adapter giới hạn frame size, discovery page, call argument, result capture, task lifetime và cancellation grace. Timeout hoặc transport hỏng tạo failure/uncertain outcome rõ ràng; không tự replay mutating call.

## 4. Mô hình capability

| Capability | Host bắt buộc | Trách nhiệm extension |
|---|---|---|
| Tool | Host policy và approval trước execution | Khai báo input schema và trả result/error có giới hạn |
| Resource | Host scope và read policy | Trả URI/payload ổn định trong limit đã negotiate |
| Prompt/template | Host chọn context và provider budget | Khai báo biến và render content xác định được |
| Task | Host sở hữu lifecycle, cancellation và receipt | Báo progress/result, không tự nhận host đã accept |
| Skill | Host pin source/version và áp skill policy | Cung cấp instruction/resource có giới hạn; không có authority ngầm |
| Transport | Host chọn transport local/loopback được phép | Tuân thủ frame, timeout và cancellation limit |

Role, skill name, server name hay tool name không phải approval. Nested call đi lại qua cùng host gate và giữ scope/correlation ID ban đầu.

## 5. Lifecycle và xử lý lỗi

Mount là transactional: validate requirement, cấp resource, đăng ký handle rồi mới expose plugin. Nếu bước sau lỗi, rollback chỉ gỡ generation của mount đó và cancel task do nó sở hữu. Unmount idempotent và quan sát được.

Trong provider/tool call đang chạy, việc gỡ required service phải dừng admission mới, drain dependent và persist success, failure hoặc uncertainty trước khi đóng storage. Plugin không được giữ task ẩn sau khi lease hết hạn.

Extension result chỉ là evidence khi host đã persist command, policy decision và receipt tương ứng. Presentation có thể truncate output nhưng không được viết lại durable record.

## 6. Cấu hình và vận hành cục bộ

Configuration được resolve trước composition. CLI có thể mount local extension process và loopback integration; release matrix hiện tại là nguồn chuẩn cho platform support. Remote service, native-code isolation và OS sandbox cần evidence riêng, không tự suy ra từ contract này.

Các kiểm tra hữu ích:

```powershell
cargo test -p harness-kernel --locked
cargo test -p harness-extensions --locked
cargo test -p harness-cli --test phase_p6 --locked
pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest
```

## 7. Checklist review

- Required service đã validate trước input admission chưa?
- Mỗi registration có scope và generation chưa?
- Duplicate, stale-generation và cycle có bị từ chối không?
- Mọi extension call có input, output, timeout và cancellation limit không?
- Nested tool call có quay lại policy thay vì thừa hưởng grant chưa kiểm tra không?
- Shutdown order và uncertain outcome có được ghi lại không?
- Mọi claim isolation có capability result theo platform hỗ trợ không?
