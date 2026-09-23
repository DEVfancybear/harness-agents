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
use super::events::{
    AppPhase, HistoryItem, Key, Modal, RunOutcome, SessionCandidate, SessionEvent, ToolState,
    UiState,
};
use super::input::{InputOutcome, LineEditor};
use super::service::{ApprovalDecision, SessionChannel, SessionPort, SubmitRequest};
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    /// Add one entry to the scrollback.
    History(HistoryItem),
    /// Append stream text exactly as given, for incremental output.
    Stream(String),
    /// TUI-only provider reasoning, excluded from plain transcript output.
    Thinking(String),
    /// Repaint the viewport (the composer, the live block and the status bar).
    Redraw,
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
    workspace: String,
    scope: String,
    expires_at: Instant,
    /// Whether the action only reads, so the panel offers the wider grant only
    /// where granting it means something.
    read_only: bool,
}

/// Interactive app state and its transitions.
pub struct InteractiveController {
    phase: AppPhase,
    setup_required: bool,
    header: Vec<String>,
    setup_hint: Option<String>,
    /// The launch context this session started from. Kept so `/key` can refresh
    /// the provider state from the same rules the launch used, instead of writing
    /// a second copy of them here.
    context: LaunchContext,
    /// The committed plain transcript, byte for byte what the old controller
    /// pushed. Kept as text so U20 can be asserted without a renderer.
    transcript: Vec<String>,
    /// The newest lines of the same transcript, so `/more` can show them again in a
    /// scrollable panel without the terminal's scrollback.
    recall: Vec<String>,
    editor: LineEditor,
    service: Box<dyn SessionPort>,
    channel: SessionChannel,
    /// Model text that has not been printed yet.
    pending_text: String,
    /// Number of line breaks in `pending_text`. It is maintained as deltas
    /// arrive, so the common streaming path does not rescan an ever-growing
    /// buffer on every token.
    pending_newlines: usize,
    pending_approval: Option<PendingApproval>,
    /// Whether the user allowed every gated action for the run now in flight.
    ///
    /// Mirrored here so the status row can say the gate is open, and cleared by
    /// `finish_run` so the grant covers one turn rather than the session: a new
    /// request never inherits the last turn's permission.
    granted_for_run: bool,
    /// Last resume listing, so a number can select from it.
    session_candidates: Vec<SessionCandidate>,
    /// The plain renderer prints slash-command output; the TUI opens an overlay.
    plain: bool,
    /// The tool card that is still open, so it settles in place.
    open_tool: Option<(String, String)>,
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
        Self {
            // The app boots before it can render; boot_lines performs the
            // transition once the header has actually been produced.
            phase: AppPhase::Booting,
            setup_required: context.setup_required,
            header,
            setup_hint: context.setup_hint(),
            context: context.clone(),
            transcript: Vec::new(),
            recall: Vec::new(),
            editor: LineEditor::new(),
            service,
            channel,
            pending_text: String::new(),
            pending_newlines: 0,
            pending_approval: None,
            granted_for_run: false,
            session_candidates: Vec::new(),
            plain,
            open_tool: None,
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
        }
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
            header: self.header.clone(),
            buffer: self.display_buffer(),
            cursor: self.editor.cursor(),
            live_text: self.pending_text.clone(),
            open_tool: self.open_tool.clone(),
            modal: self.modal(),
            granted_for_run: self.granted_for_run,
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
            });
        }
        if let Some(picker) = self.editor.picker() {
            return Some(Modal::Picker {
                items: picker.items().to_vec(),
                selected: picker.selected(),
            });
        }
        self.editor.overlay().map(|overlay| Modal::Overlay {
            title: overlay.title.clone(),
            lines: overlay.lines.clone(),
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
        let mut lines = vec![String::new()];
        lines.extend(self.header.iter().cloned());
        if let Some(hint) = &self.setup_hint {
            lines.push(hint.clone());
        }
        lines.push("Nhập yêu cầu. /help trợ giúp · /status chẩn đoán · /exit thoát".to_owned());
        lines
    }

    /// Apply one key.
    pub fn handle_key(&mut self, key: Key) -> Vec<Effect> {
        if key == Key::Redraw {
            return vec![Effect::Redraw];
        }
        // A modal owns the keyboard while it is open: Escape closes it, and the
        // picker takes the arrows, Enter and Escape.
        if self.pending_approval.is_some() {
            match key {
                Key::EndOfInput => return self.command("/exit"),
                Key::Esc => return Vec::new(),
                // In the TUI the panel says `y chạy · a cho phép cả lượt · n từ
                // chối`, so a single y, a or n answers immediately; anything else is
                // typed and answered with Enter, which is what plain mode has always
                // done. `a` is offered on every panel, because the grant it gives
                // covers every kind - including the command in front of the user.
                Key::Char('y' | 'Y') if !self.plain => return self.answer("y"),
                Key::Char('a' | 'A') if !self.plain => return self.answer("a"),
                Key::Char('n' | 'N') if !self.plain => return self.answer("n"),
                Key::Char(character) => return self.handle_key(Key::Paste(character.to_string())),
                _ => {}
            }
        } else if self.editor.overlay().is_some() {
            // An open panel owns the scroll keys: a long answer or a long help page
            // has to be readable to its first line, and the composer is not being
            // typed into while the panel is up. Escape still closes it, which is
            // what acceptance U09 asserts. Arrows are left to the editor, which is
            // what a panel opened from a draft must not steal.
            let scrolled = match key {
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
                    return vec![Effect::Redraw];
                }
                Key::Enter => {
                    let chosen = self.selected_candidate();
                    self.editor.close_picker();
                    return match chosen {
                        Some(session_id) => self.continue_session(&session_id),
                        None => vec![Effect::Redraw],
                    };
                }
                Key::EndOfInput => return self.command("/exit"),
                _ => {}
            }
        }
        // The suggestion menu does not own the keyboard - the composer keeps the
        // focus and the draft stays visible - but while it is drawn these keys act
        // on it, and only then. Enter completes a half-typed command instead of
        // submitting it; the next Enter runs it.
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
                Key::Tab | Key::Enter => {
                    self.editor.accept_suggestion();
                    return vec![Effect::Redraw];
                }
                _ => {}
            }
        }
        // A pasted screenshot: the key is handled here rather than by the editor
        // because there is no text to insert until the clipboard has been read.
        if key == Key::PasteImage {
            let mut effects = Vec::new();
            self.paste_image(&mut effects);
            return effects;
        }
        match self.editor.handle(key) {
            InputOutcome::Unchanged => Vec::new(),
            InputOutcome::Redraw | InputOutcome::CompleteSuggestion => vec![Effect::Redraw],
            InputOutcome::Exit => self.command("/exit"),
            InputOutcome::Interrupt => self.interrupt(),
            InputOutcome::Submit(text) => self.submit(text),
            InputOutcome::Secret(value) => self.save_key(&value),
        }
    }

    /// Move everything the session port produced into effects.
    pub fn pump_events(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        let events = self.channel.drain();
        if events.is_empty() {
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
            SessionEvent::TextDelta { text } => {
                self.pending_newlines = self
                    .pending_newlines
                    .saturating_add(text.bytes().filter(|byte| *byte == b'\n').count());
                self.pending_text.push_str(&text);
            }
            SessionEvent::ThinkingDelta { text } => {
                self.flush_stream(effects);
                effects.push(Effect::Thinking(text));
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
            SessionEvent::ToolStarted { name, summary } => {
                self.flush_stream(effects);
                self.tool_calls = self.tool_calls.saturating_add(1);
                self.open_tool = Some((name.clone(), summary.clone()));
                if self.plain {
                    self.push_history(
                        effects,
                        HistoryItem::Tool {
                            name,
                            summary,
                            state: ToolState::Started,
                        },
                    );
                }
            }
            SessionEvent::ToolSettled {
                name,
                ok,
                elapsed,
                detail,
            } => {
                self.flush_stream(effects);
                let summary = self
                    .open_tool
                    .take()
                    .filter(|(open_name, _)| open_name == &name)
                    .map_or_else(String::new, |(_, summary)| summary);
                let state = if ok {
                    ToolState::Ok { elapsed }
                } else {
                    ToolState::Failed { elapsed, detail }
                };
                self.push_history(
                    effects,
                    HistoryItem::Tool {
                        name,
                        // In the TUI this is the same card moving from the live
                        // region into scrollback. Plain mode already printed the
                        // summary on the Started row, so its settled row remains
                        // byte-identical to H03.
                        summary: if self.plain { String::new() } else { summary },
                        state,
                    },
                );
            }
            SessionEvent::ApprovalRequired {
                request_id,
                action,
                summary,
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
                self.pending_approval = Some(PendingApproval {
                    request_id,
                    action,
                    summary,
                    workspace,
                    scope,
                    expires_at,
                    read_only,
                });
                self.phase = AppPhase::WaitingApproval;
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
            SessionEvent::Notice { message } => {
                self.flush_stream(effects);
                self.push_history(effects, HistoryItem::Notice { message });
            }
            SessionEvent::RunTerminal { outcome } => {
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
                // After `finish_run`: a continuation is a new request, and the phase has
                // to be idle again before the service will accept one.
                effects.extend(self.maybe_continue(&outcome));
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
        }
    }

    fn submit(&mut self, text: String) -> Vec<Effect> {
        // A request the user typed starts a fresh continuation budget: the app may carry
        // this one on by itself when a bound stops it.
        self.continuations = 0;
        self.dispatch(text, false)
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
        // While a gated action waits, the next line is the answer — never a new
        // request that would run beside the pending one.
        if self.phase == AppPhase::WaitingApproval {
            return self.answer(&text);
        }
        if text.trim_start().starts_with('/') {
            // A leading space must not turn a command into chat text: `/key`
            // would otherwise be sent to the provider and stored in history.
            return self.command(&text);
        }
        if self.phase.has_active_run() {
            return vec![
                Effect::History(HistoryItem::Notice {
                    message: "a run is already active; wait for it or press Ctrl-C to cancel"
                        .to_owned(),
                }),
                Effect::Redraw,
            ];
        }
        // Ask the port at submission time, not at boot: the answer changes the
        // moment `/key` saves a credential, and a stale "setup required" would
        // refuse a request the app can now serve.
        if let Some(problem) = self.service.provider_problem() {
            return vec![
                Effect::History(HistoryItem::Error { message: problem }),
                Effect::Redraw,
            ];
        }
        let input_id = InputId::generate();
        self.fresh_run(Instant::now(), Some(text.clone()));
        self.service.submit(SubmitRequest {
            input_id,
            text: text.clone(),
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

    /// Start the accounting for a new turn.
    fn fresh_run(&mut self, now: Instant, request: Option<String>) {
        self.pending_text.clear();
        self.pending_newlines = 0;
        self.open_tool = None;
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
        self.open_tool = None;
    }

    fn interrupt(&mut self) -> Vec<Effect> {
        if self.phase.has_active_run() {
            self.service.cancel();
            // Ctrl-C also means "do not start another one": the budget is spent, so the
            // bound that ends the canceled turn is a real stop until the user speaks.
            self.continuations = self.max_continuations;
            self.phase = AppPhase::Canceling;
            return vec![
                Effect::History(HistoryItem::Notice {
                    message: "^C canceling the active run...".to_owned(),
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

    #[allow(clippy::too_many_lines)]
    fn command(&mut self, line: &str) -> Vec<Effect> {
        // The raw remainder matters for `/key`, whose argument may contain any
        // character. Commands that take one whitespace-delimited word keep reading
        // `argument`, so their behavior is unchanged.
        let trimmed = line.trim();
        let name = trimmed.split_whitespace().next().unwrap_or_default();
        let raw_argument = trimmed
            .get(name.len()..)
            .map(str::trim)
            .filter(|rest| !rest.is_empty());
        let argument = raw_argument.and_then(|rest| rest.split_whitespace().next());
        let mut effects = Vec::new();
        match name {
            "/exit" | "/quit" => {
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
                self.reference("/help", view::help_lines(), &mut effects);
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
            "/status" => {
                let mut lines = self.header.clone();
                lines.push(format!("Phase:   {}", self.phase.label()));
                // The project id is what memory is scoped by, and the app shows it
                // nowhere else: the projects directory is named after a digest, so
                // without this line there is nothing to hand to `ha memory --project-id`.
                lines.push(match self.service.project_id() {
                    Some(id) => format!("Project: {id} (memory scope for this workspace)"),
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
                    .filter(|line| line.starts_with("Config:") || line.starts_with("Data:"))
                    .cloned());
                self.reference("/config", lines, &mut effects);
            }
            "/cost" => {
                self.reference(
                    "/cost",
                    vec![format!("session cost: {}", self.service.cost_summary())],
                    &mut effects,
                );
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
            "/more" => {
                // The whole point is to read what the viewport clipped, so the panel
                // opens at the TOP: a reader who has to scroll before seeing the
                // beginning is exactly the problem this command exists to fix.
                self.reference("/more", self.recall_lines(), &mut effects);
            }
            "/key" => match raw_argument {
                Some(value) => {
                    // `line` is the raw submitted buffer, which is what the
                    // editor stored: forgetting the trimmed form would leave the
                    // key reachable through Up-arrow history.
                    self.editor.forget_submission(line);
                    return self.save_key(value);
                }
                None if self.phase.has_active_run() => {
                    self.push_history(
                        &mut effects,
                        HistoryItem::Notice {
                            message:
                                "cannot enter an API key while a run is active; cancel it first"
                                    .to_owned(),
                        },
                    );
                }
                None => {
                    self.editor.begin_secret_entry();
                    self.push_history(
                        &mut effects,
                        HistoryItem::Notice {
                            message:
                                "paste the API key and press Enter; it is masked, never stored in \
                                      history, and saved to the credential file so the next launch \
                                      starts configured. Esc cancels. /key <value> also works but \
                                      shows the value while it is typed"
                                    .to_owned(),
                        },
                    );
                }
            },
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
                    self.session_candidates.clear();
                    self.editor.close_picker();
                    self.push_history(
                        &mut effects,
                        HistoryItem::Notice {
                            message: "starting a fresh conversation; the earlier chain is no longer continued"
                                .to_owned(),
                        },
                    );
                }
            }
            "/model" => {
                // The backend label names the configured model or states that setup
                // is required; it never claims a model that was not resolved. The
                // provider facts follow it, so a surprising answer can be diagnosed
                // without leaving the app.
                if let Some(model) = argument {
                    if self.phase.has_active_run() {
                        self.push_history(&mut effects, HistoryItem::Notice {
                            message: "cannot change the model while a run is active; the running request keeps its selected model".to_owned(),
                        });
                    } else {
                        match self.service.set_model(model) {
                            Ok(message) => self.push_history(&mut effects, HistoryItem::Notice { message }),
                            Err(message) => self.push_history(&mut effects, HistoryItem::Error { message }),
                        }
                    }
                } else {
                    let label = self.service.label();
                    let mut lines = vec![format!("backend: {label}")];
                    lines.extend(self.service.provider_diagnostics());
                    self.reference("/model", lines, &mut effects);
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
                self.push_history(
                    &mut effects,
                    HistoryItem::Notice {
                        message: format!(
                            "unknown command {other}; /help lists what this revision supports"
                        ),
                    },
                );
            }
        }
        effects.push(Effect::Redraw);
        effects
    }

    /// Deliver reference output: as an overlay in the TUI, as history in plain
    /// mode.
    fn reference(&mut self, title: &str, lines: Vec<String>, effects: &mut Vec<Effect>) {
        if self.plain {
            for text in lines {
                self.push_history(effects, HistoryItem::Message { text });
            }
            return;
        }
        self.editor.open_overlay(title, lines);
    }

    /// Save an API key entered in the app, then make it effective at once.
    ///
    /// The value is written to the credential file and nowhere else: it is not
    /// pushed to history, not rendered, and not sent anywhere. The gate clears in
    /// the same step, so the next message is dispatched for real instead of being
    /// refused — a saved key that still needed a restart would be a trap.
    fn save_key(&mut self, value: &str) -> Vec<Effect> {
        let key = value.trim();
        if key.is_empty() {
            return vec![
                Effect::History(HistoryItem::Notice {
                    message: "no key was entered; nothing was saved".to_owned(),
                }),
                Effect::Redraw,
            ];
        }
        let source = match self.service.save_credential(key) {
            Ok(source) => source,
            Err(message) => {
                self.editor.cancel_secret();
                return vec![
                    Effect::History(HistoryItem::Error { message }),
                    Effect::Redraw,
                ];
            }
        };
        match self.activate_credential(source.clone()) {
            Ok(()) => {
                let mut effects = Vec::new();
                self.push_history(
                    &mut effects,
                    HistoryItem::Notice {
                        message: format!(
                            "API key saved to {}; the next message uses it, and the next launch starts configured. The value is never shown, logged or kept in history.",
                            source.describe()
                        ),
                    },
                );
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

    /// The session the picker currently highlights.
    fn selected_candidate(&self) -> Option<String> {
        let picker = self.editor.picker()?;
        let selected = picker.selected();
        self.session_candidates
            .get(selected)
            .map(|candidate| candidate.session_id.clone())
    }

    fn flush_stream(&mut self, effects: &mut Vec<Effect>) {
        if self.pending_text.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.pending_text);
        self.pending_newlines = 0;
        // One effect, two renderers: plain mode appends the text exactly as it
        // arrives (that is what makes the transcript byte-identical), and the TUI
        // commits it to the scrollback through the history renderer.
        effects.push(Effect::Stream(text.clone()));
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
        self.remember(lines);
        effects.push(Effect::History(item));
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
        const RECALL_LINES: usize = 500;
        self.recall.extend(lines);
        if self.recall.len() > RECALL_LINES {
            let excess = self.recall.len() - RECALL_LINES;
            self.recall.drain(..excess);
        }
    }

    /// The text `/more` shows: the recent transcript, newest last.
    fn recall_lines(&self) -> Vec<String> {
        let mut lines = self.recall.clone();
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

    fn finish_run(&mut self) {
        self.pending_approval = None;
        self.open_tool = None;
        self.phase = if self.setup_required {
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

#[cfg(test)]
mod tests {
    use super::{DEFAULT_CONTINUATIONS, EXIT_SUCCESS, Effect, InteractiveController, TurnBounds};
    use crate::interactive::bootstrap::{self, LaunchContext, LaunchRequest};
    use crate::interactive::events::{
        AppPhase, HistoryItem, Key, Modal, PauseReason, RunOutcome, SessionCandidate, SessionEvent,
        ToolState,
    };
    use crate::interactive::input::SLASH_COMMANDS;
    use crate::interactive::paths::{HostPlatform, LaunchEnvironment};
    use crate::interactive::service::{
        ApprovalDecision, FixtureService, SessionChannel, SessionPort, SubmitRequest,
    };
    use crate::interactive::view;
    use harness_types::InputId;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// Test port that records what the controller admitted.
    #[derive(Clone, Default)]
    struct RecordingPort {
        submissions: Arc<Mutex<Vec<String>>>,
        cancels: Arc<Mutex<u32>>,
        answers: Arc<Mutex<Vec<(String, ApprovalDecision)>>>,
        resumes: Arc<Mutex<Vec<Option<String>>>>,
        /// Every turn-wide grant the controller handed to the port, and every
        /// revocation, so a test can assert the grant is scoped to one turn.
        run_grants: Arc<Mutex<Vec<bool>>>,
        limits: TurnBounds,
    }

    impl SessionPort for RecordingPort {
        fn label(&self) -> String {
            "recording port".to_owned()
        }

        fn submit(&mut self, request: SubmitRequest) {
            self.submissions
                .lock()
                .expect("submission log")
                .push(request.text);
        }

        fn cancel(&mut self) {
            *self.cancels.lock().expect("cancel log") += 1;
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

        fn resume(&mut self, session_id: Option<String>) -> Result<(), String> {
            self.resumes.lock().expect("resume log").push(session_id);
            Ok(())
        }

        fn limits(&self) -> TurnBounds {
            self.limits
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
            key: &str,
        ) -> Result<crate::interactive::credentials::CredentialSource, String> {
            let path = crate::interactive::credentials::resolve_file(
                &crate::interactive::paths::LaunchEnvironment::default(),
                &self.data_dir,
            );
            let protection = crate::interactive::credentials::save(&path, key)
                .expect("the fixture credential directory is writable");
            Ok(crate::interactive::credentials::CredentialSource::File { path, protection })
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

    fn saved_credential_path(context: &LaunchContext) -> std::path::PathBuf {
        crate::interactive::credentials::resolve_file(
            &crate::interactive::paths::LaunchEnvironment::default(),
            &context.paths.data_dir,
        )
    }

    /// K01: the whole `/key` chain, driven by keys rather than by calling the
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

    /// A bare `/key` opens masked entry, and the prompt shows the mask, not the key.
    fn assert_masked_entry_hides_the_key(controller: &mut InteractiveController) {
        submit_text(controller, "/key");
        assert!(
            controller.editor.secret_entry(),
            "a bare /key starts secret entry"
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
                .contains("API key saved to"),
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

    /// `/key` refuses while a run is active, and Esc then abandons entry safely.
    fn assert_escape_abandons_entry_without_overwriting(
        controller: &mut InteractiveController,
        events: &tokio::sync::mpsc::UnboundedSender<SessionEvent>,
        context: &LaunchContext,
    ) {
        submit_text(controller, "/key");
        assert!(
            !controller.editor.secret_entry(),
            "/key must not open secret entry while a run is active"
        );
        assert!(
            controller
                .transcript()
                .join("\n")
                .contains("cannot enter an API key while a run is active"),
            "{:?}",
            controller.transcript()
        );
        let _ = events.send(SessionEvent::RunTerminal {
            outcome: RunOutcome::Done,
        });
        let _ = controller.pump_events();

        submit_text(controller, "/key");
        assert!(controller.editor.secret_entry());
        type_text(controller, "sk-abandoned");
        let _ = controller.handle_key(Key::Esc);
        assert!(!controller.editor.secret_entry(), "Esc cancels entry");
        assert_eq!(
            controller.prompt(),
            "> ",
            "the prompt is a normal one again"
        );
        let path = saved_credential_path(context);
        assert_eq!(
            std::fs::read_to_string(&path).expect("credential file"),
            "DEEPSEEK_API_KEY=\"sk-controller-fixture\"\n",
            "Esc must not overwrite the stored key"
        );
    }

    #[test]
    fn k01_inline_key_uses_the_full_remainder_and_is_not_recallable() {
        let (_temp, context) = context(false);
        let channel = SessionChannel::new();
        let port = SavingPort {
            recorded: RecordingPort::default(),
            data_dir: context.paths.data_dir.clone(),
        };
        let mut controller = InteractiveController::new(&context, Box::new(port), channel, true);
        controller.boot_lines();

        let command = "/key sk-with an intentional space";
        let effects = submit_text(&mut controller, command);
        assert!(
            effects_to_plain(&effects)
                .join("\n")
                .contains("API key saved"),
            "{effects:#?}"
        );
        assert_eq!(
            std::fs::read_to_string(saved_credential_path(&context)).expect("credential file"),
            "DEEPSEEK_API_KEY=\"sk-with an intentional space\"\n"
        );

        let _ = controller.handle_key(Key::Up);
        assert_eq!(
            controller.prompt(),
            "> ",
            "Up must not recall an inline credential"
        );
        assert!(
            !controller
                .editor
                .history()
                .iter()
                .any(|entry| entry == command),
            "the inline credential must not remain in editor history"
        );
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
                Effect::Thinking(_) | Effect::Redraw | Effect::Exit(_) => {}
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

    /// A gated **write**, which is the case that keeps its panel.
    fn approval_event(request_id: &str) -> SessionEvent {
        SessionEvent::ApprovalRequired {
            request_id: request_id.to_owned(),
            action: "apply_patch".to_owned(),
            summary: "path=src/parser.rs".to_owned(),
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
            workspace: "C:/work/project".to_owned(),
            scope: "once".to_owned(),
            expires_at: Instant::now() + Duration::from_mins(5),
            read_only: true,
        }
    }

    #[test]
    fn h03_one_admission_per_message_and_a_running_run_refuses_a_second() {
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
        assert!(plain.contains("a run is already active"), "{plain}");
        assert_eq!(
            harness.port.submissions.lock().expect("submissions").len(),
            1,
            "a second input is never admitted while a run is active"
        );
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
                summary: "path=a.rs".to_owned(),
            })
            .expect("started");
        harness
            .events
            .send(SessionEvent::ToolSettled {
                name: "apply_patch".to_owned(),
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
            harness.controller.ui_state().open_tool.is_none(),
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
                summary: "path=a.rs".to_owned(),
            })
            .expect("tool");
        harness
            .events
            .send(SessionEvent::ToolSettled {
                name: "read_file".to_owned(),
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
        assert!(help.contains("Ctrl-D"), "{help}");

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
                .map(|command| command.name)
                .collect::<Vec<_>>(),
            SLASH_COMMANDS
                .iter()
                .map(|command| command.name)
                .collect::<Vec<_>>(),
            "one slash offers every command"
        );
        assert_eq!(state.suggestion_selected, 0);
        assert_eq!(state.buffer, "/", "and the draft is untouched");

        type_text(&mut harness.controller, "re");
        let state = harness.controller.ui_state();
        assert_eq!(
            state
                .suggestions
                .iter()
                .map(|command| command.name)
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
        assert_eq!(harness.controller.ui_state().buffer, "/re");
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
            "/help",
            "Tab takes the first row"
        );

        // Move the highlight, and Tab takes that row instead.
        let _ = harness.controller.handle_key(Key::EraseToLineStart);
        type_text(&mut harness.controller, "/");
        let _ = harness.controller.handle_key(Key::Down);
        let _ = harness.controller.handle_key(Key::Down);
        let _ = harness.controller.handle_key(Key::Tab);
        assert_eq!(harness.controller.ui_state().buffer, "/key");
    }

    /// Enter completes a half-typed command instead of submitting it; the next
    /// Enter runs the command. `/he` used to be answered with "unknown command".
    #[test]
    fn slash_enter_completes_a_half_typed_command_then_runs_it() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        type_text(&mut harness.controller, "/he");

        let effects = harness.controller.handle_key(Key::Enter);
        assert_eq!(effects, vec![Effect::Redraw], "nothing ran yet");
        assert_eq!(harness.controller.ui_state().buffer, "/help");
        assert!(
            submissions(&harness).is_empty(),
            "completing a command is not submitting a request"
        );
        assert!(
            harness.controller.transcript().is_empty(),
            "and not a command either"
        );

        let _ = harness.controller.handle_key(Key::Enter);
        assert!(
            matches!(
                harness.controller.ui_state().modal,
                Some(Modal::Overlay { ref title, .. }) if title == "/help"
            ),
            "the second Enter ran the completed command: {:?}",
            harness.controller.ui_state().modal
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
        assert_eq!(
            tui.controller.ui_state().suggestions.len(),
            1,
            "the editor still holds the candidate; the frame is what hides it"
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
                "the panel must say what memory is scoped by: {text}"
            );
            drop(temp);
        }
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
    fn t04_tool_card_settles_in_place_with_duration() {
        let mut harness = tui_bench(true);
        let _ = harness.controller.boot_lines();
        let _ = submit_text(&mut harness.controller, "work");
        harness
            .events
            .send(SessionEvent::ToolStarted {
                name: "read_file".to_owned(),
                summary: "path=a.rs".to_owned(),
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
            harness.controller.ui_state().open_tool,
            Some(("read_file".to_owned(), "path=a.rs".to_owned()))
        );

        harness
            .events
            .send(SessionEvent::ToolSettled {
                name: "read_file".to_owned(),
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
            HistoryItem::Tool { name, summary, state: ToolState::Ok { elapsed } }
                if name == "read_file" && summary == "path=a.rs" && *elapsed == Duration::from_millis(12)
        ));
        assert!(harness.controller.ui_state().open_tool.is_none());
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
                summary: "path=a.rs".to_owned(),
            })
            .expect("start");
        harness
            .events
            .send(SessionEvent::ToolSettled {
                name: "read_file".to_owned(),
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
        harness
            .events
            .send(approval_event("req-4"))
            .expect("approval");
        let _ = harness.controller.pump_events();
        // Expire it behind the controller's back. The panel closes and the run is
        // active again, so a late "y" is a normal submission and the second-input
        // guard refuses it: it never reaches the gate as an answer.
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
            plain.contains("a run is already active"),
            "a late answer is refused as a second input, not sent to the gate: {effects:#?}"
        );
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
                state: ToolState::Started,
            },
            HistoryItem::Tool {
                name: "search_text".to_owned(),
                summary: String::new(),
                state: ToolState::Failed {
                    elapsed: Duration::from_millis(3100),
                    detail: String::new(),
                },
            },
            HistoryItem::Tool {
                name: "apply_patch".to_owned(),
                summary: String::new(),
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
        assert!(line.contains("8 steps"), "{line}");
        assert!(line.contains("16 tool calls"), "{line}");
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
}
