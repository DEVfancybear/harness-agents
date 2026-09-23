//! The durable notification outbox.
//!
//! Three rules, and the table enforces all three:
//!
//! 1. **One logical delivery per change.** `(subject_kind, subject_id,
//!    change_digest)` is unique, so the same event enqueued twice - by a retried
//!    tick, by two hosts that both noticed it, by a re-render that means the same
//!    thing - is one row.
//! 2. **Nothing is sent until something is configured to send it.** A row waits
//!    as `pending` until a connector exists: the outbox is a record, not a
//!    promise, and a host with no connector sends nothing.
//! 3. **Retries are bounded.** `attempts` and `next_attempt_unix_ms` are the
//!    host's own arithmetic; after [`NOTIFICATION_MAX_ATTEMPTS`] the row is
//!    `failed` and stays visible instead of being retried forever.

use harness_types::ErrorCode;
use serde_json::Value;
use sqlx::Row;

use super::{SqliteStore, database_error};
use crate::StoreError;

/// How many times one notification may be attempted before it is left failed.
pub const NOTIFICATION_MAX_ATTEMPTS: u64 = 5;

/// The first retry delay; each further attempt doubles it.
pub const NOTIFICATION_BACKOFF_MS: i64 = 5_000;

/// The longest retry delay.
pub const NOTIFICATION_MAX_BACKOFF_MS: i64 = 300_000;

/// The states a notification may be in.
pub const NOTIFICATION_STATES: [&str; 4] = ["pending", "delivered", "failed", "canceled"];

/// One durable notification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredNotification {
    pub notification_id: String,
    /// What the notification is about: `schedule`, `occurrence`, `external_job`.
    pub subject_kind: String,
    pub subject_id: String,
    /// The digest of the *meaningful* change, which is what makes two reports of
    /// the same change one notification.
    pub change_digest: String,
    /// What kind of change it was, for a reader that filters.
    pub kind: String,
    pub payload_json: String,
    pub state: String,
    pub attempts: u64,
    pub next_attempt_unix_ms: i64,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
    pub delivered_at_unix_ms: Option<i64>,
    /// Why the row is in the state it is in: the last failure, or the reason no
    /// connector was asked.
    pub detail: Option<String>,
}

impl StoredNotification {
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self.state.as_str(), "delivered" | "failed" | "canceled")
    }
}

/// Counts a status report can show without reading every row.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OutboxCounts {
    pub pending: u64,
    pub delivered: u64,
    pub failed: u64,
    pub canceled: u64,
}

impl SqliteStore {
    /// Enqueue one notification, or report that this change is already queued.
    ///
    /// Returns `true` when this call created the row and `false` when the same
    /// change was already there. Neither answer is an error: a duplicate is the
    /// mechanism, not a mistake.
    pub async fn enqueue_notification(
        &self,
        notification: &StoredNotification,
    ) -> Result<bool, StoreError> {
        validate_notification(notification)?;
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let inserted = sqlx::query(
            "INSERT OR IGNORE INTO notification_outbox(
                 notification_id, subject_kind, subject_id, change_digest, kind, payload_json,
                 state, attempts, next_attempt_unix_ms, created_at_unix_ms, updated_at_unix_ms,
                 delivered_at_unix_ms, detail)
             VALUES (?, ?, ?, ?, ?, ?, 'pending', 0, ?, ?, ?, NULL, NULL)",
        )
        .bind(&notification.notification_id)
        .bind(&notification.subject_kind)
        .bind(&notification.subject_id)
        .bind(&notification.change_digest)
        .bind(&notification.kind)
        .bind(&notification.payload_json)
        .bind(notification.next_attempt_unix_ms)
        .bind(notification.created_at_unix_ms)
        .bind(notification.created_at_unix_ms)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "enqueue notification", error)
        })?;
        if inserted.rows_affected() == 0 {
            // The same change is already queued. A *different* change under the
            // same id would be a caller bug, so it is refused rather than
            // silently merged.
            let existing: Option<String> = sqlx::query_scalar(
                "SELECT change_digest FROM notification_outbox WHERE notification_id = ?",
            )
            .bind(&notification.notification_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageOpenFailed, "read notification", error)
            })?;
            if let Some(digest) = existing
                && digest != notification.change_digest
            {
                return Err(StoreError::new(
                    ErrorCode::IdempotencyConflict,
                    "the same notification id cannot carry a different change",
                ));
            }
            tx.commit().await.map_err(|error| {
                database_error(ErrorCode::StorageWriteFailed, "commit notification", error)
            })?;
            return Ok(false);
        }
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit notification", error)
        })?;
        Ok(true)
    }

    /// Record one delivery attempt and when the next one is due.
    ///
    /// `state` is `delivered` or `failed` for a terminal attempt, `pending` for
    /// one that will be tried again. The attempt count is the host's, so a
    /// caller cannot pretend a notification was tried fewer times than it was.
    pub async fn record_notification_attempt(
        &self,
        notification_id: &str,
        state: &str,
        detail: &str,
        next_attempt_unix_ms: Option<i64>,
        at_unix_ms: i64,
    ) -> Result<bool, StoreError> {
        if !NOTIFICATION_STATES.contains(&state) {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                "invalid notification state",
            ));
        }
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let updated = sqlx::query(
            "UPDATE notification_outbox
             SET state = ?, attempts = attempts + 1, detail = ?,
                 next_attempt_unix_ms = COALESCE(?, next_attempt_unix_ms),
                 updated_at_unix_ms = ?,
                 delivered_at_unix_ms = CASE WHEN ? = 'delivered' THEN ? ELSE delivered_at_unix_ms END
             WHERE notification_id = ? AND state IN ('pending', 'failed')",
        )
        .bind(state)
        .bind(detail)
        .bind(next_attempt_unix_ms)
        .bind(at_unix_ms)
        .bind(state)
        .bind(at_unix_ms)
        .bind(notification_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "record notification attempt",
                error,
            )
        })?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit notification attempt",
                error,
            )
        })?;
        Ok(updated.rows_affected() == 1)
    }

    /// Stop trying to deliver one notification, saying why.
    pub async fn cancel_notification(
        &self,
        notification_id: &str,
        reason: &str,
        at_unix_ms: i64,
    ) -> Result<bool, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let updated = sqlx::query(
            "UPDATE notification_outbox
             SET state = 'canceled', detail = ?, updated_at_unix_ms = ?
             WHERE notification_id = ? AND state IN ('pending', 'failed')",
        )
        .bind(reason)
        .bind(at_unix_ms)
        .bind(notification_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "cancel notification", error)
        })?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit notification cancellation",
                error,
            )
        })?;
        Ok(updated.rows_affected() == 1)
    }

    pub async fn notification(
        &self,
        notification_id: &str,
    ) -> Result<Option<StoredNotification>, StoreError> {
        let row = sqlx::query("SELECT * FROM notification_outbox WHERE notification_id = ?")
            .bind(notification_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageOpenFailed, "read notification", error)
            })?;
        row.map(|row| decode_notification(&row)).transpose()
    }

    pub async fn list_notifications(&self) -> Result<Vec<StoredNotification>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM notification_outbox ORDER BY created_at_unix_ms, notification_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageOpenFailed, "list notifications", error)
        })?;
        rows.iter().map(decode_notification).collect()
    }

    /// Pending notifications whose next attempt is due.
    pub async fn due_notifications(
        &self,
        now_unix_ms: i64,
    ) -> Result<Vec<StoredNotification>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM notification_outbox
             WHERE state = 'pending' AND next_attempt_unix_ms <= ?
             ORDER BY next_attempt_unix_ms, notification_id",
        )
        .bind(now_unix_ms)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "list due notifications",
                error,
            )
        })?;
        rows.iter().map(decode_notification).collect()
    }

    pub async fn outbox_counts(&self) -> Result<OutboxCounts, StoreError> {
        let rows =
            sqlx::query("SELECT state, COUNT(*) AS total FROM notification_outbox GROUP BY state")
                .fetch_all(&self.pool)
                .await
                .map_err(|error| {
                    database_error(ErrorCode::StorageOpenFailed, "count notifications", error)
                })?;
        let mut counts = OutboxCounts::default();
        for row in &rows {
            let state: String = row.get("state");
            let total = u64::try_from(row.get::<i64, _>("total")).unwrap_or(0);
            match state.as_str() {
                "pending" => counts.pending = total,
                "delivered" => counts.delivered = total,
                "failed" => counts.failed = total,
                "canceled" => counts.canceled = total,
                _ => {}
            }
        }
        Ok(counts)
    }
}

fn validate_notification(notification: &StoredNotification) -> Result<(), StoreError> {
    if notification.notification_id.trim().is_empty()
        || notification.subject_kind.trim().is_empty()
        || notification.subject_id.trim().is_empty()
        || notification.kind.trim().is_empty()
    {
        return Err(StoreError::new(
            ErrorCode::InvalidPayload,
            "a notification needs an id, a subject and a kind",
        ));
    }
    if notification.change_digest.trim().is_empty() {
        return Err(StoreError::new(
            ErrorCode::InvalidPayload,
            "a notification records the digest of the change it reports",
        ));
    }
    serde_json::from_str::<Value>(&notification.payload_json).map_err(|_| {
        StoreError::new(
            ErrorCode::InvalidPayload,
            "a notification payload must be JSON",
        )
    })?;
    Ok(())
}

fn decode_notification(row: &sqlx::sqlite::SqliteRow) -> Result<StoredNotification, StoreError> {
    Ok(StoredNotification {
        notification_id: row.get("notification_id"),
        subject_kind: row.get("subject_kind"),
        subject_id: row.get("subject_id"),
        change_digest: row.get("change_digest"),
        kind: row.get("kind"),
        payload_json: row.get("payload_json"),
        state: row.get("state"),
        attempts: u64::try_from(row.get::<i64, _>("attempts")).map_err(|_| {
            StoreError::new(ErrorCode::InvalidSequence, "stored attempts are negative")
        })?,
        next_attempt_unix_ms: row.get("next_attempt_unix_ms"),
        created_at_unix_ms: row.get("created_at_unix_ms"),
        updated_at_unix_ms: row.get("updated_at_unix_ms"),
        delivered_at_unix_ms: row.get("delivered_at_unix_ms"),
        detail: row.get("detail"),
    })
}

/// The retry delay for one attempt count, bounded and growing.
#[must_use]
pub fn notification_backoff_ms(attempts: u64) -> i64 {
    let mut delay = NOTIFICATION_BACKOFF_MS;
    for _ in 0..attempts.min(16) {
        delay = delay.saturating_mul(2);
        if delay >= NOTIFICATION_MAX_BACKOFF_MS {
            return NOTIFICATION_MAX_BACKOFF_MS;
        }
    }
    delay.min(NOTIFICATION_MAX_BACKOFF_MS)
}
