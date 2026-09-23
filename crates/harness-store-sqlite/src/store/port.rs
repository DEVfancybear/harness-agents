//! Production `StorePort` adapter for the `SQLite` transaction coordinator.

use harness_types::{
    AdmissionCommit, AdmissionOutcome, CommitRef, DeliveryCommit, FreezeStepCommit,
    FrozenBudgetReservation, HarnessError, PortRecoveryView, ReceiptCommit, RunLease,
    RunStartRequest, StoreFuture, StorePort, ToolIntentCommit, ToolSettlementCommit,
    ToolTaskUpdateCommit,
};

use super::{database_error, to_u64};
use crate::{
    BudgetReservationRecord, BudgetReservationState, RunRecord, RunStepRecord, SqliteStore,
    StoreError,
};

fn port_error(error: StoreError) -> HarnessError {
    error.into_harness_error()
}

fn lease(record: RunRecord) -> RunLease {
    RunLease {
        run_id: record.run_id,
        session_id: record.session_id,
        task_id: record.task_id,
        input_id: record.input_id,
        owner_generation: record.owner_generation,
        revision: record.revision,
    }
}

fn reservation(value: FrozenBudgetReservation) -> BudgetReservationRecord {
    BudgetReservationRecord {
        reservation_id: value.reservation_id,
        budget_id: value.budget_id,
        operation_id: value.operation_id,
        origin: value.origin,
        upper_bound_tokens: value.upper_bound_tokens,
        settled_tokens: None,
        state: BudgetReservationState::Reserved,
        revision: 1,
    }
}

impl StorePort for SqliteStore {
    fn admit_input(&self, commit: AdmissionCommit) -> StoreFuture<'_, AdmissionOutcome> {
        Box::pin(async move {
            let ack = self.commit_admission(commit).await.map_err(port_error)?;
            Ok(if ack.idempotent_replay {
                AdmissionOutcome::Duplicate(ack)
            } else {
                AdmissionOutcome::Admitted(ack)
            })
        })
    }

    fn claim_run(&self, request: RunStartRequest) -> StoreFuture<'_, RunLease> {
        Box::pin(async move {
            let fence = self.fence().map_err(port_error)?;
            if request.expected_owner_generation != fence.generation {
                return Err(HarnessError::new(
                    harness_types::ErrorCode::StaleWriter,
                    "run claim used a stale owner generation",
                ));
            }
            let record = self
                .start_run(
                    &request.session_id,
                    &request.task_id,
                    &request.input_id,
                    request.budget_id.as_ref(),
                )
                .await
                .map_err(port_error)?;
            Ok(lease(record))
        })
    }

    fn freeze_step(&self, commit: FreezeStepCommit) -> StoreFuture<'_, RunLease> {
        Box::pin(async move {
            let fence = self.fence().map_err(port_error)?;
            if commit.expected_owner_generation != fence.generation {
                return Err(HarnessError::new(
                    harness_types::ErrorCode::StaleWriter,
                    "step freeze used a stale owner generation",
                ));
            }
            let step = RunStepRecord {
                step_id: commit.step.step_id,
                run_id: commit.step.run_id,
                step_index: commit.step.step_index,
                request_id: commit.step.request_id,
                packet_id: commit.step.packet_id,
                manifest_hash: commit.step.manifest_hash,
                source_sequence: commit.step.source_sequence,
                state: commit.step.state,
                stop_reason: commit.step.stop_reason,
            };
            let budget = commit.reservation.map(reservation);
            let record = self
                .freeze_run_step(&commit.run_id, commit.expected_revision, step, budget)
                .await
                .map_err(port_error)?;
            Ok(lease(record))
        })
    }

    fn record_synthetic_receipt(
        &self,
        commit: ReceiptCommit,
    ) -> StoreFuture<'_, crate::ReceiptAck> {
        Box::pin(async move { self.commit_receipt(commit).await.map_err(port_error) })
    }

    fn admit_invocation(&self, commit: ToolIntentCommit) -> StoreFuture<'_, CommitRef> {
        Box::pin(async move {
            let result = CommitRef {
                event_id: commit.event.event_id.clone(),
                seq: commit.event.seq,
                state_revision: commit.working_state.revision,
            };
            self.commit_tool_intent(commit).await.map_err(port_error)?;
            Ok(result)
        })
    }

    fn settle_invocation(
        &self,
        commit: ToolSettlementCommit,
    ) -> StoreFuture<'_, crate::ReceiptAck> {
        Box::pin(async move {
            self.commit_tool_settlement(commit)
                .await
                .map_err(port_error)
        })
    }

    fn commit_task_update(&self, commit: ToolTaskUpdateCommit) -> StoreFuture<'_, CommitRef> {
        Box::pin(async move {
            let result = CommitRef {
                event_id: commit.event.event_id.clone(),
                seq: commit.event.seq,
                state_revision: commit.working_state.revision,
            };
            self.commit_tool_task_update(commit)
                .await
                .map_err(port_error)?;
            Ok(result)
        })
    }

    fn settle_child(&self, commit: DeliveryCommit) -> StoreFuture<'_, ()> {
        Box::pin(async move { self.commit_delivery(commit).await.map_err(port_error) })
    }

    fn recover_readonly(
        &self,
        session_id: harness_types::SessionId,
    ) -> StoreFuture<'_, PortRecoveryView> {
        Box::pin(async move {
            // Read every recovery field from one SQLite snapshot; independent
            // pool queries could otherwise miss an effect committed mid-read.
            let mut tx = self.pool.begin().await.map_err(|error| {
                port_error(database_error(
                    harness_types::ErrorCode::StorageOpenFailed,
                    "begin recovery snapshot",
                    error,
                ))
            })?;
            let next_sequence = sqlx::query_scalar::<_, i64>(
                "SELECT next_sequence FROM sessions WHERE session_id = ?",
            )
            .bind(session_id.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| {
                port_error(database_error(
                    harness_types::ErrorCode::StorageOpenFailed,
                    "read recovery session",
                    error,
                ))
            })?
            .ok_or_else(|| {
                HarnessError::new(
                    harness_types::ErrorCode::InvalidPayload,
                    "session does not exist",
                )
            })?;
            let replayed_through_sequence = to_u64(next_sequence, "next session sequence")
                .map_err(port_error)?
                .checked_sub(1)
                .ok_or_else(|| {
                    HarnessError::new(
                        harness_types::ErrorCode::StorageOpenFailed,
                        "stored next session sequence is zero",
                    )
                })?;
            let pending_intents = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM tool_intents WHERE session_id = ? AND status = 'recorded'",
            )
            .bind(session_id.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| {
                port_error(database_error(
                    harness_types::ErrorCode::StorageOpenFailed,
                    "count pending tool intents",
                    error,
                ))
            })?;
            let pending_intents =
                to_u64(pending_intents, "pending tool intent count").map_err(port_error)?;
            let pending_commands = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM runtime_commands
                 WHERE session_id = ? AND state IN ('pending', 'claimed')",
            )
            .bind(session_id.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| {
                port_error(database_error(
                    harness_types::ErrorCode::StorageOpenFailed,
                    "count pending runtime commands",
                    error,
                ))
            })?;
            let pending_commands =
                to_u64(pending_commands, "pending runtime command count").map_err(port_error)?;
            let pending_effects =
                pending_intents
                    .checked_add(pending_commands)
                    .ok_or_else(|| {
                        HarnessError::new(
                            harness_types::ErrorCode::StorageOpenFailed,
                            "pending effect count overflow",
                        )
                    })?;
            tx.commit().await.map_err(|error| {
                port_error(database_error(
                    harness_types::ErrorCode::StorageOpenFailed,
                    "finish recovery snapshot",
                    error,
                ))
            })?;
            Ok(PortRecoveryView {
                session_id,
                replayed_through_sequence,
                pending_effects,
                blocked_reason: (pending_effects > 0)
                    .then(|| "unsettled external effects require reconciliation".to_owned()),
            })
        })
    }
}
