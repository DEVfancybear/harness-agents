# Tự đánh giá một lượt đã xong

[English](SELF_EVALUATION.en.md) | Tiếng Việt

Câu trả lời cuối của một lượt được chấm trên năm trục chất lượng. Người chấm là
một lần gọi model riêng của host, không phải agent tự chấm trong chính lượt của
nó. Tính năng này lấy từ skill
[`agent-self-evaluation`](https://github.com/affaan-m/ECC/tree/main/skills/agent-self-evaluation)
của ECC. ECC chỉ mô tả cách đánh giá. ha chạy nó, kiểm tra nó, hành động theo
nó một lần và lưu nó lại.

Code: `crates/harness-cli/src/host/memory/self_eval.rs` (người chấm, báo cáo,
log và thống kê) và `TurnEvaluation` trong
`crates/harness-tools/src/runtime/turn_driver.rs` (cổng điều kiện và việc gửi
lượt quay lại). Phần nối dây nằm ở `host/application/service.rs` (chat) và
`host/protocols/headless.rs` (`ha exec`).

## 1. Đối chiếu với skill của ECC

| ECC | ha |
|---|---|
| Năm trục trọng số bằng nhau, mỗi trục một số nguyên từ 1 đến 5: accuracy, completeness, clarity, actionability, conciseness | Cùng các trục, thang điểm và cách diễn đạt, nằm trong prompt của người chấm |
| Mọi điểm dưới 5 phải dẫn bằng chứng | Có kiểm tra: một trục dưới 5 mà không có bằng chứng làm bản đánh giá không hợp lệ. Nó được ghi lại là không hợp lệ. Không điểm nào bị bịa ra hay bị nâng lên |
| Luật biên: yêu cầu mơ hồ không bị trừ điểm, việc đơn giản vẫn có thể đạt 5, lỗi đã sửa trong lượt vẫn đạt 5, kết quả tool mâu thuẫn với một khẳng định thì accuracy tối đa 2 | Nằm trong prompt. Người chấm đọc kết quả tool của lượt, nên thấy được chỗ mâu thuẫn |
| Anti-pattern: toàn điểm 5 không có bằng chứng, trừ điểm vì phạm vi không ai yêu cầu, lấy sở thích làm bằng chứng | Nằm trong prompt |
| Tự kiểm: "người dùng có đồng ý không?" | Được hỏi, và lưu thành `user_would_agree` kèm một câu giải thích |
| Điểm chung = trung bình, một chữ số thập phân; kết luận theo trục yếu nhất | Host tự tính từ các điểm, không lấy từ model |
| Mẫu báo cáo (thanh điểm, `OVERALL`, `CRITICAL ISSUES`, `TOP IMPROVEMENTS`, `VERDICT`) | `/self-eval` in ra đúng mẫu đó |
| `evaluate.py` chấm bằng biểu thức chính quy | Không dùng heuristic trên chữ. Model chấm, host chỉ kiểm tra hình dạng câu trả lời của nó |
| Không lưu gì | Mỗi lần đánh giá là một dòng trong `evaluations.jsonl`, và `ha learning report` tổng hợp lại |
| Hook chỉ in ra một lời nhắc mà model có thể không bao giờ thấy | Turn driver gửi lượt yếu quay lại một lần, kèm bằng chứng |

Những khác biệt có chủ ý:
- **Người chấm đứng ngoài lượt.** Một agent tự chấm câu trả lời của mình trong
  cùng lượt là đang kiểm lại chính suy luận của nó. Host gọi riêng model phụ
  (helper), và hội thoại được đưa vào dưới dạng dữ liệu, không bao giờ là chỉ
  dẫn.
- **Không phải bài học.** Điểm không bao giờ đi vào vòng học
  (`LEARNING_LOOP.vi.md`). Việc của agent do một model chấm không phải là tín
  hiệu độc lập về điều người dùng muốn. Thống kê được in cạnh các bài học,
  không bao giờ nằm trong số đó.

## 2. Khi nào một lượt được đánh giá

Điều kiện kích hoạt của ECC là: làm việc trên 3 file trở lên hoặc 50 dòng trở
lên, một quy trình nhiều bước, debug mất từ 3 lần thử, một thiết kế hay một
phân tích, hoặc người dùng yêu cầu. Nó bỏ qua lời gọi tool đơn lẻ và hội thoại
thuần. ha chỉ dùng những điều kiện kiểm được từ cấu trúc:

- lượt đã đổi **từ 3 file trở lên** (`files`);
- các thay đổi chạm **từ 50 dòng trở lên** (`lines`): các dòng `+`/`-` trong
  diff của mỗi lần edit, và mọi dòng của một lần ghi cả file (là cận trên khi
  file đã có từ trước);
- lượt **hoàn thành một goal**: `goal_complete` đã qua trong lượt đó (`goal`);
- **người dùng yêu cầu** bằng `/self-eval` (`manual`).

Một lượt chỉ trò chuyện, hoặc chỉ có một thay đổi nhỏ, thì không được chấm.
"Một thiết kế hay một phân tích" và "debug mất 3 lần thử" cần phải phán đoán
chữ mới nhận ra được. Quy tắc của maintainer là không danh sách từ nào được
dùng để phán đoán chữ, nên các trường hợp này để cho `/self-eval`.

## 3. Chuỗi kiểm tra cuối lượt

Khi câu trả lời của lượt không có lời gọi tool, các bước kiểm chạy theo thứ tự,
và nhiều nhất một bước gửi lượt quay lại:

1. Goal evaluator, khi lượt có goal spec (`ha exec --goal`).
2. Stop hook.
3. **Verify reminder hoặc jev** (`GUARDS.vi.md`). Bước này tất định: code đã
   đổi và không lệnh nào chạy sau thay đổi cuối. Khi completion check của jev
   bật, jev phán đoán ở đây thay cho reminder.
4. **Tự đánh giá.** Chỉ chạy khi không bước nào ở trên đã gửi lượt quay lại,
   completion check của jev tắt (khi bật thì jev phán đoán), còn bước để chạy,
   lượt chưa được đánh giá lần nào, và cổng điều kiện ở trên thỏa.

Kết quả làm gì:
- **Trục yếu nhất ≤ 2:** lượt được gửi quay lại một lần với `Before you
  finish: a self-evaluation of this turn scored <axis> N/5 (<evidence>; fix:
  <improvement>). Fix it, or say plainly why it stands.`
- **Trục yếu nhất = 3:** lượt được gửi quay lại một lần, được yêu cầu sửa các
  trục đó hoặc nêu chúng ra trong câu trả lời.
- **Còn lại:** không gửi gì và lượt kết thúc.

Hội thoại hiện `self-evaluation: an axis scored low; asking for a fix`. Một
lượt được đánh giá nhiều nhất một lần và bị gửi quay lại nhiều nhất một lần.
Câu trả lời sau lần gửi quay lại kết thúc lượt.

Nếu người chấm không trả lời được, hoặc trả lời sai hình dạng, thì không gửi gì
quay lại. Lỗi đó được ghi lại.

## 4. `/self-eval [trọng tâm]`

`/self-eval` chấm lượt gần nhất đã kết thúc bằng một câu trả lời. Khi có trọng
tâm, nó yêu cầu người chấm xem kỹ điểm đó. Nó in báo cáo theo mẫu của ECC:

```
AGENT SELF-EVALUATION REPORT
Overall score 3.8/5 across 5 quality axes.

Accuracy       ████░ 4/5
  Evidence: ...
  Improvement: ...
...
OVERALL 3.8/5

CRITICAL ISSUES (axes <= 2)
  - Completeness (2/5): ...

TOP IMPROVEMENTS
  1. Completeness (2/5): ...

VERDICT: REDO: fix the critical issues before delivering
Would the user agree: yes - ...
```

Nó không bao giờ gửi agent quay lại. Báo cáo là để người dùng đọc. Khi một lượt
đang chạy, `/self-eval` chờ lượt đó xong. Không có CLI `ha self-eval`. `ha
exec` tự chấm, và `ha learning report` đọc kết quả.

## 5. Công tắc

- `"selfEvaluation": "auto" | "manual" | "off"` trong `settings.json` (mặc
  định `auto`). `manual` chỉ giữ `/self-eval`. `off` tắt cả hai.
- `HA_SELF_EVAL=auto|manual|off` ghi đè setting.
- Agent con được ủy quyền không bao giờ bị chấm: agent cha phán đoán kết quả
  của chúng.
- `ha exec` được chấm như chat khi chế độ là `auto`. Mock profile thì không.

## 6. Những gì được lưu

Mỗi lần đánh giá, tự động hay thủ công, thêm một dòng vào
`<data dir>/harness/evaluations.jsonl`:
- `at`, `conversation`, `project` (khóa project), `trigger`, `changed_files`;
- `scores` theo từng trục, `mean`, `weakest`, `verdict`, `sent_back`,
  `user_would_agree`;
- hoặc `valid: false` kèm `error`.

Yêu cầu chỉ được giữ 200 ký tự đầu, sau khi các luật bí mật của learned skill
đã che (redact) nó. Nội dung lỗi cũng được che như vậy. Câu trả lời và kết quả
tool không được lưu.

Log được chuyển sang `evaluations.jsonl.old` khi vượt 5 MB, như log stderr của
kernel.

`ha learning report` kết thúc bằng 30 ngày gần nhất:
- bao nhiêu lần đánh giá hợp lệ và bao nhiêu lần bị từ chối;
- tỉ lệ bị gửi quay lại;
- theo từng trục: điểm trung bình và số lần;
- trục yếu nhất.

Mỗi lần đánh giá cũng báo `self_eval evaluated` vào chẩn đoán (diagnostics),
kèm `trigger`, `mean`, `min_axis`, `min_score`, `verdict` và `sent_back`. Lần
bị từ chối báo `self_eval invalid`. Cả hai đều có mặt trong bug report.

## 7. Kiểm thử

- `turn_driver.rs`:
  - `a_turn_is_evaluated_only_when_it_did_enough`;
  - `the_reminder_and_jev_come_first_and_a_turn_is_judged_once`;
  - `changed_lines_are_counted_per_file`;
  - `the_review_names_each_result_by_its_tool_and_keeps_its_end`.
- `self_eval.rs`:
  - `an_evaluation_out_of_shape_is_refused` (ngoài khoảng, số lẻ, thiếu trục,
    thiếu bằng chứng);
  - `the_verdict_follows_the_weakest_axis`;
  - `only_a_weak_axis_sends_the_turn_back`;
  - `the_report_follows_ecc_template`;
  - `the_switches_set_the_mode`;
  - `the_stats_read_the_last_thirty_days`;
  - `a_full_log_is_moved_aside`;
  - với mock provider: `a_weak_turn_is_sent_back_kept_and_reported`,
    `an_invalid_evaluation_is_kept_as_such_and_sends_nothing`,
    `self_eval_without_a_turn_says_so`.
- `tests/milestone_m4.rs`, turn driver thật với provider theo kịch bản:
  - `self_evaluation_sends_a_qualifying_turn_back_once`;
  - `self_evaluation_waits_behind_the_verify_reminder`.
