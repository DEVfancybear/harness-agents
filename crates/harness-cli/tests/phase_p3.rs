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
                .denial_for(&CodingToolAction::ReadFile { path: path.into() })
                .is_some(),
            "policy bypass: {path}"
        );
    }
    #[cfg(windows)]
    assert!(
        policy
            .denial_for(&CodingToolAction::ReadFile {
                path: "SRC\\PRIVATE\\file.txt".into()
            })
            .is_some()
    );
    for action in [
        CodingToolAction::ListFiles { path: None },
        CodingToolAction::SearchText {
            query: "x".into(),
            path: Some("src".into()),
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
                path: "src/private-other/file.txt".into()
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
async fn review_p3_search_redacts_before_truncation() {
    let temp = TempDir::new().unwrap();
    let root = setup_workspace(&temp);
    let line = format!("FAKE_VALUE_ONLY_FOR_TEST {} token", "x".repeat(300));
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
        },
    )
    .await;
    let view = tools.execute(prepared, Some(approval)).await.unwrap();
    let ToolOutput::SearchText { matches, .. } = view.output else {
        panic!("expected search output")
    };
    assert_eq!(matches.len(), 1);
    assert!(!matches[0].preview.contains("FAKE_VALUE_ONLY_FOR_TEST"));
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
    assert_eq!(schemas.len(), 9);
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
        },
    )
    .await;
    fs::write(root.join("src/parser.txt"), "changed outside the harness\n")
        .expect("external edit fixture");
    let stale = tools
        .execute(stale_prepared, Some(stale_approval))
        .await
        .expect("workspace drift must be durably denied");
    assert!(matches!(
        stale.output,
        ToolOutput::Denied { ref code, .. } if code == "stale_workspace"
    ));

    let (policy_prepared, policy_approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        CodingToolAction::ReadFile {
            path: "src/parser.txt".to_owned(),
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
    let (prepared, approval) = prepared_and_approved(
        &tools,
        &session_id,
        &task_id,
        &root,
        CodingToolAction::ReadFile {
            path: "src/parser.txt".to_owned(),
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
        ..
    } = output.output
    else {
        panic!("process tool returned an unexpected output");
    };
    assert!(stdout_truncated && stderr_truncated);
    assert!(stdout.len() <= 64 * 1024 && stderr.len() <= 64 * 1024);
    assert!(tree_cleanup_confirmed);

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
            },
        ))
        .await
        .expect("linked worktree with shared Git common dir may register explicitly");
    assert_eq!(
        linked_prepared.action(),
        &CodingToolAction::ReadFile {
            path: "src/parser.txt".to_owned()
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
            },
        ))
        .await
        .expect("explicitly reassociated root should prepare");
    drop(tools);
    close_writer(store).await;
}

#[tokio::test]
async fn p3_c29_secret_output_is_redacted_and_artifact_scope_is_enforced() {
    let temp = TempDir::new().expect("temporary store");
    let root = setup_workspace(&temp);
    fs::write(
        root.join("notes.txt"),
        "api_key=fixture-secret-value\nnormal=visible\n",
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

#[tokio::test]
async fn p3_s07_cli_fixture_runs_p2_response_through_p3_tools_and_recovers_after_restart() {
    let temp = TempDir::new().expect("temporary fixture");
    let root = setup_workspace(&temp);
    let data_dir = temp.path().join("cli-data");
    let data_dir_text = data_dir.to_str().expect("data directory Unicode");
    let root_text = root.to_str().expect("workspace Unicode");
    let capability = run_ha(&["code", "capabilities", "--json"]);
    assert!(
        capability.status.success(),
        "{}",
        String::from_utf8_lossy(&capability.stderr)
    );
    let capabilities: Value =
        serde_json::from_slice(&capability.stdout).expect("capabilities JSON");
    assert_eq!(capabilities["tool_contract_version"], 1);
    assert_eq!(capabilities["tool_schema_count"], 9);

    let fixture = run_ha(&[
        "code",
        "fixture",
        "--data-dir",
        data_dir_text,
        "--workspace",
        root_text,
        "--path",
        "src/parser.txt",
        "--find",
        "BUG",
        "--replace",
        "FIXED parser",
        "--approve",
        "--json",
    ]);
    assert!(
        fixture.status.success(),
        "{}",
        String::from_utf8_lossy(&fixture.stderr)
    );
    let output: Value = serde_json::from_slice(&fixture.stdout).expect("fixture JSON");
    assert_eq!(output["provider_tool_call_count"], 3);
    assert_eq!(output["executions"].as_array().map_or(0, Vec::len), 3);
    assert_eq!(
        fs::read_to_string(root.join("src/parser.txt")).unwrap(),
        "FIXED parser"
    );
    let session_id = output["session_id"].as_str().expect("fixture session ID");
    let resume = run_ha(&[
        "resume",
        "--data-dir",
        data_dir_text,
        "--session-id",
        session_id,
        "--json",
    ]);
    assert!(
        resume.status.success(),
        "{}",
        String::from_utf8_lossy(&resume.stderr)
    );
    let status = run_ha(&[
        "status",
        "--data-dir",
        data_dir_text,
        "--session-id",
        session_id,
        "--json",
    ]);
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let status_json: Value = serde_json::from_slice(&status.stdout).expect("status JSON");
    assert_eq!(status_json["recovery"]["receipt_count"], 3);
    let replay = run_ha(&[
        "session",
        "replay",
        "--data-dir",
        data_dir_text,
        "--session-id",
        session_id,
        "--offline",
        "--json",
    ]);
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    let replay_json: Value = serde_json::from_slice(&replay.stdout).expect("replay JSON");
    assert_eq!(replay_json["dispatch_count"], 0);
}
