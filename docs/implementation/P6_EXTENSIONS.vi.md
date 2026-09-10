# P6 — Skills, MCP và protocol plugin bên ngoài

[English](P6_EXTENSIONS.en.md) | Tiếng Việt

Runbook triển khai; trạng thái: **chưa bắt đầu**. Dự toán: 5–8 ngày công. Target files, Rust tests và lệnh `ha` dưới đây là output tương lai, trừ khi checkout thật đã có. Riêng tài liệu này không là bằng chứng hoàn tất.

## 1. Kết quả và điều kiện vào phase

Cần [P5](P5_MULTI_AGENT.vi.md) được chấp nhận. Bàn giao extension points có kiểm soát: skills có version, MCP client, stdio bridges tool/provider bounded; không làm yếu durability, policy hoặc scoped memory.

Đọc [sổ tay](README.vi.md), [plan](../RUST_HARNESS_PLAN.vi.md), [hợp đồng plugin](../PLUGIN_ARCHITECTURE.vi.md), [hợp đồng memory](../MEMORY_AND_CONTINUITY.vi.md), [bảng nghiệm thu](ACCEPTANCE_MAP.vi.md). Inspect evidence tiền nhiệm thật trước khi code.

## 2. Scope sở hữu và target files

Sở hữu extension modules trong `harness-tools`, `harness-providers`, `harness-kernel`, `harness-runtime`; examples trusted profiles/skills; fixture plugin executables; `crates/harness-cli/tests/phase_p6.rs`. Thêm plugin/config inspection và đường cài/đăng ký local được cấp quyền rõ. Không marketplace/native dynamic loading tùy ý.

## 3. Contracts cần chốt trước implementation

Chốt host protocol versions, handshake/capability schemas, NDJSON frame limits, invocation IDs, inflight bounds, cancel/EOF. MCP negotiation/transport theo official SDK được chọn, không mặc định tương thích custom plugin protocol. Skill có identity/version/provenance và vẫn chịu instruction/tool authority policy.

## 4. Work items theo thứ tự

### 4.1. P6-S01 — Đặc tả trust/extension contracts

Phụ thuộc: P5 đã được chấp nhận.

Định nghĩa plugin identity/digest, requested/granted capabilities, allowlisted host methods, local registration. Chốt skill discovery/version pinning, config trust boundaries. Ghi release hỗ trợ providers/operations nào thật.

Bằng chứng: Chỉ mở repository config không được tự chạy plugin hay lấy secrets.

### 4.2. P6-S02 — Làm bounded stdio transport

Phụ thuộc: P6-S01.

Chạy executable đã pin với environment tối thiểu; handshake trước nhận việc. Parse bounded UTF-8 frames, route unique IDs, tách stdout protocol/stderr, giới hạn inflight calls, xử lý cancel/EOF. Validate response schema, dừng cây process khi hết deadline.

Bằng chứng: K12 frames sai/quá lớn, IDs trùng, output floods, bỏ qua cancel đều thất bại có giới hạn.

### 4.3. P6-S03 — Làm tool/provider bridge consumers

Phụ thuộc: P6-S02.

Chỉ expose external tools qua gate P3. Thêm external model-provider capability hẹp với normalized stream/terminal frames/cancel; validate bằng fixture provider. Giữ parent call correlation, grants không tăng quyền.

Bằng chứng: Denial external tool bền vững; request/provider content được ghi; reconnect không tự retry mutation.

### 4.4. P6-S04 — Tích hợp MCP client

Phụ thuộc: P6-S01, P6-S02.

Chọn/pin `rmcp` version hỗ trợ sau khi kiểm tra docs chính thức. Discover schemas, đăng ký scoped tools, chạy qua cùng gate. Ban đầu local fixture servers; remote endpoints cần trust/network policy rõ. Schema đổi phải negotiate trước dùng.

Bằng chứng: K13 chặn MCP/nested calls bỏ qua policy hoặc đổi arguments sau approval.

### 4.5. P6-S05 — Làm skills/profile composition

Phụ thuộc: P6-S01.

Lazy load trusted filesystem skills, có source/version/content bounded. Ghi admitted instructions vào context manifest; skill text không tạo permissions. Giải thích config precedence, row IDs hiệu lực, yêu cầu restart, lý do plugin inactive.

Bằng chứng: K14 chặn repo không trust xin executable/secret; skill update xuất hiện tại boundary có log.

### 4.6. P6-S06 — Chứng minh unload/restart compatibility

Phụ thuộc: P6-S03, P6-S04, P6-S05.

Unload extractor/tool/provider trong fixtures, restart plugin được phép bằng generation mới, giữ jobs/session evidence. Giữ packets cũ để replay; chặn critical event/schema không hỗ trợ. Recheck memory revocation/sensitive data qua extensions.

Bằng chứng: K01/K03/K05/K08/K10/K11 vẫn đúng với extension processes thật.

### 4.7. P6-S07 — Tích hợp examples/compatibility evidence

Phụ thuộc: P6-S01..P6-S06.

Tài liệu hóa fixture plugin clone-and-run và skill/profile tối thiểu có trust grants/teardown. Chạy full plugin suite không phụ thuộc dịch vụ trả phí. Ghi rõ methods/protocol versions chưa hỗ trợ.

Bằng chứng: User tái hiện cài/đăng ký/gỡ trong disposable profile không cần broad host privileges.

## 5. Tests và lệnh kiểm chứng

Primary: K12, K13, K14. Regression C03/C08/C14/C20/C29, K01/K03/K05/K08/K10/K11. Test process crash trước/sau side effect, handshake không hỗ trợ, schema đổi, cancel, stale registration removal. Malicious skill text là data, không là permission; fixture environments không có secret thật.

Lệnh phase tương lai, chạy sau khi đã triển khai các targets:

```powershell
cargo test -p harness-cli --test phase_p6 --locked
pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P6
```

Full gate gồm formatting, clippy, workspace tests, kiểm tra test discovery và docs checks trong sổ tay. Không báo acceptance pass khi filter chỉ chạy 0 tests.

## 6. Kịch bản demo

1. Đăng ký local fixture tool plugin với read-only grants; inspect digest/capabilities.
2. Chạy một tool được phép và một mutation bị từ chối qua agent.
3. Crash plugin giữa uncertain call; resume không retry mù.
4. Load skill có version, sửa source, kiểm tra context mới chỉ nhận version mới tại admission.
5. Chạy MCP/local provider fixtures; unload tất cả, chứng minh không sót registrations/processes.

## 7. Exit gate và các cách làm không được phép

Ba extension gates và plugin regressions cũ pass. Không inject sau freeze, tải package ngầm, kế thừa full environment chứa secrets, hoặc chạy untrusted code cùng process. Không gọi transport isolation là OS sandboxing. Chưa làm marketplace, Wasm, external loop/storage tùy ý.

Không chuyển phase chỉ dựa trên summary. Gắn kết quả với revision cuối đã test, báo checks chưa chạy, giữ mọi regressions tiền nhiệm. Không sửa fixture expectations chỉ để implementation pass.

## 8. Giao việc và handoff

S02 transport có owner chung. Khi contract ổn định có thể giao S03 bridge/S04 MCP/S05 skills vào paths riêng. Integrator giữ protocol versions/config registry. P7 nhận compatibility fixtures, inventory supported methods, shutdown/error evidence.

Bàn giao `docs/evidence/P6.en.md`, `P6.vi.md`, cùng handoff tiếp tục được trong `docs/handoffs/` theo sổ tay. Ghi step IDs đã xong, lỗi còn lại, schema changes, commands và next action. Publish cần được cấp quyền rõ trong assignment coding.

## 9. Prompt giao agent

```text
Chỉ triển khai P6. Đọc docs/implementation/README.vi.md và
P6_EXTENSIONS.vi.md trong cùng thư mục, các hợp đồng kiến trúc được link,
và quy định repo áp dụng. Xác minh gate tiền nhiệm bằng source/evidence.
Tạo SPEC phase rồi thực hiện P6-S01..P6-S07 theo dependencies.
Giữ đúng scope phase, bảo toàn thay đổi không liên quan và contracts đã chấp nhận.
Acceptance tests dùng component thật; chỉ mock boundaries bên ngoài phù hợp.
Chạy phase gate và regressions tiền nhiệm; bàn giao evidence hai ngôn ngữ cùng restart handoff.
Dừng trước phase tiếp. Không spawn agents, commit, push hoặc publish nếu chưa được giao rõ.
Nếu thiếu prerequisite hoặc verification bắt buộc, báo đúng khoảng trống, không nhận đã hoàn tất.
```
