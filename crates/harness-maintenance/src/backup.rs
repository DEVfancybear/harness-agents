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
use harness_types::{ContentHash, ErrorCode, EventId, HostId};
use sqlx::{Connection, Row, SqliteConnection, sqlite::SqliteConnectOptions};

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
    ensure_new_backup_destination(&backup_dir)?;
    let parent = backup_dir
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    ensure_new_backup_destination(&backup_dir)?;

    // Build beside the final path. Errors at any stage remove the unpublished
    // snapshot, while the final rename publishes the database, artifacts and
    // manifest as one complete directory.
    let staging_path = parent.join(format!(".harness-backup-{}", EventId::generate()));
    std::fs::create_dir(&staging_path)?;
    let mut staging = BackupStagingDir {
        path: staging_path,
        published: false,
    };
    let target = staging.path.join(BACKUP_DATABASE_NAME);

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
    let database_byte_len = u64::try_from(database_bytes.len()).map_err(|_| {
        MaintenanceError::new(
            ErrorCode::BackupManifestInvalid,
            "the backup database is too large to describe",
        )
    })?;

    // Read the pinned artifacts and schema revisions from the snapshot itself, so
    // the manifest describes the snapshot rather than the live store.
    let snapshot = SqliteStore::open_read_only(&staging.path).await?;
    let artifacts = read_artifact_pins(&snapshot).await?;
    let schema_revisions = read_schema_revisions(&snapshot).await?;
    // These are part of the restore contract, not optional diagnostics. A
    // damaged or incompatible snapshot must not be certified with empty lists.
    let tombstones = read_tombstone_ids(&snapshot).await?;
    let pins = read_retention_pins(&snapshot).await?;
    let total_artifact_bytes = artifacts.iter().try_fold(0_u64, |total, pin| {
        total.checked_add(pin.byte_len).ok_or_else(|| {
            MaintenanceError::new(
                ErrorCode::BackupManifestInvalid,
                "the total artifact byte count overflows",
            )
        })
    })?;
    snapshot.close().await?;

    // Copy every referenced artifact next to the snapshot and verify the bytes
    // that were copied.
    let artifact_dir = backup_dir.join("artifacts");
    let mut verified = Vec::with_capacity(artifacts.len());
    for pin in &artifacts {
        let source =
            resolve_contained_file(&paths.data_dir, &pin.relative_path, "source artifact")?;
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
        let destination = staging.path.join(&pin.relative_path);
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
    let manifest_path = staging.path.join(BACKUP_MANIFEST_NAME);
    let rendered = serde_json::to_vec_pretty(&manifest).map_err(|_| {
        MaintenanceError::new(ErrorCode::InvalidPayload, "manifest is not serializable")
    })?;
    std::fs::write(&manifest_path, rendered)?;
    ensure_new_backup_destination(&backup_dir)?;
    std::fs::rename(&staging.path, &backup_dir)?;
    staging.published = true;

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

fn ensure_new_backup_destination(destination: &Path) -> Result<(), MaintenanceError> {
    match std::fs::symlink_metadata(destination) {
        Ok(_) => Err(MaintenanceError::new(
            ErrorCode::BackupManifestInvalid,
            format!(
                "{} already exists; backup requires a new directory",
                destination.display()
            ),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

struct BackupStagingDir {
    path: PathBuf,
    published: bool,
}

impl Drop for BackupStagingDir {
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// Validate a backup without restoring it. This is what the doctor uses.
pub async fn verify_backup(
    backup_dir: impl AsRef<Path>,
) -> Result<BackupManifest, MaintenanceError> {
    let dir = backup_dir.as_ref();
    let backup_root = std::fs::canonicalize(dir).map_err(|error| {
        MaintenanceError::new(
            ErrorCode::BackupManifestInvalid,
            format!("cannot resolve backup directory {}: {error}", dir.display()),
        )
    })?;
    let manifest_path = resolve_contained_file(&backup_root, BACKUP_MANIFEST_NAME, "manifest")?;
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

    verify_backup_files(&backup_root, &manifest).await?;
    verify_manifest_snapshot(dir, &manifest).await?;
    Ok(manifest)
}

async fn verify_backup_files(
    backup_root: &Path,
    manifest: &BackupManifest,
) -> Result<(), MaintenanceError> {
    if manifest.database_file != BACKUP_DATABASE_NAME {
        return Err(MaintenanceError::new(
            ErrorCode::BackupManifestInvalid,
            "the backup database must use the supported database file name",
        ));
    }

    let database = resolve_contained_file(backup_root, &manifest.database_file, "backup database")?;
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
    let database_byte_len = u64::try_from(database_bytes.len()).map_err(|_| {
        MaintenanceError::new(
            ErrorCode::BackupManifestInvalid,
            "the backup database is too large to describe",
        )
    })?;
    if database_byte_len != manifest.database_byte_len {
        return Err(MaintenanceError::new(
            ErrorCode::BackupManifestInvalid,
            "the backup database byte length does not match its manifest",
        ));
    }
    if !verify_snapshot_integrity(&database).await? {
        return Err(MaintenanceError::new(
            ErrorCode::SnapshotCorrupt,
            "the backup database failed SQLite integrity_check",
        ));
    }
    for pin in &manifest.artifacts {
        let path = resolve_contained_file(backup_root, &pin.relative_path, "backup artifact")?;
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
        let byte_len = u64::try_from(bytes.len()).map_err(|_| {
            MaintenanceError::new(
                ErrorCode::BackupManifestInvalid,
                format!(
                    "backup artifact {} is too large to describe",
                    pin.relative_path
                ),
            )
        })?;
        if byte_len != pin.byte_len {
            return Err(MaintenanceError::new(
                ErrorCode::BackupManifestInvalid,
                format!(
                    "backup artifact {} has a byte length that does not match its manifest",
                    pin.relative_path
                ),
            ));
        }
    }
    Ok(())
}

async fn verify_manifest_snapshot(
    backup_dir: &Path,
    manifest: &BackupManifest,
) -> Result<(), MaintenanceError> {
    // The manifest digest detects accidental changes to the manifest body, but
    // it is not a signature. Cross-check every operational field against the
    // snapshot so a self-consistent manifest cannot omit or invent metadata.
    // StorePaths builds the SQLite URL from this caller path; keep the original
    // path form here because Windows drive separators are not URL escapes.
    let snapshot = SqliteStore::open_read_only(backup_dir).await?;
    let artifacts = read_artifact_pins(&snapshot).await?;
    let schema_revisions = read_schema_revisions(&snapshot).await?;
    let tombstones = read_tombstone_ids(&snapshot).await?;
    let pins = read_retention_pins(&snapshot).await?;
    snapshot.close().await?;

    for (matches, description) in [
        (artifacts == manifest.artifacts, "artifact rows"),
        (
            schema_revisions == manifest.schema_revisions,
            "schema revisions",
        ),
        (tombstones == manifest.tombstones, "tombstone identities"),
        (pins == manifest.pins, "retention pins"),
    ] {
        if !matches {
            return Err(MaintenanceError::new(
                ErrorCode::BackupManifestInvalid,
                format!("the backup manifest {description} do not match its database snapshot"),
            ));
        }
    }
    Ok(())
}

/// Restore a backup into a **new** data directory and validate it.
///
/// The destination must not already hold a store. Restoring never activates the
/// result: activation is a separate explicit maintenance action.
pub async fn restore_backup(
    backup_dir: impl AsRef<Path>,
    destination: impl AsRef<Path>,
) -> Result<RestoreOutcome, MaintenanceError> {
    let backup_dir = std::fs::canonicalize(backup_dir.as_ref()).map_err(|error| {
        MaintenanceError::new(
            ErrorCode::BackupManifestInvalid,
            format!("cannot resolve backup directory: {error}"),
        )
    })?;
    let destination = destination.as_ref().to_path_buf();
    let manifest = verify_backup(&backup_dir).await?;
    ensure_new_restore_destination(&destination)?;

    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    ensure_new_restore_destination(&destination)?;

    // Build beside the final path so every failure cleans up its partial copy.
    // Publishing happens only after SQLite, artifacts, and provenance validate.
    let staging_path = parent.join(format!(".harness-restore-{}", EventId::generate()));
    std::fs::create_dir(&staging_path)?;
    let mut staging = RestoreStagingDir {
        path: staging_path,
        published: false,
    };
    let staging_paths = StorePaths::new(&staging.path);

    // Copy the snapshot, then let SQLite validate it before trusting it.
    let snapshot_source =
        resolve_contained_file(&backup_dir, &manifest.database_file, "backup database")?;
    std::fs::copy(&snapshot_source, &staging_paths.database_path)?;

    let database_verified = verify_snapshot_integrity(&staging_paths.database_path).await?;

    let mut artifacts_verified = 0usize;
    let mut missing = Vec::new();
    let mut corrupt = Vec::new();
    for pin in &manifest.artifacts {
        let source = resolve_contained_file(&backup_dir, &pin.relative_path, "backup artifact")?;
        match std::fs::read(&source) {
            Ok(bytes) => {
                if ContentHash::from_bytes(&bytes) == pin.content_hash {
                    let target = staging.path.join(&pin.relative_path);
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
    let provenance = serde_json::to_vec_pretty(&provenance).map_err(|error| {
        MaintenanceError::new(
            ErrorCode::InvalidPayload,
            format!("cannot serialize restore provenance: {error}"),
        )
    })?;
    std::fs::write(staging.path.join("restore.json"), provenance)?;
    ensure_new_restore_destination(&destination)?;
    std::fs::rename(&staging.path, &destination)?;
    staging.published = true;

    Ok(RestoreOutcome {
        destination: destination.to_string_lossy().into_owned(),
        report,
    })
}

/// Resolve a manifest-controlled relative file only when its canonical target
/// remains inside the selected directory. Canonicalizing the returned path
/// prevents a symlink/reparse point from redirecting a later read elsewhere.
fn resolve_contained_file(
    directory: &Path,
    relative_path: &str,
    description: &str,
) -> Result<PathBuf, MaintenanceError> {
    crate::contracts::validate_relative_path(description, relative_path)?;
    let root = std::fs::canonicalize(directory).map_err(|error| {
        MaintenanceError::new(
            ErrorCode::BackupManifestInvalid,
            format!("cannot resolve {description} root: {error}"),
        )
    })?;
    let candidate = root.join(relative_path);
    let resolved = std::fs::canonicalize(&candidate).map_err(|error| {
        MaintenanceError::new(
            ErrorCode::BackupManifestInvalid,
            format!(
                "cannot resolve {description} at {}: {error}",
                candidate.display()
            ),
        )
    })?;
    if !resolved.starts_with(&root)
        || !std::fs::metadata(&resolved).is_ok_and(|metadata| metadata.is_file())
    {
        return Err(MaintenanceError::new(
            ErrorCode::BackupManifestInvalid,
            format!("{description} must be a regular file inside the selected directory"),
        ));
    }
    Ok(resolved)
}

fn ensure_new_restore_destination(destination: &Path) -> Result<(), MaintenanceError> {
    match std::fs::symlink_metadata(destination) {
        Ok(_) => Err(MaintenanceError::new(
            ErrorCode::RestoreTargetConflict,
            format!(
                "{} already exists; restore requires a new directory",
                destination.display()
            ),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

struct RestoreStagingDir {
    path: PathBuf,
    published: bool,
}

impl Drop for RestoreStagingDir {
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
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
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .create_if_missing(false);
    let mut connection = SqliteConnection::connect_with(&options)
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
