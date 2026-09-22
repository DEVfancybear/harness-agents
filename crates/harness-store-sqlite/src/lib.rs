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
    MAINTENANCE_SCHEMA_VERSION, MEMORY_SCHEMA_VERSION, MemoryBindingRow, MemoryCreateCommit,
    MemorySourceKind, MemorySourceRecord, MemoryVersionCommit, ParentDeliveryRecord,
    PersistedPluginManifest, ProjectRegistrationRecord, ProviderAttemptRecord, PublishedArtifact,
    QuestionAnswer, QuestionOutcome, QuestionRecord, QuestionState, RUNTIME_SCHEMA_VERSION,
    ReceiptAck, ReceiptCommit, RecoveredToolResult, RefreshSource, RunCommandKind,
    RunCommandRecord, RunCommandState, RunRecord, RunState, RunStepRecord, RuntimeCommandRecord,
    RuntimeCommandState, STORE_SCHEMA_VERSION, SessionSummary, SnapshotRecord, SourceWorkMarker,
    SourceWorkRange, StoreDiagnostics, StoreFaultPlan, StoreFaultPoint, StoreMemoryPrincipal,
    StorePaths, StoredDelegatedResultRecord, StoredExtractionJobRecord,
    StoredExtractionLeaseRecord, StoredMemoryAssetRecord, StoredMemoryGrantRecord,
    StoredMemoryVersionRecord, StoredTaskNodeRecord, TOOLS_SCHEMA_VERSION, TaskOwnerRecord,
    TombstoneRow, ToolApprovalBinding, ToolApprovalRecord, ToolApprovalState, ToolIntentCommit,
    ToolIntentRecord, ToolIntentStatus, ToolSettlementCommit, ToolTaskUpdateCommit,
    WRITER_LOCK_FILE_NAME, WorktreeRecordRow, WriterOpenOptions,
};
pub use store::SqliteStore;
pub use store::delegation::TaskAdmission;
pub use store::history::{
    HISTORY_READ_DEFAULT_BYTES, HISTORY_READ_MAX_BYTES, HISTORY_SOURCE_LIMIT_BYTES, HistoryHit,
    HistoryPage, HistoryScope, HistorySource, NOTE_CONTENT_LIMIT_BYTES, NOTE_KEY_LIMIT_CHARS,
    NoteRecord, SourceAvailability, history_terms,
};
