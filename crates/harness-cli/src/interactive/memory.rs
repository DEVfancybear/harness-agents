//! Chat memory: what the user already said, recalled into the next turn.
//!
//! Memory is on unless `HA_MEMORY` is set to `off` (or `0`, `false`, `no`). It used to be
//! off unless asked for, and in practice that meant it was off: the conversation was
//! never learned from, and the feature existed only for whoever knew the variable. With
//! it on, one turn does two bounded things:
//!
//! - **before dispatch** the user's text is the retrieval query; matching scoped
//!   assets are contributed as optional context blocks together with the exact
//!   memory versions they came from;
//! - **after the turn** three things are written: the turn itself (the conversation
//!   log), the input as a user-confirmed asset *only when the user asked for it to be
//!   remembered*, and the facts a model extracts from the turn. A confident fact is
//!   applied at once; a less certain one waits for review as a candidate.
//!
//! Scope is host-issued: the principal carries the project identity the workspace
//! root was registered under, so a later process that opens the same workspace
//! resolves the same identity and sees the same memory. The model never chooses
//! scope. Model output reaches durable memory only through fact extraction, where
//! every fact carries the confidence it was applied with.

use std::sync::Arc;
use std::time::Duration;

use harness_memory::{
    CreateMemoryAsset, EvidenceState, FACT_CATEGORIES, FACT_KIND, InferredFact, MemoryContribution,
    MemoryIndex, MemoryLayer, MemoryPrincipal, MemoryService, RetrievalState, normalize_terms,
    sanitize_memory_text,
};
use harness_providers::{
    CancellationToken, MessageRole, ModelProvider, ProviderMessage, ProviderRequest,
};
use harness_store_sqlite::{SqliteStore, StoreError};
use harness_types::{
    EventId, HarnessError, MemoryAssetId, MemoryScope, ProjectId, SessionId, SourceAuthority,
    TaskId,
};

use super::paths::LaunchEnvironment;

/// Environment variable that switches chat memory off.
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

/// How much of the model's answer a turn record keeps, in characters.
///
/// The question alone cannot answer "what did I ask you before?", because the question
/// is the half the reader already knows. A turn record carries both halves, and this
/// bounds the one that is model output.
///
/// It used to bound it to 200 characters. That answered "what did I ask" and could not
/// answer anything *about* an answer: the command, the path or the conclusion sat past
/// the cut, and the question was answered from a record that visibly stopped
/// mid-sentence. The bound is now the point past which more text cannot reach the model
/// anyway - one turn's memory budget is 800 tokens, roughly 3200 characters, and a block
/// that does not fit is clipped to its share of that budget. Keeping more would grow the
/// store with text no turn could ever be shown.
const TURN_ANSWER_CHARS: usize = 4000;

/// How many turn records one project keeps, newest first.
///
/// Turn records exist to answer questions about the recent past, so the oldest are the
/// least useful and are retired first. Only turn records are pruned: a directive the
/// user typed is not a log entry and never expires.
const TURN_MEMORY_LIMIT: usize = 200;

/// How many over-cap turn records one turn retires.
///
/// Pruning runs on the path that just admitted a turn, so an unbounded sweep would let a
/// large log delay the answer it belongs to. The cap is reached over a few turns instead
/// of in one, which is the only cost of bounding the work.
const PRUNE_PER_TURN: usize = 8;

/// Marks an asset as a record of one conversation turn.
///
/// `provenance_kind` is the field that says where an asset came from, so it is also
/// what pruning uses to find the log entries it may retire.
const TURN_PROVENANCE: &str = harness_memory::TURN_PROVENANCE_KIND;

/// Whether chat memory runs, given the value of [`MEMORY_VARIABLE`].
///
/// Memory is on by default. Only a value that plainly means "off" turns it off; any
/// other value, including a misspelling, leaves the default, because a switch that
/// cannot be read is not a request to stop. The words that do mean off are the ones a
/// shell user reaches for: `off`, `0`, `false`, `no`.
#[must_use]
pub fn memory_requested(value: Option<&str>) -> bool {
    !matches!(
        value
            .map(|value| value.trim().to_ascii_lowercase())
            .as_deref(),
        Some("off" | "0" | "false" | "no")
    )
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
    workspace_root: &std::path::Path,
    text: &str,
) -> Result<Recall, HarnessError> {
    let service = MemoryService::new(store);
    // A question about the conversation is answered from the log, newest first, instead
    // of by keyword overlap. Measured: "session trước tôi hỏi bạn những gì?" shares one
    // term with the turn record it is asking about, so the overlap floor - which exists
    // to keep unrelated notes out - rejected it. Recentness is what that question is
    // actually about.
    //
    // Everything else is asked of durable memory first and of the log only if durable
    // memory holds nothing. The log quotes the input it recorded, so asking both at once
    // let a turn record shadow the very directive it recorded, and the block that reached
    // the model framed the user's instruction as something they were quoted saying.
    let (index, result) = if asks_about_history(text) {
        (
            MemoryIndex::Log,
            service
                .recent_turns(principal, principal.project_id.as_ref(), RECALL_HITS)
                .await?,
        )
    } else {
        let terms = normalize_terms(text);
        let refresh = service
            .refresh_workspace_file_sources(principal, workspace_root)
            .await?;
        service
            .search_durable_before_log_fresh(principal, &terms, RECALL_HITS, None, &refresh)
            .await?
    };
    let contribution = service.contribute_indexed(principal, &result, CONTRIBUTION_TOKENS, index);
    let hits = result.hits.len();
    let blocks = contribution.blocks.len();
    let message = match result.state {
        RetrievalState::Found => match index {
            MemoryIndex::Durable => format!("memory: {hits} hit(s), {blocks} block(s) injected"),
            // Said out loud, because a log entry is an excerpt of what was said and is
            // retired at the retention cap: the reader should know which one answered.
            MemoryIndex::Log => format!(
                "memory: {hits} hit(s), {blocks} block(s) injected from the conversation log, \
                 not from durable memory"
            ),
        },
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
    /// The turn was recorded, but the sweep that keeps the log bounded failed.
    ///
    /// This is not an error: the turn is durable and recall can read it. It is also not
    /// an ordinary success, because the log is now over its cap and will keep growing
    /// until a sweep works. Reporting it as either one would hide a store that needs
    /// attention.
    StoredButUnpruned {
        /// The turn record that was written.
        asset_id: MemoryAssetId,
        /// What the failed sweep said, in words the transcript can print.
        reason: String,
    },
    /// An asset already held this text; the new source event was recorded on it.
    Duplicate(MemoryAssetId),
    /// This input is not the kind of thing memory keeps. The reason is reported.
    NotKnowledge {
        /// Why it was not stored, in words the transcript can print.
        reason: &'static str,
    },
    /// The turn admitted no input, or the input was blank.
    NothingAdmitted,
    /// The user did not ask for this input to be remembered.
    ///
    /// Silent on purpose: it is what happens on almost every turn. Whatever in the
    /// turn is worth keeping reaches memory through fact extraction instead.
    NotRequested,
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
/// The test is deliberately narrow, and it only applies to a short input: it looks for
/// a question mark at the end of a short input, or a short input that opens with an
/// interrogative *and* ends like a question. Anything else stays knowledge, because
/// dropping a real instruction is the worse of the two mistakes.
///
/// `do`, `have`, `can` and their neighbours open questions and also open ordinary
/// imperatives - "Do not force-push to main", "Have a look at the deploy script". The
/// opening word alone therefore decides nothing; it is only ever a supporting signal
/// beside the ending.
fn classify_input(text: &str) -> Option<&'static str> {
    let trimmed = text.trim();
    let short = trimmed.split_whitespace().count() <= MAX_QUESTION_WORDS;
    if !short {
        // A pasted specification can end in a question mark without being a question
        // aimed at this assistant, and it is certainly worth keeping.
        return None;
    }
    let asks = trimmed.ends_with('?') || trimmed.ends_with('？');
    let opening = trimmed
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase();
    let opening = opening.trim_end_matches(['?', '？', ',', '.', '!']);
    let interrogative = INTERROGATIVE_OPENINGS.contains(&opening);
    // A question mark is decisive on its own. An interrogative opening needs the
    // ending to agree, which is what keeps an imperative that merely starts with one of
    // those words out of the question bucket.
    let closes_like_a_question = ["?", "？", ".", "!", "không", "chứ", "nhỉ", "vậy"]
        .iter()
        .any(|ending| trimmed.to_lowercase().ends_with(ending));
    if asks || (interrogative && closes_like_a_question) {
        return Some("a question is not an instruction");
    }
    None
}

/// Whether the message asks about the conversation itself rather than about a subject.
///
/// Deliberately a short, explicit phrase list and not a classifier: the cost of a false
/// positive is low (the newest turns are injected, which is what a question about the
/// past wants anyway) and the cost of a false negative is the answer being invisible.
/// It must stay narrow enough that an ordinary question about, say, a session cookie
/// does not drag the whole conversation into context.
fn asks_about_history(text: &str) -> bool {
    const PHRASES: &[&str] = &[
        "session trước",
        "session truoc",
        "lần trước",
        "lan truoc",
        "lượt trước",
        "luot truoc",
        "hỏi bạn những gì",
        "hoi ban nhung gi",
        "đã hỏi gì",
        "da hoi gi",
        "nói gì với bạn",
        "noi gi voi ban",
        "previous session",
        "last session",
        "last time",
        "earlier session",
        "what did i ask",
        "what did we discuss",
        "conversation history",
        "chat history",
    ];
    let lowered = text.to_lowercase();
    PHRASES.iter().any(|phrase| lowered.contains(phrase))
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
    // Only what the user asked to be remembered is kept verbatim. Every other input
    // used to be stored as a confirmed instruction that never expires, so one-off
    // requests ("fix this bug for me") piled up as standing rules and were injected
    // into later turns. The durable part of an ordinary turn is extracted as facts.
    let Some(text) = explicit_memory(&text) else {
        return Ok(RememberOutcome::NotRequested);
    };
    // A project-scoped asset needs the project the principal carries; a principal
    // without one can still store what the user typed, at user scope.
    let (scope, project_id) = match principal.project_id.clone() {
        Some(project_id) => (MemoryScope::Project, Some(project_id)),
        None => (MemoryScope::User, None),
    };
    let service = MemoryService::new(Arc::clone(&store));
    // The lookup key must be the text the store will hold. `create_asset` redacts
    // credential-looking lines before it hashes and before it writes the search mirror,
    // so a lookup of the raw text can never match a redacted value - every repeat of
    // such an input minted another asset, which is the duplication this path exists to
    // stop.
    let storable = sanitize_memory_text(&text);
    if let Some(existing) = service.find_active_by_content(principal, &storable).await? {
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
                sources: Vec::new(),
            },
        )
        .await?;
    Ok(RememberOutcome::Stored(asset.asset.memory_asset_id))
}

/// Words that open a request to remember something.
const REMEMBER_OPENINGS: &[&str] = &[
    "hãy ghi nhớ rằng",
    "hãy ghi nhớ là",
    "hãy ghi nhớ",
    "ghi nhớ rằng",
    "ghi nhớ là",
    "ghi nhớ",
    "hãy nhớ rằng",
    "hãy nhớ là",
    "hãy nhớ",
    "nhớ rằng",
    "nhớ là",
    "please remember that",
    "please remember",
    "remember that",
    "remember",
    "note that",
];

/// Words that open a standing instruction, kept as part of what is stored.
const STANDING_OPENINGS: &[&str] = &[
    "từ giờ",
    "từ nay",
    "từ bây giờ",
    "luôn luôn",
    "luôn",
    "đừng bao giờ",
    "không bao giờ",
    "from now on",
    "always",
    "never",
    "going forward",
];

/// The input, if the user asked for it to be remembered.
///
/// Two shapes count: "ghi nhớ: X" / "remember that X", and a standing rule such as
/// "từ giờ luôn X" / "from now on, X". Anything else is not a request to remember,
/// however imperative it sounds.
///
/// The text is kept whole, opening included. The opening is the word a later question
/// uses to find it - "what did I ask you to remember?" - and cutting it off left the
/// question one shared term short of the overlap floor.
fn explicit_memory(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let lowered = trimmed.to_lowercase();
    let starts_with_word = |opening: &str| {
        lowered.starts_with(opening)
            && lowered[opening.len()..]
                .chars()
                .next()
                .is_none_or(|next| !next.is_alphanumeric())
    };
    let asked = REMEMBER_OPENINGS.iter().any(|opening| {
        starts_with_word(opening)
            // "Remember" alone asks for nothing to be kept.
            && lowered[opening.len()..]
                .trim_matches(|character: char| !character.is_alphanumeric())
                .chars()
                .next()
                .is_some()
    });
    let standing = STANDING_OPENINGS
        .iter()
        .any(|opening| starts_with_word(opening));
    (asked || standing).then(|| trimmed.to_owned())
}

/// The extractor version recorded on every fact it writes.
pub const EXTRACTOR_VERSION: &str = "turn-facts-v1";

/// How long one extraction may take before the turn stops waiting for it.
const EXTRACTION_TIMEOUT: Duration = Duration::from_secs(20);

/// The output bound of one extraction call.
const EXTRACTION_MAX_OUTPUT_TOKENS: u32 = 800;

/// How many facts one turn may add.
const EXTRACTION_MAX_FACTS: usize = 8;

/// The longest fact accepted, in characters. A fact is one sentence; a paragraph
/// is the model restating the answer.
const FACT_MAX_CHARS: usize = 400;

/// How many known facts the extractor is shown so it can skip or replace them.
const KNOWN_FACTS: usize = 12;

/// How much of each side of the turn the extractor reads.
const EXTRACTION_TURN_CHARS: usize = 4000;

/// What one extraction did.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExtractionReport {
    /// Facts written as active: they reach the next turn.
    pub applied: usize,
    /// Facts written as candidates: below the auto-apply confidence.
    pub candidates: usize,
    /// Facts memory already held; the new source was recorded on them.
    pub duplicates: usize,
    /// Known facts retired because a new fact replaced them.
    pub replaced: usize,
    /// Facts refused: malformed, unknown category, too long or credential-like.
    pub refused: usize,
    /// Why nothing ran, when nothing ran.
    pub skipped: Option<&'static str>,
}

impl ExtractionReport {
    /// One transcript line, or none when there is nothing worth saying.
    #[must_use]
    pub fn message(&self) -> Option<String> {
        if self.applied + self.candidates + self.replaced + self.refused == 0 {
            return None;
        }
        let mut parts = vec![format!("learned {} fact(s)", self.applied)];
        if self.candidates > 0 {
            parts.push(format!(
                "{} kept for review (`ha memory candidates`)",
                self.candidates
            ));
        }
        if self.replaced > 0 {
            parts.push(format!("{} outdated fact(s) replaced", self.replaced));
        }
        if self.refused > 0 {
            parts.push(format!("{} refused", self.refused));
        }
        Some(format!("memory: {}", parts.join(", ")))
    }
}

#[derive(serde::Deserialize)]
struct ExtractionReply {
    #[serde(default)]
    facts: Vec<ReplyFact>,
}

/// One fact as the extractor wrote it.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct ReplyFact {
    #[serde(default)]
    content: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    confidence: f64,
    #[serde(default)]
    replaces: Option<String>,
    /// Which turn of the batch it came from, 1-based; the last one when absent.
    #[serde(default)]
    turn: Option<usize>,
}

/// One finished turn waiting for extraction, captured while its store was open.
///
/// Everything the extractor needs from the turn is copied here, so the work can run
/// after the turn has released the store and the next turn has taken it.
#[derive(Clone, Debug)]
pub struct QueuedTurn {
    /// Who the facts from this turn belong to.
    pub principal: MemoryPrincipal,
    /// The admitted input the facts are sourced from.
    pub input_event: EventId,
    /// What the user asked.
    pub question: String,
    /// What the model answered.
    pub answer: String,
}

/// Capture one finished turn for extraction, or `None` when there is nothing to learn.
///
/// # Errors
/// Fails when the store cannot be read.
pub async fn queued_turn(
    store: &SqliteStore,
    principal: &MemoryPrincipal,
    session_id: &SessionId,
    answer: &str,
) -> Result<Option<QueuedTurn>, HarnessError> {
    let Some((input_event, question)) = store
        .session_admitted_input(session_id)
        .await
        .map_err(StoreError::into_harness_error)?
    else {
        return Ok(None);
    };
    let question = question.trim();
    let answer = answer.trim();
    if question.is_empty() || answer.is_empty() {
        return Ok(None);
    }
    Ok(Some(QueuedTurn {
        principal: principal.clone(),
        input_event,
        question: question.to_owned(),
        answer: answer.to_owned(),
    }))
}

/// Extract the facts one turn established and write them to memory.
///
/// This is the L0 → L1 step TencentDB-Agent-Memory runs on every conversation and
/// deer-flow runs after every turn: the model that just answered reads the turn and
/// states what is worth knowing next time - a preference, a decision, a convention, a
/// correction - with how sure it is. A fact at or above the auto-apply confidence is
/// active at once; one below it is a candidate the user can confirm or reject.
///
/// The interactive app does not call this on the turn's path: it queues the turn for
/// [`super::memory_worker::ExtractionWorker`], which runs the same three phases -
/// [`known_facts_for`], [`ask_for_facts`], [`apply_facts`] - in the background and for
/// several turns at once. This single-turn form is what a caller that already holds a
/// writer, such as a test, uses.
///
/// # Errors
/// Fails when the store cannot be read or written. A model that is unavailable, slow
/// or answers with something other than the requested JSON is a skipped extraction,
/// reported in the result, not an error: the turn it follows already succeeded.
pub async fn extract_facts(
    provider: Arc<dyn ModelProvider>,
    store: Arc<SqliteStore>,
    principal: &MemoryPrincipal,
    session_id: &SessionId,
    answer: &str,
) -> Result<ExtractionReport, HarnessError> {
    let Some(turn) = queued_turn(&store, principal, session_id, answer).await? else {
        return Ok(ExtractionReport {
            skipped: Some("nothing was said"),
            ..ExtractionReport::default()
        });
    };
    let turns = [turn];
    let known = known_facts_for(Arc::clone(&store), &turns).await?;
    match ask_for_facts(provider.as_ref(), &known, &turns).await {
        Ok(facts) => apply_facts(store, &turns, &known, facts).await,
        Err(reason) => Ok(ExtractionReport {
            skipped: Some(reason),
            ..ExtractionReport::default()
        }),
    }
}

/// Facts memory already holds that relate to these turns, as (id, content).
///
/// The model is shown them so it can leave a known fact out or name the one a new
/// fact replaces. It can only replace a fact from this list.
///
/// # Errors
/// Fails when the store cannot be searched.
pub async fn known_facts_for(
    store: Arc<SqliteStore>,
    turns: &[QueuedTurn],
) -> Result<Vec<(MemoryAssetId, String)>, HarnessError> {
    let Some(last) = turns.last() else {
        return Ok(Vec::new());
    };
    let questions = turns
        .iter()
        .map(|turn| turn.question.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    known_facts(&MemoryService::new(store), &last.principal, &questions).await
}

/// Ask the model for the facts these turns established.
///
/// # Errors
/// The error is the reason the extraction was skipped: a fixture provider, a model
/// that failed or took too long, or a reply that is not the requested JSON.
pub async fn ask_for_facts(
    provider: &dyn ModelProvider,
    known: &[(MemoryAssetId, String)],
    turns: &[QueuedTurn],
) -> Result<Vec<ReplyFact>, &'static str> {
    if provider.capabilities().fixture {
        return Err("fixture providers do not extract");
    }
    if turns.is_empty() {
        return Err("nothing was said");
    }
    let reply = ask_extractor(provider, extraction_prompt(known, turns)).await?;
    parse_reply(&reply)
        .map(|reply| reply.facts)
        .ok_or("the model did not answer with the requested JSON")
}

/// Write the facts the model returned for these turns.
///
/// # Errors
/// Fails when the store cannot be written.
pub async fn apply_facts(
    store: Arc<SqliteStore>,
    turns: &[QueuedTurn],
    known: &[(MemoryAssetId, String)],
    facts: Vec<ReplyFact>,
) -> Result<ExtractionReport, HarnessError> {
    let mut report = ExtractionReport::default();
    let service = MemoryService::new(store);
    for fact in facts
        .into_iter()
        .take(EXTRACTION_MAX_FACTS * turns.len().max(1))
    {
        let content = fact.content.trim().to_owned();
        let category = fact.category.trim().to_lowercase();
        // A fact names its turn by number; an absent or impossible number is the
        // last turn, which is the one the batch was flushed for.
        let Some(turn) = fact
            .turn
            .and_then(|turn| turn.checked_sub(1))
            .and_then(|index| turns.get(index))
            .or_else(|| turns.last())
        else {
            break;
        };
        if content.is_empty()
            || content.chars().count() > FACT_MAX_CHARS
            || !FACT_CATEGORIES.contains(&category.as_str())
            || !fact.confidence.is_finite()
            || !(0.0..=1.0).contains(&fact.confidence)
            // A fact the store would redact is a fact about a credential; keeping the
            // redacted stub would remember nothing.
            || sanitize_memory_text(&content) != content
        {
            report.refused += 1;
            continue;
        }
        let principal = &turn.principal;
        let stored_text = format!("[{category}] {content}");
        if let Some(existing) = service.find_any_by_content(principal, &stored_text).await? {
            service
                .append_version_source(principal, &existing, &turn.input_event)
                .await?;
            report.duplicates += 1;
            continue;
        }
        let (scope, project_id) = match principal.project_id.clone() {
            Some(project_id) => (MemoryScope::Project, Some(project_id)),
            None => (MemoryScope::User, None),
        };
        let written = service
            .apply_inferred_fact(
                principal,
                InferredFact {
                    content,
                    category,
                    confidence: fact.confidence,
                    scope,
                    project_id,
                    source_event_refs: vec![turn.input_event.clone()],
                    extractor_version: EXTRACTOR_VERSION.to_owned(),
                },
            )
            .await?;
        let active = written.asset.status == harness_types::MemoryAssetStatus::Active;
        if active {
            report.applied += 1;
        } else {
            report.candidates += 1;
        }
        // Only an active fact replaces one: a candidate is a guess, and a guess must
        // not retire something memory is using.
        if active
            && let Some(replaced) = fact
                .replaces
                .as_deref()
                .and_then(|id| known.iter().find(|(known_id, _)| known_id.as_str() == id))
        {
            service
                .invalidate(
                    principal,
                    &replaced.0,
                    &format!("replaced by {}", written.asset.memory_asset_id),
                )
                .await?;
            report.replaced += 1;
        }
    }
    Ok(report)
}

/// One bounded extraction call; the error is the reason it was skipped.
async fn ask_extractor(
    provider: &dyn ModelProvider,
    prompt: String,
) -> Result<String, &'static str> {
    let request = ProviderRequest::new(
        harness_types::RequestId::generate(),
        provider.capabilities().model,
        vec![ProviderMessage::new(MessageRole::User, prompt)],
    )
    .with_max_output_tokens(EXTRACTION_MAX_OUTPUT_TOKENS);
    let events = tokio::time::timeout(
        EXTRACTION_TIMEOUT,
        provider.stream(request, CancellationToken::new()),
    )
    .await
    .map_err(|_| "the model did not answer in time")?
    .map_err(|_| "the model was unavailable")?;
    harness_providers::assemble_stream(&events)
        .map(|response| response.text)
        .map_err(|_| "the extraction reply could not be read")
}

/// Facts memory already holds that relate to this text, as (id, content).
async fn known_facts(
    service: &MemoryService,
    principal: &MemoryPrincipal,
    question: &str,
) -> Result<Vec<(MemoryAssetId, String)>, HarnessError> {
    let terms = normalize_terms(question);
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let result = service
        .search_terms(principal, &terms, KNOWN_FACTS, None)
        .await?;
    Ok(result
        .hits
        .into_iter()
        .filter(|hit| hit.asset.kind == FACT_KIND)
        .map(|hit| (hit.asset.memory_asset_id, hit.current.content))
        .collect())
}

fn extraction_prompt(known: &[(MemoryAssetId, String)], turns: &[QueuedTurn]) -> String {
    let known = if known.is_empty() {
        "(none)".to_owned()
    } else {
        known
            .iter()
            .map(|(id, content)| format!("- {id}: {content}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let conversation = turns
        .iter()
        .enumerate()
        .map(|(index, turn)| {
            format!(
                "Turn {number}:\nUser: {question}\nAssistant: {answer}",
                number = index + 1,
                question = clip(&turn.question, EXTRACTION_TURN_CHARS),
                answer = clip(&turn.answer, EXTRACTION_TURN_CHARS),
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    format!(
        "You maintain long-term memory for a coding assistant. Read the conversation turns \
         below and extract the facts worth knowing in FUTURE conversations about this user \
         or this project: stable preferences, conventions, decisions, environment details, \
         goals, and corrections the user made.\n\
         Do NOT extract: one-off task requests, a restatement of an answer, anything only \
         true for one turn, guesses, secrets or credentials.\n\
         Each fact is one self-contained sentence in the user's language.\n\
         confidence: 0.9-1.0 the user stated it explicitly; 0.7-0.9 clearly implied; below \
         0.7 uncertain.\n\
         category: one of {categories}.\n\
         turn: the number of the turn the fact comes from.\n\
         If a fact is already known, leave it out. If a fact updates or contradicts a known \
         fact, set \"replaces\" to that fact's id; otherwise null. If a later turn corrects \
         an earlier one, keep only the corrected fact.\n\
         Reply with JSON only, no prose: \
         {{\"facts\":[{{\"content\":\"...\",\"category\":\"preference\",\"confidence\":0.9,\"turn\":1,\"replaces\":null}}]}}. \
         Reply {{\"facts\":[]}} when nothing qualifies.\n\n\
         Known facts:\n{known}\n\n\
         {conversation}",
        categories = FACT_CATEGORIES.join(", "),
    )
}

/// The JSON object in a reply, tolerating a code fence or a sentence around it.
fn parse_reply(reply: &str) -> Option<ExtractionReply> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    serde_json::from_str(reply.get(start..=end)?).ok()
}

/// Store one completed turn: what was asked, and the beginning of what was answered.
///
/// Without this, "what did I ask you in the previous session?" cannot be answered by
/// memory at all. The store held the user's directives and never the conversation, so
/// retrieval found the question being asked rather than any answer to it - measured on
/// a real session, where the agent correctly reported that no earlier question content
/// had been kept.
///
/// Provenance is stated exactly as far as it goes. The admission event is a durable
/// reference for the turn: it is proof the question was really asked, which is what
/// `VerifiedObservation` requires and what makes the asset publishable at L1. The
/// answer line is an excerpt of the model's reply, copied from the turn in memory; the
/// runtime did not independently confirm the claim inside it, so it is quoted as text
/// and never promoted to evidence of its own.
///
/// # Errors
/// Fails when the store cannot write. A turn that could not be recorded is reported to
/// the caller, not swallowed.
pub async fn remember_turn(
    store: Arc<SqliteStore>,
    principal: &MemoryPrincipal,
    session_id: &SessionId,
    answer: &str,
) -> Result<RememberOutcome, HarnessError> {
    let Some((input_event, question)) = store
        .session_admitted_input(session_id)
        .await
        .map_err(StoreError::into_harness_error)?
    else {
        return Ok(RememberOutcome::NothingAdmitted);
    };
    let question = question.trim();
    if question.is_empty() {
        return Ok(RememberOutcome::NothingAdmitted);
    }
    // The store redacts any line that looks like it carries a secret, and it does so
    // before writing the search mirror. A record whose `asked:` line is redacted has
    // lost the one thing it exists to keep, so it is not written at all rather than
    // written as a stub that answers nothing.
    if sanitize_memory_text(question) != question {
        return Ok(RememberOutcome::NotKnowledge {
            reason: "the question looks like it carries a credential, so the turn is not recorded",
        });
    }
    let (scope, project_id) = match principal.project_id.clone() {
        Some(project_id) => (MemoryScope::Project, Some(project_id)),
        None => (MemoryScope::User, None),
    };
    let mut content = format!("asked: {question}\nsession: {session_id}");
    let answer = answer.trim();
    if !answer.is_empty() {
        let answer = clip(answer, TURN_ANSWER_CHARS);
        content.push_str("\nanswered: ");
        content.push_str(&answer);
    }
    let prune_scope = project_id.clone();
    let service = MemoryService::new(Arc::clone(&store));
    let asset = service
        .create_asset(
            principal,
            CreateMemoryAsset {
                kind: "session_turn".to_owned(),
                scope,
                layer: MemoryLayer::L1,
                project_id,
                task_id: None,
                agent_profile_id: None,
                session_id: None,
                visibility: "scoped".to_owned(),
                content,
                // The turn happened: the runtime observed the question arrive and the
                // answer go out. That is what `RuntimeObserved` records, and it is also
                // why this cannot be a candidate - a candidate is not injectable at all,
                // so recording it as one made the whole feature invisible. Measured:
                // `recent_turns` returned the record and `validate_memory_snapshot`
                // refused it, and the runtime then dropped the memory in silence.
                //
                // What this does **not** claim is that the answer is true. `answered:` is
                // an unverified excerpt of model output, quoted so a later turn can see
                // what was said and never promoted to evidence of its own. The block that
                // reaches the model says so in its heading, because a reply that is read
                // as established fact would harden into durable knowledge.
                authority: SourceAuthority::RuntimeObserved,
                evidence: EvidenceState::VerifiedObservation,
                user_confirmed: false,
                source_event_refs: vec![input_event],
                source_file_hashes: Vec::new(),
                source_commit: None,
                provenance_kind: TURN_PROVENANCE.to_owned(),
                sources: Vec::new(),
            },
        )
        .await?;
    let id = asset.asset.memory_asset_id.clone();
    // The record is durable by now, so a sweep that fails does not undo the turn and must
    // not be reported as if the turn had failed. It is still reported: a log over its cap
    // is a fact the operator can act on, and swallowing it here is how the cap silently
    // stopped applying before.
    match prune_turns(
        &service,
        principal,
        prune_scope.as_ref(),
        TURN_MEMORY_LIMIT,
        PRUNE_PER_TURN,
    )
    .await
    {
        Ok(sweep) if sweep.pinned == 0 => Ok(RememberOutcome::Stored(id)),
        Ok(sweep) => Ok(RememberOutcome::StoredButUnpruned {
            asset_id: id,
            reason: format!(
                "retired {} record(s); {} over-cap record(s) are the source of another memory \
                 and cannot be retired without taking it down too",
                sweep.retired, sweep.pinned
            ),
        }),
        Err(error) => Ok(RememberOutcome::StoredButUnpruned {
            asset_id: id,
            reason: error.to_string(),
        }),
    }
}

/// What one retention sweep found and did.
///
/// `pinned` is not a failure: those records are over the cap and are load-bearing, because
/// another live asset was derived from them. It is carried out of the sweep so the caller
/// can report it - a cap that quietly stops applying is how the log grew without bound
/// before, and a cap that quietly deletes knowledge would be worse.
struct RetentionSweep {
    /// How many over-cap records were retired this sweep.
    retired: usize,
    /// How many over-cap records are sources of another live asset and stayed.
    pinned: usize,
}

/// Retire the oldest turn records past the cap, at most `batch` of them.
///
/// Only assets this code wrote as turn records are eligible, so a directive is never
/// pruned by a log limit. A retirement that fails is reported: silently keeping an
/// unbounded log would be worse than an error.
///
/// `keep` and `batch` are parameters rather than the constants directly so a test can
/// reach the boundary without writing two hundred turns.
async fn prune_turns(
    service: &MemoryService,
    principal: &MemoryPrincipal,
    project_id: Option<&ProjectId>,
    keep: usize,
    batch: usize,
) -> Result<RetentionSweep, HarnessError> {
    let (mut excess, pinned) = service
        .turn_records_over_limit(principal, project_id, keep)
        .await?;
    // Bounded work: the oldest go first, and whatever is left over the cap is retired by
    // the turns that follow.
    excess.truncate(batch);
    let retired = excess.len();
    for id in excess {
        service
            .invalidate(principal, &id, "turn record retired past the retention cap")
            .await?;
    }
    Ok(RetentionSweep { retired, pinned })
}

/// Shorten text to at most `limit` characters, counting characters rather than bytes.
fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let head: String = text.chars().take(limit).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::{
        MEMORY_VARIABLE, RECALL_HITS, RetrievalState, asks_about_history, memory_requested,
        memory_requested_from_environment, principal, prune_turns, recall, remember_input,
        remember_turn,
    };
    use crate::interactive::paths::LaunchEnvironment;
    use crate::interactive::project::resolve_project_id;
    use harness_memory::{
        CreateMemoryAsset, EvidenceState, MemoryIndex, MemoryLayer, MemoryPrincipal, MemoryService,
        MemorySource, SelectionReason, normalize_terms,
    };
    use harness_providers::MockProvider;
    use harness_runtime::{RunRequest, RuntimeConfig, RuntimeService};
    use harness_session::{AdmitInputRequest, SessionService};
    use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
    use harness_tools::observe_workspace;
    use harness_types::{
        ContentHash, HostId, InputId, MemoryScope, ProjectId, SessionId, SourceAuthority, TaskId,
    };
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
        let recalled = recall(
            Arc::clone(&fixture.store),
            &later,
            &fixture.workspace,
            asked,
        )
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

    /// A turn record is written, is found by the history path, and carries both halves.
    #[tokio::test]
    async fn memory_a_previous_turn_is_recorded_and_found_by_a_later_session() {
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
            "Giải thích ngắn gọn memory dài hạn là gì.",
        )
        .await;

        let outcome = remember_turn(
            Arc::clone(&fixture.store),
            &principal(project_id.clone(), task_id.clone(), first.clone()),
            &first,
            "Memory dài hạn lưu những gì bền vững qua các session.",
        )
        .await
        .expect("remember_turn runs");
        let asset_id = match outcome {
            super::RememberOutcome::Stored(asset_id) => asset_id,
            other => panic!("a turn is recorded: {other:?}"),
        };

        // The record says what was asked and what was answered, so a question about the
        // conversation has something to answer with.
        let service = MemoryService::new(Arc::clone(&fixture.store));
        let stored = service
            .read(
                &principal(project_id.clone(), task_id.clone(), SessionId::generate()),
                &asset_id,
            )
            .await
            .expect("read runs")
            .expect("the record exists");
        assert!(
            stored
                .current
                .content
                .contains("asked: Giải thích ngắn gọn"),
            "{}",
            stored.current.content
        );
        assert!(
            stored
                .current
                .content
                .contains("answered: Memory dài hạn lưu"),
            "{}",
            stored.current.content
        );
        assert_eq!(
            stored.current.record.evidence_state, "verified_observation",
            "the runtime observed the turn happen"
        );
        assert_eq!(
            stored.asset.created_by,
            SourceAuthority::RuntimeObserved,
            "the runtime is the author of the record; the model authored only the quote"
        );
        assert_eq!(
            format!("{:?}", stored.asset.status),
            "Active",
            "a candidate is not injectable at all: validate_memory_snapshot refuses it \
             and the runtime then drops the memory in silence"
        );

        // And the history the user asks about is what a later session reads.
        let recent = service
            .recent_turns(
                &principal(project_id, task_id, SessionId::generate()),
                Some(
                    &resolve_project_id(&fixture.store, &fixture.workspace)
                        .await
                        .expect("project identity"),
                ),
                8,
            )
            .await
            .expect("recent turns");
        assert_eq!(recent.hits.len(), 1, "the turn is the newest record");
        assert!(
            recent.hits[0]
                .current
                .content
                .contains("Giải thích ngắn gọn")
        );
        assert!(
            recent.revision > 0,
            "the revision is what dispatch revalidates against; a placeholder here \
             makes the whole contribution be dropped in silence"
        );
    }

    /// The log is bounded, and the bound never touches a directive.
    ///
    /// Turn records exist to answer questions about the recent past, so the oldest go
    /// first. A directive the user typed is not a log entry: retiring it because the
    /// conversation got long would delete knowledge the user asked to keep.
    #[tokio::test]
    async fn memory_the_turn_log_is_bounded_and_a_directive_is_never_retired() {
        let fixture = fixture().await;
        let project_id = resolve_project_id(&fixture.store, &fixture.workspace)
            .await
            .expect("project identity");

        // One directive, which must survive the pruning below.
        let task_id = TaskId::generate();
        let session = SessionId::generate();
        admit(
            &fixture,
            &project_id,
            &session,
            &task_id,
            "Always run cargo test before you commit",
        )
        .await;
        remember_input(
            Arc::clone(&fixture.store),
            &principal(project_id.clone(), task_id, session.clone()),
            &session,
        )
        .await
        .expect("remember runs")
        .expect_stored();

        // Four turns, kept two at a time.
        let mut turn_ids = Vec::new();
        for index in 0..4 {
            let task_id = TaskId::generate();
            let session = SessionId::generate();
            admit(
                &fixture,
                &project_id,
                &session,
                &task_id,
                &format!("question number {index}"),
            )
            .await;
            let owner = principal(project_id.clone(), task_id, session.clone());
            let outcome = remember_turn(
                Arc::clone(&fixture.store),
                &owner,
                &session,
                &format!("answer number {index}"),
            )
            .await
            .expect("remember_turn runs");
            let id = match outcome {
                super::RememberOutcome::Stored(id) => id,
                other => panic!("a turn is recorded: {other:?}"),
            };
            turn_ids.push(id);
            let service = MemoryService::new(Arc::clone(&fixture.store));
            prune_turns(&service, &owner, Some(&project_id), 2, 8)
                .await
                .expect("pruning runs");
        }

        let service = MemoryService::new(Arc::clone(&fixture.store));
        let reader = principal(
            project_id.clone(),
            TaskId::generate(),
            SessionId::generate(),
        );
        let recent = service
            .recent_turns(&reader, Some(&project_id), 8)
            .await
            .expect("recent turns");
        assert_eq!(
            recent.hits.len(),
            2,
            "the log holds the cap, not every turn ever taken"
        );
        let kept = recent
            .hits
            .iter()
            .map(|hit| hit.current.content.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            kept.contains("question number 3") && !kept.contains("question number 0"),
            "the newest survive and the oldest retire: {kept}"
        );

        // The directive is still there, and still searchable.
        let directive = service
            .search(&reader, "cargo test", 8, None)
            .await
            .expect("search runs");
        assert!(
            directive
                .hits
                .iter()
                .any(|hit| hit.current.content.contains("Always run cargo test")),
            "a retention cap for the log must not expire what the user asked to keep"
        );
    }

    /// One turn retires a bounded number of records, not the whole backlog.
    ///
    /// Pruning runs on the path that answers a turn, so an unbounded sweep turns a long
    /// log into a long wait before the answer the user is waiting for. The cap is reached
    /// over the turns that follow - but only if the sweep really does progress, which is
    /// the other half of this test.
    #[tokio::test]
    async fn memory_one_sweep_retires_a_bounded_batch_and_still_reaches_the_cap() {
        let fixture = fixture().await;
        let project_id = resolve_project_id(&fixture.store, &fixture.workspace)
            .await
            .expect("project identity");
        let service = MemoryService::new(Arc::clone(&fixture.store));

        // Six turns, kept two at a time, retired one per sweep.
        let mut owner = None;
        for index in 0..6 {
            let task_id = TaskId::generate();
            let session = SessionId::generate();
            admit(
                &fixture,
                &project_id,
                &session,
                &task_id,
                &format!("question number {index}"),
            )
            .await;
            let turn_owner = principal(project_id.clone(), task_id, session.clone());
            remember_turn(
                Arc::clone(&fixture.store),
                &turn_owner,
                &session,
                &format!("answer number {index}"),
            )
            .await
            .expect("remember_turn runs");
            owner = Some(turn_owner);
        }
        let owner = owner.expect("six turns ran");
        let reader = principal(
            project_id.clone(),
            TaskId::generate(),
            SessionId::generate(),
        );
        let count = async || {
            service
                .recent_turns(&reader, Some(&project_id), 32)
                .await
                .expect("recent turns")
                .hits
                .len()
        };
        assert_eq!(count().await, 6, "all six turns are recorded to begin with");

        prune_turns(&service, &owner, Some(&project_id), 2, 1)
            .await
            .expect("pruning runs");
        assert_eq!(
            count().await,
            5,
            "one sweep retires one record when the batch is one - work on the answer path \
             must not scale with the length of the log"
        );

        // Repeated sweeps converge on the cap rather than stalling above it.
        for _ in 0..3 {
            prune_turns(&service, &owner, Some(&project_id), 2, 1)
                .await
                .expect("pruning runs");
        }
        assert_eq!(count().await, 2, "the sweeps reach the cap");
        prune_turns(&service, &owner, Some(&project_id), 2, 1)
            .await
            .expect("pruning runs");
        assert_eq!(count().await, 2, "and never retire past it");
    }

    /// An ordinary question must not drag the whole conversation into context.
    ///
    /// The recency path exists for questions about the conversation. A question about a
    /// session *cookie*, a *previous* release, or "last time" in another sense must go
    /// down the keyword path, or every turn would carry the recent log and bury the note
    /// that actually answers it.
    #[test]
    fn only_a_question_about_the_conversation_takes_the_history_path() {
        for asking in [
            "session trước tôi hỏi bạn những gì?",
            "lần trước bạn nói gì với tôi?",
            "What did I ask you in the previous session?",
            "what did we discuss last time?",
        ] {
            assert!(
                asks_about_history(asking),
                "{asking:?} is about the conversation"
            );
        }
        for other in [
            "how does the session cookie expire?",
            "explain the previous release notes",
            "what did this function do before the refactor?",
            "sửa lỗi trong file session.rs",
            "commit gần nhất là gì?",
        ] {
            assert!(
                !asks_about_history(other),
                "{other:?} is about a subject, not about our conversation"
            );
        }
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
            "remember that the deploy script is kept beside the fireplace",
            // Shares two: `notes` and `drawer`.
            "remember that the notes are kept in the bottom drawer",
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
        let recalled = recall(
            Arc::clone(&fixture.store),
            &later,
            &fixture.workspace,
            question,
        )
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
    fn memory_is_on_unless_a_value_plainly_turns_it_off() {
        assert!(memory_requested(None), "on by default");
        assert!(memory_requested(Some("on")));
        assert!(memory_requested(Some("")));
        assert!(
            memory_requested(Some("of")),
            "a misspelling is not a request to stop"
        );
        for off in ["off", "OFF", " off ", "0", "false", "no"] {
            assert!(!memory_requested(Some(off)), "{off:?} turns memory off");
        }

        let unrelated = LaunchEnvironment::from_pairs([("HA_UI", "plain")]);
        assert!(memory_requested_from_environment(&unrelated));
        let off = LaunchEnvironment::from_pairs([(MEMORY_VARIABLE, "off")]);
        assert!(!memory_requested_from_environment(&off));
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
                "ha".to_owned(),
                "ban".to_owned(),
            ],
            "every word that can carry the subject is a term; a function word such as              \"gì\" is not, because it matches any other question"
        );
        assert!(
            terms.len() > 4,
            "keeping only the four longest terms is the behaviour that failed"
        );
    }

    /// A turn record that another memory was built on is pinned, not retired.
    ///
    /// The retention cap exists for a log. A record something else derives from is no
    /// longer only a log entry: retiring it invalidates the derived asset too, one hop
    /// later, with nothing in the transcript to connect the two. This is the legacy case -
    /// the edge is written straight into the store the way a build without the guard would
    /// have written it - because that is exactly the data the sweep still has to survive.
    #[tokio::test]
    async fn memory_a_turn_record_another_memory_depends_on_is_not_retired() {
        let fixture = fixture().await;
        let project_id = resolve_project_id(&fixture.store, &fixture.workspace)
            .await
            .expect("project identity");
        let service = MemoryService::new(Arc::clone(&fixture.store));

        let mut turn_ids = Vec::new();
        let mut owner = None;
        for index in 0..3 {
            let task_id = TaskId::generate();
            let session = SessionId::generate();
            admit(
                &fixture,
                &project_id,
                &session,
                &task_id,
                &format!("question number {index}"),
            )
            .await;
            let turn_owner = principal(project_id.clone(), task_id, session.clone());
            let outcome = remember_turn(
                Arc::clone(&fixture.store),
                &turn_owner,
                &session,
                &format!("answer number {index}"),
            )
            .await
            .expect("remember_turn runs");
            turn_ids.push(match outcome {
                super::RememberOutcome::Stored(id) => id,
                other => panic!("a turn is recorded: {other:?}"),
            });
            owner = Some(turn_owner);
        }
        let owner = owner.expect("three turns ran");

        // A live asset that names the oldest turn as its source.
        let derived = service
            .create_asset(
                &owner,
                CreateMemoryAsset {
                    kind: "project_fact".to_owned(),
                    scope: super::MemoryScope::Project,
                    layer: MemoryLayer::L1,
                    project_id: Some(project_id.clone()),
                    task_id: None,
                    agent_profile_id: None,
                    session_id: None,
                    visibility: "scoped".to_owned(),
                    content: "a summary built on the oldest turn".to_owned(),
                    authority: SourceAuthority::RuntimeObserved,
                    evidence: EvidenceState::VerifiedObservation,
                    user_confirmed: false,
                    source_event_refs: Vec::new(),
                    source_file_hashes: Vec::new(),
                    source_commit: Some("legacy-edge-fixture".to_owned()),
                    provenance_kind: "runtime_observation".to_owned(),
                    sources: Vec::new(),
                },
            )
            .await
            .expect("derived asset is written");
        let edge = link_dependency(
            &fixture.store,
            derived.asset.memory_asset_id.as_str(),
            turn_ids[0].as_str(),
        )
        .await;
        assert!(edge, "the legacy dependency edge is written");

        let sweep = prune_turns(&service, &owner, Some(&project_id), 2, 8)
            .await
            .expect("pruning runs");
        assert_eq!(
            (sweep.retired, sweep.pinned),
            (0, 1),
            "the oldest record is over the cap and pinned: retiring it would take the \
             derived asset down with it"
        );

        // The pinned record is still readable, and so is what was built on it.
        let reader = principal(
            project_id.clone(),
            TaskId::generate(),
            SessionId::generate(),
        );
        let recent = service
            .recent_turns(&reader, Some(&project_id), 8)
            .await
            .expect("recent turns");
        assert!(
            recent
                .hits
                .iter()
                .any(|hit| hit.current.content.contains("question number 0")),
            "the pinned record stays: {:#?}",
            recent.hits.len()
        );
        assert!(
            service
                .read(&reader, &derived.asset.memory_asset_id)
                .await
                .expect("read runs")
                .is_some(),
            "the asset built on it must not be retired by a log limit"
        );
    }

    /// Nothing durable may be built on a turn record, and the refusal says why.
    ///
    /// A turn record is the one asset guaranteed to expire. Accepting it as a source is
    /// how a summary becomes a memory that disappears two hundred turns later with no
    /// event that explains the loss.
    #[tokio::test]
    async fn memory_a_turn_record_cannot_become_a_source_of_durable_memory() {
        let fixture = fixture().await;
        let project_id = resolve_project_id(&fixture.store, &fixture.workspace)
            .await
            .expect("project identity");
        let task_id = TaskId::generate();
        let session = SessionId::generate();
        admit(
            &fixture,
            &project_id,
            &session,
            &task_id,
            "question number 0",
        )
        .await;
        let owner = principal(project_id.clone(), task_id, session.clone());
        let service = MemoryService::new(Arc::clone(&fixture.store));
        let turn = remember_turn(
            Arc::clone(&fixture.store),
            &owner,
            &session,
            "answer number 0",
        )
        .await
        .expect("remember_turn runs");
        let turn_id = match turn {
            super::RememberOutcome::Stored(id) => id,
            other => panic!("a turn is recorded: {other:?}"),
        };

        let error = service
            .derive_l2(
                &owner,
                &[harness_types::MemoryVersionRef {
                    memory_asset_id: turn_id.clone(),
                    version: 1,
                }],
                "a summary of one turn",
            )
            .await
            .expect_err("a log entry is not a durable source");
        assert!(
            error.to_string().contains("log entry that expires"),
            "the refusal explains itself: {error}"
        );
    }

    /// Write one legacy dependency row straight into the store.
    ///
    /// Returns false when the store is not reachable, so a test can fail on the fact rather
    /// than on a panic inside a helper.
    async fn link_dependency(store: &SqliteStore, derived: &str, source: &str) -> bool {
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&store.paths().database_path)
            .create_if_missing(false);
        let Ok(pool) = sqlx::SqlitePool::connect_with(options).await else {
            return false;
        };
        let written = sqlx::query(
            "INSERT INTO memory_dependencies(derived_asset_id, derived_version, source_kind, \
             source_id, source_version) VALUES (?, 1, 'asset', ?, 1)",
        )
        .bind(derived)
        .bind(source)
        .execute(&pool)
        .await
        .is_ok();
        pool.close().await;
        written
    }

    /// A directive is never displaced by the log entry of the turn that recorded it.
    ///
    /// The turn record holds the input verbatim plus part of the answer, so it overlaps the
    /// directive almost completely. Asked as one index, the two compete on a bm25
    /// tie-break, and the block that reached the model was sometimes the log entry - the
    /// user's instruction arriving framed as something they were quoted saying, and
    /// expiring at the retention cap. The knowledge path asks durable memory first now, so
    /// the answer is the directive or it is nothing.
    #[tokio::test]
    async fn memory_a_directive_is_not_shadowed_by_its_own_turn_record() {
        let fixture = fixture().await;
        let project_id = resolve_project_id(&fixture.store, &fixture.workspace)
            .await
            .expect("project identity");
        let said = "Always run cargo test before you commit";

        // Three identical directive turns. The directive collapses to one asset; the log
        // keeps one record per turn, which is what makes the shadowing plain.
        for _ in 0..3 {
            let task_id = TaskId::generate();
            let session = SessionId::generate();
            admit(&fixture, &project_id, &session, &task_id, said).await;
            let owner = principal(project_id.clone(), task_id.clone(), session.clone());
            remember_input(Arc::clone(&fixture.store), &owner, &session)
                .await
                .expect("remember runs");
            remember_turn(
                Arc::clone(&fixture.store),
                &owner,
                &session,
                "Understood, I will run cargo test before committing.",
            )
            .await
            .expect("remember_turn runs");
        }

        let later = principal(
            project_id.clone(),
            TaskId::generate(),
            SessionId::generate(),
        );
        let recalled = recall(
            Arc::clone(&fixture.store),
            &later,
            &fixture.workspace,
            "Do I need to run cargo test before I commit?",
        )
        .await
        .expect("recall runs");
        assert_eq!(
            recalled.state,
            RetrievalState::Found,
            "the directive is in the store: {}",
            recalled.message
        );
        let injected = recalled
            .contribution
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            injected.contains("Always run cargo test"),
            "the instruction must reach the model: {injected}"
        );
        assert!(
            !injected.contains("asked:"),
            "a turn record quotes the instruction back; injected as the answer it frames the \
             user's own instruction as something they were quoted saying: {injected}"
        );
        assert!(
            !recalled.message.contains("conversation log"),
            "durable memory answered, so the log was not consulted: {}",
            recalled.message
        );
    }

    /// When durable memory holds nothing, the log still answers the question.
    ///
    /// The knowledge path asks durable memory first, and the point of asking the log second
    /// is that an answer which was given and never promoted to knowledge is still reachable.
    /// Without the fallback, keeping the log out of the first query would have traded a
    /// shadow for a silence.
    #[tokio::test]
    async fn memory_the_log_answers_when_durable_memory_holds_nothing() {
        let fixture = fixture().await;
        let project_id = resolve_project_id(&fixture.store, &fixture.workspace)
            .await
            .expect("project identity");
        let task_id = TaskId::generate();
        let session = SessionId::generate();
        // A question, so nothing durable is written for it: the log record and its answer
        // are the only material that mentions the marker.
        let asked = "Which marker identifies the zebra release?";
        admit(&fixture, &project_id, &session, &task_id, asked).await;
        let owner = principal(project_id.clone(), task_id, session.clone());
        let outcome = remember_input(Arc::clone(&fixture.store), &owner, &session)
            .await
            .expect("remember runs");
        assert!(
            matches!(outcome, super::RememberOutcome::NotKnowledge { .. }),
            "a question is not stored as knowledge: {outcome:?}"
        );
        remember_turn(
            Arc::clone(&fixture.store),
            &owner,
            &session,
            "The zebra release is identified by marker zebra-quasar-7719.",
        )
        .await
        .expect("remember_turn runs");

        let later = principal(
            project_id.clone(),
            TaskId::generate(),
            SessionId::generate(),
        );
        let service = MemoryService::new(Arc::clone(&fixture.store));
        let terms = normalize_terms("what marker identifies the zebra release");
        let (index, result) = service
            .search_durable_before_log(&later, &terms, RECALL_HITS, None)
            .await
            .expect("search runs");
        assert_eq!(
            index,
            MemoryIndex::Log,
            "nothing durable holds these words, so the log answered: {:#?}",
            result.detail
        );
        assert_eq!(result.state, RetrievalState::Found);
        assert!(
            result
                .hits
                .iter()
                .any(|hit| hit.current.content.contains("zebra-quasar-7719")),
            "the answer the model gave is still reachable by keyword"
        );
    }

    /// A long answer is in the log in full, not as a 200-character head.
    ///
    /// The record kept the first 200 characters of the model's answer, which answers "what
    /// did I ask" and cannot answer anything *about* the answer: the command, the path or
    /// the conclusion sits past the cut, and the model is shown a record that visibly stops
    /// mid-sentence. The bound is now the point past which more text could not reach the
    /// model in one turn anyway, so the question "which marker did you give me" is answered
    /// from the answer it is asking about.
    #[tokio::test]
    async fn memory_a_long_answer_is_reachable_by_a_question_about_its_content() {
        let fixture = fixture().await;
        let project_id = resolve_project_id(&fixture.store, &fixture.workspace)
            .await
            .expect("project identity");
        let task_id = TaskId::generate();
        let session = SessionId::generate();
        // A question, so nothing durable is written: the record and its answer are the only
        // material that carries the marker.
        admit(
            &fixture,
            &project_id,
            &session,
            &task_id,
            "How do I deploy the parser service to production?",
        )
        .await;
        let owner = principal(project_id.clone(), task_id, session.clone());
        // The marker sits far past the old 200-character cut, which is the whole point.
        let mut answer = String::from("To deploy the parser service to production, in order:\n");
        for step in 1..=18 {
            let line = format!(
                "{step}. Check the release notes and the configuration for step {step} before \
                 continuing.\n"
            );
            answer.push_str(&line);
        }
        answer.push_str(
            "Finally run the deploy with `cargo run --release --locked --bin ha-deploy`, and \
             record it under deploy-marker-zebra-9911.",
        );
        assert!(
            answer.chars().count() > 600,
            "the fixture answer must be longer than the cut this test is about: {}",
            answer.chars().count()
        );
        remember_turn(Arc::clone(&fixture.store), &owner, &session, &answer)
            .await
            .expect("remember_turn runs");

        let later = principal(
            project_id.clone(),
            TaskId::generate(),
            SessionId::generate(),
        );
        let recalled = recall(
            Arc::clone(&fixture.store),
            &later,
            &fixture.workspace,
            "Which deploy marker did you give me for the parser service?",
        )
        .await
        .expect("recall runs");
        assert_eq!(
            recalled.state,
            RetrievalState::Found,
            "the answer is in the log: {}",
            recalled.message
        );
        let injected = recalled
            .contribution
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            injected.contains("deploy-marker-zebra-9911"),
            "the end of the answer must reach the model, not just its first line: {injected}"
        );
        assert!(
            !injected.contains("truncated"),
            "this answer fits the turn budget, so nothing about it should be clipped: \
             {injected}"
        );
    }

    /// One long hit does not hide the other hits the same search found.
    ///
    /// A block that did not fit the budget used to be dropped whole, so a long memory
    /// crowded out everything the search found beside it. Conversation records made that
    /// certain rather than unlikely: a record is a question followed by an answer, and
    /// answers are longer than questions. Each hit now gets a fair share, and a hit that
    /// still does not fit is clipped and says so.
    #[tokio::test]
    async fn memory_a_long_hit_does_not_hide_the_hits_beside_it() {
        let fixture = fixture().await;
        let project_id = resolve_project_id(&fixture.store, &fixture.workspace)
            .await
            .expect("project identity");
        let owner = principal(
            project_id.clone(),
            TaskId::generate(),
            SessionId::generate(),
        );
        let service = MemoryService::new(Arc::clone(&fixture.store));
        // The long one repeats the shared term, which is what puts it first in bm25 and
        // used to spend the whole budget before the others were ever considered. Its
        // distinctive token is at the front, because a clipped block is a head: that is
        // what "clipped" means, and its tail is what the clip marker is telling the reader
        // about.
        let long = format!(
            "long-hit-head shared-term\n{}",
            "shared-term filler sentence. ".repeat(400)
        );
        let mut contents = vec![
            long,
            "shared-term short-hit-one".to_owned(),
            "shared-term short-hit-two".to_owned(),
        ];
        for content in contents.drain(..) {
            service
                .create_asset(
                    &owner,
                    CreateMemoryAsset {
                        kind: "project_fact".to_owned(),
                        scope: super::MemoryScope::Project,
                        layer: MemoryLayer::L1,
                        project_id: Some(project_id.clone()),
                        task_id: None,
                        agent_profile_id: None,
                        session_id: None,
                        visibility: "scoped".to_owned(),
                        content,
                        authority: SourceAuthority::RuntimeObserved,
                        evidence: EvidenceState::VerifiedObservation,
                        user_confirmed: false,
                        source_event_refs: Vec::new(),
                        source_file_hashes: Vec::new(),
                        source_commit: Some("long-hit-fixture".to_owned()),
                        provenance_kind: "runtime_observation".to_owned(),
                        sources: Vec::new(),
                    },
                )
                .await
                .expect("fixture asset is written");
        }

        let result = service
            .search(&owner, "shared term", 8, None)
            .await
            .expect("search runs");
        assert_eq!(
            result.hits.len(),
            3,
            "all three assets hold the query terms: {:#?}",
            result.detail
        );
        let contribution = service.contribute(&owner, &result, 800);
        let injected = contribution
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        for marker in ["long-hit-head", "short-hit-one", "short-hit-two"] {
            assert!(
                injected.contains(marker),
                "{marker} is missing from the blocks the model would read: {injected}"
            );
        }
        assert!(
            injected.contains("truncated"),
            "the long hit was clipped, and a clipped memory says so: {injected}"
        );
    }

    /// A question about the conversation still sees several turns, not just the newest.
    ///
    /// "What did I ask you before?" is a question about breadth: the answer is the list of
    /// questions, and the newest turn is not more of an answer than the ones before it.
    /// Storing whole answers made this the case that would break first - one long answer
    /// could spend the whole budget and leave the model with a single turn - so the fair
    /// share is what keeps the record's question in the message even when its answer is
    /// longer than the budget for all eight.
    #[tokio::test]
    async fn memory_history_still_shows_every_recent_turn_after_long_answers() {
        let fixture = fixture().await;
        let project_id = resolve_project_id(&fixture.store, &fixture.workspace)
            .await
            .expect("project identity");
        for index in 0..8 {
            let task_id = TaskId::generate();
            let session = SessionId::generate();
            admit(
                &fixture,
                &project_id,
                &session,
                &task_id,
                &format!("question number {index}"),
            )
            .await;
            let owner = principal(project_id.clone(), task_id, session.clone());
            let answer = format!(
                "Answer number {index}. {}",
                "This paragraph is long enough that one turn cannot hold eight of them. "
                    .repeat(12)
            );
            remember_turn(Arc::clone(&fixture.store), &owner, &session, &answer)
                .await
                .expect("remember_turn runs");
        }

        let later = principal(
            project_id.clone(),
            TaskId::generate(),
            SessionId::generate(),
        );
        let recalled = recall(
            Arc::clone(&fixture.store),
            &later,
            &fixture.workspace,
            "session trước tôi hỏi bạn những gì?",
        )
        .await
        .expect("recall runs");
        assert_eq!(
            recalled.state,
            RetrievalState::Found,
            "the log holds eight turns: {}",
            recalled.message
        );
        assert!(
            !recalled.contribution.stamps.is_empty()
                && recalled
                    .contribution
                    .stamps
                    .iter()
                    .all(|stamp| stamp.reason == SelectionReason::LogFallback),
            "history results must carry log-fallback provenance"
        );
        let injected = recalled
            .contribution
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        for index in 0..8 {
            assert!(
                injected.contains(&format!("question number {index}")),
                "every recent turn has to be in the message, and question number {index} is \
                 missing (hits={}, blocks={}): {injected}",
                recalled.hits,
                recalled.contribution.blocks.len()
            );
        }
    }

    /// Deduplication never reaches across a project boundary.
    ///
    /// "The same bytes" is only a safe reason to skip a write if the asset the caller found
    /// is one the caller could have written. Read without the search rule, the lookup saw
    /// every project's assets, so a principal in one project could have its input silently
    /// folded into another project's memory - the second project's asset, the second
    /// project's scope, and nothing stored where the user actually is.
    #[tokio::test]
    async fn memory_deduplication_is_scoped_to_what_the_principal_can_search() {
        let fixture = fixture().await;
        let project_id = resolve_project_id(&fixture.store, &fixture.workspace)
            .await
            .expect("project identity");
        let said = "Always run cargo test before you commit";
        let task_id = TaskId::generate();
        let session = SessionId::generate();
        admit(&fixture, &project_id, &session, &task_id, said).await;
        let owner = principal(project_id.clone(), task_id, session.clone());
        let stored = remember_input(Arc::clone(&fixture.store), &owner, &session)
            .await
            .expect("remember runs")
            .expect_stored();

        let service = MemoryService::new(Arc::clone(&fixture.store));
        assert_eq!(
            service
                .find_active_by_content(&owner, said)
                .await
                .expect("lookup runs"),
            Some(stored.clone()),
            "the project that holds it must still see it, or nothing would ever deduplicate"
        );

        // Another project, with the same words. A project identity is what the launch
        // resolves, so this is what a second workspace looks like.
        let elsewhere = MemoryPrincipal {
            project_id: Some(ProjectId::generate()),
            ..owner.clone()
        };
        assert_eq!(
            service
                .find_active_by_content(&elsewhere, said)
                .await
                .expect("lookup runs"),
            None,
            "an asset in another project is not this project's memory"
        );
    }

    /// A caller's own terms cannot turn into FTS5 syntax.
    ///
    /// The store takes a MATCH expression, and a term is quoted to keep it from becoming
    /// one. A term that already carries a quote defeats that: the expression stops parsing
    /// and the whole query is reported as an index failure. Terms now pass through the same
    /// normalization the indexed mirror does, so a quote is only a character.
    #[tokio::test]
    async fn memory_a_term_with_a_quote_is_a_term_and_not_syntax() {
        let fixture = fixture().await;
        let project_id = resolve_project_id(&fixture.store, &fixture.workspace)
            .await
            .expect("project identity");
        let task_id = TaskId::generate();
        let session = SessionId::generate();
        admit(
            &fixture,
            &project_id,
            &session,
            &task_id,
            "Remember this marker for later: zebra-quasar-7719",
        )
        .await;
        let owner = principal(project_id.clone(), task_id, session.clone());
        remember_input(Arc::clone(&fixture.store), &owner, &session)
            .await
            .expect("remember runs")
            .expect_stored();

        let service = MemoryService::new(Arc::clone(&fixture.store));
        // Exactly what a JSON or shell caller passes through: a quoted phrase, and an
        // operator that would mean something to FTS5.
        for term in [
            "\"zebra quasar\"",
            "zebra*",
            "NEAR(zebra quasar)",
            "zebra\"",
        ] {
            let result = service
                .search_terms(&owner, &[term.to_owned()], 8, None)
                .await
                .unwrap_or_else(|error| panic!("{term} must be a term, not syntax: {error}"));
            assert_ne!(
                result.state,
                RetrievalState::Error,
                "{term} was reported as an index outage: {:?}",
                result.detail
            );
        }
    }

    /// The conjunction is still asked when the wider union has nothing to trust.
    ///
    /// The union is wide, so its candidate window can fill with documents that share one
    /// term - here, thirty-three long documents repeating `alpha` - and hide the one
    /// document that holds every term. The overlap floor rejects all of them, and the
    /// fallback is what keeps the exact ask from being answered with silence.
    #[tokio::test]
    async fn memory_the_exact_ask_still_answers_when_the_union_window_is_crowded() {
        let fixture = fixture().await;
        let project_id = resolve_project_id(&fixture.store, &fixture.workspace)
            .await
            .expect("project identity");
        let owner = principal(
            project_id.clone(),
            TaskId::generate(),
            SessionId::generate(),
        );
        let service = MemoryService::new(Arc::clone(&fixture.store));
        let long = "alpha ".repeat(40);
        for index in 0..33 {
            service
                .create_asset(
                    &owner,
                    CreateMemoryAsset {
                        kind: "project_fact".to_owned(),
                        scope: super::MemoryScope::Project,
                        layer: MemoryLayer::L1,
                        project_id: Some(project_id.clone()),
                        task_id: None,
                        agent_profile_id: None,
                        session_id: None,
                        visibility: "scoped".to_owned(),
                        content: format!("{long}{index}"),
                        authority: SourceAuthority::RuntimeObserved,
                        evidence: EvidenceState::VerifiedObservation,
                        user_confirmed: false,
                        source_event_refs: Vec::new(),
                        source_file_hashes: Vec::new(),
                        source_commit: Some("crowded-window-fixture".to_owned()),
                        provenance_kind: "runtime_observation".to_owned(),
                        sources: Vec::new(),
                    },
                )
                .await
                .expect("crowding asset is written");
        }
        service
            .create_asset(
                &owner,
                CreateMemoryAsset {
                    kind: "project_fact".to_owned(),
                    scope: super::MemoryScope::Project,
                    layer: MemoryLayer::L1,
                    project_id: Some(project_id.clone()),
                    task_id: None,
                    agent_profile_id: None,
                    session_id: None,
                    visibility: "scoped".to_owned(),
                    content: "alpha beta".to_owned(),
                    authority: SourceAuthority::RuntimeObserved,
                    evidence: EvidenceState::VerifiedObservation,
                    user_confirmed: false,
                    source_event_refs: Vec::new(),
                    source_file_hashes: Vec::new(),
                    source_commit: Some("crowded-window-fixture".to_owned()),
                    provenance_kind: "runtime_observation".to_owned(),
                    sources: Vec::new(),
                },
            )
            .await
            .expect("the exact asset is written");

        let result = service
            .search_terms(&owner, &["alpha".to_owned(), "beta".to_owned()], 8, None)
            .await
            .expect("search runs");
        assert_eq!(
            result.state,
            RetrievalState::Found,
            "the one document holding both terms is the answer: {:?}",
            result.detail
        );
        assert_eq!(
            result.hits.len(),
            1,
            "and it is the only hit: {:#?}",
            result
                .hits
                .iter()
                .map(|hit| hit.current.content.as_str())
                .collect::<Vec<_>>()
        );
        assert!(
            result.hits[0].current.content.contains("beta"),
            "the union window could not reach it, so the conjunction answered: {}",
            result.hits[0].current.content
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

    /// A provider that answers every call with one fixed text, and is not a fixture.
    struct ReplyProvider {
        reply: String,
    }

    impl harness_providers::ModelProvider for ReplyProvider {
        fn capabilities(&self) -> harness_providers::ModelCapabilities {
            harness_providers::ModelCapabilities {
                fixture: false,
                ..harness_providers::ModelCapabilities::deepseek_fixture()
            }
        }

        fn stream(
            &self,
            request: harness_providers::ProviderRequest,
            _cancellation: harness_providers::CancellationToken,
        ) -> harness_providers::ProviderFuture {
            let events = vec![
                harness_providers::ProviderStreamEvent::Started {
                    request_id: request.request_id,
                },
                harness_providers::ProviderStreamEvent::text(self.reply.clone()),
                harness_providers::ProviderStreamEvent::completed("stop"),
            ];
            Box::pin(async move { Ok(events) })
        }
    }

    fn reply(text: &str) -> Arc<dyn harness_providers::ModelProvider> {
        Arc::new(ReplyProvider {
            reply: text.to_owned(),
        })
    }

    #[test]
    fn only_an_explicit_request_is_remembered_verbatim() {
        for asked in [
            "ghi nhớ: dự án dùng pnpm",
            "Hãy nhớ rằng CI chạy trên Windows",
            "Remember that the staging DB is read-only",
            "từ giờ luôn trả lời bằng tiếng Việt",
            "From now on, run cargo fmt before committing",
            "never push to master directly",
        ] {
            assert_eq!(
                super::explicit_memory(asked).as_deref(),
                Some(asked),
                "{asked:?} asks to be remembered and is kept whole"
            );
        }
        for ordinary in [
            "hãy sửa lỗi build cho tôi",
            "hãy mô tả cho tôi memory dự án này",
            "fix the failing test",
            "remember",
            "remembering the old API, port it",
            "luonvan is a word that only starts like one",
        ] {
            assert_eq!(
                super::explicit_memory(ordinary),
                None,
                "{ordinary:?} is a request, not something to remember"
            );
        }
    }

    /// Measured on a real project: "hãy mô tả cho tôi memory dự án này" shares "hay",
    /// "cho" and "toi" with any polite request, so an unrelated old input cleared the
    /// two-term overlap floor on grammar alone and was injected as memory.
    #[tokio::test]
    async fn memory_function_words_alone_do_not_make_a_match() {
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
            "từ giờ hãy sửa lỗi cho tôi trước khi commit",
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
        let recalled = recall(
            Arc::clone(&fixture.store),
            &principal(project_id, task_id, SessionId::generate()),
            &fixture.workspace,
            "hãy mô tả cho tôi memory dự án này",
        )
        .await
        .expect("recall runs");
        assert_ne!(
            recalled.state,
            RetrievalState::Found,
            "an unrelated request must not be injected: {}",
            recalled.message
        );
    }

    /// The L0 -> L1 step: a turn's confident facts are applied, the uncertain one waits
    /// for review, a malformed one is refused, and a later fact can replace an earlier
    /// one it was shown.
    #[tokio::test]
    #[allow(clippy::too_many_lines)] // two extractions and what each left behind, in order
    async fn memory_extracted_facts_are_applied_by_confidence_and_can_be_replaced() {
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
            "project build uses pnpm and deploys to staging on fridays",
        )
        .await;
        let owner = principal(project_id.clone(), task_id.clone(), first.clone());
        let report = super::extract_facts(
            reply(
                "```json\n{\"facts\":[\
                 {\"content\":\"The project build uses pnpm.\",\"category\":\"preference\",\"confidence\":0.95,\"replaces\":null},\
                 {\"content\":\"Staging deploys may happen on fridays.\",\"category\":\"context\",\"confidence\":0.5,\"replaces\":null},\
                 {\"content\":\"Something\",\"category\":\"gossip\",\"confidence\":0.9}\
                 ]}\n```",
            ),
            Arc::clone(&fixture.store),
            &owner,
            &first,
            "Noted: pnpm for builds.",
        )
        .await
        .expect("extraction runs");
        assert_eq!(
            (report.applied, report.candidates, report.refused),
            (1, 1, 1),
            "{report:?}"
        );
        assert!(
            report
                .message()
                .is_some_and(|message| message.contains("learned 1 fact(s)")),
            "{report:?}"
        );

        let later = principal(project_id.clone(), task_id.clone(), SessionId::generate());
        let recalled = recall(
            Arc::clone(&fixture.store),
            &later,
            &fixture.workspace,
            "which package manager does the project build use",
        )
        .await
        .expect("recall runs");
        let injected = recalled
            .contribution
            .blocks
            .iter()
            .map(|block| block.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            injected.contains("The project build uses pnpm.")
                && injected.contains("confidence=0.95"),
            "the applied fact reaches the next turn with its confidence: {injected}"
        );
        assert!(
            !injected.contains("fridays"),
            "a candidate is not injected: {injected}"
        );

        // The next turn changes the fact; the extractor is shown the known one and
        // names it, so the old fact is retired instead of contradicting the new one.
        let service = MemoryService::new(Arc::clone(&fixture.store));
        let known = super::known_facts(&service, &later, "project build package manager")
            .await
            .expect("known facts");
        let (old_id, _) = known
            .iter()
            .find(|(_, content)| content.contains("pnpm"))
            .expect("the applied fact is known")
            .clone();
        // Facts are project-scoped, so the next conversation (another task) sees them.
        let task_id = TaskId::generate();
        let second = SessionId::generate();
        admit(
            &fixture,
            &project_id,
            &second,
            &task_id,
            "we switched the project build from pnpm to bun",
        )
        .await;
        let report = super::extract_facts(
            reply(&format!(
                "{{\"facts\":[{{\"content\":\"The project build uses bun instead of pnpm.\",\"category\":\"correction\",\"confidence\":0.9,\"replaces\":\"{old_id}\"}},\
                 {{\"content\":\"Unrelated guess.\",\"category\":\"context\",\"confidence\":0.9,\"replaces\":\"memory_asset_not_shown\"}}]}}"
            )),
            Arc::clone(&fixture.store),
            &principal(project_id.clone(), task_id.clone(), second.clone()),
            &second,
            "Understood, bun from now on.",
        )
        .await
        .expect("extraction runs");
        assert_eq!((report.applied, report.replaced), (2, 1), "{report:?}");
        let recalled = recall(
            Arc::clone(&fixture.store),
            &principal(project_id, task_id, SessionId::generate()),
            &fixture.workspace,
            "which package manager does the project build use",
        )
        .await
        .expect("recall runs");
        let injected = recalled
            .contribution
            .blocks
            .iter()
            .map(|block| block.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(injected.contains("bun instead of pnpm"), "{injected}");
        assert!(
            !injected.contains("The project build uses pnpm."),
            "the replaced fact is retired: {injected}"
        );
    }

    #[tokio::test]
    async fn memory_an_unreadable_extraction_is_skipped_not_an_error() {
        let fixture = fixture().await;
        let project_id = resolve_project_id(&fixture.store, &fixture.workspace)
            .await
            .expect("project identity");
        let task_id = TaskId::generate();
        let session = SessionId::generate();
        admit(&fixture, &project_id, &session, &task_id, "anything").await;
        let report = super::extract_facts(
            reply("I could not find any facts, sorry."),
            Arc::clone(&fixture.store),
            &principal(project_id, task_id, session.clone()),
            &session,
            "an answer",
        )
        .await
        .expect("a reply that is not JSON is not a failure");
        assert!(report.skipped.is_some(), "{report:?}");
        assert_eq!(report.message(), None);
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
            "ghi nhớ: dự án này dùng Rust nhé",
        )
        .await;
        let stored = remember_input(Arc::clone(store), &owner, &first)
            .await
            .expect("remember runs")
            .expect_stored();
        assert_eq!(stored.as_str().split('_').next(), Some("memory"));

        let later = principal(project_id, task_id, second);
        let recalled = recall(
            Arc::clone(store),
            &later,
            &fixture.workspace,
            "dự án dùng Rust nhé?",
        )
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

    /// Runtime recall re-reads file sources before ranking, so a moved file cannot
    /// leave its old derived text in the model context.
    #[tokio::test]
    async fn memory_recall_excludes_file_backed_assets_after_the_source_moves() {
        let fixture = fixture().await;
        let project_id = resolve_project_id(&fixture.store, &fixture.workspace)
            .await
            .expect("project identity");
        let source_path = fixture.workspace.join("policy.md");
        let original = b"The aurora ledger must remain enabled";
        std::fs::write(&source_path, original).expect("source file is written");
        let owner = principal(
            project_id.clone(),
            TaskId::generate(),
            SessionId::generate(),
        );
        MemoryService::new(Arc::clone(&fixture.store))
            .create_asset(
                &owner,
                CreateMemoryAsset {
                    kind: "project_fact".to_owned(),
                    scope: MemoryScope::Project,
                    layer: MemoryLayer::L1,
                    project_id: Some(project_id.clone()),
                    task_id: None,
                    agent_profile_id: None,
                    session_id: None,
                    visibility: "scoped".to_owned(),
                    content: "The aurora ledger must remain enabled".to_owned(),
                    authority: SourceAuthority::RuntimeObserved,
                    evidence: EvidenceState::VerifiedObservation,
                    user_confirmed: false,
                    source_event_refs: Vec::new(),
                    source_file_hashes: Vec::new(),
                    source_commit: None,
                    provenance_kind: "workspace_fact".to_owned(),
                    sources: vec![MemorySource::file(
                        "policy.md",
                        ContentHash::from_bytes(original),
                    )],
                },
            )
            .await
            .expect("file-backed memory is stored");

        let reader = principal(project_id, TaskId::generate(), SessionId::generate());
        let fresh = recall(
            Arc::clone(&fixture.store),
            &reader,
            &fixture.workspace,
            "what does the aurora ledger require?",
        )
        .await
        .expect("fresh recall runs");
        assert_eq!(fresh.state, RetrievalState::Found, "{}", fresh.message);
        assert!(
            fresh.contribution.blocks[0].text.contains("remain enabled"),
            "the current source is searchable"
        );

        std::fs::write(&source_path, b"The aurora ledger is now disabled")
            .expect("source file changes");
        let stale = recall(
            Arc::clone(&fixture.store),
            &reader,
            &fixture.workspace,
            "what does the aurora ledger require?",
        )
        .await
        .expect("stale recall runs");
        assert!(
            !stale
                .contribution
                .blocks
                .iter()
                .any(|block| { block.text.contains("must remain enabled") }),
            "text derived from an earlier file version must be removed before ranking"
        );
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
            "từ giờ chỉ dùng cargo test",
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
        let recalled = recall(
            Arc::clone(store),
            &stranger,
            &fixture.workspace,
            "cargo test",
        )
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
        let found = recall(
            Arc::clone(store),
            &owner,
            &fixture.workspace,
            "marker zeta42",
        )
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
            packet
                .packet
                .content
                .contains(harness_memory::MEMORY_BLOCK_HEADING),
            "it must be rendered as a memory block, under a heading that says what the \
             material is: {}",
            packet.packet.content
        );
        assert!(
            packet.packet.content.contains("authority="),
            "and it must keep its provenance note for audit: {}",
            packet.packet.content
        );
        assert_eq!(
            packet.packet.memory_versions.len(),
            1,
            "the packet must pin the memory version it used"
        );
    }
}
