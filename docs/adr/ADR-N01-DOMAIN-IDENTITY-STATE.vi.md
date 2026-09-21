# ADR-N01 — Domain identity, state machines và acceptance

**Trạng thái:** accepted trong M0 (21/09/2026).
**Phạm vi:** identity/state/acceptance dùng chung cho M0–M12.
**Thay thế:** không. ADR-0001 (P0 foundation boundaries) vẫn hiệu lực.

## 1. Bối cảnh

M0 phải chốt identity và state trước khi M1 dựng bảng run/step và M3 dựng
TurnDriver. Code hiện có đã có typed IDs, `WorkingState` projection, một run state
machine nhỏ trong `harness-runtime`, và một quyết định acceptance nằm trong
orchestrator (`DelegatedResult::accepted_completion`). Nếu không tách rõ, "run
kết thúc" rất dễ bị dùng thay cho "task được nghiệm thu".

## 2. Quyết định

1. **Bốn identity tách biệt.** `SessionId` là container hội thoại durable;
   `TaskId` là đơn vị nghiệm thu; `AgentRunId` là một lần thực thi; `StepId` là
   một chu kỳ model/tool bên trong run. `StepId` là public type từ M0, bảng
   runs/steps thuộc M3.
2. **Ordering chỉ bằng sequence.** `(session_id, seq)` là thứ tự giao dịch duy
   nhất. ID và timestamp không bao giờ quyết định ordering.
3. **Ba state machine độc lập.** Run (`AgentState`), acceptance
   (`AcceptanceRecord`), projection (`WorkingState`). Reducer là hàm thuần
   `(state, validated command) -> (next state, proposed events)`.
4. **Terminal không regress.** Run terminal chỉ nhận `Dispose`; acceptance đã
   accepted là terminal với mọi command khác.
5. **Acceptance cần evidence.** Required criterion chỉ được `Satisfied` khi có
   evidence typed (`FileChanged`/`CheckExecuted`/`ArtifactProduced`); còn
   `pending_effects` thì chưa accepted. Human acceptance ghi `actor_id` +
   `SourceRef` và **không** sửa criteria thành satisfied — nó là override được
   ghi lại, không phải test-pass giả.
6. **Lỗi có retry class.** `ErrorReport { schema_version, code, retry_class,
   safe_message, correlation_id?, details_ref? }`; `RetryClass` suy từ
   `ErrorCode`, không từ message. CLI exit code suy từ `ErrorCode::exit_code()`.
7. **Scope do host tạo.** `ScopeContext` (principal, project/worktree, task,
   session, capabilities, config revision, owner generation) được host dựng;
   tool args/contribution chỉ được kiểm tra *theo* nó và không thể mở rộng nó.

## 3. Hệ quả

- M1 cài `StorePort` trên `SQLite`; M3 thêm bảng step/run nhưng không đổi ID.
- M3/M4 không được tạo "accepted" từ assistant final text; phải gọi acceptance
  reducer.
- Mọi thay đổi `ErrorCode` phải cập nhật `retry_class()`/`exit_code()` (match
  exhaustive, compiler ép).
- Thêm command vào run machine phải cập nhật bảng `ALLOWED` trong test
  table-driven của `harness-runtime`.
