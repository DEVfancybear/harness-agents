//! Durable human input (M3-02).
//!
//! A question is persisted before the run pauses, so the pause survives a
//! process restart: the run stops with a typed `needs_input` reason, no compute
//! permit and no database transaction stay open, and the answer arrives later
//! through the same store. Answers are scope-checked and one-shot: the first
//! answer wins, repeating it returns the stored answer, a different payload is
//! a conflict, and an expired or empty answer is never consent.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use harness_store_sqlite::{
    QuestionOutcome, QuestionRecord, QuestionState, RunRecord, SqliteStore,
};
use harness_types::{AgentRunId, ContentHash, ErrorCode, QuestionId, SessionId, TaskId};
use serde_json::Value;

use crate::RuntimeError;

/// One question the host wants the user to answer.
#[derive(Clone, Debug)]
pub struct AskRequest {
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub run_id: Option<AgentRunId>,
    /// Stable dedupe scope; the same scope never opens a second question.
    pub scope_key: String,
    pub kind: String,
    pub prompt: String,
    pub payload: Value,
    pub expires_at_unix_ms: Option<u64>,
}

impl AskRequest {
    #[must_use]
    pub fn for_request(
        session_id: SessionId,
        task_id: TaskId,
        run_id: &AgentRunId,
        request_id: &str,
        prompt: impl Into<String>,
    ) -> Self {
        Self {
            session_id,
            task_id,
            run_id: Some(run_id.clone()),
            scope_key: question_scope(run_id, request_id),
            kind: "clarification".to_owned(),
            prompt: prompt.into(),
            payload: Value::Null,
            expires_at_unix_ms: None,
        }
    }
}

/// The dedupe scope of one question: one request in one run.
#[must_use]
pub fn question_scope(run_id: &AgentRunId, request_id: &str) -> String {
    format!("{}|{}", run_id.as_str(), request_id)
}

/// The durable question port.
#[derive(Clone)]
pub struct HumanInputService {
    store: Arc<SqliteStore>,
}

impl HumanInputService {
    #[must_use]
    pub fn new(store: Arc<SqliteStore>) -> Self {
        Self { store }
    }

    /// Persist the question. Opening the same scope twice returns the first row.
    pub async fn ask(
        &self,
        request: AskRequest,
        now_unix_ms: u64,
    ) -> Result<QuestionRecord, RuntimeError> {
        if request.scope_key.trim().is_empty() {
            return Err(RuntimeError::new(
                ErrorCode::InvalidPayload,
                "a question needs a scope key",
            ));
        }
        if request.prompt.trim().is_empty() {
            return Err(RuntimeError::new(
                ErrorCode::InvalidPayload,
                "a question needs a prompt",
            ));
        }
        self.store
            .open_question(QuestionRecord {
                question_id: QuestionId::generate(),
                scope_key: request.scope_key,
                session_id: request.session_id,
                task_id: request.task_id,
                run_id: request.run_id,
                kind: request.kind,
                prompt: request.prompt,
                payload: request.payload,
                state: QuestionState::Open,
                answer: None,
                answer_hash: None,
                answered_by: None,
                expires_at_unix_ms: request.expires_at_unix_ms,
                created_at_unix_ms: now_unix_ms,
                answered_at_unix_ms: None,
            })
            .await
            .map_err(RuntimeError::from)
    }

    /// Answer one question, deduped by scope and payload.
    pub async fn answer(
        &self,
        scope_key: &str,
        answer: &Value,
        actor: &str,
        now_unix_ms: u64,
    ) -> Result<QuestionOutcome, RuntimeError> {
        if is_empty_answer(answer) {
            return Err(RuntimeError::new(
                ErrorCode::InvalidPayload,
                "an empty answer is not consent; say what the run should do",
            ));
        }
        if actor.trim().is_empty() {
            return Err(RuntimeError::new(
                ErrorCode::InvalidPayload,
                "an answer needs an actor",
            ));
        }
        let answer_hash = ContentHash::from_canonical_json(answer)
            .map_err(|error| RuntimeError::new(error.code(), error.to_string()))?;
        self.store
            .answer_question(scope_key, answer, &answer_hash, actor, now_unix_ms)
            .await
            .map_err(RuntimeError::from)
    }

    pub async fn question(
        &self,
        question_id: &QuestionId,
    ) -> Result<Option<QuestionRecord>, RuntimeError> {
        self.store
            .question(question_id)
            .await
            .map_err(RuntimeError::from)
    }

    pub async fn questions(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<QuestionRecord>, RuntimeError> {
        self.store
            .list_questions(session_id)
            .await
            .map_err(RuntimeError::from)
    }

    /// Cancel an open question; a canceled question refuses every answer.
    pub async fn cancel(&self, question_id: &QuestionId) -> Result<QuestionRecord, RuntimeError> {
        self.store
            .cancel_question(question_id)
            .await
            .map_err(RuntimeError::from)
    }

    /// Open questions of one run, oldest first.
    pub async fn open_for_run(
        &self,
        run_id: &AgentRunId,
    ) -> Result<Vec<QuestionRecord>, RuntimeError> {
        let session_id = self
            .store
            .run_by_id(run_id)
            .await
            .map_err(RuntimeError::from)?
            .map(|run: RunRecord| run.session_id)
            .ok_or_else(|| RuntimeError::new(ErrorCode::InvalidPayload, "run does not exist"))?;
        Ok(self
            .questions(&session_id)
            .await?
            .into_iter()
            .filter(|question| {
                question.state == QuestionState::Open && question.run_id.as_ref() == Some(run_id)
            })
            .collect())
    }
}

/// A blank answer is a missing answer, not permission to continue.
#[must_use]
pub fn is_empty_answer(answer: &Value) -> bool {
    match answer {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        Value::Bool(_) | Value::Number(_) => false,
    }
}

/// The host clock, in Unix milliseconds.
#[must_use]
pub fn now_unix_ms() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        Err(_) => 0,
    }
}
