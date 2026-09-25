#![forbid(unsafe_code)]

//! SQLite-backed transaction coordination for P1 durable continuation.

mod error;
mod models;
mod store;

pub use error::StoreError;
pub use models::{
    AdmissionAck, AdmissionCommit, AgentStateRecord, ArtifactPage, BudgetAccountRecord,
    BudgetReservationRecord, BudgetReservationState, BudgetSettlement, BudgetUsageRecord,
    CONTEXT_SCHEMA_VERSION, CompositionSnapshotRecord, ContextCheckpointRecord,
    ContextPacketRecord, ContinuationLinkRecord, DATA_DIRECTORY_FORMAT_VERSION,
    DATA_DIRECTORY_KIND, DATA_DIRECTORY_MARKER_FILE_NAME, DATABASE_FILE_NAME,
    DELEGATION_SCHEMA_VERSION, DataDirectoryMarker, DeliveryCommit, FrozenRequestRecord, HostFence,
    MAINTENANCE_SCHEMA_VERSION, ParentDeliveryRecord, PersistedPluginManifest,
    ProjectRegistrationRecord, ProviderAttemptRecord, PublishedArtifact, QuestionAnswer,
    QuestionOutcome, QuestionRecord, QuestionState, RUNTIME_SCHEMA_VERSION, ReceiptAck,
    ReceiptCommit, RecoveredToolResult, RunCommandKind, RunCommandRecord, RunCommandState,
    RunRecord, RunState, RunStepRecord, RuntimeCommandRecord, RuntimeCommandState,
    STORE_SCHEMA_VERSION, SessionSummary, SnapshotRecord, SourceWorkMarker, StoreDiagnostics,
    StoreFaultPlan, StoreFaultPoint, StorePaths, StoredDelegatedResultRecord, StoredTaskNodeRecord,
    TOOLS_SCHEMA_VERSION, TaskOwnerRecord, TombstoneRow, ToolApprovalBinding, ToolApprovalRecord,
    ToolApprovalState, ToolIntentCommit, ToolIntentRecord, ToolIntentStatus, ToolSettlementCommit,
    ToolTaskUpdateCommit, WRITER_LOCK_FILE_NAME, WorktreeRecordRow, WriterOpenOptions,
};
pub use store::SqliteStore;
pub use store::approvals::{APPROVAL_STATES, StoredApproval};
pub use store::backend_leases::{LEASE_STATES, StoredBackendLease};
pub use store::delegation::TaskAdmission;
pub use store::external_jobs::{
    EXTERNAL_JOB_STATES, EXTERNAL_JOB_TERMINAL_STATES, ExternalJobSettlement,
    RecoveredExternalJobs, StoredExternalJob, StoredExternalJobPoll, external_job_delivery_id,
};
pub use store::history::{
    HISTORY_READ_DEFAULT_BYTES, HISTORY_READ_MAX_BYTES, HISTORY_SOURCE_LIMIT_BYTES, HistoryHit,
    HistoryPage, HistoryScope, HistorySource, NOTE_CONTENT_LIMIT_BYTES, NOTE_KEY_LIMIT_CHARS,
    NoteRecord, SourceAvailability, history_terms,
};
pub use store::notifications::{
    NOTIFICATION_BACKOFF_MS, NOTIFICATION_MAX_ATTEMPTS, NOTIFICATION_MAX_BACKOFF_MS,
    NOTIFICATION_STATES, OutboxCounts, StoredNotification, notification_backoff_ms,
};
pub use store::schedules::{StoredOccurrenceRecord, StoredScheduleRecord};
