#![forbid(unsafe_code)]

use std::{fs, path::PathBuf, process::ExitCode};

use clap::{Args, Parser, Subcommand};
use harness_types::{ErrorCode, HarnessConfig, HarnessError};

/// Personal coding-agent harness.
///
/// P0 exposes only the foundation command surface. Agent execution is not
/// implemented in this phase.
#[derive(Debug, Parser)]
#[command(name = "ha", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Validate a strict, non-secret P0 configuration file.
    Config(ConfigCommand),
}

#[derive(Debug, Args)]
struct ConfigCommand {
    #[command(subcommand)]
    command: ConfigSubcommand,
}

#[derive(Debug, Subcommand)]
enum ConfigSubcommand {
    /// Parse and validate a P0 TOML configuration without starting a host.
    Validate {
        /// Path to the TOML configuration file.
        #[arg(long)]
        config: PathBuf,
        /// Emit a versioned JSON result to stdout.
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> Result<(), HarnessError> {
    match cli.command {
        Some(Command::Config(ConfigCommand {
            command: ConfigSubcommand::Validate { config, json },
        })) => {
            let config = read_config(&config)?;
            if json {
                let result = serde_json::json!({
                    "schema_version": 1,
                    "valid": true,
                    "config": config,
                });
                println!("{result}");
            } else {
                println!("config valid: schema_version={}", config.schema_version);
            }
            Ok(())
        }
        None => Ok(()),
    }
}

fn read_config(path: &PathBuf) -> Result<HarnessConfig, HarnessError> {
    let contents = fs::read_to_string(path).map_err(|_| {
        HarnessError::new(
            ErrorCode::ConfigReadError,
            "configuration file could not be read",
        )
    })?;
    let config: HarnessConfig = toml::from_str(&contents).map_err(|error| {
        let code = if error.to_string().contains("unknown field") {
            ErrorCode::ConfigUnknownField
        } else {
            ErrorCode::ConfigParseError
        };
        HarnessError::new(code, "configuration file is invalid")
    })?;
    config.validate()?;
    Ok(config)
}
