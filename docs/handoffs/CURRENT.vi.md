# CURRENT — bàn giao đang mở

**Cập nhật:** 21/09/2026 · **Assignment:** M0 (Foundation và executable contracts), scope `M0-01..M0-04`.

## 1. Assignment hiện tại và ràng buộc mới nhất của người dùng

- Prompt người dùng (21/09/2026): triển khai M0 trong scope M0-01..M0-04; đọc README/CONTRACTS/M0/ACCEPTANCE; xác minh prerequisites; tạo SPEC; sửa crate hiện có trong root workspace và nối vào cùng binary `ha`; reuse tests/services; làm từng item theo thứ tự, chạy targeted tests rồi milestone gate; không dùng production stub-success hay đổi expected fixture để che lỗi; bàn giao evidence/digest/test discovery/OS limitations và CURRENT handoff; **dừng sau M0**, không tự chạy milestone tiếp.
- Quyền: chạy local (cargo/PowerShell, temp dir, mock provider) và — theo yêu cầu trực tiếp "commit and push" của người dùng trong session này — commit/push M0. Không publish, không paid API, không spawn agent tự động. Prompt mẫu không tạo quyền mới.
- Người dùng đã commit phần HA_TUI đang dở trong session (`dfee38a`, `bebc6cc`); M0 không sửa/revert phần đó.

## 2. Branch/base/source digest

- Branch: `master`, commit `f1fb002` (`feat(m0): executable contracts, run/acceptance reducers and a milestone gate`), đã push lên `origin/master` theo yêu cầu trực tiếp của người dùng.
- Base: `bebc6cc` (`docs(HA_TUI): the measured counts, as measured on this tree`).
- Source digest của lần gate cuối: `sha256:00f3d5bf25f652ab5e625a13c71e91cc0a82940be0dd68a7c10d5f7f48715db6` (312 file, loại trừ `docs/evidence/M0.vi.md` và file này); đã xác minh lại trên cây đã commit.

## 3. Work item đã xong (kèm evidence)

| Item | Trạng thái | Evidence |
|---|---|---|
| M0-01 Identities và state reducers | `implemented_unverified` | `docs/evidence/M0.vi.md` §M0-01; `crates/harness-types/src/{error,ids,scope,acceptance}.rs`; `crates/harness-runtime/src/lib.rs` (`RunCommand`/`RunStateEvent`/`RunTransition` + test table); `crates/harness-orchestrator/src/contracts.rs` |
| M0-02 Ports, envelopes, schema fixtures | `implemented_unverified` | `crates/harness-types/src/{ports,version}.rs`; `tests/fixtures/m0/envelope/*`; `schemas/dependency-allowlist.v1.json`; `crates/harness-cli/src/bin/dependency_check.rs` |
| M0-03 Workspace, config, CLI | `implemented_unverified` | `crates/harness-cli/src/main.rs` (exit code typed + JSON error report khi `--json`); `SessionService::with_id_source`; `schemas/error-report.v1.schema.json` |
| M0-04 Acceptance registry và gate runner | `implemented_unverified` | `tests/acceptance/milestones.json`; `scripts/Verify-Milestone.ps1` (+ self-test 6 negative control); `crates/harness-cli/src/bin/m0_fixture_host.rs` |

## 4. File đã đổi và lý do

Sửa: `crates/harness-types/src/{error,ids,lib,schema}.rs`, `crates/harness-runtime/src/lib.rs`, `crates/harness-session/src/lib.rs`, `crates/harness-orchestrator/src/contracts.rs`, `crates/harness-cli/src/main.rs`, `crates/harness-cli/Cargo.toml`, `crates/harness-cli/tests/{phase_p0,phase_p2}.rs` (adapt theo contract mới).
Thêm: `crates/harness-types/src/{scope,acceptance,ports,version}.rs`, `crates/harness-cli/src/bin/{dependency_check,m0_fixture_host}.rs`, `crates/harness-cli/tests/milestone_m0.rs`, `schemas/{dependency-allowlist.v1.json,error-report.v1.schema.json}`, `tests/acceptance/milestones.json`, `tests/fixtures/m0/**`, `scripts/Verify-Milestone.ps1`, `docs/specs/M0.vi.md`, `docs/adr/ADR-N01-DOMAIN-IDENTITY-STATE.{vi,en}.md`, `docs/evidence/M0.vi.md`, file này.

## 5. Contract/ADR đã chốt — không đổi ngầm

- ADR-N01: bốn identity tách biệt (`SessionId`/`TaskId`/`AgentRunId`/`StepId`); ordering chỉ bằng `seq`; ba state machine độc lập; terminal không regress; acceptance cần evidence typed; human acceptance ghi actor+source và không sửa criteria; `ErrorReport` + `RetryClass`; `ScopeContext` do host tạo.
- CLI: lỗi ở **stderr** (contract H/P giữ nguyên); `--json` của subcommand legacy in một JSON error document có version ở stderr; stdout rỗng khi lỗi; exit code theo `ErrorCode::exit_code()` (2/3/4/5/130/1).
- `StorePort` là contract-only: **không** có implementation production trước M1 (chỉ test double trong `#[cfg(test)]`); gate M0 kiểm bằng test quét `crates/*/src`.
- Dependency allowlist là nguồn duy nhất cho edge nội bộ; `harness-runtime → harness-tools` và mọi edge từ `harness-types` bị cấm.

## 6. Lệnh đã chạy và kết quả cuối

- `cargo fmt --all -- --check` → pass.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` → pass.
- `cargo test --workspace --all-targets --locked` → pass (số suite trong evidence).
- `pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M0 -SelfTest` → pass, 6 negative control.
- `pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M0 -Json` → `result: passed`, 11/11 required tests, 5 unit tests, 44 edges.
- `pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P0` (regression) — xem evidence; các gate P/H khác **chưa chạy lại toàn bộ** trong session này (workspace-tests bao phủ test của chúng).

## 7. Việc còn lại theo thứ tự (với test oracle)

1. **Chạy đủ gate P/H như regression đa nền tảng** (`Verify-Phase.ps1 -Phase P0..P7`) trên Windows và Linux trước khi reviewer nghiệm thu M0 — oracle: mỗi gate exit 0.
2. **M1**: implement `StorePort` trên `SQLite` (migration + transaction), thêm bảng runs/steps; oracle: A01/A02/A05 cases + test `ports` hiện có phải thấy implementation thật (test M0 sẽ fail nếu implementation nằm ngoài M1 — đúng thiết kế).
3. **Clock injection**: hoãn sang M3 (chỉ `TurnLimits` của `harness-tools` cần wall-clock).
4. **Ghi chú OS**: các case cần console thật (ConPTY/pty) vẫn `ignored` như trước, chạy bằng `scripts/Invoke-HaPtyAcceptance.ps1`; M0 không đổi phần này.

## 8. Next action chính xác

Chạy `pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P7` (Windows) để xác nhận toàn bộ regression P/H trên digest hiện tại, rồi giao reviewer đọc `docs/evidence/M0.vi.md`. Nếu gate P7 xanh, M0 có thể được reviewer nghiệm thu (`verified_local` → `accepted` chỉ do reviewer/user quyết định).

## 9. Blocked on

Không. (Không có credential/paid API nào cần cho M0.)

## 10. Không lặp lại

- Không tạo lại SPEC/ADR/registry/gate; không sửa `tests/acceptance/registry.json` (P/H).
- Không "sửa" test M0 để cho qua: mọi thay đổi test là adapt theo contract mới và đã ghi lý do (schema count 9→10, `RunCommand` thay `AgentState` trong `phase_p2`).
- Không chạy lại migration/store M1 trong khi chưa được giao.
