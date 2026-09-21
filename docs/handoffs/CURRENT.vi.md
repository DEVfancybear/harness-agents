# CURRENT — bàn giao đang mở

**Cập nhật:** 21/09/2026 · **Assignment:** M2 (Provider protocol và incremental stream), scope `M2-01..M2-04`.

## 1. Assignment hiện tại và ràng buộc mới nhất

- Prompt người dùng (21/09/2026): triển khai M2; đọc README/CONTRACTS/M2 + acceptance A06/A07; xác minh prerequisites; tạo SPEC; sửa crate hiện có, nối vào cùng binary `ha`; reuse tests; từng item theo thứ tự, targeted tests rồi milestone gate; không stub-success/không đổi fixture để che lỗi; bàn giao evidence/digest/discovery/OS limitations + CURRENT handoff; **dừng sau M2**; **commit and push sau khi xong việc**.
- Quyền đã cấp trong assignment này: local + đọc docs công khai DeepSeek + **commit/push** M2 sau khi gate xanh. Không paid API, không live smoke, không spawn agent.

## 2. Branch/base/source digest

- Branch `master`; base `a824b2c` (M1 đã push).
- Source digest lần gate cuối: `sha256:d67a4e07b6191deb77969aef0c333f02babe5f9edc9fe2ca90524665c0229750` (324 file, loại trừ `docs/evidence/M2.vi.md` và file này).

## 3. Work item

| Item | Trạng thái | Evidence |
|---|---|---|
| M2-01 typed messages + capability registry | `implemented_unverified` | `ProviderMessage::{tool_calls,tool_call_id}` + `assistant_with_calls`/`tool_result`; `validate_transcript` gọi trong `RuntimeService::run_inner`; `CapabilityClaim`/`CapabilityMatrix::validate` |
| M2-02 SSE assembler có state | `implemented_unverified` | `SseLimits` (buffer/frame/calls/args), key `(choice,index)` + conflict, `Usage` event, `[DONE]` không ghi đè reason, `ProviderResponse::is_dispatchable()` |
| M2-03 transport/DeepSeek/cancellation | `implemented_unverified` | `http_status_error` theo bảng lỗi chính thức; timeout connect/total; `Retry-After` + cap 2s; runtime retry theo `RetryClass`; redaction test |
| M2-04 conformance + smoke recipe | `implemented_unverified` | `FakeProvider` barrier trong `milestone_m2`; conformance mock vs adapter; live smoke không chạy (ghi rõ) |

## 4. File đã đổi

Sửa: `crates/harness-providers/src/{lib,streaming}.rs`, `crates/harness-runtime/src/lib.rs`, `crates/harness-tools/src/turn_driver.rs`, `crates/harness-cli/Cargo.toml` (dev-dep `futures-util`), `Cargo.lock`, `tests/acceptance/milestones.json`.
Thêm: `crates/harness-cli/tests/milestone_m2.rs`, `docs/specs/M2.vi.md`, `docs/adr/ADR-N04-PROVIDER-PROTOCOL.{vi,en}.md`, `docs/evidence/M2.vi.md`, file này.

## 5. Contract/ADR đã chốt — không đổi ngầm

- ADR-N04: canonical model vs wire encoding (tool result vẫn là user message có marker **vì API đo được** yêu cầu `tool_call_id`; canonical giữ identity); validator trước dispatch; capability tri-state (`Unknown` không phải bằng chứng); `[DONE]` là transport marker không ghi đè finish reason; identity theo `(choice,index)`; limits; retry owner = runtime, adapter không retry; redaction.
- Taxonomy lỗi theo bảng chính thức DeepSeek: 400/401/402/422 Never; 429/500/503 transient.
- A06/A07 do M2 sở hữu, map sang selector qualified; registry P/H không đổi.

## 6. Lệnh đã chạy và kết quả cuối

- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings` → pass.
- `cargo test -p harness-providers --locked` → 21 pass.
- `cargo test -p harness-cli --test milestone_m2 --locked` → 10 pass.
- `cargo test -p harness-cli --test milestone_m0 --test milestone_m1 --locked` → 11 + 6 pass.
- `cargo test -p harness-cli --test phase_p2 --test phase_p3 --test interactive_session --locked` → 17 + 21 + 13 pass.
- `pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M2 -SelfTest` → 7/7 OK.
- `pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M2 -Json` → `result: passed`, closure `[M2, M0]`, 12 required + 11 closure M0 + 3 unit test + 44 edge; digest ở §2.

## 7. Việc còn lại theo thứ tự

1. Commit + push M2 (assignment này đã cấp quyền) sau khi digest khớp evidence.
2. Chạy gate M2 trên **Linux** và chạy `Verify-Phase.ps1 -Phase P2`/`P3` như regression đa nền tảng.
3. **Live smoke tùy chọn** (`scripts/Smoke-HaProvider.ps1`) với credential riêng nếu muốn claim tương thích live — evidence hiện tại **không** claim điều đó.
4. **M3:** tiêu thụ `ProviderResponse::is_dispatchable()` trước khi dựng step; M4 bind `tool_call_id` vào intent/receipt; `StorePort` implement ở M3/M4.

## 8. Next action chính xác

`pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P2` (Windows) để xác nhận regression P2 trên digest M2, rồi reviewer đọc `docs/evidence/M2.vi.md`.

## 9. Blocked on

Không. (Live smoke cần credential — không nằm trong scope bắt buộc.)

## 10. Không lặp lại

- Không thêm provider thứ hai; không đổi wire sang tool role (đã đo là API từ chối).
- Không "sửa" test để che lỗi: thay đổi test duy nhất là bổ sung (`sse_limit_tests`) và adapt registry/selector.
- Không claim live compatibility từ mock/fixture.
