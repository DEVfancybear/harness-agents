//! M0 — Foundation và executable contracts.
//!
//! Contract, schema, state, dependency and gate tests for work items M0-01 to
//! M0-04. Every assertion here runs against the real `ha` binary, the real
//! store, or the real contract types; nothing is a production stub.

use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::Arc,
};

use harness_orchestrator::{BudgetUsage, DelegatedOutcome, DelegatedResult, WorkerRef};
use harness_runtime::{AgentState, RunCommand};
use harness_session::{AdmitInputRequest, SessionService};
use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
use harness_types::{
    AcceptanceActor, AcceptanceCommand, AcceptanceRecord, ContentHash, CriterionEvidence,
    CriterionState, CriterionStatus, ErrorCode, EventId, FixedIdSource, HostId, InputId, ProjectId,
    SessionId, SourceAuthority, TaskId, VersionedDocument, WorkspaceObservation,
    known_document_kinds,
};
use serde_json::Value;

/// The fixed `UUIDv7`s the M0 fixtures inject; also used by the child fixture.
/// Admission generates two IDs, so the fixture source carries both.
const FIXED_UUID: &str = "018f8b64-5c8d-7a0a-8f21-123456789abc";
const FIXED_UUID_SECOND: &str = "018f8b64-5c8d-7a0a-8f21-123456789abd";

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

fn fixture_workspace() -> WorkspaceObservation {
    WorkspaceObservation {
        project_id: ProjectId::generate(),
        worktree_id: "m0-test".to_owned(),
        base_commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        observed_fingerprint: ContentHash::from_bytes(b"m0 test workspace"),
    }
}

// ---------------------------------------------------------------------------
// M0-01 — identities, error contract and state reducers
// ---------------------------------------------------------------------------

/// The exit-code and retry-class contract, pinned against a hand-written
/// fixture so a mapping change cannot pass silently.
#[test]
fn m0_01_retry_class_and_exit_codes_match_the_contract() {
    let fixture: Value = serde_json::from_str(
        &fs::read_to_string(fixture_path("m0/cli/exit-codes.json")).expect("exit-code fixture"),
    )
    .expect("exit-code fixture is JSON");
    assert_eq!(fixture["schema_version"], 1);
    let cases = fixture["cases"].as_array().expect("cases are an array");
    assert!(cases.len() >= 12, "the fixture must cover every class");
    for case in cases {
        let name = case["code"].as_str().expect("a code name");
        let code = match name {
            "invalid_payload" => ErrorCode::InvalidPayload,
            "config_unknown_field" => ErrorCode::ConfigUnknownField,
            "config_read_error" => ErrorCode::ConfigReadError,
            "unsupported_schema_version" => ErrorCode::UnsupportedSchemaVersion,
            "gate_required_test_ignored" => ErrorCode::GateRequiredTestIgnored,
            "approval_required" => ErrorCode::ApprovalRequired,
            "runtime_blocked" => ErrorCode::RuntimeBlocked,
            "process_outcome_unknown" => ErrorCode::ProcessOutcomeUnknown,
            "storage_write_failed" => ErrorCode::StorageWriteFailed,
            "provider_protocol" => ErrorCode::ProviderProtocol,
            "sequence_conflict" => ErrorCode::SequenceConflict,
            "writer_locked" => ErrorCode::WriterLocked,
            "idempotency_conflict" => ErrorCode::IdempotencyConflict,
            "provider_canceled" => ErrorCode::ProviderCanceled,
            "process_canceled" => ErrorCode::ProcessCanceled,
            "policy_denied" => ErrorCode::PolicyDenied,
            "task_not_found" => ErrorCode::TaskNotFound,
            other => panic!("fixture names an unknown code: {other}"),
        };
        assert_eq!(
            i32::from(code.exit_code()),
            i32::try_from(case["exit_code"].as_u64().expect("exit code")).expect("fits"),
            "exit code for {name}"
        );
        assert_eq!(
            code.retry_class().as_str(),
            case["retry_class"].as_str().expect("retry class"),
            "retry class for {name}"
        );
    }
    // The serializable report carries the version, code and retry class.
    let report = harness_types::HarnessError::new(
        ErrorCode::ConfigUnknownField,
        "configuration file is invalid",
    )
    .report();
    let rendered = serde_json::to_value(&report).expect("report serializes");
    assert_eq!(rendered["schema_version"], 1);
    assert_eq!(rendered["code"], "config_unknown_field");
    assert_eq!(rendered["retry_class"], "never");
    assert_eq!(rendered["safe_message"], "configuration file is invalid");
}

/// An injected `IdSource` makes admission deterministic without changing the
/// production path, which keeps using `SystemIdSource`.
#[tokio::test]
async fn m0_01_id_source_makes_admission_deterministic() {
    let temp = tempfile::tempdir().expect("temp dir");
    let store = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(temp.path(), HostId::generate()))
            .await
            .expect("writer opens"),
    );
    let ids = Arc::new(
        FixedIdSource::parse_uuids([FIXED_UUID, FIXED_UUID_SECOND]).expect("fixture UUIDs parse"),
    );
    let service = SessionService::with_id_source(Arc::clone(&store), ids);
    let session_id = SessionId::generate();
    let task_id = TaskId::generate();
    let ack = service
        .admit_input(AdmitInputRequest {
            session_id: session_id.clone(),
            task_id: task_id.clone(),
            input_id: InputId::generate(),
            expected_sequence: 1,
            authority: SourceAuthority::User,
            raw_text: "deterministic admission".to_owned(),
            workspace: fixture_workspace(),
            initial_plan_items: Vec::new(),
        })
        .await
        .expect("admission succeeds");
    assert_eq!(ack.sequence, 1);
    assert_eq!(
        ack.event_id,
        EventId::parse(format!("event_{FIXED_UUID}")).expect("typed event id")
    );
    let recovery = service.recover(&session_id).await.expect("recovery works");
    assert_eq!(recovery.replayed_through_sequence, 1);
    assert_eq!(recovery.working_state.task_id, task_id);
    drop(service);
    let store = Arc::try_unwrap(store).expect("store consumers released");
    store.close().await.expect("writer closes");
}

/// Terminal run states cannot regress, and a real run still reaches a terminal
/// state through the wired reducer.
#[test]
fn m0_01_run_state_reducer_never_regresses_from_terminal() {
    for terminal in [
        AgentState::Completed,
        AgentState::Failed,
        AgentState::Canceled,
    ] {
        for command in RunCommand::ALL {
            if command == RunCommand::Dispose {
                continue;
            }
            let error = terminal
                .apply(command)
                .expect_err("a terminal state must not move");
            assert_eq!(error.code(), ErrorCode::InvalidStateTransition);
        }
        assert_eq!(
            terminal
                .apply(RunCommand::Dispose)
                .expect("dispose is the one terminal command")
                .next,
            AgentState::Disposed
        );
    }
    let started = AgentState::Idle
        .apply(RunCommand::Start)
        .expect("idle starts");
    assert_eq!(started.next, AgentState::Running);
    assert_eq!(
        started.events,
        vec![harness_runtime::RunStateEvent::Started]
    );
    assert_eq!(
        AgentState::Running
            .apply(RunCommand::Complete)
            .expect("running completes")
            .next,
        AgentState::Completed
    );

    // End to end: the wired reducer still drives one keyless mock run.
    let temp = tempfile::tempdir().expect("temp dir");
    let data_dir = temp.path().join("data");
    let run = run_ha(&[
        "run",
        "--data-dir",
        data_dir.to_str().expect("UTF-8 path"),
        "--text",
        "m0 reducer wiring",
        "--json",
    ]);
    assert!(
        run.status.success(),
        "ha run failed: {}",
        output_text(&run.stderr)
    );
    let result: Value = serde_json::from_slice(&run.stdout).expect("run output is JSON");
    assert_eq!(result["response"], "mock response");
    let session_id = result["session_id"].as_str().expect("session id");
    let status = run_ha(&[
        "status",
        "--data-dir",
        data_dir.to_str().expect("UTF-8 path"),
        "--session-id",
        session_id,
        "--json",
    ]);
    assert!(
        status.status.success(),
        "ha status failed: {}",
        output_text(&status.stderr)
    );
    let status: Value = serde_json::from_slice(&status.stdout).expect("status output is JSON");
    assert!(
        status["recovery"]["replayed_through_sequence"]
            .as_u64()
            .unwrap_or_default()
            >= 1,
        "the run must be durable: {status}"
    );
}

fn satisfied_criterion(id: &str) -> CriterionState {
    CriterionState {
        criterion_id: id.to_owned(),
        required: true,
        status: CriterionStatus::Satisfied,
        evidence: vec![CriterionEvidence::CheckExecuted {
            command: "cargo test -p harness-types".to_owned(),
            workspace_digest: Some(ContentHash::from_bytes(b"workspace")),
            exit_code: Some(0),
            outcome: harness_types::CheckOutcome::Passed,
            receipt_ref: Some("tool_execution_fixture".to_owned()),
        }],
    }
}

/// A terminal run is not an accepted task: acceptance needs every required
/// criterion with evidence, and a human acceptance is recorded as an override.
#[test]
fn m0_01_completed_run_does_not_accept_a_task_without_criteria() {
    let record = AcceptanceRecord::initial(TaskId::generate());
    let transition = record
        .apply(AcceptanceCommand::Evaluate {
            criteria: vec![
                satisfied_criterion("tests-pass"),
                CriterionState {
                    criterion_id: "final-review".to_owned(),
                    required: true,
                    status: CriterionStatus::Pending,
                    evidence: Vec::new(),
                },
            ],
            pending_effects: 0,
            evidence_fingerprint: Some(ContentHash::from_bytes(b"evidence")),
        })
        .expect("evaluation is a valid command");
    assert!(!transition.next.is_accepted());
    assert_eq!(transition.next.decided_by, AcceptanceActor::Automatic);
    let accepted = transition
        .next
        .apply(AcceptanceCommand::Evaluate {
            criteria: vec![satisfied_criterion("tests-pass")],
            pending_effects: 0,
            evidence_fingerprint: Some(ContentHash::from_bytes(b"evidence")),
        })
        .expect("a satisfied task is accepted");
    assert!(accepted.next.is_accepted());
    assert!(
        accepted
            .next
            .apply(AcceptanceCommand::HumanAccept {
                actor_id: "operator".to_owned(),
                source: harness_types::SourceRef {
                    event_id: EventId::generate(),
                    sequence: 1,
                    content_hash: ContentHash::from_bytes(b"decision"),
                },
            })
            .is_err(),
        "acceptance is terminal"
    );

    // The orchestrator's host-side acceptance uses the same reducer.
    let task_id = TaskId::generate();
    let worker = WorkerRef {
        profile_id: harness_types::AgentProfileId::generate(),
        run_id: harness_types::AgentRunId::generate(),
        role: harness_orchestrator::AgentRole::Coder,
        generation: 0,
    };
    let mut result = DelegatedResult {
        schema_version: harness_orchestrator::DELEGATION_CONTRACT_VERSION,
        result_id: "result-1".to_owned(),
        task_id: task_id.clone(),
        worker,
        outcome: DelegatedOutcome::Completed,
        summary: "did the work".to_owned(),
        artifact_refs: vec!["artifact-1".to_owned()],
        base_revision: "aaa".to_owned(),
        result_revision: "bbb".to_owned(),
        checked_revisions: Vec::new(),
        check_receipts: Vec::new(),
        usage: BudgetUsage {
            model_requests: 1,
            retries: 0,
        },
        detail: Value::Null,
    };
    assert!(
        result.accepted_completion(),
        "a completed report with an artifact is accepted work"
    );
    result.artifact_refs.clear();
    assert!(
        !result.accepted_completion(),
        "a completed report without evidence is not accepted work"
    );
    result.artifact_refs.push("artifact-1".to_owned());
    result.outcome = DelegatedOutcome::OutcomeUnknown;
    assert!(
        !result.accepted_completion(),
        "an unknown outcome is never accepted completion"
    );
}

// ---------------------------------------------------------------------------
// M0-02 — ports, envelopes, schema fixtures, dependency allowlist
// ---------------------------------------------------------------------------

#[test]
fn m0_02_versioned_envelope_golden_fixtures() {
    let oracle: Value = serde_json::from_str(
        &fs::read_to_string(fixture_path("m0/envelope/cases.json")).expect("envelope oracle"),
    )
    .expect("envelope oracle is JSON");
    let cases = oracle["cases"].as_array().expect("cases are an array");
    assert_eq!(cases.len(), 5);
    let known = known_document_kinds();
    let mut hashes = Vec::new();
    for case in cases {
        let file = case["file"].as_str().expect("a fixture file");
        let raw = fs::read_to_string(fixture_path("m0/envelope").join(file))
            .expect("fixture is readable");
        match case["expected"].as_str().expect("an expectation") {
            "accepted" => {
                let document =
                    VersionedDocument::parse_json(&raw, &known).expect("document is admitted");
                assert_eq!(document.kind, case["kind"].as_str().expect("kind"));
                assert_eq!(
                    document.critical,
                    case["critical"].as_bool().expect("critical flag")
                );
                if let Some(expected) = case["content_hash"].as_str() {
                    let hash = document.content_hash().expect("canonical hash");
                    assert_eq!(hash.as_str(), expected, "{file}");
                    hashes.push(hash);
                }
                if let Some(length) = case["canonical_length"].as_u64() {
                    assert_eq!(
                        u64::try_from(document.canonical_bytes().expect("bytes").len())
                            .expect("fits"),
                        length,
                        "{file} canonical length"
                    );
                }
            }
            "unknown_critical_event" => {
                assert_eq!(
                    VersionedDocument::parse_json(&raw, &known)
                        .expect_err("an unknown critical document is blocked")
                        .code(),
                    ErrorCode::UnknownCriticalEvent,
                    "{file}"
                );
            }
            "unsupported_schema_version" => {
                assert_eq!(
                    VersionedDocument::parse_json(&raw, &known)
                        .expect_err("another version is not readable")
                        .code(),
                    ErrorCode::UnsupportedSchemaVersion,
                    "{file}"
                );
            }
            other => panic!("unknown expectation {other}"),
        }
    }
    // Hash order equivalence: the known and reordered fixtures share one hash.
    assert_eq!(hashes.len(), 2);
    assert_eq!(hashes[0], hashes[1]);
}

#[test]
fn m0_02_store_port_contract_is_declared_without_fake_implementation() {
    // Compile-time proof that the port trait exists and can bound a generic.
    #[allow(dead_code, reason = "compile-time bound proof")]
    fn store_port_bound<T: harness_types::StorePort>() {}
    // Documented capability names are stable strings.
    let capabilities = harness_types::documented_capabilities();
    assert_eq!(capabilities.len(), 3);
    assert!(capabilities.contains(harness_types::CAPABILITY_TOOLS_READ));

    // The declared method surface matches CONTRACTS §3.
    let ports_source =
        fs::read_to_string(repository_root().join("crates/harness-types/src/ports.rs"))
            .expect("ports source");
    for method in [
        "admit_input",
        "claim_run",
        "append_domain_change",
        "freeze_step",
        "admit_invocation",
        "settle_invocation",
        "settle_child",
        "recover_readonly",
    ] {
        assert!(
            ports_source.contains(&format!("fn {method}")),
            "StorePort is missing {method}"
        );
    }
    assert!(
        ports_source.contains("mod tests") && ports_source.contains("#[cfg(test)]"),
        "the only implementation double must stay test-only"
    );

    // No production source implements the port before M1. The only
    // implementation is the cfg(test) double in harness-types.
    let mut implementations = Vec::new();
    for entry in fs::read_dir(repository_root().join("crates"))
        .expect("crates directory")
        .flatten()
    {
        let source = entry.path().join("src");
        if source.is_dir() {
            collect_store_port_implementations(&source, &mut implementations);
        }
    }
    assert_eq!(
        implementations,
        vec![PathBuf::from("harness-types/src/ports.rs")],
        "a StorePort implementation outside the test double must wait for M1"
    );
}

fn collect_store_port_implementations(directory: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_store_port_implementations(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            let Ok(contents) = fs::read_to_string(&path) else {
                continue;
            };
            if contents.contains("impl StorePort") {
                let relative = path
                    .strip_prefix(repository_root().join("crates"))
                    .unwrap_or(&path)
                    .to_path_buf();
                found.push(relative);
            }
        }
    }
    found.sort();
}

#[test]
fn m0_02_dependency_allowlist_rejects_forbidden_edge() {
    let root = repository_root();
    let root_argument = root.to_str().expect("UTF-8 root");
    let clean = run_dependency_check(&["--root", root_argument]);
    assert!(clean.status.success(), "{}", output_text(&clean.stderr));
    let report: Value = serde_json::from_slice(&clean.stdout).expect("checker output is JSON");
    assert_eq!(report["status"], "ok");
    assert!(report["edge_count"].as_u64().unwrap_or_default() >= 40);

    // A declared-forbidden edge fails the checker.
    let forbidden = run_dependency_check(&[
        "--root",
        root_argument,
        "--extra-edge",
        "harness-runtime->harness-tools",
    ]);
    assert_eq!(forbidden.status.code(), Some(2));
    let report: Value = serde_json::from_slice(&forbidden.stdout).expect("JSON");
    assert_eq!(report["status"], "error");
    assert!(
        report["violations"]
            .as_array()
            .expect("violations")
            .iter()
            .any(|violation| violation["kind"] == "forbidden_edge"),
        "the forbidden edge must be named: {report}"
    );

    // A wildcard rule catches a base-crate edge.
    let wildcard = run_dependency_check(&[
        "--root",
        root_argument,
        "--extra-edge",
        "harness-types->harness-session",
    ]);
    assert_eq!(wildcard.status.code(), Some(2));
    let report: Value = serde_json::from_slice(&wildcard.stdout).expect("JSON");
    assert!(
        report["violations"]
            .as_array()
            .expect("violations")
            .iter()
            .any(|violation| violation["kind"] == "forbidden_edge")
    );

    // An undeclared target is an undeclared edge.
    let undeclared = run_dependency_check(&[
        "--root",
        root_argument,
        "--extra-edge",
        "harness-cli->harness-not-a-crate",
    ]);
    assert_eq!(undeclared.status.code(), Some(2));

    // A declared edge is not a violation: the checker is not "any extra edge".
    let allowed = run_dependency_check(&[
        "--root",
        root_argument,
        "--extra-edge",
        "harness-cli->harness-tools",
    ]);
    assert!(
        allowed.status.success(),
        "a declared edge must pass: {}",
        output_text(&allowed.stdout)
    );
}

fn run_dependency_check(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dependency_check"))
        .args(arguments)
        .output()
        .expect("dependency_check binary should execute")
}

// ---------------------------------------------------------------------------
// M0-03 — workspace, configuration and CLI integration
// ---------------------------------------------------------------------------

#[test]
fn m0_03_cli_json_errors_keep_stdout_clean_with_typed_exit_codes() {
    let unknown_path = fixture_path("p0/config/unknown-field.toml");
    let unknown = unknown_path.to_str().expect("UTF-8 path");

    let json = run_ha(&["config", "validate", "--config", unknown, "--json"]);
    assert_eq!(
        json.status.code(),
        Some(2),
        "typed exit code for a config error"
    );
    assert!(
        output_text(&json.stdout).is_empty(),
        "stdout carries only a result, never an error: {}",
        output_text(&json.stdout)
    );
    // The accepted H/P contract reads failures from stderr; a `--json`
    // invocation gets the typed report as one JSON document there.
    let report: Value = serde_json::from_str(&output_text(&json.stderr))
        .expect("a --json failure writes one JSON error document to stderr");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["status"], "error");
    assert_eq!(report["exit_code"], 2);
    assert_eq!(report["error"]["code"], "config_unknown_field");
    assert_eq!(report["error"]["retry_class"], "never");
    assert!(
        !report["error"]["safe_message"]
            .as_str()
            .expect("message")
            .contains("bearer"),
        "a safe message never carries credentials"
    );

    // Without `--json` the legacy stderr contract is unchanged.
    let plain = run_ha(&["config", "validate", "--config", unknown]);
    assert_eq!(plain.status.code(), Some(2));
    assert!(output_text(&plain.stdout).is_empty());
    assert!(output_text(&plain.stderr).starts_with("config_unknown_field:"));

    // A successful JSON invocation is unchanged.
    let valid_path = fixture_path("p0/config/valid.toml");
    let valid = run_ha(&[
        "config",
        "validate",
        "--config",
        valid_path.to_str().expect("UTF-8 path"),
        "--json",
    ]);
    assert!(valid.status.success());
    assert!(output_text(&valid.stderr).is_empty());
    let valid: Value = serde_json::from_slice(&valid.stdout).expect("stdout is JSON");
    assert_eq!(valid["valid"], true);
}

#[test]
fn m0_03_non_utf8_config_is_a_typed_read_error() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("non-utf8.toml");
    fs::write(&path, b"schema_version = \xff\xfe\n").expect("fixture bytes");
    let path = path.to_str().expect("UTF-8 path");

    let json = run_ha(&["config", "validate", "--config", path, "--json"]);
    assert_eq!(json.status.code(), Some(2));
    assert!(output_text(&json.stdout).is_empty());
    let report: Value =
        serde_json::from_str(&output_text(&json.stderr)).expect("stderr is the JSON report");
    assert_eq!(report["error"]["code"], "config_read_error");
    assert_eq!(report["error"]["retry_class"], "never");
    let message = report["error"]["safe_message"].as_str().expect("message");
    assert!(
        !message.contains('\u{fffd}') && !message.contains("\\xff"),
        "the rejected bytes must not be echoed: {message}"
    );

    let plain = run_ha(&["config", "validate", "--config", path]);
    assert_eq!(plain.status.code(), Some(2));
    assert!(output_text(&plain.stderr).starts_with("config_read_error:"));
}

// ---------------------------------------------------------------------------
// M0-04 — registry, gate and child-fixture protocol
// ---------------------------------------------------------------------------

#[test]
#[allow(clippy::too_many_lines)] // one registry contract across milestones
fn m0_04_milestone_registry_and_gate_self_test() {
    let root = repository_root();
    let registry: Value = serde_json::from_str(
        &fs::read_to_string(root.join("tests/acceptance/milestones.json"))
            .expect("milestone registry exists"),
    )
    .expect("milestone registry is JSON");
    assert_eq!(registry["schema_version"], 1);
    let milestones = registry["milestones"]
        .as_array()
        .expect("milestones is an array");
    let m0 = milestones
        .iter()
        .find(|milestone| milestone["id"] == "M0")
        .expect("M0 is registered");
    assert!(
        m0["prerequisites"]
            .as_array()
            .expect("prerequisites")
            .is_empty(),
        "M0 has no predecessor"
    );
    assert_eq!(m0["integration_target"], "milestone_m0");
    let required = m0["required_tests"]
        .as_array()
        .expect("required tests")
        .iter()
        .map(|value| value.as_str().expect("a selector").to_owned())
        .collect::<Vec<_>>();
    assert_eq!(required.len(), 11);
    let source = fs::read_to_string(root.join("crates/harness-cli/tests/milestone_m0.rs"))
        .expect("milestone target source");
    for selector in &required {
        assert!(
            source.contains(&format!("fn {selector}(")),
            "required selector has no test: {selector}"
        );
    }
    let work_item_tests = m0["work_items"]
        .as_array()
        .expect("work items")
        .iter()
        .flat_map(|item| {
            item["tests"]
                .as_array()
                .expect("item tests")
                .iter()
                .map(|value| value.as_str().expect("selector").to_owned())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(work_item_tests.len(), required.len());

    // A01-A36 are registered as planned cross-layer cases with an owner.
    let cases = registry["acceptance_cases"]
        .as_array()
        .expect("acceptance cases");
    assert_eq!(cases.len(), 36);
    for case in cases {
        assert_eq!(case["status"], "planned", "{}", case["id"]);
        assert!(
            !case["owner_milestones"]
                .as_array()
                .expect("owners")
                .is_empty()
        );
        assert!(case["planned_test"].as_str().is_some());
    }

    // The registry of P/H cases is untouched by this file.
    let p_registry: Value = serde_json::from_str(
        &fs::read_to_string(root.join("tests/acceptance/registry.json"))
            .expect("P/H registry exists"),
    )
    .expect("P/H registry is JSON");
    assert_eq!(p_registry["registry_kind"], "acceptance");

    let self_test = Command::new("pwsh")
        .current_dir(&root)
        .args([
            "-NoProfile",
            "-File",
            "scripts/Verify-Milestone.ps1",
            "-Milestone",
            "M0",
            "-SelfTest",
        ])
        .output()
        .expect("PowerShell 7 must execute the milestone gate self-test");
    assert!(
        self_test.status.success(),
        "{}",
        output_text(&self_test.stderr)
    );
    let stdout = output_text(&self_test.stdout);
    for control in [
        "missing-selector",
        "ignored-required-test",
        "command-nonzero",
        "zero-test-discovery",
        "unknown-milestone",
        "dependency-edge",
    ] {
        assert!(
            stdout.contains(&format!("NEGATIVE_CONTROL_OK: {control}")),
            "negative control did not run: {control}\n{stdout}"
        );
    }
    assert!(stdout.contains("MILESTONE_GATE_SELFTEST_OK: M0"));
}

#[test]
fn m0_04_child_fixture_failpoint_protocol() {
    let temp = tempfile::tempdir().expect("temp dir");
    let data_dir = temp.path().join("data");
    let marker = temp.path().join("failpoint.marker");
    let mut child = Command::new(env!("CARGO_BIN_EXE_m0_fixture_host"))
        .args([
            "--data-dir",
            data_dir.to_str().expect("UTF-8 path"),
            "--host-id",
            HostId::generate().as_ref(),
            "--mode",
            "failpoint",
            "--failpoint",
            "after-input-commit",
            "--marker",
            marker.to_str().expect("UTF-8 path"),
            "--id-uuid",
            FIXED_UUID,
            "--id-uuid",
            FIXED_UUID_SECOND,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("child fixture starts");
    let stdout = child.stdout.take().expect("stdout is piped");
    let mut lines = BufReader::new(stdout).lines();

    let ready: Value = serde_json::from_str(
        &lines
            .next()
            .expect("a readiness line")
            .expect("readiness is readable"),
    )
    .expect("readiness is JSON");
    assert_eq!(ready["ready"], "m0_fixture_host");
    assert!(ready["pid"].as_u64().is_some());
    assert!(ready["generation"].as_u64().is_some());

    let ack: Value =
        serde_json::from_str(&lines.next().expect("an ack line").expect("ack is readable"))
            .expect("ack is JSON");
    assert_eq!(ack["ack"], "input");
    assert_eq!(ack["sequence"], 1);
    assert_eq!(ack["event_id"], format!("event_{FIXED_UUID}"));

    let failpoint: Value = serde_json::from_str(
        &lines
            .next()
            .expect("a failpoint line")
            .expect("failpoint is readable"),
    )
    .expect("failpoint is JSON");
    assert_eq!(failpoint["failpoint"], "after-input-commit");

    let status = child.wait().expect("child exits");
    assert_eq!(
        status.code(),
        Some(86),
        "a failpoint exit is distinctive, never a success code"
    );
    assert_eq!(
        fs::read_to_string(&marker)
            .expect("the failpoint marker exists")
            .trim(),
        "after-input-commit"
    );
}
