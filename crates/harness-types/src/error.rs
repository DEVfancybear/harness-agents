use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Stable machine-readable error categories used by P0 contracts and the CLI.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidHash,
    InvalidId,
    InvalidPayload,
    InvalidSequence,
    MissingAuthority,
    UnsupportedSchemaVersion,
    ConfigReadError,
    ConfigParseError,
    ConfigUnknownField,
    FixtureIntegrity,
    GateConfigurationError,
    GateRequiredTestIgnored,
    GateTestDiscoveryEmpty,
    WriterLocked,
    StaleWriter,
    ReadOnlyStore,
    SequenceConflict,
    IdempotencyConflict,
    TaskLeaseConflict,
    StorageOpenFailed,
    StorageWriteFailed,
    MigrationFailed,
    SnapshotCorrupt,
    UnknownCriticalEvent,
    ArtifactWriteFailed,
    MissingRequiredService,
    IncompatibleService,
    PluginCycle,
    DuplicateRegistration,
    ServiceUnavailable,
    ShutdownFailed,
}

impl ErrorCode {
    /// The stable serialized spelling for diagnostics and scripting.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidHash => "invalid_hash",
            Self::InvalidId => "invalid_id",
            Self::InvalidPayload => "invalid_payload",
            Self::InvalidSequence => "invalid_sequence",
            Self::MissingAuthority => "missing_authority",
            Self::UnsupportedSchemaVersion => "unsupported_schema_version",
            Self::ConfigReadError => "config_read_error",
            Self::ConfigParseError => "config_parse_error",
            Self::ConfigUnknownField => "config_unknown_field",
            Self::FixtureIntegrity => "fixture_integrity",
            Self::GateConfigurationError => "gate_configuration_error",
            Self::GateRequiredTestIgnored => "gate_required_test_ignored",
            Self::GateTestDiscoveryEmpty => "gate_test_discovery_empty",
            Self::WriterLocked => "writer_locked",
            Self::StaleWriter => "stale_writer",
            Self::ReadOnlyStore => "read_only_store",
            Self::SequenceConflict => "sequence_conflict",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::TaskLeaseConflict => "task_lease_conflict",
            Self::StorageOpenFailed => "storage_open_failed",
            Self::StorageWriteFailed => "storage_write_failed",
            Self::MigrationFailed => "migration_failed",
            Self::SnapshotCorrupt => "snapshot_corrupt",
            Self::UnknownCriticalEvent => "unknown_critical_event",
            Self::ArtifactWriteFailed => "artifact_write_failed",
            Self::MissingRequiredService => "missing_required_service",
            Self::IncompatibleService => "incompatible_service",
            Self::PluginCycle => "plugin_cycle",
            Self::DuplicateRegistration => "duplicate_registration",
            Self::ServiceUnavailable => "service_unavailable",
            Self::ShutdownFailed => "shutdown_failed",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// An error whose code can be used without parsing prose.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("{code}: {message}")]
pub struct HarnessError {
    code: ErrorCode,
    message: String,
}

impl HarnessError {
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
}
