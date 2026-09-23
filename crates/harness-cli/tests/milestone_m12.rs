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
    IsolationMode, PROCESS_ENVIRONMENT_ALLOWLIST, PROCESS_OUTPUT_PAGE_MAX_BYTES, ProbeChild,
    StrictProfile, ToolExecutionService, ToolOutput, ToolRequest, export_artifact,
    export_destination, observe_workspace,
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
