//! Host-side extension lifecycle: registration, generations, unload and reload.
//!
//! Unloading an extension never discards durable evidence. Reloading creates a
//! new generation, and a late disposer from an older generation cannot remove
//! the replacement. Remounting never authorizes repeating a side effect.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use harness_kernel::{KernelError, ManagedResource, RegistrationToken, ScopedRegistry};
use harness_types::{ErrorCode, PluginInstanceId, ScopeId};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::contracts::{
    CapabilityOffer, ExtensionCapability, ExtensionError, ExtensionInventoryEntry,
    ExtensionManifest, InactiveReason, NegotiatedSession, RestartPolicy, TrustGrant,
};
use crate::transport::{CallOutcome, EnvironmentOverrides, ExtensionTransport};

/// The result of asking the host to load one extension.
#[derive(Clone, Debug, PartialEq)]
pub enum LoadOutcome {
    /// The extension handshook and its registrations are published.
    Active {
        plugin_id: String,
        generation: u64,
        capabilities: Vec<CapabilityOffer>,
        host_methods: Vec<String>,
    },
    /// The extension is known but not active, with the honest reason.
    Inactive {
        plugin_id: String,
        reason: InactiveReason,
        detail: String,
    },
}

/// One known extension and its current activation.
struct ExtensionSlot {
    plugin_id: String,
    executable: PathBuf,
    manifest: ExtensionManifest,
    generation: u64,
    transport: Option<Arc<ExtensionTransport>>,
    tokens: Vec<RegistrationToken>,
    inactive_reason: Option<InactiveReason>,
}

/// The host extension runtime. It owns plugin processes and their scoped
/// registrations so ordered shutdown still works.
pub struct ExtensionRuntime {
    scope_id: ScopeId,
    registry: Mutex<ScopedRegistry>,
    slots: Mutex<BTreeMap<String, ExtensionSlot>>,
    /// Restart policies per plugin id, honoured on reload.
    policies: Mutex<BTreeMap<String, RestartPolicy>>,
    /// Last known executable, manifest and generation per plugin id. Kept after
    /// an unload so a reload can start from a remembered definition.
    sources: Mutex<BTreeMap<String, (PathBuf, ExtensionManifest, u64)>>,
}

impl std::fmt::Debug for ExtensionRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExtensionRuntime")
            .field("scope_id", &self.scope_id)
            .finish_non_exhaustive()
    }
}

impl ExtensionRuntime {
    /// Create a runtime rooted at one scope with no extensions loaded.
    #[must_use]
    pub fn new(scope_id: ScopeId, registry: ScopedRegistry) -> Self {
        Self {
            scope_id,
            registry: Mutex::new(registry),
            slots: Mutex::new(BTreeMap::new()),
            policies: Mutex::new(BTreeMap::new()),
            sources: Mutex::new(BTreeMap::new()),
        }
    }

    #[must_use]
    pub const fn scope_id(&self) -> &ScopeId {
        &self.scope_id
    }

    /// Load an extension from a pinned manifest and an explicit trust grant.
    ///
    /// Every refusal path is explicit: no grant, a digest mismatch, a capability
    /// the user did not allow, an untrusted secret reference, a rejected
    /// handshake or an early exit all produce an `Inactive` outcome with a typed
    /// reason instead of a silent skip.
    #[allow(clippy::too_many_lines)] // One activation path; the refusals are ordered.
    pub async fn load(
        &self,
        executable: impl Into<PathBuf>,
        manifest: ExtensionManifest,
        grant: Option<&TrustGrant>,
        environment: EnvironmentOverrides,
    ) -> LoadOutcome {
        let executable = executable.into();
        let plugin_id = manifest.plugin_id.clone();
        let Some(grant) = grant else {
            return self.inactive(
                plugin_id,
                InactiveReason::NoTrustGrant,
                executable,
                manifest,
            );
        };
        if let Err(error) = grant.verify_manifest(&manifest) {
            let reason = match error.code() {
                ErrorCode::ExtensionDigestMismatch => InactiveReason::DigestMismatch,
                ErrorCode::ExtensionCapabilityMismatch => InactiveReason::CapabilityNotGranted,
                ErrorCode::SecretNotGranted => InactiveReason::SecretNotGranted,
                _ => InactiveReason::NoTrustGrant,
            };
            return self.inactive_with_error(plugin_id, reason, &error, executable, manifest);
        }

        let generation = self.next_generation(&plugin_id).await;
        let connected = ExtensionTransport::connect(
            executable.clone(),
            &manifest,
            grant,
            self.scope_id.clone(),
            generation,
            environment,
        )
        .await;
        let transport = match connected {
            Ok(transport) => Arc::new(transport),
            Err(error) => {
                let reason = match error.code() {
                    ErrorCode::ExtensionDigestMismatch => InactiveReason::DigestMismatch,
                    ErrorCode::ExtensionNotFound => InactiveReason::ExitBeforeHandshake,
                    _ => InactiveReason::HandshakeRejected,
                };
                return self.inactive_with_error(plugin_id, reason, &error, executable, manifest);
            }
        };

        let session = transport.session().clone();
        let mut tokens = Vec::new();
        {
            let mut registry = self.registry.lock().await;
            for offer in &session.capabilities {
                let service_id = match offer.capability {
                    ExtensionCapability::Tools => "extension.tools",
                    ExtensionCapability::ModelProvider => "extension.model_provider",
                    ExtensionCapability::MemoryExtractor => "extension.memory_extractor",
                    ExtensionCapability::Skills => "extension.skills",
                };
                let service_id = format!("{service_id}:{}", session.plugin_id);
                match registry.register(
                    &self.scope_id,
                    service_id,
                    transport.instance_id().clone(),
                    generation,
                ) {
                    Ok(token) => tokens.push(token),
                    Err(error) => {
                        // A duplicate name in the same layer is refused rather
                        // than overwritten.
                        transport.shutdown().await;
                        let duplicate = ExtensionError::new(error.code(), error.to_string());
                        return self.inactive_with_error(
                            plugin_id,
                            InactiveReason::HandshakeRejected,
                            &duplicate,
                            executable,
                            manifest,
                        );
                    }
                }
            }
        }

        let policy = manifest.restart_policy;
        let outcome = LoadOutcome::Active {
            plugin_id: plugin_id.clone(),
            generation,
            capabilities: session.capabilities.clone(),
            host_methods: session.host_methods.clone(),
        };
        let mut slots = self.slots.lock().await;
        let previous = slots.insert(
            plugin_id.clone(),
            ExtensionSlot {
                plugin_id: plugin_id.clone(),
                executable: executable.clone(),
                manifest: manifest.clone(),
                generation,
                transport: Some(Arc::clone(&transport)),
                tokens,
                inactive_reason: None,
            },
        );
        drop(slots);
        // Replacing a live extension retires the previous generation.
        if let Some(previous) = previous
            && let Some(previous_transport) = previous.transport
        {
            previous_transport.shutdown().await;
        }
        self.policies.lock().await.insert(plugin_id.clone(), policy);
        self.sources
            .lock()
            .await
            .insert(plugin_id, (executable, manifest, generation));
        outcome
    }

    /// The next generation for a plugin id. It accounts for both the live slot
    /// and the remembered definition, so a reload after an unload still
    /// produces a strictly newer generation.
    async fn next_generation(&self, plugin_id: &str) -> u64 {
        let live = self
            .slots
            .lock()
            .await
            .get(plugin_id)
            .map_or(0, |slot| slot.generation);
        let remembered = self
            .sources
            .lock()
            .await
            .get(plugin_id)
            .map_or(0, |(_, _, generation)| *generation);
        live.max(remembered).saturating_add(1)
    }

    fn inactive(
        &self,
        plugin_id: String,
        reason: InactiveReason,
        executable: PathBuf,
        manifest: ExtensionManifest,
    ) -> LoadOutcome {
        let detail = reason.as_str().to_owned();
        self.remember_inactive(plugin_id.clone(), reason, executable, manifest);
        LoadOutcome::Inactive {
            plugin_id,
            reason,
            detail,
        }
    }

    fn inactive_with_error(
        &self,
        plugin_id: String,
        reason: InactiveReason,
        error: &ExtensionError,
        executable: PathBuf,
        manifest: ExtensionManifest,
    ) -> LoadOutcome {
        let detail = error.to_string();
        self.remember_inactive(plugin_id.clone(), reason, executable, manifest);
        LoadOutcome::Inactive {
            plugin_id,
            reason,
            detail,
        }
    }

    fn remember_inactive(
        &self,
        plugin_id: String,
        reason: InactiveReason,
        executable: PathBuf,
        manifest: ExtensionManifest,
    ) {
        // The slot map is only touched through the sync path during a refusal,
        // so a blocking lock here cannot deadlock an async caller holding it.
        if let Ok(mut slots) = self.slots.try_lock() {
            let generation = slots.get(&plugin_id).map_or(1, |slot| slot.generation);
            slots.insert(
                plugin_id.clone(),
                ExtensionSlot {
                    plugin_id,
                    executable,
                    manifest,
                    generation,
                    transport: None,
                    tokens: Vec::new(),
                    inactive_reason: Some(reason),
                },
            );
        }
    }

    /// Live transport for one active extension.
    pub async fn transport(&self, plugin_id: &str) -> Option<Arc<ExtensionTransport>> {
        self.slots
            .lock()
            .await
            .get(plugin_id)
            .and_then(|slot| slot.transport.clone())
    }

    /// The generation currently live for one plugin id, if any.
    pub async fn live_generation(&self, plugin_id: &str) -> Option<u64> {
        self.slots
            .lock()
            .await
            .get(plugin_id)
            .filter(|slot| slot.transport.is_some())
            .map(|slot| slot.generation)
    }

    /// Take a generation-bound lease on one active extension.
    ///
    /// A lease is the only way a consumer reaches a plugin process through the
    /// host, and it is bound to the exact generation it was taken from. After an
    /// unload or a reload the lease is refused, so a consumer that cached one
    /// cannot dispatch into a process that is gone or into its replacement.
    pub async fn lease(self: &Arc<Self>, plugin_id: &str) -> Option<ExtensionLease> {
        let slots = self.slots.lock().await;
        let slot = slots.get(plugin_id)?;
        let transport = slot.transport.clone()?;
        Some(ExtensionLease {
            runtime: Arc::clone(self),
            plugin_id: slot.plugin_id.clone(),
            instance_id: transport.instance_id().clone(),
            scope_id: self.scope_id.clone(),
            generation: slot.generation,
            transport,
        })
    }

    /// Unload one extension, giving in-flight calls a bounded chance to settle.
    ///
    /// The slot is removed first, so no new lease or call can be taken while the
    /// drain runs. Calls still pending at the deadline are terminated and
    /// reported as uncertain — an unload never silently drops work that had
    /// already crossed the process boundary.
    pub async fn unload_draining(&self, plugin_id: &str, drain_ms: u64) -> UnloadReport {
        let slot = self.slots.lock().await.remove(plugin_id);
        let Some(slot) = slot else {
            return UnloadReport {
                plugin_id: plugin_id.to_owned(),
                generation: 0,
                inflight: 0,
                drained: true,
                uncertain: 0,
            };
        };
        {
            let mut registry = self.registry.lock().await;
            for token in &slot.tokens {
                registry.undo(token);
            }
        }
        let Some(transport) = slot.transport else {
            return UnloadReport {
                plugin_id: slot.plugin_id,
                generation: slot.generation,
                inflight: 0,
                drained: true,
                uncertain: 0,
            };
        };
        let inflight = transport.inflight();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(drain_ms);
        while transport.inflight() > 0 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let still_pending = transport.inflight();
        // Termination settles anything left as uncertain rather than leaving a
        // caller waiting on a process that no longer exists.
        transport.shutdown().await;
        UnloadReport {
            plugin_id: slot.plugin_id,
            generation: slot.generation,
            inflight,
            drained: still_pending == 0,
            uncertain: still_pending,
        }
    }

    /// The negotiated session for one active extension.
    pub async fn session(&self, plugin_id: &str) -> Option<NegotiatedSession> {
        self.transport(plugin_id)
            .await
            .map(|transport| transport.session().clone())
    }

    /// Unload one extension. Its scoped registrations are removed by exact
    /// token, and the process tree is terminated.
    pub async fn unload(&self, plugin_id: &str) -> bool {
        let slot = self.slots.lock().await.remove(plugin_id);
        let Some(slot) = slot else {
            return false;
        };
        {
            let mut registry = self.registry.lock().await;
            for token in &slot.tokens {
                // Undo targets an exact (instance, generation, registration id),
                // so a late disposer can never remove a replacement.
                registry.undo(token);
            }
        }
        if let Some(transport) = slot.transport {
            transport.shutdown().await;
        }
        true
    }

    /// Reload one extension with a new generation.
    ///
    /// The reload source is remembered separately from the live slot, because
    /// unloading an extension is what a reload starts with.
    pub async fn reload(
        &self,
        plugin_id: &str,
        grant: Option<&TrustGrant>,
        environment: &EnvironmentOverrides,
    ) -> Option<LoadOutcome> {
        let slot = self.slots.lock().await.get(plugin_id).map(|slot| {
            (
                slot.executable.clone(),
                slot.manifest.clone(),
                slot.generation,
            )
        });
        let (executable, manifest, previous_generation) = if let Some(slot) = slot {
            slot
        } else {
            let remembered = self.sources.lock().await.get(plugin_id).cloned()?;
            (remembered.0, remembered.1, remembered.2)
        };
        let policy = self
            .policies
            .lock()
            .await
            .get(plugin_id)
            .copied()
            .unwrap_or(RestartPolicy::Manual);
        if policy == RestartPolicy::Never {
            return Some(LoadOutcome::Inactive {
                plugin_id: plugin_id.to_owned(),
                reason: InactiveReason::RestartRequired,
                detail: "this extension is pinned to a never-restart policy".to_owned(),
            });
        }
        self.unload(plugin_id).await;
        let outcome = self
            .load(executable, manifest, grant, environment.clone())
            .await;
        // The new generation must be strictly newer than the retired one.
        if let (LoadOutcome::Active { generation, .. }, true) = (&outcome, previous_generation > 0)
            && *generation <= previous_generation
        {
            return Some(LoadOutcome::Inactive {
                plugin_id: plugin_id.to_owned(),
                reason: InactiveReason::RestartRequired,
                detail: "reload did not produce a new generation".to_owned(),
            });
        }
        Some(outcome)
    }

    /// The full inventory, including inactive entries and why.
    pub async fn inventory(&self) -> Vec<ExtensionInventoryEntry> {
        let slots = self.slots.lock().await;
        slots
            .values()
            .map(|slot| ExtensionInventoryEntry {
                plugin_id: slot.plugin_id.clone(),
                implementation_version: Some(slot.manifest.implementation_version.clone()),
                state: if slot.transport.is_some() {
                    "active"
                } else {
                    "inactive"
                },
                generation: slot.generation,
                inactive_reason: slot.inactive_reason,
                protocols: crate::contracts::supported_protocol_versions().to_vec(),
                capabilities: slot
                    .manifest
                    .provides
                    .iter()
                    .map(|offer| offer.capability)
                    .collect(),
            })
            .collect()
    }

    /// Terminate every extension process. Used by host shutdown.
    pub async fn shutdown_all(&self) {
        let ids = self
            .slots
            .lock()
            .await
            .values()
            .map(|slot| slot.plugin_id.clone())
            .collect::<Vec<_>>();
        for plugin_id in ids {
            self.unload(&plugin_id).await;
        }
    }

    /// Number of live extension processes. Used to prove nothing is left behind.
    pub async fn live_process_count(&self) -> usize {
        self.slots
            .lock()
            .await
            .values()
            .filter(|slot| slot.transport.is_some())
            .count()
    }
}

impl ManagedResource for ExtensionRuntime {
    fn name(&self) -> &'static str {
        "p6-extension-runtime"
    }

    fn shutdown<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), KernelError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.shutdown_all().await;
            Ok(())
        })
    }

    fn join<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), KernelError>> + Send + 'a>>
    {
        Box::pin(async move { Ok(()) })
    }
}

/// What one unload did to the work that was already in flight.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnloadReport {
    pub plugin_id: String,
    pub generation: u64,
    /// Calls that were in flight when the unload started.
    pub inflight: u64,
    /// Whether every in-flight call settled inside the drain window.
    pub drained: bool,
    /// Calls terminated before they settled. Their outcome is uncertain and the
    /// caller must reconcile rather than retry.
    pub uncertain: u64,
}

impl UnloadReport {
    /// Whether the unload had to cut anything short.
    #[must_use]
    pub const fn cut_short(&self) -> bool {
        !self.drained
    }
}

/// A generation-bound handle on one live extension.
///
/// Holding a lease is not authority: it says *which process* a call would reach,
/// never whether the call is allowed. Every tool call still crosses the M4 gate,
/// and the lease only guarantees that a stale handle cannot reach a replacement
/// process.
#[derive(Clone)]
pub struct ExtensionLease {
    runtime: Arc<ExtensionRuntime>,
    plugin_id: String,
    instance_id: PluginInstanceId,
    scope_id: ScopeId,
    generation: u64,
    transport: Arc<ExtensionTransport>,
}

impl std::fmt::Debug for ExtensionLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExtensionLease")
            .field("plugin_id", &self.plugin_id)
            .field("generation", &self.generation)
            .field("alive", &self.transport.is_alive())
            .finish_non_exhaustive()
    }
}

impl ExtensionLease {
    #[must_use]
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    #[must_use]
    pub const fn instance_id(&self) -> &PluginInstanceId {
        &self.instance_id
    }

    #[must_use]
    pub const fn scope_id(&self) -> &ScopeId {
        &self.scope_id
    }

    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub fn transport(&self) -> &Arc<ExtensionTransport> {
        &self.transport
    }

    /// Whether this lease still points at the live generation.
    pub async fn is_valid(&self) -> bool {
        self.transport.is_alive()
            && self.runtime.live_generation(&self.plugin_id).await == Some(self.generation)
    }

    /// Dispatch one call under this lease.
    ///
    /// Both the generation and the process are re-checked here, so a lease taken
    /// before a reload fails typed instead of reaching the new process with the
    /// old caller's intent.
    pub async fn call(
        &self,
        method: &str,
        payload: Value,
        deadline_ms: u64,
    ) -> Result<CallOutcome, ExtensionError> {
        let id = format!("{}:{}", self.instance_id, self.transport.inflight());
        self.call_with_id(&id, method, payload, deadline_ms).await
    }

    /// Dispatch one call under a caller-chosen id, so the caller can cancel it.
    pub async fn call_with_id(
        &self,
        id: &str,
        method: &str,
        payload: Value,
        deadline_ms: u64,
    ) -> Result<CallOutcome, ExtensionError> {
        if !self.is_valid().await {
            return Err(ExtensionError::new(
                ErrorCode::ServiceUnavailable,
                format!(
                    "extension {} generation {} is no longer live; this lease is stale",
                    self.plugin_id, self.generation
                ),
            ));
        }
        self.transport
            .call_with_id(id, method, payload, deadline_ms)
            .await
    }
}

/// Read a manifest from disk without executing anything or resolving a secret.
pub fn read_manifest(path: &Path) -> Result<ExtensionManifest, ExtensionError> {
    let bytes = std::fs::read(path).map_err(|error| {
        ExtensionError::new(
            ErrorCode::ExtensionNotFound,
            format!("cannot read {}: {error}", path.display()),
        )
    })?;
    ExtensionManifest::from_json_bytes(&bytes)
}

/// The instance id of an extension's registration token, for assertions.
#[must_use]
pub fn token_instance(token: &RegistrationToken) -> &PluginInstanceId {
    &token.instance_id
}
