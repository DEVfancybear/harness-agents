//! P5 delegation CLI: coordinator/worker runs, task views and cancellation.
//!
//! The CLI is a thin presentation layer over `harness-orchestrator`. It opens
//! the same single-writer store the rest of the harness uses, and it never
//! fabricates a result: only host-recorded durable state is printed.

use std::{collections::BTreeMap, future::Future, path::PathBuf, pin::Pin, sync::Arc};

use clap::{Args, Subcommand};
use harness_orchestrator::{
    AgentRole, DEFAULT_MAX_WORKERS, DelegatedOutcome, DelegatedResult, DelegationBudget,
    DelegationCoordinator, DelegationGrants, OrchestratorError, SchedulerConfig, TaskBrief,
    TaskGraph, TaskNode, TaskPlan, TaskStatus, VerifiedSnapshot, WorkerBackend, WorkerOutcome,
    WorkerRef, WorkerRequest, WorkerScheduler, WorkspaceManager,
};
use harness_providers::{CancellationToken, MockProvider};
use harness_runtime::{RunRequest, RuntimeConfig, RuntimeService};
use harness_session::{AdmitInputRequest, SessionService};
use harness_store_sqlite::{
    SqliteStore, StoredTaskNodeRecord, TaskAdmission, WorktreeRecordRow, WriterOpenOptions,
};
use harness_types::{
    AgentProfileId, AgentRunId, ContentHash, ErrorCode, HarnessError, HostId, InputId, ProjectId,
    SessionId, SourceAuthority, TaskId, ToolExecutionId, ToolExecutionReceipt as RunnerReceipt,
    ToolIntentState, ToolOutcomeState, WorkspaceObservation,
};
use serde_json::json;

use crate::{demo_workspace, store_error};

const DELEGATION_BUDGET_REQUESTS: u32 = 12;

#[derive(Debug, Args)]
pub struct TaskCommand {
    #[command(subcommand)]
    command: TaskSubcommand,
}

#[derive(Debug, Subcommand)]
enum TaskSubcommand {
    /// Run one delegated task with a coordinator and up to three worker slots.
    Run {
        #[arg(long)]
        data_dir: PathBuf,
        /// The delegated objective.
        #[arg(long)]
        text: String,
        /// Concurrent worker slots. Role availability is separate from slots.
        #[arg(long, default_value_t = 3)]
        agents: u32,
        /// Workspace root used to verify the input snapshot (defaults to the cwd).
        #[arg(long)]
        workspace: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Show durable task, ownership, result and delivery state without running work.
    Status {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        task_id: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Inspect the recorded result for one task, including its exact revisions.
    Result {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        task_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Cancel a task. Cancel cascades to descendants; a pause would not.
    Cancel {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        task_id: String,
        #[arg(long)]
        json: bool,
    },
}

/// Run one `ha tasks` subcommand.
pub async fn run(command: TaskCommand) -> Result<(), HarnessError> {
    match command.command {
        TaskSubcommand::Run {
            data_dir,
            text,
            agents,
            workspace,
            json,
        } => run_delegation(&data_dir, &text, agents, workspace, json).await,
        TaskSubcommand::Status {
            data_dir,
            task_id,
            json,
        } => show_tasks(&data_dir, task_id.as_deref(), json).await,
        TaskSubcommand::Result {
            data_dir,
            task_id,
            json,
        } => show_result(&data_dir, &task_id, json).await,
        TaskSubcommand::Cancel {
            data_dir,
            task_id,
            json,
        } => cancel_task(&data_dir, &task_id, json).await,
    }
}

/// Deterministic scripted workers: explorer observes sources, coder edits its
/// own worktree, verifier reruns the declared check against the exact result
/// revision. Only the model boundary is scripted; the session service, runtime
/// admission, transaction coordinator, DAG store, worktrees and scheduler are
/// all real components.
struct ScriptedWorkerBackend {
    store: Arc<SqliteStore>,
}

impl WorkerBackend for ScriptedWorkerBackend {
    #[allow(clippy::too_many_lines)] // One worker turn; the steps are not separable.
    fn dispatch(
        &self,
        request: WorkerRequest,
    ) -> Pin<Box<dyn Future<Output = WorkerOutcome> + Send + '_>> {
        Box::pin(async move {
            if request.cancellation.is_cancelled() {
                return WorkerOutcome::OutcomeUnknown {
                    reason: "worker canceled before dispatch".to_owned(),
                };
            }
            let role = request.brief.role;
            let task_id = request.task_id.clone();
            let run_id = request.run_id.clone();
            let base_commit = request.brief.base_commit.clone();
            let objective = request.brief.objective.clone();
            let mut expected = request.brief.expected_artifacts.clone();
            let worktree = request.worktree.clone();
            let cancellation = request.cancellation.clone();
            // A child agent owns its own session. The coordinator never appends
            // to another agent's event log.
            let session_id = SessionId::generate();
            let runtime = RuntimeService::new(
                Arc::clone(&self.store),
                Arc::new(MockProvider::text(scripted_response(role))),
                RuntimeConfig::default(),
            );
            let run_request = RunRequest::new(
                session_id.clone(),
                task_id.clone(),
                InputId::generate(),
                format!("{} objective: {objective}", role.as_str()),
                demo_workspace(),
            )
            .with_system_policy(format!(
                "You are the {} worker. A role name grants no permission; host grants are explicit.",
                role.as_str()
            ));
            let response = match runtime
                .run_with_cancellation(run_request, cancellation)
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    return WorkerOutcome::Failed {
                        error: OrchestratorError::new(error.code(), error.to_string()),
                    };
                }
            };
            let mut result_revision = base_commit.clone();
            if role.default_edit_capable() {
                let Some(record) = worktree.as_ref() else {
                    return WorkerOutcome::Failed {
                        error: OrchestratorError::new(
                            ErrorCode::ScopeAuthorityDenied,
                            "an editing worker requires a host-owned worktree",
                        ),
                    };
                };
                match edit_scoped_file(record) {
                    Ok(revision) => result_revision = revision,
                    Err(error) => return WorkerOutcome::Failed { error },
                }
            } else {
                expected.retain(|artifact| !artifact.starts_with("edit:"));
            }
            let mut checked_revisions = Vec::new();
            let mut check_receipts = Vec::new();
            let mut outcome = DelegatedOutcome::Completed;
            if role == AgentRole::Verifier {
                let Some(record) = worktree.as_ref() else {
                    return WorkerOutcome::Failed {
                        error: OrchestratorError::new(
                            ErrorCode::ScopeAuthorityDenied,
                            "the verifier requires the host-owned dependency worktree",
                        ),
                    };
                };
                match verify_worktree_revision(record, &base_commit, &task_id) {
                    Ok((revision, receipt)) => {
                        result_revision.clone_from(&revision.revision);
                        if !revision.passed {
                            outcome = DelegatedOutcome::Failed;
                        }
                        checked_revisions.push(revision);
                        check_receipts.push(receipt);
                    }
                    Err(error) => return WorkerOutcome::Failed { error },
                }
            }
            let report = DelegatedResult {
                schema_version: harness_orchestrator::DELEGATION_CONTRACT_VERSION,
                result_id: format!("{}-result", task_id.as_str()),
                task_id: task_id.clone(),
                worker: WorkerRef {
                    profile_id: AgentProfileId::generate(),
                    run_id,
                    role,
                    generation: 1,
                },
                outcome,
                summary: format!(
                    "{} completed against {result_revision}: {}",
                    role.as_str(),
                    response.response
                ),
                artifact_refs: expected,
                base_revision: base_commit,
                result_revision: result_revision.clone(),
                checked_revisions,
                check_receipts,
                usage: harness_orchestrator::BudgetUsage {
                    model_requests: 1,
                    retries: 0,
                },
                detail: json!({"session_id": session_id, "role": role.as_str()}),
            };
            WorkerOutcome::Reported(Box::new(report))
        })
    }
}

fn scripted_response(role: AgentRole) -> &'static str {
    match role {
        AgentRole::Explorer => "recorded source observations for the delegated objective",
        AgentRole::Coder => "edited the scoped file and committed the change",
        AgentRole::Verifier => "reran the declared check against the exact result revision",
        AgentRole::Reviewer => "reviewed the diff against the declared acceptance criteria",
        AgentRole::Coordinator => "coordinated the delegated subtasks",
    }
}

/// Write inside the declared scope and commit, so the recorded revision is a
/// real Git revision produced on the worker's own branch.
fn edit_scoped_file(
    record: &harness_orchestrator::WorktreeRecord,
) -> Result<String, OrchestratorError> {
    let scope = record.write_scope.first().ok_or_else(|| {
        OrchestratorError::new(
            ErrorCode::ScopeAuthorityDenied,
            "an editing worker requires a declared write scope",
        )
    })?;
    let target = PathBuf::from(&record.path).join(scope);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            OrchestratorError::new(
                ErrorCode::ArtifactWriteFailed,
                format!("cannot create the scoped directory: {error}"),
            )
        })?;
    }
    let previous = std::fs::read_to_string(&target).unwrap_or_default();
    let updated = format!(
        "{previous}\npub const P5_DELEGATED_CHANGE: &str = \"{}\";\n",
        record.task_id.as_str()
    );
    std::fs::write(&target, updated).map_err(|error| {
        OrchestratorError::new(
            ErrorCode::ArtifactWriteFailed,
            format!("cannot write the scoped file: {error}"),
        )
    })?;
    git_commit(&record.path, scope)
}

fn git_commit(worktree: &str, scope: &str) -> Result<String, OrchestratorError> {
    let git = |arguments: &[&str]| -> Result<String, OrchestratorError> {
        let output = std::process::Command::new("git")
            .args(arguments)
            .current_dir(worktree)
            .output()
            .map_err(|error| {
                OrchestratorError::new(
                    ErrorCode::ProcessCanceled,
                    format!("cannot run git {}: {error}", arguments.join(" ")),
                )
            })?;
        if !output.status.success() {
            return Err(OrchestratorError::new(
                ErrorCode::StorageWriteFailed,
                format!(
                    "git {} failed: {}",
                    arguments.join(" "),
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    };
    git(&["add", "--", scope])?;
    git(&["commit", "--quiet", "-m", "p5 delegated scoped change"])?;
    Ok(git(&["rev-parse", "HEAD"])?.trim().to_owned())
}

fn verify_worktree_revision(
    record: &harness_orchestrator::WorktreeRecord,
    base_commit: &str,
    task_id: &TaskId,
) -> Result<(harness_orchestrator::CheckedRevision, RunnerReceipt), OrchestratorError> {
    if record.base_commit != base_commit || record.path.trim().is_empty() {
        return Err(OrchestratorError::new(
            ErrorCode::ScopeAuthorityDenied,
            "the verifier worktree does not match the task's declared base revision",
        ));
    }
    let run_git = |arguments: &[&str]| -> Result<std::process::Output, OrchestratorError> {
        std::process::Command::new("git")
            .args(arguments)
            .current_dir(&record.path)
            .output()
            .map_err(|error| {
                OrchestratorError::new(
                    ErrorCode::ProcessCanceled,
                    format!("cannot run verifier command: {error}"),
                )
            })
    };
    let head = run_git(&["rev-parse", "HEAD"])?;
    if !head.status.success() {
        return Err(OrchestratorError::new(
            ErrorCode::StorageWriteFailed,
            format!(
                "cannot resolve verifier revision: {}",
                String::from_utf8_lossy(&head.stderr).trim()
            ),
        ));
    }
    let revision = String::from_utf8_lossy(&head.stdout).trim().to_owned();
    let tree = run_git(&["rev-parse", "HEAD^{tree}"])?;
    if !tree.status.success() {
        return Err(OrchestratorError::new(
            ErrorCode::StorageWriteFailed,
            format!(
                "cannot fingerprint verifier worktree: {}",
                String::from_utf8_lossy(&tree.stderr).trim()
            ),
        ));
    }
    let workspace_digest =
        ContentHash::from_bytes(String::from_utf8_lossy(&tree.stdout).trim().as_bytes());
    let command = format!("git diff --check {base_commit}..{revision}");
    let check = run_git(&["diff", "--check", &format!("{base_commit}..{revision}")])?;
    let exit_code = check.status.code();
    let passed = check.status.success();
    let receipt = RunnerReceipt {
        schema_version: 1,
        tool_execution_id: ToolExecutionId::generate(),
        task_id: task_id.clone(),
        invocation_id: format!("integration-check:{}", task_id.as_str()),
        call_id: None,
        input_hash: ContentHash::from_canonical_json(&json!({
            "command": &command,
            "base_commit": base_commit,
            "revision": &revision,
        }))
        .map_err(|error| OrchestratorError::new(error.code(), error.to_string()))?,
        policy_revision: 1,
        approval_id: None,
        intent_state: ToolIntentState::Validated,
        outcome_state: ToolOutcomeState::Settled,
        before_fingerprint: Some(ContentHash::from_bytes(base_commit.as_bytes())),
        after_fingerprint: Some(workspace_digest.clone()),
        before_hash: None,
        after_hash: None,
        exit_code,
        artifact_id: None,
        observed_at_seq: 1,
    };
    Ok((
        harness_orchestrator::CheckedRevision {
            command,
            revision,
            passed,
            workspace_digest: Some(workspace_digest),
            exit_code,
            artifact_id: None,
        },
        receipt,
    ))
}

async fn open_writer(data_dir: &PathBuf) -> Result<Arc<SqliteStore>, HarnessError> {
    Ok(Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(data_dir, HostId::generate()))
            .await
            .map_err(store_error)?,
    ))
}

#[allow(clippy::too_many_lines)]
async fn run_delegation(
    data_dir: &PathBuf,
    text: &str,
    agents: u32,
    workspace_root: Option<PathBuf>,
    json_output: bool,
) -> Result<(), HarnessError> {
    if agents == 0 || agents > DEFAULT_MAX_WORKERS {
        return Err(HarnessError::new(
            ErrorCode::BudgetExhausted,
            format!("--agents must be between 1 and {DEFAULT_MAX_WORKERS}"),
        ));
    }
    let store = open_writer(data_dir).await?;
    let workspace = Arc::new(WorkspaceManager::new(data_dir.join("delegation")));
    let root = workspace_root.unwrap_or_else(|| PathBuf::from("."));
    // One project identity per workspace root — the same one the interactive app and the
    // memory commands resolve. A fresh id per invocation would put every worktree,
    // artifact and memory this run writes out of reach of the next run.
    let project_id = crate::interactive::project::resolve_project_id(&store, &root).await?;
    let coordinator_task = TaskId::generate();
    // The coordinator session exists before any child result, so a delivery to
    // a not-yet-running parent always has a durable inbox target.
    SessionService::new(Arc::clone(&store))
        .admit_input(AdmitInputRequest {
            session_id: SessionId::generate(),
            task_id: coordinator_task.clone(),
            input_id: InputId::generate(),
            expected_sequence: 1,
            authority: SourceAuthority::User,
            raw_text: text.to_owned(),
            workspace: demo_workspace(),
            initial_plan_items: Vec::new(),
        })
        .await
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
    let snapshot = resolve_snapshot(&workspace, &root, &project_id).await?;
    let plan = build_demo_plan(text, &snapshot, &coordinator_task, agents)?;
    let scheduler = Arc::new(
        WorkerScheduler::new(
            SchedulerConfig {
                max_concurrent_workers: agents,
                max_depth: harness_orchestrator::DEFAULT_MAX_DEPTH,
                // The demo plan is exactly `agents` wide, so a queue is never
                // needed; the bound still exists so the run cannot outgrow the
                // plan if that ever changes.
                max_queued_workers: harness_orchestrator::DEFAULT_MAX_QUEUED_WORKERS,
                budget: DelegationBudget {
                    max_workers: agents,
                    max_model_requests: DELEGATION_BUDGET_REQUESTS,
                    max_retries: 1,
                    max_cost_units: 0,
                },
            },
            Arc::new(ScriptedWorkerBackend {
                store: Arc::clone(&store),
            }),
            Some(Arc::clone(&workspace)),
        )
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?,
    );
    let coordinator = DelegationCoordinator::new(Arc::clone(&store), Arc::clone(&scheduler), 1);
    coordinator
        .admit(&plan)
        .await
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
    dispatch_plan(&store, &workspace, &scheduler, &plan, &snapshot).await?;
    let outcomes = collect_outcomes(&coordinator, &plan, &scheduler).await?;
    let deliveries = store
        .pending_deliveries(&coordinator_task)
        .await
        .map_err(store_error)?;
    let summary = json!({
        "schema_version": 1,
        "coordinator_task_id": coordinator_task,
        "project_id": project_id,
        "snapshot": {
            "root": snapshot.root,
            "base_commit": snapshot.base_commit,
            "base_branch": snapshot.base_branch,
            "fingerprint": snapshot.fingerprint,
        },
        "worker_slots": agents,
        "role_availability": AgentRole::worker_roles()
            .iter()
            .map(|role| role.as_str())
            .collect::<Vec<_>>(),
        "plan": plan.topological_order.iter().map(|task_id| {
            let node = plan.node(task_id);
            json!({
                "task_id": task_id,
                "role": node.map_or("unknown", |node| node.role.as_str()),
                "depth": node.map_or(0, |node| node.depth),
                "depends_on": node.map(|node| node.depends_on.clone()).unwrap_or_default(),
            })
        }).collect::<Vec<_>>(),
        "outcomes": outcomes.iter().map(|step| json!({
            "task_id": step.task_id,
            "status": step.status.as_str(),
            "accepted": step.accepted,
            "stop_reason": step.stop_reason(),
            "claimed_but_unverified": step.claimed_but_unverified(),
            "detail": step.detail,
        })).collect::<Vec<_>>(),
        "verification": {
            "accepted": outcomes.iter().filter(|step| step.accepted).count(),
            "claimed_but_unverified": outcomes
                .iter()
                .filter(|step| step.claimed_but_unverified())
                .count(),
            "note": "a worker's completion report is a claim; only the host's acceptance is verified work",
        },
        "requests_used": scheduler.ledger().requests_used(),
        "pending_deliveries": deliveries.len(),
        "deliveries": deliveries.iter().map(|delivery| json!({
            "message_id": delivery.message_id,
            "sender_task_id": delivery.sender_task_id,
            "recipient_task_id": delivery.recipient_task_id,
            "result_id": delivery.result_id,
            "state": delivery.state,
        })).collect::<Vec<_>>(),
    });
    drop(coordinator);
    drop(scheduler);
    close_store(store).await?;
    if json_output {
        println!("{summary}");
    } else {
        println!(
            "delegation completed: {} tasks, {} accepted, {} pending deliveries",
            outcomes.len(),
            outcomes.iter().filter(|step| step.accepted).count(),
            deliveries.len()
        );
    }
    Ok(())
}

async fn resolve_snapshot(
    workspace: &Arc<WorkspaceManager>,
    root: &PathBuf,
    project_id: &ProjectId,
) -> Result<VerifiedSnapshot, HarnessError> {
    match workspace.inspect_input(root, project_id).await {
        Ok(harness_orchestrator::InputInspection::Clean(snapshot)) => Ok(snapshot),
        Ok(harness_orchestrator::InputInspection::Dirty(reasons)) => {
            // Without a preservation-tested snapshot path the host refuses
            // editing delegation; it never stashes or resets user changes.
            Err(HarnessError::new(
                ErrorCode::DirtyWorkspaceDenied,
                format!(
                    "editing delegation requires a clean repository; found {}",
                    reasons
                        .iter()
                        .map(harness_orchestrator::DirtyReason::describe)
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            ))
        }
        Err(error) => Err(HarnessError::new(error.code(), error.to_string())),
    }
}

async fn dispatch_plan(
    store: &Arc<SqliteStore>,
    workspace: &Arc<WorkspaceManager>,
    scheduler: &Arc<WorkerScheduler>,
    plan: &TaskPlan,
    snapshot: &VerifiedSnapshot,
) -> Result<(), HarnessError> {
    let coordinator_session = store
        .list_sessions()
        .await
        .map_err(store_error)?
        .first()
        .map(|summary| summary.session_id.clone())
        .ok_or_else(|| {
            HarnessError::new(ErrorCode::InvalidPayload, "no coordinator session exists")
        })?;
    let coordinator = DelegationCoordinator::new(Arc::clone(store), Arc::clone(scheduler), 1);
    let mut dispatched: Vec<TaskId> = Vec::new();
    let mut task_worktrees = BTreeMap::new();
    for task_id in &plan.topological_order {
        let node = plan
            .node(task_id)
            .ok_or_else(|| HarnessError::new(ErrorCode::TaskNotFound, "the plan lost a node"))?;
        if node
            .depends_on
            .iter()
            .any(|dependency| !dispatched.contains(dependency))
        {
            return Err(HarnessError::new(
                ErrorCode::TaskNotReady,
                format!("task {task_id} was dispatched before its dependencies ran"),
            ));
        }
        let worker = WorkerRef {
            profile_id: AgentProfileId::generate(),
            run_id: AgentRunId::generate(),
            role: node.role,
            generation: 1,
        };
        let mut worktree = None;
        if node.role.default_edit_capable() {
            let record = workspace
                .create_worktree(
                    snapshot,
                    task_id,
                    &worker.run_id,
                    &node.brief.grants.write_scope,
                    1,
                )
                .await
                .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
            upsert_worktree(store, &record).await?;
            task_worktrees.insert(task_id.clone(), record.clone());
            worktree = Some(record);
        } else if node.role == AgentRole::Verifier {
            worktree = node
                .depends_on
                .iter()
                .find_map(|dependency| task_worktrees.get(dependency).cloned());
        }
        coordinator
            .claim(task_id, &worker, &coordinator_session)
            .await
            .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
        scheduler
            .dispatch(WorkerRequest {
                task_id: task_id.clone(),
                run_id: worker.run_id.clone(),
                generation: worker.generation,
                depth: node.depth,
                brief: node.brief.clone(),
                worktree,
                cancellation: CancellationToken::new(),
            })
            .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
        dispatched.push(task_id.clone());
    }
    Ok(())
}

async fn upsert_worktree(
    store: &Arc<SqliteStore>,
    record: &harness_orchestrator::WorktreeRecord,
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
        .map_err(store_error)
}

async fn collect_outcomes(
    coordinator: &DelegationCoordinator,
    plan: &TaskPlan,
    scheduler: &Arc<WorkerScheduler>,
) -> Result<Vec<harness_orchestrator::StepOutcome>, HarnessError> {
    let mut lease = scheduler
        .acquire_lease()
        .await
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
    let mut outcomes = Vec::new();
    loop {
        let Ok((task_id, outcome)) = coordinator.await_next(&mut lease).await else {
            break;
        };
        let step = coordinator
            .settle(plan, &task_id, outcome)
            .await
            .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
        outcomes.push(step);
    }
    Ok(outcomes)
}

fn build_demo_plan(
    text: &str,
    snapshot: &VerifiedSnapshot,
    coordinator_task: &TaskId,
    agents: u32,
) -> Result<TaskPlan, HarnessError> {
    let explorer = TaskId::generate();
    let coder = TaskId::generate();
    let verifier = TaskId::generate();
    let mut tasks = vec![node(
        &explorer,
        Some(coordinator_task),
        AgentRole::Explorer,
        text,
        snapshot,
        agents,
        &[],
        &["observation:source"],
        Vec::new(),
    )];
    if agents >= 2 {
        tasks.push(node(
            &coder,
            Some(coordinator_task),
            AgentRole::Coder,
            text,
            snapshot,
            agents,
            std::slice::from_ref(&explorer),
            &["edit:src/lib.rs"],
            vec!["src/lib.rs".to_owned()],
        ));
    }
    if agents >= 3 {
        tasks.push(node(
            &verifier,
            Some(coordinator_task),
            AgentRole::Verifier,
            text,
            snapshot,
            agents,
            std::slice::from_ref(&coder),
            &["check:integrated-revision"],
            Vec::new(),
        ));
    }
    TaskPlan::compile(TaskGraph {
        schema_version: harness_orchestrator::DELEGATION_CONTRACT_VERSION,
        coordinator: WorkerRef {
            profile_id: AgentProfileId::generate(),
            run_id: AgentRunId::generate(),
            role: AgentRole::Coordinator,
            generation: 1,
        },
        tasks,
    })
    .map_err(|error| HarnessError::new(error.code(), error.to_string()))
}

#[allow(clippy::too_many_arguments)]
fn node(
    task_id: &TaskId,
    parent: Option<&TaskId>,
    role: AgentRole,
    objective: &str,
    snapshot: &VerifiedSnapshot,
    agents: u32,
    depends_on: &[TaskId],
    expected_artifacts: &[&str],
    write_scope: Vec<String>,
) -> TaskNode {
    let edit = role.default_edit_capable();
    TaskNode {
        schema_version: harness_orchestrator::DELEGATION_CONTRACT_VERSION,
        task_id: task_id.clone(),
        parent_task_id: parent.cloned(),
        role,
        brief: TaskBrief {
            schema_version: harness_orchestrator::DELEGATION_CONTRACT_VERSION,
            task_id: task_id.clone(),
            title: format!("{} delegated task", role.as_str()),
            objective: objective.to_owned(),
            acceptance_criteria: vec![
                "the declared artifact is produced".to_owned(),
                "the result records the revision it was produced against".to_owned(),
            ],
            inputs: vec!["snapshot".to_owned()],
            base_snapshot: snapshot.fingerprint.as_str().to_owned(),
            base_commit: snapshot.base_commit.clone(),
            workspace: WorkspaceObservation {
                project_id: snapshot.project_id.clone(),
                worktree_id: format!("p5-{}", task_id.as_str()),
                base_commit: snapshot.base_commit.clone(),
                observed_fingerprint: snapshot.fingerprint.clone(),
            },
            grants: DelegationGrants {
                project_id: snapshot.project_id.clone(),
                task_id: task_id.clone(),
                actions: vec![harness_orchestrator::GrantAction::Read],
                write_scope,
                edit_workspace: edit,
                max_depth: harness_orchestrator::DEFAULT_MAX_DEPTH,
                budget: DelegationBudget {
                    max_workers: agents,
                    max_model_requests: DELEGATION_BUDGET_REQUESTS,
                    max_retries: 1,
                    max_cost_units: 0,
                },
            },
            expected_artifacts: expected_artifacts
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            deadline_unix_ms: None,
            role,
        },
        depends_on: depends_on.to_vec(),
        status: TaskStatus::Pending,
        depth: 0,
        revision: 1,
    }
}

#[allow(clippy::too_many_lines)]
async fn show_tasks(
    data_dir: &PathBuf,
    task_filter: Option<&str>,
    json_output: bool,
) -> Result<(), HarnessError> {
    let store = Arc::new(
        SqliteStore::open_read_only(data_dir)
            .await
            .map_err(store_error)?,
    );
    let nodes = store.list_task_nodes().await.map_err(store_error)?;
    let filter = task_filter.map(TaskId::parse).transpose()?;
    let worktrees = store.list_worktrees().await.map_err(store_error)?;
    let mut tasks = Vec::new();
    for node in nodes {
        if let Some(filter) = &filter
            && &node.task_id != filter
        {
            continue;
        }
        let owner = store.task_owner(&node.task_id).await.map_err(store_error)?;
        let result = store
            .task_result(&node.task_id)
            .await
            .map_err(store_error)?;
        let deliveries = store
            .pending_deliveries(&node.task_id)
            .await
            .map_err(store_error)?;
        let worktree = worktrees
            .iter()
            .find(|record| record.task_id == node.task_id);
        tasks.push(json!({
            "task_id": node.task_id,
            "role": node.role,
            "status": node.status,
            "revision": node.revision,
            "depth": node.depth,
            "dependencies": node.depends_on,
            "ownership": owner.as_ref().map(|owner| json!({
                "owner_run_id": owner.owner_run_id,
                "owner_session_id": owner.owner_session_id,
                "generation": owner.generation,
            })),
            "result": result.as_ref().map(|result| json!({
                "result_id": result.result_id,
                "outcome": result.outcome,
                "base_revision": result.base_revision,
                "result_revision": result.result_revision,
                "artifacts": result.artifact_refs,
            })),
            "worktree": worktree.map(|record| json!({
                "worktree_id": record.worktree_id,
                "branch": record.branch,
                "state": record.state,
                "result_fingerprint": record.result_fingerprint,
            })),
            "pending_deliveries": deliveries.len(),
        }));
    }
    let remaining = tasks
        .iter()
        .filter(|task| {
            !matches!(
                task["status"].as_str(),
                Some("completed" | "failed" | "canceled")
            )
        })
        .count();
    let output = json!({
        "schema_version": 1,
        "task_count": tasks.len(),
        "remaining_work": remaining,
        "tasks": tasks,
    });
    drop(store);
    if json_output {
        println!("{output}");
    } else {
        println!(
            "{} tasks, {} remaining",
            output["task_count"], output["remaining_work"]
        );
        for task in output["tasks"].as_array().into_iter().flatten() {
            println!("{} {} {}", task["task_id"], task["role"], task["status"]);
        }
    }
    Ok(())
}

async fn show_result(
    data_dir: &PathBuf,
    task_text: &str,
    json_output: bool,
) -> Result<(), HarnessError> {
    let task_id = TaskId::parse(task_text.to_owned())?;
    let store = Arc::new(
        SqliteStore::open_read_only(data_dir)
            .await
            .map_err(store_error)?,
    );
    let node = store
        .task_node(&task_id)
        .await
        .map_err(store_error)?
        .ok_or_else(|| HarnessError::new(ErrorCode::TaskNotFound, "task was not found"))?;
    let result = store.task_result(&task_id).await.map_err(store_error)?;
    let deliveries = store
        .pending_deliveries(&task_id)
        .await
        .map_err(store_error)?;
    let output = json!({
        "schema_version": 1,
        "task_id": task_id,
        "status": node.status,
        "revision": node.revision,
        "result": result.as_ref().map(|result| json!({
            "result_id": result.result_id,
            "outcome": result.outcome,
            "base_revision": result.base_revision,
            "result_revision": result.result_revision,
            "artifacts": result.artifact_refs,
            "result_hash": result.result_hash,
        })),
        "pending_deliveries": deliveries.len(),
    });
    drop(store);
    if json_output {
        println!("{output}");
    } else {
        println!(
            "task {task_id} is {} at revision {}",
            node.status, node.revision
        );
    }
    Ok(())
}

async fn cancel_task(
    data_dir: &PathBuf,
    task_text: &str,
    json_output: bool,
) -> Result<(), HarnessError> {
    let task_id = TaskId::parse(task_text.to_owned())?;
    let store = open_writer(data_dir).await?;
    let node = store
        .task_node(&task_id)
        .await
        .map_err(store_error)?
        .ok_or_else(|| HarnessError::new(ErrorCode::TaskNotFound, "task was not found"))?;
    let status = TaskStatus::parse(&node.status)
        .ok_or_else(|| HarnessError::new(ErrorCode::InvalidPayload, "stored status is unknown"))?;
    let next = status
        .transition(TaskStatus::Canceled)
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
    let descendants = descendants_of(&store, &task_id).await?;
    let mut canceled = Vec::new();
    for descendant in &descendants {
        if let Some(child) = store.task_node(descendant).await.map_err(store_error)?
            && let Some(child_status) = TaskStatus::parse(&child.status)
            && let Ok(terminal) = child_status.transition(TaskStatus::Canceled)
        {
            write_status(&store, &child, terminal).await?;
            canceled.push(descendant.clone());
        }
    }
    write_status(&store, &node, next).await?;
    let output = json!({
        "schema_version": 1,
        "task_id": task_id,
        "status": next.as_str(),
        "canceled_tasks": canceled,
        "descendants": descendants,
    });
    close_store(store).await?;
    if json_output {
        println!("{output}");
    } else {
        println!("task {task_id} canceled; {} descendants", canceled.len());
    }
    Ok(())
}

async fn write_status(
    store: &Arc<SqliteStore>,
    node: &StoredTaskNodeRecord,
    status: TaskStatus,
) -> Result<(), HarnessError> {
    let mut node_json = node.node_json.clone();
    if let Some(object) = node_json.as_object_mut() {
        object.insert("status".to_owned(), json!(status.as_str()));
    }
    store
        .admit_task(TaskAdmission {
            task: StoredTaskNodeRecord {
                task_id: node.task_id.clone(),
                parent_task_id: node.parent_task_id.clone(),
                role: node.role.clone(),
                status: node.status.clone(),
                revision: node.revision,
                depth: node.depth,
                depends_on: node.depends_on.clone(),
                brief_json: node.brief_json.clone(),
                node_json,
            },
            owner: None,
        })
        .await
        .map_err(store_error)
}

async fn descendants_of(
    store: &Arc<SqliteStore>,
    task_id: &TaskId,
) -> Result<Vec<TaskId>, HarnessError> {
    let nodes = store.list_task_nodes().await.map_err(store_error)?;
    let parents: BTreeMap<String, Option<TaskId>> = nodes
        .iter()
        .map(|node| {
            (
                node.task_id.as_str().to_owned(),
                node.parent_task_id.clone(),
            )
        })
        .collect();
    let mut descendants = Vec::new();
    for node in &nodes {
        let mut cursor = node.parent_task_id.clone();
        let mut guard = 0;
        while let Some(parent) = cursor {
            guard += 1;
            if guard > nodes.len() + 1 {
                break;
            }
            if &parent == task_id {
                descendants.push(node.task_id.clone());
                break;
            }
            cursor = parents.get(parent.as_str()).cloned().flatten();
        }
    }
    Ok(descendants)
}

async fn close_store(store: Arc<SqliteStore>) -> Result<(), HarnessError> {
    Arc::try_unwrap(store)
        .map_err(|_| {
            HarnessError::new(
                ErrorCode::StorageWriteFailed,
                "delegation store consumers were not released",
            )
        })?
        .close()
        .await
        .map_err(store_error)
}
