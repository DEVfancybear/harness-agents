//! M7 acceptance target: memory asset provenance, the durable extraction
//! consumer, retrieval freshness and the CLI surface over both.
//!
//! Everything here runs against **real** components: the real `SQLite` store with
//! its real memory schema, the real `MemoryService` and its transactions, and the
//! real `ha` binary for the CLI cases. The only thing mocked is the extractor,
//! which is an external inference boundary by contract
//! (`MemoryExtractor`); the store, the job table, the cursor and the settlement
//! are the ones the product uses.
//!
//! The three due acceptance cases live here:
//!
//! * `a19_optional_services_failure` — the M7 half: a committed range whose
//!   extractor is disabled stays queued, the task resumes without waiting for
//!   extraction, a bounded catch-up settles it, and the journal remains readable
//!   with no memory service at all.
//! * `a25_memory_job_cas` — two consumers racing one range produce one logical
//!   settlement, a crash before settlement leaves no asset and no cursor move, a
//!   stale lease cannot commit, and a filtered range still gets a disposition so
//!   the cursor cannot stall.
//! * `a26_memory_provenance` — a stale file source, a user correction and a
//!   revoked asset are all excluded from the next packet, transitive invalidation
//!   reaches a summary, and injected memory repeated back is not stored as new
//!   evidence.

use std::{future::Future, pin::Pin, process::Output, sync::Arc};

use harness_memory::{
    CreateMemoryAsset, EvidenceState, ExtractedCandidate, ExtractionJobStatus, ExtractionOutput,
    ExtractionScope, ExtractionStrategy, MAX_QUERY_BYTES, MemoryBudget, MemoryExtractor,
    MemoryLayer, MemoryPrincipal, MemoryService, MemorySource, SourceProjection,
};
use harness_session::{AdmitInputRequest, SessionService};
use harness_store_sqlite::{
    MemorySourceKind, RefreshSource, SqliteStore, StoreMemoryPrincipal, WriterOpenOptions,
};
use harness_types::{
    ContentHash, ErrorCode, HostId, InputId, MemoryAssetStatus, ProjectId, SessionId,
    SourceAuthority, TaskId, WorkspaceObservation,
};
use serde_json::json;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

struct Fixture {
    temp: TempDir,
    store: Arc<SqliteStore>,
    memory: MemoryService,
    session_id: SessionId,
    task_id: TaskId,
    project_id: ProjectId,
}

const STRATEGY_LABEL: &str = "m7-extractor-v1";

/// The extraction strategy the fixture uses. The asset scope is part of it on
/// purpose: the same stream extracted at another scope is another generation of
/// work with its own cursor.
fn strategy(scope: ExtractionScope) -> ExtractionStrategy {
    ExtractionStrategy {
        extractor_version: STRATEGY_LABEL.to_owned(),
        strategy_digest: ContentHash::from_bytes(format!("{STRATEGY_LABEL}#{scope:?}").as_bytes()),
        replay_start_sequence: None,
        asset_scope: scope,
    }
}

/// A principal in a project this fixture never writes to.
fn other_store_principal() -> StoreMemoryPrincipal {
    StoreMemoryPrincipal {
        principal_id: "m7-fixture".to_owned(),
        project_id: Some(ProjectId::generate()),
        task_id: None,
        agent_profile_id: None,
        session_id: None,
    }
}

async fn open_fixture() -> Fixture {
    let temp = TempDir::new().expect("temporary store");
    let store = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(temp.path(), HostId::generate()))
            .await
            .expect("writer opens"),
    );
    Fixture {
        temp,
        memory: MemoryService::new(Arc::clone(&store)),
        store,
        session_id: SessionId::generate(),
        task_id: TaskId::generate(),
        project_id: ProjectId::generate(),
    }
}

impl Fixture {
    fn principal(&self) -> MemoryPrincipal {
        MemoryPrincipal::user("m7-fixture")
            .with_project(self.project_id.clone())
            .with_task(self.task_id.clone())
            .with_session(self.session_id.clone())
    }

    /// The same principal as the store sees it, for a store-level call.
    fn store_principal(&self) -> StoreMemoryPrincipal {
        let principal = self.principal();
        StoreMemoryPrincipal {
            principal_id: principal.principal_id,
            project_id: principal.project_id,
            task_id: principal.task_id,
            agent_profile_id: principal.agent_profile_id,
            session_id: principal.session_id,
        }
    }

    /// Commit `count` admitted inputs through the real session service.
    ///
    /// The markers these write are what the extraction backlog is derived from,
    /// so the fixture goes through the same transaction the product does rather
    /// than inserting job rows directly.
    async fn admit(&self, count: u64) -> Vec<String> {
        self.admit_range(1, count).await
    }

    /// Admit one input with exactly this text, at the next sequence.
    async fn admit_text(&self, text: &str) {
        self.admit_one(text).await;
    }

    async fn admit_one(&self, text: &str) -> String {
        let service = SessionService::new(Arc::clone(&self.store));
        let sequence = self
            .store
            .next_sequence(&self.session_id)
            .await
            .unwrap_or(1);
        service
            .admit_input(AdmitInputRequest {
                session_id: self.session_id.clone(),
                task_id: self.task_id.clone(),
                input_id: InputId::generate(),
                expected_sequence: sequence,
                authority: SourceAuthority::User,
                raw_text: text.to_owned(),
                workspace: WorkspaceObservation {
                    project_id: self.project_id.clone(),
                    worktree_id: "m7".to_owned(),
                    base_commit: "0".repeat(40),
                    observed_fingerprint: ContentHash::from_bytes(b"m7-workspace"),
                },
                initial_plan_items: Vec::new(),
            })
            .await
            .expect("input is admitted");
        text.to_owned()
    }

    async fn admit_range(&self, first: u64, count: u64) -> Vec<String> {
        let service = SessionService::new(Arc::clone(&self.store));
        let mut texts = Vec::new();
        let mut sequence = self
            .store
            .next_sequence(&self.session_id)
            .await
            .unwrap_or(1);
        for index in first..first.saturating_add(count) {
            let text = format!("range item {index}");
            service
                .admit_input(AdmitInputRequest {
                    session_id: self.session_id.clone(),
                    task_id: self.task_id.clone(),
                    input_id: InputId::generate(),
                    expected_sequence: sequence,
                    authority: SourceAuthority::User,
                    raw_text: text.clone(),
                    workspace: WorkspaceObservation {
                        project_id: self.project_id.clone(),
                        worktree_id: "m7".to_owned(),
                        base_commit: "0".repeat(40),
                        observed_fingerprint: ContentHash::from_bytes(b"m7-workspace"),
                    },
                    initial_plan_items: Vec::new(),
                })
                .await
                .expect("input is admitted");
            sequence += 1;
            texts.push(text);
        }
        texts
    }

    fn observation_asset(&self, content: &str, sources: Vec<MemorySource>) -> CreateMemoryAsset {
        // A verified observation has to name what it was observed from, and the
        // keyed source is exactly that reference. A fixture that asked for
        // `VerifiedObservation` with nothing behind it would be writing the false
        // claim the validator exists to refuse, so the commit is named here.
        let sources = if sources.is_empty() {
            vec![MemorySource::commit("m7-fixture-r1")]
        } else {
            sources
        };
        CreateMemoryAsset {
            kind: "project_fact".to_owned(),
            scope: harness_types::MemoryScope::Project,
            layer: MemoryLayer::L1,
            project_id: Some(self.project_id.clone()),
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
            source_commit: None,
            provenance_kind: "runtime_observation".to_owned(),
            sources,
        }
    }
}

/// A deterministic extractor that echoes one candidate per eligible source.
struct EchoExtractor {
    version: String,
}

impl MemoryExtractor for EchoExtractor {
    fn version(&self) -> &str {
        &self.version
    }

    fn extract<'a>(
        &'a self,
        sources: &'a [SourceProjection],
    ) -> Pin<Box<dyn Future<Output = Result<String, harness_types::HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            serde_json::to_string(&ExtractionOutput {
                candidates: sources
                    .iter()
                    .map(|source| ExtractedCandidate {
                        content: format!("fact from seq {}: {}", source.sequence, source.content),
                        source_event_refs: vec![source.event_id.clone()],
                    })
                    .collect(),
            })
            .map_err(|_| {
                harness_types::HarnessError::new(
                    ErrorCode::InvalidPayload,
                    "fixture output is not serializable",
                )
            })
        })
    }
}

/// An extractor that is configured but refuses to run, like a disabled service.
struct DisabledExtractor;

impl MemoryExtractor for DisabledExtractor {
    fn version(&self) -> &'static str {
        STRATEGY_LABEL
    }

    fn extract<'a>(
        &'a self,
        _sources: &'a [SourceProjection],
    ) -> Pin<Box<dyn Future<Output = Result<String, harness_types::HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            Err(harness_types::HarnessError::new(
                ErrorCode::ServiceUnavailable,
                "optional extractor disabled",
            ))
        })
    }
}

/// An extractor that proposes exactly the text it was given, so a re-run of the
/// same range quotes memory back at the store.
struct QuoteBackExtractor {
    version: String,
}

impl MemoryExtractor for QuoteBackExtractor {
    fn version(&self) -> &str {
        &self.version
    }

    fn extract<'a>(
        &'a self,
        sources: &'a [SourceProjection],
    ) -> Pin<Box<dyn Future<Output = Result<String, harness_types::HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            serde_json::to_string(&ExtractionOutput {
                candidates: sources
                    .iter()
                    .map(|source| ExtractedCandidate {
                        // Verbatim: this is what a model quoting an injected block
                        // produces, and it must not become a second asset.
                        content: source.content.clone(),
                        source_event_refs: vec![source.event_id.clone()],
                    })
                    .collect(),
            })
            .map_err(|_| {
                harness_types::HarnessError::new(
                    ErrorCode::InvalidPayload,
                    "fixture output is not serializable",
                )
            })
        })
    }
}

fn echo() -> EchoExtractor {
    EchoExtractor {
        version: STRATEGY_LABEL.to_owned(),
    }
}

fn quote_back() -> QuoteBackExtractor {
    QuoteBackExtractor {
        version: STRATEGY_LABEL.to_owned(),
    }
}

/// Settle every outstanding job for a strategy, one lease at a time.
///
/// Kept as the fixture's "drain the backlog" helper so a test that cares about the
/// end state does not repeat the lease/settle dance; the tests that care about the
/// individual steps call those steps themselves.
#[allow(dead_code)]
async fn drain(fixture: &Fixture, strategy: &ExtractionStrategy, extractor: &dyn MemoryExtractor) {
    let report = fixture
        .memory
        .reconcile(&fixture.session_id, strategy, 16)
        .await
        .expect("reconcile runs");
    for job in report.outstanding {
        let lease = fixture
            .memory
            .lease_job(&job.job_id, "m7-fixture")
            .await
            .expect("job leases");
        let _ = fixture
            .memory
            .extract_lease(
                &fixture.principal(),
                &lease,
                extractor,
                16_384,
                strategy.asset_scope,
            )
            .await;
    }
}

// ---------------------------------------------------------------------------
// M7-02 — the durable extraction consumer
// ---------------------------------------------------------------------------

#[tokio::test]
async fn m7_02_committed_ranges_are_enqueued_and_deduplicated() {
    let fixture = open_fixture().await;
    let strategy = strategy(ExtractionScope::Project);

    // Nothing is committed yet, so there is nothing to enqueue.
    let empty = fixture
        .memory
        .reconcile(&fixture.session_id, &strategy, 16)
        .await
        .expect("reconcile runs on an empty stream");
    assert_eq!(empty.enqueued, 0);
    assert_eq!(empty.cursor, 0);
    assert!(empty.outstanding.is_empty());

    fixture.admit(4).await;
    let first = fixture
        .memory
        .reconcile(&fixture.session_id, &strategy, 16)
        .await
        .expect("reconcile runs");
    assert_eq!(
        first.enqueued, 1,
        "the four committed markers are one contiguous range"
    );
    assert_eq!(first.outstanding[0].start_sequence, 1);
    assert_eq!(
        first.outstanding[0].end_sequence, 4,
        "the job covers exactly the committed range"
    );

    // Another admission above the cursor is a second range, batched at the bound.
    fixture.admit_range(5, 2).await;
    let second = fixture
        .memory
        .reconcile(&fixture.session_id, &strategy, 2)
        .await
        .expect("reconcile runs");
    assert_eq!(
        second.enqueued, 1,
        "the batch bound splits a longer range into one job per batch"
    );
    assert_eq!(
        second.outstanding.len(),
        2,
        "both ranges are outstanding: the settled one has not been settled"
    );

    // Settling in order advances the cursor; the second range stays outstanding.
    let first_job = second.outstanding[0].job_id.clone();
    let lease = fixture
        .memory
        .lease_job(&first_job, "worker-a")
        .await
        .expect("lease");
    fixture
        .memory
        .extract_lease(
            &fixture.principal(),
            &lease,
            &echo(),
            16_384,
            strategy.asset_scope,
        )
        .await
        .expect("settlement");
    assert_eq!(
        fixture
            .memory
            .extraction_cursor(&fixture.session_id, &strategy)
            .await
            .expect("cursor is readable"),
        4,
        "the cursor covers settled ranges only"
    );
    let after = fixture
        .memory
        .reconcile(&fixture.session_id, &strategy, 16)
        .await
        .expect("reconcile runs");
    assert_eq!(
        after.enqueued, 0,
        "the second range was already enqueued, so a third reconcile adds nothing"
    );
    assert_eq!(
        after.outstanding.len(),
        1,
        "one range was settled and exactly one is still outstanding"
    );
    assert_eq!(
        after.outstanding[0].start_sequence, 5,
        "the outstanding range is the one that was never settled"
    );
    assert_eq!(after.outstanding[0].end_sequence, 6);
}

#[tokio::test]
async fn m7_02_backlog_survives_reopen_and_schedules_are_stable() {
    let fixture = open_fixture().await;
    let strategy = strategy(ExtractionScope::Session);
    fixture.admit(3).await;
    let opened = fixture
        .memory
        .reconcile(&fixture.session_id, &strategy, 16)
        .await
        .expect("reconcile runs");
    assert_eq!(opened.enqueued, 1);
    let job_id = opened.outstanding[0].job_id.clone();
    let data_dir = fixture.temp.path().to_path_buf();
    let session_id = fixture.session_id.clone();
    drop(fixture);

    // The queue is in the store, not in the process: a new host sees the same job
    // and does not create a second one for the same range.
    let store = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(&data_dir, HostId::generate()))
            .await
            .expect("store reopens"),
    );
    let memory = MemoryService::new(Arc::clone(&store));
    let reopened = memory
        .reconcile(&session_id, &strategy, 16)
        .await
        .expect("reconcile runs after reopen");
    assert_eq!(reopened.enqueued, 0, "no duplicate job for a settled range");
    assert_eq!(reopened.outstanding.len(), 1);
    assert_eq!(reopened.outstanding[0].job_id, job_id);
    drop(memory);
    Arc::try_unwrap(store)
        .expect("all consumers released the store")
        .close()
        .await
        .expect("store closes");
}

// ---------------------------------------------------------------------------
// M7-01 — assets and authority
// ---------------------------------------------------------------------------

#[tokio::test]
async fn m7_01_source_dependency_is_queryable_and_scoped() {
    let fixture = open_fixture().await;
    let principal = fixture.principal();

    let path = "src/lib.rs";
    let observed = ContentHash::from_bytes(b"revision one");
    let asset = fixture
        .memory
        .create_asset(
            &principal,
            fixture.observation_asset(
                "the parser lives in src/lib.rs",
                vec![MemorySource::file(path, observed.clone())],
            ),
        )
        .await
        .expect("asset is created");

    let sources = fixture
        .memory
        .version_sources(&principal, &asset.asset.memory_asset_id, 1)
        .await
        .expect("sources are readable");
    assert_eq!(
        sources.len(),
        1,
        "the keyed source is recorded with the version"
    );
    assert_eq!(sources[0].kind, MemorySourceKind::File);
    assert_eq!(sources[0].id, path);
    assert_eq!(
        sources[0].observed_digest.as_ref(),
        Some(&observed),
        "the digest recorded is the one observed at write time"
    );

    // The same relative path in another project is another source: a stale file in
    // one project must not retire knowledge in the other.
    let moved = RefreshSource {
        kind: MemorySourceKind::File,
        id: path.to_owned(),
        observed: ContentHash::from_bytes(b"revision two"),
    };
    let mine = fixture
        .store
        .memory_versions_with_changed_sources(
            &fixture.store_principal(),
            std::slice::from_ref(&moved),
        )
        .await
        .expect("stale lookup runs");
    assert_eq!(
        mine,
        vec![asset.asset.memory_asset_id.clone()],
        "the file the asset was read from is reported as moved"
    );
    let theirs = fixture
        .store
        .memory_versions_with_changed_sources(&other_store_principal(), &[moved])
        .await
        .expect("scoped stale lookup runs");
    assert!(
        theirs.is_empty(),
        "another project's principal cannot see, let alone retire, this asset"
    );

    // A source that still hashes the same is not a change.
    let unchanged = RefreshSource {
        kind: MemorySourceKind::File,
        id: path.to_owned(),
        observed,
    };
    assert!(
        fixture
            .store
            .memory_versions_with_changed_sources(&fixture.store_principal(), &[unchanged])
            .await
            .expect("unchanged lookup runs")
            .is_empty(),
        "re-reading a file that did not move is not a source change"
    );
}

#[tokio::test]
async fn m7_01_version_cas_and_content_identity() {
    let fixture = open_fixture().await;
    let principal = fixture.principal();
    let asset = fixture
        .memory
        .create_asset(
            &principal,
            fixture.observation_asset("the first fact", Vec::new()),
        )
        .await
        .expect("asset is created");

    // A proposal on a stale read is refused rather than overwriting the version
    // someone else published.
    let stale = fixture
        .memory
        .propose(
            &principal,
            Some((&asset.asset.memory_asset_id, 7)),
            fixture.observation_asset("a proposal built on a stale read", Vec::new()),
        )
        .await;
    assert_eq!(
        stale.expect_err("a stale proposal is refused").code(),
        ErrorCode::SequenceConflict
    );

    // The real CAS: two writers at the same expected version, one wins.
    let first = fixture
        .memory
        .propose(
            &principal,
            Some((&asset.asset.memory_asset_id, 1)),
            fixture.observation_asset("the second fact", Vec::new()),
        )
        .await
        .expect("the first writer advances the version");
    assert_eq!(first.asset.current_version, 2);
    let second = fixture
        .memory
        .propose(
            &principal,
            Some((&asset.asset.memory_asset_id, 1)),
            fixture.observation_asset("a racing second fact", Vec::new()),
        )
        .await;
    assert_eq!(
        second.expect_err("the loser is refused").code(),
        ErrorCode::SequenceConflict,
        "one logical settlement: the version pointer moves once"
    );

    // Content identity is exact, not a ranked match.
    let identity = fixture
        .memory
        .find_active_by_content(&principal, "the second fact")
        .await
        .expect("identity lookup runs");
    assert_eq!(identity, Some(asset.asset.memory_asset_id.clone()));
    assert_eq!(
        fixture
            .memory
            .find_active_by_content(&principal, "the second")
            .await
            .expect("partial lookup runs"),
        None,
        "a near miss is not the same bytes"
    );

    // A correction is a new version, and the version it replaced is still there.
    let corrected = fixture
        .memory
        .confirm_version(
            &principal,
            &asset.asset.memory_asset_id,
            2,
            "host_confirmation",
        )
        .await
        .expect("confirmation writes a new version");
    assert_eq!(corrected.asset.current_version, 3);
    assert_eq!(corrected.current.record.supersedes, Some(2));
    let versions = fixture
        .memory
        .export_versions(&principal, &asset.asset.memory_asset_id)
        .await
        .expect("history is readable");
    assert_eq!(versions.len(), 3, "no version was overwritten");
    assert_eq!(versions[0].content, "the first fact");
    assert_eq!(versions[1].content, "the second fact");
}

// ---------------------------------------------------------------------------
// A19 — summary/extractor unavailable (M7 half)
// ---------------------------------------------------------------------------

#[tokio::test]
#[allow(clippy::too_many_lines)] // one extractor outage, told from enqueue to catch-up
async fn a19_optional_services_failure() {
    let fixture = open_fixture().await;
    let principal = fixture.principal();
    let strategy = strategy(ExtractionScope::Project);
    fixture.admit(2).await;

    // A disabled extractor must not consume the backlog: the range stays queued
    // so a later catch-up can still settle it.
    let cancellation = harness_providers::CancellationToken::new();
    let first = fixture
        .memory
        .catch_up(
            &principal,
            &strategy,
            &DisabledExtractor,
            &MemoryBudget::calls(4),
            &cancellation,
        )
        .await
        .expect("catch-up reports a refused extractor instead of failing");
    assert_eq!(first.enqueued, 1, "the committed range was enqueued");
    assert_eq!(
        first.failed, 1,
        "the disabled extractor is a failed attempt"
    );

    let jobs = fixture
        .memory
        .list_jobs_for(&fixture.session_id)
        .await
        .expect("jobs are listed");
    assert_eq!(jobs.len(), 1, "one job for one committed range");
    assert_eq!(
        jobs[0].status,
        ExtractionJobStatus::Blocked,
        "a disabled extractor blocks its range instead of dropping it"
    );
    let cursor = fixture
        .memory
        .extraction_cursor(&fixture.session_id, &strategy)
        .await
        .expect("cursor is readable");
    assert_eq!(cursor, 0, "a blocked range does not advance the cursor");

    // Resume does not wait for extraction: the journal is readable on its own,
    // through the store the runtime reads, with no memory service constructed at
    // all. `SessionService::recover` is deliberately not used here - it rebuilds
    // the *runtime* view, which requires exactly one admitted input, and this
    // fixture admits two to make one range with more than one event.
    let summary = fixture
        .store
        .session_summary(&fixture.session_id)
        .await
        .expect("the session summary is readable")
        .expect("the session exists");
    assert_eq!(
        summary.input_count, 2,
        "both admitted inputs are durable in the inbox"
    );
    assert_eq!(
        summary.next_sequence, 3,
        "the journal advanced past both admissions"
    );
    let tail = fixture
        .store
        .load_events_after(&fixture.session_id, 0)
        .await
        .expect("the journal tail is readable");
    assert_eq!(tail.len(), 2, "both events are in the journal");
    let admitted = tail
        .iter()
        .filter_map(|event| event.payload.get("text").and_then(|text| text.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        admitted,
        vec!["range item 1", "range item 2"],
        "the journal is the recovery source, not memory, and it is in order"
    );

    // A bounded catch-up with a working extractor settles what the disabled one
    let before = fixture
        .memory
        .reconcile(&fixture.session_id, &strategy, 16)
        .await
        .expect("reconcile runs");
    assert_eq!(
        before.outstanding.len(),
        1,
        "the blocked range is still on the queue: {before:?}"
    );
    assert_eq!(
        before.outstanding[0].status,
        ExtractionJobStatus::Blocked,
        "a disabled extractor leaves the range blocked, which is a state catch-up retries: {before:?}"
    );
    // left behind, in order, and advances the cursor only as far as it settled.
    let second = fixture
        .memory
        .catch_up(
            &principal,
            &strategy,
            &echo(),
            &MemoryBudget::calls(4),
            &cancellation,
        )
        .await
        .expect("catch-up runs");
    assert_eq!(
        second.completed, 1,
        "the blocked range is retried: {second:?}"
    );
    assert_eq!(
        fixture
            .memory
            .extraction_cursor(&fixture.session_id, &strategy)
            .await
            .expect("cursor is readable"),
        2,
        "the cursor covers exactly the settled range"
    );
    let settled = fixture
        .memory
        .list_jobs_for(&fixture.session_id)
        .await
        .expect("jobs are listed");
    assert_eq!(settled[0].status, ExtractionJobStatus::Completed);
    assert_eq!(settled[0].disposition.as_deref(), Some("candidates"));
}

// ---------------------------------------------------------------------------
// A25 — memory publication and durable jobs
// ---------------------------------------------------------------------------

#[tokio::test]
#[allow(clippy::too_many_lines)] // one range's whole settlement story, told in order
async fn a25_memory_job_cas() {
    let fixture = open_fixture().await;
    let run_strategy = strategy(ExtractionScope::Session);
    fixture.admit(3).await;

    // Two reconciles over the same committed range: one job, not two.
    let first = fixture
        .memory
        .reconcile(&fixture.session_id, &run_strategy, 16)
        .await
        .expect("first reconcile runs");
    assert_eq!(first.enqueued, 1);
    assert_eq!(first.outstanding.len(), 1);
    let second = fixture
        .memory
        .reconcile(&fixture.session_id, &run_strategy, 16)
        .await
        .expect("second reconcile runs");
    assert_eq!(second.enqueued, 0, "replay creates no duplicate job");
    assert_eq!(
        second.outstanding.len(),
        1,
        "the outstanding range is reported, not re-enqueued"
    );
    assert_eq!(
        fixture
            .memory
            .list_jobs_for(&fixture.session_id)
            .await
            .expect("jobs are listed")
            .len(),
        1
    );

    // A second consumer cannot claim a live lease.
    let job_id = second.outstanding[0].job_id.clone();
    let stale = fixture
        .memory
        .lease_job(&job_id, "worker-a")
        .await
        .expect("first lease");
    let fresh = fixture.memory.lease_job(&job_id, "worker-b").await;
    assert_eq!(
        fresh
            .expect_err("a leased job is not leaseable again")
            .code(),
        ErrorCode::RuntimeCommandConflict,
        "a second consumer cannot claim a live lease"
    );

    // The range is still exactly where it was: no asset, no cursor move.
    assert_eq!(
        fixture
            .memory
            .extraction_cursor(&fixture.session_id, &run_strategy)
            .await
            .expect("cursor is readable"),
        0
    );
    assert!(
        fixture
            .memory
            .list_jobs_for(&fixture.session_id)
            .await
            .expect("jobs are listed")[0]
            .disposition
            .is_none(),
        "a job that never settled has no disposition to report"
    );

    // A crash between the lease and the settlement: the process is gone with the
    // lease in hand. The next host recovers the interrupted job and leases it with
    // a new generation, and the old lease can no longer commit anything - which is
    // the generation check, not a time-based guess.
    let data_dir = fixture.temp.path().to_path_buf();
    let session_id = fixture.session_id.clone();
    let task_id = fixture.task_id.clone();
    let project_id = fixture.project_id.clone();
    drop(fixture);
    let reopened = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(&data_dir, HostId::generate()))
            .await
            .expect("the store reopens after the crash"),
    );
    let memory = MemoryService::new(Arc::clone(&reopened));
    let recovered = memory
        .recover_interrupted_jobs()
        .await
        .expect("recovery runs");
    assert_eq!(recovered, 1, "the interrupted lease is released");
    let successor = memory
        .lease_job(&job_id, "worker-b")
        .await
        .expect("the new host leases the recovered job");
    assert!(
        successor.generation > stale.generation,
        "the successor holds a newer generation ({} > {})",
        successor.generation,
        stale.generation
    );
    let refused = memory
        .extract_lease(
            &MemoryPrincipal::user("m7-fixture")
                .with_project(project_id.clone())
                .with_task(task_id.clone())
                .with_session(session_id.clone()),
            &stale,
            &echo(),
            16_384,
            run_strategy.asset_scope,
        )
        .await;
    assert_eq!(
        refused
            .expect_err("a superseded lease cannot settle")
            .code(),
        ErrorCode::StaleWriter,
        "the generation check is what rejects the old consumer"
    );

    // Settle through the live lease: candidates and the cursor commit together.
    let principal = MemoryPrincipal::user("m7-fixture")
        .with_project(project_id.clone())
        .with_task(task_id.clone())
        .with_session(session_id.clone());
    memory
        .extract_lease(
            &principal,
            &successor,
            &echo(),
            16_384,
            run_strategy.asset_scope,
        )
        .await
        .expect("the live lease settles");
    let jobs = memory
        .list_jobs_for(&session_id)
        .await
        .expect("jobs are listed");
    assert_eq!(jobs[0].status, ExtractionJobStatus::Completed);
    assert_eq!(jobs[0].disposition.as_deref(), Some("candidates"));
    assert_eq!(
        memory
            .extraction_cursor(&session_id, &run_strategy)
            .await
            .expect("cursor is readable"),
        3,
        "the cursor is gap-free across the whole committed range"
    );
    let published = memory
        .list_candidates(&principal, 8)
        .await
        .expect("settled candidates are listed");
    assert!(
        !published.is_empty(),
        "the settled range published candidates a human can confirm"
    );
    assert!(
        published
            .iter()
            .all(|candidate| candidate.asset.status == MemoryAssetStatus::Candidate),
        "extraction settles candidates, never published memory"
    );
    drop(memory);
    Arc::try_unwrap(reopened)
        .expect("all consumers released the store")
        .close()
        .await
        .expect("the store closes");

    // A range that projects no eligible source still gets a disposition, so the
    // cursor crosses it instead of stalling on it forever.
    let second = open_fixture().await;
    let tail_strategy = strategy(ExtractionScope::Session);
    second.admit(1).await;
    let report = second
        .memory
        .reconcile(&second.session_id, &tail_strategy, 16)
        .await
        .expect("reconcile runs");
    let lease = second
        .memory
        .lease_job(&report.outstanding[0].job_id, "worker-a")
        .await
        .expect("lease");
    // Settle the range as one whose sources the extractor declined to read.
    second
        .memory
        .settle_filtered(&lease)
        .await
        .expect("filtered settlement runs");
    let jobs = second
        .memory
        .list_jobs_for(&second.session_id)
        .await
        .expect("jobs are listed");
    assert_eq!(jobs[0].disposition.as_deref(), Some("filtered"));
    assert_eq!(
        second
            .memory
            .extraction_cursor(&second.session_id, &tail_strategy)
            .await
            .expect("cursor is readable"),
        1,
        "a filtered range is covered rather than skipped"
    );
}

// ---------------------------------------------------------------------------
// A26 — memory staleness and self-reinforcement
// ---------------------------------------------------------------------------

#[tokio::test]
#[allow(clippy::too_many_lines)] // staleness, correction and revocation in one walk
async fn a26_memory_provenance() {
    let fixture = open_fixture().await;
    let principal = fixture.principal();
    let path = "notes/architecture.md";
    let observed = ContentHash::from_bytes(b"the cache is disabled");

    let source_backed = fixture
        .memory
        .create_asset(
            &principal,
            fixture.observation_asset(
                "the cache is disabled by configuration",
                vec![MemorySource::file(path, observed.clone())],
            ),
        )
        .await
        .expect("source-backed fact is created");
    let id = source_backed.asset.memory_asset_id.clone();
    assert_eq!(source_backed.asset.status, MemoryAssetStatus::Active);

    let found = fixture
        .memory
        .search(&principal, "cache disabled configuration", 8, None)
        .await
        .expect("search runs");
    assert_eq!(
        found.hits.len(),
        1,
        "a fresh source-backed fact is retrievable"
    );
    let packet = fixture.memory.contribute(&principal, &found, 800);
    assert_eq!(packet.stamps.len(), packet.blocks.len());
    assert_eq!(packet.stamps[0].version, 1);
    assert_eq!(
        packet.stamps[0].source_digests[0].id, path,
        "the packet names the source the fact was read from"
    );
    assert_eq!(
        packet.stamps[0].source_digests[0].observed_digest,
        Some(observed.clone())
    );

    // The source moved. Retrieval with the caller's re-read excludes the version
    // *before* ranking, and the asset is untouched until someone invalidates it.
    let moved = RefreshSource {
        kind: MemorySourceKind::File,
        id: path.to_owned(),
        observed: ContentHash::from_bytes(b"the cache is enabled"),
    };
    let stale = fixture
        .memory
        .search_terms_fresh(
            &principal,
            &[
                "cache".to_owned(),
                "disabled".to_owned(),
                "configuration".to_owned(),
            ],
            8,
            None,
            std::slice::from_ref(&moved),
        )
        .await
        .expect("freshness-filtered search runs");
    assert!(
        stale.hits.is_empty(),
        "a version whose source moved is not selected"
    );
    assert_eq!(
        fixture
            .memory
            .read(&principal, &id)
            .await
            .expect("read runs")
            .expect("asset still exists")
            .asset
            .status,
        MemoryAssetStatus::Active,
        "filtering is not invalidation: the audit is intact"
    );

    // The explicit act retires it, and the report says which asset went. The
    // summary is derived *first*: a summary of an invalidated source is refused
    // (`SequenceConflict`), which is the other half of the same invariant - a
    // derived asset can never be built on something already retired.
    let summary = fixture
        .memory
        .derive_l2(
            &principal,
            &[harness_types::MemoryVersionRef {
                memory_asset_id: id.clone(),
                version: 1,
            }],
            "the cache policy is settled",
        )
        .await
        .expect("summary is derived from a live source");
    let summary_id = summary.asset.memory_asset_id.clone();
    let retired = fixture
        .memory
        .invalidate_changed_sources(&principal, std::slice::from_ref(&moved))
        .await
        .expect("source invalidation runs");
    assert!(retired.contains(&id));
    assert_eq!(
        fixture
            .memory
            .read(&principal, &id)
            .await
            .expect("read runs")
            .expect("asset still exists")
            .asset
            .status,
        MemoryAssetStatus::Invalidated
    );
    let after = fixture
        .memory
        .search_terms_fresh(
            &principal,
            &["cache".to_owned(), "disabled".to_owned()],
            8,
            None,
            &[],
        )
        .await
        .expect("search runs");
    assert!(
        after.hits.iter().all(|hit| hit.asset.memory_asset_id != id),
        "a revoked asset is not in the next packet"
    );

    // A summary depends on the fact: invalidating the fact takes the summary.
    let cascade = fixture
        .memory
        .invalidate(&principal, &id, "source_changed")
        .await
        .expect("invalidation runs");
    assert!(
        cascade.contains(&summary_id),
        "dependency invalidation reaches the summary: {cascade:?}"
    );
    assert_eq!(
        fixture
            .memory
            .read(&principal, &summary_id)
            .await
            .expect("read runs")
            .expect("summary still exists")
            .asset
            .status,
        MemoryAssetStatus::Invalidated
    );

    // A user correction wins over what it replaced.
    let second = open_fixture().await;
    let principal = second.principal();
    let asset = second
        .memory
        .create_asset(
            &principal,
            second.observation_asset("the answer is 41", Vec::new()),
        )
        .await
        .expect("asset is created");
    let corrected = second
        .memory
        .propose(
            &principal,
            Some((&asset.asset.memory_asset_id, 1)),
            CreateMemoryAsset {
                content: "the answer is 42".to_owned(),
                authority: SourceAuthority::User,
                evidence: EvidenceState::UserConfirmed,
                user_confirmed: true,
                provenance_kind: "user_correction".to_owned(),
                ..second.observation_asset("the answer is 42", Vec::new())
            },
        )
        .await
        .expect("correction is written");
    assert_eq!(corrected.asset.current_version, 2);
    assert_eq!(corrected.current.record.supersedes, Some(1));
    let latest = second
        .memory
        .search(&principal, "the answer", 8, None)
        .await
        .expect("search runs");
    assert!(
        latest
            .hits
            .iter()
            .all(|hit| hit.current.content != "the answer is 41"),
        "the corrected version is what retrieval returns"
    );
    let packet = second.memory.contribute(&principal, &latest, 800);
    assert!(
        packet
            .stamps
            .iter()
            .all(|stamp| stamp.version == corrected.asset.current_version),
        "the packet stamps the selected version"
    );

    // Self-reinforcement: an extractor that quotes memory back must not be able
    // to store memory as its own evidence.
    //
    // The `quote_back` extractor projects each eligible source verbatim, but only
    // `input.admitted` and runtime observations are eligible - a rendered packet or
    // a memory block is never a source (see `project_sources`). So the quote this
    // test needs is an admitted input whose text *is* the stored fact: that is the
    // shape a model quoting an injected block produces.
    let third = open_fixture().await;
    let principal = third.principal();
    let strategy = strategy(ExtractionScope::Project);
    let text = "the deployment window is Tuesday".to_owned();
    third
        .memory
        .create_asset(&principal, third.observation_asset(&text, Vec::new()))
        .await
        .expect("the original fact is created");
    // The same sentence, admitted as user input. A real turn quotes the memory block
    // in its message; the fixture says the sentence instead of building the packet.
    third.admit_text(&text).await;
    let report = third
        .memory
        .reconcile(&third.session_id, &strategy, 16)
        .await
        .expect("reconcile runs");
    let lease = third
        .memory
        .lease_job(&report.outstanding[0].job_id, "worker-a")
        .await
        .expect("lease");
    let published = third
        .memory
        .extract_lease(
            &principal,
            &lease,
            &quote_back(),
            16_384,
            strategy.asset_scope,
        )
        .await
        .expect("extraction settles");
    assert!(
        published.is_empty(),
        "a candidate that is already memory is not published as an independent fact: {published:?}"
    );
    let jobs = third
        .memory
        .list_jobs_for(&third.session_id)
        .await
        .expect("jobs are listed");
    assert_eq!(
        jobs[0].disposition.as_deref(),
        Some("self_referential"),
        "the range is covered, and the disposition says why nothing was published"
    );
    assert_eq!(
        third
            .memory
            .extraction_cursor(&third.session_id, &strategy)
            .await
            .expect("cursor is readable"),
        1
    );

    // The guard is not "drop everything": a candidate the store does not already
    // hold is still published as a candidate.
    third.admit_text("a genuinely new observation").await;
    let report = third
        .memory
        .reconcile(&third.session_id, &strategy, 16)
        .await
        .expect("reconcile runs");
    let lease = third
        .memory
        .lease_job(&report.outstanding[0].job_id, "worker-a")
        .await
        .expect("lease");
    let published = third
        .memory
        .extract_lease(
            &principal,
            &lease,
            &quote_back(),
            16_384,
            strategy.asset_scope,
        )
        .await
        .expect("extraction settles");
    assert_eq!(
        published.len(),
        1,
        "new material is still extracted, so the guard is not a blanket refusal"
    );
    assert_eq!(
        published[0].asset.status,
        MemoryAssetStatus::Candidate,
        "an extracted fact is a candidate until a human confirms it"
    );
    assert_eq!(
        third
            .memory
            .extraction_cursor(&third.session_id, &strategy)
            .await
            .expect("cursor is readable"),
        2
    );
}

// ---------------------------------------------------------------------------
// M7-04 — CLI surface and the honestly-split quality report
// ---------------------------------------------------------------------------

#[tokio::test]
#[allow(clippy::too_many_lines)] // one CLI surface, exercised end to end
async fn m7_04_cli_propose_confirm_reject_export() {
    let temp = TempDir::new().expect("temporary data directory");
    let data_dir = temp.path().to_string_lossy().into_owned();
    let project_id = ProjectId::generate();
    let project = project_id.as_str().to_owned();

    let proposed = run_cli(&[
        "memory",
        "--data-dir",
        &data_dir,
        "--project-id",
        &project,
        "--json",
        "propose",
        "--content",
        "the release window is Tuesday",
    ]);
    assert_eq!(
        proposed.status.code(),
        Some(0),
        "propose runs: {}",
        String::from_utf8_lossy(&proposed.stderr)
    );
    let proposed: serde_json::Value =
        serde_json::from_slice(&proposed.stdout).expect("propose emits JSON");
    let asset_id = proposed["memory"]["proposed"]["asset"]["memory_asset_id"]
        .as_str()
        .expect("the proposed asset id is reported")
        .to_owned();
    assert_eq!(
        proposed["memory"]["published"], false,
        "a proposal is a candidate, not published memory"
    );
    assert_eq!(
        proposed["memory"]["proposed"]["asset"]["status"], "candidate",
        "the publication policy decides the status"
    );

    // A proposal on a stale version is refused, so it cannot overwrite a
    // published version.
    let stale = run_cli(&[
        "memory",
        "--data-dir",
        &data_dir,
        "--project-id",
        &project,
        "--json",
        "propose",
        "--asset",
        &asset_id,
        "--expected-version",
        "9",
        "--content",
        "a proposal built on a stale read",
    ]);
    assert_eq!(stale.status.code(), Some(5), "a CAS conflict is exit 5");
    assert!(
        String::from_utf8_lossy(&stale.stderr).contains("sequence_conflict")
            || String::from_utf8_lossy(&stale.stderr).contains("SequenceConflict"),
        "the refusal is typed: {}",
        String::from_utf8_lossy(&stale.stderr)
    );

    // Publishing requires the human flag, and confirmation works at the version
    // the human inspected.
    let unconfirmed = run_cli(&[
        "memory",
        "--data-dir",
        &data_dir,
        "--project-id",
        &project,
        "--json",
        "publish",
        "--asset-id",
        &asset_id,
        "--expected-version",
        "1",
    ]);
    assert_ne!(unconfirmed.status.code(), Some(0));
    let confirmed = run_cli(&[
        "memory",
        "--data-dir",
        &data_dir,
        "--project-id",
        &project,
        "--json",
        "confirm",
        "--asset",
        &asset_id,
        "--confirm",
    ]);
    assert_eq!(
        confirmed.status.code(),
        Some(0),
        "confirm runs: {}",
        String::from_utf8_lossy(&confirmed.stderr)
    );

    // The export carries the versions and their recorded sources, redacted.
    let exported = run_cli(&[
        "memory",
        "--data-dir",
        &data_dir,
        "--project-id",
        &project,
        "--json",
        "export",
        &asset_id,
    ]);
    assert_eq!(
        exported.status.code(),
        Some(0),
        "export runs: {}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let exported: serde_json::Value =
        serde_json::from_slice(&exported.stdout).expect("export emits JSON");
    let versions = exported["memory"]["versions"]
        .as_array()
        .expect("versions are listed");
    assert_eq!(versions.len(), 2, "the confirmation is a second version");
    assert_eq!(versions[0]["record"]["version"], json!(1));
    assert_eq!(versions[1]["record"]["supersedes"], json!(1));
    assert_eq!(exported["memory"]["redacted"], json!(true));

    // A rejection is a typed refusal at a version, and it retires the candidate.
    let second = run_cli(&[
        "memory",
        "--data-dir",
        &data_dir,
        "--project-id",
        &project,
        "--json",
        "propose",
        "--content",
        "a fact the operator will refuse",
    ]);
    assert_eq!(second.status.code(), Some(0));
    let second: serde_json::Value =
        serde_json::from_slice(&second.stdout).expect("propose emits JSON");
    let second_id = second["memory"]["proposed"]["asset"]["memory_asset_id"]
        .as_str()
        .expect("id")
        .to_owned();
    let rejected = run_cli(&[
        "memory",
        "--data-dir",
        &data_dir,
        "--project-id",
        &project,
        "--json",
        "reject",
        &second_id,
        "--expected-version",
        "1",
        "--reason",
        "the operator disagrees",
    ]);
    assert_eq!(
        rejected.status.code(),
        Some(0),
        "reject runs: {}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    let after = run_cli(&[
        "memory",
        "--data-dir",
        &data_dir,
        "--project-id",
        &project,
        "--json",
        "read",
        &second_id,
    ]);
    let after: serde_json::Value = serde_json::from_slice(&after.stdout).expect("read emits JSON");
    assert_eq!(
        after["memory"]["asset"]["asset"]["status"], "invalidated",
        "a rejection is recorded on the asset rather than deleting it"
    );
}

#[tokio::test]
async fn m7_04_quality_report_separates_measured_from_unmeasured() {
    let fixture = open_fixture().await;
    let principal = fixture.principal();

    // Planted material: one source-backed fact and one plausible-looking
    // misleading fact. They deliberately share no distinguishing term, so a hit
    // count says which one answered.
    let truth = "the parser rejects a missing comma";
    let misleading = "the parser accepts every trailing comma";
    fixture
        .memory
        .create_asset(
            &principal,
            fixture.observation_asset(
                truth,
                vec![MemorySource::file(
                    "src/parser.rs",
                    ContentHash::from_bytes(b"parser revision"),
                )],
            ),
        )
        .await
        .expect("true fact is created");
    fixture
        .memory
        .create_asset(
            &principal,
            fixture.observation_asset(misleading, Vec::new()),
        )
        .await
        .expect("misleading fact is created");

    let report = quality_report(&fixture, &principal).await;
    assert_eq!(report["retrieval"]["queries"], json!(2));
    assert_eq!(
        report["retrieval"]["hits"],
        json!(2),
        "each planted fact answers one query, so both are retrievable"
    );
    assert_eq!(
        report["retrieval"]["stamped"],
        json!(2),
        "every injected block carries a selection stamp"
    );
    assert!(
        report["tokens"]["injected"].as_u64().unwrap_or(0) > 0,
        "the token cost of injection is measured, not assumed"
    );
    assert!(
        report["latency_ms"]["search"].as_u64().is_some(),
        "search latency is measured"
    );
    assert_eq!(
        report["correctness"]["measured"],
        json!(false),
        "no correctness number is claimed, because nothing here judges an answer"
    );
    assert_eq!(
        report["correctness"]["reason"],
        json!(
            "the runtime does not grade model output; a correctness number would be a claim it cannot support"
        )
    );
    assert_eq!(
        report["self_reference"]["independent_proofs"],
        json!(0),
        "a quote of memory never counts as an independent source"
    );
    assert!(
        report["limits"]["unsupported"]
            .as_array()
            .expect("limits are listed")
            .iter()
            .any(|limit| limit == "vector_similarity"),
        "unsupported capabilities are named instead of being implied"
    );
}

/// The measured half of a memory on/off comparison.
///
/// Everything in the returned object was read from the run: counts, tokens and
/// durations. The one field that is not measured is `correctness`, and it says so
/// with a reason rather than reporting a number nobody computed.
async fn quality_report(fixture: &Fixture, principal: &MemoryPrincipal) -> serde_json::Value {
    // Two queries, each naming one planted fact by a term the other does not have,
    // so a hit count says which fact answered.
    let queries = ["rejects missing comma", "accepts every trailing"];
    let mut hits = 0usize;
    let mut stamped = 0usize;
    let mut injected_tokens = 0u64;
    let mut blocks = 0usize;
    let mut search_micros = 0u128;
    for query in queries {
        let started = std::time::Instant::now();
        let found = fixture
            .memory
            .search(principal, query, 8, None)
            .await
            .expect("search runs");
        search_micros = search_micros.saturating_add(started.elapsed().as_micros());
        hits = hits.saturating_add(found.hits.len());
        let packet = fixture.memory.contribute(principal, &found, 800);
        stamped = stamped.saturating_add(packet.stamps.len());
        blocks = blocks.saturating_add(packet.blocks.len());
        injected_tokens = injected_tokens.saturating_add(
            packet
                .blocks
                .iter()
                .map(|block| u64::try_from(block.text.len().div_ceil(4)).unwrap_or(u64::MAX))
                .sum::<u64>(),
        );
    }
    let independent = fixture
        .memory
        .search(principal, "the deployment window is Tuesday", 8, None)
        .await
        .expect("search runs")
        .hits
        .iter()
        .filter(|hit| hit.current.record.provenance_kind == "journal_derived")
        .count();
    json!({
        "schema_version": 1,
        "retrieval": {
            "queries": queries.len(),
            "hits": hits,
            "blocks": blocks,
            "stamped": stamped,
        },
        "tokens": {
            "injected": injected_tokens,
            "method": "bytes/4, the same rough measure the retrieval boundary uses",
        },
        "latency_ms": {
            "search": u64::try_from(search_micros / 1000).unwrap_or(u64::MAX),
            "method": "wall clock around the search call, one run, this host",
        },
        "correctness": {
            "measured": false,
            "reason": "the runtime does not grade model output; a correctness number would be a claim it cannot support",
        },
        "self_reference": {
            "independent_proofs": independent,
            "rule": "a candidate whose text is already memory is settled as self_referential, not published",
        },
        "limits": {
            "unsupported": ["vector_similarity", "quality_claim"],
            "query_byte_bound": MAX_QUERY_BYTES,
            "validity": "valid",
        },
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[must_use]
fn run_cli(arguments: &[&str]) -> Output {
    let mut path = std::env::current_exe().expect("test binary path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    let binary = path.join(format!("ha{}", std::env::consts::EXE_SUFFIX));
    std::process::Command::new(binary)
        .args(arguments)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("ha binary runs")
}
