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

- **Bảo trì không có daemon.** Mọi thao tác bảo trì dưới đây là lệnh chạy nền
  trước do người vận hành gọi; không gì tự thử lại hay tự thu gom. Còn phiên
  tương tác thì chạy trong worker nền ([12.7](#127-agent-chạy-nền)).
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

## 6. Tombstone

```console
ha maintenance tombstones --data-dir <DATA_DIR> --json
```

`tombstone` là bản ghi bền vững rằng một nguồn đã bị cố ý quên, kèm mọi bản sao bên
ngoài hoặc bản sao lưu có thể còn chứa dữ liệu của nó. Các thao tác retention theo
nguồn `invalidate`/`archive`/`forget` không thuộc command surface hiện tại, nên bản này
không ghi tombstone mới; record mà store cũ đang giữ vẫn tồn tại nguyên vẹn qua sao lưu
và phục hồi, và `ha maintenance tombstones` liệt kê chúng.

## 7. Thu gom rác

```console
ha maintenance gc --data-dir <DATA_DIR> --grace-seconds 604800 --dry-run --json
ha maintenance gc --data-dir <DATA_DIR> --grace-seconds 604800 --json
```

Thu gom rác xóa byte của artifact, và chỉ xóa artifact đồng thời:

1. **không được tham chiếu** — không receipt hay tool artifact scope nào trỏ tới;
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
- **verified_cases**, **unverified_checks**, **out_of_scope** — 32 ca liên tục và
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
- Tombstone là cục bộ và bền vững, không phải toàn cầu. Hãy đọc `surviving_copies` trước
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

- **Bảo trì chạy nền trước.** Không daemon nào chạy nó: backup, thu gom hay thao tác
  retention xảy ra đúng lúc bạn gọi và không lúc nào khác. (Phiên tương tác thì vẫn
  chạy trong worker nền sau khi terminal đóng; xem [12.7](#127-agent-chạy-nền).)
- **Thư mục mới là đích hợp lệ.** `ha maintenance doctor --data-dir <DIR>` chạy được
  trên thư mục chưa có store và báo nó là chưa khởi tạo — đúng trạng thái mà binary
  này được phép tạo store trong đó. Nó không giả vờ rằng store đã tồn tại, và việc
  sao lưu một thư mục như vậy vẫn bị từ chối.

Build từ source vẫn dùng được cho phát triển:
`cargo build -p harness-cli --bin ha --locked` ghi ra `target/debug/ha`, và
`cargo run -p harness-cli --bin ha -- <args>` chạy nó mà không cài gì.


## 12. Sử dụng `ha` tương tác và headless

`ha` hoặc `ha chat` mở chat ở terminal. `ha chat --cwd <project>` chọn workspace; khi stdin/stdout không phải terminal, lệnh tương tác thoát mã 2 thay vì chờ. `ha chat --plain` dùng giao diện dòng; `ha chat --fixture` dùng fixture cục bộ. `/session` (hoặc `/status`) cho biết project, provider, dữ liệu và trạng thái thiết lập. `/login` theo prime-agent: DeepSeek, OpenAI, Anthropic, OpenCode Zen và OpenCode Go nhận API key qua ô nhập có che ký tự; ChatGPT Plus/Pro đăng nhập bằng trình duyệt (OAuth với PKCE trên `localhost:1455`; nếu trình duyệt ở máy khác, dán URL chuyển hướng cuối cùng vào). Credential được lưu theo từng provider trong `auth.json` ở thư mục dữ liệu riêng và được ưu tiên hơn biến môi trường của provider (`DEEPSEEK_API_KEY`, `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `OPENCODE_API_KEY`); `/logout <provider>` xóa một mục. Giá trị không bao giờ xuất hiện trong status hay transcript. `/model` liệt kê model của các provider đã đăng nhập, lấy từ model catalog của prime-agent (bản chụp đi kèm app, làm mới mỗi ngày) cùng danh sách model mà DeepSeek và OpenCode tự công bố, nên model mới hơn catalog cũng có mặt; các mức thinking của từng model lấy từ catalog, và `/effort` chỉ gợi ý các mức đó; lựa chọn được lưu trong `selection.json` cạnh config người dùng. Không có đăng nhập Claude Pro/Max của Anthropic: cách đó chỉ chạy khi app tự nhận là Claude Code, nên Anthropic dùng API key.

### 12.1. Công cụ của agent

18 công cụ cốt lõi đi qua host, policy và receipt. Các tool skill (`list_skills`, `activate_skill`, `read_skill_file`), tool web (`web_search`, `web_fetch`), Python REPL (`ipython`) và, khi không có REPL, `mcp__<server>__<tool>` được thêm khi khả dụng.

**MCP theo cách của prime-agent.** Khi có Python REPL, MCP server không còn là tool native: object `mcp` được import sẵn trong kernel (`rlm.mcp` của prime-agent, đã vendor) tự mở server đã cấu hình - `await mcp.list_tools("<server>")`, `await mcp.call_tool("<server>", "<tool>", arguments)`, `await mcp.list_connections()` - và prompt liệt kê các server đang bật. Server là những server `ha mcp add` đã cấu hình; host đưa cấu hình từng server cho kernel theo định dạng của prime-agent (`secret://NAME` thành tham chiếu `{"env": "NAME"}`; `streamable_http` thành `http` kèm `bearerTokenEnvVar`). Package Python `mcp` có sẵn trong kernel venv. Catalog dịch vụ của prime-agent chưa được port (danh sách plugin rỗng); đăng nhập OAuth thì đã port, là `ha mcp login` ([12.8](#128-thêm-từ-prime-agent)). Khi không có REPL, hoặc khi tin nhắn đính kèm resource của server bằng `@server:uri`, server được kết nối native như trước; `/mcp` cho biết mỗi server đi theo đường nào.

**Web.** `web_search` tìm kiếm trên web: dùng Google qua Serper khi có `SERPER_API_KEY` hoặc key đã lưu bằng `/login serper` (key miễn phí tại serper.dev), nếu không thì dùng DuckDuckGo, không cần key. `web_fetch` mở một trang http(s) và trả về văn bản đọc được kèm danh sách link, chia theo từng đoạn và đọc tiếp bằng `start_index`. Địa chỉ local và mạng nội bộ bị từ chối, kể cả khi bị redirect tới; file nhị phân bị từ chối. `web_search` chạy không cần bảng phê duyệt (chỉ gửi câu truy vấn); `web_fetch` phải hỏi, vì URL có thể mang dữ liệu ra ngoài - trả lời `a` để cho phép cả lượt, hoặc dùng `full-auto`. `HA_WEB=off` gỡ cả hai tool.

**Python REPL.** Tool `ipython` chạy cell trong một kernel Python bền - chính runtime của prime-agent, được vendor tại `crates/harness-cli/python` và ghi ra `<data-dir>/runtime` ở lần dùng đầu. Dùng được `await` ở top-level; biến và import được giữ qua các cell và các lượt; kernel chết thì được khởi động lại ở lần gọi sau và kết quả báo rằng state cũ đã mất. `bash('cmd')` chạy lệnh nền và trả về handle (`tail`, `output`, `poll`, `kill`, `await`); lệnh chạy nền mà xong khi không ai đọc kết quả sẽ được báo bằng `[bash-done pid:... exit:...]` của prime-agent - đưa vào lượt đang chạy, hoặc thành một lượt riêng khi rảnh; trên Windows lệnh chạy trong Git Bash, tìm ở vị trí cài mặc định hoặc đặt bằng `HA_REPL_SHELL`. Một cell chạy tối đa 10 phút rồi bị ngắt; trên Windows chỉ ngắt được cell đang chờ ở `await`, cell kẹt trong code đồng bộ sẽ khiến kernel khởi động lại. Chạy Python là chạy code, nên mỗi cell hỏi phê duyệt như `run_shell`, trừ chế độ `full-auto`. Kernel dùng project root làm thư mục làm việc và dùng môi trường của người dùng. Cần Python 3.11+ (`HA_PYTHON`, nếu không thì `python3`, `python`, `py -3`); không có thì tool không được đưa ra. `HA_REPL=off` gỡ tool.

Khi có delegation, object `rlm` trong kernel chạy agent con như prime-agent: `await rlm.spawn('task', name='worker')` khởi động một agent con và trả về ngay khi được nhận; `await rlm.collect([...], timeout_ms=...)` chờ câu trả lời; `rlm.list_subagents()`, `rlm.delete_subagent(...)` và `rlm.find_models()` hoạt động như ở prime-agent. Agent con thuộc session chứ không thuộc lượt đã khởi động nó: lượt có thể kết thúc trong khi agent con vẫn làm việc. Khi agent con xong mà không ai đang chờ kết quả, agent cha được báo bằng thông báo của prime-agent - `[child-failed child:<tên>]`, `[child-exited: cancelled child:<tên>]` hoặc `[child-exited: no-reply child:<tên>]` kèm câu trả lời cuối - trong một lượt riêng nếu cha đang rảnh, hoặc sau khi lượt đang chạy kết thúc (không chen vào lượt đang chạy). Ctrl-C dừng lượt của cha, không dừng agent con; `/agents stop <tên>` (hoặc `/agents stop` cho tất cả) dừng chúng, còn `/new` hay resume sang hội thoại khác dừng chúng mà không báo. Agent con chạy trên model của cha trừ khi `rlm.spawn(..., model='provider/id')` chỉ một model khác trong catalog có credential, nếu không thì `subagentDefaultModel` của prime-agent trong `settings.json`, rồi đến `[agents] default_model`; model không dùng được thì spawn thất bại. `/subagent-model` cho xem model đó và nguồn của nó; `/subagent-model <provider/model>` (Tab hoặc Enter mở danh sách model) lưu `subagentDefaultModel` cho child tiếp theo, còn `/subagent-model inherit` xoá nó. `/subagent-login <provider>` đăng nhập cho các agent con bằng tài khoản riêng của chúng - API key, hoặc đăng nhập ChatGPT qua trình duyệt - lưu trong `subagent-auth.json` cạnh `auth.json`: agent con dùng tài khoản riêng cho provider đó nếu có, không thì dùng tài khoản của model chính, còn model chính không bao giờ dùng tài khoản của agent con. `/subagent-logout <provider>` xoá tài khoản riêng đó. Agent con chạy ở mức thinking của cha trừ khi `rlm.spawn(..., thinking='high')` chỉ định - như prime-agent, mức mà model của agent con không có thì spawn thất bại - hoặc `/subagent-effort <level>` đã lưu `subagentDefaultThinking` trong `settings.json` (kẹp về mức gần nhất model của agent con có; `/subagent-effort inherit` xoá nó). Mọi tin nhắn các agent trong phiên gửi cho nhau (`agent_message`, nhắn tin qua `rlm`) hiện trong hội thoại theo dòng của prime-agent, `◆ agent message · <người gửi> → <người nhận> · <đoạn đầu>`, toàn văn hiện ở chế độ chi tiết và mở rộng (Ctrl-O); `/agents messages` liệt kê mọi lượt trao đổi của phiên, đầy đủ và theo thứ tự. `rlm.create_session` khởi động một agent top-level riêng ([12.7](#127-agent-chạy-nền)).

**Python skill.** Các skill của prime-agent đi kèm ứng dụng (`.agents/skills`, MIT; xem `.agents/PRIME-AGENT-SOURCE.md`): `edit`, `websearch`, `attach_image`, `goal`, `compact`, `refine`, `agent_message`, `agent_observe`, `rlm_heartbeat`, cùng các hướng dẫn `mcp` và `skill-creator`. Skill có package Python được import vào kernel theo tên khi kernel khởi động - `await edit(path=..., old_str=..., new_str=...)`, `await goal.complete()`, `await compact.run()` - và được liệt kê trong prompt kèm `python_import`. Skill import lỗi được thay bằng một stub nói rõ lý do, và cell đầu tiên báo điều đó. Host trả lời các yêu cầu của skill: `goal.*` điều khiển `/goal` (mục tiêu model tạo được xử lý như mục tiêu bạn đặt, kèm token budget của nó), `compact.run` hẹn `/compact` chạy khi lượt kết thúc, `model.info` cho biết model, `agent_observe` đọc các agent con của session, `rlm_heartbeat` giữ các lời nhắc lặp lại cho session (mặc định `every 5m`; loại `steer` chen vào lượt đang chạy qua hộp `/steer`, loại `follow_up` đợi lượt kết thúc), và ảnh mà `attach_image` nạp được gửi cho model cùng kết quả tool. `agent_message.send(message, receiver_role='child', receiver_name=...)` (hoặc `'all'`) nhắn cho agent con đang chạy; agent con nhắn cho cha hoặc anh em bằng tool `agent_message` của nó, và để lại một dòng `progress_note` mà cha đọc trong `list_subagents` và `/agents`. Như prime-agent, một tin tối đa 16 384 ký tự, mỗi người gửi được ba tin liền rồi thêm một tin mỗi giây, và tin tới dưới dạng `[agent-message from <quan hệ>:<tên>]` ở bước kế tiếp của người nhận - hoặc, với cha đang rảnh, thành một lượt riêng. Tin gửi cho agent con đã xong bị từ chối (agent con đã xong không được đánh thức lại).

**Kernel venv.** Giống prime-agent, kernel chạy trong một venv ở `<data-dir>/kernel-venv`, dựng bằng `uv` ở lần dùng đầu - Python 3.11, `dill`, bộ package mặc định của prime-agent (requests, httpx, pyyaml, tomli, python-dotenv, pandas, numpy, scipy, beautifulsoup4, lxml, pydantic, tyro) và `pillow` - và dựng lại khi danh sách đó đổi. ha không bao giờ tự cài `uv`: không có nó thì kernel chạy trên Python hệ thống, cell đầu tiên báo điều đó, và skill thiếu package (`websearch` cần httpx, `attach_image` cần pillow) được báo là không khả dụng. `HA_PYTHON` chỉ định một trình thông dịch dùng nguyên như vậy.

**Skill** theo cấu trúc Agent Skills (một thư mục có `SKILL.md` cùng các file đi kèm). Skill được tìm trong bộ tích hợp sẵn, `<config-dir>/skills`, `~/.agents/skills`, mọi thư mục liệt kê trong `HA_SKILL_PATHS` (phân tách như `PATH`, ví dụ `~/.claude/skills`), và - với project đã trust - `.harness/skills` cùng `.agents/skills` ở workspace và từng thư mục cha tới Git root. System prompt liệt kê tên, mô tả và vị trí từng skill trong khối `<available_skills>`, giống prime-agent; model kích hoạt skill theo tên (digest từ `list_skills` là tùy chọn để ghim phiên bản, chấp nhận có hoặc không có `sha256:`). Khi kích hoạt, model nhận hướng dẫn cùng thư mục và danh sách file của skill, và `read_skill_file` đọc các file đó - chỉ trong phạm vi skill. Ba tool skill chỉ đọc catalogue đã trust nên chạy không cần bảng phê duyệt; deny rule vẫn chặn được. `disable-model-invocation: true` trong front matter ẩn skill khỏi model; `/skill:<name>` vẫn chạy được. Đặt ở đầu tin nhắn, `/skill:<name> [args]` gửi skill kèm phần còn lại làm yêu cầu, như prime-agent; đặt giữa tin nhắn (`... dùng /skill:<name>`), nó gửi skill đó kèm toàn bộ tin nhắn làm yêu cầu.

**Skill tự học (theo tigerless-labs/autoharness).** Ngoài prompt note, memory, Python skill và subagent spec, một lần refine có thể đề xuất `skillFiles`: `create`, `update`, `patch` (một `old_string` xuất hiện đúng một lần), `remove_file` hoặc `delete` một skill `SKILL.md` do ha tự viết, mỗi đề xuất kèm `reason` và trích dẫn nguyên văn `evidence`. Model không bao giờ tự ghi các file đó: host lint từng đề xuất trong bộ nhớ và chỉ ghi đề xuất sạch, file phụ trước, `SKILL.md` sau cùng, mỗi file qua file tạm rồi đổi tên. Lint từ chối: front matter có `name` khác tên skill, mô tả quá 200 ký tự, thân quá 30 dòng không trống, file phụ nằm ngoài `references/`, `templates/`, `scripts/`, `assets/` hoặc không được `SKILL.md` nhắc tới, file được nhắc tới mà không tồn tại, `TODO`/`FIXME`/`TBD`, secret, nội dung nguy hại rõ ràng (tải về rồi pipe vào shell, "ignore previous instructions", lệnh phá hủy hay tự cài thường trú), skill global nhắc tới đường dẫn của workspace, tên trùng skill khác, và mọi thay đổi vào skill không do ha viết. `delete` có `absorbed_into` phải trỏ tới một skill tự học khác đang sống. Đề xuất bị từ chối được liệt kê cùng lý do trong dòng kết quả refine. Skill project nằm ở `.harness/skills/<name>` (chỉ với project đã trust), skill global ở `<config-dir>/skills/<name>`; skill do ha viết có `.ha-learned.json` và sổ `.ledger.jsonl` chỉ ghi thêm (lý do và bằng chứng đã che secret cho mỗi thay đổi). `/refine --rollback <id>` hoàn tác cả các thay đổi skill. Lượt dùng được đếm trong `<data-dir>/skill-usage`: activate hoặc `/skill:<name>` là một lần dùng, `read_skill_file` hay `read_file` trong thư mục skill là một lần xem. Skill mới thử việc trong 100 request của project (300 với skill global); hết thử việc mà chưa được dùng hay xem lần nào thì bị archive, và khi quá 20 skill global hoặc 50 skill project đã qua thử việc thì skill ít dùng nhất bị archive trước. Archive là chuyển thư mục vào `.archive/` của layer đó; chuyển lại là khôi phục. `HA_SKILL_LIFECYCLE=off` giữ lại tất cả. Lượt tự xem xét đến hạn sau 25 lượt hoặc 50 lần gọi tool, tùy cái nào tới trước, và chạy song song với hội thoại thay vì ở cuối lượt. Session kết thúc khi còn ít nhất 5 lượt hoặc 10 lần gọi tool chưa được xem xét sẽ không bắt bạn chờ lúc thoát: nó để lại yêu cầu xem xét trong thư mục dữ liệu của project (`refine-pending/`), và lượt kế tiếp trong project sẽ chạy nền phần xem xét đó trên hội thoại của session cũ (`auto-refine: reviewing the last N turn(s) of an earlier session`). Refine đọc hội thoại theo cách prime-agent serialize: câu hỏi của mỗi lượt, lời của model, các lần gọi tool đánh số `#N` kèm tham số, và kết quả của từng lần gọi (2.000 ký tự đầu và 500 ký tự cuối), nên bài học đến cả từ những gì tool đã làm chứ không chỉ từ lời nói. Trong một Git worktree phụ, skill project được ghi vào và đọc từ `.harness/skills` của checkout chính, nên những gì học được ở đó không mất khi xóa worktree và mọi worktree dùng chung một thư viện. Cứ mỗi 250 request trong project (hoặc `/refine --curate [chỉ dẫn]`), curator đọc toàn bộ thư viện skill tự học và gộp các skill hẹp thành skill tổng: nó patch hoặc tạo một skill tổng rồi xóa các skill đã được gộp với `absorbed_into`, qua cùng bộ kiểm tra. Thư viện được sao lưu vào `<data-dir>/skill-snapshots/<thời điểm>/` trước (giữ 5 bản mới nhất), và lượt gộp được ghi thành một refinement global mà `/refine --rollback <id>` hoàn tác được.

| Nhóm | Tool | Mục đích |
| --- | --- | --- |
| Workspace | `read_file`, `list_files`, `search_text`, `glob` | Đọc và tìm kiếm có giới hạn trong workspace |
| Sửa file | `apply_patch`, `write_file`, `edit_file` | Ghi với hash/điều kiện match và phê duyệt; `write_file` tự tạo thư mục còn thiếu bên trong workspace |
| Process | `run_process`, `run_shell`, `read_process_output` | Chạy lệnh và đọc output artifact có giới hạn |
| Git | `git_status`, `git_diff`, `git_log` | Quan sát repository |
| Task | `task_update`, `delegate` | Ghi next action; giao explorer/coder |
| Lịch sử | `history_search`, `history_read` | Tìm và đọc journal của task |
| Người dùng | `ask_user` | Tạm dừng và hỏi, không cấp quyền tool. Các lựa chọn là một menu (mũi tên và Enter, hoặc số của lựa chọn khi ô nhập trống); gõ chữ thì trả lời bằng ô nhập. Câu hỏi của agent con hiện kèm tên agent đó và agent chờ câu trả lời; câu hỏi và yêu cầu duyệt quyền từ nhiều agent mở lần lượt từng cái |

Như prime-agent, child lồng nhau tới `RLM_MAX_DEPTH`, mặc định 2: child của agent gốc có tool `delegate` riêng để tạo explorer (coder chỉ do agent gốc tạo), và một child chỉ xong khi các child của nó đã xong - thông báo của chúng mở thêm một lượt cho child đó rồi nó mới báo lên cha. Child nhắn được cho cha, anh em và child của chính nó (`agent_message` với `receiver_role` `parent`, `sibling`, `child` hoặc `all`); cháu không nhắn thẳng lên agent gốc. `/rlm-max-depth` cho xem giá trị và nguồn (chat, global, env hoặc default); `/rlm-max-depth <n>` đặt cho hội thoại này, thêm `--global` thì lưu `rlmMaxDepth` vào `settings.json` cạnh config người dùng; sau đó mới đến `RLM_MAX_DEPTH`. ha chạy tối đa 2 tầng (giới hạn của hợp đồng delegation); child giữ giá trị lúc nó được tạo. Mỗi tầng chạy tối đa ba child cùng lúc. Như child của prime-agent, child có đủ tool và quyền của cha - permission mode, luật allow/deny và những gì người dùng đã cho phép trong lượt: explorer (và child của `rlm.spawn`) làm việc ngay trong workspace của cha, coder làm việc trên worktree Git sạch do host cấp. Nếu không thể cấp worktree, tool trả `role_unavailable`; `/agents` liệt kê mọi child kèm role, model, trạng thái, thời gian, số tool call, chi phí và ghi chú mới nhất (`/agents messages`: các agent đã nói gì với nhau); `/agents stop [tên]` dừng chúng (Ctrl-C thì không). `delegate` mặc định chờ câu trả lời của child; với `wait: false` nó trả về ngay và kết quả tới sau dưới dạng thông báo. Nhiều lời gọi `delegate` trong một phản hồi chạy song song. Child thừa hưởng tool web của cha (`web_search`, `web_fetch`). Child gặp sự cố - lỗi provider, vòng lặp, phản hồi rỗng - được trả về cho cha dưới dạng kết quả thất bại mà cha có thể xử lý, `[child-failed explorer] <code>: <lý do>`, không bao giờ báo là đã xong. Mọi lượt, của cha hay của child, dừng với `loop_detected` khi đọc cùng một thứ (cùng tool, cùng tham số) ba lần mà ở giữa không có gì thay đổi, kể cả khi các lần lặp bị xen bởi lời gọi khác.

### 12.2. Lệnh trong chat và bàn phím

| Nhóm | Lệnh |
| --- | --- |
| Trợ giúp và trạng thái | `/help`, `/hotkeys`, `/fullscreen`, `/session` (`/status`), `/config`, `/model [search]`, `/effort [level]` (`/thinking`), `/cost`, `/context` (`/usage`), `/system-prompt`, `/permissions`, `/hooks`, `/mcp`, `/agents`, `/subagent-model`, `/subagent-effort`, `/subagent-login`, `/subagent-logout`, `/rlm-max-depth`, `/skills`, `/logs` (nơi ghi log: mọi file `*.log` trong thư mục data) |
| Phiên và câu trả lời | `/new` (`/clear` xóa cả viewport), `/resume [id]`, `/name [name]` (`/rename`), `/more`, `/compact [instructions]`, `/export [path]` (mặc định là trang HTML; `.md`, `.html` cho một trang tự chứa, hoặc `.jsonl` cho file phiên của ha - dạng JSONL của prime-agent, một dòng header rồi mỗi dòng một message - chứa những gì model được gửi cho hội thoại này, đã che secret), `/import <path.jsonl>` (hội thoại mới tiếp nối hội thoại đã export, hoặc file phiên của prime-agent; file được chép vào `<data>/imports`), `/share`, `/new [prompt]`, `/heartbeat <chỉ dẫn>`, `/heartbeats`, `/copy`, `/fork [n]`, `/clone`, `/tree [n]`, `/btw <câu hỏi>` (`/side`), `/quit` (`/exit`) |
| Workspace và điều khiển | `/undo`, `/trust [yes]`, `/init`, `/permissions [ask|auto-edit|full-auto]` (`/permission`, `/mode`), `/steer <text>`, `/queue [text|list|edit n text|drop n|up n|down n]` (`/followup`), `/stash`, `/goal <objective>|status|pause|resume|clear`, `/autonomous [status|off|on ...]`, `/schedule [list|add <khi nào> -- <prompt>|pause|resume|cancel <id>]` |

**Hàng đợi, cất nháp, câu hỏi bên lề và rẽ nhánh (như prime-agent).** Enter khi agent đang làm sẽ chen vào lượt đang chạy; tin nào lượt chưa nhận được thì chờ ở lane steer. `/queue <text>` thêm một follow-up chạy thành lượt riêng sau lượt đang chạy; `/queue` liệt kê hàng đợi và `/queue edit|drop|up|down <n>` sửa nó. Những gì đang chờ được gửi sau lượt - steer trước, follow-up sau - mỗi lượt một tin, hoặc cả lane một lần với `[queue] steering_mode = "all"` / `follow_up_mode = "all"`. Trong một lượt đang chạy, steer cũng theo chế độ này như vòng lặp của prime-agent: mỗi lần gọi model nhận một tin, đặt sau kết quả tool của bước mà tin đó đến. Sau Ctrl-C, hàng đợi (và báo cáo của agent con) chờ lượt tiếp theo của bạn. Ctrl-S (hoặc `/stash`) cất bản nháp và, trên dòng trống, lấy lại nó. `/btw <câu hỏi>` hỏi model về cuộc hội thoại, không dùng tool và không thêm gì vào hội thoại; câu trả lời mở trong một bảng, `/btw` tiếp theo hỏi tiếp. `/fork` liệt kê các tin bạn đã gửi và `/fork <n>` mở một hội thoại mới ngay trước tin n, đưa tin đó lại vào ô soạn; `/clone` mở hội thoại mới với toàn bộ lịch sử; cả hai hiện riêng trong `/resume`. `/tree` liệt kê các lượt của hội thoại và `/tree <n>` tiếp tục sau lượt n; `/tree <n> --summarize [trọng tâm]` còn cho model tóm tắt các lượt bị bỏ lại bằng prompt tóm tắt nhánh của prime-agent (dùng model phụ nếu có), và tin nhắn kế tiếp được đọc kèm `[branch-summary]` đó ở trước. `/tree label <n> [text]` gắn nhãn cho một lượt, hiện dạng `[nhãn] ` trong danh sách; để trống thì xoá nhãn.

**Model tự thêm, đổi nhanh trong phạm vi và định tuyến model (như prime-agent).** File `models.json` cạnh config người dùng (`<HA_HOME>/models.json`) thêm model theo schema của prime-agent - `{"providers": {"<id>": {"baseUrl", "api", "apiKey", "models": [{"id", "name", "reasoning", "input", "cost", "contextWindow", "maxTokens"}], "modelOverrides": {"<model id>": {...}}}}}`, cho phép chú thích `//`. `api` là `openai-completions`, `anthropic-messages`, `openai-responses` hoặc `openai-codex-responses`; provider mới cần `baseUrl` và `apiKey`; trường thiếu lấy mặc định của prime-agent (`contextWindow` 128000, `maxTokens` 16384, input chữ). `apiKey` là **tên biến môi trường** chứa key, không bao giờ là key. Model trùng provider và id có sẵn sẽ thay nó, còn `modelOverrides` chỉ đổi các trường được ghi. File không dùng được sẽ được báo một lần lúc khởi động (`models.json: <lỗi> - using built-in models only`). Model hiện trong `/model` khi đã có key.

`[routing]` trong config: `scoped = ["deepseek/*", "openai/gpt-5*:high"]` (glob trên `provider/id` hoặc `id`, không phân biệt hoa thường, hậu tố `:level` tuỳ chọn) là các model mà `/model next`, `/model prev`, Alt+M và Shift+Alt+M đi qua - những model có credential, cần ít nhất hai. `/scoped-models <pattern>...` lưu phạm vi cho lần này và các lần sau (đứng trên config, như `/model`), `/scoped-models` hiện nó, `/scoped-models clear` bỏ nó. `auxiliary = "provider/id"` viết tóm tắt khi compact và đánh giá của `/refine` (không làm được thì model phiên làm, có thông báo). `backup = "provider/id"` nhận lượt khi model phiên lỗi giới hạn tần suất hoặc dịch vụ không sẵn sàng ở mọi lần thử lại (`Primary model unavailable (...) — retrying on backup model ...`); lượt sau quay về model phiên (`Primary provider recovered — back on ...`). `image = "provider/id"` trả lời request có ảnh khi catalog ghi model phiên chỉ nhận chữ; không có thì request đó lỗi `This model does not accept images; set [routing] image in config` thay vì gửi ảnh cho model chữ. Provider bị giới hạn (HTTP 429, hoặc mã quota có cấu trúc như `insufficient_quota`; mã lỗi `rate_limited`) được chờ như prime-agent - 1 giây, gấp đôi tới 5 phút mỗi lần kiểm tra, tối đa 30 lần và 15 phút, ưu tiên `Retry-After` của server - với dòng `Waiting for provider usage to recover (n/30), next check in Ns... (esc to cancel)` ở thanh trạng thái; Esc để huỷ. `wait_for_usage = false` tắt việc chờ. Khi provider báo thời điểm reset hạn mức vượt giới hạn đó - qua `Retry-After`, hoặc qua khoảng thời gian ghi trong lỗi như "Try again in ~7272 min" - phiên được park như prime-agent: lượt kết thúc với `Session parked until <thời điểm> and will resume automatically`, và một job chạy một lần lưu theo hội thoại (`/schedule` liệt kê, `/schedule cancel <id>` huỷ) gửi prompt tiếp tục của prime-agent 30 giây sau thời điểm reset, chậm nhất một ngày. Job vẫn còn sau khi thoát `ha`: nó chạy khi hội thoại được mở lại. `/tier` cho xem service tier đang dùng và các tier model nhận; `/tier default|flex|priority|auto` đặt tier, `/fast` bật/tắt `priority`, như prime-agent. Tier được gửi trong trường `service_tier` của request tới OpenAI (API key: `auto` cho mọi model; `priority` và `flex` cho gpt-5.4, gpt-5.5, gpt-5.6, gpt-5.6-* và gpt-6-astra) và tới đăng nhập ChatGPT (như trên, không có `flex`); provider khác chỉ nhận default, và tier model không nhận được gửi thành `default`. Tier được giữ theo hội thoại và lưu thành `defaultServiceTier` trong `settings.json` cạnh config người dùng cho hội thoại mới; thanh trạng thái hiện `fast` cho priority và tên tier cho tier khác.

**Chế độ tự chủ và lịch chạy (như prime-agent).** `/autonomous on` giữ phiên tiếp tục làm sau khi model dừng: mỗi lượt kết thúc bình thường được nối bằng một lượt khác với câu nhắc tiếp tục của prime-agent, tới khi hết ngân sách - mặc định 3 lần tiếp tục, 12 lượt, 80.000 token và 30 phút, xét theo đúng thứ tự đó (`--max-continuations`, `--max-turns`, `--max-tokens`, `--timeout-ms`; nêu một cái thì các cái không nêu thành không giới hạn; nhận `unlimited`). `--gate "<lệnh>"` (lặp được) thêm cổng kiểm tra: sau mỗi lượt các cổng chạy trong workspace, cùng shell và môi trường đã lọc như shell tool của model (mỗi cổng 5 phút, `--gate-timeout-ms`); tất cả qua thì dừng (`autonomous: quality gates passed`), một cổng hỏng thì tiếp tục kèm mã thoát và output (`[autonomous-continuation: gate-failed] ...`), hỏng quá `--gate-retries` (3) lần thì dừng. Cổng đã hỏng không chạy lại khi git worktree chưa đổi; vẫn tính là một lần thử. Khi agent con đang chạy, phiên chờ báo cáo của chúng. `/autonomous` hiện `[autonomous-status: ...]`; `/autonomous off` tắt (và dừng cổng đang chạy). Goal và chế độ tự chủ không chạy cùng lúc: bật cái này thì cái kia tắt, có thông báo. `/schedule add <khi nào> -- <prompt>` hẹn một prompt cho hội thoại này: `in 10m` / `in 2h` / `in 1d` (một lần), `every 30s` / `every 1h` (tối thiểu 10 giây), `at 2030-01-01T09:00:00`, cron 5 trường theo giờ máy (`0 9 * * 1-5`) hoặc `@hourly|@daily|@weekly|@monthly`; `--steer` đưa nó vào lượt đang chạy thay vì chờ lượt xong. Job lưu ở `<data dir>/schedules/<task>.json` và quay lại khi bạn resume hội thoại; chúng chỉ chạy khi `ha` đang mở, các lần lỡ lúc đóng app chạy bù một lần, lần sau tính từ lúc đó. `/schedule` liệt kê, `/schedule pause|resume|cancel <id>` đổi một job. Job tới giờ đến dưới dạng `[heartbeat: <khi nào> run#N]` rồi tới prompt.
| Nội dung và mở rộng | `/login`, `/logout`, `/image`, `/attach <path>`, `/skill:<name> [args]`, `/reload`; các template trong `.harness/commands` hoặc `<config-dir>/commands` chạy bằng `/name` |

Bảng lệnh, thứ tự, mô tả và alias theo `slash-commands.ts` của prime-agent, menu cũng vậy: gõ `/` liệt kê mọi lệnh, gõ thêm chữ sẽ lọc bằng tìm mờ của prime-agent (`/skil` gợi ý `/skills` và mọi `/skill:<name>`; prompt command cũng có mặt), và lệnh nhận tham số sẽ mở menu tham số khi Tab hoặc Enter - `/effort` gợi ý các mức, `/model` các model, `/login` các provider - Enter trên một giá trị là áp dụng luôn. Gõ sai lệnh sẽ được gợi ý lệnh gần nhất. `/effort <level>` (alias `/thinking`) chọn mức suy luận của model, từ `off` qua `minimal`, `low`, `medium`, `high`, `xhigh` tới `max`, giống prime-agent; `/effort` không kèm tham số cho xem mức đang dùng và các mức model hỗ trợ. Mức model không có sẽ được kẹp về mức gần nhất (DeepSeek V4 có `off`, `high` và `xhigh`, gửi đi là `max`). Lựa chọn được lưu cùng hội thoại và, như `setThinkingLevel` của prime-agent, lưu thành `defaultThinkingLevel` trong `settings.json` cho hội thoại mới (trừ `off` trên model không suy luận); sau đó mới đến `provider.thinking` / `HA_PROVIDER_THINKING`. Khi bật thinking, DeepSeek nhận lại `reasoning_content` của từng tin assistant và Claude nhận lại block thinking có chữ ký, trong phạm vi một lượt; reasoning không bao giờ được lưu. Model không có trong bảng dựng sẵn được coi là không suy luận.

**Trong lúc agent đang làm việc**, giống prime-agent và Claude Code: tin nhắn gửi bằng Enter được đưa vào lượt đang chạy - model đọc nó ở bước kế tiếp, hoặc ngay sau câu trả lời đang viết, và lượt tiếp tục - thay vì chờ xong việc (chỉ xếp hàng khi lượt chưa nhận được). `/effort`, `/model` và `/permissions` áp dụng từ lần gọi model hoặc thao tác kế tiếp của lượt đang chạy, không bị từ chối.

`/goal <mục tiêu>` đặt một mục tiêu bền. Mỗi lượt mang mục tiêu trong context, và model có tool `goal_complete` (được phép không cần hỏi). Lượt kết thúc mà chưa gọi `goal_complete` sẽ được tự động tiếp tục, tối đa 10 lần, sau đó mục tiêu tạm dừng. Ctrl-C và `/goal pause` tạm dừng; `/goal resume` tiếp tục; `/goal clear` xoá. Mục tiêu được lưu cùng hội thoại: `/resume` khôi phục nó ở trạng thái tạm dừng, `/new` bỏ nó. Trong một lượt, khi transcript của chính lượt đó vượt khoảng 200 KB, các kết quả tool cũ nhất (trừ 6 kết quả mới nhất) được thay bằng một dòng ghi chú; model có thể gọi lại tool nếu cần.

`@` mở bộ chọn file; `@<server>:<uri>` đính kèm MCP text resource. `!cmd` chạy shell qua cổng phê duyệt; `!!cmd` chỉ hiển thị output. Enter gửi, Ctrl-J xuống dòng, ↑↓ chọn menu hoặc lịch sử, PgUp/PgDn và Home/End cuộn panel `/more`, Esc đóng panel hoặc ngắt lượt, Ctrl-C hủy lượt (bấm hai lần trong 2 giây trên dòng rỗng để thoát), Ctrl-O đổi chế độ chi tiết như prime-agent: thu gọn ẩn reasoning và cắt mọi output tool còn ba dòng, chi tiết hiện reasoning và toàn bộ diff của các lần sửa file, mở rộng hiện toàn bộ output; thẻ của mỗi lời gọi tool giống nhau ở cả ba chế độ. Ctrl-D trên dòng rỗng thoát. Như prime-agent, TUI mặc định chạy **fullscreen**: alternate screen chứa một cửa sổ cuộn được trên hội thoại, ô nhập, gợi ý phím và thanh trạng thái ghim ở đáy, tên chat (tên đặt bằng `/name`, nếu không thì tên thư mục workspace) và chi phí ghim ở trên cùng. Con lăn chuột cuộn ba dòng, PgUp/PgDn cuộn một trang (panel hoặc menu đang mở vẫn giữ PgUp/PgDn của nó), Shift+Alt+↑ lên đầu và Ctrl+End hoặc Ctrl+Shift+↓ về cuối rồi bám theo output (khi đang đọc lại thì hiện `ctrl+end to follow`; Windows Terminal giữ Ctrl+Shift+↓ cho việc cuộn scrollback của nó). Kéo chuột để bôi đen - vùng chọn bám theo các dòng đã chọn dù câu trả lời vẫn đang chạy, và giữ chuột ở mép thì tự cuộn tiếp - thả ra là chép vào clipboard (`Copied selection to clipboard`). Khi rời fullscreen, những gì nó hiển thị được in vào scrollback của terminal. `/fullscreen [on|off]` đổi ngay và lưu lựa chọn thành `terminal.fullscreen` trong `settings.json`; `terminal.fullscreenMouse: false` để chuột cho terminal xử lý, còn `HA_FULLSCREEN` (tương ứng `PI_FULLSCREEN` của prime-agent: chỉ bật khi bằng `1`) được ưu tiên hơn cả hai. Ở chế độ inline, bản ghi cũ ở scrollback của terminal; plain mode in theo dòng. Như Codex, lúc khởi động ha báo `Update available! <bản này> -> <bản mới nhất>` khi npm registry có bản `harness-agents` mới hơn (hỏi ngầm ở mỗi lần mở, lưu ở `<data-dir>/cache/version.json`, nên thông báo hiện từ lần mở sau khi có bản mới); `HA_NO_UPDATE_CHECK=1` hoặc `"checkForUpdates": false` trong `settings.json` tắt nó.

### 12.3. Config v2, quyền và hook

Config v2 hợp nhất default → user (`<config-dir>/config.toml`) → project đã trust (`.harness/config.toml`, rồi `.harness/config.local.toml` cho permission local) → env → CLI. `/config` chỉ ra nguồn của giá trị; project chưa trust không nạp config, hook hoặc skill của project. `/trust` yêu cầu xác nhận. Profile và model có thể chọn cho lượt tiếp theo.

`[permissions]` hỗ trợ `mode = "ask" | "auto-edit" | "full-auto"`, `allow` và `deny`; deny và protected path luôn thắng allow. Panel có `y` (một action), `a` (cả lượt), `n` (từ chối); `A` đề xuất rule lâu dài nhưng chỉ ghi vào `.harness/config.local.toml` sau xác nhận riêng. Headless không tự duyệt khi chưa cấp quyền. `--allowed-tools`, `--disallowed-tools` và `--approval` dùng cùng policy. `[hooks]` theo mô hình hook của Claude Code; hook chỉ nạp từ lớp đã trust, có timeout (tối đa 60 giây) và không thể biến quyết định ask thành allow. `/hooks` xem hook có hiệu lực.

```toml
[[hooks.pre_tool_use]]
matcher = "write_file|edit_file"   # `*`, các tên nối bằng `|`, hoặc regex như `git_.*`
command = "python"                 # một file thực thi, không phải lệnh shell
args = ["scripts/guard.py"]
timeout_seconds = 10
```

| Event | Khi nào | Hook làm được gì |
|---|---|---|
| `pre_tool_use` | trước một lần gọi tool, sau policy | chặn (exit 2, `decision: "block"` hoặc `permissionDecision: "deny"`); hỏi người dùng cả khi policy cho phép (`"ask"`); sửa tham số (`updatedInput`, policy xét lại); thêm `additionalContext` |
| `post_tool_use` | sau một lần gọi đã chạy | đọc `tool_input` và `tool_response`; gửi phản hồi cho model (`decision: "block"` + `reason`, `additionalContext`) |
| `stop` / `subagent_stop` | agent chính / agent con kết thúc | `decision: "block"` + `reason` cho lượt chạy tiếp với lý do đó; lần dừng thứ hai có `stop_hook_active` là `true` |
| `user_prompt_submit` | trước khi gửi prompt | chặn prompt; stdout hoặc `additionalContext` được thêm vào prompt |
| `session_start` | trước prompt đầu tiên của một phiên; `source` là `startup`, `resume` (`/resume`, `/fork`, `/clone`), `clear` (`/new`) hoặc `compact` (sau `/compact`) | stdout hoặc `additionalContext` được thêm vào prompt |
| `session_end` | app đóng | chỉ quan sát |
| `pre_compact` | trước `/compact` | chỉ quan sát |
| `notification` | có câu hỏi hoặc yêu cầu duyệt đang chờ | chỉ quan sát |

Hook nhận một object JSON qua stdin (`hook_event_name`, `session_id`, `cwd`, và tùy event: `tool_name`, `tool_input`, `tool_response`, `prompt`, `last_assistant_message`, `stop_hook_active`), tối đa 8 KiB, tham số giống bí mật bị che. Exit 0 là đi tiếp; exit 2 là chặn, lý do là dòng đầu stdout (không có thì stderr). Exit 0 kèm object JSON trên stdout được đọc các trường `continue: false` + `stopReason` (kết thúc lượt, `hook_stopped`), `systemMessage` (hiện cho người dùng), `decision`/`reason` và `hookSpecificOutput`. Exit khác hoặc quá thời gian thì chặn lần gọi ở `pre_tool_use`, còn ở event khác chỉ báo lại. `permissionDecision: "allow"` không được áp dụng: hook không thể trả lời thay một yêu cầu duyệt.

`[mcp_servers.<name>]` cấu hình stdio hoặc Streamable HTTP. Stdio dùng `command`, `args`, `cwd`, `env` dạng secret ref; HTTP bearer đọc từ env. `enabled_tools`, `disabled_tools`, `tool_timeout` (tối đa 120 giây), `required` giới hạn server. Server được khởi động khi cần. Trong chat, lệnh của prime-agent `/mcp add <name> [--env KEY=VALUE] [--cwd DIR] [--force] -- <command> [args...]` (hoặc `--url <https-url> [--bearer-token-env-var VAR]`), `/mcp list`, `/mcp get <name>` và `/mcp remove <name>` quản lý server trong `mcp-servers.json` cạnh config người dùng, được đọc ở tầng user và có hiệu lực từ lượt sau, không viết lại `config.toml`; `/mcp` không kèm tham số cho xem trạng thái từng server. `ha mcp add|list|get|remove` sửa `config.toml` từ dòng lệnh. Tool MCP đi qua cùng policy/approval và có receipt. Skills tìm ở `<config-dir>/skills`, `~/.agents/skills` và project đã trust (`.agents/skills`, `.harness/skills`); prompt ban đầu chỉ chứa tên và mô tả, nội dung nạp theo digest khi kích hoạt.

### 12.4. Automation

```text
ha exec "Summarize the changed files" --output-format text
ha exec --prompt - --output-format json < prompt.txt
ha exec "Continue the task" --continue --goal "Task complete" --max-turns 4 --output-format stream-json
```

`--prompt -` đọc tối đa 10 MiB từ stdin; `-` ở vị trí prompt là chữ thường. `text` in câu trả lời, `json` in envelope schema 1 với trường mới additive (khi `--goal` satisfied: `acceptance.command_id`), `stream-json` in mỗi event trên một dòng NDJSON, không ANSI. `--continue` chọn session mới nhất của project. Các mã thoát: 0 hoàn tất, 2 usage, 3 chờ câu hỏi hoặc approval, 4 thất bại, 5 xung đột ownership, 130 hủy. `ha chat --headless --json` vẫn tương thích cách gọi cũ. Chỉ dùng `--mock` hoặc `--fixture` cho thử nghiệm cục bộ; gọi provider thật có thể phát sinh chi phí.

### 12.5. Giới hạn và kiểm tra

Khi model gọi nhiều tool một lúc, chúng chạy song song như prime-agent; một lô có `ipython`, tool ghi file, `run_process`, `run_shell` hoặc `ask_user` thì chạy lần lượt từng lệnh. Phê duyệt, intent và receipt vẫn theo đúng thứ tự gọi. Tool chỉ đọc không còn lấy fingerprint workspace, và fingerprint chỉ hash lại những file đổi kích thước hoặc thời gian sửa. `run_shell` dùng `pwsh` trên Windows và `powershell.exe` khi thiếu `pwsh`; receipt ghi shell được chọn. Strict isolation chỉ được báo khi backend đã đo hỗ trợ. `/status` và `/config` là điểm bắt đầu khi provider hoặc quyền không như dự kiến. Giống prime-agent, một lượt không bị giới hạn số bước, số tool call hay thời gian: lượt chạy tới khi model xong, bạn dừng, hoặc hết token; `HA_TURN_MAX_STEPS`, `HA_TURN_MAX_TOOL_CALLS` và `HA_TURN_DEADLINE_SECONDS` đặt giới hạn nếu cần, và lượt chạm giới hạn được tự tiếp tục tối đa hai lần (`HA_TURN_CONTINUATIONS`). Toàn bộ hội thoại được giữ, và khi một yêu cầu gần đầy cửa sổ của model (cửa sổ trừ phần dành cho câu trả lời và phần dự trữ tối đa 16384 token) thì được compact như prime-agent: các lượt cũ thành bản tóm tắt do model viết, phần gần nhất (khoảng 20000 token) giữ nguyên văn, sau đó các kết quả tool cũ của lượt được rút gọn, rồi lượt chạy tiếp. `/compact` và checkpoint tự động tóm tắt toàn bộ hội thoại, không chỉ lượt cuối. Chỉ yêu cầu vẫn không vừa cửa sổ mới báo lỗi. Nếu `ha` chạy như bản cũ, `Get-Command ha -All` cho biết file thực thi nào đang chạy; cài lại bằng `scripts/Install-Ha.ps1`. Các gate M0–M6, H và PTY có evidence riêng. Linux đang chờ hỗ trợ: job CI của Linux không chặn push, và không có tuyên bố nào về Linux được suy ra từ nó.

### 12.6. `/resume`

`/resume` phát lại các lượt durable của hội thoại được chọn cho model và hiện chúng trên màn hình. Nó không suy luận hoặc tạo thêm data source cross-session.

### 12.7. Agent chạy nền

Như daemon của prime-agent, một phiên tương tác chạy trong một worker nền và terminal gắn vào nó. Đóng terminal - `/quit`, ctrl+d, đóng cửa sổ - chỉ tách terminal ra: lượt đang chạy vẫn chạy xong, còn goal, các cổng autonomous, tin nhắn đang xếp hàng, heartbeat, job `/schedule` và agent con vẫn tiếp tục. Terminal báo điều đó khi thoát (`agent <id> keeps running in the background`).

| Lệnh | Tác dụng |
| --- | --- |
| `ha agents` | Agents view của prime-agent, trong terminal: mọi agent đang chạy và các hội thoại đã lưu của project này, chia nhóm Running / Idle / Inactive và làm mới mỗi giây. Gõ chữ để tìm (danh sách xếp theo độ khớp); ↑/↓ chọn; Enter hoặc → mở - gắn vào agent đang chạy, hoặc mở lại hội thoại đã lưu thành agent; Space viết trả lời (gửi cho agent, hoặc là prompt để mở lại hội thoại đã lưu ở chế độ nền); Ctrl+R đổi tên agent; Ctrl+X hai lần dừng agent; Ctrl+N tạo phiên mới; Esc thoát. Trong phiên do worker chạy, ← ở ô nhập trống hoặc `/resume` không tham số quay về view. Khi không phải terminal, hoặc có `--json`, lệnh in danh sách như `ha list`. |
| `ha list` | Mọi agent đang chạy: id, tên, trạng thái, thời gian rảnh, project và yêu cầu gần nhất; `*` đánh dấu agent đang có terminal gắn vào. `--json` cho script. Các agent còn nằm trong journal sau khi khởi động lại máy hoặc worker chết sẽ được khởi động lại trước. |
| `ha attach <agent>` | Đưa agent về terminal này: hội thoại đến giờ được vẽ lại và bạn gõ tiếp. Nhiều terminal có thể cùng gắn vào một agent, như các client của prime-agent cùng xem một phiên: terminal nào cũng thấy agent làm gì, phím nào được trả lời về đúng terminal đã gõ, và `/quit` chỉ tách terminal đó. |
| `ha send <agent> "<tin nhắn>"` | `send` của prime-agent: agent rảnh bắt đầu một lượt với tin nhắn; agent đang bận đọc nó ở bước kế tiếp (`--steer`, mặc định) hoặc sau lượt (`--follow-up`). `--from <agent>` ghi tên người gửi. |
| `ha rename <agent> <tên>` | Đặt tên để `attach`, `send` và `stop` dùng. |
| `ha stop <agent>` | Dừng agent: lượt và agent con của nó bị hủy. Hội thoại vẫn nằm trong store. |
| `ha shutdown [--force]` | Dừng mọi agent và worker; không có `--force` thì hỏi trước. |

`<agent>` là id, tên hoặc tiền tố id mà không agent nào khác trùng.

Mỗi project có một worker, và các agent của project dùng chung store của nó, nên hai agent cùng project chạy song song thay vì chờ writer lock của store. Trong Python REPL của agent, `await rlm.create_session('<prompt>', name=..., model=..., thinking=..., cwd=...)` khởi động một agent top-level riêng (API của prime-agent): trong cùng worker, hoặc trong worker của project mà `cwd` thuộc về.

Agent không có terminal, không có việc đang chạy và không có lịch sẽ dừng sau `idleEvictionMinutes` của prime-agent (mặc định 90; đặt một số hoặc `"off"` trong `settings.json`). Worker thoát khi agent cuối cùng không còn. Descriptor của nó (port và token, chỉ chủ sở hữu đọc được) và log nằm trong `<data-dir>/workers/`; kết nối phải trình token, còn môi trường của client - nơi lấy credential - đi qua socket loopback và không bao giờ được ghi ra file.

`ha exec` cũng chạy lượt của nó trong worker của project, như prime-agent chạy phiên headless qua daemon: lượt ghi qua store mà các agent dùng chung, nên một agent đang chạy lượt không còn làm `ha exec` lỗi `writer_locked`. Output, JSON và exit code giữ nguyên như trước; Ctrl-C hủy lượt. Với `HA_DAEMON=off`, hoặc khi không có worker cùng bản build đang chạy, lượt chạy ngay trong tiến trình gọi. Worker chỉ được khởi động từ terminal: worker do một lệnh có output bị chuyển hướng khởi động sẽ giữ output đó mở suốt thời gian nó chạy (trên Windows tiến trình con kế thừa handle), khiến `ha exec ... | tool` không bao giờ kết thúc.

Như supervisor của prime-agent, worker chạy dưới một tiến trình supervisor, và supervisor khởi động lại worker khi nó chết - sau 250 ms, 1 s và 5 s; worker cứ chết mãi thì dừng hẳn. Worker giữ một journal các agent của nó (`<data-dir>/workers/<project>.journal`: id, tên và hội thoại - không bao giờ ghi môi trường), và worker vừa khởi động sẽ đưa chúng trở lại với đúng id và hội thoại, rồi mở mọi hội thoại của project còn job `/schedule` hoặc quota park, để job chạy dù không có terminal nào mở. Sau khi khởi động lại máy, lần `ha` kế tiếp trong project, hoặc `ha agents` chạy trong terminal, sẽ khởi động worker và các agent quay lại; chúng chạy với môi trường của terminal đã khởi động worker. Khi `ha` được cập nhật, terminal của bản mới thấy worker của bản cũ đang rảnh thì yêu cầu nó dừng nhưng giữ journal, rồi khởi động worker mới để đưa các agent trở lại; worker có agent đang làm việc thì để nguyên, terminal báo điều đó và tự chạy phiên.

Khác prime-agent ở chỗ: mỗi project một worker thay vì mỗi session, và lượt bị crash cắt ngang không được chạy tiếp - agent được khôi phục tiếp tục hội thoại, còn lượt bị cắt được phát lại là đã bị gián đoạn. `HA_DAEMON=off` hoặc `"daemon": false` trong `settings.json` chạy phiên ngay trong terminal như trước; renderer plain luôn chạy như vậy.

### 12.8. Thêm từ prime-agent

**Cách mở.** `ha "sửa parser"` mở app và gửi luôn tin nhắn; khi có input được pipe vào (`cat log | ha "vì sao?"`) input được nối sau tin nhắn và câu trả lời được in ra như `ha exec`. `ha exec` (và `ha chat --headless`) nhận `--model`, `--thinking`, `--system-prompt` và `--append-system-prompt`. `ha model list [search] [--json]` liệt kê model trong catalog đã có credential, `ha prompt [--cwd] [--json]` in system prompt mà một phiên sẽ nhận. `--offline` (hoặc `HA_OFFLINE=1`) bỏ mọi lần tải lúc khởi động - model catalog, danh sách model của provider đã đăng nhập và kiểm tra cập nhật.

**RPC và ACP.** `ha --mode rpc [--model M]` phục vụ chế độ RPC của prime-agent: lệnh JSON qua stdin (`prompt` kèm `streamingBehavior` `steer` hoặc `followUp` khi đang có lượt chạy, `steer`, `follow_up`, `abort`, `new_session`, `get_state`, `set_model`, `cycle_model`, `get_available_models`, `set_thinking_level`, `compact`, `get_last_assistant_text`, `set_session_name`), phản hồi `{id, type: "response", command, success, data | error}` và các session event của prime-agent (`agent_start`, `message_start` / `message_update` / `message_end`, `tool_execution_start` / `_update` / `_end`, `compaction_start` / `_end`, `agent_end`) qua stdout. `ha --mode acp` phục vụ Agent Client Protocol cho editor qua stdio: `initialize`, `session/new` (mỗi kết nối một phiên, mở ở `cwd` được nêu), `session/prompt` stream các `session/update` (chữ, tool call) và trả về `stopReason`, `session/cancel` và `session/close`. Ở cả hai chế độ không ai trả lời được bảng phê duyệt, nên hành động cần duyệt bị từ chối, như `ha exec`.

**Agent chạy nền.** `ha send --wait` in câu trả lời của lượt mà tin nhắn khởi động; `ha abort <agent>` dừng lượt đang chạy, agent vẫn còn. `ha send <session id> "<tin>"` tới một hội thoại đã lưu mà không agent nào đang chạy sẽ mở lại nó thành agent mới với tin đó là prompt đầu. `ha schedule list [--all] [agent] [--json]`, `ha schedule add <agent> <when> -- <tin>` và `ha schedule cancel [agent] <job-id>` quản lý prompt hẹn giờ của agent qua worker của nó. Heartbeat (`/heartbeat <chỉ dẫn>`, `/heartbeats`) được lưu cùng hội thoại như job `/schedule`, và dừng agent thì hủy lịch của nó.

**Ô nhập và màn hình.** Ô nhập sửa như prime-agent: nhảy theo từ (Ctrl/Alt+←/→, Alt+B/F), Ctrl+K và Ctrl+Y (kill ring), Alt+D, Ctrl+T, Ctrl+Z / Ctrl+- hoàn tác và Ctrl+Shift+Z làm lại, Ctrl+D xóa về phía trước, và đoạn dán lớn hiện thành `[paste #N +L lines]`, gửi đi đúng nội dung nó đại diện. Ctrl+G sửa tin nhắn bằng `$VISUAL` / `$EDITOR`. Esc lần hai trong nửa giây xóa bản nháp, hoặc mở cây phiên khi ô nhập trống. `/model` không tham số mở menu model; `/tree` và `/fork` mở bộ chọn (mũi tên, Enter, Esc). Lệnh ở đầu dòng, token `@path` và `--flag` được tô màu như prime-agent. Khi có tin đang chờ, dải hàng đợi phía trên ô nhập hiện từng tin (`Steering:` / `Follow-up:`). Khi tham số của một tool call đang stream, dòng trạng thái ghi `Writing code`. Lỗi nhiều dòng chỉ hiện dòng tóm tắt cho tới khi Ctrl+O mở rộng. Việc chép được báo bằng một toast ngắn, và chép được qua OSC 52 trên SSH và tmux. Bảng và link markdown hiển thị như prime-agent. `keybindings.json` cạnh `settings.json` gán lại phím theo id của prime-agent - `{"app.editor.external": "ctrl+e", "tui.editor.yank": []}`: binding đã cấu hình chỉ nhận đúng các phím đó, danh sách rỗng thì tắt nó.

**Model và provider.** `allowedModels` trong `settings.json` toàn cục (`provider/id` chính xác, id trần hoặc glob) giới hạn model mà lượt chạy, `/model` và agent con được dùng; model ngoài danh sách bị từ chối theo thông báo của prime-agent và không bao giờ tự chuyển sang model khác. Anthropic dùng `cache_control` của prime-agent (`HA_CACHE_RETENTION=long` để giữ một giờ), fine-grained tool streaming và interleaved thinking; token prompt được cache đếm và tính giá riêng. Yêu cầu bị provider báo quá dài được compact rồi gửi lại; compaction viết bản tóm tắt có cấu trúc của prime-agent và cập nhật nó ở lần sau thay vì tóm tắt lại từ đầu, và sau compaction prompt kế tiếp mở đầu bằng ghi chú `[python-state]` của prime-agent nêu những tên kernel vẫn còn. `/login serper` lưu key Serper cho `web_search`.

**MCP server.** `ha mcp login <server> [--client-id] [--scope]` đăng nhập vào server Streamable HTTP yêu cầu OAuth (khám phá RFC 9728 / RFC 8414, đăng ký client động, PKCE trên `localhost:53700-53709` hoặc dán URL chuyển hướng); token được lưu trong `auth.json` dưới `mcp:<server>`, làm mới trước khi hết hạn và chỉ gửi tới đúng URL của server đó, `ha mcp logout <server>` xóa nó. Server nhận `enabled = false`, `headers`, `header_env` (header lấy từ biến môi trường) và `startup_timeout_seconds`.

**Hội thoại.** Goal có token budget (`goal.create(..., token_budget=...)`) và tạm dừng ở trạng thái budget-limited khi dùng hết. `/share` đăng hội thoại thành GitHub gist bí mật (qua `gh`); `/new <prompt>` mở hội thoại mới với prompt đó; `/export` mặc định ghi trang HTML. `/import` đọc được cả file phiên của prime-agent. `/btw` khai báo tool của phiên nhưng từ chối mọi lời gọi, như câu hỏi bên lề của prime-agent. `autoRefine.enabled` và `autoRefine.turnInterval` trong `settings.json` điều khiển việc tự review. `run_shell` từ chối lệnh git làm mất thay đổi chưa commit (`git checkout -- .`, `git reset --hard`, `git clean -f`, ...) trừ khi `HA_ALLOW_DESTRUCTIVE_GIT=1`. `rlm.rename(...)` đổi tên phiên hoặc một agent con trực tiếp.

Chưa port: đăng nhập Claude Pro/Max, Copilot và xAI (chúng giả danh client khác), các phần riêng của nền tảng Prime (inference, traces, telemetry, cloud, catalog dịch vụ, Factory), activity dock, `/settings`, theme trong TUI của prime-agent, và trong agents view thì cây agent con (agent con của ha nằm trong một hội thoại), cột chi phí và việc xóa phiên đã lưu.
