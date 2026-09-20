//! Session port for the interactive app.
//!
//! The port is the only thing the controller knows about execution. Three
//! producers implement it: the real application service wired to the runtime and
//! the P3 tool gate, and a labelled fixture used by tests and explicit demos.
//! A production launch never falls back to the fixture: when provider settings
//! are missing the service reports exactly what to set.

use std::collections::HashMap;
use std::future::Future;
use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use harness_providers::{
    CancellationToken, CredentialResolver, DeepSeekAdapter, ModelCapabilities, ModelProvider,
    ProviderError,
};
use harness_runtime::{RunRequest, RuntimeConfig, RuntimeService};
use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
use harness_tools::{
    ApprovalAnswer, ApprovalGate, ApprovalMode, ApprovalProposal, ToolExecutionService, TurnDriver,
    TurnLimits, TurnObserver, TurnOptions, TurnProgress, TurnStop, coding_tool_schemas,
    observe_workspace,
};
use harness_types::{ErrorCode, HostId, InputId, ProjectId, SessionId, TaskId};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

use super::bootstrap::{CREDENTIAL_VARIABLES, DEEPSEEK_ENDPOINT, DEEPSEEK_MODEL, LaunchContext};
use super::controller::{DEFAULT_APPROVAL_TIMEOUT, TurnBounds};
use super::credentials::{self, CredentialSource};
use super::events::{RunOutcome, SessionCandidate, SessionEvent};
use super::paths::LaunchEnvironment;

#[cfg(test)]
#[path = "service_completion_tests.rs"]
mod completion_tests;

/// Environment variable holding the provider endpoint.
pub const ENDPOINT_VARIABLE: &str = "HA_PROVIDER_ENDPOINT";
/// Environment variable holding the model name.
pub const MODEL_VARIABLE: &str = "HA_PROVIDER_MODEL";

/// One admitted user request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubmitRequest {
    pub input_id: InputId,
    pub text: String,
}

/// The user's decision on one gated action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApprovalDecision {
    /// Run this exact action once.
    Granted,
    /// Do not run it.
    Denied,
}

/// What the controller needs from an execution backend.
pub trait SessionPort: Send {
    /// Label shown in the header; a fixture must never look like the real thing.
    fn label(&self) -> String;
    fn submit(&mut self, request: SubmitRequest);
    fn cancel(&mut self);
    /// Answer one pending approval request; false when the id is not pending.
    fn answer(&mut self, _request_id: &str, _decision: ApprovalDecision) -> bool {
        false
    }
    /// Ask for the resumable sessions of this project; the list arrives as an event.
    fn list_sessions(&mut self) {}
    /// Continue from a persisted session, or start a fresh conversation when None.
    fn resume(&mut self, session_id: Option<String>) -> Result<(), String> {
        if session_id.is_some() {
            Err("this backend does not support persisted sessions".to_owned())
        } else {
            Ok(())
        }
    }
    /// The step and tool-call bounds this backend enforces, so the status bar can
    /// show `step k/max` without the UI knowing the driver's type.
    fn limits(&self) -> TurnBounds {
        TurnBounds::default()
    }
    /// Lines `/status` prints about the provider: which credential variable holds
    /// the key (never its value), whether the endpoint and the model came from the
    /// environment or from the defaults, and whether the endpoint answers.
    fn provider_diagnostics(&self) -> Vec<String> {
        Vec::new()
    }
    /// Why a real dispatch is impossible right now, or `None` when it is possible.
    ///
    /// The controller asks this before submitting a turn, so the answer is always
    /// evaluated against the *current* credential: after `/key` saves one, this
    /// turns `None` and the very next message reaches the provider.
    fn provider_problem(&self) -> Option<String> {
        None
    }
    /// Save a key so this session and the next launch can use it.
    ///
    /// The file is the source of truth because the credential resolver re-reads it
    /// at call time; nothing has to restart for the next turn to use it. The
    /// returned event carries the source *name*, never the value.
    fn save_credential(&mut self, _key: &str) -> Option<SessionEvent> {
        None
    }
}

/// Asks the user for each gated action and waits for the answer.
///
/// The proposal travels as a session event; the answer comes back through
/// `ChannelApprovalGate::answer`. No answer inside the timeout is an expiry,
/// never a silent grant, and an expiry is **announced** as
/// `SessionEvent::ApprovalExpired` before the driver is told, so the panel closes
/// instead of waiting for the run to end.
pub struct ChannelApprovalGate {
    sender: UnboundedSender<SessionEvent>,
    pending: Mutex<HashMap<String, oneshot::Sender<ApprovalAnswer>>>,
    timeout: Duration,
}

impl ChannelApprovalGate {
    #[must_use]
    pub fn new(sender: UnboundedSender<SessionEvent>, timeout: Duration) -> Self {
        Self {
            sender,
            pending: Mutex::new(HashMap::new()),
            timeout,
        }
    }

    /// Answer one pending request.
    pub fn answer(&self, request_id: &str, decision: ApprovalDecision) -> bool {
        let pending = self
            .pending
            .lock()
            .ok()
            .and_then(|mut map| map.remove(request_id));
        match pending {
            Some(sender) => sender
                .send(match decision {
                    ApprovalDecision::Granted => ApprovalAnswer::Granted,
                    ApprovalDecision::Denied => ApprovalAnswer::Denied,
                })
                .is_ok(),
            None => false,
        }
    }
}

impl ApprovalGate for ChannelApprovalGate {
    fn request(
        &self,
        proposal: ApprovalProposal,
    ) -> Pin<Box<dyn Future<Output = ApprovalAnswer> + Send>> {
        let (sender, receiver) = oneshot::channel();
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(proposal.request_id.clone(), sender);
        }
        let _ = self.sender.send(SessionEvent::ApprovalRequired {
            request_id: proposal.request_id.clone(),
            action: proposal.action,
            summary: proposal.summary,
            workspace: proposal.workspace.display().to_string(),
            scope: proposal.scope,
            // The deadline travels with the proposal: the panel counts down to
            // the gate's real timeout instead of hard-coding a second one.
            expires_at: Instant::now() + self.timeout,
        });
        let timeout = self.timeout;
        let request_id = proposal.request_id;
        let sender = self.sender.clone();
        Box::pin(async move {
            match tokio::time::timeout(timeout, receiver).await {
                Ok(Ok(answer)) => answer,
                // The sender was dropped without an answer: refuse, never grant.
                Ok(Err(_)) => ApprovalAnswer::Denied,
                Err(_) => {
                    // Tell the UI first: the panel must close even though the
                    // driver only learns about the expiry when it resumes.
                    let _ = sender.send(SessionEvent::ApprovalExpired { request_id });
                    ApprovalAnswer::Expired
                }
            }
        })
    }
}

/// Event channel shared by the port implementation and the controller.
#[derive(Debug)]
pub struct SessionChannel {
    sender: UnboundedSender<SessionEvent>,
    receiver: UnboundedReceiver<SessionEvent>,
}

impl SessionChannel {
    #[must_use]
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self { sender, receiver }
    }

    #[must_use]
    pub fn sender(&self) -> UnboundedSender<SessionEvent> {
        self.sender.clone()
    }

    /// Take everything the port produced, without blocking the render loop.
    pub fn drain(&mut self) -> Vec<SessionEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.receiver.try_recv() {
            events.push(event);
        }
        events
    }
}

impl Default for SessionChannel {
    fn default() -> Self {
        Self::new()
    }
}

/// Provider settings for a real model call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderConfig {
    pub endpoint: String,
    pub model: String,
    /// Name of the source that holds the key; never the key.
    pub credential: CredentialSource,
}

impl ProviderConfig {
    /// The environment variable the credential resolver should probe first.
    ///
    /// When the active source is the saved file there is no variable to name, so
    /// the resolver is bound to the primary variable anyway: it stays empty in the
    /// environment and the resolver falls through to the file, which is exactly
    /// the ordered pair of sources the resolver implements.
    #[must_use]
    pub fn credential_variable(&self) -> String {
        match &self.credential {
            CredentialSource::Environment { variable } => variable.clone(),
            CredentialSource::File { .. } => CREDENTIAL_VARIABLES[0].to_owned(),
        }
    }
}

/// Resolve provider settings without touching the filesystem.
///
/// The credential is mandatory and is **never** substituted: without it the error
/// names what to set, and no fixture answer is produced. The endpoint and the model
/// fall back to `DeepSeek`'s documented values, so one `DEEPSEEK_API_KEY` is a complete
/// setup; an explicit variable still overrides either one, which is what another
/// provider or another model needs.
pub fn resolve_provider(
    environment: &LaunchEnvironment,
    data_dir: &Path,
) -> Result<ProviderConfig, String> {
    let explicit_endpoint = environment
        .value(ENDPOINT_VARIABLE)
        .filter(|value| !value.is_empty());
    let explicit_model = environment
        .value(MODEL_VARIABLE)
        .filter(|value| !value.is_empty());

    let Some(credential) = credentials::source(environment, data_dir) else {
        return Err(format!(
            "provider setup is incomplete: set {} or save the key in the app with /key (API key). Nothing was sent and no fixture answer was substituted.",
            credentials::CREDENTIAL_VARIABLES.join(" or ")
        ));
    };
    Ok(ProviderConfig {
        endpoint: explicit_endpoint.map_or_else(
            || DEEPSEEK_ENDPOINT.to_owned(),
            |value| value.to_string_lossy().into_owned(),
        ),
        model: explicit_model.map_or_else(
            || DEEPSEEK_MODEL.to_owned(),
            |value| value.to_string_lossy().into_owned(),
        ),
        credential,
    })
}

/// Check that a key the app is about to trust can really be read.
///
/// Two things have to agree: the source the environment resolves to, and the file
/// the credential resolver reads at call time. They are the same path by
/// construction, and this proves it for the launch at hand — if an override makes
/// them disagree, the key would be accepted here and then fail on the first call,
/// which is exactly the confusing state this check exists to prevent.
pub fn validate_credential_file(
    environment: &LaunchEnvironment,
    data_dir: &Path,
) -> Result<(), String> {
    let Some(CredentialSource::File { path, .. }) = credentials::source(environment, data_dir)
    else {
        return Ok(());
    };
    let resolver_path = credentials::resolve_file(environment, data_dir);
    if resolver_path != path {
        return Err(format!(
            "the credential source {} and the file the resolver reads {} disagree; nothing was sent",
            path.display(),
            resolver_path.display()
        ));
    }
    match credentials::load(&path) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(format!(
            "the credential file {} holds no key; save it in the app with /key",
            path.display()
        )),
        Err(error) => Err(error.to_string()),
    }
}

/// One line per provider fact, for `/status`.
///
/// It answers the three questions an operator actually has: is a credential set and
/// where from, which endpoint and model will be used and why, and is that endpoint
/// answering. The credential **value** never appears.
#[must_use]
pub fn provider_diagnostics(environment: &LaunchEnvironment, data_dir: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    match credentials::source(environment, data_dir) {
        Some(source) => {
            lines.push(format!(
                "Provider: credential from {} (value hidden)",
                source.describe()
            ));
            if let Some(protection) = source.protection() {
                lines.push(format!(
                    "Provider: credential file protection: {}",
                    protection.describe()
                ));
            }
        }
        None => lines.push(format!(
            "Provider: no credential; set one of {} or save it with /key",
            CREDENTIAL_VARIABLES.join(", ")
        )),
    }
    match environment
        .value(ENDPOINT_VARIABLE)
        .filter(|value| !value.is_empty())
    {
        Some(value) => lines.push(format!(
            "Provider: endpoint {ENDPOINT_VARIABLE}={}",
            value.to_string_lossy()
        )),
        None => lines.push(format!(
            "Provider: endpoint not set, using the default {DEEPSEEK_ENDPOINT}"
        )),
    }
    match environment
        .value(MODEL_VARIABLE)
        .filter(|value| !value.is_empty())
    {
        Some(value) => lines.push(format!(
            "Provider: model {MODEL_VARIABLE}={}",
            value.to_string_lossy()
        )),
        None => lines.push(format!(
            "Provider: model not set, using the default {DEEPSEEK_MODEL}"
        )),
    }
    match resolve_provider(environment, data_dir) {
        Ok(config) => {
            lines.push(format!("Provider: ready, would call {}", config.model));
            lines.push(match endpoint_reachability(&config.endpoint) {
                Ok(()) => "Provider: endpoint answered a TCP connection".to_owned(),
                Err(reason) => format!("Provider: endpoint did not answer ({reason})"),
            });
        }
        Err(message) => lines.push(format!("Provider: not ready ({message})")),
    }
    lines
}

/// Whether a TCP connection to the endpoint's host and port succeeds.
///
/// A reachability check, not a call: it sends no request and needs no credential, so
/// it can never spend money or leak a key.
fn endpoint_reachability(endpoint: &str) -> Result<(), String> {
    let without_scheme = endpoint
        .split_once("://")
        .map_or(endpoint, |(_scheme, rest)| rest);
    let authority = without_scheme
        .split(['/', '?'])
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "the endpoint has no host".to_owned())?;
    let (host, port) = authority.rsplit_once(':').map_or_else(
        || {
            (
                authority,
                if endpoint.starts_with("https://") {
                    443
                } else {
                    80
                },
            )
        },
        |(host, port)| (host, port.parse::<u16>().unwrap_or(443)),
    );
    let address = (host, port)
        .to_socket_addrs()
        .map_err(|error| format!("{host}:{port} does not resolve: {error}"))?
        .next()
        .ok_or_else(|| format!("{host}:{port} does not resolve"))?;
    std::net::TcpStream::connect_timeout(&address, Duration::from_secs(5))
        .map(|_| ())
        .map_err(|error| format!("{host}:{port} refused: {error}"))
}

/// Reads the credential when a call is made; the value is never stored, logged
/// or rendered anywhere in the app.
///
/// Two ordered sources, and the order is the whole contract: the environment
/// variable wins, the file saved by `/key` is the fallback. The file is re-read
/// on every call, which is why saving a key takes effect in a running app
/// without a restart.
pub struct EnvironmentCredential {
    variable: String,
    data_dir: PathBuf,
    environment: LaunchEnvironment,
}

impl EnvironmentCredential {
    /// Bind the resolver to one environment variable name and one data root.
    #[must_use]
    pub fn new(variable: impl Into<String>, data_dir: PathBuf) -> Self {
        Self {
            variable: variable.into(),
            data_dir,
            environment: LaunchEnvironment::capture(),
        }
    }

    /// The file this launch falls back to when the variable is absent.
    #[must_use]
    pub fn file(&self) -> PathBuf {
        credentials::resolve_file(&self.environment, &self.data_dir)
    }
}

impl CredentialResolver for EnvironmentCredential {
    fn resolve(&self) -> Result<String, ProviderError> {
        if let Some(value) = self
            .environment
            .value(&self.variable)
            .filter(|value| !value.is_empty())
        {
            return Ok(value.to_string_lossy().into_owned());
        }
        let path = self.file();
        match credentials::load(&path) {
            Ok(Some(value)) => Ok(value),
            Err(error) => Err(ProviderError::new(
                ErrorCode::SecretNotGranted,
                format!(
                    "credential {} is not present in the environment and the saved file cannot be used: {error}",
                    self.variable
                ),
            )),
            Ok(None) => Err(ProviderError::new(
                ErrorCode::SecretNotGranted,
                format!(
                    "credential {} is not present in the environment and {} holds no key; save it in the app with /key",
                    self.variable,
                    path.display()
                ),
            )),
        }
    }
}

/// Maps turn progress onto the UI vocabulary.
struct ChannelObserver {
    sender: UnboundedSender<SessionEvent>,
    /// Calls are executed serially by the turn driver. Keeping the current
    /// boundary here makes duration delivery O(1) and avoids a process-lifetime
    /// map keyed by a non-unique tool name.
    tool_started: Mutex<Option<(String, Instant)>>,
}

impl TurnObserver for ChannelObserver {
    fn observe(&self, progress: TurnProgress) {
        let event = match progress {
            TurnProgress::TextDelta(text) => SessionEvent::TextDelta { text },
            // Step boundaries are what the status bar counts (`step 2/8`).
            TurnProgress::StepStarted { step } => SessionEvent::StepStarted { step },
            TurnProgress::ToolStarted { name, summary } => {
                if let Ok(mut started) = self.tool_started.lock() {
                    *started = Some((name.clone(), Instant::now()));
                }
                SessionEvent::ToolStarted { name, summary }
            }
            TurnProgress::ToolSettled { name, ok } => {
                let elapsed = self
                    .tool_started
                    .lock()
                    .ok()
                    .and_then(|mut started| started.take())
                    .filter(|(started_name, _)| started_name == &name)
                    .map_or(Duration::ZERO, |(_, started)| started.elapsed());
                SessionEvent::ToolSettled { name, ok, elapsed }
            }
        };
        let _ = self.sender.send(event);
    }
}

/// Production session port behind the interactive app.
///
/// The accepted journal admits exactly one user input per session, so a
/// conversation keeps one task identity and opens a new session per turn, linked
/// to its predecessor. The store writer is opened per running turn and released
/// when the turn ends.
pub struct AgentSessionService {
    sender: UnboundedSender<SessionEvent>,
    store_dir: PathBuf,
    /// Root that owns the credential file `/key` writes.
    data_dir: PathBuf,
    workspace_root: PathBuf,
    environment: LaunchEnvironment,
    task_id: TaskId,
    previous_session: Arc<Mutex<Option<SessionId>>>,
    /// Asks the user for every gated action; never grants on its own.
    gate: Arc<ChannelApprovalGate>,
    cancellation: Option<CancellationToken>,
    /// The limits this service hands to the driver, reported to the UI.
    limits: TurnLimits,
}

/// How long a gated action waits for the user before it expires.
const APPROVAL_TIMEOUT: Duration = DEFAULT_APPROVAL_TIMEOUT;

/// Upper bound on the resume list, newest first.
const RESUME_LIST_LIMIT: usize = 20;

impl AgentSessionService {
    #[must_use]
    pub fn new(
        context: &LaunchContext,
        environment: LaunchEnvironment,
        sender: UnboundedSender<SessionEvent>,
    ) -> Self {
        let gate = Arc::new(ChannelApprovalGate::new(sender.clone(), APPROVAL_TIMEOUT));
        Self {
            sender,
            store_dir: context.project_store_dir(),
            data_dir: context.paths.data_dir.clone(),
            workspace_root: context.project.root.clone(),
            environment,
            task_id: TaskId::generate(),
            previous_session: Arc::new(Mutex::new(None)),
            gate,
            cancellation: None,
            limits: TurnLimits::default(),
        }
    }

    fn configured(&self) -> Result<ProviderConfig, String> {
        resolve_provider(&self.environment, &self.data_dir)
    }
}

impl SessionPort for AgentSessionService {
    fn label(&self) -> String {
        match self.configured() {
            Ok(config) => format!("{} via {}", config.model, config.endpoint),
            Err(_) => "setup required (no provider configured)".to_owned(),
        }
    }

    fn submit(&mut self, request: SubmitRequest) {
        let cancellation = CancellationToken::new();
        self.cancellation = Some(cancellation.clone());
        // A turn must run on an async runtime. In the app it always does; this
        // guard keeps a misuse from panicking inside the UI thread.
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            let _ = self.sender.send(SessionEvent::RecoverableError {
                message: "the application service needs an async runtime (internal error); nothing was sent".to_owned(),
            });
            return;
        };
        let sender = self.sender.clone();
        let store_dir = self.store_dir.clone();
        let data_dir = self.data_dir.clone();
        let workspace_root = self.workspace_root.clone();
        let environment = self.environment.clone();
        let task_id = self.task_id.clone();
        let previous_session = Arc::clone(&self.previous_session);
        let gate = Arc::clone(&self.gate);
        let limits = self.limits;
        // Every user input opens its own session; the conversation is the chain of
        // sessions linked to the same task.
        let session_id = SessionId::generate();
        handle.spawn(async move {
            run_turn(
                sender,
                store_dir,
                data_dir,
                workspace_root,
                environment,
                session_id,
                task_id,
                previous_session,
                gate,
                limits,
                request,
                cancellation,
            )
            .await;
        });
    }

    fn answer(&mut self, request_id: &str, decision: ApprovalDecision) -> bool {
        self.gate.answer(request_id, decision)
    }

    fn limits(&self) -> TurnBounds {
        TurnBounds {
            max_steps: self.limits.max_steps,
            max_tool_calls: self.limits.max_tool_calls,
        }
    }

    fn provider_diagnostics(&self) -> Vec<String> {
        provider_diagnostics(&self.environment, &self.data_dir)
    }

    fn provider_problem(&self) -> Option<String> {
        self.configured().err()
    }

    fn save_credential(&mut self, key: &str) -> Option<SessionEvent> {
        let path = credentials::resolve_file(&self.environment, &self.data_dir);
        match credentials::save(&path, key) {
            Ok(protection) => Some(SessionEvent::ProviderConfigured {
                source: CredentialSource::File { path, protection },
            }),
            Err(error) => Some(SessionEvent::RecoverableError {
                message: format!("the key could not be saved: {error}"),
            }),
        }
    }

    fn list_sessions(&mut self) {
        let sender = self.sender.clone();
        let store_dir = self.store_dir.clone();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            match SqliteStore::open_read_only(store_dir.clone()).await {
                Ok(store) => match store.list_sessions().await {
                    Ok(summaries) => {
                        // Newest first, bounded: a resume list is a menu, not a dump.
                        let mut sessions: Vec<SessionCandidate> = summaries
                            .into_iter()
                            .map(|summary| SessionCandidate {
                                session_id: summary.session_id.as_str().to_owned(),
                                task_id: summary.task_id.as_str().to_owned(),
                                detail: format!(
                                    "{} input(s), {} event(s)",
                                    summary.input_count, summary.next_sequence
                                ),
                            })
                            .collect();
                        sessions.reverse();
                        sessions.truncate(RESUME_LIST_LIMIT);
                        let _ = sender.send(SessionEvent::SessionsListed { sessions });
                    }
                    Err(error) => {
                        let _ = sender.send(SessionEvent::RecoverableError {
                            message: format!("session list could not be read: {error}"),
                        });
                    }
                },
                Err(error) => {
                    let _ = sender.send(SessionEvent::Notice {
                        message: format!(
                            "no persisted sessions in this project yet ({error}); this conversation starts fresh"
                        ),
                    });
                }
            }
        });
    }

    fn resume(&mut self, session_id: Option<String>) -> Result<(), String> {
        let source = session_id
            .map(SessionId::parse)
            .transpose()
            .map_err(|error| format!("resume needs a canonical session id: {error}"))?;
        let mut previous = self
            .previous_session
            .lock()
            .map_err(|_| "conversation state is unavailable; nothing was resumed".to_owned())?;
        // Selection is synchronous: submit cannot overtake a background lookup.
        // The turn validates ownership in this project's store before dispatch.
        if source.is_none() {
            self.task_id = TaskId::generate();
        }
        *previous = source;
        Ok(())
    }

    fn cancel(&mut self) {
        if let Some(token) = self.cancellation.take() {
            token.cancel();
        }
    }
}

/// One turn, from admission to terminal event.
///
/// Linear setup followed by one bounded turn: the length is the wiring, not
/// hidden branching logic.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn run_turn(
    sender: UnboundedSender<SessionEvent>,
    store_dir: PathBuf,
    data_dir: PathBuf,
    workspace_root: PathBuf,
    environment: LaunchEnvironment,
    session_id: SessionId,
    task_id: TaskId,
    previous_session: Arc<Mutex<Option<SessionId>>>,
    gate: Arc<ChannelApprovalGate>,
    limits: TurnLimits,
    request: SubmitRequest,
    cancellation: CancellationToken,
) {
    let send = |event| {
        let _ = sender.send(event);
    };
    send(SessionEvent::Accepted {
        input_id: request.input_id.clone(),
    });

    let config = match resolve_provider(&environment, &data_dir) {
        Ok(config) => config,
        Err(message) => {
            send(SessionEvent::RecoverableError { message });
            return;
        }
    };

    let store = match SqliteStore::open_writer(WriterOpenOptions::new(
        store_dir.clone(),
        HostId::generate(),
    ))
    .await
    {
        Ok(store) => Arc::new(store),
        Err(error) => {
            send(SessionEvent::RecoverableError {
                message: format!(
                    "cannot open the project store at {}: {error}. Another terminal may be writing this project.",
                    store_dir.display()
                ),
            });
            return;
        }
    };

    let source = if let Ok(previous) = previous_session.lock() {
        previous.clone()
    } else {
        send(SessionEvent::RecoverableError {
            message: "conversation state is unavailable".to_owned(),
        });
        return;
    };
    let task_id = match &source {
        Some(source) => match store.session_task(source).await {
            Ok(Some(task)) => task,
            result => {
                let message = match result {
                    Ok(None) => format!(
                        "session {source} is not in this project's store; nothing was resumed"
                    ),
                    Err(error) => format!("session could not be recovered: {error}"),
                    Ok(Some(_)) => unreachable!(),
                };
                if let Ok(store) = Arc::try_unwrap(store) {
                    let _ = store.close().await;
                }
                send(SessionEvent::RecoverableError { message });
                return;
            }
        },
        None => task_id,
    };

    let capabilities = ModelCapabilities {
        provider_id: "deepseek".to_owned(),
        model: config.model.clone(),
        supports_streaming: true,
        supports_tools: true,
        // A real adapter: this is never presented as a fixture.
        fixture: false,
    };
    let credentials = Arc::new(EnvironmentCredential::new(
        config.credential_variable(),
        data_dir.clone(),
    ));
    let provider: Arc<dyn ModelProvider> =
        match DeepSeekAdapter::new(config.endpoint.clone(), credentials, capabilities) {
            Ok(adapter) => Arc::new(adapter),
            Err(error) => {
                send(SessionEvent::RecoverableError {
                    message: format!("provider configuration is invalid: {error}"),
                });
                return;
            }
        };

    let observation = match observe_workspace(ProjectId::generate(), &workspace_root) {
        Ok(observation) => observation,
        Err(error) => {
            send(SessionEvent::RecoverableError {
                message: format!(
                    "workspace {} cannot be observed: {error}",
                    workspace_root.display()
                ),
            });
            return;
        }
    };

    let runtime = Arc::new(RuntimeService::new(
        Arc::clone(&store),
        provider,
        RuntimeConfig::default(),
    ));
    let driver = TurnDriver::new(
        Arc::clone(&runtime),
        ToolExecutionService::new(Arc::clone(&store)),
    );
    let run_request = RunRequest::new(
        session_id,
        task_id,
        request.input_id.clone(),
        request.text,
        observation,
    )
    .with_tool_schemas(coding_tool_schemas());
    let options = TurnOptions {
        workspace_root,
        actor_id: "interactive.user".to_owned(),
        // Every gated action is rendered to the user and answered by them; nothing
        // is granted without an explicit answer.
        approvals: ApprovalMode::Ask(gate as Arc<dyn ApprovalGate>),
        limits,
    };
    let observer: Arc<dyn TurnObserver> = Arc::new(ChannelObserver {
        sender: sender.clone(),
        tool_started: Mutex::new(None),
    });

    // A follow-up turn continues the previous session; the first turn starts one.
    let outcome = match &source {
        Some(source) => {
            driver
                .run_turn_continuing(source, run_request, options, observer, cancellation)
                .await
        }
        None => {
            driver
                .run_turn(run_request, options, observer, cancellation)
                .await
        }
    };

    // Release the writer before announcing the terminal event: the next turn takes
    // a newer generation of the task lease, and it must not race this one.
    drop(driver);
    drop(runtime);
    if let Ok(store) = Arc::try_unwrap(store) {
        let _ = store.close().await;
    }

    let terminal = match outcome {
        Ok(outcome) => {
            if let Ok(mut guard) = previous_session.lock() {
                *guard = Some(outcome.session_id.clone());
            }
            match outcome.stop {
                TurnStop::Final => RunOutcome::Done,
                TurnStop::Canceled => RunOutcome::Canceled,
                TurnStop::StepLimit => RunOutcome::Failed("step limit reached".to_owned()),
                TurnStop::ToolLimit => RunOutcome::Failed("tool-call limit reached".to_owned()),
                TurnStop::Deadline => RunOutcome::Failed("deadline reached".to_owned()),
            }
        }
        Err(error) => RunOutcome::Failed(error.to_string()),
    };
    send(SessionEvent::RunTerminal { outcome: terminal });
}

/// Deterministic labelled fixture used by tests and explicit demos.
#[derive(Debug)]
pub struct FixtureService {
    sender: UnboundedSender<SessionEvent>,
    pending_approval: Arc<Mutex<Option<String>>>,
}

impl FixtureService {
    #[must_use]
    pub fn new(sender: UnboundedSender<SessionEvent>) -> Self {
        Self {
            sender,
            pending_approval: Arc::new(Mutex::new(None)),
        }
    }

    /// One deterministic tool call: started, measured, then settled.
    fn run_fixture_tool(&self, name: &str, summary: &str, ok: bool) {
        let started = Instant::now();
        let _ = self.sender.send(SessionEvent::ToolStarted {
            name: name.to_owned(),
            summary: summary.to_owned(),
        });
        let elapsed = started.elapsed();
        let _ = self.sender.send(SessionEvent::ToolSettled {
            name: name.to_owned(),
            ok,
            elapsed,
        });
    }
}

impl SessionPort for FixtureService {
    fn label(&self) -> String {
        "fixture (no model was called)".to_owned()
    }

    fn submit(&mut self, request: SubmitRequest) {
        let send = |event| {
            let _ = self.sender.send(event);
        };
        send(SessionEvent::Accepted {
            input_id: request.input_id,
        });
        send(SessionEvent::StepStarted { step: 1 });
        if request.text == "request approval fixture" {
            let request_id = "fixture-approval-1".to_owned();
            if let Ok(mut pending) = self.pending_approval.lock() {
                *pending = Some(request_id.clone());
            }
            send(SessionEvent::ApprovalRequired {
                request_id,
                action: "fixture_action".to_owned(),
                summary: "prove the terminal approval path".to_owned(),
                workspace: "fixture workspace (no mutation)".to_owned(),
                scope: "once".to_owned(),
                expires_at: Instant::now() + DEFAULT_APPROVAL_TIMEOUT,
            });
            return;
        }
        // Echoing the admitted text makes the transcript prove exactly which
        // buffer was submitted, which is what the editor tests assert on.
        send(SessionEvent::TextDelta {
            text: format!("fixture answer for: {}", request.text),
        });
        send(SessionEvent::TextDelta {
            text: " (no model was called)".to_owned(),
        });
        self.run_fixture_tool("search_text", "pattern=parser", false);
        self.run_fixture_tool("apply_patch", "path=src/parser.rs", true);
        // A request that asks for a failure exercises the failure rendering
        // deterministically, without pretending a model produced it.
        let outcome = if request.text.contains("fail") {
            RunOutcome::Failed("fixture failure requested by the prompt".to_owned())
        } else {
            RunOutcome::Done
        };
        send(SessionEvent::RunTerminal { outcome });
    }

    fn cancel(&mut self) {
        let _ = self.sender.send(SessionEvent::RunTerminal {
            outcome: RunOutcome::Canceled,
        });
    }

    fn answer(&mut self, request_id: &str, decision: ApprovalDecision) -> bool {
        let accepted = self
            .pending_approval
            .lock()
            .ok()
            .and_then(|mut pending| {
                (pending.as_deref() == Some(request_id)).then(|| pending.take())
            })
            .flatten()
            .is_some();
        if !accepted {
            return false;
        }
        if decision == ApprovalDecision::Granted {
            self.run_fixture_tool("fixture_action", "no mutation", true);
        }
        let _ = self.sender.send(SessionEvent::RunTerminal {
            outcome: RunOutcome::Done,
        });
        true
    }

    fn provider_diagnostics(&self) -> Vec<String> {
        vec!["Provider: the labelled fixture; no model is called".to_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AgentSessionService, ApprovalDecision, ApprovalGate, ApprovalProposal, ChannelApprovalGate,
        DEEPSEEK_ENDPOINT, DEEPSEEK_MODEL, ENDPOINT_VARIABLE, EnvironmentCredential,
        FixtureService, MODEL_VARIABLE, SessionChannel, SessionPort, SubmitRequest,
        provider_diagnostics, resolve_provider, validate_credential_file,
    };
    use crate::interactive::bootstrap::{self, LaunchRequest};
    use crate::interactive::credentials::{self, CredentialSource, Protection};
    use crate::interactive::events::{RunOutcome, SessionEvent};
    use crate::interactive::paths::{HostPlatform, LaunchEnvironment};
    use harness_tools::ApprovalAnswer;
    use harness_types::InputId;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn request() -> SubmitRequest {
        SubmitRequest {
            input_id: InputId::generate(),
            text: "fix the parser".to_owned(),
        }
    }

    fn environment(pairs: &[(&str, &str)]) -> LaunchEnvironment {
        LaunchEnvironment::from_pairs(pairs.iter().map(|(name, value)| (*name, *value)))
    }

    /// A credential directory that belongs to one test only.
    ///
    /// These cases must never read or write the credential file of the account
    /// running them, so the directory is `HA_CREDENTIALS_DIR` under a temporary
    /// root. The per-test counter matters: tests in one binary run in parallel, and
    /// a directory keyed only by the process id let two cases overwrite each
    /// other's file. It is deliberately not cleaned up so a failing assertion can
    /// still be inspected afterwards.
    fn scoped_credentials() -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let directory = std::env::temp_dir().join(format!(
            "ha-service-credential-tests-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::create_dir_all(&directory);
        directory
    }

    /// Environment plus a directory reserved for this test's credential file.
    fn credential_environment(pairs: &[(&str, &str)]) -> (LaunchEnvironment, PathBuf) {
        let directory = scoped_credentials();
        let merged = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .chain([(
                credentials::CREDENTIAL_DIRECTORY_VARIABLE.to_owned(),
                directory.to_string_lossy().into_owned(),
            )])
            .collect::<Vec<_>>();
        (LaunchEnvironment::from_pairs(merged), directory)
    }

    #[test]
    fn h03_fixture_service_is_labelled_and_streams_a_full_run() {
        let mut channel = SessionChannel::new();
        let mut service = FixtureService::new(channel.sender());
        assert!(service.label().contains("fixture"));
        service.submit(request());

        let events = channel.drain();
        assert!(matches!(events[0], SessionEvent::Accepted { .. }));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, SessionEvent::TextDelta { .. }))
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, SessionEvent::ToolStarted { .. }))
                .count(),
            2
        );
        let SessionEvent::RunTerminal { outcome } = events.last().expect("terminal event") else {
            panic!("the fixture must end the run: {events:?}");
        };
        assert_eq!(outcome, &RunOutcome::Done);
        assert!(channel.drain().is_empty(), "draining empties the channel");
    }

    #[test]
    fn h03_cancel_reports_a_canceled_run() {
        let mut channel = SessionChannel::new();
        let mut service = FixtureService::new(channel.sender());
        service.cancel();
        assert_eq!(
            channel.drain(),
            vec![SessionEvent::RunTerminal {
                outcome: RunOutcome::Canceled
            }]
        );
    }

    fn proposal(request_id: &str) -> ApprovalProposal {
        ApprovalProposal {
            request_id: request_id.to_owned(),
            action: "ApplyPatch".to_owned(),
            summary: "patch src/parser.rs".to_owned(),
            workspace: std::path::PathBuf::from("C:/work/repo"),
            scope: "one action, this turn only".to_owned(),
        }
    }

    #[tokio::test]
    async fn h05_the_gate_expires_without_an_answer_and_never_grants_late() {
        let mut channel = SessionChannel::new();
        let gate = ChannelApprovalGate::new(channel.sender(), Duration::from_millis(30));

        let answer = ApprovalGate::request(&gate, proposal("approval-1")).await;
        assert_eq!(
            answer,
            ApprovalAnswer::Expired,
            "no answer inside the timeout is an expiry, not a grant"
        );

        let announced = channel.drain();
        assert!(
            announced.iter().any(|event| matches!(
                event,
                SessionEvent::ApprovalRequired { request_id, action, .. }
                    if request_id == "approval-1" && action == "ApplyPatch"
            )),
            "{announced:?}"
        );
        assert!(!gate.answer("approval-1", ApprovalDecision::Granted));
        assert!(!gate.answer("unknown", ApprovalDecision::Denied));
    }

    #[tokio::test]
    async fn h05_the_gate_forwards_the_users_answer() {
        let mut channel = SessionChannel::new();
        let gate = Arc::new(ChannelApprovalGate::new(
            channel.sender(),
            Duration::from_secs(5),
        ));
        let asking = Arc::clone(&gate);
        let handle = tokio::spawn(async move {
            ApprovalGate::request(asking.as_ref(), proposal("approval-2")).await
        });

        // Wait for the proposal to reach the UI before answering it.
        let mut request_id = None;
        for _ in 0..200 {
            if let Some(id) = channel.drain().into_iter().find_map(|event| match event {
                SessionEvent::ApprovalRequired { request_id, .. } => Some(request_id),
                _ => None,
            }) {
                request_id = Some(id);
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let request_id = request_id.expect("the proposal reaches the channel");
        assert_eq!(request_id, "approval-2");
        assert!(gate.answer(&request_id, ApprovalDecision::Granted));
        assert_eq!(handle.await.expect("asking task"), ApprovalAnswer::Granted);
        assert!(
            !gate.answer(&request_id, ApprovalDecision::Denied),
            "a request is answered once"
        );
    }

    #[test]
    fn h04_provider_setup_names_the_missing_credential_and_never_substitutes_a_fixture() {
        let (environment, data_dir) = credential_environment(&[]);
        let error = resolve_provider(&environment, &data_dir)
            .expect_err("an empty environment is not configured");
        assert!(error.contains("DEEPSEEK_API_KEY"), "{error}");
        assert!(
            error.contains("/key"),
            "the message must name the in-app way to set it: {error}"
        );
        assert!(
            error.contains("no fixture answer"),
            "the message must say nothing was substituted: {error}"
        );
    }

    /// T-track change: one `DEEPSEEK_API_KEY` is a complete setup.
    ///
    /// The endpoint and the model are the values `DeepSeek` documents, so defaulting
    /// to them is not guessing; an explicit variable still wins. The credential
    /// stays mandatory. Recorded in the operator guide section 12.
    #[test]
    fn t08_one_deepseek_key_is_a_complete_provider_setup() {
        let (environment, data_dir) =
            credential_environment(&[("DEEPSEEK_API_KEY", "fixture-secret")]);
        let bare = resolve_provider(&environment, &data_dir)
            .expect("a bare DeepSeek key configures the provider");
        assert_eq!(bare.endpoint, DEEPSEEK_ENDPOINT);
        assert_eq!(bare.model, DEEPSEEK_MODEL);
        assert_eq!(
            bare.credential,
            CredentialSource::Environment {
                variable: "DEEPSEEK_API_KEY".to_owned()
            }
        );
        assert!(
            bare.endpoint.starts_with("https://"),
            "the default is TLS: {bare:?}"
        );

        let (environment, data_dir) = credential_environment(&[
            ("DEEPSEEK_API_KEY", "fixture-secret"),
            (ENDPOINT_VARIABLE, "http://127.0.0.1:9/chat/completions"),
            (MODEL_VARIABLE, "fixture-model"),
        ]);
        let explicit =
            resolve_provider(&environment, &data_dir).expect("explicit settings still resolve");
        assert_eq!(explicit.endpoint, "http://127.0.0.1:9/chat/completions");
        assert_eq!(explicit.model, "fixture-model");

        // The alias credential works the same way.
        let (environment, data_dir) = credential_environment(&[("HA_API_KEY", "fixture-secret")]);
        let alias = resolve_provider(&environment, &data_dir)
            .expect("the alias credential configures the provider");
        assert_eq!(
            alias.credential,
            CredentialSource::Environment {
                variable: "HA_API_KEY".to_owned()
            }
        );
        assert_eq!(alias.model, DEEPSEEK_MODEL);
    }

    #[test]
    fn h04_provider_setup_accepts_a_complete_environment_and_the_key_alias() {
        let (environment, data_dir) = credential_environment(&[
            (ENDPOINT_VARIABLE, "http://127.0.0.1:9/chat/completions"),
            (MODEL_VARIABLE, "fixture-model"),
            ("DEEPSEEK_API_KEY", "fixture-secret"),
        ]);
        let config = resolve_provider(&environment, &data_dir).expect("a complete setup resolves");
        assert_eq!(config.model, "fixture-model");
        assert_eq!(
            config.credential,
            CredentialSource::Environment {
                variable: "DEEPSEEK_API_KEY".to_owned()
            }
        );
        assert!(
            !config.endpoint.contains("fixture-secret"),
            "the credential never becomes part of the endpoint"
        );

        let (environment, data_dir) = credential_environment(&[
            (ENDPOINT_VARIABLE, "http://127.0.0.1:9/chat/completions"),
            (MODEL_VARIABLE, "fixture-model"),
            ("HA_API_KEY", "fixture-secret"),
        ]);
        let alias =
            resolve_provider(&environment, &data_dir).expect("the alias credential resolves");
        assert_eq!(
            alias.credential,
            CredentialSource::Environment {
                variable: "HA_API_KEY".to_owned()
            }
        );
    }

    /// K02: the file `/key` writes is a real credential source, and the
    /// environment still beats it.
    #[test]
    fn k02_a_saved_file_configures_the_provider_and_the_environment_still_wins() {
        let (environment, data_dir) = credential_environment(&[]);
        let path = credentials::resolve_file(&environment, &data_dir);
        credentials::save(&path, "sk-from-file").expect("the key is saved");

        let from_file =
            resolve_provider(&environment, &data_dir).expect("a saved key configures the provider");
        let CredentialSource::File {
            path: source_path,
            protection,
        } = &from_file.credential
        else {
            panic!("a saved file must be the active source: {from_file:?}");
        };
        assert_eq!(
            source_path, &path,
            "the source must be the file this test wrote"
        );
        assert_eq!(
            *protection,
            Protection::NotReverified,
            "a launch must not claim it re-measured the permissions"
        );
        assert_eq!(from_file.model, DEEPSEEK_MODEL);

        let (with_variable, same_dir) =
            credential_environment(&[("DEEPSEEK_API_KEY", "sk-from-environment")]);
        let precedence = resolve_provider(&with_variable, &same_dir)
            .expect("the environment and the file both resolve");
        assert_eq!(
            precedence.credential,
            CredentialSource::Environment {
                variable: "DEEPSEEK_API_KEY".to_owned()
            },
            "an exported variable must override the saved file"
        );

        // The parser round trip is asserted through `load` above. The resolver
        // itself reads the process environment and cannot be pointed at this
        // directory without mutating global state under parallel tests, so its
        // file branch is covered where that environment is controlled.
        let _ = EnvironmentCredential::new("DEEPSEEK_API_KEY", data_dir.clone());
        let _ = std::fs::remove_file(&path);
    }

    /// K02: the file `/key` writes is what the resolver reads, and a corrupt file
    /// is refused instead of being treated as no credential.
    ///
    /// A **missing** file is not this function's business: "no credential at all"
    /// is reported by `resolve_provider`, which is checked in its own case.
    #[test]
    fn k02_a_saved_file_is_readable_and_a_corrupt_one_is_refused() {
        let (environment, data_dir) = credential_environment(&[]);
        let path = credentials::resolve_file(&environment, &data_dir);
        validate_credential_file(&environment, &data_dir)
            .expect("no file is nothing to validate, not a failure");

        credentials::save(&path, "sk-validated").expect("the key is saved");
        validate_credential_file(&environment, &data_dir).expect("the saved file is readable");

        std::fs::write(&path, "DEEPSEEK_API_KEY=unquoted\n").expect("fixture");
        let error = validate_credential_file(&environment, &data_dir)
            .expect_err("a corrupt file must stop the launch");
        assert!(error.contains("credentials.env"), "{error}");
        assert!(
            !error.contains("unquoted"),
            "the refusal must not echo the file: {error}"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// The credential value must not appear in any rendered diagnostic.
    #[test]
    fn k02_diagnostics_name_the_source_and_never_the_value() {
        let (environment, data_dir) = credential_environment(&[]);
        let path = credentials::resolve_file(&environment, &data_dir);
        credentials::save(&path, "sk-must-not-be-rendered").expect("the key is saved");

        let lines = provider_diagnostics(&environment, &data_dir);
        let joined = lines.join("\n");
        assert!(joined.contains("credentials.env"), "{joined}");
        assert!(joined.contains("value hidden"), "{joined}");
        assert!(
            !joined.contains("sk-must-not-be-rendered"),
            "a diagnostic rendered the key: {joined}"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// T08 follow-up: `/status` must answer "what is this app actually using?".
    ///
    /// It names the credential variable (never the value), says whether the endpoint
    /// and the model came from the environment or from the defaults, and reports
    /// whether the endpoint answers. The reachability line is allowed to fail - a
    /// sandbox may have no route - so the assertion is on the shape, not on success.
    #[test]
    fn t08_status_diagnostics_name_the_source_of_every_provider_fact() {
        // A loopback endpoint keeps this test off the network; the reachability line
        // is asserted for its shape, never for the network being up.
        let (environment, data_dir) = credential_environment(&[
            ("DEEPSEEK_API_KEY", "fixture-secret-value"),
            (ENDPOINT_VARIABLE, "http://127.0.0.1:9/chat/completions"),
        ]);
        let configured = provider_diagnostics(&environment, &data_dir);
        let joined = configured.join("\n");
        assert!(
            joined.contains("credential from environment variable DEEPSEEK_API_KEY"),
            "{joined}"
        );
        assert!(
            !joined.contains("fixture-secret-value"),
            "the key value must never appear: {joined}"
        );
        assert!(
            joined.contains("model not set, using the default"),
            "{joined}"
        );
        assert!(joined.contains(DEEPSEEK_MODEL), "{joined}");
        assert!(
            joined.contains("ready, would call"),
            "a bare key is a complete setup: {joined}"
        );
        assert!(
            joined.contains("endpoint answered") || joined.contains("endpoint did not answer"),
            "reachability is reported either way: {joined}"
        );

        let (environment, data_dir) = credential_environment(&[
            ("HA_API_KEY", "fixture-secret-value"),
            (ENDPOINT_VARIABLE, "http://127.0.0.1:9/chat/completions"),
            (MODEL_VARIABLE, "fixture-model"),
        ]);
        let explicit = provider_diagnostics(&environment, &data_dir).join("\n");
        assert!(
            explicit.contains("credential from environment variable HA_API_KEY"),
            "{explicit}"
        );
        assert!(
            explicit.contains("endpoint HA_PROVIDER_ENDPOINT="),
            "{explicit}"
        );
        assert!(
            explicit.contains("model HA_PROVIDER_MODEL=fixture-model"),
            "{explicit}"
        );
        assert!(
            !explicit.contains("using the default"),
            "an explicit value is never reported as defaulted: {explicit}"
        );

        let (environment, data_dir) = credential_environment(&[]);
        let empty = provider_diagnostics(&environment, &data_dir).join("\n");
        assert!(empty.contains("no credential; set one of"), "{empty}");
        assert!(
            empty.contains("/key"),
            "the empty state must name the in-app way to fix it: {empty}"
        );
        assert!(empty.contains("not ready"), "{empty}");
    }

    #[test]
    fn h04_agent_service_says_setup_required_until_the_environment_is_configured() {
        let temp = tempfile::tempdir().expect("temp root");
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&home).expect("fixture home");
        std::fs::create_dir_all(&project).expect("fixture project");

        let context = |pairs: &[(&str, &str)]| {
            let mut all = vec![("HA_HOME", home.to_string_lossy().into_owned())];
            all.extend(
                pairs
                    .iter()
                    .map(|(name, value)| (*name, (*value).to_owned())),
            );
            let environment = LaunchEnvironment::from_pairs(all);
            bootstrap::resolve(LaunchRequest {
                cwd: None,
                caller_dir: project.clone(),
                platform: HostPlatform::current(),
                environment,
                explicit_data_dir: None,
            })
            .expect("context resolves")
        };

        let unconfigured = environment(&[]);
        let channel = SessionChannel::new();
        let service = AgentSessionService::new(&context(&[]), unconfigured, channel.sender());
        assert!(
            service.label().contains("setup required"),
            "{}",
            service.label()
        );

        let configured = environment(&[
            (ENDPOINT_VARIABLE, "http://127.0.0.1:9/chat/completions"),
            (MODEL_VARIABLE, "fixture-model"),
            ("DEEPSEEK_API_KEY", "fixture-secret"),
        ]);
        let channel = SessionChannel::new();
        let service = AgentSessionService::new(&context(&[]), configured, channel.sender());
        assert!(
            service.label().contains("fixture-model"),
            "{}",
            service.label()
        );
        assert!(
            !service.label().contains("fixture-secret"),
            "the label never renders the credential"
        );
    }
}
