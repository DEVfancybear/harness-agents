use std::collections::{BTreeMap, BTreeSet};

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
    /// The provider `call_id` this execution answers, when it came from a model
    /// call. Correlation only; the host invocation id stays the authority.
    /// Optional so records written before M4 still deserialize.
    #[serde(default)]
    pub call_id: Option<String>,
    pub input_hash: ContentHash,
    pub policy_revision: u64,
    pub approval_id: Option<String>,
    pub intent_state: ToolIntentState,
    pub outcome_state: ToolOutcomeState,
    pub before_fingerprint: Option<ContentHash>,
    pub after_fingerprint: Option<ContentHash>,
    /// Content digest immediately before a mutating workspace action.
    /// Optional so receipts written before G04 still deserialize unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_hash: Option<ContentHash>,
    /// Content digest immediately after a mutating workspace action.
    /// Optional so receipts written before G04 still deserialize unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_hash: Option<ContentHash>,
    /// Process exit status, when this receipt represents a process execution.
    /// Older receipts and non-process tools do not carry this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
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

/// Additive user configuration contract for the interactive agent.
///
/// The v1 [`HarnessConfig`] remains frozen for P0 consumers and keeps generating
/// `harness-config.v1.schema.json`; this v2 contract is emitted separately.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessConfigV2 {
    pub schema_version: u16,
    #[serde(default)]
    pub cli: CliPresentationConfig,
    #[serde(default)]
    pub provider: Option<ProviderConfigV2>,
    #[serde(default)]
    pub profiles: BTreeMap<String, ProfileConfigV2>,
    #[serde(default)]
    pub models: BTreeMap<String, ModelConfigV2>,
    #[serde(default)]
    pub limits: Option<LimitsConfigV2>,
    #[serde(default)]
    pub permissions: Option<PermissionsConfigV2>,
    #[serde(default)]
    pub mcp_servers: BTreeMap<String, McpServerConfigV2>,
    #[serde(default)]
    pub hooks: BTreeMap<String, Vec<HookConfigV2>>,
    #[serde(default)]
    pub ui: Option<UiConfigV2>,
    #[serde(default)]
    pub trust: Option<TrustConfigV2>,
}

impl HarnessConfigV2 {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.schema_version != 2 {
            return Err(HarnessError::new(
                ErrorCode::UnsupportedSchemaVersion,
                format!(
                    "unsupported harness config schema_version {}",
                    self.schema_version
                ),
            ));
        }
        if let Some(provider) = &self.provider {
            provider.validate()?;
        }
        for model in self.models.values() {
            model.validate()?;
        }
        for (name, server) in &self.mcp_servers {
            server.validate(name)?;
        }
        for (event, hooks) in &self.hooks {
            if !matches!(
                event.as_str(),
                "pre_tool_use" | "post_tool_use" | "stop" | "notification"
            ) {
                return Err(HarnessError::new(
                    ErrorCode::ConfigParseError,
                    format!("unsupported hook event {event:?}"),
                ));
            }
            for hook in hooks {
                hook.validate(event)?;
            }
        }
        Ok(())
    }
}

/// One configured MCP server. Stdio is the default transport; a remote
/// Streamable HTTP endpoint may instead name an environment variable holding
/// its bearer token. Secret values are never stored in configuration.
#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfigV2 {
    #[serde(default)]
    pub transport: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub enabled_tools: Vec<String>,
    #[serde(default)]
    pub disabled_tools: Vec<String>,
    #[serde(default)]
    pub tool_timeout_seconds: Option<u64>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub bearer_token_env: Option<String>,
}

impl McpServerConfigV2 {
    pub fn validate(&self, name: &str) -> Result<(), HarnessError> {
        let transport = self.transport.as_deref().unwrap_or("stdio");
        let bad = match transport {
            "stdio" => {
                self.command.as_deref().is_none_or(str::is_empty)
                    || self.url.is_some()
                    || self.bearer_token_env.is_some()
                    || self.args.len() > 64
                    || self.args.iter().any(|value| value.len() > 4096)
                    || self.env.iter().any(|(key, value)| {
                        key.is_empty()
                            || !key
                                .chars()
                                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                            || value.len() > 4096
                            || (is_secret_env_name(key) && !value.starts_with("secret://"))
                            || (value.starts_with("secret://")
                                && (value.len() <= "secret://".len()
                                    || !value[9..]
                                        .chars()
                                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')))
                    })
            }
            "streamable_http" => {
                self.command.is_some()
                    || !self.args.is_empty()
                    || !self.env.is_empty()
                    || self.cwd.is_some()
                    || self.url.as_deref().is_none_or(|url| {
                        !(url.starts_with("https://") || url.starts_with("http://127.0.0.1"))
                    })
                    || self.bearer_token_env.as_deref().is_some_and(|env| {
                        env.is_empty()
                            || !env
                                .chars()
                                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                    })
            }
            _ => true,
        };
        if name.is_empty()
            || name.len() > 64
            || !name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
            || bad
            || self
                .tool_timeout_seconds
                .is_some_and(|seconds| seconds == 0 || seconds > 120)
            || self.enabled_tools.len() > 256
            || self.disabled_tools.len() > 256
            || self
                .enabled_tools
                .iter()
                .chain(&self.disabled_tools)
                .any(|tool| tool.trim().is_empty() || tool.len() > 256)
        {
            return Err(HarnessError::new(
                ErrorCode::ConfigParseError,
                format!(
                    "mcp_servers.{name} has an invalid transport, command, filter, environment, URL, or timeout"
                ),
            ));
        }
        Ok(())
    }
}

fn is_secret_env_name(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    ["TOKEN", "SECRET", "PASSWORD", "API_KEY", "PRIVATE_KEY"]
        .iter()
        .any(|needle| name.contains(needle))
}

#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HookConfigV2 {
    #[serde(default)]
    pub matcher: Option<String>,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
}

impl HookConfigV2 {
    fn validate(&self, event: &str) -> Result<(), HarnessError> {
        if self.command.trim().is_empty()
            || self
                .timeout_seconds
                .is_some_and(|timeout| timeout == 0 || timeout > 60)
            || self.args.len() > 64
            || self.args.iter().any(|arg| arg.len() > 4096)
            || self
                .matcher
                .as_deref()
                .is_some_and(|matcher| matcher.trim().is_empty())
        {
            return Err(HarnessError::new(
                ErrorCode::ConfigParseError,
                format!("hooks.{event} command, matcher, args, or timeout is out of bounds"),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfigV2 {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub thinking: Option<String>,
}

impl ProviderConfigV2 {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if let Some(protocol) = &self.protocol
            && !matches!(protocol.as_str(), "openai_chat" | "anthropic_messages")
        {
            return Err(HarnessError::new(
                ErrorCode::ConfigParseError,
                "provider.protocol must be openai_chat or anthropic_messages",
            ));
        }
        if let Some(thinking) = &self.thinking
            && !matches!(thinking.as_str(), "off" | "on")
        {
            return Err(HarnessError::new(
                ErrorCode::ConfigParseError,
                "provider.thinking must be off or on",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileConfigV2 {
    #[serde(default)]
    pub provider: Option<ProviderConfigV2>,
    #[serde(default)]
    pub limits: Option<LimitsConfigV2>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfigV2 {
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub input_price_per_mtok: Option<f64>,
    #[serde(default)]
    pub output_price_per_mtok: Option<f64>,
    #[serde(default)]
    pub supports_images: Option<bool>,
}

impl ModelConfigV2 {
    fn validate(&self) -> Result<(), HarnessError> {
        if self.context_window.is_some_and(|window| window == 0)
            || self
                .input_price_per_mtok
                .is_some_and(|price| !price.is_finite() || price < 0.0)
            || self
                .output_price_per_mtok
                .is_some_and(|price| !price.is_finite() || price < 0.0)
        {
            return Err(HarnessError::new(
                ErrorCode::ConfigParseError,
                "model prices must be finite and non-negative",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsConfigV2 {
    #[serde(default)]
    pub max_steps: Option<u32>,
    #[serde(default)]
    pub max_tool_calls: Option<u32>,
    #[serde(default)]
    pub deadline_seconds: Option<u64>,
    #[serde(default)]
    pub continuations: Option<u32>,
    #[serde(default)]
    pub output_reservation_tokens: Option<u64>,
    #[serde(default)]
    pub compaction_reserve_tokens: Option<u64>,
    #[serde(default)]
    pub max_retry_after_seconds: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionsConfigV2 {
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UiConfigV2 {
    #[serde(default)]
    pub renderer: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub bell: Option<bool>,
    #[serde(default)]
    pub notify_command: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrustConfigV2 {
    #[serde(default)]
    pub projects: Vec<String>,
}
