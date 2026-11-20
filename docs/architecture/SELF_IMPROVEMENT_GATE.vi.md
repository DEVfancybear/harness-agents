# Điều review được báo trước khi lên kế hoạch lần nữa

[English](SELF_IMPROVEMENT_GATE.en.md) | Tiếng Việt

ha lấy gì từ hai bài báo, nó hoạt động ra sao, và điều gì không lấy:

- **[GUARD] arXiv 2611.23476.** Vòng tự cải thiện được chặn bằng *regret*:
  mỗi tiêu chí mà một ứng viên được chấm được phân biệt là **regressed** (trước
  khi sửa thì đạt, giờ thì hỏng), **chưa giải được** (unsolved) hay **đã thành
  thạo** (mastered), và mỗi loại được xử lý khác nhau. Một **sổ cái** các sửa
  đổi bị từ chối (`L_max` cái gần nhất) được đọc lại mỗi vòng, để bên đề xuất
  không đưa chúng ra lần nữa. Một **cổng Soft-HURT** chỉ cho ứng viên lên khi
  nó không làm hại điều đã chạy tốt, với một lần thử sửa phạm vi cho ứng viên
  làm hại.
- **[SIM] arXiv 2611.18856.** Một bộ nhớ tự cải thiện giữ, cho mỗi lần thử - được
  chấp nhận *và* bị thoái lui - giả thuyết nguyên nhân gốc, sửa đổi, kết quả đã
  dự đoán và kết quả quan sát được, và đưa nguyên văn cho bên đề xuất. Bên đề
  xuất chỉ thấy cái đã hỏng sẽ lặp lại đúng nguyên nhân mà nó đã thử.

Mã: `crates/harness-cli/src/host/memory/refused.rs` (sổ cái),
`learned.rs` (`Revision`, `Standing`, `begin_trial`, `settle_trial`,
`library_overview`) và `refine.rs` (`apply`, `plan`, `planner_given`). Về nơi bản
sửa và đợt chạy thử của nó được quyết định, xem [chi phí và
skill](COST_AND_SKILLS.vi.md) mục 4.

## 1. ha 0.9.14 đã có gì, và đã đổi gì

| GUARD / SIM | ha 0.9.14 | Đã đổi gì |
|---|---|---|
| Sổ cái các sửa đổi bị từ chối, đọc lại mỗi vòng | Một learned skill giữ 5 bản sửa bị từ chối gần nhất và hiện chúng thành các dòng "rejected before". Một sửa đổi **memory hoặc ghi chú prompt** mà host từ chối chỉ để lý do trong bản ghi của refinement (`review.applied`), nên review sau lại lên kế hoạch đúng sửa đổi đó | Mỗi scope giữ các sửa đổi memory và prompt mà host từ chối (`refused.json`, tối đa 12, loại trùng); mọi planner đề xuất sửa memory nhận một phần `<refused_before>` |
| Regressed / unsolved / mastered | Bộ đếm của skill được hiện (judged worked và failed, checks, "last failure"), nhưng không skill nào được phân biệt là đã *mất* thứ gì | Mỗi learned skill được đánh dấu `regressed`, `unsolved` hoặc `mastered` từ bản ghi sử dụng, mỗi loại có một chỉ dẫn; skill mastered hiện những gì nó đã chạy tốt |
| Bộ nhớ giả thuyết, sửa đổi, dự đoán, kết quả | Đợt chạy thử giữ các sửa đổi và bộ đếm của bản cũ; bản sửa bị rollback giữ một lý do | Một bản sửa mang `cause` và `expect`; đợt chạy thử, bản ghi accepted và rejected giữ chúng cùng số lần dùng quan sát được |
| Cả các lần thử được chấp nhận cũng được giữ | Chỉ giữ các bản sửa bị từ chối; bản được chấp nhận để lại một dòng ledger và không có gì planner nhìn thấy | 5 bản sửa được chấp nhận gần nhất của mỗi skill được giữ và hiện cùng các bản bị từ chối, thành một lịch sử |
| Ứng viên được kiểm với điều đã chạy tốt, kèm sửa phạm vi | Bản sửa vào chạy thử và bị rollback khi các lần dùng được chấm của nó tệ hơn (SkillOpt, mục 4 tài liệu chi phí); lượt repair sửa một intent bị từ chối | Không đổi (xem "Không lấy") |

## 2. Sổ cái các sửa đổi bị từ chối

Một sửa đổi memory hoặc ghi chú prompt mà review đã lên kế hoạch và host từ chối
được giữ trong scope mà nó được áp dụng, ở `refused.json` cạnh state của scope
đó: `<data>/harness/refused.json` cho scope global và
`<data>/sessions/<task>/harness/refused.json` cho scope local của một cuộc hội
thoại. Một sửa đổi là bị từ chối khi nó **không được áp dụng và có lỗi**; sửa đổi
**bị bỏ qua** (entry đổi trong lúc review lên kế hoạch, một lần gộp mà bản giữ
lại chưa được ghi) hoặc **đang chờ người dùng** trong hàng đợi memory thì không,
và một rollback hay một thay đổi hàng đợi đã được chấp nhận không phải sửa đổi
của planner.

```json
{"refused": [
  {"at": "2026-11-19T08:15:02Z", "kind": "memory", "action": "create",
   "target": "deploy_key", "reason": "the harness never keeps secrets; this looks like ...",
   "summary": "Deploy key: The deploy key is [REDACTED]",
   "evidence": "the deploy key is [REDACTED]"}
]}
```

- **Có giới hạn và loại trùng.** Tối đa 12 bản ghi mỗi scope (`KEPT`, `L_max`
  của GUARD), bản cũ nhất bị bỏ. Bản ghi cùng kind, target và lý do thay bản cũ.
  Lý do, tóm tắt điều đã đề xuất và trích dẫn bằng chứng mỗi thứ một dòng, cắt
  còn 300, 200 và 200 ký tự, và được che như văn bản memory.
- **Sửa đổi bị từ chối vì an toàn không giữ chữ nào của nó.** Sửa đổi bị từ chối
  vì chứa bí mật, văn bản điều khiển (mẫu injection hoặc exfiltration), hoặc lời
  của một trang web bị biến thành luật (và sửa đổi của Deep Dream có câu trích
  chứa bí mật) được đánh dấu `safety` ngay chỗ bị từ chối. Bản ghi của nó chỉ giữ
  kind, action, lý do, và target: id của entry khi sửa đổi có nêu id, `a new
  entry` với lệnh create (id và tiêu đề của entry mới chính là lời của sửa đổi).
  Không có tóm tắt, không có câu trích, và phần prompt ghi *refused for safety:
  its words are not kept*, nên sổ cái không mang chữ đó trở lại mọi prompt sau.
- **Đọc lại.** Mọi planner của sửa đổi memory - review tự động, `/refine` và
  `/learn` (`plan`), và curator theo lô của Deep Dream (`memory_edits`) - nhận một
  phần `<refused_before>`, mới nhất trước, tối đa 12 trên cả hai scope, sau câu
  này: *The host refused these memory and prompt edits earlier, with why. Do not
  propose the same change again unless the conversation shows something the
  refusal did not know.*
- **Diagnostics.** `refine` / `refusal_kept` cho mỗi bản ghi được giữ (và
  `refine` / `refusal_not_kept` khi không đọc hoặc ghi được file; sổ cái không
  đọc được thì không bị ghi đè). `refine` / `planner_given` được ghi cho mỗi lần
  lên kế hoạch, chỉ gồm tên các phần của prompt và số đếm (số dòng bị từ chối,
  standing, dòng "worked before" và "revised", đều đếm từ chính văn bản prompt
  mà planner nhận, không phải từ thư viện): bản thân prompt không được lưu,
  nên bug report và các live test thấy được rằng các phần đã tới planner.

## 3. Nguyên nhân và kỳ vọng của một bản sửa, và lịch sử của nó

Planner được yêu cầu, trong định dạng learned-skill, đưa cho mọi `edit`, `patch`
hoặc `update` của một skill đã có hai câu tùy chọn: `cause`, nguyên nhân gốc của
lỗi hay chỗ thiếu mà bản sửa nhắm tới, và `expect`, điều mà việc làm theo skill đã
sửa giờ phải làm khác đi.

- Khi một bản sửa được áp dụng, `begin_trial` giữ cả hai trong đợt chạy thử. Các
  thay đổi nhập vào một bản sửa thì nối các nguyên nhân (`a; b`); một bản vá thay
  cho bản sửa bị chấm tệ hơn thì bắt đầu bằng nguyên nhân của riêng nó.
- Khi đợt chạy thử kết thúc, bản sửa **accepted** được giữ trong danh sách
  `revisions` của skill: `{at, kind: "accepted", ledger_id, cause, expect,
  edits_summary, good, bad}`, 5 bản gần nhất. Một bản sửa bị bản sau thay thế
  khi chưa bị chấm tệ hơn cũng được giữ là accepted. Bản **rolled-back** hoặc
  **superseded** giữ `cause`, `expect` và số `good`, `bad` quan sát được trong
  danh sách `rejected`; bản **refused** giữ `cause` và `expect` (nó chưa từng
  chạy nên không có số đếm).
- Góc nhìn thư viện mà planner, curator và lượt repair đọc gộp ba nguồn thành
  một lịch sử, cũ nhất trước, ba dòng cuối (bản đang chạy thử ở cuối):

  ```
  revised 2026-11-17 (rolled_back): cause: the port was busy; expected: it frees the port first; observed: 0 good, 2 bad; why not kept: judged worse than the version it replaced ...; tried: append "Use --fast."
  revised 2026-11-19 (on trial): cause: the flag name was wrong; expected: the flag works; observed: 0 good, 1 bad; tried: append "Use --lean."
  ```

  Bản ghi không có cause (từ trước) in ra không có nó. Các câu giải thích những
  dòng này nói: bản sửa bị rolled back, refused hoặc superseded đã không trụ lại,
  nên đừng đề xuất lại, và khi nguyên nhân của nó đúng nhưng thay đổi sai thì hãy
  xử lý cùng nguyên nhân bằng cách khác; bản accepted thì đang có hiệu lực.

## 4. Regressed, unsolved, mastered

Bản ghi sử dụng của mỗi learned skill cho nó một standing, không gọi model
(`LearnedSkill::standing`), hiện thành `; status: ...` trong góc nhìn thư viện:

| Standing | Khi nào | Planner được báo gì |
|---|---|---|
| `regressed` | một bản sửa đang chạy thử, ít nhất một lần dùng được chấm kể từ đó đã hỏng, và bản cũ (trước bản sửa) vốn tốt: nó đã chạy tốt, hoặc check của nó đã qua | Thu hẹp khi nào văn bản mới áp dụng: thêm điều kiện ("When <condition>: ...") quanh các bước đã đổi, thay vì viết lại điều bản cũ đã làm |
| `mastered` | nó đã chạy tốt ít nhất hai lần và chưa hỏng kể từ lần sửa gần nhất (hoặc chưa bao giờ, nếu chưa từng sửa) | Chỉ sửa để thêm; không bao giờ bỏ hay thay một bước mà các dòng "worked before" của nó dựa vào. Hai case gần nhất của nó (câu trích bằng chứng, cắt còn 200 ký tự) hiện dưới nó thành `worked before: ...` |
| `unsolved` | nó đã hỏng (hoặc một check hỏng sau nó) và chưa từng chạy tốt, và không có đợt chạy thử nào của nó là regressed | Có thể viết lại |

"Kể từ lần sửa gần nhất" là `trial_good` / `trial_bad` của bản ghi sử dụng, mà mỗi
bản sửa đặt lại từ đầu. Đợt chạy thử chỉ có lần dùng tốt thì không regressed; một
skill chạy tốt hai lần rồi hỏng một lần kể từ lần sửa gần nhất không thuộc loại
nào trong ba loại. Các câu này cũng nằm trong văn bản định dạng learned-skill mà
planner, curator và lượt repair đọc, và prompt repair yêu cầu giữ điều kiện hoặc
"chỉ thêm" khi sửa một intent.

**Cắt gọn (prune) không phải một bản sửa như vậy.** Quy tắc "chỉ thêm" nói về các
bản sửa làm đổi điều skill làm. Lượt prune (`ha learning prune`, [chi phí và
skill](COST_AND_SKILLS.vi.md) mục 5) đọc cùng văn bản định dạng và cùng status, và
skill mastered là ứng viên quen thuộc của nó: prompt của prune nói rằng prune được
rút gọn câu chữ và bỏ những gì không dòng "worked before" nào dựa vào, nhưng giữ
mọi bước mà các dòng đó dựa vào, và không thêm quy tắc.

## 5. Không lấy

- **Cổng Soft-HURT của GUARD** (kiểm một bản sửa với điều đã chạy tốt trước khi nó
  được áp dụng, kèm một lần thử sửa phạm vi). Người bảo trì chọn không lấy: nó
  thêm một lần gọi model cho mỗi bản sửa của skill có case, và đợt chạy thử trực
  tuyến kèm rollback thay vào chỗ đó - bản sửa làm hại điều đã chạy tốt được chấm
  bởi các lần dùng sau và bị đặt lại khi hai lần hỏng. Dấu `regressed` và chỉ dẫn
  "thu hẹp bằng một điều kiện" là phần regret của GUARD mà planner nhận ở đây,
  không có bước kiểm trước khi áp dụng.
- **M >= 2 ứng viên mỗi vòng của GUARD, và ContrastEvolve**: chúng nhân số lần gọi
  model của mọi review lên M và thêm một lượt đối chiếu; ha đề xuất một bản sửa
  mỗi review.
- **Checklist theo từng task của GUARD chấm theo từng tiêu chí**: ha chấm một lần
  dùng skill như một khối (worked hay failed, kèm một câu trích), không chấm từng
  tiêu chí.
- **Chạy lại các task cũ để đo ứng viên**: các cuộc hội thoại cũ của một coding
  agent có tác dụng phụ (file đã ghi, lệnh đã chạy, lời gọi đã gửi); ha không phát
  lại chúng. Các case đã ghi của skill (câu trích bằng chứng) là điều nó phải tiếp
  tục chạy được, hiện cho planner, và các lần dùng thật sau đó là phép thử.
- **"Harnesses That Improve Themselves" (arXiv 2611.22299)**: nó tiến hóa chính mã
  của harness. Runtime của ha là Rust đã biên dịch, và các cơ chế của nó (kiểm
  tra trước khi dừng, bộ ngắt lời gọi lặp) là mã cố định, không phải văn bản mà
  model viết lại.
- **"Inference after coding agents" (arXiv 2611.23174)**: một quy trình thống kê
  cho phân tích dữ liệu sau một lượt chạy của agent; nhiều nhất là một skill,
  không phải cơ chế của harness.
- **"Recirculation" (arXiv 2608.17981)**: cần activation của model, thứ mà model
  được lưu trữ từ xa không cung cấp.

## 6. Kiểm thử

- Unit test: `refused.rs` (bị từ chối là có lỗi; được giữ, loại trùng, giới hạn
  12, được che; sửa đổi bị bỏ qua và đang chờ không được giữ; sổ cái không đọc
  được không bị ghi đè; phần prompt và chỉ dẫn của nó); `refine.rs`
  (`an_edit_the_host_refused_is_kept_and_the_next_planner_is_told`,
  `a_skipped_or_queued_edit_is_not_kept_but_a_refused_one_among_them_is`,
  `the_planner_is_told_a_skills_standing_and_asked_for_a_cause`,
  `an_edit_refused_for_safety_is_kept_without_its_words`,
  `the_planner_given_record_counts_the_standings_from_the_prompt`; `refused.rs` also
  `a_refusal_for_safety_keeps_no_word_of_the_edit_and_an_ordinary_one_keeps_them`); `learned.rs`
  (`a_revisions_cause_and_expectation_live_in_its_trial_and_its_history_line`,
  `a_rolled_back_a_superseded_and_a_refused_revision_keep_their_cause_and_counts`,
  `the_accepted_history_keeps_the_last_five`,
  `a_skill_is_regressed_unsolved_or_mastered_by_its_usage_record`).
- Live: `scripts/testing/live-tests/guard.sh`, với model thật: một sửa đổi memory
  bị từ chối được giữ trong `refused.json` và báo cho review sau, các dòng
  standing và lịch sử tới được planner, và nguyên nhân cùng kỳ vọng của một bản
  sửa được giữ trong đợt chạy thử của nó.
