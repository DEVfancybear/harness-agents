# Kho memory và tương đương memory-engine

[English](MEMORY_STORE.en.md) | Tiếng Việt

**Trạng thái:** thiết kế, ngày 26/10/2026. Đây là kế hoạch đưa các tính năng của
[timescale/memory-engine](https://github.com/timescale/memory-engine) (đọc ở
commit `9c9bdc1`, ngày 23/10/2026) vào memory của ha. Việc chia làm bảy phase, và
mỗi mục ghi rõ phase nào làm. Khi một phase chưa ship thì code là chuẩn, không
phải tài liệu này.

Bản 0.8.1 gồm P1 tới P4. P5 (CLI, định dạng, pack), P6 (importer) và P7 (MCP
server, tích hợp, console) tạm hoãn tới khi maintainer làm tiếp. P1 chỉ thêm đúng
một lệnh CLI mà test của nó cần: `ha memory export`.

Bản 0.8.2 bổ sung những gì lần rà soát thứ hai so với ME còn thiếu ở P1 tới P4
(§13):

- name duy nhất theo tree và được giữ qua các lần cập nhật;
- cập nhật kiểu patch;
- `memory_id` không đổi dùng làm địa chỉ;
- history phân trang, đủ mọi trường;
- stemming tiếng Anh;
- `%` của lquery theo đúng PostgreSQL;
- các method JSONPath của PostgreSQL 17;
- bộ lọc áp lên trang knowledge;
- đọc nguyên văn phiên bản trang cũ;
- action `status`.

## 1. Mục tiêu và quyết định

memory-engine (ME) là server memory dùng chung cho nhóm, chạy trên Postgres với
`ltree`, `pgvector` và BM25; agent dùng nó qua MCP. ha hiện giữ:

- memory trong các file `harness_state.json`;
- knowledge dưới dạng trang Markdown;
- một history index trong SQLite.

Mục tiêu: mọi khả năng của ME mà một người dùng đơn lẻ dùng được thì ha cũng có,
chạy cục bộ.

Maintainer đã quyết:

| Chủ đề | Quyết định |
|---|---|
| Chia sẻ | **Chưa làm.** ha chỉ chạy cục bộ, một người dùng: không có space, principal, group, grant, API key, login, invite hay backend từ xa. Tính lại sau. |
| Lưu memory | **Mỗi scope một kho SQLite**, theo mô hình record của ME: tree, name, meta, temporal, version, event log. Thay cho `harness_state.json`. |
| Theo nghĩa | **Không dùng embedding (0.8.8).** Search và digest xếp theo từ, rồi theo entry được dùng gần đây và nhiều đến đâu (§14). Không gì phải chờ một lần gọi mạng, và không gì rời khỏi máy. |
| Knowledge | **Vẫn là file Markdown**, người dùng sửa tay được. Có thêm lịch sử trang và revert, và dùng chung một search với memory. |
| Bộ lọc | **Đủ như ME:** tree theo đường dẫn chính xác, lquery và ltxtquery; `meta` kiểu "chứa"; `metaPredicate` (JSONPath); temporal 5 kiểu; `grep`. |

Những thứ ha giữ lại vì ME không có:

- digest chèn vào mỗi lượt, có pinned và constraints;
- review tự động, Deep Dream và curator skill;
- từ chối bản ghi trùng khi ghi;
- nhiều cách diễn đạt câu hỏi (`queries`);
- đồ thị liên kết knowledge.

ME cố ý không có lớp tự trích xuất; ở ha lớp đó vẫn còn và ghi qua kho mới.

## 2. Bản đồ tính năng

Trạng thái trước khi làm: ✅ có, 🟡 một phần, ❌ chưa có. P1 tới P7 là các phase
ở §12. Từ 0.8.2 mọi dòng tới P4 đều đã có; §13 liệt kê những gì lần rà soát sau
0.8.1 tìm ra và 0.8.2 đã đổi gì.

| Tính năng ME | Hiện tại | Phase |
|---|---|---|
| `content`, version, CAS | ✅ | P1 (giữ) |
| `tree` phân cấp, gọi theo `tree/name`, id không đổi | 🟡 | P1 |
| Object `meta` tự do; khóa dành riêng `$prev`/`$next`/`$thread`, `display_name` | ❌ | P1 |
| `temporal` (một thời điểm hoặc một khoảng) | ❌ | P1 (lưu) · P3 (truy vấn) |
| `on_conflict` error / replace / ignore | 🟡 | P1 |
| Event log chỉ ghi thêm: snapshot, actor, cause, `operation_id` | 🟡 | P1 |
| Lịch sử theo memory, path, cây con, operation hoặc feed theo thời gian, có cursor | 🟡 | P2 |
| Revert và undelete thành một phiên bản mới | ✅ memory · ❌ knowledge | P1 · P4 |
| Giữ log theo thời gian (30 ngày) | ❌ | P1 |
| `tree` (số lượng mỗi nút), `count`, `mv`, `copy`, `delete_tree`, get/delete theo path | ❌ | P2 |
| BM25, chỉ trả kết quả thật sự khớp từ khóa | 🟡 | P3 |
| Semantic cho memory, với hàng đợi embedding chạy nền | ❌ | P3 · bỏ ở 0.8.8 (§14) |
| Hybrid RRF có trọng số; `weights`, `candidateLimit`, `semanticThreshold` theo từng lần gọi | 🟡 | P3 · từ 0.8.8 chỉ còn `candidate_limit` |
| Bộ lọc `tree`, `meta`, `metaPredicate`, `temporal`, `grep`; duyệt chỉ bằng lọc, có `order_by` | ✅ (từ 0.8.1) | P3 |
| `select` (chọn trường) và `format` yaml/json/compact | 🟡 | P3 |
| Lịch sử trang knowledge, revert, undelete | ❌ | P4 |
| Một search cho cả memory và knowledge | ❌ | P4 |
| Bộ lọc history (`since`, `until`, `kind`); liên kết thread khi `history_read` | ✅ (từ 0.8.1; `until` từ 0.8.2) | P4 |
| CLI memory, có `edit` | ❌ | P5 |
| Import/export JSON, YAML, Markdown, NDJSON | ❌ | P5 |
| Memory pack (validate, install, list) | ❌ | P5 |
| Import tài liệu (`--git-aware`, `--prune`) | 🟡 | P6 |
| Import lịch sử git | ❌ | P6 |
| Import phiên Claude Code, Codex, OpenCode | ❌ | P6 |
| MCP server cung cấp tool memory cho harness khác | ❌ | P7 |
| Cài tích hợp vào harness khác, có hook capture | ❌ | P7 |
| Web UI cho memory | 🟡 chỉ knowledge | P7 |
| `doctor` / `status` cho memory | 🟡 | P7 |

Ngoài phạm vi:

- space, principal, group, grant, API key, service account, invite, login;
- workflow import qua CI, backend từ xa;
- importer Granola (chỉ có trên macOS) và Slab (dịch vụ ít dùng).

## 3. Lưu trữ (P1)

### 3.1 File

Mỗi thư mục scope có `memory.sqlite3` thay cho `harness_state.json`:

| Scope | Thư mục | Gốc tree của record |
|---|---|---|
| global | `<data>/harness/` | `/global/...`; entry gắn với một project nằm ở `/projects/<project_key>/...`, key của repository chứa project ([định danh project](PROJECT_IDENTITY.vi.md)) |
| local (một cuộc trò chuyện) | `<data>/sessions/<task>/harness/` | `/local/...` |

Mỗi thư mục scope một database, không phải một database chung cho cả ha, vì:

- Một lượt chỉ đọc đúng hai kho: global và kho local của cuộc trò chuyện đó.
  Memory local của cuộc trò chuyện khác không bao giờ nằm trong phạm vi.
- Hai kho khóa độc lập với nhau. `knowledge.sh` có kiểm: giữ khóa global chỉ làm
  hỏng phần global của review.
- Test và tool trỏ vào một thư mục bất kỳ vẫn chạy được.

Khóa cho mỗi lần đọc-sửa-ghi đổi tên thành `memory.lock`, nằm cạnh database (hiện
là `harness_state.json.lock`). Database chạy WAL, busy timeout 5 s.
`PRAGMA user_version` ghi phiên bản schema; migration là các hàm Rust có thứ tự,
chạy khi mở database.

ha truy cập qua `rusqlite` 0.39. Bản này link cùng `libsqlite3-sys` 0.37 (SQLite
3.53.2 đã vendor) với `sqlx`, nên chỉ có một thư viện SQLite và gọi đồng bộ được.
Code memory vốn đồng bộ, nên không cần cầu nối sang runtime async.

### 3.2 Schema

```sql
CREATE TABLE memory (
  id              TEXT PRIMARY KEY,      -- UUIDv7, không đổi
  kind            TEXT NOT NULL,         -- prompt|memory|skill|subagent|factory|record
  key             TEXT NOT NULL,         -- id entry của ha (slug), duy nhất theo kind
  name            TEXT,                  -- name của ME, duy nhất trong tree (schema 3)
  tree            TEXT NOT NULL,         -- '/global/user/preferences'
  title           TEXT NOT NULL,
  content         TEXT NOT NULL,
  meta            TEXT NOT NULL DEFAULT '{}',   -- object JSON (`metadata` của entry)
  temporal_start  TEXT,                  -- RFC 3339 UTC; NULL = không có temporal
  temporal_end    TEXT,                  -- không tính đầu mút; = start nếu là một thời điểm
  doc             TEXT NOT NULL,         -- toàn bộ entry như ha ghi (không mất trường nào)
  version         INTEGER NOT NULL,
  version_hash    TEXT NOT NULL,         -- 32 ký tự hex của SHA-256 trên payload chuẩn hóa
  content_version INTEGER NOT NULL,      -- tăng khi title hoặc content đổi
  created_at      TEXT NOT NULL,
  updated_at      TEXT NOT NULL,
  UNIQUE (kind, key)
);
CREATE INDEX memory_tree ON memory(tree);
CREATE INDEX memory_path ON memory(kind, tree, name);
CREATE INDEX memory_temporal ON memory(temporal_start, temporal_end);

CREATE TABLE memory_event (
  event_id     TEXT PRIMARY KEY,   -- UUIDv7: thứ tự thời gian tuyệt đối
  at           TEXT NOT NULL,
  memory_id    TEXT NOT NULL,      -- còn lại sau khi memory bị xóa
  kind TEXT NOT NULL, key TEXT NOT NULL, name TEXT, tree TEXT NOT NULL,
  operation    TEXT NOT NULL,      -- insert|update|delete
  operation_id TEXT NOT NULL,      -- một giá trị cho mỗi transaction ghi
  cause        TEXT,               -- create|update|delete|move|copy|revert|import|migrate|review|dream|curate|rollback|...
  actor        TEXT NOT NULL,      -- JSON: {"type": model|refine|kernel|migration|cli|mcp|import, ...origin}
  version      INTEGER NOT NULL,
  doc          TEXT NOT NULL       -- snapshot trạng thái sau thay đổi (với delete: trạng thái bị xóa)
);
CREATE INDEX memory_event_memory ON memory_event(memory_id, at, event_id);
CREATE INDEX memory_event_operation ON memory_event(operation_id);
CREATE INDEX memory_event_tree ON memory_event(tree);
CREATE INDEX memory_event_path ON memory_event(tree, name);
CREATE INDEX memory_event_at ON memory_event(at DESC);

CREATE TABLE refinement (seq INTEGER PRIMARY KEY, event TEXT NOT NULL);  -- mảng `refinements`
CREATE TABLE store_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);     -- `schema` và các trường cấp gốc khác
-- P3 (schema 2): memory_vector, memory_embedding_queue (schema 4 xóa đi)
-- 0.8.2 (schema 3): cột `name` của V1 đổi tên thành `key`, và thêm cột `name` (§13)
-- 0.8.8 (schema 4): thêm memory_usage, xóa hai bảng embedding (§14)
CREATE TABLE memory_usage (
  memory_id     TEXT PRIMARY KEY,   -- xóa cùng memory của nó
  reads         INTEGER NOT NULL DEFAULT 0,  last_read_at  TEXT,
  hits          INTEGER NOT NULL DEFAULT 0,  last_hit_at   TEXT,
  shown         INTEGER NOT NULL DEFAULT 0,  last_shown_at TEXT
);
```

Entry của ha ánh xạ sang record của ME như sau:

- **key.** Là id entry của ha, khóa của document, duy nhất theo kind trong một
  kho. Key là name nếu name còn trống, nếu không thì là name thêm hậu tố `-2`,
  `-3`. Khi key và name đang trùng nhau, key đi theo name mới và giữ key cũ làm
  alias.
- **name.** Là name của ME. Từ 0.8.2 name duy nhất trong tree như ME, nên cùng một
  name có thể nằm ở hai tree.
  - Nguồn: name do lệnh ghi đưa vào (theo regex của ME), nếu không có thì là slug
    từ title.
  - Đổi title: name tạo từ title thì đi theo title mới; name được đặt, hoặc name
    khác với title, thì giữ nguyên.
  - Entry ghi trước 0.8.2 lấy key làm name.
  - `<tree>/<name>` là địa chỉ của entry.
- **tree.** Là gốc của scope nối với `path` của entry, đổi thành các nhãn:
  - mỗi đoạn bỏ dấu (NFKD rồi bỏ dấu);
  - ký tự ngoài `[A-Za-z0-9_-]` thành `_`;
  - bỏ đoạn rỗng;
  - path rỗng thành `general`.

  `path` gốc được giữ trong `doc`. Tree nằm dưới `/projects/<key>` chính là scope
  "project" hiện nay; record không gắn project là global.
- **meta.** Chính là object `metadata` mà entry đang có, nên `pinned` là
  `meta.pinned`. Khóa dành riêng giống ME: `$prev`, `$next`, `$thread`,
  `display_name`, `pinned`. Cập nhật thì `meta` bị thay cả object, giống ME.
- **temporal.** Một thời điểm (chỉ có `start`) hoặc một khoảng `[start, end)`.
  Đầu vào có thể là một chuỗi, một mảng 1–2 chuỗi, hoặc `{start, end?}`.
- **doc.** Giữ mọi thứ ha ghi, kể cả trường lạ, để lớp tương thích ở §3.3 đọc ra
  đúng y như đã ghi.

### 3.3 Lớp tương thích dạng document

Hiện code đọc và ghi state qua ba chỗ:

- `harness::load`, dùng cho digest;
- `refine::load_raw` và `refine::save_raw`, dùng cho tool `memory`, review, Deep
  Dream, rollback và `/learn`;
- `harness_kernel::Store`, dùng cho `rlm.harness`.

Ba chỗ này giữ nguyên dạng document JSON:

- `load_raw(dir)` dựng `{"schema", "entries": {kind: {name: doc}}, "refinements"}`
  từ các dòng trong bảng.
- `save_raw(dir, &state, &Writer)` so document mới với các dòng hiện có, rồi trong
  một transaction:
  - chèn, sửa, xóa đúng những dòng thay đổi;
  - ghi mỗi dòng thay đổi một event, tất cả chung một `operation_id`;
  - thay bảng `refinement` nếu nó đổi;
  - tăng bộ đếm thay đổi memory (`memory_changes`) như hiện nay.
- `Writer { actor, cause }` là tham số mới và bắt buộc: mỗi chỗ gọi phải nói ai
  đang ghi:
  - `model`: tool `memory`;
  - `refine`: mọi lượt refine (`/refine`, review tự động, Deep Dream, curator),
    kèm id của refinement (giải thích trong `refinements.jsonl`), cause là
    `refine`, hoặc `rollback` khi rollback;
  - `kernel`: `rlm.harness`;
  - `migration`: nhập file cũ;
  - `cli` và `mcp` khi các phần đó có.

Tính năng mới (bộ lọc, lịch sử, thao tác trên cây, search) truy vấn thẳng các
dòng. Lớp document chỉ phục vụ các chỗ gọi cũ, nên ở P1 hành vi của chúng không
đổi.

### 3.4 Chuyển dữ liệu và file cũ

Khi mở một kho mà thư mục còn file `harness_state.json`, các entry trong file được
nhập vào:

- upsert theo `(kind, name)`;
- payload khác thì thay bản đang lưu;
- refinement được nối thêm, trừ khi `id` đã có.

Sau đó file đổi tên thành `harness_state.json.imported`. Cùng lượt đó,
`memory_history.jsonl` được chuyển thành các event `migrate` rồi cũng đổi tên.
Record trong file là các trạng thái cũ, nên mỗi record thành một snapshot ở đúng
`version` của nó, và `history`/`revert` vẫn tới được các phiên bản ghi trước khi
nâng cấp.

Nhờ vậy, lần mở đầu tiên sau khi nâng cấp ha tự chuyển memory của người dùng. Quy
tắc này cũng là cách nạp entry cho test và người dùng: đặt một file
`harness_state.json` vào thư mục, lần mở sau nó sẽ được nhập.

Một lần đọc cũng mở kho theo cách đó (`Store::existing`, nằm sau `load_document`):
scope không có kho lẫn file cũ thì được để nguyên, nhưng kho có sẵn thì được
migrate lên schema này, còn chỉ có file cũ thì được tạo kho và nhập file vào. Lần
đọc như vậy có ghi. Lần đọc nào không được đổi gì - snapshot của bug report, dry
run của `purge`, store meta mà `ha doctor` đọc - thì mở database ở chế độ chỉ đọc.

Muốn quay về bản ha cũ: `ha memory export --legacy` (P5) ghi ra một
`harness_state.json` mà 0.8.x đọc được. File `.imported` là trạng thái trước lúc
nâng cấp.

### 3.5 Thời gian giữ event log

Event cũ hơn 30 ngày bị xóa, giống ME. Ngoại lệ: 10 phiên bản mới nhất của mỗi
memory luôn được giữ, đúng số `history` đang hiển thị. Nhờ vậy, một memory không
đổi suốt một tháng vẫn revert được.

Việc dọn chạy tối đa mỗi giờ một lần, trong một lần ghi. History và revert chỉ
với tới được những gì log còn giữ.

## 4. Ghi và định danh (P1)

Action `write` của tool `memory` có thêm:

- `name`: đặt slug cụ thể, duy nhất trong tree;
- `tree`: đường dẫn tuyệt đối (`/global/...`, `/projects/<project này>/...`,
  `/local/...`, đồng thời chọn luôn scope) hoặc tương đối theo `scope` (chính là
  `path` hiện nay, vẫn được chấp nhận);
- `meta`: một object;
- `temporal`: `{start, end?}`;
- `on_conflict`: `error` | `replace` | `ignore`, theo khóa `(kind, tree, name)`.

`replace` có so nội dung: payload y hệt thì không làm gì và không tăng version.
Ngược lại, entry trở thành đúng như lệnh ghi, kể cả `meta` và `temporal`, giống
ME. Cơ chế từ chối entry trùng của ha (độ giống 0,6 hoặc chứa 85 %) vẫn chạy
trước `on_conflict` với entry mới, và `distinct` vẫn bỏ qua được nó.

Cập nhật vẫn dùng `expected_version`, và nhận thêm `version_hash` như ME. Từ
0.8.2, cập nhật là một patch:

- chỉ đổi những gì được đưa vào: title, content, name, tree hoặc path, meta,
  temporal, pinned; phần còn lại giữ nguyên, nên có thể bỏ `content`;
- cập nhật không đổi gì thì giữ nguyên version;
- `tree` tuyệt đối thuộc scope khác thì chuyển entry sang kho của scope đó, giữ
  nguyên id bất biến;
- kết quả ghi trả về `name`, `tree`, `version_hash` và `memory_id` của entry.

Có ba cách gọi tới một entry: key, `<tree>/<name>`, hoặc `memory_id`. `memory_id`
là UUID do kho cấp, không bao giờ đổi; list và search đều hiển thị nó.

`restore` đổi thành `revert`, và `restore` vẫn dùng được như một tên gọi khác:

- nó áp lại một snapshot từ event log thành phiên bản mới, với cause `revert`;
- revert một memory đã xóa sẽ tạo lại nó với id cũ, version đánh số tiếp;
- revert vào một key, hoặc một `(tree, name)`, mà entry khác đang giữ là xung
  đột.

Digest ở P1 không đổi. Chỉ thêm một điều: entry có khoảng temporal đã kết thúc
(`temporal_end <= now`) không được đưa vào digest, vì đó là lịch sử chứ không còn
là sự thật hiện tại. Search vẫn tìm thấy chúng. Từ 0.9.0, entry bị một lần sửa
hoặc lượt hết hạn đóng lại cũng không có trong search, trừ khi search nêu một
thời điểm hoặc `include_ended` (§16).

## 5. Sắp xếp và lịch sử (P2)

Các action mới của tool `memory`. Agent con chỉ có các action chỉ đọc.

| Action | Tương đương ME | Ghi chú |
|---|---|---|
| `get` với `path` (`tree/name`) | `get_by_path` | Tên trùng giữa các kind thì phải ghi `kind` |
| `tree` (`tree`, `levels`) | `me_memory_tree` | Các nút kèm số lượng, global và local gộp chung |
| `count` (lọc `tree`, `max_count`) | `me_memory_count` | Nhận tree, lquery hoặc ltxtquery như search |
| `move` (`source`, `destination`, `dry_run`) | `me_memory_mv` | Thay tiền tố. Chuyển giữa hai gốc scope thì chuyển dòng sang kho kia, chung một `operation_id` |
| `copy` (`source`, `destination`, `dry_run`) | `me_memory_copy` | Bản sao có id mới và giữ name; tree đích đã có name đó thì xung đột và không sao chép gì |
| `delete` với `path` | `delete_by_path` | |
| `delete_tree` (`tree`, `dry_run`) | `me_memory_delete_tree` | |
| `history` | `me_memory_history` | **Có `id`** (key, path hoặc `memory_id`): các phiên bản của entry. Mỗi phiên bản kèm tree, name, meta, temporal, cause và `operation_id`. Mỗi trang `limit` phiên bản (mặc định 10, tối đa 100), bắt đầu từ `offset`. **Không có `id`:** feed, giới hạn theo `tree`, `operation_id`, `memory_id` hoặc `since`. Thêm được `until`, `operation`, `order`, `cursor` (keyset theo `event_id`), `limit` (tối đa 1000) |
| `status` | `memory.embeddingStatus` | Số entry mỗi scope, và `search: words`; một ghi chú khi còn sót `.ha-embedding.json` cũ (0.8.8) |

Mỗi refinement ghi lại `operation_id` của lần ghi, để `/refine --rollback` và
`history operation_id=…` khớp nhau về những gì một lượt review đã đổi.

## 6. Tìm kiếm (P3)

### 6.1 Theo từ khóa

BM25 chạy trong `memory/search.rs` trên các entry mà search xét, sau khi lọc. Kho
global và kho của cuộc trò chuyện được xếp hạng như một kho chung, với cùng một
bộ thống kê. Vì kho nhỏ, cách này đơn giản hơn giữ một index FTS5 đồng bộ; khi
kho lớn lên thì có thể thêm index sau.

Từ được tách theo cách của ha:

- chữ thường, NFC;
- chuỗi CJK tách thành bigram;
- câu hỏi không dấu thì đọc entry đã bỏ dấu, nên `may in` tìm ra `máy in`; câu
  hỏi có dấu thì đọc đúng như đã viết.

Một từ trong câu hỏi khớp các từ bắt đầu bằng nó, nên "deploy" vẫn tìm ra
"deploys" như cách khớp chuỗi con trước đây. Từ 0.8.2, nó còn khớp các từ cùng
gốc tiếng Anh (Snowball, như cấu hình `english` của PostgreSQL mà ME dùng), nên
"deploying" tìm ra "deployed". Không có danh sách stopword: ha đánh giá từ theo
thống kê, không theo danh sách.

Trọng số trường: title 3, content 1, tree và name 1, giá trị meta 0,5. Chỉ entry
chứa ít nhất một từ của câu hỏi mới được trả về: điều kiện "phải thật sự khớp"
của ME. Các cách diễn đạt (`queries`) được gộp bằng RRF ngay trong nhánh này.

Search khớp theo từ, không theo nghĩa. Kết quả rỗng ghi rõ các query đã tìm và
khuyên tìm lại bằng từ khác - từ đồng nghĩa, ngôn ngữ còn lại mà project dùng,
một từ ngắn hơn hoặc rộng hơn, một tên hay đường dẫn trong code - trước khi nói
là không nhớ. Kết quả rỗng còn liệt kê `branches`: các nhánh cây chứa những entry
đã được tìm (tối đa 12, nhánh nhiều entry nhất trước). Tên nhánh là từ của chính
kho, nên model thấy kho được viết bằng ngôn ngữ nào và có thể duyệt một nhánh
bằng `tree` mà không cần query. Một câu hỏi bằng tiếng Anh vào kho ghi bằng
tiếng Việt chỉ được tìm lại bằng từ đồng nghĩa tiếng Anh, cho đến khi các nhánh
được hiện ra. Model tự chọn từ; ha không giữ bảng từ đồng nghĩa nào.

### 6.2 Theo nghĩa (bỏ ở 0.8.8)

Phần dưới là cách 0.8.1 đến 0.8.7 tìm theo nghĩa. 0.8.8 đã bỏ: các bảng bị xóa
(§14), và `semantic`, `semantic_threshold`, `weights`, `has_embedding` không còn
trong tool.

Memory dùng endpoint embedding đã cấu hình cho thư viện knowledge global
(`/knowledge semantic --global`). Chưa cấu hình thì không embed gì.

| Bảng | Nội dung |
|---|---|
| `memory_vector` | `memory_id`, `content_version`, `namespace` (endpoint + model), vector |
| `memory_embedding_queue` | Một dòng cho mỗi `(memory_id, content_version)` |

Một entry cần vector khi chưa có vector cho phiên bản nội dung hiện tại trong
namespace hiện tại. Bảng hàng đợi chỉ ghi các lần thử thất bại. Việc embed chạy
trong một thread nền sau mỗi lần ghi qua tool `memory`, và tối đa 20 s ngay trước
một lần search theo nghĩa.

Lúc ghi kết quả có kiểm `content_version`, nên vector cũ không bao giờ ghi đè.
Lỗi thì thử lại 3 lần; gặp 429 thì hoãn dòng đó mà không tính một lần thử.

Điểm là cosine tính đủ trên mọi vector: kho của ha đủ nhỏ để không gặp các giới
hạn độ phủ của HNSW như ME. Kết quả là độ tương đồng cosine trong `[-1, 1]`.
`semantic_threshold` nằm trong `[0, 1]`; giá trị ngoài khoảng bị từ chối chứ
không bị kẹp lại.

### 6.3 Hybrid (bỏ ở 0.8.8)

Đến 0.8.7, Reciprocal Rank Fusion có trọng số, giống ME:

```text
score = w_fulltext / (60 + rank_fulltext) + w_semantic / (60 + rank_semantic)
```

| Tham số | Mặc định và giới hạn |
|---|---|
| `weights` | 1 / 1, mỗi giá trị trong [0, 1] |
| `candidate_limit` mỗi nhánh | 30, ít nhất bằng `limit`, tối đa 1000 |
| `limit` | 10, tối đa 200 |

Nhánh nào không có kết quả thì góp 0; điểm bằng nhau thì xếp theo id. Trọng số
0 tắt nhánh đó: `weights {semantic: 0}` chỉ xếp theo từ, với điểm BM25, và báo
khi không từ nào khớp.

Các cách diễn đạt (`queries`) được gộp trong từng nhánh trước, bằng RRF thường
như hiện nay.

`harness_types::search::hybrid_fuse` được thay bằng cách gộp này, nên search
knowledge và history cũng được thưởng khi hai nhánh cùng thấy một kết quả. Hiện
nay chúng lấy trung bình thứ hạng đã chuẩn hóa, nên kết quả đứng đầu của chỉ một
nhánh lại thắng kết quả mà cả hai nhánh cùng tìm thấy.

Điểm của mỗi chế độ giữ đúng nghĩa của nó, giống ME:

| Chế độ | Điểm |
|---|---|
| fulltext | BM25 > 0 |
| semantic | cosine |
| hybrid | RRF |
| chỉ lọc | -1 |

Từ 0.8.8 search chỉ còn hai mode: `fulltext` (BM25, thứ tự có tính cả độ mới và
mức dùng, §14) và `filter`.

### 6.4 Bộ lọc

Dùng được với mọi chế độ và kết hợp với nhau theo AND. Chúng chạy bằng Rust trên
các dòng ứng viên; trước đó SQL đã thu hẹp ứng viên bằng tiền tố tree và index
temporal.

| Bộ lọc | Ý nghĩa |
|---|---|
| `tree` | **Đường dẫn chính xác:** nút đó và mọi nút con. **lquery,** khi có `* ! { } \| @ %`: `*`, `*{n}`, `*{n,m}`, `!nhãn`, `a\|b`, `nhãn@` (không phân biệt hoa thường), `nhãn%` (mỗi từ của mẫu khớp một từ nào đó, tách bằng `_`, của nhãn, không cần đúng thứ tự, như `compare_subnode` của PostgreSQL), `nhãn*` (tiền tố). **ltxtquery,** khi có `&`: Boolean `& \| !` trên các nhãn, cùng các bổ ngữ trên. Nhãn nối bằng `/`, hoặc bằng `.` như ltree của ME; mẫu không bắt đầu bằng `/` còn khớp tree tính từ dưới gốc scope. |
| `meta` | "Chứa" kiểu JSON, theo ngữ nghĩa `@>` của PostgreSQL: object khớp object con, mảng phải chứa các phần tử đã cho. |
| `meta_predicate` | Vị từ Boolean SQL/JSON path, xem bên dưới. |
| `temporal` | `before`, `after` (nghiêm ngặt, loại dòng không có temporal), `contains`, `overlaps {start,end}`, `within {start,end}`. Nhiều kiểu thì AND lại. Khoảng là `[start, end)`; một thời điểm là `[t, t]`. |
| `grep` | Regex không phân biệt hoa thường (crate `regex`) trên content. Giống ME, phải đi kèm search, `tree`, `meta` hoặc `temporal`; `meta_predicate` một mình không đủ. |

`meta_predicate` theo cú pháp SQL/JSON path của PostgreSQL cho vị từ:

- `$`, `@`, `.key`, `."key có ngoặc"`, `[*]`, `[n]`, `[n to m]`, `last`, `.**`;
- bộ lọc `? ( … )`;
- `== != < <= > >=`, `&& || !`, `exists( … )`, `like_regex "…" flag "i"`,
  `starts with`, `is unknown`;
- số học `+ - * / %` và dấu trừ một ngôi;
- `.**`, và `.**{n}`, `.**{n to m}` cùng `last`;
- escape trong chuỗi, kể cả `\uXXXX`;
- các method `.size() .type() .double() .ceiling() .floor() .abs() .keyvalue()`.
  Thêm các method của PostgreSQL 17, không nhận đối số: `.datetime() .date()
  .timestamp() .timestamp_tz() .string() .number() .decimal() .integer()
  .bigint() .boolean()`.
  - Ngày giờ được đổi sang RFC 3339 UTC, nên hai giá trị so sánh đúng thứ tự thời
    gian.
  - Method khác bị từ chối ngay lúc parse. Nếu cho qua rồi không khớp gì, kết quả
    sẽ trông như không entry nào có facet đó;
- chế độ lax như ME dùng, trong đó lỗi cấu trúc cho kết quả false.

Bộ parser và evaluator tự viết trong `memory/filters.rs`.

Không có văn bản search thì bộ lọc dùng để duyệt: mới nhất trước, hoặc theo
`order_by` `asc`/`desc`. Mọi kết quả đều có `total`, đếm theo kiểu `max_count`
tới 10 000.

### 6.5 Trình bày

| Tham số | Cách dùng |
|---|---|
| `select` | Các trường cần trả: `id`, `memory_id`, `name`, `tree`, `kind`, `title`, `meta`, `meta.<khóa>` (nằm trong `meta`; khóa không có thì bỏ, như ME), `temporal`, `version`, `version_hash`, `score`, và lát cắt content `content:N`, `content:M:N`, `content:M:` (tính theo ký tự, kèm `content_length`). Dùng được với `search`, `read` và `history`. |
| `format` | `json` (mặc định của tool, dạng gọn), `compact`, hoặc `yaml`; dùng được với `search`, `read` và `history`. |

`format: yaml` dùng một bộ xuất nhỏ trong `memory/search.rs`, đặt ngoặc kép cho
mọi chuỗi mà YAML có thể đọc thành thứ khác. Đọc YAML (pack, import) cần một
parser: crate YAML cho serde sẽ được chọn và ghim khi làm tiếp P5.

Đến 0.8.7 `semantic` là điều muốn tìm theo nghĩa; 0.8.8 bỏ nó cùng embedding.

## 7. Knowledge và history (P4)

### Lịch sử trang knowledge

Mỗi thư viện có thêm `.ha-history.sqlite3`. Bảng `page_event` ghi mỗi lần ghi và
xóa:

- `event_id`, `at`, `path`;
- `operation`, `operation_id`, `cause`, `actor`;
- `hash`, `title`, `summary`, `sources`, `related` và toàn bộ nội dung trang.

Event được ghi ngay sau khi trang được commit, vẫn trong khóa của thư viện. Nếu
không ghi được lịch sử thì lỗi được báo trong diagnostics, còn lần ghi trang vẫn
giữ nguyên. Lần thay đổi đầu tiên được ghi lại của một trang cũng lưu văn bản mà
nó thay thế (cause `baseline`), nên trang viết trước khi có lịch sử vẫn revert về
được.

Tool `knowledge` có thêm:

- `history`: một trang, một thư mục, hoặc feed theo thời gian. Lọc được bằng
  `since`, `until`, `operation`, và phân trang bằng `cursor` riêng cho từng
  scope. Kèm `version` thì trả nguyên văn phiên bản đó, để đọc trước khi revert;
- `revert` (`path`, `version` = id của event).

Revert một trang đã xóa sẽ tạo lại trang đó, theo cùng luật backlink như khi ghi.
Thời gian giữ giống memory: 30 ngày, luôn giữ 10 phiên bản mới nhất của mỗi
trang.

### Một search cho cả hai

`search` của memory nhận `sources: ["memory", "knowledge"]`; mặc định chỉ tìm
memory. Kết quả knowledge lấy từ engine knowledge (BM25) rồi gộp bằng RRF thành một danh sách. Mỗi kết quả ghi rõ `source`, và với
trang knowledge thì có `path` cùng khoảng dòng. CLI làm tương tự với
`--knowledge`.

Bộ lọc áp lên trang theo cách ME đọc một record không có meta và không có
temporal:

- bộ lọc `meta`, `meta_predicate` hoặc `temporal` loại mọi trang;
- bộ lọc `tree` coi trang nằm ở `/knowledge/<scope>/<các thư mục của nó>`;
- `grep` đọc toàn văn trang;
- `select` chọn trường của trang.

Kết quả ghi rõ việc này.

### Tool history

- **`history_search`** có thêm:
  - `since` và `until`: ID của nguồn là `event_<UUIDv7>` của event trong journal,
    phần đầu chính là thời điểm, nên một khoảng thời gian là một khoảng ID và
    không cần thêm cột;
  - `kind`, một loại event của journal hoặc một danh sách;
  - `order`, `newest` (mặc định) hoặc `oldest` trước khi điểm bằng nhau.
- **`history_read`** trả thêm `previous_source_id` và `next_source_id` trong cùng
  phiên, theo `sequence`, để đọc nối tiếp mà không phải tìm lại.

## 8. CLI, định dạng và pack (P5)

### `ha memory`

`ha memory <action>`, giống `me memory`:

- `create`, `get`, `search`, `update`, `delete`, `deltree`;
- `history`, `revert`, `edit`;
- `count`, `tree`, `copy`, `move`;
- `import`, `export`.

Ngoài ra có `ha memory pack validate|install|list`.

| Tùy chọn | Ý nghĩa |
|---|---|
| `--cwd DIR` | Project, như `ha knowledge` |
| `--global` | Chỉ entry global |
| `--conversation ID` | Kho local của một cuộc trò chuyện |
| `--json`, `--yaml` | Định dạng đầu ra |
| `--select` | Các trường cần hiện |

Mọi lần ghi từ CLI được ghi nhận với actor `cli`.

`edit` mở `$VISUAL` / `$EDITOR` (trên Windows là Notepad nếu chưa đặt) với một file
Markdown: front matter YAML cho tree, name, meta, temporal, rồi tới nội dung. Lưu
file thì ghi lại kèm `expected_version`.

### Định dạng

Giống `formats.md` của ME:

- mảng JSON hoặc một object;
- NDJSON (`.ndjson`, `.jsonl`; chỉ để import);
- mảng YAML hoặc một object;
- Markdown, mỗi file một memory, có front matter YAML.

Định dạng lấy theo đuôi file hoặc tự nhận diện. `temporal` nhận cả ba dạng đầu
vào. Record không có tree vào `/global/imported`.

Import dùng `on_conflict: ignore`, nên import lại không đổi gì. Export dùng bộ lọc
§6.4 và ghi ra file hoặc thư mục (Markdown thì mỗi memory một file). Mỗi lần gọi
tối đa 200 record; trên CLI dùng `--all` để lấy nhiều hơn. `--legacy` ghi ra
`harness_state.json` mà bản 0.8 đọc được.

### Pack

Định dạng giống ME:

- mảng YAML các record, có header dạng comment `# Pack:`, `# Version:`,
  `# Description:`, `# ID prefix:`;
- id UUIDv7 cố định;
- `meta.pack {name, version}`;
- tree dưới `/global/pack/<tên>/...`.

`install` xóa record của phiên bản pack cũ rồi chèn bản mới. `validate` chạy
offline.

## 9. Importer (P6)

| Lệnh | Ghi vào | Nội dung |
|---|---|---|
| `ha memory import docs [dir]` | Trang knowledge trong thư viện project, dưới `docs/<đường dẫn tương đối>`, `origin: import:docs`, `sources: [đường dẫn trong repo]` | Các tùy chọn của ME: `--git-aware`, `--include`, `--exclude`, `--temporal-key`, `--prune` (chỉ xóa trang do importer này ghi), `--dry-run`. Front matter là dữ liệu của tài liệu. Trang quá cỡ bị cắt và đánh dấu. |
| `ha memory import git [repo]` | Dòng `record` tại `/projects/<key>/git_history/<sha>` | Tiêu đề, thân commit và tối đa 50 file thay đổi kèm số dòng; temporal = ngày commit; `$prev` theo cha thứ nhất; khóa metadata như ME; import tăng dần từ commit mới nhất đã có; `--branch`, `--since`, `--until`, `--max-count`, `--full`, `--no-merges`, `--no-file-list`. |
| `ha memory import claude\|codex\|opencode` | Dòng `record` tại `/projects/<key>/agent_sessions/<phiên>/msg_<id>` | Các cờ như ME (`--project`, `--since`, `--until`, `--full-transcript`, `--include-temp-cwd`, `--include-trivial`, `--include-sidechains`, `--dry-run`); `$prev` và `$thread`; metadata `source_*` của ME; `on_conflict: replace` kèm `importer_version`. |

`record` là kind mới: tìm và lọc được, nhưng **không bao giờ vào digest**, để hàng
nghìn commit không đẩy mất sở thích của người dùng.

Tài liệu được nhập vào knowledge thay vì memory, vì trang knowledge vốn đã là tài
liệu. Search chung ở §7 tìm tới được chúng dù nằm ở đâu.

## 10. Tích hợp (P7)

- **`ha mcp serve memory`.** MCP server qua stdio (rmcp, đã là dependency và có
  `server`), cung cấp bộ tool của ME dưới tên `ha_memory_*`:
  - `context`, `create`, `update`, `delete`, `delete_by_path`, `delete_tree`;
  - `search`, `get`, `get_by_path`, `count`, `tree`, `copy`, `mv`;
  - `import`, `export`, `history`, `revert`;
  - search knowledge.

  Server làm việc trên project của thư mục nó chạy. `--read-only` chỉ cung cấp các
  tool đọc.
- **`ha integrate install|uninstall claude|codex|opencode`.**
  - Đăng ký MCP server trên với harness đó.
  - Thêm `--capture` thì cài thêm hook stop/session-end của harness, chạy lệnh
    `ha memory import <harness>` tương ứng cho đúng phiên đó. Hook chạy bất đồng bộ
    và luôn thoát thành công.
  - ha ghi lại mọi file nó tạo vào `<data>/integrations.json`.
  - Gỡ cài chỉ xóa những file đã ghi lại mà chưa bị sửa.
  - Hook của Codex vẫn phải được người dùng duyệt trong Codex.
- **`ha memory serve`.** Web console knowledge, mở rộng thêm memory: duyệt cây,
  search có bộ lọc, lịch sử một entry kèm revert, và nút trước/sau theo `$prev`.
- **`ha doctor`.** Thêm phần memory: các kho và phiên bản schema, số entry và
  event, file cũ còn chờ nhập, các tích hợp đã cài.

## 11. Quy tắc tương thích

- Tool `memory`, `rlm.harness` của kernel, `/refine`, `/learn`, Deep Dream và
  digest chạy như hiện nay, trừ chỗ tài liệu này nói khác. Unit test của chúng vẫn
  pass qua lớp document.
- Agent con vẫn chỉ được đọc. Mỗi action mới đều được xếp vào loại đọc hoặc ghi.
- Bug report chứa bản export JSON của cả hai kho thay cho file thô, kèm số event.
- Không có gì rời khỏi máy. Embedding chỉ gọi endpoint người dùng đã cấu hình.

## 12. Các phase

Mỗi phase phải qua gate (`fmt`, `clippy`, `test`) và các suite live liên quan
trước khi sang phase sau (xem `AGENTS.md`). Một suite live mới, `memory-store.sh`,
gom các check về tương đương. Mỗi check phải fail trên 0.8.0 và pass trên bản mới,
và lần chạy đối chứng đó được báo cáo. Bump version và release do maintainer quyết.

| Phase | Nội dung | Suite live |
|---|---|---|
| **P1 Lõi** | §3, §4: kho SQLite, lớp document, chuyển dữ liệu, event log có actor/cause/operation, thời gian giữ log, `name`/`tree`/`meta`/`temporal`/`on_conflict`, revert dựa trên log, luật temporal cho digest | memory, kernel, knowledge, compaction, session-continuity, children, memory-store |
| **P2 Sắp xếp** | §5: get/delete theo path, tree, count, move, copy, delete_tree, feed lịch sử | memory, children, memory-store |
| **P3 Tìm kiếm** | §6: FTS5 BM25, vector và hàng đợi cho memory (bỏ ở 0.8.8), RRF có trọng số (cả knowledge và history), mọi bộ lọc, JSONPath, select/format, crate YAML | memory, knowledge, memory-store |
| **P4 Knowledge và history** | §7: lịch sử trang và revert, search chung, bộ lọc history và liên kết thread | knowledge, session-continuity, memory-store |
| **P5 CLI** | §8: `ha memory`, bốn định dạng, `--legacy`, pack | memory-store |
| **P6 Importer** | §9: docs, git, phiên Claude, Codex, OpenCode, kind `record` | memory-store |
| **P7 Tích hợp** | §10: MCP server, tích hợp harness và capture, web console, doctor | memory-store, cộng một lần chạy với MCP client thật |

Trước P1, các test live đọc và nạp thẳng `harness_state.json` (`kernel.sh`,
`knowledge.sh`, `memory.sh`, `session-continuity-evidence.py`). P1 chuyển việc nạp
sang quy tắc nhập file cũ ở §3.4, còn việc đọc chuyển sang
`ha memory export --json` (P1 làm sẵn một bản export tối thiểu cho việc này) hoặc
truy vấn `sqlite3` chỉ đọc.

## 13. Lần rà soát sau 0.8.1 (0.8.2)

Sau 0.8.1, maintainer yêu cầu rà soát lại lần nữa, độc lập, phần P1 tới P4 so
với mã nguồn của ME (không tính P5 tới P7). Những gì tìm ra và 0.8.2 đã làm:

| Tìm thấy | 0.8.2 |
|---|---|
| Cập nhật làm đổi tên entry đã có name riêng: name đi theo title ở mọi lần cập nhật, nên `<tree>/<name>` hỏng | Name chỉ đi theo title khi nó đang nói title; name được đặt thì giữ (§3.2) |
| Name duy nhất theo kho, nên hai tree không dùng chung được, và copy đổi tên entry | Name duy nhất theo tree (schema 3: `key` và `name`); copy giữ name, trùng thì báo xung đột (§5) |
| Cập nhật nào cũng bắt buộc `content` | Cập nhật là patch; cập nhật không đổi gì thì giữ version (§4) |
| `memory_id` mà feed hiển thị không dùng làm địa chỉ được | `memory_id` dùng được để đọc, ghi, phân trang history, revert, xóa; list và search hiển thị nó (§4) |
| History của entry chỉ hiện 10 phiên bản, chỉ có title và content | Đủ mọi trường của mỗi phiên bản, theo từng trang (§5) |
| Không chuyển riêng một entry sang scope khác được | `tree` tuyệt đối ở scope khác thì chuyển entry, giữ id (§4) |
| `replace` giữ meta và khoảng thời gian cũ khi lệnh ghi bỏ trống | `replace` thay toàn bộ entry, như ME (§4) |
| Đổi title vẫn giữ vector cũ | Title được embed cùng content, đổi title thì tăng `content_version` |
| Không có stemming tiếng Anh | Snowball tiếng Anh, như cấu hình `english` của PostgreSQL (§6.1) |
| Đã bật embedding thì không tìm thuần theo từ khóa được | Trọng số 0 tắt một nhánh (§6.3) |
| `%` khớp từ theo vị trí; tree chỉ nhận `/` | `%` theo `compare_subnode` của PostgreSQL; `.` nối nhãn; tree tương đối (§6.4) |
| Method JSONPath lạ âm thầm không khớp gì; không có `.datetime()`, `.**{n}`, `\u` | Method lạ bị từ chối; có các method chuyển kiểu, mức độ sâu và escape của PostgreSQL 17 (§6.4) |
| Search chung bỏ qua bộ lọc với trang knowledge | Bộ lọc áp lên cả trang (§7) |
| Không đọc được phiên bản trang cũ, không có `until` hay cursor | `history` với `version`, `until`, `operation`, `cursor` (§7) |
| Không có trạng thái embedding, không có `has_embedding` | `status`, và `has_embedding` trong `select` (§5, §6.5) |
| `select` và `format` chỉ có ở search; `meta.<khóa>` nằm phẳng | Có cả ở read và history; `meta.<khóa>` nằm trong `meta` (§6.5) |

Không làm:

- batch create của ME, vì chỉ importer của nó dùng (để P5);
- danh sách stopword của ME, vì ha không đánh giá văn bản theo danh sách từ;
- chế độ JSONPath `strict`, vì ME chạy lax.

Các dòng về vector và `has_embedding` mô tả 0.8.2; 0.8.8 đã bỏ cả hai (§14).

## 14. Từ khóa, độ mới và mức dùng (0.8.8)

0.8.8 bỏ semantic search ở mọi nơi: search memory, nhánh theo nghĩa của digest,
phần recall theo nghĩa của readout, search knowledge, search history, cache
embedding và `/knowledge semantic`. Nó vốn tắt nếu người dùng chưa cấu hình
endpoint, và khi bật thì có thể làm một lượt đứng chờ provider, hoặc loại vĩnh
viễn entry khi provider tạm hỏng. Thay vào đó, thứ hạng dựa vào:

**Mức dùng.** Schema 4 thêm `memory_usage`: theo memory id, `reads` và
`last_read_at` (`read` của tool memory), `hits` và `last_hit_at` (các kết quả một
lần `search` memory trả về, cả hai mode), `shown` và `last_shown_at` (các entry
digest của một lượt đã hiện, kể cả entry pinned). Mỗi đợt là một upsert trong
transaction riêng, chỉ cho id kho đang giữ; dòng bị xóa cùng entry. Một lần dùng
không bao giờ đụng document của entry hay event log, nên không thành một version
và không đẩy version có thể revert ra ngoài. Digest ghi các entry đã hiện ở một
thread nền, ngoài đường đi của lượt.

**Xếp hạng.** Digest và search memory xếp như nhau:

1. Constraint trước (ở digest), entry pinned đứng ngoài thứ hạng.
2. Entry khớp từ (BM25 lớn hơn 0), tốt nhất trước. Nhóm đầu - gấp đôi số được
   hiện: 12 ở digest, gấp đôi `limit` ở search - được gộp bằng RRF có trọng số
   với hai nhánh nhẹ hơn (mỗi nhánh 0,3, nhánh từ là 1):
   - **độ mới**: thời điểm muộn hơn giữa `updated_at` và `last_read_at`;
   - **mức dùng**: `reads`, giảm một nửa trọng số sau mỗi 30 ngày kể từ lần đọc
     cuối.

   Entry không có thời điểm hoặc chưa được đọc thì không vào nhánh đó. Phần còn
   lại của các entry khớp giữ thứ tự theo từ, nên entry chỉ vừa khớp không bao
   giờ vượt được nhóm tốt nhất.
3. Entry không khớp gì đứng sau mọi entry khớp, entry được ghi hoặc đọc gần nhất
   trước; khi không có từ nào, mọi entry xếp như vậy. Trước đây chúng xếp theo
   thứ tự chữ cái.

`hits` và `shown` được đếm nhưng không bao giờ dùng để xếp: chúng ghi lại cái
đã đặt trước model, không phải cái model đã dùng, và xếp theo chúng sẽ tự khuếch
đại. Khi bằng nhau, vị trí trước thắng (path, title và key ở digest), nên cùng
một trạng thái luôn xếp như nhau. `score` của một kết quả search vẫn là điểm
BM25.

**Nâng cấp.** Kho mở lên schema 4: `memory_vector` và `memory_embedding_queue`
bị xóa, ngoài ra không đổi gì. `.ha-embedding.json`, `.ha-embedding.sqlite` của
thư viện knowledge và `<data>/history-embedding/` được để nguyên trên đĩa và
không bao giờ được đọc. `memory status` và `/knowledge status` báo khi vẫn còn
`.ha-embedding.json`, và diagnostics ghi `knowledge.embedding_config_ignored`
một lần.

## 15. Chỉ mục để xếp hạng, và quên (0.8.9)

**Từ được lập chỉ mục một lần.** Tới 0.8.8, digest và memory search tách từ,
lấy gốc từ và bỏ dấu cho mọi entry ở mỗi lần gọi, rồi sắp xếp mọi kết quả khớp:
khoảng 0,4 s mỗi lượt với 10.000 entry và 2 s với 50.000. Giờ:

- Entry của mỗi store được lập chỉ mục một lần cho mỗi process
  (`memory/search.rs`, `Corpus`): mỗi từ của bốn trường, viết nguyên và bỏ
  dấu, trỏ tới các entry chứa nó (chỉ mục đảo), cùng gốc từ tiếng Anh của nó.
- Một entry được giữ trong chỉ mục của store dưới key và `version_hash` của
  nó; hash đổi mỗi khi tree, name, title, content, meta hay temporal đổi, nên
  mọi lần ghi, revert, move, import hay purge đều được thấy ở lần đọc sau; chỉ
  các entry đã đổi được lập chỉ mục lại. Entry không đọc từ store (các session
  log mà `rank_documents` xếp hạng) được lập chỉ mục cho riêng lần gọi đó.
- Entry của một term là postings của các từ nó mở đầu và các từ cùng gốc, nên
  chỉ entry chứa từ của truy vấn mới được tính điểm. Thống kê vẫn là của đúng
  các entry lần gọi xét tới, nên điểm giống hệt trước, tới từng bit; một test
  so chỉ mục với cách duyệt cũ trên các store ngẫu nhiên.
- Digest chỉ lấy vài entry nó hiện, và search có một truy vấn chỉ lấy các ứng
  viên của nó, mà không sắp xếp phần còn lại.
- Chỉ mục của một store có số slot vượt hai lần số entry lớn nhất một lần gọi
  đọc từ nó (cộng 1.024) bị bỏ và dựng lại; giữ tối đa 32 store.

Đo bằng `ranking_cost_over_large_stores` (`cargo test --release -p
harness-cli --bin ha -- --ignored ranking_cost --nocapture`) trên một máy;
"giờ" là lần gọi thứ hai và thứ ba trong ba lần trên cùng một state đã nạp:

| Entry | Digest trước | Digest giờ | Search trước | Search giờ |
|---|---|---|---|---|
| 10.000 | 380 ms | 20 ms (225 ms ở lần đầu) | 330 ms | 6 ms |
| 50.000 | 2,1 s | 125 ms (1,1 s ở lần đầu) | 2,0 s | 45 ms |

**Purge.** Trước đây không gì quên được: các phiên bản của entry đã xoá nằm
trong event log 30 ngày, mười phiên bản mới nhất thì mãi mãi, và một secret lọt
qua bộ lọc khi ghi cũng nằm đó. `ha memory purge` xoá một entry và mọi dấu vết
nội dung của nó:

- row của nó, mọi event của nó (mọi phiên bản), row `memory_usage` của nó;
- trong `refinements.jsonl`, mỗi edit của refinement về nó bị rút lại còn
  action, kind và kết quả, không còn title, content, `before` hay `after`;
- mỗi refinement đã thay đổi nó - bản tóm tắt trong store (`refinement`),
  bản ghi trong `refinements.jsonl`, bản sao trong legacy state đã import -
  mất phần chữ (trigger, evidence, outcome; summary, rationale, expected
  outcome), thay bằng một ghi chú rằng nó đã đổi một memory bị purge, và key mà
  các change của nó nêu thành `[purged]`: tóm tắt của một lần review nói nó đã
  ghi gì;
- bản ghi của nó trong `harness_state.json.imported` và các dòng của nó trong
  `memory_history.jsonl.imported`;
- còn lại một event: operation `purge`, cause `purge`, actor `cli`, với kind
  và id của memory, tree là root của store, không có name, memory id thay cho
  key (key có thể được tạo từ một title chứa nội dung đó), và document rỗng.
  History và revert không bao giờ đưa nó ra như một phiên bản.

Các row bị xoá được ghi đè (`secure_delete`), cơ sở dữ liệu được dựng lại
(`VACUUM`) và log của nó được làm rỗng (`wal_checkpoint(TRUNCATE)`), nên nội
dung không còn trong file nào của store. Khi một process khác đang đọc store,
việc dựng lại hay checkpoint có thể chưa xong: purge vẫn có hiệu lực, và lệnh
báo rằng log còn giữ nội dung tới khi process đó đóng store. Journal riêng của
hội thoại, nơi ghi lượt đã viết entry, không phải memory và không bị đổi.

`--secrets` tìm những gì chứa giá trị secret, theo các dạng mà đường ghi từ
chối (`harness_tools::secret_values`), trong store toàn cục và store của mọi
hội thoại: entry đang sống chứa secret bị purge toàn bộ; entry đang sống giờ
không còn chứa thì chỉ mất các phiên bản từng chứa, và giữ trạng thái hiện tại;
entry đã xoá mà log còn chứa secret thì bị purge toàn bộ. Nó cũng che các giá
trị secret còn lại trong bảng refinement, `refinements.jsonl` và các file
legacy đã import. Danh sách nêu loại của mỗi secret, không bao giờ nêu giá trị.

Scope giới hạn những gì một lần purge chạm tới: với `--scope`, `--secrets`
purge các entry của scope đó và các bản sao của chính chúng, để nguyên giá trị
secret trong review của entry khác và trong các file đã import; chỉ `--secrets`
không có scope mới che tất cả. `--conversation` chỉ store của một hội thoại, nên
đi với `--scope local` hoặc không có scope.

Lần chạy thử đọc store ở chế độ chỉ đọc: không migrate, không import, không
đổi file nào. Nó liệt kê, bên cạnh mỗi entry, số bản tóm tắt refinement, bản
ghi `refinements.jsonl` và dòng hay entry legacy đã import mà `--yes` sẽ viết
lại, và nêu file legacy còn chờ import, thứ nó không đọc.

| Lệnh | Làm gì |
|---|---|
| `ha memory purge <id hoặc key>` | Liệt kê các entry mà memory id hay key đó chỉ tới, còn sống hay đã xoá, và không đổi gì |
| `... --yes` | Purge chúng |
| `--scope global`, `project`, `local` | Chỉ entry toàn cục, của project này (`--cwd`), hoặc của một hội thoại (`--conversation`) |
| `ha memory purge --secrets [--yes]` | Như trên cho mọi thứ chứa giá trị secret |

## 16. Khi nào một entry còn đúng, và chữ của nó từ đâu ra (0.9.0)

Memory tự nó không giúp gì: phần lớn các cách dựng memory được đo năm 2026
không tốt hơn khi tắt memory. Những gì các hệ thống hiện nay có mà ha còn
thiếu: mâu thuẫn được giải bằng cách đóng hiệu lực của sự thật cũ thay vì xoá
nó (ADD/UPDATE/DELETE/NOOP của Mem0, `invalid_at` của Zep/Graphiti), câu hỏi
theo thời gian, độ tin theo nguồn để chống đầu độc memory (MINJA), và cho hết
hạn memory không ai dùng (Copilot bỏ memory không được dùng sau 28 ngày).

### 16.1 Các trường

Tất cả nằm trong `memory/validity.rs`, chỗ duy nhất mà một trường mới về sau
sẽ được thêm vào:

| Trường | Có ở | Nói gì |
|---|---|---|
| `origin.kind` | mọi entry do memory tool hoặc một review ghi | chữ của nó từ đâu ra: `user`, `tool`, `web`, `file`, `agent`, `child`, `review`. Host quyết định (§16.4). Entry không có trường này được đọc là `agent`. |
| `supersedes` | entry đã sửa các entry khác | memory id của các entry nó đã đóng |
| `superseded_by` | entry bị một entry sau thay thế | memory id của entry đó |
| `expired` | entry bị lượt hết hạn đóng | lý do: `unused` |

**Hiệu lực** của một entry là giá trị `temporal` của nó: một khoảng
`[start, end)`, hoặc một thời điểm mà từ đó nó đúng. Không có `temporal` thì
nó đúng từ lúc được tạo. Đóng một entry là cho khoảng của nó một điểm kết thúc
là lúc này, tính từ điểm bắt đầu nó đang có (lúc tạo nếu chưa có). Digest bỏ
ra entry đã đóng, như với một khoảng đã hết, kể cả khi khoảng của nó chưa hết:
một entry có ngày trong tương lai bị thay thế hôm nay sẽ kết thúc một mili giây
sau điểm bắt đầu của nó.

### 16.2 Mâu thuẫn được giải ngay lúc ghi, bằng cách đóng hiệu lực

`write` của tool `memory` nhận `supersedes: [id, ...]` (key, địa chỉ
`<tree>/<name>` hoặc memory id):

- Khi tạo mới, entry mới ghi `supersedes` (memory id của các entry đó) và đúng
  từ lúc này, trừ khi `temporal` của nó nói từ khi nào. Mỗi entry được nêu bị
  đóng: `temporal.end` = lúc này, `superseded_by` = memory id của entry mới,
  thành phiên bản kế tiếp của nó. Không xoá gì. Memory id của entry mới được
  cấp trước khi lưu (`Writer::reviving`) để các entry bị đóng có thể nêu nó;
  việc đóng là thêm một lần lưu trong cùng operation, với cause `supersede`.
- Khi cập nhật, entry được ghi cũng thay thế chúng như vậy, và `supersedes`
  của nó dài thêm. Một lần cập nhật có thể chỉ có `supersedes`.
- Entry được nêu phải còn sống, cùng loại, không phải chính entry đang ghi, và
  chưa bị thay thế (nếu đã bị thì lỗi nêu entry đã thay nó, để thay thế entry
  đó). Kiểm tra trùng lặp và kiểm tra lặp lại bằng ngôn ngữ khác không tính
  các entry mà lần ghi thay thế: lần ghi sửa chúng chứ không lặp lại chúng.
  Chúng cũng không tính entry đã đóng, hay entry mà khoảng của nó đã hết:
  chúng so với những gì đang đúng, và một sự thật đúng trở lại thì được ghi
  mới.
- Ghi chú `related` khi tạo mới, và cả hai lời từ chối vì trùng, đưa ra cho
  model các lựa chọn: thay thế entry cũ, cập nhật entry đang có, hoặc không ghi
  gì (xoá entry mới).

Các edit của review cũng vậy: edit `{"action": "supersede", "kind",
"id": <cũ>, "title", "content", "path"?, "metadata"?, "evidence"}` ghi một
entry mới có key theo title, cạnh path và project của entry cũ, rồi đóng entry
cũ (`validity::supersede_edit`). Refinement ghi nó thành hai edit: edit của
entry cũ (`before` của nó, và `after` đã đóng) và `create` của entry mới, nên
rollback sẽ xoá entry mới và đặt lại entry cũ. Nó hoàn tác cả hai hoặc không
cái nào: khi một trong hai đã đổi từ đó, cả hai được giữ, nên sự thật cũ và
bản sửa của nó không bao giờ cùng còn sống, và kết quả nêu entry đã đổi.

`restore` (hoặc `revert`) chỉ với id, trên một entry bị một lần sửa hoặc lượt
hết hạn đóng, sẽ mở lại nó: `superseded_by` và `expired` bị bỏ, và khoảng hiệu
lực trở về như trước khi bị đóng.

`history` cho thấy chuỗi đó: mỗi phiên bản mang `supersedes`,
`superseded_by`, `expired` và `from` của nó.

### 16.3 Câu hỏi theo thời gian

Memory search nhận:

| Tham số | Đọc |
|---|---|
| `as_of` | các entry còn hiệu lực lúc đó: hiệu lực của chúng phủ thời điểm ấy, và chúng đã được ghi trước đó. Entry cũ của một sự thật đã được sửa, khi hỏi về một lúc trước lần sửa. |
| `since`, `until` | các entry còn hiệu lực ở một lúc nào đó giữa hai mốc |
| `include_ended` | cả các entry đã đóng |

Search nêu một thời điểm (`as_of`, `since`/`until`, hoặc bộ lọc temporal ở
§6.4) thì đọc cả entry đã đóng, và xếp kết quả theo mức hiệu lực khớp với thời
điểm đó trước rồi mới đến từ khóa: với một thời điểm, entry có đúng lúc ấy
không; với một khoảng, phần của khoảng mà entry còn đúng (`validity::fit`).
Các chế độ của bộ lọc temporal tạo nên khoảng đó: `contains` là một thời điểm,
`after` và `before` là hai mốc, `overlaps` và `within` là khoảng của chúng.

Search không nêu thời điểm thì bỏ ra các entry đã đóng (bị thay thế hoặc hết
hạn). Entry mà khoảng của chính nó đã hết vẫn được đọc như trước. Giữa những
entry khớp từ khóa ngang nhau, entry còn đúng bây giờ đứng trên entry đã kết
thúc, và entry do người dùng hay agent nói đứng trên entry từ một trang web
hay một child (`search::settled`); entry khớp hơn giữ nguyên chỗ. Khi từ khóa
cũng khớp entry đã đóng, kết quả nói có bao nhiêu (`closed.left_out`) và rằng
`as_of` hoặc `include_ended` sẽ đọc chúng.

Không dùng model để xếp lại: mỗi lần search gọi model thì tốn thời gian và
token của một request, trong khi hiệu lực và từ khóa đã đủ sắp xếp những gì
một câu hỏi theo thời gian cần.

### 16.4 Độ tin theo nguồn

`origin.kind` do host quyết định, không bao giờ do model. Memory tool đọc hội
thoại như journal của project store đang giữ (`memory/provenance.rs`; 4.000
event mới nhất): những chữ người dùng gõ ở mỗi lượt (lượt do host bắt đầu, như
báo cáo của một child, là của child), và kết quả của mỗi lần gọi tool theo
tool đó - của web tool (`web_search`, `web_fetch`) là `web`, của lần đọc file
(`read_file`, `search_text`, `list_files`, `glob`, `find_files`) là `file`, của
một child được giao việc là `child`, của chính các tool memory thì không tính,
của mọi tool khác là `tool`. Mỗi từ của entry, đã bỏ dấu và lấy gốc, có trọng
số bằng IDF của nó trên các phần đó (từ không có ở phần nào nặng như từ chỉ có
ở một phần). Entry thuộc loại có các phần giữ nhiều trọng số nhất, khi chúng
giữ ít nhất một nửa; nếu không thì là `agent`, chữ của chính agent. Hòa thì
loại đáng tin hơn thắng, theo thứ tự user, file, tool, child, web. Không có
danh sách từ nào phán xét văn bản.

Một lần cập nhật lấy loại mà chữ của nó có bây giờ; khi hội thoại này không
cho thấy gì, entry giữ loại cũ, nên chữ của một trang web vẫn là của trang đó
dù về sau ai viết lại. Một lần move giữ nguyên loại. Edit của review lấy loại
của lượt mà evidence của nó trích nguyên văn (`provenance::quoted_kind`), nếu
không thì loại mà các từ của trích dẫn chỉ tới, nếu không nữa thì `review`:
review tự suy ra. Transcript của review được cắt thành từng lượt ở các dòng
trống, và giữ một dòng trống bên trong một lượt hay một kết quả tool thành một
dòng chỉ có một dấu cách (`harness_runtime::transcript_body`): một trang web
mang `[User]: ...` sau một dòng trống vẫn là của trang đó. Edit của Deep Dream
lấy loại của các entry nó viết lại: một lần cập nhật lấy loại kém tin nhất
giữa entry của nó và các entry gộp vào nó, nên chữ của một trang web gộp vào
một constraint bị từ chối. `origin_kind` mà model tự ghi vào một edit bị bỏ.

Hệ quả:

- Digest ghi rõ entry từ một trang web (`, from a web page: unverified`) hay
  từ một child (`, from a delegated agent: unverified`). Search, read và
  history cho thấy `from` và `unverified`.
- Entry như vậy đứng dưới một entry ngang bằng khác (§16.3).
- Entry có loại `web` - bây giờ, hoặc lúc được lưu - không thể được ghi vào
  `constraints/*` hay `user/preferences` (và bên dưới): một lần tạo, cập nhật,
  move hay edit của review đều bị từ chối kèm lý do, trước khi lệnh chạy. Model
  được bảo giữ nó thành một fact ở chỗ khác, hoặc nhờ người dùng tự nói quy
  tắc đó.

### 16.5 Hết hạn theo mức dùng

Một lượt chạy đóng các entry không ai dùng: không được đọc và không xuất hiện
trong kết quả search nào trong `HA_MEMORY_EXPIRE_DAYS` ngày (mặc định 28; 0 là
tắt; tối đa 36.500, một thế kỷ; được dùng số ngày lẻ, nên test có thể dùng vài
giây) kể từ lần cuối được
ghi, đọc hoặc tìm thấy. Được hiện trong digest không tính là dùng. Mỗi entry bị
đóng chứ không bị xoá (`temporal.end`, `expired: "unused"`), thành một event
của writer `sleep` với cause `expire`; `restore` chỉ với id sẽ đưa nó về.

- Được miễn: `constraints/*`, `user/preferences`, entry `source: learning`, và
  entry được ghim.
- Đồng hồ bắt đầu từ lượt chạy đầu tiên trên một kho (`expiry_since` trong meta
  của kho): trước 0.8.8 không có lần đọc nào được đếm, và một lần nâng cấp
  không được đóng cả kho vì một sự không-dùng chưa ai đo.
- Lượt chạy là một job của hàng đợi Sleep (kind `expiry`), chạy ở cuối một
  lượt trong app và cuối `ha exec`, trên kho toàn cục và kho của hội thoại,
  nhiều nhất mỗi ngày một lần (hoặc mỗi chu kỳ một lần, nếu chu kỳ ngắn hơn).
  Nó chạy cục bộ và nhanh: không gọi model. Lượt chạy lỗi, hay panic, thì job
  của nó thất bại, và hàng đợi thử lại.
- `ha doctor` cho biết lượt gần nhất chạy lúc nào và đã đóng bao nhiêu entry
  (meta `expiry` của kho toàn cục).

## 17. Những gì một review ghi, và Deep Dream theo từng lô (0.9.0)

0.9.0 đóng các lỗ hổng mà hai lần audit tìm thấy trong cách các refinement ghi
vào store (`memory/refine.rs`, `memory/learned.rs`). Mỗi điều là một quy tắc của
`apply`, cửa vào duy nhất mà mọi refinement - review, `/refine`, `/learn`, Deep
Dream, curator, rollback - đều ghi qua.

**Một lần cập nhật giữ ai đã lưu entry.** Cập nhật giữ `source` của entry
(`agent`, `learning`, `refine`, ...) và `origin` của nó, và ghi refinement đã
sửa nó vào `edited_by`: `{via: "refine", refinement, conversation, evidence,
confidence}`. Một lần tạo mới là của review (`source: refine`, `origin` là cùng
bản ghi đó). Trước đây mọi lần cập nhật đều đặt `source: refine`: một review
diễn đạt lại một learned rule biến nó thành entry của review, Deep Dream (vốn bỏ
qua `source: learning`) sau đó có thể gộp mất nó, và learning loop retire lesson
của nó vì đã bị xoá. Rollback đặt lại nguyên snapshot, kèm `source` và
`origin`, thành một version mới. Memory tool của model cũng làm vậy với entry
của learning loop: cập nhật, thay thế hay di chuyển một entry `source: learning`
giữ `source` và `origin` của nó và ghi origin của lần sửa vào `edited_by`; entry
khác mà tool sửa thì thành của agent, như trước.

**Rollback chỉ hoàn tác những gì refinement của nó đã ghi.** Mỗi lần khôi phục
mang theo version mà refinement để lại (`after`, hoặc không có gì với entry nó
đã xoá). Một entry chỉ được rollback khi nó vẫn là version đó - cùng `version`,
tiêu đề và nội dung; entry mà refinement đã xoá thì chỉ khi chưa entry nào lấy
id của nó. Một thay đổi learned skill ghi SHA-256 của `SKILL.md` mà nó đã viết
(`undo.written`) và của từng file khác nó đã viết hay xoá
(`undo.written_files`), và chỉ được hoàn tác khi mọi file vẫn như vậy; thay đổi được
ghi trước 0.9.0 không có hash và được hoàn tác như trước. Trang knowledge vốn
đã chỉ bị xoá khi còn nguyên như lúc ghi. Thứ đã đổi từ đó được bỏ qua, không
tính là lỗi: kết quả nêu nó ("Skipped: update local memory `deploy`: changed
since refinement refine_... wrote it: the later version is kept"), và rollback
bỏ qua tất cả thì là "Harness rollback unchanged".

**Lệnh xoá của một lần gộp đứng hay đổ cùng entry còn lại.** Lệnh xoá gộp một
entry vào entry khác thì nêu entry đó trong `merged_into`; prompt của curator
yêu cầu điều này, và prompt của review cho phép. Các lệnh xoá như vậy được áp
dụng sau mọi sửa đổi khác của refinement, và một lệnh xoá bị bỏ qua khi sửa
đổi của entry còn lại không được áp dụng (bị từ chối, hoặc bị bỏ qua vì entry
đã đổi trong lúc đó), khi entry còn lại không còn sau các sửa đổi khác (bị xoá
trong cùng refinement), hoặc bị từ chối khi nó tự nêu chính mình. Lệnh xoá
không có `merged_into` (cũ, tạm thời, bị cài vào) được xét riêng, như trước.
Các rào chắn của Deep Dream cũng dùng entry còn lại: entry đã ghim gộp vào entry
mà lượt này cập nhật thì entry đó được ghim, gộp vào entry vẫn không ghim thì
không bị xoá, constraint chỉ gộp vào constraint, và không gì được gộp vào entry
mà lượt này xoá.

**Entry đã đóng được giữ nguyên.** Review, `/refine`, `/learn` và Deep Dream
không được thấy các entry mà một lần sửa hay lượt hết hạn đã đóng, và các entry
đó không được tính cho việc gộp (`CONSOLIDATE_MIN_ENTRIES`,
`LOCAL_CONSOLIDATE_ENTRIES`). Một lần cập nhật, xoá hay supersede một entry như
vậy, hoặc gộp vào nó, bị từ chối kèm lý do, được ghi như mọi lần từ chối khác:
một edit sẽ mở lại nó, hoặc xoá thứ mà `as_of` và history đọc. Rollback vẫn đặt
lại những gì refinement của nó đã đóng. Cập nhật một entry còn sống thì giữ
`temporal`, `supersedes`, `superseded_by` và `expired` của nó.

**Sửa đổi memory của review trích những gì nó đã đọc.** Một sửa đổi memory hay
prompt note của review, `/refine` hoặc `/learn` chỉ được áp dụng khi `evidence`
của nó tìm thấy trong những gì refinement đã đọc: hội thoại (có cả khối
`[Child findings, from <child>]` của một child, được trích cho một sự thật do
child tìm ra), và với `/refine` là những gì người dùng gõ sau lệnh - không bao
giờ là instructions của review gate, hay của một refinement mà model yêu cầu
(`refine.run`), vốn là lời của một model. Câu trích được so
không tính khoảng trắng và hoa thường, bỏ dấu ngoặc kép bao quanh; các đoạn nối
bằng `...` hoặc `…` phải tìm thấy từng đoạn, theo thứ tự; và các đoạn gộp lại
phải là một cụm dài từ 12 ký tự, như câu trích của lesson, mỗi đoạn từ 4 ký tự:
vài chữ cái rải giữa các dấu ba chấm thì văn bản nào cũng có. Nếu không, sửa đổi bị từ chối kèm lý
do, được ghi cùng refinement như mọi lần từ chối khác. Sửa đổi global không có
evidence vẫn không thành sửa đổi global, và bị từ chối ở nơi nó ở lại.
`confidence` của sửa đổi (`high`, `medium` hay `low`) được ghi vào `origin` của
entry (hoặc `edited_by`); chưa có gì đọc nó.

**Bằng chứng của Deep Dream là chính các entry nó được đưa.** Một lần gộp không
trích hội thoại nào, nên phép kiểm tra nằm trên các entry: sửa đổi nêu một entry
ngoài lô của nó bị từ chối; lệnh xoá trong một lần gộp nêu một entry còn lại
thuộc lô của nó; và cập nhật có hơn một nửa số từ trong nội dung (các term của
tìm kiếm) không nằm trong entry nào của lô bị từ chối vì thêm điều không entry
nào nói. Tiêu đề có thể mới. Lượt gộp không tạo entry nào và không supersede
entry nào: `create` hay `supersede` mà curator đề xuất bị bỏ, và rationale nói
vậy.

**Deep Dream theo từng lô.** Toàn bộ memory toàn cục từng nằm trong một prompt,
không giới hạn: khoảng 10.000 entry làm tràn context của model, và lượt chạy
thất bại mọi lần. Giờ một lượt:

- sắp các entry của phạm vi để các entry liên quan đứng cạnh nhau - theo loại,
  project và path, rồi theo từ hiếm nhất mà mỗi entry cùng có với một entry
  khác, từ mà một bản gần trùng có chung với bản kia;
- cắt thứ tự đó thành các lô tối đa `HA_CONSOLIDATE_BATCH_TOKENS` token entry
  (mặc định 32.000; khoảng bốn byte một token, như bộ biên dịch context đếm),
  mỗi lô từ hai entry; memory nằm trong ngân sách là một lô, như trước;
- dời chỗ lô đầu tiên kết thúc, trong khoảng một phần tư đến toàn bộ ngân sách,
  theo thời điểm của lượt, nên ranh giới của lượt này không trùng lượt sau và
  hai entry bị một ranh giới tách ra sẽ gặp nhau sau đó;
- gọi curator một lần cho mỗi lô, chỉ cho nó thấy các entry của lô và vị trí
  của lô (`batch 2 of 5`, kích thước memory, phần của lô trong mục tiêu 30
  entry), và áp dụng ngay các sửa đổi của lô: mỗi lượt xoá tối đa một nửa mỗi
  lô;
- ghi mỗi lô thành một refinement `<id>_b<n>` có `batch` nêu lượt
  (`{of, index, count}`), hiển thị chung dưới `<id>`, mà `/refine --rollback
  <id>` hoàn tác một lần; lượt chỉ có một lô được ghi là `<id>`, như trước;
- thất bại khi lời gọi hoặc lần áp dụng của một lô thất bại: đó là thất bại
  của job, mà hàng đợi thử lại ([SLEEP.vi.md](SLEEP.vi.md)); các lô trước nó vẫn
  được giữ, và thông báo lỗi nêu tên chúng.

Điều tương tự áp dụng cho store của một hội thoại quá 40 entry và phần memory
của curator (`/refine --curate`), mà các lô của nó đi trước refinement `<id>`
của phần skill.

## 18. Code mà một entry trích dẫn, và memory chỉ dành cho một số file (wave 2)

Một sự thật về code sẽ cũ đi khi code đổi, mà không gì trong entry nói ra điều
đó: digest vẫn trình bày nó như sự thật. Còn một rule chỉ có ý nghĩa với một số
file (billing làm tròn thế nào, một migration không được đụng vào đâu) thì
hoặc nằm trong digest của mọi lượt, chiếm một chỗ trong sáu chỗ, hoặc nằm ngoài
đúng lúc cần đến nó. Hai tính năng của các agent hiện nay giải quyết việc này:
GitHub Copilot Memory giữ trích dẫn tới code kèm mỗi memory và kiểm tra chúng
với nhánh hiện tại khi memory được lấy ra; Claude Code nạp một
`.claude/rules/*.md` có `paths:` khớp với file nó đọc, và Amp, Devin cũng giới
hạn knowledge theo đường dẫn như vậy. Cả hai đều đo được và tắt được (§18.5).

### 18.1 Các trường

Tất cả nằm trong `memory/citations.rs` (`memory_citations`), như các trường
của §16.1 nằm trong `memory/validity.rs`:

| Trường | Có ở | Nói gì |
|---|---|---|
| `citations` | entry được tool `memory` ghi kèm trích dẫn | code mà nó dựa vào: `[{path, quote, lines, project}]` - một file tính từ gốc workspace, một đoạn ngắn đúng chữ của file, các dòng `[start, end]` nơi host tìm thấy đoạn đó, và key của project lúc ghi |
| `paths` | entry chỉ dành cho một số file | các glob kiểu gitignore, tính từ gốc workspace (`src/billing/**`) |

Hai bản ghi nằm cạnh các entry, không bao giờ nằm trong chúng: khoá meta
`citations` của kho giữ lần kiểm tra gần nhất các trích dẫn của mỗi entry, theo
memory id (§18.3); khoá meta `scoped_loaded` trong kho của một hội thoại giữ
những gì hội thoại đó đã nạp (§18.4).

### 18.2 Trích dẫn, kiểm tra lúc ghi

`write` của tool `memory`, tạo mới hay cập nhật, nhận
`citations: [{path, quote, lines?}]`, tối đa 5:

- `path` là một file của workspace của hội thoại, tính từ gốc của nó; đường
  dẫn tuyệt đối nằm trong workspace được đổi thành tương đối, đường dẫn nằm
  ngoài, hoặc có `..`, bị từ chối. File lớn hơn 1 MiB không được đọc.
- `quote` là đúng chữ của code mà sự thật dựa vào: ít nhất 8 ký tự không kể
  khoảng trắng, tối đa 400. Nó được so sánh khi đã gộp khoảng trắng: một chuỗi
  dấu cách, tab và xuống dòng là một dấu cách, chữ hoa thường giữ nguyên.
- `lines` (`[start, end]`, đếm từ 1) là tuỳ chọn. Có thì quote phải nằm trong
  các dòng đó; không có thì tìm ở bất kỳ đâu, lấy chỗ đầu tiên.

Host kiểm tra từng trích dẫn trước khi lần ghi chạy và giữ nó cùng các dòng nơi
tìm thấy quote và key của project. Trích dẫn không đúng bị từ chối kèm lý do,
ví dụ `citation 1: src/billing/tax.py does not hold that quote (compared with
whitespace aside): copy a short span of the file's own words`, hoặc `... holds
that quote at lines 6-6, not within 1-2: give those lines, or leave lines out`.

`citations` của một lần cập nhật thay toàn bộ trích dẫn của entry; `[]` hoặc
`null` xoá chúng. Lần tạo mới thay entry đang giữ tên đó (`on_conflict:
replace`) chỉ giữ các trích dẫn nó đưa. Trích dẫn có thể nằm trên entry thuộc
scope nào cũng được; đọc trong workspace của project khác thì nó không được
kiểm tra (§18.3), không bao giờ bị gọi là hỏng: đó là những file khác.

**Host không tự gắn trích dẫn.** Khi provenance cho thấy chữ của entry đến từ
một lần đọc file (`origin.kind = file`, §16.4) mà lần ghi không đưa trích dẫn
nào, kết quả có `cite` nói cách thêm. Host biết file nào đã được đọc, không
biết đoạn nào của nó là chỗ sự thật dựa vào: một đoạn đoán mò về sau sẽ đánh
dấu một entry đúng là đã cũ, hoặc để một entry đã cũ trông như còn đúng.

### 18.3 Trích dẫn, kiểm tra lúc đọc

Mỗi lần đọc một entry đều kiểm tra trích dẫn của nó với file như hiện tại:
digest, với từng entry nó hiển thị (kể cả entry ghim); `search`, với từng kết
quả, ở cả hai mode; `read`. Một trích dẫn:

| Trạng thái | Khi nào | Các dòng |
|---|---|---|
| `holds` | quote vẫn ở chỗ đã tìm thấy | như đã giữ |
| `moved` | quote ở chỗ khác trong file (có dòng được chèn phía trên) | chỗ gần nhất với nơi tìm thấy lần trước: chúng đi theo code |
| `broken` | file không còn, không còn là file, hoặc không còn chứa quote | |
| `unchecked` | file lớn hơn 1 MiB hoặc không đọc được, trích dẫn của project khác, hoặc ngân sách đọc của lần đọc đã hết | |

Kết luận của entry là `broken` khi có một trích dẫn hỏng, `unchecked` khi có
một trích dẫn chưa kiểm tra và không cái nào hỏng, ngoài ra là `holds`. Kết
quả search và read mang `citations` (`path`, `state`, `lines` và `reason` của
từng cái), `citations_state` (kết luận) và, khi hỏng, `citations_note`.

Digest ghi `[cites src/billing/tax.py:6]` sau entry có trích dẫn còn đúng, và
sau entry có trích dẫn hỏng thì ghi `[its cited code changed since:
src/billing/tax.py no longer holds the quoted code; check the code before
relying on it, then update or supersede the entry]`: nó không được trình bày
như sự thật. Nó được đánh dấu chứ không bị bỏ ra, để model thấy và có thể cập
nhật hoặc thay thế nó, và một lần ablation (tắt `citations`) đo được dấu đó.
Entry chưa kiểm tra được hiển thị như trước.

- **Chi phí.** File được cache theo đường dẫn, thời điểm sửa và kích thước
  (256 file, file lâu nhất chưa dùng bị bỏ trước), nên digest không đọc lại
  file chưa đổi; một digest, một search hay một read đọc tối đa 4 MiB file
  chưa có trong cache, các trích dẫn sau đó là chưa kiểm tra.
- **Không tạo phiên bản.** Lần kiểm tra không bao giờ ghi vào entry: kết quả
  của nó là bản ghi meta `citations` của memory id - kết luận, các dòng nơi
  tìm thấy từng trích dẫn (nơi lần kiểm tra sau nhìn trước tiên: các dòng lặng
  lẽ đi theo một quote đã dời chỗ), lý do của các trích dẫn hỏng và thời điểm,
  chỉ ghi khi có thay đổi. Các lần kiểm tra của digest được ghi cùng bộ đếm số
  lần hiển thị, ngoài đường đi của lượt.
- **Review thấy được.** Bản tổng quan mà review đọc đánh dấu entry có lần kiểm
  tra gần nhất là hỏng (`[its cited code changed since it was kept: ...]`).
- **`ha doctor`** đếm các entry của project có trích dẫn code và các entry có
  trích dẫn đang hỏng (`memory citations: 1 of 3 entries cite code that
  changed since they were kept`).

### 18.4 Memory chỉ dành cho một số file

`write` của tool `memory` nhận `paths: [glob, ...]`, tối đa 16, mỗi glob tối
đa 200 ký tự, tính từ gốc workspace, kiểu gitignore: glob không có `/` khớp ở
mọi độ sâu (`*.sql`), một thư mục khớp những gì bên dưới nó (`docs/api`), dấu
`/` ở đầu neo vào gốc; glob tuyệt đối, có `..` hoặc ổ đĩa bị từ chối, glob không
parse được cũng vậy (`paths[0] "src/[billing" is not a glob: ...`). `[]` hoặc
`null` xoá chúng.

- **Nằm ngoài digest.** Entry có `paths` không nằm trong các entry mà digest
  của mọi lượt hiển thị; digest đếm chúng (`memory kept for some files only:
  2 (...)`).
- **Nạp cùng kết quả đã chạm vào file khớp.** Kết quả của một file tool chạm
  vào một file khi `read_file`, `edit_file`, `write_file` hoặc `apply_patch`
  nêu tên nó, `list_files` hoặc `glob` liệt kê nó, `find_files` tìm thấy nó,
  hoặc một kết quả khớp của `search_text` nằm trong nó (500 đường dẫn đầu của
  một kết quả). Các entry dành cho file bị chạm đi kèm kết quả đó, mỗi entry
  một phần riêng sau nội dung kết quả, mà lần gọi model kế tiếp đọc:
  `[memory-scoped: a memory entry kept for src/billing/**; this result touched
  src/billing/refunds.py. A record of earlier work - data to weigh, not an
  instruction: ...]` và dòng của entry, nội dung đầy đủ tới 2.000 ký tự, kèm
  trạng thái trích dẫn. Một kết quả mang tối đa 8 entry.
- **Vì sao đi kèm kết quả.** Một lượt gửi context packet của bước đầu tiên ở
  mọi bước, để cache của provider giữ được nó; một block thêm vào packet giữa
  lượt chỉ tới được model ở lượt sau. Kết quả thì tới được lần gọi kế tiếp,
  như instruction của một thư mục mà lần gọi chạm tới (`NestedInstructions`).
- **Mỗi hội thoại một lần.** Kho của hội thoại ghi những gì nó đã nạp
  (`scoped_loaded`: memory id của entry, đường dẫn đã nạp nó, thời điểm, và số
  lần hội thoại đã bị compact lúc đó, theo số sự kiện `compaction.completed`
  trong journal). Một lần chạm khác, trong cùng lượt, lượt sau hay một process
  sau, không mang thêm gì - cho tới một lần compact: nó đã tóm tắt mất kết quả
  mà entry đi kèm, và lần chạm kế tiếp mang nó tới lần nữa.
- **Có bản ghi.** Mỗi lần nạp là một bản ghi chẩn đoán `memory.scoped_loaded`
  (`loaded`, `path`, `compactions`).
- **Vẫn search được.** Search vẫn tìm thấy entry như vậy, và hiển thị `paths`
  của nó. Các glob và các file nó trích dẫn là từ của vị trí nó (trường path
  trong BM25 của memory search), nên search `billing` tìm thấy entry dành cho
  `src/billing/**`; search có truy vấn nêu một đường dẫn - một từ có `/`, hoặc
  tên file có phần mở rộng (`invoice.rs`) - đưa các entry dành cho hoặc trích
  dẫn nó lên trước, mỗi nhóm giữ thứ tự của nó.
- **Trích dẫn không giới hạn phạm vi.** Entry trích dẫn `src/billing/tax.py`
  vẫn nằm trong digest của mọi lượt nếu không có `paths`: sự thật về một hàm
  cũng có ý nghĩa ở nơi gọi nó, xa file của nó, và entry áp dụng ở đâu là lựa
  chọn của người ghi.
- **Chạy ở đâu.** Chat và `ha exec`. Child được uỷ quyền không có memory theo
  file: agent uỷ quyền giữ memory, và các lần đọc file của child không nạp gì.
- **Kho markdown.** Một file memory có `paths:` trong frontmatter được nạp
  cùng cách ([MARKDOWN_MEMORY.vi.md](MARKDOWN_MEMORY.vi.md) §9).

### 18.5 Đo được, và tắt được

`HA_MEMORY_OFF` nhận thêm hai tính năng ([hướng dẫn vận hành](../guides/OPERATOR_GUIDE.vi.md)
§12.21):

- `citations`: trích dẫn không được kiểm tra cũng không được hiển thị - không
  kiểm tra trong digest, search hay read, không đánh dấu, không ghi meta - và
  lần ghi có trích dẫn bị từ chối; tool không đưa ra tham số đó.
- `scoped`: entry dành cho một số file không được nạp cũng không được ghi -
  không nạp cùng kết quả, tool `memory` không liệt kê, search hay read chúng,
  lần ghi có `paths` bị từ chối và tham số không được đưa ra - và các file của
  kho markdown có `paths:` không được nạp.

`/context` và `ha doctor` nêu tên chúng khi chúng bị tắt. `memory-ab.sh` có một
tác vụ cho mỗi tính năng: `retries` (agent đã giữ giới hạn retry của HTTP
client kèm trích dẫn, rồi bài test đổi dòng đó: tác vụ thành công khi lần chạy
nhận ra sự thật đã giữ đã cũ) và `ledger` (một entry dành cho `app/billing/**`
chứa hằng số mà một module billing mới phải định nghĩa; một file trong
repository thì không được, vì lần chạy tắt memory vẫn đọc các file của kho
markdown như mọi file khác).
