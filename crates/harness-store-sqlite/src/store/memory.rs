use harness_types::{
    AgentProfileId, ContentHash, ErrorCode, MemoryAsset, MemoryAssetId, MemoryAssetStatus,
    MemoryVersion, SessionId, TaskId, Validity,
};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};

use super::{SqliteStore, database_error, to_i64};
use crate::{
    MEMORY_SCHEMA_VERSION, MemoryCreateCommit, MemoryVersionCommit, StoreError, StoreFaultPoint,
    StoreMemoryPrincipal, StoredExtractionJobRecord, StoredExtractionLeaseRecord,
    StoredMemoryAssetRecord, StoredMemoryGrantRecord, StoredMemoryVersionRecord,
};

mod advanced;

const MEMORY_SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS memory_revision (id INTEGER PRIMARY KEY CHECK(id = 1), revision INTEGER NOT NULL)",
    "INSERT OR IGNORE INTO memory_revision VALUES (1, 1)",
    "CREATE TABLE IF NOT EXISTS memory_invalidations (id INTEGER PRIMARY KEY, memory_asset_id TEXT NOT NULL, reason TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP)",
    "CREATE TABLE IF NOT EXISTS memory_schema_migrations (
        version INTEGER PRIMARY KEY,
        applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    )",
    "CREATE TABLE IF NOT EXISTS memory_assets (
        memory_asset_id TEXT PRIMARY KEY,
        owner_id TEXT NOT NULL,
        project_id TEXT,
        task_id TEXT,
        agent_profile_id TEXT,
        session_id TEXT,
        scope TEXT NOT NULL,
        layer TEXT NOT NULL,
        status TEXT NOT NULL,
        current_version INTEGER NOT NULL CHECK (current_version >= 1),
        asset_json TEXT NOT NULL,
        revision INTEGER NOT NULL CHECK (revision >= 1),
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    )",
    "CREATE TABLE IF NOT EXISTS memory_versions (
        memory_asset_id TEXT NOT NULL REFERENCES memory_assets(memory_asset_id),
        version INTEGER NOT NULL CHECK (version >= 1),
        content TEXT NOT NULL,
        normalized_content TEXT NOT NULL,
        content_hash TEXT NOT NULL,
        version_json TEXT NOT NULL,
        strategy_digest TEXT,
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        PRIMARY KEY (memory_asset_id, version)
    )",
    "CREATE TABLE IF NOT EXISTS memory_grants (
        principal_id TEXT NOT NULL,
        memory_asset_id TEXT NOT NULL REFERENCES memory_assets(memory_asset_id),
        project_id TEXT,
        actions_json TEXT NOT NULL,
        revision INTEGER NOT NULL CHECK (revision >= 1),
        active INTEGER NOT NULL CHECK (active IN (0, 1)),
        PRIMARY KEY (principal_id, memory_asset_id)
    )",
    "CREATE TABLE IF NOT EXISTS memory_bindings (
        binding_id TEXT PRIMARY KEY,
        memory_asset_id TEXT NOT NULL REFERENCES memory_assets(memory_asset_id),
        principal_id TEXT NOT NULL,
        injection_mode TEXT NOT NULL,
        priority INTEGER NOT NULL,
        revision INTEGER NOT NULL CHECK (revision >= 1)
    )",
    "CREATE TABLE IF NOT EXISTS memory_dependencies (
        derived_asset_id TEXT NOT NULL,
        derived_version INTEGER NOT NULL,
        source_kind TEXT NOT NULL,
        source_id TEXT NOT NULL,
        source_version INTEGER,
        PRIMARY KEY (derived_asset_id, derived_version, source_kind, source_id)
    )",
    "CREATE TABLE IF NOT EXISTS memory_jobs (
        job_id TEXT PRIMARY KEY,
        source_stream TEXT NOT NULL,
        start_sequence INTEGER NOT NULL CHECK (start_sequence >= 1),
        end_sequence INTEGER NOT NULL CHECK (end_sequence >= start_sequence),
        source_digest TEXT NOT NULL,
        source_ids_json TEXT NOT NULL,
        extractor_version TEXT NOT NULL,
        strategy_digest TEXT NOT NULL,
        status TEXT NOT NULL,
        attempts INTEGER NOT NULL DEFAULT 0,
        lease_owner TEXT,
        lease_generation INTEGER NOT NULL DEFAULT 0,
        next_due_unix_ms INTEGER NOT NULL DEFAULT 0,
        last_error TEXT,
        disposition TEXT,
        UNIQUE (source_stream, start_sequence, end_sequence, extractor_version, strategy_digest)
    )",
    "CREATE TABLE IF NOT EXISTS extraction_cursors (
        source_stream TEXT NOT NULL,
        extractor_version TEXT NOT NULL,
        strategy_digest TEXT NOT NULL,
        contiguous_sequence INTEGER NOT NULL CHECK (contiguous_sequence >= 0),
        revision INTEGER NOT NULL CHECK (revision >= 1),
        PRIMARY KEY (source_stream, extractor_version, strategy_digest)
    )",
    "CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
        memory_asset_id UNINDEXED,
        version UNINDEXED,
        content,
        normalized_content,
        tokenize = 'unicode61 remove_diacritics 2'
    )",
    "CREATE TRIGGER IF NOT EXISTS memory_asset_insert_revision AFTER INSERT ON memory_assets BEGIN UPDATE memory_revision SET revision = revision + 1; END",
    "CREATE TRIGGER IF NOT EXISTS memory_asset_update_revision AFTER UPDATE ON memory_assets BEGIN UPDATE memory_revision SET revision = revision + 1; END",
    "CREATE TRIGGER IF NOT EXISTS memory_grant_insert_revision AFTER INSERT ON memory_grants BEGIN UPDATE memory_revision SET revision = revision + 1; END",
    "CREATE TRIGGER IF NOT EXISTS memory_grant_update_revision AFTER UPDATE ON memory_grants BEGIN UPDATE memory_revision SET revision = revision + 1; END",
    "CREATE TRIGGER IF NOT EXISTS memory_binding_insert_revision AFTER INSERT ON memory_bindings BEGIN UPDATE memory_revision SET revision = revision + 1; END",
    "CREATE TRIGGER IF NOT EXISTS memory_binding_update_revision AFTER UPDATE ON memory_bindings BEGIN UPDATE memory_revision SET revision = revision + 1; END",
];

pub(super) async fn ensure_memory_schema(pool: &SqlitePool) -> Result<(), StoreError> {
    let mut tx = pool.begin().await.map_err(|error| {
        database_error(ErrorCode::MigrationFailed, "begin memory migration", error)
    })?;
    for statement in MEMORY_SCHEMA {
        sqlx::query(*statement)
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                database_error(ErrorCode::MigrationFailed, "apply memory schema", error)
            })?;
    }
    let current =
        sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(version) FROM memory_schema_migrations")
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| {
                database_error(
                    ErrorCode::MigrationFailed,
                    "read memory schema version",
                    error,
                )
            })?
            .unwrap_or(0);
    if current > MEMORY_SCHEMA_VERSION {
        return Err(StoreError::new(
            ErrorCode::MigrationFailed,
            "memory schema is newer than this host supports",
        ));
    }
    if current < MEMORY_SCHEMA_VERSION {
        sqlx::query("INSERT INTO memory_schema_migrations(version) VALUES (?)")
            .bind(MEMORY_SCHEMA_VERSION)
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                database_error(
                    ErrorCode::MigrationFailed,
                    "record memory schema version",
                    error,
                )
            })?;
    }
    tx.commit().await.map_err(|error| {
        database_error(ErrorCode::MigrationFailed, "commit memory migration", error)
    })
}

impl SqliteStore {
    pub async fn create_memory_asset(
        &self,
        commit: MemoryCreateCommit,
    ) -> Result<StoredMemoryAssetRecord, StoreError> {
        validate_record(&commit.record)?;
        if commit.record.asset.current_version != 1 || commit.record.current.record.version != 1 {
            return Err(StoreError::new(
                ErrorCode::InvalidSequence,
                "a new memory asset must start at version 1",
            ));
        }
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        insert_asset(&mut tx, &commit.record).await?;
        insert_version(&mut tx, &commit.record.current).await?;
        refresh_fts(&mut tx, &commit.record).await?;
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit memory asset", error)
        })?;
        Ok(commit.record)
    }

    #[allow(clippy::too_many_lines)] // One fenced transaction owns the CAS and dependent invalidation.
    pub async fn write_memory_version(
        &self,
        commit: MemoryVersionCommit,
    ) -> Result<StoredMemoryAssetRecord, StoreError> {
        commit.asset.validate().map_err(|error| {
            StoreError::new(error.code(), format!("invalid memory asset: {error}"))
        })?;
        commit.version.record.validate().map_err(|error| {
            StoreError::new(error.code(), format!("invalid memory version: {error}"))
        })?;
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_authorized(
            &mut tx,
            &commit.authorization,
            &commit.memory_asset_id,
            &commit.action,
        )
        .await?;
        let current = sqlx::query_scalar::<_, i64>(
            "SELECT current_version FROM memory_assets WHERE memory_asset_id = ?",
        )
        .bind(commit.memory_asset_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "read memory CAS pointer",
                error,
            )
        })?
        .ok_or_else(|| StoreError::new(ErrorCode::InvalidPayload, "memory asset was not found"))?;
        if current != to_i64(commit.expected_version, "expected memory version")? {
            return Err(StoreError::new(
                ErrorCode::SequenceConflict,
                "memory current version does not match expected version",
            ));
        }
        if commit.version.record.version != commit.expected_version.saturating_add(1)
            || commit.asset.current_version != commit.version.record.version
        {
            return Err(StoreError::new(
                ErrorCode::InvalidSequence,
                "memory CAS commit does not advance exactly one version",
            ));
        }
        for source in &commit.source_assets {
            assert_authorized(
                &mut tx,
                &commit.authorization,
                &source.memory_asset_id,
                "read",
            )
            .await?;
            let asset = load_asset_in_tx(&mut tx, &source.memory_asset_id)
                .await?
                .ok_or_else(|| {
                    StoreError::new(ErrorCode::InvalidPayload, "merge source missing")
                })?;
            if asset.asset.current_version != source.version
                || source.memory_asset_id == commit.memory_asset_id
                || matches!(
                    asset.asset.status,
                    MemoryAssetStatus::Invalidated | MemoryAssetStatus::Archived
                )
            {
                return Err(StoreError::new(
                    ErrorCode::SequenceConflict,
                    "semantic merge source is stale or cyclic",
                ));
            }
            advanced::assert_no_dependency_cycle(
                &mut tx,
                &commit.memory_asset_id,
                &source.memory_asset_id,
            )
            .await?;
        }
        insert_version(&mut tx, &commit.version).await?;
        if commit.source_assets.is_empty() {
            sqlx::query("INSERT OR IGNORE INTO memory_dependencies SELECT derived_asset_id, ?, source_kind, source_id, source_version FROM memory_dependencies WHERE derived_asset_id = ? AND derived_version = ? AND source_kind = 'asset'")
            .bind(to_i64(commit.version.record.version, "derived version")?).bind(commit.memory_asset_id.as_str()).bind(to_i64(commit.expected_version, "previous version")?)
            .execute(&mut *tx).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "inherit derived lineage", error))?;
        } else {
            for source in &commit.source_assets {
                advanced::insert_dependency(
                    &mut tx,
                    &commit.memory_asset_id,
                    commit.version.record.version,
                    "asset",
                    source.memory_asset_id.as_str(),
                    Some(source.version),
                )
                .await?;
            }
        }
        advanced::invalidate_tree(
            &mut tx,
            &commit.memory_asset_id,
            false,
            "source_version_changed",
        )
        .await?;
        let asset_json = serde_json::to_string(&commit.asset).map_err(|_| {
            StoreError::new(
                ErrorCode::InvalidPayload,
                "memory asset cannot be serialized",
            )
        })?;
        let updated = sqlx::query(
            "UPDATE memory_assets
             SET current_version = ?, status = ?, asset_json = ?, revision = revision + 1,
                 updated_at = CURRENT_TIMESTAMP
             WHERE memory_asset_id = ? AND current_version = ?",
        )
        .bind(to_i64(
            commit.asset.current_version,
            "memory current version",
        )?)
        .bind(status_name(commit.asset.status))
        .bind(asset_json)
        .bind(commit.memory_asset_id.as_str())
        .bind(to_i64(commit.expected_version, "expected memory version")?)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "advance memory CAS pointer",
                error,
            )
        })?;
        if updated.rows_affected() != 1 {
            return Err(StoreError::new(
                ErrorCode::SequenceConflict,
                "memory current version changed during CAS commit",
            ));
        }
        let record = load_asset_in_tx(&mut tx, &commit.memory_asset_id)
            .await?
            .ok_or_else(|| {
                StoreError::new(ErrorCode::InvalidPayload, "memory asset was not found")
            })?;
        refresh_fts(&mut tx, &record).await?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit memory version",
                error,
            )
        })?;
        Ok(record)
    }

    pub async fn grant_memory(
        &self,
        owner: &StoreMemoryPrincipal,
        grant: StoredMemoryGrantRecord,
    ) -> Result<(), StoreError> {
        if grant.revision == 0 || grant.allowed_actions.is_empty() {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                "memory grant requires a revision and actions",
            ));
        }
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let asset_owner = sqlx::query_scalar::<_, String>(
            "SELECT owner_id FROM memory_assets WHERE memory_asset_id = ?",
        )
        .bind(grant.memory_asset_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read memory owner", error))?
        .ok_or_else(|| StoreError::new(ErrorCode::InvalidPayload, "memory asset was not found"))?;
        if asset_owner != owner.principal_id {
            return Err(StoreError::new(
                ErrorCode::PolicyDenied,
                "only the exact memory owner may grant asset actions",
            ));
        }
        assert_authorized(&mut tx, owner, &grant.memory_asset_id, "bind").await?;
        let previous = sqlx::query_scalar::<_, i64>(
            "SELECT revision FROM memory_grants WHERE principal_id = ? AND memory_asset_id = ?",
        )
        .bind(&grant.principal_id)
        .bind(grant.memory_asset_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "read grant revision", error)
        })?
        .unwrap_or(0);
        if to_i64(grant.revision, "grant revision")? != previous + 1 {
            return Err(StoreError::new(
                ErrorCode::SequenceConflict,
                "grant revision must advance exactly once",
            ));
        }
        let actions = serde_json::to_string(&grant.allowed_actions).map_err(|_| {
            StoreError::new(
                ErrorCode::InvalidPayload,
                "memory grant actions are invalid",
            )
        })?;
        sqlx::query(
            "INSERT INTO memory_grants(principal_id, memory_asset_id, project_id, actions_json, revision, active)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(principal_id, memory_asset_id) DO UPDATE SET
               project_id = excluded.project_id, actions_json = excluded.actions_json,
               revision = excluded.revision, active = excluded.active",
        )
        .bind(&grant.principal_id)
        .bind(grant.memory_asset_id.as_str())
        .bind(grant.project_id.as_ref().map(ToString::to_string))
        .bind(actions)
        .bind(to_i64(grant.revision, "memory grant revision")?)
        .bind(i64::from(grant.active))
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "persist memory grant", error)
        })?;
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit memory grant", error)
        })
    }

    pub async fn read_memory_asset(
        &self,
        principal: &StoreMemoryPrincipal,
        memory_asset_id: &MemoryAssetId,
        action: &str,
    ) -> Result<Option<StoredMemoryAssetRecord>, StoreError> {
        let mut tx = self.pool.begin().await.map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "begin authorized memory read",
                error,
            )
        })?;
        let exists = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM memory_assets WHERE memory_asset_id = ?",
        )
        .bind(memory_asset_id.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageOpenFailed, "find memory asset", error)
        })?;
        if exists == 0 {
            return Ok(None);
        }
        assert_authorized(&mut tx, principal, memory_asset_id, action).await?;
        advanced::assert_lineage_authorized(&mut tx, principal, memory_asset_id, false).await?;
        let record = load_asset_in_tx(&mut tx, memory_asset_id).await?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "close authorized memory read",
                error,
            )
        })?;
        Ok(record)
    }

    pub async fn export_memory_versions(
        &self,
        principal: &StoreMemoryPrincipal,
        memory_asset_id: &MemoryAssetId,
        action: &str,
    ) -> Result<Vec<StoredMemoryVersionRecord>, StoreError> {
        let mut tx = self.pool.begin().await.map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "begin authorized memory export",
                error,
            )
        })?;
        assert_authorized(&mut tx, principal, memory_asset_id, action).await?;
        advanced::assert_lineage_authorized(&mut tx, principal, memory_asset_id, false).await?;
        let rows = sqlx::query(
            "SELECT version_json, content, normalized_content, strategy_digest
             FROM memory_versions WHERE memory_asset_id = ? ORDER BY version",
        )
        .bind(memory_asset_id.as_str())
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "export memory versions",
                error,
            )
        })?;
        let versions = rows.iter().map(decode_version).collect();
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "close authorized memory export",
                error,
            )
        })?;
        versions
    }

    pub async fn ensure_extraction_cursor(
        &self,
        source_stream: &harness_types::SessionId,
        extractor_version: &str,
        strategy_digest: &ContentHash,
        initial_sequence: u64,
    ) -> Result<u64, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        sqlx::query(
            "INSERT INTO extraction_cursors(source_stream, extractor_version, strategy_digest,
             contiguous_sequence, revision) VALUES (?, ?, ?, ?, 1)
             ON CONFLICT(source_stream, extractor_version, strategy_digest) DO NOTHING",
        )
        .bind(source_stream.as_str())
        .bind(extractor_version)
        .bind(strategy_digest.as_str())
        .bind(to_i64(initial_sequence, "initial extraction sequence")?)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "initialize extraction cursor",
                error,
            )
        })?;
        let cursor =
            read_cursor(&mut tx, source_stream, extractor_version, strategy_digest).await?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit extraction cursor",
                error,
            )
        })?;
        Ok(cursor)
    }

    pub async fn extraction_schedule_start(
        &self,
        source_stream: &harness_types::SessionId,
        extractor_version: &str,
        strategy_digest: &ContentHash,
    ) -> Result<u64, StoreError> {
        let cursor = self
            .extraction_cursor(source_stream, extractor_version, strategy_digest)
            .await?;
        let latest = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT MAX(end_sequence) FROM memory_jobs
             WHERE source_stream = ? AND extractor_version = ? AND strategy_digest = ?",
        )
        .bind(source_stream.as_str())
        .bind(extractor_version)
        .bind(strategy_digest.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "read extraction schedule tail",
                error,
            )
        })?
        .unwrap_or(0);
        let latest = u64::try_from(latest).map_err(|_| {
            StoreError::new(
                ErrorCode::InvalidSequence,
                "stored extraction range is negative",
            )
        })?;
        Ok(cursor.max(latest).saturating_add(1))
    }

    pub async fn load_memory_source_events(
        &self,
        source_stream: &harness_types::SessionId,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<harness_types::EventEnvelope>, StoreError> {
        if limit == 0 {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                "memory source page limit must be positive",
            ));
        }
        let rows = sqlx::query(
            "SELECT events.event_json FROM source_work_markers
             JOIN events ON events.session_id = source_work_markers.session_id
                        AND events.event_id = source_work_markers.event_id
             WHERE source_work_markers.session_id = ? AND source_work_markers.sequence > ?
             ORDER BY source_work_markers.sequence LIMIT ?",
        )
        .bind(source_stream.as_str())
        .bind(to_i64(after_sequence, "memory source cursor")?)
        .bind(i64::try_from(limit).map_err(|_| {
            StoreError::new(
                ErrorCode::InvalidPayload,
                "memory source page limit is too large",
            )
        })?)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "load memory source markers",
                error,
            )
        })?;
        rows.iter()
            .map(|row| {
                serde_json::from_str(row.get("event_json")).map_err(|_| {
                    StoreError::new(
                        ErrorCode::InvalidPayload,
                        "stored memory source event is invalid",
                    )
                })
            })
            .collect()
    }

    pub async fn insert_extraction_job(
        &self,
        job: StoredExtractionJobRecord,
    ) -> Result<(), StoreError> {
        validate_job(&job)?;
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let overlap = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM memory_jobs
             WHERE source_stream = ? AND extractor_version = ? AND strategy_digest = ?
               AND NOT (end_sequence < ? OR start_sequence > ?)",
        )
        .bind(job.source_stream.as_str())
        .bind(&job.extractor_version)
        .bind(job.strategy_digest.as_str())
        .bind(to_i64(job.start_sequence, "extraction range start")?)
        .bind(to_i64(job.end_sequence, "extraction range end")?)
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "check extraction range overlap",
                error,
            )
        })?;
        if overlap != 0 {
            return Err(StoreError::new(
                ErrorCode::IdempotencyConflict,
                "extraction source ranges must not overlap",
            ));
        }
        let source_ids = serde_json::to_string(&job.source_event_ids).map_err(|_| {
            StoreError::new(
                ErrorCode::InvalidPayload,
                "extraction source IDs are invalid",
            )
        })?;
        sqlx::query(
            "INSERT INTO memory_jobs(job_id, source_stream, start_sequence, end_sequence,
             source_digest, source_ids_json, extractor_version, strategy_digest, status,
             attempts, lease_owner, lease_generation, last_error, disposition)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&job.job_id)
        .bind(job.source_stream.as_str())
        .bind(to_i64(job.start_sequence, "extraction range start")?)
        .bind(to_i64(job.end_sequence, "extraction range end")?)
        .bind(job.source_digest.as_str())
        .bind(source_ids)
        .bind(&job.extractor_version)
        .bind(job.strategy_digest.as_str())
        .bind(&job.status)
        .bind(i64::from(job.attempts))
        .bind(job.lease_owner)
        .bind(to_i64(job.lease_generation, "extraction lease generation")?)
        .bind(job.last_error)
        .bind(job.disposition)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "insert extraction job",
                error,
            )
        })?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit extraction job",
                error,
            )
        })
    }

    pub async fn lease_extraction_job(
        &self,
        job_id: &str,
        owner: &str,
    ) -> Result<StoredExtractionLeaseRecord, StoreError> {
        if owner.trim().is_empty() {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                "lease owner is required",
            ));
        }
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let changed = sqlx::query(
            "UPDATE memory_jobs SET status = 'leased', lease_owner = ?,
             lease_generation = lease_generation + 1, attempts = attempts + 1, last_error = NULL
             WHERE job_id = ? AND status IN ('pending', 'retry_wait', 'paused')
             AND (status != 'retry_wait' OR next_due_unix_ms <= CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER))",
        )
        .bind(owner)
        .bind(job_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "lease extraction job", error)
        })?;
        if changed.rows_affected() != 1 {
            return Err(StoreError::new(
                ErrorCode::RuntimeCommandConflict,
                "extraction job is not leaseable",
            ));
        }
        let row = sqlx::query("SELECT * FROM memory_jobs WHERE job_id = ?")
            .bind(job_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| {
                database_error(
                    ErrorCode::StorageWriteFailed,
                    "read leased extraction job",
                    error,
                )
            })?;
        let job = decode_job(&row)?;
        let source_rows = sqlx::query(
            "SELECT event_json FROM events WHERE session_id = ? AND sequence BETWEEN ? AND ?
             ORDER BY sequence",
        )
        .bind(job.source_stream.as_str())
        .bind(to_i64(job.start_sequence, "extraction range start")?)
        .bind(to_i64(job.end_sequence, "extraction range end")?)
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "load leased source batch",
                error,
            )
        })?;
        let source_events = source_rows
            .iter()
            .map(|row| {
                serde_json::from_str(row.get("event_json")).map_err(|_| {
                    StoreError::new(ErrorCode::InvalidPayload, "leased source event is invalid")
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit extraction lease",
                error,
            )
        })?;
        Ok(StoredExtractionLeaseRecord {
            generation: job.lease_generation,
            owner: owner.to_owned(),
            job,
            source_events,
        })
    }

    pub async fn settle_extraction_no_facts(
        &self,
        lease: &StoredExtractionLeaseRecord,
    ) -> Result<(), StoreError> {
        self.settle_extraction_assets(lease, &[]).await
    }

    pub async fn settle_extraction_assets(
        &self,
        lease: &StoredExtractionLeaseRecord,
        assets: &[StoredMemoryAssetRecord],
    ) -> Result<(), StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_job_lease(&mut tx, lease).await?;
        let cursor = read_cursor(
            &mut tx,
            &lease.job.source_stream,
            &lease.job.extractor_version,
            &lease.job.strategy_digest,
        )
        .await?;
        if lease.job.start_sequence != cursor.saturating_add(1) {
            return Err(StoreError::new(
                ErrorCode::SequenceConflict,
                "extraction settlement cannot advance across a cursor gap",
            ));
        }
        for asset in assets {
            validate_record(asset)?;
            if asset.current.record.source_event_refs.is_empty()
                || asset
                    .current
                    .record
                    .source_event_refs
                    .iter()
                    .any(|id| !lease.job.source_event_ids.contains(id))
            {
                return Err(StoreError::new(
                    ErrorCode::InvalidPayload,
                    "extracted asset has foreign sources",
                ));
            }
            insert_asset(&mut tx, asset).await?;
            insert_version(&mut tx, &asset.current).await?;
            refresh_fts(&mut tx, asset).await?;
        }
        sqlx::query(
            "UPDATE memory_jobs SET status = 'completed', disposition = ?,
             lease_owner = NULL WHERE job_id = ? AND lease_owner = ? AND lease_generation = ?",
        )
        .bind(if assets.is_empty() {
            "no_facts"
        } else {
            "candidates"
        })
        .bind(&lease.job.job_id)
        .bind(&lease.owner)
        .bind(to_i64(lease.generation, "extraction lease generation")?)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "settle no-facts job", error)
        })?;
        sqlx::query(
            "UPDATE extraction_cursors SET contiguous_sequence = ?, revision = revision + 1
             WHERE source_stream = ? AND extractor_version = ? AND strategy_digest = ?
               AND contiguous_sequence = ?",
        )
        .bind(to_i64(lease.job.end_sequence, "extraction range end")?)
        .bind(lease.job.source_stream.as_str())
        .bind(&lease.job.extractor_version)
        .bind(lease.job.strategy_digest.as_str())
        .bind(to_i64(cursor, "extraction cursor")?)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "advance extraction cursor",
                error,
            )
        })?;
        self.inject(StoreFaultPoint::BeforeMemorySettlementCommit)?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit extraction settlement",
                error,
            )
        })
    }

    pub async fn fail_extraction_job(
        &self,
        lease: &StoredExtractionLeaseRecord,
        status: &str,
        message: &str,
    ) -> Result<(), StoreError> {
        if !matches!(status, "retry_wait" | "blocked" | "paused" | "dead_letter") {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                "invalid extraction failure state",
            ));
        }
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_job_lease(&mut tx, lease).await?;
        sqlx::query(
            "UPDATE memory_jobs SET status = ?, last_error = ?, lease_owner = NULL,
             next_due_unix_ms = CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
                 + min(60000, 100 * (1 << min(attempts, 9))) + abs(random() % 100)
             WHERE job_id = ? AND lease_owner = ? AND lease_generation = ?",
        )
        .bind(status)
        .bind(message)
        .bind(&lease.job.job_id)
        .bind(&lease.owner)
        .bind(to_i64(lease.generation, "extraction lease generation")?)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "record extraction failure",
                error,
            )
        })?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit extraction failure",
                error,
            )
        })
    }

    pub async fn extraction_cursor(
        &self,
        source_stream: &harness_types::SessionId,
        extractor_version: &str,
        strategy_digest: &ContentHash,
    ) -> Result<u64, StoreError> {
        let value = sqlx::query_scalar::<_, i64>(
            "SELECT contiguous_sequence FROM extraction_cursors
             WHERE source_stream = ? AND extractor_version = ? AND strategy_digest = ?",
        )
        .bind(source_stream.as_str())
        .bind(extractor_version)
        .bind(strategy_digest.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "read extraction cursor",
                error,
            )
        })?
        .unwrap_or(0);
        u64::try_from(value).map_err(|_| {
            StoreError::new(
                ErrorCode::InvalidSequence,
                "stored extraction cursor is negative",
            )
        })
    }

    pub async fn list_extraction_jobs(&self) -> Result<Vec<StoredExtractionJobRecord>, StoreError> {
        let rows = sqlx::query("SELECT * FROM memory_jobs ORDER BY source_stream, start_sequence")
            .fetch_all(&self.pool)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageOpenFailed, "list extraction jobs", error)
            })?;
        rows.iter().map(decode_job).collect()
    }

    pub async fn recover_extraction_jobs(&self) -> Result<u64, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let result = sqlx::query(
            "UPDATE memory_jobs SET status = 'pending', lease_owner = NULL
             WHERE status = 'leased'",
        )
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "recover interrupted extraction jobs",
                error,
            )
        })?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit extraction recovery",
                error,
            )
        })?;
        Ok(result.rows_affected())
    }
}

async fn read_cursor(
    tx: &mut Transaction<'_, Sqlite>,
    stream: &harness_types::SessionId,
    extractor: &str,
    strategy: &ContentHash,
) -> Result<u64, StoreError> {
    let value = sqlx::query_scalar::<_, i64>(
        "SELECT contiguous_sequence FROM extraction_cursors
         WHERE source_stream = ? AND extractor_version = ? AND strategy_digest = ?",
    )
    .bind(stream.as_str())
    .bind(extractor)
    .bind(strategy.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| {
        database_error(
            ErrorCode::StorageWriteFailed,
            "read extraction cursor in transaction",
            error,
        )
    })?
    .ok_or_else(|| StoreError::new(ErrorCode::InvalidPayload, "extraction cursor is missing"))?;
    u64::try_from(value).map_err(|_| {
        StoreError::new(
            ErrorCode::InvalidSequence,
            "stored extraction cursor is negative",
        )
    })
}

async fn assert_job_lease(
    tx: &mut Transaction<'_, Sqlite>,
    lease: &StoredExtractionLeaseRecord,
) -> Result<(), StoreError> {
    let row = sqlx::query("SELECT * FROM memory_jobs WHERE job_id = ?")
        .bind(&lease.job.job_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "load immutable job", error)
        })?
        .ok_or_else(|| StoreError::new(ErrorCode::StaleWriter, "extraction job missing"))?;
    let stored = decode_job(&row)?;
    if stored.source_stream != lease.job.source_stream
        || stored.start_sequence != lease.job.start_sequence
        || stored.end_sequence != lease.job.end_sequence
        || stored.source_event_ids != lease.job.source_event_ids
        || stored.extractor_version != lease.job.extractor_version
        || stored.strategy_digest != lease.job.strategy_digest
    {
        return Err(StoreError::new(
            ErrorCode::StaleWriter,
            "immutable extraction range was altered",
        ));
    }
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM memory_jobs WHERE job_id = ? AND status = 'leased'
         AND lease_owner = ? AND lease_generation = ? AND source_digest = ?",
    )
    .bind(&lease.job.job_id)
    .bind(&lease.owner)
    .bind(to_i64(lease.generation, "extraction lease generation")?)
    .bind(lease.job.source_digest.as_str())
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| {
        database_error(
            ErrorCode::StorageWriteFailed,
            "validate extraction lease",
            error,
        )
    })?;
    if count == 1 {
        Ok(())
    } else {
        Err(StoreError::new(
            ErrorCode::StaleWriter,
            "extraction lease owner or generation is stale",
        ))
    }
}

fn validate_job(job: &StoredExtractionJobRecord) -> Result<(), StoreError> {
    if job.job_id.trim().is_empty()
        || job.extractor_version.trim().is_empty()
        || job.start_sequence == 0
        || job.end_sequence < job.start_sequence
        || job.source_event_ids.len()
            != usize::try_from(job.end_sequence - job.start_sequence + 1).unwrap_or(usize::MAX)
    {
        return Err(StoreError::new(
            ErrorCode::InvalidPayload,
            "extraction job range is invalid",
        ));
    }
    Ok(())
}

fn decode_job(row: &sqlx::sqlite::SqliteRow) -> Result<StoredExtractionJobRecord, StoreError> {
    let source_stream = harness_types::SessionId::parse(row.get::<String, _>("source_stream"))
        .map_err(|error| StoreError::new(error.code(), error.to_string()))?;
    let source_digest = ContentHash::parse(row.get::<String, _>("source_digest"))
        .map_err(|error| StoreError::new(error.code(), error.to_string()))?;
    let strategy_digest = ContentHash::parse(row.get::<String, _>("strategy_digest"))
        .map_err(|error| StoreError::new(error.code(), error.to_string()))?;
    let source_event_ids = serde_json::from_str(row.get("source_ids_json")).map_err(|_| {
        StoreError::new(
            ErrorCode::InvalidPayload,
            "stored extraction source IDs are invalid",
        )
    })?;
    let start = row.get::<i64, _>("start_sequence");
    let end = row.get::<i64, _>("end_sequence");
    let attempts = row.get::<i64, _>("attempts");
    let generation = row.get::<i64, _>("lease_generation");
    Ok(StoredExtractionJobRecord {
        job_id: row.get("job_id"),
        source_stream,
        start_sequence: u64::try_from(start)
            .map_err(|_| StoreError::new(ErrorCode::InvalidSequence, "job start is negative"))?,
        end_sequence: u64::try_from(end)
            .map_err(|_| StoreError::new(ErrorCode::InvalidSequence, "job end is negative"))?,
        source_digest,
        source_event_ids,
        extractor_version: row.get("extractor_version"),
        strategy_digest,
        status: row.get("status"),
        attempts: u32::try_from(attempts)
            .map_err(|_| StoreError::new(ErrorCode::InvalidSequence, "job attempts are invalid"))?,
        lease_owner: row.get("lease_owner"),
        lease_generation: u64::try_from(generation).map_err(|_| {
            StoreError::new(ErrorCode::InvalidSequence, "job generation is negative")
        })?,
        last_error: row.get("last_error"),
        disposition: row.get("disposition"),
    })
}

async fn insert_asset(
    tx: &mut Transaction<'_, Sqlite>,
    record: &StoredMemoryAssetRecord,
) -> Result<(), StoreError> {
    let json = serde_json::to_string(&record.asset).map_err(|_| {
        StoreError::new(
            ErrorCode::InvalidPayload,
            "memory asset cannot be serialized",
        )
    })?;
    sqlx::query(
        "INSERT INTO memory_assets(memory_asset_id, owner_id, project_id, task_id,
         agent_profile_id, session_id, scope, layer, status, current_version, asset_json, revision)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1)",
    )
    .bind(record.asset.memory_asset_id.as_str())
    .bind(&record.asset.owner_id)
    .bind(record.asset.project_id.as_ref().map(ToString::to_string))
    .bind(record.task_id.as_ref().map(ToString::to_string))
    .bind(record.agent_profile_id.as_ref().map(ToString::to_string))
    .bind(record.session_id.as_ref().map(ToString::to_string))
    .bind(scope_name(record.asset.scope))
    .bind(&record.layer)
    .bind(status_name(record.asset.status))
    .bind(to_i64(
        record.asset.current_version,
        "memory current version",
    )?)
    .bind(json)
    .execute(&mut **tx)
    .await
    .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "insert memory asset", error))?;
    sqlx::query("INSERT INTO memory_bindings(binding_id, memory_asset_id, principal_id, injection_mode, priority, revision) VALUES (?, ?, ?, 'on_demand', 0, 1)")
        .bind(format!("owner:{}", record.asset.memory_asset_id)).bind(record.asset.memory_asset_id.as_str()).bind(&record.asset.owner_id)
        .execute(&mut **tx).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "bind owner memory", error))?;
    Ok(())
}

async fn insert_version(
    tx: &mut Transaction<'_, Sqlite>,
    version: &StoredMemoryVersionRecord,
) -> Result<(), StoreError> {
    let json = serde_json::to_string(&version.record).map_err(|_| {
        StoreError::new(
            ErrorCode::InvalidPayload,
            "memory version cannot be serialized",
        )
    })?;
    sqlx::query(
        "INSERT INTO memory_versions(memory_asset_id, version, content, normalized_content,
         content_hash, version_json, strategy_digest) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(version.record.memory_asset_id.as_str())
    .bind(to_i64(version.record.version, "memory version")?)
    .bind(&version.content)
    .bind(&version.normalized_content)
    .bind(version.record.content_hash.as_str())
    .bind(json)
    .bind(version.strategy_digest.as_ref().map(ContentHash::as_str))
    .execute(&mut **tx)
    .await
    .map_err(|error| {
        database_error(
            ErrorCode::StorageWriteFailed,
            "insert memory version",
            error,
        )
    })?;
    for source in &version.record.source_event_refs {
        advanced::insert_dependency(
            tx,
            &version.record.memory_asset_id,
            version.record.version,
            "event",
            source.as_str(),
            None,
        )
        .await?;
    }
    for source in &version.record.source_file_hashes {
        advanced::insert_dependency(
            tx,
            &version.record.memory_asset_id,
            version.record.version,
            "file",
            source.as_str(),
            None,
        )
        .await?;
    }
    if let Some(commit) = &version.record.source_commit {
        advanced::insert_dependency(
            tx,
            &version.record.memory_asset_id,
            version.record.version,
            "commit",
            commit,
            None,
        )
        .await?;
    }
    Ok(())
}

async fn refresh_fts(
    tx: &mut Transaction<'_, Sqlite>,
    record: &StoredMemoryAssetRecord,
) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM memory_fts WHERE memory_asset_id = ?")
        .bind(record.asset.memory_asset_id.as_str())
        .execute(&mut **tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "remove stale memory index",
                error,
            )
        })?;
    if record.asset.status == MemoryAssetStatus::Active
        && record.current.record.validity == Validity::Valid
    {
        sqlx::query(
            "INSERT INTO memory_fts(memory_asset_id, version, content, normalized_content)
             VALUES (?, ?, ?, ?)",
        )
        .bind(record.asset.memory_asset_id.as_str())
        .bind(to_i64(
            record.current.record.version,
            "memory index version",
        )?)
        .bind(&record.current.content)
        .bind(&record.current.normalized_content)
        .execute(&mut **tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "index memory version", error)
        })?;
    }
    Ok(())
}

async fn assert_authorized(
    tx: &mut Transaction<'_, Sqlite>,
    principal: &StoreMemoryPrincipal,
    id: &MemoryAssetId,
    action: &str,
) -> Result<(), StoreError> {
    let row = sqlx::query(
        "SELECT owner_id, project_id, task_id, agent_profile_id, session_id
         FROM memory_assets WHERE memory_asset_id = ?",
    )
    .bind(id.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| {
        database_error(
            ErrorCode::StorageWriteFailed,
            "authorize memory asset",
            error,
        )
    })?
    .ok_or_else(|| StoreError::new(ErrorCode::InvalidPayload, "memory asset was not found"))?;
    authorize_row(tx, principal, id, action, &row).await
}

async fn authorize_row(
    tx: &mut Transaction<'_, Sqlite>,
    principal: &StoreMemoryPrincipal,
    id: &MemoryAssetId,
    action: &str,
    row: &sqlx::sqlite::SqliteRow,
) -> Result<(), StoreError> {
    let owner: String = row.get("owner_id");
    let project: Option<String> = row.get("project_id");
    let task: Option<String> = row.get("task_id");
    let profile: Option<String> = row.get("agent_profile_id");
    let session: Option<String> = row.get("session_id");
    let scope_matches = project.as_deref().is_none_or(|value| {
        principal
            .project_id
            .as_ref()
            .is_some_and(|id| id.as_str() == value)
    }) && task.as_deref().is_none_or(|value| {
        principal
            .task_id
            .as_ref()
            .is_some_and(|id| id.as_str() == value)
    }) && profile.as_deref().is_none_or(|value| {
        principal
            .agent_profile_id
            .as_ref()
            .is_some_and(|id| id.as_str() == value)
    }) && session.as_deref().is_none_or(|value| {
        principal
            .session_id
            .as_ref()
            .is_some_and(|id| id.as_str() == value)
    });
    if !scope_matches {
        return Err(StoreError::new(
            ErrorCode::PolicyDenied,
            "memory asset scope does not match the host principal",
        ));
    }
    if owner == principal.principal_id {
        return Ok(());
    }
    let grant = sqlx::query(
        "SELECT project_id, actions_json, active FROM memory_grants
         WHERE principal_id = ? AND memory_asset_id = ?",
    )
    .bind(&principal.principal_id)
    .bind(id.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read memory grant", error))?;
    let Some(grant) = grant else {
        return Err(StoreError::new(
            ErrorCode::PolicyDenied,
            "memory action has no active exact-principal grant",
        ));
    };
    let active: i64 = grant.get("active");
    let grant_project: Option<String> = grant.get("project_id");
    let actions_json: String = grant.get("actions_json");
    let actions: Vec<String> = serde_json::from_str(&actions_json).map_err(|_| {
        StoreError::new(ErrorCode::InvalidPayload, "stored memory grant is invalid")
    })?;
    let project_matches = grant_project.as_deref().is_none_or(|value| {
        principal
            .project_id
            .as_ref()
            .is_some_and(|project_id| project_id.as_str() == value)
    });
    if active == 1 && project_matches && actions.iter().any(|candidate| candidate == action) {
        Ok(())
    } else {
        Err(StoreError::new(
            ErrorCode::PolicyDenied,
            "memory action is outside the active exact-principal grant",
        ))
    }
}

async fn load_asset_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &MemoryAssetId,
) -> Result<Option<StoredMemoryAssetRecord>, StoreError> {
    let row = sqlx::query(
        "SELECT asset_json, layer, task_id, agent_profile_id, session_id
         FROM memory_assets WHERE memory_asset_id = ?",
    )
    .bind(id.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| {
        database_error(
            ErrorCode::StorageWriteFailed,
            "load memory asset in transaction",
            error,
        )
    })?;
    let Some(row) = row else { return Ok(None) };
    let asset: MemoryAsset = serde_json::from_str(row.get("asset_json")).map_err(|_| {
        StoreError::new(ErrorCode::InvalidPayload, "stored memory asset is invalid")
    })?;
    let version_row = sqlx::query(
        "SELECT version_json, content, normalized_content, strategy_digest
         FROM memory_versions WHERE memory_asset_id = ? AND version = ?",
    )
    .bind(asset.memory_asset_id.as_str())
    .bind(to_i64(asset.current_version, "memory current version")?)
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| {
        database_error(
            ErrorCode::StorageWriteFailed,
            "load current memory version",
            error,
        )
    })?;
    decode_asset_row(&row, asset, decode_version(&version_row)?).map(Some)
}

fn decode_asset_row(
    row: &sqlx::sqlite::SqliteRow,
    asset: MemoryAsset,
    current: StoredMemoryVersionRecord,
) -> Result<StoredMemoryAssetRecord, StoreError> {
    Ok(StoredMemoryAssetRecord {
        asset,
        layer: row.get("layer"),
        task_id: parse_optional_id(row.get("task_id"), TaskId::parse)?,
        agent_profile_id: parse_optional_id(row.get("agent_profile_id"), AgentProfileId::parse)?,
        session_id: parse_optional_id(row.get("session_id"), SessionId::parse)?,
        current,
    })
}

fn decode_version(row: &sqlx::sqlite::SqliteRow) -> Result<StoredMemoryVersionRecord, StoreError> {
    let record: MemoryVersion = serde_json::from_str(row.get("version_json")).map_err(|_| {
        StoreError::new(
            ErrorCode::InvalidPayload,
            "stored memory version is invalid",
        )
    })?;
    let digest = row
        .get::<Option<String>, _>("strategy_digest")
        .map(|value| {
            ContentHash::parse(value)
                .map_err(|error| StoreError::new(error.code(), error.to_string()))
        })
        .transpose()?;
    Ok(StoredMemoryVersionRecord {
        record,
        content: row.get("content"),
        normalized_content: row.get("normalized_content"),
        strategy_digest: digest,
    })
}

fn parse_optional_id<T, F>(value: Option<String>, parse: F) -> Result<Option<T>, StoreError>
where
    F: FnOnce(String) -> Result<T, harness_types::HarnessError>,
{
    value
        .map(|text| parse(text).map_err(|error| StoreError::new(error.code(), error.to_string())))
        .transpose()
}

fn validate_record(record: &StoredMemoryAssetRecord) -> Result<(), StoreError> {
    record
        .asset
        .validate()
        .map_err(|error| StoreError::new(error.code(), format!("invalid memory asset: {error}")))?;
    record.current.record.validate().map_err(|error| {
        StoreError::new(error.code(), format!("invalid memory version: {error}"))
    })?;
    if record.current.record.memory_asset_id != record.asset.memory_asset_id {
        return Err(StoreError::new(
            ErrorCode::InvalidPayload,
            "memory asset and version IDs differ",
        ));
    }
    Ok(())
}

const fn scope_name(scope: harness_types::MemoryScope) -> &'static str {
    match scope {
        harness_types::MemoryScope::User => "user",
        harness_types::MemoryScope::Project => "project",
        harness_types::MemoryScope::Task => "task",
        harness_types::MemoryScope::AgentProfile => "agent_profile",
        harness_types::MemoryScope::Session => "session",
    }
}

const fn status_name(status: MemoryAssetStatus) -> &'static str {
    match status {
        MemoryAssetStatus::Candidate => "candidate",
        MemoryAssetStatus::Active => "active",
        MemoryAssetStatus::Superseded => "superseded",
        MemoryAssetStatus::Invalidated => "invalidated",
        MemoryAssetStatus::Archived => "archived",
    }
}
