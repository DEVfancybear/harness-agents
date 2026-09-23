//! M3 acceptance: durable driver, human input and bounded goals.
//!
//! Everything here is real except the model boundary: a `SQLite` store with
//! writer fencing, the P2 runtime, the P3 tool gate with intents and receipts,
//! and a scripted provider/evaluator. A01/A02/A05/A06/A07 predecessors keep
//! their own targets; this file proves the M3 work items and A09/A10/A11.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use harness_providers::{
    CancellationToken, MessageRole, ModelCapabilities, ModelProvider, ProviderError,
    ProviderFuture, ProviderRequest, ProviderStreamEvent,
};
use harness_runtime::{
    AcceptanceState, AskRequest, BudgetLedger, BudgetView, EvidenceKind, GoalCriterion,
    GoalEvaluation, GoalEvaluationInput, GoalEvaluator, GoalSpec, GoalVerdict, HumanInputService,
    RunInbox, RunRequest, RuntimeConfig, RuntimeError, RuntimeService, Usage,
};
use harness_session::{AdmitInputRequest, SessionService};
use harness_store_sqlite::{
    BudgetReservationState, QuestionOutcome, QuestionState, RunState, SqliteStore, StoreFaultPlan,
    StoreFaultPoint, WriterOpenOptions,
};
use harness_tools::{
    ApprovalMode, ToolExecutionService, TurnDriver, TurnLimits, TurnObserver, TurnOptions,
    TurnProgress, TurnStop, coding_tool_schemas, observe_workspace,
};
use harness_types::{
    AgentRunId, BudgetId, ContentHash, ErrorCode, HostId, InputId, ProjectId, SessionId, TaskId,
};
use serde_json::json;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// One scripted provider call: events, or a typed failure.
#[derive(Clone)]
enum ScriptStep {
    Events(Vec<ProviderStreamEvent>),
    Fail(ErrorCode),
}

/// Deterministic provider: one scripted step per call, recording every request.
struct ScriptedProvider {
    steps: Vec<ScriptStep>,
    calls: AtomicUsize,
    seen: Mutex<Vec<ProviderRequest>>,
}

impl ScriptedProvider {
    fn new(steps: Vec<ScriptStep>) -> Self {
        assert!(!steps.is_empty(), "a script needs at least one step");
        Self {
            steps,
            calls: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn text(text: &str) -> Self {
        Self::new(vec![ScriptStep::Events(vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::text(text),
            ProviderStreamEvent::completed("stop"),
        ])])
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
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
            .min(self.steps.len() - 1);
        let step = self.steps[index].clone();
        Box::pin(async move {
            match step {
                ScriptStep::Fail(code) => {
                    Err(ProviderError::new(code, "scripted provider failure"))
                }
                ScriptStep::Events(mut events) => {
                    if let Some(ProviderStreamEvent::Started { request_id }) = events.first_mut() {
                        *request_id = request.request_id;
                    }
                    Ok(events)
                }
            }
        })
    }
}

/// A scripted evaluator: every call returns the same typed verdict.
struct FixedEvaluator {
    verdict: GoalVerdict,
}

impl GoalEvaluator for FixedEvaluator {
    fn evaluate(&self, input: &GoalEvaluationInput<'_>) -> Result<GoalEvaluation, RuntimeError> {
        Ok(GoalEvaluation {
            verdict: self.verdict.clone(),
            progress_signature: input.evidence.progress_signature(),
        })
    }
}

/// An evaluator that fails: a malformed evaluator must never satisfy a goal.
struct BrokenEvaluator;

impl GoalEvaluator for BrokenEvaluator {
    fn evaluate(&self, _input: &GoalEvaluationInput<'_>) -> Result<GoalEvaluation, RuntimeError> {
        Err(RuntimeError::new(
            ErrorCode::ServiceUnavailable,
            "scripted evaluator failure",
        ))
    }
}

#[derive(Default)]
struct RecordingObserver {
    progress: Mutex<Vec<TurnProgress>>,
}

impl TurnObserver for RecordingObserver {
    fn observe(&self, progress: TurnProgress) {
        self.progress.lock().expect("progress log").push(progress);
    }
}

struct Bench {
    _temp: tempfile::TempDir,
    home: PathBuf,
    workspace: PathBuf,
    data_dir: PathBuf,
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

    async fn open_store_with_fault(&self, point: StoreFaultPoint) -> Arc<SqliteStore> {
        Arc::new(
            SqliteStore::open_writer(
                WriterOpenOptions::new(self.data_dir.clone(), HostId::generate())
                    .with_fault_plan(StoreFaultPlan::with_point(point)),
            )
            .await
            .expect("store opens"),
        )
    }
}

fn bench() -> Bench {
    let temp = tempfile::tempdir().expect("temp root");
    let home = temp.path().join("home");
    let workspace = temp.path().join("repo");
    std::fs::create_dir_all(workspace.join("src")).expect("workspace");
    std::fs::write(
        workspace.join("src").join("parser.rs"),
        "fn parse() { todo!() }\n",
    )
    .expect("fixture file");
    let data_dir = temp.path().join("data");
    Bench {
        _temp: temp,
        home,
        workspace,
        data_dir,
    }
}

fn options(workspace: &std::path::Path) -> TurnOptions {
    TurnOptions {
        workspace_root: workspace.to_path_buf(),
        actor_id: "m3.test".to_owned(),
        approvals: ApprovalMode::Auto,
        limits: TurnLimits {
            max_steps: 8,
            max_tool_calls: 16,
            deadline: Duration::from_mins(1),
        },
    }
}

fn request(
    workspace: &std::path::Path,
    session: SessionId,
    task: TaskId,
    text: &str,
) -> RunRequest {
    RunRequest::new(
        session,
        task,
        InputId::generate(),
        text.to_owned(),
        observe_workspace(ProjectId::generate(), workspace).expect("workspace observation"),
    )
    .with_tool_schemas(coding_tool_schemas())
}

/// Build a runtime with the same store, optionally budgeted and evaluated.
async fn runtime_for(
    store: &Arc<SqliteStore>,
    provider: Arc<dyn ModelProvider>,
    evaluator: Option<Arc<dyn GoalEvaluator>>,
    budget_limit: Option<u64>,
) -> (Arc<RuntimeService>, Option<BudgetId>) {
    let mut runtime = RuntimeService::new(Arc::clone(store), provider, RuntimeConfig::default());
    if let Some(evaluator) = evaluator {
        runtime = runtime.with_evaluator(evaluator);
    }
    let mut budget_id = None;
    if let Some(limit) = budget_limit {
        let id = BudgetId::generate();
        let ledger = BudgetLedger::new(Arc::clone(store));
        ledger
            .ensure_account(&id, None, limit)
            .await
            .expect("budget account");
        runtime = runtime.with_budget(ledger, id.clone());
        budget_id = Some(id);
    }
    (Arc::new(runtime), budget_id)
}

async fn close(store: Arc<SqliteStore>) {
    Arc::try_unwrap(store)
        .expect("store consumers released")
        .close()
        .await
        .expect("store closes");
}

fn cli_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ha"))
}

fn run_cli(arguments: &[&str], home: &std::path::Path) -> std::process::Output {
    Command::new(cli_binary())
        .args(arguments)
        .env("HA_HOME", home)
        .output()
        .expect("cli runs")
}

// ---------------------------------------------------------------------------
// M3-01: durable driver and minimum context
// ---------------------------------------------------------------------------

#[tokio::test]
async fn m3_01_one_input_distinct_steps() {
    let bench = bench();
    let store = bench.open_store().await;
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptStep::Fail(ErrorCode::ServiceUnavailable),
        ScriptStep::Events(vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::tool_delta(
                "call-1",
                "search_text",
                json!({"query": "todo", "path": "."}).to_string(),
            ),
            ProviderStreamEvent::completed("tool_calls"),
        ]),
        ScriptStep::Events(vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::text("fixture finished"),
            ProviderStreamEvent::completed("stop"),
        ]),
    ]));
    let (runtime, _) = runtime_for(&store, provider.clone(), None, None).await;
    let session = SessionId::generate();
    let task = TaskId::generate();
    let run_request = request(&bench.workspace, session.clone(), task.clone(), "fix it");
    let input_id = run_request.input_id.clone();
    let driver = TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(&store)));

    let outcome = driver
        .run_turn(
            run_request,
            options(&bench.workspace),
            Arc::new(RecordingObserver::default()),
            CancellationToken::new(),
        )
        .await
        .expect("turn runs");
    assert_eq!(outcome.stop, TurnStop::Final);
    assert_eq!(outcome.input_id, input_id);

    // One admitted input: the retry and the tool round did not admit another.
    let summary = store.session_summary(&session).await.unwrap().unwrap();
    assert_eq!(summary.input_count, 1, "one input for the whole run");

    // One durable run, with one step per model call and distinct step IDs.
    let run = store
        .run_by_input(&input_id)
        .await
        .unwrap()
        .expect("run is durable");
    assert_eq!(run.run_id, outcome.run_id);
    assert_eq!(run.state, RunState::Completed);
    assert_eq!(run.acceptance.as_deref(), Some("not_evaluated"));
    let steps = store.run_steps(&run.run_id).await.unwrap();
    assert_eq!(steps.len(), 2, "two model calls, two frozen steps");
    assert_ne!(steps[0].step_id, steps[1].step_id, "distinct StepIds");
    assert_eq!(steps[0].step_index, 0);
    assert_eq!(steps[1].step_index, 1);
    assert_ne!(steps[0].request_id, steps[1].request_id);
    for step in &steps {
        assert_ne!(step.manifest_hash.as_str(), "", "frozen manifest hash");
    }

    // The retry reused the frozen request: one request, two attempts, same hash.
    let frozen = store.list_frozen_requests(&session).await.unwrap();
    let attempts = store.list_provider_attempts(&session).await.unwrap();
    assert_eq!(frozen.len(), 2, "one frozen request per step");
    assert_eq!(
        attempts.len(),
        3,
        "one failure plus two successful attempts"
    );
    let first_step_attempts: Vec<_> = attempts
        .iter()
        .filter(|attempt| attempt.request_id == steps[0].request_id)
        .collect();
    assert_eq!(first_step_attempts.len(), 2, "the retry is an attempt");
    let frozen_first = frozen
        .iter()
        .find(|record| record.request_id == steps[0].request_id)
        .expect("first step frozen request");
    assert_eq!(
        frozen_first.content_hash,
        ContentHash::from_canonical_json(&frozen_first.request_json).expect("canonical hash")
    );

    drop(driver);
    close(store).await;
}

#[tokio::test]
async fn m3_01_incomplete_stream_is_not_dispatched() {
    let bench = bench();
    let store = bench.open_store().await;
    // A06 at the loop level: the stream never reached a terminal marker and
    // its one call has unparseable arguments. The response is reported, but
    // nothing may execute and the turn is not a completed answer.
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptStep::Events(vec![
        ProviderStreamEvent::started(),
        ProviderStreamEvent::tool_delta("call-1", "search_text", "{\"query\":".to_owned()),
    ])]));
    let (runtime, _) = runtime_for(&store, provider.clone(), None, None).await;
    let driver = TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(&store)))
        .with_goal(goal_with(EvidenceKind::ToolExecution, 2, 1));
    let outcome = driver
        .run_turn(
            request(
                &bench.workspace,
                SessionId::generate(),
                TaskId::generate(),
                "fix it",
            ),
            options(&bench.workspace),
            Arc::new(RecordingObserver::default()),
            CancellationToken::new(),
        )
        .await
        .expect("turn runs");
    assert_eq!(outcome.stop, TurnStop::Unverified);
    assert_eq!(outcome.acceptance, AcceptanceState::Unverified);
    assert!(outcome.executions.is_empty(), "nothing may execute");
    assert_eq!(
        provider.calls(),
        1,
        "a truncated stream is not retried as a turn"
    );
    let run = store.run_by_id(&outcome.run_id).await.unwrap().unwrap();
    assert_eq!(run.state, RunState::Failed);
    assert_eq!(run.stop_reason.as_deref(), Some("unverified"));
    drop(driver);
    close(store).await;
}
#[tokio::test]
async fn m3_01_store_fault_stops_dispatch() {
    let bench = bench();
    let store = bench
        .open_store_with_fault(StoreFaultPoint::BeforeFreezeStepCommit)
        .await;
    let provider = Arc::new(ScriptedProvider::text("must not be called"));
    let (runtime, _) = runtime_for(&store, provider.clone(), None, None).await;
    let session = SessionId::generate();
    let task = TaskId::generate();
    let run_request = request(&bench.workspace, session.clone(), task, "fix it");
    let input_id = run_request.input_id.clone();
    let driver = TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(&store)));

    let error = driver
        .run_turn(
            run_request,
            options(&bench.workspace),
            Arc::new(RecordingObserver::default()),
            CancellationToken::new(),
        )
        .await
        .expect_err("the injected freeze failure stops the turn");
    assert_eq!(error.code(), ErrorCode::StorageWriteFailed);
    assert_eq!(provider.calls(), 0, "nothing was dispatched");

    let run = store.run_by_input(&input_id).await.unwrap().unwrap();
    assert_eq!(run.state, RunState::Running);
    assert!(
        store.run_steps(&run.run_id).await.unwrap().is_empty(),
        "no step was committed"
    );
    drop(driver);
    close(store).await;
}

#[tokio::test]
async fn m3_01_manifest_and_overflow() {
    let bench = bench();
    // A mandatory block larger than the window must stop before dispatch.
    let store = bench.open_store().await;
    let provider = Arc::new(ScriptedProvider::text("must not be called"));
    let runtime = Arc::new(RuntimeService::new(
        Arc::clone(&store),
        provider.clone(),
        RuntimeConfig {
            context_window_tokens: 400,
            output_reservation_tokens: 100,
            protocol_overhead_tokens: 10,
            safety_margin_tokens: 10,
            optional_token_budget: 0,
            max_attempts: 3,
            config_revision: 1,
        },
    ));
    let session = SessionId::generate();
    let task = TaskId::generate();
    let long = "x".repeat(8000);
    let run_request = request(&bench.workspace, session, task, &long);
    let driver = TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(&store)));

    let error = driver
        .run_turn(
            run_request,
            options(&bench.workspace),
            Arc::new(RecordingObserver::default()),
            CancellationToken::new(),
        )
        .await
        .expect_err("mandatory overflow stops dispatch");
    assert_eq!(error.code(), ErrorCode::MandatoryContextOverflow);
    assert_eq!(provider.calls(), 0, "overflow must not reach the provider");
    drop(driver);
    close(store).await;
}

// ---------------------------------------------------------------------------
// M3-02: questions, steering and cancel
// ---------------------------------------------------------------------------

#[tokio::test]
#[allow(clippy::too_many_lines)] // one acceptance scenario, told in order
async fn m3_02_question_restart_dedupe() {
    let bench = bench();
    let store = bench.open_store().await;
    let session = SessionId::generate();
    let task = TaskId::generate();
    let run_id = AgentRunId::generate();
    let scope = harness_runtime::question_scope(&run_id, "step-a");
    let service = HumanInputService::new(Arc::clone(&store));
    service
        .ask(
            AskRequest {
                session_id: session.clone(),
                task_id: task.clone(),
                run_id: Some(run_id.clone()),
                scope_key: scope.clone(),
                kind: "clarification".to_owned(),
                prompt: "which parser?".to_owned(),
                payload: json!({}),
                expires_at_unix_ms: None,
            },
            1_000,
        )
        .await
        .expect("question persists");
    // Asking the same scope again is the same question, not a second one.
    let duplicate_open = service
        .ask(
            AskRequest {
                session_id: session.clone(),
                task_id: task.clone(),
                run_id: Some(run_id.clone()),
                scope_key: scope.clone(),
                kind: "clarification".to_owned(),
                prompt: "which parser?".to_owned(),
                payload: json!({}),
                expires_at_unix_ms: None,
            },
            2_000,
        )
        .await
        .expect("dedupe open");
    assert_eq!(duplicate_open.created_at_unix_ms, 1_000);
    drop(service);
    close(store).await;

    // Restart: a new writer generation answers the persisted question.
    let store = bench.open_store().await;
    let service = HumanInputService::new(Arc::clone(&store));
    let first = service
        .answer(&scope, &json!("the hand-written one"), "operator", 3_000)
        .await
        .expect("answer");
    let QuestionOutcome::Answered(question) = first else {
        panic!("first answer must be recorded: {first:?}");
    };
    assert_eq!(question.state, QuestionState::Answered);
    assert_eq!(question.answered_by.as_deref(), Some("operator"));

    // The same answer is a duplicate with the stored record, not a second write.
    let again = service
        .answer(&scope, &json!("the hand-written one"), "operator", 4_000)
        .await
        .expect("duplicate answer");
    let QuestionOutcome::Duplicate(again) = again else {
        panic!("the same payload must be a duplicate: {again:?}");
    };
    assert_eq!(again.question_id, question.question_id);
    assert_eq!(again.answered_at_unix_ms, Some(3_000));

    // A different payload for an answered question is a conflict.
    let conflict = service
        .answer(&scope, &json!("the generated one"), "operator", 5_000)
        .await
        .expect("conflict answer");
    assert!(matches!(conflict, QuestionOutcome::Conflict(_)));

    // Wrong scope (a different request in the same run) is not an answer.
    let wrong_scope = harness_runtime::question_scope(&run_id, "step-b");
    let not_found = service
        .answer(&wrong_scope, &json!("anything"), "operator", 6_000)
        .await
        .expect("wrong scope");
    assert_eq!(not_found, QuestionOutcome::NotFound);

    // Empty answers and expired questions are never consent.
    let empty = service
        .answer(&scope, &json!("   "), "operator", 7_000)
        .await
        .expect_err("blank answer refused");
    assert_eq!(empty.code(), ErrorCode::InvalidPayload);

    let expired_scope = harness_runtime::question_scope(&run_id, "step-c");
    service
        .ask(
            AskRequest {
                session_id: session.clone(),
                task_id: task.clone(),
                run_id: Some(run_id.clone()),
                scope_key: expired_scope.clone(),
                kind: "clarification".to_owned(),
                prompt: "late?".to_owned(),
                payload: json!({}),
                expires_at_unix_ms: Some(10_000),
            },
            9_000,
        )
        .await
        .expect("expiring question");
    let expired = service
        .answer(&expired_scope, &json!("too late"), "operator", 12_000)
        .await
        .expect("expired answer");
    assert!(matches!(expired, QuestionOutcome::Expired(_)));

    // Exactly one stored answer for the scope.
    let questions = service.questions(&session).await.unwrap();
    assert_eq!(questions.len(), 2, "two questions, no duplicates");
    let answered = questions
        .iter()
        .find(|question| question.scope_key == scope)
        .expect("the answered question");
    assert_eq!(answered.answer, Some(json!("the hand-written one")));
    drop(service);
    close(store).await;
}

#[tokio::test]
async fn m3_02_canceled_queue_does_not_invoke() {
    let bench = bench();
    let store = bench.open_store().await;
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptStep::Events(vec![
        ProviderStreamEvent::started(),
        ProviderStreamEvent::tool_delta(
            "call-1",
            "search_text",
            json!({"query": "todo", "path": "."}).to_string(),
        ),
        ProviderStreamEvent::completed("tool_calls"),
    ])]));
    let (runtime, _) = runtime_for(&store, provider.clone(), None, None).await;
    let session = SessionId::generate();
    let task = TaskId::generate();
    let input_id = InputId::generate();
    // The run exists before the turn, so the cancel is already queued at the
    // first boundary - like a user pressing Ctrl-C while the app was starting.
    let run = store
        .start_run(&session, &task, &input_id, None)
        .await
        .expect("run starts");
    let inbox = RunInbox::new(Arc::clone(&store));
    inbox
        .cancel(&run, "user canceled", 1_000)
        .await
        .expect("cancel queues");

    let run_request = RunRequest::new(
        session.clone(),
        task.clone(),
        input_id.clone(),
        "fix it".to_owned(),
        observe_workspace(ProjectId::generate(), &bench.workspace).unwrap(),
    )
    .with_tool_schemas(coding_tool_schemas());
    let driver =
        TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(&store))).with_inbox(inbox);
    let outcome = driver
        .run_turn(
            run_request,
            options(&bench.workspace),
            Arc::new(RecordingObserver::default()),
            CancellationToken::new(),
        )
        .await
        .expect("turn runs");
    assert_eq!(
        outcome.stop,
        TurnStop::Canceled,
        "the cancel stops the turn"
    );
    assert!(
        outcome.executions.is_empty(),
        "a canceled queue must not invoke the fixture tool"
    );
    assert_eq!(provider.calls(), 1, "the model was called once");
    let finished = store.run_by_id(&run.run_id).await.unwrap().unwrap();
    assert_eq!(finished.state, RunState::Canceled);
    assert_eq!(finished.stop_reason.as_deref(), Some("canceled"));
    drop(driver);
    close(store).await;
}

#[tokio::test]
async fn m3_02_steering_reaches_next_step() {
    let bench = bench();
    let store = bench.open_store().await;
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptStep::Events(vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::tool_delta(
                "call-1",
                "search_text",
                json!({"query": "todo", "path": "."}).to_string(),
            ),
            ProviderStreamEvent::completed("tool_calls"),
        ]),
        ScriptStep::Events(vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::text("steered and finished"),
            ProviderStreamEvent::completed("stop"),
        ]),
    ]));
    let (runtime, _) = runtime_for(&store, provider.clone(), None, None).await;
    let session = SessionId::generate();
    let task = TaskId::generate();
    let input_id = InputId::generate();
    let run = store
        .start_run(&session, &task, &input_id, None)
        .await
        .expect("run starts");
    let inbox = RunInbox::new(Arc::clone(&store));
    inbox
        .steer(&run, "also check the lexer", 1_000)
        .await
        .expect("steering queues");

    let run_request = RunRequest::new(
        session.clone(),
        task.clone(),
        input_id.clone(),
        "fix it".to_owned(),
        observe_workspace(ProjectId::generate(), &bench.workspace).unwrap(),
    )
    .with_tool_schemas(coding_tool_schemas());
    let driver =
        TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(&store))).with_inbox(inbox);
    let outcome = driver
        .run_turn(
            run_request,
            options(&bench.workspace),
            Arc::new(RecordingObserver::default()),
            CancellationToken::new(),
        )
        .await
        .expect("turn runs");
    assert_eq!(outcome.stop, TurnStop::Final);

    let second = provider.seen().get(1).expect("second request").clone();
    assert!(
        second.messages.iter().any(|message| {
            message.role == MessageRole::User
                && message
                    .content
                    .contains("[steering correction from the user]")
                && message.content.contains("also check the lexer")
        }),
        "the correction reaches the next step: {:?}",
        second.messages
    );
    // The steering did not admit a second user input.
    let summary = store.session_summary(&session).await.unwrap().unwrap();
    assert_eq!(summary.input_count, 1);
    drop(driver);
    close(store).await;
}

// ---------------------------------------------------------------------------
// M3-03: budget accounting and loop detection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn m3_03_budget_reservation_atomicity() {
    let bench = bench();
    let store = bench.open_store().await;
    let ledger = BudgetLedger::new(Arc::clone(&store));
    let account = BudgetId::generate();
    ledger
        .ensure_account(&account, None, 100)
        .await
        .expect("account");

    // Ten concurrent reservations of 20 against a limit of 100: exactly five
    // may be admitted, and the account may not be overdrawn by a race.
    let mut handles = Vec::new();
    for index in 0..10 {
        let ledger = ledger.clone();
        let account = account.clone();
        handles.push(tokio::spawn(async move {
            ledger
                .reserve(
                    &account,
                    &format!("operation-{index}"),
                    "provider_attempt",
                    20,
                )
                .await
        }));
    }
    let mut admitted = 0;
    for handle in handles {
        match handle.await.expect("task") {
            Ok(_) => admitted += 1,
            Err(error) => assert_eq!(error.code(), ErrorCode::BudgetExhausted),
        }
    }
    assert_eq!(admitted, 5, "the limit admits five reservations, not six");
    let view: BudgetView = ledger.view(&account).await.expect("account view");
    assert_eq!(view.account.spent_tokens, 100);
    assert_eq!(view.remaining_tokens, 0);

    // Settling the same reservation twice with the same usage is idempotent.
    let reservation = ledger
        .reservation("operation-0")
        .await
        .expect("read reservation")
        .expect("reservation exists");
    let settled = ledger
        .settle(&reservation.reservation_id, Usage::Measured(8))
        .await
        .expect("settle");
    assert_eq!(settled.settled_tokens, Some(8));
    let after_first = ledger.view(&account).await.unwrap().account.spent_tokens;
    assert_eq!(after_first, 88, "20 reserved, 8 measured");
    let again = ledger
        .settle(&reservation.reservation_id, Usage::Measured(8))
        .await
        .expect("idempotent settle");
    assert_eq!(again.settled_tokens, Some(8));
    assert_eq!(
        ledger.view(&account).await.unwrap().account.spent_tokens,
        88,
        "duplicate settle must not double count"
    );
    let conflicting = ledger
        .settle(&reservation.reservation_id, Usage::Measured(9))
        .await
        .expect_err("a different usage is a conflict");
    assert_eq!(conflicting.code(), ErrorCode::IdempotencyConflict);

    // Unknown usage keeps the conservative bound charged.
    let reservation = ledger.reservation("operation-1").await.unwrap().unwrap();
    ledger
        .settle(&reservation.reservation_id, Usage::Unknown)
        .await
        .expect("unknown settle");
    let after_unknown = ledger.view(&account).await.unwrap().account.spent_tokens;
    assert_eq!(after_unknown, 88, "unknown usage is not zero");
    let stored = ledger.reservation("operation-1").await.unwrap().unwrap();
    assert_eq!(stored.state, BudgetReservationState::Unknown);

    // Releasing an undispatched reservation returns its bound.
    let reservation = ledger.reservation("operation-2").await.unwrap().unwrap();
    ledger
        .release(&reservation.reservation_id)
        .await
        .expect("release");
    assert_eq!(
        ledger.view(&account).await.unwrap().account.spent_tokens,
        68
    );

    // Hierarchical limits: the child has room, the parent does not.
    let parent = BudgetId::generate();
    let child = BudgetId::generate();
    ledger
        .ensure_account(&parent, None, 50)
        .await
        .expect("parent");
    ledger
        .ensure_account(&child, Some(&parent), 500)
        .await
        .expect("child");
    ledger
        .reserve(&child, "child-a", "provider_attempt", 30)
        .await
        .expect("child reservation inside the parent");
    let over = ledger
        .reserve(&child, "child-b", "provider_attempt", 30)
        .await
        .expect_err("the parent limit refuses the second child reservation");
    assert_eq!(over.code(), ErrorCode::BudgetExhausted);
    drop(ledger);
    close(store).await;
}

#[tokio::test]
async fn m3_03_loop_detection() {
    let bench = bench();
    let store = bench.open_store().await;
    // The model repeats one identical call forever; only the loop detector can
    // stop it, and the third identical request must not execute.
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptStep::Events(vec![
        ProviderStreamEvent::started(),
        ProviderStreamEvent::tool_delta(
            "call-loop",
            "search_text",
            json!({"query": "todo", "path": "."}).to_string(),
        ),
        ProviderStreamEvent::completed("tool_calls"),
    ])]));
    let (runtime, _) = runtime_for(&store, provider.clone(), None, None).await;
    let session = SessionId::generate();
    let task = TaskId::generate();
    let run_request = request(&bench.workspace, session, task, "fix it");
    let driver = TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(&store)));

    let outcome = driver
        .run_turn(
            run_request,
            options(&bench.workspace),
            Arc::new(RecordingObserver::default()),
            CancellationToken::new(),
        )
        .await
        .expect("turn runs");
    assert_eq!(outcome.stop, TurnStop::LoopDetected);
    assert_eq!(
        outcome.executions.len(),
        2,
        "the repeated third call is not executed"
    );
    assert_eq!(provider.calls(), 3, "detection happens before dispatch");
    drop(driver);
    close(store).await;
}

#[tokio::test]
async fn m3_03_valid_polling_is_not_a_loop() {
    let bench = bench();
    let store = bench.open_store().await;
    // Four polls with different arguments, then a final answer: real progress
    // must not be flagged by the signature window.
    let mut steps = Vec::new();
    for index in 0..4 {
        steps.push(ScriptStep::Events(vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::tool_delta(
                format!("call-{index}"),
                "search_text",
                json!({"query": format!("poll-{index}"), "path": "."}).to_string(),
            ),
            ProviderStreamEvent::completed("tool_calls"),
        ]));
    }
    steps.push(ScriptStep::Events(vec![
        ProviderStreamEvent::started(),
        ProviderStreamEvent::text("polling done"),
        ProviderStreamEvent::completed("stop"),
    ]));
    let provider = Arc::new(ScriptedProvider::new(steps));
    let (runtime, _) = runtime_for(&store, provider.clone(), None, None).await;
    let session = SessionId::generate();
    let task = TaskId::generate();
    let driver = TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(&store)));

    let outcome = driver
        .run_turn(
            request(&bench.workspace, session, task, "poll it"),
            options(&bench.workspace),
            Arc::new(RecordingObserver::default()),
            CancellationToken::new(),
        )
        .await
        .expect("turn runs");
    assert_eq!(outcome.stop, TurnStop::Final);
    assert_eq!(outcome.executions.len(), 4, "every poll executed");
    drop(driver);
    close(store).await;
}

#[tokio::test]
async fn m3_03_provider_usage_settles_reservation() {
    let bench = bench();
    let store = bench.open_store().await;
    // The provider reports a cumulative total for the call. The ledger must
    // settle to that number, not to the host's byte estimate.
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptStep::Events(vec![
        ProviderStreamEvent::started(),
        ProviderStreamEvent::text("a measured answer that is longer than the number says"),
        ProviderStreamEvent::usage(7, 3, 10),
        ProviderStreamEvent::completed("stop"),
    ])]));
    let (runtime, budget_id) = runtime_for(&store, provider.clone(), None, Some(1_000_000)).await;
    let driver = TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(&store)));
    let outcome = driver
        .run_turn(
            request(
                &bench.workspace,
                SessionId::generate(),
                TaskId::generate(),
                "fix it",
            ),
            options(&bench.workspace),
            Arc::new(RecordingObserver::default()),
            CancellationToken::new(),
        )
        .await
        .expect("turn runs");
    assert_eq!(outcome.stop, TurnStop::Final);
    let ledger = BudgetLedger::new(Arc::clone(&store));
    let reservation = ledger
        .reservation(&format!("attempt:{}:0:1", outcome.run_id))
        .await
        .expect("read reservation")
        .expect("the first attempt reserved");
    assert_eq!(
        reservation.settled_tokens,
        Some(10),
        "the provider's total wins over the host estimate"
    );
    let account = budget_id.expect("account id");
    assert_eq!(
        ledger.view(&account).await.unwrap().account.spent_tokens,
        10
    );
    drop(driver);
    drop(ledger);
    close(store).await;
}
// ---------------------------------------------------------------------------
// M3-04 and acceptance A09/A10/A11
// ---------------------------------------------------------------------------

fn goal_with(kind: EvidenceKind, max_continuations: u32, max_no_progress: u32) -> GoalSpec {
    GoalSpec::new(
        "repair the parser and prove it",
        vec![GoalCriterion::required("criterion-1", kind)],
    )
    .with_limits(max_continuations, max_no_progress)
}

#[tokio::test]
async fn a09_terminal_acceptance() {
    let bench = bench();

    // Variant 1: an empty final is terminal but unverifiable, and it is not retried.
    let store = bench.open_store().await;
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptStep::Events(vec![
        ProviderStreamEvent::started(),
        ProviderStreamEvent::completed("stop"),
    ])]));
    let (runtime, _) = runtime_for(&store, provider.clone(), None, None).await;
    let session = SessionId::generate();
    let task = TaskId::generate();
    let driver = TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(&store)))
        .with_goal(goal_with(EvidenceKind::Check, 2, 1));
    let outcome = driver
        .run_turn(
            request(&bench.workspace, session, task, "fix it"),
            options(&bench.workspace),
            Arc::new(RecordingObserver::default()),
            CancellationToken::new(),
        )
        .await
        .expect("turn runs");
    assert_eq!(outcome.stop, TurnStop::Unverified);
    assert_eq!(outcome.acceptance, AcceptanceState::Unverified);
    assert_ne!(outcome.acceptance, AcceptanceState::Satisfied);
    assert_eq!(provider.calls(), 1, "an empty final is not retried forever");
    let run = store.run_by_id(&outcome.run_id).await.unwrap().unwrap();
    assert_eq!(run.state, RunState::Failed);
    assert_eq!(run.stop_reason.as_deref(), Some("unverified"));
    assert_eq!(run.acceptance.as_deref(), Some("unverified"));
    drop(driver);
    close(store).await;

    // Variant 2: a response cut by the output cap is not a satisfied goal.
    let store = bench.open_store().await;
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptStep::Events(vec![
        ProviderStreamEvent::started(),
        ProviderStreamEvent::text("half an ans"),
        ProviderStreamEvent::completed("length"),
    ])]));
    let (runtime, _) = runtime_for(&store, provider.clone(), None, None).await;
    let driver = TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(&store)))
        .with_goal(goal_with(EvidenceKind::Check, 2, 1));
    let outcome = driver
        .run_turn(
            request(
                &bench.workspace,
                SessionId::generate(),
                TaskId::generate(),
                "fix it",
            ),
            options(&bench.workspace),
            Arc::new(RecordingObserver::default()),
            CancellationToken::new(),
        )
        .await
        .expect("turn runs");
    assert_eq!(outcome.stop, TurnStop::Unverified);
    assert_eq!(outcome.acceptance, AcceptanceState::Unverified);
    assert_eq!(provider.calls(), 1);
    drop(driver);
    close(store).await;

    // Variant 3: the model says it is done without the evidence the criteria
    // demand. It is continued, then stopped for no progress - never accepted.
    let store = bench.open_store().await;
    let provider = Arc::new(ScriptedProvider::text("Done! Everything works."));
    let (runtime, _) = runtime_for(&store, provider.clone(), None, None).await;
    let driver = TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(&store)))
        .with_goal(goal_with(EvidenceKind::Check, 4, 1));
    let outcome = driver
        .run_turn(
            request(
                &bench.workspace,
                SessionId::generate(),
                TaskId::generate(),
                "fix it",
            ),
            options(&bench.workspace),
            Arc::new(RecordingObserver::default()),
            CancellationToken::new(),
        )
        .await
        .expect("turn runs");
    assert_eq!(outcome.stop, TurnStop::NoProgress);
    assert_eq!(outcome.acceptance, AcceptanceState::NeedsWork);
    assert_ne!(outcome.acceptance, AcceptanceState::Satisfied);
    assert!(provider.calls() > 1, "the goal was continued once");
    let run = store.run_by_id(&outcome.run_id).await.unwrap().unwrap();
    assert_eq!(run.state, RunState::Failed);
    assert_eq!(run.acceptance.as_deref(), Some("needs_work"));
    drop(driver);
    close(store).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one acceptance scenario, told in order
async fn a10_goal_no_progress() {
    let bench = bench();

    // A needs_work verdict with a changing response keeps the continuations
    // bounded by the goal limit.
    let store = bench.open_store().await;
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptStep::Events(vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::text("attempt one"),
            ProviderStreamEvent::completed("stop"),
        ]),
        ScriptStep::Events(vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::text("attempt two"),
            ProviderStreamEvent::completed("stop"),
        ]),
        ScriptStep::Events(vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::text("attempt three"),
            ProviderStreamEvent::completed("stop"),
        ]),
        ScriptStep::Events(vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::text("attempt four"),
            ProviderStreamEvent::completed("stop"),
        ]),
    ]));
    let evaluator = Arc::new(FixedEvaluator {
        verdict: GoalVerdict::NeedsWork {
            reason: "still missing".to_owned(),
            next_action: "produce check evidence".to_owned(),
            missing: vec!["criterion-1".to_owned()],
        },
    });
    let (runtime, _) = runtime_for(&store, provider.clone(), Some(evaluator), None).await;
    let driver = TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(&store)))
        .with_goal(goal_with(EvidenceKind::Check, 2, 1));
    let outcome = driver
        .run_turn(
            request(
                &bench.workspace,
                SessionId::generate(),
                TaskId::generate(),
                "fix it",
            ),
            options(&bench.workspace),
            Arc::new(RecordingObserver::default()),
            CancellationToken::new(),
        )
        .await
        .expect("turn runs");
    assert_eq!(outcome.stop, TurnStop::GoalLimit);
    assert_eq!(outcome.acceptance, AcceptanceState::NeedsWork);
    let goal = outcome.goal.as_ref().expect("goal report");
    assert_eq!(goal.continuations, 2, "exactly the allowed continuations");
    drop(driver);
    close(store).await;

    // An external wait stops the run without polling the model again.
    let store = bench.open_store().await;
    let provider = Arc::new(ScriptedProvider::text("waiting on CI"));
    let evaluator = Arc::new(FixedEvaluator {
        verdict: GoalVerdict::ExternalWait {
            detail: "CI job 42".to_owned(),
            missing: vec!["criterion-1".to_owned()],
        },
    });
    let (runtime, _) = runtime_for(&store, provider.clone(), Some(evaluator), None).await;
    let driver = TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(&store)))
        .with_goal(goal_with(EvidenceKind::Check, 4, 1));
    let outcome = driver
        .run_turn(
            request(
                &bench.workspace,
                SessionId::generate(),
                TaskId::generate(),
                "fix it",
            ),
            options(&bench.workspace),
            Arc::new(RecordingObserver::default()),
            CancellationToken::new(),
        )
        .await
        .expect("turn runs");
    assert_eq!(outcome.stop, TurnStop::ExternalWait);
    assert_eq!(outcome.acceptance, AcceptanceState::ExternalWait);
    assert_eq!(provider.calls(), 1, "external waits are not polled");
    let run = store.run_by_id(&outcome.run_id).await.unwrap().unwrap();
    assert_eq!(run.state, RunState::Paused);
    assert_eq!(run.stop_reason.as_deref(), Some("external_wait"));
    drop(driver);
    close(store).await;

    // A malformed evaluator cannot accept the goal: the run fails closed.
    let store = bench.open_store().await;
    let provider = Arc::new(ScriptedProvider::text("trust me"));
    let (runtime, _) = runtime_for(
        &store,
        provider.clone(),
        Some(Arc::new(BrokenEvaluator)),
        None,
    )
    .await;
    let driver = TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(&store)))
        .with_goal(goal_with(EvidenceKind::Check, 4, 1));
    let run_request = request(
        &bench.workspace,
        SessionId::generate(),
        TaskId::generate(),
        "fix it",
    );
    let input_id = run_request.input_id.clone();
    let error = driver
        .run_turn(
            run_request,
            options(&bench.workspace),
            Arc::new(RecordingObserver::default()),
            CancellationToken::new(),
        )
        .await
        .expect_err("a broken evaluator fails the turn");
    assert_eq!(error.code(), ErrorCode::ServiceUnavailable);
    let run = store.run_by_input(&input_id).await.unwrap().unwrap();
    assert_eq!(run.state, RunState::Failed);
    assert_eq!(run.acceptance.as_deref(), Some("unverified"));
    assert_ne!(run.acceptance.as_deref(), Some("satisfied"));
    drop(driver);
    close(store).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one acceptance scenario, told in order
async fn a11_human_input_grant() {
    let bench = bench();
    let store = bench.open_store().await;
    let provider = Arc::new(ScriptedProvider::text("I need a decision"));
    let evaluator = Arc::new(FixedEvaluator {
        verdict: GoalVerdict::NeedsInput {
            question: "which parser should I repair?".to_owned(),
            missing: vec!["criterion-1".to_owned()],
        },
    });
    let (runtime, _) = runtime_for(&store, provider.clone(), Some(evaluator), None).await;
    let session = SessionId::generate();
    let task = TaskId::generate();
    let driver = TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(&store)))
        .with_goal(goal_with(EvidenceKind::Check, 4, 1));
    let outcome = driver
        .run_turn(
            request(&bench.workspace, session.clone(), task.clone(), "fix it"),
            options(&bench.workspace),
            Arc::new(RecordingObserver::default()),
            CancellationToken::new(),
        )
        .await
        .expect("turn runs");
    assert_eq!(outcome.stop, TurnStop::NeedsInput);
    assert_eq!(outcome.acceptance, AcceptanceState::NeedsInput);
    let question_id = outcome.pending_question.clone().expect("question id");
    let run = store.run_by_id(&outcome.run_id).await.unwrap().unwrap();
    assert_eq!(run.state, RunState::Paused);
    assert_eq!(run.awaiting_question_id.as_ref(), Some(&question_id));
    let question = store.question(&question_id).await.unwrap().unwrap();
    assert_eq!(question.state, QuestionState::Open);
    let scope = question.scope_key.clone();
    // M3 half only: no tool approval exists yet, so A11 stays partial until M4.
    drop(driver);
    close(store).await;

    // Restart: the question is answered through the CLI, then through the
    // service, and the store proves exactly one answer was consumed. The CLI
    // needs the writer for itself, so nothing holds it while it runs.
    let answered = run_cli(
        &[
            "input",
            "answer",
            "--data-dir",
            bench.data_dir.to_str().unwrap(),
            "--scope-key",
            &scope,
            "--text",
            "the hand-written parser",
            "--json",
        ],
        &bench.home,
    );
    assert!(
        answered.status.success(),
        "cli answer failed: {}",
        String::from_utf8_lossy(&answered.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&answered.stdout).expect("answer JSON");
    assert_eq!(parsed["outcome"], "answered");

    let store = bench.open_store().await;
    let service = HumanInputService::new(Arc::clone(&store));
    let duplicate = service
        .answer(
            &scope,
            &json!("the hand-written parser"),
            "operator",
            harness_runtime::now_unix_ms(),
        )
        .await
        .expect("duplicate answer");
    assert!(matches!(duplicate, QuestionOutcome::Duplicate(_)));
    let conflict = service
        .answer(
            &scope,
            &json!("the generated parser"),
            "operator",
            harness_runtime::now_unix_ms(),
        )
        .await
        .expect("conflicting answer");
    assert!(matches!(conflict, QuestionOutcome::Conflict(_)));
    let wrong = service
        .answer(
            "run_00000000-0000-7000-8000-000000000000|step-x",
            &json!("no"),
            "operator",
            harness_runtime::now_unix_ms(),
        )
        .await
        .expect("wrong scope");
    assert_eq!(wrong, QuestionOutcome::NotFound);
    drop(service);
    close(store).await;

    // The CLI status of the waiting run names the question and separates run
    // status from acceptance.
    let status = run_cli(
        &[
            "status",
            "--data-dir",
            bench.data_dir.to_str().unwrap(),
            "--session-id",
            session.as_str(),
            "--json",
        ],
        &bench.home,
    );
    assert!(
        status.status.success(),
        "cli status failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&status.stdout).expect("status JSON");
    assert_eq!(parsed["run"]["state"], "paused");
    assert_eq!(parsed["run"]["awaiting_question_id"], question_id.as_str());
    assert_eq!(parsed["run"]["acceptance"], "needs_input");
    assert_eq!(provider.calls(), 1, "the pause did not re-run the model");
}

#[tokio::test]
async fn m3_04_status_separates_acceptance() {
    let bench = bench();
    let store = bench.open_store().await;
    let provider = Arc::new(ScriptedProvider::text("all done, trust me"));
    let (runtime, _) = runtime_for(&store, provider.clone(), None, None).await;
    let session = SessionId::generate();
    let task = TaskId::generate();
    let driver = TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(&store)))
        .with_goal(goal_with(EvidenceKind::Artifact, 3, 1));
    let outcome = driver
        .run_turn(
            request(&bench.workspace, session.clone(), task, "fix it"),
            options(&bench.workspace),
            Arc::new(RecordingObserver::default()),
            CancellationToken::new(),
        )
        .await
        .expect("turn runs");
    assert_eq!(outcome.stop, TurnStop::NoProgress);
    drop(driver);
    close(store).await;

    let status = run_cli(
        &[
            "status",
            "--data-dir",
            bench.data_dir.to_str().unwrap(),
            "--session-id",
            session.as_str(),
            "--json",
        ],
        &bench.home,
    );
    assert!(status.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&status.stdout).expect("status JSON");
    // The run status and the acceptance are distinct fields with distinct
    // values: a failed run is not an accepted task.
    assert_eq!(parsed["run"]["state"], "failed");
    assert_eq!(parsed["run"]["stop_reason"], "no_progress");
    assert_eq!(parsed["run"]["acceptance"], "needs_work");
    assert_ne!(parsed["run"]["state"], parsed["run"]["acceptance"]);
}

#[tokio::test]
async fn m3_04_cli_mock_profile() {
    let bench = bench();
    let output = run_cli(
        &[
            "chat",
            "--headless",
            "--mock",
            "--prompt",
            "say something",
            "--cwd",
            bench.workspace.to_str().unwrap(),
            "--json",
        ],
        &bench.home,
    );
    assert!(
        output.status.success(),
        "mock run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("run JSON");
    assert_eq!(parsed["fixture"], true, "the mock profile is labelled");
    assert_eq!(parsed["acceptance"], "not_evaluated");
    assert_eq!(parsed["run"]["state"], "completed");
    assert_eq!(parsed["run"]["stop"], "final");

    // The same profile with an unsatisfiable goal: the mock says it is done and
    // the host refuses to accept it, with a typed stop and a bounded loop.
    let output = run_cli(
        &[
            "chat",
            "--headless",
            "--mock",
            "--goal",
            "repair the parser",
            "--criteria",
            "check",
            "--max-continuations",
            "2",
            "--prompt",
            "fix it",
            "--cwd",
            bench.workspace.to_str().unwrap(),
            "--json",
        ],
        &bench.home,
    );
    assert!(
        output.status.success(),
        "goal run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("goal JSON");
    assert_eq!(parsed["fixture"], true);
    assert_eq!(parsed["acceptance"], "needs_work");
    assert_ne!(parsed["acceptance"], "satisfied");
    assert_eq!(parsed["run"]["state"], "failed");
    assert_eq!(parsed["run"]["stop"], "no_progress");
    let goal = &parsed["goal"];
    assert_eq!(goal["missing"][0], "criterion-1");
    assert!(goal["continuations"].as_u64().unwrap() >= 1);
}

#[tokio::test]
async fn m3_04_goal_satisfied_by_evidence() {
    let bench = bench();
    let store = bench.open_store().await;
    // The model runs one committed tool call (a real search through the P3
    // gate), then answers. A goal requiring tool-execution evidence is
    // satisfied by the receipt, not by the prose.
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptStep::Events(vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::tool_delta(
                "call-1",
                "search_text",
                json!({"query": "todo", "path": "."}).to_string(),
            ),
            ProviderStreamEvent::completed("tool_calls"),
        ]),
        ScriptStep::Events(vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::text("found and fixed"),
            ProviderStreamEvent::completed("stop"),
        ]),
    ]));
    let (runtime, _) = runtime_for(&store, provider.clone(), None, None).await;
    let driver = TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(&store)))
        .with_goal(goal_with(EvidenceKind::ToolExecution, 2, 1));
    let outcome = driver
        .run_turn(
            request(
                &bench.workspace,
                SessionId::generate(),
                TaskId::generate(),
                "fix it",
            ),
            options(&bench.workspace),
            Arc::new(RecordingObserver::default()),
            CancellationToken::new(),
        )
        .await
        .expect("turn runs");
    assert_eq!(outcome.stop, TurnStop::Final);
    assert_eq!(outcome.acceptance, AcceptanceState::Satisfied);
    let run = store.run_by_id(&outcome.run_id).await.unwrap().unwrap();
    assert_eq!(run.state, RunState::Completed);
    assert_eq!(run.acceptance.as_deref(), Some("satisfied"));
    drop(driver);
    close(store).await;
}

#[tokio::test]
async fn m3_04_budget_guards_dispatch() {
    let bench = bench();

    // A budget too small for one frozen step refuses before dispatch: the run
    // never calls the model, and the typed error says why.
    let store = bench.open_store().await;
    let provider = Arc::new(ScriptedProvider::text("must not be called"));
    let (runtime, budget_id) = runtime_for(&store, provider.clone(), None, Some(1)).await;
    let driver = TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(&store)))
        .with_goal(goal_with(EvidenceKind::Check, 4, 2));
    let error = driver
        .run_turn(
            request(
                &bench.workspace,
                SessionId::generate(),
                TaskId::generate(),
                "fix it",
            ),
            options(&bench.workspace),
            Arc::new(RecordingObserver::default()),
            CancellationToken::new(),
        )
        .await
        .expect_err("a step that the budget cannot fund is refused");
    assert_eq!(error.code(), ErrorCode::BudgetExhausted);
    assert_eq!(
        provider.calls(),
        0,
        "nothing was dispatched without a reservation"
    );
    let account = budget_id.expect("account id");
    let view = BudgetLedger::new(Arc::clone(&store))
        .view(&account)
        .await
        .expect("account view");
    assert_eq!(
        view.account.spent_tokens, 0,
        "the refused step charged nothing"
    );
    drop(driver);
    close(store).await;

    // A budget large enough is reserved before dispatch and settled to the
    // measured usage afterwards.
    let store = bench.open_store().await;
    let provider = Arc::new(ScriptedProvider::text("budgeted answer"));
    let (runtime, budget_id) = runtime_for(&store, provider.clone(), None, Some(1_000_000)).await;
    let driver = TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(&store)));
    let outcome = driver
        .run_turn(
            request(
                &bench.workspace,
                SessionId::generate(),
                TaskId::generate(),
                "fix it",
            ),
            options(&bench.workspace),
            Arc::new(RecordingObserver::default()),
            CancellationToken::new(),
        )
        .await
        .expect("budgeted turn runs");
    assert_eq!(outcome.stop, TurnStop::Final);
    let account = budget_id.expect("account id");
    let ledger = BudgetLedger::new(Arc::clone(&store));
    let view = ledger.view(&account).await.expect("account view");
    assert!(
        view.account.spent_tokens > 0 && view.account.spent_tokens < 1_000_000,
        "the account is charged the measured usage, not the bound: {}",
        view.account.spent_tokens
    );
    let reservation = ledger
        .reservation(&format!("attempt:{}:0:1", outcome.run_id))
        .await
        .expect("read reservation")
        .expect("the first attempt reserved");
    assert_eq!(reservation.settled_tokens, Some(view.account.spent_tokens));
    assert_eq!(reservation.state, BudgetReservationState::Settled);
    drop(driver);
    drop(ledger);
    close(store).await;
}

// ---------------------------------------------------------------------------
// Compatibility: runtime schema 1 upgrades in place, newer refuses writes
// ---------------------------------------------------------------------------

#[tokio::test]
#[allow(clippy::too_many_lines)] // one upgrade, told in order
async fn m3_01_runtime_schema_upgrade() {
    let bench = bench();
    let store = bench.open_store().await;
    // Keep a pre-M3 row so the upgrade can prove it is additive.
    let session = SessionId::generate();
    let task = TaskId::generate();
    let input = InputId::generate();
    SessionService::new(Arc::clone(&store))
        .admit_input(AdmitInputRequest {
            session_id: session.clone(),
            task_id: task.clone(),
            input_id: input.clone(),
            expected_sequence: 1,
            authority: harness_types::SourceAuthority::User,
            raw_text: "before the upgrade".to_owned(),
            workspace: observe_workspace(ProjectId::generate(), &bench.workspace).unwrap(),
            initial_plan_items: Vec::new(),
        })
        .await
        .expect("pre-upgrade input");
    close(store).await;

    // Make the database look like a runtime schema 1 host wrote it: the M3
    // tables are absent and the marker says 1.
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
        "DROP TABLE IF EXISTS runs",
        "DROP TABLE IF EXISTS run_steps",
        "DROP TABLE IF EXISTS budget_accounts",
        "DROP TABLE IF EXISTS budget_reservations",
        "DROP TABLE IF EXISTS questions",
        "DROP TABLE IF EXISTS run_commands",
    ] {
        sqlx::query(statement)
            .execute(&pool)
            .await
            .expect("drop M3 table");
    }
    sqlx::query("DELETE FROM runtime_schema_migrations")
        .execute(&pool)
        .await
        .expect("clear runtime version");
    sqlx::query("INSERT INTO runtime_schema_migrations(version) VALUES (1)")
        .execute(&pool)
        .await
        .expect("rewind runtime version");
    pool.close().await;

    // Opening a writer upgrades the copy in place and keeps the old rows.
    let store = bench.open_store().await;
    let revisions = store.all_schema_revisions().await.expect("revisions");
    // The assertion is that the marker reached the revision this host writes,
    // not that it is a particular number: a hard-coded 2 turned this upgrade test
    // into a change-detector the first time another milestone added a table.
    assert_eq!(
        revisions.get("runtime").copied(),
        Some(harness_store_sqlite::RUNTIME_SCHEMA_VERSION)
    );
    // And the M11 tables exist after the same upgrade, because the slice runs
    // from whatever revision the database was at.
    store
        .create_schedule(&harness_store_sqlite::StoredScheduleRecord {
            schedule_id: "m3-upgrade-fixture".to_owned(),
            title: "created after the upgrade".to_owned(),
            state: "active".to_owned(),
            revision: 1,
            next_due_unix_ms: 1_700_000_000_000,
            spec_json: r#"{"kind":"once","at_unix_ms":1700000000000}"#.to_owned(),
            grants_json: "{}".to_owned(),
        })
        .await
        .expect("M11 tables exist after the upgrade");
    // And the M12 lease table, for the same reason: every slice runs from
    // whatever revision the database was at, so an old database gains the new
    // table without losing a row.
    store
        .create_backend_lease(&harness_store_sqlite::StoredBackendLease {
            lease_id: "m3-upgrade-lease".to_owned(),
            owner_generation: 1,
            host_id: "host_m3_upgrade".to_owned(),
            session_id: session.as_str().to_owned(),
            task_id: task.as_str().to_owned(),
            tool_execution_id: "execution.m3-upgrade".to_owned(),
            backend: "fixture".to_owned(),
            profile: "containment".to_owned(),
            pid: None,
            lock_path: "leases/m3-upgrade.owner.lock".to_owned(),
            state: "acquiring".to_owned(),
            enforced_json: "[]".to_owned(),
            not_claimed_json: "[]".to_owned(),
            artifact_id: None,
            artifact_digest: None,
            created_at_unix_ms: 1_700_000_000_000,
            heartbeat_at_unix_ms: 1_700_000_000_000,
            released_at_unix_ms: None,
            recovery_json: None,
        })
        .await
        .expect("M12 tables exist after the upgrade");
    assert_eq!(
        store
            .session_summary(&session)
            .await
            .unwrap()
            .unwrap()
            .input_count,
        1,
        "the pre-M3 input survived the upgrade"
    );
    let run = store
        .start_run(&session, &task, &input, None)
        .await
        .expect("M3 tables exist after the upgrade");
    assert_eq!(run.state, RunState::Running);
    close(store).await;

    // A database from a newer runtime host is refused for writes, not
    // silently downgraded.
    let pool = sqlx::SqlitePool::connect(&url)
        .await
        .expect("raw connection");
    sqlx::query("DELETE FROM runtime_schema_migrations")
        .execute(&pool)
        .await
        .expect("clear runtime version");
    sqlx::query("INSERT INTO runtime_schema_migrations(version) VALUES (99)")
        .execute(&pool)
        .await
        .expect("bump runtime version");
    pool.close().await;
    let error = SqliteStore::open_writer(WriterOpenOptions::new(
        bench.data_dir.clone(),
        HostId::generate(),
    ))
    .await
    .expect_err("a newer runtime schema is refused");
    assert_eq!(error.code(), ErrorCode::MigrationFailed);

    // Read-only inspection still sees the newer store.
    let reader = SqliteStore::open_read_only(&bench.data_dir)
        .await
        .expect("read-only open");
    let revisions = reader.all_schema_revisions().await.expect("revisions");
    assert_eq!(revisions.get("runtime").copied(), Some(99));
    reader.close().await.expect("reader closes");
}
