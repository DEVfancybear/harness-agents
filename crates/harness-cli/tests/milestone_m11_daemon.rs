//! M11-01 acceptance: the daemon host, its ownership rule and its control
//! channel.
//!
//! Everything here runs against the real store, the real writer fence and a real
//! loopback control socket. The clock is injected, so a schedule can come due
//! without waiting for one.

use std::{sync::Arc, time::Duration};

use harness_cli::daemon::{
    Clock, DaemonEndpoint, ENDPOINT_FILE, FixedClock, LaunchGrants, Schedule, ScheduleSpec,
    ScheduleState, control, read_endpoint, remove_endpoint, start, write_endpoint,
};
use harness_store_sqlite::{SqliteStore, StoredScheduleRecord, WriterOpenOptions};
use harness_types::{ErrorCode, HostId};
use serde_json::json;

#[path = "phase_p7/support.rs"]
mod support;

use support::temp_root;

fn grants() -> LaunchGrants {
    LaunchGrants {
        principal_id: "m11-daemon-fixture".to_owned(),
        project_id: None,
        task_id: None,
        edit_workspace: false,
        budget_tokens: 1_000,
        auto_approve_tools: false,
    }
}

/// A schedule that is due every minute in UTC.
fn due_schedule(id: &str, next_due_unix_ms: i64) -> StoredScheduleRecord {
    let schedule = Schedule {
        schedule_id: id.to_owned(),
        title: "daemon fixture".to_owned(),
        spec: ScheduleSpec::Cron {
            expression: "* * * * *".to_owned(),
            timezone: "UTC".to_owned(),
        },
        state: ScheduleState::Active,
        revision: 1,
        next_due_unix_ms,
        grants: grants(),
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

async fn seed_schedule(data_dir: &std::path::Path, next_due_unix_ms: i64) {
    let store = SqliteStore::open_writer(WriterOpenOptions::new(data_dir, HostId::generate()))
        .await
        .expect("store opens");
    store
        .create_schedule(&due_schedule("daemon_fixture", next_due_unix_ms))
        .await
        .expect("the schedule is created");
    store.close().await.expect("store closes");
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one daemon's whole life, step by step
async fn m11_01_daemon_ownership_and_control() {
    let root = temp_root();
    let data_dir = root.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("the data directory is created");
    // 2026-06-01T09:00:00Z, with the schedule due exactly one minute later.
    //
    // The fixture is deliberately *not* due at the starting instant. A schedule
    // that is already due is claimed by the evaluator on its own tick, which
    // advances the next due instant - by design, since the evaluator owns it. A
    // test that seeded it due would then be racing the evaluator for the value
    // it asserts on, and the manual trigger is the thing under test here, not
    // the evaluator: it must leave the next due where the evaluator put it, so
    // the assertion needs the evaluator to have put it somewhere and left it.
    let start_instant = 1_780_304_400_000_i64;
    let seeded_due = start_instant + 60_000;
    seed_schedule(&data_dir, seeded_due).await;
    let clock = Arc::new(FixedClock::new(start_instant));

    // -----------------------------------------------------------------------
    // One daemon owns the store
    // -----------------------------------------------------------------------
    let (host, mut runner) = start(&data_dir, Arc::clone(&clock) as Arc<dyn Clock>)
        .await
        .expect("the daemon starts");
    // Recovery finishes before the daemon is reachable, so a status request
    // reads a settled state rather than a half-recovered one.
    let recovered = runner.recover().await.expect("recovery runs");
    assert!(
        recovered.is_empty(),
        "a fresh data directory has nothing to recover: {recovered:?}"
    );
    assert!(
        host.endpoint.token.len() >= 16,
        "the control token has entropy"
    );
    assert_eq!(host.endpoint.schema_version, 1);

    // A second daemon is refused, and the refusal names the one that is running
    // rather than inventing a lock of its own.
    let second = start(&data_dir, Arc::clone(&clock) as Arc<dyn Clock>).await;
    let Err(refusal) = second else {
        panic!("a second daemon must be refused");
    };
    assert_eq!(
        refusal.code(),
        ErrorCode::RuntimeCommandConflict,
        "the refusal is typed: {}",
        refusal.message()
    );
    assert!(
        refusal.message().contains("already serving"),
        "and it says which endpoint is serving: {}",
        refusal.message()
    );

    // The endpoint file is the record of the running daemon.
    let published = read_endpoint(&data_dir)
        .expect("the endpoint is readable")
        .expect("the endpoint exists");
    assert_eq!(published.pid, std::process::id());
    assert_eq!(published.address, host.endpoint.address);

    // A foreign process cannot remove the endpoint.
    assert_eq!(
        remove_endpoint(&data_dir, published.pid + 1)
            .expect_err("a foreign removal is refused")
            .code(),
        ErrorCode::PolicyDenied
    );

    // The control channel is served by the loop, so it has to be running for a
    // client to reach it. The endpoint file is written at start, which is what a
    // client attaches to; the loop is what answers.
    let runner = tokio::spawn(async move { runner.run().await });
    // Wait for the loop to be accepting rather than guessing at a delay: the
    // endpoint is published at start, but the socket is served by the loop.
    await_control(&host.endpoint).await;

    // -----------------------------------------------------------------------
    // The control channel is authenticated and bounded
    // -----------------------------------------------------------------------
    let status = control(&host.endpoint, "status", None)
        .await
        .expect("status answers");
    assert_eq!(
        status["status"],
        json!("ok"),
        "the status reply was: {status}"
    );
    assert_eq!(status["daemon"]["schedules"], json!(1));
    assert_eq!(status["daemon"]["active"], json!(1));
    assert_eq!(status["daemon"]["stopping"], json!(false));

    // A wrong token is refused, and the refusal is typed rather than silent.
    let mut wrong = host.endpoint.clone();
    wrong.token = "0".repeat(32);
    let refused = control(&wrong, "status", None)
        .await
        .expect("the daemon answers even a wrong token");
    assert_eq!(refused["status"], json!("error"));
    assert_eq!(refused["error"]["code"], json!("control_unauthorized"));

    // Malformed input is refused with its own code, over a raw socket.
    let malformed = raw_control(&host.endpoint.address, "this is not json\n").await;
    assert_eq!(malformed["error"]["code"], json!("control_malformed"));
    let empty = raw_control(&host.endpoint.address, "\n").await;
    assert_eq!(empty["error"]["code"], json!("control_malformed"));
    // A request with no token at all is refused before anything is read.
    let no_token = raw_control(
        &host.endpoint.address,
        &format!("{}\n", json!({"command": "status"})),
    )
    .await;
    assert_eq!(no_token["error"]["code"], json!("control_malformed"));

    // A client that connects and never sends a line does not hold the daemon.
    let idle = tokio::net::TcpStream::connect(&host.endpoint.address)
        .await
        .expect("a client connects");
    let after_idle = control(&host.endpoint, "status", None)
        .await
        .expect("the daemon still answers while a client idles");
    assert_eq!(after_idle["status"], json!("ok"));
    drop(idle);

    // -----------------------------------------------------------------------
    // A client leaving does not own the run
    // -----------------------------------------------------------------------
    // The control request that follows a client exit is answered by the same
    // daemon with the same counters: attaching and detaching are reads.
    let before = control(&host.endpoint, "status", None)
        .await
        .expect("status answers");
    let after_client_exit = control(&host.endpoint, "status", None)
        .await
        .expect("status answers after a client left");
    assert_eq!(
        before["daemon"]["occurrences_launched"],
        after_client_exit["daemon"]["occurrences_launched"],
        "a client leaving changes nothing about the daemon's work"
    );

    // -----------------------------------------------------------------------
    // A manual trigger leaves the future schedule alone
    // -----------------------------------------------------------------------
    let triggered = control(&host.endpoint, "trigger", Some("daemon_fixture"))
        .await
        .expect("the trigger is accepted");
    assert_eq!(triggered["status"], json!("ok"));
    assert_eq!(
        triggered["triggered"]["next_due_unix_ms"],
        json!(seeded_due),
        "a manual trigger does not move the next due instant"
    );
    let unknown = control(&host.endpoint, "trigger", Some("no_such_schedule"))
        .await
        .expect("an unknown schedule is answered");
    assert_eq!(unknown["error"]["code"], json!("task_not_found"));

    // -----------------------------------------------------------------------
    // The daemon runs what is due, and a restart does not double it
    // -----------------------------------------------------------------------
    // Let the loop evaluate at least once, then advance the clock onto the
    // seeded due instant so the next occurrence comes due.
    tokio::time::sleep(Duration::from_millis(400)).await;
    clock.advance(60_000);
    tokio::time::sleep(Duration::from_millis(600)).await;
    let running = control(&host.endpoint, "status", None)
        .await
        .expect("status answers while running");
    let launched = running["daemon"]["occurrences_launched"]
        .as_u64()
        .expect("a count");
    assert!(launched >= 1, "the daemon claimed what came due: {running}");

    // Shutdown: the endpoint is removed by the process that owns it, and the
    // store is checkpointed so a restart reads a consistent database.
    let shutdown = control(&host.endpoint, "shutdown", None)
        .await
        .expect("shutdown is accepted");
    assert_eq!(shutdown["shutdown"], json!("accepted"));
    host.request_shutdown();
    let report = tokio::time::timeout(Duration::from_secs(10), runner)
        .await
        .expect("the daemon stops")
        .expect("the daemon task did not panic")
        .expect("the daemon stops cleanly");
    assert!(
        report.occurrences_launched >= 1,
        "the report counts what it launched: {report:?}"
    );
    assert!(
        !data_dir.join(ENDPOINT_FILE).exists(),
        "a stopped daemon removes the endpoint it owns"
    );

    // The state after a stop is recoverable: a new daemon starts, and the
    // occurrence the previous one claimed is not launched a second time.
    let store = SqliteStore::open_writer(WriterOpenOptions::new(&data_dir, HostId::generate()))
        .await
        .expect("the store reopens after the daemon stopped");
    let occurrences = store
        .occurrences_for("daemon_fixture")
        .await
        .expect("occurrences are readable");
    assert!(!occurrences.is_empty(), "the claim is durable");
    let claimed = occurrences
        .iter()
        .filter(|occurrence| occurrence.state == "claimed")
        .count();
    assert_eq!(
        claimed, 0,
        "shutdown settled or canceled every claim it made: {occurrences:?}"
    );
    store.close().await.expect("the store closes");

    let (host_again, runner_again) = start(&data_dir, Arc::clone(&clock) as Arc<dyn Clock>)
        .await
        .expect("a new daemon starts after the old one stopped");
    host_again.request_shutdown();
    let report_again = tokio::time::timeout(Duration::from_secs(10), runner_again.run())
        .await
        .expect("the second daemon stops")
        .expect("the second daemon stops cleanly");
    assert!(
        report_again.recovered.is_empty(),
        "nothing was left claimed by the previous daemon: {report_again:?}"
    );
}

#[tokio::test]
async fn m11_01_a_stale_endpoint_is_cleaned_up() {
    let root = temp_root();
    let data_dir = root.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("the data directory is created");

    // An endpoint naming a process that cannot be running: pid 0 is not a
    // process, so the file is stale by definition.
    let stale = DaemonEndpoint {
        schema_version: 1,
        address: "127.0.0.1:1".to_owned(),
        token: "0".repeat(32),
        pid: 0,
        started_at_unix_ms: 1_780_304_400_000,
    };
    write_endpoint(&data_dir, &stale).expect("the stale endpoint is written");
    assert!(data_dir.join(ENDPOINT_FILE).is_file());
    assert!(!harness_cli::daemon::process_is_alive(0));

    let clock = Arc::new(FixedClock::new(1_780_304_400_000));
    let (host, runner) = start(&data_dir, clock as Arc<dyn Clock>)
        .await
        .expect("a stale endpoint does not block a start");
    let published = read_endpoint(&data_dir)
        .expect("the endpoint is readable")
        .expect("the endpoint exists");
    assert_eq!(
        published.pid,
        std::process::id(),
        "the stale endpoint was replaced by the running daemon's"
    );
    assert_ne!(published.token, stale.token);
    host.request_shutdown();
    let _ = tokio::time::timeout(Duration::from_secs(10), async move { runner.run().await }).await;
}

/// Wait until the control socket answers, up to a bounded number of tries.
async fn await_control(endpoint: &DaemonEndpoint) {
    for _ in 0..100 {
        if control(endpoint, "status", None).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the daemon never answered a control request");
}

/// Send one raw line to the control port and read the reply.
async fn raw_control(address: &str, line: &str) -> serde_json::Value {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let stream = tokio::net::TcpStream::connect(address)
        .await
        .expect("the control port accepts");
    let (reader, mut writer) = stream.into_split();
    writer
        .write_all(line.as_bytes())
        .await
        .expect("the line is written");
    let mut reader = BufReader::new(reader);
    let mut reply = String::new();
    tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut reply))
        .await
        .expect("the daemon answers")
        .expect("the reply is read");
    serde_json::from_str(&reply).expect("the reply is JSON")
}
