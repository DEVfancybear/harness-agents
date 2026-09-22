# ADR-N10 — Web stack, identity và event cursor

**Trạng thái:** accepted trong M10 · **Ngày:** 23/09/2026 · **Phạm vi:** M10-01..M10-04

[Runbook M10](../implementation-next/M10.vi.md) · [P8 web plan](../implementation/P8_WEB.vi.md) · [SPEC M10](../specs/M10.vi.md)

## 1. Bối cảnh

P8 đã viết contract cho web (route/version, request ID, cursor, view model đã lọc, loopback không thay thế auth) nhưng
chưa implement. M10 là mốc đầu tiên dựng nó. Ba câu hỏi phải chốt trước code: **stack nào**, **identity ở đâu**, và
**cursor là gì**.

## 2. Quyết định

### D1 — HTTP server dùng crate đã có trong lockfile, không thêm framework

`hyper 1.11.1` + `hyper-util 0.1.20` (`TokioIo`) + `http 1.5.0` + `http-body-util 0.1.5` + `bytes 1.12.1`, tất cả
**đã có trong `Cargo.lock`** vì `reqwest` kéo chúng vào. Không thêm axum/warp/actix.

**Lý do:** P8 §3 yêu cầu "chọn và pin HTTP/frontend dependencies thật theo tài liệu chính thức", không yêu cầu một
framework. Một server loopback một người dùng không đáng một cây dependency mới trong lockfile: nó tăng audit
surface, tăng thời gian CI, và tạo thêm một thứ phải theo dõi CVE. Dùng crate đã có giữ lockfile ổn định và vẫn là
HTTP/1.1 thật (keep-alive, chunked, streaming body cho SSE) chứ không phải một parser tự viết.

### D2 — Frontend không có build step

HTML/CSS/JS tĩnh nhúng bằng `include_str!`, phục vụ từ cùng binary. Không dev server, không Node trong CI.

**Lý do:** P8 §2 cho phép "chọn frontend stack tối thiểu một cách rõ ràng; runbook không ép framework", và §8 yêu cầu
"build và serve static assets khớp version mà không phụ thuộc development server". Asset nhúng thoả cả hai: thứ được
phục vụ chính là thứ đã build, và không có bước nào có thể lệch version.

### D3 — Identity resolve server-side; loopback **không** miễn auth

Mọi mutation đòi header `X-Ha-Session` khớp token sinh lúc khởi động, và `Origin`/`Host` phải nằm trong allowlist
loopback. Token in ra stdout lúc khởi động, **không** vào URL, không vào log, không vào history.

**Lý do:** P8 §3 nói thẳng "Loopback không thay thế authentication". Một tiến trình khác trên cùng máy vẫn kết nối
được tới loopback; đó là toàn bộ lý do token tồn tại.

### D4 — Cursor là `sequence` của journal, không phải số đếm của buffer

SSE `id:` là `EventEnvelope.seq` của session. `Last-Event-ID`/`?cursor=` được so với **oldest retained** của buffer;
nếu cursor cũ hơn, server phát một event `gap` **tường minh** kèm range còn giữ, rồi client phải reload projection
trước khi nghe tail.

**Lý do:** A33 yêu cầu "gap explicit" và negative control "silently replay partial buffer phải fail". Một cursor là
số đếm nội bộ của buffer thì không so được với journal, và client không có cách nào biết mình đã mất event.

### D5 — Heartbeat không phải durable state

Comment `:` của SSE chỉ giữ kết nối sống. Nó không mang sequence, không vào reducer, và không được tính là tiến triển.

**Lý do:** nếu heartbeat mang sequence thì một client đang chậm sẽ tưởng mình đã theo kịp.

### D6 — Reducer terminal là monotonic, và thứ tự đến không quyết định

Client bỏ qua event `running` đến sau một `terminal` cho cùng run. `RunState` phía host đã monotonic
(`m0_01_run_state_reducer_never_regresses_from_terminal`); client phải giữ cùng luật, nếu không một lần reconnect
muộn sẽ vẽ lại "đang chạy" cho một run đã xong.

### D7 — Không SQL trong adapter

Route chỉ gọi service hiện có (`SessionService`, `RuntimeService`, store read API). Không route nào tự viết
`INSERT`/`UPDATE`.

**Lý do:** P8 §8 liệt kê "routes ghi SQLite trực tiếp" là điều không được phép. Adapter mỏng là điều kiện để
ownership/policy/durability vẫn thuộc owner hiện có.

## 3. Hệ quả

| Hệ quả | Xử lý |
|---|---|
| `harness-cli` thêm direct dependency `hyper`/`hyper-util`/`http-body-util`/`bytes` | Version đã có trong lock; nếu `Cargo.lock` đổi thì ghi rõ trong evidence |
| `ha web` là subcommand mới | Không đổi subcommand cũ; UI là asset nhúng |
| Loopback + token | Test A33 khẳng định 401/403 thật, không chỉ đọc code |
| Buffer bound | Subscriber chậm bị cắt và **được báo** bằng `gap`, không im lặng |

## 4. Phương án bị bác bỏ

- **Thêm axum.** Một cây dependency mới cho một server loopback; không có yêu cầu nào cần nó.
- **Frontend có build step (Vite/React).** Thêm Node vào CI và thêm một bước có thể lệch version với binary.
- **Bỏ auth vì "chỉ loopback".** Chính P8 cấm điều đó, và tiến trình khác trên cùng máy vẫn vào được.
- **Cursor là offset của buffer.** Không so được với journal ⇒ không phát hiện được gap.
- **Heartbeat mang sequence.** Client chậm sẽ tưởng đã theo kịp.
- **Route tự query SQL.** Phá ownership và bỏ qua policy/durability.
