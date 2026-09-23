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

use harness_store_sqlite::{
    OutboxCounts, SqliteStore, StoredApproval, StoredOccurrenceRecord, WriterOpenOptions,
};
use harness_types::{ErrorCode, HarnessError, HostId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::Notify,
};

use crate::daemon::{
    APPROVAL_WINDOW_MS, Clock, ExternalTaskRunner, MisfirePolicy, NotificationConnector,
    NotificationEvent, NotificationOutbox, ScheduleSpec, ScheduleState, SystemClock,
    TaskRemoteResolver, WaitingResolution, due_now, occurrence_key, requires_approval,
    resolve_waiting,
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
    // This process is necessarily alive, even when the host policy prevents
    // querying the process table (as it does in some Windows sandboxes).
    if pid == std::process::id() {
        return true;
    }
    #[cfg(windows)]
    {
        // `tasklist` is the portable check on Windows. A missing tool, access
        // denial, failed command, or unrecognized output is unknown and must
        // answer "alive": deleting an endpoint on an unknown result could
        // allow a second daemon to race the writer fence.
        let output = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output();
        output
            .ok()
            .and_then(|output| {
                tasklist_pid_status(
                    pid,
                    output.status.success(),
                    &String::from_utf8_lossy(&output.stdout),
                    &String::from_utf8_lossy(&output.stderr),
                )
            })
            .unwrap_or(true)
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

#[cfg(windows)]
fn tasklist_pid_status(pid: u32, success: bool, stdout: &str, stderr: &str) -> Option<bool> {
    if !success || !stderr.trim().is_empty() {
        return None;
    }
    for line in stdout.lines() {
        // CSV rows start with the image name and then the PID. Compare the
        // complete second field, not a substring that could confuse PID 12
        // with PID 123.
        if let Some((_, rest)) = line
            .trim()
            .strip_prefix('"')
            .and_then(|line| line.split_once("\",\""))
            && let Some((reported_pid, _)) = rest.split_once("\",\"")
            && reported_pid.parse::<u32>().ok() == Some(pid)
        {
            return Some(true);
        }
    }
    let lines = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    (!lines.is_empty()
        && lines
            .iter()
            .all(|line| line.to_ascii_uppercase().starts_with("INFO:")))
    .then_some(false)
}

#[cfg(all(test, windows))]
mod process_probe_tests {
    use super::tasklist_pid_status;

    #[test]
    fn exact_csv_pid_is_alive_and_nonmatching_substrings_are_not_matches() {
        assert_eq!(
            tasklist_pid_status(
                123,
                true,
                "\"ha.exe\",\"123\",\"Console\",\"1\",\"10,000 K\"",
                ""
            ),
            Some(true)
        );
        assert_eq!(
            tasklist_pid_status(
                12,
                true,
                "\"ha.exe\",\"123\",\"Console\",\"1\",\"10,000 K\"",
                ""
            ),
            None
        );
    }

    #[test]
    fn only_an_explicit_no_match_is_dead_and_unknown_results_stay_unknown() {
        assert_eq!(
            tasklist_pid_status(
                123,
                true,
                "INFO: No tasks are running which match the specified criteria.",
                ""
            ),
            Some(false)
        );
        assert_eq!(
            tasklist_pid_status(123, true, "ERROR: Access denied", ""),
            None
        );
        assert_eq!(tasklist_pid_status(123, true, "", "access denied"), None);
        assert_eq!(tasklist_pid_status(123, true, "", ""), None);
        assert_eq!(tasklist_pid_status(123, false, "", ""), None);
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
    /// Occurrences held for a human decision, with what is being asked and when
    /// the window closes. A scheduled run that cannot proceed is visible here
    /// rather than silent.
    pub waiting: Vec<WaitingOccurrence>,
    pub outbox: OutboxCounts,
}

/// One occurrence the daemon is holding for a decision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WaitingOccurrence {
    pub occurrence_key: String,
    pub schedule_id: String,
    pub revision: u64,
    pub due_unix_ms: i64,
    pub prompt: String,
    pub expires_at_unix_ms: i64,
    /// What the daemon will do at the end of the window.
    pub next_action: String,
}

/// What resolving one waiting occurrence did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedWaiting {
    pub occurrence_key: String,
    /// `launched`, `skipped`, `expired` or `canceled`.
    pub outcome: String,
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

    let outbox = Arc::new(NotificationOutbox::new(
        Arc::clone(&store),
        None,
        Arc::clone(&clock),
    ));
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
        outbox,
        control_tasks: tokio::task::JoinSet::new(),
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
    /// The notification outbox. It exists from the start and, until a connector
    /// is attached, it records without sending anything.
    outbox: Arc<NotificationOutbox>,
    /// The control connections this daemon is serving. Every one of them holds a
    /// store handle, so shutdown waits for them instead of leaving the writer
    /// fence behind.
    control_tasks: tokio::task::JoinSet<()>,
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
    /// Give this daemon somewhere to send notifications.
    ///
    /// Until this is called the outbox records and sends nothing, which is the
    /// honest default: a host that was never told where a notification goes must
    /// not guess. Returns the outbox, so a caller can also deliver by hand.
    pub fn attach_notifications(
        &mut self,
        connector: Option<Arc<dyn NotificationConnector>>,
    ) -> Arc<NotificationOutbox> {
        let outbox = Arc::new(NotificationOutbox::new(
            Arc::clone(&self.store),
            connector,
            Arc::clone(&self.clock),
        ));
        self.outbox = Arc::clone(&outbox);
        outbox
    }

    #[must_use]
    pub fn outbox(&self) -> &Arc<NotificationOutbox> {
        &self.outbox
    }

    /// Hold one claimed occurrence for a human decision.
    ///
    /// The occurrence is already claimed, so nothing can launch it twice; this
    /// makes the wait durable (an approval row with a window) and visible (a
    /// notification, and the daemon's own status).
    async fn hold_for_approval(
        &self,
        schedule: &crate::daemon::Schedule,
        occurrence: &StoredOccurrenceRecord,
        due_unix_ms: i64,
        now: i64,
    ) -> Result<(), HarnessError> {
        let approval = StoredApproval {
            approval_id: format!("approval_{}", occurrence.occurrence_key),
            occurrence_key: occurrence.occurrence_key.clone(),
            schedule_id: schedule.schedule_id.clone(),
            revision: schedule.revision,
            state: "open".to_owned(),
            prompt: format!(
                "the schedule {} wants to edit a workspace for the occurrence due at {due_unix_ms}",
                schedule.schedule_id
            ),
            requested_at_unix_ms: now,
            expires_at_unix_ms: now.saturating_add(APPROVAL_WINDOW_MS),
            decided_at_unix_ms: None,
            decided_by: None,
            reason: None,
        };
        self.store
            .request_approval(&approval)
            .await
            .map_err(|error| store_error(&error))?;
        self.store
            .settle_occurrence(&occurrence.occurrence_key, "waiting")
            .await
            .map_err(|error| store_error(&error))?;
        self.notify(NotificationEvent {
            subject_kind: "occurrence".to_owned(),
            subject_id: occurrence.occurrence_key.clone(),
            kind: "schedule_approval_requested".to_owned(),
            payload: json!({
                "schema_version": 1,
                "occurrence_key": occurrence.occurrence_key,
                "schedule_id": schedule.schedule_id,
                "revision": schedule.revision,
                "due_unix_ms": due_unix_ms,
                "expires_at_unix_ms": approval.expires_at_unix_ms,
                "next_action": "a human approves or denies it; otherwise the window closes",
            }),
        })
        .await
    }

    /// Resolve every waiting occurrence whose decision or window has arrived.
    ///
    /// # Errors
    /// Fails when the store refuses.
    pub async fn resolve_approvals(&self) -> Result<Vec<ResolvedWaiting>, HarnessError> {
        let now = self.clock.now_unix_ms();
        let mut resolved = Vec::new();
        for occurrence in self
            .store
            .waiting_occurrences()
            .await
            .map_err(|error| store_error(&error))?
        {
            let Some(approval) = self
                .store
                .approval(&occurrence.occurrence_key)
                .await
                .map_err(|error| store_error(&error))?
            else {
                continue;
            };
            let resolution = resolve_waiting(&approval, now);
            if resolution == WaitingResolution::Wait {
                continue;
            }
            // A pause or delete that landed while the occurrence waited makes it
            // stale, exactly as it would a claim: the decision is honoured as a
            // cancellation rather than a launch.
            let current = self
                .store
                .schedule(&occurrence.schedule_id)
                .await
                .map_err(|error| store_error(&error))?;
            let stale = current.is_none_or(|stored| {
                stored.revision != occurrence.revision
                    || stored.state != ScheduleState::Active.as_str()
            });
            let outcome = match (resolution, stale) {
                (_, true) => "canceled",
                (WaitingResolution::Launch, false) => "launched",
                (WaitingResolution::Skip, false) => "skipped",
                (WaitingResolution::Expire, false) => "expired",
                (WaitingResolution::Wait, false) => continue,
            };
            if outcome == "expired" {
                self.store
                    .expire_approval(&occurrence.occurrence_key, now)
                    .await
                    .map_err(|error| store_error(&error))?;
            }
            if !self
                .store
                .settle_waiting_occurrence(&occurrence.occurrence_key, outcome)
                .await
                .map_err(|error| store_error(&error))?
            {
                // Something else settled it first; that settlement stands.
                continue;
            }
            if outcome == "launched" {
                self.occurrences_launched.fetch_add(1, Ordering::SeqCst);
            }
            self.notify(NotificationEvent {
                subject_kind: "occurrence".to_owned(),
                subject_id: occurrence.occurrence_key.clone(),
                kind: format!("schedule_approval_{outcome}"),
                payload: json!({
                    "schema_version": 1,
                    "occurrence_key": occurrence.occurrence_key,
                    "schedule_id": occurrence.schedule_id,
                    "revision": occurrence.revision,
                    "due_unix_ms": occurrence.due_unix_ms,
                    "outcome": outcome,
                }),
            })
            .await?;
            resolved.push(ResolvedWaiting {
                occurrence_key: occurrence.occurrence_key,
                outcome: outcome.to_owned(),
            });
        }
        Ok(resolved)
    }

    /// Record one meaningful change in the outbox.
    async fn notify(&self, event: NotificationEvent) -> Result<(), HarnessError> {
        let _recorded = self.outbox.notify(&event).await?;
        Ok(())
    }

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
                        // Claimed is not launched. A launch that would edit a
                        // workspace has nobody attached to approve it, so it is
                        // held - durably, and visibly - instead of being run or
                        // silently skipped.
                        if requires_approval(&schedule.grants) {
                            self.hold_for_approval(&schedule, &occurrence, due, now)
                                .await?;
                            continue;
                        }
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
        // The waiting list is what a client acts on: which run is held, what is
        // being asked, when the window closes, and what happens if nobody
        // answers.
        let mut waiting = Vec::new();
        for occurrence in self
            .store
            .waiting_occurrences()
            .await
            .map_err(|error| store_error(&error))?
        {
            let approval = self
                .store
                .approval(&occurrence.occurrence_key)
                .await
                .map_err(|error| store_error(&error))?;
            let (prompt, expires_at_unix_ms) = approval.map_or_else(
                || ("waiting for a decision".to_owned(), 0),
                |approval| (approval.prompt, approval.expires_at_unix_ms),
            );
            waiting.push(WaitingOccurrence {
                occurrence_key: occurrence.occurrence_key,
                schedule_id: occurrence.schedule_id,
                revision: occurrence.revision,
                due_unix_ms: occurrence.due_unix_ms,
                prompt,
                expires_at_unix_ms,
                next_action: "expires at the end of its window unless a human decides".to_owned(),
            });
        }
        let outbox = self
            .store
            .outbox_counts()
            .await
            .map_err(|error| store_error(&error))?;
        Ok(DaemonStatus {
            pid: self.endpoint.pid,
            started_at_unix_ms: self.endpoint.started_at_unix_ms,
            schedules: schedules.len(),
            active,
            occurrences_launched: self.occurrences_launched(),
            stopping: self.stopping.load(Ordering::SeqCst),
            waiting,
            outbox,
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
                        // Tracked, not detached: a control task holds a store
                        // handle, and a task that outlived this loop would keep
                        // the writer lock alive after the daemon stopped - so the
                        // next host would be refused for as long as the task's
                        // read timeout lasts.
                        self.control_tasks.spawn(async move {
                            let _ = serve_control(stream, &token, status, store).await;
                        });
                    }
                }
                () = self.shutdown.notified() => break,
                () = tokio::time::sleep(tick) => {
                    self.evaluate_once().await?;
                    // A waiting occurrence is resolved on the same tick, so an
                    // approval a human just gave is acted on without a second
                    // mechanism to keep in step.
                    self.resolve_approvals().await?;
                    // The external worker is a consumer of the same tick, not a
                    // loop of its own: one clock, one store, one place where the
                    // daemon's work is bounded.
                    if let Some(external) = &self.external {
                        external.poll_due().await?;
                    }
                    // Notifications are delivered last: a change is recorded
                    // before anything tries to report it.
                    self.outbox.deliver_due().await?;
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
        //
        // Control tasks are stopped first: they hold store handles, and a reply
        // that is still being written is not worth a writer lock that outlives
        // the daemon.
        self.control_tasks.shutdown().await;
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
                    Ok(envelope) => {
                        let (response, keep_going) =
                            answer_control(envelope, &status, &store).await;
                        more = keep_going;
                        response
                    }
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

/// Answer one authenticated control request, and say whether the conversation
/// continues on the same connection.
async fn answer_control(
    envelope: ControlEnvelope,
    status: &DaemonStatus,
    store: &SqliteStore,
) -> (Value, bool) {
    match envelope {
        ControlEnvelope::Status { .. } => (
            json!({
                "status": "ok",
                "daemon": {
                    "pid": status.pid,
                    "started_at_unix_ms": status.started_at_unix_ms,
                    "schedules": status.schedules,
                    "active": status.active,
                    "occurrences_launched": status.occurrences_launched,
                    "stopping": status.stopping,
                    "waiting": status.waiting,
                    "outbox": {
                        "pending": status.outbox.pending,
                        "delivered": status.outbox.delivered,
                        "failed": status.outbox.failed,
                        "canceled": status.outbox.canceled,
                    },
                },
            }),
            true,
        ),
        ControlEnvelope::Trigger { schedule_id, .. } => {
            (answer_trigger(store, &schedule_id).await, true)
        }
        // A decision on a waiting occurrence. It goes through the daemon because
        // the daemon is the only writer: a client asks, and the daemon records
        // what a human decided.
        ControlEnvelope::Decide {
            occurrence_key,
            decision,
            ..
        } => (answer_decide(store, &occurrence_key, &decision).await, true),
        // Asking the daemon to stop ends the conversation: the next thing this
        // peer sees is the process stopping.
        ControlEnvelope::Shutdown { .. } => (
            json!({
                "schema_version": 1,
                "status": "ok",
                "shutdown": "accepted",
            }),
            false,
        ),
    }
}

/// Answer a manual trigger by naming the schedule it would have moved.
async fn answer_trigger(store: &SqliteStore, schedule_id: &str) -> Value {
    let now = SystemClock.now_unix_ms();
    let stored = store
        .schedule(schedule_id)
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

/// Record one decision, or answer that there was nothing left to decide.
async fn answer_decide(store: &SqliteStore, occurrence_key: &str, decision: &str) -> Value {
    let now = SystemClock.now_unix_ms();
    let stored = store
        .approval(occurrence_key)
        .await
        .map_err(|error| store_error(&error));
    let approval = match stored {
        Err(error) => return control_error(error.code().as_str(), error.message()),
        Ok(None) => {
            return control_error(
                "task_not_found",
                "no approval is waiting for that occurrence",
            );
        }
        Ok(Some(approval)) => approval,
    };
    let recorded = store
        .decide_approval(
            occurrence_key,
            decision,
            "control",
            "decided through the control channel",
            now,
        )
        .await;
    match recorded {
        Err(error) => control_error(error.code().as_str(), error.to_string().as_str()),
        Ok(true) => json!({
            "schema_version": 1,
            "status": "ok",
            "decision": {
                "occurrence_key": occurrence_key,
                "schedule_id": approval.schedule_id,
                "state": decision,
                "decided_at_unix_ms": now,
            },
        }),
        // The row is answered already, which is not an error: the first decision
        // stands.
        Ok(false) => json!({
            "schema_version": 1,
            "status": "ok",
            "decision": {
                "occurrence_key": occurrence_key,
                "schedule_id": approval.schedule_id,
                "state": approval.state,
                "unchanged": true,
            },
        }),
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum ControlEnvelope {
    Status {
        token: String,
    },
    Trigger {
        token: String,
        schedule_id: String,
    },
    Decide {
        token: String,
        occurrence_key: String,
        decision: String,
    },
    Shutdown {
        token: String,
    },
}

impl ControlEnvelope {
    fn token(&self) -> &str {
        match self {
            Self::Status { token }
            | Self::Trigger { token, .. }
            | Self::Decide { token, .. }
            | Self::Shutdown { token } => token,
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
    control_request(endpoint, &envelope).await
}

/// Decide one waiting occurrence through the daemon that owns it.
///
/// The decision goes through the control channel because the daemon holds the
/// store's writer fence: a second process cannot write a decision, and it should
/// not be able to. `decision` is `approved` or `denied`.
///
/// # Errors
/// Fails when the daemon cannot be reached, or when the request is refused.
pub async fn decide(
    endpoint: &DaemonEndpoint,
    occurrence_key: &str,
    decision: &str,
) -> Result<Value, HarnessError> {
    let envelope = json!({
        "token": endpoint.token,
        "command": "decide",
        "occurrence_key": occurrence_key,
        "decision": decision,
    });
    control_request(endpoint, &envelope).await
}

/// Send one prepared envelope and read its reply.
async fn control_request(
    endpoint: &DaemonEndpoint,
    envelope: &Value,
) -> Result<Value, HarnessError> {
    let mut line = serde_json::to_string(envelope).map_err(|_| {
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
