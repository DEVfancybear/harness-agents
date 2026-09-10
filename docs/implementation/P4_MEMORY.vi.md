# P4 — Memory tái sử dụng và extraction phục hồi được

[English](P4_MEMORY.en.md) | Tiếng Việt

Runbook triển khai; trạng thái: **chưa bắt đầu**. Dự toán: 7–10 ngày công. Target files, Rust tests và lệnh `ha` dưới đây là output tương lai, trừ khi checkout thật đã có. Riêng tài liệu này không là bằng chứng hoàn tất.

## 1. Kết quả và điều kiện vào phase

Cần [P3](P3_CODING_TOOLS.vi.md) được chấp nhận. Bàn giao cross-session memory inspect được, có scope, dùng SQLite/FTS native và background extraction phục hồi được. Task hiện tại vẫn tiếp tục từ journal/WorkingState khi mọi optional extractor/embedding adapter vắng mặt.

Đọc [sổ tay](README.vi.md), [plan](../RUST_HARNESS_PLAN.vi.md), [hợp đồng plugin](../PLUGIN_ARCHITECTURE.vi.md), [hợp đồng memory](../MEMORY_AND_CONTINUITY.vi.md), [bảng nghiệm thu](ACCEPTANCE_MAP.vi.md). Inspect evidence tiền nhiệm thật trước khi code.

## 2. Scope sở hữu và target files

Sở hữu `crates/harness-memory/`, migrations memory store/query qua SQLite coordinator hiện có, source dependency tracking, test extractors/contributors, `crates/harness-cli/tests/phase_p4.rs`, CLI memory search/read/inspect/invalidate/jobs/catch-up. Không deploy Tencent, Redis hay vector database riêng.

## 3. Contracts cần chốt trước implementation

Chốt asset/version/binding/grant/dependency records, phân biệt authority/validity, strategy digests, immutable input batches, job transitions, contiguous cursor keys. Scopes project/task/profile/session do host cấp. FTS là baseline phát hành; mock optional vector adapter chỉ test degradation, không nhận đã hỗ trợ vector thật. L3 ban đầu do user xác nhận/thủ công.

## 4. Work items theo thứ tự

### 4.1. P4-S01 — Chốt memory contracts/publication policy

Phụ thuộc: P3 đã được chấp nhận.

Viết schemas/authorized actions cho assets, versions, scopes, grants, bindings. Quy định observations đã kiểm tra nào được publish, inferences nào còn candidate, decisions user supersede facts cũ thế nào. Chốt retention interfaces; purge phá hủy dữ liệu ở P7.

Bằng chứng: Confidence model, tên role hay cùng chủ tài khoản không trở thành permission/runner evidence.

### 4.2. P4-S02 — Làm scoped assets và versioned writes

Phụ thuộc: P4-S01.

Dùng transaction coordinator hiện có cho immutable versions, current pointers, provenance/dependencies, CAS. Check scope trước top-k và ở direct reads/artifacts/exports. Phân biệt query availability và authorization failure.

Bằng chứng: C07/C08 dùng ba actor identities/proposals đồng thời; không lost update hay đọc vượt quyền.

### 4.3. P4-S03 — Làm durable extraction scheduling

Phụ thuộc: P4-S01, P4-S02.

Đổi committed source-work markers thành event ranges bounded không chồng. Lease có generation fencing, extract ngoài SQL, rồi atomically settle versions/dependencies/disposition/cursor/job. Retry backoff có giới hạn; startup tìm ranges chưa xử lý dù mất notification.

Bằng chứng: C12/C16/C17/C22 chặn settle trùng, bỏ dữ liệu do timestamp, cursor gaps, JSON sai, worker stale.

### 4.4. P4-S04 — Làm L1/L2 và profile updates có kiểm soát

Phụ thuộc: P4-S03.

Extract atomic candidates có source từ journal projection; dựng L2 từ L1 changes có version, cả invalidation. Validate output/source refs, semantic-merge proposals, expected versions. Lưu no-facts disposition. Đổi strategy chọn replay range rõ, không reset tiến độ ngầm.

Bằng chứng: Summaries/injected content không thành bằng chứng độc lập; confirmed L3 không là authority suy diễn.

### 4.5. P4-S05 — Làm bounded retrieval/context contribution

Phụ thuộc: P4-S02.

Làm FTS/metadata parameterized, normalization Unicode/tiếng Việt/identifiers, source-aware read tools, bounded index/bootstrap blocks. Trả found/empty/degraded/error. Chỉ context builder P2 nhận/chốt blocks; mandatory rules không cạnh tranh top-k.

Bằng chứng: C05/C26 kiểm tra optional services tắt, rỗng, timeout; resume từ journal vẫn chạy.

### 4.6. P4-S06 — Làm invalidation, cache consistency, budgets

Phụ thuộc: P4-S04, P4-S05.

Theo transitive sources qua summaries, invalidate blocks phụ thuộc, so policy/asset revisions lại trước dispatch. Phát hiện source files/revisions đổi. Giới hạn calls/tokens/chi phí memory riêng; pause jobs bền vững khi shutdown/hết budget.

Bằng chứng: C13/C20/C30 kiểm tra reinjection loops, cached summary bị revoke, extraction gián đoạn khi CLI thoát.

### 4.7. P4-S07 — Tích hợp memory CLI/end-to-end tests

Phụ thuộc: P4-S01..P4-S06.

Expose provenance/version/validity/source inspection, catch-up rõ với budget hữu hạn. Restart task khi tắt extraction, xử lý backlog bằng mock extractor xác định. Chạy concurrent writers/retrieval fixtures trên SQLite thật.

Bằng chứng: Memory hữu ích/inspect được nhưng không bắt buộc để restore task; thấy được độ mới capture/queue/index.

## 5. Tests và lệnh kiểm chứng

Primary: C05, C07, C08, C12, C13, C16, C17, C20, C22, C26, C30. Tăng cường C09/C18/C19/C29/K02/K10. C17 inject completion ngoài thứ tự dù scheduler v1 xử lý tuần tự từng stream. Test no-facts success và extraction fail riêng; chỉ trường hợp đầu tiến cursor. FTS precision/recall bao gồm tiếng Việt có/không dấu, code symbols.

Lệnh phase tương lai, chạy sau khi đã triển khai các targets:

```powershell
cargo test -p harness-cli --test phase_p4 --locked
pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P4
```

Full gate gồm formatting, clippy, workspace tests, kiểm tra test discovery và docs checks trong sổ tay. Không báo acceptance pass khi filter chỉ chạy 0 tests.

## 6. Kịch bản demo

1. Hoàn tất fixture observation/check, lưu event nguồn khi extraction đang dừng.
2. Restart CLI; tiếp tục coding task chưa xong từ WorkingState.
3. Chạy `ha memory catch-up --budget <finite-limit>` với mock extractor; inspect L1/L2 có source.
4. Sửa decision, invalidate asset cũ, kiểm tra context mới loại derived summary của nó.
5. Publish proposals đồng thời từ ba identities, restart extraction sau settlement failpoint; chứng minh không thiếu/trùng source disposition.

## 7. Exit gate và các cách làm không được phép

Primary cases/gates trước pass; không cần Tencent live/embeddings. Không ghi memory ngoài CAS/transaction owner, tin actor IDs từ model, cursor chỉ timestamp, inject sau freeze, self-reinforcing evidence hay chi phí nền vô hạn. Physical deletion/backup migration đầy đủ ở P7.

Không chuyển phase chỉ dựa trên summary. Gắn kết quả với revision cuối đã test, báo checks chưa chạy, giữ mọi regressions tiền nhiệm. Không sửa fixture expectations chỉ để implementation pass.

## 8. Giao việc và handoff

Sau S01, asset store S02 và chuẩn bị retrieval fixtures cô lập có thể giao riêng; retrieval code cần S02. Một owner giữ cursor settlement; owner khác làm FTS S05 sau chốt schema. P5 nhận APIs asset/grant, durable jobs và quy tắc rõ: task coordination không qua semantic search.

Bàn giao `docs/evidence/P4.en.md`, `P4.vi.md`, cùng handoff tiếp tục được trong `docs/handoffs/` theo sổ tay. Ghi step IDs đã xong, lỗi còn lại, schema changes, commands và next action. Publish cần được cấp quyền rõ trong assignment coding.

## 9. Prompt giao agent

```text
Chỉ triển khai P4. Đọc docs/implementation/README.vi.md và
P4_MEMORY.vi.md trong cùng thư mục, các hợp đồng kiến trúc được link,
và quy định repo áp dụng. Xác minh gate tiền nhiệm bằng source/evidence.
Tạo SPEC phase rồi thực hiện P4-S01..P4-S07 theo dependencies.
Giữ đúng scope phase, bảo toàn thay đổi không liên quan và contracts đã chấp nhận.
Acceptance tests dùng component thật; chỉ mock boundaries bên ngoài phù hợp.
Chạy phase gate và regressions tiền nhiệm; bàn giao evidence hai ngôn ngữ cùng restart handoff.
Dừng trước phase tiếp. Không spawn agents, commit, push hoặc publish nếu chưa được giao rõ.
Nếu thiếu prerequisite hoặc verification bắt buộc, báo đúng khoảng trống, không nhận đã hoàn tất.
```
