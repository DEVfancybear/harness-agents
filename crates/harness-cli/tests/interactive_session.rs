//! `HA_LAUNCH` H04 acceptance for the application turn loop and the interactive
//! session service.
//!
//! Everything here is real: a `SQLite` store with writer fencing, a workspace on
//! disk, the P3 tool gate with intents and receipts, and a provider that is a
//! deterministic sequence fixture rather than a live model. No live credential is
//! used, so a live provider smoke stays explicitly not run.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use harness_providers::{
    CancellationToken, MessageRole, ModelCapabilities, ModelProvider, ProviderFuture,
    ProviderRequest, ProviderStreamEvent,
};
use harness_runtime::{RunRequest, RuntimeConfig, RuntimeService};
use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
use harness_tools::{
    ApprovalMode, ToolExecutionService, TurnDriver, TurnLimits, TurnObserver, TurnOptions,
    TurnOutcome, TurnProgress, TurnStop, coding_tool_schemas, observe_workspace,
};
use harness_types::{HostId, InputId, ProjectId, SessionId, TaskId};
use serde_json::json;

/// Deterministic provider: one scripted response per call, recording every
/// request it is asked to answer.
struct SequenceProvider {
    responses: Vec<Vec<ProviderStreamEvent>>,
    calls: AtomicUsize,
    seen: Mutex<Vec<ProviderRequest>>,
}

impl SequenceProvider {
    fn new(responses: Vec<Vec<ProviderStreamEvent>>) -> Self {
        assert!(
            !responses.is_empty(),
            "a sequence needs at least one response"
        );
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

impl ModelProvider for SequenceProvider {
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

/// Records the progress a UI would render.
#[derive(Default)]
struct RecordingObserver {
    progress: Mutex<Vec<TurnProgress>>,
}

impl RecordingObserver {
    fn progress(&self) -> Vec<TurnProgress> {
        self.progress.lock().expect("progress log").clone()
    }
}

impl TurnObserver for RecordingObserver {
    fn observe(&self, progress: TurnProgress) {
        self.progress.lock().expect("progress log").push(progress);
    }
}

struct Bench {
    _temp: tempfile::TempDir,
    workspace: std::path::PathBuf,
    data_dir: std::path::PathBuf,
}

impl Bench {
    /// Open a store writer.
    ///
    /// Each open acquires a new writer generation, and a turn owns its writer for
    /// the duration of the turn — exactly how the application service behaves. A
    /// follow-up turn therefore runs on a newer generation, which is what the
    /// accepted task-lease rule requires.
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
    let workspace = temp.path().join("repo with spaces");
    std::fs::create_dir_all(workspace.join("src")).expect("workspace");
    std::fs::write(
        workspace.join("src").join("parser.rs"),
        "fn parse() { todo!() }\n",
    )
    .expect("fixture file");
    let data_dir = temp.path().join("data");
    Bench {
        _temp: temp,
        workspace,
        data_dir,
    }
}

fn options(workspace: &std::path::Path, limits: TurnLimits) -> TurnOptions {
    TurnOptions {
        workspace_root: workspace.to_path_buf(),
        actor_id: "test.actor".to_owned(),
        approvals: ApprovalMode::Auto,
        limits,
    }
}

fn request(workspace: &std::path::Path, text: &str) -> RunRequest {
    let observation =
        observe_workspace(ProjectId::generate(), workspace).expect("workspace observation");
    RunRequest::new(
        SessionId::generate(),
        TaskId::generate(),
        InputId::generate(),
        text,
        observation,
    )
    .with_tool_schemas(coding_tool_schemas())
}

fn driver(store: &Arc<SqliteStore>, provider: Arc<SequenceProvider>) -> TurnDriver {
    let runtime = Arc::new(RuntimeService::new(
        Arc::clone(store),
        provider,
        RuntimeConfig::default(),
    ));
    TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(store)))
}

/// Release the writer so the next turn can acquire a newer generation.
async fn close_store(store: Arc<SqliteStore>) {
    Arc::try_unwrap(store)
        .expect("the turn released its store handles")
        .close()
        .await
        .expect("store closes");
}

async fn run(
    bench: &Bench,
    provider: Arc<SequenceProvider>,
    limits: TurnLimits,
) -> (TurnOutcome, Arc<RecordingObserver>) {
    let observer = Arc::new(RecordingObserver::default());
    let store = bench.open_store().await;
    let outcome = driver(&store, provider)
        .run_turn(
            request(&bench.workspace, "find the todo and tell me about it"),
            options(&bench.workspace, limits),
            Arc::clone(&observer) as Arc<dyn TurnObserver>,
            CancellationToken::new(),
        )
        .await
        .expect("the turn runs");
    (outcome, observer)
}

#[tokio::test]
async fn g2_tool_results_return_to_the_model_and_the_turn_ends_with_the_answer() {
    let bench = bench();
    let provider = Arc::new(SequenceProvider::new(vec![
        vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::tool_delta(
                "call-1",
                "search_text",
                json!({"query": "todo", "path": "."}).to_string(),
            ),
            ProviderStreamEvent::completed("tool_calls"),
        ],
        vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::text("the fixture parser still has a todo"),
            ProviderStreamEvent::completed("stop"),
        ],
    ]));

    let (outcome, observer) = run(&bench, Arc::clone(&provider), TurnLimits::default()).await;

    assert_eq!(outcome.stop, TurnStop::Final);
    assert_eq!(outcome.tool_calls, 1);
    assert!(
        outcome.final_text.contains("fixture parser"),
        "{}",
        outcome.final_text
    );
    assert_eq!(outcome.executions.len(), 1);

    let seen = provider.seen();
    assert_eq!(seen.len(), 2, "one provider call per step");
    let second = &seen[1];
    assert!(
        second
            .messages
            .iter()
            .any(|message| message.role == MessageRole::Tool
                && message.content.contains("search_text")),
        "the tool result must be sent back to the model: {:?}",
        second.messages
    );
    assert!(
        second
            .messages
            .iter()
            .any(|message| message.role == MessageRole::Assistant),
        "the requested tool calls are recorded as the assistant turn"
    );

    let progress = observer.progress();
    assert!(progress.iter().any(
        |item| matches!(item, TurnProgress::ToolStarted { name, .. } if name == "search_text")
    ));
    assert!(progress.iter().any(
        |item| matches!(item, TurnProgress::ToolSettled { name, ok: true } if name == "search_text")
    ));
    assert!(progress.iter().any(
        |item| matches!(item, TurnProgress::TextDelta(text) if text.contains("fixture parser"))
    ));
}

#[tokio::test]
async fn g2_a_failed_tool_call_is_reported_instead_of_ending_the_turn() {
    let bench = bench();
    let provider = Arc::new(SequenceProvider::new(vec![
        vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::tool_delta("call-1", "not_a_real_tool", json!({}).to_string()),
            ProviderStreamEvent::completed("tool_calls"),
        ],
        vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::text("recovered with a different approach"),
            ProviderStreamEvent::completed("stop"),
        ],
    ]));

    let (outcome, observer) = run(&bench, Arc::clone(&provider), TurnLimits::default()).await;

    assert_eq!(outcome.stop, TurnStop::Final);
    let seen = provider.seen();
    assert_eq!(seen.len(), 2, "the loop continued after the failed tool");
    assert!(
        seen[1]
            .messages
            .iter()
            .any(|message| message.role == MessageRole::Tool && message.content.contains("failed")),
        "the failure must be handed back to the model: {:?}",
        seen[1].messages
    );
    assert!(
        observer
            .progress()
            .iter()
            .any(|item| matches!(item, TurnProgress::ToolSettled { ok: false, .. }))
    );
}

#[tokio::test]
async fn g2_the_tool_loop_is_bounded_and_reports_which_bound_stopped_it() {
    let bench = bench();
    // The provider always asks for another tool call: without a bound this would
    // loop forever.
    let provider = Arc::new(SequenceProvider::new(vec![vec![
        ProviderStreamEvent::started(),
        ProviderStreamEvent::tool_delta(
            "call-loop",
            "search_text",
            json!({"query": "todo", "path": "."}).to_string(),
        ),
        ProviderStreamEvent::completed("tool_calls"),
    ]]));

    let limits = TurnLimits {
        max_steps: 2,
        max_tool_calls: 16,
        deadline: Duration::from_mins(1),
    };
    let (outcome, _observer) = run(&bench, Arc::clone(&provider), limits).await;

    assert_eq!(outcome.stop, TurnStop::StepLimit);
    assert!(
        provider.seen().len() <= limits.max_steps as usize + 1,
        "the loop must stop at the step bound: {} calls",
        provider.seen().len()
    );
}

/// Build one request that belongs to an existing session and task.
fn request_for(
    workspace: &std::path::Path,
    session_id: &SessionId,
    task_id: &TaskId,
    text: &str,
) -> RunRequest {
    let observation =
        observe_workspace(ProjectId::generate(), workspace).expect("workspace observation");
    RunRequest::new(
        session_id.clone(),
        task_id.clone(),
        InputId::generate(),
        text,
        observation,
    )
    .with_tool_schemas(coding_tool_schemas())
}

#[tokio::test]
async fn g3_a_second_input_in_the_same_session_carries_real_context() {
    let bench = bench();
    let provider = Arc::new(SequenceProvider::new(vec![
        vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::text("first answer"),
            ProviderStreamEvent::completed("stop"),
        ],
        vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::text("second answer"),
            ProviderStreamEvent::completed("stop"),
        ],
    ]));
    let task_id = TaskId::generate();

    let first_store = bench.open_store().await;
    let first = driver(&first_store, Arc::clone(&provider))
        .run_turn(
            request_for(
                &bench.workspace,
                &SessionId::generate(),
                &task_id,
                "first user message",
            ),
            options(&bench.workspace, TurnLimits::default()),
            Arc::new(RecordingObserver::default()) as Arc<dyn TurnObserver>,
            CancellationToken::new(),
        )
        .await
        .expect("the first turn runs");
    assert_eq!(first.stop, TurnStop::Final);
    assert_eq!(first.final_text, "first answer");
    close_store(first_store).await;

    // The accepted P1 journal admits one user input per session, so the follow-up
    // is a new session linked to its predecessor and sharing the task identity.
    let second_store = bench.open_store().await;
    let second = driver(&second_store, Arc::clone(&provider))
        .run_turn_continuing(
            &first.session_id,
            request_for(
                &bench.workspace,
                &SessionId::generate(),
                &task_id,
                "second user message",
            ),
            options(&bench.workspace, TurnLimits::default()),
            Arc::new(RecordingObserver::default()) as Arc<dyn TurnObserver>,
            CancellationToken::new(),
        )
        .await
        .expect("the continuation turn runs");
    close_store(second_store).await;
    assert_eq!(second.stop, TurnStop::Final);
    assert_eq!(second.final_text, "second answer");
    assert_eq!(second.task_id, first.task_id, "the task identity is kept");
    assert_ne!(
        second.session_id, first.session_id,
        "each user input owns its session in the accepted journal model"
    );

    let seen = provider.seen();
    assert_eq!(seen.len(), 2, "one provider call per input");
    let second_packet = &seen[1].messages[1].content;
    assert!(
        second_packet.contains("second user message"),
        "the new input reaches the model: {second_packet}"
    );
    assert_ne!(
        &seen[0].messages[1].content, second_packet,
        "the second turn must rebuild context from the session journal, not start empty"
    );
}

#[tokio::test]
async fn g3_the_foundation_admits_one_input_per_session_and_says_so() {
    let bench = bench();
    let provider = Arc::new(SequenceProvider::new(vec![vec![
        ProviderStreamEvent::started(),
        ProviderStreamEvent::text("only answer"),
        ProviderStreamEvent::completed("stop"),
    ]]));
    let store = bench.open_store().await;
    let driver = driver(&store, Arc::clone(&provider));
    let session_id = SessionId::generate();
    let task_id = TaskId::generate();

    driver
        .run_turn(
            request_for(&bench.workspace, &session_id, &task_id, "first message"),
            options(&bench.workspace, TurnLimits::default()),
            Arc::new(RecordingObserver::default()) as Arc<dyn TurnObserver>,
            CancellationToken::new(),
        )
        .await
        .expect("the first turn runs");

    // A second admission in the same session is refused by the accepted journal:
    // this is exactly why the conversation uses a linked-session chain instead.
    let conflict = driver
        .run_turn(
            request_for(&bench.workspace, &session_id, &task_id, "second message"),
            options(&bench.workspace, TurnLimits::default()),
            Arc::new(RecordingObserver::default()) as Arc<dyn TurnObserver>,
            CancellationToken::new(),
        )
        .await
        .expect_err("one input per session is the accepted foundation rule");
    assert_eq!(
        conflict.code(),
        harness_types::ErrorCode::IdempotencyConflict
    );
    assert!(
        conflict
            .to_string()
            .contains("more than one admitted input"),
        "{conflict}"
    );
}
