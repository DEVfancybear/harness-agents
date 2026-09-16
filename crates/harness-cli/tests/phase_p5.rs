//! P5 acceptance: delegation, durable task DAG, isolated workspaces, integration
//! and delegated recovery, exercised through real components.
//!
//! Only external boundaries are replaced: a scripted worker backend stands in
//! for the model, and `Git` repositories are created in temporary directories.
//! The transaction coordinator, `SQLite` store, DAG, scheduler, worktree manager,
//! integrator and the compiled `ha` binary are all real.

#[path = "phase_p5/support.rs"]
mod support;

#[path = "phase_p5/strengthening.rs"]
mod strengthening;

use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use harness_orchestrator::{
    AgentRole, DEFAULT_MAX_DEPTH, DEFAULT_MAX_WORKERS, DELEGATION_CONTRACT_VERSION,
    DelegationBudget, DelegationCoordinator, IntegrationOutcome, SchedulerConfig, TaskPlan,
    TaskStatus, WorkerOutcome, WorkerRequest, WorkerScheduler, WorkspaceManager,
};
use harness_providers::CancellationToken;
use harness_store_sqlite::{SqliteStore, StoreFaultPlan, StoreFaultPoint};
use harness_types::{AgentRunId, ErrorCode, ProjectId, SessionId, TaskId};
use serde_json::json;

use support::{
    FIXTURE_REQUESTS, ScriptedWorker, TestRepo, graph_from, make_brief, make_node, open_store,
    plan_from, request_of, test_repo,
};

fn workspace_for(root: &TestRepo) -> Arc<WorkspaceManager> {
    Arc::new(WorkspaceManager::new(root.data_dir("delegation")))
}

/// One node plan whose parent is the coordinator root task, so deliveries are
/// addressed to the durable coordinator session.
fn single_task_plan(
    parent: &TaskId,
    role: AgentRole,
    snapshot: &harness_orchestrator::VerifiedSnapshot,
    agents: u32,
    expected: &[&str],
    scope: Vec<String>,
) -> (TaskId, TaskPlan) {
    let task_id = TaskId::generate();
    let node = make_node(
        &task_id,
        Some(parent),
        role,
        snapshot,
        agents,
        &[],
        expected,
        scope,
    );
    let plan = plan_from(vec![node]);
    (task_id, plan)
}

// ---------------------------------------------------------------------------
// P5-S01
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p5_s01_delegation_contracts_roles_and_dag_validation_are_versioned() {
    let repo = test_repo();
    let manager = workspace_for(&repo);
    let snapshot = repo.snapshot(&manager).await;

    // A role name grants nothing: a verifier cannot ask for an editing workspace.
    let mut node = make_node(
        &TaskId::generate(),
        None,
        AgentRole::Verifier,
        &snapshot,
        3,
        &[],
        &["check:x"],
        vec!["src/lib.rs".to_owned()],
    );
    node.brief.grants.edit_workspace = true;
    let error = TaskPlan::compile(graph_from(vec![node])).unwrap_err();
    assert_eq!(error.code(), ErrorCode::ScopeAuthorityDenied);

    // A grant can never widen the parent grant.
    let parent = make_node(
        &TaskId::generate(),
        None,
        AgentRole::Coder,
        &snapshot,
        3,
        &[],
        &["edit:x"],
        vec!["src/lib.rs".to_owned()],
    );
    let mut child = make_brief(
        &TaskId::generate(),
        AgentRole::Coder,
        &snapshot,
        3,
        &["edit:x"],
        vec!["src/other.rs".to_owned()],
    );
    child.grants.max_depth = parent.brief.grants.max_depth;
    let error = parent.brief.grants.intersect(&child.grants).unwrap_err();
    assert_eq!(error.code(), ErrorCode::ScopeAuthorityDenied);

    // A narrower but legal child grant is accepted: same depth allowance,
    // a read-only action set and a write scope inside the parent scope.
    let mut allowed = make_brief(
        &TaskId::generate(),
        AgentRole::Coder,
        &snapshot,
        3,
        &["edit:x"],
        vec!["src/lib.rs".to_owned()],
    );
    allowed.grants.actions = vec![harness_orchestrator::GrantAction::Read];
    allowed.grants.max_depth = parent.brief.grants.max_depth;
    allowed.task_id = parent.task_id.clone();
    allowed.grants.task_id = parent.task_id.clone();
    allowed.grants.project_id = parent.brief.grants.project_id.clone();
    let narrowed = parent
        .brief
        .grants
        .intersect(&allowed.grants)
        .expect("narrower grant");
    assert_eq!(
        narrowed.actions,
        vec![harness_orchestrator::GrantAction::Read]
    );
    assert_eq!(narrowed.max_depth, parent.brief.grants.max_depth);

    // The contract carries an explicit serialization revision.
    let node = make_node(
        &TaskId::generate(),
        None,
        AgentRole::Explorer,
        &snapshot,
        3,
        &[],
        &["observe:x"],
        Vec::new(),
    );
    assert_eq!(node.schema_version, DELEGATION_CONTRACT_VERSION);
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One admission gate; splitting hides ordering.
async fn p5_dag_cycle_and_dependency_failure_are_typed() {
    let repo = test_repo();
    let manager = workspace_for(&repo);
    let snapshot = repo.snapshot(&manager).await;

    // A two-node cycle is rejected before any admission.
    let a = TaskId::generate();
    let b = TaskId::generate();
    let node_a = make_node(
        &a,
        None,
        AgentRole::Explorer,
        &snapshot,
        3,
        std::slice::from_ref(&b),
        &["observe:x"],
        Vec::new(),
    );
    let node_b = make_node(
        &b,
        None,
        AgentRole::Explorer,
        &snapshot,
        3,
        std::slice::from_ref(&a),
        &["observe:y"],
        Vec::new(),
    );
    let error = TaskPlan::compile(graph_from(vec![node_a, node_b])).unwrap_err();
    assert_eq!(error.code(), ErrorCode::DagCycle);

    // A dependency on a task that is not in the graph is rejected.
    let node = make_node(
        &TaskId::generate(),
        None,
        AgentRole::Explorer,
        &snapshot,
        3,
        &[TaskId::generate()],
        &["observe:x"],
        Vec::new(),
    );
    let error = TaskPlan::compile(graph_from(vec![node])).unwrap_err();
    assert_eq!(error.code(), ErrorCode::UnknownTaskDependency);

    // Self dependency is a cycle.
    let self_id = TaskId::generate();
    let node = make_node(
        &self_id,
        None,
        AgentRole::Explorer,
        &snapshot,
        3,
        std::slice::from_ref(&self_id),
        &["observe:x"],
        Vec::new(),
    );
    let error = TaskPlan::compile(graph_from(vec![node])).unwrap_err();
    assert_eq!(error.code(), ErrorCode::DagCycle);

    // Depth beyond the P5 cap is refused rather than clamped.
    let mut deep = make_node(
        &TaskId::generate(),
        None,
        AgentRole::Explorer,
        &snapshot,
        3,
        &[],
        &["observe:x"],
        Vec::new(),
    );
    deep.depth = DEFAULT_MAX_DEPTH + 1;
    let error = TaskPlan::compile(graph_from(vec![deep])).unwrap_err();
    assert_eq!(error.code(), ErrorCode::DelegationDepthExceeded);

    // Two tasks claiming one write scope have no decidable owner.
    let shared = vec!["src/lib.rs".to_owned()];
    let first = make_node(
        &TaskId::generate(),
        None,
        AgentRole::Coder,
        &snapshot,
        3,
        &[],
        &["edit:x"],
        shared.clone(),
    );
    let second = make_node(
        &TaskId::generate(),
        None,
        AgentRole::Coder,
        &snapshot,
        3,
        &[],
        &["edit:y"],
        shared,
    );
    let error = TaskPlan::compile(graph_from(vec![first, second])).unwrap_err();
    assert_eq!(error.code(), ErrorCode::AmbiguousTaskOwner);

    // A graph larger than the worker budget is refused.
    let nodes = (0..=DEFAULT_MAX_WORKERS)
        .map(|_| {
            make_node(
                &TaskId::generate(),
                None,
                AgentRole::Explorer,
                &snapshot,
                3,
                &[],
                &["observe:x"],
                Vec::new(),
            )
        })
        .collect::<Vec<_>>();
    let error = TaskPlan::compile(graph_from(nodes)).unwrap_err();
    assert_eq!(error.code(), ErrorCode::BudgetExhausted);
}

// ---------------------------------------------------------------------------
// P5-S02 / C11 / C25
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p5_s02_task_transitions_and_delivery_commit_atomically() {
    let repo = test_repo();
    let manager = workspace_for(&repo);
    let snapshot = repo.snapshot(&manager).await;
    let store = Arc::new(open_store(&repo).await);
    let (session, root) = support::coordinator_root(&store).await;

    let (task_id, plan) = single_task_plan(
        &root,
        AgentRole::Explorer,
        &snapshot,
        2,
        &["observe:x"],
        Vec::new(),
    );
    let coordinator = support::coordinator(&store, Arc::new(ScriptedWorker::explorer()), 1);
    coordinator.admit(&plan).await.expect("admit");
    let worker = support::worker_ref(AgentRole::Explorer, 1);
    coordinator
        .claim(&task_id, &worker, &session)
        .await
        .expect("claim");

    // A claim is durable and single-owner.
    let owner = store.task_owner(&task_id).await.unwrap().expect("owner");
    assert_eq!(owner.owner_run_id, worker.run_id);
    assert_eq!(owner.generation, 1);

    // The transition, the result and the parent message commit together.
    let report = support::explorer_report(&task_id, &worker, &snapshot.base_commit, "observe:x");
    let step = coordinator
        .settle(&plan, &task_id, WorkerOutcome::Reported(Box::new(report)))
        .await
        .expect("settle");
    assert!(step.accepted, "{}", step.detail);

    let node = store.task_node(&task_id).await.unwrap().expect("node");
    assert_eq!(node.status, TaskStatus::Completed.as_str());
    let result = store.task_result(&task_id).await.unwrap().expect("result");
    assert_eq!(result.outcome, "completed");
    assert!(result.report_json.is_object());

    // Delivery is exactly once logically: the first consume wins, a replay is a
    // no-op that still references the original result.
    let recipient = session_task(&store, &session).await;
    let deliveries = store.pending_deliveries(&recipient).await.unwrap();
    assert_eq!(deliveries.len(), 1);
    let message_id = deliveries[0].message_id.clone();
    assert_eq!(
        deliveries[0].result_id.as_deref(),
        Some(result.result_id.as_str())
    );
    assert!(store.consume_delivery(&message_id, "parent").await.unwrap());
    assert!(!store.consume_delivery(&message_id, "parent").await.unwrap());
    let replayed = store
        .delivery(&message_id)
        .await
        .unwrap()
        .expect("delivery");
    assert_eq!(replayed.state, "consumed");
    assert_eq!(replayed.consumed_by.as_deref(), Some("parent"));

    // A completed task is never claimed again.
    let error = coordinator
        .claim(&task_id, &worker, &session)
        .await
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::TaskOwnershipConflict);

    support::close(store).await;
}

async fn session_task(store: &Arc<SqliteStore>, session: &SessionId) -> TaskId {
    store
        .session_task(session)
        .await
        .expect("session lookup")
        .expect("session task")
}

#[tokio::test]
async fn p5_c11_completed_child_result_survives_parent_restart() {
    let repo = test_repo();
    let manager = workspace_for(&repo);
    let snapshot = repo.snapshot(&manager).await;
    let session = {
        let store = Arc::new(open_store(&repo).await);
        let (session, root) = support::coordinator_root(&store).await;
        let (task_id, plan) = single_task_plan(
            &root,
            AgentRole::Explorer,
            &snapshot,
            2,
            &["observe:x"],
            Vec::new(),
        );
        let coordinator = support::coordinator(&store, Arc::new(ScriptedWorker::explorer()), 1);
        coordinator.admit(&plan).await.expect("admit");
        let worker = support::worker_ref(AgentRole::Explorer, 1);
        coordinator
            .claim(&task_id, &worker, &session)
            .await
            .expect("claim");
        let report =
            support::explorer_report(&task_id, &worker, &snapshot.base_commit, "observe:x");
        let step = coordinator
            .settle(&plan, &task_id, WorkerOutcome::Reported(Box::new(report)))
            .await
            .expect("settle");
        assert!(step.accepted, "{}", step.detail);
        // The parent stops here: the child result is committed but the parent
        // never consumed the notification.
        support::close(store).await;
        session
    };

    // A recovered host reads the durable result without re-running the child.
    let store = Arc::new(
        SqliteStore::open_read_only(repo.data_dir("data"))
            .await
            .expect("read-only store"),
    );
    let nodes = store.list_task_nodes().await.unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].status, TaskStatus::Completed.as_str());
    let coordinator = DelegationCoordinator::new(
        Arc::new(open_store(&repo).await),
        Arc::new(
            WorkerScheduler::new(
                SchedulerConfig::default(),
                Arc::new(ScriptedWorker::explorer()),
                None,
            )
            .unwrap(),
        ),
        1,
    );
    let completed = coordinator.completed_tasks().await.unwrap();
    assert_eq!(completed.len(), 1, "completed work must not be reassigned");
    let progress = coordinator.durable_progress().await.unwrap();
    assert_eq!(
        progress.get(nodes[0].task_id.as_str()),
        Some(&TaskStatus::Completed)
    );
    let recipient = session_task(&store, &session).await;
    let deliveries = store.pending_deliveries(&recipient).await.unwrap();
    assert_eq!(deliveries.len(), 1, "the delivery is still durable");
    assert_eq!(store.delegation_task_count().await.unwrap(), 1);
}

#[tokio::test]
async fn p5_c25_lost_parent_notification_replays_exactly_once() {
    let repo = test_repo();
    let manager = workspace_for(&repo);
    let snapshot = repo.snapshot(&manager).await;
    // Commit the child result with a fault injected before the delivery commit.
    // The task transition, the result and the message are one transaction, so a
    // lost notification cannot leave a half-committed delivery.
    let faulted = Arc::new(
        support::open_store_with_faults(
            &repo,
            StoreFaultPlan::with_point(StoreFaultPoint::BeforeDelegationDeliveryCommit),
        )
        .await,
    );
    let (session, root) = support::coordinator_root(&faulted).await;
    let (task_id, plan) = single_task_plan(
        &root,
        AgentRole::Explorer,
        &snapshot,
        2,
        &["observe:x"],
        Vec::new(),
    );
    let coordinator = support::coordinator(&faulted, Arc::new(ScriptedWorker::explorer()), 1);
    coordinator.admit(&plan).await.expect("admit");
    let worker = support::worker_ref(AgentRole::Explorer, 1);
    coordinator
        .claim(&task_id, &worker, &session)
        .await
        .expect("claim");
    let report = support::explorer_report(&task_id, &worker, &snapshot.base_commit, "observe:x");
    let error = coordinator
        .settle(&plan, &task_id, WorkerOutcome::Reported(Box::new(report)))
        .await
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::StorageWriteFailed);
    // The injected failure rolled the whole transaction back.
    assert!(faulted.task_result(&task_id).await.unwrap().is_none());
    assert_eq!(
        faulted.task_node(&task_id).await.unwrap().unwrap().status,
        TaskStatus::Ready.as_str()
    );

    // Retrying the same report on a healthy store settles it exactly once.
    let report = support::explorer_report(&task_id, &worker, &snapshot.base_commit, "observe:x");
    let step = coordinator
        .settle(&plan, &task_id, WorkerOutcome::Reported(Box::new(report)))
        .await
        .expect("settle after recovery");
    assert!(step.accepted, "{}", step.detail);
    let recipient = session_task(&faulted, &session).await;
    let deliveries = faulted.pending_deliveries(&recipient).await.unwrap();
    assert_eq!(deliveries.len(), 1);
    let message_id = deliveries[0].message_id.clone();
    let original_result = deliveries[0].result_id.clone();
    assert!(
        faulted
            .consume_delivery(&message_id, "parent")
            .await
            .unwrap()
    );
    assert!(
        !faulted
            .consume_delivery(&message_id, "parent")
            .await
            .unwrap()
    );
    let replayed = faulted.delivery(&message_id).await.unwrap().unwrap();
    assert_eq!(replayed.result_id, original_result);
    assert_eq!(faulted.delegation_task_count().await.unwrap(), 1);
    support::close(faulted).await;
}

#[tokio::test]
async fn p5_s06_recovery_rebuilds_dag_progress_from_durable_state() {
    let repo = test_repo();
    let manager = workspace_for(&repo);
    let snapshot = repo.snapshot(&manager).await;
    let session;
    let explorer;
    let coder;
    {
        let store = Arc::new(open_store(&repo).await);
        let (session_id, root) = support::coordinator_root(&store).await;
        session = session_id;
        explorer = TaskId::generate();
        coder = TaskId::generate();
        let plan = plan_from(vec![
            make_node(
                &explorer,
                Some(&root),
                AgentRole::Explorer,
                &snapshot,
                2,
                &[],
                &["observe:x"],
                Vec::new(),
            ),
            make_node(
                &coder,
                Some(&root),
                AgentRole::Coder,
                &snapshot,
                2,
                std::slice::from_ref(&explorer),
                &["edit:src/lib.rs"],
                vec!["src/lib.rs".to_owned()],
            ),
        ]);
        let coordinator = support::coordinator(&store, Arc::new(ScriptedWorker::explorer()), 1);
        coordinator.admit(&plan).await.expect("admit");
        let worker = support::worker_ref(AgentRole::Explorer, 1);
        coordinator
            .claim(&explorer, &worker, &session)
            .await
            .expect("claim");
        let report =
            support::explorer_report(&explorer, &worker, &snapshot.base_commit, "observe:x");
        let step = coordinator
            .settle(&plan, &explorer, WorkerOutcome::Reported(Box::new(report)))
            .await
            .expect("settle");
        assert!(step.accepted, "{}", step.detail);
        // The coder never ran: the host stops with it still pending.
        support::close(store).await;
    }

    let store = Arc::new(
        SqliteStore::open_read_only(repo.data_dir("data"))
            .await
            .expect("read-only store"),
    );
    let nodes = store.list_task_nodes().await.unwrap();
    assert_eq!(nodes.len(), 2);
    let by_id = nodes
        .iter()
        .map(|node| (node.task_id.clone(), node.status.clone()))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(
        by_id.get(&explorer).map(String::as_str),
        Some(TaskStatus::Completed.as_str())
    );
    assert_eq!(
        by_id.get(&coder).map(String::as_str),
        Some(TaskStatus::Pending.as_str()),
        "unfinished work stays visible for the resumed coordinator"
    );
    assert!(
        store.task_result(&coder).await.unwrap().is_none(),
        "unfinished work has no fabricated result"
    );
    assert!(
        store.task_result(&explorer).await.unwrap().is_some(),
        "the finished result is durable"
    );
    let dependencies = store.task_dependencies(&coder).await.unwrap();
    assert_eq!(dependencies, vec![explorer.clone()]);
}

// ---------------------------------------------------------------------------
// P5-S03 / fairness / shutdown
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p5_s03_scheduler_bounds_slots_depth_and_budget() {
    let repo = test_repo();
    let manager = workspace_for(&repo);
    let snapshot = repo.snapshot(&manager).await;
    let store = Arc::new(open_store(&repo).await);
    let census = Arc::new(AtomicU32::new(0));
    let scheduler = Arc::new(
        WorkerScheduler::new(
            SchedulerConfig {
                max_concurrent_workers: 2,
                max_depth: 1,
                budget: DelegationBudget {
                    max_workers: 2,
                    max_model_requests: 3,
                    max_retries: 0,
                    max_cost_units: 0,
                },
            },
            Arc::new(ScriptedWorker::census(Arc::clone(&census))),
            None,
        )
        .expect("scheduler"),
    );

    // Depth beyond the configured cap is refused before a worker exists.
    let (_, plan) = single_task_plan(
        &TaskId::generate(),
        AgentRole::Explorer,
        &snapshot,
        2,
        &["observe:x"],
        Vec::new(),
    );
    let task_id = plan.topological_order[0].clone();
    let mut deep_request = request_of(&plan, &task_id, 4);
    deep_request.depth = 2;
    let error = scheduler.dispatch(deep_request).unwrap_err();
    assert_eq!(error.code(), ErrorCode::DelegationDepthExceeded);
    assert_eq!(census.load(Ordering::SeqCst), 0);
    assert_eq!(scheduler.available_slots(), 2);

    // Dispatch up to the budget and confirm requests are charged before the
    // worker runs, so an unreported usage is never treated as zero.
    let mut handles = Vec::new();
    for index in 0..3 {
        let request = WorkerRequest {
            task_id: TaskId::generate(),
            run_id: AgentRunId::generate(),
            generation: 1,
            depth: 1,
            brief: plan.node(&task_id).unwrap().brief.clone(),
            worktree: None,
            cancellation: CancellationToken::new(),
        };
        match scheduler.dispatch(request) {
            Ok(handle) => handles.push(handle),
            Err(error) => {
                assert_eq!(error.code(), ErrorCode::BudgetExhausted);
                assert_eq!(index, 2, "budget must not fail before the limit");
            }
        }
    }
    assert_eq!(scheduler.ledger().requests_used(), 3);
    assert_eq!(scheduler.ledger().remaining_requests(), 0);

    // Simulation config validation refuses out-of-range values outright.
    assert!(
        SchedulerConfig {
            max_concurrent_workers: DEFAULT_MAX_WORKERS + 1,
            ..SchedulerConfig::default()
        }
        .validate()
        .is_err()
    );
    assert!(scheduler.live_worker_count() <= 2);
    let drained = scheduler.drain_descendants().await;
    assert!(drained.len() <= 3);
    support::close(store).await;
}

#[tokio::test]
async fn p5_s03_parent_wait_releases_slots_and_shutdown_drains_descendants() {
    let repo = test_repo();
    let manager = workspace_for(&repo);
    let snapshot = repo.snapshot(&manager).await;
    let store = Arc::new(open_store(&repo).await);
    let census = Arc::new(AtomicU32::new(0));
    let scheduler = Arc::new(
        WorkerScheduler::new(
            SchedulerConfig {
                max_concurrent_workers: 1,
                max_depth: 2,
                budget: DelegationBudget {
                    max_workers: 1,
                    max_model_requests: FIXTURE_REQUESTS,
                    max_retries: 0,
                    max_cost_units: 0,
                },
            },
            Arc::new(ScriptedWorker::census(Arc::clone(&census))),
            None,
        )
        .expect("scheduler"),
    );
    let (_, plan) = single_task_plan(
        &TaskId::generate(),
        AgentRole::Explorer,
        &snapshot,
        1,
        &["observe:x"],
        Vec::new(),
    );
    let task_id = plan.topological_order[0].clone();

    // The coordinator holds the only slot, then waits for its child.
    let mut lease = scheduler.acquire_lease().await.expect("lease");
    assert_eq!(scheduler.available_slots(), 0);
    assert!(lease.is_held());

    // The child is dispatched while the parent still holds the slot. It cannot
    // run until the parent releases, which is exactly the fairness property.
    let request = request_of(&plan, &task_id, 1);
    scheduler.dispatch(request).expect("dispatch child");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        census.load(Ordering::SeqCst),
        0,
        "a child must not start while the only slot is held"
    );

    // Waiting releases the slot, so the child can run and settle.
    let settled = scheduler
        .await_next_releasing(&mut lease)
        .await
        .expect("settled");
    assert!(
        !lease.is_held(),
        "waiting on a child releases the compute slot"
    );
    let (settled_task, outcome) = settled.expect("child settled");
    assert_eq!(settled_task, task_id);
    assert!(matches!(outcome, WorkerOutcome::Reported(_)));
    assert_eq!(census.load(Ordering::SeqCst), 1);

    // Shutdown drains descendants and reports the ones with no established
    // outcome instead of claiming a silent clean shutdown.
    let slow_scheduler = Arc::new(
        WorkerScheduler::new(
            SchedulerConfig {
                max_concurrent_workers: 1,
                max_depth: 2,
                budget: DelegationBudget {
                    max_workers: 1,
                    max_model_requests: FIXTURE_REQUESTS,
                    max_retries: 0,
                    max_cost_units: 0,
                },
            },
            Arc::new(ScriptedWorker::stalled()),
            None,
        )
        .expect("scheduler"),
    );
    let request = request_of(&plan, &task_id, 1);
    slow_scheduler.dispatch(request).expect("dispatch stalled");
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let drained = slow_scheduler.drain_descendants().await;
    assert_eq!(drained.len(), 1, "the canceled worker reports uncertainty");
    assert!(
        drained[0].contains("canceled"),
        "an under-established outcome must be reported: {drained:?}"
    );
    assert_eq!(slow_scheduler.live_worker_count(), 0);
    support::close(store).await;
}

// ---------------------------------------------------------------------------
// P5-S04
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p5_s04_clean_worktrees_isolate_workers_and_block_out_of_scope_writes() {
    let repo = test_repo();
    let manager = workspace_for(&repo);
    let snapshot = repo.snapshot(&manager).await;

    let first_task = TaskId::generate();
    let second_task = TaskId::generate();
    let first = manager
        .create_worktree(
            &snapshot,
            &first_task,
            &AgentRunId::generate(),
            &["src/first.rs".to_owned()],
            1,
        )
        .await
        .expect("first worktree");
    let second = manager
        .create_worktree(
            &snapshot,
            &second_task,
            &AgentRunId::generate(),
            &["src/second.rs".to_owned()],
            1,
        )
        .await
        .expect("second worktree");

    assert_ne!(first.branch, second.branch, "each worker owns one branch");
    assert_ne!(first.path, second.path);
    assert_eq!(first.base_commit, snapshot.base_commit);
    assert_eq!(second.base_commit, snapshot.base_commit);

    // Two workers do not overwrite each other.
    std::fs::write(
        std::path::Path::new(&first.path).join("src/first.rs"),
        "one\n",
    )
    .unwrap();
    std::fs::write(
        std::path::Path::new(&second.path).join("src/second.rs"),
        "two\n",
    )
    .unwrap();
    let first_revision = manager
        .commit_worker_changes(&first, "first worker change")
        .await
        .expect("commit first");
    let second_revision = manager
        .commit_worker_changes(&second, "second worker change")
        .await
        .expect("commit second");
    assert_ne!(first_revision, second_revision);

    let first_changes = manager
        .branch_changes(&first.path, &snapshot.base_commit)
        .await
        .unwrap();
    let second_changes = manager
        .branch_changes(&second.path, &snapshot.base_commit)
        .await
        .unwrap();
    assert_eq!(first_changes.paths, vec!["src/first.rs".to_owned()]);
    assert_eq!(second_changes.paths, vec!["src/second.rs".to_owned()]);

    // A change outside the declared scope is refused, not committed.
    std::fs::write(
        std::path::Path::new(&second.path).join("src/escape.rs"),
        "x\n",
    )
    .unwrap();
    let error = manager
        .commit_worker_changes(&second, "escape")
        .await
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::ScopeAuthorityDenied);
    let error = manager
        .assert_write_scope(&second.path, &second.write_scope)
        .await
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::ScopeAuthorityDenied);

    // The user's checkout is untouched by any worker.
    assert_eq!(repo.commit_count(), 1);
    assert!(
        repo.is_clean(),
        "the user checkout must stay clean, saw: {:?}",
        repo.status_porcelain()
    );
}

#[tokio::test]
async fn p5_s04_dirty_input_is_refused_without_touching_user_changes() {
    let repo = test_repo();
    let manager = workspace_for(&repo);
    let project = ProjectId::generate();
    assert!(matches!(
        manager.inspect_input(repo.root(), &project).await.unwrap(),
        harness_orchestrator::InputInspection::Clean(_)
    ));

    // An untracked file makes the input dirty.
    std::fs::write(repo.root().join("src/untracked.rs"), "scratch\n").unwrap();
    let inspection = manager.inspect_input(repo.root(), &project).await.unwrap();
    let harness_orchestrator::InputInspection::Dirty(reasons) = inspection else {
        panic!("expected a dirty refusal");
    };
    assert!(
        reasons
            .iter()
            .any(|reason| reason.describe().contains("untracked file"))
    );

    std::fs::write(
        repo.root().join("src/lib.rs"),
        "pub fn greet() { /* changed */ }\n",
    )
    .unwrap();
    let inspection = manager.inspect_input(repo.root(), &project).await.unwrap();
    let harness_orchestrator::InputInspection::Dirty(reasons) = inspection else {
        panic!("expected a dirty refusal");
    };
    assert!(
        reasons
            .iter()
            .any(|reason| reason.describe().contains("tracked modification"))
    );

    // The refusal never stashed, reset or discarded anything: the exact user
    // state observed before the refusal is still present.
    let expected_dirty = "M src/lib.rs\n?? src/untracked.rs".to_owned();
    let after = repo.status_porcelain();
    assert_eq!(
        after, expected_dirty,
        "a dirty refusal must leave the user state byte-for-byte unchanged"
    );
    assert!(repo.root().join("src/untracked.rs").is_file());
    assert!(
        std::fs::read_to_string(repo.root().join("src/lib.rs"))
            .unwrap()
            .contains("changed")
    );
    assert_eq!(repo.commit_count(), 1);
}

// ---------------------------------------------------------------------------
// P5-S05
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p5_s05_report_without_artifacts_is_not_accepted_work() {
    let repo = test_repo();
    let manager = workspace_for(&repo);
    let snapshot = repo.snapshot(&manager).await;
    let store = Arc::new(open_store(&repo).await);
    let (session, root) = support::coordinator_root(&store).await;
    let worker = support::worker_ref(AgentRole::Coder, 1);
    let (task_id, plan) = single_task_plan(
        &root,
        AgentRole::Coder,
        &snapshot,
        2,
        &["edit:src/lib.rs"],
        vec!["src/lib.rs".to_owned()],
    );
    let coordinator = support::coordinator(&store, Arc::new(ScriptedWorker::explorer()), 1);
    coordinator.admit(&plan).await.expect("admit");
    coordinator
        .claim(&task_id, &worker, &session)
        .await
        .expect("claim");

    // An empty response and a bare "done" both fail to settle accepted work.
    let mut bare = support::explorer_report(&task_id, &worker, &snapshot.base_commit, "edit:x");
    bare.summary = "done".to_owned();
    bare.artifact_refs.clear();
    bare.checked_revisions.clear();
    bare.check_receipts.clear();
    let step = coordinator
        .settle(&plan, &task_id, WorkerOutcome::Reported(Box::new(bare)))
        .await
        .expect("settle");
    assert!(!step.accepted);
    assert_eq!(step.status, TaskStatus::Blocked);
    assert!(step.detail.contains("artifact") || step.detail.contains("receipt"));

    let node = store.task_node(&task_id).await.unwrap().unwrap();
    assert_eq!(node.status, TaskStatus::Blocked.as_str());
    assert!(
        store.task_result(&task_id).await.unwrap().is_none(),
        "a rejected report must not become a durable accepted result"
    );

    // An empty worker response is a report the host cannot accept either.
    let empty = support::explorer_report(&task_id, &worker, &snapshot.base_commit, "edit:x");
    let mut empty = empty;
    empty.summary = String::new();
    let step = coordinator
        .settle(&plan, &task_id, WorkerOutcome::Reported(Box::new(empty)))
        .await
        .expect("settle");
    assert!(!step.accepted);

    // And a worker that simply settles with no report cannot complete the task.
    let step = coordinator
        .settle(
            &plan,
            &task_id,
            WorkerOutcome::NoReport {
                reason: "the worker returned nothing".to_owned(),
            },
        )
        .await
        .expect("settle");
    assert!(!step.accepted);
    assert_eq!(step.status, TaskStatus::Blocked);
    support::close(store).await;
}

#[tokio::test]
async fn p5_final_worktree_change_is_rejected_before_apply() {
    let repo = test_repo();
    let manager = workspace_for(&repo);
    let snapshot = repo.snapshot(&manager).await;
    let integrator = harness_orchestrator::ResultIntegrator::new(Arc::clone(&manager));

    // A clean apply is allowed while the fingerprint is unchanged.
    let report = harness_orchestrator::IntegrationReport {
        project_id: snapshot.project_id.clone(),
        integration_root: manager
            .state_root()
            .join("integration/worktree")
            .to_string_lossy()
            .into_owned(),
        base_commit: snapshot.base_commit.clone(),
        final_commit: snapshot.base_commit.clone(),
        final_fingerprint: snapshot.fingerprint.clone(),
        steps: Vec::new(),
        conflicts: Vec::new(),
        checks: Vec::new(),
    };
    let verdict = integrator
        .recheck_before_apply(repo.root(), &snapshot.fingerprint, &report)
        .await
        .expect("recheck");
    assert!(matches!(
        verdict,
        harness_orchestrator::FinalApply::Allowed { .. }
    ));

    // The user changes the worktree after integration: the host refuses.
    std::fs::write(
        repo.root().join("src/lib.rs"),
        "pub fn greet() { /* user edit */ }\n",
    )
    .unwrap();
    let verdict = integrator
        .recheck_before_apply(repo.root(), &snapshot.fingerprint, &report)
        .await
        .expect("recheck");
    match verdict {
        harness_orchestrator::FinalApply::Refused { reason } => {
            assert!(reason.contains("refusing to apply"));
        }
        harness_orchestrator::FinalApply::Allowed { .. } => {
            panic!("a changed user worktree must not be overwritten");
        }
    }
    assert_eq!(repo.commit_count(), 1);
}

// ---------------------------------------------------------------------------
// P5-S05 integration ordering with real worktrees
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p5_s05_integration_is_dependency_ordered_and_rechecks_final_revision() {
    let repo = test_repo();
    let manager = workspace_for(&repo);
    let snapshot = repo.snapshot(&manager).await;

    let first_task = TaskId::generate();
    let second_task = TaskId::generate();
    let first = manager
        .create_worktree(
            &snapshot,
            &first_task,
            &AgentRunId::generate(),
            &["src/first.rs".to_owned()],
            1,
        )
        .await
        .expect("first worktree");
    let second = manager
        .create_worktree(
            &snapshot,
            &second_task,
            &AgentRunId::generate(),
            &["src/second.rs".to_owned()],
            1,
        )
        .await
        .expect("second worktree");
    std::fs::write(
        std::path::Path::new(&first.path).join("src/first.rs"),
        "one\n",
    )
    .unwrap();
    std::fs::write(
        std::path::Path::new(&second.path).join("src/second.rs"),
        "two\n",
    )
    .unwrap();
    let first_revision = manager
        .commit_worker_changes(&first, "first")
        .await
        .expect("commit first");
    let second_revision = manager
        .commit_worker_changes(&second, "second")
        .await
        .expect("commit second");

    let integrator = harness_orchestrator::ResultIntegrator::new(Arc::clone(&manager));
    let candidates = vec![
        harness_orchestrator::workspace_candidate(
            &first,
            &first_revision,
            vec!["src/first.rs".to_owned()],
        ),
        harness_orchestrator::workspace_candidate(
            &second,
            &second_revision,
            vec!["src/second.rs".to_owned()],
        ),
    ];
    let outcome = integrator
        .integrate(
            &snapshot,
            &[first_task.clone(), second_task.clone()],
            &candidates,
            &[],
        )
        .await
        .expect("integrate");
    let IntegrationOutcome::Ready(report) = outcome else {
        panic!("expected a clean integration, got {outcome:?}");
    };
    assert_eq!(report.steps.len(), 2);
    assert_eq!(report.steps[0].task_id, first_task);
    assert_eq!(report.steps[1].task_id, second_task);
    assert_ne!(report.final_commit, snapshot.base_commit);

    // The integrated revision contains both changes, which a single branch
    // revision could not prove.
    let integrated = std::path::Path::new(&report.integration_root);
    assert!(integrated.join("src/first.rs").is_file());
    assert!(integrated.join("src/second.rs").is_file());

    // Final checks run against the integrated revision, and a failing check
    // does not produce a ready report.
    let failed = integrator
        .integrate(
            &snapshot,
            &[first_task.clone(), second_task.clone()],
            &candidates,
            &["git rev-parse --verify definitely-not-a-revision".to_owned()],
        )
        .await
        .expect("integrate");
    assert!(matches!(failed, IntegrationOutcome::ChecksFailed { .. }));

    // Branch-level receipts never became integrated evidence.
    assert_ne!(first_revision, report.final_commit);
    assert_ne!(second_revision, report.final_commit);
    assert_eq!(repo.commit_count(), 1);
}

// ---------------------------------------------------------------------------
// P5-S05 conflict detection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p5_concurrent_worktree_creation_serializes_git_metadata() {
    let repo = test_repo();
    let manager = workspace_for(&repo);
    let snapshot = repo.snapshot(&manager).await;

    let mut handles = Vec::new();
    for _ in 0..3 {
        let manager = Arc::clone(&manager);
        let snapshot = snapshot.clone();
        handles.push(tokio::spawn(async move {
            manager
                .create_worktree(
                    &snapshot,
                    &TaskId::generate(),
                    &AgentRunId::generate(),
                    &["src/lib.rs".to_owned()],
                    1,
                )
                .await
        }));
    }
    let mut paths = Vec::new();
    let mut branches = Vec::new();
    for handle in handles {
        let record = handle.await.expect("join").expect("worktree");
        paths.push(record.path);
        branches.push(record.branch);
    }
    paths.sort();
    paths.dedup();
    branches.sort();
    branches.dedup();
    assert_eq!(
        paths.len(),
        3,
        "concurrent creation produced distinct worktrees"
    );
    assert_eq!(
        branches.len(),
        3,
        "concurrent creation produced distinct branches"
    );
    for path in &paths {
        assert!(std::path::Path::new(path).join(".git").exists());
    }
    assert_eq!(repo.commit_count(), 1);
}

// ---------------------------------------------------------------------------
// P5-S07
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p5_s07_cli_run_agents_reports_ownership_and_result_revisions() {
    let repo = test_repo();
    let data_dir = repo.data_dir("cli-data");
    let output = support::run_cli(&[
        "tasks",
        "run",
        "--data-dir",
        &data_dir.to_string_lossy(),
        "--text",
        "add a greeting",
        "--agents",
        "3",
        "--workspace",
        &repo.root().to_string_lossy(),
        "--json",
    ]);
    assert!(output.status.success(), "cli failed: {output:?}");
    let summary: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("cli json summary");
    assert_eq!(summary["schema_version"], 1);
    assert_eq!(summary["worker_slots"], 3);
    let roles = summary["role_availability"]
        .as_array()
        .expect("roles")
        .iter()
        .map(|value| value.as_str().unwrap_or_default().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(roles, vec!["explorer", "coder", "verifier"]);
    let outcomes = summary["outcomes"].as_array().expect("outcomes");
    assert_eq!(outcomes.len(), 3);
    assert!(
        outcomes
            .iter()
            .all(|outcome| outcome["accepted"] == json!(true)),
        "every delegated task must be accepted: {outcomes:?}"
    );

    // The CLI reports ownership, input/result revisions and remaining work.
    let status = support::run_cli(&[
        "tasks",
        "status",
        "--data-dir",
        &data_dir.to_string_lossy(),
        "--json",
    ]);
    assert!(status.status.success(), "cli status failed: {status:?}");
    let view: serde_json::Value = serde_json::from_slice(&status.stdout).expect("status json");
    assert_eq!(view["task_count"], 3);
    assert_eq!(view["remaining_work"], 0);
    for task in view["tasks"].as_array().expect("tasks") {
        assert_eq!(task["status"], "completed");
        assert!(task["ownership"]["owner_run_id"].is_string());
        assert!(task["ownership"]["owner_session_id"].is_string());
        let result = &task["result"];
        assert!(result["base_revision"].is_string());
        assert!(result["result_revision"].is_string());
        // An editing worker reports the revision it produced, which must differ
        // from the revision it started from. Non-editing workers report the
        // revision they observed.
        if task["role"] == "coder" {
            assert_ne!(
                result["base_revision"], result["result_revision"],
                "the coder must report the revision it committed"
            );
        } else {
            assert_eq!(result["base_revision"], result["result_revision"]);
        }
    }

    // Result inspection shows the recorded revisions, and the fixture repo is
    // still exactly as the user left it.
    let coder = view["tasks"]
        .as_array()
        .expect("tasks")
        .iter()
        .find(|task| task["role"] == "coder")
        .expect("coder task");
    let result = support::run_cli(&[
        "tasks",
        "result",
        "--data-dir",
        &data_dir.to_string_lossy(),
        "--task-id",
        coder["task_id"].as_str().unwrap(),
        "--json",
    ]);
    assert!(result.status.success(), "cli result failed: {result:?}");
    let inspected: serde_json::Value = serde_json::from_slice(&result.stdout).expect("result json");
    assert_eq!(inspected["status"], "completed");
    assert!(inspected["result"]["result_revision"].is_string());
    assert_eq!(repo.commit_count(), 1);
    assert!(repo.is_clean());
}
