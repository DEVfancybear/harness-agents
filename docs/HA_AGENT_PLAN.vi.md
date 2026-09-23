# Plan cho DeepSeek: hoàn thiện `ha` thành CLI coding agent đầy đủ (track G01–G14)

**Lập ngày 22/09/2026 · Planning only · Feature track G01–G14.** Chưa có dòng code G nào tồn tại; trạng thái thật của track nằm ở `docs/specs/HA_AGENT.vi.md`, `docs/evidence/HA_AGENT.vi.md` và `docs/handoffs/HA_AGENT.vi.md` (planned, DeepSeek tạo khi nhận assignment).

[Prompt giao DeepSeek](HA_AGENT_PROMPT.vi.md) · [Track khởi động H01–H08](HA_LAUNCH_PLAN.vi.md) · [Track TUI T01–T08](HA_TUI_PLAN.vi.md) · [Master plan](HARNESS_MASTER_PLAN.vi.md) · [Roadmap M0–M12](HARNESS_ROADMAP.vi.md) · [Sổ tay implementation-next](implementation-next/README.vi.md) · [Bản đồ tích hợp](implementation-next/INTEGRATION_MAP.vi.md) · [Templates SPEC/evidence/handoff](implementation-next/TEMPLATES.vi.md) · [Handoff hiện tại](handoffs/CURRENT.vi.md)

## 1. Vì sao có track này

Sau H (khởi động), T (TUI) và M0–M6 (kernel, store, provider, TurnDriver, tools, context, extensions), `ha` đã là một app tương tác chạy thật với DeepSeek: streaming, tool gate, approval fail-closed, resume, ảnh/file trong yêu cầu. Nhưng khi đối chiếu **code tại HEAD `9bc7d49`** với ba tham chiếu (OpenAI Codex CLI, Claude Code, pi coding agent) và với chính docs của repo, `ha` còn thiếu phần lớn những gì cả ba tham chiếu đều có ("table-stakes"):

| Nhóm | Codex / Claude Code / pi đều có | `ha` tại `9bc7d49` |
|---|---|---|
| Ngữ cảnh cho model | System prompt có môi trường (OS, cwd, git, ngày), hướng dẫn dùng tool; `AGENTS.md`/`CLAUDE.md` toàn cục + theo thư mục | System prompt **một dòng** `"You are a careful coding agent."` ([`runtime/lib.rs:318`](../crates/harness-runtime/src/lib.rs)); `project_rules` **luôn rỗng** (`lib.rs:2101`); không đọc file hướng dẫn nào |
| Cấu hình | File config user + project, chọn model/effort, profile, precedence rõ | `config.toml` chỉ có `schema_version` và `[cli] output` ([`contracts.rs`](../crates/harness-types/src/contracts.rs)); mọi knob là biến môi trường (`HA_PROVIDER_*`, `HA_TURN_*`, `HA_MEMORY`, `HA_EXTENSIONS`) |
| Provider | Nhiều provider/endpoint, đổi model giữa phiên, thinking level, chi phí | Một adapter DeepSeek/OpenAI-chat ([`providers/lib.rs`](../crates/harness-providers/src/lib.rs)); `provider_id` cố định `"deepseek"`; `/model` chỉ đọc; `thinking` bị tắt cứng; không có `/cost` |
| Tools | read (offset/limit), write, edit (search/replace), glob, grep regex, shell; web fetch; ask-user; todo/plan | `read_file` không offset; `apply_patch` là **thay cả file** theo hash ([`tools/contracts.rs`](../crates/harness-tools/src/contracts.rs)); không `write_file`/`edit_file`/`glob`; `search_text` là substring; không có tool hỏi người dùng |
| Quyền | Chế độ ask / auto-edit / full-auto; rule allow/deny theo tool + pattern; "always allow" ghi vào settings; trust project | Hỏi **mọi** action kể cả `read_file`; chỉ có `a` cho **một lượt**; `ToolPolicy` rules và `granted_secrets` tồn tại nhưng CLI không bao giờ đặt ([`policy.rs`](../crates/harness-tools/src/policy.rs)); headless fail-closed mọi tool |
| Session | `/compact` + auto-compaction, `/clear`, `/cost`, `/diff`, `/rename`, `--continue`, undo/rewind, export | `compact()` chỉ được gọi từ fixture M5 ([`m5_fixture_host.rs`](../crates/harness-cli/src/bin/m5_fixture_host.rs)); summarizer là stub; `context_window_tokens` **cứng 8192** (`lib.rs:229`); không có các lệnh trên |
| Input | `@file` picker, `!cmd`, queue message khi đang chạy, Esc ngắt | Có path/ảnh/`/attach`; không `@` picker, không `!`, Enter khi đang chạy bị từ chối, Esc không ngắt |
| Extensions | MCP stdio + HTTP trong chat, skills `SKILL.md` gọi được, prompt template/custom command, hooks chặn tool, subagent tool | `McpClient`/`SkillContributor`/`compose_skills` **không được `ha chat` gọi** — chỉ có subcommand inspect ([`extension_cli.rs`](../crates/harness-cli/src/extension_cli.rs)); không hooks; delegation chỉ chạy `ScriptedWorkerBackend` + `MockProvider` ([`delegation_cli.rs`](../crates/harness-cli/src/delegation_cli.rs)) |
| Headless | print mode, JSONL event stream, allow/deny tools, resume, stdin | Có `--headless --prompt --json` một lượt; không stream-json, không allow/deny, không đọc stdin |
| Nền tảng | Windows/macOS/Linux đều được chứng minh | Mọi milestone M chỉ chứng minh trên Windows; `ci.yml` không chạy `milestone_m4..m6`; `run_shell` Windows cần `pwsh` |

Đồng thời, docs của repo tự ghi **93 giới hạn/mock/chưa làm** (mục 3). Track G gom những khoản **không thuộc M7–M12** thành 14 work item để `ha` thành một CLI agent hoàn chỉnh, và trỏ rõ khoản nào để M7–M12 làm.

Mockup hành vi mục tiêu (không phải output hiện có):

```text
Harness Agents 0.2.0        project: C:\work\parser   git: main (2 changed)
provider: anthropic/claude-sonnet-5 · mode: auto-edit · AGENTS.md: 2 files · mcp: 1 server · skills: 3

> @src/parser.rs sửa lỗi ở dòng 42 rồi chạy tests
● read_file   src/parser.rs:30-60                         ok 4ms
● edit_file   src/parser.rs  (+1 −1)                      ok 2ms      ← auto-edit: không hỏi
● run_shell   cargo test -p parser                        ▸ cần phê duyệt
┌ approval ──────────────────────────────────────────────────────────┐
│ run_shell  cargo test -p parser        timeout 60s   cwd C:\work\parser
│ y chạy · a cả lượt · A luôn cho phép "cargo test *" · n từ chối · Esc
└─────────────────────────────────────────────────────────────────────┘
 ⠹ running · step 3/8 · tools 3/16 · 12.4k/200k ctx · $0.012 · 00:09   Esc ngắt
```

## 2. Căn cứ source ở HEAD `9bc7d49`

Khảo sát tại `9bc7d49` (`docs(m6): record the passed gate…`), nhánh `master`, cây sạch. Bảng dưới là **baseline lúc lập plan**; trước mỗi assignment DeepSeek phải đọc lại `git status`, SPEC/evidence/handoff của H/T/M và chỉ xử lý gap **thật**.

| Căn cứ | Hành vi hiện có (đã có test) | Khoảng trống cho track G |
|---|---|---|
| [`harness-runtime/src/lib.rs`](../crates/harness-runtime/src/lib.rs) `RuntimeConfig`, `RuntimeService` | `system_policy` một dòng; `context_window_tokens: 8192`, output reservation 1024, optional budget 2048; retry 3 lần, `Retry-After` chỉ giây và cap 2 s; `compact()` có CAS, `DefaultSummaryProvider` là stub | Không có builder system prompt; không lấy cửa sổ ngữ cảnh theo model; `compact()` không có caller trong `ha`; `project_rules` rỗng |
| [`harness-session/src/context.rs`](../crates/harness-session/src/context.rs) | 10 channel (`Policy…Skill`), authority class, ranking optional theo budget, port `ContextContributor`, `build_with_contributors` | `ProjectRule`, `Skill`, `Instruction` chưa có producer nào trong `ha chat`; chưa có báo cáo "context breakdown" cho người dùng |
| [`harness-types/src/contracts.rs`](../crates/harness-types/src/contracts.rs) `HarnessConfig` + [`schemas/harness-config.v1.schema.json`](../schemas/harness-config.v1.schema.json) | TOML strict, `deny_unknown_fields`, chỉ `schema_version` + `[cli]` | Không có provider/model/limits/permissions/mcp/hooks; không có lớp project; `ha config explain` in `not_available_in_p1` |
| [`interactive/bootstrap.rs`](../crates/harness-cli/src/interactive/bootstrap.rs), [`bounds.rs`](../crates/harness-cli/src/interactive/bounds.rs), [`credentials.rs`](../crates/harness-cli/src/interactive/credentials.rs) | Đọc `HA_PROVIDER_ENDPOINT/MODEL`, `HA_TURN_*`; credential env > file `private/credentials.env`; `/key` | Env là nguồn duy nhất; không có precedence CLI > env > project > user; chỉ một key `DEEPSEEK_API_KEY`/`HA_API_KEY` |
| [`harness-providers/src/lib.rs`](../crates/harness-providers/src/lib.rs) `ModelProvider`, `DeepSeekAdapter`, `MockProvider` | Port `stream`/`stream_events`; SSE bounded; `CapabilityMatrix::deepseek_documented`; https bắt buộc trừ loopback | Không có adapter thứ hai; `thinking:{type:"disabled"}` cứng; không có usage/pricing |
| [`harness-tools/src/contracts.rs`](../crates/harness-tools/src/contracts.rs), [`workspace.rs`](../crates/harness-tools/src/workspace.rs), [`service.rs`](../crates/harness-tools/src/service.rs), [`policy.rs`](../crates/harness-tools/src/policy.rs) | 13 tool; prepare → approval → intent → execute → receipt; path policy (protected names, không thoát root); `EffectClass` read_only/mutating/external; `ApprovalMode::{Ask, None}`; `ToolPolicy` có rules nhưng mặc định rỗng | Không có write/edit/glob/regex; không có diff preview; không có mode auto-edit/full-auto; không có rule allow/deny từ config; không có hook |
| [`harness-tools/src/turn_driver.rs`](../crates/harness-tools/src/turn_driver.rs) | `TurnLimits` 8/16/10 phút; loop detection; `TurnStop::NeedsInput` chỉ từ goal evaluator | Model không có tool `ask_user`; không có delegate |
| [`harness-runtime/src/inbox.rs`](../crates/harness-runtime/src/inbox.rs), [`human_input.rs`](../crates/harness-runtime/src/human_input.rs) | `RunInbox::steer`, `HumanInputService::ask` — durable, có test M3 | Interactive không gắn inbox; không có `/steer`; không có UI trả lời câu hỏi của model |
| [`interactive/controller.rs`](../crates/harness-cli/src/interactive/controller.rs), [`input.rs`](../crates/harness-cli/src/interactive/input.rs), [`tui/`](../crates/harness-cli/src/interactive/tui/mod.rs) | 11 slash command (`input.rs:755-813`); `y/a/n`; picker `/resume`; overlay help/status; `/more` 500 dòng; markdown-lite | Không `/compact /clear /cost /diff /rename /permissions /mcp /skills /hooks /context /undo /export /copy`; không `@`/`!`; Enter khi Running bị từ chối; Esc không ngắt |
| [`interactive/headless.rs`](../crates/harness-cli/src/interactive/headless.rs) | Một lượt, JSON schema 1, `ApprovalMode::None`, `--goal/--criteria/--budget/--mock/--resume` | Không `stream-json`, không `--approval`, không allow/deny tools, prompt chỉ qua `--prompt` |
| [`harness-extensions/src/{mcp,skills,catalog,config}.rs`](../crates/harness-extensions/src/mcp.rs) | `McpClient` stdio, `McpSupportMatrix` (Tools+Resources), `McpToolDispatcher: ExternalToolDispatcher`; skill catalog metadata-only + activate theo digest; `SkillContributor`; `ConfigExplain` | Không được nối vào `ha chat`; không có file cấu hình MCP; skill không bao giờ được activate trong lượt |
| [`harness-orchestrator`](../crates/harness-orchestrator/src/lib.rs), [`delegation_cli.rs`](../crates/harness-cli/src/delegation_cli.rs) | DAG, scheduler, budget ledger, worktree per worker, integrator — chạy với `ScriptedWorkerBackend` | Không có worker dùng provider thật; model không có tool delegate; `/agents` không có |
| [`.github/workflows/ci.yml`](../.github/workflows/ci.yml), [`scripts/Verify-Milestone.ps1`](../scripts/Verify-Milestone.ps1), [`scripts/Verify-HaLaunch.ps1`](../scripts/Verify-HaLaunch.ps1) | CI ubuntu+windows chạy `Verify-Phase` P0/P3–P7; gate M có retry flake; gate H/T có selector bắt buộc + PTY thủ công | CI không chạy `Verify-Milestone -Milestone M4..M6` và gate H/T; Linux là `platform pending` cho A12–A24 |

Ba luật nền tảng đã đo ở H/T/M và **không đổi** ở track G: một input mỗi session (hội thoại là chuỗi session cùng task); approval fail-closed (không trả lời = hết hạn = không chạy); protected paths và thoát workspace bị chặn **trước** khi có panel — không mode nào, rule nào, hook nào mở được.

## 3. Giới hạn docs đã ghi và track nào đóng

Từ [SPEC M6 mục 11](specs/M6.vi.md), các SPEC/evidence/handoff M0–M6, H, T, [MEMORY_RECALL](handoffs/MEMORY_RECALL.vi.md), [P7](handoffs/P7.vi.md), [audit](audit/SECURITY_PERF_AUDIT.vi.md) và [`known_defects.rs`](../crates/harness-cli/tests/known_defects.rs). Cột "Đóng ở" là quyết định của plan này.

| # | Giới hạn đã ghi (nguồn) | Đóng ở |
|---|---|---|
| L1 | **M6 §11:** nhánh Unix của `process_wrap`/transport không chạy được trên host (không WSL/Docker); A22/A23/A24 chỉ chứng minh trên Windows → `platform pending` Linux. Cùng giới hạn cho A12–A21 (M4/M5 §11) | **G14** — CI ubuntu chạy gate M4–M6 và gate H/T không-PTY |
| L2 | **M6 §11:** một số capability MCP, marketplace, WASM, dynamic ABI chưa được nối vào chat; "công bố rõ, không tạo success placeholder" | **G10** nối MCP stdio + Streamable HTTP vào chat và hỗ trợ mọi entry của `McpSupportMatrix`; SSE/OAuth, multi-round prompts/tool elicitation, sampling kèm tools/ảnh và tự mở rộng resource template URI vẫn là giới hạn tường minh; marketplace/WASM/dynamic ABI vẫn ngoài phạm vi |
| L3 | **M6 §11:** mock chỉ provider/network ngoài; store/runner/transport thật | Giữ nguyên nguyên tắc ở mọi item G |
| L4 | Live/paid smoke không chạy trong gate M; capability DeepSeek là fixture (M2 §5.7) | **G03** mở rộng `Smoke-HaProvider.ps1` cho mọi provider trong config; vẫn **không** chạy trong gate, chỉ khi assignment cấp |
| L5 | `Retry-After` chỉ giây, cap 2 s (evidence M2 §8) | **G03** |
| L6 | Flake `milestone_m2::a07_401…` 3/8 xanh; cần `FakeProvider` theo từng attempt (CURRENT §7.1) | **G14** |
| L7 | Không sandbox; `strict_isolation:false` (M4 §12.2) | **M12** — G không claim sandbox |
| L8 | `StorePort` chưa implement từ M0-02 (CURRENT §7.4) | **G14** — implement trên `SqliteStore` hoặc ADR gỡ bỏ, không để lơ lửng |
| L9 | Headless fail-closed mọi tool, chưa có flag cho phép (HA_LAUNCH H08, audit §5 "cần user chốt") | **G05 + G13** — `--approval` với `full-auto` chỉ khi flag tường minh |
| L10 | Read-only allowlist 5 tool, không phân loại lệnh shell read-only (HA_TUI §3g.2) | **G05** rule pattern cho `run_process`/`run_shell` |
| L11 | Không có trust vĩnh viễn / policy file / `--yes` (HA_TUI §3j.2) | **G05** |
| L12 | Reconciliation cho budget reservation `Unknown` không có (M3 §12.3) | **M9** |
| L13 | Task acceptance chỉ ghi proposal, không phát `AcceptanceCommand` (M3 §12.3) | **G13** khi headless có `--goal`: phát command khi goal satisfied |
| L14 | `run_shell` Windows dùng `pwsh` — không có PowerShell 7 thì hỏng (inventory code) | **G14** fallback `powershell.exe` ghi trong receipt |
| L15 | PTY cases thiếu cho `/key`, `/more`, phím cuộn, slash menu, `a` (HA_TUI §3i.5, §3j.5) | **G14** |
| L16 | Help footer nói `↑↓ cuộn` nhưng ↑↓ sửa draft (HA_TUI §3e.2); `/key <value>` chỉ lấy token đầu (§3d.5) | **G14** |
| L17 | Runtime bỏ im lặng contribution memory khi `validate_contribution` lỗi (MEMORY_RECALL §7.3) | **G07** phát `Notice` |
| L18 | Không embeddings, không memory publication, A19 nửa M7 | **M7** |
| L19 | Fork chỉ copy view; rollback không phục hồi file (M5 §5, §6.6) | **G08** `/undo` là action riêng có before/after hash (master plan §6); fork ownership → **M8** |
| L20 | N8 chưa có negative control; `McpClient::call_tool` vẫn `pub` (evidence M6 §7.1, §9) | **G10** (khi P6 test chuyển sang dispatcher thì thu hẹp visibility) |
| L21 | `ha extensions config-explain` luôn `active_plugins: []` | **G02** dùng `ConfigExplain` cho config thật, có plugin/MCP đang active |
| L22 | Backup/restore/retention/doctor/install smoke/release | **M9** |
| L23 | Không daemon, schedules, external tasks | **M11** |
| L24 | Web | **M10** |
| L25 | OPERATOR_GUIDE lệch code: "chín tool" (có 13), ví dụ `deepseek-chat` (mặc định `deepseek-flash`), bảng slash thiếu `/more /image /attach`; README "Status" vẫn nói P5+ chưa bắt đầu | **G14** |
| L26 | Commit `a5ac2e2` trên origin không build (HA_TUI handoff §17) — kiểm lại có còn đúng ở HEAD hiện tại không | **G14** bước 0: xác minh, ghi kết quả |

Ngoài scope track G và **không** làm khi chưa giao: OAuth login theo subscription, marketplace/plugin registry, cloud session, IDE extension, voice, LSP, sandbox OS (M12), web (M10), daemon (M11), vector memory (M7), DAG bền vững (M8).

## 4. Quyết định kiến trúc

Các quyết định dưới đây là mặc định của plan; muốn đổi phải ghi lý do và số đo vào SPEC HA_AGENT, không đổi ngầm.

**D1 — Một binary, một engine, nối vào cái đã có.** Mọi item G sửa `crates/harness-cli` và các crate `harness-*` hiện có; không crate UI/agent mới; không CLI/runtime thứ hai. `TurnDriver`, `ToolService`, `RuntimeService`, `ContextCompiler`, `McpToolDispatcher`, `SkillContributor`, `HumanInputService`, `RunInbox` là **điểm nối bắt buộc** — item nào bỏ qua chúng để viết đường riêng là sai.

**D2 — Config là nguồn cấu hình, env vẫn thắng, CLI thắng env.** `config.toml` schema_version **2** (tất cả khoá mới optional; file v1 vẫn load nguyên vẹn — test compat). Lớp: `default < user config < project config (chỉ khi project được trust) < env < CLI flag`. Mọi khoá phải giải thích được bằng `ha config explain` (tái dùng `ConfigExplain` M6-04: giá trị, lớp nguồn, lý do inactive). Schema JSON sinh bằng `generate_schemas`, không sửa tay; drift test `p0_f03` phải xanh.

**D3 — Hướng dẫn dự án là `AGENTS.md`.** Thứ tự nạp: `<config-dir>/AGENTS.md` (toàn cục) → `AGENTS.md` ở project root → `AGENTS.md` ở từng thư mục con trên đường từ root tới cwd; fallback tên `CLAUDE.md` khi thư mục không có `AGENTS.md`. Trần tổng 32 KiB (như Codex `project_doc_max_bytes`), cắt có thông báo. Đưa vào channel `ProjectRule` (authority `User`, có thể mandatory); file trong project **không** được cấp thêm quyền tool — chỉ là văn bản.

**D4 — Ba chế độ quyền, một nguồn sự thật.** `ask` (hiện tại), `auto-edit` (read-only + mutating trong workspace không hỏi; `run_process`/`run_shell`/extension/MCP hỏi), `full-auto` (không hỏi trong workspace). Rule `allow`/`deny` dạng `tool(pattern)` ví dụ `run_shell(cargo test *)`, `read_file(docs/**)`. Thứ tự quyết định: **protected path / thoát root (chặn cứng) → deny rule → allow rule → mode → panel**. Rule và mode sống trong `ToolPolicy` (đã có `PolicyRule`, `revision`) — không tạo lớp policy thứ hai. `a` cả lượt giữ nguyên; thêm `A` = "luôn cho phép pattern này" ghi rule vào `.harness/config.local.toml` (gitignored) sau khi người dùng xác nhận pattern hiển thị. Headless mặc định `ask` = fail-closed như nay; `full-auto` chỉ khi `--approval full-auto` tường minh.

**D5 — Provider thứ hai chứng minh port.** Thêm `AnthropicMessagesAdapter` (Messages API, SSE, `tool_use`/`tool_result`, `max_tokens` bắt buộc, `thinking` budget) bên cạnh adapter OpenAI-chat được tổng quát hoá (`provider_id` từ config, endpoint bất kỳ https, Ollama/vLLM/OpenRouter đi đường này). Không thêm OAuth. Mock/fixture HTTP loopback cho cả hai protocol; live smoke chỉ khi assignment cấp.

**D6 — Compaction là của model, có fallback xác định.** `SummaryProvider` mới gọi provider hiện tại với prompt tóm tắt cố định; lỗi → fallback deterministic hiện có, và packet ghi `summary_source: model|fallback`. Auto-compaction khi `packet_tokens > context_window − output_reservation − reserve` (reserve mặc định 16 384 như pi). `context_window` theo model: từ `[models.<name>] context_window` trong config, fallback bảng nội bộ theo tên model đã biết, fallback cuối 8192 **kèm notice**.

**D7 — Hook là lệnh ngoài, chỉ được chặn, không được cấp.** Sự kiện `session_start | user_prompt_submit | pre_tool_use | post_tool_use | stop | notification`; handler = command nhận JSON qua stdin, exit 0 = tiếp tục, exit 2 = chặn kèm lý do stderr, timeout 60 s = coi như lỗi và **chặn** (fail-closed); hook chỉ nạp từ user config hoặc project config đã trust. Hook không thể chuyển một action từ "hỏi" thành "chạy".

**D8 — Extension trong chat qua đúng cổng M6.** MCP: `McpToolDispatcher` là đường duy nhất vào `ToolService`; tool tên `mcp__<server>__<tool>`; approval và policy như tool extension. Skills: `SkillContributor` + `activate` theo digest; model chỉ thấy **metadata** (progressive disclosure), body chỉ nạp khi activate. Không đổi `EXTENSION_PROTOCOL_VERSION`, không đổi `MCP_SPEC_REVISION` trừ khi G10b cần và có số đo.

**D9 — Subagent trong lượt tái dùng orchestrator, không engine mới.** Tool `delegate` chạy `TurnDriver` con với provider thật qua `WorkerBackend` thật (thay `ScriptedWorkerBackend`), depth 1, tối đa 3 song song, role `explorer` read-only không cần worktree; role `coder` được host cấp worktree bằng `WorkerScheduler`/`WorkspaceManager` M8-03 từ snapshot Git sạch, persist record và giữ lại branch/path. Nếu input bẩn, manager không có hoặc worktree không tạo được thì trả `RoleUnavailable{reason}`; không chạy coder trên checkout người dùng và không tạo stub.

**D10 — Dependency pin đúng, không bịa.** Chỉ dùng crate đã có trong `Cargo.lock` khi có thể: `regex = "=1.13.1"` và `globset = "=0.4.20"` đã nằm trong lockfile (qua `ignore`/`sqlx`) — thêm vào `[workspace.dependencies]` với đúng version đó, `cargo tree -d` không được sinh bản thứ hai. Diff preview: `similar = "=3.2.0"` (Apache-2.0, MSRV 1.85; `cargo info similar` ngày 22/09/2026) — DeepSeek kiểm lại bằng `cargo info` trước khi pin. G10b: bật feature `transport-streamable-http-client-reqwest` của `rmcp =3.4.0`; điều kiện dừng: `cargo tree -i reqwest` chỉ có **một phiên bản**, version thực tế lấy từ lockfile. Không dependency edge runtime → tools.

**D11 — Kiểm chứng như H/T/M.** Unit `TestBackend` cho TUI; fixture HTTP loopback qua adapter thật cho provider; fixture binary thật cho MCP/hook/subagent; PTY thật cho phím mới; không mock host; không stub-success; test chạy 0 case là gate fail.

## 5. Kiến trúc module dự kiến

```text
crates/harness-cli/src/interactive/
  config.rs          (+ lớp user/project/local, precedence, trust list, ConfigExplain)   G02
  instructions.rs    (mới) nạp AGENTS.md chain → ContextBlock ProjectRule                 G01
  prompt.rs          (mới) system prompt builder: policy + environment + tool guidance    G01
  permissions.rs     (mới) mode + rules → ToolPolicy; ghi rule "always allow"            G05
  hooks.rs           (mới) HookRunner: sự kiện → command, JSON stdin, exit code          G09
  mcp.rs             (mới) [mcp_servers] → McpRuntime → McpToolDispatcher                G10
  skills.rs          (mới) discovery roots, /skill:<id>, activate_skill tool             G11
  commands.rs        (mới) prompt template .harness/commands/*.md → /name $ARGUMENTS     G11
  mention.rs         (mới) @ picker (ignore walker, fuzzy), ! shell prefix               G06
  cost.rs            (mới) usage → pricing table → $                                     G03
  service.rs         (+ compaction wiring, ask_user, steer, delegate, hooks, mcp)        G06 G07 G12
  controller.rs      (+ slash mới, queue khi Running, Esc ngắt, panel ask_user/diff)     G06 G08
  headless.rs        (+ --output-format, --approval, --allowed-tools, stdin)             G13
crates/harness-providers/src/
  openai_chat.rs     (tách từ lib.rs, provider_id/capabilities từ config)                G03
  anthropic.rs       (mới) Messages API adapter                                          G03
crates/harness-tools/src/
  contracts.rs       (+ write_file, edit_file, glob, search_text regex, read_file range) G04
  workspace.rs       (+ edit/glob/regex, diff summary cho approval)                      G04
  policy.rs          (+ PolicyMode, rule pattern matcher — tái dùng PolicyRule)          G05
  turn_driver.rs     (+ ask_user tool → NeedsInput; delegate tool)                       G06 G12
crates/harness-runtime/src/lib.rs
                     (+ ModelSummaryProvider, auto-compaction threshold, window theo model) G07
crates/harness-types/src/contracts.rs
                     (HarnessConfig v2)                                                  G02
```

Luồng một lượt sau track G (phần in đậm là mới):

```text
input → **hooks.user_prompt_submit** → **@mention/!cmd resolve** → attachments
      → context: Policy(**prompt builder**) + **ProjectRule(AGENTS.md)** + **Skill(đã activate)** + Memory + Tail
      → **auto-compaction nếu vượt ngưỡng**
      → provider (**openai_chat | anthropic**) stream
      → tool call → prepare → **deny/allow rule → mode** → (panel | auto) → **hooks.pre_tool_use** → intent → execute → receipt → **hooks.post_tool_use**
      → **ask_user** → panel hỏi → HumanInputService → tiếp tục
      → **delegate** → child TurnDriver → kết quả là tool result
      → run terminal → **hooks.stop** → **cost/usage** vào status + `/cost`
```

## 6. Work items G01–G14

Mỗi item: mục tiêu, phụ thuộc, việc cụ thể, test oracle, tiêu chí đóng, ước lượng (S ≈ 1 lượt coding, M ≈ 2–3, L ≈ 4+). Mỗi item làm RED → GREEN, chạy `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, affected tests; không nhận item xong khi còn `ignored` trong selector bắt buộc; không stub-success trong production path.

### G01 — System prompt và AGENTS.md (M)

Phụ thuộc: không. Mục tiêu: model biết mình đang ở đâu và dự án muốn gì.

1. `interactive/prompt.rs`: `SystemPromptBuilder` sinh `system_policy` gồm: vai trò + luật tool (đọc trước khi sửa, không đoán path, trả lời ngắn khi xong, không tự nới giới hạn); khối môi trường (OS, shell dùng cho `run_shell`, cwd, project root, git branch + số file thay đổi từ `git_status` hiện có, ngày ISO, giới hạn lượt `TurnLimits`); danh sách tool có sẵn theo `EffectClass`. Không có secret. Tổng ≤ 2 KiB mặc định; đo token bằng estimator hiện có.
2. `interactive/instructions.rs`: chain `AGENTS.md` theo D3; mỗi file thành một `ContextBlock` channel `ProjectRule` với `source` là path tương đối + digest; trần 32 KiB tổng, cắt có `Notice`; file nằm ở protected path bị bỏ qua có thông báo. Nạp lại mỗi lượt (không cache qua lượt).
3. `service.rs`: truyền `project_rules` thật vào `RuntimeService` (thay `Vec::new()` ở `lib.rs:2101`); header TUI thêm `AGENTS.md: n files`.
4. `/init`: nếu chưa có `AGENTS.md` ở root thì hỏi model sinh bản mẫu (một lượt bình thường, đi qua `write_file` của G04 nên cần approval) — khi G04 chưa có, `/init` chỉ in mẫu tĩnh ra transcript và ghi rõ.
5. Test: `g01_system_prompt_contains_environment_and_no_secret`, `g01_agents_md_chain_is_loaded_root_to_cwd_in_order`, `g01_agents_md_over_32k_is_truncated_with_notice`, `g01_agents_md_cannot_grant_tools` (nội dung "allow run_shell" không đổi policy), `g01_claude_md_is_fallback_only`.

Tiêu chí đóng: `interactive_session` + `interactive_launch` xanh; packet trong store có block `ProjectRule` với digest đúng; `/status` hiện số file hướng dẫn.

### G02 — Config v2, lớp và precedence (M)

Phụ thuộc: không (G01 có thể chạy song song). Mục tiêu: một `config.toml` thay cho rừng biến môi trường.

1. `harness-types/src/contracts.rs`: `HarnessConfig` schema_version 2 với `[provider]` (`id`, `protocol = "openai_chat" | "anthropic_messages"`, `endpoint`, `model`, `api_key_env`, `thinking`), `[profiles.<name>]` (ghi đè provider/limits), `[models.<name>]` (`context_window`, `input_price_per_mtok`, `output_price_per_mtok`, `supports_images`), `[limits]` (`max_steps`, `max_tool_calls`, `deadline_seconds`, `continuations`, `output_reservation_tokens`, `compaction_reserve_tokens`), `[permissions]` (`mode`, `allow`, `deny`), `[mcp_servers.<name>]`, `[hooks.<event>]`, `[ui]` (`renderer`, `color`), `[trust] projects = [...]`. Mọi khoá optional; v1 load được không đổi nghĩa (test compat với fixture v1 hiện có).
2. `interactive/config.rs`: nạp user config, `.harness/config.toml` (chỉ khi root ∈ `trust.projects`), `.harness/config.local.toml`; áp env (`HA_*`, `DEEPSEEK_API_KEY`) rồi CLI (`--model`, `--profile`, `--approval`). Kết quả là `ResolvedConfig` bất biến cho cả lượt; `ConfigExplain` (M6-04) ghi nguồn từng khoá.
3. `ha config explain [--json]` và `/config` overlay in bảng khoá → giá trị → lớp; secret luôn ẩn. `/trust` thêm root hiện tại vào user config sau xác nhận; project config nạp từ lượt sau.
4. `bounds.rs` đọc từ `ResolvedConfig`; env `HA_TURN_*` vẫn thắng (ghi deprecation notice một lần).
5. Schema: `cargo run -p harness-types --bin generate_schemas --locked` sinh `harness-config.v2.schema.json`; giữ v1 file; `p0_f03` xanh.
6. Test: `g02_v1_config_still_loads_unchanged`, `g02_precedence_cli_over_env_over_project_over_user`, `g02_untrusted_project_config_is_ignored_with_reason`, `g02_config_explain_names_the_layer_for_every_key`, `g02_unknown_key_is_rejected_with_path`.

Tiêu chí đóng: không còn đường đọc `HA_PROVIDER_*` ngoài `config.rs`; `ha config explain` không in `not_available_in_p1`.

### G03 — Provider: OpenAI-chat tổng quát, Anthropic, `/model`, thinking, chi phí (L)

Phụ thuộc: G02.

1. Tách `DeepSeekAdapter` thành `OpenAiChatAdapter { provider_id, endpoint, model, capabilities }`; DeepSeek là một cấu hình mặc định của nó (`provider_id = "deepseek"` khi không đặt) — mọi test M2 giữ xanh. `thinking` từ config: `off` → giữ `disabled`; `on` → không gửi field hoặc gửi theo provider; delta `reasoning_content` được stream vào một `SessionEvent::ThinkingDelta` hiển thị mờ/thu gọn (Ctrl-T bật/tắt), **không** vào transcript plain mặc định.
2. `anthropic.rs`: `AnthropicMessagesAdapter` — `x-api-key`, `anthropic-version` pin (ghi vào SPEC), `system` tách riêng, `tools` + `tool_use`/`tool_result` mapping sang `ProviderStreamEvent` hiện có (`ToolCallDelta` theo `input_json_delta`), `max_tokens` từ `output_reservation_tokens`, SSE `message_start…message_stop`, `usage` từ `message_delta`. Fixture loopback cho: text, tool call, 429 + `retry-after` HTTP-date, cắt stream giữa chừng.
3. `Retry-After` HTTP-date (RFC 7231) parse + cap nâng lên 30 s có config (`limits.max_retry_after_seconds`) — đóng L5.
4. `/model [name|provider/name]`: đổi model cho **các lượt sau** của session hiện tại (persist vào store dạng session setting, `STORE_SCHEMA_VERSION` 1→2 additive: bảng `session_settings`); không đổi giữa lượt đang chạy. Picker khi không tham số: liệt kê `[models.*]` + model hiện tại.
5. `cost.rs`: usage mỗi lượt (đã có từ provider hoặc estimator) × giá trong `[models.<name>]` → `$` tích luỹ session; status bar `12.4k/200k ctx · $0.012`; `/cost` overlay theo lượt; thiếu giá thì hiện `n/a`, không đoán.
6. `scripts/Smoke-HaProvider.ps1` nhận `-Protocol`/`-Endpoint`/`-Model` — vẫn không chạy trong gate.
7. Test: `g03_openai_chat_adapter_keeps_m2_wire_format` (snapshot request bytes), `g03_anthropic_stream_maps_tool_use_to_tool_call_delta`, `g03_anthropic_429_http_date_is_bounded`, `g03_model_switch_applies_next_turn_only`, `g03_cost_uses_config_prices_or_na`, `g03_thinking_delta_never_enters_plain_transcript`.

Tiêu chí đóng: `milestone_m2` + `providers` xanh; header hiện `provider/model`; fixture Anthropic chạy trọn một lượt tool-call qua `TurnDriver`.

### G04 — Bộ tool sửa file, glob, regex, diff preview (M)

Phụ thuộc: không (nên làm sau G02 để `similar` và pins vào cùng lượt).

1. `contracts.rs`: thêm `write_file{path, content, expected_hash?}` (tạo mới khi không có `expected_hash`, ghi đè khi hash khớp), `edit_file{path, old_string, new_string, replace_all?}` (old_string phải xuất hiện đúng một lần trừ `replace_all`; lỗi typed `EditAmbiguous{count}` / `EditNotFound`), `glob{pattern, path?}` (globset, tôn trọng `.gitignore` qua `ignore`, trần 4096), `search_text` thêm `regex: bool`, `case_insensitive`, `glob`, `context_lines` (regex crate, trần match 512 giữ nguyên), `read_file` thêm `offset`, `limit` (theo dòng, có số dòng). `TOOLS_SCHEMA_VERSION` 2→3 additive; `apply_patch` giữ nguyên cho compat.
2. `workspace.rs`: mọi tool mới đi qua cùng `validate_workspace_action` (protected/escape); receipt ghi `before_hash`/`after_hash` cho write/edit (đã có cho patch) — G08 dùng cho `/undo`.
3. Approval panel cho mutating tool hiện **diff** (unified, ≤ 40 dòng, `similar`) thay vì chỉ summary; plain mode in `[diff]` block.
4. System prompt (G01) ưu tiên `edit_file` hơn `apply_patch`.
5. Test: `g04_edit_file_requires_a_unique_match`, `g04_write_file_new_needs_no_hash_but_overwrite_does`, `g04_glob_respects_gitignore_and_cap`, `g04_regex_search_is_bounded`, `g04_read_file_range_has_line_numbers`, `g04_mutating_receipt_has_before_after_hash`, `g04_approval_panel_shows_diff` (TestBackend).

Tiêu chí đóng: `milestone_m4` + `phase_p3` xanh; 18 tool trong `capabilities`; OPERATOR_GUIDE bảng tool cập nhật ở G14.

### G05 — Chế độ quyền, rule allow/deny, "luôn cho phép", trust (M)

Phụ thuộc: G02, G04.

1. `policy.rs`: `PolicyMode { Ask, AutoEdit, FullAuto }`; rule `tool(pattern)` với glob trên đối số chính (`path` cho file tool, `executable + args` join cho process, `command` cho shell, tên tool cho MCP/extension); `ToolPolicy::decide(action) -> Decision::{Blocked(reason), Deny(rule), Allow(rule|mode), Ask}` theo thứ tự D4; `revision` tăng khi rule đổi (M6-04 revalidate dùng được).
2. `permissions.rs`: dựng `ToolPolicy` từ `ResolvedConfig`; panel thêm phím `A` → hiện pattern đề xuất (ví dụ `run_shell(cargo test *)`) → Enter xác nhận → ghi `[permissions] allow` vào `.harness/config.local.toml` (tạo + thêm vào `.gitignore` của `.harness` nếu có) → áp dụng ngay cho lượt này; deny rule không có UI, chỉ config.
3. `/permissions` overlay: mode, rule theo lớp, đếm action đã tự cho phép trong session; `/mode ask|auto-edit|full-auto` đổi cho session (không ghi file). Transcript ghi `[info] allowed by rule <rule>: <action>` / `allowed by mode auto-edit: <action>` — không có gì chạy mà không để dấu (giữ luật của `a`).
4. Headless: `--approval ask|auto-edit|full-auto` (mặc định `ask` = fail-closed như nay); `--allowed-tools`/`--disallowed-tools` là rule tạm cho lượt.
5. Test: `g05_protected_path_beats_every_allow_rule_and_mode`, `g05_deny_rule_beats_allow_rule`, `g05_auto_edit_never_auto_runs_process_or_shell`, `g05_always_allow_writes_a_rule_only_after_confirmation`, `g05_rule_pattern_matches_args_not_tool_name_only`, `g05_headless_default_still_fails_closed`, `g05_transcript_records_every_auto_allowed_action`; h05 (deny/expire) của `interactive_session` giữ xanh.

Tiêu chí đóng: không đường nào tự cấp approval ngoài `Decision::Allow` có lý do ghi được; PTY case `A` ở G14.

### G06 — `ask_user`, hàng đợi khi đang chạy, Esc ngắt, `@` mention, `!` shell (M)

Phụ thuộc: G04, G05.

1. Tool `ask_user{question, options?}` (EffectClass mới `Interactive`, không cần approval): `TurnDriver` → `HumanInputService::ask` → `TurnStop::NeedsInput{question_id}`; TUI mở panel câu hỏi (options bằng phím số, hoặc gõ chữ), câu trả lời → `HumanInputService::answer` → lượt tiếp tục tự động (một `input.admitted` mới cùng task, giữ luật một input/session). Headless: dừng với `pending_question` như hiện có; `ha input answer` + `--resume` tiếp tục.
2. Enter khi `Running`: **xếp hàng** thay vì từ chối — hiển thị `queued (1)` ở status; gửi thành lượt kế tiếp ngay khi `RunTerminal`; `/steer <text>` gửi tức thì qua `RunInbox::steer` (gắn inbox cho interactive như headless). Ctrl-C khi có hàng đợi: xoá hàng đợi trước, lần hai mới hủy run.
3. Esc khi `Running`: hủy run (cùng đường Ctrl-C) — cập nhật quyết định T (§5.2 SPEC HA_TUI) có ghi lý do "table-stakes cả ba tham chiếu". Esc ở Approval vẫn không từ chối.
4. `@`: gõ `@` mở picker file (walker `ignore`, tối đa 4096 entry, fuzzy substring, ↑↓ Enter Esc); chọn → chèn path vào composer; pipeline attachments hiện có nạp nội dung (giữ trần 4 file/1 MiB). `@server:resource` dành cho G10.
5. `!cmd`: chạy `cmd` qua `run_shell` **với cùng policy/approval** (không đường tắt), output (≤ 64 KiB) thành một attachment block `[shell] cmd` trong message kế tiếp; `!!cmd` chỉ hiện output, không gửi model.
6. Test: `g06_ask_user_stops_the_turn_and_answer_resumes_it`, `g06_enter_while_running_queues_and_sends_after_terminal`, `g06_steer_reaches_the_driver_mid_run`, `g06_esc_cancels_a_running_turn_but_not_an_approval`, `g06_at_picker_inserts_a_workspace_relative_path`, `g06_bang_prefix_goes_through_the_same_approval_gate`.

Tiêu chí đóng: i05/i07 PTY cũ xanh; PTY mới `g06_pty_ask_user_panel`, `g06_pty_at_picker` ở G14.

### G07 — Compaction thật, cửa sổ ngữ cảnh theo model, `/context` (M)

Phụ thuộc: G02, G03.

1. `ModelSummaryProvider` trong `harness-runtime`: gọi provider hiện tại với prompt tóm tắt cố định (mục tiêu, việc đã làm, file đã chạm, quyết định, việc còn lại); token trần cho summary từ config; lỗi/timeout → fallback deterministic hiện có; packet ghi `summary_source`. Không gọi provider khi `--mock`/`--fixture`.
2. `/compact [hướng dẫn]` gọi `RuntimeService::compact` (CAS đã có); `[info] compacted: <before>→<after> tokens (model|fallback)`.
3. Auto-compaction theo D6 trước khi dispatch; `context_window` theo model (`[models]` → bảng nội bộ → 8192 + notice). Xoá hằng 8192 ở `RuntimeConfig::default()` khỏi đường `ha chat` (giữ cho test fixture).
4. `/context` overlay: bảng channel → block → token (từ `ContextBuildResult`), block bị drop và lý do; đóng L17: contribution memory bị từ chối thì `Notice` thay vì im lặng.
5. Test: `g07_compact_uses_the_model_and_records_the_source`, `g07_compact_falls_back_when_the_provider_fails`, `g07_auto_compaction_triggers_at_threshold_and_never_loops`, `g07_context_window_comes_from_config_then_table_then_default_with_notice`, `g07_rejected_memory_contribution_emits_a_notice`; `milestone_m5` giữ xanh (CAS).

### G08 — Session UX: `--continue`, `/rename`, `/clear`, `/diff`, `/undo`, `/export`, `/copy` (M)

Phụ thuộc: G04 (before/after hash), G03 (session settings table).

1. Session title: tự đặt từ 60 ký tự đầu của input đầu tiên; `/rename <tên>`; picker `/resume` hiện `title · model · thời gian · số lượt`; `ha chat --continue` = resume session mới nhất của project; `ha sessions list --cwd` dùng project store.
2. `/clear` = `/new` + xoá viewport (scrollback giữ); `/diff` = overlay `git diff` (tái dùng `git_diff` tool path, read-only, không approval theo rule read-only) với hai chế độ: từ đầu session (ghi `HEAD` + hash cây lúc session bắt đầu) và uncommitted.
3. `/undo`: liệt kê file mutating tool đã chạm trong **lượt gần nhất** (từ receipts: path, before_hash, after_hash); mỗi file hiện hash hiện tại; chỉ khôi phục khi hash hiện tại == after_hash (khác → bỏ qua có lý do); bản trước lấy từ artifact `before` được ghi kèm receipt (G04 phải lưu nội dung trước khi sửa vào artifact store, trần 1 MiB/file); `/undo` là **action riêng** đi qua approval như mutating tool (master plan §6), có receipt riêng. Không phải rollback conversational của M5.
4. `/export [path]` ghi transcript JSONL (event đã fold, không secret) hoặc `.md`; `/copy` đưa câu trả lời cuối vào clipboard (arboard đã có); plain mode `/copy` in thông báo không hỗ trợ.
5. Test: `g08_continue_picks_the_newest_session_of_this_project_only`, `g08_rename_persists_and_shows_in_the_picker`, `g08_undo_restores_only_files_whose_hash_is_unchanged`, `g08_undo_is_an_approved_action_with_its_own_receipt`, `g08_diff_since_session_start_uses_the_recorded_base`, `g08_export_contains_no_credential_values`.

### G09 — Hooks và thông báo (S/M)

Phụ thuộc: G02, G05.

1. `hooks.rs`: `[hooks.<event>] = [{ matcher = "run_shell|run_process", command = "...", timeout_seconds = 60 }]`; JSON stdin gồm `event, session_id, task_id, tool{name,args_digest,args}` (args cắt 8 KiB), `cwd`; `pre_tool_use` exit 2 → action bị chặn kèm lý do ghi vào transcript và receipt `blocked_by_hook`; `post_tool_use` chỉ quan sát; `stop` nhận tóm tắt run; `notification` khi cần approval hoặc hỏi người dùng. Hook chạy qua `harness-tools::process` (cùng env allowlist, timeout cứng) — không `std::process` trần.
2. Chỉ nạp hook từ user config và project config đã trust; `/hooks` overlay liệt kê + nguồn; hook lỗi/timeout ở `pre_tool_use` = chặn (fail-closed) và notice.
3. Thông báo: `[ui] bell = true` phát BEL khi cần approval/ask_user/turn end trong lúc terminal không focus (không đo được focus → luôn phát khi cấu hình bật); `notify_command` chạy như hook `notification`.
4. Test: `g09_pre_tool_use_exit_2_blocks_and_records_the_reason`, `g09_hook_cannot_turn_ask_into_allow`, `g09_hook_timeout_blocks_not_allows`, `g09_untrusted_project_hooks_do_not_run`, `g09_hook_receives_bounded_json_without_secrets`.

### G10 — MCP trong `ha chat` (M) · G10b Streamable HTTP (được chọn theo assignment)

Phụ thuộc: G02, G05.

1. `mcp.rs`: `[mcp_servers.<name>] command, args, env (chỉ `secret://` ref hoặc literal không nhạy cảm), cwd, enabled_tools, disabled_tools, tool_timeout_seconds ≤ 120, required`; khởi động **lười** ở lượt đầu cần, `unload_draining` khi thoát/`/mcp restart`; `required = true` mà server hỏng → lượt lỗi rõ, không im lặng.
2. Tool `mcp__<server>__<tool>` đăng ký qua `McpToolDispatcher` → `ToolService` (schema validate M6-03, policy + approval G05 như extension tool); resource `@<server>:<uri>` trong composer → `read_resource` → attachment (text only; blob bị từ chối như M6).
3. `ha mcp add|list|get|remove` ghi user config (hoặc `--project` vào `.harness/config.toml`); `/mcp` overlay: server, trạng thái, số tool, lỗi cuối.
4. Đóng L20: khi test P6 chuyển sang gọi qua dispatcher, `McpClient::call_tool` → `pub(crate)`; thêm negative control N8 riêng (lease generation) vào `milestone_m6`.
5. **G10b (assignment chọn mọi entry hiện `unsupported` trong matrix):** `transport = "streamable_http"`, `url` https hoặc loopback, `bearer_token_env`; bật `transport-streamable-http-client-reqwest`; `cargo tree -i reqwest` phải có một phiên bản. Sau triển khai, cả 9 entry trong `McpSupportMatrix` được thực thi và kiểm chứng: tools/resources/resource templates/prompts/sampling/elicitation/subscriptions/remote transport/tasks. SSE/OAuth, sampling kèm tools/ảnh, multi-round và tự mở rộng RFC 6570 URI vẫn là giới hạn tường minh. Fixture: dùng MCP server thật chạy loopback.
6. Test: `g10_mcp_tool_goes_through_the_dispatcher_and_the_approval_gate`, `g10_required_server_failure_fails_the_turn_loudly`, `g10_disabled_tool_is_not_advertised`, `g10_resource_mention_becomes_a_text_attachment`, `g10_unload_drains_before_exit`, `a23_extension_bounds` giữ xanh; G10b: `g10b_streamable_http_uses_one_reqwest_and_bearer_from_env`.

### G11 — Skills và prompt template trong chat (M)

Phụ thuộc: G01, G02.

1. `skills.rs`: root discovery `<config-dir>/skills`, `~/.agents/skills`, `.agents/skills` và `.harness/skills` (hai root project chỉ khi trust); mỗi skill là thư mục có `SKILL.md` (front matter `name`, `description`, `version`, tuỳ chọn `requested_tools`) — tương thích catalog M6-01 (`TrustedSkillRoot`, digest). System prompt liệt kê **tên + description** (≤ 2 KiB tổng, progressive disclosure); tool `activate_skill{name}` nạp body theo digest vào channel `Skill` cho các step còn lại của lượt và lượt sau cùng session; `/skill:<name> [args]` activate từ composer; `/skills` overlay.
2. `commands.rs`: `.harness/commands/*.md` và `<config-dir>/commands/*.md` → `/name args`; thay `$ARGUMENTS`, `$1..$9`; front matter `description`, `argument-hint`; hiện trong slash menu (Tab hoàn thành); `/reload` nạp lại skills/commands/AGENTS.md không thoát app.
3. Test: `g11_skill_metadata_is_in_the_prompt_but_body_is_not_until_activated`, `g11_activate_skill_verifies_the_digest`, `g11_project_skills_require_trust`, `g11_command_template_substitutes_arguments`, `g11_reload_picks_up_a_new_command_without_restart`; `a22_skill_version` giữ xanh.

### G12 — Subagent trong lượt (bridge M8) (M/L)

Phụ thuộc: G03, G05; phối hợp M8.

1. `WorkerBackend` thật trong `harness-orchestrator`: mỗi worker là một `TurnDriver` + `ToolService` với provider từ config (có thể profile riêng `[profiles.explorer]`), `TaskBrief` bất biến, budget con trừ vào ledger cha (đã có `BudgetLedger`).
2. Tool `delegate{role: explorer|coder, brief, max_steps?}`: `explorer` chỉ có tool read-only, chạy trên cùng workspace, không worktree; `coder` được cấp worktree M8-03 từ snapshot Git sạch, ghi worktree record và giữ lại branch/path để review; input bẩn hoặc không tạo được worktree trả typed `RoleUnavailable{reason}` (không chạy trên checkout người dùng, không stub). Depth 1 (child không được delegate), tối đa 3 song song, kết quả = `TaskResult` (text + receipts digest + worktree metadata) làm tool result; approval của child hiện lên panel cha với nhãn `[child <role>]`.
3. `/agents` overlay: child đang chạy, bước, chi phí; Ctrl-C cha hủy child (cancel token đã có).
4. `ha tasks run` bỏ `ScriptedWorkerBackend` khi có provider thật; giữ `--mock` cho fixture.
5. Test: `g12_explorer_child_cannot_call_mutating_tools`, `g12_child_cannot_delegate_again`, `g12_child_budget_is_charged_to_the_parent_ledger`, `g12_parent_cancel_stops_the_child_within_grace`, `g12_coder_gets_a_real_m8_worktree_for_a_clean_workspace`, `g12_coder_refuses_a_dirty_parent_without_losing_changes`.

**CP-D implementation note:** `/agents`, explorer và coder backend được nối trong interactive chat bằng `WorkerScheduler` + `WorkspaceManager` M8-03 + `WorkerBackend`/`TurnDriver`/`ToolExecutionService` với provider của lượt cha. Explorer chỉ có read tools; coder chỉ chạy trong worktree do host tạo từ Git snapshot sạch và record được lưu bền vững. `RoleUnavailable` chỉ còn khi workspace bẩn hoặc host không thể cấp worktree an toàn. `/skills`/`/skill:<name>` và model tool `activate_skill` cùng nạp nội dung theo digest vào Skill channel. Gate/test status của CP-D nằm trong `docs/handoffs/HA_AGENT.vi.md` và `docs/evidence/M6.vi.md`.

### G13 — Headless và automation (S/M)

Phụ thuộc: G05, G06.

1. `ha exec "<prompt>"` = alias `ha chat --headless --prompt`; `--prompt -` đọc stdin (tường minh, trần 10 MiB); `--output-format text|json|stream-json` (`json` = envelope hiện có schema 1 + các trường mới additive; `stream-json` = NDJSON `{"type": "turn.started|text.delta|thinking.delta|tool.started|tool.settled|approval.blocked|ask_user|run.terminal|usage", …}` mỗi dòng); `--continue`; `--max-turns` (map sang continuations); `--approval`, `--allowed-tools`, `--disallowed-tools` (G05).
2. Exit code theo [CONTRACTS](implementation-next/CONTRACTS.vi.md): 0 xong; 2 usage; 3 đang chờ (`pending_question`/approval bị chặn); 4 thất bại; 5 xung đột ownership; 130 hủy — i03 headless cũ giữ xanh.
3. Đóng L13: `--goal` satisfied → phát `AcceptanceCommand` (M3 contract) và ghi vào JSON `acceptance.command_id`.
4. Test: `g13_stream_json_emits_one_event_per_line_in_order`, `g13_stdin_prompt_requires_the_explicit_dash`, `g13_exit_code_3_when_the_model_asks`, `g13_headless_never_emits_ansi` (giữ), `g13_goal_satisfied_emits_acceptance_command`.

### G14 — Đóng giới hạn nền tảng, gate, PTY, docs (M)

Phụ thuộc: G01–G13 (bước 1–3 có thể làm sớm, song song).

1. **Bước 0:** xác minh L26 (`a5ac2e2` có còn hỏng trên origin) và ghi kết quả; xác minh `Verify-HaLaunch.ps1 -Json` xanh ở HEAD trước khi sửa gì.
2. CI: [`ci.yml`](../.github/workflows/ci.yml) thêm job ubuntu chạy `Verify-Milestone.ps1 -Milestone M4`, `M5`, `M6` và `Verify-HaLaunch.ps1 -Json` (không PTY); artifact log; job windows tương tự. Kết quả xanh trên ubuntu là bằng chứng để reviewer đổi A12–A24 sang `accepted` — implementer **không** tự đổi registry.
3. `run_shell` Windows: `pwsh` → fallback `powershell.exe` với cùng flag, receipt ghi `shell: powershell-5.1 (pwsh not found)`; test giả lập `PATH` không có `pwsh`.
4. Flake M2: dựng lại `FakeProvider` theo từng attempt (mỗi attempt một listener/response tách biệt), chạy `milestone_m2` 10 lần liên tiếp phải 10/10.
5. `StorePort`: implement cho `SqliteStore` (adapter mỏng gọi các method hiện có) **hoặc** ADR-N12 gỡ port khỏi `harness-types` với lý do; không để trạng thái "contract-only" tồn tại sau G14.
6. Sửa nhỏ đã ghi: help footer `↑↓` (L16), `/key` giữ nguyên chuỗi có khoảng trắng, `ha memory|tasks|maintenance` nhận `--cwd` để tìm project store như `ha chat`.
7. PTY: thêm vào `Invoke-HaPtyAcceptance.ps1` các ca `g05_pty_always_allow_writes_local_rule`, `g06_pty_ask_user_panel`, `g06_pty_at_picker`, `g06_pty_bang_prefix_needs_approval`, `g08_pty_undo_panel`, `t_key_pty_key_with_spaces`, `t_more_pty_scroll_keys`; toàn bộ ca cũ + mới xanh trong **một** lần chạy.
8. Gate: `Verify-HaLaunch.ps1` thêm bước `unit-agent` và ≥ 8 selector bắt buộc G (`g01_agents_md_cannot_grant_tools`, `g02_precedence_cli_over_env_over_project_over_user`, `g04_edit_file_requires_a_unique_match`, `g05_protected_path_beats_every_allow_rule_and_mode`, `g06_bang_prefix_goes_through_the_same_approval_gate`, `g07_auto_compaction_triggers_at_threshold_and_never_loops`, `g09_hook_cannot_turn_ask_into_allow`, `g10_mcp_tool_goes_through_the_dispatcher_and_the_approval_gate`); `cargo audit`/`cargo deny` thêm vào CI **chỉ khi** cài được `--locked` với version ghi trong SPEC, ngược lại ghi `not_run` có lý do.
9. Docs: OPERATOR_GUIDE vi+en mục 12 viết lại bảng tool (18), slash (đầy đủ), config v2, permissions, hooks, MCP, skills, headless; README đoạn "Status" cập nhật (M0–M6 gate xanh, track G); `docs/evidence/HA_AGENT.vi.md` + `docs/handoffs/HA_AGENT.vi.md` theo templates; `Verify-Docs.ps1 -SelfTest` xanh. Linux x64 bundle: CI ubuntu build `--release` và upload artifact (không publish).
10. Không cài lên máy user, không đổi PATH, không publish, không paid smoke nếu assignment không cấp.

## 7. Acceptance V01–V32

| ID | Điều phải đúng | Oracle | Test dự kiến (planned, chưa tồn tại) |
|---|---|---|---|
| V01 | System prompt có OS/cwd/git/ngày/giới hạn, không có secret | packet trong store; grep giá trị key | `g01_system_prompt_contains_environment_and_no_secret` |
| V02 | `AGENTS.md` toàn cục → root → cwd, đúng thứ tự, trần 32 KiB có notice | block `ProjectRule` + digest | `g01_agents_md_chain_is_loaded_root_to_cwd_in_order`, `g01_agents_md_over_32k_is_truncated_with_notice` |
| V03 | Nội dung `AGENTS.md` không đổi policy tool | action vẫn bị hỏi/chặn | `g01_agents_md_cannot_grant_tools` |
| V04 | Config v1 load nguyên; v2 precedence CLI > env > project(trust) > user > default | `ConfigExplain` | `g02_v1_config_still_loads_unchanged`, `g02_precedence_cli_over_env_over_project_over_user` |
| V05 | Project chưa trust: config/hook/skill của project không nạp, có lý do | explain + notice | `g02_untrusted_project_config_is_ignored_with_reason`, `g09_untrusted_project_hooks_do_not_run`, `g11_project_skills_require_trust` |
| V06 | OpenAI-chat adapter giữ wire format M2 byte-identical | snapshot request | `g03_openai_chat_adapter_keeps_m2_wire_format` |
| V07 | Anthropic adapter chạy trọn lượt tool-call qua `TurnDriver` với fixture | receipts + response | `g03_anthropic_stream_maps_tool_use_to_tool_call_delta` |
| V08 | `/model` đổi từ lượt sau, persist trong session | store `session_settings` | `g03_model_switch_applies_next_turn_only` |
| V09 | Chi phí từ giá config, thiếu giá thì `n/a` | `/cost` overlay | `g03_cost_uses_config_prices_or_na` |
| V10 | Thinking delta không vào transcript plain | scan transcript | `g03_thinking_delta_never_enters_plain_transcript` |
| V11 | `edit_file` một match; `write_file` mới không cần hash, ghi đè cần | typed errors | `g04_edit_file_requires_a_unique_match`, `g04_write_file_new_needs_no_hash_but_overwrite_does` |
| V12 | glob/regex bị trần và tôn trọng `.gitignore` | đếm kết quả | `g04_glob_respects_gitignore_and_cap`, `g04_regex_search_is_bounded` |
| V13 | Panel approval mutating hiện diff | TestBackend | `g04_approval_panel_shows_diff` |
| V14 | Protected path thắng mọi allow/mode; deny thắng allow | không receipt | `g05_protected_path_beats_every_allow_rule_and_mode`, `g05_deny_rule_beats_allow_rule` |
| V15 | `auto-edit` không tự chạy process/shell/MCP | panel vẫn hiện | `g05_auto_edit_never_auto_runs_process_or_shell` |
| V16 | `A` chỉ ghi rule sau xác nhận, vào `.harness/config.local.toml` | file + gitignore | `g05_always_allow_writes_a_rule_only_after_confirmation`, PTY `g05_pty_always_allow_writes_local_rule` |
| V17 | Mọi action tự cho phép có dòng `[info] allowed by …` | transcript | `g05_transcript_records_every_auto_allowed_action` |
| V18 | `ask_user` dừng lượt, trả lời tiếp tục; headless exit 3 | question record | `g06_ask_user_stops_the_turn_and_answer_resumes_it`, `g13_exit_code_3_when_the_model_asks` |
| V19 | Enter khi chạy = queue; `/steer` tới driver; Esc hủy run, không hủy approval | controller + inbox | `g06_enter_while_running_queues_and_sends_after_terminal`, `g06_steer_reaches_the_driver_mid_run`, `g06_esc_cancels_a_running_turn_but_not_an_approval` |
| V20 | `!cmd` qua cùng cổng approval | receipt + panel | `g06_bang_prefix_goes_through_the_same_approval_gate`, PTY `g06_pty_bang_prefix_needs_approval` |
| V21 | `/compact` dùng model, ghi nguồn; lỗi → fallback | packet `summary_source` | `g07_compact_uses_the_model_and_records_the_source`, `g07_compact_falls_back_when_the_provider_fails` |
| V22 | Auto-compaction ở ngưỡng, không lặp; cửa sổ theo model | đếm compaction | `g07_auto_compaction_triggers_at_threshold_and_never_loops`, `g07_context_window_comes_from_config_then_table_then_default_with_notice` |
| V23 | `/undo` chỉ phục hồi file hash chưa đổi, là action có approval + receipt | hash + receipt | `g08_undo_restores_only_files_whose_hash_is_unchanged`, `g08_undo_is_an_approved_action_with_its_own_receipt` |
| V24 | `--continue` chỉ chọn session của project này | store | `g08_continue_picks_the_newest_session_of_this_project_only` |
| V25 | Hook exit 2/timeout = chặn; hook không thể cấp | receipt `blocked_by_hook` | `g09_pre_tool_use_exit_2_blocks_and_records_the_reason`, `g09_hook_cannot_turn_ask_into_allow`, `g09_hook_timeout_blocks_not_allows` |
| V26 | MCP tool đi qua dispatcher + approval; server `required` hỏng = lỗi rõ | receipt + error | `g10_mcp_tool_goes_through_the_dispatcher_and_the_approval_gate`, `g10_required_server_failure_fails_the_turn_loudly` |
| V27 | Skill: metadata trong prompt, body chỉ khi activate theo digest | packet | `g11_skill_metadata_is_in_the_prompt_but_body_is_not_until_activated`, `g11_activate_skill_verifies_the_digest` |
| V28 | Explorer child không mutating, không delegate lại, tính vào ledger cha | receipts + ledger | `g12_explorer_child_cannot_call_mutating_tools`, `g12_child_cannot_delegate_again`, `g12_child_budget_is_charged_to_the_parent_ledger` |
| V29 | `stream-json` một event/dòng đúng thứ tự, không ANSI | parse NDJSON | `g13_stream_json_emits_one_event_per_line_in_order`, `g13_headless_never_emits_ansi` |
| V30 | Gate M4–M6 và gate H/T xanh trên **ubuntu** CI | CI log | job `milestone-gates-ubuntu` |
| V31 | `milestone_m2` 10/10 lần xanh; `run_shell` chạy khi không có `pwsh` | log 10 lần; test PATH | `a07_401_is_not_retried_and_transient_is_bounded` ×10, `g14_run_shell_falls_back_to_powershell_5` |
| V32 | Toàn bộ PTY cũ + mới xanh một lần chạy; operator guide + README đúng với code | `PTY_EXIT: 0`; `Verify-Docs -SelfTest` | `Invoke-HaPtyAcceptance.ps1` |

## 8. Hợp đồng, schema và migration bị chạm

| Hợp đồng | Trước | Sau | Item |
|---|---|---|---|
| `HarnessConfig` / `harness-config` schema | v1 (`[cli]`) | v2, additive, v1 vẫn load; file JSON v2 sinh bằng `generate_schemas` | G02 |
| `TOOLS_SCHEMA_VERSION` | 2 | 3 (thêm tool, thêm field optional; `apply_patch` giữ) | G04 |
| `STORE_SCHEMA_VERSION` | 1 | 2 (bảng `session_settings`: title, model, mode; migration additive qua hạ tầng M1; host cũ từ chối DB mới như luật hiện có) | G03/G08 |
| `EffectClass` | read_only/mutating/external | + `interactive` (`ask_user`, `delegate`) | G06/G12 |
| Headless JSON | schema 1 | schema 1 + trường additive; `stream-json` là format riêng | G13 |
| `EXTENSION_PROTOCOL_VERSION`, `MCP_SPEC_REVISION`, `CONTEXT_SCHEMA_VERSION` | 1 / 2026-07-28 / 1 | **không đổi** (G10b nếu làm: ghi số đo, không đổi revision trừ khi rmcp yêu cầu) | — |
| `error-report.v1.schema.json` | — | có thể thêm `ErrorCode` (`EditAmbiguous`, `RoleUnavailable`, `BlockedByHook`) — regenerate + fixture compat | G04/G09/G12 |
| Dependency allowlist | 45 edge | + `harness-cli → harness-orchestrator` đã có; **không** thêm `runtime → tools`; edge mới phải vào `schemas/dependency-allowlist.v1.json` cùng commit | G12 |

## 9. Gate và bằng chứng

- **Gate cục bộ mỗi item:** `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings`; `cargo test -p harness-cli --bin ha --locked`; suite bị ảnh hưởng với `--test-threads=1` (`interactive_session`, `interactive_launch`, `milestone_m2/m4/m5/m6`, `phase_p3/p6`).
- **Gate checkpoint:** `pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json` → `passed: true`, `failures: []`; `pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M6` → `passed` (closure M0–M5 kèm theo); PTY: `pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1 -TimeoutSeconds 900` → `PTY_EXIT: 0`.
- **Flake loopback đã biết** (evidence HA_LAUNCH §22, M6 §5.1): chạy lại tối đa ba lần, ghi từng lần, chỉ nhận lần `failures: []`; **không** sửa test để xanh; G14 mới là nơi sửa gốc.
- **Evidence** theo [TEMPLATES §2](implementation-next/TEMPLATES.vi.md): source digest, OS, lệnh thật, số test kỳ vọng/thực tế, transcript PTY, negative controls (ví dụ: bỏ bước deny-trước-allow thì V14 phải đỏ; hook trả exit 0 với `permission: allow` trong stdout thì V25 phải vẫn hỏi), `not_run` (live smoke, Linux nếu CI chưa chạy, VM sạch).
- **Không nới `--locked`**: dependency mới (`similar`, pin `regex`/`globset`, feature rmcp) kèm `Cargo.lock` cùng commit; version và license ghi vào SPEC.

## 10. Rủi ro và giới hạn nền tảng

| Rủi ro | Cách xử lý |
|---|---|
| Config v2 phá fixture v1 hoặc test P0 drift | V04 + `p0_f03` là selector bắt buộc; v1 file trong `tests/fixtures` không được sửa |
| Rule pattern quá rộng (`run_shell(*)`) làm mất fail-closed | Panel `A` hiển thị pattern hẹp nhất theo executable + subcommand; `*` toàn cục chỉ được viết tay trong config, và transcript vẫn ghi từng action |
| Anthropic API đổi version | Pin `anthropic-version` trong SPEC; fixture loopback theo version pin; live smoke chỉ khi cấp |
| Auto-compaction lặp vô hạn khi summary vẫn vượt ngưỡng | Tối đa một compaction mỗi lượt; nếu vẫn vượt → lỗi typed `ContextOverflow` thay vì lặp (V22) |
| Model gọi `ask_user` liên tục | Đếm vào `max_steps`; ≥ 3 câu hỏi một lượt → `NoProgress` |
| `/undo` phục hồi sai file khi người dùng đã sửa tay | Chỉ khi hash hiện tại == after_hash (V23); còn lại bỏ qua kèm lý do |
| Hook chậm làm treo lượt | Timeout 60 s cứng qua `harness-tools::process`; timeout = chặn |
| MCP server treo khi thoát | `unload_draining` + cancel bounded 2 s của M6-02; outcome `Uncertain` được ghi |
| Child agent tiêu ngân sách cha | Ledger cha reserve trước, V28 |
| Windows không phân biệt Shift+Enter, Ctrl-J = Enter trên ConPTY (đã đo T01) | Không hứa phím mới nào chưa đo; Esc ngắt phải đo PTY trước khi ghi vào help |
| Linux chỉ có qua CI (host không WSL/Docker) | V30 là bằng chứng duy nhất; evidence ghi `platform: linux via CI run <id>` |
| Hai người cùng sửa một working tree (đã xảy ra ở M4/M5/T) | Stage theo path; mỗi assignment một checkpoint; không commit file của session khác |

## 11. Quyền và điểm dừng

Plan này **không tự cấp quyền**. Assignment mặc định cho DeepSeek: đọc/sửa source trong root workspace, thêm dependency đã pin theo D10, build/test local, tạo tiến trình con và thư mục tạm, chạy PTY runner trên máy hiện tại, sửa docs SPEC/evidence/handoff/operator guide. **Không** được nếu assignment không nêu: commit/push, gọi paid API (live smoke), cài lên máy user, ghi User PATH, publish, xoá/nới test đã được chấp nhận, đổi `EXTENSION_PROTOCOL_VERSION`/`MCP_SPEC_REVISION`/`CONTEXT_SCHEMA_VERSION`, đổi registry `tests/acceptance/*.json` sang `accepted`.

Điểm dừng bắt buộc: sau G03 (CP-A), sau G06 (CP-B), sau G09 (CP-C), sau G12 (CP-D), sau G14 (CP-E). Mỗi điểm dừng cập nhật `docs/handoffs/HA_AGENT.vi.md` với exact next action; không chuyển checkpoint khi gate chưa `failures: []` ít nhất một lần.

## 12. Thứ tự giao và checkpoint

| Checkpoint | Phạm vi | Prerequisite | Nhận là đạt khi |
|---|---|---|---|
| CP-A | G01–G03 (ngữ cảnh, config, provider) | H/T evidence hiện có; gate M6 xanh ở HEAD | V01–V10 xanh; `milestone_m2` + `interactive_*` xanh; gate H một lần `failures: []` |
| CP-B | G04–G06 (tools, quyền, tương tác) | CP-A | V11–V20 xanh; h05 + i05/i07 PTY xanh trên TUI |
| CP-C | G07–G09 (compaction, session UX, hooks) | CP-B | V21–V25 xanh; `milestone_m5` xanh (CAS) |
| CP-D | G10–G12 (MCP, skills, subagent) | CP-C; G12 `coder` cần M8-03 | V26–V28 xanh; `milestone_m6` + `phase_p6` xanh; gate M6 `passed` |
| CP-E | G13–G14 (headless, nền tảng, PTY, docs) | CP-D | V29–V32; CI ubuntu xanh; PTY đủ ca một lần chạy; `Verify-Docs.ps1 -SelfTest` xanh |

Ba mốc trải nghiệm để không tuyên bố sớm: **"model hiểu dự án"** (CP-A) chưa phải **"dùng hằng ngày được"** (CP-B + CP-C); chỉ **CP-E** mới là "track G xong". Sau CP-E, `ha` còn thiếu so với tham chiếu: sandbox OS (M12), DAG bền vững + worktree (M8), memory publication/embeddings (M7), backup/release/install smoke (M9), daemon/schedule (M11), web (M10), OAuth login, marketplace, cloud — là việc của các milestone đó, không của track G.
