//! Durable schedules and the occurrences they produce.
//!
//! The whole point of this module is that **a wall-clock timestamp is not an
//! idempotency key**. Two different things can happen at the same instant, and
//! the same logical occurrence can be evaluated at two different instants; a key
//! made of the time would confuse both. An occurrence is therefore identified by
//! its schedule, its revision and its *nominal* due time, and the row is written
//! **before** anything is launched. A daemon that dies after the write finds the
//! occurrence already claimed and does not launch it twice; a daemon that dies
//! before the write finds nothing claimed and computes the same key again.
//!
//! Time is injected. `Utc::now()` appears in exactly one place, and every
//! decision is made from the instant a caller passes in, so a test can advance
//! through a daylight-saving transition without waiting for one.

use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Timelike, Utc};
use serde::{Deserialize, Serialize};

use crate::daemon::ScheduleError;

/// How a schedule decides when it is next due.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleSpec {
    /// One occurrence at a fixed instant, then nothing.
    Once { at_unix_ms: i64 },
    /// Every `every_ms` milliseconds, counted from `anchor_unix_ms`.
    ///
    /// The anchor, not the last run, is what makes the sequence stable: an
    /// interval that drifted with each late run would slowly walk away from the
    /// schedule the user asked for.
    Interval { every_ms: i64, anchor_unix_ms: i64 },
    /// A five-field cron expression in one named zone.
    ///
    /// The fields are `minute hour day-of-month month day-of-week`, with `*`,
    /// lists, ranges and steps. The expression is evaluated in the zone's local
    /// time, which is the only way a spring-forward hour can be skipped and a
    /// fall-back hour can avoid running twice.
    Cron {
        expression: String,
        timezone: String,
    },
}

/// What a schedule is called.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleState {
    Active,
    Paused,
    Deleted,
}

impl ScheduleState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Deleted => "deleted",
        }
    }

    /// # Errors
    /// Fails when the stored state is not one this host wrote.
    pub fn parse(value: &str) -> Result<Self, ScheduleError> {
        match value {
            "active" => Ok(Self::Active),
            "paused" => Ok(Self::Paused),
            "deleted" => Ok(Self::Deleted),
            _ => Err(ScheduleError::new(
                "invalid_payload",
                "stored schedule state is unsupported",
            )),
        }
    }
}

/// One durable schedule.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    pub schedule_id: String,
    pub title: String,
    pub spec: ScheduleSpec,
    pub state: ScheduleState,
    /// Bumped by every change. A queued occurrence carries the revision it was
    /// claimed at, and dispatch re-reads this before launching: a pause that
    /// landed while the occurrence waited is honoured.
    pub revision: u64,
    /// The next nominal instant, as computed by the evaluator. Never derived
    /// from "now", so a late run cannot move the schedule.
    pub next_due_unix_ms: i64,
    /// The authority a launch inherits. Never wider than the source.
    pub grants: LaunchGrants,
}

/// What a scheduled launch is allowed to do.
///
/// A schedule captures the authority of the user who created it, and never
/// more. It carries no auto-approval: a scheduled run that needs a human
/// decision waits for one, exactly as an interactive run would.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchGrants {
    pub principal_id: String,
    pub project_id: Option<String>,
    pub task_id: Option<String>,
    /// Whether a launch may edit the workspace. A read-only schedule stays
    /// read-only however often it runs.
    pub edit_workspace: bool,
    pub budget_tokens: u64,
    /// Always false in this release, and stored so a reader can see that it is a
    /// decision rather than an omission.
    pub auto_approve_tools: bool,
}

/// How many missed occurrences one catch-up will run.
///
/// A daemon that was down for a week must not launch a week of work in one
/// burst: the backlog is reported, and the schedule resumes from now. One is the
/// bound because the plan forbids "lateness increases catch-up without bound".
pub const MAX_CATCH_UP_OCCURRENCES: usize = 1;

/// What to do about occurrences that came due while the host was not running.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MisfirePolicy {
    /// Run at most [`MAX_CATCH_UP_OCCURRENCES`], then resume from now.
    #[default]
    CatchUpBounded,
    /// Skip everything that came due and resume from now.
    Skip,
}

/// An occurrence that has been claimed and is durable.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Occurrence {
    /// `(schedule_id, revision, nominal due)` — never a wall-clock stamp.
    pub occurrence_key: String,
    pub schedule_id: String,
    pub revision: u64,
    pub due_unix_ms: i64,
    /// When the host actually claimed it. Recorded, and never part of the key.
    pub claimed_at_unix_ms: i64,
    pub state: OccurrenceState,
    /// `scheduled` or `manual`; a manual trigger never moves the next due.
    pub trigger: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OccurrenceState {
    Claimed,
    /// Held for a human decision: claimed, never launched, and durable across a
    /// restart. A scheduled launch that would edit a workspace waits here.
    Waiting,
    Launched,
    Skipped,
    /// A pause or delete landed between the claim and the launch.
    Canceled,
    /// A waiting occurrence whose approval window closed with no answer.
    Expired,
}

impl OccurrenceState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claimed => "claimed",
            Self::Waiting => "waiting",
            Self::Launched => "launched",
            Self::Skipped => "skipped",
            Self::Canceled => "canceled",
            Self::Expired => "expired",
        }
    }

    /// # Errors
    /// Fails when the stored state is not one this host wrote.
    pub fn parse(value: &str) -> Result<Self, ScheduleError> {
        match value {
            "claimed" => Ok(Self::Claimed),
            "waiting" => Ok(Self::Waiting),
            "launched" => Ok(Self::Launched),
            "skipped" => Ok(Self::Skipped),
            "canceled" => Ok(Self::Canceled),
            "expired" => Ok(Self::Expired),
            _ => Err(ScheduleError::new(
                "invalid_payload",
                "stored occurrence state is unsupported",
            )),
        }
    }
}

/// The key of one occurrence.
///
/// Built from the schedule, the revision and the nominal due time. Two hosts
/// evaluating the same schedule for the same instant produce the same string, so
/// the durable primary key is what stops a double launch - not a lock, and not
/// an in-process set.
#[must_use]
pub fn occurrence_key(schedule_id: &str, revision: u64, due_unix_ms: i64) -> String {
    format!("occ_{schedule_id}_{revision}_{due_unix_ms}")
}

/// What the evaluator decided.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DueDecision {
    /// Occurrences to claim now, oldest first, at most the catch-up bound.
    pub due: Vec<i64>,
    /// How many came due and were not claimed because of the bound.
    pub skipped: usize,
    /// The next nominal instant after this decision.
    pub next_due_unix_ms: i64,
}

/// The clock the evaluator reads.
///
/// Injected so a test can advance through a daylight-saving transition in
/// microseconds, and so the one place the process reads the wall clock is
/// visible.
pub trait Clock: Send + Sync {
    fn now_unix_ms(&self) -> i64;
}

/// The system clock: the only reader of `Utc::now()` in this module.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix_ms(&self) -> i64 {
        Utc::now().timestamp_millis()
    }
}

/// A clock a test drives.
#[derive(Debug)]
pub struct FixedClock {
    now: std::sync::atomic::AtomicI64,
}

impl FixedClock {
    #[must_use]
    pub fn new(at_unix_ms: i64) -> Self {
        Self {
            now: std::sync::atomic::AtomicI64::new(at_unix_ms),
        }
    }

    pub fn set(&self, at_unix_ms: i64) {
        self.now
            .store(at_unix_ms, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn advance(&self, by_ms: i64) {
        self.now
            .fetch_add(by_ms, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Clock for FixedClock {
    fn now_unix_ms(&self) -> i64 {
        self.now.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// A zone this host knows, with its transitions written out.
///
/// A full IANA database is a dependency tree this milestone does not need
/// (`ADR-N10` D1's rule, applied to M11): the behaviour a schedule has to get
/// right is what happens at a transition, and that is decided by the transition
/// instants rather than by the size of the database. The transitions below are
/// the real ones for central European time in 2026 and 2027, and a zone the host
/// does not know is a typed refusal rather than a silent UTC fallback - a
/// schedule silently evaluated in the wrong zone is worse than one that refuses
/// to run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScheduleZone {
    name: &'static str,
    standard_offset_seconds: i32,
    daylight_offset_seconds: i32,
    /// `(utc instant of the change, offset in effect from then on)`.
    transitions: &'static [(i64, i32)],
}

/// Central European time, whose transitions are the ones the acceptance case
/// exercises.
const CET_TRANSITIONS: &[(i64, i32)] = &[
    // 2026-03-29 01:00 UTC: 02:00 local becomes 03:00.
    (1_774_746_000_000, 7_200),
    // 2026-10-25 01:00 UTC: 03:00 local becomes 02:00.
    (1_792_890_000_000, 3_600),
    // 2027-03-28 01:00 UTC.
    (1_806_195_600_000, 7_200),
    // 2027-10-31 01:00 UTC.
    (1_824_944_400_000, 3_600),
];

impl ScheduleZone {
    /// Resolve a zone name.
    ///
    /// # Errors
    /// Fails for a zone this host does not know.
    pub fn parse(name: &str) -> Result<Self, ScheduleError> {
        match name {
            "UTC" => Ok(Self {
                name: "UTC",
                standard_offset_seconds: 0,
                daylight_offset_seconds: 0,
                transitions: &[],
            }),
            "Europe/Berlin" | "Europe/Paris" | "Europe/Madrid" | "Europe/Rome" => Ok(Self {
                name: "Europe/Berlin",
                standard_offset_seconds: 3_600,
                daylight_offset_seconds: 7_200,
                transitions: CET_TRANSITIONS,
            }),
            other => Err(ScheduleError::new(
                "invalid_payload",
                format!(
                    "this host does not know the zone `{other}`; \
                     a schedule is never evaluated in a guessed zone"
                ),
            )),
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// The offset in effect at one instant.
    #[must_use]
    pub fn offset_seconds_at(self, unix_ms: i64) -> i32 {
        let mut offset = self.standard_offset_seconds;
        for (instant, next) in self.transitions {
            if unix_ms >= *instant {
                offset = *next;
            }
        }
        offset
    }

    /// The local wall clock at one instant, as `(year, month, day, hour, minute)`.
    #[must_use]
    pub fn local_parts(self, unix_ms: i64) -> (i32, u32, u32, u32, u32) {
        let offset = self.offset_seconds_at(unix_ms);
        let local_ms = unix_ms.saturating_add(i64::from(offset) * 1_000);
        let instant = Utc
            .timestamp_millis_opt(local_ms)
            .single()
            .unwrap_or_else(Utc::now);
        (
            instant.year(),
            instant.month(),
            instant.day(),
            instant.hour(),
            instant.minute(),
        )
    }

    /// The first instant whose local minute is one minute after this one.
    ///
    /// This is the step the cron walk takes. It is a local step on purpose: a
    /// UTC step would revisit the same local minute after a fall-back, and a
    /// schedule that fires twice in one repeated hour is a double launch caused
    /// by the clock rather than by the user.
    #[must_use]
    pub fn local_minutes_after(self, unix_ms: i64, minutes: i64) -> i64 {
        let offset = i64::from(self.offset_seconds_at(unix_ms));
        let local_minute = (unix_ms + offset * 1_000).div_euclid(60_000) + minutes;
        // The offset that applies is the one in effect at the candidate, so the
        // conversion is a fixed point rather than a subtraction: two passes are
        // enough because a transition moves the offset by at most an hour, and
        // the loop stops as soon as the answer stops changing.
        let mut candidate = local_minute * 60_000 - offset * 1_000;
        for _ in 0..4 {
            let next = i64::from(self.offset_seconds_at(candidate));
            let refined = local_minute * 60_000 - next * 1_000;
            if refined == candidate {
                break;
            }
            candidate = refined;
        }
        candidate
    }

    /// The weekday of one instant's local date, Sunday = 0.
    #[must_use]
    pub fn local_weekday(self, unix_ms: i64) -> u32 {
        let offset = self.offset_seconds_at(unix_ms);
        let local_ms = unix_ms.saturating_add(i64::from(offset) * 1_000);
        Utc.timestamp_millis_opt(local_ms)
            .single()
            .map_or(0, |instant| instant.weekday().num_days_from_sunday())
    }
}

/// Compute the first instant strictly after `after_unix_ms` that the spec names.
///
/// # Errors
/// Fails on an unparsable cron expression or an unknown zone, rather than
/// silently treating it as "never".
pub fn next_after(spec: &ScheduleSpec, after_unix_ms: i64) -> Result<i64, ScheduleError> {
    match spec {
        ScheduleSpec::Once { at_unix_ms } => {
            if *at_unix_ms > after_unix_ms {
                Ok(*at_unix_ms)
            } else {
                // A `once` schedule that is past has no next; `i64::MAX` is the
                // "never" a caller checks for.
                Ok(i64::MAX)
            }
        }
        ScheduleSpec::Interval {
            every_ms,
            anchor_unix_ms,
        } => {
            if *every_ms <= 0 {
                return Err(ScheduleError::new(
                    "invalid_payload",
                    "an interval schedule needs a positive period",
                ));
            }
            let elapsed = after_unix_ms.saturating_sub(*anchor_unix_ms);
            let steps = elapsed.div_euclid(*every_ms).saturating_add(1);
            Ok(anchor_unix_ms.saturating_add(steps.saturating_mul(*every_ms)))
        }
        ScheduleSpec::Cron {
            expression,
            timezone,
        } => {
            let fields = CronFields::parse(expression)?;
            let zone = ScheduleZone::parse(timezone)?;
            // Walk forward a minute at a time. Two transitions matter and they
            // pull in opposite directions:
            //
            // * a spring-forward gap has no matching local time, so the
            //   occurrence is skipped rather than moved to an hour the user did
            //   not ask for;
            // * a fall-back repeat has **two** instants with the same local time,
            //   and only the first may run - a schedule that fired twice in one
            //   repeated hour would be a double launch caused by the clock.
            //
            // The second rule needs the local time to be monotonic, which is why
            // the walk compares local minutes rather than counting UTC ones.
            // Advance on the **local** clock, not the UTC one. Adding a UTC
            // minute during a fall-back lands on the same local minute again,
            // which is how a repeated hour gets served twice; advancing the
            // local reading and converting back makes the repeated hour
            // unreachable, because 02:59 local already happened.
            let mut minutes = 1_i64;
            for _ in 0..=(366 * 24 * 60 * 2) {
                // Each candidate is computed from the *original* instant plus a
                // local minute count, never from the previous candidate: feeding
                // one result into the next would compound the offset at every
                // transition instead of applying it once.
                let candidate = zone.local_minutes_after(after_unix_ms, minutes);
                if fields.matches(zone, candidate) {
                    return Ok(candidate);
                }
                minutes = minutes.saturating_add(1);
            }
            Ok(i64::MAX)
        }
    }
}

/// Decide what is due, honouring the misfire policy.
///
/// # Errors
/// Fails when the spec cannot be evaluated.
pub fn due_now(
    schedule: &Schedule,
    now_unix_ms: i64,
    policy: MisfirePolicy,
) -> Result<DueDecision, ScheduleError> {
    let mut due = Vec::new();
    let mut cursor = schedule.next_due_unix_ms;
    // Walk the instants that came due, bounded: a host that was down for a month
    // must not enumerate a month of minutes before deciding.
    for _ in 0..=MAX_CATCH_UP_OCCURRENCES {
        if cursor == i64::MAX || cursor > now_unix_ms {
            break;
        }
        due.push(cursor);
        cursor = next_after(&schedule.spec, cursor)?;
    }
    let next_due = if cursor == i64::MAX {
        i64::MAX
    } else if cursor > now_unix_ms {
        cursor
    } else {
        // Still behind after the bound: resume from now rather than queueing the
        // whole backlog.
        next_after(&schedule.spec, now_unix_ms)?
    };
    let skipped = match policy {
        MisfirePolicy::CatchUpBounded => due.len().saturating_sub(MAX_CATCH_UP_OCCURRENCES),
        MisfirePolicy::Skip => {
            let count = due.len();
            due.clear();
            count
        }
    };
    due.truncate(MAX_CATCH_UP_OCCURRENCES);
    Ok(DueDecision {
        due,
        skipped,
        next_due_unix_ms: next_due,
    })
}

/// A parsed five-field cron expression.
#[derive(Clone, Debug)]
struct CronFields {
    minute: CronField,
    hour: CronField,
    day_of_month: CronField,
    month: CronField,
    day_of_week: CronField,
}

#[derive(Clone, Debug)]
struct CronField {
    any: bool,
    values: Vec<u32>,
}

impl CronField {
    fn matches(&self, value: u32) -> bool {
        self.any || self.values.contains(&value)
    }
}

impl CronFields {
    fn parse(expression: &str) -> Result<Self, ScheduleError> {
        let parts = expression.split_whitespace().collect::<Vec<_>>();
        if parts.len() != 5 {
            return Err(ScheduleError::new(
                "invalid_payload",
                "a cron expression needs five fields: minute hour day-of-month month day-of-week",
            ));
        }
        Ok(Self {
            minute: CronField::parse(parts[0], 0, 59)?,
            hour: CronField::parse(parts[1], 0, 23)?,
            day_of_month: CronField::parse(parts[2], 1, 31)?,
            month: CronField::parse(parts[3], 1, 12)?,
            day_of_week: CronField::parse(parts[4], 0, 7)?,
        })
    }

    fn matches(&self, zone: ScheduleZone, unix_ms: i64) -> bool {
        let (_, month, day, hour, minute) = zone.local_parts(unix_ms);
        let weekday = zone.local_weekday(unix_ms);
        let dow = self.day_of_week.any
            || self.day_of_week.matches(weekday)
            || (weekday == 0 && self.day_of_week.matches(7));
        self.minute.matches(minute)
            && self.hour.matches(hour)
            && self.day_of_month.matches(day)
            && self.month.matches(month)
            && dow
    }
}

impl CronField {
    fn parse(text: &str, min: u32, max: u32) -> Result<Self, ScheduleError> {
        if text == "*" {
            return Ok(Self {
                any: true,
                values: Vec::new(),
            });
        }
        let mut values = Vec::new();
        for part in text.split(',') {
            let (range, step) = match part.split_once('/') {
                Some((range, step)) => (
                    range,
                    step.parse::<u32>().map_err(|_| {
                        ScheduleError::new("invalid_payload", "a cron step is not a number")
                    })?,
                ),
                None => (part, 1),
            };
            if step == 0 {
                return Err(ScheduleError::new(
                    "invalid_payload",
                    "a cron step of zero never matches",
                ));
            }
            let (start, end) = if range == "*" {
                (min, max)
            } else if let Some((start, end)) = range.split_once('-') {
                (
                    start.parse::<u32>().map_err(|_| {
                        ScheduleError::new("invalid_payload", "a cron range start is not a number")
                    })?,
                    end.parse::<u32>().map_err(|_| {
                        ScheduleError::new("invalid_payload", "a cron range end is not a number")
                    })?,
                )
            } else {
                let value = range.parse::<u32>().map_err(|_| {
                    ScheduleError::new("invalid_payload", "a cron value is not a number")
                })?;
                (value, value)
            };
            if start < min || end > max || start > end {
                return Err(ScheduleError::new(
                    "invalid_payload",
                    format!("a cron field is outside {min}..={max}"),
                ));
            }
            let mut value = start;
            while value <= end {
                values.push(value);
                value = value.saturating_add(step);
            }
        }
        values.sort_unstable();
        values.dedup();
        if values.is_empty() {
            return Err(ScheduleError::new(
                "invalid_payload",
                "a cron field matches nothing",
            ));
        }
        Ok(Self { any: false, values })
    }
}

/// The local date a nominal instant falls on in one zone, for a report.
///
/// # Errors
/// Fails on an unknown zone.
pub fn local_date(unix_ms: i64, timezone: &str) -> Result<NaiveDate, ScheduleError> {
    let zone = ScheduleZone::parse(timezone)?;
    let offset = zone.offset_seconds_at(unix_ms);
    let local_ms = unix_ms.saturating_add(i64::from(offset) * 1_000);
    let instant: DateTime<Utc> = Utc
        .timestamp_millis_opt(local_ms)
        .single()
        .ok_or_else(|| ScheduleError::new("invalid_payload", "the instant is out of range"))?;
    Ok(instant.date_naive())
}
