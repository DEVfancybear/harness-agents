//! State reducer for the interactive app.
//!
//! The controller owns the phase, the history, the editor and the compiled
//! effects. It never touches a terminal, so every rule below is unit tested
//! without a PTY; the host translates effects into terminal calls.
//!
//! T02 changed the output type, not the rules: effects now carry a typed
//! [`HistoryItem`] (or raw stream text) instead of a pre-rendered string, and
//! [`InteractiveController::ui_state`] exposes the snapshot the TUI viewport
//! draws. The plain renderer still prints [`view::plain_lines`] of the same item,
//! which is required to reproduce the pre-T02 transcript exactly.

use std::time::{Duration, Instant};

use harness_types::InputId;

use super::attachments::{self, paths_touched};
use super::bootstrap::LaunchContext;
use super::bounds::{self, DEFAULT_CONTINUATIONS};
use super::credentials::CredentialSource;
use super::events::ShellPrefixMode;
use super::events::{
    AppPhase, HistoryItem, Key, Modal, RunOutcome, SessionCandidate, SessionEvent, ToolState,
    UiState,
};
use super::goal::{GoalState, GoalStatus};
use super::input::{InputOutcome, LineEditor};
use super::service::{ApprovalDecision, SessionChannel, SessionPort, ShellPrefix, SubmitRequest};
use super::view;

/// Exit code for a normal quit.
pub const EXIT_SUCCESS: u8 = 0;

/// Default step and tool-call bounds, so the status bar can show `step 2/8` and
/// `tools 2/16` before the first event arrives.
///
/// These mirror `TurnLimits::default()` in `harness-tools`; the real service
/// reports the limits it actually uses through `SessionPort::limits`.
pub const DEFAULT_MAX_STEPS: u32 = 8;
pub const DEFAULT_MAX_TOOL_CALLS: u32 = 16;

/// What the app sends when it continues a turn a bound stopped.
///
/// It says why it is speaking, because a bare `continue` makes the model guess whether
/// the user asked something new or the last turn was cut short.
const CONTINUATION_TEXT: &str = "continue: the previous turn stopped at a bound, not because the task was \
     finished — pick up where it left off and complete the task";

/// How long the approval panel has before the gate refuses the request.
pub const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_mins(5);

/// One thing the host must do after a controller step.
///
/// The exit code is a plain number so effects stay comparable in tests; the host
/// maps it to a process exit code.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Effect {
    /// Add one entry to the scrollback.
    History(HistoryItem),
    /// Append stream text exactly as given, for incremental output.
    Stream(String),
    /// TUI-only provider reasoning, excluded from plain transcript output.
    Thinking(String),
    /// Draw the whole scrollback again in this detail mode (ctrl+o).
    Reprint(super::events::Detail),
    /// Take over a conversation a background agent already drew (`ha attach`):
    /// the renderer keeps these rows for ctrl+o and draws the newest that fit.
    Restore(Vec<HistoryItem>),
    /// Copy the latest assistant answer (TUI mode only).
    Copy(String),
    /// Emit BEL in the interactive terminal.
    Bell,
    /// Clear only the TUI viewport; terminal scrollback and transcript stay intact.
    ClearViewport,
    /// Repaint the viewport (the composer, the live block and the status bar).
    Redraw,
    /// Enter or leave prime-agent's fullscreen rendering (`/fullscreen`).
    Fullscreen(bool),
    /// prime-agent's external-editor handoff: the terminal edits this draft in
    /// `$VISUAL`/`$EDITOR` and sends the result back as [`Key::SetDraft`].
    EditExternally(String),
    /// The opening banner names a model or a thinking level that has changed: draw
    /// it again with these lines, wherever the banner still is on screen.
    Banner(Vec<String>),
    /// Leave the app with this exit code.
    Exit(u8),
}

/// The step and tool-call bounds the status bar shows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TurnBounds {
    pub max_steps: u32,
    pub max_tool_calls: u32,
}

impl Default for TurnBounds {
    fn default() -> Self {
        Self {
            max_steps: DEFAULT_MAX_STEPS,
            max_tool_calls: DEFAULT_MAX_TOOL_CALLS,
        }
    }
}

/// One gated action waiting for the user's answer.
#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingApproval {
    request_id: String,
    action: String,
    summary: String,
    rule_pattern: String,
    always_allow_confirm: bool,
    workspace: String,
    scope: String,
    expires_at: Instant,
    /// Whether the action only reads, so the panel offers the wider grant only
    /// where granting it means something.
    read_only: bool,
    scroll: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingQuestion {
    question_id: String,
    prompt: String,
    options: Vec<String>,
    /// The option the arrows have highlighted.
    selected: usize,
    /// A delegated child's question: its answer goes back to the waiting
    /// child instead of starting a turn.
    child_request: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingMcpElicitation {
    request_id: String,
    server: String,
    message: String,
    requested_schema: Option<serde_json::Value>,
    url: Option<String>,
}

/// The width `/more` draws for until the TUI reports the console's.
const DEFAULT_COLUMNS: u16 = 100;

/// One entry of what `/more` can show again.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Recalled {
    /// A line exactly as the transcript printed it.
    Line(String),
    /// A tool call or its output, drawn when `/more` opens and in the detail mode
    /// of that moment: the transcript has one line for a call and nothing for what
    /// it returned, and looking back in expanded mode is how a user reads exactly
    /// what ran.
    Tool(HistoryItem),
}

/// How many lines of a running call's output the live block shows.
const LIVE_TOOL_LINES: usize = 3;
/// How much of a running call's output is kept to find those lines.
const TOOL_PROGRESS_BYTES: usize = 8 * 1024;

/// The last `count` non-empty lines of `text`, a carriage return keeping only
/// what a progress bar last drew on its line.
fn last_lines(text: &str, count: usize) -> Vec<String> {
    let mut lines = text
        .lines()
        .map(|line| {
            line.rsplit('\r')
                .next()
                .unwrap_or(line)
                .trim_end()
                .to_owned()
        })
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    let start = lines.len().saturating_sub(count);
    lines.drain(..start);
    lines
}

/// Interactive app state and its transitions.
pub struct InteractiveController {
    phase: AppPhase,
    setup_required: bool,
    header: Vec<String>,
    setup_hint: Option<String>,
    /// The launch context this session started from. Kept so `/login` can refresh
    /// the provider state from the same rules the launch used, instead of writing
    /// a second copy of them here.
    context: LaunchContext,
    /// The committed plain transcript, byte for byte what the old controller
    /// pushed. Kept as text so U20 can be asserted without a renderer.
    transcript: Vec<String>,
    /// The newest entries of the same transcript, so `/more` can show them again in
    /// a scrollable panel without the terminal's scrollback.
    recall: Vec<Recalled>,
    /// The console width the TUI last reported, so `/more` can draw an expanded
    /// tool block to fit its panel.
    columns: u16,
    editor: LineEditor,
    service: Box<dyn SessionPort>,
    channel: SessionChannel,
    /// Model text that has not been printed yet.
    pending_text: String,
    /// The assistant answer accumulated for `/copy`.
    last_answer: String,
    /// Number of line breaks in `pending_text`. It is maintained as deltas
    /// arrive, so the common streaming path does not rescan an ever-growing
    /// buffer on every token.
    pending_newlines: usize,
    pending_approval: Option<PendingApproval>,
    pending_question: Option<PendingQuestion>,
    /// Questions and approvals that arrived while another was open, oldest
    /// first: several agents asking at once are asked one after another.
    question_queue: std::collections::VecDeque<PendingQuestion>,
    approval_queue: std::collections::VecDeque<PendingApproval>,
    /// The phase a child's question interrupted, restored once it is answered.
    phase_before_question: Option<AppPhase>,
    pending_mcp_elicitation: Option<PendingMcpElicitation>,
    /// Whether the user allowed every gated action for the run now in flight.
    ///
    /// Mirrored here so the status row can say the gate is open, and cleared by
    /// `finish_run` so the grant covers one turn rather than the session: a new
    /// request never inherits the last turn's permission.
    granted_for_run: bool,
    /// Last resume listing, so a number can select from it.
    session_candidates: Vec<SessionCandidate>,
    /// What the user queued while the agent worked: prime-agent's steering and
    /// follow-up lanes.
    queue: super::queue::InputQueue,
    /// prime-agent's prompt stash (Ctrl-S): one draft put aside.
    stash: Option<String>,
    /// A `/btw` side question is being answered.
    side_pending: bool,
    /// The provider-usage wait line, while the running turn waits.
    provider_wait: Option<String>,
    /// The conversation's turns from the last `/fork` or `/tree` listing:
    /// session id and the message that opened the turn.
    turn_points: Vec<(String, String)>,
    /// Bounded shell output blocks waiting to ride with the next model message.
    pending_shell_outputs: Vec<String>,
    /// The shared editor picker is showing files instead of persisted sessions.
    file_picker_active: bool,
    file_picker_query: String,
    file_picker_candidates: Vec<String>,
    /// The plain renderer prints slash-command output; the TUI opens an overlay.
    plain: bool,
    /// Whether the terminal renders fullscreen, for `/fullscreen` to toggle.
    fullscreen: bool,
    /// The chat's name `/name` gave it, for prime-agent's fullscreen top bar.
    chat_title: Option<String>,
    /// The tool cards still open, so each settles in place: the call's id, name,
    /// one-line summary and the full input the settled row carries for the
    /// expanded view.
    open_tools: Vec<(String, String, String, String)>,
    /// Why the next tool runs without a panel, shown on its card in the TUI.
    pending_allowance: Option<String>,
    /// What each running call has printed, bounded, by call id: the live
    /// block shows the last lines of the newest under its card.
    tool_progress: Vec<(String, String)>,
    // Progress accounting for the status bar.
    steps: u32,
    tool_calls: u32,
    bounds: TurnBounds,
    /// Automatic continuations used since the user last spoke, and the budget for them.
    continuations: u32,
    max_continuations: u32,
    run_started_at: Option<Instant>,
    last_run_elapsed: Duration,
    last_request: Option<String>,
    /// Why the host is not using the TUI, when it fell back to plain.
    fallback_reason: Option<String>,
    /// Tick counter, so the spinner animates and an idle loop stays still.
    tick: u64,
    /// A quit requested during a run is completed only after the service emits
    /// its terminal event and releases the writer it owns.
    exit_after_run: bool,
    /// The app runs as a background agent (`ha attach`): `/quit` closes the
    /// client and leaves the agent working, as prime-agent's daemon does.
    detachable: bool,
    /// The persistent goal of this conversation, set by `/goal`.
    goal: Option<GoalState>,
    /// A compaction the `compact` skill asked for (with its guidance, possibly empty),
    /// run when the turn ends.
    pending_compact: Option<String>,
    /// Heartbeats that came due while the session was busy, sent when it is free.
    pending_heartbeats: std::collections::VecDeque<String>,
    /// What delegated children sent while the parent was busy - prime-agent's
    /// terminal notices and child messages - read in turns of their own once it is
    /// free, as prime-agent queues them on its follow-up lane.
    pending_notices: std::collections::VecDeque<String>,
    /// After the user stopped a turn, children's notices wait for the user's next
    /// turn to end rather than starting work the user just interrupted.
    notices_held: bool,
    /// What the tool about to settle returned, shown under its card in the TUI.
    pending_tool_output: Option<String>,
    /// prime-agent's detail mode, cycled with ctrl+o.
    detail: super::events::Detail,
    /// When ctrl+c last found nothing to interrupt or clear, for the second press
    /// that exits (prime-agent's "Press ctrl+c again to exit").
    exit_armed_at: Option<Instant>,
    /// prime-agent's double-Escape: when the first idle Escape was pressed.
    escape_armed_at: Option<Instant>,
    /// The shared picker is prime-agent's tree (or fork) selector.
    turn_picker: Option<super::events::TurnsPurpose>,
    /// The shared picker is `/plugins`: the service id of each row.
    plugin_picker: Option<Vec<String>>,
    /// A tool call's arguments are streaming (prime-agent's "Writing code").
    writing_code: bool,
    /// The prompt the app was started with, until it is sent.
    initial_prompt: Option<String>,
    /// prime-agent's `agentMessagesPaused`: other agents' messages are refused.
    agent_messages_paused: bool,
    /// The queued messages other agents sent, which `/agents messages clear`
    /// drops.
    agent_messages_queued: Vec<String>,
    /// `--thinking`: the level the session starts at, until it is applied.
    initial_thinking: Option<String>,
    /// `--goal`: the goal the session starts with, until it is set.
    initial_goal: Option<String>,
    /// The provider whose API key the masked prompt is collecting.
    login_provider: Option<String>,
    /// Whose login the running `/login` or `/subagent-login` saves.
    login_scope: super::credentials::Scope,
    /// Reasoning received since the last committed row.
    pending_thinking: String,
    /// The thinking level the next turn uses, for the status line.
    thinking_label: Option<String>,
    tier_label: Option<String>,
    /// The provider whose browser sign-in waits; a pasted redirect URL finishes it.
    signing_in: Option<String>,
    /// prime-agent's autonomous mode (`/autonomous`).
    autonomous: super::autonomous::Autonomous,
    /// The autonomous quality gates are running; their verdict decides what follows.
    gates_pending: bool,
    /// The session's tokens when the running turn started, for its share.
    turn_tokens_start: u64,
}

impl InteractiveController {
    /// Build the controller for one launch.
    ///
    /// `plain` selects how slash-command output is delivered: the plain renderer
    /// prints it as history lines, the TUI opens it as a modal overlay.
    #[must_use]
    pub fn new(
        context: &LaunchContext,
        service: Box<dyn SessionPort>,
        channel: SessionChannel,
        plain: bool,
    ) -> Self {
        let mut header = context.header_lines();
        header.push(format!("Service: {}", service.label()));
        let bounds = service.limits();
        let (steering_mode, follow_up_mode) = service.queue_modes();
        let fullscreen = !plain && service.fullscreen_prefs().enabled;
        Self {
            fullscreen,
            chat_title: None,
            // The app boots before it can render; boot_lines performs the
            // transition once the header has actually been produced.
            phase: AppPhase::Booting,
            setup_required: context.setup_required,
            header,
            setup_hint: context.setup_hint(),
            context: context.clone(),
            transcript: Vec::new(),
            recall: Vec::new(),
            columns: DEFAULT_COLUMNS,
            editor: LineEditor::new(),
            service,
            channel,
            pending_text: String::new(),
            last_answer: String::new(),
            pending_newlines: 0,
            pending_approval: None,
            pending_question: None,
            question_queue: std::collections::VecDeque::new(),
            approval_queue: std::collections::VecDeque::new(),
            phase_before_question: None,
            pending_mcp_elicitation: None,
            granted_for_run: false,
            session_candidates: Vec::new(),
            queue: super::queue::InputQueue::new(steering_mode, follow_up_mode),
            stash: None,
            side_pending: false,
            provider_wait: None,
            turn_points: Vec::new(),
            pending_shell_outputs: Vec::new(),
            file_picker_active: false,
            file_picker_query: String::new(),
            file_picker_candidates: Vec::new(),
            plain,
            open_tools: Vec::new(),
            tool_progress: Vec::new(),
            pending_allowance: None,
            steps: 0,
            tool_calls: 0,
            bounds,
            continuations: 0,
            max_continuations: DEFAULT_CONTINUATIONS,
            run_started_at: None,
            last_run_elapsed: Duration::ZERO,
            last_request: None,
            fallback_reason: None,
            tick: 0,
            exit_after_run: false,
            detachable: false,
            goal: None,
            pending_compact: None,
            pending_heartbeats: std::collections::VecDeque::new(),
            pending_notices: std::collections::VecDeque::new(),
            notices_held: false,
            pending_tool_output: None,
            detail: super::events::Detail::default(),
            exit_armed_at: None,
            escape_armed_at: None,
            turn_picker: None,
            plugin_picker: None,
            writing_code: false,
            initial_prompt: None,
            agent_messages_paused: false,
            agent_messages_queued: Vec::new(),
            initial_thinking: None,
            initial_goal: None,
            login_provider: None,
            login_scope: super::credentials::Scope::Main,
            signing_in: None,
            pending_thinking: String::new(),
            thinking_label: None,
            tier_label: None,
            autonomous: super::autonomous::Autonomous::default(),
            gates_pending: false,
            turn_tokens_start: 0,
        }
    }

    /// Run as a background agent: `/quit` detaches the client instead of ending
    /// the run, and the agent keeps its turns, children and schedules.
    pub const fn set_detachable(&mut self) {
        self.detachable = true;
    }

    /// What keeps a background agent from being idle: a run, a question or an
    /// approval it waits on, a child still working, or a job it will start.
    #[must_use]
    /// prime-agent's `abort` from outside the terminal: cancel the running
    /// turn. `false`, and nothing done, when there is none.
    pub fn abort_run(&mut self) -> (bool, Vec<Effect>) {
        if !self.phase.has_active_run() {
            return (false, Vec::new());
        }
        (true, self.interrupt())
    }

    /// The last answer the model gave, as `/copy` takes it.
    #[must_use]
    pub fn last_answer(&self) -> &str {
        &self.last_answer
    }

    /// `/schedule` from outside the terminal (`ha schedule`).
    ///
    /// # Errors
    /// The command's own usage or store errors.
    pub fn schedule(&mut self, argument: &str) -> Result<Vec<String>, String> {
        self.service.schedule(Some(argument))
    }

    pub fn is_busy(&self) -> bool {
        self.phase.has_active_run()
            || self.pending_approval.is_some()
            || self.pending_question.is_some()
            || self.service.running_children() > 0
    }

    /// Whether a schedule or a heartbeat will start a turn on its own.
    #[must_use]
    pub fn has_scheduled_work(&self) -> bool {
        self.service.has_scheduled_work()
    }

    /// The model this agent's turns use, as the session port names it.
    #[must_use]
    pub fn model_label(&self) -> String {
        self.service.label()
    }

    /// The conversation the agent is in, once it has one.
    #[must_use]
    pub fn conversation_id(&self) -> Option<String> {
        self.service.conversation_id()
    }

    /// Choose the thinking level, as `/effort` does.
    ///
    /// # Errors
    /// The model does not have that level.
    pub fn set_thinking(&mut self, level: &str) -> Result<String, String> {
        self.service.set_thinking(level)
    }

    /// Let the agent's code start separate top-level sessions
    /// (`rlm.create_session`).
    pub fn set_session_host(&mut self, host: std::sync::Arc<dyn super::agents::SessionHost>) {
        self.service.set_session_host(host);
    }

    /// A message from outside the terminal (`ha send`), delivered as
    /// prime-agent's daemon delivers one: an idle agent starts a turn with it, a
    /// busy one reads it at its next step (`auto`, `steer`) or after the turn
    /// (`follow_up`). Returns what happened: `delivered` or `queued`.
    /// Whether other agents' messages are refused (`/agents messages pause`).
    #[must_use]
    pub const fn agent_messages_paused(&self) -> bool {
        self.agent_messages_paused
    }

    /// [`Self::deliver_external`] for a message another agent sent: one that
    /// waits in the queue can be cleared with `/agents messages clear`.
    pub fn deliver_external_from(
        &mut self,
        text: String,
        steer: bool,
        from_agent: bool,
        effects: &mut Vec<Effect>,
    ) -> &'static str {
        let outcome = self.deliver_external(text.clone(), steer, effects);
        if from_agent && outcome == "queued" {
            self.agent_messages_queued.push(text);
        }
        outcome
    }

    pub fn deliver_external(
        &mut self,
        text: String,
        steer: bool,
        effects: &mut Vec<Effect>,
    ) -> &'static str {
        if self.free_for_automatic_turn() {
            effects.extend(self.submit(text));
            return "delivered";
        }
        if steer && self.phase == AppPhase::Running && self.service.steer(&text).is_ok() {
            effects.push(Effect::History(HistoryItem::User { text }));
            effects.push(Effect::History(HistoryItem::Notice {
                message: "a message reached the running agent; it reads this at its next step"
                    .to_owned(),
            }));
            effects.push(Effect::Redraw);
            return "delivered";
        }
        let lane = if steer {
            super::queue::Lane::Steer
        } else {
            super::queue::Lane::FollowUp
        };
        self.queue.push(lane, text);
        effects.push(Effect::History(HistoryItem::Notice {
            message: format!(
                "a message is queued ({}): sent after the active run finishes",
                self.queue.len()
            ),
        }));
        effects.push(Effect::Redraw);
        "queued"
    }

    /// Stop the agent's work before it goes away: the running turn and every
    /// child it delegated.
    pub fn shut_down(&mut self) {
        if self.phase.has_active_run() {
            self.service.cancel();
        }
        let _ = self.service.stop_agents("all");
    }

    /// Record why the host is using the plain renderer instead of the TUI.
    ///
    /// The reason is shown once, on stderr, by the host; keeping it here makes it
    /// inspectable from a unit test and visible in the status bar.
    pub fn set_fallback_reason(&mut self, reason: impl Into<String>) {
        self.fallback_reason = Some(reason.into());
    }

    /// Set how many times one request may be continued automatically.
    ///
    /// The host reads it from the environment; a budget of zero turns the app back into
    /// one that stops at every bound and waits for the user.
    #[must_use]
    pub fn with_continuations(mut self, budget: u32) -> Self {
        self.max_continuations = budget;
        self
    }

    /// prime-agent's initial message: the prompt `ha "..."` was started with,
    /// sent as soon as the app is up.
    #[must_use]
    pub fn with_initial_prompt(mut self, prompt: Option<String>) -> Self {
        self.initial_prompt = prompt.filter(|prompt| !prompt.trim().is_empty());
        self
    }

    /// prime-agent's `--thinking`: the thinking level the session starts at,
    /// applied as `/effort` applies one.
    #[must_use]
    pub fn with_initial_thinking(mut self, level: Option<String>) -> Self {
        self.initial_thinking = level.filter(|level| !level.trim().is_empty());
        self
    }

    /// `--goal`: the goal the session starts with, set as `/goal <objective>`
    /// sets one. With an initial message the message is the first turn and
    /// the goal continues it.
    #[must_use]
    pub fn with_initial_goal(mut self, goal: Option<String>) -> Self {
        self.initial_goal = goal.filter(|goal| !goal.trim().is_empty());
        self
    }

    #[cfg(test)]
    #[must_use]
    pub const fn phase(&self) -> AppPhase {
        self.phase
    }

    /// How many automatic continuations one request may use.
    #[cfg(test)]
    #[must_use]
    pub const fn continuation_budget(&self) -> u32 {
        self.max_continuations
    }

    /// The committed transcript, exactly as the plain renderer printed it.
    #[cfg(test)]
    #[must_use]
    pub fn transcript(&self) -> &[String] {
        &self.transcript
    }

    /// Prompt text for the current phase and buffer.
    ///
    /// While a secret is being entered the buffer is masked here, at the single
    /// point every renderer reads, instead of relying on each renderer to mask it.
    #[must_use]
    pub fn prompt(&self) -> String {
        view::prompt_line(self.phase, &self.display_buffer())
    }

    /// The prompt as terminal rows: a multi-line draft is one prompt, not several.
    #[must_use]
    pub fn prompt_lines(&self) -> Vec<String> {
        view::prompt_lines(self.phase, &self.display_buffer())
    }

    /// Row and column for the terminal cursor inside the current prompt.
    #[must_use]
    pub fn prompt_cursor_cell(&self) -> (usize, usize) {
        view::cursor_cell(self.phase, &self.display_buffer(), self.editor.cursor())
    }

    /// The buffer as it may be rendered; masked while a secret is being typed.
    #[must_use]
    pub fn display_buffer(&self) -> String {
        self.editor.display_buffer()
    }

    /// Select a startup source before accepting the first user message.
    pub fn resume_source(&mut self, session_id: &str) -> Result<(), String> {
        self.service.resume(Some(session_id.to_owned()))
    }

    /// The snapshot the TUI viewport draws.
    #[must_use]
    pub fn ui_state(&self) -> UiState {
        UiState {
            phase: self.phase,
            setup_required: self.setup_required,
            setup_hint: self.setup_hint.clone(),
            header: {
                // prime-agent's top bar names the chat: its name, else the
                // workspace directory.
                let mut header = self.header.clone();
                let name = self.chat_title.clone().unwrap_or_else(|| {
                    self.context
                        .project
                        .root
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default()
                });
                header.push(format!("Chat: {name}"));
                header
            },
            buffer: self.display_buffer(),
            cursor: self.editor.cursor(),
            live_text: self.pending_text.clone(),
            // The live block only ever draws the compact card.
            open_tools: self
                .open_tools
                .iter()
                .map(|(_, name, summary, _)| (name.clone(), summary.clone()))
                .collect(),
            live_tool_output: self
                .tool_progress
                .last()
                .map(|(_, text)| last_lines(text, LIVE_TOOL_LINES))
                .unwrap_or_default(),
            modal: self.modal(),
            granted_for_run: self.granted_for_run,
            queued_input: !self.queue.is_empty(),
            queued_count: self.queue.len(),
            queued_previews: self.queue.previews(),
            writing_code: self.writing_code,
            provider_wait: self.provider_wait.clone(),
            last_request: self.last_request.clone(),
            run_started_at: self.run_started_at,
            last_run_elapsed: self.last_run_elapsed,
            steps: self.steps,
            max_steps: self.bounds.max_steps,
            tool_calls: self.tool_calls,
            max_tool_calls: self.bounds.max_tool_calls,
            suggestions: self.editor.suggestions().to_vec(),
            suggestion_selected: self.editor.suggestion_selected(),
            fallback_reason: self.fallback_reason.clone(),
            tick: self.tick,
            detail: self.detail,
            thinking: self.thinking_label.clone(),
            service_tier: self.tier_label.clone(),
            goal: self.goal.as_ref().and_then(GoalState::tray_label),
        }
    }

    /// The modal panel for the current state, if any.
    fn modal(&self) -> Option<Modal> {
        if let Some(pending) = &self.pending_approval {
            return Some(Modal::Approval {
                request_id: pending.request_id.clone(),
                action: pending.action.clone(),
                summary: pending.summary.clone(),
                workspace: pending.workspace.clone(),
                scope: pending.scope.clone(),
                expires_at: pending.expires_at,
                read_only: pending.read_only,
                scroll: pending.scroll,
            });
        }
        if let Some(request) = &self.pending_mcp_elicitation {
            return Some(Modal::McpElicitation {
                message: format!("{} — {}", request.server, request.message),
                requested_schema: request.requested_schema.clone(),
            });
        }
        if let Some(question) = &self.pending_question {
            return Some(Modal::Question {
                prompt: question.prompt.clone(),
                options: question.options.clone(),
                selected: question.selected,
            });
        }
        if let Some(picker) = self.editor.picker() {
            if let Some(purpose) = self.turn_picker {
                return Some(Modal::TurnPicker {
                    items: picker.items().to_vec(),
                    selected: picker.selected(),
                    fork: purpose == super::events::TurnsPurpose::Fork,
                });
            }
            if self.file_picker_active {
                return Some(Modal::FilePicker {
                    items: picker.items().to_vec(),
                    selected: picker.selected(),
                });
            }
            if self.plugin_picker.is_some() {
                return Some(Modal::ServicePicker {
                    items: picker.items().to_vec(),
                    selected: picker.selected(),
                });
            }
            return Some(Modal::Picker {
                items: picker.items().to_vec(),
                selected: picker.selected(),
            });
        }
        self.editor.overlay().map(|overlay| Modal::Overlay {
            title: overlay.title.clone(),
            lines: overlay.lines.clone(),
            rich: overlay.rich.clone(),
            scroll: overlay.scroll(),
        })
    }

    /// Whether the slash-command menu is on screen right now.
    ///
    /// This is the same predicate the layout uses to reserve rows for it, and it
    /// has to be: a key must never act on a list the user cannot see. The plain
    /// renderer draws no menu, and a panel or picker that owns the keyboard
    /// replaces it, so neither may consume an arrow or an Enter for it.
    fn suggestion_menu_open(&self) -> bool {
        !self.plain
            && self.pending_approval.is_none()
            && self.editor.picker().is_none()
            && self.editor.overlay().is_none()
            && !self.editor.suggestions().is_empty()
    }

    /// Everything the host prints once, before the first prompt.
    ///
    /// Rendering the boot header is the transition out of the booting phase, so
    /// the prompt that follows already shows the state the app is really in.
    pub fn boot_lines(&mut self) -> Vec<String> {
        self.phase = if self.setup_required {
            AppPhase::SetupRequired
        } else {
            AppPhase::Ready
        };
        self.refresh_menu();
        let mut lines = self.banner_lines();
        lines.push(
            "Nhập yêu cầu. / lệnh · Alt+V dán ảnh/file · /login đăng nhập · /quit thoát".to_owned(),
        );
        // Codex's launch notice: a newer release is out.
        if !self.plain
            && super::update_check::enabled(
                &super::paths::LaunchEnvironment::capture(),
                &self.context.paths.config_file,
            )
            && let Some(notice) = super::update_check::notice(&self.context.paths.data_dir)
        {
            lines.push(notice);
        }
        lines
    }

    /// The lines the opening banner is drawn from: the header, the thinking level in
    /// force and any setup hint. They are read again when the model or the level
    /// changes, so the banner never names a model that is no longer the one in use.
    fn banner_lines(&self) -> Vec<String> {
        let mut lines = vec![String::new()];
        lines.extend(self.header.iter().cloned());
        if !self.plain
            && let Some(level) = &self.thinking_label
        {
            lines.push(format!("Thinking: {level}"));
        }
        if let Some(hint) = &self.setup_hint {
            lines.push(hint.clone());
        }
        lines
    }

    /// Ask the renderer to draw the banner again after the model or the level changed.
    fn refresh_banner(&self, effects: &mut Vec<Effect>) {
        if !self.plain {
            effects.push(Effect::Banner(self.banner_lines()));
        }
    }

    /// What the slash menu offers beyond the built-ins: the skills and prompt
    /// commands on disk now. Skills can appear mid-session (`/refine` writes them),
    /// so this runs at boot, after `/reload` and after every turn.
    fn refresh_menu(&mut self) {
        self.refresh_status();
        if self.plain {
            return;
        }
        self.editor.set_menu_commands(self.service.menu_commands());
        let stored = self.service.stored_credentials();
        self.editor.set_argument_options(
            "/login",
            super::providers::PROVIDERS
                .iter()
                .map(|entry| {
                    let state = if stored.iter().any(|(id, _)| id == entry.id) {
                        " · logged in"
                    } else {
                        ""
                    };
                    (entry.id.to_owned(), format!("{}{state}", entry.name))
                })
                .chain([(
                    super::web::SERPER_CREDENTIAL.to_owned(),
                    format!(
                        "Serper (web search){}",
                        if stored
                            .iter()
                            .any(|(id, _)| id == super::web::SERPER_CREDENTIAL)
                        {
                            " · logged in"
                        } else {
                            ""
                        }
                    ),
                )])
                .collect(),
        );
        self.editor.set_argument_options(
            "/logout",
            stored
                .iter()
                .map(|(id, kind)| {
                    let name =
                        super::providers::provider(id).map_or(id.as_str(), |entry| entry.name);
                    (id.clone(), format!("{name} · {kind}"))
                })
                .collect(),
        );
        let models = self.service.model_options();
        self.refresh_subagent_menus();
        self.editor.set_argument_options("/model", models);
        // The modes with what each lets through, the one in force marked.
        let current = self
            .service
            .permissions_summary()
            .first()
            .and_then(|line| line.strip_prefix("mode: ").map(str::to_owned));
        self.header
            .retain(|line| !line.starts_with("Permissions: "));
        if let Some(mode) = &current {
            self.header.push(format!("Permissions: {mode}"));
        }
        self.editor.set_argument_options(
            "/permissions",
            PERMISSION_MODES
                .iter()
                .map(|(mode, meaning)| {
                    let marker = if current.as_deref() == Some(*mode) {
                        " · current"
                    } else {
                        ""
                    };
                    ((*mode).to_owned(), format!("{meaning}{marker}"))
                })
                .collect(),
        );
    }

    /// Apply one key.
    #[allow(
        clippy::too_many_lines,
        reason = "this is the single ordered keyboard state machine for modal, running, and composer input"
    )]
    pub fn handle_key(&mut self, key: Key) -> Vec<Effect> {
        if key == Key::Redraw {
            return vec![Effect::Redraw];
        }
        // prime-agent's `app.editor.external`: the prompt's draft, while no
        // approval or question holds the keyboard.
        if self.pending_approval.is_none() && self.pending_question.is_none() {
            match key {
                Key::ExternalEditor => {
                    return vec![Effect::EditExternally(self.editor.expanded_text())];
                }
                Key::SetDraft(text) => {
                    self.editor.set_text(&text);
                    return vec![Effect::Redraw];
                }
                _ => {}
            }
        }
        if key == Key::Stash && self.pending_approval.is_none() && self.pending_question.is_none() {
            return self.stash_prompt();
        }
        if let Key::CycleModel { forward } = key
            && self.pending_approval.is_none()
            && self.pending_question.is_none()
        {
            let mut effects = Vec::new();
            self.cycle_model(forward, &mut effects);
            return effects;
        }
        if key == Key::CycleDetail {
            self.detail = self.detail.next();
            if self.plain {
                return vec![Effect::Redraw];
            }
            return vec![Effect::Reprint(self.detail), Effect::Redraw];
        }
        // A modal owns the keyboard while it is open: Escape closes it, and the
        // picker takes the arrows, Enter and Escape.
        if self.pending_approval.is_some() {
            match key {
                Key::EndOfInput => return self.command("/exit"),
                Key::Esc => {
                    if let Some(pending) = &mut self.pending_approval
                        && pending.always_allow_confirm
                    {
                        pending.always_allow_confirm = false;
                        pending.summary = without_rule_confirmation(&pending.summary);
                        return vec![Effect::Redraw];
                    }
                    return Vec::new();
                }
                Key::Enter
                    if self
                        .pending_approval
                        .as_ref()
                        .is_some_and(|p| p.always_allow_confirm) =>
                {
                    return self.confirm_always_allow();
                }
                Key::PageUp => {
                    if let Some(pending) = &mut self.pending_approval {
                        pending.scroll = pending.scroll.saturating_sub(8);
                    }
                    return vec![Effect::Redraw];
                }
                Key::PageDown => {
                    if let Some(pending) = &mut self.pending_approval {
                        pending.scroll = pending.scroll.saturating_add(8);
                    }
                    return vec![Effect::Redraw];
                }
                Key::Home => {
                    if let Some(pending) = &mut self.pending_approval {
                        pending.scroll = 0;
                    }
                    return vec![Effect::Redraw];
                }
                Key::End => {
                    if let Some(pending) = &mut self.pending_approval {
                        pending.scroll = usize::MAX;
                    }
                    return vec![Effect::Redraw];
                }
                // In the TUI the panel says `y chạy · a cho phép cả lượt · n từ
                // chối`, so a single y, a or n answers immediately; anything else is
                // typed and answered with Enter, which is what plain mode has always
                // done. `a` is offered on every panel, because the grant it gives
                // covers every kind - including the command in front of the user.
                Key::Char('y' | 'Y') if !self.plain => return self.answer("y"),
                Key::Char('A') if !self.plain => return self.propose_always_allow(),
                Key::Char('a') if !self.plain => return self.answer("a"),
                Key::Char('n' | 'N') if !self.plain => return self.answer("n"),
                Key::Char(character) => return self.handle_key(Key::Paste(character.to_string())),
                _ => {}
            }
        } else if self.pending_question.is_some() && self.phase == AppPhase::WaitingInput {
            // The options are a menu while the composer is empty: the arrows move
            // the highlight, Enter or an option's number answers. Once the user
            // types, the composer is theirs - a digit is text, Enter sends it.
            let choosing = self.editor.is_empty();
            let count = self
                .pending_question
                .as_ref()
                .map_or(0, |question| question.options.len());
            match key {
                Key::Esc => return Vec::new(),
                Key::Up | Key::Down if choosing && count > 0 => {
                    if let Some(question) = self.pending_question.as_mut() {
                        question.selected = if key == Key::Up {
                            question.selected.checked_sub(1).unwrap_or(count - 1)
                        } else {
                            (question.selected + 1) % count
                        };
                    }
                    return vec![Effect::Redraw];
                }
                Key::Enter if choosing && count > 0 => {
                    if let Some(option) = self
                        .pending_question
                        .as_ref()
                        .and_then(|question| question.options.get(question.selected))
                    {
                        return self.submit_question_answer(option.clone());
                    }
                }
                Key::Char(character @ '1'..='9') if choosing => {
                    let index = usize::from(character as u8 - b'1');
                    if let Some(option) = self
                        .pending_question
                        .as_ref()
                        .and_then(|question| question.options.get(index))
                    {
                        return self.submit_question_answer(option.clone());
                    }
                }
                Key::EndOfInput => return self.command("/exit"),
                _ => {}
            }
        } else if self.editor.overlay().is_some() {
            // An open panel owns the scroll keys, arrows included: every panel
            // (`/config`, `/hotkeys`, `/help`, `/mcp`, ...) is read by moving
            // through it, as prime-agent's and Claude Code's panels are, and the
            // composer is not being typed into while one is up. Escape still
            // closes it, which is what acceptance U09 asserts.
            let scrolled = match key {
                Key::Up => self.editor.scroll_overlay(-1),
                Key::Down => self.editor.scroll_overlay(1),
                Key::PageUp => self.editor.scroll_overlay(-8),
                Key::PageDown => self.editor.scroll_overlay(8),
                Key::Home => self.editor.scroll_overlay_to(false),
                Key::End => self.editor.scroll_overlay_to(true),
                _ => false,
            };
            if scrolled {
                return vec![Effect::Redraw];
            }
        } else if self.editor.picker().is_some() {
            match key {
                Key::Up => {
                    self.editor.move_picker(-1);
                    return vec![Effect::Redraw];
                }
                Key::Down => {
                    self.editor.move_picker(1);
                    return vec![Effect::Redraw];
                }
                Key::Esc => {
                    self.editor.close_picker();
                    self.file_picker_active = false;
                    self.file_picker_query.clear();
                    self.turn_picker = None;
                    self.plugin_picker = None;
                    return vec![Effect::Redraw];
                }
                Key::Enter => {
                    if let Some(ids) = self.plugin_picker.take() {
                        let chosen = self
                            .editor
                            .picker()
                            .and_then(|picker| ids.get(picker.selected()))
                            .cloned();
                        self.editor.close_picker();
                        let mut effects = Vec::new();
                        if let Some(id) = chosen {
                            self.connect_plugin(&id, &mut effects);
                        }
                        effects.push(Effect::Redraw);
                        return effects;
                    }
                    if let Some(purpose) = self.turn_picker.take() {
                        let chosen = self.editor.picker().map(|picker| picker.selected() + 1);
                        self.editor.close_picker();
                        let mut effects = Vec::new();
                        if let Some(number) = chosen {
                            let name = match purpose {
                                super::events::TurnsPurpose::Fork => "/fork",
                                super::events::TurnsPurpose::Tree => "/tree",
                            };
                            self.branch_command(name, Some(&number.to_string()), &mut effects);
                        }
                        effects.push(Effect::Redraw);
                        return effects;
                    }
                    if self.file_picker_active {
                        let chosen = self
                            .editor
                            .picker()
                            .and_then(|picker| picker.items().get(picker.selected()))
                            .cloned();
                        self.editor.close_picker();
                        self.file_picker_active = false;
                        self.file_picker_query.clear();
                        if let Some(path) = chosen {
                            let _ = self.editor.handle(Key::Backspace);
                            let _ = self.editor.handle(Key::Paste(quote_for_composer(&path)));
                        }
                        return vec![Effect::Redraw];
                    }
                    let chosen = self.selected_candidate();
                    self.editor.close_picker();
                    return match chosen {
                        Some(session_id) => self.continue_session(&session_id),
                        None => vec![Effect::Redraw],
                    };
                }
                Key::Char(character) if self.file_picker_active && !character.is_control() => {
                    self.file_picker_query.push(character);
                    self.refresh_file_picker();
                    return vec![Effect::Redraw];
                }
                Key::Backspace if self.file_picker_active => {
                    self.file_picker_query.pop();
                    self.refresh_file_picker();
                    return vec![Effect::Redraw];
                }
                Key::EndOfInput => return self.command("/exit"),
                _ => {}
            }
        }
        // The suggestion menu does not own the keyboard - the composer keeps the
        // focus and the draft stays visible - but while it is drawn these keys act
        // on it, and only then. Tab completes the highlighted row. Enter completes it
        // too, and when the row is whole - `/copy`, or `/effort high` picked from
        // the argument menu - runs it at once; a command that takes an argument
        // gains a space instead and opens its argument menu, as in prime-agent.
        if self.suggestion_menu_open() {
            match key {
                Key::Up => {
                    self.editor.move_suggestion(-1);
                    return vec![Effect::Redraw];
                }
                Key::Down => {
                    self.editor.move_suggestion(1);
                    return vec![Effect::Redraw];
                }
                Key::Tab => {
                    self.editor.accept_suggestion();
                    return vec![Effect::Redraw];
                }
                Key::Enter => {
                    let embedded = self.editor.suggestion_is_embedded();
                    self.editor.accept_suggestion();
                    if embedded
                        || self.editor.display_buffer().ends_with(' ')
                        || !self.editor.suggestions().is_empty()
                    {
                        return vec![Effect::Redraw];
                    }
                }
                _ => {}
            }
        }
        // Escape closes what is open first - a reference panel (`/agents`, `/help`)
        // or the slash menu - and only then interrupts the run, as in prime-agent:
        // closing a panel must never cancel the work it was showing.
        if key == Key::Esc && (self.editor.overlay().is_some() || self.suggestion_menu_open()) {
            let _ = self.editor.handle(Key::Esc);
            return vec![Effect::Redraw];
        }
        if key == Key::Esc && self.phase == AppPhase::Running {
            return self.interrupt();
        }
        // prime-agent's double-Escape on an idle prompt: a second press within
        // half a second clears the draft, or opens the session tree when the
        // prompt is empty.
        // Only where Escape has nothing else to do: a masked key entry or an open
        // menu keeps its own Escape, and the first press still does what Escape
        // does.
        if key == Key::Esc
            && !self.phase.has_active_run()
            && self.signing_in.is_none()
            && !self.editor.secret_entry()
            && self.editor.suggestions().is_empty()
        {
            if self
                .escape_armed_at
                .take()
                .is_some_and(|armed| armed.elapsed() < Duration::from_millis(500))
            {
                if self.editor.is_empty() {
                    return self.command("/tree");
                }
                self.editor.set_text("");
                return vec![Effect::Redraw];
            }
            self.escape_armed_at = Some(Instant::now());
        }
        // prime-agent's agents-back: Left on an empty prompt hands this
        // terminal to the agents view; the agent keeps running.
        if key == Key::Left
            && self.detachable
            && self.editor.is_empty()
            && self.editor.suggestions().is_empty()
            && self.editor.picker().is_none()
            && self.editor.overlay().is_none()
            && self.pending_approval.is_none()
            && self.pending_question.is_none()
        {
            return vec![Effect::Exit(super::agents_view::AGENTS_VIEW_EXIT)];
        }
        if key == Key::Esc && self.signing_in.is_some() && self.editor.is_empty() {
            self.signing_in = None;
            self.service.cancel_sign_in();
            return vec![
                Effect::History(HistoryItem::Notice {
                    message: "sign-in canceled".to_owned(),
                }),
                Effect::Redraw,
            ];
        }
        // A pasted screenshot: the key is handled here rather than by the editor
        // because there is no text to insert until the clipboard has been read.
        // A paste with no text is what a terminal sends when the clipboard holds an
        // image or copied files: read the clipboard for them instead.
        let empty_paste = matches!(&key, Key::Paste(text) if text.trim().is_empty());
        if key == Key::PasteImage || (empty_paste && !self.editor.secret_entry()) {
            let mut effects = Vec::new();
            self.paste_image(&mut effects);
            return effects;
        }
        if key == Key::Char('@')
            && !self.plain
            && matches!(self.editor.handle(Key::Char('@')), InputOutcome::Redraw)
        {
            self.open_file_picker();
            return vec![Effect::Redraw];
        }
        match self.editor.handle(key) {
            InputOutcome::Unchanged => Vec::new(),
            InputOutcome::Redraw | InputOutcome::CompleteSuggestion => vec![Effect::Redraw],
            InputOutcome::Exit => self.command("/exit"),
            InputOutcome::Interrupt => self.interrupt(),
            InputOutcome::Submit(text) => self.submit(text),
            InputOutcome::Secret(value) => self.save_key(&value),
            InputOutcome::SecretCanceled => self.cancel_key(),
        }
    }

    /// Move everything the session port produced into effects.
    pub fn pump_events(&mut self) -> Vec<Effect> {
        let mut effects = self.deliver_heartbeats();
        if let Some(level) = self.initial_thinking.take()
            && let Err(message) = self.service.set_thinking(&level)
        {
            self.push_history(&mut effects, HistoryItem::Notice { message });
        }
        if !self.phase.has_active_run()
            && let Some(objective) = self.initial_goal.take()
        {
            if self.initial_prompt.is_some() {
                self.goal = Some(GoalState::new(objective.clone()));
                self.service.set_goal(Some(objective));
            } else {
                self.goal_command(Some(&objective), &mut effects);
            }
        }
        if !self.phase.has_active_run()
            && let Some(prompt) = self.initial_prompt.take()
        {
            effects.extend(self.submit(prompt));
        }
        let events = self.channel.drain();
        if events.is_empty() {
            if !effects.is_empty() {
                effects.push(Effect::Redraw);
            }
            return effects;
        }
        for event in events {
            self.apply_event(event, &mut effects);
        }
        // Plain output is an append-only transcript, so it consumes every delta.
        // The TUI keeps the newest rows in its live block and only moves overflow
        // into scrollback. Boundary events still call `flush_stream` explicitly
        // to preserve assistant/tool/run ordering.
        if self.plain {
            self.flush_stream(&mut effects);
        } else {
            self.flush_stream_overflow(&mut effects);
        }
        effects.push(Effect::Redraw);
        effects
    }

    /// Send the agent's heartbeats that came due (prime-agent's RLM heartbeats): a
    /// free session runs one at once; a running turn gets a `steer` heartbeat through
    /// the `/steer` inbox, and a `follow_up` one waits for the turn to end.
    /// `/fork [n]` and `/tree [n]`: list the conversation's turns, or act on one.
    /// `/tree`: prime-agent's tree, driven by arguments rather than a selector:
    /// `/tree` lists the turns (labels shown as `[label] `), `/tree <n>` continues
    /// after turn n, `/tree <n> --summarize [focus]` also summarises the turns it
    /// leaves (prime-agent's "Summarize" / "Summarize with custom prompt"), and
    /// `/tree label <n> [text]` labels a turn (an empty text clears it).
    fn tree_command(&mut self, raw_argument: Option<&str>, effects: &mut Vec<Effect>) {
        let words = raw_argument
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>();
        if let ["label", number, label @ ..] = words.as_slice() {
            let chosen = number
                .parse::<usize>()
                .ok()
                .and_then(|number| number.checked_sub(1))
                .and_then(|index| self.turn_points.get(index).cloned());
            let result = match chosen {
                Some((session, _)) => self.service.label_turn(&session, &label.join(" ")),
                None => Err(format!("no turn {number}; list them with /tree")),
            };
            if let Err(message) = result {
                self.push_history(effects, HistoryItem::Error { message });
            }
            effects.push(Effect::Redraw);
            return;
        }
        if let [number, "--summarize", focus @ ..] = words.as_slice() {
            let chosen = number
                .parse::<usize>()
                .ok()
                .and_then(|number| number.checked_sub(1));
            let at_leaf = chosen.is_some_and(|index| index + 1 == self.turn_points.len());
            if chosen.is_some_and(|index| index < self.turn_points.len()) && !at_leaf {
                let focus = (!focus.is_empty()).then(|| focus.join(" "));
                if let Err(message) = self.service.plan_branch_summary(focus) {
                    self.push_history(effects, HistoryItem::Error { message });
                    effects.push(Effect::Redraw);
                    return;
                }
            }
            self.branch_command("/tree", Some(number), effects);
            return;
        }
        self.branch_command("/tree", words.first().copied(), effects);
    }

    /// prime-agent's `agent_messages_status|pause|resume|clear` for this agent.
    fn agent_messages_command(&mut self, action: &str, effects: &mut Vec<Effect>) {
        let status = |paused: bool| {
            format!(
                "agent messaging {} · at most 16,384 characters a message · 20 pending · 3 per sender per second",
                if paused { "paused" } else { "active" }
            )
        };
        let message = match action {
            "status" => status(self.agent_messages_paused),
            "pause" => {
                self.agent_messages_paused = true;
                let cleared = self.clear_agent_messages();
                format!("{} · {cleared} queued message(s) dropped", status(true))
            }
            "resume" => {
                self.agent_messages_paused = false;
                status(false)
            }
            "clear" => format!(
                "{} queued agent message(s) dropped",
                self.clear_agent_messages()
            ),
            other => {
                self.push_history(
                    effects,
                    HistoryItem::Error {
                        message: format!(
                            "unknown /agents messages action {other:?}: status, pause, resume or clear"
                        ),
                    },
                );
                effects.push(Effect::Redraw);
                return;
            }
        };
        self.push_history(effects, HistoryItem::Notice { message });
        effects.push(Effect::Redraw);
    }

    /// prime-agent's `clearQueuedAgentMessages`: only messages other agents
    /// sent, never the user's own queue.
    fn clear_agent_messages(&mut self) -> usize {
        let sent = std::mem::take(&mut self.agent_messages_queued);
        self.queue
            .remove_where(|text| sent.iter().any(|queued| queued == text))
            .len()
    }

    /// prime-agent's `/plugins [search]`: the service catalog, connected
    /// first; Enter connects the highlighted service. `/plugins connect <id>`
    /// connects one directly (the plain renderer's way).
    fn plugins_command(&mut self, argument: Option<&str>, effects: &mut Vec<Effect>) {
        let argument = argument.map(str::trim).unwrap_or_default();
        if let Some(id) = argument.strip_prefix("connect ") {
            self.connect_plugin(id.trim(), effects);
            effects.push(Effect::Redraw);
            return;
        }
        let cards = self.service.plugin_cards();
        let mut cards = if argument.is_empty() {
            cards
        } else {
            super::mcp_catalog::search(&cards, argument, cards.len())
        };
        if cards.is_empty() {
            self.push_history(
                effects,
                HistoryItem::Notice {
                    message: format!("No services match \"{argument}\""),
                },
            );
            effects.push(Effect::Redraw);
            return;
        }
        let rank = |card: &serde_json::Value| match card["connectionStatus"].as_str() {
            Some("connected") => 0,
            Some("pending") => 1,
            Some("not_connected") => 2,
            _ => 3,
        };
        cards.sort_by_key(rank);
        let rows: Vec<String> = cards
            .iter()
            .map(|card| {
                let status = card["connectionStatus"]
                    .as_str()
                    .unwrap_or_default()
                    .replace('_', " ");
                let category = card["category"].as_str().unwrap_or_default();
                let description: String = card["description"]
                    .as_str()
                    .unwrap_or_default()
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .take(80)
                    .collect();
                format!(
                    "{} · {status}{} · {description}",
                    card["label"].as_str().unwrap_or_default(),
                    if category.is_empty() {
                        String::new()
                    } else {
                        format!(" · {category}")
                    }
                )
            })
            .collect();
        let ids: Vec<String> = cards
            .iter()
            .map(|card| card["serviceId"].as_str().unwrap_or_default().to_owned())
            .collect();
        if self.plain {
            let mut lines: Vec<String> = rows
                .iter()
                .zip(&ids)
                .map(|(row, id)| format!("{row}  [{id}]"))
                .collect();
            lines.push(String::new());
            lines.push("connect one with /plugins connect <id>".to_owned());
            self.reference("/plugins", lines, effects);
        } else {
            self.editor.open_picker(rows);
            self.plugin_picker = Some(ids);
        }
        effects.push(Effect::Redraw);
    }

    fn connect_plugin(&mut self, id: &str, effects: &mut Vec<Effect>) {
        match self.service.connect_plugin(id) {
            Ok(message) => self.push_history(effects, HistoryItem::Notice { message }),
            Err(message) => self.push_history(effects, HistoryItem::Error { message }),
        }
    }

    fn branch_command(&mut self, name: &str, argument: Option<&str>, effects: &mut Vec<Effect>) {
        let purpose = if name == "/fork" {
            super::events::TurnsPurpose::Fork
        } else {
            super::events::TurnsPurpose::Tree
        };
        let chosen = argument
            .and_then(|number| number.parse::<usize>().ok())
            .and_then(|number| number.checked_sub(1));
        let Some(index) = chosen else {
            if let Err(message) = self.service.list_turns(purpose) {
                self.push_history(effects, HistoryItem::Error { message });
            }
            effects.push(Effect::Redraw);
            return;
        };
        let Some((session, text)) = self.turn_points.get(index).cloned() else {
            self.push_history(
                effects,
                HistoryItem::Error {
                    message: format!("no turn {}; list them with {name}", index + 1),
                },
            );
            effects.push(Effect::Redraw);
            return;
        };
        let result = match purpose {
            // prime-agent's /fork: a new conversation up to just before the chosen
            // message, which goes back into the editor to be sent again or edited.
            super::events::TurnsPurpose::Fork => self.service.fork(&session, true).map(|()| {
                self.editor.set_text(&text);
                "Forked to new session".to_owned()
            }),
            super::events::TurnsPurpose::Tree if index + 1 == self.turn_points.len() => {
                Ok("Already at this point".to_owned())
            }
            super::events::TurnsPurpose::Tree => self.service.switch_to(&session).map(|()| {
                format!(
                    "continuing from turn {}; the next message follows it, and the later turns leave this conversation",
                    index + 1
                )
            }),
        };
        match result {
            Ok(message) => self.push_history(effects, HistoryItem::Notice { message }),
            Err(message) => self.push_history(effects, HistoryItem::Error { message }),
        }
        effects.push(Effect::Redraw);
    }

    fn show_turns(
        &mut self,
        purpose: super::events::TurnsPurpose,
        turns: Vec<(String, String)>,
        effects: &mut Vec<Effect>,
    ) {
        if turns.is_empty() {
            let message = match purpose {
                super::events::TurnsPurpose::Fork => "No messages to fork from",
                super::events::TurnsPurpose::Tree => "Nothing to show yet",
            };
            self.push_history(
                effects,
                HistoryItem::Notice {
                    message: message.to_owned(),
                },
            );
            effects.push(Effect::Redraw);
            return;
        }
        let last = turns.len();
        let mut lines: Vec<String> = turns
            .iter()
            .enumerate()
            .map(|(index, (_, text))| {
                let first = text.lines().next().unwrap_or_default();
                let clipped: String = first.chars().take(100).collect();
                let current = if index + 1 == last && purpose == super::events::TurnsPurpose::Tree {
                    "  ← current"
                } else {
                    ""
                };
                format!("{}. {clipped}{current}", index + 1)
            })
            .collect();
        lines.push(String::new());
        match purpose {
            super::events::TurnsPurpose::Fork => lines.push(
                "fork before a message with /fork <number>; it comes back into the editor"
                    .to_owned(),
            ),
            super::events::TurnsPurpose::Tree => lines.extend([
                "continue after a turn with /tree <number>".to_owned(),
                "  add --summarize [focus] to summarize the turns it leaves".to_owned(),
                "label a turn with /tree label <number> [text]".to_owned(),
            ]),
        }
        self.turn_points = turns;
        // prime-agent's tree and fork selectors: the messages in a menu, the
        // current one highlighted; Enter picks, Esc leaves. The plain
        // renderer keeps the numbered list.
        if !self.plain {
            let items = lines[..last].to_vec();
            self.editor.open_picker_at(items, last - 1);
            self.turn_picker = Some(purpose);
            effects.push(Effect::Redraw);
            return;
        }
        let title = match purpose {
            super::events::TurnsPurpose::Fork => "/fork",
            super::events::TurnsPurpose::Tree => "/tree",
        };
        self.reference(title, lines, effects);
        effects.push(Effect::Redraw);
    }

    /// `/model next|prev`, Alt+M: the next scoped model, applied as `/model` is.
    fn cycle_model(&mut self, forward: bool, effects: &mut Vec<Effect>) {
        match self.service.cycle_model(forward) {
            Ok(message) => self.push_history(
                effects,
                HistoryItem::Notice {
                    message: if self.phase.has_active_run() {
                        format!("{message}; the running turn switches at its next step")
                    } else {
                        message
                    },
                },
            ),
            Err(message) => self.push_history(effects, HistoryItem::Error { message }),
        }
        self.refresh_status();
        self.refresh_banner(effects);
    }

    /// prime-agent's prompt stash (Ctrl-S): a draft is put aside, and an empty
    /// prompt brings it back.
    fn stash_prompt(&mut self) -> Vec<Effect> {
        let draft = self.editor.text();
        let message = if draft.trim().is_empty() {
            match self.stash.take() {
                Some(stashed) => {
                    self.editor.set_text(&stashed);
                    "Restored stashed prompt"
                }
                None => "No prompt to stash",
            }
        } else if self.stash.is_some() {
            "Prompt stash already has a draft"
        } else {
            self.stash = Some(draft);
            self.editor.clear();
            "Stashed prompt"
        };
        vec![
            Effect::History(HistoryItem::Notice {
                message: message.to_owned(),
            }),
            Effect::Redraw,
        ]
    }

    /// `/queue`: list the queue, add a follow-up, or edit, drop and reorder it.
    fn queue_command(&mut self, argument: Option<&str>, effects: &mut Vec<Effect>) {
        let argument = argument.map(str::trim).unwrap_or_default();
        let (verb, rest) = argument.split_once(' ').unwrap_or((argument, ""));
        let index = |text: &str| {
            text.split_whitespace()
                .next()
                .and_then(|number| number.parse::<usize>().ok())
                .and_then(|number| number.checked_sub(1))
        };
        let message = match verb {
            "" | "list" => {
                let lines = self.queue.lines();
                if lines.is_empty() {
                    "the queue is empty".to_owned()
                } else {
                    self.reference("/queue", lines, effects);
                    return;
                }
            }
            "drop" | "up" | "down" | "edit" => {
                let Some(at) = index(rest) else {
                    self.push_history(
                        effects,
                        HistoryItem::Error {
                            message: format!("usage: /queue {verb} <number>"),
                        },
                    );
                    return;
                };
                let done = match verb {
                    "drop" => self.queue.remove(at).is_some(),
                    "up" => self.queue.move_up(at),
                    "down" => self.queue.move_down(at),
                    _ => {
                        let text = rest
                            .trim_start()
                            .split_once(' ')
                            .map_or("", |(_, text)| text)
                            .to_owned();
                        self.queue.edit(at, text)
                    }
                };
                if done {
                    format!("queue: {} ({} waiting)", verb, self.queue.len())
                } else {
                    format!("no queued message {}", at + 1)
                }
            }
            _ if self.phase.has_active_run() => {
                self.queue
                    .push(super::queue::Lane::FollowUp, argument.to_owned());
                format!(
                    "follow-up queued ({}): it runs after the active run finishes",
                    self.queue.len()
                )
            }
            // Nothing is running: a follow-up is simply the next message.
            _ => {
                effects.extend(self.dispatch(argument.to_owned(), false));
                return;
            }
        };
        self.push_history(effects, HistoryItem::Notice { message });
        effects.push(Effect::Redraw);
    }

    /// Whether the parent can start a turn of its own right now.
    fn free_for_automatic_turn(&self) -> bool {
        !self.phase.has_active_run()
            && self.pending_approval.is_none()
            && self.pending_question.is_none()
    }

    /// A child's terminal notice: a turn of its own when the parent is free,
    /// otherwise it waits for the running turn to end. It is never steered into
    /// the running turn, as prime-agent's notices ride its follow-up lane.
    fn deliver_notice(&mut self, name: &str, notice: String, effects: &mut Vec<Effect>) {
        if self.free_for_automatic_turn() && !self.notices_held && self.pending_notices.is_empty() {
            effects.extend(self.dispatch(notice, true));
        } else {
            self.pending_notices.push_back(notice);
            self.push_history(
                effects,
                HistoryItem::Notice {
                    message: format!(
                        "child {name} finished; its report is read when this turn ends"
                    ),
                },
            );
        }
    }

    /// A child's message to its parent, delivered as prime-agent delivers agent
    /// messages: into the running turn at its next step, or as a turn of its own.
    fn deliver_agent_message(&mut self, text: String, effects: &mut Vec<Effect>) {
        if self.phase.has_active_run() && self.service.deliver_message(&text).is_ok() {
            self.push_history(
                effects,
                HistoryItem::Notice {
                    message: "a child's message reached the running turn".to_owned(),
                },
            );
        } else if self.free_for_automatic_turn()
            && !self.notices_held
            && self.pending_notices.is_empty()
        {
            effects.extend(self.dispatch(text, true));
        } else {
            self.pending_notices.push_back(text);
        }
    }

    fn deliver_heartbeats(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        for due in self.service.due_heartbeats() {
            let free = !self.phase.has_active_run()
                && self.pending_approval.is_none()
                && self.pending_question.is_none();
            if free && self.pending_heartbeats.is_empty() {
                effects.extend(self.dispatch(due.text, true));
            } else if self.phase.has_active_run()
                && due.delivery == super::heartbeat::Delivery::Steer
                && self.service.steer(&due.text).is_ok()
            {
                effects.push(Effect::History(HistoryItem::Notice {
                    message: "heartbeat steered into the running turn".to_owned(),
                }));
            } else {
                self.pending_heartbeats.push_back(due.text);
            }
        }
        effects
    }

    /// Time-driven repaint: the spinner, the elapsed clock and the approval
    /// countdown.
    ///
    /// It never changes the phase: a tick that mutated state could expire an
    /// approval the gate is still waiting on. The gate owns expiry, and reports
    /// it as `ApprovalExpired`. An idle app returns no effect at all, which is
    /// how acceptance U06 proves the idle loop does not redraw.
    #[allow(dead_code, reason = "T05 wires the tick into the render loop")]
    pub fn tick(&mut self) -> Vec<Effect> {
        if self.phase.has_active_run() || self.pending_approval.is_some() {
            self.tick = self.tick.saturating_add(1);
            vec![Effect::Redraw]
        } else {
            Vec::new()
        }
    }

    #[allow(clippy::too_many_lines, reason = "one arm per session event")]
    fn apply_event(&mut self, event: SessionEvent, effects: &mut Vec<Effect>) {
        match event {
            SessionEvent::Accepted { input_id } => {
                self.flush_stream(effects);
                self.push_history(
                    effects,
                    HistoryItem::RunAccepted {
                        input_id: input_id.as_str().to_owned(),
                    },
                );
            }
            // An empty answer delta (a frame that carries only reasoning) is not
            // answer text: it must not commit the reasoning gathered so far.
            SessionEvent::TextDelta { text } if text.is_empty() => {}
            SessionEvent::ToolCallWriting { .. } => {
                self.writing_code = true;
                effects.push(Effect::Redraw);
            }
            SessionEvent::TextDelta { text } => {
                self.writing_code = false;
                self.flush_thinking(effects);
                self.pending_newlines = self
                    .pending_newlines
                    .saturating_add(text.bytes().filter(|byte| *byte == b'\n').count());
                self.pending_text.push_str(&text);
            }
            SessionEvent::StreamRestarted => {
                // The provider stream failed part-way and the runtime is asking
                // again: the retry repeats the answer from its start, so the
                // text the failed attempt streamed is dropped instead of being
                // shown twice. Lines already committed to the scrollback stay,
                // and the notice says the answer below replaces them.
                let committed = !self.last_answer.is_empty();
                self.pending_text.clear();
                self.pending_newlines = 0;
                self.pending_thinking.clear();
                self.push_history(
                    effects,
                    HistoryItem::Notice {
                        message: if committed {
                            "the provider stream failed part-way; retrying - the answer above was cut off and is written again below".to_owned()
                        } else {
                            "the provider stream failed part-way; retrying".to_owned()
                        },
                    },
                );
                effects.push(Effect::Redraw);
            }
            SessionEvent::ThinkingDelta { text } => {
                self.writing_code = false;
                // Reasoning is one row, not one row per delta: it is gathered and
                // committed when answer text arrives or the step ends. Providers
                // can interleave reasoning with answer tokens. Keep the TUI's
                // unfinished answer live so this cannot split a word into two
                // permanent history blocks. Plain output preserves event order.
                if self.plain {
                    self.flush_text(effects);
                }
                self.pending_thinking.push_str(&text);
                effects.push(Effect::Redraw);
            }
            SessionEvent::UsageUpdated { label, tokens } => {
                // prime-agent's goal accounting: every call of a goal's turns
                // spends its budget, and the crossing steers the model to wrap
                // up instead of starting new work.
                if let Some(goal) = &mut self.goal
                    && goal.spend(tokens)
                {
                    let steer = super::goal::budget_limit_text(goal);
                    let _ = self.service.steer(&steer);
                    self.pending_notices.push_back(format!(
                        "goal budget spent ({} of {} tokens); the model is asked to wrap up",
                        goal.tokens_used,
                        goal.token_budget.unwrap_or_default()
                    ));
                }
                let line = format!("Context: {label}");
                if let Some(existing) = self
                    .header
                    .iter_mut()
                    .find(|entry| entry.starts_with("Context:"))
                {
                    *existing = line;
                } else {
                    self.header.push(line);
                }
                effects.push(Effect::Redraw);
            }
            SessionEvent::CostUpdated { label } => {
                let line = format!("Cost: {label}");
                if let Some(existing) = self
                    .header
                    .iter_mut()
                    .find(|entry| entry.starts_with("Cost:"))
                {
                    *existing = line;
                } else {
                    self.header.push(line);
                }
                effects.push(Effect::Redraw);
            }
            SessionEvent::StepStarted { step } => {
                self.steps = step;
            }
            SessionEvent::ToolStarted {
                name,
                call_id,
                summary,
                input,
            } => {
                self.writing_code = false;
                self.flush_stream(effects);
                self.tool_calls = self.tool_calls.saturating_add(1);
                self.open_tools
                    .push((call_id, name.clone(), summary.clone(), input.clone()));
                if self.plain {
                    self.push_history(
                        effects,
                        HistoryItem::Tool {
                            name,
                            summary,
                            input,
                            state: ToolState::Started,
                        },
                    );
                }
            }
            SessionEvent::ToolProgress { call_id, text } => {
                // Newest call last: its lines are the ones shown.
                let mut buffer = self
                    .tool_progress
                    .iter()
                    .position(|(id, _)| id == &call_id)
                    .map(|index| self.tool_progress.remove(index).1)
                    .unwrap_or_default();
                buffer.push_str(&text);
                if buffer.len() > TOOL_PROGRESS_BYTES {
                    let mut cut = buffer.len() - TOOL_PROGRESS_BYTES;
                    while !buffer.is_char_boundary(cut) {
                        cut += 1;
                    }
                    buffer.drain(..cut);
                }
                self.tool_progress.push((call_id, buffer));
                effects.push(Effect::Redraw);
            }
            SessionEvent::ToolOutput { text } => {
                if !self.plain {
                    self.pending_tool_output = Some(text);
                }
            }
            SessionEvent::ToolSettled {
                name,
                call_id,
                ok,
                elapsed,
                detail,
            } => {
                self.flush_stream(effects);
                self.tool_progress.retain(|(id, _)| id != &call_id);
                // Found by the call's id: calls of a parallel batch settle in
                // completion order. A host action has none; its name finds it.
                let (summary, input) = self
                    .open_tools
                    .iter()
                    .position(|(id, open_name, _, _)| {
                        if call_id.is_empty() {
                            open_name == &name
                        } else {
                            id == &call_id
                        }
                    })
                    .map_or_else(
                        || (String::new(), String::new()),
                        |index| {
                            let (_, _, summary, input) = self.open_tools.remove(index);
                            (summary, input)
                        },
                    );
                let state = if ok {
                    ToolState::Ok { elapsed }
                } else {
                    ToolState::Failed { elapsed, detail }
                };
                let summary = if self.plain {
                    String::new()
                } else {
                    match self.pending_allowance.take() {
                        Some(reason) if summary.is_empty() => reason,
                        Some(reason) => format!("{summary} · {reason}"),
                        None => summary,
                    }
                };
                let path = super::events::path_argument(&input);
                self.push_history(
                    effects,
                    HistoryItem::Tool {
                        name: name.clone(),
                        // In the TUI this is the same card moving from the live
                        // region into scrollback. Plain mode already printed the
                        // summary on the Started row, so its settled row remains
                        // byte-identical to H03.
                        summary,
                        input,
                        state,
                    },
                );
                if let Some(text) = self
                    .pending_tool_output
                    .take()
                    .filter(|text| !text.trim().is_empty())
                {
                    self.push_history(effects, HistoryItem::ToolOutput { name, text, path });
                }
            }
            SessionEvent::ApprovalRequired {
                request_id,
                action,
                summary,
                rule_pattern,
                workspace,
                scope,
                expires_at,
                read_only,
            } => {
                self.flush_stream(effects);
                // The panel owns the proposal while it is open, so writing it to the
                // scrollback as well would print the same block twice - once above the
                // viewport and once in the viewport. Plain mode has no panel, so there
                // the transcript keeps the block: `history` is what a reader of the
                // plain transcript has instead of a panel.
                if self.plain {
                    self.push_history(
                        effects,
                        HistoryItem::Approval {
                            action: action.clone(),
                            summary: summary.clone(),
                            workspace: workspace.clone(),
                            scope: scope.clone(),
                            request_id: request_id.clone(),
                        },
                    );
                }
                let approval = PendingApproval {
                    request_id,
                    action,
                    summary,
                    rule_pattern,
                    always_allow_confirm: false,
                    workspace,
                    scope,
                    expires_at,
                    read_only,
                    scroll: 0,
                };
                // One panel at a time: a request that arrives while another
                // is open waits its turn instead of replacing it.
                if self.pending_approval.is_some() || self.pending_question.is_some() {
                    self.approval_queue.push_back(approval);
                } else {
                    self.pending_approval = Some(approval);
                    self.phase = AppPhase::WaitingApproval;
                }
            }
            SessionEvent::ApprovalExpired { request_id } => {
                self.flush_stream(effects);
                let matches_pending = self
                    .pending_approval
                    .as_ref()
                    .is_some_and(|pending| pending.request_id == request_id);
                if matches_pending {
                    self.pending_approval = None;
                    if self.phase == AppPhase::WaitingApproval {
                        self.phase = AppPhase::Running;
                    }
                    self.next_waiting();
                } else {
                    self.approval_queue
                        .retain(|queued| queued.request_id != request_id);
                }
                self.push_history(
                    effects,
                    HistoryItem::ApprovalResolution {
                        label: "expired".to_owned(),
                        request_id,
                    },
                );
            }
            SessionEvent::SessionsListed { sessions } => {
                self.flush_stream(effects);
                if sessions.is_empty() {
                    self.push_history(
                        effects,
                        HistoryItem::Notice {
                            message: "no persisted sessions in this project yet".to_owned(),
                        },
                    );
                } else {
                    let mut lines = vec![format!("sessions in this project ({}):", sessions.len())];
                    for (index, candidate) in sessions.iter().enumerate() {
                        lines.push(format!(
                            "  {}. {}  {}  {}",
                            index + 1,
                            candidate.session_id,
                            candidate.task_id,
                            candidate.detail
                        ));
                    }
                    if self.plain {
                        lines.push("use /resume <number> to continue one of them".to_owned());
                    }
                    let items: Vec<String> = sessions
                        .iter()
                        .map(|candidate| {
                            format!(
                                "{}  {}  {}",
                                candidate.session_id, candidate.task_id, candidate.detail
                            )
                        })
                        .collect();
                    self.push_history(effects, HistoryItem::Sessions { lines });
                    // The TUI opens a picker instead of asking for a number.
                    if !self.plain {
                        self.editor.open_picker(items);
                    }
                }
                self.session_candidates = sessions;
            }
            SessionEvent::ConversationRestored {
                turns,
                omitted,
                summarized,
            } => {
                self.flush_stream(effects);
                // A goal belongs to its conversation; the resumed one sends its own.
                self.goal = None;
                self.service.set_goal(None);
                if summarized {
                    self.push_history(
                        effects,
                        HistoryItem::Notice {
                            message: "earlier turns were compacted; the model continues from their summary".to_owned(),
                        },
                    );
                }
                if omitted > 0 {
                    self.push_history(
                        effects,
                        HistoryItem::Notice {
                            message: format!(
                                "{omitted} older turn(s) are not shown and are not sent to the model"
                            ),
                        },
                    );
                }
                let count = turns.len();
                for (question, answer) in turns {
                    self.push_history(effects, HistoryItem::User { text: question });
                    self.push_history(effects, HistoryItem::Assistant { text: answer });
                }
                self.push_history(
                    effects,
                    HistoryItem::Notice {
                        message: format!(
                            "resumed {count} turn(s); the next message continues this conversation"
                        ),
                    },
                );
            }
            SessionEvent::Reference { title, lines } => {
                self.flush_stream(effects);
                self.reference(&title, lines, effects);
            }
            SessionEvent::LoginFinished { provider, result } => {
                self.flush_stream(effects);
                self.signing_in = None;
                let name = super::providers::provider(&provider)
                    .map_or(provider.as_str(), |entry| entry.name)
                    .to_owned();
                let for_subagents = self.login_scope == super::credentials::Scope::Subagent;
                self.credential_scope(super::credentials::Scope::Main);
                match result {
                    Ok(()) if for_subagents => {
                        self.push_history(
                            effects,
                            HistoryItem::Notice {
                                message: format!(
                                    "Logged in to {name} for subagents; the main model's login is unchanged."
                                ),
                            },
                        );
                        self.refresh_menu();
                    }
                    Ok(()) => {
                        let path = super::credentials::resolve_file(
                            &super::paths::LaunchEnvironment::capture(),
                            &self.context.paths.data_dir,
                        );
                        let _ = self.activate_credential(CredentialSource::File {
                            path,
                            protection: super::credentials::Protection::NotReverified,
                        });
                        self.push_history(
                            effects,
                            HistoryItem::Notice {
                                message: format!("Logged in to {name}."),
                            },
                        );
                        self.after_login(&provider, effects);
                    }
                    Err(message) => self.push_history(
                        effects,
                        HistoryItem::Error {
                            message: format!("{name} sign-in failed: {message}"),
                        },
                    ),
                }
            }
            SessionEvent::Notice { message } => {
                // Child progress can arrive between two tokens of the parent's
                // answer. Keep the TUI's live text together so markdown spans
                // across those tokens render as one assistant message.
                if self.plain || !message.starts_with("[child ") {
                    self.flush_stream(effects);
                }
                // An action that ran without a panel is announced before it runs. In
                // the TUI that was one `[info] allowed by ...` row above every tool
                // card - half the transcript of a read-heavy turn. The reason now rides
                // on the card it belongs to; the plain transcript keeps the row, since
                // it is a log.
                if !self.plain && message.starts_with("allowed by ") {
                    let reason = message
                        .split_once(':')
                        .map_or(message.as_str(), |(reason, _)| reason)
                        .trim()
                        .to_owned();
                    self.pending_allowance = Some(reason);
                } else {
                    self.push_history(effects, HistoryItem::Notice { message });
                }
            }
            SessionEvent::Bell => effects.push(Effect::Bell),
            SessionEvent::ShellPrefixCompleted {
                command,
                output,
                attach_to_next_message,
            } => {
                self.flush_stream(effects);
                if attach_to_next_message {
                    self.push_shell_output(&command, &output);
                }
                self.push_history(
                    effects,
                    HistoryItem::Message {
                        text: format!("[shell] {command}\n{output}"),
                    },
                );
            }
            SessionEvent::QuestionRequired {
                question_id,
                prompt,
                options,
            } => {
                self.flush_stream(effects);
                if self.plain {
                    let choices = options
                        .iter()
                        .enumerate()
                        .map(|(index, option)| format!("{}. {option}", index + 1))
                        .collect::<Vec<_>>();
                    self.push_history(
                        effects,
                        HistoryItem::Message {
                            text: format!(
                                "[question] {}{}",
                                prompt,
                                if choices.is_empty() {
                                    String::new()
                                } else {
                                    format!("\n{}", choices.join("\n"))
                                }
                            ),
                        },
                    );
                }
                self.offer_question(PendingQuestion {
                    question_id,
                    prompt,
                    options,
                    selected: 0,
                    child_request: None,
                });
            }
            SessionEvent::ChildQuestionRequired {
                request_id,
                prompt,
                options,
            } => {
                self.flush_stream(effects);
                if self.plain {
                    let choices = options
                        .iter()
                        .enumerate()
                        .map(|(index, option)| format!("{}. {option}", index + 1))
                        .collect::<Vec<_>>();
                    self.push_history(
                        effects,
                        HistoryItem::Message {
                            text: format!(
                                "[question] {prompt}{}",
                                if choices.is_empty() {
                                    String::new()
                                } else {
                                    format!("\n{}", choices.join("\n"))
                                }
                            ),
                        },
                    );
                }
                self.offer_question(PendingQuestion {
                    question_id: request_id.clone(),
                    prompt,
                    options,
                    selected: 0,
                    child_request: Some(request_id),
                });
                effects.push(Effect::Redraw);
            }
            SessionEvent::McpElicitationRequired {
                request_id,
                server,
                message,
                requested_schema,
                url,
            } => {
                self.flush_stream(effects);
                if self.pending_mcp_elicitation.is_some() {
                    self.service.cancel();
                    self.push_history(effects, HistoryItem::Error {
                        message: "another MCP elicitation arrived before the current request was answered; the turn was canceled".to_owned(),
                    });
                    return;
                }
                if self.plain {
                    let schema = requested_schema
                        .as_ref()
                        .map_or_else(String::new, |schema| format!("\nSchema: {schema}"));
                    let url_line = url
                        .as_ref()
                        .map_or_else(String::new, |url| format!("\nURL: {url}"));
                    self.push_history(effects, HistoryItem::Message {
                        text: format!("[MCP input from {server}] {message}{url_line}{schema}\nEnter a JSON object, or type decline/cancel."),
                    });
                }
                self.pending_mcp_elicitation = Some(PendingMcpElicitation {
                    request_id,
                    server,
                    message,
                    requested_schema,
                    url,
                });
                self.phase = AppPhase::WaitingMcpInput;
            }
            SessionEvent::TurnsListed { purpose, turns } => {
                self.show_turns(purpose, turns, effects);
            }
            SessionEvent::Refined {
                header,
                summary,
                details,
            } => {
                self.flush_stream(effects);
                self.push_history(
                    effects,
                    HistoryItem::Refinement {
                        header,
                        summary,
                        details,
                    },
                );
            }
            SessionEvent::UnreadMessage { text, verbatim } => {
                // It arrived as the turn was ending: another agent's message is read
                // like a child's report, the user's steer waits in the steering lane.
                if verbatim {
                    self.pending_notices.push_back(text);
                } else {
                    self.queue.push(super::queue::Lane::Steer, text);
                }
                self.push_history(
                    effects,
                    HistoryItem::Notice {
                        message: "a message arrived as the turn ended; it is read next".to_owned(),
                    },
                );
            }
            SessionEvent::GatesChecked { result, state } => {
                self.gates_pending = false;
                // A turn the user started meanwhile owns the session now; the gates'
                // memory is kept for the next check.
                if self.phase.has_active_run() || !self.autonomous.enabled {
                    self.autonomous.gate_state = state;
                } else {
                    let decision = self.autonomous.decide(
                        true,
                        Some((result, state)),
                        Instant::now(),
                        &autonomous_timestamp(),
                    );
                    effects.extend(self.apply_autonomous(decision));
                }
            }
            SessionEvent::ProviderWaiting { line } => {
                // The plain transcript is a log: each wait is a line of it.
                if self.plain
                    && let Some(line) = &line
                {
                    self.push_history(
                        effects,
                        HistoryItem::Notice {
                            message: line.clone(),
                        },
                    );
                }
                self.provider_wait = line;
            }
            SessionEvent::SideAnswer { question, answer } => {
                self.side_pending = false;
                match answer {
                    Ok(answer) => {
                        let mut lines = vec![format!("Q: {question}"), String::new()];
                        lines.extend(answer.lines().map(str::to_owned));
                        lines.push(String::new());
                        lines.push(
                            "reply with /btw <question> to follow up · esc to return to session"
                                .to_owned(),
                        );
                        self.reference("/btw", lines, effects);
                    }
                    Err(message) => self.push_history(
                        effects,
                        HistoryItem::Error {
                            message: format!("side question failed: {message}"),
                        },
                    ),
                }
                effects.push(Effect::Redraw);
            }
            SessionEvent::ChildSettled { name, notice } => {
                self.deliver_notice(&name, notice, effects);
            }
            SessionEvent::QuotaParked { until } => {
                let item = match self.service.park_until(until) {
                    Ok(message) => HistoryItem::Notice { message },
                    Err(message) => HistoryItem::Error { message },
                };
                self.push_history(effects, item);
            }
            SessionEvent::AgentMessage { text } => {
                self.deliver_agent_message(text, effects);
            }
            SessionEvent::AgentExchange { from, to, text } => {
                self.push_history(effects, HistoryItem::AgentExchange { from, to, text });
            }
            // prime-agent delivers the completion as it delivers an agent message:
            // steered into a running turn, a turn of its own when idle.
            SessionEvent::BashCompleted {
                pid,
                command,
                exit_code,
            } => {
                self.deliver_agent_message(bash_done_notice(pid, &command, exit_code), effects);
            }
            SessionEvent::BashConsumed { pid, command } => {
                // One read withdraws one notice that has not been delivered yet.
                let exit = |text: &String| {
                    text.starts_with(&format!("[bash-done pid:{pid} exit:"))
                        && text.ends_with(&format!(
                            "\n\nCommand: {}",
                            serde_json::to_string(&command).unwrap_or_default()
                        ))
                };
                if let Some(index) = self.pending_notices.iter().position(exit) {
                    self.pending_notices.remove(index);
                }
            }
            SessionEvent::GoalCompleted { summary } => {
                self.flush_stream(effects);
                if let Some(goal) = &mut self.goal {
                    goal.status = GoalStatus::Complete;
                    goal.summary = Some(summary.clone());
                }
                self.service.set_goal(None);
                self.push_history(
                    effects,
                    HistoryItem::Notice {
                        message: format!("goal complete: {summary}"),
                    },
                );
            }
            SessionEvent::GoalCreated {
                objective,
                token_budget,
            } => {
                self.flush_stream(effects);
                self.service.set_goal(Some(objective.clone()));
                self.push_history(
                    effects,
                    HistoryItem::Notice {
                        message: format!(
                            "goal set by the model: {objective} (/goal pause stops it)"
                        ),
                    },
                );
                self.goal = Some(GoalState::new(objective).with_budget(token_budget));
            }
            SessionEvent::CompactRequested { instructions } => {
                self.pending_compact = Some(instructions.unwrap_or_default());
            }
            SessionEvent::RenameRequested { name } => {
                let message = self
                    .service
                    .rename(&name)
                    .unwrap_or_else(|error| format!("the session could not be renamed: {error}"));
                self.push_history(effects, HistoryItem::Notice { message });
            }
            SessionEvent::GoalRestored { objective } => {
                let mut goal = GoalState::new(objective);
                goal.status = GoalStatus::Paused;
                self.push_history(
                    effects,
                    HistoryItem::Notice {
                        message: format!(
                            "this conversation has a goal: {} (paused; /goal resume continues it)",
                            goal.objective
                        ),
                    },
                );
                self.goal = Some(goal);
            }
            SessionEvent::RunTerminal { outcome } => {
                self.provider_wait = None;
                self.refresh_menu();
                self.flush_stream(effects);
                self.settle_run(Some(outcome.clone()));
                self.push_history(
                    effects,
                    HistoryItem::Run {
                        outcome: outcome.clone(),
                        steps: self.steps,
                        tool_calls: self.tool_calls,
                        elapsed: self.last_run_elapsed,
                    },
                );
                self.finish_run();
                // Every turn counts against an autonomous run's budgets.
                let used = self
                    .service
                    .session_tokens()
                    .saturating_sub(self.turn_tokens_start);
                self.autonomous.record_turn(used);
                // After the user stopped a turn, what waits - their queue and the
                // children's reports - waits for their next turn, as prime-agent
                // pauses queued work after an abort.
                if matches!(outcome, RunOutcome::Canceled) {
                    self.notices_held = true;
                }
                // What the user queued runs first: steering, then follow-ups.
                if !self.notices_held
                    && let Some(text) = self
                        .queue
                        .take_next(super::queue::Lane::Steer)
                        .or_else(|| self.queue.take_next(super::queue::Lane::FollowUp))
                {
                    self.close_run_grant();
                    effects.extend(self.dispatch(text, false));
                    return;
                }
                // What a child reported while this turn ran is read now, one turn
                // for each report.
                if !self.notices_held
                    && let Some(text) = self.pending_notices.pop_front()
                {
                    self.close_run_grant();
                    effects.extend(self.dispatch(text, true));
                    self.finish_pending_exit(effects);
                    return;
                }
                // A heartbeat that waited for this turn runs now.
                if let Some(text) = self.pending_heartbeats.pop_front() {
                    effects.extend(self.dispatch(text, true));
                    self.finish_pending_exit(effects);
                    return;
                }
                // The `compact` skill's request runs now that the turn is over; the goal,
                // if any, continues after the compaction run ends.
                if let Some(instructions) = self.pending_compact.take() {
                    let command = if instructions.trim().is_empty() {
                        "/compact".to_owned()
                    } else {
                        format!("/compact {instructions}")
                    };
                    effects.extend(self.command(&command));
                    self.finish_pending_exit(effects);
                    return;
                }
                // After `finish_run`: a continuation is a new request, and the phase has
                // to be idle again before the service will accept one.
                effects.extend(self.maybe_continue(&outcome));
                if !self.phase.has_active_run() {
                    effects.extend(self.continue_goal(&outcome));
                }
                if !self.phase.has_active_run() {
                    effects.extend(self.continue_autonomous(&outcome));
                }
                // The phase above is what tells the two cases apart: a continuation is the
                // *same* user turn carrying on, so the grant the user gave for this turn
                // stays open across it - revoking here would ask again in the middle of
                // work they already allowed. When nothing continues the turn, the grant
                // closes, on this one path, however the turn ended.
                if !self.phase.has_active_run() {
                    self.close_run_grant();
                }
                self.finish_pending_exit(effects);
            }
            SessionEvent::RecoverableError { message } => {
                self.flush_stream(effects);
                self.settle_run(None);
                self.push_history(effects, HistoryItem::Error { message });
                self.finish_run();
                // A turn that broke is over: nothing continues it from here.
                self.close_run_grant();
                self.finish_pending_exit(effects);
            }
            // The RPC mode's reads; the app asks for neither.
            SessionEvent::ConversationRead { .. } | SessionEvent::HtmlExported { .. } => {}
        }
    }

    fn submit(&mut self, text: String) -> Vec<Effect> {
        // A request the user typed starts a fresh continuation budget: the app may carry
        // this one on by itself when a bound stops it.
        self.continuations = 0;
        // The user took the session over while the gates ran: their verdict
        // would judge a turn that is no longer the last one.
        if self.gates_pending {
            self.gates_pending = false;
            self.service.cancel_gates();
        }
        self.dispatch(text, false)
    }

    /// A message sent while a turn runs: it steers the turn, or waits for it.
    fn dispatch_while_running(&mut self, text: String) -> Vec<Effect> {
        // prime-agent's Enter while the agent works: the message steers the
        // running turn - the model reads it at its next step, or right after
        // the answer it is writing - instead of waiting for the whole task to
        // finish. It shows in the conversation now, as it was sent.
        if self.phase == AppPhase::Running && self.service.steer(&text).is_ok() {
            return vec![
                Effect::History(HistoryItem::User { text }),
                Effect::History(HistoryItem::Notice {
                    message: "sent to the running agent; it reads this at its next step".to_owned(),
                }),
                Effect::Redraw,
            ];
        }
        // The turn cannot take it yet: it waits in the steering lane and is sent
        // when the turn ends, as prime-agent queues a steer it cannot deliver.
        self.queue.push(super::queue::Lane::Steer, text);
        vec![
            Effect::History(HistoryItem::Notice {
                message: format!(
                    "queued ({}): sent after the active run finishes",
                    self.queue.len()
                ),
            }),
            Effect::Redraw,
        ]
    }

    /// Send one request to the service.
    ///
    /// `automatic` marks a continuation the app started because a bound stopped the
    /// previous turn. It does not reset the budget, which is what makes the budget a
    /// budget rather than an infinite loop.
    fn dispatch(&mut self, text: String, automatic: bool) -> Vec<Effect> {
        if matches!(text.split_whitespace().next(), Some("/exit" | "/quit")) {
            return self.command(&text);
        }
        // While a browser sign-in waits, a pasted redirect URL (or code) finishes it:
        // the browser may run on another machine, as prime-agent allows.
        if self.signing_in.is_some() && !automatic && !text.trim_start().starts_with('/') {
            let mut effects = Vec::new();
            match self.service.finish_sign_in(&text) {
                Ok(()) => self.push_history(
                    &mut effects,
                    HistoryItem::Notice {
                        message: "exchanging the authorization code for tokens...".to_owned(),
                    },
                ),
                Err(message) => self.push_history(&mut effects, HistoryItem::Error { message }),
            }
            effects.push(Effect::Redraw);
            return effects;
        }
        if text.trim_start().starts_with("/steer") {
            return self.command(&text);
        }
        // While a gated action waits, the next line is the answer — never a new
        // request that would run beside the pending one.
        if self.phase == AppPhase::WaitingApproval {
            return self.answer(&text);
        }
        if self.phase == AppPhase::WaitingInput && self.pending_question.is_some() {
            return self.submit_question_answer(text);
        }
        if self.phase == AppPhase::WaitingMcpInput && self.pending_mcp_elicitation.is_some() {
            return self.answer_mcp_elicitation(&text);
        }
        if text.trim_start().starts_with('/') {
            // A leading space must not turn a command into chat text: `/login`
            // would otherwise be sent to the provider and stored in history.
            return self.command(&text);
        }
        if self.phase.has_active_run() {
            return self.dispatch_while_running(text);
        }
        let shell_prefix = match parse_shell_prefix(&text) {
            Ok(prefix) => prefix,
            Err(message) => {
                return vec![
                    Effect::History(HistoryItem::Error { message }),
                    Effect::Redraw,
                ];
            }
        };
        if let Some(shell_prefix) = shell_prefix {
            return self.dispatch_shell_prefix(text, shell_prefix);
        }
        // Ask the port at submission time, not at boot: the answer changes the
        // moment `/login` saves a credential, and a stale "setup required" would
        // refuse a request the app can now serve.
        if let Some(problem) = self.service.provider_problem() {
            return vec![
                Effect::History(HistoryItem::Error { message: problem }),
                Effect::Redraw,
            ];
        }
        if !automatic && let Some(expanded) = self.expand_inline_skills(&text) {
            return self.submit_prompt_as(expanded, text, Vec::new());
        }
        let text = self.attach_pending_shell_outputs(text);
        if !automatic {
            self.notices_held = false;
        }
        let input_id = InputId::generate();
        self.fresh_run(Instant::now(), Some(text.clone()));
        self.service.submit(SubmitRequest {
            input_id,
            text: text.clone(),
            answer_question_id: None,
            shell_prefix: None,
            compact_guidance: None,
            refine: None,
        });
        let mut effects = Vec::new();
        let item = if automatic {
            HistoryItem::Automatic { text }
        } else {
            HistoryItem::User { text }
        };
        self.push_history(&mut effects, item);
        effects.push(Effect::Redraw);
        effects
    }

    fn submit_question_answer(&mut self, text: String) -> Vec<Effect> {
        if text.trim().is_empty() {
            return vec![
                Effect::History(HistoryItem::Error {
                    message: "answer the question with text or choose one of its numbered options"
                        .to_owned(),
                }),
                Effect::Redraw,
            ];
        }
        let Some(question) = self.pending_question.take() else {
            return Vec::new();
        };
        if let Some(request_id) = &question.child_request {
            // A child waits on this answer; the turn it belongs to goes on.
            self.editor.clear();
            let delivered = self.service.answer_child_question(request_id, &text);
            self.phase = self
                .phase_before_question
                .take()
                .unwrap_or(AppPhase::Running);
            let mut effects = vec![Effect::History(HistoryItem::User { text })];
            if !delivered {
                effects.push(Effect::History(HistoryItem::Notice {
                    message: "that agent stopped waiting; the answer was not delivered".to_owned(),
                }));
            }
            self.next_waiting();
            effects.push(Effect::Redraw);
            return effects;
        }
        // The answer is sent: whatever was typed toward it goes with the panel.
        self.editor.clear();
        self.continuations = 0;
        self.fresh_run(Instant::now(), Some(text.clone()));
        self.service.submit(SubmitRequest {
            input_id: InputId::generate(),
            text: text.clone(),
            answer_question_id: Some(question.question_id),
            shell_prefix: None,
            compact_guidance: None,
            refine: None,
        });
        vec![Effect::History(HistoryItem::User { text }), Effect::Redraw]
    }

    /// Show `question` now, or after the panel that is open.
    fn offer_question(&mut self, question: PendingQuestion) {
        if self.pending_question.is_some() || self.pending_approval.is_some() {
            self.question_queue.push_back(question);
            return;
        }
        if question.child_request.is_some() {
            self.phase_before_question = Some(self.phase);
        }
        self.pending_question = Some(question);
        self.phase = AppPhase::WaitingInput;
    }

    /// Open the next waiting approval or question, oldest approval first.
    fn next_waiting(&mut self) {
        if self.pending_approval.is_some() || self.pending_question.is_some() {
            return;
        }
        if let Some(approval) = self.approval_queue.pop_front() {
            self.pending_approval = Some(approval);
            self.phase = AppPhase::WaitingApproval;
        } else if let Some(question) = self.question_queue.pop_front() {
            self.offer_question(question);
        }
    }

    fn dispatch_shell_prefix(&mut self, text: String, shell_prefix: ShellPrefix) -> Vec<Effect> {
        self.continuations = 0;
        self.fresh_run(Instant::now(), Some(text.clone()));
        self.service.submit(SubmitRequest {
            input_id: InputId::generate(),
            text: text.clone(),
            answer_question_id: None,
            shell_prefix: Some(shell_prefix),
            compact_guidance: None,
            refine: None,
        });
        let mut effects = Vec::new();
        self.push_history(&mut effects, HistoryItem::User { text });
        effects.push(Effect::Redraw);
        effects
    }

    fn push_shell_output(&mut self, command: &str, output: &str) {
        const MAX_PENDING_BYTES: usize = 64 * 1024;
        let mut block = format!("[shell] {command}\n{output}");
        let pending_bytes = self
            .pending_shell_outputs
            .iter()
            .map(String::len)
            .sum::<usize>();
        let remaining = MAX_PENDING_BYTES.saturating_sub(pending_bytes);
        if block.len() > remaining {
            const TRUNCATION: &str = "\n[attachment truncated: 64 KiB session limit]";
            let max_content = remaining.saturating_sub(TRUNCATION.len());
            let mut boundary = max_content.min(block.len());
            while !block.is_char_boundary(boundary) {
                boundary = boundary.saturating_sub(1);
            }
            block.truncate(boundary);
            if remaining >= TRUNCATION.len() {
                block.push_str(TRUNCATION);
            }
        }
        if !block.is_empty() {
            self.pending_shell_outputs.push(block);
        }
    }

    fn attach_pending_shell_outputs(&mut self, mut text: String) -> String {
        if self.pending_shell_outputs.is_empty() {
            return text;
        }
        let blocks = std::mem::take(&mut self.pending_shell_outputs).join("\n\n");
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&blocks);
        text
    }

    /// Continue a turn a bound stopped, while the continuation budget lasts.
    ///
    /// The bounds exist to stop a loop that has gone wrong, not to end a task that is
    /// still moving: a turn that stopped after eight model calls had usually not
    /// finished, and making the user type "continue" to let their own agent keep
    /// working reads as a stall. So the app continues by itself, says so, and stops
    /// asking once the budget is spent — after that the pause is real.
    fn maybe_continue(&mut self, outcome: &RunOutcome) -> Vec<Effect> {
        let RunOutcome::Paused(reason) = outcome else {
            return Vec::new();
        };
        if !reason.is_continuable() || self.continuations >= self.max_continuations {
            return Vec::new();
        }
        self.continuations += 1;
        let mut effects = vec![Effect::History(HistoryItem::Notice {
            message: format!(
                "{}; continuing automatically ({} of {}) — Ctrl-C stops this",
                reason.label(),
                self.continuations,
                self.max_continuations
            ),
        })];
        effects.extend(self.dispatch(CONTINUATION_TEXT.to_owned(), true));
        effects
    }

    /// Carry an active goal into another turn once a turn finished without it.
    ///
    /// prime-agent's persistent goals: the model stopping is not the goal being done -
    /// only `goal_complete` is. Each carried turn spends the goal's own budget, and a
    /// spent budget pauses the goal instead of looping.
    fn continue_goal(&mut self, outcome: &RunOutcome) -> Vec<Effect> {
        let Some(goal) = &mut self.goal else {
            return Vec::new();
        };
        if goal.status != GoalStatus::Active || !matches!(outcome, RunOutcome::Done) {
            return Vec::new();
        }
        if goal.continuations >= goal.max_continuations {
            goal.status = GoalStatus::Paused;
            let message = format!(
                "goal paused after {} automatic turn(s) without being completed; /goal resume continues it",
                goal.continuations
            );
            self.service.set_goal(None);
            return vec![Effect::History(HistoryItem::Notice { message })];
        }
        goal.continuations += 1;
        let text = super::goal::continuation_text(goal);
        let mut effects = vec![Effect::History(HistoryItem::Notice {
            message: format!(
                "goal not complete yet; continuing ({} of {}) - Ctrl-C or /goal pause stops this",
                goal.continuations, goal.max_continuations
            ),
        })];
        self.continuations = 0;
        effects.extend(self.dispatch(text, true));
        effects
    }

    /// prime-agent's autonomous continuation after a turn: the gates run first
    /// when there are any, and their verdict (`GatesChecked`) decides.
    fn continue_autonomous(&mut self, outcome: &RunOutcome) -> Vec<Effect> {
        if !self.autonomous.enabled || !matches!(outcome, RunOutcome::Done) {
            return Vec::new();
        }
        let mut effects = Vec::new();
        // prime-agent holds its continuation while children run: their reports
        // wake the parent, and the turn those reports start is judged in turn.
        let running = self.service.running_children();
        if running > 0 {
            self.push_history(
                &mut effects,
                HistoryItem::Notice {
                    message: format!(
                        "autonomous: {running} child(ren) still working; their reports carry the run on"
                    ),
                },
            );
            return effects;
        }
        if let Some(job) = self.autonomous.gate_job() {
            let commands = job.commands.len();
            match self.service.run_gates(job) {
                Ok(()) => {
                    self.gates_pending = true;
                    self.push_history(
                        &mut effects,
                        HistoryItem::Notice {
                            message: format!("autonomous: running {commands} quality gate(s)..."),
                        },
                    );
                }
                Err(message) => self.push_history(&mut effects, HistoryItem::Error { message }),
            }
            return effects;
        }
        let decision = self
            .autonomous
            .decide(true, None, Instant::now(), &autonomous_timestamp());
        effects.extend(self.apply_autonomous(decision));
        effects
    }

    /// Continue the run, or say why autonomous mode stopped carrying it.
    fn apply_autonomous(&mut self, decision: super::autonomous::Decision) -> Vec<Effect> {
        use super::autonomous::{Decision, Stop};
        let message = match decision {
            Decision::Continue(text) => {
                let limit = self.autonomous.limits.max_continuations;
                let mut effects = vec![Effect::History(HistoryItem::Notice {
                    message: format!(
                        "autonomous: continuing ({}/{}) - /autonomous off stops this",
                        self.autonomous.continuations_used,
                        if limit >= super::autonomous::UNLIMITED {
                            "unlimited".to_owned()
                        } else {
                            limit.to_string()
                        }
                    ),
                })];
                self.continuations = 0;
                effects.extend(self.dispatch(text, true));
                return effects;
            }
            Decision::Stop(Stop::NotNeeded) => return Vec::new(),
            Decision::Stop(Stop::GatesPassed) => "autonomous: quality gates passed".to_owned(),
            Decision::Stop(Stop::GateRetriesExhausted) => format!(
                "autonomous: stopped; a quality gate failed more than {} time(s)",
                self.autonomous.gates.max_retries
            ),
            Decision::Stop(Stop::Limit(reason)) => format!("autonomous: stopped; {reason} reached"),
        };
        vec![
            Effect::History(HistoryItem::Notice { message }),
            Effect::Redraw,
        ]
    }

    /// `/autonomous [status|off]` and `/autonomous on [flags]`.
    fn autonomous_command(&mut self, argument: Option<&str>, effects: &mut Vec<Effect>) {
        match super::autonomous::parse(argument.unwrap_or_default()) {
            Err(message) => self.push_history(effects, HistoryItem::Error { message }),
            Ok(command) => {
                match command {
                    super::autonomous::Command::On(options) => {
                        // One driver at a time: autonomous mode and a goal both
                        // carry the conversation on by themselves.
                        if let Some(goal) = &mut self.goal
                            && goal.status == GoalStatus::Active
                        {
                            goal.status = GoalStatus::Paused;
                            self.service.set_goal(None);
                            self.push_history(effects, HistoryItem::Notice {
                                message: "goal paused: autonomous mode drives this conversation now; /goal resume brings it back".to_owned(),
                            });
                        }
                        self.autonomous.turn_on(&options, Instant::now());
                    }
                    super::autonomous::Command::Off => {
                        self.autonomous.turn_off();
                        if self.gates_pending {
                            self.gates_pending = false;
                            self.service.cancel_gates();
                        }
                    }
                    super::autonomous::Command::Status => {}
                }
                let message = self.autonomous.status(Instant::now());
                self.push_history(effects, HistoryItem::Notice { message });
            }
        }
    }

    /// A goal takes over from autonomous mode.
    fn autonomous_yields_to_goal(&mut self, effects: &mut Vec<Effect>) {
        if !self.autonomous.enabled {
            return;
        }
        self.autonomous.turn_off();
        if self.gates_pending {
            self.gates_pending = false;
            self.service.cancel_gates();
        }
        self.push_history(
            effects,
            HistoryItem::Notice {
                message: "autonomous mode turned off: the goal drives this conversation now"
                    .to_owned(),
            },
        );
    }

    /// `/goal` and its subcommands.
    fn goal_command(&mut self, raw_argument: Option<&str>, effects: &mut Vec<Effect>) {
        match raw_argument.map(str::trim).unwrap_or_default() {
            "" | "status" => match &self.goal {
                Some(goal) => {
                    let lines = goal.describe();
                    self.reference("/goal", lines, effects);
                }
                None => self.push_history(effects, HistoryItem::Notice {
                    message: "no goal is set; /goal <objective> sets one and keeps working until it is complete".to_owned(),
                }),
            },
            "pause" => {
                let message = match &mut self.goal {
                    Some(goal) if goal.status == GoalStatus::Active => {
                        goal.status = GoalStatus::Paused;
                        "goal paused; the current turn finishes, then nothing continues it".to_owned()
                    }
                    Some(goal) => format!("the goal is already {}", goal.status.label()),
                    None => "no goal is set".to_owned(),
                };
                self.service.set_goal(None);
                self.push_history(effects, HistoryItem::Notice { message });
            }
            "resume" => {
                let Some(goal) = &mut self.goal else {
                    self.push_history(effects, HistoryItem::Notice {
                        message: "no goal is set".to_owned(),
                    });
                    return;
                };
                if goal.status == GoalStatus::Complete {
                    self.push_history(effects, HistoryItem::Notice {
                        message: "the goal is already complete; /goal <objective> sets a new one".to_owned(),
                    });
                    return;
                }
                goal.status = GoalStatus::Active;
                goal.continuations = 0;
                let objective = goal.objective.clone();
                let continuation = super::goal::continuation_text(goal);
                self.autonomous_yields_to_goal(effects);
                self.service.set_goal(Some(objective.clone()));
                self.push_history(effects, HistoryItem::Notice {
                    message: format!("goal resumed: {objective}"),
                });
                if !self.phase.has_active_run() {
                    self.continuations = 0;
                    effects.extend(self.dispatch(continuation, true));
                }
            }
            "clear" => {
                let message = if self.goal.take().is_some() {
                    "goal cleared"
                } else {
                    "no goal is set"
                };
                self.service.forget_goal();
                self.push_history(effects, HistoryItem::Notice {
                    message: message.to_owned(),
                });
            }
            objective => {
                let objective = objective.to_owned();
                self.autonomous_yields_to_goal(effects);
                self.goal = Some(GoalState::new(objective.clone()));
                self.service.set_goal(Some(objective.clone()));
                if self.phase.has_active_run() {
                    self.push_history(effects, HistoryItem::Notice {
                        message: "goal set; it applies from the next turn".to_owned(),
                    });
                } else {
                    self.continuations = 0;
                    effects.extend(self.dispatch(format!("Goal: {objective}"), false));
                }
            }
        }
    }

    /// Start the accounting for a new turn.
    fn fresh_run(&mut self, now: Instant, request: Option<String>) {
        self.turn_tokens_start = self.service.session_tokens();
        self.pending_text.clear();
        self.pending_newlines = 0;
        self.last_answer.clear();
        self.open_tools.clear();
        self.tool_progress.clear();
        self.steps = 0;
        self.tool_calls = 0;
        self.last_run_elapsed = Duration::ZERO;
        self.run_started_at = Some(now);
        if request.is_some() {
            self.last_request = request;
        }
        self.phase = AppPhase::Running;
    }

    /// Close the accounting for a finished turn.
    fn settle_run(&mut self, _outcome: Option<RunOutcome>) {
        self.last_run_elapsed = self
            .run_started_at
            .take()
            .map_or(Duration::ZERO, |started| started.elapsed());
        self.open_tools.clear();
        self.tool_progress.clear();
    }

    fn interrupt(&mut self) -> Vec<Effect> {
        if self.phase.has_active_run() {
            // The queue survives the interrupt, as prime-agent keeps it: what
            // the user queued is still theirs to send (`/queue drop` removes it).
            self.service.cancel();
            // Ctrl-C also means "do not start another one": the budget is spent, so the
            // bound that ends the canceled turn is a real stop until the user speaks.
            self.continuations = self.max_continuations;
            if let Some(goal) = &mut self.goal
                && goal.status == GoalStatus::Active
            {
                goal.status = GoalStatus::Paused;
                self.service.set_goal(None);
            }
            self.phase = AppPhase::Canceling;
            return vec![
                Effect::History(HistoryItem::Notice {
                    message: "^C canceling the active run...".to_owned(),
                }),
                Effect::Redraw,
            ];
        }
        // prime-agent: ctrl+c on an empty, idle prompt arms an exit, and a second
        // press within two seconds leaves the app.
        if self.editor.is_empty() && !self.plain {
            if self
                .exit_armed_at
                .is_some_and(|armed| armed.elapsed() < Duration::from_secs(2))
            {
                self.exit_armed_at = None;
                return self.command("/exit");
            }
            self.exit_armed_at = Some(Instant::now());
            return vec![
                Effect::History(HistoryItem::Notice {
                    message: "Press ctrl+c again to exit".to_owned(),
                }),
                Effect::Redraw,
            ];
        }
        self.editor.clear();
        vec![
            Effect::History(HistoryItem::Message {
                text: String::new(),
            }),
            Effect::Redraw,
        ]
    }

    fn open_file_picker(&mut self) {
        self.file_picker_candidates = file_picker_candidates(&self.context.project.root);
        self.file_picker_query.clear();
        self.file_picker_active = true;
        self.editor.open_picker(self.file_picker_candidates.clone());
        self.refresh_file_picker();
    }

    fn refresh_file_picker(&mut self) {
        if !self.file_picker_active {
            return;
        }
        let query = self.file_picker_query.to_lowercase();
        let items = self
            .file_picker_candidates
            .iter()
            .filter(|path| path.to_lowercase().contains(&query))
            .cloned()
            .collect();
        self.editor.open_picker(items);
    }

    #[allow(clippy::too_many_lines)]
    fn command(&mut self, line: &str) -> Vec<Effect> {
        // Commands that take free text read `raw_argument`; those that take one
        // word read `argument`.
        let trimmed = line.trim();
        let name = trimmed.split_whitespace().next().unwrap_or_default();
        let raw_argument = trimmed
            .get(name.len()..)
            .map(str::trim)
            .filter(|rest| !rest.is_empty());
        let argument = raw_argument.and_then(|rest| rest.split_whitespace().next());
        let mut effects = Vec::new();
        // An alias runs its command: `/thinking` is `/effort`, `/exit` is `/quit`.
        let typed = name;
        let name = super::commands::canonical(typed);
        match name {
            "/quit" => {
                // A background agent keeps working: only this client goes away.
                if self.detachable {
                    effects.push(Effect::Exit(EXIT_SUCCESS));
                    return effects;
                }
                if self.phase.has_active_run() {
                    self.service.cancel();
                    self.exit_after_run = true;
                    self.phase = AppPhase::Canceling;
                    self.push_history(
                        &mut effects,
                        HistoryItem::Notice {
                            message: "^C canceling the active run before exit".to_owned(),
                        },
                    );
                    effects.push(Effect::Redraw);
                    return effects;
                }
                self.phase = AppPhase::Closed;
                effects.push(Effect::Exit(EXIT_SUCCESS));
                return effects;
            }
            "/help" => {
                let lines = if self.plain || argument == Some("all") {
                    view::help_lines()
                } else {
                    view::help_card_lines()
                };
                self.reference("/help", lines, &mut effects);
            }
            "/image" => {
                self.paste_image(&mut effects);
            }
            "/attach" => {
                match raw_argument {
                    Some(paths) => self.attach_file(paths, &mut effects),
                    None => self.push_history(
                        &mut effects,
                        HistoryItem::Notice {
                            message: "/attach <path>: name the file to attach, or type or paste the path into your message. An image is shown to the model, a text file is put in the message; a path with spaces has to be quoted"
                                .to_owned(),
                        },
                    ),
                }
            }
            "/session" => {
                let mut lines = self.header.clone();
                lines.push(format!("Phase:   {}", self.phase.label()));
                // The project id names this workspace's store, and the app shows it
                // nowhere else: the projects directory is named after a digest, so
                // without this line there is nothing to hand to `--project-id`.
                lines.push(match self.service.project_id() {
                    Some(id) => format!("Project: {id} (store scope for this workspace)"),
                    None => {
                        "Project: not registered yet; the first turn in this workspace creates it"
                            .to_owned()
                    }
                });
                // What one turn may spend, and whether the app carries on by itself when
                // a bound stops it: "paused" without this line is a mystery, and these
                // are the numbers to raise when a long task keeps pausing.
                lines.push(bounds::describe(
                    &self.service.turn_limits(),
                    self.max_continuations,
                ));
                // The provider facts answer "what is this app actually using?":
                // which credential variable holds the key (never its value), whether
                // the endpoint and the model came from the environment or from the
                // defaults, and whether the endpoint is reachable at all.
                lines.extend(self.service.provider_diagnostics());
                self.reference("/status", lines, &mut effects);
            }
            "/config" => {
                let mut lines = self.service.config_explain();
                lines.extend(self
                    .header
                    .iter()
                    .filter(|line| ["Config:", "Data:", "Store:"].iter().any(|prefix| line.starts_with(prefix)))
                    .cloned());
                self.reference("/config", lines, &mut effects);
            }
            "/permissions" => {
                if let Some(mode) = argument {
                    // Allowed while the agent works, as Claude Code's Shift+Tab
                    // is: the running turn's next action is decided under it.
                    let running = self.phase.has_active_run();
                    match self.service.set_mode(mode) {
                        Ok(message) => self.push_history(&mut effects, HistoryItem::Notice {
                            message: if running {
                                format!("{message}; the running turn follows it from its next action")
                            } else {
                                message
                            },
                        }),
                        Err(message) => self.push_history(&mut effects, HistoryItem::Error { message }),
                    }
                    self.refresh_menu();
                    // The banner's mode badge reads the header refresh_menu just
                    // updated; without a redraw it kept saying "ask".
                    self.refresh_banner(&mut effects);
                } else {
                    let mut lines = self.service.permissions_summary();
                    lines.push(
                        "choose a mode: /permissions ask|auto-edit|full-auto (Tab or Enter opens the list)"
                            .to_owned(),
                    );
                    self.reference("/permissions", lines, &mut effects);
                }
            }
            "/hooks" => {
                self.reference("/hooks", self.service.hooks_summary(), &mut effects);
            }
            "/mcp" => {
                // prime-agent's `/mcp add | list | get | remove`; bare `/mcp`
                // shows the servers and how to add one.
                match raw_argument.map(super::mcp_config::split_words) {
                    Some(args) if !args.is_empty() => match self.service.manage_mcp(&args) {
                        Ok(lines) if lines.len() == 1 => self.push_history(
                            &mut effects,
                            HistoryItem::Notice {
                                message: lines.into_iter().next().unwrap_or_default(),
                            },
                        ),
                        Ok(lines) => self.reference("/mcp", lines, &mut effects),
                        Err(message) => {
                            self.push_history(&mut effects, HistoryItem::Error { message });
                        }
                    },
                    _ => {
                        let mut lines = self.service.mcp_summary();
                        lines.push(String::new());
                        lines.extend(super::mcp_config::USAGE.iter().map(|line| (*line).to_owned()));
                        self.reference("/mcp", lines, &mut effects);
                    }
                }
            }
            "/plugins" => self.plugins_command(raw_argument, &mut effects),
            "/queue" => self.queue_command(raw_argument, &mut effects),
            "/stash" => return self.stash_prompt(),
            "/autonomous" => self.autonomous_command(raw_argument, &mut effects),
            "/schedule" => match self.service.schedule(raw_argument) {
                Ok(lines) => self.reference("/schedule", lines, &mut effects),
                Err(message) => self.push_history(&mut effects, HistoryItem::Error { message }),
            },
            "/heartbeats" | "/heartbeat" => {
                // `/heartbeat <instruction>` adds one, as prime-agent's does.
                let argument = match (name, raw_argument) {
                    ("/heartbeat", Some(text))
                        if !text.trim_start().starts_with("every ")
                            && !text.trim_start().starts_with("each ") =>
                    {
                        Some(format!("every 5m {text}"))
                    }
                    (_, argument) => argument.map(str::to_owned),
                };
                match self.service.heartbeat(argument.as_deref()) {
                    Ok(lines) => self.reference(name, lines, &mut effects),
                    Err(message) => self.push_history(&mut effects, HistoryItem::Error { message }),
                }
            }
            "/scoped-models" => match self.service.scoped_models(raw_argument) {
                Ok(lines) => {
                    self.reference("/scoped-models", lines, &mut effects);
                    self.refresh_status();
                }
                Err(message) => self.push_history(&mut effects, HistoryItem::Error { message }),
            },
            "/fork" | "/clone" | "/tree" if self.phase.has_active_run() => {
                self.push_history(
                    &mut effects,
                    HistoryItem::Error {
                        message: format!("{name} waits for the active run to finish"),
                    },
                );
                effects.push(Effect::Redraw);
            }
            "/fork" => self.branch_command(name, argument, &mut effects),
            "/tree" => self.tree_command(raw_argument, &mut effects),
            "/clone" => {
                match self.service.clone_conversation() {
                    Ok(()) => {
                        self.editor.clear();
                        self.push_history(
                            &mut effects,
                            HistoryItem::Notice {
                                message: "Cloned to new session".to_owned(),
                            },
                        );
                    }
                    Err(message) => {
                        self.push_history(&mut effects, HistoryItem::Notice { message });
                    }
                }
                effects.push(Effect::Redraw);
            }
            // prime-agent's side question: answered from the conversation, kept
            // out of it. It may run while the agent works.
            "/btw" => {
                let message = if self.side_pending {
                    Err("Wait for the current side question to finish or cancel it first."
                        .to_owned())
                } else {
                    self.service
                        .side_question(raw_argument.unwrap_or_default())
                        .map(|()| {
                            self.side_pending = true;
                            "btw: asking on the side; the answer opens in a panel".to_owned()
                        })
                };
                match message {
                    Ok(message) => self.push_history(&mut effects, HistoryItem::Notice { message }),
                    Err(message) => self.push_history(&mut effects, HistoryItem::Error { message }),
                }
                effects.push(Effect::Redraw);
            }
            "/agents" => match raw_argument.map(|rest| rest.split_once(' ').unwrap_or((rest, ""))) {
                // prime-agent stops a child with `rlm.delete_subagent`; the user
                // stops one here, and it reports as cancelled.
                Some(("stop", target)) => {
                    let target = if target.trim().is_empty() { "all" } else { target.trim() };
                    match self.service.stop_agents(target) {
                        Ok(message) => self.push_history(
                            &mut effects,
                            HistoryItem::Notice { message },
                        ),
                        Err(message) => {
                            self.push_history(&mut effects, HistoryItem::Error { message });
                        }
                    }
                    effects.push(Effect::Redraw);
                }
                // prime-agent's agent-message ingestion controls
                // (`agent_messages_status|pause|resume|clear`).
                Some(("messages", action)) if !action.trim().is_empty() => {
                    self.agent_messages_command(action.trim(), &mut effects);
                }
                Some(("messages", _)) => {
                    self.reference("/agents messages", self.service.agent_exchanges(), &mut effects);
                }
                _ => self.reference("/agents", self.service.agents_summary(), &mut effects),
            },
            // prime-agent's `/fast`: priority on, or back to the default tier.
            "/fast" => {
                let (current, available) = self.service.service_tier();
                if available.contains(&"priority") {
                    let next = if current == "priority" { "default" } else { "priority" };
                    self.apply_tier(next, true, &mut effects);
                } else {
                    self.push_history(&mut effects, HistoryItem::Notice {
                        // prime-agent's wording for a model without the priority tier.
                        message: "Fast mode requires GPT-5.4, GPT-5.5, or GPT-5.6 with ChatGPT or OpenAI API key authentication".to_owned(),
                    });
                }
                effects.push(Effect::Redraw);
            }
            // prime-agent's `/tier [default|flex|priority|auto]`.
            "/tier" => {
                match raw_argument.map(str::trim).filter(|tier| !tier.is_empty()) {
                    None => {
                        let (current, available) = self.service.service_tier();
                        self.push_history(&mut effects, HistoryItem::Notice {
                            message: format!(
                                "Service tier: {current} (available: {})",
                                available.join(", ")
                            ),
                        });
                    }
                    Some(tier) => self.apply_tier(tier, false, &mut effects),
                }
                effects.push(Effect::Redraw);
            }
            // prime-agent's `/fullscreen [on|off]`, kept as `terminal.fullscreen`.
            "/fullscreen" => {
                let enable = match argument {
                    None => Some(!self.fullscreen),
                    Some("on") => Some(true),
                    Some("off") => Some(false),
                    Some(_) => None,
                };
                match enable {
                    None => self.push_history(&mut effects, HistoryItem::Error {
                        message: "Usage: /fullscreen [on|off]".to_owned(),
                    }),
                    Some(enable) => {
                        if let Err(message) = self.service.set_fullscreen(enable) {
                            self.push_history(&mut effects, HistoryItem::Error { message });
                        }
                        if self.plain {
                            self.push_history(&mut effects, HistoryItem::Notice {
                                message: "Fullscreen rendering requires an interactive terminal"
                                    .to_owned(),
                            });
                        } else {
                            self.fullscreen = enable;
                            effects.push(Effect::Fullscreen(enable));
                            self.push_history(&mut effects, HistoryItem::Notice {
                                message: if enable {
                                    format!(
                                        "Fullscreen rendering on — wheel/pageUp scroll, {} follows output",
                                        super::tui::fullscreen::FOLLOW_KEY
                                    )
                                } else {
                                    "Fullscreen rendering off".to_owned()
                                },
                            });
                        }
                    }
                }
                effects.push(Effect::Redraw);
            }
            "/subagent-effort" => {
                let change = match argument {
                    None => super::service::SubagentSetting::Show,
                    Some("inherit") => super::service::SubagentSetting::Inherit,
                    Some(level) => super::service::SubagentSetting::Set(level.to_owned()),
                };
                match self.service.subagent_effort(change) {
                    Ok(message) => {
                        self.push_history(&mut effects, HistoryItem::Notice { message });
                    }
                    Err(message) => self.push_history(&mut effects, HistoryItem::Error { message }),
                }
                effects.push(Effect::Redraw);
            }
            "/subagent-model" => {
                let change = match argument {
                    None => super::service::SubagentSetting::Show,
                    Some("inherit") => super::service::SubagentSetting::Inherit,
                    Some(model) => super::service::SubagentSetting::Set(model.to_owned()),
                };
                match self.service.subagent_model(change) {
                    Ok(message) => {
                        self.push_history(&mut effects, HistoryItem::Notice { message });
                    }
                    Err(message) => self.push_history(&mut effects, HistoryItem::Error { message }),
                }
                effects.push(Effect::Redraw);
            }
            // prime-agent's `/rlm-max-depth [<int> [--global]]`.
            "/rlm-max-depth" => {
                let words = raw_argument
                    .unwrap_or_default()
                    .split_whitespace()
                    .collect::<Vec<_>>();
                // `Err(())`: the words are not `[<int> [--global]]`.
                let change: Result<Option<(u32, bool)>, ()> = match words.as_slice() {
                    [] => Ok(None),
                    [depth] => depth.parse().map(|depth| Some((depth, false))).map_err(drop),
                    [depth, "--global"] => {
                        depth.parse().map(|depth| Some((depth, true))).map_err(drop)
                    }
                    _ => Err(()),
                };
                match change {
                    Ok(Some((depth, _))) if depth > super::delegation::RLM_MAX_DEPTH_CAP => {
                        self.push_history(&mut effects, HistoryItem::Error {
                            message: format!(
                                "ha runs children at most {} levels deep; /rlm-max-depth takes 0..={}",
                                super::delegation::RLM_MAX_DEPTH_CAP,
                                super::delegation::RLM_MAX_DEPTH_CAP
                            ),
                        });
                    }
                    Ok(change) => {
                        if let Err(message) = self.service.rlm_max_depth(change) {
                            self.push_history(&mut effects, HistoryItem::Error { message });
                        }
                    }
                    Err(()) => self.push_history(&mut effects, HistoryItem::Notice {
                        message: "Usage: /rlm-max-depth [<non-negative integer> [--global]]".to_owned(),
                    }),
                }
                effects.push(Effect::Redraw);
            }
            // prime-agent's `/import <path.jsonl>`.
            "/import" => {
                match raw_argument.map(str::trim).filter(|path| !path.is_empty()) {
                    None => self.push_history(&mut effects, HistoryItem::Error {
                        message: "Usage: /import <path.jsonl>".to_owned(),
                    }),
                    Some(_) if self.phase.has_active_run() => {
                        self.push_history(&mut effects, HistoryItem::Notice {
                            message: "cannot import a session while a run is active; press Ctrl-C to cancel it first".to_owned(),
                        });
                    }
                    Some(path) => match self.service.import_session(path) {
                        Ok(message) => {
                            // A new conversation starts, as `/new` starts one.
                            self.header.retain(|line| !line.starts_with("Context:"));
                            self.goal = None;
                            self.session_candidates.clear();
                            self.push_history(&mut effects, HistoryItem::Notice { message });
                        }
                        Err(message) => {
                            self.push_history(&mut effects, HistoryItem::Error { message });
                        }
                    },
                }
                effects.push(Effect::Redraw);
            }
            // prime-agent's `/logs`.
            "/logs" => self.reference("Logs", self.service.logs(), &mut effects),
            "/skills" => {
                let rows = self.service.skills_rows();
                self.reference_rows("/skills", rows, &mut effects);
            }
            "/reload" => {
                if self.phase.has_active_run() {
                    self.push_history(&mut effects, HistoryItem::Error {
                        message: "cannot reload prompt inputs while a run is active".to_owned(),
                    });
                } else {
                    match self.service.reload() {
                        Ok(message) => self.push_history(&mut effects, HistoryItem::Notice { message }),
                        Err(message) => self.push_history(&mut effects, HistoryItem::Error { message }),
                    }
                    self.refresh_menu();
                }
            }
            skill_command if skill_command.starts_with("/skill:") => {
                let skill_name = skill_command.trim_start_matches("/skill:");
                if skill_name.is_empty() {
                    self.push_history(&mut effects, HistoryItem::Error {
                        message: "usage: /skill:<name> [task]".to_owned(),
                    });
                } else {
                    // As prime-agent does: the skill goes to the model as the
                    // message itself, so it is in the conversation and a later
                    // "which skill did I just load?" has its answer there. The row
                    // shows what was typed; the model reads the whole skill.
                    match self
                        .service
                        .expand_skill(skill_name, raw_argument.unwrap_or_default())
                    {
                        Ok(message) => {
                            return self.submit_prompt_as(message, trimmed.to_owned(), effects);
                        }
                        Err(message) => self.push_history(&mut effects, HistoryItem::Error { message }),
                    }
                }
            }
            "/steer" => {
                if !self.phase.has_active_run() {
                    self.push_history(&mut effects, HistoryItem::Error {
                        message: "/steer is available only while a run is active".to_owned(),
                    });
                } else if let Some(text) = raw_argument {
                    match self.service.steer(text) {
                        Ok(()) => self.push_history(&mut effects, HistoryItem::Notice {
                            message: "steering note queued for the next safe step".to_owned(),
                        }),
                        Err(message) => self.push_history(&mut effects, HistoryItem::Error { message }),
                    }
                } else {
                    self.push_history(&mut effects, HistoryItem::Error {
                        message: "usage: /steer <text>".to_owned(),
                    });
                }
            }
            "/cost" => {
                self.reference(
                    "/cost",
                    vec![format!("session cost: {}", self.service.cost_summary())],
                    &mut effects,
                );
            }
            "/context" => {
                self.reference("/context", self.service.context_summary(), &mut effects);
            }
            "/undo" => self.start_host_action("/undo".to_owned(), &mut effects),
            "/export" => {
                // prime-agent exports HTML unless a path says otherwise.
                let path = raw_argument.unwrap_or("session-export.html");
                let path_value = std::path::Path::new(path);
                if path_value.is_absolute()
                    || path_value.components().any(|component| {
                        matches!(component, std::path::Component::ParentDir)
                    })
                {
                    self.push_history(&mut effects, HistoryItem::Error {
                        message: "export path must stay inside the workspace".to_owned(),
                    });
                } else if !matches!(path_value.extension().and_then(|ext| ext.to_str()), Some("md" | "jsonl" | "html")) {
                    self.push_history(&mut effects, HistoryItem::Error {
                        message: "usage: /export [path.md|path.jsonl|path.html]".to_owned(),
                    });
                } else {
                    self.start_host_action(format!("/export {path}"), &mut effects);
                }
            }
            "/share" => {
                if self.phase.has_active_run() {
                    self.push_history(&mut effects, HistoryItem::Notice {
                        message: "wait for the running turn to finish before sharing".to_owned(),
                    });
                } else {
                    self.start_host_action("/share".to_owned(), &mut effects);
                }
            }
            "/copy" => {
                if self.plain {
                    self.push_history(&mut effects, HistoryItem::Notice {
                        message: "/copy is available in TUI mode; the plain renderer does not access the clipboard".to_owned(),
                    });
                } else if self.last_answer.is_empty() {
                    self.push_history(&mut effects, HistoryItem::Notice {
                        message: "there is no assistant answer to copy yet".to_owned(),
                    });
                } else {
                    effects.push(Effect::Copy(self.last_answer.clone()));
                }
            }
            "/goal" => self.goal_command(raw_argument, &mut effects),
            "/effort" => {
                if let Some(level) = argument {
                    // Allowed while the agent works, as in prime-agent and Claude
                    // Code: the running turn uses it from its next model call.
                    match self.service.set_thinking(level) {
                        Ok(message) => self.push_history(&mut effects, HistoryItem::Notice {
                            message: if self.phase.has_active_run() {
                                format!(
                                    "{}; the running turn uses it from its next step",
                                    message.replace(" for the next turn", "")
                                )
                            } else {
                                message
                            },
                        }),
                        Err(message) => self.push_history(&mut effects, HistoryItem::Error { message }),
                    }
                    self.refresh_status();
                    self.refresh_banner(&mut effects);
                } else {
                    let lines = self.service.thinking_status();
                    self.reference("/effort", lines, &mut effects);
                }
            }
            "/name" => {
                if self.phase.has_active_run() {
                    self.push_history(&mut effects, HistoryItem::Notice {
                        message: "cannot rename the session while a run is active".to_owned(),
                    });
                } else if let Some(title) = raw_argument {
                    match self.service.rename(title) {
                        Ok(message) => {
                            self.chat_title =
                                Some(title.split_whitespace().collect::<Vec<_>>().join(" "));
                            self.push_history(&mut effects, HistoryItem::Notice { message });
                        }
                        Err(message) => self.push_history(&mut effects, HistoryItem::Error { message }),
                    }
                } else {
                    self.push_history(&mut effects, HistoryItem::Error {
                        message: "usage: /name <name up to 60 characters>".to_owned(),
                    });
                }
            }
            "/compact" => {
                if self.phase.has_active_run() {
                    self.push_history(&mut effects, HistoryItem::Notice {
                        message: "cannot compact while a run is active; wait for it to finish".to_owned(),
                    });
                } else if let Some(problem) = self.service.provider_problem() {
                    self.push_history(&mut effects, HistoryItem::Error { message: problem });
                } else {
                    let guidance = raw_argument.unwrap_or_default().to_owned();
                    let text = if guidance.is_empty() {
                        "/compact".to_owned()
                    } else {
                        format!("/compact {guidance}")
                    };
                    self.continuations = 0;
                    self.fresh_run(Instant::now(), Some(text.clone()));
                    self.service.submit(SubmitRequest {
                        input_id: InputId::generate(),
                        text: text.clone(),
                        answer_question_id: None,
                        shell_prefix: None,
                        compact_guidance: Some(guidance),
                        refine: None,
                    });
                    self.push_history(&mut effects, HistoryItem::User { text });
                }
            }
            "/refine" => {
                if self.phase.has_active_run() {
                    self.push_history(&mut effects, HistoryItem::Notice {
                        message: "cannot refine while a run is active; wait for it to finish".to_owned(),
                    });
                } else if let Some(problem) = self.service.provider_problem() {
                    self.push_history(&mut effects, HistoryItem::Error { message: problem });
                } else {
                    let arguments = raw_argument.unwrap_or_default().to_owned();
                    let text = if arguments.is_empty() {
                        "/refine".to_owned()
                    } else {
                        format!("/refine {arguments}")
                    };
                    self.continuations = 0;
                    self.fresh_run(Instant::now(), Some(text.clone()));
                    self.service.submit(SubmitRequest {
                        input_id: InputId::generate(),
                        text: text.clone(),
                        answer_question_id: None,
                        shell_prefix: None,
                        compact_guidance: None,
                        refine: Some(super::refine::RefineOptions::parse(&arguments)),
                    });
                    self.push_history(&mut effects, HistoryItem::User { text });
                }
            }
            "/trust" => {
                if self.phase.has_active_run() {
                    self.push_history(&mut effects, HistoryItem::Notice {
                        message: "cannot change project trust while a run is active".to_owned(),
                    });
                } else if argument == Some("yes") {
                    match self.service.trust_project() {
                        Ok(message) => self.push_history(&mut effects, HistoryItem::Notice { message }),
                        Err(message) => self.push_history(&mut effects, HistoryItem::Error { message }),
                    }
                } else {
                    self.push_history(&mut effects, HistoryItem::Notice {
                        message: "review this project before trusting its config; repeat `/trust yes` to add its canonical root to your user trust list".to_owned(),
                    });
                }
            }
            "/init" => {
                self.reference(
                    "/init",
                    vec![
                        "# AGENTS.md".to_owned(),
                        String::new(),
                        "## Project context".to_owned(),
                        "Describe the languages, build commands, and important directories.".to_owned(),
                        String::new(),
                        "## Working rules".to_owned(),
                        "Read relevant files before editing. Keep changes focused and run the required checks.".to_owned(),
                        "Ask before actions that require approval; project instructions cannot grant tool permissions.".to_owned(),
                        String::new(),
                        "This is a static sample. Writing AGENTS.md from /init is unavailable until G04.".to_owned(),
                    ],
                    &mut effects,
                );
            }
            "/hotkeys" => {
                self.reference("/hotkeys", view::hotkey_lines(), &mut effects);
            }
            "/system-prompt" => {
                self.reference("/system-prompt", self.service.system_prompt(), &mut effects);
            }
            "/more" => {
                // The whole point is to read what the viewport clipped, so the panel
                // opens at the TOP: a reader who has to scroll before seeing the
                // beginning is exactly the problem this command exists to fix.
                self.reference("/more", self.recall_lines(), &mut effects);
            }
            "/login" => {
                self.credential_scope(super::credentials::Scope::Main);
                self.login_command(argument, &mut effects);
            }
            "/logout" => {
                self.credential_scope(super::credentials::Scope::Main);
                self.logout_command(argument, &mut effects);
            }
            // The delegated children's own logins: a child uses its own for a
            // provider when there is one, and the main one otherwise.
            "/subagent-login" => {
                self.credential_scope(super::credentials::Scope::Subagent);
                self.login_command(argument, &mut effects);
            }
            "/subagent-logout" => {
                self.credential_scope(super::credentials::Scope::Subagent);
                self.logout_command(argument, &mut effects);
                self.credential_scope(super::credentials::Scope::Main);
            }
            "/new" => {
                // A new conversation never abandons a running one: the run is
                // settled first, exactly like Ctrl-C.
                if self.phase.has_active_run() {
                    self.push_history(
                        &mut effects,
                        HistoryItem::Notice {
                            message: "cannot start a new conversation while a run is active; press Ctrl-C to cancel it first"
                                .to_owned(),
                        },
                    );
                } else if let Err(error) = self.service.resume(None) {
                    return vec![
                        Effect::History(HistoryItem::Error { message: error }),
                        Effect::Redraw,
                    ];
                } else {
                    // A new conversation starts without the old one's goal, and
                    // with nothing in its context.
                    self.header.retain(|line| !line.starts_with("Context:"));
                    self.goal = None;
                    self.service.set_goal(None);
                    self.session_candidates.clear();
                    self.editor.close_picker();
                    // `/clear` is prime-agent's alias for `/new`; it also clears the
                    // viewport, and the earlier output stays in the scrollback.
                    let message = if typed == "/clear" {
                        effects.push(Effect::ClearViewport);
                        "started a new session; earlier output remains in scrollback"
                    } else {
                        "starting a fresh conversation; the earlier chain is no longer continued"
                    };
                    self.push_history(
                        &mut effects,
                        HistoryItem::Notice {
                            message: message.to_owned(),
                        },
                    );
                    // prime-agent's `/new [prompt]`: the new conversation starts
                    // with that prompt.
                    if let Some(prompt) = raw_argument.map(str::trim).filter(|prompt| !prompt.is_empty()) {
                        effects.extend(self.dispatch(prompt.to_owned(), false));
                    }
                }
            }
            "/model" => {
                // The backend label names the configured model or states that setup
                // is required; it never claims a model that was not resolved. The
                // provider facts follow it, so a surprising answer can be diagnosed
                // without leaving the app.
                if let Some(direction @ ("next" | "prev")) = argument {
                    self.cycle_model(direction == "next", &mut effects);
                } else if let Some(model) = argument {
                    // Allowed while the agent works, as prime-agent's model
                    // selector is: the running turn switches at its next call.
                    match self.service.set_model(model) {
                        Ok(message) => self.push_history(&mut effects, HistoryItem::Notice {
                            message: if self.phase.has_active_run() {
                                format!("{message}; the running turn switches at its next step")
                            } else {
                                message
                            },
                        }),
                        Err(message) => self.push_history(&mut effects, HistoryItem::Error { message }),
                    }
                    self.refresh_status();
                    self.refresh_banner(&mut effects);
                } else {
                    let label = self.service.label();
                    let mut lines = vec![format!("backend: {label}")];
                    lines.extend(self.service.provider_diagnostics());
                    self.reference("/model", lines, &mut effects);
                    // prime-agent's bare `/model` opens the model selector: the
                    // catalog menu, filtered as the user types.
                    self.editor.open_argument_menu("/model");
                }
            }
            "/resume" if self.phase.has_active_run() => {
                self.push_history(
                    &mut effects,
                    HistoryItem::Notice {
                        message:
                            "cannot change or list sessions while a run is active; cancel it first"
                                .to_owned(),
                    },
                );
            }
            // prime-agent's bare `/resume` opens the agents view, from a
            // session the background worker hosts.
            "/resume" if argument.is_none() && self.detachable => {
                return vec![Effect::Exit(super::agents_view::AGENTS_VIEW_EXIT)];
            }
            "/resume" => match argument {
                None => {
                    self.service.list_sessions();
                    self.push_history(
                        &mut effects,
                        HistoryItem::Notice {
                            message: "looking for persisted sessions in this project...".to_owned(),
                        },
                    );
                }
                Some(selector) => {
                    let chosen = match selector.parse::<usize>() {
                        Ok(index) => index
                            .checked_sub(1)
                            .and_then(|index| self.session_candidates.get(index))
                            .map(|candidate| candidate.session_id.clone()),
                        Err(_) => self
                            .session_candidates
                            .iter()
                            .find(|candidate| candidate.session_id == selector)
                            .map(|candidate| candidate.session_id.clone()),
                    };
                    match chosen {
                        Some(session_id) => {
                            effects.extend(self.continue_session(&session_id));
                        }
                        None => {
                            self.push_history(
                                &mut effects,
                                HistoryItem::Notice {
                                    message: "that session is not in the last list; run /resume to list this project's sessions"
                                        .to_owned(),
                                },
                            );
                        }
                    }
                }
            },
            other => {
                match self.service.expand_prompt_command(
                    other.trim_start_matches('/'),
                    raw_argument.unwrap_or_default(),
                ) {
                    Ok(Some(prompt)) => return self.submit_prompt_text(prompt, effects),
                    Ok(None) => self.push_history(
                        &mut effects,
                        HistoryItem::Notice {
                            message: match super::commands::closest(other) {
                                Some(near) => format!("unknown command {other}; did you mean {near}?"),
                                None => format!(
                                    "unknown command {other}; /help lists built-in commands and configured prompt commands"
                                ),
                            },
                        },
                    ),
                    Err(message) => self.push_history(&mut effects, HistoryItem::Error { message }),
                }
            }
        }
        effects.push(Effect::Redraw);
        effects
    }

    fn submit_prompt_text(&mut self, text: String, effects: Vec<Effect>) -> Vec<Effect> {
        let shown = text.clone();
        self.submit_prompt_as(text, shown, effects)
    }

    /// `/skill:<name>` written inside a message, not at its start: the user
    /// asked for the agent to read that skill (measured: "... sử dụng
    /// /skill:swarm-planner" reached the model as plain text, and the model,
    /// which is not shown user-only skills, answered that no such skill
    /// exists). Each named skill is expanded as prime-agent expands a leading
    /// one, and the whole message follows as the request. `None` when the
    /// message names no skill this session has.
    fn expand_inline_skills(&self, text: &str) -> Option<String> {
        let mut names = Vec::new();
        for word in text.split_whitespace() {
            let Some(name) = word.strip_prefix("/skill:") else {
                continue;
            };
            let name = name.trim_end_matches(|character: char| {
                !(character.is_ascii_alphanumeric() || character == '-' || character == '_')
            });
            if !name.is_empty() && !names.contains(&name) {
                names.push(name);
            }
        }
        let blocks = names
            .into_iter()
            .filter_map(|name| self.service.expand_skill(name, "").ok())
            .collect::<Vec<_>>();
        if blocks.is_empty() {
            return None;
        }
        Some(format!("{}\n\n{}", blocks.join("\n\n"), text.trim()))
    }

    /// Send `text` to the model while the transcript shows `shown`.
    fn submit_prompt_as(
        &mut self,
        text: String,
        shown: String,
        mut effects: Vec<Effect>,
    ) -> Vec<Effect> {
        if self.phase.has_active_run() {
            self.push_history(
                &mut effects,
                HistoryItem::Error {
                    message: "cannot start a prompt command while a run is active".to_owned(),
                },
            );
            effects.push(Effect::Redraw);
            return effects;
        }
        if let Some(problem) = self.service.provider_problem() {
            self.push_history(&mut effects, HistoryItem::Error { message: problem });
            effects.push(Effect::Redraw);
            return effects;
        }
        let text = self.attach_pending_shell_outputs(text);
        self.continuations = 0;
        self.fresh_run(Instant::now(), Some(text.clone()));
        self.service.submit(SubmitRequest {
            input_id: InputId::generate(),
            text: text.clone(),
            answer_question_id: None,
            shell_prefix: None,
            compact_guidance: None,
            refine: None,
        });
        self.push_history(&mut effects, HistoryItem::User { text: shown });
        effects.push(Effect::Redraw);
        effects
    }

    /// Deliver reference output: as an overlay in the TUI, as history in plain
    /// mode.
    /// A reference panel of structured rows: the TUI draws them richly, the plain
    /// renderer prints their text.
    fn reference_rows(
        &mut self,
        title: &str,
        rows: Vec<super::events::RefLine>,
        effects: &mut Vec<Effect>,
    ) {
        if self.plain {
            for row in &rows {
                self.push_history(effects, HistoryItem::Message { text: row.plain() });
            }
            return;
        }
        self.editor.open_rich_overlay(title, rows);
    }

    fn reference(&mut self, title: &str, lines: Vec<String>, effects: &mut Vec<Effect>) {
        if self.plain {
            for text in lines {
                self.push_history(effects, HistoryItem::Message { text });
            }
            return;
        }
        self.editor.open_overlay(title, lines);
    }

    /// Esc left the masked key prompt: say so, since the prompt looks like a
    /// normal one again (measured: with no word, a user took it for stuck and
    /// typed into it), and leave the subagent login scope.
    fn cancel_key(&mut self) -> Vec<Effect> {
        let Some(provider) = self.login_provider.take() else {
            return vec![Effect::Redraw];
        };
        self.credential_scope(super::credentials::Scope::Main);
        let name = super::providers::provider(&provider)
            .map_or(provider.as_str(), |entry| entry.name)
            .to_owned();
        let mut effects = Vec::new();
        self.push_history(
            &mut effects,
            HistoryItem::Notice {
                message: format!("API key entry for {name} canceled; nothing was saved."),
            },
        );
        effects.push(Effect::Redraw);
        effects
    }

    /// Save an API key entered in the app, then make it effective at once.
    ///
    /// The value is written to the credential file and nowhere else: it is not
    /// pushed to history, not rendered, and not sent anywhere. The gate clears in
    /// the same step, so the next message is dispatched for real instead of being
    /// refused — a saved key that still needed a restart would be a trap.
    fn save_key(&mut self, value: &str) -> Vec<Effect> {
        let Some(provider) = self.login_provider.take() else {
            return vec![Effect::Redraw];
        };
        if value.trim().is_empty() {
            return vec![
                Effect::History(HistoryItem::Notice {
                    message: "no key was entered; nothing was saved".to_owned(),
                }),
                Effect::Redraw,
            ];
        }
        let credential = super::credentials::Credential::api_key(value.trim());
        let source = match self.service.save_credential(&provider, &credential) {
            Ok(source) => source,
            Err(message) => {
                self.editor.cancel_secret();
                return vec![
                    Effect::History(HistoryItem::Error { message }),
                    Effect::Redraw,
                ];
            }
        };
        if provider == super::web::SERPER_CREDENTIAL {
            let mut effects = Vec::new();
            self.push_history(
                &mut effects,
                HistoryItem::Notice {
                    message: format!(
                        "Saved the Serper key for web search to {}; web_search uses Google from the next turn. The value is never shown, logged or kept in history.",
                        source.describe()
                    ),
                },
            );
            self.refresh_menu();
            effects.push(Effect::Redraw);
            return effects;
        }
        if self.login_scope == super::credentials::Scope::Subagent {
            self.credential_scope(super::credentials::Scope::Main);
            let name = super::providers::provider(&provider)
                .map_or(provider.as_str(), |entry| entry.name)
                .to_owned();
            let mut effects = Vec::new();
            self.push_history(
                &mut effects,
                HistoryItem::Notice {
                    message: format!(
                        "Saved API key for {name} for subagents. Credentials saved to {}; the main model's login is unchanged, and the value is never shown, logged or kept in history.",
                        source.describe()
                    ),
                },
            );
            self.refresh_menu();
            effects.push(Effect::Redraw);
            return effects;
        }
        match self.activate_credential(source.clone()) {
            Ok(()) => {
                let mut effects = Vec::new();
                let name = super::providers::provider(&provider)
                    .map_or(provider.as_str(), |entry| entry.name);
                self.push_history(
                    &mut effects,
                    HistoryItem::Notice {
                        message: format!(
                            "Saved API key for {name}. Credentials saved to {}; the value is never shown, logged or kept in history.",
                            source.describe()
                        ),
                    },
                );
                self.after_login(&provider, &mut effects);
                effects.push(Effect::Redraw);
                effects
            }
            Err(error) => {
                let mut effects = Vec::new();
                self.push_history(
                    &mut effects,
                    HistoryItem::Error {
                        message: format!(
                            "the key was saved, but this session cannot use it yet: {error}"
                        ),
                    },
                );
                effects.push(Effect::Redraw);
                effects
            }
        }
    }

    /// The argument menus of the subagent commands: the models, levels and
    /// logins the delegated children can be given.
    fn refresh_subagent_menus(&mut self) {
        // A child runs on a catalog model it has a login for - its own or the
        // main one - or on this agent's.
        let mut subagent_models = vec![(
            "inherit".to_owned(),
            "children run on this agent's model".to_owned(),
        )];
        subagent_models.extend(self.service.subagent_model_options());
        let mut subagent_levels = vec![(
            "inherit".to_owned(),
            "children run at this agent's level".to_owned(),
        )];
        subagent_levels.extend(harness_providers::ThinkingLevel::ALL.iter().map(|level| {
            (
                level.as_str().to_owned(),
                format!("thinking {}", level.as_str()),
            )
        }));
        self.editor
            .set_argument_options("/subagent-effort", subagent_levels);
        let own = self.service.stored_subagent_credentials();
        self.editor.set_argument_options(
            "/subagent-login",
            super::providers::PROVIDERS
                .iter()
                .map(|entry| {
                    let state = if own.iter().any(|(id, _)| id == entry.id) {
                        " · logged in for subagents"
                    } else {
                        ""
                    };
                    (entry.id.to_owned(), format!("{}{state}", entry.name))
                })
                .collect(),
        );
        self.editor.set_argument_options(
            "/subagent-logout",
            own.iter()
                .map(|(id, kind)| {
                    let name =
                        super::providers::provider(id).map_or(id.as_str(), |entry| entry.name);
                    (id.clone(), format!("{name} · {kind}"))
                })
                .collect(),
        );
        self.editor
            .set_argument_options("/subagent-model", subagent_models);
    }

    /// Whose login the credential commands act on, here and in the service.
    fn credential_scope(&mut self, scope: super::credentials::Scope) {
        self.login_scope = scope;
        self.service.set_credential_scope(scope);
    }

    /// Refresh every controller view of provider readiness from one saved source.
    /// Phase, setup hint and header are refreshed together so they cannot disagree.
    fn activate_credential(&mut self, source: CredentialSource) -> Result<(), String> {
        let context = self
            .context
            .credential_saved(source)
            .map_err(|error| error.to_string())?;
        self.setup_required = context.setup_required;
        self.setup_hint = context.setup_hint();
        self.header = context.header_lines();
        self.header
            .push(format!("Service: {}", self.service.label()));
        self.context = context;
        if matches!(self.phase, AppPhase::SetupRequired | AppPhase::Booting) {
            self.phase = AppPhase::Ready;
        }
        Ok(())
    }

    /// prime-agent's `/login`: pick a provider, then type its API key into the
    /// masked prompt, or sign in in the browser.
    #[allow(clippy::too_many_lines)] // the login menu: providers, then Serper
    fn login_command(&mut self, argument: Option<&str>, effects: &mut Vec<Effect>) {
        if self.phase.has_active_run() {
            self.push_history(
                effects,
                HistoryItem::Notice {
                    message: "cannot log in while a run is active; cancel it first".to_owned(),
                },
            );
            return;
        }
        let for_subagents = self.login_scope == super::credentials::Scope::Subagent;
        // prime-agent's "Serper (web search)" entry: the key web_search and
        // the websearch skill read, kept in the credential file.
        if argument == Some(super::web::SERPER_CREDENTIAL) && !for_subagents {
            self.login_provider = Some(super::web::SERPER_CREDENTIAL.to_owned());
            self.editor.begin_secret_entry();
            self.push_history(
                effects,
                HistoryItem::Notice {
                    message: "Enter the Serper API key for web search (a free key from https://serper.dev): it is masked, never kept in history, and saved to the credential file. Esc cancels.".to_owned(),
                },
            );
            return;
        }
        let Some(provider) = argument.and_then(super::providers::provider) else {
            let (stored, command) = if for_subagents {
                (
                    self.service.stored_subagent_credentials(),
                    "/subagent-login",
                )
            } else {
                (self.service.stored_credentials(), "/login")
            };
            let mut lines = vec![format!("{command} <provider>:")];
            if for_subagents {
                lines.push(
                    "  the delegated children's own logins; a child without one uses the main model's"
                        .to_owned(),
                );
            }
            for entry in &super::providers::PROVIDERS {
                let state = stored
                    .iter()
                    .find(|(id, _)| id == entry.id)
                    .map_or(String::new(), |(_, kind)| format!(" · logged in ({kind})"));
                let how = match entry.login {
                    super::providers::Login::ApiKey => "API key",
                    super::providers::Login::OAuth => "browser sign-in",
                };
                lines.push(format!("  {:<14}{} · {how}{state}", entry.id, entry.name));
            }
            if !for_subagents {
                let state = if stored
                    .iter()
                    .any(|(id, _)| id == super::web::SERPER_CREDENTIAL)
                {
                    " · logged in (api_key)"
                } else {
                    ""
                };
                lines.push(format!(
                    "  {:<14}Serper (web search) · API key{state}",
                    super::web::SERPER_CREDENTIAL
                ));
            }
            self.reference(command, lines, effects);
            return;
        };
        match provider.login {
            super::providers::Login::ApiKey => {
                self.login_provider = Some(provider.id.to_owned());
                self.editor.begin_secret_entry();
                self.push_history(
                    effects,
                    HistoryItem::Notice {
                        message: format!(
                            "Enter API key for {}{}: it is masked, never kept in history, and saved to the credential file. Esc cancels.",
                            provider.name,
                            if for_subagents { " (subagents)" } else { "" }
                        ),
                    },
                );
            }
            super::providers::Login::OAuth => match self.service.begin_sign_in(provider.id) {
                Ok(url) => {
                    self.signing_in = Some(provider.id.to_owned());
                    // The URL is long and wraps; copying it is what makes it usable
                    // when the browser did not open.
                    let copied = if self.plain {
                        ""
                    } else {
                        effects.push(Effect::Copy(url.clone()));
                        " (copied to the clipboard)"
                    };
                    self.push_history(
                        effects,
                        HistoryItem::Notice {
                            message: format!(
                                "Complete the {} sign-in in your browser. If it did not open, visit{copied}:\n{url}\nIf the browser is on another machine, paste the final redirect URL here. Esc cancels.",
                                provider.name
                            ),
                        },
                    );
                }
                Err(message) => self.push_history(effects, HistoryItem::Error { message }),
            },
        }
    }

    /// prime-agent's `/logout`: remove a saved credential; environment variables
    /// are left alone.
    fn logout_command(&mut self, argument: Option<&str>, effects: &mut Vec<Effect>) {
        let for_subagents = self.login_scope == super::credentials::Scope::Subagent;
        let stored = if for_subagents {
            self.service.stored_subagent_credentials()
        } else {
            self.service.stored_credentials()
        };
        let Some(provider) = argument else {
            let message = if stored.is_empty() {
                "No stored credentials to remove; environment variables are unchanged.".to_owned()
            } else {
                format!(
                    "{} <provider>: stored credentials for {}",
                    if for_subagents {
                        "/subagent-logout"
                    } else {
                        "/logout"
                    },
                    stored
                        .iter()
                        .map(|(id, kind)| format!("{id} ({kind})"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            self.push_history(effects, HistoryItem::Notice { message });
            return;
        };
        let name = super::providers::provider(provider).map_or(provider, |entry| entry.name);
        match self.service.remove_credential(provider) {
            Ok(true) => {
                self.push_history(
                    effects,
                    HistoryItem::Notice {
                        message: if for_subagents {
                            format!(
                                "Removed the subagents' own credential for {name}; they use the main model's login for it now."
                            )
                        } else {
                            format!(
                                "Removed stored credential for {name}. Environment variables and config files are unchanged."
                            )
                        },
                    },
                );
                if !for_subagents {
                    self.after_logout(effects);
                }
            }
            Ok(false) => self.push_history(
                effects,
                HistoryItem::Notice {
                    message: format!("No stored credential for {name}."),
                },
            ),
            Err(message) => self.push_history(effects, HistoryItem::Error { message }),
        }
        self.refresh_menu();
    }

    /// The model and thinking level the status line names; they change with
    /// `/model`, `/effort`, `/login` and `/logout`, so they are read again then.
    /// The tier mark of the status line and `/tier`'s argument menu, as
    /// prime-agent lists the tiers the model takes with the current one marked.
    fn refresh_tier(&mut self) {
        let (current, available) = self.service.service_tier();
        self.tier_label = super::service_tier::status_label(Some(&current));
        self.editor.set_argument_options(
            "/tier",
            available
                .into_iter()
                .map(|tier| {
                    let marker = if tier == current { " (current)" } else { "" };
                    (
                        tier.to_owned(),
                        format!("{}{marker}", super::service_tier::description(tier)),
                    )
                })
                .collect(),
        );
    }

    /// `/tier <tier>` and `/fast`: set the tier and report it.
    fn apply_tier(&mut self, tier: &str, fast: bool, effects: &mut Vec<Effect>) {
        match self.service.set_service_tier(tier) {
            Ok(message) => {
                let message = if fast {
                    format!(
                        "Fast mode: {}",
                        if tier == "priority" { "on" } else { "off" }
                    )
                } else {
                    message
                };
                self.push_history(effects, HistoryItem::Notice { message });
            }
            Err(message) => self.push_history(effects, HistoryItem::Error { message }),
        }
        self.refresh_tier();
    }

    fn refresh_status(&mut self) {
        let label = format!("Service: {}", self.service.label());
        match self
            .header
            .iter_mut()
            .find(|line| line.starts_with("Service: "))
        {
            Some(line) => *line = label,
            None => self.header.push(label),
        }
        self.thinking_label = self.service.thinking_level();
        self.refresh_tier();
        // prime-agent rewrites `/effort`'s hint to the levels the model offers,
        // each under the name its provider uses; the one in force is marked.
        let levels = self.service.thinking_levels();
        if !levels.is_empty() {
            let current = self.thinking_label.clone();
            self.editor.set_argument_options(
                "/effort",
                levels
                    .into_iter()
                    .map(|level| {
                        let marker = if current.as_deref() == Some(level.as_str()) {
                            "current".to_owned()
                        } else {
                            String::new()
                        };
                        (level, marker)
                    })
                    .collect(),
            );
        }
    }

    /// After a logout: when the model in use has lost its credential, move to a
    /// provider that still has one, or return to setup so the status says so
    /// instead of failing on the next message.
    fn after_logout(&mut self, effects: &mut Vec<Effect>) {
        let Some(problem) = self.service.provider_problem() else {
            return;
        };
        let fallback = self
            .service
            .stored_credentials()
            .into_iter()
            .find_map(|(id, _)| super::providers::provider(&id));
        if let Some(entry) = fallback {
            match self
                .service
                .set_model(&format!("{}/{}", entry.id, entry.default_model))
            {
                Ok(message) => self.push_history(effects, HistoryItem::Notice { message }),
                Err(message) => self.push_history(effects, HistoryItem::Error { message }),
            }
        } else {
            self.context = self.context.credential_removed(problem);
            self.setup_required = true;
            self.setup_hint = self.context.setup_hint();
            if !self.phase.has_active_run() {
                self.phase = AppPhase::SetupRequired;
            }
        }
        self.header = self.context.header_lines();
        self.header
            .push(format!("Service: {}", self.service.label()));
    }

    /// After a login, as prime-agent's `prepareForModelSelectionAfterLogin`: the
    /// app is ready, the menus know the new models, and when the model in use
    /// belongs to another provider that has no credential, the new provider's
    /// default model is selected. The model menu opens on the provider's models.
    fn after_login(&mut self, provider: &str, effects: &mut Vec<Effect>) {
        let current_needs_login = self.service.provider_id().is_some_and(|id| {
            id != provider
                && !self
                    .service
                    .stored_credentials()
                    .iter()
                    .any(|(stored, _)| *stored == id)
        });
        if current_needs_login && let Some(entry) = super::providers::provider(provider) {
            match self
                .service
                .set_model(&format!("{provider}/{}", entry.default_model))
            {
                Ok(message) => self.push_history(effects, HistoryItem::Notice { message }),
                Err(message) => self.push_history(effects, HistoryItem::Error { message }),
            }
        }
        // The provider's own model list, for releases newer than the catalog; it
        // is read in the background and joins the menu on the next refresh.
        super::providers::refresh_listed_models_for_logins(
            &super::paths::LaunchEnvironment::capture(),
            &self.context.paths.data_dir,
        );
        self.refresh_menu();
        if !self.plain {
            let _ = self
                .editor
                .handle(Key::Paste(format!("/model {provider}/")));
        }
    }

    /// Continue from one session id, reporting the outcome like the pre-T02 code.
    fn continue_session(&mut self, session_id: &str) -> Vec<Effect> {
        let mut effects = Vec::new();
        if let Err(error) = self.service.resume(Some(session_id.to_owned())) {
            return vec![
                Effect::History(HistoryItem::Error { message: error }),
                Effect::Redraw,
            ];
        }
        self.push_history(
            &mut effects,
            HistoryItem::Notice {
                message: format!("continuing from session {}", view::short_id(session_id)),
            },
        );
        effects.push(Effect::Redraw);
        effects
    }

    fn start_host_action(&mut self, text: String, effects: &mut Vec<Effect>) {
        if self.phase.has_active_run() {
            self.push_history(
                effects,
                HistoryItem::Notice {
                    message: "cannot start a session action while a run is active".to_owned(),
                },
            );
            return;
        }
        self.continuations = 0;
        self.fresh_run(Instant::now(), Some(text.clone()));
        self.service.submit(SubmitRequest {
            input_id: InputId::generate(),
            text: text.clone(),
            answer_question_id: None,
            shell_prefix: None,
            compact_guidance: None,
            refine: None,
        });
        self.push_history(effects, HistoryItem::User { text });
    }

    /// The session the picker currently highlights.
    fn selected_candidate(&self) -> Option<String> {
        let picker = self.editor.picker()?;
        let selected = picker.selected();
        self.session_candidates
            .get(selected)
            .map(|candidate| candidate.session_id.clone())
    }

    /// Commit the reasoning gathered so far as one row.
    fn flush_thinking(&mut self, effects: &mut Vec<Effect>) {
        let text = std::mem::take(&mut self.pending_thinking);
        if !text.trim().is_empty() {
            effects.push(Effect::Thinking(text));
        }
    }

    fn flush_stream(&mut self, effects: &mut Vec<Effect>) {
        self.flush_thinking(effects);
        self.flush_text(effects);
    }

    /// Commit the answer text gathered so far.
    fn flush_text(&mut self, effects: &mut Vec<Effect>) {
        if self.pending_text.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.pending_text);
        self.pending_newlines = 0;
        // One effect, two renderers: plain mode appends the text exactly as it
        // arrives (that is what makes the transcript byte-identical), and the TUI
        // commits it to the scrollback through the history renderer.
        effects.push(Effect::Stream(text.clone()));
        self.last_answer.push_str(&text);
        self.transcript.push(text.clone());
        self.remember(text.split('\n').map(str::to_owned).collect());
    }

    /// Keep a bounded live block without rescanning it for every token.
    ///
    /// Width-dependent wrapping stays in the renderer. Here we commit only
    /// complete logical lines, leaving at most eight in the viewport; no partial
    /// line can jump into scrollback while the model is still writing it. The
    /// block is also capped by bytes, so a model that never emits a newline
    /// cannot grow the live view (and its per-frame clone) without bound.
    fn flush_stream_overflow(&mut self, effects: &mut Vec<Effect>) {
        const LIVE_LINES: usize = 8;
        const LIVE_BYTES: usize = 16 * 1024;
        let line_count = self.pending_newlines.saturating_add(1);
        let line_cut = if line_count > LIVE_LINES {
            let overflow = line_count - LIVE_LINES;
            self.pending_text
                .match_indices('\n')
                .nth(overflow - 1)
                .map(|(index, _)| index + 1)
        } else {
            None
        };
        let byte_cut = (self.pending_text.len() > LIVE_BYTES).then(|| {
            let mut end = LIVE_BYTES;
            while end > 0 && !self.pending_text.is_char_boundary(end) {
                end -= 1;
            }
            end
        });
        let cut = match (line_cut, byte_cut) {
            (Some(line), Some(byte)) => line.min(byte),
            (Some(line), None) => line,
            (None, Some(byte)) => byte,
            (None, None) => return,
        };
        if cut == 0 {
            return;
        }
        let tail = self.pending_text.split_off(cut);
        let committed = std::mem::replace(&mut self.pending_text, tail);
        // The committed prefix may hold newlines of its own; recount from what is
        // left (bounded by the byte cap) instead of guessing.
        self.pending_newlines = self.pending_text.matches('\n').count();
        effects.push(Effect::Stream(committed.clone()));
        self.transcript.push(committed.clone());
        self.remember(committed.split('\n').map(str::to_owned).collect());
    }

    fn push_history(&mut self, effects: &mut Vec<Effect>, item: HistoryItem) {
        let lines = view::plain_lines(&item);
        self.transcript.extend(lines.clone());
        if !self.plain
            && matches!(
                item,
                HistoryItem::Tool { .. } | HistoryItem::ToolOutput { .. }
            )
        {
            self.keep_recalled(Recalled::Tool(item.clone()));
        } else {
            self.remember(lines);
        }
        effects.push(Effect::History(item));
    }

    /// The console width the TUI draws at; `/more` fits its expanded blocks to it.
    pub fn set_columns(&mut self, columns: u16) {
        self.columns = columns;
    }

    /// Take what the clipboard holds and make it part of the message.
    ///
    /// A bitmap first: a screenshot has no text form, so the app reads it itself, writes it
    /// next to the store and names the file in the composer, and from there the ordinary
    /// message scan attaches it. Then text: the clipboard of someone who copied a file in
    /// Explorer, or dragged one into the terminal, holds that file's path, and pasting the
    /// path is exactly what the message scan needs in order to read the file. Pasting
    /// **nothing** is the one case that has to say so: a key that does nothing reads as a
    /// broken key.
    ///
    /// Deliberately not pasted here: a clipboard holding a wall of unrelated text. That is
    /// what the terminal's own paste is for, and a stray Ctrl-V must not turn a draft into a
    /// document.
    fn paste_image(&mut self, effects: &mut Vec<Effect>) {
        // Files copied in the file manager first: each path goes into the message,
        // and the message scan attaches it (an image is shown, text is read).
        match attachments::clipboard_files() {
            Ok(files) if !files.is_empty() => {
                let text = files
                    .iter()
                    .map(|path| quote_for_composer(&path.display().to_string()))
                    .collect::<Vec<_>>()
                    .join(" ");
                let _ = self.editor.handle(Key::Paste(format!("{text} ")));
                self.push_history(
                    effects,
                    HistoryItem::Notice {
                        message: format!(
                            "pasted {} file(s): they are read and attached when you send the message",
                            files.len()
                        ),
                    },
                );
                effects.push(Effect::Redraw);
                return;
            }
            Ok(_) => {}
            Err(reason) => {
                self.push_history(effects, HistoryItem::Error { message: reason });
                return;
            }
        }
        let png = match attachments::clipboard_png() {
            Ok(Some(png)) => png,
            Ok(None) => {
                self.paste_clipboard_text(effects);
                return;
            }
            Err(reason) => {
                self.push_history(effects, HistoryItem::Error { message: reason });
                return;
            }
        };
        let directory = self.context.paths.data_dir.join("attachments");
        let path = match attachments::save_pasted_png(&directory, &png) {
            Ok(path) => path,
            Err(reason) => {
                self.push_history(effects, HistoryItem::Error { message: reason });
                return;
            }
        };
        let quoted = format!("\"{}\" ", path.display());
        let outcome = self.editor.handle(Key::Paste(quoted));
        self.push_history(
            effects,
            HistoryItem::Notice {
                message: format!("image ready: {}", path.display()),
            },
        );
        effects.push(Effect::Redraw);
        // The editor's own outcome is deliberately ignored beyond the redraw: the text
        // was inserted by this call, so it cannot be a submit or an exit.
        let _ = outcome;
    }

    /// Paste a path the clipboard holds, or say that the clipboard has nothing to paste.
    fn paste_clipboard_text(&mut self, effects: &mut Vec<Effect>) {
        match attachments::clipboard_text() {
            Ok(Some(text)) if !text.trim().is_empty() => {
                let candidate = text.trim().to_owned();
                let quoted = quote_for_composer(&candidate);
                let outcome = self.editor.handle(Key::Paste(format!("{quoted} ")));
                self.push_history(
                    effects,
                    HistoryItem::Notice {
                        message: format!(
                            "pasted {candidate}: it is read and attached when you send the message"
                        ),
                    },
                );
                effects.push(Effect::Redraw);
                let _ = outcome;
            }
            Ok(_) => {
                self.push_history(
                    effects,
                    HistoryItem::Notice {
                        message: "the clipboard holds no image and no text; copy a screenshot or a file, or name a path in your message"
                            .to_owned(),
                    },
                );
            }
            Err(reason) => {
                self.push_history(effects, HistoryItem::Error { message: reason });
            }
        }
    }

    /// The paths one `/attach` argument names, each as the message scan would see it.
    ///
    /// `/attach` is the path that works in every terminal, including the ones that keep
    /// Ctrl-V for their own paste. Rather than keep a second attachment pipeline, it checks
    /// the path is a real file and then puts it in the composer, so the message that is
    /// submitted is the same one a typed or dragged path produces.
    fn attach_file(&mut self, argument: &str, effects: &mut Vec<Effect>) {
        let workspace = self.context.project.root.clone();
        let mut accepted: Vec<String> = Vec::new();
        for candidate in paths_touched(argument) {
            let Some(path) = attachments::resolve_path(&candidate, &workspace) else {
                self.push_history(
                    effects,
                    HistoryItem::Error {
                        message: format!("{candidate}: no such file"),
                    },
                );
                continue;
            };
            accepted.push(quote_for_composer(&path.display().to_string()));
        }
        if accepted.is_empty() {
            return;
        }
        let pasted = format!("{} ", accepted.join(" "));
        let outcome = self.editor.handle(Key::Paste(pasted));
        self.push_history(
            effects,
            HistoryItem::Notice {
                message: format!(
                    "{} file(s) named in the message: they are read and attached when you send it",
                    accepted.len()
                ),
            },
        );
        effects.push(Effect::Redraw);
        // Nothing was submitted by naming a file, so any other outcome would be a bug this
        // call is not allowed to hide.
        debug_assert!(
            !matches!(outcome, InputOutcome::Submit(_) | InputOutcome::Exit),
            "naming a file must not submit or exit"
        );
    }

    /// Keep the newest lines for `/more`.
    ///
    /// The terminal's own scrollback is where everything lives, and this app does
    /// not try to replace it. What it adds is a way to read the last answer without
    /// leaving the app, bounded so a long session cannot grow without limit.
    fn remember(&mut self, lines: Vec<String>) {
        for line in lines {
            self.keep_recalled(Recalled::Line(line));
        }
    }

    /// Keep one entry for `/more`, dropping the oldest past the bound. A tool entry
    /// counts as one, whatever its size: its text is already held by the row.
    fn keep_recalled(&mut self, entry: Recalled) {
        const RECALL_ENTRIES: usize = 500;
        self.recall.push(entry);
        if self.recall.len() > RECALL_ENTRIES {
            let excess = self.recall.len() - RECALL_ENTRIES;
            self.recall.drain(..excess);
        }
    }

    /// The text `/more` shows: the recent transcript, newest last.
    ///
    /// Tool entries follow the detail mode: collapsed and details show the
    /// transcript's `[tool]` line, and expanded draws the whole block - the full
    /// input and every output line - fitted to the panel, as ctrl+o's reprint does
    /// in the scrollback.
    fn recall_lines(&self) -> Vec<String> {
        let panel = self.columns.saturating_sub(2);
        let mut lines = Vec::new();
        for entry in &self.recall {
            match entry {
                Recalled::Line(line) => lines.push(line.clone()),
                Recalled::Tool(item) if self.detail == super::events::Detail::Expanded => {
                    let rows = super::tui::history::render(
                        item,
                        panel,
                        &super::tui::theme::Theme::plain(),
                        self.detail,
                    );
                    lines.extend(
                        super::tui::markdown::plain_text(&rows)
                            .split('\n')
                            .map(str::to_owned),
                    );
                }
                Recalled::Tool(item) => lines.extend(view::plain_lines(item)),
            }
        }
        // Text still streaming is part of the answer the user is reading.
        if !self.pending_text.is_empty() {
            lines.extend(self.pending_text.split('\n').map(str::to_owned));
        }
        if lines.is_empty() {
            lines.push("(nothing has been shown yet)".to_owned());
        }
        lines
    }

    /// Resolve the pending approval from one typed line.
    fn answer(&mut self, line: &str) -> Vec<Effect> {
        let Some(pending) = self.pending_approval.clone() else {
            // No pending request: fall through to a normal submission.
            return Vec::new();
        };
        if line.trim() == "A" {
            return self.propose_always_allow();
        }
        if pending.always_allow_confirm && line.trim().is_empty() {
            return self.confirm_always_allow();
        }
        let decision = match line.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" | "grant" | "/approve" => Some(ApprovalDecision::Granted),
            "a" | "all" | "/approve-all" => Some(ApprovalDecision::GrantForRun),
            "n" | "no" | "deny" | "/deny" => Some(ApprovalDecision::Denied),
            _ => None,
        };
        let mut effects = Vec::new();
        let Some(decision) = decision else {
            self.push_history(
                &mut effects,
                HistoryItem::Notice {
                    message: "the request is still pending: answer y to run it once, a to allow \
                              every action for this turn, or n to refuse"
                        .to_owned(),
                },
            );
            effects.push(Effect::Redraw);
            return effects;
        };
        let accepted = self.service.answer(&pending.request_id, decision);
        self.pending_approval = None;
        self.phase = AppPhase::Running;
        self.next_waiting();
        if decision == ApprovalDecision::GrantForRun {
            // The grant is a property of the run, not of this one answer, so it is
            // recorded on the port rather than carried in the decision alone.
            self.service.grant_run_approval();
            self.granted_for_run = true;
        }
        let label = match decision {
            ApprovalDecision::Granted => "granted",
            ApprovalDecision::GrantForRun => "granted (every action allowed for this turn)",
            ApprovalDecision::Denied => "denied",
        };
        if accepted {
            self.push_history(
                &mut effects,
                HistoryItem::ApprovalResolution {
                    label: label.to_owned(),
                    request_id: pending.request_id,
                },
            );
        } else {
            self.push_history(
                &mut effects,
                HistoryItem::Notice {
                    message: "[approval] that request is no longer pending (expired or already answered); the action was not executed"
                        .to_owned(),
                },
            );
        }
        effects.push(Effect::Redraw);
        effects
    }

    fn answer_mcp_elicitation(&mut self, answer: &str) -> Vec<Effect> {
        let Some(pending) = self.pending_mcp_elicitation.as_ref() else {
            return Vec::new();
        };
        match self
            .service
            .respond_mcp_elicitation(&pending.request_id, answer)
        {
            Ok(()) => {
                let pending = self
                    .pending_mcp_elicitation
                    .take()
                    .expect("pending MCP input");
                self.phase = AppPhase::Running;
                let result = if answer.trim().eq_ignore_ascii_case("decline") {
                    "declined"
                } else if answer.trim().eq_ignore_ascii_case("cancel") {
                    "canceled"
                } else {
                    "accepted"
                };
                vec![
                    Effect::History(HistoryItem::Notice {
                        message: format!("MCP elicitation from {} {result}", pending.server),
                    }),
                    Effect::Redraw,
                ]
            }
            Err(message) => vec![
                Effect::History(HistoryItem::Error { message }),
                Effect::Redraw,
            ],
        }
    }

    fn propose_always_allow(&mut self) -> Vec<Effect> {
        let Some(pending) = &mut self.pending_approval else {
            return Vec::new();
        };
        if pending.always_allow_confirm {
            return vec![Effect::Redraw];
        }
        let pattern = pending.rule_pattern.clone();
        pending.always_allow_confirm = true;
        pending.summary = with_rule_confirmation(&pending.summary, &pending.rule_pattern);
        let mut effects = Vec::new();
        if self.plain {
            self.push_history(
                &mut effects,
                HistoryItem::Notice {
                    message: format!(
                        "proposed rule {pattern} — press Enter to save and run, Esc to cancel"
                    ),
                },
            );
        }
        effects.push(Effect::Redraw);
        effects
    }

    fn confirm_always_allow(&mut self) -> Vec<Effect> {
        let Some(pending) = self.pending_approval.clone() else {
            return Vec::new();
        };
        match self
            .service
            .confirm_always_allow(&pending.request_id, &pending.rule_pattern)
        {
            Ok(message) => {
                self.pending_approval = None;
                self.phase = AppPhase::Running;
                self.next_waiting();
                vec![
                    Effect::History(HistoryItem::Notice { message }),
                    Effect::History(HistoryItem::ApprovalResolution {
                        label: "always allowed".to_owned(),
                        request_id: pending.request_id,
                    }),
                    Effect::Redraw,
                ]
            }
            Err(message) => vec![
                Effect::History(HistoryItem::Error { message }),
                Effect::Redraw,
            ],
        }
    }

    fn finish_run(&mut self) {
        self.writing_code = false;
        self.pending_approval = None;
        self.approval_queue.clear();
        self.pending_mcp_elicitation = None;
        self.open_tools.clear();
        self.tool_progress.clear();
        self.phase = if self.pending_question.is_some() {
            AppPhase::WaitingInput
        } else if self.setup_required {
            AppPhase::SetupRequired
        } else {
            AppPhase::Ready
        };
    }

    /// Close the turn-wide grant, and tell the port.
    ///
    /// Called only where the user's turn is really over: a terminal event that
    /// nothing continues, and a turn that broke. The grant was given for this turn
    /// only, and this is the path every ending goes through - so it cannot outlive
    /// the turn even when the turn ended by failing or being canceled.
    fn close_run_grant(&mut self) {
        if self.granted_for_run {
            self.granted_for_run = false;
            self.service.revoke_run_approval();
        }
    }

    fn finish_pending_exit(&mut self, effects: &mut Vec<Effect>) {
        if self.exit_after_run {
            self.exit_after_run = false;
            self.phase = AppPhase::Closed;
            effects.push(Effect::Exit(EXIT_SUCCESS));
        }
    }
}

/// prime-agent's `createAsyncBashCompletionMessage`.
fn bash_done_notice(pid: u64, command: &str, exit_code: i64) -> String {
    format!(
        "[bash-done pid:{pid} exit:{exit_code}]\n\nCommand: {}",
        serde_json::to_string(command).unwrap_or_default()
    )
}

/// A path as the composer should hold it: quoted, because a path with spaces is one token
/// and the message scan only sees one that way.
///
/// Free rather than a method because it touches no state: the paste path and `/attach` have
/// to agree on it exactly, or one of them would put a message in the composer that the other
/// cannot read back as a path.
fn quote_for_composer(path: &str) -> String {
    let already_quoted = path.len() >= 2 && path.starts_with('"') && path.ends_with('"');
    if already_quoted || !path.contains(char::is_whitespace) {
        return path.to_owned();
    }
    format!("\"{path}\"")
}

/// Collect at most 4096 Git-aware workspace entries for `@` completion.
fn file_picker_candidates(root: &std::path::Path) -> Vec<String> {
    let mut paths = Vec::new();
    for entry in ignore::WalkBuilder::new(root)
        .standard_filters(true)
        .follow_links(false)
        .build()
        .take(4096)
        .flatten()
    {
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(root) else {
            continue;
        };
        let path = relative.to_string_lossy().replace('\\', "/");
        paths.push(path);
    }
    paths.sort();
    paths
}

fn with_rule_confirmation(summary: &str, pattern: &str) -> String {
    let prompt = format!(
        "[always-allow]\nproposed rule: {pattern}\nPress Enter to save and run; Esc cancels."
    );
    if let Some((summary, diff)) = summary.split_once("\n[diff]\n") {
        format!("{summary}\n{prompt}\n[diff]\n{diff}")
    } else {
        format!("{summary}\n{prompt}")
    }
}

/// Now, as JavaScript's `toISOString` writes it, for prime-agent's gate prompt.
fn autonomous_timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn without_rule_confirmation(summary: &str) -> String {
    let Some((summary, remainder)) = summary.split_once("\n[always-allow]\n") else {
        return summary.to_owned();
    };
    remainder.split_once("\n[diff]\n").map_or_else(
        || summary.to_owned(),
        |(_, diff)| format!("{summary}\n[diff]\n{diff}"),
    )
}

fn parse_shell_prefix(text: &str) -> Result<Option<ShellPrefix>, String> {
    let text = text.trim_start();
    let (command, mode) = if let Some(command) = text.strip_prefix("!!") {
        (command, ShellPrefixMode::DisplayOnly)
    } else if let Some(command) = text.strip_prefix('!') {
        (command, ShellPrefixMode::AttachToNextMessage)
    } else {
        return Ok(None);
    };
    let command = command.trim();
    if command.is_empty() {
        return Err("usage: !<command> runs shell output into the next message; !!<command> only displays it".to_owned());
    }
    Ok(Some(ShellPrefix {
        command: command.to_owned(),
        mode,
    }))
}

/// The permission modes `/permissions` offers, with what each lets run unasked.
const PERMISSION_MODES: [(&str, &str); 3] = [
    ("ask", "prompt for each gated action"),
    (
        "auto-edit",
        "workspace reads and edits run unasked; process, shell and extension tools ask",
    ),
    (
        "full-auto",
        "nothing asks: every tool runs unasked; deny rules still apply",
    ),
];

#[cfg(test)]
mod tests {
    use super::{DEFAULT_CONTINUATIONS, EXIT_SUCCESS, Effect, InteractiveController, TurnBounds};
    use crate::interactive::bootstrap::{self, LaunchContext, LaunchRequest};
    use crate::interactive::events::{
        AppPhase, HistoryItem, Key, Modal, PauseReason, RunOutcome, SessionCandidate, SessionEvent,
        ShellPrefixMode, ToolState,
    };
    use crate::interactive::input::SLASH_COMMANDS;
    use crate::interactive::paths::{HostPlatform, LaunchEnvironment};
    use crate::interactive::service::{
        ApprovalDecision, FixtureService, SessionChannel, SessionPort, ShellPrefix, SubmitRequest,
    };
    use crate::interactive::view;
    use harness_types::InputId;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// A `/rlm-max-depth` change as the port receives it.
    type DepthChange = Option<(u32, bool)>;

    /// Test port that records what the controller admitted.
    #[derive(Clone, Default)]
    struct RecordingPort {
        submissions: Arc<Mutex<Vec<String>>>,
        shell_prefixes: Arc<Mutex<Vec<Option<ShellPrefix>>>>,
        answer_questions: Arc<Mutex<Vec<Option<String>>>>,
        cancels: Arc<Mutex<u32>>,
        answers: Arc<Mutex<Vec<(String, ApprovalDecision)>>>,
        steers: Arc<Mutex<Vec<String>>>,
        /// Refuse steering, as a service without a running inbox does.
        refuse_steer: bool,
        resumes: Arc<Mutex<Vec<Option<String>>>>,
        /// Every turn-wide grant the controller handed to the port, and every
        /// revocation, so a test can assert the grant is scoped to one turn.
        run_grants: Arc<Mutex<Vec<bool>>>,
        always_allowed: Arc<Mutex<Vec<(String, String)>>>,
        mode_updates: Arc<Mutex<Vec<String>>>,
        permission_lines: Arc<Mutex<Vec<String>>>,
        allow_root: Arc<Mutex<Option<std::path::PathBuf>>>,
        limits: TurnBounds,
        /// Every goal handed to the port; `Some("")` records a forgotten goal.
        goals: Arc<Mutex<Vec<Option<String>>>>,
        /// Heartbeats the next pump finds due.
        heartbeats_due: Arc<Mutex<Vec<crate::interactive::heartbeat::Due>>>,
        /// Child messages delivered into the running turn.
        delivered: Arc<Mutex<Vec<String>>>,
        /// Every `/agents stop` target.
        stops: Arc<Mutex<Vec<String>>>,
        /// Every `/btw` question.
        side_questions: Arc<Mutex<Vec<String>>>,
        /// Every fork, clone (`"clone"`) and switch, in order.
        branches: Arc<Mutex<Vec<String>>>,
        /// Every run of the autonomous quality gates, and how often they were stopped.
        gate_jobs: Arc<Mutex<Vec<crate::interactive::autonomous::GateJob>>>,
        gate_cancels: Arc<Mutex<u32>>,
        /// Every `/scoped-models` argument, as the port received it.
        scopes: Arc<Mutex<Vec<Option<String>>>>,
        /// Every `/rlm-max-depth` change the port received.
        depths: Arc<Mutex<Vec<DepthChange>>>,
        /// The service tier in force, as `/tier` and `/fast` set it.
        tier: Arc<Mutex<Option<String>>>,
        /// Every answer handed to a waiting child, by its question id.
        child_answers: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl SessionPort for RecordingPort {
        fn label(&self) -> String {
            "recording port".to_owned()
        }

        fn answer_child_question(&mut self, request_id: &str, answer: &str) -> bool {
            self.child_answers
                .lock()
                .expect("child answers")
                .push((request_id.to_owned(), answer.to_owned()));
            true
        }

        fn expand_skill(&self, name: &str, arguments: &str) -> Result<String, String> {
            if name != "review" {
                return Err(format!("no skill named {name}"));
            }
            Ok(format!(
                "<skill name=\"review\">\nReview it.\n</skill>{arguments}"
            ))
        }

        fn run_gates(
            &mut self,
            job: crate::interactive::autonomous::GateJob,
        ) -> Result<(), String> {
            self.gate_jobs.lock().expect("gate log").push(job);
            Ok(())
        }

        fn cancel_gates(&mut self) {
            *self.gate_cancels.lock().expect("gate cancel log") += 1;
        }

        fn scoped_models(&mut self, argument: Option<&str>) -> Result<Vec<String>, String> {
            self.scopes
                .lock()
                .expect("scope log")
                .push(argument.map(str::to_owned));
            Ok(Vec::new())
        }

        fn submit(&mut self, request: SubmitRequest) {
            let shell_prefix = request.shell_prefix.clone();
            let answer_question = request.answer_question_id.clone();
            self.submissions
                .lock()
                .expect("submission log")
                .push(request.text);
            self.shell_prefixes
                .lock()
                .expect("shell prefix log")
                .push(shell_prefix);
            self.answer_questions
                .lock()
                .expect("question answer log")
                .push(answer_question);
        }

        fn cancel(&mut self) {
            *self.cancels.lock().expect("cancel log") += 1;
        }

        fn due_heartbeats(&mut self) -> Vec<crate::interactive::heartbeat::Due> {
            std::mem::take(&mut *self.heartbeats_due.lock().expect("heartbeats"))
        }

        fn set_goal(&mut self, objective: Option<String>) {
            self.goals.lock().expect("goal log").push(objective);
        }

        fn forget_goal(&mut self) {
            self.goals
                .lock()
                .expect("goal log")
                .push(Some(String::new()));
        }

        fn steer(&mut self, text: &str) -> Result<(), String> {
            if self.refuse_steer {
                return Err("no active run inbox is available".to_owned());
            }
            self.steers.lock().expect("steer log").push(text.to_owned());
            Ok(())
        }

        fn deliver_message(&mut self, text: &str) -> Result<(), String> {
            if self.refuse_steer {
                return Err("no active run inbox is available".to_owned());
            }
            self.delivered
                .lock()
                .expect("delivery log")
                .push(text.to_owned());
            Ok(())
        }

        fn list_turns(
            &mut self,
            purpose: crate::interactive::events::TurnsPurpose,
        ) -> Result<(), String> {
            self.branches
                .lock()
                .expect("branch log")
                .push(format!("list {purpose:?}"));
            Ok(())
        }

        fn fork(&mut self, session: &str, before: bool) -> Result<(), String> {
            self.branches
                .lock()
                .expect("branch log")
                .push(format!("fork {session} before={before}"));
            Ok(())
        }

        fn clone_conversation(&mut self) -> Result<(), String> {
            self.branches
                .lock()
                .expect("branch log")
                .push("clone".to_owned());
            Ok(())
        }

        fn switch_to(&mut self, session: &str) -> Result<(), String> {
            self.branches
                .lock()
                .expect("branch log")
                .push(format!("switch {session}"));
            Ok(())
        }

        fn side_question(&mut self, question: &str) -> Result<(), String> {
            self.side_questions
                .lock()
                .expect("side question log")
                .push(question.to_owned());
            Ok(())
        }

        fn set_service_tier(&mut self, tier: &str) -> Result<String, String> {
            if !["default", "priority", "auto"].contains(&tier) {
                return Err(format!(
                    "Service tier '{tier}' is not available for the current model. Available: default, priority, auto"
                ));
            }
            *self.tier.lock().expect("tier") = Some(tier.to_owned());
            Ok(format!("Service tier: {tier}"))
        }

        fn service_tier(&self) -> (String, Vec<&'static str>) {
            (
                self.tier
                    .lock()
                    .expect("tier")
                    .clone()
                    .unwrap_or_else(|| "default".to_owned()),
                vec!["default", "priority", "auto"],
            )
        }

        fn rlm_max_depth(&mut self, change: Option<(u32, bool)>) -> Result<(), String> {
            self.depths.lock().expect("depth log").push(change);
            Ok(())
        }

        fn stop_agents(&mut self, selector: &str) -> Result<String, String> {
            self.stops
                .lock()
                .expect("stop log")
                .push(selector.to_owned());
            Ok(format!("stopping {selector}"))
        }

        fn answer(&mut self, request_id: &str, decision: ApprovalDecision) -> bool {
            self.answers
                .lock()
                .expect("answer log")
                .push((request_id.to_owned(), decision));
            true
        }

        fn grant_run_approval(&mut self) {
            self.run_grants.lock().expect("run grant log").push(true);
        }

        fn revoke_run_approval(&mut self) {
            self.run_grants.lock().expect("run grant log").push(false);
        }

        fn confirm_always_allow(
            &mut self,
            request_id: &str,
            pattern: &str,
        ) -> Result<String, String> {
            if let Some(root) = self.allow_root.lock().expect("allow root").as_ref() {
                crate::interactive::permissions::persist_allow_rule(root, pattern)
                    .map_err(|error| error.to_string())?;
            }
            self.always_allowed
                .lock()
                .expect("always-allow log")
                .push((request_id.to_owned(), pattern.to_owned()));
            Ok("permission rule saved".to_owned())
        }

        fn resume(&mut self, session_id: Option<String>) -> Result<(), String> {
            self.resumes.lock().expect("resume log").push(session_id);
            Ok(())
        }

        fn limits(&self) -> TurnBounds {
            self.limits
        }

        fn set_mode(&mut self, mode: &str) -> Result<String, String> {
            self.mode_updates
                .lock()
                .expect("mode updates")
                .push(mode.to_owned());
            Ok(format!("permission mode set to {mode} for this session"))
        }

        fn permissions_summary(&mut self) -> Vec<String> {
            self.permission_lines
                .lock()
                .expect("permission lines")
                .clone()
        }
    }

    /// Fixture home and project, never the developer profile.
    ///
    /// `configured` writes the config file and a credential, which is what makes
    /// a launch ready instead of `setup_required`; the flag is read straight from
    /// the environment pairs, so no test depends on the developer's shell.
    fn context(configured: bool) -> (tempfile::TempDir, LaunchContext) {
        let temp = tempfile::tempdir().expect("temp root");
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&home).expect("fixture home");
        std::fs::create_dir_all(&project).expect("fixture project");
        let mut pairs = vec![("HA_HOME", home.to_string_lossy().into_owned())];
        if configured {
            std::fs::write(home.join("config.toml"), "schema_version = 1\n")
                .expect("fixture config");
            pairs.push(("DEEPSEEK_API_KEY", "fixture-secret".to_owned()));
        }
        let context = bootstrap::resolve(LaunchRequest {
            cwd: None,
            caller_dir: project,
            platform: HostPlatform::current(),
            environment: LaunchEnvironment::from_pairs(pairs),
            explicit_data_dir: None,
        })
        .expect("context resolves");
        (temp, context)
    }

    /// Test port that saves a credential the way the real service does.
    ///
    /// The real path is `credentials::save` into the credential file the resolver
    /// reads, so this port calls exactly that. `HA_CREDENTIALS_DIR` is deliberately
    /// not involved: mutating the process environment is unsafe in edition 2024 and
    /// this crate forbids `unsafe`, so the file lives under the fixture's own data
    /// directory, which the test removes with the `TempDir`.
    struct SavingPort {
        recorded: RecordingPort,
        data_dir: std::path::PathBuf,
    }

    impl SessionPort for SavingPort {
        fn label(&self) -> String {
            self.recorded.label()
        }
        fn submit(&mut self, request: SubmitRequest) {
            self.recorded.submit(request);
        }
        fn cancel(&mut self) {
            self.recorded.cancel();
        }
        fn answer(&mut self, request_id: &str, decision: ApprovalDecision) -> bool {
            self.recorded.answer(request_id, decision)
        }
        fn resume(&mut self, session_id: Option<String>) -> Result<(), String> {
            self.recorded.resume(session_id)
        }
        fn limits(&self) -> TurnBounds {
            self.recorded.limits()
        }
        fn save_credential(
            &mut self,
            provider: &str,
            credential: &crate::interactive::credentials::Credential,
        ) -> Result<crate::interactive::credentials::CredentialSource, String> {
            let path = crate::interactive::credentials::resolve_file(
                &crate::interactive::paths::LaunchEnvironment::default(),
                &self.data_dir,
            );
            let protection = crate::interactive::credentials::save(&path, provider, credential)
                .expect("the fixture credential directory is writable");
            Ok(crate::interactive::credentials::CredentialSource::File { path, protection })
        }
        fn stored_credentials(&self) -> Vec<(String, &'static str)> {
            let path = crate::interactive::credentials::resolve_file(
                &crate::interactive::paths::LaunchEnvironment::default(),
                &self.data_dir,
            );
            crate::interactive::credentials::stored(&path).unwrap_or_default()
        }
        fn remove_credential(&mut self, provider: &str) -> Result<bool, String> {
            let path = crate::interactive::credentials::resolve_file(
                &crate::interactive::paths::LaunchEnvironment::default(),
                &self.data_dir,
            );
            crate::interactive::credentials::remove(&path, provider)
                .map_err(|error| error.to_string())
        }
    }

    struct Bench {
        controller: InteractiveController,
        events: tokio::sync::mpsc::UnboundedSender<SessionEvent>,
        port: RecordingPort,
        /// Kept alive for the fixture's lifetime: dropping it would delete the home and
        /// project directories out from under the controller.
        temp: tempfile::TempDir,
    }

    fn bench(configured: bool) -> Bench {
        bench_with(configured, RecordingPort::default(), true)
    }

    /// The same bench with the TUI renderer selected, so the modal key handling
    /// (immediate y/n) is exercised instead of the plain line answers.
    fn tui_bench(configured: bool) -> Bench {
        bench_with(configured, RecordingPort::default(), false)
    }

    fn bench_with(configured: bool, port: RecordingPort, plain: bool) -> Bench {
        let (temp, context) = context(configured);
        let channel = SessionChannel::new();
        let events = channel.sender();
        let controller =
            InteractiveController::new(&context, Box::new(port.clone()), channel, plain);
        Bench {
            controller,
            events,
            port,
            temp,
        }
    }

    /// A bench whose events come from the fixture service, so streaming and tool
    /// cards are produced by the same code the app uses.
    fn fixture_bench(configured: bool) -> Bench {
        let (temp, context) = context(configured);
        let channel = SessionChannel::new();
        let events = channel.sender();
        let port = RecordingPort::default();
        let controller = InteractiveController::new(
            &context,
            Box::new(FixtureService::new(events.clone())),
            channel,
            true,
        );
        Bench {
            controller,
            events,
            port,
            temp,
        }
    }

    fn submit_text(controller: &mut InteractiveController, text: &str) -> Vec<Effect> {
        type_text(controller, text);
        controller.handle_key(Key::Enter)
    }

    /// Type one line without submitting it, so a caller can inspect the prompt.
    fn type_text(controller: &mut InteractiveController, text: &str) {
        for character in text.chars() {
            let _ = controller.handle_key(Key::Char(character));
        }
    }

    /// A `/skill:<name>` inside the message reaches the model as the skill,
    /// with the whole message as the request; an unknown name stays text.
    #[test]
    fn a_skill_named_inside_a_message_is_expanded() {
        let mut harness = bench(true);
        submit_text(
            &mut harness.controller,
            "please check this using /skill:review, and /skill:nope too",
        );
        let submissions = harness
            .port
            .submissions
            .lock()
            .expect("submissions")
            .clone();
        assert_eq!(
            submissions,
            [
                "<skill name=\"review\">\nReview it.\n</skill>\n\nplease check this using /skill:review, and /skill:nope too"
            ]
        );

        let mut plain = bench(true);
        submit_text(&mut plain.controller, "no skill /skill:nope here");
        assert_eq!(
            *plain.port.submissions.lock().expect("submissions"),
            ["no skill /skill:nope here"],
            "a name no skill has is left as text"
        );
    }

    #[test]
    fn g06_enter_while_running_steers_the_running_turn() {
        let mut harness = bench(true);
        submit_text(&mut harness.controller, "first input");
        submit_text(&mut harness.controller, "and also check the tests");
        assert_eq!(
            *harness.port.submissions.lock().expect("submissions"),
            ["first input"],
            "the second message is not a second turn"
        );
        assert_eq!(
            *harness.port.steers.lock().expect("steers"),
            ["and also check the tests"],
            "it reaches the running turn, as prime-agent's Enter does"
        );
        assert!(!harness.controller.ui_state().queued_input);

        harness
            .events
            .send(SessionEvent::RunTerminal {
                outcome: RunOutcome::Done,
            })
            .expect("terminal event");
        let _ = harness.controller.pump_events();
        assert_eq!(
            *harness.port.submissions.lock().expect("submissions"),
            ["first input"],
            "nothing is sent again after the turn"
        );
    }

    /// A turn that cannot take the message yet (no inbox) keeps it for after.
    #[test]
    fn g06_enter_while_running_queues_when_the_turn_cannot_take_it() {
        let port = RecordingPort {
            refuse_steer: true,
            ..RecordingPort::default()
        };
        let mut harness = bench_with(true, port, false);
        submit_text(&mut harness.controller, "first input");
        submit_text(&mut harness.controller, "queued input");
        assert!(harness.controller.ui_state().queued_input);
        harness
            .events
            .send(SessionEvent::RunTerminal {
                outcome: RunOutcome::Done,
            })
            .expect("terminal event");
        let _ = harness.controller.pump_events();
        assert_eq!(
            *harness.port.submissions.lock().expect("submissions"),
            ["first input", "queued input"]
        );
    }

    fn refusing_bench() -> Bench {
        let port = RecordingPort {
            refuse_steer: true,
            ..RecordingPort::default()
        };
        bench_with(true, port, false)
    }

    fn done(harness: &mut Bench) {
        harness
            .events
            .send(SessionEvent::RunTerminal {
                outcome: RunOutcome::Done,
            })
            .expect("terminal event");
        let _ = harness.controller.pump_events();
    }

    /// Q05: steering the turn could not take runs first, then follow-ups, one
    /// turn each.
    #[test]
    fn q05_follow_up_waits_for_the_turn_and_runs_after_steers() {
        let mut harness = refusing_bench();
        submit_text(&mut harness.controller, "first");
        submit_text(&mut harness.controller, "/queue then summarize");
        submit_text(&mut harness.controller, "steer me");
        submit_text(&mut harness.controller, "/followup then translate");
        assert_eq!(harness.controller.ui_state().queued_count, 3);
        assert_eq!(submissions(&harness), ["first"]);
        done(&mut harness);
        assert_eq!(submissions(&harness), ["first", "steer me"]);
        done(&mut harness);
        done(&mut harness);
        assert_eq!(
            submissions(&harness),
            ["first", "steer me", "then summarize", "then translate"]
        );
        assert!(!harness.controller.ui_state().queued_input);
    }

    /// Q05: in `all` mode the lane's follow-ups go in one turn.
    #[test]
    fn q05_all_mode_batches_the_follow_ups_into_one_turn() {
        use crate::interactive::queue::{InputQueue, QueueMode};
        let mut harness = refusing_bench();
        harness.controller.queue = InputQueue::new(QueueMode::OneAtATime, QueueMode::All);
        submit_text(&mut harness.controller, "first");
        submit_text(&mut harness.controller, "/queue one");
        submit_text(&mut harness.controller, "/queue two");
        done(&mut harness);
        assert_eq!(submissions(&harness), ["first", "one\n\ntwo"]);
    }

    /// Q05: the queue survives Ctrl-C and waits for the user's next turn.
    #[test]
    fn q05_the_queue_survives_an_interrupt() {
        let mut harness = refusing_bench();
        submit_text(&mut harness.controller, "first");
        submit_text(&mut harness.controller, "/queue keep me");
        let _ = harness.controller.interrupt();
        assert!(harness.controller.ui_state().queued_input, "still queued");
        harness
            .events
            .send(SessionEvent::RunTerminal {
                outcome: RunOutcome::Canceled,
            })
            .expect("terminal event");
        let _ = harness.controller.pump_events();
        assert_eq!(submissions(&harness), ["first"], "held after the interrupt");
        submit_text(&mut harness.controller, "next");
        done(&mut harness);
        assert_eq!(submissions(&harness), ["first", "next", "keep me"]);
    }

    /// Q06: the queue is listed, edited, reordered and dropped by number.
    #[test]
    fn q06_browse_edits_and_deletes_queued_items() {
        let mut harness = refusing_bench();
        submit_text(&mut harness.controller, "first");
        for text in ["/queue a", "/queue b", "/queue c"] {
            submit_text(&mut harness.controller, text);
        }
        submit_text(&mut harness.controller, "/queue up 3");
        submit_text(&mut harness.controller, "/queue edit 1 A!");
        submit_text(&mut harness.controller, "/queue drop 2");
        assert_eq!(
            harness.controller.queue.lines(),
            ["1. Follow-up: A!", "2. Follow-up: b"]
        );
        let plain = effects_to_plain(&submit_text(&mut harness.controller, "/queue drop 9"));
        assert!(
            plain
                .iter()
                .any(|line| line.contains("no queued message 9")),
            "{plain:#?}"
        );
    }

    /// Q07: one side question at a time; the answer opens a panel and nothing is
    /// submitted to the conversation.
    #[test]
    fn q07_btw_asks_on_the_side_one_at_a_time() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        submit_text(&mut harness.controller, "/btw which file did we change?");
        let plain = effects_to_plain(&submit_text(&mut harness.controller, "/side and why?"));
        assert!(
            plain
                .iter()
                .any(|line| line.contains("Wait for the current side question")),
            "{plain:#?}"
        );
        assert_eq!(
            harness
                .port
                .side_questions
                .lock()
                .expect("questions")
                .as_slice(),
            ["which file did we change?".to_owned()]
        );
        harness
            .events
            .send(SessionEvent::SideAnswer {
                question: "which file did we change?".to_owned(),
                answer: Ok("src/parser.rs".to_owned()),
            })
            .expect("answer");
        let _ = harness.controller.pump_events();
        assert!(
            submissions(&harness).is_empty(),
            "nothing enters the conversation"
        );
        submit_text(&mut harness.controller, "/btw and why?");
        assert_eq!(
            harness.port.side_questions.lock().expect("questions").len(),
            2
        );
    }

    fn turns_listed(harness: &mut Bench, purpose: crate::interactive::events::TurnsPurpose) {
        harness
            .events
            .send(SessionEvent::TurnsListed {
                purpose,
                turns: vec![
                    ("s1".to_owned(), "first ask".to_owned()),
                    ("s2".to_owned(), "second ask".to_owned()),
                    ("s3".to_owned(), "third ask".to_owned()),
                ],
            })
            .expect("turns");
        let _ = harness.controller.pump_events();
    }

    /// prime-agent's fork selector: in the TUI the messages open as a menu on
    /// the latest one, and Enter forks before the highlighted message.
    #[test]
    fn the_fork_selector_picks_with_the_arrows() {
        use crate::interactive::events::TurnsPurpose;
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        submit_text(&mut harness.controller, "/fork");
        turns_listed(&mut harness, TurnsPurpose::Fork);
        assert!(matches!(
            harness.controller.ui_state().modal,
            Some(Modal::TurnPicker {
                selected: 2,
                fork: true,
                ..
            })
        ));
        let _ = harness.controller.handle_key(Key::Up);
        let _ = harness.controller.handle_key(Key::Enter);
        assert_eq!(
            harness.port.branches.lock().expect("branches").last(),
            Some(&"fork s2 before=true".to_owned())
        );
        assert!(harness.controller.ui_state().modal.is_none());
    }

    /// Q08: /fork lists the user's messages, then forks before the chosen one and
    /// puts it back in the editor.
    #[test]
    fn q08_fork_starts_before_the_chosen_turn() {
        use crate::interactive::events::TurnsPurpose;
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        submit_text(&mut harness.controller, "/fork");
        turns_listed(&mut harness, TurnsPurpose::Fork);
        submit_text(&mut harness.controller, "/fork 2");
        assert_eq!(
            harness.port.branches.lock().expect("branches").as_slice(),
            ["list Fork".to_owned(), "fork s2 before=true".to_owned()]
        );
        assert_eq!(harness.controller.editor.text(), "second ask");
        harness.controller.editor.clear();
        let plain = effects_to_plain(&submit_text(&mut harness.controller, "/fork 9"));
        assert!(
            plain.iter().any(|line| line.contains("no turn 9")),
            "{plain:#?}"
        );
    }

    /// Q08: /clone forks after the last turn; /tree switches, or says it is
    /// already there; none of them runs while the agent works.
    #[test]
    fn q08_clone_and_tree_move_the_conversation() {
        use crate::interactive::events::TurnsPurpose;
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let plain = effects_to_plain(&submit_text(&mut harness.controller, "/clone"));
        assert!(
            plain
                .iter()
                .any(|line| line.contains("Cloned to new session")),
            "{plain:#?}"
        );
        submit_text(&mut harness.controller, "/tree");
        turns_listed(&mut harness, TurnsPurpose::Tree);
        let plain = effects_to_plain(&submit_text(&mut harness.controller, "/tree 3"));
        assert!(
            plain
                .iter()
                .any(|line| line.contains("Already at this point")),
            "{plain:#?}"
        );
        submit_text(&mut harness.controller, "/tree 1");
        assert_eq!(
            harness.port.branches.lock().expect("branches").as_slice(),
            [
                "clone".to_owned(),
                "list Tree".to_owned(),
                "switch s1".to_owned()
            ]
        );
        submit_text(&mut harness.controller, "a request");
        let plain = effects_to_plain(&submit_text(&mut harness.controller, "/fork"));
        assert!(
            plain
                .iter()
                .any(|line| line.contains("waits for the active run")),
            "{plain:#?}"
        );
    }

    /// Q06: prime-agent's prompt stash, with its messages.
    #[test]
    fn q06_stash_and_restore_follow_prime_messages() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let said = |effects: Vec<Effect>| effects_to_plain(&effects).join("\n");
        assert!(said(harness.controller.handle_key(Key::Stash)).contains("No prompt to stash"));
        type_text(&mut harness.controller, "half a thought");
        assert!(said(harness.controller.handle_key(Key::Stash)).contains("Stashed prompt"));
        assert_eq!(harness.controller.editor.text(), "");
        type_text(&mut harness.controller, "another");
        assert!(
            said(harness.controller.handle_key(Key::Stash))
                .contains("Prompt stash already has a draft")
        );
        harness.controller.editor.clear();
        assert!(
            said(harness.controller.handle_key(Key::Stash)).contains("Restored stashed prompt")
        );
        assert_eq!(harness.controller.editor.text(), "half a thought");
    }

    #[test]
    fn g06_steer_reaches_the_driver_mid_run() {
        let mut harness = bench(true);
        submit_text(&mut harness.controller, "start");
        submit_text(&mut harness.controller, "/steer keep the current change");
        assert_eq!(
            *harness.port.steers.lock().expect("steers"),
            ["keep the current change"]
        );
        assert_eq!(harness.controller.phase(), AppPhase::Running);
    }

    #[test]
    fn g06_esc_cancels_a_running_turn_but_not_an_approval() {
        let mut harness = bench(true);
        submit_text(&mut harness.controller, "start");
        let _ = harness.controller.handle_key(Key::Esc);
        assert_eq!(*harness.port.cancels.lock().expect("cancels"), 1);
        assert_eq!(harness.controller.phase(), AppPhase::Canceling);

        let mut approval = tui_bench(true);
        let _ = submit_text(&mut approval.controller, "start");
        approval
            .events
            .send(approval_event("g06-approval"))
            .expect("approval event");
        let _ = approval.controller.pump_events();
        let _ = approval.controller.handle_key(Key::Esc);
        assert_eq!(approval.controller.phase(), AppPhase::WaitingApproval);
        assert_eq!(*approval.port.cancels.lock().expect("cancels"), 0);
        assert!(approval.controller.ui_state().modal.is_some());
    }

    /// Escape on a panel opened during a run closes the panel; the run goes on.
    #[test]
    fn esc_closes_a_panel_opened_during_a_run_without_canceling_it() {
        let mut harness = tui_bench(true);
        let _ = submit_text(&mut harness.controller, "start");
        assert_eq!(harness.controller.phase(), AppPhase::Running);
        let _ = submit_text(&mut harness.controller, "/agents");
        assert!(
            harness.controller.ui_state().modal.is_some(),
            "the panel is open"
        );
        let _ = harness.controller.handle_key(Key::Esc);
        assert!(
            harness.controller.ui_state().modal.is_none(),
            "the panel closed"
        );
        assert_eq!(*harness.port.cancels.lock().expect("cancels"), 0);
        assert_eq!(harness.controller.phase(), AppPhase::Running);
        // With nothing open, Escape interrupts as before.
        let _ = harness.controller.handle_key(Key::Esc);
        assert_eq!(*harness.port.cancels.lock().expect("cancels"), 1);
    }

    #[test]
    fn g06_at_picker_inserts_a_workspace_relative_path() {
        let mut harness = tui_bench(true);
        let file = harness.temp.path().join("project").join("notes.txt");
        std::fs::write(&file, "attachment").expect("fixture file");
        let _ = harness.controller.handle_key(Key::Char('@'));
        assert!(
            matches!(
                harness.controller.ui_state().modal,
                Some(Modal::FilePicker { items, .. }) if items.contains(&"notes.txt".to_owned())
            ),
            "@ opens a file picker"
        );
        let _ = harness.controller.handle_key(Key::Enter);
        assert!(harness.controller.prompt().ends_with("notes.txt"));
    }

    #[test]
    fn g06_bang_prefix_routes_through_shell_and_only_single_bang_attaches_output() {
        let mut harness = bench(true);
        let _ = submit_text(&mut harness.controller, "! echo ATTACHED_OUTPUT");
        assert_eq!(
            harness
                .port
                .shell_prefixes
                .lock()
                .expect("shell prefix log")[0],
            Some(ShellPrefix {
                command: "echo ATTACHED_OUTPUT".to_owned(),
                mode: ShellPrefixMode::AttachToNextMessage,
            })
        );
        harness
            .events
            .send(SessionEvent::ShellPrefixCompleted {
                command: "echo ATTACHED_OUTPUT".to_owned(),
                output: "ATTACHED_OUTPUT".to_owned(),
                attach_to_next_message: true,
            })
            .expect("shell result");
        harness
            .events
            .send(SessionEvent::RunTerminal {
                outcome: RunOutcome::Done,
            })
            .expect("shell terminal");
        let _ = harness.controller.pump_events();
        let _ = submit_text(&mut harness.controller, "summarize this output");
        let sent = harness.port.submissions.lock().expect("submissions");
        assert!(sent[1].contains("[shell] echo ATTACHED_OUTPUT\nATTACHED_OUTPUT"));
        drop(sent);
        let _ = harness.controller.handle_key(Key::Esc);
        harness
            .events
            .send(SessionEvent::RunTerminal {
                outcome: RunOutcome::Done,
            })
            .expect("chat terminal");
        let _ = harness.controller.pump_events();

        let _ = submit_text(&mut harness.controller, "!! echo LOCAL_ONLY");
        assert_eq!(
            harness
                .port
                .shell_prefixes
                .lock()
                .expect("shell prefix log")[2],
            Some(ShellPrefix {
                command: "echo LOCAL_ONLY".to_owned(),
                mode: ShellPrefixMode::DisplayOnly,
            })
        );
        harness
            .events
            .send(SessionEvent::ShellPrefixCompleted {
                command: "echo LOCAL_ONLY".to_owned(),
                output: "LOCAL_ONLY".to_owned(),
                attach_to_next_message: false,
            })
            .expect("display-only shell result");
        harness
            .events
            .send(SessionEvent::RunTerminal {
                outcome: RunOutcome::Done,
            })
            .expect("display-only terminal");
        let _ = harness.controller.pump_events();
        let _ = submit_text(&mut harness.controller, "do not include local output");
        let sent = harness.port.submissions.lock().expect("submissions");
        assert_eq!(sent[3], "do not include local output");
        assert!(
            harness
                .controller
                .transcript()
                .iter()
                .any(|line| line.contains("[shell] echo LOCAL_ONLY\nLOCAL_ONLY"))
        );
    }

    #[test]
    fn g06_question_panel_numbered_answer_resumes_the_same_question() {
        let mut harness = tui_bench(true);
        let _ = submit_text(&mut harness.controller, "choose a color");
        harness
            .events
            .send(SessionEvent::QuestionRequired {
                question_id: "question-g06".to_owned(),
                prompt: "Which color should I use?".to_owned(),
                options: vec!["blue".to_owned(), "green".to_owned()],
            })
            .expect("question event");
        harness
            .events
            .send(SessionEvent::RunTerminal {
                outcome: RunOutcome::WaitingInput {
                    question_id: Some("question-g06".to_owned()),
                },
            })
            .expect("question turn ends");
        let _ = harness.controller.pump_events();
        assert_eq!(harness.controller.phase(), AppPhase::WaitingInput);
        assert!(matches!(
            harness.controller.ui_state().modal,
            Some(Modal::Question { ref prompt, ref options, .. })
                if prompt == "Which color should I use?" && options.len() == 2
        ));
        let _ = harness.controller.handle_key(Key::Char('2'));
        assert_eq!(
            harness
                .port
                .submissions
                .lock()
                .expect("submissions")
                .as_slice(),
            ["choose a color", "green"]
        );
        assert_eq!(
            harness
                .port
                .answer_questions
                .lock()
                .expect("question answer log")
                .as_slice(),
            [None, Some("question-g06".to_owned())]
        );
    }

    /// A question's options are a menu: the arrows move the highlight (and wrap)
    /// and Enter answers with it. Once text is typed the composer owns the keys -
    /// a digit is text, not an option - and answering clears what was typed.
    #[test]
    fn a_question_is_answered_with_the_arrows_and_typing_is_text() {
        fn ask(harness: &mut Bench, id: &str, options: &[&str]) {
            harness
                .events
                .send(SessionEvent::QuestionRequired {
                    question_id: id.to_owned(),
                    prompt: "Which one?".to_owned(),
                    options: options.iter().map(|option| (*option).to_owned()).collect(),
                })
                .expect("question event");
            harness
                .events
                .send(SessionEvent::RunTerminal {
                    outcome: RunOutcome::WaitingInput {
                        question_id: Some(id.to_owned()),
                    },
                })
                .expect("question turn ends");
            let _ = harness.controller.pump_events();
        }
        fn selected(harness: &Bench) -> usize {
            match harness.controller.ui_state().modal {
                Some(Modal::Question { selected, .. }) => selected,
                other => panic!("a question panel, not {other:?}"),
            }
        }
        let options = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"];
        let mut harness = tui_bench(true);
        let _ = submit_text(&mut harness.controller, "pick one");
        ask(&mut harness, "q1", &options);
        let _ = harness.controller.handle_key(Key::Up);
        assert_eq!(
            selected(&harness),
            11,
            "up from the first wraps to the last"
        );
        for _ in 0..3 {
            let _ = harness.controller.handle_key(Key::Down);
        }
        assert_eq!(selected(&harness), 2);
        let _ = harness.controller.handle_key(Key::Enter);
        assert_eq!(
            harness.port.submissions.lock().expect("submissions").last(),
            Some(&"c".to_owned())
        );

        ask(&mut harness, "q2", &options);
        for key in [
            Key::Char('x'),
            Key::Char(' '),
            Key::Char('1'),
            Key::Char('0'),
        ] {
            let _ = harness.controller.handle_key(key);
        }
        assert_eq!(
            harness.controller.ui_state().buffer,
            "x 10",
            "digits are text once typing"
        );
        let _ = harness.controller.handle_key(Key::Down);
        assert_eq!(selected(&harness), 0, "the arrows belong to the draft");
        let _ = harness.controller.handle_key(Key::Enter);
        assert_eq!(
            harness.port.submissions.lock().expect("submissions").last(),
            Some(&"x 10".to_owned())
        );
        assert_eq!(harness.controller.ui_state().buffer, "");
        assert!(harness.controller.ui_state().modal.is_none());
    }

    /// prime-agent's background completion: an idle app starts a turn with the
    /// `[bash-done ...]` notice; one that cannot take it keeps it, and the
    /// kernel reading the result first withdraws it.
    #[test]
    fn a_background_command_completion_is_delivered_or_withdrawn() {
        let mut harness = tui_bench(true);
        harness
            .events
            .send(SessionEvent::BashCompleted {
                pid: 7,
                command: "npm run build".to_owned(),
                exit_code: 0,
            })
            .expect("completion");
        let _ = harness.controller.pump_events();
        assert_eq!(
            harness.port.submissions.lock().expect("submissions").last(),
            Some(
                &"[bash-done pid:7 exit:0]

Command: \"npm run build\""
                    .to_owned()
            )
        );
        harness.controller.notices_held = true;
        harness.controller.phase = AppPhase::Ready;
        for event in [
            SessionEvent::BashCompleted {
                pid: 8,
                command: "npm test".to_owned(),
                exit_code: 1,
            },
            SessionEvent::BashConsumed {
                pid: 9,
                command: "npm test".to_owned(),
            },
        ] {
            harness.events.send(event).expect("event");
        }
        let _ = harness.controller.pump_events();
        assert_eq!(
            harness.controller.pending_notices.len(),
            1,
            "another pid is kept"
        );
        harness
            .events
            .send(SessionEvent::BashConsumed {
                pid: 8,
                command: "npm test".to_owned(),
            })
            .expect("consumed");
        let _ = harness.controller.pump_events();
        assert!(harness.controller.pending_notices.is_empty(), "withdrawn");
    }

    fn saved_credential_path(context: &LaunchContext) -> std::path::PathBuf {
        crate::interactive::credentials::resolve_file(
            &crate::interactive::paths::LaunchEnvironment::default(),
            &context.paths.data_dir,
        )
    }

    /// K01: the whole `/login` API-key chain, driven by keys rather than by calling the
    /// handler directly — editor, controller, the port that saves the file, and the
    /// bootstrap refresh that clears the setup gate.
    ///
    /// Split in two because the chain has two halves: what the user sees while
    /// typing, and what the app does once the key is submitted.
    #[test]
    fn k01_key_entry_masks_saves_clears_the_gate_and_admits_the_next_message() {
        let (temp, context) = context(false);
        assert!(context.setup_required, "the fixture starts unconfigured");
        let channel = SessionChannel::new();
        let events = channel.sender();
        let recorded = RecordingPort::default();
        let port = SavingPort {
            recorded: recorded.clone(),
            data_dir: context.paths.data_dir.clone(),
        };
        let mut controller = InteractiveController::new(&context, Box::new(port), channel, true);
        controller.boot_lines();

        assert_masked_entry_hides_the_key(&mut controller);
        assert_submitting_the_key_opens_the_gate(&mut controller, &context, &recorded);
        assert_escape_abandons_entry_without_overwriting(&mut controller, &events, &context);
        drop(temp);
    }

    /// `/login deepseek` opens masked entry, and the prompt shows the mask, not the key.
    fn assert_masked_entry_hides_the_key(controller: &mut InteractiveController) {
        submit_text(controller, "/login deepseek");
        assert!(
            controller.editor.secret_entry(),
            "an API-key provider starts secret entry"
        );
        assert!(
            controller.ui_state().setup_required,
            "nothing is configured until the key is submitted"
        );
        type_text(controller, "sk-controller-fixture");
        let masked = controller.prompt();
        assert!(
            !masked.contains("sk-controller-fixture"),
            "the prompt must mask the key: {masked}"
        );
        assert!(masked.contains('\u{2022}'), "the mask is visible: {masked}");
    }

    /// Submitting saves the file, clears the gate, and admits the next message.
    fn assert_submitting_the_key_opens_the_gate(
        controller: &mut InteractiveController,
        context: &LaunchContext,
        recorded: &RecordingPort,
    ) {
        let _ = controller.handle_key(Key::Enter);
        let path = saved_credential_path(context);
        assert!(path.is_file(), "the key was written to {}", path.display());
        assert!(
            !controller.ui_state().setup_required,
            "a saved key clears the setup gate"
        );
        assert_eq!(
            controller.phase(),
            AppPhase::Ready,
            "the app is ready, not still in setup"
        );
        assert!(
            controller
                .transcript()
                .join("\n")
                .contains("Saved API key for DeepSeek"),
            "the user is told what happened: {:?}",
            controller.transcript()
        );
        assert_eq!(
            recorded.submissions.lock().expect("submission log").len(),
            0,
            "entering a key is not a request"
        );
        assert!(
            !controller
                .transcript()
                .join("\n")
                .contains("sk-controller-fixture"),
            "the transcript must never carry the key"
        );

        submit_text(controller, "explain the parser");
        assert_eq!(
            recorded
                .submissions
                .lock()
                .expect("submission log")
                .as_slice(),
            ["explain the parser".to_owned()],
            "the gate is open for the next message"
        );
    }

    /// `/login` refuses while a run is active, and Esc then abandons entry safely.
    fn assert_escape_abandons_entry_without_overwriting(
        controller: &mut InteractiveController,
        events: &tokio::sync::mpsc::UnboundedSender<SessionEvent>,
        context: &LaunchContext,
    ) {
        submit_text(controller, "/login deepseek");
        assert!(
            !controller.editor.secret_entry(),
            "/login must not open secret entry while a run is active"
        );
        assert!(
            controller
                .transcript()
                .join("\n")
                .contains("cannot log in while a run is active"),
            "{:?}",
            controller.transcript()
        );
        let _ = events.send(SessionEvent::RunTerminal {
            outcome: RunOutcome::Done,
        });
        let _ = controller.pump_events();

        submit_text(controller, "/login deepseek");
        assert!(controller.editor.secret_entry());
        type_text(controller, "sk-abandoned");
        let effects = controller.handle_key(Key::Esc);
        assert!(!controller.editor.secret_entry(), "Esc cancels entry");
        assert!(
            effects.iter().any(|effect| matches!(
                effect,
                Effect::History(HistoryItem::Notice { message }) if message.contains("canceled; nothing was saved")
            )),
            "Esc says the entry ended: {effects:?}"
        );
        assert_eq!(
            controller.prompt(),
            "> ",
            "the prompt is a normal one again"
        );
        let path = saved_credential_path(context);
        assert_eq!(
            crate::interactive::credentials::load(&path, "deepseek")
                .expect("credential file")
                .map(|credential| credential.secret().to_owned())
                .as_deref(),
            Some("sk-controller-fixture"),
            "Esc must not overwrite the stored key"
        );
    }

    /// A bare `/login` lists the providers; each provider keeps its own key.
    #[test]
    fn k01_login_lists_providers_and_saves_each_key_under_its_provider() {
        let (_temp, context) = context(false);
        let channel = SessionChannel::new();
        let port = SavingPort {
            recorded: RecordingPort::default(),
            data_dir: context.paths.data_dir.clone(),
        };
        let mut controller = InteractiveController::new(&context, Box::new(port), channel, true);
        controller.boot_lines();

        let listing = effects_to_plain(&submit_text(&mut controller, "/login")).join("\n");
        for name in [
            "OpenCode Zen",
            "OpenCode Go",
            "DeepSeek",
            "OpenAI",
            "Anthropic",
        ] {
            assert!(listing.contains(name), "{listing}");
        }

        submit_text(&mut controller, "/login opencode");
        assert!(controller.editor.secret_entry());
        type_text(&mut controller, "sk-open");
        let _ = controller.handle_key(Key::Enter);
        let path = saved_credential_path(&context);
        let key = |provider: &str| {
            crate::interactive::credentials::load(&path, provider)
                .expect("credential file")
                .map(|credential| credential.secret().to_owned())
        };
        assert_eq!(key("opencode").as_deref(), Some("sk-open"));
        assert_eq!(key("deepseek"), None, "another provider is untouched");

        let _ = controller.handle_key(Key::Up);
        assert!(
            !controller.prompt().contains("sk-open"),
            "Up must never recall a key"
        );
        let _ = controller.handle_key(Key::EraseToLineStart);

        let removed =
            effects_to_plain(&submit_text(&mut controller, "/logout opencode")).join("\n");
        assert!(
            removed.contains("Removed stored credential for OpenCode Zen"),
            "{removed}"
        );
        assert_eq!(key("opencode"), None);
    }

    /// The plain lines of one effect batch: exactly what the plain renderer
    /// writes, and exactly what the pre-T02 `WriteLine`/`WritePartial` pair wrote.
    ///
    /// Stream chunks are appended with no separator, because that is what
    /// `Effect::WritePartial` did; a history line that follows starts on its own
    /// row.
    fn effects_to_plain(effects: &[Effect]) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let mut streaming = false;
        for effect in effects {
            match effect {
                Effect::History(item) => {
                    streaming = false;
                    lines.extend(view::plain_lines(item));
                }
                Effect::Stream(text) => {
                    if streaming {
                        if let Some(last) = lines.last_mut() {
                            last.push_str(text);
                        }
                    } else {
                        lines.push(text.clone());
                    }
                    streaming = true;
                }
                Effect::Thinking(_)
                | Effect::Reprint(_)
                | Effect::Restore(_)
                | Effect::Banner(_)
                | Effect::Copy(_)
                | Effect::Bell
                | Effect::Redraw
                | Effect::ClearViewport
                | Effect::Fullscreen(_)
                | Effect::EditExternally(_)
                | Effect::Exit(_) => {}
            }
        }
        lines
    }

    fn history_items(effects: &[Effect]) -> Vec<HistoryItem> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::History(item) => Some(item.clone()),
                _ => None,
            })
            .collect()
    }

    fn input_id(text: &str) -> InputId {
        InputId::parse(text).expect("canonical input id")
    }

    /// Two children ask at once: the second question waits until the first is
    /// answered, and each answer goes back to the child that asked it.
    #[test]
    fn questions_from_several_children_are_asked_one_after_another() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "plan with two children");
        for (id, prompt, options) in [
            ("q-color", "[child color] Which color?", ["red", "blue"]),
            ("q-size", "[child size] Which size?", ["S", "L"]),
        ] {
            harness
                .events
                .send(SessionEvent::ChildQuestionRequired {
                    request_id: id.to_owned(),
                    prompt: prompt.to_owned(),
                    options: options.map(str::to_owned).to_vec(),
                })
                .expect("question");
        }
        let _ = harness.controller.pump_events();
        let shown = |harness: &Bench| match harness.controller.ui_state().modal {
            Some(Modal::Question { prompt, .. }) => prompt,
            other => panic!("a question panel: {other:?}"),
        };
        assert_eq!(shown(&harness), "[child color] Which color?");
        let _ = harness.controller.handle_key(Key::Char('2'));
        assert_eq!(
            shown(&harness),
            "[child size] Which size?",
            "the next one follows"
        );
        let _ = harness.controller.handle_key(Key::Char('1'));
        assert!(harness.controller.ui_state().modal.is_none());
        assert_eq!(
            *harness.port.child_answers.lock().expect("child answers"),
            [
                ("q-color".to_owned(), "blue".to_owned()),
                ("q-size".to_owned(), "S".to_owned())
            ]
        );
        assert_eq!(
            harness.controller.ui_state().phase,
            AppPhase::Running,
            "the turn goes on"
        );
    }

    /// Two gated actions at once: the second panel waits for the first.
    #[test]
    fn approvals_from_several_agents_are_asked_one_after_another() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "edit two files");
        harness.events.send(approval_event("a-1")).expect("first");
        harness.events.send(approval_event("a-2")).expect("second");
        let _ = harness.controller.pump_events();
        let open = |harness: &Bench| match harness.controller.ui_state().modal {
            Some(Modal::Approval { request_id, .. }) => request_id,
            other => panic!("an approval panel: {other:?}"),
        };
        assert_eq!(open(&harness), "a-1");
        let _ = harness.controller.handle_key(Key::Char('y'));
        assert_eq!(
            open(&harness),
            "a-2",
            "the second waited, it was not dropped"
        );
    }

    /// A gated **write**, which is the case that keeps its panel.
    fn approval_event(request_id: &str) -> SessionEvent {
        SessionEvent::ApprovalRequired {
            request_id: request_id.to_owned(),
            action: "apply_patch".to_owned(),
            summary: "path=src/parser.rs".to_owned(),
            rule_pattern: "apply_patch(src/parser.rs)".to_owned(),
            workspace: "C:/work/project".to_owned(),
            scope: "once".to_owned(),
            expires_at: Instant::now() + Duration::from_mins(5),
            read_only: false,
        }
    }

    /// The same event for a read, which is the one that offers the wider grant.
    fn read_approval_event(request_id: &str) -> SessionEvent {
        SessionEvent::ApprovalRequired {
            request_id: request_id.to_owned(),
            action: "ListFiles".to_owned(),
            summary: "list .".to_owned(),
            rule_pattern: "list_files(.)".to_owned(),
            workspace: "C:/work/project".to_owned(),
            scope: "once".to_owned(),
            expires_at: Instant::now() + Duration::from_mins(5),
            read_only: true,
        }
    }

    #[test]
    fn h03_one_admission_per_message_and_a_second_steers_the_running_run() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let effects = submit_text(&mut harness.controller, "first request");
        assert_eq!(
            effects_to_plain(&effects),
            vec!["> first request".to_owned()],
            "the echo is the only line the submission itself adds: {effects:#?}"
        );
        assert_eq!(harness.controller.phase(), AppPhase::Running);
        assert_eq!(
            harness
                .port
                .submissions
                .lock()
                .expect("submissions")
                .as_slice(),
            ["first request"]
        );

        let effects = submit_text(&mut harness.controller, "second request");
        let plain = effects_to_plain(&effects).join("\n");
        assert!(plain.contains("> second request"), "{plain}");
        assert!(plain.contains("sent to the running agent"), "{plain}");
        assert_eq!(
            harness.port.submissions.lock().expect("submissions").len(),
            1,
            "the second message joins the running turn instead of opening another"
        );
        assert_eq!(
            *harness.port.steers.lock().expect("steers"),
            ["second request"]
        );
        assert!(!harness.controller.ui_state().queued_input);
        assert_eq!(harness.controller.phase(), AppPhase::Running);
    }

    #[test]
    fn h03_fixture_run_streams_text_before_tools_and_returns_to_ready() {
        let mut harness = fixture_bench(true);
        let _ = harness.controller.boot_lines();
        let mut plain = effects_to_plain(&submit_text(&mut harness.controller, "sửa lỗi parser"));
        plain.extend(effects_to_plain(&harness.controller.pump_events()));
        let joined = plain.join("\n");
        assert!(joined.contains("> sửa lỗi parser"), "{joined}");
        assert!(joined.contains("[run] accepted "), "{joined}");
        assert!(
            joined.contains("fixture answer for: sửa lỗi parser"),
            "{joined}"
        );
        assert!(
            joined.contains("[tool] search_text pattern=parser"),
            "{joined}"
        );
        assert!(joined.contains("[tool] search_text failed"), "{joined}");
        assert!(
            joined.contains("[tool] apply_patch path=src/parser.rs"),
            "{joined}"
        );
        assert!(joined.contains("[tool] apply_patch ok"), "{joined}");
        assert!(joined.contains("[run] done"), "{joined}");
        assert_eq!(harness.controller.phase(), AppPhase::Ready);

        // Order: the streamed answer is committed before the first tool row,
        // exactly like flush_stream did before T02.
        let answer = joined.find("fixture answer").expect("answer present");
        let tool = joined.find("[tool] search_text").expect("tool present");
        assert!(
            answer < tool,
            "text flushes before the tool line:\n{joined}"
        );
    }

    #[test]
    fn t02_step_started_updates_the_counter() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        assert_eq!(harness.controller.ui_state().steps, 0);
        harness
            .events
            .send(SessionEvent::StepStarted { step: 3 })
            .expect("step event");
        let _ = harness.controller.pump_events();
        assert_eq!(
            harness.controller.ui_state().steps,
            3,
            "the status bar counts the step the driver reported"
        );
        assert_eq!(
            harness.controller.ui_state().max_steps,
            TurnBounds::default().max_steps
        );
    }

    #[test]
    fn t02_expired_approval_closes_the_modal_and_never_grants() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        harness
            .events
            .send(approval_event("req-7"))
            .expect("approval event");
        let _ = harness.controller.pump_events();
        assert_eq!(harness.controller.phase(), AppPhase::WaitingApproval);
        assert!(
            harness.controller.ui_state().modal.is_some(),
            "the panel is open"
        );

        harness
            .events
            .send(SessionEvent::ApprovalExpired {
                request_id: "req-7".to_owned(),
            })
            .expect("expiry event");
        let effects = harness.controller.pump_events();
        let items = history_items(&effects);
        assert!(
            items.iter().any(|item| matches!(
                item,
                HistoryItem::ApprovalResolution { label, request_id }
                    if label == "expired" && request_id == "req-7"
            )),
            "the expiry is recorded: {items:#?}"
        );
        assert_eq!(
            harness.controller.phase(),
            AppPhase::Running,
            "the run continues; the gate refused the action"
        );
        assert!(
            harness.controller.ui_state().modal.is_none(),
            "the panel closes on expiry instead of waiting for the run to end"
        );
        assert!(
            harness.port.answers.lock().expect("answers").is_empty(),
            "an expiry never answers the request"
        );
    }

    #[test]
    fn t02_a_settled_tool_card_reports_the_measured_duration() {
        let mut harness = bench_with(true, RecordingPort::default(), true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "fix it");
        harness
            .events
            .send(SessionEvent::ToolStarted {
                name: "apply_patch".to_owned(),
                call_id: String::new(),
                summary: "path=a.rs".to_owned(),
                input: String::new(),
            })
            .expect("started");
        harness
            .events
            .send(SessionEvent::ToolSettled {
                name: "apply_patch".to_owned(),
                call_id: String::new(),
                ok: false,
                elapsed: Duration::from_millis(3100),
                detail: String::new(),
            })
            .expect("settled");
        let effects = harness.controller.pump_events();
        let items = history_items(&effects);
        let settled = items.iter().rev().find_map(|item| match item {
            HistoryItem::Tool { name, state, .. } if name == "apply_patch" => Some(state.clone()),
            _ => None,
        });
        assert_eq!(
            settled,
            Some(ToolState::Failed {
                elapsed: Duration::from_millis(3100),
                detail: String::new(),
            }),
            "the card carries the duration the service measured"
        );
        assert!(
            harness.controller.ui_state().open_tools.is_empty(),
            "the open card closes when it settles"
        );
    }

    #[test]
    fn t02_the_run_row_carries_steps_tools_and_elapsed() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "go");
        harness
            .events
            .send(SessionEvent::StepStarted { step: 2 })
            .expect("step");
        harness
            .events
            .send(SessionEvent::ToolStarted {
                name: "read_file".to_owned(),
                call_id: String::new(),
                summary: "path=a.rs".to_owned(),
                input: String::new(),
            })
            .expect("tool");
        harness
            .events
            .send(SessionEvent::ToolSettled {
                name: "read_file".to_owned(),
                call_id: String::new(),
                ok: true,
                elapsed: Duration::from_millis(12),
                detail: String::new(),
            })
            .expect("settled");
        harness
            .events
            .send(SessionEvent::RunTerminal {
                outcome: RunOutcome::Done,
            })
            .expect("terminal");
        let effects = harness.controller.pump_events();
        let (steps, tool_calls) = history_items(&effects)
            .into_iter()
            .find_map(|item| match item {
                HistoryItem::Run {
                    steps, tool_calls, ..
                } => Some((steps, tool_calls)),
                _ => None,
            })
            .expect("a run row");
        assert_eq!(steps, 2, "steps came from StepStarted");
        assert_eq!(tool_calls, 1, "tool calls are counted from the tool events");
        assert_eq!(harness.controller.phase(), AppPhase::Ready);
    }

    #[test]
    fn h03_ctrl_c_cancels_a_run_and_clears_an_idle_prompt() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "work");
        let effects = harness.controller.handle_key(Key::Interrupt);
        assert_eq!(harness.controller.phase(), AppPhase::Canceling);
        assert_eq!(*harness.port.cancels.lock().expect("cancels"), 1);
        assert!(
            effects_to_plain(&effects)
                .join("\n")
                .contains("^C canceling"),
            "{effects:#?}"
        );

        harness
            .events
            .send(SessionEvent::RunTerminal {
                outcome: RunOutcome::Canceled,
            })
            .expect("terminal");
        let _ = harness.controller.pump_events();
        assert_eq!(harness.controller.phase(), AppPhase::Ready);

        let _ = submit_text(&mut harness.controller, "typo");
        // Settle the run first: Ctrl-C while a run is active cancels the run, it
        // does not clear the buffer.
        harness
            .events
            .send(SessionEvent::RunTerminal {
                outcome: RunOutcome::Canceled,
            })
            .expect("terminal");
        let _ = harness.controller.pump_events();
        assert_eq!(harness.controller.phase(), AppPhase::Ready);
        let effects = harness.controller.handle_key(Key::Interrupt);
        assert_eq!(
            harness.controller.prompt(),
            "> ",
            "an idle Ctrl-C clears the buffer"
        );
        assert_eq!(effects_to_plain(&effects), vec![String::new()]);
    }

    #[test]
    fn h05_a_gated_action_is_rendered_and_answered_by_the_user() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        harness
            .events
            .send(approval_event("req-1"))
            .expect("approval");
        let effects = harness.controller.pump_events();
        assert_eq!(
            effects_to_plain(&effects),
            view::approval_lines(
                "apply_patch",
                "path=src/parser.rs",
                "C:/work/project",
                "once",
                "req-1"
            ),
            "plain mode prints the same four lines as before T02"
        );
        assert_eq!(harness.controller.phase(), AppPhase::WaitingApproval);

        let effects = submit_text(&mut harness.controller, "y");
        assert_eq!(
            harness.port.answers.lock().expect("answers").as_slice(),
            [("req-1".to_owned(), ApprovalDecision::Granted)]
        );
        assert!(
            effects_to_plain(&effects)
                .join("\n")
                .contains("[approval] granted req-1"),
            "{effects:#?}"
        );
        assert_eq!(harness.controller.phase(), AppPhase::Running);
        assert!(harness.controller.ui_state().modal.is_none());
    }

    #[test]
    fn h05_a_denial_and_an_unknown_answer_are_handled() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        harness
            .events
            .send(approval_event("req-2"))
            .expect("approval");
        let _ = harness.controller.pump_events();

        let effects = submit_text(&mut harness.controller, "maybe");
        let plain = effects_to_plain(&effects).join("\n");
        assert!(plain.contains("request is still pending"), "{plain}");
        assert_eq!(harness.controller.phase(), AppPhase::WaitingApproval);
        assert!(harness.port.answers.lock().expect("answers").is_empty());

        let effects = submit_text(&mut harness.controller, "n");
        assert_eq!(
            harness.port.answers.lock().expect("answers").as_slice(),
            [("req-2".to_owned(), ApprovalDecision::Denied)]
        );
        assert!(
            effects_to_plain(&effects)
                .join("\n")
                .contains("[approval] denied req-2"),
            "{effects:#?}"
        );
    }

    #[test]
    fn h05_resume_lists_sessions_and_selects_one_by_number() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        harness
            .events
            .send(SessionEvent::SessionsListed {
                sessions: vec![
                    SessionCandidate {
                        session_id: "session_a".to_owned(),
                        task_id: "task_1".to_owned(),
                        detail: "1 input(s), 2 event(s)".to_owned(),
                    },
                    SessionCandidate {
                        session_id: "session_b".to_owned(),
                        task_id: "task_1".to_owned(),
                        detail: "2 input(s), 4 event(s)".to_owned(),
                    },
                ],
            })
            .expect("listing");
        let listing = harness.controller.pump_events();
        let listing_plain = effects_to_plain(&listing).join("\n");
        assert!(
            listing_plain.contains("sessions in this project (2):"),
            "{listing_plain}"
        );
        assert!(listing_plain.contains("1. session_a"), "{listing_plain}");
        assert!(
            listing_plain.contains("use /resume <number> to continue one of them"),
            "plain mode still tells the user how to pick one: {listing_plain}"
        );

        let effects = submit_text(&mut harness.controller, "/resume 2");
        assert_eq!(
            harness.port.resumes.lock().expect("resumes").as_slice(),
            [Some("session_b".to_owned())]
        );
        let plain = effects_to_plain(&effects).join("\n");
        assert!(
            plain.contains("continuing from session"),
            "the resume is reported: {plain:?}"
        );
        assert!(
            plain.contains("ssion_b"),
            "and it names the chosen session: {plain:?}"
        );
    }

    #[test]
    fn a_resumed_conversation_is_shown_before_the_next_message() {
        // Measured: /resume printed "continuing from session ..." and nothing else,
        // so the user could not see what they were continuing.
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        harness
            .events
            .send(SessionEvent::ConversationRestored {
                turns: vec![
                    (
                        "mô tả memory dự án".to_owned(),
                        "Memory có hai đường cập nhật.".to_owned(),
                    ),
                    ("còn resume?".to_owned(), "Resume đọc snapshot.".to_owned()),
                ],
                omitted: 3,
                summarized: false,
            })
            .expect("restored");
        let plain = effects_to_plain(&harness.controller.pump_events()).join("\n");
        let order = [
            "3 older turn(s) are not shown",
            "mô tả memory dự án",
            "Memory có hai đường cập nhật.",
            "còn resume?",
            "Resume đọc snapshot.",
            "resumed 2 turn(s)",
        ]
        .map(|needle| {
            plain
                .find(needle)
                .unwrap_or_else(|| panic!("{needle:?} is missing: {plain}"))
        });
        assert!(
            order.windows(2).all(|pair| pair[0] < pair[1]),
            "the conversation is shown in the order it was said: {plain}"
        );
    }

    #[test]
    fn h05_an_empty_listing_and_a_notice_are_rendered_honestly() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        harness
            .events
            .send(SessionEvent::SessionsListed {
                sessions: Vec::new(),
            })
            .expect("empty listing");
        harness
            .events
            .send(SessionEvent::Notice {
                message: "hello".to_owned(),
            })
            .expect("notice");
        let effects = harness.controller.pump_events();
        let plain = effects_to_plain(&effects).join("\n");
        assert!(
            plain.contains("no persisted sessions in this project yet"),
            "{plain}"
        );
        assert!(plain.contains("[info] hello"), "{plain}");
    }

    #[test]
    fn h03_slash_commands_are_parsed_and_staged_features_stay_honest() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();

        let help = effects_to_plain(&submit_text(&mut harness.controller, "/help")).join("\n");
        assert!(help.contains("/resume"), "{help}");
        assert!(help.contains("/hotkeys"), "{help}");
        let keys = effects_to_plain(&submit_text(&mut harness.controller, "/hotkeys")).join("\n");
        assert!(keys.contains("Ctrl-D"), "{keys}");

        let status = effects_to_plain(&submit_text(&mut harness.controller, "/status")).join("\n");
        assert!(status.contains("Phase:"), "{status}");

        let model = effects_to_plain(&submit_text(&mut harness.controller, "/model")).join("\n");
        assert!(model.contains("backend: "), "{model}");

        let unknown = effects_to_plain(&submit_text(&mut harness.controller, "/nope")).join("\n");
        assert!(unknown.contains("unknown command"), "{unknown}");

        let resume =
            effects_to_plain(&submit_text(&mut harness.controller, "/resume 9")).join("\n");
        assert!(resume.contains("not in the last list"), "{resume}");

        let exit = submit_text(&mut harness.controller, "/exit");
        assert!(exit.contains(&Effect::Exit(EXIT_SUCCESS)), "{exit:#?}");
        assert_eq!(harness.controller.phase(), AppPhase::Closed);
    }

    #[test]
    fn g01_init_prints_a_static_sample_without_writing_a_file() {
        let mut harness = bench(true);
        let sample = effects_to_plain(&submit_text(&mut harness.controller, "/init")).join("\n");
        assert!(sample.contains("# AGENTS.md"), "{sample}");
        assert!(sample.contains("static sample"), "{sample}");
        assert!(
            !harness
                .temp
                .path()
                .join("project")
                .join("AGENTS.md")
                .exists(),
            "/init is transcript-only before G04"
        );
    }

    /// The measured gap this closes: typing `/` listed nothing. The user had to
    /// know the command already, and `/help` was the only way to find out - after
    /// the fact. Now the frame carries the matches and the row that the next Tab
    /// or Enter would accept, from the first slash.
    #[test]
    fn slash_the_snapshot_carries_the_menu_and_the_highlight() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();

        type_text(&mut harness.controller, "/");
        let state = harness.controller.ui_state();
        assert_eq!(
            state
                .suggestions
                .iter()
                .map(|item| item.name().to_owned())
                .collect::<Vec<_>>(),
            SLASH_COMMANDS
                .iter()
                .map(|command| command.name)
                .collect::<Vec<_>>(),
            "one slash offers every command"
        );
        assert_eq!(state.suggestion_selected, 0);
        assert_eq!(state.buffer, "/", "and the draft is untouched");

        type_text(&mut harness.controller, "resu");
        let state = harness.controller.ui_state();
        assert_eq!(
            state
                .suggestions
                .iter()
                .map(|item| item.name().to_owned())
                .collect::<Vec<_>>(),
            ["/resume"],
            "the menu narrows with every letter"
        );

        let _ = harness.controller.handle_key(Key::Down);
        assert_eq!(
            harness.controller.ui_state().suggestion_selected,
            0,
            "one match: the highlight cannot move off it"
        );
        assert_eq!(harness.controller.ui_state().buffer, "/resu");
    }

    /// Tab accepts the highlighted command even when several match - that is what
    /// the menu on screen is for. The editor alone would refuse, which is why the
    /// decision lives where the drawing happens.
    #[test]
    fn slash_tab_accepts_the_row_the_menu_has_highlighted() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        type_text(&mut harness.controller, "/");
        assert!(
            harness.controller.ui_state().suggestions.len() > 1,
            "the ambiguous case is the one under test"
        );

        let effects = harness.controller.handle_key(Key::Tab);
        assert_eq!(effects, vec![Effect::Redraw]);
        assert_eq!(
            harness.controller.ui_state().buffer,
            "/model ",
            "Tab takes the first row, and its argument follows a space"
        );

        // Move the highlight, and Tab takes that row instead.
        let _ = harness.controller.handle_key(Key::EraseToLineStart);
        type_text(&mut harness.controller, "/");
        let _ = harness.controller.handle_key(Key::Down);
        let _ = harness.controller.handle_key(Key::Down);
        let _ = harness.controller.handle_key(Key::Tab);
        assert_eq!(harness.controller.ui_state().buffer, "/export ");
    }

    #[test]
    fn inline_slash_enter_accepts_a_command_without_sending_the_sentence() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        type_text(&mut harness.controller, "hãy dùng /cop");
        assert!(
            harness
                .controller
                .ui_state()
                .suggestions
                .iter()
                .any(|item| item.name() == "/copy")
        );

        let effects = harness.controller.handle_key(Key::Enter);
        assert_eq!(effects, vec![Effect::Redraw]);
        assert_eq!(harness.controller.ui_state().buffer, "hãy dùng /copy");
        assert!(submissions(&harness).is_empty());

        let _ = harness.controller.handle_key(Key::Enter);
        assert_eq!(submissions(&harness), ["hãy dùng /copy"]);
    }

    /// Enter on the menu picks the row: a whole command runs at once, a command
    /// that takes an argument opens its argument menu, and a picked argument runs
    /// - `/thinking`, pick a level, applied (prime-agent's selectors).
    #[test]
    fn slash_enter_picks_a_row_and_runs_it_when_it_is_whole() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        type_text(&mut harness.controller, "/hel");
        let _ = harness.controller.handle_key(Key::Enter);
        assert!(
            matches!(
                harness.controller.ui_state().modal,
                Some(Modal::Overlay { ref title, .. }) if title == "/help"
            ),
            "Enter ran the picked command: {:?}",
            harness.controller.ui_state().modal
        );
        assert!(
            submissions(&harness).is_empty(),
            "a command is not a request"
        );
        let _ = harness.controller.handle_key(Key::Esc);

        type_text(&mut harness.controller, "/thinking");
        let _ = harness.controller.handle_key(Key::Enter);
        assert_eq!(harness.controller.ui_state().buffer, "/effort ");
        let levels = harness.controller.ui_state().suggestions;
        assert_eq!(levels.first().map(|item| item.label.as_str()), Some("off"));
        let _ = harness.controller.handle_key(Key::Down);
        let effects = harness.controller.handle_key(Key::Enter);
        assert!(
            harness.controller.ui_state().buffer.is_empty(),
            "the picked level was submitted: {effects:#?}"
        );
    }

    /// Measured: `/help` opened on seven of thirty-three commands and "còn 26 dòng",
    /// because the inline viewport gives a panel about eight rows. The TUI card names
    /// every command in its group and fits; `/help all` still opens the full table.
    #[test]
    fn help_in_the_tui_is_a_card_that_fits_and_all_opens_the_table() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "/help");
        let Some(Modal::Overlay { title, lines, .. }) = harness.controller.ui_state().modal else {
            panic!("no overlay: {:?}", harness.controller.ui_state().modal);
        };
        assert_eq!(title, "/help");
        assert!(lines.len() <= 8, "the card fits a small panel: {lines:#?}");
        let card = lines.join("\n");
        for command in crate::interactive::input::SLASH_COMMANDS {
            let name = command.name.trim_end_matches(':');
            assert!(card.contains(name), "{name} is on the card:\n{card}");
        }
        let _ = harness.controller.handle_key(Key::Esc);
        let _ = submit_text(&mut harness.controller, "/help all");
        let Some(Modal::Overlay { lines, .. }) = harness.controller.ui_state().modal else {
            panic!("no overlay for /help all");
        };
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("/help") && line.contains("List every command")),
            "the full table: {lines:#?}"
        );
    }

    /// Measured: every auto-allowed read printed an `[info] allowed by ...` row above
    /// its card, half the transcript of a read-heavy turn. In the TUI the reason rides
    /// on the card; nothing is hidden, and no separate row is added.
    #[test]
    fn an_auto_allowed_tool_carries_its_reason_on_the_card_in_the_tui() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        harness
            .events
            .send(SessionEvent::Notice {
                message: "allowed by mode turn-grant: list . (read-only)".to_owned(),
            })
            .expect("notice");
        harness
            .events
            .send(SessionEvent::ToolStarted {
                name: "list_files".to_owned(),
                call_id: String::new(),
                summary: "path=.".to_owned(),
                input: String::new(),
            })
            .expect("started");
        harness
            .events
            .send(SessionEvent::ToolSettled {
                name: "list_files".to_owned(),
                call_id: String::new(),
                ok: true,
                elapsed: Duration::from_millis(900),
                detail: String::new(),
            })
            .expect("settled");
        let effects = harness.controller.pump_events();
        let items = effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::History(item) => Some(item.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(
            !items
                .iter()
                .any(|item| matches!(item, HistoryItem::Notice { .. })),
            "no separate row: {items:#?}"
        );
        assert!(
            items.iter().any(|item| matches!(
                item,
                HistoryItem::Tool { name, summary, .. }
                    if name == "list_files" && summary == "path=. · allowed by mode turn-grant"
            )),
            "the card says why it ran without a panel: {items:#?}"
        );
    }

    /// Where the menu is not drawn it must not take a key. Two places: the plain
    /// renderer, which has no menu at all, and a panel that owns the keyboard.
    #[test]
    fn slash_the_menu_never_takes_a_key_where_it_is_not_drawn() {
        // Plain line input: Enter submits what was typed, as it always has.
        let mut plain = bench(true);
        let _ = plain.controller.boot_lines();
        let lines = effects_to_plain(&submit_text(&mut plain.controller, "/he")).join("\n");
        assert!(
            lines.contains("unknown command /he"),
            "plain mode must not complete a list it never showed: {lines}"
        );

        // TUI with a panel open: the panel owns the frame, so the same draft is
        // submitted rather than silently completed into a list nobody can see.
        let mut tui = tui_bench(true);
        let _ = tui.controller.boot_lines();
        let _ = submit_text(&mut tui.controller, "/help");
        assert!(
            tui.controller.ui_state().modal.is_some(),
            "the reference panel is open"
        );
        type_text(&mut tui.controller, "/he");
        assert!(
            !tui.controller.ui_state().suggestions.is_empty(),
            "the editor still holds the candidates; the frame is what hides them"
        );
        let _ = tui.controller.handle_key(Key::Enter);
        assert!(
            tui.controller
                .transcript()
                .join("\n")
                .contains("unknown command /he"),
            "Enter answered the draft, not a menu that is not on screen: {:?}",
            tui.controller.transcript()
        );
    }

    #[test]
    fn completion_typing_the_answer_still_works_while_the_panel_is_open() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "work");
        harness
            .events
            .send(approval_event("req-3"))
            .expect("approval");
        let _ = harness.controller.pump_events();

        // The TUI panel says `y chạy · n từ chối`, so n answers immediately and
        // the character never lands in the composer buffer.
        let _ = harness.controller.handle_key(Key::Char('n'));
        let state = harness.controller.ui_state();
        assert!(
            state.buffer.is_empty(),
            "the character must not reach the composer: {:?}",
            state.buffer
        );
        assert_eq!(
            harness.port.answers.lock().expect("answers").as_slice(),
            [("req-3".to_owned(), ApprovalDecision::Denied)]
        );
    }

    #[test]
    fn t06_y_key_grants_exactly_the_pending_request() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "work");
        harness
            .events
            .send(approval_event("req-8"))
            .expect("approval");
        let _ = harness.controller.pump_events();

        let effects = harness.controller.handle_key(Key::Char('y'));
        eprintln!(
            "after y: answers={:?} effects={effects:#?}",
            harness.port.answers.lock().expect("answers")
        );
        assert_eq!(
            harness.port.answers.lock().expect("answers").as_slice(),
            [("req-8".to_owned(), ApprovalDecision::Granted)],
            "y grants exactly the pending request"
        );
        assert!(
            effects_to_plain(&effects)
                .join("\n")
                .contains("[approval] granted req-8"),
            "{effects:#?}"
        );
        assert!(
            harness.controller.ui_state().modal.is_none(),
            "the panel closed"
        );
        assert_eq!(harness.controller.phase(), AppPhase::Running);
    }

    #[test]
    fn g05_always_allow_writes_a_rule_only_after_confirmation() {
        let mut harness = tui_bench(true);
        let root = harness.temp.path().join("project");
        *harness.port.allow_root.lock().expect("allow root") = Some(root.clone());
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "work");
        harness
            .events
            .send(approval_event("g05-confirm-rule"))
            .expect("approval");
        let _ = harness.controller.pump_events();

        let _ = harness.controller.handle_key(Key::Char('A'));
        assert!(
            harness
                .port
                .always_allowed
                .lock()
                .expect("rule log")
                .is_empty(),
            "A only proposes the exact pattern; it must not write configuration"
        );
        assert!(
            !root.join(".harness/config.local.toml").exists(),
            "the proposed pattern is not written before Enter"
        );
        assert!(
            harness.controller.ui_state().modal.is_some(),
            "the pending approval remains open while confirmation is outstanding"
        );

        let _ = harness.controller.handle_key(Key::Esc);
        assert!(
            harness
                .port
                .always_allowed
                .lock()
                .expect("rule log")
                .is_empty(),
            "Escape cancels saving and does not persist the proposal"
        );
        assert!(
            !root.join(".harness/config.local.toml").exists(),
            "Escape leaves configuration untouched"
        );
        assert!(
            harness.controller.ui_state().modal.is_some(),
            "cancelling the rule proposal does not deny or close the approval"
        );

        let _ = harness.controller.handle_key(Key::Char('A'));
        let _ = harness.controller.handle_key(Key::Enter);
        assert_eq!(
            harness
                .port
                .always_allowed
                .lock()
                .expect("rule log")
                .as_slice(),
            [(
                "g05-confirm-rule".to_owned(),
                "apply_patch(src/parser.rs)".to_owned()
            )],
            "only the explicit Enter confirmation may persist the displayed pattern"
        );
        let local = std::fs::read_to_string(root.join(".harness/config.local.toml"))
            .expect("confirmed local rule is written");
        assert!(local.contains("apply_patch(src/parser.rs)"), "{local}");
        assert!(root.join(".harness/.gitignore").exists());
    }

    #[test]
    fn g05_mode_changes_are_session_scoped_and_permissions_show_rule_layers() {
        let port = RecordingPort::default();
        port.permission_lines
            .lock()
            .expect("permission lines")
            .extend([
                "mode: ask".to_owned(),
                "allow (project(local)): run_shell(cargo test *)".to_owned(),
                "actions auto-allowed this session: 0".to_owned(),
            ]);
        let mut harness = bench_with(true, port, true);
        let _ = harness.controller.boot_lines();

        let changed =
            effects_to_plain(&submit_text(&mut harness.controller, "/mode auto-edit")).join("\n");
        assert!(changed.contains("permission mode set to auto-edit for this session"));
        assert_eq!(
            harness
                .port
                .mode_updates
                .lock()
                .expect("mode updates")
                .as_slice(),
            ["auto-edit"]
        );

        let permissions =
            effects_to_plain(&submit_text(&mut harness.controller, "/permissions")).join("\n");
        assert!(permissions.contains("allow (project(local)): run_shell(cargo test *)"));
        assert!(permissions.contains("actions auto-allowed this session: 0"));

        // As Claude Code's Shift+Tab: the running turn follows the change from
        // its next action.
        let _ = submit_text(&mut harness.controller, "running request");
        let changed =
            effects_to_plain(&submit_text(&mut harness.controller, "/mode ask")).join("\n");
        assert!(
            changed.contains("the running turn follows it from its next action"),
            "{changed}"
        );
        assert_eq!(
            harness
                .port
                .mode_updates
                .lock()
                .expect("mode updates")
                .as_slice(),
            ["auto-edit", "ask"]
        );
    }

    /// Seen on a real screen: the proposal was printed in the scrollback *and* drawn
    /// in the panel, so the same four rows appeared twice and the panel looked like a
    /// garbled copy of the transcript.
    #[test]
    fn t06_the_open_panel_is_the_only_place_the_proposal_is_shown() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "work");
        harness
            .events
            .send(approval_event("req-10"))
            .expect("approval");
        let effects = harness.controller.pump_events();

        assert!(
            harness.controller.ui_state().modal.is_some(),
            "the panel is open"
        );
        assert!(
            history_items(&effects).is_empty(),
            "the panel owns the proposal in the TUI: {:#?}",
            history_items(&effects)
        );
    }

    /// The plain transcript has no panel, so there the block has to be written to the
    /// transcript or the reader never learns what was proposed (U20).
    #[test]
    fn t06_a_plain_session_still_records_the_proposal_it_cannot_panel() {
        let mut harness = bench_with(true, RecordingPort::default(), true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "work");
        harness
            .events
            .send(approval_event("req-11"))
            .expect("approval");
        let effects = harness.controller.pump_events();

        let items = history_items(&effects);
        assert!(
            items.iter().any(|item| matches!(
                item,
                HistoryItem::Approval { request_id, .. } if request_id == "req-11"
            )),
            "the plain transcript keeps the block: {items:#?}"
        );
        assert!(
            harness.controller.plain,
            "the bench has to be in plain mode for this to prove anything"
        );
    }

    /// The measured complaint this closes: a turn of `git log`, `git status`,
    /// `git diff` asked about every single command, and the old read-only grant could
    /// not cover any of them - `run_process` is not a read-only kind. Now every panel
    /// offers `a`, and the hint says so where the user is looking.
    #[test]
    fn t06_the_turn_grant_is_offered_on_a_write_panel_and_on_a_read_panel() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "work");

        harness
            .events
            .send(approval_event("req-write"))
            .expect("write approval");
        let _ = harness.controller.pump_events();
        let write = harness.controller.ui_state().modal;
        assert!(
            matches!(
                write,
                Some(Modal::Approval {
                    read_only: false,
                    ..
                })
            ),
            "a patch is not read-only: {write:?}"
        );
        let hint = crate::interactive::tui::widgets::composer::hint(&harness.controller.ui_state());
        assert!(
            hint.contains('a') && hint.contains('y') && hint.contains('n'),
            "every panel names all three answers: {hint}"
        );

        let mut reads = tui_bench(true);
        let _ = reads.controller.boot_lines();
        let _ = submit_text(&mut reads.controller, "work");
        reads
            .events
            .send(read_approval_event("req-read"))
            .expect("read approval");
        let _ = reads.controller.pump_events();
        let read = reads.controller.ui_state().modal;
        assert!(
            matches!(
                read,
                Some(Modal::Approval {
                    read_only: true,
                    ..
                })
            ),
            "a listing is read-only: {read:?}"
        );
        assert!(
            crate::interactive::tui::widgets::composer::hint(&reads.controller.ui_state())
                .contains('a'),
            "the hint names the key the panel offers"
        );
    }

    /// `a` grants the action in front of the user, opens the gate for the rest of the
    /// run - every kind, not only reads - and says so in the transcript instead of
    /// widening it silently.
    #[test]
    fn t06_the_turn_key_grants_the_action_and_the_whole_turn() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "work");
        harness
            .events
            .send(approval_event("req-write-1"))
            .expect("write approval");
        let _ = harness.controller.pump_events();

        let effects = harness.controller.handle_key(Key::Char('a'));
        assert_eq!(
            harness.port.answers.lock().expect("answers").as_slice(),
            [("req-write-1".to_owned(), ApprovalDecision::GrantForRun)],
            "the answer carries the wider meaning, not a plain grant"
        );
        assert_eq!(
            harness
                .port
                .run_grants
                .lock()
                .expect("run grants")
                .as_slice(),
            [true],
            "the port is told to stop asking for the rest of the turn"
        );
        assert!(
            harness.controller.ui_state().granted_for_run,
            "the status row has to be able to say the gate is open"
        );
        let plain = effects_to_plain(&effects).join("\n");
        assert!(
            plain.contains("[approval] granted (every action allowed for this turn) req-write-1"),
            "the transcript records what was granted: {plain}"
        );
    }

    /// The grant covers one turn. A run that ends - however it ends - must not leave
    /// the next one running anything without being asked.
    #[test]
    fn t06_the_turn_grant_does_not_survive_the_turn() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "work");
        harness
            .events
            .send(approval_event("req-write-2"))
            .expect("write approval");
        let _ = harness.controller.pump_events();
        let _ = harness.controller.handle_key(Key::Char('a'));
        assert!(harness.controller.ui_state().granted_for_run);

        harness
            .events
            .send(SessionEvent::RunTerminal {
                outcome: RunOutcome::Done,
            })
            .expect("terminal");
        let _ = harness.controller.pump_events();
        assert!(
            !harness.controller.ui_state().granted_for_run,
            "the run is over, so the gate closes"
        );
        assert_eq!(
            harness
                .port
                .run_grants
                .lock()
                .expect("run grants")
                .as_slice(),
            [true, false],
            "the port is told to close it again, in that order"
        );
    }

    /// A bound that the app carries on past is the *same* turn, so the grant survives
    /// it - otherwise the user who just said "allow this turn" would be asked again
    /// halfway through the work, which is the complaint this whole change answers.
    /// When the budget is spent the pause is real and the grant closes with the turn.
    #[test]
    fn t06_the_turn_grant_survives_an_automatic_continuation_and_closes_with_the_turn() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        harness.controller.max_continuations = 1;
        let _ = submit_text(&mut harness.controller, "work");
        harness
            .events
            .send(approval_event("req-write-3"))
            .expect("write approval");
        let _ = harness.controller.pump_events();
        let _ = harness.controller.handle_key(Key::Char('a'));
        assert!(harness.controller.ui_state().granted_for_run);

        // The first pause is continued by the app itself: same turn, grant still open.
        harness
            .events
            .send(SessionEvent::RunTerminal {
                outcome: RunOutcome::Paused(PauseReason::StepLimit),
            })
            .expect("terminal");
        let _ = harness.controller.pump_events();
        assert_eq!(
            submissions(&harness).len(),
            2,
            "the app continued the turn by itself"
        );
        assert!(
            harness.controller.ui_state().granted_for_run,
            "a continuation is the same turn: the grant stays open"
        );
        assert_eq!(
            harness
                .port
                .run_grants
                .lock()
                .expect("run grants")
                .as_slice(),
            [true],
            "and the port was never told to close it"
        );

        // The budget is spent: this pause stands, so the turn is over.
        harness
            .events
            .send(SessionEvent::RunTerminal {
                outcome: RunOutcome::Paused(PauseReason::StepLimit),
            })
            .expect("terminal");
        let _ = harness.controller.pump_events();
        assert!(
            !harness.controller.ui_state().granted_for_run,
            "a pause that stands ends the turn, so the gate closes"
        );
        assert_eq!(
            harness
                .port
                .run_grants
                .lock()
                .expect("run grants")
                .as_slice(),
            [true, false],
            "the port is told to close it, once"
        );
    }

    #[test]
    fn t06_escape_closes_the_panel_without_answering() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "work");
        harness
            .events
            .send(approval_event("req-9"))
            .expect("approval");
        let _ = harness.controller.pump_events();

        let effects = harness.controller.handle_key(Key::Esc);
        assert!(effects.is_empty(), "Escape only dismisses: {effects:#?}");
        assert!(
            harness.port.answers.lock().expect("answers").is_empty(),
            "Escape never answers a gated action"
        );
        assert_eq!(
            harness.controller.phase(),
            AppPhase::WaitingApproval,
            "the request is still pending"
        );
    }

    #[test]
    fn t06_help_overlay_is_not_written_to_history() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        let transcript_before = harness.controller.transcript().to_vec();

        let effects = submit_text(&mut harness.controller, "/help");
        assert!(
            history_items(&effects).is_empty(),
            "reference output is a temporary panel in TUI mode: {effects:#?}"
        );
        assert_eq!(harness.controller.transcript(), transcript_before);
        assert!(
            matches!(
                harness.controller.ui_state().modal,
                Some(Modal::Overlay { ref title, .. }) if title == "/help"
            ),
            "the controller must expose the editor overlay to the renderer"
        );

        let effects = harness.controller.handle_key(Key::Esc);
        assert_eq!(effects, vec![Effect::Redraw]);
        assert!(harness.controller.ui_state().modal.is_none());
        assert_eq!(harness.controller.transcript(), transcript_before);
    }

    fn two_sessions() -> Vec<SessionCandidate> {
        vec![
            SessionCandidate {
                session_id: "session_a".to_owned(),
                task_id: "task_1".to_owned(),
                detail: "1 input".to_owned(),
            },
            SessionCandidate {
                session_id: "session_b".to_owned(),
                task_id: "task_1".to_owned(),
                detail: "2 inputs".to_owned(),
            },
        ]
    }

    #[test]
    fn t06_esc_closes_the_picker_without_changing_the_source() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        harness
            .events
            .send(SessionEvent::SessionsListed {
                sessions: two_sessions(),
            })
            .expect("sessions");
        let _ = harness.controller.pump_events();
        assert!(matches!(
            harness.controller.ui_state().modal,
            Some(Modal::Picker { .. })
        ));

        let effects = harness.controller.handle_key(Key::Esc);
        assert_eq!(effects, vec![Effect::Redraw]);
        assert!(harness.controller.ui_state().modal.is_none());
        assert!(harness.port.resumes.lock().expect("resumes").is_empty());
    }

    #[test]
    fn t06_picker_enter_resumes_the_highlighted_session() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        harness
            .events
            .send(SessionEvent::SessionsListed {
                sessions: two_sessions(),
            })
            .expect("sessions");
        let _ = harness.controller.pump_events();
        let _ = harness.controller.handle_key(Key::Down);
        let effects = harness.controller.handle_key(Key::Enter);

        assert_eq!(
            harness.port.resumes.lock().expect("resumes").as_slice(),
            [Some("session_b".to_owned())]
        );
        assert!(harness.controller.ui_state().modal.is_none());
        assert!(
            effects_to_plain(&effects)
                .join("\n")
                .contains("continuing from session")
        );
    }

    /// Looking back in expanded mode: `/more` draws a tool call the way ctrl+o's
    /// reprint does - every output line under the call's card - while collapsed
    /// mode keeps the preview.
    #[test]
    fn more_in_expanded_mode_shows_the_whole_tool_call() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        harness.controller.set_columns(60);
        let _ = submit_text(&mut harness.controller, "run the tests");
        for event in [
            SessionEvent::ToolStarted {
                name: "run_shell".to_owned(),
                call_id: String::new(),
                summary: "command=cargo test".to_owned(),
                input: r#"{"command":"cargo test --workspace --locked"}"#.to_owned(),
            },
            SessionEvent::ToolOutput {
                text: "run_shell:\nline 1\nline 2\nline 3\nline 4\nline 5".to_owned(),
            },
            SessionEvent::ToolSettled {
                name: "run_shell".to_owned(),
                call_id: String::new(),
                ok: true,
                elapsed: Duration::from_millis(40),
                detail: String::new(),
            },
            SessionEvent::RunTerminal {
                outcome: RunOutcome::Done,
            },
        ] {
            harness.events.send(event).expect("event");
        }
        let _ = harness.controller.pump_events();
        let more = |controller: &mut InteractiveController| {
            let _ = submit_text(controller, "/more");
            let Some(Modal::Overlay { lines, .. }) = controller.ui_state().modal else {
                panic!("/more opens a panel");
            };
            let _ = controller.handle_key(Key::Esc);
            lines.join("\n")
        };

        let collapsed = more(&mut harness.controller);
        assert!(collapsed.contains("run_shell"), "{collapsed}");
        assert!(!collapsed.contains("line 5"), "{collapsed}");
        assert!(!collapsed.contains("--locked"), "{collapsed}");

        let _ = harness.controller.handle_key(Key::CycleDetail);
        let effects = harness.controller.handle_key(Key::CycleDetail);
        assert!(
            effects.contains(&Effect::Reprint(
                crate::interactive::events::Detail::Expanded
            )),
            "ctrl+o redraws the scrollback in expanded mode: {effects:?}"
        );
        // prime-agent's "all output": every line of what the call returned, under
        // the same one-line card - no boxes.
        let expanded = more(&mut harness.controller);
        assert!(expanded.contains("line 5"), "{expanded}");
        assert!(
            expanded.contains("❯ Run shell") && expanded.contains("✓ 40ms"),
            "{expanded}"
        );
        assert!(
            !expanded.contains('┌') && !expanded.contains('╭'),
            "{expanded}"
        );
    }

    /// K04: `/more` reopens what the live viewport clipped, from its first line.
    ///
    /// The live block keeps only a bounded tail, so a long answer scrolls its own
    /// opening out of the viewport. The panel is the way back to it without leaving
    /// the app, and it must open at the top: a reader who has to scroll before seeing
    /// the beginning is the problem, not the fix.
    #[test]
    fn k04_more_opens_the_recent_transcript_from_its_first_line_and_scrolls() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "a long question");
        for index in 0..20 {
            let _ = harness.events.send(SessionEvent::TextDelta {
                text: format!("answer line {index}\n"),
            });
            let _ = harness.controller.pump_events();
        }
        let _ = harness.events.send(SessionEvent::RunTerminal {
            outcome: RunOutcome::Done,
        });
        let _ = harness.controller.pump_events();

        let effects = submit_text(&mut harness.controller, "/more");
        let Some(Modal::Overlay {
            title,
            lines,
            scroll,
            ..
        }) = harness.controller.ui_state().modal
        else {
            panic!("the TUI opens a panel, not history: {effects:#?}");
        };
        assert_eq!(title, "/more");
        assert_eq!(scroll, 0, "the panel opens at the top");
        let joined = lines.join("\n");
        assert!(
            joined.contains("answer line 0"),
            "the clipped opening is missing from the panel: {joined}"
        );
        assert!(
            joined.contains("answer line 19"),
            "the panel must hold the whole answer: {joined}"
        );

        // Scrolling moves the panel, not the transcript.
        let before = harness.controller.transcript().len();
        let _ = harness.controller.handle_key(Key::PageDown);
        let Some(Modal::Overlay { scroll, .. }) = harness.controller.ui_state().modal else {
            panic!("the panel stays open while scrolling");
        };
        assert_eq!(scroll, 8, "PageDown moves one page");
        let _ = harness.controller.handle_key(Key::End);
        let Some(Modal::Overlay { scroll, .. }) = harness.controller.ui_state().modal else {
            panic!("the panel stays open at the end");
        };
        assert!(scroll > 8, "End jumps to the bottom: {scroll}");
        let _ = harness.controller.handle_key(Key::Home);
        let Some(Modal::Overlay { scroll, .. }) = harness.controller.ui_state().modal else {
            panic!("the panel stays open at the top");
        };
        assert_eq!(scroll, 0, "Home returns to the top");
        // The arrows move a panel one line, as they do in prime-agent's panels.
        let _ = harness.controller.handle_key(Key::Down);
        let _ = harness.controller.handle_key(Key::Down);
        let _ = harness.controller.handle_key(Key::Up);
        let Some(Modal::Overlay { scroll, .. }) = harness.controller.ui_state().modal else {
            panic!("the panel stays open while the arrows move it");
        };
        assert_eq!(scroll, 1, "Down twice and Up once leave it one line down");
        assert_eq!(
            harness.controller.transcript().len(),
            before,
            "scrolling a panel must not write history"
        );
        // The panel advertises its keys. Every key it names must be one this
        // controller actually handles: a hint that names a key which falls through
        // to the editor is worse than no hint, because the user presses it and edits
        // the draft behind the panel.
        let frame = {
            use crate::interactive::tui::TuiRenderer as _;
            let backend = crate::interactive::terminal::ScriptedBackend::new(Vec::new());
            let mut renderer = crate::interactive::tui::ScriptedRenderer::open(backend, 100, 30)
                .expect("renderer opens");
            renderer
                .draw_state(&harness.controller.ui_state())
                .expect("frame draws");
            renderer.painted().join("\n")
        };
        assert!(
            frame.contains("PgUp/PgDn"),
            "the panel must say which keys scroll it: {frame}"
        );
        assert!(
            frame.contains("Home/End"),
            "the panel must name Home/End, which do scroll it: {frame}"
        );
        assert!(
            !frame.contains('↑') && !frame.contains('↓'),
            "the panel must not advertise the arrow keys, which the editor owns: {frame}"
        );
        // Escape still closes it, which is what acceptance U09 asserts.
        let _ = harness.controller.handle_key(Key::Esc);
        assert!(
            harness.controller.ui_state().modal.is_none(),
            "Escape closes the panel"
        );
    }

    /// `/status` shows the project id, because memory is scoped by it and the app
    /// shows it nowhere else.
    #[test]
    fn k05_status_reports_the_project_scope_and_says_so_when_there_is_none() {
        struct ScopePort {
            recorded: RecordingPort,
            scope: Option<String>,
        }

        impl SessionPort for ScopePort {
            fn label(&self) -> String {
                self.recorded.label()
            }
            fn submit(&mut self, request: SubmitRequest) {
                self.recorded.submit(request);
            }
            fn cancel(&mut self) {
                self.recorded.cancel();
            }
            fn answer(&mut self, request_id: &str, decision: ApprovalDecision) -> bool {
                self.recorded.answer(request_id, decision)
            }
            fn resume(&mut self, session_id: Option<String>) -> Result<(), String> {
                self.recorded.resume(session_id)
            }
            fn limits(&self) -> TurnBounds {
                self.recorded.limits()
            }
            fn project_id(&mut self) -> Option<String> {
                self.scope.clone()
            }
        }

        for (scope, expected) in [
            (
                Some("project_01a0bde9-575d-7640-96ff-f94721ad22a1".to_owned()),
                "Project: project_01a0bde9-575d-7640-96ff-f94721ad22a1",
            ),
            (None, "Project: not registered yet"),
        ] {
            let (temp, context) = context(true);
            let channel = SessionChannel::new();
            let mut controller = InteractiveController::new(
                &context,
                Box::new(ScopePort {
                    recorded: RecordingPort::default(),
                    scope,
                }),
                channel,
                true,
            );
            controller.boot_lines();
            let effects = submit_text(&mut controller, "/status");
            let text = effects_to_plain(&effects).join("\n");
            assert!(
                text.contains(expected),
                "the panel must say what the store is scoped by: {text}"
            );
            drop(temp);
        }
    }

    /// Choosing a model or a thinking level must reach the screen: the status row
    /// reads the header, and the opening banner is drawn again with the new lines.
    /// The banner used to keep naming the model the session started with.
    #[test]
    fn choosing_a_model_or_a_level_updates_the_header_and_redraws_the_banner() {
        struct ChoicePort {
            recorded: RecordingPort,
            model: std::sync::Arc<std::sync::Mutex<(String, String)>>,
        }

        impl SessionPort for ChoicePort {
            fn label(&self) -> String {
                format!("{} via https://example.test", self.model.lock().unwrap().0)
            }
            fn submit(&mut self, request: SubmitRequest) {
                self.recorded.submit(request);
            }
            fn cancel(&mut self) {
                self.recorded.cancel();
            }
            fn answer(&mut self, request_id: &str, decision: ApprovalDecision) -> bool {
                self.recorded.answer(request_id, decision)
            }
            fn resume(&mut self, session_id: Option<String>) -> Result<(), String> {
                self.recorded.resume(session_id)
            }
            fn limits(&self) -> TurnBounds {
                self.recorded.limits()
            }
            fn set_model(&mut self, model: &str) -> Result<String, String> {
                self.model.lock().unwrap().0 = model.to_owned();
                Ok(format!("model {model} selected"))
            }
            fn set_thinking(&mut self, level: &str) -> Result<String, String> {
                self.model.lock().unwrap().1 = level.to_owned();
                Ok(format!("thinking level {level} for the next turn"))
            }
            fn thinking_level(&self) -> Option<String> {
                Some(self.model.lock().unwrap().1.clone())
            }
        }

        let (temp, context) = context(true);
        let model = std::sync::Arc::new(std::sync::Mutex::new((
            "first-model".to_owned(),
            "low".to_owned(),
        )));
        let mut controller = InteractiveController::new(
            &context,
            Box::new(ChoicePort {
                recorded: RecordingPort::default(),
                model: std::sync::Arc::clone(&model),
            }),
            SessionChannel::new(),
            false,
        );
        let opening = controller.boot_lines().join("\n");
        assert!(opening.contains("first-model"), "{opening}");
        assert!(opening.contains("Thinking: low"), "{opening}");

        let banner = |effects: &[Effect]| {
            effects
                .iter()
                .filter_map(|effect| match effect {
                    Effect::Banner(lines) => Some(lines.join("\n")),
                    _ => None,
                })
                .next_back()
        };
        let effects = submit_text(&mut controller, "/model second-model");
        let redrawn = banner(&effects).expect("a model change redraws the banner");
        assert!(redrawn.contains("Service: second-model"), "{redrawn}");
        assert!(!redrawn.contains("first-model"), "{redrawn}");
        let state = controller.ui_state();
        assert!(
            state
                .header
                .iter()
                .any(|line| line.starts_with("Service: second-model")),
            "the status row reads this: {:?}",
            state.header
        );

        let effects = submit_text(&mut controller, "/effort high");
        let redrawn = banner(&effects).expect("a level change redraws the banner");
        assert!(redrawn.contains("Thinking: high"), "{redrawn}");
        assert!(!redrawn.contains("Thinking: low"), "{redrawn}");
        drop(temp);
    }

    /// A scrolling overlay keeps the help text verbatim and never clips the top.
    #[test]
    fn k04_a_long_overlay_is_readable_from_its_first_row() {
        let lines: Vec<String> = (0..40).map(|index| format!("row {index}")).collect();
        let mut editor = crate::interactive::input::LineEditor::new();
        editor.open_overlay("test", lines);
        assert_eq!(
            editor.overlay().expect("overlay").scroll(),
            0,
            "an overlay opens at the top"
        );
        assert!(editor.scroll_overlay(5));
        assert_eq!(editor.overlay().expect("overlay").scroll(), 5);
        assert!(editor.scroll_overlay(-99));
        assert_eq!(
            editor.overlay().expect("overlay").scroll(),
            0,
            "scrolling up past the top saturates instead of wrapping"
        );
        assert!(editor.scroll_overlay_to(true));
        assert!(
            editor.overlay().expect("overlay").scroll() > 30,
            "End asks for the bottom"
        );
        assert!(!crate::interactive::input::LineEditor::new().scroll_overlay(5));
    }

    /// What a running call prints shows under its card while it runs - the
    /// last lines, a progress bar's last redraw - and goes when it settles.
    #[test]
    fn a_running_calls_output_shows_under_its_card_until_it_settles() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "build it");
        for event in [
            SessionEvent::ToolStarted {
                name: "run_shell".to_owned(),
                call_id: "c1".to_owned(),
                summary: "cargo build".to_owned(),
                input: String::new(),
            },
            SessionEvent::ToolProgress {
                call_id: "c1".to_owned(),
                text: "Compiling a\nCompiling b\n".to_owned(),
            },
            SessionEvent::ToolProgress {
                call_id: "c1".to_owned(),
                text: "Compiling c\n  10%\r  90%\n".to_owned(),
            },
        ] {
            harness.events.send(event).expect("event");
        }
        let _ = harness.controller.pump_events();
        assert_eq!(
            harness.controller.ui_state().live_tool_output,
            ["Compiling b", "Compiling c", "  90%"]
        );
        harness
            .events
            .send(SessionEvent::ToolSettled {
                name: "run_shell".to_owned(),
                call_id: "c1".to_owned(),
                ok: true,
                elapsed: Duration::from_millis(5),
                detail: String::new(),
            })
            .expect("settled");
        let _ = harness.controller.pump_events();
        assert!(harness.controller.ui_state().live_tool_output.is_empty());
    }
    /// A model call retried after its stream failed part-way: the live text of
    /// the failed attempt is dropped, a notice says so, and the retry's answer
    /// is the only one shown.
    #[test]
    fn a_restarted_stream_drops_the_failed_attempts_text() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "work");
        for event in [
            SessionEvent::TextDelta {
                text: "Hel".to_owned(),
            },
            SessionEvent::StreamRestarted,
            SessionEvent::TextDelta {
                text: "Hello".to_owned(),
            },
        ] {
            harness.events.send(event).expect("event");
        }
        let effects = harness.controller.pump_events();
        assert_eq!(harness.controller.ui_state().live_text, "Hello");
        assert!(
            effects.iter().any(|effect| matches!(
                effect,
                Effect::History(HistoryItem::Notice { message }) if message.contains("retrying")
            )),
            "{effects:?}"
        );
    }
    #[test]
    fn t04_stream_text_shows_in_the_live_block_before_run_terminal() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "work");
        harness
            .events
            .send(SessionEvent::TextDelta {
                text: "partial answer".to_owned(),
            })
            .expect("delta");
        let effects = harness.controller.pump_events();
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::Stream(_))),
            "a partial line stays in the live viewport: {effects:#?}"
        );
        assert_eq!(harness.controller.ui_state().live_text, "partial answer");

        harness
            .events
            .send(SessionEvent::RunTerminal {
                outcome: RunOutcome::Done,
            })
            .expect("terminal");
        let effects = harness.controller.pump_events();
        assert!(matches!(effects.first(), Some(Effect::Stream(text)) if text == "partial answer"));
        assert!(harness.controller.ui_state().live_text.is_empty());
    }

    #[test]
    fn t04_child_progress_does_not_split_streamed_markdown() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "investigate the UI");
        let mut effects = Vec::new();

        for event in [
            SessionEvent::TextDelta {
                text: "Xác nhận thêm: trang success **crash render".to_owned(),
            },
            SessionEvent::Notice {
                message: "[child explorer] read_file: path=src/lib/config.ts".to_owned(),
            },
            SessionEvent::TextDelta {
                text: "** với `note` dạng object".to_owned(),
            },
            SessionEvent::Notice {
                message: "[child explorer] search_text: path=node_modules".to_owned(),
            },
            SessionEvent::TextDelta {
                text: " (log server có `Objects are not valid as a React child`).".to_owned(),
            },
            SessionEvent::RunTerminal {
                outcome: RunOutcome::Done,
            },
        ] {
            harness.events.send(event).expect("event");
            effects.extend(harness.controller.pump_events());
        }

        let streamed = effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Stream(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            streamed,
            [
                "Xác nhận thêm: trang success **crash render** với `note` dạng object (log server có `Objects are not valid as a React child`)."
            ],
            "child progress must not create separate assistant fragments"
        );
    }

    #[test]
    fn t04_tool_card_settles_in_place_with_duration() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "work");
        harness
            .events
            .send(SessionEvent::ToolStarted {
                name: "read_file".to_owned(),
                call_id: String::new(),
                summary: "path=a.rs".to_owned(),
                input: r#"{"path":"a.rs"}"#.to_owned(),
            })
            .expect("started");
        let started = harness.controller.pump_events();
        assert!(
            !history_items(&started)
                .iter()
                .any(|item| matches!(item, HistoryItem::Tool { .. })),
            "a running card belongs to the live viewport"
        );
        assert_eq!(
            harness.controller.ui_state().open_tools,
            vec![("read_file".to_owned(), "path=a.rs".to_owned())]
        );

        harness
            .events
            .send(SessionEvent::ToolSettled {
                name: "read_file".to_owned(),
                call_id: String::new(),
                ok: true,
                elapsed: Duration::from_millis(12),
                detail: String::new(),
            })
            .expect("settled");
        let settled = harness.controller.pump_events();
        let cards: Vec<_> = history_items(&settled)
            .into_iter()
            .filter(|item| matches!(item, HistoryItem::Tool { .. }))
            .collect();
        assert_eq!(cards.len(), 1, "settling creates one final card");
        assert!(matches!(
            &cards[0],
            HistoryItem::Tool { name, summary, input, state: ToolState::Ok { elapsed } }
                if name == "read_file" && summary == "path=a.rs" && input == r#"{"path":"a.rs"}"#
                    && *elapsed == Duration::from_millis(12)
        ));
        assert!(harness.controller.ui_state().open_tools.is_empty());
    }

    #[test]
    fn t04_long_stream_commits_overflow_lines_in_order() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "work");
        let text = (0..10)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        harness
            .events
            .send(SessionEvent::TextDelta { text })
            .expect("delta");
        let effects = harness.controller.pump_events();
        assert!(matches!(
            effects.first(),
            Some(Effect::Stream(text)) if text == "line 0\nline 1\n"
        ));
        assert_eq!(
            harness.controller.ui_state().live_text,
            (2..10)
                .map(|index| format!("line {index}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn t04_history_order_is_user_tool_assistant_run() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        let mut effects = submit_text(&mut harness.controller, "work");
        harness
            .events
            .send(SessionEvent::TextDelta {
                text: "before tool".to_owned(),
            })
            .expect("text");
        harness
            .events
            .send(SessionEvent::ToolStarted {
                name: "read_file".to_owned(),
                call_id: String::new(),
                summary: "path=a.rs".to_owned(),
                input: String::new(),
            })
            .expect("start");
        harness
            .events
            .send(SessionEvent::ToolSettled {
                name: "read_file".to_owned(),
                call_id: String::new(),
                ok: true,
                elapsed: Duration::from_millis(1),
                detail: String::new(),
            })
            .expect("settle");
        harness
            .events
            .send(SessionEvent::TextDelta {
                text: "after tool".to_owned(),
            })
            .expect("text");
        harness
            .events
            .send(SessionEvent::RunTerminal {
                outcome: RunOutcome::Done,
            })
            .expect("terminal");
        effects.extend(harness.controller.pump_events());

        let order: Vec<&str> = effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::History(HistoryItem::User { .. }) => Some("user"),
                Effect::Stream(_) => Some("assistant"),
                Effect::History(HistoryItem::Tool { .. }) => Some("tool"),
                Effect::History(HistoryItem::Run { .. }) => Some("run"),
                _ => None,
            })
            .collect();
        assert_eq!(order, ["user", "assistant", "tool", "assistant", "run"]);
    }

    #[test]
    fn completion_resume_zero_and_active_switch_are_rejected() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let effects = submit_text(&mut harness.controller, "/resume 0");
        assert!(
            effects_to_plain(&effects)
                .join("\n")
                .contains("not in the last list"),
            "{effects:#?}"
        );
        assert!(harness.port.resumes.lock().expect("resumes").is_empty());

        let _ = submit_text(&mut harness.controller, "work");
        let effects = submit_text(&mut harness.controller, "/resume");
        assert!(
            effects_to_plain(&effects)
                .join("\n")
                .contains("cannot change or list sessions"),
            "{effects:#?}"
        );
    }

    #[test]
    fn completion_exit_commands_and_eof_cancel_during_approval() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "work");
        harness
            .events
            .send(approval_event("req-5"))
            .expect("approval");
        let _ = harness.controller.pump_events();

        let effects = harness.controller.handle_key(Key::EndOfInput);
        assert!(
            !effects.contains(&Effect::Exit(EXIT_SUCCESS)),
            "Ctrl-D must wait for service cleanup before exit: {effects:#?}"
        );
        assert_eq!(*harness.port.cancels.lock().expect("cancels"), 1);
        assert_eq!(harness.controller.phase(), AppPhase::Canceling);
        harness
            .events
            .send(SessionEvent::RunTerminal {
                outcome: RunOutcome::Canceled,
            })
            .expect("canceled terminal event");
        let effects = harness.controller.pump_events();
        assert!(
            effects.contains(&Effect::Exit(EXIT_SUCCESS)),
            "{effects:#?}"
        );
        assert_eq!(harness.controller.phase(), AppPhase::Closed);
    }

    #[test]
    fn h04_an_unconfigured_provider_keeps_the_setup_state() {
        let mut harness = bench(false);
        let boot = harness.controller.boot_lines().join("\n");
        assert!(boot.contains("Harness Agents"), "{boot}");
        assert!(harness.controller.ui_state().setup_required);

        let mut configured = fixture_bench(true);
        let boot = configured.controller.boot_lines().join("\n");
        assert!(boot.contains("fixture (no model was called)"), "{boot}");
        assert_eq!(configured.controller.phase(), AppPhase::Ready);
    }

    #[test]
    fn h05_a_stale_approval_answer_is_not_claimed() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "active request");
        harness
            .events
            .send(approval_event("req-4"))
            .expect("approval");
        let _ = harness.controller.pump_events();
        // Expire it behind the controller's back. The panel closes and the run is
        // active again, so a late "y" is ordinary text for the running turn; it
        // never reaches the gate as an answer.
        harness
            .events
            .send(SessionEvent::ApprovalExpired {
                request_id: "req-4".to_owned(),
            })
            .expect("expiry");
        let _ = harness.controller.pump_events();
        let effects = submit_text(&mut harness.controller, "y");
        let plain = effects_to_plain(&effects).join("\n");
        assert!(
            plain.contains("sent to the running agent"),
            "a late answer is ordinary text for the running turn: {effects:#?}"
        );
        assert_eq!(*harness.port.steers.lock().expect("steers"), ["y"]);
        assert!(
            harness.port.answers.lock().expect("answers").is_empty(),
            "the gate is never told about a request that already expired"
        );
    }

    #[test]
    fn t02_eof_and_exit_keep_the_h05_rules() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let effects = harness.controller.handle_key(Key::EndOfInput);
        assert!(
            effects.contains(&Effect::Exit(EXIT_SUCCESS)),
            "{effects:#?}"
        );

        let mut cancelling = bench(true);
        let _ = cancelling.controller.boot_lines();
        let _ = submit_text(&mut cancelling.controller, "work");
        let effects = cancelling.controller.handle_key(Key::EndOfInput);
        assert!(
            !effects.contains(&Effect::Exit(EXIT_SUCCESS)),
            "Ctrl-D waits until cancellation releases the run: {effects:#?}"
        );
        assert_eq!(*cancelling.port.cancels.lock().expect("cancels"), 1);
        cancelling
            .events
            .send(SessionEvent::RunTerminal {
                outcome: RunOutcome::Canceled,
            })
            .expect("canceled terminal event");
        let effects = cancelling.controller.pump_events();
        assert!(
            effects.contains(&Effect::Exit(EXIT_SUCCESS)),
            "{effects:#?}"
        );
        assert_eq!(cancelling.controller.phase(), AppPhase::Closed);
    }

    #[test]
    fn t02_an_accepted_event_keeps_the_accepted_line() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        harness
            .events
            .send(SessionEvent::Accepted {
                input_id: input_id("input_0192f0aa-bbcc-7ddd-8eee-ffff00001111"),
            })
            .expect("accepted");
        let effects = harness.controller.pump_events();
        assert_eq!(
            effects_to_plain(&effects),
            vec!["[run] accepted ...00001111".to_owned()],
            "{effects:#?}"
        );
    }

    /// U20: every plain line the controller produces is the exact string the
    /// pre-T02 controller pushed with `Effect::WriteLine`/`WritePartial`.
    ///
    /// The expected strings come from the pre-T02 source (`view::tool_line`,
    /// `view::approval_lines`, `view::run_line`, `format!("> {text}")`, …), so a
    /// wording or ordering change fails here instead of silently changing what a
    /// plain-mode user sees.
    #[test]
    fn t02_plain_transcript_is_byte_identical_to_h03() {
        let items = vec![
            HistoryItem::User {
                text: "sửa lỗi parser".to_owned(),
            },
            HistoryItem::RunAccepted {
                input_id: "input_0192f0aa-bbcc-7ddd-8eee-ffff00001111".to_owned(),
            },
            HistoryItem::Tool {
                name: "search_text".to_owned(),
                summary: "pattern=parser".to_owned(),
                input: String::new(),
                state: ToolState::Started,
            },
            HistoryItem::Tool {
                name: "search_text".to_owned(),
                summary: String::new(),
                input: String::new(),
                state: ToolState::Failed {
                    elapsed: Duration::from_millis(3100),
                    detail: String::new(),
                },
            },
            HistoryItem::Tool {
                name: "apply_patch".to_owned(),
                summary: String::new(),
                input: String::new(),
                state: ToolState::Ok {
                    elapsed: Duration::from_millis(12),
                },
            },
            HistoryItem::Approval {
                action: "apply_patch".to_owned(),
                summary: "path=src/parser.rs".to_owned(),
                workspace: "C:/work/project".to_owned(),
                scope: "once".to_owned(),
                request_id: "req-1".to_owned(),
            },
            HistoryItem::ApprovalResolution {
                label: "granted".to_owned(),
                request_id: "req-1".to_owned(),
            },
            HistoryItem::Notice {
                message: "hello".to_owned(),
            },
            HistoryItem::Error {
                message: "boom".to_owned(),
            },
            HistoryItem::Message {
                text: "/help            list these commands".to_owned(),
            },
            HistoryItem::Run {
                outcome: RunOutcome::Done,
                steps: 3,
                tool_calls: 2,
                elapsed: Duration::from_millis(14_200),
            },
            HistoryItem::Run {
                outcome: RunOutcome::Failed("provider unreachable".to_owned()),
                steps: 0,
                tool_calls: 0,
                elapsed: Duration::ZERO,
            },
        ];
        let plain: Vec<String> = items.iter().flat_map(view::plain_lines).collect();
        assert_eq!(
            plain,
            vec![
                "> sửa lỗi parser".to_owned(),
                "[run] accepted ...00001111".to_owned(),
                "[tool] search_text pattern=parser".to_owned(),
                "[tool] search_text failed".to_owned(),
                "[tool] apply_patch ok".to_owned(),
                "[approval] apply_patch: path=src/parser.rs".to_owned(),
                "           workspace: C:/work/project".to_owned(),
                "           scope: once (request req-1)".to_owned(),
                "           answer y to run it once, a to allow every action for this turn, or n to refuse"
                    .to_owned(),
                "[approval] granted req-1".to_owned(),
                "[info] hello".to_owned(),
                "[error] boom".to_owned(),
                "/help            list these commands".to_owned(),
                "[run] done".to_owned(),
                "[run] failed: provider unreachable".to_owned(),
            ]
        );
    }

    /// The scripted scenario end to end: reading the transcript the controller
    /// recorded must give the same lines the renderer received.
    #[test]
    fn t02_the_recorded_transcript_is_what_the_plain_renderer_printed() {
        let mut harness = fixture_bench(true);
        let boot = harness.controller.boot_lines();
        let mut printed = boot.clone();
        printed.extend(effects_to_plain(&submit_text(
            &mut harness.controller,
            "sửa lỗi parser",
        )));
        printed.extend(effects_to_plain(&harness.controller.pump_events()));
        printed.extend(effects_to_plain(&submit_text(
            &mut harness.controller,
            "/help",
        )));
        printed.extend(effects_to_plain(&submit_text(
            &mut harness.controller,
            "please fail",
        )));
        printed.extend(effects_to_plain(&harness.controller.pump_events()));

        let recorded = harness.controller.transcript();
        assert_eq!(
            &printed[printed.len() - recorded.len()..],
            recorded,
            "the recorded transcript is exactly the tail the renderer printed"
        );

        let joined = recorded.join("\n");
        for landmark in [
            "> sửa lỗi parser",
            "fixture answer for: sửa lỗi parser (no model was called)",
            "[tool] search_text pattern=parser",
            "[tool] apply_patch ok",
            "[run] done",
            "> please fail",
            "[run] failed: fixture failure requested by the prompt",
        ] {
            assert!(
                joined.contains(landmark),
                "missing {landmark:?} in:\n{joined}"
            );
        }
    }

    fn end_with(harness: &mut Bench, outcome: RunOutcome) {
        harness
            .events
            .send(SessionEvent::RunTerminal { outcome })
            .expect("terminal event");
        let _ = harness.controller.pump_events();
    }

    fn child_settled(harness: &mut Bench, notice: &str) -> Vec<String> {
        harness
            .events
            .send(SessionEvent::ChildSettled {
                name: "scout".to_owned(),
                notice: notice.to_owned(),
            })
            .expect("child event");
        effects_to_plain(&harness.controller.pump_events())
    }

    /// Q02: an idle parent reads a child's notice in a turn of its own, at once.
    #[test]
    fn q02_a_notice_wakes_an_idle_parent() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let notice = "[child-exited: no-reply child:scout]\n\nLast assistant text: found it";
        let _ = child_settled(&mut harness, notice);
        assert_eq!(submissions(&harness), vec![notice.to_owned()]);
    }

    /// Q02: a busy parent reads it after the running turn - never steered in.
    #[test]
    fn q02_a_notice_waits_for_the_running_turn() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "first");
        let plain = child_settled(&mut harness, "[child-failed child:scout]\n\nboom");
        assert_eq!(submissions(&harness), vec!["first".to_owned()]);
        assert!(harness.port.steers.lock().expect("steers").is_empty());
        assert!(
            plain
                .iter()
                .any(|line| line.contains("its report is read when this turn ends")),
            "{plain:#?}"
        );
        end_with(&mut harness, RunOutcome::Done);
        assert_eq!(
            submissions(&harness),
            vec![
                "first".to_owned(),
                "[child-failed child:scout]\n\nboom".to_owned()
            ]
        );
    }

    /// Q02: after the user stops a turn, notices wait for the user's next turn.
    #[test]
    fn q02_a_notice_after_ctrl_c_waits_for_the_next_user_turn() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "first");
        end_with(&mut harness, RunOutcome::Canceled);
        let _ = child_settled(&mut harness, "[child-exited: no-reply child:scout]");
        assert_eq!(submissions(&harness), vec!["first".to_owned()], "held");
        let _ = submit_text(&mut harness.controller, "second");
        end_with(&mut harness, RunOutcome::Done);
        assert_eq!(
            submissions(&harness),
            vec![
                "first".to_owned(),
                "second".to_owned(),
                "[child-exited: no-reply child:scout]".to_owned()
            ]
        );
    }

    /// Q03: a child's message reaches a running parent at its next step, as
    /// written; an idle parent reads it in a turn of its own.
    #[test]
    fn q03_an_agent_message_reaches_the_parent() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "first");
        let message = "[agent-message from child:scout]\n\nfound it";
        harness
            .events
            .send(SessionEvent::AgentMessage {
                text: message.to_owned(),
            })
            .expect("child event");
        let _ = harness.controller.pump_events();
        assert_eq!(
            harness
                .port
                .delivered
                .lock()
                .expect("deliveries")
                .as_slice(),
            [message.to_owned()]
        );
        assert!(harness.port.steers.lock().expect("steers").is_empty());
        end_with(&mut harness, RunOutcome::Done);
        harness
            .events
            .send(SessionEvent::AgentMessage {
                text: "second message".to_owned(),
            })
            .expect("child event");
        let _ = harness.controller.pump_events();
        assert_eq!(
            submissions(&harness).last().map(String::as_str),
            Some("second message")
        );
    }

    /// Q01: Ctrl+C stops the turn, not the children; `/agents stop` stops them.
    #[test]
    fn q01_ctrl_c_cancels_the_turn_but_not_its_children() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "first");
        let _ = harness.controller.interrupt();
        assert!(harness.port.stops.lock().expect("stops").is_empty());
        end_with(&mut harness, RunOutcome::Canceled);
        let _ = submit_text(&mut harness.controller, "/agents stop scout");
        let _ = submit_text(&mut harness.controller, "/agents stop");
        assert_eq!(
            harness.port.stops.lock().expect("stops").as_slice(),
            ["scout".to_owned(), "all".to_owned()]
        );
    }

    /// Send the terminal event the service sends when a turn stops at a bound.
    fn end_at(harness: &mut Bench, reason: PauseReason) -> Vec<String> {
        harness
            .events
            .send(SessionEvent::RunTerminal {
                outcome: RunOutcome::Paused(reason),
            })
            .expect("terminal event");
        effects_to_plain(&harness.controller.pump_events())
    }

    fn submissions(harness: &Bench) -> Vec<String> {
        harness
            .port
            .submissions
            .lock()
            .expect("submission log")
            .clone()
    }

    /// The measured complaint: a long task ended at eight steps with tokens to spare and
    /// the app waited for a human to type "continue". A bound stops a runaway loop, not
    /// a task that is still moving, so the app carries on — visibly, and only while its
    /// continuation budget lasts.
    #[test]
    fn a_bound_continues_the_turn_until_the_budget_is_spent() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "sửa lỗi parser");
        assert_eq!(submissions(&harness).len(), 1);

        for used in 1..=DEFAULT_CONTINUATIONS {
            let plain = end_at(&mut harness, PauseReason::StepLimit);
            assert!(
                plain.iter().any(|line| line
                    == &format!(
                        "[info] step limit reached; continuing automatically ({used} of {DEFAULT_CONTINUATIONS}) — Ctrl-C stops this"
                    )),
                "continuation {used} was not announced: {plain:#?}"
            );
            assert!(
                plain
                    .iter()
                    .any(|line| line.starts_with("[auto] continue: the previous turn stopped")),
                "the app's own request was not marked as its own: {plain:#?}"
            );
            assert_eq!(
                submissions(&harness).len(),
                1 + used as usize,
                "each continuation is one more request: {plain:#?}"
            );
        }

        // The budget is spent: the next bound is the last word, and nothing is sent.
        let plain = end_at(&mut harness, PauseReason::StepLimit);
        assert!(
            !plain
                .iter()
                .any(|line| line.contains("continuing automatically")),
            "{plain:#?}"
        );
        assert_eq!(
            submissions(&harness).len(),
            1 + DEFAULT_CONTINUATIONS as usize
        );
    }

    /// The budget is per request, not per session: the next thing the user types may be
    /// continued again.
    #[test]
    fn a_new_request_restarts_the_continuation_budget() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "first");
        for _ in 0..=DEFAULT_CONTINUATIONS {
            let _ = end_at(&mut harness, PauseReason::StepLimit);
        }
        let spent = submissions(&harness).len();
        assert_eq!(spent, 1 + DEFAULT_CONTINUATIONS as usize);

        let _ = submit_text(&mut harness.controller, "second");
        let plain = end_at(&mut harness, PauseReason::StepLimit);
        assert!(
            plain
                .iter()
                .any(|line| line.contains("continuing automatically (1 of")),
            "a new request starts a new budget: {plain:#?}"
        );
        assert_eq!(submissions(&harness).len(), spent + 1 + 1);
    }

    /// The deadline is wall-clock time already spent. Continuing it by itself would spend
    /// the same budget over and over, so that pause is a real stop.
    #[test]
    fn a_deadline_does_not_continue_itself() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "sửa lỗi parser");
        let plain = end_at(&mut harness, PauseReason::Deadline);
        assert!(
            plain.contains(&"[run] paused: deadline reached".to_owned()),
            "{plain:#?}"
        );
        assert!(
            !plain
                .iter()
                .any(|line| line.contains("continuing automatically")),
            "{plain:#?}"
        );
        assert_eq!(submissions(&harness).len(), 1);
    }

    /// Ctrl-C means stop. A turn that ends at a bound right after the user canceled must
    /// not start another one behind their back.
    #[test]
    fn ctrl_c_stops_the_app_continuing_by_itself() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "sửa lỗi parser");
        let _ = harness.controller.handle_key(Key::Interrupt);
        let plain = end_at(&mut harness, PauseReason::StepLimit);
        assert!(
            !plain
                .iter()
                .any(|line| line.contains("continuing automatically")),
            "{plain:#?}"
        );
        assert_eq!(submissions(&harness).len(), 1);
    }

    fn end_done(harness: &mut Bench) -> Vec<String> {
        harness
            .events
            .send(SessionEvent::RunTerminal {
                outcome: RunOutcome::Done,
            })
            .expect("terminal event");
        effects_to_plain(&harness.controller.pump_events())
    }

    #[allow(
        clippy::option_option,
        reason = "none logged, a paused goal, and a set goal are three different answers"
    )]
    fn last_goal(harness: &Bench) -> Option<Option<String>> {
        harness.port.goals.lock().expect("goal log").last().cloned()
    }

    /// prime-agent's persistent goals: a turn that ends is not a goal that is done. The
    /// app carries the goal into the next turn until the model calls `goal_complete`.
    #[test]
    fn a_goal_continues_across_turns_until_the_model_completes_it() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "/goal ship the parser fix");
        assert_eq!(submissions(&harness), ["Goal: ship the parser fix"]);
        assert_eq!(
            last_goal(&harness),
            Some(Some("ship the parser fix".to_owned()))
        );

        let plain = end_done(&mut harness);
        assert!(
            plain
                .iter()
                .any(|line| line.contains("goal not complete yet; continuing (1 of")),
            "{plain:#?}"
        );
        assert_eq!(submissions(&harness).len(), 2);
        assert!(submissions(&harness)[1].contains("goal_complete"));

        harness
            .events
            .send(SessionEvent::GoalCompleted {
                summary: "fixed and tested".to_owned(),
            })
            .expect("goal event");
        let plain = end_done(&mut harness);
        assert!(
            plain
                .iter()
                .any(|line| line.contains("goal complete: fixed and tested")),
            "{plain:#?}"
        );
        assert_eq!(submissions(&harness).len(), 2, "a finished goal stops");
        assert_eq!(last_goal(&harness), Some(None));
    }

    /// The goal's own budget bounds it: a model that never finishes pauses, it does
    /// not loop forever.
    #[test]
    fn a_goal_pauses_when_its_budget_is_spent() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "/goal never done");
        for _ in 0..super::super::goal::DEFAULT_GOAL_CONTINUATIONS {
            let _ = end_done(&mut harness);
        }
        let sent = submissions(&harness).len();
        assert_eq!(
            sent,
            1 + super::super::goal::DEFAULT_GOAL_CONTINUATIONS as usize
        );
        let plain = end_done(&mut harness);
        assert!(
            plain.iter().any(|line| line.contains("goal paused after")),
            "{plain:#?}"
        );
        assert_eq!(submissions(&harness).len(), sent);
    }

    /// Ctrl-C and `/goal pause` stop the goal; `/goal resume` picks it up; `/goal clear`
    /// forgets it.
    #[test]
    fn a_goal_can_be_paused_resumed_and_cleared() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "/goal refactor the store");
        let _ = harness.controller.handle_key(Key::Interrupt);
        assert_eq!(last_goal(&harness), Some(None));
        let _ = end_done(&mut harness);
        assert_eq!(submissions(&harness).len(), 1, "Ctrl-C pauses the goal");

        let _ = submit_text(&mut harness.controller, "/goal resume");
        assert_eq!(submissions(&harness).len(), 2);
        assert_eq!(
            last_goal(&harness),
            Some(Some("refactor the store".to_owned()))
        );
        let _ = end_done(&mut harness);
        assert_eq!(submissions(&harness).len(), 3);

        let _ = submit_text(&mut harness.controller, "/goal pause");
        let _ = end_done(&mut harness);
        assert_eq!(submissions(&harness).len(), 3, "a paused goal waits");

        let _ = submit_text(&mut harness.controller, "/goal clear");
        assert_eq!(last_goal(&harness), Some(Some(String::new())));
        let plain = effects_to_plain(&submit_text(&mut harness.controller, "/goal"));
        assert!(
            plain.iter().any(|line| line.contains("no goal is set")),
            "{plain:#?}"
        );
    }

    /// prime-agent's `goal` and `compact` skills: a goal the model starts is carried
    /// like one the user set, and a compaction it asks for runs when the turn ends,
    /// before the goal continues.
    #[test]
    fn the_skills_start_a_goal_and_compact_after_the_turn() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "work on the release");
        harness
            .events
            .send(SessionEvent::GoalCreated {
                objective: "ship the release".to_owned(),
                token_budget: None,
            })
            .expect("goal event");
        harness
            .events
            .send(SessionEvent::CompactRequested {
                instructions: Some("keep the plan".to_owned()),
            })
            .expect("compact event");
        let plain = end_done(&mut harness);
        assert!(
            plain
                .iter()
                .any(|line| line.contains("goal set by the model: ship the release")),
            "{plain:#?}"
        );
        assert_eq!(
            last_goal(&harness),
            Some(Some("ship the release".to_owned()))
        );
        assert_eq!(
            submissions(&harness).last().map(String::as_str),
            Some("/compact keep the plan"),
            "the compaction runs first"
        );
        let _ = end_done(&mut harness);
        assert!(
            submissions(&harness)
                .last()
                .is_some_and(|text| text.contains("goal_complete")),
            "then the goal continues: {:?}",
            submissions(&harness)
        );
    }

    /// prime-agent's RLM heartbeats: one that comes due while the app is idle runs
    /// at once; while a turn runs, a `steer` heartbeat goes through the steer inbox
    /// and a `follow_up` one waits for the turn to end.
    #[test]
    fn heartbeats_run_when_due_and_respect_their_delivery_mode() {
        use crate::interactive::heartbeat::{Delivery, Due};
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let due = |text: &str, delivery| Due {
            text: text.to_owned(),
            delivery,
        };
        harness
            .port
            .heartbeats_due
            .lock()
            .expect("heartbeats")
            .push(due(
                "[heartbeat: every 5m run#1]\n\ncheck ci",
                Delivery::Steer,
            ));
        let _ = harness.controller.pump_events();
        assert_eq!(
            submissions(&harness).len(),
            1,
            "an idle app runs it at once"
        );

        harness
            .port
            .heartbeats_due
            .lock()
            .expect("heartbeats")
            .extend([
                due("steer me", Delivery::Steer),
                due("after the turn", Delivery::FollowUp),
            ]);
        let plain = effects_to_plain(&harness.controller.pump_events());
        assert_eq!(
            harness.port.steers.lock().expect("steers").as_slice(),
            ["steer me".to_owned()]
        );
        assert!(
            plain.iter().any(|line| line.contains("heartbeat steered")),
            "{plain:#?}"
        );
        assert_eq!(submissions(&harness).len(), 1, "the follow-up waits");
        let _ = end_done(&mut harness);
        assert_eq!(
            submissions(&harness).last().map(String::as_str),
            Some("after the turn")
        );
    }

    /// Q03: what reached the turn's inbox after its last step runs next - a
    /// child's message as a report, the user's steer first in the steering lane.
    #[test]
    fn q03_unread_messages_run_after_the_turn() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "ask a child");
        for (text, verbatim) in [
            ("[agent-message from child:c]\n\nlate", true),
            ("and the lexer", false),
        ] {
            harness
                .events
                .send(SessionEvent::UnreadMessage {
                    text: text.to_owned(),
                    verbatim,
                })
                .expect("unread");
        }
        done(&mut harness);
        assert_eq!(
            submissions(&harness).last().map(String::as_str),
            Some("and the lexer")
        );
        done(&mut harness);
        assert_eq!(
            submissions(&harness).last().map(String::as_str),
            Some("[agent-message from child:c]\n\nlate")
        );
    }

    /// Q11: every pattern of `/scoped-models` reaches the port, not the first alone.
    #[test]
    fn q11_scoped_models_takes_every_pattern() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(
            &mut harness.controller,
            "/scoped-models deepseek/* openai/gpt-5*:high",
        );
        assert_eq!(
            *harness.port.scopes.lock().expect("scopes"),
            [Some("deepseek/* openai/gpt-5*:high".to_owned())]
        );
    }

    /// Q14: with a gate, each finished turn runs it; a failure continues the run
    /// with prime-agent's gate prompt, and a pass ends it.
    #[test]
    fn q14_autonomous_continues_until_the_gate_passes() {
        use crate::interactive::autonomous::{GateFailure, GateResult, GateState};
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let plain = effects_to_plain(&submit_text(
            &mut harness.controller,
            "/autonomous on --gate \"cargo test\"",
        ))
        .join("\n");
        assert!(plain.contains("[autonomous-status: on]"), "{plain}");
        assert!(plain.contains("Gates: \"cargo test\"."), "{plain}");
        let _ = submit_text(&mut harness.controller, "fix the parser");
        assert_eq!(submissions(&harness), ["fix the parser"]);

        done(&mut harness);
        assert_eq!(
            harness.port.gate_jobs.lock().expect("gates").len(),
            1,
            "the gate runs after the turn"
        );
        assert_eq!(
            submissions(&harness).len(),
            1,
            "nothing continues before the verdict"
        );

        let failed = GateState {
            last_failure: Some(GateFailure {
                command: "cargo test".to_owned(),
                attempt: 1,
                exit_text: "exited 101".to_owned(),
                output: "1 failed".to_owned(),
            }),
            ..GateState::default()
        };
        harness
            .events
            .send(SessionEvent::GatesChecked {
                result: GateResult::Failed,
                state: failed.clone(),
            })
            .expect("verdict");
        let plain = effects_to_plain(&harness.controller.pump_events()).join("\n");
        assert!(plain.contains("autonomous: continuing (1/3)"), "{plain}");
        let continued = submissions(&harness);
        assert_eq!(continued.len(), 2);
        assert!(continued[1].starts_with(
            "[autonomous-continuation: gate-failed]\n\nAutonomous quality gate failed (attempt 1/3): `cargo test` exited 101.\n\nOutput:\n1 failed\n"
        ));

        done(&mut harness);
        let jobs = harness.port.gate_jobs.lock().expect("gates").clone();
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[1].state, failed, "the gate remembers the last failure");
        harness
            .events
            .send(SessionEvent::GatesChecked {
                result: GateResult::Passed,
                state: GateState::default(),
            })
            .expect("verdict");
        let plain = effects_to_plain(&harness.controller.pump_events()).join("\n");
        assert!(
            plain.contains("autonomous: quality gates passed"),
            "{plain}"
        );
        assert_eq!(submissions(&harness).len(), 2, "a pass ends the run");
    }

    /// Q14: without gates the run continues until a budget is spent, and a turn
    /// that failed or was stopped is not carried on.
    #[test]
    fn q14_autonomous_without_gates_stops_at_its_limit() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(
            &mut harness.controller,
            "/autonomous on --max-continuations 2",
        );
        let _ = submit_text(&mut harness.controller, "work");
        done(&mut harness);
        done(&mut harness);
        assert_eq!(submissions(&harness).len(), 3);
        assert!(
            submissions(&harness)[1]
                .starts_with("[autonomous-continuation]\n\nNo human input is available")
        );
        harness
            .events
            .send(SessionEvent::RunTerminal {
                outcome: RunOutcome::Done,
            })
            .expect("terminal");
        let plain = effects_to_plain(&harness.controller.pump_events()).join("\n");
        assert!(
            plain.contains("autonomous: stopped; maxContinuations reached"),
            "{plain}"
        );
        assert_eq!(submissions(&harness).len(), 3);

        let _ = submit_text(&mut harness.controller, "/autonomous on");
        let _ = submit_text(&mut harness.controller, "again");
        harness
            .events
            .send(SessionEvent::RunTerminal {
                outcome: RunOutcome::Canceled,
            })
            .expect("terminal");
        let _ = harness.controller.pump_events();
        assert_eq!(
            submissions(&harness).last().map(String::as_str),
            Some("again"),
            "a stopped turn is not carried on"
        );
        let status =
            effects_to_plain(&submit_text(&mut harness.controller, "/autonomous")).join("\n");
        // A limit not named again keeps its last value, as prime-agent's
        // `setAutonomousLimits` does; only the counts start over.
        assert!(
            status.contains("Continuations: 0/2. Turns: 1/unlimited."),
            "{status}"
        );
    }

    /// Q14: a goal and autonomous mode do not drive the conversation together;
    /// turning autonomous mode off stops gates that are running.
    #[test]
    fn q14_goal_and_autonomous_take_turns() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "/autonomous on --gate true");
        let _ = submit_text(&mut harness.controller, "work");
        done(&mut harness);
        assert_eq!(harness.port.gate_jobs.lock().expect("gates").len(), 1);
        let plain =
            effects_to_plain(&submit_text(&mut harness.controller, "/goal ship it")).join("\n");
        assert!(plain.contains("autonomous mode turned off"), "{plain}");
        assert_eq!(*harness.port.gate_cancels.lock().expect("cancels"), 1);
        done(&mut harness);
        let plain =
            effects_to_plain(&submit_text(&mut harness.controller, "/autonomous on")).join("\n");
        assert!(
            plain.contains("goal paused: autonomous mode drives this conversation now"),
            "{plain}"
        );
        let error = effects_to_plain(&submit_text(
            &mut harness.controller,
            "/autonomous on --nope 1",
        ))
        .join("\n");
        assert!(
            error.contains("Unknown autonomous budget flag: --nope"),
            "{error}"
        );
    }

    /// A resumed conversation brings its goal back paused: nothing runs until asked.
    #[test]
    fn a_restored_goal_waits_for_resume() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        harness
            .events
            .send(SessionEvent::GoalRestored {
                objective: "finish the docs".to_owned(),
            })
            .expect("goal event");
        let plain = effects_to_plain(&harness.controller.pump_events());
        assert!(
            plain
                .iter()
                .any(|line| line.contains("has a goal: finish the docs (paused")),
            "{plain:#?}"
        );
        assert!(submissions(&harness).is_empty());
        let _ = submit_text(&mut harness.controller, "/goal resume");
        assert_eq!(submissions(&harness).len(), 1);
    }

    /// `/status` answers "why did it stop, and what do I raise?": the bounds in force are
    /// the numbers an operator has to know to move them.
    #[test]
    fn status_reports_the_bounds_and_the_continuation_budget() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        let plain = effects_to_plain(&submit_text(&mut harness.controller, "/status"));
        let line = plain
            .iter()
            .find(|line| line.starts_with("Bounds:"))
            .unwrap_or_else(|| panic!("no bounds line in {plain:#?}"));
        assert!(line.contains("steps 8"), "{line}");
        assert!(line.contains("tool calls 16"), "{line}");
        assert!(
            line.contains(&format!("{DEFAULT_CONTINUATIONS} automatic continuation")),
            "{line}"
        );
    }

    /// `/attach` names a file in the message, and sending that message attaches it.
    ///
    /// The point of the command is that it needs no clipboard and no terminal support, so
    /// this drives it the way a user does: type the command, press Enter, then write the
    /// message. The notice that comes back says the file is named in the message; the real
    /// read happens when that message is sent.
    #[test]
    fn t_attach_names_a_file_in_the_message_and_the_turn_reads_it() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();
        // A name with a space, because that is the case the quoting exists for.
        let spaced = harness.temp.path().join("my notes.txt");
        std::fs::write(&spaced, b"the parser drops the last line\n").expect("fixture file");
        let quoted = format!("\"{}\"", spaced.display());

        type_text(&mut harness.controller, &format!("/attach {quoted}"));
        let plain = effects_to_plain(&harness.controller.handle_key(Key::Enter));
        assert!(
            plain
                .iter()
                .any(|line| line.contains("1 file(s) named in the message")),
            "{plain:#?}"
        );
        assert_eq!(harness.controller.display_buffer(), format!("{quoted} "));

        // The message that follows carries the path, so the turn reads the file.
        let _ = harness.controller.handle_key(Key::Newline);
        type_text(&mut harness.controller, "what is wrong?");
        let submitted = submissions(&harness);
        assert!(submitted.is_empty(), "nothing was sent yet: {submitted:?}");
        let _ = harness.controller.handle_key(Key::Enter);
        let submitted = submissions(&harness);
        assert_eq!(submitted.len(), 1, "{submitted:?}");
        assert!(
            submitted[0].contains("what is wrong?")
                && submitted[0].contains(&spaced.display().to_string()),
            "{submitted:?}"
        );
    }

    /// A path that is not there is refused where the user can still fix it.
    #[test]
    fn t_attach_refuses_a_path_that_is_not_there_and_explains_itself() {
        let mut harness = bench(true);
        let _ = harness.controller.boot_lines();

        let plain = effects_to_plain(&submit_text(
            &mut harness.controller,
            "/attach definitely-not-here.txt",
        ));
        assert!(
            plain
                .iter()
                .any(|line| line.contains("no such file") && line.starts_with("[error]")),
            "{plain:#?}"
        );
        assert_eq!(
            harness.controller.display_buffer(),
            "",
            "a refused path must not be pasted into the message"
        );

        // No argument: the command says how it is used instead of doing nothing.
        let plain = effects_to_plain(&submit_text(&mut harness.controller, "/attach"));
        assert!(
            plain
                .iter()
                .any(|line| line.contains("/attach <path>") && line.contains("quoted")),
            "{plain:#?}"
        );
    }

    /// A path the composer receives is quoted exactly once, whichever road it came in by.
    ///
    /// A paste with no bitmap on the clipboard pastes the path it holds, so a file copied
    /// in Explorer becomes an attachment without any terminal support for it. The clipboard
    /// itself is machine state this crate cannot set, so what is asserted here is the text
    /// that branch hands the composer.
    #[test]
    fn t_a_pasted_path_is_quoted_once() {
        assert_eq!(
            super::quote_for_composer(r"C:\work\a b\shot.png"),
            "\"C:\\work\\a b\\shot.png\""
        );
        assert_eq!(
            super::quote_for_composer(r"C:\work\shot.png"),
            r"C:\work\shot.png"
        );
        assert_eq!(
            super::quote_for_composer("\"C:\\work\\shot.png\""),
            "\"C:\\work\\shot.png\"",
            "an already quoted path is not quoted twice"
        );
    }

    /// prime-agent's `/fast` toggles the priority tier, `/tier` shows or sets the
    /// tier, and the status line marks priority as `fast`.
    #[test]
    fn fast_and_tier_follow_prime_agent() {
        let mut harness = bench(true);
        let on = effects_to_plain(&submit_text(&mut harness.controller, "/fast")).join(
            "
",
        );
        assert!(on.contains("Fast mode: on"), "{on}");
        assert_eq!(
            harness.controller.ui_state().service_tier.as_deref(),
            Some("fast")
        );
        let off = effects_to_plain(&submit_text(&mut harness.controller, "/fast")).join(
            "
",
        );
        assert!(off.contains("Fast mode: off"), "{off}");
        assert_eq!(harness.controller.ui_state().service_tier, None);
        let shown = effects_to_plain(&submit_text(&mut harness.controller, "/tier")).join(
            "
",
        );
        assert!(
            shown.contains("Service tier: default (available: default, priority, auto)"),
            "{shown}"
        );
        let refused = effects_to_plain(&submit_text(&mut harness.controller, "/tier flex")).join(
            "
",
        );
        assert!(
            refused.contains("Service tier 'flex' is not available for the current model"),
            "{refused}"
        );
        let auto = effects_to_plain(&submit_text(&mut harness.controller, "/tier auto")).join(
            "
",
        );
        assert!(auto.contains("Service tier: auto"), "{auto}");
        assert_eq!(
            harness.controller.ui_state().service_tier.as_deref(),
            Some("auto")
        );
    }

    /// prime-agent's `/rlm-max-depth [<int> [--global]]`, and its usage line.
    #[test]
    fn rlm_max_depth_reads_prime_agents_arguments() {
        let mut harness = bench(true);
        for line in [
            "/rlm-max-depth",
            "/rlm-max-depth 1",
            "/rlm-max-depth 0 --global",
        ] {
            submit_text(&mut harness.controller, line);
        }
        assert_eq!(
            *harness.port.depths.lock().expect("depths"),
            [None, Some((1, false)), Some((0, true))]
        );
        let usage = effects_to_plain(&submit_text(&mut harness.controller, "/rlm-max-depth two"))
            .join("\n");
        assert!(
            usage.contains("Usage: /rlm-max-depth [<non-negative integer> [--global]]"),
            "{usage}"
        );
        let over =
            effects_to_plain(&submit_text(&mut harness.controller, "/rlm-max-depth 5")).join("\n");
        assert!(over.contains("0..=2"), "{over}");
        assert_eq!(harness.port.depths.lock().expect("depths").len(), 3);
    }

    #[test]
    fn g08_clear_starts_a_new_session_and_clears_only_the_viewport() {
        let mut harness = bench(true);
        let effects = submit_text(&mut harness.controller, "/clear");

        assert_eq!(*harness.port.resumes.lock().expect("resume log"), [None]);
        assert!(effects.contains(&Effect::ClearViewport));
        let plain = effects_to_plain(&effects).join("\n");
        assert!(
            plain.contains("earlier output remains in scrollback"),
            "{plain}"
        );
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::Exit(_)))
        );
    }

    #[test]
    fn g08_session_commands_are_listed_and_not_reported_as_unknown() {
        for command in ["/undo", "/export", "/copy", "/hooks"] {
            assert!(
                SLASH_COMMANDS.iter().any(|entry| entry.name == command),
                "{command} is missing from the help and suggestion table"
            );
        }
    }

    /// Reasoning arrives token by token; it is one row, not one row per token.
    #[test]
    fn reasoning_deltas_become_one_row() {
        let mut harness = tui_bench(true);
        for token in ["Let", " me", " keep", " it", " short"] {
            harness
                .events
                .send(SessionEvent::ThinkingDelta {
                    text: token.to_owned(),
                })
                .expect("thinking");
        }
        harness
            .events
            .send(SessionEvent::TextDelta {
                text: "answer".to_owned(),
            })
            .expect("text");
        let effects = harness.controller.pump_events();
        let thinking = effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Thinking(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(thinking, ["Let me keep it short"]);
    }

    /// DeepSeek-style frames carry `"content": ""` next to every reasoning token.
    /// Measured in the TUI (ctrl+o): each empty answer delta committed the
    /// reasoning gathered so far, so the trace printed one word per row.
    #[test]
    fn empty_answer_deltas_do_not_split_reasoning() {
        let mut harness = tui_bench(true);
        for token in ["Let", " me", " keep", " it", " short"] {
            for event in [
                SessionEvent::TextDelta {
                    text: String::new(),
                },
                SessionEvent::ThinkingDelta {
                    text: token.to_owned(),
                },
            ] {
                harness.events.send(event).expect("stream event");
            }
        }
        harness
            .events
            .send(SessionEvent::TextDelta {
                text: "answer".to_owned(),
            })
            .expect("text");
        let effects = harness.controller.pump_events();
        let thinking = effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Thinking(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(thinking, ["Let me keep it short"]);
    }

    #[test]
    fn interleaved_reasoning_keeps_streamed_answer_together() {
        let mut harness = tui_bench(true);
        let mut effects = Vec::new();
        for event in [
            SessionEvent::TextDelta { text: "T".into() },
            SessionEvent::ThinkingDelta {
                text: "checking the workspace".into(),
            },
            SessionEvent::TextDelta {
                text: "ôi sẽ tiếp tục nâng cấp TUI.".into(),
            },
        ] {
            harness.events.send(event).expect("stream event");
            effects.extend(harness.controller.pump_events());
        }
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::Stream(_))),
            "reasoning must not commit an unfinished answer: {effects:?}"
        );
        assert_eq!(
            harness.controller.ui_state().live_text,
            "Tôi sẽ tiếp tục nâng cấp TUI."
        );
        harness
            .events
            .send(SessionEvent::RunTerminal {
                outcome: RunOutcome::Done,
            })
            .unwrap();
        effects.extend(harness.controller.pump_events());
        let answers: Vec<_> = effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Stream(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(answers, ["Tôi sẽ tiếp tục nâng cấp TUI."]);
        assert!(effects.contains(&Effect::Thinking("checking the workspace".into())));
    }

    #[test]
    fn g09_bell_event_becomes_a_tui_effect() {
        let mut harness = tui_bench(true);
        harness.events.send(SessionEvent::Bell).expect("bell event");
        let effects = harness.controller.pump_events();
        assert!(effects.contains(&Effect::Bell), "{effects:#?}");
    }

    #[test]
    fn g08_copy_writes_the_last_answer_to_clipboard() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "question");
        harness
            .events
            .send(SessionEvent::TextDelta {
                text: "final answer".to_owned(),
            })
            .expect("answer delta");
        harness
            .events
            .send(SessionEvent::RunTerminal {
                outcome: RunOutcome::Done,
            })
            .expect("turn finished");
        let _ = harness.controller.pump_events();

        let effects = submit_text(&mut harness.controller, "/copy");

        assert!(
            effects.contains(&Effect::Copy("final answer".to_owned())),
            "{effects:#?}"
        );
    }

    #[test]
    fn a_bare_model_command_opens_the_model_menu() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        harness.controller.editor.set_argument_options(
            "/model",
            vec![("openai/gpt-5".to_owned(), "GPT-5".to_owned())],
        );
        let _ = submit_text(&mut harness.controller, "/model");
        assert_eq!(harness.controller.editor.text(), "/model ");
        assert!(!harness.controller.editor.suggestions().is_empty());
    }

    #[test]
    fn ctrl_g_hands_the_draft_out_and_takes_the_edit_back() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        type_text(&mut harness.controller, "draft");
        assert_eq!(
            harness.controller.handle_key(Key::ExternalEditor),
            vec![Effect::EditExternally("draft".to_owned())]
        );
        let _ = harness
            .controller
            .handle_key(Key::SetDraft("edited\nlines".to_owned()));
        assert_eq!(harness.controller.editor.text(), "edited\nlines");
    }
}
