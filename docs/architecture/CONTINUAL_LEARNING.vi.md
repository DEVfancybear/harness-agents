# Điều gì còn lại sau một cuộc hội thoại

[English](CONTINUAL_LEARNING.en.md) | Tiếng Việt

Phần ha lấy từ một bài báo đọc ngày 2026-11-12: nó hoạt động thế nào, và phần
nào không lấy.

- **arXiv 2611.14505**, *Continual-ARC: Measuring Skill Acquisition over a
  Working Lifetime* (Lenton, Arif, Mahmood). Một chuỗi 200 câu đố trên 25 task,
  trong đó cùng một task quay lại trong một cuộc hội thoại mới. Bài chạy ba
  harness agent, trong đó có prime-agent (ha port nhiều phần từ đây), và hỏi
  việc lưu giữ của mỗi harness tiết kiệm được bao nhiêu khi một task quay lại.
  Để mặc nó, kết quả là không đo được gì. Các tác giả đọc các kho lưu để tìm lý
  do:
  - Kho của prime-agent mặc định theo session, và một entry chỉ đến được session
    sau khi được ghi ở scope global. Trong 600 session, model không bao giờ làm
    vậy.
  - Harness có lưu một rule bằng lời thì chỉ hiện sáu entry, mỗi entry bị cắt
    còn vài dòng, chọn trước khi biết task. Rule đến được model ở 40 trong 154
    lần task quay lại; khi đến được, lượt đó tốn chưa đến một nửa.
  - Một tin nhắn sau mỗi phán quyết, yêu cầu harness lưu mọi thứ có ích cho các
    lần sau của task, làm prime-agent học được (tiết kiệm 0,42 phần ngân sách
    có thể học, trong khi bản gốc không tiết kiệm gì).
  - Thứ có lợi nhất là một artifact chạy được và đã kiểm thử cho mỗi task, chạy
    trước tiên khi task quay lại (tiết kiệm 0,65 đến 0,81). Một rule bằng lời
    kết thúc bằng "hãy đối chiếu với các ví dụ" lại tốn đúng cái lượt tra cứu
    mà nó định tiết kiệm.
  - Không harness nào giữ lại bằng chứng mà nó đã trả tiền để có. Mỗi lần đều
    mua lại.

Với điểm thứ nhất, thứ hai và thứ tư, ha đã đứng trên nền khác: tool `memory`
buộc model nêu scope, digest được xếp hạng theo yêu cầu, và learned skill có thể
mang `scripts/`. Phần còn lại là các mặc định và lời lẽ quanh chúng, cùng chỗ
mà một phát hiện của bài chưa có phần tương ứng. Đó là sáu thay đổi nhỏ, và hai thay đổi nữa đến từ một lần chạy benchmark (P7, P8).

## Scope: nói rõ ai thấy một entry (P1)

Code: `AUTO_SCOPE_POLICY`, chính sách scope trong prompt của review và
`Edit::observed_project_fact` trong `crates/harness-cli/src/host/memory/refine.rs`;
`SCOPE_REACH` trong `memory/harness.rs`; `_LOCAL_WRITE_NOTE` trong
`crates/harness-cli/python/rlm/harness.py`; phần continual harness trong
`prompting/prompts/core.md`.

| Bài báo | ha | Vì sao |
|---|---|---|
| Mọi mặc định đều trỏ về session; model không bao giờ chọn rộng hơn | Chính sách của review không còn nói edit đi vào kho của cuộc hội thoại trừ khi đánh dấu khác. Local dành cho thứ chỉ cuộc hội thoại này cần; một sự thật về project mà kết quả tool hay một file đã cho thấy thì sống lâu hơn nó | Chính sách chính là mặc định mà model làm theo |
| (như trên) | Một memory entry mới mà review ghi là `about: project`, có trích dẫn và không có scope, được ghi với scope project, gắn project. Entry mà review chọn `local`, entry `about` người dùng, và entry không có trích dẫn thì giữ như cũ | Quyết định là lời của chính review; host không so khớp văn bản |
| Digest của prime-agent bảo model mặc định dùng local | Header của digest nói ai thấy từng scope: global, mọi cuộc hội thoại; project, các cuộc của project này; local, chỉ cuộc này, và mất khi nó kết thúc | Model chọn scope theo điều nó được bảo mỗi scope đến được đâu |
| `global_=False` là mặc định của Python | Giữ nguyên (code của prime-agent dựa vào nó). Một lần ghi không có `global_=True` in ra một dòng: entry chỉ của cuộc hội thoại này, và `global_=True` giữ nó cho các cuộc sau. Entry trả về vẫn như cũ | Mặc định giữ nguyên; cái giá của nó được nói ngay chỗ ghi |

Mọi biện pháp bảo vệ về điều review được ghi vẫn như cũ. Một rule hay một
constraint vẫn cần lời của chính người dùng hoặc một lần lặp lại. Hàng đợi
review, các kiểm tra provenance và ngoại lệ markdown-store áp dụng cho entry
project như cho entry global: nó thuộc kho global, gắn project của nó. Chỉ có
chỗ rơi về mặc định cho một sự thật về project là đổi, và chỉ cho entry mới có
trích dẫn.

## Entry khớp yêu cầu được hiện đầy đủ (P2)

Code: `whole_entry`, `Pass` và `WHOLE_CONTENT_LIMIT` trong `memory/harness.rs`;
`standout_of` trong `crates/harness-types/src/readout.rs`.

| Bài báo | ha | Vì sao |
|---|---|---|
| Sáu entry, cắt còn vài dòng, chọn trước khi biết task | Digest vốn đã xếp hạng theo yêu cầu. Entry nổi bật theo yêu cầu nay được hiện đến 1.200 ký tự thay vì 180 | Rule bị cắt ở 180 ký tự mất đúng bước khiến nó đáng lưu |
| (rule đến được model ở 40 trong 154 lần quay lại) | Một entry nổi bật khi điểm của nó cao hơn trung bình của loại đó một độ lệch, như recall learned skill phán đoán, và nó chung ít nhất hai từ của yêu cầu | Điểm cố định hay danh sách từ không đúng với mọi kho; một từ chỉ một entry có thì không phải là khớp |

`standout_of` là phép chọn mà `skill_recall::best_of` đã dùng đến 0.9.7, tách ra
để cả hai dùng chung: điểm cao nhất phải hơn điểm kế và, với từ năm entry trở
lên, cao hơn trung bình `STANDOUT` độ lệch. Từ 0.9.8 recall có quy tắc riêng
([chiến lược và runtime](STRATEGY_AND_RUNTIME.vi.md)); quy tắc tính cả skill tốt
nhất vào độ lệch gần như luôn tìm ra một skill nổi bật trong thư viện cỡ chục
skill, còn digest giữ nguyên `standout_of`. Mỗi digest có nhiều nhất một entry hiện đầy đủ,
entry tốt nhất qua các loại. Constraint không bao giờ là ứng viên (nó vốn đã
hiện cho mọi yêu cầu), và entry được ghim giữ như cũ. Block digest giữ giới hạn
3.000 token; entry đầy đủ thêm tối đa khoảng một nghìn ký tự.

## Ở điểm kết thúc đã xác minh, hỏi cần giữ gì (P3)

Code: `KeepAsk` và `GoalHost::with_keep` trong `coordination/goal.rs`; nối trong
`application/service.rs`.

| Bài báo | ha | Vì sao |
|---|---|---|
| Một tin nhắn sau mỗi phán quyết yêu cầu lưu mọi thứ có ích cho các lần sau | Khi một goal hoàn thành đã xác minh, kết quả của `goal_complete` kết thúc bằng một đoạn: nếu task này có thể quay lại, hãy lưu ngay thứ giúp làm ngay lần sau. Sự thật về project mà kết quả tool hay file cho thấy: tool `memory`, scope project. Các bước đã chạy được: trong câu trả lời, theo thứ tự, là các lệnh chính xác kèm kết quả cho thấy chúng chạy được, để review giữ thành learned skill, và thành script khi đó là chuỗi lệnh. Memory ghi với scope local mất cùng cuộc hội thoại này | ha chỉ hỏi trước một bản tóm tắt compaction. Một goal đã xác minh là phán quyết duy nhất ha tin |

Đoạn này là văn bản nằm trong một kết quả mà agent vốn phải đọc: không thêm lần
gọi model. Nó chỉ hỏi một lần, vì goal chỉ hoàn thành một lần. Mỗi câu chỉ xuất
hiện khi có tính năng có thể thực hiện nó đang bật (`digest_on` cho tool memory,
`reviews_on` cho review); cả hai tắt thì không thêm gì. Goal bị từ chối thì
không bị hỏi gì.

Agent chính không tự ghi được learned skill; review làm việc đó sau lượt, từ
cuộc hội thoại. Vì vậy đoạn này xin agent thứ review cần đọc, chứ không phải
một lần gọi tool không tồn tại.

## Script đã chạy là script đã kiểm thử (P4)

Code: `SkillScript`, `record_script_runs`, `clear_script_marks` và
`LearnedSkill::scripts` trong `memory/learned.rs`; `confirmed_script_runs`
trong `memory/refine.rs`; `scripts_note` trong `memory/skill_recall.rs`.

| Bài báo | ha | Vì sao |
|---|---|---|
| Một chương trình đã kiểm thử cho mỗi task, chạy trước tiên khi task quay lại | `scripts/` của learned skill được liệt kê trong skill được recall: mỗi script kèm ngày một lần chạy thành công, hoặc "not run yet". Với script đã kiểm thử: chạy nó trước, trước khi làm lại việc bằng tay; nếu nó lỗi, sửa script và nói ra | Rule bằng lời phải nghĩ lại từ đầu; script thì chỉ cần chạy |
| (kiểm thử trước khi lưu) | Review có thể đặt `script_runs` vào intent của skill: `{path, evidence}`, trích dẫn một lần chạy thành công. Host chỉ đánh dấu script đã kiểm thử nếu trích dẫn nằm trong một kết quả tool báo thoát sạch (`exit=Some(0) timed_out=false`) và lời gọi tạo ra kết quả đó nêu tên file của script. Trích dẫn đúng từng chữ và dài ít nhất 12 ký tự (`QUOTE_MIN_CHARS`); prompt của review nói điều đó, con số lấy từ chính hằng số ấy, và nói rằng khi output của script ngắn hơn thì trích thêm từ cùng kết quả, vốn còn có dòng thoát. Trích dẫn ngắn hơn thì lần chạy nằm lại trong `scriptRunsUnconfirmed` | Host không chạy được script do model viết (chính sách phê duyệt cấm), nên "đã kiểm thử" là điều cuộc hội thoại đã cho thấy, được kiểm tra |
| (chương trình đã lưu vẫn phải là chương trình đã chạy) | Dấu đánh dấu giữ SHA-256 của script. Script bị ghi lại, bởi patch hay update, mất dấu; script sửa trực tiếp trên đĩa cũng vậy | Dấu đánh dấu là của nội dung một file |
| Hướng dẫn chuyên gia bảo harness lưu một chương trình | Review được báo rằng một chuỗi lệnh đã giải một task có thể quay lại đáng giữ làm script của skill, kèm trích dẫn lần chạy | Review quyết định giữ gì; không có danh sách từ |

Các dấu đánh dấu nằm trong bản ghi usage của skill (`skill-usage/*.json`), cạnh
`cases`. Danh sách trong block được recall có tối đa tám script và 1.200 ký tự,
script đã kiểm thử đứng đầu, và nằm trước ca đã chạy được và các thất bại, nên
khi block bị cắt trong giới hạn 1.500 token thì chúng mất sau cùng.

Phép kiểm tra có giới hạn. Trích dẫn trong một trang web, một file đã đọc hay
báo cáo của child không phải một lần chạy; lần chạy thất bại hay chạy file khác
cũng không. Lệnh đã chạy script mà kết quả không có dòng exit của host thì không
được xác nhận. Bản ghi của một refinement liệt kê `scriptRuns` và
`scriptRunsUnconfirmed`.

## Tìm ở đây không thấy thì báo khi các cuộc trước có (P5)

Code: `earlier_conversations` trong
`crates/harness-tools/src/runtime/service.rs`;
`ToolOutput::HistorySearch::earlier` trong `contracts.rs`; phần hiển thị trong
`runtime/turn_driver.rs`.

| Bài báo | ha | Vì sao |
|---|---|---|
| Không harness nào giữ bằng chứng nó đã trả tiền để có | ha giữ mọi kết quả tool để tìm được, mặc định scope `task`. Khi tìm trong cuộc này không có nguồn nào chứa đủ mọi từ, cùng truy vấn được chạy ở scope `project` trên cùng index, và kết quả kết thúc bằng `N match(es) in earlier conversations of this project do - search again with scope project` | Model không biết một cuộc trước đã có thứ nó đang mua lại |

Các kết quả trước không được trả về, và scope mặc định không đổi: thứ không được
hỏi thì không gửi. Chỉ đếm các nguồn chứa đủ mọi từ của truy vấn, để dòng này
không được dựng từ các khớp một phần. Các cuộc khác của project được bắt kịp
index trong tối đa một giây; phần còn lại bắt kịp ở nền. Tìm kiểu readout, và
tìm đã yêu cầu scope project, không nói gì thêm. Nếu lần tìm thêm này lỗi thì
số đếm là không, không bao giờ làm hỏng lần tìm.

## Child nhận prompt người dùng đã thêm (P6)

Code: `ChildLaunch::appended_system`, `ChildLaunch::config_dir` và
`inherited_system` trong `coordination/delegation.rs`; `appended_system` trong
`configuration/resources.rs`; các launch trong `application/service.rs` và
`protocols/headless.rs`.

| Bài báo | ha | Vì sao |
|---|---|---|
| Prompt của child thiếu thứ system prompt của cha có | System policy của child kết thúc bằng phần bổ sung theo model của chính model child chạy, rồi văn bản người dùng đã thêm (`--append-system-prompt` hoặc `APPEND_SYSTEM.md`), theo thứ tự của root. Tối đa 4.000 ký tự văn bản của người dùng; chỗ cắt nói độ dài gốc và rằng cha có phần còn lại | "Trả lời bằng tiếng Việt" và "không đụng vendor/" đến được root mà không đến child nào |

Nó là một phần system policy dựng một lần khi child khởi động, nên system prompt
của child giống hệt ở mọi lượt và prefix cache của provider vẫn giữ. Cháu thừa
hưởng nó cùng phần còn lại của launch. `ha exec` dựng launch cho child của nó
theo cùng cách.

## Ghi chú của review không phải lời người dùng (P7)

Code: `plan`, `REVIEW_NOTE_HEADER` và `AUTO_REFINE_REVIEW_SYSTEM_PROMPT` trong
`crates/harness-cli/src/host/memory/refine.rs`.

Review tự động chạy bằng hai lần gọi model: gate quyết định có refine không và
viết ghi chú về thứ đáng giữ, còn planner đề xuất các edit. Planner từng nhận ghi
chú của gate trong block dành cho thứ người dùng gõ sau `/refine`, nên nó tưởng
người dùng đã yêu cầu, ghi như vậy trong rationale và trích câu của gate làm
evidence của edit. Host từ chối các edit đó, đúng: chỉ cuộc hội thoại và thứ
người dùng gõ mới là evidence. Trong một lần chạy 24 cuộc hội thoại, 39 trên
khoảng 50 edit được đề xuất bị từ chối theo cách này. Nay chỉ dẫn người dùng
không gõ (của gate, của agent qua `refine.run`, của `/learn`) đến planner trong
block `<review_note>` nói rõ nó là gì: không do người dùng viết, chỉ ra thứ có vẻ
đáng giữ, không phải yêu cầu của ai và không phải evidence. Evidence của mọi edit
vẫn là trích nguyên văn từ cuộc hội thoại, và edit mà cuộc hội thoại không có lời
nào chứng minh thì bị bỏ. Chỉ thứ người dùng gõ mới giữ
`<user_refine_instructions>`. Gate được yêu cầu điều tương tự từ phía nó: nêu
từng thứ đáng giữ và, với mỗi thứ, lời trong cuộc hội thoại cho thấy điều đó,
chép nguyên văn. Phép kiểm tra của host không đổi.

## Vấn đề sẽ quay lại được giữ theo tên của nó (P8)

Code: `AUTO_SCOPE_POLICY` và prompt của gate trong `refine.rs`.

Các task trong bài quay lại với đầu vào mới và mỗi lần kết thúc bằng một phán
quyết, nhưng các review chỉ giữ một rule chung chung, không bao giờ giữ thứ đã
giải được một task cụ thể. Nay review được bảo, theo cách chung: khi cuộc hội
thoại đã giải một vấn đề có thể quay lại (nó nói vậy, hoặc vấn đề mang tên riêng:
một mã định danh, một file, một dòng lỗi), thứ cuối cùng đã chạy và thứ phân biệt
nó với các lần thử thất bại đáng được giữ ở nơi cuộc hội thoại sau tìm thấy. Đó là
learned skill nếu là một quy trình, còn không là một memory entry về project
(global khi không thuộc riêng project nào), đặt tiêu đề bằng tên của vấn đề để
một yêu cầu nêu tên đó khớp với entry. Evidence là kết quả cho thấy nó chạy được:
kết quả của một tool, hoặc tin nhắn của người dùng nói vậy. Quy tắc từ chối nhiễu
một lần được đọc cùng: một phỏng đoán không kết quả nào xác nhận và các lần thử
thất bại vẫn là nhiễu. Không có danh sách từ nào quyết định; review tự đánh giá.

## Không lấy

- **Benchmark làm bộ test trong ha.** Nó đo harness trên một chuỗi câu đố; nó
  không đổi việc harness làm. Mỗi phần thay vào đó có unit test và kiểm tra
  live riêng.
- **Cập nhật trọng số và học kiểu DAgger từ chuỗi.** ha không huấn luyện model.
- **Thư viện chương trình tự động theo task id** (phương pháp PL của bài, chạy
  chương trình đã lưu trước mà không gọi model). ha không có task id, và chỉ
  biết một yêu cầu là cùng task qua từ ngữ của nó. Script đã kiểm thử của skill
  được recall, tìm theo từ của yêu cầu, là thứ gần nhất ha có, và model vẫn
  quyết định chạy nó.
- **Host tự chạy script lúc promote.** Chính sách phê duyệt cấm host chạy script
  do model viết. Dấu đánh dấu là của một lần chạy mà cuộc hội thoại đã làm.
- **Hỏi sau mỗi phán quyết.** Bài hỏi sau phán quyết của mỗi câu đố. Một lượt
  coding không có phán quyết nào ha tin ngoài điểm kết thúc đã xác minh của goal
  và một lần compaction; hỏi sau mỗi lượt sẽ thêm một đoạn vào từng lượt.
- **Hiện đầy đủ nhiều hơn một entry.** Mỗi entry tốn tới một nghìn ký tự ở mọi
  lượt. Phát hiện của bài nói về entry của task đang làm, tức entry nổi bật.
- **Đổi mặc định của Python thành `global_=True`.** Code của prime-agent và test
  của ha dựa vào `global_=False`; lần ghi nói ra cái giá của nó.
- **Trả kết quả của các cuộc trước khi không được hỏi.** Mặc định của lần tìm
  là cuộc này; số đếm là một dòng, không phải đổi scope.
- **Phát hiện của bài về lời khuyên phương pháp** (bài học chung giúp mỗi lượt
  một chút và không giúp lượt quay lại nhiều hơn). Bài học của ha không đổi.

## Kiểm thử

- Unit, P1: `a_project_fact_the_review_gives_no_scope_is_kept_for_the_project`
  trong `refine.rs`; `the_digest_says_which_conversations_see_each_scope` trong
  `harness.rs`; `test_a_local_write_says_it_is_gone_with_the_conversation`
  trong `crates/harness-cli/python/tests/test_harness_state.py`.
- Unit, P2: `the_entry_that_stands_out_for_the_request_is_shown_whole`,
  `no_entry_is_shown_whole_when_none_stands_out` và
  `one_entry_per_digest_is_shown_whole_and_never_a_constraint` trong
  `harness.rs`; `one_score_stands_out_by_the_spread_of_the_rest` trong
  `readout.rs`; các test recall hiện có trong `skill_recall.rs` chạy qua cùng
  hàm này.
- Unit, P3: `a_verified_goal_asks_what_to_keep_for_the_task_s_return` trong
  `goal.rs`.
- Unit, P4:
  `a_script_run_is_recorded_as_tested_and_writing_it_again_clears_it` trong
  `learned.rs`;
  `a_script_run_is_confirmed_only_by_a_clean_result_of_a_call_that_named_it`
  `a_skill_intent_with_a_confirmed_run_marks_its_script_tested` và
  `a_run_with_a_short_output_is_confirmed_by_quoting_more_of_the_same_result`
  trong `refine.rs`;
  `the_block_lists_the_scripts_and_says_to_run_the_tested_one_first`,
  `the_scripts_list_stays_within_its_bound` và
  `a_recalled_skill_carries_its_scripts_and_a_changed_one_is_untested` trong
  `skill_recall.rs`.
- Integration, P5:
  `a20b_a_search_that_finds_nothing_here_counts_the_earlier_conversations`
  trong `crates/harness-cli/tests/milestone_m5.rs`.
- Unit, P6:
  `a_child_gets_the_appended_text_and_the_additions_of_its_own_model` và
  `what_a_child_inherits_is_bounded_and_empty_when_there_is_nothing` trong
  `delegation.rs`.
- Unit, P7 và P8 trong `refine.rs`:
  `instructions_the_user_did_not_type_reach_the_planner_as_a_note_not_theirs`,
  `an_edit_quoting_the_gate_s_note_is_refused_and_one_quoting_the_conversation_is_kept`
  và `the_review_is_told_to_keep_what_solved_a_problem_that_may_come_back`.
