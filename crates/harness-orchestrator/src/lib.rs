#![forbid(unsafe_code)]

//! P5 multi-agent orchestration: delegation contracts, the durable task DAG,
//! worker scheduling with bounded budgets, host-owned isolated worktrees,
//! revision-bound result integration and delegated crash recovery.
//!
//! Workers are presets of the existing P2 runtime. This crate never becomes a
//! second agent engine and never opens a second database authority: every
//! durable write goes through the P1 transaction coordinator and host fence.

pub mod contracts;
pub mod coordinator;
pub mod integration;
pub mod memory;
pub mod scheduler;
pub mod workspace;

pub use contracts::{
    AgentRole, BudgetUsage, CheckedRevision, DEFAULT_MAX_DEPTH, DEFAULT_MAX_MODEL_REQUESTS,
    DEFAULT_MAX_QUEUED_WORKERS, DEFAULT_MAX_WORKERS, DELEGATION_CONTRACT_VERSION, DelegatedOutcome,
    DelegatedResult, DelegationBudget, DelegationGrants, DeliveryNotifier, DeliveryState,
    DirtyReason, GrantAction, IntegrationReport, IntegrationStep, OrchestratorError, ParentDelivery,
    SchedulerConfig, TaskBrief, TaskGraph, TaskMemoryBinding, TaskNode, TaskPlan, TaskStatus,
    VerifiedSnapshot, WorkerRef, WorktreeRecord, WorktreeState,
};
pub use coordinator::{
    DelegationCoordinator, EvidenceVerifier, ResultVerifier, StepOutcome, TaskSession,
    coordinator_generation, fresh_run,
};
pub use integration::{
    BranchCandidate, FinalApply, IntegrationOutcome, ResultIntegrator, workspace_candidate,
};
pub use memory::{DelegatedMemoryBinding, DelegatedMemoryService, MemoryBoundary};
pub use scheduler::{
    BudgetLedger, RefusingBackend, WorkerBackend, WorkerHandle, WorkerLease, WorkerOutcome,
    WorkerRequest, WorkerScheduler,
};
pub use workspace::{ChangeSet, InputInspection, ScopeViolation, WorkspaceManager};
