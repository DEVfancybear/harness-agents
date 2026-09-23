//! Shared fixtures for the P5 acceptance target.
#![allow(dead_code)]

use std::{
    collections::BTreeSet,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    process::{Command, Output},
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use harness_orchestrator::{
    AgentRole, CheckedRevision, DEFAULT_MAX_DEPTH, DELEGATION_CONTRACT_VERSION, DelegatedOutcome,
    DelegatedResult, DelegationBudget, DelegationCoordinator, DelegationGrants, GrantAction,
    OrchestratorError, SchedulerConfig, TaskBrief, TaskGraph, TaskNode, TaskPlan, TaskStatus,
    VerifiedSnapshot, WorkerBackend, WorkerOutcome, WorkerRef, WorkerRequest, WorkerScheduler,
    WorkspaceManager,
};

use harness_store_sqlite::{SqliteStore, StoreFaultPlan, WriterOpenOptions};
use harness_types::{
    AgentProfileId, AgentRunId, ContentHash, HostId, InputId, ProjectId, SessionId,
    SourceAuthority, TaskId, ToolExecutionId, ToolExecutionReceipt, ToolIntentState,
    ToolOutcomeState, WorkspaceObservation,
};

/// Requests available to a fixture budget.
pub const FIXTURE_REQUESTS: u32 = 24;

/// The delegation workspace manager for one fixture repository.
///
/// Public so the M8 milestone target reuses the same state root rather than
/// inventing a second layout; the worktrees it creates are still per-test.
#[must_use]
pub fn workspace_for(root: &TestRepo) -> Arc<WorkspaceManager> {
    Arc::new(WorkspaceManager::new(root.data_dir("delegation")))
}

/// A real Git repository in a temporary directory.
pub struct TestRepo {
    root: tempfile::TempDir,
    repo: PathBuf,
}

impl TestRepo {
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.repo
    }

    /// The temporary parent that also holds harness data.
    #[must_use]
    pub fn home(&self) -> &Path {
        self.root.path()
    }

    /// A harness data directory placed outside the repository.
    #[must_use]
    pub fn data_dir(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// Run Git inside the fixture repository.
    pub fn git(&self, arguments: &[&str]) -> Output {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(&self.repo)
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    #[must_use]
    pub fn commit_count(&self) -> usize {
        let output = self.git(&["rev-list", "--count", "HEAD"]);
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .expect("commit count")
    }

    #[must_use]
    pub fn status_porcelain(&self) -> String {
        let output = self.git(&["status", "--porcelain=v1", "--untracked-files=all"]);
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.status_porcelain().is_empty()
    }

    #[must_use]
    pub fn head(&self) -> String {
        let output = self.git(&["rev-parse", "HEAD"]);
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// Verify and return the clean input snapshot.
    pub async fn snapshot(&self, manager: &WorkspaceManager) -> VerifiedSnapshot {
        let project = ProjectId::generate();
        match manager
            .inspect_input(&self.repo, &project)
            .await
            .expect("inspect input")
        {
            harness_orchestrator::InputInspection::Clean(snapshot) => snapshot,
            harness_orchestrator::InputInspection::Dirty(reasons) => {
                panic!("fixture repository must start clean, got {reasons:?}");
            }
        }
    }
}

/// Create a clean fixture repository with one commit.
#[must_use]
pub fn test_repo() -> TestRepo {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(root.path().join("repo/src")).expect("mkdir");
    std::fs::write(
        root.path().join("repo/src/lib.rs"),
        "pub fn greet() -> &'static str {\n    \"hi\"\n}\n",
    )
    .expect("write lib");
    std::fs::write(root.path().join("repo/README.md"), "# fixture\n").expect("write readme");
    let repo = TestRepo {
        repo: root.path().join("repo"),
        root,
    };
    repo.git(&["init", "-q"]);
    repo.git(&["config", "user.email", "p5-fixture@localhost"]);
    repo.git(&["config", "user.name", "p5-fixture"]);
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "-qm", "fixture base"]);
    repo
}

/// Open a writable store for a fixture repository.
pub async fn open_store(repo: &TestRepo) -> SqliteStore {
    open_store_with_faults(repo, StoreFaultPlan::default()).await
}

/// Open a writable store with an armed fault plan.
pub async fn open_store_with_faults(repo: &TestRepo, faults: StoreFaultPlan) -> SqliteStore {
    SqliteStore::open_writer(
        WriterOpenOptions::new(repo.data_dir("data"), HostId::generate()).with_fault_plan(faults),
    )
    .await
    .expect("store opens")
}

/// Close a store when this is the last handle. A lingering scheduler or
/// coordinator handle is not a failure: the writer lock frees on last drop.
pub async fn close(store: Arc<SqliteStore>) {
    if let Ok(store) = Arc::try_unwrap(store) {
        store.close().await.expect("store closes");
    }
}

/// Admit one input so a durable recipient session exists before any delivery.
pub async fn coordinator_session(store: &Arc<SqliteStore>) -> SessionId {
    coordinator_root(store).await.0
}

/// Admit the coordinator root input and return both its session and its task.
///
/// A delegated delivery is addressed to the parent task, so a fixture that
/// wants to read a delivery must plan its tasks as children of this task.
pub async fn coordinator_root(store: &Arc<SqliteStore>) -> (SessionId, TaskId) {
    let session_id = SessionId::generate();
    let task_id = TaskId::generate();
    harness_session::SessionService::new(Arc::clone(store))
        .admit_input(harness_session::AdmitInputRequest {
            session_id: session_id.clone(),
            task_id: task_id.clone(),
            input_id: InputId::generate(),
            expected_sequence: 1,
            authority: SourceAuthority::User,
            raw_text: "delegated objective".to_owned(),
            workspace: demo_workspace(),
            initial_plan_items: Vec::new(),
        })
        .await
        .expect("admit coordinator input");
    (session_id, task_id)
}

/// A P2-compatible workspace observation for the durable session row.
#[must_use]
pub fn demo_workspace() -> WorkspaceObservation {
    WorkspaceObservation {
        project_id: ProjectId::generate(),
        worktree_id: "p5-fixture".to_owned(),
        base_commit: "0".repeat(40),
        observed_fingerprint: ContentHash::from_bytes(b"p5-fixture"),
    }
}

/// Build one host-authored task brief.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn make_brief(
    task_id: &TaskId,
    role: AgentRole,
    snapshot: &VerifiedSnapshot,
    agents: u32,
    expected: &[&str],
    write_scope: Vec<String>,
) -> TaskBrief {
    TaskBrief {
        schema_version: DELEGATION_CONTRACT_VERSION,
        task_id: task_id.clone(),
        title: format!("{} delegated task", role.as_str()),
        objective: "complete the delegated objective".to_owned(),
        acceptance_criteria: vec!["the declared artifact is produced".to_owned()],
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
            actions: vec![GrantAction::Read, GrantAction::Publish],
            edit_workspace: role.default_edit_capable() && !write_scope.is_empty(),
            write_scope,
            max_depth: DEFAULT_MAX_DEPTH,
            budget: DelegationBudget {
                max_workers: agents,
                max_model_requests: FIXTURE_REQUESTS,
                max_retries: 1,
                max_cost_units: 0,
            },
        },
        expected_artifacts: expected.iter().map(|value| (*value).to_owned()).collect(),
        deadline_unix_ms: None,
        role,
    }
}

/// Build one task node from a brief.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn make_node(
    task_id: &TaskId,
    parent: Option<&TaskId>,
    role: AgentRole,
    snapshot: &VerifiedSnapshot,
    agents: u32,
    depends_on: &[TaskId],
    expected: &[&str],
    write_scope: Vec<String>,
) -> TaskNode {
    if !role.default_edit_capable() && !write_scope.is_empty() {
        // A non-editing role cannot hold write scope; fixtures must not build
        // an impossible grant accidentally.
        let mut brief = make_brief(task_id, role, snapshot, agents, expected, Vec::new());
        brief.grants.edit_workspace = false;
        return TaskNode {
            schema_version: DELEGATION_CONTRACT_VERSION,
            task_id: task_id.clone(),
            parent_task_id: parent.cloned(),
            role,
            brief,
            depends_on: depends_on.to_vec(),
            status: TaskStatus::Pending,
            depth: 0,
            revision: 1,
        };
    }
    TaskNode {
        schema_version: DELEGATION_CONTRACT_VERSION,
        task_id: task_id.clone(),
        parent_task_id: parent.cloned(),
        role,
        brief: make_brief(task_id, role, snapshot, agents, expected, write_scope),
        depends_on: depends_on.to_vec(),
        status: TaskStatus::Pending,
        depth: 0,
        revision: 1,
    }
}

/// Wrap nodes into a graph owned by a coordinator outside the graph.
#[must_use]
pub fn graph_from(nodes: Vec<TaskNode>) -> TaskGraph {
    TaskGraph {
        schema_version: DELEGATION_CONTRACT_VERSION,
        coordinator: WorkerRef {
            profile_id: AgentProfileId::generate(),
            run_id: AgentRunId::generate(),
            role: AgentRole::Coordinator,
            generation: 1,
        },
        tasks: nodes,
    }
}

/// Compile a node list into a plan. Panics when the fixture is invalid.
#[must_use]
pub fn plan_from(nodes: Vec<TaskNode>) -> TaskPlan {
    TaskPlan::compile(graph_from(nodes)).expect("fixture plan compiles")
}

/// A fresh worker identity at one ownership generation.
#[must_use]
pub fn worker_ref(role: AgentRole, generation: u64) -> WorkerRef {
    WorkerRef {
        profile_id: AgentProfileId::generate(),
        run_id: AgentRunId::generate(),
        role,
        generation,
    }
}

/// Build a dispatch request for one planned task.
#[must_use]
pub fn request_of(plan: &TaskPlan, task_id: &TaskId, _agents: u32) -> WorkerRequest {
    let node = plan.node(task_id).expect("planned task");
    WorkerRequest {
        task_id: task_id.clone(),
        run_id: AgentRunId::generate(),
        generation: 1,
        depth: node.depth,
        brief: node.brief.clone(),
        worktree: None,
        cancellation: harness_providers::CancellationToken::new(),
    }
}

/// Build a coordinator over a store and a scheduler.
#[must_use]
pub fn coordinator(
    store: &Arc<SqliteStore>,
    backend: Arc<dyn WorkerBackend>,
    generation: u64,
) -> DelegationCoordinator {
    let scheduler = Arc::new(
        WorkerScheduler::new(SchedulerConfig::default(), backend, None).expect("scheduler"),
    );
    DelegationCoordinator::new(Arc::clone(store), scheduler, generation)
}

/// Build a coordinator whose delivery recipient is the session's own task.
pub fn coordinator_for(
    store: &Arc<SqliteStore>,
    backend: Arc<dyn WorkerBackend>,
    generation: u64,
    session: &SessionId,
) -> DelegationCoordinator {
    let coordinator = coordinator(store, backend, generation);
    let _ = session;
    coordinator
}

/// Admit a plan and dispatch its tasks in dependency order.
pub async fn admit_and_dispatch(
    coordinator: &DelegationCoordinator,
    plan: &TaskPlan,
    session: &SessionId,
    scheduler: &Arc<WorkerScheduler>,
) {
    coordinator.admit(plan).await.expect("admit plan");
    let mut dispatched: BTreeSet<String> = BTreeSet::new();
    for task_id in &plan.topological_order {
        let node = plan.node(task_id).expect("planned task");
        assert!(
            node.depends_on
                .iter()
                .all(|dependency| dispatched.contains(dependency.as_str())),
            "dependencies must be dispatched first"
        );
        let worker = worker_ref(node.role, 1);
        coordinator
            .claim(task_id, &worker, session)
            .await
            .expect("claim task");
        scheduler
            .dispatch(WorkerRequest {
                task_id: task_id.clone(),
                run_id: worker.run_id,
                generation: 1,
                depth: node.depth,
                brief: node.brief.clone(),
                worktree: None,
                cancellation: harness_providers::CancellationToken::new(),
            })
            .expect("dispatch task");
        dispatched.insert(task_id.as_str().to_owned());
    }
}

/// A report an honest worker would produce for a real revision.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn explorer_report(
    task_id: &TaskId,
    worker: &WorkerRef,
    base_commit: &str,
    artifact: &str,
) -> DelegatedResult {
    let revision = format!("{base_commit}-observed");
    DelegatedResult {
        schema_version: DELEGATION_CONTRACT_VERSION,
        result_id: format!("{}-result", task_id.as_str()),
        task_id: task_id.clone(),
        worker: worker.clone(),
        outcome: DelegatedOutcome::Completed,
        summary: format!("{} completed the delegated objective", worker.role.as_str()),
        artifact_refs: vec![artifact.to_owned()],
        base_revision: base_commit.to_owned(),
        result_revision: revision.clone(),
        checked_revisions: vec![CheckedRevision {
            command: "fixture-check".to_owned(),
            revision: revision.clone(),
            passed: true,
            artifact_id: None,
        }],
        check_receipts: vec![ToolExecutionReceipt {
            schema_version: 1,
            tool_execution_id: ToolExecutionId::generate(),
            task_id: task_id.clone(),
            invocation_id: "fixture-check".to_owned(),
            call_id: None,
            input_hash: ContentHash::from_bytes(revision.as_bytes()),
            policy_revision: 1,
            approval_id: None,
            intent_state: ToolIntentState::IntentRecorded,
            outcome_state: ToolOutcomeState::Settled,
            before_fingerprint: None,
            after_fingerprint: Some(ContentHash::from_bytes(revision.as_bytes())),
            before_hash: None,
            after_hash: None,
            artifact_id: None,
            observed_at_seq: 1,
        }],
        usage: harness_orchestrator::BudgetUsage {
            model_requests: 1,
            retries: 0,
        },
        detail: serde_json::json!({}),
    }
}

/// The deterministic worker backend used by most P5 tests.
pub struct ScriptedWorker {
    mode: Mode,
}

enum Mode {
    Explorer,
    Census(Arc<AtomicU32>),
    Stalled,
}

impl ScriptedWorker {
    #[must_use]
    pub fn explorer() -> Self {
        Self {
            mode: Mode::Explorer,
        }
    }

    /// Count dispatches and report a normal completion.
    #[must_use]
    pub fn census(counter: Arc<AtomicU32>) -> Self {
        Self {
            mode: Mode::Census(counter),
        }
    }

    /// Never complete: used to prove cancellation and drain behaviour.
    #[must_use]
    pub fn stalled() -> Self {
        Self {
            mode: Mode::Stalled,
        }
    }
}

impl WorkerBackend for ScriptedWorker {
    fn dispatch(
        &self,
        request: WorkerRequest,
    ) -> Pin<Box<dyn Future<Output = WorkerOutcome> + Send + '_>> {
        match &self.mode {
            Mode::Stalled => Box::pin(async move {
                // Wait until the host cancels; the outcome is then unknown, not
                // a fabricated completion.
                request.cancellation.cancelled().await;
                WorkerOutcome::OutcomeUnknown {
                    reason: "worker was canceled mid-flight".to_owned(),
                }
            }),
            Mode::Census(counter) => {
                counter.fetch_add(1, Ordering::SeqCst);
                let task_id = request.task_id.clone();
                let base = request.brief.base_commit.clone();
                let expected = request.brief.expected_artifacts.clone();
                let role = request.brief.role;
                Box::pin(async move {
                    let worker = worker_ref(role, request.generation);
                    let artifact = expected.first().cloned().unwrap_or_default();
                    WorkerOutcome::Reported(Box::new(explorer_report(
                        &task_id, &worker, &base, &artifact,
                    )))
                })
            }
            Mode::Explorer => {
                let task_id = request.task_id.clone();
                let base = request.brief.base_commit.clone();
                let expected = request.brief.expected_artifacts.clone();
                let role = request.brief.role;
                let generation = request.generation;
                let run_id = request.run_id.clone();
                Box::pin(async move {
                    let worker = WorkerRef {
                        profile_id: AgentProfileId::generate(),
                        run_id,
                        role,
                        generation,
                    };
                    let artifact = expected.first().cloned().unwrap_or_default();
                    WorkerOutcome::Reported(Box::new(explorer_report(
                        &task_id, &worker, &base, &artifact,
                    )))
                })
            }
        }
    }
}

/// Run the compiled `ha` binary.
#[must_use]
pub fn run_cli(arguments: &[&str]) -> Output {
    let binary = cli_binary();
    Command::new(binary)
        .args(arguments)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("ha binary runs")
}

fn cli_binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("test binary path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    let candidate = path.join(format!("ha{}", std::env::consts::EXE_SUFFIX));
    assert!(
        candidate.is_file(),
        "compiled ha binary missing at {}",
        candidate.display()
    );
    candidate
}

/// Assert one orchestrator result carries an expected error code.
pub fn assert_code(result: Result<(), OrchestratorError>, code: harness_types::ErrorCode) {
    match result {
        Ok(()) => panic!("expected {code:?}, got success"),
        Err(error) => assert_eq!(error.code(), code),
    }
}

/// Build the acceptance registry target path for a phase.
#[must_use]
pub fn phase_target(phase: &str) -> String {
    format!("phase_{}", phase.to_lowercase())
}
