# Prompt giao DeepSeek: hoàn thiện `ha` thành CLI coding agent đầy đủ

[Plan chi tiết G01–G14](HA_AGENT_PLAN.vi.md) · [Track TUI T01–T08](HA_TUI_PLAN.vi.md) · [Prompt track T](HA_TUI_PROMPT.vi.md) · [Track khởi động H01–H08](HA_LAUNCH_PLAN.vi.md) · [Sổ tay implementation-next](implementation-next/README.vi.md)

Mỗi prompt là một assignment độc lập, dừng ở điểm dừng ghi trong plan (mục 11–12). Prompt mẫu **không tự cấp quyền** commit/push/paid API/cài thật/publish; muốn cấp thì ghi rõ trong assignment. Trước mỗi assignment DeepSeek đọc lại `git status`/HEAD, `docs/handoffs/CURRENT.vi.md`, `docs/handoffs/HA_TUI.vi.md` và chỉ xử lý gap **thật** so với plan.

## 1. Prompt bắt đầu — G01–G03 (CP-A: ngữ cảnh, config v2, provider)

```text
Triển khai G01–G03 trong docs/HA_AGENT_PLAN.vi.md. Chưa làm G04 trở đi.
Mục tiêu: model biết môi trường và luật dự án (system prompt + AGENTS.md), config.toml
v2 thay biến môi trường với precedence CLI > env > project(trust) > user > default,
provider OpenAI-chat tổng quát + adapter Anthropic Messages, /model, thinking, /cost.

Đọc quy định repo (docs/implementation-next/README.vi.md mục 1–5), git status/HEAD,
docs/HA_AGENT_PLAN.vi.md mục 2–5 và G01–G03, docs/specs/HA_TUI.vi.md, docs/specs/M2.vi.md,
docs/specs/M6.vi.md mục 5.6 (ConfigExplain), docs/handoffs/CURRENT.vi.md, và các file
source plan dẫn. Mọi việc trên root workspace và binary ha duy nhất; không crate mới,
không CLI/engine thứ hai. Giữ luật đã chốt: một input mỗi session, approval fail-closed,
protected path chặn trước panel, headless không ANSI, exit code không đổi.

G01: SystemPromptBuilder (môi trường, luật tool, không secret, ≤ 2 KiB) và chain
AGENTS.md toàn cục → root → cwd (fallback CLAUDE.md, trần 32 KiB có notice) vào channel
ProjectRule; project_rules thật thay Vec::new() ở harness-runtime; nội dung AGENTS.md
không được đổi policy tool (test). /init in mẫu tĩnh nếu G04 chưa có.
G02: HarnessConfig schema_version 2 additive theo plan mục 6 G02 (mọi khoá optional,
v1 load nguyên); lớp user/.harness/config.toml (chỉ khi trust)/config.local.toml; env
rồi CLI --model/--profile/--approval; ConfigExplain cho ha config explain và /config;
/trust; sinh schema bằng cargo run -p harness-types --bin generate_schemas --locked,
p0_f03 xanh. Không sửa schema JSON bằng tay.
G03: tách OpenAiChatAdapter (DeepSeek là cấu hình mặc định, wire format M2 byte-identical
— test snapshot), AnthropicMessagesAdapter với fixture loopback (text, tool_use,
429 + Retry-After HTTP-date, cắt stream), Retry-After HTTP-date + cap có config,
/model đổi từ lượt sau (bảng session_settings, STORE_SCHEMA_VERSION 1→2 additive qua
hạ tầng migration M1), thinking delta hiển thị mờ không vào transcript plain,
cost.rs + /cost + status bar từ [models.<name>] giá; thiếu giá → n/a.

Dependency: chỉ những gì plan mục 4 D10 cho phép, pin exact, Cargo.lock cùng commit,
version/license ghi vào SPEC. Không paid API: live smoke chỉ khi assignment cấp.
Tạo docs/specs/HA_AGENT.vi.md theo docs/implementation-next/TEMPLATES.vi.md mục 1
(requirement inventory reuse_verified/adapt/missing/incompatible cho từng item) TRƯỚC
khi code. RED trước implementation; không xoá/nới test cũ; test mới đặt tên đúng plan.

Chạy fmt, clippy -D warnings, cargo test -p harness-cli --bin ha --locked, milestone_m2,
interactive_session, interactive_launch với --test-threads=1, rồi
scripts/Verify-HaLaunch.ps1 -Json một lần xanh (flake loopback: chạy lại tối đa ba lần,
ghi từng lần, không sửa test). Tạo docs/evidence/HA_AGENT.vi.md và
docs/handoffs/HA_AGENT.vi.md; ghi số test, source digest, OS, not_run.
Dừng sau G03 (CP-A). Không commit/push/paid API/cài lên máy user nếu không được cấp.
```

## 2. Prompt dùng hằng ngày — G04–G06 (CP-B: tools, quyền, tương tác)

```text
Tiếp tục G04–G06 theo docs/HA_AGENT_PLAN.vi.md và docs/handoffs/HA_AGENT.vi.md.
Verify G01–G03 trong source/tests trước khi sửa; giữ quyết định D1–D11 trừ khi có
số đo mâu thuẫn (ghi lại nếu đổi).

G04: write_file, edit_file (old_string duy nhất trừ replace_all; lỗi typed), glob
(globset =0.4.20 đã có trong lockfile, tôn trọng .gitignore qua ignore, trần 4096),
search_text regex/case_insensitive/glob/context_lines (regex =1.13.1 đã có trong
lockfile), read_file offset/limit có số dòng; TOOLS_SCHEMA_VERSION 2→3 additive,
apply_patch giữ; mọi tool mới qua validate_workspace_action; receipt mutating có
before/after hash và lưu nội dung trước khi sửa vào artifact (≤ 1 MiB) cho /undo;
panel approval hiện diff (similar =3.2.0 — kiểm cargo info trước khi pin).
G05: PolicyMode ask|auto-edit|full-auto và rule tool(pattern) trong ToolPolicy hiện có
(không lớp policy thứ hai); thứ tự protected/escape → deny → allow → mode → panel;
phím A "luôn cho phép" ghi rule vào .harness/config.local.toml chỉ sau xác nhận pattern;
/permissions, /mode; transcript ghi [info] allowed by rule|mode cho mọi action tự cho
phép; headless --approval mặc định ask = fail-closed, --allowed-tools/--disallowed-tools.
G06: tool ask_user → HumanInputService → TurnStop::NeedsInput → panel hỏi → tiếp tục
tự động (input mới cùng task); Enter khi Running = queue, /steer qua RunInbox::steer
(gắn inbox cho interactive); Esc khi Running = hủy run (ghi thay đổi quyết định T vào
SPEC HA_TUI có lý do), Esc ở approval không từ chối; @ picker (walker ignore, ≤ 4096,
↑↓ Enter Esc) chèn path; !cmd qua run_shell với cùng approval, !!cmd chỉ hiện.

Kiểm chứng bằng TestBackend cho panel/diff/picker; chạy lại PTY i05/i06/i07/h05 trên
TUI trong console thật bằng scripts/Invoke-HaPtyAcceptance.ps1; không mock host TUI.
milestone_m4, phase_p3, interactive_* xanh; gate H một lần failures: []. Cập nhật
SPEC/evidence/handoff HA_AGENT. Dừng sau G06 (CP-B); không tự làm G07 trở đi.
```

## 3. Prompt session và hooks — G07–G09 (CP-C)

```text
Tiếp tục G07–G09 theo docs/HA_AGENT_PLAN.vi.md và docs/handoffs/HA_AGENT.vi.md.
G07: ModelSummaryProvider gọi provider hiện tại với prompt tóm tắt cố định, lỗi →
fallback deterministic, packet ghi summary_source; /compact [hướng dẫn] gọi
RuntimeService::compact (CAS M5 giữ); auto-compaction khi vượt ngưỡng
context_window − output_reservation − reserve, tối đa một lần mỗi lượt, vẫn vượt →
ContextOverflow typed; context_window theo [models] → bảng nội bộ → 8192 + notice;
/context overlay từ ContextBuildResult; contribution memory bị từ chối → Notice.
G08: title session tự đặt + /rename (session_settings), picker hiện title/model/thời
gian; ha chat --continue; ha sessions list --cwd; /clear; /diff (git_diff read-only,
từ đầu session và uncommitted); /undo là action riêng qua approval có receipt, chỉ phục
hồi file có hash hiện tại == after_hash, nguồn là artifact "before" của G04;
/export JSONL|md không secret; /copy qua arboard.
G09: [hooks.<event>] command với JSON stdin bounded, chạy qua harness-tools::process;
pre_tool_use exit 2 hoặc timeout 60 s = chặn (fail-closed) ghi receipt blocked_by_hook;
hook không thể chuyển hỏi thành cho phép (test bắt buộc); chỉ nạp từ user config và
project đã trust; /hooks; [ui] bell và notify_command.

milestone_m5, interactive_*, phase_p2 xanh; gate H một lần failures: []; gate
Verify-Milestone.ps1 -Milestone M5 passed. Negative control: bỏ kiểm hash ở /undo thì
V23 đỏ; hook in "allow" ra stdout thì V25 vẫn hỏi. Cập nhật SPEC/evidence/handoff.
Dừng sau G09 (CP-C).
```

## 4. Prompt extensions — G10–G12 (CP-D)

```text
Tiếp tục G10–G12 theo docs/HA_AGENT_PLAN.vi.md và docs/handoffs/HA_AGENT.vi.md.
Đọc docs/specs/M6.vi.md (mục 5.5 McpSupportMatrix, 5.4 cancel, 11 giới hạn) và
docs/evidence/M6.vi.md mục 7/9 trước khi sửa harness-extensions.
G10: [mcp_servers.<name>] stdio (command/args/env secret ref/cwd/enabled_tools/
disabled_tools/tool_timeout ≤ 120 s/required); khởi động lười, unload_draining khi
thoát; tool mcp__<server>__<tool> chỉ qua McpToolDispatcher → ToolService với policy +
approval như extension tool; @<server>:<uri> → read_resource text → attachment;
ha mcp add|list|get|remove; /mcp; negative control N8 riêng; McpClient::call_tool
thu về pub(crate) khi P6 test đã chuyển qua dispatcher. G10b (Streamable HTTP, bearer
từ env) CHỈ nếu assignment này nói rõ; nếu làm: feature rmcp
transport-streamable-http-client-reqwest, cargo tree -i reqwest phải một phiên bản,
McpSupportMatrix nói đúng (SSE/OAuth/sampling/elicitation/prompts vẫn unsupported).
G11: skills từ <config-dir>/skills, ~/.agents/skills, .agents/skills và
.harness/skills (project chỉ khi trust), SKILL.md front matter tương thích catalog
M6-01; system prompt chỉ có tên + description (≤ 2 KiB); tool activate_skill theo
digest → channel Skill; /skill:<name>, /skills; prompt template .harness/commands và
<config-dir>/commands → /name với $ARGUMENTS $1..$9; /reload.
G12: WorkerBackend thật (TurnDriver + ToolService + provider từ config/profile) thay
ScriptedWorkerBackend; tool delegate{role explorer|coder, brief}; explorer read-only
cùng workspace, depth 1, ≤ 3 song song, budget trừ ledger cha; coder cần worktree
M8-03 — chưa có thì RoleUnavailable typed, không stub; /agents; Ctrl-C cha hủy child.

milestone_m6, phase_p6, phase_p5 xanh; Verify-Milestone.ps1 -Milestone M6 passed;
a22/a23/a24 giữ xanh; dependency-allowlist: edge mới phải ghi vào
schemas/dependency-allowlist.v1.json cùng commit, không thêm runtime → tools.
Không đổi EXTENSION_PROTOCOL_VERSION/MCP_SPEC_REVISION/CONTEXT_SCHEMA_VERSION.
Cập nhật SPEC/evidence/handoff. Dừng sau G12 (CP-D).
```

## 5. Prompt hoàn thiện và nghiệm thu — G13–G14 (CP-E)

```text
Triển khai G13–G14 theo docs/HA_AGENT_PLAN.vi.md sau khi G01–G12 có evidence.
G13: ha exec "<prompt>" alias headless; --prompt - đọc stdin (trần 10 MiB);
--output-format text|json|stream-json (NDJSON một event/dòng, không ANSI); --continue;
--max-turns; --approval/--allowed-tools/--disallowed-tools từ G05; exit code
0/2/3/4/5/130 theo docs/implementation-next/CONTRACTS.vi.md; --goal satisfied → phát
AcceptanceCommand M3 và ghi acceptance.command_id; i03 headless cũ giữ xanh.
G14 bước 0: xác minh Verify-HaLaunch.ps1 -Json xanh ở HEAD và commit a5ac2e2 trên
origin còn hỏng không, ghi kết quả. Rồi: ci.yml thêm job ubuntu + windows chạy
Verify-Milestone.ps1 -Milestone M4, M5, M6 và Verify-HaLaunch.ps1 -Json (không PTY),
upload log; run_shell Windows fallback powershell.exe khi không có pwsh, receipt ghi rõ;
FakeProvider theo từng attempt để milestone_m2 10/10 lần xanh; StorePort: implement
trên SqliteStore hoặc ADR-N12 gỡ port (quyết định ghi rõ, không để lơ lửng); sửa help
footer ↑↓, /key giữ khoảng trắng, --cwd cho ha memory|tasks|maintenance; PTY mới
g05/g06/g08 + /key + /more theo plan G14.7, toàn bộ ca cũ + mới xanh MỘT lần chạy;
Verify-HaLaunch.ps1 thêm bước unit-agent và ≥ 8 selector G bắt buộc; cargo audit/deny
chỉ khi cài được --locked với version ghi trong SPEC, ngược lại not_run có lý do;
OPERATOR_GUIDE vi+en mục 12 viết lại (18 tool, slash đầy đủ, config v2, permissions,
hooks, MCP, skills, headless); README đoạn Status cập nhật; evidence/handoff HA_AGENT
đầy đủ theo TEMPLATES; Verify-Docs.ps1 -SelfTest xanh. CI ubuntu build --release và
upload artifact Linux x64, không publish.

Bằng chứng: gate H passed:true failures:[], gate M6 passed, PTY_EXIT 0 đủ ca kèm
transcript, CI run id ubuntu xanh (đó là bằng chứng Linux duy nhất — evidence ghi
platform: linux via CI), negative controls, not_run (live smoke, VM sạch). Không đổi
registry tests/acceptance sang accepted — việc của reviewer. Không publish, không cài
lên máy user, không paid smoke nếu không được cấp. Chỉ nhận "track G xong" khi CP-E đạt.
```

## 6. Nếu muốn giao cả track trong một assignment

Dùng: "Triển khai G01–G14 theo plan HA_AGENT, đi theo checkpoint CP-A → CP-E; không chuyển checkpoint khi gate chưa `failures: []`; cập nhật handoff HA_AGENT sau mỗi checkpoint; G10b và G12 role `coder` chỉ làm khi tôi nói rõ." Track này chắc chắn cần nhiều lượt coding. Quyền commit/push, paid smoke, cài thật phải ghi trong assignment; đưa prompt mẫu này không tự cấp quyền.

## 7. Prompt tiếp tục sau khi hết context

```text
Tiếp tục assignment HA_AGENT đang ghi trong docs/handoffs/HA_AGENT.vi.md.
Đối chiếu git status/HEAD, docs/specs/HA_AGENT.vi.md, evidence và docs/HA_AGENT_PLAN.vi.md
trước khi sửa; inspect symbol/test của item đang dở, đừng chỉ tin summary. Giữ D1–D11
của plan nếu không có số đo mâu thuẫn. Không lặp lệnh có side effect đã xong (migration,
ghi config.local.toml, paid call). Làm exact next action, chạy affected tests và gate
cần thiết, cập nhật handoff. Không tự chuyển sang item/checkpoint kế tiếp khi assignment
hiện tại kết thúc.
```
