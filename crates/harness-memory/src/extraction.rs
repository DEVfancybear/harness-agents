use std::{future::Future, pin::Pin};

use super::{
    BTreeSet, ContentHash, Deserialize, ErrorCode, EventId, EvidenceState, ExtractionFailureKind,
    ExtractionLease, HarnessError, MEMORY_CONTRACT_VERSION, MemoryAsset, MemoryAssetId,
    MemoryAssetStatus, MemoryLayer, MemoryPrincipal, MemoryScope, MemoryService, MemoryVersion,
    Serialize, SourceAuthority, StoredMemoryAsset, StoredMemoryAssetRecord,
    StoredMemoryVersionRecord, Validity, convert_asset, normalize_search_text, store_lease,
    store_principal, to_harness_error,
};

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
            self.settle_no_facts(lease).await?;
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
        let records = parsed
            .candidates
            .into_iter()
            .filter(|candidate| {
                seen.insert((
                    candidate.content.clone(),
                    candidate.source_event_refs.clone(),
                ))
            })
            .map(|candidate| {
                let mut record = candidate_record(
                    principal,
                    MemoryLayer::L1,
                    &candidate.content,
                    candidate.source_event_refs,
                );
                record.current.record.extractor_version = Some(lease.job.extractor_version.clone());
                record.current.strategy_digest = Some(lease.job.strategy_digest.clone());
                record
            })
            .collect::<Vec<_>>();
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
            events.extend(asset.current.record.source_event_refs);
            files.extend(asset.current.record.source_file_hashes);
        }
        let mut record = candidate_record(
            principal,
            MemoryLayer::L2,
            content,
            events.into_iter().collect(),
        );
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

pub(crate) fn sanitize_memory_text(text: &str) -> String {
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
    sources: Vec<EventId>,
) -> StoredMemoryAssetRecord {
    let id = MemoryAssetId::generate();
    let content = sanitize_memory_text(content);
    let hash = ContentHash::from_bytes(content.as_bytes());
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
            scope: if principal.session_id.is_some() {
                MemoryScope::Session
            } else if principal.project_id.is_some() {
                MemoryScope::Project
            } else {
                MemoryScope::User
            },
            visibility: "scoped".to_owned(),
            status: MemoryAssetStatus::Candidate,
            current_version: 1,
            created_by: SourceAuthority::ModelProposed,
        },
        layer: layer.as_str().to_owned(),
        task_id: principal.task_id.clone(),
        agent_profile_id: principal.agent_profile_id.clone(),
        session_id: principal.session_id.clone(),
        current: StoredMemoryVersionRecord {
            record: MemoryVersion {
                schema_version: MEMORY_CONTRACT_VERSION,
                memory_asset_id: id,
                version: 1,
                content_or_artifact_hash: hash.clone(),
                content_hash: hash,
                source_event_refs: sources,
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
        },
    }
}
