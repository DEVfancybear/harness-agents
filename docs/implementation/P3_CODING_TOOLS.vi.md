# P3 — Coding tools, execution policy và receipts

[English](P3_CODING_TOOLS.en.md) | Tiếng Việt

Runbook triển khai; trạng thái: **chưa bắt đầu**. Dự toán: 7–10 ngày công. Target files, Rust tests và lệnh `ha` dưới đây là output tương lai, trừ khi checkout thật đã có. Riêng tài liệu này không là bằng chứng hoàn tất.

## 1. Kết quả và điều kiện vào phase

Cần [P2](P2_RUNTIME_CONTEXT.vi.md) được chấp nhận. Bàn giao CLI coding một agent hữu dụng: đọc/sửa fixture repo, chạy checks, resume với execution evidence đáng tin. Host-development mode phải báo mức bảo vệ thật.

Đọc [sổ tay](README.vi.md), [plan](../RUST_HARNESS_PLAN.vi.md), [hợp đồng plugin](../PLUGIN_ARCHITECTURE.vi.md), [hợp đồng memory](../MEMORY_AND_CONTINUITY.vi.md), [bảng nghiệm thu](ACCEPTANCE_MAP.vi.md). Inspect evidence tiền nhiệm thật trước khi code.

## 2. Scope sở hữu và target files

Sở hữu modules `crates/harness-tools/` cho policy/approval, filesystem/search, Git, process runners, receipts; revision observation trong session services; `crates/harness-cli/tests/phase_p3.rs`; disposable repo/process fixtures. Mở rộng tool boundary của loop, không tạo executor thứ hai.

## 3. Contracts cần chốt trước implementation

Chốt path/root canonicalization, symlink/junction policy, content hashes before/after, binary/encoding, process executable/argv khác shell script, output limits, approval gắn invocation trước code. Test receipt gắn worktree fingerprint được test, không chỉ HEAD. Công bố runner capabilities; yêu cầu strict isolation trên nền tảng chưa hỗ trợ phải fail closed.

## 4. Work items theo thứ tự

### 4.1. P3-S01 — Đặc tả tools, policy, receipt schemas

Phụ thuộc: P2 đã được chấp nhận.

Định nghĩa tools read/list/search/edit/process/Git/task-update ban đầu và capabilities. Tách proposed action, execution intent, immutable receipt, model/UI view. Approval gắn actor, resolved args, invocation, workspace, tool, policy revision.

Bằng chứng: Schema/guard lỗi trả denied/failed; không mở rộng quyền.

### 4.2. P3-S02 — Làm một execution gate duy nhất

Phụ thuộc: P3-S01.

Validate arguments/identity, transforms, revalidate, approval, monotonic guards, commit intent/approval consumption rồi mới dispatch. Check cancel/revocation khi nhận việc. Persist receipt trước notify observers; presentation không sửa evidence.

Bằng chứng: K08/K09 chặn allow muộn, arguments đổi, success giả; observer lỗi vẫn có receipt bền vững.

### 4.3. P3-S03 — Làm filesystem và search tools

Phụ thuộc: P3-S01, P3-S02.

Làm rooted read/list/search/patch có giới hạn output/size, ignore rules, sensitive-path policy. Recheck observed content hash trước sửa; serialize host edits, phát hiện thay đổi ngoài host. Xử lý Unicode, CRLF, spaces, symlink/junction escape, từ chối binary.

Bằng chứng: Stale edit/path traversal không ghi đè nội dung bất ngờ; searches bounded và có nguồn.

### 4.4. P3-S04 — Làm process runners Windows/Linux

Phụ thuộc: P3-S02.

Hỗ trợ executable/argv và shell request rõ, stdout/stderr bounded, timeout, tree cancellation. Windows test Job Object ownership/descendants; Linux test cơ chế process-tree đã chọn. Future cancel không đồng nghĩa process đã dừng.

Bằng chứng: Grandchild giữ output pipe được dừng/reap hoặc báo chưa rõ; không treo shutdown/báo clean receipt sai.

### 4.5. P3-S05 — Gắn Git observations/checks với file thật

Phụ thuộc: P3-S03, P3-S04.

Làm status/diff, project registration/reassociation checks. Ghi fingerprints tracked/untracked quanh tests/edits. Đổi file sau test vẫn giữ lịch sử nhưng invalid current applicability; task_update không tự tạo runner receipt.

Bằng chứng: C10/C23 và fixtures artifact/secret C29 chặn stale evidence, gộp project sai, đọc hash khác scope.

### 4.6. P3-S06 — Làm uncertainty reconciliation

Phụ thuộc: P3-S02..P3-S05.

Crash sau patch/command thật trên disposable fixture nhưng trước result commit. Restore intent, so before/after, phân loại applied/not applied/unknown bằng kiểm tra theo operation. Không tự chạy lại shell tùy ý.

Bằng chứng: C03 pass; tăng cường C02/C21 bằng side effects thật và settlement storage lỗi.

### 4.7. P3-S07 — Tích hợp coding flow và kiểm chứng platforms

Phụ thuộc: P3-S01..P3-S06.

Chạy parser-fix fixture qua loop P2 và tools mới. Hiển thị checks fail/pass trên exact revisions, edits, việc còn sau restart. Ghi platform capabilities/startup instructions không phụ thuộc package cài sẵn.

Bằng chứng: CLI một agent sửa/test fixture thật; strict sandbox chưa hỗ trợ vẫn deny rõ, không nhận an toàn đã chứng minh.

## 5. Tests và lệnh kiểm chứng

Primary: C03, C10, C23, C29, K08, K09. Tăng cường C02/C21/K06/K07 bằng subprocesses thật. Thêm tests path escape, stale hash, encoding, binary, output quá lớn, timeout, descendant cleanup, cancel races Windows/Linux. Chỉ dùng fake credentials sinh cho tests; không chạy destructive fixtures trong repo user.

Lệnh phase tương lai, chạy sau khi đã triển khai các targets:

```powershell
cargo test -p harness-cli --test phase_p3 --locked
pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P3
```

Full gate gồm formatting, clippy, workspace tests, kiểm tra test discovery và docs checks trong sổ tay. Không báo acceptance pass khi filter chỉ chạy 0 tests.

## 6. Kịch bản demo

1. Tạo temporary parser fixture có test fail và ràng buộc public API.
2. Chạy agent bằng responses mock/model đi qua read/edit/process tools thật.
3. Ghi pass tại R1, đổi source thành R2, cho thấy receipt cũ stale.
4. Kill sau khi fixture command không nên lặp đã ghi marker nhưng chưa có receipt; resume không ghi marker lần hai.
5. Cancel cây child/grandchild và inspect terminal receipt.

## 7. Exit gate và các cách làm không được phép

Edits/checks thật và uncertainty handling pass trên platforms bắt buộc. Capability denied vẫn denied qua nested/presentation code. Không reset/stash user cưỡng ép, process output vô hạn, dùng lại approval cho call đổi quan trọng, hoặc báo sandbox success khi chưa hỗ trợ. Multi-agent worktrees thuộc P5.

Không chuyển phase chỉ dựa trên summary. Gắn kết quả với revision cuối đã test, báo checks chưa chạy, giữ mọi regressions tiền nhiệm. Không sửa fixture expectations chỉ để implementation pass.

## 8. Giao việc và handoff

Sau S02 có thể giao filesystem S03/process S04 riêng; Git observation S05 tích hợp cả hai. Owner policy/receipt giữ shared types. P4 nhận source receipts đáng tin, sensitive-data handling, project fingerprints và common gate.

Bàn giao `docs/evidence/P3.en.md`, `P3.vi.md`, cùng handoff tiếp tục được trong `docs/handoffs/` theo sổ tay. Ghi step IDs đã xong, lỗi còn lại, schema changes, commands và next action. Publish cần được cấp quyền rõ trong assignment coding.

## 9. Prompt giao agent

```text
Chỉ triển khai P3. Đọc docs/implementation/README.vi.md và
P3_CODING_TOOLS.vi.md trong cùng thư mục, các hợp đồng kiến trúc được link,
và quy định repo áp dụng. Xác minh gate tiền nhiệm bằng source/evidence.
Tạo SPEC phase rồi thực hiện P3-S01..P3-S07 theo dependencies.
Giữ đúng scope phase, bảo toàn thay đổi không liên quan và contracts đã chấp nhận.
Acceptance tests dùng component thật; chỉ mock boundaries bên ngoài phù hợp.
Chạy phase gate và regressions tiền nhiệm; bàn giao evidence hai ngôn ngữ cùng restart handoff.
Dừng trước phase tiếp. Không spawn agents, commit, push hoặc publish nếu chưa được giao rõ.
Nếu thiếu prerequisite hoặc verification bắt buộc, báo đúng khoảng trống, không nhận đã hoàn tất.
```
