# ADR-N02 — Store ownership, durability và artifact publish

**Trạng thái:** accepted trong M1 (21/09/2026).
**Phạm vi:** data directory ownership, generation fence, durability pragmas, data directory marker, artifact publish/GC. Không thay ADR-N01.

## 1. Bối cảnh

P1 đã có store `SQLite` với writer lock, host epoch và transaction cho input/receipt/snapshot. M1 phải chốt bằng văn bản những gì đang là *quyết định* (để M3/M4/M9 không vô tình nới lỏng), và bổ sung hai chỗ còn thiếu bằng chứng: marker của data directory và quy tắc "artifact chỉ sống theo reachability".

## 2. Quyết định

1. **Lock scope.** Quyền ghi thuộc về một *data directory*, bảo vệ bằng `writer.lock` (`fs2` exclusive lock trên file OS). Trong process, writer pool có đúng một connection để mọi write đi qua một coordinator. `Mutex` trong process **không** được coi là bằng chứng exclusive host.
2. **Generation fence.** Mỗi lần mở writer, `host_epoch.generation` được tăng trong transaction và `hosts(host_id, generation)` được ghi. Mọi transaction ghi kiểm tra fence hiện tại (`begin_write` + `assert_fence_in_tx`); fence cũ → `StaleWriter`. Fence là *durable*: nó sống trong database, không trong RAM.
3. **Durability.** `foreign_keys=ON`, `busy_timeout=5s`, `journal_mode=WAL`, `synchronous=FULL` cho writer; read-only mở không tạo file, không migrate. ACK chỉ được trả **sau** `COMMIT`.
4. **Data directory marker.** `harness-data.json` (`kind: harness-data`, `schema_version`, `store_schema_version`) được tạo ở lần mở writer đầu tiên, sau khi migration thành công; được validate trước mọi migration ở các lần sau. Marker mới hơn host, sai `kind`, hoặc không parse được → từ chối ghi và **không ghi đè**. Đọc read-only không cần marker (database P1 cũ vẫn mở được).
5. **Schema mới hơn.** Mỗi nhóm bảng có bảng migration riêng; `MAX(version) > version hiện tại` → từ chối ghi (`MigrationFailed`) thay vì migrate mò.
6. **Artifact publish.** Thứ tự bắt buộc: ghi bytes → flush → hash → **rồi** commit reference trong transaction. File không có reference chỉ bị thu hồi bởi reachability sweep (`referenced_artifact_ids` + retention pin); artifact đang được receipt tham chiếu trả `RetentionRefused` khi bị yêu cầu xoá. Không suy "log đã capture" thành "file được phép xoá".
7. **Outbox.** Delivery bền dùng `parent_deliveries` (P5) làm outbox logic (`DeliveryState`, dedupe theo `message_id`); M1 **không** tạo bảng `outbox` thứ hai. Nếu M11 cần transport outbox riêng, nó tham chiếu cùng delivery id chứ không tạo authority mới.
8. **Reopen ≠ rerun.** Mở lại database, `ha status`/`ha resume`/offline replay chỉ đọc; reconciliation side effect thuộc M3/M4.

## 3. Hệ quả

- M3 thêm bảng runs/steps/attempts bằng migration riêng, không đổi `STORE_SCHEMA_VERSION`.
- M4 thêm approvals/intents/receipts (đã có từ P3). StorePort được hoãn ở M1 vì contract khi đó thiếu transaction inputs; audit M0–M6 sau này chuẩn hóa DTO đầy đủ rồi implement adapter production `SqliteStore` cho admission, run lease/freeze, intent/settlement, task update, receipt và child delivery. Adapter gọi atomic operations hiện hữu, kiểm generation/revision và không tạo success placeholder.
- M9 backup/restore phải copy cả `harness-data.json`; restore vào directory có marker mới hơn phải từ chối.
- Mọi thay đổi pragma durability phải cập nhật `diagnostics()` và test P1 tương ứng.
