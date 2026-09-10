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
    InvalidStateTransition,
    MandatoryContextOverflow,
    ProviderCanceled,
    ProviderProtocol,
    RetryExhausted,
    CompactionConflict,
    RuntimeCommandConflict,
    RuntimeBlocked,
    PolicyDenied,
    ApprovalRequired,
    ApprovalStale,
    ApprovalConsumed,
    ApprovalRevoked,
    WorkspaceEscape,
    SensitivePathDenied,
    StaleWorkspace,
    UnsupportedTextEncoding,
    BinaryContentDenied,
    OutputLimitExceeded,
    ProcessTimedOut,
    ProcessCanceled,
    ProcessOutcomeUnknown,
    StrictIsolationUnavailable,
    ProjectIdentityConflict,
    ToolIntentConflict,
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
            Self::InvalidStateTransition => "invalid_state_transition",
            Self::MandatoryContextOverflow => "mandatory_context_overflow",
            Self::ProviderCanceled => "provider_canceled",
            Self::ProviderProtocol => "provider_protocol",
            Self::RetryExhausted => "retry_exhausted",
            Self::CompactionConflict => "compaction_conflict",
            Self::RuntimeCommandConflict => "runtime_command_conflict",
            Self::RuntimeBlocked => "runtime_blocked",
            Self::PolicyDenied => "policy_denied",
            Self::ApprovalRequired => "approval_required",
            Self::ApprovalStale => "approval_stale",
            Self::ApprovalConsumed => "approval_consumed",
            Self::ApprovalRevoked => "approval_revoked",
            Self::WorkspaceEscape => "workspace_escape",
            Self::SensitivePathDenied => "sensitive_path_denied",
            Self::StaleWorkspace => "stale_workspace",
            Self::UnsupportedTextEncoding => "unsupported_text_encoding",
            Self::BinaryContentDenied => "binary_content_denied",
            Self::OutputLimitExceeded => "output_limit_exceeded",
            Self::ProcessTimedOut => "process_timed_out",
            Self::ProcessCanceled => "process_canceled",
            Self::ProcessOutcomeUnknown => "process_outcome_unknown",
            Self::StrictIsolationUnavailable => "strict_isolation_unavailable",
            Self::ProjectIdentityConflict => "project_identity_conflict",
            Self::ToolIntentConflict => "tool_intent_conflict",
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
