//! Interactive event vocabulary for `HA_LAUNCH` H03 and `HA_TUI` T02.
//!
//! Terminal input is normalized into the Key enum so the editor, the controller
//! and their tests never depend on a terminal library type. Backend work is
//! normalized into `SessionEvent`; H04 replaces the staged producer of those
//! events with the application service and this vocabulary stays.
//!
//! T02 adds the second half of the vocabulary: `HistoryItem` is what the
//! controller emits instead of a bare string, `UiState` is the snapshot the TUI
//! viewport draws, and `Modal` is the temporary panel that replaces the live
//! block. The plain renderer keeps working from `HistoryItem::plain_lines()`,
//! which is required to reproduce the pre-T02 strings exactly.

use std::time::{Duration, Instant};

use harness_types::InputId;
use serde_json::Value;

use super::input::SlashCommand;

/// One normalized terminal input event.
///
/// The T03 key set is defined here in T02 so the vocabulary is complete in one
/// place; the editor starts consuming the new variants in T03.
#[allow(dead_code, reason = "T03 consumes the composer keys")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Key {
    Char(char),
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    Up,
    Down,
    /// Ctrl-A: start of the current line.
    LineStart,
    /// Ctrl-E: end of the current line.
    LineEnd,
    /// Ctrl-U: erase to the start of the current line.
    EraseToLineStart,
    /// Ctrl-W: erase one word before the cursor.
    EraseWord,
    /// Tab: complete a slash command.
    Tab,
    /// Escape: dismiss a suggestion or close a modal; during a run it cancels it.
    Esc,
    /// Ctrl-L: repaint the viewport without touching the scrollback.
    Redraw,
    PageUp,
    PageDown,
    Enter,
    /// Ctrl-J or Alt+Enter: insert a line break instead of submitting.
    ///
    /// A combination the terminal reports distinctly is required here: on Windows
    /// a console cannot tell Shift+Enter from Enter, so the plan's "keys that are
    /// actually tested" rule means Ctrl-J is the documented multiline key.
    /// Measured in `HA_TUI` T01 on this console: a line feed arrives as
    /// `Enter + CONTROL` and Alt+Enter as `Enter + ALT`, so both reach this
    /// variant and neither is mistaken for a submit.
    Newline,
    /// Ctrl-C: cancel an active run, or clear an idle prompt.
    Interrupt,
    /// Ctrl-D on an empty prompt: leave the app.
    EndOfInput,
    /// Bracketed paste; embedded newlines must never become separate commands.
    Paste(String),
    /// Ctrl-V: take a bitmap off the clipboard, because no terminal sends one as text.
    ///
    /// A terminal that binds Ctrl-V itself never forwards the key, which is why
    /// `/image` runs the same code — the documented, terminal-independent way in.
    PasteImage,
    Resize {
        columns: u16,
        rows: u16,
    },
    Unknown,
}

/// Phase of the interactive app, as described in the `HA_LAUNCH` plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppPhase {
    Booting,
    Ready,
    SetupRequired,
    Running,
    /// A gated action is waiting for the user's answer; the run is still active.
    WaitingApproval,
    /// A durable `ask_user` question awaits a separate user input.
    WaitingInput,
    /// An MCP server is paused until the operator answers an elicitation.
    WaitingMcpInput,
    Canceling,
    Closed,
}

impl AppPhase {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Booting => "booting",
            Self::Ready => "ready",
            Self::SetupRequired => "setup_required",
            Self::Running => "running",
            Self::WaitingApproval => "waiting_approval",
            Self::WaitingInput => "waiting_input",
            Self::WaitingMcpInput => "waiting_mcp_input",
            Self::Canceling => "canceling",
            Self::Closed => "closed",
        }
    }

    /// A run is active, so a second input must not be admitted.
    #[must_use]
    pub const fn has_active_run(self) -> bool {
        matches!(
            self,
            Self::Running | Self::WaitingApproval | Self::WaitingMcpInput | Self::Canceling
        )
    }
}

/// Which bound stopped a turn.
///
/// A bound is a safety net for a loop that has gone wrong, not the task's budget, so
/// the app distinguishes the bounds it may carry on past from the one it may not: the
/// deadline is a real stop, while a step or tool-call bound can be continued.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PauseReason {
    StepLimit,
    ToolLimit,
    Deadline,
    /// The goal continued without new evidence.
    NoProgress,
    /// The goal continuation bound was reached.
    GoalLimit,
    /// The goal's budget cannot fund another step.
    BudgetExhausted,
}

impl PauseReason {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::StepLimit => "step limit reached",
            Self::ToolLimit => "tool-call limit reached",
            Self::Deadline => "deadline reached",
            Self::NoProgress => "no progress across continuations",
            Self::GoalLimit => "goal continuation limit reached",
            Self::BudgetExhausted => "budget exhausted",
        }
    }

    /// Whether the app may continue this turn on its own.
    ///
    /// The step and tool-call bounds count work in progress; carrying on keeps the
    /// task moving. The deadline is wall-clock time already spent, so continuing it
    /// by itself would spend the same budget again and again. A goal that stopped
    /// for no progress, a goal bound or a spent budget is a decision for the user:
    /// continuing it by itself would repeat the thing that just stopped.
    #[must_use]
    pub fn is_continuable(self) -> bool {
        matches!(self, Self::StepLimit | Self::ToolLimit)
    }
}

/// How one user turn ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunOutcome {
    Done,
    /// The turn stopped at a bound — steps, tool calls or the deadline — rather than
    /// breaking. Everything it did is durable and the conversation continues, so
    /// calling it a failure told the user their work was lost when it was not.
    Paused(PauseReason),
    /// The run paused on a durable question; the id is printed so the operator
    /// can answer it after a restart.
    WaitingInput {
        question_id: Option<String>,
    },
    /// The goal waits on something outside the host. The host does not poll.
    ExternalWait,
    /// The run stopped with an explicit reason that is not success and not a
    /// transient bound: a detected loop or an unverifiable answer.
    Blocked(String),
    Failed(String),
    Canceled,
}

/// Whether output from an interactive shell prefix is sent with the next
/// message or stays visible only in the local transcript.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellPrefixMode {
    AttachToNextMessage,
    DisplayOnly,
}

impl RunOutcome {
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Done => "done".to_owned(),
            Self::Paused(reason) => format!("paused: {}", reason.label()),
            Self::WaitingInput { question_id } => match question_id {
                Some(id) => format!("waiting for input: {id}"),
                None => "waiting for input".to_owned(),
            },
            Self::ExternalWait => "waiting on an external system".to_owned(),
            Self::Blocked(reason) => format!("blocked: {reason}"),
            Self::Failed(reason) => format!("failed: {reason}"),
            Self::Canceled => "canceled".to_owned(),
        }
    }
}

/// One persisted session the user can continue from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionCandidate {
    pub session_id: String,
    pub task_id: String,
    /// Short, secret-free description for the list.
    pub detail: String,
}

/// How far one tool call has got.
///
/// A settled card carries the duration it took, so the TUI can show
/// `ok 12ms` / `failed 3.1s` and the plain writer can keep its old two lines. A
/// failure also carries the reason, because a card that only says `failed 962ms`
/// leaves the reader unable to tell a malformed call from a policy denial.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolState {
    /// The card is open; nothing has settled yet.
    Started,
    Ok {
        elapsed: Duration,
    },
    Failed {
        elapsed: Duration,
        /// Empty when the producer reported no reason.
        detail: String,
    },
}

impl ToolState {
    /// Whether the card has settled.
    #[allow(dead_code, reason = "T04 decides whether a card is updated in place")]
    #[must_use]
    pub const fn is_settled(&self) -> bool {
        !matches!(self, Self::Started)
    }
}

/// One entry of the conversation history.
///
/// The controller emits these instead of pre-rendered strings, so the TUI can
/// style a user turn differently from a tool card while the plain renderer keeps
/// printing exactly what it printed before T02.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HistoryItem {
    /// The one-time header printed at startup.
    ///
    /// The boot header is pushed as `Message` rows in T02; T04 renders the banner
    /// with its own styling.
    #[allow(dead_code, reason = "T04 styles the banner")]
    Banner { lines: Vec<String> },
    /// A submitted user request, stored without the prompt marker.
    User { text: String },
    /// A request the app sent by itself, to continue a turn a bound stopped.
    ///
    /// It is recorded apart from `User` on purpose: the reader has to be able to tell
    /// what they asked for from what the app said next on their behalf.
    Automatic { text: String },
    /// Model text for one turn.
    ///
    /// Streaming text is committed from the live block in T04; until then it goes
    /// to the scrollback through `Effect::Stream`.
    #[allow(dead_code, reason = "T04 commits streamed text as an item")]
    Assistant { text: String },
    /// Provider reasoning shown only in the dim TUI surface; plain transcript
    /// renderers deliberately have no representation for this item.
    Thinking { text: String },
    /// A tool card: started, then settled with a duration.
    Tool {
        name: String,
        summary: String,
        state: ToolState,
    },
    /// The end-of-turn summary line.
    Run {
        outcome: RunOutcome,
        steps: u32,
        tool_calls: u32,
        elapsed: Duration,
    },
    /// The store admitted one input; the plain line is `[run] accepted <id>`.
    RunAccepted { input_id: String },
    /// A failure the user has to know about.
    Error { message: String },
    /// A line printed exactly as given, with no prefix.
    ///
    /// Slash-command reference output (`/help`, `/status`, `/model`) uses this in
    /// plain mode, because the pre-T02 renderer printed those lines verbatim.
    Message { text: String },
    /// Something worth knowing that is not a failure; the plain line is
    /// `[info] <message>`.
    Notice { message: String },
    /// One gated action, recorded when the user answered it.
    Approval {
        action: String,
        summary: String,
        workspace: String,
        scope: String,
        request_id: String,
    },
    /// How one approval ended, recorded after the answer.
    ApprovalResolution { label: String, request_id: String },
    /// The session list, as plain lines.
    Sessions { lines: Vec<String> },
}

/// One temporary panel that replaces the live block instead of joining the
/// scrollback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Modal {
    /// A gated action waiting for the user's answer.
    Approval {
        request_id: String,
        action: String,
        summary: String,
        workspace: String,
        scope: String,
        /// When the pending request expires, so the panel can count down.
        expires_at: Instant,
        /// Whether the action only reads, so the panel offers the wider grant only
        /// where it would cover something.
        read_only: bool,
        /// Vertical scroll offset while a bounded diff preview is open.
        scroll: usize,
    },
    /// The session picker opened by `/resume` with no argument.
    Picker { items: Vec<String>, selected: usize },
    /// Git-aware workspace file picker opened by `@` in the composer.
    FilePicker { items: Vec<String>, selected: usize },
    /// A model question; numbered options select a value, and free text is accepted.
    Question {
        prompt: String,
        options: Vec<String>,
    },
    /// An MCP server requests form input or user confirmation of a URL action.
    McpElicitation {
        message: String,
        requested_schema: Option<Value>,
    },
    /// `/help`, `/status`, `/config`, `/model` and `/more` output.
    ///
    /// `scroll` is the row offset from the top, so a panel holding more than it can
    /// show is readable instead of clipped. It is clamped by the renderer, which is
    /// the only place that knows how many rows fit.
    Overlay {
        title: String,
        lines: Vec<String>,
        scroll: usize,
    },
}

/// The controller state the TUI viewport draws.
///
/// Pure data: the renderer never reaches back into the controller, so a frame is
/// a function of this snapshot plus the theme.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiState {
    pub phase: AppPhase,
    pub setup_required: bool,
    /// Exact actionable text resolved by bootstrap; the status bar must not
    /// invent a provider-specific substitute.
    pub setup_hint: Option<String>,
    pub header: Vec<String>,
    /// The composer buffer and where the cursor sits inside it, counted in
    /// characters.
    pub buffer: String,
    pub cursor: usize,
    /// Model text that has not been committed to the scrollback yet.
    pub live_text: String,
    /// The open tool card, if any: it updates in place until it settles.
    pub open_tool: Option<(String, String)>,
    pub modal: Option<Modal>,
    /// Whether the user allowed every gated action for the run in flight, so the
    /// status row can say the gate is open instead of leaving a silent widening of
    /// what runs without asking.
    pub granted_for_run: bool,
    /// One next user input held until the active run releases its session writer.
    pub queued_input: bool,
    /// The last submitted request, so the status bar can name it.
    pub last_request: Option<String>,
    /// When the active run started, for the elapsed clock.
    pub run_started_at: Option<Instant>,
    /// When the last run ended, for the `[run]` summary line.
    pub last_run_elapsed: Duration,
    pub steps: u32,
    pub max_steps: u32,
    pub tool_calls: u32,
    pub max_tool_calls: u32,
    /// Slash commands the composer is offering right now, in table order.
    ///
    /// The menu that draws these is not a modal: the composer keeps the focus and
    /// the draft stays visible. It is drawn only while the composer owns the
    /// keyboard, so a panel or picker that replaces it also takes the menu away.
    pub suggestions: Vec<&'static SlashCommand>,
    /// Which suggestion row carries the highlight.
    pub suggestion_selected: usize,
    /// Why the TUI is not in use, when the host fell back to the plain renderer.
    pub fallback_reason: Option<String>,
    /// How many ticks have passed, so the spinner animates without wall clock.
    pub tick: u64,
}

/// Events the controller consumes from the session port.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionEvent {
    Accepted {
        input_id: InputId,
    },
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        text: String,
    },
    CostUpdated {
        label: String,
    },
    /// A model step started; the status bar counts these.
    StepStarted {
        step: u32,
    },
    ToolStarted {
        name: String,
        summary: String,
    },
    ToolSettled {
        name: String,
        ok: bool,
        /// Measured by the producer that observed both boundaries. Carrying the
        /// value on the event avoids a second name-based lookup that races when
        /// the same tool is called more than once.
        elapsed: Duration,
        /// Why a failed call did not execute; empty when it succeeded.
        detail: String,
    },
    /// A gated action is waiting for the user's decision.
    ApprovalRequired {
        request_id: String,
        action: String,
        summary: String,
        rule_pattern: String,
        workspace: String,
        scope: String,
        /// When the gate stops waiting, so the panel can count down from the real
        /// deadline instead of a second hard-coded timeout.
        expires_at: Instant,
        /// Whether the action only reads, so the panel can offer the wider grant
        /// only where it means something. The gate decides what it may auto-approve
        /// from the same flag, so the offer and the behaviour cannot drift apart.
        read_only: bool,
    },
    /// Nobody answered the pending request in time: the gate refused it and the
    /// action was not executed.
    ApprovalExpired {
        request_id: String,
    },
    QuestionRequired {
        question_id: String,
        prompt: String,
        options: Vec<String>,
    },
    /// A server-to-client MCP request awaiting explicit operator input.
    McpElicitationRequired {
        request_id: String,
        server: String,
        message: String,
        requested_schema: Option<Value>,
        url: Option<String>,
    },
    /// The answer to a resume listing.
    SessionsListed {
        sessions: Vec<SessionCandidate>,
    },
    /// The conversation a resumed session continues, as the model will be sent it.
    ConversationRestored {
        /// (question, answer) pairs, oldest first.
        turns: Vec<(String, String)>,
        /// Turns older than the replay bound, which the model does not see.
        omitted: usize,
        /// Whether a compaction summary stands for turns before these.
        summarized: bool,
    },
    /// Read-only session diff result for the reference overlay.
    Reference {
        title: String,
        lines: Vec<String>,
    },
    /// Something the user should know that is not an error.
    Notice {
        message: String,
    },
    /// Emit BEL in the interactive terminal when the user configured it.
    Bell,
    ShellPrefixCompleted {
        command: String,
        output: String,
        attach_to_next_message: bool,
    },
    RunTerminal {
        outcome: RunOutcome,
    },
    RecoverableError {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::{AppPhase, RunOutcome, SessionEvent};
    use harness_types::InputId;
    use std::time::Duration;

    #[test]
    fn h03_phase_labels_and_active_run_are_explicit() {
        assert_eq!(AppPhase::SetupRequired.label(), "setup_required");
        assert!(AppPhase::Running.has_active_run());
        assert!(AppPhase::Canceling.has_active_run());
        assert!(AppPhase::WaitingApproval.has_active_run());
        assert_eq!(AppPhase::WaitingApproval.label(), "waiting_approval");
        assert!(!AppPhase::Ready.has_active_run());
        assert!(!AppPhase::Closed.has_active_run());
    }

    #[test]
    fn h03_terminal_outcomes_are_labelled_for_the_transcript() {
        assert_eq!(RunOutcome::Canceled.label(), "canceled");
        assert_eq!(RunOutcome::Done.label(), "done");
        assert!(
            RunOutcome::Failed("boom".to_owned())
                .label()
                .contains("boom")
        );

        let input_id = InputId::generate();
        let events = [
            SessionEvent::Accepted { input_id },
            SessionEvent::TextDelta {
                text: "hi".to_owned(),
            },
            SessionEvent::ToolStarted {
                name: "read_file".to_owned(),
                summary: "path=a.rs".to_owned(),
            },
            SessionEvent::ToolSettled {
                name: "read_file".to_owned(),
                ok: true,
                elapsed: Duration::from_millis(12),
                detail: String::new(),
            },
            SessionEvent::RunTerminal {
                outcome: RunOutcome::Done,
            },
            SessionEvent::RecoverableError {
                message: "connection pending".to_owned(),
            },
        ];
        assert_eq!(events.len(), 6, "every vocabulary item stays representable");
    }
}
