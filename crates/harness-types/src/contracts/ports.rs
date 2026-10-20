//! Transaction-boundary ports (CONTRACTS §3).
//!
//! Methods represent complete atomic writes. They accept the event, projection,
//! provenance and authority records that must commit together; they do not
//! expose a sequence of partial writes for callers to compose.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;

use crate::{
    AdmissionAck, AdmissionCommit, CommitRef, DeliveryCommit, FreezeStepCommit, HarnessError,
    PortRecoveryView, ReceiptAck, ReceiptCommit, RunLease, RunStartRequest, SessionId,
    ToolIntentCommit, ToolSettlementCommit, ToolTaskUpdateCommit,
};

/// First admission or an idempotent replay of the exact same input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdmissionOutcome {
    Admitted(AdmissionAck),
    Duplicate(AdmissionAck),
}

/// Storage adapters do substantial transaction preparation. Boxing at this
/// boundary keeps that implementation detail from inflating every caller's
/// future (for example, an entire interactive turn state machine).
pub type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, HarnessError>> + Send + 'a>>;

/// The durable store boundary. Each operation maps to one transaction or one
/// read-only recovery query. Implementations must preserve typed conflicts and
/// must never report success before commit.
pub trait StorePort: Send + Sync {
    fn admit_input(&self, commit: AdmissionCommit) -> StoreFuture<'_, AdmissionOutcome>;

    fn claim_run(&self, request: RunStartRequest) -> StoreFuture<'_, RunLease>;

    fn freeze_step(&self, commit: FreezeStepCommit) -> StoreFuture<'_, RunLease>;

    fn record_synthetic_receipt(&self, commit: ReceiptCommit) -> StoreFuture<'_, ReceiptAck>;

    fn admit_invocation(&self, commit: ToolIntentCommit) -> StoreFuture<'_, CommitRef>;

    fn settle_invocation(&self, commit: ToolSettlementCommit) -> StoreFuture<'_, ReceiptAck>;

    fn commit_task_update(&self, commit: ToolTaskUpdateCommit) -> StoreFuture<'_, CommitRef>;

    fn settle_child(&self, commit: DeliveryCommit) -> StoreFuture<'_, ()>;

    fn recover_readonly(&self, session_id: SessionId) -> StoreFuture<'_, PortRecoveryView>;
}

/// Documented capability names used by the M0 scope contract. They are stable
/// strings, not a closed enum, so a later milestone can add one without a
/// breaking change to stored scopes.
pub const CAPABILITY_TOOLS_READ: &str = "tools.read";
pub const CAPABILITY_TOOLS_WRITE: &str = "tools.write";
pub const CAPABILITY_MEMORY_READ: &str = "memory.read";

#[must_use]
pub fn documented_capabilities() -> BTreeSet<&'static str> {
    [
        CAPABILITY_TOOLS_READ,
        CAPABILITY_TOOLS_WRITE,
        CAPABILITY_MEMORY_READ,
    ]
    .into_iter()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::StorePort;
    use crate::{
        AdmissionCommit, AdmissionOutcome, CommitRef, DeliveryCommit, FreezeStepCommit,
        HarnessError, PortRecoveryView, ReceiptAck, ReceiptCommit, RunLease, RunStartRequest,
        SessionId, StoreFuture, ToolIntentCommit, ToolSettlementCommit, ToolTaskUpdateCommit,
    };
    use std::future::Future;

    /// Test-only implementation proves implementability and fails every write
    /// explicitly; production code must use a durable adapter.
    struct UnsupportedStore;

    fn unsupported<T>() -> Result<T, HarnessError> {
        Err(HarnessError::new(
            crate::ErrorCode::ServiceUnavailable,
            "StorePort test double has no durable backend",
        ))
    }

    impl StorePort for UnsupportedStore {
        fn admit_input(&self, _commit: AdmissionCommit) -> StoreFuture<'_, AdmissionOutcome> {
            Box::pin(std::future::ready(unsupported()))
        }

        fn claim_run(&self, _request: RunStartRequest) -> StoreFuture<'_, RunLease> {
            Box::pin(std::future::ready(unsupported()))
        }

        fn freeze_step(&self, _commit: FreezeStepCommit) -> StoreFuture<'_, RunLease> {
            Box::pin(std::future::ready(unsupported()))
        }

        fn record_synthetic_receipt(&self, _commit: ReceiptCommit) -> StoreFuture<'_, ReceiptAck> {
            Box::pin(std::future::ready(unsupported()))
        }

        fn admit_invocation(&self, _commit: ToolIntentCommit) -> StoreFuture<'_, CommitRef> {
            Box::pin(std::future::ready(unsupported()))
        }

        fn settle_invocation(&self, _commit: ToolSettlementCommit) -> StoreFuture<'_, ReceiptAck> {
            Box::pin(std::future::ready(unsupported()))
        }

        fn commit_task_update(&self, _commit: ToolTaskUpdateCommit) -> StoreFuture<'_, CommitRef> {
            Box::pin(std::future::ready(unsupported()))
        }

        fn settle_child(&self, _commit: DeliveryCommit) -> StoreFuture<'_, ()> {
            Box::pin(std::future::ready(unsupported()))
        }

        fn recover_readonly(&self, _session_id: SessionId) -> StoreFuture<'_, PortRecoveryView> {
            Box::pin(std::future::ready(unsupported()))
        }
    }

    #[test]
    fn the_test_double_never_returns_fake_success() {
        let store = UnsupportedStore;
        let error = futures_lite_block_on(store.recover_readonly(SessionId::generate()))
            .expect_err("test double must reject reads without a backend");
        assert_eq!(error.code(), crate::ErrorCode::ServiceUnavailable);
    }

    /// Minimal executor keeps the shared contracts crate runtime-free.
    fn futures_lite_block_on<T>(future: impl Future<Output = T>) -> T {
        use std::task::{Context, Poll, Waker};
        let mut future = Box::pin(future);
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(value) => return value,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }
}
