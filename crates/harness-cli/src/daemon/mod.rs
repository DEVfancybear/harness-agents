//! The daemon surface: durable schedules, the occurrences they produce, and the
//! process that runs them without a terminal attached.
//!
//! The daemon is **not** a second runtime. It composes the same services the
//! interactive app composes, over the same store and the same writer fence; what
//! it adds is a lifetime that outlives a client, and a clock that decides when
//! work starts. That is the whole difference, and it is why nothing here opens
//! its own database authority.

pub mod schedule;

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
