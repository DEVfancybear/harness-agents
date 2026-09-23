//! Transaction inputs shared by application services and durable store adapters.
//!
//! These are values for one atomic store operation. Keeping them in
//! `harness-types` prevents application crates from depending on a concrete
//! database crate merely to describe what must commit together.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AgentRunId, ArtifactId, BudgetId, BudgetReservationId, ContentHash, ContextPacketId,
    EventEnvelope, EventId, InputId, InstructionLedgerEntry, RequestId, SessionId, StepId, TaskId,
    ToolApprovalId, ToolExecutionId, ToolExecutionReceipt, ToolOutcomeState, WorkingState,
};

/// Provenance marker committed with the projection and journal event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceWorkMarker {
    pub marker_id: String,
    pub event_id: EventId,
    pub sequence: u64,
    pub kind: String,
    pub status: String,
}

/// All records that must commit together to admit a user input.
#[derive(Clone, Debug, PartialEq)]
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

/// The durable result of admitting one input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmissionAck {
    pub input_id: InputId,
    pub event_id: EventId,
    pub sequence: u64,
    pub idempotent_replay: bool,
}

/// The atomic request to create or recover the durable run for an input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunStartRequest {
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub input_id: InputId,
    pub budget_id: Option<BudgetId>,
    pub expected_owner_generation: u64,
}

/// The durable run identity and revision token returned by the store.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunLease {
    pub run_id: AgentRunId,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub input_id: InputId,
    pub owner_generation: u64,
    pub revision: u64,
}

/// A reservation committed in the same transaction that freezes a provider step.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrozenBudgetReservation {
    pub reservation_id: BudgetReservationId,
    pub budget_id: BudgetId,
    pub operation_id: String,
    pub origin: String,
    pub upper_bound_tokens: u64,
}

/// A provider step whose packet/request are already frozen by the caller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrozenRunStep {
    pub step_id: StepId,
    pub run_id: AgentRunId,
    pub step_index: u32,
    pub request_id: RequestId,
    pub packet_id: ContextPacketId,
    pub manifest_hash: ContentHash,
    pub source_sequence: u64,
    pub state: String,
    pub stop_reason: Option<String>,
}

/// CAS input for the freeze-step + budget-reservation transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FreezeStepCommit {
    pub run_id: AgentRunId,
    pub expected_owner_generation: u64,
    pub expected_revision: u64,
    pub step: FrozenRunStep,
    pub reservation: Option<FrozenBudgetReservation>,
}

/// All records that must commit together to record a settled synthetic receipt.
#[derive(Clone, Debug, PartialEq)]
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

/// Immutable binding persisted for a single-use approval.
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

/// Everything required to durably consume an approval and record an intent.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolIntentCommit {
    pub expected_sequence: u64,
    pub event: EventEnvelope,
    pub intent: ToolIntentRecord,
    pub working_state: WorkingState,
    pub marker: SourceWorkMarker,
}

/// Everything required to settle a previously committed tool intent.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolSettlementCommit {
    pub expected_sequence: u64,
    pub event: EventEnvelope,
    pub receipt: ToolExecutionReceipt,
    pub working_state: WorkingState,
    pub marker: SourceWorkMarker,
    pub artifact: Option<PublishedArtifact>,
    pub final_status: ToolIntentStatus,
}

/// An atomic task update that consumes a tool approval without fabricating a receipt.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolTaskUpdateCommit {
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub expected_sequence: u64,
    pub event: EventEnvelope,
    pub working_state: WorkingState,
    pub marker: SourceWorkMarker,
    pub approval: ToolApprovalBinding,
}

/// Bytes flushed and atomically published before a database transaction references them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishedArtifact {
    pub artifact_id: ArtifactId,
    pub content_hash: ContentHash,
    pub byte_len: u64,
    pub relative_path: String,
}

/// The durable worker report committed with a parent delivery.
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

/// The durable state of one delegated task transition.
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

/// Observed usage charged to the task transitioned by the same delivery commit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BudgetUsageRecord {
    pub model_requests: u32,
    pub retries: u32,
    pub cost_units: u64,
}

/// A task transition and its result/message outbox committed atomically.
#[derive(Clone, Debug, PartialEq)]
pub struct DeliveryCommit {
    pub task_transition: StoredTaskNodeRecord,
    pub result: Option<StoredDelegatedResultRecord>,
    pub delivery: ParentDeliveryRecord,
    pub usage: Option<BudgetUsageRecord>,
}

/// A committed journal position.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommitRef {
    pub event_id: EventId,
    pub seq: u64,
    pub state_revision: u64,
}

/// The observable part of one execution receipt.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationReceiptRef {
    pub invocation_id: crate::ToolInvocationId,
    pub outcome: ToolOutcomeState,
    pub before_fingerprint: Option<ContentHash>,
    pub after_fingerprint: Option<ContentHash>,
    pub exit_code: Option<i32>,
    pub uncertainty: Option<String>,
}

/// The read-only recovery view returned by the store.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortRecoveryView {
    pub session_id: SessionId,
    pub replayed_through_sequence: u64,
    pub pending_effects: u64,
    pub blocked_reason: Option<String>,
}
