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
    ContextOverflow,
    ProviderCanceled,
    ProviderProtocol,
    RetryExhausted,
    CompactionConflict,
    RuntimeCommandConflict,
    RuntimeBlocked,
    BlockedByHook,
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
    /// A requested worker role cannot run with the host's current isolation capabilities.
    RoleUnavailable,
    ProjectIdentityConflict,
    ToolIntentConflict,
    EditNotFound,
    EditAmbiguous,
    TaskNotFound,
    TaskNotReady,
    TaskDependencyFailed,
    TaskOwnershipConflict,
    DagCycle,
    UnknownTaskDependency,
    DuplicateTaskId,
    AmbiguousTaskOwner,
    DelegationDepthExceeded,
    BudgetExhausted,
    ScopeAuthorityDenied,
    DirtyWorkspaceDenied,
    IntegrationConflict,
    ResultIncomplete,
    SchedulerShutdown,
    /// A delegation was refused because the queue already holds as many workers
    /// as it may. Distinct from `BudgetExhausted`, which says no more requests may
    /// be spent: a full queue is backpressure and clears when a worker settles,
    /// where an exhausted budget does not clear at all.
    DelegationQueueFull,
    DeliveryConflict,
    ExtensionProtocolError,
    ExtensionDigestMismatch,
    ExtensionProtocolUnsupported,
    ExtensionNotFound,
    ExtensionUntrusted,
    ExtensionCapabilityMismatch,
    HostMethodDenied,
    EnvironmentDenied,
    SecretNotGranted,
    FrameLimitExceeded,
    InflightLimitExceeded,
    DuplicateFrameId,
    SchemaVersionMismatch,
    SkillUnavailable,
    /// A referenced durable source is no longer available to read. Distinct
    /// from a scope refusal: the caller may read it in principle, but the bytes
    /// behind the reference are gone.
    SourceUnavailable,
    ConfigTrustRequired,
    BackupManifestInvalid,
    RestoreTargetConflict,
    RetentionRefused,
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
            Self::ContextOverflow => "context_overflow",
            Self::ProviderCanceled => "provider_canceled",
            Self::ProviderProtocol => "provider_protocol",
            Self::RetryExhausted => "retry_exhausted",
            Self::CompactionConflict => "compaction_conflict",
            Self::RuntimeCommandConflict => "runtime_command_conflict",
            Self::RuntimeBlocked => "runtime_blocked",
            Self::BlockedByHook => "blocked_by_hook",
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
            Self::RoleUnavailable => "role_unavailable",
            Self::ProjectIdentityConflict => "project_identity_conflict",
            Self::ToolIntentConflict => "tool_intent_conflict",
            Self::EditNotFound => "edit_not_found",
            Self::EditAmbiguous => "edit_ambiguous",
            Self::TaskNotFound => "task_not_found",
            Self::TaskNotReady => "task_not_ready",
            Self::TaskDependencyFailed => "task_dependency_failed",
            Self::TaskOwnershipConflict => "task_ownership_conflict",
            Self::DagCycle => "dag_cycle",
            Self::UnknownTaskDependency => "unknown_task_dependency",
            Self::DuplicateTaskId => "duplicate_task_id",
            Self::AmbiguousTaskOwner => "ambiguous_task_owner",
            Self::DelegationDepthExceeded => "delegation_depth_exceeded",
            Self::BudgetExhausted => "budget_exhausted",
            Self::ScopeAuthorityDenied => "scope_authority_denied",
            Self::DirtyWorkspaceDenied => "dirty_workspace_denied",
            Self::IntegrationConflict => "integration_conflict",
            Self::ResultIncomplete => "result_incomplete",
            Self::SchedulerShutdown => "scheduler_shutdown",
            Self::DelegationQueueFull => "delegation_queue_full",
            Self::DeliveryConflict => "delivery_conflict",
            Self::ExtensionProtocolError => "extension_protocol_error",
            Self::ExtensionDigestMismatch => "extension_digest_mismatch",
            Self::ExtensionProtocolUnsupported => "extension_protocol_unsupported",
            Self::ExtensionNotFound => "extension_not_found",
            Self::ExtensionUntrusted => "extension_untrusted",
            Self::ExtensionCapabilityMismatch => "extension_capability_mismatch",
            Self::HostMethodDenied => "host_method_denied",
            Self::EnvironmentDenied => "environment_denied",
            Self::SecretNotGranted => "secret_not_granted",
            Self::FrameLimitExceeded => "frame_limit_exceeded",
            Self::InflightLimitExceeded => "inflight_limit_exceeded",
            Self::DuplicateFrameId => "duplicate_frame_id",
            Self::SchemaVersionMismatch => "schema_version_mismatch",
            Self::SkillUnavailable => "skill_unavailable",
            Self::SourceUnavailable => "source_unavailable",
            Self::ConfigTrustRequired => "config_trust_required",
            Self::BackupManifestInvalid => "backup_manifest_invalid",
            Self::RestoreTargetConflict => "restore_target_conflict",
            Self::RetentionRefused => "retention_refused",
        }
    }
}

/// How a caller may retry after a failure. The class is derived from the
/// stable code, never from the human-readable message.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryClass {
    /// The same input cannot succeed by repeating it.
    Never,
    /// The operation may be repeated; the failure was environmental.
    Transient,
    /// The operation may be repeated inside a bounded retry budget.
    Bounded,
    /// State moved; re-read and rebase before trying again.
    Conflict,
    /// Progress needs a human decision, answer, or grant.
    HumanAction,
}

impl RetryClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::Transient => "transient",
            Self::Bounded => "bounded",
            Self::Conflict => "conflict",
            Self::HumanAction => "human_action",
        }
    }
}

impl fmt::Display for RetryClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl ErrorCode {
    /// The retry class of this failure. Total by construction: adding a code
    /// forces an explicit decision here.
    #[must_use]
    pub const fn retry_class(self) -> RetryClass {
        match self {
            Self::WriterLocked
            | Self::StorageOpenFailed
            | Self::StorageWriteFailed
            | Self::ArtifactWriteFailed
            | Self::ServiceUnavailable
            | Self::ShutdownFailed
            | Self::SchedulerShutdown
            | Self::InflightLimitExceeded => RetryClass::Transient,
            Self::ProviderProtocol | Self::RetryExhausted | Self::ProcessTimedOut => {
                RetryClass::Bounded
            }
            Self::StaleWriter
            | Self::SequenceConflict
            | Self::IdempotencyConflict
            | Self::TaskLeaseConflict
            | Self::DuplicateRegistration
            | Self::InvalidStateTransition
            | Self::CompactionConflict
            | Self::RuntimeCommandConflict
            | Self::ApprovalStale
            | Self::ApprovalConsumed
            | Self::StaleWorkspace
            | Self::ProjectIdentityConflict
            | Self::ToolIntentConflict
            | Self::TaskOwnershipConflict
            | Self::DuplicateTaskId
            | Self::AmbiguousTaskOwner
            | Self::IntegrationConflict
            | Self::DeliveryConflict
            | Self::DuplicateFrameId
            | Self::SchemaVersionMismatch
            | Self::RestoreTargetConflict
            | Self::RetentionRefused => RetryClass::Conflict,
            Self::RuntimeBlocked
            | Self::BlockedByHook
            | Self::ApprovalRequired
            | Self::TaskNotReady
            | Self::ProcessOutcomeUnknown
            | Self::BudgetExhausted
            | Self::DelegationQueueFull
            | Self::DirtyWorkspaceDenied
            | Self::SecretNotGranted
            | Self::ConfigTrustRequired => RetryClass::HumanAction,
            Self::InvalidHash
            | Self::InvalidId
            | Self::InvalidPayload
            | Self::InvalidSequence
            | Self::MissingAuthority
            | Self::UnsupportedSchemaVersion
            | Self::ConfigReadError
            | Self::ConfigParseError
            | Self::ConfigUnknownField
            | Self::FixtureIntegrity
            | Self::GateConfigurationError
            | Self::GateRequiredTestIgnored
            | Self::GateTestDiscoveryEmpty
            | Self::ReadOnlyStore
            | Self::MigrationFailed
            | Self::SnapshotCorrupt
            | Self::UnknownCriticalEvent
            | Self::MissingRequiredService
            | Self::IncompatibleService
            | Self::PluginCycle
            | Self::MandatoryContextOverflow
            | Self::ContextOverflow
            | Self::ProviderCanceled
            | Self::PolicyDenied
            | Self::ApprovalRevoked
            | Self::WorkspaceEscape
            | Self::SensitivePathDenied
            | Self::UnsupportedTextEncoding
            | Self::BinaryContentDenied
            | Self::OutputLimitExceeded
            | Self::ProcessCanceled
            | Self::StrictIsolationUnavailable
            | Self::RoleUnavailable
            | Self::TaskNotFound
            | Self::TaskDependencyFailed
            | Self::DagCycle
            | Self::UnknownTaskDependency
            | Self::DelegationDepthExceeded
            | Self::ScopeAuthorityDenied
            | Self::ResultIncomplete
            | Self::ExtensionProtocolError
            | Self::ExtensionDigestMismatch
            | Self::ExtensionProtocolUnsupported
            | Self::ExtensionNotFound
            | Self::ExtensionUntrusted
            | Self::ExtensionCapabilityMismatch
            | Self::HostMethodDenied
            | Self::EnvironmentDenied
            | Self::FrameLimitExceeded
            | Self::SkillUnavailable
            | Self::SourceUnavailable
            | Self::EditNotFound
            | Self::EditAmbiguous
            | Self::BackupManifestInvalid => RetryClass::Never,
        }
    }

    /// The process exit code the `ha` CLI uses for this failure (CONTRACTS §9):
    /// 2 invalid usage/config, 3 waiting for input or action, 4 execution failed,
    /// 5 ownership/conflict, 130 user cancel, 1 otherwise.
    #[must_use]
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::ProviderCanceled | Self::ProcessCanceled => 130,
            Self::InvalidHash
            | Self::InvalidId
            | Self::InvalidPayload
            | Self::InvalidSequence
            | Self::MissingAuthority
            | Self::UnsupportedSchemaVersion
            | Self::ConfigReadError
            | Self::ConfigParseError
            | Self::ConfigUnknownField
            | Self::ConfigTrustRequired
            | Self::FixtureIntegrity
            | Self::GateConfigurationError
            | Self::GateRequiredTestIgnored
            | Self::GateTestDiscoveryEmpty => 2,
            Self::RuntimeBlocked
            | Self::ApprovalRequired
            | Self::TaskNotReady
            | Self::ProcessOutcomeUnknown
            | Self::BudgetExhausted
            | Self::DelegationQueueFull
            | Self::DirtyWorkspaceDenied
            | Self::SecretNotGranted
            | Self::MandatoryContextOverflow
            | Self::ContextOverflow => 3,
            Self::WriterLocked
            | Self::StaleWriter
            | Self::SequenceConflict
            | Self::IdempotencyConflict
            | Self::TaskLeaseConflict
            | Self::ApprovalStale
            | Self::ApprovalConsumed
            | Self::ApprovalRevoked
            | Self::StaleWorkspace
            | Self::ProjectIdentityConflict
            | Self::ToolIntentConflict
            | Self::TaskOwnershipConflict
            | Self::DuplicateTaskId
            | Self::AmbiguousTaskOwner
            | Self::DeliveryConflict
            | Self::DuplicateFrameId
            | Self::DuplicateRegistration
            | Self::RetentionRefused
            | Self::RestoreTargetConflict => 5,
            Self::StorageOpenFailed
            | Self::StorageWriteFailed
            | Self::MigrationFailed
            | Self::SnapshotCorrupt
            | Self::UnknownCriticalEvent
            | Self::ArtifactWriteFailed
            | Self::ServiceUnavailable
            | Self::ShutdownFailed
            | Self::SchedulerShutdown
            | Self::InvalidStateTransition
            | Self::ProviderProtocol
            | Self::RetryExhausted
            | Self::CompactionConflict
            | Self::RuntimeCommandConflict
            | Self::ProcessTimedOut
            | Self::OutputLimitExceeded
            | Self::StrictIsolationUnavailable
            | Self::RoleUnavailable
            | Self::IntegrationConflict
            | Self::ResultIncomplete
            | Self::InflightLimitExceeded
            | Self::SchemaVersionMismatch
            | Self::BackupManifestInvalid => 4,
            Self::ReadOnlyStore
            | Self::MissingRequiredService
            | Self::IncompatibleService
            | Self::PluginCycle
            | Self::PolicyDenied
            | Self::BlockedByHook
            | Self::WorkspaceEscape
            | Self::SensitivePathDenied
            | Self::UnsupportedTextEncoding
            | Self::BinaryContentDenied
            | Self::TaskNotFound
            | Self::TaskDependencyFailed
            | Self::DagCycle
            | Self::UnknownTaskDependency
            | Self::DelegationDepthExceeded
            | Self::ScopeAuthorityDenied
            | Self::ExtensionProtocolError
            | Self::ExtensionDigestMismatch
            | Self::ExtensionProtocolUnsupported
            | Self::ExtensionNotFound
            | Self::ExtensionUntrusted
            | Self::ExtensionCapabilityMismatch
            | Self::HostMethodDenied
            | Self::EnvironmentDenied
            | Self::FrameLimitExceeded
            | Self::SkillUnavailable
            | Self::SourceUnavailable
            | Self::EditNotFound
            | Self::EditAmbiguous => 1,
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The serializable error contract. Display messages are diagnostics; only the
/// code, retry class and envelope version are protocol.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorReport {
    pub schema_version: u16,
    pub code: ErrorCode,
    pub retry_class: RetryClass,
    /// A message that is safe to show and to log: it never carries credentials,
    /// bearer tokens, or raw endpoint queries.
    pub safe_message: String,
    pub correlation_id: Option<crate::EventId>,
    pub details_ref: Option<String>,
}

impl ErrorReport {
    #[must_use]
    pub fn from_error(
        error: &HarnessError,
        correlation_id: Option<crate::EventId>,
        details_ref: Option<String>,
    ) -> Self {
        Self {
            schema_version: crate::P0_SCHEMA_VERSION,
            code: error.code(),
            retry_class: error.code().retry_class(),
            safe_message: error.message.clone(),
            correlation_id,
            details_ref,
        }
    }

    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        self.code.exit_code()
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

    /// The message this error carries. Messages are diagnostics and must stay
    /// free of credentials; they are never the protocol.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub const fn retry_class(&self) -> RetryClass {
        self.code.retry_class()
    }

    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        self.code.exit_code()
    }

    /// The serializable contract view of this error.
    #[must_use]
    pub fn report(&self) -> ErrorReport {
        ErrorReport::from_error(self, None, None)
    }
}
