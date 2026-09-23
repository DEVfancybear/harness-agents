//! User-configured MCP server management.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use harness_types::{ErrorCode, HarnessConfigV2, HarnessError, McpServerConfigV2};

#[derive(Debug, Args)]
pub struct McpCommand {
    #[command(subcommand)]
    command: McpSubcommand,
}

#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)] // clap stores a single command's parsed options together.
enum McpSubcommand {
    /// Add a stdio or Streamable HTTP MCP server to config.
    Add {
        name: String,
        #[arg(long)]
        command: Option<String>,
        #[arg(long = "arg")]
        args: Vec<String>,
        #[arg(long = "env", value_name = "KEY=VALUE")]
        env: Vec<String>,
        #[arg(long)]
        cwd: Option<String>,
        #[arg(long = "enabled-tool")]
        enabled_tools: Vec<String>,
        #[arg(long = "disabled-tool")]
        disabled_tools: Vec<String>,
        #[arg(long)]
        tool_timeout_seconds: Option<u64>,
        #[arg(long)]
        required: bool,
        #[arg(long, value_parser = ["stdio", "streamable_http"])]
        transport: Option<String>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        bearer_token_env: Option<String>,
        #[arg(long)]
        project: bool,
    },
    /// List configured MCP servers.
    List {
        #[arg(long)]
        project: bool,
        #[arg(long)]
        json: bool,
    },
    /// Show one server's non-secret configuration.
    Get {
        name: String,
        #[arg(long)]
        project: bool,
        #[arg(long)]
        json: bool,
    },
    /// Remove one configured server.
    Remove {
        name: String,
        #[arg(long)]
        project: bool,
    },
}

#[allow(clippy::too_many_lines)]
pub fn run(command: McpCommand) -> Result<(), HarnessError> {
    match command.command {
        McpSubcommand::Add {
            name,
            command,
            args,
            env,
            cwd,
            enabled_tools,
            disabled_tools,
            tool_timeout_seconds,
            required,
            transport,
            url,
            bearer_token_env,
            project,
        } => {
            let mut environment = std::collections::BTreeMap::new();
            for value in env {
                let (key, value) = value.split_once('=').ok_or_else(|| {
                    HarnessError::new(
                        ErrorCode::ConfigParseError,
                        "--env values must use KEY=VALUE; secrets must use secret://ENV_NAME",
                    )
                })?;
                if environment
                    .insert(key.to_owned(), value.to_owned())
                    .is_some()
                {
                    return Err(HarnessError::new(
                        ErrorCode::ConfigParseError,
                        format!("--env repeats key {key}"),
                    ));
                }
            }
            let config = McpServerConfigV2 {
                transport,
                command,
                args,
                env: environment,
                cwd,
                enabled_tools,
                disabled_tools,
                tool_timeout_seconds,
                required,
                url,
                bearer_token_env,
            };
            config.validate(&name)?;
            mutate_server(&name, config, project, false)?;
            println!("MCP server {name} added to {} config", layer_name(project));
            Ok(())
        }
        McpSubcommand::List { project, json } => {
            let table = read_servers(project)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&table).unwrap_or_default()
                );
            } else if table.is_empty() {
                println!("no MCP servers configured");
            } else {
                for (name, config) in table {
                    println!(
                        "{name}: transport={}, required={}, command={}, tools={}",
                        config.transport.as_deref().unwrap_or("stdio"),
                        config.required,
                        config
                            .command
                            .as_deref()
                            .or(config.url.as_deref())
                            .unwrap_or("(unset)"),
                        if config.enabled_tools.is_empty() {
                            "all except disabled"
                        } else {
                            "enabled list"
                        },
                    );
                }
            }
            Ok(())
        }
        McpSubcommand::Get {
            name,
            project,
            json,
        } => {
            let table = read_servers(project)?;
            let config = table.get(&name).ok_or_else(|| {
                HarnessError::new(
                    ErrorCode::ExtensionNotFound,
                    format!("MCP server {name} is not configured"),
                )
            })?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(config).unwrap_or_default()
                );
            } else {
                println!("{}", toml::to_string_pretty(config).unwrap_or_default());
            }
            Ok(())
        }
        McpSubcommand::Remove { name, project } => {
            mutate_server(&name, McpServerConfigV2::default(), project, true)?;
            println!(
                "MCP server {name} removed from {} config",
                layer_name(project)
            );
            Ok(())
        }
    }
}

fn layer_name(project: bool) -> &'static str {
    if project { "project" } else { "user" }
}

fn config_path(project: bool) -> Result<PathBuf, HarnessError> {
    if project {
        let cwd = std::env::current_dir().map_err(|error| {
            HarnessError::new(
                ErrorCode::ConfigReadError,
                format!("current directory unavailable: {error}"),
            )
        })?;
        Ok(cwd.join(".harness").join("config.toml"))
    } else {
        let environment = crate::interactive::paths::LaunchEnvironment::capture();
        crate::interactive::paths::resolve(&crate::interactive::paths::PathRequest {
            platform: crate::interactive::paths::HostPlatform::current(),
            environment: &environment,
            explicit_data_dir: None,
        })
        .map(|paths| paths.config_file)
    }
}

fn read_document(path: &Path) -> Result<toml::Value, HarnessError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut table = toml::map::Map::new();
            table.insert("schema_version".to_owned(), toml::Value::Integer(2));
            return Ok(toml::Value::Table(table));
        }
        Err(error) => {
            return Err(HarnessError::new(
                ErrorCode::ConfigReadError,
                format!("{} could not be read: {error}", path.display()),
            ));
        }
    };
    toml::from_str(&text).map_err(|error| {
        HarnessError::new(
            ErrorCode::ConfigParseError,
            format!("{} is invalid TOML: {error}", path.display()),
        )
    })
}

fn read_servers(
    project: bool,
) -> Result<std::collections::BTreeMap<String, McpServerConfigV2>, HarnessError> {
    let path = config_path(project)?;
    let document = read_document(&path)?;
    let version = document
        .get("schema_version")
        .and_then(toml::Value::as_integer)
        .unwrap_or(2);
    if version != 2 {
        return Err(HarnessError::new(
            ErrorCode::ConfigParseError,
            "ha mcp requires a schema_version = 2 config before MCP server entries can be edited",
        ));
    }
    let config: HarnessConfigV2 = document.try_into().map_err(|error: toml::de::Error| {
        HarnessError::new(
            ErrorCode::ConfigParseError,
            format!("MCP config is invalid: {error}"),
        )
    })?;
    config.validate()?;
    Ok(config.mcp_servers)
}

fn mutate_server(
    name: &str,
    config: McpServerConfigV2,
    project: bool,
    remove: bool,
) -> Result<(), HarnessError> {
    if !remove {
        config.validate(name)?;
    }
    let path = config_path(project)?;
    let mut document = read_document(&path)?;
    let root = document.as_table_mut().ok_or_else(|| {
        HarnessError::new(
            ErrorCode::ConfigParseError,
            "config root must be a TOML table",
        )
    })?;
    root.insert("schema_version".to_owned(), toml::Value::Integer(2));
    let servers = root
        .entry("mcp_servers".to_owned())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| {
            HarnessError::new(
                ErrorCode::ConfigParseError,
                "mcp_servers must be a TOML table",
            )
        })?;
    if remove {
        if servers.remove(name).is_none() {
            return Err(HarnessError::new(
                ErrorCode::ExtensionNotFound,
                format!("MCP server {name} is not configured"),
            ));
        }
    } else {
        if servers.contains_key(name) {
            return Err(HarnessError::new(
                ErrorCode::DuplicateRegistration,
                format!("MCP server {name} is already configured"),
            ));
        }
        servers.insert(
            name.to_owned(),
            toml::Value::try_from(config).map_err(|error| {
                HarnessError::new(
                    ErrorCode::ConfigParseError,
                    format!("MCP config cannot be serialized: {error}"),
                )
            })?,
        );
    }
    let validated: HarnessConfigV2 =
        document
            .clone()
            .try_into()
            .map_err(|error: toml::de::Error| {
                HarnessError::new(
                    ErrorCode::ConfigParseError,
                    format!("config cannot be upgraded safely: {error}"),
                )
            })?;
    validated.validate()?;
    let output = toml::to_string_pretty(&document).map_err(|error| {
        HarnessError::new(
            ErrorCode::ConfigParseError,
            format!("config cannot be serialized: {error}"),
        )
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            HarnessError::new(
                ErrorCode::ConfigReadError,
                format!("{} cannot be created: {error}", parent.display()),
            )
        })?;
    }
    let temporary = path.with_extension("toml.tmp");
    std::fs::write(&temporary, output).map_err(|error| {
        HarnessError::new(
            ErrorCode::ConfigReadError,
            format!("{} cannot be written: {error}", temporary.display()),
        )
    })?;
    std::fs::rename(&temporary, &path).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        HarnessError::new(
            ErrorCode::ConfigReadError,
            format!("{} cannot be updated: {error}", path.display()),
        )
    })
}
