//! Durable backend leases: the record that says a resource has an owner.
//!
//! The rule this table exists for is in the runbook: *persist the lifecycle
//! handle before the execution is exposed*, so a host that dies at any point
//! leaves a row a later host can read. The unique index on
//! `(host_id, tool_execution_id)` is what makes "one execution, one live
//! resource" a property of the schema rather than of the code that calls it,
//! and `released_at_unix_ms` is what makes a repeated release a no-op instead of
//! a second cleanup.
//!
//! The table deliberately does **not** decide whether a resource may be cleaned
//! up: it records the owner generation and the lock path. The decision belongs
//! to the reclaimer, which can only clean a lease whose lock it can take.

use harness_types::ErrorCode;
use sqlx::Row;

use super::{SqliteStore, database_error, to_i64};
use crate::StoreError;

/// The states a lease moves through.
pub const LEASE_STATES: [&str; 5] = [
    // Written before a process exists, so a crash during startup is visible.
    "acquiring",
    "acquired",
    // Written before the artifact is exported and the handle is dropped.
    "releasing",
    "released",
    // Settled by a reclaimer after the owner was proven gone.
    "recovered",
];

/// One stored lease.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredBackendLease {
    pub lease_id: String,
    /// Bumped by whoever takes the lease; a stale generation is never a reason
    /// to destroy a resource, only a reason to ask who owns it.
    pub owner_generation: u64,
    pub host_id: String,
    pub session_id: String,
    pub task_id: String,
    pub tool_execution_id: String,
    pub backend: String,
    pub profile: String,
    /// The process id the owner recorded, when a process existed. It is
    /// evidence, never an identity: pids are reused.
    pub pid: Option<i64>,
    pub lock_path: String,
    pub state: String,
    pub enforced_json: String,
    pub not_claimed_json: String,
    pub artifact_id: Option<String>,
    pub artifact_digest: Option<String>,
    pub created_at_unix_ms: i64,
    pub heartbeat_at_unix_ms: i64,
    pub released_at_unix_ms: Option<i64>,
    pub recovery_json: Option<String>,
}

impl SqliteStore {
    /// Write the lease before the execution it describes is exposed.
    ///
    /// A second lease for the same `(host, execution)` is refused: that pair is
    /// the identity of the resource, and two owners for one resource is the
    /// failure this table exists to prevent.
    pub async fn create_backend_lease(&self, lease: &StoredBackendLease) -> Result<(), StoreError> {
        if !LEASE_STATES.contains(&lease.state.as_str()) {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                format!("unknown lease state {}", lease.state),
            ));
        }
        if lease.lock_path.trim().is_empty() {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                "a lease must record the lock that proves its owner",
            ));
        }
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let inserted = sqlx::query(
            "INSERT OR IGNORE INTO backend_leases(lease_id, owner_generation, host_id, session_id, task_id, tool_execution_id, backend, profile, pid, lock_path, state, enforced_json, not_claimed_json, artifact_id, artifact_digest, created_at_unix_ms, heartbeat_at_unix_ms, released_at_unix_ms, recovery_json)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&lease.lease_id)
        .bind(to_i64(lease.owner_generation, "lease owner generation")?)
        .bind(&lease.host_id)
        .bind(&lease.session_id)
        .bind(&lease.task_id)
        .bind(&lease.tool_execution_id)
        .bind(&lease.backend)
        .bind(&lease.profile)
        .bind(lease.pid)
        .bind(&lease.lock_path)
        .bind(&lease.state)
        .bind(&lease.enforced_json)
        .bind(&lease.not_claimed_json)
        .bind(&lease.artifact_id)
        .bind(&lease.artifact_digest)
        .bind(lease.created_at_unix_ms)
        .bind(lease.heartbeat_at_unix_ms)
        .bind(lease.released_at_unix_ms)
        .bind(&lease.recovery_json)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "insert backend lease", error)
        })?;
        if inserted.rows_affected() != 1 {
            return Err(StoreError::new(
                ErrorCode::TaskLeaseConflict,
                "this host already has a lease for that execution",
            ));
        }
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit backend lease", error)
        })
    }

    pub async fn backend_lease(
        &self,
        lease_id: &str,
    ) -> Result<Option<StoredBackendLease>, StoreError> {
        let row = sqlx::query("SELECT * FROM backend_leases WHERE lease_id = ?")
            .bind(lease_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageWriteFailed, "read backend lease", error)
            })?;
        row.map(|row| lease_from_row(&row)).transpose()
    }

    pub async fn backend_lease_for_execution(
        &self,
        host_id: &str,
        tool_execution_id: &str,
    ) -> Result<Option<StoredBackendLease>, StoreError> {
        let row =
            sqlx::query("SELECT * FROM backend_leases WHERE host_id = ? AND tool_execution_id = ?")
                .bind(host_id)
                .bind(tool_execution_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|error| {
                    database_error(ErrorCode::StorageWriteFailed, "read backend lease", error)
                })?;
        row.map(|row| lease_from_row(&row)).transpose()
    }

    /// Leases that are not settled and whose owner has not written a heartbeat
    /// since `older_than_unix_ms`.
    ///
    /// The query only narrows the field; it does not decide anything. A row here
    /// is a candidate for reconciliation, never a licence to delete.
    pub async fn unsettled_backend_leases(
        &self,
        older_than_unix_ms: i64,
    ) -> Result<Vec<StoredBackendLease>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM backend_leases WHERE released_at_unix_ms IS NULL AND heartbeat_at_unix_ms <= ? ORDER BY created_at_unix_ms",
        )
        .bind(older_than_unix_ms)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "list backend leases", error)
        })?;
        rows.iter().map(lease_from_row).collect()
    }

    /// Move a lease forward, and record the artifact once it exists.
    ///
    /// A settled lease is never moved again: the update is guarded on
    /// `released_at_unix_ms IS NULL`, so a late writer cannot reopen a closed
    /// lifecycle. The return value says whether this call was the one that moved
    /// it.
    pub async fn advance_backend_lease(
        &self,
        lease_id: &str,
        state: &str,
        heartbeat_at_unix_ms: i64,
        artifact_id: Option<&str>,
        artifact_digest: Option<&str>,
    ) -> Result<bool, StoreError> {
        if !LEASE_STATES.contains(&state) {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                format!("unknown lease state {state}"),
            ));
        }
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let updated = sqlx::query(
            "UPDATE backend_leases SET state = ?, heartbeat_at_unix_ms = ?, artifact_id = COALESCE(?, artifact_id), artifact_digest = COALESCE(?, artifact_digest) WHERE lease_id = ? AND released_at_unix_ms IS NULL",
        )
        .bind(state)
        .bind(heartbeat_at_unix_ms)
        .bind(artifact_id)
        .bind(artifact_digest)
        .bind(lease_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "advance backend lease", error)
        })?;
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit backend lease", error)
        })?;
        Ok(updated.rows_affected() == 1)
    }

    /// Close a lease. Repeating it is a no-op that reports so.
    pub async fn release_backend_lease(
        &self,
        lease_id: &str,
        state: &str,
        released_at_unix_ms: i64,
        artifact_id: Option<&str>,
        artifact_digest: Option<&str>,
        recovery_json: Option<&str>,
    ) -> Result<bool, StoreError> {
        if !LEASE_STATES.contains(&state) {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                format!("unknown lease state {state}"),
            ));
        }
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        let updated = sqlx::query(
            "UPDATE backend_leases SET state = ?, released_at_unix_ms = ?, heartbeat_at_unix_ms = ?, artifact_id = COALESCE(?, artifact_id), artifact_digest = COALESCE(?, artifact_digest), recovery_json = COALESCE(?, recovery_json) WHERE lease_id = ? AND released_at_unix_ms IS NULL",
        )
        .bind(state)
        .bind(released_at_unix_ms)
        .bind(released_at_unix_ms)
        .bind(artifact_id)
        .bind(artifact_digest)
        .bind(recovery_json)
        .bind(lease_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "release backend lease", error)
        })?;
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit backend lease", error)
        })?;
        Ok(updated.rows_affected() == 1)
    }
}

fn lease_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<StoredBackendLease, StoreError> {
    Ok(StoredBackendLease {
        lease_id: row.get("lease_id"),
        owner_generation: u64::try_from(row.get::<i64, _>("owner_generation")).map_err(|_| {
            StoreError::new(
                ErrorCode::SnapshotCorrupt,
                "lease owner generation is invalid",
            )
        })?,
        host_id: row.get("host_id"),
        session_id: row.get("session_id"),
        task_id: row.get("task_id"),
        tool_execution_id: row.get("tool_execution_id"),
        backend: row.get("backend"),
        profile: row.get("profile"),
        pid: row.get("pid"),
        lock_path: row.get("lock_path"),
        state: row.get("state"),
        enforced_json: row.get("enforced_json"),
        not_claimed_json: row.get("not_claimed_json"),
        artifact_id: row.get("artifact_id"),
        artifact_digest: row.get("artifact_digest"),
        created_at_unix_ms: row.get("created_at_unix_ms"),
        heartbeat_at_unix_ms: row.get("heartbeat_at_unix_ms"),
        released_at_unix_ms: row.get("released_at_unix_ms"),
        recovery_json: row.get("recovery_json"),
    })
}
