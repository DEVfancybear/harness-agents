//! Session port for the interactive app.
//!
//! The port is the only thing the controller knows about execution. Three
//! producers implement it: the real application service wired to the runtime and
//! the P3 tool gate, and a labelled fixture used by tests and explicit demos.
//! A production launch never falls back to the fixture: when provider settings
//! are missing the service reports exactly what to set.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use harness_providers::anthropic::AnthropicMessagesAdapter;
use harness_providers::{
    CancellationToken, CredentialResolver, MessageRole, ModelCapabilities, ModelProvider,
    OpenAiChatAdapter, OpenAiChatOptions, ProviderError, ProviderMessage, ProviderRequest,
};
use harness_runtime::{HumanInputService, RunInbox, RunRequest, RuntimeConfig, RuntimeService};
use harness_session::{AdmitInputRequest, SessionService};
use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
use harness_tools::{
    ApprovalAnswer, ApprovalGate, ApprovalMode, ApprovalProposal, CodingToolAction, IsolationMode,
    PolicyMode, ToolExecutionService, ToolOutput, ToolPatternRule, ToolPolicyRules, TurnDriver,
    TurnLimits, TurnObserver, TurnOptions, TurnProgress, TurnStop, coding_tool_schemas,
    execute_action_with_approval, observe_workspace, observed_file_hash, validate_tool_pattern,
};
use harness_types::{
    ErrorCode, HostId, InputId, QuestionId, RequestId, SessionId, SourceAuthority, TaskId,
};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

use super::attachments;
use super::bootstrap::{CREDENTIAL_VARIABLES, LaunchContext};
use super::bounds;
use super::config::ConfigOverrides;
#[cfg(test)]
use super::config::{DEEPSEEK_ENDPOINT, DEEPSEEK_MODEL};
use super::controller::{DEFAULT_APPROVAL_TIMEOUT, TurnBounds};
use super::cost::{CostTracker, ModelPrice, Usage as CostUsage};
use super::credentials::{self, CredentialSource};
use super::events::{PauseReason, RunOutcome, SessionCandidate, SessionEvent, ShellPrefixMode};
use super::extensions;
use super::paths::LaunchEnvironment;
use super::project;
use super::prompt::{PromptEnvironment, SystemPromptBuilder};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct TurnModelSelection {
    active_model: Option<String>,
    pending_model: Option<String>,
}

impl TurnModelSelection {
    fn with_initial(model: Option<String>) -> Self {
        Self {
            active_model: model,
            pending_model: None,
        }
    }

    fn select_for_next_turn(&mut self, model: String) {
        self.pending_model = Some(model);
    }

    fn begin_turn(&mut self) -> Option<String> {
        if let Some(model) = self.pending_model.take() {
            self.active_model = Some(model);
        }
        self.active_model.clone()
    }
}

#[cfg(test)]
mod g03_model_selection_tests {
    use super::TurnModelSelection;

    #[test]
    fn g03_model_switch_applies_next_turn_only() {
        let mut selection = TurnModelSelection::with_initial(Some("active-model".to_owned()));
        let running_turn = selection.begin_turn().expect("active model");
        selection.select_for_next_turn("next-model".to_owned());
        assert_eq!(selection.active_model.as_deref(), Some("active-model"));
        assert_eq!(running_turn, "active-model");
        assert_eq!(selection.begin_turn().as_deref(), Some("next-model"));
    }
}

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
    /// Durable question answered by this new session, when this is an answer turn.
    pub answer_question_id: Option<String>,
    /// Host-initiated shell action represented by this admitted input, when set.
    pub shell_prefix: Option<ShellPrefix>,
    /// A host-issued compaction action. It shares the normal admitted-input and
    /// cancellation path but never dispatches the command text to the model.
    pub compact_guidance: Option<String>,
    /// A host-issued `/refine`: like compaction, it never reaches the model as a turn.
    pub refine: Option<super::refine::RefineOptions>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShellPrefix {
    pub command: String,
    pub mode: ShellPrefixMode,
}

/// The user's decision on one gated action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApprovalDecision {
    /// Run this exact action once.
    Granted,
    /// Run this action once, and stop asking about **any** action until the run
    /// ends.
    ///
    /// It grants the pending request as well, because a panel that offers "allow
    /// this turn" and then blocks the action in front of you would be a lie. The
    /// grant is bounded by the turn: it is dropped when the run reaches its
    /// terminal event, and it is recorded - one transcript line per action it
    /// covers - so nothing runs unasked *and* unseen.
    ///
    /// Measured complaint that widened it: a turn of `git log`, `git status`,
    /// `git diff` asked about every single command, and the old read-only grant
    /// could not cover any of them, because `run_process` is not a read-only kind.
    GrantForRun,
    /// Do not run it.
    Denied,
}

/// What the controller needs from an execution backend.
pub trait SessionPort: Send {
    /// Label shown in the header; a fixture must never look like the real thing.
    fn label(&self) -> String;
    fn submit(&mut self, request: SubmitRequest);
    fn cancel(&mut self);
    /// Deliver an immediate correction to the run currently in progress.
    fn steer(&mut self, _text: &str) -> Result<(), String> {
        Err("this backend does not support steering an active run".to_owned())
    }
    /// Answer one pending approval request; false when the id is not pending.
    fn answer(&mut self, _request_id: &str, _decision: ApprovalDecision) -> bool {
        false
    }
    /// Stop asking about gated actions until the current run ends.
    ///
    /// Separate from `answer` because it is a property of the run rather than a
    /// reply to one proposal: the controller grants it when the user picks the
    /// wider option, and revokes it when the run reaches its terminal event. The
    /// policy checks that run before a proposal exists are **not** part of this
    /// grant: it skips the question, never the check.
    fn grant_run_approval(&mut self) {}
    /// Revoke the turn-wide grant, because the run it was given for is over.
    fn revoke_run_approval(&mut self) {}
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
    /// The full bounds one turn runs under, for `/status`.
    ///
    /// The default mirrors `limits()` and keeps the driver's own deadline, so a backend
    /// that only knows the two counts still reports what a pause would mean.
    fn turn_limits(&self) -> TurnLimits {
        let bounds = self.limits();
        TurnLimits {
            max_steps: bounds.max_steps,
            max_tool_calls: bounds.max_tool_calls,
            deadline: TurnLimits::default().deadline,
        }
    }
    /// Lines `/status` prints about the provider: which credential variable holds
    /// the key (never its value), whether the endpoint and the model came from the
    /// environment or from the defaults, and whether the endpoint answers.
    fn provider_diagnostics(&self) -> Vec<String> {
        Vec::new()
    }
    fn config_explain(&self) -> Vec<String> {
        Vec::new()
    }
    fn hooks_summary(&self) -> Vec<String> {
        vec!["no hooks are configured".to_owned()]
    }
    fn mcp_summary(&self) -> Vec<String> {
        vec!["MCP status is unavailable".to_owned()]
    }
    fn agents_summary(&self) -> Vec<String> {
        vec!["no delegated workers have run in this session".to_owned()]
    }
    fn skills_summary(&self) -> Vec<String> {
        vec!["skill catalog is unavailable".to_owned()]
    }
    fn activate_skill(&mut self, _name: &str) -> Result<String, String> {
        Err("this backend cannot activate skills".to_owned())
    }
    fn reload(&mut self) -> Result<String, String> {
        Err("this backend cannot reload prompt inputs".to_owned())
    }
    fn expand_prompt_command(
        &self,
        _name: &str,
        _arguments: &str,
    ) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn respond_mcp_elicitation(&mut self, _request_id: &str, _answer: &str) -> Result<(), String> {
        Err("this backend cannot answer MCP elicitation".to_owned())
    }
    fn cost_summary(&self) -> String {
        "n/a".to_owned()
    }
    fn context_summary(&self) -> Vec<String> {
        vec!["no context packet has been built in this session yet".to_owned()]
    }
    fn git_diff(&mut self) -> Result<(), String> {
        Err("this backend does not support session diffs".to_owned())
    }
    fn rename(&mut self, _title: &str) -> Result<String, String> {
        Err("this backend does not support session titles".to_owned())
    }
    /// The goal the next turns work toward; `None` while it is paused or gone.
    fn set_goal(&mut self, _objective: Option<String>) {}
    /// Drop the stored goal, so `/resume` no longer brings it back.
    fn forget_goal(&mut self) {}
    fn set_model(&mut self, _model: &str) -> Result<String, String> {
        Err("this backend does not support model switching".to_owned())
    }
    /// The agent's heartbeats whose time has come, advanced to their next run.
    fn due_heartbeats(&mut self) -> Vec<super::heartbeat::Due> {
        Vec::new()
    }
    /// Choose the thinking level for the next turns.
    fn set_thinking(&mut self, _level: &str) -> Result<String, String> {
        Err("this backend does not support thinking levels".to_owned())
    }
    /// The thinking level in force and the levels the model offers.
    fn thinking_status(&self) -> Vec<String> {
        vec!["this backend does not support thinking levels".to_owned()]
    }
    fn set_mode(&mut self, _mode: &str) -> Result<String, String> {
        Err("this backend does not support session permission modes".to_owned())
    }
    fn permissions_summary(&mut self) -> Vec<String> {
        Vec::new()
    }
    fn confirm_always_allow(
        &mut self,
        _request_id: &str,
        _pattern: &str,
    ) -> Result<String, String> {
        Err("this backend cannot save permission rules".to_owned())
    }
    fn trust_project(&mut self) -> Result<String, String> {
        Err("this backend cannot update project trust".to_owned())
    }
    /// Why a real dispatch is impossible right now, or `None` when it is possible.
    ///
    /// The controller asks this before submitting a turn, so the answer is always
    /// evaluated against the *current* credential: after `/key` saves one, this
    /// turns `None` and the very next message reaches the provider.
    fn provider_problem(&self) -> Option<String> {
        None
    }
    /// The project identity this workspace is scoped to, for `/status`.
    ///
    /// Memory is scoped by this id, and the app otherwise never shows it: the
    /// projects directory is named after a digest, so an operator who wants to
    /// inspect what a turn stored has nothing to pass to `ha memory`. This is the
    /// missing link, answered on demand because resolving it needs the store.
    fn project_id(&mut self) -> Option<String> {
        None
    }
    /// Save a key so this session and the next launch can use it.
    ///
    /// The file is the source of truth because the credential resolver re-reads it
    /// at call time; nothing has to restart for the next turn to use it. The
    /// returned source carries the source *name*, never the value.
    fn save_credential(&mut self, _key: &str) -> Result<CredentialSource, String> {
        Err("this backend cannot save a provider credential".to_owned())
    }
}

/// Asks the user for each gated action and waits for the answer.
///
/// The proposal travels as a session event; the answer comes back through
/// `ChannelApprovalGate::answer`. No answer inside the timeout is an expiry,
/// never a silent grant, and an expiry is **announced** as
/// `SessionEvent::ApprovalExpired` before the driver is told, so the panel closes
/// instead of waiting for the run to end.
///
/// One thing this gate *may* let through without asking: any action, once the user
/// has answered `a` - allow this turn - with
/// [`ChannelApprovalGate::grant_for_run`]. That grant is a field on this object,
/// not on disk, and the controller clears it when the run ends, so it cannot
/// outlive the turn it was given for. It covers every kind, including patches and
/// processes: the measured complaint was a turn of `git log`/`git status` asking
/// about each command, and `run_process` is not a read-only kind. The checks that
/// matter are untouched - `ToolExecutionService::prepare` validates the workspace
/// path and the policy *before* a proposal exists, so an action that is refused
/// outright never reaches this gate and is not rescued by the grant.
pub struct ChannelApprovalGate {
    sender: UnboundedSender<SessionEvent>,
    pending: Mutex<HashMap<String, oneshot::Sender<ApprovalAnswer>>>,
    timeout: Duration,
    /// Whether the user allowed every action for the run now in flight.
    granted_for_run: Arc<AtomicBool>,
    confirmed_rules: Mutex<HashMap<String, ToolPatternRule>>,
    turn_rules: ToolPolicyRules,
    bell: AtomicBool,
}

impl ChannelApprovalGate {
    #[must_use]
    pub fn new(sender: UnboundedSender<SessionEvent>, timeout: Duration) -> Self {
        Self {
            sender,
            pending: Mutex::new(HashMap::new()),
            timeout,
            granted_for_run: Arc::new(AtomicBool::new(false)),
            confirmed_rules: Mutex::new(HashMap::new()),
            turn_rules: ToolPolicyRules::default(),
            bell: AtomicBool::new(false),
        }
    }

    /// The rule set shared with this turn's existing `ToolPolicy`.
    #[must_use]
    pub fn turn_rules(&self) -> ToolPolicyRules {
        self.turn_rules.clone()
    }

    /// Clear ephemeral confirmations at the start of a new user input.
    pub fn clear_turn_rules(&self) {
        self.turn_rules.clear();
        if let Ok(mut rules) = self.confirmed_rules.lock() {
            rules.clear();
        }
    }

    fn stage_always_allow(&self, request_id: &str, pattern: &str) -> Result<(), String> {
        if !self.is_pending(request_id) {
            return Err("approval is no longer pending; the rule was not staged".to_owned());
        }
        validate_tool_pattern(pattern).map_err(|error| error.to_string())?;
        self.confirmed_rules
            .lock()
            .map_err(|_| "confirmed permission rules are unavailable".to_owned())?
            .insert(
                request_id.to_owned(),
                ToolPatternRule::allow(pattern, "user-confirmed rule"),
            );
        Ok(())
    }

    /// Allow every gated action without asking, until the run ends.
    ///
    /// Idempotent, and deliberately not persisted anywhere: the only way to widen
    /// it is another answer from the user.
    pub fn grant_for_run(&self) {
        self.granted_for_run.store(true, Ordering::SeqCst);
    }

    /// Stop allowing actions without asking.
    ///
    /// The controller calls this when a run reaches its terminal event, so the
    /// grant covers one turn and never the next one.
    pub fn clear_grant_for_run(&self) {
        self.granted_for_run.store(false, Ordering::SeqCst);
    }

    fn set_bell(&self, enabled: bool) {
        self.bell.store(enabled, Ordering::SeqCst);
    }

    /// Whether gated actions are currently allowed without asking.
    #[must_use]
    pub fn granted_for_run(&self) -> bool {
        self.granted_for_run.load(Ordering::SeqCst)
    }

    /// Answer one pending request.
    pub fn answer(&self, request_id: &str, decision: ApprovalDecision) -> bool {
        if decision == ApprovalDecision::GrantForRun {
            self.grant_for_run();
        }
        let pending = self
            .pending
            .lock()
            .ok()
            .and_then(|mut map| map.remove(request_id));
        match pending {
            Some(sender) => sender
                .send(match decision {
                    ApprovalDecision::Granted | ApprovalDecision::GrantForRun => {
                        ApprovalAnswer::Granted
                    }
                    ApprovalDecision::Denied => ApprovalAnswer::Denied,
                })
                .is_ok(),
            None => false,
        }
    }

    fn is_pending(&self, request_id: &str) -> bool {
        self.pending
            .lock()
            .is_ok_and(|pending| pending.contains_key(request_id))
    }

    fn action_completed(&self, request_id: &str) {
        let rule = self
            .confirmed_rules
            .lock()
            .ok()
            .and_then(|mut rules| rules.remove(request_id));
        if let Some(rule) = rule
            && self.turn_rules.add(rule.clone()).is_err()
        {
            let _ = self.sender.send(SessionEvent::Notice {
                message: format!(
                    "[permissions] {} was saved and will apply on the next turn",
                    rule.pattern
                ),
            });
        }
    }
}

impl ApprovalGate for ChannelApprovalGate {
    fn request(
        &self,
        proposal: ApprovalProposal,
    ) -> Pin<Box<dyn Future<Output = ApprovalAnswer> + Send>> {
        // An action the user already allowed for this turn never reaches the panel,
        // so the transcript shows the turn's work rather than one block per step. It
        // is still announced: a grant that made work invisible would be worse than
        // the question it replaced.
        if self.granted_for_run() {
            let _ = self.sender.send(SessionEvent::Notice {
                message: format!(
                    "allowed by mode turn-grant: {}{}",
                    proposal.summary,
                    if proposal.read_only {
                        " (read-only)"
                    } else {
                        ""
                    }
                ),
            });
            return Box::pin(async { ApprovalAnswer::Granted });
        }
        let (sender, receiver) = oneshot::channel();
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(proposal.request_id.clone(), sender);
        }
        if self.bell.load(Ordering::SeqCst) {
            let _ = self.sender.send(SessionEvent::Bell);
        }
        let _ = self.sender.send(SessionEvent::ApprovalRequired {
            request_id: proposal.request_id.clone(),
            action: proposal.action,
            summary: proposal.summary,
            rule_pattern: proposal.rule_pattern,
            workspace: proposal.workspace.display().to_string(),
            scope: proposal.scope,
            // The deadline travels with the proposal: the panel counts down to
            // the gate's real timeout instead of hard-coding a second one.
            expires_at: Instant::now() + self.timeout,
            read_only: proposal.read_only,
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

    fn action_completed(&self, request_id: &str) {
        Self::action_completed(self, request_id);
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
#[derive(Clone, Debug, PartialEq)]
pub struct ProviderConfig {
    pub provider_id: String,
    pub protocol: String,
    pub endpoint: String,
    pub model: String,
    pub api_key_env: String,
    pub thinking: String,
    pub approval: String,
    pub allow_rules: Vec<String>,
    pub deny_rules: Vec<String>,
    pub model_price: Option<ModelPrice>,
    pub context_window_tokens: u64,
    pub context_window_notice: Option<String>,
    pub output_reservation_tokens: u64,
    pub compaction_reserve_tokens: u64,
    pub max_retry_after_seconds: u64,
    pub hooks: Vec<harness_tools::ConfiguredToolHook>,
    pub mcp_servers: BTreeMap<String, harness_types::McpServerConfigV2>,
    pub project_trusted: bool,
    pub bell: bool,
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
            CredentialSource::File { .. } => self.api_key_env.clone(),
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
#[cfg(test)]
pub fn resolve_provider(
    environment: &LaunchEnvironment,
    data_dir: &Path,
) -> Result<ProviderConfig, String> {
    resolve_provider_with_config(
        Path::new(".ha-no-user-config.toml"),
        Path::new("."),
        environment,
        data_dir,
    )
}

#[cfg(test)]
fn resolve_provider_with_config(
    user_path: &Path,
    project_root: &Path,
    environment: &LaunchEnvironment,
    data_dir: &Path,
) -> Result<ProviderConfig, String> {
    resolve_provider_with_overrides(
        user_path,
        project_root,
        environment,
        data_dir,
        &ConfigOverrides::default(),
    )
}

pub(super) fn resolve_provider_with_overrides(
    user_path: &Path,
    project_root: &Path,
    environment: &LaunchEnvironment,
    data_dir: &Path,
    overrides: &ConfigOverrides,
) -> Result<ProviderConfig, String> {
    let resolved = super::config::resolve_layers(user_path, project_root, environment, overrides)
        .map_err(|error| error.to_string())?;
    if !matches!(
        resolved.provider.protocol.as_str(),
        "openai_chat" | "anthropic_messages"
    ) {
        return Err(format!(
            "unsupported provider protocol {:?}",
            resolved.provider.protocol
        ));
    }
    let credential = credentials::source_for(environment, data_dir, &resolved.provider.api_key_env)
        .or_else(|| credentials::source(environment, data_dir));
    let Some(credential) = credential else {
        return Err(format!(
            "provider setup is incomplete: set {} or save the key in the app with /key (API key). Nothing was sent and no fixture answer was substituted.",
            resolved.provider.api_key_env
        ));
    };
    let model_price = resolved.model_prices.get(&resolved.provider.model).copied();
    let max_retry_after_seconds = resolved.retry_after_max_seconds;
    Ok(ProviderConfig {
        provider_id: resolved.provider.id,
        protocol: resolved.provider.protocol,
        endpoint: resolved.provider.endpoint,
        model: resolved.provider.model,
        api_key_env: resolved.provider.api_key_env,
        thinking: resolved.provider.thinking,
        approval: resolved.approval,
        allow_rules: resolved.allow_rules,
        deny_rules: resolved.deny_rules,
        model_price,
        context_window_tokens: resolved.context_window_tokens,
        context_window_notice: resolved.context_window_notice,
        output_reservation_tokens: resolved.output_reservation_tokens,
        compaction_reserve_tokens: resolved.compaction_reserve_tokens,
        max_retry_after_seconds,
        hooks: resolved.hooks,
        mcp_servers: resolved.mcp_servers,
        project_trusted: resolved.project_trusted,
        bell: resolved.bell,
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
#[cfg(test)]
pub fn provider_diagnostics(environment: &LaunchEnvironment, data_dir: &Path) -> Vec<String> {
    provider_diagnostics_with_config(
        Path::new(".ha-no-user-config.toml"),
        Path::new("."),
        environment,
        data_dir,
    )
}

fn provider_diagnostics_with_config(
    user_path: &Path,
    project_root: &Path,
    environment: &LaunchEnvironment,
    data_dir: &Path,
) -> Vec<String> {
    let resolved = match super::config::resolve_layers(
        user_path,
        project_root,
        environment,
        &super::config::ConfigOverrides::default(),
    ) {
        Ok(config) => config,
        Err(error) => return vec![format!("Provider: configuration unavailable ({error})")],
    };
    let mut lines = vec![format!(
        "Provider: {} ({})",
        resolved.provider.id, resolved.provider.protocol
    )];
    match credentials::source_for(environment, data_dir, &resolved.provider.api_key_env)
        .or_else(|| credentials::source(environment, data_dir))
    {
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
    for key in ["provider.endpoint", "provider.model"] {
        if let Some(entry) = resolved.explain.iter().find(|entry| entry.key == key) {
            if entry.layer == super::config::ConfigLayer::Default {
                lines.push(format!(
                    "Provider: {} not set, using the default {}",
                    key, entry.value
                ));
            } else if entry.layer == super::config::ConfigLayer::Environment {
                let (label, variable) = match key {
                    "provider.endpoint" => ("endpoint", ENDPOINT_VARIABLE),
                    "provider.model" => ("model", MODEL_VARIABLE),
                    _ => (key, "unknown"),
                };
                lines.push(format!(
                    "Provider: {label} {variable}={} ({})",
                    entry.value,
                    entry.layer.as_str()
                ));
            } else {
                lines.push(format!(
                    "Provider: {}={} ({})",
                    key,
                    entry.value,
                    entry.layer.as_str()
                ));
            }
        }
    }
    match credentials::source_for(environment, data_dir, &resolved.provider.api_key_env)
        .or_else(|| credentials::source(environment, data_dir))
    {
        Some(_) => {
            lines.push(format!(
                "Provider: ready, would call {}",
                resolved.provider.model
            ));
            lines.push(match endpoint_reachability(&resolved.provider.endpoint) {
                Ok(()) => "Provider: endpoint answered a TCP connection".to_owned(),
                Err(reason) => format!("Provider: endpoint did not answer ({reason})"),
            });
        }
        None => lines.push(format!(
            "Provider: not ready (set {} or save a key with /key)",
            resolved.provider.api_key_env
        )),
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
    cost_tracker: Arc<Mutex<CostTracker>>,
    model_price: Option<ModelPrice>,
    auto_allowed_count: Arc<AtomicUsize>,
    bell: bool,
    /// Calls are executed serially by the turn driver. Keeping the current
    /// boundary here makes duration delivery O(1) and avoids a process-lifetime
    /// map keyed by a non-unique tool name.
    tool_started: Mutex<Option<(String, Instant)>>,
}

impl TurnObserver for ChannelObserver {
    fn observe(&self, progress: TurnProgress) {
        let event = match progress {
            TurnProgress::TextDelta(text) => Some(SessionEvent::TextDelta { text }),
            TurnProgress::ThinkingDelta(text) => Some(SessionEvent::ThinkingDelta { text }),
            TurnProgress::Info(message) => {
                self.auto_allowed_count.fetch_add(1, Ordering::Relaxed);
                Some(SessionEvent::Notice { message })
            }
            TurnProgress::Notice(message) => Some(SessionEvent::Notice { message }),
            TurnProgress::Usage {
                prompt_tokens,
                completion_tokens,
            } => {
                let label = if let Ok(mut tracker) = self.cost_tracker.lock() {
                    tracker.record(
                        self.model_price,
                        CostUsage {
                            input_tokens: prompt_tokens,
                            output_tokens: completion_tokens,
                        },
                    );
                    tracker.display()
                } else {
                    "n/a".to_owned()
                };
                Some(SessionEvent::CostUpdated { label })
            }
            // Step boundaries are what the status bar counts (`step 2/8`).
            TurnProgress::StepStarted { step } => Some(SessionEvent::StepStarted { step }),
            TurnProgress::ToolStarted { name, summary } => {
                if self.bell && name == "ask_user" {
                    let _ = self.sender.send(SessionEvent::Bell);
                }
                if let Ok(mut started) = self.tool_started.lock() {
                    *started = Some((name.clone(), Instant::now()));
                }
                Some(SessionEvent::ToolStarted { name, summary })
            }
            TurnProgress::ToolSettled { name, ok, detail } => {
                let elapsed = self
                    .tool_started
                    .lock()
                    .ok()
                    .and_then(|mut started| started.take())
                    .filter(|(started_name, _)| started_name == &name)
                    .map_or(Duration::ZERO, |(_, started)| started.elapsed());
                Some(SessionEvent::ToolSettled {
                    name,
                    ok,
                    elapsed,
                    detail: detail.unwrap_or_default(),
                })
            }
        };
        if let Some(event) = event {
            let _ = self.sender.send(event);
        }
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
    config_file: PathBuf,
    workspace_root: PathBuf,
    caller_dir: PathBuf,
    global_config_dir: PathBuf,
    environment: LaunchEnvironment,
    config_overrides: ConfigOverrides,
    model_selection: Arc<Mutex<TurnModelSelection>>,
    cost_tracker: Arc<Mutex<CostTracker>>,
    session_mode: Arc<Mutex<Option<PolicyMode>>>,
    auto_allowed_count: Arc<AtomicUsize>,
    task_id: TaskId,
    previous_session: Arc<Mutex<Option<SessionId>>>,
    /// Current run's durable inbox, exposed only while its driver is active.
    active_inbox: Arc<Mutex<Option<ActiveTurnInbox>>>,
    /// Asks the user for every gated action; never grants on its own.
    gate: Arc<ChannelApprovalGate>,
    cancellation: Option<CancellationToken>,
    /// The limits this service hands to the driver, reported to the UI.
    limits: TurnLimits,
    /// The project identity, once `/status` has asked for it.
    ///
    /// Cached because resolving it opens the project store, and only an explicit
    /// request should pay for that: the app deliberately opens no store until the
    /// first turn arrives.
    project_id: Arc<Mutex<Option<String>>>,
    context_summary: Arc<Mutex<Vec<String>>>,
    active_skills: Arc<Mutex<BTreeMap<String, harness_extensions::SkillActivation>>>,
    pending_mcp_elicitations: Arc<Mutex<HashMap<String, PendingMcpElicitationRequest>>>,
    mcp_status: Arc<Mutex<Vec<String>>>,
    agents_status: Arc<Mutex<Vec<String>>>,
    /// Held by a turn for as long as it owns the project store, and by `/rename` while
    /// it writes, so the two never want the writer at the same time.
    writer_gate: Arc<tokio::sync::Mutex<()>>,
    /// The active goal, carried into every turn while it is set.
    goal: Option<String>,
    /// The stored goal must be erased by the next turn.
    goal_forgotten: bool,
    /// The Python REPL, kept for the whole session so its state outlives a turn.
    repl: Option<Arc<super::repl::ReplShared>>,
    /// The thinking level `/thinking` chose, for the next turns.
    thinking: Option<harness_providers::ThinkingLevel>,
    /// Turns since the last automatic refine review.
    turns_since_review: Arc<std::sync::atomic::AtomicU32>,
    /// The agent's own recurring prompts (`rlm_heartbeat`), for the whole session.
    heartbeats: Arc<super::heartbeat::Heartbeats>,
    /// `HA_AUTO_REFINE=off` turns the automatic review off (prime-agent's
    /// `autoRefine.enabled`, on by default).
    auto_refine: bool,
}

enum McpElicitationAnswer {
    Accept(serde_json::Value),
    Decline,
    Cancel,
}

struct PendingMcpElicitationRequest {
    schema: Option<serde_json::Value>,
    url: Option<String>,
    reply: oneshot::Sender<McpElicitationAnswer>,
}

struct InteractiveMcpCallbacks {
    server: String,
    provider: Arc<dyn ModelProvider>,
    model: String,
    sender: UnboundedSender<SessionEvent>,
    pending: Arc<Mutex<HashMap<String, PendingMcpElicitationRequest>>>,
    cancellation: CancellationToken,
}

#[allow(deprecated)]
impl harness_extensions::McpRequestCallbacks for InteractiveMcpCallbacks {
    fn sample<'a>(
        &'a self,
        request: harness_extensions::rmcp::model::CreateMessageRequestParams,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        harness_extensions::rmcp::model::CreateMessageResult,
                        harness_extensions::rmcp::model::ErrorData,
                    >,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            use harness_extensions::rmcp::model::{Role, SamplingMessageContentBlock};
            const MAX_MESSAGES: usize = 64;
            const MAX_INPUT_BYTES: usize = 256 * 1024;
            if request.messages.is_empty() || request.messages.len() > MAX_MESSAGES {
                return Err(harness_extensions::rmcp::model::ErrorData::invalid_params(
                    format!("sampling requires 1..={MAX_MESSAGES} messages"),
                    None,
                ));
            }
            if request
                .tools
                .as_ref()
                .is_some_and(|tools| !tools.is_empty())
            {
                return Err(harness_extensions::rmcp::model::ErrorData::invalid_params(
                    "sampling tools are not available on the text-only callback",
                    None,
                ));
            }
            let mut messages = Vec::with_capacity(request.messages.len() + 1);
            if let Some(system) = request.system_prompt.filter(|text| !text.trim().is_empty()) {
                messages.push(ProviderMessage::new(MessageRole::System, system));
            }
            let mut total_bytes = messages
                .iter()
                .map(|message| message.content.len())
                .sum::<usize>();
            for message in request.messages {
                let role = match message.role {
                    Role::User => MessageRole::User,
                    Role::Assistant => MessageRole::Assistant,
                };
                let mut chunks = Vec::new();
                for block in message.content.into_vec() {
                    match block {
                        SamplingMessageContentBlock::Text(text) => chunks.push(text.text),
                        _ => {
                            return Err(
                                harness_extensions::rmcp::model::ErrorData::invalid_params(
                                    "sampling accepts text messages only",
                                    None,
                                ),
                            );
                        }
                    }
                }
                let content = chunks.join("\n");
                total_bytes = total_bytes.saturating_add(content.len());
                if total_bytes > MAX_INPUT_BYTES {
                    return Err(harness_extensions::rmcp::model::ErrorData::invalid_params(
                        "sampling request exceeds the 256 KiB input limit",
                        None,
                    ));
                }
                messages.push(ProviderMessage::new(role, content));
            }
            let max_tokens = request.max_tokens.clamp(1, 4096);
            let call = self.provider.stream(
                ProviderRequest::new(RequestId::generate(), self.model.clone(), messages)
                    .with_max_output_tokens(max_tokens),
                self.cancellation.clone(),
            );
            let events = tokio::select! {
                () = self.cancellation.cancelled() => return Err(mcp_callback_error("sampling was canceled with its parent turn")),
                result = tokio::time::timeout(Duration::from_mins(1), call) => match result {
                    Ok(Ok(events)) => events,
                    Ok(Err(error)) => return Err(mcp_callback_error(&format!("sampling provider failed: {error}"))),
                    Err(_) => return Err(mcp_callback_error("sampling exceeded its 60 second deadline")),
                },
            };
            let response = harness_providers::assemble_stream(&events).map_err(|error| {
                mcp_callback_error(&format!("sampling response was invalid: {error}"))
            })?;
            if response.finish_reason.is_none()
                || response.incomplete_tool_calls
                || !response.tool_calls.is_empty()
            {
                return Err(mcp_callback_error(
                    "sampling provider did not return a complete text answer",
                ));
            }
            let sampling_message = harness_extensions::rmcp::model::SamplingMessage::new(
                Role::Assistant,
                SamplingMessageContentBlock::text(response.text),
            );
            Ok(harness_extensions::rmcp::model::CreateMessageResult::new(
                sampling_message,
                self.model.clone(),
            )
            .with_stop_reason("endTurn"))
        })
    }

    fn elicit<'a>(
        &'a self,
        request: harness_extensions::rmcp::model::ElicitRequestParams,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        harness_extensions::rmcp::model::ElicitResult,
                        harness_extensions::rmcp::model::ErrorData,
                    >,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            use harness_extensions::rmcp::model::{
                ElicitRequestParams, ElicitResult, ElicitationAction,
            };
            let (message, schema, url) = match request {
                ElicitRequestParams::FormElicitationParams {
                    message,
                    requested_schema,
                    ..
                } => (
                    message,
                    Some(
                        serde_json::to_value(requested_schema)
                            .map_err(|error| mcp_callback_error(&error.to_string()))?,
                    ),
                    None,
                ),
                ElicitRequestParams::UrlElicitationParams { message, url, .. } => {
                    (message, None, Some(url))
                }
                _ => return Err(mcp_callback_error("this elicitation mode is not available")),
            };
            if message.len() > 16 * 1024
                || schema
                    .as_ref()
                    .is_some_and(|value| value.to_string().len() > 64 * 1024)
            {
                return Err(harness_extensions::rmcp::model::ErrorData::invalid_params(
                    "elicitation request exceeds the host size limit",
                    None,
                ));
            }
            let request_id = format!("mcp-{}-{}", self.server, RequestId::generate());
            let (reply, response) = oneshot::channel();
            self.pending
                .lock()
                .map_err(|_| mcp_callback_error("elicitation state is unavailable"))?
                .insert(
                    request_id.clone(),
                    PendingMcpElicitationRequest {
                        schema: schema.clone(),
                        url: url.clone(),
                        reply,
                    },
                );
            if self
                .sender
                .send(SessionEvent::McpElicitationRequired {
                    request_id: request_id.clone(),
                    server: self.server.clone(),
                    message,
                    requested_schema: schema,
                    url,
                })
                .is_err()
            {
                if let Ok(mut pending) = self.pending.lock() {
                    pending.remove(&request_id);
                }
                return Err(mcp_callback_error(
                    "interactive input is no longer available",
                ));
            }
            let answer = tokio::select! {
                () = self.cancellation.cancelled() => None,
                result = response => result.ok(),
            };
            if let Ok(mut pending) = self.pending.lock() {
                pending.remove(&request_id);
            }
            match answer {
                Some(McpElicitationAnswer::Accept(value)) => {
                    Ok(ElicitResult::new(ElicitationAction::Accept).with_content(value))
                }
                Some(McpElicitationAnswer::Decline) => {
                    Ok(ElicitResult::new(ElicitationAction::Decline))
                }
                Some(McpElicitationAnswer::Cancel) | None => {
                    Ok(ElicitResult::new(ElicitationAction::Cancel))
                }
            }
        })
    }
}

fn mcp_callback_error(message: &str) -> harness_extensions::rmcp::model::ErrorData {
    harness_extensions::rmcp::model::ErrorData::internal_error(message.to_owned(), None)
}

fn validate_elicitation_response(
    schema: Option<&serde_json::Value>,
    value: &serde_json::Value,
) -> Result<(), String> {
    let Some(schema) = schema else {
        return Err("the MCP server did not provide an input schema".to_owned());
    };
    let Some(object) = value.as_object() else {
        return Err("the elicitation response must be a JSON object".to_owned());
    };
    let properties = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "the MCP input schema is invalid".to_owned())?;
    if let Some(required) = schema.get("required").and_then(serde_json::Value::as_array) {
        for key in required.iter().filter_map(serde_json::Value::as_str) {
            if !object.contains_key(key) {
                return Err(format!("required elicitation field {key:?} is missing"));
            }
        }
    }
    for (key, input) in object {
        let Some(definition) = properties.get(key) else {
            continue;
        };
        if let Some(allowed) = definition.get("enum").and_then(serde_json::Value::as_array)
            && !allowed.contains(input)
        {
            return Err(format!(
                "elicitation field {key:?} must match one of its listed values"
            ));
        }
        let kind = definition
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let matches = match kind {
            "string" => input.is_string(),
            "number" => input.is_number(),
            "integer" => input.as_i64().is_some() || input.as_u64().is_some(),
            "boolean" => input.is_boolean(),
            _ => false,
        };
        if !matches {
            return Err(format!("elicitation field {key:?} must have type {kind:?}"));
        }
        if let Some(text) = input.as_str()
            && (definition
                .get("minLength")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|limit| {
                    usize::try_from(limit).map_or(true, |limit| text.chars().count() < limit)
                })
                || definition
                    .get("maxLength")
                    .and_then(serde_json::Value::as_u64)
                    .is_some_and(|limit| {
                        usize::try_from(limit).is_ok_and(|limit| text.chars().count() > limit)
                    }))
        {
            return Err(format!(
                "elicitation field {key:?} violates its length limit"
            ));
        }
        if let Some(number) = input.as_f64()
            && (definition
                .get("minimum")
                .and_then(serde_json::Value::as_f64)
                .is_some_and(|limit| number < limit)
                || definition
                    .get("maximum")
                    .and_then(serde_json::Value::as_f64)
                    .is_some_and(|limit| number > limit))
        {
            return Err(format!(
                "elicitation field {key:?} is outside its numeric range"
            ));
        }
    }
    Ok(())
}

#[derive(Clone)]
struct ActiveTurnInbox {
    inbox: RunInbox,
    store: Arc<SqliteStore>,
    session_id: SessionId,
}

/// How long a gated action waits for the user before it expires.
const APPROVAL_TIMEOUT: Duration = DEFAULT_APPROVAL_TIMEOUT;
const SHELL_PREFIX_OUTPUT_LIMIT: usize = 64 * 1024;
const SHELL_PREFIX_OUTPUT_TRUNCATION: &str = "\n[output truncated at 64 KiB]";

/// Upper bound on the resume list, newest first.
const RESUME_LIST_LIMIT: usize = 20;

#[cfg(test)]
mod resume_list_tests {
    use super::conversation_heads;
    use harness_store_sqlite::SessionSummary;
    use harness_types::{SessionId, TaskId};

    fn summary(task: &TaskId, created_at: &str) -> SessionSummary {
        SessionSummary {
            session_id: SessionId::generate(),
            task_id: task.clone(),
            next_sequence: 3,
            input_count: 1,
            latest_snapshot_sequence: None,
            created_at: created_at.to_owned(),
        }
    }

    #[test]
    fn the_resume_list_has_one_row_per_conversation_and_it_is_the_newest_turn() {
        let long = TaskId::generate();
        let short = TaskId::generate();
        // Three turns of one conversation in the same second, then another one.
        let first = summary(&long, "2026-09-24 10:00:00");
        let second = summary(&long, "2026-09-24 10:00:00");
        let third = summary(&long, "2026-09-24 10:00:00");
        let other = summary(&short, "2026-09-24 09:00:00");
        let (heads, turns) =
            conversation_heads(vec![second.clone(), other.clone(), third.clone(), first]);
        let ids = heads
            .iter()
            .map(|head| head.session_id.clone())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec![third.session_id, other.session_id]);
        assert_eq!(turns.get(long.as_str()), Some(&3));
        assert_eq!(turns.get(short.as_str()), Some(&1));
    }
}

/// Newest first. `created_at` has one-second resolution, so turns taken in the same
/// second tie on it; session ids are time-ordered and break the tie the right way.
fn sort_newest_first(sessions: &mut [harness_store_sqlite::SessionSummary]) {
    sessions.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.session_id.as_str().cmp(left.session_id.as_str()))
    });
}

/// One entry per conversation: its newest session, with how many turns it has.
///
/// Every turn is stored as its own session linked to the one before, and the picker
/// used to list each of them. A ten-turn conversation filled half the list with ten
/// rows of the same title, and choosing any row but the newest resumed the
/// conversation from the middle, as if the later turns had never happened. A
/// conversation is a task, so the list keeps the newest session of each task.
fn conversation_heads(
    mut sessions: Vec<harness_store_sqlite::SessionSummary>,
) -> (
    Vec<harness_store_sqlite::SessionSummary>,
    std::collections::HashMap<String, usize>,
) {
    sort_newest_first(&mut sessions);
    let mut turns = std::collections::HashMap::<String, usize>::new();
    for session in &sessions {
        *turns
            .entry(session.task_id.as_str().to_owned())
            .or_default() += 1;
    }
    let mut seen = std::collections::HashSet::new();
    sessions.retain(|session| seen.insert(session.task_id.as_str().to_owned()));
    (sessions, turns)
}

impl AgentSessionService {
    #[must_use]
    #[cfg(test)]
    pub fn new(
        context: &LaunchContext,
        environment: LaunchEnvironment,
        sender: UnboundedSender<SessionEvent>,
    ) -> Self {
        Self::new_with_overrides(context, environment, sender, ConfigOverrides::default())
    }

    #[must_use]
    pub fn new_with_overrides(
        context: &LaunchContext,
        environment: LaunchEnvironment,
        sender: UnboundedSender<SessionEvent>,
        config_overrides: ConfigOverrides,
    ) -> Self {
        let gate = Arc::new(ChannelApprovalGate::new(sender.clone(), APPROVAL_TIMEOUT));
        // The bounds are the environment's, not a constant here: a long task needs a
        // real way to raise them, and `/status` reports what is in force.
        let limits = bounds::limits_from_environment(&environment);
        let model_selection = Arc::new(Mutex::new(TurnModelSelection::with_initial(
            config_overrides.model.clone(),
        )));
        let writer_gate = Arc::new(tokio::sync::Mutex::new(()));
        let auto_refine = !environment
            .value("HA_AUTO_REFINE")
            .and_then(|value| value.to_str())
            .is_some_and(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "off" | "0" | "false" | "no"
                )
            });
        let repl = super::repl::ReplShared::from_environment(
            &environment,
            &context.paths.data_dir,
            &context.project.root,
        );
        Self {
            sender,
            store_dir: context.project_store_dir(),
            data_dir: context.paths.data_dir.clone(),
            config_file: context.paths.config_file.clone(),
            workspace_root: context.project.root.clone(),
            caller_dir: context.caller_dir.clone(),
            global_config_dir: context
                .paths
                .config_file
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf(),
            environment,
            config_overrides,
            model_selection,
            cost_tracker: Arc::new(Mutex::new(CostTracker::default())),
            session_mode: Arc::new(Mutex::new(None)),
            auto_allowed_count: Arc::new(AtomicUsize::new(0)),
            task_id: TaskId::generate(),
            previous_session: Arc::new(Mutex::new(None)),
            active_inbox: Arc::new(Mutex::new(None)),
            gate,
            cancellation: None,
            limits,
            project_id: Arc::new(Mutex::new(None)),
            context_summary: Arc::new(Mutex::new(Vec::new())),
            active_skills: Arc::new(Mutex::new(BTreeMap::new())),
            pending_mcp_elicitations: Arc::new(Mutex::new(HashMap::new())),
            mcp_status: Arc::new(Mutex::new(Vec::new())),
            agents_status: Arc::new(Mutex::new(Vec::new())),
            writer_gate,
            goal: None,
            goal_forgotten: false,
            repl,
            thinking: None,
            turns_since_review: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            heartbeats: Arc::new(super::heartbeat::Heartbeats::default()),
            auto_refine,
        }
    }

    /// Resolve the project identity by opening this project's store read-only.
    ///
    /// Read-only is what keeps `/status` from becoming a second writer: it needs the
    /// identity the first turn registered, not write authority over it.
    fn resolve_project_id(&self) -> Option<String> {
        if let Ok(cache) = self.project_id.lock()
            && let Some(known) = cache.as_ref()
        {
            return Some(known.clone());
        }
        let store_dir = self.store_dir.clone();
        let workspace_root = self.workspace_root.clone();
        let resolved = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()?;
            runtime.block_on(async move {
                let store = SqliteStore::open_read_only(store_dir).await.ok()?;
                let id = crate::interactive::project::resolve_project_id(&store, &workspace_root)
                    .await
                    .ok()?;
                let _ = store.close().await;
                Some(id.as_str().to_owned())
            })
        })
        .join()
        .ok()
        .flatten()?;
        if let Ok(mut cache) = self.project_id.lock() {
            *cache = Some(resolved.clone());
        }
        Some(resolved)
    }

    fn configured(&self) -> Result<ProviderConfig, String> {
        let mut overrides = self.config_overrides.clone();
        if let Ok(selection) = self.model_selection.lock()
            && let Some(model) = &selection.active_model
        {
            overrides.model = Some(model.clone());
        }
        resolve_provider_with_overrides(
            &self.config_file,
            &self.workspace_root,
            &self.environment,
            &self.data_dir,
            &overrides,
        )
    }

    /// Show the user the conversation a resumed session continues.
    ///
    /// Resuming used to print one line and nothing else, so the user could not see
    /// what they were continuing and had no way to tell whether the model could. What
    /// is shown here is read with the same function the runtime uses to build the next
    /// request, so the screen and the model agree on what the conversation was.
    fn show_conversation(&self, source: SessionId) {
        let sender = self.sender.clone();
        let store_dir = self.store_dir.clone();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            let store = match SqliteStore::open_read_only(store_dir).await {
                Ok(store) => store,
                Err(error) => {
                    let _ = sender.send(SessionEvent::RecoverableError {
                        message: format!("the resumed conversation could not be read: {error}"),
                    });
                    return;
                }
            };
            let goal = match store.session_task(&source).await {
                Ok(Some(task)) => store
                    .session_setting(&task, super::goal::GOAL_SETTING)
                    .await
                    .ok()
                    .flatten()
                    .filter(|objective| !objective.trim().is_empty()),
                _ => None,
            };
            match harness_runtime::conversation_history(&store, &source).await {
                Ok(history) => {
                    let _ = sender.send(SessionEvent::ConversationRestored {
                        turns: history.turns(),
                        omitted: history.omitted,
                        summarized: history.summary.is_some(),
                    });
                    if let Some(objective) = goal {
                        let _ = sender.send(SessionEvent::GoalRestored { objective });
                    }
                }
                Err(error) => {
                    let _ = sender.send(SessionEvent::RecoverableError {
                        message: format!("the resumed conversation could not be read: {error}"),
                    });
                }
            }
            let _ = store.close().await;
        });
    }

    fn newest_project_session(&self) -> Result<SessionId, String> {
        let store_dir = self.store_dir.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| format!("session lookup runtime could not start: {error}"))?;
            runtime.block_on(async move {
                let store = SqliteStore::open_read_only(store_dir)
                    .await
                    .map_err(|error| format!("project session store could not open: {error}"))?;
                let mut sessions = store
                    .list_sessions()
                    .await
                    .map_err(|error| format!("project sessions could not be listed: {error}"))?;
                sort_newest_first(&mut sessions);
                let latest = sessions
                    .into_iter()
                    .next()
                    .map(|session| session.session_id)
                    .ok_or_else(|| "this project has no session to continue".to_owned());
                let _ = store.close().await;
                latest
            })
        })
        .join()
        .map_err(|_| "project session lookup thread failed".to_owned())?
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
        self.gate.clear_turn_rules();
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
        let config_file = self.config_file.clone();
        let workspace_root = self.workspace_root.clone();
        let caller_dir = self.caller_dir.clone();
        let global_config_dir = self.global_config_dir.clone();
        let environment = self.environment.clone();
        let config_overrides = self.config_overrides.clone();
        let selected_model = self
            .model_selection
            .lock()
            .ok()
            .and_then(|mut selection| selection.begin_turn());
        let cost_tracker = Arc::clone(&self.cost_tracker);
        let session_mode = self.session_mode.lock().ok().and_then(|mode| *mode);
        let auto_allowed_count = Arc::clone(&self.auto_allowed_count);
        let context_summary = Arc::clone(&self.context_summary);
        let active_skills = Arc::clone(&self.active_skills);
        let pending_mcp_elicitations = Arc::clone(&self.pending_mcp_elicitations);
        let mcp_status = Arc::clone(&self.mcp_status);
        let agents_status = Arc::clone(&self.agents_status);
        let task_id = self.task_id.clone();
        let previous_session = Arc::clone(&self.previous_session);
        let active_inbox = Arc::clone(&self.active_inbox);
        let gate = Arc::clone(&self.gate);
        let limits = self.limits;
        let writer_gate = Arc::clone(&self.writer_gate);
        let goal = match (&self.goal, std::mem::take(&mut self.goal_forgotten)) {
            (Some(objective), _) => GoalRecord::Active(objective.clone()),
            (None, true) => GoalRecord::Forget,
            (None, false) => GoalRecord::Keep,
        };
        let repl = self.repl.clone();
        let thinking = self.thinking;
        let turns_since_review = Arc::clone(&self.turns_since_review);
        let heartbeats = Arc::clone(&self.heartbeats);
        let auto_refine = self.auto_refine;
        // Every user input opens its own session; the conversation is the chain of
        // sessions linked to the same task.
        let session_id = SessionId::generate();
        handle.spawn(async move {
            Box::pin(run_turn(
                sender,
                store_dir,
                data_dir,
                config_file,
                workspace_root,
                caller_dir,
                global_config_dir,
                environment,
                config_overrides,
                selected_model,
                cost_tracker,
                session_mode,
                auto_allowed_count,
                context_summary,
                active_skills,
                pending_mcp_elicitations,
                mcp_status,
                agents_status,
                session_id,
                task_id,
                previous_session,
                active_inbox,
                gate,
                limits,
                request,
                cancellation,
                writer_gate,
                goal,
                repl,
                thinking,
                turns_since_review,
                auto_refine,
                heartbeats,
            ))
            .await;
        });
    }

    fn set_goal(&mut self, objective: Option<String>) {
        self.goal = objective;
    }

    fn forget_goal(&mut self) {
        self.goal = None;
        self.goal_forgotten = true;
    }

    fn answer(&mut self, request_id: &str, decision: ApprovalDecision) -> bool {
        self.gate.answer(request_id, decision)
    }

    fn respond_mcp_elicitation(&mut self, request_id: &str, answer: &str) -> Result<(), String> {
        let (schema, url) = self
            .pending_mcp_elicitations
            .lock()
            .map_err(|_| "MCP elicitation state is unavailable".to_owned())?
            .get(request_id)
            .map(|request| (request.schema.clone(), request.url.clone()))
            .ok_or_else(|| "that MCP request is no longer pending".to_owned())?;
        let response = if answer.trim().eq_ignore_ascii_case("decline") {
            McpElicitationAnswer::Decline
        } else if answer.trim().eq_ignore_ascii_case("cancel") {
            McpElicitationAnswer::Cancel
        } else if url.is_some() {
            if !matches!(
                answer.trim().to_ascii_lowercase().as_str(),
                "done" | "yes" | "accept"
            ) {
                return Err(
                    "visit the displayed URL, then enter done, decline, or cancel".to_owned(),
                );
            }
            McpElicitationAnswer::Accept(serde_json::json!({}))
        } else {
            let value: serde_json::Value = serde_json::from_str(answer).map_err(|error| {
                format!("enter the elicitation response as a JSON object: {error}")
            })?;
            validate_elicitation_response(schema.as_ref(), &value)?;
            McpElicitationAnswer::Accept(value)
        };
        let request = self
            .pending_mcp_elicitations
            .lock()
            .map_err(|_| "MCP elicitation state is unavailable".to_owned())?
            .remove(request_id)
            .ok_or_else(|| "that MCP request was already answered".to_owned())?;
        request
            .reply
            .send(response)
            .map_err(|_| "the MCP request was canceled before it received the answer".to_owned())
    }

    fn grant_run_approval(&mut self) {
        self.gate.grant_for_run();
    }

    fn revoke_run_approval(&mut self) {
        self.gate.clear_grant_for_run();
    }

    fn limits(&self) -> TurnBounds {
        TurnBounds {
            max_steps: self.limits.max_steps,
            max_tool_calls: self.limits.max_tool_calls,
        }
    }

    fn turn_limits(&self) -> TurnLimits {
        self.limits
    }

    fn provider_diagnostics(&self) -> Vec<String> {
        provider_diagnostics_with_config(
            &self.config_file,
            &self.workspace_root,
            &self.environment,
            &self.data_dir,
        )
    }

    fn config_explain(&self) -> Vec<String> {
        let mut overrides = self.config_overrides.clone();
        if let Ok(selection) = self.model_selection.lock()
            && let Some(model) = &selection.active_model
        {
            overrides.model = Some(model.clone());
        }
        super::config::resolve_layers(
            &self.config_file,
            &self.workspace_root,
            &self.environment,
            &overrides,
        )
        .map_or_else(
            |error| vec![format!("configuration unavailable: {error}")],
            |resolved| {
                resolved
                    .explain
                    .into_iter()
                    .map(|entry| {
                        format!(
                            "{} = {} [{}]{}",
                            entry.key,
                            entry.value,
                            entry.layer.as_str(),
                            entry
                                .reason
                                .map_or_else(String::new, |reason| format!(" — {reason}")),
                        )
                    })
                    .collect()
            },
        )
    }

    fn hooks_summary(&self) -> Vec<String> {
        match super::config::resolve_layers(
            &self.config_file,
            &self.workspace_root,
            &self.environment,
            &self.config_overrides,
        ) {
            Ok(config) if config.hooks.is_empty() => {
                vec!["no trusted hooks are configured".to_owned()]
            }
            Ok(config) => config
                .hooks
                .iter()
                .map(|hook| {
                    format!(
                        "{} matcher={} command={} timeout={}s source={}",
                        hook.event,
                        hook.matcher.as_deref().unwrap_or("*"),
                        hook.command,
                        hook.timeout_seconds,
                        hook.source,
                    )
                })
                .collect(),
            Err(error) => vec![format!("hooks unavailable: {error}")],
        }
    }

    fn mcp_summary(&self) -> Vec<String> {
        let mut lines = match super::config::resolve_layers(
            &self.config_file,
            &self.workspace_root,
            &self.environment,
            &self.config_overrides,
        ) {
            Ok(config) if config.mcp_servers.is_empty() => {
                vec!["no MCP servers configured".to_owned()]
            }
            Ok(config) => config
                .mcp_servers
                .iter()
                .map(|(name, server)| {
                    format!(
                        "{name}: configured, lazy start, transport={}, required={}, tool_timeout={}s",
                        server.transport.as_deref().unwrap_or("stdio"),
                        server.required,
                        server.tool_timeout_seconds.unwrap_or(60),
                    )
                })
                .collect(),
            Err(error) => vec![format!("MCP configuration unavailable: {error}")],
        };
        if let Ok(status) = self.mcp_status.lock() {
            lines.extend(status.iter().cloned());
        }
        lines
    }

    fn agents_summary(&self) -> Vec<String> {
        self.agents_status.lock().map_or_else(
            |_| vec!["delegated worker status is unavailable".to_owned()],
            |status| {
                if status.is_empty() {
                    vec!["no delegated workers have run in this session".to_owned()]
                } else {
                    status.clone()
                }
            },
        )
    }

    fn skills_summary(&self) -> Vec<String> {
        let mut lines = match super::skills::discover(
            &self.global_config_dir,
            &self.workspace_root,
            &self.environment,
            super::config::resolve_layers(
                &self.config_file,
                &self.workspace_root,
                &self.environment,
                &self.config_overrides,
            )
            .is_ok_and(|config| config.project_trusted),
        ) {
            Ok(catalog) => catalog
                .entries()
                .iter()
                .map(|entry| {
                    format!(
                        "{}@{} — {} [{}]",
                        entry.name,
                        entry.version,
                        entry.description,
                        entry.source.as_str()
                    )
                })
                .collect::<Vec<_>>(),
            Err(error) => vec![format!("skill catalog unavailable: {error}")],
        };
        if let Ok(active) = self.active_skills.lock() {
            for activation in active.values() {
                lines.push(format!(
                    "active: {} ({})",
                    activation.entry.version_ref(),
                    activation.entry.digest.as_str()
                ));
            }
        }
        if lines.is_empty() {
            lines.push("no trusted skills found".to_owned());
        }
        lines
    }

    fn activate_skill(&mut self, name: &str) -> Result<String, String> {
        let trusted = super::config::resolve_layers(
            &self.config_file,
            &self.workspace_root,
            &self.environment,
            &self.config_overrides,
        )
        .map_err(|error| error.to_string())?
        .project_trusted;
        let catalog = super::skills::discover(
            &self.global_config_dir,
            &self.workspace_root,
            &self.environment,
            trusted,
        )
        .map_err(|error| error.to_string())?;
        let activation =
            super::skills::activate(&catalog, name, 1).map_err(|error| error.to_string())?;
        let label = format!(
            "activated {} ({}) in the Skill context channel",
            activation.entry.version_ref(),
            activation.entry.digest.as_str()
        );
        self.active_skills
            .lock()
            .map_err(|_| "active skills are unavailable".to_owned())?
            .insert(name.to_owned(), activation);
        Ok(label)
    }

    fn reload(&mut self) -> Result<String, String> {
        let trusted = super::config::resolve_layers(
            &self.config_file,
            &self.workspace_root,
            &self.environment,
            &self.config_overrides,
        )
        .map_err(|error| error.to_string())?
        .project_trusted;
        let skills = super::skills::discover(
            &self.global_config_dir,
            &self.workspace_root,
            &self.environment,
            trusted,
        )
        .map_err(|error| error.to_string())?;
        let commands =
            super::skills::commands(&self.global_config_dir, &self.workspace_root, trusted)
                .map_err(|error| error.to_string())?;
        let instructions = super::instructions::load(
            &self.global_config_dir,
            &self.workspace_root,
            &self.caller_dir,
        );
        Ok(format!(
            "reloaded {} skill(s), {} prompt command(s), and {} instruction file(s); active skills remain digest-pinned",
            skills.entries().len(),
            commands.len(),
            instructions.files.len()
        ))
    }

    fn expand_prompt_command(&self, name: &str, arguments: &str) -> Result<Option<String>, String> {
        let trusted = super::config::resolve_layers(
            &self.config_file,
            &self.workspace_root,
            &self.environment,
            &self.config_overrides,
        )
        .map_err(|error| error.to_string())?
        .project_trusted;
        let command =
            super::skills::commands(&self.global_config_dir, &self.workspace_root, trusted)
                .map_err(|error| error.to_string())?
                .into_iter()
                .find(|command| command.name == name);
        Ok(command.map(|command| super::skills::expand(&command, arguments)))
    }

    fn cost_summary(&self) -> String {
        self.cost_tracker
            .lock()
            .map_or_else(|_| "n/a".to_owned(), |tracker| tracker.display())
    }

    fn context_summary(&self) -> Vec<String> {
        self.context_summary.lock().map_or_else(
            |_| vec!["context details are unavailable".to_owned()],
            |lines| {
                if lines.is_empty() {
                    vec!["no context packet has been built in this session yet".to_owned()]
                } else {
                    lines.clone()
                }
            },
        )
    }

    fn git_diff(&mut self) -> Result<(), String> {
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| "the application service needs an async runtime".to_owned())?;
        let sender = self.sender.clone();
        let store_dir = self.store_dir.clone();
        let workspace_root = self.workspace_root.clone();
        let task_id = self.task_id.clone();
        handle.spawn(async move {
            let result = async {
                let store = SqliteStore::open_read_only(store_dir)
                    .await
                    .map_err(|error| error.to_string())?;
                let base = store
                    .session_setting(&task_id, "git_base")
                    .await
                    .map_err(|error| error.to_string())?;
                let lines = if let Some(base) = base {
                    let diff = harness_tools::git_diff_from(&workspace_root, &base)
                        .await
                        .map_err(|error| error.to_string())?;
                    if diff.is_empty() {
                        vec!["no tracked changes since this session started".to_owned()]
                    } else {
                        let mut lines = diff
                            .lines()
                            .take(512)
                            .map(ToOwned::to_owned)
                            .collect::<Vec<_>>();
                        if diff.lines().count() > 512 {
                            lines.push("[diff truncated after 512 lines]".to_owned());
                        }
                        lines
                    }
                } else {
                    vec!["no Git commit was recorded when this session started".to_owned()]
                };
                let _ = store.close().await;
                Ok::<_, String>(lines)
            }
            .await;
            let lines = result.unwrap_or_else(|error| vec![format!("git diff failed: {error}")]);
            let _ = sender.send(SessionEvent::Reference {
                title: "/diff".to_owned(),
                lines,
            });
        });
        Ok(())
    }

    fn rename(&mut self, title: &str) -> Result<String, String> {
        let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
        if title.is_empty() || title.chars().count() > 60 {
            return Err("usage: /rename <name up to 60 characters>".to_owned());
        }
        let task_id = self.task_id.clone();
        let store_dir = self.store_dir.clone();
        let sender = self.sender.clone();
        let title_for_write = title.clone();
        let writer_gate = Arc::clone(&self.writer_gate);
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| "the application service needs an async runtime".to_owned())?;
        handle.spawn(async move {
            let result = async {
                let _writer = writer_gate.lock().await;
                let store =
                    SqliteStore::open_writer(WriterOpenOptions::new(store_dir, HostId::generate()))
                        .await
                        .map_err(|error| error.to_string())?;
                let write = store
                    .set_session_setting(&task_id, "title", &title_for_write)
                    .await
                    .map_err(|error| error.to_string());
                let _ = store.close().await;
                write
            }
            .await;
            let _ = sender.send(SessionEvent::Notice {
                message: result.map_or_else(
                    |error| format!("session title could not be saved: {error}"),
                    |()| format!("session renamed to {title_for_write}"),
                ),
            });
        });
        Ok(format!("saving session title: {title}"))
    }

    fn set_model(&mut self, model: &str) -> Result<String, String> {
        let model = model.trim();
        if model.is_empty() {
            return Err("model name must not be empty".to_owned());
        }
        self.model_selection
            .lock()
            .map_err(|_| "model selection is unavailable".to_owned())?
            .select_for_next_turn(model.to_owned());
        Ok(format!("model {model} selected for the next turn"))
    }

    fn due_heartbeats(&mut self) -> Vec<super::heartbeat::Due> {
        self.heartbeats.due(std::time::Instant::now())
    }

    fn set_thinking(&mut self, level: &str) -> Result<String, String> {
        let requested = harness_providers::ThinkingLevel::parse(level).ok_or_else(|| {
            format!(
                "unknown thinking level {level:?}; choose one of {}",
                harness_providers::ThinkingLevel::ALL
                    .map(harness_providers::ThinkingLevel::as_str)
                    .join(", ")
            )
        })?;
        self.thinking = Some(requested);
        let config = self.configured()?;
        let model =
            harness_providers::thinking::reasoning_model(&config.provider_id, &config.model);
        let used = harness_providers::thinking::clamp(model, requested);
        Ok(if used == requested {
            format!("thinking {requested} selected for the next turn")
        } else {
            format!(
                "thinking {requested} selected for the next turn; {} uses {used}, the nearest level it offers",
                config.model
            )
        })
    }

    fn thinking_status(&self) -> Vec<String> {
        let Ok(config) = self.configured() else {
            return vec!["no provider is configured".to_owned()];
        };
        let model =
            harness_providers::thinking::reasoning_model(&config.provider_id, &config.model);
        let chosen = self
            .thinking
            .or_else(|| harness_providers::ThinkingLevel::parse(&config.thinking))
            .unwrap_or_default();
        let offered = harness_providers::thinking::supported_levels(model)
            .into_iter()
            .map(harness_providers::ThinkingLevel::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        vec![
            format!("Thinking: {chosen} (next turn uses {})", harness_providers::thinking::clamp(model, chosen)),
            format!("Model:    {} offers {offered}", config.model),
            "/thinking <off|minimal|low|medium|high|xhigh|max> changes it; provider.thinking or HA_PROVIDER_THINKING sets the default".to_owned(),
        ]
    }

    fn set_mode(&mut self, mode: &str) -> Result<String, String> {
        let mode = mode
            .parse::<PolicyMode>()
            .map_err(|error| error.to_string())?;
        *self
            .session_mode
            .lock()
            .map_err(|_| "session permission mode is unavailable".to_owned())? = Some(mode);
        Ok(format!(
            "permission mode set to {} for this session",
            mode.as_str()
        ))
    }

    fn permissions_summary(&mut self) -> Vec<String> {
        let mut overrides = self.config_overrides.clone();
        if let Ok(selection) = self.model_selection.lock()
            && let Some(model) = &selection.active_model
        {
            overrides.model = Some(model.clone());
        }
        match super::config::resolve_layers(
            &self.config_file,
            &self.workspace_root,
            &self.environment,
            &overrides,
        ) {
            Ok(config) => {
                let mode = self
                    .session_mode
                    .lock()
                    .ok()
                    .and_then(|mode| *mode)
                    .map_or(config.approval, |mode| mode.as_str().to_owned());
                let mut lines = vec![format!("mode: {mode}")];
                lines.extend(config.explain.iter().filter_map(|entry| {
                    let permission = if entry.key.starts_with("permissions.allow.rule.") {
                        Some("allow")
                    } else if entry.key.starts_with("permissions.deny.rule.") {
                        Some("deny")
                    } else {
                        None
                    }?;
                    Some(format!(
                        "{permission} ({}): {}",
                        entry.layer.as_str(),
                        entry.value
                    ))
                }));
                lines.push(format!(
                    "actions auto-allowed this session: {}",
                    self.auto_allowed_count.load(Ordering::Relaxed)
                ));
                lines
            }
            Err(error) => vec![format!("permissions unavailable: {error}")],
        }
    }

    fn confirm_always_allow(&mut self, request_id: &str, pattern: &str) -> Result<String, String> {
        if !self.gate.is_pending(request_id) {
            return Err("approval is no longer pending; the rule was not saved".to_owned());
        }
        super::permissions::persist_allow_rule(&self.workspace_root, pattern)
            .map_err(|error| error.to_string())?;
        self.gate.stage_always_allow(request_id, pattern)?;
        if !self.gate.answer(request_id, ApprovalDecision::Granted) {
            if let Ok(mut rules) = self.gate.confirmed_rules.lock() {
                rules.remove(request_id);
            }
            return Err(
                "approval expired while saving; the rule was saved but this action was not run"
                    .to_owned(),
            );
        }
        Ok(format!("always-allow rule saved: {pattern}"))
    }

    fn trust_project(&mut self) -> Result<String, String> {
        let canonical = self
            .workspace_root
            .canonicalize()
            .map_err(|error| format!("project root cannot be canonicalized: {error}"))?;
        trust_project_config(&self.config_file, &canonical)?;
        Ok(format!(
            "trusted project config for {}",
            canonical.display()
        ))
    }

    fn provider_problem(&self) -> Option<String> {
        self.configured().err()
    }

    fn project_id(&mut self) -> Option<String> {
        self.resolve_project_id()
    }

    fn save_credential(&mut self, key: &str) -> Result<CredentialSource, String> {
        let path = credentials::resolve_file(&self.environment, &self.data_dir);
        credentials::save(&path, key)
            .map(|protection| CredentialSource::File { path, protection })
            .map_err(|error| format!("the key could not be saved: {error}"))
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
                        let (summaries, turns_per_task) = conversation_heads(summaries);
                        let mut sessions = Vec::new();
                        for summary in summaries.into_iter().take(RESUME_LIST_LIMIT) {
                            let turns = turns_per_task
                                .get(summary.task_id.as_str())
                                .copied()
                                .unwrap_or(1);
                            let title = store
                                .session_setting(&summary.task_id, "title")
                                .await
                                .ok()
                                .flatten()
                                .unwrap_or_else(|| "untitled session".to_owned());
                            let model = store
                                .session_setting(&summary.task_id, "model")
                                .await
                                .ok()
                                .flatten()
                                .unwrap_or_else(|| "model unknown".to_owned());
                            let created = chrono::NaiveDateTime::parse_from_str(
                                &summary.created_at,
                                "%Y-%m-%d %H:%M:%S",
                            )
                            .map_or_else(
                                |_| summary.created_at.clone(),
                                |time| time.format("%Y-%m-%d %H:%M UTC").to_string(),
                            );
                            sessions.push(SessionCandidate {
                                session_id: summary.session_id.as_str().to_owned(),
                                task_id: summary.task_id.as_str().to_owned(),
                                detail: format!("{title} · {model} · {created} · {turns} turn(s)"),
                            });
                        }
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
        let source = match session_id.as_deref() {
            Some("latest") => Some(self.newest_project_session()?),
            Some(value) => Some(
                SessionId::parse(value.to_owned())
                    .map_err(|error| format!("resume needs a canonical session id: {error}"))?,
            ),
            None => None,
        };
        let mut previous = self
            .previous_session
            .lock()
            .map_err(|_| "conversation state is unavailable; nothing was resumed".to_owned())?;
        // Selection is synchronous: submit cannot overtake a background lookup.
        // The turn validates ownership in this project's store before dispatch.
        if source.is_none() {
            self.task_id = TaskId::generate();
        }
        previous.clone_from(&source);
        drop(previous);
        if let Some(source) = source {
            self.show_conversation(source);
        }
        Ok(())
    }

    fn cancel(&mut self) {
        if let Some(token) = self.cancellation.take() {
            token.cancel();
        }
    }

    fn steer(&mut self, text: &str) -> Result<(), String> {
        if text.trim().is_empty() {
            return Err("usage: /steer <text>".to_owned());
        }
        let active = self
            .active_inbox
            .lock()
            .map_err(|_| "the active run inbox is unavailable".to_owned())?
            .clone()
            .ok_or_else(|| "no active run inbox is available".to_owned())?;
        let sender = self.sender.clone();
        let text = text.to_owned();
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| "the application service needs an async runtime".to_owned())?;
        handle.spawn(async move {
            let result = async {
                let deadline = Instant::now() + Duration::from_secs(2);
                let run = loop {
                    match active.store.latest_run(&active.session_id).await {
                        Ok(Some(run)) => break run,
                        Ok(None) if Instant::now() < deadline => {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                        Ok(None) => {
                            return Err("the active run did not open its inbox within two seconds"
                                .to_owned());
                        }
                        Err(error) => return Err(error.to_string()),
                    }
                };
                active
                    .inbox
                    .steer(&run, text, harness_runtime::now_unix_ms())
                    .await
                    .map_err(|error| error.to_string())?;
                Ok::<(), String>(())
            }
            .await;
            if let Err(error) = result {
                let _ = sender.send(SessionEvent::Notice {
                    message: format!("steering note was not queued: {error}"),
                });
            }
        });
        Ok(())
    }
}

fn trust_project_config(user_path: &Path, canonical_root: &Path) -> Result<(), String> {
    if user_path.exists() {
        super::config::load(user_path).map_err(|error| error.to_string())?;
    }
    let mut document = if user_path.exists() {
        let contents = std::fs::read_to_string(user_path)
            .map_err(|error| format!("user config cannot be read: {error}"))?;
        toml::from_str::<toml::Value>(&contents)
            .map_err(|_| "user config is not valid TOML".to_owned())?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };
    let root_table = document
        .as_table_mut()
        .ok_or_else(|| "user config must be a TOML table".to_owned())?;
    root_table.insert("schema_version".to_owned(), toml::Value::Integer(2));
    let trust = root_table
        .entry("trust")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    let trust_table = trust
        .as_table_mut()
        .ok_or_else(|| "trust config must be a TOML table".to_owned())?;
    let projects = trust_table
        .entry("projects")
        .or_insert_with(|| toml::Value::Array(Vec::new()));
    let projects = projects
        .as_array_mut()
        .ok_or_else(|| "trust.projects must be an array".to_owned())?;
    let canonical_text = canonical_root.to_string_lossy().into_owned();
    if !projects.iter().any(|value| {
        value.as_str().is_some_and(|path| {
            Path::new(path)
                .canonicalize()
                .is_ok_and(|existing| same_canonical_path(&existing, canonical_root))
        })
    }) {
        projects.push(toml::Value::String(canonical_text));
    }
    projects.sort_by_key(toml::Value::to_string);
    let v2: harness_types::HarnessConfigV2 = document
        .clone()
        .try_into()
        .map_err(|_| "user config cannot be upgraded to schema v2 safely".to_owned())?;
    v2.validate().map_err(|error| error.to_string())?;
    let rendered = toml::to_string_pretty(&document)
        .map_err(|_| "user config cannot be serialized".to_owned())?;
    if let Some(parent) = user_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("user config directory cannot be created: {error}"))?;
    }
    let mut output = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(user_path)
        .map_err(|error| format!("user config cannot be written: {error}"))?;
    std::io::Write::write_all(&mut output, rendered.as_bytes())
        .and_then(|()| output.sync_all())
        .map_err(|error| format!("user config cannot be flushed: {error}"))
}

fn same_canonical_path(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

/// What a turn does with the task's stored goal.
enum GoalRecord {
    /// A goal is active: carry it and store it.
    Active(String),
    /// The goal was cleared: erase it.
    Forget,
    /// Leave whatever is stored alone (a paused goal stays resumable).
    Keep,
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
    config_file: PathBuf,
    workspace_root: PathBuf,
    caller_dir: PathBuf,
    global_config_dir: PathBuf,
    environment: LaunchEnvironment,
    config_overrides: ConfigOverrides,
    selected_model: Option<String>,
    cost_tracker: Arc<Mutex<CostTracker>>,
    session_mode: Option<PolicyMode>,
    auto_allowed_count: Arc<AtomicUsize>,
    context_summary: Arc<Mutex<Vec<String>>>,
    active_skills: Arc<Mutex<BTreeMap<String, harness_extensions::SkillActivation>>>,
    pending_mcp_elicitations: Arc<Mutex<HashMap<String, PendingMcpElicitationRequest>>>,
    mcp_status: Arc<Mutex<Vec<String>>>,
    agents_status: Arc<Mutex<Vec<String>>>,
    session_id: SessionId,
    task_id: TaskId,
    previous_session: Arc<Mutex<Option<SessionId>>>,
    active_inbox: Arc<Mutex<Option<ActiveTurnInbox>>>,
    gate: Arc<ChannelApprovalGate>,
    limits: TurnLimits,
    request: SubmitRequest,
    cancellation: CancellationToken,
    writer_gate: Arc<tokio::sync::Mutex<()>>,
    goal: GoalRecord,
    repl: Option<Arc<super::repl::ReplShared>>,
    thinking: Option<harness_providers::ThinkingLevel>,
    turns_since_review: Arc<std::sync::atomic::AtomicU32>,
    auto_refine: bool,
    heartbeats: Arc<super::heartbeat::Heartbeats>,
) {
    let send = |event| {
        let _ = sender.send(event);
    };
    send(SessionEvent::Accepted {
        input_id: request.input_id.clone(),
    });

    // `/rename` writes between turns under this gate; a turn waits the moment it takes
    // to finish rather than failing to open the store.
    let _writer_turn = writer_gate.lock().await;
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

    if store
        .session_setting(&task_id, "git_base")
        .await
        .ok()
        .flatten()
        .is_none()
        && let Ok(Some(base)) = harness_tools::git_head_commit(&workspace_root).await
        && let Err(error) = store.set_session_setting(&task_id, "git_base", &base).await
    {
        send(SessionEvent::Notice {
            message: format!("session Git base could not be saved: {error}"),
        });
    }

    if let Some(shell_prefix) = request.shell_prefix.clone() {
        run_shell_prefix_turn(
            sender,
            store,
            config_file,
            workspace_root,
            environment,
            config_overrides,
            session_mode,
            session_id,
            task_id,
            source,
            previous_session,
            gate,
            cost_tracker,
            auto_allowed_count,
            limits,
            request,
            shell_prefix,
            cancellation,
        )
        .await;
        return;
    }

    if request.text == "/undo" || request.text.starts_with("/export ") {
        run_session_file_action(
            sender,
            store,
            data_dir,
            config_file,
            workspace_root,
            environment,
            config_overrides,
            session_mode,
            session_id,
            task_id,
            source,
            previous_session,
            gate,
            cost_tracker,
            auto_allowed_count,
            limits,
            request,
            cancellation,
        )
        .await;
        return;
    }

    if let Some(question_id) = request.answer_question_id.as_deref() {
        let answer_result = async {
            let question_id = QuestionId::parse(question_id.to_owned())
                .map_err(|error| format!("question id is invalid: {error}"))?;
            let service = HumanInputService::new(Arc::clone(&store));
            let question = service
                .question(&question_id)
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "the pending question no longer exists".to_owned())?;
            if question.task_id != task_id || source.as_ref() != Some(&question.session_id) {
                return Err("the pending question belongs to another task or session".to_owned());
            }
            service
                .answer(
                    &question.scope_key,
                    &serde_json::Value::String(request.text.clone()),
                    "interactive.user",
                    harness_runtime::now_unix_ms(),
                )
                .await
                .map_err(|error| error.to_string())?;
            Ok::<(), String>(())
        }
        .await;
        if let Err(message) = answer_result {
            send(SessionEvent::RecoverableError {
                message: format!("question answer was not accepted: {message}"),
            });
            if let Ok(store) = Arc::try_unwrap(store) {
                let _ = store.close().await;
            }
            return;
        }
    }

    if request.compact_guidance.is_none()
        && request.refine.is_none()
        && store
            .session_setting(&task_id, "title")
            .await
            .ok()
            .flatten()
            .is_none()
    {
        let title = request
            .text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(60)
            .collect::<String>();
        if !title.is_empty()
            && let Err(error) = store.set_session_setting(&task_id, "title", &title).await
        {
            send(SessionEvent::Notice {
                message: format!("session title could not be saved: {error}"),
            });
        }
    }

    let persisted_model = if selected_model.is_none() && config_overrides.model.is_none() {
        store
            .session_setting(&task_id, "model")
            .await
            .ok()
            .flatten()
    } else {
        None
    };
    let selected_model = selected_model
        .or_else(|| config_overrides.model.clone())
        .or(persisted_model);
    // The goal lives with the task, so `/resume` can bring it back.
    let stored_goal = match &goal {
        GoalRecord::Active(objective) => Some(objective.as_str()),
        GoalRecord::Forget => Some(""),
        GoalRecord::Keep => None,
    };
    if let Some(value) = stored_goal
        && let Err(error) = store
            .set_session_setting(&task_id, super::goal::GOAL_SETTING, value)
            .await
    {
        send(SessionEvent::Notice {
            message: format!("goal could not be saved with the session: {error}"),
        });
    }
    let goal_task = task_id.clone();
    let mut turn_overrides = config_overrides;
    if let Some(model) = &selected_model {
        turn_overrides.model = Some(model.clone());
        if let Err(error) = store.set_session_setting(&task_id, "model", model).await {
            send(SessionEvent::RecoverableError {
                message: format!(
                    "model selection could not be persisted; nothing was sent: {error}"
                ),
            });
            if let Ok(store) = Arc::try_unwrap(store) {
                let _ = store.close().await;
            }
            return;
        }
    }
    let config = match resolve_provider_with_overrides(
        &config_file,
        &workspace_root,
        &environment,
        &data_dir,
        &turn_overrides,
    ) {
        Ok(config) => config,
        Err(message) => {
            send(SessionEvent::RecoverableError { message });
            if let Ok(store) = Arc::try_unwrap(store) {
                let _ = store.close().await;
            }
            return;
        }
    };
    gate.set_bell(config.bell);

    // A workspace root keeps one project identity, whether or not memory is on: a
    // generated id per turn would put every project-scoped record this turn writes
    // out of reach of the next one.
    if let Some(message) = &config.context_window_notice {
        send(SessionEvent::Notice {
            message: message.clone(),
        });
    }
    let project_id = match project::resolve_project_id(&store, &workspace_root).await {
        Ok(project_id) => project_id,
        Err(error) => {
            send(SessionEvent::RecoverableError {
                message: format!("project identity is unavailable: {error}"),
            });
            return;
        }
    };
    // What the model reads, for `model.info`: the provider's documented claim, as
    // `pi-ai`'s model table carries `input`; a model with no claim reads text, as a
    // `pi-ai` custom model does.
    let capabilities_images = match config.protocol.as_str() {
        "anthropic_messages" => true,
        _ => {
            config.provider_id == "deepseek"
                && harness_providers::CapabilityMatrix::deepseek_documented(config.model.clone())
                    .images
                    == harness_providers::CapabilityClaim::Supported
        }
    };
    let capabilities = ModelCapabilities {
        provider_id: config.provider_id.clone(),
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
    // The thinking level: what `/thinking` chose (kept with the task), else what the
    // task last used, else the configuration's.
    let thinking_level = match thinking {
        Some(level) => {
            let _ = store
                .set_session_setting(&task_id, "thinking", level.as_str())
                .await;
            level
        }
        None => store
            .session_setting(&task_id, "thinking")
            .await
            .ok()
            .flatten()
            .and_then(|value| harness_providers::ThinkingLevel::parse(&value))
            .or_else(|| harness_providers::ThinkingLevel::parse(&config.thinking))
            .unwrap_or_default(),
    };
    let provider: Result<Arc<dyn ModelProvider>, ProviderError> = match config.protocol.as_str() {
        "openai_chat" => OpenAiChatAdapter::with_options(
            config.endpoint.clone(),
            credentials,
            capabilities,
            OpenAiChatOptions {
                thinking: Some(harness_providers::Thinking {
                    level: thinking_level,
                    format: harness_providers::thinking::chat_format(
                        &config.provider_id,
                        &config.endpoint,
                    ),
                }),
            },
        )
        .map(|adapter| Arc::new(adapter) as Arc<dyn ModelProvider>),
        "anthropic_messages" => {
            AnthropicMessagesAdapter::new(config.endpoint.clone(), credentials, capabilities).map(
                |adapter| {
                    Arc::new(adapter.with_thinking(Some(harness_providers::Thinking {
                        level: thinking_level,
                        format: harness_providers::ThinkingFormat::Anthropic,
                    }))) as Arc<dyn ModelProvider>
                },
            )
        }
        _ => Err(ProviderError::new(
            ErrorCode::ProviderProtocol,
            "provider protocol is unsupported",
        )),
    };
    let provider = match provider {
        Ok(provider) => provider,
        Err(error) => {
            send(SessionEvent::RecoverableError {
                message: format!("provider configuration is invalid: {error}"),
            });
            if let Ok(store) = Arc::try_unwrap(store) {
                let _ = store.close().await;
            }
            return;
        }
    };

    let observation = match observe_workspace(project_id, &workspace_root) {
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

    let callback_provider = Arc::clone(&provider);
    let callback_sender = sender.clone();
    let callback_pending = Arc::clone(&pending_mcp_elicitations);
    let callback_cancellation = cancellation.clone();
    let callback_model = config.model.clone();
    let mcp_callback_factory: super::mcp::McpCallbackFactory = Arc::new(move |server| {
        Arc::new(InteractiveMcpCallbacks {
            server: server.to_owned(),
            provider: Arc::clone(&callback_provider),
            model: callback_model.clone(),
            sender: callback_sender.clone(),
            pending: Arc::clone(&callback_pending),
            cancellation: callback_cancellation.clone(),
        })
    });

    let runtime = Arc::new(RuntimeService::new(
        Arc::clone(&store),
        Arc::clone(&provider),
        RuntimeConfig {
            context_window_tokens: config.context_window_tokens,
            output_reservation_tokens: config.output_reservation_tokens,
            compaction_reserve_tokens: config.compaction_reserve_tokens,
            max_retry_after_seconds: config.max_retry_after_seconds,
            ..RuntimeConfig::default()
        },
    ));
    if let Some(guidance) = request.compact_guidance.as_deref() {
        let result = match source.as_ref() {
            Some(source_session) => runtime
                .compact_with_guidance(source_session, Some(guidance))
                .await
                .map(|compacted| {
                    send(SessionEvent::Notice {
                        message: format!(
                            "compacted through event {}; summary source: {}",
                            compacted.covered_through, compacted.summary_source
                        ),
                    });
                })
                .map_err(|error| format!("compaction failed: {error}")),
            None => Err("there is no earlier session to compact".to_owned()),
        };
        drop(runtime);
        if let Ok(store) = Arc::try_unwrap(store) {
            let _ = store.close().await;
        }
        match result {
            Ok(()) => send(SessionEvent::RunTerminal {
                outcome: RunOutcome::Done,
            }),
            Err(message) => {
                send(SessionEvent::RecoverableError { message });
                send(SessionEvent::RunTerminal {
                    outcome: RunOutcome::Failed("compaction failed".to_owned()),
                });
            }
        }
        return;
    }
    // `/refine` reads the conversation so far and edits the harness state; it is not
    // a model turn, like compaction above.
    if let Some(options) = request.refine.clone() {
        let scopes = super::refine::HarnessScopes {
            global: super::harness::global_dir(&data_dir),
            local: super::harness::local_dir(&data_dir, task_id.as_str()),
        };
        let conversation = match source.as_ref() {
            Some(source_session) => harness_runtime::conversation_history(&store, source_session)
                .await
                .map(|history| super::refine::serialize_turns(&history.turns()))
                .unwrap_or_default(),
            None => String::new(),
        };
        let result =
            super::refine::refine(&provider, &config.model, &conversation, &scopes, &options).await;
        drop(runtime);
        if let Ok(store) = Arc::try_unwrap(store) {
            let _ = store.close().await;
        }
        match result {
            Ok(refinement) => {
                send(SessionEvent::Notice {
                    message: format!("refine {}: {}", refinement.id(), refinement.notice()),
                });
                send(SessionEvent::RunTerminal {
                    outcome: RunOutcome::Done,
                });
            }
            Err(message) => {
                send(SessionEvent::RecoverableError {
                    message: format!("refine failed: {message}"),
                });
                send(SessionEvent::RunTerminal {
                    outcome: RunOutcome::Failed("refine failed".to_owned()),
                });
            }
        }
        return;
    }
    // Local extensions are explicit opt-in, loaded for this turn and stopped when it
    // ends: a chat turn never leaves an extension process behind.
    let extension_root = extensions::extensions_root(&environment, &data_dir);
    let active_extensions = if extensions::extensions_requested_from_environment(&environment) {
        match extensions::load_active(&extension_root).await {
            Ok(active) => {
                send(SessionEvent::Notice {
                    message: active.report().message(&extension_root),
                });
                Some(active)
            }
            Err(error) => {
                send(SessionEvent::Notice {
                    message: format!("extensions: not loaded ({error})"),
                });
                None
            }
        }
    } else {
        None
    };
    // MCP launch is deferred until an actual model turn. Each server config is
    // trust-layer resolved above, and every tool it advertises still crosses
    // ToolExecutionService's policy, approval, intent and receipt path.
    let active_mcp = if config.mcp_servers.is_empty() {
        if let Ok(mut status) = mcp_status.lock() {
            status.clear();
        }
        None
    } else {
        let notice_sender = sender.clone();
        let mcp_notice: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |message| {
            let _ = notice_sender.send(SessionEvent::Notice { message });
        });
        match super::mcp::ActiveMcp::connect(
            config.mcp_servers.clone(),
            &workspace_root,
            mcp_callback_factory,
            mcp_notice,
        )
        .await
        {
            Ok(active) => {
                let summary = active.summary();
                if let Ok(mut status) = mcp_status.lock() {
                    status.clone_from(&summary);
                }
                for line in summary {
                    send(SessionEvent::Notice {
                        message: format!("MCP: {line}"),
                    });
                }
                Some(active)
            }
            Err(error) => {
                send(SessionEvent::RecoverableError {
                    message: format!("MCP setup failed: {error}"),
                });
                send(SessionEvent::RunTerminal {
                    outcome: RunOutcome::Failed("MCP setup failed".to_owned()),
                });
                if let Ok(store) = Arc::try_unwrap(store) {
                    let _ = store.close().await;
                }
                return;
            }
        }
    };
    let tool_policy = match super::permissions::build_tool_policy(
        &config.approval,
        &config.allow_rules,
        &config.deny_rules,
        session_mode,
    ) {
        Ok(policy) => policy.with_turn_rules(gate.turn_rules()),
        Err(error) => {
            send(SessionEvent::RecoverableError {
                message: format!("tool permissions are invalid: {error}"),
            });
            if let Some(active) = active_mcp {
                let _ = active.shutdown().await;
            }
            if let Ok(store) = Arc::try_unwrap(store) {
                let _ = store.close().await;
            }
            return;
        }
    };
    let delegate_host = match super::delegation::DelegateHost::new(
        &store,
        Arc::clone(&provider),
        RuntimeConfig {
            context_window_tokens: config.context_window_tokens,
            output_reservation_tokens: config.output_reservation_tokens,
            compaction_reserve_tokens: config.compaction_reserve_tokens,
            max_retry_after_seconds: config.max_retry_after_seconds,
            ..RuntimeConfig::default()
        },
        workspace_root.clone(),
        observation.clone(),
        data_dir.join("delegation"),
        config.hooks.clone(),
        config.deny_rules.clone(),
        config.model_price,
        Arc::clone(&gate) as Arc<dyn ApprovalGate>,
        sender.clone(),
        cancellation.clone(),
        Arc::clone(&agents_status),
    ) {
        Ok(host) => Some(host),
        Err(error) => {
            send(SessionEvent::Notice {
                message: format!("delegation unavailable: {error}"),
            });
            None
        }
    };
    let skill_catalog = match super::skills::discover(
        &global_config_dir,
        &workspace_root,
        &environment,
        config.project_trusted,
    ) {
        Ok(catalog) => Some(catalog),
        Err(error) => {
            send(SessionEvent::Notice {
                message: format!("skills: catalog unavailable ({error})"),
            });
            None
        }
    };
    let skill_host = skill_catalog
        .as_ref()
        .map(|catalog| super::skills::SkillHost::new(catalog.clone(), Arc::clone(&active_skills)));
    let web_host = super::web::WebHost::from_environment(&environment);
    let goal_host =
        matches!(goal, GoalRecord::Active(_)).then(|| super::goal::GoalHost::new(sender.clone()));
    let kernel_skills = skill_catalog
        .as_ref()
        .map(|catalog| {
            catalog
                .entries()
                .iter()
                .filter(|entry| entry.model_invocable)
                .flat_map(|entry| super::repl::python_skill_packages(&entry.path))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let kernel_skill_imports = kernel_skills
        .iter()
        .map(|skill| skill.import_name.clone())
        .collect::<Vec<_>>();
    // What the `refine` skill asks for, run when the turn ends.
    let refine_request: Arc<Mutex<Option<super::refine::RefineOptions>>> =
        Arc::new(Mutex::new(None));
    let repl_host = match &repl {
        Some(shared) => {
            // `rlm.spawn` and its family run on this turn's delegated workers; the
            // other skills' requests are answered from the session's state.
            let skills: Arc<dyn super::repl::HostRequests> =
                Arc::new(super::skill_requests::SkillRequests::new(
                    Arc::clone(&refine_request),
                    sender.clone(),
                    super::skill_requests::ModelInfo {
                        id: config.model.clone(),
                        provider: config.provider_id.clone(),
                        images: capabilities_images,
                    },
                    config.context_window_tokens,
                    match &goal {
                        GoalRecord::Active(objective) => Some(objective.clone()),
                        _ => None,
                    },
                    goal_host.clone(),
                ));
            let mut chain: Vec<Arc<dyn super::repl::HostRequests>> = vec![
                skills,
                Arc::clone(&heartbeats) as Arc<dyn super::repl::HostRequests>,
            ];
            if let Some(host) = &delegate_host {
                chain.push(host.rlm_requests(config.model.clone()));
            }
            let requests: Arc<dyn super::repl::HostRequests> =
                Arc::new(super::skill_requests::ChainedRequests(chain));
            super::repl::ReplHost::for_turn(
                shared,
                requests,
                super::repl::KernelContext {
                    global: super::harness::global_dir(&data_dir),
                    local: super::harness::local_dir(&data_dir, task_id.as_str()),
                    skills: kernel_skills.clone(),
                },
            )
            .await
        }
        _ => None,
    };
    let mut tools = ToolExecutionService::new(Arc::clone(&store))
        .with_policy(tool_policy)
        .with_hooks(config.hooks.clone());
    if let Some(dispatcher) = super::mcp::combined_dispatcher_with_delegate(
        active_mcp.as_ref(),
        active_extensions.as_ref(),
        delegate_host.as_ref(),
        skill_host.as_ref(),
        web_host.as_ref(),
        goal_host.as_ref(),
        repl_host.as_ref(),
    ) {
        tools = tools.with_external(dispatcher);
    }
    let driver = TurnDriver::new(Arc::clone(&runtime), tools);
    let external_tools = super::mcp::combined_tools_with_delegate(
        active_mcp.as_ref(),
        active_extensions.as_ref(),
        delegate_host.as_ref(),
        skill_host.as_ref(),
        web_host.as_ref(),
        goal_host.as_ref(),
        repl_host.as_ref(),
    );
    let driver = match &external_tools {
        Some(tools) => driver.with_external(tools.clone()),
        None => driver,
    };
    let mut tool_schemas = coding_tool_schemas();
    if let Some(tools) = &external_tools {
        tool_schemas.extend(tools.schemas());
    }
    // Content the message names rides with it: images as blocks the model is shown, files
    // as text the model reads. A candidate that cannot be attached is said out loud: a
    // reader who is not told why cannot tell it from a bug.
    let mut attached = attachments::from_message(&request.text, &workspace_root);
    if let Some(mcp) = &active_mcp {
        let (mut resource_files, notes) = mcp.attach_mentions(&request.text).await;
        attached.files.append(&mut resource_files);
        attached.notes.extend(notes);
    }
    for note in &attached.notes {
        send(SessionEvent::Notice {
            message: format!("not attached ({note})"),
        });
    }
    // The file text becomes part of the message itself, so what runs is what the user
    // handed over: the API has no file block, and text is the only shape a file travels in.
    let prompt = format!(
        "{}{}",
        request.text,
        attachments::attachment_blocks(&attached.files)
    );
    let loaded_instructions =
        super::instructions::load(&global_config_dir, &workspace_root, &caller_dir);
    for notice in &loaded_instructions.notices {
        send(SessionEvent::Notice {
            message: notice.clone(),
        });
    }
    send(SessionEvent::Notice {
        message: format!("AGENTS.md: {} files", loaded_instructions.files.len()),
    });
    let (git_branch, changed_files) = prompt_git_facts(&workspace_root);
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let shell = if cfg!(windows) {
        "PowerShell".to_owned()
    } else {
        std::env::var("SHELL")
            .ok()
            .and_then(|value| {
                Path::new(&value)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "sh".to_owned())
    };
    let prompt_environment = PromptEnvironment {
        os: std::env::consts::OS,
        shell: &shell,
        cwd: &caller_dir,
        project_root: &workspace_root,
        git_branch: git_branch.as_deref(),
        changed_files,
        date_iso: &today,
        limits,
    };
    // Every advertised tool, core and external: the prompt shows a block only when
    // its tool is active, the way prime-agent's does.
    let prompt_tools = tool_schemas
        .iter()
        .filter_map(|schema| {
            schema
                .pointer("/function/name")
                .and_then(|name| name.as_str())
        })
        .collect::<Vec<_>>();
    let mut built_prompt = SystemPromptBuilder::build(&prompt_environment, &prompt_tools);
    if repl_host.is_some()
        && let Some(block) = super::prompt::python_skills_block(&kernel_skill_imports)
    {
        built_prompt.text.push_str("\n\n");
        built_prompt.text.push_str(&block);
    }
    if let Some(catalog) = &skill_catalog {
        built_prompt.text =
            super::prompt::append_skill_metadata(built_prompt.text, catalog.entries());
    }
    let mut project_blocks = loaded_instructions.blocks;
    if let Ok(active) = active_skills.lock() {
        project_blocks.extend(
            active
                .values()
                .map(harness_extensions::SkillActivation::block),
        );
    }
    if let GoalRecord::Active(objective) = &goal {
        project_blocks.push(super::goal::goal_block(objective));
    }
    // Memory is prime-agent's continual harness state: the model keeps it through
    // `rlm.harness`, and every turn carries a digest of it ranked for this task.
    let harness_state = super::harness::merge(
        super::harness::load(&super::harness::global_dir(&data_dir), "global"),
        super::harness::load(
            &super::harness::local_dir(&data_dir, task_id.as_str()),
            "local",
        ),
    );
    let goal_objective = match &goal {
        GoalRecord::Active(objective) => Some(objective.as_str()),
        _ => None,
    };
    project_blocks.push(super::harness::digest_block(
        &super::harness::format_digest(
            &harness_state,
            super::harness::DigestOptions {
                repl: repl_host.is_some(),
                refine: repl_host.is_some()
                    && kernel_skill_imports.iter().any(|name| name == "refine"),
            },
            &super::harness::weighted_terms(goal_objective, &[request.text.as_str()]),
        ),
    ));
    let mut run_request = RunRequest::new(
        session_id.clone(),
        task_id,
        request.input_id.clone(),
        prompt,
        observation,
    )
    .with_system_policy(built_prompt.text)
    .with_project_rules(project_blocks)
    .with_tool_schemas(tool_schemas);
    if !attached.is_empty() {
        for notice in attachments::attachment_notices(&attached.images, &attached.files) {
            send(SessionEvent::Notice { message: notice });
        }
        run_request = run_request.with_images(
            attached
                .images
                .into_iter()
                .map(|image| image.attachment)
                .collect(),
        );
    }
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
        cost_tracker,
        model_price: config.model_price,
        auto_allowed_count,
        bell: config.bell,
        tool_started: Mutex::new(None),
    });

    let run_inbox = RunInbox::new(Arc::clone(&store));
    if let Ok(mut active) = active_inbox.lock() {
        *active = Some(ActiveTurnInbox {
            inbox: run_inbox.clone(),
            store: Arc::clone(&store),
            session_id: session_id.clone(),
        });
    }
    let driver = driver.with_inbox(run_inbox);

    // A follow-up turn continues the previous session; the first turn starts one.
    let outcome = match &source {
        Some(source) => {
            driver
                .run_turn_continuing(source, run_request, options, observer, cancellation.clone())
                .await
        }
        None => {
            driver
                .run_turn(run_request, options, observer, cancellation.clone())
                .await
        }
    };

    if let Some(delegate) = &delegate_host {
        let unknown = delegate.shutdown().await;
        for item in unknown {
            send(SessionEvent::Notice {
                message: format!("delegated worker ended without a settled outcome: {item}"),
            });
        }
    }

    if let Some(built) = runtime.context_result(&session_id) {
        let mut lines = vec![
            format!("model: {}", built.manifest.model_id),
            format!("source revision: {}", built.manifest.source_revision),
            format!(
                "tokens: mandatory {}, optional {}",
                built.mandatory_tokens, built.optional_tokens
            ),
        ];
        lines.extend(built.block_usage.iter().map(|block| {
            format!(
                "{} [{}] {} tokens — {}{}",
                block.block_id,
                block.channel.as_str(),
                block.token_estimate,
                if block.included {
                    "included"
                } else {
                    "omitted"
                },
                block
                    .drop_reason
                    .as_ref()
                    .map_or_else(String::new, |reason| format!(" ({reason})"))
            )
        }));
        if let Ok(mut current) = context_summary.lock() {
            *current = lines;
        }
    }

    if let Ok(turn) = &outcome
        && let Some(question_id) = turn.pending_question.as_ref()
    {
        match HumanInputService::new(Arc::clone(&store))
            .question(question_id)
            .await
        {
            Ok(Some(question)) => send(SessionEvent::QuestionRequired {
                question_id: question.question_id.as_str().to_owned(),
                prompt: question.prompt,
                options: question.payload["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect(),
            }),
            Ok(None) | Err(_) => send(SessionEvent::RecoverableError {
                message: "the model asked a question, but the durable question could not be read"
                    .to_owned(),
            }),
        }
    }

    if let Ok(mut active) = active_inbox.lock() {
        *active = None;
    }

    // Release the writer before announcing the terminal event: the next turn takes
    // a newer generation of the task lease, and it must not race this one.
    drop(driver);
    // prime-agent refines at the turn boundary: what the `refine` skill asked for, and
    // every twenty-five turns an automatic review that refines when the trajectory
    // holds something worth keeping.
    if outcome.is_ok() {
        let requested = refine_request
            .lock()
            .ok()
            .and_then(|mut pending| pending.take());
        let turns = turns_since_review.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let review_due = auto_refine && turns >= super::refine::AUTO_REFINE_TURN_INTERVAL;
        if requested.is_some() || review_due {
            let scopes = super::refine::HarnessScopes {
                global: super::harness::global_dir(&data_dir),
                local: super::harness::local_dir(&data_dir, goal_task.as_str()),
            };
            let conversation = harness_runtime::conversation_history(&store, &session_id)
                .await
                .map(|history| super::refine::serialize_turns(&history.turns()))
                .unwrap_or_default();
            let options = if let Some(options) = requested {
                Some(options)
            } else {
                turns_since_review.store(0, std::sync::atomic::Ordering::SeqCst);
                match super::refine::review(&provider, &config.model, &conversation, &scopes, turns)
                    .await
                {
                    Ok(review) if review.should_refine => Some(super::refine::RefineOptions {
                        instructions: review.instructions,
                        global: false,
                        rollback: None,
                    }),
                    Ok(review) => {
                        send(SessionEvent::Notice {
                            message: format!(
                                "auto-refine: nothing to refine ({})",
                                review.rationale
                            ),
                        });
                        None
                    }
                    Err(error) => {
                        send(SessionEvent::Notice {
                            message: format!("auto-refine review skipped: {error}"),
                        });
                        None
                    }
                }
            };
            if let Some(options) = options {
                match super::refine::refine(
                    &provider,
                    &config.model,
                    &conversation,
                    &scopes,
                    &options,
                )
                .await
                {
                    Ok(refinement) => send(SessionEvent::Notice {
                        message: format!("refine {}: {}", refinement.id(), refinement.notice()),
                    }),
                    Err(error) => send(SessionEvent::Notice {
                        message: format!("refine failed: {error}"),
                    }),
                }
            }
        }
    }
    // A finished goal is not brought back by `/resume`.
    if goal_host
        .as_ref()
        .is_some_and(super::goal::GoalHost::completed)
    {
        let _ = store
            .set_session_setting(&goal_task, super::goal::GOAL_SETTING, "")
            .await;
    }
    // Stop the extension processes this turn started, whatever the outcome was.
    if let Some(active) = active_extensions {
        active.shutdown().await;
    }
    if let Some(active) = active_mcp {
        let reports = active.shutdown().await;
        for report in reports.into_iter().filter(|report| !report.drained) {
            send(SessionEvent::Notice {
                message: format!(
                    "MCP {} unloaded after its {} in-flight call(s) reached the cancel grace",
                    report.label, report.inflight
                ),
            });
        }
    }
    drop(runtime);
    if let Ok(store) = Arc::try_unwrap(store) {
        let _ = store.close().await;
    }

    if config.bell {
        send(SessionEvent::Bell);
    }

    let terminal = match outcome {
        Ok(outcome) => {
            if let Ok(mut guard) = previous_session.lock() {
                *guard = Some(outcome.session_id.clone());
            }
            match outcome.stop {
                TurnStop::Final => RunOutcome::Done,
                TurnStop::Canceled => RunOutcome::Canceled,
                // A bound is not a break: the turn stopped where the user set the limit,
                // the work is durable, and `continue` picks it up. Saying `failed` told
                // the user eight tool calls had been lost when none were. The reason is
                // typed, because the host continues a count of work by itself and treats
                // the deadline as a real stop.
                TurnStop::StepLimit => RunOutcome::Paused(PauseReason::StepLimit),
                TurnStop::ToolLimit => RunOutcome::Paused(PauseReason::ToolLimit),
                TurnStop::Deadline => RunOutcome::Paused(PauseReason::Deadline),
                // A goal that ran out of progress, continuations or budget is a
                // decision for the user, not something the app repeats by itself.
                TurnStop::NoProgress => RunOutcome::Paused(PauseReason::NoProgress),
                TurnStop::GoalLimit => RunOutcome::Paused(PauseReason::GoalLimit),
                TurnStop::BudgetExhausted => RunOutcome::Paused(PauseReason::BudgetExhausted),
                TurnStop::NeedsInput => RunOutcome::WaitingInput {
                    question_id: outcome.pending_question.as_ref().map(ToString::to_string),
                },
                TurnStop::ExternalWait => RunOutcome::ExternalWait,
                TurnStop::LoopDetected => {
                    RunOutcome::Blocked("the same tool call repeated".to_owned())
                }
                TurnStop::Unverified => {
                    RunOutcome::Blocked("the final answer could not be verified".to_owned())
                }
            }
        }
        Err(error) => RunOutcome::Failed(error.to_string()),
    };
    send(SessionEvent::RunTerminal { outcome: terminal });
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn run_session_file_action(
    sender: UnboundedSender<SessionEvent>,
    store: Arc<SqliteStore>,
    data_dir: PathBuf,
    config_file: PathBuf,
    workspace_root: PathBuf,
    environment: LaunchEnvironment,
    config_overrides: ConfigOverrides,
    session_mode: Option<PolicyMode>,
    session_id: SessionId,
    task_id: TaskId,
    source: Option<SessionId>,
    previous_session: Arc<Mutex<Option<SessionId>>>,
    gate: Arc<ChannelApprovalGate>,
    cost_tracker: Arc<Mutex<CostTracker>>,
    auto_allowed_count: Arc<AtomicUsize>,
    limits: TurnLimits,
    request: SubmitRequest,
    cancellation: CancellationToken,
) {
    let result = run_session_file_action_inner(
        &sender,
        Arc::clone(&store),
        data_dir,
        config_file,
        workspace_root,
        environment,
        config_overrides,
        session_mode,
        session_id,
        task_id,
        source,
        previous_session,
        gate,
        cost_tracker,
        auto_allowed_count,
        limits,
        request,
        cancellation,
    )
    .await;
    if let Ok(store) = Arc::try_unwrap(store) {
        let _ = store.close().await;
    }
    match result {
        Ok(outcome) => {
            let _ = sender.send(SessionEvent::RunTerminal { outcome });
        }
        Err(message) => {
            let _ = sender.send(SessionEvent::RecoverableError { message });
            let _ = sender.send(SessionEvent::RunTerminal {
                outcome: RunOutcome::Failed("session file action failed".to_owned()),
            });
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn run_session_file_action_inner(
    sender: &UnboundedSender<SessionEvent>,
    store: Arc<SqliteStore>,
    data_dir: PathBuf,
    config_file: PathBuf,
    workspace_root: PathBuf,
    environment: LaunchEnvironment,
    config_overrides: ConfigOverrides,
    session_mode: Option<PolicyMode>,
    session_id: SessionId,
    task_id: TaskId,
    source: Option<SessionId>,
    previous_session: Arc<Mutex<Option<SessionId>>>,
    gate: Arc<ChannelApprovalGate>,
    cost_tracker: Arc<Mutex<CostTracker>>,
    auto_allowed_count: Arc<AtomicUsize>,
    limits: TurnLimits,
    request: SubmitRequest,
    cancellation: CancellationToken,
) -> Result<RunOutcome, String> {
    let config = super::config::resolve_layers(
        &config_file,
        &workspace_root,
        &environment,
        &config_overrides,
    )
    .map_err(|error| error.to_string())?;
    gate.set_bell(config.bell);
    let project_id = project::resolve_project_id(&store, &workspace_root)
        .await
        .map_err(|error| error.to_string())?;
    let observation = observe_workspace(project_id.clone(), &workspace_root)
        .map_err(|error| error.to_string())?;
    let expected_sequence = store
        .session_summary(&session_id)
        .await
        .map_err(|error| error.to_string())?
        .map_or(1, |summary| summary.next_sequence);
    SessionService::new(Arc::clone(&store))
        .admit_input(AdmitInputRequest {
            session_id: session_id.clone(),
            task_id: task_id.clone(),
            input_id: request.input_id.clone(),
            expected_sequence,
            authority: SourceAuthority::User,
            raw_text: request.text.clone(),
            workspace: observation,
            initial_plan_items: Vec::new(),
        })
        .await
        .map_err(|error| format!("session action was not admitted: {error}"))?;
    if let Some(source) = &source {
        store
            .record_continuation_link(source, &session_id, &task_id)
            .await
            .map_err(|error| format!("session action link could not be recorded: {error}"))?;
    }
    if let Ok(mut previous) = previous_session.lock() {
        *previous = Some(session_id.clone());
    }

    let is_undo = request.text == "/undo";
    let action = if is_undo {
        latest_undo_action(&store, &task_id, &project_id, &workspace_root).await?
    } else {
        let path = request
            .text
            .strip_prefix("/export ")
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .ok_or_else(|| "usage: /export [path.md|path.jsonl]".to_owned())?;
        let extension = Path::new(path).extension();
        let jsonl = extension.is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"));
        let markdown = extension.is_some_and(|extension| extension.eq_ignore_ascii_case("md"));
        if !jsonl && !markdown {
            return Err("usage: /export [path.md|path.jsonl]".to_owned());
        }
        if Path::new(path).is_absolute()
            || Path::new(path)
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err("export path must stay inside the workspace".to_owned());
        }
        let content = render_session_export(
            &store,
            &task_id,
            &environment,
            &data_dir,
            &config.provider.api_key_env,
            jsonl,
        )
        .await?;
        let expected_hash = if workspace_root.join(path).exists() {
            Some(observed_file_hash(&workspace_root, path).map_err(|error| error.to_string())?)
        } else {
            None
        };
        CodingToolAction::WriteFile {
            path: path.to_owned(),
            content,
            expected_hash,
        }
    };
    let policy = if is_undo {
        super::permissions::build_tool_policy("ask", &[], &config.deny_rules, Some(PolicyMode::Ask))
    } else {
        super::permissions::build_tool_policy(
            &config.approval,
            &config.allow_rules,
            &config.deny_rules,
            session_mode,
        )
    }
    .map_err(|error| format!("tool permissions are invalid: {error}"))?
    .with_turn_rules(gate.turn_rules());
    let tools = ToolExecutionService::new(Arc::clone(&store))
        .with_policy(policy)
        .with_hooks(config.hooks.clone());
    let options = TurnOptions {
        workspace_root: workspace_root.clone(),
        actor_id: "interactive.user".to_owned(),
        approvals: ApprovalMode::Ask(gate as Arc<dyn ApprovalGate>),
        limits,
    };
    let observer: Arc<dyn TurnObserver> = Arc::new(ChannelObserver {
        sender: sender.clone(),
        cost_tracker,
        model_price: config.model_prices.get(&config.provider.model).copied(),
        auto_allowed_count,
        bell: config.bell,
        tool_started: Mutex::new(None),
    });
    let name = if is_undo { "undo" } else { "write_file" };
    observer.observe(TurnProgress::StepStarted { step: 1 });
    observer.observe(TurnProgress::ToolStarted {
        name: name.to_owned(),
        summary: if is_undo {
            "restore the most recent changed file".to_owned()
        } else {
            "export session transcript".to_owned()
        },
    });
    let tool_request = harness_tools::ToolRequest::new(
        session_id,
        task_id,
        "interactive.user",
        &workspace_root,
        action,
    );
    let executed =
        execute_action_with_approval(&tools, tool_request, &options, 1, &observer, &cancellation)
            .await;
    let (message, outcome) = match executed {
        Ok(view) => {
            let settled = view
                .receipt
                .as_ref()
                .is_some_and(|r| r.outcome_state == harness_types::ToolOutcomeState::Settled);
            let message = if settled {
                if is_undo {
                    "undo action completed"
                } else {
                    "session export written"
                }
            } else {
                "session action was not completed"
            };
            observer.observe(TurnProgress::ToolSettled {
                name: name.to_owned(),
                ok: settled,
                detail: (!settled).then(|| message.to_owned()),
            });
            (
                message.to_owned(),
                if settled {
                    RunOutcome::Done
                } else {
                    RunOutcome::Blocked(message.to_owned())
                },
            )
        }
        Err(error) => {
            let message = format!("session action was not run: {error}");
            observer.observe(TurnProgress::ToolSettled {
                name: name.to_owned(),
                ok: false,
                detail: Some(message.clone()),
            });
            (message.clone(), RunOutcome::Blocked(message))
        }
    };
    let _ = sender.send(SessionEvent::Notice { message });
    Ok(outcome)
}

async fn latest_undo_action(
    store: &SqliteStore,
    task_id: &TaskId,
    project_id: &harness_types::ProjectId,
    workspace_root: &Path,
) -> Result<CodingToolAction, String> {
    let mut sessions = store
        .list_sessions()
        .await
        .map_err(|error| error.to_string())?;
    sessions.retain(|session| &session.task_id == task_id);
    sessions.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    for session in sessions {
        let receipts = store
            .load_receipts(&session.session_id)
            .await
            .map_err(|error| error.to_string())?;
        for receipt in receipts.into_iter().rev() {
            if receipt.task_id != *task_id
                || receipt.outcome_state != harness_types::ToolOutcomeState::Settled
            {
                continue;
            }
            let (Some(before_hash), Some(after_hash), Some(artifact_id)) = (
                receipt.before_hash.clone(),
                receipt.after_hash.clone(),
                receipt.artifact_id.clone(),
            ) else {
                continue;
            };
            let intent = store
                .tool_intent(&receipt.tool_execution_id)
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "the latest file receipt has no matching intent".to_owned())?;
            if !matches!(
                intent.tool_name.as_str(),
                "write_file" | "edit_file" | "apply_patch"
            ) {
                continue;
            }
            let path = intent
                .action_json
                .get("path")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "the latest file intent has no path".to_owned())?;
            if !store
                .artifact_is_scoped_to(&artifact_id, project_id, task_id)
                .await
                .map_err(|error| error.to_string())?
            {
                return Err("the original file artifact is outside this task's scope".to_owned());
            }
            let page = store
                .read_artifact_page(artifact_id.as_str(), 0, 1024 * 1024 + 1)
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "the original file artifact is missing".to_owned())?;
            if page.total_bytes > 1024 * 1024
                || page.bytes.len() as u64 != page.total_bytes
                || harness_types::ContentHash::from_bytes(&page.bytes) != before_hash
                || page.content_hash != before_hash
            {
                return Err("the original file artifact failed its size or hash check".to_owned());
            }
            let current_hash =
                observed_file_hash(workspace_root, path).map_err(|error| error.to_string())?;
            if current_hash != after_hash {
                return Err(format!(
                    "undo skipped {path}: its current hash differs from the last receipt"
                ));
            }
            let content = String::from_utf8(page.bytes)
                .map_err(|_| "the original file artifact is not UTF-8".to_owned())?;
            return Ok(CodingToolAction::WriteFile {
                path: path.to_owned(),
                content,
                expected_hash: Some(after_hash),
            });
        }
    }
    Err("no reversible file change with a before artifact was found in this session".to_owned())
}

async fn render_session_export(
    store: &SqliteStore,
    task_id: &TaskId,
    environment: &LaunchEnvironment,
    data_dir: &Path,
    provider_key_variable: &str,
    jsonl: bool,
) -> Result<String, String> {
    const EXPORT_LIMIT: usize = 1024 * 1024;
    let mut secrets = super::bootstrap::CREDENTIAL_VARIABLES
        .iter()
        .filter_map(|name| environment.value(name))
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if let Some(value) = environment.value(provider_key_variable) {
        let value = value.to_string_lossy().into_owned();
        if !value.is_empty() {
            secrets.push(value);
        }
    }
    if let Some(value) = credentials::load(&credentials::resolve_file(environment, data_dir))
        .map_err(|error| error.to_string())?
        && !value.is_empty()
    {
        secrets.push(value);
    }
    let mut sessions = store
        .list_sessions()
        .await
        .map_err(|error| error.to_string())?;
    sessions.retain(|session| &session.task_id == task_id);
    sessions.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    let mut output = if jsonl {
        String::new()
    } else {
        "# Harness session export\n\n".to_owned()
    };
    for session in sessions {
        let mut cursor = 0;
        loop {
            let events = store
                .load_events_after_limit(&session.session_id, cursor, 256)
                .await
                .map_err(|error| error.to_string())?;
            if events.is_empty() {
                break;
            }
            let count = events.len();
            for event in events {
                cursor = event.seq;
                let mut value = serde_json::to_value(event).map_err(|error| error.to_string())?;
                redact_export_value(&mut value, &secrets);
                if jsonl {
                    output.push_str(
                        &serde_json::to_string(&value).map_err(|error| error.to_string())?,
                    );
                    output.push('\n');
                } else {
                    output.push_str("## Event\n\n```json\n");
                    output.push_str(
                        &serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?,
                    );
                    output.push_str("\n```\n\n");
                }
                if output.len() > EXPORT_LIMIT {
                    return Err("session export exceeds the 1 MiB file limit".to_owned());
                }
            }
            if count < 256 {
                break;
            }
        }
    }
    Ok(output)
}

fn redact_export_value(value: &mut serde_json::Value, secrets: &[String]) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, child) in object.iter_mut() {
                let key = key.to_ascii_lowercase();
                if [
                    "secret",
                    "token",
                    "password",
                    "api_key",
                    "credential",
                    "authorization",
                ]
                .iter()
                .any(|needle| key.contains(needle))
                {
                    *child = serde_json::Value::String("[REDACTED]".to_owned());
                } else {
                    redact_export_value(child, secrets);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                redact_export_value(value, secrets);
            }
        }
        serde_json::Value::String(text) => {
            for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
                *text = text.replace(secret, "[REDACTED]");
            }
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
#[allow(
    clippy::too_many_lines,
    reason = "the direct shell route keeps admission, shared policy, approval, receipt, and output in order"
)]
async fn run_shell_prefix_turn(
    sender: UnboundedSender<SessionEvent>,
    store: Arc<SqliteStore>,
    config_file: PathBuf,
    workspace_root: PathBuf,
    environment: LaunchEnvironment,
    config_overrides: ConfigOverrides,
    session_mode: Option<PolicyMode>,
    session_id: SessionId,
    task_id: TaskId,
    source: Option<SessionId>,
    previous_session: Arc<Mutex<Option<SessionId>>>,
    gate: Arc<ChannelApprovalGate>,
    cost_tracker: Arc<Mutex<CostTracker>>,
    auto_allowed_count: Arc<AtomicUsize>,
    limits: TurnLimits,
    request: SubmitRequest,
    shell_prefix: ShellPrefix,
    cancellation: CancellationToken,
) {
    let send = |event| {
        let _ = sender.send(event);
    };
    let config = match super::config::resolve_layers(
        &config_file,
        &workspace_root,
        &environment,
        &config_overrides,
    ) {
        Ok(config) => config,
        Err(error) => {
            send(SessionEvent::RecoverableError {
                message: error.to_string(),
            });
            if let Ok(store) = Arc::try_unwrap(store) {
                let _ = store.close().await;
            }
            return;
        }
    };
    gate.set_bell(config.bell);
    if let Some(message) = &config.context_window_notice {
        send(SessionEvent::Notice {
            message: message.clone(),
        });
    }
    let project_id = match project::resolve_project_id(&store, &workspace_root).await {
        Ok(project_id) => project_id,
        Err(error) => {
            send(SessionEvent::RecoverableError {
                message: format!("project identity is unavailable: {error}"),
            });
            if let Ok(store) = Arc::try_unwrap(store) {
                let _ = store.close().await;
            }
            return;
        }
    };
    let observation = match observe_workspace(project_id, &workspace_root) {
        Ok(observation) => observation,
        Err(error) => {
            send(SessionEvent::RecoverableError {
                message: format!("workspace cannot be observed: {error}"),
            });
            if let Ok(store) = Arc::try_unwrap(store) {
                let _ = store.close().await;
            }
            return;
        }
    };
    let expected_sequence = match store.session_summary(&session_id).await {
        Ok(summary) => summary.map_or(1, |summary| summary.next_sequence),
        Err(error) => {
            send(SessionEvent::RecoverableError {
                message: format!("shell input sequence could not be read: {error}"),
            });
            if let Ok(store) = Arc::try_unwrap(store) {
                let _ = store.close().await;
            }
            return;
        }
    };
    if let Err(error) = SessionService::new(Arc::clone(&store))
        .admit_input(AdmitInputRequest {
            session_id: session_id.clone(),
            task_id: task_id.clone(),
            input_id: request.input_id.clone(),
            expected_sequence,
            authority: SourceAuthority::User,
            raw_text: request.text,
            workspace: observation,
            initial_plan_items: Vec::new(),
        })
        .await
    {
        send(SessionEvent::RecoverableError {
            message: format!("shell input was not admitted: {error}"),
        });
        if let Ok(store) = Arc::try_unwrap(store) {
            let _ = store.close().await;
        }
        return;
    }
    if let Some(source) = source
        && let Err(error) = store
            .record_continuation_link(&source, &session_id, &task_id)
            .await
    {
        send(SessionEvent::RecoverableError {
            message: format!("shell session link could not be recorded: {error}"),
        });
        if let Ok(store) = Arc::try_unwrap(store) {
            let _ = store.close().await;
        }
        return;
    }
    if let Ok(mut previous) = previous_session.lock() {
        *previous = Some(session_id.clone());
    }

    let policy = match super::permissions::build_tool_policy(
        &config.approval,
        &config.allow_rules,
        &config.deny_rules,
        session_mode,
    ) {
        Ok(policy) => policy.with_turn_rules(gate.turn_rules()),
        Err(error) => {
            send(SessionEvent::RecoverableError {
                message: format!("tool permissions are invalid: {error}"),
            });
            if let Ok(store) = Arc::try_unwrap(store) {
                let _ = store.close().await;
            }
            return;
        }
    };
    let tools = ToolExecutionService::new(Arc::clone(&store))
        .with_policy(policy)
        .with_hooks(config.hooks.clone());
    let options = TurnOptions {
        workspace_root: workspace_root.clone(),
        actor_id: "interactive.user".to_owned(),
        approvals: ApprovalMode::Ask(gate as Arc<dyn ApprovalGate>),
        limits,
    };
    let observer: Arc<dyn TurnObserver> = Arc::new(ChannelObserver {
        sender: sender.clone(),
        cost_tracker,
        model_price: config.model_prices.get(&config.provider.model).copied(),
        auto_allowed_count,
        bell: config.bell,
        tool_started: Mutex::new(None),
    });
    observer.observe(TurnProgress::StepStarted { step: 1 });
    observer.observe(TurnProgress::ToolStarted {
        name: "run_shell".to_owned(),
        summary: format!("shell: {}", shell_prefix.command),
    });
    let tool_request = harness_tools::ToolRequest::new(
        session_id,
        task_id,
        "interactive.user",
        &workspace_root,
        CodingToolAction::RunShell {
            command: shell_prefix.command.clone(),
            timeout_ms: 60_000,
            isolation: IsolationMode::BestEffort,
            env: Vec::new(),
        },
    );
    let result =
        execute_action_with_approval(&tools, tool_request, &options, 1, &observer, &cancellation)
            .await;
    let (output, attachable, outcome) = match result {
        Ok(view) => {
            let settled = view.receipt.as_ref().is_some_and(|receipt| {
                receipt.outcome_state == harness_types::ToolOutcomeState::Settled
            });
            let (output, process_result) = shell_output(&view.output);
            let outcome = if settled {
                RunOutcome::Done
            } else {
                RunOutcome::Blocked(output.clone())
            };
            observer.observe(TurnProgress::ToolSettled {
                name: "run_shell".to_owned(),
                ok: settled && process_result,
                detail: (!settled || !process_result).then(|| output.clone()),
            });
            (output, settled, outcome)
        }
        Err(error) => {
            let output = format!("not run: {error}");
            observer.observe(TurnProgress::ToolSettled {
                name: "run_shell".to_owned(),
                ok: false,
                detail: Some(output.clone()),
            });
            (output.clone(), false, RunOutcome::Blocked(output))
        }
    };
    send(SessionEvent::ShellPrefixCompleted {
        command: shell_prefix.command,
        output,
        attach_to_next_message: attachable
            && shell_prefix.mode == ShellPrefixMode::AttachToNextMessage,
    });
    drop(tools);
    if let Ok(store) = Arc::try_unwrap(store) {
        let _ = store.close().await;
    }
    send(SessionEvent::RunTerminal { outcome });
}

fn shell_output(output: &ToolOutput) -> (String, bool) {
    use std::fmt::Write as _;

    let (mut text, successful) = match output {
        ToolOutput::Process {
            exit_code,
            timed_out,
            canceled,
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
            capture_truncated,
            capture_tail,
            ..
        } => {
            let mut text = String::new();
            if !stdout.is_empty() {
                text.push_str(stdout);
            }
            if !stderr.is_empty() {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str("[stderr]\n");
                text.push_str(stderr);
            }
            if let Some(exit_code) = exit_code {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                let _ = write!(text, "[exit code {exit_code}]");
            }
            if *timed_out {
                text.push_str("\n[command timed out]");
            }
            if *canceled {
                text.push_str("\n[command canceled]");
            }
            if *stdout_truncated {
                text.push_str("\n[stdout preview truncated]");
            }
            if *stderr_truncated {
                text.push_str("\n[stderr preview truncated]");
            }
            if *capture_truncated {
                text.push_str("\n[capture truncated at quota]");
                if !capture_tail.is_empty() {
                    text.push_str("\n[capture tail]\n");
                    text.push_str(capture_tail);
                }
            }
            (
                text,
                exit_code.is_some_and(|code| code == 0) && !timed_out && !canceled,
            )
        }
        ToolOutput::Denied { reason, .. } => (format!("not run: {reason}"), false),
        ToolOutput::OutcomeUnknown { reason } => (format!("outcome unknown: {reason}"), false),
        _ => (
            "run_shell returned an unexpected output type".to_owned(),
            false,
        ),
    };
    if text.len() > SHELL_PREFIX_OUTPUT_LIMIT {
        let max_content =
            SHELL_PREFIX_OUTPUT_LIMIT.saturating_sub(SHELL_PREFIX_OUTPUT_TRUNCATION.len());
        let mut boundary = max_content.min(text.len());
        while !text.is_char_boundary(boundary) {
            boundary = boundary.saturating_sub(1);
        }
        text.truncate(boundary);
        text.push_str(SHELL_PREFIX_OUTPUT_TRUNCATION);
    }
    (text, successful)
}

fn prompt_git_facts(root: &Path) -> (Option<String>, Option<usize>) {
    let branch = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|output| output.trim().to_owned())
        .filter(|branch| !branch.is_empty());
    let changed = Command::new("git")
        .args(["status", "--porcelain=v1", "--untracked-files=all"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|output| output.lines().count());
    (branch, changed)
}

/// Deterministic labelled fixture used by tests and explicit demos.
#[derive(Debug)]
pub struct FixtureService {
    sender: UnboundedSender<SessionEvent>,
    pending_approval: Arc<Mutex<Option<String>>>,
    /// Mirrors the real gate's run-scoped grant, so a PTY case can drive the same
    /// contract instead of a simplified one.
    granted_for_run: bool,
}

impl FixtureService {
    #[must_use]
    pub fn new(sender: UnboundedSender<SessionEvent>) -> Self {
        Self {
            sender,
            pending_approval: Arc::new(Mutex::new(None)),
            granted_for_run: false,
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
            detail: String::new(),
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
                rule_pattern: "fixture_action()".to_owned(),
                workspace: "fixture workspace (no mutation)".to_owned(),
                scope: "once".to_owned(),
                expires_at: Instant::now() + DEFAULT_APPROVAL_TIMEOUT,
                // The fixture action mutates nothing, but it stands in for the gated
                // write: the case that must never be covered by "allow reads".
                read_only: false,
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
        if decision != ApprovalDecision::Denied {
            if decision == ApprovalDecision::GrantForRun {
                self.granted_for_run = true;
            }
            self.run_fixture_tool("fixture_action", "no mutation", true);
        }
        let _ = self.sender.send(SessionEvent::RunTerminal {
            outcome: RunOutcome::Done,
        });
        true
    }

    fn grant_run_approval(&mut self) {
        self.granted_for_run = true;
    }

    fn revoke_run_approval(&mut self) {
        self.granted_for_run = false;
    }

    fn provider_diagnostics(&self) -> Vec<String> {
        vec!["Provider: the labelled fixture; no model is called".to_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AdmitInputRequest, AgentSessionService, ApprovalDecision, ApprovalGate, ApprovalProposal,
        ChannelApprovalGate, CodingToolAction, DEEPSEEK_ENDPOINT, DEEPSEEK_MODEL,
        ENDPOINT_VARIABLE, EnvironmentCredential, FixtureService, MODEL_VARIABLE, SessionChannel,
        SessionPort, SessionService, SourceAuthority, SubmitRequest, ToolOutput,
        execute_action_with_approval, latest_undo_action, observe_workspace, observed_file_hash,
        provider_diagnostics, render_session_export, resolve_provider, validate_credential_file,
    };
    use crate::interactive::bootstrap::{self, LaunchRequest};
    use crate::interactive::credentials::{self, CredentialSource, Protection};
    use crate::interactive::events::{RunOutcome, SessionEvent};
    use crate::interactive::paths::{HostPlatform, LaunchEnvironment};
    use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
    use harness_tools::{
        ApprovalAnswer, ApprovalMode, ConfiguredToolHook, HostEnvironment, TurnLimits,
        TurnObserver, TurnOptions, TurnProgress,
    };
    use harness_types::{ErrorCode, InputId};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn request() -> SubmitRequest {
        SubmitRequest {
            input_id: InputId::generate(),
            text: "fix the parser".to_owned(),
            answer_question_id: None,
            shell_prefix: None,
            compact_guidance: None,
            refine: None,
        }
    }

    #[derive(Default)]
    struct HookTestObserver {
        progress: Mutex<Vec<TurnProgress>>,
    }

    impl TurnObserver for HookTestObserver {
        fn observe(&self, progress: TurnProgress) {
            if let Ok(mut entries) = self.progress.lock() {
                entries.push(progress);
            }
        }
    }

    fn hook_process(script: &str) -> (String, Vec<String>) {
        #[cfg(windows)]
        {
            (
                "pwsh".to_owned(),
                vec![
                    "-NoProfile".to_owned(),
                    "-NonInteractive".to_owned(),
                    "-Command".to_owned(),
                    script.to_owned(),
                ],
            )
        }
        #[cfg(not(windows))]
        {
            ("sh".to_owned(), vec!["-c".to_owned(), script.to_owned()])
        }
    }

    fn hook(
        event: &str,
        matcher: Option<&str>,
        timeout_seconds: u64,
        script: &str,
    ) -> ConfiguredToolHook {
        let (command, args) = hook_process(script);
        ConfiguredToolHook {
            event: event.to_owned(),
            matcher: matcher.map(str::to_owned),
            command,
            args,
            timeout_seconds,
            source: "test fixture".to_owned(),
        }
    }

    fn hook_read_stdin_script(output: &str, exit: i32) -> String {
        #[cfg(windows)]
        {
            format!(
                "$null = [Console]::In.ReadToEnd(); Write-Output '{}'; exit {}",
                output.replace('\'', "''"),
                exit
            )
        }
        #[cfg(not(windows))]
        {
            format!(
                "cat >/dev/null; printf '%s\\n' '{}'; exit {exit}",
                output.replace('\'', "'\\''")
            )
        }
    }

    #[tokio::test]
    async fn g09_pre_tool_use_exit_2_blocks_and_records_the_reason() {
        let fixture = undo_fixture().await;
        let mut channel = SessionChannel::new();
        let gate = Arc::new(ChannelApprovalGate::new(
            channel.sender(),
            Duration::from_secs(3),
        ));
        let tools = harness_tools::ToolExecutionService::new(Arc::clone(&fixture.store))
            .with_hooks(vec![hook(
                "pre_tool_use",
                Some("write_file"),
                5,
                &hook_read_stdin_script("fixture refused this write", 2),
            )]);
        let action = CodingToolAction::WriteFile {
            path: "src/blocked.txt".to_owned(),
            content: "must stay absent".to_owned(),
            expected_hash: None,
        };
        let request = harness_tools::ToolRequest::new(
            fixture.source_session.clone(),
            fixture.task_id.clone(),
            "hook.test",
            &fixture.workspace,
            action,
        );
        let options = TurnOptions {
            workspace_root: fixture.workspace.clone(),
            actor_id: "hook.test".to_owned(),
            approvals: ApprovalMode::Ask(gate),
            limits: TurnLimits::default(),
        };
        let observer = Arc::new(HookTestObserver::default());
        let observer_trait: Arc<dyn TurnObserver> = observer.clone();
        let view = execute_action_with_approval(
            &tools,
            request,
            &options,
            2,
            &observer_trait,
            &harness_providers::CancellationToken::new(),
        )
        .await
        .expect("hook denial is a settled receipt");

        let ToolOutput::Denied { code, reason } = &view.output else {
            panic!("exit 2 must block the action: {:?}", view.output);
        };
        assert_eq!(code, "blocked_by_hook");
        assert!(reason.contains("fixture refused this write"), "{reason}");
        assert!(observer.progress.lock().expect("observer entries").iter().any(|progress| matches!(progress, TurnProgress::Notice(message) if message.contains("blocked by hook"))));
        assert_eq!(
            view.receipt.as_ref().expect("denial receipt").outcome_state,
            harness_types::ToolOutcomeState::Denied
        );
        assert!(!fixture.workspace.join("src/blocked.txt").exists());
        assert!(
            channel
                .drain()
                .iter()
                .all(|event| !matches!(event, SessionEvent::ApprovalRequired { .. }))
        );
        let events = fixture
            .store
            .load_events_after(&fixture.source_session, 0)
            .await
            .expect("journal");
        let denial = events
            .iter()
            .find(|event| {
                event
                    .payload
                    .get("model_view")
                    .is_some_and(|view| view["code"] == "blocked_by_hook")
            })
            .expect("blocked_by_hook and reason are durable with the receipt event");
        assert!(
            denial.payload["model_view"]["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("fixture refused"))
        );
        drop(tools);
        Arc::try_unwrap(fixture.store)
            .expect("store consumers released")
            .close()
            .await
            .expect("close store");
    }

    #[tokio::test]
    async fn g09_hook_cannot_turn_ask_into_allow() {
        let fixture = undo_fixture().await;
        let mut channel = SessionChannel::new();
        let gate = Arc::new(ChannelApprovalGate::new(
            channel.sender(),
            Duration::from_secs(3),
        ));
        let tools = harness_tools::ToolExecutionService::new(Arc::clone(&fixture.store))
            .with_hooks(vec![hook(
                "pre_tool_use",
                Some("write_file"),
                5,
                &hook_read_stdin_script("allow", 0),
            )]);
        let request = harness_tools::ToolRequest::new(
            fixture.source_session.clone(),
            fixture.task_id.clone(),
            "hook.test",
            &fixture.workspace,
            CodingToolAction::WriteFile {
                path: "src/allow.txt".to_owned(),
                content: "still needs approval".to_owned(),
                expected_hash: None,
            },
        );
        let options = TurnOptions {
            workspace_root: fixture.workspace.clone(),
            actor_id: "hook.test".to_owned(),
            approvals: ApprovalMode::Ask(Arc::clone(&gate) as Arc<dyn harness_tools::ApprovalGate>),
            limits: TurnLimits::default(),
        };
        let observer: Arc<dyn TurnObserver> = Arc::new(HookTestObserver::default());
        let execution = tokio::spawn(async move {
            execute_action_with_approval(
                &tools,
                request,
                &options,
                2,
                &observer,
                &harness_providers::CancellationToken::new(),
            )
            .await
        });
        let request_id = tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                if let Some(id) = channel.drain().into_iter().find_map(|event| match event {
                    SessionEvent::ApprovalRequired { request_id, .. } => Some(request_id),
                    _ => None,
                }) {
                    break id;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("exit 0 stdout allow still opens Ask panel");
        assert!(!fixture.workspace.join("src/allow.txt").exists());
        assert!(gate.answer(&request_id, ApprovalDecision::Denied));
        assert_eq!(
            execution
                .await
                .expect("tool task joined")
                .expect_err("explicit denial")
                .code(),
            ErrorCode::PolicyDenied
        );
        assert!(!fixture.workspace.join("src/allow.txt").exists());
        Arc::try_unwrap(fixture.store)
            .expect("store consumers released")
            .close()
            .await
            .expect("close store");
    }

    #[tokio::test]
    async fn g09_hook_timeout_blocks_not_allows() {
        let fixture = undo_fixture().await;
        #[cfg(windows)]
        let wait_script = "$null = [Console]::In.ReadToEnd(); Start-Sleep -Seconds 5; exit 0";
        #[cfg(not(windows))]
        let wait_script = "cat >/dev/null; sleep 5; exit 0";
        let tools =
            harness_tools::ToolExecutionService::new(Arc::clone(&fixture.store)).with_hooks(vec![
                hook("pre_tool_use", Some("write_file"), 1, wait_script),
            ]);
        let request = harness_tools::ToolRequest::new(
            fixture.source_session.clone(),
            fixture.task_id.clone(),
            "hook.test",
            &fixture.workspace,
            CodingToolAction::WriteFile {
                path: "src/timeout.txt".to_owned(),
                content: "must stay absent".to_owned(),
                expected_hash: None,
            },
        );
        let options = TurnOptions {
            workspace_root: fixture.workspace.clone(),
            actor_id: "hook.test".to_owned(),
            approvals: ApprovalMode::None,
            limits: TurnLimits::default(),
        };
        let observer: Arc<dyn TurnObserver> = Arc::new(HookTestObserver::default());
        let view = execute_action_with_approval(
            &tools,
            request,
            &options,
            2,
            &observer,
            &harness_providers::CancellationToken::new(),
        )
        .await
        .expect("timeout is a fail-closed receipt");
        assert!(
            matches!(&view.output, ToolOutput::Denied { code, reason } if code == "blocked_by_hook" && reason.contains("timed out"))
        );
        assert!(!fixture.workspace.join("src/timeout.txt").exists());
        drop(tools);
        Arc::try_unwrap(fixture.store)
            .expect("store consumers released")
            .close()
            .await
            .expect("close store");
    }

    #[tokio::test]
    async fn g09_post_tool_hook_failure_does_not_change_the_settled_receipt() {
        let fixture = undo_fixture().await;
        let mut channel = SessionChannel::new();
        let gate = Arc::new(ChannelApprovalGate::new(
            channel.sender(),
            Duration::from_secs(3),
        ));
        let tools = harness_tools::ToolExecutionService::new(Arc::clone(&fixture.store))
            .with_hooks(vec![hook(
                "post_tool_use",
                Some("write_file"),
                5,
                &hook_read_stdin_script("post hook failed", 2),
            )]);
        let request = harness_tools::ToolRequest::new(
            fixture.source_session.clone(),
            fixture.task_id.clone(),
            "hook.test",
            &fixture.workspace,
            CodingToolAction::WriteFile {
                path: "src/post-hook.txt".to_owned(),
                content: "committed".to_owned(),
                expected_hash: None,
            },
        );
        let options = TurnOptions {
            workspace_root: fixture.workspace.clone(),
            actor_id: "hook.test".to_owned(),
            approvals: ApprovalMode::Ask(Arc::clone(&gate) as Arc<dyn harness_tools::ApprovalGate>),
            limits: TurnLimits::default(),
        };
        let observer = Arc::new(HookTestObserver::default());
        let observer_trait: Arc<dyn TurnObserver> = observer.clone();
        let execution = tokio::spawn(async move {
            execute_action_with_approval(
                &tools,
                request,
                &options,
                2,
                &observer_trait,
                &harness_providers::CancellationToken::new(),
            )
            .await
        });
        let request_id = tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                if let Some(id) = channel.drain().into_iter().find_map(|event| match event {
                    SessionEvent::ApprovalRequired { request_id, .. } => Some(request_id),
                    _ => None,
                }) {
                    break id;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("write waits at Ask");
        assert!(gate.answer(&request_id, ApprovalDecision::Granted));
        let view = execution
            .await
            .expect("tool task joined")
            .expect("action settled");
        assert_eq!(
            view.receipt.expect("settled receipt").outcome_state,
            harness_types::ToolOutcomeState::Settled
        );
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.join("src/post-hook.txt"))
                .expect("write committed"),
            "committed"
        );
        assert!(observer.progress.lock().expect("observer entries").iter().any(|progress| matches!(progress, TurnProgress::Notice(message) if message.contains("post_tool_use hook") && message.contains("post hook failed"))));
        Arc::try_unwrap(fixture.store)
            .expect("store consumers released")
            .close()
            .await
            .expect("close store");
    }

    #[tokio::test]
    async fn g09_stop_hook_failure_is_notice_only() {
        let temp = tempfile::tempdir().expect("temporary root");
        let store = Arc::new(
            SqliteStore::open_writer(WriterOpenOptions::new(
                temp.path().join("data"),
                harness_types::HostId::generate(),
            ))
            .await
            .expect("store opens"),
        );
        let tools =
            harness_tools::ToolExecutionService::new(Arc::clone(&store)).with_hooks(vec![hook(
                "stop",
                None,
                5,
                &hook_read_stdin_script("stop hook failed", 2),
            )]);
        let payload = serde_json::json!({"event":"stop", "cwd": temp.path()});
        let notices = tools
            .run_event_hooks("stop", payload, harness_providers::CancellationToken::new())
            .await;
        assert_eq!(notices.len(), 1);
        assert!(
            notices[0].contains("stop hook") && notices[0].contains("stop hook failed"),
            "{notices:?}"
        );
        drop(tools);
        Arc::try_unwrap(store)
            .expect("store consumers released")
            .close()
            .await
            .expect("close store");
    }

    #[tokio::test]
    async fn g09_hook_receives_bounded_json_without_secrets() {
        let temp = tempfile::tempdir().expect("temporary root");
        let host = HostEnvironment::from_process().with_values([(
            "DEEPSEEK_API_KEY".to_owned(),
            "sk-hook-fixture-secret".to_owned(),
        )]);
        let input = serde_json::json!({
            "event": "pre_tool_use",
            "tool": {"name": "write_file", "args": {"api_key": "[REDACTED]", "content": "x".repeat(7000)}},
        }).to_string();
        assert!(input.len() <= 8 * 1024);
        let script = {
            #[cfg(windows)]
            {
                "$payload = [Console]::In.ReadToEnd(); Write-Output $payload; if ($env:DEEPSEEK_API_KEY) { Write-Output $env:DEEPSEEK_API_KEY; exit 9 }; exit 0"
            }
            #[cfg(not(windows))]
            {
                "cat; if [ -n \"$DEEPSEEK_API_KEY\" ]; then printf '%s' \"$DEEPSEEK_API_KEY\"; exit 9; fi; exit 0"
            }
        };
        let (command, args) = hook_process(script);
        let result = harness_tools::run_hook_command_with_host(
            temp.path(),
            &command,
            &args,
            input.as_bytes(),
            5000,
            harness_providers::CancellationToken::new(),
            &host,
        )
        .await
        .expect("fixture process ran through harness-tools::process");
        assert_eq!(result.exit_code, Some(0), "{}", result.stdout);
        assert!(result.stdout.contains("pre_tool_use"));
        assert!(!result.stdout.contains("sk-hook-fixture-secret"));
        assert!(
            harness_tools::run_hook_command_with_host(
                temp.path(),
                &command,
                &args,
                &vec![b'x'; 8 * 1024 + 1],
                5000,
                harness_providers::CancellationToken::new(),
                &host,
            )
            .await
            .is_err(),
            "input beyond 8 KiB never spawns"
        );
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

    async fn record_project_session(
        store: &Arc<SqliteStore>,
        workspace_root: &std::path::Path,
        session_id: harness_types::SessionId,
    ) {
        let project_id = crate::interactive::project::resolve_project_id(store, workspace_root)
            .await
            .expect("project id");
        let workspace = observe_workspace(project_id, workspace_root).expect("workspace snapshot");
        SessionService::new(Arc::clone(store))
            .admit_input(AdmitInputRequest {
                session_id,
                task_id: harness_types::TaskId::generate(),
                input_id: InputId::generate(),
                expected_sequence: 1,
                authority: SourceAuthority::User,
                raw_text: "session input".to_owned(),
                workspace,
                initial_plan_items: Vec::new(),
            })
            .await
            .expect("session input admitted");
    }

    struct UndoFixture {
        temp: tempfile::TempDir,
        store: Arc<SqliteStore>,
        workspace: PathBuf,
        project_id: harness_types::ProjectId,
        task_id: harness_types::TaskId,
        source_session: harness_types::SessionId,
        original_receipt: harness_types::ToolExecutionReceipt,
    }

    struct UndoContext {
        workspace: PathBuf,
        project_id: harness_types::ProjectId,
        task_id: harness_types::TaskId,
        source_session: harness_types::SessionId,
        original_receipt: harness_types::ToolExecutionReceipt,
    }

    async fn undo_fixture() -> UndoFixture {
        let temp = tempfile::tempdir().expect("temporary root");
        let workspace = temp.path().join("workspace");
        let data_dir = temp.path().join("data");
        std::fs::create_dir_all(workspace.join("src")).expect("source directory");
        std::fs::create_dir_all(&data_dir).expect("data directory");
        std::fs::write(workspace.join("src").join("undo.txt"), "before\n").expect("original file");
        let store = Arc::new(
            SqliteStore::open_writer(WriterOpenOptions::new(
                data_dir,
                harness_types::HostId::generate(),
            ))
            .await
            .expect("project store"),
        );
        let project_id = crate::interactive::project::resolve_project_id(&store, &workspace)
            .await
            .expect("project id");
        let task_id = harness_types::TaskId::generate();
        let source_session = harness_types::SessionId::generate();
        let observation =
            observe_workspace(project_id.clone(), &workspace).expect("workspace snapshot");
        SessionService::new(Arc::clone(&store))
            .admit_input(AdmitInputRequest {
                session_id: source_session.clone(),
                task_id: task_id.clone(),
                input_id: InputId::generate(),
                expected_sequence: 1,
                authority: SourceAuthority::User,
                raw_text: "change a file".to_owned(),
                workspace: observation,
                initial_plan_items: Vec::new(),
            })
            .await
            .expect("admit source turn");
        let tools = harness_tools::ToolExecutionService::new(Arc::clone(&store));
        let before_hash = observed_file_hash(&workspace, "src/undo.txt").expect("before hash");
        let prepared = tools
            .prepare(harness_tools::ToolRequest::new(
                source_session.clone(),
                task_id.clone(),
                "undo.fixture",
                &workspace,
                harness_tools::CodingToolAction::WriteFile {
                    path: "src/undo.txt".to_owned(),
                    content: "after\n".to_owned(),
                    expected_hash: Some(before_hash),
                },
            ))
            .await
            .expect("write proposal prepares");
        let approval = tools
            .approve(&prepared)
            .await
            .expect("test grants exact write");
        tools
            .execute_with_cancellation(
                prepared,
                Some(approval),
                harness_providers::CancellationToken::new(),
            )
            .await
            .expect("write settles");
        let original_receipt = store
            .load_receipts(&source_session)
            .await
            .expect("receipt list")
            .into_iter()
            .next()
            .expect("write receipt");
        drop(tools);
        UndoFixture {
            temp,
            store,
            workspace,
            project_id,
            task_id,
            source_session,
            original_receipt,
        }
    }

    async fn reopen_undo_store(
        temp: &tempfile::TempDir,
        previous: Arc<SqliteStore>,
    ) -> Arc<SqliteStore> {
        Arc::try_unwrap(previous)
            .expect("setup store released")
            .close()
            .await
            .expect("close the source turn writer");
        Arc::new(
            SqliteStore::open_writer(WriterOpenOptions::new(
                temp.path().join("data"),
                harness_types::HostId::generate(),
            ))
            .await
            .expect("next turn opens a newer writer generation"),
        )
    }

    async fn admit_undo_input(
        fixture: &UndoContext,
        store: &Arc<SqliteStore>,
    ) -> (harness_types::SessionId, harness_tools::CodingToolAction) {
        let action = latest_undo_action(
            store,
            &fixture.task_id,
            &fixture.project_id,
            &fixture.workspace,
        )
        .await
        .expect("undo proposal");
        let session_id = harness_types::SessionId::generate();
        let workspace = observe_workspace(fixture.project_id.clone(), &fixture.workspace)
            .expect("fresh workspace snapshot");
        SessionService::new(Arc::clone(store))
            .admit_input(AdmitInputRequest {
                session_id: session_id.clone(),
                task_id: fixture.task_id.clone(),
                input_id: InputId::generate(),
                expected_sequence: 1,
                authority: SourceAuthority::User,
                raw_text: "/undo".to_owned(),
                workspace,
                initial_plan_items: Vec::new(),
            })
            .await
            .expect("admit the separate undo action input");
        store
            .record_continuation_link(&fixture.source_session, &session_id, &fixture.task_id)
            .await
            .expect("link undo turn");
        (session_id, action)
    }

    struct UndoQuietObserver;

    impl harness_tools::TurnObserver for UndoQuietObserver {
        fn observe(&self, _progress: harness_tools::TurnProgress) {}
    }

    async fn run_undo_through_approval(
        fixture: &UndoContext,
        store: &Arc<SqliteStore>,
        session_id: harness_types::SessionId,
        action: harness_tools::CodingToolAction,
    ) -> harness_tools::ToolExecutionView {
        let mut channel = SessionChannel::new();
        let gate = Arc::new(ChannelApprovalGate::new(
            channel.sender(),
            Duration::from_secs(3),
        ));
        let tools = harness_tools::ToolExecutionService::new(Arc::clone(store));
        let observer: Arc<dyn harness_tools::TurnObserver> = Arc::new(UndoQuietObserver);
        let options = harness_tools::TurnOptions {
            workspace_root: fixture.workspace.clone(),
            actor_id: "interactive.user".to_owned(),
            approvals: harness_tools::ApprovalMode::Ask(Arc::clone(&gate) as Arc<dyn ApprovalGate>),
            limits: harness_tools::TurnLimits::default(),
        };
        let tool_request = harness_tools::ToolRequest::new(
            session_id,
            fixture.task_id.clone(),
            "interactive.user",
            &fixture.workspace,
            action,
        );
        let execution = tokio::spawn(async move {
            execute_action_with_approval(
                &tools,
                tool_request,
                &options,
                1,
                &observer,
                &harness_providers::CancellationToken::new(),
            )
            .await
        });
        let request_id = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(request_id) =
                    channel.drain().into_iter().find_map(|event| match event {
                        SessionEvent::ApprovalRequired {
                            request_id, action, ..
                        } if action == "WriteFile" => Some(request_id),
                        _ => None,
                    })
                {
                    break request_id;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("undo displays an approval panel");
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.join("src/undo.txt")).expect("still current"),
            "after\n",
            "undo does not mutate while its panel is waiting",
        );
        assert!(gate.answer(&request_id, ApprovalDecision::Granted));
        execution
            .await
            .expect("undo task joined")
            .expect("approval executes")
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

    /// A mutating action. `read_only: false` marks it as one that used to be
    /// ineligible for the wider grant, so the tests below still prove the wider
    /// grant reaches it.
    fn proposal(request_id: &str) -> ApprovalProposal {
        ApprovalProposal {
            request_id: request_id.to_owned(),
            action: "ApplyPatch".to_owned(),
            summary: "patch src/parser.rs".to_owned(),
            rule_pattern: "apply_patch(src/parser.rs)".to_owned(),
            workspace: std::path::PathBuf::from("C:/work/repo"),
            scope: "one action, this turn only".to_owned(),
            read_only: false,
        }
    }

    /// The same proposal for a read: identical but for the flag the gate keys on.
    fn read_proposal(request_id: &str) -> ApprovalProposal {
        ApprovalProposal {
            action: "ListFiles".to_owned(),
            summary: "list .".to_owned(),
            read_only: true,
            ..proposal(request_id)
        }
    }

    /// K05: `/status` can name the project scope memory is keyed by.
    ///
    /// The app shows this id nowhere else, and memory is scoped by it, so without
    /// this line an operator has nothing to pass to `ha memory --project-id`. It also
    /// has to stay honest before the first turn: no store means no identity yet, and
    /// inventing one would point later lookups at the wrong place.
    #[tokio::test]
    async fn k05_status_names_the_project_scope_once_the_store_exists() {
        let temp = tempfile::tempdir().expect("temp root");
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&home).expect("fixture home");
        std::fs::create_dir_all(&project).expect("fixture project");
        let environment =
            LaunchEnvironment::from_pairs([("HA_HOME", home.to_string_lossy().into_owned())]);
        let context = bootstrap::resolve(LaunchRequest {
            cwd: None,
            caller_dir: project.clone(),
            platform: HostPlatform::current(),
            environment: environment.clone(),
            explicit_data_dir: None,
        })
        .expect("context resolves");
        let channel = SessionChannel::new();
        let mut service = AgentSessionService::new(&context, environment, channel.sender());

        assert_eq!(
            service.project_id(),
            None,
            "no store yet means the workspace has no registered identity"
        );

        // Register it the way a first turn does, then ask again.
        let store = SqliteStore::open_writer(WriterOpenOptions::new(
            context.project_store_dir(),
            harness_types::HostId::generate(),
        ))
        .await
        .expect("store opens");
        let registered =
            crate::interactive::project::resolve_project_id(&store, &context.project.root)
                .await
                .expect("the root registers");
        store.close().await.expect("store closes");

        let reported = service.project_id().expect("the id is reported");
        assert_eq!(reported, registered.as_str());
        assert!(
            reported.starts_with("project_"),
            "the reported id must be the id the CLI accepts: {reported}"
        );
        assert_eq!(
            service.project_id().as_deref(),
            Some(registered.as_str()),
            "a second ask answers from the cache"
        );
    }

    #[tokio::test]
    async fn g08_rename_persists_and_shows_in_the_picker() {
        let temp = tempfile::tempdir().expect("temporary root");
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&project).expect("project");
        let environment =
            LaunchEnvironment::from_pairs([("HA_HOME", home.to_string_lossy().into_owned())]);
        let context = bootstrap::resolve(LaunchRequest {
            cwd: None,
            caller_dir: project,
            platform: HostPlatform::current(),
            environment: environment.clone(),
            explicit_data_dir: None,
        })
        .expect("launch context");
        let mut channel = SessionChannel::new();
        let mut service = AgentSessionService::new(&context, environment, channel.sender());
        let task_id = service.task_id.clone();
        let session_id = harness_types::SessionId::generate();
        let store = Arc::new(
            SqliteStore::open_writer(WriterOpenOptions::new(
                context.project_store_dir(),
                harness_types::HostId::generate(),
            ))
            .await
            .expect("project store"),
        );
        let project_id =
            crate::interactive::project::resolve_project_id(&store, &context.project.root)
                .await
                .expect("project id");
        let workspace =
            observe_workspace(project_id, &context.project.root).expect("workspace snapshot");
        SessionService::new(Arc::clone(&store))
            .admit_input(AdmitInputRequest {
                session_id,
                task_id,
                input_id: InputId::generate(),
                expected_sequence: 1,
                authority: SourceAuthority::User,
                raw_text: "start a named session".to_owned(),
                workspace,
                initial_plan_items: Vec::new(),
            })
            .await
            .expect("admit a session for the picker");
        Arc::try_unwrap(store)
            .expect("setup store consumers released")
            .close()
            .await
            .expect("close setup store");

        service
            .rename("CP-C review")
            .expect("rename schedules persistence");
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if channel.drain().iter().any(|event| matches!(
                    event, SessionEvent::Notice { message } if message.contains("renamed to CP-C review")
                )) { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("rename completed");
        service.list_sessions();
        let sessions = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(sessions) = channel.drain().into_iter().find_map(|event| {
                    if let SessionEvent::SessionsListed { sessions } = event {
                        Some(sessions)
                    } else {
                        None
                    }
                }) {
                    break sessions;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("picker list arrived");
        assert_eq!(sessions.len(), 1);
        assert!(
            sessions[0].detail.contains("CP-C review"),
            "{}",
            sessions[0].detail
        );
    }

    #[tokio::test]
    async fn g09_hooks_overlay_lists_event_matcher_and_source() {
        let temp = tempfile::tempdir().expect("temporary root");
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&project).expect("project");
        let environment =
            LaunchEnvironment::from_pairs([("HA_HOME", home.to_string_lossy().into_owned())]);
        let context = bootstrap::resolve(LaunchRequest {
            cwd: None,
            caller_dir: project,
            platform: HostPlatform::current(),
            environment: environment.clone(),
            explicit_data_dir: None,
        })
        .expect("launch context");
        std::fs::create_dir_all(context.paths.config_file.parent().expect("config parent"))
            .expect("config directory");
        std::fs::write(
            &context.paths.config_file,
            "schema_version = 2\n[[hooks.pre_tool_use]]\nmatcher = 'write_file|edit_file'\ncommand = 'check-hook'\ntimeout_seconds = 2\n",
        ).expect("trusted user config");
        let channel = SessionChannel::new();
        let service = AgentSessionService::new(&context, environment, channel.sender());

        let lines = service.hooks_summary();

        assert!(
            lines.iter().any(|line| line.contains("pre_tool_use")
                && line.contains("write_file|edit_file")
                && line.contains("source=user")),
            "{lines:?}"
        );
    }

    #[tokio::test]
    async fn g08_export_contains_no_credential_values() {
        let temp = tempfile::tempdir().expect("temporary root");
        let workspace_root = temp.path().join("workspace");
        let data_dir = temp.path().join("data");
        std::fs::create_dir_all(&workspace_root).expect("workspace");
        std::fs::create_dir_all(&data_dir).expect("data directory");
        let store = Arc::new(
            SqliteStore::open_writer(WriterOpenOptions::new(
                data_dir.clone(),
                harness_types::HostId::generate(),
            ))
            .await
            .expect("project store"),
        );
        let session_id = harness_types::SessionId::generate();
        let task_id = harness_types::TaskId::generate();
        let workspace = observe_workspace(harness_types::ProjectId::generate(), &workspace_root)
            .expect("workspace snapshot");
        SessionService::new(Arc::clone(&store))
            .admit_input(AdmitInputRequest {
                session_id,
                task_id: task_id.clone(),
                input_id: InputId::generate(),
                expected_sequence: 1,
                authority: SourceAuthority::User,
                raw_text: "remember sk-fixture-export-secret for later".to_owned(),
                workspace,
                initial_plan_items: Vec::new(),
            })
            .await
            .expect("admit transcript text");
        let environment =
            LaunchEnvironment::from_pairs([("DEEPSEEK_API_KEY", "sk-fixture-export-secret")]);

        let exported = render_session_export(
            &store,
            &task_id,
            &environment,
            &data_dir,
            "DEEPSEEK_API_KEY",
            true,
        )
        .await
        .expect("export is built");

        assert!(!exported.contains("sk-fixture-export-secret"), "{exported}");
        assert!(exported.contains("[REDACTED]"), "{exported}");
        for line in exported.lines() {
            serde_json::from_str::<serde_json::Value>(line).expect("JSONL line stays valid JSON");
        }
        Arc::try_unwrap(store)
            .expect("store consumers released")
            .close()
            .await
            .expect("close store");
    }

    #[tokio::test]
    async fn g08_undo_restores_only_files_whose_hash_is_unchanged() {
        let fixture = undo_fixture().await;
        let store = &fixture.store;
        let action = latest_undo_action(
            store,
            &fixture.task_id,
            &fixture.project_id,
            &fixture.workspace,
        )
        .await
        .expect("unchanged file produces an undo proposal");
        let harness_tools::CodingToolAction::WriteFile {
            path,
            content,
            expected_hash,
        } = action
        else {
            panic!("undo must use the normal write_file action");
        };
        assert_eq!(path, "src/undo.txt");
        assert_eq!(content, "before\n");
        assert_eq!(expected_hash, fixture.original_receipt.after_hash);

        std::fs::write(fixture.workspace.join(&path), "newer manual edit\n").expect("new edit");
        let stale = latest_undo_action(
            store,
            &fixture.task_id,
            &fixture.project_id,
            &fixture.workspace,
        )
        .await
        .expect_err("changed file is refused before approval");

        assert!(stale.contains("current hash differs"), "{stale}");
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.join(path)).expect("current file"),
            "newer manual edit\n"
        );
        Arc::try_unwrap(fixture.store)
            .expect("store consumers released")
            .close()
            .await
            .expect("close store");
    }

    #[tokio::test]
    async fn g08_undo_is_an_approved_action_with_its_own_receipt() {
        let UndoFixture {
            temp,
            store: previous_store,
            workspace,
            project_id,
            task_id,
            source_session,
            original_receipt,
        } = undo_fixture().await;
        let store = reopen_undo_store(&temp, previous_store).await;
        let fixture = UndoContext {
            workspace,
            project_id,
            task_id,
            source_session,
            original_receipt,
        };
        let (undo_session, action) = admit_undo_input(&fixture, &store).await;
        let view = run_undo_through_approval(&fixture, &store, undo_session.clone(), action).await;
        let undo_receipt = view.receipt.expect("undo's own receipt");
        assert_ne!(
            undo_receipt.tool_execution_id,
            fixture.original_receipt.tool_execution_id
        );
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.join("src/undo.txt")).expect("restored"),
            "before\n",
        );
        assert_eq!(
            store
                .load_receipts(&undo_session)
                .await
                .expect("undo receipt list")
                .len(),
            1
        );
        Arc::try_unwrap(store)
            .expect("store consumers released")
            .close()
            .await
            .expect("close store");
    }

    #[tokio::test]
    async fn g08_continue_picks_the_newest_session_of_this_project_only() {
        let temp = tempfile::tempdir().expect("temporary root");
        let home = temp.path().join("home");
        let project_a = temp.path().join("project-a");
        let project_b = temp.path().join("project-b");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&project_a).expect("project A");
        std::fs::create_dir_all(&project_b).expect("project B");
        let environment =
            LaunchEnvironment::from_pairs([("HA_HOME", home.to_string_lossy().into_owned())]);
        let context_a = bootstrap::resolve(LaunchRequest {
            cwd: None,
            caller_dir: project_a.clone(),
            platform: HostPlatform::current(),
            environment: environment.clone(),
            explicit_data_dir: None,
        })
        .expect("project A context");
        let context_b = bootstrap::resolve(LaunchRequest {
            cwd: None,
            caller_dir: project_b.clone(),
            platform: HostPlatform::current(),
            environment: environment.clone(),
            explicit_data_dir: None,
        })
        .expect("project B context");
        let channel = SessionChannel::new();
        let service_a = AgentSessionService::new(&context_a, environment.clone(), channel.sender());
        let store_a = Arc::new(
            SqliteStore::open_writer(WriterOpenOptions::new(
                context_a.project_store_dir(),
                harness_types::HostId::generate(),
            ))
            .await
            .expect("project A store"),
        );
        let oldest = harness_types::SessionId::generate();
        record_project_session(&store_a, &project_a, oldest).await;
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let newest = harness_types::SessionId::generate();
        record_project_session(&store_a, &project_a, newest.clone()).await;
        Arc::try_unwrap(store_a)
            .expect("project A store consumers released")
            .close()
            .await
            .expect("close project A store");

        let store_b = Arc::new(
            SqliteStore::open_writer(WriterOpenOptions::new(
                context_b.project_store_dir(),
                harness_types::HostId::generate(),
            ))
            .await
            .expect("project B store"),
        );
        let foreign = harness_types::SessionId::generate();
        record_project_session(&store_b, &project_b, foreign.clone()).await;
        Arc::try_unwrap(store_b)
            .expect("project B store consumers released")
            .close()
            .await
            .expect("close project B store");

        assert_eq!(
            service_a.newest_project_session().expect("continue lookup"),
            newest
        );
        assert_ne!(
            service_a.newest_project_session().expect("repeat lookup"),
            foreign
        );
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

    /// The whole point of the turn grant: an action the user already allowed is not
    /// asked about again, and it is not allowed *silently* either - the transcript
    /// records one line, so a reader can still see what ran.
    #[tokio::test]
    async fn t08_an_allowed_action_is_not_asked_about_again_and_is_still_recorded() {
        let mut channel = SessionChannel::new();
        let gate = ChannelApprovalGate::new(channel.sender(), Duration::from_millis(30));
        gate.grant_for_run();

        // The timeout is 30 ms: if this path waited for an answer at all, the test
        // would observe an expiry instead of a grant.
        let answer = ApprovalGate::request(&gate, read_proposal("approval-read-1")).await;
        assert_eq!(
            answer,
            ApprovalAnswer::Granted,
            "a granted read is granted without asking"
        );

        let announced = channel.drain();
        assert!(
            !announced.iter().any(|event| matches!(
                event,
                SessionEvent::ApprovalRequired { .. } | SessionEvent::ApprovalExpired { .. }
            )),
            "the panel must not open for a read the user already allowed: {announced:?}"
        );
        assert!(
            announced.iter().any(|event| matches!(
                event,
                SessionEvent::Notice { message } if message.contains("read-only") && message.contains("list .")
            )),
            "the auto-granted read is recorded instead of being invisible: {announced:?}"
        );
    }

    /// The measured complaint, as a contract: a turn of shell commands used to ask
    /// about every one of them, because `run_process` is not a read-only kind and the
    /// old grant could not cover it. The turn grant covers it - and the transcript
    /// says which command ran without a panel.
    #[tokio::test]
    async fn t08_the_turn_grant_covers_a_command_and_a_patch_without_a_panel() {
        let mut channel = SessionChannel::new();
        let gate = ChannelApprovalGate::new(channel.sender(), Duration::from_millis(30));
        gate.grant_for_run();

        let command = ApprovalProposal {
            action: "RunProcess".to_owned(),
            summary: "run git log -1 --stat --format=fuller".to_owned(),
            ..proposal("approval-run-1")
        };
        assert_eq!(
            ApprovalGate::request(&gate, command).await,
            ApprovalAnswer::Granted,
            "a command the user allowed for the turn does not wait for an answer"
        );
        assert_eq!(
            ApprovalGate::request(&gate, proposal("approval-write-1")).await,
            ApprovalAnswer::Granted,
            "and neither does the patch that follows it"
        );

        let announced = channel.drain();
        assert!(
            !announced.iter().any(|event| matches!(
                event,
                SessionEvent::ApprovalRequired { .. } | SessionEvent::ApprovalExpired { .. }
            )),
            "no panel opens while the grant is open: {announced:?}"
        );
        for summary in [
            // The renderer adds the `[info] ` marker; a message that carried its own
            // was shown as `[info] [info] allowed by ...`.
            "allowed by mode turn-grant: run git log -1 --stat --format=fuller",
            "allowed by mode turn-grant: patch src/parser.rs",
        ] {
            assert!(
                announced.iter().any(|event| matches!(
                    event,
                    SessionEvent::Notice { message } if message == summary
                )),
                "every auto-allowed action is recorded, not invisible: {announced:?}"
            );
        }
    }

    #[tokio::test]
    async fn g05_transcript_records_every_auto_allowed_action() {
        let mut channel = SessionChannel::new();
        let gate = ChannelApprovalGate::new(channel.sender(), Duration::from_millis(30));
        gate.grant_for_run();

        for proposal in [
            ApprovalProposal {
                action: "RunProcess".to_owned(),
                summary: "run cargo test --locked".to_owned(),
                ..proposal("g05-auto-1")
            },
            proposal("g05-auto-2"),
        ] {
            assert_eq!(
                ApprovalGate::request(&gate, proposal).await,
                ApprovalAnswer::Granted,
                "the explicit per-turn grant handles each action"
            );
        }

        let notices = channel
            .drain()
            .into_iter()
            .filter_map(|event| match event {
                SessionEvent::Notice { message } => Some(message),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            notices.len(),
            2,
            "one info row is recorded per allowed action"
        );
        assert!(
            notices.iter().all(|message| {
                message.starts_with("allowed by mode ")
                    && (message.contains("cargo test --locked")
                        || message.contains("patch src/parser.rs"))
            }),
            "{notices:?}"
        );
    }

    #[tokio::test]
    async fn g05_confirmed_rule_is_applied_after_the_approved_action_settles() {
        use harness_tools::{CodingToolAction, Decision, ToolPolicy};

        let mut channel = SessionChannel::new();
        let gate = Arc::new(ChannelApprovalGate::new(
            channel.sender(),
            Duration::from_secs(5),
        ));
        let policy = ToolPolicy::default().with_turn_rules(gate.turn_rules());
        let action = CodingToolAction::RunProcess {
            executable: "cargo".to_owned(),
            args: vec!["test".to_owned(), "--locked".to_owned()],
            timeout_ms: 5_000,
            isolation: harness_tools::IsolationMode::BestEffort,
            env: Vec::new(),
        };
        assert_eq!(policy.decide(&action), Decision::Ask);

        let asking = Arc::clone(&gate);
        let mut proposed = proposal("g05-delayed-rule");
        proposed.action = "RunProcess".to_owned();
        proposed.summary = "run cargo test --locked".to_owned();
        proposed.rule_pattern = "run_process(cargo test --locked)".to_owned();
        let handle =
            tokio::spawn(async move { ApprovalGate::request(asking.as_ref(), proposed).await });
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
        let request_id = request_id.expect("the proposed tool reaches the approval panel");
        gate.stage_always_allow(&request_id, "run_process(cargo test --locked)")
            .expect("the displayed rule is staged after confirmation");
        assert!(gate.answer(&request_id, ApprovalDecision::Granted));
        assert_eq!(
            handle.await.expect("approval request"),
            ApprovalAnswer::Granted
        );
        assert_eq!(
            policy.decide(&action),
            Decision::Ask,
            "do not stale the pending grant"
        );

        ApprovalGate::action_completed(gate.as_ref(), &request_id);
        assert_eq!(
            policy.decide(&action),
            Decision::Allow {
                reason: "rule run_process(cargo test --locked)".to_owned()
            },
            "the confirmed pattern takes effect for later calls in the same turn"
        );
    }

    /// Before the user says so, a read keeps its panel - the grant is opt-in, not a
    /// default.
    #[tokio::test]
    async fn t08_a_read_is_asked_about_until_the_user_allows_the_turn() {
        let mut channel = SessionChannel::new();
        let gate = ChannelApprovalGate::new(channel.sender(), Duration::from_millis(30));
        assert!(
            !gate.granted_for_run(),
            "a fresh gate has not been given anything"
        );

        let answer = ApprovalGate::request(&gate, read_proposal("approval-read-2")).await;
        assert_eq!(answer, ApprovalAnswer::Expired);
        let announced = channel.drain();
        assert!(
            announced.iter().any(|event| matches!(
                event,
                SessionEvent::ApprovalRequired { request_id, read_only, .. }
                    if request_id == "approval-read-2" && *read_only
            )),
            "the panel opens and says the action is read-only: {announced:?}"
        );

        // The same for a command: nothing about the wider grant makes a process
        // run unasked before the user gives it.
        let answer = ApprovalGate::request(
            &gate,
            ApprovalProposal {
                action: "RunProcess".to_owned(),
                summary: "run git log -1".to_owned(),
                ..proposal("approval-run-2")
            },
        )
        .await;
        assert_eq!(answer, ApprovalAnswer::Expired);
    }

    /// Answering the panel with `a` grants the pending action *and* opens the gate;
    /// answering `y` does neither beyond the one action.
    #[tokio::test]
    async fn t08_the_wider_answer_grants_the_pending_action_and_the_run() {
        let mut channel = SessionChannel::new();
        let gate = Arc::new(ChannelApprovalGate::new(
            channel.sender(),
            Duration::from_secs(5),
        ));
        let asking = Arc::clone(&gate);
        let handle = tokio::spawn(async move {
            ApprovalGate::request(asking.as_ref(), read_proposal("approval-read-3")).await
        });

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
        assert!(gate.answer(&request_id, ApprovalDecision::GrantForRun));
        assert_eq!(
            handle.await.expect("asking task"),
            ApprovalAnswer::Granted,
            "the action in front of the user runs; offering the grant and then blocking it would be a lie"
        );
        assert!(
            gate.granted_for_run(),
            "and the run now runs without asking"
        );

        gate.clear_grant_for_run();
        assert!(
            !gate.granted_for_run(),
            "clearing the grant closes the gate again"
        );
        let reopened = gate.clone();
        let after = tokio::spawn(async move {
            ApprovalGate::request(reopened.as_ref(), read_proposal("approval-read-4")).await
        });
        assert_eq!(
            after.await.expect("second asking task"),
            ApprovalAnswer::Expired,
            "a read after the run ends waits for an answer again"
        );
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
