//! Bounded model -> tool -> model loop (`HA_LAUNCH` H04, gate G2).
//!
//! The P2 runtime admits one user input and calls the provider once; the P3 loop
//! executed the requested tools but never sent their results back. This module
//! owns the missing loop as an application-layer driver: one admission per user
//! message (the runtime keeps that authority), tool execution through the
//! existing policy/approval/receipt gate, and a bounded number of continuation
//! steps.

use std::fmt::{self, Write as _};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use harness_providers::{
    CancellationToken, MessageRole, NormalizedToolCall, ProviderMessage, ProviderStreamEvent,
    ProviderToolCall,
};
use harness_runtime::{
    AcceptanceState, AgentState, AskRequest, CheckObservation, GoalEvaluationInput, GoalEvidence,
    GoalSpec, GoalVerdict, HumanInputService, ProviderEventSink, RunCommand, RunInbox, RunRequest,
    RunResult, RuntimeService, now_unix_ms,
};
use harness_store_sqlite::{RunCommandKind, RunState};
use harness_types::{ErrorCode, HarnessError, QuestionId};
use serde_json::{Value, json};

use crate::{
    ApprovalGrant, AskUserInput, CodingToolAction, Decision, GIT_LOG_DEFAULT_LIMIT,
    HISTORY_SEARCH_DEFAULT_LIMIT, PreparedToolRequest, ToolExecutionService, ToolExecutionView,
    ToolOutput, ToolRequest,
    capture::StreamTail,
    coding_tool_names, tool_pattern_for_action,
    truncate::{
        DEFAULT_MAX_BYTES, GREP_MAX_LINE_LENGTH, TruncatedBy, TruncationLimits, format_size,
        truncate_head, truncate_tail,
    },
    workspace::MAX_SEARCH_MATCHES,
};

/// Limits that bound one user turn.
#[derive(Clone, Copy, Debug)]
pub struct TurnLimits {
    pub max_steps: u32,
    pub max_tool_calls: u32,
    pub deadline: Duration,
}

impl Default for TurnLimits {
    fn default() -> Self {
        Self {
            max_steps: 8,
            max_tool_calls: 16,
            deadline: Duration::from_mins(10),
        }
    }
}

/// Progress reported while a turn runs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TurnProgress {
    TextDelta(String),
    ThinkingDelta(String),
    Usage {
        prompt_tokens: u64,
        completion_tokens: u64,
    },
    StepStarted {
        step: u32,
    },
    ToolStarted {
        name: String,
        /// One short line of the arguments, for the collapsed card.
        summary: String,
        /// The call's arguments exactly as the model sent them (normally a JSON
        /// object), so an expanded transcript can show the whole input - the full
        /// command, every path, the patch - rather than the cut summary.
        input: String,
    },
    ToolSettled {
        name: String,
        ok: bool,
        /// Why the call did not execute, for the human reading the transcript.
        ///
        /// The model is told the same thing in its tool result, but a card that only
        /// says `failed 962ms` leaves the person who has to answer the next approval
        /// unable to tell a malformed call from a policy denial.
        detail: Option<String>,
    },
    /// What a tool returned, as the model is shown it; the TUI shows the first
    /// lines under the tool's panel, the way prime-agent's tool panel does.
    ToolOutput {
        name: String,
        text: String,
    },
    /// An action ran without opening the approval panel; this reason is part of
    /// the user-visible transcript.
    Info(String),
    /// A non-fatal host condition the user should know about.
    Notice(String),
}

/// Receives progress; the interactive service maps it to display events.
pub trait TurnObserver: Send + Sync {
    fn observe(&self, progress: TurnProgress);
}

/// Why the loop stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurnStop {
    /// The model answered without requesting another tool.
    Final,
    /// The step bound was reached.
    StepLimit,
    /// The tool-call bound was reached.
    ToolLimit,
    /// The deadline was reached.
    Deadline,
    /// The caller canceled the turn.
    Canceled,
    /// The run paused on a durable question; nothing is held open.
    NeedsInput,
    /// The goal waits on something outside the host; the host does not poll.
    ExternalWait,
    /// Continuations repeated without new evidence.
    NoProgress,
    /// The goal continuation bound was reached.
    GoalLimit,
    /// The goal's budget cannot fund another step.
    BudgetExhausted,
    /// The same tool call repeated inside the loop window.
    LoopDetected,
    /// The terminal response cannot be trusted (empty or output-capped).
    Unverified,
}

impl TurnStop {
    /// The stable spelling persisted with the run and printed in JSON.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Final => "final",
            Self::StepLimit => "step_limit",
            Self::ToolLimit => "tool_limit",
            Self::Deadline => "deadline",
            Self::Canceled => "canceled",
            Self::NeedsInput => "needs_input",
            Self::ExternalWait => "external_wait",
            Self::NoProgress => "no_progress",
            Self::GoalLimit => "goal_limit",
            Self::BudgetExhausted => "budget_exhausted",
            Self::LoopDetected => "loop_detected",
            Self::Unverified => "unverified",
        }
    }

    /// Whether the run can be continued by a later turn without new input.
    #[must_use]
    pub const fn is_resumable(self) -> bool {
        matches!(
            self,
            Self::StepLimit
                | Self::ToolLimit
                | Self::NoProgress
                | Self::GoalLimit
                | Self::BudgetExhausted
                | Self::NeedsInput
                | Self::ExternalWait
        )
    }
}

/// One gated action waiting for the user's decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovalProposal {
    /// Stable id the answer must quote back.
    pub request_id: String,
    /// What the action is, in one line.
    pub action: String,
    /// The concrete target: path, command or query.
    pub summary: String,
    /// Exact, action-scoped rule suggested by uppercase `A` in the panel.
    pub rule_pattern: String,
    /// Directory the action would act on.
    pub workspace: PathBuf,
    /// How far the grant reaches.
    pub scope: String,
    /// Whether the action only reads.
    ///
    /// The gate uses this to decide what it may grant without asking, and the panel
    /// uses it to decide what it offers: a host may let read-only actions through
    /// once the user says so, but it must never extend that to a write. A request
    /// that is read-only by kind can still have been refused outright before this
    /// proposal existed - path containment and credential-like names are checked in
    /// `prepare` - so this flag means "nothing is written", not "already allowed".
    pub read_only: bool,
}

/// The user's answer to one proposal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApprovalAnswer {
    /// Run this exact action once.
    Granted,
    /// Do not run it; the model is told the action was denied.
    Denied,
    /// Nobody answered in time; treated as a refusal.
    Expired,
}

/// Asks the host to answer one proposal.
///
/// The driver never grants by itself: a gate that is asked must be answered by the
/// user through the interactive app or by an explicit fixture. A gate is free to
/// answer `Granted` without asking when `proposal.read_only` is set and the user has
/// already allowed reads for this run - that decision belongs to the host, not here,
/// so it stays one implementation instead of one per caller.
pub trait ApprovalGate: Send + Sync {
    fn request(
        &self,
        proposal: ApprovalProposal,
    ) -> Pin<Box<dyn Future<Output = ApprovalAnswer> + Send>>;

    /// Called after a granted action has settled, so a host may activate a
    /// separately confirmed pattern for the remaining actions in this turn.
    fn action_completed(&self, _request_id: &str) {}
}

/// How tool approvals are handled inside this turn.
#[derive(Clone)]
pub enum ApprovalMode {
    /// Grant the prepared action (used by fixtures and explicit auto runs).
    Auto,
    /// Never grant a blanket approval; a gated action fails closed.
    None,
    /// Ask the host for each gated action.
    Ask(Arc<dyn ApprovalGate>),
}

impl fmt::Debug for ApprovalMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => formatter.write_str("Auto"),
            Self::None => formatter.write_str("None"),
            Self::Ask(_) => formatter.write_str("Ask(..)"),
        }
    }
}

/// Scope text shown with every proposal.
const APPROVAL_SCOPE: &str = "one action, this turn only";

/// Per-turn options.
#[derive(Clone, Debug)]
pub struct TurnOptions {
    pub workspace_root: PathBuf,
    pub actor_id: String,
    pub approvals: ApprovalMode,
    pub limits: TurnLimits,
}

/// Result of one bounded turn.
#[derive(Clone, Debug)]
pub struct TurnOutcome {
    pub session_id: harness_types::SessionId,
    pub task_id: harness_types::TaskId,
    pub input_id: harness_types::InputId,
    pub run_id: harness_types::AgentRunId,
    /// Run revision after the last frozen step.
    pub run_revision: u64,
    pub final_text: String,
    pub steps: u32,
    pub tool_calls: u32,
    pub executions: Vec<ToolExecutionView>,
    pub stop: TurnStop,
    /// Task acceptance, kept separate from the stop reason.
    pub acceptance: AcceptanceState,
    /// The goal report, when a goal was attached to this turn.
    pub goal: Option<GoalReport>,
    /// The durable question this run paused on, when it did.
    pub pending_question: Option<harness_types::QuestionId>,
}

/// What one goal evaluation run reported, in words the CLI can print.
#[derive(Clone, Debug)]
pub struct GoalReport {
    pub objective: String,
    pub acceptance: AcceptanceState,
    pub verdict: GoalVerdict,
    pub continuations: u32,
    pub no_progress: u32,
    /// Why continuation stopped, when it did.
    pub stop: Option<TurnStop>,
    pub missing: Vec<String>,
}

impl GoalReport {
    fn from_verdict(
        objective: &str,
        verdict: &GoalVerdict,
        continuations: u32,
        no_progress: u32,
        stop: Option<TurnStop>,
    ) -> Self {
        Self {
            objective: objective.to_owned(),
            acceptance: verdict.acceptance(),
            verdict: verdict.clone(),
            continuations,
            no_progress,
            stop,
            missing: missing_of(verdict),
        }
    }
}

fn missing_of(verdict: &GoalVerdict) -> Vec<String> {
    match verdict {
        GoalVerdict::Satisfied { .. } => Vec::new(),
        GoalVerdict::NeedsWork { missing, .. }
        | GoalVerdict::NeedsInput { missing, .. }
        | GoalVerdict::ExternalWait { missing, .. }
        | GoalVerdict::Unverified { missing, .. } => missing.clone(),
    }
}

/// Host-owned view of the external tools one turn may see and call.
///
/// The host decides what is advertised and what a name means; a plugin cannot
/// announce itself to the model. Everything resolved here still crosses the same
/// gate as a built-in action — policy, approval, durable intent and receipt — so
/// this trait cannot authorize anything by itself.
pub trait ExternalToolCatalog: Send + Sync {
    /// Provider-function schemas for the external tools this host advertises.
    fn schemas(&self) -> Vec<Value>;
    /// Resolve one advertised name into an external action.
    ///
    /// `None` means the name is not one this host advertises, which the turn reports
    /// as an unsupported tool rather than executing anything.
    fn resolve(&self, name: &str, arguments: &Value) -> Option<CodingToolAction>;
}

/// A catalogue attached to a driver. Its `Debug` never prints plugin internals.
#[derive(Clone)]
pub struct ExternalTools(Arc<dyn ExternalToolCatalog>);

impl ExternalTools {
    #[must_use]
    pub fn new(catalog: Arc<dyn ExternalToolCatalog>) -> Self {
        Self(catalog)
    }

    #[must_use]
    pub fn schemas(&self) -> Vec<Value> {
        self.0.schemas()
    }

    #[must_use]
    pub fn resolve(&self, name: &str, arguments: &Value) -> Option<CodingToolAction> {
        self.0.resolve(name, arguments)
    }
}

impl fmt::Debug for ExternalTools {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExternalTools")
            .field("schemas", &self.schemas().len())
            .finish()
    }
}

/// How large one turn's own transcript may grow before older tool results are
/// shortened, in bytes (about 50k tokens).
const TRANSCRIPT_BUDGET_BYTES: usize = 200_000;

/// How many of the newest tool results always stay whole.
const KEEP_RECENT_RESULTS: usize = 6;

/// What an elided tool result is replaced with.
const ELIDED_RESULT: &str =
    "[earlier tool result shortened to save context; call the tool again if you need it]";

/// Keep a long turn's transcript within budget by shortening its oldest tool results.
///
/// A turn now carries everything it said and ran, which is what fixed the loop that
/// forgot its own steps - and it also means a long turn grows until the request
/// overflows. prime-agent's loop (and pi's) handle that before each model call with a
/// context transform; this is the same move for the part of the context the turn owns:
/// the oldest tool results, which are the bulk of it and the part a model can always
/// ask for again, are replaced by a one-line note, newest last. The call/result pairs
/// stay whole, so the transcript remains valid, and the newest results and every
/// assistant message are never touched. Returns how many results were shortened.
fn trim_transcript(transcript: &mut [ProviderMessage], budget: usize, keep_recent: usize) -> usize {
    let size = |messages: &[ProviderMessage]| -> usize {
        messages
            .iter()
            .map(|message| {
                message.content.len()
                    + message
                        .tool_calls
                        .iter()
                        .map(|call| call.arguments.len())
                        .sum::<usize>()
            })
            .sum()
    };
    let mut total = size(transcript);
    if total <= budget {
        return 0;
    }
    let results = transcript
        .iter()
        .enumerate()
        .filter(|(_, message)| message.role == MessageRole::Tool)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let eligible = results.len().saturating_sub(keep_recent);
    let mut elided = 0;
    for &index in results.iter().take(eligible) {
        if total <= budget {
            break;
        }
        let message = &mut transcript[index];
        if message.content.len() <= ELIDED_RESULT.len() {
            continue;
        }
        total -= message.content.len() - ELIDED_RESULT.len();
        ELIDED_RESULT.clone_into(&mut message.content);
        elided += 1;
    }
    elided
}

/// Longest tool result text handed back to the model.
///
/// It was 4000 characters - about a thousand tokens - so one `read_file` of an
/// ordinary source file came back cut, and the model spent step after step reading
/// the same file in slices. Every tool already bounds its own output to the
/// shared limits of [`crate::truncate`] and says what it left out; this is the
/// last guard against one result taking over the context. It leaves room above
/// that limit for a result's header and notices, so a notice telling the model
/// how to read the rest is never the part that gets cut.
const TOOL_RESULT_LIMIT: usize = DEFAULT_MAX_BYTES + 4 * 1024;

/// How many recent tool signatures the loop detector remembers.
const LOOP_WINDOW: usize = 6;

/// How often the same tool signature may repeat in the window before the loop
/// is stopped. Three repeats of one identical call is already a loop; two is a
/// retry the model is allowed to make.
const LOOP_REPEAT_LIMIT: usize = 3;

/// Bounded model -> tool -> model loop.
#[derive(Clone)]
pub struct TurnDriver {
    runtime: Arc<RuntimeService>,
    tools: ToolExecutionService,
    external: Option<ExternalTools>,
    /// The goal this turn must satisfy, when the caller attached one.
    goal: Option<GoalSpec>,
    /// Durable steering/cancel inbox, when the host attached one.
    inbox: Option<RunInbox>,
}

impl TurnDriver {
    #[must_use]
    pub fn new(runtime: Arc<RuntimeService>, tools: ToolExecutionService) -> Self {
        Self {
            runtime,
            tools,
            external: None,
            goal: None,
            inbox: None,
        }
    }

    /// Advertise the external tools of trusted extensions to this driver.
    #[must_use]
    pub fn with_external(mut self, external: ExternalTools) -> Self {
        self.external = Some(external);
        self
    }

    /// Attach the goal criteria and continuation bounds for this turn.
    #[must_use]
    pub fn with_goal(mut self, goal: GoalSpec) -> Self {
        self.goal = Some(goal);
        self
    }

    /// Attach the durable steering/cancel inbox.
    #[must_use]
    pub fn with_inbox(mut self, inbox: RunInbox) -> Self {
        self.inbox = Some(inbox);
        self
    }

    /// Run one user turn to a final answer or a bound.
    pub async fn run_turn(
        &self,
        request: RunRequest,
        options: TurnOptions,
        observer: Arc<dyn TurnObserver>,
        cancellation: CancellationToken,
    ) -> Result<TurnOutcome, HarnessError> {
        let hook_observer = Arc::clone(&observer);
        let outcome =
            Box::pin(self.run_turn_inner(None, request, options, observer, cancellation.clone()))
                .await?;
        self.run_stop_hooks(&outcome, &hook_observer, cancellation)
            .await;
        Ok(outcome)
    }

    /// Run one user turn that continues a previous session.
    ///
    /// The accepted journal admits one user input per session, so a follow-up turn
    /// opens a new session that is linked to its predecessor and inherits its
    /// context packet. The task identity stays the same across the conversation.
    pub async fn run_turn_continuing(
        &self,
        source_session_id: &harness_types::SessionId,
        request: RunRequest,
        options: TurnOptions,
        observer: Arc<dyn TurnObserver>,
        cancellation: CancellationToken,
    ) -> Result<TurnOutcome, HarnessError> {
        let hook_observer = Arc::clone(&observer);
        let outcome = Box::pin(self.run_turn_inner(
            Some(source_session_id),
            request,
            options,
            observer,
            cancellation.clone(),
        ))
        .await?;
        self.run_stop_hooks(&outcome, &hook_observer, cancellation)
            .await;
        Ok(outcome)
    }

    async fn run_stop_hooks(
        &self,
        outcome: &TurnOutcome,
        observer: &Arc<dyn TurnObserver>,
        cancellation: CancellationToken,
    ) {
        let notices = self
            .tools
            .run_event_hooks(
                "stop",
                json!({
                    "event": "stop",
                    "session_id": outcome.session_id.as_str(),
                    "task_id": outcome.task_id.as_str(),
                    "stop": outcome.stop.as_str(),
                    "tool_calls": outcome.tool_calls,
                }),
                cancellation,
            )
            .await;
        for notice in notices {
            observer.observe(TurnProgress::Notice(notice));
        }
    }

    // The turn is one bounded pass per model call, wrapped in a goal loop that
    // may continue the same admitted input. The length is the wiring, not
    // hidden branching logic.
    #[allow(clippy::too_many_lines)]
    async fn run_turn_inner(
        &self,
        source_session_id: Option<&harness_types::SessionId>,
        mut request: RunRequest,
        options: TurnOptions,
        observer: Arc<dyn TurnObserver>,
        cancellation: CancellationToken,
    ) -> Result<TurnOutcome, HarnessError> {
        if let Some(goal) = &self.goal {
            goal.validate()
                .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
        }
        let started = Instant::now();
        let session_id = request.session_id.clone();
        let task_id = request.task_id.clone();
        let input_id = request.input_id.clone();
        let mut executions = Vec::new();
        let mut tool_calls = 0_u32;
        // One step is one model call, and the first call is already one: counting the
        // dispatch again after each round of tools made `max_steps` mean about half of
        // what it says and reported a turn that used four calls as eight steps.
        let mut steps = 1_u32;
        // The first model call is step one; later ones announce themselves as
        // they start. Without this a turn answered in one call reported
        // `0 steps`, and the status line counted from zero while it ran.
        observer.observe(TurnProgress::StepStarted { step: steps });
        // Goal state. `continuations` counts host continuations of one admitted
        // input; `no_progress` counts continuations whose evidence fingerprint
        // did not change.
        let mut continuations = 0_u32;
        let mut no_progress = 0_u32;
        let mut last_signature: Option<String> = None;
        let mut goal_report: Option<GoalReport> = None;
        let mut acceptance = AcceptanceState::NotEvaluated;
        let mut pending_question: Option<QuestionId> = None;
        // Recent tool signatures for the loop detector.
        let mut loop_signatures: Vec<String> = Vec::new();
        // Everything this turn has already said and executed. Each continuation
        // sends the whole transcript, not just the newest step: a model that is
        // handed back only the last tool result has no record of what it already
        // looked at, so it starts over every step and the turn burns its bounds
        // re-reading the same files instead of answering.
        let mut transcript: Vec<ProviderMessage> = Vec::new();
        // Every call id already announced in that transcript.
        let mut announced_call_ids: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();

        let first_step = match source_session_id {
            Some(source) => {
                self.runtime
                    .continue_task_streaming(
                        source,
                        request.clone(),
                        cancellation.clone(),
                        sink_for(&observer),
                    )
                    .await
            }
            None => {
                self.runtime
                    .run_streaming(request.clone(), cancellation.clone(), sink_for(&observer))
                    .await
            }
        };
        let mut result =
            first_step.map_err(|error| HarnessError::new(error.code(), error.to_string()))?;

        // The loop yields why it stopped, so no bound can silently fall through.
        let stop = 'turn: loop {
            for notice in std::mem::take(&mut result.notices) {
                observer.observe(TurnProgress::Notice(notice));
            }
            // A06: a stream that never reached a terminal marker is not a
            // completed answer. It is reported, but nothing may execute and the
            // turn is not accepted. A terminal response whose call is malformed
            // is a different case: the call is refused below with its own
            // reason so the model can re-issue it.
            if result.finish_reason.is_none() {
                break TurnStop::Unverified;
            }
            if result.tool_calls.is_empty() {
                // A terminal answer is evaluated before it is accepted. The
                // evaluator decides whether the goal is satisfied; a model that
                // says "done" without evidence is continued or blocked.
                if let Some(goal) = &self.goal {
                    let evidence = goal_evidence(&result, &executions);
                    let remaining_budget = self
                        .runtime
                        .budget_remaining()
                        .await
                        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
                    let evaluation = match self.runtime.evaluator().evaluate(&GoalEvaluationInput {
                        spec: goal,
                        evidence: &evidence,
                        previous_signature: last_signature.as_deref(),
                        continuations,
                        no_progress,
                        remaining_budget,
                    }) {
                        Ok(evaluation) => evaluation,
                        Err(error) => {
                            // An evaluator that cannot answer is not a satisfied
                            // goal: the run ends failed and nothing is accepted.
                            let _ = self
                                .runtime
                                .finish_run_record(
                                    &result.run_id,
                                    result.run_revision,
                                    RunState::Failed,
                                    Some(AcceptanceState::Unverified.as_str()),
                                    "evaluator_failed",
                                    None,
                                )
                                .await;
                            return Err(HarnessError::new(error.code(), error.to_string()));
                        }
                    };
                    if let Err(error) =
                        harness_runtime::validate_evaluation(goal, &evidence, &evaluation)
                    {
                        let _ = self
                            .runtime
                            .finish_run_record(
                                &result.run_id,
                                result.run_revision,
                                RunState::Failed,
                                Some(AcceptanceState::Unverified.as_str()),
                                "evaluator_invalid",
                                None,
                            )
                            .await;
                        return Err(HarnessError::new(error.code(), error.to_string()));
                    }
                    acceptance = evaluation.verdict.acceptance();
                    match &evaluation.verdict {
                        GoalVerdict::Satisfied { .. } => {
                            goal_report = Some(GoalReport::from_verdict(
                                &goal.objective,
                                &evaluation.verdict,
                                continuations,
                                no_progress,
                                None,
                            ));
                            break TurnStop::Final;
                        }
                        GoalVerdict::Unverified { .. } => {
                            goal_report = Some(GoalReport::from_verdict(
                                &goal.objective,
                                &evaluation.verdict,
                                continuations,
                                no_progress,
                                Some(TurnStop::Unverified),
                            ));
                            break TurnStop::Unverified;
                        }
                        GoalVerdict::NeedsInput { question, .. } => {
                            let question =
                                self.ask_question(&result, question)
                                    .await
                                    .map_err(|error| {
                                        HarnessError::new(error.code(), error.to_string())
                                    })?;
                            pending_question = Some(question.question_id.clone());
                            goal_report = Some(GoalReport::from_verdict(
                                &goal.objective,
                                &evaluation.verdict,
                                continuations,
                                no_progress,
                                Some(TurnStop::NeedsInput),
                            ));
                            break TurnStop::NeedsInput;
                        }
                        GoalVerdict::ExternalWait { .. } => {
                            goal_report = Some(GoalReport::from_verdict(
                                &goal.objective,
                                &evaluation.verdict,
                                continuations,
                                no_progress,
                                Some(TurnStop::ExternalWait),
                            ));
                            break TurnStop::ExternalWait;
                        }
                        GoalVerdict::NeedsWork { next_action, .. } => {
                            let progressed = last_signature
                                .as_deref()
                                .is_none_or(|previous| previous != evaluation.progress_signature);
                            last_signature = Some(evaluation.progress_signature.clone());
                            if progressed {
                                no_progress = 0;
                            } else {
                                no_progress += 1;
                            }
                            let exhausted = remaining_budget == Some(0);
                            let stop_reason = if no_progress > goal.max_no_progress {
                                Some(TurnStop::NoProgress)
                            } else if continuations >= goal.max_continuations {
                                Some(TurnStop::GoalLimit)
                            } else if exhausted {
                                Some(TurnStop::BudgetExhausted)
                            } else if steps >= options.limits.max_steps {
                                Some(TurnStop::StepLimit)
                            } else {
                                None
                            };
                            if let Some(stop_reason) = stop_reason {
                                goal_report = Some(GoalReport::from_verdict(
                                    &goal.objective,
                                    &evaluation.verdict,
                                    continuations,
                                    no_progress,
                                    Some(stop_reason),
                                ));
                                break stop_reason;
                            }
                            continuations += 1;
                            steps += 1;
                            observer.observe(TurnProgress::StepStarted { step: steps });
                            // The answer the evaluator judged is part of the
                            // record too, otherwise the continuation reads as if
                            // the model had never replied.
                            if !result.response.trim().is_empty() {
                                transcript.push(
                                    ProviderMessage::new(
                                        MessageRole::Assistant,
                                        result.response.clone(),
                                    )
                                    .with_reasoning(result.reasoning.clone()),
                                );
                            }
                            // The continuation instruction is host policy, not
                            // user text: the input was admitted once and this
                            // never creates a second input identity.
                            transcript.push(ProviderMessage::new(
                                MessageRole::System,
                                format!(
                                    "The goal is not complete yet. Next action: {next_action}. Produce the missing evidence, then answer again."
                                ),
                            ));
                            result = self
                                .runtime
                                .continue_run(
                                    request.clone(),
                                    transcript.clone(),
                                    cancellation.clone(),
                                    Some(sink_for(&observer)),
                                )
                                .await
                                .map_err(|error| {
                                    HarnessError::new(error.code(), error.to_string())
                                })?;
                            continue 'turn;
                        }
                    }
                }
                break TurnStop::Final;
            }
            if cancellation.is_cancelled() {
                break TurnStop::Canceled;
            }
            if started.elapsed() >= options.limits.deadline {
                break TurnStop::Deadline;
            }
            // The bound counts model calls, so it is checked before the next one.
            if steps >= options.limits.max_steps {
                break TurnStop::StepLimit;
            }
            let requested = u32::try_from(result.tool_calls.len()).unwrap_or(u32::MAX);
            if tool_calls + requested > options.limits.max_tool_calls {
                break TurnStop::ToolLimit;
            }

            let mut appended = Vec::new();
            // A safe boundary: steering is delivered and a cancel stops the turn
            // before any queued work executes.
            if let Some(inbox) = &self.inbox {
                let run = self
                    .runtime
                    .run_record(&result.run_id)
                    .await
                    .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
                if let Some(run) = run {
                    let commands = inbox
                        .claim(&run, 8, now_unix_ms())
                        .await
                        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
                    let mut canceled = None;
                    for command in &commands {
                        match command.kind {
                            RunCommandKind::Cancel => {
                                canceled = Some(
                                    RunInbox::cancel_reason(command)
                                        .unwrap_or_else(|| "canceled by the user".to_owned()),
                                );
                                inbox
                                    .apply(command, "canceled at a step boundary", now_unix_ms())
                                    .await
                                    .map_err(|error| {
                                        HarnessError::new(error.code(), error.to_string())
                                    })?;
                            }
                            RunCommandKind::Steer => {
                                if let Some(text) = RunInbox::steering_text(command) {
                                    appended.push(ProviderMessage::new(
                                        MessageRole::User,
                                        format!("[steering correction from the user]\n{text}"),
                                    ));
                                }
                                inbox
                                    .apply(
                                        command,
                                        "steering delivered to the next step",
                                        now_unix_ms(),
                                    )
                                    .await
                                    .map_err(|error| {
                                        HarnessError::new(error.code(), error.to_string())
                                    })?;
                            }
                        }
                    }
                    if let Some(reason) = canceled {
                        observer.observe(TurnProgress::TextDelta(format!(
                            "canceled at a step boundary: {reason}"
                        )));
                        break TurnStop::Canceled;
                    }
                }
            }
            // Loop detection: the same tool call repeated inside the window is a
            // loop, not progress. Different arguments or a changed batch are not.
            for call in &result.tool_calls {
                loop_signatures.push(call_signature(call));
            }
            if loop_signatures.len() > LOOP_WINDOW {
                let drain = loop_signatures.len() - LOOP_WINDOW;
                loop_signatures.drain(0..drain);
            }
            if repeated_tail(&loop_signatures) {
                break TurnStop::LoopDetected;
            }

            // The assistant turn keeps its own words and its typed calls, and
            // nothing else: a call-only response stays empty, as prime-agent keeps
            // it. A placeholder naming the tools was sent back as the model's own
            // words, and the model learned to write tool names instead of calling
            // them - which ended turns with nothing done and no answer.
            let assistant_text = result.response.trim().to_owned();
            // A transcript that keeps every step can meet the same call id
            // twice, because a provider only promises an id is unique inside one
            // response. Two announcements of one id is an invalid transcript, so
            // the repeat is renamed for the wire while the durable intent and the
            // receipt keep the id the provider actually sent.
            let transcript_ids: Vec<String> = result
                .tool_calls
                .iter()
                .map(|call| {
                    let mut id = call.call_id.clone();
                    let mut suffix = 1_u32;
                    while !announced_call_ids.insert(id.clone()) {
                        id = format!("{}#{suffix}", call.call_id);
                        suffix += 1;
                    }
                    id
                })
                .collect();
            appended.push(
                ProviderMessage::assistant_with_calls(
                    assistant_text,
                    result
                        .tool_calls
                        .iter()
                        .zip(transcript_ids.iter())
                        .map(|(call, transcript_id)| {
                            ProviderToolCall::new(
                                transcript_id.clone(),
                                call.name.clone(),
                                call.arguments.clone(),
                            )
                        })
                        .collect(),
                )
                .with_reasoning(result.reasoning.clone()),
            );
            // prime-agent's `executeToolCalls`: a batch runs its calls side by side
            // unless one of them must run alone. Gates, approvals and intents go
            // one at a time in call order; the side effects run concurrently on the
            // runtime's worker threads; receipts and results are committed in call
            // order, so the journal and the transcript read as if run in sequence.
            let parallel = result.tool_calls.len() > 1
                && !result.tool_calls.iter().any(|call| runs_alone(&call.name));
            let mut slots: Vec<(String, String, Slot)> = Vec::new();
            for (call, transcript_id) in result.tool_calls.clone().into_iter().zip(transcript_ids) {
                tool_calls += 1;
                let name = call.name.clone();
                observer.observe(TurnProgress::ToolStarted {
                    name: name.clone(),
                    summary: summarize_arguments(&call.arguments),
                    input: call.arguments.clone(),
                });
                // A call the model never named, or whose arguments never became JSON,
                // is not an action. Rejecting it here says so in one sentence; the
                // execution gate would otherwise answer with a policy denial that
                // reads as if the tool itself had been refused.
                if let Some(reason) = malformed_call(&call) {
                    slots.push((
                        name.clone(),
                        transcript_id,
                        Slot::Failed(format!(
                            "tool call {name:?} was not executed: {reason}; re-issue it with a function name and complete JSON arguments"
                        ), reason.to_owned()),
                    ));
                    continue;
                }
                if name == "ask_user" {
                    for notice in self
                        .tools
                        .run_event_hooks(
                            "notification",
                            json!({
                                "event": "notification",
                                "notification": "ask_user",
                                "session_id": result.session_id.as_str(),
                                "task_id": result.task_id.as_str(),
                            }),
                            cancellation.clone(),
                        )
                        .await
                    {
                        observer.observe(TurnProgress::Notice(notice));
                    }
                    match self.ask_user(&result, &call).await {
                        Ok(question_id) => {
                            pending_question = Some(question_id);
                            observer.observe(TurnProgress::ToolSettled {
                                name,
                                ok: true,
                                detail: None,
                            });
                            break 'turn TurnStop::NeedsInput;
                        }
                        Err(error) => {
                            let message = format!("ask_user failed: {error}");
                            slots.push((
                                name,
                                transcript_id,
                                Slot::Failed(message, error.to_string()),
                            ));
                            continue;
                        }
                    }
                }
                let gated = match self.resolve_action(&call) {
                    Ok(action) => {
                        let request = ToolRequest::new(
                            result.session_id.clone(),
                            result.task_id.clone(),
                            options.actor_id.clone(),
                            options.workspace_root.clone(),
                            action,
                        );
                        // Correlation only: the provider call id is recorded with the
                        // intent and receipt so the transcript and the durable records
                        // can be paired.
                        let request = if call.call_id.trim().is_empty() {
                            request
                        } else {
                            request.with_call_id(call.call_id.clone())
                        };
                        gate_action(
                            &self.tools,
                            request,
                            &options,
                            tool_calls,
                            &observer,
                            &cancellation,
                        )
                        .await
                    }
                    Err(error) => Err(error),
                };
                let slot = match gated {
                    Ok(Gated::Done(done)) => Slot::Done(done),
                    Ok(Gated::Ready(ready)) if parallel => Slot::Ready(ready),
                    Ok(Gated::Ready(ready)) => {
                        let outcome = self
                            .tools
                            .run_begun(&ready.begun, cancellation.clone())
                            .await;
                        Slot::Done(
                            complete_action(&self.tools, ready, outcome, &observer, &cancellation)
                                .await,
                        )
                    }
                    Err(error) => Slot::Done(Err(error)),
                };
                slots.push((name, transcript_id, slot));
            }

            // The side effects of a parallel batch, each on its own task.
            let mut running = Vec::new();
            for (index, (_, _, slot)) in slots.iter_mut().enumerate() {
                if let Slot::Ready(_) = slot {
                    let Slot::Ready(ready) = std::mem::replace(slot, Slot::Taken) else {
                        continue;
                    };
                    let tools = self.tools.clone();
                    let cancellation = cancellation.clone();
                    running.push(tokio::spawn(async move {
                        let outcome = tools.run_begun(&ready.begun, cancellation).await;
                        (index, ready, outcome)
                    }));
                }
            }
            let mut finished = Vec::new();
            for task in running {
                match task.await {
                    Ok(done) => finished.push(done),
                    Err(error) => {
                        return Err(HarnessError::new(
                            ErrorCode::ProviderProtocol,
                            format!("a tool task stopped unexpectedly: {error}"),
                        ));
                    }
                }
            }
            finished.sort_by_key(|(index, _, _)| *index);
            for (index, ready, outcome) in finished {
                let done =
                    complete_action(&self.tools, ready, outcome, &observer, &cancellation).await;
                slots[index].2 = Slot::Done(done);
            }

            // Results in call order.
            for (name, transcript_id, slot) in slots {
                match slot {
                    Slot::Failed(message, detail) => {
                        observer.observe(TurnProgress::ToolSettled {
                            name,
                            ok: false,
                            detail: Some(detail),
                        });
                        appended.push(ProviderMessage::tool_result(transcript_id, message));
                    }
                    Slot::Done(Ok(view)) => {
                        let blocked = match &view.output {
                            ToolOutput::Denied { code, reason } => {
                                Some(format!("{code}: {reason}"))
                            }
                            _ => None,
                        };
                        if let ToolOutput::SkillActivated { block } = &view.output {
                            request
                                .project_rules
                                .retain(|existing| existing.id != block.id);
                            request.project_rules.push(block.clone());
                        }
                        let rendered = render_tool_output(&name, &view.output);
                        observer.observe(TurnProgress::ToolOutput {
                            name: name.clone(),
                            text: rendered.clone(),
                        });
                        observer.observe(TurnProgress::ToolSettled {
                            name: name.clone(),
                            ok: blocked.is_none(),
                            detail: blocked,
                        });
                        appended.push(
                            ProviderMessage::tool_result(transcript_id, rendered)
                                .with_attachments(tool_output_images(&view.output)),
                        );
                        executions.push(view);
                    }
                    Slot::Done(Err(error)) => {
                        // A failed tool is reported back to the model instead of
                        // ending the turn: that is what lets it fix its own call.
                        observer.observe(TurnProgress::ToolSettled {
                            name: name.clone(),
                            ok: false,
                            detail: Some(error.to_string()),
                        });
                        appended.push(ProviderMessage::tool_result(
                            transcript_id,
                            format!("tool {name} failed: {error}"),
                        ));
                    }
                    Slot::Ready(_) | Slot::Taken => {}
                }
            }

            steps += 1;
            if started.elapsed() >= options.limits.deadline {
                break TurnStop::Deadline;
            }
            observer.observe(TurnProgress::StepStarted { step: steps });
            transcript.extend(appended);
            let elided = trim_transcript(
                &mut transcript,
                TRANSCRIPT_BUDGET_BYTES,
                KEEP_RECENT_RESULTS,
            );
            if elided > 0 {
                observer.observe(TurnProgress::Notice(format!(
                    "context: {elided} older tool result(s) in this turn were shortened to stay within budget"
                )));
            }
            result = self
                .runtime
                .continue_run(
                    request.clone(),
                    transcript.clone(),
                    cancellation.clone(),
                    Some(sink_for(&observer)),
                )
                .await
                .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
        };

        // Persist the terminal outcome of the run. Resumable bounds keep the run
        // running: a later turn continues it, and a reopen must not read a pause
        // as a failure.
        if let Some(goal) = &self.goal
            && goal_report.is_none()
        {
            // The loop stopped before (or instead of) an evaluator verdict. The
            // report still says what happened and what acceptance that means.
            let (state, verdict) = match stop {
                TurnStop::Unverified => (
                    AcceptanceState::Unverified,
                    GoalVerdict::Unverified {
                        reason: "the provider stream was not dispatchable".to_owned(),
                        missing: goal.required_ids(),
                    },
                ),
                TurnStop::NoProgress | TurnStop::GoalLimit | TurnStop::BudgetExhausted => (
                    AcceptanceState::NeedsWork,
                    GoalVerdict::NeedsWork {
                        reason: format!("run stopped: {}", stop.as_str()),
                        next_action: "resume the run to continue the goal".to_owned(),
                        missing: goal.required_ids(),
                    },
                ),
                TurnStop::LoopDetected => (
                    AcceptanceState::Rejected,
                    GoalVerdict::Unverified {
                        reason: "the same tool call repeated".to_owned(),
                        missing: goal.required_ids(),
                    },
                ),
                _ => (
                    AcceptanceState::NotEvaluated,
                    GoalVerdict::NeedsWork {
                        reason: format!("run stopped: {}", stop.as_str()),
                        next_action: "resume the run to continue the goal".to_owned(),
                        missing: goal.required_ids(),
                    },
                ),
            };
            acceptance = state;
            goal_report = Some(GoalReport {
                objective: goal.objective.clone(),
                acceptance: state,
                verdict,
                continuations,
                no_progress,
                stop: Some(stop),
                missing: goal.required_ids(),
            });
        }
        // The accepted M0 reducer owns the run state vocabulary; M3 adds no
        // state of its own. A waiting run is a paused run, a run that cannot
        // satisfy its goal is a failed run, and the typed stop reason plus the
        // acceptance field carry the M3 distinctions.
        let run_state = match stop {
            TurnStop::StepLimit | TurnStop::ToolLimit | TurnStop::Deadline => RunState::Running,
            _ => {
                let command = match stop {
                    TurnStop::Canceled => RunCommand::Cancel,
                    TurnStop::NeedsInput | TurnStop::ExternalWait => RunCommand::Pause,
                    TurnStop::Final
                        if self.goal.is_none() || acceptance == AcceptanceState::Satisfied =>
                    {
                        RunCommand::Complete
                    }
                    _ => RunCommand::Fail,
                };
                let next = AgentState::Running
                    .apply(command)
                    .map_or(AgentState::Failed, |transition| transition.next);
                match next {
                    AgentState::Running => RunState::Running,
                    AgentState::Paused => RunState::Paused,
                    AgentState::Completed => RunState::Completed,
                    AgentState::Canceled => RunState::Canceled,
                    AgentState::Idle | AgentState::Disposed | AgentState::Failed => {
                        RunState::Failed
                    }
                }
            }
        };
        if run_state != RunState::Running {
            self.runtime
                .finish_run_record(
                    &result.run_id,
                    result.run_revision,
                    run_state,
                    Some(acceptance.as_str()),
                    stop.as_str(),
                    pending_question.as_ref(),
                )
                .await
                .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
        }

        Ok(TurnOutcome {
            session_id,
            task_id,
            input_id,
            run_id: result.run_id.clone(),
            run_revision: result.run_revision,
            final_text: result.response.clone(),
            steps,
            tool_calls,
            executions,
            stop,
            acceptance,
            goal: goal_report,
            pending_question,
        })
    }

    /// Persist the question a `needs_input` verdict asks for.
    async fn ask_question(
        &self,
        result: &RunResult,
        question: &str,
    ) -> Result<harness_store_sqlite::QuestionRecord, harness_runtime::RuntimeError> {
        let service = HumanInputService::new(Arc::clone(self.runtime.store()));
        service
            .ask(
                AskRequest::for_request(
                    result.session_id.clone(),
                    result.task_id.clone(),
                    &result.run_id,
                    result.step_id.as_str(),
                    question,
                ),
                now_unix_ms(),
            )
            .await
    }

    async fn ask_user(
        &self,
        result: &RunResult,
        call: &NormalizedToolCall,
    ) -> Result<QuestionId, HarnessError> {
        let input = AskUserInput::parse(&call.arguments)?;
        let request_id = if call.call_id.trim().is_empty() {
            result.step_id.as_str()
        } else {
            call.call_id.as_str()
        };
        let mut request = AskRequest::for_request(
            result.session_id.clone(),
            result.task_id.clone(),
            &result.run_id,
            request_id,
            input.question,
        );
        "ask_user".clone_into(&mut request.kind);
        request.payload = json!({"options": input.options});
        let record = HumanInputService::new(Arc::clone(self.runtime.store()))
            .ask(request, now_unix_ms())
            .await
            .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
        Ok(record.question_id)
    }

    /// Resolve one streamed call into an action.
    ///
    /// A built-in name is resolved by the P3 parser first, so an extension can never
    /// shadow a built-in tool with a name of its own; a name the host's catalogue does
    /// not advertise stays the unsupported-tool denial it always was.
    fn resolve_action(&self, call: &NormalizedToolCall) -> Result<CodingToolAction, HarnessError> {
        if coding_tool_names().contains(&call.name.as_str()) {
            return CodingToolAction::from_provider_call(&call.name, &call.arguments);
        }
        let Some(external) = &self.external else {
            return CodingToolAction::from_provider_call(&call.name, &call.arguments);
        };
        let arguments: Value = serde_json::from_str(&call.arguments).map_err(|_| {
            HarnessError::new(
                ErrorCode::ProviderProtocol,
                "provider tool arguments are incomplete or invalid JSON",
            )
        })?;
        external.resolve(&call.name, &arguments).ok_or_else(|| {
            HarnessError::new(
                ErrorCode::PolicyDenied,
                "provider requested a tool this host does not advertise",
            )
        })
    }
}

/// Execute a host-initiated action through the same policy, approval and receipt
/// path used for a model tool call.
///
/// Interactive input prefixes such as `!cmd` are still proposals. Sharing this
/// function keeps protected-path validation, deny/allow ordering, approval
/// panels, and durable receipts identical to the model-tool path.
pub async fn execute_action_with_approval(
    tools: &ToolExecutionService,
    request: ToolRequest,
    options: &TurnOptions,
    sequence: u32,
    observer: &Arc<dyn TurnObserver>,
    cancellation: &CancellationToken,
) -> Result<ToolExecutionView, HarnessError> {
    match gate_action(tools, request, options, sequence, observer, cancellation).await? {
        Gated::Done(result) => result,
        Gated::Ready(ready) => {
            let outcome = tools.run_begun(&ready.begun, cancellation.clone()).await;
            complete_action(tools, ready, outcome, observer, cancellation).await
        }
    }
}

/// A call that has passed its gate: answered already, or with its durable intent
/// committed and its side effect still to run.
#[allow(
    clippy::large_enum_variant,
    reason = "one value per tool call, moved once; boxing the view only adds an allocation"
)]
enum Gated {
    Done(Result<ToolExecutionView, HarnessError>),
    Ready(Box<ReadyAction>),
}

struct ReadyAction {
    begun: Box<crate::service::BegunCall>,
    prepared: PreparedToolRequest,
    granted_gate: Option<(Arc<dyn ApprovalGate>, String)>,
}

/// Everything before the side effect: policy, hooks, the approval (asked of the
/// user when the policy says so) and the durable intent. It runs one call at a
/// time, in the order the model asked, so approvals are asked in that order and
/// intents take their journal sequences in that order.
async fn gate_action(
    tools: &ToolExecutionService,
    request: ToolRequest,
    options: &TurnOptions,
    sequence: u32,
    observer: &Arc<dyn TurnObserver>,
    cancellation: &CancellationToken,
) -> Result<Gated, HarnessError> {
    let prepared = tools.prepare(request).await?;
    let decision = match (tools.decision(&prepared), &options.approvals) {
        (Decision::Ask, ApprovalMode::Auto) => Decision::Allow {
            reason: "explicit auto approval".to_owned(),
        },
        (decision, _) => decision,
    };
    if matches!(&decision, Decision::Blocked(_) | Decision::Deny(_)) {
        return Ok(Gated::Done(
            tools
                .execute_with_cancellation(prepared, None, cancellation.clone())
                .await,
        ));
    }
    if let Some(reason) = tools.run_pre_tool_hooks(&prepared, cancellation).await {
        observer.observe(TurnProgress::Notice(format!("blocked by hook: {reason}")));
        return Ok(Gated::Done(
            tools.record_hook_block(&prepared, &reason).await,
        ));
    }
    notify_action_approval_required(tools, &prepared, &decision, observer, cancellation).await;
    let (approval, granted_gate) = resolve_action_approval(
        tools,
        &prepared,
        decision,
        options,
        sequence,
        observer,
        cancellation,
    )
    .await?;
    match tools.begin(prepared.clone(), approval, cancellation).await {
        Ok(crate::service::Begun::Ready(begun)) => Ok(Gated::Ready(Box::new(ReadyAction {
            begun,
            prepared,
            granted_gate,
        }))),
        Ok(crate::service::Begun::Finished(view)) => {
            if let Some((gate, request_id)) = granted_gate {
                gate.action_completed(&request_id);
            }
            Ok(Gated::Done(Ok(view)))
        }
        Err(error) => {
            if let Some((gate, request_id)) = granted_gate {
                gate.action_completed(&request_id);
            }
            Ok(Gated::Done(Err(error)))
        }
    }
}

/// Everything after the side effect: the receipt, the approval gate's release and
/// the post-tool hooks. Run in call order, so receipts keep the order of intents.
async fn complete_action(
    tools: &ToolExecutionService,
    ready: Box<ReadyAction>,
    outcome: Result<crate::service::Dispatched, HarnessError>,
    observer: &Arc<dyn TurnObserver>,
    cancellation: &CancellationToken,
) -> Result<ToolExecutionView, HarnessError> {
    let ReadyAction {
        begun,
        prepared,
        granted_gate,
    } = *ready;
    let execution = tools.finish_begun(*begun, outcome).await;
    if let Some((gate, request_id)) = granted_gate {
        gate.action_completed(&request_id);
    }
    if execution.is_ok() {
        run_post_tool_hooks(tools, &prepared, observer, cancellation).await;
    }
    execution
}

/// One call of a batch on its way to a result.
#[allow(
    clippy::large_enum_variant,
    reason = "one value per tool call, moved once; boxing the view only adds an allocation"
)]
enum Slot {
    /// Refused before any gate: the text the model gets, and the reason shown.
    Failed(String, String),
    Done(Result<ToolExecutionView, HarnessError>),
    Ready(Box<ReadyAction>),
    Taken,
}

/// Whether a batch holding this call runs one call at a time, as prime-agent runs
/// a batch sequentially when any tool in it is marked `sequential`: the Python
/// kernel keeps state between cells, and writes and processes may depend on the
/// calls before them. Reads, searches, web and MCP lookups run side by side.
fn runs_alone(name: &str) -> bool {
    matches!(
        crate::contracts::effect_class_for(name),
        crate::contracts::EffectClass::Mutating | crate::contracts::EffectClass::Interactive
    ) || matches!(name, "ipython" | "run_process" | "run_shell" | "shell")
}

async fn notify_action_approval_required(
    tools: &ToolExecutionService,
    prepared: &PreparedToolRequest,
    decision: &Decision,
    observer: &Arc<dyn TurnObserver>,
    cancellation: &CancellationToken,
) {
    if matches!(decision, Decision::Ask) {
        for notice in tools
            .run_event_hooks(
                "notification",
                json!({
                    "event": "notification",
                    "notification": "approval_required",
                    "session_id": prepared.request.session_id.as_str(),
                    "task_id": prepared.request.task_id.as_str(),
                    "cwd": prepared.workspace_root_text,
                    "tool": {"name": prepared.final_action.kind().as_str()},
                }),
                cancellation.clone(),
            )
            .await
        {
            observer.observe(TurnProgress::Notice(notice));
        }
    }
}

async fn resolve_action_approval(
    tools: &ToolExecutionService,
    prepared: &PreparedToolRequest,
    decision: Decision,
    options: &TurnOptions,
    sequence: u32,
    observer: &Arc<dyn TurnObserver>,
    cancellation: &CancellationToken,
) -> Result<
    (
        Option<ApprovalGrant>,
        Option<(Arc<dyn ApprovalGate>, String)>,
    ),
    HarnessError,
> {
    let mut granted_gate = None;
    let approval = match decision {
        Decision::Blocked(_) | Decision::Deny(_) => unreachable!("handled before hooks"),
        Decision::Allow { reason } => {
            observer.observe(TurnProgress::Info(format!(
                "allowed by {reason}: {}",
                summarize_action(prepared.action())
            )));
            Some(tools.approve(prepared).await?)
        }
        Decision::Ask => match &options.approvals {
            ApprovalMode::Auto => unreachable!("auto is an allow decision"),
            ApprovalMode::None => None,
            ApprovalMode::Ask(gate) => {
                let proposal = proposal_for(
                    sequence,
                    prepared,
                    &options.workspace_root,
                    ToolExecutionService::approval_diff(prepared)?,
                );
                let request_id = proposal.request_id.clone();
                // A canceled turn does not wait for an answer nobody will give: the
                // action is refused and the cancellation ends the turn.
                let answer = tokio::select! {
                    answer = gate.request(proposal) => answer,
                    () = cancellation.cancelled() => {
                        return Err(HarnessError::new(
                            ErrorCode::ProviderCanceled,
                            "the turn was canceled while the action waited for approval; it was not executed",
                        ));
                    }
                };
                match answer {
                    ApprovalAnswer::Granted => {
                        granted_gate = Some((Arc::clone(gate), request_id));
                        Some(tools.approve(prepared).await?)
                    }
                    ApprovalAnswer::Denied => {
                        return Err(HarnessError::new(
                            ErrorCode::PolicyDenied,
                            "denied by the user; the action was not executed",
                        ));
                    }
                    ApprovalAnswer::Expired => {
                        return Err(HarnessError::new(
                            ErrorCode::ApprovalStale,
                            "the approval request expired before an answer arrived; the action was not executed",
                        ));
                    }
                }
            }
        },
    };
    Ok((approval, granted_gate))
}

async fn run_post_tool_hooks(
    tools: &ToolExecutionService,
    prepared: &PreparedToolRequest,
    observer: &Arc<dyn TurnObserver>,
    cancellation: &CancellationToken,
) {
    let payload = json!({
        "event": "post_tool_use",
        "session_id": prepared.request.session_id.as_str(),
        "task_id": prepared.request.task_id.as_str(),
        "cwd": prepared.workspace_root_text,
        "tool": {
            "name": prepared.final_action.kind().as_str(),
            "args_digest": prepared.action_hash.as_str(),
        },
    });
    for notice in tools
        .run_event_hooks("post_tool_use", payload, cancellation.clone())
        .await
    {
        observer.observe(TurnProgress::Notice(notice));
    }
}

/// Build the proposal the user answers.
fn proposal_for(
    sequence: u32,
    prepared: &PreparedToolRequest,
    workspace_root: &std::path::Path,
    diff: Option<String>,
) -> ApprovalProposal {
    let action = prepared.action();
    let kind = action.kind();
    let mut summary = summarize_action(action);
    if let Some(diff) = diff {
        summary.push('\n');
        summary.push_str(&diff);
    }
    ApprovalProposal {
        request_id: format!(
            "approval-{sequence}-{}",
            short_hash(prepared.action_hash().as_str())
        ),
        action: format!("{kind:?}"),
        summary,
        rule_pattern: tool_pattern_for_action(action),
        workspace: workspace_root.to_path_buf(),
        scope: APPROVAL_SCOPE.to_owned(),
        read_only: kind.is_read_only(),
    }
}

fn short_hash(hash: &str) -> String {
    let hexadecimal = hash.strip_prefix("sha256:").unwrap_or(hash);
    hexadecimal.chars().take(12).collect()
}

/// One-line, secret-free description of the exact action being approved.
fn summarize_action(action: &CodingToolAction) -> String {
    match action {
        CodingToolAction::ReadFile {
            path,
            offset,
            limit,
        } => format!(
            "read {path} (lines {}–{})",
            offset.unwrap_or(0).saturating_add(1),
            offset.unwrap_or(0).saturating_add(u64::from(
                limit.unwrap_or(crate::contracts::READ_FILE_DEFAULT_LINES)
            ))
        ),
        CodingToolAction::ListFiles { path } => {
            format!("list {}", path.as_deref().unwrap_or("."))
        }
        CodingToolAction::SearchText { query, path, .. } => {
            format!("search {query:?} in {}", path.as_deref().unwrap_or("."))
        }
        CodingToolAction::ApplyPatch { path, .. } => format!("patch {path}"),
        CodingToolAction::WriteFile { path, .. } => format!("write {path}"),
        CodingToolAction::EditFile { path, .. } => format!("edit {path}"),
        CodingToolAction::Glob { pattern, path } => {
            format!("glob {pattern:?} in {}", path.as_deref().unwrap_or("."))
        }
        CodingToolAction::RunProcess {
            executable, args, ..
        } => format!("run {executable} {}", args.join(" ")),
        CodingToolAction::RunShell { command, .. } => format!("shell: {command}"),
        CodingToolAction::ReadProcessOutput {
            artifact_id,
            stream,
            offset,
            length,
        } => format!(
            "read captured {} of {artifact_id} at {offset} ({length} bytes)",
            stream.as_str()
        ),
        CodingToolAction::HistorySearch { query, limit } => format!(
            "search history {query:?} (limit {})",
            limit.unwrap_or(HISTORY_SEARCH_DEFAULT_LIMIT)
        ),
        CodingToolAction::HistoryRead {
            source_id, offset, ..
        } => format!("read history source {source_id} at {offset}"),
        CodingToolAction::GitStatus => "git status".to_owned(),
        CodingToolAction::GitDiff { path } => {
            format!("git diff {}", path.as_deref().unwrap_or("."))
        }
        CodingToolAction::GitLog { path, limit } => format!(
            "git log {} (limit {})",
            path.as_deref().unwrap_or("."),
            limit.unwrap_or(GIT_LOG_DEFAULT_LIMIT)
        ),
        CodingToolAction::TaskUpdate { note } => format!("task update: {note}"),
        CodingToolAction::ExternalTool {
            plugin_id,
            tool_name,
            ..
        } => format!("extension {plugin_id}/{tool_name}"),
    }
}

fn sink_for(observer: &Arc<dyn TurnObserver>) -> ProviderEventSink {
    let observer = Arc::clone(observer);
    Arc::new(move |event: ProviderStreamEvent| match event {
        ProviderStreamEvent::TextDelta { text } => observer.observe(TurnProgress::TextDelta(text)),
        ProviderStreamEvent::ThinkingDelta { text } => {
            observer.observe(TurnProgress::ThinkingDelta(text));
        }
        ProviderStreamEvent::Usage {
            prompt_tokens,
            completion_tokens,
            ..
        } => {
            observer.observe(TurnProgress::Usage {
                prompt_tokens,
                completion_tokens,
            });
        }
        _ => {}
    })
}

/// The images a tool returned for the model to see: an external tool's payload may
/// carry `images` as `{mime_type, data}` (base64), which is how the Python REPL hands
/// back what prime-agent's `attach_image` skill loaded.
fn tool_output_images(output: &ToolOutput) -> Vec<harness_providers::ImageAttachment> {
    let ToolOutput::ExternalTool { payload, .. } = output else {
        return Vec::new();
    };
    payload
        .get("images")
        .and_then(Value::as_array)
        .map(|images| {
            images
                .iter()
                .filter_map(|image| {
                    let mime = image.get("mime_type")?.as_str()?;
                    let data = image.get("data")?.as_str()?;
                    let label = image.get("path").and_then(Value::as_str).unwrap_or(mime);
                    Some(harness_providers::ImageAttachment::inline(
                        mime, data, label,
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Short, non-secret summary of the requested arguments for the transcript.
///
/// A JSON object is shown as `key=value` pairs - `list_files path=.`,
/// `search_text query=parser path=src` - instead of the raw `{"path": "."}` the card
/// used to carry, which made every row of a read-heavy turn look like a protocol dump.
/// Long values (file contents, patches) are cut, and anything that is not an object is
/// shown as it came.
fn summarize_arguments(arguments: &str) -> String {
    const VALUE_CHARS: usize = 60;
    let summary = match serde_json::from_str::<Value>(arguments) {
        Ok(Value::Object(fields)) => fields
            .iter()
            .filter(|(_, value)| !value.is_null())
            .map(|(key, value)| {
                let value = match value {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                let value = value.replace(['\n', '\r'], " ");
                let value = if value.chars().count() > VALUE_CHARS {
                    let cut = value.chars().take(VALUE_CHARS).collect::<String>();
                    format!("{cut}…")
                } else {
                    value
                };
                format!("{key}={value}")
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => arguments.replace(['\n', '\r'], " "),
    };
    truncate_text(&summary, 160)
}

/// A stable signature of one requested tool call.
fn call_signature(call: &NormalizedToolCall) -> String {
    format!("{}|{}", call.name, call.arguments)
}

/// Whether the tail of the window is one signature repeated to the limit.
fn repeated_tail(signatures: &[String]) -> bool {
    let Some(last) = signatures.last() else {
        return false;
    };
    signatures.len() >= LOOP_REPEAT_LIMIT
        && signatures
            .iter()
            .rev()
            .take(LOOP_REPEAT_LIMIT)
            .all(|signature| signature == last)
}

/// What one terminal response proved, as typed evidence.
///
/// A check counts only at the digest it observed, so the final criteria are
/// judged against the workspace the run actually left behind rather than
/// against the model's account of it.
fn goal_evidence(result: &RunResult, executions: &[ToolExecutionView]) -> GoalEvidence {
    let mut evidence = GoalEvidence {
        response: result.response.clone(),
        finish_reason: result.finish_reason.clone(),
        truncated: result.finish_reason.is_none(),
        tool_executions: u32::try_from(executions.len()).unwrap_or(u32::MAX),
        ..GoalEvidence::default()
    };
    for view in executions {
        let receipt = view.receipt.as_ref();
        let settled = receipt.is_some_and(|receipt| {
            receipt.outcome_state == harness_types::ToolOutcomeState::Settled
        });
        if settled {
            evidence.successful_tool_executions += 1;
        }
        // The digest after this execution, when it settled one: it is the
        // workspace every later criterion is measured against.
        if let Some(after) = receipt.and_then(|receipt| receipt.after_fingerprint.clone()) {
            evidence.workspace_digest = Some(after);
        }
        match &view.output {
            ToolOutput::ApplyPatch { .. }
            | ToolOutput::WriteFile { .. }
            | ToolOutput::EditFile { .. } => evidence.file_changes += 1,
            ToolOutput::Process {
                exit_code,
                timed_out: false,
                canceled: false,
                ..
            } => {
                let passed = *exit_code == Some(0);
                if passed {
                    evidence.checks_passed += 1;
                }
                if let Some(receipt) = receipt {
                    evidence.checks.push(CheckObservation {
                        command_digest: receipt.input_hash.clone(),
                        workspace_digest: receipt.after_fingerprint.clone(),
                        exit_code: *exit_code,
                        passed,
                    });
                }
            }
            _ => {}
        }
        if receipt.is_some_and(|receipt| receipt.artifact_id.is_some()) {
            evidence.artifacts += 1;
        }
    }
    evidence
}

/// Why a streamed call can never become an action.
///
/// The measured case is a stream that splits one call across frames: a decoder that
/// loses that identity hands back a named call with no arguments and an anonymous
/// call holding them. Neither is executable, and the model has to be told which
/// half was wrong instead of being told the tool was denied.
fn malformed_call(call: &NormalizedToolCall) -> Option<&'static str> {
    if call.name.trim().is_empty() {
        return Some("the function name is missing");
    }
    if serde_json::from_str::<serde_json::Value>(&call.arguments).is_err() {
        return Some("the arguments are not complete JSON");
    }
    None
}

/// Render a bounded tool result as the message the model receives next.
#[allow(
    clippy::too_many_lines,
    reason = "the exhaustive output mapping keeps every tool result visible to the model"
)]
pub(crate) fn render_tool_output(name: &str, output: &ToolOutput) -> String {
    let body = match output {
        ToolOutput::ReadFile {
            path,
            content,
            truncated,
        } => format!(
            "read_file {path}{}:\n{}",
            if *truncated { " (truncated)" } else { "" },
            content
        ),
        ToolOutput::ListFiles { paths, truncated } => format!(
            "list_files{}: {}",
            if *truncated { " (truncated)" } else { "" },
            paths.join(", ")
        ),
        // A count is not a result. `search_text: 1 match(es)` tells the model
        // nothing it can act on, so it searches again with another query and
        // the turn spends its bounds rediscovering what it already found. The
        // hits are already capped and redacted where they are produced, so the
        // rendering here is the same bound the tool applied.
        ToolOutput::SearchText { matches, truncated } => {
            let mut notices = String::new();
            if *truncated {
                let _ = write!(
                    notices,
                    "\n\n[Results stop at {MAX_SEARCH_MATCHES} matches or {}. Narrow the path, glob or query to see the rest.]",
                    format_size(DEFAULT_MAX_BYTES as u64)
                );
            }
            if matches
                .iter()
                .any(|hit| hit.preview.ends_with("... [truncated]"))
            {
                let _ = write!(
                    notices,
                    "\n\n[Some lines truncated to {GREP_MAX_LINE_LENGTH} chars. Use read_file to see full lines.]"
                );
            }
            let hits = matches
                .iter()
                .map(|hit| {
                    format!(
                        "{}:{}:{}: {}",
                        hit.path,
                        hit.line,
                        hit.column,
                        hit.preview.trim()
                    )
                })
                .collect::<Vec<_>>()
                .join(
                    "
",
                );
            format!(
                "search_text{}: {} match(es){}{hits}{notices}",
                if *truncated { " (truncated)" } else { "" },
                matches.len(),
                if matches.is_empty() {
                    ""
                } else {
                    "
"
                }
            )
        }
        ToolOutput::Glob { paths, truncated } => format!(
            "glob{}: {} path(s){}{}",
            if *truncated { " (truncated)" } else { "" },
            paths.len(),
            if paths.is_empty() {
                ""
            } else {
                "
"
            },
            paths.join(
                "
"
            )
        ),
        ToolOutput::ApplyPatch {
            path,
            before_hash,
            after_hash,
        } => format!(
            "apply_patch {path}: {} -> {}",
            before_hash.as_str(),
            after_hash.as_str()
        ),
        ToolOutput::WriteFile {
            path,
            before_hash,
            after_hash,
        } => format!(
            "write_file {path}: {} -> {}",
            before_hash.as_str(),
            after_hash.as_str()
        ),
        ToolOutput::EditFile {
            path,
            before_hash,
            after_hash,
            replacements,
            diff,
        } => format!(
            "edit_file {path}: {} -> {} ({} replacement(s)){}",
            before_hash.as_str(),
            after_hash.as_str(),
            replacements,
            render_edit_diff(diff)
        ),
        ToolOutput::Process {
            executable,
            exit_code,
            timed_out,
            canceled,
            queued,
            tree_cleanup,
            stdout_tail,
            stderr_tail,
            artifact_id,
            capture_truncated,
            ..
        } => {
            let (stdout_limits, stderr_limits) = split_process_limits(stdout_tail, stderr_tail);
            let full_output = |stream: &str| {
                artifact_id.as_ref().map(|id| {
                    let quota = if *capture_truncated {
                        " (captured up to the capture quota)"
                    } else {
                        ""
                    };
                    format!(
                        ". Full output: read_process_output artifact_id={id} stream={stream}{quota}"
                    )
                })
            };
            format!(
                "process {executable} exit={exit_code:?} timed_out={timed_out} canceled={canceled} queued={queued} tree_cleanup={tree_cleanup}\nstdout:\n{}\nstderr:\n{}",
                render_stream_tail(stdout_tail, stdout_limits, full_output("stdout")),
                render_stream_tail(stderr_tail, stderr_limits, full_output("stderr")),
            )
        }
        ToolOutput::ProcessOutput {
            artifact_id,
            stream,
            offset,
            length,
            total_bytes,
            text,
        } => format!(
            "read_process_output {artifact_id} {stream} @{offset} ({length}/{total_bytes} bytes):\n{text}"
        ),
        ToolOutput::HistorySearch { hits, truncated } => format!(
            "history_search{}: {} hit(s)\n{}",
            if *truncated { " (truncated)" } else { "" },
            hits.len(),
            hits.iter()
                .map(|hit| format!(
                    "{} seq={} kind={} availability={} matched={} :: {}",
                    hit.source_id,
                    hit.sequence,
                    hit.kind,
                    hit.availability,
                    hit.matched_terms,
                    hit.preview
                ))
                .collect::<Vec<_>>()
                .join("\n")
        ),
        ToolOutput::HistoryRead {
            source_id,
            sequence,
            source_kind,
            offset,
            length,
            total_bytes,
            text,
        } => format!(
            "history_read {source_id} seq={sequence} kind={source_kind} @{offset} ({length}/{total_bytes} bytes):\n{text}"
        ),
        ToolOutput::Git {
            operation, output, ..
        } => format!("git {operation}:\n{output}"),
        ToolOutput::TaskUpdate { note } => format!("task_update: {note}"),
        // A plugin, MCP or skill tool answers with JSON. A payload that carries its
        // answer as text is handed over as that text; anything else as compact JSON.
        // It used to be the Rust debug form of the whole output value, which a model
        // had to parse through `Object {` and `String(` wrappers.
        ToolOutput::ExternalTool { payload, .. } => {
            match payload.get("text").and_then(Value::as_str) {
                Some(text) => format!("{name}:\n{text}"),
                None => format!(
                    "{name}: {}",
                    serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_owned())
                ),
            }
        }
        ToolOutput::SkillActivated { block } => format!(
            "activated skill {} ({}) on the {} context channel",
            block.id,
            block.digest.as_str(),
            block.channel.as_str()
        ),
        other => format!("{name}: {other:?}"),
    };
    truncate_text(&body, TOOL_RESULT_LIMIT)
}

/// An edit's diff below its summary line, cut to the shared output limits.
fn render_edit_diff(diff: &str) -> String {
    if diff.is_empty() {
        return String::new();
    }
    let cut = truncate_head(diff, TruncationLimits::default());
    if cut.truncated {
        format!(
            "\n{}\n[diff truncated: showing {} of {} lines]",
            cut.content, cut.output_lines, cut.total_lines
        )
    } else {
        format!("\n{}", cut.content)
    }
}

/// Share one tool-output budget between a process's two streams.
///
/// prime-agent writes stdout and stderr into one accumulator and shows its
/// tail. ha keeps the streams apart, so the budget is split instead: a stream
/// that fits in half leaves the rest to the other, and two long streams get
/// half each.
fn split_process_limits(
    stdout: &StreamTail,
    stderr: &StreamTail,
) -> (TruncationLimits, TruncationLimits) {
    let full = TruncationLimits::default();
    let half = TruncationLimits {
        max_lines: full.max_lines / 2,
        max_bytes: full.max_bytes / 2,
    };
    let fits = |stream: &StreamTail| {
        stream.total_lines <= half.max_lines as u64 && stream.total_bytes <= half.max_bytes as u64
    };
    let rest = |stream: &StreamTail| TruncationLimits {
        max_lines: full.max_lines - usize::try_from(stream.total_lines).unwrap_or(0),
        max_bytes: full.max_bytes - usize::try_from(stream.total_bytes).unwrap_or(0),
    };
    if fits(stderr) {
        (rest(stderr), half)
    } else if fits(stdout) {
        (half, rest(stdout))
    } else {
        (half, half)
    }
}

/// The end of one process stream as the model reads it, with prime-agent's
/// notice when lines were left out (the snapshot and `formatOutput` of its
/// bash tool). `full_output` says where the whole stream can be read.
fn render_stream_tail(
    stream: &StreamTail,
    limits: TruncationLimits,
    full_output: Option<String>,
) -> String {
    let cut = truncate_tail(&stream.text, limits);
    // The tail text is only the end of the stream; the totals decide whether
    // the stream as a whole was cut.
    let truncated = stream.total_lines > limits.max_lines as u64
        || stream.total_bytes > limits.max_bytes as u64;
    if !truncated {
        return cut.content;
    }
    let truncated_by =
        cut.truncated_by
            .unwrap_or(if stream.total_bytes > limits.max_bytes as u64 {
                TruncatedBy::Bytes
            } else {
                TruncatedBy::Lines
            });
    let total = stream.total_lines;
    let start = total.saturating_sub(cut.output_lines as u64) + 1;
    // A capture that could not be published has no location; never advertise
    // one that does not exist.
    let location = full_output.unwrap_or_default();
    let notice = if cut.last_line_partial {
        let line_size = if stream.last_line_bytes > 0 {
            format!(" (line is {})", format_size(stream.last_line_bytes))
        } else {
            String::new()
        };
        format!(
            "[Showing last {} of line {start}{line_size}{location}]",
            format_size(cut.output_bytes as u64)
        )
    } else if truncated_by == TruncatedBy::Lines {
        format!("[Showing lines {start}-{total} of {total}{location}]")
    } else {
        format!(
            "[Showing lines {start}-{total} of {total} ({} limit){location}]",
            format_size(limits.max_bytes as u64)
        )
    };
    format!("{}\n\n{notice}", cut.content)
}

fn truncate_text(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let kept: String = text.chars().take(limit).collect();
    let rest = text.chars().count() - limit;
    format!("{kept}\n[truncated: {rest} more characters; request a narrower range]")
}

#[cfg(test)]
mod tool_output_render_tests {
    use super::{
        StreamTail, TruncationLimits, render_stream_tail, render_tool_output, split_process_limits,
    };
    use crate::{ToolOutput, contracts::SearchMatch};
    use harness_types::ContentHash;

    fn stream(text: &str) -> StreamTail {
        StreamTail {
            text: text.to_owned(),
            total_bytes: text.len() as u64,
            total_lines: text.split('\n').count() as u64,
            last_line_bytes: text.rsplit('\n').next().map_or(0, str::len) as u64,
        }
    }

    fn limits(max_lines: usize, max_bytes: usize) -> TruncationLimits {
        TruncationLimits {
            max_lines,
            max_bytes,
        }
    }

    #[test]
    fn a_stream_within_its_limits_is_shown_whole_without_a_notice() {
        assert_eq!(
            render_stream_tail(&stream("a\nb"), limits(10, 100), Some(". x".to_owned())),
            "a\nb"
        );
    }

    #[test]
    fn a_long_stream_shows_its_last_lines_and_where_the_rest_is() {
        let rendered = render_stream_tail(
            &stream("1\n2\n3\n4\n5"),
            limits(2, 100),
            Some(". Full output: read_process_output artifact_id=a1 stream=stdout".to_owned()),
        );
        assert_eq!(
            rendered,
            "4\n5\n\n[Showing lines 4-5 of 5. Full output: read_process_output artifact_id=a1 stream=stdout]"
        );
    }

    #[test]
    fn a_byte_cut_names_the_limit_and_a_missing_capture_names_no_location() {
        let rendered = render_stream_tail(&stream("aaa\nbbb\nccc"), limits(10, 9), None);
        assert_eq!(rendered, "bbb\nccc\n\n[Showing lines 2-3 of 3 (9B limit)]");
    }

    #[test]
    fn an_oversized_last_line_shows_its_end_and_its_size() {
        let rendered = render_stream_tail(&stream(&"z".repeat(30)), limits(10, 8), None);
        assert_eq!(
            rendered,
            "zzzzzzzz\n\n[Showing last 8B of line 1 (line is 30B)]"
        );
    }

    /// The totals, not the kept text, decide the line numbers: the tail may
    /// be only the end of a stream far longer than the window.
    #[test]
    fn line_numbers_count_from_the_whole_stream() {
        let tail = StreamTail {
            text: "x\ny".to_owned(),
            total_bytes: 1_000_000,
            total_lines: 5000,
            last_line_bytes: 1,
        };
        let rendered = render_stream_tail(&tail, limits(10, 100), None);
        assert_eq!(
            rendered,
            "x\ny\n\n[Showing lines 4999-5000 of 5000 (100B limit)]"
        );
    }

    #[test]
    fn the_two_streams_share_one_budget() {
        let full = TruncationLimits::default();
        let short = stream("warning");
        let long = StreamTail {
            text: String::new(),
            total_bytes: 10_000_000,
            total_lines: 100_000,
            last_line_bytes: 0,
        };
        let (stdout, stderr) = split_process_limits(&long, &short);
        assert_eq!(stdout.max_bytes, full.max_bytes - 7);
        assert_eq!(stdout.max_lines, full.max_lines - 1);
        assert_eq!(stderr.max_bytes, full.max_bytes / 2);

        let (stdout, stderr) = split_process_limits(&short, &long);
        assert_eq!(stdout.max_bytes, full.max_bytes / 2);
        assert_eq!(stderr.max_bytes, full.max_bytes - 7);

        let (stdout, stderr) = split_process_limits(&long, &long);
        assert_eq!(
            (stdout.max_bytes, stderr.max_bytes),
            (full.max_bytes / 2, full.max_bytes / 2)
        );
    }

    #[test]
    fn process_output_is_rendered_from_the_stream_tails() {
        let output = ToolOutput::Process {
            executable: "sh".to_owned(),
            shell: None,
            exit_code: Some(1),
            timed_out: false,
            canceled: false,
            queued: false,
            tree_cleanup_confirmed: false,
            tree_cleanup: "reaped_on_exit".to_owned(),
            stdout: "HEAD ONLY".to_owned(),
            stderr: String::new(),
            stdout_truncated: true,
            stderr_truncated: false,
            artifact_id: Some("art-1".to_owned()),
            captured_bytes: 0,
            capture_hash: None,
            capture_truncated: false,
            capture_tail: String::new(),
            stdout_tail: stream("the end"),
            stderr_tail: stream("error: boom"),
        };
        let rendered = render_tool_output("run_shell", &output);
        assert!(
            rendered.contains("stdout:\nthe end\nstderr:\nerror: boom"),
            "{rendered}"
        );
        assert!(!rendered.contains("HEAD ONLY"), "{rendered}");
    }

    #[test]
    fn an_edit_result_carries_its_diff() {
        let output = ToolOutput::EditFile {
            path: "f.txt".to_owned(),
            before_hash: ContentHash::from_bytes(b"a"),
            after_hash: ContentHash::from_bytes(b"b"),
            replacements: 1,
            diff: "-1 a\n+1 b".to_owned(),
        };
        let rendered = render_tool_output("edit_file", &output);
        assert!(
            rendered.ends_with("(1 replacement(s))\n-1 a\n+1 b"),
            "{rendered}"
        );
    }

    #[test]
    fn a_cut_search_says_so_and_how_to_see_full_lines() {
        let output = ToolOutput::SearchText {
            matches: vec![SearchMatch {
                path: "a.rs".to_owned(),
                line: 1,
                column: 1,
                preview: "long... [truncated]".to_owned(),
                context: Vec::new(),
            }],
            truncated: true,
        };
        let rendered = render_tool_output("search_text", &output);
        assert!(
            rendered.contains("[Results stop at 512 matches or 50.0KB."),
            "{rendered}"
        );
        assert!(
            rendered
                .ends_with("[Some lines truncated to 500 chars. Use read_file to see full lines.]"),
            "{rendered}"
        );
    }
}

#[cfg(test)]
mod transcript_budget_tests {
    use super::{ELIDED_RESULT, trim_transcript};
    use harness_providers::{MessageRole, ProviderMessage, ProviderToolCall};

    fn step(index: usize, result_bytes: usize) -> Vec<ProviderMessage> {
        let id = format!("call-{index}");
        vec![
            ProviderMessage::assistant_with_calls(
                format!("step {index}"),
                vec![ProviderToolCall::new(id.clone(), "read_file", "{}")],
            ),
            ProviderMessage::tool_result(id, "x".repeat(result_bytes)),
        ]
    }

    #[test]
    fn the_oldest_results_are_shortened_first_and_the_newest_stay_whole() {
        let mut transcript = (0..10)
            .flat_map(|index| step(index, 30_000))
            .collect::<Vec<_>>();
        let elided = trim_transcript(&mut transcript, 200_000, 6);
        assert!(elided > 0);
        let results = transcript
            .iter()
            .filter(|message| message.role == MessageRole::Tool)
            .collect::<Vec<_>>();
        // Oldest first, newest never.
        assert_eq!(results[0].content, ELIDED_RESULT);
        for recent in &results[results.len() - 6..] {
            assert_eq!(recent.content.len(), 30_000);
        }
        // Within budget, and every call still has its result.
        let total: usize = transcript.iter().map(|message| message.content.len()).sum();
        assert!(total <= 200_000, "{total}");
        assert!(harness_providers::validate_transcript(&transcript).is_ok());
        // Assistant text is never touched.
        assert!(transcript[0].content.starts_with("step 0"));
    }

    #[test]
    fn a_transcript_within_budget_is_left_alone() {
        let mut transcript = (0..3)
            .flat_map(|index| step(index, 1_000))
            .collect::<Vec<_>>();
        let before = transcript.clone();
        assert_eq!(trim_transcript(&mut transcript, 200_000, 6), 0);
        assert_eq!(transcript, before);
    }
}
