use std::collections::BTreeSet;

use super::{
    ContentHash, ErrorCode, MemoryAssetId, MemoryAssetStatus, RefreshSource, Row, SessionId,
    Sqlite, SqliteStore, StoreError, StoreMemoryPrincipal, StoredMemoryAssetRecord, Transaction,
    Validity, assert_authorized, database_error, insert_asset, insert_version, load_asset_in_tx,
    refresh_fts, to_i64, validate_record,
};
use harness_types::MemoryVersionRef;

/// The caller's current view of the sources it re-read, as a temporary table.
///
/// A temporary table rather than an `IN (…)` list because the pairs are
/// `(kind, id, digest)` triples and the freshness predicate is a join: a list
/// would have to be re-bound per row, and the query planner can use the index on
/// `memory_sources(source_kind, source_id)` against a table.
async fn load_refresh_table(
    tx: &mut Transaction<'_, Sqlite>,
    refresh: &[RefreshSource],
) -> Result<(), StoreError> {
    sqlx::query(
        "CREATE TEMP TABLE IF NOT EXISTS refresh_current(
             source_kind TEXT NOT NULL,
             source_id TEXT NOT NULL,
             observed_digest TEXT,
             PRIMARY KEY (source_kind, source_id)
         )",
    )
    .execute(&mut **tx)
    .await
    .map_err(|error| database_error(ErrorCode::StorageOpenFailed, "create refresh view", error))?;
    sqlx::query("DELETE FROM refresh_current")
        .execute(&mut **tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageOpenFailed, "clear refresh view", error)
        })?;
    for source in refresh {
        sqlx::query(
            "INSERT OR REPLACE INTO refresh_current(source_kind, source_id, observed_digest)
             VALUES (?, ?, ?)",
        )
        .bind(source.kind.as_str())
        .bind(&source.id)
        .bind(source.observed.as_ref().map(ContentHash::as_str))
        .execute(&mut **tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageOpenFailed, "fill refresh view", error)
        })?;
    }
    Ok(())
}

impl SqliteStore {
    pub async fn bound_memory(
        &self,
        principal: &StoreMemoryPrincipal,
    ) -> Result<(Vec<(StoredMemoryAssetRecord, String)>, u64), StoreError> {
        self.bound_memory_fresh(principal, &[]).await
    }

    /// Bound memory, leaving out versions whose named sources the caller saw move.
    ///
    /// The same freshness rule as [`Self::search_memory_fresh`], applied to the
    /// injection path: a bootstrap block built from a version whose source changed
    /// would put stale text in the frozen packet, where nothing downstream can
    /// tell it apart from a current fact.
    pub async fn bound_memory_fresh(
        &self,
        principal: &StoreMemoryPrincipal,
        refresh: &[RefreshSource],
    ) -> Result<(Vec<(StoredMemoryAssetRecord, String)>, u64), StoreError> {
        let mut tx = self.pool.begin().await.map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "begin bound memory snapshot",
                error,
            )
        })?;
        let revision = read_revision(&mut tx).await?;
        load_refresh_table(&mut tx, refresh).await?;
        let rows = sqlx::query("SELECT a.memory_asset_id, b.injection_mode FROM memory_bindings b JOIN memory_assets a ON a.memory_asset_id = b.memory_asset_id
            WHERE b.principal_id = ?1 AND b.injection_mode IN ('bootstrap', 'index') AND a.status = 'active'
              AND (a.project_id IS NULL OR a.project_id = ?2) AND (a.task_id IS NULL OR a.task_id = ?3)
              AND (a.agent_profile_id IS NULL OR a.agent_profile_id = ?4) AND (a.session_id IS NULL OR a.session_id = ?5)
              AND (a.owner_id = ?1 OR EXISTS (SELECT 1 FROM memory_grants g, json_each(g.actions_json) action WHERE g.memory_asset_id = a.memory_asset_id AND g.principal_id = ?1 AND g.active = 1 AND (g.project_id IS NULL OR g.project_id = ?2) AND action.value = 'search'))
              AND NOT EXISTS (SELECT 1 FROM memory_sources ms
                  JOIN refresh_current rc ON rc.source_kind = ms.source_kind AND rc.source_id = ms.source_id
                  WHERE ms.derived_asset_id = a.memory_asset_id AND ms.derived_version = a.current_version
                    AND (rc.observed_digest IS NULL OR ms.observed_digest IS NULL OR ms.observed_digest != rc.observed_digest))
            ORDER BY b.priority DESC, b.binding_id")
            .bind(&principal.principal_id).bind(principal.project_id.as_ref().map(ToString::to_string)).bind(principal.task_id.as_ref().map(ToString::to_string))
            .bind(principal.agent_profile_id.as_ref().map(ToString::to_string)).bind(principal.session_id.as_ref().map(ToString::to_string))
            .fetch_all(&mut *tx).await.map_err(|error| database_error(ErrorCode::StorageOpenFailed, "read scoped memory bindings", error))?;
        let mut records = Vec::new();
        for row in rows {
            let id = MemoryAssetId::parse(row.get::<String, _>("memory_asset_id"))
                .map_err(|error| StoreError::new(error.code(), error.to_string()))?;
            match assert_lineage_authorized(&mut tx, principal, &id, true).await {
                Ok(()) => {}
                Err(error)
                    if matches!(
                        error.code(),
                        ErrorCode::PolicyDenied | ErrorCode::SequenceConflict
                    ) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            }
            if let Some(asset) = load_asset_in_tx(&mut tx, &id).await?
                && asset.current.record.validity == Validity::Valid
            {
                records.push((asset, row.get("injection_mode")));
            }
            if records.len() == 8 {
                break;
            }
        }
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "close bound memory snapshot",
                error,
            )
        })?;
        Ok((records, revision))
    }
    /// Candidate assets of one principal that a human can still confirm, oldest first.
    ///
    /// Scope is filtered in SQL and authorization is re-checked per row with the
    /// `publish` action, so a listing can never hand back a candidate the principal
    /// would not be allowed to confirm.
    pub async fn list_memory_candidates(
        &self,
        principal: &StoreMemoryPrincipal,
        limit: usize,
    ) -> Result<Vec<StoredMemoryAssetRecord>, StoreError> {
        let mut tx = self.pool.begin().await.map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "begin candidate listing",
                error,
            )
        })?;
        let rows = sqlx::query(
            "SELECT a.memory_asset_id FROM memory_assets a
             WHERE a.status = 'candidate'
               AND (a.project_id IS NULL OR a.project_id = ?1)
               AND (a.task_id IS NULL OR a.task_id = ?2)
               AND (a.agent_profile_id IS NULL OR a.agent_profile_id = ?3)
               AND (a.session_id IS NULL OR a.session_id = ?4)
             ORDER BY a.created_at, a.memory_asset_id LIMIT ?5",
        )
        .bind(principal.project_id.as_ref().map(ToString::to_string))
        .bind(principal.task_id.as_ref().map(ToString::to_string))
        .bind(principal.agent_profile_id.as_ref().map(ToString::to_string))
        .bind(principal.session_id.as_ref().map(ToString::to_string))
        .bind(i64::try_from(limit.min(64)).unwrap_or(64))
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| database_error(ErrorCode::StorageOpenFailed, "list candidates", error))?;
        let mut records = Vec::new();
        for row in rows {
            let id = MemoryAssetId::parse(row.get::<String, _>("memory_asset_id"))
                .map_err(|error| StoreError::new(error.code(), error.to_string()))?;
            match assert_authorized(&mut tx, principal, &id, "publish").await {
                Ok(()) => {}
                Err(error)
                    if matches!(
                        error.code(),
                        ErrorCode::PolicyDenied | ErrorCode::SequenceConflict
                    ) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            }
            if let Some(asset) = load_asset_in_tx(&mut tx, &id).await? {
                records.push(asset);
            }
        }
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "close candidate listing",
                error,
            )
        })?;
        Ok(records)
    }

    pub async fn invalidate_memory(
        &self,
        principal: &StoreMemoryPrincipal,
        id: &MemoryAssetId,
        reason: &str,
    ) -> Result<Vec<MemoryAssetId>, StoreError> {
        if reason.trim().is_empty() {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                "invalidation reason required",
            ));
        }
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_authorized(&mut tx, principal, id, "invalidate").await?;
        let affected = invalidate_tree(&mut tx, id, true, reason).await?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit transitive invalidation",
                error,
            )
        })?;
        Ok(affected)
    }

    pub async fn validate_memory_snapshot(
        &self,
        principal: &StoreMemoryPrincipal,
        sources: &[MemoryVersionRef],
        revision: u64,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await.map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "begin memory snapshot validation",
                error,
            )
        })?;
        if read_revision(&mut tx).await? != revision {
            return Err(StoreError::new(
                ErrorCode::SequenceConflict,
                "memory policy or asset revision changed; rebuild context",
            ));
        }
        for source in sources {
            assert_authorized(&mut tx, principal, &source.memory_asset_id, "search").await?;
            let asset = load_asset_in_tx(&mut tx, &source.memory_asset_id)
                .await?
                .ok_or_else(|| {
                    StoreError::new(ErrorCode::InvalidPayload, "memory snapshot asset missing")
                })?;
            if asset.asset.current_version != source.version
                || asset.asset.status != MemoryAssetStatus::Active
                || asset.current.record.validity != Validity::Valid
            {
                return Err(StoreError::new(
                    ErrorCode::SequenceConflict,
                    "memory snapshot is stale",
                ));
            }
            assert_lineage_authorized(&mut tx, principal, &source.memory_asset_id, true).await?;
        }
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "close memory snapshot validation",
                error,
            )
        })
    }

    pub async fn pause_extraction_jobs(
        &self,
        stream: &SessionId,
        extractor: &str,
        strategy: &ContentHash,
    ) -> Result<u64, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let result = sqlx::query("UPDATE memory_jobs SET status = 'paused', lease_owner = NULL, last_error = 'budget_or_shutdown' WHERE source_stream = ? AND extractor_version = ? AND strategy_digest = ? AND status IN ('pending', 'retry_wait', 'paused')")
            .bind(stream.as_str()).bind(extractor).bind(strategy.as_str()).execute(&mut *tx).await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "pause memory backlog", error))?;
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit memory pause", error)
        })?;
        Ok(result.rows_affected())
    }

    pub async fn invalidate_memory_source(
        &self,
        principal: &StoreMemoryPrincipal,
        kind: &str,
        source_id: &str,
    ) -> Result<Vec<MemoryAssetId>, StoreError> {
        if !matches!(kind, "file" | "commit") {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                "unsupported source change",
            ));
        }
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let ids = sqlx::query_scalar::<_, String>(
            "SELECT DISTINCT ms.derived_asset_id FROM memory_sources ms
             JOIN memory_assets a ON a.memory_asset_id = ms.derived_asset_id
                                  AND a.current_version = ms.derived_version
             WHERE ms.source_kind = ?1 AND ms.source_id = ?2
               AND (?3 IS NULL OR ms.scope_project_id IS NULL OR ms.scope_project_id = ?3)
             ORDER BY ms.derived_asset_id",
        )
        .bind(kind)
        .bind(source_id)
        .bind(principal.project_id.as_ref().map(ToString::to_string))
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "find changed memory sources",
                error,
            )
        })?;
        let mut affected = Vec::new();
        for id in ids {
            let id = MemoryAssetId::parse(id)
                .map_err(|error| StoreError::new(error.code(), error.to_string()))?;
            match assert_authorized(&mut tx, principal, &id, "invalidate").await {
                Ok(()) => {
                    affected.extend(invalidate_tree(&mut tx, &id, true, "source_changed").await?);
                }
                Err(error) if error.code() == ErrorCode::PolicyDenied => {}
                Err(error) => return Err(error),
            }
        }
        affected.sort();
        affected.dedup();
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit source invalidation",
                error,
            )
        })?;
        Ok(affected)
    }

    /// Apply one operator retention status and its journal entry atomically.
    ///
    /// Retention addresses a global source identity; the tombstone key is also
    /// global, so every project copy derived from that exact file/commit source
    /// is included. Ordinary source invalidation remains principal-scoped.
    pub async fn update_memory_source_retention_and_journal(
        &self,
        request: crate::MemorySourceRetentionUpdate,
    ) -> Result<Vec<MemoryAssetId>, StoreError> {
        let crate::MemorySourceRetentionUpdate {
            source_kind: kind,
            source_id,
            status,
            reason,
            journal_entry_id,
            journal_detail,
            created_unix_ms,
        } = request;
        let (journal_action, invalidation_reason) = match status {
            MemoryAssetStatus::Invalidated => {
                let reason = reason
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        StoreError::new(ErrorCode::InvalidPayload, "invalidation reason required")
                    })?;
                ("invalidate", Some(reason))
            }
            MemoryAssetStatus::Archived => ("archive", None),
            _ => {
                return Err(StoreError::new(
                    ErrorCode::InvalidPayload,
                    "retention may only invalidate or archive a source",
                ));
            }
        };
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        super::super::assert_fence_in_tx(&mut tx, &fence).await?;
        let affected = source_derived_assets(&mut tx, &kind, &source_id, false).await?;
        set_source_assets_status(&mut tx, &affected, status, invalidation_reason).await?;

        let mut detail = journal_detail;
        if let Some(fields) = detail.as_object_mut() {
            fields.insert(
                "affected_assets".to_owned(),
                serde_json::json!(affected.len()),
            );
        }
        let detail = serde_json::to_string(&detail).map_err(|_| {
            StoreError::new(
                ErrorCode::InvalidPayload,
                "retention journal detail is not serializable",
            )
        })?;
        sqlx::query(
            "INSERT INTO maintenance_journal(
                 entry_id, action, target, detail_json, created_unix_ms)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(journal_entry_id)
        .bind(journal_action)
        .bind(format!("{kind}:{source_id}"))
        .bind(detail)
        .bind(to_i64(created_unix_ms, "retention journal time")?)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "record retention journal entry",
                error,
            )
        })?;

        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit memory retention",
                error,
            )
        })?;
        Ok(affected)
    }

    /// Physically forget memory derived from a source and write its tombstone in
    /// the same fenced transaction. A failed tombstone or journal write rolls
    /// back the deletion, so the source cannot be re-extracted after a partial
    /// forget.
    pub async fn forget_memory_source_and_tombstone(
        &self,
        request: crate::MemorySourceForget,
    ) -> Result<Vec<MemoryAssetId>, StoreError> {
        let crate::MemorySourceForget {
            source_kind: kind,
            source_id,
            tombstone,
            journal_entry_id,
            journal_action,
            journal_detail,
        } = request;
        validate_forget_request(&kind, &source_id, &tombstone)?;

        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        super::super::assert_fence_in_tx(&mut tx, &fence).await?;
        ensure_source_not_forgotten(&mut tx, &kind, &source_id).await?;
        // Forget must purge the asset's historical versions too: an older
        // version can still contain content derived from this source even when
        // the current version has moved on to different sources.
        let affected = source_derived_assets(&mut tx, &kind, &source_id, true).await?;
        delete_forgotten_source_assets(&mut tx, &affected).await?;
        insert_forget_records(
            &mut tx,
            &kind,
            &source_id,
            &tombstone,
            &journal_entry_id,
            &journal_action,
            &journal_detail,
        )
        .await?;
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit source forget", error)
        })?;
        Ok(affected)
    }

    /// The active asset that already holds this exact text, if any.
    ///
    /// A write path uses this to avoid minting a second asset for text it has already
    /// remembered. It looks the text up in `memory_versions` rather than through the
    /// index: the index is for finding things by meaning, and this is an identity
    /// question - is this the same bytes - where a ranked match would be the wrong
    /// tool and a near-miss would be the wrong answer.
    ///
    /// Scope and authorization are the same predicates a search uses, so a caller
    /// cannot discover an asset through this that it could not have searched for.
    pub async fn find_active_memory_by_content(
        &self,
        principal: &StoreMemoryPrincipal,
        normalized_content: &str,
    ) -> Result<Option<MemoryAssetId>, StoreError> {
        let mut tx = self.pool.begin().await.map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "begin memory content lookup",
                error,
            )
        })?;
        let row = sqlx::query(
            "SELECT a.memory_asset_id FROM memory_versions v
             JOIN memory_assets a ON a.memory_asset_id = v.memory_asset_id
             WHERE v.normalized_content = ?1
               AND v.version = a.current_version
               AND a.status = 'active'
               AND json_extract(v.version_json, '$.validity') = 'valid'
               -- The same scope predicates a search applies, including the ones that
               -- tolerate a NULL column: search accepts a user-scoped asset from a
               -- project principal, and a lookup that did not would mint a duplicate of
               -- something the principal can already read.
               AND (a.project_id IS NULL OR a.project_id = ?2)
               AND (a.task_id IS NULL OR a.task_id = ?3)
               AND (a.agent_profile_id IS NULL OR a.agent_profile_id = ?4)
               AND (a.session_id IS NULL OR a.session_id = ?5)
               -- And the same reachability rule: owner or an active grant that allows
               -- searching. Requiring only a binding let a bind-only or revoked grant
               -- make this report someone else's asset and store nothing.
               AND (a.owner_id = ?6 OR EXISTS (SELECT 1 FROM memory_grants g,
                        json_each(g.actions_json) action
                        WHERE g.memory_asset_id = a.memory_asset_id AND g.principal_id = ?6
                          AND g.active = 1 AND (g.project_id IS NULL OR g.project_id = ?2)
                          AND action.value = 'search'))
               AND EXISTS (SELECT 1 FROM memory_bindings b
                   WHERE b.memory_asset_id = a.memory_asset_id AND b.principal_id = ?6)
             ORDER BY a.memory_asset_id LIMIT 1",
        )
        .bind(normalized_content)
        .bind(principal.project_id.as_ref().map(ToString::to_string))
        .bind(principal.task_id.as_ref().map(ToString::to_string))
        .bind(principal.agent_profile_id.as_ref().map(ToString::to_string))
        .bind(principal.session_id.as_ref().map(ToString::to_string))
        .bind(&principal.principal_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "look up memory by content",
                error,
            )
        })?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "close memory content lookup",
                error,
            )
        })?;
        row.map(|row| {
            MemoryAssetId::parse(row.get::<String, _>("memory_asset_id"))
                .map_err(|error| StoreError::new(error.code(), error.to_string()))
        })
        .transpose()
    }

    /// Search, treating the named sources as moved.
    ///
    /// `refresh` is what the caller currently sees for the sources it re-read. A
    /// version whose recorded digest differs from the caller's observation is not
    /// returned, and the filter is applied in SQL before `bm25`/`LIMIT`, so a
    /// stale version cannot push a live one off the end of the page. The caller
    /// does the reading because memory does not own the workspace; an empty
    /// `refresh` filters nothing, which is the honest answer for a caller that has
    /// re-read nothing (ADR-N07, D2).
    pub async fn search_memory_fresh(
        &self,
        principal: &StoreMemoryPrincipal,
        query: &str,
        limit: usize,
        exclude_provenance: Option<&str>,
        refresh: &[RefreshSource],
    ) -> Result<(Vec<StoredMemoryAssetRecord>, u64), StoreError> {
        let mut tx = self.pool.begin().await.map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "begin memory search snapshot",
                error,
            )
        })?;
        let revision = read_revision(&mut tx).await?;
        if query.is_empty() {
            return Ok((Vec::new(), revision));
        }
        load_refresh_table(&mut tx, refresh).await?;
        // All scope, grant, binding and validity predicates precede ranking/LIMIT in SQL.
        let rows = sqlx::query(
            "WITH RECURSIVE allowed(id, readable, searchable) AS (
               SELECT a.memory_asset_id,
                 (a.owner_id = ?6 OR EXISTS (SELECT 1 FROM memory_grants g, json_each(g.actions_json) action WHERE g.memory_asset_id = a.memory_asset_id AND g.principal_id = ?6 AND g.active = 1 AND (g.project_id IS NULL OR g.project_id = ?2) AND action.value = 'read')),
                 (a.owner_id = ?6 OR EXISTS (SELECT 1 FROM memory_grants g, json_each(g.actions_json) action WHERE g.memory_asset_id = a.memory_asset_id AND g.principal_id = ?6 AND g.active = 1 AND (g.project_id IS NULL OR g.project_id = ?2) AND action.value = 'search'))
               FROM memory_assets a WHERE (a.project_id IS NULL OR a.project_id = ?2) AND (a.task_id IS NULL OR a.task_id = ?3)
                 AND (a.agent_profile_id IS NULL OR a.agent_profile_id = ?4) AND (a.session_id IS NULL OR a.session_id = ?5)
             ), lineage(root, id, version) AS (
               SELECT d.derived_asset_id, d.source_id, d.source_version FROM memory_dependencies d JOIN memory_assets a ON a.memory_asset_id = d.derived_asset_id AND a.current_version = d.derived_version WHERE d.source_kind = 'asset'
               UNION SELECT l.root, d.source_id, d.source_version FROM lineage l JOIN memory_dependencies d ON d.derived_asset_id = l.id JOIN memory_assets a ON a.memory_asset_id = d.derived_asset_id AND a.current_version = d.derived_version WHERE d.source_kind = 'asset'
             )
             SELECT a.memory_asset_id FROM memory_fts
             JOIN memory_assets a ON a.memory_asset_id = memory_fts.memory_asset_id
             JOIN memory_versions v ON v.memory_asset_id = a.memory_asset_id AND v.version = a.current_version
             JOIN allowed permission ON permission.id = a.memory_asset_id AND permission.searchable = 1
             WHERE memory_fts MATCH ?1 AND a.status = 'active' AND json_extract(v.version_json, '$.validity') = 'valid'
               AND (?8 IS NULL OR json_extract(v.version_json, '$.provenance_kind') IS NOT ?8)
               AND EXISTS (SELECT 1 FROM memory_bindings b WHERE b.memory_asset_id = a.memory_asset_id AND b.principal_id = ?6)
               AND NOT EXISTS (SELECT 1 FROM lineage l LEFT JOIN allowed p ON p.id = l.id
                   LEFT JOIN memory_assets s ON s.memory_asset_id = l.id
                   JOIN memory_versions sv ON sv.memory_asset_id = s.memory_asset_id AND sv.version = s.current_version
                   WHERE l.root = a.memory_asset_id AND (coalesce(p.readable, 0) = 0 OR s.status IN ('invalidated', 'archived') OR s.current_version != l.version OR json_extract(sv.version_json, '$.validity') != 'valid'))
               -- A source that moved since this version was written makes the
               -- version stale. `refresh_current` holds only the pairs the caller
               -- re-read; a source that is not in it cannot be judged, and is left
               -- alone rather than assumed fresh.
               AND NOT EXISTS (SELECT 1 FROM memory_sources ms
                   JOIN refresh_current rc ON rc.source_kind = ms.source_kind AND rc.source_id = ms.source_id
                   WHERE ms.derived_asset_id = a.memory_asset_id AND ms.derived_version = a.current_version
                     AND (rc.observed_digest IS NULL OR ms.observed_digest IS NULL OR ms.observed_digest != rc.observed_digest))
             ORDER BY bm25(memory_fts), a.memory_asset_id LIMIT ?7")
            .bind(query).bind(principal.project_id.as_ref().map(ToString::to_string))
            .bind(principal.task_id.as_ref().map(ToString::to_string)).bind(principal.agent_profile_id.as_ref().map(ToString::to_string))
            .bind(principal.session_id.as_ref().map(ToString::to_string)).bind(&principal.principal_id)
            .bind(i64::try_from(limit.min(32)).unwrap_or(32))
            .bind(exclude_provenance)
            .fetch_all(&mut *tx).await
            .map_err(|error| database_error(ErrorCode::StorageOpenFailed, "scoped FTS search", error))?;
        let mut hits = Vec::new();
        for row in rows {
            let id = MemoryAssetId::parse(row.get::<String, _>("memory_asset_id"))
                .map_err(|error| StoreError::new(error.code(), error.to_string()))?;
            if let Some(asset) = load_asset_in_tx(&mut tx, &id).await? {
                hits.push(asset);
            }
        }
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "close memory search snapshot",
                error,
            )
        })?;
        Ok((hits, revision))
    }

    #[allow(clippy::too_many_arguments)] // All fields belong to one atomic binding update.
    /// The asset that already holds this exact text, whatever its status.
    ///
    /// The extraction loop's guard against feeding on its own output. It cannot use
    /// the active-only lookup: an extraction settles a *candidate*, so the first
    /// time a sentence is extracted it is a candidate, and the second time the
    /// quote has to be recognised against something that is not `active` yet.
    /// Looking only at active memory would let the loop publish the same sentence
    /// twice and call the second one evidence.
    ///
    /// Reachability and scope are the same predicates a search uses, so this never
    /// reports an asset the principal could not have found. Row lifetime differs on
    /// purpose: an `invalidated` row still counts, because a candidate a human
    /// refused must not come back as a fresh proposal.
    pub async fn find_memory_by_content_any_status(
        &self,
        principal: &StoreMemoryPrincipal,
        normalized_content: &str,
    ) -> Result<Option<MemoryAssetId>, StoreError> {
        let mut tx = self.pool.begin().await.map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "begin memory content lookup",
                error,
            )
        })?;
        let row = sqlx::query(
            "SELECT a.memory_asset_id FROM memory_versions v
             JOIN memory_assets a ON a.memory_asset_id = v.memory_asset_id
             WHERE v.normalized_content = ?1
               AND v.version = a.current_version
               AND a.status != 'archived'
               AND (a.project_id IS NULL OR a.project_id = ?2)
               AND (a.task_id IS NULL OR a.task_id = ?3)
               AND (a.agent_profile_id IS NULL OR a.agent_profile_id = ?4)
               AND (a.session_id IS NULL OR a.session_id = ?5)
               AND a.owner_id = ?6
             ORDER BY a.memory_asset_id LIMIT 1",
        )
        .bind(normalized_content)
        .bind(principal.project_id.as_ref().map(ToString::to_string))
        .bind(principal.task_id.as_ref().map(ToString::to_string))
        .bind(principal.agent_profile_id.as_ref().map(ToString::to_string))
        .bind(principal.session_id.as_ref().map(ToString::to_string))
        .bind(&principal.principal_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "look up memory by content",
                error,
            )
        })?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "close memory content lookup",
                error,
            )
        })?;
        row.map(|row| {
            MemoryAssetId::parse(row.get::<String, _>("memory_asset_id"))
                .map_err(|error| StoreError::new(error.code(), error.to_string()))
        })
        .transpose()
    }

    #[allow(clippy::too_many_arguments)] // All fields belong to one atomic binding update.
    pub async fn bind_memory(
        &self,
        principal: &StoreMemoryPrincipal,
        binding_id: &str,
        id: &MemoryAssetId,
        target: &str,
        mode: &str,
        priority: i32,
        revision: u64,
    ) -> Result<(), StoreError> {
        if binding_id.trim().is_empty()
            || target.trim().is_empty()
            || !matches!(mode, "bootstrap" | "index" | "on_demand")
        {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                "invalid memory binding",
            ));
        }
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_authorized(&mut tx, principal, id, "bind").await?;
        let previous = sqlx::query_as::<_, (i64, String, String)>(
            "SELECT revision, memory_asset_id, principal_id FROM memory_bindings WHERE binding_id = ?",
        )
        .bind(binding_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "read binding revision",
                error,
            )
        })?;
        if previous
            .as_ref()
            .is_some_and(|(_, asset, actor)| asset != id.as_str() || actor != target)
        {
            return Err(StoreError::new(
                ErrorCode::PolicyDenied,
                "binding identity cannot be reassigned",
            ));
        }
        if to_i64(revision, "binding revision")? != previous.map_or(0, |(value, _, _)| value) + 1 {
            return Err(StoreError::new(
                ErrorCode::SequenceConflict,
                "binding revision must advance once",
            ));
        }
        sqlx::query("INSERT INTO memory_bindings VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(binding_id) DO UPDATE SET injection_mode = excluded.injection_mode, priority = excluded.priority, revision = excluded.revision")
            .bind(binding_id).bind(id.as_str()).bind(target).bind(mode).bind(priority).bind(to_i64(revision, "binding revision")?)
            .execute(&mut *tx).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "write memory binding", error))?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit memory binding",
                error,
            )
        })
    }

    pub async fn create_derived_memory(
        &self,
        principal: &StoreMemoryPrincipal,
        record: StoredMemoryAssetRecord,
        sources: &[MemoryVersionRef],
    ) -> Result<StoredMemoryAssetRecord, StoreError> {
        validate_record(&record)?;
        if sources.is_empty() || record.asset.owner_id != principal.principal_id {
            return Err(StoreError::new(
                ErrorCode::PolicyDenied,
                "derived memory requires authorized sources",
            ));
        }
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        for source in sources {
            assert_authorized(&mut tx, principal, &source.memory_asset_id, "read").await?;
            let asset = load_asset_in_tx(&mut tx, &source.memory_asset_id)
                .await?
                .ok_or_else(|| {
                    StoreError::new(ErrorCode::InvalidPayload, "derived source missing")
                })?;
            if asset.asset.current_version != source.version
                || matches!(
                    asset.asset.status,
                    MemoryAssetStatus::Invalidated | MemoryAssetStatus::Archived
                )
                || asset.current.record.validity != Validity::Valid
            {
                return Err(StoreError::new(
                    ErrorCode::SequenceConflict,
                    "derived source changed or was invalidated",
                ));
            }
        }
        insert_asset(&mut tx, &record).await?;
        insert_version(&mut tx, &record.current).await?;
        for source in sources {
            insert_dependency(
                &mut tx,
                &record.asset.memory_asset_id,
                1,
                "asset",
                source.memory_asset_id.as_str(),
                Some(source.version),
            )
            .await?;
        }
        refresh_fts(&mut tx, &record).await?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit derived memory",
                error,
            )
        })?;
        Ok(record)
    }
}

fn validate_forget_request(
    kind: &str,
    source_id: &str,
    tombstone: &crate::TombstoneRow,
) -> Result<(), StoreError> {
    if tombstone.source_kind != kind || tombstone.source_id != source_id {
        return Err(StoreError::new(
            ErrorCode::InvalidPayload,
            "forget tombstone identity does not match its source",
        ));
    }
    if kind.trim().is_empty() || source_id.trim().is_empty() || tombstone.reason.trim().is_empty() {
        return Err(StoreError::new(
            ErrorCode::InvalidPayload,
            "forget requires a source kind, identity and reason",
        ));
    }
    Ok(())
}

async fn ensure_source_not_forgotten(
    tx: &mut Transaction<'_, Sqlite>,
    kind: &str,
    source_id: &str,
) -> Result<(), StoreError> {
    let already_forgotten = sqlx::query_scalar::<_, i64>(
        "SELECT EXISTS(
             SELECT 1 FROM maintenance_tombstones
             WHERE source_kind = ? AND source_id = ?
         )",
    )
    .bind(kind)
    .bind(source_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| {
        database_error(
            ErrorCode::StorageWriteFailed,
            "check existing forget tombstone",
            error,
        )
    })?;
    if already_forgotten != 0 {
        return Err(StoreError::new(
            ErrorCode::RetentionRefused,
            format!("source {kind}:{source_id} was already forgotten"),
        ));
    }
    Ok(())
}

async fn delete_forgotten_source_assets(
    tx: &mut Transaction<'_, Sqlite>,
    affected: &[MemoryAssetId],
) -> Result<(), StoreError> {
    let deletions = [
        (
            "DELETE FROM memory_fts WHERE memory_asset_id = ?",
            "remove forgotten memory from search index",
            false,
        ),
        (
            "DELETE FROM memory_invalidations WHERE memory_asset_id = ?",
            "remove forgotten memory invalidation history",
            false,
        ),
        (
            "DELETE FROM memory_bindings WHERE memory_asset_id = ?",
            "remove forgotten memory bindings",
            false,
        ),
        (
            "DELETE FROM memory_grants WHERE memory_asset_id = ?",
            "remove forgotten memory grants",
            false,
        ),
        (
            "DELETE FROM memory_versions WHERE memory_asset_id = ?",
            "remove forgotten memory versions",
            false,
        ),
        (
            "DELETE FROM memory_sources WHERE derived_asset_id = ?",
            "remove forgotten memory sources",
            false,
        ),
        (
            "DELETE FROM memory_dependencies
             WHERE derived_asset_id = ? OR (source_kind = 'asset' AND source_id = ?)",
            "remove forgotten memory dependencies",
            true,
        ),
        (
            "DELETE FROM memory_assets WHERE memory_asset_id = ?",
            "remove forgotten memory asset",
            false,
        ),
    ];
    for id in affected {
        for (statement, operation, has_source_id) in deletions {
            let mut query = sqlx::query(statement).bind(id.as_str());
            if has_source_id {
                query = query.bind(id.as_str());
            }
            query
                .execute(&mut **tx)
                .await
                .map_err(|error| database_error(ErrorCode::StorageWriteFailed, operation, error))?;
        }
    }
    Ok(())
}

async fn insert_forget_records(
    tx: &mut Transaction<'_, Sqlite>,
    kind: &str,
    source_id: &str,
    tombstone: &crate::TombstoneRow,
    journal_entry_id: &str,
    journal_action: &str,
    journal_detail: &serde_json::Value,
) -> Result<(), StoreError> {
    let copies = serde_json::to_string(&tombstone.surviving_copies).map_err(|_| {
        StoreError::new(
            ErrorCode::InvalidPayload,
            "surviving copies are not serializable",
        )
    })?;
    sqlx::query(
        "INSERT INTO maintenance_tombstones(
             tombstone_id, source_kind, source_id, reason, surviving_copies_json, created_unix_ms)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&tombstone.tombstone_id)
    .bind(&tombstone.source_kind)
    .bind(&tombstone.source_id)
    .bind(&tombstone.reason)
    .bind(copies)
    .bind(to_i64(tombstone.created_unix_ms, "tombstone time")?)
    .execute(&mut **tx)
    .await
    .map_err(|error| {
        database_error(
            ErrorCode::StorageWriteFailed,
            "record forget tombstone",
            error,
        )
    })?;
    let detail = serde_json::to_string(journal_detail).map_err(|_| {
        StoreError::new(
            ErrorCode::InvalidPayload,
            "journal detail is not serializable",
        )
    })?;
    sqlx::query(
        "INSERT INTO maintenance_journal(
             entry_id, action, target, detail_json, created_unix_ms)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(journal_entry_id)
    .bind(journal_action)
    .bind(format!("{kind}:{source_id}"))
    .bind(detail)
    .bind(to_i64(tombstone.created_unix_ms, "journal time")?)
    .execute(&mut **tx)
    .await
    .map_err(|error| {
        database_error(
            ErrorCode::StorageWriteFailed,
            "record forget journal entry",
            error,
        )
    })?;
    Ok(())
}

async fn set_source_assets_status(
    tx: &mut Transaction<'_, Sqlite>,
    affected: &[MemoryAssetId],
    status: MemoryAssetStatus,
    invalidation_reason: Option<&str>,
) -> Result<(), StoreError> {
    let status_value = match status {
        MemoryAssetStatus::Active => "active",
        MemoryAssetStatus::Candidate => "candidate",
        MemoryAssetStatus::Superseded => "superseded",
        MemoryAssetStatus::Invalidated => "invalidated",
        MemoryAssetStatus::Archived => "archived",
    };
    for id in affected {
        let Some(mut record) = load_asset_in_tx(tx, id).await? else {
            continue;
        };
        record.asset.status = status;
        let asset_json = serde_json::to_string(&record.asset)
            .map_err(|_| StoreError::new(ErrorCode::InvalidPayload, "invalid memory asset"))?;
        sqlx::query(
            "UPDATE memory_assets SET status = ?, asset_json = ?, revision = revision + 1
             WHERE memory_asset_id = ?",
        )
        .bind(status_value)
        .bind(asset_json)
        .bind(id.as_str())
        .execute(&mut **tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "update derived memory status",
                error,
            )
        })?;
        if let Some(reason) = invalidation_reason {
            sqlx::query("INSERT INTO memory_invalidations(memory_asset_id, reason) VALUES (?, ?)")
                .bind(id.as_str())
                .bind(reason)
                .execute(&mut **tx)
                .await
                .map_err(|error| {
                    database_error(
                        ErrorCode::StorageWriteFailed,
                        "record invalidation audit",
                        error,
                    )
                })?;
        }
        refresh_fts(tx, &record).await?;
    }
    Ok(())
}

async fn source_derived_assets(
    tx: &mut Transaction<'_, Sqlite>,
    kind: &str,
    source_id: &str,
    include_history: bool,
) -> Result<Vec<MemoryAssetId>, StoreError> {
    if !matches!(kind, "file" | "commit") {
        return Err(StoreError::new(
            ErrorCode::InvalidPayload,
            "unsupported source change",
        ));
    }
    let roots_query = if include_history {
        "SELECT DISTINCT ms.derived_asset_id FROM memory_sources ms
         WHERE ms.source_kind = ?1 AND ms.source_id = ?2 ORDER BY ms.derived_asset_id"
    } else {
        "SELECT DISTINCT ms.derived_asset_id FROM memory_sources ms
         JOIN memory_assets a ON a.memory_asset_id = ms.derived_asset_id
                              AND a.current_version = ms.derived_version
         WHERE ms.source_kind = ?1 AND ms.source_id = ?2 ORDER BY ms.derived_asset_id"
    };
    let roots = sqlx::query_scalar::<_, String>(roots_query)
        .bind(kind)
        .bind(source_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "find changed memory sources",
                error,
            )
        })?;

    let mut affected = BTreeSet::new();
    for root in roots {
        let root = MemoryAssetId::parse(root)
            .map_err(|error| StoreError::new(error.code(), error.to_string()))?;
        let descendants_query = if include_history {
            "WITH RECURSIVE affected(id) AS (
                 SELECT ?
                 UNION
                 SELECT d.derived_asset_id FROM memory_dependencies d
                 JOIN affected a ON d.source_kind = 'asset' AND d.source_id = a.id
             )
             SELECT id FROM affected ORDER BY id"
        } else {
            "WITH RECURSIVE affected(id) AS (
                 SELECT ?
                 UNION
                 SELECT d.derived_asset_id FROM memory_dependencies d
                 JOIN memory_assets current ON current.memory_asset_id = d.derived_asset_id
                                           AND current.current_version = d.derived_version
                 JOIN affected a ON d.source_kind = 'asset' AND d.source_id = a.id
             )
             SELECT id FROM affected ORDER BY id"
        };
        let descendants = sqlx::query_scalar::<_, String>(descendants_query)
            .bind(root.as_str())
            .fetch_all(&mut **tx)
            .await
            .map_err(|error| {
                database_error(
                    ErrorCode::StorageWriteFailed,
                    "find transitive memory dependents",
                    error,
                )
            })?;
        for id in descendants {
            affected.insert(
                MemoryAssetId::parse(id)
                    .map_err(|error| StoreError::new(error.code(), error.to_string()))?,
            );
        }
    }
    Ok(affected.into_iter().collect())
}

pub(super) async fn invalidate_tree(
    tx: &mut Transaction<'_, Sqlite>,
    root: &MemoryAssetId,
    include_root: bool,
    reason: &str,
) -> Result<Vec<MemoryAssetId>, StoreError> {
    let ids = sqlx::query_scalar::<_, String>("WITH RECURSIVE affected(id) AS (SELECT ? UNION SELECT d.derived_asset_id FROM memory_dependencies d JOIN memory_assets current ON current.memory_asset_id = d.derived_asset_id AND current.current_version = d.derived_version JOIN affected a ON d.source_id = a.id WHERE d.source_kind = 'asset') SELECT id FROM affected ORDER BY id")
        .bind(root.as_str()).fetch_all(&mut **tx).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "find transitive memory dependents", error))?;
    let mut affected = Vec::new();
    for id in ids {
        let id = MemoryAssetId::parse(id)
            .map_err(|error| StoreError::new(error.code(), error.to_string()))?;
        if !include_root && &id == root {
            continue;
        }
        let Some(mut record) = load_asset_in_tx(tx, &id).await? else {
            continue;
        };
        record.asset.status = MemoryAssetStatus::Invalidated;
        let json = serde_json::to_string(&record.asset)
            .map_err(|_| StoreError::new(ErrorCode::InvalidPayload, "invalid memory asset"))?;
        sqlx::query("UPDATE memory_assets SET status = 'invalidated', asset_json = ?, revision = revision + 1 WHERE memory_asset_id = ?")
            .bind(json).bind(id.as_str()).execute(&mut **tx).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "invalidate dependent memory", error))?;
        sqlx::query("INSERT INTO memory_invalidations(memory_asset_id, reason) VALUES (?, ?)")
            .bind(id.as_str())
            .bind(reason)
            .execute(&mut **tx)
            .await
            .map_err(|error| {
                database_error(
                    ErrorCode::StorageWriteFailed,
                    "record invalidation audit",
                    error,
                )
            })?;
        refresh_fts(tx, &record).await?;
        affected.push(id);
    }
    Ok(affected)
}

pub(super) async fn assert_lineage_authorized(
    tx: &mut Transaction<'_, Sqlite>,
    principal: &StoreMemoryPrincipal,
    root: &MemoryAssetId,
    require_valid: bool,
) -> Result<(), StoreError> {
    let ids = sqlx::query_scalar::<_, String>("WITH RECURSIVE ancestors(id) AS (SELECT source_id FROM memory_dependencies WHERE derived_asset_id = ? AND source_kind = 'asset' UNION SELECT d.source_id FROM memory_dependencies d JOIN ancestors a ON d.derived_asset_id = a.id WHERE d.source_kind = 'asset') SELECT id FROM ancestors")
        .bind(root.as_str()).fetch_all(&mut **tx).await.map_err(|error| database_error(ErrorCode::StorageOpenFailed, "read transitive source permissions", error))?;
    for id in ids {
        let id = MemoryAssetId::parse(id)
            .map_err(|error| StoreError::new(error.code(), error.to_string()))?;
        assert_authorized(tx, principal, &id, "read").await?;
        let source = load_asset_in_tx(tx, &id)
            .await?
            .ok_or_else(|| StoreError::new(ErrorCode::InvalidPayload, "memory source missing"))?;
        if require_valid
            && (matches!(
                source.asset.status,
                MemoryAssetStatus::Invalidated | MemoryAssetStatus::Archived
            ) || source.current.record.validity != Validity::Valid)
        {
            return Err(StoreError::new(
                ErrorCode::SequenceConflict,
                "memory source no longer valid",
            ));
        }
    }
    Ok(())
}

async fn read_revision(tx: &mut Transaction<'_, Sqlite>) -> Result<u64, StoreError> {
    let value = sqlx::query_scalar::<_, i64>("SELECT revision FROM memory_revision WHERE id = 1")
        .fetch_one(&mut **tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageOpenFailed, "read memory revision", error)
        })?;
    u64::try_from(value)
        .map_err(|_| StoreError::new(ErrorCode::InvalidSequence, "invalid memory revision"))
}

pub(super) async fn assert_no_dependency_cycle(
    tx: &mut Transaction<'_, Sqlite>,
    target: &MemoryAssetId,
    source: &MemoryAssetId,
) -> Result<(), StoreError> {
    let count = sqlx::query_scalar::<_, i64>("WITH RECURSIVE ancestors(id) AS (SELECT ? UNION SELECT source_id FROM memory_dependencies d JOIN ancestors a ON d.derived_asset_id = a.id WHERE source_kind = 'asset') SELECT COUNT(*) FROM ancestors WHERE id = ?")
        .bind(source.as_str()).bind(target.as_str()).fetch_one(&mut **tx).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "check semantic merge cycle", error))?;
    if count != 0 {
        return Err(StoreError::new(
            ErrorCode::SequenceConflict,
            "semantic merge would create a dependency cycle",
        ));
    }
    Ok(())
}

pub(super) async fn insert_dependency(
    tx: &mut Transaction<'_, Sqlite>,
    asset: &MemoryAssetId,
    version: u64,
    kind: &str,
    source_id: &str,
    source_version: Option<u64>,
) -> Result<(), StoreError> {
    sqlx::query("INSERT OR IGNORE INTO memory_dependencies(derived_asset_id, derived_version, source_kind, source_id, source_version) VALUES (?, ?, ?, ?, ?)")
        .bind(asset.as_str()).bind(to_i64(version, "derived version")?).bind(kind).bind(source_id)
        .bind(source_version.map(|value| to_i64(value, "source version")).transpose()?)
        .execute(&mut **tx).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "persist memory lineage", error))?;
    Ok(())
}
