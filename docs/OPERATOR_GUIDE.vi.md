# Hướng dẫn vận hành — chạy và phục hồi harness

Ngôn ngữ: [English](OPERATOR_GUIDE.en.md)

Tài liệu này dành cho người phải chạy, sao lưu, phục hồi, chuyển đổi và thanh lý
một thư mục dữ liệu harness. Nó nêu đúng những lệnh có trong bản phát hành này,
mỗi lệnh từ chối làm gì, và những giới hạn mà người vận hành không được phát hiện
muộn. Đây là tài liệu vận hành đi kèm
[P7_RELEASE.vi.md](implementation/P7_RELEASE.vi.md).

## 1. Bản phát hành này là gì

Harness là runtime agent lập trình chạy nền trước, trên một host duy nhất. Mỗi
thời điểm chỉ một host được ghi vào một thư mục dữ liệu; quyền sở hữu là khóa
file của hệ điều hành cộng với một thế hệ fencing ghi trong cơ sở dữ liệu, không
phải một dịch vụ mạng.

Điều đó nghĩa là:

- **Không có daemon.** Công việc dừng khi tiến trình host thoát. Không có dịch vụ
  nền nào tự chạy tiếp, tự thử lại hay tự thu gom. Mọi thao tác bảo trì dưới đây
  là lệnh chạy nền trước do người vận hành gọi.
- **Không có server để kết nối.** Endpoint MCP từ xa và sandbox cấp hệ điều hành
  được công bố là không hỗ trợ trong ma trận phát hành; cô lập transport không
  phải là sandbox.
- **Không có artifact phát hành nào được công bố.** Bản dựng Windows được phase
  gate chạy; `scripts/New-HaRelease.ps1` build một bản candidate cục bộ kèm
  checksum ([BUILD_AND_RELEASE.md](BUILD_AND_RELEASE.md)), nhưng không có gì được ký
  hay phát hành.
- **Linux đang chờ hỗ trợ.** Job CI của Linux vẫn chạy để theo dõi nhưng không chặn
  push; ma trận phát hành báo Linux là `unverified`.
- **Thông tin xác thực provider không được kiểm chứng.** Các gate không gọi API
  mô hình trả phí, nên không có tuyên bố phát hành nào phụ thuộc vào nó.

Nguồn có thẩm quyền và đọc được bằng máy cho tất cả những điều trên là
`ha maintenance release-matrix --json`. Nếu tài liệu này và kết quả đó khác nhau,
kết quả đúng và tài liệu là lỗi.

## 2. Chẩn đoán một thư mục dữ liệu

```console
ha maintenance doctor --data-dir <DATA_DIR> --json
```

`doctor` mở store ở chế độ ghi, báo cáo các revision schema tìm thấy, thiết lập
`SQLite` đang áp dụng, số lượng session, task được giao, artifact và retention, và
quan trọng nhất là danh sách `not_verified` nêu rõ những gì nó **không** kiểm tra.
Nó không bao giờ báo "khỏe" cho thứ nó chưa soi.

Hai trường cần đọc trước tiên:

- `writable` — binary này có được ghi vào thư mục hay không. Store do binary **mới
  hơn** ghi sẽ báo `false`: schema đi trước bản dựng này, ghi bị từ chối, và việc
  đọc chỉ vẫn hoạt động để có thể chẩn đoán thay vì phá dữ liệu.
- `compatibility` — câu mô tả cùng sự thật đó bằng ngôn ngữ thường.

## 3. Sao lưu

```console
ha maintenance backup --data-dir <DATA_DIR> --into <NEW_BACKUP_DIR> --json
ha maintenance verify-backup --backup <BACKUP_DIR> --json
```

Một bản sao lưu là ảnh chụp trọn vẹn một thư mục dữ liệu vào một thư mục chưa tồn
tại:

- Ảnh chụp cơ sở dữ liệu do chính `SQLite` tạo sau khi gộp write-ahead log, nên là
  một cơ sở dữ liệu độc lập nhất quán chứ không phải một bộ WAL sao chép dở.
- Mọi artifact mà store tham chiếu đều được sao chép và băm. Manifest ghi lại định
  danh, đường dẫn tương đối, hash nội dung và độ dài byte của từng artifact.
- Revision schema của ảnh chụp, các retention pin còn hiệu lực và các tombstone
  đang hoạt động đều được ghi vào manifest.
- Manifest mang một digest trên chính nội dung của nó, nên sửa đổi sau đó là phát
  hiện được.

Một thư mục sao lưu chứa ảnh chụp `harness.sqlite3`, các artifact đã sao chép, và
`backup-manifest.json` — tệp nêu tên mọi artifact, hash và revision mà ảnh chụp chứa.

`verify-backup` đọc lại manifest, kiểm tra digest của nó, kiểm tra hash cơ sở dữ
liệu và kiểm tra hash của từng artifact. Hãy dùng nó trước khi tin một bản sao lưu,
và dùng lại sau khi chuyển bản sao lưu sang phương tiện khác.

Những gì sao lưu từ chối:

- **Không bao giờ ghi đè hay gộp.** Thư mục sao lưu đã tồn tại sẽ bị từ chối, nên
  một bản sao lưu không thể âm thầm trộn hai ảnh chụp. Mỗi lần sao lưu hãy dùng
  một thư mục mới.
- **Không sao lưu store chưa có gì.** Thư mục không có cơ sở dữ liệu bị từ chối
  (`backup_manifest_invalid`) thay vì tạo ra một bản sao lưu rỗng.
- **Không động vào nguồn.** Thư mục nguồn chỉ được đọc, ngoại trừ bước checkpoint
  write-ahead log để ảnh chụp nhất quán.

Thư mục sao lưu là khép kín. Sao chép nó đi nơi khác là đủ; không có catalog hay
chỉ mục nào cần đồng bộ.

## 4. Phục hồi

```console
ha maintenance restore --backup <BACKUP_DIR> --into <NEW_DATA_DIR> --json
```

Phục hồi sẽ kiểm tra bản sao lưu, sao chép ảnh chụp cùng mọi artifact, rồi chứng
minh kết quả trước khi trả về:

- `database_verified` — kiểm tra toàn vẹn của chính `SQLite` đã đạt trên bản phục hồi.
- `artifacts_verified`, `artifacts_missing`, `artifacts_corrupt` — mọi artifact được
  băm lại sau khi ghi.
- `tombstones_restored` — nguồn đã bị quên vẫn bị quên qua một lần phục hồi.

**Phục hồi không tự kích hoạt.** Nó ghi vào một thư mục mới, ghi `restore.json` với
`"activated": false`, rồi dừng. Không có gì trỏ vào bản phục hồi cho tới khi người
vận hành quyết định chuyển sang. Sự tách bạch đó chính là mục đích: một bản phục
hồi hỏng không thể thay thế thư mục đang chạy, vì phục hồi không thay thế bất cứ thứ gì.

Những gì phục hồi từ chối:

- **Đích đã có store.** `restore_target_conflict`. Bao gồm cả việc phục hồi đè lên
  thư mục đang chạy và phục hồi vào chính thư mục sao lưu.
- **Đích đã được đánh dấu active.** Thư mục có `.active` bị từ chối.
- **Ảnh chụp thiếu hoặc hỏng.** Manifest không khớp nội dung của chính nó, cơ sở dữ
  liệu không đạt kiểm tra toàn vẹn, hay artifact có byte không còn khớp đều bị từ
  chối với `backup_manifest_invalid`. Phục hồi dở dang là lỗi, không phải cảnh báo.

Kích hoạt là bước riêng, tường minh, do công cụ của người vận hành thực hiện, không
phải bởi lệnh này. Chỉ sau khi bản phục hồi đã được kiểm tra mới nên mở nó ở chế độ
ghi và đánh dấu active.

## 5. Nâng cấp và hạ cấp

```console
ha maintenance migrate-copy --data-dir <DATA_DIR> --into <NEW_DATA_DIR> --json
```

Chuyển đổi luôn chạy trên một **bản sao**:

- Thư mục nguồn được sao chép sang một đích mới, và bản sao được mở ở chế độ ghi để
  đường chuyển đổi thông thường chạy trên nó.
- Nguồn được giữ nguyên từng byte. Một lần chuyển đổi bị ngắt để lại nguồn đọc được
  và không đổi, còn bản sao dở dang chỉ cần xóa.
- Khóa writer là trạng thái tiến trình, không phải dữ liệu. Nó không bao giờ được
  sao chép; lần mở chuyển đổi tự lấy khóa riêng.

Những gì chuyển đổi từ chối:

- **Đích đã có dữ liệu.** `restore_target_conflict`, nên chuyển đổi không bao giờ
  gộp vào một store đang tồn tại.
- **Nguồn không có store.** `migration_failed`.

Hạ cấp bị từ chối chứ không được thử. Nếu store ghi revision schema mới hơn bản
dựng đang chạy, `doctor` báo `writable: false`, ghi thất bại với lỗi có kiểu, và
việc đọc chỉ vẫn hoạt động. Cách khắc phục là binary mới hơn, không bao giờ là sửa
tay một dòng schema.

## 6. Retention: invalidate, archive và forget

```console
ha maintenance retain --data-dir <DATA_DIR> --action invalidate \
    --source-kind file --source-id src/lib.rs --reason "nội dung đã đổi" --json
ha maintenance retain --data-dir <DATA_DIR> --action archive \
    --source-kind file --source-id src/lib.rs --reason "giữ để kiểm toán" --json
ha maintenance retain --data-dir <DATA_DIR> --action forget \
    --source-kind file --source-id src/lib.rs --reason "yêu cầu của người vận hành" \
    --confirm file:src/lib.rs --surviving-copy backup-2026-01 --json
ha maintenance tombstones --data-dir <DATA_DIR> --json
```

Đây là ba thao tác khác nhau, và khác biệt này quan trọng:

| Hành động | Việc nó làm | Có xóa nội dung? | Cần xác nhận? |
| --- | --- | --- | --- |
| `invalidate` | Đánh dấu tri thức dẫn xuất là không dùng được, giữ lịch sử | Không | Không |
| `archive` | Đưa nội dung ra khỏi dùng đang hoạt động, vẫn phục hồi được | Không | Không |
| `forget` | Xóa nội dung và ghi một tombstone | **Có** | **Có** |

**`invalidate` không bao giờ là `delete`.** Một mục bị invalidate sẽ bị từ chối cho
việc dùng tiếp trong khi bản ghi của nó — cùng sự thật rằng nó từng tồn tại và đã
đổi — vẫn được giữ. Chỉ `forget` mới xóa nội dung.

`forget` yêu cầu `--confirm` bằng đủ target `--source-kind:--source-id` (ví dụ
`file:src/lib.rs`). Chỉ source ID, sai kind/ID hoặc để rỗng đều bị từ chối với
`retention_refused`, và không có gì được ghi. Retention áp dụng cho mọi project
trong store có cùng cặp kind/ID; file source identity hiện là path tương đối với
workspace.

`tombstone` là bản ghi bền vững rằng một nguồn đã bị cố ý quên. Nó được ghi trong
cùng giao dịch với thao tác forget, tồn tại qua sao lưu và phục hồi, và chặn việc
trích xuất lại: mọi lượt về sau định đọc lại nguồn đó đều bị từ chối, nên dữ liệu
đã quên không thể âm thầm quay lại. `ha maintenance tombstones` liệt kê chúng.

**Tombstone không thể vươn ra ngoài thư mục dữ liệu này.** Việc xóa là cục bộ; những
bản sao bạn đã tạo ở nơi khác không bị động tới. Vì vậy `forget` nhận
`--surviving-copy` (lặp lại được) và báo lại: tombstone ghi mọi bản sao bên ngoài
hoặc bản sao lưu có thể còn chứa dữ liệu, để người vận hành có danh sách tường minh
mà xử lý. Truyền danh sách rỗng là hợp lệ, và nghĩa là bạn khẳng định không còn bản
sao nào khác — công cụ sẽ không tự bịa ra, và cũng không giả vờ rằng việc xóa là toàn cầu.

## 7. Thu gom rác

```console
ha maintenance gc --data-dir <DATA_DIR> --grace-seconds 604800 --dry-run --json
ha maintenance gc --data-dir <DATA_DIR> --grace-seconds 604800 --json
```

Thu gom rác xóa byte của artifact, và chỉ xóa artifact đồng thời:

1. **không được tham chiếu** — không receipt, tool artifact scope hay memory version nào trỏ tới;
2. **không bị pin** — không bản sao lưu hay task dở dang nào giữ retention pin trên nó; và
3. **cũ hơn thời gian ân hạn** — mặc định là 604800 giây (7 ngày).

Báo cáo nêu đúng lý do từng artifact sống sót: `retained_pinned`,
`retained_referenced` hay `retained_young`. Hãy chạy `--dry-run` trước; nó phân tích
y hệt và không xóa gì.

Pin tồn tại để bịt một cuộc đua cụ thể: bản sao lưu hứa giữ artifact trong khi lượt
thu gom muốn xóa. Candidate list chỉ là snapshot; trước khi quarantine, store kiểm
tra lại pin, reference và mtime trong writer transaction. Nếu mtime không đọc được,
artifact được giữ trong grace period. `pin_artifacts` chỉ chấp nhận ID có artifact
row hiện hữu, và cả batch bị từ chối nếu thiếu dù chỉ một ID. Bản sao lưu ghi lại
pin của nó trong manifest để có thể kiểm toán về sau.

## 8. Ma trận phát hành

```console
ha maintenance release-matrix --json
ha maintenance release-matrix --json --retrieval-p95-ms 900 --restore-ms 1200
```

Ma trận báo cáo bốn thứ riêng biệt và từ chối làm mờ chúng:

- **platforms** — từng target triple được hỗ trợ với `verified`, `unverified` hay
  `failing`, kèm bằng chứng cho trạng thái đó. Nền tảng chưa từng được chạy là
  `unverified`, không bao giờ bị bỏ qua.
- **capabilities** — `supported`, `component_only` (đã hiện thực nhưng chưa chạy
  đầu-cuối) hay `unsupported`. Bề mặt không hỗ trợ được nêu kèm lý do.
- **benchmarks** — một `target` đã nêu với giá trị `measured` là `null` cho tới khi
  có lượt chạy thật. `met` là `null` khi `measured` là `null`. Mục tiêu là mục tiêu;
  nó không bao giờ được báo là đạt chỉ vì đã được viết ra.
- **verified_cases**, **unverified_checks**, **out_of_scope** — 44 ca liên tục và
  plugin mà bản phát hành này chạy, những kiểm tra chưa chạy cùng lý do, và những gì
  bản phát hành này dứt khoát không làm.

`verdict` là câu tóm tắt trung thực một dòng: một nền tảng lỗi khiến bản phát hành
"not release-ready", một nền tảng chưa chạy khiến nó "partially verified", và một
benchmark chưa đo vẫn được nêu là chưa đo ngay cả khi mọi nền tảng đều xanh.

## 9. Những gì người vận hành không được giả định

- Không tiến trình nền nào giữ hệ thống gọn gàng. Nếu bạn không chạy `gc`, không gì
  được thu gom; nếu bạn không chạy `backup`, không gì được bảo vệ.
- Phục hồi không phải một công tắc. Nó tạo ra một thư mục ứng viên; kích hoạt nó là
  quyết định của người vận hành với hệ quả riêng.
- Forget là cục bộ và bền vững, không phải toàn cầu. Hãy đọc `surviving_copies` trước
  khi nói với ai rằng dữ liệu đã biến mất.
- Một bản sao lưu chỉ được coi là đã kiểm chứng khi `verify-backup` nói vậy trên đúng
  phương tiện bạn đang giữ.
- Artifact còn trong thời gian ân hạn, đang bị pin, hoặc đang được tham chiếu sẽ không
  bị thu gom, và đó là hành vi đúng chứ không phải lỗi.
- Các gate chứng minh đúng hành vi chúng đã chạy. Nền tảng, capability và benchmark
  chúng chưa chạy đều được báo là như vậy; hãy coi mọi tuyên bố không có trong ma trận
  là chưa kiểm chứng.

## 10. Tóm tắt lệnh

| Lệnh | Mục đích | Từ chối khi |
| --- | --- | --- |
| `ha maintenance doctor` | Chẩn đoán một thư mục dữ liệu | Không bao giờ; thay vào đó báo `writable: false` |
| `ha maintenance backup` | Ảnh chụp vào một thư mục mới | Thư mục sao lưu đã tồn tại; nguồn không có store |
| `ha maintenance verify-backup` | Kiểm tra một bản sao lưu và các artifact của nó | Manifest, cơ sở dữ liệu hay bất kỳ artifact nào sai hash |
| `ha maintenance restore` | Phục hồi vào thư mục mới, không kích hoạt | Đích đã có store hoặc đang active; ảnh chụp không đầy đủ |
| `ha maintenance retain` | `invalidate`, `archive` hay `forget` một nguồn | `forget` thiếu `--confirm` bằng `--source-kind:--source-id` |
| `ha maintenance tombstones` | Liệt kê nguồn đã quên và các bản sao còn lại | Không bao giờ |
| `ha maintenance gc` | Thu gom artifact không tham chiếu, không pin, đã cũ | Không bao giờ; nó báo những gì được giữ và vì sao |
| `ha maintenance migrate-copy` | Chuyển đổi store trên một bản sao | Đích đã có dữ liệu; nguồn không có store |
| `ha maintenance release-matrix` | Báo cáo nền tảng, capability và tính trung thực của benchmark | Không bao giờ; nó báo những gì chưa kiểm chứng |

## 11. Đưa CLI vào terminal

```console
pwsh -NoProfile -File scripts/Install-Ha.ps1
ha --version
ha maintenance doctor --data-dir <DATA_DIR>
```

`ha` là một executable bình thường, nên "cài" nghĩa là đưa executable đó vào
`PATH`. Repo có sẵn một script làm việc đó, theo hai đường:

| Đường | Lệnh | Việc nó làm |
| --- | --- | --- |
| Copy (mặc định) | `scripts/Install-Ha.ps1` | Build `ha` bằng `cargo build --release -p harness-cli --bin ha --locked` rồi copy artifact đã biên dịch vào `$HOME/.cargo/bin`, thư mục mà trình cài Rust đã đặt sẵn trong `PATH` |
| Cargo | `scripts/Install-Ha.ps1 -UseCargoInstall` | Chạy `cargo install --path crates/harness-cli --locked --root $HOME/.cargo`, để sau này `cargo uninstall harness-cli` gỡ được |

Vài biến thể hữu ích:

```console
pwsh -NoProfile -File scripts/Install-Ha.ps1 -Profile Debug        # build nhanh hơn, cho vòng lặp cục bộ
pwsh -NoProfile -File scripts/Install-Ha.ps1 -Force                # build lại kể cả khi binary có vẻ còn mới
pwsh -NoProfile -File scripts/Install-Ha.ps1 -Destination <DIR>    # cài vào chỗ khác
pwsh -NoProfile -File scripts/Install-Ha.ps1 -SkipBuild            # cài binary đã build sẵn
```

Những gì script **không** làm, một cách có chủ ý: không tải gì về, không publish gì
lên package registry, và không tự sửa `PATH` của bạn trừ khi bạn yêu cầu rõ bằng
`-ModifyUserPath`. Khi thư mục cài chưa có trong `PATH`, nó in ra đúng thư mục cần
thêm thay vì âm thầm sửa profile; khi bạn truyền `-ModifyUserPath`, nó chỉ sửa
**User PATH** (thêm một lần, không nhân bản, giữ nguyên các entry khác và không bao giờ
ghi Machine PATH hay PATH tổng hợp của process) và nói rõ terminal mới mới thấy thay đổi.

Từ bản này script còn:

- cài **đúng artifact Cargo báo**, kiểm tra digest và `--version` của bản staged trước
  khi thay thế, giữ file cũ làm backup để rollback nếu bước cuối thất bại;
- ghi manifest `ha.install.json` cạnh binary (version, sha256, source, build commit,
  danh sách file sở hữu) để update/uninstall chỉ đụng file của mình;
- phân loại lỗi khi thay thế: file đang chạy là `in_use` (đóng app rồi chạy lại — script
  **không** kill process nào), thiếu quyền là `access_denied`;
- cảnh báo nếu một `ha` khác đứng trước trên `PATH`; nó **không** xóa command đó.

Chạy `pwsh -NoProfile -File scripts/Install-Ha.ps1 -SelfTest` để tự kiểm chứng các luật
trên mà không cài vào đâu thật.

Cả hai đường đều build từ đúng cây source mà phase gate kiểm; chỉ khác cargo profile.
Nếu bạn muốn đúng artifact mà release gate đã chạy, hãy dùng profile release mặc định.

Gỡ lại:

```console
Remove-Item "$HOME/.cargo/bin/ha.exe"     # đường copy
cargo uninstall harness-cli               # đường cargo
```

Nếu bạn cài từ **bundle release** (`-FromBundle <dir>`, đường end-user), nên gỡ bằng
chính installer để nó chỉ xóa thứ nó đã ghi:

```console
pwsh -NoProfile -File scripts/Install-Ha.ps1 -Uninstall -Destination <DIR>
pwsh -NoProfile -File scripts/Install-Ha.ps1 -Uninstall -Destination <DIR> -RemoveUserPathEntry
```

- Lệnh đầu gỡ **đúng các file trong manifest** và **giữ nguyên** entry `PATH` mà lần cài đã
  thêm (nó in ra entry đó kèm cách gỡ). Lệnh thứ hai xóa luôn entry — cần cờ riêng vì lần
  cài cũng cần `-ModifyUserPath` mới được ghi vào User PATH.
- Cả hai đều **không** đụng config và session data của bạn. Hiện **chưa** có lệnh nào xóa dữ
  liệu người dùng: kế hoạch có nêu một đường `purge` riêng kèm kiểm tra path containment và
  xác nhận, nhưng nó **chưa** được implement, nên đừng trông vào nó.

Hai điều cần biết sau khi cài:

- **`ha` chạy nền trước.** Không có daemon: không có gì chạy giữa các lệnh của bạn,
  nên backup, thu gom hay thao tác retention xảy ra đúng lúc bạn gọi và không lúc nào khác.
- **Thư mục mới là đích hợp lệ.** `ha maintenance doctor --data-dir <DIR>` chạy được
  trên thư mục chưa có store và báo nó là chưa khởi tạo — đúng trạng thái mà binary
  này được phép tạo store trong đó. Nó không giả vờ rằng store đã tồn tại, và việc
  sao lưu một thư mục như vậy vẫn bị từ chối.

Build từ source vẫn dùng được cho phát triển:
`cargo build -p harness-cli --bin ha --locked` ghi ra `target/debug/ha`, và
`cargo run -p harness-cli --bin ha -- <args>` chạy nó mà không cài gì.


## 12. Sử dụng `ha` tương tác và headless

`ha` hoặc `ha chat` mở chat ở terminal. `ha chat --cwd <project>` chọn workspace; khi stdin/stdout không phải terminal, lệnh tương tác thoát mã 2 thay vì chờ. `ha chat --plain` dùng giao diện dòng; `ha chat --fixture` dùng fixture cục bộ. `/status` cho biết project, provider, dữ liệu và trạng thái thiết lập. Key có thể đến từ biến môi trường hoặc nhập bằng `/key`; giá trị không xuất hiện trong status hay transcript. `/key <value>` là dạng hiển thị khi gõ, còn `/key` trần mở ô nhập có che ký tự.

### 12.1. Công cụ của agent

18 công cụ cốt lõi đi qua host, policy và receipt. Các tool skill (`list_skills`, `activate_skill`, `read_skill_file`), tool web (`web_search`, `web_fetch`), Python REPL (`ipython`) và, khi không có REPL, `mcp__<server>__<tool>` được thêm khi khả dụng.

**MCP theo cách của prime-agent.** Khi có Python REPL, MCP server không còn là tool native: object `mcp` được import sẵn trong kernel (`rlm.mcp` của prime-agent, đã vendor) tự mở server đã cấu hình - `await mcp.list_tools("<server>")`, `await mcp.call_tool("<server>", "<tool>", arguments)`, `await mcp.list_connections()` - và prompt liệt kê các server đang bật. Server là những server `ha mcp add` đã cấu hình; host đưa cấu hình từng server cho kernel theo định dạng của prime-agent (`secret://NAME` thành tham chiếu `{"env": "NAME"}`; `streamable_http` thành `http` kèm `bearerTokenEnvVar`). Package Python `mcp` có sẵn trong kernel venv. Catalog dịch vụ và đăng nhập OAuth của prime-agent chưa được port: danh sách plugin rỗng và yêu cầu làm mới credential bị từ chối. Khi không có REPL, hoặc khi tin nhắn đính kèm resource của server bằng `@server:uri`, server được kết nối native như trước; `/mcp` cho biết mỗi server đi theo đường nào.

**Web.** `web_search` tìm kiếm trên web: dùng Google qua Serper khi có `SERPER_API_KEY` (key miễn phí tại serper.dev), nếu không thì dùng DuckDuckGo, không cần key. `web_fetch` mở một trang http(s) và trả về văn bản đọc được kèm danh sách link, chia theo từng đoạn và đọc tiếp bằng `start_index`. Địa chỉ local và mạng nội bộ bị từ chối, kể cả khi bị redirect tới; file nhị phân bị từ chối. `web_search` chạy không cần bảng phê duyệt (chỉ gửi câu truy vấn); `web_fetch` phải hỏi, vì URL có thể mang dữ liệu ra ngoài - trả lời `a` để cho phép cả lượt, hoặc dùng `full-auto`. `HA_WEB=off` gỡ cả hai tool.

**Python REPL.** Tool `ipython` chạy cell trong một kernel Python bền - chính runtime của prime-agent, được vendor tại `crates/harness-cli/python` và ghi ra `<data-dir>/runtime` ở lần dùng đầu. Dùng được `await` ở top-level; biến và import được giữ qua các cell và các lượt; kernel chết thì được khởi động lại ở lần gọi sau và kết quả báo rằng state cũ đã mất. `bash('cmd')` chạy lệnh nền và trả về handle (`tail`, `output`, `poll`, `kill`, `await`); trên Windows lệnh chạy trong Git Bash, tìm ở vị trí cài mặc định hoặc đặt bằng `HA_REPL_SHELL`. Một cell chạy tối đa 10 phút rồi bị ngắt; trên Windows chỉ ngắt được cell đang chờ ở `await`, cell kẹt trong code đồng bộ sẽ khiến kernel khởi động lại. Chạy Python là chạy code, nên mỗi cell hỏi phê duyệt như `run_shell`, trừ chế độ `full-auto`. Kernel dùng project root làm thư mục làm việc và dùng môi trường của người dùng. Cần Python 3.11+ (`HA_PYTHON`, nếu không thì `python3`, `python`, `py -3`); không có thì tool không được đưa ra. `HA_REPL=off` gỡ tool.

Khi có delegation, object `rlm` trong kernel chạy agent con trên worker của lượt: `await rlm.spawn('task', name='worker')` khởi động một explorer chỉ-đọc và trả về ngay khi được nhận; `await rlm.collect([...], timeout_ms=...)` chờ câu trả lời; `rlm.list_subagents()`, `rlm.delete_subagent(...)` và `rlm.find_models()` hoạt động như ở prime-agent. Agent con ghi qua store của lượt nên, khác prime-agent, không sống quá lượt: agent con còn chạy khi lượt kết thúc sẽ bị huỷ. Agent con dùng model của agent cha; `rlm.create_session` (session daemon) không khả dụng.

**Python skill.** Các skill của prime-agent đi kèm ứng dụng (`.agents/skills`, MIT; xem `.agents/PRIME-AGENT-SOURCE.md`): `edit`, `websearch`, `attach_image`, `goal`, `compact`, `refine`, `agent_message`, `agent_observe`, `rlm_heartbeat`, cùng các hướng dẫn `mcp` và `skill-creator`. Skill có package Python được import vào kernel theo tên khi kernel khởi động - `await edit(path=..., old_str=..., new_str=...)`, `await goal.complete()`, `await compact.run()` - và được liệt kê trong prompt kèm `python_import`. Skill import lỗi được thay bằng một stub nói rõ lý do, và cell đầu tiên báo điều đó. Host trả lời các yêu cầu của skill: `goal.*` điều khiển `/goal` (mục tiêu model tạo được xử lý như mục tiêu bạn đặt; ha không có token budget), `compact.run` hẹn `/compact` chạy khi lượt kết thúc, `model.info` cho biết model, `agent_observe` đọc các agent con `rlm.spawn` của lượt, `rlm_heartbeat` giữ các lời nhắc lặp lại cho session (mặc định `every 5m`; loại `steer` chen vào lượt đang chạy qua hộp `/steer`, loại `follow_up` đợi lượt kết thúc), và ảnh mà `attach_image` nạp được gửi cho model cùng kết quả tool. `agent_message` bị từ chối: agent này không có cha và các agent con không nhận tin nhắn.

**Kernel venv.** Giống prime-agent, kernel chạy trong một venv ở `<data-dir>/kernel-venv`, dựng bằng `uv` ở lần dùng đầu - Python 3.11, `dill`, bộ package mặc định của prime-agent (requests, httpx, pyyaml, tomli, python-dotenv, pandas, numpy, scipy, beautifulsoup4, lxml, pydantic, tyro) và `pillow` - và dựng lại khi danh sách đó đổi. ha không bao giờ tự cài `uv`: không có nó thì kernel chạy trên Python hệ thống, cell đầu tiên báo điều đó, và skill thiếu package (`websearch` cần httpx, `attach_image` cần pillow) được báo là không khả dụng. `HA_PYTHON` chỉ định một trình thông dịch dùng nguyên như vậy.

**Skill** theo cấu trúc Agent Skills (một thư mục có `SKILL.md` cùng các file đi kèm). Skill được tìm trong bộ tích hợp sẵn, `<config-dir>/skills`, `~/.agents/skills`, mọi thư mục liệt kê trong `HA_SKILL_PATHS` (phân tách như `PATH`, ví dụ `~/.claude/skills`), và - với project đã trust - `.harness/skills` cùng `.agents/skills` ở workspace và từng thư mục cha tới Git root. System prompt liệt kê tên, mô tả và vị trí từng skill trong khối `<available_skills>`, giống prime-agent; model kích hoạt skill theo tên (digest từ `list_skills` là tùy chọn để ghim phiên bản, chấp nhận có hoặc không có `sha256:`). Khi kích hoạt, model nhận hướng dẫn cùng thư mục và danh sách file của skill, và `read_skill_file` đọc các file đó - chỉ trong phạm vi skill. Ba tool skill chỉ đọc catalogue đã trust nên chạy không cần bảng phê duyệt; deny rule vẫn chặn được. `disable-model-invocation: true` trong front matter ẩn skill khỏi model; `/skill:<name>` vẫn chạy được.

| Nhóm | Tool | Mục đích |
| --- | --- | --- |
| Workspace | `read_file`, `list_files`, `search_text`, `glob` | Đọc và tìm kiếm có giới hạn trong workspace |
| Sửa file | `apply_patch`, `write_file`, `edit_file` | Ghi với hash/điều kiện match và phê duyệt |
| Process | `run_process`, `run_shell`, `read_process_output` | Chạy lệnh và đọc output artifact có giới hạn |
| Git | `git_status`, `git_diff`, `git_log` | Quan sát repository |
| Task | `task_update`, `delegate` | Ghi next action; giao explorer/coder |
| Lịch sử | `history_search`, `history_read` | Tìm và đọc journal của task |
| Người dùng | `ask_user` | Tạm dừng và hỏi, không cấp quyền tool |

`delegate` giới hạn một tầng, tối đa ba child. Explorer chỉ đọc cùng workspace; coder làm việc trên worktree Git sạch do host cấp. Nếu không thể cấp worktree, tool trả `role_unavailable`; `/agents` cho thấy tiến độ, Ctrl-C của cha hủy child.

### 12.2. Lệnh trong chat và bàn phím

| Nhóm | Lệnh |
| --- | --- |
| Trợ giúp và trạng thái | `/help`, `/status`, `/config`, `/model <name>`, `/cost`, `/context`, `/permissions`, `/hooks`, `/mcp`, `/agents`, `/skills` |
| Phiên và câu trả lời | `/new`, `/clear`, `/resume <id>`, `/rename <name>`, `/more`, `/compact [guidance]`, `/export [path]`, `/copy`, `/exit` |
| Workspace và điều khiển | `/diff`, `/undo`, `/trust [yes]`, `/init`, `/mode <ask|auto-edit|full-auto>`, `/steer <text>`, `/goal <objective>|status|pause|resume|clear` |
| Nội dung và mở rộng | `/key`, `/image`, `/attach <path>`, `/skill:<name> [args]`, `/reload`; các template trong `.harness/commands` hoặc `<config-dir>/commands` chạy bằng `/name` |

`/thinking <level>` chọn mức suy luận của model, từ `off` qua `minimal`, `low`, `medium`, `high`, `xhigh` tới `max`, giống prime-agent; `/thinking` không kèm tham số cho xem mức đang dùng và các mức model hỗ trợ. Mức model không có sẽ được kẹp về mức gần nhất (DeepSeek V4 có `off`, `high` và `xhigh`, gửi đi là `max`). Lựa chọn được lưu cùng hội thoại; `provider.thinking` / `HA_PROVIDER_THINKING` đặt mặc định. Khi bật thinking, DeepSeek nhận lại `reasoning_content` của từng tin assistant và Claude nhận lại block thinking có chữ ký, trong phạm vi một lượt; reasoning không bao giờ được lưu. Model không có trong bảng dựng sẵn được coi là không suy luận.

`/goal <mục tiêu>` đặt một mục tiêu bền. Mỗi lượt mang mục tiêu trong context, và model có tool `goal_complete` (được phép không cần hỏi). Lượt kết thúc mà chưa gọi `goal_complete` sẽ được tự động tiếp tục, tối đa 10 lần, sau đó mục tiêu tạm dừng. Ctrl-C và `/goal pause` tạm dừng; `/goal resume` tiếp tục; `/goal clear` xoá. Mục tiêu được lưu cùng hội thoại: `/resume` khôi phục nó ở trạng thái tạm dừng, `/new` bỏ nó. Trong một lượt, khi transcript của chính lượt đó vượt khoảng 200 KB, các kết quả tool cũ nhất (trừ 6 kết quả mới nhất) được thay bằng một dòng ghi chú; model có thể gọi lại tool nếu cần.

`@` mở bộ chọn file; `@<server>:<uri>` đính kèm MCP text resource. `!cmd` chạy shell qua cổng phê duyệt; `!!cmd` chỉ hiển thị output. Enter gửi, Ctrl-J xuống dòng, ↑↓ chọn menu hoặc lịch sử, PgUp/PgDn và Home/End cuộn panel `/more`, Esc đóng panel hoặc ngắt lượt, Ctrl-C hủy lượt (bấm hai lần trong 2 giây trên dòng rỗng để thoát), Ctrl-O đổi chế độ chi tiết: thu gọn ẩn reasoning và cắt output tool còn ba dòng, chi tiết hiện reasoning, mở rộng hiện toàn bộ output. Ctrl-D trên dòng rỗng thoát. Trong TUI, bản ghi cũ ở scrollback của terminal; plain mode in theo dòng.

### 12.3. Config v2, quyền và hook

Config v2 hợp nhất default → user (`<config-dir>/config.toml`) → project đã trust (`.harness/config.toml`, rồi `.harness/config.local.toml` cho permission local) → env → CLI. `/config` chỉ ra nguồn của giá trị; project chưa trust không nạp config, hook hoặc skill của project. `/trust` yêu cầu xác nhận. Profile và model có thể chọn cho lượt tiếp theo.

`[permissions]` hỗ trợ `mode = "ask" | "auto-edit" | "full-auto"`, `allow` và `deny`; deny và protected path luôn thắng allow. Panel có `y` (một action), `a` (cả lượt), `n` (từ chối); `A` đề xuất rule lâu dài nhưng chỉ ghi vào `.harness/config.local.toml` sau xác nhận riêng. Headless không tự duyệt khi chưa cấp quyền. `--allowed-tools`, `--disallowed-tools` và `--approval` dùng cùng policy. `[hooks]` nhận `pre_tool_use`, `post_tool_use`, `stop`; hook chỉ được từ lớp đã trust, có timeout và không thể biến quyết định ask thành allow. `/hooks` xem hook có hiệu lực.

`[mcp_servers.<name>]` cấu hình stdio hoặc Streamable HTTP. Stdio dùng `command`, `args`, `cwd`, `env` dạng secret ref; HTTP bearer đọc từ env. `enabled_tools`, `disabled_tools`, `tool_timeout` (tối đa 120 giây), `required` giới hạn server. Server được khởi động khi cần; `ha mcp add|list|get|remove` quản lý cấu hình, `/mcp` xem trong chat. Tool MCP đi qua cùng policy/approval và có receipt. Skills tìm ở `<config-dir>/skills`, `~/.agents/skills` và project đã trust (`.agents/skills`, `.harness/skills`); prompt ban đầu chỉ chứa tên và mô tả, nội dung nạp theo digest khi kích hoạt.

### 12.4. Automation

```text
ha exec "Summarize the changed files" --output-format text
ha exec --prompt - --output-format json < prompt.txt
ha exec "Continue the task" --continue --goal "Task complete" --max-turns 4 --output-format stream-json
```

`--prompt -` đọc tối đa 10 MiB từ stdin; `-` ở vị trí prompt là chữ thường. `text` in câu trả lời, `json` in envelope schema 1 với trường mới additive (khi `--goal` satisfied: `acceptance.command_id`), `stream-json` in mỗi event trên một dòng NDJSON, không ANSI. `--continue` chọn session mới nhất của project. Các mã thoát: 0 hoàn tất, 2 usage, 3 chờ câu hỏi hoặc approval, 4 thất bại, 5 xung đột ownership, 130 hủy. `ha chat --headless --json` vẫn tương thích cách gọi cũ. Chỉ dùng `--mock` hoặc `--fixture` cho thử nghiệm cục bộ; gọi provider thật có thể phát sinh chi phí.

### 12.5. Giới hạn và kiểm tra

`run_shell` dùng `pwsh` trên Windows và `powershell.exe` khi thiếu `pwsh`; receipt ghi shell được chọn. Strict isolation chỉ được báo khi backend đã đo hỗ trợ. `/status` và `/config` là điểm bắt đầu khi provider hoặc quyền không như dự kiến. Một lượt tạm dừng sau 30 lời gọi model hoặc 80 tool call (`HA_TURN_MAX_STEPS`, `HA_TURN_MAX_TOOL_CALLS`) và được tự tiếp tục tối đa hai lần (`HA_TURN_CONTINUATIONS`). Nếu `ha` chạy như bản cũ, `Get-Command ha -All` cho biết file thực thi nào đang chạy; cài lại bằng `scripts/Install-Ha.ps1`. Các gate M0–M6, H và PTY có evidence riêng. Linux đang chờ hỗ trợ: job CI của Linux không chặn push, và không có tuyên bố nào về Linux được suy ra từ nó.

### 12.6. Memory và `/resume`

Memory theo harness state của prime-agent: model tự giữ memory, ghi chú prompt, skill và đặc tả subagent qua `rlm.harness` trong Python REPL, loại global dùng chung cho mọi hội thoại và loại local theo từng hội thoại, và mỗi lượt mang theo digest của chúng xếp theo mức liên quan. Không có gì được lưu theo từ khoá. `/refine` (hoặc `refine.run()` trong kernel) biến hội thoại thành các chỉnh sửa đã kiểm tra, `/refine --rollback <id>` hoàn tác một lần, và cứ 25 lượt một bước review tự động refine local khi có điều đáng giữ (`HA_AUTO_REFINE=off` để tắt). `/resume` phát lại chính các lượt của hội thoại cho model và hiện chúng trên màn hình. Xem mục 19 và 20 của `MEMORY_AND_CONTINUITY`.

