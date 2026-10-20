//! Retention pins, tombstones and artifact garbage collection.

use std::collections::BTreeSet;
use std::path::Path;

use harness_store_sqlite::{SqliteStore, StorePaths, TombstoneRow};
use harness_types::{ContentHash, ErrorCode, TaskId};

use crate::contracts::{
    DEFAULT_GC_GRACE_SECONDS, GcCandidate, GcReport, MaintenanceError, now_unix_ms,
};

/// Every tombstone, which is what a restore must preserve.
pub async fn list_tombstones(store: &SqliteStore) -> Result<Vec<TombstoneRow>, MaintenanceError> {
    Ok(store.tombstones().await?)
}

/// A short retention summary for `doctor`.
pub async fn retention_summary(store: &SqliteStore) -> Result<serde_json::Value, MaintenanceError> {
    let tombstones = store.tombstones().await?;
    let pins = store.retention_pins().await?;
    let pinned_artifacts = store.pinned_artifact_ids().await?;
    Ok(serde_json::json!({
        "schema_version": 1,
        "tombstones": tombstones.len(),
        "pins": pins.len(),
        "pinned_artifacts": pinned_artifacts.len(),
        "surviving_copies": tombstones
            .iter()
            .flat_map(|tombstone| tombstone.surviving_copies.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>(),
    }))
}

/// Collect unreferenced artifacts after the grace period.
///
/// An artifact is removed only when it is unreferenced, unpinned and older than
/// the grace period. The candidate snapshot is advisory: collection rechecks
/// pins and references in the same writer transaction that deletes the row,
/// quarantines the file until commit, then unlinks the quarantined bytes.
pub async fn collect_garbage(
    store: &SqliteStore,
    grace_seconds: u64,
    dry_run: bool,
) -> Result<GcReport, MaintenanceError> {
    let candidates = gc_candidates(store, grace_seconds).await?;
    let mut report = GcReport {
        considered: candidates.len(),
        collected: Vec::new(),
        retained_pinned: Vec::new(),
        retained_referenced: Vec::new(),
        retained_young: Vec::new(),
        bytes_reclaimed: 0,
    };
    for candidate in candidates {
        if candidate.pinned {
            report.retained_pinned.push(candidate.artifact_id);
            continue;
        }
        if !candidate.unreferenced {
            report.retained_referenced.push(candidate.artifact_id);
            continue;
        }
        if candidate.age_seconds < grace_seconds {
            report.retained_young.push(candidate.artifact_id);
            continue;
        }
        if !dry_run {
            // The candidate list is a snapshot. The store rechecks holds and
            // file age inside its writer transaction, then quarantines bytes
            // until row deletion commits.
            if !store
                .collect_artifact(&candidate.artifact_id, grace_seconds)
                .await?
            {
                if let Some(current) = gc_candidates(store, grace_seconds)
                    .await?
                    .into_iter()
                    .find(|current| current.artifact_id == candidate.artifact_id)
                {
                    if current.pinned {
                        report.retained_pinned.push(candidate.artifact_id);
                    } else if !current.unreferenced {
                        report.retained_referenced.push(candidate.artifact_id);
                    } else {
                        report.retained_young.push(candidate.artifact_id);
                    }
                }
                continue;
            }
        }
        report.bytes_reclaimed = report.bytes_reclaimed.saturating_add(candidate.byte_len);
        report.collected.push(candidate.artifact_id);
    }
    Ok(report)
}

/// The collection candidates, with the reason each one is or is not collectable.
pub async fn gc_candidates(
    store: &SqliteStore,
    _grace_seconds: u64,
) -> Result<Vec<GcCandidate>, MaintenanceError> {
    let pins: BTreeSet<String> = store.pinned_artifact_ids().await?.into_iter().collect();
    let referenced: BTreeSet<String> = store.referenced_artifact_ids().await?.into_iter().collect();
    let paths = StorePaths::new(store.paths().data_dir.clone());
    let now = now_unix_ms();
    let mut candidates = Vec::new();
    for (artifact_id, relative_path, content_hash, byte_len) in store.artifact_pins().await? {
        let path = paths.data_dir.join(&relative_path);
        let metadata = std::fs::metadata(&path).ok();
        let age_seconds = metadata
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |duration| {
                let modified_unix_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
                now.saturating_sub(modified_unix_ms) / 1000
            });
        let _ = content_hash;
        // Receipts and tool scopes are the durable rows that hold artifact IDs.
        candidates.push(GcCandidate {
            artifact_id: artifact_id.clone(),
            relative_path,
            byte_len,
            unreferenced: !referenced.contains(&artifact_id),
            pinned: pins.contains(&artifact_id),
            age_seconds,
        });
    }
    Ok(candidates)
}

/// Pin the artifacts an unfinished task depends on, so GC leaves them alone.
pub async fn pin_unfinished_work(
    store: &SqliteStore,
    artifact_ids: &[String],
    task_id: &TaskId,
) -> Result<usize, MaintenanceError> {
    store
        .pin_artifacts(
            artifact_ids,
            "unfinished_work",
            Some(task_id),
            now_unix_ms(),
        )
        .await
        .map_err(MaintenanceError::from)
}

/// Pin artifacts for a backup, so a concurrent GC cannot remove them.
pub async fn pin_backup(
    store: &SqliteStore,
    artifact_ids: &[String],
    backup_label: &str,
) -> Result<usize, MaintenanceError> {
    store
        .pin_artifacts(artifact_ids, backup_label, None, now_unix_ms())
        .await
        .map_err(MaintenanceError::from)
}

/// Verify that an artifact file still matches its recorded hash.
pub fn verify_artifact(
    data_dir: impl AsRef<Path>,
    relative_path: &str,
    expected: &ContentHash,
) -> Result<bool, MaintenanceError> {
    let path = data_dir.as_ref().join(relative_path);
    let bytes = std::fs::read(&path).map_err(|error| {
        MaintenanceError::new(
            ErrorCode::ArtifactWriteFailed,
            format!("cannot read {}: {error}", path.display()),
        )
    })?;
    Ok(&ContentHash::from_bytes(&bytes) == expected)
}

/// The default grace period, exposed so the CLI and docs agree.
#[must_use]
pub const fn default_grace_seconds() -> u64 {
    DEFAULT_GC_GRACE_SECONDS
}
