#![forbid(unsafe_code)]

//! P5 multi-agent orchestration: delegation contracts, the durable task DAG,
//! worker scheduling with bounded budgets, host-owned isolated worktrees,
//! revision-bound result integration and delegated crash recovery.
//!
//! Workers are presets of the existing P2 runtime. This crate never becomes a
//! second agent engine and never opens a second database authority: every
//! durable write goes through the P1 transaction coordinator and host fence.

#[path = "coordination/contracts.rs"]
pub mod contracts;
#[path = "coordination/coordinator.rs"]
pub mod coordinator;
#[path = "workspaces/integration.rs"]
pub mod integration;
#[path = "coordination/scheduler.rs"]
pub mod scheduler;
#[path = "workspaces/workspace.rs"]
pub mod workspace;

pub use contracts::{
    AgentRole, BudgetUsage, CheckedRevision, DEFAULT_MAX_DEPTH, DEFAULT_MAX_MODEL_REQUESTS,
    DEFAULT_MAX_QUEUED_WORKERS, DEFAULT_MAX_WORKERS, DELEGATION_CONTRACT_VERSION, DelegatedOutcome,
    DelegatedResult, DelegationBudget, DelegationGrants, DirtyReason, GrantAction,
    IntegrationReport, IntegrationStep, OrchestratorError, SchedulerConfig, TaskBrief, TaskGraph,
    TaskNode, TaskPlan, TaskStatus, UNBOUNDED_DEPTH, UNBOUNDED_WORKERS, VerifiedSnapshot,
    WorkerRef, WorktreeRecord, WorktreeState,
};
pub use coordinator::{
    DelegationCoordinator, EvidenceVerifier, ResultVerifier, StepOutcome, fresh_run,
};
pub use integration::{
    ApplyOutcome, BranchCandidate, CheckResult, FinalApply, IntegrationOutcome, ResultIntegrator,
    workspace_candidate,
};
pub use scheduler::{
    BudgetLedger, RefusingBackend, WorkerBackend, WorkerHandle, WorkerLease, WorkerOutcome,
    WorkerRequest, WorkerScheduler,
};
pub use workspace::{ChangeSet, InputInspection, ScopeViolation, WorkspaceManager};
