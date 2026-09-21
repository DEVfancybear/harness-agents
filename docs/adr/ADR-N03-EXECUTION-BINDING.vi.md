# ADR-N03 — Execution binding, final guard, host limitations và reconciliation

**Trạng thái:** accepted trong M4 (22/09/2026).
**Phạm vi:** tool authority (grant/intent/receipt), final guard trước dispatch, giới hạn host và xử lý outcome chưa xác định. Không thay ADR-N01/N02/N04.

## 1. Bối cảnh

P3 đã có một gate duy nhất (normalize → policy → approval → intent → executor → receipt) với approval binding theo actor/action/workspace/revisions, consume+intent atomic, receipt bất biến và reconcile cho pending. M4 phải: (a) bind grant chặt hơn (invocation/task/session), (b) ghi correlation `tool_call_id` của provider vào record bền vững, (c) nói thẳng host không phải sandbox và TOCTOU không thể đóng hoàn toàn, (d) xử lý "side effect có thể đã xảy ra" mà không biến thành failed/rerun.

## 2. Quyết định

1. **Grant binding.** Một approval bind tất cả: `actor`, `task_id`, `session_id`, `invocation_id`, `action_hash`, `workspace_root` + `workspace_fingerprint`, `policy_revision`, `tool_revision`, `expiry`. `binding_hash` canonical bao gồm mọi field này; thiếu bất kỳ field nào bị test A14 bắt (negative control: bỏ invocation khỏi hash).
2. **Một lần consume.** Consume grant + insert intent + event + projection nằm trong **một** transaction; CAS `active → consumed`; grant của invocation/task/session khác hoặc đã consume/revoke/hết hạn → typed deny, executor không được gọi. `revoke` là chuyển `active → revoked`; mọi lần execute sau đó deny.
3. **Correlation `tool_call_id`.** `call_id` của provider được ghi vào `ToolRequest` → intent → receipt (optional, `serde(default)` cho record cũ). Authority vẫn là host `InvocationId`; `call_id` chỉ để đối chiếu transcript/model view, không thay ID.
4. **Final guard.** Ngay trước dispatch: revalidate policy revision, workspace fingerprint, target hash (patch), cancellation và admission permit. Nếu bất kỳ check nào đổi → typed deny/stale, không có side effect. Khoảng TOCTOU giữa check và hành động OS là **hữu hạn và được thừa nhận**, không tuyên bố đóng được bằng DB.
5. **Host limitations.** `filesystem_network_sandbox=false` và `strict_isolation=false` là sự thật phải hiển thị; path policy chỉ ràng buộc các thao tác host-mediated, không nhốt shell/process. Process tree: Windows dùng JobObject kill-on-close, Unix dùng process session; `tree_cleanup_confirmed` chỉ true khi backend xác nhận, ngược lại receipt mang `uncertainty` (`outcome_unknown`).
6. **Effect trước receipt.** Settlement fail sau side effect ⇒ intent giữ `recorded` và receipt `outcome_unknown`; **không** tự rerun, **không** đổi thành failed-no-effect. `reconcile_pending` là hành động tường minh, ghi event mới tham chiếu intent cũ, chỉ settle `applied` khi nội dung đích khớp chính xác.
7. **Output dài.** Process spool ra artifact có phần header/tail preview + page read; memory không giữ full log; vượt quota/disk-full → typed error, không tạo ref giả; receipt mang `captured_bytes`/`content_hash`/`truncated` của đúng bytes đã capture.
8. **Evidence cuối.** Check evidence gắn `workspace_digest`; final criteria chỉ satisfied khi digest của check khớp fingerprint workspace cuối. Model prose và counter không đủ.

## 3. Hệ quả

- M4 thêm cột binding (session/task/invocation/`call_id`) và tools schema v2 additive; DB cũ tự nâng cấp, host cũ từ chối DB mới hơn.
- A14/A11 kiểm binding + expiry/revoke/replay; A03/A04 kiểm receipt-before-checkpoint và effect-before-receipt bằng child process bị kill thật.
- A13/A16/A17 kiểm permit queue, env allowlist/JIT secret, tree cleanup theo backend và spool/quota.
- A08 dùng repo tạm + test runner thật, final criteria tại workspace digest.
- Không milestone nào được quảng cáo là sandbox; giới hạn host phải xuất hiện trong `ha code capabilities` và evidence.
