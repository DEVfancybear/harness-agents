//! P6 CLI: extension trust inspection, local registration and skill inspection.
//!
//! Reading a manifest, a repository profile or a skill directory is inert: no
//! executable is started and no secret is resolved. An explicit trust grant is
//! required before anything runs, and the local registration path always reports
//! which grants are still missing.

use std::{path::Path, path::PathBuf, sync::Arc};

use clap::{Args, Subcommand};
use harness_extensions::{
    ConfigInspection, EnvironmentOverrides, ExtensionCapability, ExtensionRuntime, SkillDescriptor,
    SkillSource, TrustGrant, compose_skills, discover_skills, executable_digest,
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
            "extension protocol {:?}; {} allowlisted host methods; this is not OS sandboxing",
            supported_protocol_versions(),
            supported_host_methods().len()
        );
    }
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
    let discovered = discover_skills(directory, SkillSource::TrustedProject)
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
    // A repository skill grants nothing: report what it asks for, resolve none.
    let (composed, replaced) = compose_skills(&discovered);
    let output = json!({
        "schema_version": 1,
        "directory": directory,
        "skill_count": composed.len(),
        "skills": composed.iter().map(skill_json).collect::<Vec<_>>(),
        "replaced": replaced.iter().map(|(name, loser, winner)| json!({
            "name": name,
            "lower": loser.as_str(),
            "higher": winner.as_str(),
        })).collect::<Vec<_>>(),
        "grants_resolved": 0,
        "note": "skill content is data; it cannot grant tool or secret authority",
    });
    if json_output {
        println!("{output}");
    } else {
        println!(
            "{} skills discovered in {}; no grants were resolved",
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

fn skill_json(skill: &SkillDescriptor) -> serde_json::Value {
    json!({
        "skill_id": skill.skill_id,
        "name": skill.name,
        "version": skill.version,
        "digest": skill.digest,
        "source": skill.source.as_str(),
        "requested_tools": skill.requested_tools,
        "requested_secrets": skill.requested_secrets,
    })
}
