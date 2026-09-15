use std::{path::PathBuf, sync::Arc};

use clap::{Args, Subcommand, ValueEnum};
use harness_memory::{
    ExtractionOutput, ExtractionStrategy, MemoryBudget, MemoryExtractor, MemoryPrincipal,
    MemoryService, SourceProjection, WriteMemoryVersion,
};
use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
use harness_types::{
    AgentProfileId, ContentHash, ErrorCode, HarnessError, HostId, MemoryAssetId, ProjectId,
    SessionId, SourceAuthority, TaskId, Validity,
};
use serde_json::{Value, json};

#[derive(Debug, Args)]
pub struct MemoryCommand {
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    /// Local host identity; never populated from extractor/model arguments.
    #[arg(long, global = true, default_value = "local-user")]
    principal: String,
    #[arg(long, global = true)]
    project_id: Option<String>,
    #[arg(long, global = true)]
    task_id: Option<String>,
    #[arg(long, global = true)]
    profile_id: Option<String>,
    #[arg(long, global = true)]
    session_id: Option<String>,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: MemoryAction,
}

#[derive(Debug, Subcommand)]
enum MemoryAction {
    Search {
        query: String,
        #[arg(long, default_value_t = 8)]
        limit: usize,
    },
    Read {
        asset_id: String,
    },
    Inspect {
        asset_id: String,
    },
    Invalidate {
        asset_id: String,
        #[arg(long)]
        reason: String,
    },
    /// Explicit manual confirmation through a version CAS.
    Publish {
        asset_id: String,
        #[arg(long)]
        expected_version: u64,
        #[arg(long)]
        confirm: bool,
    },
    /// Propose a source-linked L2 summary from an exact L1/L2 version.
    Summarize {
        asset_id: String,
        #[arg(long)]
        expected_version: u64,
        #[arg(long)]
        content: String,
    },
    Jobs,
    CatchUp {
        #[arg(long)]
        budget: u32,
        #[arg(long, value_enum, default_value_t = ExtractorMode::Disabled)]
        extractor: ExtractorMode,
        /// Explicit replay generation; a changed strategy also requires --replay-from.
        #[arg(long, default_value = "p4-journal-v1")]
        strategy: String,
        #[arg(long)]
        replay_from: Option<u64>,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ExtractorMode {
    Disabled,
    Mock,
}

pub async fn run(command: MemoryCommand) -> Result<(), HarnessError> {
    let data_dir = command.data_dir.as_ref().ok_or_else(|| {
        HarnessError::new(ErrorCode::InvalidPayload, "memory requires --data-dir")
    })?;
    let principal = MemoryPrincipal {
        principal_id: command.principal.clone(),
        project_id: command.project_id.map(ProjectId::parse).transpose()?,
        task_id: command.task_id.map(TaskId::parse).transpose()?,
        agent_profile_id: command.profile_id.map(AgentProfileId::parse).transpose()?,
        session_id: command.session_id.map(SessionId::parse).transpose()?,
    };
    let read_only = matches!(
        command.command,
        MemoryAction::Search { .. }
            | MemoryAction::Read { .. }
            | MemoryAction::Inspect { .. }
            | MemoryAction::Jobs
    );
    let store = Arc::new(
        if read_only {
            SqliteStore::open_read_only(data_dir).await
        } else {
            SqliteStore::open_writer(WriterOpenOptions::new(data_dir, HostId::generate())).await
        }
        .map_err(super::store_error)?,
    );
    let service = MemoryService::new(Arc::clone(&store));
    let result = execute(&service, &principal, command.command).await;
    drop(service);
    Arc::try_unwrap(store)
        .map_err(|_| {
            HarnessError::new(
                ErrorCode::RuntimeBlocked,
                "memory consumers did not release store",
            )
        })?
        .close()
        .await
        .map_err(super::store_error)?;
    let result = result?;
    if command.json {
        println!("{}", json!({"schema_version": 1, "memory": result}));
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&result).map_err(|_| HarnessError::new(
                ErrorCode::InvalidPayload,
                "memory output is invalid"
            ))?
        );
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // One explicit CLI action table.
async fn execute(
    service: &MemoryService,
    principal: &MemoryPrincipal,
    action: MemoryAction,
) -> Result<Value, HarnessError> {
    match action {
        MemoryAction::Search { query, limit } => {
            let result = service.search(principal, &query, limit, None).await?;
            Ok(
                json!({"state": result.state, "detail": result.detail, "revision": result.revision, "hits": result.hits.iter().map(asset_json).collect::<Vec<_>>()}),
            )
        }
        MemoryAction::Read { asset_id } => {
            let asset = service
                .read(principal, &MemoryAssetId::parse(asset_id)?)
                .await?;
            Ok(json!({"asset": asset.as_ref().map(asset_json)}))
        }
        MemoryAction::Inspect { asset_id } => {
            let id = MemoryAssetId::parse(asset_id)?;
            let asset = service.read(principal, &id).await?;
            let versions = service.export_versions(principal, &id).await?;
            Ok(
                json!({"asset": asset.as_ref().map(asset_json), "versions": versions.iter().map(|version| json!({"record": version.record, "content": version.content, "strategy_digest": version.strategy_digest})).collect::<Vec<_>>()}),
            )
        }
        MemoryAction::Invalidate { asset_id, reason } => Ok(
            json!({"invalidated": service.invalidate(principal, &MemoryAssetId::parse(asset_id)?, &reason).await?}),
        ),
        MemoryAction::Publish {
            asset_id,
            expected_version,
            confirm,
        } => {
            if !confirm {
                return Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "manual publication requires --confirm",
                ));
            }
            let id = MemoryAssetId::parse(asset_id)?;
            let current = service
                .read(principal, &id)
                .await?
                .ok_or_else(|| HarnessError::new(ErrorCode::InvalidPayload, "asset not found"))?;
            let record = current.current.record;
            let published = service
                .write_version(
                    principal,
                    &id,
                    expected_version,
                    WriteMemoryVersion {
                        source_assets: Vec::new(),
                        content: current.current.content,
                        authority: SourceAuthority::User,
                        evidence: harness_memory::EvidenceState::UserConfirmed,
                        user_confirmed: true,
                        source_event_refs: record.source_event_refs,
                        source_file_hashes: record.source_file_hashes,
                        source_commit: record.source_commit,
                        provenance_kind: "manual_confirmation".to_owned(),
                        validity: Validity::Valid,
                        supersedes: Some(expected_version),
                        extractor_version: record.extractor_version,
                        strategy_digest: current.current.strategy_digest,
                    },
                )
                .await?;
            Ok(asset_json(&published))
        }
        MemoryAction::Summarize {
            asset_id,
            expected_version,
            content,
        } => {
            let asset = service
                .derive_l2(
                    principal,
                    &[harness_types::MemoryVersionRef {
                        memory_asset_id: MemoryAssetId::parse(asset_id)?,
                        version: expected_version,
                    }],
                    &content,
                )
                .await?;
            Ok(asset_json(&asset))
        }
        MemoryAction::Jobs => {
            let stream = principal.session_id.as_ref().ok_or_else(|| {
                HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "jobs requires --session-id host scope",
                )
            })?;
            let jobs = service
                .list_jobs()
                .await?
                .into_iter()
                .filter(|job| &job.source_stream == stream)
                .collect::<Vec<_>>();
            Ok(json!({"jobs": jobs}))
        }
        MemoryAction::CatchUp {
            budget,
            extractor,
            strategy,
            replay_from,
        } => {
            let stream = principal.session_id.as_ref().ok_or_else(|| {
                HarnessError::new(ErrorCode::PolicyDenied, "catch-up requires --session-id")
            })?;
            let strategy = ExtractionStrategy {
                extractor_version: "mock-extractor-v1".to_owned(),
                strategy_digest: ContentHash::from_bytes(strategy.as_bytes()),
                replay_start_sequence: replay_from,
            };
            service.recover_interrupted_jobs().await?;
            let scheduled = service.schedule_backlog(stream, &strategy, 16).await?.len();
            let cancellation = harness_providers::CancellationToken::new();
            let signal = cancellation.clone();
            let listener = tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    signal.cancel();
                }
            });
            let result = service
                .catch_up(
                    principal,
                    &strategy,
                    &CliExtractor(extractor),
                    &MemoryBudget::calls(budget),
                    &cancellation,
                )
                .await;
            listener.abort();
            let _ = listener.await;
            Ok(
                json!({"scheduled": scheduled, "report": result?, "contiguous_sequence": service.extraction_cursor(stream, &strategy).await?, "extractor": if matches!(extractor, ExtractorMode::Mock) { "deterministic_mock" } else { "disabled" }}),
            )
        }
    }
}

fn asset_json(asset: &harness_memory::StoredMemoryAsset) -> Value {
    json!({"asset": asset.asset, "layer": asset.layer, "current": asset.current.record, "content": asset.current.content, "strategy_digest": asset.current.strategy_digest})
}

struct CliExtractor(ExtractorMode);
impl MemoryExtractor for CliExtractor {
    fn version(&self) -> &'static str {
        "mock-extractor-v1"
    }
    fn extract<'a>(
        &'a self,
        sources: &'a [SourceProjection],
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, HarnessError>> + Send + 'a>,
    > {
        Box::pin(async move {
            if matches!(self.0, ExtractorMode::Disabled) {
                return Err(HarnessError::new(
                    ErrorCode::ServiceUnavailable,
                    "optional extractor disabled",
                ));
            }
            serde_json::to_string(&ExtractionOutput {
                candidates: sources
                    .iter()
                    .take(16)
                    .map(|source| harness_memory::ExtractedCandidate {
                        content: source.content.clone(),
                        source_event_refs: vec![source.event_id.clone()],
                    })
                    .collect(),
            })
            .map_err(|_| {
                HarnessError::new(
                    ErrorCode::InvalidPayload,
                    "mock output serialization failed",
                )
            })
        })
    }
}
