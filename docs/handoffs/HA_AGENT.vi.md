# Handoff — HA_AGENT CP-A / Bàn giao — HA_AGENT CP-A

## English

### Assignment and constraints

Continue only the assignment in `docs/HA_AGENT_PLAN.vi.md` items G01–G03 and stop at CP-A. Preserve one input per session, fail-closed approval, protected-path rejection before the approval panel, no ANSI in headless mode, and existing exit codes. No new crate or second product binary/runtime. No G04+. No paid API or HA user install. The user explicitly authorized installing Clippy and committing/pushing this assignment. New `.vi.md` documents are bilingual.

### Current source and ownership

- Branch `master`; assignment baseline `37e355dc2cc22a381c7e86ae3231b14c0c354be0`.
- Implementation commit `0b6d19da17a3387ddbe46daca91ab5f81e66f9d3` (`feat: implement HA_AGENT G01-G03`) is pushed to `origin/master`. It contains 40 reviewed assignment files; concurrent M7–M12 commit `5498f74071aae04998e51afdfa4eeb4281f5e870` is its parent and was preserved.
- Final source digest: `sha256:b9fc76caf88fff2f8eba2a229f7082d3ff10c91104f894e46952b52a95b3a137`, 419 files, excluding only `docs/evidence/HA_AGENT.vi.md` and this handoff. `Cargo.lock`: `sha256:9a778b3745b5ae8e1c954ad1128abb12c9807e1148b7237653a9c1be61a825a6`.
- OS Windows `10.0.26200.0` x64; rustc `1.97.1 (8bab26f4f 2026-07-14)`.
- The implementation commit staged only the reviewed HA_AGENT assignment files; concurrent M7–M12 changes were not included. This evidence/handoff update is documentation-only.

### Completed G work

- **G01:** `interactive/prompt.rs` builds a bounded, secret-free system prompt; `interactive/instructions.rs` loads the AGENTS.md chain with CLAUDE fallback, cap, and notice; real project rules enter the runtime; rule text cannot grant tools; `/init` prints a static sample.
- **G02:** additive `HarnessConfig` v2, trusted project layers and precedence, env/CLI overrides, ConfigExplain, `ha config explain`, `/config`, and `/trust`; generated schema and `p0_f03` pass.
- **G03:** generic OpenAI Chat adapter plus separate DeepSeek preset; Anthropic Messages adapter; HTTP-date retry cap; session model setting with additive store migration; thinking is dimmed in TUI and absent from plain transcript; cost display uses configured prices or `n/a`.
- The SPEC was written before code and is bilingual: [docs/specs/HA_AGENT.vi.md](../specs/HA_AGENT.vi.md). Evidence: [docs/evidence/HA_AGENT.vi.md](../evidence/HA_AGENT.vi.md).

### Latest verification and remaining blockers

- `cargo fmt --all -- --check`: pass.
- `cargo test -p harness-cli --bin ha --locked`: latest exact run **278 passed, 0 failed, 1 ignored**.
- `milestone_m2` **10/10**, `interactive_session` **14/14**, `interactive_launch` **19/19**; P0–P7 regression suites passed in Verify-HaLaunch attempt 3; full PTY finally **17/17** after an isolated I05 pass.
- G01 selectors **5/5**, G02 **5/5**, G03 CLI **3/3**, store model-switch migration **1/1**. The final Verify-HaLaunch attempt 4 passed format, Clippy, unit/acceptance, P0–P7, installer/release, and docs; provider suite was **31/32**, failing `g03_anthropic_429_http_date_is_bounded`. The loopback retry limit is exhausted. Do not rerun or edit that test.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` passed with Rust 1.97.1 installed in `C:\Users\duong\.rustup-ha-agent-2026-09-23`. The default Rustup home was inconsistent; attempted repair encountered a `rustc_driver` DLL locked by Rust Analyzer proc-macro workers and left the default pinned toolchain without `rustc.exe`/`cargo.exe`. Avoid changing the test; restoring the default toolchain requires releasing that DLL lock.
- Verify-HaLaunch attempt 3 failed only at Clippy startup. Attempt 4 passed Clippy and all steps except `providers-streaming`; full details and the capped loopback history are recorded in evidence.
- `pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest` after creating the bilingual evidence and handoff passed (170 Markdown files; 15 language pairs). / Chạy sau khi tạo evidence/handoff song ngữ đạt (170 file Markdown; 15 cặp ngôn ngữ).

### Exact next action

The requested G01–G03 implementation is committed and pushed; do not start G04. Keep CP-A `implemented_unverified` because one provider loopback test failed after the retry budget was exhausted. Do not edit or rerun that test. There is no pending implementation commit. Any resumed work must stay within G01–G03 unless the user starts a new assignment. Do not make a paid call, write `config.local.toml`, rerun the additive migration manually, or install HA.

## Tiếng Việt

### Assignment và ràng buộc

Tiếp tục duy nhất assignment G01–G03 trong `docs/HA_AGENT_PLAN.vi.md`, dừng tại CP-A. Giữ một input mỗi session, approval fail-closed, chặn protected path trước panel approval, headless không ANSI và exit code hiện hữu. Không crate/binary/runtime sản phẩm thứ hai. Không làm G04+. Không API trả phí hoặc cài HA lên máy user. User đã cấp rõ quyền cài Clippy và commit/push assignment này. Tài liệu `.vi.md` mới phải song ngữ.

### Source hiện tại và quyền sở hữu

- Branch `master`; baseline assignment `37e355dc2cc22a381c7e86ae3231b14c0c354be0`.
- Commit triển khai `0b6d19da17a3387ddbe46daca91ab5f81e66f9d3` (`feat: implement HA_AGENT G01-G03`) đã push lên `origin/master`. Commit gồm 40 file assignment đã review; commit M7–M12 đồng thời `5498f74071aae04998e51afdfa4eeb4281f5e870` là parent và được giữ nguyên.
- Digest source cuối: `sha256:b9fc76caf88fff2f8eba2a229f7082d3ff10c91104f894e46952b52a95b3a137`, 419 file; loại khỏi digest chỉ `docs/evidence/HA_AGENT.vi.md` và handoff này. `Cargo.lock`: `sha256:9a778b3745b5ae8e1c954ad1128abb12c9807e1148b7237653a9c1be61a825a6`.
- OS Windows `10.0.26200.0` x64; rustc `1.97.1 (8bab26f4f 2026-07-14)`.
- Commit triển khai chỉ stage các file HA_AGENT đã review; không bao gồm thay đổi M7–M12 đồng thời. Bản evidence/handoff này chỉ cập nhật tài liệu.

### Phần G đã hoàn tất

- **G01:** `interactive/prompt.rs` dựng system prompt có giới hạn và không secret; `interactive/instructions.rs` nạp chain AGENTS.md, fallback CLAUDE, trần và notice; project rules thật vào runtime; nội dung rules không cấp tool; `/init` in mẫu tĩnh.
- **G02:** `HarnessConfig` v2 additive, lớp project có trust và precedence, env/CLI overrides, ConfigExplain, `ha config explain`, `/config`, `/trust`; schema được sinh và `p0_f03` đạt.
- **G03:** adapter OpenAI Chat tổng quát cùng preset DeepSeek riêng; Anthropic Messages adapter; Retry-After HTTP-date có cap; model setting theo session với migration store additive; thinking mờ trong TUI và không vào transcript plain; cost dùng giá cấu hình hoặc `n/a`.
- SPEC viết trước code và song ngữ: [docs/specs/HA_AGENT.vi.md](../specs/HA_AGENT.vi.md). Evidence: [docs/evidence/HA_AGENT.vi.md](../evidence/HA_AGENT.vi.md).

### Verification mới nhất và blocker còn lại

- `cargo fmt --all -- --check`: đạt.
- `cargo test -p harness-cli --bin ha --locked`: lượt exact cuối **278 passed, 0 failed, 1 ignored**.
- `milestone_m2` **10/10**, `interactive_session` **14/14**, `interactive_launch` **19/19**; regression P0–P7 đạt trong Verify-HaLaunch lượt 3; full PTY cuối **17/17** sau khi selector I05 đạt riêng.
- Selector G01 **5/5**, G02 **5/5**, G03 CLI **3/3**, migration model-switch **1/1**. Verify-HaLaunch lượt cuối số 4 đạt format, Clippy, unit/acceptance, P0–P7, installer/release và docs; provider suite **31/32**, lỗi `g03_anthropic_429_http_date_is_bounded`. Đã hết giới hạn retry loopback. Không chạy lại hay sửa test.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` đạt với Rust 1.97.1 cài trong `C:\Users\duong\.rustup-ha-agent-2026-09-23`. Rustup home mặc định không nhất quán; lần sửa gặp DLL `rustc_driver` bị các tiến trình Rust Analyzer proc-macro giữ khóa và khiến toolchain mặc định thiếu `rustc.exe`/`cargo.exe`. Muốn phục hồi toolchain mặc định cần giải phóng khóa DLL.
- Verify-HaLaunch lượt 3 chỉ lỗi khởi chạy Clippy. Lượt 4 đạt Clippy và tất cả bước trừ `providers-streaming`; chi tiết cùng lịch sử loopback ghi ở evidence.
- `pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest` chạy sau khi tạo evidence/handoff song ngữ đã đạt (170 file Markdown; 15 cặp ngôn ngữ).

### Bước kế tiếp chính xác

Phần triển khai G01–G03 đã commit và push; không bắt đầu G04. Giữ CP-A ở trạng thái `implemented_unverified` vì một provider loopback test thất bại sau khi hết retry. Không sửa hoặc chạy lại test đó. Không còn implementation commit đang chờ. Nếu tiếp tục, chỉ làm trong G01–G03 trừ khi user mở assignment mới. Không gọi API trả phí, không ghi `config.local.toml`, không chạy tay migration additive hoặc cài HA.
