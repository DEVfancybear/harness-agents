# ADR-N11 — Schedule occurrences, clock semantics và daemon ownership

**Trạng thái:** accepted trong M11 · **Ngày:** 23/09/2026 · **Phạm vi:** M11-01..M11-04

[Runbook M11](../implementation-next/M11.vi.md) · [SPEC M11](../specs/M11.vi.md) · [ADR-N10](ADR-N10-WEB-STACK-AND-IDENTITY.vi.md)

## 1. Bối cảnh

M11 thêm một thứ mới về bản chất: một tiến trình sống lâu hơn client, và một cái đồng hồ quyết định khi nào việc bắt
đầu. Cả hai đều là nguồn lỗi kinh điển — launch trùng sau restart, và một occurrence chạy hai lần vì đồng hồ đổi
giờ. ADR này chốt bốn quyết định.

## 2. Quyết định

### D1 — Occurrence key là `(schedule, revision, nominal due)`, không bao giờ là wall clock

`occurrence_key(schedule_id, revision, due_unix_ms)`. **Thời điểm claim không nằm trong key.**

**Lý do:** nếu key chứa thời điểm claim thì hai host claim cùng một occurrence sẽ tạo hai key khác nhau, và tính
idempotent biến mất đúng ở chỗ cần nó. Key theo nominal due làm cho hai host **tính ra cùng một chuỗi**, nên PRIMARY
KEY của bảng là cơ chế chống launch trùng — không phải lock, không phải set trong process.

### D2 — Claim là một transaction: insert occurrence + advance `next_due`

`claim_occurrence` làm cả hai trong một transaction, và trả `false` khi occurrence đã tồn tại. Lần claim thứ hai
**không** advance `next_due` lần nữa: lần đầu đã advance, advance lần nữa là bỏ qua một occurrence chưa ai chạy.

### D3 — Claimed nhưng chưa launch thì **cancel**, không relaunch

Host tìm thấy một occurrence `claimed` không thể biết effect đã xảy ra chưa. Relaunch chính là cái double mà claim
sinh ra để chặn. Nó bị đánh `canceled` và **được báo** để người vận hành thấy khoảng trống.

### D4 — Cron đi trên đồng hồ **local**, và hai chiều DST xử lý ngược nhau

- **Spring forward:** giờ local không tồn tại ⇒ occurrence bị **skip**, không dời sang giờ khác. Dời là chạy một
  việc vào giờ người dùng không yêu cầu.
- **Fall back:** giờ local lặp lại ⇒ **chỉ chạy một lần**. Bước nhảy phải là một **local minute**, không phải một UTC
  minute: cộng một phút UTC sau fall-back rơi lại đúng local minute vừa chạy.

Đây là **bug thật** mà A34 bắt được: giờ lặp chạy hai lần.

### D5 — Zone viết ra, không kéo database IANA

`ScheduleZone` biết `UTC` và central European time với **các instant chuyển đổi thật của 2026–2027**. Zone không biết
⇒ **typed refusal**, không fallback về UTC.

**Lý do:** một database IANA đầy đủ là một cây dependency cho một thứ mà hành vi cần đúng được quyết định bởi
**các instant chuyển đổi**, không bởi kích thước database. Và một schedule âm thầm chạy sai zone tệ hơn một schedule
từ chối chạy.

### D6 — Misfire bị chặn trần

`MAX_CATCH_UP_OCCURRENCES = 1`, và số bị bỏ được **báo**. Daemon tắt một tuần không được launch một tuần việc.

### D7 — Manual trigger là occurrence riêng, không đụng `next_due`

Một lần chạy tay có key riêng và `trigger_kind = manual`; `next_due_unix_ms` giữ nguyên chỗ evaluator đặt.

### D8 — Pause bump revision, và claim cũ bị từ chối

`set_schedule_state` tăng `revision`; một occurrence đang chờ mang revision cũ nên bị `SequenceConflict`. Đây là
cách một pause "đang xếp hàng" được tôn trọng.

### D9 — Schedule không bao giờ mang auto-approval

`LaunchGrants.auto_approve_tools` **luôn false** trong release này, và được **lưu** để người đọc thấy đó là quyết
định chứ không phải thiếu sót. `edit_workspace` cũng không được nới.

## 3. Hệ quả

| Hệ quả | Xử lý |
|---|---|
| Runtime store schema 2 → **3** | Hai bảng mới, additive; store version 2 mở được và upgrade tại chỗ |
| `chrono` thành direct dependency | Đã có trong lock; `chrono-tz` **không** thêm |
| Daemon là module của `harness-cli`, không phải binary thứ hai | `ha` vẫn là entry point duy nhất |

## 4. Phương án bị bác bỏ

- **Key theo thời điểm claim.** Hai host claim cùng occurrence tạo hai key ⇒ mất idempotent.
- **Relaunch occurrence `claimed`.** Không biết effect đã xảy ra chưa; relaunch là double.
- **Dời occurrence spring-forward sang giờ kế tiếp.** Chạy việc vào giờ người dùng không yêu cầu.
- **Bước UTC trong cron walk.** Giờ lặp chạy hai lần (đã đo).
- **Fallback UTC cho zone lạ.** Chạy sai zone mà không nói gì.
- **Catch-up không trần.** Daemon tắt lâu sẽ launch cả backlog.
- **Auto-approve trong schedule.** Schedule template không được bao gồm quyền approve mọi tool.
