//! Bounded NDJSON stdio transport for external extensions.
//!
//! The host starts a pinned executable with a minimal environment, performs a
//! handshake before admitting any work, and then routes requests by unique id.
//! Every failure mode is bounded: oversize frames, floods, duplicate ids,
//! ignored cancels and crashes all end in a typed error or an explicit
//! uncertain outcome, never in an unbounded buffer or a synthesized success.
//!
//! Transport isolation here is a protocol boundary, **not** OS sandboxing. This
//! module does not confine untrusted native code.

use std::{
    collections::HashMap,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use harness_kernel::{KernelError, ManagedResource};
use harness_types::{ContentHash, ErrorCode, PluginInstanceId, ScopeId};
use process_wrap::tokio::{CommandWrap, KillOnDrop};
use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::ChildStdin,
    sync::oneshot,
    time::timeout,
};

use crate::contracts::{
    CANCEL_GRACE_MS, DEFAULT_CALL_TIMEOUT_MS, ENVIRONMENT_ALLOWLIST, ExtensionError,
    ExtensionFrame, ExtensionHandshake, ExtensionManifest, FrameKind, HANDSHAKE_TIMEOUT_MS,
    HostHandshakeOffer, MAX_FRAME_BYTES, MAX_INFLIGHT_CALLS, MAX_STDERR_BYTES, NegotiatedSession,
    TrustGrant,
};

#[cfg(windows)]
use process_wrap::tokio::JobObject;
#[cfg(unix)]
use process_wrap::tokio::ProcessSession;

/// Environment overrides the host deliberately passes to one plugin. Values come
/// from the host, and a secret value is never placed here.
pub type EnvironmentOverrides = HashMap<String, String>;

/// How one call ended.
#[derive(Clone, Debug, PartialEq)]
pub enum CallOutcome {
    /// The plugin answered. The payload is plugin data, not host authority.
    Answered { payload: Value },
    /// The plugin reported a protocol-level error.
    PluginError { code: String, message: String },
    /// The plugin acknowledged a host cancel instead of completing the work.
    /// The side effect did not happen, and the caller must not retry blind.
    Canceled { reason: String },
    /// The call crossed a side-effect boundary without a settled answer.
    Uncertain { reason: String },
}

impl CallOutcome {
    #[must_use]
    pub fn answered(&self) -> Option<&Value> {
        match self {
            Self::Answered { payload } => Some(payload),
            Self::PluginError { .. } | Self::Canceled { .. } | Self::Uncertain { .. } => None,
        }
    }

    #[must_use]
    pub fn is_uncertain(&self) -> bool {
        matches!(self, Self::Uncertain { .. })
    }
}

/// What happened to a cancel request.
///
/// The host promises it *sent* the cancel and that it stopped waiting at a
/// bound. It cannot promise the plugin obeyed, so the two outcomes are named
/// separately rather than folded into one success.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancelOutcome {
    /// The plugin settled the call inside the grace window.
    Acknowledged,
    /// The plugin did not settle in time; its process tree was terminated and
    /// the in-flight call is uncertain.
    Ignored,
}

/// Host methods a plugin may call back into. Only an allowlisted method can
/// reach an implementation, and each implementation decides what it returns.
pub trait HostMethodHandler: Send + Sync {
    fn call<'a>(
        &'a self,
        method: &'a str,
        payload: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, ExtensionError>> + Send + 'a>>;
}

/// The default handler: it answers only the echo method and refuses everything
/// else, so an unimplemented host method can never quietly succeed.
#[derive(Clone, Copy, Debug, Default)]
pub struct EchoOnlyHost;

impl HostMethodHandler for EchoOnlyHost {
    fn call<'a>(
        &'a self,
        method: &'a str,
        payload: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, ExtensionError>> + Send + 'a>> {
        Box::pin(async move {
            match method {
                "host.echo" => Ok(payload),
                other => Err(ExtensionError::new(
                    ErrorCode::HostMethodDenied,
                    format!("host method {other} is not implemented in this build"),
                )),
            }
        })
    }
}

type PendingMap = Arc<Mutex<HashMap<String, oneshot::Sender<CallOutcome>>>>;
type SharedWriter = Arc<tokio::sync::Mutex<ChildStdin>>;
type SharedChild = Arc<Mutex<Option<Box<dyn process_wrap::tokio::ChildWrapper>>>>;

struct InflightGuard(Arc<AtomicU64>);

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A call future being dropped leaves its outcome unknown. In that case the
/// plugin process is terminated so abandoned work cannot outlive the host's
/// inflight accounting or race a later call with the same id.
struct PendingCallGuard {
    id: String,
    pending: PendingMap,
    child: SharedChild,
    dead: Arc<AtomicBool>,
}

impl Drop for PendingCallGuard {
    fn drop(&mut self) {
        let should_terminate = if let Ok(mut pending) = self.pending.lock() {
            if pending.remove(&self.id).is_none() {
                false
            } else {
                self.dead.store(true, Ordering::Release);
                for (_, sender) in pending.drain() {
                    let _ = sender.send(CallOutcome::Uncertain {
                        reason: "the extension call future was canceled by its caller".to_owned(),
                    });
                }
                true
            }
        } else {
            self.dead.store(true, Ordering::Release);
            true
        };
        if !should_terminate {
            return;
        }
        let mut child = match self.child.lock() {
            Ok(mut slot) => slot.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        let Some(mut child) = child.take() else {
            return;
        };
        // Dispatch termination synchronously from Drop; scheduling an async
        // task first could let the abandoned plugin keep running after the
        // caller has already observed cancellation.
        let _ = child.start_kill();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                loop {
                    if !matches!(child.try_wait(), Ok(None))
                        || tokio::time::Instant::now() >= deadline
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            });
        }
    }
}

/// An external extension process after a successful handshake.
pub struct ExtensionTransport {
    instance_id: PluginInstanceId,
    scope_id: ScopeId,
    generation: u64,
    session: NegotiatedSession,
    executable: PathBuf,
    child: SharedChild,
    writer: SharedWriter,
    pending: PendingMap,
    next_id: AtomicU64,
    inflight: Arc<AtomicU64>,
    stderr_tail: Arc<Mutex<String>>,
    host: Arc<dyn HostMethodHandler>,
    /// Set once the process tree is gone. A handle that outlives its process
    /// must fail typed rather than write into a closed pipe and call the result
    /// a transport error.
    dead: Arc<AtomicBool>,
}

impl std::fmt::Debug for ExtensionTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExtensionTransport")
            .field("plugin_id", &self.session.plugin_id)
            .field("generation", &self.generation)
            .field("inflight", &self.inflight.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl ExtensionTransport {
    /// Start a trusted plugin and complete the handshake.
    ///
    /// The digest in `grant` must equal the manifest digest and the executable's
    /// real digest, otherwise nothing is started.
    pub async fn connect(
        executable: impl Into<PathBuf>,
        manifest: &ExtensionManifest,
        grant: &TrustGrant,
        scope_id: ScopeId,
        generation: u64,
        environment: EnvironmentOverrides,
    ) -> Result<Self, ExtensionError> {
        grant.validate()?;
        grant.verify_manifest(manifest)?;
        let executable = executable.into();
        let actual = executable_digest(&executable)?;
        if actual != manifest.executable_digest {
            return Err(ExtensionError::new(
                ErrorCode::ExtensionDigestMismatch,
                "the plugin executable on disk does not match the manifest digest",
            ));
        }
        Self::connect_verified(&executable, manifest, scope_id, generation, environment).await
    }

    /// Start a plugin whose digest was already verified against a pinned
    /// manifest. Split out so tests can exercise transport failures with a
    /// deliberately hostile fixture.
    #[allow(clippy::too_many_lines)] // Ordered activation; each refusal must precede the next step.
    pub async fn connect_verified(
        executable: &Path,
        manifest: &ExtensionManifest,
        scope_id: ScopeId,
        generation: u64,
        environment: EnvironmentOverrides,
    ) -> Result<Self, ExtensionError> {
        manifest.validate()?;
        let instance_id = PluginInstanceId::generate();
        let mut wrapped = CommandWrap::with_new(executable, |command| {
            // Minimal environment: only allowlisted names, plus explicit
            // overrides the host chose to pass. A secret value is never placed
            // here and the host environment is not inherited wholesale.
            command.env_clear();
            for name in ENVIRONMENT_ALLOWLIST {
                if let Ok(value) = std::env::var(name) {
                    command.env(name, value);
                }
            }
            for (name, value) in &environment {
                command.env(name, value);
            }
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
        });
        wrapped.wrap(KillOnDrop);
        #[cfg(windows)]
        wrapped.wrap(JobObject);
        #[cfg(unix)]
        wrapped.wrap(ProcessSession);
        let mut child = wrapped.spawn().map_err(|error| {
            ExtensionError::new(
                ErrorCode::ExtensionNotFound,
                format!("cannot start the plugin process: {error}"),
            )
        })?;
        let stdin = child.stdin().take().ok_or_else(|| {
            ExtensionError::new(
                ErrorCode::ExtensionProtocolError,
                "plugin stdin is unavailable",
            )
        })?;
        let stdout = child.stdout().take().ok_or_else(|| {
            ExtensionError::new(
                ErrorCode::ExtensionProtocolError,
                "plugin stdout is unavailable",
            )
        })?;
        let stderr = child.stderr().take().ok_or_else(|| {
            ExtensionError::new(
                ErrorCode::ExtensionProtocolError,
                "plugin stderr is unavailable",
            )
        })?;

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let stderr_tail = Arc::new(Mutex::new(String::new()));
        let host: Arc<dyn HostMethodHandler> = Arc::new(EchoOnlyHost);

        let transport = Self {
            instance_id,
            scope_id,
            generation,
            session: NegotiatedSession {
                protocol_version: 0,
                plugin_id: manifest.plugin_id.clone(),
                implementation_version: manifest.implementation_version.clone(),
                executable_digest: manifest.executable_digest.clone(),
                capabilities: Vec::new(),
                host_methods: Vec::new(),
                config_schema_version: manifest.config_schema_version,
            },
            executable: executable.to_path_buf(),
            child: Arc::new(Mutex::new(Some(child))),
            writer: Arc::new(tokio::sync::Mutex::new(stdin)),
            pending: Arc::clone(&pending),
            next_id: AtomicU64::new(1),
            inflight: Arc::new(AtomicU64::new(0)),
            stderr_tail: Arc::clone(&stderr_tail),
            host: Arc::clone(&host),
            dead: Arc::new(AtomicBool::new(false)),
        };
        transport.spawn_stdout_reader(stdout, Arc::clone(&pending), Arc::clone(&transport.dead));
        Self::spawn_stderr_reader(stderr, stderr_tail);

        let offer = HostHandshakeOffer::default();
        let reply = transport
            .call_with_deadline(
                "handshake",
                serde_json::json!({
                    "protocol_min": offer.protocol_min,
                    "protocol_max": offer.protocol_max,
                    "host_api_version": offer.host_api_version,
                    "event_schema_version": offer.event_schema_version,
                    "allowed_host_methods": offer.allowed_host_methods,
                    "max_frame_bytes": offer.max_frame_bytes,
                    "max_inflight_calls": offer.max_inflight_calls,
                }),
                HANDSHAKE_TIMEOUT_MS,
            )
            .await?;
        let payload = match reply {
            CallOutcome::Answered { payload } => payload,
            CallOutcome::PluginError { code, message } => {
                transport.shutdown().await;
                return Err(ExtensionError::new(
                    ErrorCode::ExtensionProtocolUnsupported,
                    format!("plugin refused the handshake: {code}: {message}"),
                ));
            }
            CallOutcome::Canceled { reason } => {
                transport.shutdown().await;
                return Err(ExtensionError::new(
                    ErrorCode::ExtensionProtocolError,
                    format!("plugin cancelled the handshake: {reason}"),
                ));
            }
            CallOutcome::Uncertain { reason } => {
                transport.shutdown().await;
                return Err(ExtensionError::new(
                    ErrorCode::ExtensionProtocolError,
                    format!("plugin handshake did not settle: {reason}"),
                ));
            }
        };
        let handshake: ExtensionHandshake = serde_json::from_value(payload).map_err(|_| {
            ExtensionError::new(
                ErrorCode::ExtensionProtocolError,
                "plugin handshake reply is not a valid handshake",
            )
        })?;
        let session = match NegotiatedSession::negotiate(&offer, manifest, &handshake) {
            Ok(session) => session,
            Err(error) => {
                transport.shutdown().await;
                return Err(error);
            }
        };
        let mut transport = transport;
        transport.session = session;
        Ok(transport)
    }

    fn spawn_stdout_reader(
        &self,
        stdout: tokio::process::ChildStdout,
        pending: PendingMap,
        dead: Arc<AtomicBool>,
    ) {
        let host = Arc::clone(&self.host);
        let writer = Arc::clone(&self.writer);
        tokio::spawn(async move {
            let mut lines = CappedLines::new(stdout, MAX_FRAME_BYTES);
            loop {
                let Ok(Some((segment, overflow))) = lines.next_line().await else {
                    break;
                };
                // A frame larger than the limit was drained and refused without
                // ever being held in memory.
                if overflow {
                    continue;
                }
                let Ok(frame) = ExtensionFrame::decode_line(&segment) else {
                    continue;
                };
                match frame.kind {
                    FrameKind::Message => {
                        // Plugin-initiated call: only allowlisted host methods
                        // are reachable, and the answer goes back on the wire.
                        let reply = match frame.method.as_deref() {
                            Some(method) => match host.call(method, frame.payload.clone()).await {
                                Ok(value) => ExtensionFrame::response(frame.id.clone(), value),
                                Err(error) => ExtensionFrame::error(
                                    frame.id.clone(),
                                    error.code().as_str(),
                                    error.to_string(),
                                ),
                            },
                            None => ExtensionFrame::error(
                                frame.id.clone(),
                                ErrorCode::ExtensionProtocolError.as_str(),
                                "message frame without a method",
                            ),
                        };
                        if let Ok(bytes) = reply.encode_line() {
                            let mut writer = writer.lock().await;
                            let _ = writer.write_all(&bytes).await;
                            let _ = writer.flush().await;
                        }
                    }
                    FrameKind::Response | FrameKind::Error | FrameKind::Cancel => {
                        let sender = pending
                            .lock()
                            .ok()
                            .and_then(|mut map| map.remove(&frame.id));
                        // Unknown or duplicate id: never settle a call with a
                        // forged identifier.
                        let Some(sender) = sender else {
                            continue;
                        };
                        let outcome = match frame.kind {
                            FrameKind::Response => CallOutcome::Answered {
                                payload: frame.payload,
                            },
                            FrameKind::Cancel => CallOutcome::Canceled {
                                reason: "the plugin acknowledged the cancel".to_owned(),
                            },
                            _ => {
                                let code = frame
                                    .payload
                                    .get("code")
                                    .and_then(Value::as_str)
                                    .unwrap_or("extension_error");
                                let message = frame
                                    .payload
                                    .get("message")
                                    .and_then(Value::as_str)
                                    .unwrap_or("plugin reported an error")
                                    .to_owned();
                                // A plugin that honours a cancel says so with a
                                // cancelled error code; that is a settled
                                // "did not happen", not a failure.
                                if code == "canceled" || code == "cancelled" {
                                    CallOutcome::Canceled { reason: message }
                                } else {
                                    CallOutcome::PluginError {
                                        code: code.to_owned(),
                                        message,
                                    }
                                }
                            }
                        };
                        let _ = sender.send(outcome);
                    }
                }
            }
            // EOF: the plugin's protocol channel closed, which means the process
            // is gone whether it exited cleanly or crashed. Every pending call
            // becomes uncertain, and the handle is marked dead so nothing tries
            // to write into a closed pipe afterwards.
            dead.store(true, Ordering::Release);
            if let Ok(mut map) = pending.lock() {
                for (_, sender) in map.drain() {
                    let _ = sender.send(CallOutcome::Uncertain {
                        reason: "the plugin stream ended before the call was answered".to_owned(),
                    });
                }
            }
        });
    }

    fn spawn_stderr_reader(stderr: tokio::process::ChildStderr, tail: Arc<Mutex<String>>) {
        tokio::spawn(async move {
            // The stderr tail is bounded, so a line longer than the whole tail is
            // drained at the cap instead of buffered in full first.
            let mut lines = CappedLines::new(stderr, MAX_STDERR_BYTES);
            while let Ok(Some((segment, _overflow))) = lines.next_line().await {
                if let Ok(mut captured) = tail.lock()
                    && captured.len() < MAX_STDERR_BYTES
                {
                    captured.push_str(&String::from_utf8_lossy(&segment));
                    captured.push('\n');
                    captured.truncate(MAX_STDERR_BYTES);
                }
            }
        });
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
    pub const fn session(&self) -> &NegotiatedSession {
        &self.session
    }

    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    #[must_use]
    pub fn inflight(&self) -> u64 {
        self.inflight.load(Ordering::SeqCst)
    }

    /// Bounded stderr capture, for diagnostics only. Never parsed as protocol.
    #[must_use]
    pub fn stderr_tail(&self) -> String {
        self.stderr_tail
            .lock()
            .map(|tail| tail.clone())
            .unwrap_or_default()
    }

    /// Send one request and wait for its answer with the default deadline.
    pub async fn call(&self, method: &str, payload: Value) -> Result<CallOutcome, ExtensionError> {
        self.call_with_deadline(method, payload, DEFAULT_CALL_TIMEOUT_MS)
            .await
    }

    /// Send one request with an explicit deadline. On expiry the process tree is
    /// terminated and the outcome is reported as uncertain.
    pub async fn call_with_deadline(
        &self,
        method: &str,
        payload: Value,
        deadline_ms: u64,
    ) -> Result<CallOutcome, ExtensionError> {
        let id = format!("call-{}", self.next_id.fetch_add(1, Ordering::SeqCst));
        self.call_with_id(&id, method, payload, deadline_ms).await
    }

    /// Send one request under a caller-chosen id.
    ///
    /// The host owns invocation identity, so a caller that already has one — a
    /// tool invocation id, say — uses it here. That is also what makes a call
    /// cancellable: the caller knows the id before the call settles and can hand
    /// it to [`Self::cancel_call`] from another task.
    pub async fn call_with_id(
        &self,
        id: &str,
        method: &str,
        payload: Value,
        deadline_ms: u64,
    ) -> Result<CallOutcome, ExtensionError> {
        if id.trim().is_empty() {
            return Err(ExtensionError::new(
                ErrorCode::InvalidPayload,
                "an extension call requires a non-empty id",
            ));
        }
        let previous = self.inflight.fetch_add(1, Ordering::SeqCst);
        if previous >= MAX_INFLIGHT_CALLS as u64 {
            self.inflight.fetch_sub(1, Ordering::SeqCst);
            return Err(ExtensionError::new(
                ErrorCode::InflightLimitExceeded,
                format!("the extension already has {MAX_INFLIGHT_CALLS} calls in flight"),
            ));
        }
        let _inflight = InflightGuard(Arc::clone(&self.inflight));
        self.call_inner(method, payload, deadline_ms, id.to_owned())
            .await
    }

    async fn call_inner(
        &self,
        method: &str,
        payload: Value,
        deadline_ms: u64,
        id: String,
    ) -> Result<CallOutcome, ExtensionError> {
        self.require_alive()?;
        let bytes = ExtensionFrame::message(id.clone(), method, payload).encode_line()?;
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self.pending.lock().map_err(|_| {
                ExtensionError::new(ErrorCode::RuntimeBlocked, "transport state is poisoned")
            })?;
            // A caller can cancel another task while this call waits to register.
            // Recheck under the same lock the cancellation guard drains.
            self.require_alive()?;
            if pending.contains_key(&id) {
                return Err(ExtensionError::new(
                    ErrorCode::DuplicateFrameId,
                    "a call id was reused before it settled",
                ));
            }
            pending.insert(id.clone(), sender);
        }
        let _pending_call = PendingCallGuard {
            id: id.clone(),
            pending: Arc::clone(&self.pending),
            child: Arc::clone(&self.child),
            dead: Arc::clone(&self.dead),
        };
        {
            let mut writer = self.writer.lock().await;
            if writer.write_all(&bytes).await.is_err() || writer.flush().await.is_err() {
                self.forget(&id);
                return Ok(CallOutcome::Uncertain {
                    reason: "the plugin stdin closed before the call was sent".to_owned(),
                });
            }
        }
        match timeout(Duration::from_millis(deadline_ms), receiver).await {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(_)) => Ok(CallOutcome::Uncertain {
                reason: "the plugin dropped the call without answering".to_owned(),
            }),
            Err(_) => {
                self.forget(&id);
                // A completed future never implies the process stopped, so the
                // whole tree is terminated and the outcome is uncertain.
                self.terminate_process_tree().await;
                Ok(CallOutcome::Uncertain {
                    reason: format!("call {method} exceeded its {deadline_ms}ms deadline"),
                })
            }
        }
    }

    fn forget(&self, id: &str) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(id);
        }
    }

    /// Whether the plugin process tree is still there.
    ///
    /// A handle whose process is gone stays invalid for its whole lifetime: a
    /// later call fails typed instead of appearing to succeed. That is what
    /// makes a crashed plugin visible to every holder of the handle at once.
    #[must_use]
    pub fn is_alive(&self) -> bool {
        !self.dead.load(Ordering::Acquire)
    }

    fn require_alive(&self) -> Result<(), ExtensionError> {
        if self.dead.load(Ordering::Acquire) {
            return Err(ExtensionError::new(
                ErrorCode::ServiceUnavailable,
                format!(
                    "extension {} generation {} is no longer running; this handle is invalid",
                    self.session.plugin_id, self.generation
                ),
            ));
        }
        Ok(())
    }

    /// Ask the plugin to stop one in-flight call, bounded by a grace window.
    ///
    /// A plugin that answers in time settles the call; one that does not has its
    /// process tree terminated, which settles every pending call as uncertain.
    /// Either way this returns inside `grace_ms`, so an ignored cancel cannot
    /// become a hung host.
    pub async fn cancel_call(
        &self,
        call_id: &str,
        grace_ms: u64,
    ) -> Result<CancelOutcome, ExtensionError> {
        self.require_alive()?;
        let bytes = ExtensionFrame::cancel(call_id).encode_line()?;
        {
            let mut writer = self.writer.lock().await;
            if writer.write_all(&bytes).await.is_err() || writer.flush().await.is_err() {
                // The pipe is already gone: the process is dead either way.
                self.terminate_process_tree().await;
                return Ok(CancelOutcome::Ignored);
            }
        }
        let deadline = tokio::time::Instant::now() + Duration::from_millis(grace_ms);
        loop {
            let still_pending = self
                .pending
                .lock()
                .is_ok_and(|pending| pending.contains_key(call_id));
            if !still_pending {
                return Ok(CancelOutcome::Acknowledged);
            }
            if tokio::time::Instant::now() >= deadline {
                self.terminate_process_tree().await;
                return Ok(CancelOutcome::Ignored);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Cancel with the default grace window.
    pub async fn cancel(&self, call_id: &str) -> Result<CancelOutcome, ExtensionError> {
        self.cancel_call(call_id, CANCEL_GRACE_MS).await
    }

    /// Terminate the plugin and every descendant, then settle pending calls as
    /// uncertain.
    pub async fn terminate_process_tree(&self) {
        self.dead.store(true, Ordering::Release);
        let child = {
            match self.child.lock() {
                Ok(mut slot) => slot.take(),
                Err(poisoned) => poisoned.into_inner().take(),
            }
        };
        if let Some(mut child) = child {
            let _ = child.start_kill();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                if !matches!(child.try_wait(), Ok(None)) {
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        if let Ok(mut pending) = self.pending.lock() {
            for (_, sender) in pending.drain() {
                let _ = sender.send(CallOutcome::Uncertain {
                    reason: "the extension was terminated".to_owned(),
                });
            }
        }
    }

    /// Ordered teardown used by unload and by host shutdown.
    pub async fn shutdown(&self) {
        self.terminate_process_tree().await;
    }
}

impl ManagedResource for ExtensionTransport {
    fn name(&self) -> &'static str {
        "p6-extension-transport"
    }

    fn shutdown<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<(), KernelError>> + Send + 'a>> {
        Box::pin(async move {
            self.terminate_process_tree().await;
            Ok(())
        })
    }

    fn join<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<(), KernelError>> + Send + 'a>> {
        Box::pin(async move { Ok(()) })
    }
}

/// Compute the digest a trust grant must pin for an executable.
pub fn executable_digest(path: &Path) -> Result<ContentHash, ExtensionError> {
    let bytes = std::fs::read(path).map_err(|error| {
        ExtensionError::new(
            ErrorCode::ExtensionNotFound,
            format!("cannot read {}: {error}", path.display()),
        )
    })?;
    Ok(ContentHash::from_bytes(&bytes))
}

/// A line reader with a hard cap.
///
/// `BufReader::split` grows a line until its delimiter arrives, so a child that
/// never writes `\n` can exhaust host memory before any size check runs. This
/// reader keeps at most `max` bytes of the line being built (plus one read
/// chunk) and reports `overflow` for the rest, so an unbounded line is drained
/// and refused instead of held.
struct CappedLines<R> {
    reader: R,
    buffer: Vec<u8>,
    max: usize,
    overflow: bool,
}

impl<R: AsyncRead + Unpin> CappedLines<R> {
    fn new(reader: R, max: usize) -> Self {
        Self {
            reader,
            buffer: Vec::new(),
            max,
            overflow: false,
        }
    }

    /// The next line without its trailing `\n`, and whether its bytes past the
    /// cap were discarded. `Ok(None)` at end of input.
    async fn next_line(&mut self) -> std::io::Result<Option<(Vec<u8>, bool)>> {
        loop {
            if let Some(index) = self.buffer.iter().position(|byte| *byte == b'\n') {
                let mut line = self.buffer.drain(..=index).collect::<Vec<_>>();
                line.pop();
                let overflow = self.overflow;
                self.overflow = false;
                return Ok(Some((line, overflow)));
            }
            if self.buffer.len() > self.max {
                self.buffer.truncate(self.max);
                self.overflow = true;
            }
            let mut scratch = [0_u8; 8 * 1024];
            let read = self.reader.read(&mut scratch).await?;
            if read == 0 {
                if self.buffer.is_empty() && !self.overflow {
                    return Ok(None);
                }
                let line = std::mem::take(&mut self.buffer);
                let overflow = self.overflow;
                self.overflow = false;
                return Ok(Some((line, overflow)));
            }
            self.buffer.extend_from_slice(&scratch[..read]);
        }
    }
}
