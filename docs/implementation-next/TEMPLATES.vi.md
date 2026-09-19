# Templates — SPEC, evidence và handoff

[Sổ tay](README.vi.md) · [Prompts](PROMPTS.vi.md)

## 1. SPEC trước khi code

Tạo `vnext/docs/specs/Mn.vi.md` (hoặc work item cụ thể) khi milestone được giao, không tạo mọi SPEC trước. Đây là template, không bằng chứng implementation:

```text
Assignment: M?-?? ; source/base revision: ... ; working tree changes: ...
Goal / non-goals:
Prerequisites + evidence revisions + verification performed:
Logical module -> actual path map:
Contract versions / schemas / migrations touched:
ADR decisions, defaults overridden and reasons:
Invariants / failure modes / rollback or compatibility behavior:
Ordered slices (each: input -> behavior -> output -> test):
Acceptance IDs + exact integration target/test names:
External boundaries mocked; real components exercised:
Platform requirements and unsupported behavior:
Commands expected to run; prerequisites that still need implementation:
Authority already granted (network, paid smoke, commit/push etc.):
```

## 2. Evidence sau verification

Tạo `vnext/docs/evidence/Mn.vi.md`. Khi chưa commit, dùng base commit + reproducible tracked/untracked source manifest/hash, không ghi HEAD như thể toàn diff đã nằm trong commit. Nếu test revision khác final revision, rerun hoặc ghi chính xác docs-only delta và lý do.

```text
Milestone/work items covered:
Implementation status: implemented_unverified / verified_local / accepted
Tested source: commit or base + source-tree/diff digest
OS/architecture, toolchain, lockfile digest, fixture version:
Commands actually executed:
For each suite: expected discovery count, actual count, pass/fail/skip
Acceptance IDs: implemented test names, result, evidence refs
Crash tests: failpoint, child process exit, reopened state assertions
Compatibility/migration/source-scope assertions:
Artifacts/log paths + content hashes (no secrets):
Negative controls: what invariant-breaking variant was rejected
Remaining limitations: platform not run, paid smoke not run, unsupported features
Required gates not run -> milestone cannot be accepted
```

Logs đặt trong `vnext/.artifacts/verification/` có ignore rule, chia sẻ redacted selected evidence; docs không link tới file bị ignore mà reviewer không thể lấy. Artifact manifest có commands để tái tạo. Không commit credentials/generated runtime databases.

## 3. Handoff khi hết context hoặc dừng

Tạo/cập nhật `vnext/docs/handoffs/CURRENT.vi.md`, link milestone-specific handoff nếu cần. File này không thay store state của sản phẩm.

```text
Current assignment and latest user constraints:
Current branch/base/source digest:
Completed work items (with evidence references):
In-progress item and exact last successful boundary:
Changed files and reasons:
Contracts/ADRs already fixed; do not silently change:
Commands actually run and latest failures:
Known uncertain side effects/external jobs (if any):
Remaining ordered TODOs with test oracles:
Next action: exact file/symbol/command and expected outcome
Blocked on: concrete missing input/environment, or none
Do not repeat: actions already completed, paid calls, migrations
```

Handoff phải cho model mới tiếp tục trong vài phút, không viết lại toàn nghiên cứu. Chỉ đề cập symbols/files đã tồn tại trong implementation tại lúc bàn giao; chưa tồn tại ghi planned.

## 4. Review gate trước bàn giao

Reviewer kiểm tra dependency/module ownership, side-effect ordering, failure settlement, authority/provenance, fixture independence, exact test discovery và compatibility. Tìm đường runtime bypass (CLI/API/plugin) chứ không chỉ helper unit tests. Nếu patch thay tests expected để che lỗi, phải có contract change rõ và regression được chứng minh.

Routine review không nhất thiết cần agent khác. Người thực hiện có thể self-review nhưng phải giữ evidence limitations; không tự tạo review approval giả. Khi user chỉ giao plan, không chạy các prompts implementation trong bộ này.
