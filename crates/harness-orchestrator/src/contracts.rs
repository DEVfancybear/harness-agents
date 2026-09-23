//! P5 delegation, task-DAG, budget and worker contracts.
//!
//! Every authority-bearing field here is a host-supplied value. A role preset
//! name never grants a permission: the host materializes grants, scopes, depth
//! and budgets explicitly before a worker can be created.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use harness_types::{
    AcceptanceCommand, AcceptanceRecord, AgentProfileId, AgentRunId, ArtifactId, CheckOutcome,
    ContentHash, CriterionEvidence, CriterionState, CriterionStatus, ErrorCode, ProjectId,
    SessionId, TaskId, ToolExecutionReceipt, WorkspaceObservation,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Serialization revision for every P5 delegation contract.
pub const DELEGATION_CONTRACT_VERSION: u16 = 1;

pub const DEFAULT_MAX_WORKERS: u32 = 3;
/// The host cap on dispatched-but-not-yet-settled workers. See
/// [`SchedulerConfig::max_queued_workers`] for why a bound exists at all.
pub const DEFAULT_MAX_QUEUED_WORKERS: u32 = 8;
pub const DEFAULT_MAX_DEPTH: u32 = 2;
pub const DEFAULT_MAX_MODEL_REQUESTS: u32 = 24;
pub const MAX_ROLE_PRESETS: usize = 4;

/// Typed P5 failure carrying a stable `ErrorCode` plus detail.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct OrchestratorError {
    code: ErrorCode,
    message: String,
}

impl OrchestratorError {
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl From<harness_types::HarnessError> for OrchestratorError {
    fn from(error: harness_types::HarnessError) -> Self {
        Self::new(error.code(), error.to_string())
    }
}

impl From<harness_store_sqlite::StoreError> for OrchestratorError {
    fn from(error: harness_store_sqlite::StoreError) -> Self {
        Self::new(error.code(), error.to_string())
    }
}

impl From<harness_runtime::RuntimeError> for OrchestratorError {
    fn from(error: harness_runtime::RuntimeError) -> Self {
        Self::new(error.code(), error.to_string())
    }
}

/// The role presets P5 ships. Roles are presets of the same runtime, and the
/// preset name grants nothing on its own.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Coordinator,
    Explorer,
    Coder,
    Reviewer,
    Verifier,
}

impl AgentRole {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Coordinator => "coordinator",
            Self::Explorer => "explorer",
            Self::Coder => "coder",
            Self::Reviewer => "reviewer",
            Self::Verifier => "verifier",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "coordinator" => Some(Self::Coordinator),
            "explorer" => Some(Self::Explorer),
            "coder" => Some(Self::Coder),
            "reviewer" => Some(Self::Reviewer),
            "verifier" => Some(Self::Verifier),
            _ => None,
        }
    }

    #[must_use]
    pub const fn worker_roles() -> [Self; 3] {
        [Self::Explorer, Self::Coder, Self::Verifier]
    }

    /// Only the coder role is allowed to request an editing workspace.
    #[must_use]
    pub const fn default_edit_capable(self) -> bool {
        matches!(self, Self::Coder)
    }
}

impl fmt::Display for AgentRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A concrete worker identity chosen by the host. `profile` is stable across
/// activations, `run` is one activation and `generation` fences a dead run.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkerRef {
    pub profile_id: AgentProfileId,
    pub run_id: AgentRunId,
    pub role: AgentRole,
    pub generation: u64,
}

/// One permission value the host grants. The model cannot raise these.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantAction {
    Read,
    Propose,
    Publish,
    Bind,
    Invalidate,
}

impl GrantAction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Propose => "propose",
            Self::Publish => "publish",
            Self::Bind => "bind",
            Self::Invalidate => "invalidate",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "read" => Some(Self::Read),
            "propose" => Some(Self::Propose),
            "publish" => Some(Self::Publish),
            "bind" => Some(Self::Bind),
            "invalidate" => Some(Self::Invalidate),
            _ => None,
        }
    }
}

/// Host-issued authority for one worker. Grants intersect with the parent's
/// grants; a child can never widen them.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DelegationGrants {
    pub project_id: ProjectId,
    pub task_id: TaskId,
    pub actions: Vec<GrantAction>,
    /// Repository-relative write scope. Empty means the worker may not write.
    pub write_scope: Vec<String>,
    pub edit_workspace: bool,
    pub max_depth: u32,
    pub budget: DelegationBudget,
}

impl DelegationGrants {
    #[must_use]
    pub fn allows(&self, action: GrantAction) -> bool {
        self.actions.contains(&action)
    }

    #[must_use]
    pub fn may_write(&self) -> bool {
        self.edit_workspace && !self.write_scope.is_empty()
    }

    pub fn validate(&self) -> Result<(), OrchestratorError> {
        if self.max_depth > DEFAULT_MAX_DEPTH {
            return Err(OrchestratorError::new(
                ErrorCode::DelegationDepthExceeded,
                format!(
                    "delegation depth {} exceeds the P5 maximum of {DEFAULT_MAX_DEPTH}",
                    self.max_depth
                ),
            ));
        }
        self.budget.validate()?;
        if self.edit_workspace && self.write_scope.is_empty() {
            return Err(OrchestratorError::new(
                ErrorCode::ScopeAuthorityDenied,
                "an editing worker requires a non-empty write scope",
            ));
        }
        for path in &self.write_scope {
            validate_scope_path(path)?;
        }
        Ok(())
    }

    /// Intersect a child request with this grant. The result is never wider.
    pub fn intersect(
        &self,
        requested: &DelegationGrants,
    ) -> Result<DelegationGrants, OrchestratorError> {
        if requested.project_id != self.project_id || requested.task_id != self.task_id {
            return Err(OrchestratorError::new(
                ErrorCode::ScopeAuthorityDenied,
                "a delegated grant cannot move to another project or task",
            ));
        }
        // A delegated grant may keep the parent's depth allowance but can never
        // widen it. The host coordinator and a worker are both at depth 1, so
        // equality is legitimate; a larger child depth is not.
        if requested.max_depth > self.max_depth {
            return Err(OrchestratorError::new(
                ErrorCode::DelegationDepthExceeded,
                "delegated depth cannot exceed the parent depth",
            ));
        }
        let actions = requested
            .actions
            .iter()
            .copied()
            .filter(|action| self.allows(*action))
            .collect::<Vec<_>>();
        if actions.len() != requested.actions.len() {
            return Err(OrchestratorError::new(
                ErrorCode::ScopeAuthorityDenied,
                "a delegated grant cannot add an action the parent does not hold",
            ));
        }
        let mut write_scope = Vec::new();
        for path in &requested.write_scope {
            if !self.write_scope.iter().any(|parent| path == parent) {
                return Err(OrchestratorError::new(
                    ErrorCode::ScopeAuthorityDenied,
                    format!("write scope {path} is not contained in the parent write scope"),
                ));
            }
            write_scope.push(path.clone());
        }
        if requested.edit_workspace && !self.edit_workspace {
            return Err(OrchestratorError::new(
                ErrorCode::ScopeAuthorityDenied,
                "a delegated grant cannot enable an editing workspace the parent lacks",
            ));
        }
        let budget = self.budget.intersect(&requested.budget);
        let intersected = Self {
            project_id: self.project_id.clone(),
            task_id: self.task_id.clone(),
            actions,
            write_scope,
            edit_workspace: requested.edit_workspace,
            max_depth: requested.max_depth,
            budget,
        };
        intersected.validate()?;
        Ok(intersected)
    }
}

fn validate_scope_path(path: &str) -> Result<(), OrchestratorError> {
    if path.trim().is_empty()
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.contains("..")
        || path.contains(':')
    {
        return Err(OrchestratorError::new(
            ErrorCode::ScopeAuthorityDenied,
            format!("write scope {path} is not a repository-relative path"),
        ));
    }
    Ok(())
}

/// Budget dimensions. Missing usage is never treated as zero: a worker that
/// never reported usage keeps `reported_requests` at its recorded value and the
/// scheduler refuses to invent headroom.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DelegationBudget {
    pub max_workers: u32,
    pub max_model_requests: u32,
    pub max_retries: u32,
    pub max_cost_units: u64,
}

impl Default for DelegationBudget {
    fn default() -> Self {
        Self {
            max_workers: DEFAULT_MAX_WORKERS,
            max_model_requests: DEFAULT_MAX_MODEL_REQUESTS,
            max_retries: 1,
            max_cost_units: 0,
        }
    }
}

impl DelegationBudget {
    pub fn validate(&self) -> Result<(), OrchestratorError> {
        if self.max_workers == 0 || self.max_workers > DEFAULT_MAX_WORKERS {
            return Err(OrchestratorError::new(
                ErrorCode::BudgetExhausted,
                format!(
                    "worker budget {} is outside 1..={DEFAULT_MAX_WORKERS}",
                    self.max_workers
                ),
            ));
        }
        if self.max_model_requests == 0 {
            return Err(OrchestratorError::new(
                ErrorCode::BudgetExhausted,
                "model request budget must be positive",
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn intersect(&self, requested: &Self) -> Self {
        Self {
            max_workers: self.max_workers.min(requested.max_workers),
            max_model_requests: self.max_model_requests.min(requested.max_model_requests),
            max_retries: self.max_retries.min(requested.max_retries),
            max_cost_units: self.max_cost_units.min(requested.max_cost_units),
        }
    }

    /// Deduct observed usage. Returns `budget_exhausted` rather than clamping.
    pub fn charge(&mut self, usage: BudgetUsage) -> Result<(), OrchestratorError> {
        let requests = self
            .max_model_requests
            .checked_sub(usage.model_requests)
            .ok_or_else(|| {
                OrchestratorError::new(
                    ErrorCode::BudgetExhausted,
                    "model request budget is exhausted",
                )
            })?;
        let retries = self.max_retries.checked_sub(usage.retries).ok_or_else(|| {
            OrchestratorError::new(ErrorCode::BudgetExhausted, "retry budget is exhausted")
        })?;
        self.max_model_requests = requests;
        self.max_retries = retries;
        Ok(())
    }
}

/// Observed usage reported by one worker completion.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct BudgetUsage {
    pub model_requests: u32,
    pub retries: u32,
}

/// The host-authored brief handed to one worker.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaskBrief {
    pub schema_version: u16,
    pub task_id: TaskId,
    pub title: String,
    pub objective: String,
    pub acceptance_criteria: Vec<String>,
    /// Immutable inputs: repository-relative observations the worker may read.
    pub inputs: Vec<String>,
    /// The verified input snapshot every editing worker must start from.
    pub base_snapshot: String,
    pub base_commit: String,
    pub workspace: WorkspaceObservation,
    pub grants: DelegationGrants,
    pub expected_artifacts: Vec<String>,
    pub deadline_unix_ms: Option<u64>,
    pub role: AgentRole,
}

impl TaskBrief {
    pub fn validate(&self) -> Result<(), OrchestratorError> {
        if self.schema_version != DELEGATION_CONTRACT_VERSION {
            return Err(OrchestratorError::new(
                ErrorCode::UnsupportedSchemaVersion,
                format!("task brief schema {} is not supported", self.schema_version),
            ));
        }
        if self.title.trim().is_empty() || self.objective.trim().is_empty() {
            return Err(OrchestratorError::new(
                ErrorCode::InvalidPayload,
                "a task brief requires a title and an objective",
            ));
        }
        if self.acceptance_criteria.is_empty() {
            return Err(OrchestratorError::new(
                ErrorCode::InvalidPayload,
                "a task brief requires explicit acceptance criteria",
            ));
        }
        if self.base_snapshot.trim().is_empty() || self.base_commit.trim().is_empty() {
            return Err(OrchestratorError::new(
                ErrorCode::InvalidPayload,
                "a task brief requires a base snapshot and base commit",
            ));
        }
        self.grants.validate()?;
        if self.grants.task_id != self.task_id {
            return Err(OrchestratorError::new(
                ErrorCode::AmbiguousTaskOwner,
                "task brief task_id does not match its grants",
            ));
        }
        if self.role.default_edit_capable() != self.grants.edit_workspace
            && self.role != AgentRole::Coder
        {
            return Err(OrchestratorError::new(
                ErrorCode::ScopeAuthorityDenied,
                format!(
                    "role {} cannot hold an editing workspace",
                    self.role.as_str()
                ),
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn content_hash(&self) -> Option<ContentHash> {
        ContentHash::from_canonical_json(&serde_json::to_value(self).ok()?).ok()
    }
}

/// Authoritative task lifecycle.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Ready,
    Assigned,
    Running,
    Blocked,
    Completed,
    Failed,
    Canceled,
}

impl TaskStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::Assigned => "assigned",
            Self::Running => "running",
            Self::Blocked => "blocked",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "ready" => Some(Self::Ready),
            "assigned" => Some(Self::Assigned),
            "running" => Some(Self::Running),
            "blocked" => Some(Self::Blocked),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "canceled" => Some(Self::Canceled),
            _ => None,
        }
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Canceled)
    }

    #[must_use]
    pub const fn is_live(self) -> bool {
        matches!(
            self,
            Self::Ready | Self::Assigned | Self::Running | Self::Blocked
        )
    }

    /// Authority for one task transition. Completed work is terminal so a
    /// restart cannot reassign it.
    pub fn transition(self, next: Self) -> Result<Self, OrchestratorError> {
        if self == next {
            return Ok(next);
        }
        let allowed = match self {
            Self::Pending => matches!(next, Self::Ready | Self::Canceled | Self::Blocked),
            Self::Ready => matches!(
                next,
                Self::Pending | Self::Assigned | Self::Blocked | Self::Canceled
            ),
            Self::Assigned => matches!(
                next,
                Self::Ready | Self::Running | Self::Blocked | Self::Canceled
            ),
            Self::Running => matches!(
                next,
                Self::Blocked | Self::Completed | Self::Failed | Self::Canceled | Self::Ready
            ),
            Self::Blocked => matches!(next, Self::Ready | Self::Failed | Self::Canceled),
            Self::Completed | Self::Failed | Self::Canceled => false,
        };
        if allowed {
            Ok(next)
        } else {
            Err(OrchestratorError::new(
                ErrorCode::InvalidStateTransition,
                format!(
                    "invalid task transition {} -> {}",
                    self.as_str(),
                    next.as_str()
                ),
            ))
        }
    }
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One node of the delegation DAG.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TaskNode {
    pub schema_version: u16,
    pub task_id: TaskId,
    pub parent_task_id: Option<TaskId>,
    pub role: AgentRole,
    pub brief: TaskBrief,
    pub depends_on: Vec<TaskId>,
    pub status: TaskStatus,
    pub depth: u32,
    pub revision: u64,
}

impl TaskNode {
    pub fn validate(&self) -> Result<(), OrchestratorError> {
        if self.schema_version != DELEGATION_CONTRACT_VERSION {
            return Err(OrchestratorError::new(
                ErrorCode::UnsupportedSchemaVersion,
                format!("task node schema {} is not supported", self.schema_version),
            ));
        }
        if self.brief.task_id != self.task_id {
            return Err(OrchestratorError::new(
                ErrorCode::AmbiguousTaskOwner,
                "task node brief does not describe this task",
            ));
        }
        if self.depth > self.brief.grants.max_depth {
            return Err(OrchestratorError::new(
                ErrorCode::DelegationDepthExceeded,
                format!(
                    "task {} at depth {} exceeds its granted depth {}",
                    self.task_id, self.depth, self.brief.grants.max_depth
                ),
            ));
        }
        self.brief.validate()
    }
}

/// The complete candidate DAG a coordinator proposes.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TaskGraph {
    pub schema_version: u16,
    pub coordinator: WorkerRef,
    pub tasks: Vec<TaskNode>,
}

/// Canonically ordered DAG plan that is proven acyclic and unambiguous.
#[derive(Clone, Debug, PartialEq)]
pub struct TaskPlan {
    pub schema_version: u16,
    pub coordinator: WorkerRef,
    /// Insertion order used for deterministic scheduling among ready tasks.
    pub order: Vec<TaskId>,
    pub nodes: BTreeMap<TaskId, TaskNode>,
    /// Dependency-first execution order.
    pub topological_order: Vec<TaskId>,
}

impl TaskPlan {
    /// Validate every structural rule before any task is admitted.
    #[allow(clippy::too_many_lines)] // One admission gate; splitting hides ordering.
    pub fn compile(graph: TaskGraph) -> Result<Self, OrchestratorError> {
        if graph.schema_version != DELEGATION_CONTRACT_VERSION {
            return Err(OrchestratorError::new(
                ErrorCode::UnsupportedSchemaVersion,
                format!(
                    "task graph schema {} is not supported",
                    graph.schema_version
                ),
            ));
        }
        if graph.coordinator.role != AgentRole::Coordinator {
            return Err(OrchestratorError::new(
                ErrorCode::AmbiguousTaskOwner,
                "a task graph must be owned by a coordinator worker",
            ));
        }
        if graph.tasks.len() > DEFAULT_MAX_WORKERS as usize {
            return Err(OrchestratorError::new(
                ErrorCode::BudgetExhausted,
                format!(
                    "task graph has {} tasks but P5 admits at most {DEFAULT_MAX_WORKERS} workers",
                    graph.tasks.len()
                ),
            ));
        }
        let mut order = Vec::new();
        let mut nodes: BTreeMap<TaskId, TaskNode> = BTreeMap::new();
        for node in graph.tasks {
            node.validate()?;
            if node
                .parent_task_id
                .as_ref()
                .is_some_and(|parent| parent == &node.task_id)
            {
                return Err(OrchestratorError::new(
                    ErrorCode::AmbiguousTaskOwner,
                    format!("task {} cannot be its own parent", node.task_id),
                ));
            }
            if nodes.contains_key(&node.task_id) {
                return Err(OrchestratorError::new(
                    ErrorCode::DuplicateTaskId,
                    format!("task {} appears twice in one graph", node.task_id),
                ));
            }
            order.push(node.task_id.clone());
            nodes.insert(node.task_id.clone(), node);
        }
        if nodes.is_empty() {
            return Err(OrchestratorError::new(
                ErrorCode::InvalidPayload,
                "a task graph requires at least one task",
            ));
        }
        // Depth comes from the parent chain inside this graph. A parent that is
        // not a graph node is the host coordinator, which owns the root of the
        // delegation and is therefore depth 0 outside the graph.
        let mut scopes: BTreeMap<String, TaskId> = BTreeMap::new();
        for task_id in &order {
            let depth = depth_from_parents(&nodes, task_id)?;
            if depth > DEFAULT_MAX_DEPTH {
                return Err(OrchestratorError::new(
                    ErrorCode::DelegationDepthExceeded,
                    format!("task {task_id} at depth {depth} exceeds the P5 depth cap"),
                ));
            }
            let node = nodes.get_mut(task_id).ok_or_else(|| {
                OrchestratorError::new(ErrorCode::TaskNotFound, "task disappeared during compile")
            })?;
            if node.depth != depth {
                node.depth = depth;
            }
            if depth > node.brief.grants.max_depth {
                return Err(OrchestratorError::new(
                    ErrorCode::DelegationDepthExceeded,
                    format!(
                        "task {task_id} at depth {depth} exceeds its granted depth {}",
                        node.brief.grants.max_depth
                    ),
                ));
            }
        }
        // Ambiguous owner: two tasks may not claim the same write scope, which
        // would make concurrent edits undecidable. Exact duplicates and nested
        // scopes are both ambiguous: `src` and `src/foo` name overlapping files,
        // so two workers could edit the same tree under different grants.
        for task_id in &order {
            let node = nodes.get(task_id).ok_or_else(|| {
                OrchestratorError::new(ErrorCode::TaskNotFound, "task disappeared during compile")
            })?;
            for path in &node.brief.grants.write_scope {
                let normalized = normalize_scope_path(path);
                for (existing, owner) in &scopes {
                    if scopes_overlap(&normalized, existing) {
                        return Err(OrchestratorError::new(
                            ErrorCode::AmbiguousTaskOwner,
                            format!(
                                "write scope {path} overlaps {existing}, claimed by both {owner} and {task_id}"
                            ),
                        ));
                    }
                }
                scopes.insert(normalized, task_id.clone());
            }
        }
        // Dependency validation and cycle detection.
        for task_id in &order {
            let node = nodes.get(task_id).ok_or_else(|| {
                OrchestratorError::new(ErrorCode::TaskNotFound, "task disappeared during compile")
            })?;
            let mut seen = BTreeSet::new();
            for dependency in &node.depends_on {
                if dependency == task_id {
                    return Err(OrchestratorError::new(
                        ErrorCode::DagCycle,
                        format!("task {task_id} depends on itself"),
                    ));
                }
                if !nodes.contains_key(dependency) {
                    return Err(OrchestratorError::new(
                        ErrorCode::UnknownTaskDependency,
                        format!("task {task_id} depends on unknown task {dependency}"),
                    ));
                }
                if !seen.insert(dependency.clone()) {
                    return Err(OrchestratorError::new(
                        ErrorCode::DuplicateTaskId,
                        format!("task {task_id} lists dependency {dependency} twice"),
                    ));
                }
            }
        }
        let topological_order = topological_sort(&nodes, &order)?;
        Ok(Self {
            schema_version: DELEGATION_CONTRACT_VERSION,
            coordinator: graph.coordinator,
            order,
            nodes,
            topological_order,
        })
    }

    #[must_use]
    pub fn node(&self, task_id: &TaskId) -> Option<&TaskNode> {
        self.nodes.get(task_id)
    }

    /// Tasks whose dependencies all completed and which may be dispatched now.
    #[must_use]
    pub fn ready_tasks(&self) -> Vec<TaskId> {
        let mut ready = Vec::new();
        for task_id in &self.topological_order {
            let Some(node) = self.nodes.get(task_id) else {
                continue;
            };
            if node.status != TaskStatus::Pending && node.status != TaskStatus::Ready {
                continue;
            }
            let dependencies_ready = node.depends_on.iter().all(|dependency| {
                self.nodes
                    .get(dependency)
                    .is_some_and(|dep| dep.status == TaskStatus::Completed)
            });
            if dependencies_ready {
                ready.push(task_id.clone());
            }
        }
        ready
    }
}

/// Depth of a task inside its own graph. A parent outside the graph is the host
/// coordinator, so it contributes depth 0; an in-graph parent contributes one.
fn depth_from_parents(
    nodes: &BTreeMap<TaskId, TaskNode>,
    task_id: &TaskId,
) -> Result<u32, OrchestratorError> {
    let mut depth = 0;
    let mut cursor = nodes
        .get(task_id)
        .and_then(|node| node.parent_task_id.as_ref());
    let mut guard = 0;
    while let Some(parent_id) = cursor {
        guard += 1;
        if guard > nodes.len() + 1 {
            return Err(OrchestratorError::new(
                ErrorCode::DagCycle,
                format!("parent chain of {task_id} does not terminate"),
            ));
        }
        let Some(parent) = nodes.get(parent_id) else {
            // The host coordinator owns the root and is not a graph node.
            return Ok(depth);
        };
        depth += 1;
        cursor = parent.parent_task_id.as_ref();
    }
    Ok(depth)
}

/// Normalize one declared write scope for comparison: forward slashes, no
/// trailing separator, no leading `./`.
fn normalize_scope_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let normalized = normalized.trim_end_matches('/');
    normalized
        .strip_prefix("./")
        .unwrap_or(normalized)
        .to_owned()
}

/// Whether two write scopes can name the same file: equal paths, or one scope
/// nested under the other.
fn scopes_overlap(left: &str, right: &str) -> bool {
    left == right
        || left.starts_with(&format!("{right}/"))
        || right.starts_with(&format!("{left}/"))
}

fn topological_sort(
    nodes: &BTreeMap<TaskId, TaskNode>,
    order: &[TaskId],
) -> Result<Vec<TaskId>, OrchestratorError> {
    let mut state: BTreeMap<TaskId, u8> = BTreeMap::new();
    let mut sorted = Vec::new();
    for task_id in order {
        visit(nodes, task_id, &mut state, &mut sorted)?;
    }
    Ok(sorted)
}

fn visit(
    nodes: &BTreeMap<TaskId, TaskNode>,
    task_id: &TaskId,
    state: &mut BTreeMap<TaskId, u8>,
    sorted: &mut Vec<TaskId>,
) -> Result<(), OrchestratorError> {
    match state.get(task_id).copied().unwrap_or(0) {
        1 => {
            return Err(OrchestratorError::new(
                ErrorCode::DagCycle,
                format!("task dependency cycle detected at {task_id}"),
            ));
        }
        2 => return Ok(()),
        _ => {}
    }
    state.insert(task_id.clone(), 1);
    let node = nodes.get(task_id).ok_or_else(|| {
        OrchestratorError::new(
            ErrorCode::UnknownTaskDependency,
            format!("unknown task {task_id}"),
        )
    })?;
    for dependency in &node.depends_on {
        visit(nodes, dependency, state, sorted)?;
    }
    state.insert(task_id.clone(), 2);
    sorted.push(task_id.clone());
    Ok(())
}

/// Worker report outcome. `OutcomeUnknown` exists so an uncertain side effect
/// can never be recorded as accepted completion.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegatedOutcome {
    Completed,
    Failed,
    Blocked,
    Canceled,
    OutcomeUnknown,
}

impl DelegatedOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
            Self::Canceled => "canceled",
            Self::OutcomeUnknown => "outcome_unknown",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "blocked" => Some(Self::Blocked),
            "canceled" => Some(Self::Canceled),
            "outcome_unknown" => Some(Self::OutcomeUnknown),
            _ => None,
        }
    }

    #[must_use]
    pub const fn maps_to(self) -> TaskStatus {
        match self {
            Self::Completed => TaskStatus::Completed,
            Self::Failed => TaskStatus::Failed,
            Self::Canceled => TaskStatus::Canceled,
            Self::Blocked | Self::OutcomeUnknown => TaskStatus::Blocked,
        }
    }
}

/// A checked revision: what the worker actually ran, not what it claims.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CheckedRevision {
    pub command: String,
    pub revision: String,
    pub passed: bool,
    /// Fingerprint of the workspace the check actually observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_digest: Option<ContentHash>,
    /// Exit status recorded by the runner receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub artifact_id: Option<ArtifactId>,
}

/// The durable worker report.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DelegatedResult {
    pub schema_version: u16,
    pub result_id: String,
    pub task_id: TaskId,
    pub worker: WorkerRef,
    pub outcome: DelegatedOutcome,
    pub summary: String,
    pub artifact_refs: Vec<String>,
    pub base_revision: String,
    pub result_revision: String,
    pub checked_revisions: Vec<CheckedRevision>,
    pub check_receipts: Vec<ToolExecutionReceipt>,
    pub usage: BudgetUsage,
    pub detail: Value,
}

impl DelegatedResult {
    pub fn validate(&self, brief: &TaskBrief) -> Result<(), OrchestratorError> {
        if self.schema_version != DELEGATION_CONTRACT_VERSION {
            return Err(OrchestratorError::new(
                ErrorCode::UnsupportedSchemaVersion,
                format!(
                    "delegated result schema {} is not supported",
                    self.schema_version
                ),
            ));
        }
        if self.task_id != brief.task_id {
            return Err(OrchestratorError::new(
                ErrorCode::AmbiguousTaskOwner,
                "delegated result does not belong to this task",
            ));
        }
        if self.result_id.trim().is_empty() {
            return Err(OrchestratorError::new(
                ErrorCode::InvalidPayload,
                "a delegated result requires a stable result id",
            ));
        }
        if self.base_revision.trim().is_empty() || self.result_revision.trim().is_empty() {
            return Err(OrchestratorError::new(
                ErrorCode::ResultIncomplete,
                "a delegated result requires base and result revisions",
            ));
        }
        if self.summary.trim().is_empty() {
            return Err(OrchestratorError::new(
                ErrorCode::ResultIncomplete,
                "a delegated result requires a summary",
            ));
        }
        for expected in &brief.expected_artifacts {
            if !self.artifact_refs.iter().any(|actual| actual == expected) {
                return Err(OrchestratorError::new(
                    ErrorCode::ResultIncomplete,
                    format!("result is missing expected artifact {expected}"),
                ));
            }
        }
        for revision in &self.checked_revisions {
            if revision.revision.trim().is_empty() || revision.revision != self.result_revision {
                return Err(OrchestratorError::new(
                    ErrorCode::ResultIncomplete,
                    "every check must run against the delegated result revision",
                ));
            }
        }
        if self.checked_revisions.len() != self.check_receipts.len() {
            return Err(OrchestratorError::new(
                ErrorCode::ResultIncomplete,
                "every checked revision requires a matching runner receipt",
            ));
        }
        for (check, receipt) in self.checked_revisions.iter().zip(&self.check_receipts) {
            let expected_input_hash = ContentHash::from_canonical_json(&serde_json::json!({
                "command": &check.command,
                "base_commit": &self.base_revision,
                "revision": &check.revision,
            }))
            .map_err(|error| OrchestratorError::new(error.code(), error.to_string()))?;
            if check.command.trim().is_empty()
                || check.workspace_digest.is_none()
                || check.passed != (check.exit_code == Some(0))
                || receipt.task_id != self.task_id
                || !matches!(
                    receipt.intent_state,
                    harness_types::ToolIntentState::Validated
                        | harness_types::ToolIntentState::IntentRecorded
                )
                || receipt.outcome_state != harness_types::ToolOutcomeState::Settled
                || receipt.exit_code != check.exit_code
                || receipt.after_fingerprint != check.workspace_digest
                || receipt.input_hash != expected_input_hash
            {
                return Err(OrchestratorError::new(
                    ErrorCode::ResultIncomplete,
                    "a checked revision must match a settled runner receipt, command input hash, workspace digest, and exit code",
                ));
            }
        }
        // A report of completion must carry real evidence. A human-readable
        // "done" without artifacts or receipts is not accepted work.
        if matches!(self.outcome, DelegatedOutcome::Completed) {
            let has_evidence = !self.check_receipts.is_empty() || !self.artifact_refs.is_empty();
            let declared = !brief.expected_artifacts.is_empty();
            if declared && !has_evidence {
                return Err(OrchestratorError::new(
                    ErrorCode::ResultIncomplete,
                    "a completion report without artifacts or receipts is not accepted work",
                ));
            }
            if !declared && self.check_receipts.is_empty() {
                return Err(OrchestratorError::new(
                    ErrorCode::ResultIncomplete,
                    "a completion report requires at least one runner receipt",
                ));
            }
        }
        Ok(())
    }

    /// Host-accepted completion is a separate decision from the worker report.
    ///
    /// The decision is made by the shared acceptance reducer, so "the worker
    /// said completed" and "the task is accepted" cannot drift apart: a
    /// completed outcome with no typed evidence is not accepted work.
    #[must_use]
    pub fn accepted_completion(&self) -> bool {
        AcceptanceRecord::initial(self.task_id.clone())
            .apply(AcceptanceCommand::Evaluate {
                criteria: self.acceptance_criteria(),
                pending_effects: 0,
                evidence_fingerprint: self.acceptance_fingerprint(),
            })
            .is_ok_and(|transition| transition.next.is_accepted())
    }

    fn acceptance_fingerprint(&self) -> Option<ContentHash> {
        let mut checks = self.checked_revisions.iter();
        let first = checks.next()?.workspace_digest.clone()?;
        checks
            .all(|check| check.workspace_digest.as_ref() == Some(&first))
            .then_some(first)
    }

    /// The criteria the host checks before accepting this report.
    #[must_use]
    pub fn acceptance_criteria(&self) -> Vec<CriterionState> {
        vec![CriterionState {
            criterion_id: "worker.outcome".to_owned(),
            required: true,
            status: if matches!(self.outcome, DelegatedOutcome::Completed) {
                CriterionStatus::Satisfied
            } else {
                CriterionStatus::Failed
            },
            evidence: self.acceptance_evidence(),
        }]
    }

    /// The typed evidence this report carries: artifacts it produced and checks
    /// it executed with their receipts.
    #[must_use]
    pub fn acceptance_evidence(&self) -> Vec<CriterionEvidence> {
        let mut evidence: Vec<CriterionEvidence> = self
            .artifact_refs
            .iter()
            .map(|reference| CriterionEvidence::ArtifactProduced {
                artifact_id: None,
                reference: reference.clone(),
            })
            .collect();
        evidence.extend(self.checked_revisions.iter().zip(&self.check_receipts).map(
            |(revision, receipt)| CriterionEvidence::CheckExecuted {
                command: revision.command.clone(),
                workspace_digest: revision.workspace_digest.clone(),
                exit_code: revision.exit_code,
                outcome: if revision.passed && revision.exit_code == Some(0) {
                    CheckOutcome::Passed
                } else {
                    CheckOutcome::Failed
                },
                receipt_ref: Some(receipt.tool_execution_id.to_string()),
            },
        ));
        evidence
    }
}

/// Delivery state of a durable parent message.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Pending,
    Consumed,
}

impl DeliveryState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Consumed => "consumed",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "consumed" => Some(Self::Consumed),
            _ => None,
        }
    }
}

/// A durable parent message. Exactly-once logical delivery is enforced by
/// `message_id` deduplication and a `Consumed` flag checked inside the write
/// transaction; a notification only wakes a reader.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ParentDelivery {
    pub message_id: String,
    pub sender_task_id: TaskId,
    pub recipient_task_id: TaskId,
    pub recipient_session_id: SessionId,
    pub payload_hash: ContentHash,
    pub payload: Value,
    pub state: DeliveryState,
    pub result_id: Option<String>,
    pub consumed_by: Option<String>,
}

impl ParentDelivery {
    pub fn validate(&self) -> Result<(), OrchestratorError> {
        if self.message_id.trim().is_empty() {
            return Err(OrchestratorError::new(
                ErrorCode::InvalidPayload,
                "a parent delivery requires a stable message id",
            ));
        }
        if self.sender_task_id == self.recipient_task_id {
            return Err(OrchestratorError::new(
                ErrorCode::AmbiguousTaskOwner,
                "a task cannot deliver a message to itself",
            ));
        }
        Ok(())
    }
}

/// Scheduler bounds. These are host policy, not model input.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SchedulerConfig {
    pub max_concurrent_workers: u32,
    pub max_depth: u32,
    /// How many workers may be dispatched but not yet holding a compute slot.
    ///
    /// The queue is what makes dispatch backpressure instead of an unbounded
    /// spawn: a caller that asks to fan out more work than the host can run has
    /// to be told it was refused, not discover it when the host runs out of
    /// memory. `0` is legal and means "no waiting at all" - a dispatch is then
    /// refused unless a slot is free at that instant.
    #[serde(default = "default_max_queued_workers")]
    pub max_queued_workers: u32,
    pub budget: DelegationBudget,
}

/// The absence of a queue bound in a stored config means the default, not zero.
///
/// Serde's `default` on a field is what keeps a `SchedulerConfig` written before
/// M8 readable: without it, every stored config would fail to deserialize, and
/// with a plain `Default` it would silently become unbounded.
fn default_max_queued_workers() -> u32 {
    DEFAULT_MAX_QUEUED_WORKERS
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            max_concurrent_workers: DEFAULT_MAX_WORKERS,
            max_depth: DEFAULT_MAX_DEPTH,
            max_queued_workers: DEFAULT_MAX_QUEUED_WORKERS,
            budget: DelegationBudget::default(),
        }
    }
}

impl SchedulerConfig {
    pub fn validate(&self) -> Result<(), OrchestratorError> {
        if self.max_concurrent_workers == 0 || self.max_concurrent_workers > DEFAULT_MAX_WORKERS {
            return Err(OrchestratorError::new(
                ErrorCode::BudgetExhausted,
                format!(
                    "concurrent worker slots {} are outside 1..={DEFAULT_MAX_WORKERS}",
                    self.max_concurrent_workers
                ),
            ));
        }
        if self.max_depth == 0 || self.max_depth > DEFAULT_MAX_DEPTH {
            return Err(OrchestratorError::new(
                ErrorCode::DelegationDepthExceeded,
                format!(
                    "delegation depth {} is outside 1..={DEFAULT_MAX_DEPTH}",
                    self.max_depth
                ),
            ));
        }
        if self.max_queued_workers > DEFAULT_MAX_QUEUED_WORKERS {
            return Err(OrchestratorError::new(
                ErrorCode::DelegationQueueFull,
                format!(
                    "a queue of {} workers exceeds the host cap {DEFAULT_MAX_QUEUED_WORKERS}",
                    self.max_queued_workers
                ),
            ));
        }
        self.budget.validate()
    }

    /// Refuse a dispatch that would put more than the queue bound in waiting.
    ///
    /// `reserved` counts every worker that has been dispatched and has not
    /// settled, so it is exactly "running plus queued". The bound is therefore
    /// expressed once, on the total the caller can see, instead of twice on two
    /// numbers that can drift apart.
    pub fn require_queue_capacity(&self, reserved: u32) -> Result<(), OrchestratorError> {
        let capacity = self
            .max_concurrent_workers
            .saturating_add(self.max_queued_workers);
        if reserved >= capacity {
            return Err(OrchestratorError::new(
                ErrorCode::DelegationQueueFull,
                format!(
                    "the delegation queue is full: {reserved} worker(s) are dispatched and at most \
                     {capacity} may be ({} running, {} waiting)",
                    self.max_concurrent_workers, self.max_queued_workers
                ),
            ));
        }
        Ok(())
    }

    pub fn require_slot(&self, in_use: u32) -> Result<(), OrchestratorError> {
        if in_use >= self.max_concurrent_workers {
            return Err(OrchestratorError::new(
                ErrorCode::BudgetExhausted,
                format!(
                    "all {} worker slots are in use",
                    self.max_concurrent_workers
                ),
            ));
        }
        Ok(())
    }

    pub fn require_depth(&self, depth: u32) -> Result<(), OrchestratorError> {
        if depth > self.max_depth {
            return Err(OrchestratorError::new(
                ErrorCode::DelegationDepthExceeded,
                format!(
                    "depth {depth} exceeds the configured cap {}",
                    self.max_depth
                ),
            ));
        }
        Ok(())
    }
}

/// Lifecycle of an isolated worker workspace.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeState {
    Creating,
    Ready,
    Integrating,
    Integrated,
    Removed,
}

impl WorktreeState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Creating => "creating",
            Self::Ready => "ready",
            Self::Integrating => "integrating",
            Self::Integrated => "integrated",
            Self::Removed => "removed",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "creating" => Some(Self::Creating),
            "ready" => Some(Self::Ready),
            "integrating" => Some(Self::Integrating),
            "integrated" => Some(Self::Integrated),
            "removed" => Some(Self::Removed),
            _ => None,
        }
    }

    /// A worker owns exactly one branch. Two workers never share a branch.
    pub fn transition(self, next: Self) -> Result<Self, OrchestratorError> {
        let allowed = matches!(
            (self, next),
            (Self::Creating, Self::Ready | Self::Removed)
                | (Self::Ready, Self::Integrating | Self::Removed)
                | (
                    Self::Integrating,
                    Self::Integrated | Self::Ready | Self::Removed
                )
                | (Self::Integrated, Self::Removed)
        );
        if allowed || self == next {
            Ok(next)
        } else {
            Err(OrchestratorError::new(
                ErrorCode::InvalidStateTransition,
                format!(
                    "invalid worktree transition {} -> {}",
                    self.as_str(),
                    next.as_str()
                ),
            ))
        }
    }
}

/// A host-owned isolated worker workspace.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorktreeRecord {
    pub worktree_id: String,
    pub task_id: TaskId,
    pub run_id: AgentRunId,
    pub project_id: ProjectId,
    pub base_commit: String,
    pub base_branch: String,
    pub branch: String,
    pub path: String,
    pub write_scope: Vec<String>,
    pub state: WorktreeState,
    pub input_fingerprint: ContentHash,
    pub result_fingerprint: Option<ContentHash>,
    pub generation: u64,
}

/// The verified input snapshot shared by every editing worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedSnapshot {
    pub project_id: ProjectId,
    pub root: String,
    pub base_commit: String,
    pub base_branch: String,
    pub fingerprint: ContentHash,
}

/// Reason a dirty working tree was refused. Recorded so the refusal is
/// explainable rather than silent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DirtyReason {
    TrackedModification { path: String },
    StagedChange { path: String },
    UntrackedFile { path: String },
    DetachedHead,
    NotARepository,
}

impl DirtyReason {
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::TrackedModification { path } => {
                format!("tracked modification at {path}")
            }
            Self::StagedChange { path } => format!("staged change at {path}"),
            Self::UntrackedFile { path } => format!("untracked file {path}"),
            Self::DetachedHead => "repository is in detached HEAD state".to_owned(),
            Self::NotARepository => "workspace root is not a Git repository".to_owned(),
        }
    }
}

/// Host-created memory binding handed to one worker at spawn. Later changes
/// enter only at a logged boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskMemoryBinding {
    pub profile_id: AgentProfileId,
    pub task_id: TaskId,
    pub asset_id: harness_types::MemoryAssetId,
    pub version: u64,
    pub injection_mode: String,
    pub priority: i64,
    pub actions: Vec<GrantAction>,
}

/// One integration step result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntegrationStep {
    pub task_id: TaskId,
    pub branch: String,
    pub result_revision: String,
    pub applied_commit: String,
}

/// Final integration report. Only the integrated revision is current evidence.
#[derive(Clone, Debug, PartialEq)]
pub struct IntegrationReport {
    pub project_id: ProjectId,
    pub integration_root: String,
    pub base_commit: String,
    pub final_commit: String,
    pub final_fingerprint: ContentHash,
    pub steps: Vec<IntegrationStep>,
    pub conflicts: Vec<String>,
    pub checks: Vec<CheckedRevision>,
}

impl IntegrationReport {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.conflicts.is_empty() && self.checks.iter().all(|check| check.passed)
    }
}

/// Token-free notification handle. Notifications carry no payload authority;
/// a woken reader must re-read the durable inbox.
#[derive(Clone, Debug, Default)]
pub struct DeliveryNotifier {
    waiters: std::sync::Arc<tokio::sync::Notify>,
    wake_count: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl DeliveryNotifier {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn notify(&self) {
        self.wake_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.waiters.notify_waiters();
    }

    pub async fn wait(&self) {
        self.waiters.notified().await;
    }

    #[must_use]
    pub fn wake_count(&self) -> u64 {
        self.wake_count.load(std::sync::atomic::Ordering::SeqCst)
    }
}
