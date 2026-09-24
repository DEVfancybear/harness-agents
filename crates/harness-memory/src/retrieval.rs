use super::{
    ErrorCode, EvidenceState, HarnessError, InjectionMode, MEMORY_BLOCK_HEADING, MemoryBinding,
    MemoryPrincipal, MemoryService, Serialize, StoredMemoryAsset, TURN_PROVENANCE_KIND,
    convert_asset, normalize_search_text, store_principal, to_harness_error,
};
use harness_session::{ContextBlock, ContextBlockKind};
use harness_store_sqlite::RefreshSource;
use harness_types::{ContentHash, MemoryAssetId, MemoryVersionRef};
use std::{future::Future, pin::Pin};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalState {
    Found,
    Empty,
    Degraded,
    Error,
}

/// Which of the store's two kinds of material answered a query.
///
/// Reported rather than assumed, because the two are not equivalent: durable memory is
/// what someone chose to keep, and the conversation log is an excerpt of what was said.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryIndex {
    /// What was published, extracted or asked to be kept.
    Durable,
    /// The conversation log: what was asked and answered.
    Log,
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
                "stamps": self.stamps,
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

/// Why one version was the one injected, and what it was checked against.
///
/// The packet has to be able to say which version of a fact it carried and why
/// that one, because the answer to "why does the model believe this" is otherwise
/// only recoverable by re-running the search - which may select something else by
/// then. `source_digests` are the digests recorded when the version was written,
/// so a later reader can see what the version was built from without the workspace
/// still being in that state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SelectionStamp {
    pub memory_asset_id: MemoryAssetId,
    pub version: u64,
    /// The content hash of the exact version injected.
    pub content_digest: ContentHash,
    pub reason: SelectionReason,
    pub source_digests: Vec<SourceDigest>,
}

/// One named source of the injected version, with the digest read at write time.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SourceDigest {
    pub kind: String,
    pub id: String,
    pub observed_digest: Option<ContentHash>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionReason {
    /// Live memory of a host observation or a user instruction.
    Fresh,
    /// A version a human confirmed; it outranks whatever it superseded.
    UserConfirmed,
    /// A version that exists because a previous one was corrected or replaced.
    UserCorrected,
    /// The conversation log, which answers only when durable memory is silent.
    LogFallback,
}

impl SelectionReason {
    /// The short spelling used inside an injected block.
    ///
    /// Short because it is charged to every block's fixed cost, and because the
    /// long form is already available to a caller through [`SelectionStamp`].
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::UserConfirmed => "confirmed",
            Self::UserCorrected => "corrected",
            Self::LogFallback => "log",
        }
    }
}

#[derive(Clone, Debug)]
pub struct MemoryContribution {
    pub principal: MemoryPrincipal,
    pub blocks: Vec<ContextBlock>,
    pub versions: Vec<MemoryVersionRef>,
    /// One entry per injected block, in the same order as `blocks`.
    pub stamps: Vec<SelectionStamp>,
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
        Ok(self.contribute_indexed(
            principal,
            &RetrievalResult {
                state: RetrievalState::Found,
                hits,
                detail: None,
                revision,
            },
            max_tokens,
            MemoryIndex::Durable,
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
        self.search_terms_fresh(principal, terms, limit, vector, &[])
            .await
    }

    /// Search with the caller's current view of the sources it re-read.
    ///
    /// `refresh` filters, it does not invalidate: a version whose named file changed
    /// is left out of this answer, while the asset keeps its status and its audit
    /// until someone invalidates it on purpose. See `ADR-N07` D2.
    ///
    /// # Errors
    /// Fails when the query is unusable or the store refuses it.
    pub async fn search_terms_fresh(
        &self,
        principal: &MemoryPrincipal,
        terms: &[String],
        limit: usize,
        vector: Option<&dyn VectorAdapter>,
        refresh: &[RefreshSource],
    ) -> Result<RetrievalResult, HarnessError> {
        self.search_terms_in(principal, terms, limit, vector, None, refresh)
            .await
    }

    /// Search, leaving one provenance kind out of the index.
    ///
    /// See [`MemoryService::search_durable_before_log`] for why a caller would want that;
    /// this is the mechanism, and `exclude_provenance` is a kind, not a filter language.
    async fn search_terms_in(
        &self,
        principal: &MemoryPrincipal,
        terms: &[String],
        limit: usize,
        vector: Option<&dyn VectorAdapter>,
        exclude_provenance: Option<&str>,
        refresh: &[RefreshSource],
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
        // Normalize and deduplicate here rather than trusting the caller. `normalize_terms`
        // strips every non-alphanumeric character, so a term that arrived with a quote, a
        // wildcard or an operator in it cannot reach the MATCH expression: an unbalanced
        // quote made the whole query a syntax error, which the store reported as an index
        // outage. A caller that passes raw user words also gets the diacritic folding the
        // indexed mirror uses, instead of silently zero hits.
        let mut normalized = Vec::with_capacity(terms.len());
        for term in terms {
            normalized.extend(normalize_terms(term));
        }
        let mut terms = normalized;
        terms.retain(|term| !term.is_empty());
        let mut seen = std::collections::HashSet::new();
        terms.retain(|term| seen.insert(term.clone()));
        terms.truncate(MAX_QUERY_TERMS); // The same byte budget the boundary always enforced, now measured on the
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
            .search_store(
                principal,
                &match_any(&terms),
                candidates,
                exclude_provenance,
                refresh,
            )
            .await?;
        let Some((records, mut revision)) = outcome else {
            return Ok(Self::unavailable());
        };
        let mut records = keep_relevant(records, &terms, floor);
        if records.is_empty() {
            // Nothing overlapped enough to trust. Fall back to the exact
            // conjunction: a caller asking for a specific phrase means it, and that
            // query can still match a document sharing every term.
            //
            // The fallback's revision replaces the first one. The runtime revalidates a
            // contribution against it before dispatch, so returning rows read at one
            // revision with the number from another makes the whole contribution be
            // dropped in silence - which is the defect this path was written to fix.
            let outcome = self
                .search_store(
                    principal,
                    &match_all(&terms),
                    limit,
                    exclude_provenance,
                    refresh,
                )
                .await?;
            let Some((found, fallback_revision)) = outcome else {
                return Ok(Self::unavailable());
            };
            records = found;
            revision = fallback_revision;
        }
        // Breadth first, then the store's own ranking. A stable sort is what keeps
        // bm25 as the tie-break instead of replacing it. The key is cached: `covers`
        // normalizes the whole content, and recomputing it per comparison made one
        // search normalize each candidate O(log n) times.
        records.sort_by_cached_key(|record| {
            std::cmp::Reverse(covers(&record.current.content, &terms))
        });
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

    /// Answer from durable memory, and from the conversation log only if it is silent.
    ///
    /// The store holds two kinds of material. Durable memory is what someone chose to keep:
    /// the user's own instructions, what the runtime observed, what an extraction published.
    /// The conversation log is what was said, turn by turn, and it quotes the input it
    /// recorded - so a directive and the log entry of the turn that carried it overlap
    /// almost completely, and the log entry is not the weaker match: it holds the directive
    /// plus part of the answer.
    ///
    /// Searching both at once therefore makes the user's instruction compete with its own
    /// echo, decided by a bm25 tie-break. Measured: with a directive and its turn record
    /// both present, the block injected for the directive's own words was sometimes the log
    /// entry, which reaches the model framed as something the user was quoted saying rather
    /// than as an instruction, and which expires at the retention cap.
    ///
    /// So the two are asked in order, not together. Durable memory first: if it holds
    /// anything for this query, that is the answer, and no log entry can displace it. The
    /// log is the fallback for what durable memory never captured - an answer that was given
    /// and never promoted to knowledge - and when it answers, the caller is told, because a
    /// log entry is an excerpt of what was said and not a verified fact.
    ///
    /// An outage is not a silence: a search that failed to run is returned as it is rather
    /// than retried against the other index, which lives in the same table and would fail
    /// the same way.
    ///
    /// # Errors
    /// Fails when the query is unusable or the store refuses it.
    pub async fn search_durable_before_log(
        &self,
        principal: &MemoryPrincipal,
        terms: &[String],
        limit: usize,
        vector: Option<&dyn VectorAdapter>,
    ) -> Result<(MemoryIndex, RetrievalResult), HarnessError> {
        self.search_durable_before_log_fresh(principal, terms, limit, vector, &[])
            .await
    }

    /// Search durable memory before the conversation log using the caller's
    /// current view of source files. Stale assets are removed before ranking.
    pub async fn search_durable_before_log_fresh(
        &self,
        principal: &MemoryPrincipal,
        terms: &[String],
        limit: usize,
        vector: Option<&dyn VectorAdapter>,
        refresh: &[RefreshSource],
    ) -> Result<(MemoryIndex, RetrievalResult), HarnessError> {
        let durable = self
            .search_terms_in(
                principal,
                terms,
                limit,
                vector,
                Some(TURN_PROVENANCE_KIND),
                refresh,
            )
            .await?;
        if durable.state != RetrievalState::Empty {
            return Ok((MemoryIndex::Durable, durable));
        }
        let log = self
            .search_terms_in(principal, terms, limit, vector, None, refresh)
            .await?;
        Ok((MemoryIndex::Log, log))
    }

    /// Run one MATCH expression under the store timeout.
    ///
    /// Three outcomes, not two. `Err` is a refusal and travels as one - flattening it
    /// made an authorization failure indistinguishable from an empty answer, which is
    /// why the caller used to report `no_term_overlap` for a query that was denied.
    /// `Ok(None)` is an index that did not answer, which is an outage. `Ok(Some(..))` is
    /// an answer, including an empty one.
    async fn search_store(
        &self,
        principal: &MemoryPrincipal,
        expression: &str,
        limit: usize,
        exclude_provenance: Option<&str>,
        refresh: &[RefreshSource],
    ) -> Result<Option<(Vec<super::StoredMemoryAssetRecord>, u64)>, HarnessError> {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.store.search_memory_fresh(
                &store_principal(principal),
                expression,
                limit,
                exclude_provenance,
                refresh,
            ),
        )
        .await;
        match result {
            Ok(Ok(found)) => Ok(Some(found)),
            Ok(Err(error)) => Err(to_harness_error(error)),
            Err(_) => Ok(None),
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

    /// Render one turn's memory blocks from a retrieval result.
    ///
    /// The budget is shared, not raced for. A block that did not fit used to be dropped
    /// whole, so one long hit could hide every other hit the same search found - and the
    /// long hits are exactly the ones that carry content: a conversation record is the
    /// user's question followed by the model's answer, and an answer is longer than a
    /// question. Each hit is now offered a fair share of what is left, and a block that
    /// still does not fit is clipped and says so rather than disappearing.
    ///
    /// The share keeps the ranking meaningful: the first hit is offered the largest share,
    /// a hit that does not use its whole share leaves the rest for the hits after it, and
    /// once the remaining share is too small to hold anything but the heading, the loop
    /// stops instead of padding the context with stubs.
    pub fn contribute(
        &self,
        principal: &MemoryPrincipal,
        result: &RetrievalResult,
        max_tokens: u64,
    ) -> MemoryContribution {
        self.contribute_indexed(principal, result, max_tokens, MemoryIndex::Durable)
    }

    /// Render one turn's memory blocks, saying which index answered.
    ///
    /// The index is what decides the selection reason on each stamp: a log entry is
    /// a fallback and a durable version is not, and a packet that reported both the
    /// same way would hide the one fact a reader needs to weigh the block.
    pub fn contribute_indexed(
        &self,
        principal: &MemoryPrincipal,
        result: &RetrievalResult,
        max_tokens: u64,
        index: MemoryIndex,
    ) -> MemoryContribution {
        let mut remaining = max_tokens.min(2000);
        let mut contribution = MemoryContribution {
            principal: principal.clone(),
            blocks: Vec::new(),
            versions: Vec::new(),
            stamps: Vec::new(),
            revision: result.revision,
            seal: ContentHash::from_bytes(b""),
        };
        let mut waiting =
            u64::try_from(result.hits.len().min(MAX_CONTRIBUTED_BLOCKS)).unwrap_or(u64::MAX);
        for (rank, hit) in result.hits.iter().take(MAX_CONTRIBUTED_BLOCKS).enumerate() {
            let share = remaining / waiting.max(1);
            if share < MIN_BLOCK_TOKENS {
                break;
            }
            let stamp = selection_stamp(hit, index);
            let text = render_memory_block(hit, share, stamp.reason);
            // The heading and the source line are fixed costs, and for a small share they
            // are the whole block. Injecting that would spend the budget of every hit
            // after this one to tell the model nothing, so the hit is left out instead.
            if estimate_tokens(&text) > share {
                continue;
            }
            remaining = remaining.saturating_sub(estimate_tokens(&text).min(share));
            waiting = waiting.saturating_sub(1);
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
            contribution.stamps.push(stamp);
            contribution.versions.push(reference);
        }
        contribution.seal = contribution.rendering_hash();
        contribution
    }
}

/// The stamp for one selected hit.
///
/// `UserConfirmed` and `Superseded` are read from the *version*, because they are
/// properties of that version rather than of the query: a version a human confirmed
/// stays confirmed however it was found, and a version that recorded the version it
/// replaced is the correction of it.
fn selection_stamp(hit: &StoredMemoryAsset, index: MemoryIndex) -> SelectionStamp {
    let reason = if hit.current.record.evidence_state == EvidenceState::UserConfirmed.as_str() {
        SelectionReason::UserConfirmed
    } else if hit.current.record.supersedes.is_some() {
        SelectionReason::UserCorrected
    } else if index == MemoryIndex::Log {
        SelectionReason::LogFallback
    } else {
        SelectionReason::Fresh
    };
    SelectionStamp {
        memory_asset_id: hit.asset.memory_asset_id.clone(),
        version: hit.asset.current_version,
        content_digest: hit.current.record.content_hash.clone(),
        reason,
        source_digests: hit
            .current
            .sources
            .iter()
            .map(|source| SourceDigest {
                kind: source.kind.as_str().to_owned(),
                id: source.id.clone(),
                observed_digest: source.observed_digest.clone(),
            })
            .collect(),
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

/// The size estimate used for one rendered block.
///
/// Four bytes to a token, the same rough measure the rest of this crate uses when it has
/// to bound text without a tokenizer.
const BYTES_PER_TOKEN: u64 = 4;

/// The smallest block worth injecting.
///
/// Every block opens with the heading that says what this material is and how to read it,
/// and that heading is most of a small block. Below this, a block is heading and nothing
/// else - it spends the budget and tells the model nothing.
const MIN_BLOCK_TOKENS: u64 = 32;

/// Appended to a block that was shortened to fit the turn's budget.
///
/// A memory that stops mid-sentence and does not say so is read as a memory that ends
/// there, which is how a clipped answer becomes a wrong answer.
const CLIPPED_MARKER: &str =
    "\n[truncated: the rest of this memory did not fit this turn's budget]";

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

/// The size estimate for one rendered block.
fn estimate_tokens(text: &str) -> u64 {
    u64::try_from(text.len().div_ceil(4)).unwrap_or(u64::MAX)
}

/// Render one hit for injection, clipped to `share` tokens when it does not fit.
///
/// The heading leads with what the material is and what to do with it, and keeps
/// provenance as a trailing note: provenance is for audit, not for the model to weigh
/// before deciding whether to read on. A clip therefore shortens the *content* and keeps
/// both ends: the heading is what tells the model how to read what follows, and the
/// source line is what an auditor follows back to the version this came from.
///
/// The note is kept short on purpose. It is a fixed cost charged to every block, and
/// the budget is shared: a first hit offered an eighth of 800 tokens has room for about
/// a hundred, so a verbose note spends the whole share and the hit is dropped for
/// telling the model nothing. Version, digest prefix and reason are what a reader needs
/// to identify the selection; the event refs are already carried by the block id.
fn render_memory_block(hit: &StoredMemoryAsset, share: u64, reason: SelectionReason) -> String {
    let head = format!("{MEMORY_BLOCK_HEADING}\n");
    let digest = hit.current.record.content_hash.as_str();
    // A fact a model extracted says how sure it was: the reader weighs a 0.72 guess
    // differently from something the user typed.
    let confidence = hit
        .current
        .record
        .confidence_annotation
        .as_deref()
        .map(|confidence| format!(", confidence={confidence}"))
        .unwrap_or_default();
    let tail = format!(
        "\n(source: authority={:?}, validity={:?}{confidence}, v{} {} {})",
        hit.asset.created_by,
        hit.current.record.validity,
        hit.asset.current_version,
        reason.as_str(),
        digest.get(..19).unwrap_or(digest),
    );
    let whole = format!("{head}{}{tail}", hit.current.content);
    if estimate_tokens(&whole) <= share {
        return whole;
    }
    let fixed = head.len() + tail.len() + CLIPPED_MARKER.len();
    let room = usize::try_from(share.saturating_mul(BYTES_PER_TOKEN))
        .unwrap_or(usize::MAX)
        .saturating_sub(fixed);
    let content = clip_chars(&hit.current.content, room);
    format!("{head}{content}{CLIPPED_MARKER}{tail}")
}

/// The first `room` characters of `text`, never splitting one.
fn clip_chars(text: &str, room: usize) -> String {
    if text.len() <= room {
        return text.to_owned();
    }
    let mut end = room.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
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
        .filter(|term| !STOPWORDS.contains(term))
        .filter(|term| seen.insert((*term).to_owned()))
        .take(MAX_QUERY_TERMS)
        .map(str::to_owned)
        .collect()
}

/// Function words left out of a query, in their folded spelling.
///
/// Vietnamese is indexed one syllable at a time, and its function syllables are in
/// almost every sentence: "hãy ... cho tôi" shares three terms with any other request
/// phrased politely. The overlap floor of two terms was therefore met by grammar alone,
/// and an unrelated old input was injected as memory. Only words that carry no subject
/// are listed; a syllable that is also half of a content word in common use (`an` in
/// "dự án", `du` in "dữ liệu") is kept, because dropping it would lose the subject.
const STOPWORDS: &[&str] = &[
    // Vietnamese, diacritics folded
    // ("ban", "anh", "de", "chu", "tu" are left out: they fold together with "bản",
    // "ảnh", "đề", "chủ", "từ", which name subjects.)
    "hay", "cho", "toi", "minh", "la", "cua", "va", "voi", "cac", "nhung", "mot", "nay", "do",
    "thi", "ma", "duoc", "co", "khong", "se", "da", "dang", "rat", "cung", "nhu", "trong", "tren",
    "ve", "gi", "nao", "sao", "the", "vay", "nhe", "a", "oi", "giup", "xin", "em", "chi", "nhi",
    "roi", "con", "neu", "khi", // English
    "i", "me", "my", "you", "your", "we", "our", "it", "its", "this", "that", "these", "those",
    "is", "are", "was", "were", "be", "been", "am", "to", "of", "and", "or", "in", "on", "at",
    "for", "with", "by", "from", "as", "please", "can", "could", "would", "should", "will", "do",
    "does", "did", "what", "how", "why", "when", "where", "which", "who", "about",
];

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
