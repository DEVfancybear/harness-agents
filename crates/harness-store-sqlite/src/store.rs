use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    time::Duration,
};

use fs2::FileExt;
use harness_types::{
    AgentRunId, ArtifactId, CompositionSnapshotId, ContentHash, ContextPacket, ContextPacketId,
    ErrorCode, EventEnvelope, EventId, HostId, PluginInstanceId, PluginManifest, ProviderAttemptId,
    RequestId, RuntimeCommandId, SessionId, SnapshotId, TaskId, WorkingState,
};
use serde::Serialize;
use serde_json::Value;
use sqlx::{
    Row, Sqlite, SqlitePool, Transaction,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};

use crate::{
    AdmissionAck, AdmissionCommit, AgentStateRecord, CompositionSnapshotRecord,
    ContextCheckpointRecord, ContextPacketRecord, ContinuationLinkRecord, FrozenRequestRecord,
    HostFence, PersistedPluginManifest, ProviderAttemptRecord, PublishedArtifact,
    RUNTIME_SCHEMA_VERSION, ReceiptAck, ReceiptCommit, RuntimeCommandRecord, RuntimeCommandState,
    STORE_SCHEMA_VERSION, SessionSummary, SnapshotRecord, SourceWorkMarker, StoreDiagnostics,
    StoreError, StoreFaultPlan, StoreFaultPoint, StorePaths, WriterOpenOptions,
};

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

const MIGRATION_1: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS host_epoch (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        generation INTEGER NOT NULL CHECK (generation >= 0)
    )",
    "CREATE TABLE IF NOT EXISTS hosts (
        host_id TEXT PRIMARY KEY,
        generation INTEGER NOT NULL CHECK (generation >= 1),
        opened_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    )",
    "CREATE TABLE IF NOT EXISTS sessions (
        session_id TEXT PRIMARY KEY,
        task_id TEXT NOT NULL,
        next_sequence INTEGER NOT NULL CHECK (next_sequence >= 1),
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    )",
    "CREATE TABLE IF NOT EXISTS task_leases (
        task_id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL REFERENCES sessions(session_id),
        host_id TEXT NOT NULL,
        generation INTEGER NOT NULL CHECK (generation >= 1),
        claimed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    )",
    "CREATE TABLE IF NOT EXISTS events (
        session_id TEXT NOT NULL REFERENCES sessions(session_id),
        sequence INTEGER NOT NULL CHECK (sequence >= 1),
        event_id TEXT NOT NULL UNIQUE,
        event_json TEXT NOT NULL,
        continuity_critical INTEGER NOT NULL CHECK (continuity_critical IN (0, 1)),
        PRIMARY KEY (session_id, sequence)
    )",
    "CREATE TABLE IF NOT EXISTS inbox (
        input_id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL REFERENCES sessions(session_id),
        task_id TEXT NOT NULL,
        input_hash TEXT NOT NULL,
        raw_text TEXT NOT NULL,
        event_id TEXT NOT NULL UNIQUE REFERENCES events(event_id),
        admitted_sequence INTEGER NOT NULL CHECK (admitted_sequence >= 1)
    )",
    "CREATE TABLE IF NOT EXISTS instruction_ledger (
        instruction_id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL REFERENCES sessions(session_id),
        source_event_id TEXT NOT NULL UNIQUE REFERENCES events(event_id),
        instruction_json TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS task_projections (
        task_id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL REFERENCES sessions(session_id),
        revision INTEGER NOT NULL CHECK (revision >= 1),
        through_sequence INTEGER NOT NULL CHECK (through_sequence >= 1),
        working_state_json TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS source_work_markers (
        marker_id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL REFERENCES sessions(session_id),
        task_id TEXT NOT NULL,
        event_id TEXT NOT NULL REFERENCES events(event_id),
        sequence INTEGER NOT NULL CHECK (sequence >= 1),
        marker_json TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS snapshots (
        snapshot_id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL REFERENCES sessions(session_id),
        task_id TEXT NOT NULL,
        through_sequence INTEGER NOT NULL CHECK (through_sequence >= 1),
        schema_version INTEGER NOT NULL CHECK (schema_version >= 1),
        snapshot_json TEXT NOT NULL,
        content_hash TEXT NOT NULL,
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    )",
    "CREATE INDEX IF NOT EXISTS snapshots_by_session_sequence
        ON snapshots(session_id, through_sequence DESC)",
    "CREATE TABLE IF NOT EXISTS artifacts (
        artifact_id TEXT PRIMARY KEY,
        content_hash TEXT NOT NULL,
        byte_len INTEGER NOT NULL CHECK (byte_len >= 0),
        relative_path TEXT NOT NULL UNIQUE
    )",
    "CREATE TABLE IF NOT EXISTS receipts (
        tool_execution_id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL REFERENCES sessions(session_id),
        task_id TEXT NOT NULL,
        event_id TEXT NOT NULL UNIQUE REFERENCES events(event_id),
        sequence INTEGER NOT NULL CHECK (sequence >= 1),
        receipt_json TEXT NOT NULL,
        artifact_id TEXT REFERENCES artifacts(artifact_id)
    )",
    "CREATE TABLE IF NOT EXISTS plugin_manifests (
        instance_id TEXT PRIMARY KEY,
        generation INTEGER NOT NULL CHECK (generation >= 1),
        manifest_json TEXT NOT NULL
    )",
];

struct WriterLease {
    lock_file: File,
    fence: HostFence,
}

/// The P1 `SQLite` transaction coordinator.
///
/// There is one connection in each pool deliberately: all durable P1 writes
/// are serialized through one host-owned coordinator, while process ownership
/// is protected by the lock file and fencing epoch.
pub struct SqliteStore {
    paths: StorePaths,
    pool: SqlitePool,
    writer: Option<WriterLease>,
    fault_plan: StoreFaultPlan,
}

impl std::fmt::Debug for SqliteStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SqliteStore")
            .field("paths", &self.paths)
            .field("writer", &self.writer.is_some())
            .finish_non_exhaustive()
    }
}

impl SqliteStore {
    /// Open a writable, migrated store and acquire a new fenced host generation.
    pub async fn open_writer(options: WriterOpenOptions) -> Result<Self, StoreError> {
        let paths = StorePaths::new(options.data_dir);
        fs::create_dir_all(&paths.data_dir).map_err(|error| {
            StoreError::new(
                ErrorCode::StorageOpenFailed,
                format!("cannot create data directory: {error}"),
            )
        })?;
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&paths.writer_lock_path)
            .map_err(|error| {
                StoreError::new(
                    ErrorCode::StorageOpenFailed,
                    format!("cannot open writer lock: {error}"),
                )
            })?;
        lock_file.try_lock_exclusive().map_err(|_| {
            StoreError::new(
                ErrorCode::WriterLocked,
                "another writable host owns this data directory",
            )
        })?;

        let pool = open_pool(&paths, false).await?;
        if let Err(error) = run_migrations(&pool, &options.fault_plan).await {
            let _ = FileExt::unlock(&lock_file);
            return Err(error);
        }
        if let Err(error) = ensure_runtime_schema(&pool).await {
            let _ = FileExt::unlock(&lock_file);
            return Err(error);
        }
        let fence = match acquire_fence(&pool, options.host_id).await {
            Ok(fence) => fence,
            Err(error) => {
                let _ = FileExt::unlock(&lock_file);
                return Err(error);
            }
        };

        Ok(Self {
            paths,
            pool,
            writer: Some(WriterLease { lock_file, fence }),
            fault_plan: options.fault_plan,
        })
    }

    /// Open an existing store without lock acquisition, creation, or migration.
    pub async fn open_read_only(
        data_dir: impl Into<std::path::PathBuf>,
    ) -> Result<Self, StoreError> {
        let paths = StorePaths::new(data_dir);
        if !paths.database_path.is_file() {
            return Err(StoreError::new(
                ErrorCode::ReadOnlyStore,
                "read-only open requires an initialized database",
            ));
        }
        let pool = open_pool(&paths, true).await?;
        Ok(Self {
            paths,
            pool,
            writer: None,
            fault_plan: StoreFaultPlan::default(),
        })
    }

    #[must_use]
    pub fn paths(&self) -> &StorePaths {
        &self.paths
    }

    pub fn fence(&self) -> Result<HostFence, StoreError> {
        self.writer
            .as_ref()
            .map(|writer| writer.fence.clone())
            .ok_or_else(|| {
                StoreError::new(
                    ErrorCode::ReadOnlyStore,
                    "this store was opened without write authority",
                )
            })
    }

    /// Check an externally retained fence against the current durable epoch.
    pub async fn assert_current_fence(&self, fence: &HostFence) -> Result<(), StoreError> {
        let current = sqlx::query_scalar::<_, i64>(
            "SELECT host_epoch.generation
             FROM host_epoch
             JOIN hosts ON hosts.generation = host_epoch.generation
                         AND hosts.host_id = ?
             WHERE host_epoch.singleton = 1",
        )
        .bind(fence.host_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read host epoch", error))?;
        if current != Some(to_i64(fence.generation, "fence generation")?) {
            return Err(StoreError::new(
                ErrorCode::StaleWriter,
                "writer host or generation is no longer current",
            ));
        }
        Ok(())
    }

    pub async fn diagnostics(&self) -> Result<StoreDiagnostics, StoreError> {
        let foreign_keys = sqlx::query_scalar::<_, i64>("PRAGMA foreign_keys")
            .fetch_one(&self.pool)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageOpenFailed, "read foreign_keys", error)
            })?;
        let journal_mode = sqlx::query_scalar::<_, String>("PRAGMA journal_mode")
            .fetch_one(&self.pool)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageOpenFailed, "read journal_mode", error)
            })?;
        let synchronous = sqlx::query_scalar::<_, i64>("PRAGMA synchronous")
            .fetch_one(&self.pool)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageOpenFailed, "read synchronous", error)
            })?;
        let busy_timeout_ms = sqlx::query_scalar::<_, i64>("PRAGMA busy_timeout")
            .fetch_one(&self.pool)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageOpenFailed, "read busy_timeout", error)
            })?;
        Ok(StoreDiagnostics {
            foreign_keys_enabled: foreign_keys == 1,
            journal_mode,
            synchronous,
            busy_timeout_ms,
        })
    }

    /// Commit every durable record required to acknowledge a user input.
    #[allow(clippy::too_many_lines)]
    pub async fn commit_admission(
        &self,
        commit: AdmissionCommit,
    ) -> Result<AdmissionAck, StoreError> {
        validate_admission(&commit)?;
        let fence = self.fence()?;
        let mut transaction = self.begin_write(&fence).await?;

        let existing = sqlx::query(
            "SELECT session_id, task_id, input_hash, event_id, admitted_sequence
             FROM inbox WHERE input_id = ?",
        )
        .bind(commit.input_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "read idempotent inbox",
                error,
            )
        })?;
        if let Some(row) = existing {
            let session_id: String = row_get(&row, "session_id")?;
            let task_id: String = row_get(&row, "task_id")?;
            let input_hash: String = row_get(&row, "input_hash")?;
            if session_id == commit.session_id.as_str()
                && task_id == commit.task_id.as_str()
                && input_hash == commit.input_hash.as_str()
            {
                let event_id =
                    EventId::parse(row_get::<String>(&row, "event_id")?).map_err(|_| {
                        StoreError::new(ErrorCode::StorageWriteFailed, "stored event ID is invalid")
                    })?;
                let sequence = to_u64(
                    row_get::<i64>(&row, "admitted_sequence")?,
                    "admitted sequence",
                )?;
                transaction.rollback().await.map_err(|error| {
                    database_error(
                        ErrorCode::StorageWriteFailed,
                        "rollback idempotent input",
                        error,
                    )
                })?;
                return Ok(AdmissionAck {
                    input_id: commit.input_id,
                    event_id,
                    sequence,
                    idempotent_replay: true,
                });
            }
            return Err(StoreError::new(
                ErrorCode::IdempotencyConflict,
                "input ID was already admitted with different content or ownership",
            ));
        }

        ensure_session(
            &mut transaction,
            &commit.session_id,
            &commit.task_id,
            commit.expected_sequence,
        )
        .await?;
        claim_task(
            &mut transaction,
            &commit.task_id,
            &commit.session_id,
            &fence,
        )
        .await?;
        ensure_expected_sequence(
            &mut transaction,
            &commit.session_id,
            commit.expected_sequence,
        )
        .await?;

        let event_json = to_json(&commit.event, "serialize input event")?;
        let instruction_json = to_json(&commit.instruction, "serialize instruction")?;
        let state_json = to_json(&commit.working_state, "serialize working state")?;
        let marker_json = to_json(&commit.marker, "serialize source-work marker")?;
        let event_id = commit.event.event_id.clone();
        let sequence = commit.event.seq;

        sqlx::query(
            "INSERT INTO events(session_id, sequence, event_id, event_json, continuity_critical)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(commit.session_id.as_str())
        .bind(to_i64(sequence, "event sequence")?)
        .bind(event_id.as_str())
        .bind(event_json)
        .bind(i64::from(u8::from(commit.event.continuity_critical)))
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "insert input event", error)
        })?;
        sqlx::query(
            "INSERT INTO inbox(input_id, session_id, task_id, input_hash, raw_text, event_id, admitted_sequence)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(commit.input_id.as_str())
        .bind(commit.session_id.as_str())
        .bind(commit.task_id.as_str())
        .bind(commit.input_hash.as_str())
        .bind(&commit.raw_text)
        .bind(event_id.as_str())
        .bind(to_i64(sequence, "admitted sequence")?)
        .execute(&mut *transaction)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "insert inbox", error))?;
        sqlx::query(
            "INSERT INTO instruction_ledger(instruction_id, session_id, source_event_id, instruction_json)
             VALUES (?, ?, ?, ?)",
        )
        .bind(commit.instruction.instruction_id.as_str())
        .bind(commit.session_id.as_str())
        .bind(event_id.as_str())
        .bind(instruction_json)
        .execute(&mut *transaction)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "insert instruction ledger", error))?;
        upsert_projection(
            &mut transaction,
            &commit.task_id,
            &commit.session_id,
            &commit.working_state,
            state_json,
        )
        .await?;
        insert_marker(
            &mut transaction,
            &commit.session_id,
            &commit.task_id,
            &commit.marker,
            marker_json,
        )
        .await?;
        advance_sequence(
            &mut transaction,
            &commit.session_id,
            commit.expected_sequence,
        )
        .await?;

        self.inject(StoreFaultPoint::BeforeAdmissionCommit)?;
        transaction.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit input admission",
                error,
            )
        })?;
        Ok(AdmissionAck {
            input_id: commit.input_id,
            event_id,
            sequence,
            idempotent_replay: false,
        })
    }

    /// Commit immutable receipt evidence and the next projection revision.
    #[allow(clippy::too_many_lines)]
    pub async fn commit_receipt(&self, commit: ReceiptCommit) -> Result<ReceiptAck, StoreError> {
        validate_receipt(&commit)?;
        if let Some(artifact) = &commit.artifact {
            self.validate_published_artifact(artifact)?;
        }
        let fence = self.fence()?;
        let mut transaction = self.begin_write(&fence).await?;

        let receipt_json = to_json(&commit.receipt, "serialize receipt")?;
        let existing = sqlx::query(
            "SELECT session_id, task_id, event_id, sequence, receipt_json
             FROM receipts WHERE tool_execution_id = ?",
        )
        .bind(commit.receipt.tool_execution_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "read idempotent receipt",
                error,
            )
        })?;
        if let Some(row) = existing {
            let stored: String = row_get(&row, "receipt_json")?;
            let stored_session: String = row_get(&row, "session_id")?;
            let stored_task: String = row_get(&row, "task_id")?;
            if stored_session == commit.session_id.as_str()
                && stored_task == commit.task_id.as_str()
                && stored == receipt_json
            {
                let event_id =
                    EventId::parse(row_get::<String>(&row, "event_id")?).map_err(|_| {
                        StoreError::new(
                            ErrorCode::StorageWriteFailed,
                            "stored receipt event ID is invalid",
                        )
                    })?;
                let sequence = to_u64(row_get::<i64>(&row, "sequence")?, "receipt sequence")?;
                transaction.rollback().await.map_err(|error| {
                    database_error(
                        ErrorCode::StorageWriteFailed,
                        "rollback idempotent receipt",
                        error,
                    )
                })?;
                return Ok(ReceiptAck {
                    event_id,
                    sequence,
                    idempotent_replay: true,
                });
            }
            return Err(StoreError::new(
                ErrorCode::IdempotencyConflict,
                "tool execution ID was already committed with different evidence",
            ));
        }

        ensure_session_existing(&mut transaction, &commit.session_id, &commit.task_id).await?;
        claim_task(
            &mut transaction,
            &commit.task_id,
            &commit.session_id,
            &fence,
        )
        .await?;
        ensure_expected_sequence(
            &mut transaction,
            &commit.session_id,
            commit.expected_sequence,
        )
        .await?;

        if let Some(artifact) = &commit.artifact {
            sqlx::query(
                "INSERT INTO artifacts(artifact_id, content_hash, byte_len, relative_path)
                 VALUES (?, ?, ?, ?)
                 ON CONFLICT(artifact_id) DO NOTHING",
            )
            .bind(artifact.artifact_id.as_str())
            .bind(artifact.content_hash.as_str())
            .bind(to_i64(artifact.byte_len, "artifact byte length")?)
            .bind(&artifact.relative_path)
            .execute(&mut *transaction)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageWriteFailed, "record artifact", error)
            })?;
        }

        let event_json = to_json(&commit.event, "serialize receipt event")?;
        let state_json = to_json(&commit.working_state, "serialize receipt state")?;
        let marker_json = to_json(&commit.marker, "serialize receipt marker")?;
        let event_id = commit.event.event_id.clone();
        let sequence = commit.event.seq;
        sqlx::query(
            "INSERT INTO events(session_id, sequence, event_id, event_json, continuity_critical)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(commit.session_id.as_str())
        .bind(to_i64(sequence, "receipt event sequence")?)
        .bind(event_id.as_str())
        .bind(event_json)
        .bind(i64::from(u8::from(commit.event.continuity_critical)))
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "insert receipt event", error)
        })?;
        sqlx::query(
            "INSERT INTO receipts(tool_execution_id, session_id, task_id, event_id, sequence, receipt_json, artifact_id)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(commit.receipt.tool_execution_id.as_str())
        .bind(commit.session_id.as_str())
        .bind(commit.task_id.as_str())
        .bind(event_id.as_str())
        .bind(to_i64(sequence, "receipt sequence")?)
        .bind(receipt_json)
        .bind(commit.artifact.as_ref().map(|artifact| artifact.artifact_id.as_str()))
        .execute(&mut *transaction)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "insert receipt", error))?;
        upsert_projection(
            &mut transaction,
            &commit.task_id,
            &commit.session_id,
            &commit.working_state,
            state_json,
        )
        .await?;
        insert_marker(
            &mut transaction,
            &commit.session_id,
            &commit.task_id,
            &commit.marker,
            marker_json,
        )
        .await?;
        advance_sequence(
            &mut transaction,
            &commit.session_id,
            commit.expected_sequence,
        )
        .await?;

        self.inject(StoreFaultPoint::BeforeReceiptCommit)?;
        transaction.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit receipt", error)
        })?;
        Ok(ReceiptAck {
            event_id,
            sequence,
            idempotent_replay: false,
        })
    }

    /// Persist a validated snapshot after all covered journal records exist.
    pub async fn write_snapshot(&self, snapshot: SnapshotRecord) -> Result<(), StoreError> {
        let expected_hash =
            ContentHash::from_canonical_json(&snapshot.content).map_err(|error| {
                StoreError::new(
                    error.code(),
                    format!("snapshot content is not canonical: {error}"),
                )
            })?;
        if expected_hash != snapshot.content_hash {
            return Err(StoreError::new(
                ErrorCode::SnapshotCorrupt,
                "snapshot content hash does not match canonical content",
            ));
        }
        let fence = self.fence()?;
        let mut transaction = self.begin_write(&fence).await?;
        ensure_session_existing(&mut transaction, &snapshot.session_id, &snapshot.task_id).await?;
        let next_sequence = session_next_sequence(&mut transaction, &snapshot.session_id).await?;
        if snapshot.through_sequence >= next_sequence {
            return Err(StoreError::new(
                ErrorCode::SequenceConflict,
                "snapshot cannot cover an uncommitted event",
            ));
        }
        let snapshot_json = to_json(&snapshot.content, "serialize snapshot")?;
        sqlx::query(
            "INSERT INTO snapshots(snapshot_id, session_id, task_id, through_sequence, schema_version, snapshot_json, content_hash)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(snapshot.snapshot_id.as_str())
        .bind(snapshot.session_id.as_str())
        .bind(snapshot.task_id.as_str())
        .bind(to_i64(snapshot.through_sequence, "snapshot sequence")?)
        .bind(i64::from(snapshot.schema_version))
        .bind(snapshot_json)
        .bind(snapshot.content_hash.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "insert snapshot", error))?;
        self.inject(StoreFaultPoint::BeforeSnapshotCommit)?;
        transaction.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit snapshot", error)
        })?;
        Ok(())
    }

    /// Flush and publish an artifact before any database reference is accepted.
    pub fn publish_artifact(&self, bytes: &[u8]) -> Result<PublishedArtifact, StoreError> {
        self.fence()?;
        fs::create_dir_all(&self.paths.artifact_dir).map_err(|error| {
            StoreError::new(
                ErrorCode::ArtifactWriteFailed,
                format!("cannot create artifact directory: {error}"),
            )
        })?;
        let artifact_id = ArtifactId::generate();
        let relative_path = format!(
            "{}/{}.bin",
            crate::models::ARTIFACT_DIRECTORY_NAME,
            artifact_id
        );
        let final_path = self.paths.data_dir.join(&relative_path);
        let temporary_path = self
            .paths
            .artifact_dir
            .join(format!("{}.tmp", artifact_id.as_str()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
            .map_err(|error| {
                StoreError::new(
                    ErrorCode::ArtifactWriteFailed,
                    format!("cannot create artifact temporary file: {error}"),
                )
            })?;
        file.write_all(bytes).map_err(|error| {
            StoreError::new(
                ErrorCode::ArtifactWriteFailed,
                format!("cannot write artifact bytes: {error}"),
            )
        })?;
        file.sync_all().map_err(|error| {
            StoreError::new(
                ErrorCode::ArtifactWriteFailed,
                format!("cannot flush artifact bytes: {error}"),
            )
        })?;
        drop(file);
        fs::rename(&temporary_path, &final_path).map_err(|error| {
            StoreError::new(
                ErrorCode::ArtifactWriteFailed,
                format!("cannot publish artifact bytes: {error}"),
            )
        })?;
        Ok(PublishedArtifact {
            artifact_id,
            content_hash: ContentHash::from_bytes(bytes),
            byte_len: u64::try_from(bytes.len()).map_err(|_| {
                StoreError::new(ErrorCode::ArtifactWriteFailed, "artifact is too large")
            })?,
            relative_path,
        })
    }

    /// Persist implementation metadata for later inspection; this does not load
    /// or execute a plugin.
    pub async fn register_plugin_manifest(
        &self,
        manifest: PluginManifest,
    ) -> Result<(), StoreError> {
        manifest.validate().map_err(|error| {
            StoreError::new(error.code(), format!("plugin manifest is invalid: {error}"))
        })?;
        let fence = self.fence()?;
        let mut transaction = self.begin_write(&fence).await?;
        let serialized = to_json(&manifest, "serialize plugin manifest")?;
        sqlx::query(
            "INSERT INTO plugin_manifests(instance_id, generation, manifest_json)
             VALUES (?, ?, ?)
             ON CONFLICT(instance_id) DO UPDATE SET
                 generation = excluded.generation,
                 manifest_json = excluded.manifest_json",
        )
        .bind(manifest.instance_id.as_str())
        .bind(to_i64(fence.generation, "plugin generation")?)
        .bind(serialized)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "persist plugin manifest",
                error,
            )
        })?;
        transaction.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit plugin manifest",
                error,
            )
        })
    }

    pub async fn list_plugin_manifests(&self) -> Result<Vec<PersistedPluginManifest>, StoreError> {
        let rows = sqlx::query(
            "SELECT manifest_json, generation FROM plugin_manifests ORDER BY instance_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "list plugin manifests",
                error,
            )
        })?;
        rows.into_iter()
            .map(|row| {
                let serialized: String = row_get(&row, "manifest_json")?;
                let manifest: PluginManifest = serde_json::from_str(&serialized).map_err(|_| {
                    StoreError::new(
                        ErrorCode::StorageWriteFailed,
                        "stored plugin manifest is invalid",
                    )
                })?;
                manifest.validate().map_err(|error| {
                    StoreError::new(
                        error.code(),
                        format!("stored plugin manifest is invalid: {error}"),
                    )
                })?;
                Ok(PersistedPluginManifest {
                    manifest,
                    generation: to_u64(row_get::<i64>(&row, "generation")?, "plugin generation")?,
                })
            })
            .collect()
    }

    pub async fn plugin_manifest(
        &self,
        instance_id: &PluginInstanceId,
    ) -> Result<Option<PersistedPluginManifest>, StoreError> {
        let row = sqlx::query(
            "SELECT manifest_json, generation FROM plugin_manifests WHERE instance_id = ?",
        )
        .bind(instance_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "read plugin manifest", error)
        })?;
        row.map(|row| {
            let serialized: String = row_get(&row, "manifest_json")?;
            let manifest: PluginManifest = serde_json::from_str(&serialized).map_err(|_| {
                StoreError::new(
                    ErrorCode::StorageWriteFailed,
                    "stored plugin manifest is invalid",
                )
            })?;
            Ok(PersistedPluginManifest {
                manifest,
                generation: to_u64(row_get::<i64>(&row, "generation")?, "plugin generation")?,
            })
        })
        .transpose()
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionSummary>, StoreError> {
        let rows = sqlx::query(
            "SELECT sessions.session_id, sessions.task_id, sessions.next_sequence,
                     COUNT(DISTINCT inbox.input_id) AS input_count,
                    MAX(snapshots.through_sequence) AS snapshot_sequence
             FROM sessions
             LEFT JOIN inbox ON inbox.session_id = sessions.session_id
             LEFT JOIN snapshots ON snapshots.session_id = sessions.session_id
             GROUP BY sessions.session_id, sessions.task_id, sessions.next_sequence
             ORDER BY sessions.session_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "list sessions", error))?;
        rows.into_iter()
            .map(|row| {
                let session_id =
                    SessionId::parse(row_get::<String>(&row, "session_id")?).map_err(|_| {
                        StoreError::new(
                            ErrorCode::StorageWriteFailed,
                            "stored session ID is invalid",
                        )
                    })?;
                let task_id = TaskId::parse(row_get::<String>(&row, "task_id")?).map_err(|_| {
                    StoreError::new(ErrorCode::StorageWriteFailed, "stored task ID is invalid")
                })?;
                let next_sequence =
                    to_u64(row_get::<i64>(&row, "next_sequence")?, "next sequence")?;
                let input_count = to_u64(row_get::<i64>(&row, "input_count")?, "input count")?;
                let snapshot_sequence = row
                    .try_get::<Option<i64>, _>("snapshot_sequence")
                    .map_err(|_| {
                        StoreError::new(
                            ErrorCode::StorageWriteFailed,
                            "stored snapshot sequence is invalid",
                        )
                    })?
                    .map(|value| to_u64(value, "snapshot sequence"))
                    .transpose()?;
                Ok(SessionSummary {
                    session_id,
                    task_id,
                    next_sequence,
                    input_count,
                    latest_snapshot_sequence: snapshot_sequence,
                })
            })
            .collect()
    }

    pub async fn session_summary(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionSummary>, StoreError> {
        let summaries = self.list_sessions().await?;
        Ok(summaries
            .into_iter()
            .find(|summary| &summary.session_id == session_id))
    }

    pub async fn session_task(&self, session_id: &SessionId) -> Result<Option<TaskId>, StoreError> {
        let task_id =
            sqlx::query_scalar::<_, String>("SELECT task_id FROM sessions WHERE session_id = ?")
                .bind(session_id.as_str())
                .fetch_optional(&self.pool)
                .await
                .map_err(|error| {
                    database_error(ErrorCode::StorageWriteFailed, "read session task", error)
                })?;
        task_id
            .map(|value| {
                TaskId::parse(value).map_err(|_| {
                    StoreError::new(ErrorCode::StorageWriteFailed, "stored task ID is invalid")
                })
            })
            .transpose()
    }

    pub async fn load_events_after(
        &self,
        session_id: &SessionId,
        through_sequence: u64,
    ) -> Result<Vec<EventEnvelope>, StoreError> {
        let rows = sqlx::query(
            "SELECT event_json FROM events WHERE session_id = ? AND sequence > ? ORDER BY sequence",
        )
        .bind(session_id.as_str())
        .bind(to_i64(through_sequence, "tail sequence")?)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "read journal tail", error)
        })?;
        rows.into_iter()
            .map(|row| {
                let serialized: String = row_get(&row, "event_json")?;
                EventEnvelope::parse_json(&serialized).map_err(|error| {
                    StoreError::new(error.code(), format!("stored event is invalid: {error}"))
                })
            })
            .collect()
    }

    pub async fn latest_snapshot(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SnapshotRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT snapshot_id, session_id, task_id, through_sequence, schema_version, snapshot_json, content_hash
             FROM snapshots WHERE session_id = ? ORDER BY through_sequence DESC LIMIT 1",
        )
        .bind(session_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read latest snapshot", error))?;
        row.map(|row| snapshot_from_row(&row)).transpose()
    }

    pub async fn load_receipts(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<harness_types::ToolExecutionReceipt>, StoreError> {
        let rows =
            sqlx::query("SELECT receipt_json FROM receipts WHERE session_id = ? ORDER BY sequence")
                .bind(session_id.as_str())
                .fetch_all(&self.pool)
                .await
                .map_err(|error| {
                    database_error(ErrorCode::StorageWriteFailed, "read receipts", error)
                })?;
        rows.into_iter()
            .map(|row| {
                let serialized: String = row_get(&row, "receipt_json")?;
                let receipt: harness_types::ToolExecutionReceipt =
                    serde_json::from_str(&serialized).map_err(|_| {
                        StoreError::new(ErrorCode::StorageWriteFailed, "stored receipt is invalid")
                    })?;
                receipt.validate().map_err(|error| {
                    StoreError::new(error.code(), format!("stored receipt is invalid: {error}"))
                })?;
                Ok(receipt)
            })
            .collect()
    }

    pub async fn current_projection(
        &self,
        task_id: &TaskId,
    ) -> Result<Option<WorkingState>, StoreError> {
        let row = sqlx::query("SELECT working_state_json FROM task_projections WHERE task_id = ?")
            .bind(task_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageWriteFailed, "read task projection", error)
            })?;
        row.map(|row| {
            let serialized: String = row_get(&row, "working_state_json")?;
            let state: WorkingState = serde_json::from_str(&serialized).map_err(|_| {
                StoreError::new(
                    ErrorCode::StorageWriteFailed,
                    "stored working state is invalid",
                )
            })?;
            state.validate().map_err(|error| {
                StoreError::new(
                    error.code(),
                    format!("stored working state is invalid: {error}"),
                )
            })?;
            Ok(state)
        })
        .transpose()
    }

    pub async fn inbox_count(&self, session_id: &SessionId) -> Result<u64, StoreError> {
        let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM inbox WHERE session_id = ?")
            .bind(session_id.as_str())
            .fetch_one(&self.pool)
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "count inbox", error))?;
        to_u64(count, "inbox count")
    }

    /// Test support performs a real database corruption after a valid snapshot
    /// has been committed, so recovery must exercise its fallback path.
    pub async fn corrupt_latest_snapshot_for_test(
        &self,
        session_id: &SessionId,
    ) -> Result<(), StoreError> {
        let fence = self.fence()?;
        let mut transaction = self.begin_write(&fence).await?;
        let result = sqlx::query(
            "UPDATE snapshots SET snapshot_json = '{corrupt' WHERE snapshot_id = (
                SELECT snapshot_id FROM snapshots WHERE session_id = ?
                ORDER BY through_sequence DESC LIMIT 1
             )",
        )
        .bind(session_id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "corrupt snapshot fixture",
                error,
            )
        })?;
        if result.rows_affected() != 1 {
            return Err(StoreError::new(
                ErrorCode::SnapshotCorrupt,
                "fixture requires an existing snapshot",
            ));
        }
        transaction.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit snapshot corruption fixture",
                error,
            )
        })
    }

    /// Test support appends a real durable event with no projection update.
    pub async fn append_event_for_test(&self, event: EventEnvelope) -> Result<(), StoreError> {
        event.validate().map_err(|error| {
            StoreError::new(error.code(), format!("fixture event is invalid: {error}"))
        })?;
        let fence = self.fence()?;
        let mut transaction = self.begin_write(&fence).await?;
        let task_id = session_task_in_tx(&mut transaction, &event.session_id)
            .await?
            .ok_or_else(|| {
                StoreError::new(ErrorCode::InvalidPayload, "fixture session does not exist")
            })?;
        ensure_expected_sequence(&mut transaction, &event.session_id, event.seq).await?;
        let serialized = to_json(&event, "serialize fixture event")?;
        sqlx::query(
            "INSERT INTO events(session_id, sequence, event_id, event_json, continuity_critical)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(event.session_id.as_str())
        .bind(to_i64(event.seq, "fixture event sequence")?)
        .bind(event.event_id.as_str())
        .bind(serialized)
        .bind(i64::from(u8::from(event.continuity_critical)))
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "insert fixture event", error)
        })?;
        let marker_json = to_json(
            &serde_json::json!({"fixture": true, "task_id": task_id.as_str()}),
            "serialize fixture marker",
        )?;
        sqlx::query(
            "INSERT INTO source_work_markers(marker_id, session_id, task_id, event_id, sequence, marker_json)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(format!("fixture-{}", event.event_id.as_str()))
        .bind(event.session_id.as_str())
        .bind(task_id.as_str())
        .bind(event.event_id.as_str())
        .bind(to_i64(event.seq, "fixture marker sequence")?)
        .bind(marker_json)
        .execute(&mut *transaction)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "insert fixture marker", error))?;
        advance_sequence(&mut transaction, &event.session_id, event.seq).await?;
        transaction.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit fixture event", error)
        })
    }

    /// Low-level event append used by runtime continuity fixtures. It retains
    /// the same fence and sequence checks as the P1 test helper.
    pub async fn append_event(&self, event: EventEnvelope) -> Result<(), StoreError> {
        self.append_event_for_test(event).await
    }

    /// Return the next unallocated journal sequence for a session.
    pub async fn next_sequence(&self, session_id: &SessionId) -> Result<u64, StoreError> {
        let value =
            sqlx::query_scalar::<_, i64>("SELECT next_sequence FROM sessions WHERE session_id = ?")
                .bind(session_id.as_str())
                .fetch_optional(&self.pool)
                .await
                .map_err(|error| {
                    database_error(ErrorCode::StorageWriteFailed, "read next sequence", error)
                })?;
        value
            .map(|value| to_u64(value, "next sequence"))
            .transpose()?
            .ok_or_else(|| StoreError::new(ErrorCode::InvalidPayload, "session does not exist"))
    }

    /// Commit an event, projection and source marker as one durable unit.
    pub async fn commit_projected_event(
        &self,
        event: EventEnvelope,
        task_id: &TaskId,
        state: WorkingState,
        marker: SourceWorkMarker,
    ) -> Result<(), StoreError> {
        event.validate().map_err(|error| {
            StoreError::new(error.code(), format!("runtime event is invalid: {error}"))
        })?;
        state.validate().map_err(|error| {
            StoreError::new(
                error.code(),
                format!("runtime projection is invalid: {error}"),
            )
        })?;
        if event.session_id != state.session_id
            || event.seq != state.through_event_seq
            || state.task_id != *task_id
        {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                "runtime event and projection identity do not match",
            ));
        }
        let fence = self.fence()?;
        let mut transaction = self.begin_write(&fence).await?;
        ensure_session_existing(&mut transaction, &event.session_id, task_id).await?;
        claim_task(&mut transaction, task_id, &event.session_id, &fence).await?;
        ensure_expected_sequence(&mut transaction, &event.session_id, event.seq).await?;
        sqlx::query("INSERT INTO events(session_id, sequence, event_id, event_json, continuity_critical) VALUES (?, ?, ?, ?, ?)")
            .bind(event.session_id.as_str()).bind(to_i64(event.seq, "runtime event sequence")?).bind(event.event_id.as_str())
            .bind(to_json(&event, "serialize runtime event")?).bind(i64::from(u8::from(event.continuity_critical)))
            .execute(&mut *transaction).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "insert runtime event", error))?;
        upsert_projection(
            &mut transaction,
            task_id,
            &event.session_id,
            &state,
            to_json(&state, "serialize runtime projection")?,
        )
        .await?;
        insert_marker(
            &mut transaction,
            &event.session_id,
            task_id,
            &marker,
            to_json(&marker, "serialize runtime marker")?,
        )
        .await?;
        advance_sequence(&mut transaction, &event.session_id, event.seq).await?;
        transaction.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit runtime event", error)
        })
    }

    pub async fn persist_composition_snapshot(
        &self,
        record: CompositionSnapshotRecord,
    ) -> Result<(), StoreError> {
        validate_hashed_json(&record.content, &record.content_hash)?;
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let json = to_json(&record.content, "serialize composition snapshot")?;
        let result = sqlx::query("INSERT INTO composition_snapshots(snapshot_id, session_id, task_id, revision, content_json, content_hash) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(snapshot_id) DO NOTHING")
            .bind(record.snapshot_id.as_str()).bind(record.session_id.as_str()).bind(record.task_id.as_str()).bind(to_i64(record.revision, "composition revision")?).bind(json).bind(record.content_hash.as_str())
            .execute(&mut *tx).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "persist composition snapshot", error))?;
        if result.rows_affected() == 0 {
            let existing = sqlx::query_scalar::<_, String>(
                "SELECT content_hash FROM composition_snapshots WHERE snapshot_id = ?",
            )
            .bind(record.snapshot_id.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| {
                database_error(
                    ErrorCode::StorageWriteFailed,
                    "read composition idempotency",
                    error,
                )
            })?;
            if existing != record.content_hash.as_str() {
                return Err(StoreError::new(
                    ErrorCode::IdempotencyConflict,
                    "composition snapshot ID is immutable",
                ));
            }
        }
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit composition snapshot",
                error,
            )
        })
    }

    pub async fn latest_composition_snapshot(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<CompositionSnapshotRecord>, StoreError> {
        let row = sqlx::query("SELECT snapshot_id, session_id, task_id, revision, content_json, content_hash FROM composition_snapshots WHERE session_id = ? ORDER BY revision DESC LIMIT 1").bind(session_id.as_str()).fetch_optional(&self.pool).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read composition snapshot", error))?;
        row.map(|row| composition_from_row(&row)).transpose()
    }

    pub async fn persist_context_packet(
        &self,
        record: ContextPacketRecord,
    ) -> Result<(), StoreError> {
        record.packet.validate().map_err(|error| {
            StoreError::new(error.code(), format!("context packet is invalid: {error}"))
        })?;
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let result = sqlx::query("INSERT INTO context_packets(packet_id, session_id, task_id, packet_json, composition_snapshot_id, omitted_json, degradation) VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT(packet_id) DO NOTHING")
            .bind(record.packet.packet_id.as_str()).bind(record.packet.session_id.as_str()).bind(record.packet.task_id.as_str()).bind(to_json(&record.packet, "serialize context packet")?)
            .bind(record.composition_snapshot_id.as_ref().map(harness_types::CompositionSnapshotId::as_str)).bind(to_json(&record.omitted_optional, "serialize omitted context")?).bind(record.degradation.as_deref())
            .execute(&mut *tx).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "persist context packet", error))?;
        if result.rows_affected() == 0 {
            let existing = sqlx::query_scalar::<_, String>(
                "SELECT packet_json FROM context_packets WHERE packet_id = ?",
            )
            .bind(record.packet.packet_id.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| {
                database_error(
                    ErrorCode::StorageWriteFailed,
                    "read packet idempotency",
                    error,
                )
            })?;
            if existing != to_json(&record.packet, "serialize context packet")? {
                return Err(StoreError::new(
                    ErrorCode::IdempotencyConflict,
                    "context packet ID is immutable",
                ));
            }
        }
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit context packet",
                error,
            )
        })
    }

    pub async fn list_context_packets(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<ContextPacketRecord>, StoreError> {
        let rows = sqlx::query("SELECT packet_json, composition_snapshot_id, omitted_json, degradation FROM context_packets WHERE session_id = ? ORDER BY rowid").bind(session_id.as_str()).fetch_all(&self.pool).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "list context packets", error))?;
        rows.into_iter()
            .map(|row| context_packet_from_row(&row))
            .collect()
    }

    pub async fn latest_context_packet(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<ContextPacketRecord>, StoreError> {
        let row = sqlx::query("SELECT packet_json, composition_snapshot_id, omitted_json, degradation FROM context_packets WHERE session_id = ? ORDER BY rowid DESC LIMIT 1").bind(session_id.as_str()).fetch_optional(&self.pool).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "latest context packet", error))?;
        row.map(|row| context_packet_from_row(&row)).transpose()
    }

    pub async fn persist_frozen_request(
        &self,
        record: FrozenRequestRecord,
    ) -> Result<(), StoreError> {
        validate_hashed_json(&record.request_json, &record.content_hash)?;
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let result = sqlx::query("INSERT INTO frozen_requests(request_id, packet_id, composition_snapshot_id, session_id, task_id, request_json, content_hash, provider_id, model, config_revision) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(request_id) DO NOTHING")
            .bind(record.request_id.as_str()).bind(record.packet_id.as_str()).bind(record.composition_snapshot_id.as_ref().map(harness_types::CompositionSnapshotId::as_str)).bind(record.session_id.as_str()).bind(record.task_id.as_str())
            .bind(to_json(&record.request_json, "serialize frozen request")?).bind(record.content_hash.as_str()).bind(&record.provider_id).bind(&record.model).bind(to_i64(record.config_revision, "config revision")?)
            .execute(&mut *tx).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "persist frozen request", error))?;
        if result.rows_affected() == 0 {
            let existing = sqlx::query_scalar::<_, String>(
                "SELECT content_hash FROM frozen_requests WHERE request_id = ?",
            )
            .bind(record.request_id.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| {
                database_error(
                    ErrorCode::StorageWriteFailed,
                    "read frozen idempotency",
                    error,
                )
            })?;
            if existing != record.content_hash.as_str() {
                return Err(StoreError::new(
                    ErrorCode::IdempotencyConflict,
                    "frozen request ID is immutable",
                ));
            }
        }
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit frozen request",
                error,
            )
        })
    }

    pub async fn list_frozen_requests(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<FrozenRequestRecord>, StoreError> {
        let rows = sqlx::query("SELECT request_id, packet_id, composition_snapshot_id, session_id, task_id, request_json, content_hash, provider_id, model, config_revision FROM frozen_requests WHERE session_id = ? ORDER BY rowid").bind(session_id.as_str()).fetch_all(&self.pool).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "list frozen requests", error))?;
        rows.into_iter()
            .map(|row| frozen_request_from_row(&row))
            .collect()
    }

    pub async fn persist_provider_attempt(
        &self,
        record: ProviderAttemptRecord,
    ) -> Result<(), StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        sqlx::query("INSERT INTO provider_attempts(attempt_id, request_id, session_id, task_id, attempt_number, state, events_json, response_hash, error) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(attempt_id) DO NOTHING")
            .bind(record.attempt_id.as_str()).bind(record.request_id.as_str()).bind(record.session_id.as_str()).bind(record.task_id.as_str()).bind(i64::from(record.attempt_number)).bind(&record.state).bind(to_json(&record.events, "serialize provider attempt")?).bind(record.response_hash.as_ref().map(harness_types::ContentHash::as_str)).bind(record.error.as_deref())
            .execute(&mut *tx).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "persist provider attempt", error))?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit provider attempt",
                error,
            )
        })
    }

    pub async fn list_provider_attempts(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<ProviderAttemptRecord>, StoreError> {
        let rows = sqlx::query("SELECT attempt_id, request_id, session_id, task_id, attempt_number, state, events_json, response_hash, error FROM provider_attempts WHERE session_id = ? ORDER BY attempt_number").bind(session_id.as_str()).fetch_all(&self.pool).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "list provider attempts", error))?;
        rows.into_iter()
            .map(|row| provider_attempt_from_row(&row))
            .collect()
    }

    pub async fn write_context_checkpoint_cas(
        &self,
        record: ContextCheckpointRecord,
        expected_last_sequence: u64,
    ) -> Result<(), StoreError> {
        validate_hashed_json(&record.content, &record.content_hash)?;
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let actual = session_next_sequence(&mut tx, &record.session_id)
            .await?
            .checked_sub(1)
            .ok_or_else(|| {
                StoreError::new(ErrorCode::CompactionConflict, "session sequence is invalid")
            })?;
        if actual != expected_last_sequence {
            return Err(StoreError::new(
                ErrorCode::CompactionConflict,
                format!("context checkpoint CAS expected {expected_last_sequence}, found {actual}"),
            ));
        }
        let prior = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT MAX(revision) FROM context_checkpoints WHERE session_id = ?",
        )
        .bind(record.session_id.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "read checkpoint revision",
                error,
            )
        })?;
        if prior.is_some_and(|value| record.revision <= u64::try_from(value).unwrap_or(u64::MAX)) {
            return Err(StoreError::new(
                ErrorCode::CompactionConflict,
                "context checkpoint revision is stale",
            ));
        }
        sqlx::query("INSERT INTO context_checkpoints(checkpoint_id, session_id, task_id, through_sequence, revision, content_json, content_hash) VALUES (?, ?, ?, ?, ?, ?, ?)")
            .bind(&record.checkpoint_id).bind(record.session_id.as_str()).bind(record.task_id.as_str()).bind(to_i64(record.through_sequence, "checkpoint sequence")?).bind(to_i64(record.revision, "checkpoint revision")?).bind(to_json(&record.content, "serialize checkpoint")?).bind(record.content_hash.as_str())
            .execute(&mut *tx).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "write context checkpoint", error))?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit context checkpoint",
                error,
            )
        })
    }

    pub async fn latest_context_checkpoint(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<ContextCheckpointRecord>, StoreError> {
        let row = sqlx::query("SELECT checkpoint_id, session_id, task_id, through_sequence, revision, content_json, content_hash FROM context_checkpoints WHERE session_id = ? ORDER BY revision DESC LIMIT 1").bind(session_id.as_str()).fetch_optional(&self.pool).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "latest context checkpoint", error))?;
        row.map(|row| checkpoint_from_row(&row)).transpose()
    }

    pub async fn enqueue_runtime_command(
        &self,
        record: RuntimeCommandRecord,
    ) -> Result<bool, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let payload = to_json(&record.payload, "serialize runtime command")?;
        let result = sqlx::query("INSERT INTO runtime_commands(command_id, session_id, task_id, state, attempts, owner_generation, payload_json, last_error) VALUES (?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(command_id) DO NOTHING")
            .bind(record.command_id.as_str()).bind(record.session_id.as_str()).bind(record.task_id.as_str()).bind(record.state.as_str()).bind(i64::from(record.attempts)).bind(to_i64(record.owner_generation, "command generation")?).bind(payload).bind(record.last_error.as_deref())
            .execute(&mut *tx).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "enqueue runtime command", error))?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit runtime command",
                error,
            )
        })?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn runtime_command(
        &self,
        command_id: &RuntimeCommandId,
    ) -> Result<Option<RuntimeCommandRecord>, StoreError> {
        let row = sqlx::query("SELECT command_id, session_id, task_id, state, attempts, owner_generation, payload_json, last_error FROM runtime_commands WHERE command_id = ?").bind(command_id.as_str()).fetch_optional(&self.pool).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read runtime command", error))?;
        row.map(|row| runtime_command_from_row(&row)).transpose()
    }

    pub async fn claim_runtime_command(
        &self,
        command_id: &RuntimeCommandId,
        max_attempts: u32,
    ) -> Result<RuntimeCommandRecord, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let mut record = sqlx::query("SELECT command_id, session_id, task_id, state, attempts, owner_generation, payload_json, last_error FROM runtime_commands WHERE command_id = ?").bind(command_id.as_str()).fetch_optional(&mut *tx).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "claim runtime command", error))?.ok_or_else(|| StoreError::new(ErrorCode::InvalidPayload, "runtime command does not exist")).and_then(|row| runtime_command_from_row(&row))?;
        if record.state == RuntimeCommandState::Completed
            || record.state == RuntimeCommandState::Canceled
        {
            tx.rollback().await.ok();
            return Ok(record);
        }
        if record.attempts >= max_attempts {
            tx.rollback().await.ok();
            return Err(StoreError::new(
                ErrorCode::RetryExhausted,
                "runtime command retry limit reached",
            ));
        }
        record.attempts = record.attempts.saturating_add(1);
        record.state = RuntimeCommandState::Claimed;
        record.owner_generation = fence.generation;
        sqlx::query("UPDATE runtime_commands SET state = ?, attempts = ?, owner_generation = ?, last_error = NULL WHERE command_id = ?")
            .bind(record.state.as_str()).bind(i64::from(record.attempts)).bind(to_i64(record.owner_generation, "command generation")?).bind(command_id.as_str()).execute(&mut *tx).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "update runtime command claim", error))?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit runtime command claim",
                error,
            )
        })?;
        Ok(record)
    }

    pub async fn complete_runtime_command(
        &self,
        command_id: &RuntimeCommandId,
        owner_generation: u64,
        state: RuntimeCommandState,
        error: Option<&str>,
    ) -> Result<(), StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        let result = sqlx::query("UPDATE runtime_commands SET state = ?, last_error = ? WHERE command_id = ? AND owner_generation = ?")
            .bind(state.as_str()).bind(error).bind(command_id.as_str()).bind(to_i64(owner_generation, "command generation")?).execute(&mut *tx).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "complete runtime command", error))?;
        if result.rows_affected() != 1 {
            return Err(StoreError::new(
                ErrorCode::RuntimeCommandConflict,
                "runtime command owner is stale",
            ));
        }
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit runtime command completion",
                error,
            )
        })
    }

    pub async fn record_agent_state(&self, record: AgentStateRecord) -> Result<(), StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        sqlx::query("INSERT INTO agent_states(agent_run_id, session_id, task_id, state, generation, revision, detail_json) VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT(agent_run_id) DO UPDATE SET state=excluded.state, generation=excluded.generation, revision=excluded.revision, detail_json=excluded.detail_json")
            .bind(record.agent_run_id.as_str()).bind(record.session_id.as_str()).bind(record.task_id.as_str()).bind(&record.state).bind(to_i64(record.generation, "agent generation")?).bind(to_i64(record.revision, "agent revision")?).bind(to_json(&record.detail, "serialize agent state")?)
            .execute(&mut *tx).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "record agent state", error))?;
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit agent state", error)
        })
    }

    pub async fn agent_state(
        &self,
        agent_run_id: &AgentRunId,
    ) -> Result<Option<AgentStateRecord>, StoreError> {
        let row = sqlx::query("SELECT agent_run_id, session_id, task_id, state, generation, revision, detail_json FROM agent_states WHERE agent_run_id = ?")
            .bind(agent_run_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read agent state", error))?;
        row.map(|row| {
            Ok(AgentStateRecord {
                agent_run_id: AgentRunId::parse(row_get::<String>(&row, "agent_run_id")?).map_err(
                    |_| StoreError::new(ErrorCode::StorageWriteFailed, "agent run ID is invalid"),
                )?,
                session_id: parse_session(row_get::<String>(&row, "session_id")?)?,
                task_id: parse_task(row_get::<String>(&row, "task_id")?)?,
                state: row_get::<String>(&row, "state")?,
                generation: to_u64(row_get::<i64>(&row, "generation")?, "agent generation")?,
                revision: to_u64(row_get::<i64>(&row, "revision")?, "agent revision")?,
                detail: serde_json::from_str(&row_get::<String>(&row, "detail_json")?).map_err(
                    |_| StoreError::new(ErrorCode::StorageWriteFailed, "agent detail is invalid"),
                )?,
            })
        })
        .transpose()
    }

    pub async fn record_continuation_link(
        &self,
        source_session_id: &SessionId,
        new_session_id: &SessionId,
        task_id: &TaskId,
    ) -> Result<(), StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        sqlx::query("INSERT INTO session_lineage(source_session_id, new_session_id, task_id) VALUES (?, ?, ?) ON CONFLICT(new_session_id) DO UPDATE SET source_session_id=excluded.source_session_id, task_id=excluded.task_id")
            .bind(source_session_id.as_str()).bind(new_session_id.as_str()).bind(task_id.as_str()).execute(&mut *tx).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "record continuation link", error))?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit continuation link",
                error,
            )
        })
    }

    pub async fn continuation_link(
        &self,
        new_session_id: &SessionId,
    ) -> Result<Option<ContinuationLinkRecord>, StoreError> {
        let row = sqlx::query("SELECT source_session_id, new_session_id, task_id FROM session_lineage WHERE new_session_id = ?").bind(new_session_id.as_str()).fetch_optional(&self.pool).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read continuation link", error))?;
        row.map(|row| {
            Ok(ContinuationLinkRecord {
                source_session_id: parse_session(row_get(&row, "source_session_id")?)?,
                new_session_id: parse_session(row_get(&row, "new_session_id")?)?,
                task_id: parse_task(row_get(&row, "task_id")?)?,
            })
        })
        .transpose()
    }

    /// Close the connection pool explicitly. The lock is released only after
    /// the pool is closed, allowing the kernel to make storage its last phase.
    pub async fn close(mut self) -> Result<(), StoreError> {
        self.pool.close().await;
        if let Some(writer) = self.writer.take() {
            FileExt::unlock(&writer.lock_file).map_err(|error| {
                StoreError::new(
                    ErrorCode::StorageWriteFailed,
                    format!("cannot release writer lock: {error}"),
                )
            })?;
        }
        Ok(())
    }

    async fn begin_write(&self, fence: &HostFence) -> Result<Transaction<'_, Sqlite>, StoreError> {
        let mut transaction = self.pool.begin().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "begin write transaction",
                error,
            )
        })?;
        assert_fence_in_tx(&mut transaction, fence).await?;
        Ok(transaction)
    }

    fn inject(&self, point: StoreFaultPoint) -> Result<(), StoreError> {
        if self.fault_plan.consume(point) {
            return Err(StoreError::new(
                ErrorCode::StorageWriteFailed,
                format!("injected durable storage failure at {point:?}"),
            ));
        }
        Ok(())
    }

    fn validate_published_artifact(&self, artifact: &PublishedArtifact) -> Result<(), StoreError> {
        let expected_relative_path = format!(
            "{}/{}.bin",
            crate::models::ARTIFACT_DIRECTORY_NAME,
            artifact.artifact_id
        );
        if artifact.relative_path != expected_relative_path {
            return Err(StoreError::new(
                ErrorCode::ArtifactWriteFailed,
                "artifact reference path does not match its stable ID",
            ));
        }
        let path = self.paths.data_dir.join(&artifact.relative_path);
        let metadata = fs::metadata(&path).map_err(|error| {
            StoreError::new(
                ErrorCode::ArtifactWriteFailed,
                format!("referenced artifact is not published: {error}"),
            )
        })?;
        if !metadata.is_file() || metadata.len() != artifact.byte_len {
            return Err(StoreError::new(
                ErrorCode::ArtifactWriteFailed,
                "artifact metadata does not match the published file",
            ));
        }
        let bytes = fs::read(&path).map_err(|error| {
            StoreError::new(
                ErrorCode::ArtifactWriteFailed,
                format!("cannot verify published artifact: {error}"),
            )
        })?;
        if ContentHash::from_bytes(&bytes) != artifact.content_hash {
            return Err(StoreError::new(
                ErrorCode::ArtifactWriteFailed,
                "artifact content hash does not match the published file",
            ));
        }
        Ok(())
    }
}

async fn open_pool(paths: &StorePaths, read_only: bool) -> Result<SqlitePool, StoreError> {
    let mut options = SqliteConnectOptions::new()
        .filename(&paths.database_path)
        .read_only(read_only)
        .create_if_missing(!read_only)
        .foreign_keys(true)
        .busy_timeout(BUSY_TIMEOUT);
    if !read_only {
        options = options
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full);
    }
    SqlitePoolOptions::new()
        .max_connections(1)
        .min_connections(1)
        .connect_with(options)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageOpenFailed, "open SQLite database", error)
        })
}

async fn run_migrations(pool: &SqlitePool, fault_plan: &StoreFaultPlan) -> Result<(), StoreError> {
    let mut transaction = pool.begin().await.map_err(|error| {
        database_error(
            ErrorCode::MigrationFailed,
            "begin migration transaction",
            error,
        )
    })?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        )",
    )
    .execute(&mut *transaction)
    .await
    .map_err(|error| database_error(ErrorCode::MigrationFailed, "create migration table", error))?;
    let current =
        sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(version) FROM schema_migrations")
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| {
                database_error(ErrorCode::MigrationFailed, "read migration version", error)
            })?
            .unwrap_or(0);
    if current > STORE_SCHEMA_VERSION {
        return Err(StoreError::new(
            ErrorCode::MigrationFailed,
            "database schema is newer than this host supports",
        ));
    }
    if current < STORE_SCHEMA_VERSION {
        for statement in MIGRATION_1 {
            sqlx::query(*statement)
                .execute(&mut *transaction)
                .await
                .map_err(|error| {
                    database_error(ErrorCode::MigrationFailed, "apply migration 1", error)
                })?;
        }
        sqlx::query("INSERT INTO schema_migrations(version) VALUES (?)")
            .bind(STORE_SCHEMA_VERSION)
            .execute(&mut *transaction)
            .await
            .map_err(|error| {
                database_error(ErrorCode::MigrationFailed, "record migration 1", error)
            })?;
    }
    if fault_plan.consume(StoreFaultPoint::BeforeMigrationCommit) {
        return Err(StoreError::new(
            ErrorCode::MigrationFailed,
            "injected failure before migration commit",
        ));
    }
    transaction.commit().await.map_err(|error| {
        database_error(
            ErrorCode::MigrationFailed,
            "commit migration transaction",
            error,
        )
    })
}

impl SqliteStore {
    /// Persist the next retry attempt while retaining the same fenced owner.
    pub async fn bump_runtime_command_attempt(
        &self,
        command_id: &RuntimeCommandId,
        owner_generation: u64,
        max_attempts: u32,
    ) -> Result<u32, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        let attempts = sqlx::query_scalar::<_, i64>(
            "SELECT attempts FROM runtime_commands WHERE command_id = ? AND owner_generation = ?",
        )
        .bind(command_id.as_str())
        .bind(to_i64(owner_generation, "command generation")?)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "read retry attempt", error)
        })?
        .ok_or_else(|| {
            StoreError::new(
                ErrorCode::RuntimeCommandConflict,
                "runtime command owner is stale",
            )
        })?;
        let attempts = u32::try_from(to_u64(attempts, "command attempts")?).map_err(|_| {
            StoreError::new(
                ErrorCode::StorageWriteFailed,
                "command attempts exceed range",
            )
        })?;
        if attempts >= max_attempts {
            return Err(StoreError::new(
                ErrorCode::RetryExhausted,
                "runtime command retry limit reached",
            ));
        }
        let next = attempts.saturating_add(1);
        sqlx::query("UPDATE runtime_commands SET attempts = ?, state = 'claimed' WHERE command_id = ? AND owner_generation = ?")
            .bind(i64::from(next))
            .bind(command_id.as_str())
            .bind(to_i64(owner_generation, "command generation")?)
            .execute(&mut *tx)
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "persist retry attempt", error))?;
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit retry attempt", error)
        })?;
        Ok(next)
    }
}

async fn ensure_runtime_schema(pool: &SqlitePool) -> Result<(), StoreError> {
    let mut tx = pool.begin().await.map_err(|error| {
        database_error(ErrorCode::MigrationFailed, "begin runtime migration", error)
    })?;
    let statements = [
        "CREATE TABLE IF NOT EXISTS runtime_schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP)",
        "CREATE TABLE IF NOT EXISTS runtime_commands (command_id TEXT PRIMARY KEY, session_id TEXT NOT NULL, task_id TEXT NOT NULL, state TEXT NOT NULL, attempts INTEGER NOT NULL, owner_generation INTEGER NOT NULL, payload_json TEXT NOT NULL, last_error TEXT)",
        "CREATE TABLE IF NOT EXISTS agent_states (agent_run_id TEXT PRIMARY KEY, session_id TEXT NOT NULL, task_id TEXT NOT NULL, state TEXT NOT NULL, generation INTEGER NOT NULL, revision INTEGER NOT NULL, detail_json TEXT NOT NULL)",
        "CREATE TABLE IF NOT EXISTS composition_snapshots (snapshot_id TEXT PRIMARY KEY, session_id TEXT NOT NULL, task_id TEXT NOT NULL, revision INTEGER NOT NULL, content_json TEXT NOT NULL, content_hash TEXT NOT NULL)",
        "CREATE TABLE IF NOT EXISTS context_checkpoints (checkpoint_id TEXT PRIMARY KEY, session_id TEXT NOT NULL, task_id TEXT NOT NULL, through_sequence INTEGER NOT NULL, revision INTEGER NOT NULL, content_json TEXT NOT NULL, content_hash TEXT NOT NULL)",
        "CREATE TABLE IF NOT EXISTS context_packets (packet_id TEXT PRIMARY KEY, session_id TEXT NOT NULL, task_id TEXT NOT NULL, packet_json TEXT NOT NULL, composition_snapshot_id TEXT, omitted_json TEXT NOT NULL, degradation TEXT)",
        "CREATE TABLE IF NOT EXISTS frozen_requests (request_id TEXT PRIMARY KEY, packet_id TEXT NOT NULL, composition_snapshot_id TEXT, session_id TEXT NOT NULL, task_id TEXT NOT NULL, request_json TEXT NOT NULL, content_hash TEXT NOT NULL, provider_id TEXT NOT NULL, model TEXT NOT NULL, config_revision INTEGER NOT NULL)",
        "CREATE TABLE IF NOT EXISTS provider_attempts (attempt_id TEXT PRIMARY KEY, request_id TEXT NOT NULL, session_id TEXT NOT NULL, task_id TEXT NOT NULL, attempt_number INTEGER NOT NULL, state TEXT NOT NULL, events_json TEXT NOT NULL, response_hash TEXT, error TEXT)",
        "CREATE TABLE IF NOT EXISTS session_lineage (new_session_id TEXT PRIMARY KEY, source_session_id TEXT NOT NULL, task_id TEXT NOT NULL)",
    ];
    for statement in statements {
        sqlx::query(statement)
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                database_error(ErrorCode::MigrationFailed, "apply runtime schema", error)
            })?;
    }
    let current =
        sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(version) FROM runtime_schema_migrations")
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| {
                database_error(
                    ErrorCode::MigrationFailed,
                    "read runtime schema version",
                    error,
                )
            })?
            .unwrap_or(0);
    if current > RUNTIME_SCHEMA_VERSION {
        return Err(StoreError::new(
            ErrorCode::MigrationFailed,
            "runtime schema is newer than this host supports",
        ));
    }
    if current < RUNTIME_SCHEMA_VERSION {
        sqlx::query("INSERT INTO runtime_schema_migrations(version) VALUES (?)")
            .bind(RUNTIME_SCHEMA_VERSION)
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                database_error(ErrorCode::MigrationFailed, "record runtime schema", error)
            })?;
    }
    tx.commit().await.map_err(|error| {
        database_error(
            ErrorCode::MigrationFailed,
            "commit runtime migration",
            error,
        )
    })
}

async fn acquire_fence(pool: &SqlitePool, host_id: HostId) -> Result<HostFence, StoreError> {
    let mut transaction = pool.begin().await.map_err(|error| {
        database_error(
            ErrorCode::StorageWriteFailed,
            "begin host fence transaction",
            error,
        )
    })?;
    sqlx::query(
        "INSERT INTO host_epoch(singleton, generation) VALUES (1, 0)
         ON CONFLICT(singleton) DO NOTHING",
    )
    .execute(&mut *transaction)
    .await
    .map_err(|error| {
        database_error(
            ErrorCode::StorageWriteFailed,
            "initialize host epoch",
            error,
        )
    })?;
    let current =
        sqlx::query_scalar::<_, i64>("SELECT generation FROM host_epoch WHERE singleton = 1")
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageWriteFailed, "read host epoch", error)
            })?;
    let next = current.checked_add(1).ok_or_else(|| {
        StoreError::new(ErrorCode::StorageWriteFailed, "host generation overflow")
    })?;
    sqlx::query("UPDATE host_epoch SET generation = ? WHERE singleton = 1")
        .bind(next)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "advance host epoch", error)
        })?;
    sqlx::query(
        "INSERT INTO hosts(host_id, generation) VALUES (?, ?)
         ON CONFLICT(host_id) DO UPDATE SET generation = excluded.generation, opened_at = CURRENT_TIMESTAMP",
    )
    .bind(host_id.as_str())
    .bind(next)
    .execute(&mut *transaction)
    .await
    .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "record host fence", error))?;
    transaction.commit().await.map_err(|error| {
        database_error(
            ErrorCode::StorageWriteFailed,
            "commit host fence transaction",
            error,
        )
    })?;
    Ok(HostFence {
        host_id,
        generation: to_u64(next, "host generation")?,
    })
}

async fn assert_fence_in_tx(
    transaction: &mut Transaction<'_, Sqlite>,
    fence: &HostFence,
) -> Result<(), StoreError> {
    let current = sqlx::query_scalar::<_, i64>(
        "SELECT host_epoch.generation
         FROM host_epoch
         JOIN hosts ON hosts.generation = host_epoch.generation
                     AND hosts.host_id = ?
         WHERE host_epoch.singleton = 1",
    )
    .bind(fence.host_id.as_str())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "check host fence", error))?;
    if current != Some(to_i64(fence.generation, "fence generation")?) {
        return Err(StoreError::new(
            ErrorCode::StaleWriter,
            "writer host or generation is no longer current",
        ));
    }
    Ok(())
}

async fn ensure_session(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: &SessionId,
    task_id: &TaskId,
    expected_sequence: u64,
) -> Result<(), StoreError> {
    let existing = session_task_in_tx(transaction, session_id).await?;
    match existing {
        Some(existing_task) if existing_task != *task_id => Err(StoreError::new(
            ErrorCode::IdempotencyConflict,
            "session is already bound to another task",
        )),
        Some(_) => Ok(()),
        None => {
            if expected_sequence != 1 {
                return Err(StoreError::new(
                    ErrorCode::SequenceConflict,
                    "a new session must admit sequence 1",
                ));
            }
            sqlx::query(
                "INSERT INTO sessions(session_id, task_id, next_sequence) VALUES (?, ?, 1)",
            )
            .bind(session_id.as_str())
            .bind(task_id.as_str())
            .execute(&mut **transaction)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageWriteFailed, "create session", error)
            })?;
            Ok(())
        }
    }
}

async fn ensure_session_existing(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: &SessionId,
    task_id: &TaskId,
) -> Result<(), StoreError> {
    match session_task_in_tx(transaction, session_id).await? {
        Some(existing_task) if existing_task == *task_id => Ok(()),
        Some(_) => Err(StoreError::new(
            ErrorCode::IdempotencyConflict,
            "session is already bound to another task",
        )),
        None => Err(StoreError::new(
            ErrorCode::InvalidPayload,
            "session must exist before this operation",
        )),
    }
}

async fn session_task_in_tx(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: &SessionId,
) -> Result<Option<TaskId>, StoreError> {
    let task_id =
        sqlx::query_scalar::<_, String>("SELECT task_id FROM sessions WHERE session_id = ?")
            .bind(session_id.as_str())
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|error| {
                database_error(
                    ErrorCode::StorageWriteFailed,
                    "read session ownership",
                    error,
                )
            })?;
    task_id
        .map(|value| {
            TaskId::parse(value).map_err(|_| {
                StoreError::new(ErrorCode::StorageWriteFailed, "stored task ID is invalid")
            })
        })
        .transpose()
}

async fn claim_task(
    transaction: &mut Transaction<'_, Sqlite>,
    task_id: &TaskId,
    session_id: &SessionId,
    fence: &HostFence,
) -> Result<(), StoreError> {
    let existing =
        sqlx::query("SELECT session_id, host_id, generation FROM task_leases WHERE task_id = ?")
            .bind(task_id.as_str())
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageWriteFailed, "read task lease", error)
            })?;
    if let Some(row) = existing {
        let existing_session: String = row_get(&row, "session_id")?;
        let existing_host: String = row_get(&row, "host_id")?;
        let existing_generation = to_u64(row_get(&row, "generation")?, "task lease generation")?;
        if existing_session != session_id.as_str()
            && (existing_generation > fence.generation
                || (existing_generation == fence.generation
                    && existing_host == fence.host_id.as_str()))
        {
            return Err(StoreError::new(
                ErrorCode::TaskLeaseConflict,
                format!(
                    "another session currently owns this task (lease_session={existing_session}, lease_host={existing_host}, lease_generation={existing_generation}, current_host={}, current_generation={})",
                    fence.host_id, fence.generation
                ),
            ));
        }
        sqlx::query(
            "UPDATE task_leases SET session_id = ?, host_id = ?, generation = ?, claimed_at = CURRENT_TIMESTAMP
             WHERE task_id = ?",
        )
        .bind(session_id.as_str())
        .bind(fence.host_id.as_str())
        .bind(to_i64(fence.generation, "task lease generation")?)
        .bind(task_id.as_str())
        .execute(&mut **transaction)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "refresh task lease", error)
        })?;
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO task_leases(task_id, session_id, host_id, generation) VALUES (?, ?, ?, ?)",
    )
    .bind(task_id.as_str())
    .bind(session_id.as_str())
    .bind(fence.host_id.as_str())
    .bind(to_i64(fence.generation, "task lease generation")?)
    .execute(&mut **transaction)
    .await
    .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "claim task lease", error))?;
    Ok(())
}

async fn ensure_expected_sequence(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: &SessionId,
    expected_sequence: u64,
) -> Result<(), StoreError> {
    let actual = session_next_sequence(transaction, session_id).await?;
    if actual != expected_sequence {
        return Err(StoreError::new(
            ErrorCode::SequenceConflict,
            format!("expected sequence {expected_sequence}, current next sequence is {actual}"),
        ));
    }
    Ok(())
}

async fn session_next_sequence(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: &SessionId,
) -> Result<u64, StoreError> {
    let value =
        sqlx::query_scalar::<_, i64>("SELECT next_sequence FROM sessions WHERE session_id = ?")
            .bind(session_id.as_str())
            .fetch_one(&mut **transaction)
            .await
            .map_err(|error| {
                database_error(
                    ErrorCode::StorageWriteFailed,
                    "read session sequence",
                    error,
                )
            })?;
    to_u64(value, "session sequence")
}

async fn advance_sequence(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: &SessionId,
    expected_sequence: u64,
) -> Result<(), StoreError> {
    let result = sqlx::query(
        "UPDATE sessions SET next_sequence = next_sequence + 1
         WHERE session_id = ? AND next_sequence = ?",
    )
    .bind(session_id.as_str())
    .bind(to_i64(expected_sequence, "expected sequence")?)
    .execute(&mut **transaction)
    .await
    .map_err(|error| {
        database_error(
            ErrorCode::StorageWriteFailed,
            "advance session sequence",
            error,
        )
    })?;
    if result.rows_affected() != 1 {
        return Err(StoreError::new(
            ErrorCode::SequenceConflict,
            "session sequence compare-and-swap failed",
        ));
    }
    Ok(())
}

async fn upsert_projection(
    transaction: &mut Transaction<'_, Sqlite>,
    task_id: &TaskId,
    session_id: &SessionId,
    state: &WorkingState,
    state_json: String,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO task_projections(task_id, session_id, revision, through_sequence, working_state_json)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(task_id) DO UPDATE SET
             session_id = excluded.session_id,
             revision = excluded.revision,
             through_sequence = excluded.through_sequence,
             working_state_json = excluded.working_state_json",
    )
    .bind(task_id.as_str())
    .bind(session_id.as_str())
    .bind(to_i64(state.revision, "working state revision")?)
    .bind(to_i64(state.through_event_seq, "working state sequence")?)
    .bind(state_json)
    .execute(&mut **transaction)
    .await
    .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "upsert task projection", error))?;
    Ok(())
}

async fn insert_marker(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: &SessionId,
    task_id: &TaskId,
    marker: &crate::SourceWorkMarker,
    marker_json: String,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO source_work_markers(marker_id, session_id, task_id, event_id, sequence, marker_json)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&marker.marker_id)
    .bind(session_id.as_str())
    .bind(task_id.as_str())
    .bind(marker.event_id.as_str())
    .bind(to_i64(marker.sequence, "marker sequence")?)
    .bind(marker_json)
    .execute(&mut **transaction)
    .await
    .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "insert source-work marker", error))?;
    Ok(())
}

fn validate_admission(commit: &AdmissionCommit) -> Result<(), StoreError> {
    if commit.raw_text.trim().is_empty() {
        return Err(StoreError::new(
            ErrorCode::InvalidPayload,
            "raw user input must not be empty",
        ));
    }
    commit.event.validate().map_err(|error| {
        StoreError::new(error.code(), format!("input event is invalid: {error}"))
    })?;
    commit.instruction.validate().map_err(|error| {
        StoreError::new(
            error.code(),
            format!("instruction ledger entry is invalid: {error}"),
        )
    })?;
    commit.working_state.validate().map_err(|error| {
        StoreError::new(error.code(), format!("working state is invalid: {error}"))
    })?;
    if commit.event.session_id != commit.session_id
        || commit.event.seq != commit.expected_sequence
        || commit.working_state.session_id != commit.session_id
        || commit.working_state.task_id != commit.task_id
        || commit.working_state.through_event_seq != commit.expected_sequence
        || commit.marker.event_id != commit.event.event_id
        || commit.marker.sequence != commit.expected_sequence
    {
        return Err(StoreError::new(
            ErrorCode::InvalidPayload,
            "admission records do not describe one consistent event",
        ));
    }
    Ok(())
}

fn validate_receipt(commit: &ReceiptCommit) -> Result<(), StoreError> {
    commit.event.validate().map_err(|error| {
        StoreError::new(error.code(), format!("receipt event is invalid: {error}"))
    })?;
    commit
        .receipt
        .validate()
        .map_err(|error| StoreError::new(error.code(), format!("receipt is invalid: {error}")))?;
    commit.working_state.validate().map_err(|error| {
        StoreError::new(error.code(), format!("working state is invalid: {error}"))
    })?;
    if commit.event.session_id != commit.session_id
        || commit.event.seq != commit.expected_sequence
        || commit.receipt.task_id != commit.task_id
        || commit.receipt.observed_at_seq != commit.expected_sequence
        || commit.working_state.session_id != commit.session_id
        || commit.working_state.task_id != commit.task_id
        || commit.working_state.through_event_seq != commit.expected_sequence
        || commit.marker.event_id != commit.event.event_id
        || commit.marker.sequence != commit.expected_sequence
    {
        return Err(StoreError::new(
            ErrorCode::InvalidPayload,
            "receipt records do not describe one consistent event",
        ));
    }
    match (&commit.receipt.artifact_id, &commit.artifact) {
        (None, None) => {}
        (Some(receipt_artifact), Some(published_artifact))
            if receipt_artifact == &published_artifact.artifact_id => {}
        (Some(_), Some(_)) => {
            return Err(StoreError::new(
                ErrorCode::ArtifactWriteFailed,
                "receipt artifact ID does not match the published artifact",
            ));
        }
        (Some(_), None) | (None, Some(_)) => {
            return Err(StoreError::new(
                ErrorCode::ArtifactWriteFailed,
                "receipt artifact ID and published artifact must be supplied together",
            ));
        }
    }
    Ok(())
}

fn snapshot_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<SnapshotRecord, StoreError> {
    let snapshot_id = SnapshotId::parse(row_get::<String>(row, "snapshot_id")?).map_err(|_| {
        StoreError::new(
            ErrorCode::StorageWriteFailed,
            "stored snapshot ID is invalid",
        )
    })?;
    let session_id = SessionId::parse(row_get::<String>(row, "session_id")?).map_err(|_| {
        StoreError::new(
            ErrorCode::StorageWriteFailed,
            "stored snapshot session ID is invalid",
        )
    })?;
    let task_id = TaskId::parse(row_get::<String>(row, "task_id")?).map_err(|_| {
        StoreError::new(
            ErrorCode::StorageWriteFailed,
            "stored snapshot task ID is invalid",
        )
    })?;
    let content: Value = serde_json::from_str(&row_get::<String>(row, "snapshot_json")?)
        .map_err(|_| StoreError::new(ErrorCode::SnapshotCorrupt, "snapshot JSON is corrupt"))?;
    let content_hash =
        ContentHash::parse(row_get::<String>(row, "content_hash")?).map_err(|_| {
            StoreError::new(
                ErrorCode::SnapshotCorrupt,
                "snapshot content hash is invalid",
            )
        })?;
    Ok(SnapshotRecord {
        snapshot_id,
        session_id,
        task_id,
        through_sequence: to_u64(
            row_get::<i64>(row, "through_sequence")?,
            "snapshot sequence",
        )?,
        schema_version: u16::try_from(row_get::<i64>(row, "schema_version")?).map_err(|_| {
            StoreError::new(
                ErrorCode::SnapshotCorrupt,
                "snapshot schema version is invalid",
            )
        })?,
        content,
        content_hash,
    })
}

fn validate_hashed_json(content: &Value, expected: &ContentHash) -> Result<(), StoreError> {
    let actual = ContentHash::from_canonical_json(content).map_err(|error| {
        StoreError::new(error.code(), format!("content is not canonical: {error}"))
    })?;
    if &actual != expected {
        return Err(StoreError::new(
            ErrorCode::InvalidHash,
            "content hash does not match canonical content",
        ));
    }
    Ok(())
}

fn parse_session(value: String) -> Result<SessionId, StoreError> {
    SessionId::parse(value).map_err(|_| {
        StoreError::new(
            ErrorCode::StorageWriteFailed,
            "stored session ID is invalid",
        )
    })
}
fn parse_task(value: String) -> Result<TaskId, StoreError> {
    TaskId::parse(value)
        .map_err(|_| StoreError::new(ErrorCode::StorageWriteFailed, "stored task ID is invalid"))
}

fn runtime_command_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<RuntimeCommandRecord, StoreError> {
    let state = RuntimeCommandState::parse(&row_get::<String>(row, "state")?).ok_or_else(|| {
        StoreError::new(
            ErrorCode::StorageWriteFailed,
            "stored runtime command state is invalid",
        )
    })?;
    Ok(RuntimeCommandRecord {
        command_id: RuntimeCommandId::parse(row_get::<String>(row, "command_id")?).map_err(
            |_| {
                StoreError::new(
                    ErrorCode::StorageWriteFailed,
                    "stored command ID is invalid",
                )
            },
        )?,
        session_id: parse_session(row_get::<String>(row, "session_id")?)?,
        task_id: parse_task(row_get::<String>(row, "task_id")?)?,
        state,
        attempts: u32::try_from(to_u64(
            row_get::<i64>(row, "attempts")?,
            "command attempts",
        )?)
        .map_err(|_| {
            StoreError::new(
                ErrorCode::StorageWriteFailed,
                "command attempts exceed range",
            )
        })?,
        owner_generation: to_u64(
            row_get::<i64>(row, "owner_generation")?,
            "command generation",
        )?,
        payload: serde_json::from_str(&row_get::<String>(row, "payload_json")?).map_err(|_| {
            StoreError::new(
                ErrorCode::StorageWriteFailed,
                "stored command payload is invalid",
            )
        })?,
        last_error: row.try_get("last_error").map_err(|_| {
            StoreError::new(
                ErrorCode::StorageWriteFailed,
                "stored command error is invalid",
            )
        })?,
    })
}

fn composition_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<CompositionSnapshotRecord, StoreError> {
    let content: Value =
        serde_json::from_str(&row_get::<String>(row, "content_json")?).map_err(|_| {
            StoreError::new(
                ErrorCode::StorageWriteFailed,
                "composition snapshot JSON is invalid",
            )
        })?;
    let hash = ContentHash::parse(row_get::<String>(row, "content_hash")?).map_err(|_| {
        StoreError::new(
            ErrorCode::StorageWriteFailed,
            "composition snapshot hash is invalid",
        )
    })?;
    validate_hashed_json(&content, &hash)?;
    Ok(CompositionSnapshotRecord {
        snapshot_id: CompositionSnapshotId::parse(row_get::<String>(row, "snapshot_id")?).map_err(
            |_| {
                StoreError::new(
                    ErrorCode::StorageWriteFailed,
                    "composition snapshot ID is invalid",
                )
            },
        )?,
        session_id: parse_session(row_get::<String>(row, "session_id")?)?,
        task_id: parse_task(row_get::<String>(row, "task_id")?)?,
        revision: to_u64(row_get::<i64>(row, "revision")?, "composition revision")?,
        content,
        content_hash: hash,
    })
}

fn context_packet_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<ContextPacketRecord, StoreError> {
    let packet: ContextPacket = serde_json::from_str(&row_get::<String>(row, "packet_json")?)
        .map_err(|_| {
            StoreError::new(
                ErrorCode::StorageWriteFailed,
                "context packet JSON is invalid",
            )
        })?;
    packet.validate().map_err(|error| {
        StoreError::new(
            error.code(),
            format!("stored context packet is invalid: {error}"),
        )
    })?;
    let composition_snapshot_id = row
        .try_get::<Option<String>, _>("composition_snapshot_id")
        .map_err(|_| {
            StoreError::new(
                ErrorCode::StorageWriteFailed,
                "composition snapshot reference is invalid",
            )
        })?
        .map(|value| {
            CompositionSnapshotId::parse(value).map_err(|_| {
                StoreError::new(
                    ErrorCode::StorageWriteFailed,
                    "composition snapshot ID is invalid",
                )
            })
        })
        .transpose()?;
    let omitted_optional =
        serde_json::from_str(&row_get::<String>(row, "omitted_json")?).map_err(|_| {
            StoreError::new(
                ErrorCode::StorageWriteFailed,
                "omitted context JSON is invalid",
            )
        })?;
    Ok(ContextPacketRecord {
        packet,
        composition_snapshot_id,
        omitted_optional,
        degradation: row.try_get("degradation").map_err(|_| {
            StoreError::new(
                ErrorCode::StorageWriteFailed,
                "context degradation is invalid",
            )
        })?,
    })
}

#[allow(clippy::needless_borrow)]
fn frozen_request_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<FrozenRequestRecord, StoreError> {
    let request_json: Value = serde_json::from_str(&row_get::<String>(&row, "request_json")?)
        .map_err(|_| {
            StoreError::new(
                ErrorCode::StorageWriteFailed,
                "frozen request JSON is invalid",
            )
        })?;
    let content_hash =
        ContentHash::parse(row_get::<String>(&row, "content_hash")?).map_err(|_| {
            StoreError::new(
                ErrorCode::StorageWriteFailed,
                "frozen request hash is invalid",
            )
        })?;
    validate_hashed_json(&request_json, &content_hash)?;
    let composition_snapshot_id = row
        .try_get::<Option<String>, _>("composition_snapshot_id")
        .map_err(|_| {
            StoreError::new(
                ErrorCode::StorageWriteFailed,
                "frozen composition reference is invalid",
            )
        })?
        .map(|value| {
            CompositionSnapshotId::parse(value).map_err(|_| {
                StoreError::new(
                    ErrorCode::StorageWriteFailed,
                    "composition snapshot ID is invalid",
                )
            })
        })
        .transpose()?;
    Ok(FrozenRequestRecord {
        request_id: RequestId::parse(row_get::<String>(&row, "request_id")?)
            .map_err(|_| StoreError::new(ErrorCode::StorageWriteFailed, "request ID is invalid"))?,
        packet_id: ContextPacketId::parse(row_get::<String>(&row, "packet_id")?)
            .map_err(|_| StoreError::new(ErrorCode::StorageWriteFailed, "packet ID is invalid"))?,
        composition_snapshot_id,
        session_id: parse_session(row_get::<String>(&row, "session_id")?)?,
        task_id: parse_task(row_get::<String>(&row, "task_id")?)?,
        request_json,
        content_hash,
        provider_id: row_get::<String>(&row, "provider_id")?,
        model: row_get::<String>(&row, "model")?,
        config_revision: to_u64(row_get::<i64>(&row, "config_revision")?, "config revision")?,
    })
}

#[allow(clippy::needless_borrow)]
fn provider_attempt_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<ProviderAttemptRecord, StoreError> {
    let events = serde_json::from_str(&row_get::<String>(&row, "events_json")?).map_err(|_| {
        StoreError::new(
            ErrorCode::StorageWriteFailed,
            "provider events JSON is invalid",
        )
    })?;
    let response_hash = row
        .try_get::<Option<String>, _>("response_hash")
        .map_err(|_| {
            StoreError::new(
                ErrorCode::StorageWriteFailed,
                "provider response hash is invalid",
            )
        })?
        .map(|value| {
            ContentHash::parse(value).map_err(|_| {
                StoreError::new(
                    ErrorCode::StorageWriteFailed,
                    "provider response hash is invalid",
                )
            })
        })
        .transpose()?;
    Ok(ProviderAttemptRecord {
        attempt_id: ProviderAttemptId::parse(row_get::<String>(&row, "attempt_id")?)
            .map_err(|_| StoreError::new(ErrorCode::StorageWriteFailed, "attempt ID is invalid"))?,
        request_id: RequestId::parse(row_get::<String>(&row, "request_id")?)
            .map_err(|_| StoreError::new(ErrorCode::StorageWriteFailed, "request ID is invalid"))?,
        session_id: parse_session(row_get::<String>(&row, "session_id")?)?,
        task_id: parse_task(row_get::<String>(&row, "task_id")?)?,
        attempt_number: u32::try_from(to_u64(
            row_get::<i64>(&row, "attempt_number")?,
            "attempt number",
        )?)
        .map_err(|_| {
            StoreError::new(
                ErrorCode::StorageWriteFailed,
                "attempt number exceeds range",
            )
        })?,
        state: row_get::<String>(&row, "state")?,
        events,
        response_hash,
        error: row.try_get("error").map_err(|_| {
            StoreError::new(ErrorCode::StorageWriteFailed, "provider error is invalid")
        })?,
    })
}

#[allow(clippy::needless_borrow)]
fn checkpoint_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<ContextCheckpointRecord, StoreError> {
    let content: Value =
        serde_json::from_str(&row_get::<String>(&row, "content_json")?).map_err(|_| {
            StoreError::new(ErrorCode::StorageWriteFailed, "checkpoint JSON is invalid")
        })?;
    let content_hash =
        ContentHash::parse(row_get::<String>(&row, "content_hash")?).map_err(|_| {
            StoreError::new(ErrorCode::StorageWriteFailed, "checkpoint hash is invalid")
        })?;
    validate_hashed_json(&content, &content_hash)?;
    Ok(ContextCheckpointRecord {
        checkpoint_id: row_get::<String>(&row, "checkpoint_id")?,
        session_id: parse_session(row_get::<String>(&row, "session_id")?)?,
        task_id: parse_task(row_get::<String>(&row, "task_id")?)?,
        through_sequence: to_u64(
            row_get::<i64>(&row, "through_sequence")?,
            "checkpoint sequence",
        )?,
        revision: to_u64(row_get::<i64>(&row, "revision")?, "checkpoint revision")?,
        content,
        content_hash,
    })
}

fn to_json<T: Serialize>(value: &T, context: &str) -> Result<String, StoreError> {
    serde_json::to_string(value)
        .map_err(|_| StoreError::new(ErrorCode::InvalidPayload, format!("{context} failed")))
}

fn row_get<T>(row: &sqlx::sqlite::SqliteRow, column: &str) -> Result<T, StoreError>
where
    T: for<'r> sqlx::Decode<'r, Sqlite> + sqlx::Type<Sqlite> + Send + Unpin,
{
    row.try_get(column).map_err(|_| {
        StoreError::new(
            ErrorCode::StorageWriteFailed,
            format!("stored column {column} has an invalid type"),
        )
    })
}

fn to_i64(value: u64, field: &str) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| {
        StoreError::new(
            ErrorCode::InvalidPayload,
            format!("{field} exceeds SQLite integer range"),
        )
    })
}

fn to_u64(value: i64, field: &str) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| {
        StoreError::new(
            ErrorCode::StorageWriteFailed,
            format!("stored {field} is negative"),
        )
    })
}

#[allow(clippy::needless_pass_by_value)]
fn database_error(code: ErrorCode, operation: &str, error: sqlx::Error) -> StoreError {
    StoreError::new(code, format!("{operation}: {error}"))
}
