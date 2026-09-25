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

use std::collections::BTreeSet;

use harness_types::{ContentHash, ErrorCode, ProjectId, SessionId, SourceRef, TaskId};
use sqlx::Row;

use crate::{SqliteStore, StoreError};

/// Longest stored content per source; a journal entry larger than this keeps its
/// head and says it was cut.
pub const HISTORY_SOURCE_LIMIT_BYTES: usize = 64 * 1024;

/// Longest single term kept in the index.
const HISTORY_TERM_LIMIT_CHARS: usize = 64;

/// Most terms one source contributes.
const HISTORY_TERMS_PER_SOURCE: usize = 512;

/// Largest page `history_read` returns.
pub const HISTORY_READ_MAX_BYTES: usize = 64 * 1024;

/// Page size when a caller names none.
pub const HISTORY_READ_DEFAULT_BYTES: usize = 16 * 1024;

/// Longest note key.
pub const NOTE_KEY_LIMIT_CHARS: usize = 120;

/// Longest note body.
pub const NOTE_CONTENT_LIMIT_BYTES: usize = 16 * 1024;

/// One indexed journal entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistorySource {
    pub source_id: String,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub project_id: ProjectId,
    pub sequence: u64,
    pub kind: String,
    pub content: String,
    pub content_hash: ContentHash,
    pub availability: SourceAvailability,
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
    /// How many of the query's terms this source contains.
    pub matched_terms: u64,
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
}

impl HistoryScope {
    #[must_use]
    pub fn new(project_id: ProjectId, task_id: TaskId) -> Self {
        Self {
            project_id,
            task_id,
            granted_source_ids: Vec::new(),
        }
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
#[must_use]
pub fn history_terms(text: &str) -> Vec<String> {
    let mut terms = BTreeSet::new();
    let mut current = String::new();
    for character in text.chars() {
        if character.is_alphanumeric() {
            current.extend(character.to_lowercase());
        } else if !current.is_empty() {
            terms.insert(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        terms.insert(current);
    }
    // Joined identifiers: every run of alphanumerics joined by `-`, `_` or `.`
    // is a term of its own, so `XYZ-731` is findable whole and by its parts.
    for run in text.split(|character: char| {
        !(character.is_alphanumeric() || character == '-' || character == '_' || character == '.')
    }) {
        let joined = run.trim_matches(['-', '_', '.']).to_lowercase();
        if joined.is_empty() {
            continue;
        }
        if joined.chars().any(|character| !character.is_alphanumeric()) {
            terms.insert(joined);
        }
    }
    terms
        .into_iter()
        .filter(|term| !term.is_empty())
        .map(|term| {
            term.chars()
                .take(HISTORY_TERM_LIMIT_CHARS)
                .collect::<String>()
        })
        .take(HISTORY_TERMS_PER_SOURCE)
        .collect()
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
        let indexed_through = self.history_watermark(session_id).await?;
        let rows = sqlx::query(
            "SELECT sequence, event_id, event_json FROM events
             WHERE session_id = ? AND sequence > ? ORDER BY sequence",
        )
        .bind(session_id.as_str())
        .bind(to_i64(indexed_through, "history watermark")?)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "read journal for the index",
                error,
            )
        })?;
        let mut written = 0_u64;
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
            self.write_history_source(
                session_id,
                &event_id,
                sequence,
                &kind,
                &content,
                SourceAvailability::Available,
            )
            .await?;
            written += 1;
        }
        Ok(written)
    }

    /// Drop a session's index and rebuild it from the journal.
    ///
    /// This is the operation that must be indistinguishable from incremental
    /// indexing: same sources, same terms, same results.
    pub async fn rebuild_history_index(&self, session_id: &SessionId) -> Result<u64, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        sqlx::query(
            "DELETE FROM history_terms WHERE source_id IN (SELECT source_id FROM history_sources WHERE session_id = ?)",
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
    pub async fn index_task_history(&self, task_id: &TaskId) -> Result<u64, StoreError> {
        let rows =
            sqlx::query("SELECT session_id FROM sessions WHERE task_id = ? ORDER BY created_at")
                .bind(task_id.as_str())
                .fetch_all(&self.pool)
                .await
                .map_err(|error| {
                    database_error(ErrorCode::StorageWriteFailed, "list task sessions", error)
                })?;
        let mut indexed = 0_u64;
        for row in rows {
            let session_id = SessionId::parse(row_get::<String>(&row, "session_id")?)?;
            indexed += self.index_history(&session_id).await?;
        }
        Ok(indexed)
    }

    /// Search the index within one scope.
    ///
    /// The scope is part of the query: a source from another task is never a
    /// candidate, so no ranking decision can leak it.
    pub async fn history_search(
        &self,
        scope: &HistoryScope,
        query: &str,
        limit: usize,
    ) -> Result<Vec<HistoryHit>, StoreError> {
        let terms = history_terms(query);
        if terms.is_empty() {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                "history search needs at least one searchable term",
            ));
        }
        let limit = i64::try_from(limit.clamp(1, 100)).unwrap_or(20);
        let mut sql = String::from(
            "SELECT s.source_id, s.sequence, s.kind, s.content, s.content_hash, s.availability,
                    COUNT(DISTINCT t.term) AS matched
             FROM history_sources s JOIN history_terms t ON t.source_id = s.source_id
             WHERE s.project_id = ? AND (s.task_id = ?",
        );
        for index in 0..scope.granted_source_ids.len() {
            let _ = index;
            sql.push_str(" OR s.source_id = ?");
        }
        sql.push_str(") AND t.term IN (");
        for index in 0..terms.len() {
            if index > 0 {
                sql.push(',');
            }
            sql.push('?');
        }
        sql.push_str(") GROUP BY s.source_id HAVING matched = ? ORDER BY s.sequence DESC LIMIT ?");
        // The statement is assembled from a fixed template: every caller value
        // is a bind parameter, and the only thing that varies is how many `?`
        // placeholders the scope grants and the query terms need.
        let mut statement = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(scope.project_id.as_str())
            .bind(scope.task_id.as_str());
        for granted in &scope.granted_source_ids {
            statement = statement.bind(granted);
        }
        for term in &terms {
            statement = statement.bind(term);
        }
        let rows = statement
            .bind(i64::try_from(terms.len()).unwrap_or(i64::MAX))
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageWriteFailed, "search history", error)
            })?;
        rows.iter()
            .map(|row| {
                let content: String = row_get(row, "content")?;
                Ok(HistoryHit {
                    source_id: row_get(row, "source_id")?,
                    sequence: u64::try_from(row_get::<i64>(row, "sequence")?).unwrap_or_default(),
                    kind: row_get(row, "kind")?,
                    content_hash: ContentHash::parse(row_get::<String>(row, "content_hash")?)?,
                    availability: SourceAvailability::parse(&row_get::<String>(
                        row,
                        "availability",
                    )?)
                    .ok_or_else(|| {
                        StoreError::new(
                            ErrorCode::StorageWriteFailed,
                            "history source has an unknown availability",
                        )
                    })?,
                    preview: preview_of(&content),
                    matched_terms: u64::try_from(row_get::<i64>(row, "matched")?)
                        .unwrap_or_default(),
                })
            })
            .collect()
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
            "SELECT session_id, task_id, project_id, sequence, kind, content, content_hash, availability
             FROM history_sources WHERE source_id = ?",
        )
        .bind(source_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "read history source", error)
        })?;
        let Some(row) = row else {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                "no indexed source has that id",
            ));
        };
        let project_id = row_get::<String>(&row, "project_id")?;
        let task_id = row_get::<String>(&row, "task_id")?;
        let granted = scope.granted_source_ids.iter().any(|id| id == source_id);
        if project_id != scope.project_id.as_str()
            || (task_id != scope.task_id.as_str() && !granted)
        {
            return Err(StoreError::new(
                ErrorCode::ScopeAuthorityDenied,
                "history source belongs to another task scope",
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
        let content: String = row_get(&row, "content")?;
        let total_bytes = u64::try_from(content.len()).unwrap_or(u64::MAX);
        if offset > total_bytes {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                format!("requested offset {offset} is past the {total_bytes} stored bytes"),
            ));
        }
        let room = usize::try_from(total_bytes - offset).unwrap_or(usize::MAX);
        let end = offset.saturating_add(u64::try_from(length.min(room)).unwrap_or(u64::MAX));
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        let end = usize::try_from(end).unwrap_or(usize::MAX);
        Ok(HistoryPage {
            source_id: source_id.to_owned(),
            sequence: u64::try_from(row_get::<i64>(&row, "sequence")?).unwrap_or_default(),
            kind: row_get(&row, "kind")?,
            content_hash: ContentHash::parse(row_get::<String>(&row, "content_hash")?)?,
            offset,
            bytes: content
                .as_bytes()
                .get(start..end)
                .map(<[u8]>::to_vec)
                .unwrap_or_default(),
            total_bytes,
        })
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
        .fetch_all(&self.pool)
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
            .fetch_optional(&self.pool)
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
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read note", error))?;
        row.map(|row| note_from_row(&row)).transpose()
    }

    async fn history_watermark(&self, session_id: &SessionId) -> Result<u64, StoreError> {
        let value = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT MAX(sequence) FROM history_sources WHERE session_id = ?",
        )
        .bind(session_id.as_str())
        .fetch_one(&self.pool)
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

    async fn write_history_source(
        &self,
        session_id: &SessionId,
        source_id: &str,
        sequence: u64,
        kind: &str,
        content: &str,
        availability: SourceAvailability,
    ) -> Result<(), StoreError> {
        let task_id = self
            .session_task(session_id)
            .await?
            .ok_or_else(|| StoreError::new(ErrorCode::InvalidPayload, "session does not exist"))?;
        let project_id = self
            .session_project(session_id)
            .await?
            .ok_or_else(|| StoreError::new(ErrorCode::InvalidPayload, "session has no project"))?;
        let content_hash = ContentHash::from_bytes(content.as_bytes());
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        sqlx::query(
            "INSERT INTO history_sources(source_id, session_id, task_id, project_id, sequence, kind, content, content_hash, availability)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(source_id) DO UPDATE SET
                 content = excluded.content,
                 content_hash = excluded.content_hash,
                 availability = excluded.availability",
        )
        .bind(source_id)
        .bind(session_id.as_str())
        .bind(task_id.as_str())
        .bind(project_id.as_str())
        .bind(to_i64(sequence, "history sequence")?)
        .bind(kind)
        .bind(content)
        .bind(content_hash.as_str())
        .bind(availability.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "write history source", error)
        })?;
        sqlx::query("DELETE FROM history_terms WHERE source_id = ?")
            .bind(source_id)
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageWriteFailed, "clear history terms", error)
            })?;
        for term in history_terms(content) {
            sqlx::query(
                "INSERT INTO history_terms(term, source_id) VALUES (?, ?) ON CONFLICT DO NOTHING",
            )
            .bind(term)
            .bind(source_id)
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageWriteFailed, "write history term", error)
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

fn preview_of(content: &str) -> String {
    const PREVIEW_CHARS: usize = 160;
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
