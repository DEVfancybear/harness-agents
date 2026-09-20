#![forbid(unsafe_code)]

//! Scoped reusable-memory contracts and services.

use std::{collections::BTreeSet, sync::Arc};

use harness_store_sqlite::SqliteStore;
use harness_store_sqlite::{
    MemoryCreateCommit, MemoryVersionCommit, StoreMemoryPrincipal, StoredExtractionJobRecord,
    StoredExtractionLeaseRecord, StoredMemoryAssetRecord, StoredMemoryGrantRecord,
    StoredMemoryVersionRecord,
};
use harness_types::{
    AgentProfileId, ContentHash, ErrorCode, EventId, HarnessError, MemoryAsset, MemoryAssetId,
    MemoryAssetStatus, MemoryScope, MemoryVersion, ProjectId, SessionId, SourceAuthority, TaskId,
    Validity,
};
use serde::{Deserialize, Serialize};
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

mod extraction;
pub use extraction::{
    ExtractedCandidate, ExtractionOutput, MemoryExtractor, SourceProjection, sanitize_memory_text,
};
mod retrieval;
pub use retrieval::{
    MAX_QUERY_BYTES, MemoryContribution, RetrievalResult, RetrievalState, VectorAdapter,
    normalize_terms,
};
mod maintenance;
pub use maintenance::{CatchUpReport, MemoryBudget};

pub const MEMORY_CONTRACT_VERSION: u16 = 1;

/// The provenance kind that marks an asset as a record of one conversation turn.
///
/// Declared here, next to the contract, because it is a value two crates have to agree
/// on: the host writes it, and the retention rule in this crate selects on it.
pub const TURN_PROVENANCE_KIND: &str = "session_turn";

/// The heading that opens a memory block in the context packet.
///
/// A block used to open with `Reusable data; authority=…`, which describes where the
/// text came from and never says what it is. Measured: with a turn record injected, the
/// model still answered "I don't have access to the previous session's conversation
/// history" - it read the block as provenance metadata beside the working state rather
/// than as the answer to the question. The heading now names the material and tells the
/// model that using it is the point.
///
/// It also says what not to do with it. A turn record quotes what an earlier reply said,
/// and a reply is not an observation: read as established fact, one model answer would
/// harden into durable knowledge and later turns would cite it as though the runtime had
/// verified it.
pub const MEMORY_BLOCK_HEADING: &str = "Memory from earlier turns - use it to answer. It records what was asked and said; \
     treat a quoted reply as something that was said, not as a verified fact.";

#[cfg(test)]
mod properties {
    use super::normalize_search_text;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        #[test]
        fn p4_property_normalization_preserves_words_and_is_idempotent(word in "[a-z]{1,24}", tail in "[a-z]{1,24}") {
            let query = format!("{word}_{tail}");
            let normalized = normalize_search_text(&query);
            prop_assert_eq!(&normalized, &format!("{word} {tail}"));
            prop_assert_eq!(normalize_search_text(&normalized), normalized);
        }
    }
    #[test]
    fn p4_normalization_handles_vietnamese_and_acronyms() {
        assert_eq!(
            normalize_search_text("Đường dẫn HTTPParser parseHTTPResponse"),
            "duong dan http parser parse http response"
        );
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAction {
    Read,
    Search,
    Export,
    Propose,
    Publish,
    Bind,
    Invalidate,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryLayer {
    L1,
    L2,
    L3,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceState {
    VerifiedObservation,
    UserConfirmed,
    ModelInference,
    Derived,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InjectionMode {
    Bootstrap,
    Index,
    OnDemand,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionAction {
    Invalidate,
    Archive,
    Purge,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MemoryPrincipal {
    pub principal_id: String,
    pub project_id: Option<ProjectId>,
    pub task_id: Option<TaskId>,
    pub agent_profile_id: Option<AgentProfileId>,
    pub session_id: Option<SessionId>,
}

#[derive(Clone, Debug)]
pub struct CreateMemoryAsset {
    pub kind: String,
    pub scope: MemoryScope,
    pub layer: MemoryLayer,
    pub project_id: Option<ProjectId>,
    pub task_id: Option<TaskId>,
    pub agent_profile_id: Option<AgentProfileId>,
    pub session_id: Option<SessionId>,
    pub visibility: String,
    pub content: String,
    pub authority: SourceAuthority,
    pub evidence: EvidenceState,
    pub user_confirmed: bool,
    pub source_event_refs: Vec<EventId>,
    pub source_file_hashes: Vec<ContentHash>,
    pub source_commit: Option<String>,
    pub provenance_kind: String,
}

#[derive(Clone, Debug)]
pub struct WriteMemoryVersion {
    /// Explicit semantic merge sources; empty preserves existing derived lineage.
    pub source_assets: Vec<harness_types::MemoryVersionRef>,
    pub content: String,
    pub authority: SourceAuthority,
    pub evidence: EvidenceState,
    pub user_confirmed: bool,
    pub source_event_refs: Vec<EventId>,
    pub source_file_hashes: Vec<ContentHash>,
    pub source_commit: Option<String>,
    pub provenance_kind: String,
    pub validity: Validity,
    pub supersedes: Option<u64>,
    pub extractor_version: Option<String>,
    pub strategy_digest: Option<ContentHash>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredMemoryVersion {
    pub record: MemoryVersion,
    pub content: String,
    pub strategy_digest: Option<ContentHash>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredMemoryAsset {
    pub asset: MemoryAsset,
    pub layer: MemoryLayer,
    pub task_id: Option<TaskId>,
    pub agent_profile_id: Option<AgentProfileId>,
    pub session_id: Option<SessionId>,
    pub current: StoredMemoryVersion,
}

/// Where the assets one extraction settles belong.
///
/// The stream a job reads and the scope its result belongs to are two different host
/// decisions: a session's stream can yield run history or reusable project knowledge,
/// and only the host knows which. The scope is part of the strategy, so a change of
/// scope is a change of strategy — and callers fold it into the strategy digest, which
/// is what keeps a cursor from being reused across two different scopes.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionScope {
    /// Assets belong to the session whose stream produced them, and to no other.
    #[default]
    Session,
    /// Assets belong to the host principal's project (or to the user when the
    /// principal carries no project), and to no session or task: knowledge meant to
    /// outlive the run that produced it.
    Project,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtractionStrategy {
    pub extractor_version: String,
    pub strategy_digest: ContentHash,
    pub replay_start_sequence: Option<u64>,
    /// Scope the settled assets are written at; see [`ExtractionScope`].
    pub asset_scope: ExtractionScope,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionJobStatus {
    Pending,
    Leased,
    Completed,
    RetryWait,
    Blocked,
    Paused,
    DeadLetter,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExtractionJob {
    pub job_id: String,
    pub source_stream: SessionId,
    pub start_sequence: u64,
    pub end_sequence: u64,
    pub source_digest: ContentHash,
    pub source_event_ids: Vec<EventId>,
    pub extractor_version: String,
    pub strategy_digest: ContentHash,
    pub status: ExtractionJobStatus,
    pub attempts: u32,
    pub lease_owner: Option<String>,
    pub lease_generation: u64,
    pub last_error: Option<String>,
    pub disposition: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ExtractionLease {
    pub job: ExtractionJob,
    pub source_events: Vec<harness_types::EventEnvelope>,
    pub owner: String,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtractionFailureKind {
    Unavailable,
    InvalidOutput,
    Failed,
}

#[derive(Clone, Debug)]
pub struct MemoryService {
    store: Arc<SqliteStore>,
    publication: PublicationPolicy,
}

impl MemoryService {
    #[must_use]
    pub fn new(store: Arc<SqliteStore>) -> Self {
        Self {
            store,
            publication: PublicationPolicy,
        }
    }

    pub async fn create_asset(
        &self,
        principal: &MemoryPrincipal,
        mut request: CreateMemoryAsset,
    ) -> Result<StoredMemoryAsset, HarnessError> {
        validate_create(principal, &request)?;
        request.content = extraction::sanitize_memory_text(&request.content);
        let decision = self.publication.classify(
            request.authority,
            request.evidence,
            request.layer,
            request.user_confirmed,
        );
        let memory_asset_id = MemoryAssetId::generate();
        let content_hash = ContentHash::from_bytes(request.content.as_bytes());
        let asset = MemoryAsset {
            schema_version: MEMORY_CONTRACT_VERSION,
            memory_asset_id: memory_asset_id.clone(),
            kind: request.kind,
            owner_id: principal.principal_id.clone(),
            project_id: request.project_id,
            scope: request.scope,
            visibility: request.visibility,
            status: decision.status,
            current_version: 1,
            created_by: request.authority,
        };
        let version = MemoryVersion {
            schema_version: MEMORY_CONTRACT_VERSION,
            memory_asset_id,
            version: 1,
            content_or_artifact_hash: content_hash.clone(),
            content_hash,
            source_event_refs: request.source_event_refs,
            source_file_hashes: request.source_file_hashes,
            source_commit: request.source_commit,
            provenance_kind: request.provenance_kind,
            evidence_state: request.evidence.as_str().to_owned(),
            confidence_annotation: None,
            validity: Validity::Valid,
            supersedes: None,
            extractor_version: None,
        };
        let record = StoredMemoryAssetRecord {
            asset,
            layer: request.layer.as_str().to_owned(),
            task_id: request.task_id,
            agent_profile_id: request.agent_profile_id,
            session_id: request.session_id,
            current: StoredMemoryVersionRecord {
                record: version,
                normalized_content: normalize_search_text(&request.content),
                content: request.content,
                strategy_digest: None,
            },
        };
        let stored = self
            .store
            .create_memory_asset(MemoryCreateCommit { record })
            .await
            .map_err(to_harness_error)?;
        convert_asset(stored)
    }

    #[allow(clippy::too_many_lines)] // Keep provenance resolution and the final CAS request together.
    pub async fn write_version(
        &self,
        principal: &MemoryPrincipal,
        memory_asset_id: &MemoryAssetId,
        expected_version: u64,
        mut request: WriteMemoryVersion,
    ) -> Result<StoredMemoryAsset, HarnessError> {
        if expected_version == 0
            || request.content.trim().is_empty()
            || request.content.len() > 65_536
        {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "memory version requires positive expected version and content",
            ));
        }
        request.content = extraction::sanitize_memory_text(&request.content);
        let authorization = store_principal(principal);
        let current = self
            .store
            .read_memory_asset(
                &authorization,
                memory_asset_id,
                MemoryAction::Publish.as_str(),
            )
            .await
            .map_err(to_harness_error)?
            .ok_or_else(|| {
                HarnessError::new(ErrorCode::InvalidPayload, "memory asset was not found")
            })?;
        let layer = parse_layer(&current.layer)?;
        if request.source_assets.len() > 32
            || (!request.source_assets.is_empty() && layer != MemoryLayer::L2)
        {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "semantic merge requires bounded L2 sources",
            ));
        }
        for source in &request.source_assets {
            let asset = self
                .read(principal, &source.memory_asset_id)
                .await?
                .ok_or_else(|| {
                    HarnessError::new(ErrorCode::InvalidPayload, "merge source missing")
                })?;
            request
                .source_event_refs
                .extend(asset.current.record.source_event_refs);
            request
                .source_file_hashes
                .extend(asset.current.record.source_file_hashes);
        }
        if layer == MemoryLayer::L2 && request.source_assets.is_empty() {
            request
                .source_event_refs
                .extend(current.current.record.source_event_refs.clone());
            request
                .source_file_hashes
                .extend(current.current.record.source_file_hashes.clone());
        }
        request.source_event_refs.sort();
        request.source_event_refs.dedup();
        request
            .source_file_hashes
            .sort_by(|a, b| a.as_str().cmp(b.as_str()));
        request.source_file_hashes.dedup();
        validate_source_evidence(
            request.evidence,
            &request.source_event_refs,
            &request.source_file_hashes,
            request.source_commit.as_deref(),
        )?;
        let decision = self.publication.classify(
            request.authority,
            request.evidence,
            layer,
            request.user_confirmed,
        );
        let version_number = expected_version.saturating_add(1);
        let content_hash = ContentHash::from_bytes(request.content.as_bytes());
        let mut asset = current.asset;
        asset.current_version = version_number;
        asset.status = decision.status;
        let version = StoredMemoryVersionRecord {
            record: MemoryVersion {
                schema_version: MEMORY_CONTRACT_VERSION,
                memory_asset_id: memory_asset_id.clone(),
                version: version_number,
                content_or_artifact_hash: content_hash.clone(),
                content_hash,
                source_event_refs: request.source_event_refs,
                source_file_hashes: request.source_file_hashes,
                source_commit: request.source_commit,
                provenance_kind: request.provenance_kind,
                evidence_state: request.evidence.as_str().to_owned(),
                confidence_annotation: None,
                validity: request.validity,
                supersedes: request.supersedes,
                extractor_version: request.extractor_version,
            },
            normalized_content: normalize_search_text(&request.content),
            content: request.content,
            strategy_digest: request.strategy_digest,
        };
        let stored = self
            .store
            .write_memory_version(MemoryVersionCommit {
                source_assets: request.source_assets,
                authorization,
                action: MemoryAction::Publish.as_str().to_owned(),
                memory_asset_id: memory_asset_id.clone(),
                expected_version,
                asset,
                version,
            })
            .await
            .map_err(to_harness_error)?;
        convert_asset(stored)
    }

    /// Candidate assets this principal may still confirm, oldest first.
    ///
    /// Extraction settles model inference as a candidate on purpose, so a human needs a
    /// way to see what is waiting without copying asset ids out of a search result.
    pub async fn list_candidates(
        &self,
        principal: &MemoryPrincipal,
        limit: usize,
    ) -> Result<Vec<StoredMemoryAsset>, HarnessError> {
        if limit == 0 || limit > 64 {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "candidate listing is bounded to 1..=64 assets",
            ));
        }
        self.store
            .list_memory_candidates(&store_principal(principal), limit)
            .await
            .map_err(to_harness_error)?
            .into_iter()
            .map(convert_asset)
            .collect()
    }

    /// Confirm one asset version as user-approved memory.
    ///
    /// Confirmation is the host's act, never the model's: the content is not rewritten,
    /// the asset keeps its scope and provenance, and the new version carries
    /// `UserConfirmed` evidence, which is what the publication policy accepts as
    /// publishable. `expected_version` is the version the human inspected.
    pub async fn confirm_version(
        &self,
        principal: &MemoryPrincipal,
        memory_asset_id: &MemoryAssetId,
        expected_version: u64,
        provenance_kind: &str,
    ) -> Result<StoredMemoryAsset, HarnessError> {
        let current = self
            .read(principal, memory_asset_id)
            .await?
            .ok_or_else(|| HarnessError::new(ErrorCode::InvalidPayload, "asset not found"))?;
        if current.asset.current_version != expected_version {
            return Err(HarnessError::new(
                ErrorCode::SequenceConflict,
                "the asset changed since it was inspected; review it again",
            ));
        }
        let record = current.current.record;
        self.write_version(
            principal,
            memory_asset_id,
            expected_version,
            WriteMemoryVersion {
                source_assets: Vec::new(),
                content: current.current.content,
                authority: SourceAuthority::User,
                evidence: EvidenceState::UserConfirmed,
                user_confirmed: true,
                source_event_refs: record.source_event_refs,
                source_file_hashes: record.source_file_hashes,
                source_commit: record.source_commit,
                provenance_kind: provenance_kind.to_owned(),
                validity: Validity::Valid,
                supersedes: Some(expected_version),
                extractor_version: record.extractor_version,
                strategy_digest: current.current.strategy_digest,
            },
        )
        .await
    }

    /// Confirm a bounded batch of candidate assets at their current version.
    ///
    /// One asset that cannot be confirmed fails the batch rather than being skipped: a
    /// caller that asked for confirmation has to learn which asset refused.
    pub async fn confirm(
        &self,
        principal: &MemoryPrincipal,
        assets: &[MemoryAssetId],
    ) -> Result<Vec<StoredMemoryAsset>, HarnessError> {
        if assets.is_empty() || assets.len() > 64 {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "confirmation is bounded to 1..=64 assets",
            ));
        }
        let mut confirmed = Vec::new();
        for asset in assets {
            let current = self.read(principal, asset).await?.ok_or_else(|| {
                HarnessError::new(ErrorCode::InvalidPayload, "candidate asset not found")
            })?;
            confirmed.push(
                self.confirm_version(
                    principal,
                    asset,
                    current.asset.current_version,
                    "host_confirmation",
                )
                .await?,
            );
        }
        Ok(confirmed)
    }

    pub async fn read(
        &self,
        principal: &MemoryPrincipal,
        memory_asset_id: &MemoryAssetId,
    ) -> Result<Option<StoredMemoryAsset>, HarnessError> {
        self.store
            .read_memory_asset(
                &store_principal(principal),
                memory_asset_id,
                MemoryAction::Read.as_str(),
            )
            .await
            .map_err(to_harness_error)?
            .map(convert_asset)
            .transpose()
    }

    /// The active asset that already holds this exact text, if any.
    ///
    /// The identity half of deduplication, and deliberately not a search: this asks
    /// "are these the same bytes", where a ranked match would answer "this looks
    /// related". They are different questions and only the first one is safe to skip a
    /// write on.
    ///
    /// # Errors
    /// Fails when the principal is unusable or the store cannot answer.
    pub async fn find_active_by_content(
        &self,
        principal: &MemoryPrincipal,
        content: &str,
        scope: MemoryScope,
        project_id: Option<ProjectId>,
    ) -> Result<Option<MemoryAssetId>, HarnessError> {
        validate_principal(principal)?;
        let store_principal = store_principal(principal);
        // Project scope compares the project column; user scope compares for absence,
        // so a user-scoped asset is never matched by a project-scoped lookup.
        let project_only = scope == MemoryScope::Project;
        let project = if project_only { project_id } else { None };
        let found = self
            .store
            .find_active_memory_by_content(
                &StoreMemoryPrincipal {
                    project_id: project,
                    ..store_principal
                },
                &normalize_search_text(content),
                project_only,
            )
            .await
            .map_err(to_harness_error)?;
        Ok(found)
    }

    /// Record one more source event on a version that already exists.
    ///
    /// Used by deduplication: the same text said twice is one memory with two
    /// sources, not two memories. The asset keeps its id, its version number and its
    /// content hash, so nothing that depends on it needs rebuilding.
    ///
    /// Returns whether the source list changed - `false` when the event was already
    /// recorded.
    ///
    /// # Errors
    /// Fails when the principal may not bind the asset, or the store cannot write.
    pub async fn append_version_source(
        &self,
        principal: &MemoryPrincipal,
        memory_asset_id: &MemoryAssetId,
        event_id: &EventId,
    ) -> Result<bool, HarnessError> {
        validate_principal(principal)?;
        self.store
            .append_memory_version_source(&store_principal(principal), memory_asset_id, event_id)
            .await
            .map_err(to_harness_error)
    }

    /// Turn records past the cap, oldest first.
    ///
    /// A retention rule for the conversation log: the caller retires what this returns
    /// through [`MemoryService::invalidate`], so the removal goes through the same
    /// authorization and lineage machinery as any other invalidation.
    ///
    /// # Errors
    /// Fails when the principal is unusable or the store cannot answer.
    pub async fn turn_records_over_limit(
        &self,
        principal: &MemoryPrincipal,
        project_id: Option<&ProjectId>,
        keep: usize,
    ) -> Result<Vec<MemoryAssetId>, HarnessError> {
        validate_principal(principal)?;
        let store_principal = StoreMemoryPrincipal {
            project_id: project_id.cloned(),
            ..store_principal(principal)
        };
        self.store
            .turn_records_over_limit(&store_principal, TURN_PROVENANCE_KIND, keep)
            .await
            .map_err(to_harness_error)
    }

    /// The newest turn records, newest first, up to `limit`.
    ///
    /// A question about the conversation - "what did I ask you before?" - is a question
    /// about *when*, not about *what*. Answering it by keyword overlap is the wrong
    /// tool: the words in that question appear in no particular turn, and the overlap
    /// floor that keeps unrelated notes out of the knowledge path also keeps the
    /// history out. Recentness is the right index for it, so this is a separate read.
    ///
    /// # Errors
    /// Fails when the principal is unusable or the store cannot answer.
    pub async fn recent_turns(
        &self,
        principal: &MemoryPrincipal,
        project_id: Option<&ProjectId>,
        limit: usize,
    ) -> Result<RetrievalResult, HarnessError> {
        validate_principal(principal)?;
        if limit == 0 || limit > 64 {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "turn history is bounded to 1..=64 records",
            ));
        }
        let store_principal = StoreMemoryPrincipal {
            project_id: project_id.cloned(),
            ..store_principal(principal)
        };
        let (records, revision) = self
            .store
            .recent_turn_records(&store_principal, TURN_PROVENANCE_KIND, limit)
            .await
            .map_err(to_harness_error)?;
        let hits = records
            .into_iter()
            .map(convert_asset)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(RetrievalResult {
            state: if hits.is_empty() {
                RetrievalState::Empty
            } else {
                RetrievalState::Found
            },
            hits,
            detail: None,
            // The store's revision, never a placeholder: this value is checked before
            // dispatch and a wrong one makes the contribution be dropped in silence.
            revision,
        })
    }

    pub async fn export_versions(
        &self,
        principal: &MemoryPrincipal,
        memory_asset_id: &MemoryAssetId,
    ) -> Result<Vec<StoredMemoryVersion>, HarnessError> {
        self.store
            .export_memory_versions(
                &store_principal(principal),
                memory_asset_id,
                MemoryAction::Export.as_str(),
            )
            .await
            .map_err(to_harness_error)?
            .into_iter()
            .map(convert_version)
            .collect()
    }

    pub async fn grant(
        &self,
        owner: &MemoryPrincipal,
        grant: MemoryGrant,
    ) -> Result<(), HarnessError> {
        let memory_asset_id = grant.memory_asset_id.clone().ok_or_else(|| {
            HarnessError::new(
                ErrorCode::InvalidPayload,
                "P4 requires an explicit asset grant; scope grants are reserved",
            )
        })?;
        self.store
            .grant_memory(
                &store_principal(owner),
                StoredMemoryGrantRecord {
                    principal_id: grant.principal_id,
                    memory_asset_id,
                    project_id: grant.project_id,
                    allowed_actions: grant
                        .allowed_actions
                        .into_iter()
                        .map(|action| action.as_str().to_owned())
                        .collect(),
                    revision: grant.revision,
                    active: grant.active,
                },
            )
            .await
            .map_err(to_harness_error)
    }

    #[allow(clippy::too_many_lines)] // Durable range planning preserves one ordering invariant.
    pub async fn schedule_backlog(
        &self,
        source_stream: &SessionId,
        strategy: &ExtractionStrategy,
        batch_size: usize,
    ) -> Result<Vec<ExtractionJob>, HarnessError> {
        if batch_size == 0
            || batch_size > 128
            || strategy.extractor_version.trim().is_empty()
            || strategy.replay_start_sequence == Some(0)
        {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "extraction strategy requires a version, positive replay start, and batch size",
            ));
        }
        if strategy.replay_start_sequence.is_none()
            && self.list_jobs().await?.iter().any(|job| {
                &job.source_stream == source_stream
                    && (job.extractor_version != strategy.extractor_version
                        || job.strategy_digest != strategy.strategy_digest)
            })
        {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "strategy change requires an explicit replay start",
            ));
        }
        let initial = strategy
            .replay_start_sequence
            .unwrap_or(1)
            .saturating_sub(1);
        self.store
            .ensure_extraction_cursor(
                source_stream,
                &strategy.extractor_version,
                &strategy.strategy_digest,
                initial,
            )
            .await
            .map_err(to_harness_error)?;
        let mut next = self
            .store
            .extraction_schedule_start(
                source_stream,
                &strategy.extractor_version,
                &strategy.strategy_digest,
            )
            .await
            .map_err(to_harness_error)?;
        let mut scheduled = Vec::new();
        while scheduled.len() < 256 {
            let events = self
                .store
                .load_memory_source_events(source_stream, next.saturating_sub(1), batch_size)
                .await
                .map_err(to_harness_error)?;
            if events.is_empty() {
                break;
            }
            let bytes = events
                .iter()
                .map(|event| serde_json::to_vec(event).map_or(usize::MAX, |bytes| bytes.len()))
                .fold(0usize, usize::saturating_add);
            if bytes > 1_048_576 {
                return Err(HarnessError::new(
                    ErrorCode::InvalidPayload,
                    "source batch exceeds byte limit; choose a smaller range",
                ));
            }
            if events[0].seq != next
                || events
                    .windows(2)
                    .any(|pair| pair[1].seq != pair[0].seq.saturating_add(1))
            {
                return Err(HarnessError::new(
                    ErrorCode::InvalidSequence,
                    "memory source-work markers are not contiguous",
                ));
            }
            let source_event_ids = events
                .iter()
                .map(|event| event.event_id.clone())
                .collect::<Vec<_>>();
            let end_sequence = events.last().map_or(next, |event| event.seq);
            let source_digest = source_batch_digest(&events)?;
            let job = StoredExtractionJobRecord {
                job_id: new_job_id(),
                source_stream: source_stream.clone(),
                start_sequence: next,
                end_sequence,
                source_digest,
                source_event_ids,
                extractor_version: strategy.extractor_version.clone(),
                strategy_digest: strategy.strategy_digest.clone(),
                status: ExtractionJobStatus::Pending.as_str().to_owned(),
                attempts: 0,
                lease_owner: None,
                lease_generation: 0,
                last_error: None,
                disposition: None,
            };
            self.store
                .insert_extraction_job(job.clone())
                .await
                .map_err(to_harness_error)?;
            scheduled.push(convert_job(job)?);
            next = end_sequence.saturating_add(1);
        }
        Ok(scheduled)
    }

    pub async fn lease_job(
        &self,
        job_id: &str,
        owner: &str,
    ) -> Result<ExtractionLease, HarnessError> {
        let lease = self
            .store
            .lease_extraction_job(job_id, owner)
            .await
            .map_err(to_harness_error)?;
        let digest = source_batch_digest(&lease.source_events)?;
        let ids = lease
            .source_events
            .iter()
            .map(|event| event.event_id.clone())
            .collect::<Vec<_>>();
        if digest != lease.job.source_digest || ids != lease.job.source_event_ids {
            return Err(HarnessError::new(
                ErrorCode::InvalidHash,
                "leased source batch does not match its immutable digest and IDs",
            ));
        }
        Ok(ExtractionLease {
            job: convert_job(lease.job)?,
            source_events: lease.source_events,
            owner: lease.owner,
            generation: lease.generation,
        })
    }

    pub async fn settle_no_facts(&self, lease: &ExtractionLease) -> Result<(), HarnessError> {
        self.store
            .settle_extraction_no_facts(&store_lease(lease))
            .await
            .map_err(to_harness_error)
    }

    pub async fn fail_job(
        &self,
        lease: &ExtractionLease,
        kind: ExtractionFailureKind,
        message: &str,
    ) -> Result<(), HarnessError> {
        let status = match kind {
            _ if lease.job.attempts >= 5 => ExtractionJobStatus::DeadLetter,
            ExtractionFailureKind::Unavailable => ExtractionJobStatus::Blocked,
            ExtractionFailureKind::InvalidOutput | ExtractionFailureKind::Failed => {
                ExtractionJobStatus::RetryWait
            }
        };
        self.store
            .fail_extraction_job(&store_lease(lease), status.as_str(), message)
            .await
            .map_err(to_harness_error)
    }

    pub async fn extraction_cursor(
        &self,
        source_stream: &SessionId,
        strategy: &ExtractionStrategy,
    ) -> Result<u64, HarnessError> {
        self.store
            .extraction_cursor(
                source_stream,
                &strategy.extractor_version,
                &strategy.strategy_digest,
            )
            .await
            .map_err(to_harness_error)
    }

    /// Every extraction job in the store, whatever stream it belongs to.
    ///
    /// Whole-store reporting for maintenance; a caller acting on one stream uses
    /// [`Self::list_jobs_for`], so it never has to filter another scope's rows out.
    pub async fn list_jobs(&self) -> Result<Vec<ExtractionJob>, HarnessError> {
        self.store
            .list_extraction_jobs()
            .await
            .map_err(to_harness_error)?
            .into_iter()
            .map(convert_job)
            .collect()
    }

    /// The extraction jobs of the one stream the host is acting on.
    pub async fn list_jobs_for(
        &self,
        stream: &SessionId,
    ) -> Result<Vec<ExtractionJob>, HarnessError> {
        self.store
            .list_extraction_jobs_for_stream(stream)
            .await
            .map_err(to_harness_error)?
            .into_iter()
            .map(convert_job)
            .collect()
    }

    pub async fn recover_interrupted_jobs(&self) -> Result<u64, HarnessError> {
        self.store
            .recover_extraction_jobs()
            .await
            .map_err(to_harness_error)
    }
}

impl MemoryAction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Search => "search",
            Self::Export => "export",
            Self::Propose => "propose",
            Self::Publish => "publish",
            Self::Bind => "bind",
            Self::Invalidate => "invalidate",
        }
    }
}

impl MemoryLayer {
    const fn as_str(self) -> &'static str {
        match self {
            Self::L1 => "l1",
            Self::L2 => "l2",
            Self::L3 => "l3",
        }
    }
}

impl ExtractionJobStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Leased => "leased",
            Self::Completed => "completed",
            Self::RetryWait => "retry_wait",
            Self::Blocked => "blocked",
            Self::Paused => "paused",
            Self::DeadLetter => "dead_letter",
        }
    }

    fn parse(value: &str) -> Result<Self, HarnessError> {
        match value {
            "pending" => Ok(Self::Pending),
            "leased" => Ok(Self::Leased),
            "completed" => Ok(Self::Completed),
            "retry_wait" => Ok(Self::RetryWait),
            "blocked" => Ok(Self::Blocked),
            "paused" => Ok(Self::Paused),
            "dead_letter" => Ok(Self::DeadLetter),
            _ => Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "stored extraction job status is unsupported",
            )),
        }
    }
}

impl EvidenceState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::VerifiedObservation => "verified_observation",
            Self::UserConfirmed => "user_confirmed",
            Self::ModelInference => "model_inference",
            Self::Derived => "derived",
        }
    }
}

fn store_principal(principal: &MemoryPrincipal) -> StoreMemoryPrincipal {
    StoreMemoryPrincipal {
        principal_id: principal.principal_id.clone(),
        project_id: principal.project_id.clone(),
        task_id: principal.task_id.clone(),
        agent_profile_id: principal.agent_profile_id.clone(),
        session_id: principal.session_id.clone(),
    }
}

fn convert_asset(record: StoredMemoryAssetRecord) -> Result<StoredMemoryAsset, HarnessError> {
    Ok(StoredMemoryAsset {
        asset: record.asset,
        layer: parse_layer(&record.layer)?,
        task_id: record.task_id,
        agent_profile_id: record.agent_profile_id,
        session_id: record.session_id,
        current: convert_version(record.current)?,
    })
}

fn convert_version(record: StoredMemoryVersionRecord) -> Result<StoredMemoryVersion, HarnessError> {
    record.record.validate()?;
    if ContentHash::from_bytes(record.content.as_bytes()) != record.record.content_hash {
        return Err(HarnessError::new(
            ErrorCode::InvalidHash,
            "stored memory content hash mismatch",
        ));
    }
    Ok(StoredMemoryVersion {
        record: record.record,
        content: record.content,
        strategy_digest: record.strategy_digest,
    })
}

fn convert_job(record: StoredExtractionJobRecord) -> Result<ExtractionJob, HarnessError> {
    Ok(ExtractionJob {
        job_id: record.job_id,
        source_stream: record.source_stream,
        start_sequence: record.start_sequence,
        end_sequence: record.end_sequence,
        source_digest: record.source_digest,
        source_event_ids: record.source_event_ids,
        extractor_version: record.extractor_version,
        strategy_digest: record.strategy_digest,
        status: ExtractionJobStatus::parse(&record.status)?,
        attempts: record.attempts,
        lease_owner: record.lease_owner,
        lease_generation: record.lease_generation,
        last_error: record.last_error,
        disposition: record.disposition,
    })
}

fn store_job(job: &ExtractionJob) -> StoredExtractionJobRecord {
    StoredExtractionJobRecord {
        job_id: job.job_id.clone(),
        source_stream: job.source_stream.clone(),
        start_sequence: job.start_sequence,
        end_sequence: job.end_sequence,
        source_digest: job.source_digest.clone(),
        source_event_ids: job.source_event_ids.clone(),
        extractor_version: job.extractor_version.clone(),
        strategy_digest: job.strategy_digest.clone(),
        status: job.status.as_str().to_owned(),
        attempts: job.attempts,
        lease_owner: job.lease_owner.clone(),
        lease_generation: job.lease_generation,
        last_error: job.last_error.clone(),
        disposition: job.disposition.clone(),
    }
}

fn store_lease(lease: &ExtractionLease) -> StoredExtractionLeaseRecord {
    StoredExtractionLeaseRecord {
        job: store_job(&lease.job),
        source_events: lease.source_events.clone(),
        owner: lease.owner.clone(),
        generation: lease.generation,
    }
}

fn source_batch_digest(
    events: &[harness_types::EventEnvelope],
) -> Result<ContentHash, HarnessError> {
    let value = serde_json::Value::Array(
        events
            .iter()
            .map(|event| {
                serde_json::json!({
                    "event_id": event.event_id,
                    "sequence": event.seq,
                    "payload_hash": event.payload_hash,
                })
            })
            .collect(),
    );
    ContentHash::from_canonical_json(&value)
}

fn new_job_id() -> String {
    let event = EventId::generate().to_string();
    format!("memory_job_{}", event.trim_start_matches("event_"))
}

fn parse_layer(value: &str) -> Result<MemoryLayer, HarnessError> {
    match value {
        "l1" => Ok(MemoryLayer::L1),
        "l2" => Ok(MemoryLayer::L2),
        "l3" => Ok(MemoryLayer::L3),
        _ => Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            "stored memory layer is unsupported",
        )),
    }
}

/// A principal without an id cannot own or read anything.
fn validate_principal(principal: &MemoryPrincipal) -> Result<(), HarnessError> {
    if principal.principal_id.trim().is_empty() {
        return Err(HarnessError::new(
            ErrorCode::PolicyDenied,
            "host principal is required",
        ));
    }
    Ok(())
}

fn validate_create(
    principal: &MemoryPrincipal,
    request: &CreateMemoryAsset,
) -> Result<(), HarnessError> {
    if principal.principal_id.trim().is_empty()
        || request.kind.trim().is_empty()
        || request.visibility.trim().is_empty()
        || request.content.trim().is_empty()
        || request.content.len() > 65_536
        || request.provenance_kind.trim().is_empty()
    {
        return Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            "memory principal, kind, visibility, content, and provenance are required",
        ));
    }
    let scope_matches = match request.scope {
        MemoryScope::User => true,
        MemoryScope::Project => {
            request.project_id.is_some() && request.project_id == principal.project_id
        }
        MemoryScope::Task => {
            request.task_id.is_some()
                && request.task_id == principal.task_id
                && request.project_id == principal.project_id
        }
        MemoryScope::AgentProfile => {
            request.agent_profile_id.is_some()
                && request.agent_profile_id == principal.agent_profile_id
        }
        MemoryScope::Session => {
            request.session_id.is_some() && request.session_id == principal.session_id
        }
    };
    if !scope_matches {
        return Err(HarnessError::new(
            ErrorCode::PolicyDenied,
            "memory asset scope must match host-issued principal scope",
        ));
    }
    validate_source_evidence(
        request.evidence,
        &request.source_event_refs,
        &request.source_file_hashes,
        request.source_commit.as_deref(),
    )
}

fn validate_source_evidence(
    evidence: EvidenceState,
    events: &[EventId],
    files: &[ContentHash],
    source_commit: Option<&str>,
) -> Result<(), HarnessError> {
    if evidence == EvidenceState::VerifiedObservation
        && events.is_empty()
        && files.is_empty()
        && source_commit.is_none_or(str::is_empty)
    {
        return Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            "verified memory observation requires a durable source reference",
        ));
    }
    Ok(())
}

fn normalize_search_text(text: &str) -> String {
    let characters = text
        .nfkd()
        .filter(|character| !is_combining_mark(*character))
        .map(|character| match character {
            'đ' => 'd',
            'Đ' => 'D',
            value => value,
        })
        .collect::<Vec<_>>();
    let mut split = String::with_capacity(text.len());
    for (index, character) in characters.iter().copied().enumerate() {
        let previous = index.checked_sub(1).and_then(|index| characters.get(index));
        let next_lower = characters
            .get(index + 1)
            .is_some_and(|next| next.is_lowercase());
        if character.is_uppercase()
            && previous.is_some_and(|previous| {
                previous.is_lowercase()
                    || previous.is_numeric()
                    || (previous.is_uppercase() && next_lower)
            })
        {
            split.push(' ');
        }
        if character.is_alphanumeric() {
            split.extend(character.to_lowercase());
        } else {
            split.push(' ');
        }
    }
    split.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn to_harness_error(error: harness_store_sqlite::StoreError) -> HarnessError {
    error.into_harness_error()
}

impl MemoryPrincipal {
    #[must_use]
    pub fn user(principal_id: impl Into<String>) -> Self {
        Self {
            principal_id: principal_id.into(),
            project_id: None,
            task_id: None,
            agent_profile_id: None,
            session_id: None,
        }
    }

    #[must_use]
    pub fn with_project(mut self, project_id: ProjectId) -> Self {
        self.project_id = Some(project_id);
        self
    }

    #[must_use]
    pub fn with_task(mut self, task_id: TaskId) -> Self {
        self.task_id = Some(task_id);
        self
    }

    #[must_use]
    pub fn with_profile(mut self, profile_id: AgentProfileId) -> Self {
        self.agent_profile_id = Some(profile_id);
        self
    }

    #[must_use]
    pub fn with_session(mut self, session_id: SessionId) -> Self {
        self.session_id = Some(session_id);
        self
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MemoryGrant {
    pub principal_id: String,
    pub memory_asset_id: Option<MemoryAssetId>,
    pub project_id: Option<ProjectId>,
    pub allowed_actions: BTreeSet<MemoryAction>,
    pub revision: u64,
    pub active: bool,
}

impl MemoryGrant {
    #[must_use]
    pub fn allows(&self, principal: &MemoryPrincipal, action: MemoryAction) -> bool {
        self.active
            && self.revision > 0
            && self.principal_id == principal.principal_id
            && self.allowed_actions.contains(&action)
            && self
                .project_id
                .as_ref()
                .is_none_or(|project| principal.project_id.as_ref() == Some(project))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MemoryBinding {
    pub binding_id: String,
    pub memory_asset_id: MemoryAssetId,
    pub principal_id: String,
    pub injection_mode: InjectionMode,
    pub priority: i32,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicationDecision {
    pub status: MemoryAssetStatus,
    pub publish_allowed: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PublicationPolicy;

impl PublicationPolicy {
    #[must_use]
    pub fn classify(
        &self,
        authority: SourceAuthority,
        evidence: EvidenceState,
        layer: MemoryLayer,
        user_confirmed: bool,
    ) -> PublicationDecision {
        let verified_observation = authority == SourceAuthority::RuntimeObserved
            && evidence == EvidenceState::VerifiedObservation;
        let confirmed = authority == SourceAuthority::User
            && evidence == EvidenceState::UserConfirmed
            && user_confirmed;
        let publish_allowed = match layer {
            MemoryLayer::L1 | MemoryLayer::L2 => verified_observation || confirmed,
            MemoryLayer::L3 => confirmed,
        };
        PublicationDecision {
            status: if publish_allowed {
                MemoryAssetStatus::Active
            } else {
                MemoryAssetStatus::Candidate
            },
            publish_allowed,
        }
    }

    #[must_use]
    pub const fn supports_retention(action: RetentionAction) -> bool {
        !matches!(action, RetentionAction::Purge)
    }

    #[must_use]
    pub const fn scopes_are_host_issued() -> bool {
        true
    }

    #[must_use]
    pub const fn supported_scope_count() -> usize {
        let _ = MemoryScope::User;
        5
    }
}
