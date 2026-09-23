use super::{
    FixtureExtractor, close_store, extraction_strategy, memory_fixture, observed_version,
    project_asset, seed_source_events,
};
use harness_memory::{
    InjectionMode, MemoryAction, MemoryBinding, MemoryBudget, MemoryGrant, MemoryPrincipal,
    MemoryService, MemorySource,
};
use harness_providers::{
    CancellationToken, ModelCapabilities, ModelProvider, ProviderError, ProviderFuture,
    ProviderRequest,
};
use harness_store_sqlite::{MemorySourceForget, MemorySourceRetentionUpdate, TombstoneRow};
use harness_types::{
    ContentHash, ErrorCode, InputId, MemoryAssetStatus, MemoryVersionRef, ProjectId, SessionId,
    TaskId, WorkspaceObservation,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct RevokeDuringAttempt {
    memory: MemoryService,
    principal: MemoryPrincipal,
    id: harness_types::MemoryAssetId,
    calls: Arc<AtomicUsize>,
}
impl ModelProvider for RevokeDuringAttempt {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::deepseek_fixture()
    }
    fn stream(&self, request: ProviderRequest, _: CancellationToken) -> ProviderFuture {
        let memory = self.memory.clone();
        let principal = self.principal.clone();
        let id = self.id.clone();
        let calls = Arc::clone(&self.calls);
        Box::pin(async move {
            assert!(
                request
                    .messages
                    .iter()
                    .any(|message| message.content.contains("revocable data"))
            );
            calls.fetch_add(1, Ordering::SeqCst);
            memory
                .invalidate(&principal, &id, "revoked between attempts")
                .await
                .unwrap();
            Err(ProviderError::new(
                ErrorCode::ServiceUnavailable,
                "external provider transient failure",
            ))
        })
    }
}

#[tokio::test]
async fn p4_runtime_rechecks_revision_before_each_provider_attempt() {
    let (_temp, store, memory) = memory_fixture().await;
    let project = ProjectId::generate();
    let principal = MemoryPrincipal::user("host").with_project(project.clone());
    let asset = memory
        .create_asset(&principal, project_asset(project.clone(), "revocable data"))
        .await
        .unwrap();
    let contribution = memory.contribute(
        &principal,
        &memory
            .search(&principal, "revocable", 8, None)
            .await
            .unwrap(),
        2000,
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(RevokeDuringAttempt {
        memory: memory.clone(),
        principal,
        id: asset.asset.memory_asset_id,
        calls: Arc::clone(&calls),
    });
    let runtime = harness_runtime::RuntimeService::new(
        Arc::clone(&store),
        provider,
        harness_runtime::RuntimeConfig::default(),
    );
    let session = SessionId::generate();
    let request = harness_runtime::RunRequest::new(
        session.clone(),
        TaskId::generate(),
        InputId::generate(),
        "keep working",
        WorkspaceObservation {
            project_id: project,
            worktree_id: "p4".to_owned(),
            base_commit: "fixture".to_owned(),
            observed_fingerprint: ContentHash::from_bytes(b"p4"),
        },
    )
    .with_memory(contribution);
    assert_eq!(
        runtime.run(request).await.unwrap_err().code(),
        ErrorCode::SequenceConflict
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "revoked frozen input must not reach another attempt"
    );
    assert_eq!(
        store.list_context_packets(&session).await.unwrap().len(),
        1,
        "past sanitized packet remains immutable audit"
    );
    drop(runtime);
    close_store(memory, store).await;
}

#[tokio::test]
async fn p4_grant_revocation_hides_derived_search_read_and_export() {
    let (_temp, store, memory) = memory_fixture().await;
    let project = ProjectId::generate();
    let owner = MemoryPrincipal::user("owner").with_project(project.clone());
    let worker = MemoryPrincipal::user("worker").with_project(project.clone());
    let root = memory
        .create_asset(&owner, project_asset(project, "root build data"))
        .await
        .unwrap();
    let mut grant = MemoryGrant {
        principal_id: "worker".to_owned(),
        memory_asset_id: Some(root.asset.memory_asset_id.clone()),
        project_id: owner.project_id.clone(),
        allowed_actions: [MemoryAction::Read].into_iter().collect(),
        revision: 1,
        active: true,
    };
    memory.grant(&owner, grant.clone()).await.unwrap();
    let summary = memory
        .derive_l2(
            &worker,
            &[MemoryVersionRef {
                memory_asset_id: root.asset.memory_asset_id.clone(),
                version: 1,
            }],
            "worker build summary",
        )
        .await
        .unwrap();
    memory
        .write_version(
            &worker,
            &summary.asset.memory_asset_id,
            1,
            observed_version("worker build summary"),
        )
        .await
        .unwrap();
    assert_eq!(
        memory
            .search(&worker, "build", 1, None)
            .await
            .unwrap()
            .hits
            .len(),
        1
    );
    grant.revision = 2;
    grant.active = false;
    memory.grant(&owner, grant).await.unwrap();
    assert!(
        memory
            .search(&worker, "build", 1, None)
            .await
            .unwrap()
            .hits
            .is_empty()
    );
    assert_eq!(
        memory
            .read(&worker, &summary.asset.memory_asset_id)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::PolicyDenied
    );
    assert_eq!(
        memory
            .export_versions(&worker, &summary.asset.memory_asset_id)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::PolicyDenied
    );
    close_store(memory, store).await;
}

#[tokio::test]
async fn p4_source_changes_semantic_merge_and_binding_are_versioned() {
    let (_temp, store, memory) = memory_fixture().await;
    let project = ProjectId::generate();
    let principal = MemoryPrincipal::user("host").with_project(project.clone());
    let hash = ContentHash::from_bytes(b"source-v1");
    let mut request = project_asset(project, "HTTPParser đọc đường dẫn");
    request
        .sources
        .push(MemorySource::file("src/parser.txt", hash.clone()));
    let root = memory.create_asset(&principal, request).await.unwrap();
    let summary = memory
        .derive_l2(
            &principal,
            &[MemoryVersionRef {
                memory_asset_id: root.asset.memory_asset_id.clone(),
                version: 1,
            }],
            "summary",
        )
        .await
        .unwrap();
    memory
        .write_version(
            &principal,
            &root.asset.memory_asset_id,
            1,
            observed_version("source updated"),
        )
        .await
        .unwrap();
    let mut proposal = observed_version("merged source updated");
    proposal.source_assets = vec![MemoryVersionRef {
        memory_asset_id: root.asset.memory_asset_id.clone(),
        version: 2,
    }];
    let merged = memory
        .write_version(&principal, &summary.asset.memory_asset_id, 1, proposal)
        .await
        .unwrap();
    assert_eq!(merged.asset.current_version, 2);
    assert_eq!(
        memory
            .search(&principal, "merged", 8, None)
            .await
            .unwrap()
            .hits
            .len(),
        1
    );
    memory
        .bind(
            &principal,
            &MemoryBinding {
                binding_id: "bootstrap".to_owned(),
                memory_asset_id: root.asset.memory_asset_id.clone(),
                principal_id: "host".to_owned(),
                injection_mode: InjectionMode::Bootstrap,
                priority: 100,
                revision: 1,
            },
        )
        .await
        .unwrap();
    let bootstrap = memory.bootstrap(&principal, 2000).await.unwrap();
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&store.paths().database_path)
        .read_only(true);
    let inspection = sqlx::SqlitePool::connect_with(options).await.unwrap();
    let binding_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM memory_bindings WHERE binding_id = 'bootstrap'")
            .fetch_one(&inspection)
            .await
            .unwrap();
    inspection.close().await;
    assert_eq!(
        binding_count, 1,
        "host-issued binding ID must round-trip through storage"
    );
    assert_eq!(bootstrap.blocks.len(), 1);
    assert!(bootstrap.blocks[0].text.contains("source updated"));
    let invalidated = memory
        .invalidate_source(&principal, "file", "src/parser.txt")
        .await
        .unwrap();
    assert_eq!(invalidated.len(), 2);
    assert!(
        memory
            .search(&principal, "merged", 8, None)
            .await
            .unwrap()
            .hits
            .is_empty()
    );
    close_store(memory, store).await;
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one store regression covers source replacement, retention, and dependent invalidation"
)]
async fn p4_source_invalidation_only_uses_the_current_version_sources() {
    let (_temp, store, memory) = memory_fixture().await;
    let project = ProjectId::generate();
    let principal = MemoryPrincipal::user("host").with_project(project.clone());
    let old_path = "src/old-parser.rs";
    let new_path = "src/current-parser.rs";
    let mut request = project_asset(project.clone(), "parser behavior from old source");
    request.sources.push(MemorySource::file(
        old_path,
        ContentHash::from_bytes(b"old source bytes"),
    ));
    let original = memory.create_asset(&principal, request).await.unwrap();
    let unrelated = memory
        .create_asset(
            &principal,
            project_asset(project.clone(), "unrelated source fact"),
        )
        .await
        .unwrap();
    let derived = memory
        .derive_l2(
            &principal,
            &[MemoryVersionRef {
                memory_asset_id: original.asset.memory_asset_id.clone(),
                version: 1,
            }],
            "summary from old source",
        )
        .await
        .unwrap();

    let mut correction = observed_version("parser behavior from current source");
    correction.sources.push(MemorySource::file(
        new_path,
        ContentHash::from_bytes(b"current source bytes"),
    ));
    memory
        .write_version(&principal, &original.asset.memory_asset_id, 1, correction)
        .await
        .unwrap();
    let mut derived_correction = observed_version("summary from unrelated source");
    derived_correction.source_assets = vec![MemoryVersionRef {
        memory_asset_id: unrelated.asset.memory_asset_id,
        version: 1,
    }];
    memory
        .write_version(
            &principal,
            &derived.asset.memory_asset_id,
            1,
            derived_correction,
        )
        .await
        .unwrap();

    let old_file_invalidation = memory
        .invalidate_source(&principal, "file", old_path)
        .await
        .unwrap();
    assert!(
        old_file_invalidation.is_empty(),
        "a source used only by a superseded version cannot invalidate the current asset"
    );
    let old_file_retention = store
        .update_memory_source_retention_and_journal(MemorySourceRetentionUpdate {
            source_kind: "file".to_owned(),
            source_id: old_path.to_owned(),
            status: MemoryAssetStatus::Archived,
            reason: None,
            journal_entry_id: "old-source-only.archive".to_owned(),
            journal_detail: serde_json::json!({"reason": "not a current source"}),
            created_unix_ms: 1,
        })
        .await
        .unwrap();
    assert!(
        old_file_retention.is_empty(),
        "global retention also ignores superseded source rows"
    );
    let current = memory
        .read(&principal, &original.asset.memory_asset_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.asset.status, MemoryAssetStatus::Active);
    let current_derived = memory
        .read(&principal, &derived.asset.memory_asset_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current_derived.asset.status, MemoryAssetStatus::Active);

    let current_file_invalidation = memory
        .invalidate_source(&principal, "file", new_path)
        .await
        .unwrap();
    assert_eq!(
        current_file_invalidation,
        vec![original.asset.memory_asset_id],
        "only the asset whose current version cites the changed file is invalidated; a derived asset whose current version changed lineage stays active"
    );
    assert_eq!(
        memory
            .read(&principal, &derived.asset.memory_asset_id)
            .await
            .unwrap()
            .unwrap()
            .asset
            .status,
        MemoryAssetStatus::Active,
        "historical dependency rows cannot invalidate a current version with different lineage"
    );
    close_store(memory, store).await;
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one privacy regression covers historical source and transitive payload deletion"
)]
async fn p4_source_forget_removes_assets_that_only_match_historical_lineage() {
    let (_temp, store, memory) = memory_fixture().await;
    let project = ProjectId::generate();
    let principal = MemoryPrincipal::user("host").with_project(project.clone());
    let forgotten_path = "src/forgotten-history.rs";
    let mut root_request = project_asset(project.clone(), "old sensitive source content");
    root_request.sources.push(MemorySource::file(
        forgotten_path,
        ContentHash::from_bytes(b"old sensitive bytes"),
    ));
    let root = memory.create_asset(&principal, root_request).await.unwrap();
    let unrelated = memory
        .create_asset(
            &principal,
            project_asset(project.clone(), "unrelated current source"),
        )
        .await
        .unwrap();
    let derived = memory
        .derive_l2(
            &principal,
            &[MemoryVersionRef {
                memory_asset_id: root.asset.memory_asset_id.clone(),
                version: 1,
            }],
            "derived sensitive history",
        )
        .await
        .unwrap();

    let mut root_correction = observed_version("new root content");
    root_correction.sources.push(MemorySource::file(
        "src/current-root.rs",
        ContentHash::from_bytes(b"current root bytes"),
    ));
    memory
        .write_version(&principal, &root.asset.memory_asset_id, 1, root_correction)
        .await
        .unwrap();
    let mut derived_correction = observed_version("new unrelated derived content");
    derived_correction.source_assets = vec![MemoryVersionRef {
        memory_asset_id: unrelated.asset.memory_asset_id.clone(),
        version: 1,
    }];
    memory
        .write_version(
            &principal,
            &derived.asset.memory_asset_id,
            1,
            derived_correction,
        )
        .await
        .unwrap();

    let forgotten = store
        .forget_memory_source_and_tombstone(MemorySourceForget {
            source_kind: "file".to_owned(),
            source_id: forgotten_path.to_owned(),
            tombstone: TombstoneRow {
                tombstone_id: "tombstone-forgotten-history".to_owned(),
                source_kind: "file".to_owned(),
                source_id: forgotten_path.to_owned(),
                reason: "remove source and all retained derived history".to_owned(),
                surviving_copies: vec!["external-backup".to_owned()],
                created_unix_ms: 1,
            },
            journal_entry_id: "journal-forgotten-history".to_owned(),
            journal_action: "forget".to_owned(),
            journal_detail: serde_json::json!({"reason": "historical source purge"}),
        })
        .await
        .unwrap();
    assert_eq!(forgotten.len(), 2);
    assert!(forgotten.contains(&root.asset.memory_asset_id));
    assert!(forgotten.contains(&derived.asset.memory_asset_id));
    assert!(
        memory
            .read(&principal, &root.asset.memory_asset_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        memory
            .read(&principal, &derived.asset.memory_asset_id)
            .await
            .unwrap()
            .is_none(),
        "forget must remove assets with historical versions that retained the forgotten source"
    );
    assert!(
        memory
            .read(&principal, &unrelated.asset.memory_asset_id)
            .await
            .unwrap()
            .is_some(),
        "unrelated current data survives the historical purge"
    );
    assert!(store.is_tombstoned("file", forgotten_path).await.unwrap());
    close_store(memory, store).await;
}

struct CancelingExtractor(CancellationToken);
impl harness_memory::MemoryExtractor for CancelingExtractor {
    fn version(&self) -> &'static str {
        "mock-extractor-v1"
    }
    fn extract<'a>(
        &'a self,
        _: &'a [harness_memory::SourceProjection],
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<String, harness_types::HarnessError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async {
            self.0.cancel();
            std::future::pending().await
        })
    }
}

#[tokio::test]
async fn p4_inflight_shutdown_and_each_budget_dimension_pause_without_cursor_advance() {
    let (_temp, store, memory) = memory_fixture().await;
    let (session, _) = seed_source_events(&store, 1).await;
    let principal = MemoryPrincipal::user("host").with_session(session.clone());
    for dimension in 0..4 {
        let strategy = extraction_strategy(&format!("budget-{dimension}"));
        memory
            .schedule_backlog(&session, &strategy, 1)
            .await
            .unwrap();
        let mut budget = MemoryBudget::calls(1);
        match dimension {
            0 => budget.max_source_bytes = 0,
            1 => budget.max_tokens = 0,
            2 => budget.max_cost_units = 0,
            _ => {}
        }
        let cancel = CancellationToken::new();
        let extractor: &dyn harness_memory::MemoryExtractor = if dimension == 3 {
            &CancelingExtractor(cancel.clone())
        } else {
            &FixtureExtractor
        };
        let report = memory
            .catch_up(&principal, &strategy, extractor, &budget, &cancel)
            .await
            .unwrap();
        assert_eq!(report.completed, 0);
        assert_eq!(report.paused, 1);
        assert_eq!(
            memory.extraction_cursor(&session, &strategy).await.unwrap(),
            0
        );
    }
    close_store(memory, store).await;
}
