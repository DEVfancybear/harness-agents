//! The daemon surface: durable schedules, the occurrences they produce, and the
//! process that runs them without a terminal attached.
//!
//! The daemon is **not** a second runtime. It composes the same services the
//! interactive app composes, over the same store and the same writer fence; what
//! it adds is a lifetime that outlives a client, and a clock that decides when
//! work starts. That is the whole difference, and it is why nothing here opens
//! its own database authority.

pub mod external;
pub mod host;
pub mod schedule;

pub use external::{
    AttachedRemotes, CancelReceipt, ExternalTaskRunner, NewExternalJob, PollReport, SubmitReceipt,
    TaskRemoteResolver,
};

pub use host::{
    CONTROL_READ_TIMEOUT, DaemonEndpoint, DaemonHost, DaemonRunReport, DaemonRunner, DaemonStatus,
    ENDPOINT_FILE, LaunchedOccurrence, MAX_CONTROL_BYTES, MAX_CONTROL_REQUESTS_PER_CONNECTION,
    StartRefusal, control, process_is_alive, read_endpoint, remove_endpoint, start, write_endpoint,
};

pub use schedule::{
    Clock, DueDecision, FixedClock, LaunchGrants, MAX_CATCH_UP_OCCURRENCES, MisfirePolicy,
    Occurrence, OccurrenceState, Schedule, ScheduleSpec, ScheduleState, ScheduleZone, SystemClock,
    due_now, local_date, next_after, occurrence_key,
};

/// A typed refusal from the daemon surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduleError {
    code: &'static str,
    message: String,
}

impl ScheduleError {
    #[must_use]
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for ScheduleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ScheduleError {}

impl From<ScheduleError> for harness_types::HarnessError {
    fn from(error: ScheduleError) -> Self {
        // The schedule surface speaks the same stable code vocabulary as the
        // rest of the host, so a caller never has to translate between two
        // error languages.
        let code = match error.code() {
            "policy_denied" => harness_types::ErrorCode::PolicyDenied,
            "sequence_conflict" => harness_types::ErrorCode::SequenceConflict,
            "task_not_found" => harness_types::ErrorCode::TaskNotFound,
            _ => harness_types::ErrorCode::InvalidPayload,
        };
        harness_types::HarnessError::new(code, error.message().to_owned())
    }
}
