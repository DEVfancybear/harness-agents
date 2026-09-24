# Evidence — HA_AGENT CP-E current; CP-A–CP-D archive / Bằng chứng — CP-E hiện tại; lưu trữ CP-A–CP-D

## CP-E working evidence — G13–G14 (24/09/2026)

**Status / Trạng thái:** `implemented_unverified`. Source đang ở worktree chưa commit; chưa có H `passed:true, failures:[]`, M6 gate trên source CP-E, PTY toàn bộ một lượt xanh, hay CI Ubuntu run id. Không chuyển registry acceptance sang `accepted`. / Source remains uncommitted and CP-E is not accepted.

| Field / Trường | Value / Giá trị |
| --- | --- |
| Assignment base | `ffc93fe08b082a53216a26f4f2a5476222dec847` on `master`, initially clean |
| Upstream during work | `febae6ff9dc7d59c58b1127537c68cde7d7a29a1` merged PR #4 and became `origin/master`; CP-E diff remains unstaged |
| Platform/toolchain | Windows NT 10.0.26200.0 x64; `rustc 1.97.1 (8bab26f4f 2026-07-14)` via `RUSTUP_TOOLCHAIN=stable` |
| Cargo.lock SHA-256 | `38315c57cbfb563d37de95c778d52562ac3798f58224fc2517617e9289e7a859` |
| Fixture/scope | `g13_headless` mock and loopback; M2 fresh `FakeProvider` per attempt; PTY temporary projects, no paid/live API or user install |
| StorePort decision | `impl StorePort for SqliteStore` already exists in `crates/harness-store-sqlite/src/store/port.rs`; port is used, so no ADR-N12 removal |

### G14 step 0 — baseline and historical negative control

- On original HEAD `ffc93fe`, before CP-E source edits, `Verify-HaLaunch.ps1 -Json` returned `passed:false`, `failures:["acceptance-launch","providers-streaming"]`. Launch was 18/19 (i04 installed Unicode path), Anthropic provider fixture 31/32; other listed predecessor steps passed. The requested green baseline did **not** exist; no result was reclassified.
- `a5ac2e2` is an ancestor of `origin/master`. Isolated `target/verify-a5ac2e2` worktree ran `cargo check -p harness-cli --locked` and failed with 12 compile errors involving obsolete `GrantReadsForRun`/`approve_reads_for_run` versus `GrantForRun`. `gh run list --commit a5ac2e2` returned no CI runs. The historical commit remains broken; current HEAD is a later revision.

### Commands and observed results / Lệnh và kết quả đã thấy

| Command / Lệnh | Result / Kết quả |
| --- | --- |
| `cargo test -p harness-cli --test g13_headless --locked` | 6/6 passed |
| `cargo test -p harness-cli --test interactive_launch --locked i03` | 5/5 passed |
| `cargo test -p harness-cli --test milestone_m3 --locked` | 20/20 passed after preserving legacy headless goal-stop exit behavior |
| `cargo test -p harness-cli --test milestone_m2 --locked -- --test-threads=1` | First run 10/10; required ten consecutive runs pending |
| `cargo test -p harness-cli --test milestone_m6 --locked g10_mcp_tool_goes_through_the_dispatcher_and_the_approval_gate -- --exact` | 1/1 passed; A23's approved/denied dispatcher walk remains intact |
| `cargo test -p harness-cli --bin ha --locked interactive::controller::tests::k01_inline_key_uses_the_full_remainder_and_is_not_recallable -- --exact` | 1/1 passed, including leading/trailing spaces |
| `Verify-HaLaunch.ps1 -SelfTest` | `GATE_SELFTEST_OK` after eight G selectors and unit-agent step |
| `Verify-Docs.ps1 -SelfTest` | `DOCS_OK`; 174 Markdown files, 15 language pairs, 12 named negative controls |
| PTY full run 1 | 18/25; new fixture races and old i13/i14/t01 instability recorded in `target/pty-acceptance-cpe/pty-all.*` |
| PTY full run 2 | 21/25; g06 loopback, g08 premature approval input, i01 trailing terminal escape, t_more frame assertion; `target/pty-acceptance-cpe2/pty-all.*` |
| PTY run 3 | Linker exit 1140 before tests because C: had ~54 MiB free. Recursive cache delete was rejected by automatic approval review; `cargo clean -p harness-cli` completed, removing 196598 generated files (148.7 GiB). This did not touch source, user config or runtime databases. |

**Negative controls / Đối chứng âm:** G13 stdin literal dash/overflow and no ANSI, exit 3 for question, M3 legacy compatibility, M6 MCP denied path with no server log; docs self-test deliberately rejects missing translation/link/case and other invalid inputs. Historical `a5ac2e2` compile failure is a separate negative control, not an H gate result.

**Not run / Chưa chạy:** paid/live smoke, clean VM, user PATH/install, acceptance registry promotion; `cargo audit`/`cargo deny` absent from PATH and no pinned install/version is recorded in this SPEC, so conditional CI checks are not run. CI Ubuntu Linux evidence is pending; platform must be labelled `linux via CI` only after a green run id. Source digest, final gate logs and PTY transcript hashes will be recorded after CP-E verification, not invented here.

---

**Historical CP-C status / Trạng thái CP-C lịch sử:** `implemented_unverified` at CP-C. G07–G09 source and selectors were implemented. M5 passed 15/15. No whole H gate returned `failures: []` in that checkpoint. / CP-C là hồ sơ cũ; xem phần bên dưới để biết kết quả từng lượt.

SPEC: [HA_AGENT.vi.md](../specs/HA_AGENT.vi.md) · Plan: [HA_AGENT_PLAN.vi.md](../HA_AGENT_PLAN.vi.md) · Assignment handoff: [HA_AGENT.vi.md](../handoffs/HA_AGENT.vi.md)

## Current checkpoint CP-C / Checkpoint hiện tại CP-C

### Source snapshot / Snapshot source

| Field / Trường | Value / Giá trị |
|---|---|
| Branch and base / Branch và commit gốc | `master`, CP-C implementation `b37b75b`; integrated HEAD `5db5b139e73166412acdb873ec3de355429cd244` (M0–M6 and TUI changes merged during the assignment). / CP-C implementation `b37b75b`; HEAD đã tích hợp `5db5b139e73166412acdb873ec3de355429cd244` (đã merge thay đổi M0–M6 và TUI trong lúc làm assignment). |
| Implementation commit / Commit triển khai | `b37b75b feat: implement HA_AGENT CP-C G07-G09`, reachable from `origin/master`. / Commit triển khai `b37b75b`, đã có trong `origin/master`. |
| Source digest / Digest source | `sha256:85eff4e2b825ea93957c9fb58ba83da0f59ac362f8ab575ca40464377e59a231`; **426 files**. Snapshot is committed HEAD `a289f758aadbb9c41ff63c6d7d204603a2250677` after the bilingual SPEC closeout. SHA-256 over sorted tracked repo-relative paths (UTF-8 + NUL), committed file bytes + NUL; excludes only this evidence and `docs/handoffs/HA_AGENT.vi.md`. / **426 file**. Snapshot là HEAD `a289f758aadbb9c41ff63c6d7d204603a2250677` đã commit sau khi chốt SPEC song ngữ. SHA-256 trên path tracked đã sắp xếp (UTF-8 + NUL), bytes file đã commit + NUL; chỉ loại evidence này và `docs/handoffs/HA_AGENT.vi.md`. |
| `Cargo.lock` SHA-256 | `d24222134934bf555bea489948038d7ceb3719a69dbe212475482240c8b9f27d` at committed HEAD. Current worktree has a separate unstaged edit, preserved and not part of CP-C. / Digest tại HEAD đã commit. Worktree hiện có sửa đổi chưa stage riêng, được giữ nguyên và không thuộc CP-C. |
| OS / Hệ điều hành | Windows 11 Pro, `10.0.26200.0`, x64 |
| Toolchain | `rustc 1.97.1 (8bab26f4f 2026-07-14)`, `cargo 1.97.1 (c980f4866 2026-06-30)`, isolated `RUSTUP_HOME=C:\Users\duong\.rustup-ha-agent-2026-09-23`; commands used `--locked`. / `rustc 1.97.1`, `cargo 1.97.1`, dùng Rustup home riêng; lệnh Cargo có `--locked`. |
| Test configuration / Cấu hình test | `RUST_TEST_THREADS=1` for milestone and launch gates; no paid/live provider calls. / Dùng `RUST_TEST_THREADS=1` cho gate milestone và launch; không gọi provider live/trả phí. |

### Gate ledger / Nhật ký gate

| Run / Lượt | Result / Kết quả |
|---|---|
| `Verify-Milestone.ps1 -Milestone M5 -Json` | `passed`; closure M0–M5; required selectors 15/15; workspace tests, format, Clippy, build and dependency allowlist passed. A known host flake at `p2_s02_provider_streams_and_deepseek_sse_adapter_are_normalized` retried once inside the verifier, then closure M2 passed (12/12). / `passed`; closure M0–M5; selector bắt buộc 15/15; workspace, format, Clippy, build và dependency allowlist pass. Host flake đã biết ở `p2_s02_provider_streams_and_deepseek_sse_adapter_are_normalized` được verifier retry một lần rồi closure M2 pass (12/12). |
| Verify-HaLaunch attempt 1 / Lượt 1 | `failures=[providers-streaming, regression-phase_p2]`; unit 320 pass/1 ignored; launch 19/19; session 14/14; P0–P1 and P3–P7 passed. Providers 30/32; visible failing selector included `g03_openai_chat_adapter_keeps_m2_wire_format` (the verifier retained only the last three log lines, so the other failed selector is not identified). P2 17/18 (`p2_s02_provider_streams_and_deepseek_sse_adapter_are_normalized`). Installer, release and docs self-tests passed. / `failures=[providers-streaming, regression-phase_p2]`; unit 320 pass/1 ignored; launch 19/19; session 14/14; P0–P1 và P3–P7 pass. Providers 30/32; log còn thấy `g03_openai_chat_adapter_keeps_m2_wire_format` (verifier chỉ giữ ba dòng cuối nên không nhận diện được selector fail còn lại). P2 17/18 (`p2_s02_provider_streams_and_deepseek_sse_adapter_are_normalized`). Installer, release và docs self-test pass. |
| Verify-HaLaunch attempt 2 / Lượt 2 | `failures=[acceptance-launch, regression-phase_p2]`; launch 18/19, i13 resume timed out after 225 s; session 14/14; providers 32/32; P0–P1 and P3–P7 passed; P2 17/18 (`p2_s02`). Installer, release and docs passed. / `failures=[acceptance-launch, regression-phase_p2]`; launch 18/19, i13 resume timeout sau 225 giây; session 14/14; providers 32/32; P0–P1 và P3–P7 pass; P2 17/18 (`p2_s02`). Installer, release và docs pass. |
| Verify-HaLaunch attempt 3 / Lượt 3 | `failures=[acceptance-launch]`; launch 18/19, i04 Unicode install/caller-directory test failed after 43 s; session 14/14; providers 32/32; P0–P7, installer, release and docs passed. / `failures=[acceptance-launch]`; launch 18/19, test i04 đường dẫn Unicode fail sau 43 giây; session 14/14; providers 32/32; P0–P7, installer, release và docs pass. |
| Verify-HaLaunch attempt 4 / Lượt 4 | Final allowed retry: `failures=[providers-streaming, regression-phase_p2]`; launch 19/19; session 14/14; P0–P1 and P3–P7 passed. Providers 30/32 (visible failing selector again included G03 OpenAI wire snapshot); P2 17/18 (`p2_s02`). Installer, release and docs passed. / Lượt retry cuối: `failures=[providers-streaming, regression-phase_p2]`; launch 19/19; session 14/14; P0–P1 và P3–P7 pass. Providers 30/32 (log lại hiện selector G03 OpenAI wire snapshot); P2 17/18 (`p2_s02`). Installer, release và docs pass. |
| Integrated HEAD `5db5b13`, H run 1 / HEAD tích hợp, H lượt 1 | `failures=[providers-streaming]`; launch 19/19, session 14/14, P0–P7 passed, unit 320/0/1 ignored. Provider suite 31/32, visible failure `g03_anthropic_429_http_date_is_bounded`; all other H stages (format, Clippy, installer, release, docs) passed. / `failures=[providers-streaming]`; launch 19/19, session 14/14, P0–P7 pass, unit 320/0/1 ignored. Providers 31/32, selector lỗi hiện trong log là `g03_anthropic_429_http_date_is_bounded`; các stage H còn lại (format, Clippy, installer, release, docs) pass. |
| Integrated HEAD `5db5b13`, H run 2 / HEAD tích hợp, H lượt 2 | Latest: `failures=[acceptance-launch]`; i13 resume timed out after 190.11 s (18/19 launch). Session 14/14, providers 32/32, P0–P7 passed, unit 320/0/1 ignored, installer/release/docs passed. / Mới nhất: `failures=[acceptance-launch]`; i13 resume timeout sau 190,11 giây (launch 18/19). Session 14/14, providers 32/32, P0–P7 pass, unit 320/0/1 ignored, installer/release/docs pass. |

### Focused reruns and controls / Chạy riêng và kiểm tra đối chứng

- After H attempt 1, `cargo test -p harness-providers --locked -- --test-threads=1`: **32/32 passed**; `g03_openai_chat_adapter_keeps_m2_wire_format`: **1/1 passed**. `cargo test -p harness-cli --test phase_p2 --locked -- --test-threads=1`: **18/18 passed**; exact `p2_s02...` selector: **1/1 passed**. After H attempt 3, exact i04 selector: **1/1 passed**. / Sau H lượt 1, providers pass **32/32**; G03 snapshot pass **1/1**. `phase_p2` pass **18/18**; selector `p2_s02...` pass **1/1**. Sau H lượt 3, selector i04 pass **1/1**.
- On integrated HEAD, isolated `g03_anthropic_429_http_date_is_bounded`: **1/1 passed**; the full providers suite immediately after failed **31/32**, with `retry_after=None` (fixture did not expose the expected 429 in that run). Isolated `i13_resume_continues_the_task_with_recovered_context_and_no_rerun`: **1/1 passed** in 1.35 s after the latest H failure. / Trên HEAD tích hợp, selector Anthropic 429 chạy riêng pass **1/1**; ngay sau đó full providers fail **31/32**, `retry_after=None` (lượt đó fixture không trả được 429 như mong đợi). Selector i13 chạy riêng pass **1/1** trong 1,35 giây sau lỗi H mới nhất.
- `cargo test -p harness-cli --bin ha --locked -- --test-threads=1`: **320 passed, 0 failed, 1 ignored** (also reported by each H run). `milestone_m5`: **15/15**; `phase_p2`: **18/18** on clean runs; `interactive_launch`: **19/19** on H attempts 1 and 4; `interactive_session`: **14/14**. P0–P7 counts: 8, 21, 18, 21, 26, 27, 15, 15. / CLI: **320 pass, 0 fail, 1 ignored**; M5 **15/15**; P2 **18/18** ở lượt xanh; launch **19/19** ở H lượt 1 và 4; session **14/14**. Số test P0–P7: 8, 21, 18, 21, 26, 27, 15, 15.
- `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --locked -- -D warnings`: passed in every H run. `cargo run -p harness-types --bin generate_schemas --locked` regenerated schemas. CP-C added no dependency; the committed lockfile digest is shown above (M0–M6 integration had already changed the lockfile relative to the CP-B base). / Format và Clippy pass ở mọi H run. Đã chạy schema generator. CP-C không thêm dependency; digest lockfile đã commit nằm ở trên (M0–M6 đã thay lockfile so với base CP-B).
- Negative controls: removing the `/undo` hash guard made V23 red; restoring it returned that selector to green. Treating hook stdout `allow` as policy permission made V25 red; restoring the fail-closed path returned its selector to green. / Đối chứng âm: bỏ hash guard `/undo` làm V23 đỏ; phục hồi guard thì selector xanh. Coi stdout `allow` là quyền làm V25 đỏ; khôi phục fail-closed thì selector xanh.
- Inference, not a gate verdict: the failures move between loopback-backed tests across otherwise identical runs, and each named failed selector passed when isolated. This indicates host/fixture flakiness; it does not turn the final gate result into a pass. / Suy luận, không phải kết quả gate: lỗi chuyển giữa các test dùng loopback trong những lượt chạy tương tự; từng selector lỗi có tên đều pass khi chạy riêng. Điều này cho thấy host/fixture không ổn định; không biến kết quả gate cuối thành pass.

### Not run and preserved concurrent edits / Chưa chạy và sửa đổi đồng thời được giữ lại

Paid/live provider calls; Linux build/run; `scripts/Invoke-HaPtyAcceptance.ps1` (not assigned for CP-C; gate reports PTY as not run); real user install/PATH mutation; G10 and later. CP-C test fixtures use only local loopback and temporary directories. The current shared worktree also contains untouched, unstaged edits to `Cargo.toml`, `Cargo.lock`, `crates/harness-cli/src/interactive/config.rs`, `crates/harness-extensions/Cargo.toml`, `crates/harness-extensions/src/mcp.rs`, `crates/harness-types/src/contracts.rs`, and `crates/harness-types/src/lib.rs`; these concurrent MCP configuration changes are outside CP-C and excluded from the source digest. / Chưa gọi provider trả phí/live; chưa build/run Linux; chưa chạy `scripts/Invoke-HaPtyAcceptance.ps1` (ngoài yêu cầu CP-C; gate ghi PTY chưa chạy); chưa cài/PATH thật; chưa làm G10 trở đi. Fixture CP-C chỉ dùng loopback cục bộ và thư mục tạm. Worktree dùng chung hiện có các sửa đổi chưa stage, không đụng tới ở `Cargo.toml`, `Cargo.lock`, `crates/harness-cli/src/interactive/config.rs`, `crates/harness-extensions/Cargo.toml`, `crates/harness-extensions/src/mcp.rs`, `crates/harness-types/src/contracts.rs` và `crates/harness-types/src/lib.rs`; đây là thay đổi MCP đồng thời, ngoài CP-C và không nằm trong digest source.

## Historical checkpoint CP-B / Hồ sơ cũ CP-B

### Source snapshot / Snapshot source

| Field / Trường | Value / Giá trị |
|---|---|
| Branch and implementation commit / Branch và commit implementation | `master`, `bb03559f42b5d5efd9b7009ad9c651e4ad8f0ef1` |
| Worktree / Worktree | CP-B implementation is committed; this bilingual evidence/handoff update is the documentation follow-up commit. Both are pushed to `origin/master`. / Implementation CP-B đã commit; bản cập nhật evidence/handoff song ngữ này là commit tài liệu tiếp nối. Cả hai đã được push lên `origin/master`. |
| Source digest / Digest source | `sha256:7b59aa4dfedbcc26d92dac100b22ac046020d17517001e0aedb58497ef817d00`; **420 files**. Hash is SHA-256 over each sorted repo-relative path encoded UTF-8 + NUL, file bytes, then NUL, for tracked and non-ignored untracked files; excludes only this evidence and docs/handoffs/HA_AGENT.vi.md. / **420 file**; SHA-256 trên path repo-relative đã sắp xếp (UTF-8 + NUL), bytes file rồi NUL; gồm file tracked và untracked không ignore; chỉ loại evidence này và docs/handoffs/HA_AGENT.vi.md. |
| `Cargo.lock` SHA-256 | `193d2574da4a994777bf8961780e264a123c1397d04d7e66f7bf49d33d919876` |
| OS / Hệ điều hành | Windows 11 Pro, `10.0.26200.0`, x64 |
| Toolchain | `rustc 1.97.1 (8bab26f4f 2026-07-14)` with `RUSTUP_HOME=C:\Users\duong\.rustup-ha-agent-2026-09-23`; commands used `--locked`. / Chọn toolchain qua Rustup home riêng; lệnh Cargo dùng `--locked`. |
| Commit/push / Ghi commit/push | Implementation: `bb03559f42b5d5efd9b7009ad9c651e4ad8f0ef1`; this evidence/handoff follow-up is pushed with it to `origin/master`. / Implementation: `bb03559f42b5d5efd9b7009ad9c651e4ad8f0ef1`; bản evidence/handoff tiếp nối được push cùng lên `origin/master`. |

G05 is present in the single existing `ToolPolicy`: modes, ordered deny/allow/mode decisions, confirmed persistent rule save, `/permissions`, `/mode`, audit transcript entries, and fail-closed headless defaults all have tests. This continuation did not find a missing G05 feature. / G05 hiện diện trong `ToolPolicy` duy nhất: mode, thứ tự deny/allow/mode, lưu rule dài hạn sau xác nhận, `/permissions`, `/mode`, audit transcript và headless fail-closed đều có test. Lượt này không phát hiện thiếu tính năng G05.

### CP-B command ledger / Nhật ký lệnh CP-B

| Command / Lệnh | Result / Kết quả |
|---|---|
| `cargo fmt --all` and `cargo fmt --all -- --check` | Pass / Đạt |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Pass, independently and in Verify-HaLaunch / Đạt riêng lẻ và trong Verify-HaLaunch |
| `cargo test -p harness-cli --bin ha --locked` | Attempt 1 **297 passed / 1 failed / 1 ignored** (`completion_service_resume_flow` loopback child timed out); attempt 2 **298 passed / 0 failed / 1 ignored**. / Lượt 1 **297 đạt / 1 lỗi / 1 ignored** (loopback child `completion_service_resume_flow` timeout); lượt 2 **298/0/1 ignored**. |
| Updated H03/H05/T06 unit tests | **1/1 each**; running input queues and dispatches after terminal, stale `y` never answers an expired gate, and panel distinguishes turn grant from persistent-rule confirmation. / Mỗi test **1/1**; queue input, `y` cũ không trả lời gate hết hạn, panel phân biệt grant lượt với xác nhận rule dài hạn. |
| G05 targeted selectors | CLI **9/9**; `harness-tools` **5/5**. / CLI **9/9**; `harness-tools` **5/5**. |
| `milestone_m4`, `phase_p3`, `interactive_session` (serial) | **23/23**, **21/21**, **14/14**. / **23/23**, **21/21**, **14/14**. |
| `interactive_terminal` normal cargo test | **0 passed, 0 failed, 18 ignored** because the cases require a real console. / **0 đạt, 0 lỗi, 18 ignored** vì cần console thật. |
| Real-console PTY selectors through `scripts/Invoke-HaPtyAcceptance.ps1` | `h05` attempt 1 failed on an incomplete loopback fixture read; attempt 2 **1/1** after fixture now reads Content-Length. `i05`, `i06`, `i07a`, `i07b`: **1/1 each**. Transcript files are under `target/pty-acceptance-cpb/`. / `h05` lượt 1 lỗi fixture đọc thiếu; lượt 2 **1/1** sau khi fixture đọc đủ Content-Length. `i05`, `i06`, `i07a`, `i07b`: mỗi case **1/1**. Transcript nằm trong `target/pty-acceptance-cpb/`. |
| Schema generator / Sinh schema | `cargo run -p harness-types --bin generate_schemas --locked`; `p0_f03` **1/1** and P0 **8/8** after generation. It rewrote `error-report.v1.schema.json` and `tool-execution-receipt.v1.schema.json`; no JSON was edited manually. / Generator đã tạo lại hai file trên; `p0_f03` **1/1**, P0 **8/8**; không sửa JSON bằng tay. |

Verify-HaLaunch attempt ledger: attempt 1 failed `regression-phase_p0` because generated receipt schema was stale while `acceptance-launch` was **19/19**. Attempt 2 failed `acceptance-launch` **18/19**, selector `i03_a_named_file_reaches_the_model_inside_the_message`; the other steps passed, including P0–P7. Attempt 3 failed `acceptance-launch` **18/19**, selector `i13_resume_continues_the_task_with_recovered_context_and_no_rerun`; the other steps passed, including P0–P7. The final report therefore has `failures: ["acceptance-launch"]`; retry budget is exhausted and neither test was edited or rerun separately. / Nhật ký Verify-HaLaunch: lượt 1 lỗi `regression-phase_p0` vì schema receipt chưa sinh lại, trong khi `acceptance-launch` **19/19**. Lượt 2 lỗi `acceptance-launch` **18/19**, selector `i03_a_named_file_reaches_the_model_inside_the_message`; các bước khác kể cả P0–P7 đều đạt. Lượt 3 lỗi `acceptance-launch` **18/19**, selector `i13_resume_continues_the_task_with_recovered_context_and_no_rerun`; các bước còn lại kể cả P0–P7 đều đạt. Vì vậy report cuối có `failures: ["acceptance-launch"]`; đã hết retry budget, không sửa hoặc chạy riêng lại hai test.

**Acceptance / Nghiệm thu:** CP-B remains `implemented_unverified`. No live/paid API, Linux verification, user installation/PATH mutation, manual database migration, or user/shared-project `config.local.toml` write was performed; G05 persistence tests wrote only inside disposable temp roots. G07+ was not started. / CP-B vẫn `implemented_unverified`. Không gọi API live/trả phí, kiểm chứng Linux, cài/PATH user, migration DB thủ công hoặc ghi `config.local.toml` của project user/workspace dùng chung; test G05 chỉ ghi trong thư mục tạm có thể xóa. Chưa bắt đầu G07 trở đi.

## Historical CP-A evidence / Evidence CP-A lưu trữ

The following sections preserve CP-A measurements as historical evidence; they do not describe the current CP-B state. / Các phần tiếp theo lưu số đo CP-A trong lịch sử, không mô tả trạng thái CP-B hiện tại.

### 1. Tested source / Source đã kiểm tra

| Field / Trường | Value / Giá trị |
|---|---|
| Assignment baseline / Revision gốc | `37e355dc2cc22a381c7e86ae3231b14c0c354be0` |
| HA_AGENT implementation commit / Commit triển khai HA_AGENT | `0b6d19da17a3387ddbe46daca91ab5f81e66f9d3` (`master`; pushed to `origin/master`) |
| Working-tree source digest / Digest source working tree | `sha256:b9fc76caf88fff2f8eba2a229f7082d3ff10c91104f894e46952b52a95b3a137` |
| Files hashed / Số file | **419**; includes the bilingual SPEC and generated config schema; excludes only this evidence and the assignment handoff |
| `Cargo.lock` SHA-256 | `9a778b3745b5ae8e1c954ad1128abb12c9807e1148b7237653a9c1be61a825a6` |
| OS / architecture | Windows `10.0.26200.0`, x64 |
| Toolchain | `rustc 1.97.1 (8bab26f4f 2026-07-14)`; commands used `--locked` |
| Clippy/toolchain | `cargo clippy` passed under `RUSTUP_HOME=C:\Users\duong\.rustup-ha-agent-2026-09-23`, Rust `1.97.1`. The default Rustup home was inconsistent: it reported Clippy installed while the driver was absent; a repair attempt met a locked `rustc_driver` DLL used by Rust Analyzer and left the default pinned toolchain without `rustc.exe`/`cargo.exe`. No repository files were changed by this toolchain repair. |

HEAD moved from `37f4035` to `5498f74` while the gates were running because another M7–M12 audit commit landed in the shared workspace. That commit was preserved. The reviewed HA_AGENT implementation was committed as `0b6d19d` (40 files) and pushed to `origin/master`; this evidence/handoff update is a separate documentation follow-up. / HEAD đổi từ `37f4035` sang `5498f74` trong lúc chạy gate do commit audit M7–M12 khác được ghi vào workspace chung. Commit đó được giữ nguyên. Implementation HA_AGENT đã review được commit thành `0b6d19d` (40 file) và push lên `origin/master`; evidence/handoff này được cập nhật riêng sau đó.

### 2. Commands and measured results / Lệnh và kết quả đo được

| Command / Lệnh | Result / Kết quả |
|---|---|
| `cargo fmt --all -- --check` | Pass / Đạt |
| `cargo test -p harness-cli --bin ha --locked` | **278 passed, 0 failed, 1 ignored** (manual clipboard test). Latest exact run passed. / Lần chạy exact cuối đạt **278/0/1 ignored** (clipboard cần thao tác tay). |
| `cargo test -p harness-cli --bin ha --locked g01_` | **5/5** |
| `cargo test -p harness-cli --bin ha --locked g02_` | **5/5** |
| `cargo test -p harness-cli --bin ha --locked g03_` | **3/3** (CLI-side model, cost, and thinking tests) |
| `cargo test -p harness-store-sqlite --locked g03_model_switch_applies_next_turn_only` | **1/1**; persisted setting applies from the next turn and survives reopen. / Cài đặt model áp dụng từ lượt kế tiếp và còn sau reopen. |
| `cargo test -p harness-cli --test milestone_m2 --locked -- --test-threads=1` | **10/10** |
| `cargo test -p harness-cli --test interactive_session --locked -- --test-threads=1` | **14/14** |
| `cargo test -p harness-cli --test interactive_launch --locked -- --test-threads=1` | **19/19** |
| `cargo test -p harness-cli --test phase_p0` through `phase_p7` (Verify-HaLaunch attempt 3, serial) | **8, 21, 17, 21, 26, 27, 15, 15** passed respectively. / Lần lượt đạt **8, 21, 17, 21, 26, 27, 15, 15**. |
| `cargo test -p harness-cli --test phase_p0 --locked p0_f03_contract_schemas_are_generated_from_real_types` | **1/1**; schema was generated with `cargo run -p harness-types --bin generate_schemas --locked`, not edited by hand. / Schema được sinh bằng generator, không sửa tay. |
| `cargo test -p harness-cli --test milestone_m6 --locked -- --test-threads=1` | **13/13** (ConfigExplain and adjacent M6 checks). / **13/13**. |
| `pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1 -TimeoutSeconds 900` | Final run on the current tree: **17/17**. Transcript: `target/pty-acceptance/pty-all.txt` (ignored build output). / Lượt cuối trên cây hiện tại đạt **17/17**. |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | **Pass** with the isolated Rust 1.97.1 toolchain at `C:\Users\duong\.rustup-ha-agent-2026-09-23`. / **Đạt** với toolchain Rust 1.97.1 cô lập tại đường dẫn trên. |
| `pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json` | Attempt 4: **failed only at `providers-streaming`**. Format, Clippy, discovery, `ha` **278 passed / 0 failed / 1 ignored**, `interactive_launch` **19/19**, `interactive_session` **14/14**, P0–P7 **8/21/17/21/26/27/15/15**, installer/release self-tests, and docs self-test passed. Provider suite was **31 passed / 1 failed**: `anthropic::tests::g03_anthropic_429_http_date_is_bounded`. The verifier retains only the last three output lines, so the assertion detail was not captured. / Lượt 4 chỉ thất bại ở `providers-streaming`; các gate còn lại đạt như số liệu bên trái. Provider suite **31 đạt / 1 lỗi** tại test HTTP-date nêu trên; verifier chỉ giữ ba dòng cuối nên không lưu chi tiết assertion. |

### 3. Flake and regression log / Nhật ký flake và regression

- **I08 exit-code regression:** the PTY test first observed exit 4 because terminal I/O used `StorageWriteFailed`; terminal failures are specified to keep generic exit 1. The terminal paths now use the existing exit-1 class while real storage write errors remain exit 4. The I08 selector and final full PTY suite passed. The test was not changed. / **Regression I08:** lần đầu test PTY thấy exit 4 do lỗi terminal bị phân loại thành `StorageWriteFailed`; đặc tả yêu cầu exit 1. Đường lỗi terminal nay dùng nhóm exit 1 hiện có, lỗi ghi storage thật vẫn exit 4. Selector I08 và PTY suite cuối đều đạt; không sửa test.
- **Unit tests / Windows ACL:** one parallel `ha` run had three credential-file tests fail with `Access is denied` while writing separate temp directories. The serial full unit run and two later exact default runs passed (**278/0/1 ignored**). This transient was recorded; no assertion was removed or weakened. / Một lượt unit chạy song song có ba test credential tạm lỗi ACL; cả lượt serial và hai lượt exact sau đều đạt.
- **G03 Anthropic loopback:** one filtered provider run was 3/4; the truncated-stream assertion received `service_unavailable: provider request failed: error sending request` before reaching the decoder contract. Two isolated diagnostic runs reproduced it. A complete provider suite in Verify-HaLaunch attempt 3 passed on the unchanged fixture. A temporary fixture transport edit was tried and two 4/4 runs passed, but it was reverted; those passes are excluded. The final Verify-HaLaunch attempt 4 then had 31/32 provider tests pass and failed `g03_anthropic_429_http_date_is_bounded`; the verifier did not preserve the assertion detail. No test was edited and no further loopback rerun was made after the retry limit. / Một lượt lọc provider đạt 3/4; test stream bị cắt nhận `service_unavailable: provider request failed: error sending request` trước khi tới decoder. Hai lượt cô lập cũng tái hiện lỗi. Provider suite đầy đủ ở Verify-HaLaunch lượt 3 đạt với fixture nguyên trạng. Từng thử chỉnh transport fixture tạm thời và có hai lượt 4/4 đạt, nhưng đã hoàn tác; không tính hai lượt đó. Verify-HaLaunch cuối lượt 4 có 31/32 provider test đạt, thất bại tại `g03_anthropic_429_http_date_is_bounded`; verifier không giữ assertion detail. Không sửa test và không chạy thêm loopback sau khi hết giới hạn retry.
- **PTY I05 loopback:** one full PTY run was 16/17 because the follow-up local provider request returned `error sending request`; the I05 selector then passed 1/1, and the next full PTY run passed 17/17. The test stayed unchanged. / Một lượt PTY đạt 16/17 vì request provider loopback ở bước follow-up lỗi; selector I05 đạt 1/1 và full PTY kế tiếp đạt 17/17. Không sửa test.
- Verify-HaLaunch attempts 1–2 exposed stale assertions after config/store contract changes plus a transient phase-P2 failure; those assertions were aligned to the implemented contracts without weakening them, and P2 passed in isolation and later gates. Attempt 3 failed only at Clippy startup; attempt 4 passed Clippy and every step except `providers-streaming`. / Hai lượt Verify đầu phát hiện assertion cũ sau thay đổi contract config/store và một lỗi P2 thoáng qua; assertion được sửa theo contract, không nới, P2 đạt ở lượt cô lập và các gate sau. Lượt 3 chỉ lỗi khởi chạy Clippy; lượt 4 đạt Clippy và mọi bước trừ `providers-streaming`.

### 4. Scope, dependencies, and not run / Phạm vi, dependency và chưa chạy

- G01–G03 only. No G04+, no new crate or product binary, and DeepSeek remains a separate preset on the generic OpenAI Chat adapter. `/init` prints a static sample and writes nothing. / Chỉ G01–G03; không thêm crate/binary sản phẩm; DeepSeek là preset riêng trên adapter OpenAI Chat tổng quát; `/init` chỉ in mẫu tĩnh.
- `chrono =0.4.45` (MIT OR Apache-2.0) is an exact workspace pin used for HTTP-date parsing; `reqwest =0.12.24` (MIT OR Apache-2.0) remains the transport. No new crate was added. Both are listed in the SPEC. / `chrono =0.4.45` (MIT OR Apache-2.0) là pin exact dùng parse HTTP-date; `reqwest =0.12.24` (MIT OR Apache-2.0) dùng transport; không thêm crate.
- No paid/live API call; no `config.local.toml` write; no HA user app install. The user authorized installing Clippy and committing/pushing this assignment; implementation commit `0b6d19d` is on `origin/master`. The additive SQLite migration was exercised by its tests; do not manually rerun or reset user data. / Không gọi API trả phí, không ghi `config.local.toml`, không cài HA lên máy user. User đã cho phép cài Clippy và commit/push assignment này; commit implementation `0b6d19d` đã có trên `origin/master`. Migration SQLite additive đã được test; không chạy migration thủ công hoặc reset dữ liệu user.
- **Not run / Chưa chạy:** Linux verification; paid provider smoke; real HA install/PATH mutation; repairing the default Rustup home while Rust Analyzer still holds the pinned toolchain DLL. / Chưa chạy xác minh Linux, smoke provider trả phí, cài HA/PATH thật; chưa phục hồi Rustup home mặc định khi Rust Analyzer còn giữ DLL của toolchain.
- `pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest` after saving both metadata files: passed; **170 Markdown files**, 15 language pairs, all structural negative controls passed. / Chạy sau khi lưu hai file metadata: đạt; **170 file Markdown**, 15 cặp ngôn ngữ, toàn bộ negative control cấu trúc đạt.

**Acceptance conclusion / Kết luận nghiệm thu:** implementation remains `implemented_unverified`; Clippy is green on the isolated exact toolchain, but the final HA launch gate is red on one G03 loopback test. The loopback retry budget is exhausted, so CP-A is not marked green. / Phần triển khai vẫn `implemented_unverified`; Clippy xanh với toolchain chính xác được cô lập, nhưng gate HA cuối đỏ ở một test G03 loopback. Đã hết lượt retry loopback nên chưa chốt CP-A xanh.
