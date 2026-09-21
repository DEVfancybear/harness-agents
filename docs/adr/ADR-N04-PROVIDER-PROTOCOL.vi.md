# ADR-N04 — Provider protocol: canonical messages, capability claims và terminal semantics

**Trạng thái:** accepted trong M2 (21/09/2026).
**Phạm vi:** boundary provider (canonical model vs wire encoding, capability claims, terminal/retry/redaction). Không thay ADR-N01/N02.

## 1. Bối cảnh

P2 đã có `ProviderMessage`, `ProviderStreamEvent`, `SseDecoder`, `DeepSeekAdapter` và transport incremental H04. M2 phải chốt ba chỗ dễ trôi: (a) model canonical khác encoding của từng endpoint, (b) capability `unknown` khác `unsupported`, (c) một stream thiếu terminal hoặc đổi identity không được dispatch.

## 2. Quyết định

1. **Canonical vs wire.** `ProviderMessage` là canonical model, có `tool_calls` (assistant) và `tool_call_id` (tool result). `to_wire` là encoding của endpoint Chat Completions: tool result vẫn là **user message có marker**, vì endpoint này đã đo là từ chối `tool` role thiếu `tool_call_id` và không nhận tool calls giữa hội thoại. Việc flatten là ràng buộc API đã đo, **không** phải để adapter dễ code; canonical model giữ identity để validate và để M4 bind receipt.
2. **Validator trước dispatch.** `validate_transcript` chạy trên canonical model trước khi freeze: tool result phải trỏ call được announce **trước đó**, một call id chỉ được announce một lần và trả lời một lần, tool result không đứng trước call. Lỗi typed `ProviderProtocol`; runtime từ chối trước khi gửi request.
3. **Capability claims tri-state.** `Supported` / `Unsupported` / `Unknown`. `Unsupported` ⇒ từ chối request cần parameter đó (`IncompatibleService`). `Unknown` ⇒ được phép gửi nhưng **không** là bằng chứng tương thích (không dùng để claim live support).
4. **Terminal semantics.** `[DONE]` là transport marker: nó chỉ cung cấp finish reason dự phòng (`stop`) khi stream chưa có reason nào; reason của provider không bị marker ghi đè. Frame usage-only trở thành `ProviderStreamEvent::Usage` riêng. Thiếu terminal ⇒ `finish_reason == None` ⇒ `is_dispatchable() == false`; args không parse được ⇒ `incomplete_tool_calls`.
5. **Identity theo `(choice,index)`.** Call id được nhớ theo cặp; cùng một slot announce id khác ⇒ `ProviderProtocol` (không tạo hai identity cho một call).
6. **Limits.** `SseLimits` chặn buffer/frame/calls/args; vượt ⇒ `FrameLimitExceeded`/`OutputLimitExceeded`. Stream là input không tin cậy.
7. **Retry ownership.** Adapter **không** retry. Runtime retry theo `RetryClass` (Transient/Bounded) và `max_attempts`, tôn trọng `Retry-After` với cap 2 giây. 400/401/402/422 (theo bảng lỗi chính thức của DeepSeek) là `Never` ⇒ đúng một attempt.
8. **Redaction.** Error chỉ mang status + typed code; token/body không vào message. Test sentinel chứng minh ở tầng runtime.
9. **Capability matrix pin theo docs chính thức** tại thời điểm M2 (xem SPEC §5); không hardcode pricing, không bịa capability.

## 3. Hệ quả

- M3 tiêu thụ `is_dispatchable()` trước khi dựng step/tool continuation; M4 bind `tool_call_id` vào intent/receipt.
- Provider mới phải khai báo claim của nó; claim `Unknown` không được dùng để mở gate.
- Đổi encoding wire (ví dụ chuyển sang Responses API) là thay đổi adapter, không đổi canonical model.
- Mọi thay đổi taxonomy lỗi phải cập nhật `http_status_error` và test A07 tương ứng.
