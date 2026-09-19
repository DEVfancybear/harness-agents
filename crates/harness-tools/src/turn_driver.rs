//! Bounded model -> tool -> model loop (`HA_LAUNCH` H04, gate G2).
//!
//! The P2 runtime admits one user input and calls the provider once; the P3 loop
//! executed the requested tools but never sent their results back. This module
//! owns the missing loop as an application-layer driver: one admission per user
//! message (the runtime keeps that authority), tool execution through the
//! existing policy/approval/receipt gate, and a bounded number of continuation
//! steps.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use harness_providers::{
    CancellationToken, MessageRole, NormalizedToolCall, ProviderMessage, ProviderStreamEvent,
};
use harness_runtime::{ProviderEventSink, RunRequest, RunResult, RuntimeService};
use harness_types::HarnessError;

use crate::{CodingToolAction, ToolExecutionService, ToolExecutionView, ToolOutput, ToolRequest};

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
    StepStarted { step: u32 },
    ToolStarted { name: String, summary: String },
    ToolSettled { name: String, ok: bool },
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

/// How tool approvals are handled inside this turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApprovalMode {
    /// Grant the prepared action (used by fixtures and explicit auto runs).
    Auto,
    /// Never grant a blanket approval; a gated action fails closed.
    None,
}

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

/// Longest tool result text handed back to the model.
const TOOL_RESULT_LIMIT: usize = 4000;

/// Bounded model -> tool -> model loop.
#[derive(Clone)]
pub struct TurnDriver {
    runtime: Arc<RuntimeService>,
    tools: ToolExecutionService,
}

impl TurnDriver {
    #[must_use]
    pub fn new(runtime: Arc<RuntimeService>, tools: ToolExecutionService) -> Self {
        Self { runtime, tools }
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
        let mut steps = 0_u32;

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
            steps += 1;
            if result.tool_calls.is_empty() {
                break TurnStop::Final;
            }
            if cancellation.is_cancelled() {
                break TurnStop::Canceled;
            }
            if started.elapsed() >= options.limits.deadline {
                break TurnStop::Deadline;
            }
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
                match self.execute_call(&result, &call, &options).await {
                    Ok(view) => {
                        observer.observe(TurnProgress::ToolSettled {
                            name: name.clone(),
                            ok: true,
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
    ) -> Result<ToolExecutionView, HarnessError> {
        let action = CodingToolAction::from_provider_call(&call.name, &call.arguments)?;
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
        let approval = match options.approvals {
            ApprovalMode::Auto => Some(self.tools.approve(&prepared).await?),
            ApprovalMode::None => None,
        };
        self.tools.execute(prepared, approval).await
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
