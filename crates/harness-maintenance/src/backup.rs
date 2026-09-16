//! Consistent backup and isolated, validated restore.
//!
//! The snapshot is produced by `SQLite` itself (`VACUUM INTO`), which yields a
//! complete standalone database rather than a half-copied WAL set. Every
//! referenced artifact is hashed into the manifest, so a restore can prove the
//! backup is complete before anything is activated. A restore only ever writes
//! into a destination that does not already hold an active store.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use harness_store_sqlite::{SqliteStore, StorePaths, WriterOpenOptions};
use harness_types::{ContentHash, ErrorCode, HostId};
use sqlx::{Connection, Row, SqliteConnection};

use crate::contracts::{
    ArtifactPin, BACKUP_DATABASE_NAME, BACKUP_MANIFEST_NAME, BackupManifest,
    MAINTENANCE_CONTRACT_VERSION, MaintenanceError, RestoreReport, RetentionPin, now_unix_ms,
};

/// What a backup produced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupOutcome {
    pub backup_dir: String,
    pub database_hash: ContentHash,
    pub artifact_count: usize,
    pub total_artifact_bytes: u64,
    pub pinned: usize,
    pub tombstones: usize,
    pub manifest_hash: ContentHash,
}

/// What a restore produced. `report.activated` stays false by construction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RestoreOutcome {
    pub report: RestoreReport,
    pub destination: String,
}

/// Take a consistent backup of a data directory into a new backup directory.
///
/// An existing backup directory is refused rather than merged, so a backup can
/// never silently contain a mix of two snapshots.
#[allow(clippy::too_many_lines)] // One snapshot path; splitting hides the ordering.
pub async fn create_backup(
    data_dir: impl AsRef<Path>,
    backup_dir: impl AsRef<Path>,
) -> Result<BackupOutcome, MaintenanceError> {
    let paths = StorePaths::new(data_dir.as_ref());
    if !paths.database_path.is_file() {
        return Err(MaintenanceError::new(
            ErrorCode::BackupManifestInvalid,
            format!(
                "no database at {}; nothing to back up",
                paths.database_path.display()
            ),
        ));
    }
    let backup_dir = backup_dir.as_ref().to_path_buf();
    if backup_dir.exists() {
        let manifest = backup_dir.join(BACKUP_MANIFEST_NAME);
        if manifest.is_file() || backup_dir.join(BACKUP_DATABASE_NAME).is_file() {
            return Err(MaintenanceError::new(
                ErrorCode::BackupManifestInvalid,
                format!(
                    "{} already holds a backup; refusing to overwrite it",
                    backup_dir.display()
                ),
            ));
        }
    }
    std::fs::create_dir_all(&backup_dir)?;
    let target = backup_dir.join(BACKUP_DATABASE_NAME);

    // SQLite's own snapshot support produces a complete, consistent database
    // file, including everything committed from the write-ahead log.
    let mut connection =
        SqliteConnection::connect(&format!("sqlite:{}", paths.database_path.display()))
            .await
            .map_err(|error| {
                MaintenanceError::new(
                    ErrorCode::StorageOpenFailed,
                    format!("cannot open the store for backup: {error}"),
                )
            })?;
    // Fold the write-ahead log into the main database first. `VACUUM INTO` reads
    // the database file, so without this checkpoint a committed artifact row that
    // is still only in the WAL would be missing from the snapshot.
    let checkpoint = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .fetch_optional(&mut connection)
        .await
        .map_err(|error| {
            MaintenanceError::new(
                ErrorCode::StorageWriteFailed,
                format!("cannot checkpoint the store before backup: {error}"),
            )
        });
    checkpoint?;
    // `VACUUM INTO` takes a string literal, so the destination is validated and
    // then quoted by doubling any embedded quote. A path containing a NUL byte
    // or a newline is refused outright rather than escaped. SQLite does not
    // accept a bound parameter here, so the literal is unavoidable.
    let destination_text = target.to_string_lossy().into_owned();
    if destination_text.contains('\0') || destination_text.contains('\n') {
        return Err(MaintenanceError::new(
            ErrorCode::InvalidPayload,
            "the backup destination path contains an unsupported character",
        ));
    }
    let escaped = destination_text.replace('\'', "''");
    // The destination is a host-supplied path, not request data. NUL and newline
    // were refused above and quotes are doubled, so the literal is safe; SQLite
    // accepts no bound parameter for `VACUUM INTO`, so the audit assertion is
    // made explicitly rather than silently swallowed.
    let statement = sqlx::raw_sql(sqlx::AssertSqlSafe(format!("VACUUM INTO '{escaped}'")));
    statement.execute(&mut connection).await.map_err(|error| {
        MaintenanceError::new(
            ErrorCode::StorageWriteFailed,
            format!("cannot snapshot the store: {error}"),
        )
    })?;
    let _ = connection.close().await;

    let database_bytes = std::fs::read(&target)?;
    let database_hash = ContentHash::from_bytes(&database_bytes);
    let database_byte_len = u64::try_from(database_bytes.len()).unwrap_or(u64::MAX);

    // Read the pinned artifacts and schema revisions from the snapshot itself, so
    // the manifest describes the snapshot rather than the live store.
    let snapshot = SqliteStore::open_read_only(&backup_dir).await?;
    let artifacts = read_artifact_pins(&snapshot).await?;
    let schema_revisions = read_schema_revisions(&snapshot).await?;
    let tombstones = read_tombstone_ids(&snapshot).await.unwrap_or_default();
    let pins = read_retention_pins(&snapshot).await.unwrap_or_default();
    let total_artifact_bytes = artifacts.iter().map(|pin| pin.byte_len).sum();
    snapshot.close().await?;

    // Copy every referenced artifact next to the snapshot and verify the bytes
    // that were copied.
    let artifact_dir = backup_dir.join("artifacts");
    let mut verified = Vec::with_capacity(artifacts.len());
    for pin in &artifacts {
        let source = paths.data_dir.join(&pin.relative_path);
        let bytes = std::fs::read(&source).map_err(|error| {
            MaintenanceError::new(
                ErrorCode::BackupManifestInvalid,
                format!(
                    "referenced artifact {} is missing: {error}",
                    pin.relative_path
                ),
            )
        })?;
        let hash = ContentHash::from_bytes(&bytes);
        if hash != pin.content_hash {
            return Err(MaintenanceError::new(
                ErrorCode::BackupManifestInvalid,
                format!(
                    "artifact {} does not match its recorded hash",
                    pin.relative_path
                ),
            ));
        }
        let destination = backup_dir.join(&pin.relative_path);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&destination, &bytes)?;
        verified.push(pin.clone());
    }
    let _ = artifact_dir;

    let manifest = BackupManifest {
        schema_version: MAINTENANCE_CONTRACT_VERSION,
        created_unix_ms: now_unix_ms(),
        source_data_dir: paths.data_dir.to_string_lossy().into_owned(),
        schema_revisions,
        database_file: BACKUP_DATABASE_NAME.to_owned(),
        database_hash: database_hash.clone(),
        database_byte_len,
        artifacts: verified.clone(),
        pins,
        tombstones: tombstones.clone(),
        manifest_hash: ContentHash::from_bytes(b"placeholder"),
    };
    let manifest = BackupManifest {
        manifest_hash: manifest.compute_hash()?,
        ..manifest
    };
    manifest.validate()?;
    let manifest_path = backup_dir.join(BACKUP_MANIFEST_NAME);
    let rendered = serde_json::to_vec_pretty(&manifest).map_err(|_| {
        MaintenanceError::new(ErrorCode::InvalidPayload, "manifest is not serializable")
    })?;
    std::fs::write(&manifest_path, rendered)?;

    Ok(BackupOutcome {
        backup_dir: backup_dir.to_string_lossy().into_owned(),
        database_hash,
        artifact_count: verified.len(),
        total_artifact_bytes,
        pinned: manifest.pins.len(),
        tombstones: manifest.tombstones.len(),
        manifest_hash: manifest.manifest_hash.clone(),
    })
}

/// Validate a backup without restoring it. This is what the doctor uses.
pub async fn verify_backup(
    backup_dir: impl AsRef<Path>,
) -> Result<BackupManifest, MaintenanceError> {
    let dir = backup_dir.as_ref();
    let manifest_path = dir.join(BACKUP_MANIFEST_NAME);
    let bytes = std::fs::read(&manifest_path).map_err(|error| {
        MaintenanceError::new(
            ErrorCode::BackupManifestInvalid,
            format!("cannot read {}: {error}", manifest_path.display()),
        )
    })?;
    let manifest: BackupManifest = serde_json::from_slice(&bytes).map_err(|_| {
        MaintenanceError::new(
            ErrorCode::BackupManifestInvalid,
            "the backup manifest is not valid JSON",
        )
    })?;
    manifest.validate()?;

    let database = dir.join(&manifest.database_file);
    let database_bytes = std::fs::read(&database).map_err(|error| {
        MaintenanceError::new(
            ErrorCode::BackupManifestInvalid,
            format!("the backup database is missing: {error}"),
        )
    })?;
    if ContentHash::from_bytes(&database_bytes) != manifest.database_hash {
        return Err(MaintenanceError::new(
            ErrorCode::BackupManifestInvalid,
            "the backup database does not match its manifest hash",
        ));
    }
    for pin in &manifest.artifacts {
        let path = dir.join(&pin.relative_path);
        let bytes = std::fs::read(&path).map_err(|error| {
            MaintenanceError::new(
                ErrorCode::BackupManifestInvalid,
                format!("backup artifact {} is missing: {error}", pin.relative_path),
            )
        })?;
        if ContentHash::from_bytes(&bytes) != pin.content_hash {
            return Err(MaintenanceError::new(
                ErrorCode::BackupManifestInvalid,
                format!(
                    "backup artifact {} does not match its hash",
                    pin.relative_path
                ),
            ));
        }
    }
    Ok(manifest)
}

/// Restore a backup into a **new** data directory and validate it.
///
/// The destination must not already hold a store. Restoring never activates the
/// result: activation is a separate explicit maintenance action.
pub async fn restore_backup(
    backup_dir: impl AsRef<Path>,
    destination: impl AsRef<Path>,
) -> Result<RestoreOutcome, MaintenanceError> {
    let backup_dir = backup_dir.as_ref();
    let destination = destination.as_ref().to_path_buf();
    let manifest = verify_backup(backup_dir).await?;

    let destination_paths = StorePaths::new(&destination);
    if destination_paths.database_path.exists() {
        return Err(MaintenanceError::new(
            ErrorCode::RestoreTargetConflict,
            format!(
                "{} already holds a store; restore requires a fresh directory",
                destination.display()
            ),
        ));
    }
    if destination.join(".active").is_file() {
        return Err(MaintenanceError::new(
            ErrorCode::RestoreTargetConflict,
            "the destination is marked active; refusing to overwrite it",
        ));
    }
    std::fs::create_dir_all(&destination)?;

    // Copy the snapshot, then let SQLite validate it before trusting it.
    let snapshot_source = backup_dir.join(&manifest.database_file);
    std::fs::copy(&snapshot_source, &destination_paths.database_path)?;

    let database_verified = verify_snapshot_integrity(&destination_paths.database_path).await?;

    let mut artifacts_verified = 0usize;
    let mut missing = Vec::new();
    let mut corrupt = Vec::new();
    for pin in &manifest.artifacts {
        let source = backup_dir.join(&pin.relative_path);
        match std::fs::read(&source) {
            Ok(bytes) => {
                if ContentHash::from_bytes(&bytes) == pin.content_hash {
                    let target = destination.join(&pin.relative_path);
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&target, &bytes)?;
                    artifacts_verified += 1;
                } else {
                    corrupt.push(pin.relative_path.clone());
                }
            }
            Err(_) => missing.push(pin.relative_path.clone()),
        }
    }

    let report = RestoreReport {
        restored_into: destination.to_string_lossy().into_owned(),
        database_verified,
        artifacts_verified,
        artifacts_missing: missing,
        artifacts_corrupt: corrupt,
        schema_revisions: manifest.schema_revisions.clone(),
        tombstones_restored: manifest.tombstones.len(),
        activated: false,
    };
    if !report.is_complete() {
        return Err(MaintenanceError::new(
            ErrorCode::BackupManifestInvalid,
            format!(
                "the restored copy is incomplete: verified {}, missing {:?}, corrupt {:?}",
                report.artifacts_verified, report.artifacts_missing, report.artifacts_corrupt
            ),
        ));
    }
    // Record the provenance of this copy without activating it.
    let provenance = serde_json::json!({
        "schema_version": MAINTENANCE_CONTRACT_VERSION,
        "restored_from": backup_dir.to_string_lossy(),
        "manifest_hash": manifest.manifest_hash,
        "restored_unix_ms": now_unix_ms(),
        "activated": false,
    });
    std::fs::write(
        destination.join("restore.json"),
        serde_json::to_vec_pretty(&provenance).unwrap_or_default(),
    )?;

    Ok(RestoreOutcome {
        destination: destination.to_string_lossy().into_owned(),
        report,
    })
}

/// Open the restored store read-only so the caller can decide whether to
/// activate it. This is the explicit step a restore deliberately does not take.
pub async fn open_restored(destination: impl AsRef<Path>) -> Result<SqliteStore, MaintenanceError> {
    Ok(SqliteStore::open_read_only(destination.as_ref()).await?)
}

/// Mark a restored directory as active by opening it writable exactly once.
///
/// The caller must have decided to activate; nothing else in this module does.
pub async fn activate_restored(
    destination: impl AsRef<Path>,
) -> Result<SqliteStore, MaintenanceError> {
    let destination = destination.as_ref();
    let store =
        SqliteStore::open_writer(WriterOpenOptions::new(destination, HostId::generate())).await?;
    std::fs::write(
        destination.join(".active"),
        format!("activated_unix_ms={}\n", now_unix_ms()),
    )?;
    Ok(store)
}

async fn verify_snapshot_integrity(path: &Path) -> Result<bool, MaintenanceError> {
    let mut connection = SqliteConnection::connect(&format!("sqlite:{}", path.display()))
        .await
        .map_err(|error| {
            MaintenanceError::new(
                ErrorCode::StorageOpenFailed,
                format!("cannot open the restored snapshot: {error}"),
            )
        })?;
    let row = sqlx::query("PRAGMA integrity_check")
        .fetch_one(&mut connection)
        .await
        .map_err(|error| {
            MaintenanceError::new(
                ErrorCode::SnapshotCorrupt,
                format!("cannot validate the restored snapshot: {error}"),
            )
        })?;
    let verdict: String = row.try_get(0).unwrap_or_default();
    let _ = connection.close().await;
    Ok(verdict.eq_ignore_ascii_case("ok"))
}

async fn read_artifact_pins(store: &SqliteStore) -> Result<Vec<ArtifactPin>, MaintenanceError> {
    let rows = store.artifact_pins().await?;
    Ok(rows
        .into_iter()
        .map(
            |(artifact_id, relative_path, content_hash, byte_len)| ArtifactPin {
                artifact_id,
                relative_path,
                content_hash,
                byte_len,
            },
        )
        .collect())
}

async fn read_schema_revisions(
    store: &SqliteStore,
) -> Result<BTreeMap<String, i64>, MaintenanceError> {
    Ok(store.all_schema_revisions().await?)
}

async fn read_tombstone_ids(store: &SqliteStore) -> Result<Vec<String>, MaintenanceError> {
    Ok(store.tombstone_ids().await?)
}

async fn read_retention_pins(store: &SqliteStore) -> Result<Vec<RetentionPin>, MaintenanceError> {
    Ok(store
        .retention_pins()
        .await?
        .into_iter()
        .map(|(reason, task_id)| RetentionPin { reason, task_id })
        .collect())
}

/// The data directory a restored copy was taken from, for operator reporting.
pub fn backup_source(manifest: &BackupManifest) -> &str {
    &manifest.source_data_dir
}

/// A backup directory that exists and holds a manifest.
#[must_use]
pub fn is_backup_dir(path: impl AsRef<Path>) -> bool {
    path.as_ref().join(BACKUP_MANIFEST_NAME).is_file()
}

/// Convenience: the paths a caller needs when describing a backup.
#[must_use]
pub fn backup_paths(backup_dir: impl AsRef<Path>) -> (PathBuf, PathBuf) {
    let dir = backup_dir.as_ref();
    (
        dir.join(BACKUP_MANIFEST_NAME),
        dir.join(BACKUP_DATABASE_NAME),
    )
}
