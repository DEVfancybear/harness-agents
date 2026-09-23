# P0–P7 audit evidence — 2026-09-23

English | [Tiếng Việt](P0-P7_AUDIT.vi.md)

## 1. Result

Audit P0–P7 source in a clean worktree at base
`5498f74071aae04998e51afdfa4eeb4281f5e870`. This review is tied to a source
revision; it is not independent certification.

Linux remains **pending** by request. No paid provider call, product install or
artifact publication occurred. The main checkout and G01–G03 changes were
preserved.

## 2. Production fixes and test contracts

| Area | Change | Regression/evidence |
|---|---|---|
| P6 extension transport | RAII accounting releases inflight when a call future is dropped; the guard invalidates the handle, settles waiters as uncertain and calls `start_kill` synchronously before scheduling bounded reaping. An alive recheck under the pending lock blocks calls racing cancellation. | `m6_02_dropping_a_call_future_releases_and_stops_its_extension`: 1/1 passed; pre-fix RED was not separately measured. |
| P7 source retention | Invalidation reads scoped `memory_sources`, not the lineage table; maintenance traverses all projects and dependent assets; archive/invalidate/forget have distinct behavior. Forget removes payload/FTS/lineage and writes tombstone+journal in one fenced transaction; version writes check tombstones in that transaction. | `phase_p7`: retention, cascade, journal rollback, tombstone writes and revision cases. |
| P7 journal atomicity | Invalidate/archive status, FTS and invalidation record plus journal entry commit together. Journal failure rolls back all changes. | `p7_s04_retention_status_and_journal_commit_together`: observed RED — journal failure left status `Invalidated`; post-fix `Active` assertion passes. |
| P7 backup/restore | Backup requires a new path, builds database/artifacts/manifest in sibling staging and publishes after verification; failures clean staging. Reads canonicalize and require regular files inside the selected root, validate SQLite integrity/hash/length and compare manifest metadata to its DB snapshot. Restore also stages and refuses every existing path. | RED/GREEN: existing destination was populated; artifact failure left a partial DB; a rehashed manifest with altered metadata passed; empty restore destination was populated. All passed in final 30-test P7 suite. |
| P7 artifact GC/pins | GC validates `artifacts/<id>.bin`, canonical parent and regular file; quarantine rolls back on SQL/commit error or future drop. Pin batches validate every ID before inserting; GC checks pins/references in the same writer transaction. | RED/GREEN: rejected DELETE lost bytes; a directory artifact path was removed; nonexistent IDs were reported pinned. P7 regressions pass; the Unix symlink-root case is `cfg(unix)` and was not run on Windows. |
| P7 journal/tombstone | Retention and lower-level store APIs use plain INSERT; journal IDs cannot replace history and tombstones cannot upsert; journal targets store `kind:id`. Forget confirmation must match full `source_kind:source_id`. Retention identity is global within the store; file IDs are workspace-relative paths. | RED/GREEN: ID-only confirmation still forgot data; low-level tombstone/journal APIs overwrote prior records. `p7_s04_forget_confirmation_binds_to_the_full_target`, `p7_s04_store_tombstone_api_preserves_existing_audit_rows` and journal assertions. |
| P7 retention/GC edges | Blank reasons and repeated tombstones are refused; unknown mtime is age zero; grace period is checked again inside the collection transaction; failed pin batches write nothing; deleting assets/grants/bindings increments `memory_revision`. | `phase_p7`, final suite. |
| P1/P7 revision triggers | Deleting an asset/grant/binding increments `memory_revision` so cache/version observers see the change. | Included in the final P7 phase gate; store schema check. |
| CLI/provider loopback fixtures | Read the full Content-Length body before closing the socket; serve bounded retries; match the sanitized transport message; retain readiness and serialized acceptance. | `phase_p2`: 17/17; `interactive_launch`: 19/19 passed on Windows; this does not prove every host is free of loopback flakes. |
| P1/P7 test contracts | P1 tests use `STORE_SCHEMA_VERSION`; the P7 newer-schema fixture only raises the newest row to preserve primary-key uniqueness and checks against the current schema constant. Long scenario tests were split into helpers with assertions intact. | `milestone_m1`, `phase_p1`, `phase_p7`; workspace Clippy passes. |
| P4/M7 source invalidation contract | Updated the old semantic-merge fixture: legacy `source_file_hashes` contain only digests, not the path identity required by M7 ADR; the fixture now uses `MemorySource::file(relative_path, digest)` and invalidates by path. No production expectation was changed. | `phase_p4::extended::p4_source_changes_semantic_merge_and_binding_are_versioned`: 1/1 passed after the update. |
| P4/M7 current-version lineage | Source invalidation and archive/invalidate retention use source rows joined to `memory_assets.current_version`; descendant traversal filters by the current `derived_version`. Historical source/lineage rows cannot stale or archive an asset after correction. | RED: removing the current-version dependency filter made invalidation return a summary whose lineage had changed; GREEN: `p4_source_invalidation_only_uses_the_current_version_sources`. Full `phase_p4`: 28/28. |
| P7 historical forget | Forget intentionally traverses every historical source/dependency version and removes all asset versions that may retain forgotten content; archive/invalidate remain current-lineage operations. Deletion, FTS, lineage, tombstone and journal stay atomic. | RED: current-only root selection forgot **0** assets although history retained two; GREEN: `p4_source_forget_removes_assets_that_only_match_historical_lineage` removes both and preserves unrelated data. |
| M12 egress evidence | Canaries read the full nonce and ACK. The child reports success only after ACK; the listener keeps the socket open until the child closes so Windows cannot reset the ACK before delivery. Both ends use bounded timeouts and listener diagnostics. | `a36_strict_confinement`: 1/1; `milestone_m12`: 11/11 passed after the final handshake change. |
| P0–P5 review | Rechecked historical approval/compaction/process-cancellation findings and P4/P5 evidence; corrected status/handoff history without treating old tests as this audit's source gate. | `milestone_m4::a14_approval_binding`, `milestone_m5::a12_compaction_cas`, `milestone_m3::m3_02_canceled_queue_does_not_invoke`; final gate below. |
| Docs | Reconcile README/runbook statuses, P4 status and the P6 CI contradiction; preserve historical revisions, global retention scope and pending Linux status. | `Verify-Docs.ps1 -SelfTest`: **PASS** after final updates; 171 Markdown files, 15 language pairs, C01–C30, K01–K14, R01–R12, W01–W06, 63 P0–P8 steps, 53–76 P0–P7 person-days and all 12 negative controls. |

The new regressions did not weaken existing assertions. The P7 migration fixture
first failed because it tried to assign one primary-key version to all rows; the
fixture now updates only the current revision, without changing production behavior
or the contract.

The SPEC was written before the continued audit and amended append-only as findings
emerged. The user authorized the audit/fixes, but there was **no independent review
or pre-code SPEC approval**; this is not external certification.

## 3. Verification

| Command | Result |
|---|---|
| `cargo fmt --all -- --check` | **PASS** after removing the duplicate store comment. |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | **PASS** in the final official P7 gate on the audited source tree. |
| `cargo test -p harness-cli --test phase_p4 --locked` | **28/28 passed** after separating current-version invalidation from historical forget |
| `cargo test -p harness-cli --test phase_p7 --locked -- --test-threads=1` | **30/30 passed** after the shared store-query changes (current audit source) |
| `cargo test -p harness-cli --test milestone_m12 --locked` | **11/11 passed** after the final ACK keep-alive change |
| `cargo test -p harness-cli --test milestone_m6 m6_02_dropping_a_call_future_releases_and_stops_its_extension --locked -- --exact` | **1/1 passed** focused; the final official gate also passed all 12 P6 required selectors. |
| `cargo test -p harness-cli --test interactive_launch --locked` | Passed in the final workspace run. |
| `cargo test -p harness-cli --test milestone_m1 --locked` | **6/6 passed** focused; the final official gate also passed all 25 P1 required selectors. |
| `cargo test --workspace --all-targets --locked -- --skip m9_04_release_candidate_has_checksums_and_is_not_published` | **PASS** in the final official P7 gate. |
| `pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P7 -Json` | **PASS** in the final official P7 gate: 413 files, digest `sha256:e1da3e99101a7b407dbff67cc0bfe537d90b660de5080a12f9072b3974ffa37b`, format, Clippy, workspace tests, P0–P6 closure and P7 discovery/required tests all passed. The digest predates only the final evidence/handoff text update; no Rust source changed after the gate. |
| `pwsh -NoProfile -File scripts/Verify-Docs.ps1 -SelfTest` | **PASS** after final evidence/handoff edits: 171 Markdown files, 15 language pairs, C01–C30, K01–K14, R01–R12, W01–W06, 63 P0–P8 steps, 53–76 P0–P7 person-days and all 12 negative controls. |

Observed RED/GREEN cases: an existing empty restore destination; status before
journal commit; GC unlink before DB delete; tombstones not enforced on source
writes; manifest metadata inconsistent with the snapshot; missing mtime treated as
old; repeated forget rewriting a tombstone; blank reason accepted; ID-only
confirmation deleting data; backup writing into an existing directory; failed
backup leaving a snapshot; GC deleting a directory path; store APIs overwriting
tombstone/journal rows; and a missing artifact reported pinned. Fixed regressions
are green. The P7 migration fixture once failed because setup created a duplicate
primary key; that was a fixture failure, not a production defect.

## 4. Remaining limits

- Linux was not run, as requested.
- Historical P0–P7 gates and CI apply to the exact revisions in their evidence; this
  audit does not assume they automatically apply to a newer source tree.
- P7 `retrieval_p95` and `restore_state` benchmarks remain unmeasured. The release
  matrix must keep reporting `measured: null` until an actual dataset and run are
  recorded.
- Extension/process transport is not an OS sandbox; changing `strict` requires an
  ADR and a suitable backend. Token estimates remain byte-based; phase evidence
  also records incomplete coverage/advisory/secret scans.
- Windows loopback connections have intermittently been refused on this host. The
  acceptance fixture now waits for readiness and uses bounded retries; a passing
  run does not prove the host flake is absent in every environment.

## 5. Delivery

The audit was delivered on branch `codex/p0-p7-audit`; the Git history on that
branch identifies the exact revision. Only this audit branch was delivered; no
G01–G03 changes from the main checkout were staged, committed or pushed.
