# CURRENT — bàn giao đang mở

**Cập nhật:** 21/09/2026 · **Assignment:** M1 (SQLite store, journal và recovery), scope `M1-01..M1-04`.

## 1. Assignment hiện tại và ràng buộc mới nhất của người dùng

- Prompt người dùng (21/09/2026): triển khai M1 trong scope M1-01..M1-04; đọc README/CONTRACTS/M1 + acceptance A01/A02/A05; xác minh prerequisites M0; tạo SPEC; sửa crate hiện có và nối vào cùng binary `ha`; reuse tests; làm từng item theo thứ tự, targeted tests rồi milestone gate; không production stub-success, không đổi expected fixture để che lỗi; bàn giao evidence/digest/test discovery/OS limitations + CURRENT handoff; **dừng sau M1**, không tự chạy milestone tiếp.
- Quyền: chạy local (cargo/PowerShell, temp dir, mock provider). **Không** commit/push/publish, không paid API, không spawn agent. Prompt mẫu không tạo quyền mới; M0 đã commit/push ở session trước theo yêu cầu riêng.

## 2. Branch/base/source digest

- Branch: `master`; base `4d6393e` (M0 committed/pushed).
- M1 **chưa commit** (git status sẽ hiện các file ở §4).
- Source digest lần gate cuối: `sha256:1af1a78c218af4f4cd5bae129fcbbe98f75a41621de54f68af7f2bfc75c3aa73` (319 file, loại trừ evidence M1 + file này).

## 3. Work item

| Item | Trạng thái | Evidence |
|---|---|---|
| M1-01 Migrations và writer ownership | `implemented_unverified` | marker `harness-data.json` + validate/reject; test newer schema; generation takeover/stale fence (`crates/harness-store-sqlite/src/{store,models}.rs`) |
| M1-02 Input, inbox và projections | `verified_by_existing_suites` | reuse P1 (`p1_c21`, `p1_s04`, `p1_property_*`); **StorePort hoãn sang M3/M4** (ADR-N02 §2.8) |
| M1-03 Artifacts, checkpoints và replay | `implemented_unverified` | `m1_03_orphan_artifact_is_only_reachable_by_reference` + `m1_03_checkpoint_commit_failure_leaves_the_journal_foldable` (failpoint `BeforeSnapshotCommit`) |
| M1-04 Recovery queries và CLI inspection | `implemented_unverified` | `ha status`: `pending_work` + `blocking`, không fail khi block; `resume` vẫn typed |

## 4. File đã đổi

Sửa: `crates/harness-store-sqlite/src/{store,models,lib}.rs`, `crates/harness-cli/src/main.rs`, `crates/harness-cli/tests/milestone_m0.rs` (adapt registry), `scripts/Verify-Milestone.ps1` (qualified selector `target::test` + self-test), `tests/acceptance/milestones.json` (entry M1 + A01/A02/A05 `implemented`).
Thêm: `crates/harness-cli/tests/milestone_m1.rs`, `tests/fixtures/m1/marker/{newer-format,corrupt}.json`, `docs/specs/M1.vi.md`, `docs/adr/ADR-N02-STORE-OWNERSHIP-DURABILITY.{vi,en}.md`, `docs/evidence/M1.vi.md`, file này.

## 5. Contract/ADR đã chốt — không đổi ngầm

- ADR-N02: lock scope = một data directory (`writer.lock`), fence durable (`host_epoch.generation`, `StaleWriter`), durability WAL+FULL+FK+busy 5s, marker `harness-data.json` không bao giờ bị ghi đè, artifact sống theo reachability/pin (`RetentionRefused`), outbox logic = `parent_deliveries`, reopen ≠ rerun.
- `ha status --json` chỉ thêm key (`pending_work`, `blocking`); session bị block báo cáo thay vì fail.
- A01/A02/A05 do M1 sở hữu, map sang selector qualified trong `tests/acceptance/milestones.json`; registry P/H không đổi.
- Gate hỗ trợ required test dạng `target::test_name`; `-SelfTest` có 7 negative control.

## 6. Lệnh đã chạy và kết quả cuối

- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings` → pass.
- `cargo test -p harness-cli --test milestone_m1 --locked` → 6 pass.
- `cargo test -p harness-cli --test milestone_m0 --locked` → pass (adapt).
- `cargo test -p harness-cli --test phase_p1 --test phase_p2 --test phase_p7 --locked` → pass.
- `pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M1 -SelfTest` → 7/7 OK.
- `pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M1 -Json` → `result: passed`, closure `[M1, M0]`, 11 required test M1 + 11 closure M0 + 1 unit test + 44 edge; digest ở §2.

## 7. Việc còn lại theo thứ tự

1. **Commit/push M1** nếu người dùng cấp quyền (chỉ khi được yêu cầu trực tiếp).
2. **Chạy gate P1 (và P7) như regression đa nền tảng**, cùng gate M1 trên Linux → platform proof.
3. **M3/M4:** implement `StorePort` khi lease/step/proposal/grant/receipt shape đã chốt (ADR-N02 §3); reconciliation side effect thuộc M3/M4.
4. **M1 hardening (tùy chọn):** thêm negative control cho marker xuất hiện giữa hai writer (race) — hiện đã fail-closed bằng `create_new` + validate.

## 8. Next action chính xác

Chạy `pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P1` (Windows) để xác nhận regression P1 trên digest M1, rồi giao reviewer đọc `docs/evidence/M1.vi.md`.

## 9. Blocked on

Không. (M1 không cần credential/paid API.)

## 10. Không lặp lại

- Không thêm bảng `outbox` thứ hai; không đổi `STORE_SCHEMA_VERSION`.
- Không implement `StorePort` giả; không "sửa" test để che lỗi (mọi thay đổi test đều là adapt contract, ghi trong evidence §8).
- Không chạy migration/store M2+ khi chưa được giao.
