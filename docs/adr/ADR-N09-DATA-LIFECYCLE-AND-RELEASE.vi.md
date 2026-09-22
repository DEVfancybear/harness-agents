# ADR-N09 — Data lifecycle, redacted diagnostics và release candidate

**Trạng thái:** accepted trong M9 · **Ngày:** 23/09/2026 · **Phạm vi:** M9-01..M9-04

[Runbook M9](../implementation-next/M9.vi.md) · [Contracts](../implementation-next/CONTRACTS.vi.md) §9 · [SPEC M9](../specs/M9.vi.md) · [ADR-N02](ADR-N02-STORE-OWNERSHIP-DURABILITY.vi.md)

## 1. Bối cảnh

P7 đã có backup/restore/retention/migration và release matrix. Ba câu hỏi M9 phải trả lời mà P7 chưa:

1. **Support bundle là artefact duy nhất được thiết kế để rời khỏi máy.** Chưa có đường nào tạo nó, nên chưa có
   định nghĩa nào cho "cái gì được mang đi".
2. **Backup hỏng phải bị từ chối *trước* khi ghi vào đích.** `verify_backup` chạy trước khi copy, nhưng chưa có
   test nào chứng minh điều đó trên một backup thật bị sửa một byte.
3. **Migration ngắt giữa đường phải để nguồn nguyên vẹn.** `migrate_copy` chạy trên bản copy, nhưng chưa có oracle.

## 2. Quyết định

### D1 — Support bundle được dựng từ một danh sách field đóng

Bundle mang: platform/build identity, sqlite pragmas, schema revisions, counts, retention summary, **tên** biến môi
trường, config (đã redact), correlation refs, và danh sách những gì nó loại trừ. Không mang: giá trị môi trường,
transcript, payload event, byte artifact, credential.

**Lý do:** "cái gì không được mang đi" phải là một quyết định, không phải hệ quả của việc quên. Một danh sách đóng
thì review được; một `serde_json::to_value(host_state)` thì không.

### D2 — Redaction hai chiều

`redact_field(name, value)` từ chối khi **tên** trông như secret (`is_secret_name`) **hoặc** khi **giá trị** trông
như credential (`looks_like_a_secret`).

**Lý do:** quy tắc thứ nhất chỉ bắt secret được lưu dưới tên trung thực. Ca đáng lo là secret bị dán vào một field
vô hại — `objective`, `note`, `summary` — và chỉ quy tắc thứ hai bắt được nó. Test A32 khẳng định cả hai quy tắc
**như quy tắc**, rồi khẳng định lại trên **byte thật** của từng file trong bundle, nên rò rỉ ở bất kỳ tầng nesting
nào cũng bị bắt.

### D3 — Môi trường chỉ mang **tên**

`ha maintenance support-bundle` truyền `std::env::vars()` cho bộ redaction, nhưng bundle chỉ ghi tên biến. "Key có
được set hay không" là câu hỏi chẩn đoán thật; giá trị thì không phải của host để công bố.

### D4 — Bundle nói ra những gì nó đã giữ lại

Manifest ghi `redacted_field_count`, output CLI ghi `redacted_fields`. Một bundle im lặng về redaction buộc người
đọc phải **tin**; một bundle đếm nó thì người đọc **kiểm tra** được.

### D5 — Bundle phải tái lập được

Bundle mang `REPRODUCE.md`: các lệnh đã tạo ra số liệu, kèm data directory. Một bundle không có chúng là một ảnh
chụp màn hình: người đọc thấy số và không kiểm tra được.

### D6 — Chẩn đoán không phải journal

`build_support_bundle` mở store **read-only**; một lần chẩn đoán thất bại (data dir không tồn tại, exporter chết)
không thể biến một domain write đã settle thành failed. A32 khẳng định điều này theo hướng ngược lại: sau khi
diagnostics fail, artifact đã publish vẫn còn trong store và retention summary vẫn trả lời.

### D7 — Restore từ chối trước khi ghi

`restore_backup` chạy `verify_backup` (hash database + hash từng artifact) **trước** khi copy byte nào. Đích đã có
store hoặc có `.active` bị từ chối. A31 khẳng định: backup bị sửa một byte ⇒ `BackupManifestInvalid`, và thư mục
đích **rỗng**; backup nguồn vẫn verify được sau lần từ chối đó.

### D8 — Migration chạy trên bản copy và không bao giờ chạm nguồn

`migrate_copy` đích không dùng được ⇒ lỗi, và nguồn vẫn mở được, vẫn còn input và artifact bytes. Đích tốt ⇒
`MigrationOutcome.migrated` và `source` được nêu tên.

### D9 — M9 hoàn tất kỹ thuật **không** tự cho quyền publish release

`ReleaseMatrix::validate` từ chối matrix che một platform đỏ hoặc chưa đo (`not_green`, `unmeasured_benchmarks`,
`met()` trả `None` khi `measured` là `None`). Nếu chưa được cấp quyền publish thì M9 bàn giao **local release
candidate** (artifact + checksum + inventory + sample config không secret), không upload.

## 3. Hệ quả

| Hệ quả | Xử lý |
|---|---|
| `ha maintenance support-bundle` là subcommand mới | Không đổi output của subcommand cũ; `Doctor`/`ReleaseMatrix` giữ nguyên |
| `diagnostics.rs` là module mới trong `harness-maintenance` | Crate này đã là owner của release/retention; không thêm crate |
| Một data directory có **một** writable host | Fixture A31 phải nhả handle trước khi mở handle kế tiếp; đây là hành vi đúng và đã được ghi lại trong test |
| `MAINTENANCE_CONTRACT_VERSION` giữ 1 | Bundle có `schema_version` riêng; `BackupManifest`/`ReleaseMatrix` không đổi field |

## 4. Phương án bị bác bỏ

- **Serialize toàn bộ host state rồi redact.** Redaction là bước cuối; bất kỳ field nào quên đi qua nó đều bị mang đi.
- **Chỉ redact theo tên.** Bỏ sót đúng ca đáng lo nhất (secret dán vào field vô hại).
- **Mang giá trị môi trường "chỉ để chẩn đoán".** Đó là export credential với nhãn diagnostics.
- **Cho restore tự activate.** Activate là hành động riêng, có người quyết.
- **Coi M9 xong là được publish.** Quyền publish không suy ra từ việc milestone xanh.
