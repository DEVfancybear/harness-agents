//! Delegated children of an interactive session: prime-agent's sub-agents.
//!
//! A child belongs to the session, not to the turn that started it, as a
//! prime-agent child runs apart from its parent's turn (`_startRlmChildRun`):
//! `rlm.spawn` - and `delegate` with `wait: false` - return as soon as the child is
//! admitted, the parent's turn may end, and when the child settles the parent is
//! told with prime-agent's terminal notice (`[child-failed ...]`,
//! `[child-exited: ...]`), which opens a turn of its own when the parent is idle.
//! Ctrl+C stops the parent's turn, not its children; `/agents stop`, `/new` and a
//! resume into another conversation stop them.
//!
//! Children write through the session's [`SharedStore`]: the store stays open while
//! a turn or a child holds a lease on it.

use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use harness_orchestrator::{
    AgentRole, BudgetLedger, BudgetUsage, DelegatedOutcome, DelegatedResult, DelegationBudget,
    DelegationGrants, DirtyReason, GrantAction, InputInspection, OrchestratorError,
    SchedulerConfig, TaskBrief, VerifiedSnapshot, WorkerBackend, WorkerOutcome, WorkerRequest,
    WorkerScheduler, WorkspaceManager, WorktreeRecord,
};
use harness_providers::{CancellationToken, ModelProvider};
use harness_runtime::{RunInbox, RunRequest, RuntimeConfig, RuntimeService};
use harness_store_sqlite::{SqliteStore, WorktreeRecordRow};
use harness_tools::{
    ApprovalAnswer, ApprovalGate, ApprovalMode, ApprovalProposal, CodingToolAction,
    ExternalToolCatalog, ExternalToolDispatcher, ExternalTools, ToolExecutionService, ToolOutput,
    ToolPatternRule, ToolPolicy, TurnDriver, TurnLimits, TurnObserver, TurnOptions, TurnProgress,
    TurnStop, coding_tool_schemas,
};
use harness_types::{
    AgentProfileId, AgentRunId, ContentHash, ErrorCode, HarnessError, InputId, SessionId,
    SourceAuthority, TaskId, WorkspaceObservation,
};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use super::cost::{CostTracker, ModelPrice, Usage as CostUsage};
use super::events::SessionEvent;
use super::repl::{HostReply, HostRequests};
use super::store_lease::SharedStore;

const MAX_BRIEF_BYTES: usize = 8 * 1024;
const CHILD_READ_TOOLS: &[&str] = &[
    "read_file",
    "list_files",
    "search_text",
    "glob",
    "git_status",
    "git_diff",
    "git_log",
];
const EXPLORER_DENY_TOOLS: &[&str] = &[
    "apply_patch",
    "write_file",
    "edit_file",
    "run_process",
    "run_shell",
    "read_process_output",
    "history_search",
    "history_read",
    "task_update",
    "external_tool",
    "ask_user",
];

/// Longest child answer a notice, `rlm.collect` and `rlm.list_subagents` carry.
/// prime-agent's notice keeps 160 characters and leaves the rest to `collect`; a
/// parent here may have no kernel to collect with, so the notice carries the answer.
const RLM_ANSWER_MAX_CHARS: usize = 8_000;

/// prime-agent's agent-message bounds (`agent-messages.ts`).
const MAX_MESSAGE_CHARS: usize = 16_384;
const RATE_CAPACITY: f64 = 3.0;
const RATE_REFILL: Duration = Duration::from_secs(1);
const AGENT_FAMILY_REACH_ERROR: &str = "Agent reach is limited to parent, siblings, and children";

/// prime-agent's progress-note bounds (`rlm.progress_note`).
const MAX_NOTE_CHARS: usize = 512;
const NOTE_INTERVAL: Duration = Duration::from_secs(10);
const NOTE_RING: usize = 5;

/// The parent's web tools, which a child inherits: its catalog and dispatcher.
pub type ChildWebTools = (ExternalTools, Arc<dyn ExternalToolDispatcher>);

/// The model a child runs on.
#[derive(Clone)]
pub struct ChildModel {
    pub provider: Arc<dyn ModelProvider>,
    /// `provider/id`, as the catalog names it.
    pub reference: String,
    pub price: Option<ModelPrice>,
}

/// Finds the model a child asks for, as prime-agent's
/// `_resolveRlmSubagentModel` does.
pub trait ChildModels: Send + Sync {
    /// The configured model for children (`[agents] default_model`), if any.
    fn default_model(&self) -> Option<String>;

    /// A provider for a catalog model: an exact `provider/id`, or an id no
    /// other provider shares.
    ///
    /// # Errors
    /// The model is not in the catalog, or its provider has no credential.
    fn resolve(&self, reference: &str) -> Result<ChildModel, String>;
}

/// What a turn gives the children it starts.
#[derive(Clone)]
pub struct ChildLaunch {
    /// The parent's model; a child runs on it unless it asks for another.
    pub model: ChildModel,
    pub models: Option<Arc<dyn ChildModels>>,
    pub runtime_config: RuntimeConfig,
    pub workspace_root: PathBuf,
    pub workspace: WorkspaceObservation,
    pub hooks: Vec<harness_tools::ConfiguredToolHook>,
    pub parent_policy: ToolPolicy,
    /// A child is a full agent: it gets the parent turn's steps, tool calls and
    /// deadline, as prime-agent's children run under the normal turn limits.
    pub child_limits: TurnLimits,
    /// The parent's web tools. A child inherits them, as a prime-agent child
    /// inherits its parent's tools: a research brief sent to an explorer without
    /// them could only be answered from the local files.
    pub web: Option<ChildWebTools>,
}

impl ChildLaunch {
    /// This launch on the model a child asked for: the requested one, else the
    /// configured default, else the parent's. A model that cannot be used fails
    /// the spawn rather than running the child on something else.
    fn for_model(&self, requested: Option<&str>) -> Result<Self, String> {
        let reference = requested
            .map(str::trim)
            .filter(|reference| !reference.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                self.models
                    .as_ref()
                    .and_then(|models| models.default_model())
            });
        let Some(reference) = reference else {
            return Ok(self.clone());
        };
        if reference.eq_ignore_ascii_case(&self.model.reference) {
            return Ok(self.clone());
        }
        let models = self
            .models
            .as_ref()
            .ok_or_else(|| format!("Requested subagent model \"{reference}\" is not available"))?;
        let model = models.resolve(&reference)?;
        Ok(Self {
            model,
            ..self.clone()
        })
    }
}

/// How a child stands.
#[derive(Clone, Debug)]
enum ChildState {
    Running,
    Done { answer: String, detail: Value },
    Failed { error: String },
    Cancelled { reason: String },
}

impl ChildState {
    const fn settled(&self) -> bool {
        !matches!(self, Self::Running)
    }
}

struct ChildRecord {
    task_id: TaskId,
    name: String,
    role: AgentRole,
    model: String,
    started: Instant,
    duration_ms: Option<i64>,
    state: ChildState,
    cancellation: CancellationToken,
    cancel_reason: Option<String>,
    /// Callers waiting for this child's result right now (`delegate`,
    /// `rlm.collect`). A result somebody is waiting for needs no notice.
    waiters: usize,
    deleted: bool,
    suppress_notice: bool,
    /// The child sent its parent a message since its task began.
    replied: bool,
    session_id: Option<SessionId>,
    /// Messages sent before the child's run opened its inbox.
    backlog: Vec<String>,
    notes: VecDeque<String>,
    last_note: Option<Instant>,
    activity: String,
    tool_calls: u32,
    cost: String,
}

impl ChildRecord {
    fn elapsed_ms(&self) -> i64 {
        self.duration_ms.unwrap_or_else(|| {
            i64::try_from(self.started.elapsed().as_millis()).unwrap_or(i64::MAX)
        })
    }

    /// prime-agent's `list_subagents` row.
    fn row(&self, session_dir: &str) -> Value {
        let (status, answer) = match &self.state {
            ChildState::Running => ("running", None),
            ChildState::Done { answer, .. } => ("completed", Some(clip(answer))),
            ChildState::Failed { .. } | ChildState::Cancelled { .. } => ("error", None),
        };
        json!({
            "rlm_child_id": self.task_id.as_str(),
            "session_name": self.name,
            "session_dir": session_dir,
            "status": status,
            "model": self.model,
            "activity": {"kind": self.activity},
            "tool_use_count": self.tool_calls,
            "duration_ms": self.elapsed_ms(),
            "answer_preview": answer,
            "replied_since_task": self.replied,
            "progress_note": self.notes.back(),
        })
    }

    /// prime-agent's `RLMChildResult`.
    fn result(&self, session_dir: &str) -> Value {
        let (status, answer, error) = match &self.state {
            ChildState::Running => ("running", None, None),
            ChildState::Done { answer, .. } => ("done", Some(clip(answer)), None),
            ChildState::Failed { error } => ("error", None, Some(error.clone())),
            ChildState::Cancelled { reason } => ("cancelled", None, Some(reason.clone())),
        };
        json!({
            "rlm_child_id": self.task_id.as_str(),
            "session_name": self.name,
            "session_dir": session_dir,
            "status": status,
            "settled": self.state.settled(),
            "answer_preview": answer,
            "error": error,
            "duration_ms": self.state.settled().then(|| self.elapsed_ms()),
            "tool_use_count": self.tool_calls,
            "replied_since_task": self.replied,
        })
    }

    /// One line of `/agents`.
    fn line(&self) -> String {
        let state = match &self.state {
            ChildState::Running => format!("running, {}", self.activity),
            ChildState::Done { .. } => "completed".to_owned(),
            ChildState::Failed { error } => format!("failed: {}", first_line(error)),
            ChildState::Cancelled { reason } => format!("cancelled: {}", first_line(reason)),
        };
        let mut line = format!(
            "{} · {} · {} · {state} · {}s · {} tool call(s) · cost {}",
            self.name,
            self.role.as_str(),
            self.model,
            self.elapsed_ms() / 1000,
            self.tool_calls,
            self.cost,
        );
        if let Some(note) = self.notes.back() {
            line.push_str(" · note: ");
            line.push_str(first_line(note));
        }
        line
    }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

/// prime-agent's terminal notice for a settled child (`core/messages.ts`), or
/// `None` when the child already told its parent what it found.
fn terminal_notice(name: &str, state: &ChildState, replied: bool) -> Option<String> {
    match state {
        ChildState::Running => None,
        ChildState::Failed { error } => Some(format!("[child-failed child:{name}]\n\n{error}")),
        ChildState::Cancelled { reason } => Some(if reason.trim().is_empty() {
            format!("[child-exited: cancelled child:{name}]")
        } else {
            format!("[child-exited: cancelled child:{name}]\n\n{reason}")
        }),
        ChildState::Done { .. } if replied => None,
        ChildState::Done { answer, .. } => Some(if answer.trim().is_empty() {
            format!("[child-exited: no-reply child:{name}]")
        } else {
            format!(
                "[child-exited: no-reply child:{name}]\n\nLast assistant text: {}",
                clip(answer)
            )
        }),
    }
}

/// What the session's children share with the workers that run them.
struct AgentsShared {
    store: Arc<SharedStore>,
    sender: UnboundedSender<SessionEvent>,
    approval_gate: Arc<dyn ApprovalGate>,
    ledger: Mutex<Option<Arc<BudgetLedger>>>,
    launches: Mutex<HashMap<TaskId, ChildLaunch>>,
    children: Mutex<Vec<ChildRecord>>,
    changed: tokio::sync::Notify,
    buckets: Mutex<HashMap<String, (f64, Instant)>>,
}

impl AgentsShared {
    fn update<R>(&self, task_id: &TaskId, change: impl FnOnce(&mut ChildRecord) -> R) -> Option<R> {
        let mut children = self.children.lock().ok()?;
        let child = children
            .iter_mut()
            .find(|child| &child.task_id == task_id)?;
        Some(change(child))
    }

    /// Record a settled worker and tell the parent when nobody is waiting for it.
    fn settle(&self, task_id: &TaskId, outcome: WorkerOutcome) {
        let notice = self.update(task_id, |child| {
            child.duration_ms = Some(child.elapsed_ms());
            child.state = if child.cancellation.is_cancelled() {
                ChildState::Cancelled {
                    reason: child.cancel_reason.clone().unwrap_or_default(),
                }
            } else {
                outcome_state(child.role, outcome)
            };
            if let ChildState::Done { detail, .. } = &child.state
                && let Some(count) = detail["tool_calls"].as_u64()
            {
                child.tool_calls = u32::try_from(count).unwrap_or(u32::MAX);
            }
            let notice = (child.waiters == 0 && !child.suppress_notice)
                .then(|| terminal_notice(&child.name, &child.state, child.replied))
                .flatten();
            notice.map(|notice| (child.name.clone(), notice))
        });
        self.changed.notify_waiters();
        if let Some(Some((name, notice))) = notice {
            let _ = self
                .sender
                .send(SessionEvent::ChildSettled { name, notice });
        }
    }

    /// Wait until `task_id` settles, `deadline` passes or `cancel` fires.
    ///
    /// # Errors
    /// `cancel` fired first; the child keeps running and reports when it settles.
    async fn wait(
        &self,
        task_id: &TaskId,
        deadline: Option<tokio::time::Instant>,
        cancel: Option<&CancellationToken>,
    ) -> Result<Option<ChildState>, HarnessError> {
        struct Waiting<'a>(&'a AgentsShared, &'a TaskId);
        impl Drop for Waiting<'_> {
            fn drop(&mut self) {
                self.0.update(self.1, |child| {
                    child.waiters = child.waiters.saturating_sub(1);
                });
            }
        }
        if self.update(task_id, |child| child.waiters += 1).is_none() {
            return Ok(None);
        }
        let _waiting = Waiting(self, task_id);
        let never = CancellationToken::new();
        let cancel = cancel.unwrap_or(&never);
        let far = tokio::time::Instant::now() + Duration::from_hours(24 * 365);
        loop {
            let changed = self.changed.notified();
            let state = self.update(task_id, |child| child.state.clone());
            match state {
                None => return Ok(None),
                Some(state) if state.settled() => return Ok(Some(state)),
                Some(_) => {}
            }
            tokio::select! {
                () = changed => {}
                () = tokio::time::sleep_until(deadline.unwrap_or(far)) => {
                    return Ok(self.update(task_id, |child| child.state.clone()));
                }
                () = cancel.cancelled() => {
                    return Err(HarnessError::new(
                        ErrorCode::ProviderCanceled,
                        "parent canceled delegated work; the child keeps running and reports when it finishes",
                    ));
                }
            }
        }
    }

    /// prime-agent's per-sender token bucket: three messages, one more each second.
    fn take_token(&self, sender: &str) -> Result<(), String> {
        let mut buckets = self
            .buckets
            .lock()
            .map_err(|_| "agent messaging is unavailable".to_owned())?;
        let now = Instant::now();
        let (tokens, at) = buckets
            .entry(sender.to_owned())
            .or_insert((RATE_CAPACITY, now));
        let refill = now.duration_since(*at).as_secs_f64() / RATE_REFILL.as_secs_f64();
        *tokens = (*tokens + refill).min(RATE_CAPACITY);
        *at = now;
        if *tokens < 1.0 {
            return Err(
                "agent message rate limit reached; wait a second and send again".to_owned(),
            );
        }
        *tokens -= 1.0;
        Ok(())
    }

    /// Deliver `text` to a running child: into its run at the next step, or kept
    /// for its first step when its run has not opened yet.
    async fn deliver_to_child(
        &self,
        task_id: &TaskId,
        text: String,
    ) -> Result<&'static str, String> {
        let session = self
            .update(task_id, |child| {
                if child.state.settled() {
                    return Err("the child has finished; spawn a new one".to_owned());
                }
                if let Some(session) = &child.session_id {
                    Ok(Some(session.clone()))
                } else {
                    child.backlog.push(text.clone());
                    Ok(None)
                }
            })
            .ok_or_else(|| "no such child".to_owned())??;
        let Some(session) = session else {
            return Ok("queued");
        };
        let lease = self
            .store
            .lease()
            .await
            .map_err(|error| error.to_string())?;
        let store = lease.store();
        let deadline = Instant::now() + Duration::from_secs(2);
        let run = loop {
            match store.latest_run(&session).await {
                Ok(Some(run)) => break run,
                Ok(None) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Ok(None) => return Err("the child's run has not opened its inbox".to_owned()),
                Err(error) => return Err(error.to_string()),
            }
        };
        RunInbox::new(store)
            .deliver(&run, text, harness_runtime::now_unix_ms())
            .await
            .map_err(|error| error.to_string())?;
        Ok("delivered")
    }

    fn names(&self) -> Vec<(TaskId, String, bool)> {
        self.children.lock().map_or_else(
            |_| Vec::new(),
            |children| {
                children
                    .iter()
                    .filter(|child| !child.deleted)
                    .map(|child| {
                        (
                            child.task_id.clone(),
                            child.name.clone(),
                            child.state.settled(),
                        )
                    })
                    .collect()
            },
        )
    }

    fn find(&self, selector: &str) -> Option<TaskId> {
        self.names()
            .into_iter()
            .find(|(task_id, name, _)| task_id.as_str() == selector || name == selector)
            .map(|(task_id, ..)| task_id)
    }

    /// A message from the parent (the root agent) to its children.
    async fn send_from_parent(&self, request: &Value) -> Result<Value, String> {
        let (message, role, name) = message_arguments(request)?;
        self.take_token("parent")?;
        let targets: Vec<(TaskId, String)> = match role.as_str() {
            "child" => {
                let name = name.ok_or("receiver_name is required for a child")?;
                let task_id = self
                    .find(&name)
                    .ok_or_else(|| format!("no child named {name:?}"))?;
                vec![(task_id, name)]
            }
            "all" => self
                .names()
                .into_iter()
                .filter(|(_, _, settled)| !settled)
                .map(|(task_id, name, _)| (task_id, name))
                .collect(),
            _ => return Err(AGENT_FAMILY_REACH_ERROR.to_owned()),
        };
        let mut receipts = Vec::new();
        for (task_id, name) in targets {
            let text = format!("[agent-message from parent:root]\n\n{message}");
            receipts.push(match self.deliver_to_child(&task_id, text).await {
                Ok(status) => receipt(&name, "root", &message, status),
                Err(error) => json!({"target": name, "error": error}),
            });
        }
        Ok(json!({ "receipts": receipts }))
    }

    /// A message a child sends with its `agent_message` tool.
    async fn send_from_child(&self, from: &TaskId, arguments: &Value) -> Result<Value, String> {
        let (message, role, name) = message_arguments(arguments)?;
        let sender_name = self
            .update(from, |child| child.name.clone())
            .ok_or("the sending child is gone")?;
        self.take_token(from.as_str())?;
        let to_parent = |text: String| {
            self.update(from, |child| child.replied = true);
            let _ = self.sender.send(SessionEvent::AgentMessage { text });
        };
        let mut receipts = Vec::new();
        match role.as_str() {
            "parent" => {
                to_parent(format!(
                    "[agent-message from child:{sender_name}]\n\n{message}"
                ));
                receipts.push(receipt("parent", &sender_name, &message, "delivered"));
            }
            "sibling" => {
                let name = name.ok_or("receiver_name is required for a sibling")?;
                let task_id = self
                    .find(&name)
                    .filter(|task_id| task_id != from)
                    .ok_or_else(|| format!("no sibling named {name:?}"))?;
                let text = format!("[agent-message from sibling:{sender_name}]\n\n{message}");
                let status = self.deliver_to_child(&task_id, text).await?;
                receipts.push(receipt(&name, &sender_name, &message, status));
            }
            "all" => {
                to_parent(format!(
                    "[agent-message from child:{sender_name}]\n\n{message}"
                ));
                receipts.push(receipt("parent", &sender_name, &message, "delivered"));
                for (task_id, name, settled) in self.names() {
                    if &task_id == from || settled {
                        continue;
                    }
                    let text = format!("[agent-message from sibling:{sender_name}]\n\n{message}");
                    receipts.push(match self.deliver_to_child(&task_id, text).await {
                        Ok(status) => receipt(&name, &sender_name, &message, status),
                        Err(error) => json!({"target": name, "error": error}),
                    });
                }
            }
            "child" => return Err("this agent has no children".to_owned()),
            _ => return Err(AGENT_FAMILY_REACH_ERROR.to_owned()),
        }
        Ok(json!({ "receipts": receipts }))
    }

    /// prime-agent's `rlm.progress_note`: a short status line, at most one every
    /// ten seconds, the newest five kept. It is read with `list_subagents` and
    /// `/agents`; it never enters the parent's context.
    fn progress_note(&self, from: &TaskId, arguments: &Value) -> Result<Value, String> {
        let message = arguments["message"]
            .as_str()
            .map(str::trim)
            .unwrap_or_default();
        if message.is_empty() {
            return Err("a progress note needs text".to_owned());
        }
        if message.chars().count() > MAX_NOTE_CHARS {
            return Err(format!(
                "a progress note holds at most {MAX_NOTE_CHARS} characters"
            ));
        }
        self.update(from, |child| {
            let now = Instant::now();
            if let Some(last) = child.last_note {
                let since = now.duration_since(last);
                if since < NOTE_INTERVAL {
                    let wait = NOTE_INTERVAL.saturating_sub(since);
                    return json!({
                        "accepted": false,
                        "retry_after_ms": u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
                    });
                }
            }
            child.last_note = Some(now);
            child.notes.push_back(message.to_owned());
            while child.notes.len() > NOTE_RING {
                child.notes.pop_front();
            }
            json!({ "accepted": true })
        })
        .ok_or_else(|| "the sending child is gone".to_owned())
    }
}

fn receipt(target: &str, from: &str, message: &str, status: &str) -> Value {
    let mut receipt = json!({
        "id": format!("msg-{}", harness_runtime::now_unix_ms()),
        "source": "agent_message",
        "target": target,
        "from": from,
        "message": message,
        "deliveryStatus": status,
    });
    if status == "delivered" {
        receipt["deliveryMode"] = json!("steer");
    }
    receipt
}

/// `message`, `receiver_role` and `receiver_name` of an agent message.
fn message_arguments(arguments: &Value) -> Result<(String, String, Option<String>), String> {
    let message = arguments["message"]
        .as_str()
        .filter(|message| !message.trim().is_empty())
        .ok_or("an agent message needs text")?;
    if message.chars().count() > MAX_MESSAGE_CHARS {
        return Err(format!(
            "an agent message holds at most {MAX_MESSAGE_CHARS} characters"
        ));
    }
    let role = arguments["receiver_role"]
        .as_str()
        .unwrap_or("parent")
        .to_owned();
    let name = arguments["receiver_name"]
        .as_str()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned);
    Ok((message.to_owned(), role, name))
}

/// The state a worker's outcome settles its child in.
fn outcome_state(role: AgentRole, outcome: WorkerOutcome) -> ChildState {
    let report = match outcome {
        WorkerOutcome::Reported(report) => report,
        WorkerOutcome::Failed { error } => {
            return ChildState::Failed {
                error: format!("{}: {}", error.code().as_str(), error.message()),
            };
        }
        WorkerOutcome::NoReport { reason } | WorkerOutcome::OutcomeUnknown { reason } => {
            return ChildState::Failed {
                error: format!("{}: {reason}", ErrorCode::ResultIncomplete.as_str()),
            };
        }
        WorkerOutcome::Observed(_) => {
            return ChildState::Failed {
                error: format!(
                    "{}: {} returned an editing observation without a report",
                    ErrorCode::ResultIncomplete.as_str(),
                    role.as_str()
                ),
            };
        }
    };
    ChildState::Done {
        answer: report.summary,
        detail: report.detail,
    }
}

/// The `delegate` result for a settled child.
fn state_payload(role: AgentRole, name: &str, state: &ChildState) -> Value {
    match state {
        ChildState::Running => json!({
            "role": role.as_str(), "status": "running", "name": name,
        }),
        ChildState::Done { answer, detail } => json!({
            "role": role.as_str(),
            "status": "completed",
            "name": name,
            "text": answer,
            "receipts_digest": detail["receipts_digest"],
            "steps": detail["steps"],
            "tool_calls": detail["tool_calls"],
            "worktree_id": detail["worktree_id"],
            "worktree_path": detail["worktree_path"],
            "branch": detail["branch"],
        }),
        // A child that failed is still an answer: the parent is told plainly that
        // it failed and why, as prime-agent's `[child-failed ...]` notice does.
        ChildState::Failed { error } => json!({
            "role": role.as_str(),
            "status": "failed",
            "name": name,
            "error": error,
            "text": format!("[child-failed {}] {error}", role.as_str()),
        }),
        ChildState::Cancelled { reason } => json!({
            "role": role.as_str(),
            "status": "cancelled",
            "name": name,
            "text": terminal_notice(name, state, false).unwrap_or_default(),
            "reason": reason,
        }),
    }
}

/// The children of one interactive session.
pub struct SessionAgents {
    shared: Arc<AgentsShared>,
    scheduler: Arc<WorkerScheduler>,
    workspace_manager: Arc<WorkspaceManager>,
    dispatched: Arc<tokio::sync::Notify>,
    pump: AtomicBool,
}

impl SessionAgents {
    /// # Errors
    /// The worker scheduler cannot be created.
    pub fn new(
        store: Arc<SharedStore>,
        sender: UnboundedSender<SessionEvent>,
        approval_gate: Arc<dyn ApprovalGate>,
        state_root: PathBuf,
    ) -> Result<Arc<Self>, HarnessError> {
        let shared = Arc::new(AgentsShared {
            store,
            sender,
            approval_gate,
            ledger: Mutex::new(None),
            launches: Mutex::new(HashMap::new()),
            children: Mutex::new(Vec::new()),
            changed: tokio::sync::Notify::new(),
            buckets: Mutex::new(HashMap::new()),
        });
        let backend = Arc::new(InteractiveWorkerBackend {
            shared: Arc::clone(&shared),
        });
        Self::assemble(shared, state_root, backend)
    }

    /// The session's children with a scripted worker in place of the model.
    #[cfg(test)]
    fn with_backend(
        store: Arc<SharedStore>,
        sender: UnboundedSender<SessionEvent>,
        approval_gate: Arc<dyn ApprovalGate>,
        state_root: PathBuf,
        backend: Arc<dyn WorkerBackend>,
    ) -> Arc<Self> {
        let shared = Arc::new(AgentsShared {
            store,
            sender,
            approval_gate,
            ledger: Mutex::new(None),
            launches: Mutex::new(HashMap::new()),
            children: Mutex::new(Vec::new()),
            changed: tokio::sync::Notify::new(),
            buckets: Mutex::new(HashMap::new()),
        });
        Self::assemble(shared, state_root, backend).expect("scheduler")
    }

    fn assemble(
        shared: Arc<AgentsShared>,
        state_root: PathBuf,
        backend: Arc<dyn WorkerBackend>,
    ) -> Result<Arc<Self>, HarnessError> {
        let workspace_manager = Arc::new(WorkspaceManager::new(state_root));
        let scheduler = Arc::new(
            WorkerScheduler::new(
                SchedulerConfig {
                    max_concurrent_workers: 3,
                    max_depth: 1,
                    // Children beyond the running three wait for a slot rather than
                    // being refused, as prime-agent admits every spawn.
                    max_queued_workers: 8,
                    // prime-agent bounds a child by its own turn - its steps, tool
                    // calls and deadline - and the depth, not by a request pool
                    // shared across siblings. A pool of 24 ran out after four or
                    // five children had each read a large file, and every later
                    // step and spawn of the turn was refused.
                    budget: DelegationBudget {
                        max_model_requests: u32::MAX,
                        ..DelegationBudget::default()
                    },
                },
                backend,
                Some(Arc::clone(&workspace_manager)),
            )
            .map_err(|error| HarnessError::new(error.code(), error.to_string()))?,
        );
        if let Ok(mut ledger) = shared.ledger.lock() {
            *ledger = Some(Arc::clone(scheduler.ledger()));
        }
        Ok(Arc::new(Self {
            shared,
            scheduler,
            workspace_manager,
            dispatched: Arc::new(tokio::sync::Notify::new()),
            pump: AtomicBool::new(false),
        }))
    }

    #[must_use]
    pub fn store(&self) -> Arc<SharedStore> {
        Arc::clone(&self.shared.store)
    }

    /// `/agents`: one line per child, newest last.
    #[must_use]
    pub fn summary(&self) -> Vec<String> {
        let lines = self.shared.children.lock().map_or_else(
            |_| Vec::new(),
            |children| {
                children
                    .iter()
                    .filter(|child| !child.deleted)
                    .map(ChildRecord::line)
                    .collect::<Vec<_>>()
            },
        );
        if lines.is_empty() {
            vec![format!(
                "no delegated workers; {} model requests so far",
                self.scheduler.ledger().requests_used()
            )]
        } else {
            lines
        }
    }

    /// Stop the running child named `selector` - or every one, for `all` - with
    /// `reason` in its notice. Returns how many were stopped.
    ///
    /// # Errors
    /// No running child has that name.
    pub fn stop(&self, selector: &str, reason: &str) -> Result<usize, String> {
        let mut children = self
            .shared
            .children
            .lock()
            .map_err(|_| "the child registry is unavailable".to_owned())?;
        let mut stopped = 0;
        for child in children.iter_mut().filter(|child| {
            !child.deleted
                && !child.state.settled()
                && (selector == "all"
                    || child.name == selector
                    || child.task_id.as_str() == selector)
        }) {
            child.cancel_reason = Some(reason.to_owned());
            child.cancellation.cancel();
            stopped += 1;
        }
        if stopped == 0 && selector != "all" {
            return Err(format!("no running child named {selector:?}"));
        }
        Ok(stopped)
    }

    /// Stop every child without notices and forget them: the conversation they
    /// worked for is gone (`/new`, a resume into another conversation).
    pub fn reset(&self) {
        if let Ok(mut children) = self.shared.children.lock() {
            for child in children.iter_mut() {
                child.suppress_notice = true;
                if !child.state.settled() {
                    child.cancel_reason = Some("the conversation was closed".to_owned());
                    child.cancellation.cancel();
                }
                child.deleted = true;
            }
        }
    }

    fn ensure_pump(self: &Arc<Self>) {
        if self.pump.swap(true, Ordering::SeqCst) {
            return;
        }
        let scheduler = Arc::clone(&self.scheduler);
        let shared = Arc::clone(&self.shared);
        let dispatched = Arc::clone(&self.dispatched);
        tokio::spawn(async move {
            loop {
                match scheduler.next_settled().await {
                    Some(Ok((task_id, outcome))) => shared.settle(&task_id, outcome),
                    Some(Err(error)) => {
                        let _ = shared.sender.send(SessionEvent::Notice {
                            message: format!("delegated workers stopped: {error}"),
                        });
                    }
                    None => dispatched.notified().await,
                }
            }
        });
    }

    /// Admit and dispatch one child, returning as soon as it is admitted - the part
    /// of prime-agent's `rlm.spawn` that happens before the child runs.
    #[allow(clippy::too_many_lines)]
    async fn start(
        self: &Arc<Self>,
        role: AgentRole,
        brief_text: String,
        name: Option<String>,
        launch: &ChildLaunch,
    ) -> Result<(TaskId, String), HarnessError> {
        self.scheduler
            .require_admission(1)
            .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
        let task_id = TaskId::generate();
        let name = name.unwrap_or_else(|| {
            let id = task_id.as_str();
            format!("{}-{}", role.as_str(), &id[id.len().saturating_sub(8)..])
        });
        if self
            .shared
            .names()
            .iter()
            .any(|(_, existing, _)| existing == &name)
        {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                format!("a child named {name:?} already exists"),
            ));
        }
        let run_id = AgentRunId::generate();
        let (workspace, base_commit, base_snapshot, worktree) = match role {
            AgentRole::Explorer => (
                launch.workspace.clone(),
                launch.workspace.base_commit.clone(),
                launch.workspace.observed_fingerprint.as_str().to_owned(),
                None,
            ),
            AgentRole::Coder => {
                let snapshot = inspect_coder_input(
                    &self.workspace_manager,
                    &launch.workspace_root,
                    &launch.workspace.project_id,
                )
                .await?;
                let write_scope = vec![".".to_owned()];
                let record = self
                    .scheduler
                    .create_worktree(&snapshot, &task_id, &run_id, &write_scope, 1)
                    .await
                    .map_err(coder_unavailable_error)?;
                let lease = self.shared.store.lease().await?;
                persist_worktree(&lease.store(), &record).await?;
                let observation =
                    harness_tools::observe_workspace(record.project_id.clone(), &record.path)
                        .map_err(|error| {
                            HarnessError::new(
                                error.code(),
                                format!("coder worktree cannot be observed: {error}"),
                            )
                        })?;
                (
                    observation,
                    snapshot.base_commit,
                    snapshot.fingerprint.as_str().to_owned(),
                    Some(record),
                )
            }
            _ => {
                return Err(HarnessError::new(
                    ErrorCode::RoleUnavailable,
                    format!("role {} is not delegated by this host", role.as_str()),
                ));
            }
        };
        let brief = TaskBrief {
            schema_version: harness_orchestrator::DELEGATION_CONTRACT_VERSION,
            task_id: task_id.clone(),
            title: brief_text.chars().take(80).collect(),
            objective: brief_text.clone(),
            acceptance_criteria: vec!["return a concise report answering the brief".to_owned()],
            inputs: vec!["current workspace snapshot".to_owned()],
            base_snapshot,
            base_commit,
            workspace,
            grants: DelegationGrants {
                project_id: launch.workspace.project_id.clone(),
                task_id: task_id.clone(),
                actions: if role == AgentRole::Coder {
                    vec![GrantAction::Read, GrantAction::Propose]
                } else {
                    vec![GrantAction::Read]
                },
                write_scope: worktree
                    .as_ref()
                    .map_or_else(Vec::new, |record| record.write_scope.clone()),
                edit_workspace: role == AgentRole::Coder,
                max_depth: 1,
                budget: DelegationBudget::default(),
            },
            expected_artifacts: Vec::new(),
            deadline_unix_ms: None,
            role,
        };
        brief
            .validate()
            .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
        let cancellation = CancellationToken::new();
        if let Ok(mut children) = self.shared.children.lock() {
            children.push(ChildRecord {
                task_id: task_id.clone(),
                name: name.clone(),
                role,
                model: launch.model.reference.clone(),
                started: Instant::now(),
                duration_ms: None,
                state: ChildState::Running,
                cancellation: cancellation.clone(),
                cancel_reason: None,
                waiters: 0,
                deleted: false,
                suppress_notice: false,
                replied: false,
                session_id: None,
                backlog: Vec::new(),
                notes: VecDeque::new(),
                last_note: None,
                activity: "queued".to_owned(),
                tool_calls: 0,
                cost: "n/a".to_owned(),
            });
        }
        if let Ok(mut launches) = self.shared.launches.lock() {
            launches.insert(task_id.clone(), launch.clone());
        }
        self.ensure_pump();
        let dispatched = self.scheduler.dispatch(WorkerRequest {
            task_id: task_id.clone(),
            run_id,
            generation: 1,
            depth: 1,
            brief,
            worktree,
            cancellation,
        });
        if let Err(error) = dispatched {
            if let Ok(mut children) = self.shared.children.lock() {
                children.retain(|child| child.task_id != task_id);
            }
            if let Ok(mut launches) = self.shared.launches.lock() {
                launches.remove(&task_id);
            }
            return Err(HarnessError::new(error.code(), error.to_string()));
        }
        self.dispatched.notify_one();
        Ok((task_id, name))
    }
}

/// The session's children as one turn sees them: its `delegate` tool and its
/// kernel's `rlm.*` requests, both starting children with this turn's launch.
pub struct DelegateHost {
    agents: Arc<SessionAgents>,
    launch: ChildLaunch,
    catalog: Arc<DelegateCatalog>,
    dispatcher: Arc<DelegateTool>,
}

impl DelegateHost {
    #[must_use]
    pub fn new(
        agents: &Arc<SessionAgents>,
        launch: ChildLaunch,
        parent_cancellation: CancellationToken,
    ) -> Self {
        Self {
            agents: Arc::clone(agents),
            catalog: Arc::new(DelegateCatalog),
            dispatcher: Arc::new(DelegateTool {
                agents: Arc::clone(agents),
                launch: launch.clone(),
                parent_cancellation,
            }),
            launch,
        }
    }

    #[must_use]
    pub fn tools(&self) -> ExternalTools {
        ExternalTools::new(Arc::clone(&self.catalog) as Arc<dyn ExternalToolCatalog>)
    }

    #[must_use]
    pub fn dispatcher(&self) -> Arc<dyn ExternalToolDispatcher> {
        Arc::clone(&self.dispatcher) as Arc<dyn ExternalToolDispatcher>
    }

    /// The `rlm.*` host requests of the Python REPL, served by the session's
    /// children.
    #[must_use]
    pub fn rlm_requests(&self) -> Arc<dyn HostRequests> {
        Arc::new(RlmChildren {
            agents: Arc::clone(&self.agents),
            launch: self.launch.clone(),
        })
    }

    #[cfg(test)]
    fn delegate_tool(&self) -> &DelegateTool {
        &self.dispatcher
    }
}

struct DelegateCatalog;

impl ExternalToolCatalog for DelegateCatalog {
    fn schemas(&self) -> Vec<Value> {
        vec![json!({
            "type": "function",
            "function": {
                "name": "delegate",
                "description": "Delegate a bounded explorer or an isolated-worktree coder to investigate or implement the brief. By default the call waits for the child's answer; with wait=false it returns at once, the child keeps running after this turn, and its result arrives later as a [child-exited ...] or [child-failed ...] message.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "role": {"type": "string", "enum": ["explorer", "coder"]},
                        "brief": {"type": "string", "minLength": 1, "maxLength": MAX_BRIEF_BYTES},
                        "wait": {"type": "boolean", "description": "Wait for the child's answer (default true)."}
                    },
                    "required": ["role", "brief"],
                    "additionalProperties": false
                }
            }
        })]
    }

    fn resolve(&self, name: &str, arguments: &Value) -> Option<CodingToolAction> {
        (name == "delegate").then(|| CodingToolAction::ExternalTool {
            plugin_id: "delegate".to_owned(),
            tool_name: "delegate".to_owned(),
            arguments: arguments.clone(),
            parent_invocation_id: None,
            timeout_ms: 120_000,
        })
    }
}

/// The `delegate` tool of one turn.
struct DelegateTool {
    agents: Arc<SessionAgents>,
    launch: ChildLaunch,
    parent_cancellation: CancellationToken,
}

impl DelegateTool {
    fn arguments(arguments: &Value) -> Result<(AgentRole, String, bool), HarnessError> {
        let object = arguments.as_object().ok_or_else(|| {
            HarnessError::new(
                ErrorCode::InvalidPayload,
                "delegate arguments must be an object",
            )
        })?;
        let role = match object.get("role").and_then(Value::as_str) {
            Some("explorer") => AgentRole::Explorer,
            Some("coder") => AgentRole::Coder,
            _ => {
                return Err(HarnessError::new(
                    ErrorCode::InvalidPayload,
                    "delegate role must be explorer or coder",
                ));
            }
        };
        let brief = object
            .get("brief")
            .and_then(Value::as_str)
            .filter(|brief| !brief.trim().is_empty() && brief.len() <= MAX_BRIEF_BYTES)
            .ok_or_else(|| {
                HarnessError::new(
                    ErrorCode::InvalidPayload,
                    format!("delegate brief must contain 1..={MAX_BRIEF_BYTES} UTF-8 bytes"),
                )
            })?;
        let wait = match object.get("wait") {
            None | Some(Value::Null) => true,
            Some(Value::Bool(wait)) => *wait,
            Some(_) => {
                return Err(HarnessError::new(
                    ErrorCode::InvalidPayload,
                    "delegate wait must be true or false",
                ));
            }
        };
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "role" | "brief" | "wait"))
        {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "delegate accepts only role, brief and wait",
            ));
        }
        Ok((role, brief.to_owned(), wait))
    }
}

impl DelegateTool {
    /// Start the child the arguments describe and, unless told not to, wait for
    /// its answer.
    async fn run(&self, arguments: &Value) -> Result<Value, HarnessError> {
        if self.parent_cancellation.is_cancelled() {
            return Err(HarnessError::new(
                ErrorCode::ProviderCanceled,
                "parent turn was canceled before the child started",
            ));
        }
        let (role, brief, wait) = Self::arguments(arguments)?;
        let launch = self
            .launch
            .for_model(None)
            .map_err(|error| HarnessError::new(ErrorCode::InvalidPayload, error))?;
        let (task_id, name) = self.agents.start(role, brief, None, &launch).await?;
        let payload = if wait {
            match self
                .agents
                .shared
                .wait(&task_id, None, Some(&self.parent_cancellation))
                .await?
            {
                Some(state) => state_payload(role, &name, &state),
                None => {
                    return Err(HarnessError::new(
                        ErrorCode::ServiceUnavailable,
                        "the child's record is gone",
                    ));
                }
            }
        } else {
            json!({
                "role": role.as_str(),
                "status": "started",
                "name": name,
                "task_id": task_id.as_str(),
                "text": format!("started {} {name}; its result arrives as a message when it finishes", role.as_str()),
            })
        };
        Ok(payload)
    }
}

impl ExternalToolDispatcher for DelegateTool {
    fn validate_external<'a>(
        &'a self,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            if plugin_id != "delegate" || tool_name != "delegate" {
                return Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "delegation target is unavailable",
                ));
            }
            let (role, ..) = Self::arguments(arguments)?;
            if role == AgentRole::Coder {
                inspect_coder_input(
                    &self.agents.workspace_manager,
                    &self.launch.workspace_root,
                    &self.launch.workspace.project_id,
                )
                .await?;
            }
            self.agents
                .scheduler
                .require_admission(1)
                .map_err(|error| HarnessError::new(error.code(), error.to_string()))
        })
    }

    fn dispatch_external<'a>(
        &'a self,
        _authorization: &'a harness_tools::ToolDispatchAuthorization,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
        _timeout_ms: u64,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>,
    > {
        Box::pin(async move {
            if plugin_id != "delegate" || tool_name != "delegate" {
                return Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "delegation target is unavailable",
                ));
            }
            let payload = self.run(arguments).await?;
            Ok(ToolOutput::ExternalTool {
                plugin_id: "delegate".to_owned(),
                tool_name: "delegate".to_owned(),
                payload,
                inflight: 1,
            })
        })
    }
}

/// Why a child's turn ended without an answer, or `None` when it answered.
fn stopped_short(stop: TurnStop) -> Option<String> {
    let reason = match stop {
        TurnStop::Final | TurnStop::Canceled => return None,
        TurnStop::LoopDetected => {
            "it kept making the same tool call with nothing changed (loop_detected)".to_owned()
        }
        TurnStop::Unverified => {
            "its last reply was empty or cut off, so it cannot be trusted (unverified)".to_owned()
        }
        other => format!("the turn stopped: {}", other.as_str()),
    };
    Some(reason)
}

fn coder_unavailable_error(reason: impl std::fmt::Display) -> HarnessError {
    HarnessError::new(
        ErrorCode::RoleUnavailable,
        format!("coder cannot obtain an M8-03 worktree: {reason}"),
    )
}

fn dirty_reasons(reasons: &[DirtyReason]) -> String {
    reasons
        .iter()
        .map(DirtyReason::describe)
        .collect::<Vec<_>>()
        .join(", ")
}

async fn inspect_coder_input(
    manager: &WorkspaceManager,
    workspace_root: &std::path::Path,
    project_id: &harness_types::ProjectId,
) -> Result<VerifiedSnapshot, HarnessError> {
    match manager
        .inspect_input(workspace_root, project_id)
        .await
        .map_err(coder_unavailable_error)?
    {
        InputInspection::Clean(snapshot) => Ok(snapshot),
        InputInspection::Dirty(reasons) => Err(coder_unavailable_error(dirty_reasons(&reasons))),
    }
}

async fn persist_worktree(
    store: &SqliteStore,
    record: &WorktreeRecord,
) -> Result<(), HarnessError> {
    store
        .upsert_worktree(&WorktreeRecordRow {
            worktree_id: record.worktree_id.clone(),
            task_id: record.task_id.clone(),
            run_id: record.run_id.clone(),
            project_id: record.project_id.clone(),
            base_commit: record.base_commit.clone(),
            base_branch: record.base_branch.clone(),
            branch: record.branch.clone(),
            path: record.path.clone(),
            write_scope: record.write_scope.clone(),
            state: record.state.as_str().to_owned(),
            input_fingerprint: record.input_fingerprint.clone(),
            result_fingerprint: None,
            generation: record.generation,
        })
        .await
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))
}

/// An explorer works under its parent's policy - the permission mode, the allow
/// and deny rules and what the user allowed for this turn - with every tool that
/// changes anything denied on top, as prime-agent's children inherit their
/// parent's permissions. A child that asked for each read in a `full-auto`
/// session stopped at a panel nobody expected.
fn explorer_policy(parent: &ToolPolicy) -> ToolPolicy {
    let denies = EXPLORER_DENY_TOOLS
        .iter()
        .map(|name| ToolPatternRule::deny(format!("{name}*"), "explorer is read-only"))
        .collect::<Vec<_>>();
    parent.clone().with_tool_rules(denies)
}

fn explorer_tool_schemas() -> Vec<Value> {
    coding_tool_schemas()
        .into_iter()
        .filter(|schema| {
            schema["function"]["name"]
                .as_str()
                .is_some_and(|name| CHILD_READ_TOOLS.contains(&name))
        })
        .collect()
}

/// A coder works in its own worktree under its parent's policy.
fn coder_policy(parent: &ToolPolicy) -> ToolPolicy {
    parent.clone()
}

/// The tools a child has beyond the coding tools: the parent's web tools, and
/// prime-agent's `agent_message` and `rlm.progress_note`, which a prime-agent
/// child calls from its kernel and a child here calls as tools.
struct ChildTools {
    shared: Arc<AgentsShared>,
    task_id: TaskId,
    web: Option<ChildWebTools>,
}

impl ChildTools {
    fn agent_schemas() -> Vec<Value> {
        vec![
            json!({
                "type": "function",
                "function": {
                    "name": "agent_message",
                    "description": "Send a message to your parent agent (receiver_role \"parent\", the default), to a sibling child by name (\"sibling\" with receiver_name), or to all (\"all\"). The parent reads it at its next step, or it wakes the parent when the parent is idle. Use it to report what you found before you finish.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "message": {"type": "string", "minLength": 1, "maxLength": MAX_MESSAGE_CHARS},
                            "receiver_role": {"type": "string", "enum": ["parent", "sibling", "all"]},
                            "receiver_name": {"type": "string"}
                        },
                        "required": ["message"],
                        "additionalProperties": false
                    }
                }
            }),
            json!({
                "type": "function",
                "function": {
                    "name": "progress_note",
                    "description": "Leave a one-line progress note your parent can read in its list of children. At most one every ten seconds; it is not sent to the parent as a message.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "message": {"type": "string", "minLength": 1, "maxLength": MAX_NOTE_CHARS}
                        },
                        "required": ["message"],
                        "additionalProperties": false
                    }
                }
            }),
        ]
    }
}

impl ExternalToolCatalog for ChildTools {
    fn schemas(&self) -> Vec<Value> {
        let mut schemas = Self::agent_schemas();
        if let Some((catalog, _)) = &self.web {
            schemas.extend(catalog.schemas());
        }
        schemas
    }

    fn resolve(&self, name: &str, arguments: &Value) -> Option<CodingToolAction> {
        if matches!(name, "agent_message" | "progress_note") {
            return Some(CodingToolAction::ExternalTool {
                plugin_id: "agent".to_owned(),
                tool_name: name.to_owned(),
                arguments: arguments.clone(),
                parent_invocation_id: None,
                timeout_ms: 30_000,
            });
        }
        self.web
            .as_ref()
            .and_then(|(catalog, _)| catalog.resolve(name, arguments))
    }
}

impl ExternalToolDispatcher for ChildTools {
    fn validate_external<'a>(
        &'a self,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            if plugin_id == "agent" {
                return Ok(());
            }
            match &self.web {
                Some((_, dispatcher)) => {
                    dispatcher
                        .validate_external(plugin_id, tool_name, arguments)
                        .await
                }
                None => Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "this tool is not available to a child",
                )),
            }
        })
    }

    fn dispatch_external<'a>(
        &'a self,
        authorization: &'a harness_tools::ToolDispatchAuthorization,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
        timeout_ms: u64,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>,
    > {
        Box::pin(async move {
            if plugin_id != "agent" {
                return match &self.web {
                    Some((_, dispatcher)) => {
                        dispatcher
                            .dispatch_external(
                                authorization,
                                plugin_id,
                                tool_name,
                                arguments,
                                timeout_ms,
                            )
                            .await
                    }
                    None => Err(HarnessError::new(
                        ErrorCode::PolicyDenied,
                        "this tool is not available to a child",
                    )),
                };
            }
            let result = match tool_name {
                "agent_message" => self.shared.send_from_child(&self.task_id, arguments).await,
                "progress_note" => self.shared.progress_note(&self.task_id, arguments),
                _ => Err(format!("unknown agent tool {tool_name:?}")),
            };
            let payload =
                result.map_err(|error| HarnessError::new(ErrorCode::InvalidPayload, error))?;
            Ok(ToolOutput::ExternalTool {
                plugin_id: "agent".to_owned(),
                tool_name: tool_name.to_owned(),
                payload,
                inflight: 1,
            })
        })
    }
}

/// prime-agent's `rlm.spawn` family, served by the session's children.
///
/// A child is an explorer worker - the same one the `delegate` tool starts - and
/// `rlm.spawn` returns the moment it is admitted, as prime-agent's does. Children
/// outlive the turn that spawned them; each one's result reaches the parent as a
/// notice when it settles, and `rlm.collect` reads it at any time before that.
struct RlmChildren {
    agents: Arc<SessionAgents>,
    launch: ChildLaunch,
}

impl RlmChildren {
    fn session_dir(&self) -> String {
        self.launch.workspace_root.display().to_string()
    }

    /// Find children by id or name; an empty selection means every child.
    fn select(&self, selectors: &[String]) -> Result<Vec<TaskId>, String> {
        let names = self.agents.shared.names();
        if selectors.is_empty() {
            return Ok(names.into_iter().map(|(task_id, ..)| task_id).collect());
        }
        selectors
            .iter()
            .map(|selector| {
                self.agents
                    .shared
                    .find(selector)
                    .ok_or_else(|| format!("no child named or numbered {selector:?}"))
            })
            .collect()
    }

    fn rows(&self, targets: &[TaskId], full: bool) -> Result<Vec<Value>, String> {
        let dir = self.session_dir();
        let children = self
            .agents
            .shared
            .children
            .lock()
            .map_err(|_| "the child registry is unavailable".to_owned())?;
        Ok(targets
            .iter()
            .filter_map(|task_id| children.iter().find(|child| &child.task_id == task_id))
            .map(|child| {
                if full {
                    child.result(&dir)
                } else {
                    child.row(&dir)
                }
            })
            .collect())
    }

    async fn spawn(&self, request: &Value) -> Result<Value, String> {
        let prompt = request["prompt"]
            .as_str()
            .map(str::trim)
            .filter(|prompt| !prompt.is_empty() && prompt.len() <= MAX_BRIEF_BYTES)
            .ok_or_else(|| format!("rlm.spawn needs a task of 1..={MAX_BRIEF_BYTES} bytes"))?;
        let kwargs = &request["kwargs"];
        let name = kwargs["name"]
            .as_str()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or("rlm.spawn needs a name")?
            .to_owned();
        if !kwargs["thinking"].is_null() {
            return Err("thinking levels are not configurable in ha".to_owned());
        }
        let launch = self.launch.for_model(kwargs["model"].as_str())?;
        let (task_id, name) = self
            .agents
            .start(AgentRole::Explorer, prompt.to_owned(), Some(name), &launch)
            .await
            .map_err(|error| error.message().to_owned())?;
        Ok(json!({
            "rlm_child_id": task_id.as_str(),
            "name": name,
            "session_dir": self.session_dir(),
            "model": launch.model.reference,
        }))
    }

    async fn collect(&self, request: &Value) -> Result<Value, String> {
        let selectors = request["targets"]
            .as_array()
            .map(|targets| {
                targets
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let timeout = request["timeout_ms"].as_u64().unwrap_or(0);
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout);
        let targets = self.select(&selectors)?;
        if timeout > 0 {
            for task_id in &targets {
                let _ = self.agents.shared.wait(task_id, Some(deadline), None).await;
            }
        }
        Ok(json!({ "results": self.rows(&targets, true)? }))
    }

    fn list(&self) -> Result<Value, String> {
        let targets = self.select(&[])?;
        Ok(json!({ "subagents": self.rows(&targets, false)? }))
    }

    fn delete(&self, request: &Value) -> Result<Value, String> {
        let selector = request["target"].as_str().unwrap_or_default().to_owned();
        let task_id = self
            .select(std::slice::from_ref(&selector))?
            .pop()
            .ok_or("no such child")?;
        let dir = self.session_dir();
        self.agents
            .shared
            .update(&task_id, |child| {
                if !child.state.settled() {
                    child.cancel_reason = Some("Deleted by parent orchestrator".to_owned());
                    child.cancellation.cancel();
                }
                child.deleted = true;
                json!({ "subagent": child.row(&dir) })
            })
            .ok_or_else(|| "no such child".to_owned())
    }

    fn models(&self, request: &Value) -> Value {
        let query = request["query"].as_str().unwrap_or_default().to_lowercase();
        let reference = &self.launch.model.reference;
        let models = if query.is_empty() || reference.to_lowercase().contains(&query) {
            vec![json!({
                "provider": reference.split('/').next().unwrap_or("ha"),
                "id": reference,
                "name": reference,
                "selector": reference,
            })]
        } else {
            Vec::new()
        };
        json!({ "models": models })
    }

    fn observe(&self, kind: &str, request: &Value) -> Result<Value, String> {
        if kind == "agent_observe.list" {
            let children = self.list()?;
            return Ok(
                json!({ "parent": null, "siblings": [], "children": children["subagents"] }),
            );
        }
        let target = request["target"].as_str().unwrap_or_default().to_owned();
        let ids = self.select(std::slice::from_ref(&target))?;
        let row = self.rows(&ids, false)?.pop().ok_or("no such child")?;
        Ok(if kind == "agent_observe.get" {
            row
        } else {
            json!({
                "target": target,
                "messages": row["answer_preview"]
                    .as_str()
                    .map(|answer| vec![json!({"role": "assistant", "text": answer})])
                    .unwrap_or_default(),
            })
        })
    }
}

fn clip(text: &str) -> String {
    if text.chars().count() <= RLM_ANSWER_MAX_CHARS {
        return text.to_owned();
    }
    let kept = text.chars().take(RLM_ANSWER_MAX_CHARS).collect::<String>();
    format!("{kept}\n[answer cut at {RLM_ANSWER_MAX_CHARS} characters]")
}

impl HostRequests for RlmChildren {
    fn handle<'a>(&'a self, request: &'a Value) -> HostReply<'a> {
        Box::pin(async move {
            let kind = request["type"].as_str().unwrap_or_default();
            Some(match kind {
                "rlm.run" => self.spawn(request).await,
                "rlm.collect" => self.collect(request).await,
                "rlm.list_subagents" => self.list(),
                "rlm.delete_subagent" => self.delete(request),
                "rlm.find_models" => Ok(self.models(request)),
                "rlm.progress.note" => Err(
                    "progress notes are sent by child agents; this is the root agent".to_owned(),
                ),
                "rlm.create_session" => {
                    Err("separate top-level sessions are not available in ha".to_owned())
                }
                // prime-agent's `agent_observe` skill over this session's children:
                // the only family the root agent has here.
                "agent_observe.list" | "agent_observe.get" | "agent_observe.recent" => {
                    self.observe(kind, request)
                }
                // The root agent has no parent and no siblings; it messages its
                // children.
                "agent_message.send" => self.agents.shared.send_from_parent(request).await,
                _ => return None,
            })
        })
    }
}

struct InteractiveWorkerBackend {
    shared: Arc<AgentsShared>,
}

impl WorkerBackend for InteractiveWorkerBackend {
    #[allow(clippy::too_many_lines)]
    fn dispatch(
        &self,
        request: WorkerRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = WorkerOutcome> + Send + '_>> {
        Box::pin(async move {
            let task_id = request.task_id.clone();
            let task_key = task_id.as_str().to_owned();
            let role_name = request.brief.role.as_str().to_owned();
            let Some(launch) = self
                .shared
                .launches
                .lock()
                .ok()
                .and_then(|mut launches| launches.remove(&task_id))
            else {
                return WorkerOutcome::Failed {
                    error: OrchestratorError::new(
                        ErrorCode::RoleUnavailable,
                        "the child's launch settings are missing",
                    ),
                };
            };
            let (workspace_root, policy, schemas, system_policy) = match request.brief.role {
                AgentRole::Explorer => (
                    launch.workspace_root.clone(),
                    explorer_policy(&launch.parent_policy),
                    explorer_tool_schemas(),
                    "You are a read-only explorer. Inspect the current workspace - and the web, when web tools are offered - and answer the brief. You cannot edit files, run commands or delegate again. Use agent_message to tell your parent what you found when it helps before you finish.",
                ),
                AgentRole::Coder => {
                    let Some(worktree) = request.worktree.as_ref() else {
                        return WorkerOutcome::Failed {
                            error: OrchestratorError::new(
                                ErrorCode::RoleUnavailable,
                                "coder worker was dispatched without its M8-03 worktree",
                            ),
                        };
                    };
                    if worktree.task_id != task_id
                        || worktree.run_id != request.run_id
                        || worktree.project_id != request.brief.grants.project_id
                        || !request.brief.grants.edit_workspace
                        || request.brief.grants.write_scope != worktree.write_scope
                    {
                        return WorkerOutcome::Failed {
                            error: OrchestratorError::new(
                                ErrorCode::ScopeAuthorityDenied,
                                "coder worktree does not match the host-issued task grant",
                            ),
                        };
                    }
                    (
                        PathBuf::from(&worktree.path),
                        coder_policy(&launch.parent_policy),
                        coding_tool_schemas(),
                        "You are a delegated coder. Work only in the assigned isolated M8-03 worktree and follow the brief. Tool calls use the host approval and policy service. Leave your changes in that worktree and report what changed; do not claim the changes were merged into the user's checkout.",
                    )
                }
                _ => {
                    return WorkerOutcome::Failed {
                        error: OrchestratorError::new(
                            ErrorCode::RoleUnavailable,
                            format!("role {role_name} has no interactive worker backend"),
                        ),
                    };
                }
            };
            if request.cancellation.is_cancelled() {
                return WorkerOutcome::OutcomeUnknown {
                    reason: "the child was stopped before it started".to_owned(),
                };
            }
            let lease = match self.shared.store.lease().await {
                Ok(lease) => lease,
                Err(error) => {
                    return WorkerOutcome::Failed {
                        error: OrchestratorError::new(error.code(), error.message().to_owned()),
                    };
                }
            };
            let store = lease.store();
            let session_id = SessionId::generate();
            let backlog = self
                .shared
                .update(&task_id, |child| {
                    child.session_id = Some(session_id.clone());
                    "starting".clone_into(&mut child.activity);
                    std::mem::take(&mut child.backlog)
                })
                .unwrap_or_default();
            // The run admits the brief once, as the child's input and as proposed
            // by the parent's model. Admitting it here as well - with the brief's
            // text, while the run admits the prompt around it - put two contents
            // under one input id, and every child failed with
            // `idempotency_conflict` before its first step.
            let input_id = InputId::generate();
            let child_tools = Arc::new(ChildTools {
                shared: Arc::clone(&self.shared),
                task_id: task_id.clone(),
                web: launch.web.clone(),
            });
            let tools = ToolExecutionService::new(Arc::clone(&store))
                .with_policy(policy)
                .with_hooks(launch.hooks.clone())
                .with_external(Arc::clone(&child_tools) as Arc<dyn ExternalToolDispatcher>);
            let mut schemas = schemas;
            schemas.extend(child_tools.schemas());
            let runtime = Arc::new(RuntimeService::new(
                Arc::clone(&store),
                Arc::clone(&launch.model.provider),
                launch.runtime_config.clone(),
            ));
            // prime-agent frames a child's task as `[task from parent]`.
            let mut prompt = format!(
                "[task from parent]\n\nAnswer the delegated task below. Return concise results and identify changed files or findings with paths.\n\nBrief:\n{}",
                request.brief.objective
            );
            for message in backlog {
                prompt.push_str("\n\n");
                prompt.push_str(&message);
            }
            let run_request = RunRequest::new(
                session_id,
                task_id.clone(),
                input_id,
                prompt,
                request.brief.workspace.clone(),
            )
            .with_system_policy(system_policy)
            .with_tool_schemas(schemas)
            .with_authority(SourceAuthority::ModelProposed);
            let driver = TurnDriver::new(Arc::clone(&runtime), tools)
                .with_external(ExternalTools::new(
                    Arc::clone(&child_tools) as Arc<dyn ExternalToolCatalog>
                ))
                .with_inbox(RunInbox::new(Arc::clone(&store)));
            let observer = Arc::new(ExplorerObserver {
                task_id: task_id.clone(),
                role_name: role_name.clone(),
                shared: Arc::clone(&self.shared),
                ledger: self
                    .shared
                    .ledger
                    .lock()
                    .ok()
                    .and_then(|ledger| ledger.clone()),
                model_price: launch.model.price,
                cancellation: request.cancellation.clone(),
                budget_error: Mutex::new(None),
                sender: self.shared.sender.clone(),
                token_usage: Mutex::new((0_u64, 0_u64)),
                cost_tracker: Mutex::new(CostTracker::default()),
            });
            let approval: Arc<dyn ApprovalGate> = Arc::new(ChildApprovalGate {
                role: role_name.clone(),
                inner: Arc::clone(&self.shared.approval_gate),
            });
            let result = driver
                .run_turn(
                    run_request,
                    TurnOptions {
                        workspace_root: workspace_root.clone(),
                        actor_id: format!("interactive.child.{role_name}"),
                        approvals: ApprovalMode::Ask(approval),
                        limits: launch.child_limits,
                    },
                    observer.clone(),
                    request.cancellation.clone(),
                )
                .await;
            let cost = observer
                .cost_tracker
                .lock()
                .map_or_else(|_| "n/a".to_owned(), |tracker| tracker.display());
            self.shared.update(&task_id, |child| child.cost = cost);
            if let Some(error) = observer
                .budget_error
                .lock()
                .ok()
                .and_then(|error| error.clone())
            {
                return WorkerOutcome::Failed { error };
            }
            let outcome = match result {
                Ok(outcome) => outcome,
                Err(error) => {
                    return WorkerOutcome::Failed {
                        error: OrchestratorError::new(error.code(), error.to_string()),
                    };
                }
            };
            self.shared
                .update(&task_id, |child| child.tool_calls = outcome.tool_calls);
            if outcome.stop == TurnStop::Canceled || request.cancellation.is_cancelled() {
                return WorkerOutcome::OutcomeUnknown {
                    reason: "the child was stopped".to_owned(),
                };
            }
            // A child that stopped short of an answer - a loop, an unverifiable
            // reply, a bound - failed, and says why. It used to be reported as
            // completed ("Explorer completed without a final text answer"), so the
            // parent could not tell a child that broke from one that finished.
            if let Some(reason) = stopped_short(outcome.stop) {
                return WorkerOutcome::Failed {
                    error: OrchestratorError::new(
                        ErrorCode::ResultIncomplete,
                        format!(
                            "{role_name} stopped without an answer: {reason} ({} step(s), {} tool call(s))",
                            outcome.steps, outcome.tool_calls
                        ),
                    ),
                };
            }
            let receipts = serde_json::to_vec(&outcome.executions).unwrap_or_default();
            let receipts_digest = ContentHash::from_bytes(&receipts).as_str().to_owned();
            let (prompt_tokens, completion_tokens) =
                observer.token_usage.lock().map_or((0, 0), |usage| *usage);
            let summary = if outcome.final_text.trim().is_empty() {
                format!("[no answer] the {role_name} finished without writing an answer.")
            } else {
                outcome.final_text
            };
            let report = DelegatedResult {
                schema_version: harness_orchestrator::DELEGATION_CONTRACT_VERSION,
                result_id: format!("{task_key}-result"),
                task_id: task_id.clone(),
                worker: harness_orchestrator::WorkerRef {
                    profile_id: AgentProfileId::generate(),
                    run_id: request.run_id,
                    role: request.brief.role,
                    generation: request.generation,
                },
                outcome: DelegatedOutcome::Completed,
                summary,
                artifact_refs: Vec::new(),
                base_revision: request.brief.base_commit.clone(),
                result_revision: request.brief.base_commit.clone(),
                checked_revisions: Vec::new(),
                check_receipts: Vec::new(),
                usage: BudgetUsage {
                    model_requests: outcome.steps,
                    retries: 0,
                },
                detail: json!({
                    "session_id": outcome.session_id,
                    "steps": outcome.steps,
                    "tool_calls": outcome.tool_calls,
                    "prompt_tokens": prompt_tokens,
                    "completion_tokens": completion_tokens,
                    "receipts_digest": receipts_digest,
                    "worktree_id": request.worktree.as_ref().map(|record| record.worktree_id.as_str()),
                    "worktree_path": request.worktree.as_ref().map(|record| record.path.as_str()),
                    "branch": request.worktree.as_ref().map(|record| record.branch.as_str()),
                }),
            };
            drop(lease);
            WorkerOutcome::Reported(Box::new(report))
        })
    }
}

struct ExplorerObserver {
    task_id: TaskId,
    role_name: String,
    shared: Arc<AgentsShared>,
    ledger: Option<Arc<BudgetLedger>>,
    model_price: Option<ModelPrice>,
    cancellation: CancellationToken,
    budget_error: Mutex<Option<OrchestratorError>>,
    sender: UnboundedSender<SessionEvent>,
    token_usage: Mutex<(u64, u64)>,
    cost_tracker: Mutex<CostTracker>,
}

impl TurnObserver for ExplorerObserver {
    fn observe(&self, progress: TurnProgress) {
        match progress {
            TurnProgress::StepStarted { step } => {
                if step > 1
                    && let Some(ledger) = &self.ledger
                    && let Err(error) = ledger.charge_request()
                {
                    if let Ok(mut budget_error) = self.budget_error.lock() {
                        *budget_error = Some(error);
                    }
                    self.cancellation.cancel();
                }
                self.shared.update(&self.task_id, |child| {
                    child.activity = format!("step {step}");
                });
            }
            TurnProgress::Usage {
                prompt_tokens,
                completion_tokens,
            } => {
                if let Ok(mut usage) = self.token_usage.lock() {
                    usage.0 = usage.0.saturating_add(prompt_tokens);
                    usage.1 = usage.1.saturating_add(completion_tokens);
                }
                if let Ok(mut tracker) = self.cost_tracker.lock() {
                    tracker.record(
                        self.model_price,
                        CostUsage {
                            input_tokens: prompt_tokens,
                            output_tokens: completion_tokens,
                        },
                    );
                    let cost = tracker.display();
                    self.shared.update(&self.task_id, |child| child.cost = cost);
                }
            }
            TurnProgress::ToolStarted { name, summary, .. } => {
                self.shared.update(&self.task_id, |child| {
                    child.tool_calls += 1;
                    child.activity = format!("{name}: {summary}");
                });
                let _ = self.sender.send(SessionEvent::Notice {
                    message: format!("[child {}] {name}: {summary}", self.role_name),
                });
            }
            TurnProgress::ToolSettled { name, ok, detail } if !ok => {
                let _ = self.sender.send(SessionEvent::Notice {
                    message: format!(
                        "[child {}] {name} failed: {}",
                        self.role_name,
                        detail.unwrap_or_else(|| "no detail".to_owned())
                    ),
                });
            }
            TurnProgress::Notice(message) => {
                let _ = self.sender.send(SessionEvent::Notice {
                    message: format!("[child {}] {message}", self.role_name),
                });
            }
            TurnProgress::TextDelta(_)
            | TurnProgress::ThinkingDelta(_)
            | TurnProgress::ToolSettled { .. }
            | TurnProgress::ToolOutput { .. }
            | TurnProgress::Info(_) => {}
        }
    }
}

struct ChildApprovalGate {
    role: String,
    inner: Arc<dyn ApprovalGate>,
}

impl ApprovalGate for ChildApprovalGate {
    fn request(
        &self,
        mut proposal: ApprovalProposal,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ApprovalAnswer> + Send>> {
        proposal.summary = format!("[child {}] {}", self.role, proposal.summary);
        self.inner.request(proposal)
    }

    fn action_completed(&self, request_id: &str) {
        self.inner.action_completed(request_id);
    }
}

#[cfg(test)]
pub(super) mod tests {
    use std::{process::Command, sync::Arc, time::Duration};

    use harness_orchestrator::{
        BudgetLedger, RefusingBackend, SchedulerConfig, WorkerScheduler, WorkspaceManager,
    };
    use harness_providers::CancellationToken;
    use harness_tools::{CodingToolAction, TurnObserver, TurnProgress};
    use harness_types::{AgentRunId, ErrorCode, ProjectId, TaskId};

    use super::{
        AgentsShared, ChildLaunch, ChildModel, ChildModels, ChildState, DelegateCatalog,
        ExplorerObserver, SessionAgents, explorer_policy, explorer_tool_schemas,
        inspect_coder_input, message_arguments, outcome_state, state_payload, stopped_short,
        terminal_notice,
    };
    use crate::interactive::cost::CostTracker;
    use crate::interactive::store_lease::SharedStore;

    fn git(repo: &std::path::Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git starts");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn clean_repository(repo: &std::path::Path) {
        git(repo, &["init", "-b", "main"]);
        git(repo, &["config", "user.email", "worker-test@localhost"]);
        git(repo, &["config", "user.name", "worker-test"]);
        std::fs::write(repo.join("README.md"), "clean workspace\n").expect("seed repo");
        git(repo, &["add", "README.md"]);
        git(repo, &["commit", "-m", "seed"]);
    }

    #[test]
    fn g12_explorer_child_cannot_call_mutating_tools() {
        let policy = explorer_policy(&harness_tools::ToolPolicy::new(1, Vec::new()));
        let writes = [
            CodingToolAction::WriteFile {
                path: "note.txt".to_owned(),
                content: "bad".to_owned(),
                expected_hash: None,
            },
            CodingToolAction::RunShell {
                command: "echo bad".to_owned(),
                timeout_ms: 1000,
                isolation: harness_tools::IsolationMode::BestEffort,
                env: Vec::new(),
            },
            CodingToolAction::TaskUpdate {
                note: "recursive state change".to_owned(),
            },
        ];
        for action in writes {
            assert!(policy.denial_for(&action).is_some(), "{action:?}");
        }
        let read = CodingToolAction::ReadFile {
            path: "src/lib.rs".to_owned(),
            offset: None,
            limit: None,
        };
        assert!(policy.denial_for(&read).is_none());
    }

    /// A child works under its parent's permission mode: in a `full-auto` session
    /// an explorer reads without a panel, and it still cannot write.
    #[test]
    fn an_explorer_inherits_the_parents_mode_and_stays_read_only() {
        let parent = harness_tools::ToolPolicy::new(1, Vec::new())
            .with_mode(harness_tools::PolicyMode::FullAuto);
        let policy = explorer_policy(&parent);
        let read = CodingToolAction::ReadFile {
            path: "src/lib.rs".to_owned(),
            offset: None,
            limit: None,
        };
        assert!(
            matches!(policy.decide(&read), harness_tools::Decision::Allow { .. }),
            "{:?}",
            policy.decide(&read)
        );
        let write = CodingToolAction::WriteFile {
            path: "src/lib.rs".to_owned(),
            content: "x".to_owned(),
            expected_hash: None,
        };
        assert!(policy.denial_for(&write).is_some());
    }

    #[test]
    fn g12_child_cannot_delegate_again() {
        let schemas = explorer_tool_schemas();
        let names = schemas
            .iter()
            .filter_map(|schema| schema["function"]["name"].as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"read_file"));
        assert!(!names.contains(&"delegate"));
        assert!(!names.contains(&"write_file"));
        assert!(!names.contains(&"run_shell"));
    }

    /// The parts of a session's children that need no running worker.
    fn shared(
        sender: tokio::sync::mpsc::UnboundedSender<crate::interactive::events::SessionEvent>,
    ) -> Arc<AgentsShared> {
        let gate = Arc::new(crate::interactive::service::ChannelApprovalGate::new(
            sender.clone(),
            Duration::from_secs(5),
        ));
        Arc::new(AgentsShared {
            store: SharedStore::new(std::env::temp_dir().join("ha-unused-store")),
            sender,
            approval_gate: gate,
            ledger: std::sync::Mutex::new(None),
            launches: std::sync::Mutex::new(std::collections::HashMap::new()),
            children: std::sync::Mutex::new(Vec::new()),
            changed: tokio::sync::Notify::new(),
            buckets: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
    }

    #[test]
    fn g12_child_budget_is_charged_to_the_parent_ledger() {
        let mut config = SchedulerConfig::default();
        config.budget.max_model_requests = 2;
        let ledger = Arc::new(BudgetLedger::new(&config));
        ledger.charge_request().expect("dispatch charged step one");
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let observer = ExplorerObserver {
            task_id: TaskId::generate(),
            role_name: "explorer".to_owned(),
            shared: shared(sender.clone()),
            ledger: Some(Arc::clone(&ledger)),
            model_price: None,
            cancellation: CancellationToken::new(),
            budget_error: std::sync::Mutex::new(None),
            sender,
            token_usage: std::sync::Mutex::new((0, 0)),
            cost_tracker: std::sync::Mutex::new(CostTracker::default()),
        };
        observer.observe(TurnProgress::StepStarted { step: 2 });
        assert_eq!(ledger.requests_used(), 2);
        assert_eq!(ledger.remaining_requests(), 0);
    }

    /// A child that broke is reported as failed, in one line, with the error it
    /// failed on - not as an uncertain side effect, and not as completed.
    #[test]
    fn a_failed_child_is_reported_as_failed_with_its_reason() {
        use harness_orchestrator::{AgentRole, OrchestratorError, WorkerOutcome};
        use harness_tools::TurnStop;

        let failed = WorkerOutcome::Failed {
            error: OrchestratorError::new(
                ErrorCode::ProviderProtocol,
                "provider_protocol: provider_protocol: malformed provider SSE JSON",
            ),
        };
        let state = outcome_state(AgentRole::Explorer, failed);
        let payload = state_payload(AgentRole::Explorer, "scout", &state);
        assert_eq!(payload["status"], "failed");
        assert_eq!(
            payload["text"],
            "[child-failed explorer] provider_protocol: malformed provider SSE JSON"
        );

        assert!(stopped_short(TurnStop::Final).is_none());
        assert!(stopped_short(TurnStop::Canceled).is_none());
        for stop in [
            TurnStop::LoopDetected,
            TurnStop::Unverified,
            TurnStop::StepLimit,
            TurnStop::Deadline,
        ] {
            assert!(stopped_short(stop).is_some(), "{stop:?} is not an answer");
        }
    }

    /// prime-agent's terminal notices (`core/messages.ts`), word for word.
    #[test]
    fn q02_terminal_notice_texts_match_prime() {
        let failed = ChildState::Failed {
            error: "provider_protocol: stream failed".to_owned(),
        };
        assert_eq!(
            terminal_notice("scout", &failed, false).as_deref(),
            Some("[child-failed child:scout]\n\nprovider_protocol: stream failed")
        );
        let stopped = ChildState::Cancelled {
            reason: "Deleted by parent orchestrator".to_owned(),
        };
        assert_eq!(
            terminal_notice("scout", &stopped, false).as_deref(),
            Some("[child-exited: cancelled child:scout]\n\nDeleted by parent orchestrator")
        );
        let bare = ChildState::Cancelled {
            reason: String::new(),
        };
        assert_eq!(
            terminal_notice("scout", &bare, false).as_deref(),
            Some("[child-exited: cancelled child:scout]")
        );
        let done = ChildState::Done {
            answer: "found it".to_owned(),
            detail: serde_json::Value::Null,
        };
        assert_eq!(
            terminal_notice("scout", &done, false).as_deref(),
            Some("[child-exited: no-reply child:scout]\n\nLast assistant text: found it")
        );
        assert_eq!(
            terminal_notice("scout", &done, true),
            None,
            "a child that replied to its parent needs no notice"
        );
        let silent = ChildState::Done {
            answer: String::new(),
            detail: serde_json::Value::Null,
        };
        assert_eq!(
            terminal_notice("scout", &silent, false).as_deref(),
            Some("[child-exited: no-reply child:scout]")
        );
    }

    /// prime-agent's agent-message bounds: 16 384 characters, and a bucket of
    /// three messages that refills one each second.
    #[test]
    fn q03_message_limits_follow_prime() {
        let long = "x".repeat(16_385);
        assert!(message_arguments(&serde_json::json!({"message": long})).is_err());
        let fits = "x".repeat(16_384);
        let (message, role, name) =
            message_arguments(&serde_json::json!({"message": fits})).expect("fits");
        assert_eq!(message.len(), 16_384);
        assert_eq!(
            role, "parent",
            "a child's message goes to its parent by default"
        );
        assert!(name.is_none());
        assert!(message_arguments(&serde_json::json!({"message": "  "})).is_err());

        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let shared = shared(sender);
        for _ in 0..3 {
            shared.take_token("child-a").expect("three in a burst");
        }
        assert!(shared.take_token("child-a").is_err(), "the fourth waits");
        shared
            .take_token("child-b")
            .expect("each sender has its own bucket");
    }

    /// prime-agent resolves a child's model: the one asked for, else the
    /// configured default, else the parent's; one it cannot use fails the spawn.
    #[test]
    fn q04_spawn_model_resolution_order() {
        struct Known(Option<String>);
        impl ChildModels for Known {
            fn default_model(&self) -> Option<String> {
                self.0.clone()
            }
            fn resolve(&self, reference: &str) -> Result<ChildModel, String> {
                if reference.starts_with("known/") {
                    Ok(ChildModel {
                        provider: Arc::new(harness_providers::MockProvider::scripted(Vec::new())),
                        reference: reference.to_owned(),
                        price: None,
                    })
                } else {
                    Err(format!(
                        "Requested subagent model \"{reference}\" is not available"
                    ))
                }
            }
        }
        let repo = tempfile::tempdir().expect("repo");
        let mut launch = launch_for(
            repo.path(),
            Arc::new(harness_providers::MockProvider::scripted(Vec::new())),
        );
        assert_eq!(
            launch.for_model(None).expect("parent").model.reference,
            "deepseek/fixture-model",
            "no request and no default: the parent's model"
        );
        launch.models = Some(Arc::new(Known(Some("known/default".to_owned()))));
        assert_eq!(
            launch.for_model(None).expect("default").model.reference,
            "known/default"
        );
        assert_eq!(
            launch
                .for_model(Some("known/asked"))
                .expect("asked")
                .model
                .reference,
            "known/asked",
            "the model asked for wins over the default"
        );
        assert_eq!(
            launch
                .for_model(Some("DEEPSEEK/fixture-model"))
                .expect("parent by name")
                .model
                .reference,
            "deepseek/fixture-model"
        );
        let error = launch
            .for_model(Some("nowhere/model"))
            .err()
            .expect("an unknown model fails the spawn");
        assert!(error.contains("not available"), "{error}");
    }

    /// A launch on `provider` in `repo`, in full-auto so a child's reads ask
    /// nobody.
    pub(super) fn launch_for(
        repo: &std::path::Path,
        provider: Arc<dyn harness_providers::ModelProvider>,
    ) -> ChildLaunch {
        ChildLaunch {
            model: ChildModel {
                provider,
                reference: "deepseek/fixture-model".to_owned(),
                price: None,
            },
            models: None,
            runtime_config: harness_runtime::RuntimeConfig::default(),
            workspace_root: repo.to_path_buf(),
            workspace: harness_tools::observe_workspace(ProjectId::generate(), repo)
                .expect("workspace observation"),
            hooks: Vec::new(),
            parent_policy: harness_tools::ToolPolicy::new(1, Vec::new())
                .with_mode(harness_tools::PolicyMode::FullAuto),
            child_limits: harness_tools::TurnLimits::default(),
            web: None,
        }
    }

    #[tokio::test]
    async fn g12_coder_gets_a_real_m8_worktree_for_a_clean_workspace() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let repo = temporary.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo directory");
        clean_repository(&repo);

        let manager = Arc::new(WorkspaceManager::new(temporary.path().join("delegation")));
        let project_id = ProjectId::generate();
        let snapshot = inspect_coder_input(&manager, &repo, &project_id)
            .await
            .expect("clean input admits coder");
        let scheduler = WorkerScheduler::new(
            SchedulerConfig::default(),
            Arc::new(RefusingBackend),
            Some(manager),
        )
        .expect("scheduler with M8 workspace manager");
        let task_id = TaskId::generate();
        let record = scheduler
            .create_worktree(
                &snapshot,
                &task_id,
                &AgentRunId::generate(),
                &[".".to_owned()],
                1,
            )
            .await
            .expect("M8-03 creates an isolated coder worktree");

        assert_eq!(record.task_id, task_id);
        assert_eq!(record.project_id, project_id);
        assert_eq!(record.write_scope, vec![".".to_owned()]);
        assert!(
            std::path::Path::new(&record.path)
                .join("README.md")
                .is_file()
        );
        let branch = Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(&record.path)
            .output()
            .expect("read worktree branch");
        assert!(branch.status.success());
        assert!(
            String::from_utf8_lossy(&branch.stdout)
                .trim()
                .starts_with("harness/p5/")
        );
    }

    #[tokio::test]
    async fn g12_coder_refuses_a_dirty_parent_without_losing_changes() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let repo = temporary.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo directory");
        clean_repository(&repo);
        std::fs::write(repo.join("uncommitted.txt"), "preserve me\n").expect("dirty input");

        let manager = WorkspaceManager::new(temporary.path().join("delegation"));
        let error = inspect_coder_input(&manager, &repo, &ProjectId::generate())
            .await
            .expect_err("dirty workspace is not silently copied or discarded");
        assert_eq!(error.code(), ErrorCode::RoleUnavailable);
        assert!(error.message().contains("untracked file uncommitted.txt"));
        assert_eq!(
            std::fs::read_to_string(repo.join("uncommitted.txt")).expect("user change remains"),
            "preserve me\n"
        );
    }

    /// Reports each brief back as its answer; "slow" briefs take a moment, "fail"
    /// briefs fail.
    struct ScriptedBackend;

    impl harness_orchestrator::WorkerBackend for ScriptedBackend {
        fn dispatch(
            &self,
            request: harness_orchestrator::WorkerRequest,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = harness_orchestrator::WorkerOutcome> + Send + '_>,
        > {
            Box::pin(async move {
                let objective = request.brief.objective.clone();
                if objective.contains("slow") {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                }
                if objective.contains("fail") {
                    return harness_orchestrator::WorkerOutcome::Failed {
                        error: harness_orchestrator::OrchestratorError::new(
                            ErrorCode::ServiceUnavailable,
                            "scripted failure",
                        ),
                    };
                }
                harness_orchestrator::WorkerOutcome::Reported(Box::new(
                    harness_orchestrator::DelegatedResult {
                        schema_version: harness_orchestrator::DELEGATION_CONTRACT_VERSION,
                        result_id: "result".to_owned(),
                        task_id: request.task_id.clone(),
                        worker: harness_orchestrator::WorkerRef {
                            profile_id: harness_types::AgentProfileId::generate(),
                            run_id: request.run_id,
                            role: request.brief.role,
                            generation: request.generation,
                        },
                        outcome: harness_orchestrator::DelegatedOutcome::Completed,
                        summary: format!("answer: {objective}"),
                        artifact_refs: Vec::new(),
                        base_revision: request.brief.base_commit.clone(),
                        result_revision: request.brief.base_commit.clone(),
                        checked_revisions: Vec::new(),
                        check_receipts: Vec::new(),
                        usage: harness_orchestrator::BudgetUsage {
                            model_requests: 1,
                            retries: 0,
                        },
                        detail: serde_json::json!({"steps": 1, "tool_calls": 2}),
                    },
                ))
            })
        }
    }

    /// prime-agent's `rlm.spawn` family over this turn's workers: spawn returns at
    /// admission, collect waits within its timeout, and an outcome one waiter
    /// receives for another child is kept for it rather than lost.
    #[tokio::test]
    #[allow(clippy::too_many_lines, reason = "one scenario, told in order")]
    async fn rlm_children_spawn_collect_list_and_delete() {
        use super::RlmChildren;
        use crate::interactive::repl::HostRequests;
        use serde_json::{Value, json};

        let temporary = tempfile::tempdir().expect("temporary directory");
        let repo = temporary.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo directory");
        clean_repository(&repo);
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let gate = Arc::new(crate::interactive::service::ChannelApprovalGate::new(
            sender.clone(),
            Duration::from_secs(5),
        ));
        let agents = SessionAgents::with_backend(
            SharedStore::new(temporary.path().join("store")),
            sender,
            gate,
            temporary.path().join("delegation"),
            Arc::new(ScriptedBackend),
        );
        let children = RlmChildren {
            agents,
            launch: launch_for(
                &repo,
                Arc::new(harness_providers::MockProvider::scripted(Vec::new())),
            ),
        };
        let ask = |request: Value| {
            let children = &children;
            async move {
                children
                    .handle(&request)
                    .await
                    .expect("a known request type")
            }
        };

        let quick = ask(
            json!({"type": "rlm.run", "prompt": "map the parser", "kwargs": {"name": "quick"}}),
        )
        .await
        .expect("spawn");
        assert_eq!(quick["name"], "quick");
        assert_eq!(quick["model"], "deepseek/fixture-model");
        let slow =
            ask(json!({"type": "rlm.run", "prompt": "slow survey", "kwargs": {"name": "slow"}}))
                .await
                .expect("spawn returns at admission, before the slow child finishes");
        assert!(
            ask(json!({"type": "rlm.run", "prompt": "again", "kwargs": {"name": "quick"}}))
                .await
                .is_err(),
            "names are unique among siblings"
        );
        assert!(
            ask(json!({"type": "rlm.run", "prompt": "x", "kwargs": {"name": "other", "model": "else/model"}}))
                .await
                .expect_err("a model nobody can resolve")
                .contains("not available")
        );

        // Collect the slow child first: the quick child's outcome arrives while
        // waiting and must be kept for it.
        let collected = ask(
            json!({"type": "rlm.collect", "targets": [slow["rlm_child_id"]], "timeout_ms": 10_000}),
        )
        .await
        .expect("collect");
        let result = &collected["results"][0];
        assert_eq!(result["status"], "done", "{collected}");
        assert_eq!(result["settled"], true);
        assert_eq!(result["answer_preview"], "answer: slow survey");
        assert_eq!(result["tool_use_count"], 2);
        let collected = ask(json!({"type": "rlm.collect", "targets": ["quick"], "timeout_ms": 0}))
            .await
            .expect("collect by name");
        assert_eq!(
            collected["results"][0]["answer_preview"],
            "answer: map the parser"
        );

        let listed = ask(json!({"type": "rlm.list_subagents"}))
            .await
            .expect("list");
        let rows = listed["subagents"].as_array().expect("rows");
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter().all(|row| row["status"] == "completed"),
            "{listed}"
        );

        let deleted = ask(json!({"type": "rlm.delete_subagent", "target": "quick"}))
            .await
            .expect("delete");
        assert_eq!(deleted["subagent"]["session_name"], "quick");
        let listed = ask(json!({"type": "rlm.list_subagents"}))
            .await
            .expect("list");
        assert_eq!(listed["subagents"].as_array().expect("rows").len(), 1);

        let failed =
            ask(json!({"type": "rlm.run", "prompt": "fail please", "kwargs": {"name": "broken"}}))
                .await
                .expect("spawn");
        let collected = ask(json!({"type": "rlm.collect", "targets": [failed["rlm_child_id"]], "timeout_ms": 10_000}))
            .await
            .expect("collect");
        assert_eq!(collected["results"][0]["status"], "error");
        assert!(
            collected["results"][0]["error"]
                .as_str()
                .is_some_and(|error| error.contains("scripted failure"))
        );

        assert!(
            ask(json!({"type": "rlm.progress.note", "message": "hi"}))
                .await
                .is_err(),
            "the root sends no progress notes"
        );
        assert!(
            children
                .handle(&json!({"type": "custom.thing"}))
                .await
                .is_none()
        );
    }

    #[test]
    fn g12_delegate_tool_schema_is_bounded_and_only_offers_known_roles() {
        let tools = harness_tools::ExternalTools::new(Arc::new(DelegateCatalog));
        let schemas = tools.schemas();
        let schema = &schemas[0]["function"];
        assert_eq!(schema["name"], "delegate");
        assert_eq!(
            schema["parameters"]["properties"]["brief"]["maxLength"],
            8192
        );
        assert_eq!(
            schema["parameters"]["properties"]["role"]["enum"],
            serde_json::json!(["explorer", "coder"])
        );
    }
}

#[cfg(test)]
mod real_worker_tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use harness_providers::{
        CancellationToken, ModelCapabilities, ModelProvider, ProviderFuture, ProviderRequest,
        ProviderStreamEvent,
    };
    use serde_json::{Value, json};
    use tokio::sync::mpsc::UnboundedReceiver;

    use super::{ChildState, DelegateHost, SessionAgents};
    use crate::interactive::events::SessionEvent;
    use crate::interactive::service::ChannelApprovalGate;
    use crate::interactive::store_lease::SharedStore;

    /// One reply of a scripted model.
    #[derive(Clone)]
    enum Reply {
        Text(&'static str),
        Tool(&'static str, Value),
        /// A stream that fails, as a provider whose body cannot be decoded.
        Broken,
        Delay(u64, Box<Reply>),
    }

    /// Plays its replies in order (then answers "done"), and keeps every request.
    struct Scripted {
        replies: Mutex<VecDeque<Reply>>,
        seen: Mutex<Vec<ProviderRequest>>,
    }

    impl Scripted {
        fn new(replies: Vec<Reply>) -> Arc<Self> {
            Arc::new(Self {
                replies: Mutex::new(replies.into()),
                seen: Mutex::new(Vec::new()),
            })
        }

        fn requests_text(&self) -> String {
            self.seen
                .lock()
                .expect("seen")
                .iter()
                .flat_map(|request| request.messages.iter())
                .map(|message| message.content.clone())
                .collect::<Vec<_>>()
                .join("\n---\n")
        }
    }

    impl ModelProvider for Scripted {
        fn capabilities(&self) -> ModelCapabilities {
            ModelCapabilities::deepseek_fixture()
        }

        fn stream(&self, request: ProviderRequest, _: CancellationToken) -> ProviderFuture {
            let request_id = request.request_id.clone();
            self.seen.lock().expect("seen").push(request);
            let mut reply = self
                .replies
                .lock()
                .expect("replies")
                .pop_front()
                .unwrap_or(Reply::Text("done"));
            Box::pin(async move {
                while let Reply::Delay(ms, next) = reply {
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                    reply = *next;
                }
                let body = match reply {
                    Reply::Text(text) => vec![ProviderStreamEvent::text(text)],
                    Reply::Tool(name, arguments) => vec![ProviderStreamEvent::tool_delta(
                        format!("call-{name}"),
                        name,
                        arguments.to_string(),
                    )],
                    Reply::Broken => {
                        return Err(harness_providers::ProviderError::new(
                            harness_types::ErrorCode::ProviderProtocol,
                            "malformed provider SSE JSON",
                        ));
                    }
                    Reply::Delay(..) => unreachable!("delays were played above"),
                };
                let finish = if body
                    .iter()
                    .any(|event| matches!(event, ProviderStreamEvent::ToolCallDelta { .. }))
                {
                    "tool_calls"
                } else {
                    "stop"
                };
                let mut events = vec![ProviderStreamEvent::Started { request_id }];
                events.extend(body);
                events.push(ProviderStreamEvent::completed(finish));
                Ok(events)
            })
        }
    }

    struct Bench {
        _temporary: tempfile::TempDir,
        repo: std::path::PathBuf,
        agents: Arc<SessionAgents>,
        events: UnboundedReceiver<SessionEvent>,
    }

    fn bench() -> Bench {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let repo = temporary.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo");
        std::fs::write(repo.join("a.txt"), "a\n").expect("file");
        let (sender, events) = tokio::sync::mpsc::unbounded_channel();
        let gate = Arc::new(ChannelApprovalGate::new(
            sender.clone(),
            Duration::from_secs(5),
        ));
        let agents = SessionAgents::new(
            SharedStore::new(temporary.path().join("store")),
            sender,
            gate,
            temporary.path().join("delegation"),
        )
        .expect("session agents");
        Bench {
            _temporary: temporary,
            repo,
            agents,
            events,
        }
    }

    fn host(bench: &Bench, provider: Arc<dyn ModelProvider>) -> DelegateHost {
        DelegateHost::new(
            &bench.agents,
            super::tests::launch_for(&bench.repo, provider),
            CancellationToken::new(),
        )
    }

    async fn spawn(host: &DelegateHost, name: &str, prompt: &str) -> Value {
        host.rlm_requests()
            .handle(&json!({"type": "rlm.run", "prompt": prompt, "kwargs": {"name": name}}))
            .await
            .expect("known request")
            .expect("spawn")
    }

    /// Wait for the next child event (a notice or a message), skipping progress.
    async fn next_child_event(events: &mut UnboundedReceiver<SessionEvent>) -> SessionEvent {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let event @ (SessionEvent::ChildSettled { .. }
                | SessionEvent::AgentMessage { .. }) =
                    events.recv().await.expect("the session is open")
                {
                    return event;
                }
            }
        })
        .await
        .expect("a child event arrives")
    }

    /// No child event arrives within `wait`.
    async fn no_child_event(events: &mut UnboundedReceiver<SessionEvent>, wait: Duration) {
        let seen = tokio::time::timeout(wait, next_child_event(events)).await;
        assert!(seen.is_err(), "unexpected child event: {seen:?}");
    }

    /// The child's state once it settles, read without waiting on it: a result
    /// somebody waits for sends no notice, and these tests watch the notices.
    async fn state_of(agents: &SessionAgents, name: &str) -> ChildState {
        let task_id = agents.shared.find(name).expect("child");
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let state = agents
                .shared
                .update(&task_id, |child| child.state.clone())
                .expect("record");
            if state.settled() {
                return state;
            }
            assert!(std::time::Instant::now() < deadline, "the child settles");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Several children spawned at once from the kernel each run to an answer.
    #[tokio::test]
    async fn children_spawned_together_each_answer() {
        let bench = bench();
        let host = host(&bench, Scripted::new(Vec::new()));
        let children = host.rlm_requests();
        let mut ids = Vec::new();
        for name in ["one", "two", "three"] {
            ids.push(spawn(&host, name, &format!("task {name}")).await["rlm_child_id"].clone());
        }
        let collected = children
            .handle(&json!({"type": "rlm.collect", "targets": ids, "timeout_ms": 20_000}))
            .await
            .expect("known")
            .expect("collect");
        for result in collected["results"].as_array().expect("results") {
            assert_eq!(result["status"], "done", "{collected}");
        }
    }

    /// Q01: the turn that spawned a child ends, and the child keeps working.
    #[tokio::test]
    async fn q01_children_outlive_the_turn_that_spawned_them() {
        let mut bench = bench();
        let provider = Scripted::new(vec![Reply::Delay(
            800,
            Box::new(Reply::Text("late answer")),
        )]);
        let host = host(&bench, provider);
        spawn(&host, "late", "take your time").await;
        drop(host);
        let summary = bench.agents.summary().join("\n");
        assert!(summary.contains("late · explorer"), "{summary}");
        assert!(
            summary.contains("running"),
            "still running after its turn: {summary}"
        );
        match state_of(&bench.agents, "late").await {
            ChildState::Done { answer, .. } => assert_eq!(answer, "late answer"),
            other => panic!("the child finished its work: {other:?}"),
        }
        // Q02: nobody waited for it, so its parent hears about it.
        match next_child_event(&mut bench.events).await {
            SessionEvent::ChildSettled { name, notice } => {
                assert_eq!(name, "late");
                assert_eq!(
                    notice,
                    "[child-exited: no-reply child:late]\n\nLast assistant text: late answer"
                );
            }
            other => panic!("expected a notice: {other:?}"),
        }
    }

    /// Q02: a child that breaks tells its parent it failed, and why.
    #[tokio::test]
    async fn q02_a_failed_child_sends_the_failure_notice() {
        let mut bench = bench();
        // Broken on every attempt: the runtime retries a failed stream.
        let host = host(&bench, Scripted::new(vec![Reply::Broken; 6]));
        spawn(&host, "broken", "read a.txt").await;
        match next_child_event(&mut bench.events).await {
            SessionEvent::ChildSettled { notice, .. } => {
                assert!(
                    notice.starts_with("[child-failed child:broken]\n\nprovider_protocol: "),
                    "{notice}"
                );
                assert_eq!(
                    notice.matches("provider_protocol:").count(),
                    1,
                    "the code once: {notice}"
                );
            }
            other => panic!("expected a notice: {other:?}"),
        }
    }

    /// Q02: `delegate` with `wait: false` returns before the child is done, and the
    /// result arrives as a notice; with the default wait there is no notice.
    #[tokio::test]
    async fn q02_delegate_wait_false_returns_before_the_child_finishes() {
        let mut bench = bench();
        let provider = Scripted::new(vec![
            Reply::Delay(1_000, Box::new(Reply::Text("background answer"))),
            Reply::Text("waited answer"),
        ]);
        let host = host(&bench, provider);
        let tool = host.delegate_tool();
        let started = std::time::Instant::now();
        let payload = tool
            .run(&json!({"role": "explorer", "brief": "look around", "wait": false}))
            .await
            .expect("delegate");
        assert!(
            started.elapsed() < Duration::from_millis(700),
            "it did not wait"
        );
        assert_eq!(payload["status"], "started", "{payload}");
        match next_child_event(&mut bench.events).await {
            SessionEvent::ChildSettled { notice, .. } => {
                assert!(
                    notice.ends_with("Last assistant text: background answer"),
                    "{notice}"
                );
            }
            other => panic!("expected a notice: {other:?}"),
        }

        let payload = tool
            .run(&json!({"role": "explorer", "brief": "look again"}))
            .await
            .expect("delegate");
        assert_eq!(payload["status"], "completed", "{payload}");
        assert_eq!(payload["text"], "waited answer");
        no_child_event(&mut bench.events, Duration::from_millis(500)).await;
    }

    /// `/agents stop` stops a child and says so; `/new` stops them all quietly.
    #[tokio::test]
    async fn q01_stop_reports_cancelled_and_reset_is_quiet() {
        let mut bench = bench();
        let slow = || Reply::Delay(5_000, Box::new(Reply::Text("never")));
        let host = host(&bench, Scripted::new(vec![slow(), slow()]));
        spawn(&host, "slow", "wait").await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            bench
                .agents
                .stop("slow", "Stopped by the user with /agents stop")
                .expect("stop"),
            1
        );
        match next_child_event(&mut bench.events).await {
            SessionEvent::ChildSettled { notice, .. } => assert_eq!(
                notice,
                "[child-exited: cancelled child:slow]\n\nStopped by the user with /agents stop"
            ),
            other => panic!("expected a notice: {other:?}"),
        }
        assert!(bench.agents.stop("nobody", "x").is_err());

        spawn(&host, "other", "wait").await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        bench.agents.reset();
        no_child_event(&mut bench.events, Duration::from_millis(800)).await;
        assert!(
            bench.agents.summary()[0].starts_with("no delegated workers"),
            "{:?}",
            bench.agents.summary()
        );
    }

    /// Q03: a child tells its parent what it found; a child that replied gets no
    /// no-reply notice.
    #[tokio::test]
    async fn q03_a_child_message_reaches_the_parent() {
        let mut bench = bench();
        let provider = Scripted::new(vec![
            Reply::Tool("agent_message", json!({"message": "found it in a.txt"})),
            Reply::Text("done"),
        ]);
        let host = host(&bench, provider);
        spawn(&host, "scout", "find it").await;
        match next_child_event(&mut bench.events).await {
            SessionEvent::AgentMessage { text } => {
                assert_eq!(
                    text,
                    "[agent-message from child:scout]\n\nfound it in a.txt"
                );
            }
            other => panic!("expected the child's message: {other:?}"),
        }
        assert!(matches!(
            state_of(&bench.agents, "scout").await,
            ChildState::Done { .. }
        ));
        no_child_event(&mut bench.events, Duration::from_millis(500)).await;
    }

    /// Q03: the parent's message reaches a running child as written.
    #[tokio::test]
    async fn q03_a_parent_message_reaches_the_running_child() {
        let bench = bench();
        let provider = Scripted::new(vec![
            Reply::Delay(1_200, Box::new(Reply::Tool("list_files", json!({})))),
            Reply::Text("ok"),
        ]);
        let host = host(&bench, Arc::clone(&provider) as Arc<dyn ModelProvider>);
        spawn(&host, "worker", "list the files").await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let receipts = host
            .rlm_requests()
            .handle(&json!({
                "type": "agent_message.send",
                "message": "also check b.txt",
                "receiver_role": "child",
                "receiver_name": "worker",
            }))
            .await
            .expect("known")
            .expect("send");
        let status = receipts["receipts"][0]["deliveryStatus"].clone();
        assert!(status == "delivered" || status == "queued", "{receipts}");
        assert!(matches!(
            state_of(&bench.agents, "worker").await,
            ChildState::Done { .. }
        ));
        let seen = provider.requests_text();
        assert!(
            seen.contains("[agent-message from parent:root]\n\nalso check b.txt"),
            "{seen}"
        );
        assert!(
            !seen.contains("[steering correction from the user]\n[agent-message"),
            "an agent message is not the user's steering: {seen}"
        );
    }

    /// Q03: progress notes are throttled to one each ten seconds and read in the
    /// child's row, never sent to the parent.
    #[tokio::test]
    async fn q03_progress_notes_are_throttled_and_kept_five() {
        let mut bench = bench();
        let provider = Scripted::new(vec![
            Reply::Tool("progress_note", json!({"message": "reading a.txt"})),
            Reply::Tool("progress_note", json!({"message": "again too soon"})),
            Reply::Text("done"),
        ]);
        let host = host(&bench, provider);
        spawn(&host, "noter", "take notes").await;
        assert!(matches!(
            state_of(&bench.agents, "noter").await,
            ChildState::Done { .. }
        ));
        let task_id = bench.agents.shared.find("noter").expect("child");
        let notes = bench
            .agents
            .shared
            .update(&task_id, |child| {
                child.notes.iter().cloned().collect::<Vec<_>>()
            })
            .expect("record");
        assert_eq!(
            notes,
            vec!["reading a.txt".to_owned()],
            "the second was too soon"
        );
        let listed = host
            .rlm_requests()
            .handle(&json!({"type": "rlm.list_subagents"}))
            .await
            .expect("known")
            .expect("list");
        assert_eq!(listed["subagents"][0]["progress_note"], "reading a.txt");
        match next_child_event(&mut bench.events).await {
            SessionEvent::ChildSettled { notice, .. } => {
                assert!(!notice.contains("reading a.txt"), "notes are not messages");
            }
            other => panic!("no message is sent for a note: {other:?}"),
        }
    }
}
