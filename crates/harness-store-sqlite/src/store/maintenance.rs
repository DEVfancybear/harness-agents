//! Additive P7 maintenance schema: tombstones and the maintenance journal.
//!
//! Every write goes through the P1 transaction coordinator and host fence, so
//! retention never becomes a second database authority. Earlier schema
//! revisions are untouched.

use harness_types::{ContentHash, ErrorCode, TaskId};
use sqlx::SqlitePool;

use super::{SqliteStore, assert_fence_in_tx, database_error, row_get, to_i64, to_u64};
use crate::{StoreError, TombstoneRow};

/// Additive P7 maintenance tables keep their own revision.
pub const MAINTENANCE_SCHEMA_VERSION: i64 = 1;

const MAINTENANCE_SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS maintenance_schema_migrations (
        version INTEGER PRIMARY KEY,
        applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    )",
    "CREATE TABLE IF NOT EXISTS maintenance_tombstones (
        tombstone_id TEXT PRIMARY KEY,
        source_kind TEXT NOT NULL,
        source_id TEXT NOT NULL,
        reason TEXT NOT NULL,
        surviving_copies_json TEXT NOT NULL,
        created_unix_ms INTEGER NOT NULL,
        UNIQUE (source_kind, source_id)
    )",
    "CREATE INDEX IF NOT EXISTS maintenance_tombstones_by_source
        ON maintenance_tombstones(source_kind, source_id)",
    "CREATE TABLE IF NOT EXISTS maintenance_journal (
        entry_id TEXT PRIMARY KEY,
        action TEXT NOT NULL,
        target TEXT NOT NULL,
        detail_json TEXT NOT NULL,
        created_unix_ms INTEGER NOT NULL
    )",
    // A backup pin protects an artifact from collection for one reason.
    "CREATE TABLE IF NOT EXISTS maintenance_pins (
        artifact_id TEXT NOT NULL,
        reason TEXT NOT NULL,
        task_id TEXT,
        created_unix_ms INTEGER NOT NULL,
        PRIMARY KEY (artifact_id, reason)
    )",
];

pub(super) async fn ensure_maintenance_schema(pool: &SqlitePool) -> Result<(), StoreError> {
    let mut tx = pool.begin().await.map_err(|error| {
        database_error(
            ErrorCode::MigrationFailed,
            "begin maintenance migration",
            error,
        )
    })?;
    for statement in MAINTENANCE_SCHEMA {
        sqlx::query(*statement)
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                database_error(
                    ErrorCode::MigrationFailed,
                    "apply maintenance schema",
                    error,
                )
            })?;
    }
    let current = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT MAX(version) FROM maintenance_schema_migrations",
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| {
        database_error(
            ErrorCode::MigrationFailed,
            "read maintenance migration version",
            error,
        )
    })?
    .unwrap_or(0);
    if current > MAINTENANCE_SCHEMA_VERSION {
        return Err(StoreError::new(
            ErrorCode::MigrationFailed,
            "maintenance schema is newer than this host supports",
        ));
    }
    if current < MAINTENANCE_SCHEMA_VERSION {
        sqlx::query("INSERT INTO maintenance_schema_migrations(version) VALUES (?)")
            .bind(MAINTENANCE_SCHEMA_VERSION)
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                database_error(
                    ErrorCode::MigrationFailed,
                    "record maintenance migration",
                    error,
                )
            })?;
    }
    tx.commit().await.map_err(|error| {
        database_error(
            ErrorCode::MigrationFailed,
            "commit maintenance migration",
            error,
        )
    })
}

impl SqliteStore {
    /// Every published artifact with its recorded hash, for a backup manifest.
    pub async fn artifact_pins(
        &self,
    ) -> Result<Vec<(String, String, ContentHash, u64)>, StoreError> {
        let rows = sqlx::query(
            "SELECT artifact_id, relative_path, content_hash, byte_len
             FROM artifacts ORDER BY artifact_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "list artifacts", error))?;
        let mut pins = Vec::with_capacity(rows.len());
        for row in rows {
            pins.push((
                row_get::<String>(&row, "artifact_id")?,
                row_get::<String>(&row, "relative_path")?,
                ContentHash::parse(row_get::<String>(&row, "content_hash")?)?,
                to_u64(row_get::<i64>(&row, "byte_len")?, "artifact byte length")?,
            ));
        }
        Ok(pins)
    }

    /// Every schema revision present, so a restore can prove compatibility.
    pub async fn all_schema_revisions(
        &self,
    ) -> Result<std::collections::BTreeMap<String, i64>, StoreError> {
        let mut revisions = std::collections::BTreeMap::new();
        // Static statements only: a migration table name is never interpolated.
        for (name, sql) in [
            (
                "store",
                "SELECT MAX(version) AS version FROM schema_migrations",
            ),
            (
                "runtime",
                "SELECT MAX(version) AS version FROM runtime_schema_migrations",
            ),
            (
                "tools",
                "SELECT MAX(version) AS version FROM tools_schema_migrations",
            ),
            (
                "memory",
                "SELECT MAX(version) AS version FROM memory_schema_migrations",
            ),
            (
                "delegation",
                "SELECT MAX(version) AS version FROM delegation_schema_migrations",
            ),
            (
                "maintenance",
                "SELECT MAX(version) AS version FROM maintenance_schema_migrations",
            ),
        ] {
            let row = sqlx::query(sql)
                .fetch_one(&self.pool)
                .await
                .map_err(|error| {
                    database_error(ErrorCode::StorageWriteFailed, "read schema revision", error)
                })?;
            let version = row_get::<Option<i64>>(&row, "version")?.unwrap_or(0);
            revisions.insert(name.to_owned(), version);
        }
        Ok(revisions)
    }

    /// Tombstone identities, for a backup manifest and for re-extraction checks.
    pub async fn tombstone_ids(&self) -> Result<Vec<String>, StoreError> {
        let rows = sqlx::query(
            "SELECT source_kind, source_id FROM maintenance_tombstones
             ORDER BY source_kind, source_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "list tombstones", error))?;
        let mut ids = Vec::with_capacity(rows.len());
        for row in rows {
            ids.push(format!(
                "{}:{}",
                row_get::<String>(&row, "source_kind")?,
                row_get::<String>(&row, "source_id")?
            ));
        }
        Ok(ids)
    }

    /// Full tombstone records.
    pub async fn tombstones(&self) -> Result<Vec<TombstoneRow>, StoreError> {
        let rows = sqlx::query(
            "SELECT tombstone_id, source_kind, source_id, reason, surviving_copies_json,
                    created_unix_ms
             FROM maintenance_tombstones ORDER BY created_unix_ms, tombstone_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "list tombstones", error))?;
        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            let copies: Vec<String> =
                serde_json::from_str(&row_get::<String>(&row, "surviving_copies_json")?).map_err(
                    |_| {
                        StoreError::new(
                            ErrorCode::StorageWriteFailed,
                            "stored surviving copies are invalid",
                        )
                    },
                )?;
            records.push(TombstoneRow {
                tombstone_id: row_get::<String>(&row, "tombstone_id")?,
                source_kind: row_get::<String>(&row, "source_kind")?,
                source_id: row_get::<String>(&row, "source_id")?,
                reason: row_get::<String>(&row, "reason")?,
                surviving_copies: copies,
                created_unix_ms: to_u64(
                    row_get::<i64>(&row, "created_unix_ms")?,
                    "tombstone time",
                )?,
            });
        }
        Ok(records)
    }

    /// Whether a source is tombstoned, which blocks re-extraction.
    pub async fn is_tombstoned(
        &self,
        source_kind: &str,
        source_id: &str,
    ) -> Result<bool, StoreError> {
        let found = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM maintenance_tombstones
             WHERE source_kind = ? AND source_id = ?",
        )
        .bind(source_kind)
        .bind(source_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "check tombstone", error))?;
        Ok(found > 0)
    }

    /// Record a tombstone and a journal entry in one transaction.
    pub async fn record_tombstone(
        &self,
        tombstone: &TombstoneRow,
        journal_entry_id: &str,
        journal_action: &str,
        journal_detail: &serde_json::Value,
    ) -> Result<(), StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        let copies = serde_json::to_string(&tombstone.surviving_copies).map_err(|_| {
            StoreError::new(
                ErrorCode::InvalidPayload,
                "surviving copies are not serializable",
            )
        })?;
        sqlx::query(
            "INSERT INTO maintenance_tombstones(
                 tombstone_id, source_kind, source_id, reason, surviving_copies_json, created_unix_ms)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(source_kind, source_id) DO UPDATE SET
                 reason = excluded.reason,
                 surviving_copies_json = excluded.surviving_copies_json,
                 created_unix_ms = excluded.created_unix_ms",
        )
        .bind(&tombstone.tombstone_id)
        .bind(&tombstone.source_kind)
        .bind(&tombstone.source_id)
        .bind(&tombstone.reason)
        .bind(copies)
        .bind(to_i64(tombstone.created_unix_ms, "tombstone time")?)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "record tombstone", error)
        })?;
        let detail = serde_json::to_string(journal_detail).map_err(|_| {
            StoreError::new(
                ErrorCode::InvalidPayload,
                "journal detail is not serializable",
            )
        })?;
        sqlx::query(
            "INSERT OR REPLACE INTO maintenance_journal(
                 entry_id, action, target, detail_json, created_unix_ms)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(journal_entry_id)
        .bind(journal_action)
        .bind(&tombstone.source_id)
        .bind(detail)
        .bind(to_i64(tombstone.created_unix_ms, "journal time")?)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "record journal entry", error)
        })?;
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit tombstone", error)
        })
    }

    /// Append one maintenance journal entry.
    pub async fn record_maintenance_entry(
        &self,
        entry_id: &str,
        action: &str,
        target: &str,
        detail: &serde_json::Value,
        created_unix_ms: u64,
    ) -> Result<(), StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        let serialized = serde_json::to_string(detail).map_err(|_| {
            StoreError::new(
                ErrorCode::InvalidPayload,
                "journal detail is not serializable",
            )
        })?;
        sqlx::query(
            "INSERT OR REPLACE INTO maintenance_journal(
                 entry_id, action, target, detail_json, created_unix_ms)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(entry_id)
        .bind(action)
        .bind(target)
        .bind(serialized)
        .bind(to_i64(created_unix_ms, "journal time")?)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "record journal entry", error)
        })?;
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit journal entry", error)
        })
    }

    /// Pin artifacts referenced by unfinished work, so garbage collection
    /// cannot drop them.
    pub async fn pin_artifacts(
        &self,
        artifact_ids: &[String],
        reason: &str,
        task_id: Option<&TaskId>,
        created_unix_ms: u64,
    ) -> Result<usize, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        let mut pinned = 0usize;
        for artifact_id in artifact_ids {
            sqlx::query(
                "INSERT OR REPLACE INTO maintenance_pins(
                     artifact_id, reason, task_id, created_unix_ms)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(artifact_id)
            .bind(reason)
            .bind(task_id.map(TaskId::as_str))
            .bind(to_i64(created_unix_ms, "pin time")?)
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageWriteFailed, "pin artifact", error)
            })?;
            pinned += 1;
        }
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit artifact pins", error)
        })?;
        Ok(pinned)
    }

    /// Release one pin reason.
    pub async fn unpin_artifacts(&self, reason: &str) -> Result<u64, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        let result = sqlx::query("DELETE FROM maintenance_pins WHERE reason = ?")
            .bind(reason)
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                database_error(
                    ErrorCode::StorageWriteFailed,
                    "release artifact pins",
                    error,
                )
            })?;
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit pin release", error)
        })?;
        Ok(result.rows_affected())
    }

    /// Outstanding retention pins, for a backup manifest.
    pub async fn retention_pins(&self) -> Result<Vec<(String, Option<TaskId>)>, StoreError> {
        let rows =
            sqlx::query("SELECT DISTINCT reason, task_id FROM maintenance_pins ORDER BY reason")
                .fetch_all(&self.pool)
                .await
                .map_err(|error| {
                    database_error(ErrorCode::StorageWriteFailed, "list pins", error)
                })?;
        let mut pins = Vec::with_capacity(rows.len());
        for row in rows {
            let task = row_get::<Option<String>>(&row, "task_id")?;
            pins.push((
                row_get::<String>(&row, "reason")?,
                task.map(TaskId::parse).transpose()?,
            ));
        }
        Ok(pins)
    }

    /// Artifact identities currently pinned by any reason.
    pub async fn pinned_artifact_ids(&self) -> Result<Vec<String>, StoreError> {
        let rows = sqlx::query("SELECT DISTINCT artifact_id FROM maintenance_pins")
            .fetch_all(&self.pool)
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "list pins", error))?;
        rows.iter()
            .map(|row| row_get::<String>(row, "artifact_id"))
            .collect()
    }

    /// Publish artifact bytes and record the artifact row in one durable step.
    ///
    /// A caller with no surrounding transaction uses this instead of
    /// [`SqliteStore::publish_artifact`], so the bytes and the record that makes
    /// them reachable commit together.
    pub async fn publish_artifact_recorded(
        &self,
        bytes: &[u8],
    ) -> Result<crate::PublishedArtifact, StoreError> {
        let artifact = self.publish_artifact(bytes)?;
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        sqlx::query(
            "INSERT INTO artifacts(artifact_id, content_hash, byte_len, relative_path)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(artifact_id) DO NOTHING",
        )
        .bind(artifact.artifact_id.as_str())
        .bind(artifact.content_hash.as_str())
        .bind(to_i64(artifact.byte_len, "artifact byte length")?)
        .bind(&artifact.relative_path)
        .execute(&mut *tx)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "record artifact", error))?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit artifact record",
                error,
            )
        })?;
        Ok(artifact)
    }

    /// Artifact identities still referenced by a durable record: a receipt, a
    /// tool artifact scope, or a memory version payload hash.
    pub async fn referenced_artifact_ids(&self) -> Result<Vec<String>, StoreError> {
        let rows = sqlx::query(
            "SELECT artifact_id FROM artifacts
             WHERE artifact_id IN (SELECT artifact_id FROM receipts WHERE artifact_id IS NOT NULL)
                OR artifact_id IN (SELECT artifact_id FROM tool_artifact_scopes)
             ORDER BY artifact_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "list referenced artifacts",
                error,
            )
        })?;
        rows.iter()
            .map(|row| row_get::<String>(row, "artifact_id"))
            .collect()
    }

    /// Remove an artifact record after its bytes were reclaimed. This refuses
    /// while anything still references it, so a caller cannot orphan a receipt.
    pub async fn remove_artifact_record(&self, artifact_id: &str) -> Result<(), StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        for (query, reason) in [
            (
                "SELECT COUNT(*) FROM receipts WHERE artifact_id = ?",
                "a referenced artifact cannot be removed",
            ),
            (
                "SELECT COUNT(*) FROM maintenance_pins WHERE artifact_id = ?",
                "a pinned artifact cannot be removed",
            ),
            (
                "SELECT COUNT(*) FROM tool_artifact_scopes WHERE artifact_id = ?",
                "an artifact scoped to a tool execution cannot be removed",
            ),
        ] {
            let held = sqlx::query_scalar::<_, i64>(query)
                .bind(artifact_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|error| {
                    database_error(ErrorCode::StorageWriteFailed, "check artifact holds", error)
                })?;
            if held > 0 {
                return Err(StoreError::new(ErrorCode::RetentionRefused, reason));
            }
        }
        sqlx::query("DELETE FROM artifacts WHERE artifact_id = ?")
            .bind(artifact_id)
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                database_error(
                    ErrorCode::StorageWriteFailed,
                    "remove artifact record",
                    error,
                )
            })?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit artifact removal",
                error,
            )
        })
    }

    /// Reclaim one unreferenced artifact's bytes and record together.
    ///
    /// The pin, receipt and scope checks and the row deletion run inside one
    /// writer transaction, and the file is unlinked inside it. A backup that
    /// pins the artifact concurrently either commits before the checks (and this
    /// refuses) or after this transaction (and the pin then names a record that
    /// no longer exists, which retention reporting tolerates). A check made
    /// before the unlink in a separate transaction would leave a window where a
    /// fresh pin still loses its bytes.
    ///
    /// Returns `Ok(false)` when the artifact is still pinned or referenced; the
    /// caller decides how to report it.
    pub async fn collect_artifact(&self, artifact_id: &str) -> Result<bool, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        let relative_path = sqlx::query_scalar::<_, String>(
            "SELECT relative_path FROM artifacts WHERE artifact_id = ?",
        )
        .bind(artifact_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "read artifact path", error)
        })?;
        let Some(relative_path) = relative_path else {
            // Already collected by an earlier sweep; nothing to reclaim.
            return tx
                .rollback()
                .await
                .map_err(|error| {
                    database_error(
                        ErrorCode::StorageWriteFailed,
                        "rollback artifact sweep",
                        error,
                    )
                })
                .map(|()| true);
        };
        for query in [
            "SELECT COUNT(*) FROM maintenance_pins WHERE artifact_id = ?",
            "SELECT COUNT(*) FROM receipts WHERE artifact_id = ?",
            "SELECT COUNT(*) FROM tool_artifact_scopes WHERE artifact_id = ?",
        ] {
            let held = sqlx::query_scalar::<_, i64>(query)
                .bind(artifact_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|error| {
                    database_error(ErrorCode::StorageWriteFailed, "check artifact holds", error)
                })?;
            if held > 0 {
                tx.rollback().await.map_err(|error| {
                    database_error(
                        ErrorCode::StorageWriteFailed,
                        "rollback refused artifact sweep",
                        error,
                    )
                })?;
                return Ok(false);
            }
        }
        let path = self.paths.data_dir.join(&relative_path);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tx.rollback().await.map_err(|rollback| {
                    database_error(
                        ErrorCode::StorageWriteFailed,
                        "rollback failed artifact sweep",
                        rollback,
                    )
                })?;
                return Err(StoreError::new(
                    ErrorCode::ArtifactWriteFailed,
                    format!(
                        "cannot remove artifact bytes at {}: {error}",
                        path.display()
                    ),
                ));
            }
        }
        sqlx::query("DELETE FROM artifacts WHERE artifact_id = ?")
            .bind(artifact_id)
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                database_error(
                    ErrorCode::StorageWriteFailed,
                    "remove artifact record",
                    error,
                )
            })?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit artifact sweep",
                error,
            )
        })?;
        Ok(true)
    }
}
