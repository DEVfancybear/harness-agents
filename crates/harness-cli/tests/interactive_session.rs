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
    store: Arc<SqliteStore>,
}

async fn bench() -> Bench {
    let temp = tempfile::tempdir().expect("temp root");
    let workspace = temp.path().join("repo with spaces");
    std::fs::create_dir_all(workspace.join("src")).expect("workspace");
    std::fs::write(
        workspace.join("src").join("parser.rs"),
        "fn parse() { todo!() }\n",
    )
    .expect("fixture file");
    let store = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(
            temp.path().join("data"),
            HostId::generate(),
        ))
        .await
        .expect("store opens"),
    );
    Bench {
        _temp: temp,
        workspace,
        store,
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

fn driver(bench: &Bench, provider: Arc<SequenceProvider>) -> TurnDriver {
    let runtime = Arc::new(RuntimeService::new(
        Arc::clone(&bench.store),
        provider,
        RuntimeConfig::default(),
    ));
    TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(&bench.store)))
}

async fn run(
    bench: &Bench,
    provider: Arc<SequenceProvider>,
    limits: TurnLimits,
) -> (TurnOutcome, Arc<RecordingObserver>) {
    let observer = Arc::new(RecordingObserver::default());
    let outcome = driver(bench, provider)
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
    let bench = bench().await;
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
    let bench = bench().await;
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
    let bench = bench().await;
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
