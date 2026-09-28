# Evidence HA_PRIME — CP-1 (Q00–Q04)

[SPEC](../specs/HA_PRIME.vi.md) · [Handoff](../handoffs/HA_PRIME.vi.md) · [Plan](../HA_PRIME_PLAN.vi.md)

- **Work items:** Q00–Q04.
- **Trạng thái:** `implemented_partially_verified` — code xong, test mục tiêu xanh; **toàn bộ test workspace và 25 ca PTY cũ chưa chạy lại trên bản cuối** (phiên bị ngắt giữa lần chạy; người dùng chọn commit trước và chạy toàn bộ một lượt sau khi xong các checkpoint).
- **Base:** `48676b6`; Windows 11, rust-toolchain của repo; ngày 28/09/2026.

## Đã chạy

| Lệnh | Kết quả |
|---|---|
| `cargo test -p harness-cli --bin ha delegation` | 20 passed, 0 failed |
| `cargo test -p harness-cli --bin ha q0` | 16 passed, 0 failed (controller + delegation + store lease) |
| `cargo test -p harness-cli --bin ha` (trước bản sửa lease và clippy) | 443 passed, 2 failed — đúng hai test chập chờn đã biết (`a_dead_owners_journal_is_reaped_and_removed`, `g09_hook_cannot_turn_ask_into_allow`) |
| `scripts/Invoke-HaPtyAcceptance.ps1 -Filter q0` (lần 1) | 3 passed, **1 failed**: `q03_pty_child_reports_to_its_parent` — lượt được đánh thức lỗi `task_lease_conflict` (negative control tự nhiên cho bản sửa lease) |
| `scripts/Invoke-HaPtyAcceptance.ps1 -Filter q0` (sau `release_task_lease`) | 4 passed, `PTY_EXIT: 0` |
| `cargo clippy --workspace --all-targets -- -D warnings` | sạch |
| `cargo run -p harness-types --bin generate_schemas --locked` | chỉ `harness-config.v2.schema.json` đổi (thêm `agents`) |

## Chưa chạy (not_run)

- `cargo test --workspace --locked --no-fail-fast` trên bản cuối (có `p0_f03` kiểm schema).
- PTY đầy đủ 25 ca cũ + 4 ca q0 trong một lần (`-TimeoutSeconds 900`).
- Live smoke với provider thật.


# Evidence CP-2 (Q05–Q09)

- **Trạng thái:** `implemented_partially_verified` — như CP-1, toàn bộ test workspace và 25 ca PTY cũ để chạy một lượt khi xong các checkpoint (theo người dùng).
- **Base:** `93bc8d4`; Windows 11; 28/09/2026.

| Lệnh | Kết quả |
|---|---|
| `cargo test -p harness-cli --bin ha -- q05 q06 g06` | 18 passed |
| `cargo test -p harness-cli --bin ha -- q07` / `q08` / `q09` | 2 / 2 / 2 passed |
| `cargo test -p harness-cli --bin ha` | 456 passed, 2 failed → sau khi thêm lệnh vào thẻ `/help` chỉ còn `g09_hook_cannot_turn_ask_into_allow` (chập chờn đã biết) |
| `Invoke-HaPtyAcceptance.ps1 -Filter q0` | lần 1: q07, q08 đỏ vì kịch bản test (chữ có dấu cách bị vẽ bằng lệnh dời con trỏ; kịch bản fork không phân biệt được nhánh) → sửa kịch bản; q00–q07 xanh; `q08_pty_fork_then_answer` xanh riêng (`PTY_EXIT: 0`): nhánh thấy lượt 1, không thấy lượt 3 |
| `cargo clippy --workspace --all-targets -- -D warnings` | sạch |
| `generate_schemas` | `harness-config.v2.schema.json` thêm `queue` |

**not_run:** toàn bộ workspace, 25 ca PTY cũ, live smoke.


# Evidence CP-3 (Q10–Q13)

- **Trạng thái:** `implemented_partially_verified` — như CP-1/CP-2, toàn bộ workspace và 25 ca PTY cũ để chạy một lượt khi xong các checkpoint (theo người dùng).
- **Base:** `5d7efd9`; Windows 11; 28/09/2026.

| Lệnh | Kết quả |
|---|---|
| `cargo test -p harness-cli --bin ha -- q10 comments_inside` | 6 passed |
| `cargo test -p harness-providers q13` | 2 passed (`429 → rate_limited`, mã quota có cấu trúc; chữ tự do không được đọc) |
| `cargo test -p harness-cli --bin ha -- q11 q12 q13` | 10 passed; thêm 2 test `AuxiliarySummary` → 2 passed |
| `cargo test -p harness-cli --bin ha` | lần 1: 471 passed, 5 failed — 3 test menu slash (do `/scoped-models` chèn ngay sau `/model` làm lệch thứ tự menu → dời xuống cạnh `/stash`) + 2 test g09 chập chờn đã biết (`g09_hook_receives_bounded_json_without_secrets` chạy riêng xanh); lần 2: **476 passed, 0 failed** |
| `cargo test -p harness-types -p harness-providers`, `--test phase_p0` | xanh |
| `Invoke-HaPtyAcceptance.ps1 -Filter q10` | 1 passed (`PTY_EXIT: 0`): `/model local/tiny-q10` từ `models.json`, server nhận đúng model id |
| `Invoke-HaPtyAcceptance.ps1 -Filter q1` | lần 1: q11 đỏ vì kịch bản (lần chờ cuối khớp chữ cũ của lượt 1 trên màn hình) → chờ theo số request; lần cuối **3 passed** (`PTY_EXIT: 0`): Alt+M (Esc m) tới được app qua ConPTY và đổi model; 429 + `Retry-After: 2` hai lần → dòng `(1/30)` rồi câu trả lời |
| `cargo clippy --workspace --all-targets -- -D warnings` | sạch |
| `generate_schemas` | `harness-config.v2.schema.json` thêm `routing`; `error-report.v1.schema.json` thêm `rate_limited` |

**not_run:** toàn bộ workspace, 25 ca PTY cũ, live smoke với provider thật (429 thật, model phụ thật).


# Evidence CP-4 (Q14–Q15) và lượt chạy toàn bộ của track

- **Trạng thái:** `implemented_verified_with_known_failure` — toàn workspace và PTY đầy đủ đã chạy; còn `i12` đỏ **từ trước track** (xem dưới).
- **Base:** `3f423c1`; Windows 11, PowerShell 7.6.6; 28/09/2026.

| Lệnh | Kết quả |
|---|---|
| `cargo test -p harness-cli --bin ha -- q14 q15` | 9 unit (có `q14_unchanged_worktree_skips_the_gate` chạy git + pwsh thật) + 3 controller xanh; lần đầu `/autonomous`, `/schedule` và **`/scoped-models` (CP-3)** chỉ nhận từ đầu tiên của tham số → `q11_scoped_models_takes_every_pattern` đỏ (port nhận `deepseek/*`), sửa dùng `raw_argument`, xanh; `q14_autonomous_without_gates_stops_at_its_limit` đỏ vì kỳ vọng sai (prime giữ giới hạn cũ khi bật lại) → sửa test |
| `Invoke-HaPtyAcceptance.ps1 -Filter q1` | 5 passed (q10, q11, q13, q14, q15), `PTY_EXIT: 0` |
| `cargo test --workspace --locked --no-fail-fast` | **958 passed, 2 failed, 39 ignored**; 2 đỏ là hai test chập chờn đã biết (`g09_hook_cannot_turn_ask_into_allow`, `a_dead_owners_journal_is_reaped_and_removed`) — chạy riêng: 2 passed; `p0_f03_contract_schemas_are_generated_from_real_types` xanh; không crate nào lỗi biên dịch |
| PTY đầy đủ lần 1 (`-TimeoutSeconds 1500`) | 35 passed, 2 failed: `q03_pty_child_reports_to_its_parent` (lỗi thật: tin tới sau bước cuối bị mất - chạy riêng 3/3 xanh, xem SPEC) và `i12` |
| PTY đầy đủ lần 2 (sau bản sửa inbox + ca q03b) | 35 passed, 3 failed: `i12`, `i01`, `t07_pty_plain_flag`; `i01`/`t07` chạy riêng: đỏ 1/3 và 1/3 (test chờ chữ đầu khung rồi kiểm chữ vẽ sau) → sửa test chờ đúng chữ; sau sửa 4/4 và 4/4 xanh |
| **PTY đầy đủ lần 3 (bản cuối)** | **37 passed, 1 failed (`i12`)**, 38 ca = 25 cũ + 13 ca q |
| `i12` ở mốc `48676b6` (worktree riêng, trước track) | **đỏ cùng dòng 2020** → không do track này |
| `cargo test -p harness-cli --bin ha` (bản cuối) | 491 passed, 0 failed |
| `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`, `Verify-Docs.ps1` | sạch, `FMT_OK`, `DOCS_OK` |

**Lỗi còn lại, có từ trước:** `i12_a_prompt_with_an_unreachable_provider_is_reported_and_the_app_stays_alive` đòi thông báo lỗi nêu endpoint (`127.0.0.1:<port>`), nhưng commit `7a1dcc5` (audit 22/09) cố ý bỏ URL khỏi lỗi gửi request (`error.without_url()`), và đầu TUI không còn in endpoint. Thông báo hiện là `failed: service_unavailable: provider request failed: error sending request`. Chọn một: nêu host:port (không path/query) trong lỗi kết nối, hoặc sửa kỳ vọng của test — cần người dùng quyết (không tự nới test).

**not_run:** live smoke với provider thật (429 thật, model phụ thật, gate dài).
