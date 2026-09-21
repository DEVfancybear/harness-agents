//! Durable budget reservations (M3-03).
//!
//! A reservation is the host's promise to itself: the operation may spend at
//! most `upper_bound_tokens`, and the account chain is charged before the
//! operation is dispatched. Settlement replaces the bound with the measured
//! usage; when the usage is unknown the conservative bound stays charged, so a
//! lost response can never look like a free call. All arithmetic is checked and
//! all mutations happen inside one store transaction, so two concurrent
//! reservations cannot over-admit an account.

use std::sync::Arc;

use harness_store_sqlite::{
    BudgetAccountRecord, BudgetReservationRecord, BudgetReservationState, BudgetSettlement,
    SqliteStore, StoreError,
};
use harness_types::{BudgetId, BudgetReservationId, ErrorCode};

use crate::RuntimeError;

/// What one operation is allowed to spend, in host-estimated tokens.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Usage {
    /// The provider (or the host) reported a measurable usage.
    Measured(u64),
    /// The operation ended without a measurable usage. The reservation keeps
    /// its conservative bound until an explicit reconciliation releases it.
    Unknown,
}

/// One account's state after an operation.
#[derive(Clone, Debug, PartialEq)]
pub struct BudgetView {
    pub account: BudgetAccountRecord,
    pub remaining_tokens: u64,
}

/// Durable reservation/settlement over one store.
#[derive(Clone)]
pub struct BudgetLedger {
    store: Arc<SqliteStore>,
}

impl BudgetLedger {
    #[must_use]
    pub fn new(store: Arc<SqliteStore>) -> Self {
        Self { store }
    }

    /// Create the account if it does not exist, or return the existing shape.
    pub async fn ensure_account(
        &self,
        budget_id: &BudgetId,
        parent: Option<&BudgetId>,
        limit_tokens: u64,
    ) -> Result<BudgetAccountRecord, RuntimeError> {
        if limit_tokens == 0 {
            return Err(RuntimeError::new(
                ErrorCode::BudgetExhausted,
                "a budget account needs a positive limit",
            ));
        }
        self.store
            .ensure_budget_account(BudgetAccountRecord {
                budget_id: budget_id.clone(),
                parent_budget_id: parent.cloned(),
                limit_tokens,
                spent_tokens: 0,
                revision: 1,
            })
            .await
            .map_err(RuntimeError::from)
    }

    /// Reserve an upper bound for one operation.
    ///
    /// The operation ID is the idempotency key: reserving the same operation
    /// twice returns the first reservation instead of charging twice.
    pub async fn reserve(
        &self,
        budget_id: &BudgetId,
        operation_id: &str,
        origin: &str,
        upper_bound_tokens: u64,
    ) -> Result<BudgetReservationRecord, RuntimeError> {
        if operation_id.trim().is_empty() {
            return Err(RuntimeError::new(
                ErrorCode::InvalidPayload,
                "a budget reservation needs an operation ID",
            ));
        }
        if upper_bound_tokens == 0 {
            return Err(RuntimeError::new(
                ErrorCode::InvalidPayload,
                "a budget reservation needs a positive bound",
            ));
        }
        self.store
            .reserve_budget(BudgetReservationRecord {
                reservation_id: BudgetReservationId::generate(),
                budget_id: budget_id.clone(),
                operation_id: operation_id.to_owned(),
                origin: origin.to_owned(),
                upper_bound_tokens,
                settled_tokens: None,
                state: BudgetReservationState::Reserved,
                revision: 1,
            })
            .await
            .map_err(RuntimeError::from)
    }

    /// Settle a reservation with what the operation actually used.
    ///
    /// Settling the same reservation twice with the same usage is a no-op that
    /// returns the stored record; a different usage is a conflict, never a
    /// second charge.
    pub async fn settle(
        &self,
        reservation_id: &BudgetReservationId,
        usage: Usage,
    ) -> Result<BudgetReservationRecord, RuntimeError> {
        let measured = match usage {
            Usage::Measured(tokens) => Some(tokens),
            Usage::Unknown => None,
        };
        self.store
            .settle_budget_reservation(BudgetSettlement {
                reservation_id: reservation_id.clone(),
                measured_tokens: measured,
            })
            .await
            .map_err(RuntimeError::from)
    }

    /// Release a reservation whose operation never dispatched.
    pub async fn release(
        &self,
        reservation_id: &BudgetReservationId,
    ) -> Result<BudgetReservationRecord, RuntimeError> {
        self.store
            .release_budget_reservation(reservation_id)
            .await
            .map_err(RuntimeError::from)
    }

    pub async fn reservation(
        &self,
        operation_id: &str,
    ) -> Result<Option<BudgetReservationRecord>, RuntimeError> {
        self.store
            .budget_reservation_by_operation(operation_id)
            .await
            .map_err(RuntimeError::from)
    }

    pub async fn view(&self, budget_id: &BudgetId) -> Result<BudgetView, RuntimeError> {
        let account = self
            .store
            .budget_account(budget_id)
            .await
            .map_err(RuntimeError::from)?
            .ok_or_else(|| {
                RuntimeError::new(ErrorCode::BudgetExhausted, "budget account does not exist")
            })?;
        let remaining_tokens = account.limit_tokens.saturating_sub(account.spent_tokens);
        Ok(BudgetView {
            account,
            remaining_tokens,
        })
    }

    /// How much an account may still commit, including every ancestor limit.
    pub async fn remaining(&self, budget_id: &BudgetId) -> Result<u64, RuntimeError> {
        let mut remaining = u64::MAX;
        let mut current = Some(budget_id.clone());
        while let Some(account_id) = current {
            let account = self
                .store
                .budget_account(&account_id)
                .await
                .map_err(RuntimeError::from)?
                .ok_or_else(|| {
                    RuntimeError::new(ErrorCode::BudgetExhausted, "budget account does not exist")
                })?;
            remaining = remaining.min(account.limit_tokens.saturating_sub(account.spent_tokens));
            current = account.parent_budget_id;
        }
        Ok(remaining)
    }

    /// Estimate a text's token cost the way the context compiler does.
    #[must_use]
    pub fn estimate_tokens(text: &str) -> u64 {
        u64::try_from(text.len().saturating_add(3) / 4)
            .unwrap_or(u64::MAX)
            .max(1)
    }

    /// Convenience for callers that only have a store error to classify.
    #[must_use]
    pub fn classify(error: &StoreError) -> RuntimeError {
        RuntimeError::new(error.code(), error.to_string())
    }
}
