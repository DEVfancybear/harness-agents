//! M12 acceptance: the strict execution backend, and the boundary it refuses
//! to pretend it has.
//!
//! Everything here is real: a `SQLite` store with writer fencing, the P3 tool
//! gate (policy, approval, intent, receipt), a disposable Git workspace, and the
//! `m12_probe_child` fixture as a real process. The capability matrix is
//! *measured* by the same probe a deployment would run — nothing in this file
//! invents a verdict, and no test asserts a capability the probe did not
//! observe.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use harness_session::{AdmitInputRequest, SessionService};
use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
use harness_tools::{
    ARTIFACT_EXPORT_SCHEMA_VERSION, CAPABILITY_MATRIX_SCHEMA_VERSION, Capability,
    CapabilityEvidence, CapabilityFinding, CapabilityMatrix, CapabilityProbe, CapabilityVerdict,
    CodingToolAction, EXECUTION_PLAN_SCHEMA_VERSION, ExecutionPlan, ExportProvenance, HostIdentity,
    IsolationMode, LEASE_GRACE_MS, LeaseOutcome, LeaseOwner, LeaseRequest,
    PROCESS_ENVIRONMENT_ALLOWLIST, PROCESS_OUTPUT_PAGE_MAX_BYTES, ProbeChild, StrictProfile,
    ToolExecutionService, ToolOutput, ToolRequest, export_artifact, export_destination,
    observe_workspace, reconcile_backend_leases,
};
use harness_types::{
    ContentHash, ErrorCode, HostId, InputId, SessionId, SourceAuthority, TaskId, ToolIntentState,
    ToolOutcomeState,
};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

struct Bench {
    temp: tempfile::TempDir,
    data_dir: PathBuf,
    workspace: PathBuf,
    project_id: harness_types::ProjectId,
}

impl Bench {
    async fn open_store(&self) -> Arc<SqliteStore> {
        Arc::new(
            SqliteStore::open_writer(WriterOpenOptions::new(
                self.data_dir.clone(),
                HostId::generate(),
            ))
            .await
            .expect("store opens"),
        )
    }
}

fn bench() -> Bench {
    let temp = tempfile::tempdir().expect("temp root");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(workspace.join("src")).expect("source dir");
    std::fs::write(workspace.join("src").join("parser.txt"), "BUG parser\r\n")
        .expect("fixture parser");
    for arguments in [
        vec!["init"],
        vec!["config", "user.email", "m12@example.invalid"],
        vec!["config", "user.name", "M12 Fixture"],
        vec!["add", "."],
        vec!["commit", "-m", "fixture baseline"],
    ] {
        let output = Command::new("git")
            .args(&arguments)
            .current_dir(&workspace)
            .output()
            .expect("git starts");
        assert!(
            output.status.success(),
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let data_dir = temp.path().join("data");
    Bench {
        temp,
        data_dir,
        workspace,
        project_id: harness_types::ProjectId::generate(),
    }
}

async fn close(store: Arc<SqliteStore>) {
    Arc::try_unwrap(store)
        .expect("store consumers released")
        .close()
        .await
        .expect("store closes");
}

async fn admit(store: &Arc<SqliteStore>, bench: &Bench) -> (SessionId, TaskId) {
    let session_id = SessionId::generate();
    let task_id = TaskId::generate();
    SessionService::new(Arc::clone(store))
        .admit_input(AdmitInputRequest {
            session_id: session_id.clone(),
            task_id: task_id.clone(),
            input_id: InputId::generate(),
            expected_sequence: 1,
            authority: SourceAuthority::User,
            raw_text: "measure the execution boundary".to_owned(),
            workspace: observe_workspace(bench.project_id.clone(), &bench.workspace)
                .expect("observation"),
            initial_plan_items: Vec::new(),
        })
        .await
        .expect("input admitted");
    (session_id, task_id)
}

fn probe_child() -> ProbeChild {
    ProbeChild::new(
        PathBuf::from(env!("CARGO_BIN_EXE_m12_probe_child")),
        Vec::new(),
    )
}

/// The version this platform reports about itself, asked for directly.
fn platform_version() -> String {
    #[cfg(windows)]
    let (executable, args) = ("cmd", vec!["/C", "ver"]);
    #[cfg(not(windows))]
    let (executable, args) = ("uname", vec!["-sr"]);
    let output = Command::new(executable)
        .args(args)
        .output()
        .expect("platform answers");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A `RunProcess` action at the requested isolation that writes a marker file,
/// so a refusal can be told apart from an execution by looking at the disk.
fn marker_action(probe: &ProbeChild, marker: &Path, isolation: IsolationMode) -> CodingToolAction {
    CodingToolAction::RunProcess {
        executable: probe.executable().to_string_lossy().into_owned(),
        args: vec![
            "write".to_owned(),
            marker.to_string_lossy().into_owned(),
            "executed".to_owned(),
        ],
        timeout_ms: 20_000,
        isolation,
        env: Vec::new(),
    }
}

fn denied(view: &harness_tools::ToolExecutionView) -> (String, String) {
    let receipt = view.receipt.as_ref().expect("a denial is receipt-backed");
    assert_eq!(receipt.outcome_state, ToolOutcomeState::Denied);
    assert_eq!(receipt.intent_state, ToolIntentState::Denied);
    let ToolOutput::Denied { code, reason } = &view.output else {
        panic!("a refusal must be a typed denial, got {:?}", view.output);
    };
    (code.clone(), reason.clone())
}

// ---------------------------------------------------------------------------
// M12-01 — the capability matrix is measured, and strict fails closed
// ---------------------------------------------------------------------------

/// A36 setup, part one: the matrix describes *this* host, because the probes
/// ran on it.
#[tokio::test]
async fn m12_01_capability_matrix_is_measured_on_this_host() {
    let temp = tempfile::tempdir().expect("temp root");
    let probe = CapabilityProbe::new(temp.path(), probe_child());
    let matrix = probe.run().await.expect("the probes run");
    matrix.validate().expect("a complete matrix with evidence");

    assert_eq!(matrix.schema_version, CAPABILITY_MATRIX_SCHEMA_VERSION);
    assert_eq!(matrix.host.os, std::env::consts::OS);
    assert_eq!(matrix.host.arch, std::env::consts::ARCH);
    assert_eq!(matrix.host.backend, harness_tools::CONTAINMENT_BACKEND);
    assert_eq!(
        matrix.host.backend_version,
        harness_tools::CONTAINMENT_BACKEND_VERSION
    );

    // The identity is read from the machine, not remembered: the platform's own
    // answer has to be inside what the matrix recorded.
    let reported = platform_version();
    let reported = reported
        .split_once("[Version ")
        .and_then(|(_, rest)| rest.split_once(']'))
        .map_or(reported.as_str(), |(value, _)| value);
    assert!(
        matrix.host.os_version.contains(reported.trim()),
        "matrix says {} but this platform reports {reported}",
        matrix.host.os_version
    );

    // Weakest claims first: these are this workspace's own code, not the OS.
    for capability in [
        Capability::EnvironmentAllowlist,
        Capability::DeadlineEnforced,
        Capability::OutputBounds,
        Capability::ProcessContainment,
        Capability::ProcessTreeKill,
    ] {
        assert_eq!(
            matrix.verdict(capability),
            Some(CapabilityVerdict::Enforced),
            "{} must be enforced here: {:?}",
            capability.as_str(),
            matrix.evidence(capability).map(|item| &item.observation)
        );
    }

    // Every unsupported verdict carries a positive observation, never silence.
    for capability in matrix.unsupported() {
        let evidence = matrix.evidence(capability).expect("evidence");
        assert!(
            evidence.observation.len() > 40,
            "{} is unsupported without an observation to check: {}",
            capability.as_str(),
            evidence.observation
        );
    }

    // The two controls: a probe that cannot lose proves nothing.
    let boundary = probe
        .boundary_break_control()
        .await
        .expect("the boundary-break control runs");
    assert!(
        boundary.escaped,
        "with the job object omitted the descendants must survive; the control observed otherwise: {}",
        boundary.detail
    );
    assert!(
        probe
            .environment_inheritance_control()
            .await
            .expect("the environment control runs"),
        "without the allowlist the canary must reach the child"
    );
}

/// A36 setup, part two: the refusal is typed, names what is missing, and
/// happens before any process exists.
#[tokio::test]
async fn m12_01_a_strict_request_is_refused_before_anything_runs() {
    let bench = bench();
    let store = bench.open_store().await;
    let (session, task) = admit(&store, &bench).await;
    let marker = bench.temp.path().join("strict-marker.txt");
    let child = probe_child();

    // (1) Nothing was measured: the host must refuse rather than assume.
    let unmeasured = ToolExecutionService::new(Arc::clone(&store));
    let prepared = unmeasured
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.m12",
            &bench.workspace,
            marker_action(&child, &marker, IsolationMode::Strict),
        ))
        .await
        .expect("a strict action prepares; refusal is an execution gate");
    let grant = unmeasured.approve(&prepared).await.expect("approval");
    let view = unmeasured
        .execute(prepared, Some(grant))
        .await
        .expect("a refusal is a settled denial");
    let (code, reason) = denied(&view);
    assert_eq!(code, ErrorCode::StrictIsolationUnavailable.as_str());
    assert!(
        reason.contains("not been measured"),
        "the refusal has to say the host was not measured: {reason}"
    );

    // (2) A measured matrix: the refusal names the capabilities this host does
    // not have instead of a hard-coded sentence.
    let temp = tempfile::tempdir().expect("probe root");
    let matrix = Arc::new(
        CapabilityProbe::new(temp.path(), child.clone())
            .run()
            .await
            .expect("the probes run"),
    );
    let tools =
        ToolExecutionService::new(Arc::clone(&store)).with_capability_matrix(Arc::clone(&matrix));
    let prepared = tools
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.m12",
            &bench.workspace,
            marker_action(&child, &marker, IsolationMode::Strict),
        ))
        .await
        .expect("a strict action prepares");
    let grant = tools.approve(&prepared).await.expect("approval");
    let view = tools.execute(prepared, Some(grant)).await.expect("denial");
    let (code, reason) = denied(&view);
    assert_eq!(code, ErrorCode::StrictIsolationUnavailable.as_str());
    for capability in matrix.missing_for(StrictProfile::Full) {
        assert!(
            reason.contains(capability.as_str()),
            "the refusal must name {}: {reason}",
            capability.as_str()
        );
    }

    // (3) Fail closed even against a matrix that claims full confinement: this
    // revision has no confinement adapter, and a strict request must not be
    // served as containment wearing a green verdict.
    let optimistic = Arc::new(CapabilityMatrix::new(
        HostIdentity::observed("fabricated"),
        Capability::ALL
            .into_iter()
            .map(|capability| {
                CapabilityFinding::enforced(
                    capability,
                    CapabilityEvidence::new("P-FABRICATED", "a claim with no probe", "enforced"),
                )
            })
            .collect(),
    ));
    let optimistic_tools =
        ToolExecutionService::new(Arc::clone(&store)).with_capability_matrix(optimistic);
    let prepared = optimistic_tools
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.m12",
            &bench.workspace,
            marker_action(&child, &marker, IsolationMode::Strict),
        ))
        .await
        .expect("a strict action prepares");
    let grant = optimistic_tools.approve(&prepared).await.expect("approval");
    let view = optimistic_tools
        .execute(prepared, Some(grant))
        .await
        .expect("denial");
    let (code, reason) = denied(&view);
    assert_eq!(code, ErrorCode::StrictIsolationUnavailable.as_str());
    assert!(
        reason.contains("no confinement adapter"),
        "the refusal has to name the missing adapter: {reason}"
    );

    // Nothing above may have started a process.
    assert!(
        !marker.exists(),
        "a refused strict request must not run anything"
    );

    drop(optimistic_tools);
    drop(tools);
    drop(unmeasured);
    close(store).await;
}

// ---------------------------------------------------------------------------
// M12-02 — the plan names what the backend controls, and the export is bounded
// ---------------------------------------------------------------------------

/// A plan that cannot name its unmapped requests is a plan that hides them.
#[tokio::test]
async fn m12_02_the_plan_names_the_controls_it_does_not_have() {
    let temp = tempfile::tempdir().expect("probe root");
    let matrix = CapabilityProbe::new(temp.path(), probe_child())
        .run()
        .await
        .expect("the probes run");
    let root = temp.path().join("workspace");
    std::fs::create_dir_all(&root).expect("run root");
    let action = CodingToolAction::RunShell {
        command: "echo hi".to_owned(),
        timeout_ms: 5_000,
        isolation: IsolationMode::BestEffort,
        env: Vec::new(),
    };
    let plan = ExecutionPlan::for_process_action(
        &action,
        &root,
        &matrix,
        StrictProfile::Containment,
        5_000,
        1_048_576,
    );
    assert_eq!(plan.schema_version, EXECUTION_PLAN_SCHEMA_VERSION);
    assert_eq!(plan.backend, matrix.host.backend);
    assert_eq!(plan.enforced, matrix.enforced());
    assert_eq!(plan.not_claimed, matrix.unsupported());
    assert_eq!(plan.deadline_ms, 5_000);
    assert_eq!(plan.output_quota_bytes, 1_048_576);
    assert!(plan.environment.granted.is_empty());
    assert!(
        plan.environment
            .allowlisted
            .iter()
            .any(|name| name == "PATH"),
        "the plan records the environment names, not the values"
    );
    assert!(
        plan.grants
            .iter()
            .any(|grant| grant.path.replace('\\', "/").ends_with("workspace")),
        "the run root is the one granted scope"
    );

    // Containment is servable here; full confinement is not, and the plan says
    // exactly which controls are missing rather than only that something is.
    assert!(
        !plan.promises_uncontrolled(),
        "containment requires nothing this host lacks"
    );
    assert!(plan.refusal(&matrix).is_none());

    let strict = ExecutionPlan::for_process_action(
        &action,
        &root,
        &matrix,
        StrictProfile::Full,
        5_000,
        1_048_576,
    );
    assert!(strict.promises_uncontrolled());
    let refusal = strict
        .refusal(&matrix)
        .expect("full confinement has no control here");
    assert_eq!(refusal.code(), ErrorCode::StrictIsolationUnavailable);
    for control in [
        "filesystem_write_scope",
        "network_egress",
        "credential_socket",
    ] {
        assert!(
            refusal.message().contains(control),
            "the refusal must name the missing control {control}: {}",
            refusal.message()
        );
    }

    // The granted secret references are named, never valued.
    let with_secret = CodingToolAction::RunShell {
        command: "echo hi".to_owned(),
        timeout_ms: 5_000,
        isolation: IsolationMode::BestEffort,
        env: vec![harness_tools::EnvBinding {
            name: "TOKEN".to_owned(),
            reference: "secret://probe".to_owned(),
        }],
    };
    let plan = ExecutionPlan::for_process_action(
        &with_secret,
        &root,
        &matrix,
        StrictProfile::Containment,
        5_000,
        1_048_576,
    );
    assert_eq!(plan.environment.granted, vec!["secret://probe".to_owned()]);
}

/// A normal command has to keep working inside the backend: a boundary that
/// breaks ordinary work is not a boundary, it is an outage.
#[tokio::test]
async fn m12_02_a_normal_command_succeeds_inside_the_backend() {
    let bench = bench();
    let store = bench.open_store().await;
    let (session, task) = admit(&store, &bench).await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let child = probe_child();

    let prepared = tools
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.m12",
            &bench.workspace,
            CodingToolAction::RunProcess {
                executable: child.executable().to_string_lossy().into_owned(),
                args: vec!["env".to_owned()],
                timeout_ms: 20_000,
                isolation: IsolationMode::BestEffort,
                env: Vec::new(),
            },
        ))
        .await
        .expect("a normal process action prepares");
    let grant = tools.approve(&prepared).await.expect("approval");
    let view = tools.execute(prepared, Some(grant)).await.expect("runs");
    let ToolOutput::Process {
        exit_code,
        timed_out,
        tree_cleanup_confirmed,
        stdout,
        artifact_id,
        ..
    } = &view.output
    else {
        panic!("a normal command must settle: {:?}", view.output);
    };
    assert_eq!(*exit_code, Some(0));
    assert!(!timed_out);
    assert!(tree_cleanup_confirmed);
    assert!(artifact_id.is_some(), "the capture is published");

    // The environment the child printed contains nothing outside the allowlist.
    let mut seen = 0_usize;
    for line in stdout.lines() {
        let Some((name, _)) = line.split_once('=') else {
            continue;
        };
        seen += 1;
        assert!(
            PROCESS_ENVIRONMENT_ALLOWLIST.contains(&name),
            "the child saw {name}, which is not in the allowlist"
        );
    }
    assert!(seen > 0, "the fixture printed its environment");

    drop(tools);
    close(store).await;
}

/// Export is digest-bound and cannot be talked out of the export root.
#[tokio::test]
async fn m12_02_artifact_export_is_bounded_and_digest_bound() {
    let bench = bench();
    let store = bench.open_store().await;
    let (session, task) = admit(&store, &bench).await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let child = probe_child();

    // A real capture, published by the same gate that writes the receipt: the
    // artifact row exists because an execution produced it, not because a test
    // inserted it.
    let prepared = tools
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.m12",
            &bench.workspace,
            CodingToolAction::RunProcess {
                executable: child.executable().to_string_lossy().into_owned(),
                args: vec!["flood".to_owned(), "4096".to_owned()],
                timeout_ms: 20_000,
                isolation: IsolationMode::BestEffort,
                env: Vec::new(),
            },
        ))
        .await
        .expect("prepares");
    let grant = tools.approve(&prepared).await.expect("approval");
    let view = tools.execute(prepared, Some(grant)).await.expect("runs");
    let ToolOutput::Process {
        artifact_id,
        capture_hash,
        ..
    } = &view.output
    else {
        panic!("a process capture publishes an artifact: {:?}", view.output);
    };
    let artifact_id = artifact_id.clone().expect("an artifact id");
    let capture_hash = capture_hash.clone().expect("a capture digest");

    let export_root = bench.temp.path().join("export");
    std::fs::create_dir_all(&export_root).expect("export root");
    let provenance = ExportProvenance {
        backend: harness_tools::CONTAINMENT_BACKEND.to_owned(),
        backend_version: harness_tools::CONTAINMENT_BACKEND_VERSION.to_owned(),
        profile: StrictProfile::Containment.as_str().to_owned(),
        enforced: vec![Capability::ProcessContainment.as_str().to_owned()],
        not_claimed: vec![Capability::NetworkEgressDenial.as_str().to_owned()],
        lease_id: None,
    };
    let export = export_artifact(
        &store,
        &artifact_id,
        &export_root,
        Some(provenance),
        PROCESS_OUTPUT_PAGE_MAX_BYTES as usize,
    )
    .await
    .expect("export succeeds");
    assert_eq!(export.schema_version, ARTIFACT_EXPORT_SCHEMA_VERSION);
    assert_eq!(export.recorded_digest, capture_hash.as_str());
    assert_eq!(export.exported_digest, capture_hash.as_str());
    assert!(export.byte_len > 0);
    let written = std::fs::read(export_root.join(&export.relative_path)).expect("bytes on disk");
    assert_eq!(
        ContentHash::from_bytes(&written).as_str(),
        capture_hash.as_str()
    );
    assert_eq!(
        export.provenance.as_ref().map(|item| item.profile.as_str()),
        Some("containment"),
        "the export records what the execution was and was not promised"
    );

    // The destination is derived from the id and resolved inside the root.
    let destination = export_destination(&export_root, &artifact_id).expect("a valid id resolves");
    assert!(destination.starts_with(&export_root));
    assert!(
        export_destination(&export_root, "../escape.bin").is_err(),
        "an id is not a path"
    );

    // Tamper with the stored bytes, keeping the recorded length: the export has
    // to notice, or the digest in the receipt means nothing.
    let artifact_path = bench
        .data_dir
        .join("artifacts")
        .join(format!("{artifact_id}.bin"));
    let mut tampered = written.clone();
    tampered[0] = if tampered[0] == b'x' { b'y' } else { b'x' };
    std::fs::write(&artifact_path, &tampered).expect("tamper");
    let refusal = export_artifact(
        &store,
        &artifact_id,
        &export_root,
        None,
        PROCESS_OUTPUT_PAGE_MAX_BYTES as usize,
    )
    .await
    .expect_err("tampered bytes must not be exported");
    assert_eq!(refusal.code(), ErrorCode::ArtifactWriteFailed);
    assert!(
        refusal.message().contains("hash to"),
        "the refusal has to say the digest disagreed: {}",
        refusal.message()
    );

    drop(tools);
    close(store).await;
}

// ---------------------------------------------------------------------------
// M12-03 — leases: written before exposure, released once, and never taken
// from a live owner
// ---------------------------------------------------------------------------

/// The lifecycle: a row exists before the process, moves once, and settles once.
#[tokio::test]
async fn m12_03_a_lease_is_written_before_the_process_and_settled_once() {
    let bench = bench();
    let store = bench.open_store().await;
    let matrix = CapabilityMatrix::new(
        HostIdentity::observed("test"),
        Capability::ALL
            .into_iter()
            .map(|capability| {
                CapabilityFinding::enforced(
                    capability,
                    CapabilityEvidence::new("P-TEST", "a verdict for this test", "enforced"),
                )
            })
            .collect(),
    );
    let plan = ExecutionPlan::for_process_action(
        &CodingToolAction::RunShell {
            command: "echo hi".to_owned(),
            timeout_ms: 1_000,
            isolation: IsolationMode::BestEffort,
            env: Vec::new(),
        },
        &bench.workspace,
        &matrix,
        StrictProfile::Containment,
        1_000,
        65_536,
    );
    let lock_root = bench.data_dir.join("leases");
    let request = LeaseRequest::from_plan(
        &plan,
        "session.m12",
        "task.m12",
        "execution.m12-03",
        &lock_root,
    );
    let owner = LeaseOwner::open(&store, &request)
        .await
        .expect("lease opens");
    let lease_id = owner.lease_id().to_owned();
    let stored = store
        .backend_lease(&lease_id)
        .await
        .expect("read")
        .expect("the row exists before any process does");
    assert_eq!(stored.state, "acquiring");
    assert!(stored.pid.is_none(), "no process exists yet");
    assert!(stored.released_at_unix_ms.is_none());
    assert_eq!(stored.backend, plan.backend);
    assert_eq!(stored.profile, "containment");
    assert!(
        owner.lock_path().is_file(),
        "the lock file is the ownership proof"
    );

    // One execution cannot hold two live leases: that pair is the resource.
    let second = LeaseOwner::open(&store, &request).await;
    assert!(
        second.is_err(),
        "a second lease for the same execution must be refused"
    );

    owner.acquired(&store).await.expect("acquired");
    assert_eq!(
        store
            .backend_lease(&lease_id)
            .await
            .expect("read")
            .expect("row")
            .state,
        "acquired"
    );

    assert!(
        owner
            .release(&store, Some(("artifact_fixture", "sha256:fixture")))
            .await
            .expect("released"),
        "the first release settles the lease"
    );
    let released = store
        .backend_lease(&lease_id)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(released.state, "released");
    assert!(released.released_at_unix_ms.is_some());
    assert_eq!(released.artifact_id.as_deref(), Some("artifact_fixture"));

    // Repeating the settling statement is a no-op that says so.
    assert!(
        !store
            .release_backend_lease(&lease_id, "released", 1, None, None, None)
            .await
            .expect("second release is not an error"),
        "a settled lease is never settled twice"
    );

    close(store).await;
}

/// A live owner is never destroyed; an orphan is only settled once its lock is
/// free.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one recovery story: live owner, orphan, second pass, crash during acquire
async fn m12_03_recovery_protects_a_live_owner_and_settles_an_orphan() {
    let bench = bench();
    let store = bench.open_store().await;
    let lock_root = bench.data_dir.join("leases");
    let plan = ExecutionPlan::for_process_action(
        &CodingToolAction::RunShell {
            command: "echo hi".to_owned(),
            timeout_ms: 1_000,
            isolation: IsolationMode::BestEffort,
            env: Vec::new(),
        },
        &bench.workspace,
        &CapabilityMatrix::new(
            HostIdentity::observed("test"),
            Capability::ALL
                .into_iter()
                .map(|capability| {
                    CapabilityFinding::enforced(
                        capability,
                        CapabilityEvidence::new("P-TEST", "a verdict for this test", "enforced"),
                    )
                })
                .collect(),
        ),
        StrictProfile::Containment,
        1_000,
        65_536,
    );

    // A live owner: this test process holds the lock.
    let live = LeaseOwner::open(
        &store,
        &LeaseRequest::from_plan(&plan, "s", "t", "execution.live", &lock_root),
    )
    .await
    .expect("live lease");
    live.acquired(&store).await.expect("acquired");
    let live_id = live.lease_id().to_owned();

    // An orphan: the owner was killed, so its lock went with the process while
    // the row stayed behind. Dropping the owner without releasing is exactly
    // that, and it is what a crash looks like from the store's side.
    let orphan = LeaseOwner::open(
        &store,
        &LeaseRequest::from_plan(&plan, "s", "t", "execution.orphan", &lock_root),
    )
    .await
    .expect("orphan lease");
    orphan.acquired(&store).await.expect("acquired");
    let orphan_id = orphan.lease_id().to_owned();
    drop(orphan);

    // Everything is old enough to be a candidate: the grace window only narrows
    // the field, and the lock is what decides.
    // The grace window is a filter, not a decision: a lease written a moment ago
    // is not even a candidate.
    let fresh = reconcile_backend_leases(&store, 1, 1_800_000_000_000)
        .await
        .expect("reconcile runs");
    assert!(
        fresh.outcome_for(&live_id).is_none() && fresh.outcome_for(&orphan_id).is_none(),
        "a lease inside the {LEASE_GRACE_MS} ms grace window is not a candidate: {fresh:?}"
    );

    let report = reconcile_backend_leases(&store, u64::MAX, 1_800_000_000_000)
        .await
        .expect("reconcile runs");
    assert_eq!(
        report.outcome_for(&live_id),
        Some(LeaseOutcome::StillOwned),
        "a live owner must be left alone: {report:?}"
    );
    assert_eq!(
        report.outcome_for(&orphan_id),
        Some(LeaseOutcome::Recovered)
    );

    let live_row = store
        .backend_lease(&live_id)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(live_row.state, "acquired");
    assert!(
        live_row.released_at_unix_ms.is_none(),
        "reconciliation touched a live owner's lease"
    );

    let orphan_row = store
        .backend_lease(&orphan_id)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(orphan_row.state, "recovered");
    assert!(orphan_row.released_at_unix_ms.is_some());
    let recovery = orphan_row.recovery_json.expect("a recovery note");
    assert!(
        recovery.contains("terminated when its job handle closed"),
        "the note has to say what was assumed: {recovery}"
    );

    // A second pass has nothing left to do: the settled row is not a candidate.
    let again = reconcile_backend_leases(&store, u64::MAX, 1_800_000_000_001)
        .await
        .expect("reconcile runs");
    assert!(again.outcome_for(&orphan_id).is_none());
    assert_eq!(again.outcome_for(&live_id), Some(LeaseOutcome::StillOwned));

    // A crash during acquire is settled as nothing-to-clean, not as a cleanup of
    // a resource that never existed.
    let acquiring = LeaseOwner::open(
        &store,
        &LeaseRequest::from_plan(&plan, "s", "t", "execution.acquiring", &lock_root),
    )
    .await
    .expect("acquiring lease");
    let acquiring_id = acquiring.lease_id().to_owned();
    drop(acquiring);
    let report = reconcile_backend_leases(&store, u64::MAX, 1_800_000_000_002)
        .await
        .expect("reconcile runs");
    assert_eq!(
        report.outcome_for(&acquiring_id),
        Some(LeaseOutcome::Recovered)
    );
    let note = store
        .backend_lease(&acquiring_id)
        .await
        .expect("read")
        .expect("row")
        .recovery_json
        .expect("a recovery note");
    assert!(
        note.contains("nothing_to_clean"),
        "a lease that never exposed a resource says so: {note}"
    );

    live.release(&store, None).await.expect("live release");
    close(store).await;
}

/// A real execution writes its lease, records the artifact, and settles.
#[tokio::test]
async fn m12_03_a_real_execution_records_its_lease_and_artifact() {
    let bench = bench();
    let store = bench.open_store().await;
    let (session, task) = admit(&store, &bench).await;
    // The provenance a lease records is what the host was *measured* to
    // enforce, so this execution runs against a measured matrix.
    let probe_root = tempfile::tempdir().expect("probe root");
    let child = probe_child();
    let matrix = Arc::new(
        CapabilityProbe::new(probe_root.path(), child.clone())
            .run()
            .await
            .expect("the probes run"),
    );
    let tools =
        ToolExecutionService::new(Arc::clone(&store)).with_capability_matrix(Arc::clone(&matrix));
    let execution_id = {
        let prepared = tools
            .prepare(ToolRequest::new(
                session.clone(),
                task.clone(),
                "actor.m12",
                &bench.workspace,
                CodingToolAction::RunProcess {
                    executable: child.executable().to_string_lossy().into_owned(),
                    args: vec![
                        "write".to_owned(),
                        "lease-marker.txt".to_owned(),
                        "ok".to_owned(),
                    ],
                    timeout_ms: 20_000,
                    isolation: IsolationMode::BestEffort,
                    env: Vec::new(),
                },
            ))
            .await
            .expect("prepares");
        let grant = tools.approve(&prepared).await.expect("approval");
        let view = tools.execute(prepared, Some(grant)).await.expect("runs");
        assert!(
            matches!(view.output, ToolOutput::Process { .. }),
            "the fixture writes a file inside the workspace: {:?}",
            view.output
        );
        view.receipt
            .as_ref()
            .expect("a receipt")
            .tool_execution_id
            .as_str()
            .to_owned()
    };

    let host_id = store.fence().expect("fence").host_id.as_str().to_owned();
    let lease = store
        .backend_lease_for_execution(&host_id, &execution_id)
        .await
        .expect("read")
        .expect("every process execution writes a lease");
    assert_eq!(lease.state, "released");
    assert!(lease.released_at_unix_ms.is_some());
    assert_eq!(lease.profile, "containment");
    assert!(
        lease.artifact_digest.is_some(),
        "the lease records the digest an export must check"
    );
    let enforced: Vec<String> = serde_json::from_str(&lease.enforced_json).expect("enforced list");
    let not_claimed: Vec<String> =
        serde_json::from_str(&lease.not_claimed_json).expect("not-claimed list");
    assert!(enforced.contains(&"process_containment".to_owned()));
    assert!(
        not_claimed.contains(&"network_egress_denial".to_owned()),
        "the record says what was not promised: {not_claimed:?}"
    );
    assert!(std::path::Path::new(&lease.lock_path).is_file());

    drop(tools);
    close(store).await;
}

// ---------------------------------------------------------------------------
// A36 — strict backend proof
// ---------------------------------------------------------------------------

/// The support matrix this repository publishes for the platform the test is
/// running on, as key/value pairs.
fn support_matrix_for_this_platform() -> std::collections::BTreeMap<String, String> {
    const DOC: &str = include_str!("../../../docs/support/STRICT_EXECUTION_SUPPORT.vi.md");
    let mut blocks = Vec::new();
    let mut current: Option<std::collections::BTreeMap<String, String>> = None;
    for line in DOC.lines() {
        let line = line.trim();
        if line == "<!-- support-matrix:begin -->" {
            current = Some(std::collections::BTreeMap::new());
            continue;
        }
        if line == "<!-- support-matrix:end -->" {
            if let Some(block) = current.take() {
                blocks.push(block);
            }
            continue;
        }
        if let Some(block) = current.as_mut()
            && let Some((key, value)) = line.split_once('=')
        {
            block.insert(key.trim().to_owned(), value.trim().to_owned());
        }
    }
    blocks
        .into_iter()
        .find(|block| {
            block.get("platform").map(String::as_str) == Some(std::env::consts::OS)
        })
        .unwrap_or_else(|| {
            panic!(
                "the support matrix has no block for {}; an unmeasured platform is not a supported one",
                std::env::consts::OS
            )
        })
}

/// A36: the whole strict-backend story, on the real backend, with real canaries.
#[allow(clippy::too_many_lines)] // one acceptance case, told in order
#[tokio::test]
async fn a36_strict_confinement() {
    let bench = bench();
    let store = bench.open_store().await;
    let (session, task) = admit(&store, &bench).await;
    let probe_root = tempfile::tempdir().expect("probe root");
    let child = probe_child();
    let probe = CapabilityProbe::new(probe_root.path(), child.clone());
    let matrix = probe
        .run()
        .await
        .expect("the probes run on the real backend");
    matrix.validate().expect("every verdict carries evidence");

    // (1) The support matrix has to match what was measured here, not what was
    // hoped for: this is the clause A36 calls "support matrix matches real
    // environment", and it is checked rather than asserted in prose.
    let published = support_matrix_for_this_platform();
    assert_eq!(
        published.get("measured").map(String::as_str),
        Some("true"),
        "this platform is documented as unmeasured; a probe ran here, so record its result"
    );
    assert_eq!(
        published.get("backend").map(String::as_str),
        Some(matrix.host.backend.as_str())
    );
    assert_eq!(
        published.get("backend_version").map(String::as_str),
        Some(matrix.host.backend_version.as_str())
    );
    for capability in Capability::ALL {
        let measured = match matrix.verdict(capability) {
            Some(CapabilityVerdict::Enforced) => "enforced",
            Some(CapabilityVerdict::Unsupported) => "unsupported",
            None => panic!("{} has no verdict", capability.as_str()),
        };
        assert_eq!(
            published.get(capability.as_str()).map(String::as_str),
            Some(measured),
            "the published matrix disagrees with the measurement for {}",
            capability.as_str()
        );
    }
    let full_refused = StrictProfile::Full.refusal(&matrix).is_some();
    assert_eq!(
        published.get("strict_profile_full").map(String::as_str),
        Some(if full_refused { "refused" } else { "served" }),
        "the published profile line disagrees with the measurement"
    );

    let tools = ToolExecutionService::new(Arc::clone(&store))
        .with_capability_matrix(Arc::new(matrix.clone()));

    // (2) Denied actions are denied outside the model: a strict request is
    // refused before a process exists, whatever the model asked for.
    let marker = bench.temp.path().join("a36-strict-marker.txt");
    let prepared = tools
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.a36",
            &bench.workspace,
            marker_action(&child, &marker, IsolationMode::Strict),
        ))
        .await
        .expect("prepares");
    let grant = tools.approve(&prepared).await.expect("approval");
    let view = tools.execute(prepared, Some(grant)).await.expect("denied");
    let (code, reason) = denied(&view);
    assert_eq!(code, ErrorCode::StrictIsolationUnavailable.as_str());
    assert!(!marker.exists(), "a refused strict request runs nothing");
    for capability in matrix.missing_for(StrictProfile::Full) {
        assert!(
            reason.contains(capability.as_str()),
            "the refusal names every missing capability: {reason}"
        );
    }

    // (3) The canaries, end to end through the gate rather than through the
    // probe's word: a real tool call reaches outside the workspace, reaches the
    // network, and reaches a credential pipe, because this host does not deny
    // any of them. Each is a receipt-backed execution.
    let outside = probe_root.path().join("outside");
    std::fs::create_dir_all(&outside).expect("canary root");
    let file_canary = outside.join("a36-read-canary.txt");
    let file_nonce = format!("A36-FILE-{}", probe.nonce());
    std::fs::write(&file_canary, &file_nonce).expect("write canary");
    let output = run_through_gate(
        &tools,
        &bench,
        &session,
        &task,
        &child,
        vec![
            "read".to_owned(),
            file_canary.to_string_lossy().into_owned(),
        ],
    )
    .await;
    assert!(
        output.contains(&file_nonce),
        "the file canary outside the workspace must be reachable on this host, and the measurement must say so: {output}"
    );
    assert_eq!(
        matrix.verdict(Capability::FilesystemReadConfinement),
        Some(CapabilityVerdict::Unsupported)
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let port = listener.local_addr().expect("address").port();
    let network_nonce = format!("A36-NET-{}", probe.nonce());
    let accept = async {
        let Ok(Ok((mut stream, _))) =
            tokio::time::timeout(std::time::Duration::from_secs(10), listener.accept()).await
        else {
            return String::new();
        };
        let mut buffer = vec![0_u8; 256];
        match tokio::time::timeout(
            std::time::Duration::from_secs(2),
            tokio::io::AsyncReadExt::read(&mut stream, &mut buffer),
        )
        .await
        {
            Ok(Ok(read)) => String::from_utf8_lossy(&buffer[..read]).into_owned(),
            _ => String::new(),
        }
    };
    // The listener is read *after* the run: the connection and its bytes are
    // queued by the OS, and racing a run that may be waiting behind the
    // host-wide process permit would measure the queue rather than the boundary.
    let output = run_through_gate(
        &tools,
        &bench,
        &session,
        &task,
        &child,
        vec![
            "connect".to_owned(),
            port.to_string(),
            network_nonce.clone(),
        ],
    )
    .await;
    assert!(
        output.contains("connected=true"),
        "the fixture reports whether its connection succeeded: {output}"
    );
    let received = accept.await;
    assert!(
        received.contains(&network_nonce),
        "an egress canary must reach a loopback listener on this host, and the measurement must say so"
    );
    assert_eq!(
        matrix.verdict(Capability::NetworkEgressDenial),
        Some(CapabilityVerdict::Unsupported)
    );

    let socket_nonce = format!("A36-SOCK-{}", probe.nonce());
    let socket_canary = credential_canary(&socket_nonce).await;
    assert!(
        socket_canary.reached,
        "a credential pipe must be reachable on this host, and the measurement must say so: {}",
        socket_canary.detail
    );
    assert_eq!(
        matrix.verdict(Capability::CredentialSocketDenial),
        Some(CapabilityVerdict::Unsupported)
    );

    // (4) Resource exhaustion is bounded, not capped, and the difference is in
    // the matrix: a flood is cut by the quota and a crowd is reaped with the
    // tree, while memory and process count stay explicitly unclaimed.
    let flooded = run_through_gate(
        &tools,
        &bench,
        &session,
        &task,
        &child,
        vec!["flood".to_owned(), (256 * 1024).to_string()],
    )
    .await;
    assert!(
        flooded.len() < 256 * 1024,
        "the capture quota bounds the flood"
    );
    assert_eq!(
        matrix.verdict(Capability::ResourceLimitMemory),
        Some(CapabilityVerdict::Unsupported)
    );
    assert_eq!(
        matrix.verdict(Capability::ResourceLimitProcessCount),
        Some(CapabilityVerdict::Unsupported)
    );

    // (5) Lifecycle: every execution above left a settled lease with the
    // artifact digest an export must check, a live owner is never touched by
    // reconciliation, and a killed owner is settled with a note.
    let host_id = store.fence().expect("fence").host_id.as_str().to_owned();
    let live = LeaseOwner::open(
        &store,
        &LeaseRequest::from_plan(
            &ExecutionPlan::for_process_action(
                &CodingToolAction::RunShell {
                    command: "echo hi".to_owned(),
                    timeout_ms: 1_000,
                    isolation: IsolationMode::BestEffort,
                    env: Vec::new(),
                },
                &bench.workspace,
                &matrix,
                StrictProfile::Containment,
                1_000,
                65_536,
            ),
            "session.a36",
            "task.a36",
            "execution.a36-live",
            bench.data_dir.join("leases"),
        ),
    )
    .await
    .expect("live lease");
    let live_id = live.lease_id().to_owned();
    let report = reconcile_backend_leases(&store, u64::MAX, 1_800_000_000_000)
        .await
        .expect("reconcile");
    assert_eq!(
        report.outcome_for(&live_id),
        Some(LeaseOutcome::StillOwned),
        "reconciliation may not touch a live owner: {report:?}"
    );
    let settled = store
        .unsettled_backend_leases(i64::MAX)
        .await
        .expect("list");
    assert!(
        settled.iter().all(|lease| lease.lease_id == live_id),
        "the executions above left no unsettled lease: {settled:?}"
    );
    assert!(
        store
            .backend_lease(&live_id)
            .await
            .expect("read")
            .expect("row")
            .released_at_unix_ms
            .is_none()
    );
    assert!(host_id.starts_with("host_"));
    live.release(&store, None).await.expect("release");

    // (6) The negative control: the same fixtures with the boundary removed must
    // fail, or the probes are measuring nothing. Nothing here is mocked -- the
    // child is a real process and the control really omits the wrapper.
    let boundary = probe.boundary_break_control().await.expect("control");
    assert!(
        boundary.escaped,
        "without the job object a descendant must survive the kill: {}",
        boundary.detail
    );
    assert!(
        probe
            .environment_inheritance_control()
            .await
            .expect("control"),
        "without the allowlist the canary must reach the child"
    );

    drop(tools);
    close(store).await;
}

/// Run one fixture command through the real gate and return its stdout.
async fn run_through_gate(
    tools: &ToolExecutionService,
    bench: &Bench,
    session: &SessionId,
    task: &TaskId,
    child: &ProbeChild,
    args: Vec<String>,
) -> String {
    let prepared = tools
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.a36",
            &bench.workspace,
            CodingToolAction::RunProcess {
                executable: child.executable().to_string_lossy().into_owned(),
                args,
                timeout_ms: 20_000,
                isolation: IsolationMode::BestEffort,
                env: Vec::new(),
            },
        ))
        .await
        .expect("prepares");
    let grant = tools.approve(&prepared).await.expect("approval");
    let view = tools.execute(prepared, Some(grant)).await.expect("runs");
    match view.output {
        ToolOutput::Process { stdout, .. } => stdout,
        other => panic!("a process canary must settle as a process: {other:?}"),
    }
}

struct SocketCanary {
    reached: bool,
    detail: String,
}

/// Host a credential-like endpoint and let a real tool call try to open it.
#[cfg(windows)]
async fn credential_canary(nonce: &str) -> SocketCanary {
    use tokio::net::windows::named_pipe::ServerOptions;

    let name = format!(r"\\.\pipe\a36-{nonce}");
    let mut server = match ServerOptions::new().first_pipe_instance(true).create(&name) {
        Ok(server) => server,
        Err(error) => {
            return SocketCanary {
                reached: false,
                detail: format!("cannot host the canary pipe: {error}"),
            };
        }
    };
    let accept = async {
        let Ok(Ok(())) =
            tokio::time::timeout(std::time::Duration::from_secs(10), server.connect()).await
        else {
            return String::new();
        };
        let mut buffer = vec![0_u8; 256];
        match tokio::time::timeout(
            std::time::Duration::from_secs(2),
            tokio::io::AsyncReadExt::read(&mut server, &mut buffer),
        )
        .await
        {
            Ok(Ok(read)) => String::from_utf8_lossy(&buffer[..read]).into_owned(),
            _ => String::new(),
        }
    };
    let (_, received) = tokio::join!(run_pipe_canary(&name, nonce), accept);
    SocketCanary {
        reached: received.contains(nonce),
        detail: format!("pipe {name} received {received:?}"),
    }
}

#[cfg(unix)]
async fn credential_canary(nonce: &str) -> SocketCanary {
    let path = std::env::temp_dir().join(format!("a36-{nonce}.sock"));
    let listener = match tokio::net::UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(error) => {
            return SocketCanary {
                reached: false,
                detail: format!("cannot host the canary socket: {error}"),
            };
        }
    };
    let accept = async {
        let Ok(Ok((mut stream, _))) =
            tokio::time::timeout(std::time::Duration::from_secs(10), listener.accept()).await
        else {
            return String::new();
        };
        let mut buffer = vec![0_u8; 256];
        match tokio::time::timeout(
            std::time::Duration::from_secs(2),
            tokio::io::AsyncReadExt::read(&mut stream, &mut buffer),
        )
        .await
        {
            Ok(Ok(read)) => String::from_utf8_lossy(&buffer[..read]).into_owned(),
            _ => String::new(),
        }
    };
    let address = path.to_string_lossy().into_owned();
    let (_, received) = tokio::join!(run_pipe_canary(&address, nonce), accept);
    SocketCanary {
        reached: received.contains(nonce),
        detail: format!("socket {address} received {received:?}"),
    }
}

/// The tool call side of the socket canary, through its own service and store.
async fn run_pipe_canary(address: &str, nonce: &str) -> String {
    let bench = bench();
    let store = bench.open_store().await;
    let (session, task) = admit(&store, &bench).await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let output = run_through_gate(
        &tools,
        &bench,
        &session,
        &task,
        &probe_child(),
        vec!["pipe".to_owned(), address.to_owned(), nonce.to_owned()],
    )
    .await;
    drop(tools);
    close(store).await;
    output
}

// ---------------------------------------------------------------------------
// M12-04 — the operator surface tells the same truth as the matrix
// ---------------------------------------------------------------------------

/// The CLI is the surface an operator reads, so it is run for real: `--require
/// full` must fail here, and `--require containment` must not.
#[tokio::test]
async fn m12_04_the_cli_refuses_a_profile_this_host_cannot_enforce() {
    let ha = PathBuf::from(env!("CARGO_BIN_EXE_ha"));
    let child = probe_child();
    let temp = tempfile::tempdir().expect("probe root");
    let root = temp.path().join("probe");

    let refused = Command::new(&ha)
        .args([
            "sandbox",
            "probe",
            "--probe-child",
            child.executable().to_string_lossy().as_ref(),
            "--root",
            root.to_string_lossy().as_ref(),
            "--require",
            "full",
            "--json",
        ])
        .output()
        .expect("ha runs");
    assert!(
        !refused.status.success(),
        "a profile this host cannot enforce must not exit zero"
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        text.contains("strict_isolation_unavailable"),
        "the refusal has to be the typed one: {text}"
    );
    for capability in [
        "filesystem_read_confinement",
        "network_egress_denial",
        "credential_socket_denial",
    ] {
        assert!(
            text.contains(capability),
            "the refusal has to name {capability}: {text}"
        );
    }

    let served = Command::new(&ha)
        .args([
            "sandbox",
            "probe",
            "--probe-child",
            child.executable().to_string_lossy().as_ref(),
            "--root",
            root.to_string_lossy().as_ref(),
            "--require",
            "containment",
            "--json",
        ])
        .output()
        .expect("ha runs");
    assert!(
        served.status.success(),
        "containment is servable here: {}",
        String::from_utf8_lossy(&served.stderr)
    );
    let matrix: serde_json::Value =
        serde_json::from_slice(&served.stdout).expect("the CLI prints the versioned matrix");
    assert_eq!(matrix["schema_version"], 1);
    assert_eq!(
        matrix["findings"].as_array().map(Vec::len),
        Some(Capability::ALL.len())
    );
    assert_eq!(
        matrix["host"]["backend"].as_str(),
        Some(harness_tools::CONTAINMENT_BACKEND)
    );
}