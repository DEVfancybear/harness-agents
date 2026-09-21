//! Transaction-boundary ports (CONTRACTS §3).
//!
//! These are contract-only declarations: M0 fixes the names, argument shapes and
//! outcomes that the durable store must expose, without shipping an
//! implementation. The `SQLite` store is the only planned implementor (M1); no
//! production module may implement these traits with a success placeholder
//! before that milestone. A test-only double in this module returns a typed
//! unsupported error so the shapes stay compilable.

use std::collections::BTreeSet;
use std::future::Future;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    AgentRunId, ContentHash, ContextPacket, EventId, HarnessError, InputId, RequestId, SessionId,
    SourceAuthority, StepId, TaskId, ToolInvocationId, ToolOutcomeState,
};

/// The durable result of admitting one input.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdmittedInput {
    pub event_id: EventId,
    pub seq: u64,
    pub state_revision: u64,
}

/// Admitted for the first time, or the earlier identical admission. A same ID
/// with a different payload is never one of these: it fails.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdmissionOutcome {
    Admitted(AdmittedInput),
    Duplicate(AdmittedInput),
}

/// The right to append to one session until the owner generation moves.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunLease {
    pub run_id: AgentRunId,
    pub session_id: SessionId,
    pub owner_generation: u64,
}

/// A committed journal position.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommitRef {
    pub event_id: EventId,
    pub seq: u64,
    pub state_revision: u64,
}

/// A validated domain change, ready to append.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DomainChange {
    pub event_type: String,
    pub authority: SourceAuthority,
    pub payload: Map<String, Value>,
}

/// The frozen step identity written through `freeze_step`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenStepRef {
    pub step_id: StepId,
    pub request_id: RequestId,
    pub source_seq: u64,
}

/// A budget reservation passed to `freeze_step`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetReservation {
    pub operation_id: String,
    pub upper_bound: u64,
}

/// What a caller proposes to execute.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationProposal {
    pub invocation_id: ToolInvocationId,
    pub call_id: String,
    pub action: String,
    pub action_hash: ContentHash,
    pub workspace_fingerprint: ContentHash,
}

/// A one-shot grant bound to one proposal.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationGrant {
    pub invocation_id: ToolInvocationId,
    pub action_hash: ContentHash,
    pub workspace_fingerprint: ContentHash,
    pub policy_revision: u64,
    pub expires_at_seq: u64,
    pub allowed_effects: Vec<String>,
}

/// The durable intent that `admit_invocation` returns.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IntentRef {
    pub intent_id: ToolInvocationId,
    pub request_hash: ContentHash,
}

/// The observable part of one execution receipt.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationReceiptRef {
    pub invocation_id: ToolInvocationId,
    pub outcome: ToolOutcomeState,
    pub before_fingerprint: Option<ContentHash>,
    pub after_fingerprint: Option<ContentHash>,
    pub exit_code: Option<i32>,
    /// `Some` when the executor could not prove the effect; the value is never
    /// treated as failure.
    pub uncertainty: Option<String>,
}

/// A child result delivered to a parent.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChildResultRef {
    pub child_task_id: TaskId,
    pub execution_id: String,
    pub outcome: String,
    pub result_hash: ContentHash,
}

/// A durable parent delivery.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryRef {
    pub message_id: String,
    pub payload_hash: ContentHash,
}

/// The read-only recovery view returned by `recover_readonly`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortRecoveryView {
    pub session_id: SessionId,
    pub replayed_through_sequence: u64,
    pub pending_effects: u64,
    pub blocked_reason: Option<String>,
}

/// The writable store boundary. Every method is a transaction of its own;
/// callers never compose raw statements.
pub trait StorePort: Send + Sync {
    /// Admit one input. Duplicate ID + payload hash returns the earlier result;
    /// the same ID with another payload is `IdempotencyConflict`.
    fn admit_input<'a>(
        &'a self,
        session_id: &'a SessionId,
        input_id: &'a InputId,
        payload_hash: &'a ContentHash,
        expected_seq: u64,
        content: DomainChange,
    ) -> impl Future<Output = Result<AdmissionOutcome, HarnessError>> + Send + 'a;

    /// Claim the single writable lease for a session/run.
    fn claim_run<'a>(
        &'a self,
        session_id: &'a SessionId,
        command_id: &'a str,
        expected_owner_generation: u64,
    ) -> impl Future<Output = Result<RunLease, HarnessError>> + Send + 'a;

    /// Append one validated domain change at the expected sequence.
    fn append_domain_change<'a>(
        &'a self,
        lease: &'a RunLease,
        expected_seq: u64,
        change: DomainChange,
    ) -> impl Future<Output = Result<CommitRef, HarnessError>> + Send + 'a;

    /// Freeze the packet and provider request for one step.
    fn freeze_step<'a>(
        &'a self,
        lease: &'a RunLease,
        source_seq: u64,
        packet: &'a ContextPacket,
        provider_request: Map<String, Value>,
        budget: BudgetReservation,
    ) -> impl Future<Output = Result<FrozenStepRef, HarnessError>> + Send + 'a;

    /// Record the intent for one approved invocation.
    fn admit_invocation<'a>(
        &'a self,
        lease: &'a RunLease,
        proposal: &'a InvocationProposal,
        grant: &'a InvocationGrant,
        expected_workspace: &'a ContentHash,
    ) -> impl Future<Output = Result<IntentRef, HarnessError>> + Send + 'a;

    /// Settle one intent with its immutable receipt and projection change.
    fn settle_invocation<'a>(
        &'a self,
        lease: &'a RunLease,
        intent: &'a IntentRef,
        receipt: InvocationReceiptRef,
        projection_change: DomainChange,
    ) -> impl Future<Output = Result<CommitRef, HarnessError>> + Send + 'a;

    /// Deliver a child result to its parent in one transaction.
    fn settle_child<'a>(
        &'a self,
        lease: &'a RunLease,
        child_result: ChildResultRef,
        parent_delivery: DeliveryRef,
    ) -> impl Future<Output = Result<CommitRef, HarnessError>> + Send + 'a;

    /// Rebuild a read-only view from the journal and checkpoints.
    fn recover_readonly<'a>(
        &'a self,
        session_id: &'a SessionId,
    ) -> impl Future<Output = Result<PortRecoveryView, HarnessError>> + Send + 'a;
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
    use super::{
        AdmissionOutcome, BudgetReservation, ChildResultRef, CommitRef, DeliveryRef, DomainChange,
        FrozenStepRef, IntentRef, InvocationGrant, InvocationProposal, InvocationReceiptRef,
        PortRecoveryView, RunLease, StorePort,
    };
    use crate::{ContentHash, ContextPacket, ErrorCode, HarnessError, InputId, SessionId};
    use serde_json::Map;
    use std::future::Future;

    /// A test-only double: it proves the trait shape is implementable and
    /// callable, and every method returns a typed unsupported error rather than
    /// a success placeholder. It is compiled only for tests.
    struct UnsupportedStore;

    fn unsupported<T>() -> Result<T, HarnessError> {
        Err(HarnessError::new(
            ErrorCode::ServiceUnavailable,
            "StorePort has no implementation before M1",
        ))
    }

    impl StorePort for UnsupportedStore {
        fn admit_input<'a>(
            &'a self,
            _session_id: &'a SessionId,
            _input_id: &'a InputId,
            _payload_hash: &'a ContentHash,
            _expected_seq: u64,
            _content: DomainChange,
        ) -> impl Future<Output = Result<AdmissionOutcome, HarnessError>> + Send + 'a {
            std::future::ready(unsupported())
        }

        fn claim_run<'a>(
            &'a self,
            _session_id: &'a SessionId,
            _command_id: &'a str,
            _expected_owner_generation: u64,
        ) -> impl Future<Output = Result<RunLease, HarnessError>> + Send + 'a {
            std::future::ready(unsupported())
        }

        fn append_domain_change<'a>(
            &'a self,
            _lease: &'a RunLease,
            _expected_seq: u64,
            _change: DomainChange,
        ) -> impl Future<Output = Result<CommitRef, HarnessError>> + Send + 'a {
            std::future::ready(unsupported())
        }

        fn freeze_step<'a>(
            &'a self,
            _lease: &'a RunLease,
            _source_seq: u64,
            _packet: &'a ContextPacket,
            _provider_request: Map<String, serde_json::Value>,
            _budget: BudgetReservation,
        ) -> impl Future<Output = Result<FrozenStepRef, HarnessError>> + Send + 'a {
            std::future::ready(unsupported())
        }

        fn admit_invocation<'a>(
            &'a self,
            _lease: &'a RunLease,
            _proposal: &'a InvocationProposal,
            _grant: &'a InvocationGrant,
            _expected_workspace: &'a ContentHash,
        ) -> impl Future<Output = Result<IntentRef, HarnessError>> + Send + 'a {
            std::future::ready(unsupported())
        }

        fn settle_invocation<'a>(
            &'a self,
            _lease: &'a RunLease,
            _intent: &'a IntentRef,
            _receipt: InvocationReceiptRef,
            _projection_change: DomainChange,
        ) -> impl Future<Output = Result<CommitRef, HarnessError>> + Send + 'a {
            std::future::ready(unsupported())
        }

        fn settle_child<'a>(
            &'a self,
            _lease: &'a RunLease,
            _child_result: ChildResultRef,
            _parent_delivery: DeliveryRef,
        ) -> impl Future<Output = Result<CommitRef, HarnessError>> + Send + 'a {
            std::future::ready(unsupported())
        }

        fn recover_readonly<'a>(
            &'a self,
            _session_id: &'a SessionId,
        ) -> impl Future<Output = Result<PortRecoveryView, HarnessError>> + Send + 'a {
            std::future::ready(unsupported())
        }
    }

    #[test]
    fn the_declared_shape_is_implementable_and_never_returns_fake_success() {
        let store = UnsupportedStore;
        let session_id = SessionId::generate();
        let error = futures_lite_block_on(store.recover_readonly(&session_id))
            .expect_err("no implementation exists before M1");
        assert_eq!(error.code(), ErrorCode::ServiceUnavailable);
    }

    /// Minimal executor for the single test above, so the crate keeps no async
    /// runtime dependency for one future.
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
