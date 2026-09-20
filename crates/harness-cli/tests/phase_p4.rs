use std::{collections::BTreeSet, sync::Arc};

use harness_memory::{
    CreateMemoryAsset, EvidenceState, ExtractionJobStatus, ExtractionScope, ExtractionStrategy,
    MEMORY_CONTRACT_VERSION, MemoryAction, MemoryGrant, MemoryLayer, MemoryPrincipal,
    MemoryService, PublicationPolicy, RetentionAction, WriteMemoryVersion,
};
use harness_session::{AdmitInputRequest, SessionService};
use harness_store_sqlite::{SqliteStore, StoreFaultPlan, StoreFaultPoint, WriterOpenOptions};
use harness_types::{
    ContentHash, ErrorCode, HostId, InputId, MemoryAssetStatus, MemoryScope, ProjectId, SessionId,
    SourceAuthority, TaskId, Validity, WorkspaceObservation,
};
use serde_json::Map;
use tempfile::TempDir;

#[path = "phase_p4/extended.rs"]
mod extended;

struct FixtureExtractor;

fn run_memory_cli(
    data_dir: &std::path::Path,
    session_id: &SessionId,
    args: &[&str],
) -> serde_json::Value {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ha"))
        .args([
            "memory",
            "--data-dir",
            data_dir.to_str().unwrap(),
            "--session-id",
            session_id.as_str(),
            "--json",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Run a memory command that must fail, returning its stderr.
fn run_memory_cli_failure(
    data_dir: &std::path::Path,
    session_id: &SessionId,
    args: &[&str],
) -> String {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ha"))
        .args([
            "memory",
            "--data-dir",
            data_dir.to_str().unwrap(),
            "--session-id",
            session_id.as_str(),
            "--json",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "the command was expected to refuse: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The whole point of review-then-confirm, at the level a person actually uses.
#[tokio::test]
async fn p4_s07_cli_reviews_and_confirms_candidates_in_one_batch() {
    let (temp, store, memory) = memory_fixture().await;
    let (session_id, _) = seed_source_events(&store, 1).await;
    close_store(memory, store).await;
    let catch_up = run_memory_cli(
        temp.path(),
        &session_id,
        &["catch-up", "--budget", "1", "--extractor", "mock"],
    );
    assert_eq!(catch_up["memory"]["report"]["completed"], 1);
    let id = catch_up["memory"]["report"]["asset_ids"][0]
        .as_str()
        .unwrap()
        .to_owned();

    // Review first: the listing shows what would be confirmed, and confirms nothing.
    let listed = run_memory_cli(temp.path(), &session_id, &["candidates", "--limit", "8"]);
    assert_eq!(listed["memory"]["count"], 1);
    assert_eq!(
        listed["memory"]["candidates"][0]["asset"]["memory_asset_id"],
        id.as_str()
    );
    assert!(
        listed["memory"]["candidates"][0]["content_preview"]
            .as_str()
            .unwrap()
            .contains("parser"),
        "the reviewer must see the content: {listed}"
    );
    assert_eq!(
        run_memory_cli(temp.path(), &session_id, &["search", "parser"])["memory"]["state"],
        "empty",
        "nothing is retrievable before a human confirms it"
    );

    // Confirmation without --confirm is refused: it is the host's act.
    let refused = run_memory_cli_failure(temp.path(), &session_id, &["confirm", "--limit", "4"]);
    assert!(refused.contains("--confirm"), "{refused}");

    let confirmed = run_memory_cli(
        temp.path(),
        &session_id,
        &["confirm", "--limit", "4", "--confirm"],
    );
    assert_eq!(confirmed["memory"]["confirmed"], 1);
    assert_eq!(
        confirmed["memory"]["assets"][0]["asset"]["status"],
        "active"
    );
    assert_eq!(
        run_memory_cli(temp.path(), &session_id, &["search", "parser"])["memory"]["state"],
        "found",
        "a confirmed asset is retrievable"
    );
    assert_eq!(
        run_memory_cli(temp.path(), &session_id, &["candidates"])["memory"]["count"],
        0,
        "a confirmed asset is no longer waiting"
    );
}

#[tokio::test]
async fn p4_s07_cli_catch_up_inspects_memory_and_resume_is_independent() {
    let (temp, store, memory) = memory_fixture().await;
    let (session_id, _) = seed_source_events(&store, 1).await;
    close_store(memory, store).await;
    let catch_up = run_memory_cli(
        temp.path(),
        &session_id,
        &["catch-up", "--budget", "1", "--extractor", "mock"],
    );
    assert_eq!(catch_up["memory"]["report"]["completed"], 1);
    let id = catch_up["memory"]["report"]["asset_ids"][0]
        .as_str()
        .unwrap();
    let inspect = run_memory_cli(temp.path(), &session_id, &["inspect", id]);
    assert_eq!(inspect["memory"]["versions"].as_array().unwrap().len(), 1);
    assert_eq!(inspect["memory"]["asset"]["asset"]["status"], "candidate");
    assert_eq!(
        inspect["memory"]["versions"][0]["record"]["source_event_refs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    run_memory_cli(
        temp.path(),
        &session_id,
        &["publish", id, "--expected-version", "1", "--confirm"],
    );
    let search = run_memory_cli(temp.path(), &session_id, &["search", "parser"]);
    assert_eq!(search["memory"]["state"], "found");
    let summary = run_memory_cli(
        temp.path(),
        &session_id,
        &[
            "summarize",
            id,
            "--expected-version",
            "2",
            "--content",
            "parser summary",
        ],
    );
    assert_eq!(summary["memory"]["layer"], "l2");
    assert_eq!(
        run_memory_cli(temp.path(), &session_id, &["jobs"])["memory"]["jobs"][0]["status"],
        "completed"
    );
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ha"))
        .args([
            "resume",
            "--data-dir",
            temp.path().to_str().unwrap(),
            "--session-id",
            session_id.as_str(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    run_memory_cli(
        temp.path(),
        &session_id,
        &["invalidate", id, "--reason", "new decision"],
    );
    assert_eq!(
        run_memory_cli(temp.path(), &session_id, &["search", "parser"])["memory"]["state"],
        "empty"
    );
}
impl harness_memory::MemoryExtractor for FixtureExtractor {
    fn version(&self) -> &'static str {
        "mock-extractor-v1"
    }
    fn extract<'a>(
        &'a self,
        sources: &'a [harness_memory::SourceProjection],
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<String, harness_types::HarnessError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            Ok(serde_json::to_string(&harness_memory::ExtractionOutput {
                candidates: sources
                    .iter()
                    .map(|source| harness_memory::ExtractedCandidate {
                        content: source.content.clone(),
                        source_event_refs: vec![source.event_id.clone()],
                    })
                    .collect(),
            })
            .unwrap())
        })
    }
}

struct FailingExtractor;

struct ScriptedExtractor {
    version: String,
    reply: String,
}
impl harness_memory::MemoryExtractor for ScriptedExtractor {
    fn version(&self) -> &str {
        &self.version
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
        Box::pin(async { Ok(self.reply.clone()) })
    }
}

async fn memory_row_counts(store: &SqliteStore) -> (i64, i64, i64) {
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&store.paths().database_path)
        .read_only(true);
    let pool = sqlx::SqlitePool::connect_with(options).await.unwrap();
    let counts = sqlx::query_as::<_, (i64, i64, i64)>("SELECT (SELECT COUNT(*) FROM memory_assets), (SELECT COUNT(*) FROM memory_versions), (SELECT COUNT(*) FROM memory_dependencies)").fetch_one(&pool).await.unwrap();
    pool.close().await;
    counts
}
impl harness_memory::MemoryExtractor for FailingExtractor {
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
            Err(harness_types::HarnessError::new(
                ErrorCode::ServiceUnavailable,
                "external extractor offline",
            ))
        })
    }
}

#[tokio::test]
async fn p4_c05_optional_extraction_failure_does_not_block_working_state_resume() {
    let (temp, store, memory) = memory_fixture().await;
    let (session_id, _) = seed_source_events(&store, 1).await;
    let principal = MemoryPrincipal::user("host").with_session(session_id.clone());
    let strategy = extraction_strategy("optional-offline");
    memory
        .schedule_backlog(&session_id, &strategy, 2)
        .await
        .unwrap();
    let result = memory
        .catch_up(
            &principal,
            &strategy,
            &FailingExtractor,
            &harness_memory::MemoryBudget::calls(1),
            &harness_providers::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.failed, 1);
    assert_eq!(
        memory
            .extraction_cursor(&session_id, &strategy)
            .await
            .unwrap(),
        0
    );
    let before = SessionService::new(Arc::clone(&store))
        .recover(&session_id)
        .await
        .unwrap()
        .working_state;
    close_store(memory, store).await;
    let store = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(temp.path(), HostId::generate()))
            .await
            .unwrap(),
    );
    let memory = MemoryService::new(Arc::clone(&store));
    let runtime = harness_runtime::RuntimeService::new(
        Arc::clone(&store),
        Arc::new(harness_providers::MockProvider::text("unused")),
        harness_runtime::RuntimeConfig::default(),
    );
    let resumed = runtime.resume(&session_id).await.unwrap();
    assert!(!resumed.blocked);
    assert_eq!(resumed.working_state, before);
    drop(runtime);
    close_store(memory, store).await;
}

/// Extraction settles model inference as a candidate; a human must be able to finish it.
///
/// The measured gap: candidates existed but were only publishable one id at a time, so a
/// catch-up over a long session left knowledge nobody would ever confirm — and candidate
/// assets are deliberately invisible to search, so it looked like memory had vanished.
#[tokio::test]
async fn p4_a_candidate_becomes_searchable_once_the_host_confirms_it() {
    let (_temp, store, memory) = memory_fixture().await;
    let (session_id, _) = seed_source_events(&store, 1).await;
    let principal = MemoryPrincipal::user("host").with_session(session_id.clone());
    let strategy = extraction_strategy("confirm-flow");
    memory
        .schedule_backlog(&session_id, &strategy, 1)
        .await
        .unwrap();
    let settled = memory
        .catch_up(
            &principal,
            &strategy,
            &FixtureExtractor,
            &harness_memory::MemoryBudget::calls(1),
            &harness_providers::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(settled.completed, 1);

    let candidates = memory.list_candidates(&principal, 16).await.unwrap();
    assert_eq!(candidates.len(), 1, "{candidates:?}");
    let candidate = &candidates[0];
    assert_eq!(
        candidate.asset.status,
        harness_types::MemoryAssetStatus::Candidate
    );
    assert!(
        candidate.current.content.contains("parser constraints"),
        "a reviewer has to see what would be confirmed: {}",
        candidate.current.content
    );

    // A candidate is not evidence and not retrievable: search must not find it yet.
    let before = memory
        .search(&principal, "parser constraints", 8, None)
        .await
        .unwrap();
    assert_eq!(before.state, harness_memory::RetrievalState::Empty);

    let confirmed = memory
        .confirm(
            &principal,
            std::slice::from_ref(&candidate.asset.memory_asset_id),
        )
        .await
        .unwrap();
    assert_eq!(confirmed.len(), 1);
    assert_eq!(
        confirmed[0].asset.status,
        harness_types::MemoryAssetStatus::Active
    );
    assert_eq!(
        confirmed[0].asset.current_version, 2,
        "confirmation is a version"
    );

    let after = memory
        .search(&principal, "parser constraints", 8, None)
        .await
        .unwrap();
    assert_eq!(
        after.state,
        harness_memory::RetrievalState::Found,
        "a confirmed asset must be retrievable: {after:?}"
    );
    assert_eq!(after.hits.len(), 1);
    assert_eq!(
        after.hits[0].asset.memory_asset_id,
        candidate.asset.memory_asset_id
    );
    assert!(
        memory
            .list_candidates(&principal, 16)
            .await
            .unwrap()
            .is_empty(),
        "a confirmed asset is no longer waiting"
    );
    close_store(memory, store).await;
}

#[tokio::test]
async fn p4_jobs_are_listed_for_the_stream_the_host_acts_on() {
    let (_temp, store, memory) = memory_fixture().await;
    let (first_stream, _) = seed_source_events(&store, 1).await;
    let (second_stream, _) = seed_source_events(&store, 1).await;
    memory
        .schedule_backlog(&first_stream, &extraction_strategy("stream-one"), 2)
        .await
        .unwrap();
    memory
        .schedule_backlog(&second_stream, &extraction_strategy("stream-two"), 2)
        .await
        .unwrap();

    let all = memory.list_jobs().await.unwrap();
    assert_eq!(
        all.len(),
        2,
        "the whole store still sees every job: {all:?}"
    );
    let scoped = memory.list_jobs_for(&first_stream).await.unwrap();
    assert_eq!(scoped.len(), 1, "{scoped:?}");
    assert!(
        scoped.iter().all(|job| job.source_stream == first_stream),
        "a stream-scoped listing must not carry another stream's jobs: {scoped:?}"
    );
    let other = memory.list_jobs_for(&second_stream).await.unwrap();
    assert_eq!(other.len(), 1, "{other:?}");
    assert!(other.iter().all(|job| job.source_stream == second_stream));
    close_store(memory, store).await;
}

#[tokio::test]
async fn p4_extraction_scope_is_the_hosts_choice_not_the_streams() {
    let (_temp, store, memory) = memory_fixture().await;
    let (session_id, _) = seed_source_events(&store, 1).await;
    let project = ProjectId::generate();
    let host = MemoryPrincipal::user("host")
        .with_project(project.clone())
        .with_session(session_id.clone());

    // The stream a job reads is a session; the host declares that what comes out of it
    // is project knowledge.
    let mut strategy = extraction_strategy("scope-project");
    strategy.asset_scope = ExtractionScope::Project;
    memory
        .schedule_backlog(&session_id, &strategy, 1)
        .await
        .unwrap();
    let report = memory
        .catch_up(
            &host,
            &strategy,
            &FixtureExtractor,
            &harness_memory::MemoryBudget::calls(1),
            &harness_providers::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(report.completed, 1, "{report:?}");
    let asset_id = report.asset_ids.first().expect("one settled asset").clone();

    let stored = memory
        .read(&host, &asset_id)
        .await
        .unwrap()
        .expect("the extractor's principal reads its own asset");
    assert_eq!(stored.asset.scope, MemoryScope::Project);
    assert!(
        stored.session_id.is_none() && stored.task_id.is_none(),
        "project knowledge must not be bound to the run that produced it: {stored:?}"
    );

    // The promise: a later session of the same project can read it. Before the repair
    // every extracted asset carried the source session, so this read returned nothing.
    let later = MemoryPrincipal::user("host")
        .with_project(project)
        .with_session(SessionId::generate());
    assert!(
        memory.read(&later, &asset_id).await.unwrap().is_some(),
        "another session of the project must see project-scoped extraction output"
    );

    // The default is unchanged: a session-scoped extraction stays private to its stream.
    let (other_session, _) = seed_source_events(&store, 1).await;
    let private_host = MemoryPrincipal::user("host")
        .with_project(ProjectId::generate())
        .with_session(other_session.clone());
    let private_strategy = extraction_strategy("scope-session");
    memory
        .schedule_backlog(&other_session, &private_strategy, 1)
        .await
        .unwrap();
    let private_report = memory
        .catch_up(
            &private_host,
            &private_strategy,
            &FixtureExtractor,
            &harness_memory::MemoryBudget::calls(1),
            &harness_providers::CancellationToken::new(),
        )
        .await
        .unwrap();
    let private_id = private_report
        .asset_ids
        .first()
        .expect("one settled asset")
        .clone();
    let private_asset = memory
        .read(&private_host, &private_id)
        .await
        .unwrap()
        .expect("its own session reads it");
    assert_eq!(private_asset.asset.scope, MemoryScope::Session);
    assert_eq!(private_asset.session_id.as_ref(), Some(&other_session));
    let stranger = MemoryPrincipal::user("host")
        .with_project(ProjectId::generate())
        .with_session(SessionId::generate());
    assert!(
        memory.read(&stranger, &private_id).await.is_err(),
        "a session-scoped asset is not readable from another scope"
    );

    close_store(memory, store).await;
}

#[tokio::test]
async fn p4_c13_injected_memory_never_becomes_independent_evidence() {
    let (_temp, store, memory) = memory_fixture().await;
    let (session_id, task_id) = seed_source_events(&store, 1).await;
    let principal = MemoryPrincipal::user("host").with_session(session_id.clone());
    let session = SessionService::new(Arc::clone(&store));
    let mut payload = Map::new();
    payload.insert(
        "text".to_owned(),
        serde_json::json!("injected summary repeated as if independently observed"),
    );
    session
        .append_runtime_event(&session_id, &task_id, "memory.injected", payload, false)
        .await
        .unwrap();
    let strategy = extraction_strategy("reinjection");
    let jobs = memory
        .schedule_backlog(&session_id, &strategy, 1)
        .await
        .unwrap();
    let first = memory.lease_job(&jobs[0].job_id, "worker").await.unwrap();
    let original = memory
        .extract_lease(&principal, &first, &FixtureExtractor, 4096, SESSION_SCOPE)
        .await
        .unwrap();
    assert_eq!(original.len(), 1);
    let reinjected = memory.lease_job(&jobs[1].job_id, "worker").await.unwrap();
    assert!(
        memory
            .extract_lease(
                &principal,
                &reinjected,
                &FixtureExtractor,
                4096,
                SESSION_SCOPE
            )
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        memory
            .extraction_cursor(&session_id, &strategy)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        memory.list_jobs().await.unwrap()[1].disposition.as_deref(),
        Some("no_facts")
    );
    assert!(
        !original[0]
            .current
            .record
            .source_event_refs
            .contains(&jobs[1].source_event_ids[0])
    );
    drop(session);
    close_store(memory, store).await;
}

#[tokio::test]
async fn p4_s05_bounded_fts_retrieval_contributes_through_context_builder() {
    use harness_session::{ContextBlock, ContextBlockKind, ContextBuildRequest, ContextBuilder};
    let (_temp, store, memory) = memory_fixture().await;
    let project = ProjectId::generate();
    let principal = MemoryPrincipal::user("owner").with_project(project.clone());
    memory
        .create_asset(
            &principal,
            project_asset(project, "parser preserves Unicode"),
        )
        .await
        .unwrap();
    let result = memory
        .search(&principal, "parser", 8, None)
        .await
        .expect("real scoped FTS search");
    assert_eq!(result.state, harness_memory::RetrievalState::Found);
    let contribution = memory.contribute(&principal, &result, 2000);
    assert_eq!(contribution.blocks.len(), 1);
    let mut forged_rendering = contribution.clone();
    forged_rendering.blocks[0]
        .text
        .push_str(" forged instruction");
    assert_eq!(
        memory
            .validate_contribution(&forged_rendering)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::InvalidHash
    );
    assert!(memory.contribute(&principal, &result, 0).blocks.is_empty());
    let (session_id, task_id) = seed_source_events(&store, 1).await;
    let recovery = SessionService::new(Arc::clone(&store))
        .recover(&session_id)
        .await
        .unwrap();
    let built = ContextBuilder::new()
        .build(ContextBuildRequest {
            session_id,
            task_id,
            checkpoint_id: "p4-context".to_owned(),
            through_event_seq: recovery.replayed_through_sequence,
            recovery,
            project_rules: vec![ContextBlock::mandatory(
                "rule",
                ContextBlockKind::ProjectRule,
                "Keep parser source intact",
            )],
            optional_blocks: contribution.blocks,
            recent_tail: vec![],
            context_window_tokens: 8192,
            output_reservation_tokens: 512,
            protocol_overhead_tokens: 128,
            safety_margin_tokens: 128,
            optional_token_budget: 2000,
            memory_versions: contribution.versions,
        })
        .unwrap();
    assert!(built.packet.content.contains("parser preserves Unicode"));
    assert!(built.mandatory_block_ids.contains(&"rule".to_owned()));
    assert_eq!(built.packet.memory_versions.len(), 1);
    built.packet.validate().unwrap();
    close_store(memory, store).await;
}

struct TimeoutVector;
impl harness_memory::VectorAdapter for TimeoutVector {
    fn probe(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), harness_types::HarnessError>> + Send + '_>,
    > {
        Box::pin(std::future::pending())
    }
}

#[tokio::test]
async fn p4_s06_invalidation_cache_and_budgets_are_enforced() {
    let (_temp, store, memory) = memory_fixture().await;
    let project = ProjectId::generate();
    let principal = MemoryPrincipal::user("owner").with_project(project.clone());
    let asset = memory
        .create_asset(&principal, project_asset(project, "parser cache source"))
        .await
        .unwrap();
    let result = memory.search(&principal, "parser", 8, None).await.unwrap();
    let contribution = memory.contribute(&principal, &result, 2000);
    memory
        .validate_contribution(&contribution)
        .await
        .expect("fresh memory snapshot is dispatchable");
    memory
        .invalidate(&principal, &asset.asset.memory_asset_id, "decision revoked")
        .await
        .unwrap();
    assert!(memory.validate_contribution(&contribution).await.is_err());
    assert!(
        memory
            .search(&principal, "parser", 8, None)
            .await
            .unwrap()
            .hits
            .is_empty()
    );
    assert_eq!(
        memory
            .export_versions(&principal, &asset.asset.memory_asset_id)
            .await
            .unwrap()
            .len(),
        1,
        "audit version remains immutable"
    );
    close_store(memory, store).await;
}

#[tokio::test]
async fn p4_c20_revocation_invalidates_transitive_derived_context() {
    let (_temp, store, memory) = memory_fixture().await;
    let project = ProjectId::generate();
    let principal = MemoryPrincipal::user("owner").with_project(project.clone());
    let source = memory
        .create_asset(&principal, project_asset(project, "revocable parser rule"))
        .await
        .unwrap();
    let first = memory
        .derive_l2(
            &principal,
            &[harness_types::MemoryVersionRef {
                memory_asset_id: source.asset.memory_asset_id.clone(),
                version: 1,
            }],
            "parser summary one",
        )
        .await
        .unwrap();
    let mut confirmed = observed_version(&first.current.content);
    confirmed.authority = SourceAuthority::User;
    confirmed.evidence = EvidenceState::UserConfirmed;
    confirmed.user_confirmed = true;
    let first = memory
        .write_version(&principal, &first.asset.memory_asset_id, 1, confirmed)
        .await
        .unwrap();
    let second = memory
        .derive_l2(
            &principal,
            &[harness_types::MemoryVersionRef {
                memory_asset_id: first.asset.memory_asset_id.clone(),
                version: 2,
            }],
            "parser summary two",
        )
        .await
        .unwrap();
    for asset in [&second] {
        let mut version = observed_version(&asset.current.content);
        version.authority = SourceAuthority::User;
        version.evidence = EvidenceState::UserConfirmed;
        version.user_confirmed = true;
        memory
            .write_version(&principal, &asset.asset.memory_asset_id, 1, version)
            .await
            .unwrap();
    }
    let contribution = memory.contribute(
        &principal,
        &memory.search(&principal, "parser", 8, None).await.unwrap(),
        2000,
    );
    assert_eq!(contribution.blocks.len(), 3);
    let invalidated = memory
        .invalidate(
            &principal,
            &source.asset.memory_asset_id,
            "superseded decision",
        )
        .await
        .unwrap();
    assert_eq!(invalidated.len(), 3);
    assert!(memory.validate_contribution(&contribution).await.is_err());
    assert!(
        memory
            .search(&principal, "parser", 8, None)
            .await
            .unwrap()
            .hits
            .is_empty()
    );
    close_store(memory, store).await;
}

#[tokio::test]
async fn p4_c30_budget_and_shutdown_pause_jobs_durably() {
    let (temp, store, memory) = memory_fixture().await;
    let (session_id, _) = seed_source_events(&store, 2).await;
    let principal = MemoryPrincipal::user("host").with_session(session_id.clone());
    let strategy = extraction_strategy("bounded-catch-up");
    memory
        .schedule_backlog(&session_id, &strategy, 1)
        .await
        .unwrap();
    let cancel = harness_providers::CancellationToken::new();
    let report = memory
        .catch_up(
            &principal,
            &strategy,
            &FixtureExtractor,
            &harness_memory::MemoryBudget::calls(0),
            &cancel,
        )
        .await
        .expect("budget exhaustion pauses durable jobs");
    assert_eq!(report.calls, 0);
    assert_eq!(report.paused, 2);
    assert!(
        memory
            .list_jobs()
            .await
            .unwrap()
            .iter()
            .all(|job| job.status == ExtractionJobStatus::Paused)
    );
    close_store(memory, store).await;
    let store = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(temp.path(), HostId::generate()))
            .await
            .unwrap(),
    );
    let memory = MemoryService::new(Arc::clone(&store));
    let report = memory
        .catch_up(
            &principal,
            &strategy,
            &FixtureExtractor,
            &harness_memory::MemoryBudget::calls(1),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!((report.calls, report.completed, report.paused), (1, 1, 1));
    cancel.cancel();
    let stopped = memory
        .catch_up(
            &principal,
            &strategy,
            &FixtureExtractor,
            &harness_memory::MemoryBudget::calls(1),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(stopped.calls, 0);
    assert_eq!(
        memory
            .extraction_cursor(&session_id, &strategy)
            .await
            .unwrap(),
        1
    );
    close_store(memory, store).await;
}

#[tokio::test]
async fn p4_c26_fts_empty_vietnamese_identifiers_and_vector_timeout_are_distinct() {
    let (_temp, store, memory) = memory_fixture().await;
    let project = ProjectId::generate();
    let principal = MemoryPrincipal::user("owner").with_project(project.clone());
    memory
        .create_asset(
            &principal,
            project_asset(
                project,
                "Đường dẫn cấu hình parseHttpResponse parse_http_response",
            ),
        )
        .await
        .unwrap();
    for query in [
        "đường dẫn",
        "duong dan",
        "cấu hình",
        "cau hinh",
        "parseHttpResponse",
        "parse_http_response",
        "parse http response",
    ] {
        let result = memory.search(&principal, query, 8, None).await.unwrap();
        assert_eq!(
            result.state,
            harness_memory::RetrievalState::Found,
            "query={query}"
        );
        assert_eq!(result.hits.len(), 1);
    }
    for query in ["missingterm", "\" OR * --", ""] {
        assert_eq!(
            memory
                .search(&principal, query, 8, None)
                .await
                .unwrap()
                .state,
            harness_memory::RetrievalState::Empty
        );
    }
    let degraded = memory
        .search(&principal, "duong dan", 8, Some(&TimeoutVector))
        .await
        .unwrap();
    assert_eq!(degraded.state, harness_memory::RetrievalState::Degraded);
    assert_eq!(degraded.hits.len(), 1);
    close_store(memory, store).await;
}

#[tokio::test]
async fn p4_s04_l1_l2_and_confirmed_profile_updates_preserve_sources() {
    let (_temp, store, memory) = memory_fixture().await;
    let (session_id, _) = seed_source_events(&store, 1).await;
    let principal = MemoryPrincipal::user("host").with_session(session_id.clone());
    let strategy = extraction_strategy("l1-l2");
    let job = memory
        .schedule_backlog(&session_id, &strategy, 2)
        .await
        .unwrap()
        .remove(0);
    let lease = memory.lease_job(&job.job_id, "extractor").await.unwrap();
    let assets = memory
        .extract_lease(&principal, &lease, &FixtureExtractor, 4096, SESSION_SCOPE)
        .await
        .expect("real atomic L1 settlement");
    assert_eq!(assets.len(), 1);
    assert_eq!(assets[0].asset.status, MemoryAssetStatus::Candidate);
    assert_eq!(
        assets[0].current.record.source_event_refs,
        job.source_event_ids
    );
    let source = harness_types::MemoryVersionRef {
        memory_asset_id: assets[0].asset.memory_asset_id.clone(),
        version: 1,
    };
    let summary = memory
        .derive_l2(&principal, &[source], "Parser constraints summary")
        .await
        .unwrap();
    assert_eq!(summary.layer, MemoryLayer::L2);
    assert_eq!(
        summary.current.record.source_event_refs,
        job.source_event_ids
    );
    assert_eq!(summary.current.record.evidence_state, "derived");
    let profile = harness_types::AgentProfileId::generate();
    let principal = principal.with_profile(profile.clone());
    let mut request = project_asset(
        ProjectId::generate(),
        "Prefer concise Vietnamese explanations",
    );
    request.scope = MemoryScope::AgentProfile;
    request.project_id = None;
    request.agent_profile_id = Some(profile);
    request.layer = MemoryLayer::L3;
    request.authority = SourceAuthority::User;
    request.evidence = EvidenceState::UserConfirmed;
    request.user_confirmed = true;
    let profile = memory.create_asset(&principal, request).await.unwrap();
    assert_eq!(profile.asset.status, MemoryAssetStatus::Active);
    let changed = memory
        .write_version(
            &principal,
            &profile.asset.memory_asset_id,
            1,
            observed_version("inferred new preference"),
        )
        .await
        .unwrap();
    assert_eq!(changed.asset.status, MemoryAssetStatus::Candidate);
    close_store(memory, store).await;
}

#[test]
fn p4_s01_memory_contracts_and_publication_policy_are_versioned() {
    assert_eq!(MEMORY_CONTRACT_VERSION, 1);
    assert!(PublicationPolicy::scopes_are_host_issued());
    assert_eq!(PublicationPolicy::supported_scope_count(), 5);

    let policy = PublicationPolicy;
    let inference = policy.classify(
        SourceAuthority::ModelProposed,
        EvidenceState::ModelInference,
        MemoryLayer::L3,
        false,
    );
    assert_eq!(inference.status, MemoryAssetStatus::Candidate);
    assert!(!inference.publish_allowed);
    let confirmed = policy.classify(
        SourceAuthority::User,
        EvidenceState::UserConfirmed,
        MemoryLayer::L3,
        true,
    );
    assert_eq!(confirmed.status, MemoryAssetStatus::Active);
    assert!(confirmed.publish_allowed);
    assert!(!PublicationPolicy::supports_retention(
        RetentionAction::Purge
    ));

    let project_id = ProjectId::generate();
    let principal = MemoryPrincipal::user("actor-a").with_project(project_id.clone());
    let grant = MemoryGrant {
        principal_id: "actor-b".to_owned(),
        memory_asset_id: None,
        project_id: Some(project_id),
        allowed_actions: BTreeSet::from([MemoryAction::Publish]),
        revision: 1,
        active: true,
    };
    assert!(
        !grant.allows(&principal, MemoryAction::Publish),
        "a role-like name or shared project cannot replace exact principal authority"
    );
}

async fn memory_fixture() -> (TempDir, Arc<SqliteStore>, MemoryService) {
    let temp = TempDir::new().expect("temporary memory store");
    let store = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(temp.path(), HostId::generate()))
            .await
            .expect("memory writer opens"),
    );
    let memory = MemoryService::new(Arc::clone(&store));
    (temp, store, memory)
}

fn project_asset(project_id: ProjectId, content: &str) -> CreateMemoryAsset {
    CreateMemoryAsset {
        kind: "project_fact".to_owned(),
        scope: MemoryScope::Project,
        layer: MemoryLayer::L1,
        project_id: Some(project_id),
        task_id: None,
        agent_profile_id: None,
        session_id: None,
        visibility: "scoped".to_owned(),
        content: content.to_owned(),
        authority: SourceAuthority::RuntimeObserved,
        evidence: EvidenceState::VerifiedObservation,
        user_confirmed: false,
        source_event_refs: Vec::new(),
        source_file_hashes: Vec::new(),
        source_commit: Some("fixture-r1".to_owned()),
        provenance_kind: "runtime_observation".to_owned(),
    }
}

fn observed_version(content: &str) -> WriteMemoryVersion {
    WriteMemoryVersion {
        source_assets: Vec::new(),
        content: content.to_owned(),
        authority: SourceAuthority::RuntimeObserved,
        evidence: EvidenceState::VerifiedObservation,
        user_confirmed: false,
        source_event_refs: Vec::new(),
        source_file_hashes: Vec::new(),
        source_commit: Some("fixture-r2".to_owned()),
        provenance_kind: "runtime_observation".to_owned(),
        validity: Validity::Valid,
        supersedes: None,
        extractor_version: None,
        strategy_digest: None,
    }
}

async fn close_store(memory: MemoryService, store: Arc<SqliteStore>) {
    drop(memory);
    Arc::try_unwrap(store)
        .expect("all memory store consumers released")
        .close()
        .await
        .expect("memory writer closes");
}

/// The scope these cases extract at, except the one that is about scope itself.
const SESSION_SCOPE: ExtractionScope = ExtractionScope::Session;

fn extraction_strategy(label: &str) -> ExtractionStrategy {
    ExtractionStrategy {
        extractor_version: "mock-extractor-v1".to_owned(),
        strategy_digest: ContentHash::from_bytes(label.as_bytes()),
        replay_start_sequence: Some(1),
        asset_scope: SESSION_SCOPE,
    }
}

async fn seed_source_events(store: &Arc<SqliteStore>, count: u64) -> (SessionId, TaskId) {
    assert!(count > 0);
    let session_id = SessionId::generate();
    let task_id = TaskId::generate();
    let project_id = ProjectId::generate();
    let workspace = WorkspaceObservation {
        project_id,
        worktree_id: "memory-fixture".to_owned(),
        base_commit: "fixture-r1".to_owned(),
        observed_fingerprint: ContentHash::from_bytes(b"memory-fixture"),
    };
    let session = SessionService::new(Arc::clone(store));
    session
        .admit_input(AdmitInputRequest {
            session_id: session_id.clone(),
            task_id: task_id.clone(),
            input_id: InputId::generate(),
            expected_sequence: 1,
            authority: SourceAuthority::User,
            raw_text: "remember parser constraints".to_owned(),
            workspace,
            initial_plan_items: Vec::new(),
        })
        .await
        .expect("source input admitted");
    for index in 2..=count {
        let mut payload = Map::new();
        payload.insert(
            "observed_at".to_owned(),
            serde_json::json!("2026-09-11T00:00:00Z"),
        );
        payload.insert(
            "observation".to_owned(),
            serde_json::Value::String(format!("fact-{index}")),
        );
        session
            .append_runtime_event(&session_id, &task_id, "fixture.observation", payload, false)
            .await
            .expect("source event appended");
    }
    (session_id, task_id)
}

#[tokio::test]
async fn p4_s03_durable_extraction_jobs_settle_atomically() {
    let (_temp, store, memory) = memory_fixture().await;
    let (session_id, _) = seed_source_events(&store, 3).await;
    let strategy = extraction_strategy("strategy-a");
    let jobs = memory
        .schedule_backlog(&session_id, &strategy, 2)
        .await
        .expect("backlog scheduled from durable journal");
    assert_eq!(jobs.len(), 2);
    assert_eq!((jobs[0].start_sequence, jobs[0].end_sequence), (1, 2));
    assert_eq!((jobs[1].start_sequence, jobs[1].end_sequence), (3, 3));
    assert_ne!(jobs[0].source_digest, jobs[1].source_digest);

    let lease = memory
        .lease_job(&jobs[0].job_id, "worker-a")
        .await
        .expect("oldest range leased");
    assert_eq!(lease.source_events.len(), 2);
    assert_eq!(lease.job.status, ExtractionJobStatus::Leased);
    memory
        .settle_no_facts(&lease)
        .await
        .expect("no-facts is an explicit successful disposition");
    assert_eq!(
        memory
            .extraction_cursor(&session_id, &strategy)
            .await
            .unwrap(),
        2
    );
    let second = memory.lease_job(&jobs[1].job_id, "worker-a").await.unwrap();
    memory.settle_no_facts(&second).await.unwrap();
    assert_eq!(
        memory
            .extraction_cursor(&session_id, &strategy)
            .await
            .unwrap(),
        3
    );
    assert!(
        memory
            .list_jobs()
            .await
            .unwrap()
            .iter()
            .all(|job| job.status == ExtractionJobStatus::Completed)
    );
    close_store(memory, store).await;
}

#[tokio::test]
async fn p4_c12_crash_before_settlement_keeps_job_and_cursor_atomic() {
    let temp = TempDir::new().expect("temporary fault store");
    let fault_plan = StoreFaultPlan::with_point(StoreFaultPoint::BeforeMemorySettlementCommit);
    let store = Arc::new(
        SqliteStore::open_writer(
            WriterOpenOptions::new(temp.path(), HostId::generate()).with_fault_plan(fault_plan),
        )
        .await
        .expect("fault writer opens"),
    );
    let memory = MemoryService::new(Arc::clone(&store));
    let (session_id, _) = seed_source_events(&store, 1).await;
    let strategy = extraction_strategy("fault-strategy");
    let job = memory
        .schedule_backlog(&session_id, &strategy, 2)
        .await
        .unwrap()[0]
        .clone();
    let stale = memory
        .lease_job(&job.job_id, "worker-before-crash")
        .await
        .unwrap();
    let principal = MemoryPrincipal::user("host").with_session(session_id.clone());
    let error = memory
        .extract_lease(&principal, &stale, &FixtureExtractor, 4096, SESSION_SCOPE)
        .await
        .expect_err("injected crash window must roll back the complete transaction");
    assert_eq!(error.code(), ErrorCode::StorageWriteFailed);
    assert_eq!(
        memory_row_counts(&store).await,
        (0, 0, 0),
        "all asset/version/dependency writes rolled back"
    );
    assert_eq!(
        memory
            .extraction_cursor(&session_id, &strategy)
            .await
            .unwrap(),
        0
    );
    close_store(memory, store).await;

    let reopened = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(temp.path(), HostId::generate()))
            .await
            .expect("store reopens after simulated crash"),
    );
    let recovered = MemoryService::new(Arc::clone(&reopened));
    assert_eq!(recovered.recover_interrupted_jobs().await.unwrap(), 1);
    let fresh = recovered
        .lease_job(&job.job_id, "worker-after-crash")
        .await
        .unwrap();
    assert!(fresh.generation > stale.generation);
    assert_eq!(
        recovered.settle_no_facts(&stale).await.unwrap_err().code(),
        ErrorCode::StaleWriter
    );
    recovered
        .extract_lease(&principal, &fresh, &FixtureExtractor, 4096, SESSION_SCOPE)
        .await
        .unwrap();
    assert_eq!(memory_row_counts(&reopened).await, (1, 1, 1));
    assert_eq!(
        recovered
            .extract_lease(&principal, &fresh, &FixtureExtractor, 4096, SESSION_SCOPE)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::StaleWriter
    );
    assert_eq!(
        memory_row_counts(&reopened).await,
        (1, 1, 1),
        "duplicate settlement cannot duplicate facts"
    );
    assert_eq!(
        recovered
            .extraction_cursor(&session_id, &strategy)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        recovered
            .schedule_backlog(&session_id, &strategy, 2)
            .await
            .unwrap()
            .len(),
        0
    );
    close_store(recovered, reopened).await;
}

#[tokio::test]
async fn p4_c16_sequence_cursor_handles_same_timestamp_pages() {
    let (_temp, store, memory) = memory_fixture().await;
    let (session_id, _) = seed_source_events(&store, 5).await;
    let strategy = extraction_strategy("timestamp-independent");
    let jobs = memory
        .schedule_backlog(&session_id, &strategy, 2)
        .await
        .unwrap();
    assert_eq!(
        jobs.iter()
            .map(|job| (job.start_sequence, job.end_sequence))
            .collect::<Vec<_>>(),
        [(1, 2), (3, 4), (5, 5)]
    );
    for job in jobs {
        let lease = memory.lease_job(&job.job_id, "page-worker").await.unwrap();
        for event in lease.source_events.iter().filter(|event| event.seq > 1) {
            assert_eq!(event.payload["observed_at"], "2026-09-11T00:00:00Z");
        }
        memory.settle_no_facts(&lease).await.unwrap();
    }
    assert_eq!(
        memory
            .extraction_cursor(&session_id, &strategy)
            .await
            .unwrap(),
        5
    );
    close_store(memory, store).await;
}

#[tokio::test]
async fn p4_c17_out_of_order_range_cannot_advance_contiguous_cursor() {
    let (_temp, store, memory) = memory_fixture().await;
    let (session_id, _) = seed_source_events(&store, 4).await;
    let strategy = extraction_strategy("out-of-order");
    let jobs = memory
        .schedule_backlog(&session_id, &strategy, 2)
        .await
        .unwrap();
    let later = memory
        .lease_job(&jobs[1].job_id, "later-worker")
        .await
        .unwrap();
    let gap = memory
        .settle_no_facts(&later)
        .await
        .expect_err("later range cannot jump a cursor gap");
    assert_eq!(gap.code(), ErrorCode::SequenceConflict);
    assert_eq!(
        memory
            .extraction_cursor(&session_id, &strategy)
            .await
            .unwrap(),
        0
    );
    let earlier = memory
        .lease_job(&jobs[0].job_id, "earlier-worker")
        .await
        .unwrap();
    memory.settle_no_facts(&earlier).await.unwrap();
    memory.settle_no_facts(&later).await.unwrap();
    assert_eq!(
        memory
            .extraction_cursor(&session_id, &strategy)
            .await
            .unwrap(),
        4
    );
    close_store(memory, store).await;
}

#[tokio::test]
async fn p4_c22_missing_changed_or_malformed_extractor_does_not_advance() {
    let (_temp, store, memory) = memory_fixture().await;
    let (session_id, _) = seed_source_events(&store, 1).await;
    let principal = MemoryPrincipal::user("host").with_session(session_id.clone());
    let extractors: Vec<Box<dyn harness_memory::MemoryExtractor>> = vec![
        Box::new(FailingExtractor),
        Box::new(ScriptedExtractor { version: "changed-v2".to_owned(), reply: "{}".to_owned() }),
        Box::new(ScriptedExtractor { version: "mock-extractor-v1".to_owned(), reply: "{malformed".to_owned() }),
        Box::new(ScriptedExtractor { version: "mock-extractor-v1".to_owned(), reply: serde_json::json!({"candidates":[{"content":"forged", "source_event_refs":[harness_types::EventId::generate()]}]}).to_string() }),
    ];
    for (index, extractor) in extractors.into_iter().enumerate() {
        let strategy = extraction_strategy(&format!("failure-{index}"));
        let job = memory
            .schedule_backlog(&session_id, &strategy, 1)
            .await
            .unwrap()[0]
            .clone();
        let lease = memory
            .lease_job(&job.job_id, "failure-worker")
            .await
            .unwrap();
        assert!(
            memory
                .extract_lease(&principal, &lease, extractor.as_ref(), 4096, SESSION_SCOPE)
                .await
                .is_err()
        );
        assert_eq!(
            memory
                .extraction_cursor(&session_id, &strategy)
                .await
                .unwrap(),
            0
        );
        let stored = memory.list_jobs().await.unwrap();
        let failed = stored
            .iter()
            .find(|candidate| candidate.job_id == job.job_id)
            .unwrap();
        assert!(matches!(
            failed.status,
            ExtractionJobStatus::RetryWait | ExtractionJobStatus::Blocked
        ));
    }
    let mut changed = extraction_strategy("new-strategy-without-replay");
    changed.replay_start_sequence = None;
    assert_eq!(
        memory
            .schedule_backlog(&session_id, &changed, 1)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::InvalidPayload
    );
    close_store(memory, store).await;
}

#[tokio::test]
async fn p4_s02_scoped_assets_and_cas_writes_are_atomic() {
    let (_temp, store, memory) = memory_fixture().await;
    let project_id = ProjectId::generate();
    let owner = MemoryPrincipal::user("owner").with_project(project_id.clone());
    let first = memory
        .create_asset(&owner, project_asset(project_id, "parser uses CRLF"))
        .await
        .expect("owner creates scoped asset");
    assert_eq!(first.asset.current_version, 1);
    assert_eq!(first.current.content, "parser uses CRLF");

    let second = memory
        .write_version(
            &owner,
            &first.asset.memory_asset_id,
            1,
            observed_version("parser preserves CRLF and Unicode"),
        )
        .await
        .expect("matching expected version advances pointer");
    assert_eq!(second.asset.current_version, 2);
    let stale = memory
        .write_version(
            &owner,
            &first.asset.memory_asset_id,
            1,
            observed_version("stale overwrite"),
        )
        .await
        .expect_err("stale writer must not overwrite current version");
    assert_eq!(stale.code(), ErrorCode::SequenceConflict);
    let versions = memory
        .export_versions(&owner, &first.asset.memory_asset_id)
        .await
        .expect("owner export is authorized");
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].content, "parser uses CRLF");
    assert_eq!(versions[1].content, "parser preserves CRLF and Unicode");
    close_store(memory, store).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)]
async fn p4_c07_three_principals_publish_with_cas_no_lost_update() {
    let (_temp, store, memory) = memory_fixture().await;
    let memory = Arc::new(memory);
    let project_id = ProjectId::generate();
    let owner = MemoryPrincipal::user("coordinator").with_project(project_id.clone());
    let first = memory
        .create_asset(
            &owner,
            project_asset(project_id.clone(), "base observation"),
        )
        .await
        .expect("base asset");
    let asset_id = first.asset.memory_asset_id.clone();
    let actors = ["explorer", "coder", "verifier"];
    for actor in actors {
        memory
            .grant(
                &owner,
                MemoryGrant {
                    principal_id: actor.to_owned(),
                    memory_asset_id: Some(asset_id.clone()),
                    project_id: Some(project_id.clone()),
                    allowed_actions: BTreeSet::from([
                        MemoryAction::Read,
                        MemoryAction::Publish,
                        MemoryAction::Export,
                    ]),
                    revision: 1,
                    active: true,
                },
            )
            .await
            .expect("owner grants exact actor");
    }
    let mut joins = Vec::new();
    for actor in actors {
        let service = Arc::clone(&memory);
        let id = asset_id.clone();
        let principal = MemoryPrincipal::user(actor).with_project(project_id.clone());
        joins.push(tokio::spawn(async move {
            service
                .write_version(
                    &principal,
                    &id,
                    1,
                    observed_version(&format!("{actor} proposal")),
                )
                .await
                .map(|asset| (actor, asset.asset.current_version))
        }));
    }
    let mut winners = 0;
    let mut conflicts = 0;
    for join in joins {
        match join.await.expect("publisher joins") {
            Ok((_, 2)) => winners += 1,
            Err(error) if error.code() == ErrorCode::SequenceConflict => conflicts += 1,
            result => panic!("unexpected concurrent result: {result:?}"),
        }
    }
    assert_eq!((winners, conflicts), (1, 2));

    for actor in actors {
        let principal = MemoryPrincipal::user(actor).with_project(project_id.clone());
        let current = memory
            .read(&owner, &asset_id)
            .await
            .expect("owner read")
            .expect("asset exists")
            .asset
            .current_version;
        if memory
            .export_versions(&owner, &asset_id)
            .await
            .expect("owner export")
            .iter()
            .any(|version| version.content == format!("{actor} proposal"))
        {
            continue;
        }
        memory
            .write_version(
                &principal,
                &asset_id,
                current,
                observed_version(&format!("{actor} proposal")),
            )
            .await
            .expect("loser rebases through explicit CAS");
    }
    let versions = memory
        .export_versions(&owner, &asset_id)
        .await
        .expect("all immutable proposals inspectable");
    assert_eq!(versions.len(), 4);
    for actor in actors {
        assert!(
            versions
                .iter()
                .any(|version| version.content == format!("{actor} proposal")),
            "proposal from {actor} was lost"
        );
    }
    let memory = Arc::try_unwrap(memory).expect("memory tasks released");
    close_store(memory, store).await;
}

#[tokio::test]
async fn p4_c08_forged_scope_cannot_search_read_or_export() {
    let (_temp, store, memory) = memory_fixture().await;
    let owner_project = ProjectId::generate();
    let other_project = ProjectId::generate();
    let owner = MemoryPrincipal::user("owner").with_project(owner_project.clone());
    let forged_owner_scope = MemoryPrincipal::user("owner").with_project(other_project.clone());
    let asset = memory
        .create_asset(
            &owner,
            project_asset(owner_project.clone(), "private build command"),
        )
        .await
        .expect("private project asset");
    let attacker = MemoryPrincipal::user("same-account-worker").with_project(other_project);
    assert_eq!(
        memory
            .read(&forged_owner_scope, &asset.asset.memory_asset_id)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::PolicyDenied
    );
    let read = memory
        .read(&attacker, &asset.asset.memory_asset_id)
        .await
        .expect_err("direct read must distinguish authorization failure");
    assert_eq!(read.code(), ErrorCode::PolicyDenied);
    let export = memory
        .export_versions(&attacker, &asset.asset.memory_asset_id)
        .await
        .expect_err("export must enforce object authorization independently");
    assert_eq!(export.code(), ErrorCode::PolicyDenied);
    assert!(
        memory
            .search(&attacker, "build", 1, None)
            .await
            .unwrap()
            .hits
            .is_empty()
    );
    let same_project =
        MemoryPrincipal::user("forged-owner-role").with_project(owner_project.clone());
    assert!(
        memory
            .search(&same_project, "build", 1, None)
            .await
            .unwrap()
            .hits
            .is_empty()
    );
    assert_eq!(
        memory
            .read(&same_project, &asset.asset.memory_asset_id)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::PolicyDenied
    );
    let visible = memory
        .create_asset(
            &same_project,
            project_asset(owner_project, "build visible scoped observation"),
        )
        .await
        .unwrap();
    let hits = memory
        .search(&same_project, "build", 1, None)
        .await
        .unwrap()
        .hits;
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].asset.memory_asset_id, visible.asset.memory_asset_id,
        "authorization must precede top-k"
    );
    close_store(memory, store).await;
}
