# P6 — Skills, MCP and external plugin protocol

English | [Tiếng Việt](P6_EXTENSIONS.vi.md)

Implementation runbook; status: **not started**. Estimate: 5–8 person-days. All target files, Rust tests and `ha` commands below are future outputs unless already present in the checkout. This document alone is not completion evidence.

## 1. Outcome and entry gate

Require accepted [P5](P5_MULTI_AGENT.en.md). Deliver controlled extension points—versioned skills, an MCP client and bounded stdio tool/provider bridges—without weakening durability, policy or scoped memory.

Read the [handbook](README.en.md), [plan](../RUST_HARNESS_PLAN.en.md), [plugin contract](../PLUGIN_ARCHITECTURE.en.md), [memory contract](../MEMORY_AND_CONTINUITY.en.md) and [acceptance map](ACCEPTANCE_MAP.en.md). Inspect actual predecessor evidence before coding.

## 2. Owned scope and target files

Own extension modules under `harness-tools`, `harness-providers`, `harness-kernel` and `harness-runtime`; examples of trusted profiles/skills; fixture plugin executables; `crates/harness-cli/tests/phase_p6.rs`. Add CLI plugin/config inspection and explicitly authorized local installation/registration paths. No marketplace or arbitrary native dynamic loading.

## 3. Contracts to settle before implementation

Freeze host protocol versions, handshake/capability schemas, NDJSON frame limits, invocation IDs, inflight bounds, cancellation and EOF behavior. MCP transport/version negotiation belongs to the selected official SDK contract, not assumed compatibility with the custom plugin protocol. Skill content has identity/version/provenance and remains subject to instruction/tool authority policy.

## 4. Ordered work items

### 4.1. P6-S01 — Specify trust and extension contracts

Depends on: Accepted P5.

Define plugin identity/digest, requested versus granted capabilities, allowlisted host methods and local registration semantics. Define skill discovery/version pinning and configuration trust boundaries. Record which providers/operations this release actually supports.

Evidence: No repository config can execute a plugin or obtain secrets simply by being opened.

### 4.2. P6-S02 — Implement bounded stdio transport

Depends on: P6-S01.

Launch a pinned executable with minimal environment; handshake before admitting work. Parse bounded UTF-8 frames, route unique IDs, separate protocol stdout from stderr, cap inflight calls and settle cancellation/EOF. Validate response schemas and terminate process trees on deadline.

Evidence: K12 malformed/oversized frames, duplicate IDs, output floods and ignored cancellation are bounded failures.

### 4.3. P6-S03 — Implement tool and provider bridge consumers

Depends on: P6-S02.

Expose external tools only through the P3 gate. Add a narrowly defined external model-provider capability with normalized stream/terminal frames and cancellation; validate it using a fixture provider. Preserve parent call correlation and non-escalating grants.

Evidence: External tool denial is durable; provider/request content stays recorded; reconnect cannot blindly retry a mutation.

### 4.4. P6-S04 — Implement MCP client integration

Depends on: P6-S01, P6-S02.

Select/pin a supported `rmcp` SDK version after checking official docs. Discover schemas, register scoped tools and execute through the same gate. Initially use local fixture servers; remote endpoints require explicit trust/network policy. Re-negotiate changed schemas before use.

Evidence: K13 prevents MCP/nested calls from bypassing policy or changing args after approval.

### 4.5. P6-S05 — Implement skills and profile composition

Depends on: P6-S01.

Load trusted filesystem skills lazily, with source/version and bounded content. Record admitted instructions in context manifests; skill text cannot mint permissions. Explain configuration precedence, effective row IDs, required restarts and inactive plugin reasons.

Evidence: K14 rejects untrusted repo executable/secret requests; skill updates become visible at a recorded boundary.

### 4.6. P6-S06 — Prove unload/restart compatibility

Depends on: P6-S03, P6-S04, P6-S05.

Unload an extractor/tool/provider during fixtures, restart allowed plugins with new generations and retain jobs/session evidence. Preserve old packets for replay; reject unsupported critical event/schema changes. Recheck memory revocation and sensitive data across extension paths.

Evidence: K01/K03/K05/K08/K10/K11 regressions remain valid with real extension processes.

### 4.7. P6-S07 — Integrate examples and compatibility evidence

Depends on: P6-S01..P6-S06.

Document a clone-and-run fixture plugin and a minimal skill/profile example, including trust grants and teardown. Run the full plugin suite with no paid service dependency. Label unsupported provider methods/protocol versions explicitly.

Evidence: A user can reproduce installation/registration and removal in a disposable profile without broad host privileges.

## 5. Tests and verification commands

Primary: K12, K13, K14. Regress C03/C08/C14/C20/C29 and K01/K03/K05/K08/K10/K11. Test process crashes before/after a side effect, unsupported handshake, schema changes, cancellation and stale registration removal. Validate malicious skill text is data, not permission; no real secret is placed in fixture process environments.

Future phase commands, to run after implementing the targets:

```powershell
cargo test -p harness-cli --test phase_p6 --locked
pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P6
```

The full gate includes the handbook's formatting, clippy, workspace tests, test-discovery checks and docs checks. Do not report a filtered zero-test run as passing acceptance.

## 6. Demonstration rehearsal

1. Register a local fixture tool plugin with read-only grants and inspect its digest/capabilities.
2. Execute one allowed tool and one denied mutation through the agent.
3. Crash the plugin during an uncertain call; resume without blind retry.
4. Load a versioned skill, modify its source, and verify a new context records the new version only at admission.
5. Run a local MCP fixture and the external provider fixture; unload everything and prove no residual registrations/processes.

## 7. Exit gate and forbidden shortcuts

The three extension gates and all existing plugin regressions pass. No post-freeze context injection, implicit package download, secret-bearing full environment inheritance or in-process untrusted code. Transport isolation is not advertised as OS sandboxing. Do not build marketplace, Wasm or arbitrary external loop/storage plugins.

Do not advance phases on a summary alone. Bind results to the final tested revision, report missing checks, and preserve all predecessor regressions. Never alter fixture expectations merely to make implementation pass.

## 8. Delegation and handoff

Transport S02 is owned centrally. After its contract stabilizes, bridge S03, MCP S04 and skills S05 can be assigned to distinct paths. The integrator owns shared protocol versions and config registry. P7 receives compatibility fixtures, supported-method inventory and shutdown/error evidence.

Deliver `docs/evidence/P6.en.md` and `P6.vi.md`, plus a resumable handoff under `docs/handoffs/`, following the handbook. Include completed step IDs, pending failures, schema changes, commands and next action. Publication requires explicit authorization in the coding assignment.

## 9. Ready-to-use agent prompt

```text
Implement P6 only. Read docs/implementation/README.en.md and
P6_EXTENSIONS.en.md in that directory, all linked architecture contracts,
and applicable repository instructions. Verify the predecessor gate from source/evidence.
Create the phase SPEC, then implement P6-S01..P6-S07 in dependency order.
Stay within this phase's owned scope; preserve unrelated changes and accepted contracts.
Use real components for acceptance tests, with mocks only at appropriate external boundaries.
Run the phase gate and predecessor regressions; deliver bilingual evidence and a restart handoff.
Stop before the next phase. Do not spawn agents, commit, push or publish unless explicitly assigned.
If prerequisites or required verification are unavailable, report the exact gap without claiming completion.
```
