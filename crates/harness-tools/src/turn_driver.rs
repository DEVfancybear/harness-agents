//! Bounded model -> tool -> model loop (`HA_LAUNCH` H04, gate G2).
//!
//! The P2 runtime admits one user input and calls the provider once; the P3 loop
//! executed the requested tools but never sent their results back. This module
//! owns the missing loop as an application-layer driver: one admission per user
//! message (the runtime keeps that authority), tool execution through the
//! existing policy/approval/receipt gate, and a bounded number of continuation
//! steps.

use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use harness_providers::{
    CancellationToken, MessageRole, NormalizedToolCall, ProviderMessage, ProviderStreamEvent,
};
use harness_runtime::{ProviderEventSink, RunRequest, RunResult, RuntimeService};
use harness_types::{ErrorCode, HarnessError};
use serde_json::Value;

use crate::{
    CodingToolAction, PreparedToolRequest, ToolExecutionService, ToolExecutionView, ToolOutput,
    ToolRequest, coding_tool_names,
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
        /// Why the call did not execute, for the human reading the transcript.
        ///
        /// The model is told the same thing in its tool result, but a card that only
        /// says `failed 962ms` leaves the person who has to answer the next approval
        /// unable to tell a malformed call from a policy denial.
        detail: Option<String>,
    },
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
    pub final_text: String,
    pub steps: u32,
    pub tool_calls: u32,
    pub executions: Vec<ToolExecutionView>,
    pub stop: TurnStop,
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

/// Longest tool result text handed back to the model.
const TOOL_RESULT_LIMIT: usize = 4000;

/// Bounded model -> tool -> model loop.
#[derive(Clone)]
pub struct TurnDriver {
    runtime: Arc<RuntimeService>,
    tools: ToolExecutionService,
    external: Option<ExternalTools>,
}

impl TurnDriver {
    #[must_use]
    pub fn new(runtime: Arc<RuntimeService>, tools: ToolExecutionService) -> Self {
        Self {
            runtime,
            tools,
            external: None,
        }
    }

    /// Advertise the external tools of trusted extensions to this driver.
    #[must_use]
    pub fn with_external(mut self, external: ExternalTools) -> Self {
        self.external = Some(external);
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
        self.run_turn_inner(None, request, options, observer, cancellation)
            .await
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
        self.run_turn_inner(
            Some(source_session_id),
            request,
            options,
            observer,
            cancellation,
        )
        .await
    }

    // One linear pass per step with an explicit bound check between them; the
    // length is the wiring, not hidden branching logic.
    #[allow(clippy::too_many_lines)]
    async fn run_turn_inner(
        &self,
        source_session_id: Option<&harness_types::SessionId>,
        request: RunRequest,
        options: TurnOptions,
        observer: Arc<dyn TurnObserver>,
        cancellation: CancellationToken,
    ) -> Result<TurnOutcome, HarnessError> {
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
        let stop = loop {
            if result.tool_calls.is_empty() {
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
            let names: Vec<String> = result
                .tool_calls
                .iter()
                .map(|call| call.name.clone())
                .collect();
            appended.push(ProviderMessage::new(
                MessageRole::Assistant,
                format!("requested tool calls: {}", names.join(", ")),
            ));
            for call in result.tool_calls.clone() {
                tool_calls += 1;
                let name = call.name.clone();
                observer.observe(TurnProgress::ToolStarted {
                    name: name.clone(),
                    summary: summarize_arguments(&call.arguments),
                });
                // A call the model never named, or whose arguments never became JSON,
                // is not an action. Rejecting it here says so in one sentence; the
                // execution gate would otherwise answer with a policy denial that
                // reads as if the tool itself had been refused.
                if let Some(reason) = malformed_call(&call) {
                    observer.observe(TurnProgress::ToolSettled {
                        name: name.clone(),
                        ok: false,
                        detail: Some(reason.to_owned()),
                    });
                    appended.push(ProviderMessage::new(
                        MessageRole::Tool,
                        format!(
                            "tool call {name:?} was not executed: {reason}; re-issue it with a function name and complete JSON arguments"
                        ),
                    ));
                    continue;
                }
                match self
                    .execute_call(&result, &call, &options, tool_calls)
                    .await
                {
                    Ok(view) => {
                        observer.observe(TurnProgress::ToolSettled {
                            name: name.clone(),
                            ok: true,
                            detail: None,
                        });
                        appended.push(ProviderMessage::new(
                            MessageRole::Tool,
                            render_tool_output(&name, &view.output),
                        ));
                        executions.push(view);
                    }
                    Err(error) => {
                        // A failed tool is reported back to the model instead of
                        // ending the turn: that is what lets it fix its own call.
                        observer.observe(TurnProgress::ToolSettled {
                            name: name.clone(),
                            ok: false,
                            detail: Some(error.to_string()),
                        });
                        appended.push(ProviderMessage::new(
                            MessageRole::Tool,
                            format!("tool {name} failed: {error}"),
                        ));
                    }
                }
            }

            steps += 1;
            if started.elapsed() >= options.limits.deadline {
                break TurnStop::Deadline;
            }
            observer.observe(TurnProgress::StepStarted { step: steps });
            result = self
                .runtime
                .continue_run(
                    request.clone(),
                    appended,
                    cancellation.clone(),
                    Some(sink_for(&observer)),
                )
                .await
                .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
        };

        Ok(TurnOutcome {
            session_id,
            task_id,
            input_id,
            final_text: result.response.clone(),
            steps,
            tool_calls,
            executions,
            stop,
        })
    }

    async fn execute_call(
        &self,
        result: &RunResult,
        call: &NormalizedToolCall,
        options: &TurnOptions,
        sequence: u32,
    ) -> Result<ToolExecutionView, HarnessError> {
        let action = self.resolve_action(call)?;
        let prepared = self
            .tools
            .prepare(ToolRequest::new(
                result.session_id.clone(),
                result.task_id.clone(),
                options.actor_id.clone(),
                options.workspace_root.clone(),
                action,
            ))
            .await?;
        let approval = match &options.approvals {
            ApprovalMode::Auto => Some(self.tools.approve(&prepared).await?),
            ApprovalMode::None => None,
            ApprovalMode::Ask(gate) => {
                let proposal = proposal_for(sequence, &prepared, &options.workspace_root);
                let answered = gate.request(proposal).await;
                match answered {
                    ApprovalAnswer::Granted => Some(self.tools.approve(&prepared).await?),
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
        };
        self.tools.execute(prepared, approval).await
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

/// Build the proposal the user answers.
fn proposal_for(
    sequence: u32,
    prepared: &PreparedToolRequest,
    workspace_root: &std::path::Path,
) -> ApprovalProposal {
    let action = prepared.action();
    let kind = action.kind();
    ApprovalProposal {
        request_id: format!(
            "approval-{sequence}-{}",
            short_hash(prepared.action_hash().as_str())
        ),
        action: format!("{kind:?}"),
        summary: summarize_action(action),
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
        CodingToolAction::ReadFile { path } => format!("read {path}"),
        CodingToolAction::ListFiles { path } => {
            format!("list {}", path.as_deref().unwrap_or("."))
        }
        CodingToolAction::SearchText { query, path } => {
            format!("search {query:?} in {}", path.as_deref().unwrap_or("."))
        }
        CodingToolAction::ApplyPatch { path, .. } => format!("patch {path}"),
        CodingToolAction::RunProcess {
            executable, args, ..
        } => format!("run {executable} {}", args.join(" ")),
        CodingToolAction::RunShell { command, .. } => format!("shell: {command}"),
        CodingToolAction::GitStatus => "git status".to_owned(),
        CodingToolAction::GitDiff { path } => {
            format!("git diff {}", path.as_deref().unwrap_or("."))
        }
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
    Arc::new(move |event: ProviderStreamEvent| {
        if let ProviderStreamEvent::TextDelta { text } = event {
            observer.observe(TurnProgress::TextDelta(text));
        }
    })
}

/// Short, non-secret summary of the requested arguments for the transcript.
fn summarize_arguments(arguments: &str) -> String {
    truncate_text(&arguments.replace(['\n', '\r'], " "), 160)
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
fn render_tool_output(name: &str, output: &ToolOutput) -> String {
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
        ToolOutput::SearchText { matches, truncated } => format!(
            "search_text{}: {} match(es)",
            if *truncated { " (truncated)" } else { "" },
            matches.len()
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
        ToolOutput::Process {
            executable,
            exit_code,
            timed_out,
            canceled,
            stdout,
            stderr,
            ..
        } => format!(
            "process {executable} exit={exit_code:?} timed_out={timed_out} canceled={canceled}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        ),
        ToolOutput::Git {
            operation, output, ..
        } => format!("git {operation}:\n{output}"),
        ToolOutput::TaskUpdate { note } => format!("task_update: {note}"),
        other => format!("{name}: {other:?}"),
    };
    truncate_text(&body, TOOL_RESULT_LIMIT)
}

fn truncate_text(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let kept: String = text.chars().take(limit).collect();
    format!("{kept}\n[truncated]")
}
