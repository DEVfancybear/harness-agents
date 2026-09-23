# SPEC HA_AGENT — CP-A record and CP-B addendum / Đặc tả HA_AGENT — hồ sơ CP-A và bổ sung CP-B

**Current assignment / Assignment hiện tại:** G07–G09, checkpoint CP-C. CP-A and CP-B below are historical specifications and implementation records. / Thực hiện G07–G09, checkpoint CP-C. Nội dung CP-A và CP-B bên dưới là đặc tả cùng hồ sơ triển khai lịch sử.

## CP-A original baseline (historical) / Baseline CP-A ban đầu (lịch sử)

Assignment: G01–G03 from docs/HA_AGENT_PLAN.vi.md; no G04 or later.
Source/base: HEAD 37e355dc2cc22a381c7e86ae3231b14c0c354be0, branch master.
Working tree at start: eight pre-existing modified files in interactive memory,
harness-memory, and SQLite memory storage. They are unrelated M7 work and must
remain intact. No commit, push, paid API call, install, or publish is authorized.

Spec approval: not separately obtained. The user supplied the complete assignment
and acceptance constraints; this SPEC records that contract before implementation.

## Bản tiếng Việt / Vietnamese

### Mục tiêu và phạm vi

Thực hiện G01–G03 trong Cargo workspace hiện tại và binary ha. Model nhận system
prompt có thông tin môi trường, luật dùng tool, cùng project rules nạp theo chuỗi
AGENTS.md. Config v2 hợp nhất theo thứ tự default → user → project đã trust → env →
CLI; `config.local.toml` là overlay trong lớp project đã trust. Tách adapter OpenAI
Chat hiện tại thành adapter tổng quát, chuyển cấu hình DeepSeek hiện tại thành preset
DeepSeek độc lập trong config, thêm adapter Anthropic Messages, và đưa config, trust,
model, thinking, cost vào luồng tương tác đang có. Không tạo crate, binary hay runtime mới.

Không làm G04 trở đi, OAuth, MCP, permission mode mới, paid/live smoke, cài đặt,
publish, commit hoặc push. Giữ nguyên: mỗi session nhận một input; approval
fail-closed; protected path bị chặn trước panel; headless không phát ANSI; exit
code hiện hữu. Nội dung AGENTS.md chỉ là hướng dẫn từ user, không thể cấp quyền tool.

### Kiểm kê requirement

Các nhãn disposition giữ nguyên theo template: reuse_verified, adapt, missing,
incompatible. Bảng inventory chi tiết theo từng requirement nằm ở phần English
bên dưới; tóm tắt phân loại:

| Nhóm | reuse_verified | adapt | missing | incompatible |
|---|---:|---:|---:|---:|
| G01 system prompt, context channel, instructions, /init, status | 1 | 4 | 2 | 0 |
| G02 config schema, layers, trust, CLI/env, explain | 0 | 3 | 2 | 0 |
| G03 adapter OpenAI, preset DeepSeek trong config, Anthropic, retry, model/session settings, cost, thinking, fixture | 0 | 5 | 2 | 0 |

Điểm tái sử dụng chính: ContextBlock/ProjectRule và ContentHash; tool gate/protected
path hiện có; provider port, OpenAI wire format, bounded SSE và RetryClass; TOML
strict parser; SQLite migration; ConfigExplain cho extensions. Khoảng trống chính:
prompt/rule producer, layered config resolver, Anthropic adapter, model persistence,
thinking display và cost aggregation.

### Hợp đồng và failure modes

- Config v1 vẫn load nguyên nghĩa và schema v1 tiếp tục được sinh/test. Unknown
  key báo lỗi an toàn kèm đường dẫn, không echo giá trị bị từ chối.
- Không đưa API key vào config, ConfigExplain hay prompt. Project config chỉ hoạt
  động khi canonical project root nằm trong trust list của user; /trust cần xác nhận.
- Nạp AGENTS.md theo thứ tự global → project root → các thư mục tới cwd; trong mỗi
  thư mục CLAUDE.md chỉ là fallback nếu không có AGENTS.md. Tổng tối đa 32 KiB,
  cắt có notice; nội dung có digest và authority User.
- Mọi action tiếp tục qua TurnDriver, ToolService và approval gate hiện tại.
  Project rule không làm đổi policy; protected path vẫn bị từ chối trước panel.
- Migration SQLite schema 1→2 chỉ thêm bảng session_settings; không reset data,
  đổi data home hay đổi exit code.
- OpenAI request bytes phải giữ nguyên. Adapter OpenAI tổng quát không chứa riêng
  provider id, endpoint, model hoặc default thinking của DeepSeek; các giá trị này
  nằm trong preset/config DeepSeek. Anthropic stream bị cắt hoặc tool JSON chưa
  đủ phải lỗi typed, không dispatch tool call chưa hoàn chỉnh. Adapter không retry;
  RuntimeService sở hữu retry và cap.
- Thinking delta hiển thị mờ trong TUI, không vào transcript plain. Thiếu giá thì
  cost hiển thị n/a. Headless tiếp tục ANSI-free.

### Dependency và pin

Không thêm crate/workspace/binary. Parse HTTP-date theo RFC 7231 bằng chrono hiện có,
pin `=0.4.45` (license MIT OR Apache-2.0); dùng reqwest hiện có, pin `=0.12.24`
(license MIT OR Apache-2.0), cho transport. Không thêm httpdate hoặc dependency nào
không được D10 cho phép; không cần similar, regex, globset thuộc G04. Anthropic protocol pin:
anthropic-version 2023-06-01; fixture kiểm header này.

### RED trước GREEN và tên test bắt buộc

Mỗi slice thêm/chỉnh test trước implementation, quan sát assertion fail rồi mới
viết hành vi. Không xóa, skip hoặc nới test cũ.

| Slice | Oracle | Test selector chính xác |
|---|---|---|
| G01 | prompt bounded/không secret; chain, digest, truncation; CLAUDE fallback; rule không đổi policy | g01_system_prompt_contains_environment_and_no_secret; g01_agents_md_chain_is_loaded_root_to_cwd_in_order; g01_agents_md_over_32k_is_truncated_with_notice; g01_agents_md_cannot_grant_tools; g01_claude_md_is_fallback_only |
| G02 | v1 compatibility; CLI > env > project(trust) > user > default; explain có nguồn; unknown key fail closed | g02_v1_config_still_loads_unchanged; g02_precedence_cli_over_env_over_project_over_user; g02_untrusted_project_config_is_ignored_with_reason; g02_config_explain_names_the_layer_for_every_key; g02_unknown_key_is_rejected_with_path |
| G03 | preset DeepSeek tách khỏi adapter tổng quát; OpenAI byte snapshot; Anthropic fixture; retry date có cap; model đổi ở lượt sau; cost/thinking | g03_openai_chat_adapter_keeps_m2_wire_format; g03_anthropic_stream_maps_tool_use_to_tool_call_delta; g03_anthropic_429_http_date_is_bounded; g03_model_switch_applies_next_turn_only; g03_cost_uses_config_prices_or_na; g03_thinking_delta_never_enters_plain_transcript |

### Gate và điểm dừng

Sau implementation chạy fmt, clippy với -D warnings, unit test binary ha, milestone_m2,
interactive_session, interactive_launch, provider unit tests; sinh schema bằng
cargo run -p harness-types --bin generate_schemas --locked và giữ p0_f03 xanh. Chạy
Verify-HaLaunch.ps1 -Json tới một lần xanh; với flake loopback tối đa ba lượt, lưu log
từng lượt, không sửa test. M6 prerequisite gate cũng phải xanh. Không chạy paid API.

Evidence cuối ghi số test thật, digest source, OS/toolchain, dependency/license,
mọi gate attempt, spec approval riêng chưa có, và các mục not_run. Dừng ở CP-A sau
G03, không tiếp tục G04.

## Goal and non-goals / Mục tiêu và ngoài phạm vi

Implement G01–G03 in the existing Cargo workspace and binary ha. The model receives
a bounded environment-aware system policy and project rules. Config v2 resolves
defaults, user config, trusted project config (with `config.local.toml` as its local
overlay), environment, then CLI flags. The existing OpenAI Chat Completions adapter
becomes provider-neutral; its DeepSeek provider id, endpoint, model and thinking
defaults move into a separate DeepSeek config preset. It is joined by an Anthropic Messages adapter. Interactive commands expose config, trust, model,
thinking, and cost behavior. Session model choice is durable through an additive
SQLite migration.

Do not implement G04 or later, a second binary/runtime, a new crate, OAuth, MCP,
permission modes beyond the existing ask/fail-closed behavior, or paid/live smoke.
Do not alter the one-input-per-session rule, exit codes, approval fail-closed
behavior, protected-path checks before the approval panel, or headless ANSI behavior.
Project instruction text never grants tool authority.

## Prerequisites and source boundary

The repository instructions in docs/implementation-next/README.vi.md §§1–5,
HA_AGENT_PLAN.vi.md §§2–5 and G01–G03, HA_TUI.vi.md, M2.vi.md, M6.vi.md §5.6,
and handoffs/CURRENT.vi.md were read. HEAD differs from the plan's survey revision.
The codebase-memory MCP tools and index_repository were not available in this task;
source discovery therefore used the documented fallback search and targeted reads.

Existing source changes to preserve:

| File | Existing change at start |
|---|---|
| crates/harness-cli/src/interactive/headless.rs | 9 lines |
| crates/harness-cli/src/interactive/memory.rs | 26 lines |
| crates/harness-cli/src/interactive/service.rs | 8 lines |
| crates/harness-memory/src/lib.rs | 61 lines |
| crates/harness-memory/src/retrieval.rs | 18 lines |
| crates/harness-store-sqlite/src/models.rs | 4 lines |
| crates/harness-store-sqlite/src/store/memory.rs | 84 lines |
| crates/harness-store-sqlite/src/store/memory/advanced.rs | 8 lines |

## Requirement inventory

Disposition values are evidence labels: reuse_verified, adapt, missing, incompatible.

| Requirement | Existing symbol/path and callers | Existing tests/evidence | Disposition and action |
|---|---|---|---|
| G01 system prompt with role, tool rules, OS/shell/cwd/root/git/date/TurnLimits, ≤2 KiB, no secrets | RunRequest.system_policy in harness-runtime/src/lib.rs; RuntimeConfig default policy; interactive/service.rs constructs each run; tools exposes coding_tool_schemas and TurnLimits | interactive_session, interactive_launch; no prompt contract test | adapt: add SystemPromptBuilder in interactive/prompt.rs; pass only non-secret facts; deterministic byte cap and existing token estimator |
| G01 project rule channel and digest | ContextBlock, ContextChannel::ProjectRule, ContentHash in harness-session/src/context.rs; ContextBuilder called by RuntimeService::build_context | context packet tests in phase_p5/interactive_session | reuse_verified: carry mandatory blocks as ProjectRule, explicit User authority, source provenance and digest |
| G01 global→root→cwd instruction chain, CLAUDE fallback, 32 KiB notice | interactive/paths.rs and interactive/project.rs resolve user paths and project root; no instruction loader | no AGENTS.md loader tests | missing: add interactive/instructions.rs; prefer AGENTS.md per directory; use CLAUDE.md only when AGENTS.md is absent; bound total bytes and report truncation |
| G01 protected instructions are not read and rule text cannot grant tools | harness-tools/src/workspace.rs validates protected paths before approval; ToolService/ChannelApprovalGate are the only action gate | a_read_only_kind_cannot_reach_a_protected_path_through_the_gate; approval tests | adapt: expose/reuse the same protected-path predicate for loader filtering; authority User; test hostile rule text does not change a denied/approval-required action |
| G01 /init static sample before G04 | slash parser/controller in interactive/input.rs and controller.rs; no /init command | slash command catalog tests | missing: add /init as transcript-only fixed template, state that file writing is unavailable until G04 |
| G01 report instruction-file count in header/status | interactive/app.rs, view.rs, tui/widgets/status.rs | interactive_session and TUI widget tests | adapt: expose count without changing transcript landmarks |
| G02 HarnessConfig v2 while v1 remains loadable | harness-types/src/contracts.rs HarnessConfig is strict v1 schema_version+cli; interactive/config.rs loads only user config; p0_f03 compares generated schemas | h02_missing_configuration_is_a_first_run_not_an_error; h02_valid_configuration_loads_and_is_described_with_its_schema_version; p0_f03 | adapt: optional additive v2 fields and v1 compatibility; keep generated v1 schema unchanged and generate v2 through generate_schemas |
| G02 user/project/local config and trust gating | interactive/config.rs, paths.rs, project.rs; extension ConfigExplain in harness-extensions; no general layered resolver | h02 config tests; extension config explain tests | missing: add ResolvedConfig with explicit source and inactive reason; only load .harness/config.toml and config.local.toml for trusted canonical project root; treat local as an overlay within the trusted project layer |
| G02 env then CLI precedence and legacy HA_* variables | interactive/bootstrap.rs, bounds.rs, credentials.rs read HA_PROVIDER_*, HA_TURN_*, DEEPSEEK_API_KEY; main.rs ChatArgs has no model/profile/approval flags | existing bounds, credentials, launch tests | adapt: resolve every supported key default→user→trusted project (local overlay)→env→CLI; keep key values out of config/explain and retain legacy env compatibility with one deprecation notice |
| G02 config explain, /config and /trust | main.rs has config validate/explain requiring a file and reports v1 only; controller /config and /model are static reference output; ConfigExplain exists for extension plugins | config CLI, controller slash command tests, M6-04 config tests | adapt: reuse ConfigExplain vocabulary; add effective ha config explain, live /config, and explicit /trust confirmation that writes only the user trust list |
| G02 unknown key path and v1 compatibility | strict serde config parser in interactive/config.rs and main.rs | h02_unknown_field_and_unsupported_schema_keep_their_codes_and_name_the_path | adapt: preserve safe redaction and typed error; report the config path without echoing rejected values |
| G03 generic OpenAI Chat adapter and separate DeepSeek config preset, identical M2 bytes | DeepSeekAdapter and wire_messages in harness-providers/src/lib.rs; streaming implementation in providers/src/streaming.rs | providers wire_tests and milestone_m2 A06/A07 | adapt: extract OpenAiChatAdapter with provider_id/endpoint/model/capabilities; move existing DeepSeek provider id/endpoint/model/thinking defaults to the DeepSeek config preset; freeze request-byte snapshot before/after |
| G03 Anthropic Messages request/response/SSE/tool mapping | ModelProvider, ProviderRequest, ProviderMessage, ProviderStreamEvent, SseDecoder and reqwest transport in harness-providers | no Anthropic adapter tests | missing: add anthropic.rs reusing ports and bounded streaming; x-api-key; version header 2023-06-01; map text/tool_use/tool_result/max_tokens/usage; loopback fixture covers text, tool call, 429 date and truncation |
| G03 Retry-After RFC 7231 date and configured cap ≤30s | retry_after_seconds in providers/lib.rs parses integer seconds; RuntimeService caps at 2 seconds in runtime/src/lib.rs | milestone_m2 A07 retry tests | adapt: accept both delta-seconds and HTTP-date; default cap 30 seconds, configurable cap; retry stays owned by RuntimeService |
| G03 /model changes only later turns and persists setting | controller.rs /model is read-only; interactive service creates runtime for submitted input; STORE_SCHEMA_VERSION is 1 and has no session_settings | interactive_session resume/model reference tests; migration/reopen tests in harness-store-sqlite | adapt: create additive session_settings migration and port methods; apply selected model only when constructing the next run; never mutate an in-flight request |
| G03 thinking stream and plain transcript exclusion | DeepSeek thinking is disabled in providers; ProviderStreamEvent has text/tool/usage/terminal; interactive events render text history; plain renderer preserves fixed landmarks | M2 wire tests; TUI history/plain transcript tests | missing: add ThinkingDelta transport event; dim TUI display; keep it out of plain transcript and persisted conversational text |
| G03 usage pricing, /cost and status bar | ProviderStreamEvent::Usage and runtime BudgetLedger expose token totals; status widget shows current context; no pricing/cost aggregation | provider usage tests; TUI status tests | adapt: calculate session cost from [models.<name>] per-million input/output prices; use usage or estimator; unknown price is n/a |
| G03 provider fixture and smoke interface | milestone_m2 FakeProvider/loopback harness; scripts/Smoke-HaProvider.ps1 targets current provider | M2 fixture tests; smoke is not part of gates | adapt: add Anthropic loopback fixture through the real adapter; extend smoke options for protocol/endpoint/model, but never run paid smoke |

## Compatibility, authority, and failure model

1. V1 TOML continues to deserialize with the same meaning and the v1 schema remains
   generated and checked. Unknown keys fail with a safe path-specific error.
2. Config files never carry API-key values. Explain output redacts credential-like
   values and never reads environment secret contents into prompt blocks.
3. Project config is inactive until the canonical project root appears in the
   user-level trust list. A trust update requires explicit confirmation. Instructions
   are plain user-authority content and cannot change ToolPolicy.
4. Every model/tool run still goes through the existing single TurnDriver,
   ToolService and approval gate. Protected paths are denied before an approval
   panel; an unanswered proposal never executes.
5. One submitted input remains one session. A model change is recorded for a later
   session/run and cannot mutate an active request.
6. SQLite schema 1 is upgraded additively to schema 2; no data-home change, reset,
   destructive migration, or change to existing exit codes.
7. OpenAI Chat request bytes stay byte-identical for the existing DeepSeek fixture.
   Anthropic malformed/truncated streams fail typed and do not dispatch incomplete
   tool input. Provider adapters do not retry; runtime retries only transient errors.
8. Headless output stays ANSI-free and preserves exit codes.

## Setup and dependencies

No crate, product binary, runtime, or toolchain is added. Reuse the root workspace
and Cargo.lock. Add no dependency. Parse RFC HTTP dates with existing chrono 0.4.45
(MIT OR Apache-2.0) and use existing reqwest 0.12.24 (MIT OR Apache-2.0) for
transport. Do not add httpdate or a dependency outside plan §4 D10. No similar,
regex, globset, or G04 dependency is needed.
No network install or paid API call.

Anthropic API compatibility pin: anthropic-version: 2023-06-01, supported by the
official Messages API examples and the plan's required version pin. Fixture tests
assert this header.

## Ordered RED → GREEN slices

| Slice | Input → expected behavior | New exact test names |
|---|---|---|
| G01 prompt/rules | fixture environment and layered instruction files → bounded prompt and ordered ProjectRule blocks; hostile tool grant remains gated | g01_system_prompt_contains_environment_and_no_secret; g01_agents_md_chain_is_loaded_root_to_cwd_in_order; g01_agents_md_over_32k_is_truncated_with_notice; g01_agents_md_cannot_grant_tools; g01_claude_md_is_fallback_only |
| G02 config | v1/v2 files, trust roots, env and CLI collisions → resolved value + source; unknown field → typed safe error | g02_v1_config_still_loads_unchanged; g02_precedence_cli_over_env_over_project_over_user; g02_untrusted_project_config_is_ignored_with_reason; g02_config_explain_names_the_layer_for_every_key; g02_unknown_key_is_rejected_with_path |
| G03 adapters | OpenAI fixture and Anthropic SSE fixture → canonical events; HTTP-date → bounded retry; selected model/cost/thinking → next turn only and no plain leak | g03_openai_chat_adapter_keeps_m2_wire_format; g03_anthropic_stream_maps_tool_use_to_tool_call_delta; g03_anthropic_429_http_date_is_bounded; g03_model_switch_applies_next_turn_only; g03_cost_uses_config_prices_or_na; g03_thinking_delta_never_enters_plain_transcript |

Each slice writes/extends the named tests first and observes an assertion failure
before implementation. Existing tests are not deleted, skipped, or weakened.

## CLI, migration, schemas, and integration

- Extend the existing ha chat argument struct with --model, --profile and
  --approval; accept ask at CP-A and keep non-ask modes inactive until G05 so
  approval stays fail-closed.
- Preserve existing config validate compatibility; allow ha config explain
  without a file path to explain effective layers. /config and /trust use the
  same resolver and redaction.
- Add STORE_SCHEMA_VERSION 2 with session_settings via the existing M1 migration
  path. Preserve schema-1 databases and all other store migration domains.
- Generate schemas with
  cargo run -p harness-types --bin generate_schemas --locked; never edit JSON
  schema documents by hand. Keep p0_f03 green.
- Add required tests to existing targets only: interactive_session,
  interactive_launch, milestone_m2 and harness-providers unit tests. No registry
  status is changed.

## Required gates and evidence

Run RED before implementation. After implementation run:

- cargo fmt --all
- cargo clippy --workspace --all-targets --locked -- -D warnings
- cargo test -p harness-cli --bin ha --locked
- cargo test -p harness-cli --test milestone_m2 --locked -- --test-threads=1
- cargo test -p harness-cli --test interactive_session --locked -- --test-threads=1
- cargo test -p harness-cli --test interactive_launch --locked -- --test-threads=1
- provider unit tests and the M6 prerequisite gate at HEAD
- pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json

Run the launch gate until one green result, with at most three attempts for the
known loopback flake; record every attempt and do not edit tests to address it.
Regenerate schemas and confirm p0_f03. Do not run live provider smoke. Final evidence
records test counts, source digest, OS/toolchain, gate attempts, dependency versions,
spec approval status, skipped/not_run items, and remaining limits.

## Acceptance mapping

| Acceptance | Proof |
|---|---|
| V01–V03 | System prompt and ProjectRule tests; no secret, correct chain/digest/limit, policy unaffected |
| V04–V05 | Config compatibility, precedence/trust, ConfigExplain, safe unknown-key error |
| V06–V10 | OpenAI byte snapshot, Anthropic loopback including tool mapping, date retry cap, model persistence, cost/thinking secrecy |
| Existing invariants | interactive_session/interactive_launch; p0_f03; M2; launch gate |

Status: planned before coding. CP-A closes only when all requested G01–G03 gates
pass; stop there.

## Bản tiếng Việt đầy đủ

### Mục tiêu và việc không làm

Triển khai G01–G03 trong Cargo workspace hiện tại và binary ha. Model nhận system
policy có giới hạn, gồm môi trường chạy và luật dùng tool, cùng project rules. Config
v2 hợp nhất default, config user, project config đã trust (với `config.local.toml`
làm overlay nội bộ), biến môi trường, rồi cờ CLI. Adapter OpenAI Chat Completions
hiện tại được tổng quát hóa; provider id, endpoint, model và default thinking của
DeepSeek được chuyển sang preset config DeepSeek riêng. Bổ sung adapter Anthropic
Messages. Các lệnh tương tác hiện có cung cấp config, trust, model, thinking và cost.
Lựa chọn model của session được lưu qua migration SQLite cộng thêm.

Không triển khai G04 trở đi, binary/runtime thứ hai, crate mới, OAuth, MCP, permission
mode ngoài hành vi ask/fail-closed hiện tại, hoặc smoke test live có tính phí. Không
đổi quy tắc một input mỗi session, exit code, approval fail-closed, kiểm tra protected
path trước approval panel, hay hành vi headless không ANSI. Văn bản chỉ dẫn dự án không
cấp quyền cho tool.

### Điều kiện tiên quyết và phạm vi source

Đã đọc quy định repo trong `docs/implementation-next/README.vi.md` §§1–5,
`docs/HA_AGENT_PLAN.vi.md` §§2–5 và G01–G03, `docs/specs/HA_TUI.vi.md`,
`docs/specs/M2.vi.md`, `docs/specs/M6.vi.md` §5.6 và `docs/handoffs/CURRENT.vi.md`.
HEAD khác revision mà plan dùng để khảo sát. Codebase-memory MCP và `index_repository`
không có trong công cụ task này nên đã dùng cách dự phòng: tìm kiếm source và đọc các
đoạn liên quan.

Các thay đổi đã có trước khi bắt đầu cần được giữ nguyên:

| File | Thay đổi có sẵn lúc bắt đầu |
|---|---:|
| `crates/harness-cli/src/interactive/headless.rs` | 9 dòng |
| `crates/harness-cli/src/interactive/memory.rs` | 26 dòng |
| `crates/harness-cli/src/interactive/service.rs` | 8 dòng |
| `crates/harness-memory/src/lib.rs` | 61 dòng |
| `crates/harness-memory/src/retrieval.rs` | 18 dòng |
| `crates/harness-store-sqlite/src/models.rs` | 4 dòng |
| `crates/harness-store-sqlite/src/store/memory.rs` | 84 dòng |
| `crates/harness-store-sqlite/src/store/memory/advanced.rs` | 8 dòng |

### Kiểm kê yêu cầu

Các disposition theo template có nghĩa: `reuse_verified` là đã xác minh tái sử dụng,
`adapt` là cần điều chỉnh, `missing` là còn thiếu, `incompatible` là không tương thích.

| Yêu cầu | Symbol/path và caller hiện có | Test/bằng chứng hiện có | Phân loại và hành động |
|---|---|---|---|
| G01 system prompt có vai trò, luật tool, OS/shell/cwd/root/git/date/TurnLimits; tối đa 2 KiB, không secret | `RunRequest.system_policy` trong harness-runtime; policy mặc định ở `RuntimeConfig`; `interactive/service.rs` tạo từng run; tool schema và TurnLimits ở harness-tools | `interactive_session`, `interactive_launch`; chưa có test prompt | `adapt`: thêm `SystemPromptBuilder` trong `interactive/prompt.rs`; chỉ đưa facts không bí mật vào prompt; giới hạn byte xác định và dùng token estimator sẵn có |
| G01 project-rule channel và digest | `ContextBlock`, `ContextChannel::ProjectRule`, `ContentHash` trong harness-session; `RuntimeService::build_context` gọi ContextBuilder | Test context packet trong phase_p5/interactive_session | `reuse_verified`: truyền block bắt buộc dưới dạng ProjectRule, authority User, provenance và digest |
| Chuỗi chỉ dẫn global→root→cwd, fallback CLAUDE, notice 32 KiB | `interactive/paths.rs` và `interactive/project.rs` giải quyết user path/project root; chưa có loader | Chưa có test loader AGENTS.md | `missing`: thêm `interactive/instructions.rs`; ưu tiên AGENTS.md theo từng thư mục; CLAUDE.md chỉ fallback khi thiếu AGENTS.md; giới hạn tổng byte và báo cắt |
| Chỉ dẫn không được đọc từ protected path, text không được cấp quyền tool | `harness-tools/src/workspace.rs` kiểm protected path trước approval; ToolService/ChannelApprovalGate là cổng action | `a_read_only_kind_cannot_reach_a_protected_path_through_the_gate`; test approval | `adapt`: dùng chung predicate protected-path để lọc file; authority User; test rằng chỉ dẫn xấu không đổi hành vi cần approval/bị từ chối |
| G01 `/init` in mẫu tĩnh trước G04 | Slash parser/controller trong `interactive/input.rs` và `controller.rs`; chưa có `/init` | Test danh mục slash command | `missing`: thêm `/init` chỉ in vào transcript, ghi rõ chưa hỗ trợ ghi file cho tới G04 |
| G01 báo số file chỉ dẫn trong header/status | `interactive/app.rs`, `view.rs`, `tui/widgets/status.rs` | Test interactive_session và TUI status widget | `adapt`: đưa số lượng ra ngoài mà không đổi các landmark transcript |
| G02 HarnessConfig v2 nhưng vẫn đọc v1 | `HarnessConfig` strict hiện có gồm `schema_version` + `cli` trong harness-types; `interactive/config.rs` chỉ đọc user config; `p0_f03` kiểm schema sinh | `h02_missing_configuration_is_a_first_run_not_an_error`; `h02_valid_configuration_is_loaded_and_is_described_with_its_schema_version`; `p0_f03` | `adapt`: thêm field v2 tùy chọn, giữ tương thích v1; giữ schema v1 được sinh/kiểm và sinh schema v2 qua generate_schemas |
| G02 user/project/local config và trust | `interactive/config.rs`, `paths.rs`, `project.rs`; ConfigExplain cho extension ở harness-extensions; chưa có resolver nhiều lớp | Test h02 và extension config explain | `missing`: thêm `ResolvedConfig` có nguồn và lý do layer không hoạt động; chỉ nạp `.harness/config.toml` và `config.local.toml` khi canonical project root đã trust; local là overlay trong project layer |
| G02 precedence env/CLI và biến HA_* cũ | `interactive/bootstrap.rs`, `bounds.rs`, `credentials.rs` đọc `HA_PROVIDER_*`, `HA_TURN_*`, `DEEPSEEK_API_KEY`; `main.rs` chưa có cờ model/profile/approval | Test bounds, credentials, launch hiện có | `adapt`: resolver theo default→user→project được trust (local overlay)→env→CLI; giữ tương thích env cũ và một notice deprecation |
| G02 ConfigExplain, `/config`, `/trust` | `main.rs` có config validate/explain cần file, mới báo v1; `/config` và `/model` trong controller là output tham khảo; ConfigExplain có trong extensions | Test config CLI, slash controller, M6-04 | `adapt`: dùng chung từ vựng ConfigExplain; thêm `ha config explain` cho layer hiệu lực, `/config` và `/trust` có xác nhận rõ, chỉ ghi trust list user |
| G02 unknown-key path và tương thích v1 | Parser TOML strict trong `interactive/config.rs`, `main.rs` | `h02_unknown_field_and_unsupported_schema_keep_their_codes_and_name_the_path` | `adapt`: giữ lỗi typed, redaction an toàn, báo path config mà không echo giá trị bị từ chối |
| G03 adapter OpenAI Chat tổng quát và preset DeepSeek riêng trong config, byte M2 y nguyên | `DeepSeekAdapter` và `wire_messages` trong harness-providers; stream ở `providers/src/streaming.rs` | `providers` wire_tests và M2 A06/A07 | `adapt`: tách `OpenAiChatAdapter` có provider_id/endpoint/model/capabilities; chuyển provider id/endpoint/model/default thinking hiện tại của DeepSeek sang preset config DeepSeek; khóa snapshot byte trước/sau |
| G03 request/response/SSE/tool mapping cho Anthropic Messages | `ModelProvider`, `ProviderRequest`, `ProviderMessage`, `ProviderStreamEvent`, `SseDecoder`, reqwest transport trong harness-providers | Chưa có test Anthropic | `missing`: thêm `anthropic.rs` dùng chung port và bounded streaming; header x-api-key và version 2023-06-01; map text/tool_use/tool_result/max_tokens/usage; fixture loopback cho text, tool call, 429 date, stream bị cắt |
| G03 Retry-After HTTP-date và cap cấu hình tối đa 30 giây | `retry_after_seconds` hiện chỉ đọc delta-seconds; RuntimeService cap 2 giây | Test retry M2 A07 | `adapt`: hỗ trợ delta-seconds và HTTP-date; mặc định cap 30 giây, có cap config; retry do RuntimeService quản lý |
| G03 `/model` chỉ đổi lượt sau và lưu lựa chọn | `/model` trong controller chỉ đọc; interactive service tạo runtime theo input; STORE_SCHEMA_VERSION là 1, chưa có `session_settings` | Test interactive_session resume/model; test migration/reopen SQLite | `adapt`: migration cộng thêm bảng session_settings và port methods; chỉ áp model đã chọn khi tạo run sau, không đổi request đang chạy |
| G03 stream thinking và không rò vào plain transcript | Provider hiện tắt thinking; ProviderStreamEvent chưa có thinking; history hiển thị event text | M2 wire; TUI history/plain transcript tests | `missing`: thêm ThinkingDelta; render mờ trong TUI; không đưa vào plain transcript hoặc nội dung hội thoại lưu trữ |
| G03 giá theo usage, `/cost`, status bar | `ProviderStreamEvent::Usage`, `BudgetLedger` có token totals; status widget chỉ hiển thị context | Test usage provider và TUI status | `adapt`: tính cost session từ giá input/output mỗi triệu token tại `[models.<name>]`; dùng usage hoặc estimator; không có giá thì `n/a` |
| G03 provider fixture và smoke interface | FakeProvider/loopback trong `milestone_m2`; script `Smoke-HaProvider.ps1` dành cho provider hiện tại | Test M2 fixture; smoke không nằm trong gate | `adapt`: thêm fixture loopback Anthropic qua adapter thật; mở rộng tùy chọn smoke protocol/endpoint/model nhưng không chạy smoke có phí |

### Tương thích, authority và lỗi

1. TOML v1 tiếp tục deserialize cùng ý nghĩa và schema v1 tiếp tục được sinh/kiểm.
   Unknown key báo lỗi an toàn, kèm path.
2. Config không lưu API key. Explain không lộ giá trị credential và prompt không đọc
   secret từ môi trường.
3. Project config bị bỏ qua cho tới khi canonical project root nằm trong trust list
   user. Cập nhật trust cần xác nhận rõ. Chỉ dẫn có authority User, không thể đổi
   ToolPolicy.
4. Mọi model/tool run tiếp tục đi qua TurnDriver, ToolService và approval gate hiện
   có. Protected path bị chặn trước panel; proposal chưa được trả lời không chạy.
5. Một input đã gửi vẫn tương ứng một session. Đổi model chỉ có hiệu lực ở run/session
   sau, không sửa request đang hoạt động.
6. SQLite schema 1 được nâng lên schema 2 theo cách cộng thêm; không đổi data home,
   reset dữ liệu, migration phá hủy hay exit code.
7. Byte request OpenAI Chat giữ nguyên theo fixture DeepSeek hiện tại. Stream Anthropic
   sai/cụt phải lỗi typed và không dispatch tool input chưa hoàn chỉnh. Adapter không
   retry; runtime chỉ retry lỗi transient.
8. Headless không ANSI và giữ nguyên exit code.

### Setup và dependency

Không thêm crate, binary sản phẩm, runtime hay toolchain. Dùng workspace gốc và
Cargo.lock. Parse HTTP-date RFC bằng `chrono` 0.4.45 (MIT OR Apache-2.0) đã có;
transport dùng `reqwest` 0.12.24 (MIT OR Apache-2.0) hiện có. Không thêm httpdate
hoặc dependency ngoài plan §4 D10. Không cần similar, regex, globset hay dependency
G04. Pin giao thức Anthropic: `anthropic-version: 2023-06-01`, fixture
kiểm header. Không cài dependency qua network, không gọi API có phí.

### Thứ tự RED → GREEN

| Lát cắt | Input → hành vi mong đợi | Tên test mới chính xác |
|---|---|---|
| G01 prompt/rules | Fixture môi trường và file chỉ dẫn theo lớp → prompt giới hạn và ProjectRule đúng thứ tự; text đòi cấp quyền vẫn bị gate | `g01_system_prompt_contains_environment_and_no_secret`; `g01_agents_md_chain_is_loaded_root_to_cwd_in_order`; `g01_agents_md_over_32k_is_truncated_with_notice`; `g01_agents_md_cannot_grant_tools`; `g01_claude_md_is_fallback_only` |
| G02 config | File v1/v2, trust roots, env và CLI trùng khóa → giá trị kèm nguồn; unknown field → lỗi typed an toàn | `g02_v1_config_still_loads_unchanged`; `g02_precedence_cli_over_env_over_project_over_user`; `g02_untrusted_project_config_is_ignored_with_reason`; `g02_config_explain_names_the_layer_for_every_key`; `g02_unknown_key_is_rejected_with_path` |
| G03 adapter/config | Preset DeepSeek tách khỏi adapter tổng quát; fixture OpenAI và Anthropic SSE → event chuẩn; HTTP-date → retry có cap; model/cost/thinking → lượt sau, không rò plain | `g03_openai_chat_adapter_keeps_m2_wire_format`; `g03_anthropic_stream_maps_tool_use_to_tool_call_delta`; `g03_anthropic_429_http_date_is_bounded`; `g03_model_switch_applies_next_turn_only`; `g03_cost_uses_config_prices_or_na`; `g03_thinking_delta_never_enters_plain_transcript` |

Mỗi lát cắt phải thêm/mở rộng test có tên trên trước implementation và quan sát
assertion fail trước khi code. Không xóa, skip hoặc nới test cũ.

### CLI, migration, schema và tích hợp

- Mở rộng struct đối số `ha chat` hiện có với `--model`, `--profile`, `--approval`;
  ở CP-A chỉ nhận ask và chưa kích hoạt mode khác để giữ fail-closed.
- Giữ tương thích `config validate`; cho phép `ha config explain` giải thích layer
  hiệu lực mà không cần file; `/config` và `/trust` dùng cùng resolver/redaction.
- Thêm `STORE_SCHEMA_VERSION` 2 và `session_settings` qua đường migration M1 hiện có.
  Giữ nguyên DB schema 1 và các miền migration khác.
- Sinh schema bằng `cargo run -p harness-types --bin generate_schemas --locked`;
  không chỉnh JSON schema bằng tay; giữ `p0_f03` xanh.
- Thêm test vào target hiện có: `interactive_session`, `interactive_launch`,
  `milestone_m2` và unit tests của harness-providers. Không đổi registry status.

### Gate và evidence bắt buộc

Chạy RED trước implementation. Sau implementation chạy:

- `cargo fmt --all`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test -p harness-cli --bin ha --locked`
- `cargo test -p harness-cli --test milestone_m2 --locked -- --test-threads=1`
- `cargo test -p harness-cli --test interactive_session --locked -- --test-threads=1`
- `cargo test -p harness-cli --test interactive_launch --locked -- --test-threads=1`
- Unit tests provider và gate prerequisite M6 trên HEAD
- `pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1 -Json`

Chạy launch gate tới khi có một lượt xanh, tối đa ba lượt cho loopback flake đã biết;
ghi từng lượt và không sửa test để né flake. Sinh lại schema và xác nhận `p0_f03`.
Không chạy provider smoke live. Evidence cuối ghi số test, digest source, OS/toolchain,
dependency version, từng lần gate, tình trạng chưa có duyệt riêng cho SPEC, mục
`not_run` và giới hạn còn lại. Dừng sau G03 tại CP-A.

### Ánh xạ tiêu chí chấp nhận

| Tiêu chí | Bằng chứng |
|---|---|
| V01–V03 | Test system prompt và ProjectRule: không secret, đúng chain/digest/limit, policy không đổi |
| V04–V05 | Test tương thích config, precedence/trust, ConfigExplain và lỗi unknown key an toàn |
| V06–V10 | Snapshot byte OpenAI, Anthropic loopback có tool mapping, date retry cap, model persistence, cost/thinking không rò |
| Bất biến cũ | `interactive_session`, `interactive_launch`, `p0_f03`, M2, launch gate |

Trạng thái: SPEC được lập trước code. CP-A chỉ hoàn tất khi toàn bộ gate G01–G03
xanh; sau đó dừng.

## CP-B assignment / Assignment CP-B (SPEC trước RED)

**English.** The current assignment is G04–G06 only. CP-A is retained as historical
evidence; its old “do not start G04” handoff is superseded by the user's new
assignment. Keep one root workspace/binary/engine; preserve one admitted input per
session, fail-closed approval, protected/escape rejection before a panel, ANSI-free
headless output, existing exit codes, and all D1–D11 decisions. Stop after CP-B.

**Tiếng Việt.** Assignment hiện tại chỉ gồm G04–G06. CP-A được giữ làm lịch sử;
handoff cũ “không bắt đầu G04” đã được assignment mới của user thay thế. Giữ một
workspace/binary/engine; bảo toàn một input được nhận mỗi session, approval
fail-closed, chặn protected/escape trước panel, headless không ANSI, exit code cũ
và toàn bộ quyết định D1–D11. Dừng sau CP-B.

### Preflight verification / Kiểm chứng trước khi sửa

- `HEAD = origin/master = 0bf6f40bf24832da7f61f45e026c8556fc94159e`; branch
  `master`, worktree sạch. The code graph MCP tools were not exposed, so source
  discovery used the repo-prescribed `rg` and targeted reads. / Công cụ graph MCP
  không khả dụng; đã dùng `rg` và đọc source có mục tiêu theo fallback của repo.
- G01–G03 source checked before edits: `SystemPromptBuilder` enforces a 2 KiB
  cap; `instructions::load` emits digest-bearing `ProjectRule` blocks and
  `service.rs` passes them into `RunRequest`; config resolver and v1/preference
  tests exist; the OpenAI/Anthropic adapters and DeepSeek config are separate;
  model switching is persisted through `session_settings`; cost/thinking tests
  exist. / Đã đọc symbol G01–G03: prompt có trần 2 KiB; loader tạo block
  `ProjectRule` có digest và service truyền vào `RunRequest`; config v1/precedence
  có test; adapter OpenAI/Anthropic tách riêng, DeepSeek là config; đổi model được
  lưu ở `session_settings`; có test cost/thinking.
- Before-edit tests: `cargo test -p harness-cli --bin ha --locked` = **278
  passed, 0 failed, 1 ignored**; `g01_` = **5/5**; `g02_` = **5/5**;
  `g01_agents_md_cannot_grant_tools` = **1/1**; `g03_` in `harness-cli` = **3/3**;
  store model-switch migration selector = **1/1**; provider G03 selectors excluding
  the previously exhausted `g03_anthropic_429_http_date_is_bounded` = **3/3**.
  The known loopback test is not part of this preflight rerun. / Kiểm thử trước sửa:
  tổng `ha` **278 đạt, 0 lỗi, 1 ignored**; G01 **5/5**, G02 **5/5**, test rule
  **1/1**, G03 CLI **3/3**, migration selector **1/1**, provider G03 (bỏ test
  loopback đã hết retry) **3/3**. Không chạy lại test loopback đã biết lỗi ở CP-A.
- D10 check: lockfile has `globset 0.4.20`, `regex 1.13.1`, and `ignore 0.4.25`;
  `cargo info similar@3.2.0` reports version **3.2.0**, Apache-2.0, MSRV 1.85.
  Pin only these three approved dependencies exactly; keep `TOOL_CONTRACT_VERSION`
  and unrelated schema versions unchanged. / Lockfile có đúng các bản `globset`,
  `regex`, `ignore`; `cargo info similar@3.2.0` xác nhận `similar 3.2.0`, Apache-2.0,
  MSRV 1.85. Chỉ pin ba dependency D10 cho phép; giữ `TOOL_CONTRACT_VERSION` và
  schema không liên quan.

### Requirement inventory / Kiểm kê requirement

Disposition is evidence-based: `reuse_verified`, `adapt`, `missing`, or
`incompatible`. Each row names the current source/test and the CP-B action.
Disposition dựa trên bằng chứng: `reuse_verified`, `adapt`, `missing`, hoặc
`incompatible`. Mỗi dòng nêu source/test hiện có và việc CP-B sẽ làm.

| Requirement / Yêu cầu | Current source and tests / Source và test hiện có | Disposition and action / Nhãn và việc làm |
|---|---|---|
| G04 write/edit contracts; unique old-string rule; typed `EditNotFound`/`EditAmbiguous` / Hợp đồng write/edit, old string duy nhất, lỗi typed | `CodingToolAction`, `from_provider_call`, `ToolKind`, and `coding_tool_schemas()` in `harness-tools/src/contracts.rs`; `apply_patch` is the only workspace mutation; existing `a15_path_patch_safety` | `adapt`: add additive action variants, parser/schema and typed error codes; preserve `apply_patch` / thêm variant/parser/schema/error, giữ `apply_patch` |
| G04 glob with `.gitignore`, bounded at 4096 / Glob theo `.gitignore`, trần 4096 | `workspace::walk_files` already uses `ignore::WalkBuilder`, rejects links/protected paths, and caps entries at 4096; no glob filter or glob contract | `adapt`: reuse walker and apply pinned `globset` matcher within the same cap / tái dùng walker, thêm matcher `globset` |
| G04 regex/case/glob/context search, max 512; line-range read with line numbers / Search regex/case/glob/context, tối đa 512; read range có số dòng | `workspace::search_text` is substring-only and caps 512; `read_text_output` is bounded but `read_file` has no line range; no G04 tests | `adapt`: optional schema fields, regex compile errors typed, bounded context previews, line-numbered ranges / thêm field optional, lỗi regex typed, context và số dòng |
| G04 tools schema v2→v3 additive; central workspace validation; retain patch / Schema tools v2→v3 additive; validation tập trung; giữ patch | `TOOLS_SCHEMA_VERSION=2` and `migrate_tools_schema`; `ToolExecutionService::validate_workspace_action` checks containment/protection before proposals; existing `a15_path_patch_safety` | `adapt`: bump tools DB marker through its additive migration and validate every new path action before approval / bump marker bằng migration additive, validate mọi path mới trước approval |
| G04 mutating hashes and ≤1 MiB preimage artifact / Hash mutation và artifact preimage ≤1 MiB | `ApplyPatch` output has before/after hashes; receipts and artifact publication exist, but no write/edit preimage is captured | `adapt`: record file hashes with receipt evidence; publish bounded preimage in task-scoped artifact for future `/undo` / ghi hash, artifact preimage scoped theo task |
| G04 unified diff in approval panel and plain `[diff]` / Diff unified trong panel và plain `[diff]` | Current `ApprovalProposal` has action/summary only; TestBackend and plain transcript renderers exist | `missing`: attach bounded diff preview to proposal; add TestBackend and plain-mode assertions / thêm diff preview và test cả hai renderer |
| G04 prompt prefers edit over patch / Prompt ưu tiên edit hơn patch | `SystemPromptBuilder` lists available tools; existing prompt bound test | `adapt`: add explicit preference while preserving ≤2 KiB / thêm chỉ dẫn, giữ trần 2 KiB |
| G05 `PolicyMode` and `tool(pattern)` inside `ToolPolicy`; protected→deny→allow→mode→panel / `PolicyMode` và rule trong `ToolPolicy`; thứ tự protected→deny→allow→mode→panel | `ToolPolicy` owns path deny rules; workspace validation precedes `ApprovalGate`, which otherwise always asks/denies; G01 hostile-rule test proves project text does not grant tools | `adapt`: extend the existing policy source of truth and its prepared-action decision; no parallel policy class / mở rộng `ToolPolicy`, không tạo lớp policy thứ hai |
| G05 confirmed `A` rule persistence, `/permissions`, `/mode` / Lưu rule sau xác nhận `A`, `/permissions`, `/mode` | Controller has `y/a/n` turn-wide approval, `config.local.toml` exists, but `A` and the commands do not persist policy rules | `missing`: show proposed pattern, require explicit Enter, persist only afterward, apply it for the current session / hiện pattern, đòi Enter xác nhận rồi mới lưu và áp dụng |
| G05 transcript trace for each rule/mode auto-allow; headless flags default ask / Transcript ghi mọi auto-allow; cờ headless mặc định ask | Current grant-for-run logs every covered action; headless currently uses `ApprovalMode::None` and has no allow/deny flags | `adapt`: retain per-action visibility, add ask/auto-edit/full-auto plus temporary patterns; default stays fail-closed / ghi từng action, thêm mode/rule tạm, mặc định vẫn fail-closed |
| G06 `ask_user` through `HumanInputService` and `TurnStop::NeedsInput`, interactive answer and continuation / `ask_user` qua HumanInputService/NeedsInput, panel trả lời và tiếp tục | `HumanInputService`, durable question store, and `TurnStop::NeedsInput` exist for goal evaluation; interactive has no model-call question tool or answer panel | `adapt`: route the new interactive tool to the existing service, stop cleanly, answer and admit continuation in the same task / nối vào service hiện có, dừng, nhận câu trả lời và tiếp tục cùng task |
| G06 Enter queue and `/steer` via `RunInbox::steer` / Enter xếp hàng, `/steer` qua inbox | Controller currently refuses second input while active; `RunInbox::steer` and driver boundary consumption exist; interactive does not attach inbox | `incompatible`: current “one active input” UI behavior conflicts with this assignment; implement a bounded one-item pending queue and wire the inbox without creating another driver / thay hành vi từ chối bằng queue một item và nối inbox hiện có |
| G06 Esc cancels a running turn; Esc in approval does not answer / Esc hủy turn đang chạy; approval không bị trả lời | T06 currently documents Escape as non-cancel; controller preserves approval modal on Esc; Ctrl-C cancellation and `i07b` PTY exist | `incompatible`: update T06 in `HA_TUI.vi.md` with the user's “table stakes across all three references” rationale; only running Esc cancels, approval Esc remains non-answer / sửa T06 theo lý do user yêu cầu; chỉ Esc lúc chạy mới hủy |
| G06 `@` ignore walker picker; `!` shell through approval; `!!` display-only / Picker `@`; `!` qua approval; `!!` chỉ hiển thị | Attachments resolve explicitly named paths; `run_shell` already crosses `TurnDriver` approval; no picker/prefix syntax | `adapt`: add bounded visible picker and route single-bang through `run_shell`; double-bang stays local display / thêm picker có giới hạn, một `!` dùng run_shell; `!!` không gửi model |

### Ordered slices, compatibility, and acceptance / Trình tự, tương thích, acceptance

1. G04 contracts + exact RED selectors, then workspace read/write/search/glob and
   receipt preimages, then diff approval panel. / Hợp đồng + test RED chính xác,
   sau đó workspace và receipt, cuối cùng diff panel.
2. G05 policy extension + exact RED selectors, persistence only after explicit
   pattern confirmation, then commands/headless arguments and audit transcript.
   / Mở rộng policy + test RED, chỉ lưu sau khi xác nhận pattern, rồi commands/
   cờ headless và transcript.
3. G06 ask-user, queue/steer, Esc, picker and shell prefixes; test with the real
   existing TurnDriver/store/host. / Làm ask-user, queue/steer, Esc, picker và
   shell; dùng TurnDriver/store/host hiện có.

Required exact names include the G04 selectors in plan §6; G05
`g05_protected_path_beats_every_allow_rule_and_mode`,
`g05_deny_rule_beats_allow_rule`, `g05_auto_edit_never_auto_runs_process_or_shell`,
`g05_always_allow_writes_a_rule_only_after_confirmation`,
`g05_rule_pattern_matches_args_not_tool_name_only`,
`g05_headless_default_still_fails_closed`,
`g05_transcript_records_every_auto_allowed_action`; and G06
`g06_ask_user_stops_the_turn_and_answer_resumes_it`,
`g06_enter_while_running_queues_and_sends_after_terminal`,
`g06_steer_reaches_the_driver_mid_run`,
`g06_esc_cancels_a_running_turn_but_not_an_approval`,
`g06_at_picker_inserts_a_workspace_relative_path`,
`g06_bang_prefix_goes_through_the_same_approval_gate`.
TestBackend must cover diff and picker; real-console PTY selectors i05/i06/i07/h05
run through `Invoke-HaPtyAcceptance.ps1`. Run `milestone_m4`, `phase_p3`, and all
`interactive_*` with one test thread; final HA launch gate must have a green run
with `failures: []`. / Dùng đúng tên test theo plan; TestBackend kiểm diff/picker;
PTY thật chạy i05/i06/i07/h05 qua script; chạy milestone_m4, phase_p3 và mọi
interactive_* tuần tự; launch gate phải có một lượt `failures: []`.

No D1–D11 change is warranted by preflight measurements. The T06 Escape change is
the explicit G06 assignment and will be recorded separately in `HA_TUI.vi.md`.
Không có số đo nào buộc đổi D1–D11. Thay đổi Esc ở T06 được G06 giao rõ và sẽ ghi
riêng trong `HA_TUI.vi.md`.

## Preflight observation (before RED)

The M6 prerequisite gate was attempted on the then-current working tree and stopped
at its format step (exit 1), before running milestone tests. Rustfmt reported
existing format differences in crates/harness-cli/src/web/mod.rs,
crates/harness-cli/src/interactive/memory.rs,
crates/harness-cli/tests/milestone_m10.rs, crates/harness-memory/src/lib.rs, and
crates/harness-store-sqlite/src/store.rs. These were baseline formatting findings,
not G01–G03 failures. No test or implementation file had been changed at that point.
This is a historical CP-A observation; CP-B formatting and Clippy results are below.

## CP-B implementation record and acceptance / Kết quả triển khai và acceptance CP-B

### Scope and preserved decisions / Phạm vi và quyết định được giữ

G04–G06 are implemented in the existing root workspace and `ha` binary. No crate,
product binary, or second engine was added. G07 and later were not started. D1–D11
remain unchanged because the measured CP-B results did not contradict them. The
Escape behavior change is the explicit G06 assignment and is recorded in
`docs/specs/HA_TUI.vi.md` T06 with the requested rationale. / Đã triển khai G04–G06
trong workspace gốc và binary `ha`; không thêm crate, binary sản phẩm hay engine
thứ hai. Chưa bắt đầu G07 trở đi. Giữ D1–D11 vì số đo CP-B không mâu thuẫn. Thay
đổi phím Esc là yêu cầu rõ của G06, đã ghi ở T06 trong `docs/specs/HA_TUI.vi.md`
cùng lý do user yêu cầu.

The CP-B source preserves one admitted input per session, fail-closed approval,
protected-path rejection before the panel, ANSI-free headless output, and existing
exit codes. No paid API call, user install, manual migration run, or user/shared
project `config.local.toml` write was performed. G05 persistence tests use disposable
temporary roots. / Source CP-B giữ một input được nhận mỗi session, approval
fail-closed, chặn protected path trước panel, headless không ANSI và exit code hiện
hữu. Không gọi API trả phí, cài lên máy user, chạy migration thủ công hoặc ghi
`config.local.toml` của project user/workspace dùng chung. Test G05 chỉ ghi file
trong thư mục tạm có thể xóa.

### Implemented requirement inventory / Kiểm kê requirement đã triển khai

| Group / Nhóm | Result and source / Kết quả và source | Verification / Kiểm chứng |
|---|---|---|
| G04 contracts, write/edit, typed errors, glob/search/read ranges, schema 2→3 / Contract, write/edit, lỗi typed, glob/search/read range, schema 2→3 | Additive tool actions and schemas in `harness-tools/src/contracts.rs`; all path actions use `validate_workspace_action`; mutating receipts include hashes and bounded preimage artifacts; approval panel and plain transcript show bounded diff. / Thêm action/schema additive; mọi path action qua validate tập trung; receipt mutation có hash và preimage artifact có giới hạn; panel và transcript plain hiện diff có giới hạn. | Exact `g04_*` selectors plus `milestone_m4` **23/23**. The TestBackend diff case passed. / Selector `g04_*` và `milestone_m4` **23/23**; TestBackend diff đạt. |
| G05 policy, rules, persistent `A`, commands, audit, headless / Policy, rule, `A` dài hạn, command, audit, headless | `PolicyMode` and `ToolPolicy::decide` remain in the existing policy source; order is protected → deny → allow → mode → panel. Pattern persistence happens only after explicit Enter. `/permissions`, `/mode`, transcript audit, and headless flags are wired. / `PolicyMode` và `ToolPolicy::decide` nằm trong policy hiện có; thứ tự protected → deny → allow → mode → panel. Chỉ lưu pattern sau Enter xác nhận. Đã nối `/permissions`, `/mode`, audit transcript và cờ headless. | CLI G05 selectors **9/9** and `harness-tools` G05 selectors **5/5**. `/mode`, confirmation-only persistence, deny/allow precedence, default ask, and per-action audit are covered. / G05 CLI **9/9**, G05 `harness-tools` **5/5**; có test mode, xác nhận lưu, precedence deny/allow, mặc định ask và audit từng action. |
| G06 ask-user, queue/steer, Esc, picker, shell prefixes / ask-user, queue/steer, Esc, picker, shell | `ask_user` uses `HumanInputService`; a queued Enter becomes the next input after terminal; `/steer` uses the attached inbox; Esc cancels a running turn but does not answer approval; `@`, `!`, and `!!` use the existing workspace/tool paths. / `ask_user` dùng `HumanInputService`; Enter được xếp hàng thành input kế tiếp sau terminal; `/steer` dùng inbox đã nối; Esc hủy lượt đang chạy nhưng không trả lời approval; `@`, `!`, `!!` dùng đường workspace/tool hiện có. | Controller/service G06 tests, `milestone_m4` **23/23**, TestBackend picker, and PTY selectors below. / Test controller/service G06, `milestone_m4` **23/23**, picker TestBackend và PTY bên dưới. |

The three earlier assertions that expected a running input to be refused or an
expired `y` to be refused were updated to assert the G06 queue contract and to
prove that a stale approval is never answered. The approval panel test now asserts
the turn grant and the separate Enter confirmation for a persistent rule. These
tests were run RED/GREEN; no test was deleted or weakened. / Ba assertion cũ chờ
từ chối input khi run đang chạy hoặc từ chối `y` hết hạn đã được đổi để kiểm tra
queue G06 và chứng minh approval cũ không nhận câu trả lời. Test panel nay kiểm tra
grant trong lượt và xác nhận Enter riêng cho rule dài hạn. Đã chạy RED/GREEN; không
xóa hoặc nới test.

### Exact dependency pins / Pin dependency chính xác

| Crate | Exact version | License | Use / Công dụng |
|---|---:|---|---|
| `globset` | `0.4.20` | Unlicense OR MIT | G04 glob matcher / matcher glob G04 |
| `regex` | `1.13.1` | MIT OR Apache-2.0 | G04 regex search / search regex G04 |
| `ignore` | `0.4.25` | Unlicense OR MIT | `.gitignore`-aware traversal / walker theo `.gitignore` |
| `similar` | `3.2.0` | Apache-2.0 | G04 unified diff / unified diff G04 |

All four versions are exact pins in the root workspace and represented in the
same `Cargo.lock`. No crate was added. The `similar 3.2.0` metadata was checked
before pinning; its reported MSRV is 1.85. / Cả bốn version được pin exact ở
workspace gốc và ghi trong cùng `Cargo.lock`. Không thêm crate. Đã kiểm tra
metadata `similar 3.2.0` trước khi pin; MSRV được báo là 1.85.

### Verification ledger / Nhật ký kiểm chứng

| Command / Lệnh | Result / Kết quả |
|---|---|
| `cargo fmt --all` and `cargo fmt --all -- --check` / chạy và kiểm tra rustfmt | Pass / Đạt |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Pass, including final Verify-HaLaunch attempts / Đạt, kể cả trong các lượt Verify cuối |
| `cargo test -p harness-cli --bin ha --locked` | Attempt 1: **297 passed, 1 failed, 1 ignored**; loopback `completion_service_resume_flow` timed out. Attempt 2: **298 passed, 0 failed, 1 ignored**. / Lượt 1: **297 đạt, 1 lỗi, 1 ignored**, timeout loopback `completion_service_resume_flow`; lượt 2: **298 đạt, 0 lỗi, 1 ignored**. |
| Focused updated `h03`, `h05`, `t06` tests / Test đã chỉnh | **1/1 each**; queued message dispatch, stale approval remains unanswered, panel describes persistent-rule confirmation. / Mỗi test **1/1**; queue được dispatch, approval cũ không nhận câu trả lời, panel nêu xác nhận rule dài hạn. |
| `milestone_m4` / `phase_p3` / `interactive_session` (serial) | **23/23**, **21/21**, **14/14**. / Đạt lần lượt **23/23**, **21/21**, **14/14**. |
| `interactive_terminal` normal cargo run (serial) | **0 passed; 18 ignored** by design because these require a real console; the requested cases were run with the PTY script. / **0 đạt; 18 ignored** theo thiết kế vì cần console thật; các case yêu cầu đã chạy bằng PTY script. |
| PTY real-console cases `h05`, `i05`, `i06`, `i07a`, `i07b` / PTY console thật | `h05` attempt 1 exposed a partial-read fixture issue; after making the fixture consume the declared request body, attempt 2 passed **1/1**. `i05`, `i06`, `i07a`, `i07b` each passed **1/1**. Transcripts are under `target/pty-acceptance-cpb/`. / `h05` lượt 1 phát hiện fixture đọc thiếu body; sau khi fixture đọc đủ theo Content-Length, lượt 2 đạt **1/1**. `i05`, `i06`, `i07a`, `i07b` mỗi case đạt **1/1**. Transcript ở `target/pty-acceptance-cpb/`. |
| `cargo run -p harness-types --bin generate_schemas --locked` and focused `p0_f03` | Generator rewrote the two drifted schemas; focused test **1/1**. The full P0 suite passed **8/8** in both later Verify runs. / Generator tạo lại hai schema lệch; test riêng **1/1**. P0 đầy đủ đạt **8/8** trong hai lượt Verify sau. |

### HA launch gate attempts / Các lượt gate HA launch

The assignment's standalone `interactive_launch` retries before the final verifier
were **17 passed / 2 failed** (I04 and I13), **18/1 failed** (`i03_a_named_file...`),
then **19/19 passed**. The final verifier was then run three times, without editing
loopback tests: attempt 1 failed only `regression-phase_p0` because the generated
receipt schema was stale; attempt 2 had **18/19** launch tests and failed
`i03_a_named_file_reaches_the_model_inside_the_message`; attempt 3 had **18/19**
and failed `i13_resume_continues_the_task_with_recovered_context_and_no_rerun`.
After regeneration, `p0_f03` and the full P0–P7 suite passed. Every other reported
step in the last two verifier runs passed: format, Clippy, discovery, unit tests,
acceptance-session, providers-streaming, P0–P7, installer, release, and docs.
The verifier's summary captures only the final three command-output lines, so it
does not preserve the inner loopback assertion detail. The bounded retry budget is
exhausted; no further loopback run was made. / Các lượt `interactive_launch` độc
lập trước verifier lần lượt **17 đạt / 2 lỗi** (I04, I13), **18/1 lỗi**
(`i03_a_named_file...`), rồi **19/19 đạt**. Sau đó chạy verifier đúng ba lần,
không sửa test loopback: lượt 1 chỉ lỗi `regression-phase_p0` do schema receipt
chưa sinh lại; lượt 2 launch **18/19**, lỗi
`i03_a_named_file_reaches_the_model_inside_the_message`; lượt 3 **18/19**, lỗi
`i13_resume_continues_the_task_with_recovered_context_and_no_rerun`. Sau khi sinh
lại schema, `p0_f03` và P0–P7 đầy đủ đều đạt. Các bước còn lại ở hai lượt Verify
cuối đều đạt: format, Clippy, discovery, unit, acceptance-session,
providers-streaming, P0–P7, installer, release và docs. Verifier chỉ giữ ba dòng
cuối output mỗi lệnh nên không lưu assertion bên trong của loopback. Đã hết retry
budget; không chạy loopback thêm.

Consequently, CP-B source is implemented but acceptance remains
`implemented_unverified`: the required final verifier report had
`failures: ["acceptance-launch"]` on attempts 2 and 3. Do not mark CP-B accepted
or start G07. / Vì vậy source CP-B đã triển khai nhưng acceptance còn ở trạng thái
`implemented_unverified`: báo cáo Verify cuối có
`failures: ["acceptance-launch"]` ở lượt 2 và 3. Không đánh dấu CP-B accepted và
không bắt đầu G07.

### CP-B source snapshot and not run / Snapshot source và mục chưa chạy

At closeout the branch is `master`, HEAD is
`c1b4b6730f57c541f090516f9100d5cfcf9811bd`, and the worktree is dirty with CP-B
implementation, the generated schemas, and **48 changed paths** before the final
metadata updates. No commit or push was made. The system is Windows 11 Pro x64;
`rustc 1.97.1 (8bab26f4f 2026-07-14)` was selected through the task Rustup home.
The exact source digest and `Cargo.lock` digest are recorded in the evidence file.
Paid/live provider smoke, Linux verification, real user installation/PATH changes,
manual migration, user project config write, and G07+ were not run. G05 config
persistence tests wrote only inside disposable temp roots. / Khi chốt, branch
`master`, HEAD là `c1b4b6730f57c541f090516f9100d5cfcf9811bd`, worktree bẩn với
implementation CP-B, schema được sinh lại và **48 path thay đổi** trước cập nhật
metadata cuối. Không commit/push. Hệ thống Windows 11 Pro x64; chọn
`rustc 1.97.1 (8bab26f4f 2026-07-14)` qua Rustup home riêng. Digest source và
`Cargo.lock` ghi trong evidence. Chưa chạy smoke provider trả phí/live, kiểm chứng
Linux, cài/PATH thật, migration thủ công, ghi config project user hoặc G07 trở đi.
Test lưu config G05 chỉ ghi trong thư mục tạm có thể xóa.

## CP-C assignment — SPEC before RED / Assignment CP-C — SPEC trước RED

**Assignment:** G07–G09 from `docs/HA_AGENT_PLAN.vi.md`; stop after CP-C and do not start G10. **Base:** branch `master`, clean worktree, `e3f55d435a2267deac040c0f31d07fe9bded519e`. CP-B is recorded as `implemented_unverified`; the user has explicitly assigned CP-C despite that predecessor status. The handoff's two prior loopback failures were inspected without rerunning: i03 tests a named text attachment through the headless message body; i13 checks recovered prior prompt plus new input over exactly two provider requests. No deterministic product defect was identified by this source inspection. / **Assignment:** G07–G09 trong `docs/HA_AGENT_PLAN.vi.md`; dừng sau CP-C, không làm G10. **Base:** branch `master`, worktree sạch, `e3f55d435a2267deac040c0f31d07fe9bded519e`. CP-B đang được ghi `implemented_unverified`; user đã giao rõ CP-C dù trạng thái prerequisite này chưa accepted. Hai lỗi loopback trong handoff đã được inspect, không chạy lại: i03 kiểm tra file text được gắn vào body message headless; i13 kiểm tra prompt cũ được phục hồi cùng input mới qua đúng hai request provider. Source inspection không tìm thấy lỗi sản phẩm deterministic.

### Goal, non-goals, and compatibility / Mục tiêu, ngoài phạm vi và tương thích

- **EN:** Implement model-backed compaction with deterministic fallback, at most one auto-compaction per input, model-specific context windows, a `/context` overlay, and visible rejection of invalid memory contributions. **VI:** Triển khai compact bằng model có fallback xác định, tối đa một lần auto-compaction mỗi input, context window theo model, overlay `/context` và Notice khi từ chối memory contribution không hợp lệ.
- **EN:** Implement session titles/rename, picker metadata, project-scoped `--continue` and `sessions list --cwd`, `/clear`, `/diff`, guarded `/undo`, secret-redacted `/export`, and `/copy` through the existing `arboard` dependency. **VI:** Triển khai title/rename của session, metadata trong picker, `--continue` và `sessions list --cwd` giới hạn trong project, `/clear`, `/diff`, `/undo` có bảo vệ, `/export` che secret và `/copy` qua dependency `arboard` hiện có.
- **EN:** Implement trusted-source hooks through the existing `harness-tools::process` lifecycle. A hook may block but cannot approve; G05 policy/approval remains the only authority. Add `/hooks`, configurable bell, and notification command. **VI:** Triển khai hook từ nguồn đáng tin cậy qua lifecycle `harness-tools::process` hiện có. Hook được chặn nhưng không được cấp quyền; policy/approval G05 vẫn là thẩm quyền duy nhất. Thêm `/hooks`, bell cấu hình được và lệnh notification.
- **EN:** Preserve one admitted input per session, fail-closed approval, protected-path rejection before any panel, ANSI-free headless output, existing exit codes, one root workspace and the single `ha` binary, plus D1–D11. No new crate/engine, paid/live provider call, user install/PATH mutation, or manual database migration. **VI:** Giữ một input được nhận mỗi session, approval fail-closed, chặn protected path trước panel, headless không ANSI, exit code hiện có, một root workspace và binary `ha` duy nhất cùng D1–D11. Không thêm crate/engine, không gọi provider live/trả phí, không sửa PATH/cài cho user, không migration DB thủ công.
- **EN:** Config additions are optional fields in HarnessConfig v2; use existing `session_settings` and `STORE_SCHEMA_VERSION=2`. Preserve `TOOLS_SCHEMA_VERSION=3`; keep host-only undo out of the model tool schema. Regenerate JSON schemas with the existing generator, never by hand. No dependency is planned; `arboard`, process runner, session settings, artifact store and `similar` already exist. **VI:** Khoá config mới là field optional của HarnessConfig v2; dùng `session_settings` và `STORE_SCHEMA_VERSION=2` sẵn có. Giữ `TOOLS_SCHEMA_VERSION=3`; không đưa undo host-only vào schema tool của model. Sinh JSON schema bằng generator hiện có, không sửa tay. Không dự kiến thêm dependency; `arboard`, process runner, session settings, artifact store và `similar` đã có.

### Requirement inventory / Kiểm kê requirement

| Requirement | Existing symbol/path and caller | Existing test/evidence | Disposition | Gap and minimal change |
|---|---|---|---|---|
| G07 fixed-prompt summary calls the current provider, obeys summary budget, records `summary_source`, and falls back on provider failure/empty/timeout | `harness_runtime::SummaryProvider`, `RuntimeService::compact`, current `ModelProvider`; default provider is deterministic and `compact` already records source inside checkpoint metadata | `milestone_m5::a12_compaction_cas`, `p2_c04_five_compactions_preserve_mandatory_state_and_corrections`, M5 summary-failure coverage | `adapt` | Add provider-backed `ModelSummaryProvider` for real chat only; keep `--mock`/fixture summarization deterministic; add bounded prompt/request tests. |
| G07 manual and automatic compaction preserve CAS; trigger at `context_window - output_reservation - compaction_reserve`, at most once per admitted input, then return typed `ContextOverflow` if still over | `RuntimeService::compact` has M5 CAS; `RuntimeConfig` defaults to 8192; `RunRequest` is cloned across `TurnDriver` continuations; no `/compact` caller or pre-dispatch auto-compaction exists | Existing CAS tests above; planned G07 selectors are absent | `adapt` | Wire `/compact [guidance]`; resolve threshold; share one-attempt state across continuation clones; compact/rebuild once, then surface a typed overflow without changing existing exit codes. |
| G07 model context window precedence and `/context` channel/block/token/drop-reason overlay | `ModelConfigV2.context_window` exists, but `resolve_layers` currently applies only model prices; `ContextBuildResult` already has manifest, block IDs, token totals, omissions and superseded IDs; no overlay exists | `harness-session::ContextBuilder` and `phase_p2` test compiler/overflow; no G07 UI assertion exists | `adapt` | Resolve configured window → internal model table → 8192 with notice; retain enough `ContextBuildResult` details for the overlay to show channels, block token estimates, omissions and reasons. |
| G07 rejected memory contribution produces a user-visible Notice | `RuntimeService::run` and `continue_run` call `MemoryService::validate_contribution` and silently set invalid contributions to `None`; interactive `SessionEvent::Notice` and `TurnProgress` are existing paths | No test asserts a Notice on rejection | `adapt` | Emit a notice at the existing rejection point, once per turn, without admitting rejected blocks. |
| G08 automatic title, `/rename`, and picker title/model/time | Generic task-scoped `SqliteStore::{set_session_setting,session_setting}` exists and G03 persists model; `SessionSummary` and `/resume` picker exist but omit title/time | G03 model persistence test and `interactive::controller` resume picker tests cover adjacent behavior | `adapt` | Save a 60-character title from the first input; persist rename; extend session listing/picker with title, selected model, and creation time without a new table. |
| G08 `ha chat --continue` and `ha sessions list --cwd` are restricted to the current project | Headless `--resume` validates a session belongs to the current store; `SqliteStore::list_sessions` exists but is not project-filtered; no `--continue` or plural sessions command | `interactive_launch::i13_resume_continues_the_task_with_recovered_context_and_no_rerun` checks explicit resume only | `adapt` | Add newest-session selection and project-filtered list through existing project/store identity; do not select another project's task. |
| G08 `/clear` is `/new` plus viewport clear; `/diff` is read-only and offers start-base plus uncommitted views | `/new` controller action and `CodingToolAction::GitDiff` read-only path exist; session start does not record the required diff base | Existing `/new` and GitDiff tests do not cover these contracts | `adapt` | Add `/clear` viewport effect while retaining scrollback; record session `HEAD` and working-tree digest; display both diff modes through the existing read-only Git diff path. |
| G08 `/undo` uses G04 before artifacts and only restores a file whose current hash still equals receipt `after_hash`; each restore is separately approved and receipted | G04 receipts contain before/after hashes/artifact ID; `SqliteStore::load_receipts`, `tool_intent`, `read_artifact_page`; `ToolExecutionService` is the existing policy/approval/receipt authority | G04 artifact/hash tests cover storage only; no undo action exists | `missing` | Add a host-only restore action using exact scoped artifact bytes, workspace validation, and a hash recheck immediately before write; skip changed/missing files with reason; record a distinct receipt. |
| G08 `/export` writes JSONL or Markdown without credential values; `/copy` places the last answer on clipboard | Event journal and process text redaction exist; `arboard` is already used to read clipboard text/images but not to write text | Existing export and clipboard-set tests are absent | `adapt` | Add format-specific redacted export through the existing workspace write/approval path; add a clipboard text setter and report unsupported/failure plainly. |
| G09 hooks use bounded JSON stdin and the existing hardened process layer; pre-tool exit 2/timeout blocks with `blocked_by_hook`; hooks cannot turn Ask into Allow | HarnessConfig v2 has generic `hooks`; config resolution already tracks user/project trust; `harness-tools::process` enforces process lifecycle, environment allowlist and output caps; `ToolExecutionService` owns the only action gate | No hook runner or G09 selector exists | `adapt` | Add typed hook parsing and a runner in `harness-tools` so it can call private `process`; cap serialized args at 8 KiB and timeout at 60 seconds; stdout has no permission meaning; a block is visible and durable. |
| G09 hooks load only from user config and trusted project config; `/hooks` shows effective definitions and provenance | `resolve_layers` trusts canonical project roots; `ConfigExplain` has source layers; generic project hooks are currently ignored | Existing G02 trusted/untrusted config tests cover adjacent loader behavior | `adapt` | Merge hooks from global user and trusted project layers only; reject untrusted project definitions with a reason; show effective hook source in `/hooks`. |
| G09 post-tool-use/stop are observational; notification/bell trigger for approval, ask-user, and turn completion; headless stays undecorated | `ToolObserver`, `TurnObserver`, `SessionEvent::Notice`, `UiConfigV2` and TUI renderer exist; UI config currently has only renderer/color | No hook lifecycle, BEL, or notification tests | `adapt` | Invoke post/stop/notification through the same bounded runner; observational-hook failures never change action outcome; emit BEL only on interactive TUI when configured; never write BEL/ANSI from headless. |

#### Requirement inventory in Vietnamese / Bản dịch tiếng Việt của requirement inventory

| Yêu cầu | Source/test hiện có | Quyết định và thay đổi tối thiểu |
|---|---|---|
| G07: tóm tắt bằng prompt cố định qua provider hiện hành, giới hạn output, lưu `summary_source`, lỗi/empty/timeout thì fallback xác định | `harness_runtime::SummaryProvider`, `RuntimeService::compact`, `ModelProvider`; test CAS M5 và fallback summary | `adapt`: thêm `ModelSummaryProvider` cho chat thật; `--mock`/fixture giữ deterministic; test prompt/request có giới hạn. |
| G07: `/compact [guidance]`; auto-compact theo ngưỡng tối đa một lần mỗi input; packet vẫn quá lớn thì `ContextOverflow` typed; giữ CAS | `RuntimeService::compact` có CAS, `RunRequest` được clone qua continuation | `adapt`: gọi compact thủ công và trước dispatch; chia sẻ cờ one-shot qua continuation; rebuild một lần rồi trả lỗi typed nếu vẫn vượt. |
| G07: context window theo config model → bảng nội bộ → 8192 kèm Notice; `/context` hiển thị channel/block/token và lý do drop | `ModelConfigV2`, `ContextBuildResult` đã có metadata nhưng chưa overlay | `adapt`: resolve precedence và giữ đủ manifest/omission để vẽ overlay. |
| G07: memory contribution bị validator từ chối phải phát Notice, không nhận block đó | Runtime hiện bỏ contribution sai âm thầm; `SessionEvent::Notice` đã có | `adapt`: báo một Notice tại điểm từ chối, không đưa block sai vào context. |
| G08: title tự sinh, `/rename`, picker hiện title/model/time | `session_settings` và model G03 đã có; picker thiếu title/time | `adapt`: lưu title tối đa 60 ký tự, rename persistent, bổ sung metadata vào picker. |
| G08: `ha chat --continue` và `ha sessions list --cwd` chỉ chọn session project hiện tại | `SqliteStore::list_sessions` chưa project-filtered; chưa có continue/list command | `adapt`: chọn session mới nhất và lọc qua project/store identity hiện có. |
| G08: `/clear` reset view; `/diff` đọc-only, có diff từ đầu session và thay đổi chưa commit | `/new` và `GitDiff` có sẵn; thiếu mốc đầu session | `adapt`: giữ scrollback khi clear view; ghi HEAD/hash đầu session, hiển thị cả hai diff mode. |
| G08: `/undo` chỉ phục hồi khi hash hiện tại bằng `after_hash`, dùng before artifact G04 và approval/receipt riêng | Receipt G04 có hash/artifact; chưa có undo action | `missing`: thêm restore action host-only, kiểm tra lại hash ngay trước ghi và bỏ qua file đã đổi. |
| G08: `/export` JSONL/Markdown không lộ secret; `/copy` chép câu trả lời cuối | Event journal, redaction và arboard có sẵn; chưa có export/clipboard text setter | `adapt`: export qua write approval hiện có, che credential; thêm clipboard setter và báo lỗi rõ. |
| G09: bounded JSON qua process runner; pre-hook exit 2/nonzero/timeout chặn fail-closed, ghi `blocked_by_hook`; stdout không cấp quyền | `harness-tools::process` và action gate hiện có; chưa có runner/hook selector | `adapt`: typed hooks, JSON ≤8 KiB, timeout ≤60 giây, không diễn giải stdout thành permission. |
| G09: chỉ nạp hook user config hoặc project đã trust; `/hooks` hiện nguồn | Config resolver đã biết project trust; project hooks hiện bị bỏ qua | `adapt`: merge đúng trusted sources, báo lý do bỏ untrusted project và hiện provenance. |
| G09: post/stop/notification chỉ quan sát; bell/notification chỉ khi interactive | `ToolObserver`, `TurnObserver`, `SessionEvent::Notice`, `UiConfigV2` và renderer có sẵn | `adapt`: dùng bounded runner; lỗi hook quan sát không đổi receipt; BEL không bao giờ xuất ra headless.

### Failure model and defenses / Mô hình lỗi và phòng vệ

| Failure mode | Required defense and executable oracle |
|---|---|
| A summary races with a correction and overwrites current context | Preserve M5 CAS/rebase untouched; `milestone_m5` stays green and its concurrency test remains required. |
| Auto-compaction loops across driver continuations or silently dispatches an oversized request | Shared once-per-input attempt state; retry context build only after one compaction; typed `ContextOverflow` if still over; test records one compaction and no provider dispatch afterward. |
| Provider summary is empty, over budget, fails, or times out | Bound summary output and timeout; deterministic fallback; packet/checkpoint says `deterministic_fallback`, not `model`. |
| Undo overwrites newer work, reads another task's artifact, or races after approval | Scope artifact to source task; verify artifact digest and path; protected/escape validation before panel; repeat `current_hash == after_hash` immediately before write; own receipt. Required negative control removes this guard and must make V23 red. |
| Export leaks credentials or writes without consent | Redact credential values and known secret fields; use existing mutating write approval; test exported JSONL and Markdown contain no supplied credential value. |
| Hook stdout or a timeout turns Ask into Allow; untrusted repo command executes; provider key leaks | Hook result can only continue or block; exit 0 preserves existing policy decision; any block/timeout is fail-closed; trusted-source filter; `harness-tools::process` allowlist and 8 KiB bounded JSON. V25 includes stdout `allow` and still observes Ask; temporarily interpreting that text as permission must make the test fail. |
| Oversized hook input/output or failed observational hook stalls/changes tool outcome | Bound args/timeout/captured output; pre-hook failure blocks; post/stop/notification failure is Notice only and never changes a committed receipt. |

| Tình huống lỗi | Bảo vệ bắt buộc và oracle kiểm thử |
|---|---|
| Summary ghi đè correction mới do race | Giữ CAS/rebase M5; chạy lại test concurrency `milestone_m5`. |
| Auto-compaction lặp qua continuation hoặc gửi request quá lớn | Dùng cờ one-shot chung; compact rồi rebuild một lần; nếu vẫn quá lớn trả `ContextOverflow` typed và không dispatch provider. |
| Summary provider rỗng/quá dài/lỗi/timeout | Giới hạn output và thời gian; fallback deterministic; metadata ghi `deterministic_fallback`. |
| Undo ghi đè file mới, lấy artifact task khác hoặc có race sau approval | Scope artifact theo task, xác minh digest/path và hash lần cuối ngay trước ghi; V23 phải đỏ khi bỏ guard. |
| Export lộ credential hoặc ghi file không xin phép | Redact value/field secret, đi qua write approval; JSONL và Markdown không chứa credential giả lập trong test. |
| Hook stdout/timeout/untrusted command cấp Allow hoặc lộ key | Hook chỉ tiếp tục hoặc block; timeout/nonzero chặn fail-closed; trusted-source filter và process allowlist; V25 kiểm tra stdout `allow` vẫn phải hỏi Ask. |
| Hook payload/output quá lớn hoặc post/stop lỗi làm đổi tool outcome | Giới hạn JSON/timeout/output; pre-hook lỗi chặn; lỗi hook quan sát chỉ Notice, không đổi receipt đã settle. |

**Measured D6 adjustment / Điều chỉnh D6 theo số đo:** The plan's nominal
`compaction_reserve_tokens = 16,384` conflicts with the final context fallback
of 8,192 tokens (and the runtime's 1,024-token output reservation): the literal
auto-compaction threshold would be `8,192 - 1,024 - 16,384 = -9,216`, so even an
empty packet is always over threshold and cannot recover after one compaction.
Keep 16,384 as the nominal/default reserve, but cap the effective reserve to
`context_window / 4`; at the 8,192 fallback this is 2,048, yielding a usable
threshold of 5,120 tokens. Larger configured model windows retain the full
16,384 reserve. This is the only recorded D1–D11 change and is authorized by the
assignment's measured-contradiction exception. / Default reserve danh nghĩa
16.384 token trong plan mâu thuẫn với context fallback 8.192 token (và output
reservation 1.024): ngưỡng tính nguyên văn là
`8.192 - 1.024 - 16.384 = -9.216`, khiến cả packet rỗng cũng luôn vượt ngưỡng
và không thể khôi phục sau một lần compact. Giữ 16.384 là reserve mặc định danh
nghĩa, nhưng giới hạn reserve hiệu lực ở `context_window / 4`; với fallback
8.192 thì reserve hiệu lực 2.048, ngưỡng dùng được là 5.120 token. Model có
window lớn hơn vẫn dùng đủ reserve 16.384. Đây là thay đổi D1–D11 duy nhất được
ghi nhận và nằm trong ngoại lệ số đo của assignment.

### Ordered RED → GREEN and acceptance tests / Thứ tự RED → GREEN và test acceptance

**EN:** Implement and prove tests red before the corresponding source change. Exact plan selectors are retained; additional selectors cover every requested CLI behavior. **VI:** Trước mỗi sửa đổi source, test tương ứng phải được chứng minh đỏ; giữ nguyên selector của plan và bổ sung selector cho mọi hành vi CLI được yêu cầu.

1. `g07_compact_uses_the_model_and_records_the_source`; `g07_compact_falls_back_when_the_provider_fails`; `g07_auto_compaction_triggers_at_threshold_and_never_loops`; `g07_context_window_comes_from_config_then_table_then_default_with_notice`; `g07_rejected_memory_contribution_emits_a_notice`.
2. `g08_continue_picks_the_newest_session_of_this_project_only`; `g08_rename_persists_and_shows_in_the_picker`; `g08_clear_starts_a_new_session_and_clears_only_the_viewport`; `g08_diff_since_session_start_uses_the_recorded_base`; `g08_undo_restores_only_files_whose_hash_is_unchanged`; `g08_undo_is_an_approved_action_with_its_own_receipt`; `g08_export_contains_no_credential_values`; `g08_copy_writes_the_last_answer_to_clipboard`.
3. `g09_pre_tool_use_exit_2_blocks_and_records_the_reason`; `g09_hook_cannot_turn_ask_into_allow`; `g09_hook_timeout_blocks_not_allows`; `g09_untrusted_project_hooks_do_not_run`; `g09_hook_receives_bounded_json_without_secrets`; plus explicit selectors for observational post/stop, notification/bell, and invalid hook config.

Plan acceptance mapping: V21 → the two compact summary selectors; V22 → auto-compaction and context-window selectors; V23 → both undo selectors; V24 → project-scoped continue selector; V25 → pre-hook exit 2, no-hook-grant, and timeout selectors. Add selectors to the M5 registry only after they exist; do not set `accepted` before gates pass. / Ánh xạ acceptance plan: V21 → hai selector summary/compact; V22 → auto-compaction và context-window; V23 → hai selector undo; V24 → continue giới hạn theo project; V25 → pre-hook exit 2, không cho hook tự cấp quyền và timeout. Chỉ thêm selector vào registry M5 sau khi test có thật; không đặt `accepted` trước khi gate pass.

### Setup, gates, and authority / Setup, gate và quyền

- **EN:** No new dependencies or environment installs. Hook tests run a real local fixture child; summary tests use a controlled fake provider; clipboard tests isolate the existing `arboard` boundary. No paid calls. **VI:** Không thêm dependency hay cài môi trường. Hook test chạy fixture child cục bộ thật; summary test dùng fake provider có kiểm soát; clipboard test cô lập boundary `arboard` hiện có. Không gọi API trả phí.
- **EN:** Run RED tests first, then format, Clippy `-D warnings`, CLI tests, `milestone_m5`, `phase_p2`, all `interactive_*` targets with one test thread, and the H launch gate. Require M5 `passed` and H `failures: []`; run schema generation/docs self-test when inputs change. **VI:** Chạy test RED trước, sau đó format, Clippy `-D warnings`, CLI tests, `milestone_m5`, `phase_p2`, mọi target `interactive_*` với một test thread và gate H. Yêu cầu M5 `passed`, H `failures: []`; chạy sinh schema/docs self-test khi đầu vào thay đổi.
- **EN:** Negative controls: remove the undo hash guard temporarily and prove V23 fails; make hook stdout say `allow` and prove V25 still asks. Restore code and rerun clean selectors. Never weaken tests or hand-edit generated schemas. **VI:** Đối chứng âm: tạm bỏ hash guard để V23 fail; cho hook in `allow` để chứng minh V25 vẫn hỏi. Khôi phục code và chạy selector sạch. Không nới test hoặc sửa tay schema sinh tự động.
- **EN:** These gates do not change CP-B's historical status. This assignment ends after G09/CP-C. Evidence records commands/counts, source digest, OS/toolchain, failed attempts and `not_run`; do not start G10 here. **VI:** Gate hiện tại không thay trạng thái lịch sử CP-B. Assignment dừng sau G09/CP-C. Evidence ghi lệnh/số test, digest source, OS/toolchain, lượt lỗi và `not_run`; không bắt đầu G10 ở đây.

**SPEC approval status / Trạng thái duyệt SPEC:** approved by the user on 2026-09-23 before source implementation. / User đã duyệt ngày 2026-09-23 trước khi sửa source.

### CP-C implementation record / Ghi nhận triển khai CP-C

**G07:** `ModelSummaryProvider` uses the active chat provider with a fixed bounded prompt; provider failure falls back deterministically and the compacted packet records `summary_source`. `/compact [guidance]` calls the existing CAS-protected `RuntimeService::compact`. Automatic compaction runs at most once for an admitted input and returns typed `ContextOverflow` if the rebuilt packet remains too large. Context-window resolution follows model config → internal model table → 8,192 with a Notice. `/context` renders the retained context manifest, channel, block/token and omission details; rejected memory contributions emit a Notice. D6 uses the measured adjustment above. / **G07:** `ModelSummaryProvider` gọi provider chat đang chọn bằng prompt cố định có giới hạn; lỗi provider chuyển sang fallback xác định và packet lưu `summary_source`. `/compact [guidance]` gọi `RuntimeService::compact` có CAS. Auto-compaction chạy tối đa một lần cho mỗi input được nhận; nếu packet dựng lại vẫn quá lớn thì trả `ContextOverflow` có kiểu. Thứ tự context window là config model → bảng nội bộ → 8.192 kèm Notice. `/context` hiển thị manifest, channel, block/token và lý do loại; memory contribution bị từ chối phát Notice. D6 áp dụng điều chỉnh theo số đo ở trên.

**G08:** Titles and `/rename` persist through `session_settings`; session picking shows title/model/time. `ha chat --continue` and `ha sessions list --cwd` stay project-scoped. `/clear` resets the active session view, `/diff` compares the session-start base and current uncommitted work, `/undo` is a separately approved/receipted restore guarded by `after_hash`, `/export` redacts secrets in JSONL/Markdown, and `/copy` writes the last answer through the existing clipboard boundary. / **G08:** Title và `/rename` lưu trong `session_settings`; picker hiện title/model/thời gian. `ha chat --continue` và `ha sessions list --cwd` chỉ chọn session của project hiện tại. `/clear` đặt lại view của session, `/diff` so sánh mốc đầu session với thay đổi chưa commit hiện tại, `/undo` là lần phục hồi được duyệt và ghi receipt riêng, có kiểm tra `after_hash`; `/export` che secret ở JSONL/Markdown; `/copy` chép câu trả lời cuối qua clipboard boundary hiện có.

**G09:** Hooks use the existing bounded process runner and only load from user config or trusted project config. Pre-tool exit 2/nonzero and timeout block fail-closed with `blocked_by_hook`; hook stdout never grants permission. Post/stop/notification hooks are observational. Hook JSON is bounded to 8 KiB, timeout to 60 seconds; arguments are redacted. `/hooks` explains active sources, and configured BEL/notification behavior remains interactive-only. / **G09:** Hook chạy qua process runner có giới hạn hiện có và chỉ nạp từ user config hoặc project đã trust. Pre-tool exit 2/nonzero và timeout chặn theo fail-closed, ghi `blocked_by_hook`; stdout của hook không cấp quyền. Hook post/stop/notification chỉ quan sát. JSON đầu vào tối đa 8 KiB, timeout tối đa 60 giây; argument được che secret. `/hooks` giải thích nguồn đang bật; BEL/notification chỉ chạy ở interactive.

The source and targeted selectors are implemented. M5 passed. Before the upstream merge, Verify-HaLaunch did not reach `failures: []` within its initial run and three loopback retries. After integration on HEAD `5db5b139e73166412acdb873ec3de355429cd244`, H still failed on alternating loopback cases: one run reported `providers-streaming` (Anthropic Retry-After fixture), and the next reported `acceptance-launch` (i13 timeout after 190 s). The isolated Anthropic selector and i13 selector both passed; the full providers suite and all other H stages passed on the latter run. CP-C remains `implemented_unverified`, not accepted. Per-run counts, source digest, and environment are in `docs/evidence/HA_AGENT.vi.md`. No G10 work is included. / Source và test selector đã được triển khai. M5 pass. Trước khi upstream merge, Verify-HaLaunch chưa đạt `failures: []` trong lượt đầu và ba lần retry loopback. Sau khi tích hợp trên HEAD `5db5b139e73166412acdb873ec3de355429cd244`, H vẫn lỗi các case loopback luân phiên: một lượt báo `providers-streaming` (fixture Anthropic Retry-After), lượt kế báo `acceptance-launch` (i13 timeout sau 190 giây). Selector Anthropic và i13 đều pass khi chạy riêng; full providers và các stage H còn lại pass trong lượt sau. CP-C vẫn `implemented_unverified`, chưa accepted. Số liệu từng lượt, digest source và môi trường nằm trong `docs/evidence/HA_AGENT.vi.md`. Không có thay đổi G10.
