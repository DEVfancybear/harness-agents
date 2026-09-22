//! Additive P5 delegation schema and durable task-DAG operations.
//!
//! Every write goes through the P1 transaction coordinator and host fence, so
//! the delegation tables never become a second database authority.

use harness_types::{AgentRunId, ContentHash, ErrorCode, ProjectId, SessionId, TaskId};
use serde_json::Value;
use sqlx::{Sqlite, SqlitePool, Transaction};

use super::{SqliteStore, assert_fence_in_tx, database_error, row_get, to_i64, to_u64};
use crate::{
    BudgetUsageRecord, DELEGATION_SCHEMA_VERSION, DeliveryCommit, MemoryBindingRow,
    ParentDeliveryRecord, StoreError, StoreFaultPoint, StoredDelegatedResultRecord,
    StoredTaskNodeRecord, TaskOwnerRecord, WorktreeRecordRow,
};

const DELEGATION_SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS delegation_schema_migrations (
        version INTEGER PRIMARY KEY,
        applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    )",
    "CREATE TABLE IF NOT EXISTS delegation_tasks (
        task_id TEXT PRIMARY KEY,
        parent_task_id TEXT,
        role TEXT NOT NULL,
        status TEXT NOT NULL CHECK (status IN ('pending','ready','running','blocked','completed','failed','canceled')),
        revision INTEGER NOT NULL CHECK (revision >= 1),
        depth INTEGER NOT NULL CHECK (depth >= 0),
        brief_json TEXT NOT NULL,
        node_json TEXT NOT NULL,
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    )",
    "CREATE INDEX IF NOT EXISTS delegation_tasks_by_status ON delegation_tasks(status, depth)",
    "CREATE TABLE IF NOT EXISTS delegation_task_dependencies (
        task_id TEXT NOT NULL REFERENCES delegation_tasks(task_id),
        depends_on_task_id TEXT NOT NULL,
        PRIMARY KEY (task_id, depends_on_task_id)
    )",
    "CREATE TABLE IF NOT EXISTS delegation_task_owners (
        task_id TEXT PRIMARY KEY REFERENCES delegation_tasks(task_id),
        owner_run_id TEXT NOT NULL,
        owner_session_id TEXT NOT NULL,
        role TEXT NOT NULL,
        generation INTEGER NOT NULL CHECK (generation >= 1),
        lease_revision INTEGER NOT NULL CHECK (lease_revision >= 1),
        claimed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    )",
    "CREATE TABLE IF NOT EXISTS delegation_results (
        result_id TEXT PRIMARY KEY,
        task_id TEXT NOT NULL REFERENCES delegation_tasks(task_id),
        worker_run_id TEXT NOT NULL,
        outcome TEXT NOT NULL,
        base_revision TEXT NOT NULL,
        result_revision TEXT NOT NULL,
        artifact_refs_json TEXT NOT NULL,
        report_json TEXT NOT NULL,
        result_hash TEXT NOT NULL,
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    )",
    "CREATE INDEX IF NOT EXISTS delegation_results_by_task ON delegation_results(task_id)",
    "CREATE TABLE IF NOT EXISTS message_deliveries (
        message_id TEXT PRIMARY KEY,
        sender_task_id TEXT NOT NULL,
        recipient_task_id TEXT NOT NULL,
        recipient_session_id TEXT NOT NULL,
        result_id TEXT,
        payload_hash TEXT NOT NULL,
        payload_json TEXT NOT NULL,
        state TEXT NOT NULL CHECK (state IN ('pending','consumed')),
        consumed_by TEXT,
        attempts INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    )",
    "CREATE INDEX IF NOT EXISTS message_deliveries_by_recipient
        ON message_deliveries(recipient_task_id, state)",
    "CREATE TABLE IF NOT EXISTS delegation_budget_usage (
        task_id TEXT PRIMARY KEY REFERENCES delegation_tasks(task_id),
        model_requests INTEGER NOT NULL DEFAULT 0 CHECK (model_requests >= 0),
        retries INTEGER NOT NULL DEFAULT 0 CHECK (retries >= 0),
        cost_units INTEGER NOT NULL DEFAULT 0 CHECK (cost_units >= 0)
    )",
    "CREATE TABLE IF NOT EXISTS delegation_worktrees (
        worktree_id TEXT PRIMARY KEY,
        task_id TEXT NOT NULL,
        run_id TEXT NOT NULL,
        project_id TEXT NOT NULL,
        base_commit TEXT NOT NULL,
        base_branch TEXT NOT NULL,
        branch TEXT NOT NULL UNIQUE,
        path TEXT NOT NULL,
        write_scope_json TEXT NOT NULL,
        state TEXT NOT NULL,
        input_fingerprint TEXT NOT NULL,
        result_fingerprint TEXT,
        generation INTEGER NOT NULL CHECK (generation >= 1),
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    )",
    "CREATE INDEX IF NOT EXISTS delegation_worktrees_by_project
        ON delegation_worktrees(project_id, state)",
    "CREATE TABLE IF NOT EXISTS delegation_integrations (
        integration_id TEXT PRIMARY KEY,
        project_id TEXT NOT NULL,
        base_commit TEXT NOT NULL,
        final_commit TEXT NOT NULL,
        final_fingerprint TEXT NOT NULL,
        report_json TEXT NOT NULL,
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    )",
    "CREATE TABLE IF NOT EXISTS delegation_memory_bindings (
        binding_id TEXT PRIMARY KEY,
        task_id TEXT NOT NULL,
        agent_profile_id TEXT NOT NULL,
        memory_asset_id TEXT NOT NULL,
        version INTEGER NOT NULL CHECK (version >= 1),
        injection_mode TEXT NOT NULL,
        priority INTEGER NOT NULL,
        actions_json TEXT NOT NULL,
        revision INTEGER NOT NULL CHECK (revision >= 1),
        logged_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    )",
];

pub(super) async fn ensure_delegation_schema(pool: &SqlitePool) -> Result<(), StoreError> {
    let mut tx = pool.begin().await.map_err(|error| {
        database_error(
            ErrorCode::MigrationFailed,
            "begin delegation migration",
            error,
        )
    })?;
    for statement in DELEGATION_SCHEMA {
        sqlx::query(*statement)
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                database_error(ErrorCode::MigrationFailed, "apply delegation schema", error)
            })?;
    }
    let current = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT MAX(version) FROM delegation_schema_migrations",
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| {
        database_error(
            ErrorCode::MigrationFailed,
            "read delegation migration version",
            error,
        )
    })?
    .unwrap_or(0);
    if current > DELEGATION_SCHEMA_VERSION {
        return Err(StoreError::new(
            ErrorCode::MigrationFailed,
            "delegation schema is newer than this host supports",
        ));
    }
    if current < DELEGATION_SCHEMA_VERSION {
        sqlx::query("INSERT INTO delegation_schema_migrations(version) VALUES (?)")
            .bind(DELEGATION_SCHEMA_VERSION)
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                database_error(
                    ErrorCode::MigrationFailed,
                    "record delegation migration",
                    error,
                )
            })?;
    }
    tx.commit().await.map_err(|error| {
        database_error(
            ErrorCode::MigrationFailed,
            "commit delegation migration",
            error,
        )
    })
}

/// Everything needed to admit one validated task node.
#[derive(Clone, Debug)]
pub struct TaskAdmission {
    pub task: StoredTaskNodeRecord,
    pub owner: Option<TaskOwnerRecord>,
}

impl SqliteStore {
    /// Admit one task node. Duplicate IDs are rejected rather than merged.
    pub async fn admit_task(&self, admission: TaskAdmission) -> Result<(), StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        insert_task(&mut tx, &admission.task).await?;
        if let Some(owner) = &admission.owner {
            insert_owner(&mut tx, owner).await?;
        }
        tx.commit()
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "admit task", error))
    }

    /// Admit a whole proven DAG in one transaction. Either every node is
    /// durable or none is.
    pub async fn admit_task_graph(&self, admissions: &[TaskAdmission]) -> Result<(), StoreError> {
        if admissions.is_empty() {
            return Err(StoreError::new(
                ErrorCode::InvalidPayload,
                "a task graph admission requires at least one task",
            ));
        }
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        for admission in admissions {
            insert_task(&mut tx, &admission.task).await?;
            if let Some(owner) = &admission.owner {
                insert_owner(&mut tx, owner).await?;
            }
        }
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "admit task graph", error)
        })
    }

    /// Claim a task for exactly one run, fenced by ownership generation.
    pub async fn claim_task(
        &self,
        owner: &TaskOwnerRecord,
        expected_revision: u64,
    ) -> Result<u64, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        let existing = sqlx::query(
            "SELECT owner_run_id, owner_session_id, generation, lease_revision
             FROM delegation_task_owners WHERE task_id = ?",
        )
        .bind(owner.task_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read task owner", error))?;
        if let Some(row) = existing {
            let current_generation = to_u64(row_get::<i64>(&row, "generation")?, "generation")?;
            let current_run = row_get::<String>(&row, "owner_run_id")?;
            if current_generation > owner.generation {
                return Err(StoreError::new(
                    ErrorCode::TaskOwnershipConflict,
                    "a newer run generation already owns this task",
                ));
            }
            if current_generation == owner.generation && current_run != owner.owner_run_id.as_str()
            {
                return Err(StoreError::new(
                    ErrorCode::TaskOwnershipConflict,
                    "another run holds this task at the same generation",
                ));
            }
        }
        let current =
            sqlx::query("SELECT status, revision FROM delegation_tasks WHERE task_id = ?")
                .bind(owner.task_id.as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read task", error))?
                .ok_or_else(|| StoreError::new(ErrorCode::TaskNotFound, "task is not admitted"))?;
        let status = row_get::<String>(&current, "status")?;
        let revision = to_u64(row_get::<i64>(&current, "revision")?, "task revision")?;
        if status == "completed" {
            return Err(StoreError::new(
                ErrorCode::TaskOwnershipConflict,
                "a completed task is never reassigned",
            ));
        }
        if revision != expected_revision {
            return Err(StoreError::new(
                ErrorCode::SequenceConflict,
                "task revision changed before the claim committed",
            ));
        }
        sqlx::query(
            "INSERT INTO delegation_task_owners(task_id, owner_run_id, owner_session_id, role, generation, lease_revision)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(task_id) DO UPDATE SET
                 owner_run_id = excluded.owner_run_id,
                 owner_session_id = excluded.owner_session_id,
                 role = excluded.role,
                 generation = excluded.generation,
                 lease_revision = excluded.lease_revision,
                 claimed_at = CURRENT_TIMESTAMP",
        )
        .bind(owner.task_id.as_str())
        .bind(owner.owner_run_id.as_str())
        .bind(owner.owner_session_id.as_str())
        .bind(&owner.role)
        .bind(to_i64(owner.generation, "ownership generation")?)
        .bind(to_i64(owner.lease_revision, "lease revision")?)
        .execute(&mut *tx)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "claim task", error))?;
        let next = revision.saturating_add(1);
        let status = if status == "pending" {
            "ready"
        } else {
            status.as_str()
        };
        sqlx::query(
            "UPDATE delegation_tasks SET revision = ?, status = ?, updated_at = CURRENT_TIMESTAMP
             WHERE task_id = ?",
        )
        .bind(to_i64(next, "task revision")?)
        .bind(status)
        .bind(owner.task_id.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "advance task", error))?;
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit task claim", error)
        })?;
        Ok(next)
    }

    /// Commit a task transition, an optional worker report and the durable
    /// parent message in one transaction. A lost notification cannot lose the
    /// result because notification is not part of this path.
    pub async fn commit_delivery(&self, commit: DeliveryCommit) -> Result<(), StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        let current =
            sqlx::query("SELECT status, revision FROM delegation_tasks WHERE task_id = ?")
                .bind(commit.task_transition.task_id.as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read task", error))?
                .ok_or_else(|| StoreError::new(ErrorCode::TaskNotFound, "task is not admitted"))?;
        let current_status = row_get::<String>(&current, "status")?;
        if current_status == "completed" && commit.task_transition.status != "completed" {
            return Err(StoreError::new(
                ErrorCode::InvalidStateTransition,
                "a completed task cannot leave the completed state",
            ));
        }
        if let Some(result) = &commit.result {
            insert_result(&mut tx, result).await?;
        }
        sqlx::query(
            "UPDATE delegation_tasks SET status = ?, revision = ?, node_json = ?, updated_at = CURRENT_TIMESTAMP
             WHERE task_id = ?",
        )
        .bind(&commit.task_transition.status)
        .bind(to_i64(commit.task_transition.revision, "task revision")?)
        .bind(serde_json::to_string(&commit.task_transition.node_json).map_err(|_| {
            StoreError::new(ErrorCode::InvalidPayload, "task node is not serializable")
        })?)
        .bind(commit.task_transition.task_id.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "update task", error))?;
        insert_delivery(&mut tx, &commit.delivery).await?;
        if let Some(usage) = &commit.usage {
            sqlx::query(
                "INSERT INTO delegation_budget_usage(task_id, model_requests, retries, cost_units)
                 VALUES (?, ?, ?, ?)
                 ON CONFLICT(task_id) DO UPDATE SET
                     model_requests = delegation_budget_usage.model_requests + excluded.model_requests,
                     retries = delegation_budget_usage.retries + excluded.retries,
                     cost_units = delegation_budget_usage.cost_units + excluded.cost_units",
            )
            .bind(commit.task_transition.task_id.as_str())
            .bind(i64::from(usage.model_requests))
            .bind(i64::from(usage.retries))
            .bind(to_i64(usage.cost_units, "cost units")?)
            .execute(&mut *tx)
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "charge usage", error))?;
        }
        if self
            .fault_plan_ref()
            .consume(StoreFaultPoint::BeforeDelegationDeliveryCommit)
        {
            return Err(StoreError::new(
                ErrorCode::StorageWriteFailed,
                "injected failure before delegation delivery commit",
            ));
        }
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit delivery", error)
        })
    }

    /// Consume one parent message exactly once logically.
    pub async fn consume_delivery(
        &self,
        message_id: &str,
        consumer: &str,
    ) -> Result<bool, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        let row =
            sqlx::query("SELECT state, consumed_by FROM message_deliveries WHERE message_id = ?")
                .bind(message_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|error| {
                    database_error(ErrorCode::StorageWriteFailed, "read delivery", error)
                })?
                .ok_or_else(|| {
                    StoreError::new(
                        ErrorCode::DeliveryConflict,
                        "delivery message does not exist",
                    )
                })?;
        let state = row_get::<String>(&row, "state")?;
        let first_consumption = state == "pending";
        if first_consumption {
            sqlx::query(
                "UPDATE message_deliveries
                 SET state = 'consumed', consumed_by = ?, attempts = attempts + 1
                 WHERE message_id = ? AND state = 'pending'",
            )
            .bind(consumer)
            .bind(message_id)
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                database_error(ErrorCode::StorageWriteFailed, "consume delivery", error)
            })?;
        }
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit delivery consumption",
                error,
            )
        })?;
        Ok(first_consumption)
    }

    /// Mark `assigned` after a successful claim and keep the owner generation.
    pub async fn mark_task_running(
        &self,
        task_id: &TaskId,
        owner_run_id: &AgentRunId,
        generation: u64,
    ) -> Result<u64, StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        assert_owner(&mut tx, task_id, owner_run_id, generation).await?;
        let revision = read_task_revision(&mut tx, task_id).await?;
        let next = revision.saturating_add(1);
        sqlx::query(
            "UPDATE delegation_tasks SET status = 'running', revision = ?, updated_at = CURRENT_TIMESTAMP
             WHERE task_id = ?",
        )
        .bind(to_i64(next, "task revision")?)
        .bind(task_id.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "mark task running", error))?;
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit task running", error)
        })?;
        Ok(next)
    }

    /// Persist a workspace row. Two workers can never share a branch.
    pub async fn upsert_worktree(&self, record: &WorktreeRecordRow) -> Result<(), StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        let write_scope = serde_json::to_string(&record.write_scope).map_err(|_| {
            StoreError::new(ErrorCode::InvalidPayload, "write scope is not serializable")
        })?;
        sqlx::query(
            "INSERT INTO delegation_worktrees(
                 worktree_id, task_id, run_id, project_id, base_commit, base_branch, branch, path,
                 write_scope_json, state, input_fingerprint, result_fingerprint, generation)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(worktree_id) DO UPDATE SET
                 state = excluded.state,
                 result_fingerprint = excluded.result_fingerprint,
                 path = excluded.path",
        )
        .bind(&record.worktree_id)
        .bind(record.task_id.as_str())
        .bind(record.run_id.as_str())
        .bind(record.project_id.as_str())
        .bind(&record.base_commit)
        .bind(&record.base_branch)
        .bind(&record.branch)
        .bind(&record.path)
        .bind(write_scope)
        .bind(&record.state)
        .bind(record.input_fingerprint.as_str())
        .bind(record.result_fingerprint.as_ref().map(ContentHash::as_str))
        .bind(to_i64(record.generation, "worktree generation")?)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "persist worktree", error)
        })?;
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit worktree", error)
        })
    }

    /// Bind one host-issued memory version to a delegated worker.
    pub async fn bind_delegation_memory(
        &self,
        binding: &MemoryBindingRow,
    ) -> Result<(), StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        let actions_json = serde_json::to_string(&binding.actions).map_err(|_| {
            StoreError::new(
                ErrorCode::InvalidPayload,
                "binding actions are not serializable",
            )
        })?;
        sqlx::query(
            "INSERT INTO delegation_memory_bindings(
                 binding_id, task_id, agent_profile_id, memory_asset_id, version,
                 injection_mode, priority, actions_json, revision)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(binding_id) DO UPDATE SET
                 version = excluded.version,
                 injection_mode = excluded.injection_mode,
                 priority = excluded.priority,
                 actions_json = excluded.actions_json,
                 revision = excluded.revision,
                 logged_at = CURRENT_TIMESTAMP",
        )
        .bind(&binding.binding_id)
        .bind(binding.task_id.as_str())
        .bind(binding.profile_id.as_str())
        .bind(binding.memory_asset_id.as_str())
        .bind(to_i64(binding.version, "binding version")?)
        .bind(&binding.injection_mode)
        .bind(binding.priority)
        .bind(actions_json)
        .bind(to_i64(binding.revision, "binding revision")?)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "persist memory binding",
                error,
            )
        })?;
        tx.commit().await.map_err(|error| {
            database_error(
                ErrorCode::StorageWriteFailed,
                "commit memory binding",
                error,
            )
        })
    }

    /// Record the final integration report. Only this revision is current.
    pub async fn record_integration(
        &self,
        integration_id: &str,
        project_id: &ProjectId,
        base_commit: &str,
        final_commit: &str,
        final_fingerprint: &ContentHash,
        report: &Value,
    ) -> Result<(), StoreError> {
        let fence = self.fence()?;
        let mut tx = self.begin_write(&fence).await?;
        assert_fence_in_tx(&mut tx, &fence).await?;
        let report_json = serde_json::to_string(report).map_err(|_| {
            StoreError::new(
                ErrorCode::InvalidPayload,
                "integration report is not serializable",
            )
        })?;
        sqlx::query(
            "INSERT INTO delegation_integrations(
                 integration_id, project_id, base_commit, final_commit, final_fingerprint, report_json)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(integration_id) DO UPDATE SET
                 final_commit = excluded.final_commit,
                 final_fingerprint = excluded.final_fingerprint,
                 report_json = excluded.report_json",
        )
        .bind(integration_id)
        .bind(project_id.as_str())
        .bind(base_commit)
        .bind(final_commit)
        .bind(final_fingerprint.as_str())
        .bind(report_json)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "record integration", error)
        })?;
        tx.commit().await.map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "commit integration", error)
        })
    }

    pub async fn task_node(
        &self,
        task_id: &TaskId,
    ) -> Result<Option<StoredTaskNodeRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT task_id, parent_task_id, role, status, revision, depth, brief_json, node_json
             FROM delegation_tasks WHERE task_id = ?",
        )
        .bind(task_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read task", error))?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(read_task_row(&row)?))
    }

    pub async fn list_task_nodes(&self) -> Result<Vec<StoredTaskNodeRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT task_id, parent_task_id, role, status, revision, depth, brief_json, node_json
             FROM delegation_tasks ORDER BY depth, created_at, task_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "list tasks", error))?;
        rows.iter().map(read_task_row).collect()
    }

    pub async fn task_owner(
        &self,
        task_id: &TaskId,
    ) -> Result<Option<TaskOwnerRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT task_id, owner_run_id, owner_session_id, role, generation, lease_revision
             FROM delegation_task_owners WHERE task_id = ?",
        )
        .bind(task_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read owner", error))?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(TaskOwnerRecord {
            task_id: TaskId::parse(row_get::<String>(&row, "task_id")?)?,
            owner_run_id: AgentRunId::parse(row_get::<String>(&row, "owner_run_id")?)?,
            owner_session_id: SessionId::parse(row_get::<String>(&row, "owner_session_id")?)?,
            role: row_get::<String>(&row, "role")?,
            generation: to_u64(row_get::<i64>(&row, "generation")?, "generation")?,
            lease_revision: to_u64(row_get::<i64>(&row, "lease_revision")?, "lease revision")?,
        }))
    }

    pub async fn task_dependencies(&self, task_id: &TaskId) -> Result<Vec<TaskId>, StoreError> {
        let rows = sqlx::query(
            "SELECT depends_on_task_id FROM delegation_task_dependencies
             WHERE task_id = ? ORDER BY depends_on_task_id",
        )
        .bind(task_id.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "read dependencies", error)
        })?;
        rows.iter()
            .map(|row| {
                TaskId::parse(row_get::<String>(row, "depends_on_task_id")?)
                    .map_err(StoreError::from)
            })
            .collect()
    }

    pub async fn task_result(
        &self,
        task_id: &TaskId,
    ) -> Result<Option<StoredDelegatedResultRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT result_id, task_id, worker_run_id, outcome, base_revision, result_revision,
                    artifact_refs_json, report_json, result_hash
             FROM delegation_results WHERE task_id = ? ORDER BY created_at DESC LIMIT 1",
        )
        .bind(task_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read result", error))?;
        let Some(row) = row else {
            return Ok(None);
        };
        let refs: Vec<String> =
            serde_json::from_str(&row_get::<String>(&row, "artifact_refs_json")?).map_err(
                |_| {
                    StoreError::new(
                        ErrorCode::StorageWriteFailed,
                        "stored artifact refs are invalid",
                    )
                },
            )?;
        let report: Value = serde_json::from_str(&row_get::<String>(&row, "report_json")?)
            .map_err(|_| {
                StoreError::new(
                    ErrorCode::StorageWriteFailed,
                    "stored result is invalid JSON",
                )
            })?;
        Ok(Some(StoredDelegatedResultRecord {
            result_id: row_get::<String>(&row, "result_id")?,
            task_id: TaskId::parse(row_get::<String>(&row, "task_id")?)?,
            worker_run_id: AgentRunId::parse(row_get::<String>(&row, "worker_run_id")?)?,
            outcome: row_get::<String>(&row, "outcome")?,
            base_revision: row_get::<String>(&row, "base_revision")?,
            result_revision: row_get::<String>(&row, "result_revision")?,
            artifact_refs: refs,
            report_json: report,
            result_hash: ContentHash::parse(row_get::<String>(&row, "result_hash")?)?,
        }))
    }

    /// Pending parent messages for one recipient task, oldest first.
    pub async fn pending_deliveries(
        &self,
        recipient_task_id: &TaskId,
    ) -> Result<Vec<ParentDeliveryRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT message_id, sender_task_id, recipient_task_id, recipient_session_id, result_id,
                    payload_hash, payload_json, state, consumed_by
             FROM message_deliveries
             WHERE recipient_task_id = ? AND state = 'pending'
             ORDER BY created_at, message_id",
        )
        .bind(recipient_task_id.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "list deliveries", error))?;
        rows.iter().map(read_delivery_row).collect()
    }

    pub async fn delivery(
        &self,
        message_id: &str,
    ) -> Result<Option<ParentDeliveryRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT message_id, sender_task_id, recipient_task_id, recipient_session_id, result_id,
                    payload_hash, payload_json, state, consumed_by
             FROM message_deliveries WHERE message_id = ?",
        )
        .bind(message_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read delivery", error))?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(read_delivery_row(&row)?))
    }

    pub async fn worktree(
        &self,
        worktree_id: &str,
    ) -> Result<Option<WorktreeRecordRow>, StoreError> {
        let row = sqlx::query(
            "SELECT worktree_id, task_id, run_id, project_id, base_commit, base_branch, branch,
                    path, write_scope_json, state, input_fingerprint, result_fingerprint, generation
             FROM delegation_worktrees WHERE worktree_id = ?",
        )
        .bind(worktree_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read worktree", error))?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(read_worktree_row(&row)?))
    }

    pub async fn list_worktrees(&self) -> Result<Vec<WorktreeRecordRow>, StoreError> {
        let rows = sqlx::query(
            "SELECT worktree_id, task_id, run_id, project_id, base_commit, base_branch, branch,
                    path, write_scope_json, state, input_fingerprint, result_fingerprint, generation
             FROM delegation_worktrees ORDER BY created_at, worktree_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "list worktrees", error))?;
        rows.iter().map(read_worktree_row).collect()
    }

    pub async fn budget_usage(
        &self,
        task_id: &TaskId,
    ) -> Result<Option<BudgetUsageRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT task_id, model_requests, retries, cost_units
             FROM delegation_budget_usage WHERE task_id = ?",
        )
        .bind(task_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read usage", error))?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(BudgetUsageRecord {
            model_requests: u32::try_from(row_get::<i64>(&row, "model_requests")?).map_err(
                |_| StoreError::new(ErrorCode::StorageWriteFailed, "usage is out of range"),
            )?,
            retries: u32::try_from(row_get::<i64>(&row, "retries")?).map_err(|_| {
                StoreError::new(ErrorCode::StorageWriteFailed, "retries are out of range")
            })?,
            cost_units: to_u64(row_get::<i64>(&row, "cost_units")?, "cost units")?,
        }))
    }

    pub async fn delegation_task_count(&self) -> Result<u64, StoreError> {
        let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM delegation_tasks")
            .fetch_one(&self.pool)
            .await
            .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "count tasks", error))?;
        to_u64(count, "task count")
    }
}

async fn insert_task(
    tx: &mut Transaction<'_, Sqlite>,
    task: &StoredTaskNodeRecord,
) -> Result<(), StoreError> {
    let brief_json = serde_json::to_string(&task.brief_json).map_err(|_| {
        StoreError::new(ErrorCode::InvalidPayload, "task brief is not serializable")
    })?;
    let node_json = serde_json::to_string(&task.node_json)
        .map_err(|_| StoreError::new(ErrorCode::InvalidPayload, "task node is not serializable"))?;
    let result = sqlx::query(
        "INSERT OR IGNORE INTO delegation_tasks(
             task_id, parent_task_id, role, status, revision, depth, brief_json, node_json)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(task.task_id.as_str())
    .bind(task.parent_task_id.as_ref().map(TaskId::as_str))
    .bind(&task.role)
    .bind(&task.status)
    .bind(to_i64(task.revision, "task revision")?)
    .bind(i64::from(task.depth))
    .bind(brief_json)
    .bind(node_json)
    .execute(&mut **tx)
    .await
    .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "insert task", error))?;
    if result.rows_affected() == 0 {
        return Err(StoreError::new(
            ErrorCode::DuplicateTaskId,
            "task id is already admitted",
        ));
    }
    for dependency in &task.depends_on {
        sqlx::query(
            "INSERT OR IGNORE INTO delegation_task_dependencies(task_id, depends_on_task_id)
             VALUES (?, ?)",
        )
        .bind(task.task_id.as_str())
        .bind(dependency.as_str())
        .execute(&mut **tx)
        .await
        .map_err(|error| {
            database_error(ErrorCode::StorageWriteFailed, "insert dependency", error)
        })?;
    }
    Ok(())
}

async fn insert_owner(
    tx: &mut Transaction<'_, Sqlite>,
    owner: &TaskOwnerRecord,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO delegation_task_owners(
             task_id, owner_run_id, owner_session_id, role, generation, lease_revision)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(task_id) DO UPDATE SET
             owner_run_id = excluded.owner_run_id,
             owner_session_id = excluded.owner_session_id,
             role = excluded.role,
             generation = excluded.generation,
             lease_revision = excluded.lease_revision",
    )
    .bind(owner.task_id.as_str())
    .bind(owner.owner_run_id.as_str())
    .bind(owner.owner_session_id.as_str())
    .bind(&owner.role)
    .bind(to_i64(owner.generation, "ownership generation")?)
    .bind(to_i64(owner.lease_revision, "lease revision")?)
    .execute(&mut **tx)
    .await
    .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "insert owner", error))?;
    Ok(())
}

async fn insert_result(
    tx: &mut Transaction<'_, Sqlite>,
    result: &StoredDelegatedResultRecord,
) -> Result<(), StoreError> {
    let refs = serde_json::to_string(&result.artifact_refs).map_err(|_| {
        StoreError::new(
            ErrorCode::InvalidPayload,
            "artifact refs are not serializable",
        )
    })?;
    let report = serde_json::to_string(&result.report_json)
        .map_err(|_| StoreError::new(ErrorCode::InvalidPayload, "result is not serializable"))?;
    let inserted = sqlx::query(
        "INSERT OR IGNORE INTO delegation_results(
             result_id, task_id, worker_run_id, outcome, base_revision, result_revision,
             artifact_refs_json, report_json, result_hash)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&result.result_id)
    .bind(result.task_id.as_str())
    .bind(result.worker_run_id.as_str())
    .bind(&result.outcome)
    .bind(&result.base_revision)
    .bind(&result.result_revision)
    .bind(refs)
    .bind(report)
    .bind(result.result_hash.as_str())
    .execute(&mut **tx)
    .await
    .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "insert result", error))?;
    if inserted.rows_affected() == 0 {
        // Idempotent replay of the same stable result id is allowed only when
        // the content hash matches; a conflicting reuse is rejected.
        let existing =
            sqlx::query("SELECT result_hash FROM delegation_results WHERE result_id = ?")
                .bind(&result.result_id)
                .fetch_optional(&mut **tx)
                .await
                .map_err(|error| {
                    database_error(ErrorCode::StorageWriteFailed, "read result hash", error)
                })?
                .ok_or_else(|| {
                    StoreError::new(ErrorCode::StorageWriteFailed, "result row disappeared")
                })?;
        let hash = row_get::<String>(&existing, "result_hash")?;
        if hash != result.result_hash.as_str() {
            return Err(StoreError::new(
                ErrorCode::DeliveryConflict,
                "the same result id cannot carry different content",
            ));
        }
    }
    Ok(())
}

pub(crate) async fn insert_delivery(
    tx: &mut Transaction<'_, Sqlite>,
    delivery: &ParentDeliveryRecord,
) -> Result<(), StoreError> {
    if delivery.state == "consumed" {
        return Err(StoreError::new(
            ErrorCode::DeliveryConflict,
            "a new delivery cannot be inserted already consumed",
        ));
    }
    let payload = serde_json::to_string(&delivery.payload).map_err(|_| {
        StoreError::new(
            ErrorCode::InvalidPayload,
            "delivery payload is not serializable",
        )
    })?;
    let inserted = sqlx::query(
        "INSERT OR IGNORE INTO message_deliveries(
             message_id, sender_task_id, recipient_task_id, recipient_session_id, result_id,
             payload_hash, payload_json, state, consumed_by)
         VALUES (?, ?, ?, ?, ?, ?, ?, 'pending', NULL)",
    )
    .bind(&delivery.message_id)
    .bind(delivery.sender_task_id.as_str())
    .bind(delivery.recipient_task_id.as_str())
    .bind(delivery.recipient_session_id.as_str())
    .bind(delivery.result_id.as_deref())
    .bind(delivery.payload_hash.as_str())
    .bind(payload)
    .execute(&mut **tx)
    .await
    .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "insert delivery", error))?;
    if inserted.rows_affected() == 0 {
        let existing =
            sqlx::query("SELECT payload_hash, state FROM message_deliveries WHERE message_id = ?")
                .bind(&delivery.message_id)
                .fetch_optional(&mut **tx)
                .await
                .map_err(|error| {
                    database_error(ErrorCode::StorageWriteFailed, "read delivery hash", error)
                })?
                .ok_or_else(|| {
                    StoreError::new(ErrorCode::StorageWriteFailed, "delivery row disappeared")
                })?;
        let hash = row_get::<String>(&existing, "payload_hash")?;
        if hash != delivery.payload_hash.as_str() {
            return Err(StoreError::new(
                ErrorCode::DeliveryConflict,
                "the same message id cannot carry different payloads",
            ));
        }
    }
    Ok(())
}

async fn assert_owner(
    tx: &mut Transaction<'_, Sqlite>,
    task_id: &TaskId,
    owner_run_id: &AgentRunId,
    generation: u64,
) -> Result<(), StoreError> {
    let row = sqlx::query(
        "SELECT owner_run_id, generation FROM delegation_task_owners WHERE task_id = ?",
    )
    .bind(task_id.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read owner", error))?
    .ok_or_else(|| StoreError::new(ErrorCode::TaskOwnershipConflict, "task has no owner"))?;
    let current_run = row_get::<String>(&row, "owner_run_id")?;
    let current_generation = to_u64(row_get::<i64>(&row, "generation")?, "generation")?;
    if current_run != owner_run_id.as_str() || current_generation != generation {
        return Err(StoreError::new(
            ErrorCode::TaskOwnershipConflict,
            "task owner generation is stale",
        ));
    }
    Ok(())
}

async fn read_task_revision(
    tx: &mut Transaction<'_, Sqlite>,
    task_id: &TaskId,
) -> Result<u64, StoreError> {
    let row = sqlx::query("SELECT revision FROM delegation_tasks WHERE task_id = ?")
        .bind(task_id.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| database_error(ErrorCode::StorageWriteFailed, "read task", error))?
        .ok_or_else(|| StoreError::new(ErrorCode::TaskNotFound, "task is not admitted"))?;
    to_u64(row_get::<i64>(&row, "revision")?, "task revision")
}

fn read_task_row(row: &sqlx::sqlite::SqliteRow) -> Result<StoredTaskNodeRecord, StoreError> {
    let brief_json: Value = serde_json::from_str(&row_get::<String>(row, "brief_json")?)
        .map_err(|_| StoreError::new(ErrorCode::StorageWriteFailed, "stored brief is invalid"))?;
    let node_json: Value = serde_json::from_str(&row_get::<String>(row, "node_json")?)
        .map_err(|_| StoreError::new(ErrorCode::StorageWriteFailed, "stored node is invalid"))?;
    let parent = row_get::<Option<String>>(row, "parent_task_id")?;
    let depends_on = node_json
        .get("depends_on")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let depends_on = depends_on
        .iter()
        .map(TaskId::parse)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(StoredTaskNodeRecord {
        task_id: TaskId::parse(row_get::<String>(row, "task_id")?)?,
        parent_task_id: parent.map(TaskId::parse).transpose()?,
        role: row_get::<String>(row, "role")?,
        status: row_get::<String>(row, "status")?,
        revision: to_u64(row_get::<i64>(row, "revision")?, "task revision")?,
        depth: u32::try_from(row_get::<i64>(row, "depth")?).map_err(|_| {
            StoreError::new(ErrorCode::StorageWriteFailed, "stored depth is invalid")
        })?,
        depends_on,
        brief_json,
        node_json,
    })
}

fn read_delivery_row(row: &sqlx::sqlite::SqliteRow) -> Result<ParentDeliveryRecord, StoreError> {
    let payload: Value = serde_json::from_str(&row_get::<String>(row, "payload_json")?)
        .map_err(|_| StoreError::new(ErrorCode::StorageWriteFailed, "stored payload is invalid"))?;
    Ok(ParentDeliveryRecord {
        message_id: row_get::<String>(row, "message_id")?,
        sender_task_id: TaskId::parse(row_get::<String>(row, "sender_task_id")?)?,
        recipient_task_id: TaskId::parse(row_get::<String>(row, "recipient_task_id")?)?,
        recipient_session_id: SessionId::parse(row_get::<String>(row, "recipient_session_id")?)?,
        result_id: row_get::<Option<String>>(row, "result_id")?,
        payload_hash: ContentHash::parse(row_get::<String>(row, "payload_hash")?)?,
        payload,
        state: row_get::<String>(row, "state")?,
        consumed_by: row_get::<Option<String>>(row, "consumed_by")?,
    })
}

fn read_worktree_row(row: &sqlx::sqlite::SqliteRow) -> Result<WorktreeRecordRow, StoreError> {
    let scope: Vec<String> = serde_json::from_str(&row_get::<String>(row, "write_scope_json")?)
        .map_err(|_| {
            StoreError::new(
                ErrorCode::StorageWriteFailed,
                "stored write scope is invalid",
            )
        })?;
    let result_fingerprint = row_get::<Option<String>>(row, "result_fingerprint")?
        .map(ContentHash::parse)
        .transpose()?;
    Ok(WorktreeRecordRow {
        worktree_id: row_get::<String>(row, "worktree_id")?,
        task_id: TaskId::parse(row_get::<String>(row, "task_id")?)?,
        run_id: AgentRunId::parse(row_get::<String>(row, "run_id")?)?,
        project_id: ProjectId::parse(row_get::<String>(row, "project_id")?)?,
        base_commit: row_get::<String>(row, "base_commit")?,
        base_branch: row_get::<String>(row, "base_branch")?,
        branch: row_get::<String>(row, "branch")?,
        path: row_get::<String>(row, "path")?,
        write_scope: scope,
        state: row_get::<String>(row, "state")?,
        input_fingerprint: ContentHash::parse(row_get::<String>(row, "input_fingerprint")?)?,
        result_fingerprint,
        generation: to_u64(row_get::<i64>(row, "generation")?, "worktree generation")?,
    })
}
