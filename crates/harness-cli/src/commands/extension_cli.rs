//! P6 CLI: extension trust inspection, local registration and skill inspection.
//!
//! Reading a manifest, a repository profile or a skill directory is inert: no
//! executable is started and no secret is resolved. An explicit trust grant is
//! required before anything runs, and the local registration path always reports
//! which grants are still missing.

use std::{path::Path, path::PathBuf, sync::Arc};

use clap::{Args, Subcommand};
use harness_extensions::{
    CatalogEntry, CatalogSource, ConfigInspection, ConfigLayer, ConfigLayerKind,
    EnvironmentOverrides, ExtensionCapability, ExtensionRuntime, McpSupportMatrix, SkillCatalog,
    SkillSource, ToolCatalog, TrustGrant, TrustedSkillRoot, executable_digest, explain_config,
    host::read_manifest, supported_host_methods, supported_protocol_versions,
};
use harness_kernel::ScopedRegistry;
use harness_types::{ErrorCode, HarnessError, ScopeId};
use serde_json::json;

/// Everything the `ha extensions` command group can do.
#[derive(Debug, Args)]
pub struct ExtensionCommand {
    #[command(subcommand)]
    command: ExtensionSubcommand,
}

#[derive(Debug, Subcommand)]
enum ExtensionSubcommand {
    /// Inspect an extension manifest without starting it or resolving a secret.
    Inspect {
        /// Path to the extension manifest JSON.
        #[arg(long)]
        manifest: PathBuf,
        /// Emit a versioned JSON result to stdout.
        #[arg(long)]
        json: bool,
    },
    /// Report the host protocol, host methods and capabilities this build supports.
    Capabilities {
        #[arg(long)]
        json: bool,
    },
    /// Register a local extension from a manifest and an explicit trust grant.
    ///
    /// Without a grant the command reports exactly which grants are missing and
    /// starts nothing.
    Register {
        /// Path to the extension manifest JSON.
        #[arg(long)]
        manifest: PathBuf,
        /// The executable the manifest pins.
        #[arg(long)]
        executable: PathBuf,
        /// Trust file naming the digest and allowed capabilities, as JSON.
        #[arg(long)]
        trust: Option<PathBuf>,
        /// Capability to allow, repeatable. Only used when building a grant.
        #[arg(long = "allow-capability")]
        allow_capability: Vec<String>,
        /// Secret reference to allow, repeatable. Only used when building a grant.
        #[arg(long = "allow-secret")]
        allow_secret: Vec<String>,
        /// Principal recorded as the grantor.
        #[arg(long, default_value = "local-user")]
        granted_by: String,
        /// Actually start the extension. Without this the command only reports.
        #[arg(long, default_value_t = false)]
        confirm: bool,
        #[arg(long)]
        json: bool,
    },
    /// Inspect skills in a directory without granting them anything.
    Skills {
        #[arg(long)]
        directory: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// List the authorized tool catalogue, or promote one bounded definition.
    ///
    /// A listing exposes names and metadata only. A definition is promoted on
    /// request, for one entry the caller is authorized to see, and the promoted
    /// definition is bound to the catalogue digest and policy revision that
    /// produced it.
    Catalog {
        /// Path to a JSON file describing the catalogue revision.
        #[arg(long)]
        entries: PathBuf,
        /// Promote one entry's definition instead of listing.
        #[arg(long)]
        promote: Option<String>,
        /// The policy revision the promotion is bound to.
        #[arg(long, default_value_t = 1)]
        policy_revision: u64,
        #[arg(long)]
        json: bool,
    },
    /// Explain which configuration layer won, and when a change takes effect.
    ///
    /// Reading configuration starts nothing and resolves no secret.
    ConfigExplain {
        /// Path to a JSON file describing the configuration layers.
        #[arg(long)]
        layers: PathBuf,
        #[arg(long)]
        json: bool,
    },
}

/// Run one `ha extensions` subcommand.
pub async fn run(command: ExtensionCommand) -> Result<(), HarnessError> {
    match command.command {
        ExtensionSubcommand::Inspect { manifest, json } => inspect(&manifest, json),
        ExtensionSubcommand::Capabilities { json } => {
            capabilities(json);
            Ok(())
        }
        ExtensionSubcommand::Register {
            manifest,
            executable,
            trust,
            allow_capability,
            allow_secret,
            granted_by,
            confirm,
            json,
        } => {
            register(
                &manifest,
                &executable,
                trust.as_deref(),
                &allow_capability,
                &allow_secret,
                &granted_by,
                confirm,
                json,
            )
            .await
        }
        ExtensionSubcommand::Skills { directory, json } => skills(&directory, json),
        ExtensionSubcommand::Catalog {
            entries,
            promote,
            policy_revision,
            json,
        } => catalog(&entries, promote.as_deref(), policy_revision, json),
        ExtensionSubcommand::ConfigExplain { layers, json } => config_explain(&layers, json),
    }
}

fn inspect(manifest_path: &Path, json_output: bool) -> Result<(), HarnessError> {
    let manifest = read_manifest(manifest_path)
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
    let inspection = ConfigInspection::from_manifest(&manifest);
    let output = json!({
        "schema_version": 1,
        "plugin_id": inspection.plugin_id,
        "implementation_version": manifest.implementation_version,
        "protocol_versions": supported_protocol_versions(),
        "config_schema_version": manifest.config_schema_version,
        "host_api_version": manifest.host_api_version,
        "executable_digest": inspection.executable_digest,
        "requires_user_trust": inspection.requires_user_trust,
        "requested_capabilities": inspection.requested_capabilities.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
        "requested_host_methods": inspection.requested_host_methods,
        "requested_secrets": inspection.requested_secrets,
        "requested_environment": manifest.requested_environment,
        "restart_policy": serde_json::to_value(manifest.restart_policy).unwrap_or(json!("unknown")),
        "notes": inspection.notes,
        "started": false,
    });
    if json_output {
        println!("{output}");
    } else {
        println!(
            "plugin {} v{} (protocol {:?}) requires explicit user trust; nothing was started",
            output["plugin_id"],
            output["implementation_version"],
            supported_protocol_versions()
        );
        for note in output["notes"].as_array().into_iter().flatten() {
            println!("note: {}", note.as_str().unwrap_or_default());
        }
    }
    Ok(())
}

fn capabilities(json_output: bool) {
    let supported = McpSupportMatrix::supported();
    let unsupported = McpSupportMatrix::unsupported();
    let output = json!({
        "schema_version": 1,
        "extension_protocol_versions": supported_protocol_versions(),
        "host_methods": supported_host_methods(),
        "capabilities": [
            ExtensionCapability::Tools.as_str(),
            ExtensionCapability::ModelProvider.as_str(),
            ExtensionCapability::MemoryExtractor.as_str(),
            ExtensionCapability::Skills.as_str(),
        ],
        "mcp": {
            "spec_revision": McpSupportMatrix::spec_revision(),
            "sdk_version": McpSupportMatrix::sdk_version(),
            "supported": supported.iter().map(|feature| feature.as_str()).collect::<Vec<_>>(),
            "unsupported": unsupported
                .iter()
                .map(|(feature, reason)| json!({
                    "feature": feature.as_str(),
                    "reason": reason,
                }))
                .collect::<Vec<_>>(),
        },
        "unsupported": [
            "marketplace",
            "wasm",
            "native_dynamic_loading",
            "remote_mcp_endpoints",
            "external_loop_plugins",
            "external_storage_plugins",
        ],
        "isolation_note": "bounded stdio transport with a minimal environment; this is not OS sandboxing",
    });
    if json_output {
        println!("{output}");
    } else {
        println!(
            "extension protocol {:?}; {} allowlisted host methods; MCP spec {} via rmcp {}; this is not OS sandboxing",
            supported_protocol_versions(),
            supported_host_methods().len(),
            McpSupportMatrix::spec_revision(),
            McpSupportMatrix::sdk_version()
        );
        for (feature, reason) in unsupported {
            println!("unsupported: {} - {reason}", feature.as_str());
        }
    }
}

/// One catalogue entry as the CLI accepts it.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogEntryInput {
    id: String,
    /// `builtin`, `extension` or `mcp`.
    source: String,
    #[serde(default)]
    server: Option<String>,
    #[serde(default)]
    plugin_id: Option<String>,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    revision: u32,
    #[serde(default)]
    read_only: bool,
    schema: serde_json::Value,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogFile {
    schema_version: u16,
    revision: u64,
    #[serde(default)]
    held_capabilities: Vec<String>,
    entries: Vec<CatalogEntryInput>,
}

fn catalog(
    path: &Path,
    promote: Option<&str>,
    policy_revision: u64,
    json_output: bool,
) -> Result<(), HarnessError> {
    let file = read_catalog_file(path)?;
    let catalog = ToolCatalog::build(file.revision, catalog_entries(&file.entries)?)
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
    if let Some(id) = promote {
        return promote_one(
            &catalog,
            id,
            policy_revision,
            &file.held_capabilities,
            json_output,
        );
    }
    list_catalog(&catalog, &file.held_capabilities, json_output);
    Ok(())
}

fn read_catalog_file(path: &Path) -> Result<CatalogFile, HarnessError> {
    let bytes = std::fs::read(path).map_err(|error| {
        HarnessError::new(
            ErrorCode::ConfigReadError,
            format!("cannot read the catalogue: {error}"),
        )
    })?;
    let file: CatalogFile = serde_json::from_slice(&bytes).map_err(|_| {
        HarnessError::new(
            ErrorCode::ConfigParseError,
            "the catalogue file is not a valid catalogue",
        )
    })?;
    if file.schema_version != 1 {
        return Err(HarnessError::new(
            ErrorCode::UnsupportedSchemaVersion,
            format!("catalogue schema {} is not supported", file.schema_version),
        ));
    }
    Ok(file)
}

fn catalog_entries(inputs: &[CatalogEntryInput]) -> Result<Vec<CatalogEntry>, HarnessError> {
    let mut entries = Vec::new();
    for input in inputs {
        let source = match input.source.as_str() {
            "builtin" => CatalogSource::Builtin,
            "mcp" => CatalogSource::Mcp {
                server: input.server.clone().unwrap_or_else(|| "unknown".to_owned()),
            },
            "extension" => CatalogSource::Extension {
                plugin_id: input
                    .plugin_id
                    .clone()
                    .unwrap_or_else(|| "unknown".to_owned()),
            },
            other => {
                return Err(HarnessError::new(
                    ErrorCode::InvalidPayload,
                    format!("catalogue source {other} is not supported"),
                ));
            }
        };
        let effect_class = if input.read_only {
            harness_tools::EffectClass::ReadOnly
        } else {
            harness_tools::EffectClass::External
        };
        // A caller may state the providing process's revision; otherwise the
        // catalogue records the contract revision this build speaks.
        let revision = if input.revision == 0 {
            u32::from(harness_tools::TOOL_CONTRACT_VERSION)
        } else {
            input.revision
        };
        entries.push(
            CatalogEntry::new(
                input.id.clone(),
                input.id.clone(),
                source,
                revision,
                input.schema.clone(),
                effect_class,
                input.capabilities.clone(),
            )
            .map_err(|error| HarnessError::new(error.code(), error.to_string()))?
            .with_summary(input.summary.clone()),
        );
    }
    Ok(entries)
}

fn promote_one(
    catalog: &ToolCatalog,
    id: &str,
    policy_revision: u64,
    held: &[String],
    json_output: bool,
) -> Result<(), HarnessError> {
    let promoted = catalog
        .promote(id, catalog.catalog_digest(), policy_revision, held)
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
    let output = json!({
        "schema_version": 1,
        "id": promoted.id,
        "tool_name": promoted.tool_name,
        "source": promoted.source.label(),
        "catalog_revision": promoted.catalog_revision,
        "catalog_digest": promoted.catalog_digest,
        "schema_digest": promoted.schema_digest,
        "policy_revision": promoted.policy_revision,
        "effect_class": promoted.effect_class.as_str(),
        "schema": promoted.schema,
    });
    if json_output {
        println!("{output}");
    } else {
        println!(
            "promoted {} from catalogue revision {} (policy revision {})",
            output["id"], output["catalog_revision"], output["policy_revision"]
        );
    }
    Ok(())
}

fn list_catalog(catalog: &ToolCatalog, held: &[String], json_output: bool) {
    let visible = catalog.list_authorized(held);
    let visible_ids = visible
        .iter()
        .map(|view| view.id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let withheld = catalog
        .entries()
        .iter()
        .filter(|entry| !visible_ids.contains(&entry.id))
        .map(|entry| {
            json!({
                "id": entry.id,
                "source": entry.source.label(),
                "required_capabilities": entry.capabilities,
            })
        })
        .collect::<Vec<_>>();
    let output = json!({
        "schema_version": 1,
        "revision": catalog.revision(),
        "catalog_digest": catalog.catalog_digest(),
        "held_capabilities": held,
        "visible": visible
            .iter()
            .map(|view| json!({
                "id": view.id,
                "source": view.source,
                "revision": view.revision,
                "schema_digest": view.schema_digest,
                "effect_class": view.effect_class,
                "summary": view.summary,
                "promoted": view.promoted,
                "revoked": view.revoked,
            }))
            .collect::<Vec<_>>(),
        "withheld": withheld,
        "note": "a listing exposes names and metadata; a schema is promoted on request and grants nothing",
    });
    if json_output {
        println!("{output}");
    } else {
        println!(
            "catalogue revision {}: {} nameable, {} withheld",
            output["revision"],
            visible.len(),
            output["withheld"].as_array().map_or(0, std::vec::Vec::len)
        );
        for view in &visible {
            println!("  {} ({}) - {}", view.id, view.source, view.summary);
        }
    }
}

/// One configuration layer as the CLI accepts it.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigLayerInput {
    kind: String,
    origin: String,
    #[serde(default)]
    values: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigLayersFile {
    schema_version: u16,
    layers: Vec<ConfigLayerInput>,
}

fn read_layer_file(path: &Path) -> Result<Vec<ConfigLayer>, HarnessError> {
    let bytes = std::fs::read(path).map_err(|error| {
        HarnessError::new(
            ErrorCode::ConfigReadError,
            format!("cannot read the configuration layers: {error}"),
        )
    })?;
    let file: ConfigLayersFile = serde_json::from_slice(&bytes).map_err(|_| {
        HarnessError::new(
            ErrorCode::ConfigParseError,
            "the layer file is not a valid layer list",
        )
    })?;
    if file.schema_version != 1 {
        return Err(HarnessError::new(
            ErrorCode::UnsupportedSchemaVersion,
            format!("layer schema {} is not supported", file.schema_version),
        ));
    }
    let mut layers = Vec::new();
    for input in file.layers {
        let kind = match input.kind.as_str() {
            "builtin" => ConfigLayerKind::Builtin,
            "user" => ConfigLayerKind::User,
            "trusted_project" => ConfigLayerKind::TrustedProject,
            "profile" => ConfigLayerKind::Profile,
            "cli_override" => ConfigLayerKind::CliOverride,
            other => {
                return Err(HarnessError::new(
                    ErrorCode::InvalidPayload,
                    format!("configuration layer {other} is not supported"),
                ));
            }
        };
        let mut layer = ConfigLayer::new(kind, input.origin);
        layer.values = input.values;
        layers.push(layer);
    }
    Ok(layers)
}

fn config_explain(path: &Path, json_output: bool) -> Result<(), HarnessError> {
    let layers = read_layer_file(path)?;
    // Explaining configuration reports no live plugin: the CLI invocation owns
    // no extension process, so claiming one would be a lie.
    let explain = explain_config(&layers, &[])
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
    let supported = McpSupportMatrix::supported();
    let output = json!({
        "schema_version": 1,
        "config_digest": explain.config_digest,
        "reload_boundary": explain.reload_boundary.as_str(),
        "restart_sensitive_keys": explain.restart_sensitive_keys(),
        "effective": explain
            .effective
            .iter()
            .map(|entry| json!({
                "key": entry.key,
                "value": entry.value,
                "winner": entry.winner.as_str(),
                "winner_origin": entry.winner_origin,
                "overridden": entry
                    .overridden
                    .iter()
                    .map(|(kind, value)| json!({"layer": kind.as_str(), "value": value}))
                    .collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>(),
        "active_plugins": explain.active_plugins,
        "inactive_plugins": explain
            .inactive
            .iter()
            .map(|entry| json!({
                "plugin_id": entry.plugin_id,
                "state": entry.state,
                "reason": entry.reason,
                "generation": entry.generation,
            }))
            .collect::<Vec<_>>(),
        "mcp": {
            "spec_revision": McpSupportMatrix::spec_revision(),
            "sdk_version": McpSupportMatrix::sdk_version(),
            "supported": supported.iter().map(|feature| feature.as_str()).collect::<Vec<_>>(),
        },
        "note": "explaining configuration resolves no secret and starts no process",
    });
    if json_output {
        println!("{output}");
    } else {
        println!(
            "config {} (reload boundary: {})",
            explain.config_digest.as_str(),
            explain.reload_boundary.as_str()
        );
        for entry in &explain.effective {
            println!(
                "  {} = {} ({}{})",
                entry.key,
                entry.value,
                entry.winner.as_str(),
                if entry.is_contested() {
                    format!(", {} overridden", entry.overridden.len())
                } else {
                    String::new()
                }
            );
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)] // One registration path.
async fn register(
    manifest_path: &Path,
    executable: &PathBuf,
    trust_path: Option<&std::path::Path>,
    allow_capability: &[String],
    allow_secret: &[String],
    granted_by: &str,
    confirm: bool,
    json_output: bool,
) -> Result<(), HarnessError> {
    let manifest = read_manifest(manifest_path)
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
    let actual = executable_digest(executable)
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;

    let grant = match trust_path {
        Some(path) => {
            let bytes = std::fs::read(path).map_err(|error| {
                HarnessError::new(
                    ErrorCode::ConfigReadError,
                    format!("cannot read the trust file: {error}"),
                )
            })?;
            let grant: TrustGrant = serde_json::from_slice(&bytes).map_err(|_| {
                HarnessError::new(ErrorCode::ConfigParseError, "trust file is not valid JSON")
            })?;
            grant
        }
        None => TrustGrant {
            plugin_id: manifest.plugin_id.clone(),
            executable_digest: actual.clone(),
            allowed_capabilities: allow_capability
                .iter()
                .filter_map(|value| ExtensionCapability::parse(value))
                .collect(),
            allowed_secrets: allow_secret.to_vec(),
            granted_by: granted_by.to_owned(),
        },
    };

    if !confirm {
        // Report the exact gap and start nothing.
        let missing_capabilities = manifest
            .provides
            .iter()
            .map(|offer| offer.capability)
            .filter(|capability| !grant.allows(*capability))
            .map(ExtensionCapability::as_str)
            .collect::<Vec<_>>();
        let missing_secrets = manifest
            .requested_secrets
            .iter()
            .filter(|reference| !grant.allows_secret(reference))
            .cloned()
            .collect::<Vec<_>>();
        let output = json!({
            "schema_version": 1,
            "plugin_id": manifest.plugin_id,
            "executable_digest": actual,
            "granted_by": grant.granted_by,
            "missing_capabilities": missing_capabilities,
            "missing_secrets": missing_secrets,
            "started": false,
            "next_action": "re-run with --confirm once every missing grant is supplied",
        });
        if json_output {
            println!("{output}");
        } else {
            println!(
                "dry run: {} capabilities and {} secrets still ungranted; nothing was started",
                output["missing_capabilities"]
                    .as_array()
                    .map_or(0, std::vec::Vec::len),
                output["missing_secrets"]
                    .as_array()
                    .map_or(0, std::vec::Vec::len)
            );
        }
        return Ok(());
    }

    let scope_id = ScopeId::generate();
    let mut registry = ScopedRegistry::default();
    registry
        .add_root(scope_id.clone())
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
    let runtime = Arc::new(ExtensionRuntime::new(scope_id.clone(), registry));
    let reported_scope = scope_id.clone();
    let outcome = runtime
        .load(
            executable,
            manifest,
            Some(&grant),
            EnvironmentOverrides::new(),
        )
        .await;
    let output = match &outcome {
        harness_extensions::LoadOutcome::Active {
            plugin_id,
            generation,
            capabilities,
            host_methods,
        } => json!({
            "schema_version": 1,
            "plugin_id": plugin_id,
            "state": "active",
            "generation": generation,
            "capabilities": capabilities.iter().map(|offer| offer.capability.as_str()).collect::<Vec<_>>(),
            "host_methods": host_methods,
            "scope_id": reported_scope,
        }),
        harness_extensions::LoadOutcome::Inactive {
            plugin_id,
            reason,
            detail,
        } => json!({
            "schema_version": 1,
            "plugin_id": plugin_id,
            "state": "inactive",
            "reason": reason.as_str(),
            "detail": detail,
        }),
    };
    // The process is not left running for a CLI invocation.
    runtime.shutdown_all().await;
    if json_output {
        println!("{output}");
    } else {
        println!("extension {} is {}", output["plugin_id"], output["state"]);
    }
    Ok(())
}

fn skills(directory: &Path, json_output: bool) -> Result<(), HarnessError> {
    // The M6 catalogue reads metadata only: listing a skill directory must not
    // hold a document in memory, and it must never execute anything it finds.
    let catalog = SkillCatalog::discover(&[TrustedSkillRoot::new(
        directory,
        SkillSource::TrustedProject,
    )])
    .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
    // A repository skill grants nothing: report what it asks for, resolve none.
    let output = json!({
        "schema_version": 1,
        "directory": directory,
        "catalog_digest": catalog.catalog_digest(),
        "skill_count": catalog.entries().len(),
        "skills": catalog.entries().iter().map(|entry| json!({
            "skill_id": entry.skill_id,
            "name": entry.name,
            "version": entry.version,
            "digest": entry.digest,
            "source": entry.source.as_str(),
            "byte_len": entry.byte_len,
            "requested_tools": entry.requested_tools,
            "requested_secrets": entry.requested_secrets,
        })).collect::<Vec<_>>(),
        "conflicts": catalog.conflicts().iter().map(|conflict| json!({
            "conflict_id": conflict.conflict_id,
            "name": conflict.name,
            "lower": conflict.loser_source.as_str(),
            "higher": conflict.winner_source.as_str(),
        })).collect::<Vec<_>>(),
        "grants_resolved": 0,
        "content_loaded": false,
        "note": "skill content is data; it cannot grant tool or secret authority",
    });
    if json_output {
        println!("{output}");
    } else {
        println!(
            "{} skills discovered in {}; no content was read and no grants were resolved",
            output["skill_count"], output["directory"]
        );
        for skill in output["skills"].as_array().into_iter().flatten() {
            println!(
                "{} v{} ({}) requests tools {:?}",
                skill["name"], skill["version"], skill["source"], skill["requested_tools"]
            );
        }
    }
    Ok(())
}
