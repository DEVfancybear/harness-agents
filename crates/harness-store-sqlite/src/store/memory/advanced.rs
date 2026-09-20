use super::{
    ContentHash, ErrorCode, MemoryAssetId, MemoryAssetStatus, Row, SessionId, Sqlite, SqliteStore,
    StoreError, StoreMemoryPrincipal, StoredMemoryAssetRecord, Transaction, Validity,
    assert_authorized, database_error, insert_asset, insert_version, load_asset_in_tx, refresh_fts,
    to_i64, validate_record,
};
use harness_types::MemoryVersionRef;

impl SqliteStore {
    pub async fn bound_memory(
        &self,
        principal: &StoreMemoryPrincipal,
    ) -> Result<(Vec<(StoredMemoryAssetRecord, String)>, u64), StoreError> {
        let mut tx = self.pool.begin().await.map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "begin bound memory snapshot",
                error,
            )
        })?;
        let revision = read_revision(&mut tx).await?;
        let rows = sqlx::query("SELECT a.memory_asset_id, b.injection_mode FROM memory_bindings b JOIN memory_assets a ON a.memory_asset_id = b.memory_asset_id
            WHERE b.principal_id = ?1 AND b.injection_mode IN ('bootstrap', 'index') AND a.status = 'active'
              AND (a.project_id IS NULL OR a.project_id = ?2) AND (a.task_id IS NULL OR a.task_id = ?3)
              AND (a.agent_profile_id IS NULL OR a.agent_profile_id = ?4) AND (a.session_id IS NULL OR a.session_id = ?5)
              AND (a.owner_id = ?1 OR EXISTS (SELECT 1 FROM memory_grants g, json_each(g.actions_json) action WHERE g.memory_asset_id = a.memory_asset_id AND g.principal_id = ?1 AND g.active = 1 AND (g.project_id IS NULL OR g.project_id = ?2) AND action.value = 'search'))
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
        let ids = sqlx::query_scalar::<_, String>("SELECT DISTINCT derived_asset_id FROM memory_dependencies WHERE source_kind = ? AND source_id = ?")
            .bind(kind).bind(source_id).fetch_all(&mut *tx).await.map_err(|error| database_error(ErrorCode::StorageWriteFailed, "find changed memory sources", error))?;
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
        project_only: bool,
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
               AND ((?2 = 1 AND a.project_id = ?3) OR (?2 = 0 AND a.project_id IS NULL))
               AND EXISTS (SELECT 1 FROM memory_bindings b
                   WHERE b.memory_asset_id = a.memory_asset_id AND b.principal_id = ?4)
             ORDER BY a.memory_asset_id LIMIT 1",
        )
        .bind(normalized_content)
        .bind(i64::from(project_only))
        .bind(principal.project_id.as_ref().map(ToString::to_string))
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

    /// Record one more source event on a version that already exists.
    ///
    /// This is what keeps the audit trail when the same text is said twice: the
    /// asset, its version number and its content hash are all untouched, and only the
    /// source list grows. `content_hash` covers the content, so a dependent summary
    /// that pinned this version is still pinning the same bytes and must not be
    /// rebuilt.
    ///
    /// Idempotent: recording an event the version already names is not an error and
    /// writes nothing.
    pub async fn append_memory_version_source(
        &self,
        principal: &StoreMemoryPrincipal,
        asset_id: &MemoryAssetId,
        event_id: &harness_types::EventId,
    ) -> Result<bool, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let Some(mut record) = load_asset_in_tx(&mut tx, asset_id).await? else {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                "memory asset to append to was not found",
            ));
        };
        assert_authorized(&mut tx, principal, asset_id, "bind").await?;
        if record.current.record.source_event_refs.contains(event_id) {
            tx.commit().await.map_err(|error| {
                database_error(
                    ErrorCode::StorageWriteFailed,
                    "commit unchanged memory sources",
                    error,
                )
            })?;
            return Ok(false);
        }
        record
            .current
            .record
            .source_event_refs
            .push(event_id.clone());
        record.current.record.source_event_refs.sort();
        record.current.record.source_event_refs.dedup();
        record
            .current
            .record
            .validate()
            .map_err(|error| StoreError::new(error.code(), error.to_string()))?;
        // The version row is rewritten in place: same version number, same content,
        // same content hash - only `version_json` names one more source.
        let json = serde_json::to_string(&record.current.record).map_err(|_| {
            StoreError::new(
                ErrorCode::InvalidPayload,
                "memory version cannot be serialized",
            )
        })?;
        sqlx::query(
            "UPDATE memory_versions SET version_json = ? WHERE memory_asset_id = ? AND version = ?",
        )
        .bind(json)
        .bind(asset_id.as_str())
        .bind(to_i64(record.current.record.version, "memory version")?)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "append memory version source",
                error,
            )
        })?;
        super::advanced::insert_dependency(
            &mut tx,
            asset_id,
            record.current.record.version,
            "event",
            event_id.as_str(),
            None,
        )
        .await?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit appended memory source",
                error,
            )
        })?;
        Ok(true)
    }

    /// Scoped FTS search: `query` is an FTS5 MATCH expression, `limit` its row budget.
    ///
    /// The caller owns the query shape - `AND` for an exact ask, `OR` for recall -
    /// and the row budget, because only the caller knows how many candidates it has
    /// to see before it can decide which of them are relevant. What this function
    /// owns is the part that must never depend on either: the scope, grant, binding
    /// and lineage-validity predicates, which all precede ranking and `LIMIT`.
    pub async fn search_memory(
        &self,
        principal: &StoreMemoryPrincipal,
        query: &str,
        limit: usize,
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
               AND EXISTS (SELECT 1 FROM memory_bindings b WHERE b.memory_asset_id = a.memory_asset_id AND b.principal_id = ?6)
               AND NOT EXISTS (SELECT 1 FROM lineage l LEFT JOIN allowed p ON p.id = l.id
                   LEFT JOIN memory_assets s ON s.memory_asset_id = l.id
                   JOIN memory_versions sv ON sv.memory_asset_id = s.memory_asset_id AND sv.version = s.current_version
                   WHERE l.root = a.memory_asset_id AND (coalesce(p.readable, 0) = 0 OR s.status IN ('invalidated', 'archived') OR s.current_version != l.version OR json_extract(sv.version_json, '$.validity') != 'valid'))
             ORDER BY bm25(memory_fts), a.memory_asset_id LIMIT ?7")
            .bind(query).bind(principal.project_id.as_ref().map(ToString::to_string))
            .bind(principal.task_id.as_ref().map(ToString::to_string)).bind(principal.agent_profile_id.as_ref().map(ToString::to_string))
            .bind(principal.session_id.as_ref().map(ToString::to_string)).bind(&principal.principal_id)
            .bind(i64::try_from(limit.min(32)).unwrap_or(32))
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

pub(super) async fn invalidate_tree(
    tx: &mut Transaction<'_, Sqlite>,
    root: &MemoryAssetId,
    include_root: bool,
    reason: &str,
) -> Result<Vec<MemoryAssetId>, StoreError> {
    let ids = sqlx::query_scalar::<_, String>("WITH RECURSIVE affected(id) AS (SELECT ? UNION SELECT d.derived_asset_id FROM memory_dependencies d JOIN affected a ON d.source_id = a.id WHERE d.source_kind = 'asset') SELECT id FROM affected ORDER BY id")
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
