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
    MemoryService, RetrievalState, normalize_terms,
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

/// Longest input still judged by its opening word.
///
/// A question is a line; a specification, a log or a pasted document is not, and
/// judging a long input by its first word would drop knowledge that happens to start
/// with "how".
const MAX_QUESTION_WORDS: usize = 24;

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
///
/// One query, not two. This function used to run the whole question and then retry
/// with its longest terms, because the retrieval boundary joined every term with
/// `AND`. The boundary now widens and applies its own overlap floor, so a second pass
/// from here would only repeat what the service already did - and the "longest terms"
/// heuristic was never the reason the retry failed anyway: `remember answer marker
/// just` is still a conjunction of four terms the stored instruction does not all
/// hold.
pub async fn recall(
    store: Arc<SqliteStore>,
    principal: &MemoryPrincipal,
    text: &str,
) -> Result<Recall, HarnessError> {
    let service = MemoryService::new(store);
    let terms = normalize_terms(text);
    let result = service
        .search_terms(principal, &terms, RECALL_HITS, None)
        .await?;
    let contribution = service.contribute(principal, &result, CONTRIBUTION_TOKENS);
    let hits = result.hits.len();
    let blocks = contribution.blocks.len();
    let message = match result.state {
        RetrievalState::Found => format!("memory: {hits} hit(s), {blocks} block(s) injected"),
        RetrievalState::Empty => match result.detail.as_deref() {
            // Two different empties, and the difference is what tells a reader
            // whether to rephrase or to stop asking.
            Some("no_term_overlap") => {
                "memory: nothing matching this question yet (no term overlap)".to_owned()
            }
            _ => "memory: nothing to search for in this message".to_owned(),
        },
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

/// What one attempt to remember the admitted input did.
///
/// The old signature returned `Option`, which collapsed three different outcomes into
/// "nothing happened". A reader of the transcript could not tell "you said nothing",
/// "this was already remembered" and "this was a question, not knowledge" apart - and
/// the third one silently filled the corpus with questions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RememberOutcome {
    /// A new asset holds this input.
    Stored(MemoryAssetId),
    /// An asset already held this text; the new source event was recorded on it.
    Duplicate(MemoryAssetId),
    /// This input is not the kind of thing memory keeps. The reason is reported.
    NotKnowledge {
        /// Why it was not stored, in words the transcript can print.
        reason: &'static str,
    },
    /// The turn admitted no input, or the input was blank.
    NothingAdmitted,
}

/// Words that open a question. Kept at module scope because they are a vocabulary,
/// not a local detail.
const INTERROGATIVE_OPENINGS: &[&str] = &[
    "what", "why", "how", "when", "where", "which", "who", "whom", "whose", "can", "could",
    "should", "would", "will", "is", "are", "am", "was", "were", "do", "does", "did", "have",
    "has", "had", "may", "might", "must", "shall", "gì", "sao", "thế", "tại", "bao", "khi", "đâu",
    "ai", "có", "làm",
];

/// Whether one admitted input is knowledge worth keeping.
///
/// Only directives and statements are. A question is the user asking, not the user
/// telling, and storing it as a `user_instruction` asset made every question a
/// confirmed fact - which is how the corpus filled with questions that then outranked
/// the answers.
///
/// The test is deliberately narrow, and it only applies to a short input: it looks
/// for a question mark or an interrogative opening, and anything it does not
/// recognise stays knowledge. Dropping a real instruction is worse than keeping a
/// question, so the default is to keep.
fn classify_input(text: &str) -> Option<&'static str> {
    let trimmed = text.trim();
    if trimmed.ends_with('?') || trimmed.ends_with('？') {
        return Some("a question is not an instruction");
    }
    let opening = trimmed
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase();
    let opening = opening.trim_end_matches(['?', '？', ',', '.', '!']);
    // A pasted document can open with any word, so this only judges a short line: a
    // question is short, a specification is not.
    if trimmed.split_whitespace().count() <= MAX_QUESTION_WORDS
        && INTERROGATIVE_OPENINGS.contains(&opening)
    {
        return Some("a question is not an instruction");
    }
    None
}

/// Store the exact admitted input of one session as reusable memory.
///
/// The text comes from the durable admission rather than from the caller, so the
/// stored copy is what the journal acknowledged, and its event id is the memory
/// source reference.
///
/// Two things keep the corpus from filling with copies of itself:
///
/// - an input that is a question is not knowledge, and is reported as such rather
///   than stored;
/// - an input whose text an active asset already holds does not become a second
///   asset. The new source event is recorded on the existing version, which is what
///   keeps the audit trail without minting a near-duplicate that would compete with
///   the original in ranking.
pub async fn remember_input(
    store: Arc<SqliteStore>,
    principal: &MemoryPrincipal,
    session_id: &SessionId,
) -> Result<RememberOutcome, HarnessError> {
    let Some((event_id, text)) = store
        .session_admitted_input(session_id)
        .await
        .map_err(StoreError::into_harness_error)?
    else {
        return Ok(RememberOutcome::NothingAdmitted);
    };
    if text.trim().is_empty() {
        return Ok(RememberOutcome::NothingAdmitted);
    }
    if let Some(reason) = classify_input(&text) {
        return Ok(RememberOutcome::NotKnowledge { reason });
    }
    // A project-scoped asset needs the project the principal carries; a principal
    // without one can still store what the user typed, at user scope.
    let (scope, project_id) = match principal.project_id.clone() {
        Some(project_id) => (MemoryScope::Project, Some(project_id)),
        None => (MemoryScope::User, None),
    };
    let service = MemoryService::new(Arc::clone(&store));
    if let Some(existing) = service
        .find_active_by_content(principal, &text, scope, project_id.clone())
        .await?
    {
        service
            .append_version_source(principal, &existing, &event_id)
            .await?;
        return Ok(RememberOutcome::Duplicate(existing));
    }
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
    Ok(RememberOutcome::Stored(asset.asset.memory_asset_id))
}

#[cfg(test)]
mod tests {
    use super::{
        MEMORY_VARIABLE, RetrievalState, memory_requested, memory_requested_from_environment,
        principal, recall, remember_input,
    };
    use crate::interactive::paths::LaunchEnvironment;
    use crate::interactive::project::resolve_project_id;
    use harness_memory::{MemoryPrincipal, MemoryService, normalize_terms};
    use harness_providers::MockProvider;
    use harness_runtime::{RunRequest, RuntimeConfig, RuntimeService};
    use harness_session::{AdmitInputRequest, SessionService};
    use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
    use harness_tools::observe_workspace;
    use harness_types::{HostId, InputId, ProjectId, SessionId, SourceAuthority, TaskId};
    use std::sync::Arc;

    /// Unwrap the outcome of remembering an admitted input.
    trait ExpectStored {
        fn expect_stored(self) -> harness_types::MemoryAssetId;
    }

    impl ExpectStored for super::RememberOutcome {
        fn expect_stored(self) -> harness_types::MemoryAssetId {
            match self {
                super::RememberOutcome::Stored(asset_id) => asset_id,
                other => panic!("an admitted input should be stored, got {other:?}"),
            }
        }
    }

    /// Helper for the recall tests: count what a search returns for one query.
    async fn hits_for(fixture: &Fixture, query: &str) -> usize {
        let project_id = resolve_project_id(&fixture.store, &fixture.workspace)
            .await
            .expect("project identity");
        let service = MemoryService::new(Arc::clone(&fixture.store));
        let owner = MemoryPrincipal {
            principal_id: super::MEMORY_PRINCIPAL.to_owned(),
            project_id: Some(project_id),
            task_id: None,
            agent_profile_id: None,
            session_id: None,
        };
        service
            .search(&owner, query, 32, None)
            .await
            .expect("search runs")
            .hits
            .len()
    }

    /// Helper for the recall tests: what the store holds for one exact text.
    async fn assets_holding(fixture: &Fixture, text: &str) -> usize {
        hits_for(fixture, &format!("\"{text}\"")).await
    }

    /// The defect this whole round exists for.
    ///
    /// A stored instruction was recalled by a later session only when the query
    /// reused its words. A person does not talk like that: they ask a question. The
    /// retrieval boundary built an `AND` of every term, so a question that adds words
    /// the instruction never used matched nothing - and the model then answered "I
    /// don't have that" while the answer sat in the store.
    #[tokio::test]
    async fn memory_a_natural_language_question_reaches_the_stored_instruction() {
        let fixture = fixture().await;
        let project_id = resolve_project_id(&fixture.store, &fixture.workspace)
            .await
            .expect("project identity");
        let task_id = TaskId::generate();
        let first = SessionId::generate();
        admit(
            &fixture,
            &project_id,
            &first,
            &task_id,
            "Remember this marker for later: zebra-quasar-7719",
        )
        .await;
        remember_input(
            Arc::clone(&fixture.store),
            &principal(project_id.clone(), task_id.clone(), first.clone()),
            &first,
        )
        .await
        .expect("remember runs")
        .expect_stored();

        let later = principal(project_id, task_id, SessionId::generate());
        let asked = "What marker did I ask you to remember? Answer with just the marker.";
        let recalled = recall(Arc::clone(&fixture.store), &later, asked)
            .await
            .expect("recall runs");
        assert_eq!(
            recalled.state,
            RetrievalState::Found,
            "a question about a stored instruction must find it: {}",
            recalled.message
        );
        assert!(
            recalled
                .contribution
                .blocks
                .iter()
                .any(|block| block.text.contains("zebra-quasar-7719")),
            "the block that reaches the model must be the instruction, not a question \
             that resembles it: {:#?}",
            recalled.contribution.blocks
        );
    }

    /// Saying the same thing twice is one memory, not two.
    ///
    /// Every successful turn stored the whole input, so repeating an instruction
    /// minted another active, searchable asset each time - and near-identical assets
    /// then competed with the original in ranking. Measured before the fix, the same
    /// question produced 2, then 3, then 4 hits across three runs, and none of them
    /// was the answer it was looking for.
    ///
    /// The input here is a directive, not a question: a question is refused before
    /// deduplication is even reached, so it would not test this at all.
    #[tokio::test]
    async fn memory_a_repeated_input_creates_no_second_asset() {
        let fixture = fixture().await;
        let project_id = resolve_project_id(&fixture.store, &fixture.workspace)
            .await
            .expect("project identity");
        let said = "Always run cargo test before you commit";

        let mut first_asset = None;
        for _ in 0..3 {
            // A fresh task each time: a task's lease belongs to one session, and the
            // asset is project-scoped, so the repeats are three different tasks
            // saying the same thing - which is exactly the case that must collapse.
            let task_id = TaskId::generate();
            let session = SessionId::generate();
            admit(&fixture, &project_id, &session, &task_id, said).await;
            let outcome = remember_input(
                Arc::clone(&fixture.store),
                &principal(project_id.clone(), task_id.clone(), session.clone()),
                &session,
            )
            .await
            .expect("remember runs");
            match (outcome, &first_asset) {
                (super::RememberOutcome::Stored(asset_id), None) => {
                    first_asset = Some(asset_id);
                }
                (super::RememberOutcome::Duplicate(asset_id), Some(first)) => {
                    assert_eq!(&asset_id, first, "the repeat must resolve to the original");
                }
                (other, _) => panic!("a directive is stored once then deduplicated: {other:?}"),
            }
        }

        assert_eq!(
            assets_holding(&fixture, said).await,
            1,
            "the same instruction three times is one memory, not three"
        );
        // The audit trail survives the deduplication: the asset now names all three
        // admissions as its sources, while its version is untouched.
        let asset_id = first_asset.expect("first store");
        let service = MemoryService::new(Arc::clone(&fixture.store));
        let stored = service
            .read(
                &principal(project_id, TaskId::generate(), SessionId::generate()),
                &asset_id,
            )
            .await
            .expect("read runs")
            .expect("the asset exists");
        assert_eq!(
            stored.current.record.source_event_refs.len(),
            3,
            "every admission that said this is recorded as a source"
        );
        assert_eq!(
            stored.asset.current_version, 1,
            "deduplication must not mint a version"
        );
    }

    /// A wider query must not become a licence to inject anything.
    ///
    /// This is the test that gives the overlap floor its teeth. The union finds every
    /// note that shares any word with the question, and one shared word is an accident
    /// of vocabulary rather than evidence of aboutness: a note about a deploy script
    /// shares `deploy`, and would be injected as if it answered a question about notes
    /// in a drawer. The floor is what stops that.
    #[tokio::test]
    async fn memory_a_wider_query_does_not_inject_a_note_that_only_shares_one_word() {
        let fixture = fixture().await;
        let project_id = resolve_project_id(&fixture.store, &fixture.workspace)
            .await
            .expect("project identity");
        for text in [
            // Shares one term with the question: `deploy`.
            "the deploy script is kept beside the fireplace",
            // Shares two: `notes` and `drawer`.
            "the notes are kept in the bottom drawer",
        ] {
            let task_id = TaskId::generate();
            let session = SessionId::generate();
            admit(&fixture, &project_id, &session, &task_id, text).await;
            remember_input(
                Arc::clone(&fixture.store),
                &principal(project_id.clone(), task_id, session.clone()),
                &session,
            )
            .await
            .expect("remember runs")
            .expect_stored();
        }

        let later = principal(project_id, TaskId::generate(), SessionId::generate());
        let question = "which drawer holds the notes";
        let recalled = recall(Arc::clone(&fixture.store), &later, question)
            .await
            .expect("recall runs");
        let joined = recalled
            .contribution
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("bottom drawer"),
            "the note the question is about must arrive: {joined}"
        );
        assert!(
            !joined.contains("fireplace"),
            "a note sharing one incidental word must not be injected as the answer: {joined}"
        );
    }

    /// A question is not knowledge, and the store must say so instead of silently
    /// dropping it.
    #[tokio::test]
    async fn memory_a_question_is_not_stored_as_knowledge() {
        let fixture = fixture().await;
        let project_id = resolve_project_id(&fixture.store, &fixture.workspace)
            .await
            .expect("project identity");
        let task_id = TaskId::generate();
        let session = SessionId::generate();
        let asked = "What marker did I ask you to remember?";
        admit(&fixture, &project_id, &session, &task_id, asked).await;

        let outcome = remember_input(
            Arc::clone(&fixture.store),
            &principal(project_id, task_id, session.clone()),
            &session,
        )
        .await
        .expect("remember runs");
        assert!(
            matches!(outcome, super::RememberOutcome::NotKnowledge { .. }),
            "a question is not an instruction: {outcome:?}"
        );
        assert_eq!(
            assets_holding(&fixture, asked).await,
            0,
            "and it must not become searchable knowledge"
        );
    }

    /// When recall comes back empty it must not claim the query had no terms.
    ///
    /// Measured before the fix: the result carried the seed detail
    /// `no_searchable_terms` even when the query was full of terms and the real story
    /// was that none of them overlapped.
    #[tokio::test]
    async fn memory_an_empty_recall_says_whether_terms_existed() {
        let fixture = fixture().await;
        let project_id = resolve_project_id(&fixture.store, &fixture.workspace)
            .await
            .expect("project identity");
        let owner = principal(project_id, TaskId::generate(), SessionId::generate());

        let service = MemoryService::new(Arc::clone(&fixture.store));
        let probe = MemoryPrincipal {
            principal_id: owner.principal_id.clone(),
            project_id: owner.project_id.clone(),
            task_id: None,
            agent_profile_id: None,
            session_id: None,
        };
        let with_terms = service
            .search(&probe, "nothing here was ever stored", 8, None)
            .await
            .expect("search runs");
        assert_eq!(with_terms.state, RetrievalState::Empty);
        assert_ne!(
            with_terms.detail.as_deref(),
            Some("no_searchable_terms"),
            "the query had terms; the honest detail is that none overlapped"
        );

        let without_terms = service
            .search(&probe, "??? !!!", 8, None)
            .await
            .expect("search runs");
        assert_eq!(without_terms.state, RetrievalState::Empty);
        assert_eq!(
            without_terms.detail.as_deref(),
            Some("no_searchable_terms"),
            "a query with nothing searchable is a different answer"
        );
    }

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
    fn a_query_is_normalized_into_terms_and_never_grows_past_the_bound() {
        assert_eq!(
            normalize_terms("  sửa lỗi parser  "),
            vec!["sua".to_owned(), "loi".to_owned(), "parser".to_owned()],
            "terms are normalized exactly like the indexed mirror"
        );
        assert!(
            normalize_terms("!!! ???").is_empty(),
            "punctuation is not a term"
        );
        let many = (0..200)
            .map(|index| format!("term{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            normalize_terms(&many).len(),
            32,
            "a query is capped at the boundary's term limit"
        );
    }

    /// This test used to pin the two-query retry: the whole question, then its four
    /// longest terms, both of them `AND`ed.
    ///
    /// Both queries failed on the case that mattered. A stored instruction reads
    /// "Remember this marker for later"; the question asks "what marker did I ask you
    /// to remember", and no conjunction of four of those words appears in it. The
    /// retry is gone: [`MemoryService::search_terms`] widens to a union and applies an
    /// overlap floor, so the query that used to need a second attempt now succeeds on
    /// the first, and the words a question adds are no longer fatal.
    #[test]
    fn a_question_keeps_its_own_words_as_terms_instead_of_dropping_them() {
        let terms = normalize_terms("dự án dùng ngôn ngữ gì hả bạn");
        assert_eq!(
            terms,
            vec![
                "du".to_owned(),
                "an".to_owned(),
                "dung".to_owned(),
                "ngon".to_owned(),
                "ngu".to_owned(),
                "gi".to_owned(),
                "ha".to_owned(),
                "ban".to_owned(),
            ],
            "every word is a term; the union is what finds the instruction"
        );
        assert!(
            terms.len() > 4,
            "keeping only the four longest terms is the behaviour that failed"
        );
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
            .expect("remember runs")
            .expect_stored();
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
        .expect("remember runs")
        .expect_stored();

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
        .expect("remember runs")
        .expect_stored();

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
