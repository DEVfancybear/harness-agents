//! `ha sandbox` — the M12 measurement surface.
//!
//! It exists because "what can this host enforce?" must be answerable with
//! evidence instead of with a claim. The command runs the real probes, prints
//! the resulting matrix (verdicts *and* the observation behind each verdict),
//! and can be turned into a gate with `--require`, which refuses with the same
//! typed error a strict tool request would get.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use harness_tools::{CapabilityProbe, CapabilityVerdict, ProbeChild, StrictProfile};
use harness_types::{ErrorCode, HarnessError};

#[derive(Debug, Args)]
pub struct SandboxCommand {
    #[command(subcommand)]
    command: SandboxSubcommand,
}

#[derive(Debug, Subcommand)]
enum SandboxSubcommand {
    /// Measure what this host can enforce and print the capability matrix.
    Probe {
        /// The M12 probe fixture (`m12_probe_child`) the measurements drive.
        #[arg(long)]
        probe_child: PathBuf,
        /// Directory the probes may write to. Defaults to a fresh directory
        /// under the OS temp directory, removed when the probe finishes.
        #[arg(long)]
        root: Option<PathBuf>,
        /// Refuse to exit zero unless this profile is enforceable here.
        #[arg(long, value_name = "containment|full")]
        require: Option<String>,
        /// Emit the versioned capability matrix as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Export a published capture with its digest, without copying anything else.
    Export {
        /// Local `SQLite` data directory owned by this harness.
        #[arg(long)]
        data_dir: PathBuf,
        /// The artifact id a receipt points at.
        #[arg(long)]
        artifact_id: String,
        /// Directory the bytes are written into; the name is derived from the id.
        #[arg(long)]
        to: PathBuf,
        /// Emit the versioned export record as JSON.
        #[arg(long)]
        json: bool,
    },
}

pub async fn run(command: SandboxCommand) -> Result<(), HarnessError> {
    match command.command {
        SandboxSubcommand::Probe {
            probe_child,
            root,
            require,
            json,
        } => probe(&probe_child, root, require.as_deref(), json).await,
        SandboxSubcommand::Export {
            data_dir,
            artifact_id,
            to,
            json,
        } => export(&data_dir, &artifact_id, &to, json).await,
    }
}

/// Export one artifact. The store is opened read-only: an export must not be
/// able to become a writer, and a daemon that owns the data directory must not
/// have to stop for a copy to happen.
async fn export(
    data_dir: &Path,
    artifact_id: &str,
    to: &Path,
    json_output: bool,
) -> Result<(), HarnessError> {
    let store = harness_store_sqlite::SqliteStore::open_read_only(data_dir.to_owned())
        .await
        .map_err(harness_store_sqlite::StoreError::into_harness_error)?;
    let record = harness_tools::export_artifact(
        &store,
        artifact_id,
        to,
        None,
        harness_tools::PROCESS_OUTPUT_PAGE_MAX_BYTES as usize,
    )
    .await?;
    if json_output {
        let value = serde_json::to_value(&record).map_err(|_| {
            HarnessError::new(
                ErrorCode::InvalidPayload,
                "export record is not serializable",
            )
        })?;
        println!("{value}");
    } else {
        println!(
            "exported {} ({} bytes, {}) to {}",
            record.artifact_id,
            record.byte_len,
            record.exported_digest,
            to.join(&record.relative_path).display()
        );
    }
    Ok(())
}

async fn probe(
    probe_child: &Path,
    root: Option<PathBuf>,
    require: Option<&str>,
    json_output: bool,
) -> Result<(), HarnessError> {
    if !probe_child.is_file() {
        return Err(HarnessError::new(
            ErrorCode::ServiceUnavailable,
            format!(
                "the probe fixture {} does not exist; the capability matrix would have no measurement",
                probe_child.display()
            ),
        ));
    }
    let required = require.map(parse_profile).transpose()?;
    let disposable = root.is_none();
    let root = root.unwrap_or_else(|| {
        std::env::temp_dir().join(format!("ha-sandbox-probe-{}", std::process::id()))
    });
    let outcome = measure(&root, probe_child, required, json_output).await;
    if disposable {
        let _ = std::fs::remove_dir_all(&root);
    }
    outcome
}

async fn measure(
    root: &Path,
    probe_child: &Path,
    required: Option<StrictProfile>,
    json_output: bool,
) -> Result<(), HarnessError> {
    let probe = CapabilityProbe::new(
        root.to_owned(),
        ProbeChild::new(probe_child.to_owned(), Vec::new()),
    );
    let matrix = probe.run().await?;
    if let Some(profile) = required {
        // The gate form: the same refusal a strict tool request gets, with the
        // same code, so a script cannot mistake "unsupported" for "fine".
        if let Some(refusal) = profile.refusal(&matrix) {
            return Err(refusal);
        }
    }
    if json_output {
        let value = serde_json::to_value(&matrix).map_err(|_| {
            HarnessError::new(
                ErrorCode::InvalidPayload,
                "capability matrix is not serializable",
            )
        })?;
        println!("{value}");
        return Ok(());
    }
    println!(
        "host: {} {} {} · backend {} {}",
        matrix.host.os,
        matrix.host.os_version,
        matrix.host.arch,
        matrix.host.backend,
        matrix.host.backend_version
    );
    for finding in &matrix.findings {
        println!(
            "{:>11} {} — {}",
            match finding.verdict {
                CapabilityVerdict::Enforced => "enforced",
                CapabilityVerdict::Unsupported => "unsupported",
            },
            finding.capability.as_str(),
            finding.evidence.observation
        );
    }
    for profile in [StrictProfile::Containment, StrictProfile::Full] {
        match profile.refusal(&matrix) {
            None => println!("profile {}: enforceable here", profile.as_str()),
            Some(refusal) => println!(
                "profile {}: refused — {}",
                profile.as_str(),
                refusal.message()
            ),
        }
    }
    Ok(())
}

fn parse_profile(text: &str) -> Result<StrictProfile, HarnessError> {
    match text {
        "containment" => Ok(StrictProfile::Containment),
        "full" => Ok(StrictProfile::Full),
        other => Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            format!("unknown strict profile {other}; expected containment or full"),
        )),
    }
}
