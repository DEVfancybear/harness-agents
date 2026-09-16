//! P7 CLI: doctor, backup, restore, retention and garbage collection.
//!
//! Every command is explicit about what it did and what it refused. A restore
//! never activates on its own, a forget requires a confirmation equal to its
//! target, and a garbage collection reports exactly which artifacts it kept and
//! why.

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Args, Subcommand};
use harness_maintenance::{
    CapabilityStatus, CapabilitySupport, DEFAULT_GC_GRACE_SECONDS, PlatformStatus, PlatformSupport,
    ReleaseMatrix, RetentionAction, check_store_compatibility, collect_garbage, create_backup,
    forget_source, list_tombstones, migrate_copy, restore_backup, retention_summary, run_retention,
    verify_backup,
};
use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
use harness_types::{ErrorCode, HarnessError, HostId};
use serde_json::json;

/// Everything the `ha maintenance` command group can do.
#[derive(Debug, Args)]
pub struct MaintenanceCommand {
    #[command(subcommand)]
    command: MaintenanceSubcommand,
}

#[derive(Debug, Subcommand)]
enum MaintenanceSubcommand {
    /// Report store health, schema revisions, retention state and limitations.
    Doctor {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Take a consistent backup of a data directory into a new directory.
    Backup {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        into: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Validate a backup without restoring it.
    VerifyBackup {
        #[arg(long)]
        backup: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Restore a backup into a new data directory. This never activates it.
    Restore {
        #[arg(long)]
        backup: PathBuf,
        #[arg(long)]
        into: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Apply a retention action. Forget requires --confirm equal to the target.
    Retain {
        #[arg(long)]
        data_dir: PathBuf,
        /// invalidate | archive | forget
        #[arg(long)]
        action: String,
        #[arg(long)]
        source_kind: String,
        #[arg(long)]
        source_id: String,
        #[arg(long)]
        reason: String,
        /// Explicit confirmation token; must equal --source-id for forget.
        #[arg(long)]
        confirm: Option<String>,
        /// A copy that may still contain the data, repeatable.
        #[arg(long = "surviving-copy")]
        surviving_copy: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Report tombstones and which copies may still hold forgotten data.
    Tombstones {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Collect unreferenced artifacts after the grace period.
    Gc {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long, default_value_t = DEFAULT_GC_GRACE_SECONDS)]
        grace_seconds: u64,
        #[arg(long, default_value_t = true)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
    /// Migrate a store into a new directory, leaving the source untouched.
    MigrateCopy {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        into: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Print the release matrix: platforms, capabilities and benchmark honesty.
    ReleaseMatrix {
        /// Measured retrieval latency in milliseconds, if a benchmark was run.
        #[arg(long)]
        retrieval_p95_ms: Option<u64>,
        /// Measured restore time in milliseconds, if a benchmark was run.
        #[arg(long)]
        restore_ms: Option<u64>,
        #[arg(long)]
        json: bool,
    },
}

/// Run one `ha maintenance` subcommand.
pub async fn run(command: MaintenanceCommand) -> Result<(), HarnessError> {
    match command.command {
        MaintenanceSubcommand::Doctor { data_dir, json } => doctor(&data_dir, json).await,
        MaintenanceSubcommand::Backup {
            data_dir,
            into,
            json,
        } => backup(&data_dir, &into, json).await,
        MaintenanceSubcommand::VerifyBackup { backup, json } => verify(&backup, json).await,
        MaintenanceSubcommand::Restore { backup, into, json } => {
            restore(&backup, &into, json).await
        }
        MaintenanceSubcommand::Retain {
            data_dir,
            action,
            source_kind,
            source_id,
            reason,
            confirm,
            surviving_copy,
            json,
        } => {
            retain(
                &data_dir,
                &action,
                &source_kind,
                &source_id,
                &reason,
                confirm.as_deref(),
                &surviving_copy,
                json,
            )
            .await
        }
        MaintenanceSubcommand::Tombstones { data_dir, json } => tombstones(&data_dir, json).await,
        MaintenanceSubcommand::Gc {
            data_dir,
            grace_seconds,
            dry_run,
            json,
        } => gc(&data_dir, grace_seconds, dry_run, json).await,
        MaintenanceSubcommand::MigrateCopy {
            data_dir,
            into,
            json,
        } => migrate(&data_dir, &into, json).await,
        MaintenanceSubcommand::ReleaseMatrix {
            retrieval_p95_ms,
            restore_ms,
            json,
        } => {
            let matrix = release_matrix(retrieval_p95_ms, restore_ms);
            matrix
                .validate()
                .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
            let verdict = matrix.verdict();
            let output = json!({
                "schema_version": 1,
                "release_name": matrix.release_name,
                "verdict": verdict,
                "platforms": matrix.platforms.iter().map(|platform| json!({
                    "os": platform.os,
                    "target_triple": platform.target_triple,
                    "toolchain": platform.toolchain,
                    "status": platform.status.as_str(),
                    "evidence": platform.evidence,
                })).collect::<Vec<_>>(),
                "capabilities": matrix.capabilities.iter().map(|capability| json!({
                    "name": capability.name,
                    "status": capability.status.as_str(),
                    "note": capability.note,
                })).collect::<Vec<_>>(),
                "benchmarks": matrix.benchmarks.iter().map(|benchmark| json!({
                    "name": benchmark.name,
                    "unit": benchmark.unit,
                    "target": benchmark.target,
                    "measured": benchmark.measured,
                    "met": benchmark.met(),
                })).collect::<Vec<_>>(),
                "verified_cases": matrix.verified_cases.len(),
                "unverified_checks": matrix.unverified_checks,
                "out_of_scope": matrix.out_of_scope,
            });
            if json {
                println!("{output}");
            } else {
                println!("release {}: {}", output["release_name"], output["verdict"]);
                for platform in output["platforms"].as_array().into_iter().flatten() {
                    println!(
                        "  {} {} {}",
                        platform["target_triple"].as_str().unwrap_or_default(),
                        platform["status"].as_str().unwrap_or_default(),
                        platform["evidence"].as_str().unwrap_or_default()
                    );
                }
            }
            Ok(())
        }
    }
}

async fn doctor(data_dir: &PathBuf, json_output: bool) -> Result<(), HarnessError> {
    let compatibility = check_store_compatibility(data_dir)
        .await
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
    let store = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(data_dir, HostId::generate()))
            .await
            .map_err(|error| store_error(&error))?,
    );
    let diagnostics = store
        .diagnostics()
        .await
        .map_err(|error| store_error(&error))?;
    let retention = retention_summary(&store)
        .await
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
    let sessions = store
        .list_sessions()
        .await
        .map_err(|error| store_error(&error))?;
    let revisions = store
        .all_schema_revisions()
        .await
        .map_err(|error| store_error(&error))?;
    let artifacts = store
        .artifact_pins()
        .await
        .map_err(|error| store_error(&error))?;
    let tasks = store
        .list_task_nodes()
        .await
        .map_err(|error| store_error(&error))?;
    close_store(store).await?;

    let output = json!({
        "schema_version": 1,
        "data_dir": data_dir,
        "compatibility": compatibility.describe(),
        "writable": compatibility.is_writable(),
        "inspection_allowed": compatibility.inspection_allowed(),
        "sqlite": {
            "foreign_keys": diagnostics.foreign_keys_enabled,
            "journal_mode": diagnostics.journal_mode,
            "synchronous": diagnostics.synchronous,
            "busy_timeout_ms": diagnostics.busy_timeout_ms,
        },
        "schema_revisions": revisions,
        "sessions": sessions.len(),
        "delegated_tasks": tasks.len(),
        "artifacts": artifacts.len(),
        "retention": retention,
        "not_verified": [
            "live provider credentials were not exercised",
            "no daemon or background service is provided",
            "remote backup targets are out of scope",
        ],
    });
    if json_output {
        println!("{output}");
    } else {
        println!(
            "data dir {} is {}",
            output["data_dir"], output["compatibility"]
        );
        println!(
            "{} sessions, {} delegated tasks, {} artifacts",
            output["sessions"], output["delegated_tasks"], output["artifacts"]
        );
        for item in output["not_verified"].as_array().into_iter().flatten() {
            println!("not verified: {}", item.as_str().unwrap_or_default());
        }
    }
    Ok(())
}

async fn backup(data_dir: &PathBuf, into: &PathBuf, json_output: bool) -> Result<(), HarnessError> {
    let outcome = create_backup(data_dir, into)
        .await
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
    let output = json!({
        "schema_version": 1,
        "backup_dir": outcome.backup_dir,
        "database_hash": outcome.database_hash,
        "artifact_count": outcome.artifact_count,
        "total_artifact_bytes": outcome.total_artifact_bytes,
        "pinned": outcome.pinned,
        "tombstones": outcome.tombstones,
        "manifest_hash": outcome.manifest_hash,
    });
    if json_output {
        println!("{output}");
    } else {
        println!(
            "backup written to {} with {} artifacts",
            output["backup_dir"], output["artifact_count"]
        );
    }
    Ok(())
}

async fn verify(backup_dir: &PathBuf, json_output: bool) -> Result<(), HarnessError> {
    let manifest = verify_backup(backup_dir)
        .await
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
    let output = json!({
        "schema_version": 1,
        "valid": true,
        "source_data_dir": manifest.source_data_dir,
        "created_unix_ms": manifest.created_unix_ms,
        "artifacts": manifest.artifacts.len(),
        "tombstones": manifest.tombstones.len(),
        "schema_revisions": manifest.schema_revisions,
        "manifest_hash": manifest.manifest_hash,
    });
    if json_output {
        println!("{output}");
    } else {
        println!("backup is valid: {} artifacts", output["artifacts"]);
    }
    Ok(())
}

async fn restore(
    backup_dir: &PathBuf,
    into: &PathBuf,
    json_output: bool,
) -> Result<(), HarnessError> {
    let outcome = restore_backup(backup_dir, into)
        .await
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
    let output = json!({
        "schema_version": 1,
        "restored_into": outcome.report.restored_into,
        "database_verified": outcome.report.database_verified,
        "artifacts_verified": outcome.report.artifacts_verified,
        "tombstones_restored": outcome.report.tombstones_restored,
        "activated": outcome.report.activated,
        "next_action": "inspect the restored directory, then activate it explicitly if it is correct",
    });
    if json_output {
        println!("{output}");
    } else {
        println!(
            "restored into {} ({} artifacts verified, not activated)",
            output["restored_into"], output["artifacts_verified"]
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)] // One retention call; every field is operator input.
async fn retain(
    data_dir: &PathBuf,
    action: &str,
    source_kind: &str,
    source_id: &str,
    reason: &str,
    confirm: Option<&str>,
    surviving_copies: &[String],
    json_output: bool,
) -> Result<(), HarnessError> {
    let action = match action {
        "invalidate" => RetentionAction::Invalidate,
        "archive" => RetentionAction::Archive,
        "forget" => RetentionAction::Forget,
        other => {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                format!("unknown retention action {other}"),
            ));
        }
    };
    let store = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(data_dir, HostId::generate()))
            .await
            .map_err(|error| store_error(&error))?,
    );
    let report = if action == RetentionAction::Forget {
        forget_source(
            &store,
            source_kind,
            source_id,
            reason,
            confirm.unwrap_or_default(),
            surviving_copies,
        )
        .await
    } else {
        run_retention(
            &store,
            action,
            source_kind,
            source_id,
            reason,
            confirm,
            surviving_copies,
        )
        .await
    }
    .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
    close_store(store).await?;

    let output = json!({
        "schema_version": 1,
        "action": report.action.as_str(),
        "target": report.target,
        "affected_assets": report.affected_assets.len(),
        "derived_invalidated": report.derived_invalidated,
        "tombstone_id": report.tombstone_id,
        "surviving_copies": report.surviving_copies,
        "note": "invalidate keeps history; only forget removes content and records a tombstone",
    });
    if json_output {
        println!("{output}");
    } else {
        println!(
            "{} applied to {}; {} derived records invalidated",
            output["action"], output["target"], output["derived_invalidated"]
        );
    }
    Ok(())
}

async fn tombstones(data_dir: &PathBuf, json_output: bool) -> Result<(), HarnessError> {
    let store = Arc::new(
        SqliteStore::open_read_only(data_dir)
            .await
            .map_err(|error| store_error(&error))?,
    );
    let records = list_tombstones(&store)
        .await
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
    let output = json!({
        "schema_version": 1,
        "tombstones": records.iter().map(|record| json!({
            "tombstone_id": record.tombstone_id,
            "source_kind": record.source_kind,
            "source_id": record.source_id,
            "reason": record.reason,
            "surviving_copies": record.surviving_copies,
        })).collect::<Vec<_>>(),
    });
    drop(store);
    if json_output {
        println!("{output}");
    } else {
        println!(
            "{} tombstones",
            output["tombstones"]
                .as_array()
                .map_or(0, std::vec::Vec::len)
        );
    }
    Ok(())
}

async fn gc(
    data_dir: &PathBuf,
    grace_seconds: u64,
    dry_run: bool,
    json_output: bool,
) -> Result<(), HarnessError> {
    let store = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(data_dir, HostId::generate()))
            .await
            .map_err(|error| store_error(&error))?,
    );
    let report = collect_garbage(&store, grace_seconds, dry_run)
        .await
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
    close_store(store).await?;
    let output = json!({
        "schema_version": 1,
        "dry_run": dry_run,
        "grace_seconds": grace_seconds,
        "summary": report.summary(),
        "considered": report.considered,
        "collected": report.collected,
        "retained_pinned": report.retained_pinned,
        "retained_referenced": report.retained_referenced,
        "retained_young": report.retained_young,
        "bytes_reclaimed": report.bytes_reclaimed,
    });
    if json_output {
        println!("{output}");
    } else {
        println!("{}", output["summary"]);
    }
    Ok(())
}

async fn migrate(
    data_dir: &PathBuf,
    into: &PathBuf,
    json_output: bool,
) -> Result<(), HarnessError> {
    let outcome = migrate_copy(data_dir, into)
        .await
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
    let output = json!({
        "schema_version": 1,
        "source": outcome.source,
        "destination": outcome.destination,
        "migrated": outcome.migrated,
        "revisions": outcome.revisions,
        "note": "the source directory was not modified",
    });
    if json_output {
        println!("{output}");
    } else {
        println!("migrated a copy into {}", output["destination"]);
    }
    Ok(())
}

/// The release matrix this build actually supports.
///
/// Platform status is recorded from real runs; a platform that was not exercised
/// is `unverified`, never silently omitted.
#[must_use]
pub fn release_matrix(retrieval_p95_ms: Option<u64>, restore_ms: Option<u64>) -> ReleaseMatrix {
    ReleaseMatrix {
        schema_version: harness_maintenance::MAINTENANCE_CONTRACT_VERSION,
        release_name: format!("harness-cli {}", env!("CARGO_PKG_VERSION")),
        platforms: vec![
            PlatformSupport {
                os: "windows".to_owned(),
                target_triple: "x86_64-pc-windows-msvc".to_owned(),
                toolchain: "1.97.1".to_owned(),
                status: PlatformStatus::Verified,
                evidence:
                    "P0-P7 gates run locally on this platform; CI matrix run recorded in the evidence"
                        .to_owned(),
            },
            PlatformSupport {
                os: "linux".to_owned(),
                target_triple: "x86_64-unknown-linux-gnu".to_owned(),
                toolchain: "1.97.1".to_owned(),
                status: PlatformStatus::Verified,
                evidence:
                    "P0-P7 gates run in the GitHub Actions ubuntu-latest job recorded in the evidence"
                        .to_owned(),
            },
        ],
        capabilities: vec![
            capability("coding_tools", CapabilityStatus::Supported, "policy gate with receipts"),
            capability("memory", CapabilityStatus::Supported, "scoped assets with FTS retrieval"),
            capability("delegation", CapabilityStatus::Supported, "task DAG with isolated worktrees"),
            capability("extensions", CapabilityStatus::Supported, "trusted stdio plugins and local MCP"),
            capability("backup_restore", CapabilityStatus::Supported, "consistent snapshot into a new directory"),
            capability("retention", CapabilityStatus::Supported, "invalidate, archive, forget with tombstones"),
            capability(
                "packaged_linux_artifact",
                CapabilityStatus::ComponentOnly,
                "built on Linux in CI, but no release artifact was published",
            ),
            capability(
                "remote_mcp_endpoints",
                CapabilityStatus::Unsupported,
                "needs an explicit trust and network policy",
            ),
            capability(
                "os_sandboxing",
                CapabilityStatus::Unsupported,
                "transport isolation is not a sandbox",
            ),
            capability(
                "background_daemon",
                CapabilityStatus::Unsupported,
                "work stops when the host exits",
            ),
        ],
        benchmarks: vec![
            harness_maintenance::BenchmarkTarget {
                name: "retrieval_p95".to_owned(),
                description: "scoped memory retrieval p95 over the recorded dataset".to_owned(),
                unit: "ms".to_owned(),
                target: 250,
                measured: retrieval_p95_ms,
            },
            harness_maintenance::BenchmarkTarget {
                name: "restore_state".to_owned(),
                description: "restore a session state from a snapshot".to_owned(),
                unit: "ms".to_owned(),
                target: 2000,
                measured: restore_ms,
            },
        ],
        verified_cases: verified_case_ids(),
        unverified_checks: vec![
            "live provider evaluation: no credential was supplied".to_owned(),
            "mutation testing, coverage and advisory audit: tooling is not installed".to_owned(),
            "published release artifacts: none were produced or published".to_owned(),
        ],
        out_of_scope: vec![
            "P8 Web UI".to_owned(),
            "daemon or background execution after CLI exit".to_owned(),
            "marketplace, Wasm or arbitrary native dynamic loading".to_owned(),
            "production host activation".to_owned(),
        ],
    }
}

fn capability(name: &str, status: CapabilityStatus, note: &str) -> CapabilitySupport {
    CapabilitySupport {
        name: name.to_owned(),
        status,
        note: note.to_owned(),
    }
}

/// The 44 continuity and plugin cases this release claims to exercise.
#[must_use]
pub fn verified_case_ids() -> Vec<String> {
    let mut cases = (1..=30)
        .map(|index| format!("C{index:02}"))
        .collect::<Vec<_>>();
    cases.extend((1..=14).map(|index| format!("K{index:02}")));
    cases
}

#[allow(clippy::needless_pass_by_value)] // The Arc must be consumed to unwrap it.
async fn close_store(store: Arc<SqliteStore>) -> Result<(), HarnessError> {
    Arc::try_unwrap(store)
        .map_err(|_| {
            HarnessError::new(
                ErrorCode::StorageWriteFailed,
                "maintenance store consumers were not released",
            )
        })?
        .close()
        .await
        .map_err(|error| store_error(&error))
}

fn store_error(error: &harness_store_sqlite::StoreError) -> HarnessError {
    HarnessError::new(error.code(), error.to_string())
}
