//! The action bank (`HarnessMM`'s guarded decision reuse) in the real turn loop.
//!
//! Everything here is real except the model: a `SQLite` store, the tool gate
//! (policy, approval, intent, receipt), real processes in a disposable Git
//! repo, and a scripted provider that records every model call it gets. A
//! banked call that runs "without a model call" is checked by counting them.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use harness_providers::{
    CancellationToken, MessageRole, ModelCapabilities, ModelProvider, ProviderFuture,
    ProviderMessage, ProviderRequest, ProviderStreamEvent,
};
use harness_runtime::{RunRequest, RuntimeConfig, RuntimeService};
use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
use harness_tools::{
    ActionBank, ApprovalAnswer, ApprovalGate, ApprovalMode, ApprovalProposal, BankChoice,
    BankSelection, BankSelector, ToolExecutionService, TurnDriver, TurnLimits, TurnObserver,
    TurnOptions, TurnOutcome, TurnProgress, TurnStop, coding_tool_schemas, observe_workspace,
};
use harness_types::{HostId, InputId, ProjectId, SessionId, TaskId};
use serde_json::{Value, json};

struct Bench {
    _temp: tempfile::TempDir,
    data_dir: PathBuf,
    workspace: PathBuf,
    project_id: ProjectId,
}

fn bench() -> Bench {
    let temp = tempfile::tempdir().expect("temp root");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(workspace.join("src")).expect("source dir");
    std::fs::write(workspace.join("README.md"), "bank fixture\n").expect("fixture");
    git(&workspace, &["init"]);
    git(
        &workspace,
        &["config", "user.email", "bank@example.invalid"],
    );
    git(&workspace, &["config", "user.name", "Bank Fixture"]);
    git(&workspace, &["add", "."]);
    git(&workspace, &["commit", "-m", "fixture baseline"]);
    Bench {
        data_dir: temp.path().join("data"),
        _temp: temp,
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
    assert!(output.status.success(), "git {arguments:?} failed");
}

async fn open_store(bench: &Bench) -> Arc<SqliteStore> {
    Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(
            bench.data_dir.clone(),
            HostId::generate(),
        ))
        .await
        .expect("store opens"),
    )
}

/// One scripted response per model call; every request is kept.
struct Scripted {
    responses: Vec<Vec<ProviderStreamEvent>>,
    calls: AtomicUsize,
    seen: Mutex<Vec<ProviderRequest>>,
}

impl Scripted {
    fn new(responses: Vec<Vec<ProviderStreamEvent>>) -> Arc<Self> {
        Arc::new(Self {
            responses,
            calls: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn seen(&self) -> Vec<ProviderRequest> {
        self.seen.lock().expect("requests").clone()
    }
}

impl ModelProvider for Scripted {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::deepseek_fixture()
    }

    fn stream(&self, request: ProviderRequest, _cancellation: CancellationToken) -> ProviderFuture {
        self.seen.lock().expect("requests").push(request.clone());
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

struct Silent;

impl TurnObserver for Silent {
    fn observe(&self, _progress: TurnProgress) {}
}

fn calls(calls: &[(&str, &str, Value)]) -> Vec<ProviderStreamEvent> {
    let mut events = vec![ProviderStreamEvent::started()];
    for (id, name, arguments) in calls {
        events.push(ProviderStreamEvent::tool_delta(
            *id,
            *name,
            arguments.to_string(),
        ));
    }
    events.push(ProviderStreamEvent::completed("tool_calls"));
    events
}

fn answer(text: &str) -> Vec<ProviderStreamEvent> {
    vec![
        ProviderStreamEvent::started(),
        ProviderStreamEvent::text(text),
        ProviderStreamEvent::completed("stop"),
    ]
}

fn write(path: &str) -> Value {
    json!({"path": path, "content": "fn main() {}\n"})
}

fn git_call(args: &[&str]) -> Value {
    json!({"executable": "git", "args": args, "timeout_ms": 60_000})
}

fn node(id: &str, after: &str, when: &str, args: &[&str]) -> Value {
    json!({"id": id, "after": after, "when": when, "tool": "run_process", "arguments": git_call(args)})
}

fn bank(nodes: &[Value]) -> Value {
    json!({"nodes": nodes})
}

fn request(bench: &Bench, session: SessionId, task: TaskId, text: &str) -> RunRequest {
    RunRequest::new(
        session,
        task,
        InputId::generate(),
        text.to_owned(),
        observe_workspace(bench.project_id.clone(), &bench.workspace).expect("observation"),
    )
    .with_tool_schemas(coding_tool_schemas())
}

fn options(bench: &Bench, approvals: ApprovalMode, limits: TurnLimits) -> TurnOptions {
    TurnOptions {
        workspace_root: bench.workspace.clone(),
        actor_id: "bank.test".to_owned(),
        approvals,
        limits,
    }
}

struct Run {
    outcome: TurnOutcome,
    seen: Vec<ProviderRequest>,
    store: Arc<SqliteStore>,
    session: SessionId,
    task: TaskId,
}

async fn run(
    bench: &Bench,
    responses: Vec<Vec<ProviderStreamEvent>>,
    bank: Option<ActionBank>,
    approvals: ApprovalMode,
    limits: TurnLimits,
) -> Run {
    let store = open_store(bench).await;
    let provider = Scripted::new(responses);
    let runtime = Arc::new(RuntimeService::new(
        Arc::clone(&store),
        provider.clone(),
        RuntimeConfig::default(),
    ));
    let driver = TurnDriver::new(
        Arc::clone(&runtime),
        ToolExecutionService::new(Arc::clone(&store)),
    );
    let driver = match bank {
        Some(bank) => driver.with_action_bank(bank),
        None => driver,
    };
    let session = SessionId::generate();
    let task = TaskId::generate();
    let outcome = driver
        .run_turn(
            request(
                bench,
                session.clone(),
                task.clone(),
                "write a module and test it",
            ),
            options(bench, approvals, limits),
            Arc::new(Silent),
            CancellationToken::new(),
        )
        .await
        .expect("turn runs");
    Run {
        outcome,
        seen: provider.seen(),
        store,
        session,
        task,
    }
}

/// The messages of the last model call that came after the first one.
fn last_messages(run: &Run) -> &[ProviderMessage] {
    &run.seen.last().expect("a model call").messages
}

fn issued(messages: &[ProviderMessage]) -> Vec<&ProviderMessage> {
    messages
        .iter()
        .filter(|message| message.harness_issued)
        .collect()
}

/// The root (a write) and a chain of two banked calls: both run with no
/// model call between them, and the model's next step sees each as a call
/// and its result, marked as run by the harness.
#[tokio::test]
async fn a_ready_banked_call_runs_without_a_model_call() {
    let bench = bench();
    let run = run(
        &bench,
        vec![
            calls(&[
                ("w1", "write_file", write("src/main.rs")),
                (
                    "b1",
                    "action_bank",
                    bank(&[
                        node("version", "root", "ok", &["--version"]),
                        node("status", "version", "exit_code: 0", &["status", "--short"]),
                    ]),
                ),
            ]),
            answer("written and checked"),
        ],
        Some(ActionBank::default()),
        ApprovalMode::Auto,
        TurnLimits::default(),
    )
    .await;
    assert_eq!(run.outcome.stop, TurnStop::Final);
    assert_eq!(run.seen.len(), 2, "two model calls for four tool calls");
    assert_eq!(run.outcome.steps, 2);
    assert_eq!(
        run.outcome.tool_calls, 4,
        "write, bank, and two banked calls"
    );
    assert_eq!(run.outcome.bank.authored, 1);
    assert_eq!(run.outcome.bank.dispatched, 2);
    assert!(run.outcome.bank.author_tokens_estimate > 0);
    assert!(
        run.seen[0]
            .tool_schemas
            .iter()
            .any(|schema| schema["function"]["name"] == "action_bank"),
        "the bank tool is offered while the bank is on"
    );
    let messages = last_messages(&run);
    harness_providers::validate_transcript(messages).expect("valid transcript");
    let issued = issued(messages);
    assert_eq!(issued.len(), 2, "{messages:#?}");
    assert_eq!(issued[0].tool_calls[0].name, "run_process");
    assert!(issued[0].tool_calls[0].arguments.contains("--version"));
    assert!(issued[0].reasoning.is_none());
    let results = messages
        .iter()
        .filter(|message| {
            message.role == MessageRole::Tool
                && message
                    .tool_call_id
                    .as_deref()
                    .is_some_and(|id| id.starts_with("bank_"))
        })
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 2);
    assert!(
        results[0]
            .content
            .starts_with("[action bank node version: run by the harness"),
        "{}",
        results[0].content
    );
    assert!(
        results[0].content.contains("git version"),
        "{}",
        results[0].content
    );
    let kept = messages
        .iter()
        .find(|message| message.tool_call_id.as_deref() == Some("b1"))
        .expect("the bank call is answered");
    assert!(kept.content.starts_with("action bank "), "{}", kept.content);
}

/// A bank authored alone in its response has an empty root: its first node
/// runs at once, where the model's own call would have, and saves no model
/// step; the node after it does.
#[tokio::test]
async fn a_bank_authored_alone_saves_no_step_with_its_first_call() {
    let bench = bench();
    let run = run(
        &bench,
        vec![
            calls(&[(
                "b1",
                "action_bank",
                bank(&[
                    node("version", "root", "ok", &["--version"]),
                    node("status", "version", "ok", &["status", "--short"]),
                ]),
            )]),
            answer("done"),
        ],
        Some(ActionBank::default()),
        ApprovalMode::Auto,
        TurnLimits::default(),
    )
    .await;
    assert_eq!(run.seen.len(), 2);
    assert_eq!(
        run.outcome.bank.immediate, 1,
        "the first node is the response's call"
    );
    assert_eq!(
        run.outcome.bank.dispatched, 1,
        "the second saves a model step"
    );
}

/// A later turn and a resume rebuild the conversation from the store: the
/// banked calls are there as calls with their results, and the bank call
/// with its answer, so no receipt is orphaned and the transcript is valid.
#[tokio::test]
async fn banked_calls_stay_in_the_conversation_for_later_turns() {
    let bench = bench();
    let first = run(
        &bench,
        vec![
            calls(&[
                ("w1", "write_file", write("src/main.rs")),
                (
                    "b1",
                    "action_bank",
                    bank(&[node("version", "root", "ok", &["--version"])]),
                ),
            ]),
            answer("done"),
        ],
        Some(ActionBank::default()),
        ApprovalMode::Auto,
        TurnLimits::default(),
    )
    .await;
    assert_eq!(first.outcome.bank.dispatched, 1);
    let history = harness_runtime::conversation_history(&first.store, &first.session)
        .await
        .expect("history reads")
        .messages;
    harness_providers::validate_transcript(&history).expect("valid replay");
    let issued = issued(&history);
    assert_eq!(issued.len(), 1, "{history:#?}");
    let call_id = issued[0].tool_calls[0].call_id.clone();
    let result = history
        .iter()
        .find(|message| message.tool_call_id.as_deref() == Some(call_id.as_str()))
        .expect("the banked call's result is replayed");
    assert!(result.content.contains("git version"), "{}", result.content);
    assert!(result.content.starts_with("[action bank node version"));
    let bank_answer = history
        .iter()
        .find(|message| message.tool_call_id.as_deref() == Some("b1"))
        .expect("the bank call is answered in the replay");
    assert!(
        bank_answer.content.starts_with("action bank "),
        "{}",
        bank_answer.content
    );
    // The banked call comes after the root's results and before the answer.
    let position = |id: &str| {
        history
            .iter()
            .position(|message| message.tool_call_id.as_deref() == Some(id))
            .expect("result")
    };
    assert!(position("w1") < position(&call_id));

    // The next turn of the conversation, from a store opened again as a
    // resume opens it, is sent the banked call too.
    let Run {
        store,
        session,
        task,
        ..
    } = first;
    Arc::try_unwrap(store)
        .expect("store released")
        .close()
        .await
        .expect("store closes");
    let store = open_store(&bench).await;
    let provider = Scripted::new(vec![answer("the version was shown")]);
    let runtime = Arc::new(RuntimeService::new(
        Arc::clone(&store),
        provider.clone(),
        RuntimeConfig::default(),
    ));
    let driver = TurnDriver::new(
        Arc::clone(&runtime),
        ToolExecutionService::new(Arc::clone(&store)),
    )
    .with_action_bank(ActionBank::default());
    let outcome = driver
        .run_turn_continuing(
            &session,
            request(&bench, SessionId::generate(), task, "what did git say?"),
            options(&bench, ApprovalMode::Auto, TurnLimits::default()),
            Arc::new(Silent),
            CancellationToken::new(),
        )
        .await
        .expect("the follow-up runs");
    assert_eq!(outcome.stop, TurnStop::Final);
    let sent = &provider.seen()[0].messages;
    harness_providers::validate_transcript(sent).expect("valid follow-up");
    assert!(
        sent.iter()
            .any(|message| message.harness_issued && message.tool_calls[0].call_id == call_id),
        "{sent:#?}"
    );
}

/// A guard that does not hold returns to the model: the failing call ran
/// from the bank, its follower did not, and the model reads the failure.
#[tokio::test]
async fn a_guard_that_does_not_hold_calls_the_model() {
    let bench = bench();
    let run = run(
        &bench,
        vec![
            calls(&[
                ("w1", "write_file", write("src/main.rs")),
                (
                    "b1",
                    "action_bank",
                    bank(&[
                        node("bad", "root", "ok", &["no-such-subcommand-for-bank"]),
                        node("next", "bad", "ok", &["--version"]),
                    ]),
                ),
            ]),
            answer("the command failed"),
        ],
        Some(ActionBank::default()),
        ApprovalMode::Auto,
        TurnLimits::default(),
    )
    .await;
    assert_eq!(run.seen.len(), 2);
    assert_eq!(run.outcome.bank.dispatched, 1, "only the failing call ran");
    assert_eq!(run.outcome.tool_calls, 3);
    assert!(run.outcome.bank.discarded >= 1);
    let messages = last_messages(&run);
    assert_eq!(issued(messages).len(), 1);
    assert!(
        !messages.iter().any(|message| message
            .tool_calls
            .iter()
            .any(|call| call.name == "run_process" && call.arguments.contains("--version"))),
        "the node after the failure did not run"
    );
}

/// A `failed` guard is the author's own branch: it runs when the call failed.
#[tokio::test]
async fn a_failed_guard_runs_after_a_failure() {
    let bench = bench();
    let run = run(
        &bench,
        vec![
            calls(&[
                (
                    "r1",
                    "run_process",
                    git_call(&["no-such-subcommand-for-bank"]),
                ),
                (
                    "b1",
                    "action_bank",
                    bank(&[
                        node("ok_path", "root", "ok", &["status"]),
                        node("diagnose", "root", "failed", &["--version"]),
                    ]),
                ),
            ]),
            answer("diagnosed"),
        ],
        Some(ActionBank::default()),
        ApprovalMode::Auto,
        TurnLimits::default(),
    )
    .await;
    assert_eq!(run.seen.len(), 2);
    assert_eq!(run.outcome.bank.dispatched, 1);
    let issued = issued(last_messages(&run));
    assert!(issued[0].tool_calls[0].arguments.contains("--version"));
}

struct Picks(&'static str, AtomicUsize);

impl BankSelector for Picks {
    fn select<'a>(
        &'a self,
        choice: BankChoice,
    ) -> Pin<Box<dyn Future<Output = BankSelection> + Send + 'a>> {
        self.1.fetch_add(1, Ordering::SeqCst);
        assert_eq!(choice.candidates.len(), 2);
        let pick = self.0;
        Box::pin(async move {
            BankSelection {
                node: (!pick.is_empty()).then(|| pick.to_owned()),
                input_tokens: 42,
                asked: true,
            }
        })
    }
}

fn two_ready() -> Vec<Vec<ProviderStreamEvent>> {
    vec![
        calls(&[
            ("w1", "write_file", write("src/main.rs")),
            (
                "b1",
                "action_bank",
                bank(&[
                    node("a", "root", "ok", &["--version"]),
                    node("b", "root", "ok", &["status", "--short"]),
                ]),
            ),
        ]),
        answer("done"),
    ]
}

/// Several ready nodes go to the selector; without one, or when it abstains,
/// the model is asked. Its pick runs with that node's own arguments.
#[tokio::test]
async fn several_ready_nodes_go_to_the_selector_or_to_the_model() {
    let benches = [bench(), bench(), bench()];
    let without = run(
        &benches[0],
        two_ready(),
        Some(ActionBank::default()),
        ApprovalMode::Auto,
        TurnLimits::default(),
    )
    .await;
    assert_eq!(without.seen.len(), 2);
    assert_eq!(without.outcome.bank.dispatched, 0);
    assert_eq!(without.outcome.bank.abstained, 1);

    let abstains = Arc::new(Picks("", AtomicUsize::new(0)));
    let abstained = run(
        &benches[1],
        two_ready(),
        Some(ActionBank {
            selector: Some(abstains.clone()),
            timeout: None,
        }),
        ApprovalMode::Auto,
        TurnLimits::default(),
    )
    .await;
    assert_eq!(abstains.1.load(Ordering::SeqCst), 1);
    assert_eq!(abstained.outcome.bank.dispatched, 0);
    assert_eq!(abstained.outcome.bank.selector_calls, 1);
    assert_eq!(abstained.outcome.bank.selector_input_tokens, 42);

    let picks = Arc::new(Picks("b", AtomicUsize::new(0)));
    let picked = run(
        &benches[2],
        two_ready(),
        Some(ActionBank {
            selector: Some(picks.clone()),
            timeout: None,
        }),
        ApprovalMode::Auto,
        TurnLimits::default(),
    )
    .await;
    assert_eq!(picked.outcome.bank.dispatched, 1);
    assert_eq!(picked.seen.len(), 2);
    let issued = issued(last_messages(&picked));
    assert!(issued[0].tool_calls[0].arguments.contains("--short"));
}

/// A bank that cannot run as written is refused with its reason and nothing
/// is kept.
#[tokio::test]
async fn an_invalid_bank_is_refused_and_nothing_runs() {
    let bench = bench();
    let run = run(
        &bench,
        vec![
            calls(&[
                ("w1", "write_file", write("src/main.rs")),
                (
                    "b1",
                    "action_bank",
                    bank(&[json!({"id": "x", "after": "root", "when": "ok", "tool": "run_process", "arguments": {"cmd": "git"}})]),
                ),
            ]),
            answer("ok"),
        ],
        Some(ActionBank::default()),
        ApprovalMode::Auto,
        TurnLimits::default(),
    )
    .await;
    assert_eq!(run.outcome.bank.refused, 1);
    assert_eq!(run.outcome.bank.authored, 0);
    assert_eq!(run.outcome.bank.dispatched, 0);
    let refusal = last_messages(&run)
        .iter()
        .find(|message| message.tool_call_id.as_deref() == Some("b1"))
        .expect("answered");
    assert!(refusal.content.contains("refused"), "{}", refusal.content);
    assert!(refusal.content.contains("schema"), "{}", refusal.content);
    // A later turn's replay gives the refusal, not "this call did not run".
    let history = harness_runtime::conversation_history(&run.store, &run.session)
        .await
        .expect("history")
        .messages;
    let replayed = history
        .iter()
        .find(|message| message.tool_call_id.as_deref() == Some("b1"))
        .expect("replayed");
    assert!(
        replayed.content.contains("action_bank refused"),
        "{}",
        replayed.content
    );
}

/// A selector that runs out of time abstains, and its call is still counted.
struct Slow;

impl BankSelector for Slow {
    fn select<'a>(
        &'a self,
        _choice: BankChoice,
    ) -> Pin<Box<dyn Future<Output = BankSelection> + Send + 'a>> {
        Box::pin(async {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            BankSelection {
                node: Some("a".to_owned()),
                input_tokens: 1,
                asked: true,
            }
        })
    }
}

#[tokio::test]
async fn a_selector_that_times_out_abstains_and_is_counted() {
    let bench = bench();
    let run = run(
        &bench,
        two_ready(),
        Some(ActionBank {
            selector: Some(Arc::new(Slow)),
            timeout: Some(std::time::Duration::from_millis(50)),
        }),
        ApprovalMode::Auto,
        TurnLimits::default(),
    )
    .await;
    assert_eq!(run.outcome.bank.dispatched, 0);
    assert_eq!(run.outcome.bank.abstained, 1);
    assert_eq!(
        run.outcome.bank.selector_calls, 1,
        "the timed-out call counts"
    );
    assert_eq!(run.seen.len(), 2);
}

/// A steer the user sent while a banked call was being chosen reaches the
/// model before the call: the banked call is dropped, never run, and not
/// kept with the turn as a call the harness issued.
struct SteersWhileChoosing {
    store: Arc<SqliteStore>,
    input: InputId,
}

impl BankSelector for SteersWhileChoosing {
    fn select<'a>(
        &'a self,
        _choice: BankChoice,
    ) -> Pin<Box<dyn Future<Output = BankSelection> + Send + 'a>> {
        Box::pin(async move {
            let run = self
                .store
                .run_by_input(&self.input)
                .await
                .expect("run read")
                .expect("the run exists");
            harness_runtime::RunInbox::new(Arc::clone(&self.store))
                .steer(
                    &run,
                    "stop, do not run the next command",
                    harness_runtime::now_unix_ms(),
                )
                .await
                .expect("steer queued");
            BankSelection {
                node: Some("a".to_owned()),
                input_tokens: 0,
                asked: true,
            }
        })
    }
}

#[tokio::test]
async fn a_steer_sent_while_a_banked_call_is_chosen_drops_the_call() {
    let bench = bench();
    let store = open_store(&bench).await;
    let provider = Scripted::new(two_ready());
    let runtime = Arc::new(RuntimeService::new(
        Arc::clone(&store),
        provider.clone(),
        RuntimeConfig::default(),
    ));
    let input = InputId::generate();
    let driver = TurnDriver::new(
        Arc::clone(&runtime),
        ToolExecutionService::new(Arc::clone(&store)),
    )
    .with_inbox(harness_runtime::RunInbox::new(Arc::clone(&store)))
    .with_action_bank(ActionBank {
        selector: Some(Arc::new(SteersWhileChoosing {
            store: Arc::clone(&store),
            input: input.clone(),
        })),
        timeout: None,
    });
    let session = SessionId::generate();
    let outcome = driver
        .run_turn(
            RunRequest::new(
                session.clone(),
                TaskId::generate(),
                input,
                "write a module and test it".to_owned(),
                observe_workspace(bench.project_id.clone(), &bench.workspace).expect("observation"),
            )
            .with_tool_schemas(coding_tool_schemas()),
            options(&bench, ApprovalMode::Auto, TurnLimits::default()),
            Arc::new(Silent),
            CancellationToken::new(),
        )
        .await
        .expect("turn runs");
    let seen = provider.seen();
    assert_eq!(seen.len(), 2, "the model reads the steer");
    assert_eq!(outcome.bank.dispatched, 0, "the chosen call never ran");
    assert_eq!(outcome.tool_calls, 2, "only the write and the bank");
    let last = &seen[1].messages;
    assert!(issued(last).is_empty(), "{last:#?}");
    assert!(last.iter().any(|message| {
        message
            .content
            .contains("stop, do not run the next command")
    }));
    let history = harness_runtime::conversation_history(&store, &session)
        .await
        .expect("history")
        .messages;
    assert!(
        issued(&history).is_empty(),
        "a call that never ran is not replayed as issued: {history:#?}"
    );
}

/// The approvals of a banked call are asked as the model's own are, and a
/// denial ends the chain: the model is called.
struct DeniesProcesses(Mutex<Vec<ApprovalProposal>>);

impl ApprovalGate for DeniesProcesses {
    fn request(
        &self,
        proposal: ApprovalProposal,
    ) -> Pin<Box<dyn Future<Output = ApprovalAnswer> + Send>> {
        let process = proposal.action.contains("process") || proposal.summary.contains("git");
        self.0.lock().expect("proposals").push(proposal);
        Box::pin(async move {
            if process {
                ApprovalAnswer::Denied
            } else {
                ApprovalAnswer::Granted
            }
        })
    }
}

#[tokio::test]
async fn a_banked_call_is_approved_like_any_call_and_a_denial_stops_the_chain() {
    let bench = bench();
    let gate = Arc::new(DeniesProcesses(Mutex::new(Vec::new())));
    let run = run(
        &bench,
        vec![
            calls(&[
                ("w1", "write_file", write("src/main.rs")),
                (
                    "b1",
                    "action_bank",
                    bank(&[
                        node("version", "root", "ok", &["--version"]),
                        node("status", "version", "ok", &["status"]),
                    ]),
                ),
            ]),
            answer("the command was denied"),
        ],
        Some(ActionBank::default()),
        ApprovalMode::Ask(gate.clone()),
        TurnLimits::default(),
    )
    .await;
    let proposals = gate.0.lock().expect("proposals").clone();
    assert!(
        proposals
            .iter()
            .any(|proposal| proposal.summary.contains("--version")),
        "the banked call was put to the user: {proposals:#?}"
    );
    assert_eq!(run.seen.len(), 2, "the denial goes back to the model");
    assert_eq!(run.outcome.bank.dispatched, 1);
    assert!(
        !proposals
            .iter()
            .any(|proposal| proposal.summary.contains("status")),
        "nothing after the denial ran or was asked"
    );
}

/// The tool bound holds for banked calls: with no room left, the ready node
/// is not run and the model is asked.
#[tokio::test]
async fn the_tool_bound_holds_for_banked_calls() {
    let bench = bench();
    let run = run(
        &bench,
        vec![
            calls(&[
                ("w1", "write_file", write("src/main.rs")),
                (
                    "b1",
                    "action_bank",
                    bank(&[node("version", "root", "ok", &["--version"])]),
                ),
            ]),
            answer("stopped at the bound"),
        ],
        Some(ActionBank::default()),
        ApprovalMode::Auto,
        TurnLimits {
            max_tool_calls: 2,
            ..TurnLimits::default()
        },
    )
    .await;
    assert_eq!(run.outcome.bank.dispatched, 0);
    assert_eq!(run.outcome.tool_calls, 2);
    assert!(run.outcome.bank.discarded >= 1);
}

/// Off - the default - offers no bank tool and runs nothing from one.
#[tokio::test]
async fn with_the_bank_off_there_is_no_bank_tool() {
    let bench = bench();
    let run = run(
        &bench,
        vec![
            calls(&[
                ("w1", "write_file", write("src/main.rs")),
                (
                    "b1",
                    "action_bank",
                    bank(&[node("version", "root", "ok", &["--version"])]),
                ),
            ]),
            answer("ok"),
        ],
        None,
        ApprovalMode::Auto,
        TurnLimits::default(),
    )
    .await;
    assert!(
        !run.seen[0]
            .tool_schemas
            .iter()
            .any(|schema| schema["function"]["name"] == "action_bank")
    );
    assert_eq!(run.outcome.bank, harness_tools::BankTally::default());
    assert!(issued(last_messages(&run)).is_empty());
}

/// One more turn of `session`'s conversation, from the store opened again as
/// a later turn opens it: its outcome, what the model was sent, and the store.
async fn continue_turn(
    bench: &Bench,
    store: Arc<SqliteStore>,
    session: &SessionId,
    task: &TaskId,
    responses: Vec<Vec<ProviderStreamEvent>>,
) -> (TurnOutcome, Vec<ProviderRequest>, Arc<SqliteStore>) {
    Arc::try_unwrap(store)
        .expect("store released")
        .close()
        .await
        .expect("store closes");
    let store = open_store(bench).await;
    let provider = Scripted::new(responses);
    let runtime = Arc::new(RuntimeService::new(
        Arc::clone(&store),
        provider.clone(),
        RuntimeConfig::default(),
    ));
    let outcome = TurnDriver::new(
        Arc::clone(&runtime),
        ToolExecutionService::new(Arc::clone(&store)),
    )
    .with_action_bank(ActionBank::default())
    .run_turn_continuing(
        session,
        request(
            bench,
            SessionId::generate(),
            task.clone(),
            "fix it and test again",
        ),
        options(bench, ApprovalMode::Auto, TurnLimits::default()),
        Arc::new(Silent),
        CancellationToken::new(),
    )
    .await
    .expect("the turn runs");
    drop(runtime);
    (outcome, provider.seen(), store)
}

/// The same bank authored in two turns (fix, then test again) gives each
/// banked call an id of its own: the replay of the first turn and the
/// second turn's call never announce one id twice, and a third turn runs on
/// a valid transcript.
#[tokio::test]
async fn the_same_bank_in_two_turns_keeps_every_call_id_unique() {
    let bench = bench();
    let banked = || bank(&[node("version", "root", "ok", &["--version"])]);
    let first = run(
        &bench,
        vec![
            calls(&[
                ("w1", "write_file", write("src/main.rs")),
                ("b1", "action_bank", banked()),
            ]),
            answer("done"),
        ],
        Some(ActionBank::default()),
        ApprovalMode::Auto,
        TurnLimits::default(),
    )
    .await;
    assert_eq!(first.outcome.bank.dispatched, 1);
    let Run {
        store,
        session,
        task,
        ..
    } = first;
    let (second, sent, store) = continue_turn(
        &bench,
        store,
        &session,
        &task,
        vec![
            calls(&[
                ("w2", "write_file", write("src/lib.rs")),
                ("b2", "action_bank", banked()),
            ]),
            answer("fixed and tested"),
        ],
    )
    .await;
    assert_eq!(second.stop, TurnStop::Final);
    assert_eq!(second.bank.dispatched, 1);
    let last = &sent.last().expect("a model call").messages;
    harness_providers::validate_transcript(last).expect("the second turn's transcript is valid");
    let ids = issued(last)
        .iter()
        .map(|message| message.tool_calls[0].call_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 2, "the replayed call and the new one: {last:#?}");
    assert_ne!(ids[0], ids[1]);
    assert!(ids.iter().all(|id| id.len() <= 64));
    let (third, sent, store) = continue_turn(
        &bench,
        store,
        &second.session_id,
        &task,
        vec![answer("both runs passed")],
    )
    .await;
    assert_eq!(third.stop, TurnStop::Final);
    harness_providers::validate_transcript(&sent[0].messages)
        .expect("the third turn's transcript is valid");
    assert_eq!(issued(&sent[0].messages).len(), 2);
    drop(store);
}
