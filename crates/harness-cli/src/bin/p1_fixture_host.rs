#![forbid(unsafe_code)]

//! Disposable child-host fixture used only by P1 crash-boundary acceptance
//! tests. It never runs a model or a tool executor.

use std::{io::Read, path::PathBuf, process::ExitCode, sync::Arc};

use clap::{Parser, ValueEnum};
use harness_session::{AdmitInputRequest, RecordSyntheticReceiptRequest, SessionService};
use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
use harness_types::{
    ContentHash, HostId, InputId, P0_SCHEMA_VERSION, ProjectId, SessionId, SourceAuthority, TaskId,
    ToolExecutionId, ToolExecutionReceipt, ToolIntentState, ToolOutcomeState, WorkspaceObservation,
};

#[derive(Debug, Parser)]
#[command(name = "p1-fixture-host")]
struct Cli {
    #[arg(long)]
    data_dir: PathBuf,
    #[arg(long)]
    host_id: String,
    #[arg(long)]
    session_id: Option<String>,
    #[arg(long)]
    task_id: Option<String>,
    #[arg(long)]
    input_id: Option<String>,
    #[arg(long, value_enum)]
    mode: Mode,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Mode {
    InputAckWait,
    ReceiptAckWait,
    HoldWriter,
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
    let host_id = HostId::parse(cli.host_id).map_err(|error| error.to_string())?;
    let store = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(cli.data_dir, host_id))
            .await
            .map_err(|error| error.to_string())?,
    );
    if matches!(cli.mode, Mode::HoldWriter) {
        let generation = store.fence().map_err(|error| error.to_string())?.generation;
        println!(
            "{}",
            serde_json::json!({"ready":"writer","generation":generation})
        );
        wait_for_parent_termination();
        return Ok(());
    }

    let session_id = SessionId::parse(cli.session_id.ok_or("session_id is required")?)
        .map_err(|error| error.to_string())?;
    let task_id = TaskId::parse(cli.task_id.ok_or("task_id is required")?)
        .map_err(|error| error.to_string())?;
    let input_id = InputId::parse(cli.input_id.ok_or("input_id is required")?)
        .map_err(|error| error.to_string())?;
    let service = SessionService::new(Arc::clone(&store));
    let admission = service
        .admit_input(AdmitInputRequest {
            session_id: session_id.clone(),
            task_id: task_id.clone(),
            input_id,
            expected_sequence: 1,
            authority: SourceAuthority::User,
            raw_text: "recover this durable instruction".to_owned(),
            workspace: fixture_workspace(),
            initial_plan_items: Vec::new(),
        })
        .await
        .map_err(|error| error.to_string())?;

    match cli.mode {
        Mode::InputAckWait => {
            println!(
                "{}",
                serde_json::json!({"ack":"input","sequence":admission.sequence})
            );
        }
        Mode::ReceiptAckWait => {
            service
                .write_snapshot(&session_id)
                .await
                .map_err(|error| error.to_string())?;
            let receipt = ToolExecutionReceipt {
                schema_version: P0_SCHEMA_VERSION,
                tool_execution_id: ToolExecutionId::generate(),
                task_id,
                invocation_id: "p1-fixture-receipt".to_owned(),
                input_hash: ContentHash::from_bytes(b"fixture input"),
                policy_revision: 1,
                approval_id: None,
                intent_state: ToolIntentState::IntentRecorded,
                outcome_state: ToolOutcomeState::Settled,
                before_fingerprint: None,
                after_fingerprint: None,
                artifact_id: None,
                observed_at_seq: 2,
            };
            let receipt_ack = service
                .record_synthetic_receipt(RecordSyntheticReceiptRequest {
                    session_id,
                    task_id: receipt.task_id.clone(),
                    expected_sequence: 2,
                    receipt,
                    artifact: None,
                })
                .await
                .map_err(|error| error.to_string())?;
            println!(
                "{}",
                serde_json::json!({"ack":"receipt","sequence":receipt_ack.sequence})
            );
        }
        Mode::HoldWriter => unreachable!("handled before session parsing"),
    }
    wait_for_parent_termination();
    Ok(())
}

fn fixture_workspace() -> WorkspaceObservation {
    WorkspaceObservation {
        project_id: ProjectId::generate(),
        worktree_id: "p1-fixture".to_owned(),
        base_commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        observed_fingerprint: ContentHash::from_bytes(b"p1 fixture workspace"),
    }
}

fn wait_for_parent_termination() {
    let mut byte = [0_u8; 1];
    let _ = std::io::stdin().read(&mut byte);
}
