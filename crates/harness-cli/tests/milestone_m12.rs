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
    CAPABILITY_MATRIX_SCHEMA_VERSION, Capability, CapabilityEvidence, CapabilityFinding,
    CapabilityMatrix, CapabilityProbe, CapabilityVerdict, CodingToolAction, HostIdentity,
    IsolationMode, ProbeChild, StrictProfile, ToolExecutionService, ToolOutput, ToolRequest,
    observe_workspace,
};
use harness_types::{
    ErrorCode, HostId, InputId, SessionId, SourceAuthority, TaskId, ToolIntentState,
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
