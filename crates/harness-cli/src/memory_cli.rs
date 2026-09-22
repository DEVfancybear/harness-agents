use std::{path::PathBuf, sync::Arc};

use clap::{Args, Subcommand, ValueEnum};
use harness_memory::{
    ExtractionOutput, ExtractionScope, ExtractionStrategy, MemoryBudget, MemoryExtractor,
    MemoryPrincipal, MemoryService, SourceProjection, WriteMemoryVersion,
};
use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
use harness_types::{
    AgentProfileId, ContentHash, ErrorCode, HarnessError, HostId, MemoryAssetId, ProjectId,
    SessionId, SourceAuthority, TaskId, Validity,
};
use serde_json::{Value, json};

use crate::interactive::{self, paths::LaunchEnvironment};

#[derive(Debug, Args)]
pub struct MemoryCommand {
    /// Data root to read. Defaults to the same root the interactive app uses, so a
    /// key saved in the app can be inspected without repeating the path.
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    /// Workspace root whose project identity scopes the command.
    ///
    /// The app shows neither the project id nor a way to look it up, and a
    /// project-scoped read without it returns nothing at all. This resolves the
    /// identity the chat registered for that root, so an operator can inspect what a
    /// turn stored without decoding a store filename by hand.
    #[arg(long, global = true)]
    cwd: Option<PathBuf>,
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
    /// Candidate memory waiting for a human decision.
    Candidates {
        #[arg(long, default_value_t = 16)]
        limit: usize,
    },
    /// Confirm named candidates, or the oldest ones, as user-approved memory.
    Confirm {
        /// Assets to confirm; empty means the oldest `--limit` candidates.
        #[arg(long = "asset")]
        asset: Vec<String>,
        #[arg(long, default_value_t = 8)]
        limit: u32,
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
    /// Propose a candidate: recorded, inspectable, not usable memory yet.
    ///
    /// Without an `--asset` this creates a new candidate at version 1. With one, it
    /// adds a version to that asset and `--expected-version` is required, so a
    /// proposal built on a stale read is refused instead of overwriting whatever
    /// was published in the meantime.
    Propose {
        /// Existing asset to propose a new version on; omit to propose a new asset.
        #[arg(long)]
        asset: Option<String>,
        #[arg(long)]
        expected_version: Option<u64>,
        #[arg(long)]
        content: String,
        /// Project scope for a new asset. Required when `--asset` is absent.
        #[arg(long)]
        scope: Option<String>,
        #[arg(long, default_value = "proposed")]
        kind: String,
    },
    /// Refuse a candidate on the record, at the version the human inspected.
    Reject {
        asset_id: String,
        #[arg(long)]
        expected_version: u64,
        #[arg(long)]
        reason: String,
    },
    /// Dump every version of an asset with its recorded sources.
    Export {
        asset_id: String,
    },
    Jobs,
    CatchUp {
        #[arg(long)]
        budget: u32,
        #[arg(long, value_enum, default_value_t = ExtractorMode::Disabled)]
        extractor: ExtractorMode,
        /// Where the settled assets belong. `session` keeps this run's history private
        /// to its stream; `project` makes the knowledge readable from any later session
        /// of the same project, which is a different scope and therefore a different
        /// cursor generation.
        #[arg(long, value_enum, default_value_t = AssetScopeMode::Session)]
        asset_scope: AssetScopeMode,
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

#[derive(Clone, Copy, Debug, ValueEnum)]
enum AssetScopeMode {
    Session,
    Project,
}

impl AssetScopeMode {
    fn label(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Project => "project",
        }
    }

    fn scope(self) -> ExtractionScope {
        match self {
            Self::Session => ExtractionScope::Session,
            Self::Project => ExtractionScope::Project,
        }
    }
}

pub async fn run(command: MemoryCommand) -> Result<(), HarnessError> {
    let environment = LaunchEnvironment::capture();
    let mut project_id = command
        .project_id
        .clone()
        .map(ProjectId::parse)
        .transpose()?;
    // `--cwd` is the shorthand for "the project the chat registered in this
    // workspace". It also supplies the store, because these commands open the project
    // store directly: an id without the matching directory would look in the data
    // root, find no database there, and report an uninitialized store.
    let mut resolved_store = None;
    if let Some(root) = &command.cwd {
        let resolved = interactive::registered_project(root).await?;
        if project_id.is_none() {
            project_id = Some(resolved.id);
        }
        if command.data_dir.is_none() {
            resolved_store = Some(resolved.store_dir);
        }
    }
    let data_dir = match (&command.data_dir, resolved_store) {
        (Some(dir), _) => dir.clone(),
        (None, Some(store_dir)) => store_dir,
        (None, None) => interactive::default_data_dir(&environment)?,
    };
    let principal = MemoryPrincipal {
        principal_id: command.principal.clone(),
        project_id,
        task_id: command.task_id.map(TaskId::parse).transpose()?,
        agent_profile_id: command.profile_id.map(AgentProfileId::parse).transpose()?,
        session_id: command.session_id.map(SessionId::parse).transpose()?,
    };
    let read_only = matches!(
        command.command,
        MemoryAction::Search { .. }
            | MemoryAction::Read { .. }
            | MemoryAction::Inspect { .. }
            | MemoryAction::Export { .. }
            | MemoryAction::Candidates { .. }
            | MemoryAction::Jobs
    );
    // A read with no scope at all is not empty memory, it is an unasked question: the
    // service answers `[]` and the operator concludes memory is broken. Say which flag
    // is missing instead.
    //
    // "No scope" means none of them: a session- or task-scoped read is a deliberate
    // narrowing, and `p4_s07` reviews candidates that way. Only the unscoped read is
    // the accidental one this refuses.
    let scoped = principal.project_id.is_some()
        || principal.task_id.is_some()
        || principal.agent_profile_id.is_some()
        || principal.session_id.is_some();
    if read_only
        && !scoped
        && matches!(
            command.command,
            MemoryAction::Search { .. } | MemoryAction::Candidates { .. }
        )
    {
        return Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            "memory is scoped to a project: pass --cwd <workspace> to use the identity the chat registered there, or --project-id <project_uuid>",
        ));
    }
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
        MemoryAction::Candidates { limit } => {
            let candidates = service.list_candidates(principal, limit).await?;
            Ok(json!({
                "count": candidates.len(),
                "candidates": candidates.iter().map(|candidate| json!({
                    "asset": candidate.asset,
                    "layer": candidate.layer,
                    "scope": candidate.asset.scope,
                    "version": candidate.asset.current_version,
                    "content_preview": candidate.current.content.chars().take(240).collect::<String>(),
                })).collect::<Vec<_>>(),
            }))
        }
        MemoryAction::Confirm {
            asset,
            limit,
            confirm,
        } => {
            if !confirm {
                return Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "confirmation is a human act: run ha memory candidates first, then pass --confirm",
                ));
            }
            let ids = if asset.is_empty() {
                service
                    .list_candidates(principal, usize::try_from(limit).unwrap_or(8).min(64))
                    .await?
                    .into_iter()
                    .map(|candidate| candidate.asset.memory_asset_id)
                    .collect::<Vec<_>>()
            } else {
                asset
                    .iter()
                    .map(|id| MemoryAssetId::parse(id.clone()))
                    .collect::<Result<Vec<_>, _>>()?
            };
            let confirmed = service.confirm(principal, &ids).await?;
            Ok(json!({
                "confirmed": confirmed.len(),
                "assets": confirmed.iter().map(asset_json).collect::<Vec<_>>(),
            }))
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
                        sources: Vec::new(),
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
        MemoryAction::Propose {
            asset,
            expected_version,
            content,
            scope,
            kind,
        } => {
            let sources = Vec::new();
            let request = |scope: harness_types::MemoryScope, project_id: Option<ProjectId>| {
                harness_memory::CreateMemoryAsset {
                    kind: kind.clone(),
                    scope,
                    layer: harness_memory::MemoryLayer::L1,
                    project_id,
                    task_id: None,
                    agent_profile_id: None,
                    session_id: None,
                    visibility: "scoped".to_owned(),
                    content: content.clone(),
                    // A proposal is the model's or the operator's suggestion. It is
                    // recorded as such, and the publication policy is what decides
                    // it stays a candidate.
                    authority: SourceAuthority::ModelProposed,
                    evidence: harness_memory::EvidenceState::ModelInference,
                    user_confirmed: false,
                    source_event_refs: Vec::new(),
                    source_file_hashes: Vec::new(),
                    source_commit: None,
                    provenance_kind: "cli_proposal".to_owned(),
                    sources: sources.clone(),
                }
            };
            // Parsed before the call so the borrow lives long enough for
            // `propose`, which takes the target by reference.
            let parsed_target = match (asset.as_deref(), expected_version) {
                (Some(id), Some(version)) => Some((MemoryAssetId::parse(id)?, version)),
                (Some(_), None) => {
                    return Err(HarnessError::new(
                        ErrorCode::InvalidPayload,
                        "proposing a version needs --expected-version, so a stale proposal cannot overwrite a published one",
                    ));
                }
                (None, _) => None,
            };
            let target = parsed_target.as_ref().map(|(id, version)| (id, *version));
            let scope = match scope.as_deref() {
                None | Some("project") => harness_types::MemoryScope::Project,
                Some("user") => harness_types::MemoryScope::User,
                Some("task") => harness_types::MemoryScope::Task,
                Some("session") => harness_types::MemoryScope::Session,
                Some("agent_profile") => harness_types::MemoryScope::AgentProfile,
                Some(other) => {
                    return Err(HarnessError::new(
                        ErrorCode::InvalidPayload,
                        format!("unsupported memory scope: {other}"),
                    ));
                }
            };
            let proposed = service
                .propose(
                    principal,
                    target,
                    request(scope, principal.project_id.clone()),
                )
                .await?;
            Ok(json!({
                "proposed": asset_json(&proposed),
                "status": proposed.asset.status,
                "published": proposed.asset.status == harness_types::MemoryAssetStatus::Active,
            }))
        }
        MemoryAction::Reject {
            asset_id,
            expected_version,
            reason,
        } => Ok(json!({
            "rejected": service
                .reject(principal, &MemoryAssetId::parse(asset_id)?, expected_version, &reason)
                .await?,
        })),
        MemoryAction::Export { asset_id } => {
            let id = MemoryAssetId::parse(asset_id)?;
            let versions = service.export_versions(principal, &id).await?;
            let mut exported = Vec::new();
            for version in versions {
                exported.push(json!({
                    "record": version.record,
                    "content": version.content,
                    "strategy_digest": version.strategy_digest,
                    "sources": version.sources.iter().map(|source| json!({
                        "kind": source.kind,
                        "id": source.id,
                        "observed_digest": source.observed_digest,
                        "source_version": source.source_version,
                    })).collect::<Vec<_>>(),
                }));
            }
            Ok(json!({"asset_id": id, "versions": exported, "redacted": true}))
        }
        MemoryAction::Jobs => {
            let stream = principal.session_id.as_ref().ok_or_else(|| {
                HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "jobs requires --session-id host scope",
                )
            })?;
            let jobs = service.list_jobs_for(stream).await?;
            Ok(json!({"jobs": jobs}))
        }
        MemoryAction::CatchUp {
            budget,
            extractor,
            asset_scope,
            strategy,
            replay_from,
        } => {
            let stream = principal.session_id.as_ref().ok_or_else(|| {
                HarnessError::new(ErrorCode::PolicyDenied, "catch-up requires --session-id")
            })?;
            let strategy = ExtractionStrategy {
                extractor_version: "mock-extractor-v1".to_owned(),
                // The asset scope is part of the strategy: the same stream extracted at
                // another scope is another generation of work with its own cursor, so a
                // scope change cannot silently reuse what the previous scope settled.
                strategy_digest: ContentHash::from_bytes(
                    format!("{strategy}#asset-scope={}", asset_scope.label()).as_bytes(),
                ),
                replay_start_sequence: replay_from,
                asset_scope: asset_scope.scope(),
            };
            service.recover_interrupted_jobs().await?;
            // Reconcile rather than schedule: the backlog is what the journal
            // committed and this host has not settled, so a range lost to a crash
            // between the commit and the enqueue is created here, and a range that
            // was already enqueued is not created twice.
            let reconciled = service.reconcile(stream, &strategy, 16).await?;
            let scheduled = reconciled.enqueued;
            let cursor_before = reconciled.cursor;
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
                json!({"scheduled": scheduled, "cursor_before": cursor_before, "report": result?, "contiguous_sequence": service.extraction_cursor(stream, &strategy).await?, "extractor": if matches!(extractor, ExtractorMode::Mock) { "deterministic_mock" } else { "disabled" }, "asset_scope": asset_scope.label()}),
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
