# Guard cho lệnh và bí mật

[English](GUARDS.en.md) | Tiếng Việt

Những gì một lệnh hay một lần ghi file không bao giờ được làm, ở mọi chế độ
duyệt. Đây là các guard hook của
[Agnostic-AI](https://github.com/ucsandman/Agnostic-AI) (đọc ở commit
`aab09ff`), được port vào khâu dispatch tool của ha. Chúng là Rust đã biên dịch,
không phải script hook: một hook lỗi cú pháp không thể làm chúng tắt đi.

Code: `crates/harness-tools/src/security/command_guard.rs` và
`secret_values.rs`, được gọi từ `runtime/service.rs` và từ khâu đọc output của
process trong `processes/process.rs`.

## 1. ha đã có gì, và thêm gì

| Hook của Agnostic-AI | Trước đây | Thêm |
|---|---|---|
| `secret-path-guard` | File tool từ chối `.env*`, `.git`, `.harness`, tên có `credential`/`secret`/`password`/`private_key`, `*.pem`/`.key`/`.p12`/`.pfx` | Chặn đúng các file đó khi đi qua shell (`cat .env`, `Get-Content .env`, `> .env`, `git add .env`) |
| `secret-guard` | Trang knowledge chứa key bị từ chối | Chặn lần ghi file (`write_file`, `edit_file`, `apply_patch`) mang vào một giá trị trông như key mà file chưa có |
| `tool-output-secret-watch` | Lệnh không nhận biến nào của host ngoài allowlist, nên dump môi trường không lộ key của host; giá trị `secret://` được che; dòng `key=value` được che trong preview | Mọi giá trị trông như key trong output được che trước khi model, file spool hay preview đọc tới |
| `git-tree-guard` | `git_guard.rs`: từ chối lệnh huỷ thay đổi chưa commit (`reset --hard`, `checkout -- .`, `clean -f`, `restore .`) | (giữ nguyên) |
| `rm-guard` | chưa có | Xoá đệ quy |
| `process-kill-guard` | chưa có | Kill theo tên |
| `slow-command-guard` | chưa có | Tìm kiếm đệ quy trên cả ổ đĩa hoặc thư mục home |
| `gate-freeze` | File tool chỉ ở trong workspace | Config, hook và state của ha, cùng `.harness` của project, không bị sửa qua shell |

Không lấy:
- `scope-lock` (maintainer quyết định bỏ);
- `guard-canary` (guard đã biên dịch thì script hỏng không tắt được);
- `mods-liveness` (riêng của Claude Code);
- `dev-server-guard`, `security-tier-check` (chế độ duyệt và strict execution
  của ha đã lo);
- `slopsquat-guard` (cần tra registry ngay trong đường đi của tool; để cho lần
  sau).

Lời nhắc verify cuối lượt (`forced-verify-stop-gate`) dựa trên thứ ha đã có:
turn driver hỏi một `CompletionCheck` xem câu trả lời của một lượt có sửa file mà
sau đó không chạy gì có tự nhận đã xong không, và gửi trả lại một lần. Trước đây
chỉ plugin jev cài đặt check này. Giờ một lượt không có check cũng được nhắc
(`TurnDriver::with_verify_reminder`):
- Khi lượt sửa một file không phải văn bản (`.md`, `.markdown`, `.txt`, `.rst`,
  `.adoc`) và không chạy lệnh nào sau lần sửa cuối, model được nhắc một lần: chạy
  check của project trên phần đã sửa và báo kết quả, hoặc nói trong một dòng là
  không kiểm được ở đây hay chưa kiểm chứng.
- Hội thoại hiện `verify reminder: code changed and nothing ran after it`.
- Bật cho chat, `ha exec` và child. Tắt bằng `"verifyReminder": false` trong
  settings hoặc `HA_VERIFY_REMINDER=off`.
- Khi completion check của jev bật thì jev phán xét thay.

## 2. Một lệnh được đọc thế nào

`command_guard::parse` đọc dòng lệnh như bash và PowerShell đọc:

- nháy đơn, nháy kép, và comment `#`;
- dấu phân cách `;` `&&` `||` `|` `&`, xuống dòng, `( )` và `{ }`;
- `$(...)`: lệnh bên trong cũng là lệnh;
- chuyển hướng (`>`, `>>`, `2>`; `2>&1` là luồng, không phải file);
- `sudo`, `env`, `nice`, `time`, `command`, `exec`, `nohup` và phép gán
  `NAME=value` ở đầu lệnh được bỏ qua;
- toán tử gọi `&` của PowerShell;
- `${HOME}`.

Shell chạy một lệnh truyền qua tham số (`bash -c "..."`,
`powershell -Command ...`, `cmd /c ...`) thì lệnh đó cũng được đọc, tối đa bốn
tầng. Lời gọi `run_process` được đọc như một lệnh, file chạy đứng đầu.

## 3. Các guard

Mỗi lần từ chối bắt đầu bằng tên guard trong ngoặc vuông và nêu biến dùng để bỏ
qua guard. Biến được đọc từ môi trường của chính ha lúc khởi động, không bao giờ
từ lệnh, nên `HA_ALLOW_...=1 rm -rf x` không bỏ qua được gì.

| Guard | Từ chối | Bỏ qua bằng |
|---|---|---|
| `recursive-delete` | `rm -r`/`-rf`/`--recursive`, `Remove-Item -Recurse`, `del`/`rd`/`rmdir /s`, `find ... -delete`, trừ khi mọi đích đều lấy lại được | `HA_ALLOW_RECURSIVE_DELETE` |
| `kill-by-name` | `Stop-Process -Name` (và `spps`, `kill`), `Get-Process <tên> \| Stop-Process`, `taskkill /IM`, `pkill`, `killall`, `& $var -Name`, `iex ... -Name` | `HA_ALLOW_KILL_BY_NAME` |
| `root-search` | `grep -r`, `rg`, `ag`, `find`, `fd`, `Get-ChildItem -Recurse`, `dir /s`, `ls -R` từ `/`, một ổ đĩa (`C:`, `/c`, `/mnt/c`), `C:\Users`, `~`, `$HOME`, `$env:USERPROFILE` hoặc thư mục home | `HA_ALLOW_ROOT_SEARCH` |
| `secret-path` | Mọi lệnh nhắc tới file bí mật, trừ lệnh chỉ kiểm tra file có tồn tại không (`echo`, `Test-Path`, `ls`, `git status`, `git rm --cached`, ...) | `HA_ALLOW_SECRET_PATHS` |
| `harness-integrity` | Lệnh thay đổi (xoá, chuyển, chép đè, ghi, `sed -i`, `chmod`, ...) hoặc chuyển hướng vào thư mục config của ha, thư mục data của ha, hay `.harness` của workspace | `HA_ALLOW_HARNESS_WRITES` |
| `secret-write` | Lần ghi file mang vào giá trị trông như key | `HA_ALLOW_SECRET_WRITES` |

**Khi nào được xoá đệ quy.** Mọi đích phải là một trong hai loại:
- thư mục build hay cache (`node_modules`, `dist`, `build`, `target`, `out`,
  `.next`, `coverage`, `__pycache__`, `.cache`, `tmp`, ...), không đi qua `..`;
- nằm trong workspace và trong git, và
  `git status --porcelain --untracked-files=all --ignored=matching` không liệt
  kê gì bên dưới nó: mọi file đều được track và chưa đổi, nên git trả lại được.

Những trường hợp còn lại bị từ chối:
- đích nằm ngoài workspace (kể cả thư mục temp của hệ thống, vì ở đó có file
  của chương trình khác);
- cả workspace;
- biến, glob hoặc `~`;
- đường dẫn tương đối sau một lệnh `cd`;
- lệnh xoá nhận đầu vào từ pipe.

**File bí mật** gồm:
- `.env`, `.envrc`, `.env.*` (trừ `.example`, `.sample`, `.template`, `.dist`);
- `id_rsa`/`id_ed25519`/`id_ecdsa`/`id_dsa`, `.secrets`, `*private_key*`;
- `credentials.json`, `.aws/credentials`, `.npmrc`, `.netrc`, `.pgpass`,
  `.git-credentials`;
- `*.pem`/`*.key`/`*.p12`/`*.pfx`/`*.clixml`.

**Thư mục của ha** được khai báo một lần mỗi process (`protect_host_dirs`), bởi
lượt chat và bởi `ha exec`, nên tool của child cũng được bảo vệ. Worktree của
coder nằm trong thư mục state của ha. Bên trong workspace thì chỉ `.harness` bị
tính, nên child tự do sửa worktree của chính nó.

**Lệnh chạy ở đâu.** Mỗi lần gọi `run_shell` và `run_process` trước đây đều bắt
đầu ở gốc workspace với `PATH` của host, nên `cd sub` (hoặc `Set-Location sub`)
ở lần gọi này mất ở lần gọi sau, và model chạy lệnh tiếp theo sai chỗ. Python REPL
giữ được `os.chdir`, còn shell thì không. arXiv 2611.22299, *Harnesses That
Improve Themselves*, kể vị trí shell được giữ lại trong các cơ chế runtime mà
những harness tự tiến hoá cứ phải tự nghĩ ra. Nay (`processes/location.rs`):

- Host đặt một đoạn ghi nhận phía trước mỗi lệnh `run_shell`, không bao giờ nối
  sau nó (chữ nối thêm sau lệnh bị đọc như một phần của lệnh: heredoc không có
  điểm kết hay dấu gạch chéo ngược ở cuối nuốt mất nó, và lệnh tự thoát thì không
  bao giờ chạy tới). Khi shell thoát, bằng cách nào cũng vậy, nó ghi thư mục cuối
  và `PATH` vào một file phụ do host chọn trong spool của process (không bao giờ
  ghi ra stdout), và giữ nguyên trạng thái như lệnh đã tạo ra:
  - `sh`: một bẫy `EXIT` lưu `$?`, ghi `pwd` và `$PATH`, rồi thoát với trạng thái
    đã lưu. Lệnh tự đặt bẫy `EXIT` thì thay bẫy của ta: không giữ vị trí nào cho
    nó (hành vi cũ). Lỗi cú pháp để vị trí ở chỗ lần gọi bắt đầu.
  - `pwsh` và `powershell.exe`: một sự kiện engine `PowerShell.Exiting` đăng ký
    đầu tiên (nó cũng chạy khi `exit N`, trên PowerShell 7 và 5.1), và một dòng
    trạng thái sau lệnh và một dòng trống. Đã đo trên cả hai: `-Command` thoát 0
    khi câu lệnh cuối thành công, 1 khi nó hỏng (mã riêng của chương trình native
    bị mất) và N với `exit N`. Dòng trạng thái thoát 0 khi thành công,
    `$LASTEXITCODE` của chương trình native khi lệnh hỏng và nó là số nguyên khác
    0, còn lại là 1; nên `exit 5` của một chương trình native nay cho 5 (trước là
    1), thành công vẫn là 0, và `exit N` vẫn là N. Lệnh không parse được thì
    không chạy gì, nên vị trí giữ nguyên.
  - Lệnh bị kill, hết giờ hoặc bị huỷ thì không ghi file, nên vị trí giữ nguyên.
- Host giữ, theo từng cuộc trò chuyện (task), thư mục và `PATH` cuối cùng; lần
  `run_shell` và `run_process` tiếp theo của cuộc trò chuyện đó bắt đầu từ đó.
- Các lần gọi của một cuộc trò chuyện lần lượt chờ nhau: một lần gọi đọc vị trí
  bắt đầu và lưu vị trí kết thúc khi đang giữ khoá của cuộc trò chuyện, sau khi
  mọi lần gọi trước nó đã lưu xong. Trước đây nó đọc vị trí trước khi giữ permit
  của process, nên hai lần gọi chạy cùng lúc có thể lưu vị trí cũ đè lên vị trí
  mới. (Một batch của turn vốn đã chạy `run_shell` và `run_process` từng cái một;
  điều này đúng cả khi chúng không như vậy, và `bash()` của Python REPL không đọc
  hay đổi vị trí.)
- Thư mục chỉ được giữ khi nó nằm trong gốc workspace sau khi đã resolve (link
  trỏ ra ngoài không tính). Ngoài workspace thì lần gọi sau bắt đầu ở gốc, và
  kết quả nói điều đó trong một dòng. `PATH` chỉ được giữ khi khác `PATH` của
  host. Không biến nào khác được giữ: môi trường chứa bí mật.
- `PATH` được giữ chỉ là cái lệnh chạy với nó. Shell được tìm trong `PATH` của
  chính host và khởi chạy bằng đường dẫn đầy đủ, nên một lần gọi làm rỗng `PATH`
  không để lần sau không còn shell để chạy. `run_process` một chương trình gọi
  bằng tên trần thì được tìm trong `PATH` đã giữ và, nếu không có ở đó, không
  khởi chạy (`the program did not start ... it is not on the PATH an earlier
  shell call set; nothing ran`), như một shell với `PATH` đó sẽ nói. Nếu không,
  Windows vẫn tìm thấy nó trong `PATH` của host.
- Guard lệnh của Jev xét một lần gọi ở cổng, trước khi duyệt, từ chỗ shell đang
  đứng lúc đó. Không thể giữ khoá của cuộc trò chuyện từ đó tới lúc dispatch
  (guard là một lần gọi model, và bước duyệt chờ một người), nên dispatch, khi đang
  giữ khoá, so thư mục nó bắt đầu với thư mục guard đã xét và, khi khác nhau (một
  lần gọi của cuộc trò chuyện đã chạy xen giữa), hỏi guard lại từ thư mục đúng.
  Nếu bị từ chối thì đó là kết quả của lần gọi; lệnh không bao giờ chạy dựa trên
  một phán quyết cho thư mục khác.
- Các guard ở trên đọc đường dẫn tương đối từ thư mục lệnh chạy, không phải từ
  gốc: `recursive-delete` (xoá việc chưa commit, gọi tên từ thư mục đã giữ, bị từ
  chối; đọc từ gốc thì nó không chỉ vào gì và được qua), `harness-integrity`,
  guard git phá huỷ (`git status` của nó chạy ở đó) và guard lệnh của Jev
  (`working_dir`, và các script nó đọc). Một `cd` trong cùng lệnh vẫn để các đích
  sau nó không kiểm được, như trước. Danh sách file trong bản tóm tắt khi nén đọc
  từ thư mục mà kết quả của lệnh nêu tên.
- Kết quả cho biết lệnh chạy ở đâu khi đó không phải gốc (`ran in sub`) và, khi
  lệnh đã chuyển chỗ, lệnh tiếp theo sẽ bắt đầu ở đâu.
- `HA_SHELL_KEEP_LOCATION=off` trả lại hành vi cũ: mọi lần gọi bắt đầu ở gốc với
  `PATH` của host.

## 4. Giá trị trông như key

`secret_values` nhận ra:

- key của nhà cung cấp theo tiền tố và độ dài: Anthropic, OpenAI (`sk-`,
  `sk-proj-`), Stripe live, AWS access key id, token của GitHub, Slack, Google,
  npm và Hugging Face;
- header của private key PEM;
- một giá trị dài gán cho trường có tên chỉ bí mật (`api_key`, `secret`,
  `token`, `password`, `auth`);
- bearer token.

Một kết quả chỉ được tính khi giá trị đủ ngẫu nhiên: ít nhất 10 ký tự khác nhau
và entropy Shannon từ 3 tới 4 bit mỗi ký tự, tuỳ dạng. Nên placeholder như
`sk-xxxxxxxxxxxxxxxxxxxx` hay `your-api-key-goes-here` không bị coi là key, và
không cần danh sách từ placeholder nào.

- **Khi ghi:** giá trị chỉ bị từ chối khi file chưa có sẵn nó. Key đã nằm trong
  file là việc của người dùng chuyển đi; từ chối mọi lần sửa file đó thì chẳng
  giúp được ai.
- **Output:** giá trị được thay bằng `[REDACTED <loại>]` ngay khi đọc luồng,
  từng dòng một (ký tự `\r` cũng kết thúc dòng, nên thanh tiến trình vẫn chạy).
  Giá trị không bao giờ tới file spool, artifact, preview hay model.
- **Không bị che:** file người dùng đính kèm, hoặc file agent đọc bằng
  `read_file`. File thứ nhất do người dùng tự đưa; file thứ hai là văn bản mà một
  lần sửa phải khớp. Dù vậy, tool file vẫn từ chối file bí mật theo tên.

## 5. Kiểm thử

- Unit test trong cả hai module:
  - bộ đọc lệnh, gồm cả shell lồng nhau;
  - mọi trường hợp từ chối và cho qua của từng guard;
  - xoá đệ quy trên một repo git thật: đã track và sạch, chưa track, ngoài
    workspace, sau `cd`, dùng biến, cả workspace, và `build/..`;
  - worktree nằm trong state của ha;
  - các dạng key và placeholder, và việc che một giá trị bị cắt qua hai lần đọc.
- Live suite `scripts/testing/live-tests/guards.sh` nhờ một model thật gọi từng
  lệnh, rồi đọc những gì model đã được gửi:
  - xoá việc chưa track bị từ chối và dữ liệu vẫn còn, còn xoá thư mục build
    thì chạy;
  - kill theo tên, đọc `.env`, sửa `.harness` và tìm kiếm cả ổ đĩa đều bị từ
    chối;
  - key in ra từ một file settings tới model ở dạng đã che;
  - ghi một key vào file bị từ chối.
