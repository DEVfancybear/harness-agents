# P8 — Web UI dùng chung host services

[English](P8_WEB.en.md) | Tiếng Việt

Runbook triển khai; trạng thái: **chưa bắt đầu**. Dự toán: 10–15 ngày công. Target files, Rust tests và lệnh `ha` dưới đây là output tương lai, trừ khi checkout thật đã có. Riêng tài liệu này không là bằng chứng hoàn tất.

## 1. Kết quả và điều kiện vào phase

Bổ sung Web UI local sau khi [P7](P7_RELEASE.vi.md) được nghiệm thu và người dùng giao rõ công việc Web. Tái sử dụng runtime, task coordinator, policy gate và stores đã kiểm thử. Browser reconnect chỉ tiếp tục hiển thị công việc cũ; không tạo agent loop thứ hai.

Mặc định là foreground host `ha web`, bind loopback, phục vụ một người dùng local. Host giữ cùng writer lock của data directory như CLI execution. Nếu CLI writer khác đang giữ directory, trả lỗi busy rõ ràng. Remote hosting, daemon nền và nhiều tài khoản cần thiết kế cùng quyền triển khai riêng.

Đọc [sổ tay](README.vi.md), [plan](../RUST_HARNESS_PLAN.vi.md), [hợp đồng plugin](../PLUGIN_ARCHITECTURE.vi.md), [hợp đồng memory](../MEMORY_AND_CONTINUITY.vi.md), [bảng nghiệm thu](ACCEPTANCE_MAP.vi.md). Inspect evidence tiền nhiệm thật trước khi code.

## 2. Scope sở hữu và target files

Sở hữu adapter mỏng `harness-web`, thư mục frontend được chọn trong SPEC P8, Web integration tests, đóng gói static assets và tài liệu vận hành. Chỉ mở rộng application-service interfaces khi đã có thao tác CLI tương đương hoặc read API đã được review. SQL storage, context composition, task ownership, approvals và memory policy vẫn thuộc owner hiện có.

Outputs: crate/module Web adapter, frontend source cùng dependencies đã khóa, `crates/harness-cli/tests/phase_p8.rs`, browser E2E tests và W01–W06 trong acceptance registry. Chọn frontend stack tối thiểu một cách rõ ràng; runbook không ép framework.

## 3. Contracts cần chốt trước implementation

Chốt route/version conventions, request IDs, ánh xạ application-service commands, event cursor, view models đã lọc, error codes và owner frontend build. Phân loại endpoint là read, input admission hay side effect.

Loopback không thay thế authentication. Chốt bootstrap/session flow cho người dùng với credentials entropy cao, expiry, lưu local an toàn, không đưa token vào URL/history/logs; phải có origin/host allowlists, CSRF protection cho cookie-authenticated mutation và authorization từng object/artifact. Không wildcard credentialed CORS hay mặc định bind public. Approval giữ đủ bindings P3: actor, invocation, arguments, workspace, policy. Chọn và pin HTTP/frontend dependencies thật theo tài liệu chính thức.

## 4. Work items theo thứ tự

### 4.1. P8-S01 — Chốt ánh xạ ứng dụng và bảo mật

Phụ thuộc: P7 đã nghiệm thu.

Inspect CLI handlers và liệt kê commands/queries gốc cho sessions, chat, tasks, tool approvals, context, memory. Xác định authenticated principal của browser tại host boundary. Chốt request/response limits, ma trận browser/platform hỗ trợ và UX bootstrap local. Ghi khoảng trống service trong SPEC trước khi sửa contract.

Bằng chứng: Ma trận route → service, threat cases và danh sách màn hình với non-goals rõ; thiết kế không có authority thứ hai.

### 4.2. P8-S02 — Dựng foreground HTTP host

Phụ thuộc: P8-S01.

Lấy host writer lock hiện có trước khi nhận commands. Dùng cùng composition root và shutdown sequence với CLI. Chuyển HTTP request thành service call; kiểm tra schema, IDs, grants, origin, CSRF trước admission. Giới hạn request concurrency, trả structured errors. Không đưa credentials vào telemetry browser nhìn thấy hoặc URL.

Bằng chứng: Tests host thật chứng minh request trái quyền/không hợp lệ không tới input hay tool admission; CLI writer cạnh tranh bị từ chối mà không sửa state.

### 4.3. P8-S03 — Stream events bền vững theo scope, có catch-up

Phụ thuộc: P8-S02.

Cung cấp SSE hoặc transport streaming được chọn rõ trên durable event cursors. Authorize rồi project từng event thành safe view, không dump raw journal. Reconnect từ cursor cuối, deduplicate logical events; retention gap phải báo resnapshot rõ. Giới hạn buffer client chậm, kiểm tra lại quyền khi revoke. Disconnect hủy subscription, không hủy task.

Bằng chứng: Tests disconnect/reconnect lấy lại visible events có thứ tự, báo expired cursor, không phát sinh provider dispatch hay input trùng.

### 4.4. P8-S04 — Làm màn hình session và approval cốt lõi

Phụ thuộc: P8-S03.

Thêm chọn session/task, nhập message, progress/tool timeline và approval dialogs có bindings. Giữ command ID do client tạo qua các lần retry. Hiển thị persisted receipts, uncertainty, blocked states, stale approval errors rõ. Coi model/tool/memory output là dữ liệu không tin cậy: safe Markdown, không chạy raw script, allowlist link schemes.

Bằng chứng: Browser tests bao phủ retry input đã nhận, approval bị deny/hết hiệu lực, tool output chứa script và task phục hồi còn pending work.

### 4.5. P8-S05 — Thêm màn hình task, memory và context

Phụ thuộc: P8-S04.

Hiển thị task tree, child handoffs, effective scopes, source/version memory, trạng thái extraction jobs và inspection exact sanitized context packet. Gọi read/invalidate APIs cùng permission checks hiện có, không thêm trình sửa bảng trực tiếp. Giải thích evidence thiếu/bị forget và retrieval degraded. Mọi thao tác phá hủy hoặc đổi quyền dùng confirmation cùng service contract đã có.

Bằng chứng: Assertions UI/API khớp CLI về task IDs, receipts, memory scope và pending jobs; IDs/hash khác scope không lộ nội dung.

### 4.6. P8-S06 — Chạy Web acceptance và regressions CLI

Phụ thuộc: P8-S04 và P8-S05.

Triển khai W01–W06 trong bảng nghiệm thu bằng host services local thật và model boundary deterministic. Test transport retry, đóng browser, host ownership, authentication/CSRF, content rendering, stream recovery. Chạy toàn bộ C/K regressions trên cùng runtime; bổ sung adapter regression khi Web làm thay đổi shared service.

Bằng chứng: Test discovery browser/adapter khác rỗng, kết quả các platforms bắt buộc, negative controls và mọi gate cũ; phân biệt browser tests với HTTP-only tests.

### 4.7. P8-S07 — Đóng gói UI local và bàn giao vận hành

Phụ thuộc: P8-S06.

Build và serve static assets khớp version mà không phụ thuộc development server. Smoke-test packaged foreground host và browser flow từ directory fixture sạch thuộc task. Hướng dẫn bootstrap an toàn, local binding, đóng browser khác dừng host, writer-lock conflict, quay về CLI. Remote deployment và package publishing ngoài assignment trừ khi được yêu cầu rõ.

Bằng chứng: Evidence/handoff hai ngôn ngữ, startup instructions local, checksums artifact gắn source đã test; không nhận đã deploy bên ngoài.

## 5. Tests và lệnh kiểm chứng

Sở hữu các ca Web bổ sung **W01–W06** trong [bảng nghiệm thu](ACCEPTANCE_MAP.vi.md), không thay thế 44 ca continuity/plugin hiện có. Cần browser-level checks cho rendering và reconnect UX, adapter-level denial tests, writer-lock tests thật cùng full regressions P7. Chỉ chụp màn hình UI không đủ làm evidence.

P8 mở rộng `Verify-Phase` để chạy frontend formatting/type/build checks và browser test command đã chọn với dependencies khóa. Ghi exact commands và browser versions hỗ trợ trong SPEC phase; không âm thầm skip khi thiếu browser runner.

Lệnh phase tương lai, chạy sau khi đã triển khai các targets:

```powershell
cargo test -p harness-cli --test phase_p8 --locked
pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P8
```

Full gate gồm formatting, clippy, workspace tests, kiểm tra test discovery và docs checks trong sổ tay. Không báo acceptance pass khi filter chỉ chạy 0 tests.

## 6. Kịch bản demo

Chạy `ha web` trên loopback port với fixture data directory tạm. Authenticate bằng flow local đã chốt, mở task đã tạo bằng CLI, gửi một command và reconnect khi tool đang chạy. Kiểm tra cùng command ID chỉ có một logical admission, pending work còn nguyên, receipts khớp CLI read view. Đóng rồi mở browser khi foreground host vẫn chạy; sau đó dừng host bình thường và resume bằng CLI. Thử writer thứ hai và artifact read trái quyền; cả hai phải bị từ chối.

## 7. Exit gate và các cách làm không được phép

W01–W06 và gates tiền nhiệm pass trên ma trận platform/browser đã công bố. Browser reconnect chỉ là view subscription; app services giữ execution và authority. Static assets/backend có compatibility identity tái lập được. Authentication, CSRF và output rendering được test, không hoãn vì server chỉ chạy local.

Không được: tạo loop cho mỗi tab, routes ghi SQLite trực tiếp, client quyết định approval/state, stream raw private events, mặc định bind public, hoặc biến phase thành multi-user cloud service.

Không chuyển phase chỉ dựa trên summary. Gắn kết quả với revision cuối đã test, báo checks chưa chạy, giữ mọi regressions tiền nhiệm. Không sửa fixture expectations chỉ để implementation pass.

## 8. Giao việc và handoff

Nếu người dùng giao parallel work rõ ràng, backend adapter owner và frontend owner có thể làm song song sau khi P8-S01 chốt API/security contracts. Một integrator giữ dependency locks, browser fixtures và shared-service changes. Không chia authentication thành các mảnh thiếu một người review toàn bộ flow.

Bàn giao `docs/evidence/P8.en.md`, `P8.vi.md`, cùng handoff tiếp tục được trong `docs/handoffs/` theo sổ tay. Ghi step IDs đã xong, lỗi còn lại, schema changes, commands và next action. Publish cần được cấp quyền rõ trong assignment coding.

## 9. Prompt giao agent

```text
Chỉ triển khai P8. Đọc docs/implementation/README.vi.md và
P8_WEB.vi.md trong cùng thư mục, các hợp đồng kiến trúc được link,
và quy định repo áp dụng. Xác minh gate tiền nhiệm bằng source/evidence.
Tạo SPEC phase rồi thực hiện P8-S01..P8-S07 theo dependencies.
Giữ đúng scope phase, bảo toàn thay đổi không liên quan và contracts đã chấp nhận.
Acceptance tests dùng component thật; chỉ mock boundaries bên ngoài phù hợp.
Chạy phase gate và regressions tiền nhiệm; bàn giao evidence hai ngôn ngữ cùng restart handoff.
Dừng trước phase tiếp. Không spawn agents, commit, push hoặc publish nếu chưa được giao rõ.
Nếu thiếu prerequisite hoặc verification bắt buộc, báo đúng khoảng trống, không nhận đã hoàn tất.
```
