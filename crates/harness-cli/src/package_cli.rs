//! prime-agent's `package` command (`pa-cli/src/package_command.rs`): install,
//! remove, update and list packages of skills, prompt templates and themes.
//!
//! ha has no self-update, so `package update` updates installed packages
//! only (prime's `package update --extensions`).

use std::path::PathBuf;

use clap::{Args, Subcommand};
use harness_types::{ErrorCode, HarnessError};

use crate::interactive::packages::{
    ConfiguredPackage, PackageManager, ProgressEvent, ProgressEventKind, SettingsManager,
    UserOrProject,
};

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
    let settings = SettingsManager::create(&cwd, &agent_dir);
    let mut manager = PackageManager::new(cwd, agent_dir, settings);
    manager.set_progress_callback(Box::new(|event: &ProgressEvent| {
        if event.kind == ProgressEventKind::Start
            && let Some(message) = &event.message
        {
            println!("{message}");
        }
    }));
    let scope = |local: bool| {
        if local {
            UserOrProject::Project
        } else {
            UserOrProject::User
        }
    };
    match command.command {
        PackageSubcommand::Install { source, local } => {
            manager
                .install_and_persist(&source, scope(local))
                .map_err(failure)?;
            println!("Installed {source}");
        }
        PackageSubcommand::Remove { source, local } => {
            if !manager
                .remove_and_persist(&source, scope(local))
                .map_err(failure)?
            {
                return Err(HarnessError::new(
                    ErrorCode::ConfigReadError,
                    format!("No matching package found for {source}"),
                ));
            }
            println!("Removed {source}");
        }
        PackageSubcommand::Update { source } => {
            manager.update(source.as_deref()).map_err(failure)?;
            match source {
                Some(source) => println!("Updated {source}"),
                None => println!("Updated packages"),
            }
        }
        PackageSubcommand::List => print_package_list(&manager.list_configured_packages()),
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

fn failure(error: anyhow::Error) -> HarnessError {
    HarnessError::new(ErrorCode::ConfigReadError, format!("Error: {error}"))
}

/// Print the configured package list (user section, then project section).
fn print_package_list(packages: &[ConfiguredPackage]) {
    if packages.is_empty() {
        println!("No packages installed.");
        return;
    }
    let user_packages: Vec<_> = packages
        .iter()
        .filter(|package| package.scope == UserOrProject::User)
        .collect();
    let project_packages: Vec<_> = packages
        .iter()
        .filter(|package| package.scope == UserOrProject::Project)
        .collect();
    if !user_packages.is_empty() {
        println!("User packages:");
        for package in &user_packages {
            print_configured_package(package);
        }
    }
    if !project_packages.is_empty() {
        if !user_packages.is_empty() {
            println!();
        }
        println!("Project packages:");
        for package in &project_packages {
            print_configured_package(package);
        }
    }
}

fn print_configured_package(package: &ConfiguredPackage) {
    let display = if package.filtered {
        format!("{} (filtered)", package.source)
    } else {
        package.source.clone()
    };
    println!("  {display}");
    if let Some(installed_path) = &package.installed_path {
        println!("    {}", installed_path.display());
    }
}
