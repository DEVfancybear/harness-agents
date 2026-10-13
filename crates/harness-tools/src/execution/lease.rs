//! Backend leases: an owner, a generation, and a lock the OS holds.
//!
//! The runbook's rule is that the lifecycle handle is persisted **before** the
//! execution is exposed, so a host that dies mid-flight leaves a row a later
//! host can read. This module implements both halves of that:
//!
//! * [`LeaseOwner`] takes an exclusive advisory lock on the lease's lock file and
//!   writes the row while it holds it. The lock is held by the operating system
//!   for as long as the process lives, so "is the owner still alive?" is answered
//!   by the OS rather than by a heartbeat a crashed process can no longer write
//!   and a pid that can be reused.
//! * [`reconcile_backend_leases`] may only settle a lease whose lock it can take.
//!   A lease whose owner is alive is reported as `still_owned` and **nothing**
//!   about it is touched. That is the whole safety property: local absence of a
//!   process is never enough to destroy a resource that has an owner.

use std::{
    fs::{File, OpenOptions, TryLockError},
    path::{Path, PathBuf},
};

use harness_store_sqlite::{SqliteStore, StoreError, StoredBackendLease};
use harness_types::{ErrorCode, HarnessError, HostId};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{ExecutionPlan, capability::now_unix_ms};

/// Version of the lease record shape this module writes.
pub(crate) const LEASE_SCHEMA_VERSION: u16 = 1;

/// A lease whose heartbeat is older than this is a *candidate* for
/// reconciliation. It is a filter, not a decision: the lock decides.
pub const LEASE_GRACE_MS: u64 = 30_000;

/// Suffix of the file whose exclusive lock the owner holds.
pub(crate) const LEASE_LOCK_SUFFIX: &str = ".owner.lock";

/// Everything one lease needs to be opened.
#[derive(Clone, Debug)]
pub struct LeaseRequest {
    pub session_id: String,
    pub task_id: String,
    pub tool_execution_id: String,
    pub backend: String,
    pub backend_version: String,
    pub profile: String,
    pub enforced: Vec<String>,
    pub not_claimed: Vec<String>,
    /// Where lock files live.
    pub lock_root: PathBuf,
}

impl LeaseRequest {
    /// Build a request from the plan an execution was mapped onto.
    #[must_use]
    pub fn from_plan(
        plan: &ExecutionPlan,
        session_id: &str,
        task_id: &str,
        tool_execution_id: &str,
        lock_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            session_id: session_id.to_owned(),
            task_id: task_id.to_owned(),
            tool_execution_id: tool_execution_id.to_owned(),
            backend: plan.backend.clone(),
            backend_version: plan.backend_version.clone(),
            profile: plan.profile.as_str().to_owned(),
            enforced: plan
                .enforced
                .iter()
                .map(|capability| capability.as_str().to_owned())
                .collect(),
            not_claimed: plan
                .not_claimed
                .iter()
                .map(|capability| capability.as_str().to_owned())
                .collect(),
            lock_root: lock_root.into(),
        }
    }
}

/// A live owner: the lock is held until this value is dropped or released.
#[derive(Debug)]
pub struct LeaseOwner {
    lease_id: String,
    lock_path: PathBuf,
    /// `None` once released, which is also when the OS drops the lock.
    file: Option<File>,
}

impl LeaseOwner {
    /// Take the lock, then write the lease row.
    ///
    /// The order matters: a row that exists always had an owner, and a crash
    /// between the two leaves a lock file nobody holds — which reconciliation
    /// reads as "no owner", not as "delete something".
    pub async fn open(store: &SqliteStore, request: &LeaseRequest) -> Result<Self, HarnessError> {
        let lease_id = format!(
            "lease_{}",
            HostId::generate().as_str().trim_start_matches("host_")
        );
        std::fs::create_dir_all(&request.lock_root).map_err(|error| {
            HarnessError::new(
                ErrorCode::StorageOpenFailed,
                format!(
                    "cannot create the lease lock directory {}: {error}",
                    request.lock_root.display()
                ),
            )
        })?;
        let lock_path = request
            .lock_root
            .join(format!("{lease_id}{LEASE_LOCK_SUFFIX}"));
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| {
                HarnessError::new(
                    ErrorCode::StorageOpenFailed,
                    format!("cannot open lease lock {}: {error}", lock_path.display()),
                )
            })?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(HarnessError::new(
                    ErrorCode::TaskLeaseConflict,
                    format!(
                        "another owner holds the lock for lease {lease_id}; refusing to share it"
                    ),
                ));
            }
            Err(TryLockError::Error(error)) => {
                return Err(HarnessError::new(
                    ErrorCode::StorageOpenFailed,
                    format!("cannot lock lease {lease_id}: {error}"),
                ));
            }
        }
        sweep_settled_lock_files(store, &request.lock_root, &lock_path).await;
        let host_id = match store.fence() {
            Ok(fence) => fence.host_id.as_str().to_owned(),
            Err(error) => {
                discard_lock(file, &lock_path);
                return Err(error.into_harness_error());
            }
        };
        let now = to_i64(now_unix_ms());
        let created = store
            .create_backend_lease(&StoredBackendLease {
                lease_id: lease_id.clone(),
                owner_generation: 1,
                host_id,
                session_id: request.session_id.clone(),
                task_id: request.task_id.clone(),
                tool_execution_id: request.tool_execution_id.clone(),
                backend: request.backend.clone(),
                profile: request.profile.clone(),
                pid: None,
                lock_path: lock_path.to_string_lossy().replace('\\', "/"),
                state: "acquiring".to_owned(),
                enforced_json: json!(request.enforced).to_string(),
                not_claimed_json: json!(request.not_claimed).to_string(),
                artifact_id: None,
                artifact_digest: None,
                created_at_unix_ms: now,
                heartbeat_at_unix_ms: now,
                released_at_unix_ms: None,
                recovery_json: None,
            })
            .await;
        if let Err(error) = created {
            // No row names this lock - a second lease for one execution is
            // refused here - so the file is nobody's and goes with the refusal.
            discard_lock(file, &lock_path);
            return Err(error.into_harness_error());
        }
        Ok(Self {
            lease_id,
            lock_path,
            file: Some(file),
        })
    }

    /// Record that the process now exists.
    ///
    /// The pid column stays null: the runner does not expose the child's process
    /// id, and a pid would be the wrong identity anyway (it is reused). The lock
    /// is the identity, and it is held from before this row existed.
    pub async fn acquired(&self, store: &SqliteStore) -> Result<(), HarnessError> {
        store
            .advance_backend_lease(
                &self.lease_id,
                "acquired",
                to_i64(now_unix_ms()),
                None,
                None,
            )
            .await
            .map_err(StoreError::into_harness_error)?;
        Ok(())
    }

    /// Close the lease: `releasing`, then `released`, and only then the lock.
    ///
    /// Releasing twice is a no-op that reports so, and the lock is dropped after
    /// the row is settled so a concurrent reconciler never sees a settled lease
    /// whose owner is still holding the lock.
    pub async fn release(
        mut self,
        store: &SqliteStore,
        artifact: Option<(&str, &str)>,
    ) -> Result<bool, HarnessError> {
        let (artifact_id, artifact_digest) = match artifact {
            Some((id, digest)) => (Some(id), Some(digest)),
            None => (None, None),
        };
        let now = to_i64(now_unix_ms());
        store
            .advance_backend_lease(
                &self.lease_id,
                "releasing",
                now,
                artifact_id,
                artifact_digest,
            )
            .await
            .map_err(StoreError::into_harness_error)?;
        let released = store
            .release_backend_lease(
                &self.lease_id,
                "released",
                now,
                artifact_id,
                artifact_digest,
                None,
            )
            .await
            .map_err(StoreError::into_harness_error)?;
        // The row is settled, so the lock proves nothing any more: drop it and
        // its file. Every process execution makes one of these, and leaving them
        // behind grew the lock directory by one file per command for good.
        if let Some(file) = self.file.take() {
            discard_lock(file, &self.lock_path);
        }
        Ok(released)
    }

    #[must_use]
    pub fn lease_id(&self) -> &str {
        &self.lease_id
    }

    #[must_use]
    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }
}

/// What reconciliation decided about one lease.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseOutcome {
    /// The owner was proven gone and the lease was settled.
    Recovered,
    /// A live owner holds the lock: nothing was touched.
    StillOwned,
    /// The lease was already settled by someone else.
    AlreadySettled,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReconcileReport {
    pub recovered: Vec<String>,
    pub still_owned: Vec<String>,
    pub already_settled: Vec<String>,
}

impl ReconcileReport {
    #[must_use]
    pub fn outcome_for(&self, lease_id: &str) -> Option<LeaseOutcome> {
        if self.recovered.iter().any(|id| id == lease_id) {
            return Some(LeaseOutcome::Recovered);
        }
        if self.still_owned.iter().any(|id| id == lease_id) {
            return Some(LeaseOutcome::StillOwned);
        }
        if self.already_settled.iter().any(|id| id == lease_id) {
            return Some(LeaseOutcome::AlreadySettled);
        }
        None
    }
}

/// Settle the leases whose owner is provably gone.
///
/// A lease is only settled when its lock can be taken. Everything else is
/// reported and left exactly as it was.
pub async fn reconcile_backend_leases(
    store: &SqliteStore,
    older_than_unix_ms: u64,
    now_unix_ms: u64,
) -> Result<ReconcileReport, HarnessError> {
    let candidates = store
        .unsettled_backend_leases(to_i64(older_than_unix_ms))
        .await
        .map_err(StoreError::into_harness_error)?;
    let mut report = ReconcileReport::default();
    for lease in candidates {
        match owner_is_gone(&lease.lock_path) {
            LockProbe::Held => report.still_owned.push(lease.lease_id),
            LockProbe::Free { had_lock_file } => {
                let reason = if lease.state == "acquiring" && !had_lock_file {
                    "nothing_to_clean: the lease was written before a process existed and no lock file was ever taken"
                } else if lease.state == "acquiring" {
                    "nothing_to_clean: the lease never reached the acquired state, so no resource was exposed"
                } else {
                    "the owner's lock was free, so no process of this lease can still be running; the tree was terminated when its job handle closed"
                };
                let recovery = json!({
                    "schema_version": LEASE_SCHEMA_VERSION,
                    "recovered_at_unix_ms": now_unix_ms,
                    "state_when_recovered": lease.state,
                    "reason": reason,
                    "artifact_id": lease.artifact_id,
                    "owner_generation": lease.owner_generation,
                })
                .to_string();
                let settled = store
                    .release_backend_lease(
                        &lease.lease_id,
                        "recovered",
                        to_i64(now_unix_ms),
                        None,
                        None,
                        Some(&recovery),
                    )
                    .await
                    .map_err(StoreError::into_harness_error)?;
                // Settled either way, and its lock was free: the file is spent.
                let _ = std::fs::remove_file(&lease.lock_path);
                if settled {
                    report.recovered.push(lease.lease_id);
                } else {
                    report.already_settled.push(lease.lease_id);
                }
            }
        }
    }
    Ok(report)
}

enum LockProbe {
    /// Someone else holds the lock: the owner may be alive.
    Held,
    /// Nobody holds the lock.
    Free { had_lock_file: bool },
}

fn owner_is_gone(lock_path: &str) -> LockProbe {
    let path = Path::new(lock_path);
    let had_lock_file = path.is_file();
    match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => match file.try_lock() {
            Ok(()) => LockProbe::Free { had_lock_file },
            // A lock that cannot be taken for any other reason is treated as
            // held: an unknown owner is not an absent owner.
            Err(TryLockError::WouldBlock | TryLockError::Error(_)) => LockProbe::Held,
        },
        // No lock file at all: nobody can hold it.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            LockProbe::Free { had_lock_file }
        }
        // A file that is there but cannot be opened - denied, or held open
        // exclusively - says nothing about its owner. Reading that as "gone"
        // settled a lease whose owner might still be running, the one outcome
        // this module exists to rule out.
        Err(_) => LockProbe::Held,
    }
}

/// Release a lock and remove its file, best effort: a file that cannot be
/// removed now is swept later.
fn discard_lock(file: File, lock_path: &Path) {
    drop(file);
    let _ = std::fs::remove_file(lock_path);
}

/// How many stale lock files one sweep looks at, so a directory grown large
/// before lock files were removed costs a bounded first lease.
const SWEEP_LIMIT: usize = 512;

/// Remove the lock files of leases that are settled, or that never got a row.
///
/// Lock files used to stay behind after their lease was settled - one per
/// process execution, for good. Release and reconciliation now remove their
/// own; this clears what an older host left, once per lock directory per
/// process. A file is removed only when its lock can be taken right now and
/// its lease is settled or absent: a live owner's lock is never touched.
async fn sweep_settled_lock_files(store: &SqliteStore, lock_root: &Path, own: &Path) {
    static SWEPT: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
        std::sync::OnceLock::new();
    let first = SWEPT
        .get_or_init(Default::default)
        .lock()
        .is_ok_and(|mut swept| swept.insert(lock_root.to_path_buf()));
    if !first {
        return;
    }
    let Ok(entries) = std::fs::read_dir(lock_root) else {
        return;
    };
    let stale = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path != own)
        .filter_map(|path| {
            let lease_id = path
                .file_name()?
                .to_str()?
                .strip_suffix(LEASE_LOCK_SUFFIX)?
                .to_owned();
            Some((lease_id, path))
        })
        .take(SWEEP_LIMIT)
        .collect::<Vec<_>>();
    for (lease_id, path) in stale {
        // An owner creates its file, then locks it, then writes its row. A file
        // younger than the grace window may be one taken a moment ago and not
        // yet locked or recorded; removing it would leave a live lease pointing
        // at nothing, which reconciliation reads as an owner that is gone.
        let old_enough = std::fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age.as_millis() >= u128::from(LEASE_GRACE_MS));
        if !old_enough {
            continue;
        }
        let settled = match store.backend_lease(&lease_id).await {
            Ok(Some(lease)) => lease.released_at_unix_ms.is_some(),
            Ok(None) => true,
            Err(_) => false,
        };
        if !settled {
            continue;
        }
        if let Ok(file) = OpenOptions::new().read(true).write(true).open(&path)
            && file.try_lock().is_ok()
        {
            discard_lock(file, &path);
        }
    }
}

fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}
