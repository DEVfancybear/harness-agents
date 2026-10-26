//! The rebuildable journal index and the notes table (M5-03).
//!
//! History is derived state. Every row here can be thrown away and rebuilt from
//! the event journal, and a rebuild must produce exactly the same accessible
//! results — otherwise a search answer would depend on when the index happened
//! to be written rather than on what the journal says.
//!
//! Two rules shape the reads:
//!
//!   * **Scope is a filter, not a ranking input.** A caller's project and task
//!     are part of the SQL predicate, so a foreign source can never appear in a
//!     result set and then be filtered out afterwards.
//!   * **A note is a model report, not truth.** Notes carry the sources they
//!     were written from; checking a source exists proves provenance, never
//!     correctness.

use std::{
    collections::BTreeSet,
    fmt::Write as _,
    sync::Arc,
    time::{Duration, Instant},
};

use harness_types::{ContentHash, ErrorCode, ProjectId, SessionId, SourceRef, TaskId};
use sqlx::Row;

use crate::{SqliteStore, StoreError};

/// Longest stored content per source; a journal entry larger than this keeps its
/// head and says it was cut.
pub const HISTORY_SOURCE_LIMIT_BYTES: usize = 64 * 1024;

/// Longest single term kept in the index.
const HISTORY_TERM_LIMIT_CHARS: usize = 64;

/// Most terms one source contributes. A longer source keeps the terms it
/// opens with and the ones it ends with (see `history_terms`).
const HISTORY_TERMS_PER_SOURCE: usize = 1024;

/// BM25's IDF scaled to an integer, so a score sums the same way on every
/// read: a float sum depends on the order the rows arrive in.
const HISTORY_IDF_SCALE: f64 = 1000.0;

/// Largest page `history_read` returns.
pub const HISTORY_READ_MAX_BYTES: usize = 64 * 1024;

/// Page size when a caller names none.
pub const HISTORY_READ_DEFAULT_BYTES: usize = 16 * 1024;

/// Longest note key.
pub const NOTE_KEY_LIMIT_CHARS: usize = 120;

/// Longest note body.
pub const NOTE_CONTENT_LIMIT_BYTES: usize = 16 * 1024;

/// Journal entries written per index transaction: few, so the one writing
/// connection goes back to the agents between them. At 128 a transaction
/// held it for about a second, and its commit for seconds more when it made
/// the writer copy the log back.
const HISTORY_SOURCES_PER_TRANSACTION: usize = 32;

/// Transactions a background catch-up writes between pauses, and how long it
/// pauses: long enough for the store's checkpointer to find the writer quiet
/// and copy the log back itself (`checkpoint`). Without the pauses the log
/// outgrew the writer's own limit every few transactions and the writer
/// copied it back inside a commit: 648 entries of a 140 MB store took 36 s,
/// and 5 s once no commit copied.
const HISTORY_BATCHES_PER_PAUSE: usize = 8;
const HISTORY_PAUSE: Duration = Duration::from_millis(300);

/// The tables an older version's index was kept in, renamed when the schema
/// moved on and deleted in the background.
const RETIRED_HISTORY_TABLES: [&str; 2] = ["history_terms_retired", "history_sources_retired"];

/// Rows of a retired table deleted per transaction. Dropping a 5.9-million-row
/// table at once held the writer for 15 s, and 5,000 rows at a time made the
/// agent's own writes wait 133 ms in the median.
const RETIRED_ROWS_PER_TRANSACTION: i64 = 1_000;

/// How a catch-up of the index spends its time.
#[derive(Clone, Copy, Debug)]
pub enum CatchUpPace {
    /// As fast as it can, for as long as it takes.
    Whole,
    /// As fast as it can, until the deadline: what a search waits for.
    Until(Instant),
    /// Behind everything else: it pauses every few transactions.
    Background,
}

/// How a catch-up of the index ended.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HistoryCatchUp {
    /// Sources written.
    pub indexed: u64,
    /// Conversations still behind when the time ran out. The newest go
    /// first, so these are the oldest.
    pub behind: u64,
}

/// A journal entry about to be indexed.
struct NewSource {
    source_id: String,
    sequence: u64,
    kind: String,
    content: String,
}

/// The session, task and project every source of one batch belongs to.
struct SourceOwner<'a> {
    session: &'a SessionId,
    task: &'a TaskId,
    project: &'a ProjectId,
}

/// Whether the bytes behind a reference can still be read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceAvailability {
    Available,
    /// The journal no longer holds the event the reference names.
    Expired,
}

impl SourceAvailability {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Expired => "expired",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "available" => Some(Self::Available),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }
}

/// One search result. The preview is for display; `history_read` is the exact
/// bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryHit {
    pub source_id: String,
    pub sequence: u64,
    pub kind: String,
    pub content_hash: ContentHash,
    pub availability: SourceAvailability,
    pub preview: String,
    /// How many of the query's terms this source contains, of how many.
    pub matched_terms: u64,
    pub query_terms: u64,
    /// The task the source belongs to, and when its session began.
    pub task_id: String,
    pub started: String,
}

/// One source as semantic search embeds it: its id, kind and hash, and the
/// head of its text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryText {
    pub source_id: String,
    pub kind: String,
    pub content_hash: String,
    pub text: String,
}

/// One page of an exact source read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryPage {
    pub source_id: String,
    pub sequence: u64,
    pub kind: String,
    pub content_hash: ContentHash,
    pub offset: u64,
    pub bytes: Vec<u8>,
    pub total_bytes: u64,
    /// The sources before and after it in its session: memory-engine's
    /// thread links, so a reader walks the conversation without searching.
    pub previous_source_id: Option<String>,
    pub next_source_id: Option<String>,
}

/// What a history search keeps beside its words: a window of time, the kinds
/// of journal events, and which way equal scores go.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HistoryFilter {
    /// Sources written at or after this many milliseconds since the epoch.
    pub since_ms: Option<u64>,
    /// Sources written before this.
    pub until_ms: Option<u64>,
    /// Only these journal event types (`user_message`...), when any.
    pub kinds: Vec<String>,
    /// Equal scores oldest first instead of newest first.
    pub oldest_first: bool,
}

impl HistoryFilter {
    /// The least source id written at `ms`: a source id is the journal
    /// event's id, `event_<UUIDv7>`, whose head is its time, so ids order as
    /// times.
    #[must_use]
    pub fn id_at(ms: u64) -> String {
        format!(
            "{}_{:08x}-{:04x}-7000-0000-000000000000",
            harness_types::EventId::PREFIX,
            (ms >> 16) & 0xffff_ffff,
            ms & 0xffff
        )
    }

    /// Whether a source passes the filter's time window and kinds.
    #[must_use]
    pub fn admits(&self, source_id: &str, kind: &str) -> bool {
        self.since_ms
            .is_none_or(|ms| source_id >= Self::id_at(ms).as_str())
            && self
                .until_ms
                .is_none_or(|ms| source_id < Self::id_at(ms).as_str())
            && (self.kinds.is_empty() || self.kinds.iter().any(|wanted| wanted == kind))
    }

    /// The SQL the filter adds to a query of `history_sources s`, and its
    /// bind values in order.
    fn clause(&self) -> (String, Vec<String>) {
        let mut clauses = String::new();
        let mut values = Vec::new();
        if let Some(ms) = self.since_ms {
            clauses.push_str(" AND s.source_id >= ?");
            values.push(Self::id_at(ms));
        }
        if let Some(ms) = self.until_ms {
            clauses.push_str(" AND s.source_id < ?");
            values.push(Self::id_at(ms));
        }
        if !self.kinds.is_empty() {
            clauses.push_str(" AND s.kind IN (");
            clauses.push_str(&vec!["?"; self.kinds.len()].join(", "));
            clauses.push(')');
            values.extend(self.kinds.iter().cloned());
        }
        (clauses, values)
    }
}

/// Who is asking, and which foreign sources they may still read.
///
/// `granted_source_ids` is how a fork keeps reading the lineage it was allowed
/// to inherit without widening its task scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryScope {
    pub project_id: ProjectId,
    pub task_id: TaskId,
    pub granted_source_ids: Vec<String>,
    /// Every task of the project is in scope, not only `task_id`: the user's
    /// earlier conversations about the same project.
    pub project_wide: bool,
}

impl HistoryScope {
    #[must_use]
    pub fn new(project_id: ProjectId, task_id: TaskId) -> Self {
        Self {
            project_id,
            task_id,
            granted_source_ids: Vec::new(),
            project_wide: false,
        }
    }

    /// Widen the scope to every conversation of the project; the project
    /// itself stays a filter.
    #[must_use]
    pub const fn across_project(mut self) -> Self {
        self.project_wide = true;
        self
    }

    #[must_use]
    pub fn with_grants(mut self, grants: Vec<String>) -> Self {
        self.granted_source_ids = grants;
        self
    }
}

/// One stored note.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NoteRecord {
    pub note_id: String,
    pub task_id: TaskId,
    pub session_id: SessionId,
    pub key: String,
    pub revision: u64,
    pub content: String,
    pub sources: Vec<SourceRef>,
    pub authority: String,
}

impl NoteRecord {
    /// A note is a model report; nothing here is validated as true.
    pub const AUTHORITY: &'static str = "model_report";
}

/// Normalize text into the terms the index stores.
///
/// The rules are this host's, not a tokenizer's: lowercase, keep letters and
/// digits (including Vietnamese diacritics), treat `-`, `_` and `.` as
/// separators *and* keep the joined form, so `XYZ-731` is findable as a whole
/// identifier and as its parts. A test that depends on these rules is a test of
/// this function, not of a build flag.
///
/// A source with more distinct terms than the index keeps keeps the first
/// half it meets and the last half: a long tool output names its command at
/// the top and its error at the bottom. (It kept the alphabetically first
/// ones, so in a long output nothing after the first letters was findable.)
#[must_use]
pub fn history_terms(text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut ordered = Vec::new();
    let mut keep = |term: String| {
        let term = term
            .chars()
            .take(HISTORY_TERM_LIMIT_CHARS)
            .collect::<String>();
        if !term.is_empty() && seen.insert(term.clone()) {
            ordered.push(term);
        }
    };
    // Each run of alphanumerics joined by `-`, `_` or `.` gives its parts and,
    // when it has more than one, the joined form too, so `XYZ-731` is
    // findable whole and by its parts.
    for run in text.split(|character: char| !is_term_character(character)) {
        let joined = run.trim_matches(['-', '_', '.']).to_lowercase();
        if joined.is_empty() {
            continue;
        }
        for part in joined.split(|character: char| !character.is_alphanumeric()) {
            keep(part.to_owned());
        }
        if joined.chars().any(|character| !character.is_alphanumeric()) {
            keep(joined);
        }
    }
    if ordered.len() > HISTORY_TERMS_PER_SOURCE {
        let tail = HISTORY_TERMS_PER_SOURCE / 2;
        let head = HISTORY_TERMS_PER_SOURCE - tail;
        ordered.drain(head..ordered.len() - tail);
    }
    ordered
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn is_term_character(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '-' | '_' | '.')
}

/// The terms a search looks for: those of the query, less the parts of an
/// identifier it names whole. `XYZ-731` looks for `xyz-731`, not for every
/// source that mentions `xyz` or `731`.
fn search_terms(query: &str) -> Vec<String> {
    let terms = history_terms(query);
    let parts = terms
        .iter()
        .filter(|term| term.chars().any(|character| !character.is_alphanumeric()))
        .flat_map(|joined| {
            joined
                .split(|character: char| !character.is_alphanumeric())
                .filter(|part| !part.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .collect::<BTreeSet<_>>();
    terms
        .into_iter()
        .filter(|term| !parts.contains(term))
        .collect()
}

/// BM25's IDF of a term that `matching` of `sources` contain, scaled to an
/// integer. The index records which terms a source has, not how often, so a
/// hit scores the IDF of each query term it contains: BM25 with every term
/// frequency at one and no length normalization.
fn scaled_idf(sources: u64, matching: u64) -> i64 {
    let idf = harness_types::search::bm25_idf(sources, matching);
    #[allow(clippy::cast_possible_truncation)] // a small positive number
    let scaled = (idf * HISTORY_IDF_SCALE).round() as i64;
    scaled.max(1)
}

/// The text a journal entry contributes to the index.
fn indexable_text(payload: &serde_json::Value) -> Option<String> {
    const KEYS: &[&str] = &[
        "raw_text", "text", "note", "content", "answer", "question", "detail", "reason",
    ];
    if let Some(object) = payload.as_object() {
        for key in KEYS {
            if let Some(value) = object.get(*key).and_then(serde_json::Value::as_str)
                && !value.trim().is_empty()
            {
                return Some(value.to_owned());
            }
        }
        if let Some(text) = object
            .get("model_view")
            .and_then(|view| view.get("text"))
            .and_then(serde_json::Value::as_str)
            && !text.trim().is_empty()
        {
            return Some(text.to_owned());
        }
    }
    // A payload with no readable text still belongs to the journal: index its
    // canonical rendering so a search can find the event by what it recorded.
    let rendered = serde_json::to_string(payload).ok()?;
    (!rendered.trim().is_empty() && rendered != "{}").then_some(rendered)
}

impl SqliteStore {
    /// Index every journal entry of a session that is not indexed yet.
    ///
    /// Returns the number of sources written. It is safe to call before every
    /// search: the work is proportional to what changed, not to the session.
    pub async fn index_history(&self, session_id: &SessionId) -> Result<u64, StoreError> {
        Ok(self
            .index_history_paced(session_id, CatchUpPace::Whole)
            .await?
            .0)
    }

    /// `index_history` at a pace: the sources written, and whether the
    /// session is caught up. A deadline stops it between transactions; the
    /// rest is read again next time, from the last source written.
    async fn index_history_paced(
        &self,
        session_id: &SessionId,
        pace: CatchUpPace,
    ) -> Result<(u64, bool), StoreError> {
        let indexed_through = self.history_watermark(session_id).await?;
        let rows = sqlx::query(
            "SELECT sequence, event_id, event_json FROM events
             WHERE session_id = ? AND sequence > ? ORDER BY sequence",
        )
        .bind(session_id.as_str())
        .bind(to_i64(indexed_through, "history watermark")?)
        .fetch_all(&self.readers)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "read journal for the index",
                error,
            )
        })?;
        let mut sources = Vec::with_capacity(rows.len());
        for row in rows {
            let event_id: String = row_get(&row, "event_id")?;
            let sequence = u64::try_from(row_get::<i64>(&row, "sequence")?).unwrap_or_default();
            let envelope: harness_types::EventEnvelope =
                serde_json::from_str(&row_get::<String>(&row, "event_json")?).map_err(|_| {
                    StoreError::new(
                        ErrorCode::StorageWriteFailed,
                        "journal event is not readable JSON",
                    )
                })?;
            let kind = envelope.event_type;
            let payload = serde_json::Value::Object(envelope.payload);
            let Some(text) = indexable_text(&payload) else {
                continue;
            };
            let (content, truncated) = truncate_bytes(&text, HISTORY_SOURCE_LIMIT_BYTES);
            let content = if truncated {
                format!("{content}\n[history source truncated]")
            } else {
                content
            };
            sources.push(NewSource {
                source_id: event_id,
                sequence,
                kind,
                content,
            });
        }
        if sources.is_empty() {
            return Ok((0, true));
        }
        // The session's task and project are the same for every entry: look
        // them up once, not once per entry (the project is behind a parse of
        // the task's working state).
        let task_id = self
            .session_task(session_id)
            .await?
            .ok_or_else(|| StoreError::new(ErrorCode::InvalidPayload, "session does not exist"))?;
        let project_id = self
            .session_project(session_id)
            .await?
            .ok_or_else(|| StoreError::new(ErrorCode::InvalidPayload, "session has no project"))?;
        let owner = SourceOwner {
            session: session_id,
            task: &task_id,
            project: &project_id,
        };
        let mut written = 0_u64;
        // Several entries per transaction, but bounded, so a long backlog does
        // not hold the writer away from the runtime for the whole catch-up.
        for (index, batch) in sources.chunks(HISTORY_SOURCES_PER_TRANSACTION).enumerate() {
            match pace {
                CatchUpPace::Until(deadline) if Instant::now() >= deadline => {
                    return Ok((written, false));
                }
                CatchUpPace::Background
                    if index > 0 && index.is_multiple_of(HISTORY_BATCHES_PER_PAUSE) =>
                {
                    tokio::time::sleep(HISTORY_PAUSE).await;
                }
                _ => {}
            }
            self.write_history_sources(&owner, batch, SourceAvailability::Available)
                .await?;
            written += u64::try_from(batch.len()).unwrap_or(u64::MAX);
            // Whoever waits for the writer goes next.
            tokio::task::yield_now().await;
        }
        Ok((written, true))
    }

    /// Drop a session's index and rebuild it from the journal.
    ///
    /// This is the operation that must be indistinguishable from incremental
    /// indexing: same sources, same terms, same results.
    pub async fn rebuild_history_index(&self, session_id: &SessionId) -> Result<u64, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        sqlx::query(
            "DELETE FROM history_fts WHERE rowid IN (SELECT rowid FROM history_sources WHERE session_id = ?)",
        )
        .bind(session_id.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "clear history terms", error)
        })?;
        sqlx::query("DELETE FROM history_sources WHERE session_id = ?")
            .bind(session_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                database_error(
                    ErrorCode::StorageWriteFailed,
                    "clear history sources",
                    error,
                )
            })?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit history rebuild",
                error,
            )
        })?;
        self.index_history(session_id).await
    }

    /// Index every session of one task.
    ///
    /// The read scope is the task, so an index that only covered the session
    /// doing the asking would hide the task's own history from it — measured:
    /// a continuation session searched for a source its predecessor wrote and
    /// found nothing.
    ///
    /// Called before every search, so the common case — nothing new — must
    /// cost one query, not two per session: a session whose last journal entry
    /// is already indexed is skipped in SQL. A session whose newest entries had
    /// nothing to index stays listed and is re-read from its last source on;
    /// that read is small and writes nothing.
    pub async fn index_task_history(&self, task_id: &TaskId) -> Result<u64, StoreError> {
        Ok(self
            .catch_up_history(Some(task_id), CatchUpPace::Whole)
            .await?
            .indexed)
    }

    /// Index every session of the store that is behind, whatever its task:
    /// what a project-wide search reads. A store holds one project, and the
    /// project stays a filter of the search.
    ///
    /// The first call indexes the conversations no search reached before;
    /// later ones skip every caught-up session in one query.
    pub async fn index_project_history(&self) -> Result<u64, StoreError> {
        Ok(self
            .catch_up_history(None, CatchUpPace::Whole)
            .await?
            .indexed)
    }

    /// The sessions of one task (`Some`) or of the whole store (`None`)
    /// whose journal is ahead of the index, the newest first.
    async fn sessions_behind(
        &self,
        task_id: Option<&TaskId>,
    ) -> Result<Vec<SessionId>, StoreError> {
        let rows = sqlx::query(
            "SELECT session_id FROM sessions
             WHERE (?1 IS NULL OR task_id = ?1)
               AND next_sequence - 1 > COALESCE(
                   (SELECT MAX(h.sequence) FROM history_sources h
                    WHERE h.session_id = sessions.session_id), 0)
             ORDER BY created_at DESC",
        )
        .bind(task_id.map(TaskId::as_str))
        .fetch_all(&self.readers)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "list sessions behind", error)
        })?;
        rows.iter()
            .map(|row| Ok(SessionId::parse(row_get::<String>(row, "session_id")?)?))
            .collect()
    }

    /// Index one session that is behind, in a catch-up of `task_id`'s
    /// sessions or of the store's: whether it was caught up. Across the store,
    /// a session without an admitted task has nothing a search could
    /// attribute; it is skipped, not fatal to the others.
    async fn catch_up_session(
        &self,
        task_id: Option<&TaskId>,
        session_id: &SessionId,
        pace: CatchUpPace,
    ) -> Result<(u64, bool), StoreError> {
        match self.index_history_paced(session_id, pace).await {
            Err(error) if task_id.is_none() && error.code() == ErrorCode::InvalidPayload => {
                Ok((0, true))
            }
            caught => caught,
        }
    }

    /// Index the sessions of one task (`Some`) or of the whole store
    /// (`None`) that are behind, the newest first, at `pace`.
    ///
    /// A search waits for this only until its deadline: the first
    /// project-wide search of a store with months of conversations indexed all
    /// of them first and took twelve minutes. What is left is caught up by
    /// `catch_up_history_in_background`.
    pub async fn catch_up_history(
        &self,
        task_id: Option<&TaskId>,
        pace: CatchUpPace,
    ) -> Result<HistoryCatchUp, StoreError> {
        let sessions = self.sessions_behind(task_id).await?;
        let total = u64::try_from(sessions.len()).unwrap_or(u64::MAX);
        let mut caught = HistoryCatchUp::default();
        for (done, session_id) in (0_u64..).zip(&sessions) {
            if matches!(pace, CatchUpPace::Until(deadline) if Instant::now() >= deadline) {
                caught.behind = total - done;
                break;
            }
            let (indexed, finished) = self.catch_up_session(task_id, session_id, pace).await?;
            caught.indexed += indexed;
            if !finished {
                caught.behind = total - done;
                break;
            }
        }
        Ok(caught)
    }

    /// Delete some rows of the tables an older version kept its index in, or
    /// drop one that is empty: whether there was anything to delete.
    async fn retire_old_history_index(&self) -> Result<bool, StoreError> {
        use std::sync::atomic::Ordering;
        if self.history_retired.load(Ordering::Relaxed) {
            return Ok(false);
        }
        for table in RETIRED_HISTORY_TABLES {
            let exists = sqlx::query_scalar::<_, i64>(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?",
            )
            .bind(table)
            .fetch_optional(&self.readers)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageWriteFailed, "find a retired index", error)
            })?;
            if exists.is_none() {
                continue;
            }
            let fence = self.fence()?;
            let mut tx = self.begin_write(&fence).await?;
            // The table names are this file's constants, not a caller's.
            let deleted = sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {table} WHERE rowid IN (SELECT rowid FROM {table} LIMIT ?)"
            )))
            .bind(RETIRED_ROWS_PER_TRANSACTION)
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                database_error(
                    ErrorCode::StorageWriteFailed,
                    "delete a retired index",
                    error,
                )
            })?
            .rows_affected();
            if deleted == 0 {
                sqlx::query(sqlx::AssertSqlSafe(format!("DROP TABLE {table}")))
                    .execute(&mut *tx)
                    .await
                    .map_err(|error| {
                        database_error(ErrorCode::StorageWriteFailed, "drop a retired index", error)
                    })?;
            }
            tx.commit().await.map_err(|error| {
                database_error(
                    ErrorCode::StorageWriteFailed,
                    "commit a retired index",
                    error,
                )
            })?;
            return Ok(true);
        }
        self.history_retired.store(true, Ordering::Relaxed);
        Ok(false)
    }

    /// Catch the index of one task (`Some`) or of the whole store (`None`)
    /// up behind everything else, one such catch-up at a time. It does not
    /// keep the store open: a store closed meanwhile ends it, and the next
    /// search starts it again.
    pub fn catch_up_history_in_background(self: &Arc<Self>, task_id: Option<TaskId>) {
        let key = task_id
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        let running = Arc::clone(&self.history_catch_ups);
        if !running
            .lock()
            .is_ok_and(|mut running| running.insert(key.clone()))
        {
            return;
        }
        let store = Arc::downgrade(self);
        tokio::spawn(async move {
            let sessions = match store.upgrade() {
                Some(store) => store
                    .sessions_behind(task_id.as_ref())
                    .await
                    .unwrap_or_default(),
                None => Vec::new(),
            };
            // The store is held only while one session is written.
            for session_id in &sessions {
                let Some(store) = store.upgrade() else {
                    break;
                };
                if store
                    .catch_up_session(task_id.as_ref(), session_id, CatchUpPace::Background)
                    .await
                    .is_err()
                {
                    break;
                }
            }
            // Then what an older version indexed goes, a little at a time.
            let mut deleted = 0_usize;
            while let Some(store) = store.upgrade() {
                if !store.retire_old_history_index().await.unwrap_or(false) {
                    break;
                }
                deleted += 1;
                if deleted.is_multiple_of(HISTORY_BATCHES_PER_PAUSE) {
                    tokio::time::sleep(HISTORY_PAUSE).await;
                } else {
                    tokio::task::yield_now().await;
                }
            }
            if let Ok(mut running) = running.lock() {
                running.remove(&key);
            }
        });
    }

    /// Search the index within one scope, the most relevant sources first.
    ///
    /// The scope is part of the query: a source from another task is never a
    /// candidate, so no ranking decision can leak it.
    ///
    /// The rule of every search (`harness_types::search`): the sources
    /// holding every term of the query, else those holding the most terms
    /// first. Among them a source scores the BM25 IDF of the terms it holds
    /// (`scaled_idf`), so a rare word counts for more than a common one, and
    /// equal scores go newest first. Every term used to be required and the
    /// newest match came first, so one word the source put differently hid it,
    /// and a source that only repeated the query's common words came ahead of
    /// the one that answered it.
    pub async fn history_search(
        &self,
        scope: &HistoryScope,
        query: &str,
        limit: usize,
    ) -> Result<Vec<HistoryHit>, StoreError> {
        self.history_search_filtered(scope, query, limit, &HistoryFilter::default())
            .await
    }

    /// [`Self::history_search`] within a time window and among kinds of
    /// sources, equal scores in the filter's order.
    pub async fn history_search_filtered(
        &self,
        scope: &HistoryScope,
        query: &str,
        limit: usize,
        filter: &HistoryFilter,
    ) -> Result<Vec<HistoryHit>, StoreError> {
        let terms = search_terms(query);
        if terms.is_empty() {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                "history search needs at least one searchable term",
            ));
        }
        let limit = i64::try_from(limit.clamp(1, 100)).unwrap_or(20);
        let weights = self.history_term_weights(scope, &terms).await?;
        let weight_cases = (0..terms.len()).fold(String::new(), |mut cases, index| {
            let _ = write!(cases, " WHEN {index} THEN ?");
            cases
        });
        // Newest first among equal scores, unless the filter says oldest:
        // across conversations a sequence only orders one session, so the
        // session's start orders them.
        let (order, page_order) = match (scope.project_wide, filter.oldest_first) {
            (true, false) => (
                "matched DESC, weight DESC, started DESC, sequence DESC",
                "ranked.matched DESC, ranked.weight DESC, ranked.started DESC, ranked.sequence DESC",
            ),
            (true, true) => (
                "matched DESC, weight DESC, started ASC, sequence ASC",
                "ranked.matched DESC, ranked.weight DESC, ranked.started ASC, ranked.sequence ASC",
            ),
            (false, false) => (
                "matched DESC, weight DESC, sequence DESC",
                "ranked.matched DESC, ranked.weight DESC, ranked.sequence DESC",
            ),
            (false, true) => (
                "matched DESC, weight DESC, sequence ASC",
                "ranked.matched DESC, ranked.weight DESC, ranked.sequence ASC",
            ),
        };
        let (filtered, filter_values) = filter.clause();
        // The ranking reads only the index's hits; the preview is cut from
        // the few sources that make the page.
        let sql = format!(
            "WITH hits(id, term) AS ({})
             SELECT s.source_id, s.sequence, s.kind, substr(s.content, 1, ?) AS head,
                    s.content_hash, s.availability, ranked.matched, s.task_id, ranked.started
             FROM (
                 SELECT * FROM (
                     SELECT h.id AS id, COUNT(*) AS matched,
                            SUM(CASE h.term{weight_cases} ELSE 0 END) AS weight,
                            s.sequence AS sequence, COALESCE(se.created_at, '') AS started,
                            MAX(COUNT(*)) OVER () AS best
                     FROM hits h JOIN history_sources s ON s.rowid = h.id
                     LEFT JOIN sessions se ON se.session_id = s.session_id
                     WHERE {}{filtered}
                     GROUP BY h.id
                 ) WHERE matched = ? OR best < ?
                 ORDER BY {order} LIMIT ?
             ) ranked JOIN history_sources s ON s.rowid = ranked.id
             ORDER BY {page_order}",
            term_hits(terms.len()),
            scope_predicate(scope),
        );
        // The statement is assembled from a fixed template: every caller value
        // is a bind parameter, and the only thing that varies is how many `?`
        // placeholders the scope grants and the query terms need.
        let mut statement = sqlx::query(sqlx::AssertSqlSafe(sql));
        for term in &terms {
            statement = statement.bind(match_term(term));
        }
        // A hit shows a preview, never the source: fetch one character more
        // than the preview keeps, which is enough to know it was cut.
        statement = statement.bind(i64::try_from(PREVIEW_CHARS + 1).unwrap_or(i64::MAX));
        for weight in &weights {
            statement = statement.bind(*weight);
        }
        for value in scope_binds(scope) {
            statement = statement.bind(value);
        }
        for value in filter_values {
            statement = statement.bind(value);
        }
        let every = i64::try_from(terms.len()).unwrap_or(i64::MAX);
        let rows = statement
            .bind(every)
            .bind(every)
            .bind(limit)
            .fetch_all(&self.readers)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageWriteFailed, "search history", error)
            })?;
        let query_terms = u64::try_from(terms.len()).unwrap_or(u64::MAX);
        rows.iter()
            .map(|row| hit_from_row(row, query_terms))
            .collect()
    }

    /// The newest sources of a scope as semantic search embeds them: each
    /// one's head, at most `chars` characters, newest first.
    pub async fn history_texts(
        &self,
        scope: &HistoryScope,
        limit: usize,
        chars: usize,
    ) -> Result<Vec<HistoryText>, StoreError> {
        let sql = format!(
            "SELECT s.source_id, s.kind, s.content_hash, substr(s.content, 1, ?) AS head
             FROM history_sources s
             WHERE {} AND s.availability = 'available'
             ORDER BY s.rowid DESC LIMIT ?",
            scope_predicate(scope)
        );
        // A fixed template: only the scope's placeholders vary.
        let mut statement =
            sqlx::query(sqlx::AssertSqlSafe(sql)).bind(i64::try_from(chars).unwrap_or(i64::MAX));
        for value in scope_binds(scope) {
            statement = statement.bind(value);
        }
        let rows = statement
            .bind(i64::try_from(limit).unwrap_or(i64::MAX))
            .fetch_all(&self.readers)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageWriteFailed, "read history texts", error)
            })?;
        rows.iter()
            .map(|row| {
                Ok(HistoryText {
                    source_id: row_get(row, "source_id")?,
                    kind: row_get(row, "kind")?,
                    content_hash: row_get(row, "content_hash")?,
                    text: row_get(row, "head")?,
                })
            })
            .collect()
    }

    /// The hits for sources found by meaning rather than by words, in scope:
    /// they hold none of `query`'s terms the search counted (`matched=0/n`).
    pub async fn history_hits(
        &self,
        scope: &HistoryScope,
        query: &str,
        source_ids: &[String],
    ) -> Result<Vec<HistoryHit>, StoreError> {
        if source_ids.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT s.source_id, s.sequence, s.kind, substr(s.content, 1, ?) AS head,
                    s.content_hash, s.availability, 0 AS matched, s.task_id,
                    COALESCE(se.created_at, '') AS started
             FROM history_sources s LEFT JOIN sessions se ON se.session_id = s.session_id
             WHERE {} AND s.source_id IN ({})",
            scope_predicate(scope),
            vec!["?"; source_ids.len()].join(",")
        );
        // A fixed template: only the number of placeholders varies.
        let mut statement = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(i64::try_from(PREVIEW_CHARS + 1).unwrap_or(i64::MAX));
        for value in scope_binds(scope) {
            statement = statement.bind(value);
        }
        for id in source_ids {
            statement = statement.bind(id);
        }
        let rows = statement.fetch_all(&self.readers).await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "read history hits", error)
        })?;
        let query_terms = u64::try_from(search_terms(query).len()).unwrap_or(u64::MAX);
        rows.iter()
            .map(|row| hit_from_row(row, query_terms))
            .collect()
    }

    /// Each term's scaled IDF within the scope, in the order of `terms`.
    async fn history_term_weights(
        &self,
        scope: &HistoryScope,
        terms: &[String],
    ) -> Result<Vec<i64>, StoreError> {
        // A fixed template: only the number of placeholders varies.
        let mut sources = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*) FROM history_sources s WHERE {}",
            scope_predicate(scope)
        )));
        for value in scope_binds(scope) {
            sources = sources.bind(value);
        }
        let sources = sources.fetch_one(&self.readers).await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "count history sources",
                error,
            )
        })?;
        let sources = u64::try_from(sources).unwrap_or_default();
        let sql = format!(
            "WITH hits(id, term) AS ({})
             SELECT h.term AS term, COUNT(*) AS matching
             FROM hits h JOIN history_sources s ON s.rowid = h.id
             WHERE {}
             GROUP BY h.term",
            term_hits(terms.len()),
            scope_predicate(scope),
        );
        let mut statement = sqlx::query(sqlx::AssertSqlSafe(sql));
        for term in terms {
            statement = statement.bind(match_term(term));
        }
        for value in scope_binds(scope) {
            statement = statement.bind(value);
        }
        let rows = statement.fetch_all(&self.readers).await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "weigh history terms", error)
        })?;
        let mut weights = vec![0; terms.len()];
        for row in &rows {
            let term = usize::try_from(row_get::<i64>(row, "term")?).unwrap_or(usize::MAX);
            let matching = u64::try_from(row_get::<i64>(row, "matching")?).unwrap_or_default();
            if let Some(weight) = weights.get_mut(term) {
                *weight = scaled_idf(sources, matching);
            }
        }
        Ok(weights)
    }

    /// Read an exact page of one source.
    pub async fn history_read(
        &self,
        scope: &HistoryScope,
        source_id: &str,
        offset: u64,
        length: usize,
    ) -> Result<HistoryPage, StoreError> {
        let row = sqlx::query(
            "SELECT task_id, project_id, sequence, kind, content_hash, availability,
                    length(CAST(content AS BLOB)) AS total_bytes,
                    substr(CAST(content AS BLOB), ?, ?) AS page
             FROM history_sources WHERE source_id = ?",
        )
        // Only the page leaves the database, not the whole source: SQLite
        // counts a blob's substr in bytes, from 1.
        .bind(i64::try_from(offset).unwrap_or(i64::MAX).saturating_add(1))
        .bind(i64::try_from(length).unwrap_or(i64::MAX))
        .bind(source_id)
        .fetch_optional(&self.readers)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "read history source", error)
        })?;
        let Some(row) = row else {
            let nearest = self.nearest_source_ids(scope, source_id).await?;
            let reason = if nearest.is_empty() {
                "no indexed source has that id; copy a source id whole from a history_search hit"
                    .to_owned()
            } else {
                format!(
                    "no indexed source has that id; copy a source id whole from a history_search hit (the closest ids in this task: {})",
                    nearest.join(", ")
                )
            };
            return Err(StoreError::new(ErrorCode::InvalidPayload, reason));
        };
        let project_id = row_get::<String>(&row, "project_id")?;
        let task_id = row_get::<String>(&row, "task_id")?;
        let granted = scope.granted_source_ids.iter().any(|id| id == source_id);
        if project_id != scope.project_id.as_str() {
            return Err(StoreError::new(
                ErrorCode::ScopeAuthorityDenied,
                "history source belongs to another project",
            ));
        }
        if task_id != scope.task_id.as_str() && !granted && !scope.project_wide {
            return Err(StoreError::new(
                ErrorCode::ScopeAuthorityDenied,
                "history source belongs to another conversation of this project; read it with scope \"project\"",
            ));
        }
        let availability = SourceAvailability::parse(&row_get::<String>(&row, "availability")?)
            .ok_or_else(|| {
                StoreError::new(
                    ErrorCode::StorageWriteFailed,
                    "history source has an unknown availability",
                )
            })?;
        if availability != SourceAvailability::Available {
            return Err(StoreError::new(
                ErrorCode::SourceUnavailable,
                "the source behind this reference is no longer available",
            ));
        }
        let total_bytes = u64::try_from(row_get::<i64>(&row, "total_bytes")?).unwrap_or_default();
        if offset > total_bytes {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                format!("requested offset {offset} is past the {total_bytes} stored bytes"),
            ));
        }
        // The sources either side of it in its own session: the same task, so
        // in scope whenever it is.
        let neighbors = sqlx::query(
            "SELECT
               (SELECT p.source_id FROM history_sources p WHERE p.session_id = s.session_id
                  AND p.sequence < s.sequence ORDER BY p.sequence DESC LIMIT 1) AS previous,
               (SELECT n.source_id FROM history_sources n WHERE n.session_id = s.session_id
                  AND n.sequence > s.sequence ORDER BY n.sequence ASC LIMIT 1) AS next
             FROM history_sources s WHERE s.source_id = ?",
        )
        .bind(source_id)
        .fetch_optional(&self.readers)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "read history neighbors",
                error,
            )
        })?;
        let (previous_source_id, next_source_id) = match &neighbors {
            Some(row) => (
                row_get::<Option<String>>(row, "previous")?,
                row_get::<Option<String>>(row, "next")?,
            ),
            None => (None, None),
        };
        Ok(HistoryPage {
            source_id: source_id.to_owned(),
            sequence: u64::try_from(row_get::<i64>(&row, "sequence")?).unwrap_or_default(),
            kind: row_get(&row, "kind")?,
            content_hash: ContentHash::parse(row_get::<String>(&row, "content_hash")?)?,
            offset,
            bytes: row_get::<Option<Vec<u8>>>(&row, "page")?.unwrap_or_default(),
            total_bytes,
            previous_source_id,
            next_source_id,
        })
    }

    /// In-scope source ids that share the longest prefix with a mistyped one.
    ///
    /// A source id is a v7 UUID whose head is its creation time, so a model that
    /// garbled the tail of an id it was shown still has the head right (seen: a
    /// read of `event_01a0f16c-740a-7185-8a4a5bf83b` for `…-a8bf-5ae151e600bf`).
    async fn nearest_source_ids(
        &self,
        scope: &HistoryScope,
        source_id: &str,
    ) -> Result<Vec<String>, StoreError> {
        const HEAD_CHARS: usize = "event_01a0f16c".len();
        const MOST: usize = 3;
        let Some(head) = source_id.get(..HEAD_CHARS) else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query(
            "SELECT source_id FROM history_sources
             WHERE project_id = ? AND (? OR task_id = ?) AND substr(source_id, 1, ?) = ?",
        )
        .bind(scope.project_id.as_str())
        .bind(scope.project_wide)
        .bind(scope.task_id.as_str())
        .bind(i64::try_from(HEAD_CHARS).unwrap_or(i64::MAX))
        .bind(head)
        .fetch_all(&self.readers)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "list history sources", error)
        })?;
        let mut candidates = rows
            .iter()
            .map(|row| row_get::<String>(row, "source_id"))
            .collect::<Result<Vec<_>, _>>()?;
        let shared = |candidate: &str| {
            candidate
                .bytes()
                .zip(source_id.bytes())
                .take_while(|(left, right)| left == right)
                .count()
        };
        candidates.sort_by_key(|candidate| std::cmp::Reverse(shared(candidate)));
        candidates.truncate(MOST);
        Ok(candidates)
    }

    /// The project a session belongs to.
    ///
    /// The project identity lives in the task's working state, not on the
    /// session row: a session belongs to a task, and the task owns the
    /// workspace observation.
    pub async fn session_project(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<ProjectId>, StoreError> {
        let Some(task_id) = self.session_task(session_id).await? else {
            return Ok(None);
        };
        let state = self.current_projection(&task_id).await?.ok_or_else(|| {
            StoreError::new(
                ErrorCode::InvalidPayload,
                "session has no admitted task projection",
            )
        })?;
        Ok(Some(state.workspace.project_id))
    }

    /// Every indexed source id of one session, oldest first.
    pub async fn history_source_ids(
        &self,
        session_id: &SessionId,
        limit: usize,
    ) -> Result<Vec<String>, StoreError> {
        let rows = sqlx::query(
            "SELECT source_id FROM history_sources WHERE session_id = ? ORDER BY sequence LIMIT ?",
        )
        .bind(session_id.as_str())
        .bind(i64::try_from(limit.clamp(1, 1000)).unwrap_or(200))
        .fetch_all(&self.readers)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "list history sources", error)
        })?;
        rows.iter().map(|row| row_get(row, "source_id")).collect()
    }

    /// Sources a session inherited through a fork, if any.
    pub async fn history_grants(&self, session_id: &SessionId) -> Result<Vec<String>, StoreError> {
        let row = sqlx::query("SELECT fork_policy FROM session_lineage WHERE new_session_id = ?")
            .bind(session_id.as_str())
            .fetch_optional(&self.readers)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageWriteFailed, "read fork policy", error)
            })?;
        let Some(row) = row else {
            return Ok(Vec::new());
        };
        let policy: Option<String> = row_get(&row, "fork_policy")?;
        let Some(policy) = policy else {
            return Ok(Vec::new());
        };
        let parsed: serde_json::Value = serde_json::from_str(&policy).map_err(|_| {
            StoreError::new(
                ErrorCode::StorageWriteFailed,
                "fork policy is not readable JSON",
            )
        })?;
        Ok(parsed
            .get("allowed_source_ids")
            .and_then(serde_json::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Write or replace one note under a revision check.
    ///
    /// `expected_revision` is the revision the writer read; a mismatch means
    /// somebody else wrote the note first, which is a conflict rather than a
    /// silent overwrite.
    pub async fn upsert_note_cas(
        &self,
        session_id: &SessionId,
        task_id: &TaskId,
        key: &str,
        expected_revision: u64,
        content: &str,
        sources: &[SourceRef],
    ) -> Result<NoteRecord, StoreError> {
        let key = key.trim();
        if key.is_empty() || key.chars().count() > NOTE_KEY_LIMIT_CHARS {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                format!("a note key must be 1..={NOTE_KEY_LIMIT_CHARS} characters"),
            ));
        }
        if content.len() > NOTE_CONTENT_LIMIT_BYTES {
            return Err(StoreError::new(
                ErrorCode::OutputLimitExceeded,
                format!("a note body must be at most {NOTE_CONTENT_LIMIT_BYTES} bytes"),
            ));
        }
        for source in sources {
            source.validate()?;
        }
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let existing = sqlx::query(
            "SELECT note_id, revision FROM session_notes WHERE task_id = ? AND note_key = ?",
        )
        .bind(task_id.as_str())
        .bind(key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read note", error))?;
        let (note_id, revision) = if let Some(row) = existing {
            let current = u64::try_from(row_get::<i64>(&row, "revision")?).unwrap_or_default();
            if current != expected_revision {
                return Err(StoreError::new(
                    ErrorCode::CompactionConflict,
                    format!("note {key} is at revision {current}, not {expected_revision}"),
                ));
            }
            (row_get::<String>(&row, "note_id")?, current + 1)
        } else {
            if expected_revision != 0 {
                return Err(StoreError::new(
                    ErrorCode::CompactionConflict,
                    "note does not exist yet, so revision 0 was expected",
                ));
            }
            (format!("note_{}", uuid_like()), 1)
        };
        let sources_json = serde_json::to_string(sources).map_err(|_| {
            StoreError::new(
                ErrorCode::InvalidPayload,
                "note sources cannot be serialized",
            )
        })?;
        sqlx::query(
            "INSERT INTO session_notes(note_id, task_id, session_id, note_key, revision, content, sources_json, authority)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(task_id, note_key) DO UPDATE SET
                 revision = excluded.revision,
                 content = excluded.content,
                 sources_json = excluded.sources_json,
                 session_id = excluded.session_id,
                 updated_at = CURRENT_TIMESTAMP",
        )
        .bind(&note_id)
        .bind(task_id.as_str())
        .bind(session_id.as_str())
        .bind(key)
        .bind(to_i64(revision, "note revision")?)
        .bind(content)
        .bind(&sources_json)
        .bind(NoteRecord::AUTHORITY)
        .execute(&mut *tx)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "write note", error))?;
        tx.commit()
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "commit note", error))?;
        Ok(NoteRecord {
            note_id,
            task_id: task_id.clone(),
            session_id: session_id.clone(),
            key: key.to_owned(),
            revision,
            content: content.to_owned(),
            sources: sources.to_vec(),
            authority: NoteRecord::AUTHORITY.to_owned(),
        })
    }

    /// One note, if the task has it.
    pub async fn note(
        &self,
        task_id: &TaskId,
        key: &str,
    ) -> Result<Option<NoteRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT note_id, task_id, session_id, note_key, revision, content, sources_json, authority
             FROM session_notes WHERE task_id = ? AND note_key = ?",
        )
        .bind(task_id.as_str())
        .bind(key)
        .fetch_optional(&self.readers)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read note", error))?;
        row.map(|row| note_from_row(&row)).transpose()
    }

    async fn history_watermark(&self, session_id: &SessionId) -> Result<u64, StoreError> {
        let value = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT MAX(sequence) FROM history_sources WHERE session_id = ?",
        )
        .bind(session_id.as_str())
        .fetch_one(&self.readers)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "read history watermark",
                error,
            )
        })?;
        Ok(value
            .and_then(|value| u64::try_from(value).ok())
            .unwrap_or(0))
    }

    /// Write a batch of one session's sources and their terms in one
    /// transaction.
    ///
    /// A source that is written again (a concurrent indexer got there first)
    /// replaces its content and its terms, so the index never keeps a term
    /// the stored content no longer has.
    async fn write_history_sources(
        &self,
        owner: &SourceOwner<'_>,
        sources: &[NewSource],
        availability: SourceAvailability,
    ) -> Result<(), StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        for source in sources {
            let content_hash = ContentHash::from_bytes(source.content.as_bytes());
            let existed =
                sqlx::query_scalar::<_, i64>("SELECT 1 FROM history_sources WHERE source_id = ?")
                    .bind(&source.source_id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|error| {
                        database_error(ErrorCode::StorageWriteFailed, "read history source", error)
                    })?
                    .is_some();
            let rowid = sqlx::query_scalar::<_, i64>(
                "INSERT INTO history_sources(source_id, session_id, task_id, project_id, sequence, kind, content, content_hash, availability)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(source_id) DO UPDATE SET
                     content = excluded.content,
                     content_hash = excluded.content_hash,
                     availability = excluded.availability
                 RETURNING rowid",
            )
            .bind(&source.source_id)
            .bind(owner.session.as_str())
            .bind(owner.task.as_str())
            .bind(owner.project.as_str())
            .bind(to_i64(source.sequence, "history sequence")?)
            .bind(&source.kind)
            .bind(&source.content)
            .bind(content_hash.as_str())
            .bind(availability.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageWriteFailed, "write history source", error)
            })?;
            if existed {
                sqlx::query("DELETE FROM history_fts WHERE rowid = ?")
                    .bind(rowid)
                    .execute(&mut *tx)
                    .await
                    .map_err(|error| {
                        database_error(ErrorCode::StorageWriteFailed, "clear history terms", error)
                    })?;
            }
            // One row per source, its terms separated by spaces: the
            // tokenizer splits on nothing else (`ensure_context_schema`).
            sqlx::query("INSERT INTO history_fts(rowid, terms) VALUES (?, ?)")
                .bind(rowid)
                .bind(history_terms(&source.content).join(" "))
                .execute(&mut *tx)
                .await
                .map_err(|error| {
                    database_error(ErrorCode::StorageWriteFailed, "write history terms", error)
                })?;
        }
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit history source",
                error,
            )
        })
    }

    /// Mark a source whose journal entry is gone.
    ///
    /// Availability is derived state too: a reference stays in the index after
    /// its event is collected, and reading it says so instead of failing as if
    /// the id were unknown.
    pub async fn expire_history_source(&self, source_id: &str) -> Result<(), StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        sqlx::query("UPDATE history_sources SET availability = 'expired' WHERE source_id = ?")
            .bind(source_id)
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                database_error(
                    ErrorCode::StorageWriteFailed,
                    "expire history source",
                    error,
                )
            })?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit history expiry",
                error,
            )
        })
    }
}

/// A search hit from its row: `source_id`, `sequence`, `kind`, `head`,
/// `content_hash`, `availability`, `matched`, `task_id` and `started`.
fn hit_from_row(row: &sqlx::sqlite::SqliteRow, query_terms: u64) -> Result<HistoryHit, StoreError> {
    let head: String = row_get(row, "head")?;
    Ok(HistoryHit {
        query_terms,
        source_id: row_get(row, "source_id")?,
        sequence: u64::try_from(row_get::<i64>(row, "sequence")?).unwrap_or_default(),
        kind: row_get(row, "kind")?,
        content_hash: ContentHash::parse(row_get::<String>(row, "content_hash")?)?,
        availability: SourceAvailability::parse(&row_get::<String>(row, "availability")?)
            .ok_or_else(|| {
                StoreError::new(
                    ErrorCode::StorageWriteFailed,
                    "history source has an unknown availability",
                )
            })?,
        preview: preview_of(&head),
        matched_terms: u64::try_from(row_get::<i64>(row, "matched")?).unwrap_or_default(),
        task_id: row_get(row, "task_id")?,
        started: row_get(row, "started")?,
    })
}

/// The `WHERE` part that keeps a search inside its scope, over
/// `history_sources s`; `scope_binds` are its values. A task's scope names
/// the task outright, so counting its sources reads the task's index.
fn scope_predicate(scope: &HistoryScope) -> String {
    if scope.project_wide {
        "s.project_id = ?".to_owned()
    } else {
        format!(
            "s.project_id = ? AND (s.task_id = ?{})",
            " OR s.source_id = ?".repeat(scope.granted_source_ids.len())
        )
    }
}

/// The index's rows holding each query term, as `(id, term)`: the source's
/// rowid and the term's place in the query. One `?` per term, bound with
/// `match_term`.
fn term_hits(terms: usize) -> String {
    (0..terms)
        .map(|index| format!("SELECT rowid, {index} FROM history_fts WHERE history_fts MATCH ?"))
        .collect::<Vec<_>>()
        .join(" UNION ALL ")
}

/// A term as an FTS5 query: a quoted string, so `xyz-731` or `not` is the
/// one token the index holds and never an operator.
fn match_term(term: &str) -> String {
    format!("\"{}\"", term.replace('"', "\"\""))
}

fn scope_binds(scope: &HistoryScope) -> Vec<&str> {
    let mut binds = vec![scope.project_id.as_str()];
    if !scope.project_wide {
        binds.push(scope.task_id.as_str());
        binds.extend(scope.granted_source_ids.iter().map(String::as_str));
    }
    binds
}

fn note_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<NoteRecord, StoreError> {
    let sources: Vec<SourceRef> = serde_json::from_str(&row_get::<String>(row, "sources_json")?)
        .map_err(|_| {
            StoreError::new(
                ErrorCode::StorageWriteFailed,
                "note sources are not readable JSON",
            )
        })?;
    Ok(NoteRecord {
        note_id: row_get(row, "note_id")?,
        task_id: TaskId::parse(row_get::<String>(row, "task_id")?)?,
        session_id: SessionId::parse(row_get::<String>(row, "session_id")?)?,
        key: row_get(row, "note_key")?,
        revision: u64::try_from(row_get::<i64>(row, "revision")?).unwrap_or_default(),
        content: row_get(row, "content")?,
        sources,
        authority: row_get(row, "authority")?,
    })
}

/// Characters a search hit previews.
const PREVIEW_CHARS: usize = 160;

fn preview_of(content: &str) -> String {
    let flattened = content.replace(['\n', '\r'], " ");
    let mut preview = flattened.chars().take(PREVIEW_CHARS).collect::<String>();
    if flattened.chars().count() > PREVIEW_CHARS {
        preview.push('…');
    }
    preview
}

fn truncate_bytes(text: &str, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text.to_owned(), false);
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    format!("{:x}{:x}", std::process::id(), nanos)
}

fn to_i64(value: u64, what: &str) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| {
        StoreError::new(
            ErrorCode::StorageWriteFailed,
            format!("{what} does not fit in an integer column"),
        )
    })
}

fn row_get<T>(row: &sqlx::sqlite::SqliteRow, column: &str) -> Result<T, StoreError>
where
    T: for<'r> sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite>,
{
    row.try_get(column).map_err(|error| {
        StoreError::new(
            ErrorCode::StorageWriteFailed,
            format!("history column {column} is not readable: {error}"),
        )
    })
}

#[allow(clippy::needless_pass_by_value)] // mirrors the store's own helper
fn database_error(code: ErrorCode, action: &str, error: sqlx::Error) -> StoreError {
    StoreError::new(code, format!("cannot {action}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tables an older version kept its index in go a thousand rows at a
    /// time, then the empty table is dropped, and nothing is asked again.
    #[tokio::test]
    async fn an_older_index_is_deleted_a_little_at_a_time() {
        let directory = tempfile::tempdir().expect("temp");
        let store = SqliteStore::open_writer(crate::WriterOpenOptions::new(
            directory.path(),
            harness_types::HostId::generate(),
        ))
        .await
        .expect("store");
        sqlx::query(
            "CREATE TABLE history_terms_retired (term TEXT NOT NULL, source_id TEXT NOT NULL)",
        )
        .execute(&store.pool)
        .await
        .expect("table");
        sqlx::query(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2500)
             INSERT INTO history_terms_retired SELECT 'term' || i, 'source' FROM n",
        )
        .execute(&store.pool)
        .await
        .expect("rows");
        let mut steps = 0;
        while store.retire_old_history_index().await.expect("retire") {
            steps += 1;
            assert!(steps < 10, "it ends");
        }
        assert_eq!(steps, 4, "1,000, 1,000 and 500 rows, then the drop");
        let left = sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM sqlite_master WHERE name = 'history_terms_retired'",
        )
        .fetch_optional(&store.pool)
        .await
        .expect("schema");
        assert_eq!(left, None);
        store.close().await.expect("close");
    }

    /// A source id is the journal event's id, whose head is its time: a time
    /// window is a range of ids, compared as text.
    #[test]
    fn a_time_window_is_a_range_of_source_ids() {
        let at = 1_790_000_000_000_u64;
        let source = event_at(at);
        assert!(source >= HistoryFilter::id_at(at));
        assert!(source < HistoryFilter::id_at(at + 1));
        let filter = HistoryFilter {
            since_ms: Some(at - 1_000),
            until_ms: Some(at + 1_000),
            kinds: vec!["user_message".to_owned()],
            oldest_first: false,
        };
        assert!(filter.admits(&source, "user_message"));
        assert!(!filter.admits(&source, "tool_result"));
        assert!(!filter.admits(&event_at(at + 5_000), "user_message"));
        assert!(!filter.admits(&event_at(at - 5_000), "user_message"));
        assert!(HistoryFilter::default().admits(&source, "anything"));
    }

    /// An event id made at `ms`, as the journal makes them.
    fn event_at(ms: u64) -> String {
        let uuid = uuid::Uuid::new_v7(uuid::Timestamp::from_unix(
            uuid::NoContext,
            ms / 1_000,
            u32::try_from((ms % 1_000) * 1_000_000).expect("nanos"),
        ));
        harness_types::EventId::from_uuid(uuid)
            .expect("event id")
            .to_string()
    }

    #[test]
    fn a_long_source_keeps_its_first_and_last_terms() {
        let words = (0..3000).map(|n| format!("w{n:04}")).collect::<Vec<_>>();
        let text = format!("cargo test started {} zymurgy failed", words.join(" "));
        let terms = history_terms(&text);
        assert_eq!(terms.len(), HISTORY_TERMS_PER_SOURCE);
        for kept in ["cargo", "started", "w0000", "zymurgy", "failed", "w2999"] {
            assert!(terms.contains(&kept.to_owned()), "{kept} is kept");
        }
        assert!(
            !terms.contains(&"w1500".to_owned()),
            "the middle is what goes"
        );
    }

    #[test]
    fn an_identifier_is_a_term_whole_and_by_its_parts() {
        assert_eq!(
            history_terms("see XYZ-731 now"),
            ["731", "now", "see", "xyz", "xyz-731"]
        );
        assert_eq!(
            history_terms("crates/foo.rs"),
            ["crates", "foo", "foo.rs", "rs"]
        );
    }

    #[test]
    fn a_search_for_an_identifier_looks_for_it_whole() {
        assert_eq!(search_terms("XYZ-731"), ["xyz-731"]);
        assert_eq!(
            search_terms("parser ZK-42 budget"),
            ["budget", "parser", "zk-42"]
        );
    }

    #[test]
    fn a_rare_term_weighs_more_than_a_common_one() {
        assert!(scaled_idf(1000, 2) > scaled_idf(1000, 300));
        assert!(
            scaled_idf(1000, 1000) >= 1,
            "every match counts for something"
        );
    }
}
