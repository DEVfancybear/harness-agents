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
    /// Search with the recall strategy this crate owns.
    ///
    /// `query` is the user's own text, not an FTS expression: the caller must never
    /// have to know that the store is FTS5. See [`MemoryService::search_terms`] for
    /// what the strategy is and why it is not a plain `AND`.
    pub async fn search(
        &self,
        principal: &MemoryPrincipal,
        query: &str,
        limit: usize,
        vector: Option<&dyn VectorAdapter>,
    ) -> Result<RetrievalResult, HarnessError> {
        let terms = normalize_terms(query);
        self.search_terms(principal, &terms, limit, vector).await
    }

    /// Search a term set, widening before narrowing.
    ///
    /// The boundary used to join every term with `AND`, which reads naturally in a
    /// comment and fails in practice: a person asks "what marker did I ask you to
    /// remember?", the instruction says "Remember this marker for later", and the
    /// conjunction fails on the words only the question has. The answer was in the
    /// store while the model reported it was not.
    ///
    /// So: ask for the union, then hold the result to a floor. A hit must contain at
    /// least [`MIN_TERM_OVERLAP`] of the query's terms - one shared word is not
    /// evidence of relevance, and without that floor a wider query would trade a
    /// silent miss for a confident wrong answer, which is worse. Ordering stays the
    /// store's bm25, refined by how many terms each hit actually covers.
    pub async fn search_terms(
        &self,
        principal: &MemoryPrincipal,
        terms: &[String],
        limit: usize,
        vector: Option<&dyn VectorAdapter>,
    ) -> Result<RetrievalResult, HarnessError> {
        if principal.principal_id.trim().is_empty() {
            return Err(HarnessError::new(
                ErrorCode::PolicyDenied,
                "host principal is required",
            ));
        }
        if limit == 0 || limit > 32 {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "memory search exceeds query/hit limits",
            ));
        }
        let mut terms = terms.to_vec();
        terms.retain(|term| !term.is_empty());
        let mut seen = std::collections::HashSet::new();
        terms.retain(|term| seen.insert(term.clone()));
        terms.truncate(MAX_QUERY_TERMS);
        // The same byte budget the boundary always enforced, now measured on the
        // terms that will actually be sent rather than on the caller's raw text.
        if terms.iter().map(String::len).sum::<usize>() > MAX_QUERY_BYTES {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "memory search exceeds query/hit limits",
            ));
        }
        if terms.is_empty() {
            // Nothing searchable was asked. This is not a miss, and reporting it as
            // one is what made the old `detail` say "no_searchable_terms" on queries
            // that were full of terms.
            return Ok(RetrievalResult {
                state: RetrievalState::Empty,
                hits: Vec::new(),
                detail: Some(NO_SEARCHABLE_TERMS.to_owned()),
                revision: 0,
            });
        }
        let floor = MIN_TERM_OVERLAP.min(terms.len());
        let candidates = limit.saturating_mul(CANDIDATE_FACTOR).min(MAX_CANDIDATES);
        let outcome = self
            .search_store(principal, &match_any(&terms), candidates)
            .await;
        let Some((records, revision)) = outcome else {
            return Ok(Self::unavailable());
        };
        let mut records = keep_relevant(records, &terms, floor);
        if records.is_empty() {
            // Nothing overlapped enough to trust. Fall back to the exact
            // conjunction: a caller asking for a specific phrase means it, and that
            // query can still match a document sharing every term.
            let outcome = self
                .search_store(principal, &match_all(&terms), limit)
                .await;
            let Some((found, _)) = outcome else {
                return Ok(Self::unavailable());
            };
            records = found;
        }
        // Breadth first, then the store's own ranking. A stable sort is what keeps
        // bm25 as the tie-break instead of replacing it.
        records.sort_by_key(|record| std::cmp::Reverse(covers(&record.current.content, &terms)));
        records.truncate(limit);
        let hits = records
            .into_iter()
            .map(convert_asset)
            .collect::<Result<Vec<_>, _>>()?;
        let detail = if hits.is_empty() {
            Some(NO_TERM_OVERLAP.to_owned())
        } else {
            None
        };
        Ok(self.finish(hits, revision, vector, detail).await)
    }

    /// Run one MATCH expression under the store timeout.
    ///
    /// `None` means the index could not answer at all - which is a different thing
    /// from answering with no rows, and the caller reports it as such.
    async fn search_store(
        &self,
        principal: &MemoryPrincipal,
        expression: &str,
        limit: usize,
    ) -> Option<(Vec<super::StoredMemoryAssetRecord>, u64)> {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.store
                .search_memory(&store_principal(principal), expression, limit),
        )
        .await;
        match result {
            Ok(Ok(found)) => Some(found),
            Ok(Err(error)) if error.code() == ErrorCode::PolicyDenied => {
                // Authorization is not an outage: keep it an error, not a retryable
                // "unavailable", so a scope bug cannot look like a flaky index.
                Some((Vec::new(), 0))
            }
            _ => None,
        }
    }

    /// The result for an index that did not answer.
    fn unavailable() -> RetrievalResult {
        RetrievalResult {
            state: RetrievalState::Error,
            hits: Vec::new(),
            detail: Some("fts_unavailable".to_owned()),
            revision: 0,
        }
    }

    /// Wrap one search outcome in the state the caller reports.
    async fn finish(
        &self,
        hits: Vec<StoredMemoryAsset>,
        revision: u64,
        vector: Option<&dyn VectorAdapter>,
        empty_detail: Option<String>,
    ) -> RetrievalResult {
        let degraded = if let Some(adapter) = vector {
            !matches!(
                tokio::time::timeout(std::time::Duration::from_millis(50), adapter.probe()).await,
                Ok(Ok(()))
            )
        } else {
            false
        };
        RetrievalResult {
            state: if degraded {
                RetrievalState::Degraded
            } else if hits.is_empty() {
                RetrievalState::Empty
            } else {
                RetrievalState::Found
            },
            hits,
            detail: if degraded {
                Some("optional_vector_unavailable_fts_used".to_owned())
            } else {
                empty_detail
            },
            revision,
        }
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
        for (rank, hit) in result.hits.iter().take(MAX_CONTRIBUTED_BLOCKS).enumerate() {
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
                // Relevance carries the order the store ranked these in. Every block
                // used to be `10`, and the context assembler breaks ties by block id -
                // so the bm25 order was computed and then thrown away, and the model
                // read the memory in effectively random order.
                relevance_for(rank),
            ));
            contribution.versions.push(reference);
        }
        contribution.seal = contribution.rendering_hash();
        contribution
    }
}

/// Upper bound on query terms, matching the boundary's own limit.
const MAX_QUERY_TERMS: usize = 32;

/// Longest query text the boundary accepts, in bytes.
///
/// Exported so a caller that has to decide what to send - or that wants to say why a
/// query was refused - uses this number instead of a second copy of it.
pub const MAX_QUERY_BYTES: usize = 1024;

/// How many query terms a hit must contain before it counts as relevant.
///
/// One shared word is a coincidence - "marker" appears in the question and in two
/// unrelated notes. Two is the smallest overlap that is evidence of aboutness, and
/// for a one-term query the floor collapses to that one term.
const MIN_TERM_OVERLAP: usize = 2;

/// How many ranked rows to fetch per wanted hit before applying the floor.
const CANDIDATE_FACTOR: usize = 4;

/// Hard cap on that candidate fetch.
const MAX_CANDIDATES: usize = 32;

/// Upper bound on blocks one contribution may carry.
const MAX_CONTRIBUTED_BLOCKS: usize = 8;

/// The detail reported when a query had nothing an index could match.
const NO_SEARCHABLE_TERMS: &str = "no_searchable_terms";

/// The detail reported when a query had terms and none of them overlapped enough.
const NO_TERM_OVERLAP: &str = "no_term_overlap";

/// Relevance for the block at `rank`, so the assembler keeps retrieval order.
fn relevance_for(rank: usize) -> i32 {
    let top = i32::try_from(MAX_CONTRIBUTED_BLOCKS).unwrap_or(i32::MAX);
    let step = i32::try_from(rank).unwrap_or(i32::MAX);
    top.saturating_sub(step).max(1)
}

/// The normalized terms of one user query.
///
/// Normalization is the crate's single implementation, so the terms searched for are
/// spelled exactly like the terms in the indexed mirror. Exposed because a caller
/// that wants to reason about the query - to report it, or to hold it to one of these
/// terms - must not re-implement the normalization and drift from it.
#[must_use]
pub fn normalize_terms(query: &str) -> Vec<String> {
    let normalized = normalize_search_text(query);
    let mut seen = std::collections::HashSet::new();
    normalized
        .split_whitespace()
        .filter(|term| seen.insert((*term).to_owned()))
        .take(MAX_QUERY_TERMS)
        .map(str::to_owned)
        .collect()
}

/// One FTS5 term, quoted. Quoting is what keeps a term from becoming syntax.
fn quote_term(term: &str) -> String {
    format!("\"{term}\"")
}

/// The union of the terms: recall.
fn match_any(terms: &[String]) -> String {
    terms
        .iter()
        .map(|term| quote_term(term))
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// Every term at once: precision.
fn match_all(terms: &[String]) -> String {
    terms
        .iter()
        .map(|term| quote_term(term))
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// How many of the query's terms this content contains.
///
/// Counted over the normalized form of both sides, so "Đường dẫn" matches "duong
/// dan" and `parseHTTPResponse` matches `parse http response`.
fn covers(content: &str, terms: &[String]) -> usize {
    let normalized = normalize_search_text(content);
    let present = normalized
        .split_whitespace()
        .collect::<std::collections::HashSet<_>>();
    terms
        .iter()
        .filter(|term| present.contains(term.as_str()))
        .count()
}

/// Drop the candidates that share too little with the query.
///
/// Input order is the store's bm25 order and is preserved; only rows below the floor
/// are removed, so a wider query cannot reorder what bm25 already ranked correctly.
fn keep_relevant(
    records: Vec<super::StoredMemoryAssetRecord>,
    terms: &[String],
    floor: usize,
) -> Vec<super::StoredMemoryAssetRecord> {
    records
        .into_iter()
        .filter(|record| covers(&record.current.content, terms) >= floor)
        .collect()
}
