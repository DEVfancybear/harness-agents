# Một turn tốn bao nhiêu, và skill được huấn luyện như trọng số

[English](COST_AND_SKILLS.en.md) | Tiếng Việt

ha lấy gì từ bốn bài báo (đọc trọn vẹn ngày 2026-11-18), nó hoạt động thế
nào, và cái gì không lấy:

- **[P1] arXiv 2611.19823**, *EfficientSkill: Learning Skills for
  Token-Efficient Multi-Turn Agents*. Trước hết làm cho skill chạy được, không
  phạt độ dài (Pha 1); rồi từ skill đã chạy được đó, cắt gọn nó (Pha 2):
  hướng dẫn ngắn hơn, ít turn hơn, code lặp lại chuyển vào script trợ giúp.
  Một bản cắt chỉ được giữ khi độ chính xác vẫn trên 0,9 lần bản đang chạy và
  token đầu ra giảm. Token đầu ra giảm 38-80% trên 9/12 cặp model-benchmark và
  độ chính xác tăng trên cả ba benchmark. **Kết quả âm:** cắt thẳng từ skill
  hạt giống thì kẹt (3,6% so với 72,5% trên SpreadsheetBench); Grok trên
  ALFWorld chỉ tiết kiệm 3,2%, Gemma còn sinh thêm 4,4%; chạy tìm kiếm chỉ theo
  độ chính xác lâu hơn không giúp; trên các lần chạy một turn rẻ, chi phí gọi
  optimizer lớn hơn phần tiết kiệm được.
- **[P2] arXiv 2611.12122**, *System Optimizations for Cost-Efficient Agent
  Harnesses* (Agency). Harness gần như không đổi độ chính xác nhưng đổi chi
  phí khoảng 2 lần. Đọc lại context đã cache chiếm phần lớn chi phí ở các
  phiên dài (tới 73%), chỉ 27% token đầu ra của tool được dùng lại sau đó, và
  mỗi lần gọi tool lại đọc lại context. Năm kỹ thuật cộng lại đưa chi phí về
  0,77 với tỷ lệ độ chính xác 0,97. **Kết quả âm:** prompt gom lời gọi tool làm
  GPT-6-Luna tốn thêm 2% (nó thêm lời gọi thừa); chỉ mục SQLite các file hầu
  như không được truy vấn trên tác vụ code (0,2 lần mỗi lần chạy) và Luna dùng
  sai; 17-29% tác vụ tốn hơn bản gốc.
- **[P3] arXiv 2605.23904**, *SkillOpt: Executive Strategy for Self-Evolving
  Agent Skills*. Tài liệu skill được huấn luyện như trọng số: vài sửa đổi có
  cấu trúc mỗi bước ("learning rate bằng chữ", mặc định 4), một cổng held-out
  chỉ nhận sửa đổi làm tốt hơn hẳn, một bộ đệm các sửa đổi bị từ chối được đưa
  lại, và một bước cập nhật chậm theo epoch trong vùng được bảo vệ. Tốt nhất
  hoặc hòa trên 52/52 ô; skill cuối dài 379-1.995 token từ 1-4 sửa đổi được
  nhận. **Kết quả âm (ablation):** viết lại không giới hạn thì kém hơn
  (84,6/75,7/57,3 so với 87,1/77,5/61,3), bỏ bộ đệm từ chối cũng vậy (-1,6 tới
  -4,6 điểm), bỏ cập nhật chậm/meta mất 22,5 điểm trên SpreadsheetBench; kích
  thước batch và lịch trình ít ảnh hưởng.
- **[P4] arXiv 2611.17167**, *Actor, Judge, or Teacher? Understanding
  Decision Models in Long-Horizon Agents*. Một decision model hành động kém
  (JEV: 14,7% trên tác vụ có subgoal phải làm theo thứ tự) lại chấm tốt khi
  nó xem cả quỹ đạo đã xong (Hindsight) và được hỏi mỗi hành động có giúp đạt
  mục tiêu không (Progress) thay vì task có thành công không (Value): Progress
  đặt hành động milestone cao hơn hành động vô ích 0,36, Value chỉ 0,05. Làm
  teacher trong RL nó nâng một policy 1,5B từ 72,8% lên 82,9% trên ALFWorld.
  **Kết quả âm:** lời nhắc "hãy xét các phụ thuộc" chỉ đổi hành động -2,8 tới
  +5,2 điểm; khi chỉ thấy phần trước (prefix), hai câu hỏi dạy ngang nhau.

## 1. ha đã tương đương chưa? Các bảng gap

### P1 EfficientSkill

| Bài báo | ha 0.9.11 | Gap, và đã làm gì |
|---|---|---|
| Skill gồm hướng dẫn và helper trong workspace, thân helper không vào prompt | Learned skill là `SKILL.md` cộng `scripts/`, `references/` (tối đa 16 file); skill được recall liệt kê đường dẫn từng script và bảo chạy script đã kiểm thử trước | **Tương đương** |
| Pha 1: sửa theo thất bại cho tới khi chạy được | Review và `/refine` vá skill theo kết quả được chấm; `unproven()` cho tới khi có một lần dùng thành công | Tương đương về ý; cổng đo lường ở §4 |
| Chi phí của skill khi dùng: token đầu ra và số turn | Không đo: bản ghi usage chỉ đếm lần offer, dùng, xem và kết quả | **Gap.** Đã làm: §5, chi phí từng skill đang dùng |
| Pha 2: chỉ cắt skill đã chạy được, giữ khi độ chính xác ≥ 0,9·A* và token giảm | Không có bước nén; độ dài chỉ bị lint giới hạn | **Gap.** Đã làm: §5, bước prune |

### P2 Agency

| Bài báo | ha 0.9.11 | Gap, và đã làm gì |
|---|---|---|
| Chi phí = cache read + cache write + input chưa cache + output, mỗi phần theo giá của nó | `calculate_cost` với bốn mức giá | Tương đương |
| Tiền đi vào đâu, theo từng phần | `/cost` cho một tổng; `ha exec --json` cho token, không có tiền | **Gap.** Đã làm: §3 |
| Nén đầu ra tool với con trỏ dòng giữa các output | Giới hạn kích thước, cắt đuôi, diff của edit, chunk admission của jev (tắt mặc định); đọc lại một file không đổi vẫn gửi nguyên | **Gap.** Đã làm: §2 |
| GC context theo dự đoán dùng lại | `trim_transcript`, `HA_CONTEXT_DROP`, fit của runtime, đều tính tới cache | Một phần; không làm (§8) |
| Subtask stack machine | Model tóm tắt các bước của một turn; agent con trả báo cáo | Một phần; không làm (§8) |
| Tìm kiếm có metadata (chỉ mục SQLite file và symbol) | Không có chỉ mục nào agent truy vấn được | Không làm (§8) |
| Prompt gom lời gọi tool | "request them together in one step", các lời gọi chạy song song, codemode | **Tương đương** |

### P3 SkillOpt

| Bài báo | ha 0.9.11 | Gap, và đã làm gì |
|---|---|---|
| Sửa đổi là append / insert_after / replace / delete | `patch` là một lần thay chính xác; `update` viết lại toàn bộ | **Gap.** Đã làm: §4, action `edit` |
| Tối đa L sửa đổi mỗi bước (learning rate bằng chữ) | Không giới hạn | **Gap.** Đã làm: §4, tối đa 4 mỗi lần refine |
| Chỉ nhận khi tốt hơn hẳn trên tập held-out, nếu không thì rollback | Chỉ lint và an toàn; skill hỏng bị giữ lại khỏi recall; rollback bằng tay (`/refine --rollback`) | **Gap.** Đã làm: §4, chạy thử; ha không có tập held-out, các lần dùng được chấm tiếp theo thay cho nó |
| Sửa đổi bị từ chối được đưa lại | Bước repair thấy các lần từ chối của lần refine này; lịch sử ghi `failed` không có lý do | **Gap.** Đã làm: §4 |
| Báo cáo từng sửa đổi mỗi bước | `.ledger.jsonl` | Tương đương |
| Vùng cập nhật chậm được bảo vệ, meta skill của optimizer | Không có | Không làm (§8) |

### P4 Actor, Judge, or Teacher?

| Bài báo | ha 0.9.11 | Gap, và đã làm gì |
|---|---|---|
| Decision model trả lời câu hỏi có kiểu | Tích hợp jev: mười quyết định online, `noul` / `choice` / `score` | Tương đương (về giao diện) |
| Chấm từng hành động của một quỹ đạo đã xong, Hindsight + Progress | Không có gì chấm hành động đã qua: self-evaluation chấm câu trả lời cuối; kết quả của một check được tính cho mọi skill dùng trước nó | **Gap.** Đã làm: §6 |
| Teacher: điểm từng bước đưa vào advantage của RL | ha không huấn luyện model | Không làm; tương tự trong ha là cách §6 được review dùng |

## 2. Đọc lại thứ model đang có thì không gửi lại (P2)

Code: `crates/harness-tools/src/runtime/reread.rs`, được turn driver
(`crates/harness-tools/src/runtime/turn_driver.rs`) gọi ở chỗ kết quả một lời
gọi thành văn bản model đọc.

Chỉ văn bản model đọc thay đổi. Tool chạy như trước, người dùng thấy toàn bộ
kết quả, và output được lưu (executions, memory, receipt, codemode) vẫn đầy
đủ.

- **Đọc lại khi file không đổi.** Một `read_file` của file có sha256 trùng với
  một kết quả `read_file` trước đó trong turn này, cùng các dòng hoặc các dòng
  nằm trong đó, trả về:
  `notes/plan.txt: unchanged since your read_file call <id> (sha256:<12 hex>, lines A-B); that result above holds it. Pass "fresh": true to read it again.`
  `read_file` nhận `fresh: true` để vẫn đọc đầy đủ.
- **Đọc lại sau khi sửa.** Khi file đã đổi từ lần đọc đó và thay đổi nhỏ (tối
  đa một nửa số dòng, và tối đa 200), kết quả chỉ có các dòng đã đổi, đánh số
  như `read_file` đánh, với một dòng cho mỗi đoạn không đổi (`lines 12-80
  unchanged since call <id>`, kèm vị trí cũ khi chúng dịch chuyển) và hash mới.
  Thay đổi lớn hơn, hoặc các dòng có hashline anchor đã dịch chuyển, thì đọc
  đầy đủ.
- **Chỉ trỏ tới thứ model còn giữ.** Một kết quả được tính khi nó còn trong
  transcript của turn và chưa bị elide, rút gọn, drop (`HA_CONTEXT_DROP`), bị
  runtime fit hay gấp vào tóm tắt. Trước mỗi request, một con trỏ mà đích của
  nó đã mất sẽ được thay lại bằng văn bản đầy đủ. Kết quả của các turn trước
  không bao giờ được trỏ tới: runtime có thể tóm tắt hoặc rút gọn chúng khi
  dựng request kế tiếp, và driver không biết là cái nào. Còn hai khe hở, đều
  tự phục hồi được: runtime fit hoặc gấp ngay trong request nó gửi, nên một
  con trỏ tạo ra ở đúng bước đó được khôi phục từ bước sau; và một con trỏ
  còn trong cuộc hội thoại có thể, ở một turn sau, nằm sau một kết quả mà
  runtime đã rút gọn. Kết quả bị rút gọn tự nói nó đã bị rút gọn, và con trỏ
  bảo truyền `"fresh": true`.
- **Dòng lặp lại trong output lệnh.** Trong kết quả `run_shell`, `run_process`
  và `read_process_output`, một dãy từ 4 dòng giống nhau trở lên (bỏ qua
  khoảng trắng cuối dòng) thành dòng đầu và `[previous line repeated N more
  times]`. Output được ghi lại vẫn giữ mọi dòng.
- **Ghi lại.** Diagnostics `read.unchanged`, `read.delta`,
  `output.repeats_collapsed` và `read.restored`, mỗi cái kèm số byte;
  `ha exec --json` có `reread: {unchanged, delta, collapsed, saved_bytes}`, đã
  trừ các con trỏ phải gửi lại đầy đủ.
- **Công tắc.** `HA_REREAD=off` (hoặc `0`, `false`); bật theo mặc định.

## 3. Một lần chạy tốn bao nhiêu, theo từng phần (P2)

Code: `crates/harness-cli/src/host/presentation/cost.rs` (`split_cost`,
`CostParts`, `CostTracker::breakdown`) và
`crates/harness-cli/src/host/models/usage_tally.rs`.

- `calculate_cost` là tổng bốn phần của `split_cost`: input chưa cache (input
  trừ cache read và cache write), cache read, cache write và output, mỗi phần
  theo giá của nó; thiếu giá cache thì dùng giá input.
- `ha exec --json` cho `cost_usd: {uncached_input, cache_read, cache_write,
  output, total}` trong `usage`, `children_usage`, `upkeep_usage` và cho cả lần
  chạy. Mỗi lời gọi tính theo giá của model đã dùng (agent con trên model khác
  tính theo giá của model đó). Một tally có lời gọi không có giá là `null` thay
  vì một tổng thiếu.
- `/cost` giữ dòng đầu và thêm các phần cùng tỷ lệ và số token: `cache read
  $0.0123 (61%, 1.2M tok) · cache write ... · uncached input ... · output ...`.
- Mục *Model calls* của bug report cho cùng cách tách khi mọi lời gọi đều có
  giá.

## 4. Sửa một skill là vài sửa đổi, và phải chạy thử (P3)

Code: `crates/harness-cli/src/host/memory/learned.rs` (`edit`, `EditBudget`,
`begin_trial`, `settle_trial`, `roll_back_trial`, bộ đệm bị từ chối,
`revision_stats`) và `crates/harness-cli/src/host/memory/refine.rs` (ngân
sách dùng chung, bước repair, lịch sử).

- **Action `edit`.** Một lần sửa learned skill có sẵn là `{"action": "edit",
  "edits": [{"op": "append" | "insert_after" | "replace" | "delete", "target":
  ..., "content": ...}]}`, áp theo thứ tự lên `SKILL.md`. Target phải xuất hiện
  đúng một lần; target thiếu hoặc mơ hồ, op lạ hay thiếu content thì cả intent
  bị từ chối, có nêu `edits[n]`.
- **Ngân sách.** Tối đa 4 sửa đổi (`SKILL_EDIT_BUDGET`) cho một skill trong một
  lần refine, cộng qua các intent của nó và bước repair; một `patch` tính là
  một. Viết lại toàn bộ (`update`) một skill đã được chứng minh (có lần dùng
  thành công hoặc check pass) bị từ chối: phải là các sửa đổi. Skill chưa được
  chứng minh vẫn được viết lại.
- **Chạy thử.** Khi một bản sửa áp lên skill có sẵn, bản ghi usage mở một
  `trial`: id ledger, các bộ đếm của bản cũ (worked, failed, check pass và
  fail, trial good và bad) và bản undo của mọi file thay đổi đã ghi. Rồi mỗi
  lần dùng được chấm quyết định:
  - hai lần tốt, nhiều hơn lần xấu, thì nhận bản sửa (`skill
    revision_accepted`); hòa thì chạy thử tiếp;
  - hai lần xấu, nhiều hơn lần tốt, thì **tự rollback** khi bản cũ ổn (có lần
    dùng thành công hoặc check pass): file và bộ đếm của nó quay lại (`skill
    rollback`). Skill đã bị đổi từ sau bản sửa thì không bị ghi đè; bản ghi nói
    rõ điều đó (`rollback_skipped`);
  - bản cũ cũng đang hỏng thì không khôi phục; skill vẫn bị giữ lại khỏi
    recall như trước.
  Review chấm văn bản mà cuộc hội thoại đã làm theo, nên kết quả của nó được
  ghi trước khi các thay đổi của chính lần refine đó được áp: khi có một bản
  sửa đang chạy thử, lần dùng tính cho hoặc chống lại bản sửa đó (và một bản
  sửa thay thế bản đang hỏng thì giữ bản cũ ổn, bản hỏng được giữ dưới dạng
  `superseded`); khi không có, nó tính cho bản cũ.
- **Bản sửa bị từ chối được giữ và hiển thị.** Mỗi skill giữ năm bản sửa gần
  nhất bị từ chối (promoter từ chối ngay cả sau bước repair), bị rollback,
  hoặc bị thay thế khi đang bị chấm xấu (`superseded`), kèm lý do và tóm tắt
  ngắn các sửa đổi. Góc nhìn thư viện của refiner và curator hiển thị tối đa ba
  dưới skill (`rejected before: rolled_back 2026-11-18: ...; tried: ...`), và
  lịch sử ghi lý do một lần sửa skill thất bại.
- **`ha learning report`** đếm, trong 30 ngày, số bản sửa, số đang chạy thử,
  được nhận, bị rollback và bị từ chối (từ
  `<data>/skill-usage/revisions.jsonl`).

## 5. Một skill tốn bao nhiêu, và cắt gọn nó (P1)

Code: `learned.rs` (`TurnCost`, `Cost`, `record_turn`, `prune_candidates`,
luật chi phí của trial prune, `cost_stats`), `refine.rs` (`prune`,
`PRUNE_SYSTEM_PROMPT`), `service.rs` (chi phí của turn, bước tự động,
`prune_command`) và `commands/learning_cli.rs` (`ha learning prune`).

- **Một skill tốn bao nhiêu.** Sau mỗi turn trong app, mỗi learned skill đang
  dùng (được activate, xem, hoặc được recall đặt nguyên vào turn) cộng số bước
  model, số lời gọi tool và token đầu ra của turn vào bản ghi usage (`cost:
  {turns, steps, tool_calls, output_tokens, output_turns}`); một turn có lời
  gọi tool mà không có learned skill nào thì cộng vào `baseline` của layer.
  Token đầu ra là hiệu số của session trước và sau turn; turn không biết token
  thì chỉ đếm bước và lời gọi. Góc nhìn thư viện của refiner hiển thị `cost: N
  turns, mean S steps, C calls, O output tokens (turns without a skill: mean
  ...)`, và `ha learning report` liệt kê chi phí trung bình mỗi skill cạnh
  baseline. `ha exec` không ghi việc dùng skill hay chi phí: trước đây nó cũng
  chưa từng ghi lần dùng.
- **Skill nào được cắt.** Chỉ skill đã được chứng minh (bài học của P1: cắt
  một skill chưa chạy được thì kẹt): có ít nhất hai lần dùng thành công hoặc
  check pass, không đang chạy thử, không bị giữ lại, và dài (từ 20 dòng thân)
  hoặc tốn (nhiều bước mỗi turn hơn baseline, qua ít nhất hai turn đo được của
  mỗi bên). Tối đa ba skill mỗi lượt.
- **Cắt gọn.** Một lời gọi model với mục tiêu của Pha 2: giữ mọi luật và check
  làm skill chạy được; bỏ lặp lại, văn dài dòng và các bước mà bản ghi cho thấy
  không cần; ưu tiên ít turn hơn; chuyển một chuỗi lệnh skill lặp lại vào
  `scripts/<name>`; không bao giờ thêm giá trị riêng của một task. Nó trả lời
  bằng các intent `edit` theo ngân sách của §4. Promoter từ chối một bản cắt
  không phải `edit`, hoặc có `SKILL.md` không ngắn hơn cả về số dòng không trống
  lẫn số ký tự (có nêu cả hai kích thước).
- **Bản cắt chạy thử, có tính chi phí.** Một bản cắt được áp mở trial với mục
  tiêu `prune` và chi phí trung bình mỗi turn của bản cũ. Nó bị rollback khi,
  qua hai turn đo được, số bước trung bình (hoặc, khi biết cả hai, token đầu ra
  trung bình) vượt 1,10 lần bản cũ, ngay cả khi các lần dùng được chấm tốt (luật
  nhận của P1); nó được nhận khi hai lần dùng được chấm tốt và số bước trung
  bình không hơn bản cũ; ở giữa thì chờ. Trial repair (§4) không xét chi phí:
  Pha 1 của P1 không ràng buộc. Một bản sửa thay thế một bản đang hỏng vẫn giữ
  bản cũ ổn để quay về, và bản bị thay được giữ dưới dạng `superseded`.
- **Khi nào.** Sau curator, theo nhịp của nó (mỗi 250 request), khi có ứng
  viên; `HA_SKILL_PRUNE=off` tắt nó. `ha learning prune [--skill NAME] [--model
  PROVIDER/MODEL] [--thinking LEVEL]` chạy ngay (bất kể `HA_SKILL_PRUNE`; không
  chạy khi learned skill bị tắt). Diagnostics: `skill prune` với số ứng viên,
  số áp được và số bị từ chối.

## 6. Judge hindsight chấm từng bước (P4)

Code: `crates/harness-cli/src/host/memory/step_judge.rs` (cổng, prompt, kiểm
tra hình dạng, log, khối cho review, `skill_progress`, `stats`),
`turn_actions` trong `crates/harness-runtime/src/lib.rs` (các lời gọi, đối số
và kết quả của một turn, đọc từ journal), được nối trong
`application/service.rs` (app, sau turn, không chặn turn) và
`protocols/headless.rs` (`ha exec`, trước khi thoát).

- **Khi nào.** Sau một turn kết thúc bằng câu trả lời và có từ 6 lời gọi tool
  trở lên, trong app và trong `ha exec`; không cho agent con, không trong
  profile mock, và không khi review của memory bị tắt. `"stepJudge": "auto" |
  "off"` trong `settings.json`, hoặc `HA_STEP_JUDGE`, biến này thắng.
- **Câu hỏi và ngữ cảnh là bộ tốt nhất của P4.** Một lời gọi tới helper model
  mà self-evaluation dùng, với yêu cầu (4.000 ký tự đầu), mọi hành động theo
  thứ tự (đánh số từ 1: tool, đối số, đầu và cuối kết quả) và câu trả lời cuối
  (2.000 ký tự cuối), dưới dạng dữ liệu. Với mỗi hành động nó hỏi *hành động
  này có giúp đạt yêu cầu không?* (Progress), nhìn cả turn (Hindsight): một
  bước mà công dụng chỉ lộ ra về sau thì có giúp; một bước lặp lại không cần,
  bị hoàn tác, kết quả không bao giờ được dùng, hoặc không liên quan tới yêu
  cầu thì không giúp; turn có thành công không không phải là câu hỏi. Không
  nhãn kết quả nào tới judge: không self-evaluation, không verdict của goal,
  không tóm tắt check. Turn có hơn 80 hành động được chấm theo 80 hành động
  cuối, giữ nguyên số thứ tự.
- **Host chỉ kiểm tra hình dạng.** Mỗi hành động đúng một lần, `helped` là số
  từ 0 tới 1, `why` là chuỗi. Sai hình dạng thì phán quyết được ghi là không
  hợp lệ; không điền thêm gì.
- **Ghi lại.** Mỗi turn một dòng trong `<data>/harness/step-judgments.jsonl`
  (chuyển sang file cũ khi quá 5 MB): các bước với điểm và lý do, điểm trung
  bình, các bước dưới 0,3, và với mỗi learned skill được activate trong turn,
  điểm trung bình của các bước từ lúc activate tới lần activate kế tiếp hoặc
  hết turn. Diagnostics `step_judge evaluated` / `step_judge invalid`.
- **Được dùng.**
  - Lần review kế tiếp của cuộc hội thoại nhận một khối `<step_judgments>`:
    tối đa 12 bước bị chấm là không giúp kể từ lần review trước đã nhận
    chúng, dưới dạng dữ liệu; một mẫu lặp lại có thể thành lesson hay một sửa
    đổi skill, một lần đơn lẻ thì không. Khối chỉ được đánh dấu đã đưa sau khi
    lời gọi của review thành công.
  - Góc nhìn của refiner về một learned skill cho thấy các bước sau khi nó
    được activate bị chấm có ích tới đâu, trong 30 ngày qua (`steps after its
    activation judged helpful in hindsight: 72% over 5 turn(s)`): teacher của
    P4, cho một harness không huấn luyện model.
  - `ha learning report` cho số turn được chấm, điểm trung bình, tỷ lệ bị chấm
    không giúp và các tool hay bị chấm không giúp nhất.
  - `ha exec --json` có `step_judge: {judged, actions, mean, low}`, hoặc
    `{judged: false, reason}` (`off`, `reviews_off`, `mock`, `no_answer`,
    `too_few_actions`, `invalid`, `provider_failed`, ...). Trong `ha exec`
    lời gọi của judge được tính vào `upkeep_usage`, và process chờ nó (tối
    đa 3 phút) trước khi thoát, như chờ self-evaluation: một lần chạy theo
    script không muốn lời gọi thêm này thì đặt `HA_STEP_JUDGE=off`.

## 7. Công tắc

| Công tắc | Mặc định | Cái gì |
|---|---|---|
| `HA_REREAD=off` | bật | §2 |
| — | bật | §3 không có công tắc: nó chỉ báo cáo |
| — | bật | §4 là cách learned skill được sửa; tắt memory (`HA_MEMORY=off`) thì không sửa gì |
| `HA_SKILL_PRUNE=off` | bật | bước tự động của §5; `ha learning prune` vẫn chạy |
| `HA_STEP_JUDGE=off`, `"stepJudge": "off"` | bật | §6; tắt review của memory cũng tắt nó |

## 8. Không làm

- **GC context bằng model học được của P2** (LightGBM theo tuổi dòng,
  identifier và số lần tham chiếu): cần các trace có nhãn mà ha không lưu.
  Việc cắt của ha vẫn theo tuổi và kích thước, có tính tới cache.
- **Subtask stack machine của P2**: lần này maintainer chọn không làm; bài
  báo đo được 91% mỗi lần gấp, nhưng thêm 9% lời gọi để lấy lại văn bản đã bỏ,
  và nó chỉ kích hoạt khoảng một lần mỗi 20 turn.
- **Tìm kiếm có metadata của P2**: trên tác vụ code nó hầu như không được truy
  vấn, và Luna dùng sai các bảng SQLite.
- **Prompt gom lời gọi của P2**: ha đã có hướng dẫn đó, và câu chữ của bài báo
  làm Luna tốn thêm 2%.
- **Vùng cập nhật chậm được bảo vệ và meta skill của optimizer trong P3**:
  cả hai cần nhiều epoch rollout trên một tập task; ha không có tập task để
  chạy lại.
- **Tập validation held-out của P3**: ha không có; các lần dùng được chấm tiếp
  theo của skill đã sửa thay cho nó, và thay "tốt hơn hẳn trên tập đó" bằng
  "hai lần dùng được chấm tốt nhiều hơn xấu".
- **Teacher của P4**: ha không huấn luyện model.

## 9. Test

- Unit test: `reread.rs` và `turn_driver::reread_driver_tests`;
  `contracts::read_file_takes_a_fresh_boolean`; `cost.rs`,
  `usage_tally.rs`, `bug_report.rs`; các test về trial, ngân sách, bộ đệm, chi
  phí và prune trong `learned.rs` và `refine.rs`.
- `crates/harness-cli/tests/milestone_m4.rs`, một turn driver thật với
  provider có kịch bản: đọc lại thứ đang có là con trỏ, `fresh` và một lần drop
  thì gửi đầy đủ; đọc sau khi sửa là các dòng đã đổi và công tắc tắt được nó;
  dòng output lặp lại được gộp và bản ghi vẫn đầy đủ. `tests/g13_headless.rs`:
  `cost_usd` trong JSON.
- `step_judge.rs`: prompt chứa turn và không có nhãn kết quả, cổng, công tắc,
  câu trả lời sai hình dạng, log và việc xoay vòng, tiến độ từng skill, khối
  cho review kể từ lần review trước, thống kê, một turn được chấm từ journal.
- Live, với model thật: `scripts/testing/live-tests/reread.sh` (§2, §3),
  `scripts/testing/live-tests/skill-trial.sh` (§4, §5) và
  `scripts/testing/live-tests/step-judge.sh` (§6).
