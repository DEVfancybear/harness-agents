#![forbid(unsafe_code)]
#![allow(clippy::struct_excessive_bools, reason = "one flag per launch mode")]

mod delegation_cli;
mod extension_cli;
mod interactive;
mod maintenance_cli;
mod memory_cli;
mod sandbox_cli;
mod web;

use std::{path::PathBuf, process::ExitCode, sync::Arc};

use clap::{Args, Parser, Subcommand};
use harness_providers::{MockProvider, ProviderStreamEvent};
use harness_runtime::{RunRequest, RuntimeConfig, RuntimeService};
use harness_session::SessionService;
use harness_store_sqlite::{SqliteStore, StoreDiagnostics, StoreError, WriterOpenOptions};
use harness_tools::{
    CodingLoopService, ToolExecutionService, coding_tool_schemas, observe_workspace,
    observed_file_hash,
};
use harness_types::{
    AgentRunId, ContentHash, ErrorCode, HarnessError, HostId, InputId, PluginInstanceId,
    PluginManifest, ProjectId, ScopeId, ServiceContract, SessionId, TaskId, WorkspaceObservation,
};

/// Personal coding-agent harness.
///
/// P4 adds scoped local memory and bounded, explicit extraction catch-up.
#[derive(Debug, Parser)]
#[command(name = "ha", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Open the interactive Harness app; the same entrypoint as bare ha.
    Chat(ChatArgs),
    /// Search, inspect and maintain scoped reusable memory.
    Memory(memory_cli::MemoryCommand),
    /// Serve the loopback web surface: an authenticated API and the local UI.
    Web {
        #[arg(long)]
        data_dir: PathBuf,
        /// Port on loopback. A non-loopback bind is refused.
        #[arg(long, default_value_t = 8799)]
        port: u16,
        #[arg(long)]
        json: bool,
    },
    /// Initialize a local P1 `SQLite` data directory and inspectable built-in metadata.
    Init {
        #[arg(long)]
        data_dir: PathBuf,
        /// Emit a versioned JSON result to stdout.
        #[arg(long)]
        json: bool,
    },
    /// Parse, validate, or explain strict non-secret configuration.
    Config(ConfigCommand),
    /// List persisted sessions without starting an execution runtime.
    Sessions(SessionsCommand),
    /// Render a durable session recovery/status view without executing work.
    Status {
        /// Local P1 `SQLite` data directory.
        #[arg(long)]
        data_dir: PathBuf,
        /// Session ID to inspect.
        #[arg(long)]
        session_id: String,
        /// Emit a versioned JSON result to stdout.
        #[arg(long)]
        json: bool,
    },
    /// Durable human input: ask, answer or list questions (M3).
    Input(InputCommand),
    /// List or inspect persisted plugin metadata without loading a plugin.
    Plugins(PluginsCommand),
    /// Execute one keyless local mock-provider run.
    Run {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        text: String,
        #[arg(long)]
        json: bool,
    },
    /// Recover a session without dispatching a provider.
    Resume {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        session_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Continue a task in a new session using the previous task checkpoint.
    Continue {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        task_id: String,
        #[arg(long)]
        text: String,
        #[arg(long)]
        json: bool,
    },
    /// Inspect persisted context packets.
    Context(ContextCommand),
    /// Session replay and offline inspection commands.
    Session(SessionCommand),
    /// P3 coding-tool capabilities and a deterministic local fixture.
    Code(CodingCommand),
    /// M12 strict execution: measure what this host can enforce, and what it refuses.
    Sandbox(sandbox_cli::SandboxCommand),
    /// P5 delegation: coordinator/worker runs and durable task views.
    Tasks(delegation_cli::TaskCommand),
    /// P6 external extensions: trust inspection, local registration and skills.
    Extensions(extension_cli::ExtensionCommand),
    /// P7 recovery hardening: doctor, backup, restore, retention, GC and release matrix.
    Maintenance(maintenance_cli::MaintenanceCommand),
}

/// Options for the interactive entrypoint (`HA_LAUNCH` H01).
#[derive(Debug, Args)]
struct ChatArgs {
    /// Open the project at this path instead of the caller working directory.
    #[arg(long)]
    cwd: Option<PathBuf>,
    /// Resume one persisted session inside the interactive app.
    #[arg(long)]
    resume: Option<String>,
    /// Select the model for this conversation; `/model` switches subsequent turns.
    #[arg(long)]
    model: Option<String>,
    /// Select a named config profile.
    #[arg(long)]
    profile: Option<String>,
    /// Approval mode: ask, auto-edit or full-auto.
    #[arg(long, value_parser = ["ask", "auto-edit", "full-auto"])]
    approval: Option<String>,
    /// Temporary tool allow pattern for one headless turn; repeatable.
    #[arg(long = "allowed-tools", requires = "headless")]
    allowed_tools: Vec<String>,
    /// Temporary tool deny pattern for one headless turn; repeatable.
    #[arg(long = "disallowed-tools", requires = "headless")]
    disallowed_tools: Vec<String>,
    /// Use the labelled local fixture backend instead of a model; no provider is
    /// called and the header says so.
    #[arg(long)]
    fixture: bool,
    /// Run exactly one turn without a terminal and print the result to stdout.
    #[arg(long, requires = "prompt")]
    headless: bool,
    /// Prompt text for the single headless turn.
    #[arg(long, requires = "headless")]
    prompt: Option<String>,
    /// Emit a versioned JSON result; only valid with --headless.
    #[arg(long, requires = "headless")]
    json: bool,
    /// Draw the app with the plain renderer instead of the TUI.
    ///
    /// The same renderer the app falls back to when the console is too small, is
    /// not a real terminal, or `HA_UI=plain` is set.
    #[arg(long, conflicts_with = "headless")]
    plain: bool,
    /// Use the explicit deterministic mock profile for a headless turn; no
    /// provider is called and the JSON result says `"fixture": true`.
    #[arg(long, requires = "headless")]
    mock: bool,
    /// Goal objective for a headless turn. Without it the turn is one bounded
    /// pass; with it the host evaluates typed criteria and may continue.
    #[arg(long, requires = "headless")]
    goal: Option<String>,
    /// Evidence a goal criterion requires; repeatable. One of: `response`,
    /// `tool_execution`, `file_change`, `check`, `artifact`.
    #[arg(long = "criteria", requires = "goal")]
    criteria: Vec<String>,
    /// Host continuations one headless run may spend on its goal.
    #[arg(long, requires = "goal")]
    max_continuations: Option<u32>,
    /// Token budget for a headless run; the run reserves against it.
    #[arg(long, requires = "headless")]
    budget: Option<u64>,
}

impl ChatArgs {
    fn mode(&self) -> Result<interactive::LaunchMode, interactive::UsageError> {
        let mut mode = interactive::mode_from_args(
            self.cwd.clone(),
            self.resume.clone(),
            self.fixture,
            self.headless,
            self.prompt.clone(),
            self.json,
            self.plain,
            interactive::HeadlessOptions {
                mock: self.mock,
                goal: self.goal.clone(),
                criteria: self.criteria.clone(),
                max_continuations: self.max_continuations,
                budget_tokens: self.budget,
                approval: self.approval.clone(),
                allowed_tools: self.allowed_tools.clone(),
                disallowed_tools: self.disallowed_tools.clone(),
            },
        )?;
        match &mut mode {
            interactive::LaunchMode::Interactive {
                config_overrides, ..
            } => {
                config_overrides.model.clone_from(&self.model);
                config_overrides.profile.clone_from(&self.profile);
                config_overrides.approval.clone_from(&self.approval);
                config_overrides
                    .allowed_tools
                    .clone_from(&self.allowed_tools);
                config_overrides
                    .disallowed_tools
                    .clone_from(&self.disallowed_tools);
            }
            interactive::LaunchMode::Headless { .. }
                if self.model.is_some() || self.profile.is_some() =>
            {
                return Err(interactive::UsageError::new(
                    "--model and --profile apply to interactive chat only",
                ));
            }
            interactive::LaunchMode::Headless { .. } => {}
        }
        Ok(mode)
    }
}

#[derive(Debug, Args)]
struct ContextCommand {
    #[command(subcommand)]
    command: ContextSubcommand,
}

#[derive(Debug, Subcommand)]
enum ContextSubcommand {
    /// Inspect the durable context of one session.
    Inspect {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        session_id: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
struct SessionCommand {
    #[command(subcommand)]
    command: SessionSubcommand,
}

#[derive(Debug, Subcommand)]
enum SessionSubcommand {
    Replay {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        session_id: String,
        #[arg(long)]
        offline: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
struct CodingCommand {
    #[command(subcommand)]
    command: CodingSubcommand,
}

#[derive(Debug, Subcommand)]
enum CodingSubcommand {
    /// Report only the coding protections this host can actually enforce.
    Capabilities {
        /// Emit a versioned JSON result to stdout.
        #[arg(long)]
        json: bool,
    },
    /// Run a keyless parser-fix fixture through P2 normalization and the P3 gate.
    Fixture {
        /// Local `SQLite` data directory owned by this harness.
        #[arg(long)]
        data_dir: PathBuf,
        /// Existing workspace root containing the fixture file.
        #[arg(long)]
        workspace: PathBuf,
        /// Relative UTF-8 file path rooted in --workspace.
        #[arg(long)]
        path: String,
        /// Text searched before the approved edit.
        #[arg(long)]
        find: String,
        /// Complete replacement content for the fixture file.
        #[arg(long)]
        replace: String,
        /// Issue one explicit approval for every proposed fixture action.
        #[arg(long)]
        approve: bool,
        /// Emit a versioned JSON result to stdout.
        #[arg(long)]
        json: bool,
    },
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
    /// Explain effective user, trusted project, environment and CLI configuration.
    Explain {
        /// Optional user config override; defaults to the platform config path.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Show the effective configuration with this one-turn model override.
        #[arg(long)]
        model: Option<String>,
        /// Select a named profile while explaining.
        #[arg(long)]
        profile: Option<String>,
        /// Explain the requested approval policy.
        #[arg(long)]
        approval: Option<String>,
        /// Emit a versioned JSON result to stdout.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
struct SessionsCommand {
    #[command(subcommand)]
    command: SessionsSubcommand,
}

#[derive(Debug, Args)]
struct InputCommand {
    #[command(subcommand)]
    command: InputSubcommand,
}

#[derive(Debug, Subcommand)]
enum InputSubcommand {
    /// Persist a question for a run; the caller exits without holding anything.
    Ask {
        /// Local `SQLite` data directory owned by this harness.
        #[arg(long)]
        data_dir: PathBuf,
        /// Session the run belongs to.
        #[arg(long)]
        session_id: String,
        /// Task the run belongs to.
        #[arg(long)]
        task_id: String,
        /// Run ID the question is scoped to.
        #[arg(long)]
        run_id: Option<String>,
        /// The question text shown to the user.
        #[arg(long)]
        prompt: String,
        /// Unix milliseconds after which the question refuses answers.
        #[arg(long)]
        expires_at_unix_ms: Option<u64>,
        /// Scope key override; defaults to the run/request scope.
        #[arg(long)]
        scope_key: Option<String>,
        /// Emit a versioned JSON result to stdout.
        #[arg(long)]
        json: bool,
    },
    /// Answer one question by scope key. Deduped and scope-checked.
    Answer {
        /// Local `SQLite` data directory owned by this harness.
        #[arg(long)]
        data_dir: PathBuf,
        /// Scope key printed when the question was asked.
        #[arg(long)]
        scope_key: String,
        /// The answer text.
        #[arg(long)]
        text: String,
        /// Who answered; recorded with the answer.
        #[arg(long, default_value = "cli.user")]
        actor: String,
        /// Emit a versioned JSON result to stdout.
        #[arg(long)]
        json: bool,
    },
    /// List the questions of one session.
    List {
        /// Local `SQLite` data directory owned by this harness.
        #[arg(long)]
        data_dir: PathBuf,
        /// Session whose questions are listed.
        #[arg(long)]
        session_id: String,
        /// Emit a versioned JSON result to stdout.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum SessionsSubcommand {
    /// List durable session summaries through a read-only store connection.
    List {
        /// Local P1 `SQLite` data directory.
        #[arg(long)]
        data_dir: PathBuf,
        /// Emit a versioned JSON result to stdout.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
struct PluginsCommand {
    #[command(subcommand)]
    command: PluginsSubcommand,
}

#[derive(Debug, Subcommand)]
enum PluginsSubcommand {
    /// List persisted implementation metadata through a read-only store connection.
    List {
        /// Local P1 `SQLite` data directory.
        #[arg(long)]
        data_dir: PathBuf,
        /// Emit a versioned JSON result to stdout.
        #[arg(long)]
        json: bool,
    },
    /// Inspect one persisted plugin manifest without loading it.
    Inspect {
        /// Local P1 `SQLite` data directory.
        #[arg(long)]
        data_dir: PathBuf,
        /// Stable plugin instance ID.
        #[arg(long)]
        instance_id: String,
        /// Emit a versioned JSON result to stdout.
        #[arg(long)]
        json: bool,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    // The dispatch future is large because one arm resolves a project identity before
    // it opens a store. Boxing it in the entry point is one heap move at startup
    // instead of boxing each arm and changing how the whole dispatch reads.
    #[allow(
        clippy::large_futures,
        reason = "one boxed dispatch future at the entry point"
    )]
    let cli = Cli::parse();
    let json_error_envelope = json_requested() && legacy_command(&cli);
    let outcome = Box::pin(run(cli)).await;
    match outcome {
        Ok(code) => code,
        Err(error) => {
            // Failures stay on stderr, which is where every accepted H/P
            // contract reads them; stdout keeps only the command's result. A
            // legacy `--json` invocation gets the typed report as one JSON
            // document instead of prose, and the exit code always comes from
            // the stable error code (CONTRACTS §9).
            if json_error_envelope {
                eprintln!("{}", error_report_json(&error));
            } else {
                eprintln!("{error}");
            }
            ExitCode::from(error.exit_code())
        }
    }
}

/// True when the invoked command asked for a JSON result.
///
/// The flag is read before dispatch because an error can happen while the
/// command is still being parsed or resolved.
fn json_requested() -> bool {
    std::env::args().any(|argument| argument == "--json")
}

/// The interactive launch reports its own failures as prose on stderr
/// (`HA_LAUNCH` H01-I03), so the typed JSON error report is for the legacy
/// subcommands that already write JSON.
fn legacy_command(cli: &Cli) -> bool {
    !matches!(cli.command, None | Some(Command::Chat(_)))
}

/// The versioned error envelope: `schema_version`, `status`, `exit_code` and
/// the typed `error` report.
fn error_report_json(error: &HarnessError) -> serde_json::Value {
    let report = error.report();
    serde_json::json!({
        "schema_version": report.schema_version,
        "status": "error",
        "exit_code": report.exit_code(),
        "error": report,
    })
}

/// Route the launch contract added by `HA_LAUNCH` H01, then fall back to the
/// unchanged legacy dispatch for every existing subcommand.
async fn run(cli: Cli) -> Result<ExitCode, HarnessError> {
    let Cli { command } = cli;
    match command {
        None => {
            Box::pin(interactive::launch(interactive::LaunchMode::Interactive {
                cwd: None,
                resume: None,
                fixture: false,
                plain: interactive::plain_requested_from_environment(),
                config_overrides: interactive::config::ConfigOverrides::default(),
            }))
            .await
        }
        Some(Command::Chat(args)) => match args.mode() {
            Ok(mode) => Box::pin(interactive::launch(mode)).await,
            Err(usage) => {
                eprintln!("{usage}");
                Ok(ExitCode::from(interactive::USAGE_EXIT_CODE))
            }
        },
        Some(command) => {
            legacy_run(Cli {
                command: Some(command),
            })
            .await?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

#[allow(clippy::too_many_lines)]
async fn legacy_run(cli: Cli) -> Result<(), HarnessError> {
    match cli.command {
        Some(Command::Web {
            data_dir,
            port,
            json,
        }) => {
            let config = web::WebConfig::loopback(&data_dir, port);
            let handle = web::serve(config).await?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "schema_version": 1,
                        "status": "serving",
                        "url": format!("http://{}/", handle.address),
                        "session_token": handle.token,
                        "note": "a mutation needs the X-Ha-Session header; loopback is not an authentication boundary",
                    })
                );
            } else {
                println!("ha web is serving http://{}/", handle.address);
                println!(
                    "session token (keep it out of URLs and logs): {}",
                    handle.token
                );
                println!("press Ctrl+C to stop");
            }
            tokio::signal::ctrl_c().await.map_err(|error| {
                HarnessError::new(
                    ErrorCode::RuntimeBlocked,
                    format!("cannot wait for shutdown: {error}"),
                )
            })?;
            Ok(())
        }
        // Boxed: this arm now resolves a project identity before it opens a store,
        // which grows the future past the size the other arms keep. Boxing one arm is
        // cheaper than reshaping the dispatch.
        Some(Command::Memory(command)) => Box::pin(memory_cli::run(command)).await,
        Some(Command::Init { data_dir, json }) => init_store(&data_dir, json).await,
        Some(Command::Config(ConfigCommand {
            command: ConfigSubcommand::Validate { config, json },
        })) => {
            let config = interactive::config::load(&config)?;
            let (version, value) = match config {
                interactive::config::ConfigState::Loaded { config, .. } => (
                    1,
                    serde_json::to_value(config).unwrap_or(serde_json::Value::Null),
                ),
                interactive::config::ConfigState::LoadedV2 { config, .. } => (
                    2,
                    serde_json::to_value(config).unwrap_or(serde_json::Value::Null),
                ),
                interactive::config::ConfigState::FirstRun { .. } => (1, serde_json::json!({})),
            };
            if json {
                let result = serde_json::json!({
                    "schema_version": 1,
                    "valid": true,
                    "config_schema_version": version,
                    "config": value,
                });
                println!("{result}");
            } else {
                println!("config valid: schema_version={version}");
            }
            Ok(())
        }
        Some(Command::Config(ConfigCommand {
            command:
                ConfigSubcommand::Explain {
                    config,
                    model,
                    profile,
                    approval,
                    json,
                },
        })) => {
            let environment = interactive::paths::LaunchEnvironment::capture();
            let caller_dir = std::env::current_dir().map_err(|error| {
                HarnessError::new(
                    ErrorCode::ConfigReadError,
                    format!("current directory is unavailable: {error}"),
                )
            })?;
            let defaults = interactive::paths::resolve(&interactive::paths::PathRequest {
                platform: interactive::paths::HostPlatform::current(),
                environment: &environment,
                explicit_data_dir: None,
            })?;
            let user_config = config.unwrap_or(defaults.config_file);
            let effective = interactive::config::resolve_layers(
                &user_config,
                &caller_dir,
                &environment,
                &interactive::config::ConfigOverrides {
                    model,
                    profile,
                    approval,
                    ..interactive::config::ConfigOverrides::default()
                },
            )
            .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
            let result = serde_json::json!({
                "schema_version": 2,
                "effective_config": {
                    "provider": effective.provider,
                    "profile": effective.profile,
                    "approval": effective.approval,
                    "permissions": {
                        "allow": effective.allow_rules,
                        "deny": effective.deny_rules
                    }
                },
                "sources": effective.explain,
                "project_config_reason": effective.project_config_reason
            });
            if json {
                println!("{result}");
            } else {
                println!("config source: {}", user_config.display());
                for entry in effective.explain {
                    println!(
                        "{} = {} [{}]{}",
                        entry.key,
                        entry.value,
                        entry.layer.as_str(),
                        entry
                            .reason
                            .map_or_else(String::new, |reason| format!(" — {reason}"))
                    );
                }
            }
            Ok(())
        }
        Some(Command::Sessions(SessionsCommand {
            command: SessionsSubcommand::List { data_dir, json },
        })) => list_sessions(&data_dir, json).await,
        Some(Command::Status {
            data_dir,
            session_id,
            json,
        }) => show_status(&data_dir, &session_id, json).await,
        Some(Command::Input(InputCommand { command })) => run_input_command(command).await,
        Some(Command::Plugins(PluginsCommand {
            command: PluginsSubcommand::List { data_dir, json },
        })) => list_plugins(&data_dir, json).await,
        Some(Command::Plugins(PluginsCommand {
            command:
                PluginsSubcommand::Inspect {
                    data_dir,
                    instance_id,
                    json,
                },
        })) => inspect_plugin(&data_dir, &instance_id, json).await,
        Some(Command::Run {
            data_dir,
            text,
            json,
        }) => run_runtime(&data_dir, &text, json).await,
        Some(Command::Resume {
            data_dir,
            session_id,
            json,
        }) => resume_runtime(&data_dir, &session_id, json).await,
        Some(Command::Continue {
            data_dir,
            task_id,
            text,
            json,
        }) => continue_runtime(&data_dir, &task_id, &text, json).await,
        Some(Command::Context(ContextCommand {
            command:
                ContextSubcommand::Inspect {
                    data_dir,
                    session_id,
                    json,
                },
        })) => inspect_context(&data_dir, &session_id, json).await,
        Some(Command::Session(SessionCommand {
            command:
                SessionSubcommand::Replay {
                    data_dir,
                    session_id,
                    offline,
                    json,
                },
        })) => replay_session(&data_dir, &session_id, offline, json).await,
        Some(Command::Code(CodingCommand {
            command: CodingSubcommand::Capabilities { json },
        })) => {
            show_coding_capabilities(json);
            Ok(())
        }
        Some(Command::Code(CodingCommand {
            command:
                CodingSubcommand::Fixture {
                    data_dir,
                    workspace,
                    path,
                    find,
                    replace,
                    approve,
                    json,
                },
        })) => {
            run_coding_fixture(&data_dir, &workspace, &path, &find, &replace, approve, json).await
        }
        Some(Command::Sandbox(command)) => sandbox_cli::run(command).await,
        Some(Command::Tasks(command)) => delegation_cli::run(command).await,
        Some(Command::Extensions(command)) => extension_cli::run(command).await,
        Some(Command::Maintenance(command)) => maintenance_cli::run(command).await,
        // The interactive entrypoint is routed by run() before legacy dispatch;
        // reaching this arm would mean the launch contract was bypassed.
        Some(Command::Chat(_)) => Err(HarnessError::new(
            ErrorCode::InvalidStateTransition,
            "interactive chat must be routed by run()",
        )),
        None => Ok(()),
    }
}

fn show_coding_capabilities(json_output: bool) {
    let capabilities = ToolExecutionService::capabilities();
    let output = serde_json::json!({
        "schema_version": 1,
        "tool_contract_version": ToolExecutionService::contract_version(),
        "capabilities": capabilities,
        "tool_schema_count": coding_tool_schemas().len(),
    });
    if json_output {
        println!("{output}");
    } else {
        println!(
            "P3 tools v{}; tree cleanup: {}; strict isolation: {}",
            ToolExecutionService::contract_version(),
            output["capabilities"]["process_tree_cleanup"],
            output["capabilities"]["strict_isolation"]
        );
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_coding_fixture(
    data_dir: &PathBuf,
    workspace: &PathBuf,
    path: &str,
    find: &str,
    replacement: &str,
    approve: bool,
    json_output: bool,
) -> Result<(), HarnessError> {
    // The fixture belongs to the same project as every other run in this data directory:
    // the identity is resolved rather than invented, so the receipts and artifacts one
    // invocation writes stay addressable by the next one.
    let store = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(data_dir, HostId::generate()))
            .await
            .map_err(store_error)?,
    );
    let project_id = interactive::project::resolve_project_id(&store, workspace).await?;
    let workspace_observation = observe_workspace(project_id, workspace)?;
    let expected_hash = observed_file_hash(workspace, path)?;
    let patch_arguments = serde_json::to_string(&serde_json::json!({
        "path": path,
        "expected_hash": expected_hash,
        "replacement": replacement,
    }))
    .map_err(|_| HarnessError::new(ErrorCode::InvalidPayload, "fixture patch is invalid"))?;
    let initial_search_arguments = serde_json::to_string(&serde_json::json!({
        "query": find,
        "path": path,
    }))
    .map_err(|_| HarnessError::new(ErrorCode::InvalidPayload, "fixture search is invalid"))?;
    let final_search_arguments = serde_json::to_string(&serde_json::json!({
        "query": replacement,
        "path": path,
    }))
    .map_err(|_| HarnessError::new(ErrorCode::InvalidPayload, "fixture verification is invalid"))?;
    let provider = Arc::new(MockProvider::scripted(vec![
        ProviderStreamEvent::started(),
        ProviderStreamEvent::text("deterministic parser fixture"),
        ProviderStreamEvent::tool_delta(
            "fixture-search-before",
            "search_text",
            initial_search_arguments,
        ),
        ProviderStreamEvent::tool_delta("fixture-patch", "apply_patch", patch_arguments),
        ProviderStreamEvent::tool_delta(
            "fixture-search-after",
            "search_text",
            final_search_arguments,
        ),
        ProviderStreamEvent::completed("tool_calls"),
    ]));
    let runtime = Arc::new(RuntimeService::new(
        Arc::clone(&store),
        provider,
        RuntimeConfig::default(),
    ));
    let request = RunRequest::new(
        SessionId::generate(),
        TaskId::generate(),
        InputId::generate(),
        "repair the parser fixture",
        workspace_observation,
    )
    .with_tool_schemas(coding_tool_schemas());
    let loop_service = CodingLoopService::new(
        Arc::clone(&runtime),
        ToolExecutionService::new(Arc::clone(&store)),
    );
    let result = loop_service
        .run_once(request, workspace, "cli.fixture", approve)
        .await;
    drop(loop_service);
    drop(runtime);
    Arc::try_unwrap(store)
        .map_err(|_| {
            HarnessError::new(
                ErrorCode::StorageWriteFailed,
                "coding fixture store consumers were not released",
            )
        })?
        .close()
        .await
        .map_err(store_error)?;
    let result = result?;
    let output = serde_json::json!({
        "schema_version": 1,
        "tool_contract_version": ToolExecutionService::contract_version(),
        "session_id": result.runtime.session_id,
        "task_id": result.runtime.task_id,
        "request_id": result.runtime.request_id,
        "packet_id": result.runtime.packet_id,
        "response": result.runtime.response,
        "provider_tool_call_count": result.runtime.tool_calls.len(),
        "approved": approve,
        "executions": result.executions,
        "capabilities": ToolExecutionService::capabilities(),
    });
    if json_output {
        println!("{output}");
    } else {
        println!(
            "coding fixture {} executed {} gated actions",
            output["session_id"],
            output["executions"].as_array().map_or(0, Vec::len)
        );
    }
    Ok(())
}

/// Synthetic workspace observation for the keyless runtime demo flows.
///
/// These commands (`ha runtime run|continue`) start a runtime against no real
/// workspace, so they carry a placeholder observation and its project identity is not
/// resolvable later. A flow that acts on a real root — the interactive app, a headless
/// turn, the delegation CLI, `ha code fixture` — resolves the durable identity instead.
fn demo_workspace() -> WorkspaceObservation {
    WorkspaceObservation {
        project_id: ProjectId::generate(),
        worktree_id: "cli-demo".to_owned(),
        base_commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        observed_fingerprint: ContentHash::from_bytes(b"cli-demo"),
    }
}

async fn run_runtime(
    data_dir: &PathBuf,
    text: &str,
    json_output: bool,
) -> Result<(), HarnessError> {
    let store = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(data_dir, HostId::generate()))
            .await
            .map_err(store_error)?,
    );
    let runtime = RuntimeService::new(
        Arc::clone(&store),
        Arc::new(MockProvider::text("mock response")),
        RuntimeConfig::default(),
    );
    let request = RunRequest::new(
        SessionId::generate(),
        TaskId::generate(),
        InputId::generate(),
        text,
        demo_workspace(),
    );
    let result = runtime
        .run(request)
        .await
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
    let output = serde_json::json!({"schema_version": 1, "session_id": result.session_id, "task_id": result.task_id, "request_id": result.request_id, "packet_id": result.packet_id, "response": result.response, "attempts": result.attempts});
    drop(runtime);
    Arc::try_unwrap(store)
        .map_err(|_| {
            HarnessError::new(
                ErrorCode::StorageWriteFailed,
                "runtime store consumers were not released",
            )
        })?
        .close()
        .await
        .map_err(store_error)?;
    if json_output {
        println!("{output}");
    } else {
        println!("run {} completed", output["session_id"]);
    }
    Ok(())
}

async fn resume_runtime(
    data_dir: &PathBuf,
    session_text: &str,
    json_output: bool,
) -> Result<(), HarnessError> {
    let session_id = SessionId::parse(session_text.to_owned())?;
    let store = Arc::new(
        SqliteStore::open_read_only(data_dir)
            .await
            .map_err(store_error)?,
    );
    let runtime = RuntimeService::new(
        Arc::clone(&store),
        Arc::new(MockProvider::text("must not dispatch")),
        RuntimeConfig::default(),
    );
    let report = runtime
        .resume(&session_id)
        .await
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
    let output = serde_json::json!({"schema_version": 1, "session_id": session_id, "blocked": report.blocked, "replayed_through_sequence": report.working_state.through_event_seq, "packet": report.packet});
    drop(runtime);
    Arc::try_unwrap(store)
        .map_err(|_| {
            HarnessError::new(
                ErrorCode::StorageWriteFailed,
                "runtime store consumers were not released",
            )
        })?
        .close()
        .await
        .map_err(store_error)?;
    if json_output {
        println!("{output}");
    } else {
        println!("session recovered");
    }
    Ok(())
}

async fn continue_runtime(
    data_dir: &PathBuf,
    task_text: &str,
    text: &str,
    json_output: bool,
) -> Result<(), HarnessError> {
    let task_id = TaskId::parse(task_text.to_owned())?;
    let store = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(data_dir, HostId::generate()))
            .await
            .map_err(store_error)?,
    );
    let source = store
        .list_sessions()
        .await
        .map_err(store_error)?
        .into_iter()
        .find(|summary| summary.task_id == task_id)
        .ok_or_else(|| HarnessError::new(ErrorCode::InvalidPayload, "task was not found"))?;
    let runtime = RuntimeService::new(
        Arc::clone(&store),
        Arc::new(MockProvider::text("mock continuation")),
        RuntimeConfig::default(),
    );
    let request = RunRequest::new(
        SessionId::generate(),
        task_id,
        InputId::generate(),
        text,
        demo_workspace(),
    );
    let result = runtime
        .continue_task(&source.session_id, request)
        .await
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
    let output = serde_json::json!({"schema_version": 1, "session_id": result.session_id, "task_id": result.task_id, "request_id": result.request_id, "packet_id": result.packet_id, "response": result.response});
    drop(runtime);
    Arc::try_unwrap(store)
        .map_err(|_| {
            HarnessError::new(
                ErrorCode::StorageWriteFailed,
                "runtime store consumers were not released",
            )
        })?
        .close()
        .await
        .map_err(store_error)?;
    if json_output {
        println!("{output}");
    } else {
        println!("task continued");
    }
    Ok(())
}

async fn inspect_context(
    data_dir: &PathBuf,
    session_text: &str,
    json_output: bool,
) -> Result<(), HarnessError> {
    let session_id = SessionId::parse(session_text.to_owned())?;
    let store = SqliteStore::open_read_only(data_dir)
        .await
        .map_err(store_error)?;
    let packets = store
        .list_context_packets(&session_id)
        .await
        .map_err(store_error)?;
    let output =
        serde_json::json!({"schema_version": 1, "session_id": session_id, "packets": packets});
    store.close().await.map_err(store_error)?;
    if json_output {
        println!("{output}");
    } else {
        println!(
            "context packets: {}",
            output["packets"].as_array().map_or(0, Vec::len)
        );
    }
    Ok(())
}

async fn replay_session(
    data_dir: &PathBuf,
    session_text: &str,
    offline: bool,
    json_output: bool,
) -> Result<(), HarnessError> {
    if !offline {
        return Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            "P2 replay requires --offline",
        ));
    }
    let session_id = SessionId::parse(session_text.to_owned())?;
    let store = Arc::new(
        SqliteStore::open_read_only(data_dir)
            .await
            .map_err(store_error)?,
    );
    let runtime = RuntimeService::new(
        Arc::clone(&store),
        Arc::new(MockProvider::text("must not dispatch")),
        RuntimeConfig::default(),
    );
    let report = runtime
        .offline_replay(&session_id)
        .await
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
    let output = serde_json::json!({"schema_version": 1, "session_id": session_id, "offline": true, "blocked": report.blocked, "dispatch_count": report.dispatch_count, "packets": report.packets, "requests": report.requests});
    drop(runtime);
    Arc::try_unwrap(store)
        .map_err(|_| {
            HarnessError::new(
                ErrorCode::StorageWriteFailed,
                "runtime store consumers were not released",
            )
        })?
        .close()
        .await
        .map_err(store_error)?;
    if json_output {
        println!("{output}");
    } else {
        println!("offline replay dispatches: {}", output["dispatch_count"]);
    }
    Ok(())
}

async fn init_store(data_dir: &PathBuf, json: bool) -> Result<(), HarnessError> {
    let store = SqliteStore::open_writer(WriterOpenOptions::new(data_dir, HostId::generate()))
        .await
        .map_err(store_error)?;
    let root_scope = ScopeId::generate();
    let manifests = built_in_manifests(root_scope);
    for manifest in &manifests {
        store
            .register_plugin_manifest(manifest.clone())
            .await
            .map_err(store_error)?;
    }
    let diagnostics = store.diagnostics().await.map_err(store_error)?;
    let generation = store.fence().map_err(store_error)?.generation;
    let instances = manifests
        .iter()
        .map(|manifest| manifest.instance_id.as_str())
        .collect::<Vec<_>>();
    store.close().await.map_err(store_error)?;

    let result = serde_json::json!({
        "schema_version": 1,
        "initialized": true,
        "data_dir": data_dir,
        "host_generation": generation,
        "plugin_instances": instances,
        "diagnostics": diagnostics_json(&diagnostics),
        "runtime": "not_available_in_p1"
    });
    if json {
        println!("{result}");
    } else {
        println!("initialized P1 data directory: {}", data_dir.display());
        println!("runtime: not_available_in_p1");
    }
    Ok(())
}

async fn list_sessions(data_dir: &PathBuf, json: bool) -> Result<(), HarnessError> {
    let store = SqliteStore::open_read_only(data_dir)
        .await
        .map_err(store_error)?;
    let sessions = store.list_sessions().await.map_err(store_error)?;
    let rows = sessions
        .iter()
        .map(|session| {
            serde_json::json!({
                "session_id": session.session_id,
                "task_id": session.task_id,
                "next_sequence": session.next_sequence,
                "input_count": session.input_count,
                "latest_snapshot_sequence": session.latest_snapshot_sequence
            })
        })
        .collect::<Vec<_>>();
    if json {
        println!(
            "{}",
            serde_json::json!({"schema_version": 1, "sessions": rows})
        );
    } else {
        println!("sessions: {}", rows.len());
        for row in rows {
            println!("{} {}", row["session_id"], row["task_id"]);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // one inspection report, told in order
async fn show_status(data_dir: &PathBuf, session_id: &str, json: bool) -> Result<(), HarnessError> {
    let session_id = harness_types::SessionId::parse(session_id.to_owned())?;
    let store = Arc::new(
        SqliteStore::open_read_only(data_dir)
            .await
            .map_err(store_error)?,
    );
    let summary = store
        .session_summary(&session_id)
        .await
        .map_err(store_error)?
        .ok_or_else(|| HarnessError::new(ErrorCode::InvalidPayload, "session was not found"))?;
    // Status is an inspection, not a run: a session whose journal cannot be
    // folded is reported as blocked with its typed reason instead of failing,
    // so an operator can still see what is durable and what is pending.
    let recovery = SessionService::new(Arc::clone(&store))
        .recover(&session_id)
        .await;
    let (recovery_json, blocking) = match &recovery {
        Ok(view) => {
            let completed = view
                .working_state
                .plan_items
                .iter()
                .filter(|item| item.status == harness_types::PlanItemStatus::Completed)
                .count();
            let pending = view
                .working_state
                .plan_items
                .iter()
                .filter(|item| item.status == harness_types::PlanItemStatus::Pending)
                .count();
            (
                serde_json::json!({
                    "snapshot_sequence": view.snapshot_sequence,
                    "replayed_through_sequence": view.replayed_through_sequence,
                    "receipt_count": view.receipts.len(),
                    "instruction_count": view.instruction_texts.len(),
                    "completed_plan_items": completed,
                    "pending_plan_items": pending,
                    "snapshot_diagnostic": view.snapshot_diagnostic
                }),
                serde_json::json!({"blocked": false, "reason": null}),
            )
        }
        Err(error) => (
            serde_json::Value::Null,
            serde_json::json!({"blocked": true, "reason": error.code().as_str()}),
        ),
    };
    let pending_work = serde_json::json!({
        "inbox_inputs": store.inbox_count(&session_id).await.map_err(store_error)?,
        "pending_tool_intents": store
            .pending_tool_intents(&session_id)
            .await
            .map_err(store_error)?
            .len(),
        "pending_runtime_commands": store
            .pending_runtime_commands(&session_id)
            .await
            .map_err(store_error)?,
    });
    // M3: the durable run of this session, when one exists. Run status and task
    // acceptance are separate fields; a reopened store must not read a pause as
    // success.
    let run = store
        .latest_run(&session_id)
        .await
        .map_err(store_error)?
        .map(|run| {
            serde_json::json!({
                "run_id": run.run_id,
                "state": run.state.as_str(),
                "stop_reason": run.stop_reason,
                "acceptance": run.acceptance,
                "revision": run.revision,
                "awaiting_question_id": run.awaiting_question_id,
            })
        });
    let result = serde_json::json!({
        "schema_version": 1,
        "session_id": summary.session_id,
        "task_id": summary.task_id,
        "next_sequence": summary.next_sequence,
        "input_count": summary.input_count,
        "latest_snapshot_sequence": summary.latest_snapshot_sequence,
        "recovery": recovery_json,
        "pending_work": pending_work,
        "blocking": blocking,
        "run": run,
        "runtime": "not_available_in_p1"
    });
    if json {
        println!("{result}");
    } else {
        match &recovery {
            Ok(view) => {
                println!(
                    "session {} recovered through seq {}",
                    session_id, view.replayed_through_sequence
                );
            }
            Err(error) => {
                println!(
                    "session {session_id} is blocked: {} ({})",
                    error.code(),
                    error
                );
            }
        }
        println!(
            "pending work: {} input(s), {} tool intent(s), {} command(s)",
            pending_work["inbox_inputs"],
            pending_work["pending_tool_intents"],
            pending_work["pending_runtime_commands"]
        );
        if let Some(run) = result.get("run").filter(|run| !run.is_null()) {
            println!(
                "run {} state {} stop {} acceptance {}",
                run["run_id"].as_str().unwrap_or_default(),
                run["state"].as_str().unwrap_or_default(),
                run["stop_reason"].as_str().unwrap_or("-"),
                run["acceptance"].as_str().unwrap_or("-"),
            );
        }
        println!("runtime: not_available_in_p1");
    }
    Ok(())
}

/// Durable human input commands (M3-02): ask, answer, list.
#[allow(clippy::too_many_lines)] // one subcommand arm per durable input action
async fn run_input_command(command: InputSubcommand) -> Result<(), HarnessError> {
    match command {
        InputSubcommand::Ask {
            data_dir,
            session_id,
            task_id,
            run_id,
            prompt,
            expires_at_unix_ms,
            scope_key,
            json,
        } => {
            let session_id = SessionId::parse(session_id)?;
            let task_id = TaskId::parse(task_id)?;
            let run_id = run_id.map(AgentRunId::parse).transpose()?;
            let scope_key = match scope_key {
                Some(scope_key) => scope_key,
                None => match &run_id {
                    Some(run_id) => {
                        harness_runtime::human_input::question_scope(run_id, "cli-question")
                    }
                    None => format!("{}|cli-question", session_id.as_str()),
                },
            };
            let store = open_writer_store(&data_dir).await?;
            let question = harness_runtime::HumanInputService::new(Arc::clone(&store))
                .ask(
                    harness_runtime::AskRequest {
                        session_id,
                        task_id,
                        run_id,
                        scope_key,
                        kind: "clarification".to_owned(),
                        prompt,
                        payload: serde_json::Value::Null,
                        expires_at_unix_ms,
                    },
                    harness_runtime::now_unix_ms(),
                )
                .await
                .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
            close_store(store).await?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "schema_version": 1,
                        "question_id": question.question_id,
                        "scope_key": question.scope_key,
                        "state": question.state.as_str(),
                    })
                );
            } else {
                println!(
                    "question {} [{}] scope {}",
                    question.question_id,
                    question.state.as_str(),
                    question.scope_key
                );
            }
            Ok(())
        }
        InputSubcommand::Answer {
            data_dir,
            scope_key,
            text,
            actor,
            json,
        } => {
            let store = open_writer_store(&data_dir).await?;
            let answer = serde_json::Value::String(text);
            let outcome = harness_runtime::HumanInputService::new(Arc::clone(&store))
                .answer(&scope_key, &answer, &actor, harness_runtime::now_unix_ms())
                .await
                .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
            close_store(store).await?;
            let (state, question_id) = match &outcome {
                harness_store_sqlite::QuestionOutcome::Answered(question)
                | harness_store_sqlite::QuestionOutcome::Duplicate(question) => {
                    ("answered", Some(question.question_id.as_str().to_owned()))
                }
                harness_store_sqlite::QuestionOutcome::Expired(question) => {
                    ("expired", Some(question.question_id.as_str().to_owned()))
                }
                harness_store_sqlite::QuestionOutcome::Conflict(question) => {
                    ("conflict", Some(question.question_id.as_str().to_owned()))
                }
                harness_store_sqlite::QuestionOutcome::NotFound => ("not_found", None),
            };
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "schema_version": 1,
                        "outcome": state,
                        "question_id": question_id,
                    })
                );
            } else {
                println!("answer {state}");
            }
            Ok(())
        }
        InputSubcommand::List {
            data_dir,
            session_id,
            json,
        } => {
            let session_id = SessionId::parse(session_id)?;
            let store = Arc::new(
                SqliteStore::open_read_only(&data_dir)
                    .await
                    .map_err(store_error)?,
            );
            let questions = harness_runtime::HumanInputService::new(Arc::clone(&store))
                .questions(&session_id)
                .await
                .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
            close_store(store).await?;
            let rows = questions
                .iter()
                .map(|question| {
                    serde_json::json!({
                        "question_id": question.question_id,
                        "scope_key": question.scope_key,
                        "kind": question.kind,
                        "state": question.state.as_str(),
                        "prompt": question.prompt,
                        "expires_at_unix_ms": question.expires_at_unix_ms,
                        "answered_by": question.answered_by,
                    })
                })
                .collect::<Vec<_>>();
            if json {
                println!(
                    "{}",
                    serde_json::json!({"schema_version": 1, "questions": rows})
                );
            } else {
                println!("questions: {}", rows.len());
                for row in rows {
                    println!(
                        "{} [{}] {}",
                        row["question_id"],
                        row["state"].as_str().unwrap_or_default(),
                        row["scope_key"].as_str().unwrap_or_default()
                    );
                }
            }
            Ok(())
        }
    }
}

/// Open the project store for one write and release it cleanly.
async fn open_writer_store(data_dir: &std::path::Path) -> Result<Arc<SqliteStore>, HarnessError> {
    Ok(Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(data_dir, HostId::generate()))
            .await
            .map_err(store_error)?,
    ))
}

async fn close_store(store: Arc<SqliteStore>) -> Result<(), HarnessError> {
    match Arc::try_unwrap(store) {
        Ok(store) => store.close().await.map_err(store_error),
        Err(_) => Err(HarnessError::new(
            ErrorCode::StorageWriteFailed,
            "store consumers were not released before close",
        )),
    }
}

async fn list_plugins(data_dir: &PathBuf, json: bool) -> Result<(), HarnessError> {
    let store = SqliteStore::open_read_only(data_dir)
        .await
        .map_err(store_error)?;
    let manifests = store.list_plugin_manifests().await.map_err(store_error)?;
    let rows = manifests
        .iter()
        .map(|entry| {
            serde_json::json!({
                "instance_id": entry.manifest.instance_id,
                "plugin_id": entry.manifest.plugin_id,
                "implementation_version": entry.manifest.implementation_version,
                "scope_id": entry.manifest.scope_id,
                "generation": entry.generation
            })
        })
        .collect::<Vec<_>>();
    if json {
        println!(
            "{}",
            serde_json::json!({"schema_version": 1, "plugins": rows})
        );
    } else {
        println!("plugins: {}", rows.len());
        for row in rows {
            println!("{} {}", row["instance_id"], row["plugin_id"]);
        }
    }
    Ok(())
}

async fn inspect_plugin(
    data_dir: &PathBuf,
    instance_id: &str,
    json: bool,
) -> Result<(), HarnessError> {
    let instance_id = PluginInstanceId::parse(instance_id.to_owned())?;
    let store = SqliteStore::open_read_only(data_dir)
        .await
        .map_err(store_error)?;
    let entry = store
        .plugin_manifest(&instance_id)
        .await
        .map_err(store_error)?
        .ok_or_else(|| {
            HarnessError::new(ErrorCode::InvalidPayload, "plugin instance was not found")
        })?;
    let result = serde_json::json!({
        "schema_version": 1,
        "generation": entry.generation,
        "manifest": entry.manifest,
        "runtime": "not_loaded_in_p1"
    });
    if json {
        println!("{result}");
    } else {
        println!("plugin {instance_id} is persisted but not loaded in P1");
    }
    Ok(())
}

fn built_in_manifests(scope_id: ScopeId) -> Vec<PluginManifest> {
    vec![
        PluginManifest {
            schema_version: 1,
            plugin_id: "kernel.local".to_owned(),
            implementation_version: env!("CARGO_PKG_VERSION").to_owned(),
            instance_id: PluginInstanceId::generate(),
            scope_id: scope_id.clone(),
            host_api_version: 1,
            config_schema_version: 1,
            provides: vec![ServiceContract {
                service_id: "kernel_lifecycle".to_owned(),
                api_version: 1,
            }],
            requires: Vec::new(),
            capabilities: vec!["lifecycle".to_owned(), "scoped_registry".to_owned()],
            blocks_recovery_when_absent: true,
        },
        PluginManifest {
            schema_version: 1,
            plugin_id: "store.sqlite".to_owned(),
            implementation_version: env!("CARGO_PKG_VERSION").to_owned(),
            instance_id: PluginInstanceId::generate(),
            scope_id,
            host_api_version: 1,
            config_schema_version: 1,
            provides: vec![ServiceContract {
                service_id: "session_store".to_owned(),
                api_version: 1,
            }],
            requires: vec![ServiceContract {
                service_id: "kernel_lifecycle".to_owned(),
                api_version: 1,
            }],
            capabilities: vec!["durable_storage".to_owned(), "recovery".to_owned()],
            blocks_recovery_when_absent: true,
        },
    ]
}

fn diagnostics_json(diagnostics: &StoreDiagnostics) -> serde_json::Value {
    serde_json::json!({
        "foreign_keys_enabled": diagnostics.foreign_keys_enabled,
        "journal_mode": diagnostics.journal_mode,
        "synchronous": diagnostics.synchronous,
        "busy_timeout_ms": diagnostics.busy_timeout_ms
    })
}

fn store_error(error: StoreError) -> HarnessError {
    error.into_harness_error()
}
