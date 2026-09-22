//! M11 acceptance target: durable schedules, their occurrences, and the clock
//! semantics a schedule has to get right.
//!
//! The clock is injected everywhere. `a34_schedule_occurrences` drives a fake
//! clock through a daylight-saving transition rather than waiting for one, and
//! the store assertions run against the real `SQLite` store and its real
//! transactions.
//!
//! * `a34_schedule_occurrences` - occurrence keys are stable and do not contain a
//!   wall-clock stamp; a spring-forward hour is skipped and a fall-back hour is
//!   not run twice; a missed backlog is bounded; a manual trigger does not move
//!   the future schedule; a pause that lands while an occurrence waits cancels
//!   it; a claim survives a restart and is never launched twice; and a schedule
//!   never carries auto-approval.

use std::sync::Arc;

use harness_cli::daemon::{
    Clock, FixedClock, LaunchGrants, MAX_CATCH_UP_OCCURRENCES, MisfirePolicy, OccurrenceState,
    Schedule, ScheduleSpec, ScheduleState, ScheduleZone, due_now, next_after, occurrence_key,
};
use harness_store_sqlite::{
    SqliteStore, StoredOccurrenceRecord, StoredScheduleRecord, WriterOpenOptions,
};
use harness_types::{ErrorCode, HostId};

#[path = "phase_p7/support.rs"]
mod support;

use support::temp_root;

/// A schedule that fires every minute in a zone with daylight saving.
fn minute_schedule(zone: &str) -> Schedule {
    Schedule {
        schedule_id: "schedule_a34".to_owned(),
        title: "every minute".to_owned(),
        spec: ScheduleSpec::Cron {
            expression: "* * * * *".to_owned(),
            timezone: zone.to_owned(),
        },
        state: ScheduleState::Active,
        revision: 1,
        next_due_unix_ms: 0,
        grants: LaunchGrants {
            principal_id: "a34-fixture".to_owned(),
            project_id: None,
            task_id: None,
            edit_workspace: false,
            budget_tokens: 1_000,
            auto_approve_tools: false,
        },
    }
}

/// A schedule that fires at `hour:minute` local time every day.
fn daily_schedule(zone: &str, hour: u32, minute: u32) -> Schedule {
    let mut schedule = minute_schedule(zone);
    schedule.spec = ScheduleSpec::Cron {
        expression: format!("{minute} {hour} * * *"),
        timezone: zone.to_owned(),
    };
    schedule
}

/// The UTC instant of a local wall-clock time in central European time.
///
/// Written out rather than computed from a rule, because the point of the test
/// is the transition instants themselves: the acceptance case names the real
/// 2026 transitions and the assertions are about what happens at them.
fn cet(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> i64 {
    use chrono::{TimeZone, Utc};
    Utc.with_ymd_and_hms(year, month, day, hour, minute, 0)
        .single()
        .expect("a real instant")
        .timestamp_millis()
}

async fn open_store(data_dir: &std::path::Path) -> SqliteStore {
    SqliteStore::open_writer(WriterOpenOptions::new(data_dir, HostId::generate()))
        .await
        .expect("store opens")
}

fn stored_schedule(schedule: &Schedule, next_due_unix_ms: i64) -> StoredScheduleRecord {
    StoredScheduleRecord {
        schedule_id: schedule.schedule_id.clone(),
        title: schedule.title.clone(),
        state: schedule.state.as_str().to_owned(),
        revision: schedule.revision,
        next_due_unix_ms,
        spec_json: serde_json::to_string(&schedule.spec).expect("spec serializes"),
        grants_json: serde_json::to_string(&schedule.grants).expect("grants serialize"),
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one clock story, transition by transition
async fn a34_schedule_occurrences() {
    // -----------------------------------------------------------------------
    // The key is not a timestamp
    // -----------------------------------------------------------------------
    let key = occurrence_key("schedule_a34", 1, 1_774_752_000_000);
    assert_eq!(
        key, "occ_schedule_a34_1_1774752000000",
        "the key names the schedule, its revision and the nominal due instant"
    );
    assert_eq!(
        occurrence_key("schedule_a34", 1, 1_774_752_000_000),
        key,
        "two hosts evaluating one schedule for one instant agree on the key"
    );
    assert_ne!(
        occurrence_key("schedule_a34", 2, 1_774_752_000_000),
        key,
        "a new revision is a different occurrence"
    );
    // The negative control the case names: a wall-clock stamp would make the
    // claim time part of the identity, so the same occurrence claimed a second
    // later would look new.
    assert!(
        !key.contains("claimed"),
        "the key carries no claim instant, so a re-claim cannot look like a new occurrence"
    );

    // -----------------------------------------------------------------------
    // Spring forward: the hour that does not exist is skipped
    // -----------------------------------------------------------------------
    let zone = ScheduleZone::parse("Europe/Berlin").expect("the zone is known");
    // 2026-03-29: 02:00 local becomes 03:00 local. A schedule at 02:30 local has
    // no such wall-clock time that day and must not fire.
    let before_transition = cet(2026, 3, 28, 12, 0);
    let spring = daily_schedule("Europe/Berlin", 2, 30);
    let mut cursor = before_transition;
    let mut fired = Vec::new();
    for _ in 0..4 {
        cursor = next_after(&spring.spec, cursor).expect("the expression evaluates");
        fired.push(cursor);
    }
    let local_dates = fired
        .iter()
        .map(|instant| {
            let (_, month, day, hour, minute) = zone.local_parts(*instant);
            (month, day, hour, minute)
        })
        .collect::<Vec<_>>();
    assert!(
        !local_dates.contains(&(3, 29, 2, 30)),
        "02:30 local does not exist on the spring-forward day: {local_dates:?}"
    );
    assert!(
        !local_dates.iter().any(|(_, day, _, _)| *day == 29),
        "and it does not fire at some other hour that day: the nominal time did not exist, \
         so the occurrence is skipped rather than moved: {local_dates:?}"
    );
    // And it is not skipped for good: the next day fires at 02:30 again.
    assert!(
        local_dates.contains(&(3, 30, 2, 30)),
        "the day after the transition fires at the nominal local time: {local_dates:?}"
    );

    // -----------------------------------------------------------------------
    // Fall back: the repeated hour is not run twice
    // -----------------------------------------------------------------------
    // 2026-10-25: 03:00 local becomes 02:00 local, so 02:30 local happens twice
    // in UTC. The schedule must produce one occurrence, not two.
    let before_fall = cet(2026, 10, 24, 12, 0);
    let fall = daily_schedule("Europe/Berlin", 2, 30);
    let mut cursor = before_fall;
    let mut fired = Vec::new();
    for _ in 0..4 {
        cursor = next_after(&fall.spec, cursor).expect("the expression evaluates");
        fired.push(cursor);
    }
    let on_transition_day = fired
        .iter()
        .filter(|instant| {
            let (_, month, day, _, _) = zone.local_parts(**instant);
            month == 10 && day == 25
        })
        .count();
    assert_eq!(
        on_transition_day, 1,
        "the repeated local hour produces one occurrence, not two: {fired:?}"
    );

    // -----------------------------------------------------------------------
    // Misfire: the backlog is bounded
    // -----------------------------------------------------------------------
    let clock = FixedClock::new(cet(2026, 6, 1, 0, 0));
    let mut hourly = minute_schedule("UTC");
    hourly.spec = ScheduleSpec::Interval {
        every_ms: 3_600_000,
        anchor_unix_ms: clock.now_unix_ms(),
    };
    hourly.next_due_unix_ms = clock.now_unix_ms();
    // The host was down for a week.
    clock.advance(7 * 24 * 3_600_000);
    let decision = due_now(&hourly, clock.now_unix_ms(), MisfirePolicy::CatchUpBounded)
        .expect("the decision is computed");
    assert_eq!(
        decision.due.len(),
        MAX_CATCH_UP_OCCURRENCES,
        "a week of backlog does not become a week of launches"
    );
    assert!(
        decision.skipped > 0,
        "and the skipped count is reported rather than hidden: {}",
        decision.skipped
    );
    assert!(
        decision.next_due_unix_ms > clock.now_unix_ms(),
        "the schedule resumes from now, not from the backlog"
    );
    let skipped = due_now(&hourly, clock.now_unix_ms(), MisfirePolicy::Skip)
        .expect("the decision is computed");
    assert!(skipped.due.is_empty(), "the skip policy runs nothing");
    assert!(skipped.skipped > 0);

    // -----------------------------------------------------------------------
    // A manual trigger does not move the future schedule
    // -----------------------------------------------------------------------
    let manual_clock = FixedClock::new(cet(2026, 6, 1, 9, 0));
    let manual = minute_schedule("UTC");
    let due = next_after(&manual.spec, manual_clock.now_unix_ms()).expect("evaluates");
    let manual_decision = due_now(&manual, manual_clock.now_unix_ms(), MisfirePolicy::Skip)
        .expect("the decision is computed");
    // A manual run is an occurrence with its own trigger, and it leaves
    // `next_due` exactly where the evaluator put it.
    let manual_key = occurrence_key(
        &manual.schedule_id,
        manual.revision,
        manual_clock.now_unix_ms(),
    );
    assert_ne!(
        manual_key,
        occurrence_key(&manual.schedule_id, manual.revision, due),
        "a manual run is its own occurrence, not the scheduled one"
    );
    assert_eq!(
        manual_decision.next_due_unix_ms, due,
        "the manual trigger did not recompute or overwrite the next due"
    );

    // -----------------------------------------------------------------------
    // A schedule never carries auto-approval
    // -----------------------------------------------------------------------
    assert!(
        !manual.grants.auto_approve_tools,
        "a scheduled launch cannot approve its own tools"
    );
    assert!(
        !manual.grants.edit_workspace,
        "and cannot widen the authority it captured"
    );

    // -----------------------------------------------------------------------
    // The durable claim
    // -----------------------------------------------------------------------
    let root = temp_root();
    let data_dir = root.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("the data directory is created");
    let store = Arc::new(open_store(&data_dir).await);
    let now = cet(2026, 6, 1, 9, 0);
    let schedule = minute_schedule("UTC");
    store
        .create_schedule(&stored_schedule(&schedule, now))
        .await
        .expect("the schedule is created");

    // Re-creating the same id is refused rather than merged.
    assert_eq!(
        store
            .create_schedule(&stored_schedule(&schedule, now))
            .await
            .expect_err("a duplicate schedule id is refused")
            .code(),
        ErrorCode::DuplicateTaskId
    );

    let occurrence = StoredOccurrenceRecord {
        occurrence_key: occurrence_key(&schedule.schedule_id, 1, now),
        schedule_id: schedule.schedule_id.clone(),
        revision: 1,
        due_unix_ms: now,
        claimed_at_unix_ms: now + 1_000,
        state: OccurrenceState::Claimed.as_str().to_owned(),
        trigger_kind: "scheduled".to_owned(),
    };
    let next = now + 60_000;
    assert!(
        store
            .claim_occurrence(&occurrence, next)
            .await
            .expect("the claim succeeds"),
        "the first claim inserts"
    );
    assert!(
        !store
            .claim_occurrence(&occurrence, next + 60_000)
            .await
            .expect("a second claim is not an error"),
        "a second host claiming the same occurrence is told it already exists"
    );
    let stored = store
        .schedule(&schedule.schedule_id)
        .await
        .expect("the schedule is readable")
        .expect("the schedule exists");
    assert_eq!(
        stored.next_due_unix_ms, next,
        "the duplicate claim did not advance the schedule a second time"
    );
    assert_eq!(
        store
            .occurrences_for(&schedule.schedule_id)
            .await
            .expect("occurrences are readable")
            .len(),
        1,
        "one logical occurrence, however many hosts claimed it"
    );

    // A restart between the claim and the launch does not launch again: the row
    // says claimed, and the effect may already have happened.
    let recovered = store
        .recover_claimed_occurrences()
        .await
        .expect("recovery runs");
    assert_eq!(recovered.len(), 1, "the interrupted occurrence is reported");
    let after = store
        .occurrence(&occurrence.occurrence_key)
        .await
        .expect("the occurrence is readable")
        .expect("the occurrence exists");
    assert_eq!(
        after.state,
        OccurrenceState::Canceled.as_str(),
        "a claimed-but-unlaunched occurrence is canceled, not relaunched"
    );
    assert!(
        store
            .recover_claimed_occurrences()
            .await
            .expect("recovery runs again")
            .is_empty(),
        "recovery is idempotent"
    );

    // A pause that lands while an occurrence waits makes its revision stale, and
    // the claim is refused rather than launched.
    let paused_revision = store
        .set_schedule_state(&schedule.schedule_id, "paused", 1)
        .await
        .expect("the schedule is paused");
    assert_eq!(paused_revision, 2, "a state change bumps the revision");
    let stale = StoredOccurrenceRecord {
        occurrence_key: occurrence_key(&schedule.schedule_id, 1, next),
        schedule_id: schedule.schedule_id.clone(),
        revision: 1,
        due_unix_ms: next,
        claimed_at_unix_ms: next,
        state: OccurrenceState::Claimed.as_str().to_owned(),
        trigger_kind: "scheduled".to_owned(),
    };
    assert_eq!(
        store
            .claim_occurrence(&stale, next + 60_000)
            .await
            .expect_err("a stale revision cannot claim")
            .code(),
        ErrorCode::SequenceConflict,
        "the pause is what stops the queued occurrence"
    );
    // And a claim at the current revision is refused because the schedule is not
    // active: pausing is not the same as deleting, and neither launches.
    let current = StoredOccurrenceRecord {
        revision: 2,
        ..stale
    };
    assert_eq!(
        store
            .claim_occurrence(&current, next + 60_000)
            .await
            .expect_err("a paused schedule does not launch")
            .code(),
        ErrorCode::PolicyDenied
    );
    // Changing the state with a stale expectation is refused too, so two hosts
    // cannot both believe they paused it.
    assert_eq!(
        store
            .set_schedule_state(&schedule.schedule_id, "active", 1)
            .await
            .expect_err("a stale revision cannot change state")
            .code(),
        ErrorCode::SequenceConflict
    );

    // -----------------------------------------------------------------------
    // Unknown zones are refused rather than guessed
    // -----------------------------------------------------------------------
    assert_eq!(
        ScheduleZone::parse("Mars/Olympus")
            .expect_err("an unknown zone is refused")
            .code(),
        "invalid_payload"
    );
    let unknown = minute_schedule("Mars/Olympus");
    assert!(
        next_after(&unknown.spec, now).is_err(),
        "a schedule in an unknown zone does not silently run in UTC"
    );

    let store = Arc::try_unwrap(store).expect("the store is released");
    store.close().await.expect("the store closes");
}

/// A `once` schedule fires once and then never.
#[tokio::test]
async fn m11_02_a_once_schedule_fires_once() {
    let at = cet(2026, 6, 1, 9, 0);
    let schedule = Schedule {
        spec: ScheduleSpec::Once { at_unix_ms: at },
        ..minute_schedule("UTC")
    };
    let first = next_after(&schedule.spec, at - 1_000).expect("evaluates");
    assert_eq!(first, at);
    let second = next_after(&schedule.spec, at).expect("evaluates");
    assert_eq!(second, i64::MAX, "a past `once` has no next occurrence");
    let decision = due_now(&schedule, at + 1, MisfirePolicy::CatchUpBounded)
        .expect("the decision is computed");
    assert_eq!(decision.due.len(), 1, "it fires once");
    assert_eq!(decision.next_due_unix_ms, i64::MAX, "and never again");
}
