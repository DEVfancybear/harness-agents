# Handoff — HA_AGENT CP-E đang triển khai / CP-D và trước đó là lịch sử

## CP-E — G13–G14 current assignment (24/09/2026)

**Phạm vi:** Làm G13–G14 theo `docs/HA_AGENT_PLAN.vi.md`, dừng ở CP-E; giữ D1–D11. Không promote `tests/acceptance` sang accepted, không publish/cài lên máy user, không paid smoke, không chạy lại migration hay ghi config.local.toml của workspace thật. User yêu cầu đối chiếu Git/SPEC/evidence/plan, inspect symbol/test, affected tests và gate, cập nhật handoff. / Continue G13–G14 only and stop at CP-E.

**Branch/base:** `master`; bắt đầu ở `ffc93fe08b082a53216a26f4f2a5476222dec847` với worktree sạch. G13–G14 đã rebase và push tại `aad8960dd9b1aff5000e29ff01e66bd67cdf3a48` trên `728a4b4`; hiện chỉ còn ACL fixture correction + evidence/handoff delta của M6. `Cargo.lock` SHA-256 `38315c57cbfb563d37de95c778d52562ac3798f58224fc2517617e9289e7a859`. Windows NT 10.0.26200.0 x64, isolated Rust 1.97.1.

**Completed boundary / Ranh giới đã xong:** G13 code + sáu selector pass, i03 5/5, M3 20/20. G14 baseline H/L26 đã đo và ghi trong [evidence](../evidence/HA_AGENT.vi.md): baseline H đỏ; `a5ac2e2` còn lỗi compile lịch sử. CI job hai OS, shell fallback/receipt, `--cwd` tasks/maintenance, raw `/key`, footer ↑↓, M2 fixture per attempt, eight G selectors + unit-agent, bảy ca PTY và operator guide vi/en đã được sửa ở source. `StorePort for SqliteStore` có sẵn trong `store/port.rs`; không tạo adapter/ADR trùng. `interactive_launch` 19/19, full PTY 25/25 with `PTY_EXIT: 0`, M2 ten consecutive 10/10, and M6 passed (13 required / 15 discovered, closure M0–M5). H and Linux CI remain pending; status stays `implemented_unverified`.

**Current in-progress / Đang dở:** M6 initially failed twice because the Windows token could still list a directory after `icacls /deny`; `workspace.rs` now preflights that condition and has a deterministic `PermissionDenied` mapping test. `harness-tools --lib` is 20/20 and `Verify-Milestone M6` passed with source digest `sha256:13cfc509eb654436e0f030e97e3a97f0be80bf5a8e10f56ded6ac4659822ee55`. Need commit/push this correction, then run final H and wait for a CI run after the final source commit.

**Changed files and reasons / File đã đổi:** `main.rs`, `interactive/headless.rs`, `interactive/mod.rs`, `g13_headless.rs`, `harness-types/acceptance.rs`, SQLite `models.rs`/`store.rs`/`store/run.rs` cho G13 CLI, NDJSON, acceptance command và migration additive; `harness-tools/contracts.rs`/`process.rs`/`service.rs` cho shell receipt; `workspace.rs` preflight ACL fixture and deterministic permission mapping; `delegation_cli.rs`, `maintenance_cli.rs`, `interactive/controller.rs`/`view.rs`, `service_completion_tests.rs` cho CLI/TUI; `milestone_m2.rs` cho fresh fixture, `milestone_m6.rs` cho G10 selector, `interactive_terminal.rs` và PTY script cho ca G14; `Verify-HaLaunch.ps1`, `ci.yml`, README, SPEC/evidence/handoff, OPERATOR_GUIDE vi/en cho gate/CI/docs. Không thêm Cargo dependency edge hay đổi ba protocol/context constants.

**Latest failures / Lỗi gần nhất:** M6 first two attempts failed `review_unreadable_directory_is_reported_as_a_read_failure_with_the_path`: `icacls` returned success but the current token still enumerated `locked/hidden.txt`. The correction does not change product behavior: it proves the `PermissionDenied` mapping directly, runs the ACL integration assertion only when the denial is effective, and makes the environment limitation explicit. The next M6 run passed. Earlier PTY failures are retained in evidence; final PTY is 25/25.

**Contracts/decisions:** SPEC CP-E ở [SPEC](../specs/HA_AGENT.vi.md). G13 new JSON nested `acceptance.command_id`; legacy chat JSON/exit giữ M3. Runtime schema 6→7 chỉ additive; không migration dữ liệu thật. D1–D11 giữ nguyên. Conditional cargo audit/deny chưa cài/pin, ghi `not_run`; Linux chỉ `via CI` khi job xanh. Không tự ghi acceptance registry.

**Exact next action / Bước kế tiếp:** commit/push ACL fixture correction and this evidence/handoff; run `Verify-HaLaunch.ps1 -Json` on that commit. If H passes, wait for the CI matrix from the final commit and record the green Ubuntu run id as `platform: linux via CI`; otherwise inspect its failing selector only. Dừng ở CP-E, không sang item/checkpoint khác.

**Do not repeat / Không lặp:** baseline H trên `ffc93fe`, compile kiểm `a5ac2e2`, user config/migration/paid calls; `cargo clean -p harness-cli` đã xong và chỉ cần build lại. Không có side effect ngoài temp fixtures/build cache đã biết.

---

## CP-D — G10–G12 historical (24/09/2026)

Tiếp tục từ CP-C và dừng ở CP-D theo assignment. Không bắt đầu G13–G14 trong lượt này.

- G10 nối MCP stdio và Streamable HTTP bearer từ env vào interactive chat; mọi 9 entry hiện trong `McpSupportMatrix` là supported. SSE/OAuth, MRTR multi-round, sampling có tool/image context, và tự mở rộng resource-template URI vẫn ngoài hỗ trợ. `McpClient::call_tool` là `pub(crate)`; N8-MCP là negative control riêng.
- G11 discovery `/skills`, `/skill:<name>`, trusted project skill roots, `list_skills` + digest-pinned `activate_skill` và dynamic Skill-channel context; prompt commands có substitution `$ARGUMENTS`/`$1..$9`; `/reload`.
- G12 dùng WorkerScheduler + WorkerBackend thật với provider/TurnDriver/ToolExecutionService. Explorer chỉ đọc; coder được cấp/persist worktree từ M8-03 nếu repo sạch, chỉnh sửa chỉ ở branch/worktree tách biệt và tool trả lại path/branch. Repo bẩn hoặc M8 manager không tạo được worktree thì lỗi typed `role_unavailable` giải thích điều kiện; không chạy coder trên checkout người dùng.
- Schema error được regenerate cho `role_unavailable`. `EXTENSION_PROTOCOL_VERSION`, `MCP_SPEC_REVISION`, `CONTEXT_SCHEMA_VERSION` không đổi. Không thêm dependency edge; allowlist vẫn là nguồn kiểm soát và không có `harness-runtime → harness-tools` edge mới.
- CP-D verification: **passed** trên Windows, Rust stable 1.97.1. `cargo +stable fmt --all`, `cargo +stable clippy --workspace --all-targets --locked -- -D warnings`, `cargo +stable build --workspace --locked`; `milestone_m6` 14/14 (13 required, 14 discovered; A22/A23/A24 giữ xanh), `phase_p6` 15/15, `phase_p5` 28/28; G12 delegation 7/7, G11 skills 3/3, N8-MCP compile-fail doctest 1/1. `cargo +stable tree -i reqwest --locked` chỉ có `reqwest 0.13.4`.
- `Verify-Milestone.ps1 -Milestone M6` → **passed** trong lần chạy mặc định sau khi sửa các selector cũ: format, clippy, build, workspace tests, dependency allowlist (45 edges), 13 required / 14 discovered, closure M5 15, M4 14, M3 20, M1 11, M2 12, M0 12. Lệnh chạy với `$env:RUSTUP_TOOLCHAIN='stable'` vì rustc của toolchain pin cục bộ thiếu; stable báo Rust 1.97.1. Một lần xác nhận sau khi chỉ cập nhật handoff gặp hai Anthropic fixture test fail trong workspace suite; chạy `harness-providers --lib` riêng pass 32/32. Implementation không đổi sau gate xanh.
- Allowlist không đổi trong CP-D; không thêm edge `harness-runtime → harness-tools`. `EXTENSION_PROTOCOL_VERSION`, `MCP_SPEC_REVISION`, `CONTEXT_SCHEMA_VERSION` giữ nguyên. Gate-pass digest và digest snapshot sau handoff được ghi tại `docs/evidence/M6.vi.md`.

### Next action

Dừng ở CP-D sau G12. Không bắt đầu CP-E (G13–G14) cho đến assignment tiếp theo.

---

## Tiếng Việt

### Phạm vi và quyết định

Hoàn thành source G07–G09 theo `docs/HA_AGENT_PLAN.vi.md`, dừng ở CP-C; không bắt đầu G10. SPEC CP-C đã được user duyệt trước khi code. Giữ D1–D11, ngoại trừ điều chỉnh D6 theo số đo context/reserve đã ghi trong SPEC. Không thêm crate/dependency, không sửa Cargo.lock, không gọi paid/live API, không cài lên máy user. Các luật session, approval fail-closed, protected path, headless không ANSI và exit code được giữ nguyên.

### Trạng thái

Source và test selector đã triển khai. Trạng thái `implemented_unverified`: `Verify-Milestone.ps1 -Milestone M5 -Json` pass (15/15 required, closure M0–M5). Trước upstream merge, H đã hết ngân sách lượt đầu + ba retry mà chưa có `failures: []`. Sau tích hợp lên HEAD `5db5b139e73166412acdb873ec3de355429cd244`, H lượt 1 lỗi providers ở Anthropic Retry-After fixture (31/32), còn launch 19/19 và P0–P7 pass. H lượt 2 lỗi i13 resume timeout sau 190,11 giây (18/19); providers 32/32 và P0–P7 pass. Selector Anthropic retry và i13 đều pass khi chạy riêng. Chưa có gate H nào trả `failures: []`; giữ trạng thái chưa accepted. Không sửa/nới test.

### Kiểm chứng và bằng chứng

- CLI unit: 320 pass, 0 fail, 1 ignored. `interactive_session`: 14/14. `interactive_launch`: 19/19 ở lượt H1 và H4; lượt H2 lỗi i13 resume, lượt H3 lỗi i04, cả hai selector pass khi retry riêng.
- `phase_p2`: 18/18 ở các lần chạy xanh; P0–P7 trên HEAD đã tích hợp: 8/8, 21/21, 18/18, 21/21, 28/28, 27/27, 15/15, 30/30. Providers 32/32 ở H lượt 2; H lượt 1 là 31/32 do Anthropic loopback. i13 selector riêng pass 1/1 trong 1,35 giây sau timeout gate.
- Format, Clippy `-D warnings`, schema generator, installer self-test, release self-test, docs self-test và hai negative controls V23/V25 pass. Gate H cuối vẫn đỏ như trên.
- Chi tiết từng lượt gate, digest source, Cargo.lock, OS/toolchain và `not_run` nằm trong [evidence](../evidence/HA_AGENT.vi.md). Thiết kế/requirement inventory nằm trong [SPEC](../specs/HA_AGENT.vi.md).
- PTY thật, paid/live provider, Linux, user PATH/install, G10+ chưa chạy.

### Repository

CP-C implementation commit `b37b75b` hiện có trong `origin/master`; gate H chạy trên integrated HEAD `5db5b139e73166412acdb873ec3de355429cd244`. Digest source được tính trên snapshot committed `a289f758aadbb9c41ff63c6d7d204603a2250677` (**426 files**, loại evidence và handoff); các commit tiếp theo chỉ cập nhật evidence/handoff nên không đổi digest. Digest `Cargo.lock` tại snapshot: `d24222134934bf555bea489948038d7ceb3719a69dbe212475482240c8b9f27d`. Windows 11 Pro `10.0.26200.0` x64; `rustc 1.97.1 (8bab26f4f 2026-07-14)`, `cargo 1.97.1 (c980f4866 2026-06-30)`. Worktree đang có sửa đổi đồng thời chưa stage ở `Cargo.toml`, `Cargo.lock`, `crates/harness-cli/src/interactive/config.rs`, `crates/harness-extensions/Cargo.toml`, `crates/harness-extensions/src/mcp.rs`, `crates/harness-types/src/contracts.rs`, `crates/harness-types/src/lib.rs`; đã giữ nguyên, không đưa vào commit CP-C. Commit triển khai và evidence/SPEC/handoff song ngữ đã được push.

### Tiếp tục

Giữ phạm vi CP-C. Không có quyền retry loopback thêm trong assignment hiện tại; không mở G10. Nếu tiếp tục sau này, xử lý fixture/runner ổn định hoặc được giao retry budget mới trước khi đánh giá lại CP-C.

## English

### Scope and decisions

Implemented G07–G09 from `docs/HA_AGENT_PLAN.vi.md` and stop at CP-C; do not start G10. The user approved the CP-C SPEC before implementation. D1–D11 remain, except the measured D6 context/reserve adjustment documented in the SPEC. No crate/dependency was added; Cargo.lock is unchanged. No paid/live API call or user installation was made. Session input, fail-closed approval, protected-path ordering, ANSI-free headless output, and exit codes remain intact.

### Status

Source and required selectors are implemented. Status is `implemented_unverified`: `Verify-Milestone.ps1 -Milestone M5 -Json` passed (15/15 required, closure M0–M5). Before upstream integration, the initial H run plus three retries never returned `failures: []`. On integrated HEAD `5db5b139e73166412acdb873ec3de355429cd244`, H run 1 failed the Anthropic Retry-After loopback fixture (providers 31/32) while launch 19/19 and P0–P7 passed. H run 2 failed i13 resume after a 190.11 s timeout (launch 18/19), while providers 32/32 and P0–P7 passed. The Anthropic and i13 selectors passed alone. No whole-gate run returned `failures: []`; do not mark CP-C accepted. No tests were weakened or changed.

### Verification and evidence

- CLI unit: 320 passed, 0 failed, 1 ignored. `interactive_session`: 14/14. On integrated HEAD, `interactive_launch` passed 19/19 on H run 1 and failed i13 on H run 2; isolated i13 passed 1/1 in 1.35 s.
- `phase_p2`: 18/18. P0–P7 on integrated HEAD: 8/8, 21/21, 18/18, 21/21, 28/28, 27/27, 15/15, 30/30. Providers: 32/32 on H run 2; H run 1 was 31/32 due the Anthropic loopback fixture.
- Format, Clippy `-D warnings`, schema generation, installer self-test, release self-test, docs self-test, and V23/V25 negative controls passed. Final H report remains red as noted above.
- Per-run gate details, source digest, Cargo.lock digest, OS/toolchain, and `not_run` are in [evidence](../evidence/HA_AGENT.vi.md). Design and requirement inventory are in the [SPEC](../specs/HA_AGENT.vi.md).
- Real-console PTY, paid/live provider, Linux, user PATH/install, and G10+ were not run.

### Repository

CP-C implementation commit `b37b75b` is already on `origin/master`; the H gate source HEAD was `5db5b139e73166412acdb873ec3de355429cd244`. The source digest uses committed snapshot `a289f758aadbb9c41ff63c6d7d204603a2250677` (426 files; excludes evidence/handoff); subsequent commits only updated evidence/handoff and leave the digest unchanged. Committed `Cargo.lock` SHA-256: `d24222134934bf555bea489948038d7ceb3719a69dbe212475482240c8b9f27d`. Windows 11 Pro `10.0.26200.0` x64; `rustc 1.97.1 (8bab26f4f 2026-07-14)`, `cargo 1.97.1 (c980f4866 2026-06-30)`. The shared worktree has untouched unstaged edits in `Cargo.toml`, `Cargo.lock`, `crates/harness-cli/src/interactive/config.rs`, `crates/harness-extensions/Cargo.toml`, `crates/harness-extensions/src/mcp.rs`, `crates/harness-types/src/contracts.rs`, and `crates/harness-types/src/lib.rs`; those concurrent MCP changes are outside CP-C and not in the committed source digest. The implementation and bilingual evidence/SPEC/handoff are pushed.

### Continuation

Keep the CP-C scope. Do not spend more loopback retries under this assignment and do not start G10. A future continuation needs a stable fixture runner or a newly assigned retry budget before reassessing CP-C.
