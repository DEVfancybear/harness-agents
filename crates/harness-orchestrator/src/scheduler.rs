//! Bounded worker scheduling, cancellation and descendant drain.
//!
//! The scheduler owns the compute permits. A parent that is waiting for its
//! children releases its permit, so a parent can never hold every slot while its
//! children starve. Worker tasks are collected in an owned resource set and are
//! drained before the host closes storage.

use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
};

use harness_kernel::{KernelError, ManagedResource, ShutdownReport};
use harness_providers::CancellationToken;
use harness_types::{AgentRunId, ErrorCode, TaskId};
use tokio::sync::{Semaphore, mpsc};

use crate::contracts::{
    BudgetUsage, DelegatedResult, OrchestratorError, SchedulerConfig, TaskBrief, WorktreeRecord,
};
use crate::workspace::WorkspaceManager;

/// One unit of work handed to a worker backend.
pub struct WorkerRequest {
    pub task_id: TaskId,
    pub run_id: AgentRunId,
    pub generation: u64,
    pub depth: u32,
    pub brief: TaskBrief,
    pub worktree: Option<WorktreeRecord>,
    pub cancellation: CancellationToken,
}

impl std::fmt::Debug for WorkerRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkerRequest")
            .field("task_id", &self.task_id)
            .field("run_id", &self.run_id)
            .field("generation", &self.generation)
            .field("depth", &self.depth)
            .finish_non_exhaustive()
    }
}

/// The outcome of one dispatched worker.
#[derive(Debug)]
pub enum WorkerOutcome {
    /// The worker produced a report. A report is not yet accepted work.
    Reported(Box<DelegatedResult>),
    /// The host observed the worker's revisions and produced the report itself.
    Observed(Box<DelegatedResult>),
    /// The worker finished without a report.
    NoReport {
        reason: String,
    },
    Failed {
        error: OrchestratorError,
    },
    /// The worker was canceled or drained; its side effect is not established.
    OutcomeUnknown {
        reason: String,
    },
}

/// Host-side worker execution boundary. Implementations are the only place a
/// model or process boundary is touched; tests may supply a deterministic one.
pub trait WorkerBackend: Send + Sync {
    fn dispatch(
        &self,
        request: WorkerRequest,
    ) -> Pin<Box<dyn Future<Output = WorkerOutcome> + Send + '_>>;
}

/// A backend that always refuses, used to prove the scheduler bounds before any
/// dispatch happens.
#[derive(Clone, Copy, Debug, Default)]
pub struct RefusingBackend;

impl WorkerBackend for RefusingBackend {
    fn dispatch(
        &self,
        _request: WorkerRequest,
    ) -> Pin<Box<dyn Future<Output = WorkerOutcome> + Send + '_>> {
        Box::pin(async {
            WorkerOutcome::Failed {
                error: OrchestratorError::new(
                    ErrorCode::ServiceUnavailable,
                    "no worker backend is configured",
                ),
            }
        })
    }
}

/// Shared, durable-plus-in-memory budget ledger. A dispatch is charged before
/// the worker runs, so a worker that never reports usage still consumes budget:
/// missing usage is not zero.
#[derive(Debug)]
pub struct BudgetLedger {
    max_model_requests: u32,
    max_retries: u32,
    max_workers: u32,
    requests: AtomicU32,
    retries: AtomicU32,
    cost: AtomicU64,
    observed: Mutex<BTreeMap<String, BudgetUsage>>,
}

impl BudgetLedger {
    #[must_use]
    pub fn new(config: &SchedulerConfig) -> Self {
        Self {
            max_model_requests: config.budget.max_model_requests,
            max_retries: config.budget.max_retries,
            max_workers: config.max_concurrent_workers,
            requests: AtomicU32::new(0),
            retries: AtomicU32::new(0),
            cost: AtomicU64::new(0),
            observed: Mutex::new(BTreeMap::new()),
        }
    }

    /// Charge one model request before dispatch. This is the authority that
    /// makes budget exhaustible even when a worker reports nothing.
    pub fn charge_request(&self) -> Result<(), OrchestratorError> {
        let previous = self.requests.fetch_add(1, Ordering::SeqCst);
        if previous >= self.max_model_requests {
            self.requests.fetch_sub(1, Ordering::SeqCst);
            return Err(OrchestratorError::new(
                ErrorCode::BudgetExhausted,
                format!(
                    "model request budget of {} is exhausted",
                    self.max_model_requests
                ),
            ));
        }
        Ok(())
    }

    pub fn charge_retry(&self) -> Result<(), OrchestratorError> {
        let previous = self.retries.fetch_add(1, Ordering::SeqCst);
        if previous >= self.max_retries {
            self.retries.fetch_sub(1, Ordering::SeqCst);
            return Err(OrchestratorError::new(
                ErrorCode::BudgetExhausted,
                "retry budget is exhausted",
            ));
        }
        Ok(())
    }

    /// Record the usage a worker reported. It never lowers what was charged.
    pub fn observe_usage(&self, task_id: &TaskId, usage: BudgetUsage) {
        if let Ok(mut observed) = self.observed.lock() {
            observed.insert(task_id.as_str().to_owned(), usage);
        }
        self.cost
            .fetch_add(u64::from(usage.retries), Ordering::SeqCst);
    }

    #[must_use]
    pub fn requests_used(&self) -> u32 {
        self.requests.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn retries_used(&self) -> u32 {
        self.retries.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn cost_used(&self) -> u64 {
        self.cost.load(Ordering::SeqCst)
    }

    #[must_use]
    pub const fn max_workers(&self) -> u32 {
        self.max_workers
    }

    #[must_use]
    pub fn remaining_requests(&self) -> u32 {
        self.max_model_requests
            .saturating_sub(self.requests.load(Ordering::SeqCst))
    }

    #[must_use]
    pub fn observed_usage(&self, task_id: &TaskId) -> Option<BudgetUsage> {
        self.observed
            .lock()
            .ok()
            .and_then(|observed| observed.get(task_id.as_str()).copied())
    }
}

/// A running worker task. The cancellation token is the only state the host
/// needs to drain it; identity lives in the durable task store.
struct WorkerTask {
    cancellation: CancellationToken,
}

/// The host scheduler. It owns the worker permits and every spawned worker task.
pub struct WorkerScheduler {
    config: SchedulerConfig,
    slots: Arc<Semaphore>,
    ledger: Arc<BudgetLedger>,
    backend: Arc<dyn WorkerBackend>,
    workspace: Option<Arc<WorkspaceManager>>,
    settled: mpsc::Sender<Result<(TaskId, WorkerOutcome), OrchestratorError>>,
    settled_rx:
        tokio::sync::Mutex<mpsc::Receiver<Result<(TaskId, WorkerOutcome), OrchestratorError>>>,
    live: Arc<Mutex<BTreeMap<String, WorkerTask>>>,
    /// Task ids reserved by a dispatch that has not settled yet.
    reserved: Arc<Mutex<std::collections::BTreeSet<String>>>,
    /// Dispatched-but-not-yet-reported workers. This is what makes an exhausted
    /// outcome stream distinguishable from a temporarily empty one.
    outstanding: Arc<AtomicU32>,
    shutting_down: Arc<AtomicBool>,
    drained: Arc<Mutex<Vec<String>>>,
}

impl std::fmt::Debug for WorkerScheduler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkerScheduler")
            .field("config", &self.config)
            .field("slots", &self.slots.available_permits())
            .field("requests_used", &self.ledger.requests_used())
            .finish_non_exhaustive()
    }
}

impl WorkerScheduler {
    pub fn new(
        config: SchedulerConfig,
        backend: Arc<dyn WorkerBackend>,
        workspace: Option<Arc<WorkspaceManager>>,
    ) -> Result<Self, OrchestratorError> {
        config.validate()?;
        let (settled, settled_rx) = mpsc::channel(64);
        Ok(Self {
            slots: Arc::new(Semaphore::new(config.max_concurrent_workers as usize)),
            ledger: Arc::new(BudgetLedger::new(&config)),
            config,
            backend,
            workspace,
            settled,
            settled_rx: tokio::sync::Mutex::new(settled_rx),
            live: Arc::new(Mutex::new(BTreeMap::new())),
            reserved: Arc::new(Mutex::new(std::collections::BTreeSet::new())),
            outstanding: Arc::new(AtomicU32::new(0)),
            shutting_down: Arc::new(AtomicBool::new(false)),
            drained: Arc::new(Mutex::new(Vec::new())),
        })
    }

    #[must_use]
    pub fn config(&self) -> SchedulerConfig {
        self.config
    }

    #[must_use]
    pub fn ledger(&self) -> &Arc<BudgetLedger> {
        &self.ledger
    }

    #[must_use]
    pub fn available_slots(&self) -> u32 {
        u32::try_from(self.slots.available_permits()).unwrap_or(0)
    }

    /// Workers dispatched and not yet settled: running plus queued.
    ///
    /// This is the number the queue bound is expressed on, and it is observable
    /// so a caller can report backpressure instead of guessing at it.
    #[must_use]
    pub fn reserved_workers(&self) -> u32 {
        self.reserved.lock().map_or(0, |reserved| {
            u32::try_from(reserved.len()).unwrap_or(u32::MAX)
        })
    }

    /// Dispatched workers without a compute slot yet.
    #[must_use]
    pub fn queued_workers(&self) -> u32 {
        self.reserved_workers()
            .saturating_sub(self.live_worker_count().try_into().unwrap_or(u32::MAX))
    }

    /// Admit a delegation at a given depth. Depth is checked before any worker
    /// is created, and an exhausted budget is terminal for new dispatch.
    pub fn require_admission(&self, depth: u32) -> Result<(), OrchestratorError> {
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(OrchestratorError::new(
                ErrorCode::SchedulerShutdown,
                "the scheduler is shutting down and accepts no new work",
            ));
        }
        self.config.require_depth(depth)?;
        if self.ledger.requests_used() >= self.config.budget.max_model_requests {
            return Err(OrchestratorError::new(
                ErrorCode::BudgetExhausted,
                "model request budget is exhausted",
            ));
        }
        Ok(())
    }

    /// Dispatch one worker. The returned future resolves when the worker
    /// settles; the permit is released when the worker's turn ends, including
    /// when the caller stops waiting.
    ///
    /// The queue bound is checked before the task reservation is taken, so a
    /// refused dispatch leaves nothing behind: a caller that retries after a
    /// worker settles is not fighting a reservation its own refusal created.
    pub fn dispatch(
        self: &Arc<Self>,
        request: WorkerRequest,
    ) -> Result<WorkerHandle, OrchestratorError> {
        self.require_admission(request.depth)?;
        let slots = Arc::clone(&self.slots);
        let cancellation = request.cancellation.clone();
        let task_id = request.task_id.clone();
        let run_id = request.run_id.clone();
        let key = task_id.as_str().to_owned();
        // Reserve the task now so two dispatches cannot race for it, but only
        // count it as live once it actually holds a slot: a worker still queued
        // for a permit has not started.
        {
            let mut reserved = self.reserved.lock().map_err(|_| {
                OrchestratorError::new(ErrorCode::RuntimeBlocked, "scheduler state is poisoned")
            })?;
            self.config
                .require_queue_capacity(u32::try_from(reserved.len()).unwrap_or(u32::MAX))?;
            if !reserved.insert(key.clone()) {
                return Err(OrchestratorError::new(
                    ErrorCode::TaskOwnershipConflict,
                    "this task already has a live worker",
                ));
            }
        }
        let backend = Arc::clone(&self.backend);
        let live = Arc::clone(&self.live);
        let reserved = Arc::clone(&self.reserved);
        let outstanding = Arc::clone(&self.outstanding);
        let shutting_down = Arc::clone(&self.shutting_down);
        let key_for_task = key.clone();
        let sender = self.settled.clone();
        // Charge before the worker exists: a worker that never reports usage
        // still consumes budget, and the ledger is observable immediately. This
        // is the only charge for the dispatch; charging again once the slot is
        // acquired would consume two request slots for one worker.
        if let Err(error) = self.ledger.charge_request() {
            // The task reservation must not outlive the failed dispatch, or the
            // task could never be dispatched again.
            if let Ok(mut reserved) = self.reserved.lock() {
                reserved.remove(&key);
            }
            return Err(error);
        }
        // Marked before the spawned task can report, so the outcome stream can
        // never look exhausted while this worker is still in flight.
        outstanding.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            // Retire this worker before its outcome becomes observable, so a
            // caller waiting for outstanding work never waits on a worker that
            // has already reported.
            macro_rules! retire {
                () => {
                    if let Ok(mut live) = live.lock() {
                        live.remove(&key_for_task);
                    }
                    if let Ok(mut reserved) = reserved.lock() {
                        reserved.remove(&key_for_task);
                    }
                    outstanding.fetch_sub(1, Ordering::SeqCst);
                };
            }
            let permit = tokio::select! {
                permit = Arc::clone(&slots).acquire_owned() => permit,
                () = cancellation.cancelled() => {
                    retire!();
                    let _ = sender
                        .send(Ok((
                            request.task_id.clone(),
                            WorkerOutcome::OutcomeUnknown {
                                reason: "worker was canceled before it obtained a slot".to_owned(),
                            },
                        )))
                        .await;
                    return;
                }
            };
            let Ok(permit) = permit else {
                retire!();
                let _ = sender
                    .send(Err(OrchestratorError::new(
                        ErrorCode::SchedulerShutdown,
                        "worker slots were closed",
                    )))
                    .await;
                return;
            };
            let mut lease = WorkerLease {
                permit: Some(permit),
            };
            // The worker is now running and therefore occupies a live slot.
            if let Ok(mut live) = live.lock() {
                live.insert(
                    key_for_task.clone(),
                    WorkerTask {
                        cancellation: cancellation.clone(),
                    },
                );
            }
            if shutting_down.load(Ordering::SeqCst) && !cancellation.is_cancelled() {
                cancellation.cancel();
            }
            let observed = backend.dispatch(request).await;
            lease.release();
            retire!();
            let _ = sender.send(Ok((task_id, observed))).await;
        });
        Ok(WorkerHandle { key, run_id })
    }

    /// Acquire one compute permit for the caller itself. A coordinator that
    /// then waits for its children must call [`WorkerLease::release`] so the
    /// waiting parent cannot hold every slot its children need.
    pub async fn acquire_lease(self: &Arc<Self>) -> Result<WorkerLease, OrchestratorError> {
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(OrchestratorError::new(
                ErrorCode::SchedulerShutdown,
                "the scheduler is shutting down",
            ));
        }
        let permit = Arc::clone(&self.slots).acquire_owned().await.map_err(|_| {
            OrchestratorError::new(ErrorCode::SchedulerShutdown, "worker slots were closed")
        })?;
        self.ledger.charge_request()?;
        Ok(WorkerLease {
            permit: Some(permit),
        })
    }

    /// Await the next settled worker while explicitly holding no slot. This is
    /// the primitive that makes parent-wait fairness structural instead of
    /// dependent on caller discipline.
    pub async fn await_next_releasing(
        self: &Arc<Self>,
        lease: &mut WorkerLease,
    ) -> Option<Result<(TaskId, WorkerOutcome), OrchestratorError>> {
        lease.release();
        self.next_settled().await
    }

    /// Await the next settled worker. Ending this wait never cancels the worker:
    /// the permit and the drain entry are owned by the spawned task, so a parent
    /// that stops waiting cannot strand its children, and a fresh caller can
    /// still drain them.
    pub async fn next_settled(&self) -> Option<Result<(TaskId, WorkerOutcome), OrchestratorError>> {
        let mut receiver = self.settled_rx.lock().await;
        if self.outstanding.load(Ordering::SeqCst) == 0 {
            return receiver.try_recv().ok();
        }
        receiver.recv().await
    }

    /// Authorize an editing workspace for one worker from the verified snapshot.
    pub async fn create_worktree(
        &self,
        snapshot: &crate::contracts::VerifiedSnapshot,
        task_id: &TaskId,
        run_id: &AgentRunId,
        write_scope: &[String],
        generation: u64,
    ) -> Result<WorktreeRecord, OrchestratorError> {
        let workspace = self.workspace.as_ref().ok_or_else(|| {
            OrchestratorError::new(
                ErrorCode::StrictIsolationUnavailable,
                "this scheduler has no workspace manager",
            )
        })?;
        if self.ledger.max_workers() == 0 {
            return Err(OrchestratorError::new(
                ErrorCode::BudgetExhausted,
                "no worker slot budget is available",
            ));
        }
        workspace
            .create_worktree(snapshot, task_id, run_id, write_scope, generation)
            .await
    }

    /// Cancel every live worker. Their side effects become uncertain, not
    /// completed.
    pub fn cancel_descendants(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        if let Ok(live) = self.live.lock() {
            for task in live.values() {
                task.cancellation.cancel();
            }
        }
        self.slots.close();
    }

    /// Cancel and await every descendant, collecting the ones that did not
    /// establish an outcome.
    pub async fn drain_descendants(&self) -> Vec<String> {
        self.cancel_descendants();
        let mut unknowns = Vec::new();
        let mut receiver = self.settled_rx.lock().await;
        // Keep collecting until no dispatched worker is outstanding. A canceled
        // worker always settles, so this terminates.
        loop {
            if self.outstanding.load(Ordering::SeqCst) == 0 {
                break;
            }
            let remaining = 1;
            let _ = remaining;
            {
                match receiver.recv().await {
                    Some(Ok((task_id, outcome))) => {
                        if let WorkerOutcome::OutcomeUnknown { reason } = outcome {
                            unknowns.push(format!("{}: {reason}", task_id.as_str()));
                        }
                    }
                    Some(Err(error)) => unknowns.push(format!("join-error: {error}")),
                    None => break,
                }
            }
        }
        // Workers that were canceled while queued for a slot report themselves
        // through the same channel; collect whatever has already settled.
        loop {
            match receiver.try_recv() {
                Ok(Ok((task_id, WorkerOutcome::OutcomeUnknown { reason }))) => {
                    unknowns.push(format!("{}: {reason}", task_id.as_str()));
                }
                Ok(Ok(_)) => {}
                Ok(Err(error)) => unknowns.push(format!("join-error: {error}")),
                Err(_) => break,
            }
        }
        drop(receiver);
        if let Ok(mut drained) = self.drained.lock() {
            drained.clone_from(&unknowns);
        }
        unknowns
    }

    #[must_use]
    pub fn drained_unknowns(&self) -> Vec<String> {
        self.drained
            .lock()
            .map_or_else(|_| Vec::new(), |drained| drained.clone())
    }

    #[must_use]
    pub fn live_worker_count(&self) -> usize {
        self.live.lock().map_or(0, |live| live.len())
    }

    /// Number of dispatched workers that have not settled yet.
    #[must_use]
    pub fn outstanding_workers(&self) -> u32 {
        self.outstanding.load(Ordering::SeqCst)
    }

    /// Wait until no dispatched worker is outstanding, consuming the outcomes
    /// so the stream stays consistent for later readers.
    pub async fn wait_until_idle(&self) -> Vec<String> {
        let mut unknowns = Vec::new();
        let mut receiver = self.settled_rx.lock().await;
        while self.outstanding.load(Ordering::SeqCst) > 0 {
            match receiver.recv().await {
                Some(Ok((task_id, outcome))) => {
                    if let WorkerOutcome::OutcomeUnknown { reason } = outcome {
                        unknowns.push(format!("{}: {reason}", task_id.as_str()));
                    }
                }
                Some(Err(error)) => unknowns.push(format!("join-error: {error}")),
                None => break,
            }
        }
        unknowns
    }
}

/// A handle to one dispatched worker.
#[derive(Clone, Debug)]
pub struct WorkerHandle {
    pub key: String,
    pub run_id: AgentRunId,
}

/// A compute permit held by a coordinator. Dropping it before waiting on
/// children is what keeps a parent from starving its own subtree.
pub struct WorkerLease {
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl WorkerLease {
    /// Give the slot back. Idempotent.
    pub fn release(&mut self) {
        self.permit = None;
    }

    #[must_use]
    pub const fn is_held(&self) -> bool {
        self.permit.is_some()
    }
}

impl std::fmt::Debug for WorkerLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkerLease")
            .field("held", &self.is_held())
            .finish()
    }
}

impl ManagedResource for WorkerScheduler {
    fn name(&self) -> &'static str {
        "p5-worker-scheduler"
    }

    fn shutdown<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<(), KernelError>> + Send + 'a>> {
        Box::pin(async move {
            let unknowns = self.drain_descendants().await;
            if unknowns.is_empty() {
                Ok(())
            } else {
                // A drained worker whose side effect is not established is a
                // reported error, never a silent clean shutdown.
                Err(KernelError::new(
                    ErrorCode::ProcessOutcomeUnknown,
                    format!("workers drained with uncertain outcomes: {unknowns:?}"),
                ))
            }
        })
    }

    fn join<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<(), KernelError>> + Send + 'a>> {
        Box::pin(async move {
            self.drain_descendants().await;
            Ok(())
        })
    }
}

/// Convenience report for callers that drive shutdown themselves.
#[must_use]
pub fn shutdown_report(drained: &[String]) -> ShutdownReport {
    ShutdownReport {
        closed: vec!["shutdown:p5-worker-scheduler".to_owned()],
        errors: Vec::new(),
        outcome_uncertainties: drained.to_vec(),
    }
}
