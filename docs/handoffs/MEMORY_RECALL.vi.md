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

## 4. Gate **chưa** xanh — hai lỗi không thuộc lượt này

`Verify-HaLaunch.ps1 -Json` lần chạy này: `failures: ["regression-phase_p2", "regression-phase_p7"]`.

- **`p2_s02_provider_streams_and_deepseek_sse_adapter_are_normalized`** — chạy **một mình thì
  xanh**. Đúng họ flake loopback đã gặp bốn lần trước (SPEC 3e.4, handoff §15.4). Chưa vá cho ca
  này.
- **`p7_install_script_installs_a_working_binary`** — đỏ **ổn định**, kể cả khi chạy riêng.
  `Install-Ha.ps1:615` đặt `$sourceBinary = $expectedArtifact` trong nhánh `-SkipBuild`, mà
  `$expectedArtifact` trỏ vào repo staged trong temp **chưa được build**, nên dòng 638 ném
  "The artifact is missing". `git blame` cho thấy dòng đó từ `0ed44139` (19/09) — **trước** phiên
  này và không liên quan memory. Đây là lỗi thật của `-SkipBuild`, cần người sở hữu installer quyết.

Vì gate chưa `failures: []`, **objective chưa được coi là xong**.

## 5. Việc còn lại, theo thứ tự

1. **Sửa `-SkipBuild` của installer** (hoặc để người sở hữu làm): khi `-SkipBuild`, phải resolve
   artifact từ repo **đã build**, hoặc báo lỗi nói rõ "repo này chưa build" thay vì nói artifact mất.
2. **Vá flake `p2_s02`** theo đúng cách đã dùng cho `i13` (retry chỉ khi khớp
   `error sending request for url`, có giới hạn).
3. Chạy lại `Verify-HaLaunch.ps1 -Json` tới khi `failures: []`.
4. Chạy 16 ca PTY (`Invoke-HaPtyAcceptance.ps1`) — chưa chạy trong lượt này.
5. Cập nhật `docs/specs/P4.vi.md` §7 (mục retrieval) và `docs/evidence/P4.vi.md` với số đo của
   lượt này — **chưa làm**.
6. Cân nhắc: `tests/acceptance/registry.json` có nên thêm một ca cho truy hồi bằng câu tự nhiên
   không. Đây là lỗ hổng đã để lọt cả bốn lỗi: **không ca C nào hỏi bằng câu paraphrase** — cả ba
   test end-to-end cũ dùng query mà term có nguyên văn trong text đã lưu.

## 6. Cảnh báo cũ vẫn đúng

Workspace có writer song song. Trong lượt này họ: thêm `RunOutcome::Paused`, làm vỡ build test
một lúc (tôi chờ, họ tự vá), và sửa `events.rs` / `view.rs` / `tui/history.rs` /
`tests/interactive_session.rs` / `harness-tools/src/turn_driver.rs`. Tôi **không** commit file của
họ; commit `366b621` chỉ chứa file của tôi. Khi tôi commit, cây đã sạch phần của tôi.
