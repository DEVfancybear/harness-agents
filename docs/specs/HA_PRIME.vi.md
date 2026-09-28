# SPEC HA_PRIME — CP-1 (Q00–Q04): agent con chạy nền

[Plan Q01–Q15](../HA_PRIME_PLAN.vi.md) · [Evidence](../evidence/HA_PRIME.vi.md) · [Handoff](../handoffs/HA_PRIME.vi.md)

Assignment: CP-1 (Q00–Q04) của `docs/HA_PRIME_PLAN.vi.md`; base revision `48676b6`; thực hiện 28/09/2026.

## Mục tiêu / không làm

- **Mục tiêu:** agent con của `ha` chạy như của prime-agent: sống quá lượt đã khởi động nó, báo kết quả cho cha bằng thông báo đánh thức cha, nhắn tin hai chiều, chạy trên model được chỉ định.
- **Không làm (CP sau / ngoài track):** độ sâu > 1, `rlm.create_session`, daemon, đánh thức lại agent con đã xong, cap hàng đợi tin nhắn 20/đích (xem mục "Khác prime").

## Requirement inventory

| Item | Trạng thái trước | Làm gì | Bằng chứng |
|---|---|---|---|
| Q00 provider giả có kịch bản | `missing` (mỗi ca PTY tự viết server) | `ScriptedSse` + `Reply` trong `tests/interactive_terminal.rs` | `q00_scripted_sse_serves_text_and_tool_calls` |
| Q01 store dùng chung | `incompatible`: mỗi lượt `open_writer` rồi đóng | `interactive/store_lease.rs` `SharedStore`/`StoreLease`; `run_turn` và `/rename` lấy lease | `q01_a_lease_opens_once_and_closes_after_the_last_drop`, i05/i13 PTY |
| Q01 agent con cấp session | `incompatible`: scheduler thuộc `DelegateHost` từng lượt, `shutdown` huỷ + drain | `SessionAgents` (scheduler, registry con, pump kết quả) trong `AgentSessionService`; `DelegateHost` chỉ là góc nhìn của lượt | `q01_children_outlive_the_turn_that_spawned_them` |
| Q01 quyền task khi store mở qua nhiều lượt | `missing` (phát hiện khi làm, PTY q03 đỏ `task_lease_conflict`) | `SqliteStore::release_task_lease` — chỉ xoá lease của đúng instance store đang mở; `run_turn` gọi trước khi nhận task | `q03_pty_child_reports_to_its_parent` (đỏ trước, xanh sau) |
| Q02 thông báo kết thúc | `missing` | `terminal_notice` (chuỗi prime), `SessionEvent::ChildSettled`, controller `pending_notices`/`notices_held` | `q02_*` |
| Q02 `delegate wait:false` | `missing` | tham số `wait` (mặc định true) | `q02_delegate_wait_false_returns_before_the_child_finishes` |
| Q03 `agent_message` / `progress_note` | `incompatible`: Rust từ chối | cha: host request `agent_message.send`; con: tool gốc `agent_message`, `progress_note`; `RunInbox::deliver` (payload `verbatim`) | `q03_*` |
| Q04 model cho agent con | `incompatible`: khác model cha bị từ chối | `ChildLaunch::for_model`, `ChildModels` (`ServiceChildModels`), config `[agents] default_model` | `q04_spawn_model_resolution_order` |
| Q04 `/agents` quản lý | `adapt` | dòng mỗi con: tên · role · model · trạng thái · giây · tool call · chi phí · note; `/agents stop [tên]` | `q01_ctrl_c_cancels_the_turn_but_not_its_children`, PTY q04 |

## prime làm gì / ha làm gì / khác gì

| Chủ đề | prime-agent | ha | Khác và lý do |
|---|---|---|---|
| Spawn | `rlm.spawn` trả handle khi được nhận, con chạy task tách rời | như prime; thêm `delegate wait:false` | `delegate` giữ chế độ chờ làm mặc định (D5) |
| Thông báo | `[child-failed child:N]`, `[child-exited: cancelled child:N]`, `[child-exited: no-reply child:N]` + `Last assistant text` 160 ký tự; lane follow-up | cùng chuỗi; lane follow-up của controller | Giữ tới 8000 ký tự vì cha có thể không có kernel để `collect` (D4); thông báo bị bỏ khi đang có người chờ kết quả (`delegate` chờ, `rlm.collect` có timeout) |
| Ctrl+C | `requestAbort` không huỷ con | như prime | — |
| Sau khi người dùng dừng lượt | thông báo bị "demote", chờ | giữ tới khi lượt tiếp theo **của người dùng** kết thúc | cùng ý |
| Tin nhắn | skill Python `agent_message`, daemon giao kiểu steer | cha: skill Python; con: tool gốc (con không có kernel); giao vào `RunInbox` của đích, hoặc thành lượt riêng khi cha rảnh | Con đã xong không được đánh thức lại (không có daemon) |
| Giới hạn tin | 16 384 ký tự; 20 tin chờ/đích; bucket 3, +1/giây | 16 384; bucket 3, +1/giây | Chưa có cap 20 tin chờ/đích (không đếm được tin chưa claim trong inbox); ghi lại để CP sau |
| Progress note | ≤ 512, 10 s/lần, ring 5, chỉ đọc qua snapshot | như prime | — |
| Model con | tham số `model` → `subagentDefaultModel` → cha; model không dùng được thì lỗi | như prime; default từ `[agents] default_model` | — |
| Độ sâu | mặc định 2 | 1 | ngoài phạm vi (plan mục 11) |

## Quyết định thêm khi làm

- **Hàng đợi scheduler:** `max_queued_workers` = 8 — trần của host (`a queue of N workers exceeds the host cap 8`), trước đây 3.
- **Tin nhắn vào lượt đang chạy:** `RunInbox::deliver` đánh dấu payload `{"verbatim": true}`; `claim_inbox` đưa chữ nguyên văn thay vì thêm nhãn `[steering correction from the user]`. Đây là trường có cấu trúc, không dò chữ.
- **Policy:** `agent_message` và `progress_note` (plugin `agent`) được cho phép không hỏi, như `goal_complete` — chỉ đổi trạng thái trong session.
- **Provider của con:** dựng riêng từ config của lượt (`LiveProvider::build`), không dùng `LiveProvider` của cha, để `/model` ở lượt sau không đổi model của con đang chạy.
- **Lease task:** trước đây mỗi lượt mở store với `HostId` mới nên tự tiếp quản lease của lượt trước; store dùng chung thì cùng host + generation → `task_lease_conflict`. `release_task_lease` chỉ xoá lease của chính instance đang mở (host + generation khớp), không nới luật với host khác.

## Hợp đồng và schema bị chạm

- `HarnessConfigV2.agents: Option<AgentsConfigV2 { default_model }>` — optional, config cũ load nguyên; `schemas/harness-config.v2.schema.json` sinh lại bằng `generate_schemas`.
- `SessionEvent::{ChildSettled, AgentMessage}`; `SessionPort::{deliver_message, stop_agents}` (mặc định trả lỗi).
- Tool schema `delegate` thêm `wait`; con có thêm tool `agent_message`, `progress_note`.
- Store: hàm mới `release_task_lease`, không đổi schema SQL.


# CP-2 (Q05–Q09): hàng đợi, cất nháp, `/btw`, rẽ nhánh, xuất HTML

Base `93bc8d4`; thực hiện 28/09/2026.

| Item | Trạng thái trước | Làm gì | Bằng chứng |
|---|---|---|---|
| Q05 hàng đợi 2 lane | `adapt`: một `queued_input` | `interactive/queue.rs` (`Lane`, `QueueMode`, `InputQueue`); controller: steer không nhận được → lane Steer; `/queue` (`/followup`) → lane FollowUp; sau `RunTerminal`: steer → follow-up → thông báo con → heartbeat; config `[queue] steering_mode/follow_up_mode` | `q05_*`, PTY `q05_pty_follow_up_runs_after_the_turn` |
| Q05 sau Ctrl+C | Ctrl+C lần đầu xoá input chờ | hàng đợi được giữ, chờ lượt tiếp theo của người dùng (cùng cờ với thông báo con) | `q05_the_queue_survives_an_interrupt` |
| Q06 sửa hàng đợi, cất nháp | `missing` | `/queue list|edit n text|drop n|up n|down n`; Ctrl-S (`Key::Stash`) và `/stash` với chuỗi của prime | `q06_*` |
| Q07 `/btw` | `missing` | `interactive/side_question.rs` (chuỗi `SIDE_QUESTION_INSTRUCTION` nguyên văn prime); service gọi provider **không tool** với system prompt + lịch sử + các lượt bên lề; không ghi store; `SessionEvent::SideAnswer` mở bảng | `q07_*`, PTY `q07_pty_btw_answers_in_a_panel` |
| Q08 `/fork`, `/clone`, `/tree` | `missing` | Bước 0 đo: runtime từ chối nối lượt sang task khác (`continuation task does not match source session`) → **phương án B**: task mới + setting `forked_from`; `previous_in_conversation` / `conversation_turns` trong runtime; lượt đầu của nhánh nạp lịch sử tới điểm rẽ; `/tree n` tiếp tục sau lượt n trong cùng task | `q08_*`, PTY `q08_pty_fork_then_answer` |
| Q09 xuất HTML | `missing` | `interactive/export_html.rs`: một trang tự chứa, CSS nội tuyến, không script/không tải mạng, escape mọi chữ, che secret; `/export x.html` | `q09_*` |

**Khác plan / prime (có chủ đích):**
- Dòng trạng thái hiện `queued (n)`; danh sách từng tin xem bằng `/queue` (layout TUI cố định số dòng). Không có Alt+Up/Ctrl+Alt+Up: sửa/xoá/đổi chỗ bằng `/queue edit|drop|up|down`.
- `/btw` gửi request **không có tool** thay vì gửi tool rồi chặn (prime chặn tool call và tối đa 3 vòng); không huỷ được bằng Esc giữa chừng (câu trả lời tới thì mở bảng).
- `/fork`, `/tree` dùng danh sách đánh số trong bảng + `/fork <n>`, `/tree <n>` thay cho bộ chọn cây; chưa có nhãn và tóm tắt nhánh khi chuyển (plan mục 11).
- `/tree n` = tiếp tục **sau** lượt n (như chọn entry assistant ở prime).
- Như mọi lệnh có tham số tuỳ chọn, Enter đầu tiên ở menu gõ `/fork ` và Enter thứ hai chạy nó.
- HTML export dựng từ lịch sử hội thoại (tin người dùng, trả lời, lời gọi tool), không phải toàn bộ event log như prime.

Hợp đồng: `HarnessConfigV2.queue: Option<QueueConfigV2 { steering_mode, follow_up_mode }>` (schema sinh lại); `SessionEvent::{SideAnswer, TurnsListed}`, `TurnsPurpose`; `SessionPort::{queue_modes, side_question, list_turns, fork, clone_conversation, switch_to}`; `Key::Stash`; runtime `FORKED_FROM_SETTING`, `previous_in_conversation`, `conversation_turns`. 40 lệnh slash.
