# Acceptance specifications A01–A36

**Planning only · 19/09/2026.** [Sổ tay](README.vi.md) · [Contracts](CONTRACTS.vi.md) · [Manifest](manifest.json)

## 1. Tổ chức fixtures và test oracles

Các tên tests dưới đây là **tên dự kiến**, chưa có test executable. Khi coding, dùng exact names hoặc cập nhật runtime registry và SPEC cùng change. Không nhận docs kiểm tra tên case là runtime acceptance.

Mỗi case dùng disposable root có ownership marker, isolated data dir, injected deterministic clock/IDs khi cần. Crash test chạy component trong child process: test parent chờ named barrier, hard kill ở boundary cần kiểm tra, reopen ở process mới, assert persisted state và external effects. Simulated exception trong cùng process không thay crash test vì destructors/cleanup có thể che bug.

Negative controls chỉ dùng temporary variants/test adapters hoặc feature test-only; không để insecure branches enabled trong production. Không yêu cầu mutation testing toàn codebase. Case phải fail khi invariant của nó bị cố ý phá trong phạm vi kiểm chứng; never alter expected outcomes to match broken behavior.

Provider/network fake được phép. Filesystem/process/store thật phải tham gia cases liên quan; strict confinement dùng backend thật. Tests không yêu cầu paid provider credentials. Bounded optional live smoke được ghi riêng, không thay deterministic gates.

A11 có phần question ở M3 và real tool approval ở M4. A19 có phần summarizer ở M5 và extractor ở M7. Runtime registry theo dõi **subcase selectors**; milestone sớm chỉ verify phần đến hạn, toàn ID chỉ completed tại mốc cuối. Các acceptance IDs khác do một milestone sở hữu, nhưng predecessor regressions tiếp tục chạy.

## 2. Case specifications

### A01 — Input sau ACK/crash

**Owner milestone:** M1. **Planned test:** `a01_input_ack_dedupe`.

- **Setup:** Store thật trong child process, fixed InputId và payload hash.
- **Trigger:** Parent đợi ACK được flush rồi kill child; reopen và submit cùng ID lần nữa, sau đó cùng ID khác payload.
- **Oracle bắt buộc:** Đúng một input/event/projection change; duplicate same payload trả original commit; changed payload IdempotencyConflict.
- **Negative control:** Di chuyển ACK trước transaction commit phải làm fail khi kill ở barrier.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A02 — Writer ownership và stale generation

**Owner milestone:** M1. **Planned test:** `a02_owner_generation`.

- **Setup:** Hai fixture host độc lập cùng data directory; giữ old generation token.
- **Trigger:** Cho host B tranh claim khi A còn owner, rồi owner mới sau shutdown; thử old token append/finalize.
- **Oracle bắt buộc:** Không hai active writer; old generation write/finalize reject, journal sequence không bị trùng.
- **Negative control:** Bỏ generation check làm stale token test fail; chỉ Mutex trong process không qua test hai process.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A03 — Receipt đã commit, chưa checkpoint

**Owner milestone:** M4. **Planned test:** `a03_receipt_before_checkpoint`.

- **Setup:** Runtime/store/real patch tool; invocation fixed ID; file change counter hoặc inspect journal.
- **Trigger:** Dừng ở barrier ngay sau receipt commit trước checkpoint, hard kill và reopen/continue.
- **Oracle bắt buộc:** Receipt/result phục hồi từ journal; tool không execute lần hai; step tiếp theo nhận tool result đã commit.
- **Negative control:** Bỏ fold tail hoặc dedupe làm dispatch count tăng hoặc result mất.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A04 — Side effect chưa receipt

**Owner milestone:** M4. **Planned test:** `a04_effect_before_receipt`.

- **Setup:** Fixture process ghi marker ra file; host chuẩn bị intent trước chạy.
- **Trigger:** Kill host ở barrier executor confirmed marker nhưng trước receipt transaction; reopen.
- **Oracle bắt buộc:** Marker còn; invocation outcome_unknown; không tự rerun; explicit reconcile ghi event mới.
- **Negative control:** Đổi pending invocation thành retryable failed làm marker count tăng và test fail.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A05 — Snapshot lỗi và unknown event

**Owner milestone:** M1. **Planned test:** `a05_checkpoint_compatibility`.

- **Setup:** Journal nhiều events + hai verified checkpoints; clone disposable database.
- **Trigger:** Corrupt newest checkpoint hash, chạy recover; variant chèn unsupported critical event ở tail.
- **Oracle bắt buộc:** Fallback checkpoint trước + fold đúng; critical event chặn mutable execution; inspect nêu blocked reason.
- **Negative control:** Bỏ hash validation hoặc skip critical event phải fail expected state/blocked assertion.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A06 — SSE partial và multiple tool calls

**Owner milestone:** M2. **Planned test:** `a06_sse_multicall`.

- **Setup:** Fake HTTP endpoint gửi raw UTF-8 bytes theo partitions; expected calls viết độc lập.
- **Trigger:** Xen kẽ 2 indexes, ID chỉ chunk đầu, mixed text/tool, usage-only frame; variants missing terminal/malformed JSON.
- **Oracle bắt buộc:** Calls giữ exact IDs/names/args; progress sớm; invalid stream never dispatchable; usage parsed không rơi mất.
- **Negative control:** Dùng first-tool-only parser hoặc ID fallback bị exact message assertion bắt.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A07 — Provider errors/retry/cancel

**Owner milestone:** M2. **Planned test:** `a07_provider_fail_cancel`.

- **Setup:** Fake HTTP có routes 401/429/503/hang; injected retry clock/barriers.
- **Trigger:** Gửi các requests với retry budget; cancel ở connect/read/backoff; kiểm tra sentinel token.
- **Oracle bắt buộc:** 401 no retry; transient bounded attempts; cancellation settles; error/log không chứa token; stream closed.
- **Negative control:** Bỏ cancellation ở backoff hoặc retry mọi status làm count/deadline test fail.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A08 — Coding fail rồi sửa thành pass

**Owner milestone:** M4. **Planned test:** `a08_coding_e2e`.

- **Setup:** Disposable Git repo có parser bug và test runner thật; scripted provider trả calls dự định.
- **Trigger:** Read→patch chưa đủ→tests fail→read result→patch đúng→tests pass→final.
- **Oracle bắt buộc:** Tool responses correlated, file/diff đúng, failure và success receipts tồn tại; final criteria dùng cuối workspace digest.
- **Negative control:** Mock test runner luôn pass hoặc gắn evidence vào trước patch cuối phải bị reject.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A09 — Run terminal không đồng nghĩa task done

**Owner milestone:** M3. **Planned test:** `a09_terminal_acceptance`.

- **Setup:** Criteria yêu cầu evidence chưa có; provider fixtures empty/length-cap/says-done.
- **Trigger:** Chạy tới terminal với từng response variant.
- **Oracle bắt buộc:** Run có explicit stop reason; task/acceptance không satisfied khi thiếu evidence; no infinite retry.
- **Negative control:** Dùng assistant final presence làm accepted phải fail.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A10 — Goal bounded continuation

**Owner milestone:** M3. **Planned test:** `a10_goal_no_progress`.

- **Setup:** Goal với budget/continuation cap và progress signature cố định.
- **Trigger:** Evaluator đề xuất needs_work lặp, external_wait và malformed output trong các variants.
- **Oracle bắt buộc:** No-progress/cap dừng với state bền vững; external wait không model polling; malformed evaluator không satisfied.
- **Negative control:** Reset counters mỗi hidden continuation phải bị call-count cap assertion bắt.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A11 — Question và approval qua restart

**Owner milestone:** M3,M4. **Planned test:** `a11_human_input_grant`.

- **Setup:** M3 question workflow thật; M4 thêm normalized invocation proposal và grant bound.
- **Trigger:** Pause→kill/reopen→answer duplicate; wrong request ID/scope; expire/revoke approval trước execute.
- **Oracle bắt buộc:** M3: answer đúng một lần, scope đúng; M4: consume exact grant một lần, expiry/revoke không execute. A11 hoàn tất tại M4.
- **Negative control:** Treat arbitrary answer như permission hoặc consume twice phải fail; chỉ M3 half chưa đủ complete.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A12 — Correction trong lúc compaction

**Owner milestone:** M5. **Planned test:** `a12_compaction_cas`.

- **Setup:** Checkpoint và decisions A; summarizer fixture giữ generation barrier.
- **Trigger:** Khi candidate dựa A đang dựng, commit user decision B; release summarizer rồi CAS.
- **Oracle bắt buộc:** Candidate A bị reject/rebase, active instruction B và correct source seq; không overwrite B.
- **Negative control:** Đọc expected sequence sau generation thay vì trước làm test thấy stale checkpoint.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A13 — Process hủy khi còn đợi

**Owner milestone:** M4. **Planned test:** `a13_queued_process_cancel`.

- **Setup:** Process A giữ execution permit; process B có executable ghi marker nếu spawn.
- **Trigger:** Queue B, cancel B trước permit release; cho A xong.
- **Oracle bắt buộc:** B không spawn, marker absent; state canceled và permits/drain sạch.
- **Negative control:** Check cancellation chỉ sau spawn tạo marker và fail.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A14 — Approval binding và consume race

**Owner milestone:** M4. **Planned test:** `a14_approval_binding`.

- **Setup:** Proposal A/B cùng command nhưng khác invocation/task/session; grant cho A.
- **Trigger:** Dùng grant cho B; song song hai consumers cho A; thử policy/workspace revision đổi.
- **Oracle bắt buộc:** B denied; chỉ một intent cho A; revisions mismatch reject; no unintended executor call.
- **Negative control:** Bỏ invocation khỏi binding hash hoặc read/consume riêng transaction bị negative control bắt.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A15 — Path và stale file safeguards

**Owner milestone:** M4. **Planned test:** `a15_path_patch_safety`.

- **Setup:** Temp trusted root + outside canary, Unicode/CRLF/binary fixtures; symlink/junction tùy OS.
- **Trigger:** Traversal/alias/junction escape; modify file sau prepare trước apply; file locked/read failure.
- **Oracle bắt buộc:** Không đọc/ghi outside scope qua file tools; stale patch reject; expected bytes/CRLF giữ; unsupported OS case ghi đúng.
- **Negative control:** Disable before-hash hoặc canonical containment làm outside/stale sentinel assertions fail.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A16 — Process tree và environment

**Owner milestone:** M4. **Planned test:** `a16_process_tree_env`.

- **Setup:** Fixture parent spawn grandchild heartbeat; host env có fake credentials/helper/socket sentinel.
- **Trigger:** Timeout/cancel process tree; child attempts report visible env.
- **Oracle bắt buộc:** Descendants terminated/reaped theo backend contract, heartbeat stopped; scoped env không có secret/helper trừ explicit grant.
- **Negative control:** Kill parent only hoặc inherit full env làm assertions fail; không chỉ check exit code của parent.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A17 — Long logs và quota failures

**Owner milestone:** M4. **Planned test:** `a17_output_quota`.

- **Setup:** Process output lớn hơn preview, có head/tail sentinel; artifact quota và failure injector.
- **Trigger:** Stream output, read paginated tail; variant disk full trước publish.
- **Oracle bắt buộc:** Preview/ref/capture length/hash/truncated đúng; stored tail đọc được nếu captured; missing bytes báo rõ, no nonexistent artifact ref.
- **Negative control:** Memory-only truncation nhưng gắn full-log ref phải fail exact captured bytes assertions.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A18 — Exact source sau năm compactions

**Owner milestone:** M5. **Planned test:** `a18_history_after_compaction`.

- **Setup:** Input nguồn có identifier XYZ-731 và constraints; summary fixture cố ý không giữ identifier.
- **Trigger:** Ép năm compactions, kill/reopen; provider fixture gọi history_search/read rồi tạo output từ kết quả.
- **Oracle bắt buộc:** Source ID scoped/digest đúng; recovered identifier đúng trong actual output; 5 checkpoints không mất mandatory constraints.
- **Negative control:** Disable history archive/index source access làm recovery fail; không cho provider hardcode expected identifier.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A19 — Summary/extractor unavailable

**Owner milestone:** M5,M7. **Planned test:** `a19_optional_services_failure`.

- **Setup:** M5 summary failure fixture, journal có committed state; M7 durable extraction job chưa settle.
- **Trigger:** Fail summary và resume; ở M7 kill extractor rồi resume/catch-up.
- **Oracle bắt buộc:** M5 minimal context hoạt động hoặc mandatory overflow rõ; M7 job tồn tại/cursor đúng và resume không chờ extraction. Hoàn tất ở M7.
- **Negative control:** Make resume await extractor hoặc chỉ giữ job in-memory phải fail restart test.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A20 — Foreign history và fork grants

**Owner milestone:** M5. **Planned test:** `a20_source_scope_fork`.

- **Setup:** Hai project/task scopes có cùng keywords; session fork với allowed lineage refs.
- **Trigger:** Search/read foreign source, copy checkpoint refs sang scope khác; thử approval inherited trong fork.
- **Oracle bắt buộc:** Scope denied đúng; authorized lineage readable theo contract, foreign refs không mở quyền; no copied one-shot grant.
- **Negative control:** Filter sau ranking hoặc trust source ID embedded scope làm leak assertion fail.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A21 — Rollback không tự undo files

**Owner milestone:** M5. **Planned test:** `a21_rollback_effects`.

- **Setup:** File patch committed ở newer checkpoint và old checkpoint trước edit.
- **Trigger:** Rollback conversational head rồi inspect file/effects and resume.
- **Oracle bắt buộc:** File vẫn edited; UI/state nêu external effect mismatch/reconcile, không falsely report restored; undo riêng phải qua gate.
- **Negative control:** Restore old WorkingState như thể filesystem cũ mà không reobserve phải fail.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A22 — Skill activation và version retention

**Owner milestone:** M6. **Planned test:** `a22_skill_version`.

- **Setup:** Local trusted skill v1 có metadata/content/script request; chưa activate.
- **Trigger:** List catalog, activate, compact, update file v2 hoặc delete; resume/next step.
- **Oracle bắt buộc:** List không execute/activate; packet pin v1, version change chỉ admission boundary; missing content unavailable; permissions không tăng.
- **Negative control:** Use latest file content under old hash hoặc auto-run script scan bị test bắt.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A23 — MCP/protocol failure bounds

**Owner milestone:** M6. **Planned test:** `a23_extension_bounds`.

- **Setup:** Fixture external server có malformed, duplicate ID, output-flood, schema-change và ignore-cancel modes.
- **Trigger:** Discover/invoke qua app gate; toggle modes hoặc terminate server during uncertain call.
- **Oracle bắt buộc:** Typed bounded failure/cleanup; stale schema invalidated; no reconnect mutation retry; every executor call có intent/receipt state.
- **Negative control:** Call server directly bypass gate hoặc unlimited frame buffer làm gate/bound tests fail.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A24 — Deferred tool promotion không cấp quyền

**Owner milestone:** M6. **Planned test:** `a24_catalog_revocation`.

- **Setup:** Tool visible/promoted trong step; policy revision thay trước invoke.
- **Trigger:** Revoke permission hoặc đổi schema digest ở server.
- **Oracle bắt buộc:** Final gate rejects/revalidates; next request manifest có catalog revision mới; no effect from old schema.
- **Negative control:** Cache authorization at discovery only phải fail.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A25 — Memory publication và durable jobs

**Owner milestone:** M7. **Planned test:** `a25_memory_job_cas`.

- **Setup:** Hai extraction consumers cùng source range, version CAS và source dispositions.
- **Trigger:** Race claims/publish; kill sau candidate generation, trước settlement; replay job.
- **Oracle bắt buộc:** One logical settlement, no lost update; failed/filtered sources có disposition và cursor gap-free; stale lease rejected.
- **Negative control:** Publish memory rồi cursor advance separate commit tạo duplicate/gap bị fixture bắt.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A26 — Memory stale và self-reinforcement

**Owner milestone:** M7. **Planned test:** `a26_memory_provenance`.

- **Setup:** Source-backed fact v1, dependent summary, user correction v2; injected memory text lặp.
- **Trigger:** Invalidate source/revoke fact, retrieve và extract tiếp từ conversation chứa injected memory.
- **Oracle bắt buộc:** Latest accepted correction/scopes thắng; derived/index invalidated; injected copy không thành independent proof.
- **Negative control:** Filter revoked sau context injection hoặc treating model quote as user-confirmed source phải fail.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A27 — DAG/queue/budget và deadlock

**Owner milestone:** M8. **Planned test:** `a27_child_capacity_budget`.

- **Setup:** Parent + ba child tasks với limited permits/queue/budget; DAG fixtures.
- **Trigger:** Parent await children, children reserve concurrent; submit cycle/depth overflow và queue timeout.
- **Oracle bắt buộc:** Parent releases compute permit; no over-reservation; bounded queue/reject reasons; children tiến được.
- **Negative control:** Parent giữ sole compute slot hoặc non-atomic reserve làm deterministic timeout/overspend assertion fail.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A28 — Child commit trước parent notification

**Owner milestone:** M8. **Planned test:** `a28_child_delivery_recovery`.

- **Setup:** Child result+evidence+parent delivery transaction; parent notification barrier.
- **Trigger:** Kill parent sau transaction trước notification; reopen; gửi delivery duplicate.
- **Oracle bắt buộc:** Một logical child completion, result readback đúng; no respawn completed child; terminal state monotonic.
- **Negative control:** In-memory notification-only hoặc no receiver dedupe fail result/count assertions.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A29 — Integration fail dù branches pass

**Owner milestone:** M8. **Planned test:** `a29_integration_acceptance`.

- **Setup:** Hai child worktrees independent tests pass nhưng combined change cố ý fail integration test.
- **Trigger:** Collect results, integrate in workspace khác, run final checks.
- **Oracle bắt buộc:** Task unsatisfied/unverified; receipts branch revision không dùng làm final proof; integrated test failure lưu đúng digest.
- **Negative control:** Accept all children completed before integration check phải fail.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A30 — Dirty repo và concurrent destination edit

**Owner milestone:** M8. **Planned test:** `a30_dirty_workspace_preservation`.

- **Setup:** User repo có staged/unstaged/untracked sentinels; separate integration workspace.
- **Trigger:** Request unsupported dirty snapshot; hoặc edit destination sau check trước apply.
- **Oracle bắt buộc:** Sentinels/index/HEAD preserved; operation rejects/rebase explicit; không reset/stash tự động.
- **Negative control:** Blind apply/reset hoặc cleanup wrong root làm byte/index hash fail.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A31 — Backup/GC/migration consistency

**Owner milestone:** M9. **Planned test:** `a31_backup_retention_restore`.

- **Setup:** Active task, fork lineage, artifacts, uncheckpointed WAL writes; backup destination khác.
- **Trigger:** Backup/restore, GC expired unreferenced data, inject interrupted migration/corrupt artifact.
- **Oracle bắt buộc:** Restored journal+reachable artifacts valid; protected refs survive; corrupt backup reject without overwrite; recovery path documented.
- **Negative control:** Copy .db only/drop pinned ancestors or artifact before metadata checks làm restore/replay tests fail.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A32 — Telemetry và secrets

**Owner milestone:** M9. **Planned test:** `a32_diagnostics_isolation`.

- **Setup:** Exporter unavailable/slow, bounded log queue, fake secrets in host env/config.
- **Trigger:** Run task and export default support bundle.
- **Oracle bắt buộc:** Domain commits correct, bounded telemetry failure; bundle không secret/raw transcript by default; correlation refs usable.
- **Negative control:** Synchronous exporter in transaction hoặc raw-env bundle phải fail.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A33 — Web event ordering/reconnect

**Owner milestone:** M10. **Planned test:** `a33_web_replay_gap`.

- **Setup:** Real API fixture host/store + browser/client reducer; small event retention buffer.
- **Trigger:** Disconnect/trim/reconnect stale cursor, duplicate events và deliver old running after terminal.
- **Oracle bắt buộc:** Gap explicit; projection reload then tail dedupe; UI terminal stable; actions vẫn bound authority.
- **Negative control:** Silently replay partial buffer hoặc parse prose for status phải fail.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A34 — Schedule clock/restart semantics

**Owner milestone:** M11. **Planned test:** `a34_schedule_occurrences`.

- **Setup:** Fake clock timezone có DST, once/interval/cron, manual trigger và paused revisions.
- **Trigger:** Advance spring/fall, jump clock, kill before/after occurrence claim/launch; change pause while queued.
- **Oracle bắt buộc:** Occurrence keys/overlap/misfire policy đúng, no double launch; manual trigger không đổi future schedule; no auto approval.
- **Negative control:** Use wall timestamp as unique key hoặc recalc/manual overwrite next_due phải fail.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A35 — Remote submit/cancel ambiguity

**Owner milestone:** M11. **Planned test:** `a35_external_task_ambiguity`.

- **Setup:** Fixture remote service accepts mutation then drops response; persisted server identity/task handles where known.
- **Trigger:** Restart local poller, resolve ambiguous submit, race cancel with remote completion.
- **Oracle bắt buộc:** No blind resubmit; known handle polls then one terminal settlement; unknown handle blocked/reconciled explicit, cancel not rollback claim.
- **Negative control:** Retry submit after timeout hoặc discard remote handle on restart phải fail.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.

### A36 — Strict backend proof

**Owner milestone:** M12. **Planned test:** `a36_strict_confinement`.

- **Setup:** Actual selected backend/profile, outside file/network/env/socket canaries and owned resource leases.
- **Trigger:** Attempt denied accesses/resource exhaustion, kill during startup/execution/release and run reconciliation.
- **Oracle bắt buộc:** Denied actions enforced outside model; clean lifecycle, live-owned resources protected; support matrix matches real environment.
- **Negative control:** Replace backend with host fallback hoặc drop mount/egress rule phải make probe fail; not mocked OS enforcement.
- **Evidence:** exact test selector, discovered/executed count, source digest, fixture/platform version, observed state/effect assertions và log/artifact refs. Case chưa chạy ghi not_run; skipped không là passed.


## 3. Evidence và platform policy

Nhóm process/path/worktree/lock/confinement phải có Windows/Linux evidence theo support matrix. Không phát sinh một test pass trên Linux rồi ghi Windows passed. Feature chỉ supported ở một backend/OS ghi unsupported rõ và strict profile reject trên môi trường khác; milestone không được quảng cáo support rộng hơn tests.

Fault injection controls đặt ở boundaries của component thực, không làm API production nhận một flag như `pretend_success`. Test endpoints/exporters chỉ bind loopback; use fake secrets và fixture repositories. Không chạy destructive acceptance trực tiếp trên repo người dùng.

M0 unit/schema/boundary/gate tests và item-specific tests của từng mốc vẫn bắt buộc, dù không có A-ID riêng. Runtime registry phải liệt kê cả suites này. A01–A36 không phải chỉ tiêu coverage 100% mọi dòng code.
