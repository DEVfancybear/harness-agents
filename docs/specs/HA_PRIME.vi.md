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


# CP-3 (Q10–Q13): `models.json`, model trong phạm vi, định tuyến model, chờ hạn mức

Base `5d7efd9`; thực hiện 28/09/2026.

| Item | Trạng thái trước | Làm gì | Bằng chứng |
|---|---|---|---|
| Q10 `models.json` | `missing` | `interactive/custom_models.rs`: schema prime (`providers`/`models`/`modelOverrides`, comment `//` `/* */`), kiểm tra như prime (provider mới cần `baseUrl` + `apiKey`; `api` phải là wire format `ha` nói được; `contextWindow`/`maxTokens` > 0; trường lạ bị từ chối và được nêu tên), mặc định prime; `Catalog::load` áp file; `Model.key_variable`, `Selection.api_key_env`; `/model` liệt kê provider tuỳ chỉnh có key; lỗi → notice một lần | `q10_*` (5 unit), PTY `q10_pty_custom_model_is_selectable` |
| Q11 phạm vi + đổi nhanh | `missing` | `[routing] scoped` (glob `*`/`?`, không phân biệt hoa thường, `:level`); `/scoped-models [pattern…\|clear]` lưu `scoped-models.json` cạnh config; `/model next\|prev`; **đo được** Alt+M / Shift+Alt+M qua ConPTY (`Key::CycleModel`) | `q11_scope_patterns_match_like_prime`, `q11_step_wraps_around_both_ways`, PTY `q11_pty_cycle_changes_the_footer_model` |
| Q12 phụ trợ | `missing` | `[routing] auxiliary`: `RuntimeService::with_summarizer(AuxiliarySummary)` cho compaction; `/refine` và review tự động gọi model phụ; không dựng được hoặc tóm tắt lỗi → model phiên + notice `auxiliaryModel "<x>" unusable for …; using the session model.` (một lần) | `q12_auxiliary_model_writes_the_summary`, `q12_unusable_auxiliary_falls_back_with_notice` |
| Q12 dự phòng | `missing` | `routing::Router` trong `LiveProvider`: lỗi `rate_limited`/`service_unavailable` trước khi có output, tới lần thử cuối của runtime (hoặc ngay khi `rate_limited`) → lượt chuyển sang `backup`, notice prime; lượt sau về model chính, thành công thì `Primary provider recovered — back on <x>` | `q12_backup_takes_over_after_retries_and_hands_back`, `q13_a_rate_limit_goes_to_the_backup_before_any_wait` |
| Q12 ảnh | `missing` | request có ảnh + `Model.input` của model phiên không có `image` → `image` model; không có → lỗi `IncompatibleService` rõ ràng, ảnh không tới model chữ | `q12_images_route_to_the_image_model`, `q12_images_without_an_image_model_fail_clearly` |
| Q13 mã lỗi | `incompatible`: 429 = `service_unavailable` | `ErrorCode::RateLimited` (`rate_limited`, retry `Transient`, exit như `service_unavailable`); 429 → `RateLimited`; mã lỗi JSON có cấu trúc `rate_limit_exceeded`/`insufficient_quota`/`rate_limit_error` (ở `/error/code`, `/error/type`, `/code`, `/type`) → `RateLimited`, không đọc câu chữ tự do | `q13_429_maps_to_rate_limited`, `q13_a_structured_quota_code_names_a_usage_limit` |
| Q13 chờ hạn mức | `missing` | trong `Router`: 1 s gấp đôi tới 300 s, 30 lần, tổng 15 phút (hằng của prime), `Retry-After` ≤ 300 s được ưu tiên; `SessionEvent::ProviderWaiting` → dòng trạng thái prime thay dòng "Thinking"; Esc huỷ (`provider_canceled`); quá giới hạn → `Provider recovery wait gave up after N pings`; `[routing] wait_for_usage = false` tắt | `q13_rate_limit_waits_and_recovers`, `q13_rate_limit_wait_is_cancelable`, `q13_wait_gives_up_after_the_bound`, PTY `q13_pty_waiting_line_is_shown` |

**Khác plan / prime (có chủ đích):**
- Bảng config là `[routing]`, không phải `[models]` như plan: `[models]` đã là bảng theo từng model (`[models."<id>"] context_window`), không thể thêm khoá `enabled`. Khoá: `scoped`, `auxiliary`, `backup`, `image`, `wait_for_usage`.
- `apiKey` trong `models.json` chỉ nhận **tên biến môi trường** (prime nhận cả giá trị): key không bao giờ nằm trong file; giá trị không giống tên biến bị từ chối mà không in lại. Không hỗ trợ `headers` (trường lạ → lỗi nêu tên).
- `/scoped-models` là lệnh có tham số, không phải picker nhiều lựa chọn; lưu vào `scoped-models.json` (không viết lại `config.toml` để không mất comment).
- Chờ hạn mức và dự phòng nằm ở `LiveProvider` (lớp đổi model của phiên), không trong vòng retry của runtime: vòng đó gắn với store và ngân sách mỗi lần gọi. Hệ quả: `rate_limited` có backup thì chuyển ngay, không chờ; sau khi bỏ cuộc, các lần thử lại của runtime trong cùng lượt không chờ lại.
- Model ảnh được chọn cho **mọi request có ảnh** (kể cả khi ảnh nằm ở lượt trước trong lịch sử), để không bao giờ gửi ảnh cho model chữ; model không có trong catalog được coi là nhận ảnh.
- Không "park qua đêm" khi hết 15 phút (cần lịch bền - CP-4 Q15).

Hợp đồng: `ErrorCode::RateLimited` (`schemas/error-report.v1.schema.json`); `HarnessConfigV2.routing: Option<RoutingConfigV2>` (`harness-config.v2.schema.json`); `SessionEvent::ProviderWaiting`, `UiState.provider_wait`, `Key::CycleModel`; `SessionPort::{cycle_model, scoped_models}`; file mới cạnh config: `models.json`, `scoped-models.json`; `Selection.api_key_env` (tuỳ chọn). 41 lệnh slash.


# CP-4 (Q14–Q15): `/autonomous` với cổng kiểm tra, lịch chạy bền

Base `3f423c1`; thực hiện 28/09/2026.

| Item | Trạng thái trước | Làm gì | Bằng chứng |
|---|---|---|---|
| Q14 `/autonomous` | `missing` | `interactive/autonomous.rs` port `core/autonomous.ts`: `Limits` (3/12/80 000/30 phút), `Gates` (retries 3, timeout 5 phút), `parse` theo `_parseAutonomousSlashCommand` + `parseAutonomousBudgetOptions` (alias `enable/disable`, `--autonomous-*`, `unlimited`, dấu phân cách `,`/`_`, nêu một ngân sách → các ngân sách khác không giới hạn), `decide` theo `shouldAutonomouslyContinue` (tắt/lỗi/huỷ → dừng; gate pass → dừng; retry hết → dừng; gate fail → dừng nếu chạm giới hạn, không thì tiếp tục; không gate → giới hạn theo thứ tự continuations → turns → tokens → thời gian), câu tiếp tục và câu gate-failed nguyên văn, status `[autonomous-status: …]` | `q14_the_command_parses_like_prime`, `q14_decide_follows_prime_order`, `q14_gate_failure_prompt_matches_prime`, `q14_status_reads_like_prime` |
| Q14 cổng | `missing` | `run_gates`: mỗi lệnh chạy qua `harness_tools::run_user_command` (mới: shell + môi trường đã lọc như shell tool, timeout, huỷ cả cây), output cắt 6000 ký tự + `... [truncated]`; snapshot git (`status --porcelain=v1 -z -uall --no-renames`, `diff --binary HEAD`, sha256 file untracked, loại trừ pathspec của prime) - worktree không đổi so với lần hỏng trước thì không chạy lại mà tính một lần thử | `q14_unchanged_worktree_skips_the_gate` (git + pwsh thật) |
| Q14 controller | `missing` | sau `RunTerminal` (sau hàng đợi, thông báo con, heartbeat, compact, continuation bound, goal): đếm lượt + token (`SessionPort::session_tokens`), có con đang chạy → giữ (báo cáo của con đánh thức phiên), có gate → `run_gates` rồi `SessionEvent::GatesChecked` quyết định; người dùng gõ trong lúc gate chạy → huỷ gate; goal và autonomous loại trừ nhau | `q14_autonomous_continues_until_the_gate_passes`, `q14_autonomous_without_gates_stops_at_its_limit`, `q14_goal_and_autonomous_take_turns`, PTY `q14_pty_gate_passes_after_one_fix` |
| Q15 parser + cron | `missing` (heartbeat chỉ có `every`) | `interactive/schedules.rs` port `parseAgentCronSchedule`: `in N m|h|d`, `every|each N s|m|h` (≥ 10 s), `at <ngày>` (RFC 3339; không múi giờ = giờ máy; chỉ ngày = UTC, như `Date` của JS), cron 5 trường + `@hourly/@daily/@weekly/@monthly`; cron **viết theo prime** (quét từng phút, tối đa 366 ngày, theo giờ máy qua `chrono::Local`) - không dùng `daemon::schedule::next_after` | `q15_parser_accepts_prime_forms`, `q15_cron_next_run_is_local_time` |
| Q15 lưu bền | `missing` | `<data dir>/schedules/<task>.json` (ghi file tạm rồi rename); gắn với task của hội thoại: lúc mở app, `/new`, `/fork`/`/clone`, và resume (tra task trong `show_conversation`); lỡ nhiều lần → chạy một lần, lần sau tính từ lúc chạy; job một lần → `completed` | `q15_missed_runs_collapse_into_one`, `q15_jobs_survive_a_restart`, PTY `q15_pty_scheduled_job_fires` |
| Q15 giao | `missing` | job tới giờ đi qua `due_heartbeats` (cùng đường heartbeat): chữ `[heartbeat: <expr> run#N]\n\n<prompt>` như `createHeartbeatPromptMessage`; mặc định follow-up, `--steer` để chen vào lượt đang chạy; `/schedule list|add|pause|resume|cancel` | `heartbeats_run_when_due_and_respect_their_delivery_mode` (đường giao), PTY q15 |

**Khác plan / prime (có chủ đích):**
- Không có van giữ-sống 25 phút cho agent con (`subagentKeepAliveMs`): khi con chạy, phiên chỉ chờ báo cáo của chúng; cờ `--subagent-keep-alive-ms` bị từ chối như cờ lạ, và câu status không có đoạn "Subagent keep-alive".
- `ha` in một thông báo khi autonomous dừng (`quality gates passed`, `stopped; <reason> reached`, `a quality gate failed more than N time(s)`) và khi tiếp tục (`continuing (n/max)`); prime TUI không in.
- Gate chạy trong shell của `ha` (pwsh trên Windows, `sh` nơi khác) với môi trường đã lọc như shell tool của model, không phải `shell: true` của Node; người dùng tự viết lệnh nên không qua panel duyệt.
- Người dùng gõ tin trong lúc gate đang chạy → gate bị huỷ và lượt đó không bị đánh giá.
- `in N` chỉ nhận phút/giờ/ngày như prime (plan có `in 2s` - sai với prime); ca PTY dùng `every 10s`.
- `/heartbeat` (lệnh người dùng của prime) chưa port: heartbeat do model tạo (`rlm_heartbeat`) vẫn sống theo phiên như prime; lịch bền do người dùng tạo bằng `/schedule`.
- Không hoãn job khi đang compaction/chờ quota (prime chỉ hoãn heartbeat khi bận); job follow-up tự chờ lượt đang chạy.

Hợp đồng: `SessionPort::{schedule, session_tokens, running_children, run_gates, cancel_gates}`; `SessionEvent::GatesChecked`; `harness_tools::run_user_command` (mới, public); `SessionAgents::running`; file mới `<data dir>/schedules/<task>.json`. Không đổi config/schema. 43 lệnh slash.

**Sửa kèm (CP-3):** `/scoped-models a b` chỉ lưu mẫu đầu tiên vì controller đưa `argument` (một từ) thay cho `raw_argument` - tái hiện bằng `q11_scoped_models_takes_every_pattern` (đỏ: port nhận `deepseek/*`), sửa, xanh.

**Sửa khi chạy toàn bộ (CP-1, Q03):** tin của agent con tới inbox của lượt cha **sau bước cuối** của lượt (sau lần đọc inbox cuối, hoặc khi task ghi tin còn đang chạy) bị mất cùng lượt - đo được trong lần chạy PTY đầy đủ: `q03_pty_child_reports_to_its_parent` hết 90 s, transcript có "a child's message reached the running turn" rồi lượt kết thúc mà cha không đọc. Sửa: `ActiveTurnInbox.in_flight` đếm tin đang ghi; khi lượt kết thúc, gỡ inbox, chờ tin đang ghi (tối đa 3 s), nhận các lệnh steer còn chờ của run và trả về bằng `SessionEvent::UnreadMessage` - tin của agent vào hàng thông báo, steer của người dùng vào lane steer (prime giữ steer chưa nhận cho lượt sau). Bằng chứng: `q03_unread_inbox_messages_are_carried_to_the_next_turn` (negative control: bỏ vòng chờ `in_flight` → đỏ, thiếu tin "also check the lexer"), `q03_unread_messages_run_after_the_turn`, PTY `q03b_pty_a_message_after_the_last_step_is_read_next` (khe này không dựng được chắc chắn qua PTY: driver đọc inbox cả sau câu trả lời cuối, nên ca này xanh cả khi tắt bản sửa - giữ lại làm hồi quy).

**Sửa test PTY chập chờn:** `i01_bare_launch_…` và `t07_pty_plain_flag` chờ chữ ở đầu khung hình rồi kiểm tra chữ vẽ sau (đỏ ~1/3 lần chạy riêng) → chờ đúng chữ được kiểm tra; khẳng định không đổi. Khởi động không còn parse catalog khi không có `models.json`.
