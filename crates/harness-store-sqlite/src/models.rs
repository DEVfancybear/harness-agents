use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use harness_types::{
    AgentProfileId, AgentRunId, ArtifactId, BudgetId, BudgetReservationId, CompositionSnapshotId,
    ContentHash, ContextPacket, ContextPacketId, ErrorCode, EventEnvelope, EventId, HostId,
    InputId, InstructionLedgerEntry, MemoryAsset, MemoryAssetId, MemoryAssetStatus, MemoryVersion,
    PluginManifest, ProjectId, ProviderAttemptId, QuestionId, RequestId, RuntimeCommandId,
    SessionId, SnapshotId, StepId, TaskId, ToolApprovalId, ToolExecutionId, ToolExecutionReceipt,
    WorkingState,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::StoreError;

/// Additive P1 store schema revision with durable session settings.
pub const STORE_SCHEMA_VERSION: i64 = 2;
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
///
/// 2 adds the M3 durable run/step, budget and human-input tables. The change is
/// additive: opening a version 1 database with this host creates the new tables
/// and records version 2; a host that only supports version 1 refuses to write a
/// version 2 database instead of silently dropping the new records.
/// 3 adds the M11 durable schedule and occurrence tables. The change is
/// additive, so a version 2 database upgrades in place.
/// 4 adds the external-job tables, 5 the schedule approvals and the notification
/// outbox, and 6 the M12 backend leases. All three are additive.
pub const RUNTIME_SCHEMA_VERSION: i64 = 6;
/// Additive P3 tool tables. 2 adds the M4 approval scope (session/task/
/// invocation/call correlation) and the provider call id on intents; both are
/// additive columns so a version 1 database upgrades in place.
pub const TOOLS_SCHEMA_VERSION: i64 = 2;
/// Additive P4 memory tables retain all earlier schema revisions.
///
/// 2 adds `memory_sources` (M7): the keyed source dependency of one version, so
/// freshness and source invalidation are index lookups instead of a scan over a
/// JSON array of hashes. A database written by version 1 upgrades in place - the
/// table is created and the revision recorded - and a version that predates the
/// table still reads, because a version without source rows has no evidence that
/// its sources moved (see `ADR-N07`, D1/D2).
pub const MEMORY_SCHEMA_VERSION: i64 = 2;
/// Additive M5 context surface: the rebuildable history index, the notes table
/// and the manifest columns a frozen packet and checkpoint record. Version 1 is
/// the first revision of this module, so a database that predates M5 upgrades
/// by creating the tables and adding the columns in place.
pub const CONTEXT_SCHEMA_VERSION: i64 = 1;
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
    /// Before a run step and its budget reservation commit together.
    BeforeFreezeStepCommit,
    /// Before a budget settlement commits.
    BeforeBudgetSettleCommit,
    /// Before a question answer commits.
    BeforeQuestionAnswerCommit,
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
///
/// M4: the binding carries the scope it was issued for (session, task,
/// invocation) and the provider `call_id` when one exists, so a grant cannot be
/// replayed against another invocation that happens to look identical.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolApprovalRecord {
    pub approval_id: ToolApprovalId,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub invocation_id: String,
    pub call_id: Option<String>,
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
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub invocation_id: String,
    pub call_id: Option<String>,
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
    /// The provider's `call_id`, when the intent came from a model call.
    pub call_id: Option<String>,
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

/// One committed tool result a later step can replay after a crash.
///
/// The text is the same bounded, redacted view the model was given; it is
/// recorded with the receipt event so a continuation does not have to parse the
/// artifact or rerun the tool.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveredToolResult {
    pub seq: u64,
    pub call_id: Option<String>,
    pub text: String,
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

/// One bounded page of a published artifact, with the record's own length and
/// digest so a reader can prove it got the bytes the receipt points at.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactPage {
    pub artifact_id: String,
    pub offset: u64,
    /// Bytes returned in this page.
    pub bytes: Vec<u8>,
    /// Bytes recorded for the whole artifact.
    pub total_bytes: u64,
    pub content_hash: ContentHash,
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

/// Durable state of one admitted input's bounded model loop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunState {
    Running,
    /// Paused on a durable question or an external wait; no compute permit and
    /// no transaction is held. Mirrors the accepted `AgentState::Paused`.
    Paused,
    Completed,
    Failed,
    Canceled,
}

impl RunState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "running" => Some(Self::Running),
            "paused" => Some(Self::Paused),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "canceled" => Some(Self::Canceled),
            _ => None,
        }
    }
}

/// One logical run: one admitted user input and every model step it produced.
#[derive(Clone, Debug, PartialEq)]
pub struct RunRecord {
    pub run_id: AgentRunId,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub input_id: InputId,
    pub state: RunState,
    /// Typed task acceptance, written by the driver when a goal was attached.
    pub acceptance: Option<String>,
    /// Typed stop reason, written when the run leaves `running`.
    pub stop_reason: Option<String>,
    pub owner_generation: u64,
    /// Bumped by every step freeze and terminal transition; used for CAS.
    pub revision: u64,
    pub budget_id: Option<BudgetId>,
    /// The open question that paused this run, when one did.
    pub awaiting_question_id: Option<QuestionId>,
}

/// One model call inside a run.
#[derive(Clone, Debug, PartialEq)]
pub struct RunStepRecord {
    pub step_id: StepId,
    pub run_id: AgentRunId,
    pub step_index: u32,
    pub request_id: RequestId,
    pub packet_id: ContextPacketId,
    /// Hash of the frozen source manifest (block ids + source refs + packet hash).
    pub manifest_hash: ContentHash,
    pub source_sequence: u64,
    pub state: String,
    pub stop_reason: Option<String>,
}

/// A durable token account. Child accounts consume their parent's limit too.
#[derive(Clone, Debug, PartialEq)]
pub struct BudgetAccountRecord {
    pub budget_id: BudgetId,
    pub parent_budget_id: Option<BudgetId>,
    pub limit_tokens: u64,
    pub spent_tokens: u64,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BudgetReservationState {
    Reserved,
    Settled,
    /// The operation ended without a measurable usage; the conservative bound
    /// stays charged until an explicit reconciliation releases it.
    Unknown,
    Released,
    Expired,
}

impl BudgetReservationState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Settled => "settled",
            Self::Unknown => "unknown",
            Self::Released => "released",
            Self::Expired => "expired",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "reserved" => Some(Self::Reserved),
            "settled" => Some(Self::Settled),
            "unknown" => Some(Self::Unknown),
            "released" => Some(Self::Released),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }
}

/// One atomic reservation keyed by the operation that will settle it.
#[derive(Clone, Debug, PartialEq)]
pub struct BudgetReservationRecord {
    pub reservation_id: BudgetReservationId,
    pub budget_id: BudgetId,
    /// Stable operation identity: an attempt ID, a summary, an evaluator call.
    pub operation_id: String,
    pub origin: String,
    pub upper_bound_tokens: u64,
    pub settled_tokens: Option<u64>,
    pub state: BudgetReservationState,
    pub revision: u64,
}

/// One durable question the host asked the user.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuestionState {
    Open,
    Answered,
    Expired,
    Canceled,
}

impl QuestionState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Answered => "answered",
            Self::Expired => "expired",
            Self::Canceled => "canceled",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "answered" => Some(Self::Answered),
            "expired" => Some(Self::Expired),
            "canceled" => Some(Self::Canceled),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct QuestionRecord {
    pub question_id: QuestionId,
    /// Dedupe scope: the same run and request resolves to one question row.
    pub scope_key: String,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub run_id: Option<AgentRunId>,
    pub kind: String,
    pub prompt: String,
    pub payload: Value,
    pub state: QuestionState,
    pub answer: Option<Value>,
    pub answer_hash: Option<ContentHash>,
    pub answered_by: Option<String>,
    pub expires_at_unix_ms: Option<u64>,
    pub created_at_unix_ms: u64,
    pub answered_at_unix_ms: Option<u64>,
}

/// What one answer attempt did.
#[derive(Clone, Debug, PartialEq)]
pub enum QuestionAnswer {
    /// First answer for this question; persisted now.
    Answered(QuestionRecord),
    /// The same answer was already recorded; the stored record comes back.
    Duplicate(QuestionRecord),
}

/// The outcome of answering, including the states that are not an answer.
#[derive(Clone, Debug, PartialEq)]
pub enum QuestionOutcome {
    Answered(QuestionRecord),
    Duplicate(QuestionRecord),
    /// No question is scoped by that key: a wrong request ID or scope.
    NotFound,
    /// The question expired before an answer arrived.
    Expired(QuestionRecord),
    /// The question was already answered with a different payload.
    Conflict(QuestionRecord),
}

/// How a reservation is settled when its operation ends.
#[derive(Clone, Debug, PartialEq)]
pub struct BudgetSettlement {
    pub reservation_id: BudgetReservationId,
    /// `None` means the usage is unknown: the conservative bound stays charged.
    pub measured_tokens: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunCommandKind {
    /// A correction the model must see at the next safe boundary.
    Steer,
    /// Stop the run; queued work must not execute.
    Cancel,
}

impl RunCommandKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Steer => "steer",
            Self::Cancel => "cancel",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "steer" => Some(Self::Steer),
            "cancel" => Some(Self::Cancel),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunCommandState {
    Pending,
    Claimed,
    Applied,
    Rejected,
}

impl RunCommandState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::Applied => "applied",
            Self::Rejected => "rejected",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "claimed" => Some(Self::Claimed),
            "applied" => Some(Self::Applied),
            "rejected" => Some(Self::Rejected),
            _ => None,
        }
    }
}

/// One steering or cancel command waiting for a run boundary.
#[derive(Clone, Debug, PartialEq)]
pub struct RunCommandRecord {
    pub command_id: RuntimeCommandId,
    pub run_id: AgentRunId,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub kind: RunCommandKind,
    pub state: RunCommandState,
    pub payload: Value,
    pub detail: Option<String>,
    pub created_at_unix_ms: u64,
    pub claimed_at_unix_ms: Option<u64>,
    pub applied_at_unix_ms: Option<u64>,
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

/// What a memory version was derived from, and the digest observed at that moment.
///
/// The digest is the value read when the version was written, never the current
/// one: the whole point of keeping it is to be able to say later that the source
/// moved. `event` and `asset` sources reference rows the store already owns;
/// `file` and `commit` reference the workspace outside it, which is why their
/// ids are relative paths and revision names rather than ids.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemorySourceKind {
    File,
    Commit,
    Event,
    Asset,
}

impl MemorySourceKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Commit => "commit",
            Self::Event => "event",
            Self::Asset => "asset",
        }
    }

    pub fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "file" => Ok(Self::File),
            "commit" => Ok(Self::Commit),
            "event" => Ok(Self::Event),
            "asset" => Ok(Self::Asset),
            _ => Err(StoreError::new(
                ErrorCode::InvalidPayload,
                "stored memory source kind is unsupported",
            )),
        }
    }
}

/// One row of `memory_sources`: the source of exactly one memory version.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemorySourceRecord {
    pub source_kind: MemorySourceKind,
    /// Relative path for a file, revision name for a commit, id for an event or
    /// asset. Never an absolute path: the id is what a workspace fingerprint and
    /// a later invalidation agree on.
    pub source_id: String,
    /// Content digest observed when the version was written. Required for
    /// `file`, optional for the others.
    pub observed_digest: Option<ContentHash>,
    /// Version of the referenced asset, or the event sequence, when the kind
    /// names something that has one.
    pub source_version: Option<u64>,
}

/// One source, as the caller currently observes it.
///
/// Both halves are needed: `id` says which source, `observed` says what the
/// caller sees now. The stored digest is compared against this, so a caller that
/// re-reads a file and finds the same bytes reports no change.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RefreshSource {
    pub kind: MemorySourceKind,
    pub id: String,
    /// `None` means the named source could not be read inside the caller's
    /// workspace, which is stale for a source that previously had a digest.
    pub observed: Option<ContentHash>,
}

/// A maximal run of contiguous committed source-work markers.
#[derive(Clone, Debug)]
pub struct SourceWorkRange {
    pub start_sequence: u64,
    pub end_sequence: u64,
    /// The committed events the range covers, in sequence order.
    pub events: Vec<EventEnvelope>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredMemoryVersionRecord {
    pub record: MemoryVersion,
    pub content: String,
    pub normalized_content: String,
    pub strategy_digest: Option<ContentHash>,
    /// The sources this version was derived from, written in the same
    /// transaction as the version itself.
    pub sources: Vec<MemorySourceRecord>,
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

/// One atomic operator request to change memory retention and append its journal record.
#[derive(Clone, Debug)]
pub struct MemorySourceRetentionUpdate {
    pub source_kind: String,
    pub source_id: String,
    pub status: MemoryAssetStatus,
    pub reason: Option<String>,
    pub journal_entry_id: String,
    pub journal_detail: Value,
    pub created_unix_ms: u64,
}

/// One atomic operator request to forget a source, persist its tombstone and journal record.
#[derive(Clone, Debug)]
pub struct MemorySourceForget {
    pub source_kind: String,
    pub source_id: String,
    pub tombstone: TombstoneRow,
    pub journal_entry_id: String,
    pub journal_action: String,
    pub journal_detail: Value,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{
        DATA_DIRECTORY_FORMAT_VERSION, DATA_DIRECTORY_KIND, DataDirectoryMarker,
        MEMORY_SCHEMA_VERSION, STORE_SCHEMA_VERSION,
    };
    use harness_types::ErrorCode;

    /// M7: a store written by the previous memory schema gains `memory_sources`.
    ///
    /// The downgrade is `demote_memory_schema_to_version_one` plus a reopen rather
    /// than a hand-built old database, because the table is the only difference
    /// between the two revisions. What the test refuses to allow is the failure
    /// this guards: an old database that keeps the old marker while this host
    /// believes it migrated, which would leave the freshness filter querying a
    /// table that is not there.
    #[tokio::test]
    async fn a_version_one_store_gains_the_source_table_on_reopen() {
        let temp = tempfile::TempDir::new().expect("temporary store");
        let store = crate::SqliteStore::open_writer(crate::WriterOpenOptions::new(
            temp.path(),
            harness_types::HostId::generate(),
        ))
        .await
        .expect("store opens");
        store
            .demote_memory_schema_to_version_one()
            .await
            .expect("the store is put back to the version 1 shape");
        store.close().await.expect("store closes");

        let reopened = crate::SqliteStore::open_writer(crate::WriterOpenOptions::new(
            temp.path(),
            harness_types::HostId::generate(),
        ))
        .await
        .expect("the old store reopens");
        let (has_table, recorded) = reopened
            .memory_schema_shape()
            .await
            .expect("the schema is readable");
        assert!(has_table, "the upgrade created the table");
        assert_eq!(
            recorded, MEMORY_SCHEMA_VERSION,
            "the marker and the table agree, which is the pair this migration must keep true"
        );
        reopened.close().await.expect("store closes");
    }

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
