#![forbid(unsafe_code)]

//! Shared, versioned contracts for the Harness Agents workspace.
//!
//! P0 deliberately contains no persistence, policy, model, or agent runtime.

mod contracts;
mod error;
mod fixture;
mod hash;
mod ids;
mod schema;

pub use contracts::{
    CheckEvidence, CheckOutcome, ChildRunStatus, ChildState, CliOutputFormat,
    CliPresentationConfig, ContextPacket, EventEnvelope, HarnessConfig, InstructionLedgerEntry,
    InstructionStatus, MemoryAsset, MemoryAssetStatus, MemoryScope, MemoryVersion,
    MemoryVersionRef, NextActionProposal, PendingToolCall, PendingToolState, PlanItem,
    PlanItemStatus, PluginManifest, ProducerIdentity, ServiceContract, SourceAuthority, SourceRef,
    ToolExecutionReceipt, ToolIntentState, ToolOutcomeState, Validity, WorkingState,
    WorkspaceChange, WorkspaceObservation, validate_schema_version,
};
pub use error::{ErrorCode, HarnessError};
pub use fixture::{ContinuationFixtureReport, verify_continuation_fixture};
pub use hash::{ContentHash, canonical_json_bytes};
pub use ids::{
    AgentProfileId, AgentRunId, ArtifactId, ContextPacketId, EventId, InstructionId, MemoryAssetId,
    PlanItemId, PluginInstanceId, ProjectId, ScopeId, SessionId, TaskId, ToolExecutionId,
};
pub use schema::{SchemaDocument, generated_schema_documents};

/// The only schema revision accepted by P0 contracts.
pub const P0_SCHEMA_VERSION: u16 = 1;
