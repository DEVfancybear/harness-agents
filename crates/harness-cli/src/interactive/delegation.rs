//! Bounded, provider-backed explorer delegation for one interactive turn.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use harness_orchestrator::{
    AgentRole, BudgetLedger, BudgetUsage, DelegatedOutcome, DelegatedResult, DelegationBudget,
    DelegationGrants, DirtyReason, GrantAction, InputInspection, OrchestratorError,
    SchedulerConfig, TaskBrief, VerifiedSnapshot, WorkerBackend, WorkerOutcome, WorkerRequest,
    WorkerScheduler, WorkspaceManager, WorktreeRecord,
};
use harness_providers::{CancellationToken, ModelProvider};
use harness_runtime::{RunRequest, RuntimeConfig, RuntimeService};
use harness_session::{AdmitInputRequest, SessionService};
use harness_store_sqlite::{SqliteStore, WorktreeRecordRow};
use harness_tools::{
    ApprovalAnswer, ApprovalGate, ApprovalMode, ApprovalProposal, CodingToolAction,
    ExternalToolCatalog, ExternalToolDispatcher, ExternalTools, PolicyMode, ToolExecutionService,
    ToolOutput, ToolPatternRule, ToolPolicy, ToolPolicyRules, TurnDriver, TurnLimits, TurnObserver,
    TurnOptions, TurnProgress, TurnStop, coding_tool_schemas,
};
use harness_types::{
    AgentProfileId, AgentRunId, ContentHash, ErrorCode, HarnessError, InputId, SessionId,
    SourceAuthority, TaskId, WorkspaceObservation,
};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use super::cost::{CostTracker, ModelPrice, Usage as CostUsage};
use super::events::SessionEvent;
use super::repl::{HostReply, HostRequests};

const MAX_BRIEF_BYTES: usize = 8 * 1024;
const CHILD_MAX_STEPS: u32 = 8;
const CHILD_MAX_TOOL_CALLS: u32 = 16;
const CHILD_DEADLINE: Duration = Duration::from_mins(2);
const CHILD_READ_TOOLS: &[&str] = &[
    "read_file",
    "list_files",
    "search_text",
    "glob",
    "git_status",
    "git_diff",
    "git_log",
];
const EXPLORER_DENY_TOOLS: &[&str] = &[
    "apply_patch",
    "write_file",
    "edit_file",
    "run_process",
    "run_shell",
    "read_process_output",
    "history_search",
    "history_read",
    "task_update",
    "external_tool",
    "ask_user",
];

/// Creates the per-turn worker scheduler and the external-tool adapter for it.
pub struct DelegateHost {
    inner: Arc<DelegateDispatcher>,
    scheduler: Arc<WorkerScheduler>,
    catalog: Arc<DelegateCatalog>,
    dispatcher: Arc<dyn ExternalToolDispatcher>,
    parent_cancellation: CancellationToken,
    status: Arc<Mutex<Vec<String>>>,
}

impl DelegateHost {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: &Arc<SqliteStore>,
        provider: Arc<dyn ModelProvider>,
        runtime_config: RuntimeConfig,
        workspace_root: PathBuf,
        workspace: WorkspaceObservation,
        delegation_state_root: PathBuf,
        hooks: Vec<harness_tools::ConfiguredToolHook>,
        deny_rules: Vec<String>,
        model_price: Option<ModelPrice>,
        approval_gate: Arc<dyn ApprovalGate>,
        sender: UnboundedSender<SessionEvent>,
        parent_cancellation: CancellationToken,
        status: Arc<Mutex<Vec<String>>>,
    ) -> Result<Self, HarnessError> {
        let worker_status = Arc::new(Mutex::new(BTreeMap::new()));
        let ledger_slot = Arc::new(Mutex::new(None));
        let workspace_manager = Arc::new(WorkspaceManager::new(delegation_state_root));
        let backend = Arc::new(InteractiveWorkerBackend {
            store: Arc::clone(store),
            provider,
            runtime_config,
            workspace_root: workspace_root.clone(),
            hooks,
            deny_rules,
            model_price,
            approval_gate,
            sender,
            worker_status: Arc::clone(&worker_status),
            ledger_slot: Arc::clone(&ledger_slot),
            status: Arc::clone(&status),
        });
        let scheduler = Arc::new(
            WorkerScheduler::new(
                SchedulerConfig {
                    max_concurrent_workers: 3,
                    max_depth: 1,
                    max_queued_workers: 3,
                    budget: DelegationBudget::default(),
                },
                backend,
                Some(Arc::clone(&workspace_manager)),
            )
            .map_err(|error| HarnessError::new(error.code(), error.to_string()))?,
        );
        if let Ok(mut ledger) = ledger_slot.lock() {
            *ledger = Some(Arc::clone(scheduler.ledger()));
        }
        let catalog = Arc::new(DelegateCatalog);
        let inner = Arc::new(DelegateDispatcher {
            settled: Mutex::new(BTreeMap::new()),
            settled_notify: tokio::sync::Notify::new(),
            scheduler: Arc::clone(&scheduler),
            workspace_manager: Arc::clone(&workspace_manager),
            store: Arc::clone(store),
            workspace_root,
            workspace,
            parent_cancellation: parent_cancellation.clone(),
        });
        let dispatcher: Arc<dyn ExternalToolDispatcher> = Arc::clone(&inner) as _;
        Ok(Self {
            inner,
            scheduler,
            catalog,
            dispatcher,
            parent_cancellation,
            status,
        })
    }

    #[must_use]
    pub fn tools(&self) -> ExternalTools {
        ExternalTools::new(Arc::clone(&self.catalog) as Arc<dyn ExternalToolCatalog>)
    }

    #[must_use]
    pub fn dispatcher(&self) -> Arc<dyn ExternalToolDispatcher> {
        Arc::clone(&self.dispatcher)
    }

    /// The `rlm.*` host requests of the Python REPL, served by these workers.
    #[must_use]
    pub fn rlm_requests(&self, model: String) -> Arc<dyn HostRequests> {
        Arc::new(RlmChildren {
            dispatcher: Arc::clone(&self.inner),
            model,
            children: Mutex::new(Vec::new()),
        })
    }

    pub fn summary(&self) -> Vec<String> {
        let mut lines = self
            .status
            .lock()
            .map_or_else(|_| Vec::new(), |items| items.clone());
        if lines.is_empty() {
            lines.push(format!(
                "no delegated workers; budget {}/{} model requests",
                self.scheduler.ledger().requests_used(),
                24
            ));
        }
        lines
    }

    /// Parent cancellation reaches every currently executing child before the
    /// worker set is drained. The scheduler owns the descendant tasks.
    pub async fn shutdown(&self) -> Vec<String> {
        if self.parent_cancellation.is_cancelled() {
            self.scheduler.cancel_descendants();
        }
        let unknown = self.scheduler.drain_descendants().await;
        let lines = self.summary();
        if let Ok(mut status) = self.status.lock() {
            *status = lines;
        }
        unknown
    }
}

struct DelegateCatalog;

impl ExternalToolCatalog for DelegateCatalog {
    fn schemas(&self) -> Vec<Value> {
        vec![json!({
            "type": "function",
            "function": {
                "name": "delegate",
                "description": "Delegate a bounded explorer or an isolated-worktree coder to investigate or implement the brief.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "role": {"type": "string", "enum": ["explorer", "coder"]},
                        "brief": {"type": "string", "minLength": 1, "maxLength": MAX_BRIEF_BYTES}
                    },
                    "required": ["role", "brief"],
                    "additionalProperties": false
                }
            }
        })]
    }

    fn resolve(&self, name: &str, arguments: &Value) -> Option<CodingToolAction> {
        (name == "delegate").then(|| CodingToolAction::ExternalTool {
            plugin_id: "delegate".to_owned(),
            tool_name: "delegate".to_owned(),
            arguments: arguments.clone(),
            parent_invocation_id: None,
            timeout_ms: 120_000,
        })
    }
}

/// A worker `start` admitted.
struct StartedWorker {
    task_id: TaskId,
    role: AgentRole,
    /// Stops the parent-cancellation watcher once the worker settles.
    watcher_done: CancellationToken,
    /// Cancels this worker alone.
    cancellation: CancellationToken,
}

struct DelegateDispatcher {
    /// Outcomes received by one waiter for another worker.
    settled: Mutex<BTreeMap<TaskId, WorkerOutcome>>,
    settled_notify: tokio::sync::Notify,
    scheduler: Arc<WorkerScheduler>,
    workspace_manager: Arc<WorkspaceManager>,
    store: Arc<SqliteStore>,
    workspace_root: PathBuf,
    workspace: WorkspaceObservation,
    parent_cancellation: CancellationToken,
}

impl DelegateDispatcher {
    /// Admit and dispatch one worker, returning as soon as it is admitted - the
    /// part of prime-agent's `rlm.spawn` that happens before the child runs.
    #[allow(clippy::too_many_lines)]
    async fn start(
        &self,
        role: AgentRole,
        brief_text: String,
    ) -> Result<StartedWorker, HarnessError> {
        if self.parent_cancellation.is_cancelled() {
            return Err(HarnessError::new(
                ErrorCode::ProviderCanceled,
                "parent turn was canceled before the explorer started",
            ));
        }
        self.scheduler
            .require_admission(1)
            .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
        let task_id = TaskId::generate();
        let run_id = AgentRunId::generate();
        let (workspace, base_commit, base_snapshot, worktree) = match role {
            AgentRole::Explorer => (
                self.workspace.clone(),
                self.workspace.base_commit.clone(),
                self.workspace.observed_fingerprint.as_str().to_owned(),
                None,
            ),
            AgentRole::Coder => {
                let snapshot = inspect_coder_input(
                    &self.workspace_manager,
                    &self.workspace_root,
                    &self.workspace.project_id,
                )
                .await?;
                let write_scope = vec![".".to_owned()];
                let record = self
                    .scheduler
                    .create_worktree(&snapshot, &task_id, &run_id, &write_scope, 1)
                    .await
                    .map_err(coder_unavailable_error)?;
                persist_worktree(&self.store, &record).await?;
                let observation =
                    harness_tools::observe_workspace(record.project_id.clone(), &record.path)
                        .map_err(|error| {
                            HarnessError::new(
                                error.code(),
                                format!("coder worktree cannot be observed: {error}"),
                            )
                        })?;
                (
                    observation,
                    snapshot.base_commit,
                    snapshot.fingerprint.as_str().to_owned(),
                    Some(record),
                )
            }
            _ => {
                return Err(HarnessError::new(
                    ErrorCode::RoleUnavailable,
                    format!("role {} is not delegated by this host", role.as_str()),
                ));
            }
        };
        let brief = TaskBrief {
            schema_version: harness_orchestrator::DELEGATION_CONTRACT_VERSION,
            task_id: task_id.clone(),
            title: brief_text.chars().take(80).collect(),
            objective: brief_text.clone(),
            acceptance_criteria: vec!["return a concise report answering the brief".to_owned()],
            inputs: vec!["current workspace snapshot".to_owned()],
            base_snapshot,
            base_commit,
            workspace,
            grants: DelegationGrants {
                project_id: self.workspace.project_id.clone(),
                task_id: task_id.clone(),
                actions: if role == AgentRole::Coder {
                    vec![GrantAction::Read, GrantAction::Propose]
                } else {
                    vec![GrantAction::Read]
                },
                write_scope: worktree
                    .as_ref()
                    .map_or_else(Vec::new, |record| record.write_scope.clone()),
                edit_workspace: role == AgentRole::Coder,
                max_depth: 1,
                budget: DelegationBudget::default(),
            },
            expected_artifacts: Vec::new(),
            deadline_unix_ms: None,
            role,
        };
        brief
            .validate()
            .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
        let cancellation = CancellationToken::new();
        let watcher_done = CancellationToken::new();
        let watcher = watch_parent_cancellation(
            self.parent_cancellation.clone(),
            cancellation.clone(),
            watcher_done.clone(),
        );
        self.scheduler
            .dispatch(WorkerRequest {
                task_id: task_id.clone(),
                run_id,
                generation: 1,
                depth: 1,
                brief,
                worktree,
                cancellation: cancellation.clone(),
            })
            .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
        drop(watcher);
        Ok(StartedWorker {
            task_id,
            role,
            watcher_done,
            cancellation,
        })
    }

    /// Wait for worker `task_id` to settle, or until `deadline`.
    ///
    /// The scheduler has one settlement queue. The `delegate` tool and every
    /// `rlm.collect` may wait at once, so whoever receives an outcome for another
    /// worker parks it here and wakes the others: no outcome is lost to the wrong
    /// waiter. `Ok(None)` means the deadline passed first.
    async fn wait(
        &self,
        task_id: &TaskId,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<Option<WorkerOutcome>, HarnessError> {
        loop {
            let notified = self.settled_notify.notified();
            if let Some(outcome) = self
                .settled
                .lock()
                .ok()
                .and_then(|mut settled| settled.remove(task_id))
            {
                return Ok(Some(outcome));
            }
            let far = tokio::time::Instant::now() + Duration::from_hours(24);
            tokio::select! {
                received = self.scheduler.next_settled() => match received {
                    Some(Ok((settled_task, outcome))) => {
                        if &settled_task == task_id {
                            return Ok(Some(outcome));
                        }
                        if let Ok(mut settled) = self.settled.lock() {
                            settled.insert(settled_task, outcome);
                        }
                        self.settled_notify.notify_waiters();
                    }
                    Some(Err(error)) => {
                        return Err(HarnessError::new(error.code(), error.to_string()));
                    }
                    None => {
                        // Nothing is outstanding: the outcome is parked or was never
                        // produced.
                        return Ok(self
                            .settled
                            .lock()
                            .ok()
                            .and_then(|mut settled| settled.remove(task_id)));
                    }
                },
                () = notified => {}
                () = tokio::time::sleep_until(deadline.unwrap_or(far)) => return Ok(None),
                () = self.parent_cancellation.cancelled() => {
                    return Err(HarnessError::new(
                        ErrorCode::ProviderCanceled,
                        "parent canceled delegated work",
                    ));
                }
            }
        }
    }

    /// The report of a settled worker, as the `delegate` tool returns it.
    fn report(role: AgentRole, outcome: WorkerOutcome) -> Result<Value, HarnessError> {
        let report = match outcome {
            WorkerOutcome::Reported(report) => report,
            WorkerOutcome::Failed { error } => {
                return Err(HarnessError::new(error.code(), error.message().to_owned()));
            }
            WorkerOutcome::NoReport { reason } | WorkerOutcome::OutcomeUnknown { reason } => {
                return Err(HarnessError::new(ErrorCode::ResultIncomplete, reason));
            }
            WorkerOutcome::Observed(_) => {
                return Err(HarnessError::new(
                    ErrorCode::ResultIncomplete,
                    format!(
                        "{} returned an editing observation without a report",
                        role.as_str()
                    ),
                ));
            }
        };
        let receipt_digest = report.detail["receipts_digest"]
            .as_str()
            .unwrap_or("sha256:unavailable");
        Ok(json!({
            "role": role.as_str(),
            "text": report.summary,
            "receipts_digest": receipt_digest,
            "steps": report.detail["steps"],
            "tool_calls": report.detail["tool_calls"],
            "worktree_id": report.detail["worktree_id"],
            "worktree_path": report.detail["worktree_path"],
            "branch": report.detail["branch"],
        }))
    }

    fn arguments(arguments: &Value) -> Result<(AgentRole, String), HarnessError> {
        let object = arguments.as_object().ok_or_else(|| {
            HarnessError::new(
                ErrorCode::InvalidPayload,
                "delegate arguments must be an object",
            )
        })?;
        let role = match object.get("role").and_then(Value::as_str) {
            Some("explorer") => AgentRole::Explorer,
            Some("coder") => AgentRole::Coder,
            _ => {
                return Err(HarnessError::new(
                    ErrorCode::InvalidPayload,
                    "delegate role must be explorer or coder",
                ));
            }
        };
        let brief = object
            .get("brief")
            .and_then(Value::as_str)
            .filter(|brief| !brief.trim().is_empty() && brief.len() <= MAX_BRIEF_BYTES)
            .ok_or_else(|| {
                HarnessError::new(
                    ErrorCode::InvalidPayload,
                    format!("delegate brief must contain 1..={MAX_BRIEF_BYTES} UTF-8 bytes"),
                )
            })?;
        if object.len() != 2 {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "delegate accepts only role and brief",
            ));
        }
        Ok((role, brief.to_owned()))
    }
}

fn coder_unavailable_error(reason: impl std::fmt::Display) -> HarnessError {
    HarnessError::new(
        ErrorCode::RoleUnavailable,
        format!("coder cannot obtain an M8-03 worktree: {reason}"),
    )
}

fn dirty_reasons(reasons: &[DirtyReason]) -> String {
    reasons
        .iter()
        .map(DirtyReason::describe)
        .collect::<Vec<_>>()
        .join(", ")
}

async fn inspect_coder_input(
    manager: &WorkspaceManager,
    workspace_root: &std::path::Path,
    project_id: &harness_types::ProjectId,
) -> Result<VerifiedSnapshot, HarnessError> {
    match manager
        .inspect_input(workspace_root, project_id)
        .await
        .map_err(coder_unavailable_error)?
    {
        InputInspection::Clean(snapshot) => Ok(snapshot),
        InputInspection::Dirty(reasons) => Err(coder_unavailable_error(dirty_reasons(&reasons))),
    }
}

async fn persist_worktree(
    store: &SqliteStore,
    record: &WorktreeRecord,
) -> Result<(), HarnessError> {
    store
        .upsert_worktree(&WorktreeRecordRow {
            worktree_id: record.worktree_id.clone(),
            task_id: record.task_id.clone(),
            run_id: record.run_id.clone(),
            project_id: record.project_id.clone(),
            base_commit: record.base_commit.clone(),
            base_branch: record.base_branch.clone(),
            branch: record.branch.clone(),
            path: record.path.clone(),
            write_scope: record.write_scope.clone(),
            state: record.state.as_str().to_owned(),
            input_fingerprint: record.input_fingerprint.clone(),
            result_fingerprint: None,
            generation: record.generation,
        })
        .await
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))
}

fn explorer_policy(parent_deny_rules: &[String]) -> ToolPolicy {
    let mut denies = EXPLORER_DENY_TOOLS
        .iter()
        .map(|name| ToolPatternRule::deny(format!("{name}*"), "explorer is read-only"))
        .collect::<Vec<_>>();
    denies.extend(
        parent_deny_rules
            .iter()
            .map(|pattern| ToolPatternRule::deny(pattern, "parent deny rule")),
    );
    ToolPolicy::new(1, Vec::new())
        .with_mode(PolicyMode::Ask)
        .with_tool_rules(denies)
        .with_turn_rules(ToolPolicyRules::default())
}

fn explorer_tool_schemas() -> Vec<Value> {
    coding_tool_schemas()
        .into_iter()
        .filter(|schema| {
            schema["function"]["name"]
                .as_str()
                .is_some_and(|name| CHILD_READ_TOOLS.contains(&name))
        })
        .collect()
}

fn coder_policy(parent_deny_rules: &[String]) -> ToolPolicy {
    let denies = parent_deny_rules
        .iter()
        .map(|pattern| ToolPatternRule::deny(pattern, "parent deny rule"))
        .collect();
    ToolPolicy::new(1, Vec::new())
        .with_mode(PolicyMode::Ask)
        .with_tool_rules(denies)
        .with_turn_rules(ToolPolicyRules::default())
}

fn watch_parent_cancellation(
    parent: CancellationToken,
    child: CancellationToken,
    stop: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tokio::select! {
            () = parent.cancelled() => child.cancel(),
            () = stop.cancelled() => {},
        }
    })
}

impl ExternalToolDispatcher for DelegateDispatcher {
    fn validate_external<'a>(
        &'a self,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            if plugin_id != "delegate" || tool_name != "delegate" {
                return Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "delegation target is unavailable",
                ));
            }
            let (role, _) = Self::arguments(arguments)?;
            if role == AgentRole::Coder {
                inspect_coder_input(
                    &self.workspace_manager,
                    &self.workspace_root,
                    &self.workspace.project_id,
                )
                .await?;
            }
            self.scheduler
                .require_admission(1)
                .map_err(|error| HarnessError::new(error.code(), error.to_string()))
        })
    }

    #[allow(clippy::too_many_lines)]
    fn dispatch_external<'a>(
        &'a self,
        _authorization: &'a harness_tools::ToolDispatchAuthorization,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
        _timeout_ms: u64,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>,
    > {
        Box::pin(async move {
            if plugin_id != "delegate" || tool_name != "delegate" {
                return Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "delegation target is unavailable",
                ));
            }
            let (role, brief_text) = Self::arguments(arguments)?;
            let started = self.start(role, brief_text).await?;
            let outcome = self.wait(&started.task_id, None).await;
            started.watcher_done.cancel();
            let outcome = outcome?.ok_or_else(|| {
                HarnessError::new(
                    ErrorCode::ServiceUnavailable,
                    "explorer result channel closed",
                )
            })?;
            Ok(ToolOutput::ExternalTool {
                plugin_id: "delegate".to_owned(),
                tool_name: "delegate".to_owned(),
                payload: Self::report(started.role, outcome)?,
                inflight: 1,
            })
        })
    }
}

/// Longest child answer `rlm.collect` and `rlm.list_subagents` return.
const RLM_ANSWER_MAX_CHARS: usize = 8_000;

/// prime-agent's `rlm.spawn` family, served by this turn's workers.
///
/// A child is an explorer worker - the same one the `delegate` tool starts - and
/// `rlm.spawn` returns the moment it is admitted, as prime-agent's does. What differs
/// is lifetime: a worker writes through the turn's store, so it cannot outlive the
/// turn; children still running when the turn ends are canceled with it. Collect
/// what you need with `rlm.collect(..., timeout_ms=...)` before the turn ends.
struct RlmChildren {
    dispatcher: Arc<DelegateDispatcher>,
    model: String,
    children: Mutex<Vec<RlmChild>>,
}

struct RlmChild {
    task_id: TaskId,
    name: String,
    started: std::time::Instant,
    watcher_done: CancellationToken,
    cancellation: CancellationToken,
    state: RlmChildState,
}

#[derive(Clone)]
enum RlmChildState {
    Running,
    Done {
        answer: String,
        tool_calls: Option<i64>,
        duration_ms: i64,
    },
    Error {
        error: String,
        duration_ms: i64,
    },
    Cancelled,
}

impl RlmChildren {
    fn session_dir(&self) -> String {
        self.dispatcher.workspace_root.display().to_string()
    }

    /// Find children by id or name; an empty selection means every child.
    fn select(&self, selectors: &[String]) -> Result<Vec<TaskId>, String> {
        let children = self
            .children
            .lock()
            .map_err(|_| "the child registry is unavailable".to_owned())?;
        if selectors.is_empty() {
            return Ok(children.iter().map(|child| child.task_id.clone()).collect());
        }
        selectors
            .iter()
            .map(|selector| {
                children
                    .iter()
                    .find(|child| child.task_id.as_str() == selector || &child.name == selector)
                    .map(|child| child.task_id.clone())
                    .ok_or_else(|| format!("no child named or numbered {selector:?}"))
            })
            .collect()
    }

    /// Settle `task_id` if its outcome arrives before `deadline`.
    async fn settle(&self, task_id: &TaskId, deadline: tokio::time::Instant) {
        let running = self.children.lock().ok().is_some_and(|children| {
            children.iter().any(|child| {
                &child.task_id == task_id && matches!(child.state, RlmChildState::Running)
            })
        });
        if !running {
            return;
        }
        let outcome = self.dispatcher.wait(task_id, Some(deadline)).await;
        let Ok(mut children) = self.children.lock() else {
            return;
        };
        let Some(child) = children.iter_mut().find(|child| &child.task_id == task_id) else {
            return;
        };
        let duration_ms = i64::try_from(child.started.elapsed().as_millis()).unwrap_or(i64::MAX);
        child.state = match outcome {
            Ok(None) => return,
            Ok(Some(outcome)) => match DelegateDispatcher::report(AgentRole::Explorer, outcome) {
                Ok(report) => RlmChildState::Done {
                    answer: report["text"].as_str().unwrap_or_default().to_owned(),
                    tool_calls: report["tool_calls"].as_i64(),
                    duration_ms,
                },
                Err(error) => RlmChildState::Error {
                    error: error.message().to_owned(),
                    duration_ms,
                },
            },
            Err(error) => RlmChildState::Error {
                error: error.message().to_owned(),
                duration_ms,
            },
        };
        child.watcher_done.cancel();
    }

    fn row(&self, child: &RlmChild) -> Value {
        let (status, answer, duration, tool_calls) = match &child.state {
            RlmChildState::Running => ("running", None, None, None),
            RlmChildState::Done {
                answer,
                tool_calls,
                duration_ms,
            } => (
                "completed",
                Some(clip(answer)),
                Some(*duration_ms),
                *tool_calls,
            ),
            RlmChildState::Error { duration_ms, .. } => ("error", None, Some(*duration_ms), None),
            RlmChildState::Cancelled => ("error", None, None, None),
        };
        json!({
            "rlm_child_id": child.task_id.as_str(),
            "session_name": child.name,
            "session_dir": self.session_dir(),
            "status": status,
            "answer_preview": answer,
            "duration_ms": duration,
            "tool_use_count": tool_calls,
        })
    }

    fn result(&self, child: &RlmChild) -> Value {
        let (status, settled, answer, error, duration, tool_calls) = match &child.state {
            RlmChildState::Running => ("running", false, None, None, None, None),
            RlmChildState::Done {
                answer,
                tool_calls,
                duration_ms,
            } => (
                "done",
                true,
                Some(clip(answer)),
                None,
                Some(*duration_ms),
                *tool_calls,
            ),
            RlmChildState::Error { error, duration_ms } => (
                "error",
                true,
                None,
                Some(error.clone()),
                Some(*duration_ms),
                None,
            ),
            RlmChildState::Cancelled => ("cancelled", true, None, None, None, None),
        };
        json!({
            "rlm_child_id": child.task_id.as_str(),
            "session_name": child.name,
            "session_dir": self.session_dir(),
            "status": status,
            "settled": settled,
            "answer_preview": answer,
            "error": error,
            "duration_ms": duration,
            "tool_use_count": tool_calls,
        })
    }

    async fn spawn(&self, request: &Value) -> Result<Value, String> {
        let prompt = request["prompt"]
            .as_str()
            .map(str::trim)
            .filter(|prompt| !prompt.is_empty() && prompt.len() <= MAX_BRIEF_BYTES)
            .ok_or_else(|| format!("rlm.spawn needs a task of 1..={MAX_BRIEF_BYTES} bytes"))?;
        let kwargs = &request["kwargs"];
        let name = kwargs["name"]
            .as_str()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or("rlm.spawn needs a name")?
            .to_owned();
        if let Some(model) = kwargs["model"].as_str()
            && model != self.model
        {
            return Err(format!(
                "a child runs on the parent's model ({}) in ha",
                self.model
            ));
        }
        if !kwargs["thinking"].is_null() {
            return Err("thinking levels are not configurable in ha".to_owned());
        }
        if self
            .children
            .lock()
            .map_err(|_| "the child registry is unavailable")?
            .iter()
            .any(|child| child.name == name)
        {
            return Err(format!("a child named {name:?} already exists"));
        }
        let started = self
            .dispatcher
            .start(AgentRole::Explorer, prompt.to_owned())
            .await
            .map_err(|error| error.message().to_owned())?;
        let handle = json!({
            "rlm_child_id": started.task_id.as_str(),
            "name": name,
            "session_dir": self.session_dir(),
            "model": self.model,
        });
        self.children
            .lock()
            .map_err(|_| "the child registry is unavailable")?
            .push(RlmChild {
                task_id: started.task_id,
                name,
                started: std::time::Instant::now(),
                watcher_done: started.watcher_done,
                cancellation: started.cancellation,
                state: RlmChildState::Running,
            });
        Ok(handle)
    }

    async fn collect(&self, request: &Value) -> Result<Value, String> {
        let selectors = request["targets"]
            .as_array()
            .map(|targets| {
                targets
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let timeout = request["timeout_ms"].as_u64().unwrap_or(0);
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout);
        let targets = self.select(&selectors)?;
        for task_id in &targets {
            self.settle(task_id, deadline).await;
        }
        let children = self
            .children
            .lock()
            .map_err(|_| "the child registry is unavailable")?;
        let results = targets
            .iter()
            .filter_map(|task_id| children.iter().find(|child| &child.task_id == task_id))
            .map(|child| self.result(child))
            .collect::<Vec<_>>();
        Ok(json!({ "results": results }))
    }

    async fn list(&self) -> Result<Value, String> {
        let now = tokio::time::Instant::now();
        for task_id in self.select(&[])? {
            self.settle(&task_id, now).await;
        }
        let children = self
            .children
            .lock()
            .map_err(|_| "the child registry is unavailable")?;
        Ok(json!({ "subagents": children.iter().map(|child| self.row(child)).collect::<Vec<_>>() }))
    }

    fn delete(&self, request: &Value) -> Result<Value, String> {
        let selector = request["target"].as_str().unwrap_or_default().to_owned();
        let task_id = self
            .select(std::slice::from_ref(&selector))?
            .pop()
            .ok_or("no such child")?;
        let mut children = self
            .children
            .lock()
            .map_err(|_| "the child registry is unavailable")?;
        let index = children
            .iter()
            .position(|child| child.task_id == task_id)
            .ok_or("no such child")?;
        let mut child = children.remove(index);
        if matches!(child.state, RlmChildState::Running) {
            child.cancellation.cancel();
            child.watcher_done.cancel();
            child.state = RlmChildState::Cancelled;
        }
        Ok(json!({ "subagent": self.row(&child) }))
    }

    fn models(&self, request: &Value) -> Value {
        let query = request["query"].as_str().unwrap_or_default().to_lowercase();
        let models = if query.is_empty() || self.model.to_lowercase().contains(&query) {
            vec![json!({
                "provider": "ha",
                "id": self.model,
                "name": self.model,
                "selector": self.model,
            })]
        } else {
            Vec::new()
        };
        json!({ "models": models })
    }
}

fn clip(text: &str) -> String {
    if text.chars().count() <= RLM_ANSWER_MAX_CHARS {
        return text.to_owned();
    }
    let kept = text.chars().take(RLM_ANSWER_MAX_CHARS).collect::<String>();
    format!("{kept}\n[answer cut at {RLM_ANSWER_MAX_CHARS} characters]")
}

impl HostRequests for RlmChildren {
    fn handle<'a>(&'a self, request: &'a Value) -> HostReply<'a> {
        Box::pin(async move {
            let kind = request["type"].as_str().unwrap_or_default();
            Some(match kind {
                "rlm.run" => self.spawn(request).await,
                "rlm.collect" => self.collect(request).await,
                "rlm.list_subagents" => self.list().await,
                "rlm.delete_subagent" => self.delete(request),
                "rlm.find_models" => Ok(self.models(request)),
                "rlm.progress.note" => Err(
                    "progress notes are sent by child agents; this is the root agent".to_owned(),
                ),
                "rlm.create_session" => {
                    Err("separate top-level sessions are not available in ha".to_owned())
                }
                _ => return None,
            })
        })
    }
}

struct InteractiveWorkerBackend {
    store: Arc<SqliteStore>,
    provider: Arc<dyn ModelProvider>,
    runtime_config: RuntimeConfig,
    workspace_root: PathBuf,
    hooks: Vec<harness_tools::ConfiguredToolHook>,
    deny_rules: Vec<String>,
    model_price: Option<ModelPrice>,
    approval_gate: Arc<dyn ApprovalGate>,
    sender: UnboundedSender<SessionEvent>,
    worker_status: Arc<Mutex<BTreeMap<String, String>>>,
    ledger_slot: Arc<Mutex<Option<Arc<BudgetLedger>>>>,
    status: Arc<Mutex<Vec<String>>>,
}

impl WorkerBackend for InteractiveWorkerBackend {
    #[allow(clippy::too_many_lines)]
    fn dispatch(
        &self,
        request: WorkerRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = WorkerOutcome> + Send + '_>> {
        Box::pin(async move {
            let task_id = request.task_id.clone();
            let task_key = task_id.as_str().to_owned();
            let role_name = request.brief.role.as_str().to_owned();
            let (workspace_root, policy, schemas, system_policy) = match request.brief.role {
                AgentRole::Explorer => (
                    self.workspace_root.clone(),
                    explorer_policy(&self.deny_rules),
                    explorer_tool_schemas(),
                    "You are a read-only explorer. Inspect the current workspace and answer the brief. You cannot edit files, run commands, call external tools, or delegate again.",
                ),
                AgentRole::Coder => {
                    let Some(worktree) = request.worktree.as_ref() else {
                        return WorkerOutcome::Failed {
                            error: OrchestratorError::new(
                                ErrorCode::RoleUnavailable,
                                "coder worker was dispatched without its M8-03 worktree",
                            ),
                        };
                    };
                    if worktree.task_id != task_id
                        || worktree.run_id != request.run_id
                        || worktree.project_id != request.brief.grants.project_id
                        || !request.brief.grants.edit_workspace
                        || request.brief.grants.write_scope != worktree.write_scope
                    {
                        return WorkerOutcome::Failed {
                            error: OrchestratorError::new(
                                ErrorCode::ScopeAuthorityDenied,
                                "coder worktree does not match the host-issued task grant",
                            ),
                        };
                    }
                    (
                        PathBuf::from(&worktree.path),
                        coder_policy(&self.deny_rules),
                        coding_tool_schemas(),
                        "You are a delegated coder. Work only in the assigned isolated M8-03 worktree and follow the brief. Tool calls use the host approval and policy service. Leave your changes in that worktree and report what changed; do not claim the changes were merged into the user's checkout.",
                    )
                }
                _ => {
                    return WorkerOutcome::Failed {
                        error: OrchestratorError::new(
                            ErrorCode::RoleUnavailable,
                            format!("role {role_name} has no interactive worker backend"),
                        ),
                    };
                }
            };
            if request.cancellation.is_cancelled() {
                return WorkerOutcome::OutcomeUnknown {
                    reason: "parent canceled before the worker session started".to_owned(),
                };
            }
            if let Ok(mut status) = self.worker_status.lock() {
                status.insert(
                    task_key.clone(),
                    format!("{role_name} {task_key}: starting"),
                );
                if let Ok(mut summary) = self.status.lock() {
                    *summary = status.values().cloned().collect();
                }
            }
            let session_id = SessionId::generate();
            let input_id = InputId::generate();
            if let Err(error) = SessionService::new(Arc::clone(&self.store))
                .admit_input(AdmitInputRequest {
                    session_id: session_id.clone(),
                    task_id: task_id.clone(),
                    input_id: input_id.clone(),
                    expected_sequence: 1,
                    authority: SourceAuthority::ModelProposed,
                    raw_text: request.brief.objective.clone(),
                    workspace: request.brief.workspace.clone(),
                    initial_plan_items: Vec::new(),
                })
                .await
            {
                return WorkerOutcome::Failed {
                    error: OrchestratorError::new(error.code(), error.to_string()),
                };
            }
            let tools = ToolExecutionService::new(Arc::clone(&self.store))
                .with_policy(policy)
                .with_hooks(self.hooks.clone());
            let runtime = Arc::new(RuntimeService::new(
                Arc::clone(&self.store),
                Arc::clone(&self.provider),
                self.runtime_config.clone(),
            ));
            let run_request = RunRequest::new(
                session_id,
                task_id.clone(),
                input_id,
                format!(
                    "Answer the delegated task below. Return concise results and identify changed files or findings with paths.\n\nBrief:\n{}",
                    request.brief.objective
                ),
                request.brief.workspace.clone(),
            )
            .with_system_policy(system_policy)
            .with_tool_schemas(schemas);
            let driver = TurnDriver::new(Arc::clone(&runtime), tools);
            let observer = Arc::new(ExplorerObserver {
                task_key: task_key.clone(),
                role_name: role_name.clone(),
                worker_status: Arc::clone(&self.worker_status),
                ledger: self
                    .ledger_slot
                    .lock()
                    .ok()
                    .and_then(|ledger| ledger.clone()),
                model_price: self.model_price,
                cancellation: request.cancellation.clone(),
                budget_error: Mutex::new(None),
                sender: self.sender.clone(),
                token_usage: Mutex::new((0_u64, 0_u64)),
                cost_tracker: Mutex::new(CostTracker::default()),
                summary_status: Arc::clone(&self.status),
            });
            let approval: Arc<dyn ApprovalGate> = Arc::new(ChildApprovalGate {
                role: role_name.clone(),
                inner: Arc::clone(&self.approval_gate),
            });
            let result = driver
                .run_turn(
                    run_request,
                    TurnOptions {
                        workspace_root: workspace_root.clone(),
                        actor_id: format!("interactive.child.{role_name}"),
                        approvals: ApprovalMode::Ask(approval),
                        limits: TurnLimits {
                            max_steps: CHILD_MAX_STEPS,
                            max_tool_calls: CHILD_MAX_TOOL_CALLS,
                            deadline: CHILD_DEADLINE,
                        },
                    },
                    observer.clone(),
                    request.cancellation.clone(),
                )
                .await;
            if let Some(error) = observer
                .budget_error
                .lock()
                .ok()
                .and_then(|error| error.clone())
            {
                return WorkerOutcome::Failed { error };
            }
            let outcome = match result {
                Ok(outcome) => outcome,
                Err(error) => {
                    return WorkerOutcome::Failed {
                        error: OrchestratorError::new(error.code(), error.to_string()),
                    };
                }
            };
            if outcome.stop == TurnStop::Canceled || request.cancellation.is_cancelled() {
                return WorkerOutcome::OutcomeUnknown {
                    reason: "parent turn canceled the explorer".to_owned(),
                };
            }
            let receipts = serde_json::to_vec(&outcome.executions).unwrap_or_default();
            let receipts_digest = ContentHash::from_bytes(&receipts).as_str().to_owned();
            let (prompt_tokens, completion_tokens) =
                observer.token_usage.lock().map_or((0, 0), |usage| *usage);
            let summary = if outcome.final_text.trim().is_empty() {
                "Explorer completed without a final text answer.".to_owned()
            } else {
                outcome.final_text
            };
            let report = DelegatedResult {
                schema_version: harness_orchestrator::DELEGATION_CONTRACT_VERSION,
                result_id: format!("{task_key}-result"),
                task_id: task_id.clone(),
                worker: harness_orchestrator::WorkerRef {
                    profile_id: AgentProfileId::generate(),
                    run_id: request.run_id,
                    role: request.brief.role,
                    generation: request.generation,
                },
                outcome: DelegatedOutcome::Completed,
                summary,
                artifact_refs: Vec::new(),
                base_revision: request.brief.base_commit.clone(),
                result_revision: request.brief.base_commit.clone(),
                checked_revisions: Vec::new(),
                check_receipts: Vec::new(),
                usage: BudgetUsage {
                    model_requests: outcome.steps,
                    retries: 0,
                },
                detail: json!({
                    "session_id": outcome.session_id,
                    "steps": outcome.steps,
                    "tool_calls": outcome.tool_calls,
                    "prompt_tokens": prompt_tokens,
                    "completion_tokens": completion_tokens,
                    "receipts_digest": receipts_digest,
                    "worktree_id": request.worktree.as_ref().map(|record| record.worktree_id.as_str()),
                    "worktree_path": request.worktree.as_ref().map(|record| record.path.as_str()),
                    "branch": request.worktree.as_ref().map(|record| record.branch.as_str()),
                }),
            };
            if let Ok(mut status) = self.worker_status.lock() {
                status.insert(
                    task_key.clone(),
                    format!(
                        "{role_name} {task_key}: completed, {} step(s), {} tool call(s), cost {}",
                        outcome.steps,
                        outcome.tool_calls,
                        observer
                            .cost_tracker
                            .lock()
                            .map_or_else(|_| "n/a".to_owned(), |tracker| tracker.display())
                    ),
                );
                if let Ok(mut last_status) = self.status.lock() {
                    *last_status = status.values().cloned().collect();
                }
            }
            WorkerOutcome::Reported(Box::new(report))
        })
    }
}

struct ExplorerObserver {
    task_key: String,
    role_name: String,
    worker_status: Arc<Mutex<BTreeMap<String, String>>>,
    ledger: Option<Arc<BudgetLedger>>,
    model_price: Option<ModelPrice>,
    cancellation: CancellationToken,
    budget_error: Mutex<Option<OrchestratorError>>,
    sender: UnboundedSender<SessionEvent>,
    token_usage: Mutex<(u64, u64)>,
    cost_tracker: Mutex<CostTracker>,
    summary_status: Arc<Mutex<Vec<String>>>,
}

impl TurnObserver for ExplorerObserver {
    fn observe(&self, progress: TurnProgress) {
        match progress {
            TurnProgress::StepStarted { step } => {
                if step > 1
                    && let Some(ledger) = &self.ledger
                    && let Err(error) = ledger.charge_request()
                {
                    if let Ok(mut budget_error) = self.budget_error.lock() {
                        *budget_error = Some(error);
                    }
                    self.cancellation.cancel();
                }
                if let Ok(mut status) = self.worker_status.lock() {
                    status.insert(
                        self.task_key.clone(),
                        self.status_text(&format!("step {step}/{CHILD_MAX_STEPS}")),
                    );
                    if let Ok(mut summary) = self.summary_status.lock() {
                        *summary = status.values().cloned().collect();
                    }
                }
            }
            TurnProgress::Usage {
                prompt_tokens,
                completion_tokens,
            } => {
                if let Ok(mut usage) = self.token_usage.lock() {
                    usage.0 = usage.0.saturating_add(prompt_tokens);
                    usage.1 = usage.1.saturating_add(completion_tokens);
                }
                if let Ok(mut tracker) = self.cost_tracker.lock() {
                    tracker.record(
                        self.model_price,
                        CostUsage {
                            input_tokens: prompt_tokens,
                            output_tokens: completion_tokens,
                        },
                    );
                }
            }
            TurnProgress::ToolStarted { name, summary } => {
                let _ = self.sender.send(SessionEvent::Notice {
                    message: format!("[child {}] {name}: {summary}", self.role_name),
                });
            }
            TurnProgress::ToolSettled { name, ok, detail } if !ok => {
                let _ = self.sender.send(SessionEvent::Notice {
                    message: format!(
                        "[child {}] {name} failed: {}",
                        self.role_name,
                        detail.unwrap_or_else(|| "no detail".to_owned())
                    ),
                });
            }
            TurnProgress::Notice(message) => {
                let _ = self.sender.send(SessionEvent::Notice {
                    message: format!("[child {}] {message}", self.role_name),
                });
            }
            TurnProgress::TextDelta(_)
            | TurnProgress::ThinkingDelta(_)
            | TurnProgress::ToolSettled { .. }
            | TurnProgress::Info(_) => {}
        }
    }
}

impl ExplorerObserver {
    fn status_text(&self, state: &str) -> String {
        let cost = self
            .cost_tracker
            .lock()
            .map_or_else(|_| "n/a".to_owned(), |tracker| tracker.display());
        format!("{} {}: {state}, cost {cost}", self.role_name, self.task_key)
    }
}

struct ChildApprovalGate {
    role: String,
    inner: Arc<dyn ApprovalGate>,
}

impl ApprovalGate for ChildApprovalGate {
    fn request(
        &self,
        mut proposal: ApprovalProposal,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ApprovalAnswer> + Send>> {
        proposal.summary = format!("[child {}] {}", self.role, proposal.summary);
        self.inner.request(proposal)
    }

    fn action_completed(&self, request_id: &str) {
        self.inner.action_completed(request_id);
    }
}

#[cfg(test)]
mod tests {
    use std::{process::Command, sync::Arc, time::Duration};

    use harness_orchestrator::{
        BudgetLedger, RefusingBackend, SchedulerConfig, WorkerScheduler, WorkspaceManager,
    };
    use harness_providers::CancellationToken;
    use harness_tools::{CodingToolAction, TurnObserver, TurnProgress};
    use harness_types::{AgentRunId, ErrorCode, ProjectId, TaskId};

    use super::{
        DelegateCatalog, ExplorerObserver, explorer_policy, explorer_tool_schemas,
        inspect_coder_input, watch_parent_cancellation,
    };
    use crate::interactive::cost::CostTracker;

    fn git(repo: &std::path::Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git starts");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn clean_repository(repo: &std::path::Path) {
        git(repo, &["init", "-b", "main"]);
        git(repo, &["config", "user.email", "worker-test@localhost"]);
        git(repo, &["config", "user.name", "worker-test"]);
        std::fs::write(repo.join("README.md"), "clean workspace\n").expect("seed repo");
        git(repo, &["add", "README.md"]);
        git(repo, &["commit", "-m", "seed"]);
    }

    #[test]
    fn g12_explorer_child_cannot_call_mutating_tools() {
        let policy = explorer_policy(&[]);
        let writes = [
            CodingToolAction::WriteFile {
                path: "note.txt".to_owned(),
                content: "bad".to_owned(),
                expected_hash: None,
            },
            CodingToolAction::RunShell {
                command: "echo bad".to_owned(),
                timeout_ms: 1000,
                isolation: harness_tools::IsolationMode::BestEffort,
                env: Vec::new(),
            },
            CodingToolAction::TaskUpdate {
                note: "recursive state change".to_owned(),
            },
        ];
        for action in writes {
            assert!(policy.denial_for(&action).is_some(), "{action:?}");
        }
        let read = CodingToolAction::ReadFile {
            path: "src/lib.rs".to_owned(),
            offset: None,
            limit: None,
        };
        assert!(policy.denial_for(&read).is_none());
    }

    #[test]
    fn g12_child_cannot_delegate_again() {
        let schemas = explorer_tool_schemas();
        let names = schemas
            .iter()
            .filter_map(|schema| schema["function"]["name"].as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"read_file"));
        assert!(!names.contains(&"delegate"));
        assert!(!names.contains(&"write_file"));
        assert!(!names.contains(&"run_shell"));
    }

    #[test]
    fn g12_child_budget_is_charged_to_the_parent_ledger() {
        let mut config = SchedulerConfig::default();
        config.budget.max_model_requests = 2;
        let ledger = Arc::new(BudgetLedger::new(&config));
        ledger.charge_request().expect("dispatch charged step one");
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let observer = ExplorerObserver {
            task_key: "task-child".to_owned(),
            role_name: "explorer".to_owned(),
            worker_status: Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::default())),
            ledger: Some(Arc::clone(&ledger)),
            model_price: None,
            cancellation: CancellationToken::new(),
            budget_error: std::sync::Mutex::new(None),
            sender,
            token_usage: std::sync::Mutex::new((0, 0)),
            cost_tracker: std::sync::Mutex::new(CostTracker::default()),
            summary_status: Arc::new(std::sync::Mutex::new(Vec::new())),
        };
        observer.observe(TurnProgress::StepStarted { step: 2 });
        assert_eq!(ledger.requests_used(), 2);
        assert_eq!(ledger.remaining_requests(), 0);
    }

    #[tokio::test]
    async fn g12_parent_cancel_stops_the_child_within_grace() {
        let parent = CancellationToken::new();
        let child = CancellationToken::new();
        let stop = CancellationToken::new();
        let watcher = watch_parent_cancellation(parent.clone(), child.clone(), stop);
        parent.cancel();
        tokio::time::timeout(Duration::from_millis(100), child.cancelled())
            .await
            .expect("child receives parent cancellation");
        tokio::time::timeout(Duration::from_millis(100), watcher)
            .await
            .expect("cancellation watcher settles")
            .expect("watcher task succeeds");
    }

    #[tokio::test]
    async fn g12_coder_gets_a_real_m8_worktree_for_a_clean_workspace() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let repo = temporary.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo directory");
        clean_repository(&repo);

        let manager = Arc::new(WorkspaceManager::new(temporary.path().join("delegation")));
        let project_id = ProjectId::generate();
        let snapshot = inspect_coder_input(&manager, &repo, &project_id)
            .await
            .expect("clean input admits coder");
        let scheduler = WorkerScheduler::new(
            SchedulerConfig::default(),
            Arc::new(RefusingBackend),
            Some(manager),
        )
        .expect("scheduler with M8 workspace manager");
        let task_id = TaskId::generate();
        let record = scheduler
            .create_worktree(
                &snapshot,
                &task_id,
                &AgentRunId::generate(),
                &[".".to_owned()],
                1,
            )
            .await
            .expect("M8-03 creates an isolated coder worktree");

        assert_eq!(record.task_id, task_id);
        assert_eq!(record.project_id, project_id);
        assert_eq!(record.write_scope, vec![".".to_owned()]);
        assert!(
            std::path::Path::new(&record.path)
                .join("README.md")
                .is_file()
        );
        let branch = Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(&record.path)
            .output()
            .expect("read worktree branch");
        assert!(branch.status.success());
        assert!(
            String::from_utf8_lossy(&branch.stdout)
                .trim()
                .starts_with("harness/p5/")
        );
    }

    #[tokio::test]
    async fn g12_coder_refuses_a_dirty_parent_without_losing_changes() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let repo = temporary.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo directory");
        clean_repository(&repo);
        std::fs::write(repo.join("uncommitted.txt"), "preserve me\n").expect("dirty input");

        let manager = WorkspaceManager::new(temporary.path().join("delegation"));
        let error = inspect_coder_input(&manager, &repo, &ProjectId::generate())
            .await
            .expect_err("dirty workspace is not silently copied or discarded");
        assert_eq!(error.code(), ErrorCode::RoleUnavailable);
        assert!(error.message().contains("untracked file uncommitted.txt"));
        assert_eq!(
            std::fs::read_to_string(repo.join("uncommitted.txt")).expect("user change remains"),
            "preserve me\n"
        );
    }

    /// Reports each brief back as its answer; "slow" briefs take a moment, "fail"
    /// briefs fail.
    struct ScriptedBackend;

    impl harness_orchestrator::WorkerBackend for ScriptedBackend {
        fn dispatch(
            &self,
            request: harness_orchestrator::WorkerRequest,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = harness_orchestrator::WorkerOutcome> + Send + '_>,
        > {
            Box::pin(async move {
                let objective = request.brief.objective.clone();
                if objective.contains("slow") {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                }
                if objective.contains("fail") {
                    return harness_orchestrator::WorkerOutcome::Failed {
                        error: harness_orchestrator::OrchestratorError::new(
                            ErrorCode::ServiceUnavailable,
                            "scripted failure",
                        ),
                    };
                }
                harness_orchestrator::WorkerOutcome::Reported(Box::new(
                    harness_orchestrator::DelegatedResult {
                        schema_version: harness_orchestrator::DELEGATION_CONTRACT_VERSION,
                        result_id: "result".to_owned(),
                        task_id: request.task_id.clone(),
                        worker: harness_orchestrator::WorkerRef {
                            profile_id: harness_types::AgentProfileId::generate(),
                            run_id: request.run_id,
                            role: request.brief.role,
                            generation: request.generation,
                        },
                        outcome: harness_orchestrator::DelegatedOutcome::Completed,
                        summary: format!("answer: {objective}"),
                        artifact_refs: Vec::new(),
                        base_revision: request.brief.base_commit.clone(),
                        result_revision: request.brief.base_commit.clone(),
                        checked_revisions: Vec::new(),
                        check_receipts: Vec::new(),
                        usage: harness_orchestrator::BudgetUsage {
                            model_requests: 1,
                            retries: 0,
                        },
                        detail: serde_json::json!({"steps": 1, "tool_calls": 2}),
                    },
                ))
            })
        }
    }

    /// prime-agent's `rlm.spawn` family over this turn's workers: spawn returns at
    /// admission, collect waits within its timeout, and an outcome one waiter
    /// receives for another child is kept for it rather than lost.
    #[tokio::test]
    #[allow(clippy::too_many_lines, reason = "one scenario, told in order")]
    async fn rlm_children_spawn_collect_list_and_delete() {
        use super::{DelegateDispatcher, RlmChildren};
        use crate::interactive::repl::HostRequests;
        use serde_json::{Value, json};

        let temporary = tempfile::tempdir().expect("temporary directory");
        let repo = temporary.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo directory");
        clean_repository(&repo);
        let store = Arc::new(
            harness_store_sqlite::SqliteStore::open_writer(
                harness_store_sqlite::WriterOpenOptions::new(
                    temporary.path().join("store"),
                    harness_types::HostId::generate(),
                ),
            )
            .await
            .expect("store"),
        );
        let manager = Arc::new(WorkspaceManager::new(temporary.path().join("delegation")));
        let scheduler = Arc::new(
            WorkerScheduler::new(
                SchedulerConfig {
                    max_concurrent_workers: 3,
                    max_depth: 1,
                    max_queued_workers: 3,
                    budget: harness_orchestrator::DelegationBudget::default(),
                },
                Arc::new(ScriptedBackend),
                Some(Arc::clone(&manager)),
            )
            .expect("scheduler"),
        );
        let workspace = harness_tools::observe_workspace(ProjectId::generate(), &repo)
            .expect("workspace observation");
        let children = RlmChildren {
            dispatcher: Arc::new(DelegateDispatcher {
                settled: std::sync::Mutex::new(std::collections::BTreeMap::new()),
                settled_notify: tokio::sync::Notify::new(),
                scheduler,
                workspace_manager: manager,
                store,
                workspace_root: repo.clone(),
                workspace,
                parent_cancellation: CancellationToken::new(),
            }),
            model: "fixture-model".to_owned(),
            children: std::sync::Mutex::new(Vec::new()),
        };
        let ask = |request: Value| {
            let children = &children;
            async move {
                children
                    .handle(&request)
                    .await
                    .expect("a known request type")
            }
        };

        let quick = ask(
            json!({"type": "rlm.run", "prompt": "map the parser", "kwargs": {"name": "quick"}}),
        )
        .await
        .expect("spawn");
        assert_eq!(quick["name"], "quick");
        assert_eq!(quick["model"], "fixture-model");
        let slow =
            ask(json!({"type": "rlm.run", "prompt": "slow survey", "kwargs": {"name": "slow"}}))
                .await
                .expect("spawn returns at admission, before the slow child finishes");
        assert!(
            ask(json!({"type": "rlm.run", "prompt": "again", "kwargs": {"name": "quick"}}))
                .await
                .is_err(),
            "names are unique among siblings"
        );
        assert!(
            ask(json!({"type": "rlm.run", "prompt": "x", "kwargs": {"name": "other", "model": "else"}}))
                .await
                .expect_err("another model")
                .contains("parent's model")
        );

        // Collect the slow child first: the quick child's outcome arrives while
        // waiting and must be kept for it.
        let collected = ask(
            json!({"type": "rlm.collect", "targets": [slow["rlm_child_id"]], "timeout_ms": 10_000}),
        )
        .await
        .expect("collect");
        let result = &collected["results"][0];
        assert_eq!(result["status"], "done", "{collected}");
        assert_eq!(result["settled"], true);
        assert_eq!(result["answer_preview"], "answer: slow survey");
        assert_eq!(result["tool_use_count"], 2);
        let collected = ask(json!({"type": "rlm.collect", "targets": ["quick"], "timeout_ms": 0}))
            .await
            .expect("collect by name");
        assert_eq!(
            collected["results"][0]["answer_preview"],
            "answer: map the parser"
        );

        let listed = ask(json!({"type": "rlm.list_subagents"}))
            .await
            .expect("list");
        let rows = listed["subagents"].as_array().expect("rows");
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter().all(|row| row["status"] == "completed"),
            "{listed}"
        );

        let deleted = ask(json!({"type": "rlm.delete_subagent", "target": "quick"}))
            .await
            .expect("delete");
        assert_eq!(deleted["subagent"]["session_name"], "quick");
        let listed = ask(json!({"type": "rlm.list_subagents"}))
            .await
            .expect("list");
        assert_eq!(listed["subagents"].as_array().expect("rows").len(), 1);

        let failed =
            ask(json!({"type": "rlm.run", "prompt": "fail please", "kwargs": {"name": "broken"}}))
                .await
                .expect("spawn");
        let collected = ask(json!({"type": "rlm.collect", "targets": [failed["rlm_child_id"]], "timeout_ms": 10_000}))
            .await
            .expect("collect");
        assert_eq!(collected["results"][0]["status"], "error");
        assert!(
            collected["results"][0]["error"]
                .as_str()
                .is_some_and(|error| error.contains("scripted failure"))
        );

        assert!(
            ask(json!({"type": "rlm.progress.note", "message": "hi"}))
                .await
                .is_err(),
            "the root sends no progress notes"
        );
        assert!(
            children
                .handle(&json!({"type": "custom.thing"}))
                .await
                .is_none()
        );
    }

    #[test]
    fn g12_delegate_tool_schema_is_bounded_and_only_offers_known_roles() {
        let tools = harness_tools::ExternalTools::new(Arc::new(DelegateCatalog));
        let schemas = tools.schemas();
        let schema = &schemas[0]["function"];
        assert_eq!(schema["name"], "delegate");
        assert_eq!(
            schema["parameters"]["properties"]["brief"]["maxLength"],
            8192
        );
        assert_eq!(
            schema["parameters"]["properties"]["role"]["enum"],
            serde_json::json!(["explorer", "coder"])
        );
    }
}
