# Định danh project: một thư mục thuộc project nào

[English](PROJECT_IDENTITY.en.md) | Tiếng Việt

ha giữ dữ liệu của một project dưới một key: store của nó
(`<data>/projects/<key>/`, gồm journal của mọi hội thoại, artifact, lease,
review hoãn lại và dấu session log), các entry memory toàn cục giữ cho nó
(`/projects/<key>/...`) và lesson. Từ 0.8.8 key đi theo Git repository: chạy
ha trong một thư mục con của repository, hay trong một linked worktree của nó,
mở cùng project với khi chạy ha ở gốc repository.

Code: `project_root` và `resolve` trong
`crates/harness-cli/src/host/application/bootstrap.rs` (key),
`crates/harness-cli/src/host/configuration/adoption.rs` (store cũ) và
`crates/harness-cli/src/host/configuration/project.rs` (project id).

## 1. Key

Key là `project-` cùng 16 chữ số hex đầu của SHA-256 đường dẫn key root. Key
root được tìm từ thư mục ha chạy (`--cwd`, hoặc thư mục khởi động), đọc từ các
file của repository, không chạy Git:

| Thư mục nằm | Key root |
|---|---|
| Trong một main checkout (`.git` gần nhất là thư mục) | Thư mục gốc của checkout đó |
| Trong một linked worktree (file `.git` có git directory mà `commondir` là `.git` của một main checkout) | Thư mục gốc của main checkout |
| Trong submodule, hay worktree của một bare clone (của coder được ủy quyền) | Thư mục gốc của chính nó |
| Không trong repository nào | Chính thư mục đó |

Key root bị từ chối khi nó là thư mục home của người dùng hay thư mục phía trên
(home được giữ trong Git), gốc một ổ đĩa, hay bất cứ thứ gì dưới một data
directory của ha: khi đó chính thư mục đang chạy là key root.

Chạy từ gốc repository băm đúng đường dẫn như trước, nên key, cùng mọi store và
entry memory giữ dưới key đó, không đổi.

Chỉ key đi theo repository. Thư mục ha chạy vẫn là nơi các tool làm việc, nơi
dừng việc dò `AGENTS.md`, nơi tìm session log `.harness/` và trust, và là thứ
dòng `Project:` ở header hiển thị. Dòng `Store:` ở header ghi store mà key chọn.

## 2. Một project id cho repository

Trong một store, mỗi thư mục ha từng chạy được đăng ký dưới một project id
(`project_registrations`). Thư mục gặp lần đầu nhận id của thư mục đã đăng ký
trong store có cùng key root, ưu tiên chính key root, và được đăng ký dưới id
đó. Khi store từ chối vì hai thư mục không cùng Git common directory mà nó thấy
được (ví dụ không có `git` trên PATH), thư mục nhận id riêng, như mọi thư mục
trước 0.8.8.

## 3. Dữ liệu giữ dưới key cũ

Trước 0.8.8, chạy từ thư mục con hay linked worktree giữ dữ liệu dưới key của
chính thư mục đó. Lần chạy đầu tiên từ thư mục đó resolve ra key của repository
sẽ nhận lại dữ liệu:

1. Khi `<data>/projects/<key cũ>/harness.sqlite3` tồn tại, key mới chưa có
   store, không process ha nào giữ `writer.lock` của store cũ và không worker
   nền nào phục vụ nó, thư mục được đổi tên thành `<data>/projects/<key mới>/`,
   dưới `<data>/adoption.lock`. Một thư mục đã có ở key mới mà không có store
   được dời sang `<key mới>.before-adoption-<thời điểm>`, không bao giờ bị xoá.
   Store đang được dùng thì để lại cho lần chạy sau, và lần chạy này nói rõ.
2. Các entry memory toàn cục giữ cho key cũ nhận key mới và tên repository, qua
   đường lưu của chính memory store dưới lock của nó: mỗi entry là một event của
   writer `migration` với cause `adopt`, và giữ nguyên id. Lesson về key cũ (và
   project của một learned skill) nhận key mới; journal worker của store cũ bị bỏ (nó sẽ khởi động lại agent trên đường dẫn cũ) và
   tên agent được chuyển theo.
3. `<data>/projects/<key mới>/project.json` ghi key root và, trong `adopted`,
   các key cũ nó đã nhận.

Hai store đều có dữ liệu thì không bao giờ bị gộp. Khi key mới đã có store, store
cũ để nguyên chỗ: key của nó được ghi vào `apart` trong `project.json`, memory và
lesson vẫn được chuyển (chúng không nằm trong store nào), một diagnostic
`project/store_kept_apart` được ghi, và mỗi lần chạy từ thư mục đó hiện một dòng
nêu store kia (`Note:` ở header, `ha: note:` trên stderr với `ha exec`).
`ha doctor` và `/doctor` liệt kê, trong mục State, mọi store mà các thư mục đã
đăng ký của nó thuộc project này nhưng dưới key khác. Hội thoại trong đó vẫn đọc
được bằng `ha sessions list --data-dir <store>`.

Mỗi bước đều kiểm tra lại trước khi làm và không làm gì khi đã xong, nên chạy hai
lần hay từ nhiều lần chạy cùng lúc đều an toàn. Lần chạy từ thư mục có key không
đổi chỉ tốn hai lần kiểm tra file.

## 4. Những gì thay đổi

- Hội thoại bắt đầu trong thư mục con hay linked worktree nằm trong store của
  repository: `/resume` và `history_search` thấy chúng từ bất kỳ thư mục nào của
  repository.
- Một hội thoại mới chỉ nghe về các hội thoại trước của chính checkout nó. Mỗi
  hội thoại giữ checkout nơi nó bắt đầu (thư mục gần nhất có `.git`), và các
  lượt của hội thoại trước cùng task nó bỏ dở được lấy từ checkout đó: một
  linked worktree ở branch khác không được cho xem task cuối của main checkout,
  và main checkout cũng không thấy của worktree. Hội thoại từ trước 0.8.8 thuộc
  về key root.
- Một worker nền phục vụ cả repository.
- `ha maintenance --cwd <dir>` và `ha sessions list` từ thư mục con mở store của
  repository.
- `ha learning` cũng gắn lesson project được import vào repository, từ thư mục
  hiện tại được viết đúng như một lần chạy từ đó resolve ra.

## 5. Repository bị chuyển chỗ (0.8.9)

Key là hash của một đường dẫn, nên repository bị chuyển chỗ hay đổi tên sẽ có
key mới. Trước 0.8.9, lần chạy đầu tiên ở chỗ mới bắt đầu một store rỗng, và
memory của nó nằm lại dưới key cũ.

**Danh tính của nó được ghi lại.** Lần chạy nào có key root là một repository
cũng ghi danh tính của repository vào `<data>/projects/<key>/project.json`,
dưới `identity`:

- `roots`: các root commit (commit không có cha), lấy bằng một lần
  `git rev-list --max-parents=0 HEAD`, chạy một lần rồi giữ lại: lần chạy sau
  đọc từ manifest và không khởi động process nào;
- `origin`: URL của remote `origin`, đọc từ file `config` của repository ở mỗi
  lần chạy (không cần process), bỏ `/` và `.git` ở cuối.

Repository chưa có commit nào thì chưa ghi gì cho tới khi có. Thư mục không nằm
trong repository thì không có danh tính.

**Store bị chuyển được tìm lại.** Khi key của một lần chạy chưa có store, các
store dưới `<data>/projects/` có manifest cùng root commit (ít nhất một), cùng
remote (hoặc cả hai đều không có), và có root đã ghi không còn tồn tại, chính là
repository này trước khi bị chuyển:

- đúng một store: nó được nhận như một store dưới key cũ (§3): chuyển sang key
  mới dưới `<data>/adoption.lock` khi không có gì giữ nó, memory toàn cục và
  lesson của nó được gán key mới và tên mới của repository, key cũ được ghi vào
  `adopted`, và diagnostics ghi `project/store_followed`;
- nhiều store: không store nào được chuyển, lần chạy báo điều đó bằng một
  notice, và `project/store_follow_ambiguous` nêu tên chúng; `ha project rekey`
  chuyển store cần giữ.

Một fork có cùng root commit nhưng remote khác, nên không bao giờ khớp. Một bản
clone thứ hai vẫn còn tồn tại không bao giờ bị lấy: root của nó vẫn còn đó. Hai
store đều có dữ liệu thì không bao giờ bị gộp.

**Bằng tay.** `ha project rekey <đường dẫn hoặc key cũ> [--cwd DIR]` chuyển
store đang giữ dưới key khác sang key của thư mục hiện tại (hoặc `--cwd`), cũng
bằng cách chuyển và gán lại như trên. Store cũ được gọi bằng key của nó
(`project-` và 16 chữ số hex) hoặc bằng thư mục project từng ở: key thư mục đó
có, key nó có trước 0.8.8, hoặc store mà manifest hay các thư mục đã đăng ký
nêu tên thư mục đó (dấu phân cách và, trên Windows, chữ hoa thường không quan
trọng). Lệnh bị từ chối khi key hiện tại đã có store riêng, hoặc có gì đó đang
giữ store cũ.
