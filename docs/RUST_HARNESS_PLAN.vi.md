# Kế hoạch triển khai Harness Agents bằng Rust

[English](RUST_HARNESS_PLAN.en.md) | Tiếng Việt

**Trạng thái:** bản đồ implementation hiện tại, ngày 29/09/2026. Workspace đã có runtime và các service boundary được mô tả ở đây. Mọi việc còn lại phải kiểm tra theo source hiện tại; tài liệu này không còn là lời hứa trước implementation.

Đọc cùng [tổng quan kiến trúc](ARCHITECTURE_OVERVIEW.vi.md), [sổ tay triển khai](implementation/README.vi.md), [kiến trúc plugin](PLUGIN_ARCHITECTURE.vi.md) và [architecture review](ARCHITECTURE_REVIEW.vi.md).

## 1. Boundary sản phẩm

`ha` là coding-agent host local, foreground. Nó nhận command terminal/headless/web, lưu session và run record trong SQLite, gọi provider được chọn và đưa coding action được đề xuất qua policy, approval và receipt gate. Host có thể giao task có giới hạn và mount local extension đã negotiate. Nó không tuyên bố daemon chạy sau khi thoát, remote-worker durability hay OS sandbox chỉ vì có transport boundary.

## 2. Đồ thị crate hiện tại

```text
harness-types
  ├── harness-kernel
  ├── harness-store-sqlite -> harness-session
  ├── harness-providers
  ├── harness-runtime
  ├── harness-tools
  ├── harness-orchestrator
  ├── harness-extensions
  └── harness-maintenance
                         └── harness-cli compose các service
```

| Crate | Trách nhiệm implementation |
|---|---|
| `harness-types` | ID, schema, contract, error và vocabulary serialization |
| `harness-kernel` | Service requirement, scope, generation, lease và shutdown |
| `harness-store-sqlite` | Mở/fencing SQLite, migration, transaction, queue và artifact |
| `harness-session` | Input admission, journal/projection, snapshot và recovery view |
| `harness-providers` | Provider request/response, mock và streaming adapter |
| `harness-runtime` | Run state machine, context admission, budget và provider attempt |
| `harness-tools` | Canonicalization, policy, approval, process/filesystem/Git execution và receipt |
| `harness-orchestrator` | Task DAG, worker, ownership, handoff, budget và workspace |
| `harness-extensions` | Handshake, capability negotiation, MCP-style operation và transport bound |
| `harness-maintenance` | Backup/restore, compatibility, migration, retention và diagnostics |
| `harness-cli` | Composition, terminal UI, headless command và loopback web adapter |

## 3. Durable flow

```text
CLI input
  -> atomic session admission
  -> committed event/sequence ACK
  -> context có giới hạn và provider request đã freeze
  -> provider stream/attempt record
  -> answer hoặc tool action được đề xuất
  -> policy + approval + durable intent
  -> execution receipt/artifact reference
  -> projection, UI và recovery view
```

Provider response là proposal. Durable ACK chỉ trả sau authoritative write. External outcome chưa chắc chắn được giữ rõ ràng và mặc định không replay.

## 4. Recovery và authority

Store là durable authority, nhưng mỗi domain có một owner:

- session sở hữu input admission và projection;
- runtime sở hữu run lifecycle, budget và provider attempt;
- tools sở hữu policy decision, intent và receipt;
- orchestrator sở hữu task settlement và delivery;
- extensions sở hữu negotiated session và lease;
- maintenance sở hữu backup, migration, retention và support diagnostics;
- CLI sở hữu presentation và composition, không mutation domain trực tiếp.

Recovery fold snapshot đã commit và journal tail thành view xác định được. Nó có thể từ chối resume không an toàn khi event/projector version không biết, lease cũ, workspace đã đổi hoặc external effect chưa chắc chắn.

## 5. Giới hạn bảo mật và platform

Implementation phải giữ rõ các boundary sau:

- required service composition fail closed;
- tool policy và approval đi trước mutating execution;
- output, context, process capture, extension frame và delegation đều có giới hạn;
- mỗi thời điểm một writable host sở hữu một data directory;
- native extension cùng process là trusted code;
- subprocess/stdio không tự động là OS sandbox;
- Windows 10/11 x64 được hỗ trợ, Linux pending, macOS chưa test.

## 6. Trình tự công việc

| Trình tự | Phạm vi | Source anchor |
|---|---|---|
| P0 | ID, foundation và fixture | `harness-types`, CLI fixture |
| P1 | kernel và durable storage | `harness-kernel`, `harness-store-sqlite` |
| P2 | runtime/session/recovery | `harness-runtime`, `harness-session` |
| P3 | coding tool và receipt | `harness-tools` |
| P5 | delegation và workspace | `harness-orchestrator` |
| P6 | extension và MCP | `harness-extensions` |
| P7 | maintenance và release evidence | `harness-maintenance` |
| P8 | loopback Web reuse tùy chọn | `harness-cli` |

Không có active phase cho service đã xóa là có chủ ý. Việc mới phải thêm source owner, test, runbook song ngữ và manifest trong cùng một thay đổi.

## 7. Kiểm chứng

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest
```

Focused crate check hữu ích khi phát triển, nhưng release claim cần full workspace command cùng platform, revision và artifact evidence.

## 8. Ngoài phạm vi

Kế hoạch này không thêm hosted service, background daemon, remote worker fleet, database authority thứ hai, provider side effect không kiểm soát hoặc claim platform chưa được hỗ trợ. Nó cũng không biến phase document lịch sử thành acceptance evidence hiện tại.
