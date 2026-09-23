//! M3 durable run, step, budget and human-input records.
//!
//! Every write uses the P1 transaction coordinator and host fence, so these
//! tables extend the runtime journal instead of becoming a second authority.
//! The freeze path commits one run step and its budget reservation in a single
//! transaction: a step that was reserved but not frozen, or frozen but not
//! reserved, cannot be observed after a crash.

use harness_types::{
    AgentRunId, BudgetId, BudgetReservationId, ContentHash, ContextPacketId, ErrorCode, InputId,
    QuestionId, RequestId, RuntimeCommandId, SessionId, StepId, TaskId,
};
use serde_json::Value;
use sqlx::Row;

use super::{
    SqliteStore, assert_fence_in_tx, database_error, parse_session, parse_task, row_get, to_i64,
    to_json, to_u64,
};
use crate::{
    BudgetAccountRecord, BudgetReservationRecord, BudgetReservationState, BudgetSettlement,
    QuestionOutcome, QuestionRecord, QuestionState, RunCommandKind, RunCommandRecord,
    RunCommandState, RunRecord, RunState, RunStepRecord, StoreError, StoreFaultPoint,
};

fn parse_run(value: String) -> Result<AgentRunId, StoreError> {
    AgentRunId::parse(value)
        .map_err(|_| StoreError::new(ErrorCode::StorageWriteFailed, "run ID is invalid"))
}

fn parse_run_step(value: String) -> Result<StepId, StoreError> {
    StepId::parse(value)
        .map_err(|_| StoreError::new(ErrorCode::StorageWriteFailed, "run step ID is invalid"))
}

fn parse_input(value: String) -> Result<InputId, StoreError> {
    InputId::parse(value)
        .map_err(|_| StoreError::new(ErrorCode::StorageWriteFailed, "input ID is invalid"))
}

fn parse_request(value: String) -> Result<RequestId, StoreError> {
    RequestId::parse(value)
        .map_err(|_| StoreError::new(ErrorCode::StorageWriteFailed, "request ID is invalid"))
}

fn parse_packet(value: String) -> Result<ContextPacketId, StoreError> {
    ContextPacketId::parse(value).map_err(|_| {
        StoreError::new(
            ErrorCode::StorageWriteFailed,
            "context packet ID is invalid",
        )
    })
}

fn parse_question(value: String) -> Result<QuestionId, StoreError> {
    QuestionId::parse(value)
        .map_err(|_| StoreError::new(ErrorCode::StorageWriteFailed, "question ID is invalid"))
}

fn parse_runtime_command(value: String) -> Result<RuntimeCommandId, StoreError> {
    RuntimeCommandId::parse(value)
        .map_err(|_| StoreError::new(ErrorCode::StorageWriteFailed, "run command ID is invalid"))
}

fn parse_budget_id(value: String) -> Result<BudgetId, StoreError> {
    BudgetId::parse(value)
        .map_err(|_| StoreError::new(ErrorCode::StorageWriteFailed, "budget ID is invalid"))
}

fn parse_reservation(value: String) -> Result<BudgetReservationId, StoreError> {
    BudgetReservationId::parse(value).map_err(|_| {
        StoreError::new(
            ErrorCode::StorageWriteFailed,
            "budget reservation ID is invalid",
        )
    })
}

fn run_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<RunRecord, StoreError> {
    let state_text: String = row_get(row, "state")?;
    let state = RunState::parse(&state_text)
        .ok_or_else(|| StoreError::new(ErrorCode::StorageWriteFailed, "run state is invalid"))?;
    Ok(RunRecord {
        run_id: parse_run(row_get(row, "run_id")?)?,
        session_id: parse_session(row_get(row, "session_id")?)?,
        task_id: parse_task(row_get(row, "task_id")?)?,
        input_id: parse_input(row_get(row, "input_id")?)?,
        state,
        acceptance: row_get(row, "acceptance")?,
        stop_reason: row_get(row, "stop_reason")?,
        owner_generation: to_u64(row_get(row, "owner_generation")?, "run generation")?,
        revision: to_u64(row_get(row, "revision")?, "run revision")?,
        budget_id: row_get::<Option<String>>(row, "budget_id")?
            .map(parse_budget_id)
            .transpose()?,
        awaiting_question_id: row_get::<Option<String>>(row, "awaiting_question_id")?
            .map(parse_question)
            .transpose()?,
    })
}

fn step_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<RunStepRecord, StoreError> {
    Ok(RunStepRecord {
        step_id: parse_run_step(row_get(row, "step_id")?)?,
        run_id: parse_run(row_get(row, "run_id")?)?,
        step_index: u32::try_from(to_u64(row_get(row, "step_index")?, "step index")?).map_err(
            |_| StoreError::new(ErrorCode::StorageWriteFailed, "step index exceeds range"),
        )?,
        request_id: parse_request(row_get(row, "request_id")?)?,
        packet_id: parse_packet(row_get(row, "packet_id")?)?,
        manifest_hash: ContentHash::parse(row_get::<String>(row, "manifest_hash")?).map_err(
            |_| StoreError::new(ErrorCode::StorageWriteFailed, "manifest hash is invalid"),
        )?,
        source_sequence: to_u64(row_get(row, "source_sequence")?, "source sequence")?,
        state: row_get(row, "state")?,
        stop_reason: row_get(row, "stop_reason")?,
    })
}
fn reservation_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<BudgetReservationRecord, StoreError> {
    let state_text: String = row_get(row, "state")?;
    let state = BudgetReservationState::parse(&state_text).ok_or_else(|| {
        StoreError::new(
            ErrorCode::StorageWriteFailed,
            "budget reservation state is invalid",
        )
    })?;
    Ok(BudgetReservationRecord {
        reservation_id: parse_reservation(row_get(row, "reservation_id")?)?,
        budget_id: parse_budget_id(row_get(row, "budget_id")?)?,
        operation_id: row_get(row, "operation_id")?,
        origin: row_get(row, "origin")?,
        upper_bound_tokens: to_u64(row_get(row, "upper_bound_tokens")?, "reservation bound")?,
        settled_tokens: row_get::<Option<i64>>(row, "settled_tokens")?
            .map(|value| to_u64(value, "settled tokens"))
            .transpose()?,
        state,
        revision: to_u64(row_get(row, "revision")?, "reservation revision")?,
    })
}

fn question_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<QuestionRecord, StoreError> {
    let state_text: String = row_get(row, "state")?;
    let state = QuestionState::parse(&state_text).ok_or_else(|| {
        StoreError::new(ErrorCode::StorageWriteFailed, "question state is invalid")
    })?;
    let payload: String = row_get(row, "payload_json")?;
    let answer: Option<String> = row_get(row, "answer_json")?;
    Ok(QuestionRecord {
        question_id: parse_question(row_get(row, "question_id")?)?,
        scope_key: row_get(row, "scope_key")?,
        session_id: parse_session(row_get(row, "session_id")?)?,
        task_id: parse_task(row_get(row, "task_id")?)?,
        run_id: row_get::<Option<String>>(row, "run_id")?
            .map(parse_run)
            .transpose()?,
        kind: row_get(row, "kind")?,
        prompt: row_get(row, "prompt")?,
        payload: serde_json::from_str(&payload).map_err(|_| {
            StoreError::new(ErrorCode::StorageWriteFailed, "question payload is invalid")
        })?,
        state,
        answer: answer
            .map(|text| {
                serde_json::from_str(&text).map_err(|_| {
                    StoreError::new(ErrorCode::StorageWriteFailed, "question answer is invalid")
                })
            })
            .transpose()?,
        answer_hash: row_get::<Option<String>>(row, "answer_hash")?
            .map(|text| {
                ContentHash::parse(text).map_err(|_| {
                    StoreError::new(
                        ErrorCode::StorageWriteFailed,
                        "question answer hash is invalid",
                    )
                })
            })
            .transpose()?,
        answered_by: row_get(row, "answered_by")?,
        expires_at_unix_ms: row_get::<Option<i64>>(row, "expires_at_unix_ms")?
            .map(|value| to_u64(value, "question expiry"))
            .transpose()?,
        created_at_unix_ms: to_u64(
            row_get(row, "created_at_unix_ms")?,
            "question creation time",
        )?,
        answered_at_unix_ms: row_get::<Option<i64>>(row, "answered_at_unix_ms")?
            .map(|value| to_u64(value, "question answer time"))
            .transpose()?,
    })
}

fn command_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<RunCommandRecord, StoreError> {
    let kind_text: String = row_get(row, "kind")?;
    let state_text: String = row_get(row, "state")?;
    let kind = RunCommandKind::parse(&kind_text).ok_or_else(|| {
        StoreError::new(ErrorCode::StorageWriteFailed, "run command kind is invalid")
    })?;
    let state = RunCommandState::parse(&state_text).ok_or_else(|| {
        StoreError::new(
            ErrorCode::StorageWriteFailed,
            "run command state is invalid",
        )
    })?;
    let payload: String = row_get(row, "payload_json")?;
    Ok(RunCommandRecord {
        command_id: parse_runtime_command(row_get(row, "command_id")?)?,
        run_id: parse_run(row_get(row, "run_id")?)?,
        session_id: parse_session(row_get(row, "session_id")?)?,
        task_id: parse_task(row_get(row, "task_id")?)?,
        kind,
        state,
        payload: serde_json::from_str(&payload).map_err(|_| {
            StoreError::new(ErrorCode::StorageWriteFailed, "command payload is invalid")
        })?,
        detail: row_get(row, "detail")?,
        created_at_unix_ms: to_u64(row_get(row, "created_at_unix_ms")?, "command time")?,
        claimed_at_unix_ms: row_get::<Option<i64>>(row, "claimed_at_unix_ms")?
            .map(|value| to_u64(value, "command claim time"))
            .transpose()?,
        applied_at_unix_ms: row_get::<Option<i64>>(row, "applied_at_unix_ms")?
            .map(|value| to_u64(value, "command applied time"))
            .transpose()?,
    })
}

/// Charge one account and every ancestor by `delta` (positive) with checked
/// arithmetic. Returns `BudgetExhausted` before any row changes when a limit
/// would be crossed.
async fn charge_chain(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    budget_id: &BudgetId,
    delta: i64,
) -> Result<(), StoreError> {
    let mut current = Some(budget_id.clone());
    let mut visited = std::collections::BTreeSet::new();
    while let Some(account_id) = current {
        if !visited.insert(account_id.as_str().to_owned()) {
            return Err(StoreError::new(
                ErrorCode::StorageWriteFailed,
                "budget account hierarchy contains a cycle",
            ));
        }
        let row = sqlx::query("SELECT parent_budget_id, limit_tokens, spent_tokens, revision FROM budget_accounts WHERE budget_id = ?")
            .bind(account_id.as_str())
            .fetch_optional(&mut **tx)
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read budget account", error))?
            .ok_or_else(|| StoreError::new(ErrorCode::BudgetExhausted, "budget account does not exist"))?;
        let limit = to_u64(row.get::<i64, _>("limit_tokens"), "budget limit")?;
        let spent = to_u64(row.get::<i64, _>("spent_tokens"), "budget spent")?;
        let revision = to_u64(row.get::<i64, _>("revision"), "budget revision")?;
        let next = i128::from(spent) + i128::from(delta);
        if next < 0 {
            return Err(StoreError::new(
                ErrorCode::BudgetExhausted,
                "budget settlement would release more than was reserved",
            ));
        }
        if next > i128::from(limit) && delta > 0 {
            return Err(StoreError::new(
                ErrorCode::BudgetExhausted,
                format!("budget {account_id} would exceed its limit: {spent} + {delta} > {limit}"),
            ));
        }
        let next = u64::try_from(next)
            .map_err(|_| StoreError::new(ErrorCode::BudgetExhausted, "budget counter overflow"))?;
        let updated = sqlx::query("UPDATE budget_accounts SET spent_tokens = ?, revision = revision + 1 WHERE budget_id = ? AND revision = ?")
            .bind(to_i64(next, "budget spent")?)
            .bind(account_id.as_str())
            .bind(to_i64(revision, "budget revision")?)
            .execute(&mut **tx)
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "update budget account", error))?;
        if updated.rows_affected() != 1 {
            return Err(StoreError::new(
                ErrorCode::SequenceConflict,
                "budget account revision moved during settlement",
            ));
        }
        current = row
            .get::<Option<String>, _>("parent_budget_id")
            .map(parse_budget_id)
            .transpose()?;
    }
    Ok(())
}

impl SqliteStore {
    /// Open the durable run for an admitted input, or return the existing one.
    ///
    /// One admitted input owns exactly one run; calling this twice with the same
    /// input is the idempotent path a retried turn takes.
    pub async fn start_run(
        &self,
        session_id: &SessionId,
        task_id: &TaskId,
        input_id: &InputId,
        budget_id: Option<&BudgetId>,
    ) -> Result<RunRecord, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        let run_id = AgentRunId::generate();
        sqlx::query("INSERT INTO runs(run_id, session_id, task_id, input_id, state, acceptance, stop_reason, owner_generation, revision, budget_id, awaiting_question_id) VALUES (?, ?, ?, ?, 'running', NULL, NULL, ?, 1, ?, NULL) ON CONFLICT(input_id) DO NOTHING")
            .bind(run_id.as_str())
            .bind(session_id.as_str())
            .bind(task_id.as_str())
            .bind(input_id.as_str())
            .bind(to_i64(fence.generation, "run generation")?)
            .bind(budget_id.map(BudgetId::as_str))
            .execute(&mut *tx)
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "start run", error))?;
        let row = sqlx::query(
            "SELECT run_id, session_id, task_id, input_id, state, acceptance, stop_reason, owner_generation, revision, budget_id, awaiting_question_id FROM runs WHERE input_id = ?"
        )
        .bind(input_id.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read run", error))?;
        let mut record = run_from_row(&row)?;
        if &record.session_id != session_id || &record.task_id != task_id {
            return Err(StoreError::new(
                ErrorCode::IdempotencyConflict,
                "input already belongs to a different run scope",
            ));
        }
        if matches!(record.state, RunState::Running | RunState::Paused)
            && record.owner_generation != fence.generation
        {
            let updated = sqlx::query(
                "UPDATE runs SET owner_generation = ? WHERE run_id = ? AND owner_generation = ?",
            )
            .bind(to_i64(fence.generation, "run generation")?)
            .bind(record.run_id.as_str())
            .bind(to_i64(record.owner_generation, "previous run generation")?)
            .execute(&mut *tx)
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "reclaim run", error))?;
            if updated.rows_affected() != 1 {
                return Err(StoreError::new(
                    ErrorCode::StaleWriter,
                    "run ownership changed while reclaiming it",
                ));
            }
            record.owner_generation = fence.generation;
        }
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit run start", error)
        })?;
        Ok(record)
    }

    pub async fn run_by_input(&self, input_id: &InputId) -> Result<Option<RunRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT run_id, session_id, task_id, input_id, state, acceptance, stop_reason, owner_generation, revision, budget_id, awaiting_question_id FROM runs WHERE input_id = ?"
        )
        .bind(input_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read run by input", error))?;
        row.map(|row| run_from_row(&row)).transpose()
    }

    pub async fn run_by_id(&self, run_id: &AgentRunId) -> Result<Option<RunRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT run_id, session_id, task_id, input_id, state, acceptance, stop_reason, owner_generation, revision, budget_id, awaiting_question_id FROM runs WHERE run_id = ?"
        )
        .bind(run_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read run", error))?;
        row.map(|row| run_from_row(&row)).transpose()
    }

    pub async fn latest_run(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<RunRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT run_id, session_id, task_id, input_id, state, acceptance, stop_reason, owner_generation, revision, budget_id, awaiting_question_id FROM runs WHERE session_id = ? ORDER BY rowid DESC LIMIT 1"
        )
        .bind(session_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read latest run", error))?;
        row.map(|row| run_from_row(&row)).transpose()
    }

    pub async fn run_steps(&self, run_id: &AgentRunId) -> Result<Vec<RunStepRecord>, StoreError> {
        let rows = sqlx::query("SELECT step_id, run_id, step_index, request_id, packet_id, manifest_hash, source_sequence, state, stop_reason FROM run_steps WHERE run_id = ? ORDER BY step_index")
            .bind(run_id.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "list run steps", error))?;
        rows.iter().map(step_from_row).collect()
    }

    /// Freeze one run step and its budget reservation in one transaction.
    ///
    /// The run revision CAS makes the boundary explicit: a step is frozen only
    /// against the revision the caller read, and the injected fault point proves
    /// a store failure here stops the caller before any provider dispatch.
    #[allow(clippy::too_many_lines)] // one transactional boundary: reserve, freeze, advance revision
    pub async fn freeze_run_step(
        &self,
        run_id: &AgentRunId,
        expected_revision: u64,
        step: RunStepRecord,
        reservation: Option<BudgetReservationRecord>,
    ) -> Result<RunRecord, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        let row = sqlx::query(
            "SELECT run_id, session_id, task_id, input_id, state, acceptance, stop_reason, owner_generation, revision, budget_id, awaiting_question_id FROM runs WHERE run_id = ?"
        )
        .bind(run_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read run for freeze", error))?
        .ok_or_else(|| StoreError::new(ErrorCode::InvalidPayload, "run does not exist"))?;
        let mut current = run_from_row(&row)?;
        if current.owner_generation != fence.generation {
            return Err(StoreError::new(
                ErrorCode::StaleWriter,
                "run is owned by a different host generation",
            ));
        }
        if current.state != RunState::Running {
            return Err(StoreError::new(
                ErrorCode::InvalidStateTransition,
                format!("run is {} and cannot freeze a step", current.state.as_str()),
            ));
        }
        if current.revision != expected_revision {
            return Err(StoreError::new(
                ErrorCode::SequenceConflict,
                format!(
                    "run revision moved: expected {expected_revision}, found {}",
                    current.revision
                ),
            ));
        }
        if &step.run_id != run_id {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                "step belongs to a different run",
            ));
        }
        if let Some(reservation) = &reservation {
            let inserted = sqlx::query("INSERT INTO budget_reservations(reservation_id, budget_id, operation_id, origin, upper_bound_tokens, settled_tokens, state, revision, created_at_unix_ms, settled_at_unix_ms) VALUES (?, ?, ?, ?, ?, NULL, 'reserved', 1, ?, NULL) ON CONFLICT(operation_id) DO NOTHING")
                .bind(reservation.reservation_id.as_str())
                .bind(reservation.budget_id.as_str())
                .bind(&reservation.operation_id)
                .bind(&reservation.origin)
                .bind(to_i64(reservation.upper_bound_tokens, "reservation bound")?)
                .bind(0_i64)
                .execute(&mut *tx)
                .await
                .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "reserve budget", error))?;
            if inserted.rows_affected() == 0 {
                let existing = sqlx::query("SELECT budget_id, upper_bound_tokens FROM budget_reservations WHERE operation_id = ?")
                    .bind(&reservation.operation_id)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read reservation", error))?;
                let existing_budget: String = existing.get("budget_id");
                let existing_bound = to_u64(
                    existing.get::<i64, _>("upper_bound_tokens"),
                    "reservation bound",
                )?;
                if existing_budget != reservation.budget_id.as_str()
                    || existing_bound != reservation.upper_bound_tokens
                {
                    return Err(StoreError::new(
                        ErrorCode::IdempotencyConflict,
                        "budget reservation operation is immutable",
                    ));
                }
            } else {
                charge_chain(
                    &mut tx,
                    &reservation.budget_id,
                    i64::try_from(reservation.upper_bound_tokens).map_err(|_| {
                        StoreError::new(ErrorCode::BudgetExhausted, "reservation bound overflow")
                    })?,
                )
                .await?;
            }
        }
        sqlx::query("INSERT INTO run_steps(step_id, run_id, step_index, request_id, packet_id, manifest_hash, source_sequence, state, stop_reason) VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL)")
            .bind(step.step_id.as_str())
            .bind(step.run_id.as_str())
            .bind(i64::from(step.step_index))
            .bind(step.request_id.as_str())
            .bind(step.packet_id.as_str())
            .bind(step.manifest_hash.as_str())
            .bind(to_i64(step.source_sequence, "step source sequence")?)
            .bind(&step.state)
            .execute(&mut *tx)
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "freeze run step", error))?;
        let updated = sqlx::query(
            "UPDATE runs SET revision = revision + 1 WHERE run_id = ? AND revision = ?",
        )
        .bind(run_id.as_str())
        .bind(to_i64(expected_revision, "run revision")?)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "advance run revision", error)
        })?;
        if updated.rows_affected() != 1 {
            return Err(StoreError::new(
                ErrorCode::SequenceConflict,
                "run revision moved during freeze",
            ));
        }
        if self
            .fault_plan_ref()
            .consume(StoreFaultPoint::BeforeFreezeStepCommit)
        {
            return Err(StoreError::new(
                ErrorCode::StorageWriteFailed,
                "injected failure before frozen step commit",
            ));
        }
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit frozen step", error)
        })?;
        current.revision = current.revision.checked_add(1).ok_or_else(|| {
            StoreError::new(ErrorCode::StorageWriteFailed, "run revision overflow")
        })?;
        Ok(current)
    }

    /// Record what one frozen step produced and settle its reservation.
    #[allow(clippy::too_many_lines)] // one transactional boundary: settle step and reservation
    pub async fn settle_run_step(
        &self,
        step_id: &StepId,
        state: &str,
        stop_reason: Option<&str>,
        settlement: Option<BudgetSettlement>,
    ) -> Result<(), StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        let updated =
            sqlx::query("UPDATE run_steps SET state = ?, stop_reason = ? WHERE step_id = ?")
                .bind(state)
                .bind(stop_reason)
                .bind(step_id.as_str())
                .execute(&mut *tx)
                .await
                .map_err(|error| {
                    database_error(ErrorCode::StorageWriteFailed, "settle run step", error)
                })?;
        if updated.rows_affected() != 1 {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                "run step does not exist",
            ));
        }
        if let Some(settlement) = settlement {
            settle_reservation_in_tx(&mut tx, &settlement).await?;
        }
        if self
            .fault_plan_ref()
            .consume(StoreFaultPoint::BeforeBudgetSettleCommit)
        {
            return Err(StoreError::new(
                ErrorCode::StorageWriteFailed,
                "injected failure before budget settlement commit",
            ));
        }
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit step settlement",
                error,
            )
        })
    }

    /// Move a run to a terminal or waiting state with a revision CAS.
    #[allow(clippy::too_many_arguments)] // one row, every typed field it carries
    pub async fn finish_run(
        &self,
        run_id: &AgentRunId,
        expected_revision: u64,
        state: RunState,
        acceptance: Option<&str>,
        stop_reason: Option<&str>,
        awaiting_question_id: Option<&QuestionId>,
    ) -> Result<RunRecord, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        let row = sqlx::query(
            "SELECT run_id, session_id, task_id, input_id, state, acceptance, stop_reason, owner_generation, revision, budget_id, awaiting_question_id FROM runs WHERE run_id = ?"
        )
        .bind(run_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read run to finish", error))?
        .ok_or_else(|| StoreError::new(ErrorCode::InvalidPayload, "run does not exist"))?;
        let current = run_from_row(&row)?;
        if current.owner_generation != fence.generation {
            return Err(StoreError::new(
                ErrorCode::StaleWriter,
                "run is owned by a different host generation",
            ));
        }
        if current.revision != expected_revision {
            return Err(StoreError::new(
                ErrorCode::SequenceConflict,
                format!(
                    "run revision moved: expected {expected_revision}, found {}",
                    current.revision
                ),
            ));
        }
        let updated = sqlx::query("UPDATE runs SET state = ?, acceptance = ?, stop_reason = ?, awaiting_question_id = ?, revision = revision + 1 WHERE run_id = ? AND revision = ?")
            .bind(state.as_str())
            .bind(acceptance)
            .bind(stop_reason)
            .bind(awaiting_question_id.map(QuestionId::as_str))
            .bind(run_id.as_str())
            .bind(to_i64(expected_revision, "run revision")?)
            .execute(&mut *tx)
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "finish run", error))?;
        if updated.rows_affected() != 1 {
            return Err(StoreError::new(
                ErrorCode::SequenceConflict,
                "run revision moved while finishing",
            ));
        }
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit run finish", error)
        })?;
        let mut finished = current;
        finished.state = state;
        finished.acceptance = acceptance.map(ToOwned::to_owned);
        finished.stop_reason = stop_reason.map(ToOwned::to_owned);
        finished.awaiting_question_id = awaiting_question_id.cloned();
        finished.revision = finished.revision.checked_add(1).ok_or_else(|| {
            StoreError::new(ErrorCode::StorageWriteFailed, "run revision overflow")
        })?;
        Ok(finished)
    }

    /// Create a budget account, or return the existing one with the same shape.
    pub async fn ensure_budget_account(
        &self,
        record: BudgetAccountRecord,
    ) -> Result<BudgetAccountRecord, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        if let Some(parent_id) = &record.parent_budget_id {
            let mut visited =
                std::collections::BTreeSet::from([record.budget_id.as_str().to_owned()]);
            let mut current = Some(parent_id.clone());
            while let Some(account_id) = current {
                if !visited.insert(account_id.as_str().to_owned()) {
                    return Err(StoreError::new(
                        ErrorCode::InvalidPayload,
                        "budget account parent would create a cycle",
                    ));
                }
                let row =
                    sqlx::query("SELECT parent_budget_id FROM budget_accounts WHERE budget_id = ?")
                        .bind(account_id.as_str())
                        .fetch_optional(&mut *tx)
                        .await
                        .map_err(|error| {
                            database_error(
                                ErrorCode::StorageWriteFailed,
                                "validate budget parent",
                                error,
                            )
                        })?
                        .ok_or_else(|| {
                            StoreError::new(
                                ErrorCode::InvalidPayload,
                                "budget parent account does not exist",
                            )
                        })?;
                current = row
                    .get::<Option<String>, _>("parent_budget_id")
                    .map(parse_budget_id)
                    .transpose()?;
            }
        }
        sqlx::query("INSERT INTO budget_accounts(budget_id, parent_budget_id, limit_tokens, spent_tokens, revision) VALUES (?, ?, ?, 0, 1) ON CONFLICT(budget_id) DO NOTHING")
            .bind(record.budget_id.as_str())
            .bind(record.parent_budget_id.as_ref().map(BudgetId::as_str))
            .bind(to_i64(record.limit_tokens, "budget limit")?)
            .execute(&mut *tx)
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "create budget account", error))?;
        let row = sqlx::query("SELECT budget_id, parent_budget_id, limit_tokens, spent_tokens, revision FROM budget_accounts WHERE budget_id = ?")
            .bind(record.budget_id.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read budget account", error))?;
        let existing = budget_account_from_row(&row)?;
        if existing.limit_tokens != record.limit_tokens
            || existing.parent_budget_id != record.parent_budget_id
        {
            return Err(StoreError::new(
                ErrorCode::IdempotencyConflict,
                "budget account identity is immutable",
            ));
        }
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit budget account",
                error,
            )
        })?;
        Ok(existing)
    }

    pub async fn budget_account(
        &self,
        budget_id: &BudgetId,
    ) -> Result<Option<BudgetAccountRecord>, StoreError> {
        let row = sqlx::query("SELECT budget_id, parent_budget_id, limit_tokens, spent_tokens, revision FROM budget_accounts WHERE budget_id = ?")
            .bind(budget_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read budget account", error))?;
        row.map(|row| budget_account_from_row(&row)).transpose()
    }

    /// Reserve budget for an operation that is not part of a step freeze (for
    /// example a retry after the first attempt failed).
    pub async fn reserve_budget(
        &self,
        reservation: BudgetReservationRecord,
    ) -> Result<BudgetReservationRecord, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        let inserted = sqlx::query("INSERT INTO budget_reservations(reservation_id, budget_id, operation_id, origin, upper_bound_tokens, settled_tokens, state, revision, created_at_unix_ms, settled_at_unix_ms) VALUES (?, ?, ?, ?, ?, NULL, 'reserved', 1, ?, NULL) ON CONFLICT(operation_id) DO NOTHING")
            .bind(reservation.reservation_id.as_str())
            .bind(reservation.budget_id.as_str())
            .bind(&reservation.operation_id)
            .bind(&reservation.origin)
            .bind(to_i64(reservation.upper_bound_tokens, "reservation bound")?)
            .bind(0_i64)
            .execute(&mut *tx)
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "reserve budget", error))?;
        if inserted.rows_affected() == 1 {
            charge_chain(
                &mut tx,
                &reservation.budget_id,
                i64::try_from(reservation.upper_bound_tokens).map_err(|_| {
                    StoreError::new(ErrorCode::BudgetExhausted, "reservation bound overflow")
                })?,
            )
            .await?;
        }
        let row = sqlx::query("SELECT reservation_id, budget_id, operation_id, origin, upper_bound_tokens, settled_tokens, state, revision FROM budget_reservations WHERE operation_id = ?")
            .bind(&reservation.operation_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read reservation", error))?;
        let existing = reservation_from_row(&row)?;
        if inserted.rows_affected() == 0
            && (existing.budget_id != reservation.budget_id
                || existing.upper_bound_tokens != reservation.upper_bound_tokens)
        {
            return Err(StoreError::new(
                ErrorCode::IdempotencyConflict,
                "budget reservation operation is immutable",
            ));
        }
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit budget reserve",
                error,
            )
        })?;
        Ok(existing)
    }

    pub async fn budget_reservation_by_operation(
        &self,
        operation_id: &str,
    ) -> Result<Option<BudgetReservationRecord>, StoreError> {
        let row = sqlx::query("SELECT reservation_id, budget_id, operation_id, origin, upper_bound_tokens, settled_tokens, state, revision FROM budget_reservations WHERE operation_id = ?")
            .bind(operation_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read reservation", error))?;
        row.map(|row| reservation_from_row(&row)).transpose()
    }

    /// Release a reservation that was never dispatched (for example a step
    /// cancelled at the boundary). The charge is removed; the record stays as
    /// an audit row in the released state.
    pub async fn release_budget_reservation(
        &self,
        reservation_id: &BudgetReservationId,
    ) -> Result<BudgetReservationRecord, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        let row = sqlx::query("SELECT reservation_id, budget_id, operation_id, origin, upper_bound_tokens, settled_tokens, state, revision FROM budget_reservations WHERE reservation_id = ?")
            .bind(reservation_id.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read reservation", error))?
            .ok_or_else(|| StoreError::new(ErrorCode::InvalidPayload, "budget reservation does not exist"))?;
        let current = reservation_from_row(&row)?;
        if current.state != BudgetReservationState::Reserved {
            return Ok(current);
        }
        charge_chain(
            &mut tx,
            &current.budget_id,
            -i64::try_from(current.upper_bound_tokens).map_err(|_| {
                StoreError::new(ErrorCode::BudgetExhausted, "reservation bound overflow")
            })?,
        )
        .await?;
        sqlx::query("UPDATE budget_reservations SET state = 'released', revision = revision + 1 WHERE reservation_id = ? AND revision = ?")
            .bind(reservation_id.as_str())
            .bind(to_i64(current.revision, "reservation revision")?)
            .execute(&mut *tx)
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "release reservation", error))?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit reservation release",
                error,
            )
        })?;
        let mut released = current;
        released.state = BudgetReservationState::Released;
        released.revision = released.revision.checked_add(1).ok_or_else(|| {
            StoreError::new(
                ErrorCode::StorageWriteFailed,
                "reservation revision overflow",
            )
        })?;
        Ok(released)
    }

    /// Persist a question. Same scope returns the existing row, so a retried
    /// pause never opens a second question.
    pub async fn open_question(
        &self,
        record: QuestionRecord,
    ) -> Result<QuestionRecord, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        let payload = to_json(&record.payload, "serialize question payload")?;
        sqlx::query("INSERT INTO questions(question_id, scope_key, session_id, task_id, run_id, kind, prompt, payload_json, state, answer_json, answer_hash, answered_by, expires_at_unix_ms, created_at_unix_ms, answered_at_unix_ms) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'open', NULL, NULL, NULL, ?, ?, NULL) ON CONFLICT(scope_key) DO NOTHING")
            .bind(record.question_id.as_str())
            .bind(&record.scope_key)
            .bind(record.session_id.as_str())
            .bind(record.task_id.as_str())
            .bind(record.run_id.as_ref().map(AgentRunId::as_str))
            .bind(&record.kind)
            .bind(&record.prompt)
            .bind(payload)
            .bind(record.expires_at_unix_ms.map(|value| to_i64(value, "question expiry")).transpose()?)
            .bind(to_i64(record.created_at_unix_ms, "question creation time")?)
            .execute(&mut *tx)
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "open question", error))?;
        let row = sqlx::query(
            "SELECT question_id, scope_key, session_id, task_id, run_id, kind, prompt, payload_json, state, answer_json, answer_hash, answered_by, expires_at_unix_ms, created_at_unix_ms, answered_at_unix_ms FROM questions WHERE scope_key = ?"
        )
        .bind(&record.scope_key)
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read question", error))?;
        let existing = question_from_row(&row)?;
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit question", error)
        })?;
        Ok(existing)
    }

    pub async fn question(
        &self,
        question_id: &QuestionId,
    ) -> Result<Option<QuestionRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT question_id, scope_key, session_id, task_id, run_id, kind, prompt, payload_json, state, answer_json, answer_hash, answered_by, expires_at_unix_ms, created_at_unix_ms, answered_at_unix_ms FROM questions WHERE question_id = ?"
        )
        .bind(question_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read question by ID", error))?;
        row.map(|row| question_from_row(&row)).transpose()
    }

    pub async fn question_by_scope(
        &self,
        scope_key: &str,
    ) -> Result<Option<QuestionRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT question_id, scope_key, session_id, task_id, run_id, kind, prompt, payload_json, state, answer_json, answer_hash, answered_by, expires_at_unix_ms, created_at_unix_ms, answered_at_unix_ms FROM questions WHERE scope_key = ?"
        )
        .bind(scope_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read question by scope", error))?;
        row.map(|row| question_from_row(&row)).transpose()
    }

    pub async fn list_questions(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<QuestionRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT question_id, scope_key, session_id, task_id, run_id, kind, prompt, payload_json, state, answer_json, answer_hash, answered_by, expires_at_unix_ms, created_at_unix_ms, answered_at_unix_ms FROM questions WHERE session_id = ? ORDER BY rowid"
        )
        .bind(session_id.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "list questions", error))?;
        rows.iter().map(question_from_row).collect()
    }

    /// Answer one question with one-shot, scope-checked dedupe.
    ///
    /// The stored answer is the record; answering again with the same payload
    /// returns it, answering with a different payload is a conflict, and an
    /// expired question refuses both. An empty answer is refused before the
    /// store: no timeout and no blank line is consent.
    pub async fn answer_question(
        &self,
        scope_key: &str,
        answer: &Value,
        answer_hash: &ContentHash,
        answered_by: &str,
        now_unix_ms: u64,
    ) -> Result<QuestionOutcome, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        let row = sqlx::query(
            "SELECT question_id, scope_key, session_id, task_id, run_id, kind, prompt, payload_json, state, answer_json, answer_hash, answered_by, expires_at_unix_ms, created_at_unix_ms, answered_at_unix_ms FROM questions WHERE scope_key = ?"
        )
        .bind(scope_key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read question to answer", error))?;
        let Some(row) = row else {
            return Ok(QuestionOutcome::NotFound);
        };
        let current = question_from_row(&row)?;
        match current.state {
            QuestionState::Answered => {
                return Ok(if current.answer_hash.as_ref() == Some(answer_hash) {
                    QuestionOutcome::Duplicate(current)
                } else {
                    QuestionOutcome::Conflict(current)
                });
            }
            QuestionState::Expired | QuestionState::Canceled => {
                return Ok(QuestionOutcome::Expired(current));
            }
            QuestionState::Open => {}
        }
        if current
            .expires_at_unix_ms
            .is_some_and(|expires| expires <= now_unix_ms)
        {
            sqlx::query("UPDATE questions SET state = 'expired' WHERE question_id = ?")
                .bind(current.question_id.as_str())
                .execute(&mut *tx)
                .await
                .map_err(|error| {
                    database_error(ErrorCode::StorageWriteFailed, "expire question", error)
                })?;
            tx.commit().await.map_err(|error| {
                database_error(
                    ErrorCode::StorageWriteFailed,
                    "commit question expiry",
                    error,
                )
            })?;
            let mut expired = current;
            expired.state = QuestionState::Expired;
            return Ok(QuestionOutcome::Expired(expired));
        }
        let payload = to_json(answer, "serialize question answer")?;
        let updated = sqlx::query("UPDATE questions SET state = 'answered', answer_json = ?, answer_hash = ?, answered_by = ?, answered_at_unix_ms = ? WHERE question_id = ? AND state = 'open'")
            .bind(payload)
            .bind(answer_hash.as_str())
            .bind(answered_by)
            .bind(to_i64(now_unix_ms, "answer time")?)
            .bind(current.question_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "answer question", error))?;
        if updated.rows_affected() != 1 {
            return Err(StoreError::new(
                ErrorCode::SequenceConflict,
                "question was answered concurrently",
            ));
        }
        if self
            .fault_plan_ref()
            .consume(StoreFaultPoint::BeforeQuestionAnswerCommit)
        {
            return Err(StoreError::new(
                ErrorCode::StorageWriteFailed,
                "injected failure before question answer commit",
            ));
        }
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit question answer",
                error,
            )
        })?;
        let mut answered = current;
        answered.state = QuestionState::Answered;
        answered.answer = Some(answer.clone());
        answered.answer_hash = Some(answer_hash.clone());
        answered.answered_by = Some(answered_by.to_owned());
        answered.answered_at_unix_ms = Some(now_unix_ms);
        Ok(QuestionOutcome::Answered(answered))
    }

    /// Cancel an open question. A canceled question refuses every answer; it is
    /// not a grant and it is not an expiry.
    pub async fn cancel_question(
        &self,
        question_id: &QuestionId,
    ) -> Result<QuestionRecord, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        let row = sqlx::query(
            "SELECT question_id, scope_key, session_id, task_id, run_id, kind, prompt, payload_json, state, answer_json, answer_hash, answered_by, expires_at_unix_ms, created_at_unix_ms, answered_at_unix_ms FROM questions WHERE question_id = ?"
        )
        .bind(question_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read question to cancel", error))?
        .ok_or_else(|| StoreError::new(ErrorCode::InvalidPayload, "question does not exist"))?;
        let mut current = question_from_row(&row)?;
        if current.state == QuestionState::Open {
            sqlx::query(
                "UPDATE questions SET state = 'canceled' WHERE question_id = ? AND state = 'open'",
            )
            .bind(question_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageWriteFailed, "cancel question", error)
            })?;
            current.state = QuestionState::Canceled;
        }
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit question cancel",
                error,
            )
        })?;
        Ok(current)
    }

    pub async fn enqueue_run_command(&self, record: RunCommandRecord) -> Result<bool, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        let payload = to_json(&record.payload, "serialize run command")?;
        let inserted = sqlx::query("INSERT INTO run_commands(command_id, run_id, session_id, task_id, kind, state, payload_json, detail, created_at_unix_ms, claimed_at_unix_ms, applied_at_unix_ms) VALUES (?, ?, ?, ?, ?, 'pending', ?, NULL, ?, NULL, NULL) ON CONFLICT(command_id) DO NOTHING")
            .bind(record.command_id.as_str())
            .bind(record.run_id.as_str())
            .bind(record.session_id.as_str())
            .bind(record.task_id.as_str())
            .bind(record.kind.as_str())
            .bind(payload)
            .bind(to_i64(record.created_at_unix_ms, "command time")?)
            .execute(&mut *tx)
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "enqueue run command", error))?;
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit run command", error)
        })?;
        Ok(inserted.rows_affected() == 1)
    }

    /// Claim the oldest pending commands for one run at a safe boundary.
    ///
    /// Claiming is atomic: a command is claimed by exactly one turn, and a
    /// cancel claimed here is the caller's stop signal rather than a message.
    pub async fn claim_run_commands(
        &self,
        run_id: &AgentRunId,
        limit: u32,
        now_unix_ms: u64,
    ) -> Result<Vec<RunCommandRecord>, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        sqlx::query("UPDATE run_commands SET state = 'claimed', claimed_at_unix_ms = ? WHERE command_id IN (SELECT command_id FROM run_commands WHERE run_id = ? AND state = 'pending' ORDER BY rowid LIMIT ?) AND state = 'pending'")
            .bind(to_i64(now_unix_ms, "command claim time")?)
            .bind(run_id.as_str())
            .bind(i64::from(limit))
            .execute(&mut *tx)
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "claim run commands", error))?;
        let rows = sqlx::query(
            "SELECT command_id, run_id, session_id, task_id, kind, state, payload_json, detail, created_at_unix_ms, claimed_at_unix_ms, applied_at_unix_ms FROM run_commands WHERE run_id = ? AND state = 'claimed' AND claimed_at_unix_ms = ? ORDER BY rowid"
        )
        .bind(run_id.as_str())
        .bind(to_i64(now_unix_ms, "command claim time")?)
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read claimed commands", error))?;
        let commands = rows
            .iter()
            .map(command_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit command claim", error)
        })?;
        Ok(commands)
    }

    pub async fn complete_run_command(
        &self,
        command_id: &RuntimeCommandId,
        state: RunCommandState,
        detail: Option<&str>,
        now_unix_ms: u64,
    ) -> Result<(), StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        let updated = sqlx::query("UPDATE run_commands SET state = ?, detail = ?, applied_at_unix_ms = ? WHERE command_id = ? AND state = 'claimed'")
            .bind(state.as_str())
            .bind(detail)
            .bind(to_i64(now_unix_ms, "command applied time")?)
            .bind(command_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "complete run command", error))?;
        if updated.rows_affected() != 1 {
            return Err(StoreError::new(
                ErrorCode::RuntimeCommandConflict,
                "run command was not claimed by this host",
            ));
        }
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit run command completion",
                error,
            )
        })
    }

    /// Settle a reservation that is not tied to a run step (a retry attempt).
    pub async fn settle_budget_reservation(
        &self,
        settlement: BudgetSettlement,
    ) -> Result<BudgetReservationRecord, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        let record = settle_reservation_in_tx(&mut tx, &settlement).await?;
        if self
            .fault_plan_ref()
            .consume(StoreFaultPoint::BeforeBudgetSettleCommit)
        {
            return Err(StoreError::new(
                ErrorCode::StorageWriteFailed,
                "injected failure before budget settlement commit",
            ));
        }
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit budget settlement",
                error,
            )
        })?;
        Ok(record)
    }
}

/// Settle one reservation inside the caller's transaction.
///
/// Idempotent by construction: the same measured usage returns the stored
/// result, and a different usage is a conflict instead of a second charge.
async fn settle_reservation_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    settlement: &BudgetSettlement,
) -> Result<BudgetReservationRecord, StoreError> {
    let row = sqlx::query("SELECT reservation_id, budget_id, operation_id, origin, upper_bound_tokens, settled_tokens, state, revision FROM budget_reservations WHERE reservation_id = ?")
        .bind(settlement.reservation_id.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read reservation", error))?
        .ok_or_else(|| StoreError::new(ErrorCode::InvalidPayload, "budget reservation does not exist"))?;
    let mut current = reservation_from_row(&row)?;
    let already_done = matches!(
        current.state,
        BudgetReservationState::Settled
            | BudgetReservationState::Unknown
            | BudgetReservationState::Released
            | BudgetReservationState::Expired
    );
    if already_done {
        if current.settled_tokens != settlement.measured_tokens {
            return Err(StoreError::new(
                ErrorCode::IdempotencyConflict,
                "budget reservation was already settled with different usage",
            ));
        }
        return Ok(current);
    }
    let next_state = if settlement.measured_tokens.is_some() {
        BudgetReservationState::Settled
    } else {
        BudgetReservationState::Unknown
    };
    let delta = match settlement.measured_tokens {
        Some(measured) => i128::from(measured) - i128::from(current.upper_bound_tokens),
        None => 0,
    };
    if delta != 0 {
        charge_chain(
            tx,
            &current.budget_id,
            i64::try_from(delta).map_err(|_| {
                StoreError::new(ErrorCode::BudgetExhausted, "budget delta overflow")
            })?,
        )
        .await?;
    }
    let updated = sqlx::query("UPDATE budget_reservations SET state = ?, settled_tokens = ?, revision = revision + 1 WHERE reservation_id = ? AND revision = ?")
        .bind(next_state.as_str())
        .bind(
            settlement
                .measured_tokens
                .map(|value| to_i64(value, "settled tokens"))
                .transpose()?,
        )
        .bind(settlement.reservation_id.as_str())
        .bind(to_i64(current.revision, "reservation revision")?)
        .execute(&mut **tx)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "settle reservation", error))?;
    if updated.rows_affected() != 1 {
        return Err(StoreError::new(
            ErrorCode::SequenceConflict,
            "reservation revision moved during settlement",
        ));
    }
    current.state = next_state;
    current.settled_tokens = settlement.measured_tokens;
    current.revision = current.revision.checked_add(1).ok_or_else(|| {
        StoreError::new(
            ErrorCode::StorageWriteFailed,
            "reservation revision overflow",
        )
    })?;
    Ok(current)
}

fn budget_account_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<BudgetAccountRecord, StoreError> {
    Ok(BudgetAccountRecord {
        budget_id: parse_budget_id(row_get(row, "budget_id")?)?,
        parent_budget_id: row_get::<Option<String>>(row, "parent_budget_id")?
            .map(parse_budget_id)
            .transpose()?,
        limit_tokens: to_u64(row_get(row, "limit_tokens")?, "budget limit")?,
        spent_tokens: to_u64(row_get(row, "spent_tokens")?, "budget spent")?,
        revision: to_u64(row_get(row, "revision")?, "budget revision")?,
    })
}
