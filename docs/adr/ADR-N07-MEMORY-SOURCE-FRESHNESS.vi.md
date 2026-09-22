# ADR-N07 — Memory assets: source freshness, version CAS và publication authority

**Trạng thái:** accepted trong M7 · **Ngày:** 23/09/2026 · **Phạm vi:** M7-01..M7-04

[Runbook M7](../implementation-next/M7.vi.md) · [Contracts](../implementation-next/CONTRACTS.vi.md) · [SPEC M7](../specs/M7.vi.md) · [ADR-N02](ADR-N02-STORE-OWNERSHIP-DURABILITY.vi.md)

## 1. Bối cảnh

`harness-memory` đã có asset/version/scope/CAS/job (P4, M5). Ba khoảng trống chặn M7:

1. **Source dependency không nằm trong store.** `memory_versions.version_json.source_file_hashes` là một mảng hash
   không có `(kind, id)`; `memory_dependencies` (đã tồn tại) chỉ ghi quan hệ asset → asset với `source_kind='asset'`.
   Không có bảng nào trả lời được "file X đổi thì version nào là stale", nên `invalidate_memory_source` chỉ chạy
   được cho `source_kind='file'|'commit'` mà **không có producer nào ghi hai kind đó**.
2. **Extraction chỉ chạy khi có người gọi.** `MemoryService::schedule_backlog` là lệnh chủ động; không có đường nào
   nối "journal vừa commit" với "range này đã có job". Một tiến trình chết giữa lúc commit và lúc enqueue sẽ mất
   range đó cho tới khi con người chạy lại CLI.
3. **Candidate và publish chung một cửa.** `extract_lease` tự ghi asset `Candidate`; `confirm_version` publish.
   Điều đó đúng, nhưng chưa có **precondition version** ở đường propose/confirm ngoài `write_version`, và chưa có
   đường `reject` tường minh — nên "propose rồi publish" không phải một chuỗi có thể kiểm chứng bằng CLI.

## 2. Quyết định

### D1 — Nguồn của một version là một bảng có khoá, không phải một mảng hash

Thêm `memory_sources(derived_asset_id, derived_version, source_kind, source_id, observed_digest, scope_project_id, created_at)`
với `source_kind ∈ {file, commit, event, asset}`, UNIQUE theo `(derived_asset_id, derived_version, source_kind, source_id)`.

- `source_id` cho `file` là đường dẫn tương đối tới workspace root; `observed_digest` là content hash tại thời điểm
  version được ghi, **không** phải hash hiện tại.
- Mọi lần `create_asset`/`write_version`/`extract_lease` ghi version đều ghi kèm `memory_sources` **trong cùng
  transaction** với version. Không có đường ghi version nào bỏ qua bảng này.
- `memory_dependencies` giữ nguyên nghĩa cũ (asset → asset) để không phá dữ liệu P4/M5; `memory_sources` là nguồn
  sự thật cho freshness và invalidation theo nguồn ngoài.

**Lý do:** truy vấn "version nào phụ thuộc file X" phải là một index lookup, không phải `json_each` trên mọi version.
Contracts §3 cấm dùng một JSON blob thay cho constraint field cần query.

### D2 — Freshness là một filter ở read path, không phải một lần kiểm tra lúc ghi

`search_memory` và `bound_memory` loại một version khi **có** `memory_sources` kind `file` với observed digest khác
fingerprint hiện tại của file đó, hoặc khi asset/version đã `invalidated`.

- Freshness được kiểm tra **trước** rank, theo Contracts §2 ("Scope filter trước read/search/export/rank").
- Việc kiểm tra không tự động invalidate: nó chỉ không trả về. Invalidate là hành động riêng, có actor và reason
  (`ha memory invalidate --source file:<path>`), vì một cú `git checkout` không được phép xoá audit.
- Kết quả retrieve mang `selected_version`, `selection_reason` (`fresh|user_corrected|fallback`) và `source_digests`,
  để packet nói được nó đã chọn version nào và vì sao.

### D3 — Correction tạo version mới; supersede là một cạnh, không là ghi đè

`confirm_version`/`write_version` với `supersedes = Some(n)` ghi version `n+1` và giữ nguyên version `n`.
`source_assets` rỗng ở L2 nghĩa là giữ lineage cũ (hành vi đang có). Một bản sửa của user **luôn** là version mới
với `authority=User`, `evidence=UserConfirmed`; không đường nào ghi đè nội dung version cũ.

### D4 — Enqueue là hệ quả của journal đã commit, không phải một sự kiện rời

`MemoryService::reconcile(stream, principal, strategy)` là một hàm **idempotent**:

1. đọc các `source_work_markers` đã commit của stream (nguồn range),
2. với mỗi range chưa có job theo `(stream, start, end, extractor_version, strategy_digest)`, tạo job `Pending`,
3. trả về mọi job chưa `Completed` theo thứ tự range.

Vì cursor chỉ tiến khi settle, một tiến trình chết **trước** enqueue để lại cursor cũ, và lần `reconcile` sau tạo lại
đúng range đó. Job đã settle thì UNIQUE constraint chặn bản thứ hai. Không có timer, list trong process hay
notification nào là durability (Contracts §8).

Thứ tự bắt buộc: **journal commit → marker (đã có) → reconcile → lease → settlement (assets + cursor, một transaction)**.

### D5 — Settlement giữ nguyên tính nguyên tử, disposition là bắt buộc

Một job kết thúc ở đúng một trong: `completed` (có `disposition ∈ {no_facts, candidates, filtered}`), `retry_wait`
(backoff đã có), `blocked` (extractor unavailable), `paused` (budget/cancel), `dead_letter` (quá số lần thử).
Nguồn bị filter phải có disposition để cursor không mắc (Contracts §8). `settle_extraction_assets` đã ghi
assets + cursor + disposition trong một transaction; ADR này chỉ thêm `filtered` và việc ghi `memory_sources`.

### D6 — Injected memory không bao giờ là nguồn extraction

`project_sources` chỉ nhận `input.admitted` (authority `User`) và observation của runtime. Event do host sinh ra để
mang memory đã inject vào packet (`memory.injected`, channel `skill`/`memory`) **không** nằm trong danh sách đó, và
một candidate trích từ nội dung memory phải bị từ chối **trước** khi thành asset. Test A26 chứng minh: một fact được
inject rồi lặp lại trong hội thoại không tạo ra bằng chứng độc lập thứ hai.

## 3. Hệ quả

| Hệ quả | Xử lý |
|---|---|
| Schema memory tăng version | `MEMORY_SCHEMA_VERSION` 1 → 2; bảng mới dùng `CREATE TABLE IF NOT EXISTS` nên store cũ mở được; fixture "store version 1 rồi mở bằng host version 2" chạy trong `milestone_m7` |
| Version cũ không có `memory_sources` | Đọc phải chịu được version không có source row: freshness coi là `fresh` (không có bằng chứng để nói stale) — fail-open ở chiều **an toàn cho dữ liệu cũ**, không fail-open ở chiều quyền |
| `MemoryContribution` mở rộng | Thêm field optional; `CONTEXT_SCHEMA_VERSION` giữ 1 vì field mới là optional và không đổi cách render bắt buộc |
| Invalidation theo file | `invalidate_memory_source` đã có; nay có producer ghi `source_kind='file'`, nên đường đó thật sự chạy và được test |
| Extraction budget | Giữ nguyên `MemoryBudget`; extraction chạy ở mức ưu tiên thấp hơn bằng cách chỉ chạy qua `catch_up`/`reconcile` do host gọi, không chen vào turn |

## 4. Phương án bị bác bỏ

- **Dùng `memory_dependencies` cho cả file/commit.** Bảng đó có `derived_asset_id` là khoá chính đầu và không có
  `scope_project_id`; nhét source ngoài vào sẽ làm `invalidate_memory_source` quét cả cây asset mà không lọc scope.
- **Invalidate tự động khi phát hiện file đổi.** Xoá audit vì một thao tác ngoài ý muốn của user; và A26 yêu cầu
  correction/revocation **tường minh** thắng, không phải một side effect của read.
- **Enqueue bằng một task nền trong process.** Contracts §8 cấm "queue chỉ nằm trong timer/list của process" và
  "notification là durability". `reconcile` idempotent cho kết quả tương đương khi chạy lại sau crash.
- **Đổi `EXTENSION_PROTOCOL_VERSION`/`TOOL_CONTRACT_VERSION`/`CONTEXT_SCHEMA_VERSION`.** Không có thay đổi protocol
  nào ở M7; bump chỉ làm từ chối peer hợp lệ.
