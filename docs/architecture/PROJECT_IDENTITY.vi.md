# Định danh project: một thư mục thuộc project nào

[English](PROJECT_IDENTITY.en.md) | Tiếng Việt

ha giữ dữ liệu của một project dưới một key: store của nó
(`<data>/projects/<key>/`, gồm journal của mọi hội thoại, artifact, lease,
review hoãn lại và dấu session log), các entry memory toàn cục giữ cho nó
(`/projects/<key>/...`), lesson và vector lịch sử
(`<data>/history-embedding/<key>`). Từ 0.8.8 key đi theo Git repository: chạy
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
   project của một learned skill) nhận key mới;
   `<data>/history-embedding/<key cũ>` được đổi tên khi key mới chưa có; journal
   worker của store cũ bị bỏ (nó sẽ khởi động lại agent trên đường dẫn cũ) và
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
