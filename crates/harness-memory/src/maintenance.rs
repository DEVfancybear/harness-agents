use super::{
    ErrorCode, ExtractionJobStatus, ExtractionStrategy, HarnessError, MemoryAssetId,
    MemoryContribution, MemoryExtractor, MemoryPrincipal, MemoryService, Serialize, store_lease,
    store_principal, to_harness_error,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub struct MemoryBudget {
    pub max_calls: u32,
    pub max_source_bytes: usize,
    pub max_output_bytes: usize,
    pub max_tokens: u64,
    pub max_cost_units: u64,
    pub cost_units_per_call: u64,
}

impl MemoryBudget {
    pub fn calls(calls: u32) -> Self {
        Self {
            max_calls: calls,
            max_source_bytes: 65_536,
            max_output_bytes: 16_384,
            max_tokens: 32_768,
            max_cost_units: u64::from(calls),
            cost_units_per_call: 1,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct CatchUpReport {
    /// Ranges this call had to enqueue because nothing else had.
    pub enqueued: usize,
    pub calls: u32,
    pub completed: u32,
    pub failed: u32,
    pub paused: u32,
    pub source_bytes: usize,
    pub reserved_tokens: u64,
    pub reserved_cost_units: u64,
    pub asset_ids: Vec<MemoryAssetId>,
}

impl MemoryService {
    pub async fn invalidate(
        &self,
        principal: &MemoryPrincipal,
        id: &MemoryAssetId,
        reason: &str,
    ) -> Result<Vec<MemoryAssetId>, HarnessError> {
        self.store
            .invalidate_memory(&store_principal(principal), id, reason)
            .await
            .map_err(to_harness_error)
    }
    pub async fn invalidate_source(
        &self,
        principal: &MemoryPrincipal,
        kind: &str,
        old_hash_or_revision: &str,
    ) -> Result<Vec<MemoryAssetId>, HarnessError> {
        self.store
            .invalidate_memory_source(&store_principal(principal), kind, old_hash_or_revision)
            .await
            .map_err(to_harness_error)
    }
    pub async fn validate_contribution(
        &self,
        contribution: &MemoryContribution,
    ) -> Result<(), HarnessError> {
        contribution.validate_rendering()?;
        self.store
            .validate_memory_snapshot(
                &store_principal(&contribution.principal),
                &contribution.versions,
                contribution.revision,
            )
            .await
            .map_err(to_harness_error)
    }
    #[allow(clippy::too_many_lines)] // Keep budget reservation and cancellation settlement in one flow.
    pub async fn catch_up(
        &self,
        principal: &MemoryPrincipal,
        strategy: &ExtractionStrategy,
        extractor: &dyn MemoryExtractor,
        budget: &MemoryBudget,
        cancel: &CancellationToken,
    ) -> Result<CatchUpReport, HarnessError> {
        let stream = principal.session_id.as_ref().ok_or_else(|| {
            HarnessError::new(
                ErrorCode::PolicyDenied,
                "catch-up requires a host session scope",
            )
        })?;
        if budget.max_calls > 1000
            || budget.max_output_bytes > 65_536
            || budget.max_source_bytes > 1_048_576
            || budget.cost_units_per_call == 0
        {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "memory budget must be finite and within host limits",
            ));
        }
        let mut report = CatchUpReport::default();
        let mut budget_exhausted = budget.max_calls == 0;
        // Reconcile first: the backlog is whatever the journal committed and this
        // host has not settled. Doing it here, rather than trusting a previous
        // enqueue, is what makes a crash between commit and enqueue recoverable -
        // the range is still on disk, so the next call creates the same job.
        let reconciled = self.reconcile(stream, strategy, 64).await?;
        report.enqueued = reconciled.enqueued;
        for job in reconciled.outstanding {
            if cancel.is_cancelled() || report.calls >= budget.max_calls {
                break;
            }
            // The ranges this call may act on. `blocked` is included on purpose: it
            // means the extractor was not there, and an explicit catch-up is the
            // command that says it is there now. `leased` and `dead_letter` are not:
            // the first belongs to another consumer, and the second is a range that
            // has already failed its bounded number of attempts - retrying it here
            // would either loop forever or silently exceed the retry policy.
            if !matches!(
                job.status,
                ExtractionJobStatus::Pending
                    | ExtractionJobStatus::Paused
                    | ExtractionJobStatus::RetryWait
                    | ExtractionJobStatus::Blocked
            ) {
                break;
            }
            let lease = match self.lease_job(&job.job_id, "foreground-memory").await {
                Ok(lease) => lease,
                Err(error) if error.code() == ErrorCode::RuntimeCommandConflict => break,
                Err(error) => return Err(error),
            };
            let source_bytes = lease
                .source_events
                .iter()
                .map(|event| serde_json::to_vec(event).map_or(usize::MAX, |bytes| bytes.len()))
                .fold(0usize, usize::saturating_add);
            let reserved_tokens = u64::try_from(
                source_bytes
                    .saturating_add(budget.max_output_bytes)
                    .div_ceil(4),
            )
            .unwrap_or(u64::MAX);
            if report.source_bytes.saturating_add(source_bytes) > budget.max_source_bytes
                || report.reserved_tokens.saturating_add(reserved_tokens) > budget.max_tokens
                || report
                    .reserved_cost_units
                    .saturating_add(budget.cost_units_per_call)
                    > budget.max_cost_units
            {
                budget_exhausted = true;
                self.store
                    .fail_extraction_job(&store_lease(&lease), "paused", "budget_exhausted")
                    .await
                    .map_err(to_harness_error)?;
                break;
            }
            report.calls += 1;
            report.source_bytes += source_bytes;
            report.reserved_tokens += reserved_tokens;
            report.reserved_cost_units += budget.cost_units_per_call;
            let result = tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    self.store.fail_extraction_job(&store_lease(&lease), "paused", "shutdown").await.map_err(to_harness_error)?;
                    break;
                },
                result = self.extract_lease(principal, &lease, extractor, budget.max_output_bytes, strategy.asset_scope) => result,
            };
            match result {
                Ok(assets) => {
                    report.completed += 1;
                    report
                        .asset_ids
                        .extend(assets.into_iter().map(|asset| asset.asset.memory_asset_id));
                }
                Err(error)
                    if matches!(
                        error.code(),
                        ErrorCode::InvalidPayload | ErrorCode::ServiceUnavailable
                    ) =>
                {
                    report.failed += 1;
                    break;
                }
                Err(error) => return Err(error),
            }
        }
        // Failure retries keep backoff; only exhaustion/shutdown turns remaining ranges paused.
        if report.failed == 0
            && (budget_exhausted || report.calls >= budget.max_calls || cancel.is_cancelled())
        {
            report.paused = u32::try_from(
                self.store
                    .pause_extraction_jobs(
                        stream,
                        &strategy.extractor_version,
                        &strategy.strategy_digest,
                    )
                    .await
                    .map_err(to_harness_error)?,
            )
            .unwrap_or(u32::MAX);
        }
        Ok(report)
    }
}
