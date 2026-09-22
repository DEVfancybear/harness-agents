# CURRENT — bàn giao đang mở

**Cập nhật:** 23/09/2026 · **Assignment:** M4–M12 theo kế hoạch `implementation-next`, tuần tự theo dependency và gate từng checkpoint. **Checkpoint hiện tại:** **M6-01..M6-04 xong, gate M6 đã xanh** (chi tiết §11). Tiền nhiệm: M0/M1/M2 (`4d6393e`/`f1fb002`, `a824b2c`, `6c91a62`), M3 (`03bea9a`), M4 (`79c5165`), M5 (`b6e99bc`, `b8fa717` gate xanh).

## 1. Assignment hiện tại và ràng buộc mới nhất

- User (22/09/2026): triển khai M4–M12 theo dependencies, gate từng checkpoint, không chuyển tiếp khi prerequisites chưa đạt; cập nhật handoff sau mỗi checkpoint.
- User (23/09/2026, lượt này): "Triển khai M6 (Skills, MCP và versioned extensions) trong scope M6-01..M6-04 … Dừng sau M6; không tự chạy milestone tiếp." → **M6 dừng ở đây**; M7 chỉ bắt đầu khi có assignment mới.
- Quyền: local cargo/pwsh, docs SPEC/evidence/handoff, commit/push cho công việc M (đã cấp trong session). **Không** cấp: đổi User PATH, cài thật (`Install-Ha.ps1`), paid smoke/live provider, publish/release — tới M9 sẽ dừng và xin phép trước các bước đó.
- Giữ `docs/OPERATOR_GUIDE.*` (thay đổi có trước của user, không commit).

## 2. Branch/base/source digest

- Branch `master`; base M6 `b8fa717` (đầu M6 là M5 `b8fa717`).
- **Source digest của revision đã test:** `sha256:2435d287e8ac48d7269ad2427f2b28f056cf13656ff9ad66bb34115af5fbae13` (**352 file**), lấy từ `GATE_RESULT_JSON` của lần chạy `-Milestone M6` (log `target/verification-m6-gate.log`). Digest tính ở cuối lần chạy trên `git ls-files --cached --others --exclude-standard`, **trừ** `docs/evidence/M6.vi.md` + `docs/handoffs/CURRENT.vi.md` (ghi sau gate).
- Trạng thái registry: M6 để `in_progress` — implementer **không** tự đặt `accepted`/`verified_local`; verdict là việc của reviewer (§7.0).

## 3. Work item M6

| Item | Trạng thái | Evidence |
|---|---|---|
| M6-01 catalogue skill metadata-only + activation pin + contributor | `implemented_unverified` | `TrustedSkillRoot`/`SkillCatalogEntry` (**không** có field content)/`SkillConflict.conflict_id`; scan chỉ đọc head 4 KiB + hash streaming, body chỉ đọc trong `activate` và được đối chiếu digest; `SkillContributor` phát block channel `skill` (authority `User`); 3 test — `m6_01_catalog_lists_metadata_without_reading_content`, `m6_01_a_hostile_skill_script_never_runs_during_a_scan`, `a22_skill_version` |
| M6-02 cancel bounded + dead-state + generation lease | `implemented_unverified` | `FrameKind::Cancel` (protocol vẫn 1), `CancelOutcome{Acknowledged,Ignored}`, `cancel_call(id, grace=2000ms)`; `dead` set ở cả `terminate_process_tree` **và** EOF reader; `ExtensionRuntime::lease` + `ExtensionLease` generation-bound; `unload_draining` ⇒ `UnloadReport{drained,cut_short,uncertain}`; 2 test — `m6_02_cancel_is_bounded_and_a_crash_invalidates_handles`, `m6_02_scope_lease_and_partial_init_cleanup` |
| M6-03 MCP tools **và** resources, schema validate, gate | `implemented_unverified` | `MCP_SPEC_REVISION="2026-07-28"` + `MCP_SDK_VERSION="3.4.0"` (rmcp `=3.4.0`); `McpSupportMatrix` là nguồn duy nhất cho supported/unsupported; `discover_resources`/`read_resource` (1 text part, blob ⇒ `BinaryContentDenied`, digest + provenance); `validate_tool_schema`/`validate_arguments`; `McpMetadataCache` không lưu cursor; `McpToolDispatcher: ExternalToolDispatcher` là đường duy nhất vào `ToolService`; 2 test — `m6_03_mcp_schema_and_resource_provenance`, `a23_extension_bounds` |
| M6-04 deferred schema + config explain | `implemented_unverified` | `ToolCatalog`/`CatalogEntry` (schema deferred + `revoked`)/`PromotedTool::revalidate` (catalog digest + policy revision + revoked); `ToolContributor` chỉ phát **tên** trên channel Reference; `ConfigExplain` + `ReloadBoundary`; `ha extensions catalog|config-explain`; 5 test — `m6_04_promotion_is_bounded_and_revalidated`, `m6_04_unload_drains_or_rejects_inflight`, `m6_04_config_explain_reports_precedence_and_inactive_reasons`, `a24_catalog_revocation`, + 2 CLI test |

`milestone_m6` = **13/13**; gate M6 = **passed** (13 required, discovery 13, closure M5/M4/M3/M1/M2/M0).

## 4. File đã đổi (M6)

**Mới:** `crates/harness-extensions/src/{catalog,config}.rs`, `crates/harness-cli/tests/milestone_m6.rs`, `crates/harness-cli/src/bin/m6_fixture_mcp_server.rs`, `docs/specs/M6.vi.md`, `docs/evidence/M6.vi.md`.

**Sửa:** `crates/harness-session/src/context.rs` (`ContextChannel::Skill`, port `ContextContributor`, `build_with_contributors`), `harness-session/src/lib.rs`, `harness-extensions/src/{skills,mcp,transport,host,contracts,bridges,lib}.rs`, `harness-extensions/Cargo.toml`, `harness-cli/src/extension_cli.rs` (2 subcommand mới + khối `mcp`), `harness-cli/src/bin/p6_fixture_plugin.rs` (4 mode mới), `schemas/dependency-allowlist.v1.json` (+1 edge), `tests/acceptance/milestones.json` (M6 + A22/A23/A24 + selectors), `scripts/Verify-Milestone.ps1` (chữ ký flake + chuyển tiếp retry notice).

**Sửa ngoài scope M6, có lý do đo được:** `crates/harness-cli/tests/interactive_launch.rs` (`sse_fixture` accept trong vòng lặp — fixture cũ phục vụ readiness probe rồi đóng listener, làm request thật bị từ chối; chữ ký retry loopback), `crates/harness-cli/tests/milestone_m2.rs` (gom chữ ký flake vào `is_loopback_refusal`).

## 5. Contract/quyết định đã chốt — không đổi ngầm

- **`EXTENSION_PROTOCOL_VERSION` giữ 1** dù thêm `FrameKind::Cancel`: bump sẽ từ chối mọi plugin P6 hợp lệ mà không có lợi ích. Peer không hiểu cancel vẫn bị từ chối typed qua `decode_line` (fail-closed).
- **`CONTEXT_SCHEMA_VERSION` giữ 1**, **`TOOL_CONTRACT_VERSION` giữ 1**, **`schemas/error-report.v1.schema.json` không đổi** (không thêm `ErrorCode` nào). Không migration, không đổi data home.
- **Edge mới `harness-extensions -> harness-session`** (allowlist 44→45): contributor port ở `context/*`, contributor cụ thể ở `extensions/*`; chiều ngược lại là sai kiến trúc.
- **Skill channel được phép mandatory** (`may_be_mandatory = true`, authority `User`): activation là hành vi tường minh của user, cùng lớp với project rule. Đây là quyết định **có rủi ro review** — nếu reviewer cho rằng skill phải luôn optional, đổi `may_be_mandatory` cho `Skill` là 1 dòng + 1 assert.
- **MCP spec/SDK pin:** spec `2026-07-28`, SDK `rmcp =3.4.0`. `McpSupportMatrix` là nguồn duy nhất; prompts/sampling/elicitation/subscriptions/resource templates/remote transport **không hỗ trợ** và được công bố là không hỗ trợ.
- **`ha extensions skills` đổi payload JSON** (thêm `catalog_digest`/`conflicts`/`byte_len`/`content_loaded`, bỏ `replaced`). `skill_count` + `grants_resolved` giữ nguyên nên `p6_s07` vẫn xanh.
- **Cancel là best-effort có biên:** host gửi frame, chờ `CANCEL_GRACE_MS = 2000`, hết hạn thì kết thúc process tree và outcome là `Uncertain`. Không hứa plugin dừng.

## 6. Lệnh đã chạy và kết quả

- `cargo test -p harness-cli --test milestone_m6 --locked` → **13/13 pass**.
- `cargo test -p harness-cli --test {phase_p6,phase_p1,milestone_m5,milestone_m0,interactive_launch} --locked` → **15/15, 21/21, 11/11, 11/11, 19/19 pass**.
- `cargo fmt --all -- --check` + `cargo clippy --workspace --all-targets --locked -- -D warnings` → pass.
- `pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M6` → **passed**: format/clippy/build/workspace-tests/dependency-allowlist (45 edge)/unit-tests (0)/discovery (13)/required-tests (13)/closure-M5 (11)/closure-M4 (14)/closure-M3 (19)/closure-M1 (11)/closure-M2 (12)/closure-M0 (11). **`workspace-tests` cần 2 lần thử** (`GATE_RETRY` + `GATE_STEP_RETRIED` trong log).
- `pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M6 -SelfTest` → `MILESTONE_GATE_SELFTEST_OK: M6` (7 negative control của chính gate).
- Negative control M6 (bảng đầy đủ ở `docs/evidence/M6.vi.md` §7): N1/N2/N3/N4/N5/N6/N7/N10 **bị bắt**; **N8 không bị bắt** và đã ghi rõ lý do (kiểm generation ở lease là defence in depth, trùng với dead-state của transport; không có đường public nào tạo được "transport sống nhưng generation đổi").

## 7. Việc còn lại theo thứ tự

0. **Reviewer nghiệm thu M6**: gate đã xanh + `docs/evidence/M6.vi.md` §4/§5/§7. Điểm cần soi kỹ: (a) **skill channel mandatory** (§5) có đúng ý runbook không; (b) **N8 chưa có control riêng** (evidence §7.1); (c) `McpToolDispatcher` là đường duy nhất *được nối*, không phải API duy nhất *tồn tại* (evidence §9).
1. **Flake loopback của host vẫn còn** ở `milestone_m2::a07_401_is_not_retried_and_transient_is_bounded` (evidence §5.1): base `b8fa717` **0/8** lần xanh, sau khi sửa chữ ký retry **3/8**. Gate vượt qua được vì `workspace-tests` retry tối đa 3 lần, nhưng một lần đỏ thuần-assertion sẽ **không** được retry. Sửa triệt để cần dựng lại `FakeProvider` theo từng attempt — việc của M2.
2. **Gate đa nền tảng**: `ci.yml` chạy P0/P3–P7 trên ubuntu **và** windows nhưng **không** chạy `milestone_m5`/`milestone_m6`. A22/A23/A24 mới chỉ được chứng minh trên Windows. Muốn `accepted` đa nền tảng: thêm bước `Verify-Milestone.ps1 -Milestone M6` vào CI (ubuntu) hoặc chạy trên host Linux/WSL.
3. **M7** chỉ khi được giao — prerequisites M6 đã xong.
4. Việc không thuộc M6: StorePort vẫn chưa implement (handoff M3); live/paid smoke; backup/restore/retention (M9).

## 8. Next action chính xác

**Dừng.** Không chạy M7 hay milestone khác cho tới khi user giao. Nếu user yêu cầu tiếp: đọc `docs/implementation-next/M7.vi.md` + `CONTRACTS.vi.md`, xác minh gate M6 trên revision hiện tại, rồi làm SPEC M7 trước khi code. Nếu user muốn chứng minh đa nền tảng cho M6: push revision này và thêm bước gate M6 vào `ci.yml` (hoặc chạy trên Linux), rồi đối chiếu `a22_skill_version`/`a23_extension_bounds`/`a24_catalog_revocation`.

## 9. Blocked on

Không có blocker kỹ thuật cho M6: gate xanh trên host Windows này. Ba điểm cần người quyết: (1) `accepted` đa nền tảng cần chạy Linux (máy này không có WSL/Docker — CI là đường duy nhất); (2) ngữ nghĩa skill-channel-mandatory cần reviewer xác nhận; (3) flake `milestone_m2` (§7.1) cần một lượt M2 riêng nếu muốn `workspace-tests` xanh ổn định.

## 10. Không lặp lại

- Không commit file của session khác: stage theo path cụ thể.
- Không đổi `EXTENSION_PROTOCOL_VERSION`/`TOOL_CONTRACT_VERSION`/`CONTEXT_SCHEMA_VERSION` khi chỉ thêm khả năng; không sửa schema JSON bằng tay — chạy `cargo run -p harness-types --bin generate_schemas --locked` rồi để drift test `p0_f03` xác nhận.
- Không ghi event vào journal của một session chưa admit input.
- Không gọi rollback là "restore"; không claim sandbox/strict isolation; không claim MCP feature nào ngoài `McpSupportMatrix`.
- Không chạy live provider/paid smoke; không cài đặt/đổi PATH user; không publish.
- Không viết code chỉ tồn tại trên một OS mà không có đường biên dịch nó ở OS kia (`cfg!`, không `#[cfg]` cặp).
- **Không lặp lại thử nghiệm đã bác bỏ:** thêm vòng drain "đọc tới khi client đóng" vào nhánh non-200 của `FakeProvider` (`milestone_m2.rs`) làm `a07_401` **tệ hơn** (timeout ở nhánh 503×2) — đã revert (evidence §5.1).
- Không để secret vào evidence.

## 11. Checkpoint M6 (lượt này) — tóm tắt

- **Base:** `b8fa717`; thay đổi: 18 file sửa + 6 file mới (`catalog.rs`, `config.rs`, `milestone_m6.rs`, `m6_fixture_mcp_server.rs`, `docs/specs/M6.vi.md`, `docs/evidence/M6.vi.md`).
- **Gate:** `-Milestone M6` **passed**, digest `sha256:2435d287…` (352 file), 13/13 required, closure M5/M4/M3/M1/M2/M0; `workspace-tests` cần 2 lần thử (flake loopback có sẵn của host).
- **Ba bug thật** do test bắt và đã sửa trong code: EOF không set `dead`; `additionalProperties` đảo nghĩa trong `validate_arguments`; lỗ hổng test ở nhánh đối chiếu digest khi activate.
- **Hai nguyên nhân gốc của flake có sẵn** đã sửa: `sse_fixture` đóng listener sau readiness probe; chữ ký retry loopback quá hẹp ở `milestone_m2` (0/8 → 3/8 lần xanh).
- **Hai sửa đổi gate:** chữ ký flake khớp `error sending request`; retry notice không còn bị `[void]` nuốt — nên log **nói thật** rằng `workspace-tests` đã phải retry.
- **Giới hạn đã ghi:** nhánh Unix chưa chạy; A22/A23/A24 chỉ trên Windows; N8 chưa có negative control riêng; `McpClient::call_tool` vẫn `pub` (P6 dùng).

### Checkpoint trước (tham chiếu)

- **M5** (`b6e99bc`, gate xanh, digest `sha256:3afe4125…`, 346 file): `docs/evidence/M5.vi.md`.
- **M4** (`79c5165`, gate xanh hai lần, digest `sha256:f34047b5…`, 341 file): `docs/evidence/M4.vi.md`.
- **M3** `03bea9a` (`verified_local`), **M0/M1/M2** `4d6393e`/`f1fb002`/`a824b2c`/`6c91a62`.
