//! `ha project`: which store a project is kept in
//! (`docs/architecture/PROJECT_IDENTITY.en.md`).

use std::path::PathBuf;

use clap::{Args, Subcommand};
use harness_types::{ErrorCode, HarnessError};

use crate::interactive::{adoption, bootstrap, cli_extras};

#[derive(Debug, Args)]
pub struct ProjectCommand {
    #[command(subcommand)]
    command: ProjectSubcommand,
}

#[derive(Debug, Subcommand)]
enum ProjectSubcommand {
    /// Move a store kept under another key to this project's key: its
    /// conversations, and the memory and lessons kept for it. Refused when
    /// this project has a store of its own: two stores that both hold data
    /// are never merged.
    Rekey {
        /// The store to move: its key (`project-` and 16 hex digits), or the
        /// directory the project was at.
        old: String,
        /// The project; the working directory when left out.
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
}

/// Run one `ha project` command.
///
/// # Errors
/// The data directory or the project cannot be resolved, no store is kept
/// under the old key, or the move is refused.
pub fn run(command: ProjectCommand) -> Result<(), HarnessError> {
    match command.command {
        ProjectSubcommand::Rekey { old, cwd } => {
            let data = cli_extras::current_data_dir()?;
            let cwd = match cwd {
                Some(cwd) => cwd,
                None => std::env::current_dir().map_err(|error| {
                    HarnessError::new(ErrorCode::StorageOpenFailed, error.to_string())
                })?,
            };
            let root = bootstrap::project_root(&cwd);
            let key = bootstrap::project_key_of(&cwd);
            let Some(from) = adoption::store_named(&data, &old) else {
                return Err(HarnessError::new(
                    ErrorCode::ResourceNotFound,
                    format!("no project store is kept under {old}"),
                ));
            };
            if from == key {
                println!("{old} is this project's store ({key}) already: nothing to move.");
                return Ok(());
            }
            adoption::take_over(&data, &root, &key, &from, "store_rekeyed")
                .map_err(|error| HarnessError::new(ErrorCode::StorageWriteFailed, error))?;
            println!(
                "Moved the store {from} to {key}, the key of {}: its conversations, memory and lessons are this project's now.",
                root.display()
            );
            Ok(())
        }
    }
}
