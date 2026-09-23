//! M4 acceptance: coding tools and the host runner.
//!
//! Everything here is real except the model: a `SQLite` store with writer
//! fencing, the P3 tool gate (policy, approval, intent, receipt), real files in
//! a disposable Git repo, and scripted providers. M4-01 covers the approval
//! scope, expiry/revoke, the descriptor registry and the provider `call_id`
//! correlation; later items extend this file for A03/A04/A08/A13/A16/A17.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use harness_providers::{
    CancellationToken, MessageRole, ModelCapabilities, ModelProvider, ProviderFuture,
    ProviderRequest, ProviderStreamEvent,
};
use harness_runtime::{
    EvidenceKind, GoalCriterion, GoalSpec, RunRequest, RuntimeConfig, RuntimeService,
};
use harness_session::{AdmitInputRequest, SessionService};
use harness_store_sqlite::{SqliteStore, ToolIntentStatus, WriterOpenOptions};
use harness_tools::{
    AcceptanceState, ApprovalMode, CaptureStream, CodingToolAction, EffectClass, EnvBinding,
    IsolationMode, PROCESS_ENVIRONMENT_ALLOWLIST, ProcessSpoolConfig, SecretResolver, SpoolLimits,
    ToolExecutionService, ToolOutput, ToolPolicy, ToolRequest, TurnDriver, TurnLimits,
    TurnObserver, TurnOptions, TurnOutcome, TurnProgress, TurnStop, coding_tool_descriptors,
    coding_tool_names, coding_tool_schemas, effect_class_for, observe_workspace,
    observed_file_hash, parse_capture_header,
};
use harness_types::{
    ContentHash, ErrorCode, HarnessError, HostId, InputId, ProjectId, SessionId, SourceAuthority,
    TaskId, ToolIntentState, ToolOutcomeState,
};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

struct Bench {
    temp: tempfile::TempDir,
    data_dir: PathBuf,
    workspace: PathBuf,
    /// One identity per fixture root: registering the same root under two
    /// generated ids is a real conflict, not a test accident.
    project_id: ProjectId,
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

async fn close(store: Arc<SqliteStore>) {
    Arc::try_unwrap(store)
        .expect("store consumers released")
        .close()
        .await
        .expect("store closes");
}

fn bench() -> Bench {
    let temp = tempfile::tempdir().expect("temp root");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(workspace.join("src")).expect("source dir");
    std::fs::write(workspace.join("src").join("parser.txt"), "BUG parser\r\n")
        .expect("fixture parser");
    git(&workspace, &["init"]);
    git(&workspace, &["config", "user.email", "m4@example.invalid"]);
    git(&workspace, &["config", "user.name", "M4 Fixture"]);
    git(&workspace, &["add", "."]);
    git(&workspace, &["commit", "-m", "fixture baseline"]);
    let data_dir = temp.path().join("data");
    Bench {
        temp,
        data_dir,
        workspace,
        project_id: ProjectId::generate(),
    }
}

fn git(root: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(root)
        .output()
        .expect("git starts");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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
            raw_text: "repair the fixture".to_owned(),
            workspace: observe_workspace(bench.project_id.clone(), &bench.workspace)
                .expect("observation"),
            initial_plan_items: Vec::new(),
        })
        .await
        .expect("input admitted");
    (session_id, task_id)
}

fn patch_action(root: &Path) -> CodingToolAction {
    let expected_hash = observed_file_hash(root, "src/parser.txt").expect("file hash");
    CodingToolAction::ApplyPatch {
        path: "src/parser.txt".to_owned(),
        expected_hash,
        replacement: "FIXED parser\r\n".to_owned(),
    }
}

fn file_text(root: &Path) -> String {
    std::fs::read_to_string(root.join("src").join("parser.txt")).expect("file readable")
}

fn receipt_denied_code(view: &harness_tools::ToolExecutionView) -> Option<ErrorCode> {
    let receipt = view.receipt.as_ref()?;
    assert_eq!(receipt.outcome_state, ToolOutcomeState::Denied);
    assert_eq!(receipt.intent_state, ToolIntentState::Denied);
    Some(ErrorCode::PolicyDenied)
}

/// Deterministic provider: one scripted response per call, recording requests.
struct ScriptedProvider {
    responses: Vec<Vec<ProviderStreamEvent>>,
    calls: AtomicUsize,
    seen: Mutex<Vec<ProviderRequest>>,
}

impl ScriptedProvider {
    fn new(responses: Vec<Vec<ProviderStreamEvent>>) -> Self {
        assert!(!responses.is_empty());
        Self {
            responses,
            calls: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn seen(&self) -> Vec<ProviderRequest> {
        self.seen.lock().expect("request log").clone()
    }
}

impl ModelProvider for ScriptedProvider {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::deepseek_fixture()
    }

    fn stream(&self, request: ProviderRequest, _cancellation: CancellationToken) -> ProviderFuture {
        self.seen.lock().expect("request log").push(request.clone());
        let index = self
            .calls
            .fetch_add(1, Ordering::SeqCst)
            .min(self.responses.len() - 1);
        let mut events = self.responses[index].clone();
        if let Some(ProviderStreamEvent::Started { request_id }) = events.first_mut() {
            *request_id = request.request_id;
        }
        Box::pin(async move { Ok(events) })
    }
}

#[derive(Default)]
struct SilentObserver;

impl TurnObserver for SilentObserver {
    fn observe(&self, _progress: TurnProgress) {}
}

/// The process permit is host-wide, which is the behavior under test: a call in
/// this test binary holds it against every other call in the same binary. Two
/// tests that assert *queue* semantics therefore have to take turns, or one of
/// them observes the other's process holding the permit and its own call
/// legitimately reports `queued`.
fn process_queue_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

// ---------------------------------------------------------------------------
// M4-01: approval, binding, expiry/revoke, descriptors, call correlation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a14_approval_binding() {
    let bench = bench();
    let store = bench.open_store().await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let (session_a, task_a) = admit(&store, &bench).await;
    let (session_b, task_b) = admit(&store, &bench).await;

    // The same command for two different invocations. A grant issued for A must
    // never authorize B, even though actor/action/workspace are identical.
    let prepared_a = tools
        .prepare(ToolRequest::new(
            session_a.clone(),
            task_a.clone(),
            "actor.a",
            &bench.workspace,
            patch_action(&bench.workspace),
        ))
        .await
        .expect("A prepares");
    let grant_a = tools.approve(&prepared_a).await.expect("A approved");
    let prepared_b = tools
        .prepare(ToolRequest::new(
            session_b.clone(),
            task_b.clone(),
            "actor.a",
            &bench.workspace,
            patch_action(&bench.workspace),
        ))
        .await
        .expect("B prepares");

    let denied = tools
        .execute(prepared_b, Some(grant_a.clone()))
        .await
        .expect("denial is a typed outcome, not a crash");
    assert!(receipt_denied_code(&denied).is_some(), "B must be denied");
    assert_eq!(file_text(&bench.workspace), "BUG parser\r\n");
    assert!(
        store
            .pending_tool_intents(&session_b)
            .await
            .unwrap()
            .is_empty(),
        "a denied call never creates an intent"
    );

    // A executes once with its own grant.
    let applied = tools
        .execute(prepared_a, Some(grant_a.clone()))
        .await
        .expect("A executes");
    let applied_receipt = applied.receipt.expect("receipt");
    assert_eq!(applied_receipt.outcome_state, ToolOutcomeState::Settled);
    assert_eq!(applied_receipt.call_id, None, "no model call, no call id");
    assert_eq!(file_text(&bench.workspace), "FIXED parser\r\n");

    // Two consumers race the same grant: exactly one intent, one refusal.
    let prepared_c = tools
        .prepare(ToolRequest::new(
            session_a.clone(),
            task_a.clone(),
            "actor.a",
            &bench.workspace,
            CodingToolAction::SearchText {
                query: "FIXED".to_owned(),
                path: None,
            },
        ))
        .await
        .expect("C prepares");
    let grant_c = tools.approve(&prepared_c).await.expect("C approved");
    let first = tools.execute(prepared_c.clone(), Some(grant_c.clone()));
    let second = tools.execute(prepared_c, Some(grant_c));
    let (first, second) = tokio::join!(first, second);
    let outcomes = [&first, &second];
    let applied = outcomes.iter().filter(|result| result.is_ok()).count();
    let refused = outcomes
        .iter()
        .filter_map(|result| result.as_ref().err())
        .map(harness_types::HarnessError::code)
        .collect::<Vec<_>>();
    assert_eq!(applied, 1, "exactly one consumer consumes the grant");
    assert_eq!(refused.len(), 1, "the loser is refused: {refused:?}");
    assert!(
        matches!(
            refused[0],
            ErrorCode::ApprovalConsumed | ErrorCode::ApprovalStale | ErrorCode::SequenceConflict
        ),
        "the losing consumer is refused with a typed code: {refused:?}"
    );

    let receipts = SessionService::new(Arc::clone(&store))
        .recover(&session_a)
        .await
        .expect("recovery")
        .receipts;
    assert_eq!(receipts.len(), 2, "one patch plus one raced search");
    drop(tools);
    close(store).await;
}

#[tokio::test]
async fn a11_approval_grant() {
    let bench = bench();
    let store = bench.open_store().await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let (session, task) = admit(&store, &bench).await;

    // An already expired approval cannot even be issued.
    let prepared = tools
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.a",
            &bench.workspace,
            patch_action(&bench.workspace),
        ))
        .await
        .expect("prepares");
    let error = tools
        .approve_with_expiry(&prepared, Some(1))
        .await
        .expect_err("an expired approval is refused at issue");
    assert_eq!(error.code(), ErrorCode::ApprovalStale);

    // An approval that expires before consume is a refusal with no write.
    let grant = tools
        .approve_with_expiry(
            &prepared,
            Some(harness_runtime::now_unix_ms().saturating_add(30)),
        )
        .await
        .expect("short-lived approval");
    tokio::time::sleep(Duration::from_millis(60)).await;
    let denied = tools
        .execute(prepared, Some(grant))
        .await
        .expect("expiry is a typed denial");
    assert!(receipt_denied_code(&denied).is_some());
    assert_eq!(file_text(&bench.workspace), "BUG parser\r\n");

    // A revoked approval never executes.
    let prepared = tools
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.a",
            &bench.workspace,
            patch_action(&bench.workspace),
        ))
        .await
        .expect("prepares");
    let grant = tools.approve(&prepared).await.expect("approved");
    tools
        .revoke_approval(&grant, "operator changed their mind")
        .await
        .expect("revoked");
    match tools.execute(prepared, Some(grant)).await {
        Ok(denied) => assert!(receipt_denied_code(&denied).is_some()),
        Err(error) => assert_eq!(error.code(), ErrorCode::ApprovalRevoked),
    }
    assert_eq!(file_text(&bench.workspace), "BUG parser\r\n");
    assert!(
        store
            .pending_tool_intents(&session)
            .await
            .unwrap()
            .is_empty()
    );

    drop(tools);
    close(store).await;
}

#[tokio::test]
async fn m4_01_descriptor_registry_is_versioned() {
    let descriptors = coding_tool_descriptors();
    let mut expected = coding_tool_names().to_vec();
    expected.sort_unstable();
    let ids = descriptors
        .iter()
        .map(|descriptor| descriptor.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(ids, expected, "every advertised tool has a descriptor");
    assert_eq!(
        descriptors.len(),
        coding_tool_schemas().len(),
        "one descriptor per provider schema"
    );
    for descriptor in &descriptors {
        assert_eq!(descriptor.revision, 1, "revision is the tool contract");
        assert!(
            descriptor.schema_digest.starts_with("sha256:"),
            "schema digest is a real hash: {}",
            descriptor.schema_digest
        );
        assert!(!descriptor.capabilities.is_empty());
    }
    let patch = descriptors
        .iter()
        .find(|descriptor| descriptor.id == "apply_patch")
        .expect("apply_patch advertised");
    assert_eq!(patch.effect_class, EffectClass::Mutating);
    assert_eq!(effect_class_for("read_file"), EffectClass::ReadOnly);
    assert_eq!(effect_class_for("run_process"), EffectClass::External);
    assert_eq!(effect_class_for("git_status"), EffectClass::ReadOnly);

    // The schema digest is stable across calls: a schema change without a
    // descriptor revision would show up here.
    let again = coding_tool_descriptors();
    assert_eq!(descriptors, again);
}

#[tokio::test]
async fn m4_01_call_id_is_bound_to_intent_and_receipt() {
    let bench = bench();
    let store = bench.open_store().await;
    let provider = Arc::new(ScriptedProvider::new(vec![
        vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::tool_delta(
                "call-42",
                "search_text",
                serde_json::json!({"query": "BUG", "path": "src"}).to_string(),
            ),
            ProviderStreamEvent::completed("tool_calls"),
        ],
        vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::text("found it"),
            ProviderStreamEvent::completed("stop"),
        ],
    ]));
    let runtime = Arc::new(RuntimeService::new(
        Arc::clone(&store),
        provider.clone(),
        RuntimeConfig::default(),
    ));
    // The runtime admits the input; admitting here as well would be a second
    // input in the same session.
    let session = SessionId::generate();
    let task = TaskId::generate();
    let driver = TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(&store)));
    let request = RunRequest::new(
        session.clone(),
        task.clone(),
        InputId::generate(),
        "find the bug".to_owned(),
        observe_workspace(bench.project_id.clone(), &bench.workspace).unwrap(),
    )
    .with_tool_schemas(coding_tool_schemas());
    let outcome = driver
        .run_turn(
            request,
            TurnOptions {
                workspace_root: bench.workspace.clone(),
                actor_id: "m4.test".to_owned(),
                approvals: ApprovalMode::Auto,
                limits: TurnLimits::default(),
            },
            Arc::new(SilentObserver),
            CancellationToken::new(),
        )
        .await
        .expect("turn runs");
    assert_eq!(outcome.tool_calls, 1_u32);

    // The durable intent keeps the provider call id next to the host identity.
    let execution_id = outcome
        .executions
        .first()
        .and_then(|view| view.execution_id.clone())
        .expect("execution id");
    let intent = store
        .tool_intent(&execution_id)
        .await
        .expect("read intent")
        .expect("intent exists");
    assert_eq!(intent.call_id.as_deref(), Some("call-42"));
    assert_eq!(intent.invocation_id, intent.invocation_id); // host id is separate
    assert_ne!(intent.invocation_id, "call-42");

    // The immutable receipt carries it too.
    let receipts = SessionService::new(Arc::clone(&store))
        .recover(&session)
        .await
        .expect("recovery")
        .receipts;
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].call_id.as_deref(), Some("call-42"));

    // And the tool result handed back to the model quotes the same call id.
    let seen = provider.seen();
    let second = seen.get(1).expect("second request");
    assert!(
        second.messages.iter().any(|message| {
            message.role == MessageRole::Tool && message.tool_call_id.as_deref() == Some("call-42")
        }),
        "the tool result is paired with the provider call: {:?}",
        second.messages
    );
    assert_eq!(second.messages[0].role, MessageRole::System);
    drop(driver);
    close(store).await;
}

#[tokio::test]
async fn m4_01_tools_schema_upgrade() {
    let bench = bench();
    let store = bench.open_store().await;
    let (session, task) = admit(&store, &bench).await;
    close(store).await;

    // Rewrite the tools surface back to its M3 shape: the v2 columns are gone
    // and the marker says 1, exactly like a database P3 left behind.
    let url = format!(
        "sqlite://{}",
        bench
            .data_dir
            .join("harness.sqlite3")
            .display()
            .to_string()
            .replace('\\', "/")
    );
    let pool = sqlx::SqlitePool::connect(&url)
        .await
        .expect("raw connection");
    for statement in [
        "DROP TABLE tool_intents",
        "DROP TABLE tool_approvals",
        "CREATE TABLE tool_approvals (approval_id TEXT PRIMARY KEY, actor_id TEXT NOT NULL, binding_hash TEXT NOT NULL, action_hash TEXT NOT NULL, workspace_root TEXT NOT NULL, workspace_fingerprint TEXT NOT NULL, policy_revision INTEGER NOT NULL, tool_revision INTEGER NOT NULL, expires_at_unix_ms INTEGER, state TEXT NOT NULL, approval_json TEXT NOT NULL, consumed_by TEXT, revoked_reason TEXT)",
        "CREATE TABLE tool_intents (tool_execution_id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(session_id), task_id TEXT NOT NULL, invocation_id TEXT NOT NULL, actor_id TEXT NOT NULL, tool_name TEXT NOT NULL, action_json TEXT NOT NULL, action_hash TEXT NOT NULL, workspace_root TEXT NOT NULL, workspace_fingerprint TEXT NOT NULL, before_fingerprint TEXT, policy_revision INTEGER NOT NULL, tool_revision INTEGER NOT NULL, approval_id TEXT NOT NULL REFERENCES tool_approvals(approval_id), status TEXT NOT NULL, intent_sequence INTEGER NOT NULL, intent_event_id TEXT NOT NULL REFERENCES events(event_id), settlement_receipt_id TEXT)",
        "DELETE FROM tools_schema_migrations",
        "INSERT INTO tools_schema_migrations(version) VALUES (1)",
    ] {
        sqlx::raw_sql(statement)
            .execute(&pool)
            .await
            .expect("rewind tools schema");
    }
    pool.close().await;

    // Opening a writer upgrades the database in place: the new columns exist,
    // the marker is 2, and a fresh approval can carry the new scope.
    let store = bench.open_store().await;
    let revisions = store.all_schema_revisions().await.expect("revisions");
    assert_eq!(revisions.get("tools").copied(), Some(2));
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let prepared = tools
        .prepare(
            ToolRequest::new(
                session.clone(),
                task.clone(),
                "actor.a",
                &bench.workspace,
                patch_action(&bench.workspace),
            )
            .with_call_id("call-upgrade"),
        )
        .await
        .expect("prepare after upgrade");
    let grant = tools
        .approve(&prepared)
        .await
        .expect("approve after upgrade");
    assert_eq!(grant.call_id(), Some("call-upgrade"));
    let view = tools
        .execute(prepared, Some(grant))
        .await
        .expect("execute after upgrade");
    assert_eq!(
        view.receipt.expect("receipt").call_id.as_deref(),
        Some("call-upgrade")
    );
    drop(tools);
    close(store).await;
}

// ---------------------------------------------------------------------------
// M4-02: structured git log, path and patch safety (A15)
// ---------------------------------------------------------------------------

#[tokio::test]
#[allow(clippy::too_many_lines)] // one tool's whole contract, told in order
async fn m4_02_git_log_is_structured_and_bounded() {
    let bench = bench();
    let store = bench.open_store().await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let (session, task) = admit(&store, &bench).await;

    // The registry advertises it as a bounded read before any call exists.
    let descriptor = coding_tool_descriptors()
        .into_iter()
        .find(|descriptor| descriptor.id == "git_log")
        .expect("git_log is advertised");
    assert_eq!(descriptor.effect_class, EffectClass::ReadOnly);
    assert_eq!(effect_class_for("git_log"), EffectClass::ReadOnly);
    assert!(descriptor.schema_digest.starts_with("sha256:"));

    // The parser accepts an omitted limit and refuses an out-of-range one
    // before a proposal exists.
    assert!(matches!(
        CodingToolAction::from_provider_call("git_log", "{}").expect("empty args parse"),
        CodingToolAction::GitLog {
            path: None,
            limit: None
        }
    ));
    assert!(matches!(
        CodingToolAction::from_provider_call("git_log", r#"{"limit":1}"#).expect("limit parses"),
        CodingToolAction::GitLog { limit: Some(1), .. }
    ));
    for arguments in [
        r#"{"limit":0}"#,
        r#"{"limit":101}"#,
        r#"{"limit":"2"}"#,
        r#"{"limit":1.5}"#,
        r#"{"unknown":1}"#,
    ] {
        assert!(
            CodingToolAction::from_provider_call("git_log", arguments).is_err(),
            "{arguments} must not reach the gate"
        );
    }

    // Real history, bounded to the requested count and machine-readable.
    git(
        &bench.workspace,
        &["commit", "--allow-empty", "-m", "second commit"],
    );
    let prepared = tools
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.log",
            &bench.workspace,
            CodingToolAction::GitLog {
                path: None,
                limit: Some(1),
            },
        ))
        .await
        .expect("git_log prepares");
    let grant = tools.approve(&prepared).await.expect("approved");
    let view = tools
        .execute(prepared, Some(grant))
        .await
        .expect("executes");
    assert_eq!(
        view.receipt.expect("receipt").outcome_state,
        ToolOutcomeState::Settled
    );
    let ToolOutput::Git {
        operation, output, ..
    } = &view.output
    else {
        panic!("git_log must return a git output: {:?}", view.output);
    };
    assert_eq!(operation, "log");
    let lines = output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "the limit bounds the history: {output}");
    let fields = lines[0].split('\t').collect::<Vec<_>>();
    assert_eq!(fields.len(), 3, "hash, author date and subject: {output}");
    assert!(fields[2].contains("second commit"), "{output}");
    assert!(fields[1].contains('T'), "author date is ISO 8601: {output}");

    // A path scope finds the baseline commit that touched it.
    let prepared = tools
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.log",
            &bench.workspace,
            CodingToolAction::GitLog {
                path: Some("src".to_owned()),
                limit: Some(5),
            },
        ))
        .await
        .expect("scoped git_log prepares");
    let grant = tools.approve(&prepared).await.expect("approved");
    let view = tools
        .execute(prepared, Some(grant))
        .await
        .expect("executes");
    let ToolOutput::Git { output, .. } = &view.output else {
        panic!("git_log must return a git output: {:?}", view.output);
    };
    assert!(output.contains("fixture baseline"), "{output}");

    // An escaping scope is refused before a proposal exists.
    let error = tools
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.log",
            &bench.workspace,
            CodingToolAction::GitLog {
                path: Some("../".to_owned()),
                limit: None,
            },
        ))
        .await
        .expect_err("escape is refused");
    assert_eq!(error.code(), ErrorCode::WorkspaceEscape);
    drop(tools);
    close(store).await;
}

#[cfg(windows)]
fn make_dir_link(target: &Path, link: &Path) {
    let status = Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .expect("mklink starts");
    assert!(
        status.status.success(),
        "junction creation failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
}

#[cfg(unix)]
fn make_dir_link(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).expect("symlink");
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one safety surface, case by case
async fn a15_path_patch_safety() {
    let bench = bench();
    let store = bench.open_store().await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let (session, task) = admit(&store, &bench).await;

    // Parent traversal and absolute paths never reach a proposal.
    //
    // A drive-letter path is absolute only on Windows: on POSIX `C:/Windows/win.ini`
    // is an ordinary relative name (a directory called `C:`), so asserting an escape
    // there would assert the wrong thing about the platform - it is the unix absolute
    // path that carries the case on the other side. Both branches are built with
    // `cfg!` so the list stays compiled, and therefore linted, everywhere.
    let mut escapes = vec!["../outside.txt", "src/../../outside.txt", "/etc/hosts"];
    if cfg!(windows) {
        escapes.push("C:/Windows/win.ini");
    }
    for path in escapes {
        let error = tools
            .prepare(ToolRequest::new(
                session.clone(),
                task.clone(),
                "actor.a",
                &bench.workspace,
                CodingToolAction::ReadFile {
                    path: path.to_owned(),
                },
            ))
            .await
            .expect_err("escape must be refused");
        assert_eq!(error.code(), ErrorCode::WorkspaceEscape, "{path}");
    }

    // A directory link inside the workspace is refused even though its target
    // exists: the link name itself is the escape.
    let outside = bench.temp.path().join("outside");
    std::fs::create_dir_all(&outside).expect("outside dir");
    std::fs::write(outside.join("secret.txt"), "outside secret").expect("secret");
    let link = bench.workspace.join("linked");
    make_dir_link(&outside, &link);
    let error = tools
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.a",
            &bench.workspace,
            CodingToolAction::ReadFile {
                path: "linked/secret.txt".to_owned(),
            },
        ))
        .await
        .expect_err("link traversal must be refused");
    assert_eq!(error.code(), ErrorCode::WorkspaceEscape);
    assert_eq!(
        std::fs::read_to_string(outside.join("secret.txt")).expect("outside readable"),
        "outside secret",
        "nothing read or wrote through the link"
    );

    // Credential-like names are denied as sensitive, not as an escape.
    let error = tools
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.a",
            &bench.workspace,
            CodingToolAction::ReadFile {
                path: ".env".to_owned(),
            },
        ))
        .await
        .expect_err("sensitive path must be refused");
    assert_eq!(error.code(), ErrorCode::SensitivePathDenied);

    // A file another process holds open is observed as present-but-unreadable,
    // never as a path escape. Windows is where the share violation is
    // observable; POSIX advisory locks never block a plain read.
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;

        let locked = bench.workspace.join("src").join("locked.txt");
        std::fs::write(&locked, "locked content\r\n").expect("lock fixture");
        let unlocked = observe_workspace(bench.project_id.clone(), &bench.workspace)
            .expect("the fixture workspace is observable");
        let handle = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&locked)
            .expect("exclusive lock fixture");
        let locked_observation = observe_workspace(bench.project_id.clone(), &bench.workspace)
            .expect("a locked file must not fail the whole observation");
        assert_ne!(
            unlocked.observed_fingerprint, locked_observation.observed_fingerprint,
            "the locked state is part of the fingerprint, so an approval cannot slip through"
        );

        // A patch of the locked file is refused, and the refusal is not called a
        // path escape.
        let prepared = tools
            .prepare(ToolRequest::new(
                session.clone(),
                task.clone(),
                "actor.a",
                &bench.workspace,
                CodingToolAction::ApplyPatch {
                    path: "src/locked.txt".to_owned(),
                    expected_hash: ContentHash::from_bytes(b"locked content\r\n"),
                    replacement: "replaced\r\n".to_owned(),
                },
            ))
            .await
            .expect("the proposal itself is a normal path");
        let grant = tools.approve(&prepared).await.expect("approved");
        let view = tools
            .execute(prepared, Some(grant))
            .await
            .expect("a locked file is a typed denial, not a crash");
        let receipt = view.receipt.expect("denial carries a receipt");
        assert_eq!(receipt.outcome_state, ToolOutcomeState::Denied);
        let ToolOutput::Denied { code, reason } = &view.output else {
            panic!("the lock refusal must be a denial: {:?}", view.output);
        };
        assert_ne!(
            code, "workspace_escape",
            "a lock is a read failure, not a path escape: {reason}"
        );
        assert!(
            store
                .pending_tool_intents(&session)
                .await
                .unwrap()
                .is_empty(),
            "a denied patch never creates an intent"
        );
        drop(handle);
        assert_eq!(
            std::fs::read_to_string(&locked).expect("lock target readable after release"),
            "locked content\r\n",
            "the refused patch did not write through the lock"
        );
        let readable_again = observe_workspace(bench.project_id.clone(), &bench.workspace)
            .expect("observable once readable");
        assert_eq!(
            unlocked.observed_fingerprint, readable_again.observed_fingerprint,
            "an unlocked file returns to the same fingerprint"
        );
    }

    // A stale patch is refused by hash before any intent claims a side effect.
    let prepared = tools
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.a",
            &bench.workspace,
            patch_action(&bench.workspace),
        ))
        .await
        .expect("patch prepares");
    let grant = tools.approve(&prepared).await.expect("approved");
    std::fs::write(
        bench.workspace.join("src").join("parser.txt"),
        "CLEAN parser\r\n",
    )
    .expect("external edit");
    let view = tools
        .execute(prepared, Some(grant))
        .await
        .expect("a stale patch is a typed denial, not a crash");
    let receipt = view.receipt.expect("denial carries a receipt");
    assert_eq!(receipt.outcome_state, ToolOutcomeState::Denied);
    assert!(
        matches!(&view.output, ToolOutput::Denied { code, .. } if code == "stale_workspace"),
        "the denial names the stale fingerprint: {:?}",
        view.output
    );
    assert_eq!(
        file_text(&bench.workspace),
        "CLEAN parser\r\n",
        "the refused patch did not touch the file"
    );
    assert!(
        store
            .pending_tool_intents(&session)
            .await
            .unwrap()
            .is_empty(),
        "a refused patch never creates an intent"
    );

    // CRLF and Unicode survive a patch byte for byte.
    std::fs::write(
        bench.workspace.join("src").join("parser.txt"),
        "BUG parser\r\n",
    )
    .expect("restore");
    let unicode = "café ✓ — 日本語\r\nsecond\r\n";
    std::fs::write(
        bench.workspace.join("src").join("unicode.txt"),
        "placeholder\r\n",
    )
    .expect("unicode fixture");
    let expected = observed_file_hash(&bench.workspace, "src/unicode.txt").expect("hash");
    let prepared = tools
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.a",
            &bench.workspace,
            CodingToolAction::ApplyPatch {
                path: "src/unicode.txt".to_owned(),
                expected_hash: expected,
                replacement: unicode.to_owned(),
            },
        ))
        .await
        .expect("unicode patch prepares");
    let grant = tools.approve(&prepared).await.expect("approved");
    let view = tools
        .execute(prepared, Some(grant))
        .await
        .expect("unicode patch executes");
    assert_eq!(
        view.receipt.expect("receipt").outcome_state,
        ToolOutcomeState::Settled
    );
    assert_eq!(
        std::fs::read_to_string(bench.workspace.join("src").join("unicode.txt"))
            .expect("unicode readable"),
        unicode,
        "bytes are preserved exactly, including CRLF and Unicode"
    );
    drop(tools);
    close(store).await;
}

// ---------------------------------------------------------------------------
// M4-03: process permit queue (A13 queued cancel)
// ---------------------------------------------------------------------------

/// A process that writes its marker and then holds the host permit until the test
/// creates `release`.
///
/// A13 needs a *first* call that owns the permit while a *second* one waits in the
/// queue, and it needs that to be true at the moment the test withdraws the second
/// call - not to be a bet on how fast it got there. A process that blocks until the
/// test says so makes "B is queued" a fact instead of a delay.
///
/// Both shells are built with `cfg!` rather than a `#[cfg]` pair so the *other*
/// platform's command is still compiled on the host running the gate. That is the
/// lesson from the failure this replaced: `millis as f64 / 1000.0` sat in a
/// `#[cfg(unix)]` helper, so `cast_precision_loss` (the `u64 -> f64` cast clippy
/// refuses under `-D warnings`, recorded once before as `f0a4ac8` in the P3
/// evidence) could not be seen by a Windows-only gate and only ever failed on
/// ubuntu.
fn write_then_wait(marker: &Path, release: &Path) -> String {
    if cfg!(windows) {
        format!(
            "Set-Content -LiteralPath '{}' -Value x; while (-not (Test-Path -LiteralPath '{}')) {{ Start-Sleep -Milliseconds 20 }}",
            marker.display(),
            release.display()
        )
    } else {
        format!(
            "echo x > '{}'; while [ ! -f '{}' ]; do sleep 0.02; done",
            marker.display(),
            release.display()
        )
    }
}

/// A process that only writes its marker: the queued call must never run at all.
fn write_now(marker: &Path) -> String {
    if cfg!(windows) {
        format!("Set-Content -LiteralPath '{}' -Value x", marker.display())
    } else {
        format!("echo x > '{}'", marker.display())
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one queue scenario, told in order
async fn a13_queued_process_cancel() {
    let _serial = process_queue_lock().lock().await;
    let bench = bench();
    let store = bench.open_store().await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let (session, task) = admit(&store, &bench).await;

    let marker_a = bench.workspace.join("a13-a.txt");
    let marker_b = bench.workspace.join("a13-b.txt");
    let release_a = bench.workspace.join("a13-release.txt");

    let prepared = tools
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.a13",
            &bench.workspace,
            CodingToolAction::RunShell {
                command: write_then_wait(&marker_a, &release_a),
                // Generous on purpose: A is held open until this test releases it,
                // so a slow host must not turn the harness into a timeout test.
                timeout_ms: 120_000,
                isolation: IsolationMode::BestEffort,
                env: Vec::new(),
            },
        ))
        .await
        .expect("A prepares");
    let approval = tools.approve(&prepared).await.expect("A approved");
    let a = {
        let tools = tools.clone();
        tokio::spawn(async move { tools.execute(prepared, Some(approval)).await })
    };

    // A takes the host permit and holds it until this test releases it, so B is
    // guaranteed to queue behind A rather than to race A to the permit.
    let a_deadline = Instant::now() + Duration::from_secs(20);
    while !marker_a.exists() {
        assert!(
            Instant::now() < a_deadline,
            "A never wrote its marker, so it never took the permit"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let prepared_b = tools
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.a13",
            &bench.workspace,
            CodingToolAction::RunShell {
                command: write_now(&marker_b),
                timeout_ms: 30_000,
                isolation: IsolationMode::BestEffort,
                env: Vec::new(),
            },
        ))
        .await
        .expect("B prepares");
    let approval_b = tools.approve(&prepared_b).await.expect("B approved");
    let token = CancellationToken::new();
    let b = {
        let tools = tools.clone();
        let token = token.clone();
        tokio::spawn(async move {
            tools
                .execute_with_cancellation(prepared_b, Some(approval_b), token)
                .await
        })
    };
    // B is inside the runner now, waiting for A's permit. A fixed sleep here was
    // the flake CI caught: under load the withdrawal could land before B's durable
    // intent, which is a *different* and equally truthful outcome (`Denied`,
    // "canceled before the durable intent"), so asserting queue semantics on it was
    // a race rather than a test. B's intent on the record is the observable that
    // says the call is past that point; A holds the permit until this test releases
    // it, so the queue is a fact and not a delay.
    let queued_deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let pending = store
            .pending_tool_intents(&session)
            .await
            .expect("pending intents readable");
        if pending.len() >= 2 {
            break;
        }
        assert!(
            Instant::now() < queued_deadline,
            "B never reached the durable intent, so there is no queued call to withdraw: {} pending",
            pending.len()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    token.cancel();

    let b_view = b.await.expect("B joins").expect("B settles truthfully");
    // A is still holding the permit; release it now that B's withdrawal is settled.
    std::fs::write(&release_a, b"go").expect("A is released");
    let a_view = a.await.expect("A joins").expect("A completes");

    assert!(
        !marker_b.exists(),
        "a call canceled while queued must never spawn its process"
    );
    assert!(
        b_view.execution_id.is_some(),
        "B crossed the durable intent, so it was inside the runner, not denied early"
    );
    assert_eq!(
        b_view.receipt.expect("B carries a receipt").outcome_state,
        ToolOutcomeState::Settled,
        "a withdrawal before any effect settles as a real, truthful result"
    );
    let ToolOutput::Process {
        canceled: b_canceled,
        queued: b_queued,
        exit_code: b_exit,
        tree_cleanup_confirmed: b_clean,
        ..
    } = &b_view.output
    else {
        panic!("B must produce a process output: {:?}", b_view.output);
    };
    assert!(b_canceled, "B was withdrawn");
    assert!(b_queued, "B really waited behind A before being withdrawn");
    assert!(
        b_exit.is_none(),
        "no exit code for a process that never ran"
    );
    assert!(b_clean, "no process exists, so the tree is clean");

    let ToolOutput::Process {
        exit_code: a_exit,
        queued: a_queued,
        canceled: a_canceled,
        ..
    } = &a_view.output
    else {
        panic!("A must produce a process output: {:?}", a_view.output);
    };
    assert_eq!(a_exit, &Some(0), "A ran to completion");
    assert!(!a_canceled);
    assert!(!a_queued, "A took the permit without waiting");
    assert!(marker_a.exists(), "A's effect landed");
    assert!(
        store
            .pending_tool_intents(&session)
            .await
            .unwrap()
            .is_empty(),
        "both calls settled, nothing is pending"
    );
    drop(tools);
    close(store).await;
}

// ---------------------------------------------------------------------------
// M4-01b: crash boundaries (A03 receipt-before-checkpoint, A04 effect-before-receipt)
// ---------------------------------------------------------------------------

fn fixture_host() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_m4_fixture_host"))
}

fn wait_for_file(path: &Path, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn kill_child(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

async fn only_session(store: &Arc<SqliteStore>) -> (SessionId, TaskId) {
    let summaries = store.list_sessions().await.expect("sessions");
    let summary = summaries
        .iter()
        .max_by_key(|summary| summary.next_sequence)
        .expect("one session exists");
    (summary.session_id.clone(), summary.task_id.clone())
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one crash scenario, told in order
async fn a03_receipt_before_checkpoint() {
    let bench = bench();
    let barrier = bench.temp.path().join("a03.barrier");
    let mut child = Command::new(fixture_host())
        .args([
            "--mode",
            "receipt-barrier",
            "--data-dir",
            bench.data_dir.to_str().unwrap(),
            "--workspace",
            bench.workspace.to_str().unwrap(),
            "--project-id",
            bench.project_id.as_str(),
            "--barrier",
            barrier.to_str().unwrap(),
        ])
        .spawn()
        .expect("fixture host starts");
    assert!(
        wait_for_file(&barrier, Duration::from_mins(1)),
        "the fixture must settle the patch and reach the barrier"
    );
    // Hard kill between the receipt commit and the next step: no cleanup runs.
    kill_child(&mut child);

    let store = bench.open_store().await;
    let (session, task) = only_session(&store).await;
    assert_eq!(
        file_text(&bench.workspace),
        "FIXED parser\r\n",
        "the patch happened exactly once"
    );
    let receipts = SessionService::new(Arc::clone(&store))
        .recover(&session)
        .await
        .expect("recovery")
        .receipts;
    assert_eq!(receipts.len(), 1, "the receipt survived the crash");
    assert_eq!(receipts[0].call_id.as_deref(), Some("call-1"));
    assert!(
        store
            .pending_tool_intents(&session)
            .await
            .unwrap()
            .is_empty(),
        "a settled intent is not pending work"
    );

    // The next turn must receive the committed tool result and must not rerun it.
    let provider = Arc::new(ScriptedProvider::new(vec![vec![
        ProviderStreamEvent::started(),
        ProviderStreamEvent::text("continued after the crash"),
        ProviderStreamEvent::completed("stop"),
    ]]));
    let runtime = Arc::new(RuntimeService::new(
        Arc::clone(&store),
        provider.clone(),
        RuntimeConfig::default(),
    ));
    let request = RunRequest::new(
        SessionId::generate(),
        task,
        InputId::generate(),
        "continue".to_owned(),
        observe_workspace(bench.project_id.clone(), &bench.workspace).unwrap(),
    )
    .with_tool_schemas(coding_tool_schemas());
    let driver = TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(&store)));
    let outcome = driver
        .run_turn_continuing(
            &session,
            request,
            TurnOptions {
                workspace_root: bench.workspace.clone(),
                actor_id: "m4.test".to_owned(),
                approvals: ApprovalMode::Auto,
                limits: TurnLimits::default(),
            },
            Arc::new(SilentObserver),
            CancellationToken::new(),
        )
        .await
        .expect("continuation runs");
    assert_eq!(outcome.stop, harness_tools::TurnStop::Final);

    let seen = provider.seen();
    let messages = &seen.first().expect("one provider call").messages;
    assert!(
        messages.iter().any(|message| {
            message.role == MessageRole::Assistant
                && message.tool_calls.len() == 1
                && message.tool_calls[0].call_id == "call-1"
        }),
        "the recovered assistant call is replayed: {messages:?}"
    );
    assert!(
        messages.iter().any(|message| {
            message.role == MessageRole::Tool
                && message.tool_call_id.as_deref() == Some("call-1")
                && message.content.contains("apply_patch")
        }),
        "the committed tool result is paired into the next step: {messages:?}"
    );
    // And the tool still did not execute twice.
    let receipts = SessionService::new(Arc::clone(&store))
        .recover(&session)
        .await
        .expect("recovery after continuation")
        .receipts;
    assert_eq!(receipts.len(), 1, "no second execution, no second receipt");
    assert_eq!(file_text(&bench.workspace), "FIXED parser\r\n");
    drop(driver);
    close(store).await;
}

#[tokio::test]
async fn a04_effect_before_receipt() {
    let bench = bench();
    let marker = bench.temp.path().join("a04.marker");
    let mut child = Command::new(fixture_host())
        .args([
            "--mode",
            "marker-process",
            "--data-dir",
            bench.data_dir.to_str().unwrap(),
            "--workspace",
            bench.workspace.to_str().unwrap(),
            "--project-id",
            bench.project_id.as_str(),
            "--marker",
            marker.to_str().unwrap(),
        ])
        .spawn()
        .expect("fixture host starts");
    assert!(
        wait_for_file(&marker, Duration::from_mins(1)),
        "the fixture process must write its marker"
    );
    // Hard kill while the side effect exists and the receipt does not.
    kill_child(&mut child);

    let store = bench.open_store().await;
    let (session, _) = only_session(&store).await;
    let recovered = SessionService::new(Arc::clone(&store))
        .recover(&session)
        .await
        .expect("recovery");
    assert!(
        recovered.receipts.is_empty(),
        "no receipt was committed before the crash"
    );
    let pending = store.pending_tool_intents(&session).await.unwrap();
    assert_eq!(pending.len(), 1, "the intent is the pending handoff");
    assert_eq!(pending[0].status, ToolIntentStatus::Recorded);
    let marker_lines = || std::fs::read_to_string(&marker).unwrap().lines().count();
    assert_eq!(marker_lines(), 1);

    // Explicit reconciliation settles the unknown outcome without a rerun.
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let view = tools
        .reconcile_pending(&session, &pending[0].tool_execution_id)
        .await
        .expect("reconcile");
    let receipt = view.receipt.clone().expect("reconcile writes a receipt");
    assert_eq!(receipt.outcome_state, ToolOutcomeState::OutcomeUnknown);
    assert!(matches!(
        view.output,
        harness_tools::ToolOutput::OutcomeUnknown { .. }
    ));
    let reconciled = store
        .tool_intent(&pending[0].tool_execution_id)
        .await
        .unwrap()
        .expect("intent still exists");
    assert_eq!(reconciled.status, ToolIntentStatus::OutcomeUnknown);
    assert_eq!(marker_lines(), 1, "reconciliation never reruns the process");
    assert!(
        store
            .pending_tool_intents(&session)
            .await
            .unwrap()
            .is_empty(),
        "the reconciled intent is no longer pending"
    );
    let after = SessionService::new(Arc::clone(&store))
        .recover(&session)
        .await
        .expect("recovery after reconcile");
    assert_eq!(
        after.receipts.len(),
        1,
        "reconcile writes exactly one receipt"
    );
    drop(tools);
    close(store).await;
}

// ---------------------------------------------------------------------------
// M4-03.2/.3: process environment and process-tree cleanup (A16)
// ---------------------------------------------------------------------------

/// The value a deployment's vault would hand back. It never exists in this
/// process's environment, so seeing it in the child proves the grant resolved
/// it, and seeing it anywhere in the evidence proves the redaction failed.
const A16_SENTINEL: &str = "m4-sentinel-3f9c1d7e-2a44";

/// A resolver for a host that keeps secrets somewhere other than its own
/// environment. It answers exactly one reference; anything else is refused, so
/// the test also proves the port cannot be talked into widening a grant.
struct FixtureSecrets;

impl SecretResolver for FixtureSecrets {
    fn resolve(&self, reference: &str) -> Result<String, HarnessError> {
        if reference == "secret://HA_M4_SENTINEL" {
            return Ok(A16_SENTINEL.to_owned());
        }
        Err(HarnessError::new(
            ErrorCode::SecretNotGranted,
            "the fixture vault holds no such reference",
        ))
    }
}

fn fixture_host_argument(flag: &str, value: &Path) -> Vec<String> {
    vec![flag.to_owned(), value.to_string_lossy().into_owned()]
}

fn tree_parent_action(
    args: Vec<String>,
    timeout_ms: u64,
    env: Vec<EnvBinding>,
) -> CodingToolAction {
    CodingToolAction::RunProcess {
        executable: fixture_host().to_string_lossy().into_owned(),
        args,
        timeout_ms,
        isolation: IsolationMode::BestEffort,
        env,
    }
}

/// The environment a fixture child actually received, from the file the child
/// itself wrote. Windows environment names are case-insensitive, so lookups are
/// folded on both sides.
fn child_environment(path: &Path) -> Vec<(String, String)> {
    let bytes = std::fs::read(path).expect("the fixture child wrote its environment");
    serde_json::from_slice(&bytes).expect("the environment dump is a name/value list")
}

fn child_environment_get<'a>(visible: &'a [(String, String)], name: &str) -> Option<&'a str> {
    visible
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one process contract, told in order
async fn a16_process_tree_env() {
    let _serial = process_queue_lock().lock().await;
    let bench = bench();
    let store = bench.open_store().await;
    let tools = ToolExecutionService::new(Arc::clone(&store));
    let (session, task) = admit(&store, &bench).await;

    // A literal environment value is refused by the parser, before any proposal
    // exists: a model may name a reference, never a value.
    let literal = CodingToolAction::from_provider_call(
        "run_shell",
        r#"{"command":"echo hi","timeout_ms":10,"env":{"HA_LITERAL":"plain-value"}}"#,
    )
    .expect_err("a literal environment value must not reach the gate");
    assert_eq!(literal.code(), ErrorCode::EnvironmentDenied);
    let malformed = CodingToolAction::from_provider_call(
        "run_shell",
        r#"{"command":"echo hi","timeout_ms":10,"env":{"HA_BAD":"secret://"}}"#,
    )
    .expect_err("an empty secret reference must not reach the gate");
    assert_eq!(malformed.code(), ErrorCode::EnvironmentDenied);

    // (1) The tree: a parent holds while its grandchild heartbeats, and the call
    // times out. The heartbeat must stop with the tree, not with the parent.
    let heartbeat = bench.temp.path().join("a16-heartbeat.txt");
    let env_out = bench.temp.path().join("a16-env.json");
    let mut args = vec!["--mode".to_owned(), "tree-parent".to_owned()];
    args.extend(fixture_host_argument("--heartbeat", &heartbeat));
    args.extend(fixture_host_argument("--env-out", &env_out));
    args.extend(["--hold-ms".to_owned(), "30000".to_owned()]);
    let prepared = tools
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.a16",
            &bench.workspace,
            tree_parent_action(args, 2_000, Vec::new()),
        ))
        .await
        .expect("tree-parent prepares");
    let grant = tools.approve(&prepared).await.expect("approved");
    let view = tools
        .execute(prepared, Some(grant))
        .await
        .expect("a timed-out tree settles with cleanup evidence");
    let ToolOutput::Process {
        timed_out,
        tree_cleanup,
        tree_cleanup_confirmed,
        stdout,
        ..
    } = &view.output
    else {
        panic!(
            "the fixture must return a process output: {:?}",
            view.output
        );
    };
    assert!(timed_out, "the parent holds past the timeout");
    assert!(
        tree_cleanup_confirmed,
        "the tree was confirmed empty before the result was settled"
    );
    assert_eq!(
        tree_cleanup, "killed_and_reaped",
        "killing the parent is not the claim: the whole tree was reaped"
    );
    let heartbeat_lines =
        || std::fs::read_to_string(&heartbeat).map_or(0, |text| text.lines().count());
    let stopped_at = heartbeat_lines();
    assert!(
        stopped_at >= 3,
        "the grandchild must have been running during the call: {stopped_at} heartbeat lines"
    );
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(
        heartbeat_lines(),
        stopped_at,
        "a grandchild that outlives its parent must not survive the process-tree cleanup"
    );
    assert!(stdout.is_empty(), "the fixture printed nothing: {stdout}");

    // The environment the child received holds nothing this host did not
    // allowlist: the host's own variables are not inherited wholesale.
    let visible = child_environment(&env_out);
    let allowed = visible
        .iter()
        .filter(|(name, _)| {
            PROCESS_ENVIRONMENT_ALLOWLIST
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(name))
        })
        .count();
    assert_eq!(
        allowed,
        visible.len(),
        "the child environment must be allowlisted, not inherited: {visible:?}"
    );
    assert!(
        child_environment_get(&visible, "PATH").is_some(),
        "a child that cannot resolve executables is useless: {visible:?}"
    );
    let host_only: Vec<String> = std::env::vars()
        .map(|(name, _)| name)
        .filter(|name| {
            !PROCESS_ENVIRONMENT_ALLOWLIST
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(name))
        })
        .collect();
    assert!(
        !host_only.is_empty(),
        "this host has no non-allowlisted variables, so the check above would be vacuous"
    );
    for name in &host_only {
        assert!(
            child_environment_get(&visible, name).is_none(),
            "host variable {name} must not reach a tool process"
        );
    }

    // (2) A granted reference is resolved just in time and never persisted: the
    // child sees it, the evidence does not.
    let vault = ToolExecutionService::new(Arc::clone(&store))
        .with_secrets(Arc::new(FixtureSecrets))
        .with_policy(
            ToolPolicy::default().with_granted_secrets(vec!["secret://HA_M4_SENTINEL".to_owned()]),
        );
    let granted_env_out = bench.temp.path().join("a16-granted-env.json");
    let mut args = vec!["--mode".to_owned(), "tree-parent".to_owned()];
    args.extend(fixture_host_argument("--env-out", &granted_env_out));
    args.extend(["--hold-ms".to_owned(), "0".to_owned()]);
    args.extend(["--echo-env".to_owned(), "HA_M4_GRANTED".to_owned()]);
    let prepared = vault
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.a16",
            &bench.workspace,
            tree_parent_action(
                args,
                30_000,
                vec![EnvBinding {
                    name: "HA_M4_GRANTED".to_owned(),
                    reference: "secret://HA_M4_SENTINEL".to_owned(),
                }],
            ),
        ))
        .await
        .expect("a granted reference prepares");
    let grant = vault.approve(&prepared).await.expect("approved");
    let view = vault
        .execute(prepared, Some(grant))
        .await
        .expect("the granted call executes");
    let ToolOutput::Process {
        stdout,
        stderr,
        tree_cleanup,
        tree_cleanup_confirmed,
        ..
    } = &view.output
    else {
        panic!(
            "the fixture must return a process output: {:?}",
            view.output
        );
    };
    assert_eq!(
        tree_cleanup, "reaped_on_exit",
        "the direct child exited without a backend whole-tree reap signal"
    );
    assert!(
        !*tree_cleanup_confirmed,
        "a direct-child wait must not claim that every descendant exited"
    );
    let granted = child_environment(&granted_env_out);
    assert_eq!(
        child_environment_get(&granted, "HA_M4_GRANTED"),
        Some(A16_SENTINEL),
        "the granted reference was resolved into the child environment"
    );
    assert!(
        stdout.contains("echo=[REDACTED]"),
        "the child echoed the granted value, so the redaction must be visible: {stdout}"
    );
    assert!(
        !stdout.contains(A16_SENTINEL) && !stderr.contains(A16_SENTINEL),
        "a granted value must never reach the model view"
    );
    let receipt = view.receipt.clone().expect("receipt");
    let artifact_id = receipt.artifact_id.clone().expect("output artifact");
    let artifact_bytes = std::fs::read(
        bench
            .data_dir
            .join("artifacts")
            .join(format!("{artifact_id}.bin")),
    )
    .expect("the published artifact is readable");
    assert!(
        !String::from_utf8_lossy(&artifact_bytes).contains(A16_SENTINEL),
        "a granted value must never reach durable tool evidence"
    );
    let intent = store
        .tool_intent(&receipt.tool_execution_id)
        .await
        .expect("read intent")
        .expect("intent exists");
    assert!(
        !intent.action_json.to_string().contains(A16_SENTINEL),
        "the durable intent keeps the reference, never the value: {}",
        intent.action_json
    );
    assert!(
        intent
            .action_json
            .to_string()
            .contains("secret://HA_M4_SENTINEL")
    );

    // (3) The same reference without a grant never resolves, never spawns, and
    // never becomes an intent.
    let ungranted_env_out = bench.temp.path().join("a16-ungranted-env.json");
    let mut args = vec!["--mode".to_owned(), "tree-parent".to_owned()];
    args.extend(fixture_host_argument("--env-out", &ungranted_env_out));
    args.extend(["--hold-ms".to_owned(), "0".to_owned()]);
    let error = tools
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.a16",
            &bench.workspace,
            tree_parent_action(
                args,
                30_000,
                vec![EnvBinding {
                    name: "HA_M4_GRANTED".to_owned(),
                    reference: "secret://HA_M4_SENTINEL".to_owned(),
                }],
            ),
        ))
        .await
        .expect_err("an un-granted reference must be refused before a proposal exists");
    assert_eq!(error.code(), ErrorCode::SecretNotGranted);
    assert!(
        !ungranted_env_out.exists(),
        "a refused reference never starts a process"
    );
    assert!(
        store
            .pending_tool_intents(&session)
            .await
            .unwrap()
            .is_empty(),
        "a refused reference never becomes a pending intent"
    );

    // (4) The host's own environment is a legitimate source when the operator
    // exposes one of its variables by reference.
    let path_reference = "secret://PATH";
    let path_tools = ToolExecutionService::new(Arc::clone(&store))
        .with_policy(ToolPolicy::default().with_granted_secrets(vec![path_reference.to_owned()]));
    let host_path = std::env::var("PATH").expect("this host has a PATH");
    let path_env_out = bench.temp.path().join("a16-path-env.json");
    let mut args = vec!["--mode".to_owned(), "tree-parent".to_owned()];
    args.extend(fixture_host_argument("--env-out", &path_env_out));
    args.extend(["--hold-ms".to_owned(), "0".to_owned()]);
    args.extend(["--echo-env".to_owned(), "HA_M4_HOST_PATH".to_owned()]);
    let prepared = path_tools
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.a16",
            &bench.workspace,
            tree_parent_action(
                args,
                30_000,
                vec![EnvBinding {
                    name: "HA_M4_HOST_PATH".to_owned(),
                    reference: path_reference.to_owned(),
                }],
            ),
        ))
        .await
        .expect("an exposed host variable prepares");
    let grant = path_tools.approve(&prepared).await.expect("approved");
    let view = path_tools
        .execute(prepared, Some(grant))
        .await
        .expect("the exposed host variable executes");
    assert_eq!(
        child_environment_get(&child_environment(&path_env_out), "HA_M4_HOST_PATH"),
        Some(host_path.as_str()),
        "the host resolver must read the value at spawn time"
    );
    // The value came from this host's own environment and the child echoed it,
    // so the redaction has to hold for it exactly as it did for the vault.
    let ToolOutput::Process { stdout, .. } = &view.output else {
        panic!(
            "the fixture must return a process output: {:?}",
            view.output
        );
    };
    assert!(
        stdout.contains("echo=[REDACTED]") && !stdout.contains(&host_path),
        "a granted host variable is redacted from the model view: {stdout}"
    );
    drop(tools);
    drop(vault);
    drop(path_tools);
    close(store).await;
}

#[allow(dead_code)]
fn _hash_marker(value: &str) -> ContentHash {
    ContentHash::from_bytes(value.as_bytes())
}

// ---------------------------------------------------------------------------
// M4-04: end-to-end coding in a real repository, with digest-bound criteria
// ---------------------------------------------------------------------------

/// The buggy parser the disposable repository starts with.
const A08_BUGGY: &str = r"/// Normalize a value read from the configuration file.
pub fn normalize(input: &str) -> String {
    // The padding the file happened to have is kept.
    input.trim_end().to_string()
}
";

/// A plausible fix that does not repair the bug: the first attempt must fail
/// its own test, or the run would prove nothing about failure handling.
const A08_WRONG: &str = r"/// Normalize a value read from the configuration file.
pub fn normalize(input: &str) -> String {
    input.to_string()
}
";

/// The fix that makes the suite pass.
const A08_FIXED: &str = r"/// Normalize a value read from the configuration file.
pub fn normalize(input: &str) -> String {
    input.trim().to_string()
}
";

const A08_TEST: &str = r#"#[test]
fn normalize_trims_both_ends() {
    assert_eq!(m4_fixture_parser::normalize("  value  "), "value");
}
"#;

/// A disposable Rust crate with a failing parser test, committed to its own Git
/// repository. `target/` is ignored so the workspace fingerprint describes the
/// sources rather than a build directory.
fn parser_repo(bench: &Bench) -> PathBuf {
    let repo = bench.temp.path().join("parser-repo");
    std::fs::create_dir_all(repo.join("src")).expect("source directory");
    std::fs::create_dir_all(repo.join("tests")).expect("test directory");
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"m4_fixture_parser\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n",
    )
    .expect("manifest");
    std::fs::write(repo.join(".gitignore"), "/target\n").expect("ignore file");
    std::fs::write(repo.join("src").join("lib.rs"), A08_BUGGY).expect("buggy parser");
    std::fs::write(repo.join("tests").join("parser.rs"), A08_TEST).expect("failing test");
    git(&repo, &["init"]);
    git(&repo, &["config", "user.email", "m4@example.invalid"]);
    git(&repo, &["config", "user.name", "M4 Fixture"]);
    git(&repo, &["add", "."]);
    git(
        &repo,
        &["commit", "-m", "fixture parser with a failing test"],
    );
    repo
}

fn cargo_test_call(call_id: &str) -> ProviderStreamEvent {
    ProviderStreamEvent::tool_delta(
        call_id,
        "run_process",
        serde_json::json!({
            "executable": "cargo",
            "args": ["test", "--offline", "--quiet"],
            "timeout_ms": 300_000
        })
        .to_string(),
    )
}

fn patch_call(
    call_id: &str,
    path: &str,
    expected: &ContentHash,
    replacement: &str,
) -> ProviderStreamEvent {
    ProviderStreamEvent::tool_delta(
        call_id,
        "apply_patch",
        serde_json::json!({
            "path": path,
            "expected_hash": expected.as_str(),
            "replacement": replacement
        })
        .to_string(),
    )
}

fn coding_goal() -> GoalSpec {
    GoalSpec::new(
        "make the parser test pass",
        vec![
            GoalCriterion::required("file-changed", EvidenceKind::FileChange),
            GoalCriterion::required("tests-pass", EvidenceKind::Check),
        ],
    )
}

fn repo_process_view(outcome: &TurnOutcome, index: usize) -> &harness_tools::ToolExecutionView {
    outcome
        .executions
        .iter()
        .filter(|view| matches!(view.output, ToolOutput::Process { .. }))
        .nth(index)
        .unwrap_or_else(|| panic!("the run must contain at least {} process calls", index + 1))
}

fn process_exit_code(view: &harness_tools::ToolExecutionView) -> Option<i32> {
    match &view.output {
        ToolOutput::Process { exit_code, .. } => *exit_code,
        other => panic!("expected a process output: {other:?}"),
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one end-to-end story, told in order
async fn a08_coding_e2e() {
    let _serial = process_queue_lock().lock().await;
    let bench = bench();
    let repo = parser_repo(&bench);
    let store = bench.open_store().await;
    let buggy_hash = observed_file_hash(&repo, "src/lib.rs").expect("buggy hash");
    let wrong_hash = ContentHash::from_bytes(A08_WRONG.as_bytes());

    // The model: read, patch wrongly, run the suite, read, patch correctly, run
    // the suite, answer. Every call is correlated by its provider call id.
    let provider = Arc::new(ScriptedProvider::new(vec![
        vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::tool_delta(
                "call-1",
                "read_file",
                serde_json::json!({"path": "src/lib.rs"}).to_string(),
            ),
            ProviderStreamEvent::completed("tool_calls"),
        ],
        vec![
            ProviderStreamEvent::started(),
            patch_call("call-2", "src/lib.rs", &buggy_hash, A08_WRONG),
            ProviderStreamEvent::completed("tool_calls"),
        ],
        vec![
            ProviderStreamEvent::started(),
            cargo_test_call("call-3"),
            ProviderStreamEvent::completed("tool_calls"),
        ],
        vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::tool_delta(
                "call-4",
                "read_file",
                serde_json::json!({"path": "src/lib.rs"}).to_string(),
            ),
            ProviderStreamEvent::completed("tool_calls"),
        ],
        vec![
            ProviderStreamEvent::started(),
            patch_call("call-5", "src/lib.rs", &wrong_hash, A08_FIXED),
            ProviderStreamEvent::completed("tool_calls"),
        ],
        vec![
            ProviderStreamEvent::started(),
            cargo_test_call("call-6"),
            ProviderStreamEvent::completed("tool_calls"),
        ],
        vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::text("the parser trims both ends now and the suite passes"),
            ProviderStreamEvent::completed("stop"),
        ],
    ]));
    let runtime = Arc::new(RuntimeService::new(
        Arc::clone(&store),
        provider.clone(),
        RuntimeConfig::default(),
    ));
    let session = SessionId::generate();
    let task = TaskId::generate();
    let driver = TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(&store)))
        .with_goal(coding_goal());
    let request = RunRequest::new(
        session.clone(),
        task.clone(),
        InputId::generate(),
        "repair the parser".to_owned(),
        observe_workspace(bench.project_id.clone(), &repo).expect("observation"),
    )
    .with_tool_schemas(coding_tool_schemas());
    let outcome = driver
        .run_turn(
            request,
            TurnOptions {
                workspace_root: repo.clone(),
                actor_id: "m4.a08".to_owned(),
                approvals: ApprovalMode::Auto,
                limits: TurnLimits::default(),
            },
            Arc::new(SilentObserver),
            CancellationToken::new(),
        )
        .await
        .expect("the coding turn runs");

    // The real filesystem decided the outcome: the fixed source is on disk.
    assert_eq!(
        std::fs::read_to_string(repo.join("src").join("lib.rs")).expect("source readable"),
        A08_FIXED
    );
    assert_eq!(
        outcome.tool_calls, 6,
        "every scripted call reached the gate"
    );
    let failed = repo_process_view(&outcome, 0);
    let passed = repo_process_view(&outcome, 1);
    assert_ne!(
        process_exit_code(failed),
        Some(0),
        "the first attempt must really fail its own test"
    );
    assert_eq!(
        process_exit_code(passed),
        Some(0),
        "the second attempt must really pass it"
    );
    assert_eq!(
        failed
            .receipt
            .as_ref()
            .map(|receipt| receipt.call_id.clone()),
        Some(Some("call-3".to_owned()))
    );
    assert_eq!(
        passed
            .receipt
            .as_ref()
            .map(|receipt| receipt.call_id.clone()),
        Some(Some("call-6".to_owned()))
    );
    // Failure and success are both durable evidence, not just a transcript.
    let receipts = SessionService::new(Arc::clone(&store))
        .recover(&session)
        .await
        .expect("recovery")
        .receipts;
    assert_eq!(receipts.len(), 6, "one receipt per executed call");
    // The real Git repository shows exactly the fix.
    let diff = Command::new("git")
        .args(["diff", "--", "src/lib.rs"])
        .current_dir(&repo)
        .output()
        .expect("git starts");
    let diff = String::from_utf8_lossy(&diff.stdout).into_owned();
    assert!(
        diff.contains("input.trim()"),
        "the fix is in the diff: {diff}"
    );
    assert!(
        !diff.contains("input.to_string()"),
        "the wrong attempt is gone: {diff}"
    );

    // Acceptance comes from the evidence: a change plus a check that ran at the
    // workspace digest the run ended on.
    assert_eq!(outcome.stop, TurnStop::Final);
    assert_eq!(outcome.acceptance, AcceptanceState::Satisfied);
    let goal = outcome.goal.clone().expect("the turn reported its goal");
    assert_eq!(goal.acceptance, AcceptanceState::Satisfied);
    assert!(
        goal.missing.is_empty(),
        "nothing is missing: {:?}",
        goal.missing
    );

    // The model was given each result paired with the call it answers. The
    // context window is bounded, so a result only has to reach a *later* step;
    // demanding all six in the final request would assert a window size.
    let seen = provider.seen();
    assert!(seen.len() >= 7, "one provider call per scripted step");
    for call_id in ["call-1", "call-2", "call-3", "call-4", "call-5", "call-6"] {
        assert!(
            seen.iter().skip(1).any(|request| {
                request.messages.iter().any(|message| {
                    message.role == MessageRole::Tool
                        && message.tool_call_id.as_deref() == Some(call_id)
                })
            }),
            "tool result for {call_id} must be paired into a later step"
        );
    }
    assert!(
        seen.iter().skip(1).any(|request| {
            request.messages.iter().any(|message| {
                message.role == MessageRole::Tool
                    && message.tool_call_id.as_deref() == Some("call-3")
                    && message.content.contains("exit=")
            })
        }),
        "the failing run's output reaches the model"
    );
    drop(driver);
    close(store).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one negative control, told in order
async fn m4_04_check_evidence_is_digest_bound() {
    let _serial = process_queue_lock().lock().await;
    let bench = bench();
    let repo = parser_repo(&bench);
    let store = bench.open_store().await;
    let buggy_hash = observed_file_hash(&repo, "src/lib.rs").expect("buggy hash");
    let test_hash = observed_file_hash(&repo, "tests/parser.rs").expect("test hash");
    let amended_test = format!("{A08_TEST}\n// touched after the suite ran\n");

    // The model fixes the bug, proves it with a real run, and then edits the
    // test file. The passing run observed the workspace *before* that edit.
    let provider = Arc::new(ScriptedProvider::new(vec![
        vec![
            ProviderStreamEvent::started(),
            patch_call("call-1", "src/lib.rs", &buggy_hash, A08_FIXED),
            ProviderStreamEvent::completed("tool_calls"),
        ],
        vec![
            ProviderStreamEvent::started(),
            cargo_test_call("call-2"),
            ProviderStreamEvent::completed("tool_calls"),
        ],
        vec![
            ProviderStreamEvent::started(),
            patch_call("call-3", "tests/parser.rs", &test_hash, &amended_test),
            ProviderStreamEvent::completed("tool_calls"),
        ],
        vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::text("done"),
            ProviderStreamEvent::completed("stop"),
        ],
    ]));
    let runtime = Arc::new(RuntimeService::new(
        Arc::clone(&store),
        provider.clone(),
        RuntimeConfig::default(),
    ));
    let driver = TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(&store))).with_goal(
        GoalSpec::new(
            "prove the suite passes at the revision you leave behind",
            vec![GoalCriterion::required("tests-pass", EvidenceKind::Check)],
        ),
    );
    let request = RunRequest::new(
        SessionId::generate(),
        TaskId::generate(),
        InputId::generate(),
        "fix and prove it".to_owned(),
        observe_workspace(bench.project_id.clone(), &repo).expect("observation"),
    )
    .with_tool_schemas(coding_tool_schemas());
    let outcome = driver
        .run_turn(
            request,
            TurnOptions {
                workspace_root: repo.clone(),
                actor_id: "m4.a08".to_owned(),
                approvals: ApprovalMode::Auto,
                limits: TurnLimits::default(),
            },
            Arc::new(SilentObserver),
            CancellationToken::new(),
        )
        .await
        .expect("the turn runs to a bound");

    // The check really ran and really passed - at an earlier digest.
    let checked = repo_process_view(&outcome, 0);
    assert_eq!(process_exit_code(checked), Some(0));
    let checked_digest = checked
        .receipt
        .as_ref()
        .and_then(|receipt| receipt.after_fingerprint.clone())
        .expect("a settled check records the workspace it observed");
    let last_digest = outcome
        .executions
        .last()
        .and_then(|view| view.receipt.as_ref())
        .and_then(|receipt| receipt.after_fingerprint.clone())
        .expect("the last execution records its workspace");
    assert_ne!(
        checked_digest, last_digest,
        "the test file was edited after the suite ran, so the digests must differ"
    );

    // A passing check from an earlier revision is not evidence for this one.
    assert_ne!(outcome.acceptance, AcceptanceState::Satisfied);
    assert_eq!(outcome.acceptance, AcceptanceState::NeedsWork);
    let goal = outcome.goal.clone().expect("the turn reported its goal");
    assert_eq!(goal.missing, vec!["tests-pass".to_owned()]);
    assert!(
        matches!(outcome.stop, TurnStop::NoProgress | TurnStop::GoalLimit),
        "the run stops instead of accepting stale evidence: {:?}",
        outcome.stop
    );
    drop(driver);
    close(store).await;
}

// ---------------------------------------------------------------------------
// M4-03.4: bounded capture, previews and paged reads (A17)
// ---------------------------------------------------------------------------

const A17_HEAD: &str = "A17-HEAD-SENTINEL";
const A17_TAIL: &str = "A17-TAIL-SENTINEL";
const A17_ERR: &str = "A17-ERR-SENTINEL";

/// A process that writes a head sentinel, a large middle, a tail sentinel on
/// stdout, and one sentinel on stderr. Both shells are built with `cfg!` so the
/// other platform's command still has to compile under this host's clippy.
fn noisy_script(middle_bytes: usize) -> String {
    if cfg!(windows) {
        format!(
            "[Console]::Out.Write('{A17_HEAD}'); [Console]::Out.Write('m' * {middle_bytes}); [Console]::Out.Write('{A17_TAIL}'); [Console]::Error.Write('{A17_ERR}')"
        )
    } else {
        format!(
            "printf '{A17_HEAD}'; head -c {middle_bytes} /dev/zero | tr '\\0' 'm'; printf '{A17_TAIL}'; printf '{A17_ERR}' >&2"
        )
    }
}

fn capture_bytes(bench: &Bench, artifact_id: &str) -> Vec<u8> {
    std::fs::read(
        bench
            .data_dir
            .join("artifacts")
            .join(format!("{artifact_id}.bin")),
    )
    .expect("the published capture is readable")
}

fn shell_action(command: String, timeout_ms: u64) -> CodingToolAction {
    CodingToolAction::RunShell {
        command,
        timeout_ms,
        isolation: IsolationMode::BestEffort,
        env: Vec::new(),
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one capture contract, told in order
async fn a17_output_quota() {
    let _serial = process_queue_lock().lock().await;
    let bench = bench();
    let store = bench.open_store().await;
    let (session, task) = admit(&store, &bench).await;
    let quota = 64 * 1024_u64;
    let limits = SpoolLimits {
        max_capture_bytes: quota,
        head_preview_bytes: 1024,
        tail_preview_bytes: 256,
    };
    let spool_root = bench.temp.path().join("a17-spool");
    let tools = ToolExecutionService::new(Arc::clone(&store))
        .with_spool(ProcessSpoolConfig::new(&spool_root, limits));

    // (1) An output larger than the quota is cut exactly at the quota, and the
    // result says so instead of pretending the whole log was kept.
    let prepared = tools
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.a17",
            &bench.workspace,
            shell_action(noisy_script(400_000), 30_000),
        ))
        .await
        .expect("the noisy call prepares");
    let grant = tools.approve(&prepared).await.expect("approved");
    let view = tools
        .execute(prepared, Some(grant))
        .await
        .expect("the noisy call executes");
    let receipt = view.receipt.clone().expect("receipt");
    assert_eq!(receipt.outcome_state, ToolOutcomeState::Settled);
    let ToolOutput::Process {
        stdout,
        stdout_truncated,
        artifact_id,
        captured_bytes,
        capture_hash,
        capture_truncated,
        capture_tail,
        ..
    } = &view.output
    else {
        panic!(
            "the fixture must return a process output: {:?}",
            view.output
        );
    };
    assert!(stdout_truncated, "the head preview is bounded");
    assert!(capture_truncated, "the capture stopped at the quota");
    assert!(
        stdout.starts_with(A17_HEAD),
        "the head preview starts at the beginning of the stream: {stdout}"
    );
    assert!(
        stdout.len() <= limits.head_preview_bytes,
        "memory holds a bounded preview, not the log: {} bytes",
        stdout.len()
    );
    assert!(
        !capture_tail.contains(A17_TAIL),
        "the tail was past the quota, so it was never captured: {capture_tail}"
    );
    let capture_id = artifact_id.clone().expect("the capture is referenced");
    let bytes = capture_bytes(&bench, &capture_id);
    let header = parse_capture_header(&bytes).expect("the artifact carries a capture header");
    assert_eq!(
        header.stdout_bytes, quota,
        "the stdout section is exactly the quota"
    );
    assert!(header.stdout_truncated);
    assert_eq!(header.quota_bytes, quota);
    assert_eq!(
        *captured_bytes,
        u64::try_from(bytes.len()).unwrap(),
        "the reported capture length is the length of the stored bytes"
    );
    assert_eq!(
        capture_hash.as_ref(),
        Some(&ContentHash::from_bytes(&bytes)),
        "the recorded digest is the digest of the stored bytes"
    );
    assert_eq!(
        capture_tail.as_str(),
        String::from_utf8_lossy(&bytes[bytes.len() - limits.tail_preview_bytes..]),
        "the tail preview is the tail of the stored bytes"
    );
    assert_eq!(
        receipt
            .artifact_id
            .as_ref()
            .map(harness_types::ArtifactId::as_str),
        Some(capture_id.as_str()),
        "the receipt points at the capture, not at a re-serialized view"
    );
    assert_eq!(
        std::fs::read_dir(&spool_root)
            .expect("the spool directory exists")
            .count(),
        0,
        "spool staging is removed once the capture is published"
    );

    // (2) An output inside the quota keeps its tail, and a page reads it back
    // through the same gate as every other tool call.
    let prepared = tools
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.a17",
            &bench.workspace,
            shell_action(noisy_script(2_000), 30_000),
        ))
        .await
        .expect("the small call prepares");
    let grant = tools.approve(&prepared).await.expect("approved");
    let view = tools
        .execute(prepared, Some(grant))
        .await
        .expect("the small call executes");
    let ToolOutput::Process {
        artifact_id,
        capture_truncated,
        capture_tail,
        ..
    } = &view.output
    else {
        panic!(
            "the fixture must return a process output: {:?}",
            view.output
        );
    };
    assert!(!capture_truncated, "a small output is captured whole");
    assert!(
        capture_tail.contains(A17_TAIL) && capture_tail.contains(A17_ERR),
        "the tail preview shows how the capture ended: {capture_tail}"
    );
    let small_id = artifact_id.clone().expect("the capture is referenced");
    let small_bytes = capture_bytes(&bench, &small_id);
    let small_header =
        parse_capture_header(&small_bytes).expect("the artifact carries a capture header");
    assert!(!small_header.stdout_truncated && !small_header.stderr_truncated);
    let tail_offset = small_header.stdout_bytes - u64::try_from(A17_TAIL.len()).unwrap();
    let prepared = tools
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.a17",
            &bench.workspace,
            CodingToolAction::ReadProcessOutput {
                artifact_id: small_id.clone(),
                stream: CaptureStream::Stdout,
                offset: tail_offset,
                length: 64,
            },
        ))
        .await
        .expect("a page of this task's own capture prepares");
    let grant = tools.approve(&prepared).await.expect("approved");
    let view = tools
        .execute(prepared, Some(grant))
        .await
        .expect("the page read executes");
    let ToolOutput::ProcessOutput {
        stream,
        offset,
        length,
        total_bytes,
        text,
        ..
    } = &view.output
    else {
        panic!("a page read must return a page: {:?}", view.output);
    };
    assert_eq!(stream, "stdout");
    assert_eq!(*offset, tail_offset);
    assert_eq!(*total_bytes, small_header.stdout_bytes);
    assert_eq!(
        *length,
        u64::try_from(A17_TAIL.len()).unwrap(),
        "a page is bounded by the remaining bytes"
    );
    assert_eq!(
        text, A17_TAIL,
        "the stored tail reads back exactly, with no missing bytes"
    );

    // A page past the captured bytes names the captured length instead of
    // returning an empty page that looks like the end of the log.
    let prepared = tools
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.a17",
            &bench.workspace,
            CodingToolAction::ReadProcessOutput {
                artifact_id: small_id.clone(),
                stream: CaptureStream::Stdout,
                offset: small_header.stdout_bytes + 1,
                length: 64,
            },
        ))
        .await
        .expect("the proposal itself is a well-formed read");
    let grant = tools.approve(&prepared).await.expect("approved");
    let view = tools
        .execute(prepared, Some(grant))
        .await
        .expect("a page past the end is a typed refusal");
    let receipt = view.receipt.clone().expect("refusal carries a receipt");
    assert_eq!(receipt.outcome_state, ToolOutcomeState::Denied);
    let ToolOutput::Denied { code, reason } = &view.output else {
        panic!("the refusal must be a denial: {:?}", view.output);
    };
    assert_eq!(code, "invalid_payload");
    assert!(
        reason.contains(&small_header.stdout_bytes.to_string()),
        "the refusal names the captured length: {reason}"
    );
    assert!(
        store
            .pending_tool_intents(&session)
            .await
            .unwrap()
            .is_empty(),
        "a refused page never creates an intent"
    );

    // Another task cannot page this task's capture, even knowing its id.
    let (other_session, other_task) = admit(&store, &bench).await;
    let error = tools
        .prepare(ToolRequest::new(
            other_session,
            other_task,
            "actor.a17",
            &bench.workspace,
            CodingToolAction::ReadProcessOutput {
                artifact_id: small_id.clone(),
                stream: CaptureStream::Stdout,
                offset: 0,
                length: 64,
            },
        ))
        .await
        .expect_err("an artifact id is not a capability");
    assert_eq!(error.code(), ErrorCode::ScopeAuthorityDenied);

    // (3) A capture that cannot be written is reported honestly: the process
    // really ran, so the outcome is unknown and nothing points at bytes that do
    // not exist.
    let blocked = bench.temp.path().join("a17-blocked");
    std::fs::write(&blocked, b"not a directory").expect("blocking fixture");
    let broken = ToolExecutionService::new(Arc::clone(&store))
        .with_spool(ProcessSpoolConfig::new(blocked.join("spool"), limits));
    let marker = bench.workspace.join("a17-marker.txt");
    let prepared = broken
        .prepare(ToolRequest::new(
            session.clone(),
            task.clone(),
            "actor.a17",
            &bench.workspace,
            shell_action(write_now(&marker), 30_000),
        ))
        .await
        .expect("the call prepares before its capture is attempted");
    let grant = broken.approve(&prepared).await.expect("approved");
    let view = broken
        .execute(prepared, Some(grant))
        .await
        .expect("a failed capture settles truthfully");
    let receipt = view.receipt.clone().expect("receipt");
    assert_eq!(
        receipt.outcome_state,
        ToolOutcomeState::OutcomeUnknown,
        "a capture that could not be written is not a settled success"
    );
    assert!(
        matches!(view.output, ToolOutput::OutcomeUnknown { .. }),
        "the model is told the outcome is unknown: {:?}",
        view.output
    );
    assert!(
        marker.exists(),
        "the process really ran, so 'no effect' would be a lie"
    );
    if let Some(artifact_id) = &receipt.artifact_id {
        let bytes = capture_bytes(&bench, artifact_id.as_str());
        assert!(
            parse_capture_header(&bytes).is_err(),
            "an unknown outcome must not reference a capture that was never written"
        );
    }
    assert!(
        store
            .pending_tool_intents(&session)
            .await
            .unwrap()
            .is_empty(),
        "the unknown outcome was settled, not left pending"
    );
    drop(tools);
    drop(broken);
    close(store).await;
}
