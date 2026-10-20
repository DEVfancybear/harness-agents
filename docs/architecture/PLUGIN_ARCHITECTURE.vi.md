# Kiến trúc plugin và extension

[English](PLUGIN_ARCHITECTURE.en.md) | Tiếng Việt

**Trạng thái:** contract kernel/extension hiện tại, ngày 07/10/2026. `harness-kernel` và `harness-extensions` là crate đã có trong workspace; tài liệu này mô tả boundary thật, không phải SDK được đề xuất.

## 1. Ownership

`harness-kernel` sở hữu composition trong process. Nó validate service requirement, đăng ký value theo scope, theo dõi generation và lease, rồi shutdown resource theo thứ tự dependency. `harness-extensions` sở hữu negotiated extension session, discovery/call/task theo MCP, composition skill và bounded transport. `harness-cli` compose các service này và sở hữu các bề mặt người dùng xây trên chúng (`ha mcp`, `ha package`, `ha extensions`, `/plugins`, nạp skill và prompt template) nhưng không trở thành authority thứ hai.

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

Bảng bao quát trách nhiệm qua nhiều protocol, không phải một manifest enum duy nhất. Local manifest khai báo `tools`, `model_provider`, `memory_extractor` hoặc `skills`; MCP negotiate riêng tools, resources, prompts và các tính năng protocol khác. Tên capability tự nó không kích hoạt implementation hay cấp quyền.

| Capability | Host bắt buộc | Trách nhiệm extension |
|---|---|---|
| Tool | Host policy và approval trước execution | Khai báo input schema và trả result/error có giới hạn |
| Resource | Host scope và read policy | Trả URI/payload ổn định trong limit đã negotiate |
| Prompt/template | Host chọn context và provider budget | Khai báo biến và render content xác định được |
| Task | Host sở hữu lifecycle, cancellation và receipt | Báo progress/result, không tự nhận host đã accept |
| Skill | Host pin source/version và áp skill policy | Cung cấp instruction/resource có giới hạn; không có authority ngầm |
| Transport | Local extension dùng stdio; MCP dùng stdio hoặc Streamable HTTP; dùng TLS từ xa và dành cleartext cho fixture cục bộ | Tuân thủ frame, timeout và cancellation limit; transport không phải sandbox |

Role, skill name, server name hay tool name không phải approval. Nested call đi lại qua cùng host gate và giữ scope/correlation ID ban đầu.

## 5. Lifecycle và xử lý lỗi

Mount là transactional: validate requirement, cấp resource, đăng ký handle rồi mới expose plugin. Nếu bước sau lỗi, rollback chỉ gỡ generation của mount đó và cancel task do nó sở hữu. Unmount idempotent và quan sát được.

Trong provider/tool call đang chạy, việc gỡ required service phải dừng admission mới, drain dependent và persist success, failure hoặc uncertainty trước khi đóng storage. Plugin không được giữ task ẩn sau khi lease hết hạn.

Extension result chỉ là evidence khi host đã persist command, policy decision và receipt tương ứng. Presentation có thể truncate output nhưng không được viết lại durable record.

## 6. Cấu hình và vận hành cục bộ

Configuration được resolve trước composition. CLI có thể mount local extension process (NDJSON qua stdio) và MCP server qua stdio hoặc Streamable HTTP. Dùng TLS cho endpoint MCP từ xa và dành HTTP cleartext cho fixture cục bộ; kiểm tra tiền tố URL của native không phải ranh giới xác thực hostname hay cô lập mạng. MCP đọc bearer credential từ environment hoặc từ OAuth sign-in (xem operator guide). Release matrix hiện tại là nguồn chuẩn cho platform support. Native-code isolation và OS sandbox cần evidence riêng, không tự suy ra từ contract này.

## 7. Composition của CLI host

Native host của CLI như memory, knowledge, task state và web dùng seam catalog/dispatcher `ExternalTools`; chúng không tự động là subprocess extension hay MCP server. Mọi tool phơi ra vẫn đi qua policy và approval của host.

Skill root gồm skill đóng gói, skill user, `~/.agents/skills`, `HA_SKILL_PATHS` và root project chỉ sau khi trust project. Mức trust của nguồn không cấp authority. Prompt contribution bị giới hạn 256 file, mỗi file 256 KiB. Xem [operator guide](../guides/OPERATOR_GUIDE.vi.md) cho MCP login, skill, package, template và hook; xem [build và release](../guides/BUILD_AND_RELEASE.md) cho các kiểm tra.
