#![forbid(unsafe_code)]

//! Shared, versioned contracts for the Harness Agents workspace.
//!
//! P0 deliberately contains no persistence, policy, model, or agent runtime.

pub mod acceptance;
mod contracts;
mod error;
mod fixture;
mod hash;
mod ids;
mod ports;
mod schema;
mod scope;
mod store_transactions;
mod version;

pub use acceptance::{
    AcceptanceActor, AcceptanceCommand, AcceptanceDecision, AcceptanceEvent, AcceptanceRecord,
    AcceptanceTransition, CriterionEvidence, CriterionState, CriterionStatus,
};
pub use contracts::{
    CheckEvidence, CheckOutcome, ChildRunStatus, ChildState, CliOutputFormat,
    CliPresentationConfig, ContextPacket, EventEnvelope, HarnessConfig, HarnessConfigV2,
    InstructionLedgerEntry, InstructionStatus, LimitsConfigV2, McpServerConfigV2, MemoryAsset,
    MemoryAssetStatus, MemoryScope, MemoryVersion, MemoryVersionRef, ModelConfigV2,
    NextActionProposal, PendingToolCall, PendingToolState, PermissionsConfigV2, PlanItem,
    PlanItemStatus, PluginManifest, ProducerIdentity, ProfileConfigV2, ProviderConfigV2,
    ServiceContract, SourceAuthority, SourceRef, ToolExecutionReceipt, ToolIntentState,
    ToolOutcomeState, TrustConfigV2, UiConfigV2, Validity, WorkingState, WorkspaceChange,
    WorkspaceObservation, validate_schema_version,
};
pub use error::{ErrorCode, ErrorReport, HarnessError, RetryClass};
pub use fixture::{ContinuationFixtureReport, verify_continuation_fixture};
pub use hash::{ContentHash, canonical_json_bytes};
pub use ids::{
    AgentProfileId, AgentRunId, ArtifactId, BudgetId, BudgetReservationId, CompositionSnapshotId,
    ContextPacketId, EventId, ExtensionInstanceId, FixedIdSource, HostId, IdSource, InputId,
    InstructionId, MemoryAssetId, PlanItemId, PluginInstanceId, ProjectId, ProviderAttemptId,
    QuestionId, RequestId, RuntimeCommandId, ScopeId, SessionId, SkillId, SnapshotId, StepId,
    SystemIdSource, TaskId, ToolApprovalId, ToolExecutionId, ToolInvocationId,
};
pub use ports::{
    AdmissionOutcome, CAPABILITY_MEMORY_READ, CAPABILITY_TOOLS_READ, CAPABILITY_TOOLS_WRITE,
    StoreFuture, StorePort, documented_capabilities,
};
pub use schema::{SchemaDocument, generated_schema_documents};
pub use scope::{ScopeContext, ScopeTarget};
pub use store_transactions::{
    AdmissionAck, AdmissionCommit, BudgetUsageRecord, CommitRef, DeliveryCommit, FreezeStepCommit,
    FrozenBudgetReservation, FrozenRunStep, InvocationReceiptRef, ParentDeliveryRecord,
    PortRecoveryView, PublishedArtifact, ReceiptAck, ReceiptCommit, RunLease, RunStartRequest,
    SourceWorkMarker, StoredDelegatedResultRecord, StoredTaskNodeRecord, ToolApprovalBinding,
    ToolIntentCommit, ToolIntentRecord, ToolIntentStatus, ToolSettlementCommit,
    ToolTaskUpdateCommit,
};
pub use version::{VersionedDocument, known_document_kinds};

/// The only schema revision accepted by P0 contracts.
pub const P0_SCHEMA_VERSION: u16 = 1;
