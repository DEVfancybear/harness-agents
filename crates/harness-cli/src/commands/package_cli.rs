//! prime-agent's `package` command (`pa-cli/src/package_command.rs`): install,
//! remove, update and list packages of skills, prompt templates and themes.
//!
//! ha has no self-update, so `package update` updates installed packages
//! only (prime's `package update --extensions`).

use std::path::PathBuf;

use clap::{Args, Subcommand};
use harness_types::{ErrorCode, HarnessError};

use crate::interactive::packages::command::{self as package_command, Action};

#[derive(Debug, Args)]
pub struct PackageCommand {
    #[command(subcommand)]
    command: PackageSubcommand,
}

#[derive(Debug, Subcommand)]
enum PackageSubcommand {
    /// Install a package and add it to settings.
    #[command(
        after_help = "Examples:\n  ha package install npm:@foo/bar\n  ha package install git:github.com/user/repo\n  ha package install git:git@github.com:user/repo\n  ha package install https://github.com/user/repo\n  ha package install ssh://git@github.com/user/repo\n  ha package install ./local/path"
    )]
    Install {
        source: String,
        /// Install project-locally (.harness/settings.json).
        #[arg(long)]
        local: bool,
    },
    /// Remove a package and its source from settings.
    #[command(alias = "uninstall")]
    Remove {
        source: String,
        /// Remove from project settings (.harness/settings.json).
        #[arg(long)]
        local: bool,
    },
    /// Update installed packages, or one package.
    Update { source: Option<String> },
    /// List installed packages from user and project settings.
    List,
}

pub fn run(command: PackageCommand) -> Result<(), HarnessError> {
    let cwd = std::env::current_dir().map_err(|error| {
        HarnessError::new(
            ErrorCode::ConfigReadError,
            format!("current directory unavailable: {error}"),
        )
    })?;
    let agent_dir = agent_dir()?;
    let environment = crate::interactive::paths::LaunchEnvironment::capture();
    crate::interactive::packages::set_offline(crate::interactive::offline::is_offline(
        &environment,
    ));
    let mut manager = package_command::manager(&cwd, &agent_dir, |step| println!("{step}"));
    let action = match command.command {
        PackageSubcommand::Install { source, local } => Action::Install { source, local },
        PackageSubcommand::Remove { source, local } => Action::Remove { source, local },
        PackageSubcommand::Update { source } => Action::Update { source },
        PackageSubcommand::List => Action::List,
    };
    let lines = package_command::run(&mut manager, &action)
        .map_err(|message| HarnessError::new(ErrorCode::ConfigReadError, message))?;
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

/// prime's agent directory: ha's config directory, where `settings.json`,
/// `skills/` and `prompts/` live.
pub fn agent_dir() -> Result<PathBuf, HarnessError> {
    let environment = crate::interactive::paths::LaunchEnvironment::capture();
    let paths = crate::interactive::paths::resolve(&crate::interactive::paths::PathRequest {
        platform: crate::interactive::paths::HostPlatform::current(),
        environment: &environment,
        explicit_data_dir: None,
    })?;
    Ok(paths
        .config_file
        .parent()
        .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf))
}
