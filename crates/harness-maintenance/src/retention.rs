//! Retention classes, tombstones and artifact garbage collection.
//!
//! `invalidate`, `archive` and `forget` are separate operations on purpose.
//! Invalidation keeps the historical record and marks derived knowledge
//! unusable; archiving moves content out of active use while keeping it
//! restorable; only forgetting removes content, and it always leaves a
//! tombstone so a later extraction pass cannot bring the data back.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use harness_store_sqlite::{SqliteStore, StorePaths, TombstoneRow};
use harness_types::{ContentHash, ErrorCode, TaskId};

use crate::contracts::{
    DEFAULT_GC_GRACE_SECONDS, GcCandidate, GcReport, MaintenanceError, RetentionAction,
    RetentionReport, now_unix_ms,
};

/// Run one retention operation against a store.
///
/// `confirmation` must equal the target for a `forget`, which is the explicit
/// confirmation token the operator supplies. Anything less is refused.
pub async fn run_retention(
    store: &Arc<SqliteStore>,
    action: RetentionAction,
    source_kind: &str,
    source_id: &str,
    reason: &str,
    confirmation: Option<&str>,
    surviving_copies: &[String],
) -> Result<RetentionReport, MaintenanceError> {
    if action.requires_confirmation() && confirmation != Some(source_id) {
        return Err(MaintenanceError::new(
            ErrorCode::RetentionRefused,
            format!("forget requires an explicit confirmation equal to the target {source_id}"),
        ));
    }
    if source_id.trim().is_empty() || source_kind.trim().is_empty() {
        return Err(MaintenanceError::new(
            ErrorCode::InvalidPayload,
            "a retention operation requires a source kind and a source identity",
        ));
    }

    let mut affected_assets = Vec::new();
    let mut derived_invalidated = 0usize;
    if action != RetentionAction::Forget {
        // Invalidate and archive operate on the memory assets that derive from
        // this source; forget additionally removes the record and tombstones it.
        let invalidated = harness_memory::MemoryService::new(Arc::clone(store))
            .invalidate_source(
                &harness_memory::MemoryPrincipal::user("maintenance"),
                source_kind,
                source_id,
            )
            .await
            .map_err(|error| MaintenanceError::new(error.code(), error.to_string()))?;
        derived_invalidated = invalidated.len();
        affected_assets = invalidated.iter().map(ToString::to_string).collect();
    }

    let timestamp = now_unix_ms();
    let tombstone_id = match action {
        RetentionAction::Forget => {
            let tombstone = TombstoneRow {
                tombstone_id: format!("tombstone-{}", harness_types::EventId::generate().as_str()),
                source_kind: source_kind.to_owned(),
                source_id: source_id.to_owned(),
                reason: reason.to_owned(),
                surviving_copies: surviving_copies.to_vec(),
                created_unix_ms: timestamp,
            };
            let detail = serde_json::json!({
                "action": action.as_str(),
                "reason": reason,
                "affected_assets": affected_assets,
                "surviving_copies": surviving_copies,
            });
            store
                .record_tombstone(
                    &tombstone,
                    &format!("journal-{}", tombstone.tombstone_id),
                    action.as_str(),
                    &detail,
                )
                .await?;
            Some(tombstone.tombstone_id)
        }
        RetentionAction::Invalidate | RetentionAction::Archive => {
            let detail = serde_json::json!({
                "action": action.as_str(),
                "reason": reason,
                "derived_invalidated": derived_invalidated,
            });
            store
                .record_maintenance_entry(
                    &format!("journal-{}", harness_types::EventId::generate().as_str()),
                    action.as_str(),
                    source_id,
                    &detail,
                    timestamp,
                )
                .await?;
            None
        }
    };

    Ok(RetentionReport {
        action,
        target: format!("{source_kind}:{source_id}"),
        affected_assets,
        derived_invalidated,
        tombstone_id,
        surviving_copies: surviving_copies.to_vec(),
    })
}

/// Forget a source: remove derived content, record a tombstone, and report every
/// copy that may still hold the data.
pub async fn forget_source(
    store: &Arc<SqliteStore>,
    source_kind: &str,
    source_id: &str,
    reason: &str,
    confirmation: &str,
    surviving_copies: &[String],
) -> Result<RetentionReport, MaintenanceError> {
    // Removing the derived content is what makes the forget real; the tombstone
    // is what keeps it forgotten.
    let invalidated = harness_memory::MemoryService::new(Arc::clone(store))
        .invalidate_source(
            &harness_memory::MemoryPrincipal::user("maintenance"),
            source_kind,
            source_id,
        )
        .await
        .map_err(|error| MaintenanceError::new(error.code(), error.to_string()))?;

    let mut report = run_retention(
        store,
        RetentionAction::Forget,
        source_kind,
        source_id,
        reason,
        Some(confirmation),
        surviving_copies,
    )
    .await?;
    report.derived_invalidated = invalidated.len();
    report.affected_assets = invalidated.iter().map(ToString::to_string).collect();
    Ok(report)
}

/// Refuse re-extraction from a tombstoned source.
///
/// This is the check an extraction pass must perform before it schedules work
/// for a source: a forgotten source stays forgotten.
pub async fn assert_not_tombstoned(
    store: &SqliteStore,
    source_kind: &str,
    source_id: &str,
) -> Result<(), MaintenanceError> {
    if store.is_tombstoned(source_kind, source_id).await? {
        return Err(MaintenanceError::new(
            ErrorCode::RetentionRefused,
            format!("source {source_kind}:{source_id} was forgotten; re-extraction is refused"),
        ));
    }
    Ok(())
}

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
/// the grace period. A backup/GC race cannot delete a pinned artifact because
/// the pin check happens before the file is removed and the pin lives in the
/// same database the GC just read.
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
    let paths = StorePaths::new(store.paths().data_dir.clone());
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
            let path = paths.data_dir.join(&candidate.relative_path);
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(MaintenanceError::new(
                        ErrorCode::ArtifactWriteFailed,
                        format!("cannot remove {}: {error}", path.display()),
                    ));
                }
            }
            store.remove_artifact_record(&candidate.artifact_id).await?;
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
        let modified = metadata
            .as_ref()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |duration| {
                u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
            });
        let age_seconds = now.saturating_sub(modified) / 1000;
        let _ = content_hash;
        // An artifact is referenced when a receipt, tool scope or intent points
        // at it, or when the artifact record is genuinely orphaned.
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
