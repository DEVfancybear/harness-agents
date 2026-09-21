# ADR-N02 — Store ownership, durability and artifact publish

**Status:** accepted in M1 (2026-09-21).
**Scope:** data directory ownership, generation fence, durability pragmas, data directory marker, artifact publish/GC. Does not replace ADR-N01.

## 1. Context

P1 already has a `SQLite` store with a writer lock, a host epoch and transactions for input/receipt/snapshot. M1 must write down what is a *decision* (so M3/M4/M9 do not silently relax it) and close two gaps that lacked evidence: the data directory marker and the "an artifact lives only by reachability" rule.

## 2. Decisions

1. **Lock scope.** Write authority belongs to one *data directory*, protected by `writer.lock` (an `fs2` exclusive lock on an OS file). In-process, the writer pool has exactly one connection so every write goes through one coordinator. An in-process `Mutex` is **not** evidence of exclusive host ownership.
2. **Generation fence.** Every writable open increments `host_epoch.generation` in a transaction and records `hosts(host_id, generation)`. Every write transaction checks the current fence (`begin_write` + `assert_fence_in_tx`); a stale fence fails with `StaleWriter`. The fence is *durable*: it lives in the database, not in RAM.
3. **Durability.** `foreign_keys=ON`, `busy_timeout=5s`, `journal_mode=WAL`, `synchronous=FULL` for a writer; a read-only open creates nothing and migrates nothing. An ACK is returned only **after** `COMMIT`.
4. **Data directory marker.** `harness-data.json` (`kind: harness-data`, `schema_version`, `store_schema_version`) is written on the first writable open, after migrations succeed, and validated before any migration on later opens. A newer marker, a foreign `kind`, or an unparseable marker refuses writes and is **never overwritten**. A read-only open does not require the marker, so an older P1 database stays readable.
5. **Newer schema.** Each table group has its own migration table; `MAX(version) > current version` refuses writes (`MigrationFailed`) instead of migrating blindly.
6. **Artifact publish.** The order is mandatory: write bytes → flush → hash → **then** commit the reference in a transaction. A file without a reference is reclaimed only by a reachability sweep (`referenced_artifact_ids` + retention pins); an artifact a receipt references returns `RetentionRefused` when removal is requested. "The log captured it" never implies "the bytes may be deleted".
7. **Outbox.** Durable delivery uses `parent_deliveries` (P5) as the logical outbox (`DeliveryState`, dedupe by `message_id`); M1 does **not** add a second `outbox` table. If M11 needs a transport outbox, it references the same delivery id instead of creating new authority.
8. **Reopen ≠ rerun.** Reopening the database, `ha status`, `ha resume` and offline replay are read-only; side-effect reconciliation belongs to M3/M4.

## 3. Consequences

- M3 adds runs/steps/attempts through its own migration without changing `STORE_SCHEMA_VERSION`.
- M4 adds approvals/intents/receipts (P3 already has them) and **then** implements `StorePort` for `SqliteStore`/`SessionService` with real proposals/grants/receipts.
- M9 backup/restore must copy `harness-data.json` too; restoring into a directory whose marker is newer must be refused.
- Any change to a durability pragma must update `diagnostics()` and the matching P1 test.
