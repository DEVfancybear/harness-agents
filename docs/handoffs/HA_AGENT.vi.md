# Handoff — HA_AGENT CP-C (G07–G09) / Bàn giao — HA_AGENT CP-C (G07–G09)

## Tiếng Việt

### Phạm vi và quyết định

Hoàn thành source G07–G09 theo `docs/HA_AGENT_PLAN.vi.md`, dừng ở CP-C; không bắt đầu G10. SPEC CP-C đã được user duyệt trước khi code. Giữ D1–D11, ngoại trừ điều chỉnh D6 theo số đo context/reserve đã ghi trong SPEC. Không thêm crate/dependency, không sửa Cargo.lock, không gọi paid/live API, không cài lên máy user. Các luật session, approval fail-closed, protected path, headless không ANSI và exit code được giữ nguyên.

### Trạng thái

Source và test selector đã triển khai. Trạng thái `implemented_unverified`: `Verify-Milestone.ps1 -Milestone M5 -Json` pass (15/15 required, closure M0–M5). `Verify-HaLaunch.ps1 -Json` chạy lượt đầu và ba lượt retry; không lượt nào có `failures: []`. Kết quả lượt cuối: `providers-streaming` và `regression-phase_p2` fail. Lỗi thay đổi giữa các lượt; providers 32/32 và P2 18/18 pass trong các lần retry riêng, G03 wire snapshot và `p2_s02` đều pass khi chạy selector/suite riêng; i04 Unicode-path cũng pass riêng. Đây là bằng chứng host/loopback chập chờn, nhưng không được tính thành gate pass. Không sửa/nới test.

### Kiểm chứng và bằng chứng

- CLI unit: 320 pass, 0 fail, 1 ignored. `interactive_session`: 14/14. `interactive_launch`: 19/19 ở lượt H1 và H4; lượt H2 lỗi i13 resume, lượt H3 lỗi i04, cả hai selector pass khi retry riêng.
- `phase_p2`: 18/18 ở các lần chạy xanh; P0–P7: 8/8, 21/21, 18/18, 21/21, 26/26, 27/27, 15/15, 15/15 ở các lượt xanh. Providers: 32/32 ở các lượt xanh.
- Format, Clippy `-D warnings`, schema generator, installer self-test, release self-test, docs self-test và hai negative controls V23/V25 pass. Gate H cuối vẫn đỏ như trên.
- Chi tiết từng lượt gate, digest source, Cargo.lock, OS/toolchain và `not_run` nằm trong [evidence](../evidence/HA_AGENT.vi.md). Thiết kế/requirement inventory nằm trong [SPEC](../specs/HA_AGENT.vi.md).
- PTY thật, paid/live provider, Linux, user PATH/install, G10+ chưa chạy.

### Repository

Base `master`: `e3f55d435a2267deac040c0f31d07fe9bded519e`. Source digest: `sha256:51542466e75e522f54d646ca89a268801e725939ee7e66cc105e8497e6906056` (420 files). `Cargo.lock` SHA-256: `193d2574da4a994777bf8961780e264a123c1397d04d7e66f7bf49d33d919876`. Windows 11 Pro `10.0.26200.0` x64; `rustc 1.97.1 (8bab26f4f 2026-07-14)`, `cargo 1.97.1 (c980f4866 2026-06-30)`. Closeout commit/push đang chờ hoàn tất sau khi cập nhật evidence này. Worktree changes chỉ thuộc G07–G09 và tài liệu CP-C. Thuật toán digest được ghi trong evidence.

### Tiếp tục

Giữ phạm vi CP-C. Gate retry budget đã dùng hết (lượt đầu + ba lần chạy lại); không tiếp tục retry hoặc mở G10 theo handoff này. Nếu nối assignment, xử lý nguyên nhân loopback trên runner ổn định hoặc xin giới hạn retry mới rồi mới đánh giá lại CP-C.

## English

### Scope and decisions

Implemented G07–G09 from `docs/HA_AGENT_PLAN.vi.md` and stop at CP-C; do not start G10. The user approved the CP-C SPEC before implementation. D1–D11 remain, except the measured D6 context/reserve adjustment documented in the SPEC. No crate/dependency was added; Cargo.lock is unchanged. No paid/live API call or user installation was made. Session input, fail-closed approval, protected-path ordering, ANSI-free headless output, and exit codes remain intact.

### Status

Source and required selectors are implemented. Status is `implemented_unverified`: `Verify-Milestone.ps1 -Milestone M5 -Json` passed (15/15 required, closure M0–M5). `Verify-HaLaunch.ps1 -Json` ran once plus three retries; no run returned `failures: []`. The final report failed `providers-streaming` and `regression-phase_p2`. Failures moved between runs; providers 32/32 and phase P2 18/18 passed in focused reruns, as did the G03 wire snapshot, `p2_s02`, and i04 Unicode-path selector. This points to unstable host/loopback fixtures, but does not count as a gate pass. No tests were weakened or changed.

### Verification and evidence

- CLI unit: 320 passed, 0 failed, 1 ignored. `interactive_session`: 14/14. `interactive_launch`: 19/19 on H1 and H4; H2 failed i13 resume and H3 failed i04, both passed when rerun alone.
- `phase_p2`: 18/18 on clean runs; P0–P7: 8/8, 21/21, 18/18, 21/21, 26/26, 27/27, 15/15, and 15/15 on clean runs. Providers: 32/32 on clean runs.
- Format, Clippy `-D warnings`, schema generation, installer self-test, release self-test, docs self-test, and V23/V25 negative controls passed. Final H report remains red as noted above.
- Per-run gate details, source digest, Cargo.lock digest, OS/toolchain, and `not_run` are in [evidence](../evidence/HA_AGENT.vi.md). Design and requirement inventory are in the [SPEC](../specs/HA_AGENT.vi.md).
- Real-console PTY, paid/live provider, Linux, user PATH/install, and G10+ were not run.

### Repository

Base `master`: `e3f55d435a2267deac040c0f31d07fe9bded519e`. Source digest: `sha256:51542466e75e522f54d646ca89a268801e725939ee7e66cc105e8497e6906056` (420 files). `Cargo.lock` SHA-256: `193d2574da4a994777bf8961780e264a123c1397d04d7e66f7bf49d33d919876`. Windows 11 Pro `10.0.26200.0` x64; `rustc 1.97.1 (8bab26f4f 2026-07-14)`, `cargo 1.97.1 (c980f4866 2026-06-30)`. Closeout commit/push is pending until this evidence update is complete. Worktree changes are limited to G07–G09 and CP-C documentation. The source digest uses the algorithm recorded in evidence.

### Continuation

Keep the CP-C scope. The gate retry budget is exhausted (initial run plus three retries); do not retry again or start G10 under this handoff. A future continuation must stabilize the loopback runner or obtain a new retry limit before reassessing CP-C.
