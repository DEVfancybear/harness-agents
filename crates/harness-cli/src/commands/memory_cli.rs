//! `ha memory`: the memory stores from the command line
//! (`docs/architecture/MEMORY_STORE.en.md`).

use clap::{Args, Subcommand};
use harness_types::{ErrorCode, HarnessError};

use crate::interactive::{cli_extras, harness, memory_store};

#[derive(Debug, Args)]
pub struct MemoryCommand {
    #[command(subcommand)]
    command: MemorySubcommand,
}

#[derive(Debug, Subcommand)]
enum MemorySubcommand {
    /// Print a store's whole state as JSON: its entries and refinements, in
    /// `harness_state.json`'s shape.
    Export {
        /// A conversation's own store instead of the global one.
        #[arg(long)]
        conversation: Option<String>,
    },
}

/// Run one `ha memory` command.
///
/// # Errors
/// The data directory cannot be resolved or the store cannot be read.
pub fn run(command: MemoryCommand) -> Result<(), HarnessError> {
    let data = cli_extras::current_data_dir()?;
    match command.command {
        MemorySubcommand::Export { conversation } => {
            let (scope, dir) = match &conversation {
                Some(task) => ("local", harness::local_dir(&data, task)),
                None => ("global", harness::global_dir(&data)),
            };
            let document = memory_store::load_document(&dir)
                .map_err(|error| HarnessError::new(ErrorCode::StorageOpenFailed, error))?;
            println!(
                "{}",
                serde_json::json!({
                    "schema_version": 1,
                    "scope": scope,
                    "store": memory_store::db_path(&dir).display().to_string(),
                    "document": document,
                })
            );
            Ok(())
        }
    }
}
