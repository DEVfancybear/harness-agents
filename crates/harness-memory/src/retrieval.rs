use super::{
    ErrorCode, HarnessError, InjectionMode, MemoryBinding, MemoryPrincipal, MemoryService,
    Serialize, StoredMemoryAsset, convert_asset, normalize_search_text, store_principal,
    to_harness_error,
};
use harness_session::{ContextBlock, ContextBlockKind};
use harness_types::{ContentHash, MemoryVersionRef};
use std::{future::Future, pin::Pin};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalState {
    Found,
    Empty,
    Degraded,
    Error,
}

#[derive(Clone, Debug)]
pub struct RetrievalResult {
    pub state: RetrievalState,
    pub hits: Vec<StoredMemoryAsset>,
    pub detail: Option<String>,
    pub revision: u64,
}

impl MemoryContribution {
    fn rendering_hash(&self) -> ContentHash {
        ContentHash::from_bytes(
            serde_json::json!({
                "principal": self.principal,
                "blocks": self.blocks,
                "versions": self.versions,
                "revision": self.revision,
            })
            .to_string()
            .as_bytes(),
        )
    }

    pub(crate) fn validate_rendering(&self) -> Result<(), HarnessError> {
        if self.seal != self.rendering_hash() {
            return Err(HarnessError::new(
                ErrorCode::InvalidHash,
                "memory contribution changed after rendering",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct MemoryContribution {
    pub principal: MemoryPrincipal,
    pub blocks: Vec<ContextBlock>,
    pub versions: Vec<MemoryVersionRef>,
    pub revision: u64,
    seal: ContentHash,
}

/// Optional vector service probe. P4 ships lexical retrieval only.
pub trait VectorAdapter: Send + Sync {
    fn probe(&self) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + '_>>;
}

impl MemoryService {
    pub async fn bootstrap(
        &self,
        principal: &MemoryPrincipal,
        max_tokens: u64,
    ) -> Result<MemoryContribution, HarnessError> {
        let (records, revision) = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.store.bound_memory(&store_principal(principal)),
        )
        .await
        .map_err(|_| {
            HarnessError::new(ErrorCode::ServiceUnavailable, "memory bootstrap timed out")
        })?
        .map_err(to_harness_error)?;
        let mut hits = Vec::new();
        for (record, mode) in records {
            let mut asset = convert_asset(record)?;
            if mode == "index" {
                asset.current.content = format!(
                    "Memory index: {} {} v{}; read on demand",
                    asset.asset.kind, asset.asset.memory_asset_id, asset.asset.current_version
                );
            }
            hits.push(asset);
        }
        Ok(self.contribute(
            principal,
            &RetrievalResult {
                state: RetrievalState::Found,
                hits,
                detail: None,
                revision,
            },
            max_tokens,
        ))
    }
    pub async fn search(
        &self,
        principal: &MemoryPrincipal,
        query: &str,
        limit: usize,
        vector: Option<&dyn VectorAdapter>,
    ) -> Result<RetrievalResult, HarnessError> {
        if principal.principal_id.trim().is_empty() {
            return Err(HarnessError::new(
                ErrorCode::PolicyDenied,
                "host principal is required",
            ));
        }
        if limit == 0 || limit > 32 || query.len() > 1024 {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "memory search exceeds query/hit limits",
            ));
        }
        let normalized = normalize_search_text(query);
        let terms = normalized
            .split_whitespace()
            .take(32)
            .map(|term| format!("\"{term}\""))
            .collect::<Vec<_>>()
            .join(" AND ");
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.store
                .search_memory(&store_principal(principal), &terms, limit),
        )
        .await;
        let (records, revision) = match result {
            Ok(Ok(result)) => result,
            Ok(Err(error)) if error.code() == ErrorCode::PolicyDenied => {
                return Err(to_harness_error(error));
            }
            _ => {
                return Ok(RetrievalResult {
                    state: RetrievalState::Error,
                    hits: Vec::new(),
                    detail: Some("fts_unavailable".to_owned()),
                    revision: 0,
                });
            }
        };
        let hits = records
            .into_iter()
            .map(convert_asset)
            .collect::<Result<Vec<_>, _>>()?;
        let degraded = if let Some(adapter) = vector {
            !matches!(
                tokio::time::timeout(std::time::Duration::from_millis(50), adapter.probe()).await,
                Ok(Ok(()))
            )
        } else {
            false
        };
        Ok(RetrievalResult {
            state: if degraded {
                RetrievalState::Degraded
            } else if hits.is_empty() {
                RetrievalState::Empty
            } else {
                RetrievalState::Found
            },
            hits,
            detail: degraded.then(|| "optional_vector_unavailable_fts_used".to_owned()),
            revision,
        })
    }

    pub async fn bind(
        &self,
        principal: &MemoryPrincipal,
        binding: &MemoryBinding,
    ) -> Result<(), HarnessError> {
        let mode = match binding.injection_mode {
            InjectionMode::Bootstrap => "bootstrap",
            InjectionMode::Index => "index",
            InjectionMode::OnDemand => "on_demand",
        };
        self.store
            .bind_memory(
                &store_principal(principal),
                &binding.binding_id,
                &binding.memory_asset_id,
                &binding.principal_id,
                mode,
                binding.priority,
                binding.revision,
            )
            .await
            .map_err(to_harness_error)
    }

    pub fn contribute(
        &self,
        principal: &MemoryPrincipal,
        result: &RetrievalResult,
        max_tokens: u64,
    ) -> MemoryContribution {
        let mut remaining = max_tokens.min(2000);
        let mut contribution = MemoryContribution {
            principal: principal.clone(),
            blocks: Vec::new(),
            versions: Vec::new(),
            revision: result.revision,
            seal: ContentHash::from_bytes(b""),
        };
        for hit in result.hits.iter().take(8) {
            let text = format!(
                "Reusable data; authority={:?}; validity={:?}; sources={:?}\n{}",
                hit.asset.created_by,
                hit.current.record.validity,
                hit.current.record.source_event_refs,
                hit.current.content
            );
            let tokens = u64::try_from(text.len().div_ceil(4)).unwrap_or(u64::MAX);
            if tokens > remaining {
                continue;
            }
            remaining -= tokens;
            let reference = MemoryVersionRef {
                memory_asset_id: hit.asset.memory_asset_id.clone(),
                version: hit.asset.current_version,
            };
            contribution.blocks.push(ContextBlock::optional(
                format!("{}@{}", reference.memory_asset_id, reference.version),
                ContextBlockKind::Memory,
                text,
                10,
            ));
            contribution.versions.push(reference);
        }
        contribution.seal = contribution.rendering_hash();
        contribution
    }
}
