//! Sleep: the durable queue of ha's background memory passes.
//!
//! Memory is a chain of tiers kept at different speeds: the turns of a
//! conversation, the conversation's own store, the global memory, the
//! harness's evolved rules. A faster tier must be consolidated into the next
//! one before it is reset or overwritten, or what it held is lost (the "Sleep"
//! paper, arXiv 2606.03979). The passes that do it - a conversation's review,
//! Deep Dream's consolidation, the harness evolution - each call a model, and
//! a model call fails: a provider is down, a limit is reached, the process
//! dies. So every pass is a job on disk first, and what triggered it (a
//! turn's count, a compaction, a session's end) is reset only once the job is
//! written:
//!
//! - a job is one JSON file under `<data>/sleep/`, written whole or not at all;
//! - jobs with one key coalesce: two reviews of one conversation become one
//!   job that reads the newer span, so a compaction inside another's spacing
//!   waits for the job (its `next_at`) instead of being skipped;
//! - a claim holds the key's lock file for as long as the pass runs: a second
//!   process skips the key, and a claim whose process died is claimable again
//!   because its lock went with it. A running claim renews its [`LEASE`]
//!   every [`RENEW_EVERY`], up to [`MAX_RUN`]; a claim whose lease lapsed (a
//!   process that hung) is taken over, and the pass it was running can then
//!   neither remove nor rewrite the job: its done and its failure act only
//!   on the claim they hold;
//! - a pass that succeeded removes its job; one that failed keeps it, counts
//!   the attempt and waits a doubling backoff (capped at a day); after
//!   [`max_attempts`] failures in a row it gives up, stays on disk for
//!   `ha doctor` and the bug report to show, and is not tried again by itself.
//!
//! Each step is a `sleep` diagnostics record: `enqueued`, `claimed`, `done`,
//! `failed`, `gave_up`, with the job's kind, its attempts and its error class.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The queue's directory under the data directory.
pub const DIR: &str = "sleep";

/// The lock every change to the queue holds for a moment.
const QUEUE_LOCK: &str = "queue.lock";

/// How long a change waits for another process's change to the queue. A
/// change holds the lock for a listing and a write; a wait this long means
/// the holder hung.
const QUEUE_WAIT: Duration = Duration::from_secs(3);

/// How long a claim stands unless it is renewed. A running claim renews it
/// every [`RENEW_EVERY`]; one that lapsed is taken over even while its lock
/// is held, by a process that hung.
pub const LEASE: chrono::TimeDelta = chrono::TimeDelta::minutes(30);

/// How often a running claim renews its lease.
const RENEW_EVERY: Duration = Duration::from_mins(5);

/// How long a pass may run before its claim stops renewing: a review retries
/// a model call of at most five minutes a few times, and longer than this is
/// a pass that hung.
const MAX_RUN: Duration = Duration::from_hours(2);

/// How many failures in a row a job takes before it gives up
/// (`HA_SLEEP_MAX_ATTEMPTS` sets it).
pub const MAX_ATTEMPTS: u32 = 6;

/// The longest a failed job waits before it is tried again.
const MAX_BACKOFF_SECONDS: i64 = 86_400;

/// What a job does.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A conversation's automatic review: the review gate, then the
    /// refinement it approves.
    #[default]
    Review,
    /// Deep Dream's consolidation of a memory scope.
    Consolidation,
    /// The harness evolution from the calls that failed.
    Evolution,
    /// Closing the memory nobody used (`memory_validity::expire_due`).
    Expiry,
    /// The wiki pass over a knowledge library (`knowledge::wiki`): dangling
    /// links repointed or filled, pages on one subject merged, the index,
    /// claims checked against their sources.
    Wiki,
}

impl Kind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::Consolidation => "consolidation",
            Self::Evolution => "evolution",
            Self::Expiry => "expiry",
            Self::Wiki => "wiki",
        }
    }

    /// The wait after a first failure, doubled after each next one. A review
    /// is worth trying again soon, while its conversation is fresh; a
    /// consolidation or an evolution had a day to run anyway.
    const fn first_backoff_seconds(self) -> i64 {
        match self {
            Self::Review => 300,
            Self::Consolidation | Self::Evolution | Self::Expiry | Self::Wiki => 3_600,
        }
    }
}

/// Who holds a job while its pass runs.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Claim {
    pub pid: u32,
    /// When the claim lapses, in milliseconds since the epoch.
    pub until: i64,
    /// This claim, among the claims of the job: what its done and its failure
    /// check before they touch the job.
    #[serde(default)]
    pub token: String,
}

/// One background pass waiting to run, running, or given up.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct Job {
    pub id: String,
    pub kind: Kind,
    /// Jobs with one key coalesce, and one of them runs at a time.
    pub key: String,
    /// What asked for it last: `turn`, `compaction`, `idle`, `session_end`,
    /// `exec`, `due`.
    pub trigger: String,
    /// A review's project store directory: its journal holds the turns.
    pub project: Option<String>,
    pub task: Option<String>,
    /// The newest session a review reads.
    pub session: Option<String>,
    /// Whether that session is read whole, so a review marks it reviewed; a
    /// compaction's review reads a turn still running and marks `before`.
    pub whole: bool,
    pub before: Option<String>,
    pub turns: u32,
    /// Learned skills the reviewed turns used.
    pub in_play: Vec<String>,
    /// A consolidation's harness scope directory.
    pub scope: Option<String>,
    /// Failures in a row.
    pub attempts: u32,
    /// Milliseconds since the epoch.
    pub enqueued_at: i64,
    /// Not before this, in milliseconds since the epoch.
    pub next_at: i64,
    pub last_error: Option<String>,
    pub error_class: Option<String>,
    pub claimed: Option<Claim>,
    pub gave_up: bool,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl Job {
    fn new(kind: Kind, key: String, trigger: &str) -> Self {
        let now = now_ms();
        Self {
            kind,
            key,
            trigger: trigger.to_owned(),
            enqueued_at: now,
            next_at: now,
            ..Self::default()
        }
    }

    /// A review of the conversation `task` up to `session`, read whole.
    #[must_use]
    pub fn review(
        project: &Path,
        task: &str,
        session: &str,
        trigger: &str,
        turns: u32,
        in_play: Vec<String>,
    ) -> Self {
        Self {
            project: Some(project.display().to_string()),
            task: Some(task.to_owned()),
            session: Some(session.to_owned()),
            whole: true,
            turns,
            in_play,
            ..Self::new(Kind::Review, format!("review:{task}"), trigger)
        }
    }

    /// The session is still running: once reviewed, only the conversation up
    /// to `before` is marked reviewed.
    #[must_use]
    pub fn partial(mut self, before: Option<String>) -> Self {
        self.whole = false;
        self.before = before;
        self
    }

    /// Not run before `at`.
    #[must_use]
    pub fn not_before(mut self, at: chrono::DateTime<chrono::Utc>) -> Self {
        self.next_at = self.next_at.max(at.timestamp_millis());
        self
    }

    /// Deep Dream's consolidation of the scope directory `scope`.
    #[must_use]
    pub fn consolidation(scope: &Path) -> Self {
        Self {
            scope: Some(scope.display().to_string()),
            ..Self::new(
                Kind::Consolidation,
                format!("consolidation:{}", scope.display()),
                "due",
            )
        }
    }

    /// The harness evolution, once a day.
    #[must_use]
    pub fn evolution() -> Self {
        Self::new(Kind::Evolution, "evolution".to_owned(), "due")
    }

    /// The expiry pass over the memory, keyed by the global store `scope`.
    #[must_use]
    pub fn expiry(scope: &Path) -> Self {
        Self {
            scope: Some(scope.display().to_string()),
            ..Self::new(Kind::Expiry, format!("expiry:{}", scope.display()), "due")
        }
    }

    /// The wiki pass over the knowledge library at `root`, one per library;
    /// `trigger` is what made it due (`review`, `due`).
    #[must_use]
    pub fn wiki(root: &Path, trigger: &str) -> Self {
        Self {
            scope: Some(root.display().to_string()),
            ..Self::new(Kind::Wiki, format!("wiki:{}", root.display()), trigger)
        }
    }

    /// Whether `pid`'s claim stands at `now`, judging the lease alone.
    fn lease_stands(&self, now: i64) -> bool {
        self.claimed.as_ref().is_some_and(|claim| claim.until > now)
    }

    /// Take `other`'s work into this job: the newer span is read (a session
    /// read whole stays whole), the turns add up, and the skills in play join.
    /// `sooner` takes the earlier of the two times - new work brings a retry
    /// forward - else the later stands (a failure's backoff is kept).
    fn absorb(&mut self, other: &Self, sooner: bool) {
        if other.session == self.session {
            self.whole |= other.whole;
            if self.before.is_none() {
                self.before.clone_from(&other.before);
            }
        } else if other.enqueued_at >= self.enqueued_at {
            self.session.clone_from(&other.session);
            self.whole = other.whole;
            self.before.clone_from(&other.before);
            self.trigger.clone_from(&other.trigger);
        }
        // The newer job names the store as it is now: a store that moved
        // (`ha project rekey`, a repository found again) left this job its
        // old directory.
        if other.enqueued_at >= self.enqueued_at && other.project.is_some() {
            self.project.clone_from(&other.project);
        }
        self.turns = self.turns.saturating_add(other.turns);
        for name in &other.in_play {
            if !self.in_play.contains(name) {
                self.in_play.push(name.clone());
            }
        }
        self.next_at = if sooner {
            self.next_at.min(other.next_at)
        } else {
            self.next_at.max(other.next_at)
        };
        self.enqueued_at = self.enqueued_at.max(other.enqueued_at);
        self.attempts = self.attempts.max(other.attempts);
        if self.last_error.is_none() {
            self.last_error.clone_from(&other.last_error);
            self.error_class.clone_from(&other.error_class);
        }
    }

    fn record(&self, builder: harness_types::diag::Builder) -> harness_types::diag::Builder {
        let builder = builder
            .field("job", self.kind.as_str())
            .field("key", self.key.as_str())
            .field("trigger", self.trigger.as_str())
            .field("attempts", self.attempts)
            .field("turns", self.turns);
        let builder = match &self.session {
            Some(session) => builder.field("span", session.as_str()),
            None => builder,
        };
        match &self.task {
            Some(task) => builder.task(task),
            None => builder,
        }
    }
}

/// Whether `job` is of the project whose store is `store_dir`: it names that
/// directory, or one this store took over (its manifest's `adopted` keys,
/// beside it) that no longer holds a store. A store moved after its job was
/// written is found again, so its job is not stranded.
#[must_use]
pub fn belongs_to(job: &Job, store_dir: &Path) -> bool {
    let Some(project) = job.project.as_deref().map(Path::new) else {
        return false;
    };
    if project == store_dir {
        return true;
    }
    let (Some(parent), Some(name)) = (project.parent(), project.file_name()) else {
        return false;
    };
    store_dir.parent() == Some(parent)
        && !project
            .join(harness_store_sqlite::DATABASE_FILE_NAME)
            .is_file()
        && super::adoption::manifest(store_dir).is_some_and(|manifest| {
            manifest
                .adopted
                .iter()
                .any(|key| std::ffi::OsStr::new(key) == name)
        })
}

/// The queue's directory under `data_dir`.
#[must_use]
pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join(DIR)
}

fn job_path(data_dir: &Path, id: &str) -> PathBuf {
    dir(data_dir).join(format!("{id}.json"))
}

/// A file name for `key`: its letters and digits, with a hash that keeps two
/// keys apart when they spell alike. The hash is SHA-256, the same in every
/// ha build: two builds name one key's lock alike.
fn key_name(key: &str) -> String {
    let hash = harness_types::ContentHash::from_bytes(key.as_bytes());
    let digits = hash.as_str().trim_start_matches("sha256:");
    let readable = key
        .chars()
        .rev()
        .filter(char::is_ascii_alphanumeric)
        .take(24)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("{readable}-{}", &digits[..16])
}

fn key_lock(data_dir: &Path, key: &str) -> PathBuf {
    dir(data_dir).join(format!("{}.lock", key_name(key)))
}

fn lock_queue(data_dir: &Path) -> Result<File, String> {
    super::files::hold(
        &dir(data_dir).join(QUEUE_LOCK),
        QUEUE_WAIT,
        "background memory queue",
    )
}

fn save(data_dir: &Path, job: &Job) -> Result<(), String> {
    let text = serde_json::to_string_pretty(job).map_err(|error| error.to_string())?;
    super::files::write_atomic(&job_path(data_dir, &job.id), &text)
}

/// Every job on disk, oldest first. A file that cannot be read is left out.
#[must_use]
pub fn jobs(data_dir: &Path) -> Vec<Job> {
    let Ok(entries) = std::fs::read_dir(dir(data_dir)) else {
        return Vec::new();
    };
    let mut jobs = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .filter_map(|path| {
            let job = serde_json::from_str::<Job>(&std::fs::read_to_string(&path).ok()?).ok()?;
            // The file's name is the job's id, whatever the text says.
            let id = path.file_stem()?.to_string_lossy().into_owned();
            Some(Job { id, ..job })
        })
        .collect::<Vec<_>>();
    jobs.sort_by(|left, right| (left.enqueued_at, &left.id).cmp(&(right.enqueued_at, &right.id)));
    jobs
}

/// How many failures a job takes before it gives up: [`MAX_ATTEMPTS`], or
/// `HA_SLEEP_MAX_ATTEMPTS` (at least one).
#[must_use]
pub fn max_attempts() -> u32 {
    std::env::var("HA_SLEEP_MAX_ATTEMPTS")
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .map_or(MAX_ATTEMPTS, |attempts| attempts.max(1))
}

/// The wait after a job's `attempts`-th failure in a row: the kind's first
/// wait (or `HA_SLEEP_RETRY_SECONDS`), doubled each time, at most a day.
#[must_use]
pub fn backoff(kind: Kind, attempts: u32) -> chrono::TimeDelta {
    let first = std::env::var("HA_SLEEP_RETRY_SECONDS")
        .ok()
        .and_then(|value| value.trim().parse::<i64>().ok())
        .map_or(kind.first_backoff_seconds(), |seconds| seconds.max(1));
    let doublings = attempts.saturating_sub(1).min(20);
    let seconds = first
        .saturating_mul(1_i64 << doublings)
        .min(MAX_BACKOFF_SECONDS);
    chrono::TimeDelta::seconds(seconds)
}

/// The class of a failed pass's error, for the records and the reports: the
/// provider's stable error code when the error carries one (ha's model-call
/// errors end with `<code>: <message>`), `timeout` or `truncated` for the two
/// failures ha's upkeep calls name themselves, else `other`.
#[must_use]
pub fn error_class(error: &str) -> String {
    if error.starts_with(super::refine::CALL_TIMED_OUT) {
        return "timeout".to_owned();
    }
    if error == super::refine::TRUNCATED_JSON_ERROR {
        return "truncated".to_owned();
    }
    error
        .split(": ")
        .find_map(|part| {
            serde_json::from_value::<harness_types::ErrorCode>(serde_json::Value::String(
                part.trim().to_owned(),
            ))
            .ok()
        })
        .map_or_else(|| "other".to_owned(), |code| code.as_str().to_owned())
}

/// Write `job` to the queue, coalesced into a waiting job of its key when
/// there is one (not into one running: what it covers was read already). A
/// key whose job gave up takes no new consolidation or evolution - they would
/// fail the same way, every turn - while a review of new turns is a new job.
/// Returns the job as it stands on disk.
///
/// # Errors
/// The queue cannot be locked or the job cannot be written: what asked for
/// it keeps its own count, so nothing is lost.
pub fn enqueue(data_dir: &Path, job: Job) -> Result<Job, String> {
    let _queue = lock_queue(data_dir)?;
    let same = jobs(data_dir)
        .into_iter()
        .filter(|other| other.key == job.key)
        .collect::<Vec<_>>();
    if job.kind != Kind::Review
        && let Some(given_up) = same.iter().find(|other| other.gave_up)
    {
        return Ok(given_up.clone());
    }
    // An upkeep pass is one pass: asked for again while it runs, the running
    // one stands for it. A second job written then waited a day behind it,
    // reported by `ha doctor` though nothing was owed.
    if job.kind != Kind::Review
        && let Some(running) = same
            .iter()
            .find(|other| !claim_is_free(data_dir, other, now_ms()))
    {
        return Ok(running.clone());
    }
    let waiting = same
        .into_iter()
        .find(|other| !other.gave_up && claim_is_free(data_dir, other, now_ms()));
    let (job, coalesced) = if let Some(mut waiting) = waiting {
        waiting.claimed = None;
        // New turns bring a review that failed forward; an upkeep pass,
        // asked for again at every turn's end, keeps its backoff.
        waiting.absorb(&job, job.kind == Kind::Review);
        (waiting, true)
    } else {
        let id = format!(
            "{}-{}",
            key_name(&job.key),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        (Job { id, ..job }, false)
    };
    save(data_dir, &job)?;
    job.record(harness_types::diag::info("sleep", "enqueued"))
        .field("coalesced", coalesced)
        .emit();
    Ok(job)
}

/// Whether a job nobody runs: never claimed, or claimed by a process whose
/// lock is gone (it died), or by one past its lease.
fn claim_is_free(data_dir: &Path, job: &Job, now: i64) -> bool {
    if job.claimed.is_none() || !job.lease_stands(now) {
        return true;
    }
    key_free(data_dir, &job.key)
}

/// Whether nobody holds `key`'s lock.
fn key_free(data_dir: &Path, key: &str) -> bool {
    super::files::open_lock(&key_lock(data_dir, key), "background memory job")
        .and_then(|file| super::files::try_take(&file, "background memory job"))
        .unwrap_or(false)
}

/// A job this process runs. Its key's lock is held until it is done, failed
/// or dropped; dropped (a canceled run), the job waits for the next claim with
/// no attempt counted.
pub struct Claimed {
    pub job: Job,
    data_dir: PathBuf,
    token: String,
    lock: Option<File>,
    /// Set when the claim ends, which stops its renewal.
    ended: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for Claimed {
    fn drop(&mut self) {
        self.ended.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Renew the lease of the claim `token` holds on job `id`, while it is still
/// that claim. `false` when the job is gone or another claim took it.
fn renew(data_dir: &Path, id: &str, token: &str) -> bool {
    let Ok(_queue) = lock_queue(data_dir) else {
        return true; // try again at the next renewal
    };
    let Some(mut job) = read_job(data_dir, id) else {
        return false;
    };
    match job.claimed.as_mut() {
        Some(claim) if claim.token == token => {
            claim.until = now_ms() + LEASE.num_milliseconds();
            save(data_dir, &job).is_ok()
        }
        _ => false,
    }
}

fn read_job(data_dir: &Path, id: &str) -> Option<Job> {
    let job =
        serde_json::from_str::<Job>(&std::fs::read_to_string(job_path(data_dir, id)).ok()?).ok()?;
    Some(Job {
        id: id.to_owned(),
        ..job
    })
}

/// Renew the claim's lease every [`RENEW_EVERY`] until it ends, it is no
/// longer this claim, or [`MAX_RUN`] passed: a thread of its own, so a pass
/// that waits on a model call keeps its claim without asking.
fn keep_renewed(
    data_dir: PathBuf,
    id: String,
    token: String,
    ended: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    let started = std::time::Instant::now();
    let _ = std::thread::Builder::new()
        .name("sleep-lease".to_owned())
        .spawn(move || {
            let mut last = std::time::Instant::now();
            while !ended.load(std::sync::atomic::Ordering::SeqCst) && started.elapsed() < MAX_RUN {
                std::thread::sleep(Duration::from_millis(500));
                if last.elapsed() >= RENEW_EVERY {
                    last = std::time::Instant::now();
                    if ended.load(std::sync::atomic::Ordering::SeqCst)
                        || !renew(&data_dir, &id, &token)
                    {
                        return;
                    }
                }
            }
        });
}

/// Claim the first due job `pick` accepts, of a key nobody else runs: due
/// (`next_at` passed), not given up, and free (see [`enqueue`]). One job of a
/// key runs at a time, across processes.
///
/// # Errors
/// The queue cannot be locked, or the claim cannot be written.
pub fn claim(data_dir: &Path, pick: impl Fn(&Job) -> bool) -> Result<Option<Claimed>, String> {
    let _queue = lock_queue(data_dir)?;
    let now = now_ms();
    let mut candidates = jobs(data_dir)
        .into_iter()
        .filter(|job| !job.gave_up && job.next_at <= now && pick(job))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|job| job.next_at);
    let mut skipped = std::collections::BTreeSet::new();
    for mut job in candidates {
        if skipped.contains(&job.key) {
            continue;
        }
        let file = super::files::open_lock(&key_lock(data_dir, &job.key), "background memory job")?;
        let lock = if super::files::try_take(&file, "background memory job")? {
            Some(file)
        } else if job.claimed.is_some() && !job.lease_stands(now) {
            // Held past its lease: the process that holds it hung.
            None
        } else {
            skipped.insert(job.key.clone());
            continue;
        };
        let token = format!(
            "{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        job.claimed = Some(Claim {
            pid: std::process::id(),
            until: now + LEASE.num_milliseconds(),
            token: token.clone(),
        });
        save(data_dir, &job)?;
        job.record(harness_types::diag::info("sleep", "claimed"))
            .emit();
        let ended = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        keep_renewed(
            data_dir.to_path_buf(),
            job.id.clone(),
            token.clone(),
            std::sync::Arc::clone(&ended),
        );
        return Ok(Some(Claimed {
            job,
            data_dir: data_dir.to_path_buf(),
            token,
            lock,
            ended,
        }));
    }
    Ok(None)
}

/// Write `job` and claim it when it is due and free: what an upkeep pass
/// (a consolidation, the evolution) does at a turn's end. `None` when it waits
/// out a backoff, gave up, or another process runs it.
#[must_use]
pub fn claim_upkeep(data_dir: &Path, job: Job) -> Option<Claimed> {
    let key = job.key.clone();
    match enqueue(data_dir, job) {
        Ok(job) if job.gave_up => return None,
        Ok(_) => {}
        Err(error) => {
            harness_types::diag::warn("sleep", "enqueue_failed")
                .field("key", key.as_str())
                .msg(error)
                .emit();
            return None;
        }
    }
    claim(data_dir, |job| job.key == key).ok().flatten()
}

impl Claimed {
    /// Whether the job on disk is still held by this claim: `None` when it is
    /// gone, `Some(false)` when another claim took it over.
    fn still_mine(&self) -> Option<bool> {
        read_job(&self.data_dir, &self.job.id)
            .map(|job| job.claimed.is_some_and(|claim| claim.token == self.token))
    }

    /// The pass succeeded (or found nothing to do): the job is removed, unless
    /// another claim took it over meanwhile (then that one decides). The key's
    /// lock file goes with the key's last job.
    pub fn done(mut self) {
        self.ended.store(true, std::sync::atomic::Ordering::SeqCst);
        let removed = lock_queue(&self.data_dir).and_then(|_queue| {
            let outcome = match self.still_mine() {
                Some(false) => Ok(false),
                None => Ok(true),
                Some(true) => std::fs::remove_file(job_path(&self.data_dir, &self.job.id))
                    .or_else(|error| {
                        if error.kind() == std::io::ErrorKind::NotFound {
                            Ok(())
                        } else {
                            Err(error)
                        }
                    })
                    .map(|()| true)
                    .map_err(|error| error.to_string()),
            };
            // The lock file is opened only under the queue's lock, so once
            // this handle lets go nobody holds it.
            drop(self.lock.take());
            if !jobs(&self.data_dir)
                .iter()
                .any(|other| other.key == self.job.key)
            {
                let _ = std::fs::remove_file(key_lock(&self.data_dir, &self.job.key));
            }
            outcome
        });
        let record = self.job.record(harness_types::diag::info("sleep", "done"));
        match removed {
            Ok(true) => record.emit(),
            Ok(false) => record.field("superseded", true).emit(),
            Err(error) => record.field("kept", true).msg(error).emit(),
        }
    }

    /// The pass failed with `error`: the job stays, its attempt counted, and
    /// waits [`backoff`]; after [`max_attempts`] failures it gives up. A job
    /// of its key that came in meanwhile takes this one's span, keeping this
    /// one's backoff.
    pub fn failed(self, error: &str) {
        self.ended.store(true, std::sync::atomic::Ordering::SeqCst);
        let mut job = self.job.clone();
        job.claimed = None;
        job.attempts = job.attempts.saturating_add(1);
        job.last_error = Some(error.chars().take(500).collect());
        job.error_class = Some(error_class(error));
        let gave_up = job.attempts >= max_attempts();
        if gave_up {
            job.gave_up = true;
        } else {
            job.next_at = now_ms() + backoff(job.kind, job.attempts).num_milliseconds();
        }
        let result = lock_queue(&self.data_dir).and_then(|_queue| {
            // Taken over (this claim lapsed) or done by the claim that took
            // it: that claim decides, and this failure writes nothing.
            if self.still_mine() != Some(true) {
                return Err("superseded by another claim".to_owned());
            }
            let newer = (!gave_up)
                .then(|| {
                    jobs(&self.data_dir).into_iter().find(|other| {
                        other.key == job.key
                            && other.id != job.id
                            && !other.gave_up
                            && other.claimed.is_none()
                    })
                })
                .flatten();
            match newer {
                Some(mut newer) => {
                    newer.absorb(&job, false);
                    save(&self.data_dir, &newer)?;
                    std::fs::remove_file(job_path(&self.data_dir, &job.id))
                        .map_err(|error| error.to_string())
                }
                None => save(&self.data_dir, &job),
            }
        });
        let record = job.record(if gave_up {
            harness_types::diag::warn("sleep", "gave_up")
        } else {
            harness_types::diag::warn("sleep", "failed")
        });
        let record = record
            .field("class", job.error_class.clone().unwrap_or_default())
            .field("next_at", job.next_at)
            .msg(job.last_error.clone().unwrap_or_default());
        match result {
            Ok(()) => record.emit(),
            Err(write) => record.field("write_error", write).emit(),
        }
    }
}

/// Remove a job of `key` that gave up: its pass is no longer owed (it is not
/// due, so it succeeded another way, `/refine --curate` say), and the
/// doctor's warning goes with it.
pub fn forget_given_up(data_dir: &Path, key: &str) {
    forget_given_up_after(data_dir, key, Duration::ZERO);
}

/// [`forget_given_up`] once the job gave up at least `age` ago (when its file
/// was last written): a pass still due after its cool-off is owed afresh,
/// as the wiki pass of a library that keeps its work. A job that gave up was
/// otherwise never tried again while its pass stayed due.
pub fn forget_given_up_after(data_dir: &Path, key: &str, age: Duration) {
    let given_up = jobs(data_dir)
        .into_iter()
        .filter(|job| job.key == key && job.gave_up)
        .filter(|job| {
            age.is_zero()
                || std::fs::metadata(job_path(data_dir, &job.id))
                    .and_then(|metadata| metadata.modified())
                    .ok()
                    .and_then(|modified| modified.elapsed().ok())
                    .is_some_and(|elapsed| elapsed >= age)
        })
        .collect::<Vec<_>>();
    if given_up.is_empty() {
        return;
    }
    let Ok(_queue) = lock_queue(data_dir) else {
        return;
    };
    for job in given_up {
        if std::fs::remove_file(job_path(data_dir, &job.id)).is_ok() {
            job.record(harness_types::diag::info("sleep", "forgotten"))
                .emit();
        }
    }
}

/// [`enqueue`] without waiting: when another process holds the queue's lock,
/// the job is written as a new file of its own (to coalesce at the next
/// change), so a caller that cannot wait - a session ending - loses nothing.
///
/// # Errors
/// The job cannot be written.
pub fn enqueue_now(data_dir: &Path, job: Job) -> Result<(), String> {
    let lock = super::files::open_lock(&dir(data_dir).join(QUEUE_LOCK), "background memory queue")?;
    if super::files::try_take(&lock, "background memory queue")? {
        drop(lock);
        return enqueue(data_dir, job).map(|_| ());
    }
    let id = format!(
        "{}-{}",
        key_name(&job.key),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let job = Job { id, ..job };
    save(data_dir, &job)?;
    job.record(harness_types::diag::info("sleep", "enqueued"))
        .field("coalesced", false)
        .emit();
    Ok(())
}

/// Where a session that ended before 0.8.9 left its review, in the project's
/// data directory.
const LEGACY_DEFERRED_DIR: &str = "refine-pending";

/// Move the reviews an older ha left in `project_dir` (`refine-pending/`)
/// into the queue. Each file goes only once its job is written.
pub fn import_legacy(data_dir: &Path, project_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(project_dir.join(LEGACY_DEFERRED_DIR)) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let claimed = path.with_extension(format!("claimed-{}", std::process::id()));
        if std::fs::rename(&path, &claimed).is_err() {
            continue; // another process took it first
        }
        let value = std::fs::read_to_string(&claimed)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
        let Some(value) = value else {
            let _ = std::fs::remove_file(&claimed);
            continue;
        };
        let (Some(task), Some(session)) = (value["task"].as_str(), value["session"].as_str())
        else {
            let _ = std::fs::remove_file(&claimed);
            continue;
        };
        let job = Job::review(
            project_dir,
            task,
            session,
            "session_end",
            value["turns"]
                .as_u64()
                .and_then(|turns| u32::try_from(turns).ok())
                .unwrap_or(0),
            value["in_play"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect(),
        );
        if enqueue(data_dir, job).is_ok() {
            let _ = std::fs::remove_file(&claimed);
        } else {
            let _ = std::fs::rename(&claimed, &path);
        }
    }
}

/// What `ha doctor` says of the queue.
#[derive(Clone, Debug, Default)]
pub struct Summary {
    /// Jobs waiting to run or running, that never failed.
    pub waiting: usize,
    /// Jobs that failed and wait to be tried again: `kind (attempts, class)`.
    pub retrying: Vec<String>,
    /// Jobs that gave up: `kind (attempts, class: error)`.
    pub gave_up: Vec<String>,
    /// The queue's directory.
    pub dir: PathBuf,
    /// The last expiry pass: when it ran and how many unused entries it
    /// closed (`memory_validity::expire_due`).
    pub expiry: Option<(String, u64)>,
}

/// The queue under `data_dir`, for `ha doctor`.
#[must_use]
pub fn summary(data_dir: &Path) -> Summary {
    let mut summary = Summary {
        dir: dir(data_dir),
        expiry: super::memory_validity::last_expiry(&super::harness::global_dir(data_dir)),
        ..Summary::default()
    };
    for job in jobs(data_dir) {
        let what = match &job.task {
            Some(task) => format!("{} of {task}", job.kind.as_str()),
            None => job.kind.as_str().to_owned(),
        };
        let class = job.error_class.clone().unwrap_or_default();
        if job.gave_up {
            summary.gave_up.push(format!(
                "{what} ({} failures, {class}: {}; {})",
                job.attempts,
                job.last_error
                    .as_deref()
                    .unwrap_or_default()
                    .chars()
                    .take(160)
                    .collect::<String>(),
                job_path(data_dir, &job.id).display()
            ));
        } else if job.attempts > 0 {
            summary
                .retrying
                .push(format!("{what} ({} failed, {class})", job.attempts));
        } else {
            summary.waiting += 1;
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::{Job, Kind, backoff, claim, enqueue, error_class, jobs};

    fn review(task: &str, session: &str, turns: u32) -> Job {
        Job::review(
            std::path::Path::new("project"),
            task,
            session,
            "turn",
            turns,
            Vec::new(),
        )
    }

    #[test]
    fn a_failed_job_stays_counts_its_attempt_and_waits() {
        let directory = tempfile::tempdir().expect("dir");
        let data = directory.path();
        enqueue(data, review("task-1", "s1", 3)).expect("enqueued");
        let claimed = claim(data, |_| true).expect("claim").expect("due");
        claimed.failed("the refinement model call failed: service_unavailable: HTTP 503");
        let kept = jobs(data);
        assert_eq!(kept.len(), 1, "the failed review is kept");
        assert_eq!(kept[0].attempts, 1);
        assert_eq!(kept[0].error_class.as_deref(), Some("service_unavailable"));
        assert!(kept[0].claimed.is_none());
        assert!(
            claim(data, |_| true).expect("claim").is_none(),
            "it waits out its backoff"
        );
    }

    #[test]
    fn a_done_job_is_removed() {
        let directory = tempfile::tempdir().expect("dir");
        let data = directory.path();
        enqueue(data, review("task-1", "s1", 3)).expect("enqueued");
        claim(data, |_| true).expect("claim").expect("due").done();
        assert!(jobs(data).is_empty());
    }

    #[test]
    fn reviews_of_one_conversation_coalesce_into_the_newer_span() {
        let directory = tempfile::tempdir().expect("dir");
        let data = directory.path();
        let later = chrono::Utc::now() + chrono::TimeDelta::minutes(20);
        enqueue(
            data,
            review("task-1", "s1", 2)
                .partial(Some("s0".to_owned()))
                .not_before(later),
        )
        .expect("compaction");
        enqueue(data, review("task-1", "s2", 3)).expect("turn");
        enqueue(data, review("task-2", "t1", 1)).expect("another conversation");
        let kept = jobs(data);
        assert_eq!(kept.len(), 2);
        let first = kept
            .iter()
            .find(|job| job.key == "review:task-1")
            .expect("one");
        assert_eq!(first.session.as_deref(), Some("s2"));
        assert!(first.whole);
        assert_eq!(first.turns, 5);
        assert!(
            first.next_at <= chrono::Utc::now().timestamp_millis(),
            "new work brings the spaced review forward"
        );
    }

    #[test]
    fn a_running_job_takes_no_new_work_and_its_key_runs_once() {
        let directory = tempfile::tempdir().expect("dir");
        let data = directory.path();
        enqueue(data, review("task-1", "s1", 1)).expect("enqueued");
        let running = claim(data, |_| true).expect("claim").expect("due");
        enqueue(data, review("task-1", "s2", 1)).expect("more work");
        assert_eq!(jobs(data).len(), 2, "the new span is a job of its own");
        assert!(
            claim(data, |_| true).expect("claim").is_none(),
            "one job of a key at a time"
        );
        // Failed, the running job's span joins the waiting one, which keeps
        // the failure's backoff.
        running.failed("the refinement model call timed out");
        let kept = jobs(data);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].session.as_deref(), Some("s2"));
        assert_eq!(kept[0].turns, 2);
        assert_eq!(kept[0].attempts, 1);
        assert_eq!(kept[0].error_class.as_deref(), Some("timeout"));
        assert!(kept[0].next_at > chrono::Utc::now().timestamp_millis());
    }

    #[test]
    fn a_claim_whose_process_let_go_is_claimable_again() {
        let directory = tempfile::tempdir().expect("dir");
        let data = directory.path();
        enqueue(data, review("task-1", "s1", 1)).expect("enqueued");
        // Dropped without an outcome, as a run cut off or a process that died.
        drop(claim(data, |_| true).expect("claim").expect("due"));
        let again = claim(data, |_| true)
            .expect("claim")
            .expect("claimable again");
        assert_eq!(again.job.attempts, 0, "no attempt was counted");
    }

    #[test]
    fn an_upkeep_job_gives_up_after_its_attempts_and_takes_no_new_one() {
        let directory = tempfile::tempdir().expect("dir");
        let data = directory.path();
        for attempt in 1..=super::MAX_ATTEMPTS {
            let claimed = super::claim_upkeep(data, Job::evolution()).expect("claimed");
            assert_eq!(claimed.job.attempts, attempt - 1);
            claimed.failed("the refinement model call failed: rate_limited: limit");
            assert!(
                attempt == super::MAX_ATTEMPTS
                    || super::claim_upkeep(data, Job::evolution()).is_none(),
                "asked for again, it keeps its backoff"
            );
            // The backoff passed.
            for mut kept in jobs(data) {
                kept.next_at = 0;
                super::save(data, &kept).expect("due");
            }
        }
        let kept = jobs(data);
        assert_eq!(kept.len(), 1);
        assert!(kept[0].gave_up);
        assert!(super::claim_upkeep(data, Job::evolution()).is_none());
        assert_eq!(jobs(data).len(), 1, "no new job behind one that gave up");
        let summary = super::summary(data);
        assert_eq!(summary.gave_up.len(), 1);
        assert!(summary.gave_up[0].contains("rate_limited"), "{summary:?}");
    }

    #[test]
    fn the_backoff_doubles_up_to_a_day() {
        assert_eq!(backoff(Kind::Review, 1).num_seconds(), 300);
        assert_eq!(backoff(Kind::Review, 2).num_seconds(), 600);
        assert_eq!(backoff(Kind::Consolidation, 1).num_seconds(), 3_600);
        assert_eq!(backoff(Kind::Consolidation, 30).num_seconds(), 86_400);
    }

    #[test]
    fn an_error_is_classed_by_its_code() {
        assert_eq!(
            error_class(
                "the refinement model call failed: rate_limited: provider returned HTTP 429"
            ),
            "rate_limited"
        );
        assert_eq!(
            error_class("the refinement model call timed out"),
            "timeout"
        );
        assert_eq!(
            error_class("the refiner did not return a JSON object"),
            "other"
        );
    }

    #[test]
    fn a_review_an_older_ha_left_is_moved_into_the_queue() {
        let directory = tempfile::tempdir().expect("dir");
        let data = directory.path().join("data");
        let project = directory.path().join("project");
        std::fs::create_dir_all(project.join("refine-pending")).expect("dir");
        std::fs::write(
            project.join("refine-pending/task-1.json"),
            r#"{"task": "task-1", "session": "s9", "turns": 6, "in_play": ["rust-testing"]}"#,
        )
        .expect("legacy");
        super::import_legacy(&data, &project);
        let kept = jobs(&data);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].task.as_deref(), Some("task-1"));
        assert_eq!(kept[0].turns, 6);
        assert_eq!(kept[0].in_play, ["rust-testing"]);
        assert_eq!(kept[0].trigger, "session_end");
        assert_eq!(
            std::fs::read_dir(project.join("refine-pending"))
                .expect("dir")
                .count(),
            0
        );
    }

    fn store(projects: &std::path::Path, key: &str, adopted: &[&str]) -> std::path::PathBuf {
        let dir = projects.join(key);
        std::fs::create_dir_all(&dir).expect("store");
        std::fs::write(dir.join(harness_store_sqlite::DATABASE_FILE_NAME), "").expect("db");
        std::fs::write(
            dir.join("project.json"),
            serde_json::json!({"schema_version": 1, "root": "r", "adopted": adopted}).to_string(),
        )
        .expect("manifest");
        dir
    }

    /// A store moved under a new key (`ha project rekey`) keeps its review
    /// jobs: the store that took it over finds them, and a later review of the
    /// conversation coalesced into one names the store as it is now. They
    /// were stranded, and swallowed every later review.
    #[test]
    fn a_review_of_a_store_that_moved_follows_it() {
        let directory = tempfile::tempdir().expect("dir");
        let data = directory.path();
        let projects = data.join("projects");
        let old = projects.join("project-0000000000000old");
        enqueue(
            data,
            Job::review(&old, "task-1", "s1", "turn", 2, Vec::new()),
        )
        .expect("queued");
        let new = store(
            &projects,
            "project-0000000000000new",
            &["project-0000000000000old"],
        );
        let other = store(&projects, "project-00000000000other", &[]);
        let job = jobs(data).remove(0);
        assert!(super::belongs_to(&job, &new), "the store that took it over");
        assert!(!super::belongs_to(&job, &other));
        enqueue(
            data,
            Job::review(&new, "task-1", "s2", "turn", 1, Vec::new()),
        )
        .expect("more");
        let kept = jobs(data);
        assert_eq!(kept.len(), 1);
        assert_eq!(
            kept[0].project.as_deref(),
            Some(new.display().to_string().as_str())
        );
        assert_eq!(kept[0].turns, 3);
    }

    /// A claim that lapsed is taken over; the pass it was running can then
    /// neither remove the job the new claim runs nor write its failure over it.
    #[test]
    fn a_lapsed_claim_acts_only_on_itself() {
        let directory = tempfile::tempdir().expect("dir");
        let data = directory.path();
        enqueue(data, review("task-1", "s1", 1)).expect("queued");
        let first = claim(data, |_| true).expect("claim").expect("due");
        // Its lease lapsed (a process that hung).
        let mut lapsed = jobs(data).remove(0);
        if let Some(claim) = lapsed.claimed.as_mut() {
            claim.until = 0;
        }
        super::save(data, &lapsed).expect("lapsed");
        let second = claim(data, |_| true).expect("claim").expect("taken over");
        assert!(
            !super::renew(data, &first.job.id, &first.token),
            "no longer its claim"
        );
        first.done();
        assert_eq!(jobs(data).len(), 1, "the taken-over job stays");
        assert!(super::renew(data, &second.job.id, &second.token));
        let second_token = second.token.clone();
        // The first claim's failure writes nothing over the second's claim.
        let mut third = jobs(data).remove(0);
        third.claimed.as_mut().expect("claimed").until = 0;
        super::save(data, &third).expect("lapsed again");
        let mut kept = jobs(data).remove(0);
        kept.claimed.as_mut().expect("claimed").until = i64::MAX;
        super::save(data, &kept).expect("restored");
        let stale = super::Claimed {
            job: kept.clone(),
            data_dir: data.to_path_buf(),
            token: "an-old-claim".to_owned(),
            lock: None,
            ended: std::sync::Arc::default(),
        };
        stale.failed("the refinement model call timed out");
        let now = jobs(data).remove(0);
        assert_eq!(
            now.attempts, 0,
            "the failure of a lapsed claim is not counted"
        );
        assert_eq!(now.claimed.expect("claimed").token, second_token);
        second.done();
        assert!(jobs(data).is_empty());
    }

    /// A running claim renews its lease, so a long pass is not taken over.
    #[test]
    fn a_running_claim_renews_its_lease() {
        let directory = tempfile::tempdir().expect("dir");
        let data = directory.path();
        enqueue(data, review("task-1", "s1", 1)).expect("queued");
        let running = claim(data, |_| true).expect("claim").expect("due");
        let mut lapsing = jobs(data).remove(0);
        lapsing.claimed.as_mut().expect("claimed").until = 1;
        super::save(data, &lapsing).expect("about to lapse");
        assert!(super::renew(data, &running.job.id, &running.token));
        assert!(jobs(data)[0].claimed.as_ref().expect("claimed").until > 1);
        assert!(
            claim(data, |_| true).expect("claim").is_none(),
            "still running"
        );
    }

    /// An upkeep pass asked for again while it runs writes no second job,
    /// which waited a day and was reported though nothing was owed.
    #[test]
    fn an_upkeep_pass_asked_for_while_it_runs_writes_no_second_job() {
        let directory = tempfile::tempdir().expect("dir");
        let data = directory.path();
        let running = super::claim_upkeep(data, Job::evolution()).expect("claimed");
        assert!(super::claim_upkeep(data, Job::evolution()).is_none());
        assert_eq!(jobs(data).len(), 1);
        running.done();
        assert!(jobs(data).is_empty());
    }

    /// The last job of a key takes its lock file with it; the lock name is
    /// the same in every build (SHA-256, not the standard library's hasher).
    #[test]
    fn a_key_lock_goes_with_its_last_job_and_is_named_alike_by_every_build() {
        let directory = tempfile::tempdir().expect("dir");
        let data = directory.path();
        assert_eq!(
            super::key_name("review:task-1"),
            "reviewtask1-d3ec0bc7dc516cb1"
        );
        enqueue(data, review("task-1", "s1", 1)).expect("queued");
        claim(data, |_| true).expect("claim").expect("due").done();
        assert!(!super::key_lock(data, "review:task-1").exists());
    }

    /// A job that gave up is forgotten once its pass is no longer owed.
    #[test]
    fn a_job_that_gave_up_can_be_forgotten() {
        let directory = tempfile::tempdir().expect("dir");
        let data = directory.path();
        let mut job = Job::evolution();
        job.id = "evolution-1".to_owned();
        job.gave_up = true;
        super::save(data, &job).expect("gave up");
        super::forget_given_up(data, "evolution");
        assert!(jobs(data).is_empty());
    }

    /// A pass still due forgets its job that gave up only after a cool-off,
    /// counted from when the job gave up: the wiki pass of a library that
    /// keeps its work was otherwise never tried again.
    #[test]
    fn a_job_that_gave_up_is_forgotten_after_its_cool_off() {
        let directory = tempfile::tempdir().expect("dir");
        let data = directory.path();
        let mut job = Job::wiki(std::path::Path::new("lib"), "due");
        job.id = "wiki-1".to_owned();
        job.gave_up = true;
        super::save(data, &job).expect("gave up");
        let day = std::time::Duration::from_secs(86_400);
        super::forget_given_up_after(data, &job.key, day);
        assert_eq!(jobs(data).len(), 1, "it just gave up");
        let file = std::fs::File::options()
            .write(true)
            .open(super::job_path(data, &job.id))
            .expect("job file");
        file.set_modified(std::time::SystemTime::now() - 2 * day)
            .expect("backdate");
        drop(file);
        super::forget_given_up_after(data, &job.key, day);
        assert!(jobs(data).is_empty(), "a day later it is forgotten");
    }

    /// The wiki pass is a job of its library: asked for again it is one job,
    /// one library's pass runs at a time while another library's can, a
    /// failure waits an hour and doubles, and after its attempts it gives up
    /// and takes no new job until forgotten.
    #[test]
    fn a_wiki_pass_is_one_job_per_library_with_backoff_and_give_up() {
        let directory = tempfile::tempdir().expect("dir");
        let data = directory.path();
        let project = std::path::Path::new("p/.harness/knowledge");
        let global = std::path::Path::new("data/knowledge");
        let running = super::claim_upkeep(data, Job::wiki(project, "review")).expect("claimed");
        assert_eq!(running.job.kind, Kind::Wiki);
        assert_eq!(running.job.trigger, "review");
        assert!(super::claim_upkeep(data, Job::wiki(project, "due")).is_none());
        let other = super::claim_upkeep(data, Job::wiki(global, "due")).expect("another library");
        other.done();
        running.failed("the refinement model call failed: rate_limited: limit");
        let kept = jobs(data);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].attempts, 1);
        assert_eq!(backoff(Kind::Wiki, 1).num_seconds(), 3_600);
        assert!(
            super::claim_upkeep(data, Job::wiki(project, "due")).is_none(),
            "it waits out its backoff"
        );
        for _ in 1..super::MAX_ATTEMPTS {
            for mut job in jobs(data) {
                job.next_at = 0;
                super::save(data, &job).expect("due");
            }
            super::claim_upkeep(data, Job::wiki(project, "due"))
                .expect("retried")
                .failed("the refinement model call timed out");
        }
        assert!(jobs(data)[0].gave_up);
        assert!(super::claim_upkeep(data, Job::wiki(project, "due")).is_none());
        super::forget_given_up(data, &Job::wiki(project, "due").key);
        assert!(jobs(data).is_empty());
    }

    /// A session that ends while another process changes the queue still
    /// leaves its review, as a job of its own.
    #[test]
    fn a_review_written_while_the_queue_is_busy_is_not_lost() {
        let directory = tempfile::tempdir().expect("dir");
        let data = directory.path();
        let busy = super::lock_queue(data).expect("busy");
        super::enqueue_now(data, review("task-1", "s1", 4)).expect("written");
        drop(busy);
        assert_eq!(jobs(data).len(), 1);
        assert_eq!(jobs(data)[0].turns, 4);
    }
}
