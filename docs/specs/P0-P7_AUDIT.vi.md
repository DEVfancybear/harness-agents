# SPEC rà soát P0–P7 — hồi quy, an toàn và nợ kỹ thuật

Ngày: 23/09/2026. Assignment này rà implementation P0–P7 tại base
`5498f74071aae04998e51afdfa4eeb4281f5e870`, đối chiếu implementation runbooks,
SPEC, handoffs, evidence và ADR. User yêu cầu sửa lỗi/nợ còn lại và rà code kỹ;
SPEC riêng chưa được duyệt (autonomous run). Linux giữ pending theo chỉ dẫn.

## Phạm vi và bảo toàn

- Rà code đã commit P0–P7 và các findings còn mở đã ghi trong
  `docs/evidence/P0-P3_REVIEW.vi.md`; rà thêm các lỗi/debt có bằng chứng trong
  P4–P7 artifacts và source.
- Checkout chính có thay đổi G01–G03 chưa commit. Chỉ làm trong worktree audit
  sạch; không chạm, stage, commit hay push các thay đổi đó.
- Không gọi provider trả phí, không cài đặt/đổi PATH, không xóa/di chuyển dữ liệu
  người dùng, không phát hành artifact, không thêm confinement backend hay thay
  nghĩa `strict` nếu cần quyết định kiến trúc mới.
- Giữ các schema/ID/API contract đã chấp nhận. Nếu code fix bắt buộc đổi contract,
  phải có test, migration additive, SPEC/ADR giải thích trước khi áp dụng.

## Acceptance

1. Mọi finding được sửa phải có hồi quy hành vi đo RED trên source trước fix và
   GREEN sau fix; không xóa, skip hoặc nới assertion hiện có.
2. Approval một lần chỉ được dùng cho đúng lời gọi/phiên/task và chính xác action,
   arguments, workspace cùng policy revision đã được duyệt; proposal khác hoặc
   replay phải bị từ chối.
3. Compaction chỉ commit snapshot nếu source sequence/revision vẫn đúng; tail đổi
   trong lúc dựng candidate phải bị phát hiện và rebase/retry có giới hạn hoặc
   trả lỗi typed mà không làm mất instruction/event.
4. Process đã bị hủy trước khi spawn không được khởi chạy sau khi chờ lifecycle
   mutex; hủy đồng thời với spawn phải có điểm tuyến tính hóa rõ, process tree
   cleanup vẫn bounded.
5. Soát các P0–P7 contract bảo mật/dữ liệu chính: fail-closed authorization,
   single-writer/fencing, durable commit/recovery, context/source freshness,
   worktree/user-change preservation, extension trust/limits, backup/restore/
   forget/tombstone/GC. Không ghi nhận lỗi là đã sửa nếu không có reproduction và
   evidence tương ứng.
6. Cập nhật evidence/handoff để phân biệt lỗi đã sửa, kiểm tra thật đã chạy, các
   giới hạn còn mở và Linux pending. Không đổi registry sang `accepted` hoặc
   `verified_local`.
7. Sau sửa chạy regression test mục tiêu, `Verify-Phase.ps1 -Phase P7 -Json` trên
   Windows và `Verify-Docs.ps1 -SelfTest`; báo chính xác mọi gate không chạy/đỏ.

## RED/GREEN và delivery

Chạy baseline trên worktree sạch trước sửa. Thêm/chỉnh test trước implementation,
ghi lại exact failing assertion, rồi sửa theo từng finding và chạy lại test đó.
Không giả định failure thuộc source khi lỗi do môi trường. Kết quả cuối gắn với
commit/digest đã kiểm chứng. Assignment tương tự tiếp tục yêu cầu commit/push mã
đã rà; chỉ push nhánh audit này, không đưa G01–G03 vào commit.

## Addendum 1 — restore target phải là path mới

Trong lúc rà P7 backup/restore, source cho thấy `restore_backup` chấp nhận một
thư mục đích đã tồn tại nhưng rỗng. Điều này trái với doc/API contract “new
directory”, để lộ restore qua symlink/reparse-point do caller tạo sẵn, và lỗi
giữa chừng có thể để lại một store nửa chừng khiến lần chạy sau bị từ chối.

Acceptance bổ sung: nếu path đích đã tồn tại, kể cả thư mục rỗng, restore trả
`RestoreTargetConflict` và không ghi/đổi bất kỳ nội dung nào ở path đó. Chỉ path
mới được restore; lỗi khi dựng bản restore không để lại output cuối cùng. Không
thêm dependency. Spec approval vẫn chưa có; đây là autonomous run theo yêu cầu
audit/sửa hiện tại.

## Addendum 2 — trạng thái retention và journal cùng commit

Rà luồng invalidate/archive cho thấy cập nhật asset đã commit trước lần ghi
journal. Nếu journal bị lỗi, lệnh trả failure nhưng trạng thái đã đổi và audit
entry mất. Acceptance bổ sung: lỗi ghi journal phải rollback cả status/FTS/
invalidation record; khi thành công thì trạng thái và journal cùng tồn tại sau
một commit. Không thêm dependency hay thay public data contract.

## Addendum 3 — GC phải khôi phục bytes khi commit metadata thất bại

Rà P7 GC cho thấy file bytes bị unlink trước DELETE row; nếu trigger/SQLite từ
chối DELETE, transaction rollback giữ lại row nhưng không thể phục hồi file.
Acceptance bổ sung: mọi lỗi DB sau khi bắt đầu thu hồi phải giữ nguyên artifact
ở cả filesystem và index; đường thành công mới được dọn cả hai. Không thêm
dependency.

## Addendum 4 — tombstone phải chặn write path thật

Source cho thấy `assert_not_tombstoned` chỉ là helper rời và không có caller
trong pipeline ghi memory. Acceptance bổ sung: mọi đường ghi version có source
file/commit kiểm tombstone ngay trong write transaction; sau khi forget đã
commit, create/propose/extraction không thể tái tạo nội dung đó, kể cả khi hai
writer chạy cạnh tranh. Không thêm dependency hay đổi public API.

## Addendum 5 — manifest phải khớp database snapshot

P7 contract yêu cầu restore kiểm schema/artifact/tombstone set, nhưng
`verify_backup` mới so hash file với manifest và artifact bytes; manifest không
có signature, nên người ghi được manifest có thể sửa metadata rồi tính lại
digest. Acceptance: verify chạy SQLite integrity check và đối chiếu schema
revisions, toàn bộ artifact rows/lengths, retention pins, tombstone identities
và byte lengths với database snapshot trước khi restore. Mismatch phải fail
closed. Không thêm dependency hay đổi manifest schema.

## Addendum 6 — không xem mtime không đọc được là artifact đã cũ

GC đang đổi lỗi `metadata/modified()` thành Unix time 0; nếu thời gian file
không đọc được, artifact bị coi là cũ hàng chục năm và có thể bị thu hồi ngay.
Acceptance: mtime thiếu/lỗi được xem là tuổi 0 (giữ lại trong grace period),
không được dùng làm bằng chứng artifact đủ tuổi. Không thêm dependency.

## Addendum 7 — tombstone là record bất biến, không upsert âm thầm

Forget lần hai cùng source hiện thay reason/copies nhưng không thay
`tombstone_id`; CLI trả ID mới trong khi store giữ ID cũ. Acceptance: target đã
tombstoned thì forget lại trả `RetentionRefused` không đổi reason, copies, ID
hoặc timestamp. Tombstone write trong transaction là plain insert sau check,
không upsert. Không thêm dependency/API.

## Addendum 8 — retention cần lý do không rỗng

Archive/forget nhận lý do rỗng dù journal/tombstone phải giải thích hành động.
Acceptance: mọi action từ chối reason trắng trước write; status, journal,
tombstone không đổi. Không thêm dependency/API.

## Addendum 9 — confirmation phải khớp đủ identity của target

Lệnh forget báo target là `source_kind:source_id`, nhưng chỉ so confirmation với
`source_id`. Vì vậy token của một target chưa đầy đủ và có thể bị tái sử dụng
giữa hai loại nguồn có cùng ID.

Acceptance: confirmation chính xác là `source_kind:source_id`; token chỉ có ID,
hoặc kind/ID khác, phải bị từ chối trước mọi thay đổi. Không đổi schema; cập nhật
CLI help, operator guide và journal target để hiển thị identity đầy đủ.

## Addendum 10 — backup chỉ publish một snapshot hoàn chỉnh vào path mới

`create_backup` chỉ từ chối thư mục có sẵn nếu nó đã chứa tên database/manifest
nhận diện được; một thư mục người dùng có sẵn có thể bị ghi thêm hoặc ghi đè.
Nếu backup lỗi sau `VACUUM INTO`, database dở còn lại chặn lần retry.

Acceptance: từ chối mọi destination đã tồn tại và giữ nguyên nó; dựng snapshot,
artifact cùng manifest trong staging bên cạnh destination; chỉ rename staging ra
đích khi mọi kiểm tra thành công. Lỗi trước publish không để lại đích/staging.

## Addendum 11 — chỉ đọc file nằm trong root backup/store

Manifest kiểm soát đường dẫn tương đối theo cú pháp nhưng symlink/reparse point
ở thư mục trung gian vẫn có thể trỏ ra ngoài root; source artifact trong store
cũng có thể bị chuyển hướng như vậy.

Acceptance: canonicalize database, manifest và artifact file trước khi đọc/chép;
mọi target phải là file thường nằm bên trong root được chọn. Không follow symlink
ra ngoài backup/store. Thất bại phải từ chối backup/verify/restore.

## Addendum 12 — GC chỉ được thu hồi path artifact do store tạo

GC đọc `relative_path` từ database rồi ghép trực tiếp vào data root. Database hỏng
hoặc bị sửa có thể đưa path ra ngoài; symlink ở file artifact cũng có thể khiến GC
di chuyển/xóa link trong khi logic báo đã thu hồi bytes.

Acceptance: kiểm path đúng dạng identity ổn định `artifacts/<id>.bin`, parent
canonical vẫn trong data root, và path cuối là file thường trước khi quarantine.
Path không hợp lệ hoặc là thư mục/symlink phải giữ nguyên row/path và fail typed.

## Addendum 13 — journal và API tombstone không được ghi đè entry cũ

Các API store thấp hơn vẫn dùng `INSERT OR REPLACE` cho journal và
`ON CONFLICT ... DO UPDATE` cho tombstone. Caller lặp ID có thể âm thầm sửa lịch
sử, còn `record_tombstone` có thể đổi reason/copies dù đường retention mới đã từ
chối forget lặp.

Acceptance: journal là append-only theo `entry_id`; tombstone là immutable theo
`source_kind:source_id`. Trùng entry hoặc target trả lỗi typed và rollback cả
transaction, không sửa bytes của record gốc.

## Addendum 14 — không tạo pin cho artifact không tồn tại

`pin_artifacts` ghi pin mà không xác nhận artifact row còn tồn tại. Sau khi GC
commit xóa artifact, một caller mới có thể được báo pin thành công cho ID không
còn bytes/record.

Acceptance: kiểm mọi artifact ID trong writer transaction trước khi ghi pin; nếu
thiếu bất kỳ ID nào thì trả `RetentionRefused` và không thêm pin nào trong batch.
Điều này tuần tự hóa race pin/GC theo cùng writer database.

## Addendum 15 — recheck grace period tại điểm thu gom

GC đo tuổi file khi lập candidate list, rồi có thể thu gom sau một khoảng chờ.
Nếu artifact mới thay vào cùng path sau snapshot, tuổi cũ không chứng minh bytes
hiện tại đã qua grace period.

Acceptance: kiểm lại mtime/age và regular-file metadata trong writer transaction
ngay trước quarantine; tuổi không xác định hoặc còn trẻ thì rollback, giữ artifact
row/bytes và báo `retained_young`.

## Addendum 16 — freshness dùng lineage hiện tại, forget xóa cả lịch sử

Audit phát hiện invalidation và retention đi theo mọi row `memory_sources`/
`memory_dependencies`. Sau correction, source/dependency của version đã supersede
có thể làm asset hiện tại bị invalidate/archive sai. Ngược lại, lọc current-only
cho forget sẽ để lại version cũ chứa dữ liệu đã yêu cầu xóa.

Acceptance: source invalidation và archive/invalidate retention chỉ chọn root và
descendants có `derived_version = memory_assets.current_version`. Forget phải tìm
mọi root/descendant có bất kỳ version lịch sử nào phụ thuộc source rồi xóa toàn bộ
asset versions/FTS/lineage trong cùng transaction với tombstone và journal. Thêm
regression cho một asset/summary có current version đã chuyển lineage, bảo đảm
invalidation bỏ qua lineage cũ nhưng forget vẫn xóa phần history nhạy cảm. Không
đổi API/schema.

## Addendum 17 — ACK của egress canary phải được quan sát hai chiều

ACK canary M12 được gửi sau khi listener nhận nonce đúng, nhưng listener đóng
socket ngay sau `write_all`. Trên Windows, byte ACK có thể còn trong buffer khi
listener đóng và child báo `connected=false`, dù request đã được nhận.

Acceptance: child chỉ xác nhận success sau khi đọc ACK; listener giữ socket tới
khi peer đóng và cả hai phía có timeout hữu hạn. Timeout/error phải giữ trạng
thái fail-closed, còn listener diagnostics phải nêu rõ liệu nonce có được nhận.
