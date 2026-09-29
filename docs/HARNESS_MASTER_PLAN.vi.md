# Harness Agents — kế hoạch sản phẩm và kiến trúc hiện tại

[English](HARNESS_MASTER_PLAN.en.md) | Tiếng Việt

**Trạng thái:** kế hoạch theo source hiện tại, ngày 29/09/2026. Sản phẩm không còn là scaffold rỗng: workspace đã có runtime, durable store, tools, delegation, extensions, maintenance và CLI được mô tả bên dưới.

## 1. Kết quả sản phẩm

Cung cấp coding-agent host local foreground với session durable, provider run có giới hạn, coding tool qua policy gate, task delegation có recovery, local extension đã negotiate và diagnostics nhìn được cho operator. Host phải ưu tiên evidence inspect được thay vì model narration lạc quan.

## 2. Kiến trúc hiện tại

```text
operator -> harness-cli
              ├── harness-runtime -> harness-session -> harness-store-sqlite
              ├── harness-providers
              ├── harness-tools -> filesystem / Git / process adapters
              ├── harness-orchestrator
              ├── harness-extensions -> harness-kernel
              └── harness-maintenance
                         tất cả dùng contract harness-types
```

SQLite là durable authority. CLI, UI và web adapter compose application service, không trở thành domain writer thay thế. Mỗi thời điểm một writable host sở hữu một data directory.

## 3. Chuỗi capability

| Trình tự | Capability | Owner hiện tại |
|---|---|---|
| P0 | Identity, contract và fixture | `harness-types`, CLI tests |
| P1 | Composition, storage và fencing | `harness-kernel`, `harness-store-sqlite` |
| P2 | Session, runtime, budget và recovery | `harness-session`, `harness-runtime` |
| P3 | Coding tool, policy và receipt | `harness-tools` |
| P5 | Delegated task, worker và workspace | `harness-orchestrator` |
| P6 | Extension/MCP negotiation và bounded transport | `harness-extensions` |
| P7 | Backup, migration, retention và release check | `harness-maintenance` |
| P8 | Authenticated loopback Web adapter tùy chọn | `harness-cli` |

Chuỗi cố ý không có row cho subsystem đã xóa. Role, skill hay provider không phải authority boundary mới.

## 4. Invariant

- raw input được admit durable trước ACK;
- provider request được freeze và gắn budget;
- tool action được canonicalize, policy-check và ghi receipt;
- external outcome chưa chắc chắn được hiển thị, không âm thầm replay;
- task ownership và child delivery sống qua parent pause/restart;
- extension call có version, schema validation và bound;
- backup/migration công bố manifest và compatibility evidence;
- claim platform hoặc isolation chưa hỗ trợ phải fail closed.

## 5. Các mode hướng người dùng

- terminal/TUI hoặc line mode tương tác;
- headless execution cho script và automation;
- authenticated loopback Web projection trên cùng application service;
- maintenance command cho diagnostics, backup và migration.

Mọi mode dùng cùng admission, policy, persistence và recovery rules.

## 6. Quy tắc delivery

Sửa trong crate sở hữu, thêm focused test, cập nhật runbook song ngữ rồi chạy workspace gate. Không hồi sinh phase đã xóa bằng manifest row hoặc link cũ. Evidence lịch sử vẫn gắn với source revision của nó.

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest
```

## 7. Tài liệu liên quan

- [Tổng quan kiến trúc](ARCHITECTURE_OVERVIEW.vi.md)
- [Architecture review](ARCHITECTURE_REVIEW.vi.md)
- [Kế hoạch Rust](RUST_HARNESS_PLAN.vi.md)
- [Kiến trúc plugin](PLUGIN_ARCHITECTURE.vi.md)
- [Sổ tay triển khai](implementation/README.vi.md)
- [Operator guide](OPERATOR_GUIDE.vi.md)
