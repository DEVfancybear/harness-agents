# Sổ tay triển khai

[English](README.en.md) | Tiếng Việt

**Trạng thái:** bản đồ implementation hiện tại, ngày 29/09/2026. Sổ tay bám theo source tree và các gate thực thi. Nó không hứa mọi mục từng được plan đều đã hoàn tất.

## 1. Nguồn chuẩn

Dùng [tổng quan kiến trúc](../ARCHITECTURE_OVERVIEW.vi.md) cho các boundary hiện tại, [kiến trúc plugin](../PLUGIN_ARCHITECTURE.vi.md) cho contract kernel/extension, và source/tests cho hành vi thật. Các runbook phase bên dưới mô tả ownership và verification; chúng không tạo ra capability không có trong workspace.

Khi runbook mâu thuẫn với Rust code, `Cargo.toml` hoặc test, hãy ghi nhận mâu thuẫn và theo source hiện tại. Evidence lịch sử gắn với revision, không được dùng lại như release claim mới.

## 2. Workspace hiện tại

Workspace có mười một crate Rust:

- `harness-types` — ID, contract, error và schema dùng chung;
- `harness-kernel` — composition, scoped registration, lease và shutdown;
- `harness-store-sqlite` — SQLite authority, migration và durable record;
- `harness-session` — admission, projection, snapshot và recovery view;
- `harness-providers` — provider adapter, mock provider và streaming;
- `harness-runtime` — run lifecycle, budget và context admission;
- `harness-tools` — policy, approval, filesystem/Git/process tool và receipt;
- `harness-orchestrator` — delegated task, worker, DAG và workspace;
- `harness-extensions` — MCP/extension negotiation và bounded transport;
- `harness-maintenance` — backup, migration, retention và diagnostics;
- `harness-cli` — composition của `ha`, terminal UI, headless mode và loopback web.

## 3. Bản đồ phase

Chuỗi active bám theo executable test target và boundary crate hiện có; capability đã xóa không phải implementation dependency.

| Phase | Phạm vi | Phụ thuộc | Ước tính | Source anchor hiện tại |
|---|---|---|---:|---|
| P0 | Foundation, ID và test fixture | — | 3–4 | `harness-types`, CLI fixture |
| P1 | Kernel, storage ownership và durable record | P0 | 8–11 | `harness-kernel`, `harness-store-sqlite` |
| P2 | Runtime, context admission và recovery | P1 | 7–10 | `harness-runtime`, `harness-session` |
| P3 | Coding tool, policy và receipt | P2 | 7–10 | `harness-tools` |
| P5 | Delegation, task DAG và isolated workspace | P3 | 8–12 | `harness-orchestrator` |
| P6 | Extension, skill và MCP | P5 | 5–8 | `harness-extensions` |
| P7 | Backup, migration, release và support evidence | P6 | 8–11 | `harness-maintenance`, CLI release checks |
| P8 | Loopback Web adapter | P7 | 10–15 | web surface tùy chọn của `harness-cli` |

Khoảng planning P0–P7: **46–66 ngày công**. P8 vẫn tùy chọn. Việc không có một phase row là có chủ ý: subsystem đã xóa không được tiếp tục tồn tại như dependency implementation giả.

## 4. Quy tắc thực thi

1. Đọc source hiện tại và evidence tiền nhiệm trước khi sửa phase.
2. Mỗi domain chỉ có một durable authority. Không thêm SQLite writer thứ hai qua facade.
3. Coi provider output là proposal; chỉ host policy và receipt tạo execution evidence.
4. Giữ invariant về sequence, fencing, budget, workspace và recovery khi thêm UI hoặc extension path.
5. Dừng ở ranh giới phase. Không báo acceptance từ một test filter rỗng hoặc không chạy test.

## 5. Lệnh kiểm chứng

Chạy từ repository root:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest
```

Với phase có executable target, chạy focused gate rồi chạy workspace gate đầy đủ. Kết quả chỉ gắn với source khi command, revision, platform và count được ghi cùng nhau.

## 6. Evidence và recovery

Git revision hiện tại và command output là nguồn kiểm chứng. Recovery phải dùng store record, snapshot và receipt đã commit; prose không thay thế execution record.

## 7. Thêm phase hoặc crate

Trước khi thêm phase, cập nhật source map, dependency graph, runbook song ngữ, manifest, acceptance ownership và docs verifier trong cùng một thay đổi. Trước khi thêm crate, nêu rõ authority, input, output và shutdown behavior. Crate chỉ forward call không được trở thành domain owner mới.

## 8. Checklist review

- Thay đổi có khớp crate map hiện tại không?
- Mọi side effect đã qua policy và approval khi cần chưa?
- Durable ACK chỉ trả sau authoritative write chưa?
- External outcome chưa chắc chắn có hiển thị và mặc định không replay chưa?
- Claim Windows có tách khỏi evidence Linux/macOS đang pending không?
- Heading, link và command example tiếng Anh/Việt có đồng bộ không?

## 9. Tài liệu liên quan

- [Tổng quan kiến trúc](../ARCHITECTURE_OVERVIEW.vi.md)
- [Architecture review](../ARCHITECTURE_REVIEW.vi.md)
- [Kiến trúc plugin](../PLUGIN_ARCHITECTURE.vi.md)
- [Acceptance map](ACCEPTANCE_MAP.vi.md)
- [Operator guide](../OPERATOR_GUIDE.vi.md)
- [Build và release](../BUILD_AND_RELEASE.md)
