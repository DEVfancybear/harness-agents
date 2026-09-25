//! Schedule approvals: the durable record of a scheduled run that is waiting for
//! a human.
//!
//! A schedule carries the authority of the user who created it and no more, and
//! a scheduled launch has nobody attached to approve anything. So a launch that
//! would mutate a workspace is not launched and not skipped: it is **claimed**
//! (so it can never launch twice), asked about, and left `waiting` until a
//! decision or the end of its window.
//!
//! The approval is keyed by the occurrence, not by the schedule: one answer
//! resolves one run. A pause bumps the schedule's revision, which makes the
//! occurrence stale, and a stale occurrence is never launched - so a decision
//! cannot outlive the thing it was about.

use harness_types::ErrorCode;
use sqlx::Row;

use super::{SqliteStore, database_error, to_i64};
use crate::StoreError;

/// The states an approval may be in.
pub const APPROVAL_STATES: [&str; 4] = ["open", "approved", "denied", "expired"];

/// One durable approval request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredApproval {
    pub approval_id: String,
    /// The occurrence this decision is about, which is also its dedupe key.
    pub occurrence_key: String,
    pub schedule_id: String,
    /// The schedule revision the approval was requested at. A later revision
    /// makes both the occurrence and this request stale.
    pub revision: u64,
    pub state: String,
    pub prompt: String,
    pub requested_at_unix_ms: i64,
    pub expires_at_unix_ms: i64,
    pub decided_at_unix_ms: Option<i64>,
    pub decided_by: Option<String>,
    pub reason: Option<String>,
}

impl StoredApproval {
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.state == "open"
    }
}

impl SqliteStore {
    /// Ask about one occurrence, once.
    ///
    /// Returns `true` when this call created the request. A second request for
    /// the same occurrence is not an error and not a second question: the first
    /// answer is the answer.
    pub async fn request_approval(&self, approval: &StoredApproval) -> Result<bool, StoreError> {
        validate_approval(approval)?;
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let inserted = sqlx::query(
            "INSERT OR IGNORE INTO schedule_approvals(
                 approval_id, occurrence_key, schedule_id, revision, state, prompt,
                 requested_at_unix_ms, expires_at_unix_ms, decided_at_unix_ms, decided_by, reason)
             VALUES (?, ?, ?, ?, 'open', ?, ?, ?, NULL, NULL, NULL)",
        )
        .bind(&approval.approval_id)
        .bind(&approval.occurrence_key)
        .bind(&approval.schedule_id)
        .bind(to_i64(approval.revision, "approval revision")?)
        .bind(&approval.prompt)
        .bind(approval.requested_at_unix_ms)
        .bind(approval.expires_at_unix_ms)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "request approval", error)
        })?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit approval request",
                error,
            )
        })?;
        Ok(inserted.rows_affected() == 1)
    }

    pub async fn approval(
        &self,
        occurrence_key: &str,
    ) -> Result<Option<StoredApproval>, StoreError> {
        let row = sqlx::query("SELECT * FROM schedule_approvals WHERE occurrence_key = ?")
            .bind(occurrence_key)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageOpenFailed, "read approval", error)
            })?;
        row.map(|row| decode_approval(&row)).transpose()
    }

    /// Record a decision.
    ///
    /// Returns `false` when the request was already decided or had expired: a
    /// second answer never overwrites the first, and an expired window is not a
    /// decision.
    pub async fn decide_approval(
        &self,
        occurrence_key: &str,
        decision: &str,
        decided_by: &str,
        reason: &str,
        at_unix_ms: i64,
    ) -> Result<bool, StoreError> {
        if !matches!(decision, "approved" | "denied") {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                "an approval is decided as approved or denied",
            ));
        }
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let updated = sqlx::query(
            "UPDATE schedule_approvals
             SET state = ?, decided_at_unix_ms = ?, decided_by = ?, reason = ?
             WHERE occurrence_key = ? AND state = 'open'",
        )
        .bind(decision)
        .bind(at_unix_ms)
        .bind(decided_by)
        .bind(reason)
        .bind(occurrence_key)
        .execute(&mut *tx)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "decide approval", error))?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit approval decision",
                error,
            )
        })?;
        Ok(updated.rows_affected() == 1)
    }

    /// Close the window on an approval nobody answered.
    pub async fn expire_approval(
        &self,
        occurrence_key: &str,
        at_unix_ms: i64,
    ) -> Result<bool, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let updated = sqlx::query(
            "UPDATE schedule_approvals
             SET state = 'expired', decided_at_unix_ms = ?, reason = 'the approval window closed'
             WHERE occurrence_key = ? AND state = 'open'",
        )
        .bind(at_unix_ms)
        .bind(occurrence_key)
        .execute(&mut *tx)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "expire approval", error))?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit approval expiry",
                error,
            )
        })?;
        Ok(updated.rows_affected() == 1)
    }
}

fn validate_approval(approval: &StoredApproval) -> Result<(), StoreError> {
    if approval.approval_id.trim().is_empty()
        || approval.occurrence_key.trim().is_empty()
        || approval.schedule_id.trim().is_empty()
        || approval.prompt.trim().is_empty()
    {
        return Err(StoreError::new(
            ErrorCode::InvalidPayload,
            "an approval needs an id, an occurrence, a schedule and a prompt",
        ));
    }
    if approval.revision == 0 {
        return Err(StoreError::new(
            ErrorCode::InvalidPayload,
            "an approval names the revision it was requested at",
        ));
    }
    if approval.expires_at_unix_ms <= approval.requested_at_unix_ms {
        return Err(StoreError::new(
            ErrorCode::InvalidPayload,
            "an approval window has to end after it starts",
        ));
    }
    Ok(())
}

fn decode_approval(row: &sqlx::sqlite::SqliteRow) -> Result<StoredApproval, StoreError> {
    Ok(StoredApproval {
        approval_id: row.get("approval_id"),
        occurrence_key: row.get("occurrence_key"),
        schedule_id: row.get("schedule_id"),
        revision: u64::try_from(row.get::<i64, _>("revision")).map_err(|_| {
            StoreError::new(ErrorCode::InvalidSequence, "stored revision is negative")
        })?,
        state: row.get("state"),
        prompt: row.get("prompt"),
        requested_at_unix_ms: row.get("requested_at_unix_ms"),
        expires_at_unix_ms: row.get("expires_at_unix_ms"),
        decided_at_unix_ms: row.get("decided_at_unix_ms"),
        decided_by: row.get("decided_by"),
        reason: row.get("reason"),
    })
}
