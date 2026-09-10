# P2 — Agent runtime, context và compaction

[English](P2_RUNTIME_CONTEXT.en.md) | Tiếng Việt

Runbook triển khai; trạng thái: **chưa bắt đầu**. Dự toán: 7–10 ngày công. Target files, Rust tests và lệnh `ha` dưới đây là output tương lai, trừ khi checkout thật đã có. Riêng tài liệu này không là bằng chứng hoàn tất.

## 1. Kết quả và điều kiện vào phase

Cần [P1](P1_KERNEL_STORAGE.vi.md) được chấp nhận. Bàn giao model loop một agent có durable request history, context inspection, compaction nhiều lần và resume. Dùng fixture tools xác định được; filesystem/process tools sản phẩm thuộc P3.

Đọc [sổ tay](README.vi.md), [plan](../RUST_HARNESS_PLAN.vi.md), [hợp đồng plugin](../PLUGIN_ARCHITECTURE.vi.md), [hợp đồng memory](../MEMORY_AND_CONTINUITY.vi.md), [bảng nghiệm thu](ACCEPTANCE_MAP.vi.md). Inspect evidence tiền nhiệm thật trước khi code.

## 2. Scope sở hữu và target files

Sở hữu `crates/harness-runtime/`, `crates/harness-providers/`, context/projection modules trong `harness-session`, packet/composition migrations, `crates/harness-cli/tests/phase_p2.rs`, HTTP/SSE mock fixtures, CLI `run`, `resume`, `continue`, `context inspect`, `session replay --offline`. Reusable-memory service vẫn tùy chọn qua implementation rỗng/test.

## 3. Contracts cần chốt trước implementation

Đặc tả provider messages/tool-call IDs chuẩn hóa, stream finalization khác attempts, cancellation, model capabilities, provider-private fields. Chốt input/output token reservations, mandatory-context admission, compaction CAS, immutable request packet format. Chọn rõ capabilities được hỗ trợ; khi code adapter phải kiểm tra docs API DeepSeek chính thức hiện hành, không hard-code context window theo phỏng đoán.

## 4. Work items theo thứ tự

### 4.1. P2-S01 — Đặc tả actor và provider boundary

Phụ thuộc: P1 đã được chấp nhận.

Định nghĩa agent states, turn/step transitions, durable inbox claim/consumption, bounded retries/cancel. Tách application service API khỏi CLI presentation. Viết model capability và normalized request/stream contracts; lưu exact config/provenance.

Bằng chứng: Không có loop chỉ thuộc CLI hay provider sửa request ngầm; có tests transitions không hợp lệ.

### 4.2. P2-S02 — Làm mock provider và DeepSeek adapter

Phụ thuộc: P2-S01.

Làm mock responses xác định, local HTTP/SSE fixtures rồi adapter DeepSeek HTTP với credential do host resolve. Test chunk splits, tool arguments sai/dở, cancel, transport errors, capability mismatch. Chỉ giữ provider-specific fields qua hỗ trợ adapter rõ.

Bằng chứng: Tool call chưa hoàn chỉnh không chạy; key không được lưu; mock tests không cần mạng ngoài.

### 4.3. P2-S03 — Làm mandatory context admission

Phụ thuộc: P2-S01.

Lắp system policy, instruction ledger, objectives, decisions, WorkingState, tool pairs còn nợ, recent tail trước optional contributors. Giữ admitted instructions chưa phân loại và project rules bắt buộc độc lập top-k. Tính budget, giải thích phần bỏ.

Bằng chứng: C18/C19 pass dù query search cố ý không liên quan; mandatory overflow pause có lý do.

### 4.4. P2-S04 — Nối persisted loop và frozen requests

Phụ thuộc: P2-S02, P2-S03.

Claim input bền vững, resolve composition, commit exact sanitized request/manifest rồi mới gọi provider. Ghi settled messages riêng với failed/canceled attempts. Fixture tools qua execution boundary có kiểu; chưa có production shell.

Bằng chứng: Reconstruct request khớp packet đã lưu; config/context đổi không sửa request đang chạy.

### 4.5. P2-S05 — Làm compaction có deterministic fallback

Phụ thuộc: P2-S03, P2-S04.

Ghi source sequence, dựng candidate ngoài SQL transaction, validate mandatory state/tool pairs rồi CAS checkpoint. Tail đổi thì retry/rebase. Summary lỗi dùng WorkingState context dựng có quy tắc; không xóa raw events.

Bằng chứng: C04/C09 giữ yêu cầu qua năm compactions và correction A→B; summary lỗi không làm mất constraints.

### 4.6. P2-S06 — Làm resume, continuation, offline replay

Phụ thuộc: P2-S04, P2-S05.

Lấy ownership mới, restore state/inbox, reconcile fixture calls còn nợ, so composition/event versions. New-session continuation link đúng task, không lấy việc không liên quan. Offline replay đọc packets, không dispatch model/tools.

Bằng chứng: C06/C14/K10/K11 pass; unknown critical event chặn execution nhưng vẫn inspect được.

### 4.7. P2-S07 — Tích hợp CLI và recovery acceptance

Phụ thuộc: P2-S01..P2-S06.

Hiện recovery receipt trước hành động; expose token sources, optional blocks bị bỏ, config revisions. Chạy keyless end-to-end loop/restart tests. Chỉ thêm provider smoke được cấp quyền riêng, có budget, khi local có credential.

Bằng chứng: Evidence phân biệt mock/local adapter tests với live API smoke thực sự đã chạy.

## 5. Tests và lệnh kiểm chứng

Primary: C04, C06, C09, C14, C18, C19, K10, K11. Tăng cường C01/C02/C15/C24/K02 với actor thật; làm nhánh summary-failure của C05 lúc này, extraction/embedding failure đầy đủ thuộc P4. Assert offline replay có 0 network/model/tool calls; không yêu cầu output live provider xác định tuyệt đối.

Lệnh phase tương lai, chạy sau khi đã triển khai các targets:

```powershell
cargo test -p harness-cli --test phase_p2 --locked
pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P2
```

Full gate gồm formatting, clippy, workspace tests, kiểm tra test discovery và docs checks trong sổ tay. Không báo acceptance pass khi filter chỉ chạy 0 tests.

## 6. Kịch bản demo

1. Chạy task ba yêu cầu bằng mock model/fixture tools.
2. Ép năm compactions, thay decision A bằng B giữa chừng.
3. Kill fixture host sau durable result; chạy `ha resume <session-id>`.
4. Inspect recovery receipt/context manifest; vẫn phải có failure/bước còn lại và B.
5. Tắt providers, replay offline; so recorded packets và assert không dispatch calls.

## 7. Exit gate và các cách làm không được phép

Gates single-agent loop/context/compaction/resume pass. Thiếu optional memory không chặn restore. Không bỏ mandatory instructions ngầm, inject prompt không snapshot, chạy tool từ JSON dở, retry uncertain call mù. Phase này chưa có bộ coding tools thật hoặc multi-agent scheduler.

Không chuyển phase chỉ dựa trên summary. Gắn kết quả với revision cuối đã test, báo checks chưa chạy, giữ mọi regressions tiền nhiệm. Không sửa fixture expectations chỉ để implementation pass.

## 8. Giao việc và handoff

Sau S01 có thể giao providers S02/context S03 cho owners riêng. Runtime integrator sở hữu S04 và packet schema integration. P3 nhận execution boundary, actor cancellation contract, model/tool schemas, recovery receipts, fixture transcript.

Bàn giao `docs/evidence/P2.en.md`, `P2.vi.md`, cùng handoff tiếp tục được trong `docs/handoffs/` theo sổ tay. Ghi step IDs đã xong, lỗi còn lại, schema changes, commands và next action. Publish cần được cấp quyền rõ trong assignment coding.

## 9. Prompt giao agent

```text
Chỉ triển khai P2. Đọc docs/implementation/README.vi.md và
P2_RUNTIME_CONTEXT.vi.md trong cùng thư mục, các hợp đồng kiến trúc được link,
và quy định repo áp dụng. Xác minh gate tiền nhiệm bằng source/evidence.
Tạo SPEC phase rồi thực hiện P2-S01..P2-S07 theo dependencies.
Giữ đúng scope phase, bảo toàn thay đổi không liên quan và contracts đã chấp nhận.
Acceptance tests dùng component thật; chỉ mock boundaries bên ngoài phù hợp.
Chạy phase gate và regressions tiền nhiệm; bàn giao evidence hai ngôn ngữ cùng restart handoff.
Dừng trước phase tiếp. Không spawn agents, commit, push hoặc publish nếu chưa được giao rõ.
Nếu thiếu prerequisite hoặc verification bắt buộc, báo đúng khoảng trống, không nhận đã hoàn tất.
```
