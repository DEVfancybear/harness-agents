# Evidence HA_TUI — TUI terminal cho `ha`

Trạng thái: **CP-A (T01–T02) verified_local**. Theo template
[implementation-next/TEMPLATES.vi.md](../implementation-next/TEMPLATES.vi.md) mục 2.
Quyết định và hợp đồng nằm ở [SPEC HA_TUI](../specs/HA_TUI.vi.md); plan ở
[HA_TUI_PLAN.vi.md](../HA_TUI_PLAN.vi.md); handoff ở
[handoffs/HA_TUI.vi.md](../handoffs/HA_TUI.vi.md).

## 1. Work items covered

| Item | Trạng thái | Ghi chú |
|---|---|---|
| T01 — spike và quyết định | **implemented_verified** | Bốn câu hỏi đo có số; POC đã xoá |
| T02 — view model và refactor controller | **implemented_verified** | Plain transcript byte-identical; event mới có producer thật |
| T03–T08 | chưa bắt đầu | CP-A chỉ gồm T01–T02 |

## 2. Tested source

- Base commit: `c97c7dd` (`docs(ha-tui): plan the TUI track T01-T08 and the DeepSeek prompts`), nhánh `master`.
- Working tree **không sạch** khi bắt đầu: `crates/harness-cli/tests/phase_p2.rs` và
  `scripts/Verify-Phase.ps1` sửa dở từ việc đo flake loopback của track H. Hai thay
  đổi này **được giữ nguyên** và đi cùng commit T01 (không thuộc track T).
- Digest của cây nguồn: `git status --porcelain` + `git diff --stat` ghi ở mục 7.
- Lockfile: `Cargo.lock` cập nhật trong cùng commit; diff chỉ **thêm** package, không
  package nào đang dùng bị đổi version.

## 3. Môi trường

| Mục | Giá trị |
|---|---|
| OS | Windows 11 (`Windows_NT`), x64, console thật là Windows Terminal (`WT_SESSION` set) |
| Toolchain | `rustc 1.97.1`, `cargo 1.97.1`, `pwsh 7` |
| crossterm | `0.29.0` — **một bản duy nhất** trong cây dependency |
| ratatui | `0.30.2` (MIT), `ratatui-core 0.1.2`, `ratatui-crossterm 0.1.2`, `ratatui-widgets 0.3.2` |
| unicode-width | `0.2.2` — một bản, dùng chung với `ratatui-core` |
| Fixture version | `FixtureService` trong `crates/harness-cli/src/interactive/service.rs` |

## 4. Commands thực sự đã chạy (CP-A)

```text
cargo tree -p harness-cli -i crossterm           -> một bản 0.29.0
cargo tree -p harness-cli -i unicode-width       -> một bản 0.2.2
cargo build -p harness-cli --locked              -> ok
cargo test -p harness-cli --bin ha --locked      -> 124 passed; 0 failed
cargo test -p harness-cli --test interactive_session --locked -- --test-threads=1
                                                 -> 9 passed; 0 failed
cargo clippy --workspace --all-targets --locked -- -D warnings   -> ok
cargo fmt --all -- --check                       -> ok
pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1 -TimeoutSeconds 900
                                                 -> PTY_EXIT: 0, "10 passed; 0 failed" (24.07 s)
pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json
                                                 -> passed: true, failures: [] (sau khi có docs HA_TUI)
```

Số test đơn vị của binary `ha`: **70 trước T01** → **124 sau T02**. Không test nào bị
xoá; các test cũ đổi assertion sang `plain_lines`/`effects_to_plain` theo plan T02.

## 5. Acceptance đã có test

| ID | Test | Kết quả |
|---|---|---|
| U20 (plain byte-identical) | `t02_plain_lines_are_byte_identical_to_the_pre_t02_writer` | pass — so từng chuỗi với writer trước T02 |
| U20 (transcript khớp renderer) | `t02_the_recorded_transcript_is_what_the_plain_renderer_printed` | pass |
| U07 (hết hạn đóng panel, không grant) | `t02_expired_approval_closes_the_modal_and_never_grants` | pass |
| U06 (đếm step) | `t02_step_started_updates_the_counter` | pass |
| U05 (thời lượng tool) | `t02_a_settled_tool_card_reports_the_measured_duration` | pass |
| T01-a (chiều cao viewport) | `t01_*` trong spike (đã xoá) + `t02_viewport_height_is_bounded_for_any_console` | pass |
| T01-b (TestBackend inline) | `t01_test_backend_supports_an_inline_viewport_and_insert_before` (spike, đã xoá) | pass lúc đo |
| T01-c (Alt+Enter/Ctrl-J/paste) | ca PTY `t01_probe_key_delivery_measures_alt_enter_ctrl_j_and_paste` (spike) | pass lúc đo |
| T01-e (thứ tự insert_before) | `t01_insert_before_emits_the_rows_in_order_and_clears_the_viewport` (spike) | pass lúc đo |

## 6. Negative controls

| Bất biến | Cách phá | Kết quả mong đợi | Đã kiểm |
|---|---|---|---|
| Approval không bao giờ được cấp khi hết hạn | bỏ `SessionEvent::ApprovalExpired` khỏi gate | `t02_expired_approval_closes_the_modal_and_never_grants` đỏ | có (test đọc modal + log answer) |
| Plain mode không được lệch | đổi một nhãn trong `plain_lines` | `t02_plain_lines_are_byte_identical_to_the_pre_t02_writer` đỏ | có (so chuỗi nguyên văn) |
| `TOOL` card phải có thời lượng thật | trả `Duration::ZERO` từ service | test `t02_a_settled_tool_card...` đỏ | có |
| Một crossterm duy nhất | bỏ feature `crossterm_0_29` | `cargo tree -i crossterm` ra hai bản | có (đo trước/sau) |
| Gate không tự giảm coverage | xoá một selector bắt buộc | `Verify-HaLaunch.ps1 -SelfTest` đỏ | có (self test có sẵn) |

## 7. Artifacts và bằng chứng thô

| Đường dẫn | Nội dung | Ghi chú |
|---|---|---|
| `target/verification/t01-pty-insert-before.txt` | transcript thô `insert_before` (conhost) | dùng `scripts/Read-HaTranscript.mjs` để đọc |
| `target/verification/t01-pty-interactive.txt` | transcript thô spike tương tác | nt |
| `target/verification/t01-pty-keys.txt` | bàn phím: Enter / Alt+Enter / Ctrl-J / paste | nt |
| `target/verification/t01-windows-terminal.png` | chụp Windows Terminal | ảnh, không commit |
| `target/pty-acceptance/pty-all.txt` | 10 ca PTY cũ | nt |

`target/` nằm trong ignore rule, nên bằng chứng ở đây là **số đo tái lập được**, không
phải file được commit:

```text
git status --porcelain            # cây nguồn của lượt đo
pwsh -NoProfile -File scripts/Read-HaTranscript.mjs <transcript>   # đọc transcript
```

## 8. Remaining limitations / not_run

- **Linux**: chưa build/chạy; phiên này chỉ có Windows x64.
- **Paid smoke**: **được cấp quyền** trong assignment nhưng CP-A không cần: T01/T02
  chưa chạm provider thật. Sẽ chạy ở CP-C/CP-D và ghi lại ở đây.
- **Cài thật lên máy user**: **được cấp quyền**; chưa chạy ở CP-A (thuộc U17/T08).
- **Windows Terminal / conhost cũ**: đo trên Windows Terminal (ảnh) và ConPTY của repo;
  conhost "cũ" (không phải WT) chưa đo riêng.
- **Shift+Enter**: không phân biệt được trên Windows — vẫn không hứa, không ghi vào help.
- **`scrolling-regions`**: chạy được trên ConPTY nhưng **bị loại** (xem SPEC T01-d).
- **POC T01**: đã xoá cùng cờ `--tui-spike`; test `t01_*` của spike không còn trong cây.

## 9. Gate

- `pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json` → `passed: true`, `failures: []`.
- `pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1 -TimeoutSeconds 900` →
  `PTY_EXIT: 0`, "10 passed; 0 failed".

Gate chưa có selector cho track T (đó là T08). Ở CP-A gate vẫn là gate H, đúng như
plan mục 8 quy định.
