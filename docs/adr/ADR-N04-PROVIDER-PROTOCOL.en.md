# ADR-N04 — Provider protocol: canonical messages, capability claims and terminal semantics

**Status:** accepted in M2 (2026-09-21).
**Scope:** the provider boundary (canonical model vs wire encoding, capability claims, terminal/retry/redaction). Does not replace ADR-N01/N02.

## 1. Context

P2 already had `ProviderMessage`, `ProviderStreamEvent`, `SseDecoder`, `DeepSeekAdapter` and the incremental H04 transport. M2 has to fix three things that drift easily: (a) the canonical model is not the endpoint encoding, (b) `unknown` capability is not `unsupported`, (c) a stream without a terminal, or one that changes identity, must not be dispatched.

## 2. Decisions

1. **Canonical vs wire.** `ProviderMessage` is the canonical model and carries `tool_calls` (assistant) and `tool_call_id` (tool result). `to_wire` is the Chat Completions encoding: a tool result stays a **marked user message** because this endpoint was measured to refuse a `tool` role without `tool_call_id` and does not accept tool calls mid-conversation. That flattening is a measured API constraint, **not** a convenience for the adapter; the canonical model keeps identity for validation and for M4 receipt binding.
2. **Validation before dispatch.** `validate_transcript` runs on the canonical model before freezing: a tool result must reference a call announced **earlier**, a call id is announced once and answered once, and a result may not precede its call. Failures are typed `ProviderProtocol`; the runtime refuses before sending the request.
3. **Tri-state capability claims.** `Supported` / `Unsupported` / `Unknown`. `Unsupported` refuses a request that needs that parameter (`IncompatibleService`). `Unknown` may be sent but is **not** evidence of compatibility (never used to claim live support).
4. **Terminal semantics.** `[DONE]` is a transport marker: it only supplies a fallback finish reason (`stop`) when the stream named none, and never overwrites the provider's reason. A usage-only frame becomes its own `ProviderStreamEvent::Usage`. A missing terminal means `finish_reason == None` and `is_dispatchable() == false`; unparseable arguments mean `incomplete_tool_calls`.
5. **Identity by `(choice,index)`.** Call ids are remembered per pair; the same slot announcing another id is `ProviderProtocol` instead of producing two identities for one call.
6. **Limits.** `SseLimits` caps buffered bytes, frame bytes, call count and argument bytes; exceeding one is `FrameLimitExceeded`/`OutputLimitExceeded`. A stream is untrusted input.
7. **Retry ownership.** The adapter never retries. The runtime retries by `RetryClass` (Transient/Bounded) within `max_attempts`, honouring `Retry-After` with a 2-second cap. 400/401/402/422 (DeepSeek's own error table) are `Never`, so they get exactly one attempt.
8. **Redaction.** An error carries the status and a typed code only; no token and no body. A sentinel test proves it at the runtime boundary.
9. **Capability matrix pinned to the official docs** at M2 time (SPEC §5); no pricing is hardcoded and no capability is invented.

## 3. Consequences

- M3 consumes `is_dispatchable()` before building a step or tool continuation; M4 binds `tool_call_id` into intents/receipts.
- A new provider must declare its claims; an `Unknown` claim never opens a gate.
- Changing the wire encoding (for example moving to the Responses API) is an adapter change, not a canonical-model change.
- Any taxonomy change must update `http_status_error` and the matching A07 test.
