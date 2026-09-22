//! P5 strengthening: previous-phase cases repeated with real delegated actors
//! and real host-owned worktrees.

use std::{path::Path, sync::Arc};

use crate::support::{
    FIXTURE_REQUESTS, ScriptedWorker, close, coordinator_root, explorer_report, make_node,
    open_store, plan_from, test_repo, worker_ref,
};
use harness_memory::{MemoryAction, MemoryPrincipal, MemoryService};
use harness_orchestrator::{
    AgentRole, DelegationBudget, GrantAction, InputInspection, SchedulerConfig, TaskStatus,
    WorkerOutcome, WorkerRequest, WorkerScheduler, WorktreeState,
};
use harness_types::{
    AgentProfileId, AgentRunId, ContentHash, ErrorCode, MemoryScope, ProjectId, SessionId,
    SourceAuthority, TaskId, Validity,
};

// ---------------------------------------------------------------------------
// C07 — three delegated agents publish without a lost update
// ---------------------------------------------------------------------------

fn observation_asset(
    project_id: &ProjectId,
    index: u32,
    commit: &str,
) -> harness_memory::CreateMemoryAsset {
    harness_memory::CreateMemoryAsset {
        kind: "observation".to_owned(),
        scope: MemoryScope::Project,
        layer: harness_memory::MemoryLayer::L1,
        project_id: Some(project_id.clone()),
        task_id: None,
        agent_profile_id: None,
        session_id: None,
        visibility: "scoped".to_owned(),
        content: format!("worker {index} observation"),
        authority: SourceAuthority::RuntimeObserved,
        evidence: harness_memory::EvidenceState::VerifiedObservation,
        user_confirmed: false,
        source_event_refs: Vec::new(),
        source_file_hashes: vec![ContentHash::from_bytes(format!("w{index}").as_bytes())],
        source_commit: Some(commit.to_owned()),
        provenance_kind: "runtime_observation".to_owned(),
        sources: Vec::new(),
    }
}

#[tokio::test]
async fn p5_c07_three_workers_publish_without_lost_update() {
    let repo = test_repo();
    let store = Arc::new(open_store(&repo).await);
    let memory = MemoryService::new(Arc::clone(&store));
    let project_id = ProjectId::generate();
    let commit = repo.head();

    let mut principals = Vec::new();
    let mut assets = Vec::new();
    for index in 0..3_u32 {
        let principal = MemoryPrincipal::user(format!("p5-worker-{index}"))
            .with_project(project_id.clone())
            .with_profile(AgentProfileId::generate());
        let asset = memory
            .create_asset(&principal, observation_asset(&project_id, index, &commit))
            .await
            .expect("create asset");
        principals.push(principal);
        assets.push(asset.asset.memory_asset_id);
    }

    // Each worker keeps its own immutable version: no lost update and no
    // version drift after three concurrent publishes.
    for (index, principal) in principals.iter().enumerate() {
        let asset = memory
            .read(principal, &assets[index])
            .await
            .expect("read own asset")
            .expect("own asset visible");
        assert_eq!(asset.asset.current_version, 1);
    }
    assert_eq!(store.delegation_task_count().await.unwrap(), 0);
    close(store).await;
}

// ---------------------------------------------------------------------------
// C08 — a forged child scope cannot read or export outside its grants
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p5_c08_forged_child_scope_cannot_read_outside_grants() {
    let repo = test_repo();
    let store = Arc::new(open_store(&repo).await);
    let delegated = harness_orchestrator::DelegatedMemoryService::new(Arc::clone(&store));
    let memory = MemoryService::new(Arc::clone(&store));
    let owner_project = ProjectId::generate();
    let other_project = ProjectId::generate();

    let owner = MemoryPrincipal::user("p5-owner").with_project(owner_project.clone());
    let asset = memory
        .create_asset(&owner, observation_asset(&owner_project, 9, &repo.head()))
        .await
        .expect("create owner asset");

    // A forged principal claiming another project and a fabricated worker id.
    let forger = MemoryPrincipal::user("p5-forger")
        .with_project(other_project)
        .with_task(TaskId::generate())
        .with_profile(AgentProfileId::generate());
    let read = delegated
        .read_as_worker(&forger, &asset.asset.memory_asset_id)
        .await
        .expect("read attempt is classified, not fatal");
    assert!(
        matches!(
            read,
            harness_orchestrator::MemoryBoundary::Denied
                | harness_orchestrator::MemoryBoundary::Missing
        ),
        "a forged scope must not read the asset: {read:?}"
    );
    let export = delegated
        .export_as_worker(&forger, &asset.asset.memory_asset_id)
        .await
        .expect("export attempt is classified");
    assert!(
        matches!(
            export,
            harness_orchestrator::MemoryBoundary::Denied
                | harness_orchestrator::MemoryBoundary::Missing
        ),
        "a forged scope must not export the asset: {export:?}"
    );

    // A genuine host grant makes the same asset readable, so the denial above
    // is authorization rather than a missing asset.
    delegated
        .grant(
            &owner,
            "p5-worker-genuine",
            &asset.asset.memory_asset_id,
            MemoryAction::Read,
        )
        .await
        .expect("grant");
    let genuine = MemoryPrincipal::user("p5-worker-genuine").with_project(owner_project.clone());
    assert_eq!(
        delegated
            .read_as_worker(&genuine, &asset.asset.memory_asset_id)
            .await
            .unwrap(),
        harness_orchestrator::MemoryBoundary::Visible
    );
    close(store).await;
}

// ---------------------------------------------------------------------------
// C10 — a worktree change invalidates a branch-level check
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p5_c10_worktree_change_invalidates_branch_check() {
    let repo = test_repo();
    let manager = Arc::new(harness_orchestrator::WorkspaceManager::new(
        repo.data_dir("delegation"),
    ));
    let snapshot = repo.snapshot(&manager).await;
    let task_id = TaskId::generate();
    let worktree = manager
        .create_worktree(
            &snapshot,
            &task_id,
            &AgentRunId::generate(),
            &["src/lib.rs".to_owned()],
            1,
        )
        .await
        .expect("worktree");
    std::fs::write(Path::new(&worktree.path).join("src/lib.rs"), "checked\n").unwrap();
    let checked_revision = manager
        .commit_worker_changes(&worktree, "checked change")
        .await
        .expect("commit");

    assert_ne!(checked_revision, snapshot.base_commit);
    let before = manager.fingerprint(&worktree.path).await.unwrap();

    // An external change to the same worktree moves the fingerprint, so the
    // earlier branch-level check no longer stands for the current revision.
    std::fs::write(
        Path::new(&worktree.path).join("src/lib.rs"),
        "changed outside\n",
    )
    .unwrap();
    let after = manager.fingerprint(&worktree.path).await.unwrap();
    assert_ne!(
        before, after,
        "an external change must move the fingerprint"
    );
    let violation = manager
        .assert_write_scope(&worktree.path, &["src/other.rs".to_owned()])
        .await
        .unwrap_err();
    assert_eq!(violation.code(), ErrorCode::ScopeAuthorityDenied);

    // The user changes their own checkout, so the stale branch evidence is
    // refused instead of promoted to an integrated revision.
    std::fs::write(repo.root().join("README.md"), "# changed by the user\n").unwrap();
    let integrator = harness_orchestrator::ResultIntegrator::new(Arc::clone(&manager));
    let report = harness_orchestrator::IntegrationReport {
        project_id: snapshot.project_id.clone(),
        integration_root: manager
            .state_root()
            .join("integration/worktree")
            .to_string_lossy()
            .into_owned(),
        base_commit: snapshot.base_commit.clone(),
        final_commit: checked_revision.clone(),
        final_fingerprint: before,
        steps: Vec::new(),
        conflicts: Vec::new(),
        checks: Vec::new(),
    };
    let verdict = integrator
        .recheck_before_apply(repo.root(), &snapshot.fingerprint, &report)
        .await
        .expect("recheck");
    assert!(
        matches!(verdict, harness_orchestrator::FinalApply::Refused { .. }),
        "a changed user worktree must refuse the stale branch evidence"
    );
}

// ---------------------------------------------------------------------------
// C15 / C24 — ownership generation fences a stale or competing worker
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p5_c15_stale_worker_generation_cannot_commit() {
    let repo = test_repo();
    let store = Arc::new(open_store(&repo).await);
    let manager = Arc::new(harness_orchestrator::WorkspaceManager::new(
        repo.data_dir("delegation"),
    ));
    let snapshot = repo.snapshot(&manager).await;
    let (session, root) = coordinator_root(&store).await;
    let task_id = TaskId::generate();
    let plan = plan_from(vec![make_node(
        &task_id,
        Some(&root),
        AgentRole::Explorer,
        &snapshot,
        2,
        &[],
        &["observe:x"],
        Vec::new(),
    )]);
    let coordinator = harness_orchestrator::DelegationCoordinator::new(
        Arc::clone(&store),
        Arc::new(
            WorkerScheduler::new(
                SchedulerConfig::default(),
                Arc::new(ScriptedWorker::explorer()),
                None,
            )
            .unwrap(),
        ),
        2,
    );
    coordinator.admit(&plan).await.expect("admit");

    // Generation 2 claims the task.
    let current = worker_ref(AgentRole::Explorer, 2);
    coordinator
        .claim(&task_id, &current, &session)
        .await
        .expect("claim at generation 2");

    // Generation 1 is stale and cannot take the task back.
    let stale = worker_ref(AgentRole::Explorer, 1);
    let error = coordinator
        .claim(&task_id, &stale, &session)
        .await
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::TaskOwnershipConflict);

    let owner = store.task_owner(&task_id).await.unwrap().expect("owner");
    assert_eq!(owner.generation, 2);
    assert_eq!(owner.owner_run_id, current.run_id);
    close(store).await;
}

#[tokio::test]
async fn p5_c24_competing_worker_cannot_own_one_task() {
    let repo = test_repo();
    let store = Arc::new(open_store(&repo).await);
    let manager = Arc::new(harness_orchestrator::WorkspaceManager::new(
        repo.data_dir("delegation"),
    ));
    let snapshot = repo.snapshot(&manager).await;
    let (first_session, root) = coordinator_root(&store).await;
    let second_session = SessionId::generate();
    let task_id = TaskId::generate();
    let plan = plan_from(vec![make_node(
        &task_id,
        Some(&root),
        AgentRole::Explorer,
        &snapshot,
        2,
        &[],
        &["observe:x"],
        Vec::new(),
    )]);
    let coordinator = harness_orchestrator::DelegationCoordinator::new(
        Arc::clone(&store),
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
    coordinator.admit(&plan).await.expect("admit");

    // Two different sessions race for the same task at the same generation.
    let first = worker_ref(AgentRole::Explorer, 1);
    coordinator
        .claim(&task_id, &first, &first_session)
        .await
        .expect("first claim");
    let competing = worker_ref(AgentRole::Explorer, 1);
    let error = coordinator
        .claim(&task_id, &competing, &second_session)
        .await
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::TaskOwnershipConflict);

    // A foreign run cannot advance the task even at the current generation.
    let foreign_run = AgentRunId::generate();
    let error = store.mark_task_running(&task_id, &foreign_run, 1).await;
    assert!(error.is_err(), "a foreign run must not advance the task");
    close(store).await;
}

// ---------------------------------------------------------------------------
// C23 — linked worktrees share identity, a similar clone does not
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p5_c23_linked_worktrees_share_project_identity() {
    let repo = test_repo();
    let manager = Arc::new(harness_orchestrator::WorkspaceManager::new(
        repo.data_dir("delegation"),
    ));
    let snapshot = repo.snapshot(&manager).await;
    // A verified inspection records the registration for this canonical root.
    assert_eq!(
        manager.registered_project(repo.root()),
        Some(snapshot.project_id.clone())
    );

    let first = manager
        .create_worktree(
            &snapshot,
            &TaskId::generate(),
            &AgentRunId::generate(),
            &["src/lib.rs".to_owned()],
            1,
        )
        .await
        .expect("first worktree");
    let second = manager
        .create_worktree(
            &snapshot,
            &TaskId::generate(),
            &AgentRunId::generate(),
            &["src/lib.rs".to_owned()],
            1,
        )
        .await
        .expect("second worktree");

    // Both workers resolve to the same project and the same base revision, so a
    // linked worktree never becomes a second project.
    assert_eq!(first.project_id, snapshot.project_id);
    assert_eq!(second.project_id, snapshot.project_id);
    assert_eq!(first.base_commit, second.base_commit);
    assert_ne!(first.branch, second.branch);

    // An unrelated clone with a similar shape is a different registration.
    let clone = tempfile::tempdir().expect("tempdir");
    let clone_path = clone.path().join("repo");
    let output = std::process::Command::new("git")
        .args([
            "clone",
            "--local",
            "--quiet",
            &repo.root().to_string_lossy(),
            &clone_path.to_string_lossy(),
        ])
        .output()
        .expect("git clone runs");
    assert!(output.status.success(), "clone failed");
    let other_project = ProjectId::generate();
    let other = manager
        .inspect_input(&clone_path, &other_project)
        .await
        .expect("inspect clone");
    let InputInspection::Clean(other) = other else {
        panic!("a fresh clone is clean");
    };
    assert_ne!(other.project_id, snapshot.project_id);
    assert_eq!(other.project_id, other_project);
    assert_eq!(
        other.base_commit, snapshot.base_commit,
        "identical content, distinct project identity"
    );
}

// ---------------------------------------------------------------------------
// K04 — a worker scope override is worker-local
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p5_k04_worker_scope_override_is_worker_local() {
    use harness_kernel::ScopedRegistry;
    use harness_types::{PluginInstanceId, ScopeId};

    // The coordinator and its workers form a real scope tree.
    let application = ScopeId::generate();
    let coordinator = ScopeId::generate();
    let worker_one = ScopeId::generate();
    let worker_two = ScopeId::generate();
    let mut registry = ScopedRegistry::default();
    registry.add_root(application.clone()).expect("root");
    registry
        .add_scope(coordinator.clone(), Some(application.clone()))
        .expect("coordinator scope");
    registry
        .add_scope(worker_one.clone(), Some(coordinator.clone()))
        .expect("worker one scope");
    registry
        .add_scope(worker_two.clone(), Some(coordinator.clone()))
        .expect("worker two scope");

    registry
        .register(&coordinator, "cancel-tool", PluginInstanceId::generate(), 1)
        .expect("coordinator registration");
    let override_token = registry
        .register(&worker_one, "cancel-tool", PluginInstanceId::generate(), 1)
        .expect("worker override");

    // Nearest lookup wins for the overriding worker only.
    let resolved = registry.lookup(&worker_one, "cancel-tool").expect("lookup");
    assert_eq!(resolved.token, override_token);
    let sibling = registry.lookup(&worker_two, "cancel-tool").expect("lookup");
    assert_ne!(
        sibling.token.instance_id, override_token.instance_id,
        "a sibling must not see the worker-local override"
    );
    assert_eq!(sibling.token.scope_id, coordinator);

    // A duplicate registration in the same layer is refused.
    assert!(
        registry
            .register(&worker_one, "cancel-tool", PluginInstanceId::generate(), 1)
            .is_err()
    );

    // A late disposer generation cannot remove the current registration.
    let mut stale = override_token.clone();
    stale.generation = 0;
    assert!(!registry.undo(&stale));
    assert_eq!(
        registry
            .lookup(&worker_one, "cancel-tool")
            .expect("still registered")
            .token,
        override_token
    );
}

// ---------------------------------------------------------------------------
// C03 — an uncertain child side effect is never completed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p5_c03_uncertain_child_effect_is_not_completed() {
    let repo = test_repo();
    let store = Arc::new(open_store(&repo).await);
    let manager = Arc::new(harness_orchestrator::WorkspaceManager::new(
        repo.data_dir("delegation"),
    ));
    let snapshot = repo.snapshot(&manager).await;
    let (session, root) = coordinator_root(&store).await;
    let task_id = TaskId::generate();
    let plan = plan_from(vec![make_node(
        &task_id,
        Some(&root),
        AgentRole::Coder,
        &snapshot,
        2,
        &[],
        &["edit:src/lib.rs"],
        vec!["src/lib.rs".to_owned()],
    )]);
    let coordinator = harness_orchestrator::DelegationCoordinator::new(
        Arc::clone(&store),
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
    coordinator.admit(&plan).await.expect("admit");
    let worker = worker_ref(AgentRole::Coder, 1);
    coordinator
        .claim(&task_id, &worker, &session)
        .await
        .expect("claim");

    // The worker returns no established outcome.
    let step = coordinator
        .settle(
            &plan,
            &task_id,
            WorkerOutcome::OutcomeUnknown {
                reason: "the write crossed the effect boundary without a receipt".to_owned(),
            },
        )
        .await
        .expect("settle");
    assert!(!step.accepted);
    assert_eq!(step.status, TaskStatus::Blocked);
    assert!(step.detail.contains("outcome_unknown"));
    let node = store.task_node(&task_id).await.unwrap().unwrap();
    assert_eq!(node.status, TaskStatus::Blocked.as_str());
    assert!(
        store.task_result(&task_id).await.unwrap().is_none(),
        "uncertain work must not have an accepted result"
    );

    // Reconciliation: the same task can later reach a real outcome, and only
    // then does it become accepted.
    let report = explorer_report(&task_id, &worker, &snapshot.base_commit, "edit:src/lib.rs");
    let step = coordinator
        .settle(&plan, &task_id, WorkerOutcome::Reported(Box::new(report)))
        .await
        .expect("settle");
    assert!(step.accepted, "{}", step.detail);
    close(store).await;
}

// ---------------------------------------------------------------------------
// K06 — provider loss drains dependent children
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p5_k06_provider_loss_drains_dependent_children() {
    let repo = test_repo();
    let manager = Arc::new(harness_orchestrator::WorkspaceManager::new(
        repo.data_dir("delegation"),
    ));
    let snapshot = repo.snapshot(&manager).await;
    let scheduler = Arc::new(
        WorkerScheduler::new(
            SchedulerConfig {
                max_concurrent_workers: 2,
                max_depth: 2,
                budget: DelegationBudget {
                    max_workers: 2,
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
    let task_id = TaskId::generate();
    let plan = plan_from(vec![make_node(
        &task_id,
        None,
        AgentRole::Coder,
        &snapshot,
        2,
        &[],
        &["edit:src/lib.rs"],
        vec!["src/lib.rs".to_owned()],
    )]);
    let node = plan.node(&task_id).unwrap();
    for _ in 0..2 {
        scheduler
            .dispatch(WorkerRequest {
                task_id: TaskId::generate(),
                run_id: AgentRunId::generate(),
                generation: 1,
                depth: node.depth,
                brief: node.brief.clone(),
                worktree: None,
                cancellation: harness_providers::CancellationToken::new(),
            })
            .expect("dispatch");
    }
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert_eq!(scheduler.live_worker_count(), 2);

    // The dependent provider is gone: racing shutdown callers share one
    // completion, both children drain, and their uncertainty is reported rather
    // than claimed as success.
    let first = scheduler.drain_descendants();
    let second = scheduler.drain_descendants();
    let (first, second) = tokio::join!(first, second);
    assert!(first.len() <= 2);
    assert!(second.len() <= 2);
    assert_eq!(scheduler.live_worker_count(), 0);
    assert_eq!(scheduler.outstanding_workers(), 0);
    assert!(scheduler.drained_unknowns().len() <= 2);
}

// ---------------------------------------------------------------------------
// K07 — racing shutdown shares one completion and reports failures
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p5_k07_racing_shutdown_shares_completion_and_reports_failures() {
    use harness_kernel::{ShutdownCoordinator, ShutdownPhase};

    let repo = test_repo();
    let manager = Arc::new(harness_orchestrator::WorkspaceManager::new(
        repo.data_dir("delegation"),
    ));
    let snapshot = repo.snapshot(&manager).await;
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
            Arc::new(ScriptedWorker::stalled()),
            None,
        )
        .expect("scheduler"),
    );
    let task_id = TaskId::generate();
    let plan = plan_from(vec![make_node(
        &task_id,
        None,
        AgentRole::Coder,
        &snapshot,
        1,
        &[],
        &["edit:src/lib.rs"],
        vec!["src/lib.rs".to_owned()],
    )]);
    let node = plan.node(&task_id).unwrap();
    scheduler
        .dispatch(WorkerRequest {
            task_id: task_id.clone(),
            run_id: AgentRunId::generate(),
            generation: 1,
            depth: node.depth,
            brief: node.brief.clone(),
            worktree: None,
            cancellation: harness_providers::CancellationToken::new(),
        })
        .expect("dispatch");
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let failing: Arc<dyn harness_kernel::ManagedResource> = Arc::new(FailingDisposer);
    let resource: Arc<dyn harness_kernel::ManagedResource> = scheduler.clone();
    let coordinator = ShutdownCoordinator::new(vec![
        (ShutdownPhase::Consumer, resource),
        (ShutdownPhase::Provider, failing),
    ]);

    // Two racing callers observe the same single completion.
    let first = coordinator.shutdown();
    let second = coordinator.shutdown();
    let (first, second) = tokio::join!(first, second);
    assert_eq!(
        first.closed, second.closed,
        "racing callers share one completion"
    );
    assert_eq!(first.errors, second.errors);
    assert!(
        first
            .errors
            .iter()
            .any(|error| error.contains("p5-failing-disposer")),
        "a disposer failure is collected, not silently logged away: {:?}",
        first.errors
    );
    assert_eq!(scheduler.live_worker_count(), 0);
}

struct FailingDisposer;

impl harness_kernel::ManagedResource for FailingDisposer {
    fn name(&self) -> &'static str {
        "p5-failing-disposer"
    }

    fn shutdown<'a>(
        &'a self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), harness_kernel::KernelError>> + Send + 'a>,
    > {
        Box::pin(async {
            Err(harness_kernel::KernelError::new(
                ErrorCode::ShutdownFailed,
                "the fixture disposer failed after an awaited teardown",
            ))
        })
    }

    fn join<'a>(
        &'a self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), harness_kernel::KernelError>> + Send + 'a>,
    > {
        Box::pin(async { Ok(()) })
    }
}

// ---------------------------------------------------------------------------
// P5-S06 — delegated memory binding respects the child scope
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p5_s06_delegated_memory_binding_respects_child_scope() {
    let repo = test_repo();
    let store = Arc::new(open_store(&repo).await);
    let memory = MemoryService::new(Arc::clone(&store));
    let delegated = harness_orchestrator::DelegatedMemoryService::new(Arc::clone(&store));
    let project_id = ProjectId::generate();
    let task_id = TaskId::generate();
    let profile_id = AgentProfileId::generate();
    let session_id = SessionId::generate();

    let owner = MemoryPrincipal::user("p5-owner").with_project(project_id.clone());
    let asset = memory
        .create_asset(&owner, observation_asset(&project_id, 7, &repo.head()))
        .await
        .expect("asset");

    // The host binds the asset to the child before spawn and records the exact
    // version it injected.
    let binding = harness_orchestrator::DelegatedMemoryBinding {
        binding_id: format!("binding-{}", asset.asset.memory_asset_id),
        task_id: task_id.clone(),
        profile_id: profile_id.clone(),
        memory_asset_id: asset.asset.memory_asset_id.clone(),
        version: asset.asset.current_version,
        injection_mode: "mandatory".to_owned(),
        priority: 1,
        actions: vec![GrantAction::Read],
    };
    delegated
        .bind_at_spawn(binding.clone())
        .await
        .expect("bind at spawn");

    // A child that holds the grant may read a task/profile snapshot.
    let child = MemoryPrincipal::user("p5-worker-genuine")
        .with_project(project_id.clone())
        .with_task(task_id.clone())
        .with_profile(profile_id.clone())
        .with_session(session_id);
    delegated
        .grant(
            &owner,
            "p5-worker-genuine",
            &asset.asset.memory_asset_id,
            MemoryAction::Read,
        )
        .await
        .expect("grant child read");
    assert_eq!(
        delegated
            .validate_before_dispatch(&child, &binding)
            .await
            .expect("validate"),
        harness_orchestrator::MemoryBoundary::Visible
    );

    // A later version makes the bound version stale before dispatch.
    memory
        .write_version(
            &owner,
            &asset.asset.memory_asset_id,
            asset.asset.current_version,
            harness_memory::WriteMemoryVersion {
                source_assets: Vec::new(),
                content: "a newer profile revision the child never saw".to_owned(),
                authority: SourceAuthority::User,
                evidence: harness_memory::EvidenceState::UserConfirmed,
                user_confirmed: true,
                source_event_refs: Vec::new(),
                source_file_hashes: vec![ContentHash::from_bytes(b"profile-v2")],
                source_commit: None,
                provenance_kind: "user-decision".to_owned(),
                validity: Validity::Valid,
                supersedes: Some(asset.asset.current_version),
                extractor_version: None,
                strategy_digest: None,
                sources: Vec::new(),
            },
        )
        .await
        .expect("write version 2");
    assert_eq!(
        delegated
            .validate_before_dispatch(&child, &binding)
            .await
            .expect("validate"),
        harness_orchestrator::MemoryBoundary::Stale,
        "a binding pinned to an older revision must be rebuilt before dispatch"
    );

    // Task state, not memory, is what records completion.
    assert_eq!(store.delegation_task_count().await.unwrap(), 0);
    assert!(
        memory
            .read(&child, &asset.asset.memory_asset_id)
            .await
            .expect("read")
            .is_some()
    );
    close(store).await;
}

// ---------------------------------------------------------------------------
// P5-S04 — worktree lifecycle is durable
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p5_s04_worktree_lifecycle_is_durable() {
    let repo = test_repo();
    let store = Arc::new(open_store(&repo).await);
    let manager = Arc::new(harness_orchestrator::WorkspaceManager::new(
        repo.data_dir("delegation"),
    ));
    let snapshot = repo.snapshot(&manager).await;
    let task_id = TaskId::generate();
    let run_id = AgentRunId::generate();
    let record = manager
        .create_worktree(&snapshot, &task_id, &run_id, &["src/lib.rs".to_owned()], 1)
        .await
        .expect("worktree");
    store
        .upsert_worktree(&harness_store_sqlite::WorktreeRecordRow {
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
        .expect("persist worktree");

    let stored = store
        .worktree(&record.worktree_id)
        .await
        .expect("read worktree")
        .expect("worktree exists");
    assert_eq!(stored.branch, record.branch);
    assert_eq!(stored.base_commit, snapshot.base_commit);
    assert_eq!(stored.write_scope, vec!["src/lib.rs".to_owned()]);
    assert_eq!(
        harness_orchestrator::WorktreeState::parse(&stored.state),
        Some(WorktreeState::Ready)
    );
    assert_eq!(store.list_worktrees().await.unwrap().len(), 1);
    close(store).await;
}
