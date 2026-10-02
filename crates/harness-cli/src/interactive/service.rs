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
use harness_store_sqlite::SqliteStore;
use harness_tools::{
    ApprovalAnswer, ApprovalGate, ApprovalMode, ApprovalProposal, CodingToolAction, IsolationMode,
    PolicyMode, ToolExecutionService, ToolOutput, ToolPatternRule, ToolPolicyRules, TurnDriver,
    TurnLimits, TurnObserver, TurnOptions, TurnProgress, TurnStop, coding_tool_schemas,
    execute_action_with_approval, observe_workspace, observed_file_hash, validate_tool_pattern,
};
use harness_types::{
    ErrorCode, InputId, QuestionId, RequestId, SessionId, SourceAuthority, TaskId,
};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

use super::attachments;
use super::bootstrap::LaunchContext;
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

    /// prime-agent's `a_model_switch_updates_the_model_and_level_atomically`:
    /// every snapshot a model call takes carries a consistent (model, level)
    /// pair while another thread keeps switching both.
    #[test]
    fn a_model_switch_updates_the_model_and_level_atomically() {
        use harness_providers::ThinkingLevel;
        let config = |model: &str| super::ProviderConfig {
            provider_id: "anthropic".to_owned(),
            protocol: "anthropic_messages".to_owned(),
            endpoint: String::new(),
            model: model.to_owned(),
            api_key_env: String::new(),
            thinking: String::new(),
            thinking_format: None,
            approval: "ask".to_owned(),
            allow_rules: Vec::new(),
            deny_rules: Vec::new(),
            model_price: None,
            context_window_tokens: 200_000,
            context_window_notice: None,
            output_reservation_tokens: 0,
            compaction_reserve_tokens: 0,
            max_retry_after_seconds: 0,
            hooks: Vec::new(),
            mcp_servers: std::collections::BTreeMap::new(),
            project_trusted: false,
            bell: false,
            agents_default_model: None,
            queue_modes: (None, None),
            routing: super::super::config::Routing::default(),
            credential: super::CredentialSource::Environment {
                variable: String::new(),
            },
            service_tier: None,
        };
        let live = std::sync::Arc::new(super::LiveTurn::default());
        live.set_model_and_level(config("model-a"), Some(ThinkingLevel::High));
        let writer = std::sync::Arc::clone(&live);
        let switcher = std::thread::spawn(move || {
            for _ in 0..2_000 {
                writer.set_model_and_level(config("model-a"), Some(ThinkingLevel::High));
                writer.set_model_and_level(config("model-b"), Some(ThinkingLevel::Off));
            }
        });
        let mut mixed = 0u32;
        for _ in 0..20_000 {
            let snapshot = live.snapshot();
            let model = snapshot
                .config
                .map(|config| config.model)
                .unwrap_or_default();
            mixed += u32::from(
                (model == "model-a" && snapshot.level != Some(ThinkingLevel::High))
                    || (model == "model-b" && snapshot.level != Some(ThinkingLevel::Off)),
            );
        }
        switcher.join().expect("switcher");
        assert_eq!(
            mixed, 0,
            "every snapshot carries a consistent (model, level) pair"
        );
    }
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
    /// `/mcp add | list | get | remove`: change the servers the app manages.
    fn manage_mcp(&mut self, _args: &[String]) -> Result<Vec<String>, String> {
        Err("this backend does not manage MCP servers".to_owned())
    }
    fn agents_summary(&self) -> Vec<String> {
        vec!["no delegated workers have run in this session".to_owned()]
    }
    /// `/agents messages`: what the agents of this session told each other.
    fn agent_exchanges(&self) -> Vec<String> {
        vec!["no delegated workers have run in this session".to_owned()]
    }
    fn skills_summary(&self) -> Vec<String> {
        vec!["skill catalog is unavailable".to_owned()]
    }
    /// The `/skills` panel as structured rows.
    fn skills_rows(&self) -> Vec<super::events::RefLine> {
        self.skills_summary()
            .into_iter()
            .map(super::events::RefLine::Text)
            .collect()
    }
    /// prime-agent's `/import <path.jsonl>`: start a new conversation that
    /// continues the session file at `path`.
    fn import_session(&mut self, _path: &str) -> Result<String, String> {
        Err("this backend cannot import sessions".to_owned())
    }
    /// Park the conversation until `until`: prime-agent's quota park, as a
    /// durable one-shot job that wakes it with the resume prompt.
    fn park_until(&mut self, _until: chrono::DateTime<chrono::Utc>) -> Result<String, String> {
        Err("this backend cannot park a session".to_owned())
    }
    /// `/tree <n> --summarize [focus]`: the next turn summarises the turns the
    /// switch leaves. Called before [`Self::switch_to`].
    fn plan_branch_summary(&mut self, _focus: Option<String>) -> Result<(), String> {
        Err("this backend keeps no conversation".to_owned())
    }
    /// prime-agent's tree label on the turn of `session`; an empty label clears it.
    fn label_turn(&mut self, _session: &str, _label: &str) -> Result<(), String> {
        Err("this backend keeps no conversation".to_owned())
    }
    /// prime-agent's `/logs`: where the app writes its logs and what is there.
    fn logs(&self) -> Vec<String> {
        vec!["No logs written yet.".to_owned()]
    }
    /// Skills and prompt commands, for the slash-command menu.
    fn menu_commands(&self) -> Vec<super::commands::MenuCommand> {
        Vec::new()
    }
    /// prime-agent's `_expandSkillCommand`: `/skill:<name> [args]` as the message
    /// the model is sent - the skill's instructions in a `<skill>` block, then the
    /// arguments - so the skill is part of the conversation like anything else.
    fn expand_skill(&self, _name: &str, _arguments: &str) -> Result<String, String> {
        Err("this backend cannot run skills".to_owned())
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
    /// The system prompt the last turn sent, as prime-agent's `/system-prompt` shows it.
    fn system_prompt(&self) -> Vec<String> {
        vec!["no turn has run in this session yet".to_owned()]
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
    /// Move to the next (or previous) scoped model: `/model next|prev`, Alt+M.
    fn cycle_model(&mut self, _forward: bool) -> Result<String, String> {
        Err("this backend does not support model switching".to_owned())
    }
    /// `/scoped-models`: show the scope, or save a new one (`clear` drops it).
    fn scoped_models(&mut self, _argument: Option<&str>) -> Result<Vec<String>, String> {
        Err("this backend does not support model switching".to_owned())
    }
    /// The agent's heartbeats whose time has come, advanced to their next run.
    fn due_heartbeats(&mut self) -> Vec<super::heartbeat::Due> {
        Vec::new()
    }
    /// `/schedule`: list, add, pause, resume or cancel this conversation's jobs.
    fn schedule(&mut self, _argument: Option<&str>) -> Result<Vec<String>, String> {
        Err("this backend keeps no schedules".to_owned())
    }
    /// The session's input and output tokens so far, for autonomous budgets.
    fn session_tokens(&self) -> u64 {
        0
    }
    /// How many delegated children are still working.
    fn running_children(&self) -> usize {
        0
    }
    /// Whether a schedule or a heartbeat will start a turn on its own.
    fn has_scheduled_work(&self) -> bool {
        false
    }
    /// The conversation this session is in.
    fn conversation_id(&self) -> Option<String> {
        None
    }
    /// Where `rlm.create_session` starts a separate top-level session; only a
    /// background agent has one.
    fn set_session_host(&mut self, _host: Arc<dyn super::agents::SessionHost>) {}
    /// Run autonomous quality gates in the workspace; the verdict arrives as
    /// [`SessionEvent::GatesChecked`].
    fn run_gates(&mut self, _job: super::autonomous::GateJob) -> Result<(), String> {
        Err("this backend cannot run quality gates".to_owned())
    }
    /// Stop quality gates that are running.
    fn cancel_gates(&mut self) {}
    /// Choose the thinking level for the next turns.
    fn set_thinking(&mut self, _level: &str) -> Result<String, String> {
        Err("this backend does not support thinking levels".to_owned())
    }
    /// The level the next turn uses, for the status line.
    fn thinking_level(&self) -> Option<String> {
        None
    }
    /// The levels the model in use offers, for the `/effort` menu.
    fn thinking_levels(&self) -> Vec<String> {
        Vec::new()
    }
    /// prime-agent's `/tier <tier>` (and `/fast`): the tier for the next model
    /// calls, kept with the conversation and as the global default.
    fn set_service_tier(&mut self, _tier: &str) -> Result<String, String> {
        Err("Current model does not support service tiers".to_owned())
    }
    /// The tier in force (clamped to the model) and the tiers the model takes.
    fn service_tier(&self) -> (String, Vec<&'static str>) {
        ("default".to_owned(), vec!["default"])
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
    /// evaluated against the *current* credential: after `/login` saves one, this
    /// turns `None` and the very next message reaches the provider.
    fn provider_problem(&self) -> Option<String> {
        None
    }
    /// The project identity this workspace is scoped to, for `/status`.
    ///
    /// The app otherwise never shows it: the projects directory is named after a
    /// digest, so an operator who wants to inspect what a turn stored has no other
    /// way to name the project. This is the missing link, answered on demand
    /// because resolving it needs the store.
    fn project_id(&mut self) -> Option<String> {
        None
    }
    /// Save a provider's credential so this session and the next launch can use it.
    ///
    /// The file is the source of truth because the credential resolver re-reads it
    /// at call time; nothing has to restart for the next turn to use it. The
    /// returned source carries the source *name*, never the value.
    fn save_credential(
        &mut self,
        _provider: &str,
        _credential: &credentials::Credential,
    ) -> Result<CredentialSource, String> {
        Err("this backend cannot save a provider credential".to_owned())
    }
    /// Remove a provider's saved credential; whether there was one.
    fn remove_credential(&mut self, _provider: &str) -> Result<bool, String> {
        Err("this backend cannot remove a provider credential".to_owned())
    }
    /// The providers with a saved credential, and what kind each is.
    fn stored_credentials(&self) -> Vec<(String, &'static str)> {
        Vec::new()
    }
    /// The providers the delegated children have a login of their own for.
    fn stored_subagent_credentials(&self) -> Vec<(String, &'static str)> {
        Vec::new()
    }
    /// Whose login `/login`, `/logout` and a browser sign-in act on from now:
    /// the main model's, or the delegated children's own.
    fn set_credential_scope(&mut self, _scope: credentials::Scope) {}
    /// The catalog models a delegated child can use: those with a key in its
    /// own login or the main one.
    fn subagent_model_options(&self) -> Vec<(String, String)> {
        Vec::new()
    }
    /// The provider the next turn uses.
    fn provider_id(&self) -> Option<String> {
        None
    }
    /// `provider/model` and its name, for every catalog model whose provider has a
    /// credential: what the `/model` menu offers.
    fn model_options(&self) -> Vec<(String, String)> {
        Vec::new()
    }
    /// Deliver another agent's message into the running turn at its next step,
    /// read as written rather than as the user's steering correction.
    fn deliver_message(&mut self, _text: &str) -> Result<(), String> {
        Err("no running turn can take a message".to_owned())
    }
    /// Stop the delegated child named `selector`, or every one for `all`.
    fn stop_agents(&mut self, _selector: &str) -> Result<String, String> {
        Err("this backend has no delegated children".to_owned())
    }
    /// prime-agent's `getFullscreen` / `getFullscreenMouse` for this launch.
    fn fullscreen_prefs(&self) -> super::tui::fullscreen::Prefs {
        super::tui::fullscreen::Prefs {
            enabled: false,
            mouse: false,
        }
    }
    /// prime-agent's `setFullscreen`: keep the choice for later launches.
    fn set_fullscreen(&mut self, _enabled: bool) -> Result<(), String> {
        Ok(())
    }
    /// `/subagent-effort`: report the thinking level children run at, save
    /// `subagentDefaultThinking`, or remove it so children run at this agent's.
    fn subagent_effort(&mut self, _change: SubagentSetting) -> Result<String, String> {
        Err("this backend has no delegated children".to_owned())
    }
    /// `/subagent-model`: report the model children run on and where it comes
    /// from, save prime-agent's `subagentDefaultModel`, or remove it so children
    /// run on this agent's model.
    fn subagent_model(&mut self, _change: SubagentSetting) -> Result<String, String> {
        Err("this backend has no delegated children".to_owned())
    }
    /// prime-agent's `/rlm-max-depth`: `None` reports the value and its source,
    /// `Some((n, global))` sets it for this chat (and as the global default).
    /// The outcome arrives as a notice.
    fn rlm_max_depth(&mut self, _change: Option<(u32, bool)>) -> Result<(), String> {
        Err("this backend has no delegated children".to_owned())
    }
    /// How the steering and follow-up lanes are drained (`[queue]`).
    fn queue_modes(&self) -> (super::queue::QueueMode, super::queue::QueueMode) {
        Default::default()
    }
    /// List the turns of this conversation, oldest first; they arrive as
    /// [`SessionEvent::TurnsListed`].
    fn list_turns(&mut self, _purpose: super::events::TurnsPurpose) -> Result<(), String> {
        Err("this backend keeps no conversation".to_owned())
    }
    /// Start a new conversation that continues from the turn of `session`:
    /// before it (`/fork`), or after it (`/clone`).
    fn fork(&mut self, _session: &str, _before: bool) -> Result<(), String> {
        Err("this backend keeps no conversation".to_owned())
    }
    /// `/clone`: a new conversation with this one's whole history.
    fn clone_conversation(&mut self) -> Result<(), String> {
        Err("this backend keeps no conversation".to_owned())
    }
    /// `/tree`: continue this conversation from after the turn of `session`.
    fn switch_to(&mut self, _session: &str) -> Result<(), String> {
        Err("this backend keeps no conversation".to_owned())
    }
    /// Ask a `/btw` side question; the answer arrives as
    /// [`SessionEvent::SideAnswer`].
    fn side_question(&mut self, _question: &str) -> Result<(), String> {
        Err("side questions are not available here".to_owned())
    }
    /// Start a browser sign-in; returns the URL to open. The outcome arrives as
    /// [`SessionEvent::LoginFinished`].
    fn begin_sign_in(&mut self, _provider: &str) -> Result<String, String> {
        Err("this backend cannot sign in".to_owned())
    }
    /// Finish the sign-in in progress with a pasted redirect URL or code.
    fn finish_sign_in(&mut self, _pasted: &str) -> Result<(), String> {
        Err("no sign-in is in progress".to_owned())
    }
    /// Stop waiting for the browser.
    fn cancel_sign_in(&mut self) {}
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

    /// Close every approval still waiting: the turn was canceled, so each is
    /// refused and its panel closed rather than left counting down.
    pub fn cancel_pending(&self) {
        let pending = self
            .pending
            .lock()
            .map(|mut pending| std::mem::take(&mut *pending))
            .unwrap_or_default();
        for (request_id, answer) in pending {
            let _ = answer.send(ApprovalAnswer::Denied);
            let _ = self
                .sender
                .send(SessionEvent::ApprovalExpired { request_id });
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
    /// How the model takes a thinking level, from the catalog.
    pub thinking_format: Option<String>,
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
    /// `[agents] default_model`: the model a delegated child runs on by default.
    pub agents_default_model: Option<String>,
    /// `[queue]` modes, as written.
    pub queue_modes: (Option<String>, Option<String>),
    /// `[routing]`: scoped, auxiliary, backup and image models.
    pub routing: super::config::Routing,
    /// Name of the source that holds the key; never the key.
    pub credential: CredentialSource,
    /// The service tier the session asked for (`/tier`, `/fast`), before it is
    /// clamped to what the model takes; `None` sends no tier.
    pub service_tier: Option<String>,
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
    let mut resolved =
        super::config::resolve_layers(user_path, project_root, environment, overrides)
            .map_err(|error| error.to_string())?;
    super::config::apply_catalog_context_window(&mut resolved, data_dir);
    if !matches!(
        resolved.provider.protocol.as_str(),
        "openai_chat" | "anthropic_messages" | "openai_responses" | "openai_codex"
    ) {
        return Err(format!(
            "unsupported provider protocol {:?}",
            resolved.provider.protocol
        ));
    }
    let credential = credentials::source_for(
        environment,
        data_dir,
        &resolved.provider.id,
        &resolved.provider.api_key_env,
    );
    let Some(credential) = credential else {
        return Err(format!(
            "provider setup is incomplete: log in to {} with /login{}. Nothing was sent and no fixture answer was substituted.",
            resolved.provider.id,
            if resolved.provider.api_key_env.is_empty() {
                String::new()
            } else {
                format!(" or set {}", resolved.provider.api_key_env)
            }
        ));
    };
    // A price in the config wins; otherwise the catalog's, as prime-agent prices
    // every model from its registry.
    let model_price = resolved
        .model_prices
        .get(&resolved.provider.model)
        .copied()
        .or_else(|| {
            super::providers::Catalog::load(data_dir)
                .find(&format!(
                    "{}/{}",
                    resolved.provider.id, resolved.provider.model
                ))
                .and_then(|model| model.cost)
                .filter(|cost| cost.input > 0.0 || cost.output > 0.0)
                .map(|cost| ModelPrice {
                    input_per_mtok: cost.input,
                    output_per_mtok: cost.output,
                })
        });
    let max_retry_after_seconds = resolved.retry_after_max_seconds;
    Ok(ProviderConfig {
        provider_id: resolved.provider.id,
        protocol: resolved.provider.protocol,
        endpoint: resolved.provider.endpoint,
        model: resolved.provider.model,
        api_key_env: resolved.provider.api_key_env,
        thinking: resolved.provider.thinking,
        thinking_format: resolved.provider.thinking_format,
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
        agents_default_model: resolved.agents_default_model,
        queue_modes: resolved.queue_modes,
        routing: resolved.routing,
        credential,
        service_tier: None,
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
    let path = credentials::resolve_file(environment, data_dir);
    credentials::stored(&path)
        .map(|_| ())
        .map_err(|error| error.to_string())
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
    match credentials::source_for(
        environment,
        data_dir,
        &resolved.provider.id,
        &resolved.provider.api_key_env,
    ) {
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
            "Provider: no credential for {}; log in with /login",
            resolved.provider.id
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
    match credentials::source_for(
        environment,
        data_dir,
        &resolved.provider.id,
        &resolved.provider.api_key_env,
    ) {
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
            "Provider: not ready (log in to {} with /login)",
            resolved.provider.id
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
/// Two ordered sources, and the order is the whole contract, as in prime-agent:
/// what `/login` saved for the provider wins, the provider's environment variables
/// are the fallback. The file is re-read on every call, which is why logging in
/// takes effect in a running app without a restart, and why a sign-in token is
/// refreshed here when it is about to expire.
pub struct EnvironmentCredential {
    provider: String,
    variable: String,
    data_dir: PathBuf,
    environment: LaunchEnvironment,
    scope: credentials::Scope,
}

impl EnvironmentCredential {
    /// Bind the resolver to one provider, its configured variable and one data root.
    #[must_use]
    pub fn new(
        provider: impl Into<String>,
        variable: impl Into<String>,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            provider: provider.into(),
            variable: variable.into(),
            data_dir,
            environment: LaunchEnvironment::capture(),
            scope: credentials::Scope::Main,
        }
    }

    /// Read a delegated child's own login first.
    #[must_use]
    pub const fn with_scope(mut self, scope: credentials::Scope) -> Self {
        self.scope = scope;
        self
    }

    /// The file `/login` saves to and this resolver reads first.
    #[must_use]
    pub fn file(&self) -> PathBuf {
        credentials::resolve_file(&self.environment, &self.data_dir)
    }
}

impl CredentialResolver for EnvironmentCredential {
    fn resolve(&self) -> Result<String, ProviderError> {
        if self.scope == credentials::Scope::Subagent {
            let own = credentials::scoped_file(
                &self.environment,
                &self.data_dir,
                credentials::Scope::Subagent,
            );
            if let Ok(Some(credential)) = credentials::load(&own, &self.provider) {
                return super::oauth::current_secret(&own, &self.provider, credential)
                    .map_err(|message| ProviderError::new(ErrorCode::SecretNotGranted, message));
            }
        }
        let path = self.file();
        match credentials::load(&path, &self.provider) {
            Ok(Some(credential)) => {
                return super::oauth::current_secret(&path, &self.provider, credential)
                    .map_err(|message| ProviderError::new(ErrorCode::SecretNotGranted, message));
            }
            Ok(None) => {}
            Err(error) => {
                return Err(ProviderError::new(
                    ErrorCode::SecretNotGranted,
                    format!("the saved credential file cannot be used: {error}"),
                ));
            }
        }
        std::iter::once(self.variable.as_str())
            .chain(
                super::providers::env_variables(&self.provider)
                    .iter()
                    .copied(),
            )
            .filter(|name| !name.is_empty())
            .find_map(|name| {
                self.environment
                    .value(name)
                    .filter(|value| !value.is_empty())
                    .map(|value| value.to_string_lossy().into_owned())
            })
            .ok_or_else(|| {
                ProviderError::new(
                    ErrorCode::SecretNotGranted,
                    format!(
                        "no credential for {}: log in with /login{}",
                        self.provider,
                        if self.variable.is_empty() {
                            String::new()
                        } else {
                            format!(" or set {}", self.variable)
                        }
                    ),
                )
            })
    }
}

/// The adapter for one resolved provider: the wire format its protocol names,
/// with the thinking level and the headers the provider wants.
/// What the user changed while a turn runs - the thinking level and the model -
/// which the turn's next model call uses, as prime-agent's agent reads its
/// model and level for every request.
///
/// The model and the level share one lock, as prime-agent's
/// `set_model_and_thinking_level` keeps them: a model call takes both in one
/// snapshot, so a switch made while the turn runs never pairs the new model
/// with the old level.
#[derive(Debug, Default)]
pub struct LiveTurn {
    selection: Mutex<LiveSelection>,
    /// The session model a turn left for the backup model, until it answers again.
    left_primary: Arc<Mutex<Option<String>>>,
}

/// The model and the thinking level a running turn's next call uses.
#[derive(Clone, Debug, Default)]
struct LiveSelection {
    level: Option<harness_providers::ThinkingLevel>,
    config: Option<ProviderConfig>,
}

impl LiveTurn {
    /// A new turn starts from what it resolved.
    fn start(&self, level: harness_providers::ThinkingLevel) {
        if let Ok(mut current) = self.selection.lock() {
            *current = LiveSelection {
                level: Some(level),
                config: None,
            };
        }
    }

    fn set_level(&self, level: harness_providers::ThinkingLevel) {
        if let Ok(mut current) = self.selection.lock() {
            current.level = Some(level);
        }
    }

    /// prime-agent's `set_model_and_thinking_level`: the model and the level
    /// in one lock acquisition, so a call admitted mid-switch never sees the
    /// new model with the old level. `None` keeps the level in force, as
    /// prime-agent's `setModel` re-applies it.
    fn set_model_and_level(
        &self,
        config: ProviderConfig,
        level: Option<harness_providers::ThinkingLevel>,
    ) {
        if let Ok(mut current) = self.selection.lock() {
            current.config = Some(config);
            if level.is_some() {
                current.level = level;
            }
        }
    }

    /// Change the model the turn is on, from what it holds now (or `base`).
    fn update_config(&self, base: ProviderConfig, change: impl FnOnce(&mut ProviderConfig)) {
        if let Ok(mut current) = self.selection.lock() {
            let mut config = current.config.clone().unwrap_or(base);
            change(&mut config);
            current.config = Some(config);
        }
    }

    /// The model and the level, read together.
    fn snapshot(&self) -> LiveSelection {
        self.selection
            .lock()
            .map(|current| current.clone())
            .unwrap_or_default()
    }
}

/// The models a delegated child may ask for: any catalog model whose provider
/// has a credential, as prime-agent's `_resolveRlmSubagentSetting` accepts any
/// authenticated catalog model.
struct ServiceChildModels {
    base: ProviderConfig,
    level: harness_providers::ThinkingLevel,
    environment: LaunchEnvironment,
    data_dir: PathBuf,
    session: String,
    /// Where `settings.json` is: `subagentDefaultModel` is read at every spawn,
    /// so `/subagent-model` takes effect for the next child.
    config_file: PathBuf,
    /// `[agents] default_model`, read after the setting.
    configured: Option<String>,
}

impl super::delegation::ChildModels for ServiceChildModels {
    fn default_model(&self) -> Option<String> {
        subagent_default_model(&self.config_file).or_else(|| self.configured.clone())
    }

    fn resolve(&self, reference: &str) -> Result<super::delegation::ChildModel, String> {
        self.resolve_at(reference, None, false)
    }

    fn default_thinking(&self) -> Option<harness_providers::ThinkingLevel> {
        subagent_default_thinking(&self.config_file)
    }

    fn searchable(&self) -> Vec<super::delegation::SearchableModel> {
        catalog_models_scoped(
            &self.environment,
            &self.data_dir,
            credentials::Scope::Subagent,
        )
        .into_iter()
        .map(|model| super::delegation::SearchableModel {
            provider: model.provider,
            id: model.id,
            name: model.name,
        })
        .collect()
    }

    fn resolve_at(
        &self,
        reference: &str,
        level: Option<harness_providers::ThinkingLevel>,
        strict: bool,
    ) -> Result<super::delegation::ChildModel, String> {
        let (config, entry) = super::routing::resolve_scoped(
            &self.base,
            reference,
            &self.environment,
            &self.data_dir,
            credentials::Scope::Subagent,
        )
        .map_err(|unusable| match unusable {
                    super::routing::Unusable::NotInCatalog => {
                        format!("Requested subagent model \"{reference}\" is not available")
                    }
                    super::routing::Unusable::NoCredential(provider) => format!(
                        "Requested subagent model \"{reference}\" failed authentication preflight: log in to {provider} with /login"
                    ),
                })?;
        let price = config.model_price;
        let reasoning = Some(entry.reasoning_model());
        let level = match level {
            None => self.level,
            Some(level) => {
                let offered = harness_providers::thinking::supported_levels(reasoning);
                if strict && !offered.contains(&level) {
                    return Err(format!(
                        "Requested subagent thinking level \"{}\" is not supported by {}; it offers {}",
                        level.as_str(),
                        entry.reference(),
                        offered
                            .iter()
                            .map(|level| level.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                harness_providers::thinking::clamp(reasoning, level)
            }
        };
        let provider = LiveProvider::build_scoped(
            &config,
            level,
            &self.session,
            &self.data_dir,
            credentials::Scope::Subagent,
        )
        .map_err(|error| error.to_string())?;
        Ok(super::delegation::ChildModel {
            provider,
            reference: entry.reference(),
            price,
        })
    }
}

/// The turn's provider, rebuilt for the next call when `/model` or `/effort`
/// changed what it should be while the turn was running.
struct LiveProvider {
    live: Arc<LiveTurn>,
    base: ProviderConfig,
    session: String,
    data_dir: PathBuf,
    current: Mutex<(String, Arc<dyn ModelProvider>)>,
    /// Where calls go when the session model cannot take them.
    router: Option<Arc<super::routing::Router>>,
}

impl LiveProvider {
    fn new(
        live: Arc<LiveTurn>,
        base: ProviderConfig,
        level: harness_providers::ThinkingLevel,
        session: String,
        data_dir: PathBuf,
    ) -> Result<Self, ProviderError> {
        let provider = Self::build(&base, level, &session, &data_dir)?;
        Ok(Self {
            current: Mutex::new((Self::key(&base, level), provider)),
            live,
            base,
            session,
            data_dir,
            router: None,
        })
    }

    fn with_router(mut self, router: super::routing::Router) -> Self {
        self.router = Some(Arc::new(router));
        self
    }

    /// The router for this turn, from `[routing]`: the backup and image models
    /// when they can be used, and the usage wait.
    fn router_for(
        &self,
        sender: UnboundedSender<SessionEvent>,
        environment: &LaunchEnvironment,
        level: harness_providers::ThinkingLevel,
        max_attempts: u32,
    ) -> super::routing::Router {
        let routing = &self.base.routing;
        let primary = format!("{}/{}", self.base.provider_id, self.base.model);
        let catalog = super::providers::Catalog::load(&self.data_dir);
        // A model the catalog does not describe is taken at its word.
        let primary_takes_images = catalog
            .find(&primary)
            .is_none_or(|model| model.input.iter().any(|input| input == "image"));
        let handoff = |reference: &Option<String>| {
            reference.as_deref().map(|reference| {
                let (config, entry) = super::routing::resolve(
                    &self.base,
                    reference,
                    environment,
                    &self.data_dir,
                )
                .map_err(|unusable| match unusable {
                    super::routing::Unusable::NotInCatalog => {
                        format!("\"{reference}\" is not in the model catalog")
                    }
                    super::routing::Unusable::NoCredential(provider) => {
                        format!(
                            "\"{reference}\" has no credential; log in to {provider} with /login"
                        )
                    }
                })?;
                let provider = Self::build(&config, level, &self.session, &self.data_dir)
                    .map_err(|error| format!("\"{reference}\": {error}"))?;
                Ok((entry.reference(), provider))
            })
        };
        let backup = handoff(&routing.backup)
            .filter(|backup| !matches!(backup, Ok((reference, _)) if *reference == primary));
        super::routing::Router::new(
            sender,
            primary,
            primary_takes_images,
            backup,
            handoff(&routing.image),
            routing
                .wait_for_usage
                .then_some(super::routing::UsageWait::PRIME),
            max_attempts,
            Arc::clone(&self.live.left_primary),
        )
    }

    fn key(config: &ProviderConfig, level: harness_providers::ThinkingLevel) -> String {
        format!(
            "{}/{}/{}/{}",
            config.provider_id,
            config.model,
            level.as_str(),
            config.service_tier.as_deref().unwrap_or_default()
        )
    }

    fn build(
        config: &ProviderConfig,
        level: harness_providers::ThinkingLevel,
        session: &str,
        data_dir: &Path,
    ) -> Result<Arc<dyn ModelProvider>, ProviderError> {
        Self::build_scoped(config, level, session, data_dir, credentials::Scope::Main)
    }

    /// The provider as a scope's login reaches it: a delegated child's own
    /// login first.
    fn build_scoped(
        config: &ProviderConfig,
        level: harness_providers::ThinkingLevel,
        session: &str,
        data_dir: &Path,
        scope: credentials::Scope,
    ) -> Result<Arc<dyn ModelProvider>, ProviderError> {
        let capabilities = ModelCapabilities {
            provider_id: config.provider_id.clone(),
            model: config.model.clone(),
            supports_streaming: true,
            supports_tools: true,
            fixture: false,
        };
        let credentials = Arc::new(
            EnvironmentCredential::new(
                config.provider_id.clone(),
                config.credential_variable(),
                data_dir.to_path_buf(),
            )
            .with_scope(scope),
        );
        build_provider(config, credentials, capabilities, level, session, data_dir)
    }

    /// The provider for the next call: the current one, or a new one when the
    /// model or level changed. A change that cannot be built keeps the current
    /// provider, so a bad choice never breaks the running turn.
    fn provider(&self) -> Arc<dyn ModelProvider> {
        let LiveSelection { level, config } = self.live.snapshot();
        let config = config.unwrap_or_else(|| self.base.clone());
        let Ok(mut current) = self.current.lock() else {
            return Self::build(
                &self.base,
                level.unwrap_or_default(),
                &self.session,
                &self.data_dir,
            )
            .unwrap_or_else(|_| Arc::new(harness_providers::MockProvider::scripted(Vec::new())));
        };
        if let Some(level) = level {
            let key = Self::key(&config, level);
            if key != current.0
                && let Ok(provider) = Self::build(&config, level, &self.session, &self.data_dir)
            {
                *current = (key, provider);
            }
        }
        Arc::clone(&current.1)
    }
}

impl ModelProvider for LiveProvider {
    fn capabilities(&self) -> ModelCapabilities {
        self.provider().capabilities()
    }

    fn stream(
        &self,
        request: ProviderRequest,
        cancellation: CancellationToken,
    ) -> harness_providers::ProviderFuture {
        match &self.router {
            Some(router) => super::routing::collect(router.stream_events(
                self.provider(),
                request,
                cancellation,
            )),
            None => self.provider().stream(request, cancellation),
        }
    }

    fn stream_events(
        &self,
        request: ProviderRequest,
        cancellation: CancellationToken,
    ) -> harness_providers::ProviderEventStream {
        match &self.router {
            Some(router) => router.stream_events(self.provider(), request, cancellation),
            None => self.provider().stream_events(request, cancellation),
        }
    }
}

pub(super) fn build_provider(
    config: &ProviderConfig,
    credentials: Arc<dyn harness_providers::CredentialResolver>,
    capabilities: ModelCapabilities,
    thinking_level: harness_providers::ThinkingLevel,
    session: &str,
    data_dir: &Path,
) -> Result<Arc<dyn ModelProvider>, ProviderError> {
    // prime-agent's `opencode-headers.ts`: OpenCode wants to know the client and
    // the conversation.
    let extra_headers = if config.provider_id.starts_with("opencode") {
        vec![
            (
                "User-Agent".to_owned(),
                format!("ha/{}", env!("CARGO_PKG_VERSION")),
            ),
            ("x-opencode-session".to_owned(), session.to_owned()),
        ]
    } else {
        Vec::new()
    };
    // The model's levels, from the catalog as prime-agent reads them.
    let catalog_reasoning = super::providers::Catalog::load(data_dir)
        .find(&format!("{}/{}", config.provider_id, config.model))
        .map(super::providers::Model::reasoning_model);
    let chat_thinking_format = match config.thinking_format.as_deref() {
        Some("deepseek") => harness_providers::ThinkingFormat::DeepSeek,
        Some(_) => harness_providers::ThinkingFormat::ReasoningEffort,
        None => harness_providers::thinking::chat_format(&config.provider_id, &config.endpoint),
    };
    match config.protocol.as_str() {
        "openai_chat" => OpenAiChatAdapter::with_options(
            config.endpoint.clone(),
            credentials,
            capabilities,
            OpenAiChatOptions {
                thinking: Some(harness_providers::Thinking {
                    level: thinking_level,
                    format: chat_thinking_format,
                    model: catalog_reasoning,
                }),
                headers: extra_headers,
            },
        )
        .map(|adapter| Arc::new(adapter) as Arc<dyn ModelProvider>),
        "anthropic_messages" => {
            AnthropicMessagesAdapter::new(config.endpoint.clone(), credentials, capabilities).map(
                |adapter| {
                    Arc::new(
                        adapter
                            .with_thinking(Some(harness_providers::Thinking {
                                level: thinking_level,
                                format: harness_providers::ThinkingFormat::Anthropic,
                                model: catalog_reasoning,
                            }))
                            .with_headers(extra_headers),
                    ) as Arc<dyn ModelProvider>
                },
            )
        }
        "openai_responses" | "openai_codex" => harness_providers::OpenAiResponsesAdapter::new(
            config.endpoint.clone(),
            credentials,
            capabilities,
            if config.protocol == "openai_codex" {
                harness_providers::ResponsesFlavor::Codex {
                    originator: super::oauth::ORIGINATOR.to_owned(),
                }
            } else {
                harness_providers::ResponsesFlavor::Api
            },
            harness_providers::ResponsesOptions {
                // Only a model the catalog marks as reasoning is sent an effort.
                reasoning: catalog_reasoning
                    .filter(|model| model.reasoning)
                    .map(|model| harness_providers::thinking::clamp(Some(model), thinking_level)),
                headers: extra_headers,
                session_id: Some(session.to_owned()),
                // A tier the model does not take is sent as `default`.
                service_tier: super::service_tier::clamp(
                    config.service_tier.as_deref(),
                    &config.provider_id,
                    &config.protocol,
                    &config.model,
                ),
            },
        )
        .map(|adapter| Arc::new(adapter) as Arc<dyn ModelProvider>),
        _ => Err(ProviderError::new(
            ErrorCode::ProviderProtocol,
            "provider protocol is unsupported",
        )),
    }
}

/// A skill document without its `---` front matter.
fn strip_front_matter(content: &str) -> &str {
    let Some(rest) = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))
    else {
        return content;
    };
    rest.find("\n---").map_or(content, |end| {
        let after = &rest[end + 4..];
        after.split_once('\n').map_or("", |(_, body)| body)
    })
}

/// The usage line: how full the context is, and the provider's tightest limit
/// when its responses report one.
fn usage_label(context_tokens: u64, window: u64, provider_id: &str) -> String {
    use std::fmt::Write as _;
    let mut label = super::cost::context_label(context_tokens, window);
    if let Some(snapshot) = harness_providers::limits::latest(provider_id)
        && let Some(window) = snapshot.tightest()
        && let Some(used) = window.used_percent
    {
        let _ = write!(label, " · {} {used:.0}%", window.label);
    }
    label
}

/// Maps turn progress onto the UI vocabulary.
struct ChannelObserver {
    sender: UnboundedSender<SessionEvent>,
    cost_tracker: Arc<Mutex<CostTracker>>,
    model_price: Option<ModelPrice>,
    /// The model's context window, for the usage line.
    context_window: u64,
    /// Whose limits the usage line names.
    provider_id: String,
    auto_allowed_count: Arc<AtomicUsize>,
    bell: bool,
    /// Calls are executed serially by the turn driver. Keeping the current
    /// boundary here makes duration delivery O(1) and avoids a process-lifetime
    /// map keyed by a non-unique tool name.
    tool_started: Mutex<Vec<(String, String, Instant)>>,
}

impl TurnObserver for ChannelObserver {
    fn observe(&self, progress: TurnProgress) {
        let event = match progress {
            TurnProgress::TextDelta(text) => Some(SessionEvent::TextDelta { text }),
            TurnProgress::ThinkingDelta(text) => Some(SessionEvent::ThinkingDelta { text }),
            TurnProgress::StreamRestarted => Some(SessionEvent::StreamRestarted),
            TurnProgress::ToolProgress { call_id, text, .. } => {
                Some(SessionEvent::ToolProgress { call_id, text })
            }
            TurnProgress::Info(message) => {
                self.auto_allowed_count.fetch_add(1, Ordering::Relaxed);
                Some(SessionEvent::Notice { message })
            }
            TurnProgress::Notice(message) => Some(SessionEvent::Notice { message }),
            TurnProgress::ToolOutput { text, .. } => Some(SessionEvent::ToolOutput { text }),
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
                let _ = self.sender.send(SessionEvent::UsageUpdated {
                    label: usage_label(
                        prompt_tokens.saturating_add(completion_tokens),
                        self.context_window,
                        &self.provider_id,
                    ),
                });
                Some(SessionEvent::CostUpdated { label })
            }
            // Step boundaries are what the status bar counts (`step 2/8`).
            TurnProgress::StepStarted { step } => Some(SessionEvent::StepStarted { step }),
            TurnProgress::ToolStarted {
                name,
                call_id,
                summary,
                input,
            } => {
                if self.bell && name == "ask_user" {
                    let _ = self.sender.send(SessionEvent::Bell);
                }
                if let Ok(mut started) = self.tool_started.lock() {
                    started.push((call_id.clone(), name.clone(), Instant::now()));
                }
                Some(SessionEvent::ToolStarted {
                    name,
                    call_id,
                    summary,
                    input,
                })
            }
            TurnProgress::ToolSettled {
                name,
                call_id,
                ok,
                detail,
            } => {
                // The call settling is found by its id: calls of a parallel batch
                // settle in completion order. A host action has no id, and the
                // oldest open call of its name is the one settling.
                let elapsed = self
                    .tool_started
                    .lock()
                    .ok()
                    .and_then(|mut started| {
                        let index = started.iter().position(|(id, open, _)| {
                            if call_id.is_empty() {
                                open == &name
                            } else {
                                id == &call_id
                            }
                        })?;
                        Some(started.remove(index))
                    })
                    .map_or(Duration::ZERO, |(_, _, started)| started.elapsed());
                Some(SessionEvent::ToolSettled {
                    name,
                    call_id,
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
    /// The browser sign-in waiting for its code, if any.
    sign_in: Option<Arc<super::oauth::PendingLogin>>,
    /// Whose login the credential commands act on.
    credential_scope: credentials::Scope,
    /// The task of the conversation `/resume` opened, once it is read: that
    /// is the conversation this service continues, not `task_id`.
    resumed_task: Arc<Mutex<Option<String>>>,
    store_dir: PathBuf,
    /// Root that owns the credential file `/login` writes.
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
    system_prompt: Arc<Mutex<String>>,
    active_skills: Arc<Mutex<BTreeMap<String, harness_extensions::SkillActivation>>>,
    pending_mcp_elicitations: Arc<Mutex<HashMap<String, PendingMcpElicitationRequest>>>,
    mcp_status: Arc<Mutex<Vec<String>>>,
    /// The session's delegated children and the store they share with its turns.
    agents: Arc<super::delegation::SessionAgents>,
    /// The `/btw` side thread of this conversation: its questions and answers.
    side_thread: Arc<Mutex<Vec<(String, String)>>>,
    /// The fork the next turn starts, set by `/fork` and `/clone`.
    fork_plan: Option<ForkPlan>,
    /// The branch summary the next turn writes, set by `/tree <n> --summarize`.
    branch_plan: Option<super::branch_summary::BranchPlan>,
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
    /// The service tier `/tier` or `/fast` chose - or, once a turn has read it,
    /// the one the conversation keeps - for the next model calls.
    service_tier: Arc<Mutex<Option<String>>>,
    /// Turns since the last automatic refine review.
    turns_since_review: Arc<std::sync::atomic::AtomicU32>,
    /// The agent's own recurring prompts (`rlm_heartbeat`), for the whole session.
    heartbeats: Arc<super::heartbeat::Heartbeats>,
    /// `/schedule` jobs of the conversation in use, kept on disk.
    schedules: Arc<super::schedules::Schedules>,
    /// Stops the autonomous quality gates that are running.
    gate_cancellation: Option<CancellationToken>,
    /// `HA_AUTO_REFINE=off` turns the automatic review off (prime-agent's
    /// `autoRefine.enabled`, on by default).
    auto_refine: bool,
    /// The model and thinking level a running turn's next call uses.
    live: Arc<LiveTurn>,
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
    /// Messages on their way into the inbox, so the turn that ends waits for
    /// them before it hands what it did not read to the next turn.
    in_flight: Arc<AtomicUsize>,
}

/// The messages that reached a turn's inbox after its last step: prime-agent
/// keeps a steer the run did not take for the next one, so they are handed back
/// instead of being dropped with the run.
async fn carry_unread_messages(active: ActiveTurnInbox, sender: &UnboundedSender<SessionEvent>) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while active.in_flight.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let Ok(Some(run)) = active.store.latest_run(&active.session_id).await else {
        return;
    };
    let now = harness_runtime::now_unix_ms();
    let Ok(commands) = active.inbox.claim(&run, 64, now).await else {
        return;
    };
    for command in commands {
        if command.kind != harness_store_sqlite::RunCommandKind::Steer {
            continue;
        }
        let Some(text) = RunInbox::steering_text(&command) else {
            continue;
        };
        let _ = active
            .inbox
            .apply(&command, "carried to the next turn", now)
            .await;
        let _ = sender.send(SessionEvent::UnreadMessage {
            text,
            verbatim: RunInbox::is_verbatim(&command),
        });
    }
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

/// prime-agent's `/logs` listing over ha's data directory: every `*.log` the
/// app wrote there (the kernel's stderr logs, the workers' logs), as
/// `• name (N.N KB)`, sorted. prime-agent keeps its logs in one directory; ha
/// writes them next to what they log, so the names are paths under the data
/// directory. An empty log says nothing and is not listed (older builds left
/// one per kernel), and the directories that never hold a log - the kernel's
/// virtual environment, the project stores - are not walked.
fn log_lines(data_dir: &Path) -> Vec<String> {
    const NO_LOGS: &[&str] = &["kernel-venv", "projects", "cache", "attachments"];
    fn walk(dir: &Path, depth: usize, found: &mut Vec<(String, u64)>, root: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || (dir == root && NO_LOGS.contains(&name.as_str())) {
                continue;
            }
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() && depth > 0 {
                walk(&path, depth - 1, found, root);
            } else if kind.is_file() && path.extension().is_some_and(|extension| extension == "log")
            {
                let size = entry.metadata().map_or(0, |metadata| metadata.len());
                if size == 0 {
                    continue;
                }
                let shown = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .display()
                    .to_string()
                    .replace('\\', "/");
                found.push((shown, size));
            }
        }
    }
    let mut found = Vec::new();
    walk(data_dir, 4, &mut found, data_dir);
    found.sort();
    let mut lines = vec![format!("Directory: {}", data_dir.display())];
    if found.is_empty() {
        lines.push("No logs written yet.".to_owned());
    } else {
        lines.extend(found.into_iter().map(|(name, size)| {
            #[allow(clippy::cast_precision_loss, reason = "a size shown to one decimal")]
            let kib = size as f64 / 1024.0;
            format!("• {name} ({kib:.1} KB)")
        }));
    }
    lines
}

/// The session setting a turn's tree label is kept in, followed by its session id.
const TURN_LABEL_PREFIX: &str = "label:";

/// The session setting a conversation's `/rlm-max-depth` is kept in.
const RLM_MAX_DEPTH_SETTING: &str = "rlm_max_depth";

/// The session setting a conversation's service tier is kept in.
const SERVICE_TIER_SETTING: &str = "service_tier";

/// What `/subagent-model` does.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SubagentSetting {
    Show,
    /// Save this `provider/model` as `subagentDefaultModel`.
    Set(String),
    /// Remove `subagentDefaultModel`: children run on their parent's model.
    Inherit,
}

/// prime-agent's `subagentDefaultModel` from `settings.json`: the model a child
/// runs on when its spawn names none. A value that is not a non-blank string is
/// unset, as prime-agent's `getSubagentDefaultModel` reads it; ha's own
/// `[agents] default_model` is read next.
fn subagent_default_model(config_file: &Path) -> Option<String> {
    super::config::load_setting(config_file, "subagentDefaultModel")
        .and_then(|value| value.as_str().map(str::trim).map(str::to_owned))
        .filter(|reference| !reference.is_empty())
}

/// `subagentDefaultThinking` from `settings.json`: the level delegated children
/// run at when their spawn names none, as `subagentDefaultModel` is their model.
fn subagent_default_thinking(config_file: &Path) -> Option<harness_providers::ThinkingLevel> {
    super::config::load_setting(config_file, "subagentDefaultThinking").and_then(|value| {
        value
            .as_str()
            .and_then(harness_providers::ThinkingLevel::parse)
    })
}

/// prime-agent's global `defaultThinkingLevel`, from `settings.json`: the level
/// a conversation starts with when it has none of its own.
fn default_thinking_level(config_file: &Path) -> Option<harness_providers::ThinkingLevel> {
    super::config::load_setting(config_file, "defaultThinkingLevel").and_then(|value| {
        value
            .as_str()
            .and_then(harness_providers::ThinkingLevel::parse)
    })
}

/// prime-agent's global `defaultServiceTier`, from `settings.json`.
fn default_service_tier(config_file: &Path) -> Option<String> {
    super::config::load_setting(config_file, "defaultServiceTier")
        .and_then(|value| value.as_str().map(str::to_owned))
        .filter(|tier| super::service_tier::CHOICES.contains(&tier.as_str()))
}

/// The task of the conversation `previous` continues, or `fallback` (the task a
/// new conversation's first turn will use).
async fn conversation_task(
    store: &SqliteStore,
    previous: Option<&SessionId>,
    fallback: TaskId,
) -> TaskId {
    match previous {
        Some(previous) => store
            .session_task(previous)
            .await
            .ok()
            .flatten()
            .unwrap_or(fallback),
        None => fallback,
    }
}

/// prime-agent's `_resolveRlmMaxDepth`: the chat's own value, else the global
/// `rlmMaxDepth` setting, else `RLM_MAX_DEPTH`, else 2. ha has no parent session
/// to inherit from: a child takes its value from the host when it is created.
async fn resolve_rlm_max_depth(
    store: &SqliteStore,
    task_id: &TaskId,
    config_file: &Path,
    environment: &LaunchEnvironment,
) -> (u32, super::delegation::MaxDepthSource) {
    use super::delegation::{DEFAULT_RLM_MAX_DEPTH, MaxDepthSource};
    if let Some(depth) = store
        .session_setting(task_id, RLM_MAX_DEPTH_SETTING)
        .await
        .ok()
        .flatten()
        .and_then(|value| value.trim().parse::<u32>().ok())
    {
        return (depth, MaxDepthSource::Chat);
    }
    if let Some(depth) = super::config::load_setting(config_file, "rlmMaxDepth")
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
    {
        return (depth, MaxDepthSource::Global);
    }
    if let Some(depth) = environment.value("RLM_MAX_DEPTH").and_then(|value| {
        value
            .to_str()
            .and_then(|value| value.trim().parse::<u32>().ok())
    }) {
        return (depth, MaxDepthSource::Env);
    }
    (DEFAULT_RLM_MAX_DEPTH, MaxDepthSource::Default)
}

/// The sessions of the user's own conversations: a delegated child's task
/// shares the project store, but it is the parent's work, not a conversation to
/// resume. A child is marked with [`super::delegation::DELEGATED_CHILD_SETTING`]
/// when it starts; one from before the mark is told by the frame the app itself
/// writes around a brief (`[task from parent]`) on its task's first input, when
/// no title was ever given to the task (a user who typed into one - it happened
/// through "latest" - keeps it listed).
pub(super) async fn user_conversation_sessions(
    store: &SqliteStore,
    sessions: Vec<harness_store_sqlite::SessionSummary>,
) -> Vec<harness_store_sqlite::SessionSummary> {
    let mut first_session = std::collections::HashMap::<String, SessionId>::new();
    for session in &sessions {
        let entry = first_session
            .entry(session.task_id.as_str().to_owned())
            .or_insert_with(|| session.session_id.clone());
        if session.session_id.as_str() < entry.as_str() {
            *entry = session.session_id.clone();
        }
    }
    let mut children = std::collections::HashSet::new();
    for (task, first) in &first_session {
        let Ok(task_id) = TaskId::parse(task.clone()) else {
            continue;
        };
        let setting = |key: &'static str| store.session_setting(&task_id, key);
        let marked = setting(super::delegation::DELEGATED_CHILD_SETTING)
            .await
            .ok()
            .flatten()
            .is_some();
        let legacy = !marked
            && setting("title").await.ok().flatten().is_none()
            && store
                .session_admitted_input(first)
                .await
                .ok()
                .flatten()
                .is_some_and(|(_, text)| text.starts_with(super::delegation::CHILD_PROMPT_HEAD));
        if marked || legacy {
            children.insert(task.clone());
        }
    }
    sessions
        .into_iter()
        .filter(|session| !children.contains(session.task_id.as_str()))
        .collect()
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
        super::custom_models::use_beside(&context.paths.config_file);
        // Checked only when there is a file: startup does not pay for the catalog.
        if super::custom_models::path().is_some_and(|path| path.is_file())
            && let Err(error) = super::providers::Catalog::bundled().apply_custom()
        {
            let _ = sender.send(SessionEvent::Notice {
                message: format!(
                    "{}: {error} - using built-in models only",
                    super::custom_models::FILE_NAME
                ),
            });
        }
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
        if let Some(repl) = &repl {
            repl.set_session_host(Arc::new(super::skill_requests::BashCompletions {
                sender: sender.clone(),
            }));
        }
        let agents = super::delegation::SessionAgents::new(
            super::store_lease::SharedStore::for_dir(context.project_store_dir()),
            sender.clone(),
            Arc::clone(&gate) as Arc<dyn ApprovalGate>,
            context.paths.data_dir.join("delegation"),
        )
        .expect("the delegation scheduler starts with a fixed, valid configuration");
        let task_id = TaskId::generate();
        let schedules = Arc::new(super::schedules::Schedules::default());
        schedules.bind(super::schedules::path_for(
            &context.paths.data_dir,
            task_id.as_str(),
        ));
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
            task_id,
            previous_session: Arc::new(Mutex::new(None)),
            active_inbox: Arc::new(Mutex::new(None)),
            gate,
            cancellation: None,
            limits,
            project_id: Arc::new(Mutex::new(None)),
            context_summary: Arc::new(Mutex::new(Vec::new())),
            sign_in: None,
            credential_scope: credentials::Scope::Main,
            resumed_task: Arc::new(Mutex::new(None)),
            system_prompt: Arc::new(Mutex::new(String::new())),
            active_skills: Arc::new(Mutex::new(BTreeMap::new())),
            pending_mcp_elicitations: Arc::new(Mutex::new(HashMap::new())),
            mcp_status: Arc::new(Mutex::new(Vec::new())),
            agents,
            side_thread: Arc::new(Mutex::new(Vec::new())),
            fork_plan: None,
            branch_plan: None,
            writer_gate,
            goal: None,
            goal_forgotten: false,
            repl,
            thinking: None,
            service_tier: Arc::new(Mutex::new(None)),
            turns_since_review: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            heartbeats: Arc::new(super::heartbeat::Heartbeats::default()),
            schedules,
            gate_cancellation: None,
            live: Arc::new(LiveTurn::default()),
            auto_refine,
        }
    }

    /// The jobs of the conversation this session is now in.
    fn follow_schedules(&self) {
        self.schedules.bind(super::schedules::path_for(
            &self.data_dir,
            self.task_id.as_str(),
        ));
    }

    /// How full the context is, what the session has used, and the limits the
    /// provider reported on its responses - prime-agent's `/context`, plus the
    /// account limits a provider sends with every answer.
    fn usage_lines(&self) -> Vec<String> {
        let config = self.configured().ok();
        let (input, output, context, cost) = self.cost_tracker.lock().map_or_else(
            |_| (0, 0, None, "n/a".to_owned()),
            |tracker| {
                let (input, output) = tracker.tokens();
                (input, output, tracker.context_tokens(), tracker.display())
            },
        );
        let mut lines = Vec::new();
        match (&config, context) {
            (Some(config), Some(used)) => lines.push(format!(
                "Context:  {} of {}'s window ({used} tokens at the last response)",
                super::cost::context_label(used, config.context_window_tokens),
                config.model
            )),
            (Some(config), None) => lines.push(format!(
                "Context:  no response yet; {}'s window is {} tokens",
                config.model,
                super::cost::format_tokens(config.context_window_tokens)
            )),
            (None, _) => lines.push("Context:  no provider is configured".to_owned()),
        }
        lines.push(format!(
            "Session:  {} in, {} out · cost {cost}",
            super::cost::format_tokens(input),
            super::cost::format_tokens(output)
        ));
        if let Some(config) = &config {
            match harness_providers::limits::latest(&config.provider_id) {
                Some(snapshot) => {
                    let age = snapshot.at.elapsed().unwrap_or_default();
                    lines.push(format!(
                        "Limits:   as {} reported them {} ago",
                        config.provider_id,
                        harness_providers::limits::duration_label(age)
                    ));
                    lines.extend(snapshot.lines().into_iter().map(|line| format!("  {line}")));
                }
                None => lines.push(format!(
                    "Limits:   {} has not reported any on its responses in this session",
                    config.provider_id
                )),
            }
        }
        lines
    }

    /// The account balance of a provider that has an endpoint for it, sent as
    /// its own card when the answer comes.
    fn fetch_balance(&self) {
        let Ok(config) = self.configured() else {
            return;
        };
        let Some(url) =
            super::providers::provider(&config.provider_id).and_then(|entry| entry.balance_url)
        else {
            return;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let credential = EnvironmentCredential::new(
            config.provider_id.clone(),
            config.credential_variable(),
            self.data_dir.clone(),
        );
        let sender = self.sender.clone();
        handle.spawn(async move {
            let lines = match super::providers::fetch_balance(url, &credential).await {
                Ok(lines) => lines,
                Err(error) => vec![format!("balance unavailable: {error}")],
            };
            let _ = sender.send(SessionEvent::Reference {
                title: format!("{} balance", config.provider_id),
                lines,
            });
        });
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

    /// What the configured model offers for reasoning: its catalog entry, as
    /// prime-agent reads it, else the built-in table.
    fn reasoning_of(
        &self,
        config: &ProviderConfig,
    ) -> Option<harness_providers::thinking::ReasoningModel> {
        // The catalog is the only source: a model it does not know is not assumed
        // to reason, whatever its name looks like.
        super::providers::Catalog::load(&self.data_dir)
            .find(&format!("{}/{}", config.provider_id, config.model))
            .map(super::providers::Model::reasoning_model)
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
        let resumed_task = Arc::clone(&self.resumed_task);
        let store_dir = self.store_dir.clone();
        let schedules = Arc::clone(&self.schedules);
        let data_dir = self.data_dir.clone();
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
                Ok(Some(task)) => {
                    if let Ok(mut resumed) = resumed_task.lock() {
                        *resumed = Some(task.as_str().to_owned());
                    }
                    // The resumed conversation's scheduled jobs come back with it.
                    schedules.bind(super::schedules::path_for(&data_dir, task.as_str()));
                    store
                        .session_setting(&task, super::goal::GOAL_SETTING)
                        .await
                        .ok()
                        .flatten()
                        .filter(|objective| !objective.trim().is_empty())
                }
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
                let sessions = store
                    .list_sessions()
                    .await
                    .map_err(|error| format!("project sessions could not be listed: {error}"))?;
                // "latest" is the user's latest conversation, never a child that
                // happened to write last.
                let mut sessions = user_conversation_sessions(&store, sessions).await;
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

/// The app is closing: its Python kernel is disposed the way prime-agent disposes a
/// session's - a final snapshot of the namespace, then the protocol's shutdown - so
/// the next start of the conversation revives it. Only a multi-threaded runtime can
/// wait for that here; anywhere else the kernel is killed with the process.
impl Drop for AgentSessionService {
    fn drop(&mut self) {
        let Some(repl) = self.repl.take() else {
            return;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if handle.runtime_flavor() != tokio::runtime::RuntimeFlavor::MultiThread {
            return;
        }
        tokio::task::block_in_place(|| handle.block_on(repl.dispose()));
    }
}

/// The scoped models whose provider has a credential, in scope order.
fn scoped_entries(
    environment: &LaunchEnvironment,
    data_dir: &Path,
    config: &ProviderConfig,
) -> Vec<super::routing::ScopeEntry> {
    let catalog = super::providers::Catalog::load(data_dir);
    let usable = catalog
        .models()
        .iter()
        .filter(|model| {
            credentials::source_for(environment, data_dir, &model.provider, &model.key_env())
                .is_some()
        })
        .cloned()
        .collect::<Vec<_>>();
    super::routing::scope(&config.routing.scoped, &usable)
}

/// Every catalog model `/model` offers: those whose provider has a credential.
fn catalog_options(environment: &LaunchEnvironment, data_dir: &Path) -> Vec<(String, String)> {
    catalog_options_scoped(environment, data_dir, credentials::Scope::Main)
}

/// The catalog models with a key as `scope` reads it.
fn catalog_options_scoped(
    environment: &LaunchEnvironment,
    data_dir: &Path,
    scope: credentials::Scope,
) -> Vec<(String, String)> {
    catalog_models_scoped(environment, data_dir, scope)
        .into_iter()
        .map(|model| (model.reference(), model.name))
        .collect()
}

/// The catalog entries whose provider has a key as `scope` reads it, built-in
/// providers in `/login` order first.
fn catalog_models_scoped(
    environment: &LaunchEnvironment,
    data_dir: &Path,
    scope: credentials::Scope,
) -> Vec<super::providers::Model> {
    // Built-in providers in `/login` order, then those `models.json` adds.
    let catalog = super::providers::Catalog::load(data_dir);
    let mut providers = super::providers::PROVIDERS
        .iter()
        .map(|provider| provider.id.to_owned())
        .collect::<Vec<_>>();
    for model in catalog.models() {
        if !providers.contains(&model.provider) {
            providers.push(model.provider.clone());
        }
    }
    providers
        .iter()
        .flat_map(|provider| catalog.for_provider(provider))
        .filter(|model| {
            credentials::source_for_scope(
                environment,
                data_dir,
                &model.provider,
                &model.key_env(),
                scope,
            )
            .is_some()
        })
        .cloned()
        .collect()
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
        let system_prompt = Arc::clone(&self.system_prompt);
        let active_skills = Arc::clone(&self.active_skills);
        let pending_mcp_elicitations = Arc::clone(&self.pending_mcp_elicitations);
        let mcp_status = Arc::clone(&self.mcp_status);
        let agents = Arc::clone(&self.agents);
        let fork_plan = self.fork_plan.take();
        let branch_plan = self.branch_plan.take();
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
        let service_tier = Arc::clone(&self.service_tier);
        let turns_since_review = Arc::clone(&self.turns_since_review);
        let heartbeats = Arc::clone(&self.heartbeats);
        let live = Arc::clone(&self.live);
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
                system_prompt,
                active_skills,
                pending_mcp_elicitations,
                mcp_status,
                agents,
                fork_plan,
                branch_plan,
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
                service_tier,
                turns_since_review,
                auto_refine,
                heartbeats,
                live,
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

    fn manage_mcp(&mut self, args: &[String]) -> Result<Vec<String>, String> {
        let configured = super::config::resolve_layers(
            &self.config_file,
            &self.workspace_root,
            &self.environment,
            &self.config_overrides,
        )
        .map(|resolved| resolved.mcp_servers)
        .map_err(|error| error.to_string())?;
        super::mcp_config::run(&self.config_file, args, &configured).map(|outcome| outcome.lines)
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
        self.agents.summary()
    }

    fn agent_exchanges(&self) -> Vec<String> {
        self.agents.exchanges()
    }

    fn menu_commands(&self) -> Vec<super::commands::MenuCommand> {
        let trusted = super::config::resolve_layers(
            &self.config_file,
            &self.workspace_root,
            &self.environment,
            &self.config_overrides,
        )
        .is_ok_and(|config| config.project_trusted);
        let mut commands = Vec::new();
        if let Ok(catalog) = super::skills::discover(
            &self.global_config_dir,
            &self.workspace_root,
            &self.environment,
            trusted,
        ) {
            commands.extend(
                catalog
                    .entries()
                    .iter()
                    .map(|entry| super::commands::MenuCommand {
                        name: format!("/skill:{}", entry.name),
                        description: entry.description.clone(),
                        argument_hint: String::new(),
                        tag: "skill",
                    }),
            );
        }
        if let Ok(prompts) =
            super::skills::commands(&self.global_config_dir, &self.workspace_root, trusted)
        {
            commands.extend(
                prompts
                    .into_iter()
                    .map(|command| super::commands::MenuCommand {
                        name: format!("/{}", command.name),
                        description: command.description,
                        argument_hint: command.argument_hint,
                        tag: "prompt",
                    }),
            );
        }
        commands
    }

    fn logs(&self) -> Vec<String> {
        log_lines(&self.data_dir)
    }

    fn plan_branch_summary(&mut self, focus: Option<String>) -> Result<(), String> {
        let leaf = self
            .previous_session
            .lock()
            .ok()
            .and_then(|source| source.clone())
            .ok_or("this conversation has no turn to summarize yet")?;
        self.branch_plan = Some(super::branch_summary::BranchPlan { leaf, focus });
        Ok(())
    }

    fn label_turn(&mut self, session: &str, label: &str) -> Result<(), String> {
        SessionId::parse(session.to_owned()).map_err(|error| error.to_string())?;
        let previous = self
            .previous_session
            .lock()
            .ok()
            .and_then(|source| source.clone());
        let task_id = self.task_id.clone();
        let agents = Arc::clone(&self.agents);
        let sender = self.sender.clone();
        let key = format!("{TURN_LABEL_PREFIX}{session}");
        let label = label.trim().to_owned();
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| "the application service needs an async runtime".to_owned())?;
        handle.spawn(async move {
            let message = match agents.store().lease().await {
                Ok(lease) => {
                    let store = lease.store();
                    let task = conversation_task(&store, previous.as_ref(), task_id).await;
                    match store.set_session_setting(&task, &key, &label).await {
                        Ok(()) if label.is_empty() => "Label cleared".to_owned(),
                        Ok(()) => format!("Label set: {label}"),
                        Err(error) => format!("the label could not be saved: {error}"),
                    }
                }
                Err(error) => format!("the label could not be saved: {error}"),
            };
            let _ = sender.send(SessionEvent::Notice { message });
        });
        Ok(())
    }

    fn park_until(&mut self, until: chrono::DateTime<chrono::Utc>) -> Result<String, String> {
        let at = until.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        // A job kept with the conversation survives quitting the app: it runs
        // when the app is open again, or once when the conversation is resumed.
        let job = self.schedules.add(
            &format!("at {at}"),
            super::routing::QUOTA_RESUME_PROMPT,
            super::heartbeat::Delivery::FollowUp,
            chrono::Utc::now(),
        )?;
        Ok(format!(
            "Session parked until {at}; it resumes automatically ({} - /schedule cancel {} stops it)",
            job.id, job.id
        ))
    }

    fn import_session(&mut self, path: &str) -> Result<String, String> {
        // prime-agent resolves the path against the working directory, takes a
        // quoted one, and copies the file beside its sessions.
        let path = path.trim().trim_matches(['"', '\'']);
        let source = self.workspace_root.join(path);
        let text = std::fs::read_to_string(&source).map_err(|_| {
            format!(
                "Failed to import session: File not found: {}",
                source.display()
            )
        })?;
        let (header, messages) = harness_runtime::session_file::read_session_file(&text)
            .map_err(|error| format!("Failed to import session: {error}"))?;
        self.resume(None)?;
        let imports = self.data_dir.join("imports");
        std::fs::create_dir_all(&imports).map_err(|error| error.to_string())?;
        let kept = imports.join(format!("{}.jsonl", self.task_id.as_str()));
        std::fs::write(&kept, &text).map_err(|error| error.to_string())?;
        let task_id = self.task_id.clone();
        let agents = Arc::clone(&self.agents);
        let sender = self.sender.clone();
        let title = header
            .title
            .clone()
            .unwrap_or_else(|| format!("imported {path}"));
        let kept_value = kept.display().to_string();
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| "the application service needs an async runtime".to_owned())?;
        handle.spawn(async move {
            // The new conversation's task names the file its turns continue.
            match agents.store().lease().await {
                Ok(lease) => {
                    let store = lease.store();
                    let _ = store
                        .set_session_setting(
                            &task_id,
                            harness_runtime::session_file::IMPORTED_HISTORY_SETTING,
                            &kept_value,
                        )
                        .await;
                    let _ = store.set_session_setting(&task_id, "title", &title).await;
                }
                Err(error) => {
                    let _ = sender.send(SessionEvent::RecoverableError {
                        message: format!("the imported session could not be recorded: {error}"),
                    });
                }
            }
        });
        let _ = self.sender.send(SessionEvent::Notice {
            message: format!(
                "{} message(s) imported; the next message continues that conversation",
                messages.len()
            ),
        });
        Ok(format!("Session imported from: {path}"))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one panel, built top to bottom: active skills, then one section per source"
    )]
    fn skills_rows(&self) -> Vec<super::events::RefLine> {
        use super::events::{BadgeKind, RefLine};
        let trusted = super::config::resolve_layers(
            &self.config_file,
            &self.workspace_root,
            &self.environment,
            &self.config_overrides,
        )
        .is_ok_and(|config| config.project_trusted);
        let catalog = match super::skills::discover(
            &self.global_config_dir,
            &self.workspace_root,
            &self.environment,
            trusted,
        ) {
            Ok(catalog) => catalog,
            Err(error) => {
                return vec![RefLine::Text(format!("skill catalog unavailable: {error}"))];
            }
        };
        let active: Vec<(String, String, String)> = self
            .active_skills
            .lock()
            .map(|active| {
                active
                    .values()
                    .map(|activation| {
                        (
                            activation.entry.name.clone(),
                            activation.entry.version.clone(),
                            activation.entry.digest.as_str().to_owned(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut rows = Vec::new();
        if !active.is_empty() {
            rows.push(RefLine::Heading {
                title: "● Active".to_owned(),
                note: format!("{} in this conversation", active.len()),
            });
            for (name, version, digest) in &active {
                let short = digest.strip_prefix("sha256:").unwrap_or(digest);
                rows.push(RefLine::Item {
                    glyph: "●".to_owned(),
                    name: name.clone(),
                    meta: version.clone(),
                    badges: vec![(
                        format!("sha {}", short.chars().take(8).collect::<String>()),
                        BadgeKind::Neutral,
                    )],
                    detail: String::new(),
                });
            }
            rows.push(RefLine::Blank);
        }
        let mut sources: Vec<&str> = Vec::new();
        for entry in catalog.entries() {
            if !sources.contains(&entry.source.as_str()) {
                sources.push(entry.source.as_str());
            }
        }
        for source in sources {
            let entries: Vec<_> = catalog
                .entries()
                .iter()
                .filter(|entry| entry.source.as_str() == source)
                .collect();
            let title = match source {
                "builtin" => "✦ Bundled with ha".to_owned(),
                "user" => "✦ Your skills".to_owned(),
                "trusted_project" | "project" => "✦ This project".to_owned(),
                other => format!("✦ {other}"),
            };
            rows.push(RefLine::Heading {
                title,
                note: format!("{}", entries.len()),
            });
            for entry in entries {
                let is_active = active.iter().any(|(name, _, _)| *name == entry.name);
                let mut badges = Vec::new();
                if is_active {
                    badges.push(("active".to_owned(), BadgeKind::Ok));
                }
                rows.push(RefLine::Item {
                    glyph: "✦".to_owned(),
                    name: entry.name.clone(),
                    meta: entry.version.clone(),
                    badges,
                    detail: entry.description.clone(),
                });
            }
            rows.push(RefLine::Blank);
        }
        if rows.is_empty() {
            return vec![RefLine::Text("no trusted skills found".to_owned())];
        }
        rows.push(RefLine::Hint(
            "The model activates a matching skill by itself; /skill:<name> runs one yourself."
                .to_owned(),
        ));
        rows
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

    fn expand_skill(&self, name: &str, arguments: &str) -> Result<String, String> {
        let trusted = super::config::resolve_layers(
            &self.config_file,
            &self.workspace_root,
            &self.environment,
            &self.config_overrides,
        )
        .is_ok_and(|config| config.project_trusted);
        let catalog = super::skills::discover(
            &self.global_config_dir,
            &self.workspace_root,
            &self.environment,
            trusted,
        )
        .map_err(|error| error.to_string())?;
        let entry = catalog
            .entries()
            .iter()
            .find(|entry| entry.name == name)
            .ok_or_else(|| format!("no skill named {name}; /skills lists them"))?;
        let content = std::fs::read_to_string(&entry.path)
            .map_err(|error| format!("skill {name} could not be read: {error}"))?;
        // The user ran it: it is active for the session, so the model may
        // activate it and read its files although it cannot start it itself.
        if let Ok(activation) = catalog.activate(name, None, 0)
            && let Ok(mut active) = self.active_skills.lock()
        {
            active.insert(name.to_owned(), activation);
        }
        let directory = entry
            .directory()
            .map_or_else(String::new, |directory| directory.display().to_string());
        let block = format!(
            "<skill name=\"{name}\" location=\"{}\">\nReferences are relative to {directory}.\n\n{}\n</skill>",
            entry.path.display(),
            strip_front_matter(&content).trim()
        );
        let arguments = arguments.trim();
        Ok(if arguments.is_empty() {
            block
        } else {
            format!("{block}\n\n{arguments}")
        })
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

    fn system_prompt(&self) -> Vec<String> {
        match self.system_prompt.lock() {
            Ok(text) if !text.is_empty() => text.lines().map(str::to_owned).collect(),
            _ => vec!["no turn has run in this session yet".to_owned()],
        }
    }

    fn context_summary(&self) -> Vec<String> {
        let mut lines = self.usage_lines();
        lines.push(String::new());
        lines.extend(self.context_summary.lock().map_or_else(
            |_| vec!["context details are unavailable".to_owned()],
            |lines| {
                if lines.is_empty() {
                    vec!["no context packet has been built in this session yet".to_owned()]
                } else {
                    lines.clone()
                }
            },
        ));
        self.fetch_balance();
        lines
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
        let shared_store = self.agents.store();
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| "the application service needs an async runtime".to_owned())?;
        handle.spawn(async move {
            let _ = store_dir;
            let result = async {
                let _writer = writer_gate.lock().await;
                let lease = shared_store
                    .lease()
                    .await
                    .map_err(|error| error.to_string())?;
                lease
                    .store()
                    .set_session_setting(&task_id, "title", &title_for_write)
                    .await
                    .map_err(|error| error.to_string())
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
        // A catalog model, `provider/id` or an unambiguous id, becomes the saved
        // selection: provider, wire format and endpoint together, as prime-agent's
        // model selector sets its default model.
        if let Some(entry) = super::providers::Catalog::load(&self.data_dir).find(model) {
            let selection = super::config::Selection {
                provider: entry.provider.clone(),
                model: entry.id.clone(),
                protocol: entry.protocol().unwrap_or("openai_chat").to_owned(),
                endpoint: entry.endpoint(),
                context_window: entry.context_window,
                thinking_format: entry
                    .compat
                    .as_ref()
                    .and_then(|compat| compat.thinking_format.clone()),
                api_key_env: entry.key_variable.clone(),
            };
            super::config::save_selection(&self.config_file, &selection)
                .map_err(|error| error.to_string())?;
            if let Ok(mut selection) = self.model_selection.lock() {
                *selection = TurnModelSelection::default();
            }
            let ready = credentials::source_for(
                &self.environment,
                &self.data_dir,
                &entry.provider,
                &entry.key_env(),
            )
            .is_some();
            let provider_name = super::providers::provider(&entry.provider)
                .map_or(entry.provider.as_str(), |provider| provider.name);
            // A running turn switches at its next model call, as prime-agent's
            // model selector does, with the level in force re-applied in the
            // same step.
            if ready && let Ok(config) = self.configured() {
                self.live.set_model_and_level(config, self.thinking);
            }
            return Ok(if ready {
                format!("model {} ({provider_name}) selected", entry.name)
            } else {
                format!(
                    "model {} ({provider_name}) selected; log in with /login {} before the next turn",
                    entry.name, entry.provider
                )
            });
        }
        self.model_selection
            .lock()
            .map_err(|_| "model selection is unavailable".to_owned())?
            .select_for_next_turn(model.to_owned());
        Ok(format!("model {model} selected for the next turn"))
    }

    fn due_heartbeats(&mut self) -> Vec<super::heartbeat::Due> {
        let mut due = self.heartbeats.due(std::time::Instant::now());
        due.extend(self.schedules.due(chrono::Utc::now()));
        due
    }

    fn schedule(&mut self, argument: Option<&str>) -> Result<Vec<String>, String> {
        super::schedules::command(&self.schedules, argument)
    }

    fn session_tokens(&self) -> u64 {
        self.cost_tracker.lock().map_or(0, |tracker| {
            let (input, output) = tracker.tokens();
            input.saturating_add(output)
        })
    }

    fn running_children(&self) -> usize {
        self.agents.running()
    }

    fn has_scheduled_work(&self) -> bool {
        self.schedules.has_active() || self.heartbeats.has_active()
    }

    fn conversation_id(&self) -> Option<String> {
        self.resumed_task
            .lock()
            .ok()
            .and_then(|task| task.clone())
            .or_else(|| Some(self.task_id.as_str().to_owned()))
    }

    fn set_session_host(&mut self, host: Arc<dyn super::agents::SessionHost>) {
        self.agents.set_session_host(host);
    }

    fn run_gates(&mut self, job: super::autonomous::GateJob) -> Result<(), String> {
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| "the application service needs an async runtime".to_owned())?;
        self.cancel_gates();
        let cancellation = CancellationToken::new();
        self.gate_cancellation = Some(cancellation.clone());
        let sender = self.sender.clone();
        let root = self.workspace_root.clone();
        handle.spawn(async move {
            let (result, state) = super::autonomous::run_gates(&root, job, cancellation).await;
            let _ = sender.send(SessionEvent::GatesChecked { result, state });
        });
        Ok(())
    }

    fn cancel_gates(&mut self) {
        if let Some(token) = self.gate_cancellation.take() {
            token.cancel();
        }
    }

    fn cycle_model(&mut self, forward: bool) -> Result<String, String> {
        let config = self.configured()?;
        if config.routing.scoped.is_empty() {
            return Err(
                "no scoped models: choose them with /scoped-models <pattern>... or [routing] scoped"
                    .to_owned(),
            );
        }
        let entries = scoped_entries(&self.environment, &self.data_dir, &config);
        if entries.len() < 2 {
            return Err(format!(
                "cycling needs two scoped models with a credential; {} matched",
                entries.len()
            ));
        }
        let current = format!("{}/{}", config.provider_id, config.model);
        let next = super::routing::step(&entries, &current, forward)
            .cloned()
            .ok_or_else(|| "no scoped model to move to".to_owned())?;
        let mut message = self.set_model(&next.reference)?;
        if let Some(level) = next.level {
            match self.set_thinking(level.as_str()) {
                Ok(thinking) => message = format!("{message}; {thinking}"),
                Err(error) => message = format!("{message}; {error}"),
            }
        }
        Ok(message)
    }

    fn scoped_models(&mut self, argument: Option<&str>) -> Result<Vec<String>, String> {
        match argument
            .map(str::trim)
            .filter(|argument| !argument.is_empty())
        {
            None => {}
            Some("clear") => {
                super::config::save_scoped_models(&self.config_file, &[])?;
            }
            Some(patterns) => {
                let patterns = patterns
                    .split([' ', ','])
                    .filter(|pattern| !pattern.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                let catalog = super::providers::Catalog::load(&self.data_dir);
                if let Some(unmatched) = patterns.iter().find(|pattern| {
                    super::routing::scope(std::slice::from_ref(*pattern), catalog.models())
                        .is_empty()
                }) {
                    return Err(format!("{unmatched} matches no model in the catalog"));
                }
                super::config::save_scoped_models(&self.config_file, &patterns)?;
            }
        }
        let config = self.configured()?;
        if config.routing.scoped.is_empty() {
            return Ok(vec![
                "no scoped models; /scoped-models <pattern>... chooses them (e.g. deepseek/* openai/gpt-5*:high)".to_owned(),
            ]);
        }
        let current = format!("{}/{}", config.provider_id, config.model);
        let mut lines = vec![format!("patterns: {}", config.routing.scoped.join(" "))];
        let entries = scoped_entries(&self.environment, &self.data_dir, &config);
        if entries.is_empty() {
            lines.push("no model with a credential matches; log in with /login".to_owned());
        }
        for entry in entries {
            lines.push(format!(
                "{} {}{}",
                if entry.reference == current { "*" } else { " " },
                entry.reference,
                entry
                    .level
                    .map(|level| format!(":{}", level.as_str()))
                    .unwrap_or_default()
            ));
        }
        lines.push("/model next and /model prev (Alt+M, Shift+Alt+M) move through them".to_owned());
        Ok(lines)
    }

    fn set_thinking(&mut self, level: &str) -> Result<String, String> {
        let config = self.configured()?;
        let model = self.reasoning_of(&config);
        // The provider's spelling (`max`) or this app's level name (`xhigh`).
        let requested = harness_providers::thinking::resolve(model, level).ok_or_else(|| {
            let names = harness_providers::thinking::offered(model)
                .into_iter()
                .map(|(_, name)| name)
                .collect::<Vec<_>>();
            format!(
                "unknown thinking level {level:?}; {} offers {}",
                config.model,
                names.join(", ")
            )
        })?;
        self.thinking = Some(requested);
        // A running turn uses the new level from its next model call.
        self.live.set_level(requested);
        // prime-agent's `setThinkingLevel` keeps the level as the default for new
        // sessions, unless it is `off` on a model that does not think.
        if model.is_some_and(|model| model.reasoning)
            || requested != harness_providers::ThinkingLevel::Off
        {
            let _ = super::config::save_setting(
                &self.config_file,
                "defaultThinkingLevel",
                Some(serde_json::json!(requested.as_str())),
            );
        }
        let used = harness_providers::thinking::clamp(model, requested);
        let name = |level| harness_providers::thinking::provider_name(model, level);
        Ok(if used == requested {
            format!("thinking {} selected for the next turn", name(requested))
        } else {
            format!(
                "thinking {} selected for the next turn; {} uses {}, the nearest level it offers",
                name(requested),
                config.model,
                name(used)
            )
        })
    }

    fn set_service_tier(&mut self, tier: &str) -> Result<String, String> {
        let config = self.configured()?;
        let available =
            super::service_tier::available(&config.provider_id, &config.protocol, &config.model);
        if !available.contains(&tier) {
            return Err(format!(
                "Service tier '{tier}' is not available for the current model. Available: {}",
                available.join(", ")
            ));
        }
        if let Ok(mut current) = self.service_tier.lock() {
            *current = Some(tier.to_owned());
        }
        // A running turn uses the new tier from its next model call.
        self.live
            .update_config(config, |live| live.service_tier = Some(tier.to_owned()));
        // prime-agent also keeps the tier as the default for new sessions.
        let _ = super::config::save_setting(
            &self.config_file,
            "defaultServiceTier",
            Some(serde_json::json!(tier)),
        );
        Ok(format!("Service tier: {tier}"))
    }

    fn service_tier(&self) -> (String, Vec<&'static str>) {
        let Ok(config) = self.configured() else {
            return ("default".to_owned(), vec!["default"]);
        };
        let requested = self
            .service_tier
            .lock()
            .ok()
            .and_then(|tier| tier.clone())
            .or_else(|| default_service_tier(&self.config_file));
        let current = super::service_tier::clamp(
            requested.as_deref(),
            &config.provider_id,
            &config.protocol,
            &config.model,
        )
        .unwrap_or_else(|| "default".to_owned());
        (
            current,
            super::service_tier::available(&config.provider_id, &config.protocol, &config.model),
        )
    }

    fn thinking_levels(&self) -> Vec<String> {
        let Ok(config) = self.configured() else {
            return Vec::new();
        };
        harness_providers::thinking::offered(self.reasoning_of(&config))
            .into_iter()
            .map(|(_, name)| name)
            .collect()
    }

    fn thinking_level(&self) -> Option<String> {
        let config = self.configured().ok()?;
        let model = self.reasoning_of(&config);
        let chosen = self
            .thinking
            .or_else(|| default_thinking_level(&self.config_file))
            .or_else(|| harness_providers::ThinkingLevel::parse(&config.thinking))
            .unwrap_or_default();
        Some(harness_providers::thinking::provider_name(
            model,
            harness_providers::thinking::clamp(model, chosen),
        ))
    }

    fn thinking_status(&self) -> Vec<String> {
        let Ok(config) = self.configured() else {
            return vec!["no provider is configured".to_owned()];
        };
        let model = self.reasoning_of(&config);
        let chosen = self
            .thinking
            .or_else(|| default_thinking_level(&self.config_file))
            .or_else(|| harness_providers::ThinkingLevel::parse(&config.thinking))
            .unwrap_or_default();
        let names = harness_providers::thinking::offered(model)
            .into_iter()
            .map(|(_, name)| name)
            .collect::<Vec<_>>();
        let used = harness_providers::thinking::provider_name(
            model,
            harness_providers::thinking::clamp(model, chosen),
        );
        vec![
            format!(
                "Thinking: {} (next turn uses {used})",
                harness_providers::thinking::provider_name(model, chosen)
            ),
            if model.is_some() {
                format!("Model:    {} offers {}", config.model, names.join(", "))
            } else {
                format!(
                    "Model:    {} is not in the model catalog, so no thinking level is sent",
                    config.model
                )
            },
            format!(
                "/thinking <{}> changes it; provider.thinking or HA_PROVIDER_THINKING sets the default",
                names.join("|")
            ),
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
        // A turn that is running follows the change from its next action, as
        // Claude Code's mode switch does; the next turns start in it.
        self.gate.turn_rules().set_mode(mode);
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

    fn save_credential(
        &mut self,
        provider: &str,
        credential: &credentials::Credential,
    ) -> Result<CredentialSource, String> {
        let path =
            credentials::scoped_file(&self.environment, &self.data_dir, self.credential_scope);
        credentials::save(&path, provider, credential)
            .map(|protection| CredentialSource::File { path, protection })
            .map_err(|error| format!("the credential could not be saved: {error}"))
    }

    fn remove_credential(&mut self, provider: &str) -> Result<bool, String> {
        let path =
            credentials::scoped_file(&self.environment, &self.data_dir, self.credential_scope);
        credentials::remove(&path, provider).map_err(|error| error.to_string())
    }

    fn begin_sign_in(&mut self, provider: &str) -> Result<String, String> {
        self.cancel_sign_in();
        let pending = Arc::new(super::oauth::start(provider)?);
        let url = pending.url.clone();
        if pending.listening() {
            let waiting = Arc::clone(&pending);
            let sender = self.sender.clone();
            let path =
                credentials::scoped_file(&self.environment, &self.data_dir, self.credential_scope);
            std::thread::spawn(move || {
                let Some(code) = super::oauth::wait_for_code(&waiting) else {
                    return;
                };
                if waiting
                    .canceller()
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    return;
                }
                let result = super::oauth::finish(&waiting, &code, &path);
                let _ = sender.send(SessionEvent::LoginFinished {
                    provider: waiting.provider.clone(),
                    result,
                });
            });
        }
        super::oauth::open_browser(&url);
        self.sign_in = Some(pending);
        Ok(url)
    }

    fn finish_sign_in(&mut self, pasted: &str) -> Result<(), String> {
        let pending = self
            .sign_in
            .take()
            .ok_or_else(|| "no sign-in is in progress".to_owned())?;
        let code = match pending.code_from_paste(pasted) {
            Ok(code) => code,
            Err(error) => {
                self.sign_in = Some(pending);
                return Err(error);
            }
        };
        pending
            .canceller()
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let sender = self.sender.clone();
        let path =
            credentials::scoped_file(&self.environment, &self.data_dir, self.credential_scope);
        std::thread::spawn(move || {
            let result = super::oauth::finish(&pending, &code, &path);
            let _ = sender.send(SessionEvent::LoginFinished {
                provider: pending.provider.clone(),
                result,
            });
        });
        Ok(())
    }

    fn cancel_sign_in(&mut self) {
        if let Some(pending) = self.sign_in.take() {
            pending
                .canceller()
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    fn stored_credentials(&self) -> Vec<(String, &'static str)> {
        let path =
            credentials::scoped_file(&self.environment, &self.data_dir, self.credential_scope);
        credentials::stored(&path).unwrap_or_default()
    }

    fn set_credential_scope(&mut self, scope: credentials::Scope) {
        self.credential_scope = scope;
    }

    fn stored_subagent_credentials(&self) -> Vec<(String, &'static str)> {
        let path = credentials::scoped_file(
            &self.environment,
            &self.data_dir,
            credentials::Scope::Subagent,
        );
        credentials::stored(&path).unwrap_or_default()
    }

    fn subagent_model_options(&self) -> Vec<(String, String)> {
        catalog_options_scoped(
            &self.environment,
            &self.data_dir,
            credentials::Scope::Subagent,
        )
    }

    fn provider_id(&self) -> Option<String> {
        super::config::resolve_layers(
            &self.config_file,
            &self.workspace_root,
            &self.environment,
            &self.config_overrides,
        )
        .ok()
        .map(|config| config.provider.id)
    }

    fn model_options(&self) -> Vec<(String, String)> {
        let mut options = vec![
            ("next".to_owned(), "next scoped model (Alt+M)".to_owned()),
            (
                "prev".to_owned(),
                "previous scoped model (Shift+Alt+M)".to_owned(),
            ),
        ];
        options.extend(catalog_options(&self.environment, &self.data_dir));
        options
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
                        let summaries = user_conversation_sessions(&store, summaries).await;
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
                            // The model the conversation last talked to, read off its
                            // last request; a model chosen for the task comes next.
                            let sent = store
                                .session_model(&summary.session_id)
                                .await
                                .ok()
                                .flatten()
                                .map(|(provider, model)| format!("{provider}/{model}"));
                            let model = match sent {
                                Some(model) => model,
                                None => store
                                    .session_setting(&summary.task_id, "model")
                                    .await
                                    .ok()
                                    .flatten()
                                    .unwrap_or_else(|| "model unknown".to_owned()),
                            };
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
        // The children worked for the conversation being left; they stop quietly.
        self.agents.reset();
        // The next conversation reads its own tier.
        if let Ok(mut tier) = self.service_tier.lock() {
            *tier = None;
        }
        self.fork_plan = None;
        self.branch_plan = None;
        if let Ok(mut thread) = self.side_thread.lock() {
            thread.clear();
        }
        if let Ok(mut resumed) = self.resumed_task.lock() {
            *resumed = None;
        }
        if source.is_none() {
            self.task_id = TaskId::generate();
            self.follow_schedules();
            if let Ok(mut tracker) = self.cost_tracker.lock() {
                tracker.forget_context();
            }
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
        self.gate.cancel_pending();
    }

    fn steer(&mut self, text: &str) -> Result<(), String> {
        self.queue_into_run(text, false)
    }

    fn deliver_message(&mut self, text: &str) -> Result<(), String> {
        self.queue_into_run(text, true)
    }

    fn list_turns(&mut self, purpose: super::events::TurnsPurpose) -> Result<(), String> {
        let source = self
            .previous_session
            .lock()
            .ok()
            .and_then(|source| source.clone());
        let store_dir = self.store_dir.clone();
        let sender = self.sender.clone();
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| "the application service needs an async runtime".to_owned())?;
        handle.spawn(async move {
            let turns = match source {
                None => Ok(Vec::new()),
                Some(source) => match SqliteStore::open_read_only(store_dir).await {
                    Ok(store) => {
                        let turns = harness_runtime::conversation_turns(&store, &source)
                            .await
                            .map_err(|error| error.to_string());
                        // prime-agent's tree shows each labelled entry as `[label] `.
                        let task = store.session_task(&source).await.ok().flatten();
                        match (turns, task) {
                            (Ok(turns), Some(task))
                                if purpose == super::events::TurnsPurpose::Tree =>
                            {
                                let mut labelled = Vec::new();
                                for (session, question) in turns {
                                    let label = store
                                        .session_setting(
                                            &task,
                                            &format!("{TURN_LABEL_PREFIX}{}", session.as_str()),
                                        )
                                        .await
                                        .ok()
                                        .flatten()
                                        .filter(|label| !label.is_empty());
                                    let question = match label {
                                        Some(label) => format!("[{label}] {question}"),
                                        None => question,
                                    };
                                    labelled.push((session, question));
                                }
                                Ok(labelled)
                            }
                            (turns, _) => turns,
                        }
                    }
                    Err(error) => Err(error.to_string()),
                },
            };
            match turns {
                Ok(turns) => {
                    let _ = sender.send(SessionEvent::TurnsListed {
                        purpose,
                        turns: turns
                            .into_iter()
                            .map(|(session, question)| (session.as_str().to_owned(), question))
                            .collect(),
                    });
                }
                Err(error) => {
                    let _ = sender.send(SessionEvent::Notice {
                        message: format!("the conversation could not be read: {error}"),
                    });
                }
            }
        });
        Ok(())
    }

    fn fork(&mut self, session: &str, before: bool) -> Result<(), String> {
        let from = SessionId::parse(session.to_owned()).map_err(|error| error.to_string())?;
        self.start_fork(ForkPlan { from, before });
        Ok(())
    }

    fn clone_conversation(&mut self) -> Result<(), String> {
        let from = self
            .previous_session
            .lock()
            .ok()
            .and_then(|source| source.clone())
            .ok_or_else(|| "Nothing to clone yet".to_owned())?;
        self.start_fork(ForkPlan {
            from,
            before: false,
        });
        Ok(())
    }

    fn switch_to(&mut self, session: &str) -> Result<(), String> {
        let session = SessionId::parse(session.to_owned()).map_err(|error| error.to_string())?;
        let mut previous = self
            .previous_session
            .lock()
            .map_err(|_| "conversation state is unavailable".to_owned())?;
        *previous = Some(session);
        Ok(())
    }

    fn side_question(&mut self, question: &str) -> Result<(), String> {
        let question = question.trim().to_owned();
        if question.is_empty() {
            return Err("usage: /btw <question>".to_owned());
        }
        let config = self.configured()?;
        let level = self
            .thinking
            .or_else(|| harness_providers::ThinkingLevel::parse(&config.thinking))
            .unwrap_or_default();
        let provider = LiveProvider::build(&config, level, self.task_id.as_ref(), &self.data_dir)
            .map_err(|error| error.to_string())?;
        let source = self
            .previous_session
            .lock()
            .ok()
            .and_then(|source| source.clone());
        let system_prompt = self
            .system_prompt
            .lock()
            .map(|prompt| prompt.clone())
            .unwrap_or_default();
        let thread = Arc::clone(&self.side_thread);
        let store_dir = self.store_dir.clone();
        let sender = self.sender.clone();
        let model = config.model;
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| "the application service needs an async runtime".to_owned())?;
        handle.spawn(async move {
            let answer = async {
                let (summary, history) = match &source {
                    Some(source) => {
                        let store = SqliteStore::open_read_only(store_dir)
                            .await
                            .map_err(|error| error.to_string())?;
                        let history = harness_runtime::conversation_history(&store, source)
                            .await
                            .map_err(|error| error.to_string())?;
                        (history.summary, history.messages)
                    }
                    None => (None, Vec::new()),
                };
                let earlier = thread.lock().map(|turns| turns.clone()).unwrap_or_default();
                let messages = super::side_question::messages(
                    &system_prompt,
                    summary.as_deref(),
                    history,
                    &earlier,
                    &question,
                );
                // No tools: the side thread answers from the conversation alone.
                let request =
                    harness_providers::ProviderRequest::new(RequestId::generate(), model, messages);
                let events = provider
                    .stream(request, CancellationToken::new())
                    .await
                    .map_err(|error| error.to_string())?;
                let response = harness_providers::assemble_stream(&events)
                    .map_err(|error| error.to_string())?;
                let answer = response.text.trim().to_owned();
                if answer.is_empty() {
                    return Err("the model gave no answer".to_owned());
                }
                if let Ok(mut turns) = thread.lock() {
                    turns.push((question.clone(), answer.clone()));
                }
                Ok(answer)
            }
            .await;
            let _ = sender.send(SessionEvent::SideAnswer { question, answer });
        });
        Ok(())
    }

    fn queue_modes(&self) -> (super::queue::QueueMode, super::queue::QueueMode) {
        let parse = |mode: Option<&String>| {
            mode.and_then(|mode| super::queue::QueueMode::parse(mode))
                .unwrap_or_default()
        };
        self.configured().map_or_else(
            |_| Default::default(),
            |config| {
                (
                    parse(config.queue_modes.0.as_ref()),
                    parse(config.queue_modes.1.as_ref()),
                )
            },
        )
    }

    fn fullscreen_prefs(&self) -> super::tui::fullscreen::Prefs {
        super::tui::fullscreen::prefs(&self.environment, &self.config_file)
    }

    fn set_fullscreen(&mut self, enabled: bool) -> Result<(), String> {
        super::tui::fullscreen::save(&self.config_file, enabled)
    }

    fn subagent_effort(&mut self, change: SubagentSetting) -> Result<String, String> {
        match change {
            SubagentSetting::Show => Ok(match subagent_default_thinking(&self.config_file) {
                Some(level) => format!(
                    "Subagent thinking: {} (settings.json subagentDefaultThinking)",
                    level.as_str()
                ),
                None => {
                    "Subagent thinking: this agent's level (subagentDefaultThinking is not set)"
                        .to_owned()
                }
            }),
            SubagentSetting::Inherit => {
                super::config::save_setting(&self.config_file, "subagentDefaultThinking", None)?;
                Ok("Subagent thinking cleared: children run at this agent's level".to_owned())
            }
            SubagentSetting::Set(level) => {
                let parsed = harness_providers::ThinkingLevel::parse(&level).ok_or_else(|| {
                    format!(
                        "unknown thinking level {level:?}; use off, minimal, low, medium, high, xhigh or max"
                    )
                })?;
                super::config::save_setting(
                    &self.config_file,
                    "subagentDefaultThinking",
                    Some(serde_json::json!(parsed.as_str())),
                )?;
                Ok(format!(
                    "Subagent thinking set: {} (saved as subagentDefaultThinking); a child's model that lacks it uses the nearest level it offers",
                    parsed.as_str()
                ))
            }
        }
    }

    fn subagent_model(&mut self, change: SubagentSetting) -> Result<String, String> {
        match change {
            SubagentSetting::Show => {
                let (model, source) = if let Some(model) = subagent_default_model(&self.config_file)
                {
                    (model, "settings.json subagentDefaultModel")
                } else if let Some(model) = self
                    .configured()
                    .ok()
                    .and_then(|config| config.agents_default_model)
                {
                    (model, "[agents] default_model")
                } else {
                    return Ok(
                        "Subagent model: this agent's model (subagentDefaultModel is not set)"
                            .to_owned(),
                    );
                };
                Ok(format!("Subagent model: {model} ({source})"))
            }
            SubagentSetting::Inherit => {
                super::config::save_setting(&self.config_file, "subagentDefaultModel", None)?;
                Ok("Subagent model cleared: children run on this agent's model".to_owned())
            }
            SubagentSetting::Set(reference) => {
                let base = self.configured()?;
                let (_, entry) = super::routing::resolve_scoped(
                    &base,
                    &reference,
                    &self.environment,
                    &self.data_dir,
                    credentials::Scope::Subagent,
                )
                .map_err(|unusable| match unusable {
                    super::routing::Unusable::NotInCatalog => {
                        format!("model \"{reference}\" is not in the catalog")
                    }
                    super::routing::Unusable::NoCredential(provider) => {
                        format!("model \"{reference}\" has no credential: log in to {provider} with /login")
                    }
                })?;
                let model = entry.reference();
                super::config::save_setting(
                    &self.config_file,
                    "subagentDefaultModel",
                    Some(serde_json::json!(model)),
                )?;
                Ok(format!(
                    "Subagent model set: {model} (saved as subagentDefaultModel); the next child runs on it"
                ))
            }
        }
    }

    fn rlm_max_depth(&mut self, change: Option<(u32, bool)>) -> Result<(), String> {
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| "the application service needs an async runtime".to_owned())?;
        let previous = self
            .previous_session
            .lock()
            .ok()
            .and_then(|source| source.clone());
        let task_id = self.task_id.clone();
        let config_file = self.config_file.clone();
        let environment = self.environment.clone();
        let agents = Arc::clone(&self.agents);
        let sender = self.sender.clone();
        handle.spawn(async move {
            let message = match change {
                None => {
                    let resolved = match agents.store().lease().await {
                        Ok(lease) => {
                            let store = lease.store();
                            let task = conversation_task(&store, previous.as_ref(), task_id).await;
                            resolve_rlm_max_depth(&store, &task, &config_file, &environment).await
                        }
                        Err(_) => agents.max_depth(),
                    };
                    format!("RLM max depth: {} ({})", resolved.0, resolved.1.as_str())
                }
                Some((depth, global)) => {
                    agents.set_max_depth(depth, super::delegation::MaxDepthSource::Chat);
                    // The chat keeps its value: the conversation's task carries it.
                    if let Ok(lease) = agents.store().lease().await {
                        let store = lease.store();
                        let task = conversation_task(&store, previous.as_ref(), task_id).await;
                        let _ = store
                            .set_session_setting(&task, RLM_MAX_DEPTH_SETTING, &depth.to_string())
                            .await;
                    }
                    if global {
                        match super::config::save_setting(
                            &config_file,
                            "rlmMaxDepth",
                            Some(serde_json::json!(depth)),
                        ) {
                            Ok(()) => format!("RLM max depth set: {depth} and saved as global default"),
                            Err(error) => format!(
                                "RLM max depth set for this chat, but the global default was not saved: {error}"
                            ),
                        }
                    } else {
                        format!("RLM max depth set: {depth}")
                    }
                }
            };
            let _ = sender.send(SessionEvent::Notice { message });
        });
        Ok(())
    }

    fn stop_agents(&mut self, selector: &str) -> Result<String, String> {
        let stopped = self
            .agents
            .stop(selector, "Stopped by the user with /agents stop")?;
        Ok(match stopped {
            0 => "no delegated child is running".to_owned(),
            1 => "stopping 1 delegated child".to_owned(),
            count => format!("stopping {count} delegated children"),
        })
    }
}

/// Where a forked conversation starts: at the turn of `from`, before it
/// (`/fork`) or after it (`/clone`).
#[derive(Clone, Debug)]
struct ForkPlan {
    from: SessionId,
    before: bool,
}

impl AgentSessionService {
    /// A new conversation, as `/new` starts one, that its first turn will
    /// continue from the fork point.
    fn start_fork(&mut self, plan: ForkPlan) {
        self.branch_plan = None;
        self.agents.reset();
        // The next conversation reads its own tier.
        if let Ok(mut tier) = self.service_tier.lock() {
            *tier = None;
        }
        if let Ok(mut thread) = self.side_thread.lock() {
            thread.clear();
        }
        self.task_id = TaskId::generate();
        self.follow_schedules();
        if let Ok(mut tracker) = self.cost_tracker.lock() {
            tracker.forget_context();
        }
        if let Ok(mut previous) = self.previous_session.lock() {
            *previous = None;
        }
        self.fork_plan = Some(plan);
    }

    /// Queue `text` into the running turn for its next step: as the user's
    /// steering correction, or - `verbatim` - as another agent's message that
    /// already says who it is from.
    fn queue_into_run(&mut self, text: &str, verbatim: bool) -> Result<(), String> {
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
        // Counted while the inbox is still the running turn's, so a turn that ends
        // meanwhile waits for this message before it looks for unread ones.
        active.in_flight.fetch_add(1, Ordering::SeqCst);
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
                if verbatim {
                    active
                        .inbox
                        .deliver(&run, text, harness_runtime::now_unix_ms())
                        .await
                        .map_err(|error| error.to_string())?;
                } else {
                    active
                        .inbox
                        .steer(&run, text, harness_runtime::now_unix_ms())
                        .await
                        .map_err(|error| error.to_string())?;
                }
                Ok::<(), String>(())
            }
            .await;
            active.in_flight.fetch_sub(1, Ordering::SeqCst);
            if let Err(error) = result {
                let _ = sender.send(SessionEvent::Notice {
                    message: format!("steering note was not queued: {error}"),
                });
            }
        });
        Ok(())
    }
}

/// prime-agent's refinement outcome row, from what `/refine` applied.
fn refined_event(refinement: &super::refine::Refinement) -> SessionEvent {
    SessionEvent::Refined {
        header: refinement.header(),
        summary: refinement.summary(),
        details: refinement.details(),
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
    system_prompt: Arc<Mutex<String>>,
    active_skills: Arc<Mutex<BTreeMap<String, harness_extensions::SkillActivation>>>,
    pending_mcp_elicitations: Arc<Mutex<HashMap<String, PendingMcpElicitationRequest>>>,
    mcp_status: Arc<Mutex<Vec<String>>>,
    agents: Arc<super::delegation::SessionAgents>,
    fork_plan: Option<ForkPlan>,
    branch_plan: Option<super::branch_summary::BranchPlan>,
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
    service_tier: Arc<Mutex<Option<String>>>,
    turns_since_review: Arc<std::sync::atomic::AtomicU32>,
    auto_refine: bool,
    heartbeats: Arc<super::heartbeat::Heartbeats>,
    live: Arc<LiveTurn>,
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
    // The store is the session's, shared with the children that outlive this
    // turn; the lease keeps it open for the turn and lets it close when nobody
    // holds it.
    let lease = match agents.store().lease().await {
        Ok(lease) => lease,
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
    let store = lease.store();

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
    // The first turn of a fork reads the conversation up to the fork point; its
    // later turns continue it through the `forked_from` setting.
    let mut fork_history = None;
    if source.is_none()
        && let Some(plan) = &fork_plan
    {
        let forked_from = if plan.before {
            harness_runtime::previous_in_conversation(&store, &plan.from)
                .await
                .ok()
                .flatten()
        } else {
            Some(plan.from.clone())
        };
        if let Some(forked_from) = forked_from {
            let _ = store
                .set_session_setting(
                    &task_id,
                    harness_runtime::FORKED_FROM_SETTING,
                    forked_from.as_str(),
                )
                .await;
            if let Ok(Some(parent_task)) = store.session_task(&forked_from).await
                && let Ok(Some(title)) = store.session_setting(&parent_task, "title").await
            {
                let _ = store
                    .set_session_setting(&task_id, "title", &format!("{title} (fork)"))
                    .await;
            }
            fork_history = harness_runtime::conversation_history(&store, &forked_from)
                .await
                .ok();
        }
    }
    // The previous turn of this conversation may still hold the task in this
    // open store (a child kept it open); this turn takes the task over, as it did
    // when every turn opened the store anew.
    if let Err(error) = store.release_task_lease(&task_id).await {
        send(SessionEvent::Notice {
            message: format!("the previous turn's hold on this conversation was kept: {error}"),
        });
    }

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
    let mut config = match resolve_provider_with_overrides(
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
    // The service tier: what `/tier` or `/fast` chose (kept with the task), else
    // what the task last used, else the global `defaultServiceTier`, as
    // prime-agent's session entry stands above its default.
    let chosen_tier = service_tier.lock().ok().and_then(|tier| tier.clone());
    config.service_tier = match chosen_tier {
        Some(tier) => {
            let _ = store
                .set_session_setting(&task_id, SERVICE_TIER_SETTING, &tier)
                .await;
            Some(tier)
        }
        None => store
            .session_setting(&task_id, SERVICE_TIER_SETTING)
            .await
            .ok()
            .flatten()
            .or_else(|| default_service_tier(&config_file)),
    };
    if let Ok(mut tier) = service_tier.lock() {
        tier.clone_from(&config.service_tier);
    }

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
        config.provider_id.clone(),
        config.credential_variable(),
        data_dir.clone(),
    ));
    // The thinking level: what `/thinking` chose (kept with the task), else what the
    // task last used, else prime-agent's `defaultThinkingLevel`, else the
    // configuration's.
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
            .or_else(|| default_thinking_level(&config_file))
            .or_else(|| harness_providers::ThinkingLevel::parse(&config.thinking))
            .unwrap_or_default(),
    };
    // What `/model` and `/effort` change while this turn runs reaches its next
    // model call.
    live.start(thinking_level);
    let _ = (credentials, capabilities);
    let provider = LiveProvider::new(
        Arc::clone(&live),
        config.clone(),
        thinking_level,
        task_id.as_ref().to_owned(),
        data_dir.clone(),
    )
    .map(|provider| {
        let router = provider.router_for(
            sender.clone(),
            &environment,
            thinking_level,
            RuntimeConfig::default().max_attempts,
        );
        Arc::new(provider.with_router(router)) as Arc<dyn ModelProvider>
    });
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

    // `[routing] auxiliary`: the model that writes summaries and `/refine`
    // reviews, when it can be used; the session model otherwise.
    let auxiliary = config.routing.auxiliary.as_deref().and_then(|reference| {
        let built = super::routing::resolve(&config, reference, &environment, &data_dir)
            .map_err(|unusable| match unusable {
                super::routing::Unusable::NotInCatalog => "not in the model catalog".to_owned(),
                super::routing::Unusable::NoCredential(provider) => {
                    format!("no credential for {provider}")
                }
            })
            .and_then(|(auxiliary_config, entry)| {
                LiveProvider::build(
                    &auxiliary_config,
                    thinking_level,
                    task_id.as_ref(),
                    &data_dir,
                )
                .map(|provider| (provider, entry))
                .map_err(|error| error.to_string())
            });
        match built {
            Ok(built) => Some(built),
            Err(reason) => {
                send(SessionEvent::Notice {
                    message: super::routing::unusable_auxiliary(
                        reference,
                        "summaries and /refine",
                        &reason,
                    ),
                });
                None
            }
        }
    });
    let (helper_provider, helper_model) = auxiliary.as_ref().map_or_else(
        || (Arc::clone(&provider), config.model.clone()),
        |(auxiliary, entry)| (Arc::clone(auxiliary), entry.id.clone()),
    );
    let mut runtime = RuntimeService::new(
        Arc::clone(&store),
        Arc::clone(&provider),
        RuntimeConfig {
            context_window_tokens: config.context_window_tokens,
            output_reservation_tokens: config.output_reservation_tokens,
            compaction_reserve_tokens: config.compaction_reserve_tokens,
            max_retry_after_seconds: config.max_retry_after_seconds,
            ..RuntimeConfig::default()
        },
    );
    if let Some((auxiliary, entry)) = &auxiliary {
        runtime = runtime.with_summarizer(Arc::new(super::routing::AuxiliarySummary::new(
            entry.reference(),
            Arc::new(harness_runtime::ModelSummaryProvider::new(Arc::clone(
                auxiliary,
            ))),
            Arc::new(harness_runtime::ModelSummaryProvider::new(Arc::clone(
                &provider,
            ))),
            sender.clone(),
        )));
    }
    let runtime = Arc::new(runtime);
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
        // prime-agent's `_syncKernelStateAfterCompaction`: the kernel's namespace is
        // snapshotted and the variables too large to snapshot are removed with it.
        if result.is_ok()
            && let Some(repl) = &repl
            && let Some(pruned) = repl.prune_oversized_variables().await
            && !pruned.is_empty()
        {
            send(SessionEvent::Notice {
                message: format!(
                    "python: variables over the snapshot size limit were removed: {}",
                    pruned.join(", ")
                ),
            });
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
        let result = super::refine::refine(
            &helper_provider,
            &helper_model,
            &conversation,
            &scopes,
            &options,
        )
        .await;
        drop(runtime);
        if let Ok(store) = Arc::try_unwrap(store) {
            let _ = store.close().await;
        }
        match result {
            Ok(refinement) => {
                send(refined_event(&refinement));
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
    // prime-agent reaches MCP servers through the kernel's `mcp` object, not as native
    // tools. With the REPL available the servers are left to it; they are connected
    // natively only when there is no REPL, or when the message attaches one of their
    // resources with `@server:uri`, which only the native client can read.
    let repl_available = match &repl {
        Some(shared) => shared.available().await,
        None => false,
    };
    let attaches_mcp_resource = config
        .mcp_servers
        .keys()
        .any(|server| request.text.contains(&format!("@{server}:")));
    let active_mcp = if config.mcp_servers.is_empty() {
        if let Ok(mut status) = mcp_status.lock() {
            status.clear();
        }
        None
    } else if repl_available && !attaches_mcp_resource {
        if let Ok(mut status) = mcp_status.lock() {
            *status = config
                .mcp_servers
                .keys()
                .map(|server| {
                    format!(
                        "{server}: reached through the Python REPL `mcp` object, as in prime-agent"
                    )
                })
                .collect();
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
    let web_host = super::web::WebHost::from_environment(&environment);
    // Children run on a provider of their own, fixed at the model this turn
    // started with: `/model` in a later turn does not switch a child mid-task.
    let child_provider = LiveProvider::build_scoped(
        &config,
        thinking_level,
        task_id.as_ref(),
        &data_dir,
        credentials::Scope::Subagent,
    )
    .unwrap_or_else(|_| Arc::clone(&provider));
    // The depth children of this turn may spawn to, read for this conversation.
    let (max_depth, depth_source) =
        resolve_rlm_max_depth(&store, &task_id, &config_file, &environment).await;
    agents.set_max_depth(max_depth, depth_source);
    let delegate_host = Some(super::delegation::DelegateHost::new(
        &agents,
        super::delegation::ChildLaunch {
            model: super::delegation::ChildModel {
                provider: child_provider,
                reference: format!("{}/{}", config.provider_id, config.model),
                price: config.model_price,
            },
            models: Some(Arc::new(ServiceChildModels {
                base: config.clone(),
                level: thinking_level,
                environment: environment.clone(),
                data_dir: data_dir.clone(),
                session: task_id.as_ref().to_owned(),
                config_file: config_file.clone(),
                configured: config.agents_default_model.clone(),
            })),
            runtime_config: RuntimeConfig {
                context_window_tokens: config.context_window_tokens,
                output_reservation_tokens: config.output_reservation_tokens,
                compaction_reserve_tokens: config.compaction_reserve_tokens,
                max_retry_after_seconds: config.max_retry_after_seconds,
                ..RuntimeConfig::default()
            },
            workspace_root: workspace_root.clone(),
            workspace: observation.clone(),
            hooks: config.hooks.clone(),
            parent_policy: tool_policy.clone(),
            child_limits: limits,
            web: web_host
                .as_ref()
                .map(|host| (host.tools(), host.dispatcher())),
        },
        cancellation.clone(),
    ));
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
                chain.push(host.rlm_requests());
            }
            // Always answered, even with no server configured: an unanswered
            // `mcp.list_connections` reached the model as "request failed", and it
            // kept retrying a bridge that had nothing behind it.
            chain.push(Arc::new(super::skill_requests::McpRequests::new(
                config.mcp_servers.clone(),
                workspace_root.clone(),
            )));
            let requests: Arc<dyn super::repl::HostRequests> =
                Arc::new(super::skill_requests::ChainedRequests(chain));
            super::repl::ReplHost::for_turn(
                shared,
                requests,
                super::repl::KernelContext {
                    global: super::harness::global_dir(&data_dir),
                    local: super::harness::local_dir(&data_dir, task_id.as_str()),
                    skills: kernel_skills.clone(),
                    kernel_dir: super::repl::kernel_dir(&data_dir, task_id.as_str()),
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
    let mut prompt = format!(
        "{}{}",
        request.text,
        attachments::attachment_blocks(&attached.files)
    );
    // prime-agent's branch summary: the turns `/tree` left are summarised by the
    // auxiliary model (else the session model) and read ahead of this message,
    // so the conversation keeps what that branch found.
    if let (Some(plan), Some(target)) = (&branch_plan, source.as_ref()) {
        match super::branch_summary::summarize(&store, plan, target, Arc::clone(&helper_provider))
            .await
        {
            Ok(Some(summary)) => {
                send(SessionEvent::Notice {
                    message: "Summarized the branch left by /tree".to_owned(),
                });
                prompt = super::branch_summary::wrap(&summary, &prompt);
            }
            Ok(None) => send(SessionEvent::Notice {
                message: "No content to summarize".to_owned(),
            }),
            Err(error) => send(SessionEvent::Notice {
                message: format!(
                    "the branch could not be summarized ({error}); continuing without it"
                ),
            }),
        }
    }
    let loaded_instructions =
        super::instructions::load(&global_config_dir, &workspace_root, &caller_dir);
    for notice in &loaded_instructions.notices {
        send(SessionEvent::Notice {
            message: notice.clone(),
        });
    }
    // Only instructions that were actually loaded are worth a line in the
    // conversation; "0 files" before every answer was noise.
    if !loaded_instructions.files.is_empty() {
        send(SessionEvent::Notice {
            message: format!("AGENTS.md: {} files", loaded_instructions.files.len()),
        });
    }
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
    if repl_host.is_some()
        && let Some(block) = super::prompt::generic_mcp_block(
            &config.mcp_servers.keys().cloned().collect::<Vec<_>>(),
        )
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
    // The first turn of an imported conversation carries the imported messages;
    // later turns reach them through the conversation's first session.
    let imported = if source.is_none() && fork_history.is_none() {
        harness_runtime::imported_messages(&store, &task_id)
            .await
            .ok()
            .filter(|messages| !messages.is_empty())
    } else {
        None
    };
    let mut run_request = RunRequest::new(
        session_id.clone(),
        task_id,
        request.input_id.clone(),
        prompt,
        observation,
    )
    .with_system_policy({
        if let Ok(mut shown) = system_prompt.lock() {
            shown.clone_from(&built_prompt.text);
        }
        built_prompt.text
    })
    .with_project_rules(project_blocks)
    .with_tool_schemas(tool_schemas);
    if let Some(imported) = imported {
        run_request = run_request.with_conversation(imported);
    }
    // A fork's first turn carries the conversation up to its fork point, as a
    // continued turn carries the one before it.
    if let Some(history) = fork_history {
        if let Some(summary) = history.summary {
            run_request = run_request.with_continuation_context(summary);
        }
        run_request = run_request.with_conversation(history.messages);
    }
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
        context_window: config.context_window_tokens,
        provider_id: config.provider_id.clone(),
        auto_allowed_count,
        bell: config.bell,
        tool_started: Mutex::new(Vec::new()),
    });

    let run_inbox = RunInbox::new(Arc::clone(&store));
    if let Ok(mut active) = active_inbox.lock() {
        *active = Some(ActiveTurnInbox {
            inbox: run_inbox.clone(),
            store: Arc::clone(&store),
            session_id: session_id.clone(),
            in_flight: Arc::new(AtomicUsize::new(0)),
        });
    }
    // `[queue] steering_mode`: pa-agent's `one-at-a-time` unless set to `all`.
    let driver = driver.with_inbox(run_inbox).with_steering_all(
        config
            .queue_modes
            .0
            .as_deref()
            .and_then(super::queue::QueueMode::parse)
            == Some(super::queue::QueueMode::All),
    );

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

    // The turn's children belong to the session: they keep running, and what they
    // find reaches the parent as a notice.
    drop(delegate_host);

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

    // No message enters this turn's inbox from here on; what came after its last
    // step goes to the next turn.
    let finished_inbox = active_inbox
        .lock()
        .ok()
        .and_then(|mut active| active.take());
    if let Some(finished) = finished_inbox {
        carry_unread_messages(finished, &sender).await;
    }

    // Release the writer before announcing the terminal event: the next turn takes
    // a newer generation of the task lease, and it must not race this one.
    drop(driver);
    // prime-agent refines at the turn boundary: what the `refine` skill asked for, and
    // every twenty-five turns an automatic review that refines when the trajectory
    // holds something worth keeping. A canceled turn ends now: no model call runs
    // after the user asked it to stop.
    if outcome.is_ok() && !cancellation.is_cancelled() {
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
                match super::refine::review(
                    &helper_provider,
                    &helper_model,
                    &conversation,
                    &scopes,
                    turns,
                )
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
                    Ok(refinement) => send(refined_event(&refinement)),
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
    // A turn that failed or was canceled after it was admitted is still part of the
    // conversation: the next message continues from it, and the runtime replays
    // what it did, as prime-agent keeps an aborted turn's messages.
    if outcome.is_err()
        && store
            .session_task(&session_id)
            .await
            .is_ok_and(|task| task.is_some())
        && let Ok(mut guard) = previous_session.lock()
    {
        *guard = Some(session_id.clone());
    }
    drop(runtime);
    if let Some(repl) = &repl {
        repl.release_host();
    }
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
        // Esc while the model answers or before it was asked: the user stopped
        // the turn, and it says so instead of `failed: provider_canceled`.
        Err(error) if error.code() == ErrorCode::ProviderCanceled => RunOutcome::Canceled,
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
            .ok_or_else(|| "usage: /export [path.md|path.jsonl|path.html]".to_owned())?;
        let extension = Path::new(path).extension();
        let jsonl = extension.is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"));
        let markdown = extension.is_some_and(|extension| extension.eq_ignore_ascii_case("md"));
        let html = extension.is_some_and(|extension| extension.eq_ignore_ascii_case("html"));
        if !jsonl && !markdown && !html {
            return Err("usage: /export [path.md|path.jsonl|path.html]".to_owned());
        }
        if Path::new(path).is_absolute()
            || Path::new(path)
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err("export path must stay inside the workspace".to_owned());
        }
        let content = if jsonl {
            render_session_file(
                &store,
                &task_id,
                source.as_ref(),
                &workspace_root,
                &environment,
                &data_dir,
                &config.provider.api_key_env,
            )
            .await?
        } else if html {
            render_session_html(
                &store,
                &task_id,
                source.as_ref(),
                &environment,
                &data_dir,
                &config.provider.api_key_env,
                &config.provider.model,
            )
            .await?
        } else {
            render_session_export(
                &store,
                &task_id,
                &environment,
                &data_dir,
                &config.provider.api_key_env,
                jsonl,
            )
            .await?
        };
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
        context_window: config.context_window_tokens,
        provider_id: config.provider.id.clone(),
        auto_allowed_count,
        bell: config.bell,
        tool_started: Mutex::new(Vec::new()),
    });
    let name = if is_undo { "undo" } else { "write_file" };
    observer.observe(TurnProgress::StepStarted { step: 1 });
    observer.observe(TurnProgress::ToolStarted {
        name: name.to_owned(),
        call_id: String::new(),
        summary: if is_undo {
            "restore the most recent changed file".to_owned()
        } else {
            "export session transcript".to_owned()
        },
        // The app chose this action itself; the summary already says all of it,
        // and an export's input would be the whole transcript again.
        input: String::new(),
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
                call_id: String::new(),
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
                call_id: String::new(),
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

/// The secrets an export must not contain: the credential variables and the
/// saved credentials.
fn export_secrets(
    environment: &LaunchEnvironment,
    data_dir: &Path,
    provider_key_variable: &str,
) -> Vec<String> {
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
    secrets.extend(credentials::secrets(&credentials::resolve_file(
        environment,
        data_dir,
    )));
    secrets
}

/// `/export session.html`: the conversation as one self-contained page.
async fn render_session_html(
    store: &SqliteStore,
    task_id: &TaskId,
    source: Option<&SessionId>,
    environment: &LaunchEnvironment,
    data_dir: &Path,
    provider_key_variable: &str,
    model: &str,
) -> Result<String, String> {
    const EXPORT_LIMIT: usize = 1024 * 1024;
    let source =
        source.ok_or_else(|| "Nothing to export yet - start a conversation first".to_owned())?;
    let history = harness_runtime::conversation_history(store, source)
        .await
        .map_err(|error| error.to_string())?;
    let title = store
        .session_setting(task_id, "title")
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "Harness session".to_owned());
    let secrets = export_secrets(environment, data_dir, provider_key_variable);
    let page = super::export_html::render(
        &title,
        model,
        history.summary.as_deref(),
        &history.messages,
        &secrets,
    );
    if page.len() > EXPORT_LIMIT {
        return Err("session export exceeds the 1 MiB file limit".to_owned());
    }
    Ok(page)
}

/// `/export session.jsonl`: ha's session file (prime-agent's JSONL shape) -
/// what the model is sent for this conversation, which `/import` reads back.
async fn render_session_file(
    store: &SqliteStore,
    task_id: &TaskId,
    source: Option<&SessionId>,
    workspace_root: &Path,
    environment: &LaunchEnvironment,
    data_dir: &Path,
    provider_key_variable: &str,
) -> Result<String, String> {
    let source = source.ok_or("nothing to export yet: this conversation has no turn")?;
    let history = harness_runtime::conversation_history(store, source)
        .await
        .map_err(|error| error.to_string())?;
    let mut messages = Vec::new();
    if let Some(summary) = history.summary {
        messages.push(harness_providers::ProviderMessage::new(
            harness_providers::MessageRole::System,
            format!("Summary of the earlier conversation:\n{summary}"),
        ));
    }
    messages.extend(history.messages);
    let header = harness_runtime::session_file::SessionFileHeader {
        id: task_id.as_str().to_owned(),
        timestamp_ms: i64::try_from(harness_runtime::now_unix_ms()).unwrap_or(i64::MAX),
        cwd: workspace_root.display().to_string(),
        title: store.session_setting(task_id, "title").await.ok().flatten(),
    };
    let mut text = harness_runtime::session_file::write_session_file(&header, &messages)?;
    for secret in export_secrets(environment, data_dir, provider_key_variable) {
        text = text.replace(&secret, "[REDACTED]");
    }
    Ok(text)
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
    secrets.extend(credentials::secrets(&credentials::resolve_file(
        environment,
        data_dir,
    )));
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
        context_window: config.context_window_tokens,
        provider_id: config.provider.id.clone(),
        auto_allowed_count,
        bell: config.bell,
        tool_started: Mutex::new(Vec::new()),
    });
    observer.observe(TurnProgress::StepStarted { step: 1 });
    observer.observe(TurnProgress::ToolStarted {
        name: "run_shell".to_owned(),
        call_id: String::new(),
        summary: format!("shell: {}", shell_prefix.command),
        // The same shape a model's shell call has, so the expanded view shows the
        // command the user typed like any other shell call.
        input: serde_json::json!({ "command": shell_prefix.command }).to_string(),
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
                call_id: String::new(),
                ok: settled && process_result,
                detail: (!settled || !process_result).then(|| output.clone()),
            });
            (output, settled, outcome)
        }
        Err(error) => {
            let output = format!("not run: {error}");
            observer.observe(TurnProgress::ToolSettled {
                name: "run_shell".to_owned(),
                call_id: String::new(),
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
            call_id: String::new(),
            summary: summary.to_owned(),
            input: String::new(),
        });
        let elapsed = started.elapsed();
        let _ = self.sender.send(SessionEvent::ToolSettled {
            name: name.to_owned(),
            call_id: String::new(),
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
        let request_id = tokio::time::timeout(Duration::from_secs(15), async {
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
        let request_id = tokio::time::timeout(Duration::from_secs(15), async {
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

    /// A message that lands in a turn's inbox after its last step - or is still
    /// on its way there when the turn ends - goes to the next turn instead of
    /// being dropped with the run. (Measured in the full PTY run: a child's
    /// message "reached the running turn" and was never read.)
    #[tokio::test]
    async fn q03_unread_inbox_messages_are_carried_to_the_next_turn() {
        use super::{ActiveTurnInbox, carry_unread_messages};
        use harness_runtime::RunInbox;
        use harness_types::{InputId, SessionId, TaskId};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;
        use tokio::sync::mpsc;
        let temp = tempfile::tempdir().expect("temporary root");
        let store = Arc::new(
            SqliteStore::open_writer(WriterOpenOptions::new(
                temp.path().join("data"),
                harness_types::HostId::generate(),
            ))
            .await
            .expect("store opens"),
        );
        let session = SessionId::generate();
        let task = TaskId::generate();
        let run = store
            .start_run(&session, &task, &InputId::generate(), None)
            .await
            .expect("run starts");
        let inbox = RunInbox::new(Arc::clone(&store));
        inbox
            .deliver(
                &run,
                "[agent-message from child:explorer-1]\n\nlate news",
                1,
            )
            .await
            .expect("delivered");
        let active = ActiveTurnInbox {
            inbox: inbox.clone(),
            store: Arc::clone(&store),
            session_id: session.clone(),
            in_flight: Arc::new(AtomicUsize::new(1)),
        };
        // A user's steer still on its way when the turn ends.
        let late = active.clone();
        let late_run = run.clone();
        let writer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            late.inbox
                .steer(&late_run, "also check the lexer", 2)
                .await
                .expect("steered");
            late.in_flight.fetch_sub(1, Ordering::SeqCst);
        });
        let (sender, mut receiver) = mpsc::unbounded_channel();
        carry_unread_messages(active.clone(), &sender).await;
        writer.await.expect("writer");
        let mut carried = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            if let SessionEvent::UnreadMessage { text, verbatim } = event {
                carried.push((text, verbatim));
            }
        }
        assert_eq!(
            carried,
            [
                (
                    "[agent-message from child:explorer-1]\n\nlate news".to_owned(),
                    true
                ),
                ("also check the lexer".to_owned(), false),
            ]
        );
        // Handed over once: nothing is left for another sweep.
        carry_unread_messages(active, &sender).await;
        assert!(receiver.try_recv().is_err());
        drop(inbox);
        drop(store);
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
            20000,
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

    /// K05: `/status` can name the project scope the store is keyed by.
    ///
    /// The app shows this id nowhere else, so without this line an operator has no
    /// way to name the project a turn wrote. It also
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

    /// Measured in the user's store: a conversation that delegated to children
    /// showed as seven sessions in `/resume`, and "latest" after a restart
    /// continued a child's task instead of the conversation. A child's task is the
    /// parent's work: marked ones, and ones from before the mark (framed
    /// `[task from parent]`, never titled), are left out.
    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "one store holding the user's conversation and three kinds of child"
    )]
    async fn resume_lists_and_continues_the_users_conversations_not_children() {
        let temp = tempfile::tempdir().expect("temporary root");
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&project).expect("project");
        let environment =
            LaunchEnvironment::from_pairs([("HA_HOME", home.to_string_lossy().into_owned())]);
        let context = bootstrap::resolve(LaunchRequest {
            cwd: None,
            caller_dir: project.clone(),
            platform: HostPlatform::current(),
            environment: environment.clone(),
            explicit_data_dir: None,
        })
        .expect("context");
        let channel = SessionChannel::new();
        let service = AgentSessionService::new(&context, environment.clone(), channel.sender());
        let store = Arc::new(
            SqliteStore::open_writer(WriterOpenOptions::new(
                context.project_store_dir(),
                harness_types::HostId::generate(),
            ))
            .await
            .expect("store"),
        );
        let admit = |text: &'static str, task: harness_types::TaskId| {
            let store = Arc::clone(&store);
            let project = project.clone();
            async move {
                let session = harness_types::SessionId::generate();
                let project_id = crate::interactive::project::resolve_project_id(&store, &project)
                    .await
                    .expect("project id");
                SessionService::new(Arc::clone(&store))
                    .admit_input(AdmitInputRequest {
                        session_id: session.clone(),
                        task_id: task,
                        input_id: InputId::generate(),
                        expected_sequence: 1,
                        authority: SourceAuthority::User,
                        raw_text: text.to_owned(),
                        workspace: observe_workspace(project_id, &project).expect("workspace"),
                        initial_plan_items: Vec::new(),
                    })
                    .await
                    .expect("admitted");
                session
            }
        };
        let user_task = harness_types::TaskId::generate();
        let conversation = admit("ls", user_task.clone()).await;
        store
            .set_session_setting(&user_task, "title", "agents test")
            .await
            .expect("title");
        tokio::time::sleep(Duration::from_millis(20)).await;
        // A child the user typed into through "latest" before this fix: kept.
        let typed_into = harness_types::TaskId::generate();
        let typed_session = admit(
            "[task from parent]\n\nAnswer the delegated task below.",
            typed_into.clone(),
        )
        .await;
        store
            .set_session_setting(&typed_into, "title", "tiếp tục")
            .await
            .expect("title");
        tokio::time::sleep(Duration::from_millis(20)).await;
        // Children delegated later: the newest sessions in the store.
        let marked = harness_types::TaskId::generate();
        admit(
            "[task from parent]\n\nAnswer the delegated task below.",
            marked.clone(),
        )
        .await;
        store
            .set_session_setting(
                &marked,
                super::super::delegation::DELEGATED_CHILD_SETTING,
                "explorer",
            )
            .await
            .expect("mark");
        let legacy = harness_types::TaskId::generate();
        admit(
            "[task from parent]\n\nAnswer the delegated task below.",
            legacy.clone(),
        )
        .await;

        let listed =
            super::user_conversation_sessions(&store, store.list_sessions().await.expect("list"))
                .await
                .into_iter()
                .map(|session| session.task_id)
                .collect::<std::collections::HashSet<_>>();
        assert!(listed.contains(&user_task));
        assert!(listed.contains(&typed_into));
        assert!(!listed.contains(&marked), "a marked child is left out");
        assert!(!listed.contains(&legacy), "an unmarked child is left out");
        Arc::try_unwrap(store)
            .expect("store consumers released")
            .close()
            .await
            .expect("close store");
        // "latest" is the newest conversation of the user's, not the children
        // that wrote after it.
        let latest = service.newest_project_session().expect("latest");
        assert_eq!(latest, typed_session);
        let _ = conversation;
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
            error.contains("/login"),
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

    /// K02: the file `/login` writes is a real credential source, and the
    /// environment still beats it.
    #[test]
    fn k02_a_saved_file_configures_the_provider_and_the_environment_still_wins() {
        let (environment, data_dir) = credential_environment(&[]);
        let path = credentials::resolve_file(&environment, &data_dir);
        credentials::save(
            &path,
            "deepseek",
            &credentials::Credential::api_key("sk-from-file"),
        )
        .expect("the key is saved");

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
        let _ = EnvironmentCredential::new("deepseek", "DEEPSEEK_API_KEY", data_dir.clone());
        let _ = std::fs::remove_file(&path);
    }

    /// K02: the file `/login` writes is what the resolver reads, and a corrupt file
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

        credentials::save(
            &path,
            "deepseek",
            &credentials::Credential::api_key("sk-validated"),
        )
        .expect("the key is saved");
        validate_credential_file(&environment, &data_dir).expect("the saved file is readable");

        std::fs::write(&path, "{\"deepseek\": unquoted").expect("fixture");
        let error = validate_credential_file(&environment, &data_dir)
            .expect_err("a corrupt file must stop the launch");
        assert!(error.contains("auth.json"), "{error}");
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
        credentials::save(
            &path,
            "deepseek",
            &credentials::Credential::api_key("sk-must-not-be-rendered"),
        )
        .expect("the key is saved");

        let lines = provider_diagnostics(&environment, &data_dir);
        let joined = lines.join("\n");
        assert!(joined.contains("auth.json"), "{joined}");
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
        assert!(empty.contains("no credential for deepseek"), "{empty}");
        assert!(
            empty.contains("/login"),
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

        // A DeepSeek model takes the default tier only, as in prime-agent: `/tier
        // priority` is refused with the tiers it does take.
        let mut service = service;
        assert_eq!(
            service.service_tier(),
            ("default".to_owned(), vec!["default"])
        );
        let refused = service
            .set_service_tier("priority")
            .expect_err("deepseek has no priority tier");
        assert_eq!(
            refused,
            "Service tier 'priority' is not available for the current model. Available: default"
        );
    }

    /// `/subagent-model` saves prime-agent's `subagentDefaultModel` for a catalog
    /// model with a credential, reports it, and `inherit` removes it.
    #[test]
    fn the_subagent_model_command_saves_prime_agents_setting() {
        let temp = tempfile::tempdir().expect("temp");
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&project).expect("project");
        std::fs::write(
            home.join("config.toml"),
            "schema_version = 2
",
        )
        .expect("config");
        let environment = LaunchEnvironment::from_pairs([
            ("HA_HOME", home.to_string_lossy().into_owned()),
            ("DEEPSEEK_API_KEY", "fixture-secret-value".to_owned()),
        ]);
        let context = bootstrap::resolve(LaunchRequest {
            cwd: None,
            caller_dir: project,
            platform: HostPlatform::current(),
            environment: environment.clone(),
            explicit_data_dir: None,
        })
        .expect("context");
        let channel = SessionChannel::new();
        let mut service = AgentSessionService::new(&context, environment, channel.sender());
        let shown = service
            .subagent_model(super::SubagentSetting::Show)
            .expect("shown");
        assert!(shown.contains("this agent's model"), "{shown}");
        let set = service
            .subagent_model(super::SubagentSetting::Set(
                "deepseek/deepseek-v4-pro".to_owned(),
            ))
            .expect("saved");
        assert!(set.contains("deepseek/deepseek-v4-pro"), "{set}");
        assert_eq!(
            super::subagent_default_model(&context.paths.config_file).as_deref(),
            Some("deepseek/deepseek-v4-pro")
        );
        let shown = service
            .subagent_model(super::SubagentSetting::Show)
            .expect("shown");
        assert!(
            shown.contains("deepseek/deepseek-v4-pro (settings.json"),
            "{shown}"
        );
        assert!(
            service
                .subagent_model(super::SubagentSetting::Set("nowhere/no-model".to_owned()))
                .is_err()
        );
        service
            .subagent_model(super::SubagentSetting::Inherit)
            .expect("cleared");
        assert_eq!(
            super::subagent_default_model(&context.paths.config_file),
            None
        );
    }

    /// prime-agent's `defaultThinkingLevel`: a level name from `settings.json`,
    /// anything else is unset.
    #[test]
    fn the_default_thinking_level_is_prime_agents_setting() {
        let home = tempfile::tempdir().expect("temp");
        let config = home.path().join("config.toml");
        assert_eq!(super::default_thinking_level(&config), None);
        std::fs::write(
            home.path().join("settings.json"),
            r#"{"defaultThinkingLevel": "xhigh"}"#,
        )
        .expect("settings");
        assert_eq!(
            super::default_thinking_level(&config),
            Some(harness_providers::ThinkingLevel::Xhigh)
        );
        std::fs::write(
            home.path().join("settings.json"),
            r#"{"defaultThinkingLevel": "huge"}"#,
        )
        .expect("settings");
        assert_eq!(super::default_thinking_level(&config), None);
    }

    /// `/subagent-login`: a delegated child reads its own login for a
    /// provider first and the main one otherwise; the main model never reads
    /// the children's.
    #[test]
    fn a_child_uses_its_own_login_and_the_main_model_never_does() {
        use harness_providers::CredentialResolver;
        let temp = tempfile::tempdir().expect("temp");
        let data_dir = temp.path().to_path_buf();
        let environment = LaunchEnvironment::capture();
        let main = credentials::scoped_file(&environment, &data_dir, credentials::Scope::Main);
        let own = credentials::scoped_file(&environment, &data_dir, credentials::Scope::Subagent);
        assert_ne!(main, own);
        credentials::save(
            &main,
            "deepseek",
            &credentials::Credential::api_key("main-key"),
        )
        .expect("main login");
        let child = EnvironmentCredential::new("deepseek", "", data_dir.clone())
            .with_scope(credentials::Scope::Subagent);
        assert_eq!(child.resolve().expect("falls back"), "main-key");
        credentials::save(
            &own,
            "deepseek",
            &credentials::Credential::api_key("child-key"),
        )
        .expect("child login");
        assert_eq!(child.resolve().expect("its own"), "child-key");
        let parent = EnvironmentCredential::new("deepseek", "", data_dir.clone());
        assert_eq!(parent.resolve().expect("main"), "main-key");
        assert!(matches!(
            credentials::source_for_scope(&environment, &data_dir, "deepseek", "", credentials::Scope::Subagent),
            Some(CredentialSource::File { path, .. }) if path == own
        ));
    }

    /// prime-agent's `subagentDefaultModel`: a trimmed string from
    /// `settings.json`, anything else is unset.
    #[test]
    fn the_subagent_default_model_is_prime_agents_setting() {
        let home = tempfile::tempdir().expect("temp");
        let config = home.path().join("config.toml");
        assert_eq!(super::subagent_default_model(&config), None);
        std::fs::write(
            home.path().join("settings.json"),
            r#"{"subagentDefaultModel": " openai/gpt-5.5 "}"#,
        )
        .expect("settings");
        assert_eq!(
            super::subagent_default_model(&config).as_deref(),
            Some("openai/gpt-5.5")
        );
        for unset in [
            r#"{"subagentDefaultModel": 42}"#,
            r#"{"subagentDefaultModel": "  "}"#,
        ] {
            std::fs::write(home.path().join("settings.json"), unset).expect("settings");
            assert_eq!(super::subagent_default_model(&config), None, "{unset}");
        }
    }

    /// prime-agent's `/logs` shows the directory and each log as
    /// `• name (N.N KB)`; ha's logs sit under the data directory.
    #[test]
    fn logs_lists_every_log_under_the_data_directory() {
        let temp = tempfile::tempdir().expect("temp");
        let data = temp.path();
        assert_eq!(
            super::log_lines(data),
            [
                format!("Directory: {}", data.display()),
                "No logs written yet.".to_owned()
            ]
        );
        std::fs::create_dir_all(data.join("repl")).expect("repl");
        std::fs::write(
            data.join("repl").join("kernel-stderr.log"),
            vec![b'x'; 2048],
        )
        .expect("log");
        std::fs::create_dir_all(data.join("sessions").join("c1").join("kernel")).expect("kernel");
        std::fs::write(
            data.join("sessions")
                .join("c1")
                .join("kernel")
                .join("kernel-stderr.log"),
            "boom",
        )
        .expect("log");
        std::fs::write(data.join(".hidden.log"), "x").expect("hidden");
        std::fs::create_dir_all(data.join("sessions").join("c2").join("kernel")).expect("kernel");
        std::fs::write(
            data.join("sessions")
                .join("c2")
                .join("kernel")
                .join("kernel-stderr.log"),
            "",
        )
        .expect("an empty log");
        std::fs::create_dir_all(data.join("kernel-venv").join("Lib")).expect("venv");
        std::fs::write(data.join("kernel-venv").join("Lib").join("pip.log"), "x")
            .expect("a log in the venv");
        std::fs::write(data.join("notes.txt"), "x").expect("not a log");
        assert_eq!(
            super::log_lines(data)[1..],
            [
                "• repl/kernel-stderr.log (2.0 KB)".to_owned(),
                "• sessions/c1/kernel/kernel-stderr.log (0.0 KB)".to_owned(),
            ]
        );
    }
}
