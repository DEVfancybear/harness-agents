//! Optional chat memory: what the user already said, recalled into the next turn.
//!
//! Memory is off unless `HA_MEMORY=on`, because a launch never turns durable
//! retention on by itself. With it on, one turn does two bounded things:
//!
//! - **before dispatch** the user's text is the retrieval query; matching scoped
//!   assets are contributed as optional context blocks together with the exact
//!   memory versions they came from;
//! - **after the turn** the admitted input text is stored once as an active,
//!   user-confirmed, project-scoped L1 asset. Creating an asset also binds it to
//!   its owner, which is what makes the next turn able to find it.
//!
//! Scope is host-issued: the principal carries the project identity the workspace
//! root was registered under, so a later process that opens the same workspace
//! resolves the same identity and sees the same memory. The model never chooses
//! scope, and only text the user sent — never model output — is promoted to an
//! active asset.

use std::sync::Arc;

use harness_memory::{
    CreateMemoryAsset, EvidenceState, MemoryContribution, MemoryLayer, MemoryPrincipal,
    MemoryService, RetrievalResult, RetrievalState,
};
use harness_store_sqlite::{SqliteStore, StoreError};
use harness_types::{
    HarnessError, MemoryAssetId, MemoryScope, ProjectId, SessionId, SourceAuthority, TaskId,
};

use super::paths::LaunchEnvironment;

/// Environment variable that switches chat memory on; only the value `on` counts.
pub const MEMORY_VARIABLE: &str = "HA_MEMORY";

/// Host principal every interactive turn runs as.
///
/// The same name the `ha memory` commands default to, so what a chat stored can
/// be inspected and searched from the CLI.
pub const MEMORY_PRINCIPAL: &str = "local-user";

/// Upper bound of memory tokens contributed to one turn.
const CONTRIBUTION_TOKENS: u64 = 800;

/// Upper bound of hits considered for one turn.
const RECALL_HITS: usize = 8;

/// Longest query the retrieval boundary accepts.
const QUERY_BYTES: usize = 1024;

/// How many salient terms the narrower retry keeps.
const FALLBACK_TERMS: usize = 4;

/// Whether the environment asks for chat memory.
///
/// Only the exact value `on` counts: an unknown or misspelled value must not
/// silently start writing durable memory.
#[must_use]
pub fn memory_requested(value: Option<&str>) -> bool {
    matches!(value, Some(value) if value.eq_ignore_ascii_case("on"))
}

/// Read [`MEMORY_VARIABLE`] from the injected launch environment.
#[must_use]
pub fn memory_requested_from_environment(environment: &LaunchEnvironment) -> bool {
    memory_requested(
        environment
            .value(MEMORY_VARIABLE)
            .and_then(|value| value.to_str()),
    )
}

/// The host-issued principal of one turn.
#[must_use]
pub fn principal(project_id: ProjectId, task_id: TaskId, session_id: SessionId) -> MemoryPrincipal {
    MemoryPrincipal {
        principal_id: MEMORY_PRINCIPAL.to_owned(),
        project_id: Some(project_id),
        task_id: Some(task_id),
        agent_profile_id: None,
        session_id: Some(session_id),
    }
}

/// What one retrieval contributed to a turn.
pub struct Recall {
    pub contribution: MemoryContribution,
    pub state: RetrievalState,
    pub hits: usize,
    pub blocks: usize,
    /// One line for the user, including an empty or degraded result.
    pub message: String,
}

/// Retrieve the memory one turn may see, bounded in queries, hits and tokens.
pub async fn recall(
    store: Arc<SqliteStore>,
    principal: &MemoryPrincipal,
    text: &str,
) -> Result<Recall, HarnessError> {
    let service = MemoryService::new(store);
    let mut result = RetrievalResult {
        state: RetrievalState::Empty,
        hits: Vec::new(),
        detail: Some("no_searchable_terms".to_owned()),
        revision: 0,
    };
    let mut matched_terms = None;
    for (index, query) in retrieval_queries(text).into_iter().enumerate() {
        let attempt = service.search(principal, &query, RECALL_HITS, None).await?;
        let found = attempt.state == RetrievalState::Found;
        result = attempt;
        if found {
            matched_terms = Some(index);
            break;
        }
        // An unavailable index is not worth a second query, and an empty result is
        // only retried with the narrower query the loop has next.
        if result.state == RetrievalState::Error {
            break;
        }
    }
    let contribution = service.contribute(principal, &result, CONTRIBUTION_TOKENS);
    let hits = result.hits.len();
    let blocks = contribution.blocks.len();
    let message = match result.state {
        RetrievalState::Found => format!(
            "memory: {hits} hit(s), {blocks} block(s) injected{}",
            match matched_terms {
                Some(0) | None => String::new(),
                Some(_) => " (matched on salient terms)".to_owned(),
            }
        ),
        RetrievalState::Empty => "memory: nothing matching this question yet".to_owned(),
        RetrievalState::Degraded => {
            "memory: lexical search only, optional service unavailable".to_owned()
        }
        RetrievalState::Error => format!(
            "memory: retrieval unavailable ({})",
            result.detail.as_deref().unwrap_or("unknown")
        ),
    };
    Ok(Recall {
        contribution,
        state: result.state,
        hits,
        blocks,
        message,
    })
}

/// Store the exact admitted input of one session as reusable memory.
///
/// The text comes from the durable admission rather than from the caller, so the
/// stored copy is what the journal acknowledged, and its event id is the memory
/// source reference. Nothing is written when the turn admitted no input.
pub async fn remember_input(
    store: Arc<SqliteStore>,
    principal: &MemoryPrincipal,
    session_id: &SessionId,
) -> Result<Option<MemoryAssetId>, HarnessError> {
    let Some((event_id, text)) = store
        .session_admitted_input(session_id)
        .await
        .map_err(StoreError::into_harness_error)?
    else {
        return Ok(None);
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    // A project-scoped asset needs the project the principal carries; a principal
    // without one can still store what the user confirmed, at user scope.
    let (scope, project_id) = match principal.project_id.clone() {
        Some(project_id) => (MemoryScope::Project, Some(project_id)),
        None => (MemoryScope::User, None),
    };
    let service = MemoryService::new(store);
    let asset = service
        .create_asset(
            principal,
            CreateMemoryAsset {
                kind: "user_instruction".to_owned(),
                scope,
                layer: MemoryLayer::L1,
                project_id,
                task_id: None,
                agent_profile_id: None,
                session_id: None,
                visibility: "scoped".to_owned(),
                content: text.clone(),
                // The user typed it: that confirmation is what the publication
                // policy accepts as publishable for an L1 asset.
                authority: SourceAuthority::User,
                evidence: EvidenceState::UserConfirmed,
                user_confirmed: true,
                source_event_refs: vec![event_id],
                source_file_hashes: Vec::new(),
                source_commit: None,
                provenance_kind: "interactive_admission".to_owned(),
            },
        )
        .await?;
    Ok(Some(asset.asset.memory_asset_id))
}

/// The queries one turn searches with, widest first.
///
/// The retrieval boundary joins every term with `AND`, so a natural question that
/// adds words the stored text never used matches nothing. The second query keeps
/// the longest terms only, which is the term selection a recall is expected to
/// make; it runs only when the first found nothing.
fn retrieval_queries(text: &str) -> Vec<String> {
    let bounded = bound_query(text);
    let mut queries = Vec::new();
    if has_searchable_term(&bounded) {
        queries.push(bounded.clone());
    }
    let mut terms = bounded
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| term.chars().any(char::is_alphanumeric))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    terms.sort_by(|left, right| {
        right
            .chars()
            .count()
            .cmp(&left.chars().count())
            .then_with(|| left.cmp(right))
    });
    terms.dedup();
    if terms.len() > FALLBACK_TERMS {
        let narrower = terms
            .iter()
            .take(FALLBACK_TERMS)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        if has_searchable_term(&narrower) && !queries.contains(&narrower) {
            queries.push(narrower);
        }
    }
    queries
}

/// Keep a query inside the retrieval boundary's byte limit without splitting one.
fn bound_query(text: &str) -> String {
    let mut bounded = String::new();
    for character in text.chars() {
        if bounded.len() + character.len_utf8() > QUERY_BYTES {
            break;
        }
        bounded.push(character);
    }
    bounded.trim().to_owned()
}

/// Whether a query has anything an index could match.
fn has_searchable_term(text: &str) -> bool {
    text.chars().any(char::is_alphanumeric)
}

#[cfg(test)]
mod tests {
    use super::{
        MEMORY_VARIABLE, RetrievalState, bound_query, memory_requested,
        memory_requested_from_environment, principal, recall, remember_input, retrieval_queries,
    };
    use crate::interactive::paths::LaunchEnvironment;
    use crate::interactive::project::resolve_project_id;
    use harness_providers::MockProvider;
    use harness_runtime::{RunRequest, RuntimeConfig, RuntimeService};
    use harness_session::{AdmitInputRequest, SessionService};
    use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
    use harness_tools::observe_workspace;
    use harness_types::{HostId, InputId, ProjectId, SessionId, SourceAuthority, TaskId};
    use std::sync::Arc;

    #[test]
    fn memory_is_off_unless_the_exact_value_asks_for_it() {
        assert!(memory_requested(Some("on")));
        assert!(memory_requested(Some("ON")));
        assert!(!memory_requested(Some("off")));
        assert!(!memory_requested(Some("true")));
        assert!(!memory_requested(Some("")));
        assert!(!memory_requested(None));

        let environment = LaunchEnvironment::from_pairs([(MEMORY_VARIABLE, "on")]);
        assert!(memory_requested_from_environment(&environment));
        let unrelated = LaunchEnvironment::from_pairs([("HA_UI", "plain")]);
        assert!(!memory_requested_from_environment(&unrelated));
    }

    #[test]
    fn a_query_is_bounded_and_text_without_terms_is_never_searched() {
        assert_eq!(bound_query("  sửa lỗi parser  "), "sửa lỗi parser");
        let long = "ệ".repeat(2000);
        assert!(
            bound_query(&long).len() <= 1024,
            "a query stays inside the limit"
        );
        assert!(retrieval_queries("!!! ???").is_empty());
        assert_eq!(retrieval_queries("một hai"), vec!["một hai".to_owned()]);
    }

    #[test]
    fn a_question_with_extra_words_retries_with_its_longest_terms() {
        let queries = retrieval_queries("dự án dùng ngôn ngữ gì hả bạn");
        assert_eq!(queries.len(), 2, "{queries:?}");
        assert_eq!(queries[0], "dự án dùng ngôn ngữ gì hả bạn");
        // Longest terms first, ties alphabetical, so the narrower query is stable.
        assert_eq!(queries[1], "dùng ngôn bạn ngữ");
    }

    /// One data directory and one workspace root, as a real launch has.
    struct Fixture {
        _temp: tempfile::TempDir,
        store: Arc<SqliteStore>,
        workspace: std::path::PathBuf,
    }

    async fn fixture() -> Fixture {
        let temp = tempfile::tempdir().expect("temp data dir");
        let workspace = temp.path().join("project");
        std::fs::create_dir(&workspace).expect("workspace root");
        let store = Arc::new(
            SqliteStore::open_writer(WriterOpenOptions::new(
                temp.path().join("data"),
                HostId::generate(),
            ))
            .await
            .expect("store opens"),
        );
        Fixture {
            _temp: temp,
            store,
            workspace,
        }
    }

    /// Admit one input the way a turn does, so the memory source reference exists.
    async fn admit(
        fixture: &Fixture,
        project_id: &ProjectId,
        session_id: &SessionId,
        task_id: &TaskId,
        text: &str,
    ) {
        let workspace = observe_workspace(project_id.clone(), &fixture.workspace)
            .expect("workspace observation");
        SessionService::new(Arc::clone(&fixture.store))
            .admit_input(AdmitInputRequest {
                session_id: session_id.clone(),
                task_id: task_id.clone(),
                input_id: InputId::generate(),
                expected_sequence: 1,
                authority: SourceAuthority::User,
                raw_text: text.to_owned(),
                workspace,
                initial_plan_items: Vec::new(),
            })
            .await
            .expect("input admitted");
    }

    /// The end-to-end promise: what one turn stored, a later turn can recall.
    #[tokio::test]
    async fn a_stored_instruction_is_recalled_by_a_later_session() {
        let fixture = fixture().await;
        let store = &fixture.store;
        let project_id = resolve_project_id(store, &fixture.workspace)
            .await
            .expect("project identity");
        assert_eq!(
            resolve_project_id(store, &fixture.workspace)
                .await
                .expect("second resolution"),
            project_id,
            "the same root resolves to the same project identity"
        );

        let task_id = TaskId::generate();
        let first = SessionId::generate();
        let second = SessionId::generate();
        let owner = principal(project_id.clone(), task_id.clone(), first.clone());
        admit(
            &fixture,
            &project_id,
            &first,
            &task_id,
            "dự án này dùng Rust nhé",
        )
        .await;
        let stored = remember_input(Arc::clone(store), &owner, &first)
            .await
            .expect("input stored")
            .expect("an admitted input is stored");
        assert_eq!(stored.as_str().split('_').next(), Some("memory"));

        let later = principal(project_id, task_id, second);
        let recalled = recall(Arc::clone(store), &later, "dự án dùng Rust nhé?")
            .await
            .expect("recall runs");
        assert_eq!(
            recalled.state,
            RetrievalState::Found,
            "{}",
            recalled.message
        );
        assert_eq!(recalled.blocks, 1, "{}", recalled.message);
        assert!(
            recalled.contribution.versions.len() == 1,
            "a contributed block carries the version it came from"
        );
        assert_eq!(recalled.contribution.principal, later);
    }

    /// Scope is host-issued: another project identity must not see the asset.
    #[tokio::test]
    async fn memory_of_one_project_is_invisible_to_another() {
        let fixture = fixture().await;
        let store = &fixture.store;
        let project_id = resolve_project_id(store, &fixture.workspace)
            .await
            .expect("project identity");
        let task_id = TaskId::generate();
        let session_id = SessionId::generate();
        admit(
            &fixture,
            &project_id,
            &session_id,
            &task_id,
            "chỉ dùng cargo test",
        )
        .await;
        remember_input(
            Arc::clone(store),
            &principal(project_id, task_id.clone(), session_id.clone()),
            &session_id,
        )
        .await
        .expect("input stored")
        .expect("an admitted input is stored");

        let stranger = principal(ProjectId::generate(), task_id, SessionId::generate());
        let recalled = recall(Arc::clone(store), &stranger, "cargo test")
            .await
            .expect("recall runs");
        assert_ne!(
            recalled.state,
            RetrievalState::Found,
            "a different project identity must not read the asset"
        );
        assert_eq!(recalled.blocks, 0);
    }

    /// A contribution survives the runtime: it is rendered into the frozen packet.
    ///
    /// This is the last link of the chain the service relies on — recall, contribute,
    /// `with_memory`, context build, commit — checked on the packet the adapter would
    /// have been sent rather than on an internal return value.
    #[tokio::test]
    async fn a_contribution_reaches_the_frozen_packet() {
        let fixture = fixture().await;
        let store = &fixture.store;
        let project_id = resolve_project_id(store, &fixture.workspace)
            .await
            .expect("project identity");
        let task_id = TaskId::generate();
        let first = SessionId::generate();
        admit(
            &fixture,
            &project_id,
            &first,
            &task_id,
            "ghi nhớ marker zeta42",
        )
        .await;
        remember_input(
            Arc::clone(store),
            &principal(project_id.clone(), task_id.clone(), first.clone()),
            &first,
        )
        .await
        .expect("input stored")
        .expect("an admitted input is stored");

        let runtime = RuntimeService::new(
            Arc::clone(store),
            Arc::new(MockProvider::text("ghi nhận")),
            RuntimeConfig::default(),
        );
        // A second task: the admission above holds this task's lease for its own
        // session, and the memory it wrote is project-scoped, so another task in the
        // same project is exactly the case that must still see it.
        let task_id = TaskId::generate();
        let session_id = SessionId::generate();
        let owner = principal(project_id.clone(), task_id.clone(), session_id.clone());
        let found = recall(Arc::clone(store), &owner, "marker zeta42")
            .await
            .expect("recall runs");
        assert_eq!(found.state, RetrievalState::Found, "{}", found.message);
        runtime
            .run(
                RunRequest::new(
                    session_id.clone(),
                    task_id,
                    InputId::generate(),
                    "marker zeta42 là gì?",
                    observe_workspace(project_id, &fixture.workspace)
                        .expect("workspace observation"),
                )
                .with_memory(found.contribution),
            )
            .await
            .expect("turn runs");

        let packet = store
            .latest_context_packet(&session_id)
            .await
            .expect("packet read")
            .expect("the turn committed a packet");
        assert!(
            packet.packet.content.contains("marker zeta42"),
            "the stored instruction must be in the packet: {}",
            packet.packet.content
        );
        assert!(
            packet.packet.content.contains("Reusable data; authority="),
            "it must be rendered as a memory block: {}",
            packet.packet.content
        );
        assert_eq!(
            packet.packet.memory_versions.len(),
            1,
            "the packet must pin the memory version it used"
        );
    }
}
