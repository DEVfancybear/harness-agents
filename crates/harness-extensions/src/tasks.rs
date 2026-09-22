//! Protocol-neutral external task submission, polling and cancellation.
//!
//! A remote service that accepts work and answers later is the one place where
//! "the request failed" and "the request succeeded and the answer was lost" look
//! identical from the host. This module is the vocabulary that keeps them apart:
//!
//! - [`TaskSubmission::Accepted`] carries a handle the remote minted, so the
//!   outcome is *knowable* later and only polling is owed.
//! - [`SubmitFailure::Ambiguous`] says the request may have been applied. It is
//!   never retried automatically, because a second mutation is worse than an
//!   unknown one.
//! - [`SubmitFailure::Definite`] says the remote answered and refused, so
//!   nothing was applied.
//!
//! Nothing here decides *authority*: a caller reaches a remote only through the
//! tool gate that already exists, exactly like every other external call in this
//! crate. What this module adds is the lifecycle, and the arithmetic that keeps a
//! poll bounded instead of a spin loop.
//!
//! The MCP mapping is pinned, not assumed: [`MCP_TASKS_EXTENSION_ID`] is the
//! extension identifier the pinned SDK (`rmcp` 3.4.0, protocol revision
//! 2026-07-28) uses for SEP-2663 Tasks, and [`RemoteTaskState`] is exactly the
//! lifecycle that specification defines. See [`crate::McpFeature::Tasks`] for
//! what this build claims about it.

use std::{future::Future, pin::Pin};

use serde_json::Value;

use crate::ExtensionError;

/// The schema version of an external-task record written by this build.
pub const EXTERNAL_TASK_SCHEMA_VERSION: u32 = 1;

/// The MCP Tasks extension identifier (SEP-2663), as the pinned SDK spells it.
pub const MCP_TASKS_EXTENSION_ID: &str = "io.modelcontextprotocol/tasks";

/// The shortest poll delay this host will use, whatever a remote suggests.
pub const MIN_POLL_INTERVAL_MS: u64 = 100;

/// The poll delay used when a remote suggests none.
pub const DEFAULT_POLL_INTERVAL_MS: u64 = 1_000;

/// The longest poll delay this host will use, whatever a remote suggests.
pub const MAX_POLL_INTERVAL_MS: u64 = 60_000;

/// How long a task may stay unresolved before the host stops polling it.
pub const DEFAULT_TASK_DEADLINE_MS: u64 = 15 * 60 * 1000;

/// Each consecutive unresolved poll multiplies the delay by this factor.
pub const POLL_BACKOFF_FACTOR: u32 = 2;

/// A remote task's lifecycle state, named as SEP-2663 names it.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RemoteTaskState {
    Working,
    InputRequired,
    Completed,
    Failed,
    Cancelled,
}

impl RemoteTaskState {
    /// Every state, for a report that must be exhaustive.
    pub const ALL: [Self; 5] = [
        Self::Working,
        Self::InputRequired,
        Self::Completed,
        Self::Failed,
        Self::Cancelled,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::InputRequired => "input_required",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Whether this state ends the task. `input_required` does **not**: the
    /// remote is waiting for an answer, and the task is still live.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// Parse the wire name, refusing anything this build does not know.
    ///
    /// An unknown status is refused rather than folded into `working`: a host
    /// that guessed would keep polling a task that may already be over, or
    /// settle one that is not.
    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.as_str() == value)
    }
}

/// What one `status` call observed.
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteTaskSnapshot {
    pub remote_task_id: String,
    pub state: RemoteTaskState,
    pub status_message: Option<String>,
    /// The remote's suggested poll interval, if it offered one.
    pub poll_interval_ms: Option<u64>,
    /// The final result, present exactly when the state is `completed`.
    pub result: Option<Value>,
    /// The remote's error object, present when the state is `failed`.
    pub error: Option<Value>,
}

impl RemoteTaskSnapshot {
    #[must_use]
    pub fn new(remote_task_id: impl Into<String>, state: RemoteTaskState) -> Self {
        Self {
            remote_task_id: remote_task_id.into(),
            state,
            status_message: None,
            poll_interval_ms: None,
            result: None,
            error: None,
        }
    }
}

/// What a submit call produced.
#[derive(Clone, Debug, PartialEq)]
pub enum TaskSubmission {
    /// The remote accepted the work and minted a handle. Only polling is owed.
    Accepted {
        remote_task_id: String,
        state: RemoteTaskState,
        poll_interval_ms: Option<u64>,
    },
    /// The remote finished inside the submit call, so there is nothing to poll.
    Completed { result: Value },
}

/// Why a submit did not produce a handle, and whether it may be retried.
///
/// The split is **not** "the remote answered" versus "the remote went quiet". A
/// remote can apply the work and *then* answer with an error - the pinned MCP
/// SDK does exactly that when a server materialises a task for a client that did
/// not declare the tasks extension - so an error reply is not evidence that
/// nothing happened. The only definite failure is the one that happened **before
/// the request was written**: an operation this transport does not serve,
/// arguments the host itself refused, a server with no transport attached.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SubmitFailure {
    /// The request provably never left the host, so nothing was applied and a
    /// fresh attempt is a decision an operator may make.
    Definite(String),
    /// The request was written and no handle came back, so it may have been
    /// applied. A retry here is a second mutation, so this host never makes one:
    /// the job waits for a handle to be reconciled against it.
    Ambiguous(String),
}

impl SubmitFailure {
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Definite(message) | Self::Ambiguous(message) => message,
        }
    }

    #[must_use]
    pub const fn is_ambiguous(&self) -> bool {
        matches!(self, Self::Ambiguous(_))
    }
}

/// A boxed future, so [`TaskRemote`] stays object-safe: the daemon holds a
/// resolver of transports, not one concrete transport.
pub type RemoteFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What the host needs from any remote that runs work asynchronously.
///
/// The trait is deliberately three calls wide. A protocol that can express
/// submit/status/cancel can be driven by this host; a protocol that cannot is
/// driven by a caller that does not pretend otherwise.
pub trait TaskRemote: Send + Sync {
    /// The server this transport speaks to, as the store records it.
    fn server_id(&self) -> &str;

    /// Submit one operation and report what the remote did with it.
    fn submit<'a>(
        &'a self,
        operation: &'a str,
        request: &'a Value,
    ) -> RemoteFuture<'a, Result<TaskSubmission, SubmitFailure>>;

    /// Read one task's current state.
    fn status<'a>(
        &'a self,
        remote_task_id: &'a str,
    ) -> RemoteFuture<'a, Result<RemoteTaskSnapshot, ExtensionError>>;

    /// Ask the remote to cancel. Cancellation is cooperative: this call reports
    /// that the request was delivered and acknowledged, never that the effect
    /// was rolled back.
    fn cancel<'a>(
        &'a self,
        remote_task_id: &'a str,
    ) -> RemoteFuture<'a, Result<(), ExtensionError>>;
}

/// What one poll decided to do, with the clock and the deadline already
/// accounted for. Kept pure so the arithmetic is testable without a remote.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PollDecision {
    /// The task reached a terminal state: settle it with this outcome.
    Settle { state: RemoteTaskState },
    /// The task is still running: poll again after `delay_ms`.
    Continue { delay_ms: i64 },
    /// The deadline passed with the task unresolved. The host stops polling and
    /// records that the outcome is unknown - it does not claim the work never
    /// happened.
    DeadlineExceeded { last: RemoteTaskState },
}

/// Decide what one observation means.
///
/// `attempt` is how many unresolved polls this task has already cost, so the
/// delay grows with the number of times the remote said "not yet" instead of
/// hammering it at the interval it suggested.
#[must_use]
pub fn decide_poll(
    observed: &RemoteTaskSnapshot,
    attempt: u32,
    now_unix_ms: i64,
    deadline_unix_ms: i64,
) -> PollDecision {
    if observed.state.is_terminal() {
        return PollDecision::Settle {
            state: observed.state,
        };
    }
    let remaining = deadline_unix_ms.saturating_sub(now_unix_ms);
    if remaining <= 0 {
        return PollDecision::DeadlineExceeded {
            last: observed.state,
        };
    }
    PollDecision::Continue {
        delay_ms: next_poll_delay_ms(attempt, observed.poll_interval_ms, remaining),
    }
}

/// The delay before the next poll, bounded on both sides.
///
/// The remote's suggestion is honoured but clamped: a remote that asks to be
/// polled every millisecond does not get to turn this host into a spin loop, and
/// one that asks for an hour does not get to hide a finished task behind a
/// sleep. The delay never exceeds the time left before the deadline.
#[must_use]
pub fn next_poll_delay_ms(attempt: u32, suggested_ms: Option<u64>, remaining_ms: i64) -> i64 {
    let base = suggested_ms
        .unwrap_or(DEFAULT_POLL_INTERVAL_MS)
        .clamp(MIN_POLL_INTERVAL_MS, MAX_POLL_INTERVAL_MS);
    let mut delay = base;
    for _ in 0..attempt.min(16) {
        delay = delay.saturating_mul(u64::from(POLL_BACKOFF_FACTOR));
        if delay >= MAX_POLL_INTERVAL_MS {
            break;
        }
    }
    let capped = delay.min(MAX_POLL_INTERVAL_MS);
    let remaining = u64::try_from(remaining_ms)
        .unwrap_or(0)
        .max(MIN_POLL_INTERVAL_MS);
    i64::try_from(capped.min(remaining)).unwrap_or(i64::MAX)
}

/// Whether a submit failure leaves the request's effect unknowable.
///
/// The rule is the conservative one: only an answer from the remote counts as
/// definite. A timeout, a closed pipe or a decode failure all mean the request
/// may have landed, and the host must not send it again on its own.
#[must_use]
pub fn submit_failure_is_ambiguous(failure: &SubmitFailure) -> bool {
    failure.is_ambiguous()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn terminal_states_are_the_three_the_spec_names() {
        assert!(!RemoteTaskState::Working.is_terminal());
        assert!(
            !RemoteTaskState::InputRequired.is_terminal(),
            "a task waiting for input is live, not over"
        );
        assert!(RemoteTaskState::Completed.is_terminal());
        assert!(RemoteTaskState::Failed.is_terminal());
        assert!(RemoteTaskState::Cancelled.is_terminal());
        assert_eq!(
            RemoteTaskState::ALL
                .into_iter()
                .filter(|state| state.is_terminal())
                .count(),
            3
        );
    }

    #[test]
    fn an_unknown_wire_status_is_refused_rather_than_guessed() {
        assert_eq!(
            RemoteTaskState::from_wire("input_required"),
            Some(RemoteTaskState::InputRequired)
        );
        assert_eq!(RemoteTaskState::from_wire("queued"), None);
        assert_eq!(RemoteTaskState::from_wire("WORKING"), None);
    }

    #[test]
    fn a_terminal_observation_settles_and_a_live_one_reschedules() {
        let working = RemoteTaskSnapshot::new("t-1", RemoteTaskState::Working);
        assert_eq!(
            decide_poll(&working, 0, 1_000, 10_000),
            PollDecision::Continue { delay_ms: 1_000 }
        );
        let completed = RemoteTaskSnapshot::new("t-1", RemoteTaskState::Completed);
        assert_eq!(
            decide_poll(&completed, 3, 1_000, 10_000),
            PollDecision::Settle {
                state: RemoteTaskState::Completed
            }
        );
    }

    #[test]
    fn the_deadline_is_a_decision_and_not_a_settlement() {
        let working = RemoteTaskSnapshot::new("t-1", RemoteTaskState::Working);
        assert_eq!(
            decide_poll(&working, 0, 10_000, 10_000),
            PollDecision::DeadlineExceeded {
                last: RemoteTaskState::Working
            }
        );
        assert!(
            !RemoteTaskState::Working.is_terminal(),
            "a task that ran out of deadline has an unknown outcome, not a cancelled one"
        );
    }

    #[test]
    fn the_delay_is_clamped_on_both_sides_and_grows_with_the_attempt() {
        assert_eq!(
            next_poll_delay_ms(0, Some(1), 60_000),
            i64::try_from(MIN_POLL_INTERVAL_MS).expect("fits")
        );
        assert_eq!(
            next_poll_delay_ms(0, Some(u64::MAX), 10 * 60_000),
            i64::try_from(MAX_POLL_INTERVAL_MS).expect("fits")
        );
        assert_eq!(next_poll_delay_ms(0, None, 60_000), 1_000);
        assert_eq!(next_poll_delay_ms(1, None, 60_000), 2_000);
        assert_eq!(next_poll_delay_ms(2, None, 60_000), 4_000);
        assert_eq!(
            next_poll_delay_ms(20, None, 60_000),
            i64::try_from(MAX_POLL_INTERVAL_MS).expect("fits"),
            "backoff saturates at the ceiling"
        );
        assert_eq!(
            next_poll_delay_ms(4, None, 1_500),
            1_500,
            "the delay never runs past the deadline"
        );
    }

    #[test]
    fn only_a_refusal_before_the_send_is_definite() {
        assert!(!submit_failure_is_ambiguous(&SubmitFailure::Definite(
            "the transport does not serve this operation".to_owned()
        )));
        assert!(submit_failure_is_ambiguous(&SubmitFailure::Ambiguous(
            "the remote refused after the request was sent".to_owned()
        )));
        assert!(submit_failure_is_ambiguous(&SubmitFailure::Ambiguous(
            "the connection closed before an answer".to_owned()
        )));
        assert_eq!(
            SubmitFailure::Ambiguous("lost".to_owned()).message(),
            "lost"
        );
    }

    #[test]
    fn an_accepted_submission_carries_only_a_handle() {
        let accepted = TaskSubmission::Accepted {
            remote_task_id: "t-9".to_owned(),
            state: RemoteTaskState::Working,
            poll_interval_ms: Some(250),
        };
        let TaskSubmission::Accepted {
            remote_task_id,
            state,
            poll_interval_ms,
        } = accepted
        else {
            panic!("accepted is the variant under test");
        };
        assert_eq!(remote_task_id, "t-9");
        assert_eq!(state.as_str(), "working");
        assert_eq!(poll_interval_ms, Some(250));
        assert_eq!(
            TaskSubmission::Completed {
                result: json!({"ok": true})
            },
            TaskSubmission::Completed {
                result: json!({"ok": true})
            }
        );
    }
}
