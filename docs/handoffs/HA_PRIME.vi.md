# Handoff HA_PRIME

[Plan](../HA_PRIME_PLAN.vi.md) · [Prompt](../HA_PRIME_PROMPT.vi.md) · [SPEC](../specs/HA_PRIME.vi.md) · [Evidence](../evidence/HA_PRIME.vi.md)

## Trạng thái

- **CP-1 (Q00–Q04): xong**, verified local ngày 28/09/2026 (xem evidence). Agent con thuộc session, sống quá lượt, báo kết quả bằng thông báo prime, nhắn tin hai chiều, chọn được model; `/agents stop`.
- **CP-2 (Q05–Q09): xong** ngày 28/09/2026 — hàng đợi 2 lane + `/queue`, Ctrl-S/`/stash`, `/btw`, `/fork`/`/clone`/`/tree` (task mới + `forked_from`), `/export x.html`.
- **CP-3 (Q10–Q13): xong** ngày 28/09/2026 — `models.json` (apiKey = tên biến môi trường), `[routing] scoped` + `/scoped-models` + `/model next|prev` + Alt+M, model phụ/dự phòng/ảnh (`routing::Router` trong `LiveProvider`, `AuxiliarySummary`), `ErrorCode::RateLimited` + chờ hạn mức.
- **CP-4 (Q14–Q15): xong** ngày 28/09/2026 — `/autonomous` (port `autonomous.ts`, cổng kiểm tra chạy thật, snapshot git), `/schedule` bền theo hội thoại (port parser + cron của `cron-jobs.ts`). Track Q00–Q15 xong.
- **Lượt chạy toàn bộ:** workspace 958 passed / 2 đỏ chập chờn đã biết; PTY đầy đủ 39/39 sau khi sửa `i12` (lỗi gửi request nêu host:port) và 3 test PTY chập chờn. Sửa kèm: tin tới sau bước cuối bị mất (Q03), `/scoped-models` chỉ nhận mẫu đầu (Q11).
- **Quyết định của người dùng (28/09/2026):** agent con **giữ quyền như hiện tại** - `explorer` chỉ đọc (không shell, không ghi), chỉ `coder` có shell trong worktree riêng - để dễ kiểm soát. Đây là khác biệt có chủ đích với prime (`rlm.spawn` của prime tạo con đầy đủ tool); không đề xuất lại.

## Điều người làm checkpoint sau cần biết

- Store giờ là `SharedStore` của session (`interactive/store_lease.rs`). **Không** mở `SqliteStore::open_writer` riêng trong code mới của app tương tác; lấy `agents.store().lease().await` (hoặc truyền store của lượt).
- Controller có hai hàng chờ lượt tự động: `pending_notices` (thông báo/tin của agent con) và `pending_heartbeats`. Q05 thay `queued_input` bằng hàng đợi 2 lane — giữ thứ tự sau `RunTerminal`: input người dùng đang chờ → thông báo agent con → heartbeat → compact → continuation/goal, và giữ cờ `notices_held` (sau Ctrl+C thông báo chờ lượt tiếp theo của người dùng).
- Q08 (fork/tree): đầu mỗi lượt `run_turn` gọi `store.release_task_lease(task_id)`; nếu fork tạo task mới thì không ảnh hưởng, nhưng bước 0 của Q08 vẫn phải đo nguồn khác task như plan.
- Test PTY mới dùng `ScriptedSse` + `Reply` + `from_parent`/`request_text`/`tool_results` + `scripted_session` ở cuối `tests/interactive_terminal.rs`.
- Chưa làm (ghi trong SPEC): cap 20 tin chờ mỗi đích.
- Q12/Q13 sống trong `interactive/routing.rs` (`Router`, `UsageWait`, `AuxiliarySummary`); `LiveProvider::router_for` dựng nó mỗi lượt từ `ProviderConfig.routing`. Muốn đổi model theo `provider/id` thì dùng `routing::resolve` (catalog + credential), đừng tự dựng `ProviderConfig`.
- Config dùng bảng `[routing]` (không phải `[models]`, bảng đó đã là context window theo model).

## Exact next action

Track HA_PRIME xong, PTY đầy đủ 39/39 xanh. Việc tiếp theo chỉ khi được giao: các mục ngoài phạm vi ở plan mục 11. Người dùng cần cài lại `ha` (`scripts/Install-Ha.ps1`) - chuỗi mã lỗi lặp họ thấy là của bản cũ.
