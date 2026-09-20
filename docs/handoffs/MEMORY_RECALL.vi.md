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
