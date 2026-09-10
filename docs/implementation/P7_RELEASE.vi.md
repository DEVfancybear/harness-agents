# P7 — Gia cố recovery và phát hành CLI

[English](P7_RELEASE.en.md) | Tiếng Việt

Runbook triển khai; trạng thái: **chưa bắt đầu**. Dự toán: 8–11 ngày công. Target files, Rust tests và lệnh `ha` dưới đây là output tương lai, trừ khi checkout thật đã có. Riêng tài liệu này không là bằng chứng hoàn tất.

## 1. Kết quả và điều kiện vào phase

Cần [P6](P6_EXTENSIONS.vi.md) được chấp nhận. Gia cố/đóng gói CLI; chứng minh backup/migration nhất quán và retention rõ. Đây là CLI release gate, không phải quyền deploy lên production host hay publish GitHub release.

Đọc [sổ tay](README.vi.md), [plan](../RUST_HARNESS_PLAN.vi.md), [hợp đồng plugin](../PLUGIN_ARCHITECTURE.vi.md), [hợp đồng memory](../MEMORY_AND_CONTINUITY.vi.md), [bảng nghiệm thu](ACCEPTANCE_MAP.vi.md). Inspect evidence tiền nhiệm thật trước khi code.

## 2. Scope sở hữu và target files

Sở hữu backup/restore/migration/retention qua `harness-store-sqlite`, memory/session APIs, `ha doctor`, maintenance commands, `crates/harness-cli/tests/phase_p7.rs`, packaging/release automation, continuity eval fixtures, operator docs. Giữ toàn bộ contracts runtime/plugin trước.

## 3. Contracts cần chốt trước implementation

Chốt backup manifest/schema/artifact pins, restore vào directory mới rồi activate, migration version checks, tombstones, invalidate khác delete, GC retention roots. Chốt supported platform/capability matrix, release quality gates, provider-smoke policy. Fixtures deletion/restore chỉ dùng disposable data, không active store user.

## 4. Work items theo thứ tự

### 4.1. P7-S01 — Chốt release matrix/failure model

Phụ thuộc: P6 đã được chấp nhận.

Liệt kê OS/toolchain/provider/runner modes hỗ trợ, mọi C/K case và executable names. Xác định checks chưa làm/component-only cần tăng cường end-to-end. Chốt benchmark hardware/dataset và live-evaluation budget hữu hạn.

Bằng chứng: Release checklist không bỏ ngầm platform/case bắt buộc đang fail/chưa hỗ trợ.

### 4.2. P7-S02 — Làm backup nhất quán/restore cô lập

Phụ thuộc: P7-S01.

Dùng SQLite snapshot/backup được hỗ trợ với manifest version, referenced artifacts/hashes. Giữ retention pins lúc backup. Restore vào directory mới, validate integrity/schema/artifacts/tombstones; chỉ activate bằng maintenance action rõ.

Bằng chứng: C27 chặn backup thiếu, mất artifacts, overwrite active data directory.

### 4.3. P7-S03 — Làm upgrade/downgrade safeguards

Phụ thuộc: P7-S02.

Test migrations từ old fixtures hỗ trợ trên bản copy, giữ old event decoding, dựng lại projections có version. Cần backup phục hồi được trước destructive schema evolution. Binary cũ từ chối writes không tương thích; diagnosis có thể read-only.

Bằng chứng: C27 upgrade hoạt động với backlog/việc chưa xong; migration gián đoạn an toàn, downgrade không hỗ trợ không được ghi.

### 4.4. P7-S04 — Làm retention/forget/safe collection

Phụ thuộc: P7-S01, P7-S02.

Tách APIs invalidate/archive/forget. Xóa cần target/confirmation rõ, lan tới derived payloads/indexes/caches, thêm tombstones chống trích xuất lại. Pin unfinished work/backups; GC chỉ unreferenced artifacts qua grace period.

Bằng chứng: C28 báo evidence phiên cũ giảm, chặn dữ liệu bị xóa xuất hiện lại; công bố external/backup copies còn tồn tại.

### 4.5. P7-S05 — Chạy adversarial recovery/privacy suite

Phụ thuộc: P7-S03, P7-S04.

Chạy C01–C30/K01–K14 với stores/processes thật và integrated actors khi áp dụng. Lặp race/crash fixtures bằng seeds ghi lại; bao phủ quota, I/O failure, snapshot/journal corrupt, secret/path policy, unsupported schema. Test đường lỗi checker.

Bằng chứng: Required tests chạy thật, không ignore/0-test; không dùng kết quả giữa chừng làm final evidence.

### 4.6. P7-S06 — Đo và đóng gói CLI thật

Phụ thuộc: P7-S05.

Đo retrieval/restore targets trên dataset đã ghi; chạy mười continuity evaluations nhiều bước với baselines cố định, báo model variability. Build Windows/Linux artifacts, checksums, dependency notices. Smoke packaged binary, không chỉ `cargo run`; live provider dùng credential local/budget rõ.

Bằng chứng: Công bố mục tiêu hiệu năng chưa đạt; không làm yếu durability. Tách keyless tests/live-model results.

### 4.7. P7-S07 — Chuẩn bị release evidence/operator handoff

Phụ thuộc: P7-S01..P7-S06.

Viết install/config/doctor/resume/recovery/backup guides và limitations hai ngôn ngữ. Xác minh startup packaged binary, source/CI identity release candidate. Nếu được phép publish thì chỉ phát hành artifacts đã test và verify remote; không thì bàn giao local candidate.

Bằng chứng: User thấy chính xác cái đã làm/test, cái chưa xác minh, chưa deploy.

## 5. Tests và lệnh kiểm chứng

Primary: C27, C28. Final release gate chạy lại mọi C/K case cùng platform/process/integration suites trước. Thêm migration interruption, tombstone/backlog restoration, backup-GC race, active-store overwrite denial, corrupted artifact. Benchmark targets vẫn là mục tiêu phải đo, không thành achieved numbers bằng lời nói. Platform bắt buộc fail thì không nhận release đã kiểm chứng đầy đủ.

Lệnh phase tương lai, chạy sau khi đã triển khai các targets:

```powershell
cargo test -p harness-cli --test phase_p7 --locked
pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P7
```

Full gate gồm formatting, clippy, workspace tests, kiểm tra test discovery và docs checks trong sổ tay. Không báo acceptance pass khi filter chỉ chạy 0 tests.

## 6. Kịch bản demo

1. Bắt đầu task hai bước xong, check fail, child còn việc và extraction backlog.
2. Backup nhất quán khi còn việc; restore vào fresh data directory.
3. Resume, verify source state/task ownership/pending jobs không lặp side effects.
4. Forget một source đã chọn trên disposable copy; memory dẫn xuất không quay lại, old replay báo thiếu.
5. Chạy recovery bằng packaged Windows/Linux binary; ghi checksums/evidence chính xác.

## 7. Exit gate và các cách làm không được phép

Cả 44 C/K cases có final executable evidence; backup/retention gates pass, platform matrix trung thực. Không nhận production activation từ local/CI success. Nếu thiếu credentials cho required live-provider check, ghi chưa xác minh và xin quyết định scope release, không bịa pass. Chưa làm Web/daemon.

Không chuyển phase chỉ dựa trên summary. Gắn kết quả với revision cuối đã test, báo checks chưa chạy, giữ mọi regressions tiền nhiệm. Không sửa fixture expectations chỉ để implementation pass.

## 8. Giao việc và handoff

Sau S01 có thể giao S02 backup/restore và thiết kế retention riêng, nhưng GC implementation đợi backup pin contracts. Một store owner giữ migrations. Evals/packaging hội tụ về một exact release candidate revision. P8 nhận application services ổn định, command/error schemas, CLI baseline phải bảo toàn.

Bàn giao `docs/evidence/P7.en.md`, `P7.vi.md`, cùng handoff tiếp tục được trong `docs/handoffs/` theo sổ tay. Ghi step IDs đã xong, lỗi còn lại, schema changes, commands và next action. Publish cần được cấp quyền rõ trong assignment coding.

## 9. Prompt giao agent

```text
Chỉ triển khai P7. Đọc docs/implementation/README.vi.md và
P7_RELEASE.vi.md trong cùng thư mục, các hợp đồng kiến trúc được link,
và quy định repo áp dụng. Xác minh gate tiền nhiệm bằng source/evidence.
Tạo SPEC phase rồi thực hiện P7-S01..P7-S07 theo dependencies.
Giữ đúng scope phase, bảo toàn thay đổi không liên quan và contracts đã chấp nhận.
Acceptance tests dùng component thật; chỉ mock boundaries bên ngoài phù hợp.
Chạy phase gate và regressions tiền nhiệm; bàn giao evidence hai ngôn ngữ cùng restart handoff.
Dừng trước phase tiếp. Không spawn agents, commit, push hoặc publish nếu chưa được giao rõ.
Nếu thiếu prerequisite hoặc verification bắt buộc, báo đúng khoảng trống, không nhận đã hoàn tất.
```
