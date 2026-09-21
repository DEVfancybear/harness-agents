#![forbid(unsafe_code)]

//! Disposable child fixture for milestone crash-boundary and failpoint tests.
//!
//! Protocol, in order, on stdout as one JSON object per line:
//!
//! 1. `{"ready":"m0_fixture_host","pid":<n>,"generation":<n>}` after the writer
//!    lock is held, so the parent knows the durable store is open;
//! 2. `{"ack":"input","sequence":<n>,"event_id":"<id>"}` when `--mode admit`
//!    committed an input;
//! 3. `{"failpoint":"<name>","marker":"<path>"}` immediately before a
//!    `--mode failpoint` process exits with code 86.
//!
//! `--mode hold` never prints a second line: it waits until the parent kills it,
//! which is how a crash boundary is simulated without destructors running.
//!
//! The fixture never calls a model or a tool executor.

use std::{io::Read, path::PathBuf, process::ExitCode, sync::Arc};

use clap::{Parser, ValueEnum};
use harness_session::{AdmitInputRequest, SessionService};
use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
use harness_types::{
    ContentHash, FixedIdSource, HostId, IdSource, InputId, ProjectId, SessionId, SourceAuthority,
    TaskId, WorkspaceObservation,
};

/// Exit code a hit failpoint uses. It is deliberately not 0 or 1 so a parent
/// cannot mistake it for success or for a generic error.
const FAILPOINT_EXIT_CODE: u8 = 86;

#[derive(Debug, Parser)]
#[command(name = "m0_fixture_host")]
struct Cli {
    #[arg(long)]
    data_dir: PathBuf,
    #[arg(long)]
    host_id: String,
    #[arg(long, value_enum)]
    mode: Mode,
    /// Failpoint name reported when `--mode failpoint` is reached.
    #[arg(long)]
    failpoint: Option<String>,
    /// File written just before the failpoint exit, so the parent can prove the
    /// boundary was reached.
    #[arg(long)]
    marker: Option<PathBuf>,
    /// Fixed `UUIDv7` handed to the next generated ID, in order. Admission
    /// generates an event ID and an instruction ID, so two are expected.
    #[arg(long = "id-uuid")]
    id_uuids: Vec<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Mode {
    /// Hold the writer lock until killed.
    Hold,
    /// Admit one input and stay alive.
    Admit,
    /// Admit one input, then exit at the failpoint boundary.
    Failpoint,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}

async fn run(cli: Cli) -> Result<ExitCode, String> {
    let host_id = HostId::parse(cli.host_id).map_err(|error| error.to_string())?;
    let store = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(cli.data_dir, host_id))
            .await
            .map_err(|error| error.to_string())?,
    );
    let generation = store.fence().map_err(|error| error.to_string())?.generation;
    println!(
        "{}",
        serde_json::json!({"ready": "m0_fixture_host", "pid": std::process::id(), "generation": generation})
    );
    if matches!(cli.mode, Mode::Hold) {
        wait_for_parent_termination();
        return Ok(ExitCode::SUCCESS);
    }

    let session_id = SessionId::generate();
    let task_id = TaskId::generate();
    let input_id = InputId::generate();
    let ids: Arc<dyn IdSource> = if cli.id_uuids.is_empty() {
        Arc::new(harness_types::SystemIdSource)
    } else {
        Arc::new(
            FixedIdSource::parse_uuids(cli.id_uuids.iter().map(String::as_str))
                .map_err(|error| error.to_string())?,
        )
    };
    let service = SessionService::with_id_source(Arc::clone(&store), ids);
    let admission = service
        .admit_input(AdmitInputRequest {
            session_id: session_id.clone(),
            task_id: task_id.clone(),
            input_id,
            expected_sequence: 1,
            authority: SourceAuthority::User,
            raw_text: "m0 fixture input".to_owned(),
            workspace: WorkspaceObservation {
                project_id: ProjectId::generate(),
                worktree_id: "m0-fixture".to_owned(),
                base_commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                observed_fingerprint: ContentHash::from_bytes(b"m0 fixture workspace"),
            },
            initial_plan_items: Vec::new(),
        })
        .await
        .map_err(|error| error.to_string())?;
    println!(
        "{}",
        serde_json::json!({"ack": "input", "sequence": admission.sequence, "event_id": admission.event_id.as_str()})
    );

    if matches!(cli.mode, Mode::Failpoint) {
        let name = cli.failpoint.unwrap_or_else(|| "unnamed".to_owned());
        if let Some(marker) = &cli.marker {
            std::fs::write(marker, format!("{name}\n")).map_err(|error| {
                format!(
                    "failpoint marker {} is not writable: {error}",
                    marker.display()
                )
            })?;
        }
        println!(
            "{}",
            serde_json::json!({"failpoint": name, "marker": cli.marker.as_ref().map(|path| path.display().to_string())})
        );
        return Ok(ExitCode::from(FAILPOINT_EXIT_CODE));
    }

    wait_for_parent_termination();
    Ok(ExitCode::SUCCESS)
}

fn wait_for_parent_termination() {
    let mut byte = [0_u8; 1];
    let _ = std::io::stdin().read(&mut byte);
}
