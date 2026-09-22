use std::{future::Future, pin::Pin};

use super::{
    AgentProfileId, BTreeSet, ContentHash, Deserialize, ErrorCode, EventId, EvidenceState,
    ExtractionFailureKind, ExtractionLease, ExtractionScope, HarnessError, MEMORY_CONTRACT_VERSION,
    MemoryAsset, MemoryAssetId, MemoryAssetStatus, MemoryLayer, MemoryPrincipal, MemoryScope,
    MemoryService, MemoryVersion, ProjectId, Serialize, SessionId, SourceAuthority,
    StoredMemoryAsset, StoredMemoryAssetRecord, StoredMemoryVersionRecord, TaskId, Validity,
    convert_asset, ensure_source_is_durable, normalize_search_text, store_lease, store_principal,
    to_harness_error,
};

/// The scope one derived asset inherits from the source a caller named first.
struct InheritedScope {
    scope: MemoryScope,
    project_id: Option<ProjectId>,
    task_id: Option<TaskId>,
    session_id: Option<SessionId>,
    agent_profile_id: Option<AgentProfileId>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractedCandidate {
    pub content: String,
    pub source_event_refs: Vec<EventId>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionOutput {
    pub candidates: Vec<ExtractedCandidate>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SourceProjection {
    pub event_id: EventId,
    pub sequence: u64,
    pub content: String,
}

/// External inference boundary. The host owns scope, source validation and publication.
pub trait MemoryExtractor: Send + Sync {
    fn version(&self) -> &str;
    fn extract<'a>(
        &'a self,
        sources: &'a [SourceProjection],
    ) -> Pin<Box<dyn Future<Output = Result<String, HarnessError>> + Send + 'a>>;
}

impl MemoryService {
    #[allow(clippy::too_many_lines)] // Source validation, inference and one settlement share a lease.
    pub async fn extract_lease(
        &self,
        principal: &MemoryPrincipal,
        lease: &ExtractionLease,
        extractor: &dyn MemoryExtractor,
        max_output_bytes: usize,
        asset_scope: ExtractionScope,
    ) -> Result<Vec<StoredMemoryAsset>, HarnessError> {
        for event in &lease.source_events {
            event.validate()?;
        }
        if super::source_batch_digest(&lease.source_events)? != lease.job.source_digest {
            return Err(HarnessError::new(
                ErrorCode::InvalidHash,
                "extraction source batch changed",
            ));
        }
        if principal.session_id.as_ref() != Some(&lease.job.source_stream) {
            return Err(HarnessError::new(
                ErrorCode::PolicyDenied,
                "extraction stream is outside host scope",
            ));
        }
        if extractor.version() != lease.job.extractor_version {
            self.fail_job(
                lease,
                ExtractionFailureKind::Unavailable,
                "extractor version changed",
            )
            .await?;
            return Err(HarnessError::new(
                ErrorCode::ServiceUnavailable,
                "extractor version changed",
            ));
        }
        let sources = project_sources(lease);
        if sources.is_empty() {
            // A range the host committed but that projects no eligible source - a
            // range of rendered packets, provider output or memory injections - is
            // `filtered`, not `no_facts`. The two are different dispositions on
            // purpose: a human reading the cursor later has to be able to tell a
            // range the extractor declined to read from a range it read and found
            // nothing in.
            self.settle_filtered(lease).await?;
            return Ok(Vec::new());
        }
        let output = match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            extractor.extract(&sources),
        )
        .await
        {
            Ok(Ok(output)) => output,
            result => {
                let kind = if matches!(result, Ok(Err(ref error)) if error.code() == ErrorCode::ServiceUnavailable)
                {
                    ExtractionFailureKind::Unavailable
                } else {
                    ExtractionFailureKind::Failed
                };
                self.fail_job(lease, kind, "extractor unavailable, failed or timed out")
                    .await?;
                return Err(HarnessError::new(
                    ErrorCode::ServiceUnavailable,
                    "extractor unavailable, failed or timed out",
                ));
            }
        };
        let parsed = (output.len() <= max_output_bytes.min(65_536))
            .then(|| serde_json::from_str::<ExtractionOutput>(&output).ok())
            .flatten();
        let Some(parsed) = parsed.filter(|parsed| {
            parsed.candidates.len() <= 32
                && parsed.candidates.iter().all(|candidate| {
                    !candidate.content.trim().is_empty()
                        && candidate.content.len() <= 8192
                        && !candidate.source_event_refs.is_empty()
                        && candidate
                            .source_event_refs
                            .iter()
                            .all(|id| sources.iter().any(|source| &source.event_id == id))
                })
        }) else {
            self.fail_job(
                lease,
                ExtractionFailureKind::InvalidOutput,
                "invalid extractor output or source references",
            )
            .await?;
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "invalid extractor output or source references",
            ));
        };
        let mut seen = BTreeSet::new();
        let mut self_referential = 0usize;
        let mut records = Vec::new();
        for candidate in parsed.candidates {
            if !seen.insert((
                candidate.content.clone(),
                candidate.source_event_refs.clone(),
            )) {
                continue;
            }
            // A candidate whose text is already memory is the model quoting memory
            // back, not new evidence. Storing it would let one injected block become
            // an independent fact that a later extraction cites as its own source,
            // which is self-reinforcement: run the loop twice and a quoted sentence
            // is indistinguishable from something the runtime observed.
            //
            // The lookup is the same reachability rule a search uses, so a candidate
            // that duplicates an asset the principal may not read is a new candidate
            // and not a shadow of something else. It is deliberately *not* the
            // active-only lookup: the thing this guard has to catch is the candidate
            // the previous pass wrote.
            if self
                .find_any_by_content(principal, &candidate.content)
                .await?
                .is_some()
            {
                self_referential += 1;
                continue;
            }
            let mut record = candidate_record(
                principal,
                MemoryLayer::L1,
                &candidate.content,
                &candidate.source_event_refs,
                asset_scope,
            );
            record.current.record.extractor_version = Some(lease.job.extractor_version.clone());
            record.current.strategy_digest = Some(lease.job.strategy_digest.clone());
            records.push(record);
        }
        if records.is_empty() && self_referential > 0 {
            // Everything the extractor proposed was already memory. The range is
            // covered, and the disposition says why nothing was published, because a
            // cursor that advanced over an unrecorded reason is a range nobody can
            // account for later.
            self.store
                .settle_extraction_self_referential(&store_lease(lease), self_referential)
                .await
                .map_err(to_harness_error)?;
            return Ok(Vec::new());
        }
        self.store
            .settle_extraction_assets(&store_lease(lease), &records)
            .await
            .map_err(to_harness_error)?;
        records.into_iter().map(convert_asset).collect()
    }

    pub async fn derive_l2(
        &self,
        principal: &MemoryPrincipal,
        sources: &[harness_types::MemoryVersionRef],
        content: &str,
    ) -> Result<StoredMemoryAsset, HarnessError> {
        if sources.is_empty()
            || sources.len() > 32
            || content.trim().is_empty()
            || content.len() > 8192
        {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "bounded L2 content and sources are required",
            ));
        }
        let mut events = BTreeSet::new();
        let mut files = Vec::new();
        let mut inherited: Option<InheritedScope> = None;
        for source in sources {
            let asset = self
                .read(principal, &source.memory_asset_id)
                .await?
                .ok_or_else(|| {
                    HarnessError::new(ErrorCode::InvalidPayload, "L2 source not found")
                })?;
            if asset.asset.current_version != source.version {
                return Err(HarnessError::new(
                    ErrorCode::SequenceConflict,
                    "L2 source version changed",
                ));
            }
            // A turn record is a log entry with a retention cap, so it is the one asset
            // that is guaranteed to be retired eventually - and a derived memory dies with
            // its source. Refusing here is the difference between a clear answer now and a
            // summary that disappears two hundred turns later with no explanation.
            ensure_source_is_durable(&asset)?;
            events.extend(asset.current.record.source_event_refs);
            files.extend(asset.current.record.source_file_hashes);
            // A summary is never wider than the evidence it summarises, so it takes the
            // scope of the source the caller named first.
            if inherited.is_none() {
                inherited = Some(InheritedScope {
                    scope: asset.asset.scope,
                    project_id: asset.asset.project_id.clone(),
                    task_id: asset.task_id.clone(),
                    session_id: asset.session_id.clone(),
                    agent_profile_id: asset.agent_profile_id.clone(),
                });
            }
        }
        let ordered_events = events.into_iter().collect::<Vec<EventId>>();
        let mut record = candidate_record(
            principal,
            MemoryLayer::L2,
            content,
            &ordered_events,
            ExtractionScope::Session,
        );
        if let Some(inherited) = inherited {
            record.asset.scope = inherited.scope;
            record.asset.project_id = inherited.project_id;
            record.task_id = inherited.task_id;
            record.session_id = inherited.session_id;
            record.agent_profile_id = inherited.agent_profile_id;
        }
        files.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        files.dedup();
        record.current.record.source_file_hashes = files;
        EvidenceState::Derived
            .as_str()
            .clone_into(&mut record.current.record.evidence_state);
        let record = self
            .store
            .create_derived_memory(&store_principal(principal), record, sources)
            .await
            .map_err(to_harness_error)?;
        convert_asset(record)
    }
}

pub(crate) fn project_sources(lease: &ExtractionLease) -> Vec<SourceProjection> {
    lease
        .source_events
        .iter()
        .filter_map(|event| {
            // Only original admitted text and host observations are eligible. Rendered packets,
            // provider output, memory injection and summaries never become independent evidence.
            let text = match (event.event_type.as_str(), event.authority) {
                ("input.admitted", SourceAuthority::User) => {
                    event.payload.get("text")?.as_str()?.to_owned()
                }
                (
                    "receipt.recorded" | "decision.updated",
                    SourceAuthority::RuntimeObserved | SourceAuthority::User,
                ) => serde_json::to_string(&event.payload).ok()?,
                (_, SourceAuthority::RuntimeObserved)
                    if event.event_type.ends_with(".observation") =>
                {
                    event.payload.get("observation")?.as_str()?.to_owned()
                }
                _ => return None,
            };
            Some(SourceProjection {
                event_id: event.event_id.clone(),
                sequence: event.seq,
                content: sanitize_memory_text(&text),
            })
        })
        .collect()
}

/// Redact the lines of a memory value that look like they carry a secret.
///
/// Exposed because a caller that has to compare text before storing it must compare
/// the text that will actually be stored. `create_asset` runs this before hashing and
/// before writing the search mirror, so a lookup keyed on the raw text can never match
/// a redacted value: every repeat of such an input minted another asset, which is the
/// duplication this boundary exists to prevent.
pub fn sanitize_memory_text(text: &str) -> String {
    text.lines()
        .map(|line| {
            let lower = line.to_ascii_lowercase();
            if [
                "secret",
                "password",
                "api_key",
                "credential",
                "authorization",
                "bearer ",
                "token=",
            ]
            .iter()
            .any(|key| lower.contains(key))
            {
                "[REDACTED]"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn candidate_record(
    principal: &MemoryPrincipal,
    layer: MemoryLayer,
    content: &str,
    sources: &[EventId],
    asset_scope: ExtractionScope,
) -> StoredMemoryAssetRecord {
    let id = MemoryAssetId::generate();
    let content = sanitize_memory_text(content);
    let hash = ContentHash::from_bytes(content.as_bytes());
    // The stream a job reads authorises the extraction, but it does not decide where the
    // result belongs: binding every asset to the source session silently made extracted
    // knowledge unreadable from any other session.
    let (scope, task_id, session_id) = match asset_scope {
        ExtractionScope::Session => (
            MemoryScope::Session,
            principal.task_id.clone(),
            principal.session_id.clone(),
        ),
        ExtractionScope::Project => (
            if principal.project_id.is_some() {
                MemoryScope::Project
            } else {
                MemoryScope::User
            },
            None,
            None,
        ),
    };
    StoredMemoryAssetRecord {
        asset: MemoryAsset {
            schema_version: MEMORY_CONTRACT_VERSION,
            memory_asset_id: id.clone(),
            kind: if layer == MemoryLayer::L1 {
                "atomic_candidate"
            } else {
                "derived_summary"
            }
            .to_owned(),
            owner_id: principal.principal_id.clone(),
            project_id: principal.project_id.clone(),
            scope,
            visibility: "scoped".to_owned(),
            status: MemoryAssetStatus::Candidate,
            current_version: 1,
            created_by: SourceAuthority::ModelProposed,
        },
        layer: layer.as_str().to_owned(),
        task_id,
        agent_profile_id: principal.agent_profile_id.clone(),
        session_id,
        current: StoredMemoryVersionRecord {
            record: MemoryVersion {
                schema_version: MEMORY_CONTRACT_VERSION,
                memory_asset_id: id,
                version: 1,
                content_or_artifact_hash: hash.clone(),
                content_hash: hash,
                source_event_refs: sources.to_vec(),
                source_file_hashes: Vec::new(),
                source_commit: None,
                provenance_kind: "journal_derived".to_owned(),
                evidence_state: EvidenceState::ModelInference.as_str().to_owned(),
                confidence_annotation: None,
                validity: Validity::Valid,
                supersedes: None,
                extractor_version: None,
            },
            normalized_content: normalize_search_text(&content),
            content,
            strategy_digest: None,
            // Every extracted candidate names the journal events it came from, so
            // the versions that depend on those events are queryable without
            // re-deriving lineage from the version JSON.
            sources: sources
                .iter()
                .map(|event| super::MemorySource::event(event, 0).to_record())
                .collect(),
        },
    }
}
