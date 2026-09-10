# P5 — Giao việc, task DAG và workspace cô lập

[English](P5_MULTI_AGENT.en.md) | Tiếng Việt

Runbook triển khai; trạng thái: **chưa bắt đầu**. Dự toán: 8–12 ngày công. Target files, Rust tests và lệnh `ha` dưới đây là output tương lai, trừ khi checkout thật đã có. Riêng tài liệu này không là bằng chứng hoàn tất.

## 1. Kết quả và điều kiện vào phase

Cần [P4](P4_MEMORY.vi.md) được chấp nhận. Bàn giao một coordinator và tối đa ba workers có contexts riêng, task ownership/results bền vững, editing worktrees riêng. Tất cả trong cùng writable host; daemon/remote workers ngoài phase.

Đọc [sổ tay](README.vi.md), [plan](../RUST_HARNESS_PLAN.vi.md), [hợp đồng plugin](../PLUGIN_ARCHITECTURE.vi.md), [hợp đồng memory](../MEMORY_AND_CONTINUITY.vi.md), [bảng nghiệm thu](ACCEPTANCE_MAP.vi.md). Inspect evidence tiền nhiệm thật trước khi code.

## 2. Scope sở hữu và target files

Sở hữu `crates/harness-orchestrator/`, host task/DAG/delivery commands, tích hợp `WorkspaceManager`, agent profiles/presets, `crates/harness-cli/tests/phase_p5.rs`, clean/dirty repo fixtures, CLI `run --agents`, task status/cancel/handoff views. Mở rộng memory grants/runtime factory hiện có, không tạo agent engine riêng.

## 3. Contracts cần chốt trước implementation

Đặc tả task lifecycle/DAG, delegated budgets/depth, actor/profile/run IDs, generations, result acceptance, message deduplication. Chốt worktree input snapshot, file ownership, integration order, fingerprint checks. Editing delegation mặc định cần repo fixture sạch; từ chối dirty input đến khi có snapshot path kiểm thử bảo toàn thay đổi.

## 4. Work items theo thứ tự

### 4.1. P5-S01 — Chốt delegation/task contracts

Phụ thuộc: P4 đã được chấp nhận.

Viết task brief/result schemas: objective, acceptance criteria, inputs, base snapshot, grants, budget, deadline, artifacts, exact checked revisions. Tách worker report khỏi host-accepted completion. Chốt parent cancel/pause, depth/slots tối đa.

Bằng chứng: Role name không cấp quyền; chặn circular dependency/task owner mơ hồ.

### 4.2. P5-S02 — Làm durable DAG, ownership, delivery

Phụ thuộc: P5-S01.

Tạo host-owned transactional task transitions, dependency checks, generations, parent delivery records. Child completion và parent message commit chung. Consume một lần logic; notification chỉ đánh thức reader. Parent session chưa chạy nhận inbox data, không bị append log tùy ý.

Bằng chứng: C11/C25 giữ child result đã xong qua parent shutdown/mất notification.

### 4.3. P5-S03 — Làm scheduler và budgets agents

Phụ thuộc: P5-S01, P5-S02.

Spawn/factory-create child actors có context/inbox/cancellation riêng trong owned plugin resources. Giới hạn concurrency/depth/model requests/tổng phí. Parent chờ thì nhả compute permit; shutdown cancel/drain descendants.

Bằng chứng: Không deadlock parent giữ hết slots; restore host không giao lại việc đã xong.

### 4.4. P5-S04 — Làm clean-worktree isolation

Phụ thuộc: P5-S01.

Tạo editing worktrees do host quản lý từ cùng input snapshot đã xác minh. Serialize shared Git metadata, theo ownership branch/worktree, chặn thay đổi ngoài dự kiến. Read-only workers có quyền giới hạn rõ. Chỉ thêm dirty snapshot khi chứng minh giữ index/HEAD/untracked selections.

Bằng chứng: Hai workers không ghi đè file của nhau; không tự stash/reset thay đổi user.

### 4.5. P5-S05 — Làm result integration theo revision

Phụ thuộc: P5-S02, P5-S03, P5-S04.

Validate scope/artifacts/receipts, integrate theo dependencies trong integration worktree, phát hiện conflicts, chạy final checks. Trước áp vào worktree user, recheck fingerprint; đổi thì từ chối/xin hướng xử lý conflict.

Bằng chứng: Test pass từng branch không thay integrated-revision pass; worker không tự push/integrate.

### 4.6. P5-S06 — Nối scoped memory và crash recovery

Phụ thuộc: P5-S03, P5-S05.

Bind task/profile assets khi spawn; handoff ghi exact source versions. Updates sau vào qua logged boundary. Crash ở child completion, parent delivery, integration; reconcile uncertain side effects, dựng lại DAG progress từ durable state.

Bằng chứng: C07/C08/C24 đúng với delegated actors thật; semantic memory không quyết định task completed.

### 4.7. P5-S07 — Mở delegation CLI và chạy demo đầy đủ

Phụ thuộc: P5-S01..P5-S06.

Nối `--agents 3`, task/worker status, blockers, cancel, result inspection. Chạy explorer/coder/verifier qua fixture phối hợp có giới hạn requests. Tách số roles sẵn có khỏi số slots đồng thời thật.

Bằng chứng: CLI hiện đúng ownership, input/result revisions, việc còn sau restart.

## 5. Tests và lệnh kiểm chứng

Primary: C11, C25. Tăng cường C03/C07/C08/C10/C15/C23/C24/K04/K06/K07 với actors/worktrees thật. Thêm DAG cycle, dependency failure propagation, parent-wait fairness, budget exhaustion, depth cap, concurrent Git metadata, final-worktree-change. Child response rỗng hoặc chỉ nói 'done' thiếu artifacts không chốt work accepted.

Lệnh phase tương lai, chạy sau khi đã triển khai các targets:

```powershell
cargo test -p harness-cli --test phase_p5 --locked
pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P5
```

Full gate gồm formatting, clippy, workspace tests, kiểm tra test discovery và docs checks trong sổ tay. Không báo acceptance pass khi filter chỉ chạy 0 tests.

## 6. Kịch bản demo

1. Chạy `ha run <fixture-task> --agents 3` với một coordinator/ba worker slots.
2. Explorer ghi source observation; coder sửa worktree; verifier test exact result revision.
3. Kill host sau child result commit nhưng trước parent notification/consumption.
4. Resume: không giao lại child đã xong; child chưa xong tiếp tục từ durable handoff.
5. Integrate, chạy lại final checks, chỉ trình final integrated revision là current evidence.

## 7. Exit gate và các cách làm không được phép

Delegation/durable coordination/final-revision checks pass; không mất child result, trùng ownership, DAG deadlock, memory leak giữa agents. Editing clean repo bắt buộc; dirty support phải có preservation tests hoặc bị từ chối rõ. Không nhận agent chạy sau CLI exit, không remote agents/marketplace.

Không chuyển phase chỉ dựa trên summary. Gắn kết quả với revision cuối đã test, báo checks chưa chạy, giữ mọi regressions tiền nhiệm. Không sửa fixture expectations chỉ để implementation pass.

## 8. Giao việc và handoff

Sau S01 có thể giao workspace S04/task store S02 riêng; runtime S03 cần S02. Integrator sở hữu S05. P6 nhận entry points spawn/tool, scope/authority propagation, cross-agent recovery fixtures để extensions không bỏ qua.

Bàn giao `docs/evidence/P5.en.md`, `P5.vi.md`, cùng handoff tiếp tục được trong `docs/handoffs/` theo sổ tay. Ghi step IDs đã xong, lỗi còn lại, schema changes, commands và next action. Publish cần được cấp quyền rõ trong assignment coding.

## 9. Prompt giao agent

```text
Chỉ triển khai P5. Đọc docs/implementation/README.vi.md và
P5_MULTI_AGENT.vi.md trong cùng thư mục, các hợp đồng kiến trúc được link,
và quy định repo áp dụng. Xác minh gate tiền nhiệm bằng source/evidence.
Tạo SPEC phase rồi thực hiện P5-S01..P5-S07 theo dependencies.
Giữ đúng scope phase, bảo toàn thay đổi không liên quan và contracts đã chấp nhận.
Acceptance tests dùng component thật; chỉ mock boundaries bên ngoài phù hợp.
Chạy phase gate và regressions tiền nhiệm; bàn giao evidence hai ngôn ngữ cùng restart handoff.
Dừng trước phase tiếp. Không spawn agents, commit, push hoặc publish nếu chưa được giao rõ.
Nếu thiếu prerequisite hoặc verification bắt buộc, báo đúng khoảng trống, không nhận đã hoàn tất.
```
