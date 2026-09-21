#![forbid(unsafe_code)]

//! Disposable child-host fixture for the M4 crash-boundary acceptance tests.
//!
//! It runs a real store, session service and tool gate in a separate process so
//! the parent test can hard-kill it at an exact boundary:
//!
//!   * `receipt-barrier` settles one patch and then parks inside the turn
//!     observer (after the receipt commit, before the next step);
//!   * `marker-process` starts a process that writes a marker and then sleeps,
//!     and the parent kills it while the side effect exists but no receipt does.
//!
//! It never runs a model: the provider is a deterministic sequence fixture.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use clap::{Parser, ValueEnum};
use harness_providers::{
    CancellationToken, ModelCapabilities, ModelProvider, ProviderFuture, ProviderRequest,
    ProviderStreamEvent,
};
use harness_runtime::{RunRequest, RuntimeConfig, RuntimeService};
use harness_session::{AdmitInputRequest, SessionService};
use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
use harness_tools::{
    ApprovalMode, CodingToolAction, IsolationMode, ToolExecutionService, ToolRequest, TurnDriver,
    TurnLimits, TurnObserver, TurnOptions, TurnProgress, coding_tool_schemas, observe_workspace,
    observed_file_hash,
};
use harness_types::{HostId, InputId, ProjectId, SessionId, SourceAuthority, TaskId};

#[derive(Debug, Parser)]
#[command(name = "m4-fixture-host")]
struct Cli {
    #[arg(long)]
    data_dir: PathBuf,
    #[arg(long)]
    workspace: PathBuf,
    #[arg(long)]
    project_id: String,
    #[arg(long, value_enum)]
    mode: Mode,
    /// Barrier file the parent waits for before it kills this process.
    #[arg(long)]
    barrier: Option<PathBuf>,
    /// Marker file the fixture process writes before holding.
    #[arg(long)]
    marker: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Mode {
    /// Settle one patch, then park after the receipt commit.
    ReceiptBarrier,
    /// Run a process that writes a marker and holds; no receipt is committed.
    MarkerProcess,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}

#[allow(clippy::too_many_lines)] // one fixture with two crash boundaries
async fn run(cli: Cli) -> Result<(), String> {
    let project_id = ProjectId::parse(cli.project_id.clone()).map_err(|error| error.to_string())?;
    let store = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(cli.data_dir, HostId::generate()))
            .await
            .map_err(|error| error.to_string())?,
    );
    let session_id = SessionId::generate();
    let task_id = TaskId::generate();

    match cli.mode {
        Mode::ReceiptBarrier => {
            let barrier = cli.barrier.ok_or("receipt-barrier needs --barrier")?;
            let expected_hash = observed_file_hash(&cli.workspace, "src/parser.txt")
                .map_err(|error| error.to_string())?;
            let patch = serde_json::json!({
                "path": "src/parser.txt",
                "expected_hash": expected_hash,
                "replacement": "FIXED parser\r\n",
            });
            let provider = Arc::new(SequenceProvider::new(vec![
                vec![
                    ProviderStreamEvent::started(),
                    ProviderStreamEvent::tool_delta("call-1", "apply_patch", patch.to_string()),
                    ProviderStreamEvent::completed("tool_calls"),
                ],
                vec![
                    ProviderStreamEvent::started(),
                    ProviderStreamEvent::text("patched"),
                    ProviderStreamEvent::completed("stop"),
                ],
            ]));
            let runtime = Arc::new(RuntimeService::new(
                Arc::clone(&store),
                provider,
                RuntimeConfig::default(),
            ));
            let driver = TurnDriver::new(runtime, ToolExecutionService::new(Arc::clone(&store)));
            let request = RunRequest::new(
                session_id,
                task_id,
                InputId::generate(),
                "patch the fixture".to_owned(),
                observe_workspace(project_id, &cli.workspace).map_err(|error| error.to_string())?,
            )
            .with_tool_schemas(coding_tool_schemas());
            // The observer parks the worker after the settled receipt; the
            // parent kills the process at the barrier file.
            let observer: Arc<dyn TurnObserver> = Arc::new(BarrierObserver {
                barrier,
                armed: AtomicBool::new(false),
            });
            driver
                .run_turn(
                    request,
                    TurnOptions {
                        workspace_root: cli.workspace,
                        actor_id: "m4.fixture".to_owned(),
                        approvals: ApprovalMode::Auto,
                        limits: TurnLimits::default(),
                    },
                    observer,
                    CancellationToken::new(),
                )
                .await
                .map_err(|error| error.to_string())?;
        }
        Mode::MarkerProcess => {
            let marker = cli.marker.ok_or("marker-process needs --marker")?;
            // This mode drives the tool service directly, so the input is
            // admitted here; the receipt-barrier mode lets the runtime admit.
            SessionService::new(Arc::clone(&store))
                .admit_input(AdmitInputRequest {
                    session_id: session_id.clone(),
                    task_id: task_id.clone(),
                    input_id: InputId::generate(),
                    expected_sequence: 1,
                    authority: SourceAuthority::User,
                    raw_text: "m4 fixture".to_owned(),
                    workspace: observe_workspace(project_id, &cli.workspace)
                        .map_err(|error| error.to_string())?,
                    initial_plan_items: Vec::new(),
                })
                .await
                .map_err(|error| error.to_string())?;
            let (executable, args) = marker_command(&marker);
            let tools = ToolExecutionService::new(Arc::clone(&store));
            let prepared = tools
                .prepare(ToolRequest::new(
                    session_id,
                    task_id,
                    "m4.fixture",
                    &cli.workspace,
                    CodingToolAction::RunProcess {
                        executable,
                        args,
                        timeout_ms: 120_000,
                        isolation: IsolationMode::BestEffort,
                    },
                ))
                .await
                .map_err(|error| error.to_string())?;
            let approval = tools
                .approve(&prepared)
                .await
                .map_err(|error| error.to_string())?;
            // The process writes the marker and then sleeps; the parent kills
            // this host while the side effect exists and no receipt does.
            tools
                .execute(prepared, Some(approval))
                .await
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

struct BarrierObserver {
    barrier: PathBuf,
    armed: AtomicBool,
}

impl TurnObserver for BarrierObserver {
    fn observe(&self, progress: TurnProgress) {
        if let TurnProgress::ToolSettled { ok: true, .. } = progress
            && !self.armed.swap(true, Ordering::SeqCst)
        {
            // The receipt is committed before this observer runs; park here so
            // the parent can kill the process between receipt and next step.
            let _ = std::fs::write(&self.barrier, "settled");
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

struct SequenceProvider {
    responses: Vec<Vec<ProviderStreamEvent>>,
    calls: AtomicUsize,
}

impl SequenceProvider {
    fn new(responses: Vec<Vec<ProviderStreamEvent>>) -> Self {
        assert!(!responses.is_empty());
        Self {
            responses,
            calls: AtomicUsize::new(0),
        }
    }
}

impl ModelProvider for SequenceProvider {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::deepseek_fixture()
    }

    fn stream(&self, request: ProviderRequest, _cancellation: CancellationToken) -> ProviderFuture {
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

#[cfg(windows)]
fn marker_command(marker: &std::path::Path) -> (String, Vec<String>) {
    (
        "pwsh".to_owned(),
        vec![
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-Command".to_owned(),
            format!(
                "Add-Content -LiteralPath '{}' -Value x; Start-Sleep -Seconds 60",
                marker.display()
            ),
        ],
    )
}

#[cfg(not(windows))]
fn marker_command(marker: &std::path::Path) -> (String, Vec<String>) {
    (
        "sh".to_owned(),
        vec![
            "-c".to_owned(),
            format!("echo x >> '{}'; sleep 60", marker.display()),
        ],
    )
}
