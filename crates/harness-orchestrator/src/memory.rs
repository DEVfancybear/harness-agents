//! Scoped memory binding for delegated workers.
//!
//! Every binding is host-created, carries the exact asset version it injected,
//! and is re-validated before a worker dispatch. Delegated memory coordinates
//! nothing: task state remains the only authority for whether work completed.

use std::{collections::BTreeSet, sync::Arc};

use harness_memory::{MemoryAction, MemoryGrant, MemoryPrincipal, MemoryService};
use harness_store_sqlite::SqliteStore;
use harness_types::{AgentProfileId, ErrorCode, MemoryAssetId, TaskId};

use crate::contracts::{GrantAction, OrchestratorError, TaskMemoryBinding};

/// What a delegated worker may do with a bound asset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryBoundary {
    /// The binding is visible to the worker and still valid.
    Visible,
    /// The asset moved past the bound version, so the worker must not use it.
    Stale,
    /// The worker has no grant for the requested action.
    Denied,
    /// The asset no longer exists for this worker's scope.
    Missing,
}

/// A host-recorded binding plus the version it pinned.
#[derive(Clone, Debug)]
pub struct DelegatedMemoryBinding {
    pub binding_id: String,
    pub task_id: TaskId,
    pub profile_id: AgentProfileId,
    pub memory_asset_id: MemoryAssetId,
    pub version: u64,
    pub injection_mode: String,
    pub priority: i64,
    pub actions: Vec<GrantAction>,
}

impl DelegatedMemoryBinding {
    #[must_use]
    pub fn to_contract(&self) -> TaskMemoryBinding {
        TaskMemoryBinding {
            profile_id: self.profile_id.clone(),
            task_id: self.task_id.clone(),
            asset_id: self.memory_asset_id.clone(),
            version: self.version,
            injection_mode: self.injection_mode.clone(),
            priority: self.priority,
            actions: self.actions.clone(),
        }
    }
}

/// Host-side memory service used for delegated workers.
pub struct DelegatedMemoryService {
    store: Arc<SqliteStore>,
    memory: MemoryService,
}

impl std::fmt::Debug for DelegatedMemoryService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DelegatedMemoryService")
            .finish_non_exhaustive()
    }
}

impl DelegatedMemoryService {
    #[must_use]
    pub fn new(store: Arc<SqliteStore>) -> Self {
        Self {
            memory: MemoryService::new(Arc::clone(&store)),
            store,
        }
    }

    /// Grant one action on one asset to a worker principal. Only the host can
    /// call this; a worker never grants itself anything.
    pub async fn grant(
        &self,
        owner: &MemoryPrincipal,
        principal_id: &str,
        asset_id: &MemoryAssetId,
        action: MemoryAction,
    ) -> Result<(), OrchestratorError> {
        self.memory
            .grant(
                owner,
                MemoryGrant {
                    principal_id: principal_id.to_owned(),
                    memory_asset_id: Some(asset_id.clone()),
                    project_id: None,
                    allowed_actions: BTreeSet::from([action]),
                    revision: 1,
                    active: true,
                },
            )
            .await
            .map_err(|error| OrchestratorError::new(error.code(), error.to_string()))
    }

    /// Bind a task/profile asset at spawn and record the exact source version.
    pub async fn bind_at_spawn(
        &self,
        binding: DelegatedMemoryBinding,
    ) -> Result<(), OrchestratorError> {
        let actions = binding
            .actions
            .iter()
            .map(|action| action.as_str().to_owned())
            .collect::<Vec<_>>();
        self.store
            .bind_delegation_memory(&harness_store_sqlite::MemoryBindingRow {
                binding_id: binding.binding_id.clone(),
                task_id: binding.task_id.clone(),
                profile_id: binding.profile_id.clone(),
                memory_asset_id: binding.memory_asset_id.clone(),
                version: binding.version,
                injection_mode: binding.injection_mode.clone(),
                priority: binding.priority,
                actions,
                revision: 1,
            })
            .await?;
        Ok(())
    }

    /// Re-validate a binding immediately before worker dispatch. A version that
    /// moved past the bound revision is stale and must not be injected.
    pub async fn validate_before_dispatch(
        &self,
        principal: &MemoryPrincipal,
        binding: &DelegatedMemoryBinding,
    ) -> Result<MemoryBoundary, OrchestratorError> {
        let asset = self
            .memory
            .read(principal, &binding.memory_asset_id)
            .await
            .map_err(|error| OrchestratorError::new(error.code(), error.to_string()))?;
        let Some(asset) = asset else {
            return Ok(MemoryBoundary::Missing);
        };
        if asset.asset.current_version < binding.version {
            return Ok(MemoryBoundary::Stale);
        }
        if asset.asset.current_version > binding.version {
            return Ok(MemoryBoundary::Stale);
        }
        Ok(MemoryBoundary::Visible)
    }

    /// Attempt a read as the worker's own principal. A forged or missing scope
    /// returns `Denied` rather than data.
    pub async fn read_as_worker(
        &self,
        principal: &MemoryPrincipal,
        asset_id: &MemoryAssetId,
    ) -> Result<MemoryBoundary, OrchestratorError> {
        match self.memory.read(principal, asset_id).await {
            Ok(Some(_)) => Ok(MemoryBoundary::Visible),
            Ok(None) => Ok(MemoryBoundary::Missing),
            Err(error) if error.code() == ErrorCode::PolicyDenied => Ok(MemoryBoundary::Denied),
            Err(error) => Err(OrchestratorError::new(error.code(), error.to_string())),
        }
    }

    /// Direct export attempt. Export has its own authorization check, so a
    /// forged scope cannot reach an artifact path either.
    pub async fn export_as_worker(
        &self,
        principal: &MemoryPrincipal,
        asset_id: &MemoryAssetId,
    ) -> Result<MemoryBoundary, OrchestratorError> {
        match self.memory.export_versions(principal, asset_id).await {
            Ok(versions) if versions.is_empty() => Ok(MemoryBoundary::Missing),
            Ok(_) => Ok(MemoryBoundary::Visible),
            Err(error) if error.code() == ErrorCode::PolicyDenied => Ok(MemoryBoundary::Denied),
            Err(error) => Err(OrchestratorError::new(error.code(), error.to_string())),
        }
    }
}
