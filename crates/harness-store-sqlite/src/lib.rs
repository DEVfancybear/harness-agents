#![forbid(unsafe_code)]

//! SQLite-backed transaction coordination for P1 durable continuation.

mod error;
mod models;
mod store;

pub use error::StoreError;
pub use models::{
    AdmissionAck, AdmissionCommit, AgentStateRecord, BudgetAccountRecord, BudgetReservationRecord,
    BudgetReservationState, BudgetSettlement, BudgetUsageRecord, CompositionSnapshotRecord,
    ContextCheckpointRecord, ContextPacketRecord, ContinuationLinkRecord,
    DATA_DIRECTORY_FORMAT_VERSION, DATA_DIRECTORY_KIND, DATA_DIRECTORY_MARKER_FILE_NAME,
    DATABASE_FILE_NAME, DELEGATION_SCHEMA_VERSION, DataDirectoryMarker, DeliveryCommit,
    FrozenRequestRecord, HostFence, MAINTENANCE_SCHEMA_VERSION, MEMORY_SCHEMA_VERSION,
    MemoryBindingRow, MemoryCreateCommit, MemoryVersionCommit, ParentDeliveryRecord,
    PersistedPluginManifest, ProjectRegistrationRecord, ProviderAttemptRecord, PublishedArtifact,
    QuestionAnswer, QuestionOutcome, QuestionRecord, QuestionState, RUNTIME_SCHEMA_VERSION,
    ReceiptAck, ReceiptCommit, RecoveredToolResult, RunCommandKind, RunCommandRecord,
    RunCommandState, RunRecord, RunState, RunStepRecord, RuntimeCommandRecord, RuntimeCommandState,
    STORE_SCHEMA_VERSION, SessionSummary, SnapshotRecord, SourceWorkMarker, StoreDiagnostics,
    StoreFaultPlan, StoreFaultPoint, StoreMemoryPrincipal, StorePaths, StoredDelegatedResultRecord,
    StoredExtractionJobRecord, StoredExtractionLeaseRecord, StoredMemoryAssetRecord,
    StoredMemoryGrantRecord, StoredMemoryVersionRecord, StoredTaskNodeRecord, TOOLS_SCHEMA_VERSION,
    TaskOwnerRecord, TombstoneRow, ToolApprovalBinding, ToolApprovalRecord, ToolApprovalState,
    ToolIntentCommit, ToolIntentRecord, ToolIntentStatus, ToolSettlementCommit,
    ToolTaskUpdateCommit, WRITER_LOCK_FILE_NAME, WorktreeRecordRow, WriterOpenOptions,
};
pub use store::SqliteStore;
pub use store::delegation::TaskAdmission;
