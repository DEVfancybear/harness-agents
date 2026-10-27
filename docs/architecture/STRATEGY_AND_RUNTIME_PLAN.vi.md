# Plan: đổi chiến lược và runtime harness cho ha

Nguồn:
- **[A] arXiv 2608.19072**, *What Is Missing from AI Post-Training AI*: agent làm tốt ở mức thực thi nhưng hiếm khi đổi chiến lược.
- **[B] arXiv 2605.22166**, *LIFE-HARNESS*: tiến hoá runtime harness từ trajectory, giữ nguyên model.

Trạng thái: plan, chưa code. Tham chiếu code theo commit `9feb732`.

## 1. Hai bài nói gì, và cái gì dùng được cho ha

### [A] Agent khoá cứng vào chiến lược ban đầu
**Phát hiện:**
- Phân tích 1.338 trajectory: agent thực thi tốt (sửa lỗi, chỉnh tham số), nhưng khoá vào một chiến lược mặc định từ trước khi có bằng chứng.
- Chỉ **2,1%** số cặp thí nghiệm liền nhau có đổi chiến lược. Chiến lược mặc định đi theo agent chứ không theo bài toán.

**Thử ba giả thuyết:**
1. **Thiếu kinh nghiệm?** Thêm nhật ký thí nghiệm có kiểu, thư viện skill và một evaluator agent: thực thi tốt hơn hẳn (đóng được 75,1% khoảng cách so với 48,6%). Nhưng agent làm theo 22/22 gợi ý mức thực thi và **0/21** gợi ý mức chiến lược.
2. **Thiếu suy luận?** Thêm token chỉ giúp mài chiến lược đang theo, không đổi nó.
3. **Thiếu quyết định?** Kết quả chia ba:
   - Người duyệt *trước* khi chạy thì đổi được việc agent khoá vào *chiến lược nào*, chứ không đổi được việc *có khoá hay không*.
   - **Một lời bảo đổi giữa chừng, có nêu tên chiến lược thay thế,** thắng phần agent tự chạy tiếp tới +17,44 điểm, với cùng ngân sách.
   - **Chỉ bảo "cân nhắc lại" mà không nêu phương án:** agent giữ nguyên chiến lược cũ.

**Đề xuất của bài:** biến "tiếp tục hay đổi" thành một **nút quyết định tường minh sau mỗi lần đánh giá, trước mọi hành động tiếp theo**. Đo switch rate và tỉ lệ làm theo gợi ý mức chiến lược. Fork hai nhánh "tiếp tục" và "đổi" từ cùng một checkpoint để so sánh.

**Không dùng được cho ha:** tín hiệu huấn luyện từ các run đã fork (ha không train model). Phần còn lại đều là giao thức tương tác, ha làm được.

### [B] LIFE-HARNESS: sửa giao diện, không sửa model
Bốn lớp, mỗi lớp đặt ở một thời điểm trong vòng tương tác:
1. **Environment Contract:** làm rõ luật dùng tool ngay trong mô tả tool.
2. **Procedural Skill:** lấy **top-1** skill theo BM25 cho từng task, chèn vào system prompt. Chỉ lấy một skill để skill không liên quan không làm nhiễu context.
3. **Action Realization:** trước khi chạy, kiểm, chuẩn hoá, hoặc chặn những lệnh chắc chắn sẽ hỏng.
4. **Trajectory Regulation:** sau khi chạy, phát hiện lặp, giậm chân tại chỗ, thử lại vô ích, sắp hết ngân sách, và nối hướng dẫn cụ thể vào kết quả tool.

**Tiến hoá:**
- Một LLM đọc các trajectory hỏng, phân loại lỗi theo thứ tự ưu tiên (realization → contract → degeneration → reasoning), rồi đề xuất sửa trong đúng bốn lớp.
- Mỗi ứng viên phải qua validate và replay trace. Sau đó được giữ lại **chỉ khi điểm tăng trên cùng batch**, rồi đóng băng.

**Kết quả:**
- Hơn Meta-Harness 28,7 điểm và hơn AHE 7,5 điểm, với ít token tiến hoá hơn.
- Vẫn có lợi cho model frontier (+2,8 đến +4,4 điểm).
- Bỏ đi bất kỳ lớp nào cũng tụt điểm: bỏ Trajectory làm ALFWorld giảm 86,5%, bỏ Action làm Airline/OS giảm khoảng 60%.

**Khác biệt với ha:**
- Bài có benchmark và scorer cố định; ha không có, nên tiêu chí chấp nhận phải đổi thành replay trên chính các record của ha, cộng chế độ chạy bóng (shadow, xem B4).
- Harness của bài viết riêng cho từng môi trường. Môi trường của ha là "workspace + bộ tool", nên thứ học được là ghi chú và luật **theo tool và theo project**.

## 2. ha hiện có gì (đã khảo sát code)

| Lớp / ý | Đã có | Còn thiếu |
|---|---|---|
| Regulation | Loop check: `repeated_tail` / `repeated_read` (`turn_driver.rs:1754`), cảnh báo một câu cố định rồi dừng `LoopDetected` | Lặp lại cùng một lỗi với tham số khác nhau, dao động A/B, chuỗi kết quả rỗng; cảnh báo không nêu tên lệnh nào; không có diag record |
| Realization | Choke point `run_turn_inner` → `gate_action` (`turn_driver.rs:1847`, `2713`): validate + coerce schema, `misplaced_arguments`, policy, guard Jev, hooks, approval, `validate_dispatch_preconditions` | Không kiểm lệnh dựa trên lịch sử gần đây, ví dụ lặp lại y nguyên một lệnh vừa hỏng khi workspace không đổi |
| Contract | Mô tả tool là hằng số (`contracts.rs:332`) | Không có gì học được được nối vào mô tả tool |
| Skill | Catalog `<available_skills>` (`prompt.rs:306`); `list_skills` khớp từ; skill-usage có lần thất bại kèm `assumption` | Không lấy skill theo từng request; assumption thất bại không hiện ra khi skill được dùng |
| Tiến hoá | Review/refine (`refine.rs:2608`) ghi memory, skill, page, nhưng chỉ có lint và kiểm cấu trúc | Không có tiêu chí chấp nhận theo hành vi (replay hay shadow) |
| Chiến lược | `task_state` có `decisions{decision, why}` (`task_state.rs:85`); `/goal` có `refuse_unchanged`, `[verify]`, verifier | Không có trường strategy hay nhật ký các lần thử; continuation của `/goal` giữ nguyên câu chữ qua các lượt; không phát hiện plateau ở /goal tương tác |
| Fork | Hai coder chạy song song ở hai worktree; `integrate_children` chạy `[verify]` trên bản đã merge | Không có công cụ so sánh các ứng viên; verifier không trỏ được vào worktree của coder |
| Diag | Có `tool failed` kèm thông báo lỗi; bug report gom lỗi lặp ≥3 lần | Record lỗi tool **không có hash tham số**; cảnh báo vòng lặp không được ghi |

## 3. Thiết kế

Nguyên tắc chung:
- Không danh sách từ cứng: phân loại và đánh giá dựa trên thống kê hoặc cấu trúc, chỗ nào mơ hồ thì để model quyết.
- Mọi thứ học được đều đi qua refinement, nên đều rollback được.
- Mỗi cơ chế đều có diag record.

### A. Lớp chiến lược (từ [A])

**A1. Strategy và nhật ký trong `task_state`**
- `TaskState` thêm hai trường:
  - `strategy {name, why, alternatives: [{name, why_not}], since_revision}`;
  - `journal: [{kind: plan|observation|lesson|eval|eval_analysis, text, at, strategy}]`, với kiểu mục lấy từ nhật ký thí nghiệm của [A], chỉ ghi thêm, có giới hạn (giữ N mục cuối và tóm tắt phần cũ).
- `context_block` hiện strategy cùng vài mục `eval` và `lesson` gần nhất. Continuation của `/goal` không còn giữ nguyên câu chữ: nó nêu strategy hiện tại và kết quả đánh giá cuối.

**A2. Nút quyết định "tiếp tục hay đổi"**
- **Lần đánh giá** là một trong các sự kiện:
  - `goal_complete` bị từ chối (checklist chưa xong, unchanged, `[verify]` trượt, verifier nói chưa xong);
  - `[verify]` chạy giữa task;
  - một mục `eval` mà model tự ghi.
- **Sau mỗi lần đánh giá** (đã chốt: khuyên, không chặn; xem mục 5):
  - Lượt tiếp theo mang một block quyết định: điểm của lần đánh giá, xu hướng qua các lần trước, plateau hay chưa, và các phương án có tên khi có plateau.
  - Block yêu cầu agent ghi `decision {kind: continue|switch, strategy, evidence}` vào `task_state`, nhưng gate không chặn tool nào.
  - Block nằm ở **đầu** lượt chứ không chìm trong context dài, vì [A] cho thấy quyết định chìm trong context thì không bao giờ được đưa ra.
  - Lượt chưa ghi quyết định thì đếm là "không quyết định"; các lần đánh giá sau vẫn tiếp tục nhắc.

**A3. Phát hiện plateau và nêu tên phương án thay thế**
- **Điểm số của mỗi lần đánh giá lấy từ cấu trúc, không đọc chữ:**
  - số check `[verify]` trượt;
  - số mục checklist chưa xong;
  - phán quyết của verifier;
  - `progress_signature` của workspace (đã có ở `harness-runtime/control/goal.rs:216`).
- **Plateau:** `k` lần đánh giá liên tiếp dưới cùng một strategy mà điểm không tốt hơn (`k` mặc định 3, có cài đặt).
- **Khi có plateau:**
  1. Chạy một **strategist** là child explorer, context mới, không bị neo vào cách làm hiện tại. Đầu vào là objective, strategy, journal và các lần đánh giá. Đầu ra là 2–3 **chiến lược thay thế có tên**, mỗi cái kèm lý do và bằng chứng từ journal. Lý do: [A] cho thấy chỉ bảo "cân nhắc lại" là vô ích, phải nêu được phương án.
  2. Block quyết định của A2 kèm theo các phương án đó. Model phải chọn một: tiếp tục, có lý do gắn với bằng chứng; hoặc đổi sang một phương án.
  3. **Người ở giữa chừng**, cách [A] khuyên dùng ngay: TUI hiện thông báo "Chiến lược X không tiến triển sau k lần đánh giá; phương án: …". Thêm lệnh `/strategy`: xem strategy, journal và các phương án, và cho người dùng chỉ định đổi. Lựa chọn của người dùng đi vào đúng nút quyết định đó.

**A4. Fork để so sánh (tuỳ chọn, giai đoạn sau)**
- Khi chọn `switch`, có thể chạy chiến lược mới trong một coder worktree **từ cùng snapshot**, song song với nhánh hiện tại.
- Tool `compare_children` chạy `[verify]` trên từng worktree ứng viên, cho ra điểm và diff stat đặt cạnh nhau, rồi người hoặc model chọn bản để integrate.
- Cần sửa: verifier phải trỏ được vào worktree của coder.

**A5. Đo lường**
- Diag: `strategy decided` (continue/switch, có plateau hay không, nguồn: model, strategist hay người dùng) và `strategy evaluated` (điểm).
- Bug report có thêm mục **Strategy**: switch rate, tỉ lệ làm theo phương án strategist đưa ra, số lần plateau. Đây là các chỉ số mức chiến lược [A] đề xuất.

**A6. Bài học chiến lược vào memory**
- Review ghi lại "trên project này, chiến lược X giậm chân, Y thành công", kèm trích dẫn bằng chứng.
- Digest đưa bài học đó vào khi task mới chọn strategy, tức là khi A1 ghi `plan`.

### B. Runtime harness bốn lớp (từ [B])

**B0. Quan sát trước (điều kiện cho mọi phần khác)**
- Record `tool ok|failed` thêm:
  - `args_hash`: hash của tham số đã chuẩn hoá;
  - `args_shape`: tên tham số và kiểu, không chứa giá trị, để không lộ dữ liệu;
  - `error_class`: `ErrorCode` có sẵn cộng hash của thông báo lỗi sau khi bỏ số và đường dẫn;
  - `workspace_rev`: snapshot nhẹ.
- Ghi record cho mỗi lần cảnh báo vòng lặp và mỗi lần regulation can thiệp.

**B1. Trajectory Regulation (làm trước, vì [B] cho thấy đây là lớp có tác động lớn nhất)**
- Tổng quát hoá loop check thành một monitor sau mỗi bước. Các dấu hiệu đều đo bằng cấu trúc:
  1. cùng `error_class` với cùng tool lặp lại ≥ n lần, dù tham số khác nhau;
  2. dao động A/B giữa các lệnh không chỉ đọc;
  3. một chuỗi kết quả rỗng (search hay find không ra gì) mà query hầu như không đổi;
  4. sắp chạm giới hạn step hoặc tool.
- Can thiệp theo mức tăng dần: nối ghi chú vào kết quả tool, nêu **đúng lệnh và lỗi lặp lại** cùng điều đã biết, ví dụ "đường dẫn này không có; workspace có: …". Rồi mới tới cảnh báo, rồi dừng như hiện nay.
- Dấu hiệu 1 và 2 cũng tính là một **lần đánh giá âm** cho A2.

**B2. Action Realization dựa trên lịch sử**
- **Chặn trước khi chạy:** một lệnh có `args_hash` trùng một lệnh vừa hỏng, mà `workspace_rev` không đổi, thì chắc chắn sẽ hỏng lại. Thông báo chặn nêu lỗi lần trước. Lệnh chỉ đọc và lệnh có tính thời gian (process, web) không bị chặn.
- **Luật học được:** dùng một **DSL nhỏ, an toàn và có giới hạn**, không cho model viết code harness tuỳ ý:
  - `rename_argument{tool, from, to}`;
  - `block_repeat{tool, error_class}`;
  - `require_read_before{tool}`;
  - `normalize_path{tool, arg}`.
- Mỗi luật là một entry trong store có refinement, nên rollback được.

**B3. Environment Contract: ghi chú học được cho từng tool**
- Mỗi tool có thể mang các `contract notes` ngắn (theo project hoặc global), được nối vào description khi build schema (`contracts.rs:332` / `service.rs:8896`).
- Giới hạn độ dài, và giữ thứ tự cố định để không phá prompt cache.
- Ví dụ học từ chính record của ha: "`edit_file`: `old_string` phải duy nhất; khi có nhiều chỗ giống nhau, dùng `edits` mỗi chỗ một phần tử".

**B4. Procedural Skill: lấy top-1 cho từng request**
- Xếp hạng learned skill theo BM25 với request hiện tại, dùng chung `harness_types::search` như đã làm cho memory và history.
- Chèn **body của đúng một skill** (top-1, như [B]), chỉ khi điểm vượt ngưỡng tính theo thống kê điểm của cả catalog (giống `STANDOUT` của readout).
- Kèm các **assumption của những lần thất bại trước** của skill đó, lấy từ skill-usage.

**B5. Tiến hoá harness và tiêu chí chấp nhận**
- **Chạy khi nào:** một job offline giống review và Deep Dream, theo cadence, hoặc bằng lệnh `/harness evolve`.
- **Đầu vào:** các record `tool failed` của B0, gom theo `(tool, error_class)` và đặt kèm các lần thành công của cùng tool.
- **Phân loại:** theo đúng thứ tự ưu tiên của [B] (realization → contract → degeneration → reasoning), do LLM làm với luật cố định trong prompt. Nhóm "reasoning" không đề xuất gì.
- **Đề xuất:** chỉ được nằm trong B2 (DSL), B3 (ghi chú) hoặc B4 (skill).
- **Chấp nhận:** thay cho "điểm phải tăng trên cùng batch" của [B]:
  1. **Replay trên record:** luật phải chặn hoặc sửa được ≥ m lần hỏng đã ghi, và **0** lần thành công đã ghi.
  2. **Chạy bóng (shadow):** luật chạy ở chế độ chỉ quan sát trong N lượt và ghi lại "lẽ ra đã can thiệp". Chỉ được bật thật nếu precision giữ nguyên.
  3. Ghi chú (B3) và skill (B4) đi qua review như hiện nay. Mỗi thay đổi được báo cho lượt sau, kèm id để hoàn tác.

## 4. Giai đoạn đề xuất

| GĐ | Nội dung | Phụ thuộc | Live test |
|---|---|---|---|
| 1 | B0 quan sát + B1 regulation + B2 chặn lặp lại lỗi | — | suite mới `runtime.sh`: lặp lại một lỗi với tham số khác nhau, dao động A/B, lặp lại một lệnh vừa hỏng; đối chứng 0.8.3 |
| 2 | A1 strategy và journal + A2 nút quyết định + A5 đo lường | GĐ1 (đánh giá âm) | suite mới `strategy.sh`: fixture mà cách A không bao giờ qua `[verify]` (giậm chân), cách B qua; kiểm block quyết định, cổng chặn, decision được ghi |
| 3 | A3 plateau + strategist + `/strategy` + thông báo TUI | GĐ2 | `strategy.sh`: sau k lần đánh giá, strategist nêu phương án, agent đổi và qua; người dùng chỉ định đổi bằng `/strategy` |
| 4 | B4 lấy top-1 skill + assumption thất bại | — | `memory.sh`: skill liên quan được chèn, skill không liên quan thì không |
| 5 | B3 ghi chú contract + DSL của B2 + B5 tiến hoá (replay và shadow) | GĐ1 | `runtime.sh`: lỗi được gieo sẵn sinh ra luật; luật qua replay, chạy bóng, rồi được bật; rollback |
| 6 | A4 fork, `compare_children`, verifier trên worktree; A6 bài học vào memory | GĐ2–3 | `children.sh` / `strategy.sh` |

Mỗi giai đoạn: unit test, gate, live trên gpt-6-luna ở mức max, đối chứng trên 0.8.3, và tài liệu kiến trúc EN/VI.

## 5. Quyết định của chủ dự án (2026-10-27)

1. **Phiên bản:** cả sáu giai đoạn vào **một** bản phát hành.
2. **Nút quyết định:** **khuyên, không chặn**.
   - Sau mỗi lần đánh giá, lượt tiếp theo mang đánh giá của harness: điểm, xu hướng, plateau hay chưa, và các phương án có tên.
   - Agent tự đánh giá và quyết định, rồi ghi quyết định vào `task_state`. Gate không chặn tool nào.
   - Thay đổi so với A2: bỏ `decisionGate = block`. Việc nêu tên phương án (A3) giữ nguyên, vì [A] cho thấy chỉ có lời khuyên nêu được phương án mới có tác dụng.
3. **Tiến hoá harness (B5):** chạy **tự động** theo cadence, như review. Vẫn phải qua replay và chạy bóng trước khi bật thật.
4. **Thông báo plateau trên TUI và `/strategy`:** **bật mặc định, hoàn toàn tự động**.
   - Người dùng không phải bấm hay chọn gì: không có nút, không có câu hỏi.
   - Khi có plateau, strategist tự đưa ra các phương án có tên, agent tự quyết và tự đổi.
   - Thông báo trên TUI chỉ để người dùng biết chuyện gì đã xảy ra. `/strategy` chỉ để xem strategy, journal và các quyết định, không bắt buộc dùng.
   - Thay đổi so với A3 mục 3: bỏ việc người dùng chỉ định đổi chiến lược.
