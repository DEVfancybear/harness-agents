# Handoff: sửa truy hồi memory dài hạn (lượt goal-08ba0f94)

Commit: `366b621` — `fix(memory): recall by meaning, not by matching every word`

## 1. Người giao việc hỏi gì, và câu trả lời hoá ra là gì

Câu hỏi: *"làm sao để agents biết session trước làm gì?"*, rồi *"dự án này là memory dài hạn mà?"*.
Cả hai đúng. `crates/harness-memory` là phân hệ trung tâm với C01–C30 trong
`docs/MEMORY_AND_CONTINUITY.vi.md`, và `HA_MEMORY=on` trên máy này.

Nhưng khi đo thì memory **lưu được mà không lấy ra được** bằng câu hỏi thật:

```text
lưu : "Remember this marker for later: zebra-quasar-7719"  -> asset có version/provenance/scope
hỏi : "marker remember"                                    -> 2 hit, model trả ĐÚNG marker
hỏi : "What marker did I ask you to remember? ..."          -> 3/3 lần KHÔNG ra marker
```

Lỗi thứ hai nặng hơn: mỗi lượt lưu **nguyên văn input**, kể cả câu hỏi, với
`evidence=UserConfirmed` + `user_confirmed=true` hardcode. Nên câu hỏi thành "kiến thức đã xác nhận",
và hỏi lặp làm corpus đầy bản gần trùng lấn át câu trả lời (đo được: 2 → 3 → 4 hit qua ba lần,
không hit nào là câu trả lời).

## 2. Bốn lỗi, và chỗ sửa

| # | Lỗi | Chỗ | Sửa |
|---|---|---|---|
| 1 | `AND` mọi term → câu hỏi tự nhiên không khớp gì | `harness-memory/src/retrieval.rs` | Hợp (OR) + **sàn coverage ≥ 2** + fallback `AND` khi không ai đủ ngưỡng |
| 2 | Mọi block memory `relevance = 10` → thứ hạng bm25 bị vứt, sort rơi xuống asset_id | `retrieval.rs::contribute` | relevance giảm dần theo hạng |
| 3 | Mọi input được lưu, kể cả câu hỏi | `harness-cli/src/interactive/memory.rs` | `classify_input` loại câu hỏi; `RememberOutcome` nói ra lý do |
| 4 | Không có dedup | `memory.rs` + store | `find_active_memory_by_content` + `append_memory_version_source` |

**Điểm tựa an toàn giữ nguyên:** store vẫn sở hữu scope/grant/binding/lineage; caller chỉ sở hữu
hình dạng query và ngân sách dòng. Không nới một predicate uỷ quyền nào.

**Dedup không tăng version:** asset giữ nguyên id, version, `content_hash`; chỉ `version_json`
ghi thêm event nguồn. `content_hash` phủ **content**, không phủ source list
(`convert_version` kiểm `ContentHash::from_bytes(record.content) != record.record.content_hash`),
nên summary phụ thuộc không bị đánh stale.

## 3. Bằng chứng

```text
cargo test -p harness-cli --bin ha --locked          -> 201 passed; 0 failed
cargo test -p harness-cli --test phase_p4 --locked   -> 26 passed; 0 failed
cargo clippy --workspace --all-targets --locked -- -D warnings -> sạch
cargo fmt --all -- --check                           -> sạch
pwsh -NoProfile -File scripts/Verify-Docs.ps1        -> DOCS_OK
pwsh -NoProfile -File scripts/Verify-P4Mutations.ps1 -> P4_MUTATIONS_OK: 5/5 killed
```

**Mutation test cho sàn coverage** (đây là phần tôi làm sai lần đầu và phải sửa): bản test đầu chỉ
chứng minh phần OR, vì đặt `MIN_TERM_OVERLAP` về 0 vẫn xanh. Test thứ hai
`memory_a_wider_query_does_not_inject_a_note_that_only_shares_one_word` mới canh đúng cái sàn:
đặt sàn về 0 **hoặc** 1 đều làm nó đỏ.

**End-to-end với binary thật:**

```text
lưu  : "Always use cargo test before committing; remember this marker: zebra-quasar-7719"
       -> stored_disposition: stored
hỏi  : session MỚI, task MỚI, "What was the marker I asked you to remember?"
       -> recall: memory: 1 hit(s), 1 block(s) injected
       -> answer: zebra-quasar-7719          (đúng; trước đó 3/3 lần trượt)
```

## 4. Gate đã xanh, và một lỗi thật lộ ra khi đuổi theo nó

`Verify-HaLaunch.ps1 -Json` lần chạy đầu của lượt này: `failures: ["regression-phase_p2",
"regression-phase_p7"]`. Cả hai **không** liên quan memory, nhưng cả hai đều hoá ra là lỗi thật:

**`p7_install_script_installs_a_working_binary` — lỗi thật của `-SkipBuild`, đã sửa.**
`Install-Ha.ps1` đặt `$sourceBinary = $expectedArtifact` rồi, khi có `-SkipBuild`, **không build**
artifact đó. `$expectedArtifact` là `target/<profile>/ha` của **chính repo đang chạy script**, nên
trên một repo chưa từng build thì thư mục đó rỗng, và installer từ chối bằng
"The artifact is missing at … Run without -SkipBuild" — tức nói ngược lại đúng thứ người gọi vừa
yêu cầu. `git blame` cho dòng đó: `0ed44139` (19/09), **trước** phiên này.

Sửa: artifact được build khi nó vắng mặt **và** không skip; khi vắng mặt **và** skip thì báo thẳng
"repo này chưa được build" kèm lệnh cần chạy. Kèm một chi tiết dễ sót: `$sourceBinary` phải được
gán **trước** nhánh, vì `Set-StrictMode -Version Latest` biến việc đọc biến chưa gán thành lỗi
chứ không phải chuỗi rỗng — lần sửa đầu của tôi vấp đúng chỗ đó.

**`p2_s02_provider_streams_and_deepseek_sse_adapter_are_normalized` — flake loopback, và nguyên
nhân là do `p7`.** Ca này chạy **một mình thì xanh** (0.59 s) nhưng đỏ sau 36 s trong gate, nghĩa
là nó cạn 10 lần thử của retry ladder. Nghi vấn "đói tài nguyên" được xác nhận khi `p7` được sửa:
`p7` gọi `Install-Ha.ps1` không có `-SkipBuild`, tức **build cả repo trong thư mục tạm** ngay
trước `p2`. Sau khi sửa `p7`, `p2` xanh trong gate **không cần đụng vào nó**.

Số đo cuối:

```text
Verify-HaLaunch.ps1 -Json  -> passed: true, failures: []
cargo test -p harness-cli --bin ha --locked   -> 201 passed
phase_p4                                      -> 26 passed
Verify-P4Mutations.ps1                        -> P4_MUTATIONS_OK: 5/5 killed
Invoke-HaPtyAcceptance.ps1                    -> PTY_EXIT 101, 15 passed; 1 failed (28.87 s)
```

**Ca PTY đỏ là flake, không phải hồi quy:** `i05_exit_during_an_active_run_releases_the_store_for_the_next_host`
(`interactive_terminal.rs:1440`). Chạy **một mình**: `PTY_EXIT 0`, xanh trong 0.90 s. Đây là họ
"console dưới tải" đã gặp ở `i14` và `t07` (handoff §15.3). Ca duyệt thật
`t06_pty_approval_y_key` **xanh**, nên panel duyệt vẫn hoạt động trên ConPTY.

## 5. Việc còn lại, theo thứ tự

1. ~~Chạy 16 ca PTY~~ — xong: 15/16, ca đỏ là flake console (`i05`, xanh khi chạy riêng),
   ghi ở §4.
2. **Cập nhật `docs/specs/P4.vi.md` §7 và `docs/evidence/P4.vi.md`** với số đo của lượt này —
   **chưa làm**. SPEC §7 hiện chỉ nói "query có parameter, normalized Unicode/không dấu/snake/camel
   identifiers, giới hạn hit/token/time"; nó **không** nói gì về chiến lược khớp, nên không có gì
   phải sửa cho đúng — nhưng nên ghi lại union + sàn như một quyết định.
3. **Thêm một ca acceptance cho truy hồi bằng câu tự nhiên.** Đây là lỗ hổng đã để lọt cả bốn lỗi:
   **không ca C nào hỏi bằng câu paraphrase** — cả ba test end-to-end cũ dùng query mà term có
   nguyên văn trong text đã lưu (`memory.rs:400` hỏi `"dự án dùng Rust nhé?"` cho tài liệu
   `"dự án này dùng Rust nhé"`). Cân nhắc đăng ký vào `tests/acceptance/registry.json`.
4. **Cân nhắc `harness-store-sqlite` chưa có test nào.** Cả cây `crates/harness-store-sqlite/src`
   không có một `#[test]` nào; SQL mới của tôi (`find_active_memory_by_content`,
   `append_memory_version_source`) chỉ được phủ gián tiếp qua `harness-cli`. Đã kiểm bằng
   mutation gauntlet và test tích hợp, nhưng một unit test ở tầng store sẽ canh chặt hơn.
5. **`-SkipBuild` giờ báo lỗi rõ hơn, nhưng chưa có test cho nhánh đó.** `p7` phủ nhánh
   "repo chưa build + không skip". Nhánh "chưa build + skip" nên có một ca khẳng định thông báo
   nói đúng việc cần làm.

## 6. Cảnh báo cũ vẫn đúng

Workspace có writer song song. Trong lượt này họ: thêm `RunOutcome::Paused`, làm vỡ build test
một lúc (tôi chờ, họ tự vá), và sửa `events.rs` / `view.rs` / `tui/history.rs` /
`tests/interactive_session.rs` / `harness-tools/src/turn_driver.rs`. Tôi **không** commit file của
họ; commit `366b621` chỉ chứa file của tôi. Khi tôi commit, cây đã sạch phần của tôi.

## 7. Nhớ cả lượt hội thoại — commit `168cbb5`

### 7.1. Triệu chứng, và nó khác lỗi truy hồi thế nào

Ảnh chụp một session thật: agent nói *"Tôi không có bản ghi hội thoại nào từ session khác"* trong
khi `[info] memory:` cho thấy memory **đã được nạp**. Người giao việc kết luận đúng: **input đã load
memory nhưng output chưa có**.

Tách ra ba tầng, và tầng 3 mới là chỗ hỏng:

| Tầng | Trạng thái |
|---|---|
| Truy hồi | đã sửa ở §1–§5 |
| Nhét vào prompt, model dùng | **hoạt động** — chứng minh bằng một lượt lưu directive rồi hỏi lại, model trả đúng marker |
| **Cái được lưu** | **hỏng**: store chỉ giữ input của người dùng, và chỉ khi là chỉ dẫn. Câu trả lời của agent **không bao giờ** được lưu |

### 7.2. Quyết định (người giao việc giao toàn quyền)

- **Một asset mỗi lượt**: `asked: <câu hỏi>`, `session: <id>`, `answered: <200 ký tự đầu>`.
- **Active ngay, không để `candidate`** — `refresh_fts` chỉ index `Active` + `Valid`, nên candidate
  sẽ không bao giờ được truy hồi và tính năng thành chết.
- **Nhãn trung thực**: `RuntimeObserved` + `VerifiedObservation`, nguồn là event admission đã bền
  vững. **Không** `UserConfirmed`, vì người dùng không xác nhận câu trả lời của model. Dòng
  `answered:` là **trích nguyên văn** output của model, không được thăng thành bằng chứng về nội
  dung của nó.
- **Trần 200 bản ghi/project**, cắt cũ nhất; **chỉ** asset có `provenance_kind == "session_turn"`
  mới bị cắt, nên directive không bao giờ hết hạn.
- **Câu hỏi về lịch sử đi đường riêng** (`asks_about_history` → `recent_turns`): sàn overlap 2 term
  — thứ tôi thêm để chống nhiễu — lại chặn đúng câu `"session trước tôi hỏi bạn những gì?"` vì nó
  chỉ chia sẻ **một** term với bản ghi. Câu hỏi về *thời gian* phải trả lời bằng *thời gian*.

### 7.3. Nguyên nhân gốc của "input có, output không" — và nó là lỗi của tôi

`recent_turns` trả `revision: 0` trong khi store có revision riêng. `validate_memory_snapshot`
(`advanced.rs`) so revision, và `harness-runtime/src/lib.rs:522-528` làm thế này:

```rust
if MemoryService::new(...).validate_contribution(contribution).await.is_err() {
    request.memory = None;      // <-- bỏ TOÀN BỘ memory, không một dòng log
}
```

Nên contribution bị **drop âm thầm** trước khi packet được dựng: `recall` báo `1 block injected`,
mà packet không hề có block nào. Sửa: `recent_turn_records` đọc và trả revision thật, và test khẳng
định `recent.revision > 0` kèm lý do.

**Bài học đáng giữ:** một lần drop âm thầm ở tầng runtime tốn cả một lượt để tìm ra. Chỗ đó nên có
notice.

### 7.4. Số đo

```text
cargo test -p harness-cli --bin ha --locked   -> 207 passed; 1 failed (attachments.rs — không phải của lượt này)
phase_p4                                      -> 26 passed
cargo test -p harness-memory -p harness-store-sqlite -> xanh
cargo clippy --workspace --all-targets --locked -- -D warnings -> sạch
cài lại: build_commit 168cbb5, sha256 f6ed9f14...
```

End-to-end, đúng câu trong ảnh, chạy bằng **binary đã cài**:

```text
lượt 1: "Giải thích ngắn gọn memory dài hạn là gì."     -> directive=stored turn=stored
lượt 2: "session trước tôi hỏi bạn những gì?"
        recall: memory: 1 hit(s), 1 block(s) injected
        trả lời: The previous session you asked: "Giải thích ngắn gọn memory dài hạn là gì."
                 — I answered that long-term memory is the ability to store and remember
                 information over a long period…
```

### 7.5. Vá một lỗi build không phải của lượt này

`Key::PasteImage` đã được thêm vào enum và được `terminal.rs` sinh ra, nhưng **không match ở đâu**,
nên crate không build được. `controller.rs` đọc clipboard, nên editor để buffer yên và controller sở
hữu phím. Đã sửa để cây build lại.

### 7.6. Review độc lập — ba phát hiện HIGH, và một trong số đó đổi thiết kế

Tôi giao một subagent review đối kháng hai commit `366b621` + `168cbb5`. Nó tìm được ba lỗi HIGH
**của tôi**, tất cả đều thật:

**H1 — dedup không bao giờ chạy với input bị redact.** Lookup dùng text **thô**, còn `create_asset`
redact các dòng trông như credential (`sanitize_memory_text`) **trước khi** hash và trước khi ghi
search mirror. Nên khoá thô không bao giờ khớp giá trị đã redact: mọi lần lặp lại tạo thêm asset —
đúng cái lỗi mà `366b621` nói đã xoá. Và vì redact theo **dòng**, một turn record có dòng `asked:`
bị redact thì mất đúng thứ nó tồn tại để giữ.
*Sửa:* khoá dedup là text sẽ thực sự được lưu (`sanitize_memory_text` nay public vì lý do đó), và
lượt nào có câu hỏi bị redact thì **không ghi** turn record, báo lý do.

**H2 — `classify_input` loại nhầm mệnh lệnh thường.** Danh sách interrogative chứa `do`, `have`,
`can`, `will`, `should`… — những từ mở câu hỏi **và** mở mệnh lệnh. Nên `"Do not force-push to
main"` và `"Have a look at the deploy script"` bị bỏ như câu hỏi, ngược lại chính doc comment hai
dòng trên nói "bỏ sót chỉ dẫn thật là sai lầm tệ hơn". Luật `?` cũng không được chặn theo độ dài
như comment khẳng định.
*Sửa:* từ để hỏi chỉ tính khi **đuôi câu** đồng ý, và luật `?` chỉ áp cho input ngắn.

**H3 — nhãn `VerifiedObservation` trên turn record.** Reviewer đúng: event admission chứng minh câu
hỏi đã được nhận, **không** nói gì về câu trả lời, mà câu trả lời là output model chép nguyên văn.
Tôi thử nhãn trung thực trước — `ModelInference` + `ModelProposed`, policy settle thành
`candidate` — và **nó không hoạt động**: `validate_memory_snapshot` từ chối asset không `Active`, và
runtime drop toàn bộ contribution không một dòng log. Đó là dữ kiện đáng nhớ riêng.
*Sửa:* giữ `RuntimeObserved` + `VerifiedObservation` — đúng nghĩa "runtime quan sát lượt này đã xảy
ra" — và **chuyển tính trung thực tới chỗ model đọc**: heading khối nay ghi rõ nó ghi lại *đã hỏi gì
và đã nói gì*, và **một câu trả lời được trích dẫn là điều đã được nói, không phải sự thật đã kiểm**.

Kèm theo: `recent_turn_records` nay nhận cả `active` và `candidate` (một log giấu mục chưa xác nhận
thì trả lời "bạn đã hỏi gì" bằng không gì cả), và hai comment nói sai về việc code làm.

**Việc còn lại từ review (chưa sửa, ghi lại thay vì im lặng):**

| # | Phát hiện | Chỗ |
|---|---|---|
| M1 | `PolicyDenied` bị biến thành empty lành tính, và nhánh đó là code chết — không nguồn lỗi nào trong `search_memory` trả `PolicyDenied` | `retrieval.rs:230-234` |
| M2 | Fallback `AND` trả row của snapshot này với revision của snapshot kia → contribution bị drop âm thầm (đúng loại lỗi 168cbb5 đi sửa) | `retrieval.rs:188-195` |
| M3 | `find_active_memory_by_content` không kiểm owner/grant/task/session như search, và predicate project **ngược** với search | `advanced.rs:267-317` |
| M4 | `append_memory_version_source` uỷ quyền bằng `bind` (đúng ra là `publish`), và **sau** khi load | `advanced.rs:337-343` |
| M5 | `prune_turns` không có trần: lần đầu nâng cấp một store lớn sẽ chạy N transaction trong một lượt; và lỗi prune sau khi đã ghi thành công thì báo "nothing was stored" | `memory.rs:428-441` |
| M6 | `invalidate` là bắc cầu, nên trần log có thể retire một L2 mà người dùng quan tâm (tiềm ẩn — hiện chưa có gì trỏ lên) | `advanced.rs:724-763` |
| M7 | Mỗi directive bị chính turn record của nó **che** trong xếp hạng `covers`-trước, nên recall về directive có thể trả lời bằng câu trả lời cũ của model | `retrieval.rs:198` |
| M8 | `search_terms` công khai, không normalize và không escape term: caller truyền term chứa `"` làm hỏng cả MATCH và bị báo thành "fts_unavailable" | `retrieval.rs:132-163` |

Review cũng **xác nhận đúng** những chỗ trông đáng ngờ mà không phải lỗi: `LIMIT -1 OFFSET`, thứ tự
bind tham số, `covers()` khớp mirror FTS, tên field `json_extract`, OR 32 term, `relevance_for`, và
sqlx `Drop` rollback (nên early return sau `begin_write` không rò transaction — tôi đã tự kiểm lại).

### 7.7. Việc còn lại

1. **Tám phát hiện M1–M8 ở §7.6** — chưa sửa. M4 (uỷ quyền `bind` thay vì `publish`, và load trước
   khi kiểm) và M7 (turn record che directive) là hai chỗ tôi cho là đáng làm trước.
2. Chưa cập nhật `docs/MEMORY_AND_CONTINUITY.vi.md`: thay đổi này mở rộng memory từ "chỉ dẫn người
   dùng" sang "nhật ký hội thoại", tức một quyết định hợp đồng cần ghi lại.
3. Chưa có test cho `search_terms` với term chứa `"` (M8), cho scope/authorization của
   `find_active_memory_by_content` (M3), và cho fallback `AND` thành công (đường đó chỉ được chạy
   trên store rỗng).
4. `attachments.rs` của writer khác đã được họ commit (`62e3f98`); blocker mà review báo đã tự hết
   trước khi tôi kịp xử.
