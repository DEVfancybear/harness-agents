# Rà soát kiến trúc — source hiện tại

[English](ARCHITECTURE_REVIEW.en.md) | Tiếng Việt

**Ngày review:** 29/09/2026. **Phạm vi:** đối chiếu tài liệu kiến trúc với Rust workspace hiện tại. Đây là review boundary và evidence, không phải quyền tuyên bố capability chưa được hỗ trợ.

## 1. Kết luận

### A. Source tree là một local host nhiều tầng

Workspace có mười một crate: shared types, kernel composition, SQLite store, session, providers, runtime, tools, orchestrator, extensions, maintenance và CLI. CLI compose các service; nó không sở hữu mọi domain.

### B. Durable evidence là invariant trung tâm

Input admission, run transition, tool intent, policy decision, provider attempt, task settlement và receipt đều đi qua durable boundary dựa trên SQLite. Provider text là proposal. Recovery phải phân biệt result đã commit với external effect chưa chắc chắn và không được âm thầm replay trường hợp sau.

### C. Extension boundary là contract thật, không phải claim sandbox

Kernel registration/scope/lease và extension handshake/capability negotiation đã là contract trong source. Native same-process code vẫn là trusted code. Stdio, child process và loopback transport chỉ giới hạn giao tiếp; tự chúng không chứng minh hostile-code isolation.

### D. Delegation là task system do host sở hữu

Orchestrator lưu task ownership, dependency, worker budget, handoff và delivery. Tên role hay skill không cấp tool authority. Worker proposal vẫn đi qua host policy và receipt path thông thường.

## 2. Invariant bắt buộc

- Mỗi thời điểm một writable host sở hữu một data directory.
- Required service composition fail closed trước input admission.
- Durable ACK trả sau authoritative write.
- Mutating tool cần canonicalization, policy và approval khi cấu hình yêu cầu.
- Context, output, process capture, extension frame và delegation đều có giới hạn.
- UI và web adapter gọi application service, không trở thành database writer.
- Recovery có thể inspect hoặc từ chối resume không an toàn mà không cần model extractor.

## 3. Khoảng trống và giới hạn hiện tại

- Windows 10/11 x64 là platform được hỗ trợ; Linux pending, macOS chưa test.
- Sản phẩm là binary local foreground; không claim daemon sau khi thoát hay remote-worker service.
- Native extension code là trusted và không phải OS sandbox.
- Provider authentication và paid API behavior cần evidence riêng theo provider.
- Phase document lịch sử không phải acceptance evidence hiện tại nếu revision không trùng source đang kiểm tra.

## 4. Kiểm chứng bắt buộc khi sửa

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest
```

Ghi source revision, platform, command và test count liên quan. Docs checker xanh chỉ kiểm tra link/cấu trúc; không chứng minh runtime behavior.

## 5. Quyết định

Giữ crate boundary và durable-authority model hiện tại. Cập nhật plan/runbook để trỏ vào mười một crate hiện có. Không đưa domain owner đã xóa trở lại chỉ bằng phase document, facade hoặc compatibility table.
