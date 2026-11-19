//! Codemode in the real turn loop: a program's calls go through the tool
//! gate as the model's own, their results stay out of the conversation, and
//! only what the program prints and returns reaches the model.
//!
//! Everything is real except the model: a `SQLite` store, the gate (policy,
//! approval, intent, receipt) and a workspace on disk; a scripted provider
//! records every request it gets.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use harness_providers::{
    CancellationToken, ModelCapabilities, ModelProvider, ProviderFuture, ProviderRequest,
    ProviderStreamEvent,
};
use harness_runtime::{RunRequest, RuntimeConfig, RuntimeService};
use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
use harness_tools::{
    ApprovalMode, Codemode, ToolExecutionService, TurnDriver, TurnLimits, TurnObserver,
    TurnOptions, TurnOutcome, TurnProgress, coding_tool_schemas, observe_workspace,
};
use harness_types::{HostId, InputId, ProjectId, SessionId, TaskId};
use serde_json::{Value, json};

/// A line only the file holds: it must never reach the model.
const SECRET_LINE: &str = "only-in-the-file-7f3a";

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
    std::fs::write(
        workspace.join("src/a.rs"),
        format!("fn a() {{}}\n// {SECRET_LINE}\n"),
    )
    .expect("a");
    std::fs::write(
        workspace.join("src/b.rs"),
        "fn b() {}\nfn c() {}\nfn d() {}\n",
    )
    .expect("b");
    Bench {
        data_dir: temp.path().join("data"),
        _temp: temp,
        workspace,
        project_id: ProjectId::generate(),
    }
}

struct Scripted {
    responses: Vec<Vec<ProviderStreamEvent>>,
    calls: AtomicUsize,
    seen: Mutex<Vec<ProviderRequest>>,
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

/// Every card the user is shown.
#[derive(Default)]
struct Cards(Mutex<Vec<(String, String)>>);

impl TurnObserver for Cards {
    fn observe(&self, progress: TurnProgress) {
        if let TurnProgress::ToolStarted { name, call_id, .. } = progress {
            self.0.lock().expect("cards").push((name, call_id));
        }
    }
}

fn program(code: &str) -> Vec<ProviderStreamEvent> {
    vec![
        ProviderStreamEvent::started(),
        ProviderStreamEvent::tool_delta("cm1", "codemode", json!({ "code": code }).to_string()),
        ProviderStreamEvent::completed("tool_calls"),
    ]
}

fn answer(text: &str) -> Vec<ProviderStreamEvent> {
    vec![
        ProviderStreamEvent::started(),
        ProviderStreamEvent::text(text),
        ProviderStreamEvent::completed("stop"),
    ]
}

struct Run {
    outcome: TurnOutcome,
    seen: Vec<ProviderRequest>,
    cards: Vec<(String, String)>,
}

async fn run(bench: &Bench, code: &str, approvals: ApprovalMode, codemode: bool) -> Run {
    let store = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(
            bench.data_dir.clone(),
            HostId::generate(),
        ))
        .await
        .expect("store opens"),
    );
    let provider = Arc::new(Scripted {
        responses: vec![program(code), answer("done")],
        calls: AtomicUsize::new(0),
        seen: Mutex::new(Vec::new()),
    });
    let runtime = Arc::new(RuntimeService::new(
        Arc::clone(&store),
        provider.clone(),
        // The default window is 8K tokens: every tool's schema is sent with
        // the request, and two more tools (git_search, git_show) left no room
        // for a program's result, which was then cut from the next request.
        // A model ha drives has a window many times this.
        RuntimeConfig {
            context_window_tokens: 32_768,
            ..RuntimeConfig::default()
        },
    ));
    let driver = TurnDriver::new(
        Arc::clone(&runtime),
        ToolExecutionService::new(Arc::clone(&store)),
    );
    let driver = if codemode {
        driver.with_codemode(Codemode::default())
    } else {
        driver
    };
    let request = RunRequest::new(
        SessionId::generate(),
        TaskId::generate(),
        InputId::generate(),
        "count the functions in src".to_owned(),
        observe_workspace(bench.project_id.clone(), &bench.workspace).expect("observation"),
    )
    .with_tool_schemas(coding_tool_schemas());
    let cards = Arc::new(Cards::default());
    let outcome = driver
        .run_turn(
            request,
            TurnOptions {
                workspace_root: bench.workspace.clone(),
                actor_id: "codemode.test".to_owned(),
                approvals,
                limits: TurnLimits::default(),
            },
            cards.clone(),
            CancellationToken::new(),
        )
        .await
        .expect("turn runs");
    let seen = provider.seen.lock().expect("requests").clone();
    let cards = cards.0.lock().expect("cards").clone();
    Run {
        outcome,
        seen,
        cards,
    }
}

/// The text the model read for the `codemode` call.
fn codemode_result(run: &Run) -> String {
    let last = run.seen.last().expect("a second model call");
    last.messages
        .iter()
        .find(|message| message.tool_call_id.as_deref() == Some("cm1"))
        .map(|message| message.content.clone())
        .expect("the codemode result")
}

fn tool_names(request: &ProviderRequest) -> Vec<String> {
    request
        .tool_schemas
        .iter()
        .filter_map(|tool| {
            tool.pointer("/function/name")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

#[tokio::test]
async fn the_programs_calls_run_through_the_gate_and_only_its_output_reaches_the_model() {
    let bench = bench();
    let run = run(
        &bench,
        "const paths = ['src/a.rs', 'src/b.rs'];\n\
         const files = await Promise.all(paths.map(path => tools.read_file({ path })));\n\
         const counts = files.map(text => (text.match(/^fn /gm) || []).length);\n\
         text(`functions: ${counts.join(', ')}`);\n\
         return counts.reduce((a, b) => a + b, 0);",
        ApprovalMode::Auto,
        true,
    )
    .await;
    assert!(tool_names(&run.seen[0]).contains(&"codemode".to_owned()));
    let result = codemode_result(&run);
    assert_eq!(result, "functions: 1, 3\n=> 4");
    // What the files hold never entered the conversation.
    for request in &run.seen {
        for message in request.messages.iter() {
            assert!(
                !message.content.contains(SECRET_LINE),
                "{}",
                message.content
            );
        }
    }
    // Both reads are the turn's executions, each with its own card.
    let reads = run
        .outcome
        .executions
        .iter()
        .filter(|view| matches!(view.output, harness_tools::ToolOutput::ReadFile { .. }))
        .count();
    assert_eq!(reads, 2);
    let nested: Vec<&str> = run
        .cards
        .iter()
        .filter(|(name, _)| name == "read_file")
        .map(|(_, id)| id.as_str())
        .collect();
    assert_eq!(nested, vec!["cm1.1", "cm1.2"]);
}

#[tokio::test]
async fn a_call_the_gate_refuses_is_an_error_the_program_can_catch() {
    let bench = bench();
    let run = run(
        &bench,
        "try {\n  await tools.write_file({ path: 'src/new.rs', content: 'fn x() {}\\n' });\n  text('written');\n} catch (error) {\n  text('refused: ' + error.message);\n}",
        ApprovalMode::None,
        true,
    )
    .await;
    let result = codemode_result(&run);
    assert!(result.starts_with("refused: "), "{result}");
    assert!(!bench.workspace.join("src/new.rs").exists());
}

#[tokio::test]
async fn arguments_are_checked_and_control_tools_are_not_reachable() {
    let bench = bench();
    let run = run(
        &bench,
        "const errors = [];\n\
         for (const attempt of [() => tools.read_file({}), () => tools.codemode({ code: '1' }), () => tools.nope({})]) {\n\
           try { await attempt() } catch (error) { errors.push(error.message) }\n\
         }\n\
         return errors;",
        ApprovalMode::Auto,
        true,
    )
    .await;
    let result = codemode_result(&run);
    assert!(
        result.contains("path"),
        "a missing argument is named: {result}"
    );
    assert!(result.contains("tools.codemode does not exist"), "{result}");
    assert!(result.contains("tools.nope does not exist"), "{result}");
}

#[tokio::test]
async fn a_program_that_fails_reports_its_error_and_its_output_so_far() {
    let bench = bench();
    let run = run(
        &bench,
        "text('started');\nthrow new Error('stop here');",
        ApprovalMode::Auto,
        true,
    )
    .await;
    let result = codemode_result(&run);
    assert!(
        result.starts_with("started\nScript error: Error: stop here"),
        "{result}"
    );
    assert!(result.contains("codemode.js:2"), "{result}");
}

#[tokio::test]
async fn codemode_is_offered_only_when_it_is_on() {
    let bench = bench();
    let run = run(&bench, "return 1", ApprovalMode::Auto, false).await;
    assert!(!tool_names(&run.seen[0]).contains(&"codemode".to_owned()));
}

#[tokio::test]
async fn a_program_rewrites_a_file_it_read_and_writes_it_twice() {
    let bench = bench();
    let run = run(
        &bench,
        "const text = await tools.read_file({ path: 'src/b.rs' });\n\
         await tools.write_file({ path: 'src/b.rs', content: text.replace('fn b()', 'fn renamed()') });\n\
         await tools.write_file({ path: 'src/b.rs', content: text.replace('fn b()', 'fn twice()') });\n\
         return 'ok';",
        ApprovalMode::Auto,
        true,
    )
    .await;
    let result = codemode_result(&run);
    assert_eq!(result, "=> \"ok\"", "{result}");
    let written = std::fs::read_to_string(bench.workspace.join("src/b.rs")).expect("b.rs");
    assert!(written.starts_with("fn twice() {}"), "{written}");
}
