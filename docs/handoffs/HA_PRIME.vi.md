# Handoff HA_PRIME

[Plan](../HA_PRIME_PLAN.vi.md) · [Prompt](../HA_PRIME_PROMPT.vi.md) · [SPEC](../specs/HA_PRIME.vi.md) · [Evidence](../evidence/HA_PRIME.vi.md)

## Trạng thái

- **CP-1 (Q00–Q04): xong**, verified local ngày 28/09/2026 (xem evidence). Agent con thuộc session, sống quá lượt, báo kết quả bằng thông báo prime, nhắn tin hai chiều, chọn được model; `/agents stop`.
- **CP-2 (Q05–Q09): xong** ngày 28/09/2026 — hàng đợi 2 lane + `/queue`, Ctrl-S/`/stash`, `/btw`, `/fork`/`/clone`/`/tree` (task mới + `forked_from`), `/export x.html`.
- **CP-3 (Q10–Q13): chưa bắt đầu.**

## Điều người làm checkpoint sau cần biết

- Store giờ là `SharedStore` của session (`interactive/store_lease.rs`). **Không** mở `SqliteStore::open_writer` riêng trong code mới của app tương tác; lấy `agents.store().lease().await` (hoặc truyền store của lượt).
- Controller có hai hàng chờ lượt tự động: `pending_notices` (thông báo/tin của agent con) và `pending_heartbeats`. Q05 thay `queued_input` bằng hàng đợi 2 lane — giữ thứ tự sau `RunTerminal`: input người dùng đang chờ → thông báo agent con → heartbeat → compact → continuation/goal, và giữ cờ `notices_held` (sau Ctrl+C thông báo chờ lượt tiếp theo của người dùng).
- Q08 (fork/tree): đầu mỗi lượt `run_turn` gọi `store.release_task_lease(task_id)`; nếu fork tạo task mới thì không ảnh hưởng, nhưng bước 0 của Q08 vẫn phải đo nguồn khác task như plan.
- Test PTY mới dùng `ScriptedSse` + `Reply` + `from_parent`/`request_text`/`tool_results` + `scripted_session` ở cuối `tests/interactive_terminal.rs`.
- Chưa làm (ghi trong SPEC): cap 20 tin chờ mỗi đích.

## Exact next action

Bắt đầu Q10 theo plan: `interactive/custom_models.rs` đọc `<config dir>/models.json` (schema prime), test `q10_custom_model_is_added_with_defaults` trước, rồi gộp vào `Catalog::load` trong `providers.rs`. Trước khi đóng track: chạy một lượt toàn bộ workspace + PTY đầy đủ (người dùng đã dặn).
