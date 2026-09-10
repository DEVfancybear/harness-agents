#![forbid(unsafe_code)]

//! SQLite-backed transaction coordination for P1 durable continuation.

mod error;
mod models;
mod store;

pub use error::StoreError;
pub use models::{
    AdmissionAck, AdmissionCommit, AgentStateRecord, CompositionSnapshotRecord,
    ContextCheckpointRecord, ContextPacketRecord, ContinuationLinkRecord, DATABASE_FILE_NAME,
    FrozenRequestRecord, HostFence, PersistedPluginManifest, ProviderAttemptRecord,
    PublishedArtifact, RUNTIME_SCHEMA_VERSION, ReceiptAck, ReceiptCommit, RuntimeCommandRecord,
    RuntimeCommandState, STORE_SCHEMA_VERSION, SessionSummary, SnapshotRecord, SourceWorkMarker,
    StoreDiagnostics, StoreFaultPlan, StoreFaultPoint, StorePaths, WRITER_LOCK_FILE_NAME,
    WriterOpenOptions,
};
pub use store::SqliteStore;
