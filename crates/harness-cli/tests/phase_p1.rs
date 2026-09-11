use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
};

use harness_kernel::{
    KernelError, ManagedResource, PluginDescriptor, PluginGraph, ScopedRegistry, ServiceProvider,
    ServiceRequirement, ShutdownCoordinator, ShutdownPhase, mount_resources,
};
use harness_session::{AdmitInputRequest, RecordSyntheticReceiptRequest, SessionService};
use harness_store_sqlite::{
    HostFence, SqliteStore, StoreFaultPlan, StoreFaultPoint, WriterOpenOptions,
};
use harness_types::{
    ContentHash, ErrorCode, EventEnvelope, EventId, HostId, InputId, P0_SCHEMA_VERSION, PlanItem,
    PlanItemId, PlanItemStatus, PluginInstanceId, ProducerIdentity, ProjectId, ScopeId, SessionId,
    SourceAuthority, SourceRef, TaskId, ToolExecutionId, ToolExecutionReceipt, ToolIntentState,
    ToolOutcomeState, WorkspaceObservation,
};
use proptest::prelude::*;
use serde_json::{Map, Value};
use tempfile::TempDir;

fn workspace() -> WorkspaceObservation {
    WorkspaceObservation {
        project_id: ProjectId::generate(),
        worktree_id: "phase-p1".to_owned(),
        base_commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        observed_fingerprint: ContentHash::from_bytes(b"phase p1 workspace"),
    }
}

fn admission_request(
    session_id: SessionId,
    task_id: TaskId,
    input_id: InputId,
    raw_text: impl Into<String>,
    expected_sequence: u64,
) -> AdmitInputRequest {
    AdmitInputRequest {
        session_id,
        task_id,
        input_id,
        expected_sequence,
        authority: SourceAuthority::User,
        raw_text: raw_text.into(),
        workspace: workspace(),
        initial_plan_items: Vec::new(),
    }
}

fn receipt(task_id: TaskId, sequence: u64) -> ToolExecutionReceipt {
    ToolExecutionReceipt {
        schema_version: P0_SCHEMA_VERSION,
        tool_execution_id: ToolExecutionId::generate(),
        task_id,
        invocation_id: format!("receipt-{sequence}"),
        input_hash: ContentHash::from_bytes(b"synthetic receipt"),
        policy_revision: 1,
        approval_id: None,
        intent_state: ToolIntentState::IntentRecorded,
        outcome_state: ToolOutcomeState::Settled,
        before_fingerprint: None,
        after_fingerprint: None,
        artifact_id: None,
        observed_at_seq: sequence,
    }
}

fn recovery_demo_plan_items() -> Vec<PlanItem> {
    let source = SourceRef {
        event_id: EventId::generate(),
        sequence: 1,
        content_hash: ContentHash::from_bytes(b"P1 recovery demo evidence"),
    };
    [
        PlanItemStatus::Completed,
        PlanItemStatus::Completed,
        PlanItemStatus::Pending,
    ]
    .into_iter()
    .map(|status| PlanItem {
        id: PlanItemId::generate(),
        status,
        owner: None,
        dependencies: Vec::new(),
        evidence_refs: vec![source.clone()],
    })
    .collect()
}

async fn writer(temp: &TempDir) -> Arc<SqliteStore> {
    Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(temp.path(), HostId::generate()))
            .await
            .expect("writer should open"),
    )
}

fn fixture_child(
    temp: &TempDir,
    mode: &str,
    host_id: &HostId,
    session_id: Option<&SessionId>,
    task_id: Option<&TaskId>,
    input_id: Option<&InputId>,
) -> (Child, Value) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_p1_fixture_host"));
    command
        .arg("--data-dir")
        .arg(temp.path())
        .arg("--host-id")
        .arg(host_id.as_str())
        .arg("--mode")
        .arg(mode)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(session_id) = session_id {
        command.arg("--session-id").arg(session_id.as_str());
    }
    if let Some(task_id) = task_id {
        command.arg("--task-id").arg(task_id.as_str());
    }
    if let Some(input_id) = input_id {
        command.arg("--input-id").arg(input_id.as_str());
    }
    let mut child = command.spawn().expect("fixture child should start");
    let stdout = child.stdout.take().expect("child stdout is piped");
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("fixture child should write readiness line");
    assert!(
        !line.trim().is_empty(),
        "fixture child did not become ready"
    );
    let value = serde_json::from_str(&line).expect("fixture readiness must be JSON");
    (child, value)
}

fn kill_child(mut child: Child) {
    child.kill().expect("parent should terminate fixture child");
    let status = child.wait().expect("fixture child should terminate");
    assert!(
        !status.success(),
        "terminated fixture child must not report success"
    );
}

fn run_ha(arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ha"))
        .args(arguments)
        .output()
        .expect("ha binary should execute")
}

async fn close_writer(store: Arc<SqliteStore>) {
    let Ok(store) = Arc::try_unwrap(store) else {
        panic!("test must release all store consumers before close")
    };
    store.close().await.expect("writer should close cleanly");
}

#[test]
fn p1_s01_contracts_are_available_and_versioned() {
    assert_eq!(harness_store_sqlite::STORE_SCHEMA_VERSION, 1);
    assert_eq!(harness_types::P0_SCHEMA_VERSION, 1);
    assert!(HostId::generate().as_str().starts_with("host_"));
    assert!(InputId::generate().as_str().starts_with("input_"));
    assert!(harness_kernel::ShutdownPhase::Store > harness_kernel::ShutdownPhase::Provider);
}

#[tokio::test]
async fn p1_c01_child_kill_after_ack_recovers_one_input() {
    let temp = TempDir::new().expect("temporary data directory");
    let host_id = HostId::generate();
    let session_id = SessionId::generate();
    let task_id = TaskId::generate();
    let input_id = InputId::generate();
    let (child, ready) = fixture_child(
        &temp,
        "input-ack-wait",
        &host_id,
        Some(&session_id),
        Some(&task_id),
        Some(&input_id),
    );
    assert_eq!(ready["ack"], "input");
    assert_eq!(ready["sequence"], 1);
    kill_child(child);

    let store = writer(&temp).await;
    let service = SessionService::new(Arc::clone(&store));
    let recovered = service
        .recover(&session_id)
        .await
        .expect("recovery succeeds");
    assert_eq!(store.inbox_count(&session_id).await.unwrap(), 1);
    assert_eq!(
        recovered.instruction_texts,
        vec!["recover this durable instruction".to_owned()]
    );
    assert_eq!(recovered.replayed_through_sequence, 1);
    assert_eq!(
        store.load_events_after(&session_id, 0).await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn p1_c02_child_kill_after_receipt_before_snapshot_folds_committed_tail() {
    let temp = TempDir::new().expect("temporary data directory");
    let host_id = HostId::generate();
    let session_id = SessionId::generate();
    let task_id = TaskId::generate();
    let input_id = InputId::generate();
    let (child, ready) = fixture_child(
        &temp,
        "receipt-ack-wait",
        &host_id,
        Some(&session_id),
        Some(&task_id),
        Some(&input_id),
    );
    assert_eq!(ready["ack"], "receipt");
    assert_eq!(ready["sequence"], 2);
    kill_child(child);

    let store = writer(&temp).await;
    let recovered = SessionService::new(Arc::clone(&store))
        .recover(&session_id)
        .await
        .expect("snapshot plus tail recovery succeeds");
    assert_eq!(recovered.snapshot_sequence, Some(1));
    assert_eq!(recovered.replayed_through_sequence, 2);
    assert_eq!(recovered.receipts.len(), 1);
    assert_eq!(recovered.pending_execution_count, 0);
    assert_eq!(
        store.load_events_after(&session_id, 0).await.unwrap().len(),
        2
    );
}

#[tokio::test]
async fn p1_c15_writer_competition_and_stale_fence_are_rejected() {
    let temp = TempDir::new().expect("temporary data directory");
    let old_host = HostId::generate();
    let (child, ready) = fixture_child(&temp, "hold-writer", &old_host, None, None, None);
    assert_eq!(ready["ready"], "writer");
    let old_generation = ready["generation"]
        .as_u64()
        .expect("writer fixture generation is numeric");

    let Err(conflict) =
        SqliteStore::open_writer(WriterOpenOptions::new(temp.path(), HostId::generate())).await
    else {
        panic!("second process must not acquire writer lock")
    };
    assert_eq!(conflict.code(), ErrorCode::WriterLocked);
    kill_child(child);

    let store = writer(&temp).await;
    let stale = HostFence {
        host_id: old_host,
        generation: old_generation,
    };
    let error = store
        .assert_current_fence(&stale)
        .await
        .expect_err("old fence must be rejected after takeover");
    assert_eq!(error.code(), ErrorCode::StaleWriter);
}

#[tokio::test]
async fn p1_c21_injected_write_failure_never_acknowledges_or_advances_state() {
    let temp = TempDir::new().expect("temporary data directory");
    let faults = StoreFaultPlan::with_point(StoreFaultPoint::BeforeAdmissionCommit);
    let store = Arc::new(
        SqliteStore::open_writer(
            WriterOpenOptions::new(temp.path(), HostId::generate()).with_fault_plan(faults.clone()),
        )
        .await
        .expect("writer opens"),
    );
    let service = SessionService::new(Arc::clone(&store));
    let session_id = SessionId::generate();
    let task_id = TaskId::generate();
    let failed = service
        .admit_input(admission_request(
            session_id.clone(),
            task_id.clone(),
            InputId::generate(),
            "must not be acknowledged",
            1,
        ))
        .await
        .expect_err("injected admission failure must reject ACK");
    assert_eq!(failed.code(), ErrorCode::StorageWriteFailed);
    assert_eq!(store.inbox_count(&session_id).await.unwrap(), 0);
    assert!(store.session_summary(&session_id).await.unwrap().is_none());

    service
        .admit_input(admission_request(
            session_id.clone(),
            task_id.clone(),
            InputId::generate(),
            "admitted after transient storage fault",
            1,
        ))
        .await
        .expect("fault is one-shot");
    faults.arm(StoreFaultPoint::BeforeReceiptCommit);
    let error = service
        .record_synthetic_receipt(RecordSyntheticReceiptRequest {
            session_id: session_id.clone(),
            task_id: task_id.clone(),
            expected_sequence: 2,
            receipt: receipt(task_id, 2),
            artifact: None,
        })
        .await
        .expect_err("receipt failure must not become an ACK");
    assert_eq!(error.code(), ErrorCode::StorageWriteFailed);
    let recovered = service.recover(&session_id).await.unwrap();
    assert_eq!(recovered.replayed_through_sequence, 1);
    assert!(recovered.receipts.is_empty());
}

#[tokio::test]
async fn p1_c24_task_lease_blocks_competing_session_continuation() {
    let temp = TempDir::new().expect("temporary data directory");
    let store = writer(&temp).await;
    let service = SessionService::new(Arc::clone(&store));
    let task_id = TaskId::generate();
    service
        .admit_input(admission_request(
            SessionId::generate(),
            task_id.clone(),
            InputId::generate(),
            "first session owns task",
            1,
        ))
        .await
        .expect("first session claims task");
    let error = service
        .admit_input(admission_request(
            SessionId::generate(),
            task_id,
            InputId::generate(),
            "second session must be denied",
            1,
        ))
        .await
        .expect_err("competing session must not claim task");
    assert_eq!(error.code(), ErrorCode::TaskLeaseConflict);
}

#[tokio::test]
async fn p1_s02_sqlite_diagnostics_and_read_only_boundaries_are_real() {
    let temp = TempDir::new().expect("temporary data directory");
    let missing = TempDir::new().expect("empty data directory");
    let Err(read_only) = SqliteStore::open_read_only(missing.path()).await else {
        panic!("read-only open may not create a database")
    };
    assert_eq!(read_only.code(), ErrorCode::ReadOnlyStore);

    let migration_fault = StoreFaultPlan::with_point(StoreFaultPoint::BeforeMigrationCommit);
    let Err(migration_error) = SqliteStore::open_writer(
        WriterOpenOptions::new(temp.path(), HostId::generate()).with_fault_plan(migration_fault),
    )
    .await
    else {
        panic!("migration fault must abort before schema commit")
    };
    assert_eq!(migration_error.code(), ErrorCode::MigrationFailed);

    let store = writer(&temp).await;
    let diagnostics = store.diagnostics().await.expect("diagnostics are readable");
    assert!(diagnostics.foreign_keys_enabled);
    assert_eq!(diagnostics.journal_mode.to_lowercase(), "wal");
    assert_eq!(diagnostics.synchronous, 2);
    assert!(diagnostics.busy_timeout_ms >= 5_000);
}

#[tokio::test]
async fn p1_s04_duplicate_id_is_idempotent_and_retains_unclassified_text() {
    let temp = TempDir::new().expect("temporary data directory");
    let store = writer(&temp).await;
    let service = SessionService::new(Arc::clone(&store));
    let session_id = SessionId::generate();
    let task_id = TaskId::generate();
    let input_id = InputId::generate();
    let first = service
        .admit_input(admission_request(
            session_id.clone(),
            task_id.clone(),
            input_id.clone(),
            "tiếp tục sửa parser, đừng bỏ sót lỗi cũ",
            1,
        ))
        .await
        .unwrap();
    let retry = service
        .admit_input(admission_request(
            session_id.clone(),
            task_id.clone(),
            input_id.clone(),
            "tiếp tục sửa parser, đừng bỏ sót lỗi cũ",
            99,
        ))
        .await
        .unwrap();
    assert_eq!(first.event_id, retry.event_id);
    assert!(retry.idempotent_replay);
    assert_eq!(store.inbox_count(&session_id).await.unwrap(), 1);
    let conflict = service
        .admit_input(admission_request(
            session_id.clone(),
            task_id,
            input_id,
            "same ID but different text",
            2,
        ))
        .await
        .expect_err("changed idempotency payload is rejected");
    assert_eq!(conflict.code(), ErrorCode::IdempotencyConflict);
    let recovered = service.recover(&session_id).await.unwrap();
    assert!(
        recovered
            .instruction_texts
            .iter()
            .any(|text| text.contains("đừng bỏ sót lỗi cũ"))
    );
}

#[tokio::test]
async fn p1_s05_artifacts_snapshots_and_corruption_fallback_are_durable() {
    let temp = TempDir::new().expect("temporary data directory");
    let store = writer(&temp).await;
    let service = SessionService::new(Arc::clone(&store));
    let session_id = SessionId::generate();
    let task_id = TaskId::generate();
    service
        .admit_input(admission_request(
            session_id.clone(),
            task_id.clone(),
            InputId::generate(),
            "persist artifact evidence",
            1,
        ))
        .await
        .unwrap();
    let snapshot = service.write_snapshot(&session_id).await.unwrap();
    assert_eq!(snapshot.through_sequence, 1);
    let artifact = store
        .publish_artifact(b"artifact bytes that are flushed first")
        .unwrap();
    assert!(
        store
            .paths()
            .data_dir
            .join(&artifact.relative_path)
            .is_file()
    );
    let mut synthetic = receipt(task_id.clone(), 2);
    synthetic.artifact_id = Some(artifact.artifact_id.clone());
    service
        .record_synthetic_receipt(RecordSyntheticReceiptRequest {
            session_id: session_id.clone(),
            task_id: task_id.clone(),
            expected_sequence: 2,
            receipt: synthetic,
            artifact: Some(artifact),
        })
        .await
        .unwrap();
    let from_snapshot = service.recover(&session_id).await.unwrap();
    assert_eq!(from_snapshot.snapshot_sequence, Some(1));
    assert_eq!(from_snapshot.receipts.len(), 1);

    store
        .corrupt_latest_snapshot_for_test(&session_id)
        .await
        .unwrap();
    let from_journal = service.recover(&session_id).await.unwrap();
    assert!(from_journal.snapshot_diagnostic.is_some());
    assert_eq!(from_journal.working_state, from_snapshot.working_state);
    assert_eq!(from_journal.receipts, from_snapshot.receipts);

    let payload = Map::new();
    let unknown = EventEnvelope {
        schema_version: P0_SCHEMA_VERSION,
        event_id: EventId::generate(),
        session_id: session_id.clone(),
        seq: 3,
        event_type: "p1.unknown_critical".to_owned(),
        producer: ProducerIdentity {
            plugin_id: "fixture".to_owned(),
            implementation_version: "1".to_owned(),
        },
        authority: SourceAuthority::RuntimeObserved,
        correlation_id: None,
        causation_id: None,
        continuity_critical: true,
        payload: payload.clone(),
        payload_hash: ContentHash::from_canonical_json(&Value::Object(payload)).unwrap(),
    };
    store.append_event_for_test(unknown).await.unwrap();
    let error = service
        .recover(&session_id)
        .await
        .expect_err("unknown critical journal event must fail closed");
    assert_eq!(error.code(), ErrorCode::UnknownCriticalEvent);
}

fn service_contract(service_id: &str, api_version: u16) -> harness_types::ServiceContract {
    harness_types::ServiceContract {
        service_id: service_id.to_owned(),
        api_version,
    }
}

fn descriptor(
    scope_id: ScopeId,
    provides: Vec<harness_types::ServiceContract>,
    requires: Vec<ServiceRequirement>,
) -> PluginDescriptor {
    PluginDescriptor::new(
        PluginInstanceId::generate(),
        scope_id,
        1,
        provides,
        requires,
    )
}

#[test]
fn p1_k01_graph_rejects_missing_incompatible_cycle_and_duplicates_before_admission() {
    let scope = ScopeId::generate();
    let missing = PluginGraph::new(vec![descriptor(
        scope.clone(),
        Vec::new(),
        vec![ServiceRequirement::required("store", 1)],
    )]);
    assert_eq!(
        missing.validate().expect_err("missing provider").code(),
        ErrorCode::MissingRequiredService
    );

    let incompatible = PluginGraph::new(vec![
        descriptor(
            scope.clone(),
            vec![service_contract("store", 1)],
            Vec::new(),
        ),
        descriptor(
            scope.clone(),
            Vec::new(),
            vec![ServiceRequirement::required("store", 2)],
        ),
    ]);
    assert_eq!(
        incompatible
            .validate()
            .expect_err("incompatible provider")
            .code(),
        ErrorCode::IncompatibleService
    );

    let cycle_scope = ScopeId::generate();
    let cycle = PluginGraph::new(vec![
        descriptor(
            cycle_scope.clone(),
            vec![service_contract("a", 1)],
            vec![ServiceRequirement::required("b", 1)],
        ),
        descriptor(
            cycle_scope.clone(),
            vec![service_contract("b", 1)],
            vec![ServiceRequirement::required("a", 1)],
        ),
    ]);
    assert_eq!(
        cycle.validate().expect_err("dependency cycle").code(),
        ErrorCode::PluginCycle
    );

    let duplicate = PluginGraph::new(vec![
        descriptor(
            scope.clone(),
            vec![service_contract("store", 1)],
            Vec::new(),
        ),
        descriptor(scope, vec![service_contract("store", 1)], Vec::new()),
    ]);
    assert_eq!(
        duplicate.validate().expect_err("duplicate provider").code(),
        ErrorCode::DuplicateRegistration
    );
}

#[test]
fn p1_k02_optional_extractor_absence_allows_required_composition() {
    let graph = PluginGraph::new(vec![descriptor(
        ScopeId::generate(),
        Vec::new(),
        vec![ServiceRequirement::optional("memory_extractor", 1)],
    )]);
    graph
        .validate()
        .expect("optional extractor absence is a deterministic degradation");
}

#[derive(Clone)]
struct RecordingResource {
    name: String,
    events: Arc<Mutex<Vec<String>>>,
    fail_shutdown: bool,
}

impl RecordingResource {
    fn new(
        name: impl Into<String>,
        events: Arc<Mutex<Vec<String>>>,
        fail_shutdown: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            name: name.into(),
            events,
            fail_shutdown,
        })
    }
}

impl ManagedResource for RecordingResource {
    fn name(&self) -> &str {
        &self.name
    }

    fn shutdown<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), KernelError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.events
                .lock()
                .expect("recording mutex")
                .push(format!("shutdown:{}", self.name));
            if self.fail_shutdown {
                Err(KernelError::new(
                    ErrorCode::ShutdownFailed,
                    format!("{} refused shutdown", self.name),
                ))
            } else {
                Ok(())
            }
        })
    }

    fn join<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), KernelError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.events
                .lock()
                .expect("recording mutex")
                .push(format!("join:{}", self.name));
            Ok(())
        })
    }
}

#[tokio::test]
async fn p1_k03_failed_mount_rolls_back_unpublished_resources_and_joins_them() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let first = RecordingResource::new("first", Arc::clone(&events), false);
    let second = RecordingResource::new("second", Arc::clone(&events), false);
    let result = mount_resources(|resources| {
        resources.collect(first);
        resources.collect(second);
        Err(KernelError::new(
            ErrorCode::InvalidPayload,
            "fixture mount failure",
        ))
    })
    .await;
    assert!(result.is_err());
    assert_eq!(
        *events.lock().unwrap(),
        [
            "shutdown:second",
            "join:second",
            "shutdown:first",
            "join:first"
        ]
    );
}

#[test]
fn p1_k04_scoped_lookup_is_nearest_and_siblings_are_isolated() {
    let root = ScopeId::generate();
    let child = ScopeId::generate();
    let sibling = ScopeId::generate();
    let root_instance = PluginInstanceId::generate();
    let child_instance = PluginInstanceId::generate();
    let mut registry = ScopedRegistry::default();
    registry.add_root(root.clone()).unwrap();
    registry
        .add_scope(child.clone(), Some(root.clone()))
        .unwrap();
    registry
        .add_scope(sibling.clone(), Some(root.clone()))
        .unwrap();

    let root_token = registry
        .register(&root, "store", root_instance, 1)
        .expect("root registration");
    assert_eq!(registry.lookup(&child, "store").unwrap().token, root_token);
    let child_token = registry
        .register(&child, "store", child_instance, 1)
        .expect("child override");
    assert_eq!(registry.lookup(&child, "store").unwrap().token, child_token);
    assert_eq!(
        registry.lookup(&sibling, "store").unwrap().token,
        root_token
    );
    assert_eq!(
        registry
            .register(&child, "store", PluginInstanceId::generate(), 1)
            .expect_err("same-layer duplicate")
            .code(),
        ErrorCode::DuplicateRegistration
    );
}

#[test]
fn p1_k05_late_disposer_cannot_remove_replacement_generation() {
    let root = ScopeId::generate();
    let mut registry = ScopedRegistry::default();
    registry.add_root(root.clone()).unwrap();
    let old = registry
        .register(&root, "store", PluginInstanceId::generate(), 1)
        .unwrap();
    assert!(registry.undo(&old));
    let replacement = registry
        .register(&root, "store", PluginInstanceId::generate(), 2)
        .unwrap();
    assert!(!registry.undo(&old));
    assert_eq!(registry.lookup(&root, "store").unwrap().token, replacement);
}

#[tokio::test]
async fn review_p1_duplicate_active_call_ids_are_rejected() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let provider = ServiceProvider::new(
        "store",
        1,
        RecordingResource::new("provider", Arc::clone(&events), false),
    );
    let lease = provider.lease();
    let first = lease.begin_call("same-id").unwrap();
    assert!(
        lease.begin_call("same-id").is_err(),
        "duplicate call hid active work"
    );
    first.settle();
    lease.begin_call("same-id").unwrap().settle();
    let report = provider.lose_and_drain().await;
    assert!(report.outcome_uncertainties.is_empty());
    assert_eq!(
        *events.lock().unwrap(),
        ["shutdown:provider", "join:provider"]
    );
}

#[tokio::test]
async fn p1_k06_provider_loss_stops_admission_drains_dependents_and_marks_unknown() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let resource = RecordingResource::new("provider", Arc::clone(&events), false);
    let provider = ServiceProvider::new("store", 1, resource);
    let lease = provider.lease();
    let call = lease.begin_call("unsettled-call").expect("call begins");
    let draining_provider = Arc::clone(&provider);
    let draining = tokio::spawn(async move { draining_provider.lose_and_drain().await });
    tokio::task::yield_now().await;
    let Err(rejected) = lease.begin_call("must-be-rejected") else {
        panic!("loss must block new calls")
    };
    assert_eq!(rejected.code(), ErrorCode::ServiceUnavailable);
    assert!(!draining.is_finished(), "provider waits for dependent call");
    drop(call);
    let report = draining.await.unwrap();
    assert_eq!(report.outcome_uncertainties, ["unsettled-call"]);
    assert_eq!(
        *events.lock().unwrap(),
        ["shutdown:provider", "join:provider"]
    );
}

#[tokio::test]
async fn p1_k07_racing_shutdown_is_shared_aggregates_errors_and_closes_store_last() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let coordinator = Arc::new(ShutdownCoordinator::new(vec![
        (
            ShutdownPhase::Store,
            RecordingResource::new("store", Arc::clone(&events), false),
        ),
        (
            ShutdownPhase::Provider,
            RecordingResource::new("provider", Arc::clone(&events), false),
        ),
        (
            ShutdownPhase::Consumer,
            RecordingResource::new("consumer", Arc::clone(&events), true),
        ),
    ]));
    let left = {
        let coordinator = Arc::clone(&coordinator);
        tokio::spawn(async move { coordinator.shutdown().await })
    };
    let right = {
        let coordinator = Arc::clone(&coordinator);
        tokio::spawn(async move { coordinator.shutdown().await })
    };
    let left = left.await.unwrap();
    let right = right.await.unwrap();
    assert_eq!(left, right);
    assert_eq!(
        *events.lock().unwrap(),
        [
            "shutdown:consumer",
            "join:consumer",
            "shutdown:provider",
            "join:provider",
            "shutdown:store",
            "join:store"
        ]
    );
    assert_eq!(left.errors.len(), 1);
    assert!(left.errors[0].contains("consumer refused shutdown"));
}

#[tokio::test]
async fn p1_s07_cli_inspects_persisted_recovery_state_without_runtime() {
    let temp = TempDir::new().expect("temporary data directory");
    let data_dir = temp.path().to_str().expect("temporary path is UTF-8");
    let initialized = run_ha(&["init", "--data-dir", data_dir, "--json"]);
    assert!(initialized.status.success());
    let initialized_json: Value = serde_json::from_slice(&initialized.stdout).unwrap();
    assert_eq!(initialized_json["runtime"], "not_available_in_p1");
    let plugin_instance = initialized_json["plugin_instances"][0]
        .as_str()
        .expect("init reports a plugin instance")
        .to_owned();

    let store = writer(&temp).await;
    let service = SessionService::new(Arc::clone(&store));
    let session_id = SessionId::generate();
    let task_id = TaskId::generate();
    let mut request = admission_request(
        session_id.clone(),
        task_id,
        InputId::generate(),
        "inspect the two-completed one-pending recovery demo",
        1,
    );
    request.initial_plan_items = recovery_demo_plan_items();
    service.admit_input(request).await.unwrap();
    service.write_snapshot(&session_id).await.unwrap();
    drop(service);
    close_writer(store).await;

    let sessions = run_ha(&["sessions", "list", "--data-dir", data_dir, "--json"]);
    assert!(sessions.status.success());
    let sessions_json: Value = serde_json::from_slice(&sessions.stdout).unwrap();
    assert_eq!(sessions_json["sessions"].as_array().unwrap().len(), 1);

    let status = run_ha(&[
        "status",
        "--data-dir",
        data_dir,
        "--session-id",
        session_id.as_str(),
        "--json",
    ]);
    assert!(status.status.success());
    let status_json: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status_json["recovery"]["completed_plan_items"], 2);
    assert_eq!(status_json["recovery"]["pending_plan_items"], 1);
    assert_eq!(status_json["runtime"], "not_available_in_p1");

    let plugins = run_ha(&["plugins", "list", "--data-dir", data_dir, "--json"]);
    assert!(plugins.status.success());
    let plugins_json: Value = serde_json::from_slice(&plugins.stdout).unwrap();
    assert_eq!(plugins_json["plugins"].as_array().unwrap().len(), 2);
    let inspect = run_ha(&[
        "plugins",
        "inspect",
        "--data-dir",
        data_dir,
        "--instance-id",
        &plugin_instance,
        "--json",
    ]);
    assert!(inspect.status.success());
    let inspect_json: Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(inspect_json["runtime"], "not_loaded_in_p1");

    let config = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/p0/config/valid.toml");
    let explain = run_ha(&[
        "config",
        "explain",
        "--config",
        config.to_str().expect("fixture path is UTF-8"),
        "--json",
    ]);
    assert!(explain.status.success());
    let explain_json: Value = serde_json::from_slice(&explain.stdout).unwrap();
    assert_eq!(explain_json["runtime"], "not_available_in_p1");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(8))]

    #[test]
    fn p1_property_registration_undo_is_exact(service in "[a-z]{1,16}") {
        let root = ScopeId::generate();
        let mut registry = ScopedRegistry::default();
        registry.add_root(root.clone()).unwrap();
        let old = registry
            .register(&root, service.clone(), PluginInstanceId::generate(), 1)
            .unwrap();
        prop_assert!(registry.undo(&old));
        let replacement = registry
            .register(&root, service.clone(), PluginInstanceId::generate(), 2)
            .unwrap();
        prop_assert!(!registry.undo(&old));
        prop_assert_eq!(registry.lookup(&root, &service).unwrap().token, replacement);
    }

    #[test]
    fn p1_property_duplicate_input_ids_are_idempotent(raw_text in "[A-Za-z0-9]{1,24}") {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (first, retry, count) = runtime.block_on(async {
            let temp = TempDir::new().unwrap();
            let store = writer(&temp).await;
            let service = SessionService::new(Arc::clone(&store));
            let session_id = SessionId::generate();
            let task_id = TaskId::generate();
            let input_id = InputId::generate();
            let first = service
                .admit_input(admission_request(
                    session_id.clone(),
                    task_id.clone(),
                    input_id.clone(),
                    raw_text.clone(),
                    1,
                ))
                .await
                .unwrap();
            let retry = service
                .admit_input(admission_request(session_id.clone(), task_id, input_id, raw_text, 99))
                .await
                .unwrap();
            let count = store.inbox_count(&session_id).await.unwrap();
            (first, retry, count)
        });
        prop_assert_eq!(first.event_id, retry.event_id);
        prop_assert!(retry.idempotent_replay);
        prop_assert_eq!(count, 1);
    }

    #[test]
    fn p1_property_snapshot_tail_matches_full_journal_fold(raw_text in "[A-Za-z0-9]{1,24}") {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (snapshot_state, snapshot_receipts, journal_state, journal_receipts) = runtime.block_on(async {
            let temp = TempDir::new().unwrap();
            let store = writer(&temp).await;
            let service = SessionService::new(Arc::clone(&store));
            let session_id = SessionId::generate();
            let task_id = TaskId::generate();
            service
                .admit_input(admission_request(
                    session_id.clone(),
                    task_id.clone(),
                    InputId::generate(),
                    raw_text,
                    1,
                ))
                .await
                .unwrap();
            service.write_snapshot(&session_id).await.unwrap();
            service
                .record_synthetic_receipt(RecordSyntheticReceiptRequest {
                    session_id: session_id.clone(),
                    task_id: task_id.clone(),
                    expected_sequence: 2,
                    receipt: receipt(task_id, 2),
                    artifact: None,
                })
                .await
                .unwrap();
            let snapshot_view = service.recover(&session_id).await.unwrap();
            store.corrupt_latest_snapshot_for_test(&session_id).await.unwrap();
            let journal_view = service.recover(&session_id).await.unwrap();
            (
                snapshot_view.working_state,
                snapshot_view.receipts,
                journal_view.working_state,
                journal_view.receipts,
            )
        });
        prop_assert_eq!(snapshot_state, journal_state);
        prop_assert_eq!(snapshot_receipts, journal_receipts);
    }
}
