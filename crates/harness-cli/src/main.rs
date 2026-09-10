#![forbid(unsafe_code)]

use std::{fs, path::PathBuf, process::ExitCode, sync::Arc};

use clap::{Args, Parser, Subcommand};
use harness_session::SessionService;
use harness_store_sqlite::{SqliteStore, StoreDiagnostics, StoreError, WriterOpenOptions};
use harness_types::{
    ErrorCode, HarnessConfig, HarnessError, HostId, PluginInstanceId, PluginManifest, ScopeId,
    ServiceContract,
};

/// Personal coding-agent harness.
///
/// P1 exposes durable metadata inspection only. It does not start a model,
/// tool, agent, memory-extraction, or multi-agent runtime.
#[derive(Debug, Parser)]
#[command(name = "ha", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize a local P1 `SQLite` data directory and inspectable built-in metadata.
    Init {
        /// Directory owned by this local harness store.
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
    /// List or inspect persisted plugin metadata without loading a plugin.
    Plugins(PluginsCommand),
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
    /// Explain effective P0 configuration and the source of every value.
    Explain {
        /// Path to the TOML configuration file.
        #[arg(long)]
        config: PathBuf,
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
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}

async fn run(cli: Cli) -> Result<(), HarnessError> {
    match cli.command {
        Some(Command::Init { data_dir, json }) => init_store(&data_dir, json).await,
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
        Some(Command::Config(ConfigCommand {
            command: ConfigSubcommand::Explain { config, json },
        })) => {
            let effective = read_config(&config)?;
            let result = serde_json::json!({
                "schema_version": 1,
                "effective_config": effective,
                "sources": {
                    "schema_version": "file",
                    "cli.output": "file_or_default"
                },
                "runtime": "not_available_in_p1"
            });
            if json {
                println!("{result}");
            } else {
                println!("config source: {}", config.display());
                println!("runtime: not_available_in_p1");
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
        None => Ok(()),
    }
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
    let recovery = SessionService::new(Arc::clone(&store))
        .recover(&session_id)
        .await
        .map_err(store_error)?;
    let completed = recovery
        .working_state
        .plan_items
        .iter()
        .filter(|item| item.status == harness_types::PlanItemStatus::Completed)
        .count();
    let pending = recovery
        .working_state
        .plan_items
        .iter()
        .filter(|item| item.status == harness_types::PlanItemStatus::Pending)
        .count();
    let result = serde_json::json!({
        "schema_version": 1,
        "session_id": summary.session_id,
        "task_id": summary.task_id,
        "next_sequence": summary.next_sequence,
        "input_count": summary.input_count,
        "latest_snapshot_sequence": summary.latest_snapshot_sequence,
        "recovery": {
            "snapshot_sequence": recovery.snapshot_sequence,
            "replayed_through_sequence": recovery.replayed_through_sequence,
            "receipt_count": recovery.receipts.len(),
            "instruction_count": recovery.instruction_texts.len(),
            "completed_plan_items": completed,
            "pending_plan_items": pending,
            "snapshot_diagnostic": recovery.snapshot_diagnostic
        },
        "runtime": "not_available_in_p1"
    });
    if json {
        println!("{result}");
    } else {
        println!(
            "session {} recovered through seq {}",
            session_id, recovery.replayed_through_sequence
        );
        println!("runtime: not_available_in_p1");
    }
    Ok(())
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
