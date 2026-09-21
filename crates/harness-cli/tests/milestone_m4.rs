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
use std::time::Duration;

use harness_providers::{
    CancellationToken, MessageRole, ModelCapabilities, ModelProvider, ProviderFuture,
    ProviderRequest, ProviderStreamEvent,
};
use harness_runtime::{RunRequest, RuntimeConfig, RuntimeService};
use harness_session::{AdmitInputRequest, SessionService};
use harness_store_sqlite::{SqliteStore, ToolIntentStatus, WriterOpenOptions};
use harness_tools::{
    ApprovalMode, CodingToolAction, EffectClass, ToolExecutionService, ToolRequest, TurnDriver,
    TurnLimits, TurnObserver, TurnOptions, TurnProgress, coding_tool_descriptors,
    coding_tool_names, coding_tool_schemas, effect_class_for, observe_workspace,
    observed_file_hash,
};
use harness_types::{
    ContentHash, ErrorCode, HostId, InputId, ProjectId, SessionId, SourceAuthority, TaskId,
    ToolIntentState, ToolOutcomeState,
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

#[allow(dead_code)]
fn _hash_marker(value: &str) -> ContentHash {
    ContentHash::from_bytes(value.as_bytes())
}
