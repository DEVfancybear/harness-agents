# Handoff — HA_AGENT CP-B (G04–G06) / Bàn giao — HA_AGENT CP-B (G04–G06)

## English

### Assignment and constraints

Continue the HA_AGENT assignment only through G06 in docs/HA_AGENT_PLAN.vi.md. Stop at CP-B; do not begin G07. Preserve one admitted input per session, fail-closed approval, protected-path rejection before the panel, no ANSI in headless output, and existing exit codes. Use the root workspace and single ha binary. No new crate, second engine, paid API, user install, manual migration, or user/shared-project config.local.toml write. G05 persistence tests write only under disposable temporary roots. The user authorized committing and pushing CP-B. Implementation commit `bb03559f42b5d5efd9b7009ad9c651e4ad8f0ef1` and this metadata follow-up are pushed to `origin/master`.

### Current status

G04, G05, and G06 source work is implemented. G05 was not missing: PolicyMode and pattern rules are implemented inside the existing ToolPolicy, with protected → deny → allow → mode → panel order; persistent A saves only after explicit Enter confirmation; /permissions, /mode, headless flags, and per-action transcript audit are present and tested.

CP-B is implemented_unverified because the final Verify-HaLaunch gate still has an acceptance-launch loopback failure. The launch suite failures were inconsistent across runs, and the bounded retry budget is exhausted. Do not report CP-B accepted.

### Source and evidence

- Branch master; CP-B implementation commit: bb03559f42b5d5efd9b7009ad9c651e4ad8f0ef1.
- CP-B implementation, tests, and SPEC are committed. This bilingual evidence/handoff update is a documentation follow-up; both commits are pushed to origin/master.
- Source digest: sha256:7b59aa4dfedbcc26d92dac100b22ac046020d17517001e0aedb58497ef817d00 (420 tracked and non-ignored untracked files; excludes only the CP-B evidence and this handoff). Cargo.lock SHA-256: 193d2574da4a994777bf8961780e264a123c1397d04d7e66f7bf49d33d919876.
- The generated schema files were regenerated with cargo run -p harness-types --bin generate_schemas --locked. Do not edit them by hand.
- Current SPEC: docs/specs/HA_AGENT.vi.md; evidence: docs/evidence/HA_AGENT.vi.md.
- Real-console PTY transcripts are in target/pty-acceptance-cpb, including h05 attempt 2 and i05/i06/i07a/i07b.
- OS: Windows 11 Pro 10.0.26200.0 x64. Toolchain: rustc 1.97.1 (8bab26f4f 2026-07-14), selected using C:\Users\duong\.rustup-ha-agent-2026-09-23.

### Work completed

- G04: additive write_file/edit_file/glob/search_text/read_file features, typed edit errors, central workspace validation, before/after hashes and bounded preimage artifacts, unified approval diff, TestBackend coverage, and tools schema 2→3.
- G05: policy modes and patterns, deny/allow precedence, confirmed persistent rule save, /permissions, /mode, audit transcript, fail-closed headless default and temporary allow/deny flags. Targeted G05 tests passed: harness-cli 9/9 and harness-tools 5/5.
- G06: ask_user through HumanInputService, next-input queue, RunInbox steer, running Esc cancellation, file picker, and !/!! shell routing through the existing approval path.
- D1–D11 remain unchanged. T06 Escape behavior was changed as explicitly assigned and documented in HA_TUI.vi.md.
- No code was added beyond G06.

### Verification

- cargo fmt --all and cargo fmt --all -- --check: pass.
- cargo clippy --workspace --all-targets --locked -- -D warnings: pass.
- cargo test -p harness-cli --bin ha --locked: retry 1 had 297 passed, one loopback failure in completion_service_resume_flow, and one ignored; retry 2 passed 298, zero failed, one ignored.
- Updated h03, h05, and t06 tests: each 1/1. milestone_m4: 23/23; phase_p3: 21/21; interactive_session: 14/14.
- interactive_terminal ordinary test run: 18 ignored because those tests require a real console. The requested real-console tests passed through Invoke-HaPtyAcceptance.ps1: h05 attempt 2, i05, i06, i07a, and i07b each 1/1.
- Schema generator ran; focused p0_f03 passed 1/1. Verify-HaLaunch attempts 2 and 3 passed P0–P7 (8/21/17/21/26/27/15/15), providers-streaming, acceptance-session, unit, Clippy, format, installer, release, and docs. Attempt 1 had stale generated schema and failed P0; regeneration fixed it.
- Verify-HaLaunch attempt 2: failures = ["acceptance-launch"], 18/19 launch tests; failed selector i03_a_named_file_reaches_the_model_inside_the_message.
- Verify-HaLaunch attempt 3: failures = ["acceptance-launch"], 18/19 launch tests; failed selector i13_resume_continues_the_task_with_recovered_context_and_no_rerun.
- The verifier keeps only the last three lines from each command; it did not preserve either inner assertion. Do not edit or rerun these loopback tests under the exhausted retry budget.
- Not run: paid/live provider smoke, Linux verification, user install/PATH changes, manual migration, user project config write, and G07+. G05 persistence tests wrote only in disposable temp roots.

### Exact next action

Stop at CP-B. Do not start G07. Before any further loopback test, inspect the existing fixtures and production request path for i03_a_named_file_reaches_the_model_inside_the_message and i13_resume_continues_the_task_with_recovered_context_and_no_rerun without rerunning them or weakening their assertions. Only run them again if the user grants a new retry budget or a deterministic code fix is made and a new verification budget is established.

## Tiếng Việt

### Assignment và ràng buộc

Tiếp tục assignment HA_AGENT đến hết G06 trong docs/HA_AGENT_PLAN.vi.md. Dừng ở CP-B; không bắt đầu G07. Giữ một input được nhận mỗi session, approval fail-closed, protected path bị chặn trước panel, headless không ANSI và exit code hiện hữu. Dùng root workspace và binary ha duy nhất. Không crate mới, engine thứ hai, API trả phí, cài lên máy user, chạy migration thủ công hoặc ghi config.local.toml của project user/workspace dùng chung. Test lưu G05 chỉ ghi trong thư mục tạm có thể xóa. User đã cho phép commit/push CP-B; commit implementation `bb03559f42b5d5efd9b7009ad9c651e4ad8f0ef1` và bản cập nhật tài liệu này đã được push lên origin/master.

### Trạng thái hiện tại

Đã triển khai source G04, G05 và G06. G05 không bị thiếu: PolicyMode và rule pattern dùng ToolPolicy hiện có; thứ tự protected → deny → allow → mode → panel; phím A chỉ lưu sau khi Enter xác nhận; /permissions, /mode, cờ headless và audit transcript từng action đều có test.

CP-B ở trạng thái implemented_unverified vì Verify-HaLaunch cuối vẫn lỗi loopback trong acceptance-launch. Lỗi của launch suite không ổn định giữa các lượt và đã hết retry budget. Không báo CP-B là accepted.

### Source và evidence

- Branch master; commit implementation CP-B: bb03559f42b5d5efd9b7009ad9c651e4ad8f0ef1.
- Implementation và test CP-B đã commit. Bản cập nhật evidence/handoff song ngữ này là commit tài liệu tiếp nối; cả hai commit đã được push lên origin/master.
- Digest source: sha256:7b59aa4dfedbcc26d92dac100b22ac046020d17517001e0aedb58497ef817d00 (420 file tracked và untracked không ignore; chỉ loại evidence CP-B và handoff này). SHA-256 Cargo.lock: 193d2574da4a994777bf8961780e264a123c1397d04d7e66f7bf49d33d919876.
- Đã sinh lại schema bằng cargo run -p harness-types --bin generate_schemas --locked. Không sửa schema JSON bằng tay.
- SPEC hiện tại: docs/specs/HA_AGENT.vi.md; evidence: docs/evidence/HA_AGENT.vi.md.
- Transcript PTY console thật ở target/pty-acceptance-cpb, gồm h05 lượt 2 và i05/i06/i07a/i07b.
- Hệ điều hành: Windows 11 Pro 10.0.26200.0 x64. Toolchain: rustc 1.97.1 (8bab26f4f 2026-07-14), chọn qua C:\Users\duong\.rustup-ha-agent-2026-09-23.

### Phần đã làm

- G04: thêm write_file/edit_file/glob/search_text/read_file additive, lỗi edit typed, validation workspace tập trung, hash trước/sau và artifact preimage có giới hạn, diff approval, TestBackend và tools schema 2→3.
- G05: policy mode/rule, precedence deny/allow, lưu rule dài hạn sau xác nhận, /permissions, /mode, audit transcript, mặc định headless fail-closed cùng cờ allow/deny tạm. Test mục tiêu G05 đạt: harness-cli 9/9 và harness-tools 5/5.
- G06: ask_user qua HumanInputService, queue input kế tiếp, RunInbox steer, Esc hủy lúc đang chạy, file picker và !/!! qua luồng approval hiện có.
- Giữ D1–D11. Hành vi Esc ở T06 được đổi theo assignment và ghi trong HA_TUI.vi.md.
- Không làm code vượt quá G06.

### Kiểm chứng

- cargo fmt --all và cargo fmt --all -- --check: đạt.
- cargo clippy --workspace --all-targets --locked -- -D warnings: đạt.
- cargo test -p harness-cli --bin ha --locked: lượt retry 1 có 297 đạt, một loopback lỗi ở completion_service_resume_flow và một ignored; lượt retry 2 đạt 298, không lỗi, một ignored.
- Test h03, h05, t06 đã cập nhật: mỗi test 1/1. milestone_m4: 23/23; phase_p3: 21/21; interactive_session: 14/14.
- interactive_terminal chạy thường: 18 ignored vì cần console thật. Các test console thật được chạy qua Invoke-HaPtyAcceptance.ps1: h05 lượt 2, i05, i06, i07a, i07b mỗi test đạt 1/1.
- Đã chạy schema generator; p0_f03 riêng đạt 1/1. Verify-HaLaunch lượt 2 và 3 đạt P0–P7 (8/21/17/21/26/27/15/15), providers-streaming, acceptance-session, unit, Clippy, format, installer, release và docs. Lượt 1 dùng schema cũ nên P0 lỗi; sinh schema lại đã sửa.
- Verify-HaLaunch lượt 2: failures = ["acceptance-launch"], launch 18/19; test lỗi i03_a_named_file_reaches_the_model_inside_the_message.
- Verify-HaLaunch lượt 3: failures = ["acceptance-launch"], launch 18/19; test lỗi i13_resume_continues_the_task_with_recovered_context_and_no_rerun.
- Verifier chỉ giữ ba dòng cuối mỗi lệnh nên không lưu assertion bên trong. Không sửa hoặc chạy lại test loopback khi đã hết retry budget.
- Chưa chạy: smoke provider live/trả phí, kiểm chứng Linux, cài/PATH user, migration thủ công, ghi config project user và G07 trở đi. Test lưu G05 chỉ ghi trong thư mục tạm có thể xóa.

### Bước kế tiếp chính xác

Dừng ở CP-B, không làm G07. Trước khi chạy loopback nữa, kiểm tra fixture và production request path của i03_a_named_file_reaches_the_model_inside_the_message và i13_resume_continues_the_task_with_recovered_context_and_no_rerun mà không chạy lại hoặc nới assertion. Chỉ chạy lại nếu user cấp retry budget mới, hoặc đã sửa deterministic code và có budget verification mới.
