//! Session port for the interactive app.
//!
//! The port is the only thing the controller knows about execution. Three
//! producers implement it: the real application service wired to the runtime and
//! the P3 tool gate, and a labelled fixture used by tests and explicit demos.
//! A production launch never falls back to the fixture: when provider settings
//! are missing the service reports exactly what to set.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

use super::bootstrap::{CREDENTIAL_VARIABLES, LaunchContext};
use super::events::{RunOutcome, SessionCandidate, SessionEvent};
use super::paths::LaunchEnvironment;

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
    fn resume(&mut self, _session_id: Option<String>) {}
}

/// Asks the user for each gated action and waits for the answer.
///
/// The proposal travels as a session event; the answer comes back through
/// `ChannelApprovalGate::answer`. No answer inside the timeout is an expiry,
/// never a silent grant.
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
            request_id: proposal.request_id,
            action: proposal.action,
            summary: proposal.summary,
            workspace: proposal.workspace.display().to_string(),
            scope: proposal.scope,
        });
        let timeout = self.timeout;
        Box::pin(async move {
            match tokio::time::timeout(timeout, receiver).await {
                Ok(Ok(answer)) => answer,
                // The sender was dropped without an answer: refuse, never grant.
                Ok(Err(_)) => ApprovalAnswer::Denied,
                Err(_) => ApprovalAnswer::Expired,
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
    pub credential_variable: String,
}

/// Resolve provider settings from the environment.
///
/// Nothing is guessed: no default endpoint, no default model and no fixture
/// substitution. The error names every variable that is missing so the operator
/// can fix the setup instead of wondering why a run produced nothing.
pub fn resolve_provider(environment: &LaunchEnvironment) -> Result<ProviderConfig, String> {
    let endpoint = environment
        .value(ENDPOINT_VARIABLE)
        .filter(|value| !value.is_empty());
    let model = environment
        .value(MODEL_VARIABLE)
        .filter(|value| !value.is_empty());
    let credential = CREDENTIAL_VARIABLES
        .iter()
        .find(|name| {
            environment
                .value(name)
                .is_some_and(|value| !value.is_empty())
        })
        .copied();

    let mut missing = Vec::new();
    if endpoint.is_none() {
        missing.push(format!("{ENDPOINT_VARIABLE} (provider endpoint URL)"));
    }
    if model.is_none() {
        missing.push(format!("{MODEL_VARIABLE} (model name)"));
    }
    if credential.is_none() {
        missing.push(format!("{} (API key)", CREDENTIAL_VARIABLES.join(" or ")));
    }
    if !missing.is_empty() {
        return Err(format!(
            "provider setup is incomplete: set {}. Nothing was sent and no fixture answer was substituted.",
            missing.join(", ")
        ));
    }
    let (Some(endpoint), Some(model), Some(credential_variable)) = (endpoint, model, credential)
    else {
        return Err("provider setup is incomplete".to_owned());
    };
    Ok(ProviderConfig {
        endpoint: endpoint.to_string_lossy().into_owned(),
        model: model.to_string_lossy().into_owned(),
        credential_variable: credential_variable.to_owned(),
    })
}

/// Reads the credential when a call is made; the value is never stored, logged
/// or rendered anywhere in the app.
pub struct EnvironmentCredential {
    variable: String,
}

impl EnvironmentCredential {
    /// Bind the resolver to one environment variable name.
    #[must_use]
    pub fn new(variable: impl Into<String>) -> Self {
        Self {
            variable: variable.into(),
        }
    }
}

impl CredentialResolver for EnvironmentCredential {
    fn resolve(&self) -> Result<String, ProviderError> {
        match std::env::var(&self.variable) {
            Ok(value) if !value.trim().is_empty() => Ok(value),
            _ => Err(ProviderError::new(
                ErrorCode::SecretNotGranted,
                format!(
                    "credential {} is not present in the environment",
                    self.variable
                ),
            )),
        }
    }
}

/// Maps turn progress onto the UI vocabulary.
struct ChannelObserver {
    sender: UnboundedSender<SessionEvent>,
}

impl TurnObserver for ChannelObserver {
    fn observe(&self, progress: TurnProgress) {
        let event = match progress {
            TurnProgress::TextDelta(text) => SessionEvent::TextDelta { text },
            TurnProgress::ToolStarted { name, summary } => {
                SessionEvent::ToolStarted { name, summary }
            }
            TurnProgress::ToolSettled { name, ok } => SessionEvent::ToolSettled { name, ok },
            // Step boundaries are internal accounting, not something the user
            // needs to see on every turn.
            TurnProgress::StepStarted { .. } => return,
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
    workspace_root: PathBuf,
    environment: LaunchEnvironment,
    task_id: TaskId,
    previous_session: Arc<Mutex<Option<SessionId>>>,
    /// Asks the user for every gated action; never grants on its own.
    gate: Arc<ChannelApprovalGate>,
    cancellation: Option<CancellationToken>,
}

/// How long a gated action waits for the user before it expires.
const APPROVAL_TIMEOUT: Duration = Duration::from_mins(5);

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
            workspace_root: context.project.root.clone(),
            environment,
            task_id: TaskId::generate(),
            previous_session: Arc::new(Mutex::new(None)),
            gate,
            cancellation: None,
        }
    }

    fn configured(&self) -> Result<ProviderConfig, String> {
        resolve_provider(&self.environment)
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
        let workspace_root = self.workspace_root.clone();
        let environment = self.environment.clone();
        let task_id = self.task_id.clone();
        let previous_session = Arc::clone(&self.previous_session);
        let gate = Arc::clone(&self.gate);
        // Every user input opens its own session; the conversation is the chain of
        // sessions linked to the same task.
        let session_id = SessionId::generate();
        handle.spawn(async move {
            run_turn(
                sender,
                store_dir,
                workspace_root,
                environment,
                session_id,
                task_id,
                previous_session,
                gate,
                request,
                cancellation,
            )
            .await;
        });
    }

    fn answer(&mut self, request_id: &str, decision: ApprovalDecision) -> bool {
        self.gate.answer(request_id, decision)
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

    fn resume(&mut self, session_id: Option<String>) {
        match session_id {
            None => {
                if let Ok(mut guard) = self.previous_session.lock() {
                    *guard = None;
                }
                let _ = self.sender.send(SessionEvent::Notice {
                    message:
                        "starting a fresh conversation; the previous chain is no longer continued"
                            .to_owned(),
                });
            }
            Some(session_id) => {
                let sender = self.sender.clone();
                let store_dir = self.store_dir.clone();
                let previous = Arc::clone(&self.previous_session);
                let Ok(handle) = tokio::runtime::Handle::try_current() else {
                    return;
                };
                handle.spawn(async move {
                    let known = match SqliteStore::open_read_only(store_dir).await {
                        Ok(store) => store
                            .list_sessions()
                            .await
                            .is_ok_and(|summaries| {
                                summaries
                                    .iter()
                                    .any(|summary| summary.session_id.as_str() == session_id)
                            }),
                        Err(_) => false,
                    };
                    if !known {
                        let _ = sender.send(SessionEvent::Notice {
                            message: format!(
                                "session {session_id} is not in this project's store; nothing was resumed"
                            ),
                        });
                        return;
                    }
                    match harness_types::SessionId::parse(session_id.clone()) {
                        Ok(parsed) => {
                            if let Ok(mut guard) = previous.lock() {
                                *guard = Some(parsed);
                            }
                            let _ = sender.send(SessionEvent::Notice {
                                message: format!(
                                    "continuing from session {session_id}; the next request recovers that context"
                                ),
                            });
                        }
                        Err(_) => {
                            let _ = sender.send(SessionEvent::Notice {
                                message: format!(
                                    "session {session_id} is not a valid session id; nothing was resumed"
                                ),
                            });
                        }
                    }
                });
            }
        }
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
    workspace_root: PathBuf,
    environment: LaunchEnvironment,
    session_id: SessionId,
    task_id: TaskId,
    previous_session: Arc<Mutex<Option<SessionId>>>,
    gate: Arc<ChannelApprovalGate>,
    request: SubmitRequest,
    cancellation: CancellationToken,
) {
    let send = |event| {
        let _ = sender.send(event);
    };
    send(SessionEvent::Accepted {
        input_id: request.input_id.clone(),
    });

    let config = match resolve_provider(&environment) {
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

    let capabilities = ModelCapabilities {
        provider_id: "deepseek".to_owned(),
        model: config.model.clone(),
        supports_streaming: true,
        supports_tools: true,
        // A real adapter: this is never presented as a fixture.
        fixture: false,
    };
    let credentials = Arc::new(EnvironmentCredential {
        variable: config.credential_variable.clone(),
    });
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
        limits: TurnLimits::default(),
    };
    let observer: Arc<dyn TurnObserver> = Arc::new(ChannelObserver {
        sender: sender.clone(),
    });

    // A follow-up turn continues the previous session; the first turn starts one.
    let source = previous_session.lock().ok().and_then(|guard| guard.clone());
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
}

impl FixtureService {
    #[must_use]
    pub fn new(sender: UnboundedSender<SessionEvent>) -> Self {
        Self { sender }
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
        // Echoing the admitted text makes the transcript prove exactly which
        // buffer was submitted, which is what the editor tests assert on.
        send(SessionEvent::TextDelta {
            text: format!("fixture answer for: {}", request.text),
        });
        send(SessionEvent::TextDelta {
            text: " (no model was called)".to_owned(),
        });
        send(SessionEvent::ToolStarted {
            name: "search_text".to_owned(),
            summary: "pattern=parser".to_owned(),
        });
        send(SessionEvent::ToolSettled {
            name: "search_text".to_owned(),
            ok: false,
        });
        send(SessionEvent::ToolStarted {
            name: "apply_patch".to_owned(),
            summary: "path=src/parser.rs".to_owned(),
        });
        send(SessionEvent::ToolSettled {
            name: "apply_patch".to_owned(),
            ok: true,
        });
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
}

#[cfg(test)]
mod tests {
    use super::{
        AgentSessionService, ApprovalDecision, ApprovalGate, ApprovalProposal, ChannelApprovalGate,
        ENDPOINT_VARIABLE, FixtureService, MODEL_VARIABLE, SessionChannel, SessionPort,
        SubmitRequest, resolve_provider,
    };
    use crate::interactive::bootstrap::{self, LaunchRequest};
    use crate::interactive::events::{RunOutcome, SessionEvent};
    use crate::interactive::paths::{HostPlatform, LaunchEnvironment};
    use harness_tools::ApprovalAnswer;
    use harness_types::InputId;
    use std::sync::Arc;
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
    fn h04_provider_setup_names_every_missing_variable_without_guessing() {
        let error = resolve_provider(&environment(&[]))
            .expect_err("an empty environment is not configured");
        assert!(error.contains(ENDPOINT_VARIABLE), "{error}");
        assert!(error.contains(MODEL_VARIABLE), "{error}");
        assert!(error.contains("DEEPSEEK_API_KEY"), "{error}");
        assert!(
            error.contains("no fixture answer"),
            "the message must say nothing was substituted: {error}"
        );

        let partial = environment(&[(ENDPOINT_VARIABLE, "http://127.0.0.1:1/chat")]);
        let error = resolve_provider(&partial).expect_err("a partial setup is not configured");
        assert!(
            !error.contains(ENDPOINT_VARIABLE),
            "the endpoint is present: {error}"
        );
        assert!(error.contains(MODEL_VARIABLE), "{error}");
    }

    #[test]
    fn h04_provider_setup_accepts_a_complete_environment_and_the_key_alias() {
        let config = resolve_provider(&environment(&[
            (ENDPOINT_VARIABLE, "http://127.0.0.1:9/chat/completions"),
            (MODEL_VARIABLE, "fixture-model"),
            ("DEEPSEEK_API_KEY", "fixture-secret"),
        ]))
        .expect("a complete setup resolves");
        assert_eq!(config.model, "fixture-model");
        assert_eq!(config.credential_variable, "DEEPSEEK_API_KEY");
        assert!(
            !config.endpoint.contains("fixture-secret"),
            "the credential never becomes part of the endpoint"
        );

        let alias = resolve_provider(&environment(&[
            (ENDPOINT_VARIABLE, "http://127.0.0.1:9/chat/completions"),
            (MODEL_VARIABLE, "fixture-model"),
            ("HA_API_KEY", "fixture-secret"),
        ]))
        .expect("the alias credential resolves");
        assert_eq!(alias.credential_variable, "HA_API_KEY");
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
