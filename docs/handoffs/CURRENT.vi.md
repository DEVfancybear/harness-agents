# CURRENT — bàn giao đang mở

**Cập nhật:** 21/09/2026 · **Assignment:** M3 (TurnDriver, human input và bounded goals), scope `M3-01..M3-04`. Tiền nhiệm: M0/M1/M2 đã push (`f1fb002`+`4d6393e`, `a824b2c`, `6c91a62`).

## 1. Assignment hiện tại và ràng buộc mới nhất

- Prompt người dùng (21/09/2026): triển khai M3; đọc README/CONTRACTS/M3 + acceptance A09/A10/A11; xác minh prerequisites M1/M2; tạo SPEC; sửa crate hiện có, nối vào cùng binary `ha`; reuse tests; từng item theo thứ tự, targeted tests rồi milestone gate; không stub-success/không đổi fixture để che lỗi; bàn giao evidence/digest/discovery/OS limitations + CURRENT handoff; **dừng sau M3**; sau đó user yêu cầu trực tiếp **commit and push**.
- Quyền đã cấp trong session: local cargo/pwsh, **commit/push** M3 sau khi gate xanh. Không paid API, không live smoke, không spawn agent.
- Ràng buộc từ sổ tay: một workspace/CLI `ha`; không tạo runner/registry cạnh tranh với M0-04; giữ thay đổi không liên quan của user (`docs/OPERATOR_GUIDE.*`).

## 2. Branch/base/source digest

- Branch `master`; M3 rebase trên `6c91a62` (M2).
- Source digest lần gate cuối: `sha256:09a19d63cb4559901eec6f6df3ce1087ca9baa8101b4a3fe3a4a098c948794d7` (332 file, loại `docs/evidence/M3.vi.md` + file này) — từ `GATE_RESULT_JSON.source_tree`.

## 3. Work item

| Item | Trạng thái | Evidence |
|---|---|---|
| M3-01 durable driver + minimum context | `implemented_unverified` | runtime schema v2 (`runs`, `run_steps`), `freeze_run_step` atomic với budget, manifest hash/step, `AgentRunId`/`StepId`, `is_dispatchable()` chặn stream invalid; `m3_01_*` (5 test) |
| M3-02 question/steering/cancel | `implemented_unverified` | `questions` + `HumanInputService`, `run_commands` + `RunInbox`, CLI `ha input`; `m3_02_*` (3) + `a11_human_input_grant` (M3 half) |
| M3-03 accounting + loop detection | `implemented_unverified` | `BudgetLedger` + 2 bảng, settle theo usage M2 khi có, loop detector window 6/limit 3; `m3_03_*` (4) |
| M3-04 terminal/goal + CLI run | `implemented_unverified` | `goal.rs` typed criteria/verdict, continuation bounded, CLI `--mock/--goal/...`, JSON run/acceptance; `a09`, `a10`, `m3_04_*` (5) |
| Registry/gate | đã nối | thêm entry `M3` vào `tests/acceptance/milestones.json` của M0-04; không tạo runner thứ hai |

## 4. File đã đổi

Sửa: `crates/harness-types/src/{ids,lib}.rs`, `crates/harness-store-sqlite/src/{lib,models,store}.rs`, `crates/harness-runtime/src/lib.rs`, `crates/harness-tools/src/{lib,turn_driver}.rs`, `crates/harness-cli/src/main.rs`, `crates/harness-cli/src/interactive/{events,headless,mod,service}.rs`, `crates/harness-cli/src/interactive/tui/history.rs`, `crates/harness-cli/tests/{interactive_session,phase_p2,phase_p7}.rs`, `tests/acceptance/milestones.json`.
Thêm: `crates/harness-store-sqlite/src/store/run.rs`, `crates/harness-runtime/src/{budget,goal,human_input,inbox}.rs`, `crates/harness-cli/tests/milestone_m3.rs`, `docs/specs/M3.vi.md`, `docs/evidence/M3.vi.md`, file này.

## 5. Contract/ADR đã chốt — không đổi ngầm

- Run/step identity dùng M0: `AgentRunId`, `StepId`; state vocabulary mirror `AgentState` (running/paused/completed/failed/canceled); driver chọn state qua `AgentState::apply(RunCommand)`. `waiting`/`blocked` phân biệt bằng `stop_reason` + `runs.acceptance`, không thêm state thứ hai.
- `RUNTIME_SCHEMA_VERSION = 2`; DDL additive; DB mới hơn bị từ chối ghi (`MigrationFailed`), read-only vẫn đọc; test nâng cấp v1→v2.
- Budget: `operation_id` unique; reserve trước dispatch; settle ưu tiên usage provider (frame cuối thắng), thiếu thì host estimate; unknown giữ bound; checked arithmetic; parent limit chặn child.
- Question: `scope_key` unique, answer one-shot; cùng payload → Duplicate, khác → Conflict, empty/expired không consent.
- A06 ở loop: stream thiếu terminal marker → `Unverified` (không execute/không retry); call malformed trong response có terminal → từ chối theo call, báo model, không execute.
- Task acceptance vẫn thuộc `AcceptanceTransition` (M0); M3 chỉ ghi proposal gắn run.

## 6. Lệnh đã chạy và kết quả cuối

- `cargo test -p harness-cli --test milestone_m3 --locked` → **19 pass**.
- `cargo test -p harness-cli --test milestone_m0|m1|m2 --locked` → 11 / 6 / 10 pass (m2 có 1 lần flake loopback khi chạy liền chuỗi, chạy riêng pass).
- `cargo test -p harness-cli --test phase_p1|phase_p2|phase_p3|phase_p7|interactive_session --locked` → 21 / 17 / 21 / 15 / 13 pass.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo fmt --all -- --check` → pass.
- `$env:RUST_TEST_THREADS='4'; pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M3` → **passed**: format/clippy/build/workspace-tests + dependency-allowlist 44 edge + M3 19 required + closure M1 11 + M2 12 + M0 11; digest §2. (Biến môi trường chỉ giảm song song test trên host flake; runner không đổi bước/assertion.)

## 7. Việc còn lại theo thứ tự

1. Reviewer đọc diff + `docs/evidence/M3.vi.md`; nếu gate xanh, nghiệm thu M3 (implementer không tự đặt `accepted`).
2. Chạy gate M3 trên **Linux** (và `Verify-Phase.ps1 -Phase P2/P3` như regression đa nền tảng).
3. **M4** mở A11 phần grant: normalized invocation proposal + grant bound, consume một lần, expiry/revoke; đồng thời bind `tool_call_id` vào intent/receipt (handoff M2) và implement `StorePort` qua adapter quanh store methods M3.
4. Khi M4/M5 chạm: phát `AcceptanceCommand` từ goal satisfied; reconciliation/expiry cho reservation `Unknown` (M9).
5. **Live smoke tùy chọn** (`scripts/Smoke-HaProvider.ps1`) với credential riêng — evidence hiện tại không claim tương thích live.

## 8. Next action chính xác

`pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M3 -Json` trên digest đã push; đọc `GATE_RESULT_JSON`, rồi reviewer quyết định nghiệm thu (không bắt đầu M4 trong session này).

## 9. Blocked on

Không. (Live smoke cần credential — ngoài scope bắt buộc.)

## 10. Không lặp lại

- Không thêm `RunId`/`RunStepId` hay state run thứ hai; dùng M0 contracts.
- Không thay `RUNTIME_SCHEMA_VERSION` thêm lần nữa nếu không có contract change + test compat.
- Không chạy migration trên data dir thật của user; fixture dùng temp dir.
- Không tạo `Verify-Milestone.ps1`/`milestones.json` cạnh tranh với M0-04.
- Gate `workspace-tests` có retry một lần (flake loopback/FS của host); **không** thêm retry cho `required-tests` hay nới assertion để đối phó flake.
- Không "sửa" test để che lỗi: thay đổi test của M3 là bổ sung (`milestone_m3`) và hai assertion version có chủ đích; fixture step-bound đổi query vì loop detector (ghi rõ).
