# CURRENT — bàn giao đang mở

**Cập nhật:** 22/09/2026 · **Assignment:** M4–M12 theo kế hoạch `implementation-next`, tuần tự theo dependency và gate từng checkpoint. **Checkpoint hiện tại:** **M5-01..M5-04 xong, gate M5 đã xanh** (chi tiết §11). Tiền nhiệm: M0/M1/M2 (`4d6393e`/`f1fb002`, `a824b2c`, `6c91a62`), M3 (`03bea9a`), M4 (`79c5165`, gate xanh hai lần).

## 1. Assignment hiện tại và ràng buộc mới nhất

- User (22/09/2026): triển khai M4–M12 theo dependencies, gate từng checkpoint, không chuyển tiếp khi prerequisites chưa đạt; cập nhật handoff sau mỗi checkpoint.
- User (22/09/2026, lượt này): "Triển khai M5 trong scope M5-01..M5-04 … Dừng sau M5; không tự chạy milestone tiếp." → **M5 dừng ở đây**; M6 chỉ bắt đầu khi có assignment mới.
- Quyền: local cargo/pwsh, docs SPEC/evidence/handoff, commit/push cho công việc M (đã cấp trong session). **Không** cấp: đổi User PATH, cài thật (`Install-Ha.ps1`), paid smoke/live provider, publish/release — tới M9 sẽ dừng và xin phép trước các bước đó.
- Giữ `docs/OPERATOR_GUIDE.*` (thay đổi có trước của user, không commit).

## 2. Branch/base/source digest

- Branch `master`; base M5 `925cc3b` (đầu M5 là M4 `79c5165`).
- **Source digest của revision đã test:** `sha256:3afe412503ca52ec486b2ab8a1120b3e6054ad127909c75fc4eb94d95ebef422` (**346 file**), lấy từ `GATE_RESULT_JSON` của lần chạy `-Milestone M5` (log `target/verification-m5-gate.log`). Digest tính ở cuối lần chạy trên `git ls-files --cached --others --exclude-standard`, **trừ** `docs/evidence/M5.vi.md` + `docs/handoffs/CURRENT.vi.md` (hai file này ghi sau gate).
- Trạng thái registry: M5 để `in_progress` — implementer **không** tự đặt `accepted`/`verified_local`; verdict là việc của reviewer (§7.0).

## 3. Work item M5

| Item | Trạng thái | Evidence |
|---|---|---|
| M5-01 context channel/authority/manifest | `implemented_unverified` | `ContextChannel` + `ContextBlock{channel,authority,provenance,supersedes,digest}`; luật supersede loại block bị thay; `ContextManifest` đóng băng cùng checkpoint; đo **toàn bộ** request (`fixed_request_bytes`) — `m5_01_channels_authority_and_manifest`, `m5_01_a_window_that_cannot_hold_the_request_pauses_typed` |
| M5-02 compaction CAS/rebase + summary fallback | `implemented_unverified` | generation barrier + `spawn_blocking` (không giữ worker/tx khi chờ summarizer) + checkpoint id minted + rebase bounded 3; `a12_compaction_cas`, `m5_02_compaction_tail_carries_pairs_not_orphans`, `a19_summary_failure_falls_back_to_the_mandatory_state` |
| M5-03 history index + notes + 2 tool read-only | `implemented_unverified` | `history_sources`/`history_terms` rebuild idempotent; scope **trong** SQL; exact paged read + digest; `SourceUnavailable` cho ref hết hạn; notes CAS `model_report`; schema count 11→13 — `m5_03_index_rebuild_is_identical_and_scope_is_a_filter`, `m5_03_notes_are_revision_checked_model_reports`, `a18_history_after_compaction` |
| M5-04 resume/fork/rollback | `implemented_unverified` | `resume_with_observation` (stale evidence, chỉ báo cáo); `fork_session` (view + grants, **không** ownership); `rollback_conversation` (event `session.rolled_back`, `filesystem_restored=false`) — `a20_source_scope_fork`, `m5_04_fork_policy_can_only_narrow`, `a21_rollback_effects` |

`milestone_m5` = **11/11**; gate M5 = **passed** (11 required, discovery 11, closure M4/M3/M2/M1/M0).

## 4. File đã đổi (M5)

**Mới:** `crates/harness-store-sqlite/src/store/history.rs` (index/search/read/notes/grants/expire), `crates/harness-cli/tests/milestone_m5.rs` (11 test), `crates/harness-cli/src/bin/m5_fixture_host.rs` (5 compaction + barrier + park để hard-kill), `docs/specs/M5.vi.md`, `docs/evidence/M5.vi.md`.

**Sửa:** `crates/harness-session/src/context.rs` (+`ContextChannel`/manifest/`fixed_request_bytes`), `harness-session/src/lib.rs` (re-export), `harness-runtime/src/lib.rs` (`compact`, `paired_tail`, `deterministic_summary`, `resume_with_observation`, `fork_session`, `rollback_conversation`, `build_context(checkpoint_id)`), `harness-store-sqlite/src/{store.rs,lib.rs,models.rs}` (`ensure_context_schema`, `record_fork_link`, `open_forked_session`, checkpoint readers), `harness-tools/src/{contracts,service,turn_driver}.rs` (2 tool mới + render), `harness-types/src/error.rs` (`SourceUnavailable`), `schemas/error-report.v1.schema.json` (regenerate), `crates/harness-cli/tests/{phase_p2,phase_p3,phase_p4}.rs` (caller + schema count), `tests/acceptance/milestones.json` (M5 + 4 case).

## 5. Contract/quyết định đã chốt — không đổi ngầm

- **`CONTEXT_SCHEMA_VERSION = 1`** (mới): `ensure_context_schema` chỉ thêm cột additive (`context_packets.manifest_json`, `context_checkpoints.manifest_json`, `session_lineage.fork_policy`) theo PRAGMA; DB cũ nâng tại chỗ; host cũ gặp DB mới hơn vẫn từ chối.
- **`TOOL_CONTRACT_VERSION` giữ 1**; schema count 11→13 (`history_search`, `history_read`, cả hai read-only, `effect_class_for` = read-only, `phase_p3` assert 13 ở hai chỗ). Lý do: thứ được version là digest từng tool, không phải số lượng.
- **`ErrorCode::SourceUnavailable`** = `source_unavailable`, `RetryClass::Never`, exit 1: dùng khi một `SourceRef` đã index nhưng không còn đọc được (hết hạn). `schemas/error-report.v1.schema.json` regenerate từ type.
- **Kênh context:** summary/note/reference **không** bao giờ trở thành mandatory (`may_be_mandatory` chỉ đúng cho policy/project_rule/instruction/state/tail/summary-as-host-block); block bị `supersedes` bị **loại** khỏi packet, không gửi hai bản.
- **Compaction:** checkpoint id **minted** (`ContextPacketId::generate()`), không suy từ sequence (hai compaction có thể cover cùng sequence); CAS trên sequence đã cover; summarizer chạy trong `spawn_blocking` **ngoài** mọi transaction; summary fail **hoặc rỗng** ⇒ `deterministic_summary` + `summary_source=deterministic_fallback`.
- **Fork = view, không ownership** (khác bản nháp SPEC đầu, đã ghi rõ ở SPEC §5): session con chỉ có row `sessions` + grant list; `task_leases`, `task_projections`, `tool_approvals` giữ nguyên ở cha; con **không** prepare/execute được tool (`TaskLeaseConflict`), chỉ đọc được source trong grant. Bỏ "fork ghi event `session.forked` trong journal con" vì (a) session con không admit input thì `recover()` fail, (b) bàn giao lease mà không bàn giao projection ⇒ hai session cùng cho là chủ task. Tiếp quản task là việc của `continue_task*`.
- **Rollback không phải restore:** chỉ ghi event + report; `filesystem_restored=false`; effect sau checkpoint được nêu tên; undo thật phải là action riêng qua gate M4.
- **Notes** luôn `authority=model_report`, CAS theo `(task_id, note_key)` + `expected_revision`; source list chỉ kiểm tồn tại/scope, **không** kiểm tính đúng.

## 6. Lệnh đã chạy và kết quả

- `cargo test -p harness-cli --test milestone_m5 --locked` → **11/11 pass**.
- `cargo test --workspace --all-targets --locked --no-fail-fast` → xanh toàn bộ (milestone m0–m5, phase p0–p7, interactive, known_defects, unit test 12 crate).
- `cargo fmt --all -- --check` + `cargo clippy --workspace --all-targets --locked -- -D warnings` → pass (6 lỗi clippy của code mới sửa bằng code thật: `collapsible_if`, `single_match_else`, `needless_pass_by_value`, `redundant_locals`, `format_push_string`, `too_many_lines`).
- `pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M5` → **passed**: format/clippy/build/workspace-tests/dependency-allowlist (44 edges)/unit-tests (0)/discovery (11)/required-tests (11)/closure-M4 (14)/closure-M3 (19)/closure-M1 (11)/closure-M2 (12)/closure-M0 (11).
- Negative control (đã khôi phục, không còn dấu vết — bảng đầy đủ ở `docs/evidence/M5.vi.md` §7): bỏ scope khỏi SQL search → A20 **FAIL**; ép `history_read` luôn `SourceUnavailable` → A18 **FAIL**; fallback compaction trả rỗng → A19 **FAIL**; `filesystem_restored=true` → A21 **FAIL**; bỏ CAS sequence → A12 **FAIL**; bỏ lọc `ForkPolicy` → `m5_04` **FAIL**.

## 7. Việc còn lại theo thứ tự

0. **Reviewer nghiệm thu M5**: gate đã xanh + `docs/evidence/M5.vi.md` §4/§7. Điểm cần soi kỹ: (a) **fork semantics** — con không execute được gì cho task (SPEC §5, evidence §8.1) có đúng ý runbook "Fork permitted source lineage into new session IDs/grants policy" không, hay cần thêm đường "fork rồi tiếp quản"; (b) `resume_with_observation` **chỉ báo cáo**, không ghi event/packet mới (evidence §8.2); (c) ranking của `history_search` chưa có phrase/relevance bonus nên chính câu hỏi của model cũng là một hit (evidence §8.3).
1. **Gate đa nền tảng**: `ci.yml` chạy P0/P3–P7 trên ubuntu **và** windows nhưng **không** chạy `milestone_m5`; A12/A18/A19/A20/A21 mới chỉ được chứng minh trên Windows. Muốn `accepted` đa nền tảng: thêm bước `Verify-Milestone.ps1 -Milestone M5` vào CI (ubuntu) hoặc chạy trên host Linux/WSL.
2. **M6** chỉ khi được giao — prerequisites M5 đã xong (M6 là skills/version retention; A22).
3. Việc không thuộc M5: StorePort vẫn chưa implement (handoff M3); live/paid smoke; backup/restore/retention (M9).

## 8. Next action chính xác

**Dừng.** Không chạy M6 hay milestone khác cho tới khi user giao. Nếu user yêu cầu tiếp: đọc `docs/implementation-next/M6.vi.md` + `CONTRACTS.vi.md`, xác minh gate M5 trên revision hiện tại, rồi làm SPEC M6 trước khi code. Nếu user muốn chứng minh đa nền tảng cho M5: push revision này và thêm bước gate M5 vào `ci.yml` (hoặc chạy trên Linux), rồi đối chiếu `a18_history_after_compaction`/`a20_source_scope_fork`/`a21_rollback_effects`.

## 9. Blocked on

Không có blocker kỹ thuật cho M5: gate xanh trên host Windows này. Hai điểm cần người quyết: (1) `accepted` đa nền tảng cần chạy Linux (máy này không có WSL/Docker — CI là đường duy nhất); (2) ngữ nghĩa `fork` (view-only) cần reviewer xác nhận là đúng ý runbook trước khi coi A20 là đóng.

## 10. Không lặp lại

- Không commit file của session khác: stage theo path cụ thể; tránh `cargo fmt --all` khi tree còn code chưa commit của họ. Lượt này tạo file **mới** cho M5 và chỉ sửa file dùng chung khi bắt buộc.
- Không đổi `TOOL_CONTRACT_VERSION`/`TOOLS_SCHEMA_VERSION` khi chỉ thêm tool; không sửa schema JSON bằng tay — chạy `cargo run -p harness-types --bin generate_schemas --locked` rồi để drift test `p0_f03` xác nhận.
- Không ghi event vào journal của một session chưa admit input (session đó sẽ không `recover()` được).
- Không bàn giao lease/projection cho session chưa sở hữu state — hai session cùng cho là chủ task là trạng thái không đọc được.
- Không gọi rollback là "restore"; không claim sandbox/strict isolation.
- Không chạy live provider/paid smoke; không cài đặt/đổi PATH user; không publish.
- **Không viết code chỉ tồn tại trên một OS mà không có đường biên dịch nó ở OS kia** (`cfg!`, không `#[cfg]` cặp) — bài học CI 21/09.
- Không để secret vào evidence.

## 11. Checkpoint M5 (lượt này) — tóm tắt

- **Base:** `925cc3b`; thay đổi: 16 file sửa (+1713/−223) + 4 file mới (`history.rs` 934 dòng, `milestone_m5.rs` 1460 dòng, `m5_fixture_host.rs`, `docs/specs/M5.vi.md`).
- **Gate:** `-Milestone M5` **passed**, digest `sha256:3afe4125…` (346 file), 11/11 required, closure M4/M3/M2/M1/M0.
- **Sáu negative control** đều cho kết quả mong đợi (§6, chi tiết `docs/evidence/M5.vi.md` §7).
- **Ba quyết định đã ghi rõ** (khác bản nháp SPEC): fork view-only; resume chỉ báo cáo; index chưa có relevance bonus.
- **Giới hạn đã ghi:** nhánh Unix chưa chạy; A18 chỉ trên Windows; fixture host bị kill cứng nên không phân biệt "kill" với "thoát sạch" ở tầng OS; page là byte thô.

### Checkpoint trước (tham chiếu)

- **M4** (`79c5165`, gate xanh hai lần, digest `sha256:f34047b5…`, 341 file): `docs/evidence/M4.vi.md` (gồm §8.4 negative control, §4c hai lỗi CI thật theo từng OS, §9 working tree có session thứ hai).
- **M3** `03bea9a` (`verified_local`), **M0/M1/M2** `4d6393e`/`f1fb002`/`a824b2c`/`6c91a62`.
