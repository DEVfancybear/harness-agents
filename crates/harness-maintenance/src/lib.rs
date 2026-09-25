#![forbid(unsafe_code)]

//! P7 recovery hardening and release support: the release matrix, consistent
//! backup with verified restore, upgrade safeguards, retention pins,
//! tombstones and artifact garbage collection.
//!
//! This crate composes the existing stores and services. It is not a second
//! database authority: every durable write still goes through the P1
//! transaction coordinator and host fence.

pub mod backup;
pub mod contracts;
pub mod diagnostics;
pub mod migration;
pub mod retention;

pub use backup::{BackupOutcome, RestoreOutcome, create_backup, restore_backup, verify_backup};
pub use contracts::{
    ArtifactPin, BACKUP_DATABASE_NAME, BACKUP_MANIFEST_NAME, BackupManifest, BenchmarkTarget,
    CapabilityStatus, CapabilitySupport, DEFAULT_GC_GRACE_SECONDS, GcCandidate, GcReport,
    MAINTENANCE_CONTRACT_VERSION, MaintenanceError, PlatformStatus, PlatformSupport, ReleaseMatrix,
    RestoreReport, RetentionPin, Tombstone, now_unix_ms,
};
pub use diagnostics::{
    BundleFile, MAX_CORRELATION_REFS, MAX_FIELD_CHARS, REDACTED, SupportBundle,
    build_support_bundle, clip, is_secret_name, looks_like_a_secret, redact_field, redact_value,
    reproducible_commands,
};
pub use migration::{
    MigrationOutcome, StoreCompatibility, check_store_compatibility, migrate_copy,
    store_is_initialized,
};
pub use retention::{
    collect_garbage, default_grace_seconds, list_tombstones, retention_summary, verify_artifact,
};
