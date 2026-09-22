#![forbid(unsafe_code)]

//! Disposable child host for the M5 compaction and history acceptance tests.
//!
//! It runs real compactions against a real store and then parks at a barrier,
//! so the parent test can hard-kill it and reopen the same data directory. The
//! summarizer is the one fixture the plan calls for: a summary that deliberately
//! drops the exact identifier the source carries.
//!
//! It never runs a model.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, ValueEnum};
use harness_providers::{
    CancellationToken, ModelCapabilities, ModelProvider, ProviderFuture, ProviderRequest,
    ProviderStreamEvent,
};
use harness_runtime::{RuntimeConfig, RuntimeService, SummaryProvider};
use harness_session::{AdmitInputRequest, RecoveryView, SessionService};
use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
use harness_types::{HostId, InputId, ProjectId, SessionId, SourceAuthority, TaskId};

/// The identifier the source carries and the summary must not keep. The test
/// recovers it through `history_search`/`history_read` or not at all.
const EXACT_IDENTIFIER: &str = "XYZ-731";

#[derive(Debug, Parser)]
#[command(name = "m5-fixture-host")]
struct Cli {
    #[arg(long)]
    data_dir: PathBuf,
    #[arg(long)]
    workspace: PathBuf,
    #[arg(long)]
    project_id: String,
    #[arg(long, value_enum)]
    mode: Mode,
    /// File the host writes once its compactions are done.
    #[arg(long)]
    barrier: Option<PathBuf>,
    /// How many compactions to publish before parking.
    #[arg(long, default_value_t = 5)]
    compactions: u32,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Mode {
    /// Admit one constraint-bearing input, publish N compactions, then park.
    CompactLoop,
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

async fn run(cli: Cli) -> Result<(), String> {
    match cli.mode {
        Mode::CompactLoop => compact_loop(cli).await,
    }
}

async fn compact_loop(cli: Cli) -> Result<(), String> {
    let project_id = ProjectId::parse(cli.project_id.clone()).map_err(|error| error.to_string())?;
    let barrier = cli.barrier.clone().ok_or("compact-loop needs --barrier")?;
    let store = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(
            cli.data_dir.clone(),
            HostId::generate(),
        ))
        .await
        .map_err(|error| error.to_string())?,
    );
    let session_id = SessionId::generate();
    let task_id = TaskId::generate();
    let sessions = SessionService::new(Arc::clone(&store));
    // The constraint is what compaction must never lose; the identifier lives in
    // a *different* source, so recovering it afterwards proves the model read
    // the journal rather than remembering the instruction.
    let text = "constraint: apply the release marker exactly as written in the ops note".to_owned();
    sessions
        .admit_input(AdmitInputRequest {
            session_id: session_id.clone(),
            task_id: task_id.clone(),
            input_id: InputId::generate(),
            expected_sequence: 1,
            authority: SourceAuthority::User,
            raw_text: text,
            workspace: harness_tools::observe_workspace(project_id, &cli.workspace)
                .map_err(|error| error.to_string())?,
            initial_plan_items: Vec::new(),
        })
        .await
        .map_err(|error| error.to_string())?;
    let mut note = serde_json::Map::new();
    note.insert(
        "text".to_owned(),
        serde_json::Value::String(format!(
            "ops note: the release marker for this build is {EXACT_IDENTIFIER}"
        )),
    );
    sessions
        .append_runtime_event(&session_id, &task_id, "ops.note", note, false)
        .await
        .map_err(|error| error.to_string())?;
    let runtime = RuntimeService::new(
        Arc::clone(&store),
        Arc::new(MockProvider),
        RuntimeConfig::default(),
    )
    .with_summarizer(Arc::new(IdentifierDroppingSummary));
    // Each compaction must cover a newer sequence than the last one, so the
    // loop records a small durable change between rounds exactly as a real
    // session would between two compactions.
    for round in 0..cli.compactions {
        let mut payload = serde_json::Map::new();
        payload.insert("round".to_owned(), serde_json::Value::Number(round.into()));
        payload.insert(
            "text".to_owned(),
            serde_json::Value::String(format!("progress round {round}")),
        );
        sessions
            .append_runtime_event(&session_id, &task_id, "compaction.round", payload, false)
            .await
            .map_err(|error| error.to_string())?;
        runtime
            .compact(&session_id)
            .await
            .map_err(|error| error.to_string())?;
    }
    std::fs::write(&barrier, format!("{session_id}\n{task_id}\n"))
        .map_err(|error| error.to_string())?;
    // Park so the parent can kill this process between the checkpoints and the
    // next step, the way M4's crash boundaries do.
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// A summary that drops the exact identifier on purpose.
struct IdentifierDroppingSummary;

impl SummaryProvider for IdentifierDroppingSummary {
    fn summarize(&self, recovery: &RecoveryView) -> Result<String, harness_runtime::RuntimeError> {
        Ok(format!(
            "the task is still in progress; {} instruction(s) remain in force",
            recovery.instruction_texts.len()
        ))
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct MockProvider;

impl ModelProvider for MockProvider {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::deepseek_fixture()
    }

    fn stream(&self, request: ProviderRequest, _cancellation: CancellationToken) -> ProviderFuture {
        Box::pin(async move {
            let mut events = vec![
                ProviderStreamEvent::started(),
                ProviderStreamEvent::text("the fixture host never answers"),
                ProviderStreamEvent::completed("stop"),
            ];
            if let Some(ProviderStreamEvent::Started { request_id }) = events.first_mut() {
                *request_id = request.request_id;
            }
            Ok(events)
        })
    }
}
