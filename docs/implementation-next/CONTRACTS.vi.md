# Contracts triển khai — default cụ thể cho DeepSeek

[Sổ tay](README.vi.md) · [Master plan](../HARNESS_MASTER_PLAN.vi.md)

## 1. Mức độ cố định

Các signatures dưới đây là pseudocode, chưa là API Rust đã tồn tại. **Semantics/invariants là yêu cầu**, tên type/cách đóng hộp future là implementation choice. SPEC ghi quyết định khác default và test chứng minh tương đương. Không implement production stub cho một capability chưa đến mốc; trả typed unsupported hoặc không đăng ký capability.

Default IDs là typed UUIDv7 sinh tại host; UUID không là thứ tự giao dịch. Time dùng UTC timestamp cho persistence và monotonic deadline trong process; clock trait cho tests. Tiền/số lượng token dùng integer với checked arithmetic, không dùng float cho account settlement. Hash canonical JSON có version/algo và domain tag; fields có thể khác order vẫn cùng semantic hash.

`Error { code, retry_class, safe_message, correlation_id, details_ref? }`: codes ổn định, display message không là protocol; không nhét bearer token/raw endpoint query vào error. Event envelopes versioned; unknown optional fields có compatibility policy, unknown critical event chặn mutation/replay projection tại điểm đó. Config strict reject unknown field trừ namespace extension được schema cho phép.

## 2. Ownership và scope

`ScopeContext` do application tạo từ transport identity, chứa principal, project/workspace, task/session, capabilities, config revision và owner generation. Tool args không được tự thay scope. Scope filter trước read/search/export/rank; artifact hash trùng không tự cho quyền đọc.

| Writer | Domain sở hữu | Thao tác qua port |
|---|---|---|
| TransactionCoordinator | Event log, projections, inbox/outbox, claims và settlement | Transaction methods chuyên biệt; không expose arbitrary SQL cho plugin |
| RuntimeService | Run/step transitions và requests | Gọi coordinator; không mutate rows ngoài transaction |
| ToolService | Proposal/policy/intent/receipt | Coordinator commit, executor chỉ tạo outcome |
| MemoryService | Candidate/publish/invalidate | CAS + job settlement qua coordinator |
| Orchestrator | DAG/child/integration status | Child completion + parent inbox atomically |

Một `SessionRevision(u64)` tăng khi journal append thành công. Không dùng revision global của mọi session để CAS context một session. State checkpoint phải nói source event sequence; config/tool/catalog revisions là namespace riêng.

## 3. StorePort và transaction boundaries

```text
admit_input(scope, input_id, payload_hash, expected_seq, content)
  -> Admitted { event_id, seq, state_revision } | Duplicate(same_result)
     | IdempotencyConflict | RevisionConflict
claim_run(scope, command_id, expected_owner_generation) -> RunLease
append_domain_change(lease, expected_seq, validated_change) -> CommitRef
freeze_step(lease, source_seq, packet, provider_request, budget_reservation)
  -> FrozenStepRef | RevisionConflict
admit_invocation(lease, proposal, grant, expected_workspace) -> IntentRef
settle_invocation(lease, intent_id, receipt, projection_change) -> CommitRef
settle_child(lease, child_result, parent_delivery) -> CommitRef
recover_readonly(session_id) -> RecoveryView
```

Duplicate cùng ID + payload hash trả kết quả cũ; cùng ID khác payload reject, không overwrite. Claim phải conditional update/unique transaction, không read-then-write race. Receipt immutable; correction/reconciliation là event mới tham chiếu receipt cũ. `outcome_unknown` không đồng nghĩa failed/no side effect.

Core tables M1: metadata/migrations, ownership, projects/workspaces, tasks/sessions, inputs, events, projections/checkpoints, commands/inbox/outbox, artifact metadata/references. Minimum unique constraints: `(session_id, seq)`, event ID, `(scope,input_id)`, delivery dedupe key. Các table run/step có thể được M3 thêm; public IDs/types tồn tại từ M0.

M3 migrations thêm runs/steps/attempts/frozen_requests/questions/budget_reservations. M4 thêm approvals/intents/receipts. M5 thêm history index/lineage/notes. M7 thêm assets/versions/bindings/dependencies/extraction jobs/dispositions. M8 thêm DAG/children/workspace leases. Không dùng một giant mutable JSON blob thay constraint fields cần query/unique/CAS.

Database+filesystem không chung transaction. Artifact bytes stage/flush/publish trước reference commit; orphan cleanup qua reachability. Reference có content hash, captured length, MIME, scope và retention pin. Failure sau receipt commit trước UI notification vẫn là durable success, không sửa receipt thành failed.

## 4. Agent transitions và tool continuation

Input admission tạo durable input/turn, **không** mỗi model step tạo user input mới. Một logical run có nhiều steps; provider retry có attempt mới nhưng frozen request giữ identity/hash. New step tạo request mới với committed tool results. Tool provider call ID chỉ định pairing; host InvocationId là authority, map cố định trong step, không dùng provider ID làm global unique ID.

```text
start_or_resume_run -> recover pending effects -> snapshot
 -> compile -> reserve/freeze -> model stream/attempt
 -> assemble complete response
 -> tool proposals? validate all shapes, persist per-call intent, execute/settle
 -> next step with correlated results
 -> otherwise evaluate terminal/criteria -> finalize or bounded continuation
```

Malformed batch không thực thi call có arguments chưa hoàn chỉnh. Những calls đã commit ở step trước không bị chạy lại khi lần sau parser lỗi. Batch read-only có thể parallel nếu resource conflicts được xác định; mutate sequential default. Pair failed/denied tools bằng result typed để model có thể điều chỉnh; systemic store/ownership failure dừng loop, không biến thành lời model tùy ý.

Agent states không được từ terminal trở lại running do notification trễ. Resume run interrupted/waiting là command có CAS; completed run cần run mới gắn cùng task. Task accepted chỉ khi required criteria đều satisfied, không pending effect/check và evidence còn đúng fingerprint. Human acceptance được ghi actor/source; không giả thành tự động test-pass.

## 5. Provider protocol

```text
ProviderMessage = System(Text) | User(Blocks)
                | Assistant { text, calls: [Call{id,name,args}] }
                | ToolResult { call_id, payload, outcome }
ModelProvider.start(FrozenRequest, Cancellation) -> Stream<ProviderEvent>
ProviderEvent = TextDelta | ToolDelta{choice,index,id?,name?,args_fragment?}
              | Usage{actual_or_estimated} | Terminal{finish_reason} | Error
```

Protocol validator trước dispatch: ToolResult chỉ trỏ call đã admitted trong visible paired transcript; IDs unique trong step; không giả ToolResult thành User text. Normalizer phân biệt provider raw events và canonical events. Durable records lưu normalized request/response + capability/protocol revision, không credentials hoặc private internal reasoning fields không cần cho continuation.

Assembler map `(choice,index)` lưu ID/name/args qua chunks; đối chiếu ID xung đột, reject missing required identity ở terminal. Tokenizer/estimator lấy capability model; unavailable dùng conservative bound được SPEC định nghĩa. Output/token/frame/tool-count caps configurable; M2 chọn số default sau fixture memory measurements, không dùng unlimited.

Transport terminal marker khác assistant final text: length finish/refusal/EOF có stop reason riêng. Mất stream sau text không auto-retry side effect hoặc coi final hoàn chỉnh. Usage ledger dedupe theo attempt/provider usage sequence; có usage cumulative thì không cộng mỗi chunk thành nhiều lần.

## 6. Tool authority và evidence

```text
ToolDescriptor { id, revision, schema_digest, effect_class, capabilities }
Proposal { invocation_id, scope, call_id, action, action_hash, workspace_ref }
Grant { principal, task, session, invocation, action_hash, workspace_fingerprint,
        policy_revision, tool_revision, expiry, allowed_effects }
Receipt { intent_id, executor_revision, outcome, started/finished,
          before/after_fingerprint, exit_code?, artifacts, errors, uncertainty }
```

Grant one-shot consume + intent commit chung transaction; permission theo scope dài hạn có record khác và được recheck cho mỗi proposal. Clarification answer không chuyển thành grant. Revalidate authoritative policy/ownership/fingerprint ngay trước dispatch; revoke xử lý cả queued calls. Không thể atomically fence arbitrary external process/file system bằng DB: thu hẹp TOCTOU, use OS-safe handles khi khả thi, báo limitations đúng host mode.

Evidence typing: `FileChanged(path,before,after,receipt)`, `CheckExecuted(command_digest,workspace_digest,exit,outcome,receipt)`, `ArtifactProduced(ref)`. Không detect tests pass chỉ bằng grep string “passed”. Test adapter xác minh execution identity, exit code, summary/test count khi framework hỗ trợ; zero tests không thỏa criterion có expected tests. Unknown framework chỉ chứng minh command execution; chất lượng deliverable vẫn cần criteria.

## 7. Context và provenance

```text
compile(ReadSnapshot, EffectiveConfig, Contributors, Budget) -> CandidatePacket
admit_and_freeze(candidate.expected_source_seq, packet_hash, source_manifest)
contributor.collect(readonly_scope, token_limit) -> ProposedBlock[]
```

M3 minimum compiler đã có policy/instruction authority, state, paired tail và manifest. M5 thêm optional sources, history/notes, summaries, checkpoint CAS. M6/M7/M8 chỉ thêm contributors qua port. Mandatory block overflow phải block/pause, không silent truncate instruction.

`SourceRef { kind, scope, source_id, version/digest, range?, availability }`; exact stored bytes hash khác current file fingerprint. Citation valid có nghĩa nguồn tồn tại/được phép, không bảo đảm assertion đúng. History index rebuild từ journal, lexical match có rank không đổi authority. Revocation filter được áp khi read và admission; pinned historical packet vẫn là audit nhạy cảm với access policy hiện hành.

Compaction source watermark chụp trước generation; final CAS cùng source revision. Candidate computation không giữ SQLite write transaction khi chờ model. Failed candidate không ghi checkpoint active; bounded rebase hoặc pause. `fork` copy references theo explicit lineage policy; không copy approvals/ownership, run IDs hoặc unresolved effects như thể được phép chạy lại.

## 8. Budgets, jobs và delegation

`reserve(budget_id, operation_id, expected_revision, upper_bound)` atomic; `settle(reservation_id, measured_usage)` idempotent; unknown outcome giữ conservative reservation đến reconcile/expiry policy có log, không release mù. Parent child/extraction/evaluator dùng cùng hierarchy nhưng sublimits riêng. Không lock database qua await network.

Job identity `(source_scope, source_range, extractor_version)`; lease generation checked khi settle; source dispositions và publish/cursor cùng transaction. Invalid/filtered sources phải có disposition để cursor không mắc mãi. Child result keyed stable execution ID, durable outbox message có delivery ID; receiver transaction dedupe + acknowledge, notification chỉ tối ưu latency.

Child TaskBrief có source snapshot/criteria/file scopes/budget/grants. Parent wait nhả compute slot nhưng vẫn sở hữu task data; scheduler không giữ process/model permits cho idle actors. External remote handle có server identity, capability version và credentials reference; poll service không tiêu tốn model step cho mỗi lần hỏi status.

## 9. CLI và compatibility default

CLI JSON envelope: `schema_version`, `command`, `request_id`, `status`, `data`, `error?`; stdout machine-readable, progress/log stderr. Human-readable mặc định. Exit codes đề xuất M0 chốt: 0 command succeeded (không đồng nghĩa task accepted), 2 invalid usage/config, 3 waiting/input/action required ở non-interactive, 4 execution failed, 5 ownership/conflict, 130 user cancel. JSON có task acceptance/run status tách biệt.

Unimplemented commands không xuất hiện như tính năng hoạt động. `--mock`/fixture profile explicit; live default chỉ khi config provider hợp lệ và user đã chọn. Data directory mới có marker/version; reject legacy directory nếu chưa có importer. M9 migration/import support là assignment riêng, không xóa data không nhận diện.

## 10. Những quyết định phải chốt ở đúng mốc

M0 pin Rust/toolchain/test interface theo môi trường thực có; M1 chọn SQLite binding/locking và durability pragmas; M2 kiểm tra DeepSeek docs chính thức/capabilities/endpoints lúc coding; M4 chọn OS process/path primitives; M6 pin MCP SDK/spec compatibility; M10 chốt Web stack; M12 chọn backend sau capability spike. Không cần hỏi user cho những lựa chọn kỹ thuật thường lệ phù hợp defaults; ghi ADR và fixture proof. Không bịa API hoặc hardcode model pricing từ tài liệu cũ.
