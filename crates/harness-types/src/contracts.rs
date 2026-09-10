use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    AgentRunId, ArtifactId, ContentHash, ContextPacketId, ErrorCode, EventId, HarnessError,
    InstructionId, MemoryAssetId, P0_SCHEMA_VERSION, PlanItemId, PluginInstanceId, ProjectId,
    ScopeId, SessionId, TaskId, ToolExecutionId,
};

/// Validate the sole schema revision implemented by P0.
pub fn validate_schema_version(schema_version: u16) -> Result<(), HarnessError> {
    if schema_version == P0_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(HarnessError::new(
            ErrorCode::UnsupportedSchemaVersion,
            format!("schema version {schema_version} is not supported by P0"),
        ))
    }
}

fn validate_nonempty(value: &str, field: &str) -> Result<(), HarnessError> {
    if value.trim().is_empty() {
        return Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            format!("{field} must not be empty"),
        ));
    }
    Ok(())
}

fn validate_sequence(sequence: u64, field: &str) -> Result<(), HarnessError> {
    if sequence == 0 {
        return Err(HarnessError::new(
            ErrorCode::InvalidSequence,
            format!("{field} must start at 1"),
        ));
    }
    Ok(())
}

/// The authority class of a durable source. It must be explicit rather than
/// inferred from untrusted payload data.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceAuthority {
    User,
    HostPolicy,
    RuntimeObserved,
    ModelProposed,
    ExternalPlugin,
}

/// A producer is part of event provenance, not a trusted service lookup.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProducerIdentity {
    pub plugin_id: String,
    pub implementation_version: String,
}

impl ProducerIdentity {
    pub fn validate(&self) -> Result<(), HarnessError> {
        validate_nonempty(&self.plugin_id, "producer.plugin_id")?;
        validate_nonempty(
            &self.implementation_version,
            "producer.implementation_version",
        )
    }
}

/// A stable reference to an event-derived datum.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRef {
    pub event_id: EventId,
    pub sequence: u64,
    pub content_hash: ContentHash,
}

impl SourceRef {
    pub fn validate(&self) -> Result<(), HarnessError> {
        validate_sequence(self.sequence, "source_ref.sequence")
    }
}

/// The durable event envelope. Payloads are object-shaped and hashed before
/// they can be referenced by later contracts.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EventEnvelope {
    pub schema_version: u16,
    pub event_id: EventId,
    pub session_id: SessionId,
    pub seq: u64,
    pub event_type: String,
    pub producer: ProducerIdentity,
    pub authority: SourceAuthority,
    pub correlation_id: Option<EventId>,
    pub causation_id: Option<EventId>,
    pub continuity_critical: bool,
    pub payload: Map<String, Value>,
    pub payload_hash: ContentHash,
}

impl EventEnvelope {
    pub fn validate(&self) -> Result<(), HarnessError> {
        validate_schema_version(self.schema_version)?;
        validate_sequence(self.seq, "event.seq")?;
        validate_nonempty(&self.event_type, "event.event_type")?;
        self.producer.validate()?;
        let expected_hash = ContentHash::from_canonical_json(&Value::Object(self.payload.clone()))?;
        if expected_hash != self.payload_hash {
            return Err(HarnessError::new(
                ErrorCode::InvalidHash,
                "event payload_hash does not match canonical payload bytes",
            ));
        }
        Ok(())
    }

    pub fn parse_json(input: &str) -> Result<Self, HarnessError> {
        let raw: Value = serde_json::from_str(input).map_err(|_| {
            HarnessError::new(
                ErrorCode::InvalidPayload,
                "event envelope is not valid JSON",
            )
        })?;
        if raw
            .as_object()
            .is_some_and(|object| !object.contains_key("authority"))
        {
            return Err(HarnessError::new(
                ErrorCode::MissingAuthority,
                "event envelope authority is required",
            ));
        }
        let record: Self = serde_json::from_value(raw).map_err(|_| {
            HarnessError::new(
                ErrorCode::InvalidPayload,
                "event envelope is not valid JSON",
            )
        })?;
        record.validate()?;
        Ok(record)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanItemStatus {
    Pending,
    InProgress,
    Completed,
    Blocked,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlanItem {
    pub id: PlanItemId,
    pub status: PlanItemStatus,
    pub owner: Option<AgentRunId>,
    pub dependencies: Vec<PlanItemId>,
    pub evidence_refs: Vec<SourceRef>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceObservation {
    pub project_id: ProjectId,
    pub worktree_id: String,
    pub base_commit: String,
    pub observed_fingerprint: ContentHash,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceChange {
    pub path: String,
    pub before_hash: Option<ContentHash>,
    pub after_hash: Option<ContentHash>,
    pub tool_execution_id: Option<ToolExecutionId>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckOutcome {
    Passed,
    Failed,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckEvidence {
    pub command: String,
    pub outcome: CheckOutcome,
    pub tested_revision: String,
    pub artifact_id: Option<ArtifactId>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingToolState {
    Pending,
    OutcomeUnknown,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PendingToolCall {
    pub execution_id: ToolExecutionId,
    pub state: PendingToolState,
    pub reconciliation_hint: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildRunStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Canceled,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChildState {
    pub agent_run_id: AgentRunId,
    pub task_id: TaskId,
    pub status: ChildRunStatus,
    pub result_ref: Option<SourceRef>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NextActionProposal {
    pub authority: SourceAuthority,
    pub description: String,
}

/// The task projection required for resume and compaction. It remains a
/// projection, not a second authoritative event store.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkingState {
    pub schema_version: u16,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub revision: u64,
    pub through_event_seq: u64,
    pub objective_ref: SourceRef,
    pub acceptance_criteria_refs: Vec<SourceRef>,
    pub active_instruction_refs: Vec<InstructionId>,
    pub decision_refs: Vec<SourceRef>,
    pub superseded_decision_refs: Vec<SourceRef>,
    pub plan_items: Vec<PlanItem>,
    pub workspace: WorkspaceObservation,
    pub changes: Vec<WorkspaceChange>,
    pub checks: Vec<CheckEvidence>,
    pub pending_tool_calls: Vec<PendingToolCall>,
    pub children: Vec<ChildState>,
    pub blockers: Vec<String>,
    pub pending_questions: Vec<String>,
    pub next_action_proposals: Vec<NextActionProposal>,
}

impl WorkingState {
    pub fn validate(&self) -> Result<(), HarnessError> {
        validate_schema_version(self.schema_version)?;
        validate_sequence(self.through_event_seq, "working_state.through_event_seq")?;
        self.objective_ref.validate()?;
        if self.objective_ref.sequence > self.through_event_seq {
            return Err(HarnessError::new(
                ErrorCode::InvalidSequence,
                "working_state objective must not be after through_event_seq",
            ));
        }
        let mut plan_item_ids = BTreeSet::new();
        for item in &self.plan_items {
            if !plan_item_ids.insert(item.id.as_str()) {
                return Err(HarnessError::new(
                    ErrorCode::InvalidPayload,
                    "working_state plan item IDs must be unique",
                ));
            }
            for evidence_ref in &item.evidence_refs {
                evidence_ref.validate()?;
            }
        }
        validate_nonempty(
            &self.workspace.worktree_id,
            "working_state.workspace.worktree_id",
        )?;
        validate_nonempty(
            &self.workspace.base_commit,
            "working_state.workspace.base_commit",
        )?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionStatus {
    Effective,
    Superseded,
    Unknown,
}

/// The raw admitted instruction is retained by its source event; this ledger
/// records authority and mandatory status without relying on model extraction.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstructionLedgerEntry {
    pub schema_version: u16,
    pub instruction_id: InstructionId,
    pub source: SourceRef,
    pub scope: String,
    pub authority: SourceAuthority,
    pub mandatory: bool,
    pub status: InstructionStatus,
    pub source_hash: ContentHash,
    pub superseded_by: Option<InstructionId>,
}

impl InstructionLedgerEntry {
    pub fn validate(&self) -> Result<(), HarnessError> {
        validate_schema_version(self.schema_version)?;
        self.source.validate()?;
        validate_nonempty(&self.scope, "instruction.scope")
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolIntentState {
    Validated,
    Denied,
    IntentRecorded,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcomeState {
    NotStarted,
    Settled,
    OutcomeUnknown,
    Denied,
}

/// Immutable execution evidence. A future presentation/result view may never
/// rewrite the outcome represented here.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolExecutionReceipt {
    pub schema_version: u16,
    pub tool_execution_id: ToolExecutionId,
    pub task_id: TaskId,
    pub invocation_id: String,
    pub input_hash: ContentHash,
    pub policy_revision: u64,
    pub approval_id: Option<String>,
    pub intent_state: ToolIntentState,
    pub outcome_state: ToolOutcomeState,
    pub before_fingerprint: Option<ContentHash>,
    pub after_fingerprint: Option<ContentHash>,
    pub artifact_id: Option<ArtifactId>,
    pub observed_at_seq: u64,
}

impl ToolExecutionReceipt {
    pub fn validate(&self) -> Result<(), HarnessError> {
        validate_schema_version(self.schema_version)?;
        validate_nonempty(&self.invocation_id, "tool_receipt.invocation_id")?;
        validate_sequence(self.observed_at_seq, "tool_receipt.observed_at_seq")
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    User,
    Project,
    Task,
    AgentProfile,
    Session,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAssetStatus {
    Candidate,
    Active,
    Superseded,
    Invalidated,
    Archived,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Validity {
    Valid,
    Stale,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryAsset {
    pub schema_version: u16,
    pub memory_asset_id: MemoryAssetId,
    pub kind: String,
    pub owner_id: String,
    pub project_id: Option<ProjectId>,
    pub scope: MemoryScope,
    pub visibility: String,
    pub status: MemoryAssetStatus,
    pub current_version: u64,
    pub created_by: SourceAuthority,
}

impl MemoryAsset {
    pub fn validate(&self) -> Result<(), HarnessError> {
        validate_schema_version(self.schema_version)?;
        validate_nonempty(&self.kind, "memory_asset.kind")?;
        validate_nonempty(&self.owner_id, "memory_asset.owner_id")?;
        validate_nonempty(&self.visibility, "memory_asset.visibility")?;
        if self.current_version == 0 {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "memory_asset.current_version must start at 1",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryVersion {
    pub schema_version: u16,
    pub memory_asset_id: MemoryAssetId,
    pub version: u64,
    pub content_or_artifact_hash: ContentHash,
    pub content_hash: ContentHash,
    pub source_event_refs: Vec<EventId>,
    pub source_file_hashes: Vec<ContentHash>,
    pub source_commit: Option<String>,
    pub provenance_kind: String,
    pub evidence_state: String,
    pub confidence_annotation: Option<String>,
    pub validity: Validity,
    pub supersedes: Option<u64>,
    pub extractor_version: Option<String>,
}

impl MemoryVersion {
    pub fn validate(&self) -> Result<(), HarnessError> {
        validate_schema_version(self.schema_version)?;
        if self.version == 0 {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "memory_version.version must start at 1",
            ));
        }
        validate_nonempty(&self.provenance_kind, "memory_version.provenance_kind")?;
        validate_nonempty(&self.evidence_state, "memory_version.evidence_state")
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceContract {
    pub service_id: String,
    pub api_version: u16,
}

impl ServiceContract {
    pub fn validate(&self) -> Result<(), HarnessError> {
        validate_nonempty(&self.service_id, "service_contract.service_id")?;
        if self.api_version == 0 {
            return Err(HarnessError::new(
                ErrorCode::UnsupportedSchemaVersion,
                "service_contract.api_version must start at 1",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub schema_version: u16,
    pub plugin_id: String,
    pub implementation_version: String,
    pub instance_id: PluginInstanceId,
    pub scope_id: ScopeId,
    pub host_api_version: u16,
    pub config_schema_version: u16,
    pub provides: Vec<ServiceContract>,
    pub requires: Vec<ServiceContract>,
    pub capabilities: Vec<String>,
    pub blocks_recovery_when_absent: bool,
}

impl PluginManifest {
    pub fn validate(&self) -> Result<(), HarnessError> {
        validate_schema_version(self.schema_version)?;
        validate_nonempty(&self.plugin_id, "plugin_manifest.plugin_id")?;
        validate_nonempty(
            &self.implementation_version,
            "plugin_manifest.implementation_version",
        )?;
        if self.host_api_version == 0 || self.config_schema_version == 0 {
            return Err(HarnessError::new(
                ErrorCode::UnsupportedSchemaVersion,
                "plugin manifest versions must start at 1",
            ));
        }
        for contract in self.provides.iter().chain(&self.requires) {
            contract.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextPacket {
    pub schema_version: u16,
    pub packet_id: ContextPacketId,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub checkpoint_id: String,
    pub through_event_seq: u64,
    pub memory_versions: Vec<MemoryVersionRef>,
    pub rendering_version: u16,
    pub token_estimate: u64,
    pub content_hash: ContentHash,
    pub content: String,
    pub source_manifest: Vec<SourceRef>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryVersionRef {
    pub memory_asset_id: MemoryAssetId,
    pub version: u64,
}

impl ContextPacket {
    pub fn validate(&self) -> Result<(), HarnessError> {
        validate_schema_version(self.schema_version)?;
        validate_nonempty(&self.checkpoint_id, "context_packet.checkpoint_id")?;
        validate_sequence(self.through_event_seq, "context_packet.through_event_seq")?;
        if self.rendering_version == 0 {
            return Err(HarnessError::new(
                ErrorCode::UnsupportedSchemaVersion,
                "context_packet.rendering_version must start at 1",
            ));
        }
        let expected_hash = ContentHash::from_bytes(self.content.as_bytes());
        if self.content_hash != expected_hash {
            return Err(HarnessError::new(
                ErrorCode::InvalidHash,
                "context_packet.content_hash does not match rendered content",
            ));
        }
        for source in &self.source_manifest {
            source.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CliOutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CliPresentationConfig {
    #[serde(default)]
    pub output: CliOutputFormat,
}

/// Strict configuration available to P0. It contains no provider or credential
/// setting, intentionally preventing configuration from activating runtime work.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessConfig {
    pub schema_version: u16,
    #[serde(default)]
    pub cli: CliPresentationConfig,
}

impl HarnessConfig {
    pub fn validate(&self) -> Result<(), HarnessError> {
        validate_schema_version(self.schema_version)
    }
}
