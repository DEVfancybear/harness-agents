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
| Lưu memory | **Mỗi scope một kho SQLite**, theo mô hình record của ME: tree, name, meta, temporal, version, event log, FTS5, vector. Thay cho `harness_state.json`. |
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

Trạng thái hiện tại: ✅ có, 🟡 một phần, ❌ chưa có. P1 tới P7 là các phase ở §12.

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
| Semantic cho memory, với hàng đợi embedding chạy nền | ❌ | P3 |
| Hybrid RRF có trọng số; `weights`, `candidateLimit`, `semanticThreshold` theo từng lần gọi | 🟡 | P3 |
| Bộ lọc `tree`, `meta`, `metaPredicate`, `temporal`, `grep`; duyệt chỉ bằng lọc, có `order_by` | ❌ | P3 |
| `select` (chọn trường) và `format` yaml/json/compact | 🟡 | P3 |
| Lịch sử trang knowledge, revert, undelete | ❌ | P4 |
| Một search cho cả memory và knowledge | ❌ | P4 |
| Bộ lọc history (`since`, `until`, `kind`); liên kết thread khi `history_read` | ❌ | P4 |
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
| global | `<data>/harness/` | `/global/...`; entry gắn với một project nằm ở `/projects/<project_key>/...` |
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
  name            TEXT NOT NULL,         -- id entry của ha (slug), duy nhất theo kind
  tree            TEXT NOT NULL,         -- '/global/user/preferences'
  title           TEXT NOT NULL,
  content         TEXT NOT NULL,
  meta            TEXT NOT NULL DEFAULT '{}',   -- object JSON (`metadata` của entry)
  temporal_start  TEXT,                  -- RFC 3339 UTC; NULL = không có temporal
  temporal_end    TEXT,                  -- không tính đầu mút; = start nếu là một thời điểm
  doc             TEXT NOT NULL,         -- toàn bộ entry như ha ghi (không mất trường nào)
  version         INTEGER NOT NULL,
  version_hash    TEXT NOT NULL,         -- 32 ký tự hex của SHA-256 trên payload chuẩn hóa
  content_version INTEGER NOT NULL,      -- chặn ghi embedding cũ (P3)
  created_at      TEXT NOT NULL,
  updated_at      TEXT NOT NULL,
  UNIQUE (kind, name)
);
CREATE INDEX memory_tree ON memory(tree);
CREATE INDEX memory_temporal ON memory(temporal_start, temporal_end);

CREATE TABLE memory_event (
  event_id     TEXT PRIMARY KEY,   -- UUIDv7: thứ tự thời gian tuyệt đối
  at           TEXT NOT NULL,
  memory_id    TEXT NOT NULL,      -- còn lại sau khi memory bị xóa
  kind TEXT NOT NULL, name TEXT NOT NULL, tree TEXT NOT NULL,
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
-- P3: memory_fts (FTS5), memory_vector, memory_embedding_queue (§6)
```

Entry của ha ánh xạ sang record của ME như sau:

- **name.** Là id entry của ha. Quy tắc hiện tại vẫn giữ: slug từ title, đổi title
  thì giữ alias. Tên theo regex của ME cũng được chấp nhận. Tên là duy nhất theo
  `(kind, name)` trong một scope, chặt hơn `(tree, name)` của ME.
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

- `name`: đặt slug cụ thể;
- `tree`: đường dẫn tuyệt đối (`/global/...`, `/projects/<project này>/...`,
  `/local/...`, đồng thời chọn luôn scope) hoặc tương đối theo `scope` (chính là
  `path` hiện nay, vẫn được chấp nhận);
- `meta`: một object;
- `temporal`: `{start, end?}`;
- `on_conflict`: `error` | `replace` | `ignore`, theo khóa `(kind, name)`.

`replace` có so nội dung: payload y hệt thì không làm gì và không tăng version.
Cơ chế từ chối entry trùng của ha (độ giống 0,6 hoặc chứa 85 %) vẫn chạy trước
`on_conflict` với entry mới, và `distinct` vẫn bỏ qua được nó.

Cập nhật vẫn dùng `expected_version`, và nhận thêm `version_hash` như ME.

`restore` đổi thành `revert`, và `restore` vẫn dùng được như một tên gọi khác:

- nó áp lại một snapshot từ event log thành phiên bản mới, với cause `revert`;
- revert một memory đã xóa sẽ tạo lại nó với id cũ, version đánh số tiếp;
- revert vào một slot `(kind, name)` đã có entry khác là xung đột.

Digest ở P1 không đổi. Chỉ thêm một điều: entry có khoảng temporal đã kết thúc
(`temporal_end <= now`) không được đưa vào digest, vì đó là lịch sử chứ không còn
là sự thật hiện tại. Search vẫn tìm thấy chúng.

## 5. Sắp xếp và lịch sử (P2)

Các action mới của tool `memory`. Agent con chỉ có các action chỉ đọc.

| Action | Tương đương ME | Ghi chú |
|---|---|---|
| `get` với `path` (`tree/name`) | `get_by_path` | Tên trùng giữa các kind thì phải ghi `kind` |
| `tree` (`tree`, `levels`) | `me_memory_tree` | Các nút kèm số lượng, global và local gộp chung |
| `count` (lọc `tree`, `max_count`) | `me_memory_count` | |
| `move` (`source`, `destination`, `dry_run`) | `me_memory_mv` | Thay tiền tố. Chuyển giữa hai gốc scope thì chuyển dòng sang kho kia, chung một `operation_id` |
| `copy` (`source`, `destination`, `dry_run`) | `me_memory_copy` | Bản sao có id mới; tên trùng thì thêm hậu tố `-2`, `-3` |
| `delete` với `path` | `delete_by_path` | |
| `delete_tree` (`tree`, `dry_run`) | `me_memory_delete_tree` | |
| `history` | `me_memory_history` | Phạm vi theo `id`, `path`, `tree`, `operation_id` hoặc `since`; thêm `until`, `operation`, `order`, `cursor` (keyset theo `event_id`), `limit` (tối đa 1000) |

Mỗi refinement ghi lại `operation_id` của lần ghi, để `/refine --rollback` và
`history operation_id=…` khớp nhau về những gì một lượt review đã đổi.

## 6. Tìm kiếm (P3)

### 6.1 Theo từ khóa

`memory_fts` là bảng FTS5 trên `title`, `content`, `tree`/`name` và các giá trị
`meta`. Văn bản được index là kết quả `query_terms` của ha, nối bằng dấu cách:

- chữ thường, NFC;
- chuỗi CJK tách thành bigram;
- tokenizer `unicode61 remove_diacritics 2`.

Nhờ vậy giữ được cách khớp không phân biệt dấu và chạy được trên nhiều hệ chữ như
hiện nay. Các từ trong câu hỏi được nối bằng OR và khớp theo tiền tố
(`"deploy"*`), nên "deploy" vẫn tìm ra "deploys" như cách khớp chuỗi con hiện tại.

Xếp hạng bằng `bm25()`, trọng số cột: title 3, content 1, path 1, meta 0,5. Chỉ
dòng khớp ít nhất một từ mới là ứng viên; đây là điều kiện "phải thật sự khớp"
của ME, do `MATCH` bảo đảm.

### 6.2 Theo nghĩa

Memory dùng endpoint embedding đã cấu hình cho thư viện knowledge global
(`/knowledge semantic --global`). Chưa cấu hình thì không embed gì.

| Bảng | Nội dung |
|---|---|
| `memory_vector` | `memory_id`, `content_version`, `namespace` (endpoint + model), vector |
| `memory_embedding_queue` | Một dòng cho mỗi `(memory_id, content_version)` |

Hàng đợi được thêm dòng khi chèn và khi nội dung đổi. Nó được xử lý bởi task chạy
nền sau mỗi lượt, và tối đa 20 s ngay trước một lần search theo nghĩa.

Lúc ghi kết quả có kiểm `content_version`, nên vector cũ không bao giờ ghi đè.
Lỗi thì thử lại 3 lần; gặp 429 thì hoãn dòng đó mà không tính một lần thử.

Điểm là cosine tính đủ trên mọi vector: kho của ha đủ nhỏ để không gặp các giới
hạn độ phủ của HNSW như ME. Kết quả là độ tương đồng cosine trong `[-1, 1]`.
`semantic_threshold` nằm trong `[0, 1]`; giá trị ngoài khoảng bị từ chối chứ
không bị kẹp lại.

### 6.3 Hybrid

Reciprocal Rank Fusion có trọng số, giống ME:

```text
score = w_fulltext / (60 + rank_fulltext) + w_semantic / (60 + rank_semantic)
```

| Tham số | Mặc định và giới hạn |
|---|---|
| `weights` | 1 / 1, mỗi giá trị trong [0, 1] |
| `candidate_limit` mỗi nhánh | 30, ít nhất bằng `limit`, tối đa 1000 |
| `limit` | 10, tối đa 200 |

Nhánh nào không có kết quả thì góp 0; điểm bằng nhau thì xếp theo id.

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

### 6.4 Bộ lọc

Dùng được với mọi chế độ và kết hợp với nhau theo AND. Chúng chạy bằng Rust trên
các dòng ứng viên; trước đó SQL đã thu hẹp ứng viên bằng tiền tố tree và index
temporal.

| Bộ lọc | Ý nghĩa |
|---|---|
| `tree` | **Đường dẫn chính xác:** nút đó và mọi nút con. **lquery,** khi có `* ! { } \| @ %`: `*`, `*{n}`, `*{n,m}`, `!nhãn`, `a\|b`, `nhãn@` (không phân biệt hoa thường), `nhãn%` (khớp theo từ), `nhãn*` (tiền tố). **ltxtquery,** khi có `&`: Boolean `& \| !` trên các nhãn, cùng các bổ ngữ trên. Theo tài liệu ltree của PostgreSQL. |
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
- các method `.size() .type() .double() .ceiling() .floor() .abs() .keyvalue()`;
- chế độ lax như ME dùng, trong đó lỗi cấu trúc cho kết quả false.

Bộ parser và evaluator tự viết trong `memory/jsonpath.rs`, kiểm bằng một bảng kết
quả lấy từ PostgreSQL.

Không có văn bản search thì bộ lọc dùng để duyệt: mới nhất trước, hoặc theo
`order_by` `asc`/`desc`. Mọi kết quả đều có `total`, đếm theo kiểu `max_count`
tới 10 000.

### 6.5 Trình bày

| Tham số | Cách dùng |
|---|---|
| `select` | Các trường cần trả: `id`, `name`, `tree`, `kind`, `title`, `meta`, `meta.<khóa>`, `temporal`, `version`, `version_hash`, `score`, và lát cắt content `content:N`, `content:M:N`, `content:M:` (tính theo ký tự, kèm `content_length`). |
| `format` | `json` (mặc định của tool, dạng gọn), `compact`, hoặc `yaml`. |

YAML (cũng cần cho pack và import) dùng một crate YAML cho serde, còn được bảo
trì, ghim phiên bản; chọn và ghi vào `Cargo.toml` ở P3.

## 7. Knowledge và history (P4)

### Lịch sử trang knowledge

Mỗi thư viện có thêm `.ha-history.sqlite3`. Bảng `page_event` ghi mỗi lần ghi và
xóa:

- `event_id`, `at`, `path`;
- `operation`, `operation_id`, `cause`, `actor`;
- `hash`, `title`, `summary`, `sources`, `related` và toàn bộ nội dung trang.

Event được ghi trong cùng bước commit cập nhật `log.md` và `.ha-pending.json`, nên
khi khôi phục sau sự cố thì cả hai cùng được hoàn tất.

Tool `knowledge` có thêm:

- `history` (một trang, một thư mục, hoặc feed theo thời gian);
- `revert` (`path`, `version` = id của event).

Revert một trang đã xóa sẽ tạo lại trang đó, theo cùng luật backlink như khi ghi.
Thời gian giữ giống memory: 30 ngày, luôn giữ 10 phiên bản mới nhất của mỗi
trang.

### Một search cho cả hai

`search` của memory nhận `sources: ["memory", "knowledge"]`; mặc định chỉ tìm
memory. Kết quả knowledge lấy từ engine knowledge (BM25, hoặc hybrid nếu đã cấu
hình) rồi gộp bằng RRF thành một danh sách. Mỗi kết quả ghi rõ `source`, và với
trang knowledge thì có `path` cùng khoảng dòng. CLI làm tương tự với
`--knowledge`.

### Tool history

- **`history_search`** có thêm:
  - `since` và `until`; cần cột `occurred_at` trên `history_sources`, được một
    migration điền từ timestamp trong journal;
  - `kind`, một danh sách loại event của journal;
  - `order`.
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
  event, file cũ còn chờ nhập, trạng thái hàng đợi embedding, các tích hợp đã cài.

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
| **P3 Tìm kiếm** | §6: FTS5 BM25, vector và hàng đợi cho memory, RRF có trọng số (cả knowledge và history), mọi bộ lọc, JSONPath, select/format, crate YAML | memory, knowledge, memory-store |
| **P4 Knowledge và history** | §7: lịch sử trang và revert, search chung, bộ lọc history và liên kết thread | knowledge, session-continuity, memory-store |
| **P5 CLI** | §8: `ha memory`, bốn định dạng, `--legacy`, pack | memory-store |
| **P6 Importer** | §9: docs, git, phiên Claude, Codex, OpenCode, kind `record` | memory-store |
| **P7 Tích hợp** | §10: MCP server, tích hợp harness và capture, web console, doctor | memory-store, cộng một lần chạy với MCP client thật |

Trước P1, các test live đọc và nạp thẳng `harness_state.json` (`kernel.sh`,
`knowledge.sh`, `memory.sh`, `session-continuity-evidence.py`). P1 chuyển việc nạp
sang quy tắc nhập file cũ ở §3.4, còn việc đọc chuyển sang
`ha memory export --json` (P1 làm sẵn một bản export tối thiểu cho việc này) hoặc
truy vấn `sqlite3` chỉ đọc.
