#![forbid(unsafe_code)]

//! P3 coding-tool boundary.
//!
//! This crate owns the only executable coding-tool gate. Model and CLI input
//! are proposals; they become a side effect only after canonicalization,
//! policy, final-action approval, a durable intent, and a durable receipt.

mod capture;
mod contracts;
mod execution;
mod loop_service;
mod policy;
mod process;
mod secrets;
mod service;
mod turn_driver;
mod workspace;

pub use capture::{
    CAPTURE_HEADER_VERSION, CaptureHeader, DEFAULT_CAPTURE_QUOTA_BYTES, DEFAULT_HEAD_PREVIEW_BYTES,
    DEFAULT_TAIL_PREVIEW_BYTES, ProcessSpoolConfig, SpoolLimits, parse_capture_header,
};
pub use contracts::{
    ApprovalGrant, CaptureStream, CodingToolAction, ENV_REFERENCE_PREFIX, EffectClass, EnvBinding,
    GIT_LOG_DEFAULT_LIMIT, GIT_LOG_MAX_LIMIT, HISTORY_READ_DEFAULT_BYTES, HISTORY_READ_MAX_BYTES,
    HISTORY_SEARCH_DEFAULT_LIMIT, HISTORY_SEARCH_MAX_LIMIT, HistoryHitView, IsolationMode,
    MAX_ENV_BINDINGS, PROCESS_OUTPUT_PAGE_DEFAULT_BYTES, PROCESS_OUTPUT_PAGE_MAX_BYTES,
    PreparedToolRequest, TOOL_CONTRACT_VERSION, ToolCapabilities, ToolDescriptor,
    ToolExecutionView, ToolKind, ToolOutput, ToolRequest, coding_tool_descriptors,
    coding_tool_names, coding_tool_schemas, effect_class_for, normalize_env_bindings,
};
pub use loop_service::{CodingLoopResult, CodingLoopService};
// M12: the measured capability set, the probe that produces it, and the profile
// a strict request is answered against. The vocabulary is exported because an
// operator has to be able to read the evidence, not just the verdict.
pub use execution::{
    ARTIFACT_EXPORT_SCHEMA_VERSION, ArtifactExport, BoundaryBreakObservation,
    CAPABILITY_MATRIX_SCHEMA_VERSION, CONTAINMENT_BACKEND, CONTAINMENT_BACKEND_VERSION, Capability,
    CapabilityEvidence, CapabilityFinding, CapabilityMatrix, CapabilityProbe, CapabilityVerdict,
    EXECUTION_PLAN_SCHEMA_VERSION, EnvironmentPlan, ExecutionPlan, ExportProvenance, HostIdentity,
    MAX_EXPORT_BYTES, PROBE_CANARY_NAME, ProbeChild, ScopeAccess, ScopeGrant, StrictProfile,
    UnmappedControl, export_artifact, export_destination, resolve_within,
};
pub use policy::{PolicyEffect, PolicyRule, ToolPolicy};
// The environment a tool process may inherit is an operator-facing contract:
// it is published so a host can state what it exposes instead of implying a
// sandbox it does not have.
pub use process::{HostEnvironment, PROCESS_ENVIRONMENT_ALLOWLIST};
// Re-exported so a CLI that drives the loop reads acceptance from the same
// place the driver writes it.
pub use harness_runtime::AcceptanceState;
pub use secrets::{HostEnvironmentSecrets, SecretResolver};
pub use service::{ExternalToolDispatcher, ToolExecutionService, ToolObserver};
pub use turn_driver::{
    ApprovalAnswer, ApprovalGate, ApprovalMode, ApprovalProposal, ExternalToolCatalog,
    ExternalTools, GoalReport, TurnDriver, TurnLimits, TurnObserver, TurnOptions, TurnOutcome,
    TurnProgress, TurnStop,
};
pub use workspace::{observe_workspace, observed_file_hash, workspace_registration};
