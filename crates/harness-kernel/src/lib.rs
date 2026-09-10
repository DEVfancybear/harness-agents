#![forbid(unsafe_code)]

//! P1 plugin composition, scoped registry, resource ownership, and shutdown.

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};

use harness_types::{ErrorCode, PluginInstanceId, ScopeId, ServiceContract};
use thiserror::Error;
use tokio::sync::{Notify, OnceCell};

/// Stable kernel failures; callers can branch on the code without prose.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{code}: {message}")]
pub struct KernelError {
    code: ErrorCode,
    message: String,
}

impl KernelError {
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.code
    }
}

/// A required or optional versioned service contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceRequirement {
    pub service_id: String,
    pub api_version: u16,
    pub optional: bool,
}

impl ServiceRequirement {
    #[must_use]
    pub fn required(service_id: impl Into<String>, api_version: u16) -> Self {
        Self {
            service_id: service_id.into(),
            api_version,
            optional: false,
        }
    }

    #[must_use]
    pub fn optional(service_id: impl Into<String>, api_version: u16) -> Self {
        Self {
            service_id: service_id.into(),
            api_version,
            optional: true,
        }
    }
}

/// A compiled-in plugin configuration accepted by the P1 kernel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginDescriptor {
    pub instance_id: PluginInstanceId,
    pub scope_id: ScopeId,
    pub generation: u64,
    pub provides: Vec<ServiceContract>,
    pub requires: Vec<ServiceRequirement>,
}

impl PluginDescriptor {
    #[must_use]
    pub fn new(
        instance_id: PluginInstanceId,
        scope_id: ScopeId,
        generation: u64,
        provides: Vec<ServiceContract>,
        requires: Vec<ServiceRequirement>,
    ) -> Self {
        Self {
            instance_id,
            scope_id,
            generation,
            provides,
            requires,
        }
    }
}

/// Required composition validation before any input is admitted.
#[derive(Clone, Debug, Default)]
pub struct PluginGraph {
    descriptors: Vec<PluginDescriptor>,
}

impl PluginGraph {
    #[must_use]
    pub fn new(descriptors: Vec<PluginDescriptor>) -> Self {
        Self { descriptors }
    }

    pub fn validate(&self) -> Result<(), KernelError> {
        let mut by_instance = BTreeMap::new();
        let mut providers: BTreeMap<(ScopeId, String), Vec<&PluginDescriptor>> = BTreeMap::new();
        for descriptor in &self.descriptors {
            if descriptor.generation == 0 {
                return Err(KernelError::new(
                    ErrorCode::InvalidPayload,
                    "plugin generation must start at 1",
                ));
            }
            if by_instance
                .insert(descriptor.instance_id.clone(), descriptor)
                .is_some()
            {
                return Err(KernelError::new(
                    ErrorCode::DuplicateRegistration,
                    "plugin instance is declared more than once",
                ));
            }
            for service in &descriptor.provides {
                service.validate().map_err(|error| {
                    KernelError::new(
                        error.code(),
                        format!("provided service is invalid: {error}"),
                    )
                })?;
                providers
                    .entry((descriptor.scope_id.clone(), service.service_id.clone()))
                    .or_default()
                    .push(descriptor);
            }
        }
        for ((_, service_id), candidates) in &providers {
            if candidates.len() > 1 {
                return Err(KernelError::new(
                    ErrorCode::DuplicateRegistration,
                    format!("service {service_id} has duplicate providers in one scope"),
                ));
            }
        }

        let mut edges: BTreeMap<PluginInstanceId, Vec<PluginInstanceId>> = BTreeMap::new();
        for descriptor in &self.descriptors {
            let dependencies = edges.entry(descriptor.instance_id.clone()).or_default();
            for requirement in &descriptor.requires {
                if requirement.api_version == 0 || requirement.service_id.trim().is_empty() {
                    return Err(KernelError::new(
                        ErrorCode::InvalidPayload,
                        "required service contract must have a name and version",
                    ));
                }
                let candidates =
                    providers.get(&(descriptor.scope_id.clone(), requirement.service_id.clone()));
                let compatible = candidates.and_then(|values| {
                    values.iter().copied().find(|candidate| {
                        candidate.provides.iter().any(|service| {
                            service.service_id == requirement.service_id
                                && service.api_version == requirement.api_version
                        })
                    })
                });
                match compatible {
                    Some(provider) => dependencies.push(provider.instance_id.clone()),
                    None if requirement.optional => {}
                    None if candidates.is_some() => {
                        return Err(KernelError::new(
                            ErrorCode::IncompatibleService,
                            format!(
                                "service {} is present but has no compatible API version",
                                requirement.service_id
                            ),
                        ));
                    }
                    None => {
                        return Err(KernelError::new(
                            ErrorCode::MissingRequiredService,
                            format!("required service {} is absent", requirement.service_id),
                        ));
                    }
                }
            }
        }
        detect_cycle(&edges)
    }
}

fn detect_cycle(
    edges: &BTreeMap<PluginInstanceId, Vec<PluginInstanceId>>,
) -> Result<(), KernelError> {
    fn visit(
        node: &PluginInstanceId,
        edges: &BTreeMap<PluginInstanceId, Vec<PluginInstanceId>>,
        visiting: &mut BTreeSet<PluginInstanceId>,
        visited: &mut BTreeSet<PluginInstanceId>,
    ) -> Result<(), KernelError> {
        if visited.contains(node) {
            return Ok(());
        }
        if !visiting.insert(node.clone()) {
            return Err(KernelError::new(
                ErrorCode::PluginCycle,
                format!("plugin dependency cycle includes {node}"),
            ));
        }
        if let Some(children) = edges.get(node) {
            for child in children {
                visit(child, edges, visiting, visited)?;
            }
        }
        visiting.remove(node);
        visited.insert(node.clone());
        Ok(())
    }

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for node in edges.keys() {
        visit(node, edges, &mut visiting, &mut visited)?;
    }
    Ok(())
}

/// An exact registration identity. It is required to undo a registration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistrationToken {
    pub registration_id: u64,
    pub instance_id: PluginInstanceId,
    pub generation: u64,
    pub scope_id: ScopeId,
    pub service_id: String,
}

/// A resolved registration returned by nearest-scope lookup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceRegistration {
    pub token: RegistrationToken,
}

/// Scoped service registry with no implicit sibling visibility.
#[derive(Default)]
pub struct ScopedRegistry {
    parents: BTreeMap<ScopeId, Option<ScopeId>>,
    registrations: BTreeMap<(ScopeId, String), ServiceRegistration>,
    next_registration_id: u64,
}

impl ScopedRegistry {
    pub fn add_root(&mut self, scope_id: ScopeId) -> Result<(), KernelError> {
        self.add_scope(scope_id, None)
    }

    pub fn add_scope(
        &mut self,
        scope_id: ScopeId,
        parent: Option<ScopeId>,
    ) -> Result<(), KernelError> {
        if let Some(parent) = &parent
            && !self.parents.contains_key(parent)
        {
            return Err(KernelError::new(
                ErrorCode::InvalidPayload,
                "scope parent has not been registered",
            ));
        }
        if self.parents.insert(scope_id, parent).is_some() {
            return Err(KernelError::new(
                ErrorCode::DuplicateRegistration,
                "scope already exists",
            ));
        }
        Ok(())
    }

    pub fn register(
        &mut self,
        scope_id: &ScopeId,
        service_id: impl Into<String>,
        instance_id: PluginInstanceId,
        generation: u64,
    ) -> Result<RegistrationToken, KernelError> {
        if generation == 0 || !self.parents.contains_key(scope_id) {
            return Err(KernelError::new(
                ErrorCode::InvalidPayload,
                "registration has an unknown scope or zero generation",
            ));
        }
        let service_id = service_id.into();
        if service_id.trim().is_empty() {
            return Err(KernelError::new(
                ErrorCode::InvalidPayload,
                "service name must not be empty",
            ));
        }
        let key = (scope_id.clone(), service_id.clone());
        if self.registrations.contains_key(&key) {
            return Err(KernelError::new(
                ErrorCode::DuplicateRegistration,
                "one scope cannot register the same service twice",
            ));
        }
        self.next_registration_id = self.next_registration_id.checked_add(1).ok_or_else(|| {
            KernelError::new(ErrorCode::ShutdownFailed, "registration ID overflow")
        })?;
        let token = RegistrationToken {
            registration_id: self.next_registration_id,
            instance_id,
            generation,
            scope_id: scope_id.clone(),
            service_id,
        };
        self.registrations.insert(
            key,
            ServiceRegistration {
                token: token.clone(),
            },
        );
        Ok(token)
    }

    #[must_use]
    pub fn lookup(&self, scope_id: &ScopeId, service_id: &str) -> Option<ServiceRegistration> {
        let mut current = Some(scope_id.clone());
        while let Some(scope) = current {
            if let Some(registration) = self
                .registrations
                .get(&(scope.clone(), service_id.to_owned()))
            {
                return Some(registration.clone());
            }
            current = self.parents.get(&scope).cloned().flatten();
        }
        None
    }

    /// Late cleanup is harmless unless it exactly owns the currently visible
    /// registration. It cannot remove a replacement with the same name.
    pub fn undo(&mut self, token: &RegistrationToken) -> bool {
        let key = (token.scope_id.clone(), token.service_id.clone());
        self.registrations
            .get(&key)
            .is_some_and(|current| current.token == *token)
            && self.registrations.remove(&key).is_some()
    }
}

/// The asynchronous cleanup contract used by P1 resources. It uses an
/// object-safe boxed future rather than assuming an async trait object ABI.
pub trait ManagedResource: Send + Sync {
    fn name(&self) -> &str;
    fn shutdown<'a>(&'a self)
    -> Pin<Box<dyn Future<Output = Result<(), KernelError>> + Send + 'a>>;
    fn join<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<(), KernelError>> + Send + 'a>>;
}

/// A report that retains all cleanup errors and order evidence.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ShutdownReport {
    pub closed: Vec<String>,
    pub errors: Vec<String>,
    pub outcome_uncertainties: Vec<String>,
}

/// Resources acquired while mounting a plugin but not yet visible to consumers.
#[derive(Default)]
pub struct ResourceSet {
    resources: Vec<Arc<dyn ManagedResource>>,
    published: bool,
}

impl ResourceSet {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn collect(&mut self, resource: Arc<dyn ManagedResource>) {
        self.resources.push(resource);
    }

    #[must_use]
    pub const fn is_published(&self) -> bool {
        self.published
    }

    pub fn publish(&mut self) {
        self.published = true;
    }

    /// Roll back in reverse acquisition order and join every resource.
    pub async fn rollback(&mut self) -> ShutdownReport {
        self.published = false;
        let resources = std::mem::take(&mut self.resources);
        close_resources(resources.into_iter().rev()).await
    }
}

/// Run a mount build. Resources are not published if the builder returns an
/// error, and its cleanup is awaited before that original error is returned.
pub async fn mount_resources<F>(build: F) -> Result<ResourceSet, KernelError>
where
    F: FnOnce(&mut ResourceSet) -> Result<(), KernelError>,
{
    let mut resources = ResourceSet::new();
    match build(&mut resources) {
        Ok(()) => {
            resources.publish();
            Ok(resources)
        }
        Err(error) => {
            let report = resources.rollback().await;
            if report.errors.is_empty() {
                Err(error)
            } else {
                Err(KernelError::new(
                    error.code(),
                    format!("{}; rollback errors: {}", error, report.errors.join("; ")),
                ))
            }
        }
    }
}

/// A generation-bound provider that can drain controlled service calls.
pub struct ServiceProvider {
    service_id: String,
    generation: AtomicU64,
    healthy: AtomicBool,
    in_flight: AtomicUsize,
    active_calls: Mutex<BTreeSet<String>>,
    drained: Notify,
    resource: Arc<dyn ManagedResource>,
}

impl ServiceProvider {
    #[must_use]
    pub fn new(
        service_id: impl Into<String>,
        generation: u64,
        resource: Arc<dyn ManagedResource>,
    ) -> Arc<Self> {
        Arc::new(Self {
            service_id: service_id.into(),
            generation: AtomicU64::new(generation),
            healthy: AtomicBool::new(true),
            in_flight: AtomicUsize::new(0),
            active_calls: Mutex::new(BTreeSet::new()),
            drained: Notify::new(),
            resource,
        })
    }

    #[must_use]
    pub fn lease(self: &Arc<Self>) -> ServiceLease {
        ServiceLease {
            provider: Arc::clone(self),
            generation: self.generation.load(Ordering::Acquire),
        }
    }

    fn begin_call(
        self: &Arc<Self>,
        lease_generation: u64,
        call_id: String,
    ) -> Result<ServiceCall, KernelError> {
        // Serialise admission with provider loss. Without this guard a call
        // could pass the health check while loss is snapshotting active calls,
        // leaving the drain report unaware of an in-flight operation.
        let mut active_calls = self
            .active_calls
            .lock()
            .expect("service call mutex is not poisoned");
        if !self.healthy.load(Ordering::Acquire)
            || self.generation.load(Ordering::Acquire) != lease_generation
        {
            return Err(KernelError::new(
                ErrorCode::ServiceUnavailable,
                format!(
                    "service {} is not available for this lease",
                    self.service_id
                ),
            ));
        }
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        if !self.healthy.load(Ordering::Acquire)
            || self.generation.load(Ordering::Acquire) != lease_generation
        {
            if self.in_flight.fetch_sub(1, Ordering::AcqRel) == 1 {
                self.drained.notify_waiters();
            }
            return Err(KernelError::new(
                ErrorCode::ServiceUnavailable,
                format!(
                    "service {} changed generation during admission",
                    self.service_id
                ),
            ));
        }
        active_calls.insert(call_id.clone());
        drop(active_calls);
        Ok(ServiceCall {
            provider: Arc::clone(self),
            call_id,
            finished: false,
        })
    }

    fn finish_call(&self, call_id: &str) {
        self.active_calls
            .lock()
            .expect("service call mutex is not poisoned")
            .remove(call_id);
        if self.in_flight.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.drained.notify_waiters();
        }
    }

    /// Stop new calls, invalidate old leases, wait for active calls, then
    /// release the owned resource. Any active call at loss becomes uncertain.
    pub async fn lose_and_drain(&self) -> ShutdownReport {
        let uncertainties = {
            let active_calls = self
                .active_calls
                .lock()
                .expect("service call mutex is not poisoned");
            self.healthy.store(false, Ordering::Release);
            self.generation.fetch_add(1, Ordering::AcqRel);
            active_calls.iter().cloned().collect::<Vec<_>>()
        };
        loop {
            let notified = self.drained.notified();
            if self.in_flight.load(Ordering::Acquire) == 0 {
                break;
            }
            notified.await;
        }
        let mut report = close_resources(std::iter::once(Arc::clone(&self.resource))).await;
        report.outcome_uncertainties = uncertainties;
        report
    }
}

/// A client handle that is invalid once its provider generation changes.
#[derive(Clone)]
pub struct ServiceLease {
    provider: Arc<ServiceProvider>,
    generation: u64,
}

impl ServiceLease {
    pub fn begin_call(&self, call_id: impl Into<String>) -> Result<ServiceCall, KernelError> {
        self.provider.begin_call(self.generation, call_id.into())
    }
}

/// A controlled in-flight call. Drop drains it without treating its outcome as
/// settled; provider loss records uncertainty from the active set.
pub struct ServiceCall {
    provider: Arc<ServiceProvider>,
    call_id: String,
    finished: bool,
}

impl ServiceCall {
    pub fn settle(mut self) {
        self.provider.finish_call(&self.call_id);
        self.finished = true;
    }
}

impl Drop for ServiceCall {
    fn drop(&mut self) {
        if !self.finished {
            self.provider.finish_call(&self.call_id);
            self.finished = true;
        }
    }
}

/// Shutdown dependency phase. Consumers must drain before providers; storage is
/// always final.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ShutdownPhase {
    Consumer,
    Provider,
    Store,
}

struct ShutdownItem {
    phase: ShutdownPhase,
    resource: Arc<dyn ManagedResource>,
}

/// A concurrent shutdown coordinator with a single shared completion result.
pub struct ShutdownCoordinator {
    items: Vec<ShutdownItem>,
    completion: OnceCell<ShutdownReport>,
}

impl ShutdownCoordinator {
    #[must_use]
    pub fn new(items: Vec<(ShutdownPhase, Arc<dyn ManagedResource>)>) -> Self {
        Self {
            items: items
                .into_iter()
                .map(|(phase, resource)| ShutdownItem { phase, resource })
                .collect(),
            completion: OnceCell::new(),
        }
    }

    pub async fn shutdown(&self) -> ShutdownReport {
        self.completion
            .get_or_init(|| async {
                let mut report = ShutdownReport::default();
                for phase in [
                    ShutdownPhase::Consumer,
                    ShutdownPhase::Provider,
                    ShutdownPhase::Store,
                ] {
                    let phase_resources = self
                        .items
                        .iter()
                        .filter(|item| item.phase == phase)
                        .map(|item| Arc::clone(&item.resource))
                        .collect::<Vec<_>>();
                    let phase_report = close_resources(phase_resources.into_iter().rev()).await;
                    report.closed.extend(phase_report.closed);
                    report.errors.extend(phase_report.errors);
                    report
                        .outcome_uncertainties
                        .extend(phase_report.outcome_uncertainties);
                }
                report
            })
            .await
            .clone()
    }
}

async fn close_resources<I>(resources: I) -> ShutdownReport
where
    I: IntoIterator<Item = Arc<dyn ManagedResource>>,
{
    let mut report = ShutdownReport::default();
    for resource in resources {
        let name = resource.name().to_owned();
        match resource.shutdown().await {
            Ok(()) => report.closed.push(format!("shutdown:{name}")),
            Err(error) => report.errors.push(format!("shutdown:{name}:{error}")),
        }
        match resource.join().await {
            Ok(()) => report.closed.push(format!("join:{name}")),
            Err(error) => report.errors.push(format!("join:{name}:{error}")),
        }
    }
    report
}
