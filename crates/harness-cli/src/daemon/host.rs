//! The daemon host: one long-lived process, one writer, and a bounded local
//! control channel.
//!
//! Three rules come from the plan and shape everything here:
//!
//! 1. **The daemon is not a second runtime.** It composes the same services the
//!    interactive app composes, over the same store and the same writer fence.
//!    What it adds is a lifetime that outlives a client, and a clock.
//! 2. **A client does not own execution.** Attaching and detaching are reads of
//!    the same durable state; a client that exits while a run is in flight
//!    changes nothing about that run.
//! 3. **Two daemons are one too many.** The store's writer fence already refuses
//!    a second writable host, so the daemon does not invent a lock: it reports
//!    the refusal. A stale endpoint is cleaned up only when the process that
//!    wrote it is provably gone.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use harness_store_sqlite::{SqliteStore, StoredOccurrenceRecord, WriterOpenOptions};
use harness_types::{ErrorCode, HarnessError, HostId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::Notify,
};

use crate::daemon::{
    Clock, ExternalTaskRunner, MisfirePolicy, ScheduleSpec, ScheduleState, SystemClock,
    TaskRemoteResolver, due_now, occurrence_key,
};

/// The file the running daemon records its control endpoint in.
pub const ENDPOINT_FILE: &str = "daemon.json";

/// How long one control request may take before the connection is closed.
///
/// A bounded request is the point: a local client that sends half a line and
/// stops must not hold a task of the daemon's.
pub const CONTROL_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// The longest control line the daemon will read.
pub const MAX_CONTROL_BYTES: usize = 8 * 1024;

/// How many requests one control connection may carry.
///
/// The daemon answers and then keeps reading instead of closing on top of its
/// own reply: on this platform a close that races the peer's read can reach the
/// peer as a reset, and a reset discards bytes the peer has already been sent.
/// Waiting for the peer to close removes that race for every client, and this
/// bound is what stops a peer from holding a task open forever by staying
/// connected.
pub const MAX_CONTROL_REQUESTS_PER_CONNECTION: usize = 8;

/// What the daemon wrote to its endpoint file.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonEndpoint {
    pub schema_version: u16,
    pub address: String,
    /// The token a control request must present. Written to the endpoint file
    /// with owner-only permissions and never printed to a log.
    pub token: String,
    /// The process that wrote it, so a stale file can be told from a live one.
    pub pid: u32,
    pub started_at_unix_ms: i64,
}

/// Why a daemon refused to start.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StartRefusal {
    /// Another daemon holds the store's writer fence.
    AlreadyRunning { address: String },
    /// The endpoint file names a process that is gone and could not be cleaned.
    StaleEndpoint { detail: String },
}

impl StartRefusal {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::AlreadyRunning { .. } => "daemon_already_running",
            Self::StaleEndpoint { .. } => "daemon_endpoint_stale",
        }
    }

    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::AlreadyRunning { address } => {
                format!("a daemon is already serving {address}")
            }
            Self::StaleEndpoint { detail } => format!("stale daemon endpoint: {detail}"),
        }
    }
}

/// Whether a process id is still alive.
///
/// Used only to decide whether an endpoint file is stale. It is deliberately
/// conservative: an unknown answer is treated as "alive", because cleaning up
/// the endpoint of a running daemon would let a second one start.
#[must_use]
pub fn process_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(windows)]
    {
        // `tasklist` is the portable check on Windows; a missing tool answers
        // "alive", which is the safe direction.
        let output = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output();
        match output {
            Ok(output) => String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()),
            Err(_) => true,
        }
    }
    #[cfg(unix)]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = pid;
        true
    }
}

/// Read the endpoint file, if one exists.
///
/// # Errors
/// Fails when the file exists but cannot be parsed: an unreadable endpoint is a
/// fact the caller needs, not a reason to start a second daemon over it.
pub fn read_endpoint(data_dir: &Path) -> Result<Option<DaemonEndpoint>, HarnessError> {
    let path = data_dir.join(ENDPOINT_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).map_err(|error| {
        HarnessError::new(
            ErrorCode::ConfigReadError,
            format!("cannot read {}: {error}", path.display()),
        )
    })?;
    let endpoint: DaemonEndpoint = serde_json::from_str(&text).map_err(|_| {
        HarnessError::new(
            ErrorCode::ConfigParseError,
            "the daemon endpoint file is not valid JSON",
        )
    })?;
    Ok(Some(endpoint))
}

/// Write the endpoint file with owner-only permissions.
///
/// # Errors
/// Fails when the file cannot be written.
pub fn write_endpoint(data_dir: &Path, endpoint: &DaemonEndpoint) -> Result<(), HarnessError> {
    let path = data_dir.join(ENDPOINT_FILE);
    let text = serde_json::to_string_pretty(endpoint).map_err(|_| {
        HarnessError::new(
            ErrorCode::InvalidPayload,
            "the endpoint is not serializable",
        )
    })?;
    std::fs::write(&path, text).map_err(|error| {
        HarnessError::new(
            ErrorCode::StorageWriteFailed,
            format!("cannot write {}: {error}", path.display()),
        )
    })?;
    restrict_to_owner(&path)?;
    Ok(())
}

/// Remove the endpoint file, but only if this process wrote it.
///
/// # Errors
/// Fails when the file exists and belongs to another process.
pub fn remove_endpoint(data_dir: &Path, pid: u32) -> Result<bool, HarnessError> {
    let Some(endpoint) = read_endpoint(data_dir)? else {
        return Ok(false);
    };
    if endpoint.pid != pid {
        return Err(HarnessError::new(
            ErrorCode::PolicyDenied,
            "the endpoint belongs to another process; refusing to remove it",
        ));
    }
    let path = data_dir.join(ENDPOINT_FILE);
    std::fs::remove_file(&path).map_err(|error| {
        HarnessError::new(
            ErrorCode::StorageWriteFailed,
            format!("cannot remove {}: {error}", path.display()),
        )
    })?;
    Ok(true)
}

#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> Result<(), HarnessError> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)
        .map_err(|error| {
            HarnessError::new(
                ErrorCode::StorageWriteFailed,
                format!("cannot read endpoint metadata: {error}"),
            )
        })?
        .permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(path, permissions).map_err(|error| {
        HarnessError::new(
            ErrorCode::StorageWriteFailed,
            format!("cannot restrict the endpoint file: {error}"),
        )
    })
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)] // One signature for both platforms.
fn restrict_to_owner(_path: &Path) -> Result<(), HarnessError> {
    // Windows inherits the directory's ACL, which is the user's own profile
    // directory for a default data root. Recorded rather than assumed away.
    Ok(())
}

/// The running daemon.
pub struct DaemonHost {
    pub endpoint: DaemonEndpoint,
    /// Set by a shutdown request, or by the caller dropping the host.
    pub shutdown: Arc<Notify>,
    stopping: Arc<AtomicBool>,
}

impl DaemonHost {
    /// Ask the daemon to stop: cancel, drain and checkpoint.
    pub fn request_shutdown(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.shutdown.notify_waiters();
    }

    #[must_use]
    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }
}

/// What the daemon is doing, as a control client sees it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonStatus {
    pub pid: u32,
    pub started_at_unix_ms: i64,
    pub schedules: usize,
    pub active: usize,
    pub occurrences_launched: u64,
    pub stopping: bool,
}

/// Start the daemon: take the store's writer fence, publish an endpoint, and
/// serve control requests until asked to stop.
///
/// # Errors
/// Fails when a daemon already holds the store, or when the endpoint cannot be
/// published.
pub async fn start(
    data_dir: impl AsRef<Path>,
    clock: Arc<dyn Clock>,
) -> Result<(DaemonHost, DaemonRunner), HarnessError> {
    let data_dir = data_dir.as_ref().to_path_buf();
    std::fs::create_dir_all(&data_dir).map_err(|error| {
        HarnessError::new(
            ErrorCode::StorageOpenFailed,
            format!("cannot create {}: {error}", data_dir.display()),
        )
    })?;

    // An endpoint that names a live process means a daemon is running. One that
    // names a dead process is cleaned up; one that cannot be read is reported.
    if let Some(existing) = read_endpoint(&data_dir)? {
        if process_is_alive(existing.pid) {
            return Err(HarnessError::new(
                ErrorCode::RuntimeCommandConflict,
                StartRefusal::AlreadyRunning {
                    address: existing.address.clone(),
                }
                .describe(),
            ));
        }
        let path = data_dir.join(ENDPOINT_FILE);
        std::fs::remove_file(&path).map_err(|error| {
            HarnessError::new(
                ErrorCode::StorageWriteFailed,
                StartRefusal::StaleEndpoint {
                    detail: format!("cannot remove {}: {error}", path.display()),
                }
                .describe(),
            )
        })?;
    }

    // The writer fence is the real mutual exclusion. A second daemon does not
    // get its own lock: it is told the store is taken.
    let store = SqliteStore::open_writer(WriterOpenOptions::new(&data_dir, HostId::generate()))
        .await
        .map_err(|error| {
            HarnessError::new(
                ErrorCode::RuntimeCommandConflict,
                format!("the daemon could not take the store: {error}"),
            )
        })?;
    let store = Arc::new(store);

    let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|error| {
        HarnessError::new(
            ErrorCode::ServiceUnavailable,
            format!("the daemon control socket did not bind: {error}"),
        )
    })?;
    let address = listener
        .local_addr()
        .map_err(|error| {
            HarnessError::new(
                ErrorCode::ServiceUnavailable,
                format!("the control socket has no address: {error}"),
            )
        })?
        .to_string();
    let endpoint = DaemonEndpoint {
        schema_version: 1,
        address: address.clone(),
        token: new_control_token(),
        pid: std::process::id(),
        started_at_unix_ms: clock.now_unix_ms(),
    };
    write_endpoint(&data_dir, &endpoint)?;

    let shutdown = Arc::new(Notify::new());
    let stopping = Arc::new(AtomicBool::new(false));
    let host = DaemonHost {
        endpoint: endpoint.clone(),
        shutdown: Arc::clone(&shutdown),
        stopping: Arc::clone(&stopping),
    };

    let runner = DaemonRunner {
        store: Arc::clone(&store),
        clock,
        data_dir: data_dir.clone(),
        listener,
        endpoint: endpoint.clone(),
        shutdown: Arc::clone(&shutdown),
        stopping: Arc::clone(&stopping),
        occurrences_launched: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        drained: Vec::new(),
        external: None,
    };
    Ok((host, runner))
}

/// The daemon's own loop: evaluate schedules, claim occurrences, serve control.
pub struct DaemonRunner {
    store: Arc<SqliteStore>,
    clock: Arc<dyn Clock>,
    data_dir: PathBuf,
    listener: TcpListener,
    endpoint: DaemonEndpoint,
    shutdown: Arc<Notify>,
    stopping: Arc<AtomicBool>,
    occurrences_launched: Arc<std::sync::atomic::AtomicU64>,
    drained: Vec<String>,
    /// The external-task worker, when this daemon was given transports to reach
    /// remote servers with. Without it, external jobs stay exactly as durable as
    /// they were: recorded, and waiting.
    external: Option<Arc<ExternalTaskRunner>>,
}

/// One schedule evaluation, as the loop sees it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchedOccurrence {
    pub schedule_id: String,
    pub occurrence_key: String,
    pub due_unix_ms: i64,
    pub manual: bool,
}

impl DaemonRunner {
    /// Give this daemon the transports it needs to drive external jobs.
    ///
    /// The runner is built from the daemon's own store and clock, so external
    /// work shares the writer fence and the injected time of everything else
    /// here. Returns the worker, which is also how a caller submits, cancels or
    /// reconciles a job outside the loop.
    pub fn attach_external_tasks(
        &mut self,
        remotes: Arc<dyn TaskRemoteResolver>,
    ) -> Arc<ExternalTaskRunner> {
        let runner = Arc::new(ExternalTaskRunner::new(
            Arc::clone(&self.store),
            remotes,
            Arc::clone(&self.clock),
        ));
        self.external = Some(Arc::clone(&runner));
        runner
    }

    /// Evaluate every active schedule once and claim what is due.
    ///
    /// Claiming is the only side effect, and it is durable: a caller that
    /// launches the returned occurrences after this returns is the same shape a
    /// real launch has, and a crash between the claim and the launch is the
    /// crash the claim exists to survive.
    ///
    /// # Errors
    /// Fails when the store refuses, or when a schedule cannot be evaluated.
    pub async fn evaluate_once(&self) -> Result<Vec<LaunchedOccurrence>, HarnessError> {
        let now = self.clock.now_unix_ms();
        let mut launched = Vec::new();
        for stored in self
            .store
            .list_schedules()
            .await
            .map_err(|error| store_error(&error))?
        {
            if stored.state != ScheduleState::Active.as_str() {
                continue;
            }
            let spec: ScheduleSpec = serde_json::from_str(&stored.spec_json).map_err(|_| {
                HarnessError::new(
                    ErrorCode::InvalidPayload,
                    "a stored schedule specification is invalid",
                )
            })?;
            let schedule = crate::daemon::Schedule {
                schedule_id: stored.schedule_id.clone(),
                title: stored.title.clone(),
                spec,
                state: ScheduleState::Active,
                revision: stored.revision,
                next_due_unix_ms: stored.next_due_unix_ms,
                grants: serde_json::from_str(&stored.grants_json).map_err(|_| {
                    HarnessError::new(
                        ErrorCode::InvalidPayload,
                        "a stored schedule grant is invalid",
                    )
                })?,
            };
            let decision = due_now(&schedule, now, MisfirePolicy::CatchUpBounded)?;
            for due in decision.due {
                let occurrence = StoredOccurrenceRecord {
                    occurrence_key: occurrence_key(&schedule.schedule_id, schedule.revision, due),
                    schedule_id: schedule.schedule_id.clone(),
                    revision: schedule.revision,
                    due_unix_ms: due,
                    claimed_at_unix_ms: now,
                    state: crate::daemon::OccurrenceState::Claimed.as_str().to_owned(),
                    trigger_kind: "scheduled".to_owned(),
                };
                // A stale revision is not an error here: a pause that landed
                // between the read and the claim is exactly what the revision
                // check is for, and the loop moves on.
                match self
                    .store
                    .claim_occurrence(&occurrence, decision.next_due_unix_ms)
                    .await
                {
                    Ok(true) => {
                        self.occurrences_launched.fetch_add(1, Ordering::SeqCst);
                        launched.push(LaunchedOccurrence {
                            schedule_id: schedule.schedule_id.clone(),
                            occurrence_key: occurrence.occurrence_key,
                            due_unix_ms: due,
                            manual: false,
                        });
                    }
                    Ok(false) => {}
                    Err(error) if error.code() == ErrorCode::SequenceConflict => {}
                    Err(error) => return Err(store_error(&error)),
                }
            }
        }
        Ok(launched)
    }

    /// Claim one manual occurrence, leaving the next due untouched.
    ///
    /// # Errors
    /// Fails when the schedule is unknown or not active.
    pub async fn trigger_manual(
        &self,
        schedule_id: &str,
    ) -> Result<LaunchedOccurrence, HarnessError> {
        let stored = self
            .store
            .schedule(schedule_id)
            .await
            .map_err(|error| store_error(&error))?
            .ok_or_else(|| HarnessError::new(ErrorCode::TaskNotFound, "no such schedule"))?;
        if stored.state != ScheduleState::Active.as_str() {
            return Err(HarnessError::new(
                ErrorCode::PolicyDenied,
                "the schedule is not active",
            ));
        }
        let now = self.clock.now_unix_ms();
        // The manual occurrence's nominal instant is now, which is what makes it
        // its own key; `next_due_unix_ms` is passed through unchanged.
        let occurrence = StoredOccurrenceRecord {
            occurrence_key: occurrence_key(schedule_id, stored.revision, now),
            schedule_id: schedule_id.to_owned(),
            revision: stored.revision,
            due_unix_ms: now,
            claimed_at_unix_ms: now,
            state: crate::daemon::OccurrenceState::Claimed.as_str().to_owned(),
            trigger_kind: "manual".to_owned(),
        };
        let claimed = self
            .store
            .claim_occurrence(&occurrence, stored.next_due_unix_ms)
            .await
            .map_err(|error| store_error(&error))?;
        if !claimed {
            return Err(HarnessError::new(
                ErrorCode::IdempotencyConflict,
                "this manual occurrence was already claimed",
            ));
        }
        self.occurrences_launched.fetch_add(1, Ordering::SeqCst);
        Ok(LaunchedOccurrence {
            schedule_id: schedule_id.to_owned(),
            occurrence_key: occurrence.occurrence_key,
            due_unix_ms: now,
            manual: true,
        })
    }

    /// Recover occurrences a previous host claimed and never launched.
    ///
    /// # Errors
    /// Fails when the store refuses.
    pub async fn recover(&mut self) -> Result<Vec<String>, HarnessError> {
        let recovered = self
            .store
            .recover_claimed_occurrences()
            .await
            .map_err(|error| store_error(&error))?;
        self.drained.extend(recovered.iter().cloned());
        Ok(recovered)
    }

    #[must_use]
    pub fn occurrences_launched(&self) -> u64 {
        self.occurrences_launched.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn drained(&self) -> &[String] {
        &self.drained
    }

    async fn status(&self) -> Result<DaemonStatus, HarnessError> {
        let schedules = self
            .store
            .list_schedules()
            .await
            .map_err(|error| store_error(&error))?;
        let active = schedules
            .iter()
            .filter(|schedule| schedule.state == ScheduleState::Active.as_str())
            .count();
        Ok(DaemonStatus {
            pid: self.endpoint.pid,
            started_at_unix_ms: self.endpoint.started_at_unix_ms,
            schedules: schedules.len(),
            active,
            occurrences_launched: self.occurrences_launched(),
            stopping: self.stopping.load(Ordering::SeqCst),
        })
    }

    /// Serve control requests and evaluate schedules until asked to stop.
    ///
    /// The two are one loop on purpose: a control request must not be able to
    /// starve schedule evaluation, and a schedule evaluation must not block a
    /// control client for longer than one tick.
    ///
    /// # Errors
    /// Fails when the store refuses during shutdown.
    pub async fn run(mut self) -> Result<DaemonRunReport, HarnessError> {
        self.recover().await?;
        // External work is recovered the same way schedules are: a handle that
        // was recorded before the stop resumes polling, and a submission with no
        // answer waits for a human instead of being sent again.
        if let Some(external) = &self.external {
            external.recover().await?;
        }
        let tick = Duration::from_millis(250);
        loop {
            if self.stopping.load(Ordering::SeqCst) {
                break;
            }
            tokio::select! {
                accepted = self.listener.accept() => {
                    if let Ok((stream, _peer)) = accepted {
                        let token = self.endpoint.token.clone();
                        let status = self.status().await?;
                        let store = Arc::clone(&self.store);
                        tokio::spawn(async move {
                            let _ = serve_control(stream, &token, status, store).await;
                        });
                    }
                }
                () = self.shutdown.notified() => break,
                () = tokio::time::sleep(tick) => {
                    self.evaluate_once().await?;
                    // The external worker is a consumer of the same tick, not a
                    // loop of its own: one clock, one store, one place where the
                    // daemon's work is bounded.
                    if let Some(external) = &self.external {
                        external.poll_due().await?;
                    }
                }
            }
        }
        // Shutdown: settle every claim this daemon made but did not launch,
        // then checkpoint the store, then remove the endpoint it owns.
        //
        // A claim is durable and a launch is not, so stopping between them would
        // leave a row that says "claimed". The next host cannot tell whether the
        // effect happened and must not launch again, so the honest settlement is
        // `canceled` - recorded here, while the daemon still knows it made the
        // claim, rather than left for a later host to guess at.
        let canceled = self
            .store
            .recover_claimed_occurrences()
            .await
            .map_err(|error| store_error(&error))?;
        self.drained.extend(canceled);
        let report = DaemonRunReport {
            occurrences_launched: self.occurrences_launched(),
            recovered: self.drained.clone(),
        };
        let data_dir = self.data_dir.clone();
        let pid = self.endpoint.pid;
        // The store handle is dropped before the endpoint is removed, so a
        // client that reads the endpoint and then opens the store does not race
        // the writer lock this daemon still holds. A control task that cloned
        // the handle keeps the lock until it finishes, which is why the close
        // is attempted rather than asserted.
        if let Ok(store) = Arc::try_unwrap(Arc::clone(&self.store)) {
            let _ = store.close().await;
        }
        drop(self.store);
        let _ = remove_endpoint(&data_dir, pid);
        Ok(report)
    }
}

/// What a stopped daemon reports.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonRunReport {
    pub occurrences_launched: u64,
    pub recovered: Vec<String>,
}

async fn serve_control(
    stream: TcpStream,
    token: &str,
    status: DaemonStatus,
    store: Arc<SqliteStore>,
) -> Result<(), HarnessError> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    for _ in 0..MAX_CONTROL_REQUESTS_PER_CONNECTION {
        let mut line = String::new();
        // Bounded: a client that sends half a line and stops does not hold this
        // task, and a client that sends a megabyte is refused.
        let read = tokio::time::timeout(CONTROL_READ_TIMEOUT, reader.read_line(&mut line)).await;
        // Whether the conversation continues after this reply.
        let mut more = false;
        let response = match read {
            // The peer closed. There is nothing to answer and nobody to answer.
            Ok(Ok(0)) => break,
            Err(_) => control_error("control_timeout", "the control request timed out"),
            Ok(Err(error)) => control_error("control_read_failed", &error.to_string()),
            Ok(Ok(_)) if line.len() > MAX_CONTROL_BYTES => {
                control_error("control_too_large", "the control request is too large")
            }
            Ok(Ok(_)) => {
                let parsed: Result<ControlEnvelope, _> = serde_json::from_str(&line);
                match parsed {
                    Err(_) => control_error("control_malformed", "the control request is not JSON"),
                    Ok(envelope) if envelope.token() != token => control_error(
                        "control_unauthorized",
                        "the control token is not this daemon's",
                    ),
                    Ok(envelope) => match envelope {
                        ControlEnvelope::Status { .. } => {
                            more = true;
                            json!({
                                "status": "ok",
                                "daemon": {
                                    "pid": status.pid,
                                    "started_at_unix_ms": status.started_at_unix_ms,
                                    "schedules": status.schedules,
                                    "active": status.active,
                                    "occurrences_launched": status.occurrences_launched,
                                    "stopping": status.stopping,
                                },
                            })
                        }
                        ControlEnvelope::Trigger { schedule_id, .. } => {
                            more = true;
                            let now = SystemClock.now_unix_ms();
                            let stored = store
                                .schedule(&schedule_id)
                                .await
                                .map_err(|error| store_error(&error));
                            match stored {
                                Err(error) => control_error(error.code().as_str(), error.message()),
                                Ok(None) => control_error("task_not_found", "no such schedule"),
                                Ok(Some(schedule)) => json!({
                                    "schema_version": 1,
                                    "status": "ok",
                                    "triggered": {
                                        "schedule_id": schedule_id,
                                        "revision": schedule.revision,
                                        "requested_at_unix_ms": now,
                                        "next_due_unix_ms": schedule.next_due_unix_ms,
                                    },
                                }),
                            }
                        }
                        // Asking the daemon to stop ends the conversation: the
                        // next thing this peer sees is the process stopping.
                        ControlEnvelope::Shutdown { .. } => json!({
                            "schema_version": 1,
                            "status": "ok",
                            "shutdown": "accepted",
                        }),
                    },
                }
            }
        };
        let mut body = serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_owned());
        body.push('\n');
        // A reply that cannot be written means the peer left; there is no error
        // to report to a peer that is no longer there.
        if writer.write_all(body.as_bytes()).await.is_err() {
            break;
        }
        if !more {
            break;
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum ControlEnvelope {
    Status { token: String },
    Trigger { token: String, schedule_id: String },
    Shutdown { token: String },
}

impl ControlEnvelope {
    fn token(&self) -> &str {
        match self {
            Self::Status { token } | Self::Trigger { token, .. } | Self::Shutdown { token } => {
                token
            }
        }
    }
}

fn control_error(code: &str, message: &str) -> Value {
    json!({
        "schema_version": 1,
        "status": "error",
        "error": { "code": code, "safe_message": message },
    })
}

/// Send one control request and read the reply.
///
/// The whole request is retried, not only the read. On this platform a peer that
/// closes right after answering can have its close delivered as a reset before
/// the bytes reach this side, and under load that reset can arrive before the
/// request was even processed - so one failed attempt says nothing about whether
/// the daemon is healthy. The retry is bounded, and a reply that did arrive is
/// never discarded.
///
/// # Errors
/// Fails when the daemon cannot be reached, or when every attempt is refused.
pub async fn control(
    endpoint: &DaemonEndpoint,
    request: &str,
    schedule_id: Option<&str>,
) -> Result<Value, HarnessError> {
    let envelope = match schedule_id {
        Some(schedule_id) => json!({
            "token": endpoint.token,
            "command": request,
            "schedule_id": schedule_id,
        }),
        None => json!({ "token": endpoint.token, "command": request }),
    };
    let mut line = serde_json::to_string(&envelope).map_err(|_| {
        HarnessError::new(
            ErrorCode::InvalidPayload,
            "the control request is not serializable",
        )
    })?;
    line.push('\n');
    let mut last = None;
    for attempt in 0..CONTROL_ATTEMPTS {
        match control_once(endpoint, &line).await {
            Ok(value) => return Ok(value),
            Err(error) => {
                // Only a transport-level failure is retried. A typed refusal
                // from the daemon is an answer, and retrying it would turn one
                // wrong token into eight.
                let retryable = matches!(error.code(), ErrorCode::ServiceUnavailable);
                last = Some(error);
                if !retryable || attempt + 1 == CONTROL_ATTEMPTS {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
    Err(last.unwrap_or_else(|| {
        HarnessError::new(
            ErrorCode::ServiceUnavailable,
            "the daemon did not answer the control request",
        )
    }))
}

/// How many times one control request may be sent.
pub const CONTROL_ATTEMPTS: usize = 8;

/// One attempt: connect, send the line, read one reply line.
async fn control_once(endpoint: &DaemonEndpoint, line: &str) -> Result<Value, HarnessError> {
    let stream = TcpStream::connect(&endpoint.address)
        .await
        .map_err(|error| {
            HarnessError::new(
                ErrorCode::ServiceUnavailable,
                format!("the daemon at {} is unreachable: {error}", endpoint.address),
            )
        })?;
    let (reader, mut writer) = stream.into_split();
    writer.write_all(line.as_bytes()).await.map_err(|error| {
        HarnessError::new(
            ErrorCode::ServiceUnavailable,
            format!("the control request was not written: {error}"),
        )
    })?;
    let reply = read_control_line(BufReader::new(reader)).await?;
    serde_json::from_str(&reply).map_err(|_| {
        HarnessError::new(
            ErrorCode::InvalidPayload,
            "the daemon control reply is not JSON",
        )
    })
}

/// Read one newline-terminated reply, tolerating a reset that arrives first.
///
/// A reset with nothing read yet is retried rather than reported as a missing
/// answer: the bytes may still be in flight. A reset after a partial line is a
/// real truncation, and the reply is returned so the JSON parse reports it.
async fn read_control_line<R>(mut reader: R) -> Result<String, HarnessError>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let deadline = tokio::time::Instant::now() + CONTROL_READ_TIMEOUT;
    let mut reply = String::new();
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(CONTROL_READ_TIMEOUT, reader.read_line(&mut reply)).await {
            Ok(Ok(_)) => break,
            Ok(Err(_)) => {
                if !reply.trim().is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(_) => {
                return Err(HarnessError::new(
                    ErrorCode::ServiceUnavailable,
                    "the daemon did not answer the control request",
                ));
            }
        }
    }
    if reply.trim().is_empty() {
        return Err(HarnessError::new(
            ErrorCode::ServiceUnavailable,
            "the daemon closed the control connection without answering",
        ));
    }
    Ok(reply)
}

/// A control token from the process's own entropy.
fn new_control_token() -> String {
    format!(
        "{}{}",
        harness_types::EventId::generate()
            .as_str()
            .trim_start_matches("event_")
            .replace('-', ""),
        harness_types::EventId::generate()
            .as_str()
            .trim_start_matches("event_")
            .replace('-', "")
    )
}

fn store_error(error: &harness_store_sqlite::StoreError) -> HarnessError {
    HarnessError::new(error.code(), error.to_string())
}
