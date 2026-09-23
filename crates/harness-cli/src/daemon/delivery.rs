//! The notification outbox and the only place anything is ever sent from.
//!
//! A notification is a *durable record of a meaningful change* first, and a
//! message second:
//!
//! - the change digest is computed from the meaningful payload, so a re-render
//!   that means the same thing is the same notification;
//! - nothing leaves the host until a connector is configured, and a host with no
//!   connector simply keeps the record;
//! - retries are bounded by the store's arithmetic, so a connector that is down
//!   produces a visible `failed` row rather than an unbounded retry loop.

use std::sync::Arc;

use harness_store_sqlite::{
    NOTIFICATION_MAX_ATTEMPTS, SqliteStore, StoredNotification, notification_backoff_ms,
};
use harness_types::{ContentHash, ErrorCode, HarnessError};
use serde_json::Value;

use super::Clock;

/// Where a notification could go.
///
/// This build configures none: a connector is a decision about a user's
/// accounts, and the outbox is designed to be complete without one. The trait
/// exists so the decision is a single wiring point rather than a code path.
pub trait NotificationConnector: Send + Sync {
    /// The channel this connector delivers on, for the record.
    fn channel(&self) -> &str;

    /// Deliver one notification.
    fn send<'a>(
        &'a self,
        notification: &'a StoredNotification,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), HarnessError>> + Send + 'a>>;
}

/// What one change is about.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NotificationEvent {
    /// `schedule`, `occurrence` or `external_job`.
    pub subject_kind: String,
    pub subject_id: String,
    pub kind: String,
    /// The *meaningful* payload: what a reader would need to decide whether to
    /// act. Two events with equal payloads are one notification.
    pub payload: Value,
}

/// What a delivery pass did.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeliveryReport {
    /// Rows the pass looked at.
    pub considered: usize,
    pub delivered: usize,
    pub retried: usize,
    pub failed: usize,
    /// Notifications left untouched because no connector is configured. They
    /// stay pending: nothing was attempted, so nothing was lost.
    pub unconfigured: usize,
}

/// The daemon's outbox worker.
pub struct NotificationOutbox {
    store: Arc<SqliteStore>,
    connector: Option<Arc<dyn NotificationConnector>>,
    clock: Arc<dyn Clock>,
}

impl NotificationOutbox {
    #[must_use]
    pub fn new(
        store: Arc<SqliteStore>,
        connector: Option<Arc<dyn NotificationConnector>>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
            connector,
            clock,
        }
    }

    /// Whether this host has anywhere to send a notification.
    #[must_use]
    pub fn channel(&self) -> Option<&str> {
        self.connector.as_ref().map(|connector| connector.channel())
    }

    /// Record a meaningful change, once.
    ///
    /// Returns `true` when this change had not been recorded before. The id is
    /// derived from the subject and the change digest, so the same change under
    /// two spellings is one row rather than two messages.
    ///
    /// # Errors
    /// Fails when the store refuses, or when the payload cannot be hashed.
    pub async fn notify(&self, event: &NotificationEvent) -> Result<bool, HarnessError> {
        let digest = ContentHash::from_canonical_json(&event.payload).map_err(|_| {
            HarnessError::new(
                ErrorCode::InvalidPayload,
                "the notification payload is not hashable",
            )
        })?;
        let now = self.clock.now_unix_ms();
        let record = StoredNotification {
            notification_id: format!(
                "notification_{}_{}",
                event.subject_id,
                digest.as_str().trim_start_matches("sha256:")
            ),
            subject_kind: event.subject_kind.clone(),
            subject_id: event.subject_id.clone(),
            change_digest: digest.as_str().to_owned(),
            kind: event.kind.clone(),
            payload_json: serde_json::to_string(&event.payload).map_err(|_| {
                HarnessError::new(ErrorCode::InvalidPayload, "the payload is not JSON")
            })?,
            state: "pending".to_owned(),
            attempts: 0,
            next_attempt_unix_ms: now,
            created_at_unix_ms: now,
            updated_at_unix_ms: now,
            delivered_at_unix_ms: None,
            detail: None,
        };
        self.store
            .enqueue_notification(&record)
            .await
            .map_err(store_error)
    }

    /// Try to deliver every notification that is due.
    ///
    /// # Errors
    /// Fails when the store refuses. A connector that refuses is not an error
    /// here: it is a row that will be retried, and then left failed.
    pub async fn deliver_due(&self) -> Result<DeliveryReport, HarnessError> {
        let now = self.clock.now_unix_ms();
        let due = self
            .store
            .due_notifications(now)
            .await
            .map_err(store_error)?;
        let mut report = DeliveryReport::default();
        let Some(connector) = &self.connector else {
            // No connector: the record stays pending and nothing is attempted.
            report.considered = due.len();
            report.unconfigured = due.len();
            return Ok(report);
        };
        for notification in due {
            report.considered += 1;
            match connector.send(&notification).await {
                Ok(()) => {
                    self.store
                        .record_notification_attempt(
                            &notification.notification_id,
                            "delivered",
                            connector.channel(),
                            None,
                            now,
                        )
                        .await
                        .map_err(store_error)?;
                    report.delivered += 1;
                }
                Err(error) => {
                    let attempts = notification.attempts.saturating_add(1);
                    if attempts >= NOTIFICATION_MAX_ATTEMPTS {
                        self.store
                            .record_notification_attempt(
                                &notification.notification_id,
                                "failed",
                                error.message(),
                                None,
                                now,
                            )
                            .await
                            .map_err(store_error)?;
                        report.failed += 1;
                    } else {
                        let delay = notification_backoff_ms(attempts);
                        self.store
                            .record_notification_attempt(
                                &notification.notification_id,
                                "pending",
                                error.message(),
                                Some(now.saturating_add(delay)),
                                now,
                            )
                            .await
                            .map_err(store_error)?;
                        report.retried += 1;
                    }
                }
            }
        }
        Ok(report)
    }

    /// Everything the outbox is holding, for a status report.
    ///
    /// # Errors
    /// Fails when the store refuses.
    pub async fn pending(&self) -> Result<Vec<StoredNotification>, HarnessError> {
        let all = self.store.list_notifications().await.map_err(store_error)?;
        Ok(all
            .into_iter()
            .filter(|notification| !notification.is_terminal())
            .collect())
    }
}

fn store_error(error: harness_store_sqlite::StoreError) -> HarnessError {
    error.into_harness_error()
}
