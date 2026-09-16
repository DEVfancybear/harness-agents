#![forbid(unsafe_code)]

//! P7 recovery hardening and release support: the release matrix, consistent
//! backup with verified restore, upgrade safeguards, retention classes,
//! tombstones and artifact garbage collection.
//!
//! This crate composes the existing stores and services. It is not a second
//! database authority: every durable write still goes through the P1
//! transaction coordinator and host fence, and `invalidate` is never conflated
//! with `delete`.

pub mod backup;
pub mod contracts;
pub mod migration;
pub mod retention;

pub use backup::{BackupOutcome, RestoreOutcome, create_backup, restore_backup, verify_backup};
pub use contracts::{
    ArtifactPin, BACKUP_DATABASE_NAME, BACKUP_MANIFEST_NAME, BackupManifest, BenchmarkTarget,
    CapabilityStatus, CapabilitySupport, DEFAULT_GC_GRACE_SECONDS, GcCandidate, GcReport,
    MAINTENANCE_CONTRACT_VERSION, MaintenanceError, PlatformStatus, PlatformSupport, ReleaseMatrix,
    RestoreReport, RetentionAction, RetentionPin, RetentionReport, Tombstone, now_unix_ms,
};
pub use migration::{
    MigrationOutcome, StoreCompatibility, check_store_compatibility, migrate_copy,
};
pub use retention::{
    collect_garbage, default_grace_seconds, forget_source, list_tombstones, retention_summary,
    run_retention, verify_artifact,
};
