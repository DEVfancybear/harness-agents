//! Durable steering and cancel commands (M3-02).
//!
//! A command is persisted, claimed at a safe run boundary, and only then
//! applied. A cancel claimed at a boundary stops the run before the next
//! dispatch, so work that was queued but never claimed cannot execute. The
//! driver reads the inbox between steps; nothing else consumes it.

use std::sync::Arc;

use harness_store_sqlite::{
    RunCommandKind, RunCommandRecord, RunCommandState, RunRecord, SqliteStore,
};
use harness_types::{ErrorCode, RuntimeCommandId};
use serde_json::json;

use crate::RuntimeError;

/// The durable command inbox of one run.
#[derive(Clone)]
pub struct RunInbox {
    store: Arc<SqliteStore>,
}

impl RunInbox {
    #[must_use]
    pub fn new(store: Arc<SqliteStore>) -> Self {
        Self { store }
    }

    /// Queue a correction for the next safe boundary.
    pub async fn steer(
        &self,
        run: &RunRecord,
        text: impl Into<String>,
        now_unix_ms: u64,
    ) -> Result<RunCommandRecord, RuntimeError> {
        let text = text.into();
        if text.trim().is_empty() {
            return Err(RuntimeError::new(
                ErrorCode::InvalidPayload,
                "a steering command needs text",
            ));
        }
        self.enqueue(
            run,
            RunCommandKind::Steer,
            json!({"text": text}),
            now_unix_ms,
        )
        .await
    }

    /// Queue a message the next step reads as it is written - another agent's
    /// message, which already says who it is from - rather than as the user's
    /// steering correction.
    pub async fn deliver(
        &self,
        run: &RunRecord,
        text: impl Into<String>,
        now_unix_ms: u64,
    ) -> Result<RunCommandRecord, RuntimeError> {
        let text = text.into();
        if text.trim().is_empty() {
            return Err(RuntimeError::new(
                ErrorCode::InvalidPayload,
                "a message needs text",
            ));
        }
        self.enqueue(
            run,
            RunCommandKind::Steer,
            json!({"text": text, "verbatim": true}),
            now_unix_ms,
        )
        .await
    }

    /// Whether a steering command is delivered as written ([`Self::deliver`]).
    #[must_use]
    pub fn is_verbatim(command: &RunCommandRecord) -> bool {
        command.payload["verbatim"].as_bool() == Some(true)
    }

    /// Queue a cancel. The run stops at the next boundary without dispatching.
    pub async fn cancel(
        &self,
        run: &RunRecord,
        reason: impl Into<String>,
        now_unix_ms: u64,
    ) -> Result<RunCommandRecord, RuntimeError> {
        self.enqueue(
            run,
            RunCommandKind::Cancel,
            json!({"reason": reason.into()}),
            now_unix_ms,
        )
        .await
    }

    async fn enqueue(
        &self,
        run: &RunRecord,
        kind: RunCommandKind,
        payload: serde_json::Value,
        now_unix_ms: u64,
    ) -> Result<RunCommandRecord, RuntimeError> {
        let record = RunCommandRecord {
            command_id: RuntimeCommandId::generate(),
            run_id: run.run_id.clone(),
            session_id: run.session_id.clone(),
            task_id: run.task_id.clone(),
            kind,
            state: RunCommandState::Pending,
            payload: payload.clone(),
            detail: None,
            created_at_unix_ms: now_unix_ms,
            claimed_at_unix_ms: None,
            applied_at_unix_ms: None,
        };
        self.store
            .enqueue_run_command(record.clone())
            .await
            .map_err(RuntimeError::from)?;
        Ok(record)
    }

    /// Keep a message the host itself put in front of the model during a run -
    /// a goal continuation, stop-hook feedback, a repeated-read warning - with
    /// the turn, as prime-agent keeps its custom messages in the session: the
    /// next turns are sent it where it was said, not a turn with a gap in it.
    pub async fn record_host_message(
        &self,
        run: &RunRecord,
        text: impl Into<String>,
        now_unix_ms: u64,
    ) -> Result<(), RuntimeError> {
        let record = RunCommandRecord {
            command_id: RuntimeCommandId::generate(),
            run_id: run.run_id.clone(),
            session_id: run.session_id.clone(),
            task_id: run.task_id.clone(),
            kind: RunCommandKind::Steer,
            state: RunCommandState::Applied,
            payload: json!({"text": text.into(), "verbatim": true, "host": true}),
            detail: None,
            created_at_unix_ms: now_unix_ms,
            claimed_at_unix_ms: Some(now_unix_ms),
            applied_at_unix_ms: Some(now_unix_ms),
        };
        self.store
            .record_applied_run_command(record, "host message delivered to the next step")
            .await
            .map_err(RuntimeError::from)
    }

    /// Claim the pending commands for this boundary, oldest first.
    pub async fn claim(
        &self,
        run: &RunRecord,
        limit: u32,
        now_unix_ms: u64,
    ) -> Result<Vec<RunCommandRecord>, RuntimeError> {
        self.store
            .claim_run_commands(&run.run_id, limit, now_unix_ms)
            .await
            .map_err(RuntimeError::from)
    }

    pub async fn apply(
        &self,
        command: &RunCommandRecord,
        detail: impl Into<String>,
        now_unix_ms: u64,
    ) -> Result<(), RuntimeError> {
        self.store
            .complete_run_command(
                &command.command_id,
                RunCommandState::Applied,
                Some(&detail.into()),
                now_unix_ms,
            )
            .await
            .map_err(RuntimeError::from)
    }

    pub async fn reject(
        &self,
        command: &RunCommandRecord,
        detail: impl Into<String>,
        now_unix_ms: u64,
    ) -> Result<(), RuntimeError> {
        self.store
            .complete_run_command(
                &command.command_id,
                RunCommandState::Rejected,
                Some(&detail.into()),
                now_unix_ms,
            )
            .await
            .map_err(RuntimeError::from)
    }

    /// Text of a steering command, when the payload carries one.
    #[must_use]
    pub fn steering_text(command: &RunCommandRecord) -> Option<String> {
        command
            .payload
            .get("text")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
    }

    /// The user message a steering command becomes for the model: the step
    /// that receives it and every later replay of its turn read the same text.
    #[must_use]
    pub fn steering_message(command: &RunCommandRecord) -> Option<String> {
        let text = Self::steering_text(command)?;
        Some(if Self::is_verbatim(command) {
            text
        } else {
            format!("[steering correction from the user]\n{text}")
        })
    }

    /// Reason of a cancel command, when the payload carries one.
    #[must_use]
    pub fn cancel_reason(command: &RunCommandRecord) -> Option<String> {
        command
            .payload
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
    }
}
