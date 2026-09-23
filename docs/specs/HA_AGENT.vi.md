# SPEC HA_AGENT — checkpoint CP-A (G01–G03) / Đặc tả HA_AGENT — checkpoint CP-A

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

## Preflight observation (before RED)

The M6 prerequisite gate was attempted on the current working tree and stopped at
its format step (exit 1), before running milestone tests. Rustfmt reported existing
format differences in crates/harness-cli/src/web/mod.rs,
crates/harness-cli/src/interactive/memory.rs,
crates/harness-cli/tests/milestone_m10.rs, crates/harness-memory/src/lib.rs, and
crates/harness-store-sqlite/src/store.rs. These are baseline formatting findings,
not G01–G03 failures. No test or implementation file had been changed at that point.
