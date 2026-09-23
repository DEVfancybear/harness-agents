# P0–P7 audit specification — regressions, safety and technical debt

Date: 2026-09-23. Audit the implemented P0–P7 source at base
`5498f74071aae04998e51afdfa4eeb4281f5e870`, cross-checking implementation
runbooks, specs, handoffs, evidence and ADRs. The user requested a careful source
review and fixes for remaining defects/debt. Linux stays pending by instruction.

## Scope and preservation

- Review committed P0–P7 code and the open items in
  `docs/evidence/P0-P3_REVIEW.en.md`, plus findings backed by P4–P7 artifacts or
  source.
- The main checkout has uncommitted G01–G03 work. Work only in a clean audit
  worktree; do not touch, stage, commit or push those changes.
- Do not call paid providers, install the product or change PATH, delete/move user
  data, publish an artifact, add a confinement backend, or change the meaning of
  `strict` without a new architecture decision.
- Preserve accepted schema/ID/API contracts. Any required contract change needs a
  regression, additive migration and prior SPEC/ADR explanation.

## Acceptance

1. Every reported fix has a regression measured RED before the fix and GREEN after;
   existing assertions are not deleted, skipped or weakened.
2. A one-time approval binds the exact invocation/session/task/call/action,
   arguments, workspace and policy revision; a different proposal or replay fails.
3. Compaction commits only if source sequence/revision is still current; a changed
   tail is detected and safely rebased with a bound or returned as a typed error.
4. A process canceled before spawn cannot start after waiting for the lifecycle
   mutex; cancellation at spawn has a clear linearization point and tree cleanup is
   bounded.
5. Review P0–P7 security/data contracts: fail-closed authorization, single-writer
   fencing, durable commit/recovery, context/source freshness, worktree/user-change
   preservation, extension trust/limits, backup/restore/forget/tombstone/GC.
   Record a defect as fixed only with a reproduction and matching evidence.
6. Update evidence and handoff to separate fixed findings, executed checks, open
   limitations and pending Linux verification. Do not promote the registry to
   `accepted` or `verified_local`.
7. Run the P7 Windows phase gate and docs self-test after fixes; report every gate
   that was not run or failed.

## RED/GREEN and delivery

Run the baseline in a clean worktree before edits. For each finding, add or update a
test first, record the failing assertion, then fix it and rerun the exact test. Do
not assume a failure belongs to source when it may be environmental. Update
bilingual evidence and handoff against the verified revision. Commit and push only
this audit branch; never include G01–G03.

## Addenda

1. **Restore destination:** an existing path, including an empty directory, must
   return `RestoreTargetConflict` untouched. Restore uses a staging directory and
   publishes only after validation; failed construction leaves no final output.
   No dependency added. SPEC approval was not obtained; this is an autonomous run
   under the audit/fix request.
2. **Retention journal:** invalidate/archive status, FTS/invalidation metadata and
   the journal entry must commit together. A journal write failure rolls all of
   them back. No dependency or public data-contract change.
3. **Artifact GC rollback:** if a DB error occurs after collection begins, both
   filesystem bytes and the index row remain. A successful collection removes
   both. No dependency added.
4. **Enforce tombstones:** every version write sourced from a file/commit checks
   the tombstone inside the write transaction, so ordinary creation/proposal/
   extraction cannot recreate forgotten content, including under concurrent
   writers. No dependency or public API change.
5. **Manifest and snapshot consistency:** P7 requires restore to validate schema,
   artifact and tombstone sets, but `verify_backup` only compared file hashes and
   artifact bytes; the manifest is unsigned, so a writer can change metadata and
   recompute its digest. Verify SQLite integrity and compare schema revisions,
   every artifact row/length, retention pins, tombstone identities and file byte
   lengths with the database snapshot before restore. A mismatch fails closed.
   No dependency or manifest-schema change.
6. **Unknown GC age:** GC currently substitutes Unix time zero when file metadata
   or modification time cannot be read, making an unknown-age artifact appear
   decades old. Treat missing/failed mtime as age zero so it remains inside the
   grace period; unknown age is not evidence that the artifact is old. No
   dependency added.
7. **Immutable tombstones:** repeating forget for a tombstoned source currently
   updates reason/copies without changing `tombstone_id`, while returning a new
   ID. A repeated forget must return `RetentionRefused` without changing the
   original tombstone; use a plain insert after a transactional existence check.
   No dependency/API change.
8. **Non-empty retention reason:** archive/forget currently accept blank reasons
   although their journal/tombstone must explain the operation. All actions must
   reject blank reasons before writing status, journal or tombstone. No
   dependency/API change.
9. **Bind confirmation to the complete target:** forget reports a target as
   `source_kind:source_id`, but currently compares confirmation with only
   `source_id`, so the token can be reused across source kinds with the same ID.
   Require exact `source_kind:source_id`; reject an ID-only token and any other
   kind/ID before mutation. Store the complete identity in journal targets too.
   No schema change; update CLI help and operator guide.
10. **Publish backups atomically into a new path:** `create_backup` refuses an
    existing directory only when it already contains a recognized database or
    manifest, so it may write into a user's existing directory. A failure after
    `VACUUM INTO` leaves a partial database that blocks retry. Refuse every
    existing destination, build the snapshot/artifacts/manifest in a sibling
    staging directory, and rename it into place only after validation. Failure
    before publish leaves neither destination nor staging output.
11. **Keep backup reads inside the selected root:** lexical relative-path checks
    still allow an intermediate symlink/reparse point to escape the backup or
    source-store directory. Canonicalize manifest, database and artifact files;
    require each to be a regular file contained by its selected root. Refuse
    backup/verification/restore if a path escapes the root.
12. **Constrain GC to store-owned artifact paths:** GC joins a database-supplied
    `relative_path` directly to the data root, and a final-component symlink can
    make collection report reclaimed bytes without removing the target data.
    Require the stable `artifacts/<id>.bin` identity, a canonical parent inside
    the data root, and a regular final file before quarantine. Invalid paths,
    directories and symlinks fail typed while preserving the row/path.
13. **Keep journal and store-level tombstones immutable:** lower-level store APIs
    still replace journal rows by `entry_id` and upsert tombstone reason/copies on
    `(source_kind, source_id)`. Reusing either key silently rewrites audit history.
    Journal IDs and tombstone identities are insert-only; collisions return a
    typed error and roll back without changing the original records.
14. **Do not pin nonexistent artifacts:** `pin_artifacts` currently records a pin
    without verifying its artifact row. After GC commits, a caller can be told an
    absent artifact is protected. Validate every requested ID inside the writer
    transaction before inserting any pin; if one is missing, return
    `RetentionRefused` and leave the whole batch unchanged.
15. **Recheck grace age at collection:** GC reads file age when building its
    candidate snapshot, then may collect later. If newer bytes replace the path,
    the old age does not prove the current file passed the grace period. Recheck
    mtime/age and regular-file metadata in the writer transaction immediately
    before quarantine; unknown or young age rolls back and reports
    `retained_young`.

## Addendum 16 — freshness follows current lineage; forget removes history

The audit found invalidation and retention walking every `memory_sources` and
`memory_dependencies` row. After a correction, a source or dependency from a
superseded version could incorrectly invalidate or archive the current asset.
Conversely, applying a current-only filter to forget would leave old versions
that still contain content derived from the forgotten source.

Acceptance: source invalidation and archive/invalidate retention select roots and
descendants only when `derived_version = memory_assets.current_version`. Forget
must find every root and descendant with any historical version that depended on
the source, then delete all their versions/FTS/lineage atomically with the
tombstone and journal. Add regression coverage where current lineage has changed:
invalidation skips old lineage while forget still removes sensitive history. No
API or schema change.

## Addendum 17 — the egress canary must observe its ACK in both directions

The M12 canary ACK was sent after the listener received the correct nonce, but
the listener closed immediately after `write_all`. On Windows, the ACK could
still be buffered when the listener closed, so the child reported
`connected=false` even though the request had arrived.

Acceptance: the child reports success only after reading the ACK; the listener
keeps the socket open until the child closes it, and both sides use bounded
timeouts. Timeout/error remains fail-closed, and diagnostics state whether the
listener received the nonce.
