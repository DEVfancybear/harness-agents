#![forbid(unsafe_code)]

//! SQLite-backed transaction coordination for P1 durable continuation.

mod error;
mod models;
mod store;

pub use error::StoreError;
pub use models::{
    AdmissionAck, AdmissionCommit, AgentStateRecord, BudgetUsageRecord, CompositionSnapshotRecord,
    ContextCheckpointRecord, ContextPacketRecord, ContinuationLinkRecord, DATABASE_FILE_NAME,
    DELEGATION_SCHEMA_VERSION, DeliveryCommit, FrozenRequestRecord, HostFence,
    MAINTENANCE_SCHEMA_VERSION, MEMORY_SCHEMA_VERSION, MemoryBindingRow, MemoryCreateCommit,
    MemoryVersionCommit, ParentDeliveryRecord, PersistedPluginManifest, ProjectRegistrationRecord,
    ProviderAttemptRecord, PublishedArtifact, RUNTIME_SCHEMA_VERSION, ReceiptAck, ReceiptCommit,
    RuntimeCommandRecord, RuntimeCommandState, STORE_SCHEMA_VERSION, SessionSummary,
    SnapshotRecord, SourceWorkMarker, StoreDiagnostics, StoreFaultPlan, StoreFaultPoint,
    StoreMemoryPrincipal, StorePaths, StoredDelegatedResultRecord, StoredExtractionJobRecord,
    StoredExtractionLeaseRecord, StoredMemoryAssetRecord, StoredMemoryGrantRecord,
    StoredMemoryVersionRecord, StoredTaskNodeRecord, TOOLS_SCHEMA_VERSION, TaskOwnerRecord,
    TombstoneRow, ToolApprovalBinding, ToolApprovalRecord, ToolApprovalState, ToolIntentCommit,
    ToolIntentRecord, ToolIntentStatus, ToolSettlementCommit, ToolTaskUpdateCommit,
    WRITER_LOCK_FILE_NAME, WorktreeRecordRow, WriterOpenOptions,
};
pub use store::SqliteStore;
pub use store::delegation::TaskAdmission;
