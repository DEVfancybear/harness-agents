//! M1 — `SQLite` store, journal và recovery.
//!
//! Acceptance mapping for A01/A02/A05 lives in `tests/acceptance/milestones.json`
//! and points at the exact P1/P2 selectors that already prove them; this target
//! adds the M1 work-item oracles that did not exist before: the data directory
//! marker, the newer-schema refusal, the writer generation across a takeover,
//! artifact reachability, and the read-only recovery report.

use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::Arc,
};

use harness_session::{AdmitInputRequest, RecordSyntheticReceiptRequest, SessionService};
use harness_store_sqlite::{
    HostFence, SqliteStore, StoreFaultPlan, StoreFaultPoint, WriterOpenOptions,
};
use harness_types::{
    ContentHash, ErrorCode, EventEnvelope, EventId, HostId, InputId, P0_SCHEMA_VERSION,
    ProducerIdentity, ProjectId, SessionId, SourceAuthority, TaskId, ToolExecutionId,
    ToolExecutionReceipt, ToolIntentState, ToolOutcomeState, WorkspaceObservation,
};
use serde_json::Value;

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture_path(relative: &str) -> PathBuf {
    repository_root().join("tests/fixtures").join(relative)
}

fn run_ha(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ha"))
        .args(arguments)
        .output()
        .expect("compiled ha binary should execute")
}

fn output_text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("CLI output must be UTF-8")
}

fn workspace() -> WorkspaceObservation {
    WorkspaceObservation {
        project_id: ProjectId::generate(),
        worktree_id: "m1-test".to_owned(),
        base_commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        observed_fingerprint: ContentHash::from_bytes(b"m1 test workspace"),
    }
}

fn admission_request(
    session_id: SessionId,
    task_id: TaskId,
    input_id: InputId,
    raw_text: &str,
    expected_sequence: u64,
) -> AdmitInputRequest {
    AdmitInputRequest {
        session_id,
        task_id,
        input_id,
        expected_sequence,
        authority: SourceAuthority::User,
        raw_text: raw_text.to_owned(),
        workspace: workspace(),
        initial_plan_items: Vec::new(),
    }
}

fn receipt(task_id: TaskId, sequence: u64) -> ToolExecutionReceipt {
    ToolExecutionReceipt {
        schema_version: P0_SCHEMA_VERSION,
        tool_execution_id: ToolExecutionId::generate(),
        task_id,
        invocation_id: format!("m1-receipt-{sequence}"),
        input_hash: ContentHash::from_bytes(b"m1 synthetic receipt"),
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

async fn open_writer(data_dir: &Path) -> SqliteStore {
    SqliteStore::open_writer(WriterOpenOptions::new(data_dir, HostId::generate()))
        .await
        .expect("writer opens")
}

async fn close(store: Arc<SqliteStore>) {
    let store = Arc::try_unwrap(store).expect("store consumers must be released");
    store.close().await.expect("writer closes");
}

// ---------------------------------------------------------------------------
// M1-01 — migrations, marker and writer ownership
// ---------------------------------------------------------------------------

#[tokio::test]
async fn m1_01_data_directory_marker_is_versioned() {
    let temp = tempfile::tempdir().expect("temp dir");
    let data_dir = temp.path().join("data");
    let marker_path = data_dir.join("harness-data.json");

    // A read-only open of a directory with no database creates nothing.
    assert!(SqliteStore::open_read_only(&data_dir).await.is_err());
    assert!(
        !marker_path.exists(),
        "a read-only open must not create a marker"
    );

    // The first writable open writes the marker that names the directory.
    let store = Arc::new(open_writer(&data_dir).await);
    assert!(
        marker_path.is_file(),
        "the first writer open writes the marker"
    );
    let marker: Value =
        serde_json::from_str(&fs::read_to_string(&marker_path).expect("marker is readable"))
            .expect("marker is JSON");
    assert_eq!(marker["schema_version"], 1);
    assert_eq!(marker["kind"], "harness-data");
    assert_eq!(marker["store_schema_version"], 1);
    close(store).await;

    // Reopening never rewrites it.
    let written = fs::read(&marker_path).expect("marker bytes");
    let store = Arc::new(open_writer(&data_dir).await);
    close(store).await;
    assert_eq!(fs::read(&marker_path).expect("marker bytes"), written);

    // A newer format is refused and never overwritten.
    let newer = fs::read_to_string(fixture_path("m1/marker/newer-format.json"))
        .expect("the newer-format fixture exists");
    fs::write(&marker_path, &newer).expect("fixture marker");
    let error = SqliteStore::open_writer(WriterOpenOptions::new(&data_dir, HostId::generate()))
        .await
        .expect_err("a newer data directory format must be refused");
    assert_eq!(error.code(), ErrorCode::SchemaVersionMismatch);
    assert_eq!(
        fs::read_to_string(&marker_path).expect("marker survives"),
        newer,
        "a refused directory is never rewritten"
    );

    // A corrupt marker is refused as well, and also never overwritten.
    let corrupt = fs::read_to_string(fixture_path("m1/marker/corrupt.json"))
        .expect("the corrupt fixture exists");
    fs::write(&marker_path, &corrupt).expect("fixture marker");
    let error = SqliteStore::open_writer(WriterOpenOptions::new(&data_dir, HostId::generate()))
        .await
        .expect_err("a corrupt marker must be refused");
    assert_eq!(error.code(), ErrorCode::MigrationFailed);
    assert_eq!(
        fs::read_to_string(&marker_path).expect("marker survives"),
        corrupt
    );
}

#[tokio::test]
async fn m1_01_store_schema_newer_than_host_is_refused() {
    let temp = tempfile::tempdir().expect("temp dir");
    let data_dir = temp.path().join("data");
    let store = Arc::new(open_writer(&data_dir).await);
    close(store).await;

    // Simulate a database written by a host that knows a newer schema.
    let database = data_dir.join("harness.sqlite3");
    let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}", database.display()))
        .await
        .expect("fixture database opens");
    sqlx::query("INSERT INTO schema_migrations(version) VALUES (2)")
        .execute(&pool)
        .await
        .expect("newer migration row");
    pool.close().await;

    let error = SqliteStore::open_writer(WriterOpenOptions::new(&data_dir, HostId::generate()))
        .await
        .expect_err("a newer store schema must be refused for writes");
    assert_eq!(error.code(), ErrorCode::MigrationFailed);
    assert!(
        error.to_string().contains("newer than this host supports"),
        "{error}"
    );
}

#[tokio::test]
async fn m1_01_writer_generation_advances_and_stale_fence_is_rejected() {
    let temp = tempfile::tempdir().expect("temp dir");
    let old_host = HostId::generate();
    let mut child = Command::new(env!("CARGO_BIN_EXE_p1_fixture_host"))
        .args([
            "--data-dir",
            temp.path().to_str().expect("UTF-8 path"),
            "--host-id",
            old_host.as_ref(),
            "--mode",
            "hold-writer",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("writer fixture starts");
    let stdout = child.stdout.take().expect("stdout is piped");
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("readiness line");
    let ready: Value = serde_json::from_str(line.trim()).expect("readiness is JSON");
    assert_eq!(ready["ready"], "writer");
    let old_generation = ready["generation"].as_u64().expect("generation");

    // A second process cannot take the writer lock while the fixture holds it.
    let locked = SqliteStore::open_writer(WriterOpenOptions::new(temp.path(), HostId::generate()))
        .await
        .expect_err("the writer lock excludes a second process");
    assert_eq!(locked.code(), ErrorCode::WriterLocked);

    kill_child(child);

    // Takeover advances the generation, keeps the marker, and refuses the stale
    // token: only one active writer exists, and the old one cannot append.
    let store = Arc::new(open_writer(temp.path()).await);
    let new_generation = store.fence().expect("fence").generation;
    assert!(
        new_generation > old_generation,
        "takeover must advance the epoch: {old_generation} -> {new_generation}"
    );
    let marker: Value = serde_json::from_str(
        &fs::read_to_string(temp.path().join("harness-data.json")).expect("marker"),
    )
    .expect("marker is JSON");
    assert_eq!(marker["schema_version"], 1);
    let stale = HostFence {
        host_id: old_host,
        generation: old_generation,
    };
    let error = store
        .assert_current_fence(&stale)
        .await
        .expect_err("a stale generation must not append");
    assert_eq!(error.code(), ErrorCode::StaleWriter);
    close(store).await;
}

fn kill_child(mut child: Child) {
    child.kill().expect("the fixture child is killed");
    let _ = child.wait();
}

// ---------------------------------------------------------------------------
// M1-03 — artifacts and reachability
// ---------------------------------------------------------------------------

#[tokio::test]
async fn m1_03_orphan_artifact_is_only_reachable_by_reference() {
    let temp = tempfile::tempdir().expect("temp dir");
    let faults = StoreFaultPlan::with_point(StoreFaultPoint::BeforeReceiptCommit);
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
    service
        .admit_input(admission_request(
            session_id.clone(),
            task_id.clone(),
            InputId::generate(),
            "publish artifact evidence",
            1,
        ))
        .await
        .expect("admission");

    // Bytes are published (and flushed) before any reference exists.
    let orphan = store
        .publish_artifact(b"bytes published before the reference commit")
        .expect("artifact publishes");
    let orphan_path = store.paths().data_dir.join(&orphan.relative_path);
    assert!(orphan_path.is_file());
    assert!(
        store
            .referenced_artifact_ids()
            .await
            .expect("references")
            .is_empty(),
        "publishing bytes does not create a reference"
    );

    // The reference commit fails at the injected boundary.
    let mut synthetic = receipt(task_id.clone(), 2);
    synthetic.artifact_id = Some(orphan.artifact_id.clone());
    let error = service
        .record_synthetic_receipt(RecordSyntheticReceiptRequest {
            session_id: session_id.clone(),
            task_id: task_id.clone(),
            expected_sequence: 2,
            receipt: synthetic,
            artifact: Some(orphan.clone()),
        })
        .await
        .expect_err("the injected failure must reject the reference commit");
    assert_eq!(error.code(), ErrorCode::StorageWriteFailed);

    // The file survives and nothing references it: only reachability decides.
    assert!(orphan_path.is_file());
    assert!(
        store
            .referenced_artifact_ids()
            .await
            .expect("references")
            .is_empty()
    );
    store
        .remove_artifact_record(orphan.artifact_id.as_str())
        .await
        .expect("an unreferenced artifact record may be removed by a sweep");
    drop(service);
    close(store).await;

    // A referenced artifact is protected from the same sweep.
    let faults = StoreFaultPlan::default();
    let store = Arc::new(
        SqliteStore::open_writer(
            WriterOpenOptions::new(temp.path(), HostId::generate()).with_fault_plan(faults),
        )
        .await
        .expect("writer reopens"),
    );
    let service = SessionService::new(Arc::clone(&store));
    let kept = store
        .publish_artifact(b"bytes that a receipt will reference")
        .expect("artifact publishes");
    let mut synthetic = receipt(task_id.clone(), 2);
    synthetic.artifact_id = Some(kept.artifact_id.clone());
    service
        .record_synthetic_receipt(RecordSyntheticReceiptRequest {
            session_id: session_id.clone(),
            task_id: task_id.clone(),
            expected_sequence: 2,
            receipt: synthetic,
            artifact: Some(kept.clone()),
        })
        .await
        .expect("the reference commits");
    let referenced = store.referenced_artifact_ids().await.expect("references");
    assert!(
        referenced.iter().any(|id| id == kept.artifact_id.as_str()),
        "the committed reference must be visible: {referenced:?}"
    );
    let refused = store
        .remove_artifact_record(kept.artifact_id.as_str())
        .await
        .expect_err("a referenced artifact must not be swept");
    assert_eq!(refused.code(), ErrorCode::RetentionRefused);
    drop(service);
    close(store).await;
}

#[tokio::test]
async fn m1_03_checkpoint_commit_failure_leaves_the_journal_foldable() {
    let temp = tempfile::tempdir().expect("temp dir");
    // The fault is one-shot and armed before the writer opens: admission does
    // not touch this point, so the first snapshot is the one that fails.
    let faults = StoreFaultPlan::with_point(StoreFaultPoint::BeforeSnapshotCommit);
    let store = Arc::new(
        SqliteStore::open_writer(
            WriterOpenOptions::new(temp.path(), HostId::generate()).with_fault_plan(faults),
        )
        .await
        .expect("writer opens"),
    );
    let service = SessionService::new(Arc::clone(&store));
    let session_id = SessionId::generate();
    service
        .admit_input(admission_request(
            session_id.clone(),
            TaskId::generate(),
            InputId::generate(),
            "snapshot boundary",
            1,
        ))
        .await
        .expect("admission commits");

    let error = service
        .write_snapshot(&session_id)
        .await
        .expect_err("the injected failure must reject the checkpoint commit");
    assert_eq!(error.code(), ErrorCode::StorageWriteFailed);
    assert!(
        store
            .latest_snapshot(&session_id)
            .await
            .expect("snapshot lookup")
            .is_none(),
        "a failed checkpoint commit leaves no snapshot row"
    );

    // The journal is untouched: recovery still folds it without a checkpoint.
    let recovered = service.recover(&session_id).await.expect("recovery");
    assert_eq!(recovered.snapshot_sequence, None);
    assert_eq!(recovered.replayed_through_sequence, 1);
    assert_eq!(recovered.instruction_texts.len(), 1);
    drop(service);
    close(store).await;
}

// ---------------------------------------------------------------------------
// M1-04 — read-only recovery report
// ---------------------------------------------------------------------------

#[tokio::test]
#[allow(clippy::too_many_lines)] // one read-only report asserted end to end
async fn m1_04_status_reports_pending_work_and_blocking_reasons_without_a_provider() {
    let temp = tempfile::tempdir().expect("temp dir");
    let data_dir = temp.path().to_path_buf();
    let store = Arc::new(open_writer(&data_dir).await);
    let service = SessionService::new(Arc::clone(&store));
    let session_id = SessionId::generate();
    service
        .admit_input(admission_request(
            session_id.clone(),
            TaskId::generate(),
            InputId::generate(),
            "inspect this durable session",
            1,
        ))
        .await
        .expect("admission");
    drop(service);
    close(store).await;

    // Inspection needs no provider and no credential: it only reads.
    let status = run_ha(&[
        "status",
        "--data-dir",
        data_dir.to_str().expect("UTF-8 path"),
        "--session-id",
        session_id.as_str(),
        "--json",
    ]);
    assert!(
        status.status.success(),
        "status must not need a provider: {}",
        output_text(&status.stderr)
    );
    let report: Value = serde_json::from_slice(&status.stdout).expect("status is JSON");
    assert_eq!(report["pending_work"]["inbox_inputs"], 1);
    assert_eq!(report["pending_work"]["pending_tool_intents"], 0);
    assert_eq!(report["pending_work"]["pending_runtime_commands"], 0);
    assert_eq!(report["blocking"]["blocked"], false);
    assert_eq!(report["recovery"]["replayed_through_sequence"], 1);

    // An unknown critical event blocks the fold. Status still reports it with
    // the typed reason instead of failing, and resume still refuses.
    let store = Arc::new(open_writer(&data_dir).await);
    let payload = serde_json::json!({"unknown": true});
    let payload = payload.as_object().expect("object").clone();
    store
        .append_event_for_test(EventEnvelope {
            schema_version: P0_SCHEMA_VERSION,
            event_id: EventId::generate(),
            session_id: session_id.clone(),
            seq: 2,
            event_type: "future.critical.event".to_owned(),
            producer: ProducerIdentity {
                plugin_id: "fixture".to_owned(),
                implementation_version: "1".to_owned(),
            },
            authority: SourceAuthority::RuntimeObserved,
            correlation_id: None,
            causation_id: None,
            continuity_critical: true,
            payload: payload.clone(),
            payload_hash: ContentHash::from_canonical_json(&Value::Object(payload)).expect("hash"),
        })
        .await
        .expect("critical event appends");
    close(store).await;

    let status = run_ha(&[
        "status",
        "--data-dir",
        data_dir.to_str().expect("UTF-8 path"),
        "--session-id",
        session_id.as_str(),
        "--json",
    ]);
    assert!(
        status.status.success(),
        "a blocked session is still inspectable: {}",
        output_text(&status.stderr)
    );
    let report: Value = serde_json::from_slice(&status.stdout).expect("status is JSON");
    assert_eq!(report["blocking"]["blocked"], true);
    assert_eq!(report["blocking"]["reason"], "unknown_critical_event");
    assert!(report["recovery"].is_null());

    let resume = run_ha(&[
        "resume",
        "--data-dir",
        data_dir.to_str().expect("UTF-8 path"),
        "--session-id",
        session_id.as_str(),
    ]);
    assert!(!resume.status.success());
    assert!(output_text(&resume.stderr).starts_with("unknown_critical_event:"));

    // The M1 registry keeps the cross-target mapping for A01/A02/A05.
    let registry: Value = serde_json::from_str(
        &fs::read_to_string(repository_root().join("tests/acceptance/milestones.json"))
            .expect("milestone registry"),
    )
    .expect("registry is JSON");
    let m1 = registry["milestones"]
        .as_array()
        .expect("milestones")
        .iter()
        .find(|milestone| milestone["id"] == "M1")
        .expect("M1 is registered");
    assert_eq!(m1["prerequisites"][0], "M0");
    let a01 = registry["acceptance_cases"]
        .as_array()
        .expect("cases")
        .iter()
        .find(|case| case["id"] == "A01")
        .expect("A01");
    assert_eq!(a01["status"], "implemented");
    assert!(
        a01["selectors"]
            .as_array()
            .expect("selectors")
            .iter()
            .any(|selector| selector.as_str().is_some_and(|s| s.contains("p1_c01"))),
        "A01 must map to the P1 child-kill selector"
    );
}
