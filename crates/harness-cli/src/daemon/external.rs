//! The daemon's side of an external job: submit it, poll it, settle it once.
//!
//! This module is the *consumer* M11-03 owes. The store records what happened
//! ([`harness_store_sqlite`]), the vocabulary is protocol-neutral
//! ([`harness_extensions::tasks`]), and this is where the two meet a clock:
//!
//! - **Nothing is sent twice.** A submission is recorded before it is sent, an
//!   open request digest cannot be submitted again, and a send whose answer was
//!   lost leaves the job `ambiguous` - a state only a human with a handle, or an
//!   explicit abandonment, can leave.
//! - **Polling is arithmetic, not a loop.** Every poll has earned delay, a
//!   ceiling and a deadline, so a remote that never finishes cannot hold a task
//!   of the host's open forever.
//! - **A settlement is one transaction with its delivery.** A terminal result
//!   observed twice is delivered once.
//! - **A cancel is a request.** The remote's own status is what settles the job,
//!   because an accepted cancel is not a rolled-back effect.

use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

use harness_extensions::tasks::{
    DEFAULT_TASK_DEADLINE_MS, PollDecision, RemoteTaskSnapshot, RemoteTaskState, SubmitFailure,
    TaskRemote, TaskSubmission, decide_poll, next_poll_delay_ms,
};
use harness_store_sqlite::{
    ExternalJobSettlement, ParentDeliveryRecord, RecoveredExternalJobs, SqliteStore,
    StoredExternalJob, external_job_delivery_id,
};
use harness_types::{ContentHash, ErrorCode, HarnessError, SessionId, TaskId};
use serde_json::{Value, json};

use super::Clock;

/// How the daemon reaches the servers its jobs name.
///
/// A job records *which* server it belongs to, never how to start one: the
/// process that runs a remote server is the host's decision, made once, and a
/// durable row must not be able to launch anything on its own.
pub trait TaskRemoteResolver: Send + Sync {
    fn resolve(&self, server_id: &str) -> Option<Arc<dyn TaskRemote>>;
}

/// The resolver the daemon is wired with: one transport per server id.
///
/// The table is behind a lock rather than owned outright because a reconnection
/// is a runtime event: the same logical server comes back as a new process, and
/// the jobs that name it must not care which process answers.
#[derive(Default)]
pub struct AttachedRemotes {
    remotes: RwLock<BTreeMap<String, Arc<dyn TaskRemote>>>,
}

impl AttachedRemotes {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach one transport. A second attachment under the same id replaces the
    /// first, which is what a reconnection looks like.
    pub fn attach(&self, remote: Arc<dyn TaskRemote>) {
        if let Ok(mut remotes) = self.remotes.write() {
            remotes.insert(remote.server_id().to_owned(), remote);
        }
    }

    /// Detach one server, so a job that names it waits instead of failing.
    pub fn detach(&self, server_id: &str) -> bool {
        self.remotes
            .write()
            .is_ok_and(|mut remotes| remotes.remove(server_id).is_some())
    }

    #[must_use]
    pub fn labels(&self) -> Vec<String> {
        self.remotes
            .read()
            .map(|remotes| remotes.keys().cloned().collect())
            .unwrap_or_default()
    }
}

impl TaskRemoteResolver for AttachedRemotes {
    fn resolve(&self, server_id: &str) -> Option<Arc<dyn TaskRemote>> {
        self.remotes
            .read()
            .ok()
            .and_then(|remotes| remotes.get(server_id).cloned())
    }
}

/// One submission, as the caller asks for it.
#[derive(Clone, Debug)]
pub struct NewExternalJob {
    pub job_id: String,
    pub parent_task_id: String,
    pub session_id: String,
    pub server_id: String,
    pub operation: String,
    pub request: Value,
    /// How long the job may stay unresolved before this host stops polling it.
    pub deadline_ms: u64,
}

/// What a submission came to.
#[derive(Clone, Debug, PartialEq)]
pub struct SubmitReceipt {
    pub job_id: String,
    /// `accepted`, `completed`, `failed` or `ambiguous`.
    pub state: String,
    pub remote_task_id: Option<String>,
    /// Why the submission's fate is unknown, when it is.
    pub ambiguity: Option<String>,
    pub outcome: Option<Value>,
}

impl SubmitReceipt {
    #[must_use]
    pub fn is_ambiguous(&self) -> bool {
        self.state == "ambiguous"
    }
}

/// What one polling pass did.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PollReport {
    /// Jobs this pass looked at.
    pub polled: usize,
    /// Jobs that reached a terminal state in this pass.
    pub settled: Vec<String>,
    /// Jobs that ran out of deadline in this pass.
    pub deadline_exceeded: Vec<String>,
    /// Jobs left for a human: an ambiguous submission, or a server this host
    /// cannot reach.
    pub waiting: Vec<String>,
}

/// What a cancel came to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CancelReceipt {
    pub job_id: String,
    /// Whether the remote acknowledged the cancel request.
    pub acknowledged: bool,
    /// The state the job is in after the request. Never `cancelled` unless the
    /// remote said so: an accepted cancel is not a rolled-back effect.
    pub state: String,
}

/// The daemon's external-task worker.
pub struct ExternalTaskRunner {
    store: Arc<SqliteStore>,
    remotes: Arc<dyn TaskRemoteResolver>,
    clock: Arc<dyn Clock>,
}

impl ExternalTaskRunner {
    #[must_use]
    pub fn new(
        store: Arc<SqliteStore>,
        remotes: Arc<dyn TaskRemoteResolver>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
            remotes,
            clock,
        }
    }

    /// Record a submission and send it once.
    ///
    /// The order is the guarantee: the request is durable before it leaves, so a
    /// host that dies in between leaves evidence rather than silence. An
    /// unresolved submission of the same request is refused by the store, and a
    /// send whose answer was lost is recorded as ambiguous instead of retried.
    ///
    /// # Errors
    /// Fails when the store refuses, when no transport is attached for the
    /// server, or when the same request is already open.
    pub async fn submit(&self, job: NewExternalJob) -> Result<SubmitReceipt, HarnessError> {
        let remote = self.remotes.resolve(&job.server_id).ok_or_else(|| {
            HarnessError::new(
                ErrorCode::ExtensionNotFound,
                format!(
                    "no transport for {} is attached; nothing was recorded and nothing was sent",
                    job.server_id
                ),
            )
        })?;
        let now = self.clock.now_unix_ms();
        let record = submission_record(&job, now)?;
        let deadline_unix_ms = record.deadline_unix_ms;
        // Durable first, sent second. Everything below this line may have
        // happened at the remote; nothing above it has left the host.
        self.store
            .record_submission(&record)
            .await
            .map_err(store_error)?;
        match remote.submit(&job.operation, &job.request).await {
            Ok(TaskSubmission::Accepted {
                remote_task_id,
                state,
                poll_interval_ms,
            }) => {
                self.accept(
                    &job,
                    &remote_task_id,
                    state,
                    poll_interval_ms,
                    now,
                    deadline_unix_ms,
                )
                .await
            }
            Ok(TaskSubmission::Completed { result }) => {
                self.settle_by_id(&job.job_id, "completed", None, &result, now)
                    .await?;
                Ok(SubmitReceipt {
                    job_id: job.job_id,
                    state: "completed".to_owned(),
                    remote_task_id: None,
                    ambiguity: None,
                    outcome: Some(result),
                })
            }
            Err(SubmitFailure::Definite(message)) => {
                // The request never left the host - the transport refused an
                // operation it does not serve, or the host refused its own
                // arguments - so nothing was applied and the job is honestly
                // over. A fresh attempt is allowed because this one produced no
                // effect, not because a remote was asked twice.
                self.settle_by_id(
                    &job.job_id,
                    "failed",
                    None,
                    &json!({"refused_before_send": message}),
                    now,
                )
                .await?;
                Ok(SubmitReceipt {
                    job_id: job.job_id,
                    state: "failed".to_owned(),
                    remote_task_id: None,
                    ambiguity: None,
                    outcome: Some(json!({"refused_before_send": message})),
                })
            }
            Err(SubmitFailure::Ambiguous(message)) => {
                // The one case that must never be retried automatically.
                self.store
                    .mark_submission_ambiguous(&job.job_id, &message, now)
                    .await
                    .map_err(store_error)?;
                Ok(SubmitReceipt {
                    job_id: job.job_id,
                    state: "ambiguous".to_owned(),
                    remote_task_id: None,
                    ambiguity: Some(message),
                    outcome: None,
                })
            }
        }
    }

    /// Record the handle a remote minted, and the delay its first poll earned.
    async fn accept(
        &self,
        job: &NewExternalJob,
        remote_task_id: &str,
        state: RemoteTaskState,
        poll_interval_ms: Option<u64>,
        now: i64,
        deadline_unix_ms: i64,
    ) -> Result<SubmitReceipt, HarnessError> {
        let delay = next_poll_delay_ms(0, poll_interval_ms, deadline_unix_ms.saturating_sub(now));
        let attached = self
            .store
            .attach_remote_task(
                &job.job_id,
                remote_task_id,
                poll_interval_ms.map(|value| i64::try_from(value).unwrap_or(i64::MAX)),
                now.saturating_add(delay),
                now,
            )
            .await
            .map_err(store_error)?;
        if !attached {
            // The remote minted a handle this host could not record. The job
            // stays `submitting`, so recovery turns it into an ambiguous
            // submission rather than pretending it was never sent - and the
            // handle is in the message, where a human can use it to reconcile.
            return Err(HarnessError::new(
                ErrorCode::InvalidStateTransition,
                format!(
                    "the remote accepted the submission as {remote_task_id} but the job could not record the handle"
                ),
            ));
        }
        Ok(SubmitReceipt {
            job_id: job.job_id.clone(),
            state: "accepted".to_owned(),
            remote_task_id: Some(remote_task_id.to_owned()),
            ambiguity: None,
            outcome: Some(json!({"remote_state": state.as_str()})),
        })
    }

    /// Poll every job that is due, once each.
    ///
    /// # Errors
    /// Fails when the store refuses. A remote that cannot be reached is not an
    /// error: it is a job that waits, and it is reported as waiting.
    pub async fn poll_due(&self) -> Result<PollReport, HarnessError> {
        let now = self.clock.now_unix_ms();
        let due = self
            .store
            .due_external_jobs(now)
            .await
            .map_err(store_error)?;
        let mut report = PollReport::default();
        for job in due {
            self.poll_job(&job, now, &mut report).await?;
        }
        Ok(report)
    }

    /// Poll one job by id, whether or not it is due.
    ///
    /// # Errors
    /// Fails when the store refuses, or when the job does not exist.
    pub async fn poll_one(&self, job_id: &str) -> Result<PollReport, HarnessError> {
        let job = self
            .store
            .external_job(job_id)
            .await
            .map_err(store_error)?
            .ok_or_else(|| HarnessError::new(ErrorCode::TaskNotFound, "no such external job"))?;
        let now = self.clock.now_unix_ms();
        let mut report = PollReport::default();
        self.poll_job(&job, now, &mut report).await?;
        Ok(report)
    }

    async fn poll_job(
        &self,
        job: &StoredExternalJob,
        now: i64,
        report: &mut PollReport,
    ) -> Result<(), HarnessError> {
        if job.is_settled() {
            return Ok(());
        }
        report.polled += 1;
        let Some(remote_task_id) = job.remote_task_id.as_deref() else {
            // An ambiguous submission has no handle to poll, and inventing a new
            // submission is exactly what must not happen here.
            report.waiting.push(job.job_id.clone());
            return Ok(());
        };
        // The deadline is checked before the poll, not after: past it, this host
        // stops asking and records that the outcome is unknown.
        if now >= job.deadline_unix_ms {
            self.settle_deadline(job, now).await?;
            report.deadline_exceeded.push(job.job_id.clone());
            return Ok(());
        }
        let Some(remote) = self.remotes.resolve(&job.server_id) else {
            self.store
                .record_poll(
                    &job.job_id,
                    "unreachable",
                    now,
                    job.attempt.saturating_add(1),
                    now.saturating_add(next_poll_delay_ms(
                        u32::try_from(job.attempt).unwrap_or(u32::MAX),
                        None,
                        job.deadline_unix_ms.saturating_sub(now),
                    )),
                )
                .await
                .map_err(store_error)?;
            report.waiting.push(job.job_id.clone());
            return Ok(());
        };
        let observed = match remote.status(remote_task_id).await {
            Ok(observed) => observed,
            Err(error) => {
                // A poll that could not be answered is not evidence about the
                // task, so the job keeps its state and simply waits longer.
                self.store
                    .record_poll(
                        &job.job_id,
                        "unreachable",
                        now,
                        job.attempt.saturating_add(1),
                        now.saturating_add(next_poll_delay_ms(
                            u32::try_from(job.attempt).unwrap_or(u32::MAX),
                            None,
                            job.deadline_unix_ms.saturating_sub(now),
                        )),
                    )
                    .await
                    .map_err(store_error)?;
                report.waiting.push(job.job_id.clone());
                let _ = error;
                return Ok(());
            }
        };
        match decide_poll(
            &observed,
            u32::try_from(job.attempt).unwrap_or(u32::MAX),
            now,
            job.deadline_unix_ms,
        ) {
            PollDecision::Settle { state } => {
                self.settle_terminal(job, &observed, state, now).await?;
                report.settled.push(job.job_id.clone());
            }
            PollDecision::Continue { delay_ms } => {
                self.store
                    .record_poll(
                        &job.job_id,
                        observed.state.as_str(),
                        now,
                        job.attempt.saturating_add(1),
                        now.saturating_add(delay_ms),
                    )
                    .await
                    .map_err(store_error)?;
            }
            PollDecision::DeadlineExceeded { .. } => {
                self.settle_deadline(job, now).await?;
                report.deadline_exceeded.push(job.job_id.clone());
            }
        }
        Ok(())
    }

    async fn settle_terminal(
        &self,
        job: &StoredExternalJob,
        observed: &RemoteTaskSnapshot,
        state: RemoteTaskState,
        now: i64,
    ) -> Result<bool, HarnessError> {
        let outcome = json!({
            "schema_version": 1,
            "job_id": job.job_id,
            "server_id": job.server_id,
            "operation": job.operation,
            "remote_task_id": observed.remote_task_id,
            "remote_state": state.as_str(),
            "status_message": observed.status_message,
            "result": observed.result,
            "error": observed.error,
            "request_digest": job.request_digest,
        });
        let settlement_state = match state {
            RemoteTaskState::Completed => "completed",
            RemoteTaskState::Failed => "failed",
            RemoteTaskState::Cancelled => "cancelled",
            RemoteTaskState::Working | RemoteTaskState::InputRequired => {
                return Err(HarnessError::new(
                    ErrorCode::InvalidStateTransition,
                    "a non-terminal observation cannot settle a job",
                ));
            }
        };
        self.settle(job, settlement_state, Some(state.as_str()), &outcome, now)
            .await?;
        Ok(true)
    }

    async fn settle_deadline(
        &self,
        job: &StoredExternalJob,
        now: i64,
    ) -> Result<bool, HarnessError> {
        // The honest record: this host stopped asking, and it does not know how
        // the remote's work ended. It is not a cancellation and not a rollback.
        let outcome = json!({
            "schema_version": 1,
            "job_id": job.job_id,
            "server_id": job.server_id,
            "operation": job.operation,
            "remote_task_id": job.remote_task_id,
            "remote_state": job.remote_state,
            "unresolved": true,
            "note": "the deadline passed with the remote still running; the outcome is unknown and nothing was rolled back",
            "request_digest": job.request_digest,
        });
        let state = job.remote_state.clone();
        self.settle(job, "deadline_exceeded", state.as_deref(), &outcome, now)
            .await
    }

    /// Write the outcome and its delivery in one transaction.
    async fn settle(
        &self,
        job: &StoredExternalJob,
        state: &str,
        remote_state: Option<&str>,
        outcome: &Value,
        now: i64,
    ) -> Result<bool, HarnessError> {
        self.settle_by_id(&job.job_id, state, remote_state, outcome, now)
            .await
    }

    async fn settle_by_id(
        &self,
        job_id: &str,
        state: &str,
        remote_state: Option<&str>,
        outcome: &Value,
        now: i64,
    ) -> Result<bool, HarnessError> {
        let job = self
            .store
            .external_job(job_id)
            .await
            .map_err(store_error)?
            .ok_or_else(|| HarnessError::new(ErrorCode::TaskNotFound, "no such external job"))?;
        let outcome_json = serde_json::to_string(outcome)
            .map_err(|_| HarnessError::new(ErrorCode::InvalidPayload, "the outcome is not JSON"))?;
        let digest = ContentHash::from_canonical_json(outcome).map_err(|_| {
            HarnessError::new(ErrorCode::InvalidPayload, "the outcome is not hashable")
        })?;
        let delivery = ParentDeliveryRecord {
            message_id: external_job_delivery_id(&job.job_id, digest.as_str()),
            sender_task_id: TaskId::parse(&job.parent_task_id).map_err(|_| {
                HarnessError::new(
                    ErrorCode::InvalidPayload,
                    "the external job names a parent task that is not a task id",
                )
            })?,
            recipient_task_id: TaskId::parse(&job.parent_task_id).map_err(|_| {
                HarnessError::new(
                    ErrorCode::InvalidPayload,
                    "the external job names a parent task that is not a task id",
                )
            })?,
            recipient_session_id: SessionId::parse(&job.session_id).map_err(|_| {
                HarnessError::new(
                    ErrorCode::InvalidPayload,
                    "the external job names a session that is not a session id",
                )
            })?,
            result_id: None,
            payload_hash: digest.clone(),
            payload: outcome.clone(),
            state: "pending".to_owned(),
            consumed_by: None,
        };
        let settlement = ExternalJobSettlement {
            state: state.to_owned(),
            outcome_json,
            outcome_digest: digest.as_str().to_owned(),
            remote_state: remote_state.map(ToOwned::to_owned),
            settled_at_unix_ms: now,
        };
        self.store
            .settle_external_job(&job.job_id, &settlement, Some(&delivery))
            .await
            .map_err(store_error)
    }

    /// Ask the remote to cancel, and settle nothing on the strength of the ask.
    ///
    /// # Errors
    /// Fails when the store refuses or the job has no handle yet.
    pub async fn cancel(&self, job_id: &str) -> Result<CancelReceipt, HarnessError> {
        let job = self
            .store
            .external_job(job_id)
            .await
            .map_err(store_error)?
            .ok_or_else(|| HarnessError::new(ErrorCode::TaskNotFound, "no such external job"))?;
        if job.is_settled() {
            return Ok(CancelReceipt {
                job_id: job.job_id.clone(),
                acknowledged: false,
                state: job.state,
            });
        }
        let remote_task_id = job.remote_task_id.clone().ok_or_else(|| {
            HarnessError::new(
                ErrorCode::InvalidStateTransition,
                "an ambiguous submission has no remote task to cancel; reconcile it first",
            )
        })?;
        let now = self.clock.now_unix_ms();
        self.store
            .request_external_cancel(job_id, now)
            .await
            .map_err(store_error)?;
        let remote = self.remotes.resolve(&job.server_id);
        let acknowledged = match remote {
            Some(remote) => remote.cancel(&remote_task_id).await.is_ok(),
            None => false,
        };
        let state = self
            .store
            .external_job(job_id)
            .await
            .map_err(store_error)?
            .map_or_else(|| "cancel_requested".to_owned(), |job| job.state);
        Ok(CancelReceipt {
            job_id: job_id.to_owned(),
            acknowledged,
            state,
        })
    }

    /// Resolve an ambiguous submission with a handle a human supplied.
    ///
    /// # Errors
    /// Fails when the store refuses, which includes the case where the job was
    /// not ambiguous: reconciliation is not a way to edit a job at will.
    pub async fn reconcile(
        &self,
        job_id: &str,
        remote_task_id: &str,
    ) -> Result<bool, HarnessError> {
        let now = self.clock.now_unix_ms();
        let interval = next_poll_delay_ms(
            0,
            None,
            i64::try_from(DEFAULT_TASK_DEADLINE_MS).unwrap_or(i64::MAX),
        );
        self.store
            .reconcile_external_job(
                job_id,
                remote_task_id,
                Some(interval),
                now.saturating_add(interval),
                now,
            )
            .await
            .map_err(store_error)
    }

    /// Give up on an ambiguous submission, saying so durably.
    ///
    /// # Errors
    /// Fails when the store refuses, or when the job was not ambiguous.
    pub async fn abandon(&self, job_id: &str, reason: &str) -> Result<bool, HarnessError> {
        let job = self
            .store
            .external_job(job_id)
            .await
            .map_err(store_error)?
            .ok_or_else(|| HarnessError::new(ErrorCode::TaskNotFound, "no such external job"))?;
        if job.is_settled() {
            return Ok(false);
        }
        let now = self.clock.now_unix_ms();
        let outcome = json!({
            "schema_version": 1,
            "job_id": job.job_id,
            "server_id": job.server_id,
            "operation": job.operation,
            "abandoned": true,
            "note": "an operator abandoned this submission; whether the remote applied it is unknown",
            "reason": reason,
            "request_digest": job.request_digest,
        });
        self.settle_by_id(job_id, "abandoned", None, &outcome, now)
            .await
    }

    /// Resume after a host start: unresolved handles poll again, unanswered
    /// submissions wait for a human.
    ///
    /// # Errors
    /// Fails when the store refuses.
    pub async fn recover(&self) -> Result<RecoveredExternalJobs, HarnessError> {
        let now = self.clock.now_unix_ms();
        self.store
            .recover_external_jobs(now)
            .await
            .map_err(store_error)
    }

    #[must_use]
    pub fn store(&self) -> &Arc<SqliteStore> {
        &self.store
    }
}

/// The durable record of a submission, written before anything is sent.
///
/// The request digest is what makes a second submission of the same request
/// detectable, so it is computed here, from canonical JSON, rather than trusted
/// from a caller: two spellings of one request must be one request.
fn submission_record(
    job: &NewExternalJob,
    now_unix_ms: i64,
) -> Result<StoredExternalJob, HarnessError> {
    let digest = digest_of(&job.request)?;
    let deadline_ms = if job.deadline_ms == 0 {
        DEFAULT_TASK_DEADLINE_MS
    } else {
        job.deadline_ms
    };
    Ok(StoredExternalJob {
        job_id: job.job_id.clone(),
        parent_task_id: job.parent_task_id.clone(),
        session_id: job.session_id.clone(),
        server_id: job.server_id.clone(),
        operation: job.operation.clone(),
        request_json: serde_json::to_string(&job.request)
            .map_err(|_| HarnessError::new(ErrorCode::InvalidPayload, "the request is not JSON"))?,
        request_digest: digest.as_str().to_owned(),
        remote_task_id: None,
        state: "submitting".to_owned(),
        attempt: 0,
        poll_interval_ms: None,
        // Nothing is due until a handle exists: an unanswered submission is
        // waiting for a human, not for a timer.
        next_poll_unix_ms: i64::MAX,
        deadline_unix_ms: now_unix_ms
            .saturating_add(i64::try_from(deadline_ms).unwrap_or(i64::MAX)),
        submitted_at_unix_ms: now_unix_ms,
        updated_at_unix_ms: now_unix_ms,
        settled_at_unix_ms: None,
        outcome_json: None,
        outcome_digest: None,
        remote_state: None,
        ambiguity: None,
        delivery_message_id: None,
    })
}

/// The digest of a request, over canonical JSON so two spellings of the same
/// request are the same request.
fn digest_of(value: &Value) -> Result<ContentHash, HarnessError> {
    ContentHash::from_canonical_json(value).map_err(|_| {
        HarnessError::new(
            ErrorCode::InvalidPayload,
            "the external request is not hashable",
        )
    })
}

fn store_error(error: harness_store_sqlite::StoreError) -> HarnessError {
    error.into_harness_error()
}
