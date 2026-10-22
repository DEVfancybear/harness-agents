#![forbid(unsafe_code)]

#[test]
fn review_p3_provider_parser_rejects_invalid_optional_fields() {
    for (name, arguments) in [
        ("list_files", r#"{"path":42}"#),
        ("search_text", r#"{"query":"x","path":false}"#),
        ("git_diff", r#"{"path":[]}"#),
        ("git_status", r#"{"unexpected":true}"#),
        (
            "read_file",
            r#"{"path":"src/parser.txt","unexpected":true}"#,
        ),
        (
            "run_shell",
            r#"{"command":"echo x","timeout_ms":1,"isolation":true}"#,
        ),
        (
            "run_shell",
            r#"{"command":"echo x","timeout_ms":1,"isolation":null}"#,
        ),
    ] {
        assert!(
            CodingToolAction::from_provider_call(name, arguments).is_err(),
            "accepted {name} {arguments}"
        );
    }
    for arguments in ["{}", r#"{"path":null}"#, r#"{"path":"src"}"#] {
        CodingToolAction::from_provider_call("list_files", arguments).unwrap();
    }
}

#[test]
fn review_p3_policy_cannot_be_bypassed_by_path_spelling_or_ancestor() {
    let policy = ToolPolicy::new(1, vec![PolicyRule::deny("src/private", "denied")]);
    for path in [
        "./src/private/file.txt",
        "src/./private/file.txt",
        "src//private/file.txt",
    ] {
        assert!(
            policy
                .denial_for(&CodingToolAction::ReadFile {
                    path: path.into(),
                    offset: None,
                    limit: None,
                    tail: None,
                })
                .is_some(),
            "policy bypass: {path}"
        );
    }
    #[cfg(windows)]
    assert!(
        policy
            .denial_for(&CodingToolAction::ReadFile {
                path: "SRC\\PRIVATE\\file.txt".into(),
                offset: None,
                limit: None,
                tail: None,
            })
            .is_some()
    );
    for action in [
        CodingToolAction::ListFiles { path: None },
        CodingToolAction::SearchText {
            query: "x".into(),
            path: Some("src".into()),
            regex: false,
            case_insensitive: false,
            glob: None,
            context_lines: None,
            any_of: Vec::new(),
        },
        CodingToolAction::GitDiff { path: None },
    ] {
        assert!(
            policy.denial_for(&action).is_some(),
            "ancestor read bypass: {action:?}"
        );
    }
    assert!(
        policy
            .denial_for(&CodingToolAction::ReadFile {
                path: "src/private-other/file.txt".into(),
                offset: None,
                limit: None,
                tail: None,
            })
            .is_none()
    );
}

#[tokio::test]
async fn review_p3_explicit_workspace_root_can_be_listed() {
    let temp = TempDir::new().unwrap();
    let root = setup_workspace(&temp);
    let store = writer(&temp).await;
    let (session, task, _) = admit(&store, &root).await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    for path in [".", "./"] {
        let (prepared, approval) = prepared_and_approved(
            &tools,
            &session,
            &task,
            &root,
            CodingToolAction::ListFiles {
                path: Some(path.into()),
            },
        )
        .await;
        let view = tools.execute(prepared, Some(approval)).await.unwrap();
        assert!(
            matches!(view.output, ToolOutput::ListFiles { ref paths, .. } if paths.contains(&"src/parser.txt".to_owned()))
        );
    }
    drop(tools);
    close_writer(store).await;
}

#[tokio::test]
async fn review_p3_large_invalid_utf8_is_not_silently_repaired() {
    let temp = TempDir::new().unwrap();
    let root = setup_workspace(&temp);
    let mut bytes = vec![b'a'; 140 * 1024];
    bytes[1] = 0xff;
    fs::write(root.join("invalid.txt"), bytes).unwrap();
    let store = writer(&temp).await;
    let (session, task, _) = admit(&store, &root).await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let (prepared, approval) = prepared_and_approved(
        &tools,
        &session,
        &task,
        &root,
        CodingToolAction::ReadFile {
            path: "invalid.txt".into(),
            offset: None,
            limit: None,
            tail: None,
        },
    )
    .await;
    let result = tools.execute(prepared, Some(approval)).await;
    assert!(
        !matches!(result, Ok(ref view) if matches!(view.output, ToolOutput::ReadFile { .. })),
        "malformed file was reported as text: {result:?}"
    );
    drop(tools);
    close_writer(store).await;
}

#[tokio::test]
async fn handoff_file_is_writable_while_harness_configuration_stays_protected() {
    let temp = TempDir::new().unwrap();
    let root = setup_workspace(&temp);
    fs::create_dir(root.join(".harness")).unwrap();
    let store = writer(&temp).await;
    let (session, task, _) = admit(&store, &root).await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let (prepared, approval) = prepared_and_approved(
        &tools,
        &session,
        &task,
        &root,
        CodingToolAction::WriteFile {
            path: ".harness/progress.md".into(),
            content: "# Progress\nverified handoff".into(),
            expected_hash: None,
        },
    )
    .await;
    tools
        .execute(prepared, Some(approval))
        .await
        .expect("write handoff");
    assert_eq!(
        fs::read_to_string(root.join(".harness/progress.md")).unwrap(),
        "# Progress\nverified handoff"
    );
    let (prepared, approval) = prepared_and_approved(
        &tools,
        &session,
        &task,
        &root,
        CodingToolAction::ReadFile {
            path: ".harness/progress.md".into(),
            offset: None,
            limit: None,
            tail: None,
        },
    )
    .await;
    let view = tools
        .execute(prepared, Some(approval))
        .await
        .expect("read handoff");
    assert!(
        matches!(view.output, ToolOutput::ReadFile { ref content, .. } if content.contains("verified handoff"))
    );
    for path in [
        ".harness/config.toml",
        ".harness/auth.json",
        ".harness/private/progress.md",
        "nested/.harness/progress.md",
        ".harness/progress.md/secret.txt",
    ] {
        assert!(
            harness_tools::is_sensitive_workspace_path(Path::new(path)),
            "protected: {path}"
        );
    }
    drop(tools);
    close_writer(store).await;
}

#[tokio::test]
async fn review_p3_search_redacts_before_truncation() {
    let temp = TempDir::new().unwrap();
    let root = setup_workspace(&temp);
    let line = format!("api_key=FAKE_VALUE_ONLY_FOR_TEST {}", "x".repeat(300));
    fs::write(root.join("output.txt"), line).unwrap();
    let store = writer(&temp).await;
    let (session, task, _) = admit(&store, &root).await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let (prepared, approval) = prepared_and_approved(
        &tools,
        &session,
        &task,
        &root,
        CodingToolAction::SearchText {
            query: "FAKE_VALUE_ONLY_FOR_TEST".into(),
            path: None,
            regex: false,
            case_insensitive: false,
            glob: None,
            context_lines: None,
            any_of: Vec::new(),
        },
    )
    .await;
    let view = tools.execute(prepared, Some(approval)).await.unwrap();
    let ToolOutput::SearchText { matches, .. } = view.output else {
        panic!("expected search output")
    };
    assert_eq!(matches.len(), 1);
    assert!(matches[0].preview.contains("api_key=[REDACTED]"));
    assert!(!matches[0].preview.contains("FAKE_VALUE_ONLY_FOR_TEST"));
    drop(tools);
    close_writer(store).await;
}

/// A new project often starts as an empty folder, with no Git repository yet.
/// Every tool that does not need Git works there, and the Git tools say there
/// is no repository instead of failing as an unknown outcome.
#[tokio::test]
async fn p3_a_folder_without_git_supports_the_coding_tools() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("workspace");
    fs::create_dir_all(&root).unwrap();
    let store = writer(&temp).await;
    let (session, task, _) = admit(&store, &root).await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let run = |action: CodingToolAction| {
        let tools = &tools;
        let session = &session;
        let task = &task;
        let root = &root;
        async move {
            let (prepared, approval) =
                prepared_and_approved(tools, session, task, root, action).await;
            tools.execute(prepared, Some(approval)).await
        }
    };
    let written = run(CodingToolAction::WriteFile {
        path: "game.js".to_owned(),
        content: "let score = 0;\n".to_owned(),
        expected_hash: None,
    })
    .await
    .expect("write");
    assert!(
        matches!(written.output, ToolOutput::WriteFile { .. }),
        "{:?}",
        written.output
    );
    for action in [
        CodingToolAction::ListFiles { path: None },
        CodingToolAction::ReadFile {
            path: "game.js".to_owned(),
            offset: None,
            limit: None,
            tail: None,
        },
        CodingToolAction::SearchText {
            query: "score".into(),
            path: None,
            regex: false,
            case_insensitive: false,
            glob: None,
            context_lines: None,
            any_of: Vec::new(),
        },
        process_action("Write-Output ok", 30_000),
        CodingToolAction::GitStatus,
        CodingToolAction::GitDiff { path: None },
        CodingToolAction::GitLog {
            limit: None,
            path: None,
        },
    ] {
        let name = format!("{action:?}");
        let view = run(action)
            .await
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        match &view.output {
            ToolOutput::Git { output, .. } => {
                assert!(output.contains("not a Git repository"), "{name}: {output}");
                assert!(output.len() < 300, "{name}: no usage text: {output}");
            }
            ToolOutput::OutcomeUnknown { .. } | ToolOutput::Denied { .. } => {
                panic!("{name}: {:?}", view.output)
            }
            _ => {}
        }
    }
    drop(tools);
    close_writer(store).await;
}

/// Seen in a session: `run_process executable=py` (and `which chromium`) on a
/// host without them failed with "side effect may be uncertain; do not rerun
/// automatically: `process_outcome_unknown`: ... program not found". Nothing
/// ran, so it settles as a plain failure the model may correct.
#[tokio::test]
async fn p3_a_program_that_is_not_found_is_a_plain_failure() {
    let temp = TempDir::new().unwrap();
    let root = setup_workspace(&temp);
    let store = writer(&temp).await;
    let (session, task, _) = admit(&store, &root).await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let (prepared, approval) = prepared_and_approved(
        &tools,
        &session,
        &task,
        &root,
        CodingToolAction::RunProcess {
            executable: "ha-no-such-program-anywhere".to_owned(),
            args: vec!["--version".to_owned()],
            timeout_ms: 10_000,
            isolation: IsolationMode::BestEffort,
            env: Vec::new(),
        },
    )
    .await;
    let view = tools.execute(prepared, Some(approval)).await.unwrap();
    let ToolOutput::Denied { code, reason } = &view.output else {
        panic!("expected a plain failure, got {:?}", view.output)
    };
    assert_eq!(code, "invalid_payload");
    assert!(reason.contains("did not start"), "{reason}");
    assert!(reason.contains("ha-no-such-program-anywhere"), "{reason}");
    assert_eq!(
        view.receipt.expect("receipt").outcome_state,
        ToolOutcomeState::Settled
    );
    assert!(
        store
            .pending_tool_intents(&session)
            .await
            .unwrap()
            .is_empty()
    );
    drop(tools);
    close_writer(store).await;
}

/// Seen in a session: `search_text path=README.md` failed with "`search_text`
/// path must be a directory" under "side effect may be uncertain; do not rerun
/// automatically" - for a read. A file is searched now, and a read that fails
/// settles as a plain failure the model may correct.
#[tokio::test]
async fn p3_a_search_names_a_file_and_a_failed_read_is_not_an_unknown_outcome() {
    let temp = TempDir::new().unwrap();
    let root = setup_workspace(&temp);
    let store = writer(&temp).await;
    let (session, task, _) = admit(&store, &root).await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let search = |path: &str| CodingToolAction::SearchText {
        query: "parser".into(),
        path: Some(path.into()),
        regex: false,
        case_insensitive: false,
        glob: None,
        context_lines: None,
        any_of: Vec::new(),
    };
    let (prepared, approval) =
        prepared_and_approved(&tools, &session, &task, &root, search("src/parser.txt")).await;
    let view = tools.execute(prepared, Some(approval)).await.unwrap();
    let ToolOutput::SearchText { matches, .. } = view.output else {
        panic!("expected search output, got {:?}", view.output)
    };
    assert_eq!(matches.len(), 1);

    // An ignored file is not searched, as a folder search would skip it.
    let (prepared, approval) =
        prepared_and_approved(&tools, &session, &task, &root, search("ignored.tmp")).await;
    let view = tools.execute(prepared, Some(approval)).await.unwrap();
    let ToolOutput::Denied { code, reason } = &view.output else {
        panic!("expected a plain failure, got {:?}", view.output)
    };
    assert_eq!(code, "invalid_payload");
    assert!(!reason.contains("side effect"), "{reason}");
    assert_eq!(
        view.receipt.expect("receipt").outcome_state,
        ToolOutcomeState::Settled
    );
    assert!(
        store
            .pending_tool_intents(&session)
            .await
            .unwrap()
            .is_empty(),
        "a failed read leaves nothing to reconcile"
    );
    drop(tools);
    close_writer(store).await;
}

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::Arc,
    time::{Duration, Instant},
};

use harness_providers::CancellationToken;
use harness_session::{AdmitInputRequest, SessionService};
use harness_store_sqlite::{SqliteStore, StoreFaultPlan, StoreFaultPoint, WriterOpenOptions};
use harness_tools::{
    CodingToolAction, IsolationMode, PolicyRule, ToolExecutionService, ToolOutput, ToolPolicy,
    ToolRequest, coding_tool_schemas, observe_workspace,
};
use harness_types::{
    ContentHash, ErrorCode, HostId, InputId, ProjectId, SessionId, SourceAuthority, TaskId,
    ToolIntentState, ToolOutcomeState,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::sleep;

fn setup_workspace(temp: &TempDir) -> PathBuf {
    let root = temp.path().join("workspace");
    fs::create_dir_all(root.join("src")).expect("fixture source directory");
    fs::write(root.join(".gitignore"), "ignored.tmp\n").expect("fixture ignore file");
    fs::write(root.join("ignored.tmp"), "ignored\n").expect("fixture ignored file");
    fs::write(root.join("src").join("parser.txt"), "BUG parser\r\n").expect("fixture parser file");
    git(&root, &["init"]);
    git(
        &root,
        &["config", "user.email", "p3-fixture@example.invalid"],
    );
    git(&root, &["config", "user.name", "P3 Fixture"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "-m", "fixture baseline"]);
    root
}

fn git(root: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(root)
        .output()
        .expect("Git fixture command should start");
    assert!(
        output.status.success(),
        "git {:?} failed:\nstdout:\n{}\nstderr:\n{}",
        arguments,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_ha(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ha"))
        .args(arguments)
        .output()
        .expect("ha binary should execute")
}

async fn writer(temp: &TempDir) -> Arc<SqliteStore> {
    Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(temp.path(), HostId::generate()))
            .await
            .expect("writer should open"),
    )
}

async fn writer_with_fault(temp: &TempDir, point: StoreFaultPoint) -> Arc<SqliteStore> {
    let options = WriterOpenOptions::new(temp.path(), HostId::generate())
        .with_fault_plan(StoreFaultPlan::with_point(point));
    Arc::new(
        SqliteStore::open_writer(options)
            .await
            .expect("fault-injected writer should open"),
    )
}

async fn close_writer(store: Arc<SqliteStore>) {
    Arc::try_unwrap(store)
        .expect("store consumers must be released")
        .close()
        .await
        .expect("writer should close");
}

async fn admit(store: &Arc<SqliteStore>, root: &Path) -> (SessionId, TaskId, ProjectId) {
    let session_id = SessionId::generate();
    let task_id = TaskId::generate();
    let project_id = ProjectId::generate();
    let service = SessionService::new(Arc::clone(store));
    service
        .admit_input(AdmitInputRequest {
            session_id: session_id.clone(),
            task_id: task_id.clone(),
            input_id: InputId::generate(),
            expected_sequence: 1,
            authority: SourceAuthority::User,
            raw_text: "repair the disposable P3 fixture".to_owned(),
            workspace: observe_workspace(project_id.clone(), root)
                .expect("workspace observation should be real"),
            initial_plan_items: Vec::new(),
        })
        .await
        .expect("fixture input should be admitted");
    drop(service);
    (session_id, task_id, project_id)
}

fn request(
    session_id: &SessionId,
    task_id: &TaskId,
    root: &Path,
    actor: &str,
    action: CodingToolAction,
) -> ToolRequest {
    ToolRequest::new(
        session_id.clone(),
        task_id.clone(),
        actor,
        root.to_path_buf(),
        action,
    )
}

async fn prepared_and_approved(
    tools: &ToolExecutionService,
    session_id: &SessionId,
    task_id: &TaskId,
    root: &Path,
    action: CodingToolAction,
) -> (
    harness_tools::PreparedToolRequest,
    harness_tools::ApprovalGrant,
) {
    let prepared = tools
        .prepare(request(session_id, task_id, root, "fixture.actor", action))
        .await
        .expect("tool proposal should prepare");
    let approval = tools
        .approve(&prepared)
        .await
        .expect("tool proposal should be explicitly approved");
    (prepared, approval)
}

fn process_action(script: &str, timeout_ms: u64) -> CodingToolAction {
    let (executable, args) = platform_command(script);
    CodingToolAction::RunProcess {
        executable,
        args,
        timeout_ms,
        isolation: IsolationMode::BestEffort,
        env: Vec::new(),
    }
}

#[cfg(windows)]
fn platform_command(script: &str) -> (String, Vec<String>) {
    (
        "pwsh".to_owned(),
        vec![
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-Command".to_owned(),
            script.to_owned(),
        ],
    )
}

#[cfg(not(windows))]
fn platform_command(script: &str) -> (String, Vec<String>) {
    ("sh".to_owned(), vec!["-c".to_owned(), script.to_owned()])
}

#[cfg(windows)]
fn output_script() -> &'static str {
    "[Console]::Out.Write(('x' * 70000)); [Console]::Error.Write(('e' * 70000))"
}

#[cfg(not(windows))]
fn output_script() -> &'static str {
    "yes x | head -c 70000; yes e | head -c 70000 >&2"
}

#[cfg(windows)]
fn sleep_script(milliseconds: u64) -> String {
    format!("Start-Sleep -Milliseconds {milliseconds}")
}

#[cfg(not(windows))]
fn sleep_script(milliseconds: u64) -> String {
    format!("sleep {}.{:03}", milliseconds / 1_000, milliseconds % 1_000)
}

#[cfg(windows)]
fn append_marker_script(marker: &Path) -> String {
    let path = marker.to_string_lossy().replace('\'', "''");
    format!("[System.IO.File]::AppendAllText('{path}', 'x')")
}

#[cfg(not(windows))]
fn append_marker_script(marker: &Path) -> String {
    format!("printf x >> {}", shell_quote(marker))
}

#[cfg(unix)]
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"))
}

#[cfg(unix)]
fn descendant_script(marker: &Path) -> String {
    format!(
        "(sleep 0.4; printf child > {}) & sleep 2",
        shell_quote(marker)
    )
}

#[cfg(windows)]
fn make_escape_link(link: &Path, target: &Path) {
    let output = Command::new("cmd")
        .args([
            "/C",
            "mklink",
            "/J",
            link.to_str().expect("link path should be Unicode"),
            target.to_str().expect("target path should be Unicode"),
        ])
        .output()
        .expect("junction command should start");
    assert!(
        output.status.success(),
        "junction fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
fn make_escape_link(link: &Path, target: &Path) {
    std::os::unix::fs::symlink(target, link).expect("symlink fixture should be created");
}

#[cfg(not(any(unix, windows)))]
fn make_escape_link(_link: &Path, _target: &Path) {
    panic!("P3 escape-link fixture requires a Windows or Unix platform");
}

#[tokio::test]
async fn p3_s01_tool_contracts_are_versioned_and_receipts_are_immutable() {
    assert_eq!(ToolExecutionService::contract_version(), 1);
    let schemas = coding_tool_schemas();
    // The schema set grows additively when tools are added, while the tool
    // contract version stays stable. M4/M5 added six tools; CP-B adds the
    // write/edit/glob and ask_user tools without changing old tool digests.
    assert_eq!(schemas.len(), 18);
    let names = schemas
        .iter()
        .map(|schema| {
            schema["function"]["name"]
                .as_str()
                .expect("every P3 schema has a function name")
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(names.len(), schemas.len());
    assert!(names.contains("apply_patch"));
    assert!(names.contains("run_process"));

    let expected_hash = ContentHash::from_bytes(b"before");
    let parsed = CodingToolAction::from_provider_call(
        "apply_patch",
        &serde_json::to_string(&json!({
            "path": "src/parser.txt",
            "expected_hash": expected_hash,
            "replacement": "after",
        }))
        .expect("provider arguments should serialize"),
    )
    .expect("typed action should parse");
    assert_eq!(
        parsed,
        CodingToolAction::ApplyPatch {
            path: "src/parser.txt".to_owned(),
            expected_hash,
            replacement: "after".to_owned(),
        }
    );

    let temp = TempDir::new().expect("temporary store");
    let root = setup_workspace(&temp);
    let store = writer(&temp).await;
    let (session_id, task_id, _) = admit(&store, &root).await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let (prepared, approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        CodingToolAction::ReadFile {
            path: "src/parser.txt".to_owned(),
            offset: None,
            limit: None,
            tail: None,
        },
    )
    .await;
    let replay_prepared = prepared.clone();
    let replay_approval = approval.clone();
    let view = tools
        .execute(prepared, Some(approval))
        .await
        .expect("approved read should settle");
    let receipt = view.receipt.clone().expect("read has immutable receipt");
    assert_eq!(receipt.intent_state, ToolIntentState::IntentRecorded);
    assert_eq!(receipt.outcome_state, ToolOutcomeState::Settled);
    let recovered = SessionService::new(Arc::clone(&store))
        .recover(&session_id)
        .await
        .expect("receipt should recover");
    assert_eq!(
        recovered
            .receipts
            .iter()
            .find(|candidate| candidate.tool_execution_id == receipt.tool_execution_id),
        Some(&receipt)
    );
    let error = tools
        .execute(replay_prepared, Some(replay_approval))
        .await
        .expect_err("a consumed approval cannot produce a second receipt");
    assert_eq!(error.code(), ErrorCode::ApprovalConsumed);
    let recovered_again = SessionService::new(Arc::clone(&store))
        .recover(&session_id)
        .await
        .expect("recovery after replay rejection");
    assert_eq!(recovered_again.receipts.len(), 1);
    drop(tools);
    close_writer(store).await;
}

#[tokio::test]
async fn p3_k08_parent_deny_cannot_be_overridden() {
    let temp = TempDir::new().expect("temporary store");
    let root = setup_workspace(&temp);
    let store = writer(&temp).await;
    let (session_id, task_id, _) = admit(&store, &root).await;
    let policy = ToolPolicy::new(
        7,
        vec![
            PolicyRule::deny("", "ancestor denial is final"),
            PolicyRule::allow("src", "later allow must never weaken denial"),
        ],
    );
    let tools = ToolExecutionService::new(Arc::clone(&store)).with_policy(policy);
    let prepared = tools
        .prepare(request(
            &session_id,
            &task_id,
            &root,
            "fixture.actor",
            CodingToolAction::ReadFile {
                path: "src/parser.txt".to_owned(),
                offset: None,
                limit: None,
                tail: None,
            },
        ))
        .await
        .expect("denied action still has a reviewable prepared proposal");
    let error = tools
        .approve(&prepared)
        .await
        .expect_err("ancestor deny must reject approval");
    assert_eq!(error.code(), ErrorCode::PolicyDenied);
    let denied = tools
        .execute(prepared, None)
        .await
        .expect("denial must be durably evidenced");
    assert!(matches!(
        denied.output,
        ToolOutput::Denied { ref code, .. } if code == "policy_denied"
    ));
    assert_eq!(
        denied.receipt.expect("denial has receipt").outcome_state,
        ToolOutcomeState::Denied
    );
    drop(tools);
    close_writer(store).await;
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one action of every kind, each through the gate"
)]
async fn p3_s02_every_tool_path_uses_one_gate_and_approval_binds_final_action() {
    let temp = TempDir::new().expect("temporary store");
    let root = setup_workspace(&temp);
    let store = writer(&temp).await;
    let (session_id, task_id, _) = admit(&store, &root).await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let (prepared, approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        CodingToolAction::ReadFile {
            path: "src/parser.txt".to_owned(),
            offset: None,
            limit: None,
            tail: None,
        },
    )
    .await;
    let different_actor = tools
        .prepare(request(
            &session_id,
            &task_id,
            &root,
            "different.actor",
            CodingToolAction::ReadFile {
                path: "src/parser.txt".to_owned(),
                offset: None,
                limit: None,
                tail: None,
            },
        ))
        .await
        .expect("different actor proposal should prepare");
    let mismatch = tools
        .execute(different_actor, Some(approval.clone()))
        .await
        .expect("binding mismatch must be a durable denial");
    assert!(matches!(
        mismatch.output,
        ToolOutput::Denied { ref code, .. } if code == "approval_stale"
    ));
    let success = tools
        .execute(prepared, Some(approval))
        .await
        .expect("the correctly bound proposal should still execute once");
    assert_eq!(
        success
            .receipt
            .expect("approved read has receipt")
            .outcome_state,
        ToolOutcomeState::Settled
    );

    let (stale_prepared, stale_approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        CodingToolAction::ReadFile {
            path: "src/parser.txt".to_owned(),
            offset: None,
            limit: None,
            tail: None,
        },
    )
    .await;
    fs::write(root.join("src/parser.txt"), "changed outside the harness\n")
        .expect("external edit fixture");
    // An edit elsewhere in the tree after approval does not make the approved
    // action stale: the read runs on the same root and sees the file as it is.
    // (A whole-tree comparison refused good actions whenever an indexer, an editor
    // or an export wrote a file mid-turn; writes are bound to their file's hash.)
    let drifted = tools
        .execute(stale_prepared, Some(stale_approval))
        .await
        .expect("the approved read runs");
    assert!(
        !matches!(drifted.output, ToolOutput::Denied { .. }),
        "{:?}",
        drifted.output
    );

    let (policy_prepared, policy_approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        CodingToolAction::ReadFile {
            path: "src/parser.txt".to_owned(),
            offset: None,
            limit: None,
            tail: None,
        },
    )
    .await;
    let revised =
        ToolExecutionService::new(Arc::clone(&store)).with_policy(ToolPolicy::new(2, Vec::new()));
    let policy_stale = revised
        .execute(policy_prepared, Some(policy_approval))
        .await
        .expect("policy revision mismatch must be durably denied");
    assert!(matches!(
        policy_stale.output,
        ToolOutput::Denied { ref code, .. } if code == "approval_stale"
    ));
    drop(revised);
    drop(tools);
    close_writer(store).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn p3_s03_rooted_files_search_and_patch_handle_crlf_unicode_and_stale_edits() {
    let temp = TempDir::new().expect("temporary store");
    let root = setup_workspace(&temp);
    let unicode_path = root.join("src").join("đặc biệt.txt");
    let before = "alpha\r\nbánh mì\r\n";
    fs::write(&unicode_path, before).expect("Unicode fixture file");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&outside).expect("outside fixture directory");
    fs::write(outside.join("outside.txt"), "outside\n").expect("outside fixture file");
    make_escape_link(&root.join("src").join("escape"), &outside);
    fs::write(root.join(".env"), "token=fixture-secret\n").expect("sensitive fixture file");
    fs::write(root.join("src").join("binary.bin"), [0_u8, 1, 2]).expect("binary fixture file");

    let store = writer(&temp).await;
    let (session_id, task_id, _) = admit(&store, &root).await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let (list_prepared, list_approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        CodingToolAction::ListFiles { path: None },
    )
    .await;
    let listed = tools
        .execute(list_prepared, Some(list_approval))
        .await
        .expect("list should execute");
    let ToolOutput::ListFiles { paths, .. } = listed.output else {
        panic!("list tool returned an unexpected output");
    };
    assert!(paths.iter().any(|path| path == "src/đặc biệt.txt"));
    assert!(!paths.iter().any(|path| path == "ignored.tmp"));
    assert!(!paths.iter().any(|path| path == ".env"));

    let (search_prepared, search_approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        CodingToolAction::SearchText {
            query: "bánh".to_owned(),
            path: Some("src".to_owned()),
            regex: false,
            case_insensitive: false,
            glob: None,
            context_lines: None,
            any_of: Vec::new(),
        },
    )
    .await;
    let searched = tools
        .execute(search_prepared, Some(search_approval))
        .await
        .expect("search should execute");
    let ToolOutput::SearchText { matches, .. } = searched.output else {
        panic!("search tool returned an unexpected output");
    };
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].path, "src/đặc biệt.txt");

    let replacement = "beta\r\nđã sửa\r\n";
    let (patch_prepared, patch_approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        CodingToolAction::ApplyPatch {
            path: "src/đặc biệt.txt".to_owned(),
            expected_hash: ContentHash::from_bytes(before.as_bytes()),
            replacement: replacement.to_owned(),
        },
    )
    .await;
    let patched = tools
        .execute(patch_prepared, Some(patch_approval))
        .await
        .expect("exact CRLF Unicode patch should execute");
    assert!(matches!(patched.output, ToolOutput::ApplyPatch { .. }));
    assert_eq!(fs::read_to_string(&unicode_path).unwrap(), replacement);

    let (stale_prepared, stale_approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        CodingToolAction::ApplyPatch {
            path: "src/đặc biệt.txt".to_owned(),
            expected_hash: ContentHash::from_bytes(b"stale hash"),
            replacement: "unexpected overwrite\n".to_owned(),
        },
    )
    .await;
    let stale = tools
        .execute(stale_prepared, Some(stale_approval))
        .await
        .expect("stale patch must be a durable denial before dispatch");
    assert!(matches!(
        stale.output,
        ToolOutput::Denied { ref code, .. } if code == "stale_workspace"
    ));
    assert_eq!(fs::read_to_string(&unicode_path).unwrap(), replacement);

    let error = tools
        .prepare(request(
            &session_id,
            &task_id,
            &root,
            "fixture.actor",
            CodingToolAction::ReadFile {
                path: "../outside/outside.txt".to_owned(),
                offset: None,
                limit: None,
                tail: None,
            },
        ))
        .await
        .expect_err("parent traversal must be rejected");
    assert_eq!(error.code(), ErrorCode::WorkspaceEscape);
    let error = tools
        .prepare(request(
            &session_id,
            &task_id,
            &root,
            "fixture.actor",
            CodingToolAction::ReadFile {
                path: "src/escape/outside.txt".to_owned(),
                offset: None,
                limit: None,
                tail: None,
            },
        ))
        .await
        .expect_err("link/reparse traversal must be rejected");
    assert_eq!(error.code(), ErrorCode::WorkspaceEscape);
    let error = tools
        .prepare(request(
            &session_id,
            &task_id,
            &root,
            "fixture.actor",
            CodingToolAction::ReadFile {
                path: ".env".to_owned(),
                offset: None,
                limit: None,
                tail: None,
            },
        ))
        .await
        .expect_err("sensitive file must be rejected");
    assert_eq!(error.code(), ErrorCode::SensitivePathDenied);
    let (binary_prepared, binary_approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        CodingToolAction::ReadFile {
            path: "src/binary.bin".to_owned(),
            offset: None,
            limit: None,
            tail: None,
        },
    )
    .await;
    let binary = tools
        .execute(binary_prepared, Some(binary_approval))
        .await
        .expect("binary refusal must be a durable denial");
    assert!(matches!(
        binary.output,
        ToolOutput::Denied { ref code, .. } if code == "binary_content_denied"
    ));
    drop(tools);
    close_writer(store).await;
}

#[tokio::test]
async fn p3_c10_external_workspace_change_invalidates_approved_execution() {
    let temp = TempDir::new().expect("temporary store");
    let root = setup_workspace(&temp);
    let store = writer(&temp).await;
    let (session_id, task_id, _) = admit(&store, &root).await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    // The change that matters is to the file the approved write targets: the
    // write is bound to that file's hash, so an edit made while it waited for its
    // approval turns the approved execution into a durable denial. (Edits
    // elsewhere in the tree no longer invalidate an approval; see the read case.)
    let before = fs::read_to_string(root.join("src/parser.txt")).expect("fixture");
    let (prepared, approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        CodingToolAction::ApplyPatch {
            path: "src/parser.txt".to_owned(),
            expected_hash: ContentHash::from_bytes(before.as_bytes()),
            replacement: "patched\n".to_owned(),
        },
    )
    .await;
    fs::write(root.join("src/parser.txt"), "external tracked change\n")
        .expect("external tracked change");
    let denied = tools
        .execute(prepared, Some(approval))
        .await
        .expect("drift must become a durable denial");
    assert!(matches!(
        denied.output,
        ToolOutput::Denied { ref code, .. } if code == "stale_workspace"
    ));
    let receipt = denied.receipt.expect("denial receipt");
    assert_eq!(receipt.outcome_state, ToolOutcomeState::Denied);
    assert!(receipt.after_fingerprint.is_none());
    drop(tools);
    close_writer(store).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn p3_s04_process_output_timeout_cancel_and_descendant_cleanup_are_bounded() {
    let temp = TempDir::new().expect("temporary store");
    let root = setup_workspace(&temp);
    let store = writer(&temp).await;
    let (session_id, task_id, _) = admit(&store, &root).await;
    let tools = ToolExecutionService::new(Arc::clone(&store));

    let (output_prepared, output_approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        process_action(output_script(), 10_000),
    )
    .await;
    let output = tools
        .execute(output_prepared, Some(output_approval))
        .await
        .expect("bounded process output should execute");
    let ToolOutput::Process {
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
        tree_cleanup_confirmed,
        tree_cleanup,
        ..
    } = output.output
    else {
        panic!("process tool returned an unexpected output");
    };
    assert!(stdout_truncated && stderr_truncated);
    assert!(stdout.len() <= 64 * 1024 && stderr.len() <= 64 * 1024);
    assert!(
        !tree_cleanup_confirmed,
        "a naturally reaped direct child is not proof that its whole tree exited"
    );
    assert_eq!(tree_cleanup, "reaped_on_exit");

    let (timeout_prepared, timeout_approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        process_action(&sleep_script(1_000), 30),
    )
    .await;
    let started = Instant::now();
    let timed_out = tools
        .execute(timeout_prepared, Some(timeout_approval))
        .await
        .expect("timeout should settle with cleanup evidence");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(matches!(
        timed_out.output,
        ToolOutput::Process {
            timed_out: true,
            canceled: false,
            tree_cleanup_confirmed: true,
            ..
        }
    ));

    let (cancel_prepared, cancel_approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        process_action(&sleep_script(2_000), 10_000),
    )
    .await;
    let cancellation = CancellationToken::new();
    let cancelled_tools = tools.clone();
    let cancelled_token = cancellation.clone();
    let handle = tokio::spawn(async move {
        cancelled_tools
            .execute_with_cancellation(cancel_prepared, Some(cancel_approval), cancelled_token)
            .await
    });
    sleep(Duration::from_millis(80)).await;
    cancellation.cancel();
    let cancelled = handle
        .await
        .expect("cancellation task joins")
        .expect("cancellation should settle a truthful process result");
    assert!(matches!(
        cancelled.output,
        ToolOutput::Process {
            timed_out: false,
            canceled: true,
            tree_cleanup_confirmed: true,
            ..
        }
    ));

    #[cfg(unix)]
    {
        let marker = root.join("descendant-marker.txt");
        let (descendant_prepared, descendant_approval) = prepared_and_approved(
            &tools,
            &session_id,
            &task_id,
            &root,
            process_action(&descendant_script(&marker), 40),
        )
        .await;
        let descendant = tools
            .execute(descendant_prepared, Some(descendant_approval))
            .await
            .expect("descendant timeout should settle");
        assert!(matches!(
            descendant.output,
            ToolOutput::Process {
                timed_out: true,
                tree_cleanup_confirmed: true,
                ..
            }
        ));
        sleep(Duration::from_millis(650)).await;
        assert!(
            !marker.exists(),
            "the killed process group must not leave a delayed descendant writer"
        );
    }

    drop(tools);
    close_writer(store).await;
}

#[tokio::test]
async fn p3_s04_strict_isolation_is_explicitly_denied_when_unavailable() {
    let temp = TempDir::new().expect("temporary store");
    let root = setup_workspace(&temp);
    let store = writer(&temp).await;
    let (session_id, task_id, _) = admit(&store, &root).await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    assert!(!ToolExecutionService::capabilities().strict_isolation);
    let (prepared, approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        CodingToolAction::RunProcess {
            executable: "definitely-not-started".to_owned(),
            args: Vec::new(),
            timeout_ms: 1_000,
            isolation: IsolationMode::Strict,
            env: Vec::new(),
        },
    )
    .await;
    let denied = tools
        .execute(prepared, Some(approval))
        .await
        .expect("unsupported strict isolation must be a receipt-backed denial");
    assert!(matches!(
        denied.output,
        ToolOutput::Denied { ref code, .. } if code == "strict_isolation_unavailable"
    ));
    assert_eq!(
        denied.receipt.expect("strict denial receipt").outcome_state,
        ToolOutcomeState::Denied
    );
    drop(tools);
    close_writer(store).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn p3_s05_git_identity_fingerprint_and_task_update_are_scoped() {
    let temp = TempDir::new().expect("temporary store");
    let root = setup_workspace(&temp);
    let store = writer(&temp).await;
    let (session_id, task_id, _) = admit(&store, &root).await;
    let tools = ToolExecutionService::new(Arc::clone(&store));

    let (status_prepared, status_approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        CodingToolAction::GitStatus,
    )
    .await;
    let status = tools
        .execute(status_prepared, Some(status_approval))
        .await
        .expect("Git status should use the real process runner");
    assert!(matches!(
        status.output,
        ToolOutput::Git { ref operation, .. } if operation == "status"
    ));

    let original = fs::read_to_string(root.join("src/parser.txt")).unwrap();
    let (patch_prepared, patch_approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        CodingToolAction::ApplyPatch {
            path: "src/parser.txt".to_owned(),
            expected_hash: ContentHash::from_bytes(original.as_bytes()),
            replacement: "fixed parser\n".to_owned(),
        },
    )
    .await;
    tools
        .execute(patch_prepared, Some(patch_approval))
        .await
        .expect("Git fixture patch should execute");
    let (diff_prepared, diff_approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        CodingToolAction::GitDiff {
            path: Some("src/parser.txt".to_owned()),
        },
    )
    .await;
    let diff = tools
        .execute(diff_prepared, Some(diff_approval))
        .await
        .expect("Git diff should execute");
    assert!(matches!(
        diff.output,
        ToolOutput::Git { ref operation, ref output, .. }
            if operation == "diff" && output.contains("fixed parser")
    ));

    let (update_prepared, update_approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        CodingToolAction::TaskUpdate {
            note: "run the parser test after review".to_owned(),
        },
    )
    .await;
    let update = tools
        .execute(update_prepared, Some(update_approval))
        .await
        .expect("task update should use its scoped projection path");
    assert!(update.execution_id.is_none() && update.receipt.is_none());
    let recovered = SessionService::new(Arc::clone(&store))
        .recover(&session_id)
        .await
        .expect("task update should survive recovery");
    assert!(
        recovered
            .working_state
            .next_action_proposals
            .iter()
            .any(|proposal| proposal.description == "run the parser test after review")
    );

    let linked = temp.path().join("linked-worktree");
    let linked_text = linked.to_str().expect("linked path Unicode").to_owned();
    git(&root, &["worktree", "add", "-b", "p3-linked", &linked_text]);
    let linked_prepared = tools
        .prepare(request(
            &session_id,
            &task_id,
            &linked,
            "fixture.actor",
            CodingToolAction::ReadFile {
                path: "src/parser.txt".to_owned(),
                offset: None,
                limit: None,
                tail: None,
            },
        ))
        .await
        .expect("linked worktree with shared Git common dir may register explicitly");
    assert_eq!(
        linked_prepared.action(),
        &CodingToolAction::ReadFile {
            path: "src/parser.txt".to_owned(),
            offset: None,
            limit: None,
            tail: None,
        }
    );
    drop(tools);
    close_writer(store).await;
}

#[tokio::test]
async fn p3_c23_project_identity_requires_explicit_reassociation() {
    let temp = TempDir::new().expect("temporary store");
    let root = setup_workspace(&temp);
    let clone = temp.path().join("independent-clone");
    let clone_text = clone.to_str().expect("clone path Unicode").to_owned();
    let root_text = root.to_str().expect("source path Unicode").to_owned();
    let output = Command::new("git")
        .args(["clone", &root_text, &clone_text])
        .output()
        .expect("clone command should start");
    assert!(
        output.status.success(),
        "clone fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let store = writer(&temp).await;
    let (session_id, task_id, _) = admit(&store, &root).await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let (source_prepared, source_approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        CodingToolAction::ReadFile {
            path: "src/parser.txt".to_owned(),
            offset: None,
            limit: None,
            tail: None,
        },
    )
    .await;
    tools
        .execute(source_prepared, Some(source_approval))
        .await
        .expect("source project registration should settle");
    let conflict = tools
        .prepare(request(
            &session_id,
            &task_id,
            &clone,
            "fixture.actor",
            CodingToolAction::ReadFile {
                path: "src/parser.txt".to_owned(),
                offset: None,
                limit: None,
                tail: None,
            },
        ))
        .await
        .expect_err("independent clone cannot silently share the project identity");
    assert_eq!(conflict.code(), ErrorCode::ProjectIdentityConflict);
    let canonical_source = fs::canonicalize(&root)
        .expect("canonical source root")
        .to_str()
        .expect("canonical source path Unicode")
        .to_owned();
    tools
        .reassociate_workspace(&session_id, &task_id, &canonical_source, &clone)
        .await
        .expect("explicit project reassociation should be required and sufficient");
    tools
        .prepare(request(
            &session_id,
            &task_id,
            &clone,
            "fixture.actor",
            CodingToolAction::ReadFile {
                path: "src/parser.txt".to_owned(),
                offset: None,
                limit: None,
                tail: None,
            },
        ))
        .await
        .expect("explicitly reassociated root should prepare");
    drop(tools);
    close_writer(store).await;
}

/// A coder's worktree is checked out from the host's own clone of the project,
/// so its Git common directory is not the checkout's. Measured: every tool call
/// of a delegated coder failed `project_identity_conflict`. The worktree the
/// host issued and recorded for the project is that explicit association; a
/// worktree nobody issued still is not.
#[tokio::test]
async fn p3_c23_a_host_issued_coder_worktree_belongs_to_its_project() {
    let temp = TempDir::new().expect("temporary store");
    let root = setup_workspace(&temp);
    let clone = temp.path().join("delegation-source.git");
    let git = |cwd: &Path, args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git starts");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(
        temp.path(),
        &[
            "clone",
            "--local",
            "--no-hardlinks",
            "--quiet",
            root.to_str().expect("root path Unicode"),
            clone.to_str().expect("clone path Unicode"),
        ],
    );
    let issued = temp.path().join("worktrees").join("wt-issued");
    let stray = temp.path().join("worktrees").join("wt-stray");
    for (path, branch) in [(&issued, "harness/p5/issued"), (&stray, "harness/p5/stray")] {
        git(
            &clone,
            &[
                "worktree",
                "add",
                "-b",
                branch,
                path.to_str().expect("worktree path Unicode"),
            ],
        );
    }
    let store = writer(&temp).await;
    let (session_id, task_id, project_id) = admit(&store, &root).await;
    store
        .upsert_worktree(&harness_store_sqlite::WorktreeRecordRow {
            worktree_id: "wt-issued".to_owned(),
            task_id: task_id.clone(),
            run_id: harness_types::AgentRunId::generate(),
            project_id,
            base_commit: "base".to_owned(),
            base_branch: "harness/base/issued".to_owned(),
            branch: "harness/p5/issued".to_owned(),
            path: issued.to_str().expect("worktree path Unicode").to_owned(),
            write_scope: vec![".".to_owned()],
            state: "ready".to_owned(),
            input_fingerprint: harness_types::ContentHash::from_bytes(b"input"),
            result_fingerprint: None,
            generation: 1,
        })
        .await
        .expect("the host records the worktree it issued");
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let read = || CodingToolAction::ReadFile {
        path: "src/parser.txt".to_owned(),
        offset: None,
        limit: None,
        tail: None,
    };
    tools
        .prepare(request(
            &session_id,
            &task_id,
            &root,
            "fixture.actor",
            read(),
        ))
        .await
        .expect("the checkout itself prepares");
    tools
        .prepare(request(
            &session_id,
            &task_id,
            &issued,
            "fixture.coder",
            read(),
        ))
        .await
        .expect("the issued coder worktree prepares");
    let stray = tools
        .prepare(request(
            &session_id,
            &task_id,
            &stray,
            "fixture.coder",
            read(),
        ))
        .await
        .expect_err("a worktree the host never issued is still another root");
    assert_eq!(stray.code(), ErrorCode::ProjectIdentityConflict);
    drop(tools);
    close_writer(store).await;
}

#[tokio::test]
async fn p3_c29_secret_output_is_redacted_and_artifact_scope_is_enforced() {
    let temp = TempDir::new().expect("temporary store");
    let root = setup_workspace(&temp);
    fs::write(
        root.join("notes.txt"),
        "api_key=fixture-secret-value\nnormal=visible\nRead source.txt and copy its source token into artifact.txt.\n",
    )
    .expect("secret fixture file");
    let store = writer(&temp).await;
    let (session_id, task_id, project_id) = admit(&store, &root).await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let (prepared, approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        CodingToolAction::ReadFile {
            path: "notes.txt".to_owned(),
            offset: None,
            limit: None,
            tail: None,
        },
    )
    .await;
    let view = tools
        .execute(prepared, Some(approval))
        .await
        .expect("secret fixture read should settle as redacted output");
    let ToolOutput::ReadFile { content, .. } = &view.output else {
        panic!("read tool returned an unexpected output");
    };
    assert!(content.contains("api_key=[REDACTED]"));
    assert!(!content.contains("fixture-secret-value"));
    assert!(content.contains("copy its source token into artifact.txt"));
    let receipt = view.receipt.expect("read has receipt");
    let artifact_id = receipt.artifact_id.expect("redacted output has artifact");
    assert!(
        store
            .artifact_is_scoped_to(&artifact_id, &project_id, &task_id)
            .await
            .expect("artifact scope lookup")
    );
    assert!(
        !store
            .artifact_is_scoped_to(&artifact_id, &ProjectId::generate(), &task_id)
            .await
            .expect("foreign project scope lookup")
    );
    assert!(
        !store
            .artifact_is_scoped_to(&artifact_id, &project_id, &TaskId::generate())
            .await
            .expect("foreign task scope lookup")
    );
    let artifact_bytes = fs::read(
        temp.path()
            .join("artifacts")
            .join(format!("{artifact_id}.bin")),
    )
    .expect("redacted artifact should exist");
    assert!(!String::from_utf8_lossy(&artifact_bytes).contains("fixture-secret-value"));
    drop(tools);
    close_writer(store).await;
}

#[tokio::test]
async fn p3_s06_storage_faults_leave_reconcilable_pending_or_unknown_work() {
    let temp = TempDir::new().expect("temporary store");
    let root = setup_workspace(&temp);
    let store = writer_with_fault(&temp, StoreFaultPoint::BeforeToolSettlementCommit).await;
    let (session_id, task_id, _) = admit(&store, &root).await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let original = fs::read_to_string(root.join("src/parser.txt")).unwrap();
    let replacement = "reconciled after real side effect\n";
    let (prepared, approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        CodingToolAction::ApplyPatch {
            path: "src/parser.txt".to_owned(),
            expected_hash: ContentHash::from_bytes(original.as_bytes()),
            replacement: replacement.to_owned(),
        },
    )
    .await;
    let error = tools
        .execute(prepared, Some(approval))
        .await
        .expect_err("settlement fault must never claim a success receipt");
    assert_eq!(error.code(), ErrorCode::StorageWriteFailed);
    assert_eq!(
        fs::read_to_string(root.join("src/parser.txt")).unwrap(),
        replacement
    );
    let pending = store
        .pending_tool_intents(&session_id)
        .await
        .expect("durable intent should be recoverable");
    assert_eq!(pending.len(), 1);
    let execution_id = pending[0].tool_execution_id.clone();
    drop(tools);
    close_writer(store).await;

    let reopened = writer(&temp).await;
    let recovered_tools = ToolExecutionService::new(Arc::clone(&reopened));
    let reconciled = recovered_tools
        .reconcile_pending(&session_id, &execution_id)
        .await
        .expect("exact post-state should reconcile without a rerun");
    assert!(matches!(reconciled.output, ToolOutput::ApplyPatch { .. }));
    assert_eq!(
        reconciled
            .receipt
            .expect("reconciliation writes a receipt")
            .outcome_state,
        ToolOutcomeState::Settled
    );
    assert!(
        reopened
            .pending_tool_intents(&session_id)
            .await
            .expect("pending lookup")
            .is_empty()
    );
    assert_eq!(
        fs::read_to_string(root.join("src/parser.txt")).unwrap(),
        replacement
    );
    drop(recovered_tools);
    close_writer(reopened).await;
}

#[tokio::test]
async fn p3_c03_unknown_side_effect_is_reconciled_without_blind_rerun() {
    let temp = TempDir::new().expect("temporary store");
    let root = setup_workspace(&temp);
    let marker = root.join("side-effect-marker.txt");
    let store = writer_with_fault(&temp, StoreFaultPoint::BeforeToolSettlementCommit).await;
    let (session_id, task_id, _) = admit(&store, &root).await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let (prepared, approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        process_action(&append_marker_script(&marker), 10_000),
    )
    .await;
    let error = tools
        .execute(prepared, Some(approval))
        .await
        .expect_err("crash-point settlement fault must surface after real side effect");
    assert_eq!(error.code(), ErrorCode::StorageWriteFailed);
    assert_eq!(fs::read_to_string(&marker).unwrap(), "x");
    let execution_id = store
        .pending_tool_intents(&session_id)
        .await
        .expect("pending intent lookup")[0]
        .tool_execution_id
        .clone();
    drop(tools);
    close_writer(store).await;

    let reopened = writer(&temp).await;
    let recovered_tools = ToolExecutionService::new(Arc::clone(&reopened));
    let outcome = recovered_tools
        .reconcile_pending(&session_id, &execution_id)
        .await
        .expect("unknown process intent should be reconciled without execution");
    assert!(matches!(outcome.output, ToolOutput::OutcomeUnknown { .. }));
    assert_eq!(
        outcome
            .receipt
            .expect("unknown outcome has receipt")
            .outcome_state,
        ToolOutcomeState::OutcomeUnknown
    );
    assert_eq!(
        fs::read_to_string(&marker).unwrap(),
        "x",
        "reconciliation must not rerun an unknown process"
    );
    drop(recovered_tools);
    close_writer(reopened).await;
}

#[tokio::test]
async fn p3_c21_receipt_commit_failure_never_claims_success() {
    let temp = TempDir::new().expect("temporary store");
    let root = setup_workspace(&temp);
    let marker = root.join("intent-marker.txt");
    let store = writer_with_fault(&temp, StoreFaultPoint::BeforeToolIntentCommit).await;
    let (session_id, task_id, _) = admit(&store, &root).await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let (prepared, approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        process_action(&append_marker_script(&marker), 10_000),
    )
    .await;
    let error = tools
        .execute(prepared, Some(approval))
        .await
        .expect_err("intent commit failure must stop dispatch");
    assert_eq!(error.code(), ErrorCode::StorageWriteFailed);
    sleep(Duration::from_millis(100)).await;
    assert!(
        !marker.exists(),
        "no process side effect may happen without a durable intent"
    );
    assert!(
        store
            .pending_tool_intents(&session_id)
            .await
            .expect("pending intent lookup")
            .is_empty()
    );
    drop(tools);
    close_writer(store).await;
}

#[tokio::test]
async fn p3_k09_observer_failure_cannot_rewrite_receipt() {
    struct FailingObserver;

    impl harness_tools::ToolObserver for FailingObserver {
        fn observe(&self, _view: &harness_tools::ToolExecutionView) -> Result<(), String> {
            Err("renderer incorrectly reported a success".to_owned())
        }
    }

    let temp = TempDir::new().expect("temporary store");
    let root = setup_workspace(&temp);
    let store = writer(&temp).await;
    let (session_id, task_id, _) = admit(&store, &root).await;
    let tools =
        ToolExecutionService::new(Arc::clone(&store)).with_observer(Arc::new(FailingObserver));
    let prepared = tools
        .prepare(request(
            &session_id,
            &task_id,
            &root,
            "fixture.actor",
            CodingToolAction::ReadFile {
                path: "src/parser.txt".to_owned(),
                offset: None,
                limit: None,
                tail: None,
            },
        ))
        .await
        .expect("proposal should prepare");
    let denied = tools
        .execute(prepared, None)
        .await
        .expect("missing approval should produce a receipt-backed denial");
    assert!(denied.observer_failure.is_some());
    assert!(matches!(denied.output, ToolOutput::Denied { .. }));
    let receipt = denied.receipt.expect("denial receipt");
    assert_eq!(receipt.outcome_state, ToolOutcomeState::Denied);
    let recovery = SessionService::new(Arc::clone(&store))
        .recover(&session_id)
        .await
        .expect("recovery after observer failure");
    assert_eq!(
        recovery
            .receipts
            .iter()
            .find(|candidate| candidate.tool_execution_id == receipt.tool_execution_id)
            .map(|candidate| candidate.outcome_state),
        Some(ToolOutcomeState::Denied)
    );
    drop(tools);
    close_writer(store).await;
}

/// `ha code capabilities` reports the tool contract and the schemas this host
/// advertises.
#[test]
fn p3_s07_code_capabilities_reports_the_tool_contract() {
    let capability = run_ha(&["code", "capabilities", "--json"]);
    assert!(
        capability.status.success(),
        "{}",
        String::from_utf8_lossy(&capability.stderr)
    );
    let capabilities: Value =
        serde_json::from_slice(&capability.stdout).expect("capabilities JSON");
    assert_eq!(capabilities["tool_contract_version"], 1);
    // M4/M5 tools and the CP-B write/edit/glob/ask_user tools are advertised
    // additively without changing the tool contract version.
    assert_eq!(capabilities["tool_schema_count"], 18);
}

#[tokio::test]
async fn a_root_that_becomes_a_repository_keeps_its_project() {
    let temp = TempDir::new().expect("temporary store");
    let root = temp.path().join("plain-folder");
    fs::create_dir_all(root.join("src")).expect("fixture source directory");
    fs::write(root.join("src").join("parser.txt"), "BUG parser\r\n").expect("fixture file");
    let store = writer(&temp).await;
    let (session_id, task_id, _) = admit(&store, &root).await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let read = || CodingToolAction::ReadFile {
        path: "src/parser.txt".to_owned(),
        offset: None,
        limit: None,
        tail: None,
    };
    let (prepared, approval) =
        prepared_and_approved(&tools, &session_id, &task_id, &root, read()).await;
    tools
        .execute(prepared, Some(approval))
        .await
        .expect("the plain folder registers");

    git(&root, &["init"]);
    let (prepared, approval) =
        prepared_and_approved(&tools, &session_id, &task_id, &root, read()).await;
    tools
        .execute(prepared, Some(approval))
        .await
        .expect("the same root, now a repository, is the same project");
    drop(tools);
    close_writer(store).await;
}

/// The user saw a model overwrite a file with `write_file`, get
/// `stale_workspace`, compute the hash itself with node and fail again with
/// `invalid_hash: hash must start with sha256:`: nothing it could read carried
/// the hash `expected_hash` wants. `read_file` now shows the whole file's hash,
/// even for a partial range, and that value is what `write_file` accepts.
#[tokio::test]
async fn read_file_shows_the_hash_write_file_takes() {
    let temp = TempDir::new().expect("temporary store");
    let root = setup_workspace(&temp);
    fs::write(root.join("page.ts"), "one\ntwo\nthree\n").expect("fixture file");
    let store = writer(&temp).await;
    let (session_id, task_id, _project_id) = admit(&store, &root).await;
    let tools = ToolExecutionService::new(Arc::clone(&store));

    let (prepared, approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        CodingToolAction::ReadFile {
            path: "page.ts".to_owned(),
            offset: None,
            limit: Some(1),
            tail: None,
        },
    )
    .await;
    let view = tools
        .execute(prepared, Some(approval))
        .await
        .expect("read settles");
    let ToolOutput::ReadFile { hash, .. } = &view.output else {
        panic!("read tool returned an unexpected output");
    };
    assert_eq!(*hash, ContentHash::from_bytes(b"one\ntwo\nthree\n"));

    // Without the hash the error says where to get it.
    let refused = tools
        .prepare(request(
            &session_id,
            &task_id,
            &root,
            "p3-hash",
            CodingToolAction::WriteFile {
                path: "page.ts".to_owned(),
                content: "new\n".to_owned(),
                expected_hash: None,
            },
        ))
        .await
        .expect_err("an overwrite without the hash is refused");
    assert!(refused.to_string().contains("read_file"), "{refused}");

    let (prepared, approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        CodingToolAction::WriteFile {
            path: "page.ts".to_owned(),
            content: "new\n".to_owned(),
            expected_hash: Some(hash.clone()),
        },
    )
    .await;
    tools
        .execute(prepared, Some(approval))
        .await
        .expect("the hash read_file showed overwrites the file");
    assert_eq!(
        fs::read_to_string(root.join("page.ts")).expect("rewritten"),
        "new\n"
    );
    drop(tools);
    close_writer(store).await;
}
