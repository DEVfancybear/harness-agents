//! Durable external jobs: a remote submission, the handle it minted, and the
//! one settlement that ends it.
//!
//! The whole point of this table is the moment when a host cannot tell "the
//! request failed" from "the request succeeded and the answer was lost". Three
//! guarantees make that moment survivable:
//!
//! 1. **The request is recorded before it is sent.** [`SqliteStore::record_submission`]
//!    writes the operation and the digest of its payload with no handle and no
//!    answer yet. A host that dies here leaves a row that says exactly that.
//! 2. **An open request cannot be submitted twice.** A partial unique index on
//!    `(server_id, request_digest)` for unsettled jobs makes the second attempt
//!    a typed refusal rather than a second mutation. This is the negative
//!    control A35 asks for: a retry after a timeout *fails*.
//! 3. **A settlement and its parent delivery are one transaction.** The job row
//!    and the delivery row commit together, the settlement is guarded by
//!    `settled_at_unix_ms IS NULL`, and a repeat with the same outcome is an
//!    idempotent replay - so a terminal result can be observed any number of
//!    times and delivered once.

use harness_types::ErrorCode;
use serde_json::Value;
use sqlx::Row;

use super::{SqliteStore, database_error, to_i64};
use crate::StoreError;
use crate::models::ParentDeliveryRecord;

/// The states an external job may be in locally.
///
/// `ambiguous` is not an error state: it is the honest name for "the request was
/// sent and no answer came back". Leaving it requires evidence - a handle from
/// an operator, or an explicit abandonment - never a guess.
pub const EXTERNAL_JOB_STATES: [&str; 9] = [
    "submitting",
    "accepted",
    "ambiguous",
    "cancel_requested",
    "completed",
    "failed",
    "cancelled",
    "abandoned",
    "deadline_exceeded",
];

/// The states that end a job.
pub const EXTERNAL_JOB_TERMINAL_STATES: [&str; 5] = [
    "completed",
    "failed",
    "cancelled",
    "abandoned",
    "deadline_exceeded",
];

/// One stored external job.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredExternalJob {
    pub job_id: String,
    pub parent_task_id: String,
    pub session_id: String,
    pub server_id: String,
    pub operation: String,
    pub request_json: String,
    pub request_digest: String,
    /// The remote's handle. `None` means the submission's fate is unknown.
    pub remote_task_id: Option<String>,
    pub state: String,
    pub attempt: u64,
    pub poll_interval_ms: Option<i64>,
    pub next_poll_unix_ms: i64,
    pub deadline_unix_ms: i64,
    pub submitted_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
    pub settled_at_unix_ms: Option<i64>,
    pub outcome_json: Option<String>,
    pub outcome_digest: Option<String>,
    /// The last remote status this host observed, which is not the same thing as
    /// the local state: a job can be locally settled while the remote still runs.
    pub remote_state: Option<String>,
    pub ambiguity: Option<String>,
    pub delivery_message_id: Option<String>,
}

impl StoredExternalJob {
    /// Whether this job has ended, one way or another.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        self.settled_at_unix_ms.is_some()
    }
}

/// One row of the poll log, kept so a report can show that polling was bounded
/// rather than asserted to be.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredExternalJobPoll {
    pub job_id: String,
    pub polled_at_unix_ms: i64,
    pub observed_state: String,
    pub outcome: String,
}

/// What one settlement writes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalJobSettlement {
    pub state: String,
    /// The durable record of what happened, as the parent will read it.
    pub outcome_json: String,
    pub outcome_digest: String,
    /// The last remote status observed before settling, if any was.
    pub remote_state: Option<String>,
    pub settled_at_unix_ms: i64,
}

/// What a recovery pass found.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecoveredExternalJobs {
    /// Jobs whose submission was recorded and never answered: they are now
    /// `ambiguous` and need reconciliation. They are **not** resubmitted.
    pub ambiguous: Vec<String>,
    /// Jobs that were already known to a remote and can be polled again.
    pub resumed: Vec<String>,
}

/// The delivery message id for one job outcome.
///
/// Derived from the job and the outcome digest, so a replayed settlement builds
/// the same id and the delivery table's own conflict rule catches a different
/// outcome under the same id.
#[must_use]
pub fn external_job_delivery_id(job_id: &str, outcome_digest: &str) -> String {
    format!("delivery_external_{job_id}_{outcome_digest}")
}

impl SqliteStore {
    /// Record a submission **before** it is sent.
    ///
    /// Returns a typed refusal when the same request is already open on the same
    /// server: an unresolved submission must be reconciled, never repeated.
    pub async fn record_submission(&self, job: &StoredExternalJob) -> Result<(), StoreError> {
        validate_external_job(job)?;
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let open: Option<String> = sqlx::query_scalar(
            "SELECT job_id FROM external_jobs
             WHERE server_id = ? AND request_digest = ? AND settled_at_unix_ms IS NULL",
        )
        .bind(&job.server_id)
        .bind(&job.request_digest)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "read open external job",
                error,
            )
        })?;
        if let Some(existing) = open {
            return Err(StoreError::new(
                ErrorCode::IdempotencyConflict,
                format!(
                    "the same request is already open on {} as {existing}; an unresolved submission is reconciled, not repeated",
                    job.server_id
                ),
            ));
        }
        let inserted = sqlx::query(
            "INSERT OR IGNORE INTO external_jobs(
                 job_id, parent_task_id, session_id, server_id, operation, request_json,
                 request_digest, remote_task_id, state, attempt, poll_interval_ms,
                 next_poll_unix_ms, deadline_unix_ms, submitted_at_unix_ms, updated_at_unix_ms,
                 settled_at_unix_ms, outcome_json, outcome_digest, remote_state, ambiguity,
                 delivery_message_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, NULL, ?, 0, NULL, ?, ?, ?, ?, NULL, NULL, NULL, NULL, NULL, NULL)",
        )
        .bind(&job.job_id)
        .bind(&job.parent_task_id)
        .bind(&job.session_id)
        .bind(&job.server_id)
        .bind(&job.operation)
        .bind(&job.request_json)
        .bind(&job.request_digest)
        .bind(&job.state)
        .bind(job.next_poll_unix_ms)
        .bind(job.deadline_unix_ms)
        .bind(job.submitted_at_unix_ms)
        .bind(job.submitted_at_unix_ms)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "insert external job", error)
        })?;
        if inserted.rows_affected() != 1 {
            return Err(StoreError::new(
                ErrorCode::DuplicateTaskId,
                "an external job with this id already exists",
            ));
        }
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit external job", error)
        })
    }

    /// Attach the handle a remote minted, moving the job to `accepted`.
    ///
    /// Returns `false` when the job is no longer waiting for a handle, which
    /// happens when a settlement or a reconciliation landed first.
    pub async fn attach_remote_task(
        &self,
        job_id: &str,
        remote_task_id: &str,
        poll_interval_ms: Option<i64>,
        next_poll_unix_ms: i64,
        at_unix_ms: i64,
    ) -> Result<bool, StoreError> {
        if remote_task_id.trim().is_empty() {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                "a remote task id cannot be empty",
            ));
        }
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let owner: Option<String> = sqlx::query_scalar(
            "SELECT job_id FROM external_jobs WHERE server_id = (
                 SELECT server_id FROM external_jobs WHERE job_id = ?
             ) AND remote_task_id = ? AND job_id <> ?",
        )
        .bind(job_id)
        .bind(remote_task_id)
        .bind(job_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "read remote task owner",
                error,
            )
        })?;
        if let Some(owner) = owner {
            return Err(StoreError::new(
                ErrorCode::DeliveryConflict,
                format!("remote task {remote_task_id} is already owned by {owner}"),
            ));
        }
        let updated = sqlx::query(
            "UPDATE external_jobs
             SET remote_task_id = ?, state = 'accepted', poll_interval_ms = ?,
                 next_poll_unix_ms = ?, updated_at_unix_ms = ?, ambiguity = NULL
             WHERE job_id = ? AND settled_at_unix_ms IS NULL AND state = 'submitting'",
        )
        .bind(remote_task_id)
        .bind(poll_interval_ms)
        .bind(next_poll_unix_ms)
        .bind(at_unix_ms)
        .bind(job_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "attach remote task", error)
        })?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit remote task attachment",
                error,
            )
        })?;
        Ok(updated.rows_affected() == 1)
    }

    /// Record that a submission's fate is unknown.
    ///
    /// This is a terminal answer to the *submission*, not to the job: the row
    /// stays open and needs reconciliation.
    pub async fn mark_submission_ambiguous(
        &self,
        job_id: &str,
        reason: &str,
        at_unix_ms: i64,
    ) -> Result<bool, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let updated = sqlx::query(
            "UPDATE external_jobs
             SET state = 'ambiguous', ambiguity = ?, next_poll_unix_ms = ?, updated_at_unix_ms = ?
             WHERE job_id = ? AND settled_at_unix_ms IS NULL AND state = 'submitting'",
        )
        .bind(reason)
        .bind(i64::MAX)
        .bind(at_unix_ms)
        .bind(job_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "mark submission ambiguous",
                error,
            )
        })?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit ambiguous submission",
                error,
            )
        })?;
        Ok(updated.rows_affected() == 1)
    }

    /// Resolve an ambiguous submission with a handle only a human could supply.
    ///
    /// The job resumes polling from the handle. Nothing is resubmitted: the
    /// reconciliation says *which* remote task this request became.
    pub async fn reconcile_external_job(
        &self,
        job_id: &str,
        remote_task_id: &str,
        poll_interval_ms: Option<i64>,
        next_poll_unix_ms: i64,
        at_unix_ms: i64,
    ) -> Result<bool, StoreError> {
        if remote_task_id.trim().is_empty() {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                "a reconciliation needs the remote task id",
            ));
        }
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let owner: Option<String> = sqlx::query_scalar(
            "SELECT job_id FROM external_jobs WHERE server_id = (
                 SELECT server_id FROM external_jobs WHERE job_id = ?
             ) AND remote_task_id = ? AND job_id <> ?",
        )
        .bind(job_id)
        .bind(remote_task_id)
        .bind(job_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "read remote task owner",
                error,
            )
        })?;
        if let Some(owner) = owner {
            return Err(StoreError::new(
                ErrorCode::DeliveryConflict,
                format!("remote task {remote_task_id} is already owned by {owner}"),
            ));
        }
        let updated = sqlx::query(
            "UPDATE external_jobs
             SET remote_task_id = ?, state = 'accepted', poll_interval_ms = ?,
                 next_poll_unix_ms = ?, updated_at_unix_ms = ?,
                 ambiguity = ambiguity || ' | reconciled with a remote handle'
             WHERE job_id = ? AND settled_at_unix_ms IS NULL AND state = 'ambiguous'",
        )
        .bind(remote_task_id)
        .bind(poll_interval_ms)
        .bind(next_poll_unix_ms)
        .bind(at_unix_ms)
        .bind(job_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "reconcile external job",
                error,
            )
        })?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit external job reconciliation",
                error,
            )
        })?;
        Ok(updated.rows_affected() == 1)
    }

    /// Record one non-terminal poll and the delay it earned.
    ///
    /// The poll row and the job's next due instant are one transaction, so the
    /// record of how often this host asked cannot drift from when it will ask
    /// again.
    pub async fn record_poll(
        &self,
        job_id: &str,
        observed_state: &str,
        at_unix_ms: i64,
        attempt: u64,
        next_poll_unix_ms: i64,
    ) -> Result<(), StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let updated = sqlx::query(
            "UPDATE external_jobs
             SET attempt = ?, remote_state = ?, next_poll_unix_ms = ?, updated_at_unix_ms = ?
             WHERE job_id = ? AND settled_at_unix_ms IS NULL",
        )
        .bind(to_i64(attempt, "external job attempt")?)
        .bind(observed_state)
        .bind(next_poll_unix_ms)
        .bind(at_unix_ms)
        .bind(job_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "record external poll", error)
        })?;
        if updated.rows_affected() != 1 {
            return Err(StoreError::new(
                ErrorCode::InvalidStateTransition,
                "an external job that has settled cannot be polled",
            ));
        }
        sqlx::query(
            "INSERT INTO external_job_polls(job_id, polled_at_unix_ms, observed_state, outcome)
             VALUES (?, ?, ?, 'running')",
        )
        .bind(job_id)
        .bind(at_unix_ms)
        .bind(observed_state)
        .execute(&mut *tx)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "insert poll row", error))?;
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit external poll", error)
        })
    }

    /// Ask for a cooperative cancel without settling anything.
    ///
    /// A cancel is a request: the remote's own next status is what settles the
    /// job, because a cancel that was accepted is not the same claim as an
    /// effect that was rolled back.
    pub async fn request_external_cancel(
        &self,
        job_id: &str,
        at_unix_ms: i64,
    ) -> Result<bool, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let updated = sqlx::query(
            "UPDATE external_jobs
             SET state = 'cancel_requested', next_poll_unix_ms = ?, updated_at_unix_ms = ?
             WHERE job_id = ? AND settled_at_unix_ms IS NULL
               AND state IN ('accepted', 'cancel_requested')",
        )
        .bind(at_unix_ms)
        .bind(at_unix_ms)
        .bind(job_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "request external cancel",
                error,
            )
        })?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit external cancel request",
                error,
            )
        })?;
        Ok(updated.rows_affected() == 1)
    }

    /// Settle a job and deliver its outcome to the parent, in one transaction.
    ///
    /// Returns `true` when this call did the settling and `false` when the job
    /// was already settled with the same outcome - an idempotent replay, which
    /// is what a repeated terminal poll produces. A second settlement carrying a
    /// *different* outcome is refused rather than overwriting the first.
    pub async fn settle_external_job(
        &self,
        job_id: &str,
        settlement: &ExternalJobSettlement,
        delivery: Option<&ParentDeliveryRecord>,
    ) -> Result<bool, StoreError> {
        validate_settlement(settlement)?;
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let current = read_settlement_target(&mut tx, job_id).await?;
        if current.settled_at_unix_ms.is_some() {
            return finish_replay(tx, &current, settlement).await;
        }
        guard_settlement(&current, settlement)?;
        write_settlement(&mut tx, job_id, settlement, delivery).await?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit external settlement",
                error,
            )
        })?;
        Ok(true)
    }

    pub async fn external_job(
        &self,
        job_id: &str,
    ) -> Result<Option<StoredExternalJob>, StoreError> {
        let row = sqlx::query("SELECT * FROM external_jobs WHERE job_id = ?")
            .bind(job_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageOpenFailed, "read external job", error)
            })?;
        row.map(|row| decode_external_job(&row)).transpose()
    }

    pub async fn list_external_jobs(&self) -> Result<Vec<StoredExternalJob>, StoreError> {
        let rows = sqlx::query("SELECT * FROM external_jobs ORDER BY submitted_at_unix_ms, job_id")
            .fetch_all(&self.pool)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageOpenFailed, "list external jobs", error)
            })?;
        rows.iter().map(decode_external_job).collect()
    }

    /// Unsettled jobs whose next poll is due, earliest first.
    pub async fn due_external_jobs(
        &self,
        now_unix_ms: i64,
    ) -> Result<Vec<StoredExternalJob>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM external_jobs
             WHERE settled_at_unix_ms IS NULL AND remote_task_id IS NOT NULL
               AND state IN ('accepted', 'cancel_requested') AND next_poll_unix_ms <= ?
             ORDER BY next_poll_unix_ms, job_id",
        )
        .bind(now_unix_ms)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "list due external jobs",
                error,
            )
        })?;
        rows.iter().map(decode_external_job).collect()
    }

    /// Every job that still owes an answer, including the ambiguous ones.
    pub async fn unresolved_external_jobs(&self) -> Result<Vec<StoredExternalJob>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM external_jobs WHERE settled_at_unix_ms IS NULL
             ORDER BY submitted_at_unix_ms, job_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageOpenFailed,
                "list unresolved external jobs",
                error,
            )
        })?;
        rows.iter().map(decode_external_job).collect()
    }

    pub async fn external_job_polls(
        &self,
        job_id: &str,
    ) -> Result<Vec<StoredExternalJobPoll>, StoreError> {
        let rows = sqlx::query(
            "SELECT job_id, polled_at_unix_ms, observed_state, outcome FROM external_job_polls
             WHERE job_id = ? ORDER BY poll_id",
        )
        .bind(job_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageOpenFailed, "list external polls", error)
        })?;
        Ok(rows
            .iter()
            .map(|row| StoredExternalJobPoll {
                job_id: row.get("job_id"),
                polled_at_unix_ms: row.get("polled_at_unix_ms"),
                observed_state: row.get("observed_state"),
                outcome: row.get("outcome"),
            })
            .collect())
    }

    /// Recover external work after a host starts.
    ///
    /// A job recorded but never answered becomes `ambiguous`: the host that
    /// finds it cannot know whether the remote applied it, and resubmitting is
    /// the one action that could apply it twice. A job that already has a handle
    /// resumes polling, because the handle is exactly the durable fact that makes
    /// the outcome knowable.
    ///
    /// The report lists **every** ambiguous job, not only the ones this pass
    /// moved: a submission that was already waiting for a human is still waiting
    /// for one, and a recovery report that omitted it would read as "nothing
    /// needs you".
    pub async fn recover_external_jobs(
        &self,
        now_unix_ms: i64,
    ) -> Result<RecoveredExternalJobs, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let unanswered = sqlx::query(
            "SELECT job_id FROM external_jobs
             WHERE settled_at_unix_ms IS NULL AND state = 'submitting' ORDER BY submitted_at_unix_ms",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageOpenFailed, "list unanswered submissions", error)
        })?
        .iter()
        .map(|row| row.get::<String, _>("job_id"))
        .collect::<Vec<_>>();
        if !unanswered.is_empty() {
            sqlx::query(
                "UPDATE external_jobs
                 SET state = 'ambiguous',
                     ambiguity = 'the host stopped after recording the submission and before an answer was read',
                     next_poll_unix_ms = ?, updated_at_unix_ms = ?
                 WHERE settled_at_unix_ms IS NULL AND state = 'submitting'",
            )
            .bind(i64::MAX)
            .bind(now_unix_ms)
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageWriteFailed, "recover submissions", error)
            })?;
        }
        let ambiguous = sqlx::query(
            "SELECT job_id FROM external_jobs
             WHERE settled_at_unix_ms IS NULL AND state = 'ambiguous' ORDER BY submitted_at_unix_ms",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageOpenFailed, "list ambiguous jobs", error)
        })?
        .iter()
        .map(|row| row.get::<String, _>("job_id"))
        .collect::<Vec<_>>();
        let resumed = sqlx::query(
            "SELECT job_id FROM external_jobs
             WHERE settled_at_unix_ms IS NULL AND remote_task_id IS NOT NULL
               AND state IN ('accepted', 'cancel_requested') AND next_poll_unix_ms > ?
             ORDER BY submitted_at_unix_ms",
        )
        .bind(now_unix_ms)
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageOpenFailed, "list resumable jobs", error)
        })?
        .iter()
        .map(|row| row.get::<String, _>("job_id"))
        .collect::<Vec<_>>();
        if !resumed.is_empty() {
            sqlx::query(
                "UPDATE external_jobs SET next_poll_unix_ms = ?, updated_at_unix_ms = ?
                 WHERE settled_at_unix_ms IS NULL AND remote_task_id IS NOT NULL
                   AND state IN ('accepted', 'cancel_requested') AND next_poll_unix_ms > ?",
            )
            .bind(now_unix_ms)
            .bind(now_unix_ms)
            .bind(now_unix_ms)
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageWriteFailed, "resume polling", error)
            })?;
        }
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit external recovery",
                error,
            )
        })?;
        Ok(RecoveredExternalJobs { ambiguous, resumed })
    }
}

/// The job a settlement is about to write, as the guard needs to see it.
struct SettlementTarget {
    state: String,
    settled_at_unix_ms: Option<i64>,
    outcome_digest: Option<String>,
}

/// What one settlement must satisfy before it is allowed to happen.
fn validate_settlement(settlement: &ExternalJobSettlement) -> Result<(), StoreError> {
    if !EXTERNAL_JOB_TERMINAL_STATES.contains(&settlement.state.as_str()) {
        return Err(StoreError::new(
            ErrorCode::InvalidPayload,
            "an external job settles into a terminal state",
        ));
    }
    serde_json::from_str::<Value>(&settlement.outcome_json).map_err(|_| {
        StoreError::new(
            ErrorCode::InvalidPayload,
            "an external job outcome must be JSON",
        )
    })?;
    Ok(())
}

async fn read_settlement_target(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    job_id: &str,
) -> Result<SettlementTarget, StoreError> {
    let row = sqlx::query(
        "SELECT state, settled_at_unix_ms, outcome_digest FROM external_jobs WHERE job_id = ?",
    )
    .bind(job_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| database_error(ErrorCode::StorageOpenFailed, "read external job", error))?
    .ok_or_else(|| StoreError::new(ErrorCode::TaskNotFound, "no such external job"))?;
    Ok(SettlementTarget {
        state: row.get("state"),
        settled_at_unix_ms: row.get("settled_at_unix_ms"),
        outcome_digest: row.get("outcome_digest"),
    })
}

/// A settlement on an already-settled job: the same outcome is a replay, a
/// different one is a conflict. Neither may overwrite the first.
async fn finish_replay(
    tx: sqlx::Transaction<'_, sqlx::Sqlite>,
    current: &SettlementTarget,
    settlement: &ExternalJobSettlement,
) -> Result<bool, StoreError> {
    if current.outcome_digest.as_deref() == Some(settlement.outcome_digest.as_str()) {
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit replayed settlement",
                error,
            )
        })?;
        return Ok(false);
    }
    Err(StoreError::new(
        ErrorCode::DeliveryConflict,
        "a settled external job cannot be settled again with a different outcome",
    ))
}

/// The states a settlement may be written from.
fn guard_settlement(
    current: &SettlementTarget,
    settlement: &ExternalJobSettlement,
) -> Result<(), StoreError> {
    if settlement.state == "abandoned" && current.state != "ambiguous" {
        return Err(StoreError::new(
            ErrorCode::InvalidStateTransition,
            "only an ambiguous submission can be abandoned",
        ));
    }
    if !EXTERNAL_JOB_STATES.contains(&current.state.as_str()) {
        return Err(StoreError::new(
            ErrorCode::InvalidStateTransition,
            "the external job is not in a state this settlement applies to",
        ));
    }
    Ok(())
}

/// The settlement, its parent delivery and its poll-log row, in one transaction.
async fn write_settlement(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    job_id: &str,
    settlement: &ExternalJobSettlement,
    delivery: Option<&ParentDeliveryRecord>,
) -> Result<(), StoreError> {
    sqlx::query(
        "UPDATE external_jobs
         SET state = ?, settled_at_unix_ms = ?, outcome_json = ?, outcome_digest = ?,
             remote_state = COALESCE(?, remote_state), next_poll_unix_ms = ?,
             updated_at_unix_ms = ?, delivery_message_id = ?
         WHERE job_id = ? AND settled_at_unix_ms IS NULL",
    )
    .bind(&settlement.state)
    .bind(settlement.settled_at_unix_ms)
    .bind(&settlement.outcome_json)
    .bind(&settlement.outcome_digest)
    .bind(settlement.remote_state.as_deref())
    .bind(i64::MAX)
    .bind(settlement.settled_at_unix_ms)
    .bind(delivery.map(|record| record.message_id.clone()))
    .bind(job_id)
    .execute(&mut **tx)
    .await
    .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "settle external job", error))?;
    if let Some(delivery) = delivery {
        super::delegation::insert_delivery(tx, delivery).await?;
    }
    sqlx::query(
        "INSERT INTO external_job_polls(job_id, polled_at_unix_ms, observed_state, outcome)
         VALUES (?, ?, ?, ?)",
    )
    .bind(job_id)
    .bind(settlement.settled_at_unix_ms)
    .bind(
        settlement
            .remote_state
            .as_deref()
            .unwrap_or(&settlement.state),
    )
    .bind(&settlement.state)
    .execute(&mut **tx)
    .await
    .map_err(|error| {
        database_error(
            ErrorCode::StorageWriteFailed,
            "insert settlement poll",
            error,
        )
    })?;
    Ok(())
}

fn validate_external_job(job: &StoredExternalJob) -> Result<(), StoreError> {
    if job.job_id.trim().is_empty()
        || job.parent_task_id.trim().is_empty()
        || job.session_id.trim().is_empty()
        || job.server_id.trim().is_empty()
    {
        return Err(StoreError::new(
            ErrorCode::InvalidPayload,
            "an external job needs an id, a parent task, a session and a server",
        ));
    }
    if job.operation.trim().is_empty() {
        return Err(StoreError::new(
            ErrorCode::InvalidPayload,
            "an external job names the operation it submitted",
        ));
    }
    if job.request_digest.trim().is_empty() {
        return Err(StoreError::new(
            ErrorCode::InvalidPayload,
            "an external job records the digest of the request it sent",
        ));
    }
    serde_json::from_str::<Value>(&job.request_json).map_err(|_| {
        StoreError::new(
            ErrorCode::InvalidPayload,
            "an external job request must be JSON",
        )
    })?;
    if job.state != "submitting" {
        return Err(StoreError::new(
            ErrorCode::InvalidPayload,
            "a submission is recorded in the submitting state",
        ));
    }
    if job.deadline_unix_ms <= 0 || job.submitted_at_unix_ms <= 0 {
        return Err(StoreError::new(
            ErrorCode::InvalidPayload,
            "an external job needs a submission instant and a deadline",
        ));
    }
    Ok(())
}

fn decode_external_job(row: &sqlx::sqlite::SqliteRow) -> Result<StoredExternalJob, StoreError> {
    Ok(StoredExternalJob {
        job_id: row.get("job_id"),
        parent_task_id: row.get("parent_task_id"),
        session_id: row.get("session_id"),
        server_id: row.get("server_id"),
        operation: row.get("operation"),
        request_json: row.get("request_json"),
        request_digest: row.get("request_digest"),
        remote_task_id: row.get("remote_task_id"),
        state: row.get("state"),
        attempt: u64::try_from(row.get::<i64, _>("attempt")).map_err(|_| {
            StoreError::new(ErrorCode::InvalidSequence, "stored attempt is negative")
        })?,
        poll_interval_ms: row.get("poll_interval_ms"),
        next_poll_unix_ms: row.get("next_poll_unix_ms"),
        deadline_unix_ms: row.get("deadline_unix_ms"),
        submitted_at_unix_ms: row.get("submitted_at_unix_ms"),
        updated_at_unix_ms: row.get("updated_at_unix_ms"),
        settled_at_unix_ms: row.get("settled_at_unix_ms"),
        outcome_json: row.get("outcome_json"),
        outcome_digest: row.get("outcome_digest"),
        remote_state: row.get("remote_state"),
        ambiguity: row.get("ambiguity"),
        delivery_message_id: row.get("delivery_message_id"),
    })
}
