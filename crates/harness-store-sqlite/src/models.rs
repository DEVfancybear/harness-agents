use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use harness_types::{
    AgentRunId, ArtifactId, CompositionSnapshotId, ContentHash, ContextPacket, ContextPacketId,
    EventEnvelope, EventId, HostId, InputId, InstructionLedgerEntry, PluginManifest,
    ProviderAttemptId, RequestId, RuntimeCommandId, SessionId, SnapshotId, TaskId,
    ToolExecutionReceipt, WorkingState,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The only on-disk database schema revision implemented by P1.
pub const STORE_SCHEMA_VERSION: i64 = 1;
pub const DATABASE_FILE_NAME: &str = "harness.sqlite3";
pub const ARTIFACT_DIRECTORY_NAME: &str = "artifacts";
pub const WRITER_LOCK_FILE_NAME: &str = "writer.lock";
/// Additive runtime tables retain the P1 store schema version and have their
/// own migration marker so older P1 databases remain readable.
pub const RUNTIME_SCHEMA_VERSION: i64 = 1;

/// All durable paths owned by a local harness data directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorePaths {
    pub data_dir: PathBuf,
    pub database_path: PathBuf,
    pub artifact_dir: PathBuf,
    pub writer_lock_path: PathBuf,
}

impl StorePaths {
    #[must_use]
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        let data_dir = data_dir.into();
        Self {
            database_path: data_dir.join(DATABASE_FILE_NAME),
            artifact_dir: data_dir.join(ARTIFACT_DIRECTORY_NAME),
            writer_lock_path: data_dir.join(WRITER_LOCK_FILE_NAME),
            data_dir,
        }
    }

    #[must_use]
    pub fn from_database_path(path: impl AsRef<Path>) -> Option<Self> {
        path.as_ref()
            .parent()
            .map(|parent| Self::new(parent.to_owned()))
    }
}

/// A process-specific authority to write this data directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostFence {
    pub host_id: HostId,
    pub generation: u64,
}

/// Test-only fault locations. They interrupt a real `SQLite` transaction before
/// its commit; they are not a replacement for the transaction engine.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum StoreFaultPoint {
    BeforeMigrationCommit,
    BeforeAdmissionCommit,
    BeforeReceiptCommit,
    BeforeSnapshotCommit,
}

/// A one-shot, deterministic fault injector for component tests.
#[derive(Clone, Debug, Default)]
pub struct StoreFaultPlan {
    points: Arc<Mutex<BTreeSet<StoreFaultPoint>>>,
}

impl StoreFaultPlan {
    #[must_use]
    pub fn with_point(point: StoreFaultPoint) -> Self {
        let plan = Self::default();
        plan.arm(point);
        plan
    }

    pub fn arm(&self, point: StoreFaultPoint) {
        let mut points = self
            .points
            .lock()
            .expect("fault plan mutex is not poisoned");
        points.insert(point);
    }

    #[must_use]
    pub fn consume(&self, point: StoreFaultPoint) -> bool {
        let mut points = self
            .points
            .lock()
            .expect("fault plan mutex is not poisoned");
        points.remove(&point)
    }
}

/// Configuration for a writable store open.
#[derive(Clone, Debug)]
pub struct WriterOpenOptions {
    pub data_dir: PathBuf,
    pub host_id: HostId,
    pub fault_plan: StoreFaultPlan,
}

impl WriterOpenOptions {
    #[must_use]
    pub fn new(data_dir: impl Into<PathBuf>, host_id: HostId) -> Self {
        Self {
            data_dir: data_dir.into(),
            host_id,
            fault_plan: StoreFaultPlan::default(),
        }
    }

    #[must_use]
    pub fn with_fault_plan(mut self, fault_plan: StoreFaultPlan) -> Self {
        self.fault_plan = fault_plan;
        self
    }
}

/// Metadata reported by a real `SQLite` connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreDiagnostics {
    pub foreign_keys_enabled: bool,
    pub journal_mode: String,
    pub synchronous: i64,
    pub busy_timeout_ms: i64,
}

/// A durable marker proving which source work produced a projection update.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceWorkMarker {
    pub marker_id: String,
    pub event_id: EventId,
    pub sequence: u64,
    pub kind: String,
    pub status: String,
}

/// All records that must commit together to admit a user input.
#[derive(Clone, Debug)]
pub struct AdmissionCommit {
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub input_id: InputId,
    pub input_hash: ContentHash,
    pub raw_text: String,
    pub expected_sequence: u64,
    pub event: EventEnvelope,
    pub instruction: InstructionLedgerEntry,
    pub working_state: WorkingState,
    pub marker: SourceWorkMarker,
}

/// The sole result that may become an input ACK.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmissionAck {
    pub input_id: InputId,
    pub event_id: EventId,
    pub sequence: u64,
    pub idempotent_replay: bool,
}

/// All records that must commit together to record a settled synthetic receipt.
#[derive(Clone, Debug)]
pub struct ReceiptCommit {
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub expected_sequence: u64,
    pub event: EventEnvelope,
    pub receipt: ToolExecutionReceipt,
    pub working_state: WorkingState,
    pub marker: SourceWorkMarker,
    pub artifact: Option<PublishedArtifact>,
}

/// The durable result of recording a receipt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReceiptAck {
    pub event_id: EventId,
    pub sequence: u64,
    pub idempotent_replay: bool,
}

/// A snapshot is stored as canonical JSON with its coverage and checksum.
#[derive(Clone, Debug, PartialEq)]
pub struct SnapshotRecord {
    pub snapshot_id: SnapshotId,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub through_sequence: u64,
    pub schema_version: u16,
    pub content: Value,
    pub content_hash: ContentHash,
}

/// Bytes that have been flushed and atomically published before a database
/// transaction is allowed to reference them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishedArtifact {
    pub artifact_id: ArtifactId,
    pub content_hash: ContentHash,
    pub byte_len: u64,
    pub relative_path: String,
}

/// A session row suitable for a read-only CLI listing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionSummary {
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub next_sequence: u64,
    pub input_count: u64,
    pub latest_snapshot_sequence: Option<u64>,
}

/// Persisted plugin metadata available before a runtime exists.
#[derive(Clone, Debug, PartialEq)]
pub struct PersistedPluginManifest {
    pub manifest: PluginManifest,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeCommandState {
    Pending,
    Claimed,
    Completed,
    Canceled,
}

impl RuntimeCommandState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::Completed => "completed",
            Self::Canceled => "canceled",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "claimed" => Some(Self::Claimed),
            "completed" => Some(Self::Completed),
            "canceled" => Some(Self::Canceled),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeCommandRecord {
    pub command_id: RuntimeCommandId,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub state: RuntimeCommandState,
    pub attempts: u32,
    pub owner_generation: u64,
    pub payload: Value,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentStateRecord {
    pub agent_run_id: AgentRunId,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub state: String,
    pub generation: u64,
    pub revision: u64,
    pub detail: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompositionSnapshotRecord {
    pub snapshot_id: CompositionSnapshotId,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub revision: u64,
    pub content: Value,
    pub content_hash: ContentHash,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContextCheckpointRecord {
    pub checkpoint_id: String,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub through_sequence: u64,
    pub revision: u64,
    pub content: Value,
    pub content_hash: ContentHash,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ContextPacketRecord {
    pub packet: ContextPacket,
    pub composition_snapshot_id: Option<CompositionSnapshotId>,
    pub omitted_optional: Vec<String>,
    pub degradation: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FrozenRequestRecord {
    pub request_id: RequestId,
    pub packet_id: ContextPacketId,
    pub composition_snapshot_id: Option<CompositionSnapshotId>,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub request_json: Value,
    pub content_hash: ContentHash,
    pub provider_id: String,
    pub model: String,
    pub config_revision: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderAttemptRecord {
    pub attempt_id: ProviderAttemptId,
    pub request_id: RequestId,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub attempt_number: u32,
    pub state: String,
    pub events: Value,
    pub response_hash: Option<ContentHash>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContinuationLinkRecord {
    pub source_session_id: SessionId,
    pub new_session_id: SessionId,
    pub task_id: TaskId,
}
