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
    /// List the backend leases this data directory has not settled.
    Leases {
        /// Local `SQLite` data directory owned by this harness.
        #[arg(long)]
        data_dir: PathBuf,
        /// Only leases whose owner has not written a heartbeat since this many
        /// milliseconds ago. Defaults to the lease grace window.
        #[arg(long)]
        older_than_ms: Option<u64>,
        /// Emit the leases as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Settle the leases whose owner is provably gone.
    ///
    /// A lease whose owner still holds its lock is reported and left untouched;
    /// nothing here deletes a resource that has a live owner.
    Reconcile {
        /// Local `SQLite` data directory owned by this harness.
        #[arg(long)]
        data_dir: PathBuf,
        /// Grace window before a lease is even a candidate.
        #[arg(long)]
        grace_ms: Option<u64>,
        /// Emit the report as JSON.
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
        SandboxSubcommand::Leases {
            data_dir,
            older_than_ms,
            json,
        } => leases(&data_dir, older_than_ms, json).await,
        SandboxSubcommand::Reconcile {
            data_dir,
            grace_ms,
            json,
        } => reconcile(&data_dir, grace_ms, json).await,
    }
}

/// List the leases nobody has settled. This is a read: it cannot change an
/// owner's state.
async fn leases(
    data_dir: &Path,
    older_than_ms: Option<u64>,
    json_output: bool,
) -> Result<(), HarnessError> {
    let store = harness_store_sqlite::SqliteStore::open_read_only(data_dir.to_owned())
        .await
        .map_err(harness_store_sqlite::StoreError::into_harness_error)?;
    let grace = older_than_ms.unwrap_or(harness_tools::LEASE_GRACE_MS);
    let cutoff = now_unix_ms().saturating_sub(grace);
    let rows = store
        .unsettled_backend_leases(i64::try_from(cutoff).unwrap_or(i64::MAX))
        .await
        .map_err(harness_store_sqlite::StoreError::into_harness_error)?;
    if json_output {
        let value = serde_json::json!({
            "schema_version": 1,
            "grace_ms": grace,
            "unsettled": rows.iter().map(|lease| serde_json::json!({
                "lease_id": lease.lease_id,
                "state": lease.state,
                "profile": lease.profile,
                "backend": lease.backend,
                "tool_execution_id": lease.tool_execution_id,
                "lock_path": lease.lock_path,
                "heartbeat_at_unix_ms": lease.heartbeat_at_unix_ms,
                "owner_generation": lease.owner_generation,
            })).collect::<Vec<_>>(),
        });
        println!("{value}");
        return Ok(());
    }
    println!("{} unsettled lease(s) older than {grace} ms", rows.len());
    for lease in rows {
        println!(
            "{} {} profile={} execution={} lock={}",
            lease.lease_id, lease.state, lease.profile, lease.tool_execution_id, lease.lock_path
        );
    }
    Ok(())
}

/// Settle what can be proven orphaned. This writes, so it needs the writer
/// fence: reconciliation is a change to durable state, not a report.
async fn reconcile(
    data_dir: &Path,
    grace_ms: Option<u64>,
    json_output: bool,
) -> Result<(), HarnessError> {
    let store = harness_store_sqlite::SqliteStore::open_writer(
        harness_store_sqlite::WriterOpenOptions::new(
            data_dir.to_owned(),
            harness_types::HostId::generate(),
        ),
    )
    .await
    .map_err(harness_store_sqlite::StoreError::into_harness_error)?;
    let grace = grace_ms.unwrap_or(harness_tools::LEASE_GRACE_MS);
    let now = now_unix_ms();
    let report =
        harness_tools::reconcile_backend_leases(&store, now.saturating_sub(grace), now).await?;
    store
        .close()
        .await
        .map_err(harness_store_sqlite::StoreError::into_harness_error)?;
    if json_output {
        let value = serde_json::to_value(&report).map_err(|_| {
            HarnessError::new(ErrorCode::InvalidPayload, "report is not serializable")
        })?;
        println!("{value}");
    } else {
        println!(
            "recovered {} · still owned {} · already settled {}",
            report.recovered.len(),
            report.still_owned.len(),
            report.already_settled.len()
        );
        for lease_id in &report.still_owned {
            println!("still owned (untouched): {lease_id}");
        }
    }
    Ok(())
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
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
