//! Durable schedules and their occurrences.
//!
//! The claim is the whole contract: `claim_occurrence` inserts the occurrence
//! row and advances the schedule's `next_due_unix_ms` in **one** transaction, so
//! a host that dies at any point either has both effects or neither. The primary
//! key is `(schedule, revision, nominal due)`, which is what makes a second
//! launch impossible rather than unlikely.

use harness_types::ErrorCode;
use sqlx::Row;

use super::{SqliteStore, database_error, to_i64};
use crate::StoreError;

/// One stored schedule, with the JSON payloads the daemon owns.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredScheduleRecord {
    pub schedule_id: String,
    pub title: String,
    pub state: String,
    pub revision: u64,
    pub next_due_unix_ms: i64,
    pub spec_json: String,
    pub grants_json: String,
}

/// One stored occurrence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredOccurrenceRecord {
    pub occurrence_key: String,
    pub schedule_id: String,
    pub revision: u64,
    pub due_unix_ms: i64,
    pub claimed_at_unix_ms: i64,
    pub state: String,
    pub trigger_kind: String,
}

impl SqliteStore {
    /// Insert a schedule, or refuse one that already exists.
    ///
    /// Re-inserting the same id is refused rather than merged: a schedule's
    /// identity is its id, and two daemons racing to create it must not end with
    /// one of them silently editing the other's.
    pub async fn create_schedule(&self, record: &StoredScheduleRecord) -> Result<(), StoreError> {
        validate_schedule(record)?;
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let now = record.next_due_unix_ms;
        let inserted = sqlx::query(
            "INSERT OR IGNORE INTO schedules(schedule_id, title, state, revision, next_due_unix_ms, spec_json, grants_json, created_at_unix_ms, updated_at_unix_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&record.schedule_id)
        .bind(&record.title)
        .bind(&record.state)
        .bind(to_i64(record.revision, "schedule revision")?)
        .bind(now)
        .bind(&record.spec_json)
        .bind(&record.grants_json)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "insert schedule", error)
        })?;
        if inserted.rows_affected() != 1 {
            return Err(StoreError::new(
                ErrorCode::DuplicateTaskId,
                "a schedule with this id already exists",
            ));
        }
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit schedule", error)
        })
    }

    pub async fn schedule(
        &self,
        schedule_id: &str,
    ) -> Result<Option<StoredScheduleRecord>, StoreError> {
        let row = sqlx::query("SELECT * FROM schedules WHERE schedule_id = ?")
            .bind(schedule_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageOpenFailed, "read schedule", error)
            })?;
        row.map(|row| decode_schedule(&row)).transpose()
    }

    pub async fn list_schedules(&self) -> Result<Vec<StoredScheduleRecord>, StoreError> {
        let rows = sqlx::query("SELECT * FROM schedules ORDER BY schedule_id")
            .fetch_all(&self.pool)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageOpenFailed, "list schedules", error)
            })?;
        rows.iter().map(decode_schedule).collect()
    }

    /// Change a schedule's state, bumping its revision.
    ///
    /// The revision is the guard a queued occurrence is checked against: a pause
    /// that lands while an occurrence waits makes the occurrence's revision stale,
    /// and dispatch refuses it.
    pub async fn set_schedule_state(
        &self,
        schedule_id: &str,
        state: &str,
        expected_revision: u64,
    ) -> Result<u64, StoreError> {
        if !matches!(state, "active" | "paused" | "deleted") {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                "invalid schedule state",
            ));
        }
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let updated = sqlx::query(
            "UPDATE schedules SET state = ?, revision = revision + 1, updated_at_unix_ms = updated_at_unix_ms
             WHERE schedule_id = ? AND revision = ?",
        )
        .bind(state)
        .bind(schedule_id)
        .bind(to_i64(expected_revision, "schedule revision")?)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "update schedule state", error)
        })?;
        if updated.rows_affected() != 1 {
            return Err(StoreError::new(
                ErrorCode::SequenceConflict,
                "the schedule revision moved; re-read it before changing its state",
            ));
        }
        let revision =
            sqlx::query_scalar::<_, i64>("SELECT revision FROM schedules WHERE schedule_id = ?")
                .bind(schedule_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|error| {
                    database_error(
                        ErrorCode::StorageOpenFailed,
                        "read schedule revision",
                        error,
                    )
                })?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit schedule state",
                error,
            )
        })?;
        u64::try_from(revision)
            .map_err(|_| StoreError::new(ErrorCode::InvalidSequence, "stored revision is negative"))
    }

    /// Claim one occurrence: the insert and the schedule's advance, together.
    ///
    /// Returns `false` when the occurrence already exists, which is the normal
    /// answer for a second host that evaluated the same schedule. That is not an
    /// error: it is the mechanism.
    pub async fn claim_occurrence(
        &self,
        occurrence: &StoredOccurrenceRecord,
        next_due_unix_ms: i64,
    ) -> Result<bool, StoreError> {
        validate_occurrence(occurrence)?;
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        // The schedule must still be at the revision the occurrence was computed
        // for. A pause that landed in between makes this claim stale, and the
        // caller is told so rather than launching.
        let current = sqlx::query("SELECT state, revision FROM schedules WHERE schedule_id = ?")
            .bind(&occurrence.schedule_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| {
                database_error(
                    ErrorCode::StorageOpenFailed,
                    "read schedule for claim",
                    error,
                )
            })?
            .ok_or_else(|| StoreError::new(ErrorCode::TaskNotFound, "schedule does not exist"))?;
        let revision = u64::try_from(current.get::<i64, _>("revision")).map_err(|_| {
            StoreError::new(ErrorCode::InvalidSequence, "stored revision is negative")
        })?;
        if revision != occurrence.revision {
            return Err(StoreError::new(
                ErrorCode::SequenceConflict,
                "the schedule revision moved before its occurrence was claimed",
            ));
        }
        if current.get::<String, _>("state") != "active" {
            return Err(StoreError::new(
                ErrorCode::PolicyDenied,
                "the schedule is not active",
            ));
        }
        let inserted = sqlx::query(
            "INSERT OR IGNORE INTO schedule_occurrences(occurrence_key, schedule_id, revision, due_unix_ms, claimed_at_unix_ms, state, trigger_kind)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&occurrence.occurrence_key)
        .bind(&occurrence.schedule_id)
        .bind(to_i64(occurrence.revision, "occurrence revision")?)
        .bind(occurrence.due_unix_ms)
        .bind(occurrence.claimed_at_unix_ms)
        .bind(&occurrence.state)
        .bind(&occurrence.trigger_kind)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "insert occurrence", error)
        })?;
        if inserted.rows_affected() == 0 {
            // Already claimed. The schedule is not advanced again: the first
            // claim already did that, and advancing twice would skip an
            // occurrence nobody ran.
            tx.commit().await.map_err(|error| {
                database_error(
                    ErrorCode::StorageWriteFailed,
                    "commit duplicate claim",
                    error,
                )
            })?;
            return Ok(false);
        }
        sqlx::query(
            "UPDATE schedules SET next_due_unix_ms = ? WHERE schedule_id = ? AND revision = ?",
        )
        .bind(next_due_unix_ms)
        .bind(&occurrence.schedule_id)
        .bind(to_i64(occurrence.revision, "occurrence revision")?)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "advance schedule", error)
        })?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit occurrence claim",
                error,
            )
        })?;
        Ok(true)
    }

    pub async fn occurrence(
        &self,
        occurrence_key: &str,
    ) -> Result<Option<StoredOccurrenceRecord>, StoreError> {
        let row = sqlx::query("SELECT * FROM schedule_occurrences WHERE occurrence_key = ?")
            .bind(occurrence_key)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageOpenFailed, "read occurrence", error)
            })?;
        row.map(|row| decode_occurrence(&row)).transpose()
    }

    pub async fn occurrences_for(
        &self,
        schedule_id: &str,
    ) -> Result<Vec<StoredOccurrenceRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM schedule_occurrences WHERE schedule_id = ? ORDER BY due_unix_ms",
        )
        .bind(schedule_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| database_error(ErrorCode::StorageOpenFailed, "list occurrences", error))?;
        rows.iter().map(decode_occurrence).collect()
    }

    /// Mark an occurrence launched, or cancel it because its schedule moved.
    pub async fn settle_occurrence(
        &self,
        occurrence_key: &str,
        state: &str,
    ) -> Result<(), StoreError> {
        if !matches!(state, "launched" | "skipped" | "canceled") {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                "invalid occurrence settlement",
            ));
        }
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let updated = sqlx::query(
            "UPDATE schedule_occurrences SET state = ? WHERE occurrence_key = ? AND state = 'claimed'",
        )
        .bind(state)
        .bind(occurrence_key)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "settle occurrence", error)
        })?;
        if updated.rows_affected() != 1 {
            return Err(StoreError::new(
                ErrorCode::InvalidStateTransition,
                "the occurrence is not in a state this settlement applies to",
            ));
        }
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit occurrence settlement",
                error,
            )
        })
    }

    /// Recover occurrences that were claimed by a host that never launched them.
    ///
    /// A claim is durable and a launch is not, so a crash between them leaves a
    /// row that says "claimed". The host that finds one cannot know whether the
    /// effect happened, and it must not launch again - that is what would double
    /// a side effect. It is marked `canceled` and reported, so an operator sees
    /// the gap instead of a silent re-run.
    pub async fn recover_claimed_occurrences(&self) -> Result<Vec<String>, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let keys = sqlx::query(
            "SELECT occurrence_key FROM schedule_occurrences WHERE state = 'claimed' ORDER BY due_unix_ms",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageOpenFailed, "list claimed occurrences", error)
        })?
        .iter()
        .map(|row| row.get::<String, _>("occurrence_key"))
        .collect::<Vec<_>>();
        if !keys.is_empty() {
            sqlx::query(
                "UPDATE schedule_occurrences SET state = 'canceled' WHERE state = 'claimed'",
            )
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                database_error(
                    ErrorCode::StorageWriteFailed,
                    "recover claimed occurrences",
                    error,
                )
            })?;
        }
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit occurrence recovery",
                error,
            )
        })?;
        Ok(keys)
    }
}

fn validate_schedule(record: &StoredScheduleRecord) -> Result<(), StoreError> {
    if record.schedule_id.trim().is_empty() || record.title.trim().is_empty() {
        return Err(StoreError::new(
            ErrorCode::InvalidPayload,
            "a schedule needs an id and a title",
        ));
    }
    if record.revision == 0 {
        return Err(StoreError::new(
            ErrorCode::InvalidPayload,
            "a schedule revision starts at one",
        ));
    }
    if record.spec_json.trim().is_empty() || record.grants_json.trim().is_empty() {
        return Err(StoreError::new(
            ErrorCode::InvalidPayload,
            "a schedule stores its specification and its grants",
        ));
    }
    Ok(())
}

fn validate_occurrence(record: &StoredOccurrenceRecord) -> Result<(), StoreError> {
    if record.occurrence_key.trim().is_empty() || record.schedule_id.trim().is_empty() {
        return Err(StoreError::new(
            ErrorCode::InvalidPayload,
            "an occurrence needs a key and a schedule",
        ));
    }
    if record.due_unix_ms <= 0 {
        return Err(StoreError::new(
            ErrorCode::InvalidPayload,
            "an occurrence needs a nominal due instant",
        ));
    }
    Ok(())
}

fn decode_schedule(row: &sqlx::sqlite::SqliteRow) -> Result<StoredScheduleRecord, StoreError> {
    Ok(StoredScheduleRecord {
        schedule_id: row.get("schedule_id"),
        title: row.get("title"),
        state: row.get("state"),
        revision: u64::try_from(row.get::<i64, _>("revision")).map_err(|_| {
            StoreError::new(ErrorCode::InvalidSequence, "stored revision is negative")
        })?,
        next_due_unix_ms: row.get("next_due_unix_ms"),
        spec_json: row.get("spec_json"),
        grants_json: row.get("grants_json"),
    })
}

fn decode_occurrence(row: &sqlx::sqlite::SqliteRow) -> Result<StoredOccurrenceRecord, StoreError> {
    Ok(StoredOccurrenceRecord {
        occurrence_key: row.get("occurrence_key"),
        schedule_id: row.get("schedule_id"),
        revision: u64::try_from(row.get::<i64, _>("revision")).map_err(|_| {
            StoreError::new(ErrorCode::InvalidSequence, "stored revision is negative")
        })?,
        due_unix_ms: row.get("due_unix_ms"),
        claimed_at_unix_ms: row.get("claimed_at_unix_ms"),
        state: row.get("state"),
        trigger_kind: row.get("trigger_kind"),
    })
}
