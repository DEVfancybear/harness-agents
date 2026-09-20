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
- **Không có artifact phát hành nào được công bố.** Bản dựng Linux và Windows đều
  được phase gate chạy, nhưng không có gì được đóng gói, ký hay phát hành.
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
    --confirm src/lib.rs --surviving-copy backup-2026-01 --json
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

`forget` yêu cầu `--confirm` bằng đúng `--source-id`. Sai lệch hoặc để rỗng đều bị
từ chối với `retention_refused`, và không có gì được ghi.

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

Pin tồn tại để bịt một cuộc đua cụ thể: một bản sao lưu hứa giữ một artifact, và một
lượt thu gom đồng thời nếu không có pin sẽ xóa nó giữa lời hứa và lúc sao chép. Vì
pin được kiểm tra trước khi xóa bất kỳ file nào, và pin nằm trong chính cơ sở dữ liệu
mà lượt thu gom vừa đọc, artifact bị pin sẽ sống sót. Bản sao lưu ghi lại pin của nó
trong manifest, nên lời hứa vẫn kiểm toán được về sau.

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
| `ha maintenance retain` | `invalidate`, `archive` hay `forget` một nguồn | `forget` thiếu `--confirm` bằng `--source-id` |
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


## 12. Khởi động tương tác bằng `ha`

Từ track HA_LAUNCH, gõ `ha` không tham số trong một terminal sẽ mở ứng dụng tương tác
thay vì thoát ngay: header hiện project, provider và setup state, sau đó là ô nhập.

**Thay đổi hành vi cần biết (migration):** trước đây `ha` không tham số thoát với mã 0
một cách im lặng. Bây giờ:

| Tình huống | Hành vi |
| --- | --- |
| `ha` trong terminal thật | Mở ứng dụng; chỉ thoát khi bạn gõ `/exit`, Ctrl-D trên dòng rỗng, hoặc Ctrl-C khi đang chờ |
| `ha` khi stdin/stdout không phải terminal (pipe, CI, script) | **Không** treo chờ nhập: in hướng dẫn ngắn ra stderr và thoát với mã **2** |
| `ha --help`, `ha --version`, các subcommand cũ | Giữ nguyên như trước, không khởi động ứng dụng |

Lệnh và option của entrypoint tương tác:

| Lệnh | Việc nó làm |
| --- | --- |
| `ha chat` | Cùng entrypoint với `ha` không tham số |
| `ha chat --cwd <path>` | Mở project ở path đó thay vì thư mục đang đứng |
| `ha chat --resume <session-id>` | Tiếp tục từ một session đã lưu (đường tương tác dùng `/resume`) |
| `ha chat --headless --prompt "<text>" [--json]` | Chạy đúng một lượt không cần terminal; kết quả ra stdout, log ra stderr |
| `ha chat --fixture` | Dùng backend fixture **có nhãn** để thử giao diện; không gọi model nào |

**Thay đổi hành vi cần biết (định danh):** trước đây mỗi lượt sinh một project identity
mới, nên mọi thứ scope theo project — artifact của tool, approval, memory — thuộc về một
định danh mà lượt sau không gọi tên lại được. Giờ một workspace root đăng ký **một** project
identity trong store của nó, và mọi lượt sau, trong process này hay process khác, đều resolve
đúng định danh đó.

Trong ứng dụng: `/help`, `/status`, `/key`, `/config`, `/model`, `/new`, `/resume [số|id]`,
`/exit`. Khi một action cần phê duyệt, ứng dụng in action, thư mục và scope thật rồi chờ
bạn trả lời `y` (chạy một lần) hoặc `n` (từ chối); không có phê duyệt ngầm, hết thời gian
chờ được tính là từ chối.

### Phê duyệt: khi nào bị hỏi, và khi nào không

Mặc định ban đầu vẫn là **hỏi mọi action**, kể cả đọc file. Nhưng panel có thêm một lựa
chọn khi — và chỉ khi — action đó **chỉ đọc**:

| Phím | Nghĩa | Hiệu lực |
| --- | --- | --- |
| `y` | chạy action này một lần | hết action là hết |
| `a` | chạy action này **và** cho phép mọi thao tác chỉ-đọc trong **lượt này** | tới khi lượt kết thúc |
| `n` | từ chối | action không chạy |

`a` chỉ hiện với 5 action chỉ-đọc: `read_file`, `list_files`, `search_text`, `git_status`,
`git_diff`. `apply_patch`, `run_process`, `run_shell`, `task_update` và tool của extension
**luôn** hỏi từng lần, `a` không có tác dụng với chúng.

Sau khi bạn bấm `a`, thanh trạng thái hiện `· reads tự động` để bạn biết cổng đang mở, và
transcript ghi một dòng `[info] read-only, allowed for this turn: <action>` cho mỗi thao tác
được chạy theo diện này — không có gì chạy mà không để lại dấu.

Bốn điều `a` **không** làm được, kiểm bằng test:

1. **Không** đọc được file trong danh sách bảo vệ: `.env`, `.env.*`, `.git`, `.harness`, và
   tên chứa `credential`, `secret`, `password`, `private_key`, hay đuôi `.pem`/`.key`/`.p12`/
   `.pfx`/`.clixml`. Các đường này bị từ chối **trước khi** panel tồn tại, nên không có gì để
   `a` cho phép.
2. **Không** ra khỏi workspace: đường dẫn tuyệt đối, `..`, và symlink/reparse point đều bị từ
   chối trước panel.
3. **Không** sống qua lượt sau: hết lượt là cổng đóng, kể cả khi lượt kết thúc bằng lỗi hay
   Ctrl-C.
4. **Không** áp cho lượt đang chạy khi bạn chưa bấm `a`: mặc định vẫn hỏi.

**Chỉ cần một API key.** `DEEPSEEK_API_KEY` (hoặc `HA_API_KEY`) là đủ: endpoint và
model tự lấy giá trị `DeepSeek` công bố — `https://api.deepseek.com` và
`deepseek-flash` (xem <https://api-docs.deepseek.com/>). Đặt `HA_PROVIDER_ENDPOINT`
hoặc `HA_PROVIDER_MODEL` khi bạn dùng provider/model khác; biến bạn đặt luôn thắng giá
trị mặc định.

Thiếu **key** thì ứng dụng vẫn mở, hiện setup state và nói rõ còn thiếu biến nào — nó
**không** gọi model và **không** trả câu trả lời giả.

```powershell
$env:DEEPSEEK_API_KEY = '<key-của-bạn>'   # chỉ cần dòng này
ha                                        # mở TUI; thanh trạng thái hiện model đang dùng

# tuỳ chọn, chỉ khi dùng model/provider khác:
$env:HA_PROVIDER_MODEL = 'deepseek-v4-pro'
```

**Khi provider trả lỗi, gõ `/model` hoặc `/status` trong app**: hai lệnh đó in ra
credential đang lấy từ biến nào (giá trị không bao giờ hiện), endpoint và model đang dùng
là do bạn đặt hay do mặc định, và endpoint có kết nối được không. Ví dụ:

```text
backend: deepseek-flash via https://api.deepseek.com
Provider: credential from environment variable DEEPSEEK_API_KEY (value hidden)
Provider: endpoint not set, using the default https://api.deepseek.com
Provider: model not set, using the default deepseek-flash
Provider: ready, would call deepseek-flash
Provider: endpoint answered a TCP connection
```

Nếu dòng cuối là `endpoint did not answer`, lỗi nằm ở mạng/proxy. Nếu là
`no credential; set one of ...`, key chưa được đặt **trong chính shell đang chạy `ha`**
(biến môi trường chỉ có tác dụng với tiến trình được khởi động sau khi bạn đặt nó) — hoặc
bạn lưu key ngay trong app bằng `/key` (mục 12.4).

Muốn kiểm tra provider thật trước khi mở app (tốn một lượt gọi trả phí) — cũng chỉ
cần key:

```powershell
pwsh -NoProfile -File scripts/Smoke-HaProvider.ps1
```

### 12.1. Giao diện TUI (track HA_TUI)

Từ track HA_TUI, ứng dụng tương tác vẽ **inline viewport** ở đáy màn hình: hội thoại vẫn
chảy vào scrollback của terminal (cuộn bằng chính terminal), còn đáy màn hình là vùng cố
định gồm ô soạn thảo, thanh trạng thái và panel tạm. Đây **không** phải app full-screen.

```text
Harness Agents 0.1.0
Project: C:\work\my-project    Provider: deepseek-chat via https://api.deepseek.com
Session: new    Mode: trusted host

> Sửa lỗi parser và chạy tests
● [tool] read_file path=src/parser.rs  ok 12ms
[run] done
──────────────────────────────────────────────────────────
 Vì vậy tôi sẽ sửa như sau:          ← vùng đang stream
┌ Enter gửi · Ctrl-J xuống dòng · /help ─────────────────┐
> _                                  ← ô soạn thảo (1..8 dòng)
 ⠹ running · step 2/8 · tools 2/16 · 00:07 · Ctrl-C hủy
```

Bàn phím (chỉ những phím đã đo trên console thật):

| Phím | Việc nó làm |
| --- | --- |
| `Enter` | Gửi yêu cầu (không gửi buffer rỗng) |
| `Ctrl-J` | Xuống dòng trong ô soạn thảo |
| `Alt+Enter` | Xuống dòng (đo được trên ConPTY của Windows Terminal; xem giới hạn bên dưới) |
| Dán nhiều dòng | Giữ nguyên newline, **không** gửi; cả khối là **một** yêu cầu khi bạn Enter |
| `↑` / `↓` | Buffer một dòng: lịch sử; buffer nhiều dòng: di chuyển theo hàng |
| `←` `→` `Home` `End` | Di chuyển theo ký tự |
| `Ctrl-A` / `Ctrl-E` | Đầu / cuối dòng hiện tại |
| `Ctrl-U` / `Ctrl-W` | Xoá tới đầu dòng / xoá một từ |
| `Tab` | Hoàn thành slash command khi chỉ có một gợi ý (`/re` → `/resume`) |
| `Esc` | Đóng panel/overlay hoặc xoá gợi ý; **không** huỷ lượt đang chạy |
| `Ctrl-C` | Đang chạy: huỷ lượt · đang rảnh: xoá buffer |
| `Ctrl-D` | Buffer rỗng: thoát |
| `Ctrl-L` | Vẽ lại vùng đáy, không xoá scrollback |
| `y` / `n` | Trả lời panel phê duyệt (hoặc gõ `yes`/`no` rồi Enter) |
| `a` | Chỉ khi action **chỉ đọc**: chạy nó và cho phép đọc cả lượt (hoặc gõ `all` rồi Enter) |

Khi một action cần phê duyệt, panel hiện action, workspace, scope và **đếm ngược** tới
hạn của gate; hết hạn thì action **không** chạy và panel tự đóng.

### 12.2. Khi nào ứng dụng dùng giao diện đơn giản (plain)

TUI là mặc định. Ứng dụng rơi về **plain mode** (chính giao diện cũ: dòng chữ nối tiếp
trên prompt `> `) khi một trong các điều sau đúng, và **luôn in lý do ra stderr**:

| Điều kiện | Ví dụ |
| --- | --- |
| Bạn yêu cầu | `ha chat --plain` hoặc `HA_UI=plain` |
| Terminal quá nhỏ | nhỏ hơn 60 cột × 10 hàng |
| Terminal không định vị được con trỏ | `TERM=dumb` |
| Raw mode không bật được | ứng dụng in lý do rồi dùng chế độ nhập theo dòng |

`--plain` xung đột với `--headless` (clap từ chối, exit 2): một lượt headless không bao
giờ vẽ viewport.

### 12.3. Giới hạn trên Windows (đã đo)

- **Shift+Enter không phân biệt được với Enter** trên console Windows, nên nó **không**
  được ghi vào help và không phải đường xuống dòng. Dùng `Ctrl-J` (đường chính thức) hoặc
  `Alt+Enter`.
- Con trỏ terminal là do ứng dụng đặt; sau khi thoát ứng dụng để con trỏ ở cột 0 trên dòng
  mới, và hội thoại vẫn còn trong scrollback.
- Bị kill cứng (Task Manager, mất điện) thì terminal **không** được phục hồi — đó là giới
  hạn đã biết của mọi ứng dụng terminal, không phải lỗi của `ha`.

**Chưa được kiểm chứng trên máy này:** transcript PTY thật (ConPTY không hoạt động trong
môi trường sandbox đang dùng — xem mục 8 của `docs/evidence/HA_LAUNCH.vi.md`) và live
provider smoke (không có credential/budget được cấp). Đừng coi hai điều đó là đã đạt.

### 12.4. Lưu API key trong app bằng `/key`

Bạn **không** cần mở shell đặt biến môi trường trước khi mở `ha`: gõ `/key` ngay trong app.

| Cách gõ | Việc nó làm | Riêng tư |
| --- | --- | --- |
| `/key` rồi Enter | Ô soạn thảo vào chế độ nhập bí mật: mỗi ký tự hiện thành `•`; Enter lưu, **Esc huỷ** | Key không vào history của app và không được vẽ lên màn hình |
| `/key <key-của-bạn>` | Lưu ngay, không hỏi lại; chỉ nhận **một từ** | **Kém riêng tư hơn**: giá trị nằm trong history của terminal/shell — `/help` nói đúng câu này |

`/key` bị từ chối khi một lượt đang chạy. Sau khi lưu, app báo đã lưu vào file nào và
**không cần khởi động lại**: lượt gửi kế tiếp dùng key đó. Nếu chưa có `config.toml`, app
ghi file tối thiểu (`schema_version = 1`) để cổng "setup required" được xoá; file đó
**không** chứa key — key luôn nằm ở file riêng, có chủ ý.

Key được lưu ở đâu:

```text
<data dir>\private\credentials.env
# Windows mặc định: %LOCALAPPDATA%\HarnessAgents\data\private\credentials.env
```

Thư mục con `private\` là **cố ý**: nó tách key khỏi các project store nằm cùng data dir, để app
siết quyền trên đúng thư mục đó mà không đụng ACL của dữ liệu khác. Nội dung file là đúng một
dòng `DEEPSEEK_API_KEY="<key>"`. Biến `HA_CREDENTIALS_DIR` đổi **thư mục** chứa file này và được
dùng **nguyên như bạn đặt** (app không tự thêm `private\` lần nữa) — đó cũng là cách ly file khỏi
dữ liệu thật khi bạn muốn thử. Không cần đoán đường dẫn: `/status` và `/model` in ra nguồn kèm
đường dẫn thật (`credential from saved file <path>`).

Quyền, và app **nói thật** về quyền:

- Trên Unix: file được tạo với `0600` **ngay lúc tạo** (không phải siết sau khi ghi) và thư mục
  `0700`.
- Trên Windows: app cắt quyền kế thừa của thư mục credential (`icacls <dir> /inheritance:r`) rồi
  chỉ giữ **tài khoản đang dùng** và `SYSTEM`; file thừa hưởng ACL đó. Đo trước/sau trên máy này:
  trước khi lưu, thư mục cha còn `CodexSandboxUsers:(I)(OI)(CI)(RX)` — một nhóm **không phải** bạn
  đọc được; sau khi lưu, thư mục credential chỉ còn `NT AUTHORITY\SYSTEM` và `<DOMAIN>\<USER>`.
- Nếu bước siết ACL **không chạy được** (ví dụ môi trường bị chặn đổi ACL: `icacls` trả
  `Access is denied`), app **không** giả vờ: `/status` in dòng
  `Provider: credential file protection: the profile default only: no owner-only permission could
  be applied, so another account on this machine may be able to read the file`. Hãy đọc dòng đó
  trước khi tin rằng key đã được siết. File tìm thấy lúc khởi động được báo là
  `not re-measured now` — app chỉ đo lúc nó tự lưu key.

**Luật ưu tiên (quan trọng):** biến môi trường `DEEPSEEK_API_KEY` hoặc `HA_API_KEY` **luôn
thắng** file. File chỉ là phương án dự phòng khi cả hai biến đều vắng hoặc rỗng. Nếu bạn vẫn
export biến trong shell đang chạy `ha`, `/key` lưu được file nhưng app vẫn dùng biến —
`/status` và `/model` in ra nguồn đang thực sự được dùng (`credential from environment
variable ...` hay `credential from saved file ...`), và không bao giờ in giá trị.

Hoàn tác: xoá file.

```powershell
Remove-Item "$env:LOCALAPPDATA\HarnessAgents\data\private\credentials.env"
```

Không cần dọn gì thêm: `config.toml` không chứa key nên giữ nguyên được, và app quay lại
trạng thái "setup required" ở lần mở kế tiếp nếu không còn biến môi trường nào. Nếu bạn từng
đặt `HA_CREDENTIALS_DIR`, xoá file trong **thư mục đó** thay vì đường dẫn mặc định ở trên;
`/status` in ra đường dẫn thật đang dùng.

Esc **thật sự huỷ** chế độ nhập bí mật: bấm Esc thì ô trở lại bình thường, không có gì được lưu,
và file key đã lưu trước đó **không** bị ghi đè. Ctrl-C khi rảnh chỉ xoá ô, không thoát chế độ;
muốn thoát hẳn thì bấm Esc. Mask `•` không chỉ là thứ ô soạn thảo hiện: nó được canh bằng test ở
cả tầng khung hình đã vẽ, nên key không có đường lên màn hình.

**Chưa được kiểm chứng trên máy này:** hành vi lưu key có test đơn vị (`docs/evidence/HA_TUI.vi.md`
mục 11) và đường CLI thật đọc **file** credential ở `private\` đã được đo end-to-end bằng binary
release với endpoint loopback chết (evidence 11.4: app đọc file, mở store, đi tới lời gọi provider
rồi exit 1 — không có request nào rời máy). Nhưng **chưa** có ca PTY nào lái `/key` trong console
thật (mask mới chỉ được canh ở tầng khung hình vẽ bằng backend test, không phải ConPTY), **chưa**
có lượt gọi provider thật nào bằng key lưu trong app, và phép đo ACL Windows chỉ có trên **một**
máy — một môi trường bị chặn đổi ACL sẽ rơi về mặc định profile (và `/status` nói đúng như vậy).
Đừng coi ba điều đó là đã đạt.

### 12.6. Đọc lại câu trả lời dài bằng `/more`, phím cuộn và con lăn chuột

Mục này bổ sung cho 12.1 và 12.3: nó nói về hai đường đọc một câu trả lời **dài hơn vùng đáy
màn hình**, và về con lăn chuột.

**`/more` — mở lại transcript gần nhất, từ dòng đầu.** Vùng đáy chỉ giữ **phần đuôi** của câu
trả lời đang stream, nên phần bị cắt chính là **phần đầu**. Gõ `/more` để mở lại transcript gần
nhất trong **cùng loại panel** mà `/help` dùng, và panel **mở ở dòng đầu** — bạn không phải cuộn
trước khi thấy thứ mình cần.

| Phím (khi panel đang mở) | Việc nó làm |
| --- | --- |
| `PageUp` / `PageDown` | Cuộn 8 dòng một lần |
| `Home` / `End` | Về dòng đầu / nhảy tới dòng cuối |
| `Esc` | Đóng panel |

Viền dưới của panel luôn nói bạn đang ở đâu: `còn N dòng` (còn N dòng nữa ở phía dưới),
`dòng x/y` (đang ở dòng x trên y), `cuối` (đã tới đáy), cộng `Esc đóng`. Đóng panel **không**
ghi gì vào hội thoại. Ở plain mode (12.2), `/more` in ra như mọi lệnh tham khảo khác.

**Giới hạn phải biết:** `/more` chỉ giữ **500 dòng gần nhất** của phiên, và đếm theo **dòng
logic** (một dòng chữ, không phải một hàng màn hình). Phiên dài hơn thế thì phần cũ nhất **chỉ
còn trong scrollback của terminal** — cuộn terminal lên vẫn thấy, nhưng `/more` không lấy lại
được. Đây là chủ ý: app không sao chép scrollback của terminal, nó chỉ thêm một cửa sổ đọc nhỏ.

**Con lăn chuột.** App bật chế độ *alternate scroll* của terminal (`DECSET 1007`) khi khởi động
và tắt khi thoát. Nghĩa là con lăn vẫn cuộn **scrollback của terminal** như bạn quen, và app
**không** chiếm chuột: không có chế độ mouse-capture nào được bật. Đây là chủ ý — mouse capture
sẽ lấy con lăn khỏi terminal **và xoá scrollback**, mà scrollback chính là nơi app đẩy toàn bộ
hội thoại; bật nó sẽ đổi "mất phần đầu câu trả lời trong vùng đáy" lấy "mất luôn phần đầu trong
lịch sử cuộn", tệ hơn hẳn.

**Chưa được kiểm chứng trên máy này:** `/more`, phím cuộn và con lăn **chưa từng** được lái
trong console thật ở lượt này (ConPTY cần console mà môi trường build không có). Ba test của
feature chỉ khẳng định trên **state** của controller/editor — **không** phải terminal thật, và
**không** có ca PTY nào gõ `/more` hay `PageUp`/`PageDown`/`Home`/`End`. Riêng `1007` là hành vi
**phía terminal**: không test nào chứng minh được một terminal cụ thể tôn trọng nó, và nó **chưa**
được đo trên Windows Terminal hay emulator nào khác. Đừng coi ba điều đó là đã đạt.

Một chi tiết nhỏ đã đo, chưa sửa: dòng gợi ý ở viền dưới panel có ghi `↑↓` nhưng `↑`/`↓`
**không** cuộn panel — chúng vẫn thuộc ô soạn thảo (lịch sử / di chuyển theo hàng) và vì vậy có
thể **sửa bản nháp** đang gõ dở trong khi panel mở. Muốn cuộn panel thì dùng `PageUp`/`PageDown`
hoặc `Home`/`End`. Ghi ở `docs/evidence/HA_TUI.vi.md` mục 12.4 cùng phần còn lại của lượt này.

### 12.5. Memory trong chat (bật tường minh)

Memory **mặc định tắt**. Đặt `HA_MEMORY=on` trong shell khởi động `ha`; giá trị khác, hoặc
không đặt, giữ nguyên hành vi phía trên: không đọc memory và không lưu gì về cuộc trò chuyện.

Khi bật, một lượt làm hai việc có giới hạn:

- **trước khi gửi request**, text của bạn là query truy xuất; phần memory của workspace này
  khớp được sẽ vào context mà model nhận, kèm đúng version memory của từng block;
- **sau lượt**, text mà journal đã admit được lưu **một lần** thành asset memory đã xác nhận,
  scope theo project. Chỉ text bạn gửi được lưu — không bao giờ lưu câu trả lời của model.

Workspace root giữ **một** project identity trong store, nên memory của lần chạy trước vẫn đọc
được ở lần chạy sau, kể cả terminal mới, session mới hay task mới.

**Truy xuất hoạt động thế nào (đã đổi).** Trước đây app khớp tài liệu chứa **mọi** term của câu
hỏi, rồi khi không có kết quả thì thử lại bằng bốn term dài nhất. Cách đó hỏng đúng ở trường hợp
thường gặp nhất: bạn hỏi "what marker did I ask you to remember?", tài liệu viết "Remember this
marker for later", và phép giao trượt vì những từ chỉ có trong câu hỏi. Câu trả lời nằm trong
store mà model báo là không có.

Giờ app **hợp** các term lại, rồi chỉ giữ kết quả chứa **ít nhất hai** term của câu hỏi (câu hỏi
một term thì ngưỡng là một). Sàn hai term là chỗ quan trọng: một từ chung chung là trùng hợp từ
vựng, không phải bằng chứng là đúng chủ đề — nếu không có sàn thì truy vấn rộng chỉ đổi "im lặng
không tìm thấy" thành "tìm thấy thứ sai", mà cái sau tệ hơn. Nếu không ứng viên nào đủ ngưỡng, app
quay về đúng phép giao cũ, nên câu hỏi đòi chính xác vẫn chính xác.

Mỗi lượt vẫn in ra điều đã xảy ra (`memory: 1 hit(s), 1 block(s) injected`). Hai loại "rỗng" được
nói khác nhau: `nothing matching this question yet (no term overlap)` nghĩa là có tìm nhưng không
khớp, còn `nothing to search for in this message` nghĩa là tin nhắn không có gì để tra. Lượt
headless báo cùng thông tin trong `--json`, ở khoá `memory`.

**Cái gì được nhớ (đã đổi).** Chỉ **chỉ dẫn và khai báo** được lưu. Một **câu hỏi** thì không:
nó là bạn đang hỏi, không phải bạn đang dặn, và trước đây mỗi câu hỏi thành một asset
`user_instruction` đã-xác-nhận — đó là cách corpus đầy câu hỏi rồi chúng lấn át câu trả lời. Khi
một input bị bỏ qua, app **nói ra** (`memory: not stored (a question is not an instruction)`)
chứ không im lặng.

Nói cùng một câu hai lần là **một** memory: asset cũ được giữ nguyên id, nguyên version và
nguyên content hash, chỉ ghi thêm event nguồn. Audit không mất, và không sinh bản gần trùng để
cạnh tranh thứ hạng với bản gốc.

Cùng lượng memory đó tra được từ CLI; store của project chính là thư mục header của app in ra:

```powershell
ha memory --data-dir "$env:HA_HOME\data\projects\<project-key>" --principal local-user search "marker"
```

Giới hạn của truy xuất, để không phải đoán: tối đa **8** hit mỗi lượt, memory bổ sung bị chặn ở
**800 token**, và `search --limit` nhận **1..=32**. Nội dung memory vào context như block **tuỳ
chọn** — nó có thể bị loại khi ngân sách token chật, và nó **không bao giờ** thành block bắt buộc.

Extraction (`ha memory catch-up`) có thêm `--asset-scope session|project`. `session` giữ những
gì một lượt trích ra riêng cho stream của lượt đó — đây là mặc định; `project` ghi chúng thành
kiến thức mà mọi session sau của project đọc được. Scope là một phần của strategy, nên đổi
scope sẽ mở một thế hệ cursor mới thay vì tái dùng thứ scope kia đã settle.

Thứ extraction suy luận ra được settle ở trạng thái **candidate**, và candidate **không** tra
được cho tới khi có người xác nhận. Xem thứ đang chờ rồi xác nhận:

```powershell
ha memory --data-dir <store> --session-id <id> candidates --limit 16   # đang chờ gì
ha memory --data-dir <store> --session-id <id> confirm --limit 8 --confirm
ha memory --data-dir <store> --session-id <id> search "parser"         # giờ tra được
```

`confirm` từ chối nếu thiếu `--confirm`, chỉ xác nhận candidate mà principal được phép publish,
và bị chặn ở 64 asset mỗi lần gọi. Xác nhận tạo **version mới** — không bao giờ viết lại nội
dung mà extractor đề xuất.

Memory không bắt buộc để chạy app: khi `HA_MEMORY` không được đặt, truy xuất bị bỏ qua và không
ghi gì.

### 12.6. Extension cục bộ trong một lượt chat (bật tường minh)

Extension **mặc định tắt**. Đặt `HA_EXTENSIONS=on` trong shell khởi động `ha`; giá trị khác giữ
đúng chín tool built-in. Khi bật, một lượt đọc mọi installation dưới `<HA_HOME>/data/extensions`
(đổi bằng `HA_EXTENSIONS_ROOT`), chỉ start thứ đã được trust, quảng bá các tool mà installation
khai báo, và **dừng process plugin khi lượt kết thúc** — một lượt chat không để extension chạy nền.

P6 cố ý không có tool discovery: manifest chỉ khai báo capability `tools` và không nói gì về tên
tool bên trong, nên **host quyết định thứ model được thấy**. Mỗi plugin một thư mục, gồm ba file:

| File | Nội dung |
| --- | --- |
| `manifest.json` | manifest của extension, kèm digest mà nó pin cho executable |
| `trust.json` | trust grant: plugin id, đúng digest đó, capability và secret được phép |
| `installation.json` | thứ host này quảng bá: ba đường dẫn trên, và mỗi tool một mục (tên, mô tả, JSON schema tham số, timeout) |

```json
{
  "schema_version": 1,
  "plugin_id": "acme.notes",
  "manifest": "manifest.json",
  "executable": "acme-notes.exe",
  "trust": "trust.json",
  "tools": [
    {
      "name": "tool.search_notes",
      "description": "search the local note index",
      "parameters": {"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]},
      "timeout_ms": 10000
    }
  ]
}
```

Model thấy các tool đó dưới tên `plugin__<plugin>__<tool>`; tên quảng bá được làm sạch cho wire và
ánh xạ do host giữ, nên một tên chỉ resolve về installation mà người dùng đã trust. Tên built-in
luôn thắng, nên extension **không thể** mạo danh `read_file`. Mọi call đi qua đúng gate như tool
built-in — policy, rồi approval của bạn, rồi durable intent và receipt — và lượt chạy báo lại thứ
đã nạp (`extensions: 1 plugin(s), 1 tool(s) exposed`). Lượt headless báo cùng thông tin trong khoá
`extensions` của kết quả `--json`.

Installation có executable không còn khớp digest mà grant đã pin sẽ bị từ chối kèm lý do và
**không** được start; thư mục không có `installation.json` thì bị bỏ qua. Nhóm lệnh
`ha extensions inspect|capabilities|register|skills` vẫn là bề mặt kiểm tra và đăng ký, và
`register --confirm` là thứ chứng minh plugin start được.

### 12.7. Đọc cuối một lượt

Mỗi yêu cầu kết thúc bằng đúng một dòng `[run]`, và bốn chữ nó có thể dùng mang nghĩa khác nhau:

| Dòng | Điều đã xảy ra |
| --- | --- |
| `[run] done` | Model đã trả lời và không hỏi thêm gì. |
| `[run] paused: step limit reached` | Lượt dừng ở một bound bạn đặt — số step, số tool call hoặc deadline — trước khi model trả lời. **Không mất gì:** mọi receipt của tool đều bền, transcript vẫn nằm trong scrollback, và yêu cầu kế tiếp tiếp tục đúng task đó. Bound hiện ở status bar dạng `step 2/8`, và **một step là một lần gọi model**. |
| `[run] failed: <lý do>` | Có thứ hỏng: provider không tới được, tool không chuẩn bị được, hoặc chính lượt chạy lỗi. Lý do in ngay sau dấu hai chấm. |
| `[run] canceled` | Bạn đã huỷ (`Ctrl-C`). |

Card tool fail thì nói rõ vì sao: `failed 962ms · invalid_payload: optional tool path must not
be blank` là call model ghép sai (model cũng được báo y hệt và thường thử lại), còn
`failed 1.2s · policy_denied: denied by the user` là do bạn từ chối.
**Bound không phải là ngân sách, nên app tự đi tiếp qua nó.** Bound step hay tool call để chặn
một vòng lặp đã chạy sai; nó **không** có nghĩa task của bạn đã xong, và việc bạn phải tự gõ
"continue" để agent của mình chạy tiếp đọc như một sự đình trệ. Khi một lượt dừng vì bound, app
tự gửi yêu cầu kế tiếp và nói rõ trong transcript:

```text
[run] paused: step limit reached · 8 steps · 14 tool calls · 46.8s
[info] step limit reached; continuing automatically (1 of 4) — Ctrl-C stops this
[auto] continue: the previous turn stopped at a bound, not because the task was finished — …
```

`[auto]` đánh dấu những yêu cầu app gửi thay bạn, nên transcript vẫn tách được điều bạn hỏi với
điều app tự làm. `Ctrl-C` tiêu luôn phần ngân sách còn lại: sau đó bound là dừng thật cho tới khi
bạn nói tiếp. Riêng **deadline** thì không bao giờ tự tiếp — đó là thời gian thực đã trôi qua, tự
tiếp sẽ tiêu lại đúng ngần ấy thời gian.

Bốn biến môi trường chỉnh các bound, và `/status` luôn in ra giá trị đang có hiệu lực:

| Biến | Mặc định | Tác dụng |
| --- | --- | --- |
| `HA_TURN_MAX_STEPS` | 8 | Số lần gọi model trong một lượt. |
| `HA_TURN_MAX_TOOL_CALLS` | 16 | Số tool call trong một lượt. |
| `HA_TURN_DEADLINE_SECONDS` | 600 | Số giây thực tế trong một lượt. |
| `HA_TURN_CONTINUATIONS` | 4 | Số lượt app được tự tiếp sau bound step/tool call. `0` là tắt hẳn, mọi bound sẽ chờ bạn. |

Với mặc định, một yêu cầu có thể tới ba mươi hai lần gọi model (8 × (1 + 4)) rồi mới dừng hẳn —
đủ cho việc agent thật, vẫn có biên, và không bao giờ vô hạn. Ba bound chỉ nhận số nguyên dương:
`HA_TURN_MAX_STEPS=unlimited` là gõ sai nên giữ mặc định, vì một giá trị gõ sai **không được**
phép tháo mất tấm lưới an toàn. Lượt headless (`--headless --json`) vẫn đúng **một** lượt và
không tự tiếp: nó báo `"stop":"step_limit"`, script muốn thêm thì gọi `--resume`.


### 12.8. Ảnh trong một yêu cầu

`deepseek-flash` nhận ảnh, nên ảnh chụp màn hình có thể là một phần của yêu cầu thay vì một
đường dẫn mà model sẽ thử mở bằng tool đọc text. Ba đường vào:

| Cách | Bạn làm gì |
| --- | --- |
| Ảnh đã có trên đĩa | Ghi tên nó trong tin nhắn: `chỗ này sai gì? "C:\Users\me\shot.png"`. Kéo file từ Explorer cũng ra đúng đường dẫn đó. **Nhớ quote nếu đường dẫn có dấu cách.** Đường dẫn tương đối được hiểu theo thư mục project. |
| Link tới một ảnh | Dán hoặc kéo chính link đó: `chỗ này sai gì? https://cdn.example.com/shots/broken.png`. Ở đây **không** tải gì cả — link đi thẳng vào request và **provider** tự tải. Vì vậy link phải truy cập được từ internet, tối đa 8192 ký tự, ảnh tối đa 32 MiB. Nếu link là riêng tư (localhost, host nội bộ, URL gắn session) thì provider không đọc được và lượt đó fail kèm lỗi tải của nó: hãy copy ảnh vào clipboard rồi dùng `/image`. Chỉ link có đường dẫn kết thúc bằng `.png`, `.jpg`, `.jpeg`, `.gif` hoặc `.webp` mới được gắn; một link bình thường trong câu vẫn chỉ là text. |
| Ảnh chụp đang trong clipboard | `/image`. `Ctrl-V` làm y hệt ở những terminal có chuyển phím cho app — Windows Terminal giữ phím đó cho paste của nó, nên `/image` là cách luôn chạy. |

Cả ba đường đều gắn ảnh vào đúng lượt đó; trường hợp clipboard thì file PNG được ghi vào thư
mục dữ liệu và đường dẫn (đã quote) được chèn vào ô soạn thảo, còn transcript nói rõ đã xảy ra
gì (`[info] image attached: shot.png (image/png, 84 KiB)`, hoặc
`broken.png (image url, downloaded by the model)` với link). Ứng viên không gắn được sẽ được báo
kèm lý do chứ không bị bỏ im lặng: file không thật sự là ảnh, file lớn hơn 8 MiB, quá ba ảnh
trong một tin nhắn, hoặc đường dẫn trỏ vào nơi chứa credential (`.ssh/`, `*.pem`, `.env`,
`credentials*`) — những chỗ đó **không bao giờ** được gửi tới provider.

Format do **nội dung file** quyết định chứ không theo đuôi, nên file `.png` mà thật ra là text sẽ
bị từ chối; PNG, JPEG, GIF và WebP là những định dạng chạy được. Tin nhắn liệt kê ảnh theo thứ
tự, nên model có thể nói "ảnh chụp thứ hai". Ảnh đi theo dạng block của `content`, mà API chỉ
nhận trong message **user**. `read_file` vẫn từ chối file binary: ảnh tới model dưới dạng
attachment, không bao giờ là text của file.

Đã kiểm chứng bằng fixture SSE local chứ không chỉ đọc code: một lượt headless ghi tên file PNG
165 byte (`ha chat --headless --prompt "xem <file.png>" --json`) gửi message `user` với `content`
là mảng gồm block text nêu tên ảnh rồi tới block `image_url`, và base64 trong block đó giải mã ra
đúng byte của file trên đĩa (165 byte, magic PNG còn nguyên). Cùng lượt đó báo
`"images":["shot.png (image/png, 165 B)"]` — kích thước hiển thị theo byte chính xác, vì `0 KiB`
bên cạnh một ảnh đã gắn đọc như thể gắn lỗi — và lượt kế tiếp trong cùng project vẫn recall được
lượt trước, tức là gắn ảnh không làm hỏng đường memory. Lượt thứ hai ghi tên
`https://cdn.example.com/shots/broken.png` đã gửi đúng link đó làm giá trị `image_url`, nên đường
link được chứng minh ở tầng wire chứ không chỉ trong unit test.
