//! The delegation coordinator: durable admission, budgeted dispatch, result
//! acceptance and dependency-ordered progress over one proven DAG.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use harness_providers::CancellationToken;
use harness_store_sqlite::{
    SqliteStore, StoredDelegatedResultRecord, StoredTaskNodeRecord, TaskAdmission, TaskOwnerRecord,
};
use harness_types::{AgentRunId, ContentHash, ErrorCode, SessionId, TaskId};
use serde_json::json;

use crate::contracts::{
    BudgetUsage, DelegatedResult, OrchestratorError, TaskPlan, TaskStatus, WorkerRef,
};
use crate::scheduler::{
    BudgetLedger, WorkerHandle, WorkerLease, WorkerOutcome, WorkerRequest, WorkerScheduler,
};

/// What happened to one task while the coordinator was driving the DAG.
#[derive(Clone, Debug)]
pub struct StepOutcome {
    pub task_id: TaskId,
    pub status: TaskStatus,
    pub accepted: bool,
    pub detail: String,
}

/// Token that marks one physical session slot per task. A child task owns its
/// own session, so one agent never appends to another agent's session log.
#[derive(Clone, Debug)]
pub struct TaskSession {
    pub task_id: TaskId,
    pub session_id: SessionId,
}

/// Host-side verification the coordinator needs before a result can be
/// accepted. Keeping it a trait means the acceptance rule is testable without
/// touching a model.
pub trait ResultVerifier: Send + Sync {
    /// Verdict for one candidate result. `Err` rejects acceptance.
    fn verify(&self, task_id: &TaskId, result: &DelegatedResult) -> Result<(), OrchestratorError>;
}

/// The P5 default: a report is accepted only when it carries the artifacts the
/// brief declared and a receipt for every revision it claims to have checked.
#[derive(Clone, Copy, Debug, Default)]
pub struct EvidenceVerifier;

impl ResultVerifier for EvidenceVerifier {
    fn verify(&self, _task_id: &TaskId, result: &DelegatedResult) -> Result<(), OrchestratorError> {
        if !result.accepted_completion() {
            return Err(OrchestratorError::new(
                ErrorCode::ResultIncomplete,
                "the worker did not report a completed outcome",
            ));
        }
        if result.checked_revisions.is_empty() && result.artifact_refs.is_empty() {
            return Err(OrchestratorError::new(
                ErrorCode::ResultIncomplete,
                "a completion report without artifacts or receipts is not accepted work",
            ));
        }
        if result.base_revision == result.result_revision
            && !result.checked_revisions.is_empty()
            && result.checked_revisions.iter().any(|check| check.passed)
        {
            return Ok(());
        }
        if result.base_revision.trim().is_empty() || result.result_revision.trim().is_empty() {
            return Err(OrchestratorError::new(
                ErrorCode::ResultIncomplete,
                "a result must record the revision it was produced against",
            ));
        }
        Ok(())
    }
}

/// The delegation coordinator.
pub struct DelegationCoordinator {
    store: Arc<SqliteStore>,
    scheduler: Arc<WorkerScheduler>,
    verifier: Arc<dyn ResultVerifier>,
    generation: u64,
}

impl std::fmt::Debug for DelegationCoordinator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DelegationCoordinator")
            .field("generation", &self.generation)
            .field("live_workers", &self.scheduler.live_worker_count())
            .finish_non_exhaustive()
    }
}

impl DelegationCoordinator {
    #[must_use]
    pub fn new(store: Arc<SqliteStore>, scheduler: Arc<WorkerScheduler>, generation: u64) -> Self {
        Self {
            store,
            scheduler,
            verifier: Arc::new(EvidenceVerifier),
            generation,
        }
    }

    #[must_use]
    pub fn with_verifier(mut self, verifier: Arc<dyn ResultVerifier>) -> Self {
        self.verifier = verifier;
        self
    }

    #[must_use]
    pub fn ledger(&self) -> &Arc<BudgetLedger> {
        self.scheduler.ledger()
    }

    /// Admit a proven plan. Every node and its single owner generation become
    /// durable in one transaction, so a partial DAG can never be observed.
    pub async fn admit(&self, plan: &TaskPlan) -> Result<(), OrchestratorError> {
        let mut admissions = Vec::new();
        for task_id in &plan.order {
            let node = plan.node(task_id).ok_or_else(|| {
                OrchestratorError::new(ErrorCode::TaskNotFound, "plan lost a node")
            })?;
            let already = self.store.task_node(task_id).await?;
            if already.is_some() {
                // Re-admitting an identical node is idempotent; a different
                // node with the same ID is a duplicate, not a merge.
                continue;
            }
            let node_json = serde_json::to_value(node).map_err(|_| {
                OrchestratorError::new(ErrorCode::InvalidPayload, "task node is not serializable")
            })?;
            let brief_json = serde_json::to_value(&node.brief).map_err(|_| {
                OrchestratorError::new(ErrorCode::InvalidPayload, "task brief is not serializable")
            })?;
            admissions.push(TaskAdmission {
                task: StoredTaskNodeRecord {
                    task_id: node.task_id.clone(),
                    parent_task_id: node.parent_task_id.clone(),
                    role: node.role.as_str().to_owned(),
                    status: TaskStatus::Pending.as_str().to_owned(),
                    revision: 1,
                    depth: node.depth,
                    depends_on: node.depends_on.clone(),
                    brief_json,
                    node_json,
                },
                owner: None,
            });
        }
        if admissions.is_empty() {
            return Ok(());
        }
        self.store.admit_task_graph(&admissions).await?;
        Ok(())
    }

    /// Claim one task for one run at the host generation.
    pub async fn claim(
        &self,
        task_id: &TaskId,
        worker: &WorkerRef,
        session_id: &SessionId,
    ) -> Result<WorkerHandle, OrchestratorError> {
        let stored = self.store.task_node(task_id).await?.ok_or_else(|| {
            OrchestratorError::new(ErrorCode::TaskNotFound, "task is not admitted")
        })?;
        if stored.status == TaskStatus::Completed.as_str() {
            return Err(OrchestratorError::new(
                ErrorCode::TaskOwnershipConflict,
                "a completed task is never reassigned",
            ));
        }
        let revision = stored.revision;
        self.store
            .claim_task(
                &TaskOwnerRecord {
                    task_id: task_id.clone(),
                    owner_run_id: worker.run_id.clone(),
                    owner_session_id: session_id.clone(),
                    role: worker.role.as_str().to_owned(),
                    generation: worker.generation,
                    lease_revision: revision,
                },
                revision,
            )
            .await?;
        Ok(WorkerHandle {
            key: task_id.as_str().to_owned(),
            run_id: worker.run_id.clone(),
        })
    }

    /// Dispatch one claimed task to a worker backend.
    pub fn dispatch(
        &self,
        plan: &TaskPlan,
        task_id: &TaskId,
        worker: &WorkerRef,
        cancellation: CancellationToken,
    ) -> Result<WorkerHandle, OrchestratorError> {
        let node = plan.node(task_id).ok_or_else(|| {
            OrchestratorError::new(ErrorCode::TaskNotFound, "task is not part of this plan")
        })?;
        self.scheduler.dispatch(WorkerRequest {
            task_id: task_id.clone(),
            run_id: worker.run_id.clone(),
            generation: worker.generation,
            depth: node.depth,
            brief: node.brief.clone(),
            worktree: None,
            cancellation,
        })
    }

    /// Wait for the next worker to settle. The caller's own compute slot is
    /// released first so a waiting coordinator cannot starve its children.
    pub async fn await_next(
        &self,
        lease: &mut WorkerLease,
    ) -> Result<(TaskId, WorkerOutcome), OrchestratorError> {
        match self.scheduler.await_next_releasing(lease).await {
            Some(result) => result,
            None => Err(OrchestratorError::new(
                ErrorCode::SchedulerShutdown,
                "no worker is running",
            )),
        }
    }

    /// Validate a settled outcome against the plan and accept it only when the
    /// evidence is real. Completion is decided here, never by a worker's prose.
    pub async fn settle(
        &self,
        plan: &TaskPlan,
        task_id: &TaskId,
        outcome: WorkerOutcome,
    ) -> Result<StepOutcome, OrchestratorError> {
        let node = plan.node(task_id).ok_or_else(|| {
            OrchestratorError::new(ErrorCode::TaskNotFound, "task is not part of this plan")
        })?;
        match outcome {
            WorkerOutcome::Reported(result) | WorkerOutcome::Observed(result) => {
                let mut result = *result;
                if result.base_revision.trim().is_empty() {
                    result.base_revision = node.brief.base_commit.clone();
                }
                let verdict = result
                    .validate(&node.brief)
                    .and_then(|()| self.verifier.verify(task_id, &result));
                match verdict {
                    Ok(()) => {
                        let status = result.outcome.maps_to();
                        self.persist(plan, task_id, &result, status, result.usage)
                            .await?;
                        Ok(StepOutcome {
                            task_id: task_id.clone(),
                            status,
                            accepted: status == TaskStatus::Completed,
                            detail: "host accepted the reported evidence".to_owned(),
                        })
                    }
                    Err(error) => {
                        self.persist_rejection(plan, task_id, &result, &error.to_string())
                            .await?;
                        Ok(StepOutcome {
                            task_id: task_id.clone(),
                            status: TaskStatus::Blocked,
                            accepted: false,
                            detail: error.to_string(),
                        })
                    }
                }
            }
            WorkerOutcome::NoReport { reason } => {
                self.persist_blocked(plan, task_id, &reason).await?;
                Ok(StepOutcome {
                    task_id: task_id.clone(),
                    status: TaskStatus::Blocked,
                    accepted: false,
                    detail: reason,
                })
            }
            WorkerOutcome::Failed { error } => {
                self.persist_blocked(plan, task_id, &error.to_string())
                    .await?;
                Ok(StepOutcome {
                    task_id: task_id.clone(),
                    status: TaskStatus::Failed,
                    accepted: false,
                    detail: error.to_string(),
                })
            }
            WorkerOutcome::OutcomeUnknown { reason } => {
                // An uncertain side effect is never accepted completion.
                self.persist_blocked(plan, task_id, &reason).await?;
                Ok(StepOutcome {
                    task_id: task_id.clone(),
                    status: TaskStatus::Blocked,
                    accepted: false,
                    detail: format!("outcome_unknown: {reason}"),
                })
            }
        }
    }

    /// Tasks that became blocked or failed. Dependents must not run when a
    /// dependency did not complete.
    #[must_use]
    pub fn failed_tasks(plan: &TaskPlan, outcomes: &[StepOutcome]) -> Vec<TaskId> {
        let mut failed = BTreeSet::new();
        for outcome in outcomes {
            if matches!(outcome.status, TaskStatus::Failed | TaskStatus::Canceled)
                || (outcome.status == TaskStatus::Blocked && !outcome.accepted)
            {
                failed.insert(outcome.task_id.clone());
            }
        }
        let mut propagation = failed.clone();
        for task_id in &plan.topological_order {
            let Some(node) = plan.node(task_id) else {
                continue;
            };
            if node
                .depends_on
                .iter()
                .any(|dependency| propagation.contains(dependency))
            {
                propagation.insert(task_id.clone());
                failed.insert(task_id.clone());
            }
        }
        failed.into_iter().collect()
    }

    /// Progress view built only from durable state, so a restarted host has the
    /// same answer as the live one.
    pub async fn durable_progress(
        &self,
    ) -> Result<BTreeMap<String, TaskStatus>, OrchestratorError> {
        let mut progress = BTreeMap::new();
        for node in self.store.list_task_nodes().await? {
            let status = TaskStatus::parse(&node.status).ok_or_else(|| {
                OrchestratorError::new(
                    ErrorCode::StorageWriteFailed,
                    "a stored task has an unknown status",
                )
            })?;
            progress.insert(node.task_id.as_str().to_owned(), status);
        }
        Ok(progress)
    }

    /// Tasks that are finished and must never be dispatched again.
    pub async fn completed_tasks(&self) -> Result<Vec<TaskId>, OrchestratorError> {
        let mut completed = Vec::new();
        for node in self.store.list_task_nodes().await? {
            if node.status == TaskStatus::Completed.as_str() {
                completed.push(node.task_id);
            }
        }
        Ok(completed)
    }

    /// Resume view: durable result for a task, if any.
    pub async fn durable_result(
        &self,
        task_id: &TaskId,
    ) -> Result<Option<StoredDelegatedResultRecord>, OrchestratorError> {
        Ok(self.store.task_result(task_id).await?)
    }

    async fn persist(
        &self,
        plan: &TaskPlan,
        task_id: &TaskId,
        result: &DelegatedResult,
        status: TaskStatus,
        usage: BudgetUsage,
    ) -> Result<(), OrchestratorError> {
        self.ledger().observe_usage(task_id, usage);
        let stored = self.store.task_node(task_id).await?.ok_or_else(|| {
            OrchestratorError::new(ErrorCode::TaskNotFound, "task is not admitted")
        })?;
        let revision = stored.revision.saturating_add(1);
        let node = plan.node(task_id).ok_or_else(|| {
            OrchestratorError::new(ErrorCode::TaskNotFound, "task is not part of this plan")
        })?;
        let message_id = format!(
            "delivery-{}-{}",
            task_id.as_str().rsplit('-').next().unwrap_or("task"),
            result.result_id
        );
        let payload = json!({
            "task_id": task_id,
            "result_id": result.result_id,
            "outcome": result.outcome.as_str(),
            "summary": result.summary,
            "artifact_refs": result.artifact_refs,
            "base_revision": result.base_revision,
            "result_revision": result.result_revision,
            "checked_revisions": result.checked_revisions,
            "role": node.role.as_str(),
        });
        let payload_hash = ContentHash::from_canonical_json(&payload)
            .map_err(|error| OrchestratorError::new(error.code(), error.to_string()))?;
        let report_json = serde_json::to_value(result).map_err(|_| {
            OrchestratorError::new(ErrorCode::InvalidPayload, "result is not serializable")
        })?;
        let result_hash = ContentHash::from_canonical_json(&report_json)
            .map_err(|error| OrchestratorError::new(error.code(), error.to_string()))?;
        let mut node_json = stored.node_json.clone();
        if let Some(object) = node_json.as_object_mut() {
            object.insert("status".to_owned(), json!(status.as_str()));
        }
        let recipient = Self::recipient_task_id(plan, task_id);
        let parent = recipient.unwrap_or_else(|| task_id.clone());
        let recipient_session_id = self.recipient_session(&parent).await?;
        self.store
            .commit_delivery(harness_store_sqlite::DeliveryCommit {
                task_transition: StoredTaskNodeRecord {
                    task_id: task_id.clone(),
                    parent_task_id: stored.parent_task_id.clone(),
                    role: stored.role.clone(),
                    status: status.as_str().to_owned(),
                    revision,
                    depth: stored.depth,
                    depends_on: stored.depends_on.clone(),
                    brief_json: stored.brief_json.clone(),
                    node_json,
                },
                result: Some(StoredDelegatedResultRecord {
                    result_id: result.result_id.clone(),
                    task_id: task_id.clone(),
                    worker_run_id: result.worker.run_id.clone(),
                    outcome: result.outcome.as_str().to_owned(),
                    base_revision: result.base_revision.clone(),
                    result_revision: result.result_revision.clone(),
                    artifact_refs: result.artifact_refs.clone(),
                    report_json,
                    result_hash,
                }),
                delivery: harness_store_sqlite::ParentDeliveryRecord {
                    message_id,
                    sender_task_id: task_id.clone(),
                    recipient_task_id: parent,
                    recipient_session_id,
                    result_id: Some(result.result_id.clone()),
                    payload_hash,
                    payload,
                    state: "pending".to_owned(),
                    consumed_by: None,
                },
                usage: Some(harness_store_sqlite::BudgetUsageRecord {
                    model_requests: usage.model_requests,
                    retries: usage.retries,
                    cost_units: 0,
                }),
            })
            .await?;
        Ok(())
    }

    async fn persist_blocked(
        &self,
        plan: &TaskPlan,
        task_id: &TaskId,
        reason: &str,
    ) -> Result<(), OrchestratorError> {
        let stored = self.store.task_node(task_id).await?.ok_or_else(|| {
            OrchestratorError::new(ErrorCode::TaskNotFound, "task is not admitted")
        })?;
        if stored.status == TaskStatus::Completed.as_str() {
            return Ok(());
        }
        let revision = stored.revision.saturating_add(1);
        let mut node_json = stored.node_json.clone();
        if let Some(object) = node_json.as_object_mut() {
            object.insert("status".to_owned(), json!(TaskStatus::Blocked.as_str()));
            object.insert("blocked_reason".to_owned(), json!(reason));
        }
        let payload = json!({"task_id": task_id, "outcome": "blocked", "reason": reason});
        let payload_hash = ContentHash::from_canonical_json(&payload)
            .map_err(|error| OrchestratorError::new(error.code(), error.to_string()))?;
        let parent = Self::recipient_task_id(plan, task_id).unwrap_or_else(|| task_id.clone());
        let recipient_session_id = self.recipient_session(&parent).await?;
        let message_id = format!(
            "delivery-{}-blocked-{}",
            task_id.as_str().rsplit('-').next().unwrap_or("task"),
            revision
        );
        self.store
            .commit_delivery(harness_store_sqlite::DeliveryCommit {
                task_transition: StoredTaskNodeRecord {
                    task_id: task_id.clone(),
                    parent_task_id: stored.parent_task_id.clone(),
                    role: stored.role.clone(),
                    status: TaskStatus::Blocked.as_str().to_owned(),
                    revision,
                    depth: stored.depth,
                    depends_on: stored.depends_on.clone(),
                    brief_json: stored.brief_json.clone(),
                    node_json,
                },
                result: None,
                delivery: harness_store_sqlite::ParentDeliveryRecord {
                    message_id,
                    sender_task_id: task_id.clone(),
                    recipient_task_id: parent,
                    recipient_session_id,
                    result_id: None,
                    payload_hash,
                    payload,
                    state: "pending".to_owned(),
                    consumed_by: None,
                },
                usage: None,
            })
            .await?;
        Ok(())
    }

    async fn persist_rejection(
        &self,
        plan: &TaskPlan,
        task_id: &TaskId,
        result: &DelegatedResult,
        reason: &str,
    ) -> Result<(), OrchestratorError> {
        // The report is retained as a blocked task so the rejection is
        // inspectable, but it never becomes accepted completion.
        self.persist_blocked(
            plan,
            task_id,
            &format!("result {} rejected: {reason}", result.result_id.as_str()),
        )
        .await
    }

    fn recipient_task_id(plan: &TaskPlan, task_id: &TaskId) -> Option<TaskId> {
        plan.node(task_id)
            .and_then(|node| node.parent_task_id.clone())
    }

    /// A pending parent receives inbox data; the coordinator never appends to
    /// another agent's event log. The recipient session is the durable session
    /// that owns the parent task, and a missing one is reported, not invented.
    async fn recipient_session(&self, parent: &TaskId) -> Result<SessionId, OrchestratorError> {
        for summary in self.store.list_sessions().await? {
            if &summary.task_id == parent {
                return Ok(summary.session_id);
            }
        }
        Err(OrchestratorError::new(
            ErrorCode::InvalidPayload,
            "a delegated delivery requires a durable session for the recipient task",
        ))
    }
}

/// Deterministic generation for a fresh coordinator activation.
#[must_use]
pub fn coordinator_generation(previous: Option<u64>) -> u64 {
    previous.map_or(1, |value| value.saturating_add(1))
}

/// A fresh worker run identity for one activation.
#[must_use]
pub fn fresh_run(role: crate::contracts::AgentRole) -> (AgentRunId, String) {
    (AgentRunId::generate(), role.as_str().to_owned())
}
