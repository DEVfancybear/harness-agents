# ADR-N08 — Delegation capacity, child delivery và Git integration

**Trạng thái:** accepted trong M8 · **Ngày:** 23/09/2026 · **Phạm vi:** M8-01..M8-04

[Runbook M8](../implementation-next/M8.vi.md) · [Contracts](../implementation-next/CONTRACTS.vi.md) §8 · [SPEC M8](../specs/M8.vi.md) · [ADR-N02](ADR-N02-STORE-OWNERSHIP-DURABILITY.vi.md)

## 1. Bối cảnh

Orchestrator P5 đã có DAG có kiểm chứng, scheduler giữ compute permit, worktree Git thật, và delivery commit
nguyên tử. Ba khoảng trống chặn M8:

1. **Không có biên cho queue.** `require_admission` chỉ chặn depth và budget request; số worker được dispatch
   nhưng chưa có slot thì không ai chặn. Một caller fan-out quá tay chỉ biết khi hệ điều hành phản đối.
2. **"Dừng vì lý do gì" chỉ là prose.** `StepOutcome.detail` là câu cho người đọc; không có giá trị ổn định để
   nhóm/đếm, và suy ra nó bằng cách so khớp câu chữ là cách một báo cáo bắt đầu nói sai sau mỗi lần đổi wording.
3. **Integration chưa có oracle cho nhánh "branch pass, integrated fail".** `recheck_before_apply` và
   `ChecksFailed` đã tồn tại, nhưng chưa có bằng chứng rằng receipt của branch **không** được dùng làm final proof,
   và chưa có test nào chứng minh worktree sạch/ranh giới cleanup.

## 2. Quyết định

### D1 — Queue là host policy, biểu diễn một lần trên tổng số đã dispatch

`SchedulerConfig::max_queued_workers` (cap `DEFAULT_MAX_QUEUED_WORKERS = 8`) và
`require_queue_capacity(reserved)`, trong đó `reserved` là **running + queued**. Bound được kiểm tra **trước** khi
lấy task reservation, nên một dispatch bị từ chối không để lại reservation nào và không bị charge.

**Lý do:** hai con số (đang chạy, đang chờ) có thể trôi khỏi nhau; một con số mà caller nhìn thấy được thì không.
`SchedulerConfig::max_queued_workers` có `#[serde(default)]` trỏ tới **default của host**, không phải 0: một config
viết trước M8 phải đọc ra "có biên", không phải "vô hạn".

### D2 — Queue đầy là một error code riêng

`ErrorCode::DelegationQueueFull`, retry class `HumanAction`, exit code 3. Không dùng lại `BudgetExhausted`:
queue đầy là backpressure **tự hết** khi một worker settle; budget cạn thì không tự hết. Caller phải phân biệt
"chờ một chút" với "đừng thử nữa".

### D3 — Một delivery là một transaction, notification chỉ là latency

`commit_delivery` ghi task transition + result + parent delivery trong **một** transaction; `consume_delivery`
dedupe theo `message_id` và trả `bool` cho lần thứ hai. Không có đường nào để "biết child đã xong" bằng tìm kiếm
ngữ nghĩa. Parent chết sau commit và trước notification là trạng thái bình thường, không phải mất mát.

### D4 — Task đã `Completed` không bao giờ được claim lại; owner generation cũ không finalize

`claim` từ chối cả hai bằng `TaskOwnershipConflict`, và từ chối **trước** khi có worker nào được tạo. Owner
generation là fence của host, không phải tham số của caller.

### D5 — Receipt của branch không phải bằng chứng của cây tích hợp

`ResultIntegrator::integrate` chỉ chạy `checks` trên **cây đã tích hợp**, và `CheckedRevision.revision` là
`report.final_commit`. Khi một branch không apply được, **không** final commit nào được đưa ra và **không** check
nào chạy trên cây không phải là integration. Đây là quy tắc "không accept merge vì model reviewer nói looks good"
ở dạng dữ liệu: chỉ check trên cây cuối mới là proof.

### D6 — Worktree là biên concurrency, không phải sandbox bảo mật

`inspect_input` từ chối dirty kèm **lý do có cấu trúc** (`DirtyReason`), không stash/reset. `recheck_before_apply`
so fingerprint đích trước khi apply và từ chối kèm lý do. `remove_worktree` chỉ chạm worktree host tạo. Không
tuyên bố cách ly bảo mật ở bất kỳ đâu trong output.

### D7 — "Vì sao dừng" là một nhãn, không phải một câu

`StepOutcome::stop_reason()` trả nhãn ổn định (`accepted`, `completed_unverified`, `worker_failed`, `canceled`,
`evidence_incomplete`, `ready_not_dispatched`, `assigned_not_settled`, `running`, `pending`) và
`claimed_but_unverified()`. CLI in cả hai, cộng khối `verification { accepted, claimed_but_unverified, note }`.
`completed_unverified` là nhãn quan trọng nhất: worker **đã** báo hoàn thành và host **không** chấp nhận bằng chứng
— đúng trường hợp dễ bị đọc nhầm thành thành công.

## 3. Hệ quả

| Hệ quả | Xử lý |
|---|---|
| `SchedulerConfig` thêm field | `#[serde(default)]` về default của host; 5 fixture P5 cập nhật tường minh |
| Error code mới | Bắt buộc khai báo ở `retry_class` và `exit_code` (match toàn phần) — compiler ép, không thể quên |
| `ha tasks run --json` thêm field | Thêm `stop_reason`, `claimed_but_unverified`, khối `verification`; field cũ giữ nguyên nên `p5_s07` vẫn xanh |
| Check trong acceptance A29 | `git diff --quiet --exit-code HEAD`: cùng một lệnh ở branch và ở cây tích hợp, và tồn tại ở mọi nơi sản phẩm tồn tại. `sh` không có trên host này nên một check shell sẽ làm case không chứng minh được |

## 4. Phương án bị bác bỏ

- **Dùng `BudgetExhausted` cho queue đầy.** Nói sai điều caller cần biết: budget cạn là terminal, queue đầy thì không.
- **`#[serde(default)]` với `Default::default()` (0).** Mọi config đã lưu sẽ đọc thành "không cho chờ", tức là
  một thay đổi hành vi âm thầm; hoặc nếu default là `u32::MAX` thì thành vô hạn. Cả hai đều sai.
- **Suy `stop_reason` từ `detail`.** Matching prose là hợp đồng ngầm sẽ vỡ ở lần đổi câu đầu tiên.
- **Check A29 bằng script `sh`.** Không chạy được trên host Windows này ⇒ case chỉ chứng minh được trên một OS.
- **Coi "mọi branch đã apply" là đủ để accept.** Đó chính là negative control của A29.
