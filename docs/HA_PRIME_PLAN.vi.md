# Plan cho DeepSeek: đóng khoảng cách còn lại giữa `ha` và prime-agent (track Q01–Q15)

**Lập ngày 28/09/2026 · Planning only · Feature track Q01–Q15.** Chưa có dòng code Q nào. Trạng thái thật của track nằm ở `docs/specs/HA_PRIME.vi.md`, `docs/evidence/HA_PRIME.vi.md` và `docs/handoffs/HA_PRIME.vi.md` (planned, DeepSeek tạo khi nhận assignment).

[Prompt giao DeepSeek](HA_PRIME_PROMPT.vi.md) · [Track G01–G14](HA_AGENT_PLAN.vi.md) · [Track TUI T01–T08](HA_TUI_PLAN.vi.md) · [Track khởi động H01–H08](HA_LAUNCH_PLAN.vi.md) · [Templates SPEC/evidence/handoff](implementation-next/TEMPLATES.vi.md) · [Operator guide](OPERATOR_GUIDE.vi.md)

> **Đọc mục 3 trước khi code bất cứ thứ gì.** Plan này viết cho người thực hiện không nắm hết repo: mỗi work item chia thành bước nhỏ, mỗi bước nói rõ đọc file nào, sửa hàm nào, viết test gì trước, chạy lệnh gì, và thế nào là xong. Làm **đúng thứ tự bước**; không gộp bước; không "tiện tay" sửa chỗ khác.

## 1. Vì sao có track này

Sau chuỗi port từ prime-agent (system prompt, `ipython` + `rlm`, skills, `/goal`, heartbeats, `/refine`, compaction, catalog model, login, TUI, syntax highlighting, sửa delegate), `ha` đã có phần lõi của prime. Đối chiếu hai kho code ngày 28/09/2026 còn các khoảng cách sau (chi tiết từng dòng ở mục 2):

| Nhóm | prime-agent có | `ha` tại `bae5cc7` | Item |
|---|---|---|---|
| Agent con chạy nền | `rlm.spawn` trả handle ngay; con chạy độc lập với lượt; khi con xong, thông báo `[child-failed …]`/`[child-exited …]` **tự đánh thức** agent cha | Con gắn với lượt: lượt kết thúc là con bị huỷ; `delegate` chặn cha tới khi con xong | Q01, Q02 |
| Nhắn tin giữa agent | `agent_message.send` (cha/anh em/con/all), `agent_observe`, `rlm.progress_note` | Cả ba đều bị từ chối trong Rust | Q03 |
| Quản lý agent con | model riêng cho con (`subagentDefaultModel`, `model=`), màn hình quản lý con | Con luôn dùng model của cha; `/agents` chỉ in trạng thái | Q04 |
| Hàng đợi tin nhắn | steer + follow-up, chế độ `all`/`one-at-a-time`, xem/sửa/xoá hàng đợi, cất nháp Ctrl+S | Enter = steer; chỉ **một** input chờ | Q05, Q06 |
| Câu hỏi bên lề | `/btw` hỏi không ghi vào session | Không có | Q07 |
| Rẽ nhánh hội thoại | `/fork`, `/clone`, `/tree` | Không có | Q08 |
| Xuất HTML | `/export` ra HTML tự chứa | Chỉ Markdown/JSONL | Q09 |
| Model tuỳ chỉnh | `models.json` (thêm/ghi đè model), `/scoped-models` + Alt+M | Chỉ catalog có sẵn | Q10, Q11 |
| Định tuyến model | `auxiliaryModel` (tóm tắt), `providerBackupModel` (dự phòng), `imageModel` (ảnh) | Mọi việc dùng model của phiên | Q12 |
| Chờ hết hạn mức | wait-for-usage: đợi quota hồi, báo trên UI, Esc huỷ | 429 = lỗi tạm thời, thử lại 3 lần tối đa 30 s rồi thất bại | Q13 |
| Tự chủ | `/autonomous` với cổng kiểm tra (lệnh shell) và giới hạn | Không có (chỉ `/goal`) | Q14 |
| Lịch chạy | cron / `in 10m` / `at <ISO>` / interval, lưu file, chạy bù | Heartbeat chỉ `every N`, mất khi thoát | Q15 |

**Không thuộc track này** (ghi ở mục 11 để khỏi quên): thêm provider/OAuth, API extension đầy đủ, chế độ `rpc`/`acp`, fullscreen/ảnh/Mermaid trong terminal, tự cập nhật, daemon nhiều session, độ sâu agent con > 1, `/import`, tóm tắt nhánh khi chuyển nhánh.

## 2. Căn cứ source ở HEAD `bae5cc7`

Bảng này là **baseline lúc lập plan**. Trước mỗi assignment phải đọc lại `git status`/HEAD và mở đúng các dòng dưới đây — số dòng có thể lệch vài dòng; tìm theo tên hàm.

### 2.1 Phía `ha` (đường dẫn tương đối gốc repo)

| Chủ đề | Chỗ | Ghi chú |
|---|---|---|
| Trạng thái cấp session | `crates/harness-cli/src/interactive/service.rs` `struct AgentSessionService` (~1399) | Giữ `sender`, `task_id`, `previous_session`, `active_inbox`, `gate`, `writer_gate`, `goal`, `repl`, `heartbeats`, `live`. Constructor `new_with_overrides` (~1876) |
| Cổng controller → service | `service.rs` `pub trait SessionPort` (~149) | `submit`, `cancel`, `steer`, `resume`, `set_goal`, `set_model`, `due_heartbeats`, `model_options`… Mọi lệnh mới của controller đi qua trait này |
| Bắt đầu một lượt | `service.rs` `fn submit` (~2205) → `handle.spawn(run_turn(...))` (~2260) | `SessionId::generate()` mỗi lượt |
| Thân một lượt | `service.rs` `async fn run_turn` (~3389, 35 tham số) | Mở store: `writer_gate.lock()` rồi `SqliteStore::open_writer` (~3435); tạo `DelegateHost` (~3996); gắn `RunInbox` (~4317); gọi driver (~4328); `delegate.shutdown()` (~4341); đóng store (~4522); map `TurnStop` → `RunOutcome` (~4535) |
| Agent con | `interactive/delegation.rs` `DelegateHost` (~64), `DelegateDispatcher::start/wait` (~265/~389), `RlmChildren` (~726), `InteractiveWorkerBackend::dispatch` (~1119) | Scheduler: `max_concurrent_workers 3, max_depth 1, max_queued_workers 3` (~113). Chỗ từ chối: `rlm.progress.note` (~1042), `rlm.create_session` (~1045), `agent_message.send` (~1086) |
| Python kernel | `crates/harness-cli/python/rlm/__init__.py` (spawn ~161, `create_session` ~199, `progress_note` ~430); `.agents/skills/agent-message/src/agent_message/__init__.py:53` | Python không từ chối gì, chỉ chuyển `host_request` sang Rust |
| Lượt tự động | `interactive/controller.rs`: `pump_events` (~809), `deliver_heartbeats` (~837), handler `RunTerminal` (~1342), `dispatch(text, automatic)` (~1455), `dispatch_while_running` (~1415), `maybe_continue` (~1628), `continue_goal` (~1653) | Thứ tự sau `RunTerminal`: queued_input → pending_heartbeats → pending_compact → continue → goal. `HistoryItem::Automatic` cho lượt tự động |
| Hàng đợi hiện có | `controller.rs` field `queued_input: Option<String>` (~184), thông báo `"queued (1): sent after the active run finishes"` (~1429) | Q05 thay bằng hàng đợi |
| Steer | `service.rs` `fn steer` (~3253); `crates/harness-runtime/src/inbox.rs` `RunInbox` (~20); `crates/harness-tools/src/turn_driver.rs` `claim_inbox` (~470) | Steer thành message `"[steering correction from the user]\n{text}"` |
| Sự kiện | `interactive/events.rs` `enum SessionEvent` (~478), `enum HistoryItem` (~262), `RunOutcome` (~172) | Thêm variant mới ở đây |
| Liên kết lượt | `crates/harness-runtime/src/lib.rs` `pub async fn conversation_history` (~1139); store `record_continuation_link` / `continuation_link` / `open_forked_session` (`crates/harness-store-sqlite/src/store.rs` ~2531/~2677/~2560) | Mỗi lượt là một session, nối với lượt trước qua `session_lineage`. `open_forked_session` **từ chối fork khác task** |
| Resume/đổi tên | `service.rs` `resume` (~3217), `list_sessions` (~3154), `conversation_heads` (~1846, một dòng mỗi task), `rename` (~2755) | |
| Xuất | `service.rs` `render_session_export` (~4912); controller `/export` (~2090) chỉ nhận `md`/`jsonl` | |
| Compaction | `harness-runtime/src/lib.rs` `trait SummaryProvider` (~735), `ModelSummaryProvider` (~786); `RuntimeService::new` chọn summarizer (~1379) | Dùng chung provider của phiên |
| Catalog model | `interactive/providers.rs` `struct Model` (~162), `Catalog` (~256: `bundled`, `load`, `admit`, `find`); `interactive/config.rs` `Selection` (~106), `selection.json` | |
| Dựng provider | `service.rs` `build_provider` (~1179), `ProviderConfig` (~629), `LiveTurn`/`LiveProvider` (~1042/~1073) | Đổi model giữa lượt đã có |
| Lỗi provider | `crates/harness-providers/src/lib.rs` `http_status_error` (~887): 429/5xx → `ServiceUnavailable`; `retry_after_seconds` cap 30 s (~1243); vòng thử lại `harness-runtime/src/lib.rs` (~1875), `RuntimeConfig { max_attempts: 3, max_retry_after_seconds: 30 }` (~209) | **Không** có mã lỗi riêng cho 429 |
| Ảnh | `service.rs` `attachments::from_message` (~4151), `run_request.with_images` (~4290); `Model.input` trong catalog có `"image"` nhưng **chưa dùng** để chặn/định tuyến | |
| Heartbeat | `interactive/heartbeat.rs`: `parse_schedule` (~109, chỉ `every N`), `Heartbeats::due` (~328), không lưu file | |
| Lịch trong daemon | `crates/harness-cli/src/daemon/schedule.rs`: `ScheduleSpec` (~24), `next_after` (~408, cron 5 trường), `due_now` (~477); **không được binary `ha` dùng** | Có thể tái dùng `next_after` |
| Goal | `interactive/goal.rs` (`DEFAULT_GOAL_CONTINUATIONS = 10`, `goal_complete`) | |

### 2.2 Phía prime-agent (tham chiếu để port)

Mã nguồn prime nằm ở bản clone cục bộ (commit `e260085`). Nếu không có trong máy, clone `https://github.com/PrimeIntellect-ai/prime-agent` rồi `git checkout e260085`. Ký hiệu: **CA** = `packages/coding-agent/src`, **RT** = `prime-agent-runtime/src/rlm`, **SK** = `packages/coding-agent/skills`.

| Tính năng | File prime |
|---|---|
| Spawn không chặn, thông báo kết thúc | CA/core/agent-session.ts `_startRlmChildRun` (~12593–13124), `_deferRlmTerminalNotice`/`_flushDeferredRlmTerminalNotices` (~5752–5838); CA/core/messages.ts (~269–303) |
| collect/list/delete/progress | RT/`__init__.py` (~19–87, 390–478); CA/core/agent-session.ts (~11406–11456, 11553+, 11810) |
| agent_message / observe | SK/agent-message, SK/agent-observe; CA/core/agent-messages.ts (~18–75, 397–408); CA/modes/daemon/daemon-mode.ts (~6449–6483) |
| Model cho con | CA/core/agent-session.ts `_resolveRlmSubagentModel` (~12545); CA/core/settings-manager.ts `getSubagentDefaultModel` (~801) |
| Hàng đợi, stash | CA/modes/interactive/interactive-mode.ts (~7755–7785, 8238–8277, 4768–4803), queue-selection.ts; settings `steeringMode`/`followUpMode` (~854/864) |
| `/btw` | CA/core/side-question.ts |
| fork/clone/tree | interactive-mode.ts (~9210–9380); agent-session-runtime.ts (~500–620); session-manager.ts `createBranchedSession` (~2568) |
| Xuất HTML | CA/core/export-html/index.ts (~138–266) |
| models.json, scoped | CA/core/model-registry.ts (~156–213, 503, 649–770); model-resolver.ts (~292–346); agent-session.ts (~8407–8455) |
| Định tuyến, chờ quota | agent-session.ts `_resolveAuxiliaryModel` (~9451), `_handleRetryableError` (~13315–13420); CA/core/image-model-routing.ts; CA/core/provider-retry.ts (~175–180); interactive-mode.ts (~6386–6402) |
| `/autonomous` | CA/core/autonomous.ts (~53–60, 390–415); agent-session.ts (~1225–1240, 2621) |
| Lịch chạy | CA/core/cron-jobs.ts (~27–48, 1155–1215, 1450–1556, 1749–1775) |

## 3. Cách làm việc bắt buộc (đọc trước khi code)

### 3.1 Vòng lặp cho **mỗi bước** trong một item

1. **Đọc tham chiếu.** Mở file prime ở mục 2.2 cho tính năng đó và các chỗ `ha` ở mục 2.1. Ghi 3–5 dòng vào SPEC: prime làm gì, `ha` sẽ làm gì, khác ở đâu và vì sao.
2. **Viết test trước (RED).** Viết đúng tên test plan đặt. Chạy **chỉ test đó** và xác nhận nó **đỏ đúng lý do** (không phải lỗi biên dịch vô nghĩa, không phải panic ở chỗ khác). Chép dòng lỗi vào evidence.
3. **Code tối thiểu (GREEN).** Sửa đúng file bước đó nói. Không đổi tên/di chuyển code không liên quan.
4. **Chạy lại test đó** tới khi xanh, rồi chạy **suite của module** (mục 3.3).
5. `cargo fmt --all` và `cargo clippy --workspace --all-targets --locked -- -D warnings`. Không được thêm `#[allow(...)]` để né clippy trừ khi giải thích được bằng một câu trong comment.
6. Ghi evidence: lệnh đã chạy, số test pass/fail, đoạn output quan trọng.

Nếu một bước đỏ mà không hiểu vì sao: **dừng, đọc lại code thật**, không đoán. Không bao giờ sửa test cho xanh bằng cách nới điều kiện; không xoá test cũ.

### 3.2 Luật không được phá

- **Port theo prime, không tự chế heuristic.** Không dò từ khoá trong câu của người dùng hoặc của model để đoán ý ("không hard code cho model"). Khi prime giải quyết bằng tool/prompt/model, làm y như vậy.
- **Một binary `ha`, không crate mới**, không engine thứ hai. Dependency mới phải pin chính xác (`"=x.y.z"`), `Cargo.lock` đi cùng commit, ghi version + license vào SPEC. Ưu tiên thứ đã có trong `Cargo.lock`.
- **Store một writer.** `SqliteStore::open_writer` giữ khoá file; hai writer cùng lúc trong một project là lỗi. Mọi thứ chạy ngoài lượt (Q01) phải dùng chung một handle (xem D1).
- **Approval fail-closed**, luật deny thắng, protected path chặn trước panel — giữ nguyên.
- **Không đổi hành vi đã có test** trừ khi item nói rõ; nếu một test cũ phải đổi kỳ vọng, ghi lý do vào SPEC.
- Chữ hiển thị cho người dùng và thông báo gửi model: **tiếng Anh**, lấy nguyên văn prime khi prime có (mục mỗi item ghi rõ chuỗi).

### 3.3 Lệnh kiểm tra

```text
# một test
cargo test -p harness-cli --bin ha <tên_test> -- --nocapture
# unit test của app tương tác (controller, service, delegation, tui…)
cargo test -p harness-cli --bin ha --locked
# runtime / tools / providers
cargo test -p harness-runtime --locked
cargo test -p harness-tools --locked
cargo test -p harness-providers --locked
# toàn bộ (lâu, ~10 phút; chạy nền được)
cargo test --workspace --locked --no-fail-fast
# PTY thật (25 ca + ca mới của track này) — PHẢI chạy bằng script, cargo test bỏ qua chúng
pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1 -TimeoutSeconds 900
pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1 -Filter q02_ -TimeoutSeconds 300
# docs
pwsh -NoProfile -File scripts/Verify-Docs.ps1
```

**Test chập chờn đã biết** (đỏ khi cả bộ chạy nặng máy, xanh khi chạy riêng): `g09_hook_cannot_turn_ask_into_allow`, `g09_hook_receives_bounded_json_without_secrets`, `a_dead_owners_journal_is_reaped_and_removed`. Nếu chúng đỏ: chạy riêng từng test, ghi cả hai kết quả vào evidence; **không** sửa chúng trong track này.

### 3.4 Viết test ở từng tầng — mẫu để chép

**(a) Controller (logic lệnh, hàng đợi, lượt tự động)** — `controller.rs` `mod tests` (~3395). Dùng `bench(true)` (hoặc `tui_bench`) → có `controller`, `events` (gửi `SessionEvent` giả) và `port` (`RecordingPort`, ghi lại `submissions`, `steers`…). Mẫu hiện có để bắt chước: `g06_enter_while_running_steers_the_running_turn` (~3700), `heartbeats_run_when_due_and_respect_their_delivery_mode` (~6567).

```rust
#[test]
fn qNN_ten_test() {
    let mut harness = bench(true);
    submit_text(&mut harness.controller, "first");             // lượt 1 đang chạy
    // ... hành động cần test ...
    harness.events.send(SessionEvent::RunTerminal { outcome: RunOutcome::Done }).unwrap();
    harness.controller.pump_events();                          // xử lý sự kiện
    let sent = harness.port.submissions();                     // kiểm tra cái gì đã được gửi đi
    assert_eq!(sent.last().map(|s| s.text.as_str()), Some("..."));
}
```

Nếu `RecordingPort` thiếu hàm mới của `SessionPort`, **thêm vào RecordingPort** (ghi lại lời gọi) — đừng gọi service thật trong test controller.

**(b) Agent con** — `delegation.rs` `mod real_worker_tests` (~1975): provider giả `Answering` trả lời một dòng; `children_spawned_together_each_answer` dựng `DelegateHost` với store tạm. Bắt chước để test spawn/notice/message. Muốn provider giả **chậm** hoặc **lặp tool**: viết struct mới cùng file, ví dụ:

```rust
/// Trả lời sau `delay`, để test thấy con còn đang chạy.
struct Slow { delay: std::time::Duration }
impl ModelProvider for Slow {
    fn capabilities(&self) -> ModelCapabilities { ModelCapabilities::deepseek_fixture() }
    fn stream(&self, request: ProviderRequest, _: CancellationToken) -> ProviderFuture {
        let delay = self.delay;
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            Ok(vec![
                ProviderStreamEvent::Started { request_id: request.request_id },
                ProviderStreamEvent::TextDelta { text: "slow answer".to_owned() },
                ProviderStreamEvent::completed("stop"),
            ])
        })
    }
}
```

**(c) Runtime/driver** — `crates/harness-tools/src/turn_driver.rs` và `crates/harness-runtime/src/lib.rs` có module test riêng; test thuần hàm (không provider) đặt ngay cạnh hàm, ví dụ `mod repeated_read_tests` trong turn_driver.rs.

**(d) Service thật với provider giả qua HTTP** — `interactive/service_completion_tests.rs`: chạy trong tiến trình con (`HA_COMPLETION_CHILD`), dựng `AgentSessionService::new(&context, environment, channel.sender())`, provider là server SSE loopback (`tokio::net::TcpListener`, ~231–281), gọi `service.submit(...)`, chờ `terminal(&mut channel)`. Dùng khi cần chứng minh một luồng thật qua nhiều lượt (Q02, Q08, Q13).

**(e) PTY — TUI thật trong console thật** — `crates/harness-cli/tests/interactive_terminal.rs`: `sandbox()`, `provider_env(&temp, &endpoint)`, `PtySession::spawn(&project, &env)`, `send("text\r")`, `wait_for("needle", timeout)`, `transcript()`, `wait_exit`. Mọi ca PTY phải có `#[ignore = "needs a real console; run scripts/Invoke-HaPtyAcceptance.ps1"]` và chạy bằng script ở 3.3.

Server SSE giả **có kịch bản** dùng chung cho các ca PTY của track (bước chuẩn bị Q00, mục 6): thêm vào `interactive_terminal.rs` một helper `ScriptedSse` nhận một hàm `Fn(&serde_json::Value) -> Reply` (Reply = text | tool_calls | lỗi HTTP | body hỏng | chờ N ms) và ghi lại mọi request kèm thời điểm. Nhận biết request của cha hay con bằng **danh sách tool trong request** (cha có `delegate`/`ipython`, con thì không) — đây là cách đã dùng để tái hiện bug delegate ngày 27/09. Mẫu SSE hợp lệ: xem `write_sse` (~1457) và `sse_answer` (~1952) trong cùng file.

### 3.5 Test case thủ công (người dùng tự thử)

Mỗi item có bảng "Thử tay": các bước gõ trong `ha` thật và điều phải thấy. Sau khi code xong một checkpoint, DeepSeek chạy lại các bước này **bằng ca PTY tương ứng** (không cần người) và ghi transcript vào evidence; bảng thử tay là để người dùng kiểm lại trên máy mình.

### 3.6 Định nghĩa "xong" cho một item

- Mọi test plan liệt kê cho item đã có, đỏ trước, xanh sau (evidence có cả hai).
- `cargo fmt`, `clippy -D warnings` sạch; unit test `harness-cli` xanh; suite bị chạm xanh.
- Ca PTY của item xanh qua script.
- OPERATOR_GUIDE (`.en` và `.vi`) cập nhật đoạn mô tả hành vi mới; `Verify-Docs.ps1` xanh.
- SPEC ghi "prime làm gì / ha làm gì / khác gì"; handoff ghi exact next action.

## 4. Quyết định kiến trúc

| # | Quyết định | Lý do |
|---|---|---|
| D1 | **Store dùng chung có đếm tham chiếu** (`SharedStore` trong `AgentSessionService`): `lease().await` mở writer nếu đang đóng, trả `StoreLease` (giữ `Arc<SqliteStore>`); lease cuối bị drop thì đóng store ở task nền. Lượt và agent con đều lấy lease; không ai mở writer riêng nữa | Con phải sống quá lượt (Q01) mà store chỉ một writer. Đóng khi rảnh giữ nguyên hành vi i05 (thoát/nhàn rỗi nhả khoá cho host khác) |
| D2 | **Agent con thuộc session, không thuộc lượt**: `SessionAgents` (một `WorkerScheduler` + registry con) sống trong `AgentSessionService`, tạo khi cần lần đầu, huỷ khi `/new`, `/resume` sang hội thoại khác, `/quit`. Ctrl+C chỉ huỷ **lượt**, không huỷ con (như prime `requestAbort`) | prime tách abort lượt và abort session |
| D3 | **Thông báo kết thúc của con** đi vào lane follow-up của controller: cha rảnh → mở lượt tự động với thông báo; cha đang chạy → chờ lượt hiện tại xong. **Không** steer vào lượt đang chạy | Đúng prime (`followUp`, `when_run_idle`) |
| D4 | Chuỗi thông báo lấy nguyên văn prime: `[child-failed child:<name>]\n\n<error>`, `[child-exited: cancelled child:<name>]\n\n<reason>`, `[child-exited: no-reply child:<name>]\n\nLast assistant text: <text>`. **Khác prime có chủ đích:** `Last assistant text` giữ tối đa 8000 ký tự (`RLM_ANSWER_MAX_CHARS`) thay vì 160, vì con của `ha` chưa có `agent_message` trước Q03 và cha không có kernel thì không `collect` được | Ghi vào SPEC |
| D5 | `delegate` giữ chế độ chặn làm mặc định (như Task của Claude Code) và thêm tham số `wait` (mặc định `true`); `wait:false` trả handle ngay và kết quả về bằng thông báo D4 | Không phá hành vi cũ; model tự chọn |
| D6 | `agent_message` luôn giao kiểu **steer** (prime): đích đang chạy → vào `RunInbox` của đích; cha rảnh → mở lượt tự động; con đã kết thúc → lỗi `the child has finished; spawn a new one` (khác prime: `ha` không đánh thức lại con đã xong) | Không có daemon |
| D7 | Hàng đợi 2 lane `steer`/`follow_up`, chế độ `all`/`one-at-a-time` (mặc định `one-at-a-time`). **Alt+Enter giữ nguyên là xuống dòng** (đã hứa trong README); follow-up nhập bằng `/queue <text>` (alias `/followup`) | Không đổi phím đã có; Windows không phân biệt Shift+Enter (đo ở T01) |
| D8 | `/fork` và `/clone` tạo **task mới** (hội thoại mới hiện riêng trong `/resume`) với `previous_session` trỏ vào điểm rẽ. Bước 0 của Q08 đo xem đường lượt hiện tại có chấp nhận nguồn khác task không; nếu bị chặn (`a fork may not cross into another task`), dùng phương án B ghi trong Q08 | Không phá luật scope của store |
| D9 | `models.json` ở thư mục config người dùng (`<config dir>/models.json`), schema prime (mục Q10); lỗi schema → notice một lần + chỉ dùng model có sẵn | prime |
| D10 | Mã lỗi mới `ErrorCode::RateLimited` (429 và lỗi quota); 5xx vẫn là `ServiceUnavailable`. Sinh lại schema bằng `cargo run -p harness-types --bin generate_schemas --locked` | Cần phân biệt để chờ quota (Q13) và backup (Q12) |
| D11 | Lịch chạy (Q15) lưu `<data dir>/schedules/<task_id>.json`, chỉ chạy khi app đang mở; lần mở sau chạy bù **một lần** cho các lần lỡ (prime gộp lỡ thành một) | Không có daemon |
| D12 | Cổng của `/autonomous` là lệnh **người dùng tự gõ**, chạy như `!!cmd` (không qua panel duyệt, không vào transcript model ngoài phần output bị cắt 6000 ký tự) | prime; người dùng là tác giả lệnh |

## 5. Kiến trúc dự kiến (file mới/sửa)

```text
crates/harness-cli/src/interactive/
  store_lease.rs      (mới, Q01)  SharedStore + StoreLease
  agents.rs           (mới, Q01)  SessionAgents: scheduler, registry con, thông báo, message bus
  delegation.rs       (sửa)       InteractiveWorkerBackend dùng lease; RlmChildren đọc registry của SessionAgents
  queue.rs            (mới, Q05)  InputQueue 2 lane + chế độ
  side_question.rs    (mới, Q07)  /btw
  branches.rs         (mới, Q08)  fork/clone/tree
  export_html.rs      (mới, Q09)
  custom_models.rs    (mới, Q10)  models.json
  routing.rs          (mới, Q12)  aux/backup/image
  autonomous.rs       (mới, Q14)
  schedules.rs        (mới, Q15)
  controller.rs, service.rs, events.rs, commands.rs, tui/widgets/status.rs (sửa)
crates/harness-types/src/error.rs      (Q13: RateLimited)
crates/harness-providers/src/lib.rs    (Q13: 429 → RateLimited)
crates/harness-runtime/src/lib.rs      (Q12: summarizer riêng; Q13: chờ quota)
crates/harness-cli/tests/interactive_terminal.rs  (ScriptedSse + ca PTY q01_… q15_…)
```

## 6. Work items

Mỗi item: **Mục tiêu · Tham chiếu · Bước · Test tự động · Thử tay · Xong khi · Bẫy.** Ước lượng: S ≈ 1 lượt coding, M ≈ 2–3, L ≈ 4+.

### Q00 — Chuẩn bị: `ScriptedSse` và SPEC (S)

1. Tạo `docs/specs/HA_PRIME.vi.md` theo `docs/implementation-next/TEMPLATES.vi.md` mục 1; với mỗi Q ghi `reuse_verified` / `adapt` / `missing` sau khi mở code thật.
2. Trong `crates/harness-cli/tests/interactive_terminal.rs` thêm `struct ScriptedSse` (mục 3.4e): `ScriptedSse::start(script: impl Fn(&Value) -> Reply + Send + Sync + 'static) -> Self`, `endpoint() -> String`, `requests() -> Vec<(Instant, Value)>`, `stop()`. `enum Reply { Text(String), ToolCalls(Vec<(String, Value)>), Status(u16, String), Garbage, Delay(Duration, Box<Reply>) }`.
3. Ca PTY tự kiểm `q00_scripted_sse_serves_text_and_tool_calls`: kịch bản trả tool `list_files` ở request đầu, text `"done"` ở request thứ hai; gõ `go\r`, chờ `done`; assert server nhận đúng 2 request và request thứ hai có một message role `tool`.

Xong khi: ca q00 xanh qua script.

### Q01 — Store dùng chung và agent con cấp session (L)

**Mục tiêu:** agent con không còn chết khi lượt của cha kết thúc; chưa đổi cách báo kết quả (Q02 làm).

**Tham chiếu:** D1, D2; prime agent-session.ts `_startRlmChildRun` (con chạy trong task tách rời).

**Bước:**
1. **Đo trước.** Viết test `q01_a_turn_ending_cancels_its_children_today` (tạm, đặt trong `delegation.rs` real_worker_tests): dựng `DelegateHost` với provider `Slow{2s}`, `rlm.run` một con, gọi `host.shutdown()` ngay → assert con bị huỷ. Test này **xanh ở baseline** và chứng minh hành vi cũ; ở bước 7 đổi kỳ vọng thành "con vẫn chạy" (ghi lý do).
2. `store_lease.rs`: `pub struct SharedStore { dir: PathBuf, inner: tokio::sync::Mutex<Option<(Arc<SqliteStore>, usize)>> }`, `pub async fn lease(&self) -> Result<StoreLease, HarnessError>`; `StoreLease` giữ `Arc<SqliteStore>` và khi `Drop` giảm đếm; về 0 thì `tokio::spawn` đóng store (lấy `Arc::try_unwrap`, nếu còn tham chiếu thì đợi và thử lại tối đa 5 s rồi ghi notice). Unit test: `q01_a_lease_opens_once_and_closes_after_the_last_drop` (hai lease → một lần mở; drop cả hai → khoá file được nhả: mở `SqliteStore::open_writer` từ đường khác thành công).
3. `AgentSessionService`: thêm field `store: Arc<SharedStore>`. Trong `run_turn` thay `writer_gate.lock()` + `open_writer` bằng `let lease = store.lease().await?` (giữ `writer_gate` cho `/rename` như cũ). Chỗ đóng store cuối lượt đổi thành `drop(lease)`. **Chạy lại toàn bộ `cargo test -p harness-cli --bin ha`** — đây là bước dễ vỡ nhất; không đi tiếp khi chưa xanh.
4. `agents.rs`: `pub struct SessionAgents` chứa `Arc<WorkerScheduler>`, `DelegateDispatcher`, registry `Mutex<Vec<ChildRecord>>` (`task_id, name, role, model, status, started, finished, answer, error, tool_calls, progress_notes`). Chuyển phần tạo scheduler ra khỏi `DelegateHost::new`. `DelegateHost` mỗi lượt giờ chỉ là "view" của `SessionAgents` (tools/dispatcher/rlm_requests) và **không** huỷ con trong `shutdown` trừ khi `parent_cancellation` là huỷ session.
5. `InteractiveWorkerBackend`: nhận `Arc<SharedStore>` thay `Arc<SqliteStore>`, lấy lease lúc bắt đầu `dispatch`, giữ tới khi con xong.
6. Huỷ con khi: `/new`, `/resume` sang task khác, `/quit`, và lệnh mới `/agents stop <name|all>`. Ctrl+C: huỷ lượt, **không** huỷ con (D2). Thêm `SessionPort::stop_agents(selector)`.
7. Đổi kỳ vọng test bước 1 thành `q01_children_outlive_the_turn_that_spawned_them`: sau `shutdown()` của lượt, con vẫn `running`, 2 s sau là `done`.

**Test tự động:** `q01_a_lease_opens_once_and_closes_after_the_last_drop`, `q01_children_outlive_the_turn_that_spawned_them`, `q01_new_and_quit_cancel_running_children` (sau `/new` registry trống, con nhận huỷ), `q01_ctrl_c_cancels_the_turn_but_not_its_children` (controller test: `interrupt()` không gọi `stop_agents`), PTY `q01_pty_a_child_keeps_running_after_the_turn_ends` (cha gọi `ipython` với `rlm.spawn` rồi trả lời "spawned" và kết thúc; con là request chậm 3 s; sau khi thấy "spawned", `/agents` hiện con `running`, 4 s sau `/agents` hiện `completed`).

**Thử tay:**

| Bước | Thấy |
|---|---|
| Gõ `hãy dùng ipython spawn một agent con đọc README rồi kết thúc lượt ngay` | Lượt kết thúc, footer về idle |
| `/agents` ngay sau đó | Một con `running` |
| Chờ, `/agents` lần nữa | Con `completed` kèm số bước, số tool |
| Spawn con khác, `/new` | `/agents` trống, không có lỗi |

**Xong khi:** mọi test trên xanh; i05 PTY (thoát khi đang chạy nhả store) vẫn xanh.
**Bẫy:** đừng giữ lease trong `AgentSessionService` lâu dài (khoá store khi rảnh làm i05 đỏ); đừng mở writer thứ hai "cho nhanh".

### Q02 — Spawn không chặn và thông báo đánh thức cha (M)

**Mục tiêu:** khi con kết thúc, cha được báo bằng một lượt tự động, không cần `rlm.collect`.

**Tham chiếu:** D3, D4, D5; prime messages.ts (~269–303), agent-session.ts (~5752–5838, 12972–12986).

**Bước:**
1. `events.rs`: `SessionEvent::ChildSettled { name: String, notice: String }`.
2. `agents.rs`: khi một con chuyển sang `done`/`error`/`cancelled`, dựng chuỗi theo D4 và gửi `ChildSettled`. Hàm thuần `fn terminal_notice(record: &ChildRecord) -> Option<String>` — unit test mọi nhánh: `q02_terminal_notice_texts_match_prime` (failed, cancelled có/không lý do, no-reply có/không text, xoá bởi cha → lý do `Deleted by parent orchestrator`).
3. `controller.rs`: field `pending_notices: VecDeque<String>`. Nhận `ChildSettled`: nếu rảnh (không lượt chạy, không approval, không câu hỏi) → `dispatch(notice, true)`; ngược lại đẩy vào `pending_notices`. Trong handler `RunTerminal`, sau `queued_input` và **trước** heartbeats, lấy một notice ra chạy. Nếu lượt vừa kết thúc do người dùng Ctrl+C: **giữ** notice, chỉ chạy sau khi người dùng gửi tin tiếp theo và lượt đó xong (prime demote sau abort).
4. `delegate` thêm tham số `"wait": {"type":"boolean"}` (schema ở `DelegateCatalog::schemas`); `wait:false` → trả `{"status":"started","name":…,"task_id":…}` ngay; kết quả về bằng D4.
5. `rlm.run` (spawn) đã trả khi được nhận — xác nhận bằng test, không đổi.

**Test tự động:** `q02_terminal_notice_texts_match_prime`; controller `q02_a_notice_wakes_an_idle_parent` (gửi `ChildSettled` khi rảnh → `port.submissions` có đúng chuỗi, history là `Automatic`); `q02_a_notice_waits_for_the_running_turn` (đang chạy → không submit, không steer; sau `RunTerminal` mới submit); `q02_a_notice_after_ctrl_c_waits_for_the_next_user_turn`; delegation `q02_delegate_wait_false_returns_before_the_child_finishes` (provider `Slow{2s}`: dispatch trả trong < 500 ms với `status:"started"`); PTY `q02_pty_parent_is_woken_when_its_child_fails` (con nhận `Reply::Garbage` → màn hình hiện `[child-failed child:` và cha có request mới chứa chuỗi đó).

**Thử tay:** spawn một con rồi kết thúc lượt → vài giây sau tự xuất hiện lượt mới bắt đầu bằng `[child-exited: no-reply child:…]` và cha tóm tắt kết quả.

**Xong khi:** các test xanh; lượt tự động không lặp vô hạn (con xong → một lượt; lượt đó không tạo notice mới nếu không spawn).
**Bẫy:** đừng steer notice vào lượt đang chạy; đừng gửi hai notice cho một con (test: chuyển trạng thái hai lần chỉ một `ChildSettled`).

### Q03 — `agent_message`, `agent_observe`, `progress_note` (M)

**Tham chiếu:** D6; prime SK/agent-message, agent-messages.ts (~18–75, 397–408), daemon-mode.ts (~6449–6483), agent-session.ts (~11406–11415, 12869–12875).

**Bước:**
1. Gỡ ba chỗ từ chối trong `delegation.rs` (~1042, ~1086) — `rlm.create_session` **vẫn từ chối** (ngoài phạm vi).
2. Con có kernel/host request riêng? Hiện con không có `ipython`. Thêm cho con một tool **gốc** `agent_message` (schema: `message`, `receiver_role` ∈ parent|sibling|child, `receiver_name`) thay vì kernel — ghi khác biệt vào SPEC. Cha dùng skill Python `agent_message` (đã có) qua host request.
3. Giới hạn nguyên văn prime: tối đa 16 384 ký tự; tối đa 20 tin chờ mỗi đích; token bucket dung lượng 3, hồi 1 token/1000 ms; lỗi phạm vi `Agent reach is limited to parent, siblings, and children`.
4. Nội dung giao: `[agent-message from <relationship>:<senderName>]\n\n<message>`. Đích đang chạy → `RunInbox::steer` của đích (con cần gắn `RunInbox` khi chạy — thêm trong `InteractiveWorkerBackend`); cha rảnh → `SessionEvent::AgentMessage{text}` → controller mở lượt tự động; con đã xong → lỗi D6. Trả biên nhận `{id, source:"agent_message", target, from, message, deliveryStatus:"delivered"|"queued", deliveryMode:"steer"}`.
5. Tin của con gửi cha đếm vào `replied_since_task`; con đã trả lời cha thì **không** tạo notice no-reply (prime ~12972).
6. `agent_observe.list_agents()` trả `{current, agents:[{sessionId, sessionName, relationship, status, activity, isSessionActive, latestMessage}]}` từ registry.
7. `progress_note`: ≤ 512 ký tự, ≥ 10 s giữa hai note của một con (quá nhanh → `{accepted:false, retry_after_ms}`), giữ 5 note gần nhất, hiện trong `list_subagents` (`progress_note`) và `/agents`. **Không** đưa vào ngữ cảnh cha.

**Test tự động:** `q03_message_limits_follow_prime` (16 385 ký tự bị từ chối; tin thứ 4 trong 1 s bị giới hạn; tin thứ 21 chờ bị từ chối); `q03_a_child_message_steers_a_running_parent`; `q03_a_child_message_wakes_an_idle_parent`; `q03_a_message_to_a_finished_child_is_refused`; `q03_a_child_that_replied_gets_no_no_reply_notice`; `q03_progress_notes_are_throttled_and_kept_five`; PTY `q03_pty_child_reports_to_its_parent` (con gọi tool `agent_message` gửi "found it" → cha có request chứa `[agent-message from child:`).

**Thử tay:** yêu cầu cha spawn con "đọc file X và gửi kết quả cho cha bằng agent_message" → thấy lượt cha tự bắt đầu với `[agent-message from child:…]`.

### Q04 — Model cho agent con và `/agents` quản lý (M)

**Tham chiếu:** prime `_resolveRlmSubagentModel` (~12545), settings `subagentDefaultModel`.

**Bước:**
1. Config: khoá `[agents] default_model = "provider/id"` (trong `ResolvedConfig`, xem `interactive/config.rs`); `ha config explain` hiện nó.
2. `rlm.spawn(model=…)` hiện bị từ chối khi khác model cha (`RlmChildren::spawn` ~886) — cho phép. Thứ tự: tham số `model` → `default_model` → model cha. Tìm trong catalog: khớp chính xác `provider/id` (không phân biệt hoa thường) → khớp tên ngắn duy nhất → lỗi `Requested subagent model "<x>" is not available` (không âm thầm dùng model cha). Model phải có credential (dùng `model_options` hiện có để biết).
3. Dựng provider cho con bằng `build_provider` với config của model đó.
4. `/agents` thành danh sách: tên · role · model · trạng thái · thời gian · số tool · progress note mới nhất; lệnh con `/agents stop <name|all>`, `/agents show <name>` (in answer/error đầy đủ vào `/more`).

**Test tự động:** `q04_spawn_model_resolution_order` (tham số > config > cha), `q04_an_unknown_model_fails_the_spawn`, `q04_agents_lists_status_model_and_note`, PTY `q04_pty_agents_stop_cancels_a_child` (con chậm; `/agents stop all` → thông báo `[child-exited: cancelled child:…]`).

### Q05 — Hàng đợi 2 lane: steer và follow-up (M)

**Tham chiếu:** D7; prime interactive-mode.ts (~7755–7785, 8238–8277), settings `steeringMode`/`followUpMode`, pump (~6916–6934).

**Bước:**
1. `queue.rs`: `pub enum Lane { Steer, FollowUp }`, `pub enum QueueMode { All, OneAtATime }`, `pub struct InputQueue { items: Vec<(Lane, String)>, steer_mode, follow_up_mode }` với `push`, `take_next(lane) -> Vec<String>` (theo mode: `All` lấy **mọi** item cùng lane gộp bằng `\n\n`, `OneAtATime` lấy một), `remove(index)`, `move_up/down(index)`, `iter()`. Unit test thuần cho từng hàm.
2. `controller.rs`: thay `queued_input` bằng `queue: InputQueue`. Enter khi đang chạy: như cũ thử `service.steer`; nếu steer bị từ chối (lượt chưa sẵn sàng) → đẩy lane Steer. `/queue <text>` (alias `/followup`, thêm vào `commands.rs`) → lane FollowUp. Sau `RunTerminal`: lane Steer trước, rồi FollowUp, rồi notice (Q02), rồi heartbeat.
3. Hiển thị dưới dòng trạng thái (`tui/widgets/status.rs`): mỗi item một dòng `Steering: <text>` / `Follow-up: <text>` (cắt theo bề rộng), cuối cùng `╰─ alt+up to browse and edit queued messages` (hint chỉ hiện khi Q06 xong).
4. Config `[queue] steering_mode`, `follow_up_mode` = `"all"|"one-at-a-time"` (mặc định `one-at-a-time`); `ha config explain` hiện.
5. Esc/Ctrl+C khi đang chạy: **giữ** hàng đợi (hiện tại `interrupt()` xoá queued input — đổi, ghi lý do: prime giữ hàng đợi sau abort và chờ người dùng).

**Test tự động:** `q05_queue_take_next_respects_the_mode` (thuần); `q05_follow_up_waits_for_the_turn_and_runs_after_steers`; `q05_all_mode_batches_the_follow_ups_into_one_turn`; `q05_the_queue_survives_an_interrupt`; `q05_status_lists_queued_items` (render test giống `status.rs` tests); PTY `q05_pty_follow_up_runs_after_the_turn` (lượt chậm 3 s; gõ `/queue next thing\r` → thấy `Follow-up: next thing`; sau khi lượt xong, server nhận request chứa `next thing`).

**Thử tay:** hỏi một câu dài; trong lúc chạy gõ `/queue tóm tắt lại` và `/queue dịch sang tiếng Anh` → thấy 2 dòng `Follow-up:`; lượt xong thì lần lượt chạy từng cái (mode mặc định).

### Q06 — Xem/sửa hàng đợi và cất nháp (S/M)

**Tham chiếu:** prime queue-selection.ts, interactive-mode.ts (~7985–7997, 4768–4803).

**Bước:**
1. **Đo phím trước** bằng PTY: Alt+Up, Alt+Down, Ctrl+Alt+Up, Ctrl+S có tới được app qua ConPTY không (ghi kết quả vào SPEC). Phím nào không tới → dùng lệnh thay thế `/queue list`, `/queue edit <n>`, `/queue drop <n>`, `/queue up <n>`, `/stash`.
2. Alt+Up/Down: đưa item vào ô soạn để sửa (newest first); Enter lưu lại vào vị trí cũ; ô trống + Enter = xoá item. Header `<lane> <n> · alt+up/alt+down browse · ctrl+alt+up/down reorder · enter steers · empty deletes`.
3. Ctrl+S: ô soạn có chữ → cất (`Stashed prompt`; đã có nháp → `Prompt stash already has a draft`); ô trống → khôi phục (`Restored stashed prompt` / `No prompt to stash`). Nháp chỉ trong bộ nhớ.

**Test tự động:** controller `q06_browse_edits_and_deletes_queued_items`, `q06_stash_and_restore_follow_prime_messages`; PTY `q06_pty_stash_round_trip`.

### Q07 — `/btw` câu hỏi bên lề (M)

**Tham chiếu:** prime CA/core/side-question.ts (toàn file).

**Bước:**
1. `side_question.rs`: `pub async fn ask(provider, system_prompt, history: Vec<ProviderMessage>, side_turns: &[(String, String)], question) -> Result<String>`: gửi system prompt của phiên + lịch sử hội thoại (dùng `conversation_history` của session hiện tại) + các lượt bên lề trước + message user `<side_question>\n{body}\n</side_question>`. Lượt đầu `body` = `SIDE_QUESTION_INSTRUCTION` (chép nguyên văn prime, dòng ~30–31) + câu hỏi. **Không** gửi tool (hoặc gửi tool nhưng mọi tool call bị trả lỗi `Tools are deactivated in this side thread. Answer from the conversation context.` và tối đa 3 vòng — chọn cách thứ nhất nếu provider cho phép gọi không tool; ghi vào SPEC).
2. Không ghi gì vào store, không vào history của phiên, không tính vào context.
3. UI: panel riêng (tái dùng khung `/more`), tiêu đề `btw`; khi đang chạy hint `esc to cancel and return to session`; xong hint `reply to follow up · esc to return to session`. Chỉ một câu hỏi một lúc (`Wait for the current side question to finish or cancel it first.`). Chạy được cả khi lượt chính đang chạy. Alias `/side`.

**Test tự động:** `q07_side_question_sends_history_and_no_tools` (provider giả ghi request: có message của lượt trước, có `<side_question>`, `tools` rỗng); `q07_side_question_is_not_recorded` (sau `/btw`, lượt tiếp theo không thấy câu hỏi trong lịch sử); `q07_one_side_question_at_a_time`; PTY `q07_pty_btw_answers_in_a_panel`.

**Thử tay:** sau vài lượt, gõ `/btw lúc nãy mình đã sửa file nào?` → panel hiện câu trả lời; Esc → về phiên; hỏi tiếp một câu thường → model không nhắc tới câu `/btw`.

### Q08 — `/fork`, `/clone`, `/tree` (L)

**Tham chiếu:** D8; prime interactive-mode.ts (~9210–9380), agent-session-runtime.ts (~500–620), session-manager.ts (~2568–2640).

**Bước 0 — đo:** viết test `q08_probe_cross_task_source` gọi đường tạo lượt (`run_turn_continuing` hoặc tương đương mà `run_turn` dùng) với `source` thuộc task A và task mới B. Ghi kết quả vào SPEC:
- **Phương án A (nếu chấp nhận):** fork = task mới, `previous_session` = session trước điểm rẽ. `conversation_history` đi ngược qua link vào task A tự nhiên.
- **Phương án B (nếu bị chặn):** fork = task mới với session "gốc" rỗng; lưu `set_session_setting(new_task, "forked_from", "<session_id>")`; sửa `conversation_history` để khi đi tới đầu task mà có `forked_from` thì đi tiếp từ session đó. Không nới luật `open_forked_session`.

**Bước:**
1. `branches.rs`: `fn user_turns(history) -> Vec<(SessionId, String)>` (mỗi lượt người dùng + session của nó).
2. `/fork`: picker liệt kê câu người dùng (mới nhất chọn sẵn); chọn → tạo nhánh ngay **trước** lượt đó, đưa chữ của lượt đó vào ô soạn, notice `Forked to new session`. Không có lượt nào → `No messages to fork from`.
3. `/clone`: nhánh tại lượt cuối, ô soạn trống, `Cloned to new session`; chưa có lượt → `Nothing to clone yet`.
4. `/tree`: cây các lượt của hội thoại hiện tại và các nhánh fork từ nó (tìm task có link/`forked_from` vào session của hội thoại này). Chọn một nút → chuyển (`previous_session` = nút đó; nếu nút là lượt người dùng thì lấy session trước nó và đưa chữ vào ô soạn — như prime `navigateTree`). Chọn nút hiện tại → `Already at this point`. Chưa làm tóm tắt nhánh (mục 11).
5. Tên nhánh mặc định: tên hội thoại gốc + ` (fork)`.

**Test tự động:** `q08_probe_cross_task_source` (giữ lại như test hồi quy), `q08_fork_starts_before_the_chosen_turn` (service test mục 3.4d: 3 lượt; fork ở lượt 2 → lượt mới thấy lịch sử chỉ lượt 1), `q08_clone_keeps_the_whole_history`, `q08_forks_are_separate_conversations_in_resume`, `q08_tree_switch_changes_what_the_next_turn_sees`; PTY `q08_pty_fork_then_answer`.

**Thử tay:** 3 lượt (A, B, C); `/fork` chọn B → ô soạn có chữ B; sửa rồi gửi → model chỉ nhớ A; `/resume` thấy hai hội thoại; `/tree` thấy nhánh.

### Q09 — Xuất HTML (S/M)

**Tham chiếu:** prime CA/core/export-html/index.ts (~138–266).

**Bước:**
1. `export_html.rs`: một file HTML **tự chứa** (CSS inline, không tải gì từ mạng, không JS ngoài): tiêu đề, model, ngày; từng lượt người dùng/trả lời (markdown → HTML bằng renderer tối giản: đoạn văn, code fence có class ngôn ngữ, inline code; mọi chữ được escape HTML), tool call gọn (tên + tóm tắt, output trong `<details>`).
2. `/export` nhận `.html` (và mặc định đổi thành `.html` như prime? **Không** — giữ mặc định `session-export.md` cho tương thích; ghi vào SPEC). Dùng lại khâu che secret và giới hạn 1 MiB của `render_session_export`.

**Test tự động:** `q09_html_export_is_self_contained_and_escaped` (không có `<script src`, `<link href="http`; chuỗi `<script>alert(1)</script>` trong câu người dùng xuất hiện đã escape), `q09_html_export_redacts_secrets`.

### Q10 — `models.json` (M)

**Tham chiếu:** D9; prime model-registry.ts (~156–213, 649–770).

**Bước:**
1. `custom_models.rs`: đọc `<config dir>/models.json` (cho phép comment `//` — tự bỏ trước khi parse, hoặc dùng crate đã có trong lockfile nếu có; không thêm crate). Schema: `{ "providers": { "<id>": { "name"?, "baseUrl"?, "apiKey"?, "api"?, "headers"?, "models"?: [ModelDef], "modelOverrides"?: { "<modelId>": Override } } } }`. ModelDef: `id` (bắt buộc), `name`, `api`, `baseUrl`, `reasoning`, `input` (`text`/`image`), `cost{input,output,cacheRead,cacheWrite}` (đủ 4 khi có), `contextWindow`, `maxTokens`.
2. Kiểm tra như prime: provider không phải built-in mà có models thì phải có `baseUrl` và `apiKey` (tên biến môi trường hoặc giá trị); `contextWindow`/`maxTokens` > 0; `api` ∈ các protocol `ha` hỗ trợ (`openai_chat`, `anthropic_messages`, `openai_responses`…). Lỗi → notice một lần `models.json: <lỗi> — using built-in models only`.
3. Mặc định thiếu trường: `name = id`, `reasoning = false`, `input = ["text"]`, cost 0, `contextWindow = 128000`, `maxTokens = 16384`.
4. Gộp vào `Catalog::load` (providers.rs): trùng `provider/id` thì model tuỳ chỉnh thay built-in; override chỉ ghi đè trường có mặt.
5. Provider tuỳ chỉnh xuất hiện trong `/model` khi có key.

**Test tự động:** `q10_custom_model_is_added_with_defaults`, `q10_override_replaces_only_given_fields`, `q10_invalid_file_falls_back_with_one_notice`, `q10_custom_provider_needs_base_url_and_key`; PTY `q10_pty_custom_model_is_selectable` (models.json trỏ về `ScriptedSse`; `/model` chọn nó; gửi tin → server nhận đúng model id).

### Q11 — `/scoped-models` và đổi nhanh (S/M)

**Bước:**
1. Config `[models] enabled = ["deepseek/*", "openai/gpt-5*:high"]` (glob trên `provider/id` hoặc `id`, không phân biệt hoa thường, hậu tố `:level` tuỳ chọn).
2. `/scoped-models`: picker nhiều lựa chọn (Space bật/tắt, Enter lưu vào config người dùng).
3. **Đo phím** Alt+M / Shift+Alt+M qua PTY; nếu tới được: đổi sang model kế/trước trong danh sách scoped có credential (cần ≥ 2), áp dụng như `/model` (lượt sau hoặc giữa lượt qua `LiveTurn`). Không tới được → lệnh `/model next`, `/model prev`.

**Test tự động:** `q11_scope_patterns_match_like_prime`, `q11_cycle_skips_models_without_credentials`, PTY `q11_pty_cycle_changes_the_footer_model`.

### Q12 — Định tuyến model: phụ trợ, dự phòng, ảnh (M/L)

**Tham chiếu:** prime `_resolveAuxiliaryModel` (~9451), `_handleRetryableError` (~13315–13420), image-model-routing.ts.

**Bước:**
1. Config `[models] auxiliary`, `backup`, `image` (mỗi khoá là `provider/id`).
2. **Phụ trợ:** `RuntimeService` nhận summarizer riêng (`with_summarizer`) dựng từ model `auxiliary` cho compaction và `/refine`. Dùng được khi: có trong catalog, có credential, `contextWindow` ≥ kích thước request tóm tắt. Không dùng được → notice `auxiliaryModel "<x>" unusable for <purpose>; using the session model.` và dùng model phiên.
3. **Dự phòng:** khi một lượt thất bại với lỗi tạm thời (`RateLimited` hoặc `ServiceUnavailable`) **sau khi đã hết lần thử lại**, nếu có `backup` hợp lệ và khác model đang chạy → thử lại lượt ngay với backup (qua `LiveTurn::set_config`), notice `Primary model unavailable (<err>) — retrying on backup model <x>...`. Lượt sau quay về model chính; khi quay về thành công, notice `Primary provider recovered — back on <x>`.
4. **Ảnh:** trước khi gửi request có ảnh, nếu `Model.input` của model phiên không có `"image"`: có `image` hợp lệ → lượt đó dùng model ảnh (chỉ lượt đó); không có → lỗi rõ ràng `This model does not accept images; set [models] image in config` (không gửi ảnh vào model text).

**Test tự động:** `q12_auxiliary_model_writes_the_summary` (hai provider giả: summarizer gọi provider phụ, không gọi provider chính); `q12_unusable_auxiliary_falls_back_with_notice`; `q12_backup_takes_over_after_retries_and_hands_back`; `q12_images_route_to_the_image_model`; `q12_images_without_an_image_model_fail_clearly`.

### Q13 — Mã lỗi `RateLimited` và chờ hạn mức (M)

**Tham chiếu:** D10; prime provider-retry.ts, interactive-mode.ts (~6386–6402), settings `retry.provider.waitForUsage`.

**Bước:**
1. `harness-types/src/error.rs`: thêm `RateLimited` (retry class `Transient`, exit code như `ServiceUnavailable`); cập nhật `as_str`/`from_str`; chạy `generate_schemas`; test contract/schema xanh (`p0_f03`).
2. `harness-providers/src/lib.rs` `http_status_error`: 429 → `RateLimited`. Thân lỗi có cụm quota rõ ràng từ provider (mã lỗi JSON `insufficient_quota`, `rate_limit_exceeded`) → `RateLimited` — **dựa trên mã lỗi có cấu trúc trong JSON, không dò câu chữ tự do**.
3. Runtime: với `RateLimited`, không thất bại sau 3 lần mà vào **chế độ chờ**: backoff từ 1 s, gấp đôi, tối đa 300 s mỗi lần, tổng tối đa 15 phút (hằng số lấy từ prime: `baseDelayMs 1000, maxDelayMs 300000, maxAttempts 30, maxWaitMs 900000`); `Retry-After` của server được ưu tiên nếu ≤ giới hạn. Mỗi lần chờ phát sự kiện để UI hiện `Waiting for provider usage to recover (a/max), next check in Ns... (esc to cancel)`. Esc/Ctrl+C huỷ ngay. Quá 15 phút → thất bại `Provider recovery wait gave up after N pings`.
4. Status bar: hiện dòng chờ thay spinner thường.
5. Không làm "park qua đêm" (cần lịch bền — mục 11).

**Test tự động:** `q13_429_maps_to_rate_limited`; `q13_rate_limit_waits_and_recovers` (provider giả: 2 lần 429 rồi text; dùng thời gian giả hoặc hằng số test nhỏ qua `RuntimeConfig`); `q13_rate_limit_wait_is_cancelable`; `q13_wait_gives_up_after_the_bound`; PTY `q13_pty_waiting_line_is_shown` (server trả 429 với `Retry-After: 2` hai lần → thấy `Waiting for provider usage to recover`, sau đó thấy câu trả lời).

### Q14 — `/autonomous` với cổng kiểm tra (M)

**Tham chiếu:** D12; prime autonomous.ts (~53–60, 390–415), agent-session.ts (~1225–1240, 2621).

**Bước:**
1. `autonomous.rs`: `struct AutonomousConfig { max_continuations: 3, max_turns: 12, max_tokens: 80_000, timeout: 30 phút, gates: Vec<String>, gate_retries: 3, gate_timeout: 5 phút }` (mặc định prime) và `struct AutonomousState { continuations, turns, tokens, started, gate_attempts, last_failure_snapshot }`. Hàm thuần `fn decide(state, config, last_stop, gate_results) -> Decision { Stop(reason) | Continue(prompt) }` theo đúng thứ tự prime (dừng nếu tắt/lỗi/huỷ; có gate → chạy lần lượt; tất cả pass → dừng `not_needed`; fail → attempt+1, quá `gate_retries` → dừng `retry_exhausted`; không gate → tiếp tục tới khi chạm giới hạn; kiểm tra giới hạn theo thứ tự continuations → turns → tokens → thời gian).
2. Cú pháp lệnh: `/autonomous [status|off]`, `/autonomous on [--max-continuations <n|unlimited>] [--max-turns <n>] [--max-tokens <n>] [--timeout-ms <n>] [--gate <cmd>]... [--gate-retries <n>] [--gate-timeout-ms <n>]`; alias `enable`/`disable`.
3. Chạy gate: shell giống `!!cmd` (dùng `harness-tools::process`, cwd = workspace, timeout), output cắt 6000 ký tự. Nếu trạng thái git (status + diff + hash file untracked) không đổi so với lần fail trước → không chạy lại, vẫn tính một lần thử.
4. Prompt nguyên văn prime: tiếp tục `[autonomous-continuation]\n\n<continuationPrompt>` (câu mặc định dòng ~53–54 autonomous.ts); gate fail `[autonomous-continuation: gate-failed]\n\nAutonomous quality gate failed (attempt a/max): \`cmd\` <exitText>.\n\nOutput:\n…\nContinue working. Fix the failure, then produce terminal evidence. Timestamp: <ISO>.`
5. Nối vào controller sau `RunTerminal` (cùng chỗ `maybe_continue`/`continue_goal`; `/autonomous` và `/goal` không bật cùng lúc — bật cái này tắt cái kia kèm notice). Có agent con đang chạy → giữ, không tiếp tục (prime keep-alive), tối đa 25 phút.
6. `/autonomous status`: `[autonomous-status: on|off]` + `Continuations: x/y. Turns: … Tokens: … Time: Ns/Ms. Gates: …` như prime; khi dừng in một notice nêu lý do (prime TUI không in — `ha` in, ghi vào SPEC).

**Test tự động:** bảng test thuần cho `decide` — `q14_decide_follows_prime_order` (mỗi nhánh một case); `q14_gate_failure_prompt_matches_prime`; `q14_unchanged_worktree_skips_the_gate`; controller `q14_autonomous_continues_until_the_gate_passes` (gate giả: fail rồi pass); PTY `q14_pty_gate_passes_after_one_fix` (gate là `pwsh -c "exit (Test-Path ok.txt) ? 0 : 1"`; kịch bản model: lượt 2 gọi `write_file ok.txt`).

**Thử tay:** `/autonomous on --gate "cargo test -p mycrate"` rồi giao việc sửa test → agent tự chạy tiếp sau mỗi lần dừng tới khi gate xanh hoặc chạm giới hạn; `/autonomous status` hiện số đếm.

### Q15 — Lịch chạy bền (M)

**Tham chiếu:** D11; prime cron-jobs.ts (~27–48, 1155–1215, 1450–1556, 1749–1775); `ha` daemon/schedule.rs `next_after`.

**Bước:**
1. `schedules.rs`: `struct Job { id, status: active|paused|completed|cancelled, delivery: steer|follow_up, label, prompt, schedule: {kind: once|cron|interval, expression, interval_ms}, created_at, next_run_at, last_run_at, last_error, run_count }` — trường theo prime.
2. Parser như prime: `in N m|h|d` → once; `every|each N s|m|h` → interval (≥ 10 s); `at <ISO>` → once (phải ở tương lai); còn lại → cron 5 trường hoặc `@hourly/@daily/@weekly/@monthly`. Cron: tái dùng `daemon::schedule::next_after` **nếu** nó tính được theo giờ máy (local); nếu không, viết parser cron nhỏ theo prime (~1450–1556: `*`, `a-b`, `/step`, danh sách, thứ 0–7, quét từng phút tối đa một năm) — ghi lựa chọn vào SPEC.
3. Lưu `<data dir>/schedules/<task_id>.json`, ghi nguyên tử (ghi file tạm rồi rename).
4. Khi app mở: nạp lịch của hội thoại hiện tại; job lỡ nhiều lần → chạy **một** lần bù; `next_run_at` tính lại từ **thời điểm chạy**.
5. Giao: cron thường → follow-up; heartbeat mặc định steer (như hiện có). Hoãn khi đang compaction/đang chờ quota.
6. Lệnh `/schedule list|add <spec> -- <prompt>|pause <id>|resume <id>|cancel <id>`; `/heartbeat` hiện có nhận thêm cú pháp mới và được lưu bền (giữ `every 5m` mặc định).
7. Tool cho model: dùng lại host request `rlm_heartbeat.*` hiện có; không thêm tool mới trong item này.

**Test tự động:** `q15_parser_accepts_prime_forms` (bảng: `in 10m`, `every 30s`, `at 2030-01-01T09:00:00`, `0 9 * * 1-5`, `@daily`, và các dạng sai); `q15_cron_next_run_is_local_time`; `q15_missed_runs_collapse_into_one`; `q15_jobs_survive_a_restart` (ghi file, dựng lại `Schedules` từ file); controller `q15_a_due_follow_up_job_waits_for_the_turn`; PTY `q15_pty_in_2s_job_fires` (`/schedule add in 2s -- say hi` → sau ~2 s có lượt tự động `say hi`).

**Thử tay:** `/schedule add "every 1m" -- kiểm tra git status` → mỗi phút một lượt tự động; thoát rồi mở lại → `/schedule list` vẫn còn job; tắt máy 5 phút rồi mở → chỉ một lượt bù.

## 7. Acceptance QV01–QV30

| ID | Kiểm chứng | Item | Test chính |
|---|---|---|---|
| QV01 | Kịch bản SSE chạy được trong PTY | Q00 | `q00_scripted_sse_serves_text_and_tool_calls` |
| QV02 | Một writer, nhả khoá khi rảnh | Q01 | `q01_a_lease_opens_once_and_closes_after_the_last_drop`, i05 |
| QV03 | Con sống quá lượt | Q01 | `q01_children_outlive_the_turn_that_spawned_them`, PTY q01 |
| QV04 | `/new`, `/quit`, `/agents stop` huỷ con; Ctrl+C không | Q01/Q04 | `q01_new_and_quit_cancel_running_children`, `q01_ctrl_c_cancels_the_turn_but_not_its_children` |
| QV05 | Chuỗi thông báo đúng prime | Q02 | `q02_terminal_notice_texts_match_prime` |
| QV06 | Thông báo đánh thức cha rảnh, chờ cha bận | Q02 | `q02_a_notice_wakes_an_idle_parent`, `q02_a_notice_waits_for_the_running_turn` |
| QV07 | `delegate wait:false` không chặn | Q02 | `q02_delegate_wait_false_returns_before_the_child_finishes` |
| QV08 | Giới hạn tin nhắn như prime | Q03 | `q03_message_limits_follow_prime` |
| QV09 | Tin của con tới cha (steer / đánh thức) | Q03 | `q03_a_child_message_steers_a_running_parent`, PTY q03 |
| QV10 | progress note bị giới hạn, không vào ngữ cảnh cha | Q03 | `q03_progress_notes_are_throttled_and_kept_five` |
| QV11 | Thứ tự chọn model cho con | Q04 | `q04_spawn_model_resolution_order` |
| QV12 | Follow-up chạy sau lượt, theo mode | Q05 | `q05_follow_up_waits_for_the_turn_and_runs_after_steers`, `q05_all_mode_batches_the_follow_ups_into_one_turn` |
| QV13 | Hàng đợi còn sau Ctrl+C | Q05 | `q05_the_queue_survives_an_interrupt` |
| QV14 | Sửa/xoá hàng đợi, stash | Q06 | `q06_*` |
| QV15 | `/btw` không ghi vào phiên, không tool | Q07 | `q07_side_question_sends_history_and_no_tools`, `q07_side_question_is_not_recorded` |
| QV16 | Fork bắt đầu trước lượt được chọn | Q08 | `q08_fork_starts_before_the_chosen_turn` |
| QV17 | Nhánh là hội thoại riêng trong `/resume` | Q08 | `q08_forks_are_separate_conversations_in_resume` |
| QV18 | Chuyển nhánh bằng `/tree` | Q08 | `q08_tree_switch_changes_what_the_next_turn_sees` |
| QV19 | HTML tự chứa, đã escape, che secret | Q09 | `q09_*` |
| QV20 | models.json thêm/ghi đè/lỗi | Q10 | `q10_*`, PTY q10 |
| QV21 | Scoped models và đổi nhanh | Q11 | `q11_*` |
| QV22 | Model phụ viết tóm tắt | Q12 | `q12_auxiliary_model_writes_the_summary` |
| QV23 | Model dự phòng tiếp quản rồi trả lại | Q12 | `q12_backup_takes_over_after_retries_and_hands_back` |
| QV24 | Ảnh đi model ảnh hoặc lỗi rõ | Q12 | `q12_images_*` |
| QV25 | 429 là `RateLimited`, schema cập nhật | Q13 | `q13_429_maps_to_rate_limited`, `p0_f03` |
| QV26 | Chờ quota hồi, huỷ được, có giới hạn | Q13 | `q13_rate_limit_*`, PTY q13 |
| QV27 | `/autonomous` quyết định đúng thứ tự prime | Q14 | `q14_decide_follows_prime_order` |
| QV28 | Gate fail → tiếp tục có prompt prime, pass → dừng | Q14 | PTY q14 |
| QV29 | Parser lịch nhận các dạng prime | Q15 | `q15_parser_accepts_prime_forms` |
| QV30 | Lịch bền qua lần mở lại, lỡ gộp thành một | Q15 | `q15_jobs_survive_a_restart`, `q15_missed_runs_collapse_into_one` |

## 8. Hợp đồng và schema bị chạm

| Thứ | Thay đổi | Item |
|---|---|---|
| `ErrorCode` + `schemas/*error-report*` | thêm `RateLimited`; sinh lại bằng `generate_schemas`; không sửa JSON bằng tay | Q13 |
| Config (`ResolvedConfig`, `ha config explain`) | khoá mới `[agents] default_model`, `[queue] steering_mode/follow_up_mode`, `[models] enabled/auxiliary/backup/image` — optional, config cũ load nguyên | Q04/Q05/Q11/Q12 |
| File mới người dùng | `<config dir>/models.json`, `<data dir>/schedules/<task_id>.json` | Q10/Q15 |
| Tool schema | `delegate` thêm `wait` (optional); con có tool `agent_message` | Q02/Q03 |
| Store | không đổi schema SQL trừ khi Q08 phương án B cần `session_settings` (đã có bảng) | Q08 |

## 9. Gate và bằng chứng

- **Mỗi item:** mục 3.6.
- **Mỗi checkpoint:** `cargo test --workspace --locked --no-fail-fast` (ghi số pass/fail; test chập chờn ở 3.3 chạy riêng); PTY đầy đủ qua script (25 ca cũ + ca q mới) → `PTY_EXIT: 0`; `Verify-Docs.ps1` → `DOCS_OK`.
- **Evidence** theo TEMPLATES mục 2: HEAD, OS, lệnh thật, số test, transcript PTY, **negative control** cho mỗi checkpoint (ví dụ CP-1: tạm bỏ dòng giữ lease trong `InteractiveWorkerBackend` → `q01_children_outlive_the_turn_that_spawned_them` phải đỏ; CP-3: tạm map 429 về `ServiceUnavailable` → `q13_429_maps_to_rate_limited` phải đỏ), và `not_run` (live smoke với provider thật chỉ khi được cấp).

## 10. Rủi ro

| Rủi ro | Cách xử lý |
|---|---|
| Đổi cách mở store (Q01) làm vỡ nhiều test | Bước 3 của Q01 dừng lại chạy toàn bộ unit test trước khi đi tiếp; i05/i13 PTY bắt buộc |
| Lượt tự động nối nhau vô hạn (notice → lượt → spawn → notice…) | Mỗi con một notice; notice không tự spawn; `/autonomous` có giới hạn; test Q02 kiểm tra một con → đúng một lượt |
| Tin nhắn agent spam | Giới hạn prime (Q03) |
| Phím Alt/Ctrl không tới qua ConPTY | Q06/Q11 đo trước, có lệnh thay thế |
| Fork vi phạm luật scope của store | Q08 bước 0 đo, phương án B không nới luật |
| Chờ quota treo UI | Esc huỷ; giới hạn 15 phút; UI hiện đếm ngược |
| Gate chạy lệnh nguy hiểm | Gate là lệnh người dùng tự gõ (D12), chạy trong workspace, có timeout |

## 11. Ngoài phạm vi (để track sau)

Thêm provider (Google/Vertex, Bedrock, Azure, Mistral, xAI, Groq, OpenRouter, Copilot, …) và OAuth của chúng; API extension kiểu prime (sự kiện, đăng ký tool/lệnh/UI) và cài gói; `--mode rpc` / `--mode acp`; fullscreen + chuột, hiển thị ảnh/Mermaid; tự cập nhật, `/changelog`, `/logs`, `/traces`; daemon nhiều session, `rlm.create_session`, `ha agents/attach/send/stop`; độ sâu agent con > 1 (`/rlm-max-depth`); `/import`; tóm tắt nhánh và nhãn trong `/tree`; "park" chờ quota qua lần mở lại; `/settings` + chọn theme; `/fast`, `/tier`.

## 12. Quyền, điểm dừng, checkpoint

Plan **không tự cấp quyền**. Mặc định DeepSeek được: đọc/sửa source trong repo, build/test cục bộ, chạy PTY bằng script, tạo tiến trình con và thư mục tạm, sửa docs SPEC/evidence/handoff/operator guide. **Không** được nếu assignment không nói: commit/push, gọi API trả phí, cài `ha` lên máy người dùng (`Install-Ha.ps1`), xoá/nới test đã có, đổi version protocol.

| Checkpoint | Phạm vi | Đạt khi |
|---|---|---|
| CP-1 | Q00–Q04 (agent nền, thông báo, tin nhắn, model con) | QV01–QV11; PTY đủ ca, `PTY_EXIT: 0` |
| CP-2 | Q05–Q09 (hàng đợi, stash, `/btw`, fork/tree, HTML) | QV12–QV19 |
| CP-3 | Q10–Q13 (models.json, scoped, định tuyến, chờ quota) | QV20–QV26; `p0_f03` xanh |
| CP-4 | Q14–Q15 (`/autonomous`, lịch bền) | QV27–QV30; toàn bộ gate mục 9 |

Sau mỗi checkpoint cập nhật `docs/handoffs/HA_PRIME.vi.md` với exact next action và **dừng**; không tự sang checkpoint kế tiếp.
