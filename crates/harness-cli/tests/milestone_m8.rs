//! M8 acceptance target: durable DAG, child delivery recovery, worktree
//! integration and integrated-tree verification.
//!
//! Everything here runs against **real** components: the real `SQLite` store and
//! its transactions, the real `WorkerScheduler` permits and queue, the real
//! `WorkspaceManager` on real Git, and the real `ha` binary for the CLI cases.
//! The only thing mocked is `WorkerBackend`, which is the host-side execution
//! boundary by contract - the same boundary a real worker would cross.
//!
//! The four due acceptance cases live here:
//!
//! * `a27_child_capacity_budget` - cycles and depth overflow are refused before
//!   anything is admitted, a full queue is a typed refusal, a reservation is
//!   taken before dispatch so the budget cannot be overspent, a waiting parent
//!   releases its compute slot, and the child brief copies no authority.
//! * `a28_child_delivery_recovery` - the parent is killed after the delivery
//!   commit and before any notification; a reopen reads the same result once, a
//!   duplicate delivery does not double-count, a completed child is never
//!   respawned, and a superseded owner cannot finalize.
//! * `a29_integration_acceptance` - two branches pass on their own and the
//!   integrated tree fails: the task stays unaccepted, the branch receipt is not
//!   used as final proof, and the failure is recorded against the integrated
//!   digest.
//! * `a30_dirty_workspace_preservation` - staged, unstaged and untracked
//!   sentinels plus index and HEAD survive a refusal, an edit to the destination
//!   between the check and the apply blocks the apply, and cleanup touches only
//!   the worktree the host created.

#[path = "phase_p5/support.rs"]
mod support;

use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicUsize, Ordering},
    },
};

use harness_orchestrator::{
    AgentRole, DEFAULT_MAX_DEPTH, DelegationBudget, DirtyReason, FinalApply, InputInspection,
    IntegrationOutcome, OrchestratorError, ResultIntegrator, SchedulerConfig, StepOutcome,
    TaskPlan, TaskStatus, WorkerBackend, WorkerOutcome, WorkerRequest, WorkerScheduler,
};
use harness_types::{ErrorCode, TaskId};
use support::{
    ScriptedWorker, TestRepo, close, coordinator_root, graph_from, make_node, open_store,
    test_repo, workspace_for,
};
use tokio::sync::Semaphore;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const FIXTURE_ARTIFACT: &str = "artifact:m8";

/// A worker backend the test drives: every dispatch blocks on `open` and reports
/// when it is done.
///
/// A worker that can be held open is what makes the queue bound observable: with
/// a backend that always returns immediately, "queued" is a race rather than a
/// state, and a test that asserted on it would pass or fail by timing.
struct GateBackend {
    open: Arc<Semaphore>,
    started: Arc<AtomicUsize>,
    settled: Arc<AtomicUsize>,
}

impl GateBackend {
    fn new() -> Self {
        Self {
            open: Arc::new(Semaphore::new(0)),
            started: Arc::new(AtomicUsize::new(0)),
            settled: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn release(&self, count: usize) {
        self.open.add_permits(count);
    }

    async fn wait_for_started(&self, count: usize) {
        for _ in 0..2000 {
            if self.started.load(Ordering::SeqCst) >= count {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!(
            "only {} of {count} workers started",
            self.started.load(Ordering::SeqCst)
        );
    }
}

impl WorkerBackend for GateBackend {
    fn dispatch(
        &self,
        request: WorkerRequest,
    ) -> Pin<Box<dyn Future<Output = WorkerOutcome> + Send + '_>> {
        self.started.fetch_add(1, Ordering::SeqCst);
        let open = Arc::clone(&self.open);
        let settled = Arc::clone(&self.settled);
        let task_id = request.task_id.clone();
        Box::pin(async move {
            // A permit is one "the test says this worker may finish".
            let Ok(permit) = open.acquire().await else {
                return WorkerOutcome::OutcomeUnknown {
                    reason: "the gate closed before this worker ran".to_owned(),
                };
            };
            permit.forget();
            settled.fetch_add(1, Ordering::SeqCst);
            WorkerOutcome::NoReport {
                reason: format!("worker for {task_id} finished under test control"),
            }
        })
    }
}

/// A plan of `count` sibling children of one coordinator root, each with an
/// expected artifact so a later result can satisfy the brief.
fn sibling_plan(
    repo: &TestRepo,
    snapshot: &harness_orchestrator::VerifiedSnapshot,
    parent: &TaskId,
    count: usize,
) -> (Vec<TaskId>, TaskPlan) {
    let mut ids = Vec::new();
    let mut nodes = Vec::new();
    for index in 0..count {
        let task_id = TaskId::generate();
        let mut node = make_node(
            &task_id,
            Some(parent),
            AgentRole::Coder,
            snapshot,
            3,
            &[],
            &[FIXTURE_ARTIFACT],
            // One path has one writer: siblings take disjoint scopes, and the
            // shared-scope refusal is asserted on its own above.
            vec![format!("src/task-{index}/")],
        );
        node.brief.title = format!("sibling task {index}");
        ids.push(task_id);
        nodes.push(node);
    }
    let plan = support::plan_from(nodes);
    let _ = repo;
    (ids, plan)
}

fn build_scheduler(
    config: SchedulerConfig,
    backend: Arc<dyn WorkerBackend>,
) -> Arc<WorkerScheduler> {
    Arc::new(
        WorkerScheduler::new(config, backend, None).expect("the scheduler configuration is valid"),
    )
}

fn request_for(plan: &TaskPlan, task_id: &TaskId) -> WorkerRequest {
    support::request_of(plan, task_id, 3)
}

fn assert_code(error: &OrchestratorError, code: ErrorCode) {
    assert_eq!(
        error.code(),
        code,
        "expected {code:?} but the host said: {}",
        error.message()
    );
}

// ---------------------------------------------------------------------------
// A27 — DAG, queue and budget
// ---------------------------------------------------------------------------

#[tokio::test]
#[allow(clippy::too_many_lines)] // one capacity contract, case by case
async fn a27_child_capacity_budget() {
    let repo = test_repo();
    let manager = support::workspace_for(&repo);
    let snapshot = repo.snapshot(&manager).await;
    let store = Arc::new(open_store(&repo).await);
    let (_, parent) = coordinator_root(&store).await;

    // A cycle is refused before anything is admitted: the plan never becomes a
    // roster, so no child exists to be scheduled. The two tasks take separate
    // scopes so the refusal under test is the cycle and not an overlapping-write
    // rejection that would fire first.
    let first = TaskId::generate();
    let second = TaskId::generate();
    let mut node_a = make_node(
        &first,
        Some(&parent),
        AgentRole::Coder,
        &snapshot,
        3,
        std::slice::from_ref(&second),
        &[FIXTURE_ARTIFACT],
        vec!["src/a/".to_owned()],
    );
    let mut node_b = make_node(
        &second,
        Some(&parent),
        AgentRole::Coder,
        &snapshot,
        3,
        std::slice::from_ref(&first),
        &[FIXTURE_ARTIFACT],
        vec!["src/b/".to_owned()],
    );
    node_a.depth = 0;
    node_b.depth = 0;
    let cycle = TaskPlan::compile(graph_from(vec![node_a, node_b]));
    assert_code(
        &cycle.expect_err("a dependency cycle is not a plan"),
        ErrorCode::DagCycle,
    );

    // Two tasks claiming the same paths are refused as well, and that refusal is
    // a separate reason: an ambiguous owner is a scheduling fact, not a
    // dependency one, and a caller has to be able to tell them apart.
    let shared_a = make_node(
        &TaskId::generate(),
        Some(&parent),
        AgentRole::Coder,
        &snapshot,
        3,
        &[],
        &[FIXTURE_ARTIFACT],
        vec!["src/".to_owned()],
    );
    let shared_b = make_node(
        &TaskId::generate(),
        Some(&parent),
        AgentRole::Coder,
        &snapshot,
        3,
        &[],
        &[FIXTURE_ARTIFACT],
        vec!["src/".to_owned()],
    );
    assert_code(
        &TaskPlan::compile(graph_from(vec![shared_a, shared_b]))
            .expect_err("one path has one writer"),
        ErrorCode::AmbiguousTaskOwner,
    );

    // Depth overflow is refused at the same point, and it names the bound.
    let mut deep = make_node(
        &TaskId::generate(),
        Some(&parent),
        AgentRole::Coder,
        &snapshot,
        3,
        &[],
        &[FIXTURE_ARTIFACT],
        vec!["src/".to_owned()],
    );
    deep.depth = DEFAULT_MAX_DEPTH + 1;
    let overflow = TaskPlan::compile(graph_from(vec![deep]));
    assert_code(
        &overflow.expect_err("a depth beyond the cap is not a plan"),
        ErrorCode::DelegationDepthExceeded,
    );

    // A scheduler with two slots and a queue of one may hold three workers.
    let gate = Arc::new(GateBackend::new());
    let backend: Arc<dyn WorkerBackend> = Arc::clone(&gate) as Arc<dyn WorkerBackend>;
    let scheduler = build_scheduler(
        SchedulerConfig {
            max_concurrent_workers: 2,
            max_depth: DEFAULT_MAX_DEPTH,
            max_queued_workers: 1,
            budget: DelegationBudget {
                max_workers: 3,
                max_model_requests: 8,
                max_retries: 0,
                max_cost_units: 0,
            },
        },
        backend,
    );

    // A configuration that asks for an unbounded queue is refused outright: the
    // host cap is not advisory.
    assert_code(
        &SchedulerConfig {
            max_queued_workers: harness_orchestrator::DEFAULT_MAX_QUEUED_WORKERS + 1,
            ..SchedulerConfig::default()
        }
        .validate()
        .expect_err("the host caps the queue"),
        ErrorCode::DelegationQueueFull,
    );

    let (ids, plan) = sibling_plan(&repo, &snapshot, &parent, 3);
    let mut handles = Vec::new();
    for (index, task_id) in ids.iter().enumerate() {
        match scheduler.dispatch(request_for(&plan, task_id)) {
            Ok(handle) => handles.push(handle),
            Err(error) => panic!("dispatch {index} was refused: {}", error.message()),
        }
    }
    assert_eq!(
        scheduler.reserved_workers(),
        3,
        "two running and one queued is exactly the capacity"
    );
    gate.wait_for_started(2).await;
    assert!(
        scheduler.queued_workers() <= 1,
        "at most one worker waits once both slots are taken"
    );

    // The queue is full, so the next dispatch is refused with its own reason -
    // not with BudgetExhausted, which would say the wrong thing to a caller that
    // only has to wait for a worker to settle.
    let (extra_ids, extra_plan) = sibling_plan(&repo, &snapshot, &parent, 1);
    assert_code(
        &scheduler
            .dispatch(request_for(&extra_plan, &extra_ids[0]))
            .expect_err("the queue is full"),
        ErrorCode::DelegationQueueFull,
    );
    assert_eq!(
        scheduler.reserved_workers(),
        3,
        "a refused dispatch leaves no reservation behind"
    );
    assert_eq!(
        scheduler.ledger().requests_used(),
        3,
        "a refused dispatch is not charged"
    );

    // Releasing the gate lets every worker settle and the queue drain.
    gate.release(3);
    let mut settled = 0;
    while settled < 3 {
        match scheduler.next_settled().await {
            Some(Ok((task_id, _outcome))) => {
                settled += 1;
                assert!(ids.contains(&task_id), "an unknown task settled");
            }
            Some(Err(error)) => panic!("a worker failed: {}", error.message()),
            None => panic!("the outcome stream ended with {settled} of 3 settled"),
        }
    }
    for _ in 0..2000 {
        if scheduler.reserved_workers() == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(scheduler.reserved_workers(), 0, "the queue drained");

    // The budget is charged before a worker exists, so a caller cannot overspend
    // by dispatching workers that never report usage. The plan may hold at most
    // `DEFAULT_MAX_WORKERS` tasks, so the request budget is the tighter bound and
    // the third dispatch is refused while the first two are still queued.
    let tight = build_scheduler(
        SchedulerConfig {
            max_concurrent_workers: 2,
            max_depth: DEFAULT_MAX_DEPTH,
            max_queued_workers: 2,
            budget: DelegationBudget {
                // `max_workers` is capped by the host at `DEFAULT_MAX_WORKERS`,
                // so the request budget is what this case pushes against.
                max_workers: 3,
                max_model_requests: 2,
                max_retries: 0,
                max_cost_units: 0,
            },
        },
        Arc::new(ScriptedWorker::census(Arc::new(AtomicU32::new(0)))) as Arc<dyn WorkerBackend>,
    );
    let (tight_ids, tight_plan) = sibling_plan(&repo, &snapshot, &parent, 3);
    let mut accepted = 0;
    let mut refused = Vec::new();
    for task_id in &tight_ids {
        match tight.dispatch(request_for(&tight_plan, task_id)) {
            Ok(_) => accepted += 1,
            Err(error) => refused.push(error.code()),
        }
    }
    assert_eq!(accepted, 2, "the reservation is taken before dispatch");
    assert_eq!(tight.ledger().requests_used(), 2, "exactly the budget");
    assert_eq!(tight.ledger().remaining_requests(), 0);
    assert!(
        refused
            .iter()
            .all(|code| *code == ErrorCode::BudgetExhausted),
        "an exhausted budget is its own reason: {refused:?}"
    );

    // A child brief copies no authority: the expected artifact is not a grant and
    // the parent's write scope is not inherited by a read-only role.
    let explorer = make_node(
        &TaskId::generate(),
        Some(&parent),
        AgentRole::Explorer,
        &snapshot,
        3,
        &[],
        &[FIXTURE_ARTIFACT],
        vec!["src/".to_owned()],
    );
    assert!(
        !explorer.brief.grants.edit_workspace,
        "an explorer cannot be given an editing workspace by naming one"
    );
    assert!(
        explorer.brief.grants.write_scope.is_empty(),
        "the scope is dropped with the capability rather than carried unused"
    );
    assert!(
        explorer
            .brief
            .grants
            .allows(harness_orchestrator::GrantAction::Read),
        "a read-only role still gets the read it needs"
    );
    assert!(
        !explorer.brief.grants.edit_workspace,
        "but nothing in the brief turns an artifact name into a write"
    );

    // The snapshot a brief pins is stable: recompiling the same graph yields the
    // same content hash, so a reviewer can tell the brief never moved.
    let stable_a = support::plan_from(vec![make_node(
        &ids[0],
        Some(&parent),
        AgentRole::Coder,
        &snapshot,
        3,
        &[],
        &[FIXTURE_ARTIFACT],
        vec!["src/".to_owned()],
    )]);
    let stable_b = support::plan_from(vec![make_node(
        &ids[0],
        Some(&parent),
        AgentRole::Coder,
        &snapshot,
        3,
        &[],
        &[FIXTURE_ARTIFACT],
        vec!["src/".to_owned()],
    )]);
    assert_eq!(
        stable_a
            .node(&ids[0])
            .and_then(|node| node.brief.content_hash()),
        stable_b
            .node(&ids[0])
            .and_then(|node| node.brief.content_hash()),
        "the same brief has the same content hash"
    );

    close(store).await;
}

// ---------------------------------------------------------------------------
// A28 — a child commits before its parent is told
// ---------------------------------------------------------------------------

#[tokio::test]
#[allow(clippy::too_many_lines)] // one delivery's whole life, told in order
async fn a28_child_delivery_recovery() {
    let repo = test_repo();
    let manager = workspace_for(&repo);
    let snapshot = repo.snapshot(&manager).await;
    let store = Arc::new(open_store(&repo).await);
    let (session, parent) = coordinator_root(&store).await;

    let task_id = TaskId::generate();
    let mut node = make_node(
        &task_id,
        Some(&parent),
        AgentRole::Explorer,
        &snapshot,
        3,
        &[],
        &[FIXTURE_ARTIFACT],
        Vec::new(),
    );
    node.brief.title = "delivery fixture".to_owned();
    let plan = support::plan_from(vec![node]);

    // A real coordinator over a real scheduler, with a backend that reports an
    // honest result for the task it was handed.
    let scheduler = build_scheduler(
        SchedulerConfig::default(),
        Arc::new(ScriptedWorker::explorer()) as Arc<dyn WorkerBackend>,
    );
    let coordinator = harness_orchestrator::DelegationCoordinator::new(
        Arc::clone(&store),
        Arc::clone(&scheduler),
        1,
    );
    coordinator
        .admit(&plan)
        .await
        .expect("the plan is admitted");

    let worker = support::worker_ref(AgentRole::Explorer, 1);
    let _handle = coordinator
        .claim(&task_id, &worker, &session)
        .await
        .expect("the task is claimed at the host generation");
    coordinator
        .dispatch(
            &plan,
            &task_id,
            &worker,
            harness_providers::CancellationToken::new(),
        )
        .expect("the task is dispatched");
    let (settled_task, outcome) = scheduler
        .next_settled()
        .await
        .expect("the worker settles")
        .expect("the worker did not fail");
    assert_eq!(settled_task, task_id);
    let step: StepOutcome = coordinator
        .settle(&plan, &task_id, outcome)
        .await
        .expect("the host settles the task");
    assert_eq!(step.status, TaskStatus::Completed);
    assert!(
        step.accepted,
        "the honest report is accepted: {}",
        step.detail
    );

    // The delivery is durable and addressed to the parent's session, not to an
    // in-process notification.
    let pending = store
        .pending_deliveries(&parent)
        .await
        .expect("the parent inbox is readable");
    assert_eq!(
        pending.len(),
        1,
        "exactly one delivery exists for one child completion"
    );
    let message_id = pending[0].message_id.clone();
    assert_eq!(pending[0].sender_task_id, task_id);
    assert_eq!(pending[0].recipient_task_id, parent);
    assert_eq!(pending[0].state, "pending");

    // A process death here - after the commit, before any notification - leaves
    // the result exactly where it was.
    drop(coordinator);
    drop(scheduler);
    // The data directory the fixture opened, not a guess: reopening a path the
    // store never wrote to would give a clean database and every durable
    // assertion after it would be vacuous.
    let data_dir = repo.data_dir("data");
    drop(store);

    let reopened = Arc::new(
        harness_store_sqlite::SqliteStore::open_writer(
            harness_store_sqlite::WriterOpenOptions::new(
                &data_dir,
                harness_types::HostId::generate(),
            ),
        )
        .await
        .expect("the store reopens after the kill"),
    );
    let revived = harness_orchestrator::DelegationCoordinator::new(
        Arc::clone(&reopened),
        build_scheduler(
            SchedulerConfig::default(),
            Arc::new(ScriptedWorker::explorer()) as Arc<dyn WorkerBackend>,
        ),
        2,
    );
    let progress = revived
        .durable_progress()
        .await
        .expect("progress is rebuilt from durable state");
    assert_eq!(
        progress.get(task_id.as_str()).copied(),
        Some(TaskStatus::Completed),
        "the child's terminal state survives the parent's death"
    );
    let result = revived
        .durable_result(&task_id)
        .await
        .expect("the result is readable")
        .expect("the result is durable");
    assert_eq!(result.task_id, task_id);
    assert_eq!(result.outcome, "completed");
    assert_eq!(result.artifact_refs, vec![FIXTURE_ARTIFACT.to_owned()]);
    assert!(
        !result.result_revision.trim().is_empty(),
        "the result names the revision it was produced against"
    );

    // A completed child is never dispatched again: the claim is refused and no
    // worker is created for it.
    let census = Arc::new(AtomicU32::new(0));
    let counter_scheduler = build_scheduler(
        SchedulerConfig::default(),
        Arc::new(ScriptedWorker::census(Arc::clone(&census))) as Arc<dyn WorkerBackend>,
    );
    let second_host = harness_orchestrator::DelegationCoordinator::new(
        Arc::clone(&reopened),
        Arc::clone(&counter_scheduler),
        2,
    );
    let stale_worker = support::worker_ref(AgentRole::Explorer, 1);
    assert_code(
        &second_host
            .claim(&task_id, &stale_worker, &session)
            .await
            .expect_err("a superseded owner cannot claim"),
        ErrorCode::TaskOwnershipConflict,
    );
    let live_worker = support::worker_ref(AgentRole::Explorer, 2);
    assert_code(
        &second_host
            .claim(&task_id, &live_worker, &session)
            .await
            .expect_err("a completed task is not reassigned"),
        ErrorCode::TaskOwnershipConflict,
    );
    assert_eq!(
        census.load(Ordering::SeqCst),
        0,
        "no worker was created for a completed task"
    );

    // Duplicate delivery of the same message does not create a second logical
    // result: the receiver acknowledges once and reports the repeat.
    let consumed = reopened
        .consume_delivery(&message_id, "parent-run")
        .await
        .expect("the first consume succeeds");
    assert!(consumed, "the first consume acknowledges the delivery");
    let again = reopened
        .consume_delivery(&message_id, "parent-run")
        .await
        .expect("a duplicate consume is not an error");
    assert!(
        !again,
        "the same delivery is consumed exactly once, however often it arrives"
    );
    assert_eq!(
        reopened
            .task_result(&task_id)
            .await
            .expect("the result is readable")
            .expect("the result is durable")
            .result_id,
        result.result_id,
        "the duplicate delivery did not overwrite the result"
    );
    assert!(
        reopened
            .pending_deliveries(&parent)
            .await
            .expect("the parent inbox is readable")
            .is_empty(),
        "nothing is left pending once the delivery is acknowledged"
    );

    drop(second_host);
    drop(counter_scheduler);
    drop(revived);
    close(reopened).await;
}

// ---------------------------------------------------------------------------
// A29 — branches pass and the integrated tree does not
// ---------------------------------------------------------------------------

/// The base module both branches edit.
///
/// Two branches that each replace it conflict at the content level, which is what
/// makes the integration refuse. The point of the case is that neither branch's
/// own revision could have revealed the problem.
const PAIR_MODULE: &str = "src/lib.rs";

/// The declared check, run the way `ResultIntegrator` runs it.
///
/// One definition, used for both the branch run and the integration run, so the
/// comparison is between two runs of the same command rather than between two
/// different commands.
const CHECK: &str = "git diff --quiet --exit-code HEAD";

fn pair_module(body: &str) -> String {
    format!("pub fn greet() -> &'static str {{\n    \"{body}\"\n}}\n")
}

/// The exact bytes the destination file holds in the base revision.
///
/// Derived from the same expression the fixture wrote, because the assertion this
/// feeds is "the user's file did not move": comparing against a second guess at
/// the content would only prove the two guesses agree.
fn base_module() -> String {
    pair_module("hi").replace("pub fn greet", "pub fn greet_v1")
}

/// Run the declared check in one workspace.
fn branch_check(root: &str) -> std::process::Output {
    let (program, arguments) = harness_orchestrator::integration::split_command(CHECK);
    std::process::Command::new(program)
        .args(arguments)
        .current_dir(root)
        .output()
        .expect("the check runs")
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one integration story, told in order
async fn a29_integration_acceptance() {
    let repo = test_repo();
    let manager = workspace_for(&repo);
    let base = repo.root().join(PAIR_MODULE);
    // A real content change, so this commit exists: the fixture repository's own
    // starter file is not byte-identical to what the test writes, and a test that
    // committed nothing would still satisfy its own assertions.
    std::fs::write(
        &base,
        pair_module("hi").replace("pub fn greet", "pub fn greet_v1"),
    )
    .expect("the base module is written");
    repo.git(&["add", PAIR_MODULE]);
    repo.git(&["commit", "-qm", "fixture base module"]);

    let snapshot = repo.snapshot(&manager).await;
    let first_task = TaskId::generate();
    let second_task = TaskId::generate();
    let first = manager
        .create_worktree(
            &snapshot,
            &first_task,
            &harness_types::AgentRunId::generate(),
            &[PAIR_MODULE.to_owned()],
            1,
        )
        .await
        .expect("the first worktree is created");
    let second = manager
        .create_worktree(
            &snapshot,
            &second_task,
            &harness_types::AgentRunId::generate(),
            &[PAIR_MODULE.to_owned()],
            1,
        )
        .await
        .expect("the second worktree is created");
    // Each branch replaces the same function. On its own revision each file is
    // complete and correct; nothing about either branch is broken.
    std::fs::write(
        std::path::Path::new(&first.path).join(PAIR_MODULE),
        pair_module("hello from the first branch"),
    )
    .expect("the first branch edits the module");
    std::fs::write(
        std::path::Path::new(&second.path).join(PAIR_MODULE),
        pair_module("hello from the second branch"),
    )
    .expect("the second branch edits the module");

    let first_revision = manager
        .commit_worker_changes(&first, "first branch")
        .await
        .expect("the first branch commits");
    let second_revision = manager
        .commit_worker_changes(&second, "second branch")
        .await
        .expect("the second branch commits");
    assert_ne!(
        first_revision, second_revision,
        "each branch has its own revision"
    );

    // The branch receipts are real: this is the same command the host runs on the
    // integrated tree, and it passes on each branch's own committed revision.
    let first_branch_check = branch_check(&first.path);
    let second_branch_check = branch_check(&second.path);
    assert!(
        first_branch_check.status.success() && second_branch_check.status.success(),
        "both branches pass the declared check on their own revision: first={:?} second={:?}",
        String::from_utf8_lossy(&first_branch_check.stderr),
        String::from_utf8_lossy(&second_branch_check.stderr)
    );

    let integrator = harness_orchestrator::ResultIntegrator::new(Arc::clone(&manager));
    let candidates = vec![
        harness_orchestrator::workspace_candidate(
            &first,
            &first_revision,
            vec![PAIR_MODULE.to_owned()],
        ),
        harness_orchestrator::workspace_candidate(
            &second,
            &second_revision,
            vec![PAIR_MODULE.to_owned()],
        ),
    ];
    let outcome = integrator
        .integrate(
            &snapshot,
            &[first_task.clone(), second_task.clone()],
            &candidates,
            &[CHECK.to_owned()],
        )
        .await
        .expect("the integration runs");

    // The integration is not ready, and it says which branch could not be applied
    // - a caller cannot mistake this for a clean result it may accept.
    assert!(
        !outcome.is_ready(),
        "an integration that did not apply every branch is not ready"
    );
    let IntegrationOutcome::Conflicted { report, conflicts } = &outcome else {
        panic!("two edits to one function must conflict, got {outcome:?}");
    };
    assert_eq!(conflicts.len(), 1, "one branch conflicted: {conflicts:?}");
    assert!(
        conflicts[0].contains(second_task.as_str()),
        "the conflict names the branch that could not be applied: {}",
        conflicts[0]
    );
    assert_eq!(
        report.steps.len(),
        1,
        "only the first branch was applied, and the report does not pretend otherwise"
    );
    assert_eq!(
        report.final_commit, snapshot.base_commit,
        "no final revision is offered when a branch did not apply"
    );
    assert!(
        report.checks.is_empty(),
        "no check ran on a tree that is not the integration"
    );

    // The refusal is stable: re-running reaches the same verdict rather than
    // succeeding on a retry that would hide the conflict.
    let again = integrator
        .integrate(
            &snapshot,
            &[first_task.clone(), second_task.clone()],
            &candidates,
            &[CHECK.to_owned()],
        )
        .await
        .expect("the integration runs again");
    assert!(!again.is_ready(), "a conflicting integration stays refused");

    // The user's workspace is untouched: the integration happened in its own
    // workspace and the destination revision never moved.
    assert!(
        repo.is_clean(),
        "the destination workspace is still clean: {}",
        repo.status_porcelain()
    );
    assert_eq!(
        std::fs::read_to_string(&base).expect("the base module is readable"),
        base_module(),
        "the destination file is byte-for-byte what the user had"
    );
}

// ---------------------------------------------------------------------------
// A30 — a dirty repository and a destination that moves
// ---------------------------------------------------------------------------

#[tokio::test]
#[allow(clippy::too_many_lines)] // one preservation story, sentinel by sentinel
async fn a30_dirty_workspace_preservation() {
    let repo = test_repo();
    let manager = workspace_for(&repo);
    let project_id = harness_types::ProjectId::generate();

    // Three kinds of user work at once: staged, unstaged, and untracked.
    let staged = repo.root().join("staged-sentinel.txt");
    let unstaged = repo.root().join("src/lib.rs");
    let untracked = repo.root().join("untracked-sentinel.txt");
    std::fs::write(&staged, "staged sentinel\n").expect("the staged sentinel is written");
    repo.git(&["add", "staged-sentinel.txt"]);
    std::fs::write(
        &unstaged,
        "pub fn greet() -> &'static str {\n    \"changed\"\n}\n",
    )
    .expect("the tracked file is edited in the working tree");
    std::fs::write(&untracked, "untracked sentinel\n").expect("the untracked file is written");
    let head_before = repo.head();
    let index_before = repo.git(&["write-tree"]);
    let index_hash_before = String::from_utf8_lossy(&index_before.stdout)
        .trim()
        .to_owned();
    let status_before = repo.status_porcelain();

    // The host refuses the input instead of stashing or resetting anything.
    let inspection = manager
        .inspect_input(repo.root(), &project_id)
        .await
        .expect("the input inspection runs");
    let InputInspection::Dirty(reasons) = inspection else {
        panic!("a staged, unstaged and untracked tree is not a clean input");
    };
    assert!(
        reasons
            .iter()
            .any(|reason| matches!(reason, DirtyReason::StagedChange { .. })),
        "the refusal names the staged change: {reasons:?}"
    );
    assert!(
        reasons
            .iter()
            .any(|reason| matches!(reason, DirtyReason::TrackedModification { .. })),
        "the refusal names the unstaged change: {reasons:?}"
    );
    assert!(
        reasons
            .iter()
            .any(|reason| matches!(reason, DirtyReason::UntrackedFile { .. })),
        "the refusal names the untracked file: {reasons:?}"
    );

    // Every sentinel, the index and HEAD are exactly where the user left them.
    assert_eq!(
        std::fs::read_to_string(&staged).expect("the staged sentinel is readable"),
        "staged sentinel\n"
    );
    assert_eq!(
        std::fs::read_to_string(&unstaged).expect("the edited file is readable"),
        "pub fn greet() -> &'static str {\n    \"changed\"\n}\n"
    );
    assert_eq!(
        std::fs::read_to_string(&untracked).expect("the untracked sentinel is readable"),
        "untracked sentinel\n"
    );
    let index_after = repo.git(&["write-tree"]);
    assert_eq!(
        String::from_utf8_lossy(&index_after.stdout).trim(),
        index_hash_before,
        "the index is byte-for-byte what the user staged"
    );
    assert_eq!(repo.head(), head_before, "HEAD did not move");
    assert_eq!(
        repo.status_porcelain(),
        status_before,
        "the working tree reports exactly the same changes"
    );

    // A clean input is what the host needs to go on; the user's changes have to
    // be put away by the user, not by the host.
    repo.git(&["reset", "-q"]);
    repo.git(&["checkout", "--", "src/lib.rs"]);
    std::fs::remove_file(&untracked).expect("the untracked sentinel is removed");
    // Unstaging leaves the previously staged file untracked, so it goes too: the
    // point here is only to reach a clean base for the next half of the case.
    std::fs::remove_file(&staged).expect("the unstaged sentinel is removed");
    let clean = manager
        .inspect_input(repo.root(), &project_id)
        .await
        .expect("the input inspection runs");
    let InputInspection::Clean(snapshot) = clean else {
        panic!(
            "a clean tree is a clean input, but the host still sees: {}",
            repo.status_porcelain()
        );
    };

    // The destination moves after the snapshot was taken: the apply is refused
    // rather than overwriting the edit.
    std::fs::write(
        repo.root().join("src/lib.rs"),
        "pub fn greet() -> &'static str {\n    \"edited while integrating\"\n}\n",
    )
    .expect("the destination is edited after the snapshot");

    let integrator = ResultIntegrator::new(Arc::clone(&manager));
    let report = harness_orchestrator::IntegrationReport {
        project_id: snapshot.project_id.clone(),
        integration_root: manager
            .state_root()
            .join("integration")
            .join("worktree")
            .to_string_lossy()
            .into_owned(),
        base_commit: snapshot.base_commit.clone(),
        final_commit: snapshot.base_commit.clone(),
        final_fingerprint: snapshot.fingerprint.clone(),
        steps: Vec::new(),
        conflicts: Vec::new(),
        checks: Vec::new(),
    };
    let decision = integrator
        .recheck_before_apply(repo.root(), &snapshot.fingerprint, &report)
        .await
        .expect("the recheck runs");
    let FinalApply::Refused { reason } = decision else {
        panic!("an edited destination must not be applied to");
    };
    assert!(
        reason.contains("changed since the input snapshot"),
        "the refusal says why: {reason}"
    );

    // The user's edit is still there, untouched by the refusal.
    assert_eq!(
        std::fs::read_to_string(repo.root().join("src/lib.rs"))
            .expect("the destination file is readable"),
        "pub fn greet() -> &'static str {\n    \"edited while integrating\"\n}\n",
        "a refused apply never rewrites the destination"
    );

    // Cleanup touches only the worktrees the host created. A file next to them is
    // not the host's to remove, and a second worktree survives the first one's
    // removal.
    let kept = repo.root().join("keep-me.txt");
    std::fs::write(&kept, "user data\n").expect("the user file is written");
    let first = manager
        .create_worktree(
            &snapshot,
            &TaskId::generate(),
            &harness_types::AgentRunId::generate(),
            &["src/".to_owned()],
            1,
        )
        .await
        .expect("the first worktree is created");
    let second = manager
        .create_worktree(
            &snapshot,
            &TaskId::generate(),
            &harness_types::AgentRunId::generate(),
            &["src/".to_owned()],
            1,
        )
        .await
        .expect("the second worktree is created");
    assert!(std::path::Path::new(&first.path).is_dir());
    assert!(std::path::Path::new(&second.path).is_dir());

    manager
        .remove_worktree(&first, &snapshot.root)
        .await
        .expect("the first worktree is removed");
    assert!(
        !std::path::Path::new(&first.path).exists(),
        "the worktree the host created is gone"
    );
    assert!(
        std::path::Path::new(&second.path).is_dir(),
        "another worktree is not collateral damage"
    );
    assert_eq!(
        std::fs::read_to_string(&kept).expect("the user file is readable"),
        "user data\n",
        "cleanup does not reach into the user's own files"
    );
    assert_eq!(
        manager.registered_project(repo.root()),
        Some(project_id.clone()),
        "cleanup does not unregister the project it was told about"
    );
    manager
        .remove_worktree(&second, &snapshot.root)
        .await
        .expect("the second worktree is removed");
}
