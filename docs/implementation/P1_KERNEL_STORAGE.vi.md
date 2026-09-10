# P1 — Plugin kernel và lưu trữ bền vững

[English](P1_KERNEL_STORAGE.en.md) | Tiếng Việt

Runbook triển khai; trạng thái: **chưa bắt đầu**. Dự toán: 8–11 ngày công. Target files, Rust tests và lệnh `ha` dưới đây là output tương lai, trừ khi checkout thật đã có. Riêng tài liệu này không là bằng chứng hoàn tất.

## 1. Kết quả và điều kiện vào phase

Cần gate [P0](P0_FOUNDATION.vi.md) được chấp nhận. Bàn giao lát cắt tiếp tục công việc bền vững nhỏ nhất: ACK sau commit, giữ nguyên instructions, restore snapshot+tail, một writer, plugin ownership có scope. Dùng scripted actors, chưa làm production LLM loop.

Đọc [sổ tay](README.vi.md), [plan](../RUST_HARNESS_PLAN.vi.md), [hợp đồng plugin](../PLUGIN_ARCHITECTURE.vi.md), [hợp đồng memory](../MEMORY_AND_CONTINUITY.vi.md), [bảng nghiệm thu](ACCEPTANCE_MAP.vi.md). Inspect evidence tiền nhiệm thật trước khi code.

## 2. Scope sở hữu và target files

Sở hữu `crates/harness-kernel/`, `crates/harness-store-sqlite/`, `crates/harness-session/`, contract modules, migrations ban đầu, `crates/harness-cli/tests/phase_p1.rs`, crash helpers chỉ cho fixture. Mở rộng CLI `init`, `sessions list`, `status`, `plugins list/inspect`, `config explain` cho metadata đã thực sự triển khai.

## 3. Contracts cần chốt trước implementation

Chốt API SQLite transaction coordinator, host/session/task ownership trước khi consumers code theo. Quy định input idempotency, event sequence CAS, instruction ledger updates, snapshot coverage/version/hash, durable source-work markers, typed storage failures. Test được shutdown phases/generations kernel không cần model. Chuẩn bị schema memory jobs nhưng chưa làm extraction.

## 4. Work items theo thứ tự

### 4.1. P1-S01 — Chốt interfaces persistence và lifecycle

Phụ thuộc: P0 đã được chấp nhận.

Đặc tả transaction commands/read views, process lock lifetime, task/session fencing, plugin manifest/service keys, shutdown report. Viết SQL constraints/migration plan cùng failure fixtures P1.

Bằng chứng: Không hứa atomic commits xuyên backend độc lập; outcomes thiếu dependency/lỗi có tên rõ.

### 4.2. P1-S02 — Làm SQLite foundation và ownership

Phụ thuộc: P1-S01.

Mở SQLite local-disk với foreign keys, WAL, busy timeout, FULL durable writes. Làm một writable-host lock, writes kiểm tra generation; read-only client không migrate. Thêm transactional migrations/fault injection storage.

Bằng chứng: C15/C24 chặn hai session/task owners cạnh tranh; writer stale/lỗi không ACK data được.

### 4.3. P1-S03 — Làm plugin registry và resources

Phụ thuộc: P1-S01.

Thêm graph validation, nearest scoped lookup, exact-registration undo, service leases, unpublished resource collection, rollback, drain dependents khi mất provider. Join cleanup trước đóng services phụ thuộc; báo mọi lỗi.

Bằng chứng: K01–K07 component fixtures kiểm tra services trùng/thiếu/cycle, optional extractor, stale disposer, shutdown cạnh tranh.

### 4.4. P1-S04 — Commit input và WorkingState từ events

Phụ thuộc: P1-S02.

Một transaction ghi inbox, admitted user text, instruction ledger, task projection, source-work marker. Áp stable input IDs, expected sequence, actor authority. Fold event xác định; không biến model proposal thành observed receipt.

Bằng chứng: C01 và duplicate-ID fixture chỉ có một input logic; text chưa phân loại được giữ trước context tests sau này.

### 4.5. P1-S05 — Làm artifacts, snapshots và recovery

Phụ thuộc: P1-S02, P1-S04.

Publish artifact đã flush trước reference; validate snapshot schema/hash rồi fold committed tail. Snapshot hỏng fallback journal; unknown critical/corrupt journal báo rõ. Pending intent là uncertain, không tự completed.

Bằng chứng: C02 phục hồi synthetic receipt đã commit nhưng chưa trong snapshot; không bỏ tail, corruption lỗi rõ.

### 4.6. P1-S06 — Kiểm tra crash và I/O boundaries

Phụ thuộc: P1-S03, P1-S05.

Chạy disposable child host; đồng bộ bằng failpoints/ACKs rõ, terminate rồi reopen cùng data directory để assert state. Inject SQLite write failure ở admission/intent/settlement, không làm đầy disk user. Kiểm tra store đóng cuối.

Bằng chứng: C21 không ACK sai hay side effect tiếp sau commit lỗi. State qua process termination thật, không chỉ tạo lại Rust struct.

### 4.7. P1-S07 — Mở inspectable state và hoàn tất gate

Phụ thuộc: P1-S01..P1-S06.

Nối metadata/status commands tới read services, JSON schema, diagnostics. Chạy demo hai bước xong/một bước còn; đăng ký exact test names, chuẩn bị contracts/evidence cho P2.

Bằng chứng: Status khớp bằng chứng đã lưu và nói rõ execution runtime chưa có.

## 5. Tests và lệnh kiểm chứng

Primary component cases: C01, C02, C15, C21, C24, K01–K07. Thêm property tests fold event/snapshot+tail tương đương, duplicate IDs, registration undo. K02 dùng restore fixture tối thiểu không extractor; K06 dùng controlled service call. P2/P3/P5 tăng cường bằng loop/tools/children thật.

Lệnh phase tương lai, chạy sau khi đã triển khai các targets:

```powershell
cargo test -p harness-cli --test phase_p1 --locked
pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P1
```

Full gate gồm formatting, clippy, workspace tests, kiểm tra test discovery và docs checks trong sổ tay. Không báo acceptance pass khi filter chỉ chạy 0 tests.

## 6. Kịch bản demo

1. Acceptance helper tạo temporary store, nhận task chuẩn.
2. Commit hai synthetic work receipts, còn một item, in input/result ACK.
3. Parent terminate helper tại failpoint đã ACK.
4. Helper mới mở store; `ha status <session-id> --json` hiển thị đúng state.
5. Thử writer cạnh tranh, inject write failure; xác nhận từ chối rõ/không báo completed sai. Synthetic receipts không là kết quả test code thật.

## 7. Exit gate và các cách làm không được phép

Primary cases và predecessor checks chạy thật; crash recovery dùng disposable process thật. Không để queue RAM là bản duy nhất của công việc, không ACK trước commit, không cleanup async chỉ bằng `Drop`. Chưa thêm memory LLM extraction, shell execution hay multi-agent scheduling.

Không chuyển phase chỉ dựa trên summary. Gắn kết quả với revision cuối đã test, báo checks chưa chạy, giữ mọi regressions tiền nhiệm. Không sửa fixture expectations chỉ để implementation pass.

## 8. Giao việc và handoff

Sau S01 có thể giao S03 kernel và S02 store song song cho owners không trùng. S04–S06 tích hợp sau dependencies. Một integrator giữ migration numbering/shared contracts. P2 nhận APIs transaction/recovery, snapshot format, plugin leases, failure-injection protocol.

Bàn giao `docs/evidence/P1.en.md`, `P1.vi.md`, cùng handoff tiếp tục được trong `docs/handoffs/` theo sổ tay. Ghi step IDs đã xong, lỗi còn lại, schema changes, commands và next action. Publish cần được cấp quyền rõ trong assignment coding.

## 9. Prompt giao agent

```text
Chỉ triển khai P1. Đọc docs/implementation/README.vi.md và
P1_KERNEL_STORAGE.vi.md trong cùng thư mục, các hợp đồng kiến trúc được link,
và quy định repo áp dụng. Xác minh gate tiền nhiệm bằng source/evidence.
Tạo SPEC phase rồi thực hiện P1-S01..P1-S07 theo dependencies.
Giữ đúng scope phase, bảo toàn thay đổi không liên quan và contracts đã chấp nhận.
Acceptance tests dùng component thật; chỉ mock boundaries bên ngoài phù hợp.
Chạy phase gate và regressions tiền nhiệm; bàn giao evidence hai ngôn ngữ cùng restart handoff.
Dừng trước phase tiếp. Không spawn agents, commit, push hoặc publish nếu chưa được giao rõ.
Nếu thiếu prerequisite hoặc verification bắt buộc, báo đúng khoảng trống, không nhận đã hoàn tất.
```
