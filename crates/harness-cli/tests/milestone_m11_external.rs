//! A35: remote submit/cancel ambiguity, and the poller that survives it.
//!
//! Everything here runs against a real MCP server process that speaks the pinned
//! SDK's SEP-2663 Tasks extension over stdio, a real store and an injected clock.
//! The remote applies its mutation and persists its task table, so "the answer
//! was lost" is produced by killing an answer at a process boundary - not by a
//! flag inside production code.

use std::{path::Path, path::PathBuf, sync::Arc, time::Duration};

use harness_cli::daemon::{
    AttachedRemotes, Clock, DaemonEndpoint, ExternalTaskRunner, FixedClock, NewExternalJob,
    TaskRemoteResolver, control, start,
};
use harness_extensions::tasks::TaskRemote;
use harness_extensions::{McpClient, McpTaskRemote};
use harness_store_sqlite::{
    ParentDeliveryRecord, SqliteStore, StoredExternalJob, WriterOpenOptions,
};
use harness_types::{ErrorCode, HostId, ScopeId, SessionId, TaskId};
use serde_json::{Value, json};

#[path = "phase_p7/support.rs"]
mod support;

use support::temp_root;

const START: i64 = 1_780_304_400_000;
const SERVER: &str = "m11.fixture.tasks";

fn fixture_task_server() -> PathBuf {
    // Cargo exports the compiled path for a fixture binary of this package; the
    // fallback keeps the test runnable when it is invoked directly from `deps`.
    if let Some(path) = option_env!("CARGO_BIN_EXE_m11_fixture_task_server") {
        let candidate = PathBuf::from(path);
        assert!(
            candidate.is_file(),
            "compiled fixture binary missing at {}",
            candidate.display()
        );
        return candidate;
    }
    let mut path = std::env::current_exe().expect("test binary path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    let candidate = path.join(format!(
        "m11_fixture_task_server{}",
        std::env::consts::EXE_SUFFIX
    ));
    assert!(
        candidate.is_file(),
        "compiled fixture binary missing at {}",
        candidate.display()
    );
    candidate
}

/// Attach one remote service, reached the way the host reaches any MCP server.
async fn attach(
    remotes: &AttachedRemotes,
    state: &Path,
    log: &Path,
    mode: &str,
) -> Arc<dyn TaskRemote> {
    let arguments = vec![
        "--mode".to_owned(),
        mode.to_owned(),
        "--state".to_owned(),
        state.to_string_lossy().into_owned(),
        "--log".to_owned(),
        log.to_string_lossy().into_owned(),
        "--complete-after".to_owned(),
        "2".to_owned(),
        "--poll-interval-ms".to_owned(),
        "50".to_owned(),
    ];
    let client = McpClient::connect_stdio(fixture_task_server(), arguments, ScopeId::generate(), 1)
        .await
        .expect("the fixture remote connects");
    assert!(
        client.server_supports_tasks(),
        "the fixture declares the tasks extension, so the client negotiated it"
    );
    let remote: Arc<dyn TaskRemote> =
        Arc::new(McpTaskRemote::new(Arc::new(client), SERVER, "long_job"));
    remotes.attach(Arc::clone(&remote));
    remote
}

fn lines(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(ToOwned::to_owned)
        .collect()
}

fn count(log: &Path, needle: &str) -> usize {
    lines(log)
        .iter()
        .filter(|line| line.starts_with(needle))
        .count()
}

/// The handle the remote minted before its answer was lost.
fn minted_handle(log: &Path) -> String {
    lines(log)
        .iter()
        .find_map(|line| line.strip_prefix("task_created=").map(ToOwned::to_owned))
        .unwrap_or_else(|| panic!("the remote recorded the task it created: {:?}", lines(log)))
}

struct Harness {
    store: Arc<SqliteStore>,
    clock: Arc<FixedClock>,
    remotes: Arc<AttachedRemotes>,
    runner: ExternalTaskRunner,
    parent: TaskId,
    session: SessionId,
}

impl Harness {
    async fn new(data_dir: &Path) -> Self {
        std::fs::create_dir_all(data_dir).expect("the data directory is created");
        let store = Arc::new(
            SqliteStore::open_writer(WriterOpenOptions::new(data_dir, HostId::generate()))
                .await
                .expect("the store opens"),
        );
        let clock = Arc::new(FixedClock::new(START));
        let remotes = Arc::new(AttachedRemotes::new());
        let runner = ExternalTaskRunner::new(
            Arc::clone(&store),
            Arc::clone(&remotes) as Arc<dyn TaskRemoteResolver>,
            Arc::clone(&clock) as Arc<dyn Clock>,
        );
        Self {
            store,
            clock,
            remotes,
            runner,
            parent: TaskId::generate(),
            session: SessionId::generate(),
        }
    }

    /// A submission by this host, for the fixture's `long_job` operation.
    fn job(&self, job_id: &str, subject: &str, deadline_ms: u64) -> NewExternalJob {
        NewExternalJob {
            job_id: job_id.to_owned(),
            parent_task_id: self.parent.to_string(),
            session_id: self.session.to_string(),
            server_id: SERVER.to_owned(),
            operation: "long_job".to_owned(),
            request: json!({"subject": subject}),
            deadline_ms,
        }
    }

    async fn row(&self, job_id: &str) -> StoredExternalJob {
        self.store
            .external_job(job_id)
            .await
            .expect("the job is readable")
            .expect("the job exists")
    }

    /// Deliveries waiting for the parent, oldest first.
    async fn deliveries(&self) -> Vec<ParentDeliveryRecord> {
        self.store
            .pending_deliveries(&self.parent)
            .await
            .expect("deliveries are readable")
    }

    /// Poll a job until it settles, advancing the injected clock as a real host
    /// advances through wall time.
    async fn poll_until_settled(&self, job_id: &str, rounds: u32) -> String {
        let mut observed = Vec::new();
        for _ in 0..rounds {
            self.clock.advance(1_000);
            let report = self
                .runner
                .poll_one(job_id)
                .await
                .expect("the poll is answered");
            if report.settled.contains(&job_id.to_owned()) {
                return "settled".to_owned();
            }
            if report.deadline_exceeded.contains(&job_id.to_owned()) {
                return "deadline_exceeded".to_owned();
            }
            observed.push(format!("polled={}", report.polled));
        }
        panic!("the job never settled within {rounds} polls: {observed:?}");
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one acceptance case, five ordered situations
async fn a35_external_task_ambiguity() {
    let root = temp_root();
    let data_dir = root.path().join("data");
    let h = Harness::new(&data_dir).await;
    let state = root.path().join("remote-state.json");
    let log_one = root.path().join("remote-1.log");

    // -----------------------------------------------------------------------
    // 1. The remote applies the mutation, and the answer never arrives
    // -----------------------------------------------------------------------
    attach(&h.remotes, &state, &log_one, "drop_after_accept").await;
    let submitted = h
        .runner
        .submit(h.job("job-1", "one", 600_000))
        .await
        .expect("the submission is recorded before it is sent");
    assert_eq!(submitted.state, "ambiguous", "{submitted:?}");
    assert!(submitted.is_ambiguous());
    assert!(
        submitted.remote_task_id.is_none(),
        "no handle came back, so there is nothing to poll"
    );
    assert!(submitted.ambiguity.is_some(), "the reason is recorded");
    let job = h.row("job-1").await;
    assert_eq!(job.state, "ambiguous");
    assert!(job.settled_at_unix_ms.is_none(), "the job is still open");
    assert!(
        h.deliveries().await.is_empty(),
        "an unresolved submission delivers nothing to the parent"
    );

    // Negative control: the same request, sent again, is refused. This is the
    // whole point - a retry after a lost answer is a second mutation.
    let repeated = h.runner.submit(h.job("job-2", "one", 600_000)).await;
    let Err(refusal) = repeated else {
        panic!("a repeated submission must be refused");
    };
    assert_eq!(refusal.code(), ErrorCode::IdempotencyConflict);
    assert_eq!(
        count(&log_one, "call_tool long_job"),
        1,
        "the remote was asked to start the work exactly once: {:?}",
        lines(&log_one)
    );

    // A second, different request goes to a fresh remote process (the first one
    // exited) and is lost the same way, which is what makes job-3 ambiguous
    // *because its answer was dropped*, not because nothing was reachable.
    let log_two = root.path().join("remote-2.log");
    attach(&h.remotes, &state, &log_two, "drop_after_accept").await;
    let other = h
        .runner
        .submit(h.job("job-3", "two", 600_000))
        .await
        .expect("a different request is its own submission");
    assert!(other.is_ambiguous(), "{other:?}");
    assert_eq!(count(&log_two, "call_tool long_job"), 1);

    // -----------------------------------------------------------------------
    // 2. Reconciliation: the handle the lost answer carried, supplied explicitly
    // -----------------------------------------------------------------------
    let handle = minted_handle(&log_one);
    // The remote service comes back. Its task table survived because the service
    // persisted it, which is what makes polling a task from before the restart
    // possible at all.
    let log_three = root.path().join("remote-3.log");
    attach(&h.remotes, &state, &log_three, "resume").await;
    assert!(
        h.runner
            .reconcile("job-1", &handle)
            .await
            .expect("the reconciliation is answered"),
        "an ambiguous submission accepts the handle that resolves it"
    );
    assert!(
        !h.runner
            .reconcile("job-1", &handle)
            .await
            .expect("the second reconciliation is answered"),
        "a reconciled job is no longer ambiguous, so a second reconciliation changes nothing"
    );
    assert_eq!(h.poll_until_settled("job-1", 6).await, "settled");
    let settled = h.row("job-1").await;
    assert_eq!(settled.state, "completed");
    assert_eq!(settled.remote_state.as_deref(), Some("completed"));
    assert!(settled.settled_at_unix_ms.is_some());
    let delivery_id = settled
        .delivery_message_id
        .clone()
        .expect("the settlement carries its delivery id");
    let delivery = h
        .store
        .delivery(&delivery_id)
        .await
        .expect("the delivery is readable")
        .expect("the delivery exists");
    assert_eq!(delivery.payload["remote_state"], json!("completed"));
    assert_eq!(delivery.payload["job_id"], json!("job-1"));
    assert_eq!(
        h.deliveries().await.len(),
        1,
        "one settlement, one logical delivery"
    );

    // A repeated terminal observation settles nothing a second time.
    h.clock.advance(1_000);
    h.runner.poll_one("job-1").await.expect("answered");
    assert_eq!(
        h.deliveries().await.len(),
        1,
        "a terminal result observed twice is delivered once"
    );
    assert_eq!(h.row("job-1").await.state, "completed");

    // -----------------------------------------------------------------------
    // 3. A host restart polls the handle it already has, and resubmits nothing
    // -----------------------------------------------------------------------
    let accepted = h
        .runner
        .submit(h.job("job-4", "three", 600_000))
        .await
        .expect("the submission is recorded");
    assert_eq!(accepted.state, "accepted", "{accepted:?}");
    assert!(accepted.remote_task_id.is_some());
    let calls_before = count(&log_three, "call_tool long_job");

    // A new host: a new runner over the same store, which is what a restart of
    // this process leaves behind.
    let restarted = ExternalTaskRunner::new(
        Arc::clone(&h.store),
        Arc::clone(&h.remotes) as Arc<dyn TaskRemoteResolver>,
        Arc::clone(&h.clock) as Arc<dyn Clock>,
    );
    let recovered = restarted.recover().await.expect("recovery runs");
    assert!(
        recovered.resumed.contains(&"job-4".to_owned()),
        "the handle is durable, so polling resumes: {recovered:?}"
    );
    assert!(
        recovered.ambiguous.contains(&"job-3".to_owned()),
        "an unanswered submission waits for a human instead of being resent: {recovered:?}"
    );
    assert_eq!(h.poll_until_settled("job-4", 6).await, "settled");
    assert_eq!(h.row("job-4").await.state, "completed");
    assert_eq!(
        count(&log_three, "call_tool long_job"),
        calls_before,
        "resuming polls, and never resubmits"
    );

    // -----------------------------------------------------------------------
    // 4. A cancel that loses the race is not a rollback
    // -----------------------------------------------------------------------
    let log_four = root.path().join("remote-4.log");
    attach(&h.remotes, &state, &log_four, "cancel_race").await;
    let raced = h
        .runner
        .submit(h.job("job-5", "four", 600_000))
        .await
        .expect("the submission is recorded");
    let handle_five = raced.remote_task_id.clone().expect("a handle");
    let cancel = h.runner.cancel("job-5").await.expect("the cancel is sent");
    assert!(cancel.acknowledged, "the remote acknowledged the cancel");
    assert_eq!(
        cancel.state, "cancel_requested",
        "an acknowledged cancel is a request, not a settlement"
    );
    assert_eq!(h.poll_until_settled("job-5", 6).await, "settled");
    let after_cancel = h.row("job-5").await;
    assert_eq!(
        after_cancel.state, "completed",
        "the remote's own status settles the job, and it completed"
    );
    assert_eq!(after_cancel.remote_state.as_deref(), Some("completed"));
    assert_eq!(
        count(&log_four, &format!("tasks_cancel_raced={handle_five}")),
        1,
        "the cancel really reached the remote while the work was finishing: {:?}",
        lines(&log_four)
    );
    assert_eq!(
        h.deliveries().await.len(),
        3,
        "job-1, job-4 and job-5 each delivered exactly once"
    );

    // -----------------------------------------------------------------------
    // 5. A remote that never finishes runs out of deadline, and says so
    // -----------------------------------------------------------------------
    let log_five = root.path().join("remote-5.log");
    attach(&h.remotes, &state, &log_five, "never_terminal").await;
    let endless = h
        .runner
        .submit(h.job("job-6", "five", 2_500))
        .await
        .expect("the submission is recorded");
    assert_eq!(endless.state, "accepted", "{endless:?}");
    assert_eq!(
        h.poll_until_settled("job-6", 8).await,
        "deadline_exceeded",
        "the host stops asking by itself"
    );
    let expired = h.row("job-6").await;
    assert_eq!(expired.state, "deadline_exceeded");
    let outcome: Value = serde_json::from_str(
        expired
            .outcome_json
            .as_deref()
            .expect("a deadline settlement records an outcome"),
    )
    .expect("the outcome is JSON");
    assert_eq!(outcome["unresolved"], json!(true));
    assert_eq!(
        outcome["remote_state"],
        json!("working"),
        "the last observation is recorded, and it was not terminal"
    );
    let polls = h
        .store
        .external_job_polls("job-6")
        .await
        .expect("the poll log is readable");
    assert!(
        polls.len() <= 4,
        "a remote that never finishes is asked a bounded number of times, not in a spin loop: {}",
        polls.len()
    );
    assert_eq!(
        h.deliveries().await.len(),
        4,
        "an unresolved outcome is delivered too: the parent is told it is unknown"
    );
    assert!(
        h.row("job-3").await.state == "ambiguous",
        "the submission nobody reconciled is still waiting, and was never resent"
    );
    // The store is closed by dropping the last handle to it, which is also what
    // releases the writer fence this data directory is under.
    drop(h);
}
#[tokio::test]
async fn m11_03_the_daemon_polls_external_work_on_its_own_tick() {
    let root = temp_root();
    let data_dir = root.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("the data directory is created");
    let clock = Arc::new(FixedClock::new(START));
    let (host, mut runner) = start(&data_dir, Arc::clone(&clock) as Arc<dyn Clock>)
        .await
        .expect("the daemon starts");
    let remotes = Arc::new(AttachedRemotes::new());
    let state = root.path().join("remote-state.json");
    let log = root.path().join("remote.log");
    attach(&remotes, &state, &log, "normal").await;
    let external =
        runner.attach_external_tasks(Arc::clone(&remotes) as Arc<dyn TaskRemoteResolver>);
    let store = Arc::clone(external.store());
    let parent = TaskId::generate();
    let session = SessionId::generate();
    let runner_task = tokio::spawn(async move { runner.run().await });
    await_daemon(&host.endpoint).await;

    external
        .submit(NewExternalJob {
            job_id: "daemon-job".to_owned(),
            parent_task_id: parent.to_string(),
            session_id: session.to_string(),
            server_id: SERVER.to_owned(),
            operation: "long_job".to_owned(),
            request: json!({"subject": "driven by the daemon"}),
            deadline_ms: 600_000,
        })
        .await
        .expect("the submission is recorded");

    // Nobody polls by hand: the daemon's own tick is the consumer, and the only
    // thing this test does is let time pass.
    let mut settled = false;
    for _ in 0..8 {
        clock.advance(500);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let job = store
            .external_job("daemon-job")
            .await
            .expect("readable")
            .expect("the job exists");
        if job.is_settled() {
            assert_eq!(job.state, "completed");
            settled = true;
            break;
        }
    }
    assert!(settled, "the daemon settled the job without a manual poll");
    let polls = store
        .external_job_polls("daemon-job")
        .await
        .expect("the poll log is readable");
    assert!(
        polls.len() <= 6,
        "the daemon polls on a bounded schedule: {}",
        polls.len()
    );
    assert_eq!(
        store
            .pending_deliveries(&parent)
            .await
            .expect("deliveries are readable")
            .len(),
        1
    );

    host.request_shutdown();
    let _ = tokio::time::timeout(Duration::from_secs(10), runner_task).await;
    // The daemon's loop consumed its own handle to the store when it stopped, so
    // dropping the last one here closes it and releases the writer fence.
    drop(external);
    drop(store);
}

async fn await_daemon(endpoint: &DaemonEndpoint) {
    for _ in 0..100 {
        if control(endpoint, "status", None).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the daemon never answered a control request");
}

/// The negative control for the declaration, and the reason an error reply is
/// not proof that nothing happened.
///
/// The same fixture is driven by a client that does **not** declare
/// `io.modelcontextprotocol/tasks`. SEP-2663 forbids the server from handing a
/// task to such a client, and the pinned SDK enforces it - but the SDK enforces
/// it *after* the handler ran, so the fixture had already created and persisted
/// the task when the error was returned. Two things follow, and both are
/// asserted here: the declaration our adapter makes is load-bearing, and a
/// remote error cannot be read as "nothing was applied" - which is why the
/// driver treats every post-send failure as ambiguous and never resubmits one.
#[tokio::test]
async fn m11_03_the_tasks_declaration_is_load_bearing() {
    use rmcp::ServiceExt;
    use rmcp::model::{CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation};

    let root = temp_root();
    let state = root.path().join("remote-state.json");
    let log = root.path().join("remote.log");
    let mut command = tokio::process::Command::new(fixture_task_server());
    command.args([
        "--mode",
        "normal",
        "--state",
        &state.to_string_lossy(),
        "--log",
        &log.to_string_lossy(),
    ]);
    let transport = rmcp::transport::TokioChildProcess::new(command).expect("the fixture starts");
    let client = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("m11-negative-control", "0.0.0"),
    )
    .serve(transport)
    .await
    .expect("the handshake completes without the tasks extension");
    let refused = client
        .call_tool(CallToolRequestParams::new("long_job"))
        .await;
    let error = refused.expect_err("a client that declares nothing cannot be handed a task");
    assert!(
        error.to_string().contains("capabilit"),
        "the refusal names the missing capability: {error}"
    );
    assert_eq!(
        count(&log, "task_created="),
        1,
        "the remote created the task anyway: the error came after the work, so an error is not proof that nothing happened"
    );
    client.cancel().await.expect("the fixture stops");
}
