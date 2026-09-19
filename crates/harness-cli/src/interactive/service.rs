//! Session port for the interactive app.
//!
//! The port is the only thing the controller knows about execution. Three
//! producers implement it: the real application service wired to the runtime and
//! the P3 tool gate, and a labelled fixture used by tests and explicit demos.
//! A production launch never falls back to the fixture: when provider settings
//! are missing the service reports exactly what to set.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use harness_providers::{
    CancellationToken, CredentialResolver, DeepSeekAdapter, ModelCapabilities, ModelProvider,
    ProviderError,
};
use harness_runtime::{RunRequest, RuntimeConfig, RuntimeService};
use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
use harness_tools::{
    ApprovalMode, ToolExecutionService, TurnDriver, TurnLimits, TurnObserver, TurnOptions,
    TurnProgress, TurnStop, coding_tool_schemas, observe_workspace,
};
use harness_types::{ErrorCode, HostId, InputId, ProjectId, SessionId, TaskId};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use super::bootstrap::{CREDENTIAL_VARIABLES, LaunchContext};
use super::events::{RunOutcome, SessionEvent};
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

/// What the controller needs from an execution backend.
pub trait SessionPort: Send {
    /// Label shown in the header; a fixture must never look like the real thing.
    fn label(&self) -> String;
    fn submit(&mut self, request: SubmitRequest);
    fn cancel(&mut self);
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
    cancellation: Option<CancellationToken>,
}

impl AgentSessionService {
    #[must_use]
    pub fn new(
        context: &LaunchContext,
        environment: LaunchEnvironment,
        sender: UnboundedSender<SessionEvent>,
    ) -> Self {
        Self {
            sender,
            store_dir: context.project_store_dir(),
            workspace_root: context.project.root.clone(),
            environment,
            task_id: TaskId::generate(),
            previous_session: Arc::new(Mutex::new(None)),
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
                request,
                cancellation,
            )
            .await;
        });
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
        // Interactive approval rendering is H05; until then a gated action fails
        // closed instead of being granted silently.
        approvals: ApprovalMode::None,
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
        AgentSessionService, ENDPOINT_VARIABLE, FixtureService, MODEL_VARIABLE, SessionChannel,
        SessionPort, SubmitRequest, resolve_provider,
    };
    use crate::interactive::bootstrap::{self, LaunchRequest};
    use crate::interactive::events::{RunOutcome, SessionEvent};
    use crate::interactive::paths::{HostPlatform, LaunchEnvironment};
    use harness_types::InputId;

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
