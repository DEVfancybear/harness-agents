//! Regressions found in a review of the kernel, one measured failure each.

use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use harness_kernel::{
    KernelError, ManagedResource, PluginDescriptor, PluginGraph, ScopedRegistry, ServiceProvider,
    ServiceRequirement, ShutdownCoordinator, ShutdownPhase,
};
use harness_types::{ErrorCode, PluginInstanceId, ScopeId, ServiceContract};

struct Counted {
    name: String,
    shutdowns: AtomicUsize,
    delay: Duration,
}

impl Counted {
    fn new(name: &str, delay_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_owned(),
            shutdowns: AtomicUsize::new(0),
            delay: Duration::from_millis(delay_ms),
        })
    }
}

impl ManagedResource for Counted {
    fn name(&self) -> &str {
        &self.name
    }
    fn shutdown<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<(), KernelError>> + Send + 'a>> {
        Box::pin(async move {
            tokio::time::sleep(self.delay).await;
            self.shutdowns.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
    fn join<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<(), KernelError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

/// The measured failure: `add_scope` inserted before it checked for a
/// duplicate, so the refused call had already moved C under B - and C could
/// see B's services.
#[test]
fn a_refused_duplicate_scope_keeps_its_parent() {
    let (a, b, c) = (
        ScopeId::generate(),
        ScopeId::generate(),
        ScopeId::generate(),
    );
    let mut registry = ScopedRegistry::default();
    registry.add_root(a.clone()).unwrap();
    registry.add_root(b.clone()).unwrap();
    registry.add_scope(c.clone(), Some(a)).unwrap();
    registry
        .register(&b, "secret", PluginInstanceId::generate(), 1)
        .unwrap();
    let refused = registry
        .add_scope(c.clone(), Some(b))
        .expect_err("a scope is added once");
    assert_eq!(refused.code(), ErrorCode::DuplicateRegistration);
    assert!(registry.lookup(&c, "secret").is_none());
}

/// The measured failure: the same refused call could make a scope its own
/// parent, and `lookup` then walked the parent chain forever.
#[test]
fn a_refused_duplicate_scope_cannot_become_its_own_parent() {
    let a = ScopeId::generate();
    let mut registry = ScopedRegistry::default();
    registry.add_root(a.clone()).unwrap();
    assert!(registry.add_scope(a.clone(), Some(a.clone())).is_err());
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(registry.lookup(&a, "missing").is_none());
    });
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(5)),
        Ok(true),
        "lookup must return"
    );
}

/// The measured failure: one plugin offering two API versions of one service
/// was refused as "duplicate providers", although the requirement match is
/// written to pick a version out of one provider's offers.
#[test]
fn one_plugin_may_offer_several_versions_of_a_service() {
    let scope = ScopeId::generate();
    let store = |api_version| ServiceContract {
        service_id: "store".to_owned(),
        api_version,
    };
    let graph = PluginGraph::new(vec![
        PluginDescriptor::new(
            PluginInstanceId::generate(),
            scope.clone(),
            1,
            vec![store(1), store(2)],
            Vec::new(),
        ),
        PluginDescriptor::new(
            PluginInstanceId::generate(),
            scope.clone(),
            1,
            Vec::new(),
            vec![ServiceRequirement::required("store", 2)],
        ),
    ]);
    graph.validate().expect("one provider, two versions");

    // A second plugin offering the service is still a duplicate.
    let duplicate = PluginGraph::new(vec![
        PluginDescriptor::new(
            PluginInstanceId::generate(),
            scope.clone(),
            1,
            vec![store(1)],
            Vec::new(),
        ),
        PluginDescriptor::new(
            PluginInstanceId::generate(),
            scope,
            1,
            vec![store(2)],
            Vec::new(),
        ),
    ]);
    assert_eq!(
        duplicate.validate().expect_err("two providers").code(),
        ErrorCode::DuplicateRegistration
    );
}

/// The measured failure: a shutdown dropped by a timeout left nothing
/// behind, so the retry closed again every resource the first attempt had
/// already closed.
#[tokio::test]
async fn a_cancelled_shutdown_resumes_without_closing_twice() {
    let fast = Counted::new("fast", 0);
    let slow = Counted::new("slow", 300);
    let coordinator = ShutdownCoordinator::new(vec![
        (
            ShutdownPhase::Consumer,
            slow.clone() as Arc<dyn ManagedResource>,
        ),
        (
            ShutdownPhase::Consumer,
            fast.clone() as Arc<dyn ManagedResource>,
        ),
    ]);
    let first = tokio::time::timeout(Duration::from_millis(100), coordinator.shutdown()).await;
    assert!(first.is_err(), "the slow resource outlasts the timeout");
    let report = coordinator.shutdown().await;
    assert_eq!(fast.shutdowns.load(Ordering::SeqCst), 1);
    assert_eq!(slow.shutdowns.load(Ordering::SeqCst), 1);
    // What the cancelled attempt closed is still in the report.
    assert_eq!(
        report.closed,
        vec!["shutdown:fast", "join:fast", "shutdown:slow", "join:slow"]
    );
    assert_eq!(coordinator.shutdown().await, report);
}

/// The measured failure: a provider lost twice shut its resource down twice,
/// and the second report named only the calls still active at that point.
#[tokio::test]
async fn a_provider_lost_twice_releases_its_resource_once() {
    let resource = Counted::new("provider", 0);
    let provider = ServiceProvider::new("svc", 1, resource.clone());
    let lease = provider.lease();
    let call = lease.begin_call("call-1").unwrap();

    let draining = {
        let provider = Arc::clone(&provider);
        tokio::spawn(async move { provider.lose_and_drain().await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        lease.begin_call("call-2").is_err(),
        "a lost provider admits no call"
    );
    call.settle();
    let first = draining.await.unwrap();
    let second = provider.lose_and_drain().await;

    assert_eq!(resource.shutdowns.load(Ordering::SeqCst), 1);
    assert_eq!(first.outcome_uncertainties, vec!["call-1".to_owned()]);
    assert_eq!(second, first);
}
