//! M11-04 acceptance: what a schedule does when nobody is there to ask.
//!
//! Everything here runs against the real store, the real writer fence, the real
//! daemon loop and a real control socket. The clock is injected, so a window can
//! close without waiting an hour for it.

use std::{path::Path, sync::Arc, sync::Mutex, time::Duration};

use harness_cli::daemon::{
    Clock, DaemonEndpoint, FixedClock, LaunchGrants, NotificationConnector, NotificationEvent,
    NotificationOutbox, Schedule, ScheduleSpec, ScheduleState, control, decide, start,
};
use harness_store_sqlite::{
    NOTIFICATION_MAX_ATTEMPTS, SqliteStore, StoredNotification, StoredScheduleRecord,
    WriterOpenOptions,
};
use harness_types::{ErrorCode, HarnessError, HostId};
use serde_json::json;

#[path = "phase_p7/support.rs"]
mod support;

use support::temp_root;

const START: i64 = 1_780_304_400_000;

/// A schedule that wants to edit a workspace, which is the case with no human.
fn editing_schedule(id: &str, next_due_unix_ms: i64) -> StoredScheduleRecord {
    let schedule = Schedule {
        schedule_id: id.to_owned(),
        title: "nightly edit".to_owned(),
        spec: ScheduleSpec::Cron {
            expression: "* * * * *".to_owned(),
            timezone: "UTC".to_owned(),
        },
        state: ScheduleState::Active,
        revision: 1,
        next_due_unix_ms,
        grants: LaunchGrants {
            principal_id: "m11-policy-fixture".to_owned(),
            project_id: None,
            task_id: None,
            edit_workspace: true,
            budget_tokens: 1_000,
            auto_approve_tools: false,
        },
    };
    StoredScheduleRecord {
        schedule_id: schedule.schedule_id,
        title: schedule.title,
        state: schedule.state.as_str().to_owned(),
        revision: 1,
        next_due_unix_ms,
        spec_json: serde_json::to_string(&schedule.spec).expect("spec serializes"),
        grants_json: serde_json::to_string(&schedule.grants).expect("grants serialize"),
    }
}

async fn seed_schedule(data_dir: &Path, record: &StoredScheduleRecord) {
    let store = SqliteStore::open_writer(WriterOpenOptions::new(data_dir, HostId::generate()))
        .await
        .expect("store opens");
    store
        .create_schedule(record)
        .await
        .expect("the schedule is created");
    store.close().await.expect("store closes");
}

/// A read-only view of the daemon's store.
///
/// The daemon holds the writer fence, which is the point of the daemon: this
/// test can read what it wrote and can only *ask* it to decide.
async fn reader(data_dir: &Path) -> SqliteStore {
    SqliteStore::open_read_only(data_dir)
        .await
        .expect("the daemon's store is readable")
}

/// Wait until `check` holds, advancing the clock as a host would live through it.
async fn wait_until<F, Fut>(clock: &FixedClock, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..40 {
        if check().await {
            return;
        }
        clock.advance(250);
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    panic!("the daemon never reached the state this test waits for");
}

async fn daemon_status(endpoint: &DaemonEndpoint) -> serde_json::Value {
    control(endpoint, "status", None)
        .await
        .expect("status answers")
}

/// Restart the daemon on the same data directory.
///
/// A daemon's control tasks hold store handles of their own, and a task that has
/// not yet noticed its peer closing keeps the writer lock alive for a moment
/// after the loop returns. The retry is bounded, and it says how long the lock
/// took to come back, so a real leak would be visible rather than tolerated.
async fn restart(
    data_dir: &Path,
    clock: &Arc<FixedClock>,
) -> (
    harness_cli::daemon::DaemonHost,
    harness_cli::daemon::DaemonRunner,
) {
    for attempt in 1..=20 {
        match start(data_dir, Arc::clone(clock) as Arc<dyn Clock>).await {
            Ok(pair) => {
                assert_eq!(
                    attempt, 1,
                    "the daemon hands the writer lock back when it stops"
                );
                return pair;
            }
            Err(error) if attempt < 20 => {
                assert_eq!(
                    error.code(),
                    ErrorCode::RuntimeCommandConflict,
                    "the restart failed for another reason: {error}"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => panic!("the daemon never restarted: {error}"),
        }
    }
    unreachable!("the loop returns or panics")
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one acceptance case, four ordered decisions
async fn m11_04_a_scheduled_launch_without_a_human_waits() {
    let root = temp_root();
    let data_dir = root.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("the data directory is created");
    let seeded_due = START + 60_000;
    seed_schedule(&data_dir, &editing_schedule("nightly", seeded_due)).await;

    let clock = Arc::new(FixedClock::new(START));
    let (host, runner) = start(&data_dir, Arc::clone(&clock) as Arc<dyn Clock>)
        .await
        .expect("the daemon starts");
    let runner_task = tokio::spawn(async move { runner.run().await });
    wait_until(&clock, || async {
        control(&host.endpoint, "status", None).await.is_ok()
    })
    .await;

    // -----------------------------------------------------------------------
    // 1. It comes due, it is claimed, and it does not run
    // -----------------------------------------------------------------------
    let store = reader(&data_dir).await;
    clock.advance(60_000);
    wait_until(&clock, || async {
        !store
            .waiting_occurrences()
            .await
            .unwrap_or_default()
            .is_empty()
    })
    .await;
    let waiting = store
        .waiting_occurrences()
        .await
        .expect("waiting occurrences are readable");
    assert_eq!(waiting.len(), 1, "one occurrence is held");
    let occurrence_key = waiting[0].occurrence_key.clone();
    assert_eq!(waiting[0].state, "waiting");
    assert_eq!(waiting[0].due_unix_ms, seeded_due);
    let approval = store
        .approval(&occurrence_key)
        .await
        .expect("the approval is readable")
        .expect("an approval was requested");
    assert!(approval.is_open(), "nobody has decided yet: {approval:?}");
    assert!(
        approval.prompt.contains("nightly"),
        "the question names what it is about: {}",
        approval.prompt
    );
    // The window opens when the request is made, which is at or after the due
    // instant - never before it.
    assert_eq!(
        approval.expires_at_unix_ms,
        approval.requested_at_unix_ms + 3_600_000,
        "the window is an hour from the request: {approval:?}"
    );
    assert!(
        approval.requested_at_unix_ms >= seeded_due,
        "the request is made when the occurrence comes due: {approval:?}"
    );

    // The status shows it, with the window and what happens at the end of it.
    let status = daemon_status(&host.endpoint).await;
    assert_eq!(status["daemon"]["occurrences_launched"], json!(0));
    let listed = status["daemon"]["waiting"]
        .as_array()
        .expect("the status lists what is waiting");
    assert_eq!(listed.len(), 1, "{status}");
    assert_eq!(listed[0]["occurrence_key"], json!(occurrence_key));
    assert_eq!(listed[0]["schedule_id"], json!("nightly"));
    assert_eq!(
        listed[0]["expires_at_unix_ms"],
        json!(approval.expires_at_unix_ms)
    );
    assert!(
        listed[0]["next_action"]
            .as_str()
            .is_some_and(|text| text.contains("expires")),
        "{status}"
    );

    // -----------------------------------------------------------------------
    // 2. A restart keeps the question, and does not cancel the occurrence
    // -----------------------------------------------------------------------
    host.request_shutdown();
    let report = tokio::time::timeout(Duration::from_secs(10), runner_task)
        .await
        .expect("the daemon stops")
        .expect("the daemon task did not panic")
        .expect("the daemon stops cleanly");
    assert!(
        report.recovered.is_empty(),
        "a waiting occurrence is not a claim to cancel: {report:?}"
    );
    drop(store);

    let (host_again, runner_again) = restart(&data_dir, &clock).await;
    let store = reader(&data_dir).await;
    assert_eq!(
        store.waiting_occurrences().await.expect("readable").len(),
        1,
        "the question survived the restart"
    );
    let runner_task = tokio::spawn(async move { runner_again.run().await });

    // -----------------------------------------------------------------------
    // 3. An approval launches it, exactly once
    // -----------------------------------------------------------------------
    let approved = decide(&host_again.endpoint, &occurrence_key, "approved")
        .await
        .expect("the decision is accepted");
    assert_eq!(approved["status"], json!("ok"), "{approved}");
    assert_eq!(approved["decision"]["state"], json!("approved"));
    // A second decision is answered without changing the first: the record is
    // the decision, and it is already made.
    let again = decide(&host_again.endpoint, &occurrence_key, "denied")
        .await
        .expect("the second decision is answered");
    assert_eq!(again["decision"]["unchanged"], json!(true), "{again}");
    assert_eq!(again["decision"]["state"], json!("approved"));
    clock.advance(250);
    wait_until(&clock, || async {
        store
            .occurrences_for("nightly")
            .await
            .unwrap_or_default()
            .iter()
            .any(|occurrence| occurrence.state == "launched")
    })
    .await;
    let occurrences = store
        .occurrences_for("nightly")
        .await
        .expect("occurrences are readable");
    assert_eq!(
        occurrences
            .iter()
            .filter(|occurrence| occurrence.state == "launched")
            .count(),
        1,
        "exactly one launch: {occurrences:?}"
    );
    let status = daemon_status(&host_again.endpoint).await;
    assert_eq!(status["daemon"]["occurrences_launched"], json!(1));
    assert_eq!(
        status["daemon"]["waiting"].as_array().map(Vec::len),
        Some(0),
        "nothing is waiting once it was decided: {status}"
    );

    // -----------------------------------------------------------------------
    // 4. A refusal is a refusal, and a closed window is neither a yes nor a no
    // -----------------------------------------------------------------------
    // The next occurrence comes due; this time the answer is no.
    clock.advance(60_000);
    wait_until(&clock, || async {
        !store
            .waiting_occurrences()
            .await
            .unwrap_or_default()
            .is_empty()
    })
    .await;
    let denied_key = store.waiting_occurrences().await.expect("readable")[0]
        .occurrence_key
        .clone();
    assert_ne!(
        denied_key, occurrence_key,
        "a second occurrence, a second question"
    );
    let refused = decide(&host_again.endpoint, &denied_key, "denied")
        .await
        .expect("the refusal is recorded");
    assert_eq!(refused["decision"]["state"], json!("denied"), "{refused}");
    clock.advance(250);
    wait_until(&clock, || async {
        store
            .occurrences_for("nightly")
            .await
            .unwrap_or_default()
            .iter()
            .any(|occurrence| occurrence.state == "skipped")
    })
    .await;

    // A third occurrence is left to the window.
    clock.advance(60_000);
    wait_until(&clock, || async {
        !store
            .waiting_occurrences()
            .await
            .unwrap_or_default()
            .is_empty()
    })
    .await;
    let expired_key = store.waiting_occurrences().await.expect("readable")[0]
        .occurrence_key
        .clone();
    clock.advance(3_600_001);
    wait_until(&clock, || async {
        store
            .occurrences_for("nightly")
            .await
            .unwrap_or_default()
            .iter()
            .any(|occurrence| occurrence.state == "expired")
    })
    .await;
    let expired = store
        .approval(&expired_key)
        .await
        .expect("readable")
        .expect("the approval exists");
    assert_eq!(expired.state, "expired", "{expired:?}");
    let status = daemon_status(&host_again.endpoint).await;
    assert_eq!(
        status["daemon"]["occurrences_launched"],
        json!(1),
        "a refusal and an expiry launch nothing: {status}"
    );

    // Every decision is in the outbox, and each change is one row.
    let notifications = store
        .list_notifications()
        .await
        .expect("the outbox is readable");
    assert!(
        notifications
            .iter()
            .any(|notification| notification.kind == "schedule_approval_requested"),
        "the request is recorded: {notifications:?}"
    );
    assert!(
        notifications
            .iter()
            .any(|notification| notification.kind == "schedule_approval_launched"),
        "the approval is recorded"
    );
    assert!(
        notifications
            .iter()
            .any(|notification| notification.kind == "schedule_approval_skipped"),
        "the refusal is recorded"
    );
    assert!(
        notifications
            .iter()
            .any(|notification| notification.kind == "schedule_approval_expired"),
        "the expiry is recorded"
    );
    let mut digests: Vec<(String, String)> = notifications
        .iter()
        .map(|notification| {
            (
                notification.subject_id.clone(),
                notification.change_digest.clone(),
            )
        })
        .collect();
    let before = digests.len();
    digests.sort();
    digests.dedup();
    assert_eq!(digests.len(), before, "no change is recorded twice");

    host_again.request_shutdown();
    let _ = tokio::time::timeout(Duration::from_secs(10), runner_task).await;
}

/// A connector that records what it was asked to send, and can be told to fail.
struct RecordingConnector {
    sent: Mutex<Vec<String>>,
    fail: bool,
}

impl RecordingConnector {
    fn new(fail: bool) -> Self {
        Self {
            sent: Mutex::new(Vec::new()),
            fail,
        }
    }

    fn sent(&self) -> Vec<String> {
        self.sent.lock().expect("sent log").clone()
    }
}

impl NotificationConnector for RecordingConnector {
    fn channel(&self) -> &'static str {
        "fixture-channel"
    }

    fn send<'a>(
        &'a self,
        notification: &'a StoredNotification,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            if self.fail {
                return Err(HarnessError::new(
                    ErrorCode::ServiceUnavailable,
                    "the fixture connector is unavailable",
                ));
            }
            self.sent
                .lock()
                .expect("sent log")
                .push(notification.notification_id.clone());
            Ok(())
        })
    }
}

fn event(subject: &str, change: &str) -> NotificationEvent {
    NotificationEvent {
        subject_kind: "occurrence".to_owned(),
        subject_id: subject.to_owned(),
        kind: "schedule_approval_requested".to_owned(),
        payload: json!({"schema_version": 1, "subject": subject, "change": change}),
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one outbox contract, five ordered assertions
async fn m11_04_the_outbox_is_durable_and_sends_nothing_without_a_connector() {
    let root = temp_root();
    let data_dir = root.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("the data directory is created");
    let store = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(&data_dir, HostId::generate()))
            .await
            .expect("the store opens"),
    );
    let clock = Arc::new(FixedClock::new(START));

    // No connector: the record is made and nothing is sent.
    let outbox = NotificationOutbox::new(
        Arc::clone(&store),
        None,
        Arc::clone(&clock) as Arc<dyn Clock>,
    );
    assert!(outbox.channel().is_none());
    assert!(
        outbox
            .notify(&event("occurrence-a", "waiting"))
            .await
            .expect("recorded")
    );
    assert!(
        !outbox
            .notify(&event("occurrence-a", "waiting"))
            .await
            .expect("answered"),
        "the same change is one notification"
    );
    assert!(
        outbox
            .notify(&event("occurrence-a", "expired"))
            .await
            .expect("recorded"),
        "a different change is a different notification"
    );
    assert_eq!(
        store.list_notifications().await.expect("readable").len(),
        2,
        "a duplicate event yields one logical delivery"
    );
    let report = outbox.deliver_due().await.expect("the pass runs");
    assert_eq!(report.unconfigured, 2, "{report:?}");
    assert_eq!(report.delivered, 0, "nothing was sent: {report:?}");
    assert!(
        outbox
            .pending()
            .await
            .expect("readable")
            .iter()
            .all(|notification| notification.attempts == 0),
        "no attempt is recorded when there was nowhere to send"
    );
    assert_eq!(
        store.outbox_counts().await.expect("counts").pending,
        2,
        "the records are still pending, not lost"
    );

    // A connector that fails: bounded retries with a growing delay.
    let failing = Arc::new(RecordingConnector::new(true));
    let outbox = NotificationOutbox::new(
        Arc::clone(&store),
        Some(Arc::clone(&failing) as Arc<dyn NotificationConnector>),
        Arc::clone(&clock) as Arc<dyn Clock>,
    );
    assert_eq!(outbox.channel(), Some("fixture-channel"));
    let mut reported_attempts = Vec::new();
    for _ in 0..NOTIFICATION_MAX_ATTEMPTS {
        let report = outbox.deliver_due().await.expect("the pass runs");
        let pending = outbox.pending().await.expect("readable");
        reported_attempts.extend(pending.iter().map(|notification| notification.attempts));
        if report.retried == 0 {
            break;
        }
        // The next attempt is due after the delay the store computed.
        clock.advance(report_attempt_delay(&pending));
    }
    let counts = store.outbox_counts().await.expect("counts");
    assert_eq!(
        counts.failed, 2,
        "a connector that stays down ends up failed"
    );
    assert!(
        reported_attempts.windows(2).any(|pair| pair[0] < pair[1]),
        "attempts grew: {reported_attempts:?}"
    );

    // A connector that works: a change already delivered is not delivered again.
    let working = Arc::new(RecordingConnector::new(false));
    let outbox = NotificationOutbox::new(
        Arc::clone(&store),
        Some(Arc::clone(&working) as Arc<dyn NotificationConnector>),
        Arc::clone(&clock) as Arc<dyn Clock>,
    );
    assert!(
        outbox
            .notify(&event("occurrence-b", "waiting"))
            .await
            .expect("recorded")
    );
    let report = outbox.deliver_due().await.expect("the pass runs");
    assert_eq!(report.delivered, 1, "{report:?}");
    assert_eq!(working.sent().len(), 1);
    assert!(
        !outbox
            .notify(&event("occurrence-b", "waiting"))
            .await
            .expect("answered"),
        "the change is still one notification"
    );
    let report = outbox.deliver_due().await.expect("the pass runs");
    assert_eq!(
        report.delivered, 0,
        "a delivered change is not re-sent: {report:?}"
    );
    assert_eq!(working.sent().len(), 1);
    assert_eq!(store.outbox_counts().await.expect("counts").delivered, 1);

    // A canceled notification is visible and never sent.
    store
        .cancel_notification(
            &store.list_notifications().await.expect("readable")[0].notification_id,
            "the operator resolved it by hand",
            START,
        )
        .await
        .expect("the cancellation is recorded");
    assert_eq!(store.outbox_counts().await.expect("counts").canceled, 1);
    drop(outbox);
    drop(store);
}

/// How long to wait before the next attempt: the longest backoff the store can
/// compute, so the next attempt is always due.
fn report_attempt_delay(_pending: &[StoredNotification]) -> i64 {
    harness_store_sqlite::NOTIFICATION_MAX_BACKOFF_MS
}
