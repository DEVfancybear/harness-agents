# P0 — Nền tảng, contracts và khung kiểm thử

[English](P0_FOUNDATION.en.md) | Tiếng Việt

Runbook triển khai; trạng thái: **chưa bắt đầu**. Dự toán: 3–4 ngày công. Target files, Rust tests và lệnh `ha` dưới đây là output tương lai, trừ khi checkout thật đã có. Riêng tài liệu này không là bằng chứng hoàn tất.

## 1. Kết quả và điều kiện vào phase

Bàn giao skeleton build được, không cần key, cùng nền contracts/fixtures chạy kiểm tra được. Không có phase tiền nhiệm. Kiểm tra docs kiến trúc và Git state sạch/bẩn thật; không giả định đã cài Rust hoặc C toolchain.

Đọc [sổ tay](README.vi.md), [plan](../RUST_HARNESS_PLAN.vi.md), [hợp đồng plugin](../PLUGIN_ARCHITECTURE.vi.md), [hợp đồng memory](../MEMORY_AND_CONTINUITY.vi.md), [bảng nghiệm thu](ACCEPTANCE_MAP.vi.md). Inspect evidence tiền nhiệm thật trước khi code.

## 2. Scope sở hữu và target files

Sở hữu `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `crates/harness-types/`, `crates/harness-cli/` tối thiểu, `schemas/`, `tests/fixtures/`, `tests/acceptance/registry.json`, `scripts/Verify-Phase.ps1`, Rust CI workflow ban đầu. Chỉ scaffold nhóm crate khác khi cần; ghi owner/path mapping tương lai. Thêm ignore rules cho build output/runtime data local, không ignore evidence cần lưu Git.

## 3. Contracts cần chốt trước implementation

Chốt binary tên `ha`. Đặc tả ID/envelope/schema versions, canonical serialization/hash inputs, giới hạn sequence, source authority labels, typed error codes. Ghi command tree CLI dự kiến, stdout JSON tách stderr logs. Pin Rust toolchain/dependencies đã kiểm tra, nêu lý do từng dependency; kiểm tra tương thích/license từ nguồn chính thức trước cài. SDK plugin Rust mới là phác thảo đến khi có object-safety tests.

## 4. Work items theo thứ tự

### 4.1. P0-S01 — Kiểm kê và chốt contract P0

Phụ thuộc: Không có.

Inspect quy định repo, quyết định từ khảo sát và tools đã cài. Viết SPEC P0 hai ngôn ngữ, dependency/setup inventory và non-goals rõ. Chỉ sửa mâu thuẫn contract bằng revision có giải thích.

Bằng chứng: Người review thấy mọi thay đổi môi trường dự kiến; không nhận đã có runtime capability.

### 4.2. P0-S02 — Tạo workspace tối thiểu

Phụ thuộc: P0-S01.

Tạo workspace, shared types crate, CLI crate với `[[bin]] name = "ha"`. Chỉ làm `--help`, `--version`, typed config parsing cần cho contract tests. Pin toolchain/lockfile; không gọi mạng model.

Bằng chứng: `cargo check --workspace --locked` và CLI help chạy; options/config fields lạ lỗi rõ.

### 4.3. P0-S03 — Định nghĩa durable contracts

Phụ thuộc: P0-S01, P0-S02.

Tạo schema có version cho event envelope, WorkingState, instructions, tool receipts, memory versions, plugin manifest, context/composition packet. Giữ domain ownership rõ dù types đầu nằm chung module. Có serialization fixtures hợp lệ/sai.

Bằng chứng: Round trip giữ đúng giá trị; chặn IDs/versions sai, thiếu authority, payload lỗi.

### 4.4. P0-S04 — Tạo continuation fixture chuẩn

Phụ thuộc: P0-S03.

Mô tả task có ba yêu cầu, hai bước xong, một check fail và một bước còn lại. Thêm decision A→B, user text chưa phân loại, input ID trùng, unknown event, artifact bị truncate/corrupt có chủ đích. Expected state độc lập với projector tương lai.

Bằng chứng: Fixtures có source sequences/hashes và việc chưa xong kỳ vọng; không dùng model sinh test oracle.

### 4.5. P0-S05 — Xây acceptance discovery và phase runner

Phụ thuộc: P0-S02, P0-S03, P0-S04.

Tạo `phase_p0.rs`, test registry, `Verify-Phase.ps1`. Đăng ký owners C/K tương lai là not implemented. Bắt buộc discover test names không rỗng, chặn ignored required tests. Xuất gate results đọc bằng máy; thiếu command/artifact hoặc child process lỗi đều fail.

Bằng chứng: Negative controls 0 tests, test fail, fixture mất, required test bị ignore đều làm gate fail; riêng P0 pass.

### 4.6. P0-S06 — Thiết lập checks local và CI lặp lại được

Phụ thuộc: P0-S05.

Chạy fmt/clippy/tests/docs trên skeleton; định nghĩa CI Windows/Linux dùng toolchain pin và quyền tối thiểu. Tách online model smoke, mặc định tắt. Ghi tool versions/platform thật.

Bằng chứng: Clone chạy gate không cần credentials; platform chưa có vẫn ghi chưa xác minh đến khi CI chạy.

### 4.7. P0-S07 — Review và bàn giao nền tảng

Phụ thuộc: P0-S01..P0-S06.

Review schema/test registry, dependency justification, startup commands. Viết evidence, ADR chưa chốt, owner/path map thật cho P1. Không đánh dấu C/K tương lai implemented chỉ vì có fixture JSON.

Bằng chứng: P0 gate pass tại revision cuối; agent tiếp theo tái hiện được từ checkout.

## 5. Tests và lệnh kiểm chứng

P0 bắt buộc test schema round trip hợp lệ/sai, hash inputs xác định, typed error rendering, CLI help/config rejection, fixture completeness và negative controls của runner. Chưa có claim implementation C/K chính. Stub báo `not_implemented` được dùng làm scaffold, nhưng không tính là future acceptance case pass.

Lệnh phase tương lai, chạy sau khi đã triển khai các targets:

```powershell
cargo test -p harness-cli --test phase_p0 --locked
pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P0
```

Full gate gồm formatting, clippy, workspace tests, kiểm tra test discovery và docs checks trong sổ tay. Không báo acceptance pass khi filter chỉ chạy 0 tests.

## 6. Kịch bản demo

1. Clone/mở checkout trong môi trường test sạch rồi chạy P0 gate.
2. Chạy `cargo run -p harness-cli --bin ha -- --help` không có API key.
3. Validate fixture chuẩn; hiển thị hai bước đã xong cùng check fail/bước còn lại.
4. Chạy từng negative control trong fixture cô lập; quan sát exit nonzero.
5. Nói rõ validate fixture chưa phải demo persisted-session recovery.

## 7. Exit gate và các cách làm không được phép

Xong bảy steps; workspace build, P0 tests chạy thật, gate phát hiện known-bad inputs, schemas/fixtures có version. Không thêm LLM loop, SQLite runtime, memory extraction, orchestration hay Web. Không cài tools global diện rộng nếu setup assignment chưa cho phép.

Không chuyển phase chỉ dựa trên summary. Gắn kết quả với revision cuối đã test, báo checks chưa chạy, giữ mọi regressions tiền nhiệm. Không sửa fixture expectations chỉ để implementation pass.

## 8. Giao việc và handoff

Integrator sở hữu root manifests/lockfile và tên schema. Sau S03, fixture author được giao riêng có thể chuẩn bị tests trong lúc integrator làm gate; chỉ merge khi IDs/fixtures thống nhất. Handoff P1 có continuation fixture, contract revisions, test-discovery format, build instructions chính xác.

Bàn giao `docs/evidence/P0.en.md`, `P0.vi.md`, cùng handoff tiếp tục được trong `docs/handoffs/` theo sổ tay. Ghi step IDs đã xong, lỗi còn lại, schema changes, commands và next action. Publish cần được cấp quyền rõ trong assignment coding.

## 9. Prompt giao agent

```text
Chỉ triển khai P0. Đọc docs/implementation/README.vi.md và
P0_FOUNDATION.vi.md trong cùng thư mục, các hợp đồng kiến trúc được link,
và quy định repo áp dụng. P0 không có tiền nhiệm; xác minh baseline kiến trúc.
Tạo SPEC phase rồi thực hiện P0-S01..P0-S07 theo dependencies.
Giữ đúng scope phase, bảo toàn thay đổi không liên quan và contracts đã chấp nhận.
Acceptance tests dùng component thật; chỉ mock boundaries bên ngoài phù hợp.
Chạy gate P0 và docs checks hiện có; bàn giao evidence hai ngôn ngữ cùng restart handoff.
Dừng trước phase tiếp. Không spawn agents, commit, push hoặc publish nếu chưa được giao rõ.
Nếu thiếu prerequisite hoặc verification bắt buộc, báo đúng khoảng trống, không nhận đã hoàn tất.
```
