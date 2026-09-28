# Evidence HA_PRIME — CP-1 (Q00–Q04)

[SPEC](../specs/HA_PRIME.vi.md) · [Handoff](../handoffs/HA_PRIME.vi.md) · [Plan](../HA_PRIME_PLAN.vi.md)

- **Work items:** Q00–Q04.
- **Trạng thái:** `implemented_partially_verified` — code xong, test mục tiêu xanh; **toàn bộ test workspace và 25 ca PTY cũ chưa chạy lại trên bản cuối** (phiên bị ngắt giữa lần chạy; người dùng chọn commit trước và chạy toàn bộ một lượt sau khi xong các checkpoint).
- **Base:** `48676b6`; Windows 11, rust-toolchain của repo; ngày 28/09/2026.

## Đã chạy

| Lệnh | Kết quả |
|---|---|
| `cargo test -p harness-cli --bin ha delegation` | 20 passed, 0 failed |
| `cargo test -p harness-cli --bin ha q0` | 16 passed, 0 failed (controller + delegation + store lease) |
| `cargo test -p harness-cli --bin ha` (trước bản sửa lease và clippy) | 443 passed, 2 failed — đúng hai test chập chờn đã biết (`a_dead_owners_journal_is_reaped_and_removed`, `g09_hook_cannot_turn_ask_into_allow`) |
| `scripts/Invoke-HaPtyAcceptance.ps1 -Filter q0` (lần 1) | 3 passed, **1 failed**: `q03_pty_child_reports_to_its_parent` — lượt được đánh thức lỗi `task_lease_conflict` (negative control tự nhiên cho bản sửa lease) |
| `scripts/Invoke-HaPtyAcceptance.ps1 -Filter q0` (sau `release_task_lease`) | 4 passed, `PTY_EXIT: 0` |
| `cargo clippy --workspace --all-targets -- -D warnings` | sạch |
| `cargo run -p harness-types --bin generate_schemas --locked` | chỉ `harness-config.v2.schema.json` đổi (thêm `agents`) |

## Chưa chạy (not_run)

- `cargo test --workspace --locked --no-fail-fast` trên bản cuối (có `p0_f03` kiểm schema).
- PTY đầy đủ 25 ca cũ + 4 ca q0 trong một lần (`-TimeoutSeconds 900`).
- Live smoke với provider thật.


# Evidence CP-2 (Q05–Q09)

- **Trạng thái:** `implemented_partially_verified` — như CP-1, toàn bộ test workspace và 25 ca PTY cũ để chạy một lượt khi xong các checkpoint (theo người dùng).
- **Base:** `93bc8d4`; Windows 11; 28/09/2026.

| Lệnh | Kết quả |
|---|---|
| `cargo test -p harness-cli --bin ha -- q05 q06 g06` | 18 passed |
| `cargo test -p harness-cli --bin ha -- q07` / `q08` / `q09` | 2 / 2 / 2 passed |
| `cargo test -p harness-cli --bin ha` | 456 passed, 2 failed → sau khi thêm lệnh vào thẻ `/help` chỉ còn `g09_hook_cannot_turn_ask_into_allow` (chập chờn đã biết) |
| `Invoke-HaPtyAcceptance.ps1 -Filter q0` | lần 1: q07, q08 đỏ vì kịch bản test (chữ có dấu cách bị vẽ bằng lệnh dời con trỏ; kịch bản fork không phân biệt được nhánh) → sửa kịch bản; q00–q07 xanh; `q08_pty_fork_then_answer` xanh riêng (`PTY_EXIT: 0`): nhánh thấy lượt 1, không thấy lượt 3 |
| `cargo clippy --workspace --all-targets -- -D warnings` | sạch |
| `generate_schemas` | `harness-config.v2.schema.json` thêm `queue` |

**not_run:** toàn bộ workspace, 25 ca PTY cũ, live smoke.
