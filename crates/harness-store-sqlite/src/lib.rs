#![forbid(unsafe_code)]

//! SQLite-backed transaction coordination for P1 durable continuation.

mod error;
mod models;
mod store;

pub use error::StoreError;
pub use models::{
    AdmissionAck, AdmissionCommit, DATABASE_FILE_NAME, HostFence, PersistedPluginManifest,
    PublishedArtifact, ReceiptAck, ReceiptCommit, STORE_SCHEMA_VERSION, SessionSummary,
    SnapshotRecord, SourceWorkMarker, StoreDiagnostics, StoreFaultPlan, StoreFaultPoint,
    StorePaths, WRITER_LOCK_FILE_NAME, WriterOpenOptions,
};
pub use store::SqliteStore;
