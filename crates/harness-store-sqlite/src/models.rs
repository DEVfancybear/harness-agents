use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use harness_types::{
    AgentProfileId, AgentRunId, ArtifactId, CompositionSnapshotId, ContentHash, ContextPacket,
    ContextPacketId, ErrorCode, EventEnvelope, EventId, HostId, InputId, InstructionLedgerEntry,
    MemoryAsset, MemoryAssetId, MemoryVersion, PluginManifest, ProjectId, ProviderAttemptId,
    RequestId, RuntimeCommandId, SessionId, SnapshotId, TaskId, ToolApprovalId, ToolExecutionId,
    ToolExecutionReceipt, WorkingState,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::StoreError;

/// The only on-disk database schema revision implemented by P1.
pub const STORE_SCHEMA_VERSION: i64 = 1;
pub const DATABASE_FILE_NAME: &str = "harness.sqlite3";
pub const ARTIFACT_DIRECTORY_NAME: &str = "artifacts";
pub const WRITER_LOCK_FILE_NAME: &str = "writer.lock";
/// The data directory marker: one file that names the directory and its format.
pub const DATA_DIRECTORY_MARKER_FILE_NAME: &str = "harness-data.json";
/// The only data directory format this host writes.
pub const DATA_DIRECTORY_FORMAT_VERSION: u16 = 1;
/// The marker's `kind`, so a foreign JSON file is never mistaken for one.
pub const DATA_DIRECTORY_KIND: &str = "harness-data";
/// Additive runtime tables retain the P1 store schema version and have their
/// own migration marker so older P1 databases remain readable.
pub const RUNTIME_SCHEMA_VERSION: i64 = 1;
/// Additive P3 tool tables use their own revision so P0/P1/P2 storage remains
/// byte-for-byte compatible.
pub const TOOLS_SCHEMA_VERSION: i64 = 1;
/// Additive P4 memory tables retain all earlier schema revisions.
pub const MEMORY_SCHEMA_VERSION: i64 = 1;
/// Additive P5 delegation tables retain all earlier schema revisions.
pub const DELEGATION_SCHEMA_VERSION: i64 = 1;
/// Additive P7 maintenance tables retain all earlier schema revisions.
pub const MAINTENANCE_SCHEMA_VERSION: i64 = 1;

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

    /// The marker file that identifies this data directory and its format.
    #[must_use]
    pub fn marker_path(&self) -> PathBuf {
        self.data_dir.join(DATA_DIRECTORY_MARKER_FILE_NAME)
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
    BeforeToolIntentCommit,
    BeforeToolSettlementCommit,
    BeforeMemorySettlementCommit,
    BeforeDelegationDeliveryCommit,
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

/// The data directory marker's durable content.
///
/// It is written on the first writable open and validated on every later one:
/// a directory whose format is newer than this host supports is refused before
/// any migration runs, and the marker is never overwritten.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DataDirectoryMarker {
    pub schema_version: u16,
    pub kind: String,
    pub store_schema_version: i64,
}

impl DataDirectoryMarker {
    #[must_use]
    pub fn current() -> Self {
        Self {
            schema_version: DATA_DIRECTORY_FORMAT_VERSION,
            kind: DATA_DIRECTORY_KIND.to_owned(),
            store_schema_version: STORE_SCHEMA_VERSION,
        }
    }

    pub fn validate(&self) -> Result<(), StoreError> {
        if self.kind != DATA_DIRECTORY_KIND {
            return Err(StoreError::new(
                ErrorCode::MigrationFailed,
                format!(
                    "data directory marker is not a {DATA_DIRECTORY_KIND} marker: {}",
                    self.kind
                ),
            ));
        }
        if self.schema_version > DATA_DIRECTORY_FORMAT_VERSION {
            return Err(StoreError::new(
                ErrorCode::SchemaVersionMismatch,
                format!(
                    "data directory format {} is newer than this host supports ({DATA_DIRECTORY_FORMAT_VERSION})",
                    self.schema_version
                ),
            ));
        }
        if self.store_schema_version > STORE_SCHEMA_VERSION {
            return Err(StoreError::new(
                ErrorCode::SchemaVersionMismatch,
                format!(
                    "data directory store schema {} is newer than this host supports ({STORE_SCHEMA_VERSION})",
                    self.store_schema_version
                ),
            ));
        }
        Ok(())
    }
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

/// Lifecycle state of a durable P3 tool approval.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolApprovalState {
    Active,
    Consumed,
    Revoked,
}

impl ToolApprovalState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Consumed => "consumed",
            Self::Revoked => "revoked",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "consumed" => Some(Self::Consumed),
            "revoked" => Some(Self::Revoked),
            _ => None,
        }
    }
}

/// Immutable binding persisted for a single-use P3 approval.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolApprovalRecord {
    pub approval_id: ToolApprovalId,
    pub actor_id: String,
    pub binding_hash: ContentHash,
    pub action_hash: ContentHash,
    pub workspace_root: String,
    pub workspace_fingerprint: ContentHash,
    pub policy_revision: u64,
    pub tool_revision: u64,
    pub expires_at_unix_ms: Option<u64>,
    pub state: ToolApprovalState,
    pub approval_json: Value,
}

/// The durable action binding referenced by an intent or task update.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolApprovalBinding {
    pub approval_id: ToolApprovalId,
    pub actor_id: String,
    pub action_hash: ContentHash,
    pub workspace_root: String,
    pub workspace_fingerprint: ContentHash,
    pub policy_revision: u64,
    pub tool_revision: u64,
}

/// State stored for an execution that has crossed the side-effect boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolIntentStatus {
    Recorded,
    Settled,
    OutcomeUnknown,
}

impl ToolIntentStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::Settled => "settled",
            Self::OutcomeUnknown => "outcome_unknown",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "recorded" => Some(Self::Recorded),
            "settled" => Some(Self::Settled),
            "outcome_unknown" => Some(Self::OutcomeUnknown),
            _ => None,
        }
    }
}

/// The record committed before a coding-tool side effect begins.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolIntentRecord {
    pub tool_execution_id: ToolExecutionId,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub invocation_id: String,
    pub actor_id: String,
    pub tool_name: String,
    pub action_json: Value,
    pub action_hash: ContentHash,
    pub workspace_root: String,
    pub workspace_fingerprint: ContentHash,
    pub before_fingerprint: Option<ContentHash>,
    pub policy_revision: u64,
    pub tool_revision: u64,
    pub approval: ToolApprovalBinding,
    pub status: ToolIntentStatus,
    pub intent_sequence: u64,
}

/// Everything required to durably consume an approval and record an intent.
#[derive(Clone, Debug)]
pub struct ToolIntentCommit {
    pub expected_sequence: u64,
    pub event: EventEnvelope,
    pub intent: ToolIntentRecord,
    pub working_state: WorkingState,
    pub marker: SourceWorkMarker,
}

/// Everything required to settle a previously committed P3 intent.
#[derive(Clone, Debug)]
pub struct ToolSettlementCommit {
    pub expected_sequence: u64,
    pub event: EventEnvelope,
    pub receipt: ToolExecutionReceipt,
    pub working_state: WorkingState,
    pub marker: SourceWorkMarker,
    pub artifact: Option<PublishedArtifact>,
    pub final_status: ToolIntentStatus,
}

/// An atomic task update that consumes a tool approval but never fabricates a
/// process runner receipt.
#[derive(Clone, Debug)]
pub struct ToolTaskUpdateCommit {
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub expected_sequence: u64,
    pub event: EventEnvelope,
    pub working_state: WorkingState,
    pub marker: SourceWorkMarker,
    pub approval: ToolApprovalBinding,
}

/// Durable root/Git identity used to prevent accidental project conflation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectRegistrationRecord {
    pub project_id: ProjectId,
    pub canonical_root: String,
    pub git_common_dir: Option<String>,
    pub identity_hash: ContentHash,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreMemoryPrincipal {
    pub principal_id: String,
    pub project_id: Option<ProjectId>,
    pub task_id: Option<TaskId>,
    pub agent_profile_id: Option<AgentProfileId>,
    pub session_id: Option<SessionId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredMemoryVersionRecord {
    pub record: MemoryVersion,
    pub content: String,
    pub normalized_content: String,
    pub strategy_digest: Option<ContentHash>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredMemoryAssetRecord {
    pub asset: MemoryAsset,
    pub layer: String,
    pub task_id: Option<TaskId>,
    pub agent_profile_id: Option<AgentProfileId>,
    pub session_id: Option<SessionId>,
    pub current: StoredMemoryVersionRecord,
}

#[derive(Clone, Debug)]
pub struct MemoryCreateCommit {
    pub record: StoredMemoryAssetRecord,
}

#[derive(Clone, Debug)]
pub struct MemoryVersionCommit {
    pub source_assets: Vec<harness_types::MemoryVersionRef>,
    pub authorization: StoreMemoryPrincipal,
    pub action: String,
    pub memory_asset_id: MemoryAssetId,
    pub expected_version: u64,
    pub asset: MemoryAsset,
    pub version: StoredMemoryVersionRecord,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredMemoryGrantRecord {
    pub principal_id: String,
    pub memory_asset_id: MemoryAssetId,
    pub project_id: Option<ProjectId>,
    pub allowed_actions: Vec<String>,
    pub revision: u64,
    pub active: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredExtractionJobRecord {
    pub job_id: String,
    pub source_stream: SessionId,
    pub start_sequence: u64,
    pub end_sequence: u64,
    pub source_digest: ContentHash,
    pub source_event_ids: Vec<EventId>,
    pub extractor_version: String,
    pub strategy_digest: ContentHash,
    pub status: String,
    pub attempts: u32,
    pub lease_owner: Option<String>,
    pub lease_generation: u64,
    pub last_error: Option<String>,
    pub disposition: Option<String>,
}

#[derive(Clone, Debug)]
pub struct StoredExtractionLeaseRecord {
    pub job: StoredExtractionJobRecord,
    pub source_events: Vec<EventEnvelope>,
    pub owner: String,
    pub generation: u64,
}

/// One durable delegation task node, including its host-authored brief.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredTaskNodeRecord {
    pub task_id: TaskId,
    pub parent_task_id: Option<TaskId>,
    pub role: String,
    pub status: String,
    pub revision: u64,
    pub depth: u32,
    pub depends_on: Vec<TaskId>,
    pub brief_json: Value,
    pub node_json: Value,
}

/// The single durable owner of a task, fenced by ownership generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskOwnerRecord {
    pub task_id: TaskId,
    pub owner_run_id: AgentRunId,
    pub owner_session_id: SessionId,
    pub role: String,
    pub generation: u64,
    pub lease_revision: u64,
}

/// A task state transition together with the durable parent message it makes
/// visible. Both commit inside one transaction.
#[derive(Clone, Debug)]
pub struct DeliveryCommit {
    pub task_transition: StoredTaskNodeRecord,
    pub result: Option<StoredDelegatedResultRecord>,
    pub delivery: ParentDeliveryRecord,
    pub usage: Option<BudgetUsageRecord>,
}

/// The durable worker report.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredDelegatedResultRecord {
    pub result_id: String,
    pub task_id: TaskId,
    pub worker_run_id: AgentRunId,
    pub outcome: String,
    pub base_revision: String,
    pub result_revision: String,
    pub artifact_refs: Vec<String>,
    pub report_json: Value,
    pub result_hash: ContentHash,
}

/// A durable parent message. `message_id` is the logical delivery identity.
#[derive(Clone, Debug, PartialEq)]
pub struct ParentDeliveryRecord {
    pub message_id: String,
    pub sender_task_id: TaskId,
    pub recipient_task_id: TaskId,
    pub recipient_session_id: SessionId,
    pub result_id: Option<String>,
    pub payload_hash: ContentHash,
    pub payload: Value,
    pub state: String,
    pub consumed_by: Option<String>,
}

/// Observed usage charged against a delegation budget. The charged task is
/// implied by the write that carried it, so this stays a plain value record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BudgetUsageRecord {
    pub model_requests: u32,
    pub retries: u32,
    pub cost_units: u64,
}

/// A host-issued memory binding for one delegated worker. The binding pins the
/// exact asset version that was injected, so a later revision is detectable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryBindingRow {
    pub binding_id: String,
    pub task_id: TaskId,
    pub profile_id: AgentProfileId,
    pub memory_asset_id: MemoryAssetId,
    pub version: u64,
    pub injection_mode: String,
    pub priority: i64,
    pub actions: Vec<String>,
    pub revision: u64,
}

/// A host-owned isolated worker workspace row.
#[derive(Clone, Debug, PartialEq)]
pub struct WorktreeRecordRow {
    pub worktree_id: String,
    pub task_id: TaskId,
    pub run_id: AgentRunId,
    pub project_id: ProjectId,
    pub base_commit: String,
    pub base_branch: String,
    pub branch: String,
    pub path: String,
    pub write_scope: Vec<String>,
    pub state: String,
    pub input_fingerprint: ContentHash,
    pub result_fingerprint: Option<ContentHash>,
    pub generation: u64,
}

/// A durable retention tombstone. It records that a source was deliberately
/// forgotten and blocks a later extraction pass from bringing it back.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TombstoneRow {
    pub tombstone_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub reason: String,
    pub surviving_copies: Vec<String>,
    pub created_unix_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::{
        DATA_DIRECTORY_FORMAT_VERSION, DATA_DIRECTORY_KIND, DataDirectoryMarker,
        STORE_SCHEMA_VERSION,
    };
    use harness_types::ErrorCode;

    #[test]
    fn data_directory_marker_accepts_the_current_format_only() {
        DataDirectoryMarker::current()
            .validate()
            .expect("the current marker is valid");

        let newer_format = DataDirectoryMarker {
            schema_version: DATA_DIRECTORY_FORMAT_VERSION + 1,
            kind: DATA_DIRECTORY_KIND.to_owned(),
            store_schema_version: STORE_SCHEMA_VERSION,
        };
        assert_eq!(
            newer_format
                .validate()
                .expect_err("a newer format is refused")
                .code(),
            ErrorCode::SchemaVersionMismatch
        );

        let newer_store = DataDirectoryMarker {
            schema_version: DATA_DIRECTORY_FORMAT_VERSION,
            kind: DATA_DIRECTORY_KIND.to_owned(),
            store_schema_version: STORE_SCHEMA_VERSION + 1,
        };
        assert_eq!(
            newer_store
                .validate()
                .expect_err("a newer store schema is refused")
                .code(),
            ErrorCode::SchemaVersionMismatch
        );

        let foreign = DataDirectoryMarker {
            schema_version: DATA_DIRECTORY_FORMAT_VERSION,
            kind: "some-other-tool".to_owned(),
            store_schema_version: STORE_SCHEMA_VERSION,
        };
        assert_eq!(
            foreign
                .validate()
                .expect_err("a foreign marker is refused")
                .code(),
            ErrorCode::MigrationFailed
        );
    }
}
