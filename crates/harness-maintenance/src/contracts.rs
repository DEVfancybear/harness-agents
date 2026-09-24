//! P7 maintenance contracts: the release matrix, backup manifests, retention
//! classes and the maintenance journal.
//!
//! These contracts are deliberately explicit about what is *not* claimed. A
//! benchmark target is a target until it is measured, a platform that was never
//! exercised is reported as unverified, and `invalidate` is never conflated with
//! `delete`.

use std::collections::BTreeMap;
use std::fmt;

use harness_types::{ContentHash, ErrorCode, TaskId};
use serde::{Deserialize, Serialize};

/// Serialization revision for every P7 maintenance artifact.
pub const MAINTENANCE_CONTRACT_VERSION: u16 = 1;

/// Default grace period before an unreferenced artifact is collectable.
pub const DEFAULT_GC_GRACE_SECONDS: u64 = 7 * 24 * 60 * 60;

/// The backup manifest file name inside a backup directory.
pub const BACKUP_MANIFEST_NAME: &str = "backup-manifest.json";

/// Database snapshot file name inside a backup directory.
pub const BACKUP_DATABASE_NAME: &str = "harness.sqlite3";

/// A typed maintenance failure.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct MaintenanceError {
    code: ErrorCode,
    message: String,
}

impl MaintenanceError {
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl From<harness_store_sqlite::StoreError> for MaintenanceError {
    fn from(error: harness_store_sqlite::StoreError) -> Self {
        Self::new(error.code(), error.to_string())
    }
}

impl From<harness_types::HarnessError> for MaintenanceError {
    fn from(error: harness_types::HarnessError) -> Self {
        Self::new(error.code(), error.to_string())
    }
}

impl From<std::io::Error> for MaintenanceError {
    fn from(error: std::io::Error) -> Self {
        Self::new(ErrorCode::StorageWriteFailed, error.to_string())
    }
}

/// How completely one platform was exercised.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformStatus {
    /// The full gate ran and passed on this platform.
    Verified,
    /// The gate has not been run here.
    Unverified,
    /// The gate ran and failed here.
    Failing,
}

impl PlatformStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Unverified => "unverified",
            Self::Failing => "failing",
        }
    }
}

/// One supported platform and its honest status.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformSupport {
    pub os: String,
    pub target_triple: String,
    pub toolchain: String,
    pub status: PlatformStatus,
    /// What was actually run here, or why it was not.
    pub evidence: String,
}

/// A capability an operator needs to know about before relying on it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityStatus {
    Supported,
    /// Implemented but not exercised end to end on this build.
    ComponentOnly,
    Unsupported,
}

impl CapabilityStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::ComponentOnly => "component_only",
            Self::Unsupported => "unsupported",
        }
    }
}

/// One capability and the truth about it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilitySupport {
    pub name: String,
    pub status: CapabilityStatus,
    pub note: String,
}

/// A performance target. `measured` is `None` until a real run produced it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkTarget {
    pub name: String,
    pub description: String,
    pub unit: String,
    /// The stated target, which is not a result.
    pub target: u64,
    /// The observed value, or `None` when the benchmark was not measured.
    pub measured: Option<u64>,
}

impl BenchmarkTarget {
    #[must_use]
    pub const fn is_measured(&self) -> bool {
        self.measured.is_some()
    }

    /// A target is only "achieved" when it was measured and met. An unmeasured
    /// target is never reported as achieved.
    #[must_use]
    pub fn met(&self) -> Option<bool> {
        self.measured.map(|value| value <= self.target)
    }
}

/// The release matrix: what the release supports, what it does not, and what
/// still has to be measured.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseMatrix {
    pub schema_version: u16,
    pub release_name: String,
    pub platforms: Vec<PlatformSupport>,
    pub capabilities: Vec<CapabilitySupport>,
    pub benchmarks: Vec<BenchmarkTarget>,
    /// Acceptance cases that were exercised end to end.
    pub verified_cases: Vec<String>,
    /// Checks that were not run, with the reason.
    pub unverified_checks: Vec<String>,
    /// Things this release explicitly does not do.
    pub out_of_scope: Vec<String>,
}

impl ReleaseMatrix {
    /// Structural validation. It refuses a matrix that would hide a failing or
    /// unverified platform behind a green summary.
    pub fn validate(&self) -> Result<(), MaintenanceError> {
        if self.schema_version != MAINTENANCE_CONTRACT_VERSION {
            return Err(MaintenanceError::new(
                ErrorCode::UnsupportedSchemaVersion,
                format!(
                    "release matrix schema {} is not supported",
                    self.schema_version
                ),
            ));
        }
        if self.release_name.trim().is_empty() {
            return Err(MaintenanceError::new(
                ErrorCode::InvalidPayload,
                "a release matrix requires a release name",
            ));
        }
        if self.platforms.is_empty() {
            return Err(MaintenanceError::new(
                ErrorCode::InvalidPayload,
                "a release matrix requires at least one platform",
            ));
        }
        for platform in &self.platforms {
            if platform.os.trim().is_empty() || platform.target_triple.trim().is_empty() {
                return Err(MaintenanceError::new(
                    ErrorCode::InvalidPayload,
                    "every platform entry requires an os and a target triple",
                ));
            }
            if platform.evidence.trim().is_empty() {
                return Err(MaintenanceError::new(
                    ErrorCode::InvalidPayload,
                    format!(
                        "platform {} must state what was actually run",
                        platform.target_triple
                    ),
                ));
            }
        }
        for benchmark in &self.benchmarks {
            if benchmark.name.trim().is_empty() || benchmark.unit.trim().is_empty() {
                return Err(MaintenanceError::new(
                    ErrorCode::InvalidPayload,
                    "every benchmark requires a name and a unit",
                ));
            }
        }
        Ok(())
    }

    /// Platforms whose required gate is not green.
    #[must_use]
    pub fn not_green(&self) -> Vec<&PlatformSupport> {
        self.platforms
            .iter()
            .filter(|platform| platform.status != PlatformStatus::Verified)
            .collect()
    }

    /// Whether every stated benchmark target has a measurement.
    #[must_use]
    pub fn unmeasured_benchmarks(&self) -> Vec<&BenchmarkTarget> {
        self.benchmarks
            .iter()
            .filter(|benchmark| !benchmark.is_measured())
            .collect()
    }

    /// The honest one-line verdict. A single failing platform means the release
    /// is not fully verified, and an unmeasured target is never claimed.
    #[must_use]
    pub fn verdict(&self) -> String {
        let failing = self
            .platforms
            .iter()
            .filter(|platform| platform.status == PlatformStatus::Failing)
            .count();
        let unverified = self
            .platforms
            .iter()
            .filter(|platform| platform.status == PlatformStatus::Unverified)
            .count();
        let unmeasured = self.unmeasured_benchmarks().len();
        if failing > 0 {
            return format!("not release-ready: {failing} platform(s) failing");
        }
        if unverified > 0 {
            // Both facts are stated: an unverified platform must not hide that the
            // benchmark targets were not measured either.
            return if unmeasured > 0 {
                format!(
                    "partially verified: {unverified} platform(s) unverified; {unmeasured} benchmark(s) unmeasured"
                )
            } else {
                format!("partially verified: {unverified} platform(s) unverified")
            };
        }
        if unmeasured > 0 {
            return format!("verified on all platforms; {unmeasured} benchmark(s) unmeasured");
        }
        "verified on all platforms with every benchmark measured".to_owned()
    }
}

/// One artifact referenced by the store at backup time.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactPin {
    pub artifact_id: String,
    pub relative_path: String,
    pub content_hash: ContentHash,
    pub byte_len: u64,
}

/// What a backup pinned so a later garbage collection cannot drop it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionPin {
    pub reason: String,
    pub task_id: Option<TaskId>,
}

/// The versioned backup manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BackupManifest {
    pub schema_version: u16,
    pub created_unix_ms: u64,
    /// The source data directory this backup was taken from.
    pub source_data_dir: String,
    /// Schema revisions present in the snapshot.
    pub schema_revisions: BTreeMap<String, i64>,
    pub database_file: String,
    pub database_hash: ContentHash,
    pub database_byte_len: u64,
    pub artifacts: Vec<ArtifactPin>,
    pub pins: Vec<RetentionPin>,
    /// Tombstones active at backup time, so a restore cannot resurrect them.
    pub tombstones: Vec<String>,
    /// Digest over every other field, so tampering is detectable.
    pub manifest_hash: ContentHash,
}

impl BackupManifest {
    /// Recompute the digest over the manifest body.
    pub fn compute_hash(&self) -> Result<ContentHash, MaintenanceError> {
        let mut body = self.clone();
        body.manifest_hash = ContentHash::from_bytes(b"placeholder");
        let value = serde_json::to_value(&body).map_err(|_| {
            MaintenanceError::new(ErrorCode::InvalidPayload, "manifest is not serializable")
        })?;
        Ok(ContentHash::from_canonical_json(&value)?)
    }

    /// Validate structure and integrity. A manifest that does not describe a
    /// complete, self-consistent backup is refused.
    pub fn validate(&self) -> Result<(), MaintenanceError> {
        if self.schema_version != MAINTENANCE_CONTRACT_VERSION {
            return Err(MaintenanceError::new(
                ErrorCode::UnsupportedSchemaVersion,
                format!(
                    "backup manifest schema {} is not supported",
                    self.schema_version
                ),
            ));
        }
        if self.source_data_dir.trim().is_empty() {
            return Err(MaintenanceError::new(
                ErrorCode::BackupManifestInvalid,
                "a backup manifest requires the source data directory",
            ));
        }
        if self.database_file.trim().is_empty() {
            return Err(MaintenanceError::new(
                ErrorCode::BackupManifestInvalid,
                "a backup manifest requires the database file name",
            ));
        }
        // A manifest is untrusted input: `verify_backup` and `restore_backup`
        // join these names under the backup directory, so an absolute path or a
        // `..` component would read or write outside it. Every name must stay a
        // plain relative path.
        validate_relative_path("database_file", &self.database_file)?;
        for pin in &self.artifacts {
            validate_relative_path("artifact path", &pin.relative_path)?;
        }
        if self.schema_revisions.is_empty() {
            return Err(MaintenanceError::new(
                ErrorCode::BackupManifestInvalid,
                "a backup manifest must record the schema revisions it contains",
            ));
        }
        let expected = self.compute_hash()?;
        if expected != self.manifest_hash {
            return Err(MaintenanceError::new(
                ErrorCode::BackupManifestInvalid,
                "the backup manifest digest does not match its contents",
            ));
        }
        Ok(())
    }
}

/// Refuse any manifest-supplied path that could leave the backup directory.
pub(crate) fn validate_relative_path(field: &str, value: &str) -> Result<(), MaintenanceError> {
    let path = std::path::Path::new(value);
    let escapes = path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        });
    if value.trim().is_empty() || escapes {
        return Err(MaintenanceError::new(
            ErrorCode::BackupManifestInvalid,
            format!("{field} must be a relative path inside the backup directory: {value}"),
        ));
    }
    Ok(())
}

/// What a restore validated before it is allowed to activate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RestoreReport {
    pub restored_into: String,
    pub database_verified: bool,
    pub artifacts_verified: usize,
    pub artifacts_missing: Vec<String>,
    pub artifacts_corrupt: Vec<String>,
    pub schema_revisions: BTreeMap<String, i64>,
    pub tombstones_restored: usize,
    /// A restore never activates on its own.
    pub activated: bool,
}

impl RestoreReport {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.database_verified
            && self.artifacts_missing.is_empty()
            && self.artifacts_corrupt.is_empty()
    }
}

/// The retention class applied to content. They are deliberately separate
/// operations: invalidation keeps the record, forgetting removes it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionAction {
    /// Mark derived knowledge unusable while retaining history.
    Invalidate,
    /// Move content out of active use while keeping it restorable.
    Archive,
    /// Remove content and record a tombstone.
    Forget,
}

impl RetentionAction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Invalidate => "invalidate",
            Self::Archive => "archive",
            Self::Forget => "forget",
        }
    }

    /// Only forgetting removes content, so only forgetting needs confirmation.
    #[must_use]
    pub const fn requires_confirmation(self) -> bool {
        matches!(self, Self::Forget)
    }
}

impl fmt::Display for RetentionAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A durable tombstone. It records that a source was deliberately forgotten and
/// blocks a later extraction pass from bringing the content back.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Tombstone {
    pub tombstone_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub reason: String,
    pub created_unix_ms: u64,
    /// External or backup copies that may still contain the data.
    pub surviving_copies: Vec<String>,
}

/// The outcome of a retention operation, including what the operator must know.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetentionReport {
    pub action: RetentionAction,
    pub target: String,
    pub affected_assets: Vec<String>,
    pub derived_invalidated: usize,
    pub tombstone_id: Option<String>,
    pub surviving_copies: Vec<String>,
}

/// One garbage-collection candidate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GcCandidate {
    pub artifact_id: String,
    pub relative_path: String,
    pub byte_len: u64,
    pub unreferenced: bool,
    pub pinned: bool,
    pub age_seconds: u64,
}

impl GcCandidate {
    /// An artifact is collectable only when nothing references it, nothing pins
    /// it and the grace period has elapsed.
    #[must_use]
    pub const fn collectable(&self, grace_seconds: u64) -> bool {
        self.unreferenced && !self.pinned && self.age_seconds >= grace_seconds
    }
}

/// What one garbage-collection pass did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GcReport {
    pub considered: usize,
    pub collected: Vec<String>,
    pub retained_pinned: Vec<String>,
    pub retained_referenced: Vec<String>,
    pub retained_young: Vec<String>,
    pub bytes_reclaimed: u64,
}

impl GcReport {
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "considered {} artifacts: collected {} ({} bytes), pinned {}, referenced {}, within grace {}",
            self.considered,
            self.collected.len(),
            self.bytes_reclaimed,
            self.retained_pinned.len(),
            self.retained_referenced.len(),
            self.retained_young.len()
        )
    }
}

/// The current wall-clock time in milliseconds, used for manifests and ages.
#[must_use]
pub fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}
